//! Round 379 — `MovMuxer` write-side **Track Group box** (`trgr`,
//! ISO/IEC 14496-12 §8.3.4), the track-grouping membership declaration
//! the demuxer has long read (`Track::track_groups` /
//! `MovDemuxer::track_group_entries` / `track_groups` via `parse_trgr` /
//! `parse_track_group_type`) but the muxer could never write.
//! `MovMuxer::set_track_groups(track_id, &[TrackGroupTypeEntry])` emits
//! the `trgr` container + framed `TrackGroupTypeBox` children.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, TrackGroupTypeEntry, TRACK_GROUP_TYPE_MSRC,
};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn video_track(m: &mut MovMuxer) -> u32 {
    let samples: Vec<MuxSample> = (0..2)
        .map(|i| MuxSample {
            data: vec![(i as u8).wrapping_add(1); 8],
            duration: 100,
            keyframe: i == 0,
            composition_offset: 0,
        })
        .collect();
    m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 16,
            height: 16,
        },
        600,
        samples,
        &[],
    )
}

#[test]
fn msrc_group_round_trips() {
    let groups = [TrackGroupTypeEntry::msrc(42)];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_groups(id, &groups).expect("set groups");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"trgr"));
    assert!(bytes.windows(4).any(|w| w == b"msrc"));

    let d = open(bytes);
    let got = d.track_group_entries(0);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0], groups[0]);
    assert!(got[0].is_msrc());
    assert_eq!(got[0].track_group_id, 42);
    assert_eq!(got[0].track_group_type, TRACK_GROUP_TYPE_MSRC);
}

#[test]
fn multiple_groups_round_trip_in_order() {
    let groups = [
        TrackGroupTypeEntry::msrc(7),
        TrackGroupTypeEntry {
            track_group_type: *b"ster",
            track_group_id: 9,
            version: 0,
            flags: 0,
            payload: Vec::new(),
        },
    ];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_groups(id, &groups).expect("set groups");
    let d = open(m.encode_to_vec().expect("encode"));
    let got = d.track_group_entries(0);
    assert_eq!(got.len(), 2);
    assert_eq!(got, groups);
}

#[test]
fn group_with_type_specific_payload_round_trips() {
    let groups = [TrackGroupTypeEntry {
        track_group_type: *b"vndr",
        track_group_id: 100,
        version: 0,
        flags: 0,
        payload: vec![0xAA, 0xBB, 0xCC, 0xDD],
    }];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_groups(id, &groups).expect("set groups");
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(d.track_group_entries(0), groups);
}

#[test]
fn two_tracks_in_same_group_resolve_via_track_groups() {
    let mut m = MovMuxer::new();
    let a = video_track(&mut m);
    let b = video_track(&mut m);
    m.set_track_groups(a, &[TrackGroupTypeEntry::msrc(5)])
        .expect("a");
    m.set_track_groups(b, &[TrackGroupTypeEntry::msrc(5)])
        .expect("b");
    let d = open(m.encode_to_vec().expect("encode"));
    // The dual lookup groups both track indices under one (type, id).
    let groups = d.track_groups();
    let msrc5 = groups
        .iter()
        .find(|((ty, id), _)| *ty == TRACK_GROUP_TYPE_MSRC && *id == 5)
        .expect("msrc 5 group present");
    assert_eq!(msrc5.1, vec![0, 1]);
}

#[test]
fn empty_groups_emit_no_box() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_groups(id, &[TrackGroupTypeEntry::msrc(1)])
        .expect("set");
    m.set_track_groups(id, &[]).expect("clear");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(!bytes.windows(4).any(|w| w == b"trgr"));
    let d = open(bytes);
    assert!(d.track_group_entries(0).is_empty());
}

#[test]
fn group_unknown_track_errors() {
    let mut m = MovMuxer::new();
    assert!(m
        .set_track_groups(9, &[TrackGroupTypeEntry::msrc(1)])
        .is_err());
}
