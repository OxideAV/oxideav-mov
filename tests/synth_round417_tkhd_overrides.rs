//! Round 417 — `tkhd` write-side headroom (QTFF p. 42): the display
//! matrix, layer, alternate_group, volume, and 24-bit flags were
//! hardcoded on write (identity / 0 / 0 / kind-default / 0x7) even
//! though the demuxer surfaces all five typed. New granular setters
//! round-trip them through both sides.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    FragmentationMode, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, TrackRotation,
};

fn samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![i as u8; 4],
            duration: 100,
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

fn video_kind() -> MuxTrackKind {
    MuxTrackKind::Video {
        format: *b"avc1",
        width: 64,
        height: 48,
    }
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

/// 180° display matrix: a = d = -1.0, w = 1.0.
const ROT180: [i32; 9] = [-0x0001_0000, 0, 0, 0, -0x0001_0000, 0, 0, 0, 0x4000_0000];

#[test]
fn tkhd_matrix_layer_group_volume_round_trip() {
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let vid = m.add_track(video_kind(), 1000, samples(4), &[]);
    let aud = m.add_track(audio_kind(), 1000, samples(4), &[]);
    m.set_track_matrix(vid, Some(ROT180)).unwrap();
    m.set_track_layer(vid, -1).unwrap();
    m.set_track_alternate_group(aud, 2).unwrap();
    m.set_track_volume(aud, 0x0080).unwrap(); // 0.5
    let d = open(m.encode_to_vec().unwrap());
    let v = &d.tracks[0].tkhd;
    assert_eq!(v.matrix, ROT180);
    assert_eq!(v.rotation(), TrackRotation::Rotate180);
    assert_eq!(v.layer, -1);
    assert_eq!(v.alternate_group, 0);
    let a = &d.tracks[1].tkhd;
    assert_eq!(a.rotation(), TrackRotation::None);
    assert_eq!(a.alternate_group, 2);
    assert_eq!(a.volume, 0x0080);
    // Defaults untouched elsewhere: video volume 0, audio layer 0.
    assert_eq!(v.volume, 0);
    assert_eq!(a.layer, 0);
}

#[test]
fn tkhd_flags_override_drops_track_from_presentation() {
    // A disabled track (flags without 0x1) must fall out of the
    // demuxer's default-presentation fold.
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let a = m.add_track(audio_kind(), 1000, samples(2), &[]);
    let _b = m.add_track(audio_kind(), 1000, samples(2), &[]);
    m.set_track_flags(a, 0x2).unwrap(); // in-movie but NOT enabled
    let d = open(m.encode_to_vec().unwrap());
    assert_eq!(d.tracks[0].tkhd.flags, 0x2);
    assert!(!d.tracks[0].is_enabled());
    assert_eq!(d.tracks[1].tkhd.flags, 0x7);
    let presented: Vec<usize> = d.presentation_tracks().map(|(i, _)| i).collect();
    assert_eq!(presented, vec![1]);
}

#[test]
fn tkhd_flags_rejects_more_than_24_bits() {
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(audio_kind(), 1000, samples(2), &[]);
    assert!(m.set_track_flags(tid, 0x0100_0000).is_err());
    assert!(m.set_track_flags(tid, 0x00FF_FFFF).is_ok());
    // Unknown track ids error on every setter.
    assert!(m.set_track_matrix(99, None).is_err());
    assert!(m.set_track_layer(99, 0).is_err());
    assert!(m.set_track_alternate_group(99, 0).is_err());
    assert!(m.set_track_volume(99, 0).is_err());
    assert!(m.set_track_flags(99, 0).is_err());
}

#[test]
fn fragmented_init_tkhd_carries_overrides() {
    let mut m = MovMuxer::new()
        .with_movie_timescale(1000)
        .with_fragmentation(FragmentationMode::ByFrameCount(2));
    let vid = m.add_track(video_kind(), 1000, samples(4), &[]);
    m.set_track_matrix(vid, Some(ROT180)).unwrap();
    m.set_track_layer(vid, 3).unwrap();
    m.set_track_alternate_group(vid, 1).unwrap();
    m.set_track_flags(vid, 0x3).unwrap();
    let d = open(m.encode_fragmented_to_vec().unwrap());
    let t = &d.tracks[0].tkhd;
    assert_eq!(t.rotation(), TrackRotation::Rotate180);
    assert_eq!(t.layer, 3);
    assert_eq!(t.alternate_group, 1);
    assert_eq!(t.flags, 0x3);
}

#[test]
fn alternate_group_pairs_with_switch_group_fold() {
    // Two audio alternates in tkhd group 1: the demuxer's
    // alternate_groups() fold must group them together.
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let a = m.add_track(audio_kind(), 1000, samples(2), &[]);
    let b = m.add_track(audio_kind(), 1000, samples(2), &[]);
    m.set_track_alternate_group(a, 1).unwrap();
    m.set_track_alternate_group(b, 1).unwrap();
    let d = open(m.encode_to_vec().unwrap());
    let groups = d.alternate_groups();
    let g1 = groups
        .iter()
        .find(|(gid, _)| *gid == 1)
        .expect("alternate group 1");
    assert_eq!(g1.1.len(), 2);
}
