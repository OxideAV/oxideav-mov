//! Round 234 — Padding Bits Box (`padb`) decode.
//!
//! Exercises the `padb` surface (ISO/IEC 14496-12 §8.7.6) against a
//! hand-built QT file whose `stbl` carries a `padb` box. The box
//! records, for each sample, how many bits at the end of the sample's
//! media payload are writer-inserted padding to round up to a whole
//! byte (§8.7.6.3). Unlike `stdp` / `sdtp`, `padb` carries its own
//! `sample_count` field on disk so the parse does not depend on
//! `stsz` / `stz2`.
//!
//! The test opens each fixture via `MovDemuxer` and verifies
//! `MovDemuxer::sample_padding_bits` returns the per-sample values
//! supplied at build time.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{parse_padb, MovDemuxer};

/// Build a `padb` body for the given per-sample `pad` values. Layout
/// per §8.7.6.2: `[version:1][flags:3][sample_count:4]` then
/// `((sample_count + 1) / 2)` packed bytes, each encoded
/// `[reserved:1, pad1:3, reserved:1, pad2:3]` most-significant nibble
/// first. `pad1` covers sample `(i*2)+1` (1-based) and `pad2`
/// covers sample `(i*2)+2`.
fn build_padb(pads: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // version=0 + flags=0
    p.extend_from_slice(&(pads.len() as u32).to_be_bytes());
    let mut i = 0;
    while i < pads.len() {
        let pad1 = pads[i] & 0x07;
        let pad2 = if i + 1 < pads.len() {
            pads[i + 1] & 0x07
        } else {
            0
        };
        p.push((pad1 << 4) | pad2);
        i += 2;
    }
    p
}

/// Build a video QT file carrying a `padb` box in `stbl`. `pads`
/// supplies one 3-bit `pad` value per sample.
fn build_video_qt_with_padb(pads: &[u8]) -> Vec<u8> {
    let nsamples = pads.len() as u32;
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
        push_atom(&mut stbl, *b"padb", &build_padb(pads));
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", &mdat_payload);
        out
    };

    let trial = build_file(0);
    let mdat_fourcc_pos = trial.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

fn open(buf: Vec<u8>) -> MovDemuxer {
    let cursor: Box<dyn ReadSeek + Send + Sync> = Box::new(Cursor::new(buf));
    MovDemuxer::open(cursor).expect("open padb fixture")
}

/// Even sample count: every sample's `pad` value surfaces unmodified
/// through `MovDemuxer::sample_padding_bits`.
#[test]
fn padb_per_sample_pad_round_trips_even_count() {
    let pads = [0u8, 1, 7, 3];
    let buf = build_video_qt_with_padb(&pads);
    let d = open(buf);

    for (i, &want) in pads.iter().enumerate() {
        let got = d
            .sample_padding_bits(0, i as u32)
            .unwrap_or_else(|| panic!("missing pad for sample {i}"));
        assert_eq!(got, want, "sample {i} pad mismatch");
    }
    // Past-the-end index returns None.
    assert!(d.sample_padding_bits(0, 4).is_none());
    // Out-of-range track returns None.
    assert!(d.sample_padding_bits(7, 0).is_none());
}

/// Odd sample count: the trailing nibble of the final packed byte
/// (§8.7.6.2's `pad2` slot for a non-existent "sample N+1") is
/// silently discarded by the parser.
#[test]
fn padb_odd_sample_count_round_trips() {
    let pads = [5u8, 2, 6];
    let buf = build_video_qt_with_padb(&pads);
    let d = open(buf);
    for (i, &want) in pads.iter().enumerate() {
        assert_eq!(d.sample_padding_bits(0, i as u32), Some(want));
    }
    assert!(d.sample_padding_bits(0, 3).is_none());
}

/// A track with NO `padb` box returns `None` for every sample index.
#[test]
fn padb_absent_box_returns_none() {
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
    assert!(d.sample_padding_bits(0, 0).is_none());
    assert!(d.sample_padding_bits(0, 3).is_none());
}

/// A second `padb` in the same `stbl` is silently ignored — first
/// wins. ISO/IEC 14496-12 §8.7.6.1 lists the box as
/// `Quantity: Zero or one`; tolerating duplicates first-wins matches
/// the conservative-merge policy used by every other "at most once"
/// stbl-scope box in this crate (`sdtp`, `stdp`, `sbgp`, `saiz`, ...).
#[test]
fn padb_duplicate_box_in_stbl_first_wins() {
    let first = [4u8, 2, 0, 6];
    let second = [7u8, 7, 7, 7];
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
        push_atom(&mut stbl, *b"padb", &build_padb(&first));
        push_atom(&mut stbl, *b"padb", &build_padb(&second));
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
        assert_eq!(d.sample_padding_bits(0, i as u32), Some(want));
    }
}

/// `parse_padb` direct API — the public surface for callers that hold
/// a raw payload (e.g. a remuxer round-tripping the box without
/// constructing an entire `MovDemuxer`).
#[test]
fn parse_padb_direct_invocation() {
    let pads = [3u8, 0, 5, 7];
    let payload = build_padb(&pads);
    let v = parse_padb(&payload).expect("parse padb ok");
    assert_eq!(v, pads);
}

/// A header-only `padb` claiming a non-zero `sample_count` is a
/// truncated table and must be rejected. §8.7.6.2 sizes the body
/// from `sample_count`; silently emitting an empty table would be
/// wrong.
#[test]
fn parse_padb_truncated_table_errors() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    payload.extend_from_slice(&4u32.to_be_bytes()); // claim 4 samples
                                                    // no packed bytes follow
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("truncated"),
        "expected truncated rejection, got: {err}"
    );
}

/// A body shorter than the 8-byte FullBox header + sample_count is
/// rejected before the version/flags read scrapes uninitialised bytes.
#[test]
fn parse_padb_rejects_short_header() {
    let payload = vec![0u8, 0, 0, 0, 0, 0, 0]; // 7 bytes only
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("header"),
        "expected header rejection, got: {err}"
    );
}

/// Zero samples: header-only `padb` (sample_count = 0) parses to an
/// empty table.
#[test]
fn parse_padb_zero_samples_is_empty() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes()); // sample_count = 0
    let v = parse_padb(&payload).unwrap();
    assert!(v.is_empty());
}

/// A non-zero `flags` field is rejected. §8.7.6.2 defines the box as
/// `FullBox('padb', version = 0, 0)` — flags are documented as zero.
/// Silent acceptance would let a corrupt writer leak vendor bits past
/// the parser.
#[test]
fn parse_padb_rejects_nonzero_flags() {
    let mut payload = vec![
        0u8,  // version = 0
        0,    // flags high
        0x01, // flags mid (non-zero)
        0,    // flags low
    ];
    payload.extend_from_slice(&2u32.to_be_bytes()); // sample_count
    payload.push(0); // one packed byte
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("flags"),
        "expected flags rejection, got: {err}"
    );
}

/// A non-zero `version` field is rejected. §8.7.6.2 fixes version
/// at 0; surfacing the value silently would let a writer claiming a
/// non-existent extension reach downstream code paths.
#[test]
fn parse_padb_rejects_nonzero_version() {
    let mut payload = vec![1u8, 0, 0, 0]; // version=1
    payload.extend_from_slice(&2u32.to_be_bytes()); // sample_count
    payload.push(0);
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("version"),
        "expected version rejection, got: {err}"
    );
}

/// The 0x80 and 0x08 reserved bits in each packed byte are spec-fixed
/// at 0 (§8.7.6.2); a writer that leaks bits there is rejected.
#[test]
fn parse_padb_rejects_reserved_bit() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&2u32.to_be_bytes());
    payload.push(0b1001_0010); // reserved bit in pad1 slot set
    assert!(parse_padb(&payload).is_err());

    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&2u32.to_be_bytes());
    payload.push(0b0001_1010); // reserved bit in pad2 slot set
    assert!(parse_padb(&payload).is_err());
}
