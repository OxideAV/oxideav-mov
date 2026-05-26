//! Preview atom (`pnot`).
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01) pp. 26
//! – 27 / Figure 1-7. The preview atom is a *preflight* hint: it points
//! at one of the file's other atoms — typically a `PICT` image stored
//! after `moov` — and declares "this is the representative poster image
//! for the movie." A Finder / Open dialog can render the preview without
//! decoding any media samples and without instantiating the codec
//! pipeline; the atom is, in effect, the QuickTime equivalent of a
//! thumbnail-in-container hint.
//!
//! Layout per QTFF Figure 1-7:
//!
//! ```text
//! aligned(8) class PreviewAtom {
//!     unsigned int(32) size;             // standard atom header
//!     unsigned int(32) type = 'pnot';
//!     unsigned int(32) modification_date; // Mac-classic seconds since 1904-01-01
//!     unsigned int(16) version_number;    // spec fixes at 0
//!     unsigned int(32) atom_type;         // FourCC of the previewed atom (typically 'PICT')
//!     unsigned int(16) atom_index;        // 1-based index into that atom type's instances
//! }
//! ```
//!
//! The atom appears at file scope, not inside `moov`. QTFF p. 26
//! describes it as one of the optional top-level structures alongside
//! `mdat` and `moov`.
//!
//! ISO BMFF (ISO/IEC 14496-12) does not define `pnot`; it is QuickTime-
//! only and stays absent for plain MP4 / fMP4 / HEIF / AVIF inputs.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Difference in seconds between the Mac-classic epoch
/// (1904-01-01T00:00:00Z) and the Unix epoch (1970-01-01T00:00:00Z) —
/// 66 years including 17 leap days (1904, 08, 12, …, 1968 = 17 leap
/// years in the interval). The `modification_date` word in [`Pnot`] is
/// keyed to the Mac epoch (matching `mvhd`'s creation/modification
/// fields per QTFF p. 32); converting to Unix subtracts this constant.
pub const MAC_TO_UNIX_EPOCH_SECONDS: u64 = 2_082_844_800;

/// On-disk byte length of a `pnot` body — `modification_date` (4) +
/// `version_number` (2) + `atom_type` (4) + `atom_index` (2). Used as
/// both the minimum and maximum: `pnot` has no list and no variable
/// section, so any deviation is a writer error.
pub const PNOT_BODY_LEN: usize = 12;

/// Parsed Preview atom (QTFF p. 26).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pnot {
    /// Wall-clock instant at which the preview was last updated, in
    /// Mac-classic format (seconds since 1904-01-01T00:00:00Z, the
    /// same epoch QTFF's `mvhd` uses for creation / modification
    /// dates per p. 32). 32-bit unsigned, which wraps in February
    /// 2040 — a writer that needs to encode a date past the wrap
    /// has no spec-defined recourse, so we surface the raw on-disk
    /// value verbatim and let callers decide what to do.
    pub modification_date: u32,
    /// Spec fixes this at 0 (QTFF p. 26). Preserved verbatim so a
    /// round-trip writer can re-emit the on-disk value bit-for-bit;
    /// an unknown version still parses but [`Pnot::is_known_version`]
    /// returns `false` so a strict consumer can reject.
    pub version_number: u16,
    /// FourCC of the atom the preview points at. QTFF p. 26 names
    /// `PICT` as the typical value (a QuickDraw picture stored as a
    /// separate top-level atom); a writer is free to point at any
    /// FourCC. The parser does not validate the target — it carries
    /// the bytes through to the caller for opaque interpretation.
    pub atom_type: [u8; 4],
    /// 1-based index into the file's atoms of type `atom_type`. QTFF
    /// p. 27 notes the typical value is 1 (the first such atom is the
    /// preview). 0 is reserved / unused — every well-formed `pnot`
    /// uses a 1-based index — but the parser preserves whatever the
    /// writer supplied; [`Pnot::is_valid_index`] flags the
    /// out-of-band 0 for a strict consumer.
    pub atom_index: u16,
}

impl Pnot {
    /// `true` when the on-disk `version_number` matches the
    /// spec-fixed value (0). QTFF p. 26 mandates 0; preserving the
    /// raw word lets the parser stay accepting (a writer that sets
    /// 1 doesn't make the rest of the record unreadable) while
    /// giving strict consumers a one-line "is this conformant"
    /// check.
    pub fn is_known_version(&self) -> bool {
        self.version_number == 0
    }

    /// `true` when `atom_index` is at least 1. QTFF p. 27 documents
    /// the field as 1-based; an on-disk value of 0 has no defined
    /// meaning. Surfaces as a predicate rather than rejected at
    /// parse time so out-of-band writers stay round-trippable.
    pub fn is_valid_index(&self) -> bool {
        self.atom_index >= 1
    }

    /// Convert `modification_date` (Mac-classic seconds since
    /// 1904-01-01T00:00:00Z) to a Unix-epoch second count
    /// (1970-01-01T00:00:00Z), or `None` when the Mac timestamp
    /// is earlier than the Unix epoch (any value strictly less
    /// than [`MAC_TO_UNIX_EPOCH_SECONDS`] is pre-1970 and
    /// unrepresentable as an unsigned Unix instant).
    pub fn unix_seconds(&self) -> Option<u64> {
        (self.modification_date as u64).checked_sub(MAC_TO_UNIX_EPOCH_SECONDS)
    }
}

/// Parse a `pnot` payload.
///
/// Layout per QTFF p. 26 — see the module-level docs.
///
/// Returns `Error::invalid` when the payload's length is not exactly
/// [`PNOT_BODY_LEN`] (12 bytes). `pnot` has no list and no variable
/// section per QTFF Figure 1-7; a body shorter than 12 bytes is a
/// truncation that must reject (else `atom_type` / `atom_index`
/// silently default to zeros), and a body longer than 12 bytes is a
/// writer error or a non-standard extension we can't decode.
///
/// The `version_number` is *not* validated at parse time — the spec
/// fixes it at 0 but a writer that sets a stray value shouldn't fail
/// the box's other useful fields; [`Pnot::is_known_version`] surfaces
/// the conformance check to callers that care.
pub fn parse_pnot(payload: &[u8]) -> Result<Pnot> {
    if payload.len() != PNOT_BODY_LEN {
        return Err(Error::invalid(format!(
            "MOV: pnot body {} != expected {PNOT_BODY_LEN} bytes (QTFF p. 26)",
            payload.len()
        )));
    }

    let modification_date = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let version_number = u16::from_be_bytes([payload[4], payload[5]]);
    let atom_type = [payload[6], payload[7], payload[8], payload[9]];
    let atom_index = u16::from_be_bytes([payload[10], payload[11]]);

    Ok(Pnot {
        modification_date,
        version_number,
        atom_type,
        atom_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `pnot` payload exactly matching QTFF Figure 1-7.
    fn build_pnot(
        modification_date: u32,
        version_number: u16,
        atom_type: [u8; 4],
        atom_index: u16,
    ) -> Vec<u8> {
        let mut p = Vec::with_capacity(PNOT_BODY_LEN);
        p.extend_from_slice(&modification_date.to_be_bytes());
        p.extend_from_slice(&version_number.to_be_bytes());
        p.extend_from_slice(&atom_type);
        p.extend_from_slice(&atom_index.to_be_bytes());
        p
    }

    #[test]
    fn round_trip_fields() {
        // Typical `pnot`: modification date = Unix epoch + 0 (Mac
        // seconds 2_082_844_800), spec-fixed version 0, points at the
        // first PICT atom in the file.
        let p = build_pnot(2_082_844_800, 0, *b"PICT", 1);
        let pnot = parse_pnot(&p).unwrap();
        assert_eq!(pnot.modification_date, 2_082_844_800);
        assert_eq!(pnot.version_number, 0);
        assert_eq!(pnot.atom_type, *b"PICT");
        assert_eq!(pnot.atom_index, 1);
        assert!(pnot.is_known_version());
        assert!(pnot.is_valid_index());
        assert_eq!(pnot.unix_seconds(), Some(0));
    }

    #[test]
    fn unix_seconds_pre_1970_returns_none() {
        // Mac seconds = 0 means 1904-01-01, which has no unsigned
        // Unix-epoch representation. Must return `None`.
        let p = build_pnot(0, 0, *b"PICT", 1);
        let pnot = parse_pnot(&p).unwrap();
        assert_eq!(pnot.unix_seconds(), None);
    }

    #[test]
    fn unix_seconds_known_anchor() {
        // 2024-01-01T00:00:00Z = Unix 1_704_067_200 →
        //                       Mac 1_704_067_200 + 2_082_844_800
        //                         = 3_786_912_000.
        let p = build_pnot(3_786_912_000, 0, *b"PICT", 1);
        let pnot = parse_pnot(&p).unwrap();
        assert_eq!(pnot.unix_seconds(), Some(1_704_067_200));
    }

    #[test]
    fn unknown_version_parses_but_predicate_false() {
        // Writer sets a stray version word — the box's other fields
        // are still meaningful, so the parse succeeds and a strict
        // consumer can refuse via the predicate.
        let p = build_pnot(2_082_844_800, 1, *b"PICT", 1);
        let pnot = parse_pnot(&p).unwrap();
        assert!(!pnot.is_known_version());
        assert_eq!(pnot.atom_type, *b"PICT");
    }

    #[test]
    fn zero_index_parses_but_predicate_false() {
        // QTFF p. 27 says `atom_index` is 1-based. 0 has no defined
        // meaning; we preserve the byte and surface the conformance
        // signal through the predicate.
        let p = build_pnot(2_082_844_800, 0, *b"PICT", 0);
        let pnot = parse_pnot(&p).unwrap();
        assert!(!pnot.is_valid_index());
        assert_eq!(pnot.atom_index, 0);
    }

    #[test]
    fn non_pict_atom_type_round_trips() {
        // QTFF only documents `PICT` as the typical target, but the
        // spec does not constrain the FourCC. A writer may stash any
        // top-level atom-type here — round-trip the bytes verbatim.
        let p = build_pnot(2_082_844_800, 0, *b"jpeg", 2);
        let pnot = parse_pnot(&p).unwrap();
        assert_eq!(pnot.atom_type, *b"jpeg");
        assert_eq!(pnot.atom_index, 2);
    }

    #[test]
    fn truncated_body_rejects() {
        // 11 bytes — one short of the 12-byte fixed record.
        let p = vec![0u8; PNOT_BODY_LEN - 1];
        assert!(parse_pnot(&p).is_err());
    }

    #[test]
    fn empty_body_rejects() {
        let p: Vec<u8> = Vec::new();
        assert!(parse_pnot(&p).is_err());
    }

    #[test]
    fn trailing_bytes_reject() {
        // 13 bytes — one stray byte past the spec-defined record.
        // `pnot` carries no list, so any tail is malformed.
        let mut p = build_pnot(2_082_844_800, 0, *b"PICT", 1);
        p.push(0xAA);
        assert!(parse_pnot(&p).is_err());
    }

    #[test]
    fn high_bit_atom_index_round_trips() {
        // 16-bit unsigned `atom_index` — confirm values past 0x7FFF
        // round-trip without sign-extension confusion.
        let p = build_pnot(2_082_844_800, 0, *b"PICT", 0xFFFF);
        let pnot = parse_pnot(&p).unwrap();
        assert_eq!(pnot.atom_index, 0xFFFF);
        assert!(pnot.is_valid_index());
    }
}
