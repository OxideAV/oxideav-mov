//! Round-2 acceptance: edit lists, composition timing, faststart probe.
//!
//! Builds a 1-track QTFF file whose track carries an `edts/elst` with
//! an empty edit followed by a real edit, plus a `ctts` table with
//! per-sample composition offsets. Verifies the demuxer parses the
//! edit list and emits packets whose `pts` reflects DTS + composition
//! offset, and that the `is_faststart()` probe correctly reports the
//! moov-before-mdat layout.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::MovDemuxer;

fn build_elst_v0(entries: &[(u32, i32, i32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (dur, mt, rate) in entries {
        p.extend_from_slice(&dur.to_be_bytes());
        p.extend_from_slice(&mt.to_be_bytes());
        p.extend_from_slice(&rate.to_be_bytes());
    }
    p
}

fn build_ctts_v0(runs: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, off) in runs {
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&off.to_be_bytes());
    }
    p
}

/// Build a 4-sample QT file with edit list + ctts. moov-first layout
/// (faststart): ftyp → moov → mdat.
///
/// Strategy: build moov with placeholder chunk offset = 0, then once
/// we know the final file size and mdat position, patch the stco
/// entry's u32 chunk_offset in place.
fn build_qt_with_edits_ctts() -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";
    let elst = build_elst_v0(&[(50, -1, 0x0001_0000), (70, 0, 0x0001_0000)]);

    // Build moov body with placeholder chunk offset = 0 first.
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
        let mut edts = Vec::new();
        push_atom(&mut edts, *b"elst", &elst);
        push_atom(&mut trak, *b"edts", &edts);
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
        push_atom(&mut stbl, *b"ctts", &build_ctts_v0(&[(3, 10), (1, 0)]));
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

    // First pass: build with placeholder. Total file size is the same
    // regardless of chunk_offset value (a u32 is a u32), so the mdat
    // payload offset is identical in pass 1 and pass 2.
    let pass1 = build_file(0);
    // Find the `mdat` FourCC literal. The atom header is
    // `[size:4][type:4]` so the FourCC sits 4 bytes into the atom;
    // the payload begins another 4 bytes later. Payload offset thus
    // equals (FourCC byte position) + 4.
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

#[test]
fn elst_ctts_and_faststart_round_trip() {
    let bytes = build_qt_with_edits_ctts();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open with elst+ctts");
    assert_eq!(d.tracks.len(), 1);

    // Edit list: 2 entries — first is empty, second is real.
    let edits = &d.tracks[0].edits;
    assert_eq!(edits.len(), 2);
    assert!(edits[0].is_empty());
    assert_eq!(edits[0].track_duration, 50);
    assert_eq!(edits[1].track_duration, 70);
    assert_eq!(edits[1].media_time, 0);
    assert_eq!(edits[1].media_rate, 0x0001_0000);

    // ctts surfaces composition offsets on the first three samples.
    let st = &d.tracks[0].sample_table;
    assert_eq!(st.ctts.len(), 2);
    assert_eq!(st.ctts[0].sample_count, 3);
    assert_eq!(st.ctts[0].composition_offset, 10);
    assert_eq!(st.ctts[1].composition_offset, 0);

    // Faststart: moov precedes mdat in this layout.
    assert!(d.is_faststart(), "moov-first layout should be faststart");

    // Iterate packets and confirm DTS / PTS divergence.
    let pkt0 = d.next_packet().expect("packet 0");
    assert_eq!(pkt0.dts, Some(0));
    assert_eq!(pkt0.pts, Some(10));
    let pkt1 = d.next_packet().expect("packet 1");
    assert_eq!(pkt1.dts, Some(30));
    assert_eq!(pkt1.pts, Some(40));
    let pkt2 = d.next_packet().expect("packet 2");
    assert_eq!(pkt2.dts, Some(60));
    assert_eq!(pkt2.pts, Some(70));
    let pkt3 = d.next_packet().expect("packet 3");
    assert_eq!(pkt3.dts, Some(90));
    assert_eq!(pkt3.pts, Some(90));
}

/// Build the same fixture but emit `mdat` BEFORE `moov` to produce a
/// non-faststart layout, and confirm `is_faststart()` reports false.
fn build_mdat_first_qt() -> Vec<u8> {
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
        &build_stsd_video(b"avc1", 320, 240, &[]),
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
fn mdat_before_moov_is_not_faststart() {
    let bytes = build_mdat_first_qt();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open mdat-first qt");
    assert!(
        !d.is_faststart(),
        "mdat-before-moov should not be faststart"
    );
}
