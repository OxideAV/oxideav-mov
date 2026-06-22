//! Round-360 acceptance: QuickTime Text Sample Description (`text`
//! format inside `stsd` on a `text`-handler track, QTFF pp. 108–110)
//! is parsed into typed fields by the demuxer.
//!
//! Builds a minimal QuickTime movie with a single classic text track
//! whose sample description carries a non-trivial display config:
//! centered justification, drop-shadow + anti-alias display flags, a
//! bold/italic font face, distinct fore/background 48-bit RGB colours,
//! a non-zero default text box, and a trailing Pascal font name. Opens
//! it through the demuxer and asserts every field surfaces on
//! `SampleDescription::text`.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::text_sample::{
    Rgb48, TextBox, TextJustification, TEXT_FACE_BOLD, TEXT_FACE_ITALIC, TEXT_FLAG_ANTI_ALIAS,
    TEXT_FLAG_DROP_SHADOW, TEXT_SAMPLE_DESC_FIXED_LEN,
};
use oxideav_mov::MovDemuxer;

/// Build a fully-populated QuickTime Text Sample Description body
/// (the 43-byte fixed fields + a trailing Pascal font name), then wrap
/// it in a single-entry `stsd`.
fn build_stsd_text_full() -> Vec<u8> {
    let mut body = vec![0u8; TEXT_SAMPLE_DESC_FIXED_LEN];
    // display_flags = drop shadow | anti-alias
    body[0..4].copy_from_slice(&(TEXT_FLAG_DROP_SHADOW | TEXT_FLAG_ANTI_ALIAS).to_be_bytes());
    // text_justification = center (1)
    body[4..8].copy_from_slice(&1i32.to_be_bytes());
    // background_color = (0x1000, 0x2000, 0x3000)
    body[8..10].copy_from_slice(&0x1000u16.to_be_bytes());
    body[10..12].copy_from_slice(&0x2000u16.to_be_bytes());
    body[12..14].copy_from_slice(&0x3000u16.to_be_bytes());
    // default_text_box = (top 10, left 20, bottom 110, right 220)
    body[14..16].copy_from_slice(&10u16.to_be_bytes());
    body[16..18].copy_from_slice(&20u16.to_be_bytes());
    body[18..20].copy_from_slice(&110u16.to_be_bytes());
    body[20..22].copy_from_slice(&220u16.to_be_bytes());
    // font_number = 0 (must be 0 per spec)
    // font_face = bold | italic
    body[32..34].copy_from_slice(&(TEXT_FACE_BOLD | TEXT_FACE_ITALIC).to_be_bytes());
    // foreground_color = (0xFFFF, 0xFFFF, 0xFFFF)
    body[37..39].copy_from_slice(&0xFFFFu16.to_be_bytes());
    body[39..41].copy_from_slice(&0xFFFFu16.to_be_bytes());
    body[41..43].copy_from_slice(&0xFFFFu16.to_be_bytes());
    // trailing Pascal font name
    let name = b"Helvetica";
    body.push(name.len() as u8);
    body.extend_from_slice(name);

    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&1u32.to_be_bytes()); // entry count
    let entry_size: u32 = (16 + body.len()) as u32;
    p.extend_from_slice(&entry_size.to_be_bytes());
    p.extend_from_slice(b"text");
    p.extend_from_slice(&[0u8; 6]); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // dref index
    p.extend_from_slice(&body);
    p
}

fn build_text_movie() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // One text sample in mdat.
    let mut sample = Vec::new();
    sample.extend_from_slice(&5u16.to_be_bytes());
    sample.extend_from_slice(b"Hello");
    let mdat_payload_off = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &sample);
    let sample_off = mdat_payload_off;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 60));

    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 60, 0, 0));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 60));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"text"));
    let mut minf = Vec::new();
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &build_stsd_text_full());
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 60));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(
        &mut stbl,
        *b"stsz",
        &build_stsz_constant(sample.len() as u32, 1),
    );
    push_atom(&mut stbl, *b"stco", &build_stco_single(sample_off));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);

    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn text_sample_description_surfaces_typed_fields() {
    let bytes = build_text_movie();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open text fixture");

    assert_eq!(d.tracks.len(), 1);
    let track = &d.tracks[0];
    assert!(track.is_text());
    assert_eq!(track.sample_descriptions.len(), 1);
    let sd = &track.sample_descriptions[0];
    assert_eq!(&sd.format, b"text");

    let t = sd.text.as_ref().expect("text sample description parsed");
    assert_eq!(t.text_justification, TextJustification::Center);
    assert!(t.has_drop_shadow());
    assert!(t.anti_aliased());
    assert!(!t.use_movie_background());
    assert!(t.is_bold());
    assert!(t.is_italic());
    assert!(!t.is_underline());
    assert_eq!(
        t.background_color,
        Rgb48 {
            red: 0x1000,
            green: 0x2000,
            blue: 0x3000
        }
    );
    assert_eq!(
        t.foreground_color,
        Rgb48 {
            red: 0xFFFF,
            green: 0xFFFF,
            blue: 0xFFFF
        }
    );
    assert_eq!(
        t.default_text_box,
        TextBox {
            top: 10,
            left: 20,
            bottom: 110,
            right: 220
        }
    );
    assert_eq!(t.font_number, 0);
    assert_eq!(t.text_name, "Helvetica");
}

#[test]
fn non_text_handler_leaves_text_none() {
    // A `text` FourCC under a video handler must not populate the
    // QuickTime text description (the handler gate guards it).
    let mut out = Vec::new();
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 60));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 60, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 60));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 60));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(40));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open video fixture");
    assert!(d.tracks[0].sample_descriptions[0].text.is_none());
}
