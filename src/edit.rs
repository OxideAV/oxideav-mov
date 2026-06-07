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
    /// fixed-point rate (see [`Edit::rate_f64`]).
    ///
    /// Round 91 generalises [`media_pts_to_movie_pts`] to honour any
    /// strictly-positive `media_rate`: a 2.0× segment consumes twice as
    /// much media per movie tick (QTFF Chapter 5, "Playing With Edit
    /// Lists" p. 226–227 — `Track duration[1] = 600` at `Media rate[1]
    /// = 2.0` consumes 1200 media ticks worth of source frames). The
    /// unity-rate fast path remains identity-scaled.
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
/// any edit and is dropped from the presentation timeline).
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
/// duration in *media-time*. With unity rate the conversion is a pure
/// timescale ratio. With non-unity rate, the segment consumes
/// `rate × (movie_duration × media_ts / movie_ts)` media ticks per the
/// QTFF §"Playing With Edit Lists" example (p. 226–227: 600 movie
/// ticks at `Media rate = 2.0` consume 1200 media ticks). The forward
/// mapping inverts that: a media-time delta `Δm` inside the segment
/// translates to a movie-time delta `Δm × movie_ts / (media_ts ×
/// rate)`. Rate is 16.16 fixed-point so the arithmetic stays integer
/// — `Δmovie = Δmedia × movie_ts × 65536 / (media_ts × rate_fp)`.
///
/// Empty edits contribute movie-time gaps; dwell edits map the entire
/// segment to `media_time` (any `media_pts == media_time` lands at
/// `segment.movie_time_start`). Composition-shift segments (zero
/// duration, non-zero media_time) are skipped on a per-segment basis
/// but their `media_time_start` is honoured as the base for the
/// preceding sample if the following Media segment's media-time
/// matches it (see §8.6.6.1). Negative or zero `media_rate` on a
/// Media segment is rejected on a per-segment basis (QTFF p. 48
/// — "this rate value cannot be 0 or negative") and the scan
/// continues to the next segment.
///
/// Rounding for the half-step inside both the media-duration and the
/// movie-delta computation is half-up via `(num + denom/2) / denom`,
/// matching the convention already used in this module for the
/// timescale ratio. QTFF does not prescribe a rounding direction.
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
    const RATE_ONE: i128 = 0x0001_0000;
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
                // QTFF p. 48: `media_rate` "cannot be 0 or negative".
                // Reject those segments; continue scanning in case a
                // following segment matches.
                if media_rate <= 0 {
                    continue;
                }
                let rate_fp = media_rate as i128;
                let seg_dur_movie = seg.movie_time_end as i128 - seg.movie_time_start as i128;
                if seg_dur_movie < 0 {
                    continue;
                }
                // Equivalent media-time tick span consumed by this
                // movie-time slice at the segment's rate. With unity
                // rate this collapses to `seg_dur_movie * mds / mvs`.
                // With non-unity rate the QTFF example on p. 226–227
                // sets the convention: rate 2.0 doubles media
                // consumption, rate 0.5 halves it.
                let num = seg_dur_movie * mds * rate_fp;
                let denom = mvs * RATE_ONE;
                let seg_dur_media = (num + denom / 2) / denom;
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
                    // Δmovie = Δmedia × movie_ts × 65536 / (media_ts × rate_fp)
                    let num = delta_media * mvs * RATE_ONE;
                    let denom = mds * rate_fp;
                    let delta_movie = (num + denom / 2) / denom;
                    return Some(seg.movie_time_start as i64 + delta_movie as i64);
                }
            }
        }
    }
    None
}

/// Map a movie-timescale presentation timestamp (`movie_pts`) back
/// through the resolved edit segments to its corresponding
/// media-timescale presentation timestamp. The inverse of
/// [`media_pts_to_movie_pts`].
///
/// This is the "what media-sample do I need to play at movie-time T"
/// helper. Typical callers: seek-by-presentation-time on a movie whose
/// track carries a non-trivial edit list, and timeline UI surfaces
/// that scrub against the movie timeline but must drive the sample-
/// table walker keyed on media time.
///
/// Inputs:
/// * `segments` — output of [`resolve_edit_segments`].
/// * `movie_pts` — desired presentation timestamp in
///   movie-timescale ticks.
/// * `movie_timescale` — movie-timescale ticks-per-second (from
///   `mvhd.time_scale`).
/// * `media_timescale` — media-timescale ticks-per-second (from
///   `mdhd.time_scale`).
///
/// Algorithm: scan segments in order. For each segment whose half-open
/// `[movie_time_start, movie_time_end)` window contains `movie_pts`:
///
/// * [`EditSegmentKind::Empty`] — the movie-time slice has no media
///   correspondence (the player is meant to emit silence/black per
///   QTFF p. 47). Returns `None`.
/// * [`EditSegmentKind::Dwell`] — every movie-time tick inside the
///   segment maps to the same held media-time per ISO/IEC 14496-12
///   §8.6.6.3.
/// * [`EditSegmentKind::Media`] — convert the movie-time delta
///   `Δmovie = movie_pts − movie_time_start` to a media-time delta via
///   the QTFF Chapter 5 "Playing With Edit Lists" worked example
///   (p. 226–227): a 600-tick segment at rate 2.0 consumes 1200 media
///   ticks, so 1 movie tick at rate 2.0 advances the source by
///   2 media ticks. Generalising: `Δmedia = Δmovie × media_ts ×
///   rate_fp / (movie_ts × 65536)`. Rate is 16.16 fixed-point so the
///   arithmetic stays integer end-to-end.
///
/// `movie_pts` outside every segment window returns `None` — the
/// caller is asking for a movie-time that no edit covers (either past
/// `sum(track_duration)` and beyond any implicit trailing empty edit,
/// or — for negative values — before the timeline begins). When two
/// adjacent segments share a boundary tick (the half-open `[start,
/// end)` of segment N abuts the closed `start` of segment N+1) the
/// first matching segment wins, matching [`media_pts_to_movie_pts`]'s
/// declaration-order discipline.
///
/// Zero-duration Media segments (the §8.6.6.1 composition-shift
/// idiom) are inspected on a per-segment basis only when
/// `movie_pts == movie_time_start`; the resolved media tick is then
/// the segment's `media_time_start`. Zero-duration Empty and Dwell
/// segments are matched the same way: an Empty zero-segment at the
/// queried tick returns `None`; a Dwell zero-segment returns
/// `Some(media_time)`.
///
/// Per QTFF p. 48 a [`EditSegmentKind::Media`] segment with
/// `media_rate <= 0` is rejected on a per-segment basis (the spec
/// forbids both zero and negative rates), and scanning continues to
/// the next segment.
///
/// Rounding for the media-delta computation is half-up via
/// `(num + denom/2) / denom`, matching the convention used in
/// [`media_pts_to_movie_pts`] and in the timescale ratio elsewhere in
/// this module. QTFF does not prescribe a rounding direction.
pub fn movie_pts_to_media_pts(
    segments: &[EditSegment],
    movie_pts: i64,
    movie_timescale: u32,
    media_timescale: u32,
) -> Option<i64> {
    if media_timescale == 0 || movie_timescale == 0 {
        return None;
    }
    if movie_pts < 0 {
        return None;
    }
    let mvs = movie_timescale as i128;
    let mds = media_timescale as i128;
    const RATE_ONE: i128 = 0x0001_0000;
    let mpts = movie_pts as i128;
    for seg in segments {
        let start = seg.movie_time_start as i128;
        let end = seg.movie_time_end as i128;
        // Half-open membership: `mpts ∈ [start, end)`. Zero-duration
        // segments collapse the window to the single boundary tick
        // `mpts == start` and are handled below.
        let in_window = if start == end {
            mpts == start
        } else {
            mpts >= start && mpts < end
        };
        if !in_window {
            continue;
        }
        match seg.kind {
            EditSegmentKind::Empty => return None,
            EditSegmentKind::Dwell { media_time } => return Some(media_time as i64),
            EditSegmentKind::Media {
                media_time_start,
                media_rate,
            } => {
                // QTFF p. 48: media_rate "cannot be 0 or negative".
                // Reject those segments per-segment and continue
                // scanning in case a later segment also matches the
                // boundary tick.
                if media_rate <= 0 {
                    continue;
                }
                let rate_fp = media_rate as i128;
                let delta_movie = mpts - start;
                // Δmedia = Δmovie × media_ts × rate_fp / (movie_ts × 65536)
                let num = delta_movie * mds * rate_fp;
                let denom = mvs * RATE_ONE;
                let delta_media = (num + denom / 2) / denom;
                let media = media_time_start as i128 + delta_media;
                return Some(media as i64);
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

    // ─────────────── round 91: non-unity media_rate scaling ───────────────

    #[test]
    fn map_media_to_movie_pts_double_speed_segment_consumes_double_media() {
        // QTFF p. 226–227 worked example: 600 movie ticks at media_rate
        // 2.0 with movie_ts=600 / media_ts=100 consumes 200 media ticks
        // (1 second of source → ½ second of movie). Map a couple of
        // media_pts inside that window.
        let edits = vec![Edit {
            track_duration: 600, // 1.0 s @ movie_ts 600
            media_time: 0,
            media_rate: 0x0002_0000, // 2.0
        }];
        let segs = resolve_edit_segments(&edits, None);
        // media_pts 0 lands at movie 0.
        assert_eq!(media_pts_to_movie_pts(&segs, 0, 600, 100), Some(0));
        // media_pts 100 (= 1 s of source) lands at movie 300 (= ½ s).
        assert_eq!(media_pts_to_movie_pts(&segs, 100, 600, 100), Some(300));
        // media_pts 199 (last consumed media tick) maps inside.
        assert_eq!(media_pts_to_movie_pts(&segs, 199, 600, 100), Some(597));
        // media_pts 200 is past the consumed window → None.
        assert_eq!(media_pts_to_movie_pts(&segs, 200, 600, 100), None);
    }

    #[test]
    fn map_media_to_movie_pts_half_speed_segment_consumes_half_media() {
        // Half-speed: 600 movie ticks at media_rate 0.5 consumes 50
        // media ticks (½ s of source stretched over 1 s of movie time).
        let edits = vec![Edit {
            track_duration: 600,
            media_time: 0,
            media_rate: 0x0000_8000, // 0.5
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(media_pts_to_movie_pts(&segs, 0, 600, 100), Some(0));
        // media_pts 25 (= ¼ s of source) lands at movie 300 (= ½ s of
        // movie time).
        assert_eq!(media_pts_to_movie_pts(&segs, 25, 600, 100), Some(300));
        // media_pts 49 is the last consumed media tick.
        assert_eq!(media_pts_to_movie_pts(&segs, 49, 600, 100), Some(588));
        // media_pts 50 is past the consumed window.
        assert_eq!(media_pts_to_movie_pts(&segs, 50, 600, 100), None);
    }

    #[test]
    fn map_media_to_movie_pts_three_segment_qtff_example_roundtrip() {
        // The full QTFF p. 226–227 example: 3 segments totalling 6000
        // movie ticks, two 600-tick double-speed runs followed by a
        // 4800-tick unity-rate tail starting at media_time 200.
        let edits = vec![
            Edit {
                track_duration: 600,
                media_time: 0,
                media_rate: 0x0002_0000,
            },
            Edit {
                track_duration: 600,
                media_time: 0,
                media_rate: 0x0002_0000,
            },
            Edit {
                track_duration: 4800,
                media_time: 200,
                media_rate: 0x0001_0000,
            },
        ];
        let segs = resolve_edit_segments(&edits, None);
        // Within segment[0]: 200 media ticks consumed across 600 movie
        // ticks. media_pts 0..199 maps into [0, 600). The first
        // matching segment wins, so media_pts 0 → movie 0 (not 600).
        assert_eq!(media_pts_to_movie_pts(&segs, 0, 600, 100), Some(0));
        assert_eq!(media_pts_to_movie_pts(&segs, 100, 600, 100), Some(300));
        // Segment[2] is unity rate starting at media_time 200; covers
        // 4800 movie ticks → 800 media ticks. media_pts 200 lands at
        // segment start = 600 + 600 = 1200.
        assert_eq!(media_pts_to_movie_pts(&segs, 200, 600, 100), Some(1200));
        // media_pts 1000 = 800 ticks into segment[2] → end of segment.
        // We treat the window as half-open so 1000 is just outside.
        assert_eq!(media_pts_to_movie_pts(&segs, 1000, 600, 100), None);
        // media_pts 999 lands one movie tick shy of segment end.
        assert_eq!(media_pts_to_movie_pts(&segs, 999, 600, 100), Some(5994));
    }

    #[test]
    fn map_media_to_movie_pts_rejects_negative_or_zero_rate_on_media_segment() {
        // media_rate = 0 with non-empty media_time is dwell (handled
        // elsewhere). Construct a Media segment manually with rate 0
        // and -1.0 and confirm both are rejected per QTFF p. 48.
        let segs = vec![
            EditSegment {
                movie_time_start: 0,
                movie_time_end: 100,
                kind: EditSegmentKind::Media {
                    media_time_start: 0,
                    media_rate: 0, // forbidden by QTFF
                },
            },
            EditSegment {
                movie_time_start: 100,
                movie_time_end: 200,
                kind: EditSegmentKind::Media {
                    media_time_start: 0,
                    media_rate: -0x0001_0000, // -1.0, forbidden
                },
            },
        ];
        assert_eq!(media_pts_to_movie_pts(&segs, 50, 100, 100), None);
    }

    #[test]
    fn map_media_to_movie_pts_double_speed_with_initial_offset() {
        // Double-speed segment after a 100-tick empty edit. Same
        // 600/100 timescales; media segment runs 200 media ticks.
        let edits = vec![
            Edit {
                track_duration: 100,
                media_time: -1,
                media_rate: 0x0001_0000,
            },
            Edit {
                track_duration: 600,
                media_time: 0,
                media_rate: 0x0002_0000,
            },
        ];
        let segs = resolve_edit_segments(&edits, None);
        // media_pts 0 lands at movie 100 (after the empty edit).
        assert_eq!(media_pts_to_movie_pts(&segs, 0, 600, 100), Some(100));
        // media_pts 100 lands at movie 100 + 300 = 400.
        assert_eq!(media_pts_to_movie_pts(&segs, 100, 600, 100), Some(400));
    }

    // ─────────── round 246: inverse movie_pts → media_pts mapper ───────────

    #[test]
    fn map_movie_to_media_pts_initial_empty_edit_returns_none_inside_empty() {
        // 100-tick empty edit then 500-tick unity-rate Media @ media_time 0.
        // movie_pts inside the empty window is rejected; inside the Media
        // window maps back to media_pts.
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
        // movie_pts 50 sits inside the Empty segment.
        assert_eq!(movie_pts_to_media_pts(&segs, 50, 600, 600), None);
        // movie_pts 100 lands at the start of the Media segment.
        assert_eq!(movie_pts_to_media_pts(&segs, 100, 600, 600), Some(0));
        // movie_pts 150 lands 50 ticks into the Media segment.
        assert_eq!(movie_pts_to_media_pts(&segs, 150, 600, 600), Some(50));
        // movie_pts 599 lands at the last consumed tick.
        assert_eq!(movie_pts_to_media_pts(&segs, 599, 600, 600), Some(499));
        // movie_pts 600 sits past the end of every segment.
        assert_eq!(movie_pts_to_media_pts(&segs, 600, 600, 600), None);
    }

    #[test]
    fn map_movie_to_media_pts_rescales_timescales() {
        // Movie timescale 1000, media timescale 90_000. Single 1000-tick
        // Media segment starting at media_time 0. movie_pts 500 (= 0.5 s)
        // should map to media_pts 45_000.
        let edits = vec![Edit {
            track_duration: 1_000,
            media_time: 0,
            media_rate: 0x0001_0000,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(
            movie_pts_to_media_pts(&segs, 500, 1_000, 90_000),
            Some(45_000)
        );
    }

    #[test]
    fn map_movie_to_media_pts_segment_with_nonzero_media_offset() {
        // Single 100-tick Media segment pulling media[200..300). movie_pts
        // 0 lands at media 200; movie_pts 50 at 250; movie_pts 99 at 299.
        let edits = vec![Edit {
            track_duration: 100,
            media_time: 200,
            media_rate: 0x0001_0000,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(movie_pts_to_media_pts(&segs, 0, 100, 100), Some(200));
        assert_eq!(movie_pts_to_media_pts(&segs, 50, 100, 100), Some(250));
        assert_eq!(movie_pts_to_media_pts(&segs, 99, 100, 100), Some(299));
        assert_eq!(movie_pts_to_media_pts(&segs, 100, 100, 100), None);
    }

    #[test]
    fn map_movie_to_media_pts_resolves_composition_shift_zero_segment() {
        // §8.6.6.1 composition-shift idiom: a zero-duration Media
        // segment at media_time 20 followed by a 100-tick Media segment
        // at the same media_time. movie_pts 0 matches the zero-segment
        // first (declaration order wins) and resolves to media 20.
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
        assert_eq!(movie_pts_to_media_pts(&segs, 0, 100, 100), Some(20));
        // movie_pts 50 falls past the zero-segment and into the 100-tick
        // Media segment; lands at media 70.
        assert_eq!(movie_pts_to_media_pts(&segs, 50, 100, 100), Some(70));
    }

    #[test]
    fn map_movie_to_media_pts_dwell_returns_held_media_time() {
        // ISO/IEC 14496-12 §8.6.6.3 dwell: every movie-time tick in the
        // segment maps to the same held media_time.
        let edits = vec![Edit {
            track_duration: 600,
            media_time: 12_000,
            media_rate: 0,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(movie_pts_to_media_pts(&segs, 0, 600, 90_000), Some(12_000));
        // Mid-segment dwell still resolves to 12_000.
        assert_eq!(
            movie_pts_to_media_pts(&segs, 300, 600, 90_000),
            Some(12_000)
        );
        assert_eq!(
            movie_pts_to_media_pts(&segs, 599, 600, 90_000),
            Some(12_000)
        );
        // Past segment end → None.
        assert_eq!(movie_pts_to_media_pts(&segs, 600, 600, 90_000), None);
    }

    #[test]
    fn map_movie_to_media_pts_double_speed_consumes_double_media() {
        // QTFF p. 226–227 worked example: 600 movie ticks at media_rate
        // 2.0 with movie_ts=600 / media_ts=100 consumes 200 media ticks.
        // Inverse mapping: movie_pts 300 (= ½ the segment) lands at
        // media_pts 100 (= half of 200 media ticks).
        let edits = vec![Edit {
            track_duration: 600,
            media_time: 0,
            media_rate: 0x0002_0000,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(movie_pts_to_media_pts(&segs, 0, 600, 100), Some(0));
        assert_eq!(movie_pts_to_media_pts(&segs, 300, 600, 100), Some(100));
        // Last consumed movie tick (599) lands one media tick shy of
        // 200 — Δmovie 599 × 100 × 2 / (600 × 65536) × 65536 → 599×200/600
        // = 199.67 → half-up rounding → 200. Note this is the only place
        // where the half-up rounding pushes us past the half-open window
        // on the media side; the segment still owns the tick because the
        // movie-side window check came first.
        assert_eq!(movie_pts_to_media_pts(&segs, 599, 600, 100), Some(200));
        assert_eq!(movie_pts_to_media_pts(&segs, 600, 600, 100), None);
    }

    #[test]
    fn map_movie_to_media_pts_half_speed_consumes_half_media() {
        // Half-speed: 600 movie ticks at media_rate 0.5 consumes 50
        // media ticks. movie_pts 300 lands at media 25.
        let edits = vec![Edit {
            track_duration: 600,
            media_time: 0,
            media_rate: 0x0000_8000,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(movie_pts_to_media_pts(&segs, 0, 600, 100), Some(0));
        assert_eq!(movie_pts_to_media_pts(&segs, 300, 600, 100), Some(25));
        assert_eq!(movie_pts_to_media_pts(&segs, 600, 600, 100), None);
    }

    #[test]
    fn map_movie_to_media_pts_three_segment_qtff_example() {
        // The full QTFF p. 226–227 example: 3 segments totalling 6000
        // movie ticks, two 600-tick double-speed runs followed by a
        // 4800-tick unity-rate tail starting at media_time 200.
        let edits = vec![
            Edit {
                track_duration: 600,
                media_time: 0,
                media_rate: 0x0002_0000,
            },
            Edit {
                track_duration: 600,
                media_time: 0,
                media_rate: 0x0002_0000,
            },
            Edit {
                track_duration: 4800,
                media_time: 200,
                media_rate: 0x0001_0000,
            },
        ];
        let segs = resolve_edit_segments(&edits, None);
        // Segment[0]: movie 0 → media 0.
        assert_eq!(movie_pts_to_media_pts(&segs, 0, 600, 100), Some(0));
        // Segment[0] mid: movie 300 → media 100.
        assert_eq!(movie_pts_to_media_pts(&segs, 300, 600, 100), Some(100));
        // Segment[1] start: movie 600 → media 0 (re-plays from start).
        assert_eq!(movie_pts_to_media_pts(&segs, 600, 600, 100), Some(0));
        // Segment[1] mid: movie 900 → media 100.
        assert_eq!(movie_pts_to_media_pts(&segs, 900, 600, 100), Some(100));
        // Segment[2] start: movie 1200 → media 200.
        assert_eq!(movie_pts_to_media_pts(&segs, 1200, 600, 100), Some(200));
        // Segment[2] mid: movie 3000 → 1800 ticks in at unity rate;
        // 1800 movie ticks × 100/600 = 300 media ticks. media_time_start
        // 200 + 300 = 500.
        assert_eq!(movie_pts_to_media_pts(&segs, 3000, 600, 100), Some(500));
        // End of timeline: movie 6000 sits outside every segment.
        assert_eq!(movie_pts_to_media_pts(&segs, 6000, 600, 100), None);
    }

    #[test]
    fn map_movie_to_media_pts_rejects_negative_movie_pts() {
        // The presentation timeline starts at movie tick 0; negative
        // movie_pts always returns None regardless of segment shape.
        let edits = vec![Edit {
            track_duration: 100,
            media_time: 0,
            media_rate: 0x0001_0000,
        }];
        let segs = resolve_edit_segments(&edits, None);
        assert_eq!(movie_pts_to_media_pts(&segs, -1, 100, 100), None);
        assert_eq!(movie_pts_to_media_pts(&segs, i64::MIN, 100, 100), None);
    }

    #[test]
    fn map_movie_to_media_pts_rejects_zero_timescale() {
        let segs = vec![EditSegment {
            movie_time_start: 0,
            movie_time_end: 100,
            kind: EditSegmentKind::Media {
                media_time_start: 0,
                media_rate: 0x0001_0000,
            },
        }];
        assert_eq!(movie_pts_to_media_pts(&segs, 50, 0, 100), None);
        assert_eq!(movie_pts_to_media_pts(&segs, 50, 100, 0), None);
    }

    #[test]
    fn map_movie_to_media_pts_rejects_zero_or_negative_rate_on_media_segment() {
        // QTFF p. 48 forbids 0 / negative media_rate on a Media segment.
        // Hand-construct two such segments and confirm both are rejected.
        let segs = vec![
            EditSegment {
                movie_time_start: 0,
                movie_time_end: 100,
                kind: EditSegmentKind::Media {
                    media_time_start: 0,
                    media_rate: 0,
                },
            },
            EditSegment {
                movie_time_start: 100,
                movie_time_end: 200,
                kind: EditSegmentKind::Media {
                    media_time_start: 0,
                    media_rate: -0x0001_0000,
                },
            },
        ];
        assert_eq!(movie_pts_to_media_pts(&segs, 50, 100, 100), None);
        assert_eq!(movie_pts_to_media_pts(&segs, 150, 100, 100), None);
    }

    #[test]
    fn map_movie_to_media_pts_roundtrips_with_forward_mapper_on_unity_rate() {
        // For every Media segment with rate 1.0 and matching timescales,
        // forward followed by inverse should round-trip exactly. Sample
        // a few media_pts values across two segments.
        let edits = vec![
            Edit {
                track_duration: 100,
                media_time: -1,
                media_rate: 0x0001_0000,
            },
            Edit {
                track_duration: 500,
                media_time: 1_000,
                media_rate: 0x0001_0000,
            },
        ];
        let segs = resolve_edit_segments(&edits, None);
        for &media in &[1_000i64, 1_100, 1_250, 1_499] {
            let movie = media_pts_to_movie_pts(&segs, media, 600, 600).unwrap();
            assert_eq!(
                movie_pts_to_media_pts(&segs, movie, 600, 600),
                Some(media),
                "round-trip failed for media_pts {media}"
            );
        }
    }
}
