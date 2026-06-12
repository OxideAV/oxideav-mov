//! Round 283 acceptance: compressed movie resources (`cmov`) at the
//! demuxer surface.
//!
//! QTFF (2001-03-01) pp. 80 – 81 ("Compressed Movie Resources",
//! Table 2-5): beginning with QuickTime 3 a writer may losslessly
//! compress the movie resource; the file's top-level `moov` then
//! carries a single `cmov` child wrapping a `dcom` (compression
//! algorithm FourCC) and a `cmvd` (32-bit uncompressed size +
//! compressed payload). Per QTFF p. 30, decompressing the `cmov`
//! yields the standard uncompressed structure — a complete `moov`
//! atom — which the demuxer re-parses transparently.
//!
//! Builds the same 1-track, 1-sample QTFF file twice — once with a
//! plain `moov`, once with the `moov` zlib-compressed into a `cmov`
//! — and asserts both open to identical track/packet state.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{compress_movie_resource, Cmov, Cmvd, Dcom, MovDemuxer, DCOM_ALG_ZLIB};

/// Build the uncompressed movie resource — a complete `moov` atom
/// (header included, per QTFF p. 30 the decompressed contents are the
/// standard structure whose outermost atom is `moov`) for a 1-track
/// video movie whose single 8-byte sample lives at file offset 28
/// (ftyp 20 bytes + mdat header 8 bytes).
fn build_moov_atom(mdat_payload_offset: u32) -> Vec<u8> {
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
        &build_stsd_video(b"rle ", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_offset));

    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);

    let mut out = Vec::new();
    push_atom(&mut out, *b"moov", &moov);
    out
}

/// Wrap `ftyp` + `mdat` around an arbitrary top-level `moov` atom
/// (already including its own header).
fn build_file_with_moov_atom(moov_atom: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
    out.extend_from_slice(moov_atom);
    out
}

/// Compress a movie resource into the on-disk compressed layout:
/// `moov` > `cmov` > (`dcom` + `cmvd`) per QTFF Table 2-5.
fn wrap_compressed(cmov: &Cmov) -> Vec<u8> {
    let mut moov_body = Vec::new();
    push_atom(&mut moov_body, *b"cmov", &cmov.to_body_bytes());
    let mut moov_atom = Vec::new();
    push_atom(&mut moov_atom, *b"moov", &moov_body);
    moov_atom
}

const MDAT_PAYLOAD_OFFSET: u32 = 28;

#[test]
fn compressed_movie_round_trips_through_demuxer() {
    // The compressed layout must open to the same track and packet
    // state as the uncompressed layout — chunk offsets in the
    // decompressed moov address the same file's mdat.
    let moov_atom = build_moov_atom(MDAT_PAYLOAD_OFFSET);
    let cmov = compress_movie_resource(&moov_atom).expect("compress movie resource");
    let bytes = build_file_with_moov_atom(&wrap_compressed(&cmov));

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open compressed movie");
    assert_eq!(d.compressed_movie_algorithm, Some(DCOM_ALG_ZLIB));
    assert_eq!(d.tracks.len(), 1);
    assert!(d.tracks[0].is_video());
    assert_eq!(d.tracks[0].primary_format(), Some(*b"rle "));
    let pkt = d.next_packet().expect("one packet");
    assert_eq!(pkt.data, b"PAYLOAD!".to_vec());
    assert_eq!(pkt.dts, Some(0));
}

#[test]
fn compressed_and_uncompressed_layouts_agree() {
    let moov_atom = build_moov_atom(MDAT_PAYLOAD_OFFSET);

    let plain = build_file_with_moov_atom(&moov_atom);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(plain));
    let mut d_plain = MovDemuxer::open(cur).expect("open plain movie");

    let cmov = compress_movie_resource(&moov_atom).unwrap();
    let packed = build_file_with_moov_atom(&wrap_compressed(&cmov));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(packed));
    let mut d_packed = MovDemuxer::open(cur).expect("open compressed movie");

    assert_eq!(d_plain.compressed_movie_algorithm, None);
    assert_eq!(d_packed.compressed_movie_algorithm, Some(*b"zlib"));
    assert_eq!(
        d_plain.mvhd.as_ref().unwrap().time_scale,
        d_packed.mvhd.as_ref().unwrap().time_scale
    );
    assert_eq!(d_plain.tracks.len(), d_packed.tracks.len());
    let p1 = d_plain.next_packet().unwrap();
    let p2 = d_packed.next_packet().unwrap();
    assert_eq!(p1.data, p2.data);
    assert_eq!(p1.dts, p2.dts);
}

#[test]
fn non_zlib_dcom_algorithm_rejects() {
    // QTFF p. 81 names the dcom field generically; an algorithm the
    // decompressor does not implement must fail the open with an
    // error, not misread the payload as zlib.
    let moov_atom = build_moov_atom(MDAT_PAYLOAD_OFFSET);
    let mut cmov = compress_movie_resource(&moov_atom).unwrap();
    cmov.dcom.algorithm = *b"none";
    let bytes = build_file_with_moov_atom(&wrap_compressed(&cmov));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    assert!(MovDemuxer::open(cur).is_err());
}

#[test]
fn declared_size_mismatch_rejects() {
    // QTFF p. 81 makes the cmvd size word authoritative; a stream
    // that inflates to a different length is a writer error.
    let moov_atom = build_moov_atom(MDAT_PAYLOAD_OFFSET);
    let mut cmov = compress_movie_resource(&moov_atom).unwrap();
    cmov.cmvd.uncompressed_size += 1;
    let bytes = build_file_with_moov_atom(&wrap_compressed(&cmov));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    assert!(MovDemuxer::open(cur).is_err());
}

#[test]
fn nested_cmov_rejects() {
    // QTFF p. 30: the decompressed contents follow the standard
    // *uncompressed* structure — a second compression layer is
    // non-conformant and must not recurse.
    let inner_cmov = compress_movie_resource(b"whatever").unwrap();
    let inner_moov_atom = wrap_compressed(&inner_cmov);
    // Compress a movie resource that is itself a moov-wrapping-cmov.
    let outer_cmov = compress_movie_resource(&inner_moov_atom).unwrap();
    let bytes = build_file_with_moov_atom(&wrap_compressed(&outer_cmov));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    assert!(MovDemuxer::open(cur).is_err());
}

#[test]
fn decompressed_non_moov_resource_rejects() {
    // The decompressed bytes must be a complete movie resource whose
    // outermost atom is `moov` (QTFF p. 30); anything else rejects.
    let mut not_a_moov = Vec::new();
    push_atom(&mut not_a_moov, *b"free", &[0u8; 16]);
    let cmov = compress_movie_resource(&not_a_moov).unwrap();
    let bytes = build_file_with_moov_atom(&wrap_compressed(&cmov));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    assert!(MovDemuxer::open(cur).is_err());
}

#[test]
fn oversize_uncompressed_declaration_rejects_cheaply() {
    // A tiny file declaring a u32::MAX-byte movie resource must be
    // rejected on the declared size alone (the crate-wide 64 MiB
    // in-memory cap), before any decompression allocation.
    let cmov = Cmov {
        dcom: Dcom {
            algorithm: DCOM_ALG_ZLIB,
        },
        cmvd: Cmvd {
            uncompressed_size: u32::MAX,
            compressed_data: vec![0x78, 0x9C, 0x03, 0x00], // tiny stub
        },
    };
    let bytes = build_file_with_moov_atom(&wrap_compressed(&cmov));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    assert!(MovDemuxer::open(cur).is_err());
}

#[test]
fn truncated_compressed_stream_rejects() {
    // Chopping the tail off the zlib stream must surface as a clean
    // parse error, not a panic or a partial moov.
    let moov_atom = build_moov_atom(MDAT_PAYLOAD_OFFSET);
    let mut cmov = compress_movie_resource(&moov_atom).unwrap();
    let keep = cmov.cmvd.compressed_data.len() / 2;
    cmov.cmvd.compressed_data.truncate(keep);
    let bytes = build_file_with_moov_atom(&wrap_compressed(&cmov));
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    assert!(MovDemuxer::open(cur).is_err());
}
