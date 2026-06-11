//! Round 279 acceptance: `mjht` default Motion-JPEG Huffman table
//! extension at the demuxer surface.
//!
//! Builds a 1-track, 1-sample QTFF file whose `stsd` video entry
//! carries an `mjht` declaration, then asserts the demuxer routes the
//! body through [`oxideav_mov::parse_mjht`] into the typed
//! [`oxideav_mov::Mjht`] field on the sample description, preserving
//! the raw default-Huffman-table bytes verbatim.
//!
//! QTFF (2001-03-01) p. 94, Table 3-2 — "Video sample description
//! extensions" — and the Motion-JPEG "Huffman table offset == 0 ⇒
//! default" rule on pp. 95 – 96 are the sole spec sources consulted.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a representative single-table JPEG `DHT` body: a Tc/Th byte
/// (class 0 = DC, table id 0), 16 per-code-length counts, then the
/// symbol values those counts declare.
fn sample_dht_body() -> Vec<u8> {
    let mut body = Vec::with_capacity(1 + 16 + 3);
    body.push(0x00); // Tc=0 (DC), Th=0 (table 0)
    let mut counts = [0u8; 16];
    counts[1] = 3; // three codes of length 2
    body.extend_from_slice(&counts);
    body.extend_from_slice(&[0x04, 0x05, 0x06]); // symbol values
    body
}

/// Build a single-track QTFF file whose visual sample description's
/// extras blob carries the supplied `mjht` body. When `mjht_body` is
/// `None` the extras blob is left empty (the no-extension baseline).
fn build_qt_with_mjht(mjht_body: Option<&[u8]>) -> Vec<u8> {
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

    // Sample-description extras: just the mjht extension, if any.
    let mut extras = Vec::new();
    if let Some(body) = mjht_body {
        push_atom(&mut extras, *b"mjht", body);
    }

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
fn mjht_round_trips_through_demuxer() {
    // QTFF p. 94 Table 3-2: the default Motion-JPEG Huffman table is
    // surfaced verbatim for the codec to interpret.
    let dht = sample_dht_body();
    let bytes = build_qt_with_mjht(Some(&dht));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open mjht sample");
    let desc = &d.tracks[0].sample_descriptions[0];
    let m = desc.mjht.as_ref().expect("mjht parsed");
    assert_eq!(m.data, dht);
    assert_eq!(m.len(), 20);
    assert!(!m.is_empty());
}

#[test]
fn mjht_empty_body_round_trips_through_demuxer() {
    // QTFF Table 3-2 fixes no minimum length; a zero-byte body is
    // surfaced as an empty (but present) declaration.
    let bytes = build_qt_with_mjht(Some(&[]));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open empty mjht sample");
    let m = d.tracks[0].sample_descriptions[0]
        .mjht
        .as_ref()
        .expect("mjht parsed even when empty");
    assert!(m.is_empty());
    assert_eq!(m.len(), 0);
}

#[test]
fn missing_mjht_leaves_field_unset() {
    // A QTFF file with no `mjht` extension leaves the typed field
    // `None` — the implicit "use the field-level table" case.
    let bytes = build_qt_with_mjht(None);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open no-mjht sample");
    let desc = &d.tracks[0].sample_descriptions[0];
    assert!(desc.mjht.is_none(), "mjht left None when absent");
    // The sibling Table 3-2 fields stay independent.
    assert!(desc.mjqt.is_none());
    assert!(desc.fiel.is_none());
}

#[test]
fn mjht_preserves_non_dht_payload_verbatim() {
    // The container does not validate the JPEG-owned bytes; an
    // arbitrary payload survives byte-for-byte for the codec to vet.
    let body = vec![0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0xFF, 0x42];
    let bytes = build_qt_with_mjht(Some(&body));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open arbitrary mjht sample");
    let m = d.tracks[0].sample_descriptions[0]
        .mjht
        .as_ref()
        .expect("mjht parsed");
    assert_eq!(m.data, body);
}

#[test]
fn mjht_and_mjqt_coexist_in_one_sample_description() {
    // QTFF p. 94: the extensions are appended after the color table —
    // a Motion-JPEG writer typically emits BOTH default tables. Build
    // an extras blob carrying mjqt then mjht and assert each routes to
    // its own typed field.
    let dht = sample_dht_body();
    let dqt: Vec<u8> = {
        let mut b = Vec::with_capacity(65);
        b.push(0x00); // Pq=0, Tq=0
        b.extend((0u8..64).map(|i| i.wrapping_add(1)));
        b
    };

    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let payload = b"PAYLOAD!";
    push_atom(&mut out, *b"mdat", payload);
    let mdat_payload_offset: u32 = 28;

    let mut extras = Vec::new();
    push_atom(&mut extras, *b"mjqt", &dqt);
    push_atom(&mut extras, *b"mjht", &dht);

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

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open mjqt+mjht sample");
    let desc = &d.tracks[0].sample_descriptions[0];
    assert_eq!(desc.mjqt.as_ref().expect("mjqt parsed").data, dqt);
    assert_eq!(desc.mjht.as_ref().expect("mjht parsed").data, dht);
}
