//! Round 379 — `MovMuxer` write-side **Track Matte atom**
//! (`matt` > `kmat`, QTFF pp. 44–45), a QuickTime-only `trak` child the
//! demuxer has long read (`Track::matte` via `parse_matt` / `parse_kmat`)
//! but the muxer could never write.
//! `MovMuxer::set_track_matte(track_id, Some(Matte))` emits the `matt`
//! wrapper + framed `kmat` child.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{CompressedMatte, Matte, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

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

/// Build a minimal 16-byte QTFF image-description structure naming a
/// codec FourCC: `[size:u32][format:4][reserved:6][dref_index:u16]`,
/// plus `extra` trailing bytes (folded into the size word).
fn image_description(fourcc: &[u8; 4], extra: usize) -> Vec<u8> {
    let total = 16 + extra;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend(std::iter::repeat(0u8).take(extra));
    out
}

#[test]
fn matte_round_trips() {
    let matte = Matte {
        compressed: CompressedMatte {
            version: 0,
            flags: 0,
            image_description: image_description(b"png ", 4),
            matte_data: vec![0x10, 0x20, 0x30, 0x40, 0x50],
        },
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_matte(id, Some(matte.clone()))
        .expect("set matte");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"matt"));
    assert!(bytes.windows(4).any(|w| w == b"kmat"));

    let d = open(bytes);
    let got = d.tracks[0].matte.as_ref().expect("matte present");
    assert_eq!(*got, matte);
    assert_eq!(got.compressed.data_format(), Some(*b"png "));
    assert_eq!(got.compressed.matte_data, [0x10, 0x20, 0x30, 0x40, 0x50]);
}

#[test]
fn matte_empty_data_round_trips() {
    let matte = Matte {
        compressed: CompressedMatte {
            version: 0,
            flags: 0,
            image_description: image_description(b"raw ", 0),
            matte_data: Vec::new(),
        },
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_matte(id, Some(matte.clone()))
        .expect("set matte");
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(*d.tracks[0].matte.as_ref().unwrap(), matte);
}

#[test]
fn matte_none_emits_no_box() {
    let matte = Matte {
        compressed: CompressedMatte {
            version: 0,
            flags: 0,
            image_description: image_description(b"raw ", 0),
            matte_data: vec![1, 2, 3],
        },
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_matte(id, Some(matte)).expect("set");
    m.set_track_matte(id, None).expect("clear");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(!bytes.windows(4).any(|w| w == b"matt"));
    let d = open(bytes);
    assert!(d.tracks[0].matte.is_none());
}

#[test]
fn matte_unknown_track_errors() {
    let matte = Matte {
        compressed: CompressedMatte {
            version: 0,
            flags: 0,
            image_description: image_description(b"raw ", 0),
            matte_data: Vec::new(),
        },
    };
    let mut m = MovMuxer::new();
    assert!(m.set_track_matte(9, Some(matte)).is_err());
}
