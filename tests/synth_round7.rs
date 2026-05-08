//! Round-7 acceptance: ISO BMFF §8.11 `meta` box parsing
//! (`pitm` / `iinf` / `iloc` / `idat` / `xml ` / `iref`), multi-hop
//! `rmra/url ` alias-chain following with cycle detection, and the
//! `styl` / `ftab` / `hlit` / `hclr` / `drpo` text-sample style
//! trailer surface.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{file_extents_for_item, parse_text_sample_styles, MovDemuxer, MAX_ALIAS_DEPTH};

// ─────────────────────── ISO BMFF meta @ moov ───────────────────────

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
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    p.extend_from_slice(b"pict");
    p.extend_from_slice(&[0u8; 12]);
    p.push(0);
    p
}

fn pitm_v0(item_id: u16) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&item_id.to_be_bytes());
    p
}

fn iloc_v0_one_item(item_id: u16, off: u32, len: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.push(0x44); // off=4, len=4
    p.push(0x00); // base=0, idx=0
    p.extend_from_slice(&1u16.to_be_bytes()); // item_count
    p.extend_from_slice(&item_id.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes()); // dref index
    p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    p.extend_from_slice(&off.to_be_bytes());
    p.extend_from_slice(&len.to_be_bytes());
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
    iinf.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    iinf.extend_from_slice(&1u16.to_be_bytes()); // entry_count
    let size = (8 + infe_body.len()) as u32;
    iinf.extend_from_slice(&size.to_be_bytes());
    iinf.extend_from_slice(b"infe");
    iinf.extend_from_slice(&infe_body);
    iinf
}

fn build_movie_with_bmff_meta_in_moov() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
    let mdat_off: u32 = 28;

    // Build a moov with mvhd, trak, and a BMFF-shape meta box.
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    // Regular video track so the file has tracks (we don't want
    // open() to fail with "no tracks" — the BMFF meta is auxiliary).
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

    // Construct BMFF-shape meta with hdlr + pitm + iinf + iloc.
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(11)),
        (b"iinf", iinf_v0_with_one_v2_infe(11, b"hvc1", "primary")),
        (b"iloc", iloc_v0_one_item(11, mdat_off, 8)),
    ]);
    push_atom(&mut moov, *b"meta", &meta_body);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn moov_level_iso_bmff_meta_decodes() {
    let bytes = build_movie_with_bmff_meta_in_moov();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open succeeds");
    assert_eq!(d.tracks.len(), 1);
    let m = d
        .bmff_meta
        .as_ref()
        .expect("BMFF meta surfaced at moov scope");
    assert_eq!(&m.handler_type, b"pict");
    assert_eq!(m.primary_item, Some(11));
    assert_eq!(m.items.len(), 1);
    assert_eq!(&m.items[0].item_type, b"hvc1");
    assert_eq!(m.items[0].item_name, "primary");
    assert_eq!(m.locations.len(), 1);
    let loc = m.find_location(11).unwrap();
    assert_eq!(loc.extents[0].length, 8);
    // Construction method 0 → file offsets resolvable via helper.
    let extents = file_extents_for_item(m, 11).unwrap();
    assert_eq!(extents.len(), 1);
}

// ─────────────────────── File-level meta + idat ───────────────────────

fn build_file_meta_with_idat_only(payload_inline: &[u8]) -> Vec<u8> {
    // For HEIF-style still-image files; we still emit a placeholder
    // moov so MovDemuxer::open() doesn't fail with "no tracks".
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"heic");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // File-level meta with hdlr + idat + iloc(construction_method=1).
    let mut iloc = Vec::new();
    iloc.push(1); // version 1 (so construction_method field is present)
    iloc.extend_from_slice(&[0, 0, 0]);
    iloc.push(0x44);
    iloc.push(0x00);
    iloc.extend_from_slice(&1u16.to_be_bytes());
    iloc.extend_from_slice(&7u16.to_be_bytes()); // item_id
    iloc.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1 (idat)
    iloc.extend_from_slice(&0u16.to_be_bytes()); // dref index
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&0u32.to_be_bytes()); // offset within idat
    iloc.extend_from_slice(&(payload_inline.len() as u32).to_be_bytes());

    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(7)),
        (b"iinf", iinf_v0_with_one_v2_infe(7, b"hvc1", "img")),
        (b"iloc", iloc),
        (b"idat", payload_inline.to_vec()),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);

    // Empty mdat keeps the layout structurally similar to a real HEIF.
    push_atom(&mut out, *b"mdat", &[0u8; 4]);
    let mdat_off = (out.len() - 4 - 4) as u32;

    // Minimal moov so open() succeeds (no tracks would fail).
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));
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
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(4, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_off));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn file_level_iso_bmff_meta_with_idat_decodes() {
    let inline = b"INLINE-IMG-BYTES";
    let bytes = build_file_meta_with_idat_only(inline);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open file-level meta succeeds");
    let fm = d
        .file_bmff_meta
        .as_ref()
        .expect("file-level bmff meta surfaced");
    assert_eq!(fm.primary_item, Some(7));
    assert_eq!(fm.items.len(), 1);
    assert_eq!(fm.idat.len(), inline.len());
    let loc = fm.find_location(7).unwrap();
    assert_eq!(loc.construction_method, 1);
    // Locating extents via the file-offset helper returns None for
    // construction_method != 0.
    assert!(file_extents_for_item(fm, 7).is_none());
}

// ─────────────────────── Multi-hop alias chain ───────────────────────

fn build_reference_only(target_url: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));

    let mut url_buf = Vec::new();
    url_buf.extend_from_slice(target_url.as_bytes());
    url_buf.push(0);
    let mut rdrf = Vec::new();
    rdrf.extend_from_slice(&0u32.to_be_bytes());
    rdrf.extend_from_slice(b"url ");
    rdrf.extend_from_slice(&(url_buf.len() as u32).to_be_bytes());
    rdrf.extend_from_slice(&url_buf);

    let mut rmda = Vec::new();
    push_atom(&mut rmda, *b"rdrf", &rdrf);
    let mut rmra = Vec::new();
    push_atom(&mut rmra, *b"rmda", &rmda);
    push_atom(&mut moov, *b"rmra", &rmra);
    push_atom(&mut out, *b"moov", &moov);
    out
}

fn build_self_contained_movie() -> Vec<u8> {
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
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn open_with_aliases_follows_two_hops() {
    // outer.mov  → inner.mov  → final.mov (self-contained)
    let outer = build_reference_only("memory:./inner.mov");
    let inner = build_reference_only("memory:./final.mov");
    let final_bytes = build_self_contained_movie();
    let inner_clone = inner.clone();
    let final_clone = final_bytes.clone();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(outer));
    let opener = move |url: &str| -> std::io::Result<Box<dyn ReadSeek>> {
        match url {
            "memory:./inner.mov" => Ok(Box::new(Cursor::new(inner_clone.clone()))),
            "memory:./final.mov" => Ok(Box::new(Cursor::new(final_clone.clone()))),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "unknown url",
            )),
        }
    };
    let d = MovDemuxer::open_with_aliases(cur, opener).expect("two-hop chain resolves");
    assert_eq!(d.tracks.len(), 1);
    let _ = inner;
    let _ = final_bytes;
}

#[test]
fn open_with_aliases_rejects_cycle() {
    // a → b → a (cycle); the resolver detects revisit on the third step.
    let a = build_reference_only("memory:./b.mov");
    let b = build_reference_only("memory:./a.mov");
    let a_clone = a.clone();
    let b_clone = b.clone();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(a.clone()));
    let opener = move |url: &str| -> std::io::Result<Box<dyn ReadSeek>> {
        match url {
            "memory:./a.mov" => Ok(Box::new(Cursor::new(a_clone.clone()))),
            "memory:./b.mov" => Ok(Box::new(Cursor::new(b_clone.clone()))),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "unknown url",
            )),
        }
    };
    let err = match MovDemuxer::open_with_aliases(cur, opener) {
        Ok(_) => panic!("cycle must reject"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("cycle") || msg.contains("alias chain"),
        "unexpected error: {msg}"
    );
    let _ = b;
}

#[test]
fn open_with_aliases_caps_at_max_alias_depth() {
    const _: () = assert!(MAX_ALIAS_DEPTH >= 2);
    // Build a chain of length MAX_ALIAS_DEPTH+2 to exceed the cap.
    let mut layers: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..(MAX_ALIAS_DEPTH + 2) {
        let url = format!("memory:./layer{i}.mov");
        let target_url = format!("memory:./layer{}.mov", i + 1);
        layers.push((url, build_reference_only(&target_url)));
    }
    let layers_owned = layers.clone();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(layers[0].1.clone()));
    let opener = move |url: &str| -> std::io::Result<Box<dyn ReadSeek>> {
        for (u, bytes) in &layers_owned {
            if u == url {
                return Ok(Box::new(Cursor::new(bytes.clone())));
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "unknown url",
        ))
    };
    let err = match MovDemuxer::open_with_aliases(cur, opener) {
        Ok(_) => panic!("max-depth chain must reject"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("MAX_ALIAS_DEPTH") || msg.contains("alias chain"),
        "unexpected error: {msg}"
    );
}

// ─────────────────────── styl/ftab/hlit/hclr/drpo ───────────────────────

#[test]
fn styl_trailer_decodes_via_parse_text_sample_styles() {
    // "Hi" + styl(1 record covering 0..2, font 1, bold, 14pt, opaque red)
    let mut p = Vec::new();
    let txt = b"Hi";
    p.extend_from_slice(&(txt.len() as u16).to_be_bytes());
    p.extend_from_slice(txt);
    p.extend_from_slice(&22u32.to_be_bytes()); // styl size
    p.extend_from_slice(b"styl");
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(&2u16.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p.push(0x01);
    p.push(14);
    p.extend_from_slice(&[0xFF, 0x00, 0x00, 0xFF]);

    let (title, styles) = parse_text_sample_styles(&p).unwrap();
    assert_eq!(title, "Hi");
    assert_eq!(styles.style_runs.len(), 1);
    assert_eq!(styles.style_runs[0].font_size, 14);
    assert_eq!(styles.style_runs[0].color.r, 0xFF);
}
