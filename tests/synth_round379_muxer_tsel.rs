//! Round 379 — `MovMuxer` write-side **Track Selection box** (`tsel`,
//! ISO/IEC 14496-12 §8.10.3), the adaptive-switching descriptor the
//! demuxer has long read (`Track::track_selection` /
//! `MovDemuxer::track_selection` via `parse_tsel` / `find_tsel_in_udta`)
//! but the muxer could never write.
//! `MovMuxer::set_track_selection(track_id, Some(TrackSelection))` emits
//! the box into the track-level `udta`.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    KindEntry, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, TrackSelection, TSEL_ATTR_BITRATE,
    TSEL_ATTR_CODEC,
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
fn tsel_with_attributes_round_trips() {
    let sel = TrackSelection {
        switch_group: 7,
        attributes: vec![TSEL_ATTR_CODEC, TSEL_ATTR_BITRATE],
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_selection(id, Some(sel.clone()))
        .expect("set tsel");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"tsel"));

    let d = open(bytes);
    let got = d.track_selection(0).expect("tsel present");
    assert_eq!(*got, sel);
    assert_eq!(got.switch_group, 7);
    assert!(got.has_attribute(&TSEL_ATTR_CODEC));
    assert!(got.is_informative());
}

#[test]
fn tsel_negative_switch_group_round_trips() {
    let sel = TrackSelection {
        switch_group: -42,
        attributes: Vec::new(),
    };
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_selection(id, Some(sel.clone()))
        .expect("set tsel");
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(*d.track_selection(0).unwrap(), sel);
}

#[test]
fn tsel_coexists_with_kind_in_udta() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_kinds(
        id,
        &[KindEntry {
            scheme_uri: "urn:mpeg:dash:role:2011".to_string(),
            value: Some("alternate".to_string()),
        }],
    )
    .expect("set kinds");
    m.set_track_selection(
        id,
        Some(TrackSelection {
            switch_group: 1,
            attributes: vec![TSEL_ATTR_BITRATE],
        }),
    )
    .expect("set tsel");
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(d.track_kinds(0).len(), 1);
    let sel = d.track_selection(0).expect("tsel present");
    assert_eq!(sel.switch_group, 1);
    assert_eq!(sel.attributes, vec![TSEL_ATTR_BITRATE]);
}

#[test]
fn tsel_none_emits_no_box() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m);
    m.set_track_selection(
        id,
        Some(TrackSelection {
            switch_group: 5,
            attributes: Vec::new(),
        }),
    )
    .expect("set");
    m.set_track_selection(id, None).expect("clear");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(!bytes.windows(4).any(|w| w == b"tsel"));
    let d = open(bytes);
    assert!(d.track_selection(0).is_none());
}

#[test]
fn tsel_unknown_track_errors() {
    let mut m = MovMuxer::new();
    assert!(m
        .set_track_selection(
            9,
            Some(TrackSelection {
                switch_group: 1,
                attributes: Vec::new()
            })
        )
        .is_err());
}
