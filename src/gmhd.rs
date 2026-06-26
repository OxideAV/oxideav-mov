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
    /// Compositing mode (Table 4-2, QTFF p. 200). For a typed view use
    /// [`Gmin::graphics_mode_kind`]; common values include `0x0000`
    /// (copy), `0x0040` (dither copy), `0x0024` (transparent), and
    /// `0x0100` (straight alpha).
    pub graphics_mode: u16,
    /// RGB triple (16-bit per channel) used by the compositing modes
    /// whose Table 4-2 "Uses opcolor" column is set (blend, transparent,
    /// straight-alpha-blend).
    pub opcolor: [u16; 3],
    /// Stereo balance (8.8 signed fixed-point). 0 = centered. See
    /// [`Gmin::balance_as_f32`] for the [-1.0, +1.0] decoded value
    /// (QTFF p. 201).
    pub balance: i16,
}

/// Typed compositing mode (QTFF Table 4-2, p. 200). The numeric
/// representation in [`Gmin::graphics_mode`] is preserved verbatim;
/// this enum surfaces the named modes plus a fall-through that keeps
/// the raw 16-bit value for vendor or future-spec codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsMode {
    /// `0x0000` — Copy the source image over the destination.
    Copy,
    /// `0x0040` — Dither the image (if needed), otherwise do a copy.
    DitherCopy,
    /// `0x0020` — Blend source/destination per channel weighted by
    /// `opcolor`. Uses `opcolor`.
    Blend,
    /// `0x0024` — Replace the destination pixel with the source pixel
    /// when the source is not equal to `opcolor`. Uses `opcolor`.
    Transparent,
    /// `0x0100` — Blend source/destination weighted by the source
    /// alpha channel.
    StraightAlpha,
    /// `0x0101` — Same as straight alpha, but the source has been
    /// pre-multiplied with a white background.
    PremulWhiteAlpha,
    /// `0x0102` — Same as straight alpha, but the source has been
    /// pre-multiplied with a black background.
    PremulBlackAlpha,
    /// `0x0103` — (Tracks only) Composition: the track is drawn
    /// offscreen and composited onto the screen using dither copy.
    Composition,
    /// `0x0104` — Straight-alpha-blend: per-channel alpha is the
    /// combination of the source alpha and the matching channel in
    /// `opcolor`. Uses `opcolor`.
    StraightAlphaBlend,
    /// Any value not in Table 4-2 (vendor / future-spec). The raw
    /// 16-bit code is preserved.
    Other(u16),
}

impl GraphicsMode {
    /// Map a raw 16-bit compositing-mode code to its typed name per
    /// QTFF Table 4-2.
    pub fn from_raw(code: u16) -> Self {
        match code {
            0x0000 => Self::Copy,
            0x0040 => Self::DitherCopy,
            0x0020 => Self::Blend,
            0x0024 => Self::Transparent,
            0x0100 => Self::StraightAlpha,
            0x0101 => Self::PremulWhiteAlpha,
            0x0102 => Self::PremulBlackAlpha,
            0x0103 => Self::Composition,
            0x0104 => Self::StraightAlphaBlend,
            other => Self::Other(other),
        }
    }

    /// The on-disk 16-bit code for this mode. Round-trips with
    /// [`GraphicsMode::from_raw`].
    pub fn raw(&self) -> u16 {
        match *self {
            Self::Copy => 0x0000,
            Self::DitherCopy => 0x0040,
            Self::Blend => 0x0020,
            Self::Transparent => 0x0024,
            Self::StraightAlpha => 0x0100,
            Self::PremulWhiteAlpha => 0x0101,
            Self::PremulBlackAlpha => 0x0102,
            Self::Composition => 0x0103,
            Self::StraightAlphaBlend => 0x0104,
            Self::Other(raw) => raw,
        }
    }

    /// Whether this mode consults the accompanying `opcolor` field
    /// (Table 4-2, "Uses opcolor" column). `Other` is reported as
    /// `false` so a caller doesn't read meaning into an opcolor that
    /// the spec hasn't bound to the unknown code.
    pub fn uses_opcolor(&self) -> bool {
        matches!(
            self,
            Self::Blend | Self::Transparent | Self::StraightAlphaBlend
        )
    }
}

impl Gmin {
    /// Typed view of [`Gmin::graphics_mode`] per QTFF Table 4-2.
    pub fn graphics_mode_kind(&self) -> GraphicsMode {
        GraphicsMode::from_raw(self.graphics_mode)
    }

    /// Decode [`Gmin::balance`] from 8.8 signed fixed-point to the
    /// real-valued setting in the [-1.0, +1.0] range that QTFF p. 201
    /// defines: negative weights the left speaker, positive the right,
    /// zero is centered.
    pub fn balance_as_f32(&self) -> f32 {
        self.balance as f32 / 256.0
    }
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

impl Gmin {
    /// Serialise the 16-byte `gmin` body — `[ver+flags=4]
    /// [graphics_mode:u16][opcolor:3×u16][balance:i16][reserved:u16]`.
    /// Exact inverse of [`parse_gmin`].
    pub fn to_body_bytes(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(16);
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&self.graphics_mode.to_be_bytes());
        for c in self.opcolor {
            p.extend_from_slice(&c.to_be_bytes());
        }
        p.extend_from_slice(&self.balance.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes()); // reserved
        p
    }
}

impl Tcmi {
    /// Serialise the `tcmi` body — `[ver+flags=4][text_font:u16]
    /// [text_face:u16][text_size:u16][reserved:u16][bg_color:3×u16]
    /// [fg_color:3×u16][pascal font_name]`. Exact inverse of
    /// [`parse_tcmi`]. The font name is truncated to 255 bytes (the
    /// Pascal length is a single byte) and emitted as raw UTF-8.
    pub fn to_body_bytes(&self) -> Vec<u8> {
        let name = self.font_name.as_bytes();
        let n = name.len().min(255);
        let mut p = Vec::with_capacity(25 + n);
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&self.text_font.to_be_bytes());
        p.extend_from_slice(&self.text_face.to_be_bytes());
        p.extend_from_slice(&self.text_size.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes()); // reserved
        for c in self.bg_color {
            p.extend_from_slice(&c.to_be_bytes());
        }
        for c in self.fg_color {
            p.extend_from_slice(&c.to_be_bytes());
        }
        p.push(n as u8);
        p.extend_from_slice(&name[..n]);
        p
    }
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

    // ─────────────── GraphicsMode (Table 4-2) ───────────────

    #[test]
    fn graphics_mode_table_4_2_round_trip() {
        // Every named row of Table 4-2 round-trips through from_raw / raw.
        let pairs = [
            (0x0000u16, GraphicsMode::Copy),
            (0x0040, GraphicsMode::DitherCopy),
            (0x0020, GraphicsMode::Blend),
            (0x0024, GraphicsMode::Transparent),
            (0x0100, GraphicsMode::StraightAlpha),
            (0x0101, GraphicsMode::PremulWhiteAlpha),
            (0x0102, GraphicsMode::PremulBlackAlpha),
            (0x0103, GraphicsMode::Composition),
            (0x0104, GraphicsMode::StraightAlphaBlend),
        ];
        for (code, mode) in pairs {
            assert_eq!(GraphicsMode::from_raw(code), mode);
            assert_eq!(mode.raw(), code);
        }
    }

    #[test]
    fn graphics_mode_unknown_codes_survive_as_other() {
        // Vendor / future-spec codes preserve the raw bit pattern.
        for code in [0x0001u16, 0x0050, 0x0105, 0xBEEF, 0xFFFF] {
            let mode = GraphicsMode::from_raw(code);
            assert_eq!(mode, GraphicsMode::Other(code));
            assert_eq!(mode.raw(), code);
            assert!(!mode.uses_opcolor());
        }
    }

    #[test]
    fn graphics_mode_uses_opcolor_per_table_4_2() {
        // Table 4-2's "Uses opcolor" column: Blend, Transparent,
        // StraightAlphaBlend.
        assert!(GraphicsMode::Blend.uses_opcolor());
        assert!(GraphicsMode::Transparent.uses_opcolor());
        assert!(GraphicsMode::StraightAlphaBlend.uses_opcolor());
        // The alpha-only / copy / composition modes do not.
        assert!(!GraphicsMode::Copy.uses_opcolor());
        assert!(!GraphicsMode::DitherCopy.uses_opcolor());
        assert!(!GraphicsMode::StraightAlpha.uses_opcolor());
        assert!(!GraphicsMode::PremulWhiteAlpha.uses_opcolor());
        assert!(!GraphicsMode::PremulBlackAlpha.uses_opcolor());
        assert!(!GraphicsMode::Composition.uses_opcolor());
    }

    #[test]
    fn gmin_graphics_mode_kind_routes_via_field() {
        let g = Gmin {
            graphics_mode: 0x0024,
            opcolor: [0, 0, 0],
            balance: 0,
        };
        assert_eq!(g.graphics_mode_kind(), GraphicsMode::Transparent);
        assert!(g.graphics_mode_kind().uses_opcolor());
    }

    // ─────────────── Balance (8.8 signed fixed-point, p. 201) ───────────────

    #[test]
    fn balance_decodes_endpoints_and_centre() {
        // 0 → 0.0 (centered).
        let g = Gmin {
            graphics_mode: 0,
            opcolor: [0; 3],
            balance: 0,
        };
        assert_eq!(g.balance_as_f32(), 0.0);

        // +1.0 → 0x0100 (integer part = 1, fraction = 0). The spec
        // states the high-order 8 bits hold the integer portion.
        let g = Gmin {
            graphics_mode: 0,
            opcolor: [0; 3],
            balance: 0x0100,
        };
        assert!((g.balance_as_f32() - 1.0).abs() < 1e-6);

        // -1.0 → 0xFF00 reinterpreted as i16 = -256.
        let g = Gmin {
            graphics_mode: 0,
            opcolor: [0; 3],
            balance: -256,
        };
        assert!((g.balance_as_f32() + 1.0).abs() < 1e-6);
    }

    #[test]
    fn balance_decodes_intermediate_fixed_point() {
        // 0x0080 = +0.5 (integer 0, fraction 0x80 / 0x100).
        let g = Gmin {
            graphics_mode: 0,
            opcolor: [0; 3],
            balance: 0x0080,
        };
        assert!((g.balance_as_f32() - 0.5).abs() < 1e-6);

        // -0.25 — fraction 0xC0 in two's-complement low byte.
        // -0.25 × 256 = -64 → i16 0xFFC0.
        let g = Gmin {
            graphics_mode: 0,
            opcolor: [0; 3],
            balance: -64,
        };
        assert!((g.balance_as_f32() + 0.25).abs() < 1e-6);
    }
}
