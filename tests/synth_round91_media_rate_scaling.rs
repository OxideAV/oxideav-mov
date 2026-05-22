//! Round 91 — non-unity `media_rate` scaling in the QTFF edit-list
//! mapper.
//!
//! The QTFF p. 226–227 "Playing With Edit Lists" example shows two
//! 600-tick movie-time edits set to `Media rate = 2.0` consuming
//! 200 media ticks each. Round 74 stopped at unity rate; round 91
//! generalises [`oxideav_mov::EditSegmentKind::Media`] mapping to
//! arbitrary positive 16.16 fixed-point rates.
//!
//! Validators in this file are pure synth fixtures (no `ffmpeg`
//! dependency) — the QTFF worked example fully constrains both
//! sides of the math.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{EditSegmentKind, MovDemuxer};

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

/// Build a 1-track QT file with caller-supplied `movie_ts` / `media_ts`,
/// movie duration, media duration, and edit list. Frame count = 4 with
/// constant 30-tick stts entries (matches r74 synth scaffold).
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
fn movie_pts_for_double_speed_segment_compresses_media_into_half_movie_time() {
    // QTFF p. 226–227 worked example: movie_ts=600, media_ts=100,
    // 600-tick edit at media_rate=2.0 → 200 media ticks consumed,
    // mapped onto 600 movie ticks.
    let bytes = build_qt(600, 100, 600, 200, &[(600, 0, 0x0002_0000)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open 2.0× edit");

    // media_pts 0 → movie 0
    assert_eq!(d.movie_pts_for(0, 0), Some(0));
    // media_pts 100 (half of consumed 200) → movie 300 (half of 600)
    assert_eq!(d.movie_pts_for(0, 100), Some(300));
    // media_pts 199 (last consumed tick) → movie 597
    assert_eq!(d.movie_pts_for(0, 199), Some(597));
    // media_pts 200 is past the consumed window → None
    assert_eq!(d.movie_pts_for(0, 200), None);

    // The EditSegment surface reports the on-wire rate verbatim.
    let segs = d.edit_segments_for(0).unwrap();
    assert_eq!(segs.len(), 1);
    match segs[0].kind {
        EditSegmentKind::Media {
            media_time_start,
            media_rate,
        } => {
            assert_eq!(media_time_start, 0);
            assert_eq!(media_rate, 0x0002_0000);
        }
        other => panic!("expected Media kind, got {other:?}"),
    }
}

#[test]
fn movie_pts_for_half_speed_segment_stretches_media_over_double_movie_time() {
    // Half-speed: movie_ts=600, media_ts=100, 600-tick edit at rate
    // 0.5 → 50 media ticks consumed across 600 movie ticks.
    let bytes = build_qt(600, 100, 600, 200, &[(600, 0, 0x0000_8000)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open 0.5× edit");

    assert_eq!(d.movie_pts_for(0, 0), Some(0));
    // media_pts 25 (half of 50) → movie 300 (half of 600)
    assert_eq!(d.movie_pts_for(0, 25), Some(300));
    // media_pts 49 (last consumed) → movie 588
    assert_eq!(d.movie_pts_for(0, 49), Some(588));
    // media_pts 50 is past the consumed window → None
    assert_eq!(d.movie_pts_for(0, 50), None);
}

#[test]
fn movie_pts_for_full_qtff_three_segment_example_round_trips() {
    // QTFF p. 226–227 final layout: 2×600 movie-tick edits at rate 2.0
    // followed by a 4800 movie-tick unity-rate tail at media_time 200.
    // Movie ts = 600, media ts = 100, mdhd duration covers all media
    // referenced (200 + 200 of the two double-speed runs would
    // collide on the same media range, but QTFF intentionally has
    // both replay the same [0, 200) twice — see worked example).
    let bytes = build_qt(
        600,
        100,
        6000, // total movie duration
        1000, // media duration: 0..1000 covers segment[2]'s 200..1000 + earlier
        &[
            (600, 0, 0x0002_0000),    // 600 movie ticks at 2.0× → media [0, 200)
            (600, 0, 0x0002_0000),    // same media range, replayed
            (4800, 200, 0x0001_0000), // 4800 movie ticks unity → media [200, 1000)
        ],
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open 3-edit QTFF example");

    // media_pts 0 matches segment[0] first (scan order); maps to 0.
    assert_eq!(d.movie_pts_for(0, 0), Some(0));
    // media_pts 100 lands in segment[0] at movie 300.
    assert_eq!(d.movie_pts_for(0, 100), Some(300));
    // media_pts 200 lands at segment[2] start = 1200.
    assert_eq!(d.movie_pts_for(0, 200), Some(1200));
    // media_pts 999 is the last unity-rate tick → movie 5994.
    assert_eq!(d.movie_pts_for(0, 999), Some(5994));

    let segs = d.edit_segments_for(0).unwrap();
    assert_eq!(segs.len(), 3);
    assert_eq!(segs[0].movie_time_end, 600);
    assert_eq!(segs[1].movie_time_end, 1200);
    assert_eq!(segs[2].movie_time_end, 6000);
}

#[test]
fn movie_pts_for_double_speed_after_empty_edit_shifts_correctly() {
    // 100-tick empty edit followed by a 600-tick 2.0× media segment.
    let bytes = build_qt(
        600,
        100,
        700,
        200,
        &[(100, -1, 0x0001_0000), (600, 0, 0x0002_0000)],
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open empty+2.0×");

    // media_pts 0 lands at movie 100 (after the empty edit).
    assert_eq!(d.movie_pts_for(0, 0), Some(100));
    // media_pts 100 lands at movie 100 + 300 = 400.
    assert_eq!(d.movie_pts_for(0, 100), Some(400));
    // media_pts 200 is past the consumed window → None.
    assert_eq!(d.movie_pts_for(0, 200), None);
}
