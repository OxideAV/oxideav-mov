//! Round 226 — Level Assignment Box (`leva`) decode through the
//! demuxer's `moov/mvex` walker.
//!
//! Exercises the optional `leva` surface (ISO/IEC 14496-12 §8.8.13)
//! against a hand-built ISO BMFF file whose `moov/mvex` carries
//! `mehd` + `trex` + `leva`. The box is `Quantity: Zero or one`
//! (§8.8.13.1); the round-18 demuxer already walks `mvex` for `mehd`
//! and `trex`, so this round wires the optional `leva` child into the
//! `parse_mvex` return tuple and surfaces it through
//! `MovDemuxer::leva`.
//!
//! These tests verify:
//! * a `leva` carrying a 2-row track / sample-group sequence reaches
//!   `MovDemuxer::leva` byte-for-byte from §8.8.13.2;
//! * an `mvex` without a `leva` child leaves `MovDemuxer::leva` at
//!   `None`;
//! * a malformed file emitting two `leva` boxes inside one `mvex`
//!   keeps the first (§8.8.13.1 Quantity rule + first-wins
//!   conservative-merge policy);
//! * a `leva` with `level_count < 2` is rejected at open time
//!   (§8.8.13.3);
//! * a `leva` whose row sequence violates the §8.8.13.3 ordering rule
//!   ("zero or more of type 2 or 3, followed by zero or more of
//!   exactly one type") is rejected at open time;
//! * `Leva::level()` looks up rows 1-based per spec;
//! * `Leva::track_ids()` de-duplicates first-occurrence-wins.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{AssignmentType, MovDemuxer};

/// Build a `leva` body. `rows` carries `(track_id, padding_flag,
/// assignment_type byte 0..=127, trailer bytes)` tuples in
/// declaration order. The caller is responsible for the per-type
/// trailer length: type 0 = 4-byte grouping_type, type 1 = 4+4-byte
/// grouping_type + parameter, type 2/3 = empty, type 4 = 4-byte
/// sub_track_id, reserved codes carry no spec-defined trailer.
fn build_leva_body(rows: &[(u32, bool, u8, Vec<u8>)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0u8); // version
    p.extend_from_slice(&[0u8, 0, 0]); // flags
    p.push(rows.len() as u8); // level_count
    for (tid, pad, ty, trailer) in rows {
        p.extend_from_slice(&tid.to_be_bytes());
        let mut flag_type = ty & 0x7F;
        if *pad {
            flag_type |= 0x80;
        }
        p.push(flag_type);
        p.extend_from_slice(trailer);
    }
    p
}

/// Build a minimal `mehd` body (FullBox v0, fragment_duration = 0).
fn build_mehd_body() -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0u8); // version
    p.extend_from_slice(&[0u8, 0, 0]); // flags
    p.extend_from_slice(&0u32.to_be_bytes()); // fragment_duration
    p
}

/// Build a minimal `trex` body (FullBox v0).
fn build_trex_body(track_id: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0u8); // version
    p.extend_from_slice(&[0u8, 0, 0]); // flags
    p.extend_from_slice(&track_id.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes()); // default_sample_description_index
    p.extend_from_slice(&0u32.to_be_bytes()); // default_sample_duration
    p.extend_from_slice(&0u32.to_be_bytes()); // default_sample_size
    p.extend_from_slice(&0u32.to_be_bytes()); // default_sample_flags
    p
}

/// Build a minimal one-video-track fragmented-shaped ISO BMFF file
/// whose `moov/mvex` carries the supplied list of `(fourcc, body)`
/// children. The track table is non-fragmented so the round-18 sample
/// walker does not need any `moof` to enumerate samples — the
/// caller only needs to know that `moov/mvex` is reachable.
fn build_isobmff_with_mvex_children(mvex_children: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
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
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);

        // mvex container with caller-supplied children.
        let mut mvex = Vec::new();
        for (fourcc, body) in mvex_children {
            push_atom(&mut mvex, **fourcc, body);
        }
        push_atom(&mut moov, *b"mvex", &mvex);

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
fn leva_with_two_track_levels_reaches_demuxer() {
    // Two assignment_type == 2 (Track) rows. Smallest spec-legal
    // shape: level_count == 2.
    let leva_body = build_leva_body(&[(1, false, 2, vec![]), (2, true, 2, vec![])]);
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
        (b"leva", leva_body),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open leva fixture");

    let leva = d.leva.as_ref().expect("leva populated");
    assert_eq!(leva.level_count(), 2);
    assert_eq!(leva.levels[0].track_id, 1);
    assert!(!leva.levels[0].padding_flag);
    assert_eq!(leva.levels[0].assignment_type, AssignmentType::Track);
    assert_eq!(leva.levels[1].track_id, 2);
    assert!(leva.levels[1].padding_flag);
    assert_eq!(leva.levels[1].assignment_type, AssignmentType::Track);
}

#[test]
fn mvex_without_leva_leaves_demuxer_field_none() {
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open mvex-without-leva fixture");

    assert!(d.leva.is_none());
}

#[test]
fn duplicate_leva_inside_one_mvex_keeps_first() {
    // §8.8.13.1 fixes Quantity at Zero or one; first-wins matches
    // the conservative-merge policy applied to `mehd`, `ctab`,
    // `clip`, `pdin`, and the other singletons.
    let first = build_leva_body(&[(11, false, 2, vec![]), (12, false, 2, vec![])]);
    let second = build_leva_body(&[(21, false, 2, vec![]), (22, false, 2, vec![])]);
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
        (b"leva", first),
        (b"leva", second),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open dup-leva fixture");

    let leva = d.leva.as_ref().expect("leva populated");
    assert_eq!(
        leva.levels[0].track_id, 11,
        "first-wins keeps the first leva"
    );
    assert_eq!(leva.levels[1].track_id, 12);
}

#[test]
fn leva_with_level_count_one_rejected_at_open() {
    // §8.8.13.3 spec-fixes the minimum at 2. A writer emitting a
    // single-row leva violates the rule; the parser refuses at open
    // time so callers can't silently get a one-row Leva.
    let mut bad = build_leva_body(&[(1, false, 2, vec![])]);
    // build_leva_body wrote level_count == 1 already, but explicit:
    bad[4] = 1;
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
        (b"leva", bad),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "level_count == 1 must reject at open time"
    );
}

#[test]
fn leva_ordering_violation_rejected_at_open() {
    // §8.8.13.3: "The sequence of assignment_types is restricted to
    // be a set of zero or more of type 2 or 3, followed by zero or
    // more of exactly one type." A type-2 row that follows a type-0
    // row violates the "followed by" structure.
    let bad = build_leva_body(&[(1, false, 0, b"aaaa".to_vec()), (2, false, 2, vec![])]);
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
        (b"leva", bad),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "type-2 after pinned tail must reject at open time"
    );
}

#[test]
fn leva_sample_group_assignment_round_trips_through_demuxer() {
    // Type-0 row carries a 4-byte grouping_type. Picking `roll` —
    // the §10.1.1.2 grouping the demuxer already surfaces typed
    // through the round-80 path — makes the bytes meaningful.
    let leva_body = build_leva_body(&[(1, false, 2, vec![]), (1, false, 0, b"roll".to_vec())]);
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
        (b"leva", leva_body),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open type-0 leva fixture");

    let leva = d.leva.as_ref().expect("leva populated");
    assert_eq!(
        leva.levels[1].assignment_type,
        AssignmentType::SampleGroup {
            grouping_type: *b"roll"
        }
    );
}

#[test]
fn leva_sub_track_assignment_round_trips_through_demuxer() {
    let trailer = 7u32.to_be_bytes().to_vec();
    let leva_body = build_leva_body(&[(1, false, 2, vec![]), (1, false, 4, trailer)]);
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
        (b"leva", leva_body),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open type-4 leva fixture");

    let leva = d.leva.as_ref().expect("leva populated");
    assert_eq!(
        leva.levels[1].assignment_type,
        AssignmentType::SubTrack { sub_track_id: 7 }
    );
}

#[test]
fn leva_level_accessor_is_1_based_through_demuxer() {
    let leva_body = build_leva_body(&[(11, false, 2, vec![]), (22, false, 2, vec![])]);
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
        (b"leva", leva_body),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open leva fixture");
    let leva = d.leva.as_ref().expect("leva populated");

    assert!(
        leva.level(0).is_none(),
        "level 0 is not addressable per §8.8.13.3"
    );
    assert_eq!(leva.level(1).unwrap().track_id, 11);
    assert_eq!(leva.level(2).unwrap().track_id, 22);
    assert!(leva.level(3).is_none());
}

#[test]
fn leva_track_ids_dedupes_first_occurrence_wins() {
    // Three rows, two share track_id == 5 — the helper must surface
    // { 5, 9 } in first-occurrence order.
    let leva_body = build_leva_body(&[
        (5, false, 2, vec![]),
        (9, false, 2, vec![]),
        (5, false, 2, vec![]),
    ]);
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
        (b"leva", leva_body),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open leva fixture");
    let leva = d.leva.as_ref().expect("leva populated");

    assert_eq!(leva.track_ids(), vec![5, 9]);
}

#[test]
fn unknown_leva_version_rejected_at_open() {
    let mut bad = build_leva_body(&[(1, false, 2, vec![]), (2, false, 2, vec![])]);
    bad[0] = 1; // §8.8.13.2 fixes version at 0
    let file = build_isobmff_with_mvex_children(&[
        (b"mehd", build_mehd_body()),
        (b"trex", build_trex_body(1)),
        (b"leva", bad),
    ]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "unknown leva version must reject at open time"
    );
}
