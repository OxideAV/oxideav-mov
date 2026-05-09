//! Round-16 acceptance: long-deferred `iloc` resolver gaps.
//!
//! r12's `read_derivation_payload_bytes` resolved
//! `construction_method` 0 (file extents) and 1 (idat) but punted on
//! `construction_method == 2` (item_offset) — extents that sub-slice
//! into *another* item's resolved payload (ISO/IEC 14496-12 §8.11.3).
//! Round 16 closes that gap with a recursive resolver
//! ([`MovDemuxer::resolve_item_bytes`]) that:
//!
//! * walks 0 / 1 / 2 transparently,
//! * tracks visited item ids in a `HashSet` and aborts cleanly with
//!   [`Error::invalid("MOV: iloc cycle through item N")`] on
//!   self-referencing chains,
//! * sub-slices the source item's bytes per the
//!   `(base_offset + offset, length)` triple on each cm=2 extent.
//!
//! r16 also surfaces:
//!
//! * Per-extent `extent_index` field as `Option<u64>` on
//!   [`oxideav_mov::ItemExtent`] when the parent `iloc` carries
//!   `index_size > 0` (was always silently zero before — this is
//!   `Some(idx)` now and gates source-item selection on cm=2).
//! * HEIF `base` `iref` typed reference (ISO/IEC 23008-12 §6.4.7) +
//!   [`oxideav_mov::ItemReferenceType::Base`] enum variant +
//!   [`MovDemuxer::base_image_for`] accessor for pre-derived coded
//!   image lookups (e.g. an HDR variant pre-rendered alongside an
//!   SDR base).
//!
//! Spec references:
//! - ISO/IEC 14496-12:2015 §8.11.3 (ItemLocationBox: cm 0/1/2 +
//!   `index_size`).
//! - ISO/IEC 23008-12:2017 §6.4.7 (`base` iref).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::push_atom;
use oxideav_core::ReadSeek;
use oxideav_mov::{ItemReferenceType, MovDemuxer};

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

/// Build an `iref` v0 with one single-item-reference of the given kind.
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

/// Build an `iref` v0 with multiple single-item-reference children.
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

/// Build an `iloc` v1 with the given rows. `offset_size = length_size = 4`,
/// `base_offset_size = 4`, `index_size` from parameter.
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

// ─────────────────────── #1 — recursive cm=2 resolver ───────────────────────

#[test]
fn resolve_item_bytes_chains_cm2_through_cm0() {
    // Three-deep chain:
    //   item 0 (cm=0, file extents → 16-byte payload in mdat)
    //   item 1 (cm=2, sub-slice item 0 at offset 4 length 8)
    //   item 2 (cm=2, sub-slice item 1 at offset 2 length 4)
    //
    // Final resolve(item 2) yields item 0's bytes [4..12][2..6] = [6..10].

    // Build the wire payload for item 0 in mdat.
    let payload: Vec<u8> = (0u8..16).collect(); // [0,1,2,...,15]

    // First pass: probe to locate the mdat payload offset.
    let iinf = iinf_v0_with_v2_infes(&[
        (0, *b"hvc1", "src"),
        (1, *b"hvc1", "mid"),
        (2, *b"hvc1", "leaf"),
    ]);
    let iref = iref_many(&[
        (b"iloc", 1, vec![0]), // item 1 sources from item 0
        (b"iloc", 2, vec![1]), // item 2 sources from item 1
    ]);

    let mut probe = Vec::new();
    push_atom(&mut probe, *b"ftyp", &ftyp_mif1());
    let probe_iloc = build_iloc_v1(
        &[
            IlocRow {
                item_id: 0,
                construction_method: 0,
                base_offset: 0, // placeholder
                extents: vec![(0, 0, payload.len() as u32)],
            },
            IlocRow {
                item_id: 1,
                construction_method: 2,
                base_offset: 0,
                extents: vec![(0, 4, 8)], // sub-slice item 0[4..12]
            },
            IlocRow {
                item_id: 2,
                construction_method: 2,
                base_offset: 0,
                extents: vec![(0, 2, 4)], // sub-slice item 1[2..6]
            },
        ],
        0, // index_size = 0
    );
    let probe_meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(2)),
        (b"iinf", iinf.clone()),
        (b"iref", iref.clone()),
        (b"iloc", probe_iloc),
    ]);
    push_atom(&mut probe, *b"meta", &probe_meta_body);
    let mdat_payload_offset = probe.len() as u32 + 8;

    // Second pass with real file offset.
    let real_iloc = build_iloc_v1(
        &[
            IlocRow {
                item_id: 0,
                construction_method: 0,
                base_offset: mdat_payload_offset,
                extents: vec![(0, 0, payload.len() as u32)],
            },
            IlocRow {
                item_id: 1,
                construction_method: 2,
                base_offset: 0,
                extents: vec![(0, 4, 8)],
            },
            IlocRow {
                item_id: 2,
                construction_method: 2,
                base_offset: 0,
                extents: vec![(0, 2, 4)],
            },
        ],
        0,
    );
    let real_meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(2)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iloc", real_iloc),
    ]);
    let mut wire = Vec::new();
    push_atom(&mut wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut wire, *b"meta", &real_meta_body);
    push_atom(&mut wire, *b"mdat", &payload);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(wire));
    let mut dx = MovDemuxer::open(cur).unwrap();
    let bytes = dx.resolve_item_bytes(2).unwrap();
    // item 0 = [0..16]; item 1 = item 0[4..12] = [4..12];
    // item 2 = item 1[2..6] = [6..10] = [6,7,8,9].
    assert_eq!(bytes, vec![6u8, 7, 8, 9]);

    // Intermediate item resolves cleanly too.
    let bytes_mid = dx.resolve_item_bytes(1).unwrap();
    assert_eq!(bytes_mid, vec![4u8, 5, 6, 7, 8, 9, 10, 11]);

    // Source item resolves directly.
    let bytes_src = dx.resolve_item_bytes(0).unwrap();
    assert_eq!(bytes_src, payload);
}

#[test]
fn resolve_item_bytes_detects_cycle() {
    // Two-item cycle: item 1 cm=2 sources item 2; item 2 cm=2 sources
    // item 1. The recursive resolver must abort with an Error::invalid
    // mentioning the cycle rather than recursing forever or
    // overflowing the stack.

    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "a"), (2, *b"hvc1", "b")]);
    let iref = iref_many(&[
        (b"iloc", 1, vec![2]), // item 1 sources from item 2
        (b"iloc", 2, vec![1]), // item 2 sources from item 1 (CYCLE)
    ]);
    let iloc = build_iloc_v1(
        &[
            IlocRow {
                item_id: 1,
                construction_method: 2,
                base_offset: 0,
                extents: vec![(0, 0, 4)],
            },
            IlocRow {
                item_id: 2,
                construction_method: 2,
                base_offset: 0,
                extents: vec![(0, 0, 4)],
            },
        ],
        0,
    );
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iloc", iloc),
    ]);
    let mut wire = Vec::new();
    push_atom(&mut wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut wire, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(wire));
    let mut dx = MovDemuxer::open(cur).unwrap();
    let err = dx.resolve_item_bytes(1).unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("cycle"),
        "expected cycle-detection error, got: {s}"
    );
}

#[test]
fn resolve_item_bytes_self_cycle() {
    // Single-item self-reference: item 1 cm=2 sources item 1 itself.
    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "self")]);
    let iref = iref_one(b"iloc", 1, &[1]); // item 1 → item 1
    let iloc = build_iloc_v1(
        &[IlocRow {
            item_id: 1,
            construction_method: 2,
            base_offset: 0,
            extents: vec![(0, 0, 4)],
        }],
        0,
    );
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iloc", iloc),
    ]);
    let mut wire = Vec::new();
    push_atom(&mut wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut wire, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(wire));
    let mut dx = MovDemuxer::open(cur).unwrap();
    let err = dx.resolve_item_bytes(1).unwrap_err();
    assert!(format!("{err}").contains("cycle"));
}

// ─────────────────────── #2 — index_size > 0 ───────────────────────

#[test]
fn iloc_index_size_4_populates_extent_index() {
    // Build an iloc v1 with index_size=4 and one item (cm=0) carrying
    // two extents whose extent_index fields differ. Verify both
    // ItemExtent::index entries are Some(idx) with the expected values.
    //
    // Use cm=0 here so we can assert the parsed index regardless of
    // resolver semantics.

    let payload: Vec<u8> = (0u8..32).collect();

    let iinf = iinf_v0_with_v2_infes(&[(7, *b"hvc1", "x")]);
    let mut probe = Vec::new();
    push_atom(&mut probe, *b"ftyp", &ftyp_mif1());
    let probe_iloc = build_iloc_v1(
        &[IlocRow {
            item_id: 7,
            construction_method: 0,
            base_offset: 0,
            extents: vec![(11, 0, 16), (22, 16, 16)],
        }],
        4,
    );
    let probe_meta = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(7)),
        (b"iinf", iinf.clone()),
        (b"iloc", probe_iloc),
    ]);
    push_atom(&mut probe, *b"meta", &probe_meta);
    let mdat_offset = probe.len() as u32 + 8;

    let real_iloc = build_iloc_v1(
        &[IlocRow {
            item_id: 7,
            construction_method: 0,
            base_offset: mdat_offset,
            extents: vec![(11, 0, 16), (22, 16, 16)],
        }],
        4,
    );
    let real_meta = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(7)),
        (b"iinf", iinf),
        (b"iloc", real_iloc),
    ]);
    let mut wire = Vec::new();
    push_atom(&mut wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut wire, *b"meta", &real_meta);
    push_atom(&mut wire, *b"mdat", &payload);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(wire));
    let dx = MovDemuxer::open(cur).unwrap();
    let meta = dx.file_bmff_meta.as_ref().expect("meta box present");
    let loc = meta.find_location(7).expect("item 7 present");
    assert_eq!(loc.extents.len(), 2);
    assert_eq!(loc.extents[0].index, Some(11));
    assert_eq!(loc.extents[1].index, Some(22));
    // index_size==0 path still emits None.
    let mut iloc_no_idx = Vec::new();
    iloc_no_idx.push(1u8); // version
    iloc_no_idx.extend_from_slice(&[0, 0, 0]);
    iloc_no_idx.push(0x44);
    iloc_no_idx.push(0x40); // index_size=0
    iloc_no_idx.extend_from_slice(&1u16.to_be_bytes());
    iloc_no_idx.extend_from_slice(&8u16.to_be_bytes()); // item id
    iloc_no_idx.extend_from_slice(&0u16.to_be_bytes()); // cm=0
    iloc_no_idx.extend_from_slice(&0u16.to_be_bytes()); // dref
    iloc_no_idx.extend_from_slice(&0u32.to_be_bytes()); // base_offset
    iloc_no_idx.extend_from_slice(&1u16.to_be_bytes());
    iloc_no_idx.extend_from_slice(&0u32.to_be_bytes());
    iloc_no_idx.extend_from_slice(&8u32.to_be_bytes());
    let other_meta = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"iinf", iinf_v0_with_v2_infes(&[(8, *b"hvc1", "no-idx")])),
        (b"iloc", iloc_no_idx),
    ]);
    let mut other_wire = Vec::new();
    push_atom(&mut other_wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut other_wire, *b"meta", &other_meta);
    let cur2: Box<dyn ReadSeek> = Box::new(Cursor::new(other_wire));
    let dx2 = MovDemuxer::open(cur2).unwrap();
    let meta2 = dx2.file_bmff_meta.as_ref().expect("meta");
    let loc2 = meta2.find_location(8).expect("item 8");
    assert_eq!(loc2.extents[0].index, None);
}

#[test]
fn resolve_item_bytes_cm2_uses_extent_index_for_source_selection() {
    // cm=2 with index_size=4: two source items live in mdat. The
    // derived item carries one extent per source, each tagged with the
    // 1-based extent_index into the iref-iloc target list. The
    // resolver must use extent_index to pick the correct source.

    let src0_payload: Vec<u8> = b"AAAAAAAA".to_vec(); // 8 bytes
    let src1_payload: Vec<u8> = b"BBBBBBBB".to_vec(); // 8 bytes

    let iinf = iinf_v0_with_v2_infes(&[
        (10, *b"hvc1", "src0"),
        (11, *b"hvc1", "src1"),
        (20, *b"hvc1", "derived"),
    ]);
    // derived (item 20) iref-iloc → [10, 11] (in that order)
    let iref = iref_one(b"iloc", 20, &[10, 11]);

    // probe for offsets
    let mut probe = Vec::new();
    push_atom(&mut probe, *b"ftyp", &ftyp_mif1());
    let probe_iloc = build_iloc_v1(
        &[
            IlocRow {
                item_id: 10,
                construction_method: 0,
                base_offset: 0,
                extents: vec![(0, 0, 8)],
            },
            IlocRow {
                item_id: 11,
                construction_method: 0,
                base_offset: 0,
                extents: vec![(0, 0, 8)],
            },
            IlocRow {
                item_id: 20,
                construction_method: 2,
                base_offset: 0,
                // extent 1: extent_index=2 (→ src1), offset 0 length 4 → "BBBB"
                // extent 2: extent_index=1 (→ src0), offset 4 length 4 → "AAAA"
                extents: vec![(2, 0, 4), (1, 4, 4)],
            },
        ],
        4,
    );
    let probe_meta = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(20)),
        (b"iinf", iinf.clone()),
        (b"iref", iref.clone()),
        (b"iloc", probe_iloc),
    ]);
    push_atom(&mut probe, *b"meta", &probe_meta);
    let mdat_payload_offset = probe.len() as u32 + 8;

    let real_iloc = build_iloc_v1(
        &[
            IlocRow {
                item_id: 10,
                construction_method: 0,
                base_offset: mdat_payload_offset,
                extents: vec![(0, 0, 8)],
            },
            IlocRow {
                item_id: 11,
                construction_method: 0,
                base_offset: mdat_payload_offset + 8,
                extents: vec![(0, 0, 8)],
            },
            IlocRow {
                item_id: 20,
                construction_method: 2,
                base_offset: 0,
                extents: vec![(2, 0, 4), (1, 4, 4)],
            },
        ],
        4,
    );
    let real_meta = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(20)),
        (b"iinf", iinf),
        (b"iref", iref),
        (b"iloc", real_iloc),
    ]);
    let mut wire = Vec::new();
    push_atom(&mut wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut wire, *b"meta", &real_meta);
    let mut mdat = Vec::new();
    mdat.extend_from_slice(&src0_payload);
    mdat.extend_from_slice(&src1_payload);
    push_atom(&mut wire, *b"mdat", &mdat);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(wire));
    let mut dx = MovDemuxer::open(cur).unwrap();
    let bytes = dx.resolve_item_bytes(20).unwrap();
    // First extent: src1[0..4] = "BBBB"; second: src0[4..8] = "AAAA".
    assert_eq!(bytes, b"BBBBAAAA".to_vec());
}

// ─────────────────────── #4 — base iref typed surface ───────────────────────

#[test]
fn base_image_for_returns_pre_derived_target() {
    // HEIF §6.4.7: pre-derived coded image. item 2 is the derived
    // (HDR-rendered) variant; `base` iref from 2 → 0 declares the
    // SDR base it was authored from.
    let iinf =
        iinf_v0_with_v2_infes(&[(0, *b"hvc1", "sdr-base"), (2, *b"hvc1", "hdr-pre-derived")]);
    let iref = iref_one(b"base", 2, &[0]);
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(0)),
        (b"iinf", iinf),
        (b"iref", iref),
    ]);
    let mut wire = Vec::new();
    push_atom(&mut wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut wire, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(wire));
    let dx = MovDemuxer::open(cur).unwrap();
    assert_eq!(dx.base_image_for(2), Some(0));
    assert_eq!(dx.base_image_for(0), None); // base item itself has no `base` iref

    let meta = dx.file_bmff_meta.as_ref().expect("meta");
    assert_eq!(meta.base_image_for(2), Some(0));
}

#[test]
fn typed_references_promote_base_variant() {
    // Mixed iref: one `base`, one `dimg`. typed_references() must
    // promote `base` to ItemReferenceType::Base and pass the rest
    // through Other with FourCC preserved.
    let iinf = iinf_v0_with_v2_infes(&[
        (0, *b"hvc1", "sdr"),
        (1, *b"grid", "deriv"),
        (2, *b"hvc1", "hdr-base"),
        (10, *b"hvc1", "tile"),
    ]);
    let iref = iref_many(&[(b"base", 2, vec![0]), (b"dimg", 1, vec![10])]);
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(2)),
        (b"iinf", iinf),
        (b"iref", iref),
    ]);
    let mut wire = Vec::new();
    push_atom(&mut wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut wire, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(wire));
    let dx = MovDemuxer::open(cur).unwrap();
    let meta = dx.file_bmff_meta.as_ref().expect("meta");
    let typed = meta.typed_references();
    assert_eq!(typed.len(), 2);

    let mut saw_base = false;
    let mut saw_other_dimg = false;
    for r in &typed {
        match r {
            ItemReferenceType::Base { from_id, to_ids } => {
                assert_eq!(*from_id, 2);
                assert_eq!(to_ids, &vec![0]);
                saw_base = true;
            }
            ItemReferenceType::Other {
                kind,
                from_id,
                to_ids,
            } => {
                assert_eq!(kind, b"dimg");
                assert_eq!(*from_id, 1);
                assert_eq!(to_ids, &vec![10]);
                saw_other_dimg = true;
            }
        }
    }
    assert!(saw_base, "expected one Base variant");
    assert!(saw_other_dimg, "expected one Other(dimg) variant");
}

#[test]
fn base_image_for_no_base_iref_returns_none() {
    // No `base` iref in the file → accessor returns None.
    let iinf = iinf_v0_with_v2_infes(&[(1, *b"hvc1", "lone")]);
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
    ]);
    let mut wire = Vec::new();
    push_atom(&mut wire, *b"ftyp", &ftyp_mif1());
    push_atom(&mut wire, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(wire));
    let dx = MovDemuxer::open(cur).unwrap();
    assert_eq!(dx.base_image_for(1), None);
}
