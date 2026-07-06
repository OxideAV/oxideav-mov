//! Round 394 — seek on the **edited** timeline. With
//! `MovDemuxer::apply_edit_lists(true)` enabled, `seek_to(stream,
//! pts)` interprets `pts` as an edited-timeline timestamp (the same
//! contract `next_packet()` emits in that mode), resolves it back to
//! media time through the edit list (QTFF pp. 46–48 / ISO/IEC
//! 14496-12 §8.6.6), and returns the edited dts the next packet will
//! carry. Out-of-presentation targets (inside an empty edit, past the
//! end) clamp to the nearest presented media tick.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxEdit, MuxSample, MuxTrackKind};

/// One video track, `n` uniform 100-tick samples, keyframes every
/// `sync_every` samples (sample 0 always sync).
fn build(n: usize, sync_every: usize, edits: &[MuxEdit]) -> Vec<u8> {
    let samples: Vec<MuxSample> = (0..n)
        .map(|i| MuxSample {
            data: vec![i as u8; 4],
            duration: 100,
            keyframe: i % sync_every == 0,
            composition_offset: 0,
        })
        .collect();
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 64,
            height: 64,
        },
        1000,
        samples,
        &[],
    );
    if !edits.is_empty() {
        m.set_edit_list(tid, edits).unwrap();
    }
    m.encode_to_vec().unwrap()
}

fn open_edited(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open");
    d.apply_edit_lists(true);
    d
}

#[test]
fn seek_zero_on_trimmed_track_lands_first_presented_sample() {
    // Trim edit presents media [200, 1000): edited 0 is media 200.
    // Every sample is sync, so the seek lands exactly on sample 2 and
    // the returned dts matches the next packet's edited dts (0).
    let bytes = build(10, 1, &[MuxEdit::segment(800, 200)]);
    let mut d = open_edited(bytes);
    let dts = d.seek_to(0, 0).unwrap();
    assert_eq!(dts, 0);
    let p = d.next_packet().unwrap();
    assert_eq!(p.dts, Some(0));
    assert_eq!(p.pts, Some(0));
}

#[test]
fn seek_mid_segment_snaps_to_sync_and_reports_edited_dts() {
    // Keyframes every 4 samples (media dts 0/400/800). Trim edit
    // presents media [200, 1000). Edited 450 → media 650 → snaps back
    // to the sync sample at media dts 400 → edited dts 200.
    let bytes = build(10, 4, &[MuxEdit::segment(800, 200)]);
    let mut d = open_edited(bytes);
    let dts = d.seek_to(0, 450).unwrap();
    assert_eq!(dts, 200);
    let p = d.next_packet().unwrap();
    assert_eq!(p.dts, Some(200));
}

#[test]
fn seek_inside_head_empty_edit_clamps_to_first_segment() {
    // empty(500) + segment(500, 0): edited [0, 500) presents nothing.
    // Seeking to edited 250 clamps to the first presented media tick
    // (media 0) whose edited dts is 500.
    let bytes = build(5, 1, &[MuxEdit::empty(500), MuxEdit::segment(500, 0)]);
    let mut d = open_edited(bytes);
    let dts = d.seek_to(0, 250).unwrap();
    assert_eq!(dts, 500);
    let p = d.next_packet().unwrap();
    assert_eq!(p.pts, Some(500));
}

#[test]
fn seek_past_end_clamps_into_last_segment() {
    // Trim edit presents media [200, 1000); edited timeline spans
    // [0, 800). Seeking to edited 5000 clamps into the tail of the
    // presentation and lands the last sync at-or-before it.
    let bytes = build(10, 4, &[MuxEdit::segment(800, 200)]);
    let mut d = open_edited(bytes);
    let dts = d.seek_to(0, 5000).unwrap();
    // Last sync sample is media dts 800 → edited 600.
    assert_eq!(dts, 600);
    let p = d.next_packet().unwrap();
    assert_eq!(p.dts, Some(600));
}

#[test]
fn seek_landing_on_dropped_sample_reports_next_emitted_dts() {
    // Keyframes every 4 samples. Trim edit presents media [450, 1000):
    // the sync sample at media dts 400 (pts 400 < 450) is *dropped* by
    // the edit list. Seeking to edited 0 lands that sync sample for
    // decode purposes, but the reported dts must be the first packet
    // the applied mode actually emits (media 500 → edited 50).
    let bytes = build(10, 4, &[MuxEdit::segment(550, 450)]);
    let mut d = open_edited(bytes);
    let dts = d.seek_to(0, 0).unwrap();
    assert_eq!(dts, 50);
    let p = d.next_packet().unwrap();
    assert_eq!(p.dts, Some(50));
    assert_eq!(p.pts, Some(50));
}

#[test]
fn seek_without_mode_keeps_media_contract() {
    // Same file, mode off: seek_to takes media pts and returns media
    // dts, unaffected by the edit list.
    let bytes = build(10, 1, &[MuxEdit::segment(800, 200)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).unwrap();
    let dts = d.seek_to(0, 450).unwrap();
    assert_eq!(dts, 400);
    let p = d.next_packet().unwrap();
    assert_eq!(p.dts, Some(400));
}

#[test]
fn helper_clamps_are_exposed() {
    let bytes = build(5, 1, &[MuxEdit::empty(500), MuxEdit::segment(500, 0)]);
    let d = open_edited(bytes);
    // Direct correspondence: edited 600 → media 100.
    assert_eq!(d.edited_pts_to_media_pts(0, 600), 100);
    // Inside the empty window → clamps to the segment's media start.
    assert_eq!(d.edited_pts_to_media_pts(0, 100), 0);
    // Negative input is clamped to 0 first.
    assert_eq!(d.edited_pts_to_media_pts(0, -50), 0);
}
