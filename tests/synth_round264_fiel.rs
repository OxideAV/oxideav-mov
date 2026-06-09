//! Round 264 acceptance: `fiel` Field Handling extension plus the
//! typed `gama` accessor at the demuxer surface.
//!
//! Builds a 1-track, 1-sample QTFF file whose `stsd` video entry
//! carries a `fiel` declaration in each of the three spec-named
//! shapes (top-field-first, bottom-field-first, unknown) and a
//! progressive declaration, then asserts the demuxer:
//!
//! * routes the `fiel` body through [`oxideav_mov::parse_fiel`] into
//!   the typed [`oxideav_mov::Fiel`] field on the sample description,
//! * resolves the field-ordering byte through [`FieldOrdering`] in
//!   the four spec cases, and
//! * decodes the existing `gama` field through the new
//!   [`oxideav_mov::SampleDescription::gamma_value`] 16.16
//!   floating-point accessor.
//!
//! QTFF (2001-03-01) p. 94, Table 3-2 — "Video sample description
//! extensions" — is the sole spec source consulted.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{FieldOrdering, MovDemuxer};

/// Build a `stsd` extras blob carrying gama + fiel(field_count,
/// field_ordering). Used to drive `scan_video_extensions` end-to-end.
fn build_extensions_with_fiel(field_count: u8, field_ordering: u8) -> Vec<u8> {
    let mut out = Vec::new();
    // gama — 16.16 fixed-point gamma 2.2 (= 0x0002_3333). Matches
    // the value used by `synth_video_extensions.rs` so the typed
    // accessor's f64 result is stable across rounds.
    push_atom(&mut out, *b"gama", &0x0002_3333u32.to_be_bytes());
    push_atom(&mut out, *b"fiel", &[field_count, field_ordering]);
    out
}

/// Build a single-track QTFF file with the given `fiel` declaration
/// in the visual sample description's extras blob.
fn build_qt_with_fiel(field_count: u8, field_ordering: u8) -> Vec<u8> {
    let mut out = Vec::new();

    // ftyp — 'qt  '/0/'qt  '
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // mdat (8-byte sample payload)
    let payload = b"PAYLOAD!";
    push_atom(&mut out, *b"mdat", payload);
    let mdat_payload_offset: u32 = 28; // ftyp (20) + mdat header (8)

    // moov / mvhd
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 320, 240));

    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));

    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());

    let mut stbl = Vec::new();
    let extras = build_extensions_with_fiel(field_count, field_ordering);
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &extras),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_offset));

    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn fiel_progressive_round_trips_through_demuxer() {
    // QTFF p. 94: field_count=1 ⇒ progressive sample. The second
    // byte is undefined when the count is 1; we pick 0 (the
    // spec-named "Unknown" value) for a stable fixture.
    let bytes = build_qt_with_fiel(1, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open progressive fiel");
    let desc = &d.tracks[0].sample_descriptions[0];
    let f = desc.fiel.expect("fiel parsed");
    assert_eq!(f.field_count, 1);
    assert!(!f.is_interlaced());
    assert!(f.is_spec_field_count());
    assert_eq!(f.ordering(), Some(FieldOrdering::Unknown));
}

#[test]
fn fiel_top_field_first_round_trips_through_demuxer() {
    // QTFF p. 94: count=2, ordering=1 — T displayed earliest, T
    // stored first in the file (top-field-first interlaced).
    let bytes = build_qt_with_fiel(2, 1);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open T-first fiel");
    let f = d.tracks[0].sample_descriptions[0]
        .fiel
        .expect("fiel parsed");
    assert!(f.is_interlaced());
    assert_eq!(f.ordering(), Some(FieldOrdering::TopFieldFirst));
}

#[test]
fn fiel_bottom_field_first_round_trips_through_demuxer() {
    // QTFF p. 94: count=2, ordering=6 — B displayed earliest, B
    // stored first in the file (bottom-field-first interlaced).
    let bytes = build_qt_with_fiel(2, 6);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open B-first fiel");
    let f = d.tracks[0].sample_descriptions[0]
        .fiel
        .expect("fiel parsed");
    assert!(f.is_interlaced());
    assert_eq!(f.ordering(), Some(FieldOrdering::BottomFieldFirst));
}

#[test]
fn fiel_interlaced_unknown_ordering_round_trips_through_demuxer() {
    // QTFF p. 94: count=2, ordering=0 — "field ordering is unknown".
    // The decoder honours the interlaced flag but cannot pick a field
    // display order without an external heuristic.
    let bytes = build_qt_with_fiel(2, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open unknown-ordering fiel");
    let f = d.tracks[0].sample_descriptions[0]
        .fiel
        .expect("fiel parsed");
    assert!(f.is_interlaced());
    assert_eq!(f.ordering(), Some(FieldOrdering::Unknown));
}

#[test]
fn missing_fiel_leaves_field_unset() {
    // A QTFF file with no `fiel` extension is the implicit
    // progressive case — the typed field stays `None`.
    use oxideav_mov::*;
    let mut extras = Vec::new();
    push_atom(&mut extras, *b"gama", &0x0002_3333u32.to_be_bytes());
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let payload = b"PAYLOAD!";
    push_atom(&mut out, *b"mdat", payload);
    let mdat_payload_offset: u32 = 28;
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &extras),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open no-fiel sample");
    let desc: &SampleDescription = &d.tracks[0].sample_descriptions[0];
    assert!(desc.fiel.is_none(), "fiel left None when absent");
    // The gama extension is still parsed; the typed accessor
    // returns the 16.16 fixed-point value as f64.
    assert_eq!(desc.gamma, Some(0x0002_3333));
    // 0x0002_3333 / 65536 ≈ 2.1999969...; allow a strict equality
    // within an epsilon that matches the 16.16 representable step.
    let g = desc.gamma_value().expect("gamma value present");
    assert!(
        (g - (0x0002_3333u32 as f64 / 65536.0)).abs() < 1e-12,
        "gamma 16.16 round-trip: got {g}"
    );
    // Sanity-check the integer portion matches QTFF's "16.16 with
    // integer in high 16 bits" convention.
    assert_eq!((g.trunc() as u32), 2);
}

#[test]
fn gamma_value_returns_none_when_gama_absent() {
    // Build a sample description with NO `gama` extension. The typed
    // accessor must return `None` rather than substituting a default.
    use oxideav_mov::*;
    let extras: Vec<u8> = Vec::new();
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let payload = b"PAYLOAD!";
    push_atom(&mut out, *b"mdat", payload);
    let mdat_payload_offset: u32 = 28;
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &extras),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open extras-free sample");
    let desc: &SampleDescription = &d.tracks[0].sample_descriptions[0];
    assert!(desc.gamma.is_none());
    assert!(desc.gamma_value().is_none());
    assert!(desc.fiel.is_none());
}
