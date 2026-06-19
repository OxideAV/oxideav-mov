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

    /// Decode one timecode-track **sample payload** (the bytes read out
    /// of `mdat` for a single timecode sample) according to this sample
    /// description.
    ///
    /// QTFF p. 108 ("Timecode Sample Data") defines two mutually-
    /// exclusive layouts selected by [`Tmcd::is_counter`]:
    ///
    /// * Counter flag set ⇒ a single big-endian 32-bit counter value
    ///   ([`TimecodeSample::Counter`]).
    /// * Counter flag clear ⇒ a packed `[H:M:S:F]` record
    ///   ([`TimecodeSample::Record`]): an 8-bit `Hours`, a 1-bit
    ///   `Negative` sign packed into the high bit of the next byte whose
    ///   low 7 bits are `Minutes`, an 8-bit `Seconds`, and an 8-bit
    ///   `Frames`.
    ///
    /// Both layouts are 4 bytes on disk; this accepts any payload of at
    /// least 4 bytes and ignores trailing bytes (some writers pad the
    /// sample). Payloads shorter than 4 bytes are rejected.
    pub fn decode_sample(&self, payload: &[u8]) -> Result<TimecodeSample> {
        if payload.len() < 4 {
            return Err(Error::invalid("MOV: timecode sample payload < 4 bytes"));
        }
        let raw = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
        if self.is_counter() {
            Ok(TimecodeSample::Counter(raw))
        } else {
            // Byte 1 packs the 1-bit Negative sign (high bit) with the
            // 7-bit Minutes field (low 7 bits) — QTFF p. 108.
            let hours = payload[0];
            let negative = payload[1] & 0x80 != 0;
            let minutes = payload[1] & 0x7f;
            let seconds = payload[2];
            let frames = payload[3];
            Ok(TimecodeSample::Record(TimecodeRecord {
                negative,
                hours,
                minutes,
                seconds,
                frames,
            }))
        }
    }
}

/// One decoded timecode-track sample payload (QTFF p. 108).
///
/// The variant is chosen by the sample description's Counter flag, not
/// by the bytes themselves — call [`Tmcd::decode_sample`] which consults
/// the owning [`Tmcd`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimecodeSample {
    /// A 32-bit tape-counter value (Counter flag set). Interpreted in
    /// units of the sample description's `number_of_frames` "frames per
    /// counter tick".
    Counter(u32),
    /// A packed `[H:M:S:F]` timecode record (Counter flag clear).
    Record(TimecodeRecord),
}

/// A packed `[Hours:Minutes:Seconds:Frames]` timecode record with sign
/// (QTFF p. 108 "Timecode Sample Data").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimecodeRecord {
    /// Sign bit — `true` when the record value is negative (only
    /// meaningful when the description sets `TMCD_FLAG_NEGATIVES_OK`).
    pub negative: bool,
    /// Starting hours (8-bit).
    pub hours: u8,
    /// Starting minutes (7-bit; `0..=59` for valid SMPTE timecode).
    pub minutes: u8,
    /// Starting seconds (8-bit; `0..=59` for valid SMPTE timecode).
    pub seconds: u8,
    /// Starting frames within the second (8-bit). Per QTFF p. 108 this
    /// must not exceed the description's `number_of_frames`.
    pub frames: u8,
}

impl TimecodeRecord {
    /// Convert this record to an **absolute non-drop-frame frame
    /// count**, treating `fps` as the number of frames per second
    /// (the sample description's `number_of_frames`).
    ///
    /// This is plain positional arithmetic the QTFF field semantics
    /// support directly:
    /// `((H*3600 + M*60 + S) * fps + F)`, negated when `negative`.
    ///
    /// NOTE: this does **not** apply SMPTE drop-frame skipping. The
    /// QTFF specification names the drop-frame flag (p. 106) and the
    /// glossary describes drop-frame as "a synchronizing technique that
    /// skips timecodes" (p. 251) but does not state which frame numbers
    /// are dropped — that arithmetic lives in SMPTE 12M, outside
    /// `docs/container/`. Callers that need a drop-frame-corrected count
    /// must apply that correction themselves. Returns `None` if `fps`
    /// is 0 or the multiplication overflows `i64`.
    pub fn to_frames(&self, fps: u8) -> Option<i64> {
        if fps == 0 {
            return None;
        }
        let fps = fps as i64;
        let total_seconds = (self.hours as i64)
            .checked_mul(3600)?
            .checked_add((self.minutes as i64).checked_mul(60)?)?
            .checked_add(self.seconds as i64)?;
        let frames = total_seconds
            .checked_mul(fps)?
            .checked_add(self.frames as i64)?;
        Some(if self.negative { -frames } else { frames })
    }

    /// Inverse of [`TimecodeRecord::to_frames`]: build a record from an
    /// absolute non-drop-frame frame count at `fps` frames per second.
    ///
    /// The sign is taken from `frames`; magnitude is decomposed into
    /// `H:M:S:F`. Like [`to_frames`](Self::to_frames) this is the
    /// non-drop-frame mapping only. Returns `None` if `fps` is 0.
    pub fn from_frames(frames: i64, fps: u8) -> Option<Self> {
        if fps == 0 {
            return None;
        }
        let negative = frames < 0;
        let mag = frames.unsigned_abs();
        let fps_u = fps as u64;
        let f = (mag % fps_u) as u8;
        let total_seconds = mag / fps_u;
        let s = (total_seconds % 60) as u8;
        let total_minutes = total_seconds / 60;
        let m = (total_minutes % 60) as u8;
        let h = (total_minutes / 60).min(u8::MAX as u64) as u8;
        Some(TimecodeRecord {
            negative,
            hours: h,
            minutes: m,
            seconds: s,
            frames: f,
        })
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

/// Decode the body of a source-reference `name` atom into a best-effort
/// UTF-8 string.
///
/// QTFF p. 224 ("Creating a Timecode Track…") gives the concrete on-disk
/// shape of the `name` atom inside a timecode source reference:
///
/// ```text
/// [size:u32]['name'][string_length:u16][language_code:u16][text...]
/// ```
///
/// i.e. the *body* (post-header bytes passed here) is a 16-bit byte count
/// of the text, a 16-bit Mac language code, then the text itself. We
/// detect that structured form when the leading `string_length` exactly
/// accounts for the remaining bytes (`body_len - 4`) and strip the
/// 4-byte prefix; otherwise we fall back to treating the whole payload
/// as a raw counted byte string (some writers emit the name with no
/// length/language header). Trailing NULs are stripped in both cases.
///
/// Text decode is conservative (mirrors [`crate::user_data`]): valid
/// UTF-8 passes through; otherwise a Mac-Roman → ASCII expansion maps
/// bytes ≥ 0x80 to U+FFFD.
fn decode_name_payload(raw: &[u8]) -> String {
    // Structured form per QTFF p. 224: [u16 string_length][u16 language].
    let text = if raw.len() >= 4 {
        let declared = u16::from_be_bytes([raw[0], raw[1]]) as usize;
        if declared == raw.len() - 4 {
            &raw[4..]
        } else {
            raw
        }
    } else {
        raw
    };
    let trimmed = match text.last() {
        Some(0) => &text[..text.len() - 1],
        _ => text,
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

    /// Build a minimal non-counter `tmcd` description at `fps`.
    fn tmcd_record_desc(fps: u8, flags: u32) -> Tmcd {
        let mut p = vec![0u8; 20];
        p[4..8].copy_from_slice(&flags.to_be_bytes());
        p[8..12].copy_from_slice(&30000u32.to_be_bytes());
        p[12..16].copy_from_slice(&1001u32.to_be_bytes());
        p[16] = fps;
        parse_tmcd_sample_description(&p).unwrap()
    }

    #[test]
    fn decode_counter_sample() {
        let t = tmcd_record_desc(30, TMCD_FLAG_COUNTER);
        let s = t.decode_sample(&0x0001_2345u32.to_be_bytes()).unwrap();
        assert_eq!(s, TimecodeSample::Counter(0x0001_2345));
    }

    #[test]
    fn decode_record_sample_hmsf() {
        let t = tmcd_record_desc(30, 0);
        // 01:23:45:17, positive.
        let payload = [1u8, 23, 45, 17];
        let s = t.decode_sample(&payload).unwrap();
        assert_eq!(
            s,
            TimecodeSample::Record(TimecodeRecord {
                negative: false,
                hours: 1,
                minutes: 23,
                seconds: 45,
                frames: 17,
            })
        );
    }

    #[test]
    fn decode_record_negative_sign_packs_into_minutes_byte() {
        let t = tmcd_record_desc(25, TMCD_FLAG_NEGATIVES_OK);
        // Hours=2, minutes byte = 0x80 | 10 (negative, 10 minutes).
        let payload = [2u8, 0x80 | 10, 30, 12];
        match t.decode_sample(&payload).unwrap() {
            TimecodeSample::Record(r) => {
                assert!(r.negative);
                assert_eq!(r.hours, 2);
                assert_eq!(r.minutes, 10);
                assert_eq!(r.seconds, 30);
                assert_eq!(r.frames, 12);
            }
            other => panic!("expected record, got {other:?}"),
        }
    }

    #[test]
    fn decode_sample_short_payload_errors() {
        let t = tmcd_record_desc(30, 0);
        assert!(t.decode_sample(&[0u8; 3]).is_err());
    }

    #[test]
    fn decode_sample_ignores_trailing_padding() {
        let t = tmcd_record_desc(30, 0);
        let s = t.decode_sample(&[0, 0, 1, 5, 0xAA, 0xBB]).unwrap();
        assert_eq!(
            s,
            TimecodeSample::Record(TimecodeRecord {
                negative: false,
                hours: 0,
                minutes: 0,
                seconds: 1,
                frames: 5,
            })
        );
    }

    #[test]
    fn record_to_frames_non_drop() {
        // 00:00:10:05 at 30fps = 305 frames.
        let r = TimecodeRecord {
            negative: false,
            hours: 0,
            minutes: 0,
            seconds: 10,
            frames: 5,
        };
        assert_eq!(r.to_frames(30), Some(305));
        // 01:00:00:00 at 30fps = 108000 frames.
        let r2 = TimecodeRecord {
            hours: 1,
            ..Default::default()
        };
        assert_eq!(r2.to_frames(30), Some(108_000));
    }

    #[test]
    fn record_to_frames_negative() {
        let r = TimecodeRecord {
            negative: true,
            hours: 0,
            minutes: 0,
            seconds: 2,
            frames: 0,
        };
        assert_eq!(r.to_frames(25), Some(-50));
    }

    #[test]
    fn record_to_frames_zero_fps_is_none() {
        let r = TimecodeRecord::default();
        assert_eq!(r.to_frames(0), None);
    }

    #[test]
    fn record_from_frames_round_trips() {
        for fps in [24u8, 25, 30] {
            for &n in &[0i64, 1, 29, 305, 108_000, -50, 86_399 * 30] {
                let r = TimecodeRecord::from_frames(n, fps).unwrap();
                assert_eq!(r.to_frames(fps), Some(n), "fps={fps} n={n}");
            }
        }
    }

    #[test]
    fn record_from_frames_decomposes() {
        // 305 frames at 30fps = 00:00:10:05.
        let r = TimecodeRecord::from_frames(305, 30).unwrap();
        assert_eq!(r.hours, 0);
        assert_eq!(r.minutes, 0);
        assert_eq!(r.seconds, 10);
        assert_eq!(r.frames, 5);
        assert!(!r.negative);
    }

    #[test]
    fn record_from_frames_zero_fps_is_none() {
        assert_eq!(TimecodeRecord::from_frames(10, 0), None);
    }

    #[test]
    fn structured_name_source_reference_strips_length_and_language() {
        // QTFF p. 224 worked example: name body is
        // [string_length:u16=12][language:u16=0]["my tape name"].
        let mut p = vec![0u8; 20];
        p[16] = 20; // 29.97 fps frames
        let text = b"my tape name"; // 12 bytes
        let mut name_body = Vec::new();
        name_body.extend_from_slice(&(text.len() as u16).to_be_bytes());
        name_body.extend_from_slice(&0u16.to_be_bytes()); // language English
        name_body.extend_from_slice(text);
        let mut name_atom = Vec::new();
        name_atom.extend_from_slice(&((8 + name_body.len()) as u32).to_be_bytes());
        name_atom.extend_from_slice(b"name");
        name_atom.extend_from_slice(&name_body);
        p.extend_from_slice(&name_atom);
        let t = parse_tmcd_sample_description(&p).unwrap();
        assert_eq!(t.source_name.as_deref(), Some("my tape name"));
    }

    #[test]
    fn raw_name_without_length_header_still_decodes() {
        // Backward-tolerant raw form: body is just the text, no
        // length/language prefix. "Tape A1" is 7 bytes, so the leading
        // u16 (0x5461 = "Ta") will NOT equal len-4, keeping the raw path.
        let mut p = vec![0u8; 20];
        let text = b"Tape A1";
        let mut name_atom = Vec::new();
        name_atom.extend_from_slice(&((8 + text.len()) as u32).to_be_bytes());
        name_atom.extend_from_slice(b"name");
        name_atom.extend_from_slice(text);
        p.extend_from_slice(&name_atom);
        let t = parse_tmcd_sample_description(&p).unwrap();
        assert_eq!(t.source_name.as_deref(), Some("Tape A1"));
    }
}
