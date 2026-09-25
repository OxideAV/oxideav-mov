//! Round-7 acceptance: multi-hop `rmra/url ` alias-chain following
//! with cycle detection, and the `styl` / `ftab` / `hlit` / `hclr` /
//! `drpo` text-sample style trailer surface.
//!
//! (The ISO BMFF §8.11 item-based `meta` coverage that used to live
//! here moved with the HEIF layer to `oxideav-heif`.)

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{parse_text_sample_styles, MovDemuxer, MAX_ALIAS_DEPTH};

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
