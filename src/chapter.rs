//! Chapter-track resolution.
//!
//! QTFF "Chapter Lists" (p. 51, "Chapter Track References") models a
//! list of named chapters as a *secondary* track that the primary
//! audio/video track points at via a `tref/chap` reference. The
//! chapter track's media is a `text` track (handler subtype `text`)
//! whose samples are short title strings; each sample's DTS gives the
//! chapter's start time and the sample's duration gives the chapter's
//! length, both in the chapter track's media timescale.
//!
//! The on-disk Apple text-sample layout is:
//!
//! ```text
//! [text_size : u16 BE]
//! [text_bytes : text_size]      // typically Mac-Roman or UTF-8
//! [optional encd / styl / hlit / hclr extension atoms]
//! ```
//!
//! Apple's QuickTime text samples are pre-Unicode by default — the
//! bytes are interpreted as Mac-Roman unless an `encd` extension
//! atom (`[size:4][type:'encd'][encoding_id:u32]`) declares another
//! text encoding. Round 5 surfaces UTF-8 directly when the bytes are
//! valid UTF-8; otherwise falls back to a Mac-Roman → UTF-8 expansion
//! of the ASCII subset (bytes ≥ 0x80 become U+FFFD), matching the
//! conservative behaviour of `user_data::mac_roman_to_utf8`.
//!
//! Round 5 limits itself to one alias hop: the resolver follows a
//! single `tref/chap` reference per primary track, returns the
//! resolved chapter list, and surfaces an error if the referenced
//! track-id is missing or if the same primary track names itself
//! (a cycle that QTFF p. 51 forbids).

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// One resolved chapter: start/duration in the chapter track's media
/// timescale, plus the decoded title.
///
/// Time fields are media-timescale ticks (the chapter track's
/// `mdhd.time_scale`), not wall-clock seconds. Callers that want
/// seconds should divide by the chapter track's timescale. The
/// timescale itself is exposed alongside the entry list (see
/// [`ChapterList::time_scale`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChapterEntry {
    /// Start time in the chapter track's media-timescale ticks.
    pub start_time: u64,
    /// Sample duration in the chapter track's media-timescale ticks.
    pub duration: u32,
    /// Decoded chapter title. Best-effort UTF-8: valid UTF-8 bytes are
    /// surfaced verbatim; otherwise a Mac-Roman → ASCII expansion
    /// (bytes ≥ 0x80 → U+FFFD) keeps the surface lossless for ASCII.
    pub title: String,
    /// Mac TextEncoding identifier lifted from a trailing `encd`
    /// extension atom (`[size:4]['encd'][encoding_id:u32]`). `None`
    /// when the sample carried no `encd` trailer (the common case).
    /// This is a Mac-OS `TextEncoding` constant from `TextCommon.h`
    /// (e.g. `0x0500` → kCFStringEncodingUTF8); we surface it raw so
    /// callers can route to the appropriate decoder without a
    /// hard-coded mapping table here.
    pub text_encoding: Option<u32>,
}

/// A resolved chapter list — the entry vector plus the chapter
/// track's timescale (so callers can convert ticks → seconds without
/// having to walk the demuxer's track list).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChapterList {
    /// Resolved chapter track index inside `MovDemuxer::tracks`
    /// (0-based).
    pub track_index: u32,
    /// Chapter track's `mdhd.time_scale` — divide a `start_time` /
    /// `duration` by this value to obtain seconds.
    pub time_scale: u32,
    /// Ordered chapter entries.
    pub entries: Vec<ChapterEntry>,
}

/// Decode a single Apple text sample into a chapter title.
///
/// Layout: `[text_size:u16 BE][text_bytes: text_size][extensions]`. We
/// ignore the trailing extension atoms — this entry-point doesn't
/// surface styling. When the bytes are valid UTF-8 we return them
/// as-is; otherwise fall back to a conservative Mac-Roman → UTF-8
/// expansion (ASCII bytes survive, bytes ≥ 0x80 become U+FFFD). An
/// empty `text_size` returns an empty string rather than an error so
/// writers that emit zero-length placeholder samples don't break the
/// parse. See [`parse_text_sample_styles`] / [`decode_text_sample_full`]
/// for richer recovery of the extension atoms.
pub fn decode_text_sample(data: &[u8]) -> Result<String> {
    decode_text_sample_full(data).map(|(s, _)| s)
}

/// One styled run inside a `styl` extension atom (QTFF p. 56,
/// "Style Run" — `[start:u16 BE][end:u16 BE][font_id:u16 BE]
/// [face_style:u8][font_size:u8][text_color: 4 × u8 RGBA]`). The
/// character offsets are inclusive byte-positions into the text bytes
/// (not the UTF-8 code-point positions).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StyleRecord {
    /// Byte offset of the first character covered by this style run.
    pub start_char: u16,
    /// Byte offset (exclusive) past the last character covered.
    pub end_char: u16,
    /// `ftab` font-id this run paints with.
    pub font_id: u16,
    /// Bitmask: bit 0 = bold, bit 1 = italic, bit 2 = underline.
    pub face_style: u8,
    /// Point size in pixels.
    pub font_size: u8,
    /// RGBA colour (bytes).
    pub color: ColorRgba,
}

/// 4-byte RGBA colour record as written by `styl` / `hclr`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorRgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Highlighted character range (`hlit`) — `[start_char:u16][end_char:u16]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HighlightRange {
    pub start_char: u16,
    pub end_char: u16,
}

/// Highlight-color record (`hclr`) — RGBA colour painted over an
/// `hlit` range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HighlightColor {
    pub color: ColorRgba,
}

/// Font-table entry from `ftab` — `[font_id:u16][font_name_len:u8]
/// [font_name:font_name_len]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontTableEntry {
    pub font_id: u16,
    pub font_name: String,
}

/// All structured trailers extracted from a single Apple text sample.
/// `encoding_id` matches the value previously surfaced by
/// [`decode_text_sample_full`]; the rest of the fields land the §3.4
/// "Text Sample Display Properties" extension atoms documented in QTFF
/// pp. 55–57 (`styl` / `hlit` / `hclr` / `ftab` / `drpo`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextSampleStyles {
    /// Encoding override from a trailing `encd` atom, when present.
    pub encoding_id: Option<u32>,
    /// Style runs from a trailing `styl` atom, in document order.
    pub style_runs: Vec<StyleRecord>,
    /// Highlight character range (`hlit`).
    pub highlight: Option<HighlightRange>,
    /// Highlight colour (`hclr`).
    pub highlight_color: Option<HighlightColor>,
    /// Drop-shadow x/y offsets (`drpo`) — signed pixel offsets.
    pub drop_shadow_offset: Option<(i16, i16)>,
    /// Font-table entries from a trailing `ftab` atom.
    pub font_table: Vec<FontTableEntry>,
}

/// Walk the trailing extension atoms of a single text sample and
/// surface every documented styling record. This is the round-7
/// counterpart to [`decode_text_sample_full`]: the latter only lifts
/// the `encd` encoding override; this entry-point also surfaces
/// `styl` / `hlit` / `hclr` / `ftab` / `drpo`. Returns the decoded
/// title plus a [`TextSampleStyles`] aggregate.
///
/// Unknown / malformed atoms are silently skipped — text samples in
/// the wild routinely embed vendor extensions we don't recognise, and
/// failing the whole sample over an unknown trailer would be hostile.
pub fn parse_text_sample_styles(data: &[u8]) -> Result<(String, TextSampleStyles)> {
    if data.len() < 2 {
        return Err(Error::invalid("MOV: text sample < 2 bytes"));
    }
    let n = u16::from_be_bytes([data[0], data[1]]) as usize;
    if 2 + n > data.len() {
        return Err(Error::invalid("MOV: text sample size > body"));
    }
    let raw = &data[2..2 + n];
    let trailer = &data[2 + n..];
    let title = decode_text_bytes(raw);
    let mut out = TextSampleStyles::default();
    let mut p = 0usize;
    while p + 8 <= trailer.len() {
        let size = u32::from_be_bytes([trailer[p], trailer[p + 1], trailer[p + 2], trailer[p + 3]])
            as usize;
        if size < 8 || p + size > trailer.len() {
            break;
        }
        let fc = &trailer[p + 4..p + 8];
        let body = &trailer[p + 8..p + size];
        match fc {
            b"encd" => {
                if body.len() == 4 {
                    out.encoding_id =
                        Some(u32::from_be_bytes([body[0], body[1], body[2], body[3]]));
                } else if body.len() >= 8 {
                    out.encoding_id =
                        Some(u32::from_be_bytes([body[4], body[5], body[6], body[7]]));
                }
            }
            b"styl" => out.style_runs = parse_styl(body),
            b"hlit" => out.highlight = parse_hlit(body),
            b"hclr" => out.highlight_color = parse_hclr(body),
            b"drpo" => out.drop_shadow_offset = parse_drpo(body),
            b"ftab" => out.font_table = parse_ftab(body),
            _ => {}
        }
        p += size;
    }
    Ok((title, out))
}

fn parse_styl(body: &[u8]) -> Vec<StyleRecord> {
    if body.len() < 2 {
        return Vec::new();
    }
    let n = u16::from_be_bytes([body[0], body[1]]) as usize;
    let mut p = 2usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        // Each record is 12 bytes per QTFF p. 56:
        // start:2 end:2 font:2 face:1 size:1 color:4
        if p + 12 > body.len() {
            break;
        }
        let start_char = u16::from_be_bytes([body[p], body[p + 1]]);
        let end_char = u16::from_be_bytes([body[p + 2], body[p + 3]]);
        let font_id = u16::from_be_bytes([body[p + 4], body[p + 5]]);
        let face_style = body[p + 6];
        let font_size = body[p + 7];
        let color = ColorRgba {
            r: body[p + 8],
            g: body[p + 9],
            b: body[p + 10],
            a: body[p + 11],
        };
        out.push(StyleRecord {
            start_char,
            end_char,
            font_id,
            face_style,
            font_size,
            color,
        });
        p += 12;
    }
    out
}

fn parse_hlit(body: &[u8]) -> Option<HighlightRange> {
    if body.len() < 4 {
        return None;
    }
    Some(HighlightRange {
        start_char: u16::from_be_bytes([body[0], body[1]]),
        end_char: u16::from_be_bytes([body[2], body[3]]),
    })
}

fn parse_hclr(body: &[u8]) -> Option<HighlightColor> {
    if body.len() < 4 {
        return None;
    }
    Some(HighlightColor {
        color: ColorRgba {
            r: body[0],
            g: body[1],
            b: body[2],
            a: body[3],
        },
    })
}

fn parse_drpo(body: &[u8]) -> Option<(i16, i16)> {
    if body.len() < 4 {
        return None;
    }
    Some((
        i16::from_be_bytes([body[0], body[1]]),
        i16::from_be_bytes([body[2], body[3]]),
    ))
}

fn parse_ftab(body: &[u8]) -> Vec<FontTableEntry> {
    if body.len() < 2 {
        return Vec::new();
    }
    let n = u16::from_be_bytes([body[0], body[1]]) as usize;
    let mut p = 2usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        if p + 3 > body.len() {
            break;
        }
        let font_id = u16::from_be_bytes([body[p], body[p + 1]]);
        let name_len = body[p + 2] as usize;
        p += 3;
        if p + name_len > body.len() {
            break;
        }
        let font_name = std::str::from_utf8(&body[p..p + name_len])
            .unwrap_or("")
            .to_string();
        p += name_len;
        out.push(FontTableEntry { font_id, font_name });
    }
    out
}

/// Encode a chapter / text-track sample payload — `[length:u16][UTF-8
/// text]`, optionally followed by an `encd` text-encoding-override atom
/// (`[size:u32]['encd'][encoding_id:u32]`) when `encoding` is `Some`.
///
/// This is the write-side inverse of [`decode_text_sample_full`]: the
/// `length` is the byte length of the UTF-8 text (capped at
/// `u16::MAX`), the text follows verbatim, and a present `encoding`
/// emits the 12-byte `encd` trailer the read side's `scan_for_encd`
/// lifts back onto a chapter entry's `text_encoding`.
pub fn encode_text_sample(text: &str, encoding: Option<u32>) -> Vec<u8> {
    let bytes = text.as_bytes();
    let n = bytes.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(2 + n + encoding.map_or(0, |_| 12));
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&bytes[..n]);
    if let Some(enc) = encoding {
        // `encd` atom: [size:u32 = 12]['encd'][encoding_id:u32].
        out.extend_from_slice(&12u32.to_be_bytes());
        out.extend_from_slice(b"encd");
        out.extend_from_slice(&enc.to_be_bytes());
    }
    out
}

/// Decode a single Apple text sample into a chapter title plus the
/// Mac `TextEncoding` constant lifted from a trailing `encd` extension
/// atom when present.
///
/// Same body shape as [`decode_text_sample`]; this variant additionally
/// scans the byte stream that follows the text payload for an
/// `[size:4]['encd'][encoding_id:u32]` atom (Apple's text-sample
/// encoding override; not formally listed in QTFF Table 3-4 alongside
/// `styl`/`hlit`/etc., but emitted by iTunes/iOS writers when the text
/// is not Mac-Roman). Returns `(title, encoding_id)` where
/// `encoding_id` is `None` when no `encd` trailer is present.
pub fn decode_text_sample_full(data: &[u8]) -> Result<(String, Option<u32>)> {
    if data.len() < 2 {
        return Err(Error::invalid("MOV: text sample < 2 bytes"));
    }
    let n = u16::from_be_bytes([data[0], data[1]]) as usize;
    if 2 + n > data.len() {
        return Err(Error::invalid("MOV: text sample size > body"));
    }
    let raw = &data[2..2 + n];
    let trailer = &data[2 + n..];
    let encoding = scan_for_encd(trailer);
    Ok((decode_text_bytes(raw), encoding))
}

/// Scan the trailing extension-atom bytes of a text sample for an
/// `encd` atom. The body is a flat list of `[size:4][type:4][body]`
/// records; we walk it tolerantly (truncated/garbage trailers stop the
/// walk silently — the title decode never depends on the trailer).
fn scan_for_encd(buf: &[u8]) -> Option<u32> {
    let mut p = 0usize;
    while p + 8 <= buf.len() {
        let size = u32::from_be_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
        if size < 8 || p + size > buf.len() {
            break;
        }
        let fc = &buf[p + 4..p + 8];
        if fc == b"encd" {
            // `encd` body is `[encoding_id:u32]`. Some writers add a
            // FullBox prefix; accept either shape: read the trailing
            // 4 bytes when there are exactly 4 bytes of body, or skip
            // a 4-byte ver+flags when there are 8.
            let body = &buf[p + 8..p + size];
            if body.len() == 4 {
                return Some(u32::from_be_bytes([body[0], body[1], body[2], body[3]]));
            } else if body.len() >= 8 {
                return Some(u32::from_be_bytes([body[4], body[5], body[6], body[7]]));
            }
        }
        p += size;
    }
    None
}

fn decode_text_bytes(raw: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    // Mac-Roman fallback: ASCII passes through, ≥ 0x80 → U+FFFD.
    let mut s = String::with_capacity(raw.len());
    for &c in raw {
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

    #[test]
    fn decode_utf8_text_sample() {
        let mut p = Vec::new();
        let txt = "Chapter 1".as_bytes();
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        let s = decode_text_sample(&p).unwrap();
        assert_eq!(s, "Chapter 1");
    }

    #[test]
    fn decode_text_sample_with_trailing_extension() {
        // "Intro" + bogus 8-byte trailer mimicking an `encd` atom that
        // we deliberately ignore.
        let mut p = Vec::new();
        let txt = b"Intro";
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        p.extend_from_slice(&8u32.to_be_bytes());
        p.extend_from_slice(b"encd");
        let s = decode_text_sample(&p).unwrap();
        assert_eq!(s, "Intro");
    }

    #[test]
    fn decode_zero_length_sample_returns_empty() {
        let p = [0u8, 0u8];
        assert_eq!(decode_text_sample(&p).unwrap(), "");
    }

    #[test]
    fn decode_too_short_errors() {
        assert!(decode_text_sample(&[0u8]).is_err());
    }

    #[test]
    fn decode_size_beyond_body_errors() {
        // Declares 5 bytes of text but only carries 2.
        let p = [0u8, 5, b'h', b'i'];
        assert!(decode_text_sample(&p).is_err());
    }

    #[test]
    fn mac_roman_fallback_replaces_high_bytes() {
        // Invalid UTF-8 single byte 0x80 → U+FFFD.
        let mut p = Vec::new();
        p.extend_from_slice(&3u16.to_be_bytes());
        p.extend_from_slice(&[b'a', 0x80, b'b']);
        let s = decode_text_sample(&p).unwrap();
        assert_eq!(s, "a\u{FFFD}b");
    }

    #[test]
    fn encd_trailer_surfaced_via_decode_full() {
        // "Hello" + encd[utf8 = 0x0500] trailer.
        let mut p = Vec::new();
        let txt = b"Hello";
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        // encd: size=12, type='encd', body=u32 0x0500
        p.extend_from_slice(&12u32.to_be_bytes());
        p.extend_from_slice(b"encd");
        p.extend_from_slice(&0x0500u32.to_be_bytes());
        let (title, enc) = decode_text_sample_full(&p).unwrap();
        assert_eq!(title, "Hello");
        assert_eq!(enc, Some(0x0500));
    }

    #[test]
    fn encd_with_fullbox_prefix_also_decodes() {
        // 16-byte 'encd' atom: ver+flags + encoding_id
        let mut p = Vec::new();
        let txt = b"X";
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        p.extend_from_slice(&16u32.to_be_bytes());
        p.extend_from_slice(b"encd");
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&0x0123u32.to_be_bytes()); // encoding_id
        let (_, enc) = decode_text_sample_full(&p).unwrap();
        assert_eq!(enc, Some(0x0123));
    }

    #[test]
    fn no_encd_trailer_returns_none() {
        let mut p = Vec::new();
        let txt = b"Plain";
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        let (_, enc) = decode_text_sample_full(&p).unwrap();
        assert_eq!(enc, None);
    }

    #[test]
    fn styl_trailer_records_decoded() {
        let mut p = Vec::new();
        let txt = b"Hello";
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        // styl atom: size=22 = 8 hdr + 2 count + 12 record
        p.extend_from_slice(&22u32.to_be_bytes());
        p.extend_from_slice(b"styl");
        p.extend_from_slice(&1u16.to_be_bytes()); // count
        p.extend_from_slice(&0u16.to_be_bytes()); // start
        p.extend_from_slice(&5u16.to_be_bytes()); // end
        p.extend_from_slice(&1u16.to_be_bytes()); // font_id
        p.push(0x01); // face = bold
        p.push(12); // font size
        p.extend_from_slice(&[0xFF, 0x00, 0x00, 0xFF]); // red opaque
        let (title, styles) = parse_text_sample_styles(&p).unwrap();
        assert_eq!(title, "Hello");
        assert_eq!(styles.style_runs.len(), 1);
        let r = &styles.style_runs[0];
        assert_eq!(r.start_char, 0);
        assert_eq!(r.end_char, 5);
        assert_eq!(r.font_id, 1);
        assert_eq!(r.face_style, 0x01);
        assert_eq!(r.font_size, 12);
        assert_eq!(r.color.r, 0xFF);
        assert_eq!(r.color.a, 0xFF);
    }

    #[test]
    fn hlit_and_hclr_trailers_decode() {
        let mut p = Vec::new();
        let txt = b"Word";
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        // hlit
        p.extend_from_slice(&12u32.to_be_bytes());
        p.extend_from_slice(b"hlit");
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&3u16.to_be_bytes());
        // hclr
        p.extend_from_slice(&12u32.to_be_bytes());
        p.extend_from_slice(b"hclr");
        p.extend_from_slice(&[0x10, 0x20, 0x30, 0xFF]);
        let (_, styles) = parse_text_sample_styles(&p).unwrap();
        assert_eq!(
            styles.highlight,
            Some(HighlightRange {
                start_char: 1,
                end_char: 3
            })
        );
        let hc = styles.highlight_color.unwrap();
        assert_eq!(hc.color.r, 0x10);
        assert_eq!(hc.color.g, 0x20);
        assert_eq!(hc.color.b, 0x30);
    }

    #[test]
    fn drpo_offset_decodes_signed() {
        let mut p = Vec::new();
        let txt = b"X";
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        p.extend_from_slice(&12u32.to_be_bytes());
        p.extend_from_slice(b"drpo");
        p.extend_from_slice(&(-2i16).to_be_bytes());
        p.extend_from_slice(&3i16.to_be_bytes());
        let (_, styles) = parse_text_sample_styles(&p).unwrap();
        assert_eq!(styles.drop_shadow_offset, Some((-2, 3)));
    }

    #[test]
    fn ftab_table_decodes_pascal_strings() {
        let mut p = Vec::new();
        let txt = b"Sample";
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        // ftab body: count=2 + (id=1, name="Helv") + (id=2, name="Geneva")
        let mut body = Vec::new();
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.push(4);
        body.extend_from_slice(b"Helv");
        body.extend_from_slice(&2u16.to_be_bytes());
        body.push(6);
        body.extend_from_slice(b"Geneva");
        p.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
        p.extend_from_slice(b"ftab");
        p.extend_from_slice(&body);
        let (_, styles) = parse_text_sample_styles(&p).unwrap();
        assert_eq!(styles.font_table.len(), 2);
        assert_eq!(styles.font_table[0].font_id, 1);
        assert_eq!(styles.font_table[0].font_name, "Helv");
        assert_eq!(styles.font_table[1].font_name, "Geneva");
    }

    #[test]
    fn parse_text_sample_styles_emits_encoding_too() {
        let mut p = Vec::new();
        let txt = b"Hi";
        p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        p.extend_from_slice(txt);
        p.extend_from_slice(&12u32.to_be_bytes());
        p.extend_from_slice(b"encd");
        p.extend_from_slice(&0x0000_0500u32.to_be_bytes());
        let (title, styles) = parse_text_sample_styles(&p).unwrap();
        assert_eq!(title, "Hi");
        assert_eq!(styles.encoding_id, Some(0x0500));
    }
}
