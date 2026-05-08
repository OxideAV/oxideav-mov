//! Round-8 acceptance: HEIF/HEIC item-properties (`iprp`/`ipco`/`ipma`),
//! meta-only files (no `moov` tracks), and `iref` typed-reference
//! resolver helpers (`derived_from`, `auxiliary_for`, `thumbnail_of`,
//! `describes`).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{file_extents_for_item, ColorParametersKind, ItemProperty, MovDemuxer};

// ─────────────────────── helpers reused from round 7 ───────────────────────

fn build_meta_atom_payload(children: Vec<(&'static [u8; 4], Vec<u8>)>) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // FullBox ver+flags
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

fn iinf_v0_with_one_v2_infe(item_id: u16, item_type: &[u8; 4], item_name: &str) -> Vec<u8> {
    let mut infe_body = Vec::new();
    infe_body.push(2);
    infe_body.extend_from_slice(&[0, 0, 0]);
    infe_body.extend_from_slice(&item_id.to_be_bytes());
    infe_body.extend_from_slice(&0u16.to_be_bytes());
    infe_body.extend_from_slice(item_type);
    infe_body.extend_from_slice(item_name.as_bytes());
    infe_body.push(0);
    let mut iinf = Vec::new();
    iinf.extend_from_slice(&0u32.to_be_bytes());
    iinf.extend_from_slice(&1u16.to_be_bytes());
    let size = (8 + infe_body.len()) as u32;
    iinf.extend_from_slice(&size.to_be_bytes());
    iinf.extend_from_slice(b"infe");
    iinf.extend_from_slice(&infe_body);
    iinf
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

fn iloc_v0_one_item(item_id: u16, off: u32, len: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.push(0x44);
    p.push(0x00);
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&item_id.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&off.to_be_bytes());
    p.extend_from_slice(&len.to_be_bytes());
    p
}

// ─────────────────────── iprp/ipco/ipma builders ───────────────────────

fn ispe_payload(w: u32, h: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    p
}

fn pixi_payload(bits: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.push(bits.len() as u8);
    p.extend_from_slice(bits);
    p
}

fn colr_nclx_payload(prim: u16, transfer: u16, matrix: u16, full_range: bool) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"nclx");
    p.extend_from_slice(&prim.to_be_bytes());
    p.extend_from_slice(&transfer.to_be_bytes());
    p.extend_from_slice(&matrix.to_be_bytes());
    p.push(if full_range { 0x80 } else { 0 });
    p
}

fn build_ipco(props: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (fc, payload) in props {
        let size = (8 + payload.len()) as u32;
        body.extend_from_slice(&size.to_be_bytes());
        body.extend_from_slice(*fc);
        body.extend_from_slice(payload);
    }
    body
}

fn build_ipma_v0(rows: &[(u16, &[(u8, bool)])]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    body.extend_from_slice(&(rows.len() as u32).to_be_bytes());
    for (item_id, assocs) in rows {
        body.extend_from_slice(&item_id.to_be_bytes());
        body.push(assocs.len() as u8);
        for (idx, essential) in *assocs {
            let mut byte = idx & 0x7F;
            if *essential {
                byte |= 0x80;
            }
            body.push(byte);
        }
    }
    body
}

fn build_iprp(ipco_body: &[u8], ipma_body: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    let s1 = (8 + ipco_body.len()) as u32;
    body.extend_from_slice(&s1.to_be_bytes());
    body.extend_from_slice(b"ipco");
    body.extend_from_slice(ipco_body);
    let s2 = (8 + ipma_body.len()) as u32;
    body.extend_from_slice(&s2.to_be_bytes());
    body.extend_from_slice(b"ipma");
    body.extend_from_slice(ipma_body);
    body
}

// ─────────────────────── 1. iprp/ipco/ipma in moov-level meta ───────────────────────

fn build_movie_with_iprp_in_moov_meta() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
    let mdat_off: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    // Required 1-track skeleton so open() proceeds without entering
    // the meta-only branch (we want to verify iprp on the moov-scope
    // BMFF meta).
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_off));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);

    let ipco = build_ipco(&[
        (b"ispe", ispe_payload(256, 256)),
        (b"colr", colr_nclx_payload(1, 13, 6, true)),
        (b"pixi", pixi_payload(&[8, 8, 8])),
    ]);
    let ipma = build_ipma_v0(&[(11, &[(1, true), (2, false), (3, false)])]);
    let iprp = build_iprp(&ipco, &ipma);

    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(11)),
        (b"iinf", iinf_v0_with_one_v2_infe(11, b"hvc1", "primary")),
        (b"iloc", iloc_v0_one_item(11, mdat_off, 8)),
        (b"iprp", iprp),
    ]);
    push_atom(&mut moov, *b"meta", &meta_body);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn moov_iso_bmff_meta_iprp_decodes_and_resolves() {
    let bytes = build_movie_with_iprp_in_moov_meta();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open succeeds");
    let m = d.bmff_meta.as_ref().expect("BMFF meta surfaced");
    let p = m.properties.as_ref().expect("iprp surfaced");
    assert_eq!(p.properties.len(), 3);
    let resolved = p.resolve(11);
    assert_eq!(resolved.len(), 3);
    assert!(matches!(resolved[0], ItemProperty::Ispe(_)));
    assert!(matches!(resolved[1], ItemProperty::Colr(_)));
    assert!(matches!(resolved[2], ItemProperty::Pixi(_)));

    // Helpers
    let ispe = p.ispe_for(11).expect("ispe present");
    assert_eq!(ispe.width, 256);
    assert_eq!(ispe.height, 256);
    let colr = p.colr_for(11).expect("colr present");
    assert!(matches!(
        &colr.kind,
        ColorParametersKind::Nclx {
            full_range: true,
            ..
        }
    ));

    // First association is essential, others not.
    let row = p.associations_for(11).unwrap();
    assert_eq!(row.associations.len(), 3);
    assert!(row.associations[0].essential);
    assert!(!row.associations[1].essential);
}

// ─────────────────────── 2. Meta-only HEIF file (no moov) ───────────────────────

fn build_heif_only_no_moov(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // iloc v1 with construction_method=1 (idat).
    let mut iloc = Vec::new();
    iloc.push(1);
    iloc.extend_from_slice(&[0, 0, 0]);
    iloc.push(0x44);
    iloc.push(0x00);
    iloc.extend_from_slice(&1u16.to_be_bytes());
    iloc.extend_from_slice(&7u16.to_be_bytes());
    iloc.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1
    iloc.extend_from_slice(&0u16.to_be_bytes()); // dref index
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&0u32.to_be_bytes()); // offset within idat
    iloc.extend_from_slice(&(payload.len() as u32).to_be_bytes());

    let ipco = build_ipco(&[(b"ispe", ispe_payload(64, 64))]);
    let ipma = build_ipma_v0(&[(7, &[(1, true)])]);
    let iprp = build_iprp(&ipco, &ipma);

    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(7)),
        (b"iinf", iinf_v0_with_one_v2_infe(7, b"hvc1", "img")),
        (b"iloc", iloc),
        (b"idat", payload.to_vec()),
        (b"iprp", iprp),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);
    out
}

#[test]
fn meta_only_heif_file_opens_without_moov() {
    let inline = b"INLINE-HEIC-BYTES";
    let bytes = build_heif_only_no_moov(inline);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open meta-only HEIF succeeds");
    // No tracks, no mvhd — this is a still-image file.
    assert!(d.tracks.is_empty());
    assert!(d.mvhd.is_none());
    let fm = d.file_bmff_meta.as_ref().expect("file-level BMFF meta");
    assert_eq!(fm.primary_item, Some(7));
    assert_eq!(fm.idat.len(), inline.len());
    let p = fm.properties.as_ref().expect("iprp present");
    assert_eq!(p.properties.len(), 1);
    let resolved = p.resolve(7);
    assert_eq!(resolved.len(), 1);
    assert!(matches!(resolved[0], ItemProperty::Ispe(_)));
}

// ─────────────────────── 3. iref resolver helpers (HEIF grid + thumbnail) ───────────────────────

fn build_heif_grid_with_irefs(tile_payload_len: u32) -> Vec<u8> {
    // Models the corpus's still-image-grid-2x2 fixture at the iref
    // surface: item 1 = grid (derived) → contributing tiles 2/3/4/5;
    // item 6 = thumbnail (master = grid).
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // dummy mdat (tiles imagined to live here)
    push_atom(&mut out, *b"mdat", &[0u8; 16]);

    // iinf with 6 entries
    let entries = [
        (1u16, *b"grid", "derived-grid"),
        (2u16, *b"hvc1", "tile1"),
        (3u16, *b"hvc1", "tile2"),
        (4u16, *b"hvc1", "tile3"),
        (5u16, *b"hvc1", "tile4"),
        (6u16, *b"hvc1", "thumb"),
    ];
    let mut iinf_entries = Vec::new();
    for (id, ty, name) in &entries {
        iinf_entries.push((*id, *ty, *name));
    }
    let iinf = iinf_v0_with_v2_infes(&iinf_entries);

    // iref v0 with two children:
    //   dimg from 1 → [2,3,4,5]
    //   thmb from 6 → [1]
    let mut iref_body = Vec::new();
    iref_body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags

    let mut dimg_body = Vec::new();
    dimg_body.extend_from_slice(&1u16.to_be_bytes()); // from
    dimg_body.extend_from_slice(&4u16.to_be_bytes()); // to_count
    for to in [2u16, 3, 4, 5] {
        dimg_body.extend_from_slice(&to.to_be_bytes());
    }
    let s1 = (8 + dimg_body.len()) as u32;
    iref_body.extend_from_slice(&s1.to_be_bytes());
    iref_body.extend_from_slice(b"dimg");
    iref_body.extend_from_slice(&dimg_body);

    let mut thmb_body = Vec::new();
    thmb_body.extend_from_slice(&6u16.to_be_bytes());
    thmb_body.extend_from_slice(&1u16.to_be_bytes());
    thmb_body.extend_from_slice(&1u16.to_be_bytes());
    let s2 = (8 + thmb_body.len()) as u32;
    iref_body.extend_from_slice(&s2.to_be_bytes());
    iref_body.extend_from_slice(b"thmb");
    iref_body.extend_from_slice(&thmb_body);

    // Minimal iloc covering the grid item (construction 0, off 0, len `tile_payload_len`).
    let iloc = iloc_v0_one_item(1, 28, tile_payload_len);

    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iloc", iloc),
        (b"iref", iref_body),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);
    out
}

#[test]
fn iref_typed_resolver_helpers_walk_grid_and_thumbnail() {
    let bytes = build_heif_grid_with_irefs(8);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("opens (meta-only HEIF)");
    let fm = d.file_bmff_meta.as_ref().expect("file-level BMFF meta");
    // grid → tiles
    let tiles = fm.derived_from(1);
    assert_eq!(tiles, vec![2, 3, 4, 5]);
    // thumbnail → master
    let master = fm.thumbnail_of(6);
    assert_eq!(master, vec![1]);
    // inverse: thumbnails of master 1
    let thumbs = fm.thumbnails_of_master(1);
    assert_eq!(thumbs, vec![6]);
    // Items not in any iref return empty.
    assert!(fm.derived_from(99).is_empty());
    assert!(fm.thumbnail_of(99).is_empty());
    // file_extents_for_item still resolves construction_method=0.
    let extents = file_extents_for_item(fm, 1).expect("extents resolvable");
    assert_eq!(extents, vec![(28, 8)]);
}

// ─────────────────────── 4. Empty-but-present meta survives ───────────────────────

#[test]
fn meta_only_with_empty_meta_still_opens() {
    // A degenerate HEIF where only `hdlr` is present inside `meta`.
    // Round-7 already accepted this for `file_bmff_meta`; round 8 must
    // also let `open()` succeed since we now allow no tracks when a
    // file-level meta is present.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"mif1");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let meta_body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict())]);
    push_atom(&mut out, *b"meta", &meta_body);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("opens with empty meta-only file");
    assert!(d.tracks.is_empty());
    let fm = d.file_bmff_meta.as_ref().unwrap();
    assert_eq!(&fm.handler_type, b"pict");
    assert!(fm.properties.is_none());
}

// ─────────────────────── 5. ipma v1 large-index ───────────────────────

#[test]
fn ipma_v1_16bit_indices_roundtrip() {
    // A meta-only HEIF whose ipma uses the v1 / flags=1 wide form:
    // u32 item_ID, u16 assoc with 15-bit index. Mostly forward-compat
    // territory but the corpus header notes that some authoring tools
    // emit it.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"mif1");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let ipco = build_ipco(&[(b"ispe", ispe_payload(2, 2))]);
    let mut ipma = Vec::new();
    ipma.push(1u8); // version=1
    ipma.extend_from_slice(&[0, 0, 1]); // flags=1 → 16-bit indices
    ipma.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    ipma.extend_from_slice(&7u32.to_be_bytes()); // item_ID v1 = u32
    ipma.push(1); // association_count
    ipma.extend_from_slice(&0x8001u16.to_be_bytes()); // essential=1, idx=1

    let iprp = build_iprp(&ipco, &ipma);
    let meta_body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict()), (b"iprp", iprp)]);
    push_atom(&mut out, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("opens");
    let fm = d.file_bmff_meta.as_ref().unwrap();
    let p = fm.properties.as_ref().unwrap();
    let row = p.associations_for(7).unwrap();
    assert_eq!(row.associations.len(), 1);
    assert!(row.associations[0].essential);
    assert_eq!(row.associations[0].index, 1);
    let r = p.resolve(7);
    assert_eq!(r.len(), 1);
    assert!(matches!(r[0], ItemProperty::Ispe(_)));
}
