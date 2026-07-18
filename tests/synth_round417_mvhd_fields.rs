//! Round 417 — `mvhd` QuickTime-side headroom: the movie display
//! matrix and the six preview/poster/selection/current time fields
//! (QTFF pp. 33–34) now round-trip typed through both sides of the
//! crate instead of being skipped on read and zeroed on write.

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

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

/// 90°-clockwise display matrix (QTFF p. 199 convention):
/// a=0, b=1.0, c=-1.0, d=0, w=1.0.
const ROT90: [i32; 9] = [0, 0x0001_0000, 0, -0x0001_0000, 0, 0, 0, 0, 0x4000_0000];

#[test]
fn movie_matrix_and_time_fields_round_trip() {
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    m.add_track(audio_kind(), 1000, samples(4), &[]);
    m.set_movie_matrix(Some(ROT90));
    m.set_movie_preview(120, 240);
    m.set_movie_poster_time(360);
    m.set_movie_selection(480, 500);
    m.set_movie_current_time(510);
    let d = open(m.encode_to_vec().unwrap());
    let mvhd = d.mvhd.as_ref().expect("mvhd");
    assert_eq!(mvhd.matrix, ROT90);
    assert_eq!(mvhd.rotation(), TrackRotation::Rotate90);
    assert_eq!(mvhd.preview_time, 120);
    assert_eq!(mvhd.preview_duration, 240);
    assert_eq!(mvhd.poster_time, 360);
    assert_eq!(mvhd.selection_time, 480);
    assert_eq!(mvhd.selection_duration, 500);
    assert_eq!(mvhd.current_time, 510);
}

#[test]
fn default_movie_matrix_stays_identity() {
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    m.add_track(audio_kind(), 1000, samples(2), &[]);
    let d = open(m.encode_to_vec().unwrap());
    let mvhd = d.mvhd.as_ref().expect("mvhd");
    assert_eq!(mvhd.rotation(), TrackRotation::None);
    assert_eq!(mvhd.matrix[0], 0x0001_0000);
    assert_eq!(mvhd.matrix[4], 0x0001_0000);
    assert_eq!(mvhd.matrix[8], 0x4000_0000);
    assert_eq!(
        (mvhd.preview_time, mvhd.preview_duration, mvhd.poster_time),
        (0, 0, 0)
    );
    // Setting then clearing the matrix restores identity.
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    m.add_track(audio_kind(), 1000, samples(2), &[]);
    m.set_movie_matrix(Some(ROT90));
    m.set_movie_matrix(None);
    let d = open(m.encode_to_vec().unwrap());
    assert_eq!(d.mvhd.as_ref().unwrap().rotation(), TrackRotation::None);
}

#[test]
fn fragmented_init_mvhd_carries_matrix_and_time_fields() {
    let mut m = MovMuxer::new()
        .with_movie_timescale(1000)
        .with_fragmentation(FragmentationMode::ByFrameCount(2));
    m.add_track(audio_kind(), 1000, samples(4), &[]);
    m.set_movie_matrix(Some(ROT90));
    m.set_movie_preview(60, 90);
    let d = open(m.encode_fragmented_to_vec().unwrap());
    let mvhd = d.mvhd.as_ref().expect("mvhd");
    assert_eq!(mvhd.matrix, ROT90);
    assert_eq!(mvhd.preview_time, 60);
    assert_eq!(mvhd.preview_duration, 90);
}
