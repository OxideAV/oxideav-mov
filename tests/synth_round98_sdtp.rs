//! Round 98 — Independent and Disposable Samples Box (`sdtp`) decode.
//!
//! Exercises the `sdtp` surface (ISO/IEC 14496-12 §8.6.4) against a
//! hand-built QT file whose `stbl` carries a `sdtp` box with one
//! packed byte per sample. The test opens the file via `MovDemuxer`
//! and verifies `MovDemuxer::sample_dependency` agrees with the
//! per-sample field semantics in §8.6.4.3:
//!
//! * sample 0 — `sample_depends_on = 2` (I-picture, independent),
//!   `sample_is_depended_on = 1` (not disposable);
//! * samples 1..3 — `sample_depends_on = 1` (P/B), with sample 2
//!   marked `sample_is_depended_on = 2` (disposable, e.g. a
//!   non-reference B-frame that trick-mode roll-forward may skip).
//!
//! The `sdtp` box has no on-disk count field — its row count equals
//! the `stsz` sample count (§8.6.4.1); this verifies the deferred
//! sizing path through the stbl walk too.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, SampleDependsOn, SampleIsDependedOn};

/// Pack one `sdtp` byte MSB-first (§8.6.4.2): is_leading (bits 7..6),
/// sample_depends_on (5..4), sample_is_depended_on (3..2),
/// sample_has_redundancy (1..0).
fn sdtp_byte(il: u8, sdo: u8, sido: u8, shr: u8) -> u8 {
    ((il & 0x3) << 6) | ((sdo & 0x3) << 4) | ((sido & 0x3) << 2) | (shr & 0x3)
}

/// Build a 4-sample video QT file carrying an `sdtp` box in `stbl`.
fn build_video_qt_with_sdtp(sample_bytes: &[u8]) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_sdtp = || -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
        p.extend_from_slice(sample_bytes); // one packed byte per sample
        p
    };

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
        push_atom(&mut stbl, *b"sdtp", &build_sdtp());
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
fn sdtp_per_sample_dependency_decodes() {
    // sample 0: I-picture (depends_on=2), not disposable (is_dep=1).
    // sample 1: P (depends_on=1), not disposable (is_dep=1).
    // sample 2: B (depends_on=1), disposable (is_dep=2).
    // sample 3: P (depends_on=1), not disposable (is_dep=1).
    let bytes = [
        sdtp_byte(0, 2, 1, 0),
        sdtp_byte(0, 1, 1, 0),
        sdtp_byte(0, 1, 2, 0),
        sdtp_byte(0, 1, 1, 0),
    ];
    let file = build_video_qt_with_sdtp(&bytes);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open sdtp fixture");

    let e0 = d.sample_dependency(0, 0).expect("sample 0 sdtp");
    assert_eq!(e0.sample_depends_on, SampleDependsOn::Independent);
    assert_eq!(e0.sample_is_depended_on, SampleIsDependedOn::NotDisposable);
    assert!(e0.is_independent());
    assert!(!e0.is_disposable());

    let e2 = d.sample_dependency(0, 2).expect("sample 2 sdtp");
    assert_eq!(e2.sample_depends_on, SampleDependsOn::DependsOnOthers);
    assert_eq!(e2.sample_is_depended_on, SampleIsDependedOn::Disposable);
    assert!(!e2.is_independent());
    assert!(e2.is_disposable());

    let e3 = d.sample_dependency(0, 3).expect("sample 3 sdtp");
    assert!(!e3.is_independent());
    assert!(!e3.is_disposable());

    // Past-the-end index returns None.
    assert!(d.sample_dependency(0, 4).is_none());
    // Out-of-range track returns None.
    assert!(d.sample_dependency(7, 0).is_none());
}

#[test]
fn sdtp_shorter_than_stsz_count_is_rejected() {
    // The sdtp body has no on-disk count: §8.6.4.1 sizes it from the
    // stsz sample count (4 here). A header-only sdtp box (zero packed
    // bytes) is therefore a truncated table and must be rejected at
    // open rather than silently mis-parsed.
    let file = build_video_qt_with_sdtp(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "an sdtp box shorter than stsz_count must be rejected"
    );
}
