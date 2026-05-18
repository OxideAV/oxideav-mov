//! Edit-list (`edts` / `elst`) parsing.
//!
//! QTFF Chapter 2, "Edit Atoms" (pp. 46–48). The edit list maps
//! presentation time in the movie's timeline to media time in the
//! track's media. Three quirks make this non-trivial:
//!
//! 1. A `media_time` of -1 marks an *empty edit* — the player should
//!    insert silence/black for the edit's `track_duration` before
//!    consuming any media. The last edit must never be empty (QTFF
//!    p. 47, "Media time").
//! 2. A `track_duration` of 0 with a non-empty `media_time` is a no-op
//!    pause — some authoring tools emit it. We accept it.
//! 3. `media_rate` is a 16.16 signed fixed-point number; the spec
//!    forbids 0 or negative (QTFF p. 48, "Media rate"), but we accept
//!    them as parsed and let the caller decide whether to reject.
//!
//! ISO BMFF (ISO/IEC 14496-12) defines the same atom plus a v1 layout
//! with 64-bit `track_duration` + 64-bit `media_time`. We parse both.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// A single edit list entry.
///
/// `media_time = -1` marks an *empty* edit (per QTFF p. 47); the
/// helper [`Edit::is_empty`] surfaces that condition. `track_duration`
/// is in *movie* timescale units; `media_time` is in *media* timescale
/// units (mdhd.time_scale).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    /// Duration of this edit in movie-timescale units.
    pub track_duration: u64,
    /// Starting position in the media. Negative values indicate an
    /// empty edit; we store as `i64` so v0 sentinels survive.
    pub media_time: i64,
    /// 16.16 signed fixed-point relative playback rate. 0x0001_0000
    /// (= 1.0) is normal speed.
    pub media_rate: i32,
}

impl Edit {
    /// True for an *empty edit* (QTFF p. 47): the player should emit
    /// `track_duration` of fill before any media is consumed.
    pub fn is_empty(&self) -> bool {
        self.media_time < 0
    }

    /// True for a *dwell* edit (ISO/IEC 14496-12 §8.6.6.3): a single
    /// media frame at `media_time` is held for `track_duration` of
    /// movie time. The on-wire signal is `media_rate == 0`. QTFF p. 48
    /// declares 0 illegal but ISO BMFF permits it — we surface the
    /// classification either way; players that don't model dwell can
    /// treat it as a normal segment at rate 1.0.
    pub fn is_dwell(&self) -> bool {
        !self.is_empty() && self.media_rate == 0
    }

    /// 16.16 fixed-point `media_rate` represented as the floating-point
    /// rate it encodes. 0x0001_0000 → 1.0, 0x0002_0000 → 2.0, etc.
    pub fn rate_f64(&self) -> f64 {
        (self.media_rate as f64) / 65_536.0
    }
}

/// A track's edit list — the ordered sequence of [`Edit`] entries.
pub type EditList = Vec<Edit>;

/// A resolved edit segment after walking an [`EditList`] and assigning
/// movie-time bounds to each entry. Use [`resolve_edit_segments`] or
/// [`crate::Track::edit_segments`] to obtain a sequence of these from a
/// parsed list.
///
/// Each segment maps a half-open movie-time interval
/// `[movie_time_start, movie_time_end)` (in *movie timescale ticks*)
/// onto a section of the track's media according to [`EditSegmentKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditSegment {
    /// Inclusive lower bound, in movie-timescale ticks.
    pub movie_time_start: u64,
    /// Exclusive upper bound, in movie-timescale ticks. For
    /// zero-duration segments (the §8.6.6.1 composition-shift idiom)
    /// equals `movie_time_start`.
    pub movie_time_end: u64,
    /// What the segment maps to.
    pub kind: EditSegmentKind,
}

impl EditSegment {
    /// Segment duration in movie-timescale ticks.
    pub fn duration(&self) -> u64 {
        self.movie_time_end.saturating_sub(self.movie_time_start)
    }
}

/// Classification of an [`EditSegment`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditSegmentKind {
    /// Empty edit (QTFF p. 47 / ISO/IEC 14496-12 §8.6.6.3): black /
    /// silence inserted into the movie-time line for the segment's
    /// duration. No media is consumed.
    Empty,
    /// Dwell (ISO/IEC 14496-12 §8.6.6.3): hold a single media frame
    /// at `media_time` for the segment duration. Signalled on-wire by
    /// `media_rate == 0`.
    Dwell {
        /// Media-timescale tick to hold.
        media_time: u64,
    },
    /// Normal media playback. `media_time_start` is the media-timescale
    /// tick the segment begins at; `media_rate` is the on-wire 16.16
    /// fixed-point rate (see [`Edit::rate_f64`]). Round 74 only
    /// guarantees correct `media_to_movie_pts` mapping for
    /// `media_rate == 0x0001_0000` (1.0); other rates are surfaced but
    /// the mapper falls back to the unscaled identity.
    Media {
        /// Inclusive media-timescale start tick.
        media_time_start: u64,
        /// 16.16 fixed-point relative playback rate.
        media_rate: i32,
    },
}

/// Parse an `elst` payload.
///
/// Layout per QTFF Figure 2-11 (p. 47): `[ver+flags=4][n=4]` followed
/// by `n × {track_duration, media_time, media_rate}` triples; entry
/// width is 12 for version-0, 20 for version-1 (ISO BMFF extension).
pub fn parse_elst(payload: &[u8]) -> Result<EditList> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: elst payload < 8 bytes"));
    }
    let version = payload[0];
    let n = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let body = &payload[8..];
    let entry_size = match version {
        0 => 12usize,
        1 => 20usize,
        v => return Err(Error::invalid(format!("MOV: elst unknown version {v}"))),
    };
    let need = (n as usize)
        .checked_mul(entry_size)
        .ok_or_else(|| Error::invalid("MOV: elst entry count overflow"))?;
    if body.len() < need {
        return Err(Error::invalid("MOV: elst truncated table"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        let off = i * entry_size;
        let edit = match version {
            0 => Edit {
                track_duration: u32::from_be_bytes([
                    body[off],
                    body[off + 1],
                    body[off + 2],
                    body[off + 3],
                ]) as u64,
                // media_time is signed (sentinel value -1 = empty).
                media_time: i32::from_be_bytes([
                    body[off + 4],
                    body[off + 5],
                    body[off + 6],
                    body[off + 7],
                ]) as i64,
                media_rate: i32::from_be_bytes([
                    body[off + 8],
                    body[off + 9],
                    body[off + 10],
                    body[off + 11],
                ]),
            },
            _ => Edit {
                track_duration: u64::from_be_bytes([
                    body[off],
                    body[off + 1],
                    body[off + 2],
                    body[off + 3],
                    body[off + 4],
                    body[off + 5],
                    body[off + 6],
                    body[off + 7],
                ]),
                media_time: i64::from_be_bytes([
                    body[off + 8],
                    body[off + 9],
                    body[off + 10],
                    body[off + 11],
                    body[off + 12],
                    body[off + 13],
                    body[off + 14],
                    body[off + 15],
                ]),
                media_rate: i32::from_be_bytes([
                    body[off + 16],
                    body[off + 17],
                    body[off + 18],
                    body[off + 19],
                ]),
            },
        };
        out.push(edit);
    }
    Ok(out)
}

/// Resolve a parsed [`EditList`] into a sequence of [`EditSegment`]s
/// stamped with absolute movie-time bounds.
///
/// QTFF / ISO BMFF idioms handled:
///
/// * **Empty edits** (`media_time < 0`) become [`EditSegmentKind::Empty`]
///   slots that consume movie time without referencing media.
/// * **Dwell** (`media_rate == 0`, non-empty `media_time`) becomes
///   [`EditSegmentKind::Dwell`] per ISO/IEC 14496-12 §8.6.6.3.
/// * **Zero-duration edits with non-zero media_time** survive as
///   zero-length [`EditSegmentKind::Media`] segments — the
///   composition-shift idiom from §8.6.6.1 ("Particularly in an empty
///   initial movie of a fragmented movie file…"). The mapper treats
///   them as a presentation-time-base offset for the following segment.
/// * **Implicit trailing empty edit** (QTFF p. 47 / §8.6.6.3): if
///   `movie_duration` is supplied and exceeds the cumulative
///   `track_duration` of the parsed edits, an extra
///   [`EditSegmentKind::Empty`] segment is appended to fill the gap.
///
/// The bounds returned are *cumulative* — segment N's
/// `movie_time_start` equals the sum of all preceding
/// `track_duration`s, regardless of segment kind. The first segment
/// always begins at `0`.
///
/// Pass `movie_duration = None` to skip the implicit trailing-empty
/// computation (useful when the caller doesn't have an `mvhd`
/// available, e.g. when working directly with `parse_elst` output).
pub fn resolve_edit_segments(edits: &EditList, movie_duration: Option<u64>) -> Vec<EditSegment> {
    let mut out = Vec::with_capacity(edits.len() + 1);
    let mut cursor: u64 = 0;
    for e in edits {
        let end = cursor.saturating_add(e.track_duration);
        let kind = if e.is_empty() {
            EditSegmentKind::Empty
        } else if e.is_dwell() {
            EditSegmentKind::Dwell {
                media_time: e.media_time as u64,
            }
        } else {
            EditSegmentKind::Media {
                media_time_start: e.media_time as u64,
                media_rate: e.media_rate,
            }
        };
        out.push(EditSegment {
            movie_time_start: cursor,
            movie_time_end: end,
            kind,
        });
        cursor = end;
    }
    // Implicit trailing empty edit per QTFF p. 47 / §8.6.6.3.
    if let Some(total) = movie_duration {
        if cursor < total {
            out.push(EditSegment {
                movie_time_start: cursor,
                movie_time_end: total,
                kind: EditSegmentKind::Empty,
            });
        }
    }
    out
}

/// Map a media-timescale presentation timestamp (`media_pts`) through
/// the resolved edit segments to its corresponding movie-timescale
/// presentation timestamp. Returns `None` when the media-time falls
/// outside every non-empty segment (the sample is not referenced by
/// any edit and is dropped from the presentation timeline) or when
/// the segment lookup hits a non-1.0 media_rate that the round-74
/// mapper doesn't model.
///
/// Inputs:
/// * `segments` — output of [`resolve_edit_segments`].
/// * `media_pts` — sample PTS in media-timescale ticks.
/// * `movie_timescale` — movie-timescale ticks-per-second
///   (from `mvhd.time_scale`).
/// * `media_timescale` — media-timescale ticks-per-second
///   (from `mdhd.time_scale`).
///
/// Algorithm: scan segments in order. For each
/// [`EditSegmentKind::Media`], express its movie-time start and
/// duration in *media-time* (via `segment.movie_time_end -
/// segment.movie_time_start` rescaled by
/// `media_timescale / movie_timescale`), test whether `media_pts`
/// falls within `[media_time_start, media_time_start +
/// segment_media_duration)`, and if so return
/// `segment.movie_time_start + (media_pts - media_time_start) *
/// movie_timescale / media_timescale`.
///
/// Empty edits contribute movie-time gaps; dwell edits map the entire
/// segment to `media_time` (any `media_pts == media_time` lands at
/// `segment.movie_time_start`). Composition-shift segments (zero
/// duration, non-zero media_time) are skipped on a per-segment basis
/// but their `media_time_start` is honoured as the base for the
/// preceding sample if the following Media segment's media-time
/// matches it (see §8.6.6.1).
pub fn media_pts_to_movie_pts(
    segments: &[EditSegment],
    media_pts: i64,
    movie_timescale: u32,
    media_timescale: u32,
) -> Option<i64> {
    if media_timescale == 0 || movie_timescale == 0 {
        return None;
    }
    let mvs = movie_timescale as i128;
    let mds = media_timescale as i128;
    for seg in segments {
        match seg.kind {
            EditSegmentKind::Empty => continue,
            EditSegmentKind::Dwell { media_time } => {
                if media_pts as i128 == media_time as i128 {
                    return Some(seg.movie_time_start as i64);
                }
            }
            EditSegmentKind::Media {
                media_time_start,
                media_rate,
            } => {
                // Round-74 mapper only models rate=1.0; surface a
                // best-effort identity-scaled mapping otherwise (see
                // module docs).
                if media_rate != 0x0001_0000 && media_rate != 0 {
                    // continue scanning — a downstream segment may
                    // still match at unity rate.
                    continue;
                }
                let seg_dur_movie = seg.movie_time_end as i128 - seg.movie_time_start as i128;
                if seg_dur_movie < 0 {
                    continue;
                }
                // Convert the segment's movie-time duration into the
                // equivalent media-time tick span. Round nearest-even
                // (banker's rounding via simple half-up).
                let seg_dur_media = (seg_dur_movie * mds + mvs / 2) / mvs;
                let media_start = media_time_start as i128;
                let media_end = media_start + seg_dur_media;
                // Zero-duration segments are the §8.6.6.1
                // composition-shift idiom — they reference `media_pts
                // == media_time_start` exclusively.
                if seg_dur_media == 0 {
                    if media_pts as i128 == media_start {
                        return Some(seg.movie_time_start as i64);
                    }
                    continue;
                }
                if (media_pts as i128) >= media_start && (media_pts as i128) < media_end {
                    let delta_media = media_pts as i128 - media_start;
                    let delta_movie = (delta_media * mvs + mds / 2) / mds;
                    return Some(seg.movie_time_start as i64 + delta_movie as i64);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_elst_v0(entries: &[(u32, i32, i32)]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
        p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (dur, mt, rate) in entries {
            p.extend_from_slice(&dur.to_be_bytes());
            p.extend_from_slice(&mt.to_be_bytes());
            p.extend_from_slice(&rate.to_be_bytes());
        }
        p
    }

    #[test]
    fn empty_edit_marker_recognised() {
        // Empty edit (mt = -1) followed by a 200-tick segment at media 0.
        let p = build_elst_v0(&[(100, -1, 0x0001_0000), (200, 0, 0x0001_0000)]);
        let v = parse_elst(&p).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].is_empty());
        assert_eq!(v[0].track_duration, 100);
        assert!(!v[1].is_empty());
        assert_eq!(v[1].media_time, 0);
        assert_eq!(v[1].media_rate, 0x0001_0000);
    }

    #[test]
    fn version1_entries_widen_to_64bit() {
        // ver=1, one entry: dur=u64::MAX-1, mt=-2, rate=0x10000
        let mut p = Vec::new();
        p.push(1); // version
        p.extend_from_slice(&[0, 0, 0]); // flags
        p.extend_from_slice(&1u32.to_be_bytes());
        let dur: u64 = u64::MAX - 1;
        p.extend_from_slice(&dur.to_be_bytes());
        let mt: i64 = -2;
        p.extend_from_slice(&mt.to_be_bytes());
        let rate: i32 = 0x0001_0000;
        p.extend_from_slice(&rate.to_be_bytes());
        let v = parse_elst(&p).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].track_duration, dur);
        assert_eq!(v[0].media_time, mt);
        assert!(v[0].is_empty());
    }

    #[test]
    fn truncated_table_errors() {
        // Declares 4 entries but only carries 2 worth of bytes.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&4u32.to_be_bytes());
        p.extend_from_slice(&[0u8; 24]); // 2 entries
        assert!(parse_elst(&p).is_err());
    }

    #[test]
    fn unknown_version_errors() {
        let mut p = Vec::new();
        p.push(2); // bad version
        p.extend_from_slice(&[0, 0, 0]);
        p.extend_from_slice(&0u32.to_be_bytes());
        assert!(parse_elst(&p).is_err());
    }

    // ─────────────── round 74: EditSegment resolver ───────────────

    #[test]
    fn resolve_segments_assigns_cumulative_movie_time_bounds() {
        // Empty 100 movie-ticks, then 200 movie-ticks of media @ media_time 0,
        // then 50 movie-ticks of media @ media_time 600.
        let edits = vec![
            Edit {
                track_duration: 100,
                media_time: -1,
                media_rate: 0x0001_0000,
            },
            Edit {
                track_duration: 200,
                media_time: 0,
                media_rate: 0x0001_0000,
            },
            Edit {
                track_duration: 50,
                media_time: 600,
                media_rate: 0x0001_0000,
            },
        ];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].movie_time_start, 0);
        assert_eq!(segs[0].movie_time_end, 100);
        assert_eq!(segs[0].kind, EditSegmentKind::Empty);
        assert_eq!(segs[1].movie_time_start, 100);
        assert_eq!(segs[1].movie_time_end, 300);
        assert!(matches!(
            segs[1].kind,
            EditSegmentKind::Media {
                media_time_start: 0,
                media_rate: 0x0001_0000
            }
        ));
        assert_eq!(segs[2].movie_time_start, 300);
        assert_eq!(segs[2].movie_time_end, 350);
        assert!(matches!(
            segs[2].kind,
            EditSegmentKind::Media {
                media_time_start: 600,
                ..
            }
        ));
    }

    #[test]
    fn resolve_segments_appends_implicit_trailing_empty_when_short() {
        // Single 100-tick edit but mvhd says 250 — implicit trailing
        // empty edit covers the [100, 250) gap (QTFF p. 47).
        let edits = vec![Edit {
            track_duration: 100,
            media_time: 0,
            media_rate: 0x0001_0000,
        }];
        let segs = resolve_edit_segments(&edits, Some(250));
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1].kind, EditSegmentKind::Empty);
        assert_eq!(segs[1].movie_time_start, 100);
        assert_eq!(segs[1].movie_time_end, 250);
    }

    #[test]
    fn resolve_segments_skips_trailing_implicit_when_movie_duration_zero_or_match() {
        // edits sum to exactly movie_duration → no implicit segment.
        let edits = vec![Edit {
            track_duration: 250,
            media_time: 0,
            media_rate: 0x0001_0000,
        }];
        let segs = resolve_edit_segments(&edits, Some(250));
        assert_eq!(segs.len(), 1);
        assert!(matches!(segs[0].kind, EditSegmentKind::Media { .. }));
    }

    #[test]
    fn resolve_segments_recognises_dwell_when_rate_zero() {
        let edits = vec![Edit {
            track_duration: 600,
            media_time: 12_000,
            media_rate: 0,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].kind, EditSegmentKind::Dwell { media_time: 12_000 });
    }

    #[test]
    fn map_media_to_movie_pts_with_initial_empty_edit_shifts_by_segment() {
        // Movie timescale 600, media timescale 600 (1:1). 100-tick empty
        // edit followed by media @ media_time 0. media_pts 50 should
        // land at movie_pts 150 (100 empty + 50 in).
        let edits = vec![
            Edit {
                track_duration: 100,
                media_time: -1,
                media_rate: 0x0001_0000,
            },
            Edit {
                track_duration: 500,
                media_time: 0,
                media_rate: 0x0001_0000,
            },
        ];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(media_pts_to_movie_pts(&segs, 50, 600, 600), Some(150));
        // PTS at the start of the media segment lands at movie-time 100.
        assert_eq!(media_pts_to_movie_pts(&segs, 0, 600, 600), Some(100));
        // PTS past the segment's end window returns None.
        assert_eq!(media_pts_to_movie_pts(&segs, 600, 600, 600), None);
    }

    #[test]
    fn map_media_to_movie_pts_rescales_timescales() {
        // Movie timescale 1000, media timescale 90_000. 1000-tick movie
        // duration of media starting at media_time 0. A 90_000-tick
        // sample at media_pts 45_000 (= 0.5 s) should land at movie
        // tick 500.
        let edits = vec![Edit {
            track_duration: 1_000,
            media_time: 0,
            media_rate: 0x0001_0000,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(
            media_pts_to_movie_pts(&segs, 45_000, 1_000, 90_000),
            Some(500)
        );
    }

    #[test]
    fn map_media_to_movie_pts_drops_samples_outside_any_edit() {
        // Single 100-tick edit pulling [media 200, 300). media_pts < 200
        // and media_pts >= 300 are dropped.
        let edits = vec![Edit {
            track_duration: 100,
            media_time: 200,
            media_rate: 0x0001_0000,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(media_pts_to_movie_pts(&segs, 150, 100, 100), None);
        assert_eq!(media_pts_to_movie_pts(&segs, 250, 100, 100), Some(50));
        assert_eq!(media_pts_to_movie_pts(&segs, 300, 100, 100), None);
    }

    #[test]
    fn map_media_to_movie_pts_resolves_composition_shift_zero_segment() {
        // §8.6.6.1 composition-shift idiom: zero-duration segment with
        // non-zero media_time. media_pts equal to that media_time maps
        // to the segment's movie_time_start (= 0 here).
        let edits = vec![
            Edit {
                track_duration: 0,
                media_time: 20,
                media_rate: 0x0001_0000,
            },
            Edit {
                track_duration: 100,
                media_time: 20,
                media_rate: 0x0001_0000,
            },
        ];
        let segs = resolve_edit_segments(&edits, None);
        // media_pts 20 lands at movie 0 via the zero-length segment.
        assert_eq!(media_pts_to_movie_pts(&segs, 20, 100, 100), Some(0));
    }

    #[test]
    fn map_media_to_movie_pts_dwell_only_matches_at_held_time() {
        let edits = vec![Edit {
            track_duration: 600,
            media_time: 12_000,
            media_rate: 0,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(media_pts_to_movie_pts(&segs, 12_000, 600, 90_000), Some(0));
        assert_eq!(media_pts_to_movie_pts(&segs, 12_001, 600, 90_000), None);
    }

    #[test]
    fn rate_f64_decodes_16_16_fixed_point() {
        let e = Edit {
            track_duration: 0,
            media_time: 0,
            media_rate: 0x0002_8000, // 2.5
        };
        assert!((e.rate_f64() - 2.5).abs() < 1e-9);
    }
}
