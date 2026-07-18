//! Round 74 — edit-list (`edts/elst`) presentation-time honour.
//!
//! Builds synth QT files that exercise the round-74 mapping API:
//!
//! * `MovDemuxer::movie_pts_for(track, media_pts)` — translates a
//!   sample's media-timescale PTS to its movie-timescale PTS by walking
//!   the edit list (QTFF pp. 46–48 / ISO/IEC 14496-12 §8.6.5 / §8.6.6).
//! * `MovDemuxer::edit_segments_for(track)` — surfaces the resolved
//!   movie-time-bounded edit segments (including the implicit trailing
//!   empty edit when `sum(elst.track_duration) < mvhd.duration`).
//! * `Track::is_enabled` / `participates_in_movie` / `alternate_group`
//!   — `tkhd.flags` and alt-group surface.
//! * `MovDemuxer::presentation_tracks()` / `alternate_groups()` —
//!   convenience iterators that fold those `tkhd` bits into the
//!   selection / grouping a downstream player needs.
//!
//! All fixtures are hand-built in-memory (no `tests/fixtures/` files);
//! round-2 already lands the `elst` parser + ctts mapping, this round
//! adds the *presentation-time mapping* layer on top.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{EditSegment, EditSegmentKind, MovDemuxer};

/// Helper: build a v0 `elst` payload from `(track_dur, media_time, media_rate)` triples.
fn build_elst_v0(entries: &[(u32, i32, i32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (dur, mt, rate) in entries {
        p.extend_from_slice(&dur.to_be_bytes());
        p.extend_from_slice(&mt.to_be_bytes());
        p.extend_from_slice(&rate.to_be_bytes());
    }
    p
}

/// Build a 1-track QT file: `movie_timescale=600`, `media_timescale=600`,
/// 4 samples of 30-tick duration each (total 120 media ticks). The
/// caller supplies the elst, mvhd duration, tkhd flags, and alt_group.
fn build_qt(
    elst_entries: &[(u32, i32, i32)],
    mvhd_dur: u32,
    tkhd_flags: u8,
    alt_group: i16,
) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  ");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, mvhd_dur));
        let mut trak = Vec::new();
        push_atom(
            &mut trak,
            *b"tkhd",
            &build_tkhd_flags(1, mvhd_dur, 320, 240, tkhd_flags, alt_group),
        );
        if !elst_entries.is_empty() {
            let mut edts = Vec::new();
            push_atom(&mut edts, *b"elst", &build_elst_v0(elst_entries));
            push_atom(&mut trak, *b"edts", &edts);
        }
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 120));
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"vmhd", &build_vmhd());
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_video(b"avc1", 320, 240, &[]),
        );
        push_atom(&mut stbl, *b"stts", &build_stts_single(4, 30));
        push_atom(&mut stbl, *b"stsc", &build_stsc_single(4));
        push_atom(&mut stbl, *b"stsz", &build_stsz_constant(2, 4));
        push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

#[test]
fn movie_pts_for_initial_empty_edit_shifts_media_pts() {
    // 100-tick empty + 120-tick media @ 0. Track duration = 220.
    let bytes = build_qt(
        &[(100, -1, 0x0001_0000), (120, 0, 0x0001_0000)],
        220,
        0x07,
        0,
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open with initial empty edit");

    // media_pts 0 lands at movie-time 100 (after the 100-tick empty edit).
    assert_eq!(d.movie_pts_for(0, 0), Some(100));
    // media_pts 30 lands at 130, 60 at 160, 90 at 190.
    assert_eq!(d.movie_pts_for(0, 30), Some(130));
    assert_eq!(d.movie_pts_for(0, 90), Some(190));
    // media_pts 120 falls outside the media segment ([0, 120)) → None.
    assert_eq!(d.movie_pts_for(0, 120), None);
}

#[test]
fn movie_pts_for_no_elst_is_identity_rescaled() {
    // No elst at all. mvhd duration 120 (movie ts 600), mdhd ts 600,
    // mdhd duration 120 — the synthetic single segment covers
    // [0, 120) at rate 1.0.
    let bytes = build_qt(&[], 120, 0x07, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open no-elst");

    assert_eq!(d.movie_pts_for(0, 0), Some(0));
    assert_eq!(d.movie_pts_for(0, 60), Some(60));
    assert_eq!(d.movie_pts_for(0, 119), Some(119));
    assert_eq!(d.movie_pts_for(0, 120), None);
}

#[test]
fn edit_segments_for_appends_implicit_trailing_empty_edit() {
    // elst declares 100 ticks of media @ 0 (track_dur=100), but mvhd
    // says the movie runs 250 ticks total. Spec rule (QTFF p. 47 /
    // §8.6.6.3): the remaining [100, 250) is an *implicit* trailing
    // empty edit.
    let bytes = build_qt(&[(100, 0, 0x0001_0000)], 250, 0x07, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open elst < mvhd");

    let segs = d
        .edit_segments_for(0)
        .expect("track 0 has edit segments resolved");
    assert_eq!(segs.len(), 2, "got {segs:?}");
    assert!(matches!(segs[0].kind, EditSegmentKind::Media { .. }));
    assert_eq!(segs[0].movie_time_start, 0);
    assert_eq!(segs[0].movie_time_end, 100);
    assert_eq!(segs[1].kind, EditSegmentKind::Empty);
    assert_eq!(segs[1].movie_time_start, 100);
    assert_eq!(segs[1].movie_time_end, 250);
}

#[test]
fn edit_segments_for_no_elst_yields_synthetic_full_track_media_segment() {
    let bytes = build_qt(&[], 120, 0x07, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open no-elst");
    let segs = d.edit_segments_for(0).unwrap();
    assert_eq!(segs.len(), 1);
    assert!(matches!(
        segs[0],
        EditSegment {
            movie_time_start: 0,
            movie_time_end: _,
            kind: EditSegmentKind::Media {
                media_time_start: 0,
                media_rate: 0x0001_0000,
            },
        }
    ));
    // mdhd duration is 120 in media ts 600; movie ts also 600 → 120.
    assert_eq!(segs[0].movie_time_end, 120);
}

#[test]
fn movie_pts_for_returns_none_for_out_of_range_track() {
    let bytes = build_qt(&[], 120, 0x07, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open");
    assert_eq!(d.movie_pts_for(99, 0), None);
}

#[test]
fn track_flags_surface_enabled_in_movie_in_preview_in_poster() {
    // 0x0F = enabled (0x01) | in_movie (0x02) | in_preview (0x04) | in_poster (0x08)
    let bytes = build_qt(&[], 120, 0x0F, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open");
    let t = &d.tracks[0];
    assert!(t.is_enabled());
    assert!(t.participates_in_movie());
    assert!(t.participates_in_preview());
    assert!(t.participates_in_poster());
}

#[test]
fn track_flags_disabled_track_excluded_from_presentation() {
    // Disabled (flags = 0).
    let bytes = build_qt(&[], 120, 0x00, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open disabled");
    let t = &d.tracks[0];
    assert!(!t.is_enabled());
    assert!(!t.participates_in_movie());
    let pres: Vec<_> = d.presentation_tracks().collect();
    assert!(
        pres.is_empty(),
        "disabled track must not appear in presentation_tracks()"
    );
}

#[test]
fn alternate_groups_groups_tracks_by_tkhd_alt_group() {
    // Single track with alt_group = 7 — `alternate_groups()` should
    // return one entry keyed at 7.
    let bytes = build_qt(&[], 120, 0x07, 7);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open alt group");
    assert_eq!(d.tracks[0].alternate_group(), 7);
    let groups = d.alternate_groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0, 7);
    assert_eq!(groups[0].1, vec![0]);
}

#[test]
fn dwell_edit_maps_only_the_held_media_time_to_segment_start() {
    // 600-tick dwell on media_time 60 (media_rate=0). Per §8.6.6.3
    // the player holds the single frame at media_time 60 for the
    // segment's duration. Map: only media_pts == 60 lands in the
    // segment; other media_pts return None.
    let bytes = build_qt(&[(600, 60, 0)], 600, 0x07, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open dwell");
    assert_eq!(d.movie_pts_for(0, 60), Some(0));
    assert_eq!(d.movie_pts_for(0, 30), None);
    assert_eq!(d.movie_pts_for(0, 90), None);
    let segs = d.edit_segments_for(0).unwrap();
    assert_eq!(segs.len(), 1);
    assert!(matches!(
        segs[0].kind,
        EditSegmentKind::Dwell { media_time: 60 }
    ));
}

// ─────────── round 417: typed elst surface on demux ───────────

#[test]
fn elst_version_and_summary_accessors_populate_on_demux() {
    // 100-tick empty edit + 120-tick media @ media_time 30.
    let bytes = build_qt(
        &[(100, -1, 0x0001_0000), (120, 30, 0x0001_0000)],
        220,
        0x07,
        0,
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open with edits");
    let t = &d.tracks[0];
    // The on-wire elst was version 0, and its presence is recorded.
    assert_eq!(t.elst_version, Some(0));
    // Leading empty edit = 100 movie ticks of start delay.
    assert_eq!(t.edit_start_delay(), 100);
    // First presenting edit trims media before tick 30.
    assert_eq!(t.edit_media_start(), Some(30));
    // Total declared presentation span.
    assert_eq!(t.edit_total_duration(), 220);
}

#[test]
fn elst_version_none_when_no_edts_atom() {
    let bytes = build_qt(&[], 120, 0x07, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open no-elst");
    let t = &d.tracks[0];
    assert_eq!(t.elst_version, None);
    assert_eq!(t.edit_start_delay(), 0);
    assert_eq!(t.edit_media_start(), None);
    assert_eq!(t.edit_total_duration(), 0);
}
