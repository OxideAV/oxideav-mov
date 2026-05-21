//! Round 89 — Track Load Settings atom (`load`) parser wiring.
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01),
//! "Track Load Settings Atoms" (pp. 48–49). The `load` atom carries
//! per-track preloading hints (start/duration window in the movie
//! timescale + enable-mode + I/O quality flag bits). Round 89 wires
//! the parser into the demuxer's `trak` walk and surfaces the typed
//! [`oxideav_mov::Load`] body via:
//!
//! * `Track::load_settings()`
//! * `MovDemuxer::track_load(track_index)`
//!
//! Tests assemble a minimal single-video-track `qt  ` file with a
//! handful of `load` configurations, then verify the demuxer surfaces
//! the expected typed fields.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{
    MovDemuxer, LOAD_HINT_DOUBLE_BUFFER, LOAD_HINT_HIGH_QUALITY, LOAD_PRELOAD_ALWAYS,
    LOAD_PRELOAD_DURATION_TO_END, LOAD_PRELOAD_IF_ENABLED,
};

/// Build a 16-byte `load` payload — QTFF Figure 2-12 (p. 48).
fn build_load(start: u32, dur: u32, flags: u32, hints: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(16);
    p.extend_from_slice(&start.to_be_bytes());
    p.extend_from_slice(&dur.to_be_bytes());
    p.extend_from_slice(&flags.to_be_bytes());
    p.extend_from_slice(&hints.to_be_bytes());
    p
}

/// Build a one-video-track `qt  ` file with an optional `load` atom
/// inside `trak`. mvhd ts = 600, mdhd ts = 600, 4×30-tick samples.
fn build_qt_with_load(load_payload: Option<&[u8]>) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  ");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
        let mut trak = Vec::new();
        push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 120, 320, 240));
        if let Some(body) = load_payload {
            push_atom(&mut trak, *b"load", body);
        }
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 120));
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"vmhd", &build_vmhd());
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_video(b"avc1", 320, 240, &[]),
        );
        push_atom(&mut stbl, *b"stts", &build_stts_single(4, 30));
        push_atom(&mut stbl, *b"stsc", &build_stsc_single(4));
        push_atom(&mut stbl, *b"stsz", &build_stsz_constant(2, 4));
        push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

#[test]
fn no_load_atom_surfaces_none() {
    let bytes = build_qt_with_load(None);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open no-load");
    assert!(d.track_load(0).is_none());
    assert!(d.tracks[0].load_settings().is_none());
}

#[test]
fn load_atom_round_trips_canonical_fields() {
    // Preload 60 ticks starting at tick 0, "always", with double-buffer I/O.
    let body = build_load(0, 60, LOAD_PRELOAD_ALWAYS, LOAD_HINT_DOUBLE_BUFFER);
    let bytes = build_qt_with_load(Some(&body));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open with load atom");
    let l = d.track_load(0).expect("load atom parsed");
    assert_eq!(l.preload_start_time, 0);
    assert_eq!(l.preload_duration, 60);
    assert!(l.preload_always());
    assert!(!l.preload_if_enabled());
    assert!(l.hint_double_buffer());
    assert!(!l.hint_high_quality());
    assert!(!l.is_preload_to_end());
}

#[test]
fn load_preload_duration_minus_one_means_to_end_of_track() {
    let body = build_load(
        12,
        LOAD_PRELOAD_DURATION_TO_END,
        LOAD_PRELOAD_IF_ENABLED,
        LOAD_HINT_HIGH_QUALITY,
    );
    let bytes = build_qt_with_load(Some(&body));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open preload-to-end");
    let l = d.track_load(0).expect("load atom parsed");
    assert!(l.is_preload_to_end());
    assert_eq!(l.preload_start_time, 12);
    assert!(l.preload_if_enabled());
    assert!(!l.preload_always());
    assert!(l.hint_high_quality());
    assert!(!l.hint_double_buffer());
}

#[test]
fn load_combined_hint_bits_preserve_raw_field() {
    // Both spec hint bits set + a vendor-private 0x0080 bit.
    let hints = LOAD_HINT_DOUBLE_BUFFER | LOAD_HINT_HIGH_QUALITY | 0x0000_0080;
    let body = build_load(0, 0, LOAD_PRELOAD_ALWAYS, hints);
    let bytes = build_qt_with_load(Some(&body));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open combined-hints");
    let l = d.track_load(0).expect("load atom parsed");
    assert_eq!(l.default_hints, hints);
    assert!(l.hint_double_buffer());
    assert!(l.hint_high_quality());
    assert_eq!(l.default_hints & 0x0000_0080, 0x0000_0080);
}

#[test]
fn track_load_out_of_range_track_index_returns_none() {
    let body = build_load(0, 60, LOAD_PRELOAD_ALWAYS, 0);
    let bytes = build_qt_with_load(Some(&body));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open");
    assert!(d.track_load(42).is_none());
}
