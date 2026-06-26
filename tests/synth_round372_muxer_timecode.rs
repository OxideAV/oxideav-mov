//! Round 372 — `MovMuxer` write-side **time-code track** (QTFF
//! pp. 106–116). The demuxer already reads a `tmcd`-handler track's
//! `gmhd/tmcd/tcmi` media-information atom, its `tmcd` `stsd` timing
//! fields, and per-sample packed timecode payloads
//! (`MovDemuxer::timecode_sample`); round 372 lets the muxer *write* a
//! complete time-code track via the new `MuxTrackKind::Timecode`.
//!
//! Each test builds a movie through [`MovMuxer`], re-opens it through
//! [`MovDemuxer`], and asserts the track round-trips: the `tmcd`
//! handler, the `gmhd` header (`gmin` + `tcmi`), the `tmcd` sample
//! description, the per-sample timecode value, and (with a `tref/tmcd`)
//! resolution from a media track.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, Tcmi, TimecodeRecord, TimecodeSample, Tmcd,
    TrackReference, TMCD_FLAG_COUNTER, TMCD_FLAG_DROP_FRAME,
};

fn record_sample(rec: TimecodeRecord) -> Vec<MuxSample> {
    let bytes = Tmcd::encode_sample(&TimecodeSample::Record(rec));
    vec![MuxSample {
        data: bytes.to_vec(),
        duration: 100,
        keyframe: true,
        composition_offset: 0,
    }]
}

fn counter_sample(v: u32) -> Vec<MuxSample> {
    let bytes = Tmcd::encode_sample(&TimecodeSample::Counter(v));
    vec![MuxSample {
        data: bytes.to_vec(),
        duration: 100,
        keyframe: true,
        composition_offset: 0,
    }]
}

fn add_video(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 1920,
            height: 1080,
        },
        30000,
        vec![MuxSample {
            data: vec![0x55u8; 8],
            duration: 1001,
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

fn nondrop_30() -> Tmcd {
    Tmcd {
        flags: 0,
        time_scale: 30000,
        frame_duration: 1001,
        number_of_frames: 30,
        source_name: None,
    }
}

#[test]
fn timecode_track_handler_and_headers_roundtrip() {
    let mut m = MovMuxer::new();
    let tcmi = Tcmi {
        text_font: 3,
        text_face: 1,
        text_size: 12,
        bg_color: [0, 0, 0],
        fg_color: [0xFFFF, 0xFFFF, 0xFFFF],
        font_name: "Helvetica".into(),
    };
    let _ = m.add_track(
        MuxTrackKind::Timecode {
            description: nondrop_30(),
            tcmi: tcmi.clone(),
        },
        30000,
        record_sample(TimecodeRecord {
            negative: false,
            hours: 1,
            minutes: 2,
            seconds: 3,
            frames: 4,
        }),
        &[],
    );
    let bytes = m.encode_to_vec().expect("encode");

    assert!(bytes.windows(4).any(|w| w == b"gmhd"));
    assert!(bytes.windows(4).any(|w| w == b"gmin"));
    assert!(bytes.windows(4).any(|w| w == b"tcmi"));

    let d = open(bytes);
    let t = &d.tracks[0];
    assert!(t.is_timecode());
    // gmhd → gmin + tcmi.
    let g = t.gmhd.as_ref().expect("gmhd parsed");
    assert!(g.gmin.is_some());
    assert_eq!(g.tcmi.as_ref().expect("tcmi"), &tcmi);
}

#[test]
fn tmcd_sample_description_roundtrips() {
    let mut m = MovMuxer::new();
    let desc = Tmcd {
        flags: TMCD_FLAG_DROP_FRAME,
        time_scale: 30000,
        frame_duration: 1001,
        number_of_frames: 30,
        source_name: Some("Reel-01".into()),
    };
    let _ = m.add_track(
        MuxTrackKind::Timecode {
            description: desc.clone(),
            tcmi: Tcmi::default(),
        },
        30000,
        counter_sample(0),
        &[],
    );
    let d = open(m.encode_to_vec().expect("encode"));
    let parsed = d.tracks[0].sample_descriptions[0]
        .tmcd
        .clone()
        .expect("tmcd sample desc");
    assert_eq!(parsed.flags, TMCD_FLAG_DROP_FRAME);
    assert_eq!(parsed.time_scale, 30000);
    assert_eq!(parsed.frame_duration, 1001);
    assert_eq!(parsed.number_of_frames, 30);
    assert!(parsed.is_drop_frame());
    assert_eq!(parsed.source_name.as_deref(), Some("Reel-01"));
}

#[test]
fn record_sample_value_roundtrips() {
    let rec = TimecodeRecord {
        negative: false,
        hours: 10,
        minutes: 45,
        seconds: 30,
        frames: 12,
    };
    let mut m = MovMuxer::new();
    let _ = m.add_track(
        MuxTrackKind::Timecode {
            description: nondrop_30(),
            tcmi: Tcmi::default(),
        },
        30000,
        record_sample(rec),
        &[],
    );
    let mut d = open(m.encode_to_vec().expect("encode"));
    let s = d.timecode_sample(0, 0).expect("read").expect("some");
    assert_eq!(s, TimecodeSample::Record(rec));
}

#[test]
fn counter_sample_value_roundtrips() {
    let mut m = MovMuxer::new();
    let _ = m.add_track(
        MuxTrackKind::Timecode {
            description: Tmcd {
                flags: TMCD_FLAG_COUNTER,
                time_scale: 30000,
                frame_duration: 1001,
                number_of_frames: 1,
                source_name: None,
            },
            tcmi: Tcmi::default(),
        },
        30000,
        counter_sample(123_456),
        &[],
    );
    let mut d = open(m.encode_to_vec().expect("encode"));
    let s = d.timecode_sample(0, 0).expect("read").expect("some");
    assert_eq!(s, TimecodeSample::Counter(123_456));
}

#[test]
fn negative_record_sample_preserves_sign() {
    let rec = TimecodeRecord {
        negative: true,
        hours: 0,
        minutes: 5,
        seconds: 0,
        frames: 0,
    };
    let mut m = MovMuxer::new();
    let _ = m.add_track(
        MuxTrackKind::Timecode {
            description: nondrop_30(),
            tcmi: Tcmi::default(),
        },
        30000,
        record_sample(rec),
        &[],
    );
    let mut d = open(m.encode_to_vec().expect("encode"));
    let s = d.timecode_sample(0, 0).expect("read").expect("some");
    assert_eq!(s, TimecodeSample::Record(rec));
}

#[test]
fn media_track_resolves_start_timecode_via_tref() {
    let mut m = MovMuxer::new();
    let video = add_video(&mut m);
    let rec = TimecodeRecord {
        negative: false,
        hours: 1,
        minutes: 0,
        seconds: 0,
        frames: 0,
    };
    let tc = m.add_track(
        MuxTrackKind::Timecode {
            description: nondrop_30(),
            tcmi: Tcmi::default(),
        },
        30000,
        record_sample(rec),
        &[],
    );
    m.set_track_references(video, &[TrackReference::timecode(tc)])
        .expect("attach tref/tmcd");
    let mut d = open(m.encode_to_vec().expect("encode"));

    // The video track (index 0) resolves its governing timecode through
    // tref/tmcd to the timecode track (index 1).
    let start = d.start_timecode(0).expect("resolve").expect("some");
    assert_eq!(start.timecode_track_index, 1);
    assert_eq!(start.number_of_frames, 30);
    assert_eq!(start.sample, TimecodeSample::Record(rec));
}

#[test]
fn multiple_timecode_samples_roundtrip() {
    let mut samples = Vec::new();
    for f in 0..5u8 {
        let rec = TimecodeRecord {
            negative: false,
            hours: 0,
            minutes: 0,
            seconds: 0,
            frames: f,
        };
        let bytes = Tmcd::encode_sample(&TimecodeSample::Record(rec));
        samples.push(MuxSample {
            data: bytes.to_vec(),
            duration: 100,
            keyframe: true,
            composition_offset: 0,
        });
    }
    let mut m = MovMuxer::new();
    let _ = m.add_track(
        MuxTrackKind::Timecode {
            description: nondrop_30(),
            tcmi: Tcmi::default(),
        },
        30000,
        samples,
        &[],
    );
    let mut d = open(m.encode_to_vec().expect("encode"));
    for f in 0..5u32 {
        let s = d.timecode_sample(0, f).expect("read").expect("some");
        assert_eq!(
            s,
            TimecodeSample::Record(TimecodeRecord {
                negative: false,
                hours: 0,
                minutes: 0,
                seconds: 0,
                frames: f as u8,
            })
        );
    }
}
