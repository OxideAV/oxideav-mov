//! Round 379 — `MovMuxer` write-side **Track Kind box** (`kind`,
//! ISO/IEC 14496-12 §8.10.4), the role/kind label the demuxer has long
//! read (`Track::kinds` / `MovDemuxer::track_kinds` via `parse_kind` /
//! `find_kinds_in_udta`) but the muxer could never write.
//! `MovMuxer::set_track_kinds(track_id, &[KindEntry])` emits the boxes
//! into the track-level `udta`.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{KindEntry, MovDemuxer, MovMetadata, MovMuxer, MuxSample, MuxTrackKind};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn text_track(m: &mut MovMuxer) -> u32 {
    use oxideav_mov::SimpleTextSampleEntry;
    let samples = vec![MuxSample {
        data: b"hello".to_vec(),
        duration: 100,
        keyframe: true,
        composition_offset: 0,
    }];
    m.add_track(
        MuxTrackKind::SimpleText {
            description: SimpleTextSampleEntry::default(),
        },
        600,
        samples,
        &[],
    )
}

#[test]
fn single_kind_with_value_round_trips() {
    let kinds = [KindEntry {
        scheme_uri: "urn:mpeg:dash:role:2011".to_string(),
        value: Some("caption".to_string()),
    }];
    let mut m = MovMuxer::new();
    let id = text_track(&mut m);
    m.set_track_kinds(id, &kinds).expect("set kinds");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"kind"));

    let d = open(bytes);
    let got = d.track_kinds(0);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0], kinds[0]);
    assert!(got[0].has_value());
}

#[test]
fn multiple_kinds_round_trip_in_order() {
    let kinds = [
        KindEntry {
            scheme_uri: "https://www.w3.org/TR/webvtt1/".to_string(),
            value: Some("captions".to_string()),
        },
        KindEntry {
            scheme_uri: "urn:mpeg:dash:role:2011".to_string(),
            value: Some("main".to_string()),
        },
        KindEntry {
            scheme_uri: "urn:x:scheme-only".to_string(),
            value: None,
        },
    ];
    let mut m = MovMuxer::new();
    let id = text_track(&mut m);
    m.set_track_kinds(id, &kinds).expect("set kinds");
    let d = open(m.encode_to_vec().expect("encode"));
    let got = d.track_kinds(0);
    assert_eq!(got.len(), 3);
    assert_eq!(got, kinds);
    assert!(!got[2].has_value());
}

#[test]
fn kind_coexists_with_metadata_in_udta() {
    // The track udta must carry both the metadata item and the kind box.
    let mut m = MovMuxer::new();
    let id = text_track(&mut m);
    m.set_track_metadata(
        id,
        &[MovMetadata::plain_utf8(
            *b"name",
            MovMetadata::iso_language(*b"eng"),
            "My Track",
        )],
    )
    .expect("set meta");
    m.set_track_kinds(
        id,
        &[KindEntry {
            scheme_uri: "urn:mpeg:dash:role:2011".to_string(),
            value: Some("subtitle".to_string()),
        }],
    )
    .expect("set kinds");
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(d.track_kinds(0).len(), 1);
    assert_eq!(d.track_kinds(0)[0].value.as_deref(), Some("subtitle"));
    // Metadata still present on the track.
    assert!(!d.tracks[0].user_data.is_empty());
}

#[test]
fn empty_kinds_emit_no_box() {
    let mut m = MovMuxer::new();
    let id = text_track(&mut m);
    m.set_track_kinds(
        id,
        &[KindEntry {
            scheme_uri: "urn:x".to_string(),
            value: None,
        }],
    )
    .expect("set");
    m.set_track_kinds(id, &[]).expect("clear");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(!bytes.windows(4).any(|w| w == b"kind"));
    let d = open(bytes);
    assert!(d.track_kinds(0).is_empty());
}

#[test]
fn kind_unknown_track_errors() {
    let mut m = MovMuxer::new();
    assert!(m
        .set_track_kinds(
            9,
            &[KindEntry {
                scheme_uri: "urn:x".to_string(),
                value: None
            }]
        )
        .is_err());
}
