//! Round 234 — Padding Bits Box (`padb`) decode.
//!
//! Exercises the `padb` surface (ISO/IEC 14496-12 §8.7.6) against a
//! hand-built QT file whose `stbl` carries a `padb` box. Each sample
//! is encoded with a 3-bit padding-bit count packed two-per-byte; the
//! parser unpacks to one `u8` (in `0..=7`) per sample and the test
//! verifies `MovDemuxer::sample_padding_bits` returns the per-sample
//! values supplied at build time.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{parse_padb, MovDemuxer};

/// Build a `padb` body for the given per-sample padding-bit counts.
/// Layout per §8.7.6.2: `[version:1][flags:3] sample_count:u32` then
/// `(sample_count + 1) / 2` packed bytes where each byte holds
/// `[res:1][pad1:3][res:1][pad2:3]` MSB-first. For an odd
/// `sample_count`, the trailing nibble is reserved=0/pad2=0.
fn build_padb(padding: &[u8]) -> Vec<u8> {
    for (i, &v) in padding.iter().enumerate() {
        assert!(v <= 7, "padding value at index {i} out of range: {v}");
    }
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // version=0, flags=0
    p.extend_from_slice(&(padding.len() as u32).to_be_bytes());
    let packed_rows = padding.len().div_ceil(2);
    for row in 0..packed_rows {
        let lo = row * 2;
        let pad1 = padding[lo] & 0x07;
        let pad2 = if lo + 1 < padding.len() {
            padding[lo + 1] & 0x07
        } else {
            0
        };
        // bit 7 = reserved (0), bits 6..4 = pad1, bit 3 = reserved (0),
        // bits 2..0 = pad2.
        let b = (pad1 << 4) | pad2;
        p.push(b);
    }
    p
}

/// Build a video QT file carrying a `padb` box in `stbl`.
fn build_video_qt_with_padb(padding: &[u8]) -> Vec<u8> {
    let nsamples = padding.len() as u32;
    let mdat_payload: Vec<u8> = (0..nsamples as u8).map(|i| i.wrapping_add(0x21)).collect();

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
        push_atom(&mut stbl, *b"padb", &build_padb(padding));
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

/// Even-count round-trip: every sample's padding-bit count surfaces
/// unmodified through `MovDemuxer::sample_padding_bits`. Four samples
/// fit in exactly two packed bytes with no trailing nibble; the parser
/// must read `pad1` from each byte's high nibble and `pad2` from its
/// low nibble in order.
#[test]
fn padb_even_count_round_trips() {
    let padding = [0u8, 7, 3, 5];
    let buf = build_video_qt_with_padb(&padding);
    let d = open(buf);

    for (i, &want) in padding.iter().enumerate() {
        let got = d
            .sample_padding_bits(0, i as u32)
            .unwrap_or_else(|| panic!("missing padding for sample {i}"));
        assert_eq!(got, want, "sample {i} padding-bit count mismatch");
    }
    // Past-the-end index returns None.
    assert!(d.sample_padding_bits(0, 4).is_none());
    // Out-of-range track returns None.
    assert!(d.sample_padding_bits(7, 0).is_none());
}

/// Odd-count round-trip: an odd `sample_count` triples the packed-byte
/// count to two bytes but only three sample values are addressable.
/// The trailing nibble is reserved=0/pad2=0 per §8.7.6.2 and the
/// parser surfaces exactly `sample_count` entries — no phantom fourth
/// value.
#[test]
fn padb_odd_count_drops_trailing_nibble() {
    let padding = [4u8, 1, 6];
    let buf = build_video_qt_with_padb(&padding);
    let d = open(buf);

    for (i, &want) in padding.iter().enumerate() {
        assert_eq!(d.sample_padding_bits(0, i as u32), Some(want));
    }
    assert!(d.sample_padding_bits(0, 3).is_none());
}

/// Boundary values: the full 0..=7 3-bit range round-trips
/// unmodified.
#[test]
fn padb_full_3bit_range_round_trips() {
    let padding = [0u8, 1, 2, 3, 4, 5, 6, 7];
    let buf = build_video_qt_with_padb(&padding);
    let d = open(buf);
    for (i, &want) in padding.iter().enumerate() {
        assert_eq!(d.sample_padding_bits(0, i as u32), Some(want));
    }
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

/// A second `padb` in the same `stbl` is silently ignored — first wins.
/// ISO/IEC 14496-12 §8.7.6.1 lists the box as `Quantity: Zero or one`;
/// tolerating duplicates first-wins matches the conservative-merge
/// policy used by every other "at most once" stbl-scope box.
#[test]
fn padb_duplicate_box_in_stbl_first_wins() {
    let first = [3u8, 5, 7, 1];
    let second = [0u8, 0, 0, 0];
    let mdat_payload: Vec<u8> = vec![0xb1, 0xb2, 0xb3, 0xb4];

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
/// a raw payload (e.g. a remuxer round-tripping the box).
#[test]
fn parse_padb_direct_invocation() {
    let padding = [2u8, 6, 4, 0, 5];
    let payload = build_padb(&padding);
    let v = parse_padb(&payload).expect("parse padb ok");
    assert_eq!(v, padding);
}

/// Zero samples means an empty table — accepted, parser returns the
/// empty vector.
#[test]
fn parse_padb_zero_samples_is_empty() {
    let payload = build_padb(&[]);
    let v = parse_padb(&payload).unwrap();
    assert!(v.is_empty());
}

/// A header-only `padb` (the FullBox header + sample_count u32 read
/// fine but no packed-table bytes follow) against a non-zero
/// `sample_count` is a truncated table and must be rejected.
#[test]
fn parse_padb_truncated_table_errors() {
    // version=0, flags=0, sample_count=4, but no packed-table bytes.
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&4u32.to_be_bytes());
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("truncated"),
        "expected truncated rejection, got: {err}"
    );
}

/// A body shorter than the 8-byte FullBox header + sample_count must
/// reject — without this guard the version/flags/count read would
/// scrape uninitialised bytes.
#[test]
fn parse_padb_rejects_short_header() {
    let payload = vec![0u8; 7];
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("header"),
        "expected header rejection, got: {err}"
    );
}

/// A non-zero `flags` field is rejected. §8.7.6.2 defines the box as
/// `FullBox('padb', version = 0, 0)` — flags are documented as zero.
#[test]
fn parse_padb_rejects_nonzero_flags() {
    let mut payload = vec![
        0u8,  // version = 0
        0,    // flags high
        0x01, // flags mid (non-zero)
        0,    // flags low
    ];
    payload.extend_from_slice(&1u32.to_be_bytes()); // sample_count = 1
    payload.push(0); // one packed byte
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("flags"),
        "expected flags rejection, got: {err}"
    );
}

/// A non-zero reserved bit (the high bit of either nibble) in a
/// fully-occupied packed byte must reject. §8.7.6.2 fixes both
/// reserved bits at zero; silent acceptance would let a malformed
/// writer piggy-back vendor data on the high bit.
#[test]
fn parse_padb_rejects_nonzero_reserved_high_bit() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // version=0 + flags=0
    payload.extend_from_slice(&2u32.to_be_bytes()); // sample_count = 2
    payload.push(0x80); // high reserved bit = 1, pad1=0, low reserved=0, pad2=0
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("reserved"),
        "expected reserved-bit rejection, got: {err}"
    );
}

/// A non-zero low-reserved bit (bit 3) inside a fully-occupied byte
/// must also reject. The "fully occupied" qualifier matters: the
/// trailing nibble of an odd-count last byte is allowed to be any
/// value since no sample addresses it.
#[test]
fn parse_padb_rejects_nonzero_reserved_low_bit() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // version=0 + flags=0
    payload.extend_from_slice(&2u32.to_be_bytes()); // sample_count = 2
    payload.push(0x08); // high reserved=0, pad1=0, low reserved=1, pad2=0
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("reserved"),
        "expected reserved-bit rejection, got: {err}"
    );
}

/// Odd-count last-byte trailing nibble is permitted to carry the
/// `reserved = 0` bit unchecked: no sample addresses it, so the
/// trailing low-reserved bit is masked off and any pad2 value there
/// has no effect on the returned table.
#[test]
fn parse_padb_odd_count_trailing_nibble_ignored() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // version=0 + flags=0
    payload.extend_from_slice(&1u32.to_be_bytes()); // sample_count = 1
                                                    // pad1 = 5, trailing nibble (which encodes no sample) has reserved=1 and pad2=7.
                                                    // The trailing-nibble reserved check is skipped because the slot is past end-of-table.
    payload.push((5 << 4) | 0x0F);
    let v = parse_padb(&payload).expect("odd-count trailing nibble is unchecked");
    assert_eq!(v, vec![5u8]);
}

/// A `sample_count` larger than the packed-table bytes available is a
/// truncated table and must reject. Specifically `sample_count = 5`
/// requires `ceil(5/2) = 3` packed bytes; if only 2 are present, the
/// parser must error.
#[test]
fn parse_padb_oversize_count_truncated() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // version=0 + flags=0
    payload.extend_from_slice(&5u32.to_be_bytes()); // sample_count = 5 → 3 packed bytes
    payload.push(0x00);
    payload.push(0x00); // only 2 packed bytes
    let err = parse_padb(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("truncated"),
        "expected truncated rejection, got: {err}"
    );
}

/// Single-sample odd-count round-trip: the smallest non-trivial
/// padding table is one sample. The packed byte contains the value in
/// `pad1` and a trailing zero nibble; the parser must surface
/// `Some(value)` for index 0 and `None` past it.
#[test]
fn padb_single_sample_round_trips() {
    let padding = [6u8];
    let buf = build_video_qt_with_padb(&padding);
    let d = open(buf);
    assert_eq!(d.sample_padding_bits(0, 0), Some(6));
    assert!(d.sample_padding_bits(0, 1).is_none());
}
