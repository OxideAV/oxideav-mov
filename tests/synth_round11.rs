//! Round-11 acceptance: HEIF colour-profile extraction (`colr` →
//! [`ColrInfo`]) and HEIF composition-plan helpers
//! (`primary_image_layout()` → `ImageLayout::{Identity,Grid,Overlay}`).
//!
//! These integration tests build complete synthetic HEIF-shaped meta
//! boxes and exercise the public API end-to-end. The renderer's
//! pixel-level behaviour is covered by `synth_round10`; here we only
//! validate the *plan* + the typed colour-profile surface.
//!
//! Spec references:
//! - ISO/IEC 14496-12:2015 §12.1.5 (ColourInformationBox).
//! - ISO/IEC 23008-12:2017 §6.6.2 (derived images).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::push_atom;
use oxideav_core::ReadSeek;
use oxideav_mov::{parse_colr_payload, ColrInfo, ImageLayout, MovDemuxer};

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

/// Build an `iref` v0 box payload with one `dimg` single-item-reference
/// from `from_id` → `to_ids`.
fn iref_dimg(from_id: u16, to_ids: &[u16]) -> Vec<u8> {
    let mut sirb = Vec::new();
    sirb.extend_from_slice(&from_id.to_be_bytes());
    sirb.extend_from_slice(&(to_ids.len() as u16).to_be_bytes());
    for &id in to_ids {
        sirb.extend_from_slice(&id.to_be_bytes());
    }
    let mut iref = Vec::new();
    iref.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    let size = (8 + sirb.len()) as u32;
    iref.extend_from_slice(&size.to_be_bytes());
    iref.extend_from_slice(b"dimg");
    iref.extend_from_slice(&sirb);
    iref
}

/// Build an `idat`-resident `iloc` v1 with one item: offset 0, length =
/// `payload_len`, construction_method=1.
fn iloc_v1_idat_one(item_id: u16, payload_len: u32) -> Vec<u8> {
    let mut iloc = Vec::new();
    iloc.push(1); // version
    iloc.extend_from_slice(&[0, 0, 0]); // flags
    iloc.push(0x44); // offset_size=4, length_size=4
    iloc.push(0x00); // base_offset_size=0, index_size=0
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item_count
    iloc.extend_from_slice(&item_id.to_be_bytes()); // item_id
    iloc.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1 (idat)
    iloc.extend_from_slice(&0u16.to_be_bytes()); // dref_index
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&0u32.to_be_bytes()); // offset
    iloc.extend_from_slice(&payload_len.to_be_bytes());
    iloc
}

/// Build an `iprp` carrying a single `ispe` shared by every item in
/// `tile_ids`. The `ispe` is associated as essential.
fn iprp_shared_ispe(tile_w: u32, tile_h: u32, tile_ids: &[u16]) -> Vec<u8> {
    // ipco with one ispe property.
    let mut ispe_body = Vec::new();
    ispe_body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    ispe_body.extend_from_slice(&tile_w.to_be_bytes());
    ispe_body.extend_from_slice(&tile_h.to_be_bytes());
    let mut ipco = Vec::new();
    push_atom(&mut ipco, *b"ispe", &ispe_body);

    // ipma v0, 8-bit indices, one row per tile.
    let mut ipma = Vec::new();
    ipma.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    ipma.extend_from_slice(&(tile_ids.len() as u32).to_be_bytes());
    for &id in tile_ids {
        ipma.extend_from_slice(&id.to_be_bytes());
        ipma.push(1); // 1 association
        ipma.push(0x81); // essential=1, idx=1
    }

    let mut iprp = Vec::new();
    push_atom(&mut iprp, *b"ipco", &ipco);
    push_atom(&mut iprp, *b"ipma", &ipma);
    iprp
}

/// Build the `grid` derived-image payload (16-bit dims).
fn grid16_payload(rows_minus_one: u8, cols_minus_one: u8, w: u16, h: u16) -> Vec<u8> {
    let mut p = vec![0u8, 0, rows_minus_one, cols_minus_one];
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    p
}

/// Build the `iovl` derived-image payload (16-bit dims+offsets).
fn iovl16_payload(fill: [u16; 4], w: u16, h: u16, layers: &[(i16, i16)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0); // version
    p.push(0); // flags
    for c in fill {
        p.extend_from_slice(&c.to_be_bytes());
    }
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    for (h, v) in layers {
        p.extend_from_slice(&h.to_be_bytes());
        p.extend_from_slice(&v.to_be_bytes());
    }
    p
}

fn ftyp_mif1() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"mif1");
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(b"mif1");
    p
}

// ─────────────────────── A. colr typed extraction ───────────────────────

#[test]
fn parse_colr_payload_nclx_bt709_srgb_bt601_round11_acceptance() {
    // Per the round-11 acceptance: primaries = BT.709 (1), transfer =
    // sRGB (13), matrix = BT.601 (5).
    let mut p = Vec::new();
    p.extend_from_slice(b"nclx");
    p.extend_from_slice(&1u16.to_be_bytes()); // primaries = BT.709
    p.extend_from_slice(&13u16.to_be_bytes()); // transfer = sRGB
    p.extend_from_slice(&5u16.to_be_bytes()); // matrix = BT.601
    p.push(0x00); // full_range = 0
    let info = parse_colr_payload(&p).unwrap();
    match info {
        ColrInfo::Nclx {
            primaries,
            transfer,
            matrix,
            full_range,
        } => {
            assert_eq!(primaries, 1);
            assert_eq!(transfer, 13);
            assert_eq!(matrix, 5);
            assert!(!full_range);
        }
        other => panic!("expected Nclx, got {other:?}"),
    }
}

#[test]
fn parse_colr_payload_ricc_preserves_bytes_and_length_round11_acceptance() {
    let profile_bytes: Vec<u8> = (0u8..32).collect();
    let mut p = Vec::new();
    p.extend_from_slice(b"rICC");
    p.extend_from_slice(&profile_bytes);
    let info = parse_colr_payload(&p).unwrap();
    assert_eq!(info.colour_type(), *b"rICC");
    assert_eq!(info.icc_bytes().unwrap().len(), 32);
    match info {
        ColrInfo::RestrictedIcc(b) => assert_eq!(b, profile_bytes),
        other => panic!("expected RestrictedIcc, got {other:?}"),
    }
}

#[test]
fn parse_colr_payload_prof_preserves_bytes_and_length_round11_acceptance() {
    let profile_bytes: Vec<u8> = (0u8..96).collect();
    let mut p = Vec::new();
    p.extend_from_slice(b"prof");
    p.extend_from_slice(&profile_bytes);
    let info = parse_colr_payload(&p).unwrap();
    assert_eq!(info.colour_type(), *b"prof");
    assert!(info.is_icc());
    assert_eq!(info.icc_bytes().unwrap().len(), 96);
    match info {
        ColrInfo::UnrestrictedIcc(b) => assert_eq!(b, profile_bytes),
        other => panic!("expected UnrestrictedIcc, got {other:?}"),
    }
}

#[test]
fn color_profile_accessor_via_full_meta_round_trip() {
    // Build a HEIF file with hdlr + iinf(item 1 hvc1) + iprp where
    // ipco = [ispe, colr(nclx)] and ipma associates both with item 1.
    // Then resolve color_profile via the demuxer surface end-to-end.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "primary")]);

    // iprp with ispe + nclx colr, both associated with item 1.
    let mut ispe_body = Vec::new();
    ispe_body.extend_from_slice(&0u32.to_be_bytes());
    ispe_body.extend_from_slice(&64u32.to_be_bytes());
    ispe_body.extend_from_slice(&64u32.to_be_bytes());
    let mut colr_body = Vec::new();
    colr_body.extend_from_slice(b"nclx");
    colr_body.extend_from_slice(&1u16.to_be_bytes()); // BT.709
    colr_body.extend_from_slice(&13u16.to_be_bytes()); // sRGB
    colr_body.extend_from_slice(&5u16.to_be_bytes()); // BT.601
    colr_body.push(0x80); // full_range = 1
    let mut ipco = Vec::new();
    push_atom(&mut ipco, *b"ispe", &ispe_body);
    push_atom(&mut ipco, *b"colr", &colr_body);
    let mut ipma = Vec::new();
    ipma.extend_from_slice(&0u32.to_be_bytes());
    ipma.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    ipma.extend_from_slice(&1u16.to_be_bytes()); // item_id = 1
    ipma.push(2); // 2 associations
    ipma.push(0x81); // essential=1, idx=1 (ispe)
    ipma.push(0x02); // essential=0, idx=2 (colr)
    let mut iprp = Vec::new();
    push_atom(&mut iprp, *b"ipco", &ipco);
    push_atom(&mut iprp, *b"ipma", &ipma);

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().unwrap();
    let props = fm.properties.as_ref().unwrap();
    let info = props.color_profile(1).expect("expected nclx profile");
    match info {
        ColrInfo::Nclx {
            primaries,
            transfer,
            matrix,
            full_range,
        } => {
            assert_eq!(primaries, 1);
            assert_eq!(transfer, 13);
            assert_eq!(matrix, 5);
            assert!(full_range);
        }
        other => panic!("expected Nclx, got {other:?}"),
    }
}

// ─────────────────────── B. primary_image_layout(): grid ───────────────────────

#[test]
fn primary_image_layout_grid_2x2_64x64_canvas_128x128() {
    // Build a HEIF file where item 1 is a 2×2 grid of four 64×64
    // tiles (items 10..=13). Validate primary_image_layout() returns
    // Grid with tiles placed at (0,0), (64,0), (0,64), (64,64).
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());

    let iinf = iinf_v0_with_v2_infes(&[
        (1, *b"grid", "primary"),
        (10, *b"hvc1", "tile-0"),
        (11, *b"hvc1", "tile-1"),
        (12, *b"hvc1", "tile-2"),
        (13, *b"hvc1", "tile-3"),
    ]);

    let payload = grid16_payload(1, 1, 128, 128);
    let payload_len = payload.len() as u32;
    let iloc = iloc_v1_idat_one(1, payload_len);

    let iref = iref_dimg(1, &[10, 11, 12, 13]);

    let iprp = iprp_shared_ispe(64, 64, &[10, 11, 12, 13]);

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
        (b"iloc", iloc),
        (b"idat", payload),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let layout = d.primary_image_layout().expect("expected ImageLayout");
    match layout {
        ImageLayout::Grid(g) => {
            assert_eq!(g.canvas_w, 128);
            assert_eq!(g.canvas_h, 128);
            assert_eq!(g.tile_w, 64);
            assert_eq!(g.tile_h, 64);
            assert_eq!(g.rows, 2);
            assert_eq!(g.cols, 2);
            assert_eq!(g.tiles.len(), 4);
            // (col*tile_w, row*tile_h) for raster-order indices 0..4.
            assert_eq!(g.tiles[0].item_id, 10);
            assert_eq!((g.tiles[0].x, g.tiles[0].y), (0, 0));
            assert_eq!(g.tiles[1].item_id, 11);
            assert_eq!((g.tiles[1].x, g.tiles[1].y), (64, 0));
            assert_eq!(g.tiles[2].item_id, 12);
            assert_eq!((g.tiles[2].x, g.tiles[2].y), (0, 64));
            assert_eq!(g.tiles[3].item_id, 13);
            assert_eq!((g.tiles[3].x, g.tiles[3].y), (64, 64));
        }
        other => panic!("expected ImageLayout::Grid, got {other:?}"),
    }
}

// ─────────────────────── B. primary_image_layout(): iovl ───────────────────────

#[test]
fn primary_image_layout_iovl_3_layers_in_dimg_order() {
    // Build a HEIF file where item 1 is an iovl with three layers
    // (items 20, 21, 22) at offsets (0,0), (50, 50), (-10, 100).
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());

    let iinf = iinf_v0_with_v2_infes(&[
        (1, *b"iovl", "primary"),
        (20, *b"hvc1", "layer-0"),
        (21, *b"hvc1", "layer-1"),
        (22, *b"hvc1", "layer-2"),
    ]);

    let payload = iovl16_payload(
        [16384, 16384, 16384, 65535],
        256,
        256,
        &[(0, 0), (50, 50), (-10, 100)],
    );
    let payload_len = payload.len() as u32;
    let iloc = iloc_v1_idat_one(1, payload_len);

    let iref = iref_dimg(1, &[20, 21, 22]);

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iloc", iloc),
        (b"idat", payload),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let layout = d.primary_image_layout().expect("expected ImageLayout");
    match layout {
        ImageLayout::Overlay(o) => {
            assert_eq!(o.canvas_w, 256);
            assert_eq!(o.canvas_h, 256);
            assert_eq!(o.canvas_fill_color, [16384, 16384, 16384, 65535]);
            assert_eq!(o.layers.len(), 3);
            // Layer order matches dimg order with per-layer x,y from
            // the parsed Overlay.
            assert_eq!(o.layers[0].item_id, 20);
            assert_eq!((o.layers[0].x, o.layers[0].y), (0, 0));
            assert_eq!(o.layers[1].item_id, 21);
            assert_eq!((o.layers[1].x, o.layers[1].y), (50, 50));
            assert_eq!(o.layers[2].item_id, 22);
            assert_eq!((o.layers[2].x, o.layers[2].y), (-10, 100));
        }
        other => panic!("expected ImageLayout::Overlay, got {other:?}"),
    }
}

// ─────────────────────── B. primary_image_layout(): identity / bare ───────────────────────

#[test]
fn primary_image_layout_bare_hvc1_returns_identity_with_primary_id() {
    // No derivation — primary item is itself an hvc1, so the layout
    // is Identity { item_id: 1 }.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "primary")]);
    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let layout = d.primary_image_layout().expect("expected ImageLayout");
    match layout {
        ImageLayout::Identity { item_id, .. } => assert_eq!(item_id, 1),
        other => panic!("expected ImageLayout::Identity, got {other:?}"),
    }
}

#[test]
fn primary_image_layout_iden_returns_identity_with_inner_target() {
    // iden item 1 has one dimg target → item 9 (an hvc1). The layout
    // surfaces Identity { item_id: 9 } (the *inner* image), not 1.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"iden", "primary"), (9, *b"hvc1", "inner")]);
    let iref = iref_dimg(1, &[9]);
    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let layout = d.primary_image_layout().expect("expected ImageLayout");
    match layout {
        ImageLayout::Identity { item_id, .. } => assert_eq!(item_id, 9),
        other => panic!("expected ImageLayout::Identity, got {other:?}"),
    }
}

#[test]
fn primary_image_layout_returns_none_when_no_meta_box() {
    // Build a HEIF-shaped meta box that has hdlr but no pitm — the
    // demuxer parses the meta box but primary_image_layout() returns
    // None since there's no primary item.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());
    let body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict())]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    // The meta box exists but has no pitm.
    assert!(d.file_bmff_meta.is_some());
    assert!(d.primary_image_layout().is_none());
}
