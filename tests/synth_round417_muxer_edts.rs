//! Round 417 — muxer `edts/elst` full-semantics parity (QTFF pp. 46–48).
//!
//! * Typed [`MuxEdit::dwell`] / [`MuxEdit::segment_with_rate`]
//!   constructors round-trip through the demuxer's segment
//!   classification and the applied edit-list mode.
//! * `set_edit_list` rejects negative `media_rate` (QTFF p. 48).
//! * The **fragmented** write path now emits the `edts > elst` into
//!   the init-segment `trak` (the `moof`s only index samples), so a
//!   fragmented presentation's priming-skip / start-delay edits reach
//!   players.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, Packet, ReadSeek};
use oxideav_mov::{
    EditSegmentKind, FragmentationMode, MovDemuxer, MovMuxer, MuxEdit, MuxSample, MuxTrackKind,
    MEDIA_RATE_ONE,
};

fn uniform_samples(n: usize, dur: u32) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![i as u8; 4],
            duration: dur,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn audio_kind() -> MuxTrackKind {
    MuxTrackKind::Audio {
        format: *b"lpcm",
        channels: 2,
        bits_per_sample: 16,
        sample_rate: 48000,
    }
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn drain(d: &mut MovDemuxer) -> Vec<Packet> {
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push(p);
    }
    out
}

#[test]
fn dwell_constructor_round_trips_and_classifies() {
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(audio_kind(), 1000, uniform_samples(4, 100), &[]);
    m.set_edit_list(tid, &[MuxEdit::dwell(600, 200)]).unwrap();
    let d = open(m.encode_to_vec().unwrap());
    let t = &d.tracks[0];
    assert_eq!(t.edits.len(), 1);
    assert_eq!(t.edits[0].media_rate, 0);
    assert_eq!(t.edits[0].media_time, 200);
    assert!(t.edits[0].is_dwell());
    let segs = d.edit_segments_for(0).unwrap();
    assert_eq!(segs[0].kind, EditSegmentKind::Dwell { media_time: 200 });
}

#[test]
fn segment_with_rate_constructor_scales_applied_timing() {
    // Rate 2.0 over 200 movie ticks consumes media [0, 400): all four
    // 100-tick samples present, at halved spacing/durations.
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(audio_kind(), 1000, uniform_samples(4, 100), &[]);
    m.set_edit_list(tid, &[MuxEdit::segment_with_rate(200, 0, 0x0002_0000)])
        .unwrap();
    let mut d = open(m.encode_to_vec().unwrap());
    d.apply_edit_lists(true);
    let pkts = drain(&mut d);
    assert_eq!(pkts.len(), 4);
    for (i, p) in pkts.iter().enumerate() {
        assert_eq!(p.pts, Some(i as i64 * 50), "packet {i}");
        assert_eq!(p.duration, Some(50), "packet {i}");
    }
    // Unity-rate constructor equivalence.
    assert_eq!(
        MuxEdit::segment_with_rate(200, 0, MEDIA_RATE_ONE).media_rate,
        MuxEdit::segment(200, 0).media_rate
    );
}

#[test]
fn set_edit_list_rejects_negative_media_rate() {
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(audio_kind(), 1000, uniform_samples(2, 100), &[]);
    let err = m
        .set_edit_list(tid, &[MuxEdit::segment_with_rate(100, 0, -MEDIA_RATE_ONE)])
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("negative media_rate"), "got: {msg}");
    // Zero (dwell) and positive rates stay accepted.
    m.set_edit_list(tid, &[MuxEdit::dwell(100, 0)]).unwrap();
    m.set_edit_list(tid, &[MuxEdit::segment_with_rate(100, 0, 1)])
        .unwrap();
}

#[test]
fn fragmented_init_segment_carries_edts() {
    // Priming-skip shape: empty(250) delay + head-trimmed segment.
    let mut m = MovMuxer::new()
        .with_movie_timescale(1000)
        .with_fragmentation(FragmentationMode::ByFrameCount(2));
    let tid = m.add_track(audio_kind(), 1000, uniform_samples(6, 100), &[]);
    m.set_edit_list(tid, &[MuxEdit::empty(250), MuxEdit::segment(400, 200)])
        .unwrap();
    let bytes = m.encode_fragmented_to_vec().expect("encode fragmented");
    let mut d = open(bytes);
    assert!(d.is_fragmented());
    let t = &d.tracks[0];
    // The init-trak edts round-trips typed.
    assert_eq!(t.elst_version, Some(0));
    assert_eq!(t.edits.len(), 2);
    assert!(t.edits[0].is_empty());
    assert_eq!(t.edits[0].track_duration, 250);
    assert_eq!(t.edits[1].media_time, 200);
    assert_eq!(t.edits[1].track_duration, 400);
    assert_eq!(t.edit_start_delay(), 250);
    assert_eq!(t.edit_media_start(), Some(200));
    // Fragment samples still demux in full on the raw media timeline.
    let pkts = drain(&mut d);
    assert_eq!(pkts.len(), 6);
}

#[test]
fn fragmented_init_segment_auto_promotes_elst_v1() {
    // A track_duration past u32::MAX forces the 64-bit elst layout on
    // the fragmented path exactly as on the non-fragmented one.
    let big = u32::MAX as u64 + 5;
    let mut m = MovMuxer::new()
        .with_movie_timescale(1000)
        .with_fragmentation(FragmentationMode::ByFrameCount(4));
    let tid = m.add_track(audio_kind(), 1000, uniform_samples(2, 100), &[]);
    m.set_edit_list(tid, &[MuxEdit::segment(big, 0)]).unwrap();
    let d = open(m.encode_fragmented_to_vec().unwrap());
    let t = &d.tracks[0];
    assert_eq!(t.elst_version, Some(1));
    assert_eq!(t.edits.len(), 1);
    assert_eq!(t.edits[0].track_duration, big);
}

#[test]
fn fragmented_without_edits_emits_no_edts() {
    let mut m = MovMuxer::new()
        .with_movie_timescale(1000)
        .with_fragmentation(FragmentationMode::ByFrameCount(4));
    m.add_track(audio_kind(), 1000, uniform_samples(2, 100), &[]);
    let bytes = m.encode_fragmented_to_vec().unwrap();
    assert!(
        !bytes.windows(4).any(|w| w == b"edts"),
        "no edts atom may appear without an edit list"
    );
    let d = open(bytes);
    assert_eq!(d.tracks[0].elst_version, None);
}
