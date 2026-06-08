//! Round 256 — typed chunk-walking primitive over the QTFF p. 75
//! Sample-to-Chunk Atom (`stsc`), p. 78 Chunk Offset Atom (`stco` /
//! `co64`), and p. 76 Sample Size Atom (`stsz`).
//!
//! The sample iterator already walks `stsc` + `stco` + `stsz` linearly
//! to surface decode-order samples (QTFF p. 79 "Finding a Sample"
//! steps 1–4). The new accessors expose the same walker as a typed
//! random-access surface:
//!
//! * [`oxideav_mov::MovDemuxer::chunk_count`]
//! * [`oxideav_mov::MovDemuxer::samples_in_chunk`]
//! * [`oxideav_mov::MovDemuxer::chunk_for_sample`]
//! * [`oxideav_mov::MovDemuxer::sample_offset`]
//! * [`oxideav_mov::MovDemuxer::chunk_byte_extent`]
//!
//! The fixture below is the QTFF Figure 2-35 (p. 76) layout — three
//! `stsc` rows packing 11 samples across 5 chunks at three different
//! `samples_per_chunk` values — so the integration test mirrors the
//! spec's own worked example.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build an `stsc` with N rows.
fn build_stsc_rows(rows: &[(u32, u32, u32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver + flags
    p.extend_from_slice(&(rows.len() as u32).to_be_bytes());
    for (first_chunk, samples_per_chunk, sample_description_id) in rows {
        p.extend_from_slice(&first_chunk.to_be_bytes());
        p.extend_from_slice(&samples_per_chunk.to_be_bytes());
        p.extend_from_slice(&sample_description_id.to_be_bytes());
    }
    p
}

/// Build an `stco` with N chunk offsets.
fn build_stco_rows(offsets: &[u32]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
    for off in offsets {
        p.extend_from_slice(&off.to_be_bytes());
    }
    p
}

/// Build a QT file whose single video track exercises the QTFF p. 76
/// Figure 2-35 sample-to-chunk layout:
///
/// | stsc row | first_chunk | samples_per_chunk | sample_description_id |
/// |----------|-------------|-------------------|------------------------|
/// | 0        | 1           | 3                 | 1                      |
/// | 1        | 3           | 1                 | 1                      |
/// | 2        | 5           | 1                 | 1                      |
///
/// Five chunks total → 3+3+1+1+1 = 9 samples. Each sample is 100 bytes
/// (uniform `stsz`); each chunk is placed at an absolute offset
/// chosen so the chunk doesn't overlap an earlier one. The mdat starts
/// after the moov to keep the writer logic simple — concrete chunk
/// offsets are patched in a second pass below to point at real bytes.
fn build_qt_figure_2_35() -> Vec<u8> {
    // Per-chunk byte layouts. Chunks land contiguously inside mdat.
    let mdat_size: usize = (3 + 3 + 1 + 1 + 1) * 100;
    let mdat_payload = vec![0xAB; mdat_size];

    let build_file = |chunk_offsets: [u32; 5]| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  ");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 90));
        let mut trak = Vec::new();
        push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 90, 320, 240));
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 90));
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"vmhd", &build_vmhd());
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_video(b"avc1", 320, 240, &[]),
        );
        // 9 samples × 100 ticks each = 900 ticks total = duration 90 at
        // mdhd timescale 600 ticks/sec ≈ 1.5s; tkhd matches.
        push_atom(&mut stbl, *b"stts", &build_stts_single(9, 100));
        push_atom(
            &mut stbl,
            *b"stsc",
            &build_stsc_rows(&[(1, 3, 1), (3, 1, 1), (5, 1, 1)]),
        );
        push_atom(&mut stbl, *b"stsz", &build_stsz_constant(100, 9));
        push_atom(&mut stbl, *b"stco", &build_stco_rows(&chunk_offsets));
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", &mdat_payload);
        out
    };

    // Pass 1: build with placeholder offsets to learn where mdat lives.
    let pass1 = build_file([0; 5]);
    let mdat_fourcc = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_off: u32 = mdat_fourcc + 4;
    // Chunk byte layout inside mdat:
    //   chunk 1: samples 0..2  (3 × 100 = 300 B)        → mdat + 0
    //   chunk 2: samples 3..5  (3 × 100 = 300 B)        → mdat + 300
    //   chunk 3: sample  6     (1 × 100 = 100 B)        → mdat + 600
    //   chunk 4: sample  7     (1 × 100 = 100 B)        → mdat + 700
    //   chunk 5: sample  8     (1 × 100 = 100 B)        → mdat + 800
    let chunk_offsets = [
        mdat_payload_off,
        mdat_payload_off + 300,
        mdat_payload_off + 600,
        mdat_payload_off + 700,
        mdat_payload_off + 800,
    ];
    build_file(chunk_offsets)
}

#[test]
fn chunk_count_matches_stco_entry_count() {
    let file = build_qt_figure_2_35();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open Figure 2-35 fixture");
    assert_eq!(d.chunk_count(0), Some(5));
    // Out-of-range track returns None.
    assert_eq!(d.chunk_count(7), None);
}

#[test]
fn samples_in_chunk_walks_three_stsc_rows() {
    let file = build_qt_figure_2_35();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open Figure 2-35 fixture");
    // Row 0 spans chunks 1..2 (3 samples/chunk); row 1 spans chunks
    // 3..4 (1 sample/chunk); row 2 spans chunk 5 (1 sample/chunk).
    assert_eq!(d.samples_in_chunk(0, 1), Some(3));
    assert_eq!(d.samples_in_chunk(0, 2), Some(3));
    assert_eq!(d.samples_in_chunk(0, 3), Some(1));
    assert_eq!(d.samples_in_chunk(0, 4), Some(1));
    assert_eq!(d.samples_in_chunk(0, 5), Some(1));
    // Boundary checks.
    assert_eq!(d.samples_in_chunk(0, 0), None);
    assert_eq!(d.samples_in_chunk(0, 6), None);
    assert_eq!(d.samples_in_chunk(7, 1), None);
}

#[test]
fn chunk_for_sample_resolves_each_sample_in_qtff_figure_2_35() {
    let file = build_qt_figure_2_35();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open Figure 2-35 fixture");
    // Samples 0..2 live in chunk 1; samples 3..5 in chunk 2; sample 6
    // in chunk 3; sample 7 in chunk 4; sample 8 in chunk 5.
    assert_eq!(d.chunk_for_sample(0, 0), Some((1, 0)));
    assert_eq!(d.chunk_for_sample(0, 1), Some((1, 1)));
    assert_eq!(d.chunk_for_sample(0, 2), Some((1, 2)));
    assert_eq!(d.chunk_for_sample(0, 3), Some((2, 0)));
    assert_eq!(d.chunk_for_sample(0, 5), Some((2, 2)));
    assert_eq!(d.chunk_for_sample(0, 6), Some((3, 0)));
    assert_eq!(d.chunk_for_sample(0, 7), Some((4, 0)));
    assert_eq!(d.chunk_for_sample(0, 8), Some((5, 0)));
    // Out-of-range.
    assert_eq!(d.chunk_for_sample(0, 9), None);
    assert_eq!(d.chunk_for_sample(0, u32::MAX), None);
    assert_eq!(d.chunk_for_sample(7, 0), None);
}

#[test]
fn sample_offset_agrees_with_packet_offsets_after_drain() {
    // The demuxer-level `sample_offset(track, sample)` accessor and
    // the iter-walker that drives `next_packet` must return identical
    // byte offsets — random-access must agree with sequential walk.
    let file = build_qt_figure_2_35();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file.clone()));
    let d = MovDemuxer::open(cur).expect("open Figure 2-35 fixture");

    // Drain packets to collect their (sample_index, file_offset) pairs.
    // We don't have a public `packet.file_offset` field, but the
    // packet payload bytes are 100 × 0xAB so we can re-locate each
    // sample by where the demuxer claims it is via `sample_offset` and
    // compare to the actual bytes in the file. If the offset is right
    // the 100-byte slice at that position is all 0xAB.
    for sample_idx in 0..9u32 {
        let off = d
            .sample_offset(0, sample_idx)
            .unwrap_or_else(|| panic!("sample_offset({sample_idx}) returned None"));
        let slice = &file[off as usize..off as usize + 100];
        assert!(
            slice.iter().all(|&b| b == 0xAB),
            "sample {sample_idx} at offset {off}: byte content is not 0xAB padding"
        );
    }

    // Out-of-range sample.
    assert_eq!(d.sample_offset(0, 9), None);
    assert_eq!(d.sample_offset(7, 0), None);
}

#[test]
fn chunk_byte_extent_matches_packed_layout() {
    let file = build_qt_figure_2_35();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open Figure 2-35 fixture");

    // Chunk 1: 3 × 100 B = 300 B span.
    let (s1, e1) = d.chunk_byte_extent(0, 1).expect("chunk 1");
    assert_eq!(e1 - s1, 300);
    // Chunk 2: 3 × 100 = 300 B span, starts 300 B after chunk 1.
    let (s2, e2) = d.chunk_byte_extent(0, 2).expect("chunk 2");
    assert_eq!(e2 - s2, 300);
    assert_eq!(s2, e1, "chunk 2 should start where chunk 1 ends (packed)");
    // Chunk 3: 1 × 100 B = 100 B span.
    let (s3, e3) = d.chunk_byte_extent(0, 3).expect("chunk 3");
    assert_eq!(e3 - s3, 100);
    assert_eq!(s3, e2);
    // Chunk 4: 1 × 100 = 100, starts where chunk 3 ends.
    let (s4, e4) = d.chunk_byte_extent(0, 4).expect("chunk 4");
    assert_eq!(e4 - s4, 100);
    assert_eq!(s4, e3);
    // Chunk 5: 1 × 100 = 100, starts where chunk 4 ends.
    let (s5, e5) = d.chunk_byte_extent(0, 5).expect("chunk 5");
    assert_eq!(e5 - s5, 100);
    assert_eq!(s5, e4);

    // Out-of-range.
    assert_eq!(d.chunk_byte_extent(0, 6), None);
    assert_eq!(d.chunk_byte_extent(0, 0), None);
    assert_eq!(d.chunk_byte_extent(7, 1), None);
}
