//! Round 150 — Sample Auxiliary Information `saiz` / `saio` at
//! `traf` (fragmented) scope.
//!
//! ISO/IEC 14496-12:2015 §8.7.8.1 / §8.7.9.1 permit both boxes in
//! either `stbl` (non-fragmented) or `traf` (fragmented). Round 147
//! wired the `stbl`-scope form into the demuxer's
//! [`oxideav_mov::MovDemuxer::sample_aux_info`] accessor; round 150
//! extends `parse_traf` and the demuxer's `moof` walker to surface
//! the `traf`-scope form via
//! [`oxideav_mov::MovDemuxer::fragment_sample_aux_info`].
//!
//! The fixture below builds a two-fragment fMP4 where each `traf`
//! carries one `saiz` + one `saio` (CMAF / CENC-style per-fragment
//! sample-auxiliary-information records) — fragment 1 with the `cenc`
//! discriminator, fragment 2 with `cbcs`. The test then asserts:
//!
//! * `fragment_sample_aux_info` returns one entry per fragment;
//! * each entry's `mfhd_sequence_number` matches the `mfhd` it came
//!   from;
//! * the per-entry `lookup(...)` accessor honours the §8.7.8.1 /
//!   §8.7.9.1 discriminator-match rule (zero discriminator only
//!   matches boxes that declared no on-disk discriminator).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{
    MovDemuxer, TFHD_DEFAULT_BASE_IS_MOOF, TFHD_DEFAULT_SAMPLE_DURATION_PRESENT,
    TFHD_DEFAULT_SAMPLE_SIZE_PRESENT, TRUN_DATA_OFFSET_PRESENT, TRUN_FIRST_SAMPLE_FLAGS_PRESENT,
    TRUN_SAMPLE_SIZE_PRESENT,
};

// ─────────────── builders (kept local: traf-scope shape) ───────────────

fn build_trex(track_id: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&track_id.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes()); // default sample_description_index
    p.extend_from_slice(&100u32.to_be_bytes()); // default duration
    p.extend_from_slice(&0u32.to_be_bytes()); // default size
    p.extend_from_slice(&0u32.to_be_bytes()); // default flags (sync)
    p
}

fn build_tfhd(track_id: u32, default_dur: u32) -> Vec<u8> {
    let flags = TFHD_DEFAULT_BASE_IS_MOOF
        | TFHD_DEFAULT_SAMPLE_DURATION_PRESENT
        | TFHD_DEFAULT_SAMPLE_SIZE_PRESENT;
    let mut p = Vec::new();
    p.extend_from_slice(&flags.to_be_bytes());
    p.extend_from_slice(&track_id.to_be_bytes());
    p.extend_from_slice(&default_dur.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes()); // default sample_size = 0 (per-trun overrides)
    p
}

fn build_trun(data_offset: i32, sizes: &[u32]) -> Vec<u8> {
    let flags =
        TRUN_DATA_OFFSET_PRESENT | TRUN_FIRST_SAMPLE_FLAGS_PRESENT | TRUN_SAMPLE_SIZE_PRESENT;
    let mut p = Vec::new();
    p.extend_from_slice(&flags.to_be_bytes());
    p.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
    p.extend_from_slice(&data_offset.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes()); // first_sample_flags = sync
    for sz in sizes {
        p.extend_from_slice(&sz.to_be_bytes());
    }
    p
}

fn build_mfhd(sequence_number: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&sequence_number.to_be_bytes());
    p
}

fn build_empty_table_payloads() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut stts = Vec::new();
    stts.extend_from_slice(&0u32.to_be_bytes());
    stts.extend_from_slice(&0u32.to_be_bytes());
    let mut stsc = Vec::new();
    stsc.extend_from_slice(&0u32.to_be_bytes());
    stsc.extend_from_slice(&0u32.to_be_bytes());
    let mut stsz = Vec::new();
    stsz.extend_from_slice(&0u32.to_be_bytes());
    stsz.extend_from_slice(&0u32.to_be_bytes());
    stsz.extend_from_slice(&0u32.to_be_bytes());
    let mut stco = Vec::new();
    stco.extend_from_slice(&0u32.to_be_bytes());
    stco.extend_from_slice(&0u32.to_be_bytes());
    (stts, stsc, stsz, stco)
}

/// Build a `saiz` body with §8.7.8.2's default-size shape (one byte
/// per sample inline). `aux` is the optional discriminator pair.
fn build_saiz(
    aux: Option<(&[u8; 4], u32)>,
    default_size: u8,
    sample_count: u32,
    per_sample_sizes: &[u8],
) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0); // version
    let flags: u32 = if aux.is_some() { 1 } else { 0 };
    let f = flags.to_be_bytes();
    p.extend_from_slice(&f[1..4]);
    if let Some((t, par)) = aux {
        p.extend_from_slice(t);
        p.extend_from_slice(&par.to_be_bytes());
    }
    p.push(default_size);
    p.extend_from_slice(&sample_count.to_be_bytes());
    if default_size == 0 {
        p.extend_from_slice(per_sample_sizes);
    }
    p
}

/// Build a `saio` body. `version == 0` means 32-bit offsets, `1`
/// means 64-bit.
fn build_saio(version: u8, aux: Option<(&[u8; 4], u32)>, offsets: &[u64]) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(version);
    let flags: u32 = if aux.is_some() { 1 } else { 0 };
    let f = flags.to_be_bytes();
    p.extend_from_slice(&f[1..4]);
    if let Some((t, par)) = aux {
        p.extend_from_slice(t);
        p.extend_from_slice(&par.to_be_bytes());
    }
    p.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
    for &o in offsets {
        if version == 0 {
            p.extend_from_slice(&(o as u32).to_be_bytes());
        } else {
            p.extend_from_slice(&o.to_be_bytes());
        }
    }
    p
}

/// Build a two-fragment fMP4 where each `traf` carries `saiz` +
/// `saio` (CMAF / CENC shape: one box per fragment naming the bytes
/// of auxiliary information that fragment carries).
///
/// Fragment 1's discriminator is `cenc`, fragment 2's is `cbcs`.
/// Both fragments hold 3 samples of 64 bytes each.
fn build_two_fragment_with_traf_sample_aux() -> Vec<u8> {
    let mut out = Vec::new();

    // ftyp — iso5 so default-base-is-moof is unambiguous.
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(b"mp42");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // moov with mvex/trex + empty-stbl trak.
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 0, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 0));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    let (stts, stsc, stsz, stco) = build_empty_table_payloads();
    push_atom(&mut stbl, *b"stts", &stts);
    push_atom(&mut stbl, *b"stsc", &stsc);
    push_atom(&mut stbl, *b"stsz", &stsz);
    push_atom(&mut stbl, *b"stco", &stco);
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    let mut mvex = Vec::new();
    push_atom(&mut mvex, *b"trex", &build_trex(1));
    push_atom(&mut moov, *b"mvex", &mvex);
    push_atom(&mut out, *b"moov", &moov);

    // ── Fragment 1: moof[seq=1] + mdat (3 × 64 bytes) ──
    let sizes1: Vec<u32> = vec![64, 64, 64];

    // Per-fragment saiz: default_size=16 (CENC IV-length), 3 samples.
    let saiz1 = build_saiz(Some((b"cenc", 0)), 16, sizes1.len() as u32, &[]);
    // Per-fragment saio: single offset (whole-traf contiguous block).
    let saio1 = build_saio(0, Some((b"cenc", 0)), &[0x1000]);

    let tfhd1 = build_tfhd(1, 100);
    // Compute moof_size with placeholder data_offset.
    let trun_payload_len = 4 + 4 + 4 + 4 + (sizes1.len() * 4);
    let traf_payload_len =
        (8 + tfhd1.len()) + (8 + saiz1.len()) + (8 + saio1.len()) + (8 + trun_payload_len);
    let moof_payload_len = 8 + 8 /* mfhd */ + 8 + traf_payload_len /* traf hdr+body */;
    let moof_size = 8 + moof_payload_len as u64;
    let data_offset = (moof_size + 8) as i32; // mdat header is 8 bytes
    let trun1 = build_trun(data_offset, &sizes1);

    let mut traf1 = Vec::new();
    push_atom(&mut traf1, *b"tfhd", &tfhd1);
    // §8.7.8.1 / §8.7.9.1 allow `saiz` / `saio` in either stbl or
    // traf scope; spec doesn't fix child order inside traf, so we
    // place them between tfhd and trun for clarity.
    push_atom(&mut traf1, *b"saiz", &saiz1);
    push_atom(&mut traf1, *b"saio", &saio1);
    push_atom(&mut traf1, *b"trun", &trun1);

    let mut moof1 = Vec::new();
    push_atom(&mut moof1, *b"mfhd", &build_mfhd(1));
    push_atom(&mut moof1, *b"traf", &traf1);
    assert_eq!(
        (8 + moof1.len()) as u64,
        moof_size,
        "moof1 size estimate must match"
    );
    push_atom(&mut out, *b"moof", &moof1);

    let mut mdat1 = Vec::new();
    for (i, &sz) in sizes1.iter().enumerate() {
        mdat1.extend(std::iter::repeat(b'A' + i as u8).take(sz as usize));
    }
    push_atom(&mut out, *b"mdat", &mdat1);

    // ── Fragment 2: moof[seq=2] + mdat (3 × 64 bytes) ──
    let sizes2: Vec<u32> = vec![64, 64, 64];

    let saiz2 = build_saiz(Some((b"cbcs", 0)), 16, sizes2.len() as u32, &[]);
    let saio2 = build_saio(0, Some((b"cbcs", 0)), &[0x2000]);

    let tfhd2 = build_tfhd(1, 100);
    let trun_payload_len2 = 4 + 4 + 4 + 4 + (sizes2.len() * 4);
    let traf_payload_len2 =
        (8 + tfhd2.len()) + (8 + saiz2.len()) + (8 + saio2.len()) + (8 + trun_payload_len2);
    let moof_payload_len2 = 8 + 8 + 8 + traf_payload_len2;
    let moof_size2 = 8 + moof_payload_len2 as u64;
    let data_offset2 = (moof_size2 + 8) as i32;
    let trun2 = build_trun(data_offset2, &sizes2);

    let mut traf2 = Vec::new();
    push_atom(&mut traf2, *b"tfhd", &tfhd2);
    push_atom(&mut traf2, *b"saiz", &saiz2);
    push_atom(&mut traf2, *b"saio", &saio2);
    push_atom(&mut traf2, *b"trun", &trun2);

    let mut moof2 = Vec::new();
    push_atom(&mut moof2, *b"mfhd", &build_mfhd(2));
    push_atom(&mut moof2, *b"traf", &traf2);
    assert_eq!(
        (8 + moof2.len()) as u64,
        moof_size2,
        "moof2 size estimate must match"
    );
    push_atom(&mut out, *b"moof", &moof2);

    let mut mdat2 = Vec::new();
    for (i, &sz) in sizes2.iter().enumerate() {
        mdat2.extend(std::iter::repeat(b'X' + i as u8).take(sz as usize));
    }
    push_atom(&mut out, *b"mdat", &mdat2);

    out
}

#[test]
fn fragment_sample_aux_records_one_entry_per_fragment() {
    let bytes = build_two_fragment_with_traf_sample_aux();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open two-fragment fmp4 with traf-scope sample-aux");

    assert!(d.is_fragmented());
    assert_eq!(d.fragment_sequence_numbers, vec![1, 2]);

    let aux = d.fragment_sample_aux_info(0);
    assert_eq!(aux.len(), 2, "one FragmentSampleAux entry per fragment");
    assert_eq!(aux[0].mfhd_sequence_number, 1);
    assert_eq!(aux[0].track_id, 1);
    assert_eq!(aux[1].mfhd_sequence_number, 2);
    assert_eq!(aux[1].track_id, 1);
}

#[test]
fn fragment_sample_aux_lookup_honours_discriminator() {
    let bytes = build_two_fragment_with_traf_sample_aux();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open fragmented");
    let aux = d.fragment_sample_aux_info(0);
    assert_eq!(aux.len(), 2);

    // Fragment 1 declared `(cenc, 0)`; cbcs should miss.
    let (s1, o1) = aux[0].lookup(b"cenc", 0);
    assert!(s1.is_some());
    assert!(o1.is_some());
    let s1 = s1.unwrap();
    let o1 = o1.unwrap();
    assert_eq!(s1.default_sample_info_size, 16);
    assert_eq!(s1.sample_count, 3);
    assert_eq!(s1.total_size(), 16 * 3);
    assert_eq!(o1.offsets, vec![0x1000]);
    let (s_miss, o_miss) = aux[0].lookup(b"cbcs", 0);
    assert!(s_miss.is_none());
    assert!(o_miss.is_none());

    // Fragment 2 declared `(cbcs, 0)`.
    let (s2, o2) = aux[1].lookup(b"cbcs", 0);
    assert!(s2.is_some() && o2.is_some());
    let o2 = o2.unwrap();
    assert_eq!(o2.offsets, vec![0x2000]);
    let (s_miss2, _) = aux[1].lookup(b"cenc", 0);
    assert!(s_miss2.is_none());
}

#[test]
fn fragment_sample_aux_empty_for_out_of_range_track() {
    let bytes = build_two_fragment_with_traf_sample_aux();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open fragmented");
    assert!(d.fragment_sample_aux_info(99).is_empty());
}

/// A fragmented file with **no** `traf`-scope sample-aux boxes: the
/// per-track list stays empty (the round-18 baseline behaviour is
/// preserved when nothing changed).
#[test]
fn fragment_sample_aux_empty_when_no_traf_boxes() {
    // Build a one-fragment file using the same scaffolding but with
    // no `saiz` / `saio` inside `traf`.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(b"mp42");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 0, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 0));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    let (stts, stsc, stsz, stco) = build_empty_table_payloads();
    push_atom(&mut stbl, *b"stts", &stts);
    push_atom(&mut stbl, *b"stsc", &stsc);
    push_atom(&mut stbl, *b"stsz", &stsz);
    push_atom(&mut stbl, *b"stco", &stco);
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    let mut mvex = Vec::new();
    push_atom(&mut mvex, *b"trex", &build_trex(1));
    push_atom(&mut moov, *b"mvex", &mvex);
    push_atom(&mut out, *b"moov", &moov);

    let sizes: Vec<u32> = vec![64, 64];
    let tfhd = build_tfhd(1, 100);
    let trun_payload_len = 4 + 4 + 4 + 4 + (sizes.len() * 4);
    let traf_payload_len = (8 + tfhd.len()) + (8 + trun_payload_len);
    let moof_payload_len = 8 + 8 + 8 + traf_payload_len;
    let moof_size = 8 + moof_payload_len as u64;
    let data_offset = (moof_size + 8) as i32;
    let trun = build_trun(data_offset, &sizes);
    let mut traf = Vec::new();
    push_atom(&mut traf, *b"tfhd", &tfhd);
    push_atom(&mut traf, *b"trun", &trun);
    let mut moof = Vec::new();
    push_atom(&mut moof, *b"mfhd", &build_mfhd(1));
    push_atom(&mut moof, *b"traf", &traf);
    push_atom(&mut out, *b"moof", &moof);

    let mut mdat = Vec::new();
    for (i, &sz) in sizes.iter().enumerate() {
        mdat.extend(std::iter::repeat(b'A' + i as u8).take(sz as usize));
    }
    push_atom(&mut out, *b"mdat", &mdat);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open fragmented fmp4");
    assert!(d.is_fragmented());
    assert!(
        d.fragment_sample_aux_info(0).is_empty(),
        "no traf-scope sample-aux → empty list"
    );
}

/// `stbl`-scope sample-aux (round 147) and `traf`-scope sample-aux
/// (round 150) coexist on the same track without colliding — the
/// `stbl` form is still surfaced through `sample_aux_info` and the
/// `traf` form through `fragment_sample_aux_info`.
#[test]
fn stbl_scope_and_traf_scope_coexist_without_collision() {
    // The round-18 fragmented fixtures all have empty stbl tables
    // (the demuxer treats an init-segment moov + per-moof samples
    // as the normal fMP4 shape). With nothing in stbl, the round-147
    // accessor returns (None, None) and the round-150 accessor
    // returns the per-fragment slice — confirming neither path
    // accidentally picks up the other.
    let bytes = build_two_fragment_with_traf_sample_aux();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open fragmented");

    let (stbl_saiz, stbl_saio) = d.sample_aux_info(0, b"cenc", 0);
    assert!(
        stbl_saiz.is_none() && stbl_saio.is_none(),
        "init-segment stbl has no sample-aux"
    );

    let frag_aux = d.fragment_sample_aux_info(0);
    assert_eq!(frag_aux.len(), 2);
    assert!(frag_aux[0].lookup(b"cenc", 0).0.is_some());
    assert!(frag_aux[1].lookup(b"cbcs", 0).0.is_some());
}
