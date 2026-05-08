//! Round-2 acceptance: visual sample-description extensions.
//!
//! Builds a 1-track, 1-sample QTFF file whose `stsd` video entry
//! carries the four common Apple/ISO display-hint atoms (`gama`,
//! `pasp`, `clap`, `colr`) and asserts each is round-tripped onto the
//! `SampleDescription` typed fields.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{ColorParametersKind, MovDemuxer};

/// Build a `stsd` extras blob carrying gama + pasp + clap + colr(nclc).
fn build_video_extensions() -> Vec<u8> {
    let mut out = Vec::new();
    // gama — 16.16 fixed-point gamma 2.2 (= 0x0002_3333)
    push_atom(&mut out, *b"gama", &0x0002_3333u32.to_be_bytes());

    // pasp — 16:9 anamorphic
    let mut pasp = Vec::new();
    pasp.extend_from_slice(&16u32.to_be_bytes());
    pasp.extend_from_slice(&9u32.to_be_bytes());
    push_atom(&mut out, *b"pasp", &pasp);

    // clap — 704×480 / 1, no offsets.
    let mut clap = Vec::new();
    for n in [704u32, 1, 480, 1] {
        clap.extend_from_slice(&n.to_be_bytes());
    }
    clap.extend_from_slice(&0i32.to_be_bytes());
    clap.extend_from_slice(&1u32.to_be_bytes());
    clap.extend_from_slice(&0i32.to_be_bytes());
    clap.extend_from_slice(&1u32.to_be_bytes());
    push_atom(&mut out, *b"clap", &clap);

    // colr — Apple nclc variant, BT.709 / sRGB / BT.709 matrix.
    let mut colr = Vec::new();
    colr.extend_from_slice(b"nclc");
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes());
    push_atom(&mut out, *b"colr", &colr);

    out
}

fn build_qt_with_extensions() -> Vec<u8> {
    let mut out = Vec::new();

    // ftyp
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // mdat (8 bytes payload)
    let payload = b"PAYLOAD!";
    push_atom(&mut out, *b"mdat", payload);
    let mdat_payload_offset: u32 = 28; // 20 (ftyp) + 8 (mdat header)

    // moov
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
    let extras = build_video_extensions();
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
fn video_sample_description_extensions_round_trip() {
    let bytes = build_qt_with_extensions();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open with stsd extensions");
    assert_eq!(d.tracks.len(), 1);
    let desc = &d.tracks[0].sample_descriptions[0];
    assert_eq!(desc.gamma, Some(0x0002_3333));
    let pasp = desc.pasp.expect("pasp parsed");
    assert_eq!((pasp.h_spacing, pasp.v_spacing), (16, 9));
    let clap = desc.clap.expect("clap parsed");
    assert_eq!(clap.clean_aperture_width_n, 704);
    assert_eq!(clap.clean_aperture_height_n, 480);
    let colr = desc.colr.as_ref().expect("colr parsed");
    match &colr.kind {
        ColorParametersKind::Nclc {
            primaries,
            transfer,
            matrix,
        } => assert_eq!((*primaries, *transfer, *matrix), (1, 1, 1)),
        other => panic!("expected nclc kind, got {other:?}"),
    }
}
