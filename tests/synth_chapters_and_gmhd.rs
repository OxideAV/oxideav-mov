//! Round-5 acceptance: chapter-track resolution (`tref/chap` →
//! decoded `ChapterEntry` list with start/duration/title), per-
//! MediaType `gmhd` extension parsing (`gmin` / `text` / `tmcd`),
//! and a v1-`mvhd` integration fixture.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a `tref/chap` payload pointing at a single chapter track id.
fn build_tref_chap(chap_id: u32) -> Vec<u8> {
    let mut tref = Vec::new();
    push_atom(&mut tref, *b"chap", &chap_id.to_be_bytes());
    tref
}

/// Build a 2-entry `stts` payload: each (count, dur) pair.
fn build_stts_two(c1: u32, d1: u32, c2: u32, d2: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&2u32.to_be_bytes());
    p.extend_from_slice(&c1.to_be_bytes());
    p.extend_from_slice(&d1.to_be_bytes());
    p.extend_from_slice(&c2.to_be_bytes());
    p.extend_from_slice(&d2.to_be_bytes());
    p
}

/// Build an `stsz` with explicit per-sample sizes.
fn build_stsz_table(sizes: &[u32]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0 → table
    p.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
    for s in sizes {
        p.extend_from_slice(&s.to_be_bytes());
    }
    p
}

/// Build a `stsd` for a `text` track. The QuickTime text sample
/// description payload is 51 bytes (per QTFF p. 142): display flags +
/// text justification + bg color + default text box + reserved + font
/// info. We zero everything; the demuxer only needs the first 16
/// universal bytes to record the format FourCC.
fn build_stsd_text() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&1u32.to_be_bytes()); // entry count
    let entry_size: u32 = 16 + 51;
    p.extend_from_slice(&entry_size.to_be_bytes());
    p.extend_from_slice(b"text");
    p.extend_from_slice(&[0u8; 6]); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // dref index
    p.extend_from_slice(&[0u8; 51]); // text-specific body
    p
}

/// Build an Apple text sample: `[u16 BE size][text bytes]`.
fn make_text_sample(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + s.len());
    v.extend_from_slice(&(s.len() as u16).to_be_bytes());
    v.extend_from_slice(s.as_bytes());
    v
}

/// Build a `gmhd` container with all three extensions (gmin + text +
/// tmcd/tcmi). Returns the wrapping `gmhd` body — push_atom it under
/// `minf`.
fn build_gmhd_full() -> Vec<u8> {
    let mut gmhd = Vec::new();
    // gmin: ver+flags + graphics_mode=0x0040 + opcolor (FFFF, 0, 0)
    // + balance=0 + reserved.
    let mut gmin = Vec::new();
    gmin.extend_from_slice(&0u32.to_be_bytes());
    gmin.extend_from_slice(&0x0040u16.to_be_bytes());
    gmin.extend_from_slice(&0xFFFFu16.to_be_bytes());
    gmin.extend_from_slice(&0u16.to_be_bytes());
    gmin.extend_from_slice(&0u16.to_be_bytes());
    gmin.extend_from_slice(&0i16.to_be_bytes());
    gmin.extend_from_slice(&0u16.to_be_bytes());
    push_atom(&mut gmhd, *b"gmin", &gmin);

    // text: identity matrix, 36 bytes.
    let mut text = vec![0u8; 36];
    let one: i32 = 0x0001_0000;
    let w: i32 = 0x4000_0000;
    text[0..4].copy_from_slice(&one.to_be_bytes());
    text[16..20].copy_from_slice(&one.to_be_bytes());
    text[32..36].copy_from_slice(&w.to_be_bytes());
    push_atom(&mut gmhd, *b"text", &text);

    // tmcd > tcmi
    let mut tcmi = Vec::new();
    tcmi.extend_from_slice(&0u32.to_be_bytes());
    tcmi.extend_from_slice(&3u16.to_be_bytes()); // text_font (Helvetica)
    tcmi.extend_from_slice(&0u16.to_be_bytes()); // text_face
    tcmi.extend_from_slice(&12u16.to_be_bytes()); // text_size
    tcmi.extend_from_slice(&0u16.to_be_bytes()); // reserved
    for c in [0u16, 0, 0] {
        tcmi.extend_from_slice(&c.to_be_bytes());
    }
    for c in [0xFFFFu16, 0xFFFF, 0xFFFF] {
        tcmi.extend_from_slice(&c.to_be_bytes());
    }
    let name = b"Helvetica";
    tcmi.push(name.len() as u8);
    tcmi.extend_from_slice(name);

    let mut tmcd = Vec::new();
    push_atom(&mut tmcd, *b"tcmi", &tcmi);
    push_atom(&mut gmhd, *b"tmcd", &tmcd);
    gmhd
}

/// Build a movie with:
///   - track 1: video, 1 sample, with `tref/chap` pointing at track 2
///   - track 2: text (chapter), 2 samples carrying titles
///
/// Both tracks share the same `mdat` region; we lay out the bytes so
/// each sample starts at a known absolute offset.
fn build_movie_with_chapters() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // mdat layout:
    //   video sample (8 bytes): "VIDEO!!!"
    //   chapter sample 1 ("Intro" → 7 bytes)
    //   chapter sample 2 ("Outro" → 7 bytes)
    let video = b"VIDEO!!!";
    let s1 = make_text_sample("Intro"); // 7 bytes
    let s2 = make_text_sample("Outro"); // 7 bytes
    let mut mdat_body = Vec::new();
    mdat_body.extend_from_slice(video);
    mdat_body.extend_from_slice(&s1);
    mdat_body.extend_from_slice(&s2);
    let mdat_payload_off = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &mdat_body);

    let video_off = mdat_payload_off;
    let s1_off = mdat_payload_off + video.len() as u32;
    let _s2_off = s1_off + s1.len() as u32;

    // Build moov.
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 60));

    // ── Track 1: video, with tref/chap → 2.
    let mut trak1 = Vec::new();
    push_atom(&mut trak1, *b"tkhd", &build_tkhd(1, 60, 320, 240));
    push_atom(&mut trak1, *b"tref", &build_tref_chap(2));
    let mut mdia1 = Vec::new();
    push_atom(&mut mdia1, *b"mdhd", &build_mdhd(600, 60));
    push_atom(&mut mdia1, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf1 = Vec::new();
    push_atom(&mut minf1, *b"vmhd", &build_vmhd());
    let mut stbl1 = Vec::new();
    push_atom(
        &mut stbl1,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl1, *b"stts", &build_stts_single(1, 60));
    push_atom(&mut stbl1, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl1, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl1, *b"stco", &build_stco_single(video_off));
    push_atom(&mut minf1, *b"stbl", &stbl1);
    push_atom(&mut mdia1, *b"minf", &minf1);
    push_atom(&mut trak1, *b"mdia", &mdia1);
    push_atom(&mut moov, *b"trak", &trak1);

    // ── Track 2: text (chapter), 2 samples.
    let mut trak2 = Vec::new();
    push_atom(&mut trak2, *b"tkhd", &build_tkhd(2, 60, 0, 0));
    let mut mdia2 = Vec::new();
    push_atom(&mut mdia2, *b"mdhd", &build_mdhd(1000, 60));
    push_atom(&mut mdia2, *b"hdlr", &build_hdlr(b"mhlr", b"text"));

    let mut minf2 = Vec::new();
    // gmhd with gmin + text + tmcd
    push_atom(&mut minf2, *b"gmhd", &build_gmhd_full());

    let mut stbl2 = Vec::new();
    push_atom(&mut stbl2, *b"stsd", &build_stsd_text());
    // 2 samples: durations 30 and 30 (in chapter-media-timescale ticks)
    push_atom(&mut stbl2, *b"stts", &build_stts_two(1, 30, 1, 30));
    push_atom(&mut stbl2, *b"stsc", &build_stsc_single(2)); // 2 samples per chunk
    push_atom(
        &mut stbl2,
        *b"stsz",
        &build_stsz_table(&[s1.len() as u32, s2.len() as u32]),
    );
    push_atom(&mut stbl2, *b"stco", &build_stco_single(s1_off));
    push_atom(&mut minf2, *b"stbl", &stbl2);
    push_atom(&mut mdia2, *b"minf", &minf2);
    push_atom(&mut trak2, *b"mdia", &mdia2);
    push_atom(&mut moov, *b"trak", &trak2);

    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn chapter_track_resolves_to_entries() {
    let bytes = build_movie_with_chapters();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open chapter fixture");

    // 2 tracks: video (idx 0) + text/chapter (idx 1).
    assert_eq!(d.tracks.len(), 2);
    assert!(d.tracks[0].is_video());
    assert!(d.tracks[1].is_text());

    // tref/chap surfaces on the primary track.
    assert_eq!(d.tracks[0].chapter_track_ref(), Some(2));

    // gmhd parsed onto the chapter track.
    let gmhd = d.tracks[1].gmhd.as_ref().expect("chapter track has gmhd");
    let gmin = gmhd.gmin.expect("gmin");
    assert_eq!(gmin.graphics_mode, 0x0040);
    assert_eq!(gmin.opcolor, [0xFFFF, 0, 0]);
    let text_hdr = gmhd.text.expect("text header");
    assert_eq!(text_hdr.matrix[0], 0x0001_0000);
    assert_eq!(text_hdr.matrix[8], 0x4000_0000);
    let tcmi = gmhd.tcmi.as_ref().expect("tcmi");
    assert_eq!(tcmi.text_font, 3);
    assert_eq!(tcmi.text_size, 12);
    assert_eq!(tcmi.font_name, "Helvetica");

    // Resolve chapters. 2 entries, "Intro" then "Outro".
    let cl = d
        .chapters_for(0)
        .expect("chapter resolution succeeds")
        .expect("chapter list present");
    assert_eq!(cl.track_index, 1);
    assert_eq!(cl.time_scale, 1000);
    assert_eq!(cl.entries.len(), 2);
    assert_eq!(cl.entries[0].start_time, 0);
    assert_eq!(cl.entries[0].duration, 30);
    assert_eq!(cl.entries[0].title, "Intro");
    assert_eq!(cl.entries[1].start_time, 30);
    assert_eq!(cl.entries[1].duration, 30);
    assert_eq!(cl.entries[1].title, "Outro");
}

#[test]
fn chapters_for_track_without_tref_returns_none() {
    // Strip the tref/chap from the fixture.
    let mut bytes = build_movie_with_chapters();
    // Find and overwrite the 'chap' fourcc → arbitrary 'XXXX' so the
    // tref entry is no longer recognised as a chapter ref.
    let pat = b"chap";
    let pos = bytes
        .windows(4)
        .position(|w| w == pat)
        .expect("chap in moov");
    bytes[pos..pos + 4].copy_from_slice(b"XXXX");
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open without chap");
    assert_eq!(d.tracks[0].chapter_track_ref(), None);
    assert!(d
        .chapters_for(0)
        .expect("chapter resolution returns Ok(None)")
        .is_none());
}

#[test]
fn chapter_track_id_pointing_at_self_rejects() {
    // Build a movie where the only track points at *itself* via
    // tref/chap. The resolver must reject this with InvalidData (cycle).
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
    push_atom(&mut trak, *b"tref", &build_tref_chap(1)); // self
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

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let mut d = MovDemuxer::open(cur).expect("open self-cycle fixture");
    let err = d.chapters_for(0).expect_err("chapter cycle must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("cycle") || msg.contains("primary"),
        "expected cycle hint, got: {msg}"
    );
}

#[test]
fn chapter_track_id_missing_rejects() {
    // Primary track points at chapter id 99 which does not exist.
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
    push_atom(&mut trak, *b"tref", &build_tref_chap(99));
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

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let mut d = MovDemuxer::open(cur).expect("open dangling-ref fixture");
    let err = d
        .chapters_for(0)
        .expect_err("chapter dangling-ref must reject");
    assert!(format!("{err}").contains("99"));
}

/// Build a baseline `mvhd` v1 — 64-bit creation/modification/duration,
/// 32-bit time_scale. Layout per ISO BMFF §8.2.2.
fn build_mvhd_v1(ts: u32, dur: u64) -> Vec<u8> {
    // 4 (ver+flags) + 8 (creation) + 8 (modification) + 4 (ts) + 8 (dur)
    // + 4 (rate) + 2 (volume) + 10 (reserved) + 36 (matrix) + 24
    // (preview/poster/selection/current) + 4 (next_track_id) = 112.
    let mut p = vec![0u8; 112];
    p[0] = 1; // version=1
              // creation_time @ 4..12 = 0
              // modification_time @ 12..20 = 0
              // time_scale @ 20..24
    p[20..24].copy_from_slice(&ts.to_be_bytes());
    // duration @ 24..32
    p[24..32].copy_from_slice(&dur.to_be_bytes());
    // rate @ 32..36
    p[32..36].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    // volume @ 36..38
    p[36..38].copy_from_slice(&0x0100i16.to_be_bytes());
    // 10 reserved + 36 matrix + 24 pre-defined = 70 bytes (38..108).
    // next_track_id @ 108..112
    p[108..112].copy_from_slice(&2u32.to_be_bytes());
    p
}

#[test]
fn mvhd_v1_64bit_duration_round_trip() {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
    let mdat_off: u32 = 28;

    let mut moov = Vec::new();
    let big_dur: u64 = 0x0000_0001_0000_0000; // 2^32 ticks (would overflow u32)
    push_atom(&mut moov, *b"mvhd", &build_mvhd_v1(600, big_dur));

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

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open mvhd v1 fixture");
    let mvhd = d.mvhd.as_ref().expect("mvhd present");
    assert_eq!(mvhd.version, 1);
    assert_eq!(mvhd.time_scale, 600);
    assert_eq!(mvhd.duration, 0x0000_0001_0000_0000);
    assert_eq!(mvhd.next_track_id, 2);
}
