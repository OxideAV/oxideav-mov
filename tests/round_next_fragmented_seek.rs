//! Round-21 acceptance tests for fragmented-MP4 random-access seek
//! via the ISO/IEC 14496-12 §8.8.10 `tfra` index.
//!
//! Companion to `tests/seek.rs` (non-fragmented stbl-based seek,
//! round 20). The fragmented variant routes through
//! `MovDemuxer::seek_to_fragmented`: when an `mfra/tfra` is present,
//! a binary search over the per-track `time` column picks the
//! largest entry whose decode-time is `<= target_pts`, then snaps
//! the demuxer's flat sample-queue cursor to the matching sync
//! sample so the next `next_packet()` re-emits it.
//!
//! Fixtures (`tests/fixtures/`) — committed binary files, all
//! generated via `ffmpeg`:
//!
//! * `h264_frag_with_mfra.mp4` — 3 s of 160×120 H.264 at 10 fps,
//!   GOP=10, ffmpeg `-movflags +frag_keyframe+empty_moov` with
//!   `-frag_duration 500000`. Six keyframe-aligned `moof`s + a
//!   trailing `mfra` carrying 6 `tfra` entries (one per moof).
//! * `h264_frag_nomfra.mp4` — same encoding parameters but with
//!   `+skip_trailer` so no `mfra` is emitted. Used to exercise the
//!   fallback path.
//! * `h264_2s_frag.mov` (round-18 fixture) — single `moof` with a
//!   1-entry `tfra`; used for the regression "seek-to-zero re-lands
//!   on the first sync sample" case.
//! * `h264_2s.mov` (round-20 fixture) — non-fragmented; used to
//!   confirm the non-fragmented branch is unaffected by the
//!   round-21 routing change.

#![cfg(feature = "registry")]

use std::fs::File;
use std::path::PathBuf;

use oxideav_core::{Demuxer, ReadSeek};
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
    Some(MovDemuxer::open(boxed).expect("parse fragmented fixture"))
}

fn video_stream(d: &MovDemuxer) -> u32 {
    d.streams()
        .iter()
        .position(|s| s.params.media_type == oxideav_core::MediaType::Video)
        .expect("video stream") as u32
}

// ─────────────────── tfra parse + open-time wiring ───────────────────

#[test]
fn open_with_mfra_populates_tfra_indexes() {
    let d = match open_fixture("h264_frag_with_mfra.mp4") {
        Some(d) => d,
        None => return,
    };
    assert!(
        d.is_fragmented(),
        "fixture must declare fragmented (mvex/moof present)"
    );
    assert!(
        !d.tfra_indexes.is_empty(),
        "mfra-bearing fixture must produce >=1 tfra entry vector"
    );
    let tfra = &d.tfra_indexes[0];
    assert!(tfra.entries.len() >= 2, "expected at least 2 tfra entries");
    // §8.8.10.3 "entries are stored in increasing order of time".
    for w in tfra.entries.windows(2) {
        assert!(
            w[0].time <= w[1].time,
            "tfra entries are not monotonically increasing: {:?} -> {:?}",
            w[0].time,
            w[1].time
        );
    }
}

#[test]
fn open_without_mfra_leaves_tfra_empty() {
    let d = match open_fixture("h264_frag_nomfra.mp4") {
        Some(d) => d,
        None => return,
    };
    if !d.is_fragmented() {
        return;
    }
    assert!(
        d.tfra_indexes.is_empty(),
        "fixture without mfra must produce 0 tfra entries (got {})",
        d.tfra_indexes.len()
    );
}

// ─────────────────── tfra-indexed seek_to ───────────────────

#[test]
fn fragmented_with_mfra_seek_lands_at_tfra_entry() {
    let mut d = match open_fixture("h264_frag_with_mfra.mp4") {
        Some(d) => d,
        None => return,
    };
    if d.tfra_indexes.is_empty() {
        return;
    }
    let vid = video_stream(&d);
    // Pick a target inside the 3rd tfra entry so the picked entry is
    // unambiguously not entry 0. Entry times depend on the fixture;
    // query them dynamically. The tfra entry's `time` field is in
    // *presentation* (composition) timescale per §8.8.10.3.
    let entries = d.tfra_indexes[0].entries.clone();
    assert!(entries.len() >= 3);
    let target = entries[2].time as i64;
    let landed = d.seek_to(vid, target).expect("tfra-indexed seek");
    // `landed` is the chosen sample's DTS. The corresponding PTS
    // (= dts + composition_offset) must equal the tfra entry's time.
    let pkt = d.next_packet().expect("packet after fragmented seek");
    assert_eq!(pkt.stream_index, vid);
    assert!(pkt.flags.keyframe, "must land on a keyframe");
    assert_eq!(pkt.dts.unwrap_or(-1), landed);
    assert_eq!(
        pkt.pts.unwrap_or(-1),
        entries[2].time as i64,
        "post-seek packet's PTS doesn't match the tfra entry's time"
    );
}

#[test]
fn fragmented_seek_to_zero_resets_to_first_moof() {
    let mut d = match open_fixture("h264_frag_with_mfra.mp4") {
        Some(d) => d,
        None => return,
    };
    if !d.is_fragmented() {
        return;
    }
    let vid = video_stream(&d);
    // Consume a few packets first so `next` advances away from 0,
    // then seek back to pts=0.
    let _ = d.next_packet().expect("first packet");
    let _ = d.next_packet().expect("second packet");
    let landed = d.seek_to(vid, 0).expect("seek_to(0)");
    assert!(
        landed >= 0,
        "fragmented seek_to(0) should land at a non-negative dts (got {landed})"
    );
    let pkt = d.next_packet().expect("packet after seek-to-zero");
    assert!(pkt.flags.keyframe);
    assert_eq!(pkt.dts.unwrap_or(-1), landed);
    // The first keyframe's dts should be the smallest sync-sample dts
    // in the file — must be `<=` every subsequent sample's dts.
    let next = d
        .next_packet()
        .expect("at least one more packet after first keyframe");
    assert!(next.dts.unwrap_or(0) >= landed);
}

#[test]
fn fragmented_seek_between_keyframes_snaps_back_to_prior_keyframe() {
    let mut d = match open_fixture("h264_frag_with_mfra.mp4") {
        Some(d) => d,
        None => return,
    };
    if d.tfra_indexes.is_empty() {
        return;
    }
    let vid = video_stream(&d);
    let entries = d.tfra_indexes[0].entries.clone();
    assert!(entries.len() >= 3);
    // Target halfway between entry[1] and entry[2] (in PTS units);
    // the spec-mandated "largest entry whose time <= target" rule
    // means we land on entry[1].
    let mid = (entries[1].time + entries[2].time) / 2;
    let landed = d.seek_to(vid, mid as i64).expect("seek mid-gap");
    let pkt = d.next_packet().expect("packet after mid-gap seek");
    assert!(pkt.flags.keyframe);
    assert_eq!(pkt.dts.unwrap_or(-1), landed);
    assert_eq!(
        pkt.pts.unwrap_or(-1),
        entries[1].time as i64,
        "mid-gap seek did not snap to the previous tfra entry's PTS"
    );
}

// ─────────────────── fallback: fragmented WITHOUT tfra ──────────────────

#[test]
fn fragmented_without_mfra_falls_back_to_queue_scan() {
    let mut d = match open_fixture("h264_frag_nomfra.mp4") {
        Some(d) => d,
        None => return,
    };
    if !d.is_fragmented() {
        return;
    }
    assert!(d.tfra_indexes.is_empty());
    let vid = video_stream(&d);
    // Even without tfra, the round-18 walker already flattened every
    // moof's samples into `self.samples`, so seek_to should still
    // pick a sync sample.
    let landed = d.seek_to(vid, 0).expect("fallback queue-scan seek");
    assert!(landed >= 0);
    let pkt = d.next_packet().expect("packet after fallback seek");
    assert_eq!(pkt.stream_index, vid);
    assert!(pkt.flags.keyframe);
    assert_eq!(pkt.dts.unwrap_or(-1), landed);
}

// ─────────────────── regression: non-fragmented still works ──────────────────

#[test]
fn existing_non_fragmented_seek_still_works() {
    let mut d = match open_fixture("h264_2s.mov") {
        Some(d) => d,
        None => return,
    };
    assert!(
        !d.is_fragmented(),
        "h264_2s.mov should not be fragmented (regression sentinel)"
    );
    let vid = video_stream(&d);
    let landed = d.seek_to(vid, 0).expect("non-fragmented seek path");
    assert!(landed >= 0);
    let pkt = d.next_packet().expect("packet after non-fragmented seek");
    assert!(pkt.flags.keyframe);
    assert_eq!(pkt.dts.unwrap_or(-1), landed);
}

// ─────────────────── past-end clamp under tfra ──────────────────

#[test]
fn fragmented_seek_past_end_clamps_to_last_tfra_entry() {
    let mut d = match open_fixture("h264_frag_with_mfra.mp4") {
        Some(d) => d,
        None => return,
    };
    if d.tfra_indexes.is_empty() {
        return;
    }
    let vid = video_stream(&d);
    let last = *d.tfra_indexes[0].entries.last().unwrap();
    let huge = i64::MAX / 4;
    let landed = d
        .seek_to(vid, huge)
        .expect("past-end seek should clamp, not error");
    // Past-end → land on the last tfra entry. `landed` is DTS so
    // verify the matching PTS via the next packet.
    let pkt = d.next_packet().expect("packet after past-end seek");
    assert!(pkt.flags.keyframe);
    assert_eq!(pkt.dts.unwrap_or(-1), landed);
    assert_eq!(pkt.pts.unwrap_or(-1), last.time as i64);
}
