//! Round 147 — Sample Auxiliary Information `saiz` / `saio` decode.
//!
//! Exercises the surface defined in `sample_aux` against a hand-built
//! QT file whose `stbl` carries both boxes (ISO/IEC 14496-12 §8.7.8,
//! §8.7.9). The boxes pair: `saiz` records the per-sample byte count
//! of the auxiliary information stream, and `saio` records the file
//! offsets to those bytes (one per chunk / run, or one for the whole
//! `stbl`). Both boxes share the discriminator pair `(aux_info_type,
//! aux_info_type_parameter)` gated by `flags & 1`.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a `saiz` body. `aux` is the optional `(aux_info_type,
/// aux_info_type_parameter)` discriminator pair; including it sets
/// `flags & 1` automatically.
fn build_saiz(
    aux: Option<(&[u8; 4], u32)>,
    default_size: u8,
    sample_count: u32,
    per_sample_sizes: &[u8],
) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0); // version
    let flags: u32 = if aux.is_some() { 1 } else { 0 };
    let f = flags.to_be_bytes();
    p.extend_from_slice(&f[1..4]);
    if let Some((t, par)) = aux {
        p.extend_from_slice(t);
        p.extend_from_slice(&par.to_be_bytes());
    }
    p.push(default_size);
    p.extend_from_slice(&sample_count.to_be_bytes());
    if default_size == 0 {
        p.extend_from_slice(per_sample_sizes);
    }
    p
}

/// Build a `saio` body.
fn build_saio(version: u8, aux: Option<(&[u8; 4], u32)>, offsets: &[u64]) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(version);
    let flags: u32 = if aux.is_some() { 1 } else { 0 };
    let f = flags.to_be_bytes();
    p.extend_from_slice(&f[1..4]);
    if let Some((t, par)) = aux {
        p.extend_from_slice(t);
        p.extend_from_slice(&par.to_be_bytes());
    }
    p.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
    for &o in offsets {
        if version == 0 {
            p.extend_from_slice(&(o as u32).to_be_bytes());
        } else {
            p.extend_from_slice(&o.to_be_bytes());
        }
    }
    p
}

/// Build a 4-sample video QT file whose `stbl` carries the supplied
/// `(fourcc, body)` extra boxes after the required stbl entries.
fn build_video_qt_with_extras(extras: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
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
        for (fcc, body) in extras {
            push_atom(&mut stbl, *fcc, body);
        }
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
fn saiz_default_size_decodes_through_stbl() {
    let saiz_body = build_saiz(Some((b"cenc", 0)), 16, 4, &[]);
    let saio_body = build_saio(0, Some((b"cenc", 0)), &[0x1000]);
    let extras = vec![(*b"saiz", saiz_body), (*b"saio", saio_body)];
    let file = build_video_qt_with_extras(&extras);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open saiz/saio fixture");

    let (saiz, saio) = d.sample_aux_info(0, b"cenc", 0);
    let saiz = saiz.expect("saiz present for cenc");
    let saio = saio.expect("saio present for cenc");
    assert_eq!(saiz.default_sample_info_size, 16);
    assert_eq!(saiz.sample_count, 4);
    assert_eq!(saiz.size_for(0), Some(16));
    assert_eq!(saiz.size_for(3), Some(16));
    assert_eq!(saiz.size_for(4), None);
    assert_eq!(saiz.total_size(), 64);
    assert!(saio.is_single_chunk());
    assert_eq!(saio.offset_for(0), Some(0x1000));
}

#[test]
fn saiz_per_sample_sizes_decode_through_stbl() {
    let saiz_body = build_saiz(Some((b"cenc", 0)), 0, 4, &[16, 24, 16, 24]);
    let saio_body = build_saio(0, Some((b"cenc", 0)), &[0x2000]);
    let extras = vec![(*b"saiz", saiz_body), (*b"saio", saio_body)];
    let file = build_video_qt_with_extras(&extras);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open saiz/saio fixture");

    let (saiz, _) = d.sample_aux_info(0, b"cenc", 0);
    let saiz = saiz.expect("saiz present");
    assert_eq!(saiz.default_sample_info_size, 0);
    assert_eq!(saiz.sample_info_sizes, vec![16, 24, 16, 24]);
    assert_eq!(saiz.size_for(2), Some(16));
    assert_eq!(saiz.total_size(), 80);
}

#[test]
fn saio_v1_64bit_offsets_decode_through_stbl() {
    // 64-bit offset only representable under v1.
    let big = 0x1_0000_2000u64;
    let saiz_body = build_saiz(Some((b"cenc", 0)), 16, 4, &[]);
    let saio_body = build_saio(1, Some((b"cenc", 0)), &[big]);
    let extras = vec![(*b"saiz", saiz_body), (*b"saio", saio_body)];
    let file = build_video_qt_with_extras(&extras);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open v1 saio fixture");
    let (_, saio) = d.sample_aux_info(0, b"cenc", 0);
    let saio = saio.expect("saio present");
    assert_eq!(saio.version, 1);
    assert_eq!(saio.offset_for(0), Some(big));
}

#[test]
fn saiz_saio_without_aux_info_type_match_zero_zero() {
    // §8.7.8.1: when `flags & 1` is unset, the discriminator is
    // implicit. We surface the box and let callers match against
    // (b"\0\0\0\0", 0) — the helper does this for "the only box
    // without a discriminator".
    let saiz_body = build_saiz(None, 16, 4, &[]);
    let saio_body = build_saio(0, None, &[0x100]);
    let extras = vec![(*b"saiz", saiz_body), (*b"saio", saio_body)];
    let file = build_video_qt_with_extras(&extras);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open implicit-discriminator fixture");

    let (saiz, saio) = d.sample_aux_info(0, &[0u8; 4], 0);
    assert!(saiz.is_some());
    assert!(saio.is_some());

    // A discriminator that doesn't match (b"\0\0\0\0", 0) returns
    // (None, None) — the implicit-discriminator box only matches the
    // zero-zero pair via this accessor.
    let (saiz_miss, saio_miss) = d.sample_aux_info(0, b"cenc", 0);
    assert!(saiz_miss.is_none());
    assert!(saio_miss.is_none());
}

#[test]
fn multiple_saiz_saio_pairs_distinguished_by_aux_info_type() {
    // Two pairs: one for `cenc` (CTR-mode encryption sample aux),
    // one for an arbitrary other aux_info_type (`stpp`). §8.7.8.3
    // permits this: "At most one occurrence of this box with the
    // same values for aux_info_type and aux_info_type_parameter".
    let saiz_cenc = build_saiz(Some((b"cenc", 0)), 16, 4, &[]);
    let saio_cenc = build_saio(0, Some((b"cenc", 0)), &[0x1000]);
    let saiz_stpp = build_saiz(Some((b"stpp", 0)), 8, 4, &[]);
    let saio_stpp = build_saio(0, Some((b"stpp", 0)), &[0x2000]);
    let extras = vec![
        (*b"saiz", saiz_cenc),
        (*b"saio", saio_cenc),
        (*b"saiz", saiz_stpp),
        (*b"saio", saio_stpp),
    ];
    let file = build_video_qt_with_extras(&extras);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open multi-pair fixture");

    let (cenc_saiz, cenc_saio) = d.sample_aux_info(0, b"cenc", 0);
    assert_eq!(cenc_saiz.unwrap().default_sample_info_size, 16);
    assert_eq!(cenc_saio.unwrap().offset_for(0), Some(0x1000));

    let (stpp_saiz, stpp_saio) = d.sample_aux_info(0, b"stpp", 0);
    assert_eq!(stpp_saiz.unwrap().default_sample_info_size, 8);
    assert_eq!(stpp_saio.unwrap().offset_for(0), Some(0x2000));

    // An unknown discriminator returns (None, None).
    let (none_saiz, none_saio) = d.sample_aux_info(0, b"xxxx", 0);
    assert!(none_saiz.is_none());
    assert!(none_saio.is_none());
}

#[test]
fn duplicate_saiz_for_same_discriminator_first_wins() {
    // §8.7.8.3 forbids duplicates; the demuxer keeps the first and
    // silently drops the second (matches the sbgp/sgpd convention).
    let saiz_a = build_saiz(Some((b"cenc", 0)), 16, 4, &[]);
    let saiz_b = build_saiz(Some((b"cenc", 0)), 32, 4, &[]);
    let saio_body = build_saio(0, Some((b"cenc", 0)), &[0x1000]);
    let extras = vec![
        (*b"saiz", saiz_a),
        (*b"saiz", saiz_b),
        (*b"saio", saio_body),
    ];
    let file = build_video_qt_with_extras(&extras);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open duplicate-saiz fixture");

    let (saiz, _) = d.sample_aux_info(0, b"cenc", 0);
    // First wins: default size remains 16 (not 32).
    assert_eq!(saiz.unwrap().default_sample_info_size, 16);
}

#[test]
fn malformed_saiz_rejected_at_open() {
    // version != 0 → reject at open time.
    let mut bad = build_saiz(None, 16, 4, &[]);
    bad[0] = 1;
    let extras = vec![(*b"saiz", bad)];
    let file = build_video_qt_with_extras(&extras);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    assert!(MovDemuxer::open(cur).is_err());
}

#[test]
fn no_saiz_or_saio_yields_none_lookups() {
    let file = build_video_qt_with_extras(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open no-saiz/saio fixture");
    let (saiz, saio) = d.sample_aux_info(0, b"cenc", 0);
    assert!(saiz.is_none());
    assert!(saio.is_none());
    // Out-of-range track yields (None, None) too.
    let (saiz_oob, saio_oob) = d.sample_aux_info(99, b"cenc", 0);
    assert!(saiz_oob.is_none());
    assert!(saio_oob.is_none());
}
