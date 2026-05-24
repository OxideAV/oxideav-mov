//! Round 118 — Sub-Sample Information Box (`subs`) decode.
//!
//! Exercises the `subs` surface (ISO/IEC 14496-12 §8.7.7) against a
//! hand-built QT file whose `stbl` carries a `subs` box. The box is
//! *sparsely* coded: each row names a sample via a delta from the
//! previous row's sample number and lists that sample's sub-samples
//! (contiguous byte ranges). The test opens the file via `MovDemuxer`
//! and verifies `MovDemuxer::sub_samples`:
//!
//! * sample 1 — two sub-samples (e.g. a config NAL + a slice NAL);
//! * sample 3 — one sub-sample, the second marked `discardable = 1`
//!   semantics are exercised in the unit tests;
//! * samples not named by a row return `None`.
//!
//! It also verifies the two on-disk versions (v0 16-bit sizes / v1
//! 32-bit sizes) decode through the full stbl walk, and that a track
//! carrying two `subs` boxes for the same sample merges their
//! sub-sample lists (§8.7.7.1 permits more than one box per track,
//! distinguished by `flags`).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// One sub-sample record `(size, priority, discardable, csp)`.
type SubsRecord = (u32, u8, u8, u32);
/// One `subs` row `(sample_delta, &[record])`.
type SubsRow<'a> = (u32, &'a [SubsRecord]);

/// Build a `subs` box body from `(version, flags, rows)`.
fn build_subs(version: u8, flags: u32, rows: &[SubsRow<'_>]) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(version);
    let f = flags.to_be_bytes();
    p.extend_from_slice(&f[1..4]); // 3-byte flags
    p.extend_from_slice(&(rows.len() as u32).to_be_bytes());
    for (delta, subs) in rows {
        p.extend_from_slice(&delta.to_be_bytes());
        p.extend_from_slice(&(subs.len() as u16).to_be_bytes());
        for (size, prio, disc, csp) in *subs {
            if version == 1 {
                p.extend_from_slice(&size.to_be_bytes());
            } else {
                p.extend_from_slice(&(*size as u16).to_be_bytes());
            }
            p.push(*prio);
            p.push(*disc);
            p.extend_from_slice(&csp.to_be_bytes());
        }
    }
    p
}

/// Build a 4-sample video QT file whose `stbl` carries `subs_boxes`
/// (each a fully-built `subs` body). Two-pass so the `stco` chunk
/// offset points at the real `mdat` payload position.
fn build_video_qt_with_subs(subs_boxes: &[Vec<u8>]) -> Vec<u8> {
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
        for body in subs_boxes {
            push_atom(&mut stbl, *b"subs", body);
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
fn subs_v0_sparse_lookup_decodes() {
    // Row 1: delta 1 → sample 1, two sub-samples.
    // Row 2: delta 2 → sample 3, one sub-sample (discardable).
    let body = build_subs(
        0,
        0,
        &[
            (1, &[(64, 5, 0, 0), (1380, 1, 0, 0)]),
            (2, &[(120, 0, 1, 0xCAFE_0001)]),
        ],
    );
    let file = build_video_qt_with_subs(&[body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open subs fixture");

    let s1 = d.sub_samples(0, 1).expect("sample 1 has subs");
    assert_eq!(s1.len(), 2);
    assert_eq!(s1[0].subsample_size, 64);
    assert_eq!(s1[0].subsample_priority, 5);
    assert!(!s1[0].is_discardable());
    assert_eq!(s1[1].subsample_size, 1380);

    let s3 = d.sub_samples(0, 3).expect("sample 3 has subs");
    assert_eq!(s3.len(), 1);
    assert_eq!(s3[0].subsample_size, 120);
    assert!(s3[0].is_discardable());
    assert_eq!(s3[0].codec_specific_parameters, 0xCAFE_0001);

    // Samples 2 and 4 are not named by any row → no sub-sample info.
    assert!(d.sub_samples(0, 2).is_none());
    assert!(d.sub_samples(0, 4).is_none());
    // Out-of-range sample / track.
    assert!(d.sub_samples(0, 100).is_none());
    assert!(d.sub_samples(7, 1).is_none());
}

#[test]
fn subs_v1_32bit_sizes_decode() {
    // A sub-sample larger than 16 bits is only representable under v1.
    let big = 0x0002_0000u32;
    let body = build_subs(1, 0, &[(2, &[(big, 0, 0, 0)])]);
    let file = build_video_qt_with_subs(&[body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open v1 subs fixture");

    let s2 = d.sub_samples(0, 2).expect("sample 2 has subs");
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0].subsample_size, big);
}

#[test]
fn subs_two_boxes_merge_per_sample() {
    // §8.7.7.1: a track may carry more than one `subs` box, with
    // differing `flags`. Both describe sample 1 here; the demuxer
    // concatenates their sub-sample lists in box order, and an
    // additional box-2-only sample (sample 2) appears alongside.
    let box_a = build_subs(0, 0, &[(1, &[(10, 0, 0, 0)])]);
    let box_b = build_subs(0, 1, &[(1, &[(20, 0, 0, 0)]), (1, &[(30, 0, 0, 0)])]);
    let file = build_video_qt_with_subs(&[box_a, box_b]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open merged subs fixture");

    // Sample 1: box A's [10] then box B's [20], concatenated.
    let s1 = d.sub_samples(0, 1).expect("sample 1 merged");
    assert_eq!(s1.len(), 2);
    assert_eq!(s1[0].subsample_size, 10);
    assert_eq!(s1[1].subsample_size, 20);

    // Sample 2: only box B names it (delta 1 from box B's sample 1).
    let s2 = d.sub_samples(0, 2).expect("sample 2 from box B");
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0].subsample_size, 30);
}

#[test]
fn subs_zero_delta_is_rejected() {
    // A zero sample_delta would place two rows on the same sample
    // number (or a 0-numbered first sample). The sparse coding cannot
    // represent it; the file must be rejected at open.
    let body = build_subs(0, 0, &[(0, &[(10, 0, 0, 0)])]);
    let file = build_video_qt_with_subs(&[body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "a subs box with a zero sample_delta must be rejected"
    );
}

#[test]
fn no_subs_box_yields_none() {
    let file = build_video_qt_with_subs(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open no-subs fixture");
    assert!(d.sub_samples(0, 1).is_none());
}
