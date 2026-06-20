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
//! This crate decodes the structural envelope plus the well-known
//! typed entries defined in §10.1 .. §10.6 that callers need for
//! spec-correct random-access and adaptive streaming:
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
//! * `'tele'` — TemporalLevelEntry (§10.5.2). `1 bit
//!   level_independently_decodable | 7 bit reserved`. Codec-
//!   independent temporal-level grouping: the temporal level equals
//!   the group-description index, enabling extraction of temporal
//!   subsequences.
//! * `'sap '` — SAPEntry (§10.6.2). `1 bit dependent_flag | 3 bit
//!   reserved | 4 bit SAP_type`. Tags samples as Stream Access
//!   Points (Annex I) of the indicated SAP type.
//! * `'rash'` — RateShareEntry (§10.2.2.2). Variable-length rate-
//!   share record (per-operation-point target shares + min / max
//!   bitrate + discard priority) used by servers / players when
//!   allocating bandwidth across simultaneously-served tracks.
//! * `'alst'` — AlternativeStartupEntry (§10.3.2). Variable-length
//!   record describing an alternative startup sequence (a subset of
//!   samples that lets rendering begin earlier than full decode).
//!
//! Per spec note in §8.9.3.2, version-0 `sgpd` entries are deprecated
//! because their size is implicit; this crate parses them by relying
//! on the known fixed sizes of the fixed-width typed entries above
//! (2 bytes for `'roll'` / `'prol'`, 1 byte for `'rap '` / `'tele'`
//! / `'sap '`). The variable-length `'rash'` / `'alst'` entries only
//! have a defined on-disk size in version-1+ boxes (`default_length`
//! or a per-row `description_length`). Version-1 entries
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

/// Typed `'tele'` entry — §10.5.2 TemporalLevelEntry.
///
/// The temporal level of every sample in the group equals its
/// `sgpd` group-description index (1, 2, 3, …); samples of one
/// temporal level have no coding dependencies on samples of higher
/// levels, so a reader can extract a temporal subsequence by keeping
/// only the levels up to a chosen index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemporalLevel {
    /// §10.5.3 — `1` means all samples of this level have no coding
    /// dependencies on samples of other levels; `0` means no
    /// information is provided.
    pub level_independently_decodable: bool,
}

/// Typed `'sap '` entry — §10.6.2 SAPEntry (Stream Access Point).
///
/// Identifies samples (the first byte of which is the ISAU position
/// for a SAP, Annex I) as being of the indicated SAP type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamAccessPoint {
    /// §10.6.3 — `false` for non-layered media. `true` specifies that
    /// the reference layers, if any, for predicting the target layers
    /// may have to be decoded for accessing a sample of this group.
    pub dependent: bool,
    /// §10.6.3 — SAP type (Annex I). Values `0` and `7` are reserved;
    /// `1`..=`6` specify the SAP type of the associated samples.
    pub sap_type: u8,
}

/// Decode a 1-byte `'tele'` payload into a [`TemporalLevel`].
/// Bit layout (§10.5.2): top bit = `level_independently_decodable`,
/// remaining 7 bits reserved (= 0; parsers ignore the value).
pub fn decode_tele(payload: &[u8]) -> Result<TemporalLevel> {
    if payload.is_empty() {
        return Err(Error::invalid("MOV: sgpd 'tele' entry < 1 byte"));
    }
    Ok(TemporalLevel {
        level_independently_decodable: (payload[0] & 0x80) != 0,
    })
}

/// Decode a 1-byte `'sap '` payload into a [`StreamAccessPoint`].
/// Bit layout (§10.6.2): bit 0 = `dependent_flag`, bits 1..=3
/// reserved (= 0), bits 4..=7 = `SAP_type`.
pub fn decode_sap(payload: &[u8]) -> Result<StreamAccessPoint> {
    if payload.is_empty() {
        return Err(Error::invalid("MOV: sgpd 'sap ' entry < 1 byte"));
    }
    let b = payload[0];
    Ok(StreamAccessPoint {
        dependent: (b & 0x80) != 0,
        sap_type: b & 0x0f,
    })
}

/// One operation point of a `'rash'` RateShareEntry (§10.2.2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateShareOperationPoint {
    /// §10.2.2.3 — total available bitrate (kbit/s) that defines this
    /// operation point. Only present when `operation_point_count > 1`;
    /// for the single-point form this field is `0` (the share applies
    /// to all available bitrates).
    pub available_bitrate: u32,
    /// §10.2.2.3 — percentage of available bandwidth to allocate to
    /// the media at this operation point (`0` = no preference given).
    pub target_rate_share: u16,
}

/// Typed `'rash'` entry — §10.2.2.2 RateShareEntry.
///
/// Rate-share information lets a server / player extracting data from
/// a track in combination with other tracks (§10.2.3) allocate the
/// total available bitrate across simultaneously-served tracks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateShare {
    /// §10.2.2.3 — the operation points. When the on-disk
    /// `operation_point_count` is 1, this holds a single point with
    /// `available_bitrate == 0` (the spec's "applies to all bitrates"
    /// single-share form).
    pub operation_points: Vec<RateShareOperationPoint>,
    /// §10.2.2.3 — upper bitrate threshold (kbit/s); `0` = no info.
    pub maximum_bitrate: u32,
    /// §10.2.2.3 — lower bitrate threshold (kbit/s); `0` = no info.
    pub minimum_bitrate: u32,
    /// §10.2.2.3 — discard priority; the track with the highest value
    /// is discarded first when meeting rate-share constraints.
    pub discard_priority: u8,
}

/// One sample-output-rate piece of an `'alst'` AlternativeStartupEntry
/// (§10.3.2). The alternative startup sequence is divided into
/// consecutive pieces, each with a constant sample output rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlternativeStartupPiece {
    /// §10.3.3 — number of output samples in this piece.
    pub num_output_samples: u16,
    /// §10.3.3 — number of total samples in this piece.
    pub num_total_samples: u16,
}

/// Typed `'alst'` entry — §10.3.2 AlternativeStartupEntry.
///
/// Describes an alternative startup sequence: a subset of samples
/// (starting from a sync / `'rap '` sample) that lets rendering begin
/// earlier than when all samples are decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlternativeStartup {
    /// §10.3.3 — number of samples in the alternative startup sequence
    /// (`0` means the associated sample belongs to no such sequence).
    pub roll_count: u16,
    /// §10.3.3 — 1-based index (in decoding order) of the first sample
    /// intended for output among the sequence's samples.
    pub first_output_sample: u16,
    /// §10.3.3 — per-sample decoding-time deltas relative to the
    /// regular decoding time; `roll_count` entries.
    pub sample_offsets: Vec<u32>,
    /// §10.3.3 — optional sample-output-rate pieces filling the rest
    /// of the structure (may be empty).
    pub pieces: Vec<AlternativeStartupPiece>,
}

/// Decode a `'rash'` RateShareEntry payload (§10.2.2.2).
///
/// Variable-length; only meaningful for `sgpd` version-1+ boxes where
/// the on-disk entry size is known (`default_length` or a per-row
/// `description_length`). The deprecated v0 size-implicit path cannot
/// carry `'rash'` because its width depends on `operation_point_count`.
pub fn decode_rash(payload: &[u8]) -> Result<RateShare> {
    if payload.len() < 2 {
        return Err(Error::invalid("MOV: sgpd 'rash' entry < 2 bytes"));
    }
    let op_count = u16::from_be_bytes([payload[0], payload[1]]);
    if op_count == 0 {
        return Err(Error::invalid(
            "MOV: sgpd 'rash' operation_point_count must be non-zero",
        ));
    }
    let mut cursor = 2usize;
    let mut operation_points = Vec::with_capacity(op_count as usize);
    if op_count == 1 {
        // Single-point form: just one target_rate_share, no bitrate.
        if payload.len() < cursor + 2 {
            return Err(Error::invalid("MOV: sgpd 'rash' truncated single share"));
        }
        operation_points.push(RateShareOperationPoint {
            available_bitrate: 0,
            target_rate_share: u16::from_be_bytes([payload[cursor], payload[cursor + 1]]),
        });
        cursor += 2;
    } else {
        for _ in 0..op_count {
            if payload.len() < cursor + 6 {
                return Err(Error::invalid("MOV: sgpd 'rash' truncated operation point"));
            }
            let available_bitrate = u32::from_be_bytes([
                payload[cursor],
                payload[cursor + 1],
                payload[cursor + 2],
                payload[cursor + 3],
            ]);
            let target_rate_share = u16::from_be_bytes([payload[cursor + 4], payload[cursor + 5]]);
            operation_points.push(RateShareOperationPoint {
                available_bitrate,
                target_rate_share,
            });
            cursor += 6;
        }
    }
    if payload.len() < cursor + 9 {
        return Err(Error::invalid(
            "MOV: sgpd 'rash' truncated bitrate / discard trailer",
        ));
    }
    let maximum_bitrate = u32::from_be_bytes([
        payload[cursor],
        payload[cursor + 1],
        payload[cursor + 2],
        payload[cursor + 3],
    ]);
    let minimum_bitrate = u32::from_be_bytes([
        payload[cursor + 4],
        payload[cursor + 5],
        payload[cursor + 6],
        payload[cursor + 7],
    ]);
    let discard_priority = payload[cursor + 8];
    Ok(RateShare {
        operation_points,
        maximum_bitrate,
        minimum_bitrate,
        discard_priority,
    })
}

/// Decode an `'alst'` AlternativeStartupEntry payload (§10.3.2).
///
/// Variable-length; meaningful for `sgpd` version-1+ boxes where the
/// on-disk entry size delimits the trailing optional pieces. The
/// `do { } while (until end of structure)` loop in the spec is
/// realised by consuming `(num_output_samples, num_total_samples)`
/// pairs until the payload is exhausted.
pub fn decode_alst(payload: &[u8]) -> Result<AlternativeStartup> {
    if payload.len() < 4 {
        return Err(Error::invalid("MOV: sgpd 'alst' entry < 4 bytes"));
    }
    let roll_count = u16::from_be_bytes([payload[0], payload[1]]);
    let first_output_sample = u16::from_be_bytes([payload[2], payload[3]]);
    let mut cursor = 4usize;
    // `sample_offset[i]` for i in 1..=roll_count (4 bytes each).
    let need = 4usize
        .checked_mul(roll_count as usize)
        .ok_or_else(|| Error::invalid("MOV: sgpd 'alst' roll_count overflow"))?;
    if payload.len() < cursor + need {
        return Err(Error::invalid(
            "MOV: sgpd 'alst' truncated sample_offset table",
        ));
    }
    let mut sample_offsets = Vec::with_capacity(roll_count as usize);
    for _ in 0..roll_count {
        sample_offsets.push(u32::from_be_bytes([
            payload[cursor],
            payload[cursor + 1],
            payload[cursor + 2],
            payload[cursor + 3],
        ]));
        cursor += 4;
    }
    // Optional output-rate pieces fill the remaining 4-byte-aligned
    // tail. A trailing fragment < 4 bytes is malformed.
    let tail = payload.len() - cursor;
    if tail % 4 != 0 {
        return Err(Error::invalid(
            "MOV: sgpd 'alst' trailing piece table not a multiple of 4 bytes",
        ));
    }
    let mut pieces = Vec::with_capacity(tail / 4);
    while cursor + 4 <= payload.len() {
        pieces.push(AlternativeStartupPiece {
            num_output_samples: u16::from_be_bytes([payload[cursor], payload[cursor + 1]]),
            num_total_samples: u16::from_be_bytes([payload[cursor + 2], payload[cursor + 3]]),
        });
        cursor += 4;
    }
    Ok(AlternativeStartup {
        roll_count,
        first_output_sample,
        sample_offsets,
        pieces,
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

/// The `0x8000_0000` bit of a `csgp`
/// `sample_group_description_index` (after expansion the index is
/// stored in [`SampleToGroupEntry::group_description_index`]). When
/// set, the index refers to a **fragment-local** `sgpd` (defined in
/// the same `traf`); when clear, a **global** one (defined in the
/// `moov`-level `stbl`). See
/// `docs/container/isobmff/post-2015-additions.md` ("Fragment-local
/// vs global indices").
pub const CSGP_FRAGMENT_LOCAL_BIT: u32 = 0x8000_0000;

/// Result of expanding one `csgp` `sample_group_description_index`.
/// `index` is the value with the fragment-local bit *masked off* (so
/// it lines up with the 1-based indexing used by
/// [`SampleGroupDescription::entry`]); `fragment_local` records
/// whether the msb was set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CsgpIndex {
    pub index: u32,
    pub fragment_local: bool,
}

/// Split a raw `csgp` description index into the fragment-local flag
/// plus the masked 1-based index. Only meaningful when the `csgp`
/// lived inside a `traf`; for a `stbl`-scope `csgp` the msb is part
/// of the index value, so callers that know the box came from `stbl`
/// should not apply this split.
pub fn split_csgp_index(raw: u32) -> CsgpIndex {
    CsgpIndex {
        index: raw & !CSGP_FRAGMENT_LOCAL_BIT,
        fragment_local: (raw & CSGP_FRAGMENT_LOCAL_BIT) != 0,
    }
}

/// Map a `csgp` 2-bit size code to its on-wire field width in bits.
/// Per `docs/container/isobmff/post-2015-additions.md`: `width = 4 <<
/// code` → {0→4, 1→8, 2→16, 3→32}.
#[inline]
fn csgp_field_width(code: u32) -> u32 {
    4 << code
}

/// MSB-first bit reader over a `csgp` body's variable-width fields.
struct BitReader<'a> {
    bytes: &'a [u8],
    /// Absolute bit position from the start of `bytes`.
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8], byte_offset: usize) -> Self {
        BitReader {
            bytes,
            bit_pos: byte_offset * 8,
        }
    }

    /// Read `width` bits (`width` ≤ 32) MSB-first as an unsigned
    /// value. Returns `None` if the body is exhausted.
    fn read(&mut self, width: u32) -> Option<u32> {
        if width == 0 {
            return Some(0);
        }
        if self.bit_pos + width as usize > self.bytes.len() * 8 {
            return None;
        }
        let mut value: u32 = 0;
        for _ in 0..width {
            let byte = self.bytes[self.bit_pos >> 3];
            let bit = (byte >> (7 - (self.bit_pos & 7))) & 1;
            value = (value << 1) | bit as u32;
            self.bit_pos += 1;
        }
        Some(value)
    }

    /// Number of bytes consumed so far, rounded up — used only for
    /// diagnostics / asserts in tests.
    #[cfg(test)]
    fn byte_cursor_ceil(&self) -> usize {
        self.bit_pos.div_ceil(8)
    }
}

/// Parse a `csgp` payload (Compact Sample to Group Box) and expand it
/// into a plain [`SampleToGroup`] so the rest of the crate can treat
/// it identically to a v0/v1 `sbgp`.
///
/// Layout (`docs/container/isobmff/post-2015-additions.md`,
/// "`csgp` — Compact Sample to Group Box"):
///
/// ```text
/// version : u8                       (0)
/// flags   : u24                      (carries the four sub-fields below)
///   index_size_code   = flags[0..1]   width selector for description indices
///   count_size_code   = flags[2..3]   width selector for sample_count
///   pattern_size_code = flags[4..5]   width selector for pattern_length
///   grouping_type_parameter_present = flags[6]
/// grouping_type : [u8; 4]
/// if grouping_type_parameter_present { grouping_type_parameter : u32 }
/// pattern_count : u32
/// pattern_count × { pattern_length[i] : f(pattern_size_code)
///                   sample_count[i]   : f(count_size_code)   }
/// for each pattern j, pattern_length[j] × {
///     sample_group_description_index[j][k] : f(index_size_code)
/// }
/// ```
///
/// Expansion semantics: pattern `j` defines `pattern_length[j]`
/// per-sample description indices; that pattern is replayed across
/// `sample_count[j]` samples (the pattern repeats, sample-by-sample,
/// wrapping when `sample_count[j] > pattern_length[j]`). The result
/// is a flat run-length list identical in meaning to `sbgp`.
///
/// The fragment-local msb (see [`split_csgp_index`]) is **preserved**
/// verbatim in `group_description_index`; callers that need to
/// resolve a fragment-local vs global `sgpd` apply
/// [`split_csgp_index`] themselves. For a `stbl`-scope `csgp` the bit
/// is simply part of the index and round-trips unchanged.
pub fn parse_csgp(payload: &[u8]) -> Result<SampleToGroup> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: csgp header < 8 bytes"));
    }
    // version byte is payload[0] (must be 0); the 24-bit flags carry
    // the sub-fields.
    let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
    let index_size_code = flags & 0b11;
    let count_size_code = (flags >> 2) & 0b11;
    let pattern_size_code = (flags >> 4) & 0b11;
    let grouping_type_parameter_present = (flags >> 6) & 1 == 1;

    let index_width = csgp_field_width(index_size_code);
    let count_width = csgp_field_width(count_size_code);
    let pattern_width = csgp_field_width(pattern_size_code);

    let mut cursor = 4usize;
    let grouping_type = [
        payload[cursor],
        payload[cursor + 1],
        payload[cursor + 2],
        payload[cursor + 3],
    ];
    cursor += 4;

    let grouping_type_parameter = if grouping_type_parameter_present {
        if payload.len() < cursor + 4 {
            return Err(Error::invalid("MOV: csgp missing grouping_type_parameter"));
        }
        let v = read_u32(&payload[cursor..]);
        cursor += 4;
        v
    } else {
        0
    };

    if payload.len() < cursor + 4 {
        return Err(Error::invalid("MOV: csgp missing pattern_count"));
    }
    let pattern_count = read_u32(&payload[cursor..]) as usize;
    cursor += 4;

    // The pattern table and the index table are both bit-packed with
    // no byte alignment between them — a single MSB-first stream that
    // starts right after `pattern_count`.
    let mut bits = BitReader::new(payload, cursor);

    let mut pattern_lengths = Vec::with_capacity(pattern_count.min(payload.len()));
    let mut sample_counts = Vec::with_capacity(pattern_count.min(payload.len()));
    for _ in 0..pattern_count {
        let plen = bits
            .read(pattern_width)
            .ok_or_else(|| Error::invalid("MOV: csgp truncated pattern_length"))?;
        let scount = bits
            .read(count_width)
            .ok_or_else(|| Error::invalid("MOV: csgp truncated sample_count"))?;
        pattern_lengths.push(plen);
        sample_counts.push(scount);
    }

    // Expand each pattern into run-length `sbgp` rows. Reading the
    // per-pattern indices then replaying them sample-by-sample, RLE-
    // coalescing consecutive equal indices keeps the entry vector
    // bounded by the on-disk index count rather than the (potentially
    // huge) expanded sample total.
    let mut entries: Vec<SampleToGroupEntry> = Vec::new();
    for j in 0..pattern_count {
        let plen = pattern_lengths[j];
        // A zero-length pattern would make the replay modulo undefined;
        // the spec implies pattern_length ≥ 1, so reject 0 explicitly
        // rather than divide by zero below.
        if plen == 0 {
            return Err(Error::invalid("MOV: csgp pattern_length is zero"));
        }
        let mut pattern_indices = Vec::with_capacity(plen as usize);
        for _ in 0..plen {
            let idx = bits
                .read(index_width)
                .ok_or_else(|| Error::invalid("MOV: csgp truncated description index"))?;
            pattern_indices.push(idx);
        }
        let scount = sample_counts[j];
        for k in 0..scount {
            let idx = pattern_indices[(k % plen) as usize];
            match entries.last_mut() {
                Some(last) if last.group_description_index == idx => {
                    last.sample_count += 1;
                }
                _ => entries.push(SampleToGroupEntry {
                    sample_count: 1,
                    group_description_index: idx,
                }),
            }
        }
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

    // Allocate for the byte-backed entry count, not the declared one —
    // a forged `entry_count` must not drive `Vec::with_capacity` (or,
    // via the v0 zero-implicit-size fallback below, `Vec::push`)
    // beyond what the (64 MiB-capped) atom body can actually hold.
    let mut entries = Vec::with_capacity(n.min(payload.len() - cursor));
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
                    // hard-fail on unknown grouping_types. A division
                    // that rounds to zero means the declared count
                    // exceeds the body's bytes — reject rather than
                    // push `entry_count` zero-length entries (an
                    // attacker-controlled unbounded `Vec` growth).
                    let remaining = payload.len() - cursor;
                    let sz = remaining.checked_div(n).unwrap_or(0);
                    if sz == 0 {
                        return Err(Error::invalid(format!(
                            "MOV: sgpd v0 entry_count {n} exceeds {remaining} body bytes \
                             (zero-size implicit entries)"
                        )));
                    }
                    sz
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
        b"tele" => Some(1),
        b"sap " => Some(1),
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
    fn decode_tele_independently_decodable_bit() {
        // §10.5.2 — top bit = level_independently_decodable, low 7
        // reserved (=0; parsers ignore).
        let t = decode_tele(&[0x80]).unwrap();
        assert!(t.level_independently_decodable);
        // Reserved low bits must not affect the decode.
        let t2 = decode_tele(&[0x7f]).unwrap();
        assert!(!t2.level_independently_decodable);
    }

    #[test]
    fn decode_sap_dependent_and_type() {
        // §10.6.2 — bit0 dependent_flag, bits1..3 reserved, bits4..7
        // SAP_type. 0x83 → dependent=1, reserved=0, SAP_type=3.
        let s = decode_sap(&[0x83]).unwrap();
        assert!(s.dependent);
        assert_eq!(s.sap_type, 3);
        // Non-dependent SAP type 1.
        let s2 = decode_sap(&[0x01]).unwrap();
        assert!(!s2.dependent);
        assert_eq!(s2.sap_type, 1);
    }

    #[test]
    fn sgpd_v0_tele_sap_implicit_size() {
        // v0 sgpd with 'tele' entries must split into 1-byte rows via
        // the implicit-size catalogue (§8.9.3.2 deprecated path).
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // version 0 + flags
        body.extend_from_slice(b"tele");
        body.extend_from_slice(&2u32.to_be_bytes()); // entry_count
        body.push(0x80); // level 1: independently decodable
        body.push(0x00); // level 2: no info
        let parsed = parse_sgpd(&body).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert!(
            decode_tele(&parsed.entries[0].payload)
                .unwrap()
                .level_independently_decodable
        );
        assert!(
            !decode_tele(&parsed.entries[1].payload)
                .unwrap()
                .level_independently_decodable
        );
    }

    #[test]
    fn truncated_tele_sap_error() {
        assert!(decode_tele(&[]).is_err());
        assert!(decode_sap(&[]).is_err());
    }

    #[test]
    fn decode_rash_single_operation_point() {
        // op_count=1 → one 16-bit target_rate_share (no bitrate field),
        // then max/min bitrate (u32) + discard_priority (u8).
        let mut p = Vec::new();
        p.extend_from_slice(&1u16.to_be_bytes()); // operation_point_count
        p.extend_from_slice(&7500u16.to_be_bytes()); // target_rate_share
        p.extend_from_slice(&2000u32.to_be_bytes()); // maximum_bitrate
        p.extend_from_slice(&500u32.to_be_bytes()); // minimum_bitrate
        p.push(64); // discard_priority
        let r = decode_rash(&p).unwrap();
        assert_eq!(r.operation_points.len(), 1);
        assert_eq!(r.operation_points[0].available_bitrate, 0);
        assert_eq!(r.operation_points[0].target_rate_share, 7500);
        assert_eq!(r.maximum_bitrate, 2000);
        assert_eq!(r.minimum_bitrate, 500);
        assert_eq!(r.discard_priority, 64);
    }

    #[test]
    fn decode_rash_multi_operation_point() {
        // op_count=2 → each point is (available_bitrate:u32,
        // target_rate_share:u16); then the u32/u32/u8 trailer.
        let mut p = Vec::new();
        p.extend_from_slice(&2u16.to_be_bytes());
        p.extend_from_slice(&1000u32.to_be_bytes()); // pt0 available_bitrate
        p.extend_from_slice(&3000u16.to_be_bytes()); // pt0 target_rate_share
        p.extend_from_slice(&4000u32.to_be_bytes()); // pt1 available_bitrate
        p.extend_from_slice(&6000u16.to_be_bytes()); // pt1 target_rate_share
        p.extend_from_slice(&5000u32.to_be_bytes()); // maximum_bitrate
        p.extend_from_slice(&800u32.to_be_bytes()); // minimum_bitrate
        p.push(200); // discard_priority
        let r = decode_rash(&p).unwrap();
        assert_eq!(r.operation_points.len(), 2);
        assert_eq!(r.operation_points[0].available_bitrate, 1000);
        assert_eq!(r.operation_points[0].target_rate_share, 3000);
        assert_eq!(r.operation_points[1].available_bitrate, 4000);
        assert_eq!(r.operation_points[1].target_rate_share, 6000);
        assert_eq!(r.discard_priority, 200);
    }

    #[test]
    fn decode_rash_rejects_zero_op_count_and_truncation() {
        let mut zero = Vec::new();
        zero.extend_from_slice(&0u16.to_be_bytes());
        zero.extend_from_slice(&[0u8; 9]);
        assert!(decode_rash(&zero).is_err());
        // op_count=1 but missing the 9-byte trailer.
        let mut trunc = Vec::new();
        trunc.extend_from_slice(&1u16.to_be_bytes());
        trunc.extend_from_slice(&7500u16.to_be_bytes());
        assert!(decode_rash(&trunc).is_err());
    }

    #[test]
    fn decode_alst_offsets_and_pieces() {
        // roll_count=2, first_output_sample=1, two 32-bit offsets,
        // then two optional (num_output, num_total) pieces.
        let mut p = Vec::new();
        p.extend_from_slice(&2u16.to_be_bytes()); // roll_count
        p.extend_from_slice(&1u16.to_be_bytes()); // first_output_sample
        p.extend_from_slice(&10u32.to_be_bytes()); // sample_offset[1]
        p.extend_from_slice(&20u32.to_be_bytes()); // sample_offset[2]
        p.extend_from_slice(&3u16.to_be_bytes()); // piece0 num_output
        p.extend_from_slice(&5u16.to_be_bytes()); // piece0 num_total
        p.extend_from_slice(&7u16.to_be_bytes()); // piece1 num_output
        p.extend_from_slice(&9u16.to_be_bytes()); // piece1 num_total
        let a = decode_alst(&p).unwrap();
        assert_eq!(a.roll_count, 2);
        assert_eq!(a.first_output_sample, 1);
        assert_eq!(a.sample_offsets, vec![10, 20]);
        assert_eq!(a.pieces.len(), 2);
        assert_eq!(a.pieces[0].num_output_samples, 3);
        assert_eq!(a.pieces[0].num_total_samples, 5);
        assert_eq!(a.pieces[1].num_total_samples, 9);
    }

    #[test]
    fn decode_alst_no_optional_pieces() {
        // roll_count=0 is the spec "not part of any sequence" case;
        // header alone with no offsets and no pieces is valid.
        let mut p = Vec::new();
        p.extend_from_slice(&0u16.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes());
        let a = decode_alst(&p).unwrap();
        assert_eq!(a.roll_count, 0);
        assert!(a.sample_offsets.is_empty());
        assert!(a.pieces.is_empty());
    }

    #[test]
    fn decode_alst_rejects_misaligned_tail_and_short_offsets() {
        // roll_count claims 1 offset but only 2 trailing bytes.
        let mut short = Vec::new();
        short.extend_from_slice(&1u16.to_be_bytes());
        short.extend_from_slice(&0u16.to_be_bytes());
        short.extend_from_slice(&[0u8, 0]); // < 4 bytes for the offset
        assert!(decode_alst(&short).is_err());
        // Valid offsets, then a 2-byte (misaligned) trailing fragment.
        let mut misaligned = Vec::new();
        misaligned.extend_from_slice(&0u16.to_be_bytes());
        misaligned.extend_from_slice(&0u16.to_be_bytes());
        misaligned.extend_from_slice(&[0u8, 0]);
        assert!(decode_alst(&misaligned).is_err());
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

    /// Build a `csgp` body. `*_code` are the 2-bit width selectors;
    /// patterns is a list of `(sample_count, &[indices])`.
    fn build_csgp(
        grouping_type: &[u8; 4],
        grouping_type_parameter: Option<u32>,
        index_code: u32,
        count_code: u32,
        pattern_code: u32,
        patterns: &[(u32, &[u32])],
    ) -> Vec<u8> {
        let mut flags =
            (index_code & 0b11) | ((count_code & 0b11) << 2) | ((pattern_code & 0b11) << 4);
        if grouping_type_parameter.is_some() {
            flags |= 1 << 6;
        }
        let mut p = Vec::new();
        p.push(0u8); // version
        let fb = flags.to_be_bytes();
        p.extend_from_slice(&fb[1..4]); // 24-bit flags
        p.extend_from_slice(grouping_type);
        if let Some(gtp) = grouping_type_parameter {
            p.extend_from_slice(&gtp.to_be_bytes());
        }
        p.extend_from_slice(&(patterns.len() as u32).to_be_bytes());

        // Bit-pack the pattern table then the index table, MSB-first,
        // mirroring the reader.
        let pw = 4u32 << pattern_code;
        let cw = 4u32 << count_code;
        let iw = 4u32 << index_code;
        let mut acc: u64 = 0;
        let mut nbits: u32 = 0;
        let mut out: Vec<u8> = Vec::new();
        let push = |val: u32, width: u32, acc: &mut u64, nbits: &mut u32, out: &mut Vec<u8>| {
            *acc = (*acc << width) | (val as u64 & ((1u64 << width) - 1));
            *nbits += width;
            while *nbits >= 8 {
                *nbits -= 8;
                out.push(((*acc >> *nbits) & 0xff) as u8);
            }
        };
        for (sc, idxs) in patterns {
            push(idxs.len() as u32, pw, &mut acc, &mut nbits, &mut out);
            push(*sc, cw, &mut acc, &mut nbits, &mut out);
        }
        for (_, idxs) in patterns {
            for &i in *idxs {
                push(i, iw, &mut acc, &mut nbits, &mut out);
            }
        }
        if nbits > 0 {
            out.push(((acc << (8 - nbits)) & 0xff) as u8);
        }
        p.extend_from_slice(&out);
        p
    }

    #[test]
    fn csgp_field_width_codes() {
        assert_eq!(csgp_field_width(0), 4);
        assert_eq!(csgp_field_width(1), 8);
        assert_eq!(csgp_field_width(2), 16);
        assert_eq!(csgp_field_width(3), 32);
    }

    #[test]
    fn csgp_single_pattern_expands_like_sbgp() {
        // One pattern of length 2 (indices 1,2) replayed across 5
        // samples → 1,2,1,2,1 → RLE rows of count 1 each.
        // index_code=0 (4-bit), count_code=1 (8-bit), pattern_code=0.
        let body = build_csgp(b"roll", None, 0, 1, 0, &[(5, &[1, 2])]);
        let g = parse_csgp(&body).unwrap();
        assert_eq!(g.grouping_type, *b"roll");
        assert_eq!(g.grouping_type_parameter, 0);
        assert_eq!(g.covered_samples(), 5);
        assert_eq!(g.group_index_for_sample(0), 1);
        assert_eq!(g.group_index_for_sample(1), 2);
        assert_eq!(g.group_index_for_sample(2), 1);
        assert_eq!(g.group_index_for_sample(3), 2);
        assert_eq!(g.group_index_for_sample(4), 1);
    }

    #[test]
    fn csgp_coalesces_consecutive_equal_indices() {
        // Pattern length 1 (index 3) across 4 samples → all 3 → one
        // RLE row of sample_count 4.
        let body = build_csgp(b"abcd", None, 1, 1, 1, &[(4, &[3])]);
        let g = parse_csgp(&body).unwrap();
        assert_eq!(g.entries.len(), 1);
        assert_eq!(g.entries[0].sample_count, 4);
        assert_eq!(g.entries[0].group_description_index, 3);
        assert_eq!(g.covered_samples(), 4);
    }

    #[test]
    fn csgp_multiple_patterns_concatenate() {
        // Pattern 0: (3 samples, [1]) → 1,1,1.
        // Pattern 1: (2 samples, [2,0]) → 2,0.
        // Flat: 1,1,1,2,0 → RLE [(3,1),(1,2),(1,0)].
        let body = build_csgp(b"sgrp", None, 0, 0, 0, &[(3, &[1]), (2, &[2, 0])]);
        let g = parse_csgp(&body).unwrap();
        assert_eq!(g.entries.len(), 3);
        assert_eq!(
            g.entries[0],
            SampleToGroupEntry {
                sample_count: 3,
                group_description_index: 1
            }
        );
        assert_eq!(g.entries[1].group_description_index, 2);
        assert_eq!(g.entries[2].group_description_index, 0);
        assert_eq!(g.covered_samples(), 5);
    }

    #[test]
    fn csgp_grouping_type_parameter_present() {
        let body = build_csgp(b"roll", Some(0xcafe_f00d), 1, 1, 1, &[(2, &[1, 1])]);
        let g = parse_csgp(&body).unwrap();
        assert_eq!(g.grouping_type_parameter, 0xcafe_f00d);
        assert_eq!(g.covered_samples(), 2);
    }

    #[test]
    fn csgp_wide_index_width_32bit() {
        // index_code=3 → 32-bit indices, large value round-trips.
        let body = build_csgp(b"big ", None, 3, 1, 1, &[(1, &[0x1234_5678])]);
        let g = parse_csgp(&body).unwrap();
        assert_eq!(g.entries[0].group_description_index, 0x1234_5678);
    }

    #[test]
    fn csgp_fragment_local_bit_split() {
        let raw = CSGP_FRAGMENT_LOCAL_BIT | 5;
        let s = split_csgp_index(raw);
        assert!(s.fragment_local);
        assert_eq!(s.index, 5);
        let g = split_csgp_index(7);
        assert!(!g.fragment_local);
        assert_eq!(g.index, 7);
    }

    #[test]
    fn csgp_fragment_local_msb_preserved_in_expansion() {
        // A fragment-local index (msb set) is kept verbatim in the
        // expanded SampleToGroup; callers split it on demand.
        let local = CSGP_FRAGMENT_LOCAL_BIT | 2;
        let body = build_csgp(b"trfm", None, 3, 1, 1, &[(1, &[local])]);
        let g = parse_csgp(&body).unwrap();
        assert_eq!(g.entries[0].group_description_index, local);
        let split = split_csgp_index(g.entries[0].group_description_index);
        assert!(split.fragment_local);
        assert_eq!(split.index, 2);
    }

    #[test]
    fn csgp_zero_pattern_length_rejected() {
        let body = build_csgp(b"roll", None, 1, 1, 1, &[(3, &[])]);
        assert!(parse_csgp(&body).is_err());
    }

    #[test]
    fn csgp_truncated_header_errors() {
        assert!(parse_csgp(&[0u8; 7]).is_err());
    }

    #[test]
    fn csgp_truncated_index_table_errors() {
        // Declare 1 pattern, length 4, but stop the body short of all
        // four 16-bit indices.
        let mut p = Vec::new();
        p.push(0u8);
        p.extend_from_slice(&[0, 0, (2u32 << 4) as u8]); // pattern_code=2 (16-bit), others 0
        p.extend_from_slice(b"roll");
        p.extend_from_slice(&1u32.to_be_bytes()); // pattern_count
                                                  // pattern_length=4 (16-bit), sample_count=4 (4-bit)
        p.extend_from_slice(&4u16.to_be_bytes());
        p.push(0x40); // 4 in top nibble (4-bit count), low nibble starts indices
                      // Truncate here — no room for four 4-bit indices.
        assert!(parse_csgp(&p).is_err());
    }

    #[test]
    fn csgp_bitreader_byte_cursor_advances() {
        // Sanity-check the BitReader cursor accounting.
        let bytes = [0b1010_0000u8, 0u8];
        let mut br = BitReader::new(&bytes, 0);
        assert_eq!(br.read(4), Some(0b1010));
        assert_eq!(br.byte_cursor_ceil(), 1);
    }
}
