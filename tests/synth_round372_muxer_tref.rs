//! Round 372 — `MovMuxer` write-side Track Reference Box (`tref`)
//! emission (QTFF p. 50 / ISO/IEC 14496-12 §8.3.3).
//!
//! The crate already *reads* `tref`, surfacing typed accessors per
//! reference kind ([`Track::references`], `chapter_track_ref`,
//! `timecode_track_ref`, `MovDemuxer::tref_track_indices`). Round 372
//! lets the muxer *write* them via
//! [`MovMuxer::set_track_references`], one child atom per
//! [`TrackReference`] (FourCC = reference type, body = packed `u32`
//! referenced track ids).
//!
//! Each test builds a file through [`MovMuxer`], re-opens it through
//! [`MovDemuxer`], and asserts the references round-trip exactly.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, TrackRefKind, TrackReference};

fn one_sample() -> Vec<MuxSample> {
    vec![MuxSample {
        data: vec![0x22u8; 8],
        duration: 1024,
        keyframe: true,
        composition_offset: 0,
    }]
}

fn add_video(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 320,
            height: 240,
        },
        30000,
        one_sample(),
        &[],
    )
}

fn add_text(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        one_sample(),
        &[],
    )
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

#[test]
fn single_chapter_reference_roundtrips() {
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let chap = add_text(&mut m);
    m.set_track_references(video, &[TrackReference::chapter(chap)])
        .expect("attach chap ref");
    let bytes = m.encode_to_vec().expect("encode");

    assert!(bytes.windows(4).any(|w| w == b"tref"));
    assert!(bytes.windows(4).any(|w| w == b"chap"));

    let d = open(bytes);
    let vid = &d.tracks[0];
    assert_eq!(vid.references.len(), 1);
    assert_eq!(vid.references[0].kind, TrackRefKind::Chapter);
    assert_eq!(vid.references[0].track_ids, vec![chap]);
    assert_eq!(vid.chapter_track_ref(), Some(chap));
}

#[test]
fn timecode_reference_resolves_via_index() {
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let tc = add_text(&mut m);
    m.set_track_references(video, &[TrackReference::timecode(tc)])
        .expect("attach tmcd ref");
    let d = open(m.encode_to_vec().expect("encode"));

    // tc is the 2nd track ⇒ 0-based index 1.
    assert_eq!(d.timecode_track_index(0), Some(1));
    assert_eq!(d.tracks[0].timecode_track_ref(), Some(tc));
}

#[test]
fn multiple_reference_types_on_one_track() {
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let chap = add_text(&mut m);
    let tc = add_text(&mut m);
    m.set_track_references(
        video,
        &[
            TrackReference::chapter(chap),
            TrackReference::timecode(tc),
            TrackReference::to(*b"sync", video),
        ],
    )
    .expect("attach refs");
    let d = open(m.encode_to_vec().expect("encode"));

    let refs = &d.tracks[0].references;
    assert_eq!(refs.len(), 3);
    assert_eq!(d.tracks[0].chapter_track_ref(), Some(chap));
    assert_eq!(d.tracks[0].timecode_track_ref(), Some(tc));
    // Sync references resolve to the declaring track id (self-ref legal).
    let sync = refs
        .iter()
        .find(|r| r.kind == TrackRefKind::Sync)
        .expect("sync ref");
    assert_eq!(sync.track_ids, vec![video]);
}

#[test]
fn reference_with_multiple_track_ids() {
    let mut m = MovMuxer::new();
    let a = add_video(&mut m);
    let b = add_text(&mut m);
    let c = add_text(&mut m);
    m.set_track_references(
        a,
        &[TrackReference {
            reference_type: *b"sync",
            track_ids: vec![b, c],
        }],
    )
    .expect("attach multi-id ref");
    let d = open(m.encode_to_vec().expect("encode"));

    let sync = &d.tracks[0].references[0];
    assert_eq!(sync.kind, TrackRefKind::Sync);
    assert_eq!(sync.track_ids, vec![b, c]);
}

#[test]
fn no_references_emits_no_tref() {
    let mut m = MovMuxer::new();
    let _ = add_video(&mut m);
    let bytes = m.encode_to_vec().expect("encode");
    assert!(
        !bytes.windows(4).any(|w| w == b"tref"),
        "no tref when no references attached"
    );
}

#[test]
fn unknown_referenced_track_id_is_rejected() {
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let err = m.set_track_references(video, &[TrackReference::chapter(99)]);
    assert!(err.is_err(), "reference to unknown track id must error");
}

#[test]
fn zero_referenced_track_id_is_rejected() {
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let err = m.set_track_references(
        video,
        &[TrackReference {
            reference_type: *b"sync",
            track_ids: vec![0],
        }],
    );
    assert!(err.is_err(), "reference to track id 0 must error");
}

#[test]
fn unknown_declaring_track_id_is_rejected() {
    let mut m = MovMuxer::new();
    let _ = add_video(&mut m);
    let err = m.set_track_references(42, &[TrackReference::chapter(1)]);
    assert!(err.is_err(), "unknown declaring track id must error");
}

#[test]
fn references_do_not_corrupt_sample_data() {
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let chap = add_text(&mut m);
    m.set_track_references(video, &[TrackReference::chapter(chap)])
        .expect("attach");
    let mut d = open(m.encode_to_vec().expect("encode"));
    let pkt = d.next_packet().expect("packet");
    assert_eq!(pkt.data, vec![0x22u8; 8]);
}
