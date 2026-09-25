//! Round-461 acceptance: the HEIF / HEIC / MIAF layer left this crate
//! for `oxideav-heif`. These tests pin the QuickTime-side contract
//! that remains:
//!
//! 1. The `mov` content probe never claims a HEIF-family `ftyp`
//!    (`mif1` / `heic` / `heix` / `avif` / `avis` / `msf1`), whether
//!    the brand is the major brand or only listed as compatible, and
//!    still scores `qt  ` at full confidence.
//! 2. Even if both probes *did* score, `oxideav-core`'s resolution
//!    rule (score descending, then **lower** priority number wins,
//!    then registration order) hands a HEIF file to the container
//!    registered below the default priority — `oxideav-heif` registers
//!    at `DEFAULT_PRIORITY - 50`; this crate stays at the default.
//! 3. An item-based (ISO BMFF §8.11) `meta` box inside `moov` / `trak`
//!    is not QuickTime metadata: the movie opens, the Apple key-value
//!    surface stays empty, and nothing else about the movie changes.
//! 4. A `meta`-only file with no `moov` is not a QuickTime movie and
//!    is refused at open.
//! 5. A stray top-level `meta` box ahead of a real movie is skipped.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{ContainerRegistry, ProbeData, ReadSeek, DEFAULT_PRIORITY};
use oxideav_mov::{demuxer, MovDemuxer};

/// HEIF / MIAF / AVIF brands that `oxideav-heif` owns (ISO/IEC
/// 23008-12 §10 brand registry, ISO/IEC 23000-22 §7.2, AVIF §3).
const HEIF_FAMILY: [&[u8; 4]; 6] = [b"mif1", b"heic", b"heix", b"avif", b"avis", b"msf1"];

fn ftyp_bytes(major: &[u8; 4], compat: &[&[u8; 4]]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(major);
    body.extend_from_slice(&0u32.to_be_bytes());
    for b in compat {
        body.extend_from_slice(*b);
    }
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &body);
    out
}

fn probe_score(buf: &[u8]) -> u8 {
    demuxer::probe(&ProbeData { buf, ext: None })
}

/// ISO BMFF §8.11 `meta` payload: FullBox header followed by the given
/// children. `hdlr pict` + `pitm` is the smallest item-based shape.
fn item_meta_payload() -> Vec<u8> {
    let mut hdlr = vec![0u8; 8]; // ver+flags, pre_defined
    hdlr.extend_from_slice(b"pict");
    hdlr.extend_from_slice(&[0u8; 12]); // reserved
    hdlr.push(0); // empty name
    let mut pitm = vec![0u8; 4]; // ver+flags
    pitm.extend_from_slice(&1u16.to_be_bytes());
    let mut body = vec![0u8; 4]; // FullBox ver+flags
    push_atom(&mut body, *b"hdlr", &hdlr);
    push_atom(&mut body, *b"pitm", &pitm);
    body
}

/// One-track, one-sample QuickTime movie. `moov_extra` / `trak_extra`
/// splice additional atoms into the movie and track scopes.
fn build_movie(moov_extra: &[u8], trak_extra: &[u8]) -> Vec<u8> {
    let mut out = ftyp_bytes(b"qt  ", &[b"qt  "]);
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
    trak.extend_from_slice(trak_extra);
    push_atom(&mut moov, *b"trak", &trak);
    moov.extend_from_slice(moov_extra);
    push_atom(&mut out, *b"moov", &moov);
    out
}

fn open(bytes: Vec<u8>) -> oxideav_core::Result<MovDemuxer> {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur)
}

// ─────────────────────── 1. probe never claims HEIF brands ───────────────────────

#[test]
fn probe_scores_zero_for_every_heif_family_major_brand() {
    for major in HEIF_FAMILY {
        let buf = ftyp_bytes(major, &[b"mif1", b"miaf", b"isom"]);
        assert_eq!(
            probe_score(&buf),
            0,
            "major brand {:?} must not be claimed as mov",
            std::str::from_utf8(major).unwrap()
        );
    }
}

#[test]
fn probe_scores_zero_when_heif_brand_is_only_compatible() {
    for compat in HEIF_FAMILY {
        let buf = ftyp_bytes(b"isom", &[b"iso8", compat]);
        assert_eq!(
            probe_score(&buf),
            0,
            "compatible brand {:?} must not be claimed as mov",
            std::str::from_utf8(compat).unwrap()
        );
    }
}

#[test]
fn probe_still_scores_quicktime_brands() {
    assert_eq!(probe_score(&ftyp_bytes(b"qt  ", &[b"qt  "])), 100);
    assert_eq!(probe_score(&ftyp_bytes(b"isom", &[b"iso2", b"qt  "])), 90);
    // Bare `moov` (pre-`ftyp` QuickTime) stays a weak match.
    let mut bare = Vec::new();
    push_atom(&mut bare, *b"moov", &[0u8; 8]);
    assert_eq!(probe_score(&bare), 40);
}

// ─────────────────────── 2. registry resolution direction ───────────────────────

fn heif_stand_in_probe(p: &ProbeData) -> u8 {
    // Scores exactly like a HEIF-family probe would: 100 on any
    // HEIF brand in the `ftyp`, 0 otherwise.
    if p.buf.len() < 16 || &p.buf[4..8] != b"ftyp" {
        return 0;
    }
    let size = u32::from_be_bytes([p.buf[0], p.buf[1], p.buf[2], p.buf[3]]) as usize;
    let upper = size.min(p.buf.len()).max(16);
    let mut o = 8;
    while o + 4 <= upper {
        if o != 12 && HEIF_FAMILY.iter().any(|b| **b == p.buf[o..o + 4]) {
            return 100;
        }
        o += 4;
    }
    0
}

#[test]
fn heif_file_never_resolves_to_mov_in_a_shared_registry() {
    let mut reg = ContainerRegistry::new();
    // Register mov FIRST so registration order would favour it on a
    // full tie — the priority rule must still send HEIF elsewhere.
    oxideav_mov::registry::register_containers(&mut reg);
    reg.register_probe_with_priority("heif-stand-in", heif_stand_in_probe, DEFAULT_PRIORITY - 50);

    assert_eq!(reg.probe_priority("mov"), Some(DEFAULT_PRIORITY));

    for major in HEIF_FAMILY {
        let buf = ftyp_bytes(major, &[b"mif1", b"miaf"]);
        let cands = reg.probe_candidates(&ProbeData {
            buf: &buf,
            ext: None,
        });
        assert!(
            cands.iter().all(|c| c.name != "mov"),
            "mov must not appear as a candidate for {:?}: {cands:?}",
            std::str::from_utf8(major).unwrap()
        );
        assert_eq!(cands.first().map(|c| c.name), Some("heif-stand-in"));
        let mut cur = Cursor::new(buf.clone());
        assert_eq!(reg.probe_input(&mut cur, None).unwrap(), "heif-stand-in");
    }

    // A real QuickTime file stays with mov.
    let qt = ftyp_bytes(b"qt  ", &[b"qt  "]);
    let cands = reg.probe_candidates(&ProbeData {
        buf: &qt,
        ext: None,
    });
    assert_eq!(cands.first().map(|c| (c.name, c.score)), Some(("mov", 100)));
}

#[test]
fn lower_priority_number_wins_a_full_score_tie() {
    // Documents the direction of core's tie-break for the record: if a
    // HEIF-family probe and mov ever scored the same, the container
    // registered at the *smaller* priority number wins — that is the
    // HEIF side (`DEFAULT_PRIORITY - 50`), not this crate.
    fn always_100(_: &ProbeData) -> u8 {
        100
    }
    let mut reg = ContainerRegistry::new();
    reg.register_probe("mov-like-default", always_100);
    reg.register_probe_with_priority("heif-like-lower", always_100, DEFAULT_PRIORITY - 50);
    let buf = ftyp_bytes(b"heic", &[b"mif1"]);
    let cands = reg.probe_candidates(&ProbeData {
        buf: &buf,
        ext: None,
    });
    assert_eq!(cands.len(), 2);
    assert_eq!(cands[0].name, "heif-like-lower");
    assert_eq!(cands[0].priority, DEFAULT_PRIORITY - 50);
    assert_eq!(cands[1].name, "mov-like-default");
    assert_eq!(cands[1].priority, DEFAULT_PRIORITY);
}

// ─────────────────────── 3. item-based meta inside a movie ───────────────────────

#[test]
fn item_based_moov_meta_is_not_quicktime_metadata() {
    let mut extra = Vec::new();
    push_atom(&mut extra, *b"meta", &item_meta_payload());
    let d = open(build_movie(&extra, &[])).expect("movie opens");
    assert!(
        d.meta.is_empty(),
        "item-based meta must not populate the Apple surface"
    );
    assert_eq!(d.tracks.len(), 1);
    assert_eq!(d.tracks[0].sample_table.sample_count(), 1);
}

#[test]
fn item_based_trak_meta_is_not_quicktime_metadata() {
    let mut extra = Vec::new();
    push_atom(&mut extra, *b"meta", &item_meta_payload());
    let d = open(build_movie(&[], &extra)).expect("movie opens");
    assert!(d.tracks[0].meta.is_empty());
    assert!(d.meta.is_empty());
}

#[test]
fn apple_keys_ilst_meta_still_decodes_next_to_the_item_shape() {
    // Sanity: the Apple shape is unaffected — a movie-level `keys` +
    // `ilst` pair still lands one key-value entry.
    let mut meta = Vec::new();
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"mdta");
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.push(0);
    push_atom(&mut meta, *b"hdlr", &hdlr);
    let key = b"com.example.title";
    let mut keys = vec![0u8; 4];
    keys.extend_from_slice(&1u32.to_be_bytes());
    keys.extend_from_slice(&((8 + key.len()) as u32).to_be_bytes());
    keys.extend_from_slice(b"mdta");
    keys.extend_from_slice(key);
    push_atom(&mut meta, *b"keys", &keys);
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_be_bytes()); // type UTF-8
    data.extend_from_slice(&0u32.to_be_bytes()); // locale
    data.extend_from_slice(b"Hello");
    let mut item = Vec::new();
    push_atom(&mut item, *b"data", &data);
    let mut ilst = Vec::new();
    push_atom(&mut ilst, 1u32.to_be_bytes(), &item);
    push_atom(&mut meta, *b"ilst", &ilst);

    let mut extra = Vec::new();
    push_atom(&mut extra, *b"meta", &meta);
    let d = open(build_movie(&extra, &[])).expect("movie opens");
    assert_eq!(d.meta.len(), 1);
    assert_eq!(d.meta[0].key, "com.example.title");
}

// ─────────────────────── 4. meta-only files are refused ───────────────────────

#[test]
fn meta_only_still_image_is_not_a_quicktime_movie() {
    let mut bytes = ftyp_bytes(b"heic", &[b"mif1", b"heic"]);
    push_atom(&mut bytes, *b"meta", &item_meta_payload());
    push_atom(&mut bytes, *b"mdat", b"coded-image-bytes");
    let err = match open(bytes) {
        Ok(_) => panic!("a meta-only file must not open as a movie"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(msg.contains("no moov"), "unexpected error: {msg}");
}

#[test]
fn movie_with_mvhd_but_no_tracks_is_refused() {
    let mut out = ftyp_bytes(b"qt  ", &[b"qt  "]);
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));
    push_atom(&mut moov, *b"meta", &item_meta_payload());
    push_atom(&mut out, *b"moov", &moov);
    let err = match open(out) {
        Ok(_) => panic!("a track-less moov must not open as a movie"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("no tracks"), "{err}");
}

// ─────────────────────── 5. stray top-level meta is skipped ───────────────────────

#[test]
fn top_level_meta_ahead_of_a_movie_is_ignored() {
    let movie = build_movie(&[], &[]);
    // Splice a file-level `meta` between `ftyp` and `mdat`, then fix
    // up the single chunk offset for the shift.
    let ftyp_len = 8 + 4 + 4 + 4;
    let mut meta_atom = Vec::new();
    push_atom(&mut meta_atom, *b"meta", &item_meta_payload());
    let shift = meta_atom.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&movie[..ftyp_len]);
    out.extend_from_slice(&meta_atom);
    out.extend_from_slice(&movie[ftyp_len..]);
    // Patch stco: locate the atom and bump its single entry.
    let pos = out
        .windows(4)
        .position(|w| w == b"stco")
        .expect("stco present");
    let entry = pos + 4 + 4 + 4; // fourcc, ver+flags, count
    let old = u32::from_be_bytes(out[entry..entry + 4].try_into().unwrap());
    out[entry..entry + 4].copy_from_slice(&(old + shift).to_be_bytes());

    let mut d = open(out).expect("movie opens with a stray meta");
    assert!(d.meta.is_empty());
    let (_, _, data) = d.read_next().expect("one sample");
    assert_eq!(data, b"PAYLOAD!");
}
