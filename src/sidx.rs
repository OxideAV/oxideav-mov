//! Segment Index Box (`sidx`).
//!
//! ISO/IEC 14496-12:2015 §8.16.3 (pp. 105–108). The Segment Index Box
//! provides a compact index of one media stream within the media
//! (sub)segment to which it applies. It documents how a (sub)segment is
//! divided into one or more subsegments — each a contiguous time
//! interval mapping to a contiguous byte range — and records, per
//! subsegment, the byte size, duration, and Stream-Access-Point (SAP)
//! information used by adaptive-streaming clients (DASH / HLS / CMAF) to
//! seek into and switch between media segments without parsing the
//! `moof`s themselves (§8.16.3.1).
//!
//! Each reference in the box either points directly at media bytes
//! (`reference_type = 0`, e.g. a self-contained set of movie fragments)
//! or at a nested `sidx` describing a finer subdivision of the same
//! (sub)segment (`reference_type = 1`), letting a writer index a stream
//! in hierarchical / daisy-chain form.
//!
//! Layout per ISO/IEC 14496-12 §8.16.3.2:
//!
//! ```text
//! aligned(8) class SegmentIndexBox extends FullBox('sidx', version, 0) {
//!     unsigned int(32) reference_ID;
//!     unsigned int(32) timescale;
//!     if (version == 0) {
//!         unsigned int(32) earliest_presentation_time;
//!         unsigned int(32) first_offset;
//!     } else {
//!         unsigned int(64) earliest_presentation_time;
//!         unsigned int(64) first_offset;
//!     }
//!     unsigned int(16) reserved = 0;
//!     unsigned int(16) reference_count;
//!     for (i = 1; i <= reference_count; i++) {
//!         bit(1)           reference_type;
//!         unsigned int(31) referenced_size;
//!         unsigned int(32) subsegment_duration;
//!         bit(1)           starts_with_SAP;
//!         unsigned int(3)  SAP_type;
//!         unsigned int(28) SAP_delta_time;
//!     }
//! }
//! ```
//!
//! The box lives at file scope (`Container: File`, §8.16.3.1) and a
//! segment may carry zero or more (`Quantity: Zero or more`). QTFF does
//! not define `sidx`; it is an ISO BMFF-only box and stays absent for
//! plain `.mov` inputs.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// What a [`SidxReference`] points at (§8.16.3.3 `reference_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceType {
    /// `reference_type = 0` — the reference points directly at media
    /// content (for files based on this specification, a self-contained
    /// set of one or more movie fragments).
    Media,
    /// `reference_type = 1` — the reference points at a nested Segment
    /// Index Box (`sidx`) that further subdivides the referenced
    /// subsegment.
    Index,
}

/// One entry from a [`Sidx`]'s reference loop (§8.16.3.2 /
/// §8.16.3.3). Describes a single subsegment of the indexed stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidxReference {
    /// Whether this reference points at media bytes or a nested `sidx`.
    pub reference_type: ReferenceType,
    /// The distance in bytes from the first byte of the referenced item
    /// to the first byte of the next referenced item (or, for the last
    /// entry, the end of the referenced material). 31-bit field —
    /// always `<= 0x7FFF_FFFF`.
    pub referenced_size: u32,
    /// Duration of the referenced subsegment, in the box's `timescale`
    /// ticks. For a `Media` reference, the difference between the
    /// earliest presentation time of the next subsegment and that of
    /// this one; for an `Index` reference, the sum of the nested box's
    /// `subsegment_duration` fields.
    pub subsegment_duration: u32,
    /// Whether the referenced subsegment starts with a Stream Access
    /// Point. See [`Sidx`] docs / Table 4 (§8.16.3.3) for the combined
    /// semantics with `sap_type`.
    pub starts_with_sap: bool,
    /// SAP type per Annex I, or 0. 3-bit field — always `<= 7`. Values
    /// 1..=6 are defined SAP types; 0 means "no SAP information" (when
    /// `starts_with_sap` is false) or "unknown type" (when it is true).
    pub sap_type: u8,
    /// TSAP of the first SAP (in decoding order) in the referenced
    /// subsegment, expressed as the difference from the subsegment's
    /// earliest presentation time, in `timescale` ticks. Reserved with
    /// value 0 when the subsegment contains no SAP. 28-bit field —
    /// always `<= 0x0FFF_FFFF`.
    pub sap_delta_time: u32,
}

/// Parsed Segment Index Box (ISO/IEC 14496-12 §8.16.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sidx {
    /// `0` or `1` — selects the 32-bit vs 64-bit width of
    /// `earliest_presentation_time` / `first_offset`.
    pub version: u8,
    /// Stream ID for the reference stream this box indexes. For files
    /// based on this specification, this is a track ID (§8.16.3.3).
    pub reference_id: u32,
    /// Ticks per second for the time / duration fields in this box.
    /// Recommended to match the reference track's Media Header Box
    /// timescale (§8.16.3.3).
    pub timescale: u32,
    /// Earliest presentation time of any content in the reference
    /// stream in the *first* subsegment, in `timescale` ticks
    /// (§8.16.3.3). Widened to `u64` to cover both `version = 0` (32-bit
    /// on disk) and `version = 1` (64-bit).
    pub earliest_presentation_time: u64,
    /// Distance in bytes, from the box's anchor point (the first byte
    /// after this box, §8.16.3.1), to the first byte of the indexed
    /// material (§8.16.3.3). Widened to `u64` as for the presentation
    /// time.
    pub first_offset: u64,
    /// Per-subsegment references in file order. Length equals the
    /// on-disk `reference_count`.
    pub references: Vec<SidxReference>,
}

impl Sidx {
    /// The absolute file offset of the first byte of the indexed
    /// material, given `anchor` — the file offset of the first byte
    /// *after* this `sidx` box (the box's anchor point per §8.16.3.1).
    ///
    /// For an integrated file (index inline with the media), the indexed
    /// material starts at `anchor + first_offset`. Returns `None` on
    /// overflow.
    pub fn material_start(&self, anchor: u64) -> Option<u64> {
        anchor.checked_add(self.first_offset)
    }

    /// The absolute byte offset of subsegment `index` (0-based),
    /// computed by walking the `referenced_size` chain from the indexed
    /// material's start.
    ///
    /// Subsegment `i` begins at `material_start + Σ referenced_size[0..i]`
    /// because §8.16.3.3 defines `referenced_size` as "the distance in
    /// bytes from the first byte of the referenced item to the first
    /// byte of the next referenced item" — i.e. the references are
    /// contiguous in the file (§8.16.3.1). Returns `None` when `index`
    /// is out of range or any addition overflows.
    pub fn subsegment_offset(&self, anchor: u64, index: usize) -> Option<u64> {
        if index >= self.references.len() {
            return None;
        }
        let mut off = self.material_start(anchor)?;
        for r in &self.references[..index] {
            off = off.checked_add(r.referenced_size as u64)?;
        }
        Some(off)
    }

    /// The presentation time (in `timescale` ticks) at which subsegment
    /// `index` (0-based) begins, computed by accumulating
    /// `subsegment_duration` from `earliest_presentation_time`.
    ///
    /// Subsegment durations sum to the duration of the containing
    /// (sub)segment and are contiguous in presentation time
    /// (§8.16.3.1), so subsegment `i` starts at
    /// `earliest_presentation_time + Σ subsegment_duration[0..i]`.
    /// Returns `None` when `index` is out of range or the accumulation
    /// overflows.
    pub fn subsegment_start_time(&self, index: usize) -> Option<u64> {
        if index >= self.references.len() {
            return None;
        }
        let mut t = self.earliest_presentation_time;
        for r in &self.references[..index] {
            t = t.checked_add(r.subsegment_duration as u64)?;
        }
        Some(t)
    }
}

/// Parse a `sidx` payload.
///
/// Layout per ISO/IEC 14496-12 §8.16.3.2 — see the module-level docs.
///
/// Returns `Error::invalid` when:
/// * the payload is shorter than the FullBox header + the fixed-width
///   fields for the declared version,
/// * `version` is neither 0 nor 1 (the spec defines only those two),
/// * the declared `reference_count` does not match the bytes remaining
///   (each reference is exactly 12 bytes; a partial trailing reference
///   or a count that overruns the box indicates a truncated / malformed
///   box).
pub fn parse_sidx(payload: &[u8]) -> Result<Sidx> {
    if payload.len() < 4 {
        return Err(Error::invalid(format!(
            "MOV: sidx payload {} < 4-byte FullBox header",
            payload.len()
        )));
    }
    let version = payload[0];
    if version > 1 {
        return Err(Error::invalid(format!(
            "MOV: sidx unknown version {version} (spec defines only v0 and v1)"
        )));
    }
    // `flags` (payload[1..4]) is fixed at 0 by the spec but not
    // validated — vendor extensions occasionally set bits and silently
    // tolerating them keeps the parse robust.

    // After the FullBox header: reference_ID (4) + timescale (4), then
    // a version-dependent pair (8 bytes for v0, 16 for v1), then
    // reserved (2) + reference_count (2).
    let time_width = if version == 0 { 4 } else { 8 };
    // 4 (reference_ID) + 4 (timescale) + 2*time_width + 2 (reserved) +
    // 2 (reference_count).
    let fixed_len = 4 + 4 + 4 + 2 * time_width + 2 + 2;
    if payload.len() < fixed_len {
        return Err(Error::invalid(format!(
            "MOV: sidx v{version} payload {} < {fixed_len}-byte fixed header",
            payload.len()
        )));
    }

    let mut pos = 4usize; // skip version + flags
    let reference_id = read_u32(payload, &mut pos);
    let timescale = read_u32(payload, &mut pos);
    let (earliest_presentation_time, first_offset) = if version == 0 {
        let ept = read_u32(payload, &mut pos) as u64;
        let fo = read_u32(payload, &mut pos) as u64;
        (ept, fo)
    } else {
        let ept = read_u64(payload, &mut pos);
        let fo = read_u64(payload, &mut pos);
        (ept, fo)
    };
    // reserved (16 bits) — read past, not validated.
    pos += 2;
    let reference_count = read_u16(payload, &mut pos) as usize;

    // Each reference is exactly 12 bytes. The remaining body must hold
    // exactly `reference_count` of them — no more, no less.
    let body = &payload[pos..];
    let expected = reference_count
        .checked_mul(12)
        .ok_or_else(|| Error::invalid("MOV: sidx reference_count overflow"))?;
    if body.len() != expected {
        return Err(Error::invalid(format!(
            "MOV: sidx body {} bytes != reference_count {reference_count} × 12 = {expected}",
            body.len()
        )));
    }

    let mut references = Vec::with_capacity(reference_count);
    for i in 0..reference_count {
        let off = i * 12;
        // word0: [reference_type:1][referenced_size:31]
        let w0 = u32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
        let reference_type = if (w0 >> 31) == 1 {
            ReferenceType::Index
        } else {
            ReferenceType::Media
        };
        let referenced_size = w0 & 0x7FFF_FFFF;
        // word1: subsegment_duration (full 32 bits)
        let subsegment_duration =
            u32::from_be_bytes([body[off + 4], body[off + 5], body[off + 6], body[off + 7]]);
        // word2: [starts_with_SAP:1][SAP_type:3][SAP_delta_time:28]
        let w2 = u32::from_be_bytes([body[off + 8], body[off + 9], body[off + 10], body[off + 11]]);
        let starts_with_sap = (w2 >> 31) == 1;
        let sap_type = ((w2 >> 28) & 0x7) as u8;
        let sap_delta_time = w2 & 0x0FFF_FFFF;
        references.push(SidxReference {
            reference_type,
            referenced_size,
            subsegment_duration,
            starts_with_sap,
            sap_type,
            sap_delta_time,
        });
    }

    Ok(Sidx {
        version,
        reference_id,
        timescale,
        earliest_presentation_time,
        first_offset,
        references,
    })
}

#[inline]
fn read_u16(buf: &[u8], pos: &mut usize) -> u16 {
    let v = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]);
    *pos += 2;
    v
}

#[inline]
fn read_u32(buf: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    v
}

#[inline]
fn read_u64(buf: &[u8], pos: &mut usize) -> u64 {
    let v = u64::from_be_bytes([
        buf[*pos],
        buf[*pos + 1],
        buf[*pos + 2],
        buf[*pos + 3],
        buf[*pos + 4],
        buf[*pos + 5],
        buf[*pos + 6],
        buf[*pos + 7],
    ]);
    *pos += 8;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pack a 12-byte reference triple the way §8.16.3.2 lays it out.
    fn ref_bytes(
        reference_type: u8,
        referenced_size: u32,
        subsegment_duration: u32,
        starts_with_sap: u8,
        sap_type: u8,
        sap_delta_time: u32,
    ) -> [u8; 12] {
        let w0 = ((reference_type as u32 & 1) << 31) | (referenced_size & 0x7FFF_FFFF);
        let w2 = ((starts_with_sap as u32 & 1) << 31)
            | ((sap_type as u32 & 0x7) << 28)
            | (sap_delta_time & 0x0FFF_FFFF);
        let mut out = [0u8; 12];
        out[0..4].copy_from_slice(&w0.to_be_bytes());
        out[4..8].copy_from_slice(&subsegment_duration.to_be_bytes());
        out[8..12].copy_from_slice(&w2.to_be_bytes());
        out
    }

    fn build_sidx_v0(
        reference_id: u32,
        timescale: u32,
        ept: u32,
        first_offset: u32,
        refs: &[[u8; 12]],
    ) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(0); // version
        p.extend_from_slice(&[0, 0, 0]); // flags
        p.extend_from_slice(&reference_id.to_be_bytes());
        p.extend_from_slice(&timescale.to_be_bytes());
        p.extend_from_slice(&ept.to_be_bytes());
        p.extend_from_slice(&first_offset.to_be_bytes());
        p.extend_from_slice(&[0, 0]); // reserved
        p.extend_from_slice(&(refs.len() as u16).to_be_bytes());
        for r in refs {
            p.extend_from_slice(r);
        }
        p
    }

    fn build_sidx_v1(
        reference_id: u32,
        timescale: u32,
        ept: u64,
        first_offset: u64,
        refs: &[[u8; 12]],
    ) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(1); // version
        p.extend_from_slice(&[0, 0, 0]); // flags
        p.extend_from_slice(&reference_id.to_be_bytes());
        p.extend_from_slice(&timescale.to_be_bytes());
        p.extend_from_slice(&ept.to_be_bytes());
        p.extend_from_slice(&first_offset.to_be_bytes());
        p.extend_from_slice(&[0, 0]); // reserved
        p.extend_from_slice(&(refs.len() as u16).to_be_bytes());
        for r in refs {
            p.extend_from_slice(r);
        }
        p
    }

    #[test]
    fn parses_v0_two_media_references() {
        let r0 = ref_bytes(0, 4096, 30_000, 1, 1, 0);
        let r1 = ref_bytes(0, 8192, 30_000, 1, 1, 0);
        let p = build_sidx_v0(1, 90_000, 0, 0, &[r0, r1]);
        let sidx = parse_sidx(&p).unwrap();
        assert_eq!(sidx.version, 0);
        assert_eq!(sidx.reference_id, 1);
        assert_eq!(sidx.timescale, 90_000);
        assert_eq!(sidx.earliest_presentation_time, 0);
        assert_eq!(sidx.first_offset, 0);
        assert_eq!(sidx.references.len(), 2);
        assert_eq!(sidx.references[0].reference_type, ReferenceType::Media);
        assert_eq!(sidx.references[0].referenced_size, 4096);
        assert_eq!(sidx.references[0].subsegment_duration, 30_000);
        assert!(sidx.references[0].starts_with_sap);
        assert_eq!(sidx.references[0].sap_type, 1);
        assert_eq!(sidx.references[0].sap_delta_time, 0);
        assert_eq!(sidx.references[1].referenced_size, 8192);
    }

    #[test]
    fn parses_v1_wide_fields() {
        // Presentation time + first_offset beyond the 32-bit range.
        let ept = 0x1_0000_0001u64;
        let fo = 0x2_0000_0002u64;
        let r0 = ref_bytes(0, 1000, 5000, 1, 2, 0);
        let p = build_sidx_v1(2, 48_000, ept, fo, &[r0]);
        let sidx = parse_sidx(&p).unwrap();
        assert_eq!(sidx.version, 1);
        assert_eq!(sidx.earliest_presentation_time, ept);
        assert_eq!(sidx.first_offset, fo);
        assert_eq!(sidx.references.len(), 1);
        assert_eq!(sidx.references[0].sap_type, 2);
    }

    #[test]
    fn reference_type_index_bit_decoded() {
        // reference_type = 1 → Index; referenced_size must drop the
        // top bit, not absorb it.
        let r0 = ref_bytes(1, 0x7FFF_FFFF, 100, 0, 0, 0);
        let p = build_sidx_v0(1, 1000, 0, 0, &[r0]);
        let sidx = parse_sidx(&p).unwrap();
        assert_eq!(sidx.references[0].reference_type, ReferenceType::Index);
        assert_eq!(sidx.references[0].referenced_size, 0x7FFF_FFFF);
    }

    #[test]
    fn max_width_bitfields_round_trip() {
        // Exercise the full widths: 31-bit size, 28-bit delta, 3-bit
        // SAP type, both single-bit flags set.
        let r0 = ref_bytes(1, 0x7FFF_FFFF, 0xFFFF_FFFF, 1, 6, 0x0FFF_FFFF);
        let p = build_sidx_v0(7, 600, 12, 34, &[r0]);
        let sidx = parse_sidx(&p).unwrap();
        let r = sidx.references[0];
        assert_eq!(r.reference_type, ReferenceType::Index);
        assert_eq!(r.referenced_size, 0x7FFF_FFFF);
        assert_eq!(r.subsegment_duration, 0xFFFF_FFFF);
        assert!(r.starts_with_sap);
        assert_eq!(r.sap_type, 6);
        assert_eq!(r.sap_delta_time, 0x0FFF_FFFF);
    }

    #[test]
    fn empty_reference_list_is_legal() {
        // reference_count = 0 is structurally valid (an empty index).
        let p = build_sidx_v0(1, 1000, 0, 0, &[]);
        let sidx = parse_sidx(&p).unwrap();
        assert!(sidx.references.is_empty());
    }

    #[test]
    fn unknown_version_rejected() {
        let r0 = ref_bytes(0, 1, 1, 0, 0, 0);
        let mut p = build_sidx_v0(1, 1000, 0, 0, &[r0]);
        p[0] = 2; // bogus version
        assert!(parse_sidx(&p).is_err());
    }

    #[test]
    fn truncated_fixed_header_rejected() {
        // A v1 box truncated mid fixed-header (only the v0-sized header
        // present) must reject because v1 needs the wider time fields.
        let p = vec![1u8, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0x01, 0x5f]; // 12 bytes
        assert!(parse_sidx(&p).is_err());
    }

    #[test]
    fn reference_count_overrun_rejected() {
        // Declare 3 references but supply bytes for only 2.
        let r0 = ref_bytes(0, 1, 1, 0, 0, 0);
        let r1 = ref_bytes(0, 2, 2, 0, 0, 0);
        let mut p = build_sidx_v0(1, 1000, 0, 0, &[r0, r1]);
        // Patch the reference_count field (last 2 bytes of the fixed
        // header, at offset 18..20 for v0) up to 3.
        let count_off = 4 + 4 + 4 + 4 + 4 + 2; // ver/flags + id + ts + ept + fo + reserved
        p[count_off] = 0;
        p[count_off + 1] = 3;
        assert!(parse_sidx(&p).is_err());
    }

    #[test]
    fn partial_trailing_reference_rejected() {
        // One full reference + 6 stray bytes (half a reference).
        let r0 = ref_bytes(0, 1, 1, 0, 0, 0);
        let mut p = build_sidx_v0(1, 1000, 0, 0, &[r0]);
        p.extend_from_slice(&[0u8; 6]);
        assert!(parse_sidx(&p).is_err());
    }

    #[test]
    fn material_start_adds_first_offset_to_anchor() {
        let p = build_sidx_v0(1, 1000, 0, 512, &[ref_bytes(0, 100, 10, 1, 1, 0)]);
        let sidx = parse_sidx(&p).unwrap();
        // Anchor (first byte after the box) = 1000; material starts at
        // 1000 + first_offset(512) = 1512.
        assert_eq!(sidx.material_start(1000), Some(1512));
    }

    #[test]
    fn subsegment_offsets_walk_referenced_size_chain() {
        let refs = [
            ref_bytes(0, 100, 10, 1, 1, 0),
            ref_bytes(0, 250, 10, 1, 1, 0),
            ref_bytes(0, 70, 10, 1, 1, 0),
        ];
        let p = build_sidx_v0(1, 1000, 0, 0, &refs);
        let sidx = parse_sidx(&p).unwrap();
        // anchor = 2000, first_offset = 0 → material starts at 2000.
        assert_eq!(sidx.subsegment_offset(2000, 0), Some(2000));
        assert_eq!(sidx.subsegment_offset(2000, 1), Some(2100));
        assert_eq!(sidx.subsegment_offset(2000, 2), Some(2350));
        assert_eq!(sidx.subsegment_offset(2000, 3), None); // out of range
    }

    #[test]
    fn subsegment_start_times_accumulate_durations() {
        let refs = [
            ref_bytes(0, 100, 30_000, 1, 1, 0),
            ref_bytes(0, 100, 30_000, 1, 1, 0),
            ref_bytes(0, 100, 15_000, 1, 1, 0),
        ];
        let p = build_sidx_v0(1, 90_000, 9_000, 0, &refs);
        let sidx = parse_sidx(&p).unwrap();
        // earliest_presentation_time = 9_000.
        assert_eq!(sidx.subsegment_start_time(0), Some(9_000));
        assert_eq!(sidx.subsegment_start_time(1), Some(39_000));
        assert_eq!(sidx.subsegment_start_time(2), Some(69_000));
        assert_eq!(sidx.subsegment_start_time(3), None);
    }
}
