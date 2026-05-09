//! Round-15 acceptance: HEIF transformative-property dimensional math
//! ([`ImageLayout::output_extent`]), HDR mastering metadata
//! ([`Clli`] / [`Mdcv`] / [`Cclv`]) surfaced on the `Identity` layout
//! alongside r14's `pixi` / `colr` / `alpha_for`, and HEIF tone-mapping
//! item-property typed extraction (`amve`) + a new
//! [`ImageLayout::ToneMap`] variant for `tmap` derivations.
//!
//! r14 surfaced the four HDR property variants (`clli` / `mdcv` /
//! `cclv` / and now `amve`) on `iprp` but left layout consumers to
//! re-walk the iprp themselves. r15 closes three gaps:
//!
//! * **#3** — `ImageLayout::output_extent(meta)` returns the
//!   post-`TransformChain` `(out_w, out_h)` for `Identity`, deriving
//!   from the inner item's `ispe` then composing each `clap` / `irot` /
//!   `imir` step per HEIF §6.5.9 / §6.5.10 / §6.5.12. `Grid` /
//!   `Overlay` return the canvas extent verbatim; `ToneMap` defers to
//!   the base item's extent.
//! * **#4** — `ImageLayout::Identity { …, clli, mdcv, cclv, amve }`
//!   carries the four HDR property structs alongside `pixi` / `colr`
//!   so callers don't have to re-walk `iprp`.
//! * **#5** — `amve` typed extraction (Ambient Viewing Environment;
//!   HEIF Amd.1 / SMPTE ST 2108-1) and `tmap` derivation surfaced as a
//!   new `ImageLayout::ToneMap { item_id, base, params }` variant per
//!   HEIF Amd.1 §6.6.x.
//!
//! Spec references:
//! - ISO/IEC 14496-12:2015 §12.1.4 (CleanApertureBox math).
//! - ISO/IEC 23008-12:2017 §6.5.9 (clap), §6.5.10 (irot), §6.5.12 (imir).
//! - ISO/IEC 23008-12 Amd.1 §6.5.x (amve) + §6.6.x (tmap derivation).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::push_atom;
use oxideav_core::ReadSeek;
use oxideav_mov::{
    compute_post_transform_extent, image_layout_for, parse_amve_payload, parse_tmap_payload, Amve,
    Clap, Clli, ImageLayout, Imir, Irot, Mdcv, MovDemuxer, TransformOp,
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

fn irot_body(steps: u8) -> Vec<u8> {
    vec![steps & 0x03]
}

#[allow(dead_code)]
fn imir_body(axis: u8) -> Vec<u8> {
    vec![axis & 0x01]
}

fn clap_body(c: &Clap) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&c.clean_aperture_width_n.to_be_bytes());
    p.extend_from_slice(&c.clean_aperture_width_d.to_be_bytes());
    p.extend_from_slice(&c.clean_aperture_height_n.to_be_bytes());
    p.extend_from_slice(&c.clean_aperture_height_d.to_be_bytes());
    p.extend_from_slice(&c.horiz_off_n.to_be_bytes());
    p.extend_from_slice(&c.horiz_off_d.to_be_bytes());
    p.extend_from_slice(&c.vert_off_n.to_be_bytes());
    p.extend_from_slice(&c.vert_off_d.to_be_bytes());
    p
}

fn clli_body(maxcll: u16, maxfall: u16) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&maxcll.to_be_bytes());
    p.extend_from_slice(&maxfall.to_be_bytes());
    p
}

fn mdcv_body() -> Vec<u8> {
    // BT.2020 RGB primaries × 50000 + D65 + max=1000 nits, min=0.05 nits.
    // G, B, R x then G, B, R y then white_point_x, white_point_y, then
    // max u32, min u32 = 24 bytes.
    let mut p = Vec::new();
    p.extend_from_slice(&8500u16.to_be_bytes()); // G x
    p.extend_from_slice(&6550u16.to_be_bytes()); // B x
    p.extend_from_slice(&35400u16.to_be_bytes()); // R x
    p.extend_from_slice(&39850u16.to_be_bytes()); // G y
    p.extend_from_slice(&2300u16.to_be_bytes()); // B y
    p.extend_from_slice(&14600u16.to_be_bytes()); // R y
    p.extend_from_slice(&15635u16.to_be_bytes()); // wp x
    p.extend_from_slice(&16450u16.to_be_bytes()); // wp y
    p.extend_from_slice(&10_000_000u32.to_be_bytes()); // max
    p.extend_from_slice(&500u32.to_be_bytes()); // min
    p
}

fn cclv_body_max_only(max: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // FullBox ver+flags
    p.push(0x08); // bit 3 set: max_luminance present
    p.extend_from_slice(&max.to_be_bytes());
    p
}

fn amve_body() -> Vec<u8> {
    // FullBox-prefixed; 12 bytes total.
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&314_150_000u32.to_be_bytes()); // 31415 lux × 10000
    p.extend_from_slice(&15635u16.to_be_bytes()); // D65 x ×50000
    p.extend_from_slice(&16450u16.to_be_bytes()); // D65 y ×50000
    p
}

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

// ─────────────────────── #3 — output_extent over TransformChain ───────────────────────

#[test]
fn compute_post_transform_extent_irot_swaps_axes() {
    // 1280x720 with irot{1} → 720x1280 (90° CCW transpose).
    let chain = vec![TransformOp::Irot { steps: 1 }];
    let (w, h) = compute_post_transform_extent(1280, 720, &chain).unwrap();
    assert_eq!((w, h), (720, 1280));

    // Same with irot{3} (270°).
    let chain = vec![TransformOp::Irot { steps: 3 }];
    let (w, h) = compute_post_transform_extent(1280, 720, &chain).unwrap();
    assert_eq!((w, h), (720, 1280));
}

#[test]
fn compute_post_transform_extent_irot_180_preserves_axes() {
    let chain = vec![TransformOp::Irot { steps: 2 }];
    let (w, h) = compute_post_transform_extent(640, 480, &chain).unwrap();
    assert_eq!((w, h), (640, 480));
}

#[test]
fn compute_post_transform_extent_imir_preserves_axes() {
    for axis in 0u8..=1 {
        let chain = vec![TransformOp::Imir { axis }];
        let (w, h) = compute_post_transform_extent(640, 480, &chain).unwrap();
        assert_eq!((w, h), (640, 480), "axis={axis}");
    }
}

#[test]
fn compute_post_transform_extent_clap_yields_clean_aperture_dims() {
    // 256x256 with clap (W: 128/1, H: 128/1) → (128, 128).
    let clap = Clap {
        clean_aperture_width_n: 128,
        clean_aperture_width_d: 1,
        clean_aperture_height_n: 128,
        clean_aperture_height_d: 1,
        horiz_off_n: 64,
        horiz_off_d: 1,
        vert_off_n: 64,
        vert_off_d: 1,
    };
    let chain = vec![TransformOp::Clap(clap)];
    let (w, h) = compute_post_transform_extent(256, 256, &chain).unwrap();
    assert_eq!((w, h), (128, 128));
}

#[test]
fn compute_post_transform_extent_clap_then_irot_swaps_after_crop() {
    // 256x256 source, clap → 128x128, then irot{1} → swap to (128,128)
    // (square so trivially equal).
    let clap = Clap {
        clean_aperture_width_n: 128,
        clean_aperture_width_d: 1,
        clean_aperture_height_n: 128,
        clean_aperture_height_d: 1,
        horiz_off_n: 64,
        horiz_off_d: 1,
        vert_off_n: 64,
        vert_off_d: 1,
    };
    let chain = vec![TransformOp::Clap(clap), TransformOp::Irot { steps: 1 }];
    let (w, h) = compute_post_transform_extent(256, 256, &chain).unwrap();
    assert_eq!((w, h), (128, 128));

    // Non-square clap to make the swap visible.
    let clap_rect = Clap {
        clean_aperture_width_n: 200,
        clean_aperture_width_d: 1,
        clean_aperture_height_n: 100,
        clean_aperture_height_d: 1,
        horiz_off_n: 28,
        horiz_off_d: 1,
        vert_off_n: 78,
        vert_off_d: 1,
    };
    let chain = vec![TransformOp::Clap(clap_rect), TransformOp::Irot { steps: 1 }];
    let (w, h) = compute_post_transform_extent(256, 256, &chain).unwrap();
    assert_eq!((w, h), (100, 200), "post-crop 200x100 then 90° → 100x200");
}

#[test]
fn compute_post_transform_extent_full_clap_irot_imir_chain() {
    // The test the prompt called out: 256x256 + Irot{1} + Clap{w/2,h/2}
    // → after crop to 128x128 then 90° rotation → still 128x128 (square).
    // Plus imir (no-op on dims). Verify output_extent is (128, 128).
    let clap = Clap {
        clean_aperture_width_n: 128,
        clean_aperture_width_d: 1,
        clean_aperture_height_n: 128,
        clean_aperture_height_d: 1,
        horiz_off_n: 64,
        horiz_off_d: 1,
        vert_off_n: 64,
        vert_off_d: 1,
    };
    let chain = vec![
        TransformOp::Clap(clap),
        TransformOp::Irot { steps: 1 },
        TransformOp::Imir { axis: 0 },
    ];
    let (w, h) = compute_post_transform_extent(256, 256, &chain).unwrap();
    assert_eq!((w, h), (128, 128));
}

#[test]
fn compute_post_transform_extent_clap_zero_denominator_returns_none() {
    let bad = Clap {
        clean_aperture_width_n: 128,
        clean_aperture_width_d: 0, // bad
        clean_aperture_height_n: 128,
        clean_aperture_height_d: 1,
        horiz_off_n: 0,
        horiz_off_d: 1,
        vert_off_n: 0,
        vert_off_d: 1,
    };
    let chain = vec![TransformOp::Clap(bad)];
    assert!(compute_post_transform_extent(256, 256, &chain).is_none());
}

#[test]
fn output_extent_on_identity_layout_composes_irot_then_clap() {
    // Synth: 256x256 ispe, iden item carries irot{steps=1} +
    // clap {128/1, 128/1}. output_extent should be (128, 128) per the
    // spec (clap applied first, then irot — and clap is 128x128 square
    // so the irot swap is visually equivalent).
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    // iinf: id=7 iden, id=9 hvc1 inner.
    let iinf = iinf_v0_with_v2_infes(&[(7, *b"iden", "iden"), (9, *b"hvc1", "inner")]);
    let iref = iref_one(b"dimg", 7, &[9]);

    let clap = Clap {
        clean_aperture_width_n: 128,
        clean_aperture_width_d: 1,
        clean_aperture_height_n: 128,
        clean_aperture_height_d: 1,
        horiz_off_n: 64,
        horiz_off_d: 1,
        vert_off_n: 64,
        vert_off_d: 1,
    };

    // ipco: [1]=ispe(256,256), [2]=irot{1}, [3]=clap.
    // ipma: id=7 (iden) → [2], id=9 (inner) → [1, 3]
    let iprp = iprp_with_props_and_associations(
        &[
            (b"ispe", ispe_body(256, 256)),
            (b"irot", irot_body(1)),
            (b"clap", clap_body(&clap)),
        ],
        &[(7, &[2]), (9, &[1, 3])],
    );

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(7)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let layout = d.primary_image_layout().expect("iden layout");
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta");
    let extent = layout.output_extent(fm).expect("output_extent computable");
    assert_eq!(
        extent,
        (128, 128),
        "256x256 → clap(128x128) → irot{{1}}: square stays 128x128"
    );

    // Also assert the chain shape on the layout.
    match layout {
        ImageLayout::Identity { transform, .. } => {
            assert_eq!(
                transform,
                vec![TransformOp::Clap(clap), TransformOp::Irot { steps: 1 }],
                "spec order: clap then irot"
            );
        }
        other => panic!("expected Identity, got {other:?}"),
    }
}

#[test]
fn output_extent_on_identity_layout_axis_swap_visible_for_rectangular_clap() {
    // 256x256 ispe, inner item carries clap rect 200x100 then iden
    // applies irot{1}. Expected post-clap: 200x100; after irot{1}
    // swap: (100, 200).
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[(7, *b"iden", "iden"), (9, *b"hvc1", "inner")]);
    let iref = iref_one(b"dimg", 7, &[9]);

    let clap = Clap {
        clean_aperture_width_n: 200,
        clean_aperture_width_d: 1,
        clean_aperture_height_n: 100,
        clean_aperture_height_d: 1,
        horiz_off_n: 28,
        horiz_off_d: 1,
        vert_off_n: 78,
        vert_off_d: 1,
    };

    // ipco: [1]=ispe, [2]=irot, [3]=clap.
    // iden id=7 → [irot]; inner id=9 → [ispe, clap].
    let iprp = iprp_with_props_and_associations(
        &[
            (b"ispe", ispe_body(256, 256)),
            (b"irot", irot_body(1)),
            (b"clap", clap_body(&clap)),
        ],
        &[(7, &[2]), (9, &[1, 3])],
    );

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(7)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let layout = d.primary_image_layout().expect("iden layout");
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta");
    let extent = layout.output_extent(fm).expect("output_extent computable");
    assert_eq!(
        extent,
        (100, 200),
        "post-clap 200x100 → irot{{1}} → axis-swap to (100,200)"
    );
}

#[test]
fn output_extent_on_grid_layout_returns_canvas_extent() {
    // Build a 2x2 grid 256x256 (idat-resident grid payload).
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[
        (1, *b"grid", "grid"),
        (2, *b"hvc1", "tile"),
        (3, *b"hvc1", "tile"),
        (4, *b"hvc1", "tile"),
        (5, *b"hvc1", "tile"),
    ]);
    let iref = iref_one(b"dimg", 1, &[2, 3, 4, 5]);

    // grid payload: rows-1=1, cols-1=1, w=256, h=256 (16-bit dims).
    let mut grid = vec![0u8, 0u8, 1u8, 1u8];
    grid.extend_from_slice(&256u16.to_be_bytes());
    grid.extend_from_slice(&256u16.to_be_bytes());

    // idat box.
    let mut idat = Vec::new();
    idat.extend_from_slice(&grid);

    // iloc v1 with construction_method=1 (idat), one item.
    let mut iloc = Vec::new();
    iloc.push(1); // version
    iloc.extend_from_slice(&[0, 0, 0]); // flags
    iloc.push(0x44); // offset_size=4, length_size=4
    iloc.push(0x00); // base_offset_size=0, index_size=0
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item count
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item id
    iloc.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1 (idat)
    iloc.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
                                                 // base_offset_size=0 → no base_offset
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent count
    iloc.extend_from_slice(&0u32.to_be_bytes()); // offset
    iloc.extend_from_slice(&(grid.len() as u32).to_be_bytes()); // length

    // each tile needs ispe.
    let iprp = iprp_with_props_and_associations(
        &[(b"ispe", ispe_body(128, 128))],
        &[(2, &[1]), (3, &[1]), (4, &[1]), (5, &[1])],
    );

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iloc", iloc),
        (b"idat", idat),
        (b"iref", iref),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let layout = d.primary_image_layout().expect("grid layout");
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta");
    let extent = layout.output_extent(fm).expect("output_extent computable");
    assert_eq!(extent, (256, 256));
}

// ─────────────────────── #4 — clli/mdcv/cclv on Identity layout ───────────────────────

#[test]
fn identity_layout_surfaces_clli_mdcv_cclv_on_inner_item() {
    // hvc1 primary item with ispe+clli+mdcv+cclv all attached.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "hdr-primary")]);

    let iprp = iprp_with_props_and_associations(
        &[
            (b"ispe", ispe_body(3840, 2160)),
            (b"clli", clli_body(4000, 400)),
            (b"mdcv", mdcv_body()),
            (b"cclv", cclv_body_max_only(8_888_888)),
        ],
        &[(1, &[1, 2, 3, 4])],
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
    let layout = d.primary_image_layout().expect("primary layout");
    match layout {
        ImageLayout::Identity {
            item_id,
            clli,
            mdcv,
            cclv,
            ..
        } => {
            assert_eq!(item_id, 1);
            let c = clli.expect("clli surfaced");
            assert_eq!(c.max_content_light_level, 4000);
            assert_eq!(c.max_pic_average_light_level, 400);
            let m = mdcv.expect("mdcv surfaced");
            assert_eq!(m.display_primaries[2], (35400, 14600), "R primary");
            assert_eq!(m.white_point, (15635, 16450));
            assert_eq!(m.max_display_luminance, 10_000_000);
            assert_eq!(m.min_display_luminance, 500);
            let v = cclv.expect("cclv surfaced");
            assert_eq!(v.max_luminance, Some(8_888_888));
            assert!(v.min_luminance.is_none());
        }
        other => panic!("expected Identity, got {other:?}"),
    }
}

#[test]
fn identity_layout_with_no_hdr_metadata_yields_none_for_each() {
    // Plain hvc1 + ispe; no HDR properties.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));
    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "sdr-primary")]);
    let iprp = iprp_with_props_and_associations(&[(b"ispe", ispe_body(64, 64))], &[(1, &[1])]);
    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    match d.primary_image_layout().expect("layout") {
        ImageLayout::Identity {
            clli,
            mdcv,
            cclv,
            amve,
            ..
        } => {
            assert!(clli.is_none());
            assert!(mdcv.is_none());
            assert!(cclv.is_none());
            assert!(amve.is_none());
        }
        other => panic!("expected Identity, got {other:?}"),
    }
}

// ─────────────────────── #5 — amve typed extraction + tmap ───────────────────────

#[test]
fn parse_amve_payload_decodes_fullbox_form_round_trip() {
    let body = amve_body();
    assert_eq!(body.len(), 12);
    let amve = parse_amve_payload(&body).unwrap();
    assert_eq!(amve.ambient_illuminance, 314_150_000);
    assert_eq!(amve.ambient_light_x, 15635);
    assert_eq!(amve.ambient_light_y, 16450);
}

#[test]
fn iprp_amve_surfaces_on_identity_layout() {
    // Primary hvc1 + amve association.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));
    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "ambient-primary")]);
    let iprp = iprp_with_props_and_associations(
        &[(b"ispe", ispe_body(64, 64)), (b"amve", amve_body())],
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
    match d.primary_image_layout().expect("layout") {
        ImageLayout::Identity { amve, .. } => {
            let a = amve.expect("amve surfaced");
            assert_eq!(a.ambient_illuminance, 314_150_000);
            assert_eq!(a.ambient_light_x, 15635);
            assert_eq!(a.ambient_light_y, 16450);
        }
        other => panic!("expected Identity, got {other:?}"),
    }
}

#[test]
fn parse_tmap_payload_preserves_body_bytes_verbatim() {
    let raw = vec![0xDEu8, 0xAD, 0xBE, 0xEF, 0x12, 0x34];
    let tm = parse_tmap_payload(&raw).unwrap();
    assert_eq!(tm.bytes, raw);
}

#[test]
fn tmap_primary_item_surfaces_as_tonemap_layout_with_base() {
    // tmap derivation item id=1 with single dimg target id=2 (the base
    // HDR item). idat-resident algorithm payload of 6 bytes.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"tmap", "tmap-deriv"), (2, *b"hvc1", "hdr-base")]);
    let iref = iref_one(b"dimg", 1, &[2]);

    // idat: 6-byte algorithm payload.
    let algo = vec![0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06];
    let mut idat = Vec::new();
    idat.extend_from_slice(&algo);

    // iloc v1 idat for item 1 only. Use offset_size=4, length_size=4,
    // base_offset_size=0, index_size=0.
    let mut iloc = Vec::new();
    iloc.push(1); // version
    iloc.extend_from_slice(&[0, 0, 0]); // flags
    iloc.push(0x44); // offset_size=4, length_size=4
    iloc.push(0x00); // base_offset_size=0, index_size=0
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item_count
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item id
    iloc.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1 (idat)
    iloc.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&0u32.to_be_bytes()); // extent_offset
    iloc.extend_from_slice(&(algo.len() as u32).to_be_bytes()); // extent_length

    // Base item (id=2) needs an ispe so output_extent on the ToneMap
    // can defer to the base extent.
    let iprp = iprp_with_props_and_associations(&[(b"ispe", ispe_body(1920, 1080))], &[(2, &[1])]);

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iloc", iloc),
        (b"idat", idat),
        (b"iref", iref),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let layout = d.primary_image_layout().expect("ToneMap layout");
    match layout {
        ImageLayout::ToneMap {
            item_id,
            base,
            params,
        } => {
            assert_eq!(item_id, 1);
            assert_eq!(base, 2, "tmap dimg → base hvc1 item");
            assert_eq!(params.bytes, algo, "algorithm payload preserved verbatim");
        }
        other => panic!("expected ToneMap, got {other:?}"),
    }

    // ToneMap's output_extent defers to the base item's extent.
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta");
    let layout = d.primary_image_layout().expect("layout");
    let extent = layout.output_extent(fm).expect("base extent reachable");
    assert_eq!(extent, (1920, 1080));
}

#[test]
fn image_layout_for_non_primary_tmap_item_also_surfaces_tonemap() {
    // Dual-item file: primary is the base hvc1 (id=2), and id=3 is a
    // tmap derivation pointing at id=2. Confirm image_layout_for(_, 3)
    // → ToneMap.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_with_compat(b"heic", &[b"mif1"]));

    let iinf = iinf_v0_with_v2_infes(&[(2, *b"hvc1", "base"), (3, *b"tmap", "tmap-secondary")]);
    let iref = iref_one(b"dimg", 3, &[2]);

    let algo = vec![0xAAu8, 0xBB];
    let mut iloc = Vec::new();
    iloc.push(1); // version
    iloc.extend_from_slice(&[0, 0, 0]); // flags
    iloc.push(0x44);
    iloc.push(0x00);
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item_count
    iloc.extend_from_slice(&3u16.to_be_bytes()); // item id (tmap)
    iloc.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1
    iloc.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&0u32.to_be_bytes()); // extent_offset
    iloc.extend_from_slice(&(algo.len() as u32).to_be_bytes());

    let iprp = iprp_with_props_and_associations(&[(b"ispe", ispe_body(2, 2))], &[(2, &[1])]);

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(2)),
        (b"iinf", iinf),
        (b"iloc", iloc),
        (b"idat", algo),
        (b"iref", iref),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().expect("bmff meta");
    let tmap = image_layout_for(fm, 3).expect("tmap layout for non-primary id");
    match tmap {
        ImageLayout::ToneMap { base, .. } => assert_eq!(base, 2),
        other => panic!("expected ToneMap, got {other:?}"),
    }
}

// ─────────────────────── shape sanity: enum constructors used ───────────────────────

#[test]
fn unused_enum_constructor_smoke_check_for_amve_and_tonemap_types() {
    // Ensure the public types are constructible with their documented
    // fields — this is the equivalent of a `cargo doc` shape gate.
    let _a = Amve {
        ambient_illuminance: 1,
        ambient_light_x: 2,
        ambient_light_y: 3,
    };
    let _c = Clli {
        max_content_light_level: 1,
        max_pic_average_light_level: 2,
    };
    let _m: Mdcv = Default::default();
    let _r = Irot { steps: 0 };
    let _i = Imir { axis: 0 };
}
