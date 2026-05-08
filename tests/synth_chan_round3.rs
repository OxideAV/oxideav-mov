//! Round-3 acceptance: full `chan` channel-description list parsing,
//! tref `chap` / `tmcd` accessors surfaced on `Track`, and `cslg` cross
//! validation against `ctts`.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

fn build_chan_use_descriptions(num: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&0u32.to_be_bytes()); // layout_tag = UseChannelDescriptions
    p.extend_from_slice(&0u32.to_be_bytes()); // bitmap
    p.extend_from_slice(&num.to_be_bytes()); // num_descriptions
    for label in 1..=num {
        p.extend_from_slice(&label.to_be_bytes()); // label
        p.extend_from_slice(&0u32.to_be_bytes()); // flags
        p.extend_from_slice(&0f32.to_be_bytes());
        p.extend_from_slice(&0f32.to_be_bytes());
        p.extend_from_slice(&0f32.to_be_bytes());
    }
    p
}

fn build_qt_with_chan_descs() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let payload = b"AUDIODAT";
    push_atom(&mut out, *b"mdat", payload);
    let mdat_payload_offset: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(48000, 1500));

    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 1500, 0, 0));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(48000, 1500));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"smhd", &[0u8; 8]);
    let mut stbl = Vec::new();

    // Build chan with 2 channel descriptions (Left, Right).
    let chan_payload = build_chan_use_descriptions(2);
    let chan = {
        let mut wrapped = Vec::new();
        push_atom(&mut wrapped, *b"chan", &chan_payload);
        wrapped
    };
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_audio(b"sowt", 2, 16, 48000, &chan),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 1500));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn chan_with_use_descriptions_emits_per_channel_records() {
    let bytes = build_qt_with_chan_descs();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open chan with descriptions");
    let chan = d.tracks[0].sample_descriptions[0]
        .chan
        .as_ref()
        .expect("chan parsed");
    assert_eq!(chan.layout_tag, 0); // UseChannelDescriptions
    assert_eq!(chan.num_descriptions, 2);
    assert_eq!(chan.channel_descriptions.len(), 2);
    assert_eq!(chan.channel_descriptions[0].label, 1);
    assert_eq!(chan.channel_descriptions[1].label, 2);
    // Mask synthesised from labels: bit 0 = Left, bit 1 = Right.
    assert_eq!(chan.channel_mask(), Some(0b11));
    assert_eq!(chan.channel_count(), 2);
}

/// Build a 2-track movie: track 1 video with `tref/chap → 2`,
/// track 2 a text/chapter track. Verify Track::chapter_track_ref
/// surfaces 2.
fn build_qt_with_chap_tmcd() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"VIDEODATCHAPDAT_");
    let mdat_video_offset: u32 = 28;
    let mdat_chap_offset: u32 = 28 + 8;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    // trak 1: video + tref { chap → 2, tmcd → 3 }
    let mut trak1 = Vec::new();
    push_atom(&mut trak1, *b"tkhd", &build_tkhd(1, 30, 320, 240));
    let mut tref_body = Vec::new();
    push_atom(&mut tref_body, *b"chap", &2u32.to_be_bytes());
    push_atom(&mut tref_body, *b"tmcd", &3u32.to_be_bytes());
    push_atom(&mut trak1, *b"tref", &tref_body);
    let mut mdia1 = Vec::new();
    push_atom(&mut mdia1, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia1, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf1 = Vec::new();
    push_atom(&mut minf1, *b"vmhd", &build_vmhd());
    let mut stbl1 = Vec::new();
    push_atom(
        &mut stbl1,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl1, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl1, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl1, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl1, *b"stco", &build_stco_single(mdat_video_offset));
    push_atom(&mut minf1, *b"stbl", &stbl1);
    push_atom(&mut mdia1, *b"minf", &minf1);
    push_atom(&mut trak1, *b"mdia", &mdia1);
    push_atom(&mut moov, *b"trak", &trak1);

    // trak 2: text/chapter track (subtype "text")
    let mut trak2 = Vec::new();
    push_atom(&mut trak2, *b"tkhd", &build_tkhd(2, 30, 0, 0));
    let mut mdia2 = Vec::new();
    push_atom(&mut mdia2, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia2, *b"hdlr", &build_hdlr(b"mhlr", b"text"));
    let mut minf2 = Vec::new();
    let mut stbl2 = Vec::new();
    // Use a "text" 16-byte stsd entry — universal-only.
    let mut stsd_text = Vec::new();
    stsd_text.extend_from_slice(&0u32.to_be_bytes());
    stsd_text.extend_from_slice(&1u32.to_be_bytes());
    stsd_text.extend_from_slice(&16u32.to_be_bytes()); // entry size
    stsd_text.extend_from_slice(b"text");
    stsd_text.extend_from_slice(&[0u8; 6]);
    stsd_text.extend_from_slice(&1u16.to_be_bytes());
    push_atom(&mut stbl2, *b"stsd", &stsd_text);
    push_atom(&mut stbl2, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl2, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl2, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl2, *b"stco", &build_stco_single(mdat_chap_offset));
    push_atom(&mut minf2, *b"stbl", &stbl2);
    push_atom(&mut mdia2, *b"minf", &minf2);
    push_atom(&mut trak2, *b"mdia", &mdia2);
    push_atom(&mut moov, *b"trak", &trak2);

    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn track_chap_tmcd_accessors_resolve_track_ids() {
    let bytes = build_qt_with_chap_tmcd();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open chap+tmcd");
    assert_eq!(d.tracks.len(), 2);
    let video = &d.tracks[0];
    assert_eq!(video.chapter_track_ref(), Some(2));
    assert_eq!(video.timecode_track_ref(), Some(3));
    // The audio (track 2) has no chap reference.
    assert!(d.tracks[1].chapter_track_ref().is_none());
}

fn build_cslg_v0(shift: i32, least: i32, greatest: i32, start: i32, end: i32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&shift.to_be_bytes());
    p.extend_from_slice(&least.to_be_bytes());
    p.extend_from_slice(&greatest.to_be_bytes());
    p.extend_from_slice(&start.to_be_bytes());
    p.extend_from_slice(&end.to_be_bytes());
    p
}

fn build_ctts_v1(runs: &[(u32, i32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(1);
    p.extend_from_slice(&[0, 0, 0]);
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, off) in runs {
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&off.to_be_bytes());
    }
    p
}

fn build_qt_with_ctts_v1_and_cslg(cslg: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"\x01\x02\x03\x04\x05\x06\x07\x08");
    let mdat_payload_offset: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));

    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 120, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 120));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(4, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(4));
    // ctts v1: 1 sample @ -3, 2 samples @ +5, 1 sample @ 0
    push_atom(
        &mut stbl,
        *b"ctts",
        &build_ctts_v1(&[(1, -3), (2, 5), (1, 0)]),
    );
    push_atom(&mut stbl, *b"cslg", &cslg);
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(2, 4));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn cslg_v0_consistent_with_ctts_v1_round_trip() {
    let cslg = build_cslg_v0(3, -3, 5, 0, 120);
    let bytes = build_qt_with_ctts_v1_and_cslg(cslg);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open ctts+cslg");
    let track = &d.tracks[0];
    let c = track.cslg.expect("cslg parsed");
    assert_eq!(c.composition_to_dts_shift, 3);
    assert_eq!(c.least_decode_to_display_delta, -3);
    assert_eq!(c.greatest_decode_to_display_delta, 5);
    assert_eq!(c.composition_end_time, 120);
}

#[test]
fn cslg_inconsistent_with_ctts_rejects() {
    // ctts goes -3..+5 but cslg lies and claims 0..1 — must reject.
    let cslg = build_cslg_v0(0, 0, 1, 0, 120);
    let bytes = build_qt_with_ctts_v1_and_cslg(cslg);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    if MovDemuxer::open(cur).is_ok() {
        panic!("cslg/ctts mismatch must reject");
    }
}
