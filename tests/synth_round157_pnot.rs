//! Round 157 — Preview atom (`pnot`) decode.
//!
//! Exercises the file-level `pnot` surface (Apple QTFF, 2001-03-01,
//! pp. 26 – 27 / Figure 1-7) against a hand-built QuickTime file whose
//! top level carries a single `pnot` between `ftyp` and `moov`. The
//! atom is a Finder / Open-dialog preflight pointer at a poster image
//! (typically a `PICT`) and is QuickTime-only; ISO BMFF derivatives
//! never carry it.
//!
//! These tests open via `MovDemuxer` and verify:
//! * the parsed [`Pnot`] exposes `modification_date`, `version_number`,
//!   `atom_type`, and `atom_index` byte-for-byte;
//! * `unix_seconds()` converts the Mac-classic timestamp to a Unix-epoch
//!   second count (QTFF p. 32 — `mvhd` shares the same epoch);
//! * a file without `pnot` surfaces `None`;
//! * a malformed body (wrong length) is rejected at open time so a
//!   half-atom can't silently disappear;
//! * a writer that emits two `pnot` atoms (spec is silent on the case)
//!   collapses to the first per the `pdin` / `ctab` convention.
//!
//! Each fixture also runs `mdat` after `moov` so the `stco` chunk-offset
//! survives the `pnot` insertion without re-locating.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, MAC_TO_UNIX_EPOCH_SECONDS, PNOT_BODY_LEN};

/// Build a `pnot` body matching QTFF Figure 1-7 byte-for-byte.
fn build_pnot_payload(
    modification_date: u32,
    version_number: u16,
    atom_type: [u8; 4],
    atom_index: u16,
) -> Vec<u8> {
    let mut p = Vec::with_capacity(PNOT_BODY_LEN);
    p.extend_from_slice(&modification_date.to_be_bytes());
    p.extend_from_slice(&version_number.to_be_bytes());
    p.extend_from_slice(&atom_type);
    p.extend_from_slice(&atom_index.to_be_bytes());
    p
}

/// Build a minimal one-video-track QuickTime file with `ftyp`, an
/// optional list of file-level `pnot` payloads, `moov`, and `mdat`.
/// The `pnot` atoms are emitted between `ftyp` and `moov` — exactly
/// where QTFF p. 26 places them in the top-level atom stream.
fn build_qt_with_pnot(pnot_payloads: &[Vec<u8>]) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  ");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        for payload in pnot_payloads {
            push_atom(&mut out, *b"pnot", payload);
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
fn pnot_single_box_parses_byte_exact() {
    // Mac-classic seconds for 2024-01-01T00:00:00Z:
    //   unix_secs = 1_704_067_200
    //   mac_secs  = 1_704_067_200 + 2_082_844_800 = 3_786_912_000
    let mod_date = 3_786_912_000u32;
    let payload = build_pnot_payload(mod_date, 0, *b"PICT", 1);
    let file = build_qt_with_pnot(&[payload]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open pnot fixture");

    let pnot = d.pnot.expect("pnot must surface");
    assert_eq!(pnot.modification_date, mod_date);
    assert_eq!(pnot.version_number, 0);
    assert_eq!(pnot.atom_type, *b"PICT");
    assert_eq!(pnot.atom_index, 1);
    assert!(pnot.is_known_version());
    assert!(pnot.is_valid_index());
    assert_eq!(pnot.unix_seconds(), Some(1_704_067_200));
}

#[test]
fn pnot_absent_yields_none() {
    let file = build_qt_with_pnot(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open file with no pnot");
    assert!(d.pnot.is_none(), "no pnot must surface as None");
}

#[test]
fn pnot_duplicate_keeps_first() {
    // Spec doesn't define an override policy; the parser keeps the
    // first per the `pdin` / `ctab` conservative-merge convention.
    let first = build_pnot_payload(MAC_TO_UNIX_EPOCH_SECONDS as u32, 0, *b"PICT", 1);
    let second = build_pnot_payload(0, 0, *b"jpeg", 2);
    let file = build_qt_with_pnot(&[first, second]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open duplicate-pnot fixture");

    let pnot = d.pnot.expect("first pnot wins");
    assert_eq!(pnot.atom_type, *b"PICT");
    assert_eq!(pnot.atom_index, 1);
    assert_eq!(pnot.unix_seconds(), Some(0));
}

#[test]
fn pnot_truncated_body_rejects_at_open_time() {
    // Hand-craft a file whose `pnot` body is 11 bytes — one short of
    // the spec-fixed 12-byte record. The open path must reject so a
    // half-atom can't be silently dropped.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"pnot", &[0u8; PNOT_BODY_LEN - 1]);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "a truncated pnot body must be rejected at open time"
    );
}

#[test]
fn pnot_trailing_bytes_reject_at_open_time() {
    // `pnot` carries no list — any extra byte past the fixed record is
    // a writer error and must reject.
    let mut payload = build_pnot_payload(MAC_TO_UNIX_EPOCH_SECONDS as u32, 0, *b"PICT", 1);
    payload.push(0xAA);

    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"pnot", &payload);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "trailing bytes past the pnot record must be rejected at open time"
    );
}

#[test]
fn pnot_unknown_version_still_opens_but_predicate_flags_it() {
    // QTFF p. 26 fixes `version_number` at 0, but the parser stays
    // accepting so a writer that sets a stray value doesn't lose the
    // other useful fields. The conformance signal travels via
    // `is_known_version()`.
    let payload = build_pnot_payload(MAC_TO_UNIX_EPOCH_SECONDS as u32, 1, *b"PICT", 1);
    let file = build_qt_with_pnot(&[payload]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open pnot fixture with non-zero version");

    let pnot = d.pnot.expect("pnot still parses");
    assert!(!pnot.is_known_version());
    assert_eq!(pnot.atom_type, *b"PICT");
    assert_eq!(pnot.atom_index, 1);
}
