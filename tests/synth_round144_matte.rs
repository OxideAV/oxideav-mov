//! Round 144 — Track Matte atom (`matt`) + Compressed Matte atom (`kmat`)
//! decode at track scope.
//!
//! Exercises the demuxer's per-track `matte` surface (QTFF p. 44 /
//! p. 45) against hand-built QuickTime files whose `moov/trak` carries
//! an optional Track Matte atom alongside the usual `tkhd` + `mdia`
//! children. The wrapper atom is QuickTime-only (ISO BMFF does not
//! define it); these tests verify the demuxer:
//!
//! * surfaces the parsed `kmat` (FullBox header + image description +
//!   compressed matte data) byte-for-byte on
//!   [`oxideav_mov::Track::matte`];
//! * reports `None` for a track that omits the atom;
//! * preserves an empty trailing matte-data tail when the writer
//!   emits only the image description;
//! * carves a longer image-description structure correctly using
//!   the QTFF p. 70 leading size word;
//! * follows the first-wins duplicate-merge policy when a malformed
//!   writer emits two `matt` atoms on the same track;
//! * rejects a malformed `kmat` body (unknown version) at open time
//!   rather than silently producing an absent surface.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, MIN_IMAGE_DESCRIPTION_SIZE};

/// Build a minimum-shape image description structure (QTFF p. 70):
/// 4-byte size + 4-byte data format + 6 reserved + 2-byte
/// data_reference_index. Optionally pads `extra_size` zero bytes to
/// simulate per-codec extensions (e.g. video sample description's
/// version/revision/vendor fields).
fn build_image_description(fourcc: &[u8; 4], extra_size: usize) -> Vec<u8> {
    let total = MIN_IMAGE_DESCRIPTION_SIZE as usize + extra_size;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend(std::iter::repeat(0u8).take(extra_size));
    out
}

/// Build a `kmat` body (post-atom-header) with the given image
/// description and trailing matte data per QTFF p. 45.
fn build_kmat_body(image_desc: &[u8], matte_data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + image_desc.len() + matte_data.len());
    out.push(0); // version
    out.extend_from_slice(&[0, 0, 0]); // flags
    out.extend_from_slice(image_desc);
    out.extend_from_slice(matte_data);
    out
}

/// Build a `matt` wrapper body carrying a single `kmat` child whose
/// body is `kmat_body` (the canonical Figure 2-9 shape).
fn build_matt_body(kmat_body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_atom(&mut out, *b"kmat", kmat_body);
    out
}

/// Build a minimal one-video-track QuickTime file. The caller may
/// inject any number of `matt` payloads inside the single `trak`
/// (sibling of `tkhd` and `mdia`). QTFF Figure 2-6 (p. 41) places a
/// matte atom inside individual tracks; there is no movie-level
/// matte (a movie's blending is the union of its tracks').
fn build_qt_with_track_mattes(track_mattes: &[Vec<u8>]) -> Vec<u8> {
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
        // Track-level matt atoms — siblings of tkhd and mdia (QTFF
        // p. 41 Figure 2-6). Emitted before mdia so the walker sees
        // them first; ordering inside the track atom is not fixed by
        // the spec.
        for payload in track_mattes {
            push_atom(&mut trak, *b"matt", payload);
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

        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

#[test]
fn track_level_minimal_matte_round_trips() {
    // Smallest spec-legal kmat: 16-byte image description + 4 bytes
    // of opaque matte data; raw-RGB image description naming the
    // standard QTFF 'raw ' format.
    let image_desc = build_image_description(b"raw ", 0);
    let matte_payload = [0xDE, 0xAD, 0xBE, 0xEF];
    let kmat = build_kmat_body(&image_desc, &matte_payload);
    let matt = build_matt_body(&kmat);
    let file = build_qt_with_track_mattes(&[matt]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open track-matte fixture");

    let track_matte = d.tracks[0]
        .matte
        .as_ref()
        .expect("track-level matte populated");
    assert_eq!(track_matte.compressed.version, 0);
    assert_eq!(track_matte.compressed.flags, 0);
    assert_eq!(track_matte.compressed.image_description, image_desc);
    assert_eq!(track_matte.compressed.matte_data, matte_payload);
    assert_eq!(track_matte.compressed.data_format(), Some(*b"raw "));
    assert_eq!(track_matte.compressed.image_description_size(), 16);
}

#[test]
fn extended_image_description_carved_using_leading_size_word() {
    // Image description padded out to 86 bytes — the canonical video
    // sample description on-disk length. The QTFF p. 70 leading size
    // word is the only thing the parser needs to find the matte data
    // boundary; per-codec extensions are surfaced verbatim.
    let image_desc = build_image_description(b"jpeg", 70);
    assert_eq!(image_desc.len(), 86);
    let matte_payload: Vec<u8> = (0..32u8).collect();
    let kmat = build_kmat_body(&image_desc, &matte_payload);
    let matt = build_matt_body(&kmat);
    let file = build_qt_with_track_mattes(&[matt]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open extended-matte fixture");

    let m = d.tracks[0].matte.as_ref().expect("matte populated");
    assert_eq!(m.compressed.image_description_size(), 86);
    assert_eq!(m.compressed.image_description, image_desc);
    assert_eq!(m.compressed.matte_data, matte_payload);
    assert_eq!(m.compressed.data_format(), Some(*b"jpeg"));
}

#[test]
fn matte_data_may_be_empty() {
    // QTFF p. 45: "The compressed matte data, which is of variable
    // length." A writer that emits the image description with no
    // trailing data is still spec-conformant.
    let image_desc = build_image_description(b"alis", 0);
    let kmat = build_kmat_body(&image_desc, &[]);
    let matt = build_matt_body(&kmat);
    let file = build_qt_with_track_mattes(&[matt]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open empty-matte-data fixture");

    let m = d.tracks[0].matte.as_ref().expect("matte populated");
    assert!(m.compressed.matte_data.is_empty());
    assert_eq!(m.compressed.image_description, image_desc);
}

#[test]
fn matte_absent_yields_none() {
    let file = build_qt_with_track_mattes(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open matte-less fixture");
    assert!(d.tracks[0].matte.is_none());
}

#[test]
fn duplicate_track_matte_keeps_first() {
    // First-wins on the rare duplicate case (shares the conservative
    // merge policy with clip / tapt / load / cslg).
    let id_a = build_image_description(b"AAAA", 0);
    let id_b = build_image_description(b"BBBB", 0);
    let kmat_a = build_kmat_body(&id_a, b"first");
    let kmat_b = build_kmat_body(&id_b, b"second");
    let matt_a = build_matt_body(&kmat_a);
    let matt_b = build_matt_body(&kmat_b);
    let file = build_qt_with_track_mattes(&[matt_a, matt_b]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open duplicate-track-matte fixture");
    let m = d.tracks[0].matte.as_ref().expect("first matte retained");
    assert_eq!(m.compressed.data_format(), Some(*b"AAAA"));
    assert_eq!(m.compressed.matte_data, b"first");
}

#[test]
fn malformed_kmat_rejected_at_open_time() {
    // version = 0xFF violates QTFF p. 45 (the spec fixes the version
    // field at 0). The open must fail rather than silently produce
    // an absent surface.
    let image_desc = build_image_description(b"raw ", 0);
    let mut bad_kmat = build_kmat_body(&image_desc, &[]);
    bad_kmat[0] = 0xFF; // bogus version
    let matt = build_matt_body(&bad_kmat);
    let file = build_qt_with_track_mattes(&[matt]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let err = match MovDemuxer::open(cur) {
        Ok(_) => panic!("malformed kmat should have been rejected at open"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("kmat unknown version"),
        "error names the offending field; got: {msg}"
    );
}
