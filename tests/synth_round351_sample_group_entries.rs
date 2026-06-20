//! Round 351 — typed `sgpd` sample-group entries for the §10.2 .. §10.6
//! grouping types beyond the round-80 `roll` / `prol` / `rap ` set.
//!
//! Builds hand-authored QT files that carry the additional standardized
//! sample-group description entries from ISO/IEC 14496-12:2015 and
//! verifies the per-sample typed demuxer lookups:
//!
//! * `'tele'` (§10.5.2) — TemporalLevelEntry (level == sgpd index).
//! * `'sap '` (§10.6.2) — SAPEntry (dependent_flag + SAP_type).
//! * `'rash'` (§10.2.2.2) — RateShareEntry (operation points + bitrate
//!   trailer + discard priority).
//! * `'alst'` (§10.3.2) — AlternativeStartupEntry (offsets + pieces).
//!
//! Each test slides the grouping atoms into `stbl`, opens the file via
//! `MovDemuxer`, and checks `temporal_level_for` /
//! `stream_access_point_for` / `rate_share_for` /
//! `alternative_startup_for`.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a 4-sample video QT file with the caller-supplied sample-group
/// atoms appended at the end of `stbl`. mvhd / mdhd are 600-tick
/// timescale, 4 × 30 = 120 ticks total.
fn build_video_qt(sbgp_atoms: &[Vec<u8>], sgpd_atoms: &[Vec<u8>]) -> Vec<u8> {
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
        for payload in sbgp_atoms {
            push_atom(&mut stbl, *b"sbgp", payload);
        }
        for payload in sgpd_atoms {
            push_atom(&mut stbl, *b"sgpd", payload);
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
    let mdat_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    build_file(mdat_pos + 4)
}

/// v0 `sbgp`: `[ver+flags][grouping_type][entry_count]
/// entry_count × (sample_count, group_description_index)`.
fn build_sbgp_v0(grouping_type: &[u8; 4], runs: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(grouping_type);
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, idx) in runs {
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&idx.to_be_bytes());
    }
    p
}

/// v1 `sgpd` with a non-zero `default_length` (every entry that wide).
fn build_sgpd_v1_fixed(
    grouping_type: &[u8; 4],
    default_length: u32,
    entries: &[Vec<u8>],
) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(1u8);
    p.extend_from_slice(&[0, 0, 0]);
    p.extend_from_slice(grouping_type);
    p.extend_from_slice(&default_length.to_be_bytes());
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for e in entries {
        assert_eq!(
            e.len() as u32,
            default_length,
            "fixture entry width mismatch"
        );
        p.extend_from_slice(e);
    }
    p
}

/// v1 `sgpd` with `default_length == 0` so each entry carries its own
/// `description_length:u32` prefix (the variable-length path).
fn build_sgpd_v1_varlen(grouping_type: &[u8; 4], entries: &[Vec<u8>]) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(1u8);
    p.extend_from_slice(&[0, 0, 0]);
    p.extend_from_slice(grouping_type);
    p.extend_from_slice(&0u32.to_be_bytes()); // default_length == 0
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for e in entries {
        p.extend_from_slice(&(e.len() as u32).to_be_bytes());
        p.extend_from_slice(e);
    }
    p
}

#[test]
fn temporal_level_resolves_per_run_index() {
    // Two-level temporal layering: samples 0-1 → level 1 (independently
    // decodable), samples 2-3 → level 2 (no info).
    let sbgp = build_sbgp_v0(b"tele", &[(2, 1), (2, 2)]);
    let sgpd = build_sgpd_v1_fixed(b"tele", 1, &[vec![0x80], vec![0x00]]);
    let bytes = build_video_qt(&[sbgp], &[sgpd]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open tele fixture");

    assert_eq!(d.temporal_level_for(0, 0), Some((1, true)));
    assert_eq!(d.temporal_level_for(0, 1), Some((1, true)));
    assert_eq!(d.temporal_level_for(0, 2), Some((2, false)));
    assert_eq!(d.temporal_level_for(0, 3), Some((2, false)));
    // Outside the grouping → None.
    assert_eq!(d.temporal_level_for(0, 99), None);
    // Other groupings absent on this fixture.
    assert_eq!(d.stream_access_point_for(0, 0), None);
}

#[test]
fn stream_access_point_decodes_dependent_and_type() {
    // sap : sample 0 is a dependent SAP type 3; rest are non-members.
    let sbgp = build_sbgp_v0(b"sap ", &[(1, 1), (3, 0)]);
    // 0x83 → dependent=1, SAP_type=3.
    let sgpd = build_sgpd_v1_fixed(b"sap ", 1, &[vec![0x83]]);
    let bytes = build_video_qt(&[sbgp], &[sgpd]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open sap fixture");

    let sap = d.stream_access_point_for(0, 0).expect("sample 0 SAP");
    assert!(sap.dependent);
    assert_eq!(sap.sap_type, 3);
    // Samples 1-3 map to group index 0 → not a member → None.
    assert_eq!(d.stream_access_point_for(0, 1), None);
    assert_eq!(d.stream_access_point_for(0, 3), None);
}

#[test]
fn rate_share_multi_operation_point_round_trips() {
    // All 4 samples share one rate-share record with two operation
    // points (§10.2.2.2).
    let sbgp = build_sbgp_v0(b"rash", &[(4, 1)]);
    let mut entry = Vec::new();
    entry.extend_from_slice(&2u16.to_be_bytes()); // operation_point_count
    entry.extend_from_slice(&1000u32.to_be_bytes()); // pt0 available_bitrate
    entry.extend_from_slice(&3000u16.to_be_bytes()); // pt0 target_rate_share
    entry.extend_from_slice(&4000u32.to_be_bytes()); // pt1 available_bitrate
    entry.extend_from_slice(&6000u16.to_be_bytes()); // pt1 target_rate_share
    entry.extend_from_slice(&5000u32.to_be_bytes()); // maximum_bitrate
    entry.extend_from_slice(&800u32.to_be_bytes()); // minimum_bitrate
    entry.push(200); // discard_priority
    let sgpd = build_sgpd_v1_varlen(b"rash", &[entry]);
    let bytes = build_video_qt(&[sbgp], &[sgpd]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open rash fixture");

    let rash = d.rate_share_for(0, 2).expect("sample 2 rate share");
    assert_eq!(rash.operation_points.len(), 2);
    assert_eq!(rash.operation_points[0].available_bitrate, 1000);
    assert_eq!(rash.operation_points[0].target_rate_share, 3000);
    assert_eq!(rash.operation_points[1].available_bitrate, 4000);
    assert_eq!(rash.operation_points[1].target_rate_share, 6000);
    assert_eq!(rash.maximum_bitrate, 5000);
    assert_eq!(rash.minimum_bitrate, 800);
    assert_eq!(rash.discard_priority, 200);
    assert_eq!(d.rate_share_for(0, 999), None);
}

#[test]
fn alternative_startup_offsets_and_pieces_round_trip() {
    // All 4 samples map to one alternative-startup entry with two
    // offsets and one output-rate piece (§10.3.2).
    let sbgp = build_sbgp_v0(b"alst", &[(4, 1)]);
    let mut entry = Vec::new();
    entry.extend_from_slice(&2u16.to_be_bytes()); // roll_count
    entry.extend_from_slice(&1u16.to_be_bytes()); // first_output_sample
    entry.extend_from_slice(&10u32.to_be_bytes()); // sample_offset[1]
    entry.extend_from_slice(&20u32.to_be_bytes()); // sample_offset[2]
    entry.extend_from_slice(&2u16.to_be_bytes()); // piece0 num_output_samples
    entry.extend_from_slice(&4u16.to_be_bytes()); // piece0 num_total_samples
    let sgpd = build_sgpd_v1_varlen(b"alst", &[entry]);
    let bytes = build_video_qt(&[sbgp], &[sgpd]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open alst fixture");

    let a = d.alternative_startup_for(0, 0).expect("sample 0 alst");
    assert_eq!(a.roll_count, 2);
    assert_eq!(a.first_output_sample, 1);
    assert_eq!(a.sample_offsets, vec![10, 20]);
    assert_eq!(a.pieces.len(), 1);
    assert_eq!(a.pieces[0].num_output_samples, 2);
    assert_eq!(a.pieces[0].num_total_samples, 4);
    assert_eq!(d.alternative_startup_for(0, 999), None);
}

#[test]
fn multiple_grouping_types_coexist_in_one_stbl() {
    // A single track carrying both 'tele' and 'sap ' groupings — the
    // demuxer must resolve each independently by grouping_type.
    let tele_sbgp = build_sbgp_v0(b"tele", &[(4, 1)]);
    let tele_sgpd = build_sgpd_v1_fixed(b"tele", 1, &[vec![0x80]]);
    let sap_sbgp = build_sbgp_v0(b"sap ", &[(4, 1)]);
    let sap_sgpd = build_sgpd_v1_fixed(b"sap ", 1, &[vec![0x01]]);
    let bytes = build_video_qt(&[tele_sbgp, sap_sbgp], &[tele_sgpd, sap_sgpd]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open mixed-grouping fixture");

    assert_eq!(d.temporal_level_for(0, 3), Some((1, true)));
    let sap = d.stream_access_point_for(0, 3).expect("sample 3 SAP");
    assert!(!sap.dependent);
    assert_eq!(sap.sap_type, 1);
}
