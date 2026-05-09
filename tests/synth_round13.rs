//! Round-13 acceptance: HEIF iden transformative-property cascade
//! composed onto the [`ImageLayout::Identity`] layout, HEIF `pixi`
//! channel-bit-depth surfaced on the same layout, and MIAF / brand
//! classification (`MovDemuxer::brand_class()`, `is_heic()`,
//! `is_avif()`, `is_miaf()`).
//!
//! r12 surfaced iden as a bare `Identity { item_id }` pointing at the
//! `iden` derivation's inner `dimg` target, and the caller had to
//! re-walk `iprp` to discover the iden item's transformative
//! properties. r13 composes those properties (clap / irot / imir)
//! into a `TransformChain` on the layout itself, and additionally
//! surfaces the inner item's `pixi` and `colr` so callers can
//! decide on channel-bit-depth and colour-space without re-walking
//! `iprp`.
//!
//! Spec references:
//! - ISO/IEC 23008-12:2017 §6.5.6.3 (PixelInformationProperty pixi).
//! - ISO/IEC 23008-12:2017 §6.5 (transformative properties + spec order).
//! - ISO/IEC 14496-12:2015 §8.5 (FileTypeBox / brands).
//! - ISO/IEC 23000-22 (MIAF: mif1, mif2, MA1A, MA1B).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::push_atom;
use oxideav_core::ReadSeek;
use oxideav_mov::{BrandClass, ColrInfo, ImageLayout, MovDemuxer, TransformOp};

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

/// Build an `iref` v0 box payload with one `dimg` reference from
/// `from_id` → `to_ids`.
fn iref_dimg(from_id: u16, to_ids: &[u16]) -> Vec<u8> {
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
    iref.extend_from_slice(b"dimg");
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

fn pixi_body(channels: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.push(channels.len() as u8);
    p.extend_from_slice(channels);
    p
}

fn irot_body(steps: u8) -> Vec<u8> {
    vec![steps & 0x03]
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

// ─────────────────────── A. iden TransformChain on Identity layout ───────────────────────

#[test]
fn iden_layout_carries_transform_chain_with_iden_irot_and_inner_clap() {
    // iden (item 1) carries irot{1}; inner hvc1 (item 9) carries no
    // transformative props but carries pixi {3,8,8,8}.
    // Expected layout: Identity { item_id: 9, transform: [Irot{1}],
    //                              pixi: Some({8,8,8}) }.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"mif1", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"iden", "iden"), (9, *b"hvc1", "inner")]);
    let iref = iref_dimg(1, &[9]);

    // ipco: [0]=irot(steps=1), [1]=ispe(64x64), [2]=pixi{8,8,8}.
    // ipma: item 1 → [1] (irot); item 9 → [2, 3] (ispe, pixi).
    let iprp = iprp_with_props_and_associations(
        &[
            (b"irot", irot_body(1)),
            (b"ispe", ispe_body(64, 64)),
            (b"pixi", pixi_body(&[8, 8, 8])),
        ],
        &[(1, &[1]), (9, &[2, 3])],
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
    let layout = d.primary_image_layout().expect("ImageLayout for iden");
    match layout {
        ImageLayout::Identity {
            item_id,
            transform,
            pixi,
            color_profile,
        } => {
            assert_eq!(item_id, 9, "Identity surfaces inner item id");
            assert_eq!(
                transform,
                vec![TransformOp::Irot { steps: 1 }],
                "iden's irot composed onto the inner item's chain"
            );
            assert_eq!(
                pixi.expect("pixi surfaced from inner item").channels,
                vec![8, 8, 8]
            );
            assert!(color_profile.is_none(), "no colr associated");
        }
        other => panic!("expected Identity, got {other:?}"),
    }
}

#[test]
fn bare_hvc1_layout_surfaces_pixi_and_colr_directly() {
    // Bare hvc1 primary item (no derivation); pixi {3,10,10,10} +
    // colr nclx (BT.2020 + PQ + full-range).
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "primary")]);

    let mut colr = Vec::new();
    colr.extend_from_slice(b"nclx");
    colr.extend_from_slice(&9u16.to_be_bytes()); // BT.2020
    colr.extend_from_slice(&16u16.to_be_bytes()); // PQ
    colr.extend_from_slice(&9u16.to_be_bytes()); // BT.2020 NC
    colr.push(0x80); // full_range = true

    let iprp = iprp_with_props_and_associations(
        &[
            (b"ispe", ispe_body(1024, 1024)),
            (b"pixi", pixi_body(&[10, 10, 10])),
            (b"colr", colr),
        ],
        &[(1, &[1, 2, 3])],
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
    let layout = d.primary_image_layout().expect("ImageLayout");
    match layout {
        ImageLayout::Identity {
            item_id,
            transform,
            pixi,
            color_profile,
        } => {
            assert_eq!(item_id, 1);
            assert!(transform.is_empty(), "no transformative properties");
            assert_eq!(pixi.expect("pixi").channels, vec![10, 10, 10]);
            match color_profile.expect("colr") {
                ColrInfo::Nclx {
                    primaries,
                    transfer,
                    matrix,
                    full_range,
                } => {
                    assert_eq!(primaries, 9);
                    assert_eq!(transfer, 16);
                    assert_eq!(matrix, 9);
                    assert!(full_range);
                }
                other => panic!("expected Nclx, got {other:?}"),
            }
        }
        other => panic!("expected Identity, got {other:?}"),
    }
}

// ─────────────────────── B. brand_class accessors ───────────────────────

#[test]
fn brand_class_walks_ftyp_compatible_brands_in_order() {
    let mut out = Vec::new();
    push_atom(
        &mut out,
        *b"ftyp",
        &ftyp_with_compat(b"heic", &[b"mif1", b"isom"]),
    );
    let body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict())]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    assert_eq!(
        d.brand_class(),
        vec![BrandClass::Heic, BrandClass::Mif1, BrandClass::Isom]
    );
    assert!(d.is_heic());
    assert!(d.is_miaf(), "heic + mif1 entail MIAF");
    assert!(!d.is_avif());
}

#[test]
fn is_avif_picks_up_avif_compat_brand() {
    let mut out = Vec::new();
    push_atom(
        &mut out,
        *b"ftyp",
        &ftyp_with_compat(b"mif1", &[b"avif", b"miaf"]),
    );
    let body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict())]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    assert!(d.is_avif());
    assert!(d.is_miaf());
    assert!(!d.is_heic());
}

#[test]
fn is_miaf_recognises_explicit_mif1_only_brand() {
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"mif1", &[b"isom"]));
    let body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict())]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    assert!(d.is_miaf());
    assert!(!d.is_heic());
    assert!(!d.is_avif());
}

#[test]
fn ma1b_brand_classified_and_recognised_as_miaf() {
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"MA1B", &[b"avif"]));
    let body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict())]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let classes = d.brand_class();
    assert_eq!(classes, vec![BrandClass::Ma1b, BrandClass::Avif]);
    assert!(d.is_miaf(), "MA1B is a MIAF Annex A profile");
    assert!(d.is_avif());
    assert!(!d.is_heic());
}

#[test]
fn qt_only_ftyp_classified_as_qt_and_not_miaf() {
    // QTFF native: major=qt, compat=qt — none of the HEIF / MIAF /
    // AVIF accessors should report true.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"qt  ", &[b"qt  "]));
    let body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict())]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    assert_eq!(d.brand_class(), vec![BrandClass::Qt, BrandClass::Qt]);
    assert!(!d.is_heic());
    assert!(!d.is_avif());
    assert!(!d.is_miaf());
}
