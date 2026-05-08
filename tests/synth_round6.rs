//! Round-6 acceptance: alias-chain following (`open_with_aliases`),
//! `tmcd` sample-description parsing inside `stsd` (distinct from the
//! `tmcd > tcmi` shape inside `gmhd` covered in round 5), and `encd`
//! text-encoding-override extension on chapter-text samples.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, TMCD_FLAG_DROP_FRAME};

/// Build a minimal QuickTime file carrying one timecode track whose
/// sample-description is `tmcd` (the in-stsd shape) — number_of_frames
/// = 30, drop-frame flag set, 30000/1001 timing.
fn build_movie_with_tmcd_in_stsd() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // mdat with one 4-byte counter sample.
    let sample = 0x01_02_03_04u32.to_be_bytes();
    push_atom(&mut out, *b"mdat", &sample);
    let mdat_payload_off: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    // Track: handler 'tmcd', sample format 'tmcd'.
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 0, 0));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(30000, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"tmcd"));
    let mut minf = Vec::new();
    let mut stbl = Vec::new();

    // stsd: 1 entry. universal 16 bytes + 20 bytes tmcd body + name
    // source-reference user data atom.
    let name = b"Source-Tape";
    let mut name_atom = Vec::new();
    name_atom.extend_from_slice(&((8 + name.len()) as u32).to_be_bytes());
    name_atom.extend_from_slice(b"name");
    name_atom.extend_from_slice(name);

    let mut tmcd_body = Vec::new();
    tmcd_body.extend_from_slice(&0u32.to_be_bytes()); // reserved
    tmcd_body.extend_from_slice(&TMCD_FLAG_DROP_FRAME.to_be_bytes()); // flags
    tmcd_body.extend_from_slice(&30000u32.to_be_bytes()); // time_scale
    tmcd_body.extend_from_slice(&1001u32.to_be_bytes()); // frame_duration
    tmcd_body.push(30); // number_of_frames
    tmcd_body.extend_from_slice(&[0u8; 3]); // reserved 24-bit
    tmcd_body.extend_from_slice(&name_atom);

    let entry_size: u32 = (16 + tmcd_body.len()) as u32;
    let mut stsd = Vec::new();
    stsd.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    stsd.extend_from_slice(&1u32.to_be_bytes()); // entry count
    stsd.extend_from_slice(&entry_size.to_be_bytes());
    stsd.extend_from_slice(b"tmcd");
    stsd.extend_from_slice(&[0u8; 6]); // reserved
    stsd.extend_from_slice(&1u16.to_be_bytes()); // dref index
    stsd.extend_from_slice(&tmcd_body);
    push_atom(&mut stbl, *b"stsd", &stsd);
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(4, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_off));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn tmcd_sample_description_inside_stsd_decodes_fields() {
    let bytes = build_movie_with_tmcd_in_stsd();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open tmcd-stsd fixture");
    assert_eq!(d.tracks.len(), 1);
    let track = &d.tracks[0];
    assert!(track.is_timecode());
    let desc = track.sample_descriptions.first().expect("stsd entry");
    assert_eq!(&desc.format, b"tmcd");
    let tmcd = desc.tmcd.as_ref().expect("tmcd parsed");
    assert!(tmcd.is_drop_frame());
    assert_eq!(tmcd.time_scale, 30000);
    assert_eq!(tmcd.frame_duration, 1001);
    assert_eq!(tmcd.number_of_frames, 30);
    assert_eq!(tmcd.source_name.as_deref(), Some("Source-Tape"));
}

/// Build a chapter-track movie where each text sample carries a
/// trailing `encd` atom announcing the encoding ID.
fn build_movie_with_encd_chapter() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // Build a chapter sample: "Hello" + encd[0x0500].
    let txt = b"Hello";
    let mut sample = Vec::new();
    sample.extend_from_slice(&(txt.len() as u16).to_be_bytes());
    sample.extend_from_slice(txt);
    sample.extend_from_slice(&12u32.to_be_bytes()); // encd atom size
    sample.extend_from_slice(b"encd");
    sample.extend_from_slice(&0x0000_0500u32.to_be_bytes()); // utf8 ID

    let video = b"VID!";
    let mut mdat_body = Vec::new();
    mdat_body.extend_from_slice(video);
    mdat_body.extend_from_slice(&sample);
    let mdat_payload_off = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &mdat_body);
    let video_off = mdat_payload_off;
    let chap_off = mdat_payload_off + video.len() as u32;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 60));

    // Track 1: video with tref/chap → 2.
    let mut trak1 = Vec::new();
    push_atom(&mut trak1, *b"tkhd", &build_tkhd(1, 60, 320, 240));
    let mut tref = Vec::new();
    push_atom(&mut tref, *b"chap", &2u32.to_be_bytes());
    push_atom(&mut trak1, *b"tref", &tref);
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
    push_atom(&mut stbl1, *b"stsz", &build_stsz_constant(4, 1));
    push_atom(&mut stbl1, *b"stco", &build_stco_single(video_off));
    push_atom(&mut minf1, *b"stbl", &stbl1);
    push_atom(&mut mdia1, *b"minf", &minf1);
    push_atom(&mut trak1, *b"mdia", &mdia1);
    push_atom(&mut moov, *b"trak", &trak1);

    // Track 2: text chapter with the encd sample.
    let mut trak2 = Vec::new();
    push_atom(&mut trak2, *b"tkhd", &build_tkhd(2, 60, 0, 0));
    let mut mdia2 = Vec::new();
    push_atom(&mut mdia2, *b"mdhd", &build_mdhd(1000, 60));
    push_atom(&mut mdia2, *b"hdlr", &build_hdlr(b"mhlr", b"text"));
    let mut minf2 = Vec::new();
    let mut stbl2 = Vec::new();
    // Minimum text stsd: 16 universal + 51 body
    let mut stsd_text = Vec::new();
    stsd_text.extend_from_slice(&0u32.to_be_bytes());
    stsd_text.extend_from_slice(&1u32.to_be_bytes());
    let entry_size: u32 = 16 + 51;
    stsd_text.extend_from_slice(&entry_size.to_be_bytes());
    stsd_text.extend_from_slice(b"text");
    stsd_text.extend_from_slice(&[0u8; 6]);
    stsd_text.extend_from_slice(&1u16.to_be_bytes());
    stsd_text.extend_from_slice(&[0u8; 51]);
    push_atom(&mut stbl2, *b"stsd", &stsd_text);
    push_atom(&mut stbl2, *b"stts", &build_stts_single(1, 60));
    push_atom(&mut stbl2, *b"stsc", &build_stsc_single(1));
    let mut stsz = Vec::new();
    stsz.extend_from_slice(&0u32.to_be_bytes());
    stsz.extend_from_slice(&0u32.to_be_bytes());
    stsz.extend_from_slice(&1u32.to_be_bytes());
    stsz.extend_from_slice(&(sample.len() as u32).to_be_bytes());
    push_atom(&mut stbl2, *b"stsz", &stsz);
    push_atom(&mut stbl2, *b"stco", &build_stco_single(chap_off));
    push_atom(&mut minf2, *b"stbl", &stbl2);
    push_atom(&mut mdia2, *b"minf", &minf2);
    push_atom(&mut trak2, *b"mdia", &mdia2);
    push_atom(&mut moov, *b"trak", &trak2);

    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn chapter_text_sample_with_encd_surfaces_encoding_id() {
    let bytes = build_movie_with_encd_chapter();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open encd fixture");
    let cl = d
        .chapters_for(0)
        .expect("chapter resolution")
        .expect("chapter list");
    assert_eq!(cl.entries.len(), 1);
    assert_eq!(cl.entries[0].title, "Hello");
    assert_eq!(cl.entries[0].text_encoding, Some(0x0000_0500));
}

/// Build a reference-only movie pointing at `target_url`.
fn build_reference_only(target_url: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));

    // rmra > rmda > rdrf 'url '
    let mut url_buf = Vec::new();
    url_buf.extend_from_slice(target_url.as_bytes());
    url_buf.push(0); // NUL terminator
    let mut rdrf = Vec::new();
    rdrf.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
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

/// Build a regular self-contained QT movie (one video track, one
/// 8-byte sample). Returns the bytes plus the expected sample payload.
fn build_self_contained_movie() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
    let mdat_payload_off: u32 = 28;
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
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_off));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn open_with_aliases_follows_one_url_hop() {
    let target_url = "memory:./target.mov";
    let alias_bytes = build_reference_only(target_url);
    let target_bytes = build_self_contained_movie();
    let target_clone = target_bytes.clone();

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(alias_bytes));
    let opener = move |url: &str| -> std::io::Result<Box<dyn ReadSeek>> {
        if url == target_url {
            Ok(Box::new(Cursor::new(target_clone.clone())))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "url not handled",
            ))
        }
    };
    let mut d = MovDemuxer::open_with_aliases(cur, opener).expect("alias hop succeeds");
    // The resolved target has 1 track + 1 sample.
    assert_eq!(d.tracks.len(), 1);
    assert!(d.tracks[0].is_video());
    let (idx, _, data) = d.read_next().expect("read sample from target");
    assert_eq!(idx, 0);
    assert_eq!(data, b"PAYLOAD!".to_vec());
    let _ = target_bytes;
}

#[test]
fn open_with_aliases_passes_through_self_contained_input() {
    // When the file is *not* a reference-only movie, open_with_aliases
    // should never call the opener.
    let bytes = build_self_contained_movie();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let opener = |_url: &str| -> std::io::Result<Box<dyn ReadSeek>> {
        panic!("opener must not be called for self-contained input")
    };
    let d = MovDemuxer::open_with_aliases(cur, opener).expect("self-contained passes through");
    assert_eq!(d.tracks.len(), 1);
}

#[test]
fn open_with_aliases_fails_when_no_alias_resolves() {
    let alias_bytes = build_reference_only("memory:./missing.mov");
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(alias_bytes));
    let opener = |_url: &str| -> std::io::Result<Box<dyn ReadSeek>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "all targets unreachable",
        ))
    };
    let err = match MovDemuxer::open_with_aliases(cur, opener) {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("alias") || msg.contains("alternate") || msg.contains("reference"),
        "unexpected error: {msg}"
    );
}

#[test]
fn open_with_aliases_refuses_two_hop_chain() {
    // Alias pointing at another alias-only file → second hop must fail.
    let inner_url = "memory:./inner.mov";
    let outer = build_reference_only("memory:./outer.mov");
    let inner = build_reference_only(inner_url);
    let inner_clone = inner.clone();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(outer));
    let opener = move |url: &str| -> std::io::Result<Box<dyn ReadSeek>> {
        if url == "memory:./outer.mov" {
            Ok(Box::new(Cursor::new(inner_clone.clone())))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "deeper url",
            ))
        }
    };
    let err = match MovDemuxer::open_with_aliases(cur, opener) {
        Ok(_) => panic!("two-hop must reject"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("reference-movie") || msg.contains("alias") || msg.contains("alternate"),
        "unexpected error: {msg}"
    );
    let _ = inner;
}

#[test]
fn probe_reference_movies_finds_url_in_rmra() {
    use oxideav_mov::DataReference;
    let bytes = build_reference_only("memory:./somewhere.mov");
    let mut cur = Cursor::new(bytes);
    let refs = MovDemuxer::probe_reference_movies(&mut cur).expect("probe ok");
    assert_eq!(refs.len(), 1);
    match refs[0].data_ref.as_ref().expect("data_ref") {
        DataReference::Url(s) => assert_eq!(s, "memory:./somewhere.mov"),
        _ => panic!("expected URL data reference"),
    }
}
