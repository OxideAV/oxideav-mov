//! Round-2 acceptance: Apple-shaped `meta`, track references, track
//! aperture mode dimensions, audio channel layout.
//!
//! Builds a 2-track QTFF (one video + one audio) plus a movie-level
//! `meta` atom carrying a single key-value pair, and asserts the
//! demuxer surfaces all four families.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, TrackRefKind};

/// Build an Apple-shaped `meta` atom payload carrying a single
/// `com.apple.quicktime.title = "hello"` UTF-8 entry.
fn build_apple_meta_payload() -> Vec<u8> {
    let mut p = Vec::new();
    // hdlr — handler subtype 'mdta' (metadata)
    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    hdlr.extend_from_slice(&0u32.to_be_bytes()); // pre_defined / component_type
    hdlr.extend_from_slice(b"mdta"); // handler_type / component_subtype
    hdlr.extend_from_slice(&[0u8; 12]); // reserved + flags + flags_mask
    hdlr.push(0); // counted-Pascal-string name length 0
    push_atom(&mut p, *b"hdlr", &hdlr);

    // keys — 1 entry, namespace 'mdta', key 'com.apple.quicktime.title'
    let key = b"com.apple.quicktime.title";
    let mut keys = Vec::new();
    keys.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    keys.extend_from_slice(&1u32.to_be_bytes()); // entry count
    let entry_size: u32 = (8 + key.len()) as u32;
    keys.extend_from_slice(&entry_size.to_be_bytes());
    keys.extend_from_slice(b"mdta");
    keys.extend_from_slice(key);
    push_atom(&mut p, *b"keys", &keys);

    // ilst — 1 entry whose atom-type is the 1-based key index, body
    // contains a `data` sub-atom with type=1 (UTF-8) value "hello".
    let value = b"hello";
    let mut data = Vec::new();
    let data_size: u32 = (16 + value.len()) as u32;
    data.extend_from_slice(&data_size.to_be_bytes());
    data.extend_from_slice(b"data");
    data.extend_from_slice(&1u32.to_be_bytes()); // type = utf8
    data.extend_from_slice(&0u32.to_be_bytes()); // locale
    data.extend_from_slice(value);
    let mut ilst = Vec::new();
    let entry_total: u32 = (8 + data.len()) as u32;
    ilst.extend_from_slice(&entry_total.to_be_bytes());
    ilst.extend_from_slice(&1u32.to_be_bytes()); // 1-based key index
    ilst.extend_from_slice(&data);
    push_atom(&mut p, *b"ilst", &ilst);

    p
}

/// Build a `tref` payload with a single `chap` reference to track 2.
fn build_tref_chap(target: u32) -> Vec<u8> {
    let mut p = Vec::new();
    let mut chap = Vec::new();
    chap.extend_from_slice(&target.to_be_bytes());
    push_atom(&mut p, *b"chap", &chap);
    p
}

/// Build a `tapt` payload with clef/prof/enof = 320×240.
fn build_tapt_payload() -> Vec<u8> {
    let mut p = Vec::new();
    let mut dims = vec![0u8; 12];
    dims[4..8].copy_from_slice(&((320u32) << 16).to_be_bytes());
    dims[8..12].copy_from_slice(&((240u32) << 16).to_be_bytes());
    push_atom(&mut p, *b"clef", &dims);
    push_atom(&mut p, *b"prof", &dims);
    push_atom(&mut p, *b"enof", &dims);
    p
}

/// Build a `chan` atom payload — Stereo layout tag (= 100).
fn build_chan_payload() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&100u32.to_be_bytes()); // layout_tag = Stereo
    p.extend_from_slice(&0u32.to_be_bytes()); // bitmap
    p.extend_from_slice(&0u32.to_be_bytes()); // num_descriptions
    p
}

fn build_qt_with_meta_and_audio() -> Vec<u8> {
    let mut out = Vec::new();
    // ftyp
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // mdat — 16 bytes (8 video, 8 audio)
    let payload = b"VID_00001 AU01PC";
    push_atom(&mut out, *b"mdat", payload);
    let mdat_video_offset: u32 = 28;
    let mdat_audio_offset: u32 = 28 + 8;

    // moov
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    // trak 1 — video, with chap → track 2, plus tapt
    let mut trak1 = Vec::new();
    push_atom(&mut trak1, *b"tkhd", &build_tkhd(1, 30, 320, 240));
    let mut tref1 = build_tref_chap(2);
    {
        // tref1 currently is the *body* of the tref atom — wrap.
        let mut wrapped = Vec::new();
        wrapped.append(&mut tref1);
        push_atom(&mut trak1, *b"tref", &wrapped);
    }
    push_atom(&mut trak1, *b"tapt", &build_tapt_payload());

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

    // trak 2 — audio with chan
    let mut trak2 = Vec::new();
    push_atom(&mut trak2, *b"tkhd", &build_tkhd(2, 30, 0, 0));
    let mut mdia2 = Vec::new();
    push_atom(&mut mdia2, *b"mdhd", &build_mdhd(48000, 1500));
    push_atom(&mut mdia2, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
    let mut minf2 = Vec::new();
    {
        // smhd — 8-byte sound media header (4 ver+flags + 2 balance + 2 reserved).
        let smhd = vec![0u8; 8];
        push_atom(&mut minf2, *b"smhd", &smhd);
    }
    let mut stbl2 = Vec::new();
    let chan = {
        let payload = build_chan_payload();
        let mut wrapped = Vec::new();
        push_atom(&mut wrapped, *b"chan", &payload);
        wrapped
    };
    push_atom(
        &mut stbl2,
        *b"stsd",
        &build_stsd_audio(b"sowt", 2, 16, 48000, &chan),
    );
    push_atom(&mut stbl2, *b"stts", &build_stts_single(1, 1500));
    push_atom(&mut stbl2, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl2, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl2, *b"stco", &build_stco_single(mdat_audio_offset));
    push_atom(&mut minf2, *b"stbl", &stbl2);
    push_atom(&mut mdia2, *b"minf", &minf2);
    push_atom(&mut trak2, *b"mdia", &mdia2);
    push_atom(&mut moov, *b"trak", &trak2);

    // moov-level meta
    push_atom(&mut moov, *b"meta", &build_apple_meta_payload());

    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn apple_meta_tref_tapt_chan_round_trip() {
    let bytes = build_qt_with_meta_and_audio();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open with meta+tref+tapt+chan");

    // movie-level meta
    assert_eq!(d.meta.len(), 1);
    let kv = &d.meta[0];
    assert_eq!(kv.key, "com.apple.quicktime.title");
    assert_eq!(kv.as_str(), Some("hello"));

    // 2 tracks
    assert_eq!(d.tracks.len(), 2);
    let video = &d.tracks[0];
    let audio = &d.tracks[1];
    assert!(video.is_video());
    assert!(audio.is_audio());

    // tref on the video track points at audio (track 2) as 'chap'.
    assert_eq!(video.references.len(), 1);
    assert_eq!(video.references[0].kind, TrackRefKind::Chapter);
    assert_eq!(video.references[0].track_ids, vec![2]);

    // tapt on the video track surfaces all three sub-atoms.
    let tapt = video.tapt.expect("tapt parsed");
    let clef = tapt.clef.expect("clef");
    assert_eq!(clef.width(), 320);
    assert_eq!(clef.height(), 240);
    assert!(tapt.prof.is_some());
    assert!(tapt.enof.is_some());

    // chan on the audio sample description.
    let chan = audio.sample_descriptions[0]
        .chan
        .as_ref()
        .expect("chan parsed");
    assert_eq!(chan.layout_tag, 100); // kAudioChannelLayoutTag_Stereo
    assert_eq!(chan.num_descriptions, 0);
}
