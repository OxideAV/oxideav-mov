//! Progressive Download Information Box (`pdin`).
//!
//! ISO/IEC 14496-12:2015 §8.1.3 (pp. 21–22). The Progressive Download
//! Information Box is a file-level FullBox (`version = 0`, `flags = 0`)
//! that carries pairs of numbers — `(rate, initial_delay)` — describing
//! "if you can download this file at *rate* bytes/sec, then waiting
//! *initial_delay* milliseconds before starting playback will let
//! decoding proceed without stalling".
//!
//! The list runs to end-of-box (the spec uses `for (i=0; ; i++)`
//! syntax in §8.1.3.2), so the entry count is `body_len / 8` and there
//! is no on-disk count field. A receiving party estimates the actual
//! download rate it is experiencing and linearly interpolates between
//! the surrounding pairs — or extrapolates from the first / last entry
//! when its observed rate falls outside the table — to recover a
//! suitable initial playback delay.
//!
//! The box lives at file scope (not inside `moov`), and the spec
//! recommends it be placed as early as possible in the file for maximum
//! utility (§8.1.3.1).
//!
//! Layout per ISO/IEC 14496-12 §8.1.3.2:
//!
//! ```text
//! aligned(8) class ProgressiveDownloadInfoBox
//! extends FullBox('pdin', version = 0, 0) {
//!     for (i=0; ; i++) {                // to end of box
//!         unsigned int(32) rate;        // bytes/second
//!         unsigned int(32) initial_delay; // milliseconds
//!     }
//! }
//! ```
//!
//! QTFF does not define `pdin`; it is an ISO BMFF-only box.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// One `(rate, initial_delay)` pair from a Progressive Download
/// Information Box (ISO/IEC 14496-12 §8.1.3.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PdinEntry {
    /// Download rate in bytes/second.
    pub rate: u32,
    /// Suggested initial playback delay in milliseconds, such that if
    /// the download continues at `rate` bytes/sec, all data within the
    /// file will arrive in time for its use and playback should not
    /// need to stall.
    pub initial_delay: u32,
}

/// Parsed Progressive Download Information Box (ISO/IEC 14496-12
/// §8.1.3). The `entries` list is in the on-disk order; spec §8.1.3.3
/// does not require any particular ordering by `rate`, so we preserve
/// the writer's order rather than sorting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pdin {
    /// `(rate, initial_delay)` pairs in file order. Empty is legal —
    /// §8.1.3.2's `for (i=0; ; i++)` permits a zero-length list (any
    /// FullBox body of exactly 4 bytes — version + flags — and no
    /// trailing pairs).
    pub entries: Vec<PdinEntry>,
}

impl Pdin {
    /// Look up an initial-delay estimate for an observed `download_rate`
    /// (bytes/sec) by linear interpolation between the two surrounding
    /// entries, or by clamping to the first/last entry when
    /// `download_rate` falls outside the table's rate range.
    ///
    /// Per ISO/IEC 14496-12 §8.1.3.1: *"A receiving party can estimate
    /// the download rate it is experiencing, and from that obtain an
    /// upper estimate for a suitable initial delay by linear
    /// interpolation between pairs, or by extrapolation from the first
    /// or last entry."*
    ///
    /// The spec calls the result an *upper* estimate — i.e. waiting at
    /// least this long is safe. Returns `None` when the table is empty.
    ///
    /// Interpolation requires the table sorted ascending by `rate`;
    /// since §8.1.3.3 doesn't mandate ordering, this helper sorts a
    /// scratch copy of the rate axis once per call. For repeated
    /// queries against a stable `Pdin`, callers may want to cache a
    /// pre-sorted view.
    pub fn initial_delay_for(&self, download_rate: u32) -> Option<u32> {
        if self.entries.is_empty() {
            return None;
        }
        // Build a rate-sorted view of `entries` so the binary-search /
        // interpolation logic doesn't assume the writer emitted them
        // monotonically.
        let mut sorted: Vec<PdinEntry> = self.entries.clone();
        sorted.sort_by_key(|e| e.rate);

        // Clamp to first or last entry when the observed rate is
        // outside the table — §8.1.3.1 "extrapolation from the first
        // or last entry" is interpreted here as "use that endpoint's
        // initial_delay directly", which keeps the upper-estimate
        // promise (the lowest rate corresponds to the longest delay,
        // and the highest rate corresponds to the shortest).
        if download_rate <= sorted[0].rate {
            return Some(sorted[0].initial_delay);
        }
        if download_rate >= sorted[sorted.len() - 1].rate {
            return Some(sorted[sorted.len() - 1].initial_delay);
        }

        // Binary search for the bracketing pair `(lo, hi)` such that
        // `lo.rate <= download_rate < hi.rate`. `partition_point`
        // returns the index of the first entry with `rate >
        // download_rate`; the bracketing pair is `(idx-1, idx)`.
        let idx = sorted.partition_point(|e| e.rate <= download_rate);
        // idx is in `1..sorted.len()` thanks to the two clamp branches
        // above (download_rate strictly greater than sorted[0].rate
        // and strictly less than sorted[last].rate).
        let lo = sorted[idx - 1];
        let hi = sorted[idx];
        // Defensive: equal rates would divide by zero. The sort is
        // stable so duplicate-rate entries land adjacent; pick the
        // first one's delay (the spec is silent on duplicates).
        if hi.rate == lo.rate {
            return Some(lo.initial_delay);
        }
        // Linear interpolation: lower observed rates need longer
        // delays, higher rates need shorter ones — so we interpolate
        // on the `(rate, delay)` line directly. Promote to u64 for
        // intermediates so the product can't overflow.
        let span = (hi.rate - lo.rate) as u64;
        let pos = (download_rate - lo.rate) as u64;
        let d_lo = lo.initial_delay as i64;
        let d_hi = hi.initial_delay as i64;
        // `interp = d_lo + (d_hi - d_lo) * pos / span`, with the
        // multiplication promoted to i128 to keep the rounding step
        // exact for the full u32 range.
        let delta = d_hi - d_lo;
        let interp = d_lo + ((delta as i128 * pos as i128) / span as i128) as i64;
        // Clamp into u32 for the public return — both endpoints are
        // u32 and any in-range interpolation is bounded by them, so
        // this is defensive only.
        Some(interp.clamp(0, u32::MAX as i64) as u32)
    }
}

/// Parse a `pdin` payload.
///
/// Layout per ISO/IEC 14496-12 §8.1.3.2:
///
/// ```text
/// [version:1][flags:3]
/// (rate:4, initial_delay:4) × N             # N = (payload_len - 4) / 8
/// ```
///
/// Returns `Error::invalid` when:
/// * the payload is shorter than the 4-byte FullBox header,
/// * `version` is not 0 (§8.1.3.2 declares only version 0),
/// * the post-header body length is not a multiple of 8 (every entry
///   is exactly two `u32`s — a partial trailing entry indicates a
///   truncated box).
///
/// `flags` is parsed but not validated; the spec fixes it at 0 but
/// vendor extensions occasionally set bits we don't recognise, and
/// silently preserving them keeps round-trip parsers happy.
pub fn parse_pdin(payload: &[u8]) -> Result<Pdin> {
    if payload.len() < 4 {
        return Err(Error::invalid(format!(
            "MOV: pdin payload {} < 4-byte FullBox header",
            payload.len()
        )));
    }
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "MOV: pdin unknown version {version} (spec defines only v0)"
        )));
    }
    let body = &payload[4..];
    if body.len() % 8 != 0 {
        return Err(Error::invalid(format!(
            "MOV: pdin body {} bytes is not a multiple of 8",
            body.len()
        )));
    }
    let n = body.len() / 8;
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * 8;
        let rate = u32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
        let initial_delay =
            u32::from_be_bytes([body[off + 4], body[off + 5], body[off + 6], body[off + 7]]);
        entries.push(PdinEntry {
            rate,
            initial_delay,
        });
    }
    Ok(Pdin { entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_pdin(version: u8, pairs: &[(u32, u32)]) -> Vec<u8> {
        let mut p = Vec::with_capacity(4 + 8 * pairs.len());
        p.push(version);
        p.extend_from_slice(&[0, 0, 0]); // flags = 0
        for (rate, delay) in pairs {
            p.extend_from_slice(&rate.to_be_bytes());
            p.extend_from_slice(&delay.to_be_bytes());
        }
        p
    }

    #[test]
    fn parses_two_entries_in_file_order() {
        let p = build_pdin(0, &[(125_000, 2_000), (1_000_000, 250)]);
        let pdin = parse_pdin(&p).unwrap();
        assert_eq!(pdin.entries.len(), 2);
        assert_eq!(pdin.entries[0].rate, 125_000);
        assert_eq!(pdin.entries[0].initial_delay, 2_000);
        assert_eq!(pdin.entries[1].rate, 1_000_000);
        assert_eq!(pdin.entries[1].initial_delay, 250);
    }

    #[test]
    fn empty_table_is_legal() {
        // §8.1.3.2's `for (i=0; ; i++)` permits zero pairs — a 4-byte
        // FullBox body with no trailing data is valid.
        let p = build_pdin(0, &[]);
        let pdin = parse_pdin(&p).unwrap();
        assert!(pdin.entries.is_empty());
    }

    #[test]
    fn unknown_version_rejected() {
        let p = build_pdin(1, &[(1, 1)]);
        assert!(parse_pdin(&p).is_err());
    }

    #[test]
    fn truncated_header_rejected() {
        // 3 bytes — one short of the 4-byte FullBox header.
        let p = vec![0u8; 3];
        assert!(parse_pdin(&p).is_err());
    }

    #[test]
    fn partial_trailing_entry_rejected() {
        // Header + one complete pair + 4 extra bytes (a half-entry) —
        // the truncated tail must reject so callers can't silently
        // drop half a pair.
        let mut p = build_pdin(0, &[(500_000, 1_000)]);
        p.extend_from_slice(&[0u8; 4]);
        assert!(parse_pdin(&p).is_err());
    }

    #[test]
    fn interpolation_inside_bracket() {
        // Two entries at (100, 4000) and (200, 2000). Midpoint rate
        // 150 should land exactly halfway → delay 3000.
        let pdin = Pdin {
            entries: vec![
                PdinEntry {
                    rate: 100,
                    initial_delay: 4000,
                },
                PdinEntry {
                    rate: 200,
                    initial_delay: 2000,
                },
            ],
        };
        assert_eq!(pdin.initial_delay_for(150), Some(3000));
    }

    #[test]
    fn extrapolation_clamps_below_first_entry() {
        let pdin = Pdin {
            entries: vec![
                PdinEntry {
                    rate: 100,
                    initial_delay: 4000,
                },
                PdinEntry {
                    rate: 200,
                    initial_delay: 2000,
                },
            ],
        };
        // Observed rate 50 < min rate 100 → clamp to first entry's
        // delay (4000 — the longest, the upper-estimate per §8.1.3.1).
        assert_eq!(pdin.initial_delay_for(50), Some(4000));
    }

    #[test]
    fn extrapolation_clamps_above_last_entry() {
        let pdin = Pdin {
            entries: vec![
                PdinEntry {
                    rate: 100,
                    initial_delay: 4000,
                },
                PdinEntry {
                    rate: 200,
                    initial_delay: 2000,
                },
            ],
        };
        // Observed rate 1_000 > max rate 200 → clamp to last entry's
        // delay (2000 — the shortest).
        assert_eq!(pdin.initial_delay_for(1_000), Some(2000));
    }

    #[test]
    fn lookup_empty_table_is_none() {
        let pdin = Pdin {
            entries: Vec::new(),
        };
        assert_eq!(pdin.initial_delay_for(500_000), None);
    }

    #[test]
    fn unordered_writer_input_still_interpolates_correctly() {
        // Writer emits entries out of rate order; lookup must still
        // bracket on the rate-sorted view.
        let pdin = Pdin {
            entries: vec![
                PdinEntry {
                    rate: 1_000_000,
                    initial_delay: 200,
                },
                PdinEntry {
                    rate: 250_000,
                    initial_delay: 1_200,
                },
                PdinEntry {
                    rate: 500_000,
                    initial_delay: 600,
                },
            ],
        };
        // Bracket (500_000, 600) → (1_000_000, 200) at midpoint
        // 750_000 → delay 400.
        assert_eq!(pdin.initial_delay_for(750_000), Some(400));
    }

    #[test]
    fn exact_match_returns_exact_delay() {
        // partition_point on `e.rate <= 200` puts idx at 2 (past the
        // matching entry), so the bracket is (lo=200, hi=300) and the
        // interpolation reduces to lo's delay because pos == 0.
        let pdin = Pdin {
            entries: vec![
                PdinEntry {
                    rate: 100,
                    initial_delay: 5_000,
                },
                PdinEntry {
                    rate: 200,
                    initial_delay: 2_000,
                },
                PdinEntry {
                    rate: 300,
                    initial_delay: 800,
                },
            ],
        };
        assert_eq!(pdin.initial_delay_for(200), Some(2_000));
    }

    #[test]
    fn round_trip_through_parser() {
        let original = vec![(64_000, 8_000), (256_000, 2_000), (2_000_000, 200)];
        let bytes = build_pdin(0, &original);
        let pdin = parse_pdin(&bytes).unwrap();
        assert_eq!(pdin.entries.len(), 3);
        for (i, (rate, delay)) in original.iter().enumerate() {
            assert_eq!(pdin.entries[i].rate, *rate);
            assert_eq!(pdin.entries[i].initial_delay, *delay);
        }
    }
}
