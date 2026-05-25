//! Producer Reference Time Box (`prft`).
//!
//! ISO/IEC 14496-12:2015 §8.16.5 (p. 111). The Producer Reference Time
//! Box supplies a relative wall-clock time at which a following movie
//! fragment — or a segment file containing movie fragments — was
//! produced by the writer. Paired live encoders and players use it to
//! keep production and consumption rates aligned, avoiding the buffer
//! over- and under-flows that drift produces over long sessions
//! (§8.16.5.1).
//!
//! Spec §8.16.5.1 verbatim:
//!
//! * "This box is related to the next movie fragment box that follows it
//!   in bitstream order. It must follow any segment type or segment
//!   index box (if any) in the segment, and occur before the following
//!   movie fragment box (to which it refers)."
//! * "If a segment file contains any producer reference time boxes, then
//!   the first of them shall occur before the first movie fragment box
//!   in that segment."
//! * "The box contains a time value measured on a clock which increments
//!   at the same rate as a UTC-synchronized NTP [RFC 5905] clock, using
//!   NTP format. This is associated with a media time for one of the
//!   tracks in the movie fragment."
//! * "Producer reference times should be associated with at most one
//!   track."
//!
//! Layout per ISO/IEC 14496-12 §8.16.5.2:
//!
//! ```text
//! aligned(8) class ProducerReferenceTimeBox extends FullBox('prft', version, 0) {
//!     unsigned int(32) reference_track_ID;
//!     unsigned int(64) ntp_timestamp;
//!     if (version == 0) {
//!         unsigned int(32) media_time;
//!     } else {
//!         unsigned int(64) media_time;
//!     }
//! }
//! ```
//!
//! `Container: File`, `Mandatory: No`, `Quantity: Zero or more`
//! (§8.16.5.1). QTFF does not define this box; it is an ISO BMFF-only
//! construct used by live-streaming derived specifications (DASH-LL /
//! CMAF / HLS-fMP4) and stays absent for plain `.mov` inputs.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Difference in seconds between the NTP epoch (1900-01-01T00:00:00Z)
/// and the Unix epoch (1970-01-01T00:00:00Z) — 70 years including 17
/// leap days. The NTP timestamp in `prft` is keyed to the NTP epoch
/// (RFC 5905 §6); converting to Unix subtracts this constant from the
/// integer-seconds portion.
pub const NTP_TO_UNIX_EPOCH_SECONDS: u64 = 2_208_988_800;

/// Parsed Producer Reference Time Box (ISO/IEC 14496-12 §8.16.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prft {
    /// `0` or `1` — selects the 32-bit vs 64-bit width of `media_time`
    /// (§8.16.5.2). Both widths are widened to `u64` in [`Prft`].
    pub version: u8,
    /// `track_ID` of the reference track this producer time pertains to
    /// (§8.16.5.3). The spec recommends a producer reference time be
    /// associated with at most one track per segment, so this is the
    /// single track the wall-clock value indexes.
    pub reference_track_id: u32,
    /// UTC wall-clock instant in NTP format (RFC 5905 §6) at which the
    /// next movie fragment was produced. Upper 32 bits = seconds since
    /// 1900-01-01T00:00:00Z; lower 32 bits = a fractional-second
    /// fixed-point counter (2⁻³² s per LSB). Use
    /// [`Prft::ntp_seconds`] / [`Prft::ntp_fraction`] /
    /// [`Prft::unix_micros`] to decompose.
    pub ntp_timestamp: u64,
    /// Media time, in the reference track's media-timescale ticks, that
    /// corresponds to the same instant as `ntp_timestamp`. Widened to
    /// `u64` to cover both `version = 0` (32-bit on disk) and
    /// `version = 1` (64-bit). §8.16.5.3 NOTE: "this timestamp will not
    /// be equal to the timestamp of the first sample of the adjacent
    /// segment of the reference track, but it is recommended it be in
    /// the range of the segment containing this producer reference time
    /// box."
    pub media_time: u64,
}

impl Prft {
    /// The integer-seconds portion of `ntp_timestamp` (upper 32 bits) —
    /// NTP timestamp format per RFC 5905 §6 ("seconds since 1900-01-01
    /// UTC").
    pub fn ntp_seconds(&self) -> u32 {
        (self.ntp_timestamp >> 32) as u32
    }

    /// The fractional-seconds portion of `ntp_timestamp` (lower 32
    /// bits). One LSB ≈ 2⁻³² ≈ 232.83 ps.
    pub fn ntp_fraction(&self) -> u32 {
        (self.ntp_timestamp & 0xFFFF_FFFF) as u32
    }

    /// Convert `ntp_timestamp` to a Unix-epoch microsecond count
    /// (1970-01-01T00:00:00Z), or `None` when the NTP timestamp is
    /// earlier than the Unix epoch (any NTP seconds value strictly less
    /// than 2 208 988 800 is pre-1970 and unrepresentable as an unsigned
    /// Unix instant).
    ///
    /// The fractional NTP word is converted to microseconds via
    /// `fraction * 1_000_000 / 2^32`, with the multiplication promoted
    /// to `u64` so the product can't overflow. The truncation matches
    /// the standard "NTP fraction → microsecond" reduction; per
    /// §8.16.5.3 the producer time is informative and the spec doesn't
    /// fix a rounding direction.
    pub fn unix_micros(&self) -> Option<u64> {
        let ntp_secs = self.ntp_seconds() as u64;
        let unix_secs = ntp_secs.checked_sub(NTP_TO_UNIX_EPOCH_SECONDS)?;
        let frac_micros = (self.ntp_fraction() as u64 * 1_000_000) >> 32;
        unix_secs
            .checked_mul(1_000_000)
            .and_then(|us| us.checked_add(frac_micros))
    }
}

/// Parse a `prft` payload.
///
/// Layout per ISO/IEC 14496-12 §8.16.5.2 — see the module-level docs.
///
/// Returns `Error::invalid` when:
/// * the payload is shorter than the FullBox header + the fixed-width
///   fields for the declared version (16 bytes for v0, 20 bytes for
///   v1 — including the 4-byte version/flags header),
/// * `version` is neither 0 nor 1 (the spec defines only those two; an
///   unknown version means the writer used a layout we can't decode),
/// * the payload carries trailing bytes after the fixed-width record
///   (`prft` has no list and no variable section; any tail is malformed
///   per §8.16.5.2).
///
/// `flags` is parsed but not validated; the spec fixes it at 0 but a
/// writer that sets a stray bit gets silently tolerated to keep
/// round-trip parsers happy.
pub fn parse_prft(payload: &[u8]) -> Result<Prft> {
    if payload.len() < 4 {
        return Err(Error::invalid(format!(
            "MOV: prft payload {} < 4-byte FullBox header",
            payload.len()
        )));
    }
    let version = payload[0];
    if version > 1 {
        return Err(Error::invalid(format!(
            "MOV: prft unknown version {version} (spec defines only v0 and v1)"
        )));
    }

    // After the FullBox header: reference_track_ID (4) + ntp_timestamp
    // (8) + media_time (4 for v0, 8 for v1).
    let media_time_width = if version == 0 { 4 } else { 8 };
    let expected_len = 4 + 4 + 8 + media_time_width;
    if payload.len() != expected_len {
        return Err(Error::invalid(format!(
            "MOV: prft v{version} payload {} != expected {expected_len} bytes",
            payload.len()
        )));
    }

    let reference_track_id = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let ntp_timestamp = u64::from_be_bytes([
        payload[8],
        payload[9],
        payload[10],
        payload[11],
        payload[12],
        payload[13],
        payload[14],
        payload[15],
    ]);
    let media_time = if version == 0 {
        u32::from_be_bytes([payload[16], payload[17], payload[18], payload[19]]) as u64
    } else {
        u64::from_be_bytes([
            payload[16],
            payload[17],
            payload[18],
            payload[19],
            payload[20],
            payload[21],
            payload[22],
            payload[23],
        ])
    };

    Ok(Prft {
        version,
        reference_track_id,
        ntp_timestamp,
        media_time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a v0 `prft` payload (`version = 0`, `flags = 0`,
    /// 32-bit media_time).
    fn build_prft_v0(reference_track_id: u32, ntp_timestamp: u64, media_time: u32) -> Vec<u8> {
        let mut p = Vec::with_capacity(20);
        p.push(0); // version
        p.extend_from_slice(&[0, 0, 0]); // flags
        p.extend_from_slice(&reference_track_id.to_be_bytes());
        p.extend_from_slice(&ntp_timestamp.to_be_bytes());
        p.extend_from_slice(&media_time.to_be_bytes());
        p
    }

    /// Build a v1 `prft` payload (`version = 1`, `flags = 0`,
    /// 64-bit media_time).
    fn build_prft_v1(reference_track_id: u32, ntp_timestamp: u64, media_time: u64) -> Vec<u8> {
        let mut p = Vec::with_capacity(24);
        p.push(1); // version
        p.extend_from_slice(&[0, 0, 0]); // flags
        p.extend_from_slice(&reference_track_id.to_be_bytes());
        p.extend_from_slice(&ntp_timestamp.to_be_bytes());
        p.extend_from_slice(&media_time.to_be_bytes());
        p
    }

    #[test]
    fn v0_round_trip_fields() {
        // NTP timestamp for 2024-01-01T00:00:00Z (RFC 5905 §6):
        //   unix_secs = 1_704_067_200
        //   ntp_secs  = 1_704_067_200 + 2_208_988_800 = 3_913_056_000
        //   ntp_ts    = (3_913_056_000 << 32) | 0  (no fractional part)
        let ntp = 3_913_056_000u64 << 32;
        let p = build_prft_v0(1, ntp, 90_000);
        let prft = parse_prft(&p).unwrap();
        assert_eq!(prft.version, 0);
        assert_eq!(prft.reference_track_id, 1);
        assert_eq!(prft.ntp_timestamp, ntp);
        assert_eq!(prft.media_time, 90_000);
        assert_eq!(prft.ntp_seconds(), 3_913_056_000);
        assert_eq!(prft.ntp_fraction(), 0);
        assert_eq!(prft.unix_micros(), Some(1_704_067_200_000_000));
    }

    #[test]
    fn v1_wide_media_time_round_trip() {
        // media_time beyond the 32-bit range — v1 must preserve it.
        let media_time: u64 = 0x1_0000_0001;
        let ntp: u64 = 0xDEAD_BEEF_CAFE_F00Du64;
        let p = build_prft_v1(7, ntp, media_time);
        let prft = parse_prft(&p).unwrap();
        assert_eq!(prft.version, 1);
        assert_eq!(prft.reference_track_id, 7);
        assert_eq!(prft.ntp_timestamp, ntp);
        assert_eq!(prft.media_time, media_time);
    }

    #[test]
    fn ntp_fraction_to_microseconds() {
        // ntp_secs = 2_208_988_800 (Unix epoch), ntp_fraction = 1 << 31
        // (exactly 0.5 s). Expect 500_000 µs past the Unix epoch.
        let ntp = (NTP_TO_UNIX_EPOCH_SECONDS << 32) | (1u64 << 31);
        let p = build_prft_v0(1, ntp, 0);
        let prft = parse_prft(&p).unwrap();
        assert_eq!(prft.ntp_seconds(), NTP_TO_UNIX_EPOCH_SECONDS as u32);
        assert_eq!(prft.ntp_fraction(), 1u32 << 31);
        assert_eq!(prft.unix_micros(), Some(500_000));
    }

    #[test]
    fn unix_micros_pre_1970_returns_none() {
        // NTP seconds = 0 means 1900-01-01, which has no unsigned
        // Unix-epoch representation. Must return `None`.
        let p = build_prft_v0(1, 0, 0);
        let prft = parse_prft(&p).unwrap();
        assert_eq!(prft.ntp_seconds(), 0);
        assert_eq!(prft.unix_micros(), None);
    }

    #[test]
    fn unknown_version_rejected() {
        // version = 2 is not defined by §8.16.5.2.
        let mut p = build_prft_v0(1, 0, 0);
        p[0] = 2;
        assert!(parse_prft(&p).is_err());
    }

    #[test]
    fn truncated_header_rejected() {
        // 3 bytes — one short of the 4-byte FullBox header.
        let p = vec![0u8; 3];
        assert!(parse_prft(&p).is_err());
    }

    #[test]
    fn truncated_v0_body_rejected() {
        // v0 declared but only 15 bytes total (one short of the
        // 16-byte expected length).
        let mut p = build_prft_v0(1, 0, 0);
        p.pop();
        assert!(parse_prft(&p).is_err());
    }

    #[test]
    fn truncated_v1_body_rejected() {
        // v1 declared but only the v0-sized body present (16 bytes).
        // v1 needs 20 bytes — the wider media_time mustn't silently
        // collapse to a 32-bit read.
        let mut p = build_prft_v0(1, 0, 0);
        p[0] = 1; // re-tag as v1 without growing the body
        assert!(parse_prft(&p).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        // No list, no variable section — any tail past the fixed
        // record indicates corruption / a writer bug.
        let mut p = build_prft_v0(1, 0, 0);
        p.extend_from_slice(&[0u8; 4]);
        assert!(parse_prft(&p).is_err());
    }

    #[test]
    fn v0_extra_byte_rejected() {
        // 21 bytes for a v0 box (16 + 5 stray). The v0 strict-length
        // path must reject; tolerating it would let a writer shove
        // unparseable extension data after the spec-defined record.
        let mut p = build_prft_v0(1, 0, 0);
        p.extend_from_slice(&[0u8; 5]);
        assert!(parse_prft(&p).is_err());
    }

    #[test]
    fn flags_nonzero_tolerated() {
        // Spec fixes `flags = 0`, but a writer that sets a stray bit
        // shouldn't fail-open the whole box parse. The flags word
        // sits at payload[1..4]; flip a bit and confirm we still
        // decode the rest.
        let mut p = build_prft_v0(1, 0x1234_5678_9ABC_DEF0u64, 42);
        p[3] = 0x01;
        let prft = parse_prft(&p).unwrap();
        assert_eq!(prft.reference_track_id, 1);
        assert_eq!(prft.ntp_timestamp, 0x1234_5678_9ABC_DEF0u64);
        assert_eq!(prft.media_time, 42);
    }
}
