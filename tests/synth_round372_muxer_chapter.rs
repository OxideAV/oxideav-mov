//! Round 372 — `MovMuxer` write-side **chapter / text track** (QTFF
//! pp. 108–110). The demuxer already reads a QuickTime `text`-handler
//! track (`Track::is_text`, `Track::gmhd`, `parse_text_sample_description`)
//! and resolves a media track's chapters through its `tref/chap`
//! (`MovDemuxer::chapters_for`); round 372 lets the muxer *write* a text
//! track via the new `MuxTrackKind::Text`, whose samples are
//! `[length:u16][UTF-8 text]` records built with `encode_text_sample`.
//!
//! Each test builds a movie through [`MovMuxer`], re-opens it through
//! [`MovDemuxer`], and asserts the chapter list / text description
//! round-trips.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    encode_text_sample, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, TextJustification,
    TextSampleDescription, TrackReference,
};

fn add_video(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 640,
            height: 360,
        },
        600,
        vec![MuxSample {
            data: vec![0x66u8; 8],
            duration: 600,
            keyframe: true,
            composition_offset: 0,
        }],
        &[],
    )
}

/// Build a chapter text track whose samples are the given titles, each
/// `duration` ticks long (media timescale 600).
fn add_chapter_track(m: &mut MovMuxer, titles: &[(&str, u32)]) -> u32 {
    let samples: Vec<MuxSample> = titles
        .iter()
        .map(|(title, dur)| MuxSample {
            data: encode_text_sample(title, None),
            duration: *dur,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    m.add_track(
        MuxTrackKind::Text {
            description: TextSampleDescription::default(),
        },
        600,
        samples,
        &[],
    )
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

#[test]
fn text_track_handler_and_gmhd_roundtrip() {
    let mut m = MovMuxer::new();
    let _ = add_chapter_track(&mut m, &[("Intro", 600)]);
    let bytes = m.encode_to_vec().expect("encode");

    assert!(bytes.windows(4).any(|w| w == b"gmhd"));
    assert!(bytes.windows(4).any(|w| w == b"text"));

    let d = open(bytes);
    let t = &d.tracks[0];
    assert!(t.is_text());
    let g = t.gmhd.as_ref().expect("gmhd parsed");
    assert!(g.gmin.is_some());
    assert!(g.text.is_some(), "text media-info header present");
}

#[test]
fn text_sample_description_roundtrips() {
    let desc = TextSampleDescription {
        display_flags: 0x2000, // anti-alias
        text_justification: TextJustification::Center,
        font_number: 0,
        font_face: 1, // bold
        text_name: "Helvetica".into(),
        ..TextSampleDescription::default()
    };
    let mut m = MovMuxer::new();
    let _ = m.add_track(
        MuxTrackKind::Text {
            description: desc.clone(),
        },
        600,
        vec![MuxSample {
            data: encode_text_sample("X", None),
            duration: 600,
            keyframe: true,
            composition_offset: 0,
        }],
        &[],
    );
    let d = open(m.encode_to_vec().expect("encode"));
    let parsed = d.tracks[0].sample_descriptions[0]
        .text
        .clone()
        .expect("text sample desc");
    assert_eq!(parsed.text_justification, TextJustification::Center);
    assert_eq!(parsed.font_face, 1);
    assert!(parsed.anti_aliased());
    assert_eq!(parsed.text_name, "Helvetica");
}

#[test]
fn chapters_resolve_from_media_track_via_tref_chap() {
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let chap = add_chapter_track(
        &mut m,
        &[("Opening", 600), ("Chapter 2", 1200), ("Finale", 600)],
    );
    m.set_track_references(video, &[TrackReference::chapter(chap)])
        .expect("attach tref/chap");
    let mut d = open(m.encode_to_vec().expect("encode"));

    let list = d.chapters_for(0).expect("read").expect("some chapters");
    assert_eq!(list.entries.len(), 3);
    assert_eq!(list.entries[0].title, "Opening");
    assert_eq!(list.entries[0].start_time, 0);
    assert_eq!(list.entries[0].duration, 600);
    assert_eq!(list.entries[1].title, "Chapter 2");
    assert_eq!(list.entries[1].start_time, 600);
    assert_eq!(list.entries[2].title, "Finale");
    assert_eq!(list.entries[2].start_time, 1800);
}

#[test]
fn chapter_with_encoding_trailer_roundtrips() {
    // encode_text_sample with an encd trailer surfaces text_encoding.
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let sample = MuxSample {
        data: encode_text_sample("Café", Some(0x0000_0100)), // some Mac encoding id
        duration: 600,
        keyframe: true,
        composition_offset: 0,
    };
    let chap = m.add_track(
        MuxTrackKind::Text {
            description: TextSampleDescription::default(),
        },
        600,
        vec![sample],
        &[],
    );
    m.set_track_references(video, &[TrackReference::chapter(chap)])
        .expect("attach");
    let mut d = open(m.encode_to_vec().expect("encode"));
    let list = d.chapters_for(0).expect("read").expect("some");
    assert_eq!(list.entries[0].title, "Café");
    assert_eq!(list.entries[0].text_encoding, Some(0x0000_0100));
}

#[test]
fn unicode_chapter_titles_roundtrip() {
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let chap = add_chapter_track(&mut m, &[("日本語タイトル", 600), ("Ünïcödé", 600)]);
    m.set_track_references(video, &[TrackReference::chapter(chap)])
        .expect("attach");
    let mut d = open(m.encode_to_vec().expect("encode"));
    let list = d.chapters_for(0).expect("read").expect("some");
    assert_eq!(list.entries[0].title, "日本語タイトル");
    assert_eq!(list.entries[1].title, "Ünïcödé");
}

#[test]
fn no_chapter_ref_returns_none() {
    let mut m = MovMuxer::new();
    let _ = add_video(&mut m);
    let _ = add_chapter_track(&mut m, &[("Lonely", 600)]);
    // No tref/chap attached ⇒ media track has no chapters.
    let mut d = open(m.encode_to_vec().expect("encode"));
    assert!(d.chapters_for(0).expect("read").is_none());
}
