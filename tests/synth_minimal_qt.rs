//! Hand-rolled minimal QTFF file → demuxer → one packet round-trip.
//!
//! Builds (in memory) a 1-track, 1-sample QuickTime movie with the
//! `qt  ` brand, opens it with `MovDemuxer`, and asserts the
//! demuxer surface fields and the single emitted packet. This is
//! the round-1 acceptance gate.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::MovDemuxer;

/// Append a classic QTFF atom with its 4-byte size + 4-byte type
/// header followed by the payload.
fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
    let size: u32 = (8 + body.len()) as u32;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&fourcc);
    out.extend_from_slice(body);
}

/// Build the minimal QTFF file:
///
/// * `ftyp` — major `qt  `, compat `qt  `
/// * `mdat` — 8-byte payload `b"PAYLOAD!"`
/// * `moov` / `mvhd` / `trak` / `tkhd` / `mdia` / `mdhd` / `hdlr`
///   / `minf` / `vmhd` / `stbl` / `stsd` / `stts` / `stsc` /
///   `stsz` / `stco`
///
/// QTFF Chapter 2 ("Movie Atoms") and Chapter 3 ("Media Data Atom
/// Types") are the canonical layout reference; field offsets match
/// figures 2-3 (mvhd v0), 2-7 (tkhd v0), 2-16 (mdhd v0), 2-17
/// (hdlr), 2-19 (vmhd), 2-27 (stsd) and the sample-table figures
/// 2-28 / 2-33 / 2-36 / 2-38.
fn build_minimal_qt() -> (Vec<u8>, &'static [u8]) {
    const PAYLOAD: &[u8] = b"PAYLOAD!";
    let mut out = Vec::new();

    // --- ftyp ---
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // ftyp size = 8 + 12 = 20 bytes
    // mdat header @ offset 20, payload @ offset 28
    push_atom(&mut out, *b"mdat", PAYLOAD);
    let mdat_payload_offset: u32 = 28;

    // --- moov ---
    let mut moov = Vec::new();

    let mut mvhd = vec![0u8; 100];
    mvhd[12..16].copy_from_slice(&600u32.to_be_bytes()); // time_scale
    mvhd[16..20].copy_from_slice(&30u32.to_be_bytes()); // duration
    mvhd[20..24].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    mvhd[24..26].copy_from_slice(&0x0100i16.to_be_bytes()); // volume 1.0
    mvhd[96..100].copy_from_slice(&2u32.to_be_bytes()); // next_track_id
    push_atom(&mut moov, *b"mvhd", &mvhd);

    let mut trak = Vec::new();

    let mut tkhd = vec![0u8; 84];
    tkhd[3] = 0x07; // flags = enabled+in-movie+in-preview
    tkhd[12..16].copy_from_slice(&1u32.to_be_bytes()); // track_id
    tkhd[20..24].copy_from_slice(&30u32.to_be_bytes()); // duration
    tkhd[76..80].copy_from_slice(&((320u32) << 16).to_be_bytes()); // width
    tkhd[80..84].copy_from_slice(&((240u32) << 16).to_be_bytes()); // height
    push_atom(&mut trak, *b"tkhd", &tkhd);

    let mut mdia = Vec::new();

    let mut mdhd = vec![0u8; 24];
    mdhd[12..16].copy_from_slice(&600u32.to_be_bytes());
    mdhd[16..20].copy_from_slice(&30u32.to_be_bytes());
    push_atom(&mut mdia, *b"mdhd", &mdhd);

    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&0u32.to_be_bytes());
    hdlr.extend_from_slice(b"mhlr");
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.push(0); // counted name length 0
    push_atom(&mut mdia, *b"hdlr", &hdlr);

    let mut minf = Vec::new();

    let mut vmhd = vec![0u8; 12];
    vmhd[3] = 0x01; // flags = no-lean-ahead
    push_atom(&mut minf, *b"vmhd", &vmhd);

    let mut stbl = Vec::new();

    // stsd: 1 entry, 'rle ' (Apple Animation), 320×240
    let mut stsd = Vec::new();
    stsd.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    stsd.extend_from_slice(&1u32.to_be_bytes()); // entry count
    let entry_size: u32 = 86;
    stsd.extend_from_slice(&entry_size.to_be_bytes());
    stsd.extend_from_slice(b"rle ");
    stsd.extend_from_slice(&[0u8; 6]);
    stsd.extend_from_slice(&1u16.to_be_bytes()); // dref index
    let mut vbody = vec![0u8; 70];
    vbody[24..26].copy_from_slice(&320u16.to_be_bytes());
    vbody[26..28].copy_from_slice(&240u16.to_be_bytes());
    stsd.extend_from_slice(&vbody);
    push_atom(&mut stbl, *b"stsd", &stsd);

    // stts: 1 entry (count=1, duration=30 ticks @ 600/s = 50 ms)
    let mut stts = Vec::new();
    stts.extend_from_slice(&0u32.to_be_bytes());
    stts.extend_from_slice(&1u32.to_be_bytes());
    stts.extend_from_slice(&1u32.to_be_bytes());
    stts.extend_from_slice(&30u32.to_be_bytes());
    push_atom(&mut stbl, *b"stts", &stts);

    // stsc
    let mut stsc = Vec::new();
    stsc.extend_from_slice(&0u32.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes());
    push_atom(&mut stbl, *b"stsc", &stsc);

    // stsz: constant 8 bytes per sample, 1 sample
    let mut stsz = Vec::new();
    stsz.extend_from_slice(&0u32.to_be_bytes());
    stsz.extend_from_slice(&8u32.to_be_bytes());
    stsz.extend_from_slice(&1u32.to_be_bytes());
    push_atom(&mut stbl, *b"stsz", &stsz);

    // stco: 1 chunk pointing at the mdat payload
    let mut stco = Vec::new();
    stco.extend_from_slice(&0u32.to_be_bytes());
    stco.extend_from_slice(&1u32.to_be_bytes());
    stco.extend_from_slice(&mdat_payload_offset.to_be_bytes());
    push_atom(&mut stbl, *b"stco", &stco);

    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);

    (out, PAYLOAD)
}

#[test]
fn synth_minimal_qt_round_trip() {
    let (bytes, payload) = build_minimal_qt();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open minimal qt");

    // ftyp brand
    let ftyp = d.ftyp.as_ref().expect("ftyp");
    assert!(ftyp.is_quicktime(), "qt  brand should be recognised");
    assert_eq!(ftyp.major_brand, *b"qt  ");

    // mvhd
    let mvhd = d.mvhd.as_ref().expect("mvhd");
    assert_eq!(mvhd.time_scale, 600);
    assert_eq!(mvhd.duration, 30);

    // tracks
    assert_eq!(d.tracks.len(), 1);
    let tr = &d.tracks[0];
    assert!(tr.is_video());
    assert_eq!(tr.tkhd.track_id, 1);
    assert_eq!(tr.tkhd.width(), 320);
    assert_eq!(tr.tkhd.height(), 240);
    assert_eq!(tr.mdhd.time_scale, 600);
    assert_eq!(tr.primary_format(), Some(*b"rle "));

    // streams() and the first packet
    assert_eq!(d.streams().len(), 1);
    let stream = &d.streams()[0];
    assert_eq!(stream.index, 0);
    let pkt = d.next_packet().expect("first packet");
    assert_eq!(pkt.stream_index, 0);
    assert_eq!(pkt.data, payload.to_vec());
    assert_eq!(pkt.dts, Some(0));
    assert_eq!(pkt.pts, Some(0));
    assert_eq!(pkt.duration, Some(30));
    assert!(pkt.flags.keyframe, "implicit keyframe when stss is absent");

    // Past-the-end yields Eof.
    match d.next_packet() {
        Err(oxideav_core::Error::Eof) => {}
        other => panic!("expected Eof after the single packet, got {other:?}"),
    }
}
