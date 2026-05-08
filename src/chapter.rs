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
/// ignore the trailing extension atoms — round 5 doesn't surface
/// styling. When the bytes are valid UTF-8 we return them as-is;
/// otherwise fall back to a conservative Mac-Roman → UTF-8 expansion
/// (ASCII bytes survive, bytes ≥ 0x80 become U+FFFD). An empty
/// `text_size` returns an empty string rather than an error so writers
/// that emit zero-length placeholder samples don't break the parse.
pub fn decode_text_sample(data: &[u8]) -> Result<String> {
    decode_text_sample_full(data).map(|(s, _)| s)
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
}
