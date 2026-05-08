//! Time-code sample-description (`tmcd` inside `stsd`) parsing.
//!
//! QTFF p. 106 ("Timecode Sample Description") describes the body of a
//! `tmcd` entry in a track whose handler subtype is `tmcd`. This is
//! *structurally distinct* from the `tmcd` atom inside `gmhd` (round 5,
//! see [`crate::gmhd::Tcmi`]) — that one wraps a `tcmi` child carrying
//! display-style fields, whereas the `tmcd` inside `stsd` carries the
//! timing fields:
//!
//! ```text
//! [universal sample-description header: 16 bytes]
//! [reserved : u32 = 0]
//! [flags    : u32]            // bit-field; see TimecodeFlags below
//! [time_scale     : u32]      // ticks per second for frame_duration
//! [frame_duration : u32]      // ticks per frame in time_scale units
//! [number_of_frames : u8]     // frames per second (24/25/30/...) or
//!                             //   counter-tick frame count
//! [reserved : 3 bytes = 0]
//! [optional source-reference user-data atom containing 'name']
//! ```
//!
//! Round 6 surfaces the four fixed fields and the optional source-tape
//! `name`; the bit-flag values (drop frame, 24-hour, negatives OK,
//! counter) are exposed via [`Tmcd::is_drop_frame`] / etc. accessors so
//! callers don't have to remember the magic numbers.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Drop-frame flag (`0x0001`). Set when timecode follows the SMPTE
/// drop-frame counting convention used at fractional NTSC rates.
pub const TMCD_FLAG_DROP_FRAME: u32 = 0x0000_0001;
/// 24-hour-max flag (`0x0002`). Timecode wraps after 24 hours.
pub const TMCD_FLAG_24_HOUR: u32 = 0x0000_0002;
/// Negative-times-OK flag (`0x0004`). Negative time values permitted.
pub const TMCD_FLAG_NEGATIVES_OK: u32 = 0x0000_0004;
/// Counter flag (`0x0008`). Sample data is a 32-bit counter rather
/// than a packed `[H:M:S:F]` timecode record.
pub const TMCD_FLAG_COUNTER: u32 = 0x0000_0008;

/// Parsed `tmcd` sample-description body (everything after the
/// universal 16-byte sample-description header).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tmcd {
    /// Raw flags word (bit field; see `TMCD_FLAG_*` constants).
    pub flags: u32,
    /// Time scale — ticks per second for `frame_duration`.
    pub time_scale: u32,
    /// How long each frame lasts in `time_scale` units.
    pub frame_duration: u32,
    /// Frames per second (or counter-ticks-per-frame when
    /// `TMCD_FLAG_COUNTER` is set). 8-bit field per QTFF p. 107.
    pub number_of_frames: u8,
    /// Optional source-tape name lifted from the trailing `name` user
    /// data atom (QTFF p. 107 "Source reference"). `None` when absent.
    pub source_name: Option<String>,
}

impl Tmcd {
    /// True when `TMCD_FLAG_DROP_FRAME` is set.
    pub fn is_drop_frame(&self) -> bool {
        self.flags & TMCD_FLAG_DROP_FRAME != 0
    }
    /// True when `TMCD_FLAG_24_HOUR` is set (timecode wraps after 24h).
    pub fn is_24_hour_max(&self) -> bool {
        self.flags & TMCD_FLAG_24_HOUR != 0
    }
    /// True when `TMCD_FLAG_NEGATIVES_OK` is set.
    pub fn is_negatives_ok(&self) -> bool {
        self.flags & TMCD_FLAG_NEGATIVES_OK != 0
    }
    /// True when `TMCD_FLAG_COUNTER` is set; sample data is a 32-bit
    /// counter rather than a packed timecode record.
    pub fn is_counter(&self) -> bool {
        self.flags & TMCD_FLAG_COUNTER != 0
    }
}

/// Parse a `tmcd` sample-description body (the bytes that follow the
/// 16-byte universal sample-description header).
///
/// Layout per QTFF p. 106:
/// `[reserved:u32][flags:u32][time_scale:u32][frame_duration:u32]
///  [number_of_frames:u8][reserved:3 bytes][optional source-ref atom]`.
///
/// The trailing source reference is described as a "user data atom
/// containing information about the source tape" (QTFF p. 107). In
/// practice it's a `udta` container holding a single `name` atom whose
/// payload is a UTF-8 / Mac-Roman tape name. We surface the decoded
/// name on [`Tmcd::source_name`] when present and ignore unrecognised
/// trailing atoms (forward-compatibility with future per-codec
/// extensions).
pub fn parse_tmcd_sample_description(body: &[u8]) -> Result<Tmcd> {
    if body.len() < 20 {
        return Err(Error::invalid("MOV: tmcd sample description < 20 bytes"));
    }
    // Bytes 0..4 are reserved (must be zero per spec; we don't enforce
    // since some writers leak garbage here).
    let flags = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    let time_scale = u32::from_be_bytes([body[8], body[9], body[10], body[11]]);
    let frame_duration = u32::from_be_bytes([body[12], body[13], body[14], body[15]]);
    let number_of_frames = body[16];
    // body[17..20] is a 24-bit reserved that must be zero.
    let mut source_name: Option<String> = None;
    if body.len() > 20 {
        // Trailing source reference. The QTFF wording is "a user data
        // atom" — concretely the bytes are a `udta`-like list of inner
        // atoms, the canonical entry being `name` carrying a counted-
        // bytes name. We accept either:
        //   (a) a raw `[size:4]['name'][text]` atom directly, or
        //   (b) a `udta` wrapper with a `name` child.
        source_name = scan_source_reference(&body[20..]);
    }
    Ok(Tmcd {
        flags,
        time_scale,
        frame_duration,
        number_of_frames,
        source_name,
    })
}

/// Walk the trailing source-reference bytes looking for a `name` atom.
/// Tolerates either the raw atom shape or a `udta`-wrapper.
fn scan_source_reference(buf: &[u8]) -> Option<String> {
    if let Some(name) = find_name_atom(buf) {
        return Some(name);
    }
    // Try one level deeper — `udta` wrapper is a common shape.
    let mut p = 0usize;
    while p + 8 <= buf.len() {
        let size = u32::from_be_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
        if size < 8 || p + size > buf.len() {
            break;
        }
        let fc = &buf[p + 4..p + 8];
        if fc == b"udta" {
            if let Some(s) = find_name_atom(&buf[p + 8..p + size]) {
                return Some(s);
            }
        }
        p += size;
    }
    None
}

/// Find a single `name` atom inside `buf` (a flat list of
/// `[size:4][type:4][body]` records) and return its decoded text.
fn find_name_atom(buf: &[u8]) -> Option<String> {
    let mut p = 0usize;
    while p + 8 <= buf.len() {
        let size = u32::from_be_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
        if size < 8 || p + size > buf.len() {
            break;
        }
        let fc = &buf[p + 4..p + 8];
        if fc == b"name" {
            return Some(decode_name_payload(&buf[p + 8..p + size]));
        }
        p += size;
    }
    None
}

/// Decode the body of a `name` user-data atom into a best-effort
/// UTF-8 string. Mirrors the conservative decoder used in
/// [`crate::user_data`]: valid UTF-8 passes through; invalid UTF-8
/// falls back to a Mac-Roman → ASCII expansion (bytes ≥ 0x80 → U+FFFD).
///
/// QuickTime's `name` payload inside a source reference is a *raw*
/// counted byte string (no `[ver+flags]` FullBox prefix in the QTFF
/// description) — so we treat the whole payload as the name. Trailing
/// NULs are stripped.
fn decode_name_payload(raw: &[u8]) -> String {
    let trimmed = match raw.last() {
        Some(0) => &raw[..raw.len() - 1],
        _ => raw,
    };
    if let Ok(s) = std::str::from_utf8(trimmed) {
        return s.to_string();
    }
    let mut s = String::with_capacity(trimmed.len());
    for &c in trimmed {
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
    fn tmcd_minimum_body_parses() {
        // 20 bytes of fixed body, no source reference.
        let mut p = vec![0u8; 20];
        p[4..8].copy_from_slice(&0x0000_0001u32.to_be_bytes()); // drop-frame flag
        p[8..12].copy_from_slice(&30000u32.to_be_bytes()); // time_scale (29.97 base)
        p[12..16].copy_from_slice(&1001u32.to_be_bytes()); // frame_duration
        p[16] = 30; // number_of_frames
        let t = parse_tmcd_sample_description(&p).unwrap();
        assert_eq!(t.flags, 1);
        assert!(t.is_drop_frame());
        assert!(!t.is_24_hour_max());
        assert!(!t.is_counter());
        assert_eq!(t.time_scale, 30000);
        assert_eq!(t.frame_duration, 1001);
        assert_eq!(t.number_of_frames, 30);
        assert!(t.source_name.is_none());
    }

    #[test]
    fn tmcd_too_short_errors() {
        assert!(parse_tmcd_sample_description(&[0u8; 19]).is_err());
    }

    #[test]
    fn tmcd_with_raw_name_atom_parses_source() {
        let mut p = vec![0u8; 20];
        p[4..8].copy_from_slice(&TMCD_FLAG_24_HOUR.to_be_bytes());
        p[8..12].copy_from_slice(&24000u32.to_be_bytes());
        p[12..16].copy_from_slice(&1000u32.to_be_bytes());
        p[16] = 24;
        // raw `[size:4]['name'][bytes]` — counted-bytes payload.
        let name = b"Tape A1";
        let mut name_atom = Vec::new();
        name_atom.extend_from_slice(&((8 + name.len()) as u32).to_be_bytes());
        name_atom.extend_from_slice(b"name");
        name_atom.extend_from_slice(name);
        p.extend_from_slice(&name_atom);
        let t = parse_tmcd_sample_description(&p).unwrap();
        assert!(t.is_24_hour_max());
        assert_eq!(t.source_name.as_deref(), Some("Tape A1"));
    }

    #[test]
    fn tmcd_with_udta_wrapper_parses_source() {
        let mut p = vec![0u8; 20];
        p[8..12].copy_from_slice(&25000u32.to_be_bytes()); // 25fps
        p[12..16].copy_from_slice(&1000u32.to_be_bytes());
        p[16] = 25;
        // udta { name "Source" }
        let name = b"Source";
        let mut name_atom = Vec::new();
        name_atom.extend_from_slice(&((8 + name.len()) as u32).to_be_bytes());
        name_atom.extend_from_slice(b"name");
        name_atom.extend_from_slice(name);
        let mut udta = Vec::new();
        udta.extend_from_slice(&((8 + name_atom.len()) as u32).to_be_bytes());
        udta.extend_from_slice(b"udta");
        udta.extend_from_slice(&name_atom);
        p.extend_from_slice(&udta);
        let t = parse_tmcd_sample_description(&p).unwrap();
        assert_eq!(t.source_name.as_deref(), Some("Source"));
    }

    #[test]
    fn tmcd_counter_flag_round_trips() {
        let mut p = vec![0u8; 20];
        p[4..8].copy_from_slice(&TMCD_FLAG_COUNTER.to_be_bytes());
        let t = parse_tmcd_sample_description(&p).unwrap();
        assert!(t.is_counter());
    }
}
