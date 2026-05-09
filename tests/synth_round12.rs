//! Round-12 acceptance: HEIF derivation payloads resolved from
//! `mdat` (`construction_method == 0`) and per-tile / per-layer
//! `ispe` validation surfaced on the layout plan.
//!
//! r10/r11 only resolved derivation payloads (`grid` / `iovl`) when
//! they lived in the meta box's `idat`. Spec-compliant authoring may
//! also place them at file offsets (the typical home is `mdat`); the
//! r12 demuxer surface
//! ([`MovDemuxer::primary_image_layout_with_input`]) reads such
//! payloads from the input.
//!
//! r12 also walks every tile / layer's `iprp/ispe` and surfaces
//! mismatches against the canonical first-tile extent in
//! [`ImageGridLayout::tile_size_warnings`]. The renderer's existing
//! `render_grid` enforces the same rule on the decoded-buffer side; the
//! plan-time warning lets validators catch the malformed authoring
//! shape without decoding.
//!
//! Spec references:
//! - ISO/IEC 14496-12:2015 §8.11.3 (ItemLocationBox + construction_method).
//! - ISO/IEC 23008-12:2017 §6.6.2.3.3 (every grid tile shares ispe).
//! - ISO/IEC 23008-12:2017 §6.5.3 (every contributing item carries ispe).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::push_atom;
use oxideav_core::ReadSeek;
use oxideav_mov::{ImageLayout, MovDemuxer};

// ─────────────────────── helpers ───────────────────────

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

/// Build `iloc` v1 with one item at `construction_method == 0`
/// (file extent), pointing at absolute file offset `file_offset` for
/// `payload_len` bytes. base_offset_size=4.
fn iloc_v1_file_one(item_id: u16, file_offset: u32, payload_len: u32) -> Vec<u8> {
    let mut iloc = Vec::new();
    iloc.push(1); // version
    iloc.extend_from_slice(&[0, 0, 0]); // flags
    iloc.push(0x44); // offset_size=4, length_size=4
    iloc.push(0x40); // base_offset_size=4, index_size=0
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item_count
    iloc.extend_from_slice(&item_id.to_be_bytes()); // item_id
    iloc.extend_from_slice(&0u16.to_be_bytes()); // construction_method=0 (file)
    iloc.extend_from_slice(&0u16.to_be_bytes()); // dref_index
    iloc.extend_from_slice(&file_offset.to_be_bytes()); // base_offset
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&0u32.to_be_bytes()); // offset (relative to base)
    iloc.extend_from_slice(&payload_len.to_be_bytes());
    iloc
}

/// Build `iprp` carrying one shared `ispe` (`tile_w` × `tile_h`),
/// associated with every item id in `tile_ids`.
fn iprp_shared_ispe(tile_w: u32, tile_h: u32, tile_ids: &[u16]) -> Vec<u8> {
    let mut ispe_body = Vec::new();
    ispe_body.extend_from_slice(&0u32.to_be_bytes());
    ispe_body.extend_from_slice(&tile_w.to_be_bytes());
    ispe_body.extend_from_slice(&tile_h.to_be_bytes());
    let mut ipco = Vec::new();
    push_atom(&mut ipco, *b"ispe", &ispe_body);
    let mut ipma = Vec::new();
    ipma.extend_from_slice(&0u32.to_be_bytes());
    ipma.extend_from_slice(&(tile_ids.len() as u32).to_be_bytes());
    for &id in tile_ids {
        ipma.extend_from_slice(&id.to_be_bytes());
        ipma.push(1);
        ipma.push(0x81);
    }
    let mut iprp = Vec::new();
    push_atom(&mut iprp, *b"ipco", &ipco);
    push_atom(&mut iprp, *b"ipma", &ipma);
    iprp
}

/// Build `iprp` with TWO `ispe`s (canonical at idx 1, deviant at idx 2),
/// associating tiles to canonical except the deviant index.
fn iprp_two_ispes_with_deviant(
    canonical_w: u32,
    canonical_h: u32,
    deviant_w: u32,
    deviant_h: u32,
    tile_ids: &[u16],
    deviant_idx: usize,
) -> Vec<u8> {
    let mut ispe1 = Vec::new();
    ispe1.extend_from_slice(&0u32.to_be_bytes());
    ispe1.extend_from_slice(&canonical_w.to_be_bytes());
    ispe1.extend_from_slice(&canonical_h.to_be_bytes());
    let mut ispe2 = Vec::new();
    ispe2.extend_from_slice(&0u32.to_be_bytes());
    ispe2.extend_from_slice(&deviant_w.to_be_bytes());
    ispe2.extend_from_slice(&deviant_h.to_be_bytes());
    let mut ipco = Vec::new();
    push_atom(&mut ipco, *b"ispe", &ispe1);
    push_atom(&mut ipco, *b"ispe", &ispe2);
    let mut ipma = Vec::new();
    ipma.extend_from_slice(&0u32.to_be_bytes());
    ipma.extend_from_slice(&(tile_ids.len() as u32).to_be_bytes());
    for (i, &id) in tile_ids.iter().enumerate() {
        let idx: u8 = if i == deviant_idx { 0x82 } else { 0x81 };
        ipma.extend_from_slice(&id.to_be_bytes());
        ipma.push(1);
        ipma.push(idx);
    }
    let mut iprp = Vec::new();
    push_atom(&mut iprp, *b"ipco", &ipco);
    push_atom(&mut iprp, *b"ipma", &ipma);
    iprp
}

fn grid16_payload(rows_minus_one: u8, cols_minus_one: u8, w: u16, h: u16) -> Vec<u8> {
    let mut p = vec![0u8, 0, rows_minus_one, cols_minus_one];
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    p
}

fn ftyp_mif1() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"mif1");
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(b"mif1");
    p
}

// ─────────────────────── A. mdat-resident grid payload ───────────────────────

#[test]
fn primary_image_layout_with_input_resolves_mdat_resident_grid_payload() {
    // Construct a HEIF file where the `grid` payload sits in `mdat`
    // (construction_method == 0) rather than `idat`. The canonical
    // primary_image_layout() should return None (it's idat-only); the
    // _with_input variant resolves through the file extent.
    //
    // Layout on the wire:
    //   ftyp
    //   meta (no idat — just hdlr / pitm / iinf / iref / iprp / iloc)
    //   mdat
    //     [8-byte payload: grid 2×2 → 128×128]
    //
    // The iloc points at the absolute mdat-payload offset.

    let payload = grid16_payload(1, 1, 128, 128);

    // First, build the meta box body sans iloc, and with a placeholder
    // iloc whose file_offset we'll fix up once we know where mdat starts.
    let iinf = iinf_v0_with_v2_infes(&[
        (1, *b"grid", "primary"),
        (10, *b"hvc1", "tile-0"),
        (11, *b"hvc1", "tile-1"),
        (12, *b"hvc1", "tile-2"),
        (13, *b"hvc1", "tile-3"),
    ]);
    let iref = iref_dimg(1, &[10, 11, 12, 13]);
    let iprp = iprp_shared_ispe(64, 64, &[10, 11, 12, 13]);

    // Two-pass build: first pass with offset=0 to compute the fixed
    // size of meta, then re-emit with the real mdat-payload offset.
    let mut probe = Vec::new();
    push_atom(&mut probe, *b"ftyp", &ftyp_mif1());
    let probe_iloc = iloc_v1_file_one(1, 0, payload.len() as u32);
    let probe_meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf.clone()),
        (b"iref", iref.clone()),
        (b"iprp", iprp.clone()),
        (b"iloc", probe_iloc),
    ]);
    push_atom(&mut probe, *b"meta", &probe_meta_body);
    // After meta we'd emit mdat header + payload; mdat payload starts at
    // probe.len() + 8 (the mdat header).
    let mdat_payload_offset = probe.len() as u32 + 8;

    // Second pass with the correct file_offset baked in.
    let real_iloc = iloc_v1_file_one(1, mdat_payload_offset, payload.len() as u32);
    let real_meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
        (b"iloc", real_iloc),
    ]);

    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());
    push_atom(&mut out, *b"meta", &real_meta_body);
    push_atom(&mut out, *b"mdat", &payload);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let mut d = MovDemuxer::open(cur).unwrap();

    // The pure-meta resolver returns None: the iloc has
    // construction_method=0, so idat_bytes_concat finds nothing.
    assert!(d.primary_image_layout().is_none());

    // The _with_input variant reads from mdat and lands the plan.
    let layout = d
        .primary_image_layout_with_input()
        .expect("expected mdat-resident grid plan");
    match layout {
        ImageLayout::Grid(g) => {
            assert_eq!(g.canvas_w, 128);
            assert_eq!(g.canvas_h, 128);
            assert_eq!(g.tile_w, 64);
            assert_eq!(g.tile_h, 64);
            assert_eq!(g.tiles.len(), 4);
            assert!(g.tile_size_warnings.is_empty());
        }
        other => panic!("expected ImageLayout::Grid, got {other:?}"),
    }
}

#[test]
fn primary_image_layout_with_input_resolves_mdat_resident_iovl_payload() {
    // Same shape as the grid mdat test but for an iovl. Two layers,
    // 16-bit dims+offsets shape, with a per-layer ispe (so the
    // resulting layers carry (w, h) instead of zeros).
    let mut payload = Vec::new();
    payload.push(0); // version
    payload.push(0); // flags
    for c in [0u16, 0, 0, 65535] {
        payload.extend_from_slice(&c.to_be_bytes());
    }
    payload.extend_from_slice(&256u16.to_be_bytes()); // out_w
    payload.extend_from_slice(&256u16.to_be_bytes()); // out_h
    payload.extend_from_slice(&0i16.to_be_bytes());
    payload.extend_from_slice(&0i16.to_be_bytes());
    payload.extend_from_slice(&96i16.to_be_bytes());
    payload.extend_from_slice(&96i16.to_be_bytes());

    let iinf = iinf_v0_with_v2_infes(&[
        (1, *b"iovl", "primary"),
        (20, *b"hvc1", "layer-0"),
        (21, *b"hvc1", "layer-1"),
    ]);
    let iref = iref_dimg(1, &[20, 21]);
    let iprp = iprp_two_ispes_with_deviant(64, 64, 32, 16, &[20, 21], 1);

    // Probe pass for offset.
    let mut probe = Vec::new();
    push_atom(&mut probe, *b"ftyp", &ftyp_mif1());
    let probe_iloc = iloc_v1_file_one(1, 0, payload.len() as u32);
    let probe_meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf.clone()),
        (b"iref", iref.clone()),
        (b"iprp", iprp.clone()),
        (b"iloc", probe_iloc),
    ]);
    push_atom(&mut probe, *b"meta", &probe_meta_body);
    let mdat_payload_offset = probe.len() as u32 + 8;

    let real_iloc = iloc_v1_file_one(1, mdat_payload_offset, payload.len() as u32);
    let real_meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
        (b"iloc", real_iloc),
    ]);

    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());
    push_atom(&mut out, *b"meta", &real_meta_body);
    push_atom(&mut out, *b"mdat", &payload);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let mut d = MovDemuxer::open(cur).unwrap();
    assert!(d.primary_image_layout().is_none());

    let layout = d
        .primary_image_layout_with_input()
        .expect("expected mdat-resident overlay plan");
    match layout {
        ImageLayout::Overlay(o) => {
            assert_eq!(o.canvas_w, 256);
            assert_eq!(o.canvas_h, 256);
            assert_eq!(o.layers.len(), 2);
            // Per-layer ispe (w, h) plumbed through.
            assert_eq!((o.layers[0].w, o.layers[0].h), (64, 64));
            assert_eq!((o.layers[1].w, o.layers[1].h), (32, 16));
            // Both layers have ispe → no warnings.
            assert!(o.layer_size_warnings.is_empty());
        }
        other => panic!("expected ImageLayout::Overlay, got {other:?}"),
    }
}

// ─────────────────────── B. per-tile ispe validation ───────────────────────

#[test]
fn primary_image_layout_grid_surfaces_per_tile_ispe_mismatch() {
    // Build a 2×2 grid where tile 13 has a deviant 30×64 ispe while
    // 10/11/12 share the canonical 64×64 ispe (typical right-edge
    // truncation). The plan should still build, but
    // tile_size_warnings names tile 13 with expected (64, 64) vs
    // actual (30, 64).

    let payload = grid16_payload(1, 1, 128, 128);
    // Build idat-based meta (single file_offset isn't required for
    // this test; we exercise the validation path through plan_grid_layout).
    let iinf = iinf_v0_with_v2_infes(&[
        (1, *b"grid", "primary"),
        (10, *b"hvc1", "tile-0"),
        (11, *b"hvc1", "tile-1"),
        (12, *b"hvc1", "tile-2"),
        (13, *b"hvc1", "tile-3"),
    ]);
    let iref = iref_dimg(1, &[10, 11, 12, 13]);
    let iprp = iprp_two_ispes_with_deviant(64, 64, 30, 64, &[10, 11, 12, 13], 3);

    // idat-resident payload — keep test simple.
    let mut iloc = Vec::new();
    iloc.push(1); // version
    iloc.extend_from_slice(&[0, 0, 0]); // flags
    iloc.push(0x44);
    iloc.push(0x00);
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item_count
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item_id
    iloc.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1
    iloc.extend_from_slice(&0u16.to_be_bytes()); // dref_index
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&0u32.to_be_bytes());
    iloc.extend_from_slice(&(payload.len() as u32).to_be_bytes());

    let body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
        (b"iloc", iloc),
        (b"idat", payload),
    ]);
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());
    push_atom(&mut out, *b"meta", &body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let layout = d.primary_image_layout().expect("expected ImageLayout");
    match layout {
        ImageLayout::Grid(g) => {
            assert_eq!(g.tile_w, 64);
            assert_eq!(g.tile_h, 64);
            // Per-slot extents: 0..2 canonical, 3 deviant.
            assert_eq!((g.tiles[0].w, g.tiles[0].h), (64, 64));
            assert_eq!((g.tiles[3].w, g.tiles[3].h), (30, 64));
            // One mismatch warning for the deviant tile.
            assert_eq!(g.tile_size_warnings.len(), 1);
            assert_eq!(g.tile_size_warnings[0].item_id, 13);
            assert_eq!(
                (
                    g.tile_size_warnings[0].expected_w,
                    g.tile_size_warnings[0].expected_h
                ),
                (64, 64)
            );
            assert_eq!(
                (
                    g.tile_size_warnings[0].actual_w,
                    g.tile_size_warnings[0].actual_h
                ),
                (30, 64)
            );
        }
        other => panic!("expected ImageLayout::Grid, got {other:?}"),
    }
}
