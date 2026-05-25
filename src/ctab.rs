//! Color Table Atom (`ctab`).
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01),
//! §"Color Table Atoms" (p. 35; spec figure 2-4 on the same page). The
//! color table atom is an optional movie-level atom that lists a
//! preferred 4-channel (reserved/red/green/blue) palette of up to 256
//! 16-bit colors. QuickTime players use the palette when targeting a
//! display that supports only an indexed-color framebuffer, so the same
//! movie can declare authoring-time color intent for low-bit-depth
//! output. The atom is a leaf atom and its immediate parent is the
//! movie atom (`moov`).
//!
//! ## On-disk layout (QTFF p. 35)
//!
//! ```text
//! Bytes                   Field
//! 4                       Atom size
//! 4                       Type = 'ctab'
//! 4                       Color table seed              (must be 0)
//! 2                       Color table flags             (must be 0x8000)
//! 2                       Color table size              (zero-relative)
//! n × 8                   Color array, n = size + 1
//!     2                       reserved (0)
//!     2                       red
//!     2                       green
//!     2                       blue
//! ```
//!
//! The size field is **zero-relative** (QTFF p. 35: "this is a
//! zero-relative value; setting this field to 0 means that there is one
//! color in the array"). The on-wire word value `n - 1` therefore
//! encodes `n` entries — the parser widens the count to a `u32`
//! ([`Ctab::color_count`]) so callers don't have to remember to add one.
//!
//! Each color is four big-endian `u16`s; QTFF p. 36 fixes the first as
//! "must be set to 0" (the reserved Macintosh `ColorSpec.value` field
//! that on disk pairs with an `RGBColor` triple) and the remaining
//! three as the red / green / blue channels respectively.
//!
//! ## Validation
//!
//! The parser rejects, at open time:
//!
//! * a payload shorter than the fixed 8-byte header (seed + flags +
//!   size);
//! * a non-zero `color_table_seed` (QTFF p. 35: "must be set to 0");
//! * a `color_table_flags` value other than `0x8000` (QTFF p. 35: "must
//!   be set to 0x8000");
//! * a payload length that disagrees with the declared count — the body
//!   *after* the 8-byte fixed header must be exactly `8 × (size + 1)`
//!   bytes (no padding, no trailing data; the color array runs to
//!   end-of-atom per the spec figure).
//!
//! Per-entry `reserved` words that are non-zero are surfaced
//! ([`ColorTableEntry::reserved`]) rather than rejected — real Mac
//! Toolbox `ColorSpec` structures sometimes carry an authoring
//! palette-index in the same word, and dropping the bit would lose
//! that information.
//!
//! ## ISO BMFF
//!
//! ISO/IEC 14496-12 does not define `ctab`. It is a QuickTime-only
//! atom; an MP4 / fMP4 / HEIF / AVIF file will not carry one and the
//! demuxer's [`crate::MovDemuxer::ctab`] field stays `None`.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// One entry in a [`Ctab`] color array. Each on-disk slot is four
/// big-endian `u16`s; QTFF p. 36 reserves the first as 0 and assigns
/// the next three to red / green / blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorTableEntry {
    /// On-disk `reserved` word (QTFF p. 36: "must be set to 0"). Surfaced
    /// verbatim rather than discarded — some authoring tools stash a
    /// Mac Toolbox `ColorSpec.value` index here even though the spec
    /// fixes it at zero.
    pub reserved: u16,
    /// Red channel, 0..=65535 (Mac Toolbox `RGBColor.red`).
    pub red: u16,
    /// Green channel, 0..=65535 (Mac Toolbox `RGBColor.green`).
    pub green: u16,
    /// Blue channel, 0..=65535 (Mac Toolbox `RGBColor.blue`).
    pub blue: u16,
}

impl ColorTableEntry {
    /// Convert the entry to an 8-bit-per-channel RGB triple by
    /// right-shifting each 16-bit channel by 8 (the high byte of each
    /// 16-bit channel). QTFF stores the same 8-bit Mac Toolbox values
    /// duplicated into both bytes of each 16-bit field, so the high
    /// byte is the channel value and the low byte is a copy. Callers
    /// that need full 16-bit fidelity should read [`red`](Self::red) /
    /// [`green`](Self::green) / [`blue`](Self::blue) directly.
    pub fn rgb8(&self) -> [u8; 3] {
        [
            (self.red >> 8) as u8,
            (self.green >> 8) as u8,
            (self.blue >> 8) as u8,
        ]
    }
}

/// Parsed Color Table Atom (QTFF p. 35).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ctab {
    /// `color_table_seed` (QTFF p. 35: "must be set to 0"). Preserved on
    /// the typed struct as `0` after validation so the field set
    /// matches the on-disk layout.
    pub seed: u32,
    /// `color_table_flags` (QTFF p. 35: "must be set to 0x8000").
    /// Preserved as `0x8000` after validation.
    pub flags: u16,
    /// The color array. Length is `color_table_size + 1` (the
    /// zero-relative count from disk; QTFF p. 35). Capped at 256 by the
    /// 16-bit on-wire field (`size = 0xFFFF` ↔ 65 536 entries — the
    /// spec only documents the 256-color use case, but the parser
    /// accepts the full 16-bit range so a future writer that emits a
    /// larger palette still round-trips).
    pub entries: Vec<ColorTableEntry>,
}

impl Ctab {
    /// Number of entries in the color array. Same as
    /// `self.entries.len() as u32`, surfaced as a typed accessor so
    /// callers don't have to cast.
    pub fn color_count(&self) -> u32 {
        self.entries.len() as u32
    }
}

/// Parse a `ctab` body (the bytes between the atom header and its
/// trailing edge — the caller has already consumed the 8-byte `[size,
/// type]` header).
///
/// `payload.len()` must be at least 8 (the fixed seed / flags / size
/// header) and exactly `8 + 8 × (size + 1)` after that header is
/// decoded (no padding, no trailing bytes — the color array runs to
/// end-of-atom).
pub fn parse_ctab(payload: &[u8]) -> Result<Ctab> {
    if payload.len() < 8 {
        return Err(Error::invalid(format!(
            "MOV: ctab payload too short ({} bytes; need at least 8 for seed/flags/size header)",
            payload.len()
        )));
    }
    let seed = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    if seed != 0 {
        return Err(Error::invalid(format!(
            "MOV: ctab color_table_seed must be 0 (QTFF p. 35); got 0x{seed:08x}"
        )));
    }
    let flags = u16::from_be_bytes([payload[4], payload[5]]);
    if flags != 0x8000 {
        return Err(Error::invalid(format!(
            "MOV: ctab color_table_flags must be 0x8000 (QTFF p. 35); got 0x{flags:04x}"
        )));
    }
    let size_raw = u16::from_be_bytes([payload[6], payload[7]]);
    // QTFF p. 35: zero-relative — on-disk N ↔ N+1 entries.
    let count = size_raw as u32 + 1;
    let body = &payload[8..];
    let expected = (count as usize) * 8;
    if body.len() != expected {
        return Err(Error::invalid(format!(
            "MOV: ctab body length {} mismatches declared count {} (expected {} bytes, \
             size = {} → {} entries)",
            body.len(),
            count,
            expected,
            size_raw,
            count
        )));
    }
    let mut entries = Vec::with_capacity(count as usize);
    for i in 0..(count as usize) {
        let base = i * 8;
        let reserved = u16::from_be_bytes([body[base], body[base + 1]]);
        let red = u16::from_be_bytes([body[base + 2], body[base + 3]]);
        let green = u16::from_be_bytes([body[base + 4], body[base + 5]]);
        let blue = u16::from_be_bytes([body[base + 6], body[base + 7]]);
        entries.push(ColorTableEntry {
            reserved,
            red,
            green,
            blue,
        });
    }
    Ok(Ctab {
        seed,
        flags,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `ctab` *body* (post-atom-header) with the spec-required
    /// seed/flags and the caller's entries.
    fn build_ctab_body(entries: &[(u16, u16, u16, u16)]) -> Vec<u8> {
        assert!(!entries.is_empty(), "ctab body needs at least 1 entry");
        let size_raw = (entries.len() - 1) as u16;
        let mut p = Vec::with_capacity(8 + 8 * entries.len());
        p.extend_from_slice(&0u32.to_be_bytes()); // seed
        p.extend_from_slice(&0x8000u16.to_be_bytes()); // flags
        p.extend_from_slice(&size_raw.to_be_bytes()); // size (zero-relative)
        for (reserved, r, g, b) in entries {
            p.extend_from_slice(&reserved.to_be_bytes());
            p.extend_from_slice(&r.to_be_bytes());
            p.extend_from_slice(&g.to_be_bytes());
            p.extend_from_slice(&b.to_be_bytes());
        }
        p
    }

    #[test]
    fn single_entry_zero_relative() {
        // size_raw = 0 → 1 entry. White, opaque.
        let body = build_ctab_body(&[(0, 0xFFFF, 0xFFFF, 0xFFFF)]);
        let ctab = parse_ctab(&body).unwrap();
        assert_eq!(ctab.seed, 0);
        assert_eq!(ctab.flags, 0x8000);
        assert_eq!(ctab.color_count(), 1);
        assert_eq!(ctab.entries.len(), 1);
        assert_eq!(ctab.entries[0].reserved, 0);
        assert_eq!(ctab.entries[0].red, 0xFFFF);
        assert_eq!(ctab.entries[0].green, 0xFFFF);
        assert_eq!(ctab.entries[0].blue, 0xFFFF);
        assert_eq!(ctab.entries[0].rgb8(), [0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn three_entries_rgb_primaries() {
        // size_raw = 2 on disk → 3 entries.
        let body = build_ctab_body(&[
            (0, 0xFFFF, 0x0000, 0x0000), // red
            (0, 0x0000, 0xFFFF, 0x0000), // green
            (0, 0x0000, 0x0000, 0xFFFF), // blue
        ]);
        // Body shape: 4 (seed) + 2 (flags) + 2 (size) + 3 × 8 = 32 bytes.
        assert_eq!(body.len(), 32);
        let ctab = parse_ctab(&body).unwrap();
        assert_eq!(ctab.color_count(), 3);
        assert_eq!(ctab.entries[0].rgb8(), [0xFF, 0x00, 0x00]);
        assert_eq!(ctab.entries[1].rgb8(), [0x00, 0xFF, 0x00]);
        assert_eq!(ctab.entries[2].rgb8(), [0x00, 0x00, 0xFF]);
    }

    #[test]
    fn full_256_entry_palette_round_trips() {
        // size_raw = 255 → 256 entries. Classic Mac 256-color palette
        // shape.
        let mut entries = Vec::with_capacity(256);
        for i in 0..256u16 {
            entries.push((
                0,
                i << 8,
                (255 - i as u8 as u16) << 8,
                ((i.wrapping_mul(7)) & 0xFF) << 8,
            ));
        }
        let body = build_ctab_body(&entries);
        // 8 (header) + 256 × 8 = 2056 bytes.
        assert_eq!(body.len(), 2056);
        let ctab = parse_ctab(&body).unwrap();
        assert_eq!(ctab.color_count(), 256);
        // Spot-check the boundary entries.
        assert_eq!(ctab.entries[0].red, 0);
        assert_eq!(ctab.entries[255].red, 0xFF00);
        assert_eq!(ctab.entries[128].red, 0x8000);
    }

    #[test]
    fn nonzero_reserved_word_is_preserved() {
        let mut body = build_ctab_body(&[(0x1234, 0x4567, 0x89AB, 0xCDEF)]);
        // Hand-edit the reserved word to confirm the parser surfaces
        // non-spec writers' stash bits rather than zeroing them.
        body[8] = 0x12;
        body[9] = 0x34;
        let ctab = parse_ctab(&body).unwrap();
        assert_eq!(ctab.entries[0].reserved, 0x1234);
        assert_eq!(ctab.entries[0].red, 0x4567);
    }

    #[test]
    fn rejects_short_payload() {
        let body = vec![0u8; 7];
        let err = parse_ctab(&body).unwrap_err();
        assert!(format!("{err}").contains("ctab payload too short"));
    }

    #[test]
    fn rejects_nonzero_seed() {
        let mut body = build_ctab_body(&[(0, 0, 0, 0)]);
        body[0] = 0xFF;
        let err = parse_ctab(&body).unwrap_err();
        assert!(format!("{err}").contains("color_table_seed must be 0"));
    }

    #[test]
    fn rejects_wrong_flags() {
        let mut body = build_ctab_body(&[(0, 0, 0, 0)]);
        // 0x8000 → 0x4000.
        body[4] = 0x40;
        let err = parse_ctab(&body).unwrap_err();
        assert!(format!("{err}").contains("color_table_flags must be 0x8000"));
    }

    #[test]
    fn rejects_truncated_color_array() {
        // size_raw = 2 declares 3 entries, but only 2 worth of body.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // seed
        p.extend_from_slice(&0x8000u16.to_be_bytes()); // flags
        p.extend_from_slice(&2u16.to_be_bytes()); // size → 3 entries
                                                  // Only 16 bytes for 2 entries.
        p.extend_from_slice(&[0u8; 16]);
        let err = parse_ctab(&p).unwrap_err();
        assert!(format!("{err}").contains("body length"));
    }

    #[test]
    fn rejects_trailing_bytes() {
        // size_raw = 0 → 1 entry (8 bytes) + 4 trailing bytes.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // seed
        p.extend_from_slice(&0x8000u16.to_be_bytes()); // flags
        p.extend_from_slice(&0u16.to_be_bytes()); // size → 1 entry
        p.extend_from_slice(&[0u8; 8]); // one entry
        p.extend_from_slice(&[0xAA; 4]); // junk
        let err = parse_ctab(&p).unwrap_err();
        assert!(format!("{err}").contains("body length"));
    }
}
