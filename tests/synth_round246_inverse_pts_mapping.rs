//! Round 246 — inverse edit-list mapper end-to-end via `MovDemuxer`.
//!
//! Forward direction: a sample's media-PTS → movie-PTS. The new
//! [`oxideav_mov::MovDemuxer::media_pts_for`] inverts that: a user
//! requesting "what media sample is at movie-time T" gets the right
//! media-PTS back, honouring the QTFF Chapter 2 edit-list semantics
//! (empty edits, dwell, the §8.6.6.1 composition shift, and the
//! Chapter 5 worked-example rate scaling on pp. 226–227).
//!
//! These tests stand on the round-91 synth scaffold (`build_qt`)
//! parameterised over movie / media timescales + on-wire edit list
//! and validate the inverse direction against the same QTFF
//! worked-example numbers used to validate the forward mapper.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

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

/// Mirror of the round-91 `build_qt` scaffold — 1 video track with
/// caller-supplied timescales and edit list.
fn build_qt(
    movie_ts: u32,
    media_ts: u32,
    mvhd_dur: u32,
    mdhd_dur: u32,
    elst_entries: &[(u32, i32, i32)],
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
        push_atom(&mut moov, *b"mvhd", &build_mvhd(movie_ts, mvhd_dur));
        let mut trak = Vec::new();
        push_atom(
            &mut trak,
            *b"tkhd",
            &build_tkhd_flags(1, mvhd_dur, 320, 240, 0x07, 0),
        );
        if !elst_entries.is_empty() {
            let mut edts = Vec::new();
            push_atom(&mut edts, *b"elst", &build_elst_v0(elst_entries));
            push_atom(&mut trak, *b"edts", &edts);
        }
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(media_ts, mdhd_dur));
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
fn media_pts_for_unity_rate_single_segment_round_trips_with_forward() {
    // movie_ts=600, media_ts=600, single 600-tick unity edit starting
    // at media_time 0. Every (media_pts, movie_pts) pair should
    // round-trip cleanly through the forward/inverse pair.
    let bytes = build_qt(600, 600, 600, 600, &[(600, 0, 0x0001_0000)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open unity edit");

    for &mpts in &[0i64, 100, 250, 599] {
        let movie = d.movie_pts_for(0, mpts).expect("forward");
        assert_eq!(
            d.media_pts_for(0, movie),
            Some(mpts),
            "round-trip failed for media_pts {mpts}"
        );
    }
}

#[test]
fn media_pts_for_double_speed_segment_returns_double_media_per_movie_tick() {
    // QTFF p. 226–227 worked example mirrored on the inverse side.
    // 600-tick movie at media_rate 2.0 consumes 200 media ticks.
    let bytes = build_qt(600, 100, 600, 200, &[(600, 0, 0x0002_0000)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open 2.0× edit");

    assert_eq!(d.media_pts_for(0, 0), Some(0));
    // Mid-segment: movie 300 → media 100 (half of consumed 200).
    assert_eq!(d.media_pts_for(0, 300), Some(100));
    // Past the segment: movie 600 → None.
    assert_eq!(d.media_pts_for(0, 600), None);
    // Before the timeline starts: negative → None.
    assert_eq!(d.media_pts_for(0, -1), None);
}

#[test]
fn media_pts_for_empty_edit_returns_none_inside_silence_window() {
    // 100-tick empty edit then a 500-tick unity-rate segment. The
    // empty window covers movie [0, 100); inside it media-PTS is
    // undefined and the helper returns None.
    let bytes = build_qt(
        600,
        600,
        600,
        500,
        &[(100, -1, 0x0001_0000), (500, 0, 0x0001_0000)],
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open empty+unity");

    assert_eq!(d.media_pts_for(0, 50), None);
    assert_eq!(d.media_pts_for(0, 100), Some(0));
    assert_eq!(d.media_pts_for(0, 250), Some(150));
    assert_eq!(d.media_pts_for(0, 599), Some(499));
    assert_eq!(d.media_pts_for(0, 600), None);
}

#[test]
fn media_pts_for_dwell_returns_held_media_time_across_segment() {
    // ISO/IEC 14496-12 §8.6.6.3 dwell: media_rate=0 holds a single
    // media tick over the whole movie-time window. Every queried
    // movie_pts inside the window resolves to the same media_time.
    let bytes = build_qt(600, 90_000, 600, 12_001, &[(600, 12_000, 0)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open dwell");

    assert_eq!(d.media_pts_for(0, 0), Some(12_000));
    assert_eq!(d.media_pts_for(0, 300), Some(12_000));
    assert_eq!(d.media_pts_for(0, 599), Some(12_000));
    assert_eq!(d.media_pts_for(0, 600), None);
}

#[test]
fn media_pts_for_returns_none_for_missing_track_or_missing_mvhd() {
    // Real fixture — out-of-range track index returns None without
    // touching the mvhd.
    let bytes = build_qt(600, 600, 600, 600, &[(600, 0, 0x0001_0000)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open unity edit");
    assert_eq!(d.media_pts_for(99, 100), None);
}

#[test]
fn media_pts_for_full_qtff_three_segment_example_resolves_each_segment() {
    // The 3-edit QTFF p. 226–227 layout — same fixture as the forward
    // round-91 test; query inverse points landing inside each of the
    // three segments.
    let bytes = build_qt(
        600,
        100,
        6000,
        1000,
        &[
            (600, 0, 0x0002_0000),
            (600, 0, 0x0002_0000),
            (4800, 200, 0x0001_0000),
        ],
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open 3-edit QTFF example");

    // Segment[0]: movie 300 → media 100.
    assert_eq!(d.media_pts_for(0, 300), Some(100));
    // Segment[1] start: movie 600 → media 0 (re-plays).
    assert_eq!(d.media_pts_for(0, 600), Some(0));
    // Segment[2] start: movie 1200 → media 200.
    assert_eq!(d.media_pts_for(0, 1200), Some(200));
    // Segment[2] mid: movie 3000 → media 500.
    assert_eq!(d.media_pts_for(0, 3000), Some(500));
    // Past the timeline: movie 6000 → None.
    assert_eq!(d.media_pts_for(0, 6000), None);
}
