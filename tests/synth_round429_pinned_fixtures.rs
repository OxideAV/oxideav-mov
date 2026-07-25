//! Round 429 — **pinned round-trip fixtures** for the muxer surface:
//! a multi-track interleaved+faststart movie, an edit-listed movie,
//! and a B-frame `ctts` movie (negative offsets, v1 box, `cslg`).
//!
//! Each fixture lives in `tests/fixtures/` and is byte-pinned: the
//! deterministic builder in this file must reproduce the committed
//! bytes exactly (the muxer writes no wall-clock timestamps —
//! creation/modification times are zero — so output is a pure
//! function of its inputs). Any unintended change to the write path
//! shows up as a byte diff here; intended changes regenerate via
//! `OXIDEAV_MOV_REGEN_FIXTURES=1 cargo test`.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{ChunkStrategy, MovDemuxer, MovMuxer, MuxEdit, MuxSample, MuxTrackKind};

const FIXTURE_MULTITRACK: &str = "tests/fixtures/rt_r429_multitrack_interleaved_faststart.mov";
const FIXTURE_EDITLIST: &str = "tests/fixtures/rt_r429_editlist.mov";
const FIXTURE_BFRAME: &str = "tests/fixtures/rt_r429_bframe_ctts.mov";

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open fixture")
}

fn video_samples(n: usize, offsets: Option<&[i32]>) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![0x20 + i as u8; 48],
            duration: 200,
            keyframe: offsets.is_none() || i % 4 == 0,
            composition_offset: offsets.map(|o| o[i % o.len()]).unwrap_or(0),
        })
        .collect()
}

fn audio_samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![0xE0u8.wrapping_add(i as u8); 17],
            duration: 400,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn add_video(m: &mut MovMuxer, samples: Vec<MuxSample>) -> u32 {
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 4,
        },
        2400,
        samples,
        &[],
    )
}

fn add_audio(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        audio_samples(20),
        &[],
    )
}

/// Fixture 1: video+audio, half-second interleave, faststart.
fn build_multitrack() -> Vec<u8> {
    let mut m = MovMuxer::new()
        .with_chunk_strategy(ChunkStrategy::InterleaveByMovieTicks(300))
        .with_faststart();
    add_video(&mut m, video_samples(24, None)); // 24 × 200/2400 = 2 s
    add_audio(&mut m); // 20 × 400/8000 = 1 s
    m.encode_to_vec().expect("encode multitrack fixture")
}

/// Fixture 2: edit-listed video (delay + head trim + dwell) + audio.
fn build_editlist() -> Vec<u8> {
    let mut m = MovMuxer::new();
    let vid = add_video(&mut m, video_samples(16, None));
    add_audio(&mut m);
    m.set_edit_list(
        vid,
        &[
            MuxEdit::empty(300),        // 0.5 s presentation delay
            MuxEdit::segment(600, 400), // 1 s of media from tick 400
            MuxEdit::dwell(150, 2000),  // 0.25 s hold on tick 2000
        ],
    )
    .expect("edit list");
    m.encode_to_vec().expect("encode editlist fixture")
}

/// Fixture 3: B-frame decode order I P B B with negative composition
/// offsets (v1 `ctts`) and an auto-derived `cslg`.
fn build_bframe_ctts() -> Vec<u8> {
    let mut m = MovMuxer::new();
    // dts:  0   200  400  600 …  pts: 0  600  200  400 …
    let offsets = [0i32, 400, -200, -200];
    let vid = add_video(&mut m, video_samples(16, Some(&offsets)));
    m.auto_cslg(vid).expect("auto cslg");
    add_audio(&mut m);
    m.encode_to_vec().expect("encode bframe fixture")
}

fn fixture_path(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn check_pin(rel: &str, built: &[u8]) {
    let path = fixture_path(rel);
    if std::env::var_os("OXIDEAV_MOV_REGEN_FIXTURES").is_some() {
        std::fs::write(&path, built).expect("regenerate fixture");
        return;
    }
    let pinned = std::fs::read(&path).unwrap_or_else(|e| {
        panic!("read pinned fixture {rel}: {e} (regenerate with OXIDEAV_MOV_REGEN_FIXTURES=1)")
    });
    assert_eq!(
        pinned, built,
        "{rel}: committed fixture bytes must match the deterministic builder"
    );
}

#[test]
fn multitrack_fixture_is_pinned_and_round_trips() {
    let built = build_multitrack();
    check_pin(FIXTURE_MULTITRACK, &built);
    let mut d = open(built);
    assert!(d.is_faststart());
    assert_eq!(d.streams().len(), 2);
    // 2 s video at 0.5 s period ⇒ 4 chunks; 1 s audio ⇒ 2 chunks.
    assert_eq!(d.chunk_count(0), Some(4));
    assert_eq!(d.chunk_count(1), Some(2));
    // Every payload survives.
    let mut n = [0usize; 2];
    while let Ok(p) = d.next_packet() {
        n[p.stream_index as usize] += 1;
    }
    assert_eq!(n, [24, 20]);
}

#[test]
fn editlist_fixture_is_pinned_and_round_trips() {
    let built = build_editlist();
    check_pin(FIXTURE_EDITLIST, &built);
    let d = open(built);
    let t = &d.tracks[0];
    assert_eq!(t.edit_start_delay(), 300);
    assert_eq!(t.edit_media_start(), Some(400));
    // 300 (empty) + 600 (segment) + 150 (dwell) movie ticks.
    assert_eq!(t.edit_total_duration(), 1050);
    assert_eq!(t.edits.len(), 3);
}

#[test]
fn pinned_fixtures_pass_black_box_probing() {
    // Opaque-validator acceptance of the committed fixture bytes
    // themselves. Skips silently when ffprobe is not on $PATH.
    let available = std::process::Command::new("ffprobe")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !available {
        return;
    }
    for rel in [FIXTURE_MULTITRACK, FIXTURE_EDITLIST, FIXTURE_BFRAME] {
        let path = fixture_path(rel);
        let out = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=index",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .expect("run ffprobe");
        assert!(
            out.status.success() && out.stderr.is_empty(),
            "{rel}: black-box validator must accept the pinned fixture silently: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let streams = String::from_utf8_lossy(&out.stdout).lines().count();
        assert_eq!(streams, 2, "{rel}: two streams visible to the validator");
    }
}

#[test]
fn bframe_ctts_fixture_is_pinned_and_round_trips() {
    let built = build_bframe_ctts();
    check_pin(FIXTURE_BFRAME, &built);
    let mut d = open(built);
    // Recovered pts must reorder against dts exactly per the offsets.
    let mut video: Vec<(i64, i64)> = Vec::new();
    while let Ok(p) = d.next_packet() {
        if p.stream_index == 0 {
            video.push((p.dts.expect("dts"), p.pts.expect("pts")));
        }
    }
    assert_eq!(video.len(), 16);
    let offsets = [0i64, 400, -200, -200];
    for (i, (dts, pts)) in video.iter().enumerate() {
        assert_eq!(*dts, 200 * i as i64, "sample {i} dts");
        assert_eq!(*pts, dts + offsets[i % 4], "sample {i} pts from ctts");
    }
}
