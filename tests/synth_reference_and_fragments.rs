//! Round-3 / Round-18 acceptance: reference-movie (`rmra`/`rmda`)
//! recognition + clean rejection, and fragmented MP4 (`mvex`/`moof`)
//! **acceptance** (was rejection through round 17; round 18 lands a
//! real decode path for the ISO/IEC 14496-12 §8.8 fragmented atoms).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{DataReference, MovDemuxer};

/// Build a reference-movie file: `ftyp` + `moov` containing only `rmra`
/// (no track), pointing at a single URL alternate. Such a file has no
/// in-file media; the demuxer must reject with an `Unsupported` error
/// rather than treat it as malformed.
fn build_reference_movie() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));

    // rmra > rmda > rdrf 'url '
    let url = b"http://example.com/source.mov\0";
    let mut rdrf = Vec::new();
    rdrf.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    rdrf.extend_from_slice(b"url ");
    rdrf.extend_from_slice(&(url.len() as u32).to_be_bytes());
    rdrf.extend_from_slice(url);
    let mut rmda = Vec::new();
    push_atom(&mut rmda, *b"rdrf", &rdrf);

    // Add an rmdr (data-rate 256000 bps) and rmqu (quality 0x80) so we
    // exercise the qualifier parsers.
    let mut rmdr = Vec::new();
    rmdr.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    rmdr.extend_from_slice(&256_000u32.to_be_bytes());
    push_atom(&mut rmda, *b"rmdr", &rmdr);

    let mut rmqu = Vec::new();
    rmqu.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    rmqu.extend_from_slice(&0x80u32.to_be_bytes());
    push_atom(&mut rmda, *b"rmqu", &rmqu);

    let mut rmra = Vec::new();
    push_atom(&mut rmra, *b"rmda", &rmda);

    push_atom(&mut moov, *b"rmra", &rmra);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn reference_movie_rejects_with_unsupported_error() {
    let bytes = build_reference_movie();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let err = match MovDemuxer::open(cur) {
        Ok(_) => panic!("reference-movie must reject"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("reference-movie") || msg.contains("alias"),
        "expected reference-movie hint, got: {msg}"
    );
}

/// Build an `rmra`/`rmda` movie that *does* have an mdat + a valid
/// track alongside the alias list. The demuxer should keep parsing
/// the in-file media and surface the reference list informationally.
fn build_qt_with_inline_track_and_rmra() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
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

    // Add an rmra after the track. Players use it only as fallback.
    let url = b"http://example.com/alt.mov\0";
    let mut rdrf = Vec::new();
    rdrf.extend_from_slice(&0u32.to_be_bytes());
    rdrf.extend_from_slice(b"url ");
    rdrf.extend_from_slice(&(url.len() as u32).to_be_bytes());
    rdrf.extend_from_slice(url);
    let mut rmda = Vec::new();
    push_atom(&mut rmda, *b"rdrf", &rdrf);
    let mut rmra = Vec::new();
    push_atom(&mut rmra, *b"rmda", &rmda);
    push_atom(&mut moov, *b"rmra", &rmra);

    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn rmra_alongside_inline_track_is_surfaced_informationally() {
    let bytes = build_qt_with_inline_track_and_rmra();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open inline-track + rmra");
    assert_eq!(d.tracks.len(), 1);
    assert_eq!(d.reference_movies.len(), 1);
    let rmda = &d.reference_movies[0];
    match rmda.data_ref.as_ref().expect("data_ref") {
        DataReference::Url(s) => assert_eq!(s, "http://example.com/alt.mov"),
        _ => panic!("expected URL data reference"),
    }
}

/// Build a fragmented MP4 — `moov` carries a `mvex` child. The demuxer
/// must reject with a hint pointing at oxideav-mp4.
fn build_qt_with_mvex() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
    let mdat_payload_offset: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    // mvex with a single trex
    let mut trex = Vec::new();
    trex.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    trex.extend_from_slice(&1u32.to_be_bytes()); // track_id
    trex.extend_from_slice(&1u32.to_be_bytes()); // default_sample_description_index
    trex.extend_from_slice(&0u32.to_be_bytes()); // default_sample_duration
    trex.extend_from_slice(&0u32.to_be_bytes()); // default_sample_size
    trex.extend_from_slice(&0u32.to_be_bytes()); // default_sample_flags
    let mut mvex = Vec::new();
    push_atom(&mut mvex, *b"trex", &trex);
    push_atom(&mut moov, *b"mvex", &mvex);

    // Still need a normal trak so we don't trip the "no tracks" path
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
fn mvex_inside_moov_surfaces_trex_defaults() {
    // Round 18: `mvex/trex` no longer rejects. The demuxer parses
    // the per-track defaults so a downstream `moof` walk can pull
    // its per-fragment values from the cascade. With no `moof`,
    // the file has exactly the one in-moov sample from the
    // declared stbl.
    let bytes = build_qt_with_mvex();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("mvex-with-no-moof opens fine in round 18");
    assert!(d.is_fragmented(), "mvex/trex declares the file fragmented");
    assert_eq!(d.trex_defaults.len(), 1);
    let trex = &d.trex_defaults[0];
    assert_eq!(trex.track_id, 1);
    assert_eq!(trex.default_sample_description_index, 1);
    // No moof was emitted, so no fragment_sequence_numbers either.
    assert!(d.fragment_sequence_numbers.is_empty());
}

/// Build a top-level `moof` with `mfhd` only — no `traf`. The
/// demuxer accepts it: the mfhd sequence number is recorded but no
/// new samples are appended.
fn build_qt_with_empty_moof() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));
    // No fragmented tracks, but we still need at least one `trak` to
    // satisfy the "moov must declare its tracks" rule.
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
    // Empty stbl (0 samples) — a legal fragmented init-segment shape.
    let stts_empty = {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // 0 entries
        p
    };
    push_atom(&mut stbl, *b"stts", &stts_empty);
    let stsc_empty = {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // 0 entries
        p
    };
    push_atom(&mut stbl, *b"stsc", &stsc_empty);
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(0, 0));
    let stco_empty = {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p
    };
    push_atom(&mut stbl, *b"stco", &stco_empty);
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);

    // a moof at top level with only an `mfhd`
    let mut mfhd = Vec::new();
    mfhd.extend_from_slice(&0u32.to_be_bytes());
    mfhd.extend_from_slice(&1u32.to_be_bytes()); // sequence number
    let mut moof = Vec::new();
    push_atom(&mut moof, *b"mfhd", &mfhd);
    push_atom(&mut out, *b"moof", &moof);

    out
}

#[test]
fn top_level_moof_with_mfhd_only_accepted() {
    let bytes = build_qt_with_empty_moof();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("moof with empty traf list opens fine");
    assert!(d.is_fragmented());
    assert_eq!(d.fragment_sequence_numbers, vec![1]);
    // No traf → no fragment samples.
    assert_eq!(d.tracks[0].fragment_samples.len(), 0);
}
