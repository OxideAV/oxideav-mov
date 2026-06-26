//! Round 372 — `MovMuxer` write-side per-track language: the packed
//! `mdhd.language` ISO-639-2/T code and the `elng` Extended Language
//! Tag Box (ISO/IEC 14496-12 §8.4.2.3 / §8.4.6). The demuxer already
//! reads both (`Track::mdhd.language`, `Track::extended_language`);
//! round 372 lets the muxer write them via
//! [`MovMuxer::set_track_language`] /
//! [`MovMuxer::set_track_extended_language`].

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    iso_language_tag, MovDemuxer, MovMetadata, MovMuxer, MuxSample, MuxTrackKind, MDHD_LANGUAGE_UND,
};

fn add_audio(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        vec![MuxSample {
            data: vec![0x77u8; 8],
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        }],
        &[],
    )
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

#[test]
fn default_language_is_und() {
    let mut m = MovMuxer::new();
    let _ = add_audio(&mut m);
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(d.tracks[0].mdhd.language, MDHD_LANGUAGE_UND);
    assert_eq!(
        iso_language_tag(d.tracks[0].mdhd.language),
        Some([b'u', b'n', b'd'])
    );
    assert!(d.tracks[0].extended_language.is_none());
}

#[test]
fn iso639_language_roundtrips() {
    let mut m = MovMuxer::new();
    let a = add_audio(&mut m);
    m.set_track_language(a, MovMetadata::iso_language(*b"eng"))
        .expect("set lang");
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(
        iso_language_tag(d.tracks[0].mdhd.language),
        Some([b'e', b'n', b'g'])
    );
}

#[test]
fn several_languages_roundtrip() {
    for code in [b"deu", b"fra", b"jpn", b"spa"] {
        let mut m = MovMuxer::new();
        let a = add_audio(&mut m);
        m.set_track_language(a, MovMetadata::iso_language(*code))
            .expect("set lang");
        let d = open(m.encode_to_vec().expect("encode"));
        assert_eq!(iso_language_tag(d.tracks[0].mdhd.language), Some(*code));
    }
}

#[test]
fn extended_language_roundtrips() {
    let mut m = MovMuxer::new();
    let a = add_audio(&mut m);
    m.set_track_extended_language(a, "en-US").expect("set elng");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"elng"));

    let d = open(bytes);
    assert_eq!(d.tracks[0].extended_language.as_deref(), Some("en-US"));
}

#[test]
fn extended_language_with_script_subtag_roundtrips() {
    let mut m = MovMuxer::new();
    let a = add_audio(&mut m);
    m.set_track_extended_language(a, "zh-Hant-HK")
        .expect("set elng");
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(d.tracks[0].extended_language.as_deref(), Some("zh-Hant-HK"));
}

#[test]
fn empty_extended_language_clears_box() {
    let mut m = MovMuxer::new();
    let a = add_audio(&mut m);
    m.set_track_extended_language(a, "fr-CA").expect("set");
    m.set_track_extended_language(a, "").expect("clear");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(
        !bytes.windows(4).any(|w| w == b"elng"),
        "empty tag clears the elng box"
    );
    let d = open(bytes);
    assert!(d.tracks[0].extended_language.is_none());
}

#[test]
fn language_and_extended_language_coexist() {
    let mut m = MovMuxer::new();
    let a = add_audio(&mut m);
    m.set_track_language(a, MovMetadata::iso_language(*b"eng"))
        .expect("set lang");
    m.set_track_extended_language(a, "en-GB").expect("set elng");
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(
        iso_language_tag(d.tracks[0].mdhd.language),
        Some([b'e', b'n', b'g'])
    );
    assert_eq!(d.tracks[0].extended_language.as_deref(), Some("en-GB"));
}

#[test]
fn set_language_rejects_unknown_track() {
    let mut m = MovMuxer::new();
    let _ = add_audio(&mut m);
    assert!(m.set_track_language(9, 0).is_err());
    assert!(m.set_track_extended_language(9, "en").is_err());
}
