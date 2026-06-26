//! Round 375 — `MovMuxer` write-side **ISO BMFF subtitle track**
//! (`stpp` / `sbtt`, ISO/IEC 14496-12 §12.6). The demuxer already reads a
//! `subt`-handler track's `sthd` Subtitle Media Header and a `stpp` /
//! `sbtt` `stsd` sample entry onto `SampleDescription::subtitle`
//! (`parse_subtitle_sample_entry`); round 375 lets the muxer *write* a
//! complete subtitle track via the new `MuxTrackKind::Subtitle`.
//!
//! Each test builds a movie through [`MovMuxer`], re-opens it through
//! [`MovDemuxer`], and asserts the `subt` handler, the `sthd` header, the
//! sample-entry FourCC + typed fields, and per-sample payloads round
//! trip.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    BitRate, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, SubtitleSampleEntry,
    TextSubtitleSampleEntry, XmlSubtitleSampleEntry,
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

fn subtitle_track(m: &mut MovMuxer, desc: SubtitleSampleEntry, payloads: &[&[u8]]) -> u32 {
    m.add_track(
        MuxTrackKind::Subtitle { description: desc },
        1000,
        samples(payloads),
        &[],
    )
}

#[test]
fn stpp_xml_subtitle_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = XmlSubtitleSampleEntry {
        namespace: "http://www.w3.org/ns/ttml".into(),
        schema_location: String::new(),
        auxiliary_mime_types: String::new(),
        bitrate: None,
    };
    let _ = subtitle_track(
        &mut m,
        SubtitleSampleEntry::Xml(entry.clone()),
        &[b"<tt>one</tt>", b"<tt>two</tt>"],
    );
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"sthd"));
    assert!(bytes.windows(4).any(|w| w == b"stpp"));

    let d = open(bytes);
    let t = &d.tracks[0];
    assert!(t.is_subtitle());
    let sd = &t.sample_descriptions[0];
    assert_eq!(sd.format, *b"stpp");
    match sd.subtitle.as_ref().expect("subtitle entry") {
        SubtitleSampleEntry::Xml(x) => assert_eq!(x, &entry),
        other => panic!("expected Xml, got {other:?}"),
    }
}

#[test]
fn stpp_with_schema_aux_and_bitrate_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = XmlSubtitleSampleEntry {
        namespace: "http://www.w3.org/ns/ttml".into(),
        schema_location: "http://example.com/ttml.xsd".into(),
        auxiliary_mime_types: "image/png font/woff".into(),
        bitrate: Some(BitRate {
            buffer_size_db: 2048,
            max_bitrate: 64_000,
            avg_bitrate: 48_000,
        }),
    };
    let _ = subtitle_track(&mut m, SubtitleSampleEntry::Xml(entry.clone()), &[b"x"]);
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    match d.tracks[0].sample_descriptions[0]
        .subtitle
        .as_ref()
        .expect("subtitle")
    {
        SubtitleSampleEntry::Xml(x) => assert_eq!(x, &entry),
        other => panic!("expected Xml, got {other:?}"),
    }
}

#[test]
fn stpp_namespace_and_schema_only_roundtrips() {
    // schema present but auxiliary_mime_types empty ⇒ exactly two strings.
    let mut m = MovMuxer::new();
    let entry = XmlSubtitleSampleEntry {
        namespace: "urn:ns".into(),
        schema_location: "urn:schema".into(),
        auxiliary_mime_types: String::new(),
        bitrate: None,
    };
    let _ = subtitle_track(&mut m, SubtitleSampleEntry::Xml(entry.clone()), &[b"a"]);
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    match d.tracks[0].sample_descriptions[0]
        .subtitle
        .as_ref()
        .expect("subtitle")
    {
        SubtitleSampleEntry::Xml(x) => assert_eq!(x, &entry),
        other => panic!("expected Xml, got {other:?}"),
    }
}

#[test]
fn sbtt_text_subtitle_with_config_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = TextSubtitleSampleEntry {
        content_encoding: "gzip".into(),
        mime_format: "text/plain".into(),
        text_config: Some("default style".into()),
        bitrate: Some(BitRate {
            buffer_size_db: 512,
            max_bitrate: 4000,
            avg_bitrate: 2000,
        }),
    };
    let _ = subtitle_track(
        &mut m,
        SubtitleSampleEntry::Text(entry.clone()),
        &[b"line a", b"line b"],
    );
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"sbtt"));
    assert!(bytes.windows(4).any(|w| w == b"txtC"));
    let d = open(bytes);
    let t = &d.tracks[0];
    assert_eq!(t.sample_descriptions[0].format, *b"sbtt");
    match t.sample_descriptions[0]
        .subtitle
        .as_ref()
        .expect("subtitle")
    {
        SubtitleSampleEntry::Text(x) => assert_eq!(x, &entry),
        other => panic!("expected Text, got {other:?}"),
    }
    let st = &t.sample_table;
    assert_eq!(st.sample_count(), 2);
}

#[test]
fn sbtt_minimal_mime_only_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = TextSubtitleSampleEntry {
        content_encoding: String::new(),
        mime_format: "text/vtt".into(),
        text_config: None,
        bitrate: None,
    };
    let _ = subtitle_track(
        &mut m,
        SubtitleSampleEntry::Text(entry.clone()),
        &[b"WEBVTT"],
    );
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    match d.tracks[0].sample_descriptions[0]
        .subtitle
        .as_ref()
        .expect("subtitle")
    {
        SubtitleSampleEntry::Text(x) => assert_eq!(x, &entry),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn subtitle_roundtrips_through_fragmented_path() {
    use oxideav_mov::FragmentationMode;
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    let _ = subtitle_track(
        &mut m,
        SubtitleSampleEntry::Xml(XmlSubtitleSampleEntry {
            namespace: "http://www.w3.org/ns/ttml".into(),
            ..Default::default()
        }),
        &[b"s1", b"s2", b"s3"],
    );
    let bytes = m.encode_fragmented_to_vec().expect("encode fragmented");
    assert!(bytes.windows(4).any(|w| w == b"sthd"));
    assert!(bytes.windows(4).any(|w| w == b"stpp"));
    let d = open(bytes);
    assert!(d.tracks[0].is_subtitle());
    assert_eq!(d.tracks[0].sample_descriptions[0].format, *b"stpp");
}
