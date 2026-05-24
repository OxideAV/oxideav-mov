//! Round 125 — Segment Type Box (`styp`) decode.
//!
//! Exercises the file-level `styp` surface (ISO/IEC 14496-12 §8.16.2)
//! against hand-built ISO BMFF segment streams whose top level carries
//! one or more `styp` boxes. The box has the same on-disk shape as
//! `ftyp` (§4.3) — `major_brand[4]` + `minor_version[4]` +
//! `compatible_brands[4]*` — and identifies a DASH / CMAF / HLS-fMP4
//! media segment plus the specifications it conforms to.
//!
//! These tests open via `MovDemuxer` and verify:
//! * the parsed `Styp` exposes `major_brand` / `minor_version` /
//!   `compatible_brands` in file order,
//! * multiple `styp` boxes (e.g. at a concatenated segment boundary)
//!   are collected in file order,
//! * `MovDemuxer::first_styp()` surfaces the first segment-type
//!   declaration (§8.16.2.1's "shall be the first box in a segment"),
//! * `MovDemuxer::is_dash_segment()` recognises the three DASH
//!   segment-conformance brands (`msdh` / `msix` / `risx`),
//! * a file without `styp` reports an empty `Vec`.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a `styp` box body — same shape as `ftyp` (§4.3).
fn build_styp_payload(major: &[u8; 4], minor: u32, compat: &[&[u8; 4]]) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + 4 * compat.len());
    p.extend_from_slice(major);
    p.extend_from_slice(&minor.to_be_bytes());
    for b in compat {
        p.extend_from_slice(*b);
    }
    p
}

/// Build a minimal one-video-track ISO BMFF file with `ftyp`, an
/// optional file-level `styp` (controlled by the closure argument),
/// `moov`, and `mdat`. `styp_blocks` lists the segment-type boxes to
/// emit; an empty slice elides them entirely. Each entry is
/// `(position_before_moov: bool, payload)`; `false` places it after
/// `moov` so the file-order collection test can verify the walker
/// picks up `styp` regardless of placement (§8.16.2.1 says it
/// "shall be the first box in a segment", but the walker is tolerant
/// of out-of-order writers).
fn build_isobmff_with_styps(styp_blocks: &[(bool, Vec<u8>)]) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();

        // styp boxes scheduled before `ftyp` — i.e. as the very first
        // box in the file (the spec-correct placement for a segment
        // stream).
        for (before_moov, payload) in styp_blocks {
            if *before_moov {
                push_atom(&mut out, *b"styp", payload);
            }
        }

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
        push_atom(&mut out, *b"moov", &moov);

        // styp boxes scheduled after `moov` — non-spec-conformant
        // placement but the walker should still collect them.
        for (before_moov, payload) in styp_blocks {
            if !*before_moov {
                push_atom(&mut out, *b"styp", payload);
            }
        }

        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

#[test]
fn styp_first_in_file_parses_dash_brands() {
    // A typical DASH init/media segment starts with:
    //   styp(major='iso5', minor=0, compat=['iso5','dash','msdh'])
    // before `ftyp`. The walker must collect it as the first segment-
    // type box and expose `major_brand` + `compatible_brands` verbatim.
    let payload = build_styp_payload(b"iso5", 0, &[b"iso5", b"dash", b"msdh"]);
    let file = build_isobmff_with_styps(&[(true, payload)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open styp fixture");
    assert_eq!(d.styp.len(), 1);
    let s = d.first_styp().expect("first styp present");
    assert_eq!(&s.major_brand, b"iso5");
    assert_eq!(s.minor_version, 0);
    assert_eq!(s.compatible_brands.len(), 3);
    assert_eq!(&s.compatible_brands[0], b"iso5");
    assert_eq!(&s.compatible_brands[1], b"dash");
    assert_eq!(&s.compatible_brands[2], b"msdh");
    // DASH-segment classifier picks up the `msdh` compatible brand.
    assert!(d.is_dash_segment());
    assert!(!d.is_cmaf_segment());
}

#[test]
fn multiple_styp_collected_in_file_order() {
    // A concatenated segment stream may carry several `styp` boxes —
    // one per segment boundary. §8.16.2.1 says any `styp` not first
    // in its file "may be ignored", but we preserve them all so a
    // caller can use them as boundary markers.
    let s1 = build_styp_payload(b"msdh", 0, &[b"msdh"]);
    let s2 = build_styp_payload(b"msix", 1, &[b"msix"]);
    let s3 = build_styp_payload(b"cmfs", 0, &[b"cmfs", b"msdh"]);
    let file = build_isobmff_with_styps(&[(true, s1), (false, s2), (false, s3)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open multi-styp fixture");
    assert_eq!(d.styp.len(), 3);
    // File-order: first (pre-ftyp) styp is `msdh`; the two post-moov
    // styps land at positions 1 and 2 in declaration order.
    assert_eq!(&d.styp[0].major_brand, b"msdh");
    assert_eq!(&d.styp[1].major_brand, b"msix");
    assert_eq!(d.styp[1].minor_version, 1);
    assert_eq!(&d.styp[2].major_brand, b"cmfs");
    // `first_styp()` returns the file-order first — i.e. the `msdh`
    // declaration, which is the segment-type identifier per §8.16.2.1.
    let first = d.first_styp().expect("first styp present");
    assert_eq!(&first.major_brand, b"msdh");
    assert!(d.is_dash_segment());
    // `cmfs` appears on the third styp, not the first, so the
    // first-box classifier doesn't trigger for CMAF.
    assert!(!d.is_cmaf_segment());
}

#[test]
fn styp_absent_yields_empty_vec_and_false_classifiers() {
    let file = build_isobmff_with_styps(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open file with no styp");
    assert!(d.styp.is_empty());
    assert!(d.first_styp().is_none());
    assert!(!d.is_dash_segment());
    assert!(!d.is_cmaf_segment());
}

#[test]
fn styp_cmaf_brand_classifier() {
    // CMAF segment-conformance brand is `cmfs` (Common Media
    // Application Format segment). When carried as either major or
    // a compatible brand on the first styp, `is_cmaf_segment()` fires.
    let payload = build_styp_payload(b"cmfs", 0, &[]);
    let file = build_isobmff_with_styps(&[(true, payload)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open cmaf-major styp fixture");
    assert!(d.is_cmaf_segment());
    assert!(!d.is_dash_segment());

    // Same brand surfaced through `compatible_brands` rather than
    // `major_brand` — classifier still fires.
    let payload = build_styp_payload(b"iso6", 0, &[b"cmfs"]);
    let file = build_isobmff_with_styps(&[(true, payload)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open cmaf-compat styp fixture");
    assert!(d.is_cmaf_segment());
}

#[test]
fn styp_truncated_payload_rejects_at_open_time() {
    // 7-byte body — one short of the 8-byte fixed header (major +
    // minor). The walker must reject the box at open time.
    let mut out = Vec::new();
    push_atom(&mut out, *b"styp", &[0u8; 7]);
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"isom");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "a truncated styp payload must be rejected at open time"
    );
}

#[test]
fn styp_unaligned_compatible_brand_tail_rejected() {
    // Header + one extra trailing byte (less than one full FourCC).
    let mut payload = Vec::new();
    payload.extend_from_slice(b"msdh");
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.push(0xAA); // single dangling byte

    let mut out = Vec::new();
    push_atom(&mut out, *b"styp", &payload);
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"isom");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "an unaligned compatible-brand tail must be rejected at open time"
    );
}

#[test]
fn styp_empty_compatible_brands_is_legal() {
    // §4.3 / §8.16.2: an 8-byte body (just major + minor) with no
    // compatible brands is a valid segment-type box.
    let payload = build_styp_payload(b"msdh", 42, &[]);
    let file = build_isobmff_with_styps(&[(true, payload)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open empty-compat styp fixture");
    let s = d.first_styp().expect("styp parsed");
    assert_eq!(&s.major_brand, b"msdh");
    assert_eq!(s.minor_version, 42);
    assert!(s.compatible_brands.is_empty());
    // `msdh` is itself a DASH brand → classifier fires on the major.
    assert!(d.is_dash_segment());
}
