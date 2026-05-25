//! Round 137 — Color Table atom (`ctab`) decode.
//!
//! Exercises the movie-level `ctab` surface (QTFF p. 35) against a
//! hand-built QuickTime file whose `moov` carries an optional Color
//! Table atom alongside `mvhd` + a single video `trak`. The atom is
//! an Apple-only extension (no ISO BMFF counterpart); these tests
//! verify the demuxer surfaces every entry byte-for-byte and rejects
//! the spec-forbidden seed / flags variants at open time.
//!
//! Coverage:
//! * a single-entry palette (the zero-relative size-field corner
//!   case — on-disk size = 0 means one entry per QTFF p. 35);
//! * a three-entry primary-RGB palette;
//! * a full 256-entry palette (size = 0xFF on disk);
//! * a file without `ctab` reports `None`;
//! * a malformed `ctab` (`flags != 0x8000`) at open time fails the open;
//! * duplicate `ctab` atoms inside one `moov` keep the first per the
//!   conservative-merge convention shared with `mvhd` / `pdin`.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a `ctab` body (post-atom-header) — `[seed:4][flags:2][size:2]`
/// + N × `[reserved:2][r:2][g:2][b:2]` per QTFF p. 35.
fn build_ctab_body(entries: &[(u16, u16, u16, u16)]) -> Vec<u8> {
    assert!(!entries.is_empty(), "ctab declares at least one color");
    let size_raw = (entries.len() - 1) as u16;
    let mut p = Vec::with_capacity(8 + 8 * entries.len());
    p.extend_from_slice(&0u32.to_be_bytes()); // seed (must be 0)
    p.extend_from_slice(&0x8000u16.to_be_bytes()); // flags (must be 0x8000)
    p.extend_from_slice(&size_raw.to_be_bytes()); // size (zero-relative)
    for (reserved, r, g, b) in entries {
        p.extend_from_slice(&reserved.to_be_bytes());
        p.extend_from_slice(&r.to_be_bytes());
        p.extend_from_slice(&g.to_be_bytes());
        p.extend_from_slice(&b.to_be_bytes());
    }
    p
}

/// Build a minimal one-video-track QuickTime file with an optional list
/// of `ctab` payloads emitted at movie scope (siblings of `mvhd` and
/// `trak`, immediately after `trak` — QTFF p. 32 Figure 2-2 places the
/// color table atom inside the movie atom; the spec doesn't fix an
/// ordering relative to `trak`).
fn build_qt_with_ctab(ctab_payloads: &[Vec<u8>]) -> Vec<u8> {
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
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);

        // QTFF p. 32 Figure 2-2: ctab is a movie-level sibling. Emit
        // after `trak` so test reads validate the walker recognises
        // `ctab` regardless of its order relative to other children.
        for payload in ctab_payloads {
            push_atom(&mut moov, *b"ctab", payload);
        }

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
fn ctab_single_entry_zero_relative() {
    // size on disk = 0 → 1 entry. White.
    let body = build_ctab_body(&[(0, 0xFFFF, 0xFFFF, 0xFFFF)]);
    let file = build_qt_with_ctab(&[body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open ctab single-entry fixture");

    let ctab = d.ctab.as_ref().expect("ctab surface populated");
    assert_eq!(ctab.seed, 0);
    assert_eq!(ctab.flags, 0x8000);
    assert_eq!(ctab.color_count(), 1);
    assert_eq!(ctab.entries.len(), 1);
    let e = &ctab.entries[0];
    assert_eq!(e.reserved, 0);
    assert_eq!(e.red, 0xFFFF);
    assert_eq!(e.green, 0xFFFF);
    assert_eq!(e.blue, 0xFFFF);
    assert_eq!(e.rgb8(), [0xFF, 0xFF, 0xFF]);
}

#[test]
fn ctab_three_entry_primary_rgb_palette() {
    let body = build_ctab_body(&[
        (0, 0xFFFF, 0x0000, 0x0000), // red
        (0, 0x0000, 0xFFFF, 0x0000), // green
        (0, 0x0000, 0x0000, 0xFFFF), // blue
    ]);
    let file = build_qt_with_ctab(&[body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open primary-RGB ctab fixture");

    let ctab = d.ctab.as_ref().expect("ctab surface populated");
    assert_eq!(ctab.color_count(), 3);
    assert_eq!(ctab.entries[0].rgb8(), [0xFF, 0x00, 0x00]);
    assert_eq!(ctab.entries[1].rgb8(), [0x00, 0xFF, 0x00]);
    assert_eq!(ctab.entries[2].rgb8(), [0x00, 0x00, 0xFF]);
}

#[test]
fn ctab_full_256_entry_palette() {
    // size on disk = 0xFF → 256 entries. The classic Mac 256-color
    // palette shape. Each red channel ramps with the index.
    let mut entries = Vec::with_capacity(256);
    for i in 0..256u16 {
        entries.push((0, i << 8, 0xFFFF - (i << 8), 0));
    }
    let body = build_ctab_body(&entries);
    // 8 (header) + 256 × 8 = 2056 bytes inside `ctab`.
    assert_eq!(body.len(), 2056);
    let file = build_qt_with_ctab(&[body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open 256-entry ctab fixture");

    let ctab = d.ctab.as_ref().expect("ctab surface populated");
    assert_eq!(ctab.color_count(), 256);
    assert_eq!(ctab.entries[0].red, 0);
    assert_eq!(ctab.entries[0].green, 0xFFFF);
    assert_eq!(ctab.entries[255].red, 0xFF00);
    assert_eq!(ctab.entries[128].red, 0x8000);
}

#[test]
fn ctab_absent_yields_none() {
    // No ctab declared inside moov.
    let file = build_qt_with_ctab(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open ctab-less fixture");
    assert!(
        d.ctab.is_none(),
        "demuxer reports no ctab when moov omits it"
    );
}

#[test]
fn ctab_rejects_wrong_flags_at_open_time() {
    // Hand-roll a ctab with flags = 0x4000 (spec mandates 0x8000).
    let mut body = build_ctab_body(&[(0, 0, 0, 0)]);
    body[4] = 0x40;
    let file = build_qt_with_ctab(&[body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let err = match MovDemuxer::open(cur) {
        Ok(_) => panic!("malformed ctab should have been rejected at open"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("color_table_flags"),
        "error names the offending field; got: {msg}"
    );
}

#[test]
fn ctab_duplicate_keeps_first() {
    // QTFF p. 35 says nothing about duplicates; the demuxer follows
    // the conservative-merge convention shared with mvhd / pdin —
    // first wins, subsequent are ignored.
    let first = build_ctab_body(&[(0, 0xAAAA, 0xBBBB, 0xCCCC)]);
    let second = build_ctab_body(&[(0, 0x1111, 0x2222, 0x3333)]);
    let file = build_qt_with_ctab(&[first, second]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open duplicate-ctab fixture");

    let ctab = d.ctab.as_ref().expect("first ctab kept");
    assert_eq!(ctab.color_count(), 1);
    assert_eq!(
        ctab.entries[0].red, 0xAAAA,
        "first ctab wins; second discarded"
    );
    assert_eq!(ctab.entries[0].green, 0xBBBB);
    assert_eq!(ctab.entries[0].blue, 0xCCCC);
}
