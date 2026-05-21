//! Track Load Settings atom (`load`).
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01),
//! "Track Load Settings Atoms" (pp. 48–49). The `load` atom carries
//! per-track preloading hints — a movie-timescale start/duration
//! window of the track the player is asked to keep in memory, a
//! mutually-exclusive enable-mode flag pair, and a bit-field of
//! quality / I-O hints.
//!
//! Layout per QTFF Figure 2-12 (p. 48), 20 bytes total:
//!
//! ```text
//! +----------------------------+----+
//! | preload start time         |  4 |  movie timescale
//! +----------------------------+----+
//! | preload duration           |  4 |  movie timescale; -1 = to end
//! +----------------------------+----+
//! | preload flags              |  4 |  enable-mode bitfield
//! +----------------------------+----+
//! | default hints              |  4 |  playback-quality bitfield
//! +----------------------------+----+
//! ```
//!
//! The `load` atom is **not** a FullBox: there is no leading
//! version+flags byte/triplet. The four fields are raw big-endian
//! 32-bit integers.
//!
//! ISO BMFF (ISO/IEC 14496-12) does not standardise `load`; it is a
//! QuickTime-only atom carried inside `trak`.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// QTFF p. 48 — `preload flags` bit `0x0001`: preload regardless of
/// whether the track is enabled.
pub const LOAD_PRELOAD_ALWAYS: u32 = 0x0000_0001;

/// QTFF p. 48 — `preload flags` bit `0x0002`: preload only when the
/// track is enabled. Mutually exclusive with [`LOAD_PRELOAD_ALWAYS`]
/// per spec ("Only two flags are defined, and they are mutually
/// exclusive").
pub const LOAD_PRELOAD_IF_ENABLED: u32 = 0x0000_0002;

/// QTFF p. 49 — `default hints` bit `0x0020`: track should be played
/// using double-buffered I/O.
pub const LOAD_HINT_DOUBLE_BUFFER: u32 = 0x0000_0020;

/// QTFF p. 49 — `default hints` bit `0x0100`: track should be displayed
/// at highest possible quality, without regard to real-time performance.
pub const LOAD_HINT_HIGH_QUALITY: u32 = 0x0000_0100;

/// Sentinel for [`Load::preload_duration`] meaning "preload from
/// `preload_start_time` to the end of the track" (QTFF p. 48, "Preload
/// duration"). Stored on the wire as the 32-bit two's-complement -1
/// (`0xFFFF_FFFF`); we keep the field unsigned and surface this constant
/// for symmetric comparisons.
pub const LOAD_PRELOAD_DURATION_TO_END: u32 = 0xFFFF_FFFF;

/// Parsed `load` atom (Track Load Settings).
///
/// `preload_start_time` and `preload_duration` are expressed in the
/// **movie** time coordinate system (`mvhd.time_scale`), not the
/// track's media timescale. A `preload_duration` of [`LOAD_PRELOAD_DURATION_TO_END`]
/// means "preload from `preload_start_time` to the end of the track".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Load {
    /// Starting time, in movie-timescale ticks, of the segment to be
    /// preloaded.
    pub preload_start_time: u32,
    /// Duration, in movie-timescale ticks, of the segment to be
    /// preloaded. The on-wire value `0xFFFF_FFFF` means "to the end of
    /// the track" (see [`LOAD_PRELOAD_DURATION_TO_END`] +
    /// [`Self::is_preload_to_end`]).
    pub preload_duration: u32,
    /// Bitfield governing the preload operation
    /// ([`LOAD_PRELOAD_ALWAYS`] / [`LOAD_PRELOAD_IF_ENABLED`]).
    pub preload_flags: u32,
    /// Playback-quality hints bitfield ([`LOAD_HINT_DOUBLE_BUFFER`] /
    /// [`LOAD_HINT_HIGH_QUALITY`]). The spec calls out only two
    /// well-known bits; we surface the raw u32 so vendor-specific bits
    /// survive.
    pub default_hints: u32,
}

impl Load {
    /// True when `preload_duration == 0xFFFF_FFFF` — preload extends
    /// from `preload_start_time` to the end of the track per QTFF p. 48.
    pub fn is_preload_to_end(&self) -> bool {
        self.preload_duration == LOAD_PRELOAD_DURATION_TO_END
    }

    /// True when [`LOAD_PRELOAD_ALWAYS`] is set in `preload_flags` —
    /// the player should preload the segment regardless of whether the
    /// track is enabled (QTFF p. 48, "Preload flags").
    pub fn preload_always(&self) -> bool {
        (self.preload_flags & LOAD_PRELOAD_ALWAYS) != 0
    }

    /// True when [`LOAD_PRELOAD_IF_ENABLED`] is set in `preload_flags`
    /// — the player should preload the segment only when the track is
    /// enabled (QTFF p. 48, "Preload flags").
    pub fn preload_if_enabled(&self) -> bool {
        (self.preload_flags & LOAD_PRELOAD_IF_ENABLED) != 0
    }

    /// True when [`LOAD_HINT_DOUBLE_BUFFER`] is set in `default_hints`
    /// — the track should be played using double-buffered I/O (QTFF
    /// p. 49, "Double buffer").
    pub fn hint_double_buffer(&self) -> bool {
        (self.default_hints & LOAD_HINT_DOUBLE_BUFFER) != 0
    }

    /// True when [`LOAD_HINT_HIGH_QUALITY`] is set in `default_hints`
    /// — the track should be displayed at the highest possible
    /// quality, ignoring real-time-performance considerations (QTFF
    /// p. 49, "High quality").
    pub fn hint_high_quality(&self) -> bool {
        (self.default_hints & LOAD_HINT_HIGH_QUALITY) != 0
    }
}

/// Parse a `load` atom payload.
///
/// Expects exactly the 16-byte body shown in QTFF Figure 2-12 (p. 48):
/// four big-endian u32s — `preload_start_time`, `preload_duration`,
/// `preload_flags`, `default_hints`. Returns
/// `Error::invalid` on a payload shorter than 16 bytes; **trailing
/// bytes are silently ignored** so vendor extensions don't break the
/// parser.
pub fn parse_load(payload: &[u8]) -> Result<Load> {
    if payload.len() < 16 {
        return Err(Error::invalid(format!(
            "MOV: load payload {} < 16 bytes",
            payload.len()
        )));
    }
    let preload_start_time = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let preload_duration = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let preload_flags = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let default_hints = u32::from_be_bytes([payload[12], payload[13], payload[14], payload[15]]);
    Ok(Load {
        preload_start_time,
        preload_duration,
        preload_flags,
        default_hints,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_load(start: u32, dur: u32, flags: u32, hints: u32) -> Vec<u8> {
        let mut p = Vec::with_capacity(16);
        p.extend_from_slice(&start.to_be_bytes());
        p.extend_from_slice(&dur.to_be_bytes());
        p.extend_from_slice(&flags.to_be_bytes());
        p.extend_from_slice(&hints.to_be_bytes());
        p
    }

    #[test]
    fn parses_canonical_fields_in_movie_timescale() {
        // start=0, dur=600 (movie ticks), flags=PRELOAD_ALWAYS,
        // hints=DOUBLE_BUFFER.
        let p = build_load(0, 600, LOAD_PRELOAD_ALWAYS, LOAD_HINT_DOUBLE_BUFFER);
        let l = parse_load(&p).unwrap();
        assert_eq!(l.preload_start_time, 0);
        assert_eq!(l.preload_duration, 600);
        assert!(l.preload_always());
        assert!(!l.preload_if_enabled());
        assert!(l.hint_double_buffer());
        assert!(!l.hint_high_quality());
        assert!(!l.is_preload_to_end());
    }

    #[test]
    fn duration_minus_one_means_to_end() {
        let p = build_load(120, LOAD_PRELOAD_DURATION_TO_END, 0, 0);
        let l = parse_load(&p).unwrap();
        assert!(l.is_preload_to_end());
        assert_eq!(l.preload_duration, 0xFFFF_FFFF);
        assert_eq!(l.preload_start_time, 120);
    }

    #[test]
    fn preload_if_enabled_flag_decodes() {
        let p = build_load(0, 0, LOAD_PRELOAD_IF_ENABLED, 0);
        let l = parse_load(&p).unwrap();
        assert!(l.preload_if_enabled());
        assert!(!l.preload_always());
    }

    #[test]
    fn high_quality_hint_decodes() {
        let p = build_load(0, 0, 0, LOAD_HINT_HIGH_QUALITY);
        let l = parse_load(&p).unwrap();
        assert!(l.hint_high_quality());
        assert!(!l.hint_double_buffer());
    }

    #[test]
    fn combined_hints_preserve_raw_bits() {
        // Vendor-specific 0x0080 alongside the spec'd 0x0020 +
        // 0x0100 — verify the raw u32 round-trips so callers can
        // recover unknown vendor bits.
        let hints = LOAD_HINT_DOUBLE_BUFFER | LOAD_HINT_HIGH_QUALITY | 0x0000_0080;
        let p = build_load(0, 0, 0, hints);
        let l = parse_load(&p).unwrap();
        assert_eq!(l.default_hints, hints);
        assert!(l.hint_double_buffer());
        assert!(l.hint_high_quality());
        assert_eq!(l.default_hints & 0x0000_0080, 0x0000_0080);
    }

    #[test]
    fn truncated_payload_errors() {
        // 15 bytes — one short of the 16-byte minimum.
        let p = vec![0u8; 15];
        assert!(parse_load(&p).is_err());
    }

    #[test]
    fn trailing_bytes_are_ignored() {
        // Spec demands exactly 16 bytes; we forgive trailing junk so
        // vendor extensions don't break the parser.
        let mut p = build_load(0, 100, LOAD_PRELOAD_ALWAYS, LOAD_HINT_HIGH_QUALITY);
        p.extend_from_slice(&[0xDEu8, 0xAD, 0xBE, 0xEF]);
        let l = parse_load(&p).unwrap();
        assert_eq!(l.preload_duration, 100);
        assert!(l.hint_high_quality());
    }
}
