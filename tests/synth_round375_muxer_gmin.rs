//! Round 375 — `MovMuxer` write-side **gmhd/gmin override** and
//! **gmhd/text matrix override** (QTFF p. 65 / p. 144). Time-code and
//! text tracks emit a Base Media Information Header (`gmhd`) wrapping a
//! Generic Media Information header (`gmin`); a text track additionally
//! carries a `text` media-information atom with a transformation matrix.
//! Earlier rounds always wrote a default `gmin` (copy graphics mode, no
//! opcolor, centred balance) and an identity text matrix; round 375 lets
//! the caller override both via [`MovMuxer::set_track_gmin`] and
//! [`MovMuxer::set_text_header_matrix`].
//!
//! Each test builds a movie through [`MovMuxer`], re-opens it through
//! [`MovDemuxer`], and asserts the overridden `gmin` / `text` matrix
//! round-trips onto the parsed `Track::gmhd`.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    Gmin, GraphicsMode, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, Tcmi, TextSampleDescription,
};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn one_sample(data: Vec<u8>) -> Vec<MuxSample> {
    vec![MuxSample {
        data,
        duration: 100,
        keyframe: true,
        composition_offset: 0,
    }]
}

fn nondrop_tcmi() -> Tcmi {
    Tcmi {
        text_font: 3,
        text_face: 0,
        text_size: 12,
        bg_color: [0, 0, 0],
        fg_color: [0xFFFF, 0xFFFF, 0xFFFF],
        font_name: "Helvetica".into(),
    }
}

fn timecode_desc() -> oxideav_mov::Tmcd {
    oxideav_mov::Tmcd {
        flags: 0,
        time_scale: 30000,
        frame_duration: 1001,
        number_of_frames: 30,
        source_name: None,
    }
}

#[test]
fn timecode_gmin_override_roundtrips() {
    let mut m = MovMuxer::new();
    let id = m.add_track(
        MuxTrackKind::Timecode {
            description: timecode_desc(),
            tcmi: nondrop_tcmi(),
        },
        30000,
        one_sample(vec![0u8; 4]),
        &[],
    );
    // Transparent compositing mode (uses opcolor) + a green opcolor +
    // a non-centred balance — every field exercised.
    let gmin = Gmin {
        graphics_mode: GraphicsMode::Transparent.raw(),
        opcolor: [0, 0xFFFF, 0],
        balance: 0x0040, // +0.25 in 8.8 fixed-point
    };
    m.set_track_gmin(id, gmin).expect("set gmin");
    let bytes = m.encode_to_vec().expect("encode");

    let d = open(bytes);
    let t = &d.tracks[0];
    let g = t.gmhd.as_ref().expect("gmhd parsed");
    let parsed = g.gmin.expect("gmin parsed");
    assert_eq!(parsed.graphics_mode, GraphicsMode::Transparent.raw());
    assert_eq!(parsed.graphics_mode_kind(), GraphicsMode::Transparent);
    assert!(parsed.graphics_mode_kind().uses_opcolor());
    assert_eq!(parsed.opcolor, [0, 0xFFFF, 0]);
    assert_eq!(parsed.balance, 0x0040);
    assert!((parsed.balance_as_f32() - 0.25).abs() < 1e-6);
    // The tcmi still rides along unchanged.
    assert_eq!(g.tcmi.as_ref().expect("tcmi").font_name, "Helvetica");
}

#[test]
fn timecode_gmin_default_when_unset() {
    let mut m = MovMuxer::new();
    let _ = m.add_track(
        MuxTrackKind::Timecode {
            description: timecode_desc(),
            tcmi: nondrop_tcmi(),
        },
        30000,
        one_sample(vec![0u8; 4]),
        &[],
    );
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    let g = d.tracks[0].gmhd.as_ref().expect("gmhd");
    let parsed = g.gmin.expect("gmin");
    // Default: copy graphics mode, no opcolor, centred balance.
    assert_eq!(parsed.graphics_mode_kind(), GraphicsMode::Copy);
    assert_eq!(parsed.opcolor, [0, 0, 0]);
    assert_eq!(parsed.balance, 0);
}

#[test]
fn text_gmin_and_matrix_override_roundtrip() {
    let mut m = MovMuxer::new();
    let id = m.add_track(
        MuxTrackKind::Text {
            description: TextSampleDescription::default(),
        },
        600,
        one_sample(vec![0u8, 2, b'h', b'i']),
        &[],
    );
    let gmin = Gmin {
        graphics_mode: GraphicsMode::DitherCopy.raw(),
        opcolor: [0x1111, 0x2222, 0x3333],
        balance: -64, // -0.25
    };
    m.set_track_gmin(id, gmin).expect("set gmin on text");
    // A non-identity matrix: 2× horizontal scale, 90px down translate.
    let mut matrix = [0i32; 9];
    matrix[0] = 0x0002_0000; // a = 2.0
    matrix[4] = 0x0001_0000; // d = 1.0
    matrix[7] = 90 << 16; // ty = 90.0
    matrix[8] = 0x4000_0000; // w = 1.0 (2.30)
    m.set_text_header_matrix(id, matrix).expect("set matrix");
    let bytes = m.encode_to_vec().expect("encode");

    let d = open(bytes);
    let g = d.tracks[0].gmhd.as_ref().expect("gmhd");
    let parsed = g.gmin.expect("gmin");
    assert_eq!(parsed.graphics_mode_kind(), GraphicsMode::DitherCopy);
    assert_eq!(parsed.opcolor, [0x1111, 0x2222, 0x3333]);
    assert_eq!(parsed.balance, -64);
    let th = g.text.expect("text header parsed");
    assert_eq!(th.matrix, matrix);
}

#[test]
fn text_matrix_default_is_identity() {
    let mut m = MovMuxer::new();
    let _ = m.add_track(
        MuxTrackKind::Text {
            description: TextSampleDescription::default(),
        },
        600,
        one_sample(vec![0u8, 0]),
        &[],
    );
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    let g = d.tracks[0].gmhd.as_ref().expect("gmhd");
    let th = g.text.expect("text header");
    assert_eq!(th.matrix[0], 0x0001_0000);
    assert_eq!(th.matrix[4], 0x0001_0000);
    assert_eq!(th.matrix[8], 0x4000_0000);
}

#[test]
fn set_gmin_rejects_video_track() {
    let mut m = MovMuxer::new();
    let id = m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 16,
            height: 16,
        },
        600,
        one_sample(vec![0u8; 8]),
        &[],
    );
    assert!(m.set_track_gmin(id, Gmin::default()).is_err());
}

#[test]
fn set_text_matrix_rejects_timecode_track() {
    let mut m = MovMuxer::new();
    let id = m.add_track(
        MuxTrackKind::Timecode {
            description: timecode_desc(),
            tcmi: nondrop_tcmi(),
        },
        30000,
        one_sample(vec![0u8; 4]),
        &[],
    );
    assert!(m.set_text_header_matrix(id, [0i32; 9]).is_err());
}

#[test]
fn set_gmin_unknown_track_errors() {
    let mut m = MovMuxer::new();
    assert!(m.set_track_gmin(99, Gmin::default()).is_err());
}
