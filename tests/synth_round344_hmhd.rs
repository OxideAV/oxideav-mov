//! Round 344 acceptance: Hint Media Header Box (`hmhd`, ISO/IEC
//! 14496-12 §12.4.2) surfaced at the demuxer through `Track::hmhd`.
//!
//! Builds a single-track file whose `hdlr` declares the `hint`
//! component subtype and whose `minf` carries an `hmhd`, then asserts
//! the demuxer routes the body through `oxideav_mov::parse_hmhd` into
//! the typed `Hmhd` field and that `Hdlr::is_hint()` recognises the
//! handler. A control track (`vide`) confirms the field stays `None`
//! for non-hint media.
//!
//! Spec source consulted: ISO/IEC 14496-12:2015 §12.4.2.2. No external
//! implementation read.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

fn build_hmhd(max_pdu: u16, avg_pdu: u16, max_br: u32, avg_br: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&max_pdu.to_be_bytes());
    p.extend_from_slice(&avg_pdu.to_be_bytes());
    p.extend_from_slice(&max_br.to_be_bytes());
    p.extend_from_slice(&avg_br.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes()); // reserved
    p
}

/// A single-track file with a `hint` handler and the given media header.
/// `media_header` is the `(fourcc, body)` of the box placed in `minf`.
fn build_track_file(hdlr_subtype: &[u8; 4], media_header: (&[u8; 4], Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"HINTPDU0");
    let mdat_payload_offset: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 0, 0));

    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", hdlr_subtype));

    let mut minf = Vec::new();
    push_atom(&mut minf, *media_header.0, &media_header.1);

    let mut stbl = Vec::new();
    // A hint sample description uses a protocol FourCC (e.g. `rtp `);
    // the universal 16-byte header plus an opaque body is enough here.
    let mut hint_stsd = Vec::new();
    hint_stsd.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    hint_stsd.extend_from_slice(&1u32.to_be_bytes()); // entry count
    hint_stsd.extend_from_slice(&(16u32 + 4).to_be_bytes()); // entry size
    hint_stsd.extend_from_slice(b"rtp "); // protocol format
    hint_stsd.extend_from_slice(&[0u8; 6]); // reserved
    hint_stsd.extend_from_slice(&1u16.to_be_bytes()); // dref index
    hint_stsd.extend_from_slice(&[0u8; 4]); // opaque body
    push_atom(&mut stbl, *b"stsd", &hint_stsd);
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

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open track")
}

#[test]
fn hint_track_surfaces_hmhd() {
    let hmhd = build_hmhd(1480, 1024, 8_000_000, 4_500_000);
    let d = open(build_track_file(b"hint", (b"hmhd", hmhd)));
    let t = &d.tracks[0];
    assert!(t.hdlr.is_hint());
    let h = t.hmhd.expect("hmhd parsed");
    assert_eq!(h.max_pdu_size, 1480);
    assert_eq!(h.avg_pdu_size, 1024);
    assert_eq!(h.max_bitrate, 8_000_000);
    assert_eq!(h.avg_bitrate, 4_500_000);
}

#[test]
fn non_hint_track_leaves_hmhd_unset() {
    // A `vide` track with a `vmhd` carries no hint header.
    let vmhd = build_vmhd();
    let d = open(build_track_file(b"vide", (b"vmhd", vmhd)));
    let t = &d.tracks[0];
    assert!(!t.hdlr.is_hint());
    assert!(t.hmhd.is_none());
}
