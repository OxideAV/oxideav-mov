//! Round 347 — `MovMuxer` write-side **Apple QuickTime Metadata**
//! (`moov/meta` = `hdlr` `mdta` + `keys` + `ilst`) emission.
//!
//! The crate already *reads* the Apple `meta` shape — a movie-level
//! `meta` box carrying a `keys` table and an `ilst` of typed values is
//! decoded by `parse_keys` / `parse_ilst` and surfaced on
//! [`MovDemuxer::meta`] as a list of [`MetaKeyValue`]. Round 347 lets
//! the muxer *write* it via [`MovMuxer::set_apple_metadata`], closing
//! the read/write asymmetry (the legacy `udta` path was already
//! writable since round 334; this is the modern key-value shape).
//!
//! Each test builds a file through [`MovMuxer`], re-opens it through
//! [`MovDemuxer`], and asserts the metadata round-trips exactly
//! (namespace, key, type code, value bytes).

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{
    MovDemuxer, MovMetaItem, MovMuxer, MuxSample, MuxTrackKind, META_NAMESPACE_MDTA,
    META_TYPE_BE_SIGNED_INT, META_TYPE_RAW, META_TYPE_UTF8,
};

/// One single-sample audio track, no metadata yet.
fn one_audio_track(m: &mut MovMuxer) -> u32 {
    let samples = vec![MuxSample {
        data: vec![0x11u8; 8],
        duration: 1024,
        keyframe: true,
        composition_offset: 0,
    }];
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        samples,
        &[],
    )
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

#[test]
fn single_utf8_item_roundtrips() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_apple_metadata(&[MovMetaItem::utf8("com.apple.quicktime.title", "Hello")]);
    let bytes = m.encode_to_vec().expect("encode");

    // The meta / keys / ilst boxes are all in the stream.
    assert!(bytes.windows(4).any(|w| w == b"meta"));
    assert!(bytes.windows(4).any(|w| w == b"keys"));
    assert!(bytes.windows(4).any(|w| w == b"ilst"));

    let d = open(bytes);
    assert_eq!(d.meta.len(), 1);
    let kv = &d.meta[0];
    assert_eq!(kv.namespace, META_NAMESPACE_MDTA);
    assert_eq!(kv.key, "com.apple.quicktime.title");
    assert_eq!(kv.type_code, META_TYPE_UTF8);
    assert_eq!(kv.as_str(), Some("Hello"));
}

#[test]
fn multiple_items_preserve_order_and_index_mapping() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_apple_metadata(&[
        MovMetaItem::utf8("com.apple.quicktime.title", "Title One"),
        MovMetaItem::utf8("com.apple.quicktime.artist", "An Artist"),
        MovMetaItem::utf8("com.apple.quicktime.comment", "A Comment"),
    ]);
    let d = open(m.encode_to_vec().expect("encode"));

    assert_eq!(d.meta.len(), 3);
    // Each ilst entry references the keys slot at the same 1-based index,
    // so the read side resolves the keys back in declaration order.
    assert_eq!(d.meta[0].key, "com.apple.quicktime.title");
    assert_eq!(d.meta[0].as_str(), Some("Title One"));
    assert_eq!(d.meta[1].key, "com.apple.quicktime.artist");
    assert_eq!(d.meta[1].as_str(), Some("An Artist"));
    assert_eq!(d.meta[2].key, "com.apple.quicktime.comment");
    assert_eq!(d.meta[2].as_str(), Some("A Comment"));
}

#[test]
fn signed_int_item_roundtrips_raw_bytes() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_apple_metadata(&[MovMetaItem::signed_int("com.example.rating", -7)]);
    let d = open(m.encode_to_vec().expect("encode"));

    assert_eq!(d.meta.len(), 1);
    let kv = &d.meta[0];
    assert_eq!(kv.key, "com.example.rating");
    assert_eq!(kv.type_code, META_TYPE_BE_SIGNED_INT);
    // Value is a 4-byte big-endian i32.
    assert_eq!(kv.value, (-7i32).to_be_bytes().to_vec());
    assert_eq!(kv.as_str(), None); // not a UTF-8 type code
}

#[test]
fn typed_item_with_custom_namespace_and_raw_value() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    let raw = vec![0xDE, 0xAD, 0xBE, 0xEF];
    m.set_apple_metadata(&[MovMetaItem::typed(
        *b"mdir",
        "com.example.blob",
        META_TYPE_RAW,
        raw.clone(),
    )]);
    let d = open(m.encode_to_vec().expect("encode"));

    assert_eq!(d.meta.len(), 1);
    let kv = &d.meta[0];
    assert_eq!(kv.namespace, *b"mdir");
    assert_eq!(kv.key, "com.example.blob");
    assert_eq!(kv.type_code, META_TYPE_RAW);
    assert_eq!(kv.value, raw);
}

#[test]
fn duplicate_keys_each_get_their_own_slot() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_apple_metadata(&[
        MovMetaItem::utf8("com.example.tag", "first"),
        MovMetaItem::utf8("com.example.tag", "second"),
    ]);
    let d = open(m.encode_to_vec().expect("encode"));

    assert_eq!(d.meta.len(), 2);
    assert_eq!(d.meta[0].as_str(), Some("first"));
    assert_eq!(d.meta[1].as_str(), Some("second"));
}

#[test]
fn apple_meta_does_not_corrupt_sample_data() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_apple_metadata(&[MovMetaItem::utf8("com.apple.quicktime.title", "X")]);
    let mut d = open(m.encode_to_vec().expect("encode"));
    let pkt = d.next_packet().expect("packet");
    assert_eq!(pkt.data, vec![0x11u8; 8]);
}

#[test]
fn no_apple_meta_emits_no_meta_box() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    let bytes = m.encode_to_vec().expect("encode");
    assert!(
        !bytes.windows(4).any(|w| w == b"meta"),
        "no meta box when no Apple metadata attached"
    );
    let d = open(bytes);
    assert!(d.meta.is_empty());
}

#[test]
fn apple_meta_coexists_with_udta() {
    use oxideav_mov::MovMetadata;
    // Both the legacy udta and the modern meta must round-trip together.
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_metadata(&[MovMetadata::intl_text(
        [0xA9, b'n', b'a', b'm'],
        0,
        "Legacy",
    )]);
    m.set_apple_metadata(&[MovMetaItem::utf8("com.apple.quicktime.title", "Modern")]);
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"udta"));
    assert!(bytes.windows(4).any(|w| w == b"meta"));

    let d = open(bytes);
    assert_eq!(d.user_data.len(), 1);
    assert_eq!(d.user_data[0].as_str(), Some("Legacy"));
    assert_eq!(d.meta.len(), 1);
    assert_eq!(d.meta[0].as_str(), Some("Modern"));
}

#[test]
fn track_level_apple_meta_roundtrips() {
    let mut m = MovMuxer::new();
    let id = one_audio_track(&mut m);
    m.set_track_apple_metadata(
        id,
        &[
            MovMetaItem::utf8("com.apple.quicktime.title", "Track Title"),
            MovMetaItem::signed_int("com.example.gain", 42),
        ],
    )
    .expect("attach track apple metadata");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"meta"));

    let d = open(bytes);
    // No movie-level meta.
    assert!(d.meta.is_empty());
    // Track-level meta present in order.
    assert_eq!(d.tracks.len(), 1);
    let t = &d.tracks[0];
    assert_eq!(t.meta.len(), 2);
    assert_eq!(t.meta[0].key, "com.apple.quicktime.title");
    assert_eq!(t.meta[0].as_str(), Some("Track Title"));
    assert_eq!(t.meta[1].key, "com.example.gain");
    assert_eq!(t.meta[1].type_code, META_TYPE_BE_SIGNED_INT);
    assert_eq!(t.meta[1].value, 42i32.to_be_bytes().to_vec());
}

#[test]
fn set_track_apple_metadata_rejects_unknown_track_id() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    let err = m.set_track_apple_metadata(99, &[MovMetaItem::utf8("k", "v")]);
    assert!(err.is_err(), "unknown track id must error");
}

#[test]
fn movie_and_track_apple_meta_independent() {
    let mut m = MovMuxer::new();
    let id = one_audio_track(&mut m);
    m.set_apple_metadata(&[MovMetaItem::utf8("com.example.movie", "M")]);
    m.set_track_apple_metadata(id, &[MovMetaItem::utf8("com.example.track", "T")])
        .expect("attach");
    let d = open(m.encode_to_vec().expect("encode"));

    assert_eq!(d.meta.len(), 1);
    assert_eq!(d.meta[0].key, "com.example.movie");
    assert_eq!(d.meta[0].as_str(), Some("M"));
    assert_eq!(d.tracks[0].meta.len(), 1);
    assert_eq!(d.tracks[0].meta[0].key, "com.example.track");
    assert_eq!(d.tracks[0].meta[0].as_str(), Some("T"));
}
