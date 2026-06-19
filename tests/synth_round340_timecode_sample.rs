//! Round-340 acceptance: end-to-end **Timecode Sample Data** decoding
//! (QTFF p. 108).
//!
//! A timecode track's `mdat` samples are NOT codec frames — each is a
//! 4-byte payload that is either a 32-bit tape-counter value (Counter
//! flag set in the `tmcd` sample description) or a packed `[H:M:S:F]`
//! record. `MovDemuxer::timecode_sample(track, idx)` reads the sample
//! bytes via the random-access offset path and decodes them against the
//! owning `tmcd` description. These tests build real movies and assert
//! the decoded values round-trip.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, TimecodeRecord, TimecodeSample, TMCD_FLAG_COUNTER};

/// Build a two-track movie: a video track (id 1) carrying a `tref/tmcd`
/// pointing at a timecode track (id 2). The timecode track has a single
/// record-format sample = `start`.
fn build_video_plus_timecode(start: [u8; 4], number_of_frames: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // mdat: [video 4 bytes][timecode 4 bytes].
    let mut mdat_body = Vec::new();
    mdat_body.extend_from_slice(b"VID!");
    mdat_body.extend_from_slice(&start);
    let mdat_payload_off = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &mdat_body);
    let video_off = mdat_payload_off;
    let tc_off = mdat_payload_off + 4;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(30000, 30));

    // Track 1: video, tref/tmcd -> 2.
    let mut trak1 = Vec::new();
    push_atom(&mut trak1, *b"tkhd", &build_tkhd(1, 30, 320, 240));
    let mut tref = Vec::new();
    push_atom(&mut tref, *b"tmcd", &2u32.to_be_bytes());
    push_atom(&mut trak1, *b"tref", &tref);
    let mut mdia1 = Vec::new();
    push_atom(&mut mdia1, *b"mdhd", &build_mdhd(30000, 30));
    push_atom(&mut mdia1, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf1 = Vec::new();
    push_atom(&mut minf1, *b"vmhd", &build_vmhd());
    let mut stbl1 = Vec::new();
    push_atom(
        &mut stbl1,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl1, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl1, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl1, *b"stsz", &build_stsz_constant(4, 1));
    push_atom(&mut stbl1, *b"stco", &build_stco_single(video_off));
    push_atom(&mut minf1, *b"stbl", &stbl1);
    push_atom(&mut mdia1, *b"minf", &minf1);
    push_atom(&mut trak1, *b"mdia", &mdia1);
    push_atom(&mut moov, *b"trak", &trak1);

    // Track 2: timecode.
    let mut trak2 = Vec::new();
    push_atom(&mut trak2, *b"tkhd", &build_tkhd(2, 30, 0, 0));
    let mut mdia2 = Vec::new();
    push_atom(&mut mdia2, *b"mdhd", &build_mdhd(30000, 30));
    push_atom(&mut mdia2, *b"hdlr", &build_hdlr(b"mhlr", b"tmcd"));
    let mut minf2 = Vec::new();
    let mut stbl2 = Vec::new();
    push_atom(&mut stbl2, *b"stsd", &build_stsd_tmcd(0, number_of_frames));
    push_atom(&mut stbl2, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl2, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl2, *b"stsz", &build_stsz_constant(4, 1));
    push_atom(&mut stbl2, *b"stco", &build_stco_single(tc_off));
    push_atom(&mut minf2, *b"stbl", &stbl2);
    push_atom(&mut mdia2, *b"minf", &minf2);
    push_atom(&mut trak2, *b"mdia", &mdia2);
    push_atom(&mut moov, *b"trak", &trak2);

    push_atom(&mut out, *b"moov", &moov);
    out
}

/// Build a `tmcd` sample-description `stsd` body with the given flags
/// and frames-per-second; no trailing source reference.
fn build_stsd_tmcd(flags: u32, number_of_frames: u8) -> Vec<u8> {
    let mut tmcd_body = Vec::new();
    tmcd_body.extend_from_slice(&0u32.to_be_bytes()); // reserved
    tmcd_body.extend_from_slice(&flags.to_be_bytes());
    tmcd_body.extend_from_slice(&30000u32.to_be_bytes()); // time_scale
    tmcd_body.extend_from_slice(&1001u32.to_be_bytes()); // frame_duration
    tmcd_body.push(number_of_frames);
    tmcd_body.extend_from_slice(&[0u8; 3]); // reserved 24-bit

    let entry_size: u32 = (16 + tmcd_body.len()) as u32;
    let mut stsd = Vec::new();
    stsd.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    stsd.extend_from_slice(&1u32.to_be_bytes()); // entry count
    stsd.extend_from_slice(&entry_size.to_be_bytes());
    stsd.extend_from_slice(b"tmcd");
    stsd.extend_from_slice(&[0u8; 6]); // reserved
    stsd.extend_from_slice(&1u16.to_be_bytes()); // dref index
    stsd.extend_from_slice(&tmcd_body);
    stsd
}

/// Assemble a one-track timecode movie carrying `samples` 4-byte
/// timecode payloads laid out contiguously in `mdat`.
fn build_timecode_movie(flags: u32, number_of_frames: u8, samples: &[[u8; 4]]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mut mdat_body = Vec::new();
    for s in samples {
        mdat_body.extend_from_slice(s);
    }
    let mdat_payload_off = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &mdat_body);

    let n = samples.len() as u32;
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(30000, 30 * n));

    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30 * n, 0, 0));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(30000, 30 * n));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"tmcd"));
    let mut minf = Vec::new();
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_tmcd(flags, number_of_frames),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(n, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(n)); // all in one chunk
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(4, n));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_off));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn timecode_record_samples_decode_end_to_end() {
    // Three records: 00:00:00:00, 00:00:10:05, 01:23:45:17 at 30fps.
    let samples = [[0u8, 0, 0, 0], [0, 0, 10, 5], [1, 23, 45, 17]];
    let bytes = build_timecode_movie(0, 30, &samples);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open timecode fixture");
    assert!(d.tracks[0].is_timecode());

    let s0 = d.timecode_sample(0, 0).unwrap().expect("sample 0");
    assert_eq!(
        s0,
        TimecodeSample::Record(TimecodeRecord {
            negative: false,
            hours: 0,
            minutes: 0,
            seconds: 0,
            frames: 0,
        })
    );

    let s1 = d.timecode_sample(0, 1).unwrap().expect("sample 1");
    match s1 {
        TimecodeSample::Record(r) => {
            assert_eq!((r.hours, r.minutes, r.seconds, r.frames), (0, 0, 10, 5));
            assert_eq!(r.to_frames(30), Some(305));
        }
        other => panic!("expected record, got {other:?}"),
    }

    let s2 = d.timecode_sample(0, 2).unwrap().expect("sample 2");
    match s2 {
        TimecodeSample::Record(r) => {
            assert_eq!((r.hours, r.minutes, r.seconds, r.frames), (1, 23, 45, 17));
        }
        other => panic!("expected record, got {other:?}"),
    }
}

#[test]
fn timecode_counter_samples_decode_end_to_end() {
    let samples = [0x0000_0000u32, 0x0000_0001, 0x0001_2345, 0xDEAD_BEEF].map(|v| v.to_be_bytes());
    let bytes = build_timecode_movie(TMCD_FLAG_COUNTER, 1, &samples);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open counter fixture");

    for (i, raw) in [0u32, 1, 0x0001_2345, 0xDEAD_BEEF].iter().enumerate() {
        let s = d.timecode_sample(0, i as u32).unwrap().expect("counter");
        assert_eq!(s, TimecodeSample::Counter(*raw));
    }
}

#[test]
fn timecode_sample_out_of_range_is_none() {
    let samples = [[0u8, 0, 0, 0]];
    let bytes = build_timecode_movie(0, 30, &samples);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open");
    assert!(d.timecode_sample(0, 99).unwrap().is_none());
    // Track index out of range.
    assert!(d.timecode_sample(5, 0).unwrap().is_none());
}

#[test]
fn non_timecode_track_returns_none() {
    // A plain video track has no `tmcd` description.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let mdat_payload_off = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", b"VID!");

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 60));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 60, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 60));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 60));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(4, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_off));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let mut d = MovDemuxer::open(cur).expect("open video");
    assert!(!d.tracks[0].is_timecode());
    assert!(d.timecode_sample(0, 0).unwrap().is_none());
}

#[test]
fn start_timecode_resolves_through_tref_from_video() {
    // QTFF p. 224 worked value: 0x010F2004 = 01:15:32:04.
    let start = 0x010F_2004u32.to_be_bytes();
    let bytes = build_video_plus_timecode(start, 30);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open");

    // Asked about the *video* track (index 0): resolves to timecode
    // track index 1 via tref/tmcd.
    let st = d.start_timecode(0).unwrap().expect("start timecode");
    assert_eq!(st.timecode_track_index, 1);
    assert_eq!(st.number_of_frames, 30);
    assert!(!st.drop_frame);
    assert_eq!(
        st.sample,
        TimecodeSample::Record(TimecodeRecord {
            negative: false,
            hours: 1,
            minutes: 15,
            seconds: 32,
            frames: 4,
        })
    );
}

#[test]
fn start_timecode_on_timecode_track_itself() {
    let start = 0x010F_2004u32.to_be_bytes();
    let bytes = build_video_plus_timecode(start, 30);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open");
    // Asked directly about the timecode track (index 1).
    let st = d.start_timecode(1).unwrap().expect("start timecode");
    assert_eq!(st.timecode_track_index, 1);
    match st.sample {
        TimecodeSample::Record(r) => {
            assert_eq!((r.hours, r.minutes, r.seconds, r.frames), (1, 15, 32, 4));
        }
        other => panic!("expected record, got {other:?}"),
    }
}

#[test]
fn start_timecode_none_without_reference() {
    // A plain timecode movie's video-less single track resolves on
    // itself; but a movie with no timecode track at all returns None.
    let samples = [[0u8, 0, 0, 0]];
    let bytes = build_timecode_movie(0, 30, &samples);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open");
    // Track 0 IS the timecode track here, so it resolves on itself.
    assert!(d.start_timecode(0).unwrap().is_some());
    // Out-of-range track index.
    assert!(d.start_timecode(9).unwrap().is_none());
}
