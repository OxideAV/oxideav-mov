//! Round 216 — Track Input Map atom (`imap`) decode at track scope.
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01),
//! "Track Input Map Atoms" (pp. 51–53). `imap` sits inside `moov/trak`
//! (QTFF Figure 2-6, p. 41) and carries one track input atom (` in`)
//! per `'ssrc'` (non-primary source) `tref` reference, describing how
//! data from that source modulates this track's presentation.
//!
//! Verifies the demuxer:
//!
//! * surfaces the parsed [`TrackInputMap`] on
//!   [`MovDemuxer::track_input_map`] / [`Track::track_input_map`] when
//!   the track carries an `imap`;
//! * reports `None` when the track omits the atom;
//! * walks several entries in file order and exposes the 1-based
//!   `atom_id` -> `'ssrc'` slot relationship spelled out on QTFF p. 53;
//! * follows the first-wins duplicate-merge policy when a malformed
//!   writer emits two `imap` atoms at trak scope (matches the existing
//!   `clip` / `matt` / `tapt` / `load` / `cslg` policy);
//! * rejects a malformed ` in` body whose required ` ty` child is
//!   absent, so an unhandled-modifier entry can't silently disappear.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{
    InputTypeKind, MovDemuxer, INPUT_TYPE_ATOM, K_TRACK_MODIFIER_OBJECT_MATRIX,
    K_TRACK_MODIFIER_TYPE_BALANCE, K_TRACK_MODIFIER_TYPE_MATRIX, K_TRACK_MODIFIER_TYPE_VOLUME,
    OBJECT_ID_ATOM, TRACK_INPUT_ATOM,
};

fn classic_atom(fourcc: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    let size = (8 + body.len()) as u32;
    v.extend_from_slice(&size.to_be_bytes());
    v.extend_from_slice(&fourcc);
    v.extend_from_slice(body);
    v
}

fn build_ty(raw: u32) -> Vec<u8> {
    classic_atom(INPUT_TYPE_ATOM, &raw.to_be_bytes())
}

fn build_obid(id: u32) -> Vec<u8> {
    classic_atom(OBJECT_ID_ATOM, &id.to_be_bytes())
}

fn build_in(atom_id: u32, children: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&atom_id.to_be_bytes());
    body.extend_from_slice(&[0u8, 0u8]); // reserved
    body.extend_from_slice(&(children.len() as u16).to_be_bytes());
    body.extend_from_slice(&[0u8; 4]); // reserved
    for c in children {
        body.extend_from_slice(c);
    }
    classic_atom(TRACK_INPUT_ATOM, &body)
}

fn build_imap_body(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    for e in entries {
        body.extend_from_slice(e);
    }
    body
}

/// Build a minimal one-video-track QuickTime file. The caller injects
/// any number of `imap` payloads inside the single `trak`. QTFF Figure
/// 2-6 (p. 41) places `imap` inside the track atom alongside `tkhd` /
/// `mdia` / `edts` / `tref` / `load` / `clip` / `matt` / `udta`.
fn build_qt_with_imaps(imap_bodies: &[Vec<u8>]) -> Vec<u8> {
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

        // Inject `imap` payloads as siblings of `tkhd` and `mdia`.
        for payload in imap_bodies {
            push_atom(&mut trak, *b"imap", payload);
        }

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
        push_atom(&mut out, *b"mdat", b"\x01\x02\x03\x04\x05\x06\x07\x08");
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

#[test]
fn single_in_atom_with_matrix_modifier_surfaces_on_demuxer() {
    // One non-primary-source ('ssrc' slot 1) supplies a transform
    // matrix that the track applies to its own location/scaling.
    let in_atom = build_in(1, &[build_ty(K_TRACK_MODIFIER_TYPE_MATRIX)]);
    let imap_body = build_imap_body(&[in_atom]);
    let file = build_qt_with_imaps(&[imap_body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open imap fixture");

    let imap = d
        .track_input_map(0)
        .expect("track 0 surfaces parsed Track Input Map");
    assert_eq!(imap.entries.len(), 1);
    let e = &imap.entries[0];
    assert_eq!(e.atom_id, 1);
    assert_eq!(e.input_type.kind, InputTypeKind::Matrix);
    assert!(e.object_id.is_none());
}

#[test]
fn multiple_in_entries_round_trip_in_file_order() {
    // Three 'ssrc' slots: slot 1 is a volume modifier (fade), slot 2
    // is a balance modifier (pan), slot 3 is a per-object matrix
    // (sprite transform).
    let in1 = build_in(1, &[build_ty(K_TRACK_MODIFIER_TYPE_VOLUME)]);
    let in2 = build_in(2, &[build_ty(K_TRACK_MODIFIER_TYPE_BALANCE)]);
    let in3 = build_in(
        3,
        &[build_ty(K_TRACK_MODIFIER_OBJECT_MATRIX), build_obid(0x4242)],
    );
    let imap_body = build_imap_body(&[in1, in2, in3]);
    let file = build_qt_with_imaps(&[imap_body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open multi-entry imap fixture");

    let imap = d.track_input_map(0).expect("track 0 imap present");
    assert_eq!(imap.entries.len(), 3);

    assert_eq!(imap.entries[0].atom_id, 1);
    assert_eq!(imap.entries[0].input_type.kind, InputTypeKind::Volume);
    assert!(imap.entries[0].object_id.is_none());

    assert_eq!(imap.entries[1].atom_id, 2);
    assert_eq!(imap.entries[1].input_type.kind, InputTypeKind::Balance);
    assert!(imap.entries[1].object_id.is_none());

    assert_eq!(imap.entries[2].atom_id, 3);
    assert_eq!(imap.entries[2].input_type.kind, InputTypeKind::ObjectMatrix);
    assert_eq!(imap.entries[2].object_id.map(|o| o.id), Some(0x4242));

    // The atom_id-keyed accessor honours QTFF p. 53's 1-based 'ssrc'
    // slot relationship — useful when file order does not match
    // numeric slot order.
    let hit = imap.entry_for_ssrc_slot(2).expect("slot 2 found");
    assert_eq!(hit.input_type.kind, InputTypeKind::Balance);
    assert!(imap.entry_for_ssrc_slot(99).is_none());
}

#[test]
fn imap_absent_yields_none_on_demuxer() {
    let file = build_qt_with_imaps(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open imap-less fixture");
    assert!(d.track_input_map(0).is_none());
    assert!(d.tracks[0].track_input_map().is_none());
}

#[test]
fn duplicate_imap_first_wins() {
    // Two `imap` atoms in one trak — spec forbids but conservative
    // policy retains the first (matches clip / matt / tapt / load /
    // cslg first-wins behaviour at this scope).
    let first = build_imap_body(&[build_in(1, &[build_ty(K_TRACK_MODIFIER_TYPE_VOLUME)])]);
    let second = build_imap_body(&[build_in(2, &[build_ty(K_TRACK_MODIFIER_TYPE_BALANCE)])]);
    let file = build_qt_with_imaps(&[first, second]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open duplicate-imap fixture");

    let imap = d.track_input_map(0).expect("first imap retained");
    assert_eq!(imap.entries.len(), 1);
    assert_eq!(imap.entries[0].atom_id, 1);
    assert_eq!(imap.entries[0].input_type.kind, InputTypeKind::Volume);
}

#[test]
fn in_atom_missing_required_ty_rejects_open() {
    // ` in` with QT header tail but no children at all — QTFF p. 52
    // marks ` ty` as required.
    let bad = build_imap_body(&[build_in(1, &[])]);
    let file = build_qt_with_imaps(&[bad]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let r = MovDemuxer::open(cur);
    assert!(r.is_err(), "missing ' ty' child must fail open");
    let msg = format!("{:?}", r.err().unwrap());
    assert!(
        msg.contains("missing required ' ty'") || msg.contains("missing"),
        "expected missing-ty error, got: {msg}",
    );
}
