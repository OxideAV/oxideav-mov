//! Round 334 — `MovMuxer` write-side user-data metadata (`udta`)
//! emission at both movie (`moov/udta`) and track (`trak/udta`) scope
//! (QTFF pp. 36–38 / ISO/IEC 14496-12 §8.10.1).
//!
//! The crate already *reads* `udta` (Apple international-text `©XXX`
//! records, QuickTime-7+ `name`/`auth`/`cprt` UTF-8 entries, and
//! opaque `Unknown` payloads, all surfaced on
//! [`MovDemuxer::user_data`] / [`Track::user_data`]). Round 334 lets
//! the muxer *write* them via [`MovMuxer::set_metadata`] (movie scope)
//! and [`MovMuxer::set_track_metadata`] (track scope), with same-FourCC
//! international-text items coalesced into a single multi-language atom.
//!
//! These tests build a file through [`MovMuxer`], then re-open it
//! through [`MovDemuxer`] and assert the metadata round-trips through
//! the read side exactly (FourCC, language, decoded text).

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{
    iso_language_tag, MovDemuxer, MovMetadata, MovMuxer, MuxSample, MuxTrackKind, UserDataKind,
    UTF8_INTL_TEXT_FLAG,
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
fn movie_level_intl_text_roundtrips() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_metadata(&[
        MovMetadata::intl_text([0xA9, b'n', b'a', b'm'], 0, "My Movie"),
        MovMetadata::intl_text([0xA9, b'c', b'p', b'y'], 0, "Karpeles Lab"),
    ]);
    let bytes = m.encode_to_vec().expect("encode");

    // The udta box is in the stream.
    assert!(bytes.windows(4).any(|w| w == b"udta"));

    let d = open(bytes);
    assert_eq!(d.user_data.len(), 2);
    assert_eq!(d.user_data[0].fourcc, [0xA9, b'n', b'a', b'm']);
    assert_eq!(d.user_data[0].as_str(), Some("My Movie"));
    assert!(d.user_data[0].is_international_text());
    assert_eq!(d.user_data[1].fourcc, [0xA9, b'c', b'p', b'y']);
    assert_eq!(d.user_data[1].as_str(), Some("Karpeles Lab"));
}

#[test]
fn same_fourcc_intl_text_coalesces_into_multilanguage_atom() {
    // Two ©nam items (English Mac-Roman + French ISO-UTF8) must share
    // ONE ©nam atom but surface as TWO read-side entries, in order.
    let fra = UTF8_INTL_TEXT_FLAG | MovMetadata::iso_language(*b"fra");
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_metadata(&[
        MovMetadata::intl_text([0xA9, b'n', b'a', b'm'], 0, "Title"),
        MovMetadata::intl_text([0xA9, b'n', b'a', b'm'], fra, "Titre"),
    ]);
    let bytes = m.encode_to_vec().expect("encode");

    // Exactly one ©nam atom (the coalescing).
    let nam_count = bytes
        .windows(4)
        .filter(|w| *w == [0xA9, b'n', b'a', b'm'])
        .count();
    assert_eq!(nam_count, 1, "two same-FourCC items must coalesce");

    let d = open(bytes);
    assert_eq!(d.user_data.len(), 2);
    assert_eq!(d.user_data[0].as_str(), Some("Title"));
    assert_eq!(d.user_data[1].as_str(), Some("Titre"));
    // The French record decoded as UTF-8 (high bit set on its lang).
    match &d.user_data[1].kind {
        UserDataKind::InternationalText { language, .. } => {
            assert_eq!(language & UTF8_INTL_TEXT_FLAG, UTF8_INTL_TEXT_FLAG);
            assert_eq!(iso_language_tag(language & 0x7FFF), Some(*b"fra"));
        }
        _ => panic!("expected international text"),
    }
}

#[test]
fn movie_level_plain_utf8_roundtrips() {
    let eng = MovMetadata::iso_language(*b"eng");
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_metadata(&[
        MovMetadata::plain_utf8(*b"name", eng, "Movie Name"),
        MovMetadata::plain_utf8(*b"auth", eng, "An Author"),
    ]);
    let d = open(m.encode_to_vec().expect("encode"));

    assert_eq!(d.user_data.len(), 2);
    assert_eq!(d.user_data[0].fourcc, *b"name");
    assert_eq!(d.user_data[0].as_str(), Some("Movie Name"));
    match &d.user_data[0].kind {
        UserDataKind::PlainUtf8 { language, .. } => {
            assert_eq!(iso_language_tag(*language), Some(*b"eng"));
        }
        _ => panic!("expected PlainUtf8"),
    }
    assert_eq!(d.user_data[1].fourcc, *b"auth");
    assert_eq!(d.user_data[1].as_str(), Some("An Author"));
}

#[test]
fn raw_item_roundtrips_as_unknown() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_metadata(&[MovMetadata::raw(*b"vndr", vec![1u8, 2, 3, 4, 5])]);
    let d = open(m.encode_to_vec().expect("encode"));

    assert_eq!(d.user_data.len(), 1);
    assert_eq!(d.user_data[0].fourcc, *b"vndr");
    match &d.user_data[0].kind {
        UserDataKind::Unknown(b) => assert_eq!(b, &[1, 2, 3, 4, 5]),
        _ => panic!("expected Unknown"),
    }
    assert!(d.user_data[0].as_str().is_none());
}

#[test]
fn track_level_metadata_roundtrips() {
    let mut m = MovMuxer::new();
    let id = one_audio_track(&mut m);
    m.set_track_metadata(
        id,
        &[MovMetadata::intl_text(
            [0xA9, b'n', b'a', b'm'],
            0,
            "Track Title",
        )],
    )
    .expect("attach track metadata");
    let d = open(m.encode_to_vec().expect("encode"));

    // No movie-level udta.
    assert!(d.user_data.is_empty());
    // Track-level udta present.
    assert_eq!(d.tracks.len(), 1);
    let t = &d.tracks[0];
    assert_eq!(t.user_data.len(), 1);
    assert_eq!(t.user_data[0].fourcc, [0xA9, b'n', b'a', b'm']);
    assert_eq!(t.user_data[0].as_str(), Some("Track Title"));
}

#[test]
fn metadata_does_not_corrupt_sample_data() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    m.set_metadata(&[MovMetadata::intl_text([0xA9, b'n', b'a', b'm'], 0, "X")]);
    let mut d = open(m.encode_to_vec().expect("encode"));
    let pkt = d.next_packet().expect("packet");
    assert_eq!(pkt.data, vec![0x11u8; 8]);
}

#[test]
fn set_track_metadata_rejects_unknown_track_id() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    let err = m.set_track_metadata(99, &[MovMetadata::raw(*b"vndr", vec![0u8])]);
    assert!(err.is_err(), "unknown track id must error");
}

#[test]
fn no_metadata_emits_no_udta() {
    let mut m = MovMuxer::new();
    let _ = one_audio_track(&mut m);
    let bytes = m.encode_to_vec().expect("encode");
    assert!(
        !bytes.windows(4).any(|w| w == b"udta"),
        "no udta when no metadata attached"
    );
}

#[test]
fn iso_language_is_bare_inverse_of_read_decoder() {
    // The helper produces the bare packed value (no high bit) — the
    // exact inverse of the read-side iso_language_tag.
    let packed = MovMetadata::iso_language(*b"deu");
    assert_eq!(packed & 0x8000, 0, "no high bit on the bare packed value");
    assert_eq!(iso_language_tag(packed), Some(*b"deu"));
}
