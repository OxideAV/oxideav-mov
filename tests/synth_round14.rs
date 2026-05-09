//! Round-14 acceptance: HEIF auxiliary-plane resolver (`auxC` URN +
//! `auxl` iref) surfaced on the [`ImageLayout::Identity`] layout, and
//! HDR mastering-display / content-light-level / content-colour-volume
//! item-property typed extraction (`clli` / `mdcv` / `cclv`).
//!
//! r13 surfaced `pixi` and `colr` on the `Identity` layout but
//! treated the `auxC` and HDR-mastering properties as opaque
//! `ItemProperty::Other` fall-throughs. r14 closes both gaps:
//!
//! * `auxC` URN is parsed and reshaped on the layout as
//!   `Identity { alpha_for: Some(target_id) }` when the auxiliary
//!   item carries a recognised alpha URN and an `auxl` iref binds it
//!   to its master colour image (HEIF §7.5.1 / MIAF Annex B).
//! * `clli` / `mdcv` / `cclv` are typed `ItemProperty` variants with
//!   `ItemProperties::clli(item_id)` / `mdcv(item_id)` /
//!   `cclv(item_id)` accessors that return the parsed structs.
//!
//! Spec references:
//! - ISO/IEC 23008-12:2017 §7.5.1 (AuxiliaryTypeProperty + alpha URNs).
//! - ISO/IEC 23008-12:2017 §6.5 (HDR property catalogue).
//! - ISO/IEC 14496-12:2015 §8.11.12 (Item Reference Box, `auxl`).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::push_atom;
use oxideav_core::ReadSeek;
use oxideav_mov::{
    parse_auxc_payload, parse_cclv_payload, parse_clli_payload, parse_mdcv_payload, ImageLayout,
    MovDemuxer,
};

// ─────────────────────── helpers (HEIF meta builders) ───────────────────────

fn build_meta_atom_payload(children: Vec<(&'static [u8; 4], Vec<u8>)>) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes());
    for (fc, child_body) in &children {
        let size = (8 + child_body.len()) as u32;
        body.extend_from_slice(&size.to_be_bytes());
        body.extend_from_slice(*fc);
        body.extend_from_slice(child_body);
    }
    body
}

fn hdlr_pict() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(b"pict");
    p.extend_from_slice(&[0u8; 12]);
    p.push(0);
    p
}

fn pitm_v0(item_id: u16) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&item_id.to_be_bytes());
    p
}

fn iinf_v0_with_v2_infes(entries: &[(u16, [u8; 4], &str)]) -> Vec<u8> {
    let mut iinf = Vec::new();
    iinf.extend_from_slice(&0u32.to_be_bytes());
    iinf.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for (id, ty, name) in entries {
        let mut infe_body = Vec::new();
        infe_body.push(2);
        infe_body.extend_from_slice(&[0, 0, 0]);
        infe_body.extend_from_slice(&id.to_be_bytes());
        infe_body.extend_from_slice(&0u16.to_be_bytes());
        infe_body.extend_from_slice(ty);
        infe_body.extend_from_slice(name.as_bytes());
        infe_body.push(0);
        let size = (8 + infe_body.len()) as u32;
        iinf.extend_from_slice(&size.to_be_bytes());
        iinf.extend_from_slice(b"infe");
        iinf.extend_from_slice(&infe_body);
    }
    iinf
}

/// Build an `iref` v0 box payload with one `kind` reference from
/// `from_id` → `to_ids`.
fn iref_one(kind: &[u8; 4], from_id: u16, to_ids: &[u16]) -> Vec<u8> {
    let mut sirb = Vec::new();
    sirb.extend_from_slice(&from_id.to_be_bytes());
    sirb.extend_from_slice(&(to_ids.len() as u16).to_be_bytes());
    for &id in to_ids {
        sirb.extend_from_slice(&id.to_be_bytes());
    }
    let mut iref = Vec::new();
    iref.extend_from_slice(&0u32.to_be_bytes());
    let size = (8 + sirb.len()) as u32;
    iref.extend_from_slice(&size.to_be_bytes());
    iref.extend_from_slice(kind);
    iref.extend_from_slice(&sirb);
    iref
}

fn ftyp_with_compat(major: &[u8; 4], compat: &[&[u8; 4]]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(major);
    p.extend_from_slice(&0u32.to_be_bytes());
    for c in compat {
        p.extend_from_slice(*c);
    }
    p
}

fn ispe_body(w: u32, h: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    p
}

fn auxc_alpha_urn() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(b"urn:mpeg:hevc:2015:auxid:1");
    p.push(0); // NUL terminator
    p
}

/// Build an `iprp` containing a single ipco arranged as the slice of
/// (fourcc, payload) tuples, and a single ipma row for each item that
/// associates ALL ipco indices with that item (1-based, all
/// non-essential).
fn iprp_with_props_and_associations(
    ipco_props: &[(&[u8; 4], Vec<u8>)],
    item_associations: &[(u16, &[u8])],
) -> Vec<u8> {
    let mut ipco = Vec::new();
    for (fc, body) in ipco_props {
        push_atom(&mut ipco, **fc, body);
    }
    let mut ipma = Vec::new();
    ipma.extend_from_slice(&0u32.to_be_bytes()); // ver=0, flags=0
    ipma.extend_from_slice(&(item_associations.len() as u32).to_be_bytes());
    for (item_id, assoc_indices) in item_associations {
        ipma.extend_from_slice(&item_id.to_be_bytes());
        ipma.push(assoc_indices.len() as u8);
        for &idx in *assoc_indices {
            ipma.push(idx & 0x7F); // essential=0
        }
    }
    let mut iprp = Vec::new();
    push_atom(&mut iprp, *b"ipco", &ipco);
    push_atom(&mut iprp, *b"ipma", &ipma);
    iprp
}

// ─────────────────────── A. auxC alpha-plane resolver ───────────────────────

#[test]
fn auxc_alpha_with_auxl_iref_resolves_alpha_for_target() {
    // Primary colour item id=1 (hvc1 4:2:0); alpha auxiliary item id=2
    // (mono hvc1) with auxC URN urn:mpeg:hevc:2015:auxid:1. auxl iref
    // from id=2 → id=1 binds the alpha plane to its master.
    //
    // Ask for the alpha item's layout (id=2): expect Identity {
    //   item_id: 2, alpha_for: Some(1), ... }.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    let iinf =
        iinf_v0_with_v2_infes(&[(1, *b"hvc1", "primary"), (2, *b"hvc1", "alpha-plane-mono")]);
    // alpha → master: from=2, to=1.
    let iref = iref_one(b"auxl", 2, &[1]);

    // ipco: [1]=ispe, [2]=auxC(alpha URN). ipma: item 1 → [1]; item 2
    // → [1, 2].
    let iprp = iprp_with_props_and_associations(
        &[(b"ispe", ispe_body(64, 64)), (b"auxC", auxc_alpha_urn())],
        &[(1, &[1]), (2, &[1, 2])],
    );

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();

    // Primary item (id=1) is the colour image — alpha_for is None.
    let primary = d.primary_image_layout().expect("primary layout");
    match primary {
        ImageLayout::Identity {
            item_id, alpha_for, ..
        } => {
            assert_eq!(item_id, 1);
            assert!(alpha_for.is_none(), "primary colour item is not alpha");
        }
        other => panic!("expected Identity for primary, got {other:?}"),
    }

    // Drive the alpha-plane item directly.
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta available");
    let alpha = oxideav_mov::image_layout_for(fm, 2).expect("alpha layout");
    match alpha {
        ImageLayout::Identity {
            item_id, alpha_for, ..
        } => {
            assert_eq!(item_id, 2);
            assert_eq!(
                alpha_for,
                Some(1),
                "auxC URN + auxl iref resolve to master id=1"
            );
        }
        other => panic!("expected Identity for alpha plane, got {other:?}"),
    }
}

#[test]
fn auxc_with_non_alpha_urn_does_not_set_alpha_for() {
    // Same shape as the alpha test, but the auxC URN is the depth one
    // (auxid:2) — alpha_for must NOT be set.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "primary"), (2, *b"hvc1", "depth-aux")]);
    let iref = iref_one(b"auxl", 2, &[1]);

    let auxc_depth_urn = {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"urn:mpeg:hevc:2015:auxid:2");
        p.push(0);
        p
    };

    let iprp = iprp_with_props_and_associations(
        &[(b"ispe", ispe_body(64, 64)), (b"auxC", auxc_depth_urn)],
        &[(1, &[1]), (2, &[1, 2])],
    );

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta available");
    let depth = oxideav_mov::image_layout_for(fm, 2).expect("depth-aux layout");
    match depth {
        ImageLayout::Identity { alpha_for, .. } => {
            assert!(
                alpha_for.is_none(),
                "depth URN must not be classified as alpha"
            );
        }
        other => panic!("expected Identity for depth aux, got {other:?}"),
    }
}

#[test]
fn auxc_alpha_with_no_auxl_iref_yields_none_alpha_for() {
    // alpha URN attached but no auxl iref — defensive: alpha_for is
    // None because we can't resolve the master target.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "primary"), (2, *b"hvc1", "lonely-alpha")]);

    let iprp = iprp_with_props_and_associations(
        &[(b"ispe", ispe_body(64, 64)), (b"auxC", auxc_alpha_urn())],
        &[(1, &[1]), (2, &[1, 2])],
    );

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta available");
    let alpha = oxideav_mov::image_layout_for(fm, 2).expect("alpha layout");
    match alpha {
        ImageLayout::Identity { alpha_for, .. } => {
            assert!(alpha_for.is_none(), "no auxl iref → no master target");
        }
        other => panic!("expected Identity for alpha plane, got {other:?}"),
    }
}

#[test]
fn auxc_mpegb_cicp_alpha_urn_is_recognised() {
    // MIAF Annex B URN: urn:mpeg:mpegB:cicp:systems:auxiliary:alpha.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"avif", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"av01", "colour"), (2, *b"av01", "alpha-mpegb")]);
    let iref = iref_one(b"auxl", 2, &[1]);

    let mpegb_alpha = {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha");
        p.push(0);
        p
    };

    let iprp = iprp_with_props_and_associations(
        &[(b"ispe", ispe_body(128, 128)), (b"auxC", mpegb_alpha)],
        &[(1, &[1]), (2, &[1, 2])],
    );

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta available");
    let alpha = oxideav_mov::image_layout_for(fm, 2).expect("alpha layout");
    match alpha {
        ImageLayout::Identity { alpha_for, .. } => {
            assert_eq!(alpha_for, Some(1), "MIAF mpegB alpha URN is recognised");
        }
        other => panic!("expected Identity for alpha plane, got {other:?}"),
    }
}

// ─────────────────────── B. clli typed extraction ───────────────────────

#[test]
fn parse_clli_payload_returns_max_cll_and_max_fall_in_bare_form() {
    // Bare 4-byte form: no FullBox header.
    let mut p = Vec::new();
    p.extend_from_slice(&1000u16.to_be_bytes()); // MaxCLL
    p.extend_from_slice(&400u16.to_be_bytes()); // MaxFALL
    let clli = parse_clli_payload(&p).unwrap();
    assert_eq!(clli.max_content_light_level, 1000);
    assert_eq!(clli.max_pic_average_light_level, 400);
}

#[test]
fn parse_clli_payload_accepts_fullbox_prefixed_form() {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&4000u16.to_be_bytes());
    p.extend_from_slice(&500u16.to_be_bytes());
    let clli = parse_clli_payload(&p).unwrap();
    assert_eq!(clli.max_content_light_level, 4000);
    assert_eq!(clli.max_pic_average_light_level, 500);
}

#[test]
fn parse_clli_payload_rejects_wrong_size() {
    assert!(parse_clli_payload(&[0u8; 3]).is_err());
    assert!(parse_clli_payload(&[0u8; 5]).is_err());
}

#[test]
fn iprp_clli_accessor_returns_typed_struct_for_associated_item() {
    // ipco: [ispe, clli(MaxCLL=1000, MaxFALL=400)].
    // ipma: item 1 → [1, 2].
    let mut clli_body = Vec::new();
    clli_body.extend_from_slice(&1000u16.to_be_bytes());
    clli_body.extend_from_slice(&400u16.to_be_bytes());

    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));
    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "hdr-primary")]);
    let iprp = iprp_with_props_and_associations(
        &[(b"ispe", ispe_body(1024, 1024)), (b"clli", clli_body)],
        &[(1, &[1, 2])],
    );
    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta available");
    let props = fm.properties.as_ref().expect("iprp present");
    let info = props.clli(1).expect("clli surfaced for primary item");
    assert_eq!(info.max_content_light_level, 1000);
    assert_eq!(info.max_pic_average_light_level, 400);
}

// ─────────────────────── C. mdcv typed extraction ───────────────────────

fn sample_mdcv_bytes() -> Vec<u8> {
    // BT.2020 RGB primaries × 50000 + D65 white point + max=1000nits
    // (×10000 = 10_000_000), min=0.05nits (×10000 = 500). Order on
    // disk: G, B, R per ST 2086.
    let mut p = Vec::new();
    // G: (0.17, 0.797) → (8500, 39850)
    p.extend_from_slice(&8500u16.to_be_bytes());
    // B: (0.131, 0.046) → (6550, 2300)
    p.extend_from_slice(&6550u16.to_be_bytes());
    // R: (0.708, 0.292) → (35400, 14600)
    p.extend_from_slice(&35400u16.to_be_bytes());
    // Y values G, B, R
    p.extend_from_slice(&39850u16.to_be_bytes());
    p.extend_from_slice(&2300u16.to_be_bytes());
    p.extend_from_slice(&14600u16.to_be_bytes());
    // White point D65: (0.3127, 0.3290) → (15635, 16450)
    p.extend_from_slice(&15635u16.to_be_bytes());
    p.extend_from_slice(&16450u16.to_be_bytes());
    // max_display_luminance = 10_000_000 (1000 cd/m² × 10000)
    p.extend_from_slice(&10_000_000u32.to_be_bytes());
    // min_display_luminance = 500 (0.05 cd/m² × 10000)
    p.extend_from_slice(&500u32.to_be_bytes());
    p
}

#[test]
fn parse_mdcv_payload_round_trips_all_primaries_white_point_and_luminances() {
    let bytes = sample_mdcv_bytes();
    assert_eq!(bytes.len(), 24);
    let mdcv = parse_mdcv_payload(&bytes).unwrap();
    // G, B, R order per spec.
    assert_eq!(mdcv.display_primaries[0], (8500, 39850));
    assert_eq!(mdcv.display_primaries[1], (6550, 2300));
    assert_eq!(mdcv.display_primaries[2], (35400, 14600));
    assert_eq!(mdcv.white_point, (15635, 16450));
    assert_eq!(mdcv.max_display_luminance, 10_000_000);
    assert_eq!(mdcv.min_display_luminance, 500);
}

#[test]
fn parse_mdcv_payload_accepts_fullbox_prefixed_28_byte_form() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    bytes.extend_from_slice(&sample_mdcv_bytes());
    assert_eq!(bytes.len(), 28);
    let mdcv = parse_mdcv_payload(&bytes).unwrap();
    assert_eq!(mdcv.white_point, (15635, 16450));
    assert_eq!(mdcv.max_display_luminance, 10_000_000);
}

#[test]
fn parse_mdcv_payload_rejects_wrong_size() {
    assert!(parse_mdcv_payload(&[0u8; 23]).is_err());
    assert!(parse_mdcv_payload(&[0u8; 25]).is_err());
}

#[test]
fn iprp_mdcv_accessor_returns_typed_struct_for_associated_item() {
    let bytes = sample_mdcv_bytes();
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));
    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "hdr-primary")]);
    let iprp = iprp_with_props_and_associations(
        &[(b"ispe", ispe_body(3840, 2160)), (b"mdcv", bytes)],
        &[(1, &[1, 2])],
    );
    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta available");
    let props = fm.properties.as_ref().expect("iprp present");
    let info = props.mdcv(1).expect("mdcv surfaced for primary item");
    assert_eq!(info.display_primaries[2], (35400, 14600), "R primary");
    assert_eq!(info.white_point, (15635, 16450), "D65");
    assert_eq!(info.max_display_luminance, 10_000_000);
    assert_eq!(info.min_display_luminance, 500);
}

// ─────────────────────── D. cclv typed extraction ───────────────────────

#[test]
fn parse_cclv_payload_decodes_cancel_persist_flags_only_when_no_subrecords() {
    // FullBox + flags byte where bits 7+6 are set (cancel + persist),
    // all sub-record present-flags are 0.
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.push(0xC0); // cancel + persist
    let cclv = parse_cclv_payload(&p).unwrap();
    assert!(cclv.cancel_flag);
    assert!(cclv.persistence_flag);
    assert!(cclv.primaries.is_none());
    assert!(cclv.min_luminance.is_none());
    assert!(cclv.max_luminance.is_none());
    assert!(cclv.avg_luminance.is_none());
}

#[test]
fn parse_cclv_payload_decodes_max_luminance_only() {
    // Only max_luminance present → flags bit 3 set; trailing 4-byte
    // u32. Cancel/persist clear; primaries / min / avg absent.
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.push(0x08); // bit 3 set
    p.extend_from_slice(&12_345u32.to_be_bytes());
    let cclv = parse_cclv_payload(&p).unwrap();
    assert!(!cclv.cancel_flag);
    assert!(!cclv.persistence_flag);
    assert!(cclv.primaries.is_none());
    assert!(cclv.min_luminance.is_none());
    assert_eq!(cclv.max_luminance, Some(12_345));
    assert!(cclv.avg_luminance.is_none());
}

#[test]
fn parse_cclv_payload_decodes_full_record_with_primaries_and_three_luminances() {
    // primaries + min + max + avg all present.
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    // bits 5..2 set: primaries + min + max + avg present.
    p.push(0x3C);
    // primaries x[3]: G, B, R
    p.extend_from_slice(&8500i32.to_be_bytes());
    p.extend_from_slice(&6550i32.to_be_bytes());
    p.extend_from_slice(&35400i32.to_be_bytes());
    // primaries y[3]: G, B, R
    p.extend_from_slice(&39850i32.to_be_bytes());
    p.extend_from_slice(&2300i32.to_be_bytes());
    p.extend_from_slice(&14600i32.to_be_bytes());
    // min, max, avg
    p.extend_from_slice(&100u32.to_be_bytes());
    p.extend_from_slice(&10_000_000u32.to_be_bytes());
    p.extend_from_slice(&100_000u32.to_be_bytes());
    let cclv = parse_cclv_payload(&p).unwrap();
    let prims = cclv.primaries.expect("primaries present");
    assert_eq!(prims[0], (8500, 39850));
    assert_eq!(prims[1], (6550, 2300));
    assert_eq!(prims[2], (35400, 14600));
    assert_eq!(cclv.min_luminance, Some(100));
    assert_eq!(cclv.max_luminance, Some(10_000_000));
    assert_eq!(cclv.avg_luminance, Some(100_000));
}

#[test]
fn parse_cclv_payload_rejects_short_body() {
    // Bit 5 set (primaries present) but no primaries bytes follow.
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.push(0x20);
    assert!(parse_cclv_payload(&p).is_err());
}

#[test]
fn iprp_cclv_accessor_returns_typed_struct_for_associated_item() {
    // Build cclv with only max_luminance present.
    let mut cclv_body = Vec::new();
    cclv_body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    cclv_body.push(0x08); // bit 3: max_luminance present
    cclv_body.extend_from_slice(&8_888_888u32.to_be_bytes());

    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));
    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "hdr-primary")]);
    let iprp = iprp_with_props_and_associations(
        &[(b"ispe", ispe_body(64, 64)), (b"cclv", cclv_body)],
        &[(1, &[1, 2])],
    );
    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta available");
    let props = fm.properties.as_ref().expect("iprp present");
    let info = props.cclv(1).expect("cclv surfaced for primary item");
    assert_eq!(info.max_luminance, Some(8_888_888));
    assert!(info.min_luminance.is_none());
}

// ─────────────────────── E. AuxC standalone parser ───────────────────────

#[test]
fn parse_auxc_payload_extracts_alpha_urn() {
    let body = auxc_alpha_urn();
    let auxc = parse_auxc_payload(&body).unwrap();
    assert_eq!(auxc.aux_type, "urn:mpeg:hevc:2015:auxid:1");
    assert!(auxc.aux_subtype.is_empty());
    assert!(auxc.is_alpha());
    assert!(!auxc.is_depth());
}

#[test]
fn parse_auxc_payload_extracts_depth_urn() {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(b"urn:mpeg:hevc:2015:auxid:2");
    p.push(0);
    let auxc = parse_auxc_payload(&p).unwrap();
    assert_eq!(auxc.aux_type, "urn:mpeg:hevc:2015:auxid:2");
    assert!(!auxc.is_alpha());
    assert!(auxc.is_depth());
}

#[test]
fn parse_auxc_payload_rejects_short_body() {
    assert!(parse_auxc_payload(&[0u8; 3]).is_err());
}
