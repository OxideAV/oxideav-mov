//! Subsegment Index Box (`ssix`).
//!
//! ISO/IEC 14496-12:2015 §8.16.4 (pp. 109–110). The Subsegment Index
//! Box provides a mapping from *levels* (declared by a separate Level
//! Assignment Box, §8.8.13) to byte ranges within an indexed
//! subsegment — a compact "table of contents" describing how a
//! subsegment is ordered into *partial subsegments*, each tagged with
//! a level number. Adaptive-streaming clients (DASH / CMAF) use it to
//! fetch a partial subsegment without downloading the whole
//! subsegment: a temporal-scalability decoder, for example, can pull
//! only the lowest-level (base-layer) frames and skip the enhancement
//! layers it does not need (§8.16.4.1).
//!
//! Each `ssix` lives at file scope and is paired one-to-one with the
//! immediately preceding `sidx` box that indexes only leaf
//! subsegments (`Quantity: 0 or 1` per associated `sidx`). The
//! subsegment count in `ssix` must equal the preceding `sidx`'s
//! `reference_count` (§8.16.4.3). Inside each subsegment the range
//! count must be ≥ 2 — every byte must be assigned to a level
//! (§8.16.4.1) — and the per-range `range_size` is a 24-bit unsigned
//! integer (§8.16.4.2 / §8.16.4.3).
//!
//! Layout per §8.16.4.2:
//!
//! ```text
//! aligned(8) class SubsegmentIndexBox extends FullBox('ssix', 0, 0) {
//!     unsigned int(32) subsegment_count;
//!     for (i = 1; i <= subsegment_count; i++) {
//!         unsigned int(32) range_count;
//!         for (j = 1; j <= range_count; j++) {
//!             unsigned int(8)  level;
//!             unsigned int(24) range_size;
//!         }
//!     }
//! }
//! ```
//!
//! QTFF does not define `ssix`; it is an ISO BMFF-only box and stays
//! absent for plain `.mov` inputs.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// One `(level, range_size)` row from an [`SsixSubsegment`] — a single
/// partial subsegment within its containing subsegment. The decoded
/// `level` matches the level numbering assigned by the §8.8.13 Level
/// Assignment Box; the `range_size` is the byte count of the partial
/// subsegment (§8.16.4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SsixRange {
    /// Level identifier this partial subsegment carries (§8.16.4.3).
    /// Spec assigns one byte; the meaning is owned by the paired
    /// Level Assignment Box.
    pub level: u8,
    /// Size of the partial subsegment in bytes (§8.16.4.3). 24-bit
    /// on-disk field, widened to `u32` for the in-memory surface.
    pub range_size: u32,
}

/// One subsegment entry from an [`Ssix`]'s outer loop (§8.16.4.2 /
/// §8.16.4.3). Describes how the subsegment at the same index in the
/// preceding `sidx` is partitioned into partial subsegments by level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SsixSubsegment {
    /// Partial-subsegment rows in declaration order. Per §8.16.4.1
    /// `range_count` is always `>= 2` (every byte of the subsegment
    /// must be assigned to a level).
    pub ranges: Vec<SsixRange>,
}

/// Parsed Subsegment Index Box (ISO/IEC 14496-12 §8.16.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ssix {
    /// Per-subsegment partial-subsegment lists in declaration order.
    /// Length equals the on-disk `subsegment_count`, which must match
    /// the `reference_count` of the immediately preceding `sidx`
    /// (§8.16.4.3).
    pub subsegments: Vec<SsixSubsegment>,
}

impl Ssix {
    /// Number of subsegments this box indexes — the on-disk
    /// `subsegment_count` field. Convenience accessor that matches
    /// the spec field name (§8.16.4.3).
    pub fn subsegment_count(&self) -> u32 {
        self.subsegments.len() as u32
    }

    /// Sum of `range_size` across every partial subsegment of
    /// subsegment `index` (0-based) — the total byte length of the
    /// indexed subsegment. Returns `None` when `index` is out of
    /// range or the accumulation overflows `u64`.
    ///
    /// §8.16.4.1: "Each byte in the subsegment shall be explicitly
    /// assigned to a level", so the per-subsegment range_size sum is
    /// the subsegment's total size. The result is widened to `u64`
    /// because a subsegment may exceed 4 GiB even though each
    /// individual range is bounded by the 24-bit `range_size` field.
    pub fn total_size_for(&self, index: usize) -> Option<u64> {
        let sub = self.subsegments.get(index)?;
        let mut total: u64 = 0;
        for r in &sub.ranges {
            total = total.checked_add(r.range_size as u64)?;
        }
        Some(total)
    }

    /// The absolute file offset at which the partial subsegment at
    /// `range_index` (0-based) of subsegment `index` (0-based) begins,
    /// given `subsegment_start` — the file offset of the first byte of
    /// the indexed subsegment (typically resolved from the preceding
    /// `sidx`'s [`crate::sidx::Sidx::subsegment_offset`]).
    ///
    /// Computes `subsegment_start + Σ range_size[0..range_index]`
    /// because the partial subsegments lay out contiguously inside
    /// their parent subsegment (§8.16.4.1: "byte ranges for one level
    /// shall be contiguous"). Returns `None` when either index is out
    /// of range or any addition overflows.
    pub fn partial_subsegment_offset(
        &self,
        subsegment_start: u64,
        index: usize,
        range_index: usize,
    ) -> Option<u64> {
        let sub = self.subsegments.get(index)?;
        if range_index >= sub.ranges.len() {
            return None;
        }
        let mut off = subsegment_start;
        for r in &sub.ranges[..range_index] {
            off = off.checked_add(r.range_size as u64)?;
        }
        Some(off)
    }
}

/// Parse an `ssix` payload.
///
/// Layout per ISO/IEC 14496-12 §8.16.4.2 — see the module-level docs.
///
/// Returns `Error::invalid` when:
/// * the payload is shorter than the 4-byte FullBox header + the
///   `subsegment_count` u32,
/// * the FullBox `version` is non-zero (the spec fixes it at 0),
/// * a declared `range_count` is below 2 (§8.16.4.1: each byte must
///   be assigned to a level, so a single partial subsegment cannot
///   exist on its own — the box's purpose is the partition),
/// * a `range_count` or `subsegment_count` overruns the remaining
///   bytes (each range is exactly 4 bytes; a partial trailing
///   subsegment / range indicates corruption),
/// * any trailing bytes remain after the declared subsegment list
///   (the box carries no list past `subsegment_count` — leftover
///   bytes signal a truncated or padded writer that can't be
///   safely consumed).
pub fn parse_ssix(payload: &[u8]) -> Result<Ssix> {
    if payload.len() < 8 {
        return Err(Error::invalid(format!(
            "MOV: ssix payload {} < 8-byte FullBox header + subsegment_count",
            payload.len()
        )));
    }
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "MOV: ssix unknown version {version} (spec fixes at 0)"
        )));
    }
    // `flags` (payload[1..4]) is fixed at 0 by the spec; vendors
    // occasionally set bits, so the parser tolerates them silently —
    // matching the `sidx` parser convention.

    let mut pos = 4usize; // skip ver+flags
    let subsegment_count = read_u32(payload, &mut pos);

    // Bound the up-front allocation: every subsegment carries at
    // least 4 bytes of `range_count`. A 4-byte body cannot hold even
    // one subsegment header, so any declared count above
    // `(payload.len() - pos) / 4` cannot fit even the lightest
    // representation — refuse before allocating.
    let remaining = payload.len() - pos;
    if (subsegment_count as u64).saturating_mul(4) > remaining as u64 {
        return Err(Error::invalid(format!(
            "MOV: ssix subsegment_count {subsegment_count} cannot fit in {remaining} body bytes",
        )));
    }

    let mut subsegments = Vec::with_capacity(subsegment_count as usize);
    for s in 0..subsegment_count {
        if pos + 4 > payload.len() {
            return Err(Error::invalid(format!(
                "MOV: ssix subsegment {s} truncated reading range_count"
            )));
        }
        let range_count = read_u32(payload, &mut pos);
        // §8.16.4.1: "Each byte in the subsegment shall be explicitly
        // assigned to a level, and hence the range count must be 2 or
        // greater."
        if range_count < 2 {
            return Err(Error::invalid(format!(
                "MOV: ssix subsegment {s} range_count {range_count} < 2 (§8.16.4.1)"
            )));
        }
        // Each range is 4 bytes (1-byte level + 3-byte range_size).
        let rc_bytes = (range_count as u64).checked_mul(4).ok_or_else(|| {
            Error::invalid(format!(
                "MOV: ssix subsegment {s} range_count {range_count} overflows byte count"
            ))
        })?;
        if rc_bytes > (payload.len() - pos) as u64 {
            return Err(Error::invalid(format!(
                "MOV: ssix subsegment {s} declares {range_count} ranges \
                 ({rc_bytes} bytes) but only {} body bytes remain",
                payload.len() - pos
            )));
        }
        let mut ranges = Vec::with_capacity(range_count as usize);
        for _ in 0..range_count {
            let level = payload[pos];
            // 24-bit big-endian range_size.
            let range_size = ((payload[pos + 1] as u32) << 16)
                | ((payload[pos + 2] as u32) << 8)
                | (payload[pos + 3] as u32);
            pos += 4;
            ranges.push(SsixRange { level, range_size });
        }
        subsegments.push(SsixSubsegment { ranges });
    }

    if pos != payload.len() {
        return Err(Error::invalid(format!(
            "MOV: ssix has {} bytes of unconsumed trailing data after \
             {subsegment_count} subsegments",
            payload.len() - pos
        )));
    }

    Ok(Ssix { subsegments })
}

#[inline]
fn read_u32(buf: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pack one 4-byte `(level, 24-bit range_size)` row per §8.16.4.2.
    fn range_bytes(level: u8, range_size: u32) -> [u8; 4] {
        assert!(range_size < (1 << 24), "range_size must fit in 24 bits");
        [
            level,
            (range_size >> 16) as u8,
            (range_size >> 8) as u8,
            range_size as u8,
        ]
    }

    /// Build an `ssix` body (FullBox header + subsegment list) from a
    /// list of per-subsegment range vectors.
    fn build_ssix(subs: &[Vec<[u8; 4]>]) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(0); // version
        p.extend_from_slice(&[0, 0, 0]); // flags
        p.extend_from_slice(&(subs.len() as u32).to_be_bytes());
        for s in subs {
            p.extend_from_slice(&(s.len() as u32).to_be_bytes());
            for r in s {
                p.extend_from_slice(r);
            }
        }
        p
    }

    #[test]
    fn parses_minimal_two_range_subsegment() {
        let p = build_ssix(&[vec![range_bytes(1, 100), range_bytes(2, 200)]]);
        let ssix = parse_ssix(&p).unwrap();
        assert_eq!(ssix.subsegment_count(), 1);
        assert_eq!(ssix.subsegments[0].ranges.len(), 2);
        assert_eq!(ssix.subsegments[0].ranges[0].level, 1);
        assert_eq!(ssix.subsegments[0].ranges[0].range_size, 100);
        assert_eq!(ssix.subsegments[0].ranges[1].level, 2);
        assert_eq!(ssix.subsegments[0].ranges[1].range_size, 200);
    }

    #[test]
    fn parses_multiple_subsegments_with_different_levels() {
        let p = build_ssix(&[
            vec![range_bytes(0, 50), range_bytes(1, 60)],
            vec![range_bytes(0, 70), range_bytes(1, 80), range_bytes(2, 90)],
            vec![range_bytes(0, 100), range_bytes(1, 110)],
        ]);
        let ssix = parse_ssix(&p).unwrap();
        assert_eq!(ssix.subsegment_count(), 3);
        assert_eq!(ssix.subsegments[0].ranges.len(), 2);
        assert_eq!(ssix.subsegments[1].ranges.len(), 3);
        assert_eq!(ssix.subsegments[2].ranges.len(), 2);
        assert_eq!(ssix.subsegments[1].ranges[2].range_size, 90);
    }

    #[test]
    fn empty_subsegment_list_is_legal() {
        // subsegment_count = 0 carries no range loops and decodes
        // cleanly — a degenerate but structurally valid box (the
        // spec does not forbid it, and the conservative parse posture
        // is to accept it rather than reject).
        let p = build_ssix(&[]);
        let ssix = parse_ssix(&p).unwrap();
        assert_eq!(ssix.subsegment_count(), 0);
    }

    #[test]
    fn max_width_24bit_range_size_round_trips() {
        let max = (1u32 << 24) - 1;
        let p = build_ssix(&[vec![range_bytes(255, max), range_bytes(0, 1)]]);
        let ssix = parse_ssix(&p).unwrap();
        assert_eq!(ssix.subsegments[0].ranges[0].level, 255);
        assert_eq!(ssix.subsegments[0].ranges[0].range_size, max);
    }

    #[test]
    fn unknown_version_rejected() {
        let mut p = build_ssix(&[vec![range_bytes(0, 1), range_bytes(1, 2)]]);
        p[0] = 1; // spec fixes version at 0
        assert!(parse_ssix(&p).is_err());
    }

    #[test]
    fn range_count_one_rejected() {
        // §8.16.4.1 — range_count must be >= 2. A writer emitting a
        // single-range subsegment violates the "every byte assigned
        // to a level" rule.
        let p = build_ssix(&[vec![range_bytes(0, 100)]]);
        assert!(parse_ssix(&p).is_err());
    }

    #[test]
    fn range_count_zero_rejected() {
        let p = build_ssix(&[vec![]]);
        assert!(parse_ssix(&p).is_err());
    }

    #[test]
    fn truncated_header_rejected() {
        // Less than 8 bytes (ver+flags + subsegment_count).
        let p = vec![0u8, 0, 0, 0, 0, 0, 0];
        assert!(parse_ssix(&p).is_err());
    }

    #[test]
    fn subsegment_count_overrun_rejected() {
        // Declare 4 subsegments but supply bytes for 1.
        let mut p = build_ssix(&[vec![range_bytes(0, 1), range_bytes(1, 2)]]);
        // Patch the subsegment_count u32 (offset 4..8) up to 4.
        p[4..8].copy_from_slice(&4u32.to_be_bytes());
        assert!(parse_ssix(&p).is_err());
    }

    #[test]
    fn range_count_overrun_rejected() {
        // Subsegment promises 5 ranges but the body holds 2.
        let mut p = build_ssix(&[vec![range_bytes(0, 1), range_bytes(1, 2)]]);
        // The range_count u32 sits at offset 8..12 (after 8-byte
        // FullBox+subsegment_count header).
        p[8..12].copy_from_slice(&5u32.to_be_bytes());
        assert!(parse_ssix(&p).is_err());
    }

    #[test]
    fn trailing_bytes_after_subsegment_list_rejected() {
        // Append 3 stray bytes — not enough for a range, but illegal
        // since the box carries no list past the declared count.
        let mut p = build_ssix(&[vec![range_bytes(0, 1), range_bytes(1, 2)]]);
        p.extend_from_slice(&[0u8, 0, 0]);
        assert!(parse_ssix(&p).is_err());
    }

    #[test]
    fn total_size_for_sums_24bit_range_sizes() {
        let p = build_ssix(&[
            vec![range_bytes(0, 1000), range_bytes(1, 2000)],
            vec![
                range_bytes(0, 500),
                range_bytes(1, 1500),
                range_bytes(2, 250),
            ],
        ]);
        let ssix = parse_ssix(&p).unwrap();
        assert_eq!(ssix.total_size_for(0), Some(3000));
        assert_eq!(ssix.total_size_for(1), Some(2250));
        assert_eq!(ssix.total_size_for(2), None);
    }

    #[test]
    fn partial_subsegment_offsets_walk_range_size_chain() {
        let p = build_ssix(&[vec![
            range_bytes(0, 1000),
            range_bytes(1, 500),
            range_bytes(2, 250),
        ]]);
        let ssix = parse_ssix(&p).unwrap();
        // subsegment_start = 10_000.
        assert_eq!(ssix.partial_subsegment_offset(10_000, 0, 0), Some(10_000));
        assert_eq!(ssix.partial_subsegment_offset(10_000, 0, 1), Some(11_000));
        assert_eq!(ssix.partial_subsegment_offset(10_000, 0, 2), Some(11_500));
        // range_index past last partial subsegment → None.
        assert_eq!(ssix.partial_subsegment_offset(10_000, 0, 3), None);
        // subsegment index out of range → None.
        assert_eq!(ssix.partial_subsegment_offset(10_000, 1, 0), None);
    }

    #[test]
    fn partial_subsegment_offset_overflow_returns_none() {
        let max = (1u32 << 24) - 1;
        let p = build_ssix(&[vec![range_bytes(0, max), range_bytes(1, max)]]);
        let ssix = parse_ssix(&p).unwrap();
        // subsegment_start near u64::MAX → adding `max` will overflow.
        assert_eq!(
            ssix.partial_subsegment_offset(u64::MAX, 0, 1),
            None,
            "addition past u64::MAX must yield None"
        );
        // First partial subsegment uses no addition — accepted.
        assert_eq!(
            ssix.partial_subsegment_offset(u64::MAX, 0, 0),
            Some(u64::MAX)
        );
    }

    #[test]
    fn declared_count_too_large_rejected_before_allocation() {
        // Forge a subsegment_count whose minimum-encoding bytes
        // (4 per subsegment for range_count alone) cannot fit in the
        // body. Without the up-front bound the parser would allocate
        // `Vec::with_capacity(huge)` before failing on the per-row
        // read — the explicit check catches it earlier.
        let mut p = vec![0u8, 0, 0, 0]; // ver+flags
        p.extend_from_slice(&u32::MAX.to_be_bytes()); // huge subsegment_count
        assert!(parse_ssix(&p).is_err());
    }
}
