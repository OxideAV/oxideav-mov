//! Seek-to-keyframe acceptance tests for `MovDemuxer`.
//!
//! Exercises the QTFF "Finding a Sample" algorithm (pp. 79–80) against
//! ffmpeg-generated fixtures. The implementation lives in
//! `src/demuxer.rs` (`MovDemuxer::seek_to_impl`), mirroring
//! oxideav-mp4's `Mp4Demuxer::seek_to` at
//! `crates/oxideav-mp4/src/demux.rs:2418`.
//!
//! Fixtures (`tests/fixtures/`) are committed binary files, kept
//! under 100 KB each:
//! * `h264_2s.mov` — 2 s of H.264 video, 10 fps, GOP=10, no audio.
//! * `aac_2s.mov` — 2 s of stereo AAC audio, no video.
//! * `h264_2s_frag.mov` — same video, fragmented (`moof/traf/trun`).
//!
//! Each test re-opens the file rather than reusing a demuxer instance
//! so the cursor state across tests is independent.

#![cfg(feature = "registry")]

use std::fs::File;
use std::path::PathBuf;

use oxideav_core::{Demuxer, Error, ReadSeek};
use oxideav_mov::MovDemuxer;

fn fixture_path(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push(name);
    p
}

fn open_fixture(name: &str) -> Option<MovDemuxer> {
    let path = fixture_path(name);
    if !path.exists() {
        return None;
    }
    let f = File::open(&path).expect("open fixture");
    let boxed: Box<dyn ReadSeek> = Box::new(f);
    Some(MovDemuxer::open(boxed).expect("parse fixture"))
}

// ─────────────────── basic invariants ───────────────────

#[test]
fn seek_to_zero_resets_to_start() {
    let mut d = match open_fixture("h264_2s.mov") {
        Some(d) => d,
        None => return,
    };
    // Pull one packet, then seek back to 0.
    let first = d.next_packet().expect("first packet");
    let first_dts = first.dts.unwrap_or(0);

    let landed = d.seek_to(0, 0).expect("seek_to(0)");
    // Seeking to pts=0 should land at-or-before the first sample's dts.
    assert!(
        landed <= first_dts,
        "seek_to(0) landed at {landed}, first dts was {first_dts}"
    );

    // Subsequent next_packet should re-emit the first sample.
    let again = d.next_packet().expect("packet after seek");
    assert_eq!(
        again.dts, first.dts,
        "seek_to(0) didn't return to the start"
    );
    assert!(again.flags.keyframe, "first sample must be a keyframe");
}

// ─────────────────── video: keyframe-snap behaviour ───────────────────

#[test]
fn seek_to_keyframe_lands_at_or_before_target() {
    let mut d = match open_fixture("h264_2s.mov") {
        Some(d) => d,
        None => return,
    };
    // Choose the video stream.
    let video_idx = d
        .streams()
        .iter()
        .position(|s| s.params.media_type == oxideav_core::MediaType::Video)
        .expect("video stream") as u32;

    let ts = d.streams()[video_idx as usize].time_base;
    // 1 second = ts.0.den ticks (numerator 1).
    let target_pts: i64 = ts.0.den;

    let landed = d.seek_to(video_idx, target_pts).expect("seek video");
    assert!(
        landed <= target_pts,
        "seek snapped forward: requested {target_pts}, landed {landed}"
    );

    // The next packet must be a keyframe.
    let pkt = d.next_packet().expect("packet after seek");
    assert_eq!(pkt.stream_index, video_idx);
    assert!(
        pkt.flags.keyframe,
        "seek should land on a keyframe, but next_packet flag was false"
    );
    assert_eq!(
        pkt.dts.unwrap_or(0),
        landed,
        "next packet's dts mismatches seek_to's return value"
    );
}

// ─────────────────── audio: every sample is a sync ───────────────────

#[test]
fn seek_to_in_audio_only_track_lands_exactly() {
    let mut d = match open_fixture("aac_2s.mov") {
        Some(d) => d,
        None => return,
    };
    let audio_idx = d
        .streams()
        .iter()
        .position(|s| s.params.media_type == oxideav_core::MediaType::Audio)
        .expect("audio stream") as u32;

    let ts = d.streams()[audio_idx as usize].time_base;
    // 1 s into the audio.
    let target_pts: i64 = ts.0.den;

    let landed = d.seek_to(audio_idx, target_pts).expect("seek audio");
    // Audio has no stss → snap is "largest sample whose dts <= target",
    // so landed should be <= target and within one frame of it.
    assert!(landed <= target_pts);

    let pkt = d.next_packet().expect("audio packet after seek");
    assert_eq!(pkt.stream_index, audio_idx);
    assert_eq!(pkt.dts.unwrap_or(0), landed);
}

// ─────────────────── past-end clamp ───────────────────

#[test]
fn seek_past_end_clamps() {
    let mut d = match open_fixture("h264_2s.mov") {
        Some(d) => d,
        None => return,
    };
    let video_idx = d
        .streams()
        .iter()
        .position(|s| s.params.media_type == oxideav_core::MediaType::Video)
        .expect("video stream") as u32;

    let huge_pts = i64::MAX / 4;
    let landed = d
        .seek_to(video_idx, huge_pts)
        .expect("seek past end should clamp, not error");
    assert!(landed >= 0);
    assert!(
        landed < huge_pts,
        "past-end seek didn't clamp: landed {landed} >= request {huge_pts}"
    );

    // We can still pull the post-seek packet.
    let pkt = d.next_packet().expect("packet after past-end seek");
    assert!(pkt.flags.keyframe);
}

// ─────────────────── fragmented MP4: unsupported (this round) ───────────────────

#[test]
fn seek_in_fragmented_returns_unsupported() {
    let mut d = match open_fixture("h264_2s_frag.mov") {
        Some(d) => d,
        None => return,
    };
    if !d.is_fragmented() {
        // Skip cleanly when the fixture wasn't actually muxed
        // fragmented (some ffmpeg builds ignore `+frag_keyframe` for
        // tiny inputs).
        return;
    }
    let err = d
        .seek_to(0, 0)
        .expect_err("fragmented seek should be unsupported");
    assert!(
        matches!(err, Error::Unsupported(_)),
        "expected Error::Unsupported, got {err:?}"
    );
}

// ─────────────────── invariant: returned pts == next packet's pts ───────────────────

#[test]
fn seek_landed_pts_matches_next_packet_pts() {
    let d = match open_fixture("h264_2s.mov") {
        Some(d) => d,
        None => return,
    };
    let video_idx = d
        .streams()
        .iter()
        .position(|s| s.params.media_type == oxideav_core::MediaType::Video)
        .expect("video stream") as u32;

    // Sweep through a handful of target points.
    let ts_den = d.streams()[video_idx as usize].time_base.0.den;
    for target_pts in [0i64, ts_den / 4, ts_den / 2, ts_den, (3 * ts_den) / 2] {
        // Re-open per iteration to get a clean cursor (a single
        // demuxer would also work, but the per-target reopen makes
        // the test failure messages clearer).
        let mut d = open_fixture("h264_2s.mov").expect("fixture present");
        let landed = d
            .seek_to(video_idx, target_pts)
            .expect("seek for invariant check");
        let pkt = d.next_packet().expect("packet after seek");
        assert_eq!(
            pkt.dts.unwrap_or(0),
            landed,
            "invariant broken at target={target_pts}: landed={landed}, packet dts={:?}",
            pkt.dts
        );
        assert!(
            pkt.flags.keyframe,
            "post-seek packet wasn't a keyframe (target={target_pts})"
        );
    }
}

// ─────────────────── invalid stream index ───────────────────

#[test]
fn seek_invalid_stream_returns_invalid() {
    let mut d = match open_fixture("h264_2s.mov") {
        Some(d) => d,
        None => return,
    };
    let bogus = d.streams().len() as u32 + 7;
    let err = d
        .seek_to(bogus, 0)
        .expect_err("seek with bogus stream should fail");
    assert!(matches!(err, Error::InvalidData(_)), "got {err:?}");
}
