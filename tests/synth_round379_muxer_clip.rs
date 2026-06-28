//! Round 379 — `MovMuxer` write-side **Track Clipping atom**
//! (`clip` > `crgn`, QTFF pp. 43–44), a QuickTime-only `trak` child the
//! demuxer has long read (`Track::clipping` via `parse_clip` /
//! `parse_crgn`) but the muxer could never write.
//! `MovMuxer::set_track_clipping(track_id, Some(Clipping))` emits the
//! `clip` wrapper + framed `crgn` child.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    Clipping, ClippingRegion, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, QdRect,
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
fn rectangular_clip_round_trips() {
    let clip = Clipping {
        region: ClippingRegion::rectangular(QdRect {
            top: 10,
            left: 20,
            bottom: 110,
            right: 220,
        }),
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_clipping(id, Some(clip.clone()))
        .expect("set clip");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"clip"));
    assert!(bytes.windows(4).any(|w| w == b"crgn"));

    let d = open(bytes);
    let got = d.tracks[0].clipping.as_ref().expect("clip present");
    assert_eq!(*got, clip);
    assert!(got.region.is_rectangular());
    assert_eq!(got.region.bounding_box.width(), 200);
    assert_eq!(got.region.bounding_box.height(), 100);
}

#[test]
fn clip_with_scanline_tail_round_trips() {
    let clip = Clipping {
        region: ClippingRegion {
            region_size: 14,
            bounding_box: QdRect {
                top: 0,
                left: 0,
                bottom: 50,
                right: 50,
            },
            region_data: vec![0xAA, 0xBB, 0xCC, 0xDD],
        },
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_clipping(id, Some(clip.clone()))
        .expect("set clip");
    let d = open(m.encode_to_vec().expect("encode"));
    let got = d.tracks[0].clipping.as_ref().expect("clip present");
    assert_eq!(*got, clip);
    assert!(!got.region.is_rectangular());
    assert_eq!(got.region.region_data, [0xAA, 0xBB, 0xCC, 0xDD]);
}

#[test]
fn negative_origin_clip_round_trips() {
    let clip = Clipping {
        region: ClippingRegion::rectangular(QdRect {
            top: -32,
            left: -64,
            bottom: 128,
            right: 256,
        }),
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_clipping(id, Some(clip.clone()))
        .expect("set clip");
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(*d.tracks[0].clipping.as_ref().unwrap(), clip);
}

#[test]
fn clip_none_emits_no_box() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_clipping(
        id,
        Some(Clipping {
            region: ClippingRegion::rectangular(QdRect::default()),
        }),
    )
    .expect("set");
    m.set_track_clipping(id, None).expect("clear");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(!bytes.windows(4).any(|w| w == b"clip"));
    let d = open(bytes);
    assert!(d.tracks[0].clipping.is_none());
}

#[test]
fn clip_unknown_track_errors() {
    let mut m = MovMuxer::new();
    assert!(m
        .set_track_clipping(
            9,
            Some(Clipping {
                region: ClippingRegion::rectangular(QdRect::default()),
            }),
        )
        .is_err());
}
