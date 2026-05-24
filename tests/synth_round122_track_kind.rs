//! Round 122 — Track Kind box (`kind`) parser wiring.
//!
//! ISO/IEC 14496-12 §8.10.4 ("Track kind", p. 74). `kind` sits inside
//! the track-level `udta` (`moov/trak/udta/kind`) and labels the track
//! with a semantic role expressed as a `(schemeURI, value?)` pair.
//! Both strings are NULL-terminated C strings (§8.10.4.3). The box is
//! `Quantity: Zero or more` (§8.10.4.1) so a track may carry several
//! `kind` entries side by side — typically one per role taxonomy
//! (WebVTT, DASH, vendor-specific).
//!
//! Round 122 wires the parser into the per-`trak` walk and surfaces the
//! typed [`oxideav_mov::KindEntry`] list via:
//!
//! * `Track::track_kinds()`
//! * `MovDemuxer::track_kinds(track_index)`
//!
//! The box is ISO BMFF-only — QTFF defines no equivalent — so a plain
//! `.mov` input that omits `udta/kind` returns an empty slice, which
//! the absence-case test below exercises.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{KindEntry, MovDemuxer};

/// Build a `kind` FullBox body — `[ver:1][flags:3][schemeURI\0][value\0]`.
/// `value=None` writes a bare NULL terminator (the spec shape for "no
/// value, schemeURI identifies the kind itself").
fn build_kind_body(scheme: &str, value: Option<&str>) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0u8; 4]); // ver=0 + flags=0
    p.extend_from_slice(scheme.as_bytes());
    p.push(0);
    if let Some(v) = value {
        p.extend_from_slice(v.as_bytes());
    }
    p.push(0);
    p
}

/// Build a one-video-track QTFF file with an optional track-level
/// `udta/kind` carriage. `kind_bodies` lets the caller stack multiple
/// `kind` children (§8.10.4.1 — `Quantity: Zero or more`). mvhd ts =
/// 600, mdhd ts = 600, 4×30-tick samples.
fn build_qt_with_kinds(kind_bodies: &[&[u8]]) -> Vec<u8> {
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

        if !kind_bodies.is_empty() {
            let mut udta = Vec::new();
            for body in kind_bodies {
                push_atom(&mut udta, *b"kind", body);
            }
            push_atom(&mut trak, *b"udta", &udta);
        }

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
fn kind_dash_role_main_round_trips() {
    // The standard DASH role taxonomy: `urn:mpeg:dash:role:2011` with a
    // value drawn from the registry (here, "main"). This is the most
    // common shape modern CMAF / DASH files emit.
    let body = build_kind_body("urn:mpeg:dash:role:2011", Some("main"));
    let bytes = build_qt_with_kinds(&[&body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open with kind");
    let kinds: &[KindEntry] = d.track_kinds(0);
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].scheme_uri, "urn:mpeg:dash:role:2011");
    assert_eq!(kinds[0].value.as_deref(), Some("main"));
    assert!(kinds[0].has_value());
}

#[test]
fn kind_webvtt_role_subtitles_round_trips() {
    // WebVTT role taxonomy — used by HTML5 / HLS / fMP4 subtitle tracks.
    let body = build_kind_body("https://www.w3.org/TR/webvtt1/", Some("subtitles"));
    let bytes = build_qt_with_kinds(&[&body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open webvtt kind");
    let kinds = d.track_kinds(0);
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].scheme_uri, "https://www.w3.org/TR/webvtt1/");
    assert_eq!(kinds[0].value.as_deref(), Some("subtitles"));
}

#[test]
fn kind_scheme_only_no_value_is_none() {
    // §8.10.4.3 — when only the schemeURI is meaningful, `value` is the
    // empty string on the wire and surfaces as `None`.
    let body = build_kind_body("urn:example:role:identity", None);
    let bytes = build_qt_with_kinds(&[&body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open scheme-only kind");
    let kinds = d.track_kinds(0);
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].scheme_uri, "urn:example:role:identity");
    assert!(kinds[0].value.is_none());
    assert!(!kinds[0].has_value());
}

#[test]
fn kind_multiple_entries_preserved_in_file_order() {
    // §8.10.4.1 — "More than one of these may occur in a track, with
    // different contents but with appropriate semantics". Confirm we
    // surface every entry in the on-disk order rather than first-match.
    let a = build_kind_body("urn:mpeg:dash:role:2011", Some("caption"));
    let b = build_kind_body("https://www.w3.org/TR/webvtt1/", Some("captions"));
    let bytes = build_qt_with_kinds(&[&a, &b]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open multi-kind");
    let kinds = d.track_kinds(0);
    assert_eq!(kinds.len(), 2);
    assert_eq!(kinds[0].scheme_uri, "urn:mpeg:dash:role:2011");
    assert_eq!(kinds[0].value.as_deref(), Some("caption"));
    assert_eq!(kinds[1].scheme_uri, "https://www.w3.org/TR/webvtt1/");
    assert_eq!(kinds[1].value.as_deref(), Some("captions"));
}

#[test]
fn kind_absent_from_udta_yields_empty_slice() {
    // No `udta`, no `kind` — `track_kinds` returns an empty slice.
    let bytes = build_qt_with_kinds(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open kindless");
    assert!(d.track_kinds(0).is_empty());
}

#[test]
fn kind_out_of_range_track_index_returns_empty_slice() {
    let body = build_kind_body("urn:mpeg:dash:role:2011", Some("alternate"));
    let bytes = build_qt_with_kinds(&[&body]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open");
    assert!(d.track_kinds(42).is_empty());
}
