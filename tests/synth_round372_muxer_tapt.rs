//! Round 372 — `MovMuxer` write-side Track Aperture Modes box (`tapt`,
//! Apple "Movie Atoms"). The demuxer already reads `tapt`'s `clef` /
//! `prof` / `enof` aperture rectangles onto `Track::tapt`; round 372
//! lets the muxer write them via [`MovMuxer::set_track_aperture`].
//!
//! Each test builds a file through [`MovMuxer`], re-opens it through
//! [`MovDemuxer`], and asserts the aperture rectangles round-trip.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, Tapt, TaptDims};

fn one_sample() -> Vec<MuxSample> {
    vec![MuxSample {
        data: vec![0x33u8; 8],
        duration: 1024,
        keyframe: true,
        composition_offset: 0,
    }]
}

fn add_video(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 720,
            height: 480,
        },
        30000,
        one_sample(),
        &[],
    )
}

fn add_audio(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        one_sample(),
        &[],
    )
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

#[test]
fn all_three_apertures_roundtrip() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    let tapt = Tapt {
        clef: Some(TaptDims::from_pixels(704, 480)),
        prof: Some(TaptDims::from_pixels(720, 480)),
        enof: Some(TaptDims::from_pixels(720, 486)),
    };
    m.set_track_aperture(v, tapt).expect("attach tapt");
    let bytes = m.encode_to_vec().expect("encode");

    assert!(bytes.windows(4).any(|w| w == b"tapt"));
    assert!(bytes.windows(4).any(|w| w == b"clef"));
    assert!(bytes.windows(4).any(|w| w == b"prof"));
    assert!(bytes.windows(4).any(|w| w == b"enof"));

    let d = open(bytes);
    let t = d.tracks[0].tapt.expect("tapt parsed");
    assert_eq!(t.clef, Some(TaptDims::from_pixels(704, 480)));
    assert_eq!(t.prof, Some(TaptDims::from_pixels(720, 480)));
    assert_eq!(t.enof, Some(TaptDims::from_pixels(720, 486)));
    // Integer pixel accessors.
    assert_eq!(t.clef.unwrap().width(), 704);
    assert_eq!(t.enof.unwrap().height(), 486);
}

#[test]
fn partial_aperture_only_emits_present_children() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    let tapt = Tapt {
        clef: Some(TaptDims::from_pixels(640, 480)),
        prof: None,
        enof: None,
    };
    m.set_track_aperture(v, tapt).expect("attach");
    let bytes = m.encode_to_vec().expect("encode");

    assert!(bytes.windows(4).any(|w| w == b"clef"));
    assert!(!bytes.windows(4).any(|w| w == b"prof"));
    assert!(!bytes.windows(4).any(|w| w == b"enof"));

    let d = open(bytes);
    let t = d.tracks[0].tapt.expect("tapt parsed");
    assert_eq!(t.clef, Some(TaptDims::from_pixels(640, 480)));
    assert_eq!(t.prof, None);
    assert_eq!(t.enof, None);
}

#[test]
fn fractional_fixed_point_preserved() {
    // A non-integer 16.16 value (e.g. 704.5 px = 0x02C0_8000) must
    // survive the round-trip bit-exact.
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    let half = TaptDims {
        width_fp: 0x02C0_8000,  // 704.5
        height_fp: 0x01E0_0000, // 480.0
    };
    let tapt = Tapt {
        clef: Some(half),
        prof: None,
        enof: None,
    };
    m.set_track_aperture(v, tapt).expect("attach");
    let d = open(m.encode_to_vec().expect("encode"));
    let t = d.tracks[0].tapt.expect("tapt parsed");
    assert_eq!(t.clef.unwrap().width_fp, 0x02C0_8000);
    assert_eq!(t.clef.unwrap().width(), 704); // floor
}

#[test]
fn no_aperture_emits_no_tapt() {
    let mut m = MovMuxer::new();
    let _ = add_video(&mut m);
    let bytes = m.encode_to_vec().expect("encode");
    assert!(!bytes.windows(4).any(|w| w == b"tapt"));
}

#[test]
fn aperture_on_audio_track_is_rejected() {
    let mut m = MovMuxer::new();
    let a = add_audio(&mut m);
    let err = m.set_track_aperture(
        a,
        Tapt {
            clef: Some(TaptDims::from_pixels(1, 1)),
            prof: None,
            enof: None,
        },
    );
    assert!(err.is_err(), "aperture on audio track must error");
}

#[test]
fn empty_aperture_is_rejected() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    let err = m.set_track_aperture(v, Tapt::default());
    assert!(err.is_err(), "all-None Tapt must error");
}

#[test]
fn unknown_track_id_is_rejected() {
    let mut m = MovMuxer::new();
    let _ = add_video(&mut m);
    let err = m.set_track_aperture(
        7,
        Tapt {
            clef: Some(TaptDims::from_pixels(1, 1)),
            prof: None,
            enof: None,
        },
    );
    assert!(err.is_err(), "unknown track id must error");
}

#[test]
fn aperture_does_not_corrupt_sample_data() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    m.set_track_aperture(
        v,
        Tapt {
            clef: Some(TaptDims::from_pixels(640, 480)),
            prof: None,
            enof: None,
        },
    )
    .expect("attach");
    let mut d = open(m.encode_to_vec().expect("encode"));
    let pkt = d.next_packet().expect("packet");
    assert_eq!(pkt.data, vec![0x33u8; 8]);
}
