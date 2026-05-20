//! Sample-group structures (`sbgp` / `sgpd`).
//!
//! ISO/IEC 14496-12 §8.9 defines two linked tables that partition a
//! track's samples into named groups:
//!
//! * `sbgp` — Sample-to-Group Box (§8.9.2). Compact run-length list
//!   of `(sample_count, group_description_index)` pairs that assigns
//!   every sample in the track (or track fragment) to one entry in
//!   the matching `sgpd`.
//! * `sgpd` — Sample-Group-Description Box (§8.9.3). Per-group
//!   payload — e.g. roll-distance, num-leading-samples — keyed by
//!   the same FourCC `grouping_type` as the `sbgp`.
//!
//! This crate decodes the structural envelope plus the three
//! well-known typed entries defined in §10.1 / §10.4 that callers
//! need for spec-correct random-access:
//!
//! * `'roll'` — VisualRollRecoveryEntry / AudioRollRecoveryEntry
//!   (§10.1.1.2). `signed int(16) roll_distance` — number of
//!   samples to decode for gradual-decoding-refresh entry-points and
//!   for audio streams where every sample is independently decodable
//!   but the decoder output is only assured after pre-rolling.
//! * `'prol'` — AudioPreRollEntry (§10.1.1.2). `signed int(16)
//!   roll_distance` — pre-roll distance for audio streams in which
//!   not every sample is a sync sample (the AAC / Opus codec-priming
//!   convention).
//! * `'rap '` — VisualRandomAccessEntry (§10.4.2). `1 bit
//!   num_leading_samples_known | 7 bit num_leading_samples`. Open
//!   random-access points where samples following in decode order
//!   but preceding in presentation order may be undecodable.
//!
//! Per spec note in §8.9.3.2, version-0 `sgpd` entries are deprecated
//! because their size is implicit; this crate parses them by relying
//! on the known fixed sizes of the typed entries above (2 bytes for
//! `'roll'` / `'prol'`, 1 byte for `'rap '`). Version-1 entries
//! either have a global `default_length` or a per-entry
//! `description_length` prefix, both of which we honour. Version-2
//! additionally carries `default_sample_description_index`.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// One row of a `sbgp` table — a run of `sample_count` consecutive
/// samples all sharing `group_description_index`. An index of `0`
/// means "not a member of any group of this type" (§8.9.2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleToGroupEntry {
    pub sample_count: u32,
    pub group_description_index: u32,
}

/// Parsed `sbgp` (Sample-to-Group Box).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleToGroup {
    pub grouping_type: [u8; 4],
    /// `grouping_type_parameter` from §8.9.2.3 — only present on
    /// version-1 boxes. Zero on version-0 boxes (the spec doesn't
    /// define it there).
    pub grouping_type_parameter: u32,
    pub entries: Vec<SampleToGroupEntry>,
}

impl SampleToGroup {
    /// Look up the `group_description_index` for a 0-based sample
    /// index. Returns `0` when the sample is not covered by the table
    /// (the spec's "no explicit group" case in §8.9.2.3).
    pub fn group_index_for_sample(&self, sample_zero_based: u32) -> u32 {
        let mut cursor: u64 = 0;
        let want = sample_zero_based as u64;
        for e in &self.entries {
            let next = cursor + e.sample_count as u64;
            if want < next {
                return e.group_description_index;
            }
            cursor = next;
        }
        0
    }

    /// Total number of samples covered by the table. The spec
    /// (§8.9.2.3) makes it an error for the sum to exceed the actual
    /// sample_count; this returns the sum as-is and leaves the
    /// "greater than" sanity check to the caller.
    pub fn covered_samples(&self) -> u64 {
        self.entries.iter().map(|e| e.sample_count as u64).sum()
    }
}

/// One entry inside `sgpd`. The payload bytes are kept verbatim so
/// callers can decode the codec-specific layout (§10 onwards) on
/// demand without this module having to enumerate every known
/// `grouping_type`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleGroupDescriptionEntry {
    pub payload: Vec<u8>,
}

/// Parsed `sgpd` (Sample-Group-Description Box).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleGroupDescription {
    pub grouping_type: [u8; 4],
    /// Version-1 fixed entry length, or `0` when entries carry their
    /// own `description_length` prefix (§8.9.3.3).
    pub default_length: u32,
    /// Version-2 default sample-description index (§8.9.3.3). Zero
    /// on earlier versions.
    pub default_sample_description_index: u32,
    pub entries: Vec<SampleGroupDescriptionEntry>,
    /// Version of the parsed `sgpd` box (0, 1, or 2). Exposed so
    /// callers can decide whether to trust `default_length` /
    /// `default_sample_description_index`.
    pub version: u8,
}

impl SampleGroupDescription {
    /// Borrow the entry at `group_description_index` (1-based per
    /// §8.9.2.3). Returns `None` for the spec's reserved
    /// "not-a-member" index 0 or any out-of-range index.
    pub fn entry(&self, group_description_index: u32) -> Option<&SampleGroupDescriptionEntry> {
        if group_description_index == 0 {
            return None;
        }
        self.entries.get((group_description_index - 1) as usize)
    }
}

/// Typed `'roll'` entry — §10.1.1.2 VisualRollRecoveryEntry /
/// AudioRollRecoveryEntry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RollRecovery {
    pub roll_distance: i16,
}

/// Typed `'prol'` entry — §10.1.1.2 AudioPreRollEntry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioPreRoll {
    pub roll_distance: i16,
}

/// Typed `'rap '` entry — §10.4.2 VisualRandomAccessEntry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualRandomAccess {
    pub num_leading_samples_known: bool,
    pub num_leading_samples: u8,
}

/// Decode a 2-byte `'roll'` payload into a [`RollRecovery`].
pub fn decode_roll(payload: &[u8]) -> Result<RollRecovery> {
    if payload.len() < 2 {
        return Err(Error::invalid("MOV: sgpd 'roll' entry < 2 bytes"));
    }
    Ok(RollRecovery {
        roll_distance: i16::from_be_bytes([payload[0], payload[1]]),
    })
}

/// Decode a 2-byte `'prol'` payload into an [`AudioPreRoll`].
pub fn decode_prol(payload: &[u8]) -> Result<AudioPreRoll> {
    if payload.len() < 2 {
        return Err(Error::invalid("MOV: sgpd 'prol' entry < 2 bytes"));
    }
    Ok(AudioPreRoll {
        roll_distance: i16::from_be_bytes([payload[0], payload[1]]),
    })
}

/// Decode a 1-byte `'rap '` payload into a [`VisualRandomAccess`].
/// Bit layout (§10.4.2): top bit = `num_leading_samples_known`,
/// remaining 7 bits = `num_leading_samples`.
pub fn decode_rap(payload: &[u8]) -> Result<VisualRandomAccess> {
    if payload.is_empty() {
        return Err(Error::invalid("MOV: sgpd 'rap ' entry < 1 byte"));
    }
    let b = payload[0];
    Ok(VisualRandomAccess {
        num_leading_samples_known: (b & 0x80) != 0,
        num_leading_samples: b & 0x7f,
    })
}

/// Parse a `sbgp` payload (FullBox `sbgp`, §8.9.2.2).
///
/// Layout:
/// ```text
/// version : u8                      (0 or 1)
/// flags   : u24                     (reserved)
/// grouping_type : [u8; 4]
/// if version == 1 { grouping_type_parameter : u32 }
/// entry_count : u32
/// entry_count × { sample_count : u32, group_description_index : u32 }
/// ```
pub fn parse_sbgp(payload: &[u8]) -> Result<SampleToGroup> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: sbgp header < 8 bytes"));
    }
    let version = payload[0];
    let mut cursor = 4usize;
    let grouping_type = [
        payload[cursor],
        payload[cursor + 1],
        payload[cursor + 2],
        payload[cursor + 3],
    ];
    cursor += 4;
    let grouping_type_parameter = if version == 1 {
        if payload.len() < cursor + 4 {
            return Err(Error::invalid(
                "MOV: sbgp v1 missing grouping_type_parameter",
            ));
        }
        let v = read_u32(&payload[cursor..]);
        cursor += 4;
        v
    } else {
        0
    };
    if payload.len() < cursor + 4 {
        return Err(Error::invalid("MOV: sbgp missing entry_count"));
    }
    let n = read_u32(&payload[cursor..]) as usize;
    cursor += 4;
    if payload.len() < cursor + n * 8 {
        return Err(Error::invalid("MOV: sbgp truncated entries"));
    }
    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        entries.push(SampleToGroupEntry {
            sample_count: read_u32(&payload[cursor..]),
            group_description_index: read_u32(&payload[cursor + 4..]),
        });
        cursor += 8;
    }
    Ok(SampleToGroup {
        grouping_type,
        grouping_type_parameter,
        entries,
    })
}

/// Parse a `sgpd` payload (FullBox `sgpd`, §8.9.3.2).
///
/// Versions handled:
/// * v0 — entries have an implicit size; we infer per-entry size from
///   the known typed-entry catalogue (`roll`/`prol` → 2 B,
///   `rap ` → 1 B). Unknown grouping_types fall back to splitting the
///   remainder evenly by `entry_count` so the caller still gets the
///   raw bytes back. Per spec note in §8.9.3.2 this version is
///   deprecated; we keep it for compatibility with older
///   ffmpeg-authored files.
/// * v1 — `default_length` ≠ 0 means every entry is exactly that many
///   bytes; `default_length == 0` means each entry begins with a
///   `description_length:u32` prefix.
/// * v2 — adds `default_sample_description_index` ahead of
///   `entry_count`.
pub fn parse_sgpd(payload: &[u8]) -> Result<SampleGroupDescription> {
    if payload.len() < 12 {
        return Err(Error::invalid("MOV: sgpd header < 12 bytes"));
    }
    let version = payload[0];
    let mut cursor = 4usize;
    let grouping_type = [
        payload[cursor],
        payload[cursor + 1],
        payload[cursor + 2],
        payload[cursor + 3],
    ];
    cursor += 4;
    let default_length = if version >= 1 {
        if payload.len() < cursor + 4 {
            return Err(Error::invalid("MOV: sgpd v1+ missing default_length"));
        }
        let v = read_u32(&payload[cursor..]);
        cursor += 4;
        v
    } else {
        0
    };
    let default_sample_description_index = if version >= 2 {
        if payload.len() < cursor + 4 {
            return Err(Error::invalid(
                "MOV: sgpd v2 missing default_sample_description_index",
            ));
        }
        let v = read_u32(&payload[cursor..]);
        cursor += 4;
        v
    } else {
        0
    };
    if payload.len() < cursor + 4 {
        return Err(Error::invalid("MOV: sgpd missing entry_count"));
    }
    let n = read_u32(&payload[cursor..]) as usize;
    cursor += 4;

    let mut entries = Vec::with_capacity(n);
    // Decode the per-entry size: v1 uses `default_length` (or per-row
    // `description_length`); v0 uses the typed catalogue.
    let v0_implicit_size = implicit_v0_size(&grouping_type);
    for _ in 0..n {
        let entry_len = if version >= 1 {
            if default_length == 0 {
                if payload.len() < cursor + 4 {
                    return Err(Error::invalid("MOV: sgpd missing description_length"));
                }
                let v = read_u32(&payload[cursor..]) as usize;
                cursor += 4;
                v
            } else {
                default_length as usize
            }
        } else {
            match v0_implicit_size {
                Some(sz) => sz,
                None => {
                    // Spec deprecated v0 with no implicit size; fall
                    // back to "remainder / entry_count" so we don't
                    // hard-fail on unknown grouping_types.
                    let remaining = payload.len() - cursor;
                    remaining.checked_div(n).unwrap_or(0)
                }
            }
        };
        if payload.len() < cursor + entry_len {
            return Err(Error::invalid("MOV: sgpd truncated entry payload"));
        }
        entries.push(SampleGroupDescriptionEntry {
            payload: payload[cursor..cursor + entry_len].to_vec(),
        });
        cursor += entry_len;
    }

    Ok(SampleGroupDescription {
        grouping_type,
        default_length,
        default_sample_description_index,
        entries,
        version,
    })
}

/// Fixed payload size of the typed entries spelled out in §10.1.1.2
/// and §10.4.2. Used only for the deprecated v0 path where the size
/// isn't on disk.
fn implicit_v0_size(grouping_type: &[u8; 4]) -> Option<usize> {
    match grouping_type {
        b"roll" => Some(2),
        b"prol" => Some(2),
        b"rap " => Some(1),
        _ => None,
    }
}

#[inline]
fn read_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_sbgp_v0(grouping_type: &[u8; 4], entries: &[(u32, u32)]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
        p.extend_from_slice(grouping_type);
        p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (count, idx) in entries {
            p.extend_from_slice(&count.to_be_bytes());
            p.extend_from_slice(&idx.to_be_bytes());
        }
        p
    }

    #[test]
    fn sbgp_v0_round_trip() {
        let p = build_sbgp_v0(b"roll", &[(3, 1), (2, 2), (5, 0)]);
        let g = parse_sbgp(&p).unwrap();
        assert_eq!(g.grouping_type, *b"roll");
        assert_eq!(g.grouping_type_parameter, 0);
        assert_eq!(g.entries.len(), 3);
        assert_eq!(g.entries[1].sample_count, 2);
        assert_eq!(g.entries[1].group_description_index, 2);
        assert_eq!(g.covered_samples(), 10);
        // Sample 0..3 → group 1; 3..5 → group 2; 5..10 → 0.
        assert_eq!(g.group_index_for_sample(0), 1);
        assert_eq!(g.group_index_for_sample(2), 1);
        assert_eq!(g.group_index_for_sample(3), 2);
        assert_eq!(g.group_index_for_sample(4), 2);
        assert_eq!(g.group_index_for_sample(5), 0);
        // Past the table → 0 (no explicit group).
        assert_eq!(g.group_index_for_sample(100), 0);
    }

    #[test]
    fn sbgp_v1_round_trip() {
        let mut p = Vec::new();
        p.push(1u8); // version
        p.extend_from_slice(&[0, 0, 0]); // flags
        p.extend_from_slice(b"prol");
        p.extend_from_slice(&0xdeadbeefu32.to_be_bytes()); // grouping_type_parameter
        p.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        p.extend_from_slice(&4u32.to_be_bytes()); // sample_count
        p.extend_from_slice(&1u32.to_be_bytes()); // group_description_index
        let g = parse_sbgp(&p).unwrap();
        assert_eq!(g.grouping_type, *b"prol");
        assert_eq!(g.grouping_type_parameter, 0xdeadbeef);
        assert_eq!(g.entries.len(), 1);
        assert_eq!(g.group_index_for_sample(3), 1);
    }

    #[test]
    fn sbgp_truncated_header_errors() {
        assert!(parse_sbgp(&[0u8; 7]).is_err());
    }

    #[test]
    fn sgpd_v1_default_length_roll() {
        // version=1, grouping_type='roll', default_length=2,
        // entry_count=2, entries: i16 +1, i16 -3.
        let mut p = Vec::new();
        p.push(1u8);
        p.extend_from_slice(&[0, 0, 0]);
        p.extend_from_slice(b"roll");
        p.extend_from_slice(&2u32.to_be_bytes()); // default_length
        p.extend_from_slice(&2u32.to_be_bytes()); // entry_count
        p.extend_from_slice(&1i16.to_be_bytes());
        p.extend_from_slice(&(-3i16).to_be_bytes());
        let d = parse_sgpd(&p).unwrap();
        assert_eq!(d.version, 1);
        assert_eq!(d.grouping_type, *b"roll");
        assert_eq!(d.default_length, 2);
        assert_eq!(d.entries.len(), 2);
        // Index 0 is reserved (§8.9.2.3) — caller passes 1-based.
        assert!(d.entry(0).is_none());
        let r0 = decode_roll(&d.entry(1).unwrap().payload).unwrap();
        assert_eq!(r0.roll_distance, 1);
        let r1 = decode_roll(&d.entry(2).unwrap().payload).unwrap();
        assert_eq!(r1.roll_distance, -3);
    }

    #[test]
    fn sgpd_v0_roll_implicit_size() {
        // v0 omits default_length; 'roll' has implicit size = 2.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver=0
        p.extend_from_slice(b"roll");
        p.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        p.extend_from_slice(&7i16.to_be_bytes());
        let d = parse_sgpd(&p).unwrap();
        assert_eq!(d.version, 0);
        assert_eq!(d.entries.len(), 1);
        let r = decode_roll(&d.entry(1).unwrap().payload).unwrap();
        assert_eq!(r.roll_distance, 7);
    }

    #[test]
    fn sgpd_v1_variable_length_with_description_length_prefix() {
        // v1, default_length=0 → each entry begins with u32 length.
        let mut p = Vec::new();
        p.push(1u8);
        p.extend_from_slice(&[0, 0, 0]);
        p.extend_from_slice(b"abcd");
        p.extend_from_slice(&0u32.to_be_bytes()); // default_length = 0
        p.extend_from_slice(&2u32.to_be_bytes()); // entry_count
        p.extend_from_slice(&3u32.to_be_bytes()); // description_length
        p.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        p.extend_from_slice(&1u32.to_be_bytes()); // description_length
        p.extend_from_slice(&[0xDD]);
        let d = parse_sgpd(&p).unwrap();
        assert_eq!(d.entries[0].payload, vec![0xAA, 0xBB, 0xCC]);
        assert_eq!(d.entries[1].payload, vec![0xDD]);
    }

    #[test]
    fn sgpd_v2_carries_default_sample_description_index() {
        // v2, grouping_type='prol', default_length=2,
        // default_sample_description_index=7, entry_count=1.
        let mut p = Vec::new();
        p.push(2u8);
        p.extend_from_slice(&[0, 0, 0]);
        p.extend_from_slice(b"prol");
        p.extend_from_slice(&2u32.to_be_bytes()); // default_length
        p.extend_from_slice(&7u32.to_be_bytes()); // default_sd_index
        p.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        p.extend_from_slice(&(-2880i16).to_be_bytes());
        let d = parse_sgpd(&p).unwrap();
        assert_eq!(d.version, 2);
        assert_eq!(d.default_sample_description_index, 7);
        let entry = decode_prol(&d.entry(1).unwrap().payload).unwrap();
        assert_eq!(entry.roll_distance, -2880);
    }

    #[test]
    fn decode_rap_split_bits() {
        // Top bit set + 5 leading samples → 0x85.
        let r = decode_rap(&[0x85]).unwrap();
        assert!(r.num_leading_samples_known);
        assert_eq!(r.num_leading_samples, 5);
        // Top bit clear, value ignored per §10.4.3 — but we still
        // decode it for round-tripping.
        let r2 = decode_rap(&[0x03]).unwrap();
        assert!(!r2.num_leading_samples_known);
        assert_eq!(r2.num_leading_samples, 3);
    }

    #[test]
    fn decode_roll_negative_distance() {
        // Spec §10.1.1.2 — negative roll_distance means decode N
        // samples before the marked sample.
        let r = decode_roll(&(-2i16).to_be_bytes()).unwrap();
        assert_eq!(r.roll_distance, -2);
    }

    #[test]
    fn truncated_typed_entries_error() {
        assert!(decode_roll(&[0]).is_err());
        assert!(decode_prol(&[]).is_err());
        assert!(decode_rap(&[]).is_err());
    }
}
