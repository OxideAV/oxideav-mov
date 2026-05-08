//! Round-4 acceptance: `udta` user-data subtree (movie + track scope),
//! `dinf/dref` data-reference list parsing, and `tkhd` matrix rotation
//! classification.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{DataReference, MovDemuxer, TrackRotation, UserDataKind};

/// Build a v0 `tkhd` whose 36-byte matrix rotates 90° CW. Layout
/// matches `common::build_tkhd` but overwrites the matrix slot.
fn build_tkhd_rot90(track_id: u32, dur: u32, w_px: u32, h_px: u32) -> Vec<u8> {
    let mut p = build_tkhd(track_id, dur, w_px, h_px);
    // matrix lives at offset 40 (after 4 ver+flags + 20 fixed v0 +
    // 8 reserved + 8 layer/alt/vol/reserved).
    let one: i32 = 0x0001_0000;
    let neg_one: i32 = -0x0001_0000;
    let w_2_30: i32 = 0x4000_0000;
    let m = [0i32, one, 0, neg_one, 0, 0, 0, 0, w_2_30];
    for (i, v) in m.iter().enumerate() {
        let off = 40 + i * 4;
        p[off..off + 4].copy_from_slice(&v.to_be_bytes());
    }
    p
}

/// Build a `dref` with one self-referencing `url ` entry plus one
/// external alias (`url ` pointing at "http://example.com/aux.mov").
fn build_dref_self_plus_external() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&2u32.to_be_bytes()); // n=2

    // Self-reference (flags=0x000001)
    let mut self_child = Vec::new();
    self_child.extend_from_slice(&12u32.to_be_bytes());
    self_child.extend_from_slice(b"url ");
    self_child.push(0);
    self_child.extend_from_slice(&[0, 0, 1]);
    p.extend_from_slice(&self_child);

    // External URL (flags=0)
    let url = b"http://example.com/aux.mov\0";
    let size = (12 + url.len()) as u32;
    let mut ext = Vec::new();
    ext.extend_from_slice(&size.to_be_bytes());
    ext.extend_from_slice(b"url ");
    ext.push(0);
    ext.extend_from_slice(&[0, 0, 0]);
    ext.extend_from_slice(url);
    p.extend_from_slice(&ext);
    p
}

/// Build movie- and track-scoped `udta` blobs:
///
/// * Movie: ©nam = "Test Movie", ©cpy = "(c) 2026", `name` = "QT-7 name"
/// * Track: ©nam = "Track One"
fn build_movie_udta() -> Vec<u8> {
    let mut out = Vec::new();
    // ©nam record body
    let mut nam_body = Vec::new();
    let nam = b"Test Movie";
    nam_body.extend_from_slice(&(nam.len() as u16).to_be_bytes());
    nam_body.extend_from_slice(&0u16.to_be_bytes()); // Mac lang = English
    nam_body.extend_from_slice(nam);
    push_atom(&mut out, [0xA9, b'n', b'a', b'm'], &nam_body);

    let mut cpy_body = Vec::new();
    let cpy = b"(c) 2026";
    cpy_body.extend_from_slice(&(cpy.len() as u16).to_be_bytes());
    cpy_body.extend_from_slice(&0u16.to_be_bytes());
    cpy_body.extend_from_slice(cpy);
    push_atom(&mut out, [0xA9, b'c', b'p', b'y'], &cpy_body);

    // QT-7 plain UTF-8 `name`
    let mut name_body = Vec::new();
    name_body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    let eng: u16 =
        ((b'e' - 0x60) as u16) << 10 | ((b'n' - 0x60) as u16) << 5 | ((b'g' - 0x60) as u16);
    name_body.extend_from_slice(&eng.to_be_bytes());
    name_body.extend_from_slice("QT-7 name".as_bytes());
    push_atom(&mut out, *b"name", &name_body);
    out
}

fn build_track_udta() -> Vec<u8> {
    let mut out = Vec::new();
    let mut nam_body = Vec::new();
    let nam = b"Track One";
    nam_body.extend_from_slice(&(nam.len() as u16).to_be_bytes());
    nam_body.extend_from_slice(&0u16.to_be_bytes());
    nam_body.extend_from_slice(nam);
    push_atom(&mut out, [0xA9, b'n', b'a', b'm'], &nam_body);
    out
}

fn build_qt_with_round4_features() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
    let mdat_payload_offset: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    // Track with rotated tkhd + dinf/dref + udta
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd_rot90(1, 30, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));

    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());

    // dinf > dref
    let mut dinf = Vec::new();
    push_atom(&mut dinf, *b"dref", &build_dref_self_plus_external());
    push_atom(&mut minf, *b"dinf", &dinf);

    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);

    push_atom(&mut trak, *b"udta", &build_track_udta());
    push_atom(&mut moov, *b"trak", &trak);

    push_atom(&mut moov, *b"udta", &build_movie_udta());
    push_atom(&mut out, *b"moov", &moov);

    out
}

#[test]
fn udta_dref_and_rotation_round_trip() {
    let bytes = build_qt_with_round4_features();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open round-4 fixture");

    // 1. tkhd rotation 90 surfaced.
    assert_eq!(d.tracks.len(), 1);
    let t = &d.tracks[0];
    assert_eq!(t.tkhd.rotation(), TrackRotation::Rotate90);

    // 2. dref entries surfaced: SelfRef + external URL.
    assert_eq!(t.data_references().len(), 2);
    assert!(matches!(t.data_references()[0], DataReference::SelfRef));
    match &t.data_references()[1] {
        DataReference::Url(s) => assert_eq!(s, "http://example.com/aux.mov"),
        other => panic!("expected Url, got {other:?}"),
    }
    // Mixed self+external → not self-contained.
    assert!(!t.is_self_contained());

    // 3. Movie-level udta: 3 entries (©nam, ©cpy, name).
    assert_eq!(d.user_data.len(), 3);
    assert!(d
        .user_data
        .iter()
        .any(|e| e.fourcc == [0xA9, b'n', b'a', b'm'] && e.as_str() == Some("Test Movie")));
    assert!(d
        .user_data
        .iter()
        .any(|e| e.fourcc == [0xA9, b'c', b'p', b'y'] && e.as_str() == Some("(c) 2026")));
    let name_entry = d
        .user_data
        .iter()
        .find(|e| e.fourcc == *b"name")
        .expect("name entry");
    assert_eq!(name_entry.as_str(), Some("QT-7 name"));
    assert!(matches!(name_entry.kind, UserDataKind::PlainUtf8 { .. }));

    // 4. Track-level udta: 1 entry (©nam).
    assert_eq!(t.user_data.len(), 1);
    assert_eq!(t.user_data[0].as_str(), Some("Track One"));
}

/// Build a self-contained-only track and confirm `is_self_contained`
/// returns true.
fn build_qt_self_contained() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
    let mdat_payload_offset: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());

    // dinf > dref with single self-reference
    let mut dref = Vec::new();
    dref.extend_from_slice(&0u32.to_be_bytes());
    dref.extend_from_slice(&1u32.to_be_bytes());
    let mut child = Vec::new();
    child.extend_from_slice(&12u32.to_be_bytes());
    child.extend_from_slice(b"url ");
    child.push(0);
    child.extend_from_slice(&[0, 0, 1]); // self-ref
    dref.extend_from_slice(&child);
    let mut dinf = Vec::new();
    push_atom(&mut dinf, *b"dref", &dref);
    push_atom(&mut minf, *b"dinf", &dinf);

    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn self_contained_track_classified_correctly() {
    let bytes = build_qt_self_contained();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open self-contained fixture");
    assert_eq!(d.tracks.len(), 1);
    let t = &d.tracks[0];
    assert_eq!(t.data_references().len(), 1);
    assert!(matches!(t.data_references()[0], DataReference::SelfRef));
    assert!(t.is_self_contained());
    // Default `build_tkhd` writes an identity matrix → no rotation.
    assert_eq!(t.tkhd.rotation(), TrackRotation::None);
}
