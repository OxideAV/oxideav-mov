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
    if data.len() < 2 {
        return Err(Error::invalid("MOV: text sample < 2 bytes"));
    }
    let n = u16::from_be_bytes([data[0], data[1]]) as usize;
    if 2 + n > data.len() {
        return Err(Error::invalid("MOV: text sample size > body"));
    }
    let raw = &data[2..2 + n];
    Ok(decode_text_bytes(raw))
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
}
