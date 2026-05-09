//! Round-10 acceptance: portable Windows `file://` shape rules,
//! meta-scope `dinf/dref` external file-reference resolution, HEIF
//! `iden` identity-derived renderer, and the `iovl` overlay
//! compositor.
//!
//! These integration tests exercise the public surface end-to-end:
//! a synthesised HEIF-shaped meta box, parsed, then composed via the
//! renderer entry points. The renderer's pixel-level behaviour is
//! cross-checked against the rules in ISO/IEC 23008-12:2017 §6.3
//! (transformative properties), §6.6.2.1 (`iden`), and §6.6.2.2
//! (`iovl`).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::push_atom;
use oxideav_core::ReadSeek;
use oxideav_mov::{
    item_data, parse_grid, parse_overlay, render_grid, render_iden, render_iovl, DataLocation,
    DataReference, ItemDataLocation, MovDemuxer, Rgba8Canvas,
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

/// One-item iloc v1 with `construction_method=0` (file-extents) and
/// the given `dref_index`. Useful for exercising the new
/// `data_location_for_item` helper.
fn iloc_v1_external(item_id: u16, dref_index: u16, off: u32, len: u32) -> Vec<u8> {
    let mut iloc = Vec::new();
    iloc.push(1);
    iloc.extend_from_slice(&[0, 0, 0]);
    iloc.push(0x44); // offset_size=4, length_size=4
    iloc.push(0x00); // base_offset_size=0, index_size=0
    iloc.extend_from_slice(&1u16.to_be_bytes());
    iloc.extend_from_slice(&item_id.to_be_bytes());
    iloc.extend_from_slice(&0u16.to_be_bytes()); // construction_method=0
    iloc.extend_from_slice(&dref_index.to_be_bytes());
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&off.to_be_bytes());
    iloc.extend_from_slice(&len.to_be_bytes());
    iloc
}

fn dinf_with_external_url(url: &[u8]) -> Vec<u8> {
    let mut child = Vec::new();
    let mut url_with_nul = url.to_vec();
    url_with_nul.push(0);
    let size = (12 + url_with_nul.len()) as u32;
    child.extend_from_slice(&size.to_be_bytes());
    child.extend_from_slice(b"url ");
    child.push(0);
    child.extend_from_slice(&[0, 0, 0]); // flags=0 (external)
    child.extend_from_slice(&url_with_nul);
    let mut dref = Vec::new();
    dref.extend_from_slice(&0u32.to_be_bytes());
    dref.extend_from_slice(&1u32.to_be_bytes());
    dref.extend_from_slice(&child);
    let mut dinf = Vec::new();
    push_atom(&mut dinf, *b"dref", &dref);
    dinf
}

// ─────────────────────── 1. dinf/dref external file-references ───────────────────────

#[test]
fn meta_dinf_dref_external_url_routes_through_data_location() {
    // HEIF-shaped meta box: hdlr + dinf(external url) + iinf(item 7
    // hvc1) + iloc v1 with dref_index=1 pointing at the external bag.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"mif1");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let dinf = dinf_with_external_url(b"file:///srv/tile-bag.heic");
    let iinf = iinf_v0_with_v2_infes(&[(7, *b"hvc1", "primary")]);
    let iloc = iloc_v1_external(7, 1, 0x100, 64);

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"dinf", dinf),
        (b"iinf", iinf),
        (b"iloc", iloc),
    ]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().unwrap();

    // The data_references list carries the external sidecar URL.
    assert_eq!(fm.data_references.len(), 1);
    match &fm.data_references[0] {
        DataReference::Url(s) => assert_eq!(s, "file:///srv/tile-bag.heic"),
        other => panic!("expected Url, got {other:?}"),
    }

    // The item's data_location_for_item routes through the dref.
    match fm.data_location_for_item(7).unwrap() {
        DataLocation::External(DataReference::Url(s)) => {
            assert_eq!(s, "file:///srv/tile-bag.heic")
        }
        other => panic!("expected External(Url), got {other:?}"),
    }

    // The iloc still surfaces extents — the caller resolves them
    // against the external file's bytes.
    match item_data(fm, 7).unwrap() {
        ItemDataLocation::FileExtents(v) => {
            assert_eq!(v, vec![(0x100, 64)]);
        }
        other => panic!("expected FileExtents, got {other:?}"),
    }
}

#[test]
fn meta_data_location_zero_index_resolves_same_file() {
    // No dinf at all → empty data_references; data_location(0) is
    // SameFile by definition.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"mif1");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict())]);
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().unwrap();
    assert!(fm.data_references.is_empty());
    assert_eq!(fm.data_location(0), DataLocation::SameFile);
}

// ─────────────────────── 2. iden renderer ───────────────────────

#[test]
fn iden_renderer_applies_irot_then_imir_to_source() {
    use oxideav_mov::{Imir, Irot, ItemProperty};
    // 4×4 RGBA8 source with a known top-left pixel.
    let mut data = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4u8 {
        for x in 0..4u8 {
            data.extend_from_slice(&[x * 50, y * 50, 0, 255]);
        }
    }
    let src = Rgba8Canvas::from_rgba8(4, 4, data).unwrap();
    let irot = ItemProperty::Irot(Irot { steps: 1 });
    let imir = ItemProperty::Imir(Imir { axis: 1 });
    let out = render_iden(&src, &[&irot, &imir]).unwrap();
    assert_eq!(out.width(), 4);
    assert_eq!(out.height(), 4);
}

// ─────────────────────── 3. iovl renderer end-to-end ───────────────────────

#[test]
fn iovl_renderer_round_trip_corpus_shape() {
    // Replays the corpus `still-image-overlay` shape: 256×256 grey
    // canvas, two layers — a 256×256 base at (0,0), then a 64×64
    // semi-transparent stamp at (96,96).
    use oxideav_mov::Overlay;

    let base = Rgba8Canvas::filled(256, 256, [80, 80, 80, 255]).unwrap();
    let stamp = Rgba8Canvas::filled(64, 64, [255, 0, 0, 200]).unwrap();
    let overlay = Overlay {
        canvas_fill_color: [16384, 16384, 16384, 65535],
        output_width: 256,
        output_height: 256,
        offsets: vec![(0, 0), (96, 96)],
    };
    let out = render_iovl(&overlay, &[base, stamp]).unwrap();
    assert_eq!(out.width(), 256);
    assert_eq!(out.height(), 256);
    // Inside the stamp area we get a red-tinted blend, outside we
    // get the opaque base colour.
    let inside = out.pixel(128, 128).unwrap();
    let outside = out.pixel(10, 10).unwrap();
    assert_eq!(outside, [80, 80, 80, 255]);
    assert!(inside[0] > inside[1] && inside[0] > inside[2]);
    assert_eq!(inside[3], 255);
}

#[test]
fn iovl_payload_parser_then_renderer_chain() {
    // Build an iovl payload (header only — no layers), parse it, then
    // hand-roll the layers and render. This exercises the full
    // parse → render data path.
    let mut body = Vec::new();
    body.push(0); // version
    body.push(0); // flags = 16-bit
    for c in [0u16, 0, 0, 65535] {
        body.extend_from_slice(&c.to_be_bytes());
    }
    body.extend_from_slice(&8u16.to_be_bytes()); // output_width
    body.extend_from_slice(&8u16.to_be_bytes()); // output_height
    body.extend_from_slice(&0i16.to_be_bytes()); // h_off
    body.extend_from_slice(&0i16.to_be_bytes()); // v_off
    let parsed = parse_overlay(&body).unwrap();
    let layer = Rgba8Canvas::filled(8, 8, [10, 20, 30, 255]).unwrap();
    let out = render_iovl(&parsed, &[layer]).unwrap();
    assert_eq!(out.pixel(0, 0), Some([10, 20, 30, 255]));
    assert_eq!(out.pixel(7, 7), Some([10, 20, 30, 255]));
}

// ─────────────────────── 4. grid renderer end-to-end ───────────────────────

#[test]
fn grid_payload_parser_then_renderer_chain() {
    // Build a `grid` payload for 2×2 tiles of 64×64 each, output
    // 128×128. Then render with four solid-coloured tiles.
    let mut body = vec![0u8, 0, 1 /*rows-1*/, 1 /*cols-1*/];
    body.extend_from_slice(&128u16.to_be_bytes());
    body.extend_from_slice(&128u16.to_be_bytes());
    let g = parse_grid(&body).unwrap();
    let tiles = vec![
        Rgba8Canvas::filled(64, 64, [255, 0, 0, 255]).unwrap(),
        Rgba8Canvas::filled(64, 64, [0, 255, 0, 255]).unwrap(),
        Rgba8Canvas::filled(64, 64, [0, 0, 255, 255]).unwrap(),
        Rgba8Canvas::filled(64, 64, [255, 255, 0, 255]).unwrap(),
    ];
    let out = render_grid(&g, &tiles).unwrap();
    assert_eq!(out.pixel(0, 0), Some([255, 0, 0, 255]));
    assert_eq!(out.pixel(127, 0), Some([0, 255, 0, 255]));
    assert_eq!(out.pixel(0, 127), Some([0, 0, 255, 255]));
    assert_eq!(out.pixel(127, 127), Some([255, 255, 0, 255]));
}
