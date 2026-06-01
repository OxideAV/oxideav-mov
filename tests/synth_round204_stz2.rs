//! Round 204 — Compact Sample Size Box (`stz2`) decode.
//!
//! Exercises the `stz2` surface (ISO/IEC 14496-12 §8.7.3.3) against
//! three hand-built QT files whose `stbl` carries the box at each
//! defined `field_size` (4, 8, 16 bits). The tests verify that:
//!
//! * `MovDemuxer::sample_size_source(track)` correctly reports
//!   `SampleSizeSource::Stz2 { field_size }` after parse;
//! * the per-sample `SampleEntry::size` values exactly equal what
//!   was packed on disk, regardless of field width;
//! * the 4-bit packing convention from §8.7.3.3.2 ("each byte contains
//!   two values: entry[i]<<4 + entry[i+1]") round-trips both even and
//!   odd sample counts, with the trailing pad nibble dropped for odd
//!   counts (§8.7.3.3.2: "the last byte is padded with zeros");
//! * a `field_size` other than 4 / 8 / 16 is rejected at open time;
//! * a non-zero 24-bit `reserved` word is rejected at open time;
//! * a truncated entry table is rejected at open time;
//! * a `stbl` whose only sample-size box is `stz2` (i.e. no `stsz`) is
//!   accepted — the two boxes are mutually-exclusive per §8.7.3.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{parse_stz2, MovDemuxer, SampleSizeSource};

/// Build a `stz2` payload for the given `field_size` (4 / 8 / 16) and
/// per-sample sizes. The `reserved` 24-bit word is fixed at 0 per
/// §8.7.3.3.1. Sizes that don't fit in the declared field width are
/// truncated by the caller's intent — we trust the test author.
fn build_stz2(field_size: u8, sizes: &[u32]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags=0
    p.push(0); // reserved high byte
    p.push(0); // reserved mid byte
    p.push(0); // reserved low byte
    p.push(field_size); // field_size
    p.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
    match field_size {
        4 => {
            // Two entries per byte, MSB-first. Odd-count tail gets a
            // zero low nibble (§8.7.3.3.2).
            let mut i = 0;
            while i < sizes.len() {
                let hi = (sizes[i] & 0x0f) as u8;
                let lo = if i + 1 < sizes.len() {
                    (sizes[i + 1] & 0x0f) as u8
                } else {
                    0
                };
                p.push((hi << 4) | lo);
                i += 2;
            }
        }
        8 => {
            for &s in sizes {
                p.push((s & 0xff) as u8);
            }
        }
        16 => {
            for &s in sizes {
                p.extend_from_slice(&((s & 0xffff) as u16).to_be_bytes());
            }
        }
        _ => panic!("invalid field_size in test helper"),
    }
    p
}

/// Build a video QT file whose `stbl` uses `stz2` (not `stsz`) as its
/// sample-size box, with `sizes` per-sample bytes packed at
/// `field_size` bits each. The `mdat` payload is sized to fit the sum
/// of per-sample sizes so the sample iterator's byte ranges land
/// in-bounds.
fn build_video_qt_with_stz2(field_size: u8, sizes: &[u32]) -> Vec<u8> {
    let total_bytes: u32 = sizes.iter().sum();
    let mdat_payload: Vec<u8> = (0..total_bytes as u8).collect();

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(
            &mut moov,
            *b"mvhd",
            &build_mvhd(600, 120 * sizes.len() as u32),
        );
        let mut trak = Vec::new();
        push_atom(
            &mut trak,
            *b"tkhd",
            &build_tkhd(1, 120 * sizes.len() as u32, 320, 240),
        );
        let mut mdia = Vec::new();
        push_atom(
            &mut mdia,
            *b"mdhd",
            &build_mdhd(600, 120 * sizes.len() as u32),
        );
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"vmhd", &build_vmhd());
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_video(b"avc1", 320, 240, &[]),
        );
        push_atom(
            &mut stbl,
            *b"stts",
            &build_stts_single(sizes.len() as u32, 120),
        );
        push_atom(&mut stbl, *b"stsc", &build_stsc_single(sizes.len() as u32));
        // *** This is the round-204 substitution: stz2 instead of stsz. ***
        push_atom(&mut stbl, *b"stz2", &build_stz2(field_size, sizes));
        push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", &mdat_payload);
        out
    };

    // Two-pass layout to resolve the chunk offset.
    let trial = build_file(0);
    let mdat_payload_offset = (trial.len() - mdat_payload.len() - 8/* mdat header */) as u32 + 8;
    build_file(mdat_payload_offset)
}

fn open(buf: Vec<u8>) -> MovDemuxer {
    let cursor: Box<dyn ReadSeek + Send + Sync> = Box::new(Cursor::new(buf));
    MovDemuxer::open(cursor).expect("open stz2 fixture")
}

/// Field size 8: most common compact-encoding choice for streams whose
/// per-sample sizes fit in a single byte (e.g. small text-track lines,
/// extremely low-bitrate audio frames).
#[test]
fn stz2_field_size_8_round_trips() {
    let sizes = [3u32, 5, 2, 7];
    let buf = build_video_qt_with_stz2(8, &sizes);
    let dm = open(buf);
    assert_eq!(
        dm.sample_size_source(0),
        Some(SampleSizeSource::Stz2 { field_size: 8 })
    );
    let table = &dm.tracks[0].sample_table;
    assert_eq!(table.sample_count(), sizes.len() as u32);
    for (i, &want) in sizes.iter().enumerate() {
        assert_eq!(table.stsz_table[i], want, "sample {i} size mismatch");
    }
    // `stsz_default_size` is `None` because §8.7.3.3 has no constant-size
    // branch — every entry is listed explicitly.
    assert!(table.stsz_default_size.is_none());
}

/// Field size 16: covers per-sample sizes up to 65 535 bytes.
#[test]
fn stz2_field_size_16_round_trips() {
    let sizes = [1024u32, 2048, 4096, 8192];
    let buf = build_video_qt_with_stz2(16, &sizes);
    let dm = open(buf);
    assert_eq!(
        dm.sample_size_source(0),
        Some(SampleSizeSource::Stz2 { field_size: 16 })
    );
    let table = &dm.tracks[0].sample_table;
    assert_eq!(table.sample_count(), sizes.len() as u32);
    for (i, &want) in sizes.iter().enumerate() {
        assert_eq!(table.stsz_table[i], want);
    }
}

/// Field size 4 with an *even* count: two entries per byte, no padding.
#[test]
fn stz2_field_size_4_even_count() {
    // Each entry must fit in 4 bits (0..=15).
    let sizes = [1u32, 2, 3, 4, 5, 6];
    let buf = build_video_qt_with_stz2(4, &sizes);
    let dm = open(buf);
    assert_eq!(
        dm.sample_size_source(0),
        Some(SampleSizeSource::Stz2 { field_size: 4 })
    );
    let table = &dm.tracks[0].sample_table;
    assert_eq!(table.sample_count(), sizes.len() as u32);
    for (i, &want) in sizes.iter().enumerate() {
        assert_eq!(table.stsz_table[i], want, "sample {i}: 4-bit entry");
    }
}

/// Field size 4 with an *odd* count: §8.7.3.3.2 mandates "the last byte
/// is padded with zeros". The trailing pad nibble must be silently
/// dropped — the table's length matches `sample_count` exactly.
#[test]
fn stz2_field_size_4_odd_count_drops_pad_nibble() {
    let sizes = [10u32, 12, 14, 8, 6]; // 5 samples — needs ceil(5/2) = 3 bytes
    let buf = build_video_qt_with_stz2(4, &sizes);
    let dm = open(buf);
    assert_eq!(
        dm.sample_size_source(0),
        Some(SampleSizeSource::Stz2 { field_size: 4 })
    );
    let table = &dm.tracks[0].sample_table;
    assert_eq!(table.sample_count(), 5);
    assert_eq!(table.stsz_table.len(), 5);
    for (i, &want) in sizes.iter().enumerate() {
        assert_eq!(table.stsz_table[i], want);
    }
}

/// `parse_stz2` direct API — the public surface for callers that hold a
/// raw payload (e.g. a sample-aux-info round-trip writer).
#[test]
fn parse_stz2_direct_invocation() {
    let sizes = [1u32, 2, 3, 4];
    let payload = build_stz2(8, &sizes);
    let (field_size, count, table) = parse_stz2(&payload).expect("parse stz2 ok");
    assert_eq!(field_size, 8);
    assert_eq!(count, 4);
    assert_eq!(table, vec![1, 2, 3, 4]);
}

/// `field_size = 2` (or any value other than 4 / 8 / 16) must reject at
/// open time. §8.7.3.3.2 enumerates exactly the three valid widths.
#[test]
fn stz2_rejects_invalid_field_size() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    payload.push(0); // reserved
    payload.push(0);
    payload.push(0);
    payload.push(2); // *** invalid field_size ***
    payload.extend_from_slice(&0u32.to_be_bytes()); // sample_count = 0
    let err = parse_stz2(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("field_size"),
        "expected field_size rejection, got: {err}"
    );
}

/// The 24-bit `reserved` word is spec-fixed at 0. A non-zero value
/// must reject at open time — silent acceptance would let a corrupt or
/// non-conformant writer leak vendor bits past the parser.
#[test]
fn stz2_rejects_nonzero_reserved() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    payload.push(0x12); // *** non-zero reserved ***
    payload.push(0x34);
    payload.push(0x56);
    payload.push(8); // field_size
    payload.extend_from_slice(&0u32.to_be_bytes()); // sample_count = 0
    let err = parse_stz2(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("reserved"),
        "expected reserved rejection, got: {err}"
    );
}

/// A `sample_count` that overruns the on-disk body must reject — the
/// 64-MiB `MAX_INMEMORY_ATOM_BODY` cap is enforced at the atom-walker
/// level; this check catches a body that *did* fit in memory but whose
/// declared count claims more bytes than the payload carries.
#[test]
fn stz2_rejects_truncated_entry_table() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.push(0);
    payload.push(0);
    payload.push(0);
    payload.push(16); // field_size = 16 → 2 bytes per entry
    payload.extend_from_slice(&10u32.to_be_bytes()); // sample_count = 10 → need 20 bytes
    payload.extend_from_slice(&[0u8; 12]); // only 12 bytes — short by 8
    let err = parse_stz2(&payload).unwrap_err();
    assert!(
        format!("{err}").contains("truncated"),
        "expected truncation rejection, got: {err}"
    );
}

/// A body shorter than the 12-byte fixed header must reject — without
/// this the field_size / sample_count read would scrape uninitialised
/// stack bytes.
#[test]
fn stz2_rejects_short_header() {
    let payload = [0u8; 11];
    let err = parse_stz2(&payload).unwrap_err();
    assert!(format!("{err}").contains("stz2"));
}

/// Cross-check: an `stbl` carrying a normal `stsz` reports
/// `SampleSizeSource::Stsz`, not `Stz2 { .. }`. This pins the
/// round-204 discriminator's `stsz` half of the symmetry.
#[test]
fn stsz_path_reports_stsz_source() {
    // Re-use the existing `sdtp` fixture's shape: a single video track
    // with a constant-size `stsz`. We only need to confirm the
    // sample-size source enum reports `Stsz`.
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";
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
    push_atom(&mut stbl, *b"stco", &build_stco_single(0));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    push_atom(&mut out, *b"mdat", mdat_payload);

    let dm = open(out);
    assert_eq!(dm.sample_size_source(0), Some(SampleSizeSource::Stsz));
}
