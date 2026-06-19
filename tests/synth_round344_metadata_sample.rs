//! Round 344 acceptance: ISO BMFF timed-metadata sample entries
//! (`metx` / `mett` / `urim`, ISO/IEC 14496-12 §12.3.3) surfaced at
//! the demuxer through [`oxideav_mov::SampleDescription::metadata`].
//!
//! Builds single-track files whose `hdlr` declares the `meta` component
//! subtype and whose `stsd` carries one `MetaDataSampleEntry` subclass,
//! then asserts the demuxer routes the body through the new
//! [`oxideav_mov::metadata_sample`] parser into a typed
//! [`oxideav_mov::MetadataSampleEntry`].
//!
//! Spec source consulted: ISO/IEC 14496-12:2015 §12.3.3.2 (syntax) and
//! §8.5.2.2 (BitRateBox). No external implementation read.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MetadataSampleEntry, MovDemuxer};

/// Build a `meta`-handler `hdlr` body. The metadata media handler uses
/// component type `mhlr` (QuickTime) with subtype `meta`.
fn build_meta_hdlr() -> Vec<u8> {
    build_hdlr(b"mhlr", b"meta")
}

/// Build a child box (`size` + FourCC + body) for nesting inside a
/// metadata sample entry.
fn child_box(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    let size = (8 + body.len()) as u32;
    b.extend_from_slice(&size.to_be_bytes());
    b.extend_from_slice(fourcc);
    b.extend_from_slice(body);
    b
}

/// FullBox child: 4-byte version+flags then `body`.
fn fullbox_child(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut inner = Vec::new();
    inner.extend_from_slice(&0u32.to_be_bytes());
    inner.extend_from_slice(body);
    child_box(fourcc, &inner)
}

/// Build a `btrt` BitRateBox child.
fn btrt_child(buffer: u32, max: u32, avg: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&buffer.to_be_bytes());
    body.extend_from_slice(&max.to_be_bytes());
    body.extend_from_slice(&avg.to_be_bytes());
    child_box(b"btrt", &body)
}

/// Build a `stsd` with a single metadata sample entry. `subclass_body`
/// is everything after the 8-byte SampleEntry tail (6 reserved bytes +
/// 2-byte data_reference_index).
fn build_stsd_metadata(format: &[u8; 4], subclass_body: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&1u32.to_be_bytes()); // entry count
    let entry_size: u32 = (16 + subclass_body.len()) as u32;
    p.extend_from_slice(&entry_size.to_be_bytes());
    p.extend_from_slice(format);
    p.extend_from_slice(&[0u8; 6]); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    p.extend_from_slice(subclass_body);
    p
}

/// Wrap a metadata `stsd` into a complete single-track QTFF file.
fn build_metadata_file(stsd_body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();

    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let payload = b"METADATA";
    push_atom(&mut out, *b"mdat", payload);
    let mdat_payload_offset: u32 = 28; // ftyp(20) + mdat header(8)

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));

    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 0, 0));

    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_meta_hdlr());

    let mut minf = Vec::new();
    // Null media header (nmhd): FullBox with no payload (ISO/IEC
    // 14496-12 §8.4.5.2) — the media header for metadata tracks.
    push_atom(&mut minf, *b"nmhd", &0u32.to_be_bytes());

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
    MovDemuxer::open(cur).expect("open metadata track")
}

#[test]
fn metx_surfaces_xml_metadata_entry() {
    // metx: content_encoding (omitted) + namespace + schema_location
    // (omitted) + btrt.
    let mut body = Vec::new();
    body.push(0); // content_encoding = "" (NUL)
    body.extend_from_slice(b"urn:mpeg:dash:schema:mpd:2011");
    body.push(0); // namespace
    body.push(0); // schema_location = ""
    body.extend_from_slice(&btrt_child(4096, 64_000, 32_000));
    let stsd = build_stsd_metadata(b"metx", &body);

    let d = open(build_metadata_file(&stsd));
    let desc = &d.tracks[0].sample_descriptions[0];
    assert_eq!(&desc.format, b"metx");
    assert!(d.tracks[0].hdlr.is_metadata());
    match desc.metadata.as_ref().expect("metx parsed") {
        MetadataSampleEntry::Xml(x) => {
            assert_eq!(x.content_encoding, "");
            assert_eq!(x.namespace, "urn:mpeg:dash:schema:mpd:2011");
            assert_eq!(x.schema_location, "");
            let br = x.bitrate.expect("btrt present");
            assert_eq!(br.buffer_size_db, 4096);
            assert_eq!(br.max_bitrate, 64_000);
            assert_eq!(br.avg_bitrate, 32_000);
        }
        other => panic!("expected Xml, got {other:?}"),
    }
}

#[test]
fn mett_surfaces_text_metadata_entry_with_txtc() {
    // mett: content_encoding (omitted) + mime_format + txtC.
    let mut body = Vec::new();
    body.push(0); // content_encoding = ""
    body.extend_from_slice(b"application/x-mpeg-cc");
    body.push(0); // mime_format
    let mut txtc_body = Vec::new();
    txtc_body.extend_from_slice(b"WEBVTT");
    txtc_body.push(0);
    body.extend_from_slice(&fullbox_child(b"txtC", &txtc_body));
    let stsd = build_stsd_metadata(b"mett", &body);

    let d = open(build_metadata_file(&stsd));
    let desc = &d.tracks[0].sample_descriptions[0];
    assert_eq!(&desc.format, b"mett");
    match desc.metadata.as_ref().expect("mett parsed") {
        MetadataSampleEntry::Text(t) => {
            assert_eq!(t.content_encoding, "");
            assert_eq!(t.mime_format, "application/x-mpeg-cc");
            assert_eq!(t.text_config.as_deref(), Some("WEBVTT"));
            assert!(t.bitrate.is_none());
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn urim_surfaces_uri_metadata_entry() {
    // urim: uri box + uriI box + btrt.
    let mut body = Vec::new();
    let mut uri_body = Vec::new();
    uri_body.extend_from_slice(b"https://example.org/meta/form");
    uri_body.push(0);
    body.extend_from_slice(&fullbox_child(b"uri ", &uri_body));
    body.extend_from_slice(&fullbox_child(b"uriI", &[0xDE, 0xAD, 0xBE, 0xEF]));
    body.extend_from_slice(&btrt_child(0, 128_000, 96_000));
    let stsd = build_stsd_metadata(b"urim", &body);

    let d = open(build_metadata_file(&stsd));
    let desc = &d.tracks[0].sample_descriptions[0];
    assert_eq!(&desc.format, b"urim");
    match desc.metadata.as_ref().expect("urim parsed") {
        MetadataSampleEntry::Uri(u) => {
            assert_eq!(u.the_uri, "https://example.org/meta/form");
            assert_eq!(u.init.as_deref(), Some(&[0xDE, 0xAD, 0xBE, 0xEF][..]));
            assert_eq!(u.bitrate.expect("btrt").avg_bitrate, 96_000);
        }
        other => panic!("expected Uri, got {other:?}"),
    }
}

#[test]
fn non_metadata_handler_leaves_metadata_field_unset() {
    // A video track that happens to carry a four-letter format does not
    // populate the metadata field; the existing video parse path runs.
    let extras: Vec<u8> = Vec::new();
    let stsd = build_stsd_video(b"avc1", 320, 240, &extras);

    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"mdat", b"METADATA");
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
    assert!(desc.metadata.is_none());
    assert!(d.tracks[0].hdlr.is_video());
}
