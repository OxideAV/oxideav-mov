//! Sample Auxiliary Information Sizes / Offsets Boxes (`saiz` /
//! `saio`).
//!
//! ISO/IEC 14496-12:2015 §8.7.8 / §8.7.9. The pair carries per-sample
//! auxiliary information *outside* the sample data itself. The format
//! and meaning of that auxiliary information is owned by a separate
//! specification (e.g. ISO/IEC 23001-7 Common Encryption sample-aux
//! data); this crate decodes only the structural envelope — sizes
//! and chunk offsets — and surfaces the bytes verbatim for the
//! caller to interpret.
//!
//! `saiz` (Sample Auxiliary Information Sizes Box) describes how many
//! bytes of auxiliary information each sample carries. Per §8.7.8.2:
//!
//! ```text
//! aligned(8) class SampleAuxiliaryInformationSizesBox
//! extends FullBox('saiz', version = 0, flags) {
//!     if (flags & 1) {
//!         unsigned int(32) aux_info_type;
//!         unsigned int(32) aux_info_type_parameter;
//!     }
//!     unsigned int(8) default_sample_info_size;
//!     unsigned int(32) sample_count;
//!     if (default_sample_info_size == 0) {
//!         unsigned int(8) sample_info_size[ sample_count ];
//!     }
//! }
//! ```
//!
//! `saio` (Sample Auxiliary Information Offsets Box) carries the file
//! positions of those bytes. Per §8.7.9.2:
//!
//! ```text
//! aligned(8) class SampleAuxiliaryInformationOffsetsBox
//! extends FullBox('saio', version, flags) {
//!     if (flags & 1) {
//!         unsigned int(32) aux_info_type;
//!         unsigned int(32) aux_info_type_parameter;
//!     }
//!     unsigned int(32) entry_count;
//!     if (version == 0) {
//!         unsigned int(32) offset[ entry_count ];
//!     } else {
//!         unsigned int(64) offset[ entry_count ];
//!     }
//! }
//! ```
//!
//! Both boxes carry an *optional* `(aux_info_type,
//! aux_info_type_parameter)` discriminator pair gated by `flags & 1`.
//! When absent, §8.7.8.1 / §8.7.9.1 fall back to (a) the
//! `scheme_type` of the track's Protection Scheme Information box if
//! the content is transformed (e.g. CENC) or (b) the sample-entry
//! type otherwise; `aux_info_type_parameter` defaults to 0 in either
//! case. Both fallback rules are caller-side concerns — this module
//! preserves the on-disk discriminator (or zero when absent) and
//! lets the caller decide.
//!
//! `saiz` lives in either the Sample Table Box (`stbl`,
//! §8.7.8.1) — for non-fragmented files — or the Track Fragment Box
//! (`traf`, §8.8.x) — for fragmented files. This module handles the
//! envelope identically in both cases; the demuxer wires the
//! `stbl`-scope form into [`crate::sample_table::SampleTable`].
//! `saio` shares the same containers.
//!
//! `saiz` `version` is fixed at 0 by the spec; we reject any other
//! value. `saio` permits `version` 0 (32-bit offsets) and 1 (64-bit
//! offsets); we reject anything else. Per §8.7.8.3 / §8.7.9.3, a
//! single `(aux_info_type, aux_info_type_parameter)` may appear at
//! most once per containing box; the demuxer enforces this when it
//! merges the parsed boxes into the per-track sample table.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Discriminator pair (`aux_info_type`, `aux_info_type_parameter`)
/// shared by [`Saiz`] and [`Saio`] (§8.7.8.3 / §8.7.9.3). `None`
/// means the discriminator was not present on disk (`flags & 1 ==
/// 0`); the §8.7.8.1 implicit-fallback rules apply (scheme type for
/// transformed content, sample-entry type otherwise; the
/// caller-supplied default for `aux_info_type_parameter` is 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuxInfoType {
    /// 4-byte identifier of the auxiliary information format.
    pub aux_info_type: [u8; 4],
    /// 32-bit "stream" sub-discriminator. Defaults to 0 when omitted
    /// from disk; otherwise carries the writer's value verbatim.
    pub aux_info_type_parameter: u32,
}

impl AuxInfoType {
    /// Convenience: pair-equality across the two fields.
    pub fn matches(&self, aux_info_type: &[u8; 4], aux_info_type_parameter: u32) -> bool {
        &self.aux_info_type == aux_info_type
            && self.aux_info_type_parameter == aux_info_type_parameter
    }
}

/// Parsed Sample Auxiliary Information Sizes Box (`saiz`, §8.7.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saiz {
    /// `flags` field from the FullBox header. The low bit (`flags &
    /// 1`) gates the presence of the [`AuxInfoType`] discriminator;
    /// the upper bits are reserved by the spec and carried verbatim
    /// so vendor / future extensions survive a round trip.
    pub flags: u32,
    /// `(aux_info_type, aux_info_type_parameter)` pair when the box's
    /// `flags & 1` bit was set; `None` otherwise. §8.7.8.1's
    /// implicit-fallback rules govern the `None` case.
    pub aux_info_type: Option<AuxInfoType>,
    /// `default_sample_info_size` from §8.7.8.2. When non-zero, every
    /// sample's auxiliary information is exactly this many bytes.
    /// When zero, the per-sample table in [`Self::sample_info_sizes`]
    /// is authoritative.
    pub default_sample_info_size: u8,
    /// Number of samples for which a size is defined (§8.7.8.3).
    /// "If this is less than the number of samples [in the stbl /
    /// traf], then auxiliary information is supplied for the initial
    /// samples, and the remaining samples have no associated
    /// auxiliary information" — i.e. the table is a *prefix* of the
    /// track's sample list.
    pub sample_count: u32,
    /// Per-sample sizes (in bytes). Empty when
    /// `default_sample_info_size != 0` (every entry is the default).
    /// Length equals `sample_count` when populated.
    pub sample_info_sizes: Vec<u8>,
}

impl Saiz {
    /// Size in bytes of the auxiliary information for **0-based**
    /// `sample_idx`. Returns `None` when the index is past the box's
    /// `sample_count` (§8.7.8.3 — the box is a prefix; trailing
    /// samples carry no auxiliary information).
    pub fn size_for(&self, sample_idx: u32) -> Option<u32> {
        if sample_idx >= self.sample_count {
            return None;
        }
        if self.default_sample_info_size != 0 {
            return Some(self.default_sample_info_size as u32);
        }
        self.sample_info_sizes
            .get(sample_idx as usize)
            .map(|&s| s as u32)
    }

    /// Total auxiliary-information bytes referenced by this box —
    /// the sum of every sample's size. Useful for chunk-scope
    /// integrity checks against `saio`'s offset chain. Saturating on
    /// overflow so a malformed sum doesn't panic; the caller can
    /// detect by comparing to `u64::MAX`.
    pub fn total_size(&self) -> u64 {
        if self.default_sample_info_size != 0 {
            return (self.default_sample_info_size as u64).saturating_mul(self.sample_count as u64);
        }
        self.sample_info_sizes
            .iter()
            .map(|&s| s as u64)
            .fold(0u64, u64::saturating_add)
    }
}

/// Parsed Sample Auxiliary Information Offsets Box (`saio`, §8.7.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saio {
    /// `version` from the FullBox header — 0 selects 32-bit offsets,
    /// 1 selects 64-bit offsets (§8.7.9.2). Both are widened to
    /// `u64` in [`Self::offsets`] so callers don't branch.
    pub version: u8,
    /// `flags` field from the FullBox header. The low bit (`flags &
    /// 1`) gates the presence of the [`AuxInfoType`] discriminator;
    /// upper bits are reserved by the spec but carried verbatim.
    pub flags: u32,
    /// `(aux_info_type, aux_info_type_parameter)` pair when the box's
    /// `flags & 1` bit was set; `None` otherwise. §8.7.9.3 ties the
    /// semantics to the matching `saiz` box, which carries the same
    /// pair under the same gating.
    pub aux_info_type: Option<AuxInfoType>,
    /// Per-chunk (or per-track-fragment-run) offsets, both widths
    /// widened to `u64`. §8.7.9.3 — `entry_count` is either 1 (one
    /// offset for the whole `stbl` / `traf`, with all auxiliary
    /// information contiguous from that point) or equal to the
    /// chunk count / run count for the container. The interpretation
    /// of *absolute* (stbl-scope) vs *relative-to-tfhd-base-offset*
    /// (traf-scope) is a caller-side concern; this module surfaces
    /// the on-disk number verbatim.
    pub offsets: Vec<u64>,
}

impl Saio {
    /// `true` when the box carries a single offset covering every
    /// chunk / run in the container. §8.7.9.3: "If entry_count is
    /// one, then the Sample Auxiliary Information for all Chunks or
    /// Runs is contiguous in the file in chunk or run order."
    pub fn is_single_chunk(&self) -> bool {
        self.offsets.len() == 1
    }

    /// Convenience: `offsets[index]` with bounds check.
    pub fn offset_for(&self, index: usize) -> Option<u64> {
        self.offsets.get(index).copied()
    }
}

/// Parse a `saiz` (Sample Auxiliary Information Sizes Box) payload —
/// ISO/IEC 14496-12 §8.7.8.2.
///
/// Rejects:
/// * payload shorter than the 4-byte FullBox header;
/// * `version != 0` (the spec defines only v0);
/// * `flags & 1` set but the body too short for the 8-byte
///   `(aux_info_type, aux_info_type_parameter)` pair;
/// * body too short for the mandatory `default_sample_info_size:1 +
///   sample_count:4`;
/// * `default_sample_info_size == 0` but the body too short for the
///   `sample_count`-long size table.
pub fn parse_saiz(payload: &[u8]) -> Result<Saiz> {
    need(payload, 0, 4, "saiz FullBox header")?;
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "MOV: saiz unknown version {version} (spec fixes at 0)"
        )));
    }
    let flags = read_u24(&payload[1..]);

    let mut pos = 4usize;
    let aux_info_type = if (flags & 1) != 0 {
        need(payload, pos, 8, "saiz aux_info_type pair")?;
        let mut t = [0u8; 4];
        t.copy_from_slice(&payload[pos..pos + 4]);
        let p = read_u32(&payload[pos + 4..]);
        pos += 8;
        Some(AuxInfoType {
            aux_info_type: t,
            aux_info_type_parameter: p,
        })
    } else {
        None
    };

    need(payload, pos, 5, "saiz default + sample_count")?;
    let default_sample_info_size = payload[pos];
    let sample_count = read_u32(&payload[pos + 1..]);
    pos += 5;

    let sample_info_sizes = if default_sample_info_size == 0 {
        let want = sample_count as usize;
        need(payload, pos, want, "saiz per-sample size table")?;
        payload[pos..pos + want].to_vec()
    } else {
        Vec::new()
    };

    Ok(Saiz {
        flags,
        aux_info_type,
        default_sample_info_size,
        sample_count,
        sample_info_sizes,
    })
}

/// Parse a `saio` (Sample Auxiliary Information Offsets Box) payload —
/// ISO/IEC 14496-12 §8.7.9.2.
///
/// Rejects:
/// * payload shorter than the 4-byte FullBox header;
/// * `version > 1` (the spec defines only v0 / v1);
/// * `flags & 1` set but the body too short for the 8-byte
///   `(aux_info_type, aux_info_type_parameter)` pair;
/// * body too short for `entry_count:4`;
/// * body too short for the `entry_count`-long offset table at the
///   declared width (4 bytes per offset for v0, 8 bytes for v1);
/// * trailing bytes past the offset table (a malformed writer or
///   truncated input — there is no padding by spec).
pub fn parse_saio(payload: &[u8]) -> Result<Saio> {
    need(payload, 0, 4, "saio FullBox header")?;
    let version = payload[0];
    if version > 1 {
        return Err(Error::invalid(format!(
            "MOV: saio unknown version {version} (spec defines only v0 / v1)"
        )));
    }
    let flags = read_u24(&payload[1..]);

    let mut pos = 4usize;
    let aux_info_type = if (flags & 1) != 0 {
        need(payload, pos, 8, "saio aux_info_type pair")?;
        let mut t = [0u8; 4];
        t.copy_from_slice(&payload[pos..pos + 4]);
        let p = read_u32(&payload[pos + 4..]);
        pos += 8;
        Some(AuxInfoType {
            aux_info_type: t,
            aux_info_type_parameter: p,
        })
    } else {
        None
    };

    need(payload, pos, 4, "saio entry_count")?;
    let entry_count = read_u32(&payload[pos..]);
    pos += 4;

    let offset_width = if version == 0 { 4 } else { 8 };
    let want = (entry_count as usize)
        .checked_mul(offset_width)
        .ok_or_else(|| Error::invalid("MOV: saio entry_count overflow"))?;
    need(payload, pos, want, "saio offset table")?;
    let end = pos + want;
    if end != payload.len() {
        return Err(Error::invalid(format!(
            "MOV: saio trailing {} bytes past offset table",
            payload.len() - end
        )));
    }

    let mut offsets = Vec::with_capacity(entry_count as usize);
    for i in 0..(entry_count as usize) {
        let off = pos + i * offset_width;
        let v = if version == 0 {
            read_u32(&payload[off..]) as u64
        } else {
            read_u64(&payload[off..])
        };
        offsets.push(v);
    }

    Ok(Saio {
        version,
        flags,
        aux_info_type,
        offsets,
    })
}

// ─────────────────────── helpers ───────────────────────

#[inline]
fn need(buf: &[u8], at: usize, want: usize, ctx: &str) -> Result<()> {
    let end = at
        .checked_add(want)
        .ok_or_else(|| Error::invalid(format!("MOV: {ctx} length overflow")))?;
    if buf.len() < end {
        return Err(Error::invalid(format!(
            "MOV: {ctx} needs {want} bytes at offset {at}, have {}",
            buf.len().saturating_sub(at)
        )));
    }
    Ok(())
}

#[inline]
fn read_u24(buf: &[u8]) -> u32 {
    ((buf[0] as u32) << 16) | ((buf[1] as u32) << 8) | (buf[2] as u32)
}

#[inline]
fn read_u32(buf: &[u8]) -> u32 {
    u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]])
}

#[inline]
fn read_u64(buf: &[u8]) -> u64 {
    u64::from_be_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `saiz` payload from `(flags, [aux_type, aux_param]?,
    /// default_size, sample_count, &per_sample_sizes)`. The optional
    /// `aux` pair is included iff `flags & 1` is set.
    fn build_saiz(
        flags: u32,
        aux: Option<(&[u8; 4], u32)>,
        default_size: u8,
        sample_count: u32,
        sizes: &[u8],
    ) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(0); // version
        let f = flags.to_be_bytes();
        p.extend_from_slice(&f[1..4]); // 3-byte flags
        if let Some((t, par)) = aux {
            p.extend_from_slice(t);
            p.extend_from_slice(&par.to_be_bytes());
        }
        p.push(default_size);
        p.extend_from_slice(&sample_count.to_be_bytes());
        if default_size == 0 {
            p.extend_from_slice(sizes);
        }
        p
    }

    /// Build a `saio` payload from `(version, flags, [aux_type,
    /// aux_param]?, &offsets)`. The offsets are widened-to-u64 from
    /// the caller; the encoder narrows back to u32 when `version ==
    /// 0`.
    fn build_saio(
        version: u8,
        flags: u32,
        aux: Option<(&[u8; 4], u32)>,
        offsets: &[u64],
    ) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(version);
        let f = flags.to_be_bytes();
        p.extend_from_slice(&f[1..4]);
        if let Some((t, par)) = aux {
            p.extend_from_slice(t);
            p.extend_from_slice(&par.to_be_bytes());
        }
        p.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
        for &o in offsets {
            if version == 0 {
                p.extend_from_slice(&(o as u32).to_be_bytes());
            } else {
                p.extend_from_slice(&o.to_be_bytes());
            }
        }
        p
    }

    #[test]
    fn saiz_default_size_decodes() {
        // 8 samples, every one is exactly 16 bytes of aux info.
        let body = build_saiz(0, None, 16, 8, &[]);
        let s = parse_saiz(&body).expect("saiz default-size parses");
        assert_eq!(s.default_sample_info_size, 16);
        assert_eq!(s.sample_count, 8);
        assert!(s.sample_info_sizes.is_empty());
        assert!(s.aux_info_type.is_none());
        assert_eq!(s.size_for(0), Some(16));
        assert_eq!(s.size_for(7), Some(16));
        assert_eq!(s.size_for(8), None);
        assert_eq!(s.total_size(), 16 * 8);
    }

    #[test]
    fn saiz_per_sample_sizes_decode() {
        let body = build_saiz(0, None, 0, 4, &[10, 0, 24, 8]);
        let s = parse_saiz(&body).expect("saiz per-sample parses");
        assert_eq!(s.default_sample_info_size, 0);
        assert_eq!(s.sample_info_sizes, vec![10, 0, 24, 8]);
        assert_eq!(s.size_for(0), Some(10));
        assert_eq!(s.size_for(1), Some(0));
        assert_eq!(s.size_for(2), Some(24));
        assert_eq!(s.size_for(3), Some(8));
        assert_eq!(s.size_for(4), None);
        assert_eq!(s.total_size(), 10 + 24 + 8);
    }

    #[test]
    fn saiz_with_aux_info_type_decodes() {
        let body = build_saiz(0x01, Some((b"cenc", 0)), 24, 3, &[]);
        let s = parse_saiz(&body).expect("saiz w/ aux parses");
        let a = s.aux_info_type.unwrap();
        assert_eq!(&a.aux_info_type, b"cenc");
        assert_eq!(a.aux_info_type_parameter, 0);
        assert!(a.matches(b"cenc", 0));
        assert!(!a.matches(b"cbcs", 0));
    }

    #[test]
    fn saiz_version_nonzero_is_rejected() {
        let mut body = build_saiz(0, None, 16, 1, &[]);
        body[0] = 1; // illegal version
        assert!(parse_saiz(&body).is_err());
    }

    #[test]
    fn saiz_per_sample_truncated_is_rejected() {
        // sample_count = 4 but only 2 size bytes present
        let body = build_saiz(0, None, 0, 4, &[10, 20]);
        assert!(parse_saiz(&body).is_err());
    }

    #[test]
    fn saiz_short_header_is_rejected() {
        assert!(parse_saiz(&[0u8; 3]).is_err());
        // FullBox header present but missing default+sample_count
        assert!(parse_saiz(&[0u8, 0, 0, 0]).is_err());
    }

    #[test]
    fn saiz_aux_flag_set_but_pair_missing_is_rejected() {
        // flags & 1 set, but the 8-byte pair is absent
        let body = vec![0u8, 0, 0, 1, 16, 0, 0, 0, 1];
        assert!(parse_saiz(&body).is_err());
    }

    #[test]
    fn saio_v0_offsets_decode() {
        let body = build_saio(0, 0, None, &[0x100, 0x200, 0x300]);
        let s = parse_saio(&body).expect("saio v0 parses");
        assert_eq!(s.version, 0);
        assert_eq!(s.offsets, vec![0x100u64, 0x200, 0x300]);
        assert!(!s.is_single_chunk());
        assert_eq!(s.offset_for(1), Some(0x200));
        assert_eq!(s.offset_for(3), None);
    }

    #[test]
    fn saio_v1_64bit_offsets_decode() {
        let big = 0x1_0000_0000u64;
        let body = build_saio(1, 0, None, &[big]);
        let s = parse_saio(&body).expect("saio v1 parses");
        assert_eq!(s.version, 1);
        assert_eq!(s.offsets, vec![big]);
        assert!(s.is_single_chunk());
    }

    #[test]
    fn saio_with_aux_info_type_decodes() {
        let body = build_saio(0, 0x01, Some((b"cbcs", 7)), &[0x42]);
        let s = parse_saio(&body).expect("saio w/ aux parses");
        let a = s.aux_info_type.unwrap();
        assert_eq!(&a.aux_info_type, b"cbcs");
        assert_eq!(a.aux_info_type_parameter, 7);
    }

    #[test]
    fn saio_unknown_version_is_rejected() {
        let mut body = build_saio(0, 0, None, &[0]);
        body[0] = 2;
        assert!(parse_saio(&body).is_err());
    }

    #[test]
    fn saio_truncated_table_is_rejected() {
        // entry_count = 3, but only 2 × 4 bytes of offsets
        let mut body = Vec::new();
        body.extend_from_slice(&[0u8, 0, 0, 0]); // version + flags
        body.extend_from_slice(&3u32.to_be_bytes()); // entry_count = 3
        body.extend_from_slice(&0x10u32.to_be_bytes());
        body.extend_from_slice(&0x20u32.to_be_bytes());
        assert!(parse_saio(&body).is_err());
    }

    #[test]
    fn saio_trailing_bytes_are_rejected() {
        let mut body = build_saio(0, 0, None, &[0x10]);
        body.push(0xFF); // unexpected pad
        assert!(parse_saio(&body).is_err());
    }

    #[test]
    fn saio_aux_flag_set_but_pair_missing_is_rejected() {
        // flags & 1 set, but the 8-byte pair is absent
        let body = vec![0u8, 0, 0, 1, 0, 0, 0, 0];
        assert!(parse_saio(&body).is_err());
    }
}
