//! Round 182 — User-Type Box (`uuid`) decode at file scope.
//!
//! Exercises the file-level `uuid` surface (ISO/IEC 14496-12:2015
//! §4.2 / §11.1) against hand-built containers that drop one or more
//! `uuid` boxes between `ftyp` and `moov`. The `uuid` escape is the
//! spec's vendor-extension mechanism: the body starts with a 16-byte
//! UUID identifying the vendor schema and continues with an opaque
//! payload. Vendor extensions of this shape ship widely — PIFF tfxd
//! / tfrf (Smooth Streaming live-DASH), Sony XAVC clip metadata,
//! GoPro GPMF telemetry — and the parser must round-trip them
//! without committing the crate to any private schema.
//!
//! These tests open via `MovDemuxer` and verify:
//! * a `uuid` body with `usertype` + payload byte-for-byte;
//! * `Quantity: Zero or more` semantics — a file may carry several,
//!   and order is preserved;
//! * a file without `uuid` surfaces an empty `file_uuids` vec;
//! * a body shorter than the 16-byte `usertype` prefix is rejected
//!   at open time (no silent half-record);
//! * `is_iso_reserved_namespace` flags the §11.1 escape pattern
//!   (vendor extensions never use it; mis-issued boxes are caught).
//!
//! Each fixture also re-runs the `stco` chunk-offset locator after
//! prepending `uuid` boxes, so the sample table survives the
//! per-vendor box insertion without re-locating.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, USERTYPE_LEN};

/// Build a `uuid` body: 16-byte UUID prefix followed by `payload`.
fn build_uuid_body(usertype: [u8; 16], payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(USERTYPE_LEN + payload.len());
    p.extend_from_slice(&usertype);
    p.extend_from_slice(payload);
    p
}

/// Build a one-video-track QuickTime file with `ftyp`, an optional
/// list of file-level `uuid` payloads (each is the post-header body
/// — `usertype` + payload), then `moov` and `mdat`. The `uuid` boxes
/// are emitted between `ftyp` and `moov` per the §4.2 "any top-level
/// box" placement rule.
fn build_qt_with_uuids(uuid_payloads: &[Vec<u8>]) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  ");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        for payload in uuid_payloads {
            push_atom(&mut out, *b"uuid", payload);
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

/// PIFF tfxd UUID (Microsoft Smooth Streaming live-DASH timing
/// extension), commonly seen in vendor-shipped fMP4 segments.
const PIFF_TFXD_UUID: [u8; 16] = [
    0x6d, 0x1d, 0x9b, 0x05, 0x42, 0xd5, 0x44, 0xe6, 0x80, 0xe2, 0x14, 0x1d, 0xaf, 0xf7, 0x57, 0xb2,
];

/// PIFF tfrf UUID (companion live-DASH timing extension to tfxd).
const PIFF_TFRF_UUID: [u8; 16] = [
    0xd4, 0x80, 0x7e, 0xf2, 0xca, 0x39, 0x46, 0x95, 0x8e, 0x54, 0x26, 0xcb, 0x9e, 0x46, 0xa7, 0x9f,
];

#[test]
fn single_uuid_surfaces_with_payload() {
    let body = build_uuid_body(PIFF_TFXD_UUID, b"piff-tfxd-payload");
    let bytes = build_qt_with_uuids(&[body]);
    let dmx = MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).unwrap();
    assert_eq!(dmx.file_uuids.len(), 1);
    assert_eq!(dmx.file_uuids[0].usertype, PIFF_TFXD_UUID);
    assert_eq!(dmx.file_uuids[0].payload, b"piff-tfxd-payload");
    assert_eq!(
        dmx.file_uuids[0].usertype_string(),
        "6d1d9b05-42d5-44e6-80e2-141daff757b2"
    );
}

#[test]
fn multiple_uuids_preserved_in_declaration_order() {
    // §4.2 has no spec-defined ordering rule for `uuid` boxes; a
    // single file may carry several (different vendor extensions).
    // The parser preserves the writer's order so the caller can
    // match by index when a paired extension has a defined
    // sequence (e.g. PIFF tfxd appearing before tfrf in Smooth
    // Streaming fragments).
    let body_a = build_uuid_body(PIFF_TFXD_UUID, b"tfxd-bytes");
    let body_b = build_uuid_body(PIFF_TFRF_UUID, b"tfrf-bytes");
    let bytes = build_qt_with_uuids(&[body_a, body_b]);
    let dmx = MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).unwrap();
    assert_eq!(dmx.file_uuids.len(), 2);
    assert_eq!(dmx.file_uuids[0].usertype, PIFF_TFXD_UUID);
    assert_eq!(dmx.file_uuids[0].payload, b"tfxd-bytes");
    assert_eq!(dmx.file_uuids[1].usertype, PIFF_TFRF_UUID);
    assert_eq!(dmx.file_uuids[1].payload, b"tfrf-bytes");
}

#[test]
fn empty_payload_after_usertype_is_legal() {
    // §4.2 puts no lower bound on the payload length; a `uuid` body
    // of exactly 16 bytes (UUID only) parses successfully.
    let body = build_uuid_body(PIFF_TFXD_UUID, &[]);
    let bytes = build_qt_with_uuids(&[body]);
    let dmx = MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).unwrap();
    assert_eq!(dmx.file_uuids.len(), 1);
    assert_eq!(dmx.file_uuids[0].usertype, PIFF_TFXD_UUID);
    assert!(dmx.file_uuids[0].payload.is_empty());
}

#[test]
fn no_uuid_yields_empty_vec() {
    let bytes = build_qt_with_uuids(&[]);
    let dmx = MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).unwrap();
    assert!(dmx.file_uuids.is_empty());
}

#[test]
fn truncated_uuid_body_rejected_at_open() {
    // 15 bytes — one short of the mandatory 16-byte `usertype` per
    // §4.2's `if (boxtype == 'uuid') unsigned int(8)[16] usertype`.
    // A half-prefix can't silently disappear: `open` must surface
    // the parse error so the caller can fail-fast on the malformed
    // container.
    let short_body = vec![0xAA; USERTYPE_LEN - 1];
    let bytes = build_qt_with_uuids(&[short_body]);
    let result = MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>);
    assert!(result.is_err());
}

#[test]
fn iso_reserved_namespace_uuid_is_flagged() {
    // §11.1 reserves the form `type ‖ 00 11 00 10 80 00 00 AA 00 38
    // 9B 71` for the auto-derived UUID of every normative box type.
    // §11.1 also forbids writing standard boxes through the
    // `'uuid'` escape — a true result here therefore flags a
    // non-conformant writer. The parser keeps the entry rather than
    // reject so callers can diagnose the offending file.
    let id_for_free = [
        0x66, 0x72, 0x65, 0x65, 0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B,
        0x71,
    ];
    let body = build_uuid_body(id_for_free, b"reserved-namespace-violation");
    let bytes = build_qt_with_uuids(&[body]);
    let dmx = MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).unwrap();
    assert_eq!(dmx.file_uuids.len(), 1);
    assert!(dmx.file_uuids[0].is_iso_reserved_namespace());
    assert_eq!(dmx.file_uuids[0].iso_namespace_boxtype(), Some(*b"free"));
}

#[test]
fn vendor_uuid_does_not_match_iso_namespace() {
    let body = build_uuid_body(PIFF_TFXD_UUID, b"vendor-payload");
    let bytes = build_qt_with_uuids(&[body]);
    let dmx = MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).unwrap();
    assert!(!dmx.file_uuids[0].is_iso_reserved_namespace());
    assert_eq!(dmx.file_uuids[0].iso_namespace_boxtype(), None);
}

#[test]
fn binary_payload_round_trips_byte_for_byte() {
    // Vendor payloads carry arbitrary binary; the parser must not
    // alter any byte of the trailing data.
    let payload: Vec<u8> = (0..=255u8).collect();
    let body = build_uuid_body(PIFF_TFXD_UUID, &payload);
    let bytes = build_qt_with_uuids(&[body]);
    let dmx = MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).unwrap();
    assert_eq!(dmx.file_uuids[0].payload, payload);
}
