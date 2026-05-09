//! Round-17 acceptance: long-pending typed-extraction gaps closed and
//! r16's `resolve_item_bytes` wired into the layout planner.
//!
//! r17 closes three holes the previous rounds tracked as TODOs:
//!
//! * **#1** — `lsel` (LayerSelector, HEIF / ISO/IEC 23008-12 §6.5.11)
//!   typed extraction. The 2-byte / 6-byte (FullBox-prefixed) payload
//!   was previously caught by the `Other` fall-through; r17 surfaces a
//!   typed [`oxideav_mov::LayerSelector`] variant on `ItemProperty` +
//!   plumbs it through to `ImageLayout::Identity { …, lsel }`.
//! * **#3** — `ipro` (ItemProtectionBox, ISO/IEC 14496-12 §8.11.5)
//!   typed surface. The previously-skipped meta child now lands a
//!   parsed [`oxideav_mov::ItemProtection`] with one
//!   [`oxideav_mov::ProtectionScheme`] per `sinf` child (preserving
//!   `frma` original-format + `schm` scheme_type / version + raw `sinf`
//!   bytes for downstream DRM-aware callers).
//! * **#5** — r16's `resolve_item_bytes` is wired into
//!   [`oxideav_mov::MovDemuxer::primary_image_layout_with_input`] so a
//!   `grid` primary whose payload bytes live at `construction_method ==
//!   2` (sub-slice of another item) lands a `Grid` plan transparently.
//!
//! Spec references:
//! - ISO/IEC 23008-12:2017 §6.5.11 (LayerSelectorProperty).
//! - ISO/IEC 14496-12:2015 §8.11.5 (ItemProtectionBox) + §8.12
//!   (ProtectionSchemeInfoBox / SchemeTypeBox / SchemeInformationBox).
//! - ISO/IEC 14496-12:2015 §8.11.3 (ItemLocationBox cm=2).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::push_atom;
use oxideav_core::ReadSeek;
use oxideav_mov::{ImageLayout, ItemProperty, MovDemuxer};

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

fn iref_many(refs: &[(&[u8; 4], u16, Vec<u16>)]) -> Vec<u8> {
    let mut iref = Vec::new();
    iref.extend_from_slice(&0u32.to_be_bytes());
    for (kind, from_id, to_ids) in refs {
        let mut sirb = Vec::new();
        sirb.extend_from_slice(&from_id.to_be_bytes());
        sirb.extend_from_slice(&(to_ids.len() as u16).to_be_bytes());
        for &id in to_ids {
            sirb.extend_from_slice(&id.to_be_bytes());
        }
        let size = (8 + sirb.len()) as u32;
        iref.extend_from_slice(&size.to_be_bytes());
        iref.extend_from_slice(*kind);
        iref.extend_from_slice(&sirb);
    }
    iref
}

fn ftyp_mif1() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"mif1");
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(b"mif1");
    p
}

fn ispe_body(w: u32, h: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    p
}

/// Build an `iprp` carrying the given sequence of property bodies in
/// `ipco` order, plus an `ipma` row associating each `(item_id,
/// indices_1based)` listed.
fn iprp_with_props(
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

// ─────────────────────── #1 — lsel typed extraction ───────────────────────

#[test]
fn primary_image_layout_identity_surfaces_lsel_layer_id() {
    // Synth: hvc1 primary item with ispe + lsel{layer_id=2}. The
    // Identity layout the planner builds must surface lsel: Some(2).
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());

    let iinf = iinf_v0_with_v2_infes(&[(5, *b"hvc1", "primary")]);
    // ipco: [ispe, lsel] — both associated with item 5.
    let lsel_body = 2u16.to_be_bytes().to_vec();
    let iprp = iprp_with_props(
        &[(b"ispe", ispe_body(64, 64)), (b"lsel", lsel_body)],
        &[(5, &[1u8, 2])],
    );

    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(5)),
        (b"iinf", iinf),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let dx = MovDemuxer::open(cur).unwrap();
    let layout = dx.primary_image_layout().expect("identity layout");
    match layout {
        ImageLayout::Identity { item_id, lsel, .. } => {
            assert_eq!(item_id, 5);
            let l = lsel.expect("lsel surfaced on Identity layout");
            assert_eq!(l.layer_id, 2);
        }
        other => panic!("expected Identity, got {other:?}"),
    }
}

#[test]
fn primary_image_layout_identity_lsel_none_when_no_lsel_associated() {
    // Bare hvc1 primary with only ispe → lsel field is None.
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());
    let iinf = iinf_v0_with_v2_infes(&[(5, *b"hvc1", "primary")]);
    let iprp = iprp_with_props(&[(b"ispe", ispe_body(64, 64))], &[(5, &[1u8])]);
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(5)),
        (b"iinf", iinf),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let dx = MovDemuxer::open(cur).unwrap();
    match dx.primary_image_layout().expect("identity layout") {
        ImageLayout::Identity { lsel, .. } => assert!(lsel.is_none()),
        other => panic!("expected Identity, got {other:?}"),
    }
}

#[test]
fn parsed_iprp_lsel_lands_on_typed_variant_not_other() {
    // Confirm that walking the parsed iprp surfaces ItemProperty::Lsel
    // (not ItemProperty::Other { fourcc: b"lsel", .. }).
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());

    let iinf = iinf_v0_with_v2_infes(&[(5, *b"hvc1", "primary")]);
    let lsel_body = 7u16.to_be_bytes().to_vec();
    let iprp = iprp_with_props(&[(b"lsel", lsel_body)], &[(5, &[1u8])]);
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(5)),
        (b"iinf", iinf),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let dx = MovDemuxer::open(cur).unwrap();
    let meta = dx.file_bmff_meta.as_ref().expect("meta");
    let props = meta.properties.as_ref().expect("iprp parsed");
    assert_eq!(props.properties.len(), 1);
    match &props.properties[0] {
        ItemProperty::Lsel(l) => assert_eq!(l.layer_id, 7),
        other => panic!("expected ItemProperty::Lsel, got {other:?}"),
    }
    let resolved = props.resolve(5);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].fourcc(), *b"lsel");
}

// ─────────────────────── #3 — ipro typed surface ───────────────────────

fn build_sinf_cenc(original_format: &[u8; 4]) -> Vec<u8> {
    let mut sinf = Vec::new();
    push_atom(&mut sinf, *b"frma", original_format);
    let mut schm = Vec::new();
    schm.push(0); // version
    schm.extend_from_slice(&[0, 0, 0]); // flags = 0 → no URI
    schm.extend_from_slice(b"cenc");
    schm.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    push_atom(&mut sinf, *b"schm", &schm);
    sinf
}

#[test]
fn ipro_surface_carries_one_cenc_scheme_typed() {
    // Synthetic HEIF with a single cenc scheme in ipro. The accessor
    // surfaces the parsed ProtectionScheme with scheme_type == b"cenc".
    let sinf = build_sinf_cenc(b"hvc1");
    let mut ipro = Vec::new();
    ipro.extend_from_slice(&0u32.to_be_bytes()); // FullBox ver+flags
    ipro.extend_from_slice(&1u16.to_be_bytes()); // protection_count
    push_atom(&mut ipro, *b"sinf", &sinf);

    let iinf = iinf_v0_with_v2_infes(&[(5, *b"hvc1", "primary")]);
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(5)),
        (b"iinf", iinf),
        (b"ipro", ipro),
    ]);

    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());
    push_atom(&mut out, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let dx = MovDemuxer::open(cur).unwrap();
    let meta = dx.file_bmff_meta.as_ref().expect("meta");
    let ip = meta.item_protection().expect("ipro surfaced");
    assert_eq!(ip.schemes.len(), 1);
    assert_eq!(&ip.schemes[0].scheme_type, b"cenc");
    assert_eq!(ip.schemes[0].scheme_version, 0x0001_0000);
    assert_eq!(&ip.schemes[0].original_format, b"hvc1");
    assert!(ip.schemes[0].scheme_uri.is_none());
    assert_eq!(ip.schemes[0].raw_payload, sinf);
}

#[test]
fn ipro_absent_yields_none_item_protection_accessor() {
    let iinf = iinf_v0_with_v2_infes(&[(5, *b"hvc1", "primary")]);
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(5)),
        (b"iinf", iinf),
    ]);
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_mif1());
    push_atom(&mut out, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let dx = MovDemuxer::open(cur).unwrap();
    let meta = dx.file_bmff_meta.as_ref().expect("meta");
    assert!(meta.item_protection().is_none());
}

// ─────────────────────── #5 — cm=2 grid resolves through layout ───────────────────────

/// One iloc row description used by [`build_iloc_v1`].
struct IlocRow {
    item_id: u16,
    construction_method: u16,
    base_offset: u32,
    /// Each extent is `(extent_index, offset, length)`. `extent_index`
    /// is only emitted when `index_size > 0` (parent param of
    /// [`build_iloc_v1`]).
    extents: Vec<(u32, u32, u32)>,
}

/// Build an `iloc` v1 with arbitrary rows. `offset_size = length_size =
/// 4`, `base_offset_size = 4`, `index_size` from the parameter.
fn build_iloc_v1(rows: &[IlocRow], index_size: u8) -> Vec<u8> {
    assert!(matches!(index_size, 0 | 4));
    let mut iloc = Vec::new();
    iloc.push(1); // version
    iloc.extend_from_slice(&[0, 0, 0]); // flags
    iloc.push(0x44); // offset_size=4, length_size=4
    iloc.push(0x40 | (index_size & 0x0F)); // base_offset_size=4, index_size=N
    iloc.extend_from_slice(&(rows.len() as u16).to_be_bytes()); // item_count
    for r in rows {
        iloc.extend_from_slice(&r.item_id.to_be_bytes());
        iloc.extend_from_slice(&r.construction_method.to_be_bytes());
        iloc.extend_from_slice(&0u16.to_be_bytes()); // dref_index
        iloc.extend_from_slice(&r.base_offset.to_be_bytes());
        iloc.extend_from_slice(&(r.extents.len() as u16).to_be_bytes());
        for &(idx, off, len) in &r.extents {
            if index_size == 4 {
                iloc.extend_from_slice(&idx.to_be_bytes());
            }
            iloc.extend_from_slice(&off.to_be_bytes());
            iloc.extend_from_slice(&len.to_be_bytes());
        }
    }
    iloc
}

fn grid16_payload(rows_minus_one: u8, cols_minus_one: u8, w: u16, h: u16) -> Vec<u8> {
    let mut p = vec![0u8, 0, rows_minus_one, cols_minus_one];
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    p
}

fn iprp_shared_ispe(tile_w: u32, tile_h: u32, tile_ids: &[u16]) -> Vec<u8> {
    let mut ispe_b = Vec::new();
    ispe_b.extend_from_slice(&0u32.to_be_bytes());
    ispe_b.extend_from_slice(&tile_w.to_be_bytes());
    ispe_b.extend_from_slice(&tile_h.to_be_bytes());
    let mut ipco = Vec::new();
    push_atom(&mut ipco, *b"ispe", &ispe_b);
    let mut ipma = Vec::new();
    ipma.extend_from_slice(&0u32.to_be_bytes());
    ipma.extend_from_slice(&(tile_ids.len() as u32).to_be_bytes());
    for &id in tile_ids {
        ipma.extend_from_slice(&id.to_be_bytes());
        ipma.push(1);
        ipma.push(0x81); // essential=1, idx=1
    }
    let mut iprp = Vec::new();
    push_atom(&mut iprp, *b"ipco", &ipco);
    push_atom(&mut iprp, *b"ipma", &ipma);
    iprp
}

#[test]
fn primary_image_layout_with_input_resolves_cm2_grid_payload() {
    // The primary item is a `grid` (item 1) whose 8-byte `grid` payload
    // lives at `construction_method == 2`: a sub-slice of item 99
    // (which itself sits in mdat with cm=0). Without r17 the cm=2 path
    // falls through `read_derivation_payload_bytes`'s catch-all and the
    // planner returns None; with r16's resolver wired in, it lands the
    // expected `Grid` plan.
    //
    // Wire layout:
    //   ftyp
    //   meta
    //     hdlr / pitm / iinf / iref(dimg, iloc) / iprp / iloc
    //   mdat
    //     <padding ...> <grid_payload>
    //
    // Item 99's cm=0 extent points at (mdat_off + pad_len, grid_len).
    // Item 1 (the grid primary) has cm=2 with a single extent
    // sub-slicing item 99 at (offset=0, length=grid_len).

    let payload = grid16_payload(1, 1, 128, 128); // 8 bytes
    let pad_len: u32 = 16; // padding before payload in mdat
    let payload_len = payload.len() as u32;

    let iinf = iinf_v0_with_v2_infes(&[
        (1, *b"grid", "primary"),
        (10, *b"hvc1", "tile-0"),
        (11, *b"hvc1", "tile-1"),
        (12, *b"hvc1", "tile-2"),
        (13, *b"hvc1", "tile-3"),
        (99, *b"hvc1", "src"),
    ]);
    // dimg from grid → tiles + iloc-iref from grid → src so cm=2 can
    // pick item 99 as its source.
    let iref = iref_many(&[(b"dimg", 1, vec![10, 11, 12, 13]), (b"iloc", 1, vec![99])]);
    let iprp = iprp_shared_ispe(64, 64, &[10, 11, 12, 13]);

    // Two-pass build to compute mdat payload offset.
    let mut probe = Vec::new();
    push_atom(&mut probe, *b"ftyp", &ftyp_mif1());
    let probe_iloc = build_iloc_v1(
        &[
            // item 1: cm=2, single extent (offset=0 length=payload_len) into item 99.
            IlocRow {
                item_id: 1,
                construction_method: 2,
                base_offset: 0,
                extents: vec![(0, 0, payload_len)],
            },
            // item 99: cm=0, base_offset=0 (placeholder), one extent.
            IlocRow {
                item_id: 99,
                construction_method: 0,
                base_offset: 0,
                extents: vec![(0, 0, payload_len)],
            },
        ],
        0, // index_size = 0 → cm=2 picks the single iref-iloc target.
    );
    let probe_meta = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf.clone()),
        (b"iref", iref.clone()),
        (b"iprp", iprp.clone()),
        (b"iloc", probe_iloc),
    ]);
    push_atom(&mut probe, *b"meta", &probe_meta);
    // mdat starts at probe.len(); payload starts at +8 (mdat header) +pad_len.
    let mdat_payload_offset = probe.len() as u32 + 8 + pad_len;

    let real_iloc = build_iloc_v1(
        &[
            IlocRow {
                item_id: 1,
                construction_method: 2,
                base_offset: 0,
                extents: vec![(0, 0, payload_len)],
            },
            IlocRow {
                item_id: 99,
                construction_method: 0,
                base_offset: mdat_payload_offset,
                extents: vec![(0, 0, payload_len)],
            },
        ],
        0,
    );
    let real_meta = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iprp", iprp),
        (b"iloc", real_iloc),
    ]);

    let mut wire = Vec::new();
    push_atom(&mut wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut wire, *b"meta", &real_meta);
    let mut mdat = vec![0u8; pad_len as usize];
    mdat.extend_from_slice(&payload);
    push_atom(&mut wire, *b"mdat", &mdat);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(wire));
    let mut dx = MovDemuxer::open(cur).unwrap();

    // Sanity: the recursive resolver on its own returns the expected
    // 8-byte grid payload via cm=2 → cm=0 chain.
    let resolved = dx.resolve_item_bytes(1).expect("cm=2 → cm=0 resolves");
    assert_eq!(resolved, payload);

    // The pure-meta resolver returns None (cm=2 needs an input handle).
    assert!(dx.primary_image_layout().is_none());

    // The _with_input variant must read through cm=2 transparently.
    let layout = dx
        .primary_image_layout_with_input()
        .expect("expected Grid plan via cm=2 resolution");
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
