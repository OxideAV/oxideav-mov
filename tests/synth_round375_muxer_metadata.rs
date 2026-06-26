//! Round 375 — `MovMuxer` write-side **ISO BMFF timed-metadata track**
//! (`metx` / `mett` / `urim`, ISO/IEC 14496-12 §12.3). The demuxer
//! already reads a `meta`-handler track's `nmhd` null media header and a
//! `metx` / `mett` / `urim` `stsd` sample entry onto
//! `SampleDescription::metadata` (`parse_metadata_sample_entry`); round
//! 375 lets the muxer *write* a complete metadata track via the new
//! `MuxTrackKind::Metadata`.
//!
//! Each test builds a movie through [`MovMuxer`], re-opens it through
//! [`MovDemuxer`], and asserts the `meta` handler, the `nmhd` header, the
//! sample-entry FourCC + typed fields, and per-sample payloads all round
//! trip.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    BitRate, MetadataSampleEntry, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind,
    TextMetadataSampleEntry, UriMetadataSampleEntry, XmlMetadataSampleEntry,
};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn samples(payloads: &[&[u8]]) -> Vec<MuxSample> {
    payloads
        .iter()
        .map(|p| MuxSample {
            data: p.to_vec(),
            duration: 100,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn metadata_track(m: &mut MovMuxer, desc: MetadataSampleEntry, payloads: &[&[u8]]) -> u32 {
    m.add_track(
        MuxTrackKind::Metadata { description: desc },
        1000,
        samples(payloads),
        &[],
    )
}

#[test]
fn metx_xml_metadata_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = XmlMetadataSampleEntry {
        content_encoding: String::new(),
        namespace: "urn:example:meta:2026".into(),
        schema_location: String::new(),
        bitrate: None,
    };
    let _ = metadata_track(
        &mut m,
        MetadataSampleEntry::Xml(entry.clone()),
        &[b"<m a=\"1\"/>", b"<m a=\"2\"/>"],
    );
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"nmhd"));
    assert!(bytes.windows(4).any(|w| w == b"metx"));

    let d = open(bytes);
    let t = &d.tracks[0];
    assert!(t.hdlr.is_metadata());
    let sd = &t.sample_descriptions[0];
    assert_eq!(sd.format, *b"metx");
    match sd.metadata.as_ref().expect("metadata entry") {
        MetadataSampleEntry::Xml(x) => assert_eq!(x, &entry),
        other => panic!("expected Xml, got {other:?}"),
    }
    assert_eq!(t.sample_descriptions.len(), 1);
}

#[test]
fn metx_with_encoding_schema_and_bitrate_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = XmlMetadataSampleEntry {
        content_encoding: "application/zip".into(),
        namespace: "urn:ns:a urn:ns:b".into(),
        schema_location: "http://example.com/a.xsd".into(),
        bitrate: Some(BitRate {
            buffer_size_db: 4096,
            max_bitrate: 128_000,
            avg_bitrate: 96_000,
        }),
    };
    let _ = metadata_track(&mut m, MetadataSampleEntry::Xml(entry.clone()), &[b"x"]);
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    let sd = &d.tracks[0].sample_descriptions[0];
    match sd.metadata.as_ref().expect("metadata") {
        MetadataSampleEntry::Xml(x) => assert_eq!(x, &entry),
        other => panic!("expected Xml, got {other:?}"),
    }
}

#[test]
fn mett_text_metadata_with_config_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = TextMetadataSampleEntry {
        content_encoding: "gzip".into(),
        mime_format: "text/plain".into(),
        text_config: Some("header line".into()),
        bitrate: Some(BitRate {
            buffer_size_db: 1024,
            max_bitrate: 8000,
            avg_bitrate: 4000,
        }),
    };
    let _ = metadata_track(
        &mut m,
        MetadataSampleEntry::Text(entry.clone()),
        &[b"row 1", b"row 2", b"row 3"],
    );
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"mett"));
    assert!(bytes.windows(4).any(|w| w == b"txtC"));
    let d = open(bytes);
    let t = &d.tracks[0];
    let sd = &t.sample_descriptions[0];
    assert_eq!(sd.format, *b"mett");
    match sd.metadata.as_ref().expect("metadata") {
        MetadataSampleEntry::Text(x) => assert_eq!(x, &entry),
        other => panic!("expected Text, got {other:?}"),
    }
    // Three samples laid into mdat round-trip by size.
    let st = &t.sample_table;
    assert_eq!(st.sample_count(), 3);
    for i in 0u32..3 {
        assert_eq!(st.sample_size_at(i), Some(5));
    }
}

#[test]
fn mett_minimal_mime_only_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = TextMetadataSampleEntry {
        content_encoding: String::new(),
        mime_format: "application/json".into(),
        text_config: None,
        bitrate: None,
    };
    let _ = metadata_track(&mut m, MetadataSampleEntry::Text(entry.clone()), &[b"{}"]);
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    match d.tracks[0].sample_descriptions[0]
        .metadata
        .as_ref()
        .expect("metadata")
    {
        MetadataSampleEntry::Text(x) => assert_eq!(x, &entry),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn urim_uri_metadata_with_init_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = UriMetadataSampleEntry {
        the_uri: "urn:mpeg:dash:event:2012".into(),
        init: Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        bitrate: None,
    };
    let _ = metadata_track(
        &mut m,
        MetadataSampleEntry::Uri(entry.clone()),
        &[b"event-blob"],
    );
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"urim"));
    assert!(bytes.windows(4).any(|w| w == b"uri "));
    assert!(bytes.windows(4).any(|w| w == b"uriI"));
    let d = open(bytes);
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.format, *b"urim");
    match sd.metadata.as_ref().expect("metadata") {
        MetadataSampleEntry::Uri(x) => assert_eq!(x, &entry),
        other => panic!("expected Uri, got {other:?}"),
    }
}

#[test]
fn urim_uri_only_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = UriMetadataSampleEntry {
        the_uri: "https://example.com/schema".into(),
        init: None,
        bitrate: None,
    };
    let _ = metadata_track(&mut m, MetadataSampleEntry::Uri(entry.clone()), &[b"a"]);
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    match d.tracks[0].sample_descriptions[0]
        .metadata
        .as_ref()
        .expect("metadata")
    {
        MetadataSampleEntry::Uri(x) => assert_eq!(x, &entry),
        other => panic!("expected Uri, got {other:?}"),
    }
}

#[test]
fn metadata_track_with_cdsc_reference_from_video() {
    use oxideav_mov::TrackReference;
    let mut m = MovMuxer::new();
    let vid = m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 320,
            height: 240,
        },
        600,
        samples(&[b"frame"]),
        &[],
    );
    let meta = metadata_track(
        &mut m,
        MetadataSampleEntry::Text(TextMetadataSampleEntry {
            content_encoding: String::new(),
            mime_format: "text/plain".into(),
            text_config: None,
            bitrate: None,
        }),
        &[b"sidecar"],
    );
    // A media track describes its metadata via tref/cdsc (§12.3.1).
    m.set_track_references(vid, &[TrackReference::to(*b"cdsc", meta)])
        .expect("set tref");
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    assert_eq!(d.tracks.len(), 2);
    assert!(d.tracks[1].hdlr.is_metadata());
    // The video track's cdsc reference resolves to the metadata track.
    let refs = &d.tracks[0].references;
    assert!(refs
        .iter()
        .any(|r| r.fourcc == *b"cdsc" && r.track_ids.contains(&meta)));
}

#[test]
fn metadata_roundtrips_through_fragmented_path() {
    use oxideav_mov::FragmentationMode;
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    let _ = metadata_track(
        &mut m,
        MetadataSampleEntry::Text(TextMetadataSampleEntry {
            content_encoding: String::new(),
            mime_format: "text/plain".into(),
            text_config: None,
            bitrate: None,
        }),
        &[b"s1", b"s2", b"s3"],
    );
    let bytes = m.encode_fragmented_to_vec().expect("encode fragmented");
    assert!(bytes.windows(4).any(|w| w == b"nmhd"));
    assert!(bytes.windows(4).any(|w| w == b"mett"));
    let d = open(bytes);
    assert!(d.tracks[0].hdlr.is_metadata());
    assert_eq!(d.tracks[0].sample_descriptions[0].format, *b"mett");
}
