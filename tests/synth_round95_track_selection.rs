//! Round 95 — Track Selection box (`tsel`) parser wiring.
//!
//! ISO/IEC 14496-12 §8.10.3 ("Track Selection Box", pp. 72–74). `tsel`
//! sits inside the track-level `udta` and carries:
//!
//! * a signed 32-bit `switch_group` identifier — non-zero values mark
//!   the track as a candidate for runtime switching with other tracks
//!   sharing the same id (and the same `tkhd.alternate_group`);
//! * a list of attribute FourCCs (§8.10.3.5) describing or
//!   differentiating the track from its peers — six descriptive
//!   (`tesc/fgsc/cgsc/spsc/resc/vwsc`) + eight differentiating
//!   (`cdec/scsz/mpsz/mtyp/mela/bitr/frar/nvws`).
//!
//! Round 95 wires the parser into the per-`trak` walk and surfaces the
//! typed [`oxideav_mov::TrackSelection`] body via:
//!
//! * `Track::track_selection()`
//! * `MovDemuxer::track_selection(track_index)`
//! * `MovDemuxer::switch_groups()` — ranking lookup across the file.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{
    MovDemuxer, TrackSelection, TsAttributeRole, TSEL_ATTR_BITRATE, TSEL_ATTR_CODEC,
    TSEL_ATTR_MEDIA_LANGUAGE, TSEL_ATTR_TEMPORAL_SCALABILITY,
};

/// Build a `tsel` FullBox body — `[ver:1][flags:3][switch_group:i32][attrs...]`.
fn build_tsel_body(switch_group: i32, attrs: &[[u8; 4]]) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + attrs.len() * 4);
    p.extend_from_slice(&[0u8; 4]); // ver=0 + flags=0
    p.extend_from_slice(&switch_group.to_be_bytes());
    for a in attrs {
        p.extend_from_slice(a);
    }
    p
}

/// Build a one-video-track QTFF file with an optional track-level
/// `udta/tsel` carriage. mvhd ts = 600, mdhd ts = 600, 4×30-tick
/// samples.
fn build_qt_with_tsel(tsel_body: Option<&[u8]>, alt_group: i16) -> Vec<u8> {
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
        // Use the flags-aware builder so alt_group lands in tkhd.
        push_atom(
            &mut trak,
            *b"tkhd",
            &build_tkhd_flags(1, 120, 320, 240, 0x07, alt_group),
        );
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

        // Optional track-level udta carrying tsel.
        if let Some(body) = tsel_body {
            let mut udta = Vec::new();
            push_atom(&mut udta, *b"tsel", body);
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
fn no_tsel_surfaces_none() {
    let bytes = build_qt_with_tsel(None, 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open no-tsel");
    assert!(d.track_selection(0).is_none());
    assert!(d.tracks[0].track_selection().is_none());
    // No tsel in the file ⇒ switch_groups() is empty.
    assert!(d.switch_groups().is_empty());
}

#[test]
fn tsel_round_trips_switch_group_and_attribute_list() {
    // switch_group=42, [bitr, cdec, mela] — a realistic adaptive-
    // bitrate switch group annotated with codec + language pointers.
    let body = build_tsel_body(
        42,
        &[TSEL_ATTR_BITRATE, TSEL_ATTR_CODEC, TSEL_ATTR_MEDIA_LANGUAGE],
    );
    let bytes = build_qt_with_tsel(Some(&body), 1);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open with tsel");
    let ts: &TrackSelection = d.track_selection(0).expect("tsel parsed");
    assert_eq!(ts.switch_group, 42);
    assert_eq!(ts.attributes.len(), 3);
    assert!(ts.has_attribute(&TSEL_ATTR_BITRATE));
    assert!(ts.has_attribute(&TSEL_ATTR_CODEC));
    assert!(ts.has_attribute(&TSEL_ATTR_MEDIA_LANGUAGE));
    assert!(ts.is_informative());
    // All three are differentiating attributes per §8.10.3.5.
    for (_fc, role) in ts.typed_attributes() {
        assert_eq!(role, TsAttributeRole::Differentiating);
    }
}

#[test]
fn tsel_signed_switch_group_negative_round_trips() {
    // Spec declares `template int(32) switch_group` — values are
    // signed, so -1 on the wire must surface as -1.
    let body = build_tsel_body(-1, &[]);
    let bytes = build_qt_with_tsel(Some(&body), 1);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open negative switch_group");
    let ts = d.track_selection(0).expect("tsel parsed");
    assert_eq!(ts.switch_group, -1);
}

#[test]
fn tsel_descriptive_attribute_classifies_as_descriptive() {
    // tesc — Temporal Scalability, a descriptive attribute.
    let body = build_tsel_body(7, &[TSEL_ATTR_TEMPORAL_SCALABILITY]);
    let bytes = build_qt_with_tsel(Some(&body), 1);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open descriptive tsel");
    let ts = d.track_selection(0).expect("tsel parsed");
    let (fc, role) = ts.typed_attributes().next().unwrap();
    assert_eq!(fc, TSEL_ATTR_TEMPORAL_SCALABILITY);
    assert_eq!(role, TsAttributeRole::Descriptive);
}

#[test]
fn tsel_unknown_attribute_fourcc_is_preserved() {
    // Vendor extension attribute "XvNd" not in §8.10.3.5.
    let vendor = *b"XvNd";
    let body = build_tsel_body(3, &[TSEL_ATTR_BITRATE, vendor]);
    let bytes = build_qt_with_tsel(Some(&body), 1);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open vendor tsel");
    let ts = d.track_selection(0).expect("tsel parsed");
    assert!(ts.has_attribute(&vendor));
    let mut iter = ts.typed_attributes();
    assert_eq!(
        iter.next().unwrap(),
        (TSEL_ATTR_BITRATE, TsAttributeRole::Differentiating)
    );
    assert_eq!(iter.next().unwrap(), (vendor, TsAttributeRole::Unknown));
}

#[test]
fn tsel_empty_box_with_zero_switch_group_is_not_informative() {
    // tsel present but switch_group=0 and attributes empty — equivalent
    // to "tsel absent" per §8.10.3.4 last sentence; we surface the box
    // but mark it non-informative.
    let body = build_tsel_body(0, &[]);
    let bytes = build_qt_with_tsel(Some(&body), 0);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open empty tsel");
    let ts = d.track_selection(0).expect("tsel parsed");
    assert!(!ts.is_informative());
    // switch_group == 0 ⇒ excluded from the switch_groups() bucket map.
    assert!(d.switch_groups().is_empty());
}

#[test]
fn track_selection_out_of_range_track_index_returns_none() {
    let body = build_tsel_body(5, &[TSEL_ATTR_BITRATE]);
    let bytes = build_qt_with_tsel(Some(&body), 1);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open");
    assert!(d.track_selection(42).is_none());
}

#[test]
fn switch_groups_buckets_single_track_correctly() {
    // Single track with switch_group=11 — switch_groups() must return
    // exactly one bucket (11, [0]).
    let body = build_tsel_body(11, &[TSEL_ATTR_CODEC]);
    let bytes = build_qt_with_tsel(Some(&body), 2);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open switch-group track");
    let groups = d.switch_groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0, 11);
    assert_eq!(groups[0].1, vec![0usize]);
}
