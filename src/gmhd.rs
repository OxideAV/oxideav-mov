//! Per-MediaType `gmhd` (base-media information header) extensions.
//!
//! QTFF §"Base Media Information Header Atom" (p. 64). `gmhd` wraps a
//! generic media's information header alongside per-MediaType extension
//! atoms. The three commonly-encountered shapes are:
//!
//! * `gmin` — Generic Media Information Header. 16-byte payload after
//!   the 4-byte FullBox prefix:
//!   `[graphics_mode:u16][opcolor:3 × u16][balance:i16][reserved:u16]`.
//! * `text` — Text-track media-information header. 36-byte payload
//!   (after the FullBox prefix) carrying a 9-element 16.16 fixed-point
//!   transformation matrix the renderer uses to position text samples.
//! * `tmcd` — Time-code track media-information header. Inside `gmhd`
//!   it is itself a container that wraps a `tcmi` (time-code media-
//!   information) atom — `[ver+flags=4][text_font:u16][text_face:u16]
//!   [text_size:u16][reserved:u16][bg_color:3 × u16][fg_color:3 × u16]
//!   [counted-pascal-string font name]`.
//!
//! Round 5 surfaces the parsed [`Gmin`] / [`TextHeader`] / [`Tcmi`]
//! types and bundles them into a [`Gmhd`] container that gets stored
//! on each `Track` whose `minf` has a `gmhd` child.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Generic media-information header (`gmhd/gmin`). QTFF p. 65.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gmin {
    /// Compositing mode (e.g. `0x0040` = ditherCopy, `0x0100` = transparent).
    pub graphics_mode: u16,
    /// RGB triple (16-bit per channel) used by certain compositing modes.
    pub opcolor: [u16; 3],
    /// Stereo balance (8.8 signed fixed-point). 0 = centered.
    pub balance: i16,
}

/// Text-track media-information header (`gmhd/text`). QTFF p. 144.
///
/// The 36-byte payload is a 9-element 16.16 fixed-point matrix (last
/// column is 2.30, identical convention to `tkhd.matrix`) that maps
/// each text sample's local coordinates onto the movie canvas. We
/// surface the raw integer values; callers can apply the same
/// classification logic as `tkhd.rotation()` if they need oriented
/// text overlays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextHeader {
    /// Raw 9-entry matrix (3×3 row-major, mirrors `Tkhd::matrix`).
    pub matrix: [i32; 9],
}

/// Time-code track media-information atom (`gmhd/tmcd/tcmi`). QTFF
/// "Time Code Sample Description" §3.10 (p. 116).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tcmi {
    /// Native QuickTime font index (2 = Geneva, 3 = Helvetica, …).
    pub text_font: u16,
    /// Bold/italic/underline bit field (bit 0 = bold, bit 1 = italic, …).
    pub text_face: u16,
    /// Font size in points.
    pub text_size: u16,
    /// 16-bit-per-channel RGB triple for the text-overlay background.
    pub bg_color: [u16; 3],
    /// 16-bit-per-channel RGB triple for the text-overlay foreground.
    pub fg_color: [u16; 3],
    /// Pascal-style counted font name (UTF-8 lossy decode). Empty when
    /// the producer omitted it.
    pub font_name: String,
}

/// Aggregated `gmhd` payload. Exactly one of `gmin` / `text` / `tmcd`
/// usually populates a track; round 5 keeps all three slots so a
/// caller can introspect future combined-shape `gmhd` atoms without
/// re-walking the tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gmhd {
    pub gmin: Option<Gmin>,
    pub text: Option<TextHeader>,
    pub tcmi: Option<Tcmi>,
}

/// Parse a `gmhd/gmin` payload. The leading `[ver+flags=4]` is the
/// FullBox prefix; the 12 bytes that follow carry the actual fields.
pub fn parse_gmin(payload: &[u8]) -> Result<Gmin> {
    if payload.len() < 16 {
        return Err(Error::invalid("MOV: gmin payload < 16 bytes"));
    }
    Ok(Gmin {
        graphics_mode: u16::from_be_bytes([payload[4], payload[5]]),
        opcolor: [
            u16::from_be_bytes([payload[6], payload[7]]),
            u16::from_be_bytes([payload[8], payload[9]]),
            u16::from_be_bytes([payload[10], payload[11]]),
        ],
        balance: i16::from_be_bytes([payload[12], payload[13]]),
        // payload[14..16] is the trailing 2-byte reserved.
    })
}

/// Parse a `gmhd/text` payload. 36 bytes of 9-element matrix data, no
/// FullBox prefix (the `text` atom inside `gmhd` is a plain container,
/// not a FullBox — different from a top-level FullBox `tkhd`).
pub fn parse_text_header(payload: &[u8]) -> Result<TextHeader> {
    if payload.len() < 36 {
        return Err(Error::invalid("MOV: gmhd/text payload < 36 bytes"));
    }
    let mut matrix = [0i32; 9];
    for (i, slot) in matrix.iter_mut().enumerate() {
        let off = i * 4;
        *slot = i32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
    }
    Ok(TextHeader { matrix })
}

/// Parse a `gmhd/tmcd/tcmi` payload.
///
/// `tcmi` is the only mandatory child of the `tmcd` container inside
/// `gmhd`. Layout: `[ver+flags=4][text_font:u16][text_face:u16]
/// [text_size:u16][reserved:u16][bg_color:3×u16][fg_color:3×u16]
/// [counted-pascal-string font_name]`.
pub fn parse_tcmi(payload: &[u8]) -> Result<Tcmi> {
    // 4 (FullBox) + 8 (font/face/size/reserved) + 6 (bg) + 6 (fg) = 24.
    if payload.len() < 24 {
        return Err(Error::invalid("MOV: tcmi payload < 24 bytes"));
    }
    let text_font = u16::from_be_bytes([payload[4], payload[5]]);
    let text_face = u16::from_be_bytes([payload[6], payload[7]]);
    let text_size = u16::from_be_bytes([payload[8], payload[9]]);
    // payload[10..12] is reserved.
    let bg_color = [
        u16::from_be_bytes([payload[12], payload[13]]),
        u16::from_be_bytes([payload[14], payload[15]]),
        u16::from_be_bytes([payload[16], payload[17]]),
    ];
    let fg_color = [
        u16::from_be_bytes([payload[18], payload[19]]),
        u16::from_be_bytes([payload[20], payload[21]]),
        u16::from_be_bytes([payload[22], payload[23]]),
    ];
    // Pascal-style counted string starts at offset 24.
    let mut font_name = String::new();
    if payload.len() > 24 {
        let n = payload[24] as usize;
        let start: usize = 25;
        let end = start.saturating_add(n).min(payload.len());
        if end > start {
            // Apple specs Mac-Roman; ASCII-only fonts survive. Anything
            // ≥ 0x80 collapses to U+FFFD via the same lossy expansion
            // we use in user_data.
            let raw = &payload[start..end];
            if let Ok(s) = std::str::from_utf8(raw) {
                font_name = s.to_string();
            } else {
                let mut s = String::with_capacity(raw.len());
                for &c in raw {
                    if c < 0x80 {
                        s.push(c as char);
                    } else {
                        s.push('\u{FFFD}');
                    }
                }
                font_name = s;
            }
        }
    }
    Ok(Tcmi {
        text_font,
        text_face,
        text_size,
        bg_color,
        fg_color,
        font_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmin_round_trip() {
        // ver+flags + graphics_mode=0x40 + opcolor (R,G,B = 0xFFFF, 0, 0)
        // + balance = 0 + reserved.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0x0040u16.to_be_bytes());
        p.extend_from_slice(&0xFFFFu16.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes());
        p.extend_from_slice(&0i16.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes());
        let g = parse_gmin(&p).unwrap();
        assert_eq!(g.graphics_mode, 0x0040);
        assert_eq!(g.opcolor, [0xFFFF, 0, 0]);
        assert_eq!(g.balance, 0);
    }

    #[test]
    fn gmin_too_short_errors() {
        assert!(parse_gmin(&[0u8; 8]).is_err());
    }

    #[test]
    fn text_header_matrix_round_trip() {
        // Identity matrix.
        let mut p = vec![0u8; 36];
        let one: i32 = 0x0001_0000;
        let w: i32 = 0x4000_0000;
        p[0..4].copy_from_slice(&one.to_be_bytes());
        p[16..20].copy_from_slice(&one.to_be_bytes());
        p[32..36].copy_from_slice(&w.to_be_bytes());
        let t = parse_text_header(&p).unwrap();
        assert_eq!(t.matrix[0], one);
        assert_eq!(t.matrix[4], one);
        assert_eq!(t.matrix[8], w);
    }

    #[test]
    fn tcmi_round_trip_with_font_name() {
        // ver+flags + font=2 + face=1 (bold) + size=12 + reserved
        // + bg = (0,0,0) + fg = (0xFFFF,0xFFFF,0xFFFF) + "Geneva"
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&2u16.to_be_bytes()); // font
        p.extend_from_slice(&1u16.to_be_bytes()); // face (bold)
        p.extend_from_slice(&12u16.to_be_bytes()); // size
        p.extend_from_slice(&0u16.to_be_bytes()); // reserved
        for c in [0u16, 0, 0] {
            p.extend_from_slice(&c.to_be_bytes());
        }
        for c in [0xFFFFu16, 0xFFFF, 0xFFFF] {
            p.extend_from_slice(&c.to_be_bytes());
        }
        // Pascal counted string "Geneva".
        let name = b"Geneva";
        p.push(name.len() as u8);
        p.extend_from_slice(name);
        let t = parse_tcmi(&p).unwrap();
        assert_eq!(t.text_font, 2);
        assert_eq!(t.text_face, 1);
        assert_eq!(t.text_size, 12);
        assert_eq!(t.bg_color, [0, 0, 0]);
        assert_eq!(t.fg_color, [0xFFFF, 0xFFFF, 0xFFFF]);
        assert_eq!(t.font_name, "Geneva");
    }

    #[test]
    fn tcmi_without_font_name_ok() {
        let mut p = vec![0u8; 24];
        // text_size=10
        p[8..10].copy_from_slice(&10u16.to_be_bytes());
        let t = parse_tcmi(&p).unwrap();
        assert_eq!(t.text_size, 10);
        assert_eq!(t.font_name, "");
    }
}
