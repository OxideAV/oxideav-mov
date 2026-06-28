//! Round 379 — `MovMuxer` write-side **Track Load Settings atom**
//! (`load`, QTFF pp. 48–49), a QuickTime-only `trak` child the demuxer
//! has long read (`Track::load` via `parse_load`) but the muxer could
//! never write. `MovMuxer::set_track_load_settings(track_id, Some(Load))`
//! emits the 16-byte body via the new `Load::to_body_bytes`.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    Load, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, LOAD_HINT_DOUBLE_BUFFER,
    LOAD_HINT_HIGH_QUALITY, LOAD_PRELOAD_DURATION_TO_END, LOAD_PRELOAD_IF_ENABLED,
};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn video_track(m: &mut MovMuxer) -> u32 {
    let samples: Vec<MuxSample> = (0..3)
        .map(|i| MuxSample {
            data: vec![(i as u8).wrapping_add(1); 8],
            duration: 100,
            keyframe: i == 0,
            composition_offset: 0,
        })
        .collect();
    m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 16,
            height: 16,
        },
        600,
        samples,
        &[],
    )
}

#[test]
fn load_round_trips() {
    let load = Load {
        preload_start_time: 120,
        preload_duration: 600,
        preload_flags: LOAD_PRELOAD_IF_ENABLED,
        default_hints: LOAD_HINT_DOUBLE_BUFFER | LOAD_HINT_HIGH_QUALITY,
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_load_settings(id, Some(load)).expect("set load");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"load"));

    let d = open(bytes);
    let got = d.tracks[0].load.expect("load present");
    assert_eq!(got, load);
    assert!(got.preload_if_enabled());
    assert!(got.hint_double_buffer());
    assert!(got.hint_high_quality());
}

#[test]
fn load_to_end_sentinel_round_trips() {
    let load = Load {
        preload_start_time: 0,
        preload_duration: LOAD_PRELOAD_DURATION_TO_END,
        preload_flags: 0,
        default_hints: 0,
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_load_settings(id, Some(load)).expect("set load");
    let d = open(m.encode_to_vec().expect("encode"));
    let got = d.tracks[0].load.expect("load present");
    assert!(got.is_preload_to_end());
}

#[test]
fn load_none_emits_no_box() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    // Set then clear.
    m.set_track_load_settings(id, Some(Load::default()))
        .expect("set");
    m.set_track_load_settings(id, None).expect("clear");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(!bytes.windows(4).any(|w| w == b"load"));
    let d = open(bytes);
    assert!(d.tracks[0].load.is_none());
}

#[test]
fn load_unknown_track_errors() {
    let mut m = MovMuxer::new();
    assert!(m.set_track_load_settings(9, Some(Load::default())).is_err());
}
