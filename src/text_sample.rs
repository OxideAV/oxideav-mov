//! QuickTime Text Sample Description (`text` inside `stsd`) parsing.
//!
//! A classic QuickTime text track (`hdlr` component subtype `text`,
//! QTFF p. 116) declares its display configuration through a `text`
//! sample-description entry whose data-format FourCC is `text`
//! (QTFF p. 108, "Text Sample Description"). This is the *description*
//! side of the text media — distinct from the per-sample text payload
//! (`[length:u16][text…][extensions]`) decoded by
//! [`crate::chapter::parse_text_sample_styles`].
//!
//! The text media handler adds the following fields after the universal
//! 16-byte sample-description header (QTFF pp. 108–110):
//!
//! ```text
//! [display_flags     : u32]   // see TEXT_FLAG_* below
//! [text_justification: i32]   //  0 = left, 1 = center, -1 = right
//! [background_color  : 3×u16] // 48-bit RGB
//! [default_text_box  : 4×u16] // top, left, bottom, right
//! [reserved          : u64]   // must be 0
//! [font_number       : u16]   // must be 0
//! [font_face         : u16]   // style bitmask (see TEXT_FACE_*)
//! [reserved          : u8 ]   // must be 0
//! [reserved          : u16]   // must be 0
//! [foreground_color  : 3×u16] // 48-bit RGB
//! [text_name         : Pascal string] // font name; optional
//! ```
//!
//! The fixed fields occupy 43 bytes; the trailing `text_name` is a
//! classic Pascal string (1-byte length prefix + bytes). Some writers
//! omit the `text_name` entirely (the body ends at 43 bytes); that is
//! tolerated and surfaced as an empty name.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Don't-auto-scale display flag (`0x0002`). When set, the text media
/// handler reflows text instead of scaling when the track is scaled.
pub const TEXT_FLAG_DONT_AUTO_SCALE: u32 = 0x0000_0002;
/// Use-movie-background-color flag (`0x0008`). The handler ignores the
/// description's `background_color` and uses the movie background.
pub const TEXT_FLAG_USE_MOVIE_BG_COLOR: u32 = 0x0000_0008;
/// Scroll-in flag (`0x0020`). Scroll until the last of the text is in view.
pub const TEXT_FLAG_SCROLL_IN: u32 = 0x0000_0020;
/// Scroll-out flag (`0x0040`). Scroll until the last of the text is gone.
pub const TEXT_FLAG_SCROLL_OUT: u32 = 0x0000_0040;
/// Horizontal-scroll flag (`0x0080`). Scroll horizontally rather than
/// vertically.
pub const TEXT_FLAG_HORIZONTAL_SCROLL: u32 = 0x0000_0080;
/// Reverse-scroll flag (`0x0100`). Scroll down / backward.
pub const TEXT_FLAG_REVERSE_SCROLL: u32 = 0x0000_0100;
/// Continuous-scroll flag (`0x0200`). Display new samples by scrolling
/// out the old ones.
pub const TEXT_FLAG_CONTINUOUS_SCROLL: u32 = 0x0000_0200;
/// Drop-shadow flag (`0x1000`). Draw text with a drop shadow.
pub const TEXT_FLAG_DROP_SHADOW: u32 = 0x0000_1000;
/// Anti-alias flag (`0x2000`). Anti-alias when drawing text.
pub const TEXT_FLAG_ANTI_ALIAS: u32 = 0x0000_2000;
/// Key-text flag (`0x4000`). Don't display the background color so the
/// text overlay background keys through.
pub const TEXT_FLAG_KEY_TEXT: u32 = 0x0000_4000;

/// Bold font-face style bit (`0x0001`, QTFF p. 109).
pub const TEXT_FACE_BOLD: u16 = 0x0001;
/// Italic font-face style bit (`0x0002`).
pub const TEXT_FACE_ITALIC: u16 = 0x0002;
/// Underline font-face style bit (`0x0004`).
pub const TEXT_FACE_UNDERLINE: u16 = 0x0004;
/// Outline font-face style bit (`0x0008`).
pub const TEXT_FACE_OUTLINE: u16 = 0x0008;
/// Shadow font-face style bit (`0x0010`).
pub const TEXT_FACE_SHADOW: u16 = 0x0010;
/// Condense font-face style bit (`0x0020`).
pub const TEXT_FACE_CONDENSE: u16 = 0x0020;
/// Extend font-face style bit (`0x0040`).
pub const TEXT_FACE_EXTEND: u16 = 0x0040;

/// A 48-bit RGB colour as written by the QuickTime text sample
/// description (three big-endian 16-bit components, QTFF p. 109).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb48 {
    /// Red component (0..=65535).
    pub red: u16,
    /// Green component (0..=65535).
    pub green: u16,
    /// Blue component (0..=65535).
    pub blue: u16,
}

/// A QuickDraw rectangle (top, left, bottom, right) as 16-bit
/// coordinates, used for the text description's `default_text_box`
/// (QTFF p. 109). Typically all-zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextBox {
    /// Top edge.
    pub top: u16,
    /// Left edge.
    pub left: u16,
    /// Bottom edge.
    pub bottom: u16,
    /// Right edge.
    pub right: u16,
}

/// Text justification (QTFF p. 109): the `text_justification` field is a
/// signed 32-bit integer with three documented values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextJustification {
    /// Left-justified (`0`). The default.
    #[default]
    Left,
    /// Centered (`1`).
    Center,
    /// Right-justified (`-1`).
    Right,
    /// Any value the spec does not name; preserved verbatim.
    Other(i32),
}

impl TextJustification {
    /// Decode the on-wire signed value into a [`TextJustification`].
    pub fn from_raw(v: i32) -> Self {
        match v {
            0 => TextJustification::Left,
            1 => TextJustification::Center,
            -1 => TextJustification::Right,
            other => TextJustification::Other(other),
        }
    }

    /// The on-wire signed value this justification encodes to.
    pub fn to_raw(self) -> i32 {
        match self {
            TextJustification::Left => 0,
            TextJustification::Center => 1,
            TextJustification::Right => -1,
            TextJustification::Other(v) => v,
        }
    }
}

/// Parsed QuickTime Text Sample Description body (everything after the
/// universal 16-byte sample-description header). QTFF pp. 108–110.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextSampleDescription {
    /// Raw `display_flags` word (see `TEXT_FLAG_*`).
    pub display_flags: u32,
    /// Text justification.
    pub text_justification: TextJustification,
    /// 48-bit RGB background colour.
    pub background_color: Rgb48,
    /// Default text box rectangle (usually all zeros).
    pub default_text_box: TextBox,
    /// Font number (must be 0 per spec, preserved verbatim).
    pub font_number: u16,
    /// Font face style bitmask (see `TEXT_FACE_*`).
    pub font_face: u16,
    /// 48-bit RGB foreground colour.
    pub foreground_color: Rgb48,
    /// Font name from the trailing Pascal string; empty when omitted.
    pub text_name: String,
}

impl TextSampleDescription {
    /// True when [`TEXT_FLAG_USE_MOVIE_BG_COLOR`] is set: the renderer
    /// must ignore [`Self::background_color`] in favour of the movie
    /// background colour.
    pub fn use_movie_background(&self) -> bool {
        self.display_flags & TEXT_FLAG_USE_MOVIE_BG_COLOR != 0
    }
    /// True when [`TEXT_FLAG_DONT_AUTO_SCALE`] is set.
    pub fn dont_auto_scale(&self) -> bool {
        self.display_flags & TEXT_FLAG_DONT_AUTO_SCALE != 0
    }
    /// True when [`TEXT_FLAG_DROP_SHADOW`] is set.
    pub fn has_drop_shadow(&self) -> bool {
        self.display_flags & TEXT_FLAG_DROP_SHADOW != 0
    }
    /// True when [`TEXT_FLAG_ANTI_ALIAS`] is set.
    pub fn anti_aliased(&self) -> bool {
        self.display_flags & TEXT_FLAG_ANTI_ALIAS != 0
    }
    /// True when [`TEXT_FLAG_KEY_TEXT`] is set (background keys through).
    pub fn is_key_text(&self) -> bool {
        self.display_flags & TEXT_FLAG_KEY_TEXT != 0
    }
    /// True when any scroll flag (in / out / horizontal / reverse /
    /// continuous) is set.
    pub fn is_scrolling(&self) -> bool {
        const SCROLL_MASK: u32 = TEXT_FLAG_SCROLL_IN
            | TEXT_FLAG_SCROLL_OUT
            | TEXT_FLAG_HORIZONTAL_SCROLL
            | TEXT_FLAG_REVERSE_SCROLL
            | TEXT_FLAG_CONTINUOUS_SCROLL;
        self.display_flags & SCROLL_MASK != 0
    }
    /// True when [`TEXT_FACE_BOLD`] is set in `font_face`.
    pub fn is_bold(&self) -> bool {
        self.font_face & TEXT_FACE_BOLD != 0
    }
    /// True when [`TEXT_FACE_ITALIC`] is set in `font_face`.
    pub fn is_italic(&self) -> bool {
        self.font_face & TEXT_FACE_ITALIC != 0
    }
    /// True when [`TEXT_FACE_UNDERLINE`] is set in `font_face`.
    pub fn is_underline(&self) -> bool {
        self.font_face & TEXT_FACE_UNDERLINE != 0
    }
}

/// Minimum fixed-field length of a QuickTime Text Sample Description body
/// (everything before the trailing Pascal `text_name`): QTFF pp. 108–110.
/// `4 (flags) + 4 (just) + 6 (bg) + 8 (box) + 8 (rsvd) + 2 (font#)
///  + 2 (face) + 1 (rsvd) + 2 (rsvd) + 6 (fg) = 43`.
pub const TEXT_SAMPLE_DESC_FIXED_LEN: usize = 43;

fn read_rgb48(buf: &[u8], off: usize) -> Rgb48 {
    Rgb48 {
        red: u16::from_be_bytes([buf[off], buf[off + 1]]),
        green: u16::from_be_bytes([buf[off + 2], buf[off + 3]]),
        blue: u16::from_be_bytes([buf[off + 4], buf[off + 5]]),
    }
}

/// Parse a `text` sample-description body (the bytes that follow the
/// 16-byte universal sample-description header). QTFF pp. 108–110.
///
/// Bodies shorter than [`TEXT_SAMPLE_DESC_FIXED_LEN`] are rejected. The
/// trailing `text_name` Pascal string is optional: when the body ends at
/// the fixed length, or when the declared Pascal length runs past the
/// buffer, the name is taken as empty / truncated leniently rather than
/// failing the whole entry (real-world writers vary here).
pub fn parse_text_sample_description(body: &[u8]) -> Result<TextSampleDescription> {
    if body.len() < TEXT_SAMPLE_DESC_FIXED_LEN {
        return Err(Error::invalid(
            "MOV: text sample description < 43 bytes (fixed fields)",
        ));
    }
    let display_flags = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    let just_raw = i32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    let background_color = read_rgb48(body, 8); // 8..14
    let default_text_box = TextBox {
        top: u16::from_be_bytes([body[14], body[15]]),
        left: u16::from_be_bytes([body[16], body[17]]),
        bottom: u16::from_be_bytes([body[18], body[19]]),
        right: u16::from_be_bytes([body[20], body[21]]),
    };
    // body[22..30] reserved (u64, must be 0) — not enforced.
    let font_number = u16::from_be_bytes([body[30], body[31]]);
    let font_face = u16::from_be_bytes([body[32], body[33]]);
    // body[34] reserved (u8); body[35..37] reserved (u16).
    let foreground_color = read_rgb48(body, 37); // 37..43
    let text_name = read_pascal_string(&body[TEXT_SAMPLE_DESC_FIXED_LEN..]);
    Ok(TextSampleDescription {
        display_flags,
        text_justification: TextJustification::from_raw(just_raw),
        background_color,
        default_text_box,
        font_number,
        font_face,
        foreground_color,
        text_name,
    })
}

/// Decode a trailing Pascal string (1-byte length prefix + that many
/// bytes). Returns an empty string when `buf` is empty. When the declared
/// length runs past `buf`, the available bytes are taken (lenient). Text
/// decode mirrors the conservative Mac-Roman fallback used elsewhere in
/// the crate: valid UTF-8 passes through; otherwise bytes ≥ 0x80 become
/// U+FFFD.
fn read_pascal_string(buf: &[u8]) -> String {
    if buf.is_empty() {
        return String::new();
    }
    let declared = buf[0] as usize;
    let avail = (buf.len() - 1).min(declared);
    let bytes = &buf[1..1 + avail];
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut s = String::with_capacity(bytes.len());
    for &c in bytes {
        if c < 0x80 {
            s.push(c as char);
        } else {
            s.push('\u{FFFD}');
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 43-byte fixed body with the supplied flags / face,
    /// leaving colours and box zeroed.
    fn fixed_body(flags: u32, just: i32, face: u16) -> Vec<u8> {
        let mut b = vec![0u8; TEXT_SAMPLE_DESC_FIXED_LEN];
        b[0..4].copy_from_slice(&flags.to_be_bytes());
        b[4..8].copy_from_slice(&just.to_be_bytes());
        b[32..34].copy_from_slice(&face.to_be_bytes());
        b
    }

    #[test]
    fn parses_minimum_fixed_body() {
        let b = fixed_body(0, 0, 0);
        let d = parse_text_sample_description(&b).unwrap();
        assert_eq!(d.display_flags, 0);
        assert_eq!(d.text_justification, TextJustification::Left);
        assert_eq!(d.font_face, 0);
        assert_eq!(d.text_name, "");
    }

    #[test]
    fn rejects_short_body() {
        assert!(parse_text_sample_description(&[0u8; 42]).is_err());
    }

    #[test]
    fn decodes_justification_values() {
        let left = parse_text_sample_description(&fixed_body(0, 0, 0)).unwrap();
        assert_eq!(left.text_justification, TextJustification::Left);
        let center = parse_text_sample_description(&fixed_body(0, 1, 0)).unwrap();
        assert_eq!(center.text_justification, TextJustification::Center);
        let right = parse_text_sample_description(&fixed_body(0, -1, 0)).unwrap();
        assert_eq!(right.text_justification, TextJustification::Right);
        let other = parse_text_sample_description(&fixed_body(0, 7, 0)).unwrap();
        assert_eq!(other.text_justification, TextJustification::Other(7));
        assert_eq!(TextJustification::Right.to_raw(), -1);
        assert_eq!(TextJustification::Other(7).to_raw(), 7);
    }

    #[test]
    fn decodes_display_flag_accessors() {
        let flags = TEXT_FLAG_USE_MOVIE_BG_COLOR
            | TEXT_FLAG_DROP_SHADOW
            | TEXT_FLAG_ANTI_ALIAS
            | TEXT_FLAG_SCROLL_IN;
        let d = parse_text_sample_description(&fixed_body(flags, 0, 0)).unwrap();
        assert!(d.use_movie_background());
        assert!(d.has_drop_shadow());
        assert!(d.anti_aliased());
        assert!(d.is_scrolling());
        assert!(!d.dont_auto_scale());
        assert!(!d.is_key_text());
    }

    #[test]
    fn decodes_font_face_accessors() {
        let face = TEXT_FACE_BOLD | TEXT_FACE_ITALIC;
        let d = parse_text_sample_description(&fixed_body(0, 0, face)).unwrap();
        assert!(d.is_bold());
        assert!(d.is_italic());
        assert!(!d.is_underline());
    }

    #[test]
    fn decodes_colors_and_box_and_font_number() {
        let mut b = fixed_body(0, 0, 0);
        // background_color = (0x1111, 0x2222, 0x3333)
        b[8..10].copy_from_slice(&0x1111u16.to_be_bytes());
        b[10..12].copy_from_slice(&0x2222u16.to_be_bytes());
        b[12..14].copy_from_slice(&0x3333u16.to_be_bytes());
        // default_text_box = top1 left2 bottom3 right4
        b[14..16].copy_from_slice(&1u16.to_be_bytes());
        b[16..18].copy_from_slice(&2u16.to_be_bytes());
        b[18..20].copy_from_slice(&3u16.to_be_bytes());
        b[20..22].copy_from_slice(&4u16.to_be_bytes());
        // font_number
        b[30..32].copy_from_slice(&5u16.to_be_bytes());
        // foreground_color = (0xAAAA, 0xBBBB, 0xCCCC)
        b[37..39].copy_from_slice(&0xAAAAu16.to_be_bytes());
        b[39..41].copy_from_slice(&0xBBBBu16.to_be_bytes());
        b[41..43].copy_from_slice(&0xCCCCu16.to_be_bytes());
        let d = parse_text_sample_description(&b).unwrap();
        assert_eq!(
            d.background_color,
            Rgb48 {
                red: 0x1111,
                green: 0x2222,
                blue: 0x3333
            }
        );
        assert_eq!(
            d.default_text_box,
            TextBox {
                top: 1,
                left: 2,
                bottom: 3,
                right: 4
            }
        );
        assert_eq!(d.font_number, 5);
        assert_eq!(
            d.foreground_color,
            Rgb48 {
                red: 0xAAAA,
                green: 0xBBBB,
                blue: 0xCCCC
            }
        );
    }

    #[test]
    fn parses_trailing_pascal_font_name() {
        let mut b = fixed_body(0, 0, 0);
        let name = b"Helvetica";
        b.push(name.len() as u8);
        b.extend_from_slice(name);
        let d = parse_text_sample_description(&b).unwrap();
        assert_eq!(d.text_name, "Helvetica");
    }

    #[test]
    fn tolerates_truncated_pascal_length() {
        let mut b = fixed_body(0, 0, 0);
        b.push(20); // claims 20 bytes
        b.extend_from_slice(b"Geneva"); // only 6 present
        let d = parse_text_sample_description(&b).unwrap();
        assert_eq!(d.text_name, "Geneva");
    }

    #[test]
    fn empty_name_when_body_is_exactly_fixed_len() {
        let d = parse_text_sample_description(&fixed_body(0, 0, 0)).unwrap();
        assert_eq!(d.text_name, "");
    }

    #[test]
    fn non_utf8_name_uses_replacement() {
        let mut b = fixed_body(0, 0, 0);
        b.push(2);
        b.extend_from_slice(&[b'A', 0x80]);
        let d = parse_text_sample_description(&b).unwrap();
        assert_eq!(d.text_name, "A\u{FFFD}");
    }
}
