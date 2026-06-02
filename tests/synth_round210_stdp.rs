//! Round 210 — Degradation Priority Box (`stdp`) decode.
//!
//! Exercises the `stdp` surface (ISO/IEC 14496-12 §8.5.3) against a
//! hand-built QT file whose `stbl` carries an `stdp` box with one
//! 16-bit `priority` per sample. The test opens the file via
//! `MovDemuxer` and verifies `MovDemuxer::sample_degradation_priority`
//! returns the per-sample values supplied at build time. The box has
//! no on-disk count field — its row count is taken from the `stsz`
//! `sample_count` per §8.5.3.1 — so this also verifies the deferred
//! sizing path through the stbl walk.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{parse_stdp, MovDemuxer};

/// Build an `stdp` body for the given per-sample priorities. Layout
/// per §8.5.3.2: `[version:1][flags:3]` then `sample_count × u16`.
fn build_stdp(priorities: &[u16]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // version=0, flags=0
    for &v in priorities {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p
}

/// Build a 4-sample video QT file carrying an `stdp` box in `stbl`.
/// The `priorities` slice supplies one 16-bit value per sample; the
/// sample-size box (`stsz`) declares `priorities.len()` constant-size
/// samples and the `stdp` row count is implied to match per §8.5.3.1.
fn build_video_qt_with_stdp(priorities: &[u16]) -> Vec<u8> {
    let nsamples = priorities.len() as u32;
    // Tiny payload — one byte per sample is plenty for offset arithmetic.
    let mdat_payload: Vec<u8> = (0..nsamples as u8).map(|i| i.wrapping_add(0x11)).collect();

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120 * nsamples));
        let mut trak = Vec::new();
        push_atom(
            &mut trak,
            *b"tkhd",
            &build_tkhd(1, 120 * nsamples, 320, 240),
        );
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 120 * nsamples));
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"vmhd", &build_vmhd());
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_video(b"avc1", 320, 240, &[]),
        );
        push_atom(&mut stbl, *b"stts", &build_stts_single(nsamples, 120));
        push_atom(&mut stbl, *b"stsc", &build_stsc_single(nsamples));
        push_atom(&mut stbl, *b"stsz", &build_stsz_constant(1, nsamples));
        push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
        push_atom(&mut stbl, *b"stdp", &build_stdp(priorities));
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", &mdat_payload);
        out
    };

    // Two-pass layout to resolve the chunk offset of the mdat payload.
    let trial = build_file(0);
    let mdat_fourcc_pos = trial.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

fn open(buf: Vec<u8>) -> MovDemuxer {
    let cursor: Box<dyn ReadSeek + Send + Sync> = Box::new(Cursor::new(buf));
    MovDemuxer::open(cursor).expect("open stdp fixture")
}

/// Sized-from-stsz round-trip: every sample's priority surfaces
/// unmodified through `MovDemuxer::sample_degradation_priority`. The
/// raw 16-bit value is returned as-is per §8.5.3.1 (the base format
/// leaves the numeric meaning to derived specifications).
#[test]
fn stdp_per_sample_priority_round_trips() {
    let priorities = [10u16, 20, 5, 0x7fff];
    let buf = build_video_qt_with_stdp(&priorities);
    let d = open(buf);

    for (i, &want) in priorities.iter().enumerate() {
        let got = d
            .sample_degradation_priority(0, i as u32)
            .unwrap_or_else(|| panic!("missing priority for sample {i}"));
        assert_eq!(got, want, "sample {i} priority mismatch");
    }
    // Past-the-end index returns None.
    assert!(d.sample_degradation_priority(0, 4).is_none());
    // Out-of-range track returns None.
    assert!(d.sample_degradation_priority(7, 0).is_none());
}

/// Boundary value: the full 16-bit range — including 0x0000 and
/// 0xffff — round-trips unmodified. Some derived specs use the
/// extremes as sentinels; the base box must not clamp them.
#[test]
fn stdp_priority_full_u16_range_round_trips() {
    let priorities = [0u16, 0xffff, 1, 0xfffe];
    let buf = build_video_qt_with_stdp(&priorities);
    let d = open(buf);
    for (i, &want) in priorities.iter().enumerate() {
        assert_eq!(d.sample_degradation_priority(0, i as u32), Some(want));
    }
}

/// A track with NO `stdp` box returns `None` for every sample index.
#[test]
fn stdp_absent_box_returns_none() {
    // Re-use the existing helper to build a baseline file *without*
    // an stdp child, by reaching for the round-98 / round-204 path
    // through a tiny inlined builder. The simplest way is to build a
    // file via `build_video_qt_with_stdp(&[…])` and strip the stdp
    // child from the result — but that's brittle to layout. Instead
    // we hand-build the minimum file with no stdp at all.
    let priorities: [u16; 0] = [];
    // Build using the helper with an empty slice would give 0 samples
    // (invalid). Hand-build the minimum 4-sample file without stdp by
    // reusing the same builder but with no priorities argument is
    // not possible; so we accept the slightly different path:
    // construct a 4-sample file using stsz only, no stdp.
    let _ = priorities; // silence unused
    let mdat_payload: Vec<u8> = vec![1, 2, 3, 4];
    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 480));
        let mut trak = Vec::new();
        push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 480, 320, 240));
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 480));
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"vmhd", &build_vmhd());
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_video(b"avc1", 320, 240, &[]),
        );
        push_atom(&mut stbl, *b"stts", &build_stts_single(4, 120));
        push_atom(&mut stbl, *b"stsc", &build_stsc_single(4));
        push_atom(&mut stbl, *b"stsz", &build_stsz_constant(1, 4));
        push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", &mdat_payload);
        out
    };
    let trial = build_file(0);
    let pos = trial.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let buf = build_file(pos + 4);
    let d = open(buf);
    assert!(d.sample_degradation_priority(0, 0).is_none());
    assert!(d.sample_degradation_priority(0, 3).is_none());
}

/// A second `stdp` in the same `stbl` is silently ignored — first
/// wins. ISO/IEC 14496-12 §8.5.3 lists the box as
/// `Quantity: Zero or one`; tolerating duplicates first-wins matches
/// the conservative-merge policy used by every other "at most once"
/// stbl-scope box in this crate (`sdtp`, `sbgp`, `saiz`, `saio` etc.).
#[test]
fn stdp_duplicate_box_in_stbl_first_wins() {
    // Hand-build a file with TWO stdp children whose priority values
    // differ. The first one's values must survive.
    let first = [11u16, 22, 33, 44];
    let second = [99u16, 88, 77, 66];
    let mdat_payload: Vec<u8> = vec![0xa1, 0xa2, 0xa3, 0xa4];

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 480));
        let mut trak = Vec::new();
        push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 480, 320, 240));
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 480));
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"vmhd", &build_vmhd());
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_video(b"avc1", 320, 240, &[]),
        );
        push_atom(&mut stbl, *b"stts", &build_stts_single(4, 120));
        push_atom(&mut stbl, *b"stsc", &build_stsc_single(4));
        push_atom(&mut stbl, *b"stsz", &build_stsz_constant(1, 4));
        push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
        push_atom(&mut stbl, *b"stdp", &build_stdp(&first));
        push_atom(&mut stbl, *b"stdp", &build_stdp(&second));
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", &mdat_payload);
        out
    };
    let trial = build_file(0);
    let pos = trial.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let buf = build_file(pos + 4);
    let d = open(buf);
    for (i, &want) in first.iter().enumerate() {
        assert_eq!(d.sample_degradation_priority(0, i as u32), Some(want));
    }
}

/// `parse_stdp` direct API — the public surface for callers that hold a
/// raw payload (e.g. a remuxer that round-trips the box without ever
/// constructing an entire `MovDemuxer`).
#[test]
fn parse_stdp_direct_invocation() {
    let priorities = [7u16, 13, 29, 31];
    let payload = build_stdp(&priorities);
    let v = parse_stdp(&payload, 4).expect("parse stdp ok");
    assert_eq!(v, priorities);
}

/// A header-only `stdp` (zero priority bytes) against a non-zero
/// `sample_count` is a truncated table and must be rejected. §8.5.3.1
/// sizes the box from the sample-size box; silently emitting an empty
/// dependency table would be wrong.
#[test]
fn parse_stdp_truncated_table_errors() {
    let payload = build_stdp(&[]); // header only, no rows
    let err = parse_stdp(&payload, 3).unwrap_err();
    assert!(
        format!("{err}").contains("truncated"),
        "expected truncated rejection, got: {err}"
    );
}

/// A body shorter than the 4-byte FullBox header must reject — without
/// this guard the version/flags read would scrape uninitialised bytes.
#[test]
fn parse_stdp_rejects_short_header() {
    let payload = vec![0u8, 0, 0]; // 3 bytes only
    let err = parse_stdp(&payload, 0).unwrap_err();
    assert!(
        format!("{err}").contains("header"),
        "expected header rejection, got: {err}"
    );
}

/// Zero samples means an empty table — accepted, parser returns the
/// empty vector regardless of trailing padding.
#[test]
fn parse_stdp_zero_samples_is_empty() {
    let payload = build_stdp(&[]); // header only
    let v = parse_stdp(&payload, 0).unwrap();
    assert!(v.is_empty());
}

/// Trailing padding past the declared `sample_count` is silently
/// ignored. Some writers round the box body up to an 8-byte boundary;
/// the parser must read exactly `sample_count * 2` bytes and stop.
#[test]
fn parse_stdp_ignores_trailing_padding() {
    let priorities = [42u16, 7];
    let mut payload = build_stdp(&priorities);
    payload.extend_from_slice(&[0u8; 4]); // 4 bytes of padding
    let v = parse_stdp(&payload, 2).unwrap();
    assert_eq!(v, priorities);
}

/// A non-zero `flags` field is rejected. §8.5.3.2 defines the box as
/// `FullBox('stdp', version = 0, 0)` — flags are documented as zero.
/// Silent acceptance would let a corrupt or non-conformant writer leak
/// vendor bits past the parser.
#[test]
fn parse_stdp_rejects_nonzero_flags() {
    let mut payload = vec![
        0u8,  // version = 0
        0,    // flags high
        0x01, // flags mid (non-zero)
        0,    // flags low
    ];
    payload.extend_from_slice(&0u16.to_be_bytes()); // one row
    let err = parse_stdp(&payload, 1).unwrap_err();
    assert!(
        format!("{err}").contains("flags"),
        "expected flags rejection, got: {err}"
    );
}
