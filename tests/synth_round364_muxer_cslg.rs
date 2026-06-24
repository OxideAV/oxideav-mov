//! Round 364 — `MovMuxer` write-side `cslg` (CompositionToDecodeBox)
//! emission at `stbl` scope (ISO/IEC 14496-12 §8.6.1.4).
//!
//! The crate already *reads* `cslg` (`parse_cslg` →
//! [`oxideav_mov::Cslg`], cross-validated against `ctts` by the
//! demuxer). Round 364 lets the muxer *write* it, closing the
//! demux↔mux symmetry gap for a B-frame-reorder track: a `cslg`
//! summarises the composition-vs-decode timeline so a player can derive
//! the presentation-timeline bounds without scanning every `ctts` run.
//!
//! [`MovMuxer::auto_cslg`] derives the five fields from the track's
//! per-sample composition offsets + durations;
//! [`MovMuxer::set_cslg`] writes caller-supplied bounds. The box is
//! emitted right after `ctts` in the `stbl` and auto-promotes from
//! version 0 (`int(32)`) to version 1 (`int(64)`) when any field leaves
//! the signed-32-bit range.
//!
//! These tests build a file through [`MovMuxer`], confirm the `cslg`
//! box appears after `ctts`, and re-parse the emitted body through
//! [`oxideav_mov::parse_cslg`] to verify every field round-trips —
//! plus a full demux round-trip to prove the demuxer's `cslg`/`ctts`
//! cross-validation accepts the auto-derived box.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{parse_cslg, Cslg, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

/// Build a video MOV from a list of (composition_offset, duration,
/// keyframe) tuples on the only track. Optionally attach a cslg.
fn build_video_mov(frames: &[(i32, u32, bool)], cslg: Option<Cslg>, auto: bool) -> Vec<u8> {
    let samples: Vec<MuxSample> = frames
        .iter()
        .enumerate()
        .map(|(i, &(off, dur, kf))| MuxSample {
            data: vec![(0xE0 | (i & 0x0F)) as u8; 12],
            duration: dur,
            keyframe: kf,
            composition_offset: off,
        })
        .collect();
    let mut m = MovMuxer::new().with_movie_timescale(600);
    let id = m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 64,
            height: 48,
        },
        600,
        samples,
        &[],
    );
    if auto {
        m.auto_cslg(id).expect("auto cslg");
    } else if let Some(c) = cslg {
        m.set_cslg(id, c).expect("set cslg");
    }
    m.encode_to_vec().expect("encode video MOV")
}

/// Slice the first `cslg` box body out of a muxed file.
fn first_cslg_body(bytes: &[u8]) -> &[u8] {
    let pos = bytes
        .windows(4)
        .position(|w| w == b"cslg")
        .expect("cslg present in stream");
    let size = u32::from_be_bytes([
        bytes[pos - 4],
        bytes[pos - 3],
        bytes[pos - 2],
        bytes[pos - 1],
    ]) as usize;
    &bytes[pos + 4..(pos - 4) + size]
}

#[test]
fn auto_cslg_derives_bounds_from_b_frame_track() {
    // Classic I,P,B,B reorder. DTS = 0,10,20,30; offsets shift CT.
    //   sample 0 (I): off  10  dur 10 ⇒ CT  10
    //   sample 1 (P): off  30  dur 10 ⇒ CT  40
    //   sample 2 (B): off   0  dur 10 ⇒ CT  20
    //   sample 3 (B): off -10  dur 10 ⇒ CT  20
    let frames = [
        (10, 10, true),
        (30, 10, false),
        (0, 10, false),
        (-10, 10, false),
    ];
    let bytes = build_video_mov(&frames, None, true);

    assert!(bytes.windows(4).any(|w| w == b"cslg"));
    let c = parse_cslg(first_cslg_body(&bytes)).expect("parse cslg");

    // least/greatest = min/max offset across the track.
    assert_eq!(c.least_decode_to_display_delta, -10);
    assert_eq!(c.greatest_decode_to_display_delta, 30);
    // shift = max(0, -least) = 10 (keeps shifted CTS >= DTS).
    assert_eq!(c.composition_to_dts_shift, 10);
    // composition_start = min CT = 10; end = max(CT + dur) = 40 + 10 = 50.
    assert_eq!(c.composition_start_time, 10);
    assert_eq!(c.composition_end_time, 50);
}

#[test]
fn auto_cslg_all_zero_offsets_emits_zero_bounds() {
    // No reorder: every offset 0. cslg is still emitted (opt-in), with
    // all-zero deltas and shift.
    let frames = [(0, 100, true), (0, 100, true), (0, 100, true)];
    let bytes = build_video_mov(&frames, None, true);
    let c = parse_cslg(first_cslg_body(&bytes)).unwrap();
    assert_eq!(c.composition_to_dts_shift, 0);
    assert_eq!(c.least_decode_to_display_delta, 0);
    assert_eq!(c.greatest_decode_to_display_delta, 0);
    assert_eq!(c.composition_start_time, 0);
    assert_eq!(c.composition_end_time, 300);
}

#[test]
fn cslg_follows_ctts_in_stbl() {
    let frames = [(5, 10, true), (0, 10, false)];
    let bytes = build_video_mov(&frames, None, true);
    let ctts_pos = bytes.windows(4).position(|w| w == b"ctts").unwrap();
    let cslg_pos = bytes.windows(4).position(|w| w == b"cslg").unwrap();
    assert!(ctts_pos < cslg_pos, "cslg must follow ctts (§6.2.3 order)");
}

#[test]
fn no_cslg_when_not_opted_in() {
    // A reorder track without auto_cslg / set_cslg emits no cslg.
    let frames = [(5, 10, true), (0, 10, false)];
    let bytes = build_video_mov(&frames, None, false);
    assert!(!bytes.windows(4).any(|w| w == b"cslg"));
}

#[test]
fn explicit_cslg_roundtrips_verbatim() {
    let want = Cslg {
        composition_to_dts_shift: 7,
        least_decode_to_display_delta: -7,
        greatest_decode_to_display_delta: 21,
        composition_start_time: 0,
        composition_end_time: 4000,
    };
    // Offsets here must keep the demuxer's ctts-within-cslg check happy:
    // ctts range [-7, 21] sits inside [least, greatest] = [-7, 21].
    let frames = [(-7, 10, true), (21, 10, false), (0, 10, false)];
    let bytes = build_video_mov(&frames, Some(want), false);
    let got = parse_cslg(first_cslg_body(&bytes)).unwrap();
    assert_eq!(got, want);
}

#[test]
fn cslg_promotes_to_v1_past_32_bits() {
    // A composition_end_time beyond i32::MAX forces version 1.
    let big = Cslg {
        composition_to_dts_shift: 0,
        least_decode_to_display_delta: 0,
        greatest_decode_to_display_delta: 0,
        composition_start_time: 0,
        composition_end_time: i32::MAX as i64 + 1000,
    };
    let frames = [(0, 100, true)];
    let bytes = build_video_mov(&frames, Some(big), false);
    let body = first_cslg_body(&bytes);
    assert_eq!(body[0], 1, "version 1 for a 64-bit field");
    let got = parse_cslg(body).unwrap();
    assert_eq!(got, big);
}

#[test]
fn auto_cslg_full_demux_roundtrip() {
    // The auto-derived cslg passes the demuxer's cslg/ctts
    // cross-validation, and sample data stays intact.
    let frames = [
        (10, 10, true),
        (30, 10, false),
        (0, 10, false),
        (-10, 10, false),
    ];
    let bytes = build_video_mov(&frames, None, true);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open muxed file with cslg");
    for i in 0..4u8 {
        let pkt = d.next_packet().expect("packet");
        assert_eq!(pkt.data, vec![0xE0 | i; 12], "sample {i} bytes");
    }
}

#[test]
fn set_cslg_rejects_unknown_track() {
    let mut m = MovMuxer::new();
    let err = m.set_cslg(42, Cslg::default());
    assert!(err.is_err(), "unknown track id must be rejected");
    let err2 = m.auto_cslg(42);
    assert!(err2.is_err(), "unknown track id must be rejected");
}
