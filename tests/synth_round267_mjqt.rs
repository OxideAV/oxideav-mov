//! Round 267 acceptance: `mjqt` default Motion-JPEG quantization
//! table extension at the demuxer surface.
//!
//! Builds a 1-track, 1-sample QTFF file whose `stsd` video entry
//! carries an `mjqt` declaration, then asserts the demuxer routes the
//! body through [`oxideav_mov::parse_mjqt`] into the typed
//! [`oxideav_mov::Mjqt`] field on the sample description, preserving
//! the raw default-quantization-table bytes verbatim.
//!
//! QTFF (2001-03-01) p. 94, Table 3-2 — "Video sample description
//! extensions" — and the Motion-JPEG "quantization table offset == 0
//! ⇒ default" rule on pp. 95 – 96 are the sole spec sources consulted.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a representative single-table JPEG `DQT` body: a Pq/Tq byte
/// (8-bit precision, table id 0) followed by 64 zig-zag entries.
fn sample_dqt_body() -> Vec<u8> {
    let mut body = Vec::with_capacity(65);
    body.push(0x00); // Pq=0, Tq=0
    body.extend((0u8..64).map(|i| i.wrapping_add(1)));
    body
}

/// Build a single-track QTFF file whose visual sample description's
/// extras blob carries the supplied `mjqt` body.
fn build_qt_with_mjqt(mjqt_body: &[u8]) -> Vec<u8> {
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

    // Sample-description extras: just the mjqt extension.
    let mut extras = Vec::new();
    push_atom(&mut extras, *b"mjqt", mjqt_body);

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
        &build_stsd_video(b"mjpa", 320, 240, &extras),
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
fn mjqt_round_trips_through_demuxer() {
    // QTFF p. 94 Table 3-2: the default Motion-JPEG quantization
    // table is surfaced verbatim for the codec to interpret.
    let dqt = sample_dqt_body();
    let bytes = build_qt_with_mjqt(&dqt);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open mjqt sample");
    let desc = &d.tracks[0].sample_descriptions[0];
    let m = desc.mjqt.as_ref().expect("mjqt parsed");
    assert_eq!(m.data, dqt);
    assert_eq!(m.len(), 65);
    assert!(!m.is_empty());
}

#[test]
fn mjqt_empty_body_round_trips_through_demuxer() {
    // QTFF Table 3-2 fixes no minimum length; a zero-byte body is
    // surfaced as an empty (but present) declaration.
    let bytes = build_qt_with_mjqt(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open empty mjqt sample");
    let m = d.tracks[0].sample_descriptions[0]
        .mjqt
        .as_ref()
        .expect("mjqt parsed even when empty");
    assert!(m.is_empty());
    assert_eq!(m.len(), 0);
}

#[test]
fn missing_mjqt_leaves_field_unset() {
    use oxideav_mov::SampleDescription;
    // A QTFF file with no `mjqt` extension leaves the typed field
    // `None` — the implicit "use the field-level table" case.
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
    let extras: Vec<u8> = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"mjpa", 320, 240, &extras),
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
    let d = MovDemuxer::open(cur).expect("open no-mjqt sample");
    let desc: &SampleDescription = &d.tracks[0].sample_descriptions[0];
    assert!(desc.mjqt.is_none(), "mjqt left None when absent");
}

#[test]
fn mjqt_preserves_non_dqt_payload_verbatim() {
    // The container does not validate the JPEG-owned bytes; an
    // arbitrary payload survives byte-for-byte for the codec to vet.
    let body = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF, 0x42];
    let bytes = build_qt_with_mjqt(&body);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open arbitrary mjqt sample");
    let m = d.tracks[0].sample_descriptions[0]
        .mjqt
        .as_ref()
        .expect("mjqt parsed");
    assert_eq!(m.data, body);
}
