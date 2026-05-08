//! Round-9 acceptance: HEIF derived-image payloads (`grid` / `iovl`),
//! `pitm`-aware primary-item-bytes convenience helper, and the
//! built-in `file://` URL opener for reference-movie alias chains.
//!
//! Per the docs round-9 brief these compose the HEIF "multi-tile +
//! primary-item-bytes" story together: a HEIF still-image file
//! declares a derived `grid` item as its primary, the round-9 helper
//! resolves that primary's bytes from `idat` / file extents in one
//! call, and [`oxideav_mov::parse_grid`] decodes the 16-byte fixed-
//! format payload into rows / cols / output dimensions.

#![cfg(feature = "registry")]

mod common;

use std::io::{Cursor, Write};

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{
    item_data, open_file_url, parse_grid, parse_overlay, parse_overlay_with_source_count,
    primary_item_data, ItemDataLocation, MovDemuxer,
};

// ─────────────────────── builder helpers (shared shapes) ───────────────────────

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

/// One-item iloc with construction_method=1 (idat-resident); offset
/// `off` is the offset into idat, length `len` is the slice length.
fn iloc_v1_idat_one_item(item_id: u16, off: u32, len: u32) -> Vec<u8> {
    let mut iloc = Vec::new();
    iloc.push(1); // version
    iloc.extend_from_slice(&[0, 0, 0]); // flags
    iloc.push(0x44); // offset_size=4, length_size=4
    iloc.push(0x00); // base_offset_size=0, index_size=0
    iloc.extend_from_slice(&1u16.to_be_bytes()); // item_count
    iloc.extend_from_slice(&item_id.to_be_bytes()); // item_id
    iloc.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1
    iloc.extend_from_slice(&0u16.to_be_bytes()); // dref_index
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&off.to_be_bytes());
    iloc.extend_from_slice(&len.to_be_bytes());
    iloc
}

/// Multi-item iloc (v1, all idat-resident) builder. Each `(item_id,
/// offset, length)` entry adds one row pointing at the same idat.
fn iloc_v1_idat_multi(items: &[(u16, u32, u32)]) -> Vec<u8> {
    let mut iloc = Vec::new();
    iloc.push(1);
    iloc.extend_from_slice(&[0, 0, 0]);
    iloc.push(0x44);
    iloc.push(0x00);
    iloc.extend_from_slice(&(items.len() as u16).to_be_bytes());
    for (id, off, len) in items {
        iloc.extend_from_slice(&id.to_be_bytes());
        iloc.extend_from_slice(&1u16.to_be_bytes()); // method=1
        iloc.extend_from_slice(&0u16.to_be_bytes()); // dref
        iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count=1
        iloc.extend_from_slice(&off.to_be_bytes());
        iloc.extend_from_slice(&len.to_be_bytes());
    }
    iloc
}

/// Single-item iloc v0 with construction_method=0 (file-extents).
fn iloc_v0_file_one_item(item_id: u16, off: u32, len: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.push(0x44);
    p.push(0x00);
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&item_id.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes()); // dref index
    p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    p.extend_from_slice(&off.to_be_bytes());
    p.extend_from_slice(&len.to_be_bytes());
    p
}

/// Build a HEIF still-image-grid-2x2 fixture mirror: primary is a
/// `grid` item (id=1) whose 16-byte payload lives in `idat`; the four
/// `hvc1` tiles (ids 2..=5) live in `mdat` (file extents). The grid's
/// `dimg` iref lists the tile order.
fn build_heif_grid_meta_only() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // Tile bytes in mdat (just placeholder bytes — we don't decode HEVC here).
    let mdat_payload = b"TILE-1__TILE-2__TILE-3__TILE-4__"; // 4 x 8 bytes
    push_atom(&mut out, *b"mdat", mdat_payload);
    // ftyp atom = 8 hdr + 16 body = 24; mdat hdr at 24, body at 32.
    let mdat_payload_off: u32 = 32;

    // iinf with 5 entries: grid + 4 tiles.
    let entries = [
        (1u16, *b"grid", "grid"),
        (2u16, *b"hvc1", "tile1"),
        (3u16, *b"hvc1", "tile2"),
        (4u16, *b"hvc1", "tile3"),
        (5u16, *b"hvc1", "tile4"),
    ];
    let iinf = iinf_v0_with_v2_infes(&entries);

    // iref: dimg from grid → [2,3,4,5] in row-major order.
    let mut iref_body = Vec::new();
    iref_body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    let mut dimg = Vec::new();
    dimg.extend_from_slice(&1u16.to_be_bytes()); // from
    dimg.extend_from_slice(&4u16.to_be_bytes()); // to_count
    for to in [2u16, 3, 4, 5] {
        dimg.extend_from_slice(&to.to_be_bytes());
    }
    let s = (8 + dimg.len()) as u32;
    iref_body.extend_from_slice(&s.to_be_bytes());
    iref_body.extend_from_slice(b"dimg");
    iref_body.extend_from_slice(&dimg);

    // grid payload: rows=2, cols=2, output=256x256, 16-bit dims.
    let mut grid_idat = vec![
        0u8, // ver
        0,   // flags
        1,   // rows_minus_one
        1,   // cols_minus_one
    ];
    grid_idat.extend_from_slice(&256u16.to_be_bytes());
    grid_idat.extend_from_slice(&256u16.to_be_bytes()); // 8 bytes total
    let grid_payload_len = grid_idat.len() as u32;

    // iloc: grid item via idat (offset 0 → grid_payload_len), tiles via file extents.
    let mut iloc = Vec::new();
    iloc.push(1); // v1 to allow construction_method
    iloc.extend_from_slice(&[0, 0, 0]); // flags
    iloc.push(0x44); // 4-byte offset/length
    iloc.push(0x00); // base_offset=0, index=0
    iloc.extend_from_slice(&5u16.to_be_bytes()); // item_count
                                                 // grid (id=1) idat-resident
    iloc.extend_from_slice(&1u16.to_be_bytes());
    iloc.extend_from_slice(&1u16.to_be_bytes()); // method=1
    iloc.extend_from_slice(&0u16.to_be_bytes()); // dref
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count=1
    iloc.extend_from_slice(&0u32.to_be_bytes());
    iloc.extend_from_slice(&grid_payload_len.to_be_bytes());
    // tiles 2..=5 file-extents in mdat
    for (i, off) in (0..4u32).enumerate() {
        let id = 2u16 + i as u16;
        iloc.extend_from_slice(&id.to_be_bytes());
        iloc.extend_from_slice(&0u16.to_be_bytes()); // method=0 (file)
        iloc.extend_from_slice(&0u16.to_be_bytes()); // dref
        iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count=1
        iloc.extend_from_slice(&(mdat_payload_off + off * 8).to_be_bytes());
        iloc.extend_from_slice(&8u32.to_be_bytes());
    }

    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iloc", iloc),
        (b"idat", grid_idat),
        (b"iref", iref_body),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);
    out
}

#[test]
fn primary_item_data_returns_idat_bytes_for_grid_payload() {
    let bytes = build_heif_grid_meta_only();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open meta-only HEIF grid succeeds");
    let fm = d.file_bmff_meta.as_ref().expect("file-level meta surfaced");
    assert_eq!(fm.primary_item, Some(1));

    // primary_item_data resolves pitm + iloc in one call.
    let loc = primary_item_data(fm).expect("primary item resolves");
    let payload = match loc {
        ItemDataLocation::Idat(b) => b,
        other => panic!("expected idat-resident grid, got {other:?}"),
    };
    assert_eq!(payload.len(), 8);

    // parse_grid decodes into Grid{rows=2, cols=2, w=256, h=256}.
    let g = parse_grid(&payload).expect("grid payload parses");
    assert_eq!(g.rows, 2);
    assert_eq!(g.cols, 2);
    assert_eq!(g.output_width, 256);
    assert_eq!(g.output_height, 256);

    // The 4 tiles resolve as file-extents pointing into mdat.
    for tile_id in 2..=5u32 {
        let tile_loc = item_data(fm, tile_id).expect("tile resolves");
        match tile_loc {
            ItemDataLocation::FileExtents(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].1, 8);
            }
            other => panic!("tile {tile_id} expected file-extents, got {other:?}"),
        }
    }
}

#[test]
fn primary_item_data_none_when_no_pitm() {
    // A meta-only file without a pitm: primary_item_data returns None.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"mif1");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let meta_body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict())]);
    push_atom(&mut out, *b"meta", &meta_body);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open meta-only file");
    let fm = d.file_bmff_meta.as_ref().unwrap();
    assert!(primary_item_data(fm).is_none());
}

#[test]
fn item_data_surfaces_construction_method_2_as_other() {
    // Build a meta with one v1 iloc row using construction_method=2.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"mif1");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mut iloc = Vec::new();
    iloc.push(1); // v1
    iloc.extend_from_slice(&[0, 0, 0]);
    iloc.push(0x44); // off=4, len=4
    iloc.push(0x00); // base=0, idx=0
    iloc.extend_from_slice(&1u16.to_be_bytes());
    iloc.extend_from_slice(&7u16.to_be_bytes()); // item_id
    iloc.extend_from_slice(&2u16.to_be_bytes()); // construction_method=2 (item_offset)
    iloc.extend_from_slice(&0u16.to_be_bytes()); // dref
    iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    iloc.extend_from_slice(&100u32.to_be_bytes()); // offset
    iloc.extend_from_slice(&50u32.to_be_bytes()); // length

    let iinf = iinf_v0_with_v2_infes(&[(7u16, *b"hvc1", "img")]);
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(7)),
        (b"iinf", iinf),
        (b"iloc", iloc),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open succeeds");
    let fm = d.file_bmff_meta.as_ref().unwrap();
    let pl = primary_item_data(fm).expect("pitm resolves");
    match pl {
        ItemDataLocation::Other {
            construction_method,
            ..
        } => assert_eq!(construction_method, 2),
        other => panic!("expected Other (method=2), got {other:?}"),
    }
}

#[test]
fn primary_item_bytes_with_file_extents_and_real_read() {
    // Item lives in mdat; primary_item_data returns FileExtents, and
    // the caller can dispatch the read against the demuxer's input.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);
    let payload = b"DEADBEEF";
    push_atom(&mut out, *b"mdat", payload);
    // ftyp atom = 8 hdr + 16 body = 24; mdat hdr at 24, body at 32.
    let mdat_off: u32 = 8 + 16 + 8;

    let iinf = iinf_v0_with_v2_infes(&[(7u16, *b"hvc1", "img")]);
    let iloc = iloc_v0_file_one_item(7, mdat_off, payload.len() as u32);
    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(7)),
        (b"iinf", iinf),
        (b"iloc", iloc),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out.clone()));
    let d = MovDemuxer::open(cur).expect("opens");
    let fm = d.file_bmff_meta.as_ref().unwrap();
    let loc = primary_item_data(fm).expect("primary resolves");
    let extents = match loc {
        ItemDataLocation::FileExtents(v) => v,
        other => panic!("expected file extents, got {other:?}"),
    };
    assert_eq!(extents, vec![(mdat_off as u64, payload.len() as u64)]);
    // Caller-side reads against the input bytes.
    let (off, len) = extents[0];
    let read_back = &out[off as usize..(off + len) as usize];
    assert_eq!(read_back, payload);
}

// ─────────────────────── 2. iovl payload ───────────────────────

#[test]
fn iovl_corpus_shape_decodes_via_inferred_count() {
    // Mirror the corpus `still-image-overlay` payload: fill=
    // (16384,16384,16384,65535), 256×256 canvas, two layers
    // (base at (0,0), stamp at (96,96)).
    let mut body = Vec::new();
    body.push(0); // ver
    body.push(0); // flags = 16-bit dims
    for c in [16384u16, 16384, 16384, 65535] {
        body.extend_from_slice(&c.to_be_bytes());
    }
    body.extend_from_slice(&256u16.to_be_bytes());
    body.extend_from_slice(&256u16.to_be_bytes());
    // 2 layers: (0,0), (96,96) — 4 bytes each
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&96i16.to_be_bytes());
    body.extend_from_slice(&96i16.to_be_bytes());
    let o = parse_overlay(&body).unwrap();
    assert_eq!(o.canvas_fill_color, [16384, 16384, 16384, 65535]);
    assert_eq!(o.output_width, 256);
    assert_eq!(o.output_height, 256);
    assert_eq!(o.offsets, vec![(0, 0), (96, 96)]);
}

#[test]
fn iovl_with_explicit_source_count_validates_against_iref() {
    // When the caller knows how many dimg targets the iref declares
    // (3 here), parse_overlay_with_source_count enforces that count.
    let mut body = Vec::new();
    body.push(0);
    body.push(0);
    for _ in 0..4 {
        body.extend_from_slice(&0u16.to_be_bytes());
    }
    body.extend_from_slice(&64u16.to_be_bytes());
    body.extend_from_slice(&64u16.to_be_bytes());
    for (h, v) in [(0i16, 0i16), (10, -10), (-5, 5)] {
        body.extend_from_slice(&h.to_be_bytes());
        body.extend_from_slice(&v.to_be_bytes());
    }
    let o = parse_overlay_with_source_count(&body, 3).unwrap();
    assert_eq!(o.offsets.len(), 3);
    assert_eq!(o.offsets[1], (10, -10));
    // Mismatching the count rejects.
    assert!(parse_overlay_with_source_count(&body, 5).is_err());
}

// ─────────────────────── 3. file:// alias opener ───────────────────────

// The three integration tests below construct `file://` URLs from
// real filesystem paths. On Windows those paths use backslashes and
// drive-letter prefixes (e.g. `D:\foo`), which require URL escapes
// and a Windows-aware path-back-conversion that this round's
// `open_file_url` does not implement (see open_file_url module
// docs). The Unix shape (`file:///abs/path`) is exercised on Linux
// and macOS; the Windows shape is left for a follow-up round.
#[cfg(unix)]
#[test]
fn open_file_url_resolves_local_filesystem_alias() {
    use std::env;

    // 1) Write a real self-contained .mov to a tempfile.
    let mut bytes = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut bytes, *b"ftyp", &ftyp);
    push_atom(&mut bytes, *b"mdat", b"PAYLOAD!");
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
    push_atom(&mut bytes, *b"moov", &moov);

    // Tempfile path inside std::env::temp_dir().
    let mut path = env::temp_dir();
    path.push(format!(
        "oxideav_mov_round9_target_{}.mov",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(&bytes).expect("write tempfile");
    }
    // Build a reference-movie file pointing at the tempfile via file://.
    let url = format!("file://{}", path.display());
    let mut alias_bytes = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut alias_bytes, *b"ftyp", &ftyp);
    let mut moov2 = Vec::new();
    push_atom(&mut moov2, *b"mvhd", &build_mvhd(600, 0));
    // rmra/rmda/rdrf with url=file://.../target.mov
    let mut rmra = Vec::new();
    let mut rmda = Vec::new();
    let mut rdrf = Vec::new();
    rdrf.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    rdrf.extend_from_slice(b"url ");
    let url_bytes = url.as_bytes();
    let mut url_with_nul = url_bytes.to_vec();
    url_with_nul.push(0);
    rdrf.extend_from_slice(&(url_with_nul.len() as u32).to_be_bytes());
    rdrf.extend_from_slice(&url_with_nul);
    push_atom(&mut rmda, *b"rdrf", &rdrf);
    push_atom(&mut rmra, *b"rmda", &rmda);
    push_atom(&mut moov2, *b"rmra", &rmra);
    push_atom(&mut alias_bytes, *b"moov", &moov2);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(alias_bytes));
    let d = MovDemuxer::open_with_aliases(cur, open_file_url).expect("file:// alias resolves");
    assert!(!d.tracks.is_empty(), "resolved target carries the track");
    // Cleanup.
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_file_url_rejects_non_file_scheme() {
    // open_file_url returns std::io::ErrorKind::Unsupported for
    // anything that's not file://. Match on the Result rather than
    // unwrap_err — `dyn ReadSeek` doesn't impl Debug.
    match open_file_url("http://example.com/foo.mov") {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::Unsupported),
        Ok(_) => panic!("expected Unsupported error for http://"),
    }
    match open_file_url("ftp://server/path") {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::Unsupported),
        Ok(_) => panic!("expected Unsupported error for ftp://"),
    }
}

#[cfg(unix)]
#[test]
fn open_file_url_decodes_percent_encoding() {
    // Build a tempfile whose name contains a literal space, then
    // percent-encode the space in the file:// URL. The opener must
    // decode `%20` and find the file.
    use std::env;
    let mut path = env::temp_dir();
    path.push(format!(
        "oxideav_mov_round9 with spaces_{}.bin",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(b"hello").expect("write");
    }
    let raw = path.display().to_string();
    let encoded = raw.replace(' ', "%20");
    let url = format!("file://{}", encoded);
    let mut handle = open_file_url(&url).expect("opener locates encoded path");
    use std::io::Read;
    let mut buf = Vec::new();
    handle.read_to_end(&mut buf).expect("read tempfile");
    assert_eq!(buf, b"hello");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_file_url_rejects_remote_host_for_safety() {
    // file://example.com/path is rejected so we don't accidentally read
    // from a network mount the user didn't authorise.
    match open_file_url("file://example.com/etc/passwd") {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::Unsupported),
        Ok(_) => panic!("expected Unsupported error for foreign-host file://"),
    }
}

#[cfg(unix)]
#[test]
fn open_file_url_accepts_localhost_authority() {
    // file://localhost/path is the canonical "this host" authority and
    // must resolve identically to file:///path. Verify by opening a
    // tempfile via the localhost form.
    use std::env;
    let mut path = env::temp_dir();
    path.push(format!(
        "oxideav_mov_round9_localhost_{}.bin",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(b"loopback").expect("write");
    }
    let url = format!("file://localhost{}", path.display());
    let mut handle = open_file_url(&url).expect("localhost-host file:// resolves");
    use std::io::Read;
    let mut buf = Vec::new();
    handle.read_to_end(&mut buf).expect("read");
    assert_eq!(buf, b"loopback");
    let _ = std::fs::remove_file(&path);
}

// ─────────────────────── 4. iloc_v1_idat_multi ergonomic ───────────────────────

#[test]
fn item_data_and_idat_concat_round_trip_multiple_items() {
    // Two idat-resident items sharing one idat blob. item 1 = first 4
    // bytes, item 2 = last 6.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"mif1");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"mif1");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let idat = b"AAAABBBBBB"; // 4 + 6 = 10 bytes
    let iinf = iinf_v0_with_v2_infes(&[(1u16, *b"grid", "g"), (2u16, *b"grid", "g2")]);
    let iloc = iloc_v1_idat_multi(&[(1, 0, 4), (2, 4, 6)]);

    let meta_body = build_meta_atom_payload(vec![
        (b"hdlr", hdlr_pict()),
        (b"pitm", pitm_v0(1)),
        (b"iinf", iinf),
        (b"iloc", iloc),
        (b"idat", idat.to_vec()),
    ]);
    push_atom(&mut out, *b"meta", &meta_body);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    let fm = d.file_bmff_meta.as_ref().unwrap();
    match item_data(fm, 1).unwrap() {
        ItemDataLocation::Idat(b) => assert_eq!(b, b"AAAA"),
        _ => panic!("expected idat"),
    }
    match item_data(fm, 2).unwrap() {
        ItemDataLocation::Idat(b) => assert_eq!(b, b"BBBBBB"),
        _ => panic!("expected idat"),
    }
    // Unknown id → None.
    assert!(item_data(fm, 99).is_none());
}

// ─────────────────────── 5. silence unused warning ───────────────────────

#[test]
fn _smoke_iloc_v1_idat_one_item_helper() {
    // Construct a single-item iloc to silence the unused-helper
    // warning while documenting the simpler shape.
    let _ = iloc_v1_idat_one_item(1, 0, 8);
}
