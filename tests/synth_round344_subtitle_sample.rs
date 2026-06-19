//! Round 344 acceptance: ISO BMFF subtitle sample entries (`stpp` /
//! `sbtt`, ISO/IEC 14496-12 §12.6.3) surfaced at the demuxer through
//! [`oxideav_mov::SampleDescription::subtitle`].
//!
//! Builds single-track files whose `hdlr` declares the `subt` subtitle
//! component subtype and whose `stsd` carries one `SubtitleSampleEntry`
//! subclass, then asserts the demuxer routes the body through the
//! [`oxideav_mov::metadata_sample`] parser into a typed
//! [`oxideav_mov::SubtitleSampleEntry`].
//!
//! Spec source consulted: ISO/IEC 14496-12:2015 §12.6.3.2. No external
//! implementation read.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, SubtitleSampleEntry};

fn child_box(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    b.extend_from_slice(fourcc);
    b.extend_from_slice(body);
    b
}

fn fullbox_child(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut inner = Vec::new();
    inner.extend_from_slice(&0u32.to_be_bytes());
    inner.extend_from_slice(body);
    child_box(fourcc, &inner)
}

fn btrt_child(buffer: u32, max: u32, avg: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&buffer.to_be_bytes());
    body.extend_from_slice(&max.to_be_bytes());
    body.extend_from_slice(&avg.to_be_bytes());
    child_box(b"btrt", &body)
}

fn build_stsd_subtitle(format: &[u8; 4], subclass_body: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(&((16 + subclass_body.len()) as u32).to_be_bytes());
    p.extend_from_slice(format);
    p.extend_from_slice(&[0u8; 6]);
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(subclass_body);
    p
}

fn build_subtitle_file(stsd_body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();

    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    push_atom(&mut out, *b"mdat", b"SUBTITLE");
    let mdat_payload_offset: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 0, 0));

    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"subt"));

    let mut minf = Vec::new();
    // Subtitle Media Header (sthd): empty FullBox (§12.6.2.2).
    push_atom(&mut minf, *b"sthd", &0u32.to_be_bytes());

    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", stsd_body);
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
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

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open subtitle track")
}

#[test]
fn stpp_surfaces_xml_subtitle_entry() {
    let mut body = Vec::new();
    body.extend_from_slice(b"http://www.w3.org/ns/ttml");
    body.push(0); // namespace
    body.extend_from_slice(b"ttml.xsd");
    body.push(0); // schema_location
    body.extend_from_slice(b"image/png");
    body.push(0); // auxiliary_mime_types
    body.extend_from_slice(&btrt_child(2048, 48_000, 24_000));
    let stsd = build_stsd_subtitle(b"stpp", &body);

    let d = open(build_subtitle_file(&stsd));
    let desc = &d.tracks[0].sample_descriptions[0];
    assert_eq!(&desc.format, b"stpp");
    assert!(d.tracks[0].hdlr.is_subtitle());
    match desc.subtitle.as_ref().expect("stpp parsed") {
        SubtitleSampleEntry::Xml(x) => {
            assert_eq!(x.namespace, "http://www.w3.org/ns/ttml");
            assert_eq!(x.schema_location, "ttml.xsd");
            assert_eq!(x.auxiliary_mime_types, "image/png");
            assert_eq!(x.bitrate.expect("btrt").avg_bitrate, 24_000);
        }
        other => panic!("expected Xml, got {other:?}"),
    }
}

#[test]
fn sbtt_surfaces_text_subtitle_entry_with_txtc() {
    let mut body = Vec::new();
    body.push(0); // content_encoding = ""
    body.extend_from_slice(b"text/plain");
    body.push(0); // mime_format
    let mut txtc_body = Vec::new();
    txtc_body.extend_from_slice(b"hdr");
    txtc_body.push(0);
    body.extend_from_slice(&fullbox_child(b"txtC", &txtc_body));
    let stsd = build_stsd_subtitle(b"sbtt", &body);

    let d = open(build_subtitle_file(&stsd));
    let desc = &d.tracks[0].sample_descriptions[0];
    assert_eq!(&desc.format, b"sbtt");
    match desc.subtitle.as_ref().expect("sbtt parsed") {
        SubtitleSampleEntry::Text(t) => {
            assert_eq!(t.content_encoding, "");
            assert_eq!(t.mime_format, "text/plain");
            assert_eq!(t.text_config.as_deref(), Some("hdr"));
            assert!(t.bitrate.is_none());
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn non_subtitle_handler_leaves_subtitle_field_unset() {
    // metx on a metadata handler is recognised as metadata, not subtitle.
    let mut body = Vec::new();
    body.extend_from_slice(b"urn:ns");
    body.push(0);
    let mut stsd = Vec::new();
    stsd.extend_from_slice(&0u32.to_be_bytes());
    stsd.extend_from_slice(&1u32.to_be_bytes());
    stsd.extend_from_slice(&((16 + body.len()) as u32).to_be_bytes());
    stsd.extend_from_slice(b"metx");
    stsd.extend_from_slice(&[0u8; 6]);
    stsd.extend_from_slice(&1u16.to_be_bytes());
    stsd.extend_from_slice(&body);

    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"mdat", b"SUBTITLE");
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 0, 0));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"meta"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"nmhd", &0u32.to_be_bytes());
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &stsd);
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(28));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);

    let d = open(out);
    let desc = &d.tracks[0].sample_descriptions[0];
    assert!(desc.subtitle.is_none());
    assert!(desc.metadata.is_some());
}
