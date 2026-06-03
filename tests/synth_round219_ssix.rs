//! Round 219 — Subsegment Index Box (`ssix`) decode.
//!
//! Exercises the file-level `ssix` surface (ISO/IEC 14496-12 §8.16.4)
//! against a hand-built ISO BMFF file whose top-level carries a
//! `sidx` + `ssix` pair. §8.16.4.1 binds each `ssix` to the
//! immediately preceding `sidx` (`Quantity: 0 or 1` per leaf-indexing
//! `sidx`); the demuxer records that pairing at parse time and
//! surfaces it via [`MovDemuxer::ssix_for_sidx`].
//!
//! These tests open via `MovDemuxer` and verify:
//! * the parsed `Ssix` exposes the subsegment / range loops byte-for-
//!   byte from §8.16.4.2;
//! * `sidx` → `ssix` cross-reference resolution works through the
//!   `ssix_for_sidx(sidx_index)` accessor;
//! * an orphan `ssix` (not immediately preceded by `sidx`) is still
//!   surfaced through the public Vec but is NOT bound to any sidx
//!   (the §8.16.4.1 pairing rule);
//! * a file without `ssix` reports an empty list and `None` from the
//!   accessor;
//! * the 24-bit `range_size` field round-trips at maximum width
//!   (`0x00FF_FFFF`);
//! * the §8.16.4.1 `range_count >= 2` rule is enforced;
//! * `Ssix::total_size_for` sums `range_size` across each subsegment;
//! * `Ssix::partial_subsegment_offset` walks the `range_size` chain
//!   from a caller-supplied subsegment start (which would be sourced
//!   from the paired `sidx`'s `subsegment_offset`).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a v0 `sidx` payload with `reference_count` v0 references all
/// pointing at media (`reference_type = 0`), with a single
/// `subsegment_duration` and a fixed SAP-type-1 / starts-with-SAP=1
/// for each.
fn build_sidx_v0_payload(
    reference_id: u32,
    timescale: u32,
    earliest_presentation_time: u32,
    first_offset: u32,
    references: &[(u32, u32)], // (referenced_size, subsegment_duration)
) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0); // version
    p.extend_from_slice(&[0, 0, 0]); // flags
    p.extend_from_slice(&reference_id.to_be_bytes());
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&earliest_presentation_time.to_be_bytes());
    p.extend_from_slice(&first_offset.to_be_bytes());
    p.extend_from_slice(&[0, 0]); // reserved
    p.extend_from_slice(&(references.len() as u16).to_be_bytes());
    for (sz, dur) in references {
        // word0: reference_type=0 (top bit) | referenced_size (31 bits)
        let w0: u32 = sz & 0x7FFF_FFFF;
        p.extend_from_slice(&w0.to_be_bytes());
        p.extend_from_slice(&dur.to_be_bytes());
        // word2: starts_with_sap=1 | sap_type=1 | sap_delta_time=0
        let w2: u32 = (1 << 31) | (1 << 28);
        p.extend_from_slice(&w2.to_be_bytes());
    }
    p
}

/// Build an `ssix` body (FullBox header + subsegment list) from a list
/// of `(level, range_size)` rows per subsegment.
fn build_ssix_payload(subsegments: &[Vec<(u8, u32)>]) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0); // version
    p.extend_from_slice(&[0, 0, 0]); // flags
    p.extend_from_slice(&(subsegments.len() as u32).to_be_bytes());
    for sub in subsegments {
        p.extend_from_slice(&(sub.len() as u32).to_be_bytes());
        for (level, range_size) in sub {
            assert!(*range_size < (1 << 24), "range_size must fit in 24 bits");
            p.push(*level);
            p.push((range_size >> 16) as u8);
            p.push((range_size >> 8) as u8);
            p.push(*range_size as u8);
        }
    }
    p
}

/// Build a minimal one-video-track ISO BMFF file whose top-level
/// carries the supplied `top_level_boxes` (each `(fourcc, payload)`)
/// between `ftyp` and `moov`. The boxes appear in the order supplied
/// — sidx/ssix pairing relies on adjacency per §8.16.4.1.
fn build_isobmff_with_top_boxes(top_level_boxes: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        push_atom(&mut out, *b"ftyp", &ftyp);

        for (fourcc, body) in top_level_boxes {
            push_atom(&mut out, **fourcc, body);
        }

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
fn ssix_paired_with_preceding_sidx_resolves_through_accessor() {
    // sidx with 2 references — `ssix` describes those 2 subsegments.
    let sidx = build_sidx_v0_payload(1, 90_000, 0, 0, &[(4096, 30_000), (8192, 30_000)]);
    let ssix = build_ssix_payload(&[
        vec![(0, 1000), (1, 3096)],            // subseg 0: 4096 bytes total
        vec![(0, 2000), (1, 3000), (2, 3192)], // subseg 1: 8192 bytes total
    ]);

    let file = build_isobmff_with_top_boxes(&[(b"sidx", &sidx), (b"ssix", &ssix)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open sidx+ssix fixture");

    assert_eq!(d.sidx.len(), 1);
    assert_eq!(d.ssix.len(), 1);

    let bound = d.ssix_for_sidx(0).expect("sidx[0] paired with ssix[0]");
    assert_eq!(bound.subsegment_count(), 2);
    assert_eq!(bound.subsegments[0].ranges.len(), 2);
    assert_eq!(bound.subsegments[1].ranges.len(), 3);
    assert_eq!(bound.subsegments[0].ranges[0].level, 0);
    assert_eq!(bound.subsegments[0].ranges[0].range_size, 1000);
    assert_eq!(bound.subsegments[1].ranges[2].range_size, 3192);

    // Cross-check: total_size_for matches the paired sidx's
    // referenced_size, the §8.16.4.1 "each byte assigned to a level"
    // invariant.
    assert_eq!(bound.total_size_for(0), Some(4096));
    assert_eq!(bound.total_size_for(1), Some(8192));
    // Out-of-range index → None.
    assert_eq!(bound.total_size_for(2), None);
}

#[test]
fn orphan_ssix_surfaces_but_does_not_bind_to_any_sidx() {
    // ssix BEFORE sidx — out of order per §8.16.4.1 ("the next box
    // after the associated Segment Index box"). The demuxer must
    // still parse it (so callers can spot a malformed writer) but
    // must NOT bind it to the trailing sidx.
    let ssix = build_ssix_payload(&[vec![(0, 100), (1, 200)]]);
    let sidx = build_sidx_v0_payload(1, 90_000, 0, 0, &[(4096, 30_000)]);

    let file = build_isobmff_with_top_boxes(&[(b"ssix", &ssix), (b"sidx", &sidx)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open orphan ssix fixture");

    assert_eq!(d.sidx.len(), 1);
    assert_eq!(
        d.ssix.len(),
        1,
        "orphan ssix is still surfaced through public Vec"
    );
    assert!(
        d.ssix_for_sidx(0).is_none(),
        "sidx[0] is not bound to the out-of-order ssix"
    );
}

#[test]
fn ssix_with_other_box_between_sidx_breaks_pairing() {
    // sidx → free → ssix. §8.16.4.1 requires ssix to be THE NEXT box;
    // a `free` between them breaks the binding.
    let sidx = build_sidx_v0_payload(1, 90_000, 0, 0, &[(4096, 30_000)]);
    let ssix = build_ssix_payload(&[vec![(0, 1000), (1, 3096)]]);
    let free_body = [0u8; 4]; // arbitrary content

    let file =
        build_isobmff_with_top_boxes(&[(b"sidx", &sidx), (b"free", &free_body), (b"ssix", &ssix)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open sidx/free/ssix fixture");

    assert_eq!(d.sidx.len(), 1);
    assert_eq!(d.ssix.len(), 1);
    assert!(
        d.ssix_for_sidx(0).is_none(),
        "intervening `free` breaks the §8.16.4.1 pairing"
    );
}

#[test]
fn file_without_ssix_reports_empty_vec() {
    let sidx = build_sidx_v0_payload(1, 90_000, 0, 0, &[(4096, 30_000)]);
    let file = build_isobmff_with_top_boxes(&[(b"sidx", &sidx)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open sidx-only fixture");

    assert_eq!(d.sidx.len(), 1);
    assert!(d.ssix.is_empty());
    // Existing sidx slot reports None (no pairing emitted).
    assert!(d.ssix_for_sidx(0).is_none());
    // Out-of-range sidx_index also reports None.
    assert!(d.ssix_for_sidx(5).is_none());
}

#[test]
fn ssix_round_trips_24bit_max_range_size_via_demuxer() {
    let max_range = (1u32 << 24) - 1;
    let sidx = build_sidx_v0_payload(1, 90_000, 0, 0, &[(max_range, 30_000)]);
    let ssix = build_ssix_payload(&[vec![(0, max_range - 1), (1, 1)]]);
    let file = build_isobmff_with_top_boxes(&[(b"sidx", &sidx), (b"ssix", &ssix)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open max-width ssix fixture");

    let bound = d.ssix_for_sidx(0).expect("sidx[0] paired");
    assert_eq!(bound.subsegments[0].ranges[0].range_size, max_range - 1);
    assert_eq!(bound.subsegments[0].ranges[1].range_size, 1);
}

#[test]
fn ssix_partial_subsegment_offset_walks_range_chain() {
    // Use a single ssix-only fixture; the math doesn't depend on the
    // paired sidx because the accessor takes an explicit
    // subsegment_start anchor.
    let sidx = build_sidx_v0_payload(1, 90_000, 0, 0, &[(6000, 30_000)]);
    let ssix = build_ssix_payload(&[vec![(0, 1000), (1, 2000), (2, 3000)]]);
    let file = build_isobmff_with_top_boxes(&[(b"sidx", &sidx), (b"ssix", &ssix)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open offset-chain ssix fixture");

    let bound = d.ssix_for_sidx(0).expect("sidx[0] paired");
    // anchor = 10_000.
    assert_eq!(bound.partial_subsegment_offset(10_000, 0, 0), Some(10_000));
    assert_eq!(bound.partial_subsegment_offset(10_000, 0, 1), Some(11_000));
    assert_eq!(bound.partial_subsegment_offset(10_000, 0, 2), Some(13_000));
    assert_eq!(bound.partial_subsegment_offset(10_000, 0, 3), None);
}

#[test]
fn ssix_range_count_below_two_rejected_at_open_time() {
    // §8.16.4.1 — range_count must be >= 2. A malformed writer that
    // emits a single-range subsegment violates the "each byte
    // assigned to a level" invariant; the parse must reject at open
    // time so an invalid box can never silently disappear.
    let sidx = build_sidx_v0_payload(1, 90_000, 0, 0, &[(4096, 30_000)]);
    // Hand-build a one-subsegment, one-range ssix payload.
    let mut bad_ssix = Vec::new();
    bad_ssix.push(0); // version
    bad_ssix.extend_from_slice(&[0, 0, 0]); // flags
    bad_ssix.extend_from_slice(&1u32.to_be_bytes()); // subsegment_count
    bad_ssix.extend_from_slice(&1u32.to_be_bytes()); // range_count = 1 (illegal)
    bad_ssix.extend_from_slice(&[0, 0, 0, 100]); // single (level, range_size)

    let file = build_isobmff_with_top_boxes(&[(b"sidx", &sidx), (b"ssix", &bad_ssix)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "range_count < 2 must reject at open time per §8.16.4.1"
    );
}

#[test]
fn two_sidx_ssix_pairs_resolve_independently() {
    // Two sidx/ssix pairs in a row — the second pair must bind to the
    // second sidx, not the first. Models a multi-stream segment that
    // carries one (sidx, ssix) per indexed stream.
    let sidx_a = build_sidx_v0_payload(1, 90_000, 0, 0, &[(4096, 30_000)]);
    let ssix_a = build_ssix_payload(&[vec![(0, 1000), (1, 3096)]]);
    let sidx_b = build_sidx_v0_payload(2, 48_000, 0, 0, &[(2048, 1000)]);
    let ssix_b = build_ssix_payload(&[vec![(0, 500), (1, 1548)]]);

    let file = build_isobmff_with_top_boxes(&[
        (b"sidx", &sidx_a),
        (b"ssix", &ssix_a),
        (b"sidx", &sidx_b),
        (b"ssix", &ssix_b),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open multi-stream sidx/ssix fixture");

    assert_eq!(d.sidx.len(), 2);
    assert_eq!(d.ssix.len(), 2);
    assert_eq!(d.sidx[0].reference_id, 1);
    assert_eq!(d.sidx[1].reference_id, 2);

    let bound_a = d.ssix_for_sidx(0).expect("first pair binds");
    assert_eq!(bound_a.subsegments[0].ranges[0].range_size, 1000);
    let bound_b = d.ssix_for_sidx(1).expect("second pair binds");
    assert_eq!(bound_b.subsegments[0].ranges[0].range_size, 500);
}
