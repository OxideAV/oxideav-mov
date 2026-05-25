//! Round 140 — Clipping atom (`clip`) + Clipping Region atom (`crgn`)
//! decode at both movie scope and track scope.
//!
//! Exercises the demuxer's `clip` surface (QTFF p. 43 / p. 44) against
//! hand-built QuickTime files whose `moov` and `moov/trak` carry an
//! optional Clipping atom alongside the usual `mvhd` + `trak`
//! children. The wrapper atom is QuickTime-only (ISO BMFF does not
//! define it); these tests verify the demuxer:
//!
//! * surfaces the QuickDraw bounding-box rectangle byte-for-byte at
//!   both scopes (movie-level via [`MovDemuxer::clipping`], track-level
//!   via [`oxideav_mov::Track::clipping`]);
//! * reports `None` at both scopes for a file that omits the atom;
//! * preserves the opaque QuickDraw scanline tail when present;
//! * follows the first-wins duplicate-merge policy when a malformed
//!   writer emits two `clip` atoms at the same scope;
//! * rejects a malformed `crgn` body (`region_size < 10`) at open
//!   time rather than silently producing an absent surface.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{Clipping, MovDemuxer, QdRect};

/// Build a `crgn` body (post-atom-header) with the given bounding box
/// and optional scanline tail per QTFF p. 44.
fn build_crgn_body(bbox: (i16, i16, i16, i16), tail: &[u8]) -> Vec<u8> {
    let region_size = 10u16 + tail.len() as u16;
    let mut p = Vec::with_capacity(region_size as usize);
    p.extend_from_slice(&region_size.to_be_bytes());
    p.extend_from_slice(&bbox.0.to_be_bytes());
    p.extend_from_slice(&bbox.1.to_be_bytes());
    p.extend_from_slice(&bbox.2.to_be_bytes());
    p.extend_from_slice(&bbox.3.to_be_bytes());
    p.extend_from_slice(tail);
    p
}

/// Build a `clip` wrapper body carrying a single `crgn` child whose
/// body is `crgn_body` (the canonical Figure 2-8 shape).
fn build_clip_body(crgn_body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_atom(&mut out, *b"crgn", crgn_body);
    out
}

/// Build a minimal one-video-track QuickTime file. The caller may
/// inject any number of `clip` payloads at movie scope (siblings of
/// `mvhd` and `trak`) and/or inside the single `trak` (sibling of
/// `tkhd` and `mdia`). QTFF Figure 2-2 (p. 32) places `clip` inside the
/// movie atom; Figure 2-6 (p. 41) places a clipping atom inside
/// individual tracks.
fn build_qt_with_clips(movie_clips: &[Vec<u8>], track_clips: &[Vec<u8>]) -> Vec<u8> {
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
        // Track-level clip atoms — siblings of tkhd and mdia (QTFF
        // p. 41). Emitted before mdia so the walker sees them first;
        // ordering inside the track atom is not fixed by the spec.
        for payload in track_clips {
            push_atom(&mut trak, *b"clip", payload);
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

        // Movie-level clip atoms (QTFF p. 32 Figure 2-2 — siblings of
        // mvhd, trak, udta, ctab). Emitted after `trak` so the test
        // validates the walker recognises `clip` regardless of its
        // order relative to other children.
        for payload in movie_clips {
            push_atom(&mut moov, *b"clip", payload);
        }

        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

fn assert_bbox(clip: &Clipping, expected: QdRect) {
    assert_eq!(clip.region.bounding_box, expected);
    assert_eq!(clip.region.region_size, 10);
    assert!(clip.region.region_data.is_empty());
    assert!(clip.region.is_rectangular());
}

#[test]
fn movie_level_rectangular_clip_round_trips() {
    // A 200x100 mask anchored at (10, 20). Movie-scope; track-scope
    // absent.
    let crgn = build_crgn_body((10, 20, 110, 220), &[]);
    let clip = build_clip_body(&crgn);
    let file = build_qt_with_clips(&[clip], &[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open movie-clip fixture");

    let movie_clip = d.clipping.as_ref().expect("movie-level clip populated");
    assert_bbox(
        movie_clip,
        QdRect {
            top: 10,
            left: 20,
            bottom: 110,
            right: 220,
        },
    );
    assert_eq!(movie_clip.region.bounding_box.width(), 200);
    assert_eq!(movie_clip.region.bounding_box.height(), 100);
    // Track-level surface stays None when only movie-level clip is
    // declared.
    assert!(d.tracks[0].clipping.is_none());
}

#[test]
fn track_level_rectangular_clip_round_trips() {
    // A 50x50 mask anchored at (0, 0) — track-scope only; the movie
    // declares no clipping.
    let crgn = build_crgn_body((0, 0, 50, 50), &[]);
    let clip = build_clip_body(&crgn);
    let file = build_qt_with_clips(&[], &[clip]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open track-clip fixture");

    assert!(d.clipping.is_none(), "no movie-level clip declared");
    let track_clip = d.tracks[0]
        .clipping
        .as_ref()
        .expect("track-level clip populated");
    assert_bbox(
        track_clip,
        QdRect {
            top: 0,
            left: 0,
            bottom: 50,
            right: 50,
        },
    );
}

#[test]
fn both_scopes_independently_populated() {
    // Movie-scope clip and track-scope clip coexist with different
    // rectangles; the demuxer surfaces each on its own field.
    let movie_crgn = build_crgn_body((1, 2, 101, 202), &[]);
    let movie_clip = build_clip_body(&movie_crgn);
    let track_crgn = build_crgn_body((5, 6, 55, 56), &[]);
    let track_clip = build_clip_body(&track_crgn);
    let file = build_qt_with_clips(&[movie_clip], &[track_clip]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open both-scope clip fixture");

    let m = d.clipping.as_ref().expect("movie-level clip populated");
    assert_eq!(m.region.bounding_box.top, 1);
    assert_eq!(m.region.bounding_box.left, 2);
    assert_eq!(m.region.bounding_box.bottom, 101);
    assert_eq!(m.region.bounding_box.right, 202);

    let t = d.tracks[0]
        .clipping
        .as_ref()
        .expect("track-level clip populated");
    assert_eq!(t.region.bounding_box.top, 5);
    assert_eq!(t.region.bounding_box.left, 6);
    assert_eq!(t.region.bounding_box.bottom, 55);
    assert_eq!(t.region.bounding_box.right, 56);
}

#[test]
fn clip_with_scanline_tail_preserved_at_both_scopes() {
    // 6 bytes of opaque QuickDraw scanline payload — the demuxer
    // surfaces them verbatim rather than decoding.
    let scanline: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    let movie_crgn = build_crgn_body((0, 0, 200, 200), &scanline);
    let movie_clip = build_clip_body(&movie_crgn);
    let track_crgn = build_crgn_body((10, 10, 90, 90), &scanline);
    let track_clip = build_clip_body(&track_crgn);
    let file = build_qt_with_clips(&[movie_clip], &[track_clip]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open scanline-tail clip fixture");

    let m = d.clipping.as_ref().expect("movie clip populated");
    assert_eq!(m.region.region_size, 16);
    assert_eq!(m.region.region_data, scanline);
    assert!(!m.region.is_rectangular());

    let t = d.tracks[0].clipping.as_ref().expect("track clip populated");
    assert_eq!(t.region.region_size, 16);
    assert_eq!(t.region.region_data, scanline);
    assert!(!t.region.is_rectangular());
}

#[test]
fn clip_absent_yields_none_at_both_scopes() {
    let file = build_qt_with_clips(&[], &[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open clip-less fixture");
    assert!(d.clipping.is_none());
    assert!(d.tracks[0].clipping.is_none());
}

#[test]
fn duplicate_movie_clip_keeps_first() {
    // First-wins on the rare duplicate case (shares the conservative
    // merge policy with mvhd / pdin / ctab).
    let first_crgn = build_crgn_body((1, 1, 11, 11), &[]);
    let first_clip = build_clip_body(&first_crgn);
    let second_crgn = build_crgn_body((99, 99, 199, 199), &[]);
    let second_clip = build_clip_body(&second_crgn);
    let file = build_qt_with_clips(&[first_clip, second_clip], &[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open duplicate-movie-clip fixture");
    let m = d.clipping.as_ref().expect("first movie clip retained");
    assert_eq!(m.region.bounding_box.top, 1);
    assert_eq!(m.region.bounding_box.right, 11);
}

#[test]
fn malformed_crgn_rejected_at_open_time() {
    // region_size = 8 violates QTFF p. 44 (field counts itself + the
    // 8-byte bounding box, so any value < 10 is malformed). The open
    // must fail rather than silently produce an absent surface.
    let mut bad_crgn = build_crgn_body((0, 0, 10, 10), &[]);
    bad_crgn[0] = 0x00;
    bad_crgn[1] = 0x08;
    let clip = build_clip_body(&bad_crgn);
    let file = build_qt_with_clips(&[clip], &[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let err = match MovDemuxer::open(cur) {
        Ok(_) => panic!("malformed crgn should have been rejected at open"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("region_size"),
        "error names the offending field; got: {msg}"
    );
}
