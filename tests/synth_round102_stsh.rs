//! Round 102 — Shadow Sync Sample Box (`stsh`) decode.
//!
//! Exercises the `stsh` surface (ISO/IEC 14496-12 §8.6.3) against a
//! hand-built QT file whose `stbl` carries an `stsh` box pairing
//! shadowed (non-sync) samples with the alternative sync sample whose
//! media data substitutes for them when a sync sample is needed at, or
//! before, the shadowed one.
//!
//! Both sample numbers are 1-based, like `stss`. The test opens the
//! file via `MovDemuxer` and verifies
//! `MovDemuxer::shadow_sync_sample` resolves the table by exact
//! `shadowed_sample_number` and returns `None` for non-shadowed and
//! out-of-range samples.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build an `stsh` payload: FullBox header + entry_count + N pairs of
/// (shadowed_sample_number, sync_sample_number), both 1-based and
/// sorted ascending by the shadowed number per §8.6.3.1.
fn build_stsh(pairs: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
    p.extend_from_slice(&(pairs.len() as u32).to_be_bytes());
    for (shadowed, sync) in pairs {
        p.extend_from_slice(&shadowed.to_be_bytes());
        p.extend_from_slice(&sync.to_be_bytes());
    }
    p
}

/// Build a 4-sample video QT file carrying an `stsh` box in `stbl`.
fn build_video_qt_with_stsh(pairs: &[(u32, u32)]) -> Vec<u8> {
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
        // sample 1 is the sole sync sample; samples 4 and 8 (out of a
        // longer logical track) are shadowed — here we shadow within
        // the 4-sample table: sample 3 → sync sample 1.
        push_atom(&mut stbl, *b"stsh", &build_stsh(pairs));
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
fn stsh_shadow_sync_lookup_decodes() {
    // Two shadow entries: sample 3 → sync 1, sample 4 → sync 1.
    let file = build_video_qt_with_stsh(&[(3, 1), (4, 1)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open stsh fixture");

    assert_eq!(d.shadow_sync_sample(0, 3), Some(1));
    assert_eq!(d.shadow_sync_sample(0, 4), Some(1));
    // A sample with no shadow entry.
    assert_eq!(d.shadow_sync_sample(0, 1), None);
    assert_eq!(d.shadow_sync_sample(0, 2), None);
    // Sample numbers outside the table.
    assert_eq!(d.shadow_sync_sample(0, 0), None);
    assert_eq!(d.shadow_sync_sample(0, 99), None);
    // Out-of-range track returns None.
    assert_eq!(d.shadow_sync_sample(7, 3), None);
}

#[test]
fn stsh_absent_returns_none() {
    // An empty stsh table is structurally valid but yields no lookups.
    let file = build_video_qt_with_stsh(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open empty-stsh fixture");
    assert_eq!(d.shadow_sync_sample(0, 1), None);
    assert_eq!(d.shadow_sync_sample(0, 3), None);
}

#[test]
fn stsh_non_increasing_shadowed_number_is_rejected() {
    // §8.6.3.1 requires entries sorted ascending by
    // shadowed_sample_number; an out-of-order table is rejected at
    // open time rather than silently mis-indexed.
    let file = build_video_qt_with_stsh(&[(4, 1), (3, 1)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "a non-monotonic stsh table must be rejected"
    );
}
