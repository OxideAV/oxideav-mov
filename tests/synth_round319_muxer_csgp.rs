//! Round 319 — `MovMuxer` write-side `csgp` (CompactSampleToGroupBox)
//! emission at `stbl` scope (ISO/IEC 14496-12:2020 §8.9.5).
//!
//! The crate already *reads* `csgp` (the demuxer expands it into the
//! same run-length [`SampleToGroup`] a v0/v1 `sbgp` produces); round 319
//! lets the muxer *write* it. A track carrying one or more
//! [`SampleToGroupWrite`] assignments emits one `csgp` per
//! `grouping_type` inside its `stbl`, encoding the per-sample
//! description indices in the compact pattern form (one
//! `pattern_length == 1` pattern per run of consecutive equal indices).
//!
//! These tests build a file through [`MovMuxer`], confirm the `csgp`
//! box appears in the byte stream, demux it back through [`MovDemuxer`]
//! to prove the stbl is still structurally valid (sample data intact),
//! and re-parse the emitted `csgp` body through [`parse_csgp`] to verify
//! the per-sample index assignment round-trips exactly.
//!
//! Layout reference: `docs/container/isobmff/post-2015-additions.md`.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{parse_csgp, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, SampleToGroupWrite};

/// Build an N-sample audio MOV with one sample-to-group assignment on
/// the only track. Returns the encoded bytes.
fn build_audio_mov_with_group(grouping_type: [u8; 4], indices: Vec<u32>) -> Vec<u8> {
    let samples: Vec<MuxSample> = (0..indices.len())
        .map(|i| MuxSample {
            data: vec![(0xA0 | (i & 0x0F)) as u8; 8],
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    let mut m = MovMuxer::new().with_movie_timescale(600);
    let id = m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        samples,
        &[],
    );
    m.add_sample_to_group(
        id,
        SampleToGroupWrite {
            grouping_type,
            grouping_type_parameter: None,
            indices,
        },
    )
    .expect("attach sample-to-group");
    m.encode_to_vec().expect("encode audio MOV with csgp")
}

/// Slice the first `csgp` box body out of a muxed file (skipping the
/// 8-byte box header). Returns the payload the demuxer would parse.
fn first_csgp_body(bytes: &[u8]) -> &[u8] {
    let pos = bytes
        .windows(4)
        .position(|w| w == b"csgp")
        .expect("csgp present in stream");
    // The 4-byte size precedes the FourCC; body starts after the FourCC.
    let size = u32::from_be_bytes([
        bytes[pos - 4],
        bytes[pos - 3],
        bytes[pos - 2],
        bytes[pos - 1],
    ]) as usize;
    let body_start = pos + 4;
    let body_end = (pos - 4) + size;
    &bytes[body_start..body_end]
}

#[test]
fn csgp_box_present_and_indices_roundtrip() {
    let indices = vec![1u32, 1, 2, 2, 1];
    let bytes = build_audio_mov_with_group(*b"roll", indices.clone());

    // The box is in the stream.
    assert!(bytes.windows(4).any(|w| w == b"csgp"));

    // The emitted body re-parses to the exact per-sample assignment.
    let parsed = parse_csgp(first_csgp_body(&bytes)).expect("parse emitted csgp");
    assert_eq!(&parsed.grouping_type, b"roll");
    assert_eq!(parsed.covered_samples(), indices.len() as u64);
    for (i, &want) in indices.iter().enumerate() {
        assert_eq!(parsed.group_index_for_sample(i as u32), want);
    }
}

#[test]
fn csgp_does_not_corrupt_sample_data() {
    // A track with a csgp still demuxes its sample bytes correctly.
    let indices = vec![3u32, 3, 0, 1];
    let bytes = build_audio_mov_with_group(*b"rap ", indices);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open muxed file");

    // Sample i data was vec![0xA0 | i; 8].
    for i in 0..4u8 {
        let pkt = d.next_packet().expect("packet");
        assert_eq!(pkt.data, vec![0xA0 | i; 8], "sample {i} bytes");
    }
}

#[test]
fn multiple_grouping_types_emit_multiple_csgp() {
    let samples: Vec<MuxSample> = (0..3)
        .map(|i| MuxSample {
            data: vec![(0xB0 | i) as u8; 6],
            duration: 1000,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    let mut m = MovMuxer::new();
    let id = m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        samples,
        &[],
    );
    m.add_sample_to_group(
        id,
        SampleToGroupWrite {
            grouping_type: *b"roll",
            grouping_type_parameter: None,
            indices: vec![1, 1, 1],
        },
    )
    .expect("attach roll");
    m.add_sample_to_group(
        id,
        SampleToGroupWrite {
            grouping_type: *b"rap ",
            grouping_type_parameter: None,
            indices: vec![1, 0, 2],
        },
    )
    .expect("attach rap");
    let bytes = m.encode_to_vec().expect("encode");

    let count = bytes.windows(4).filter(|w| *w == b"csgp").count();
    assert_eq!(count, 2, "one csgp per grouping_type");

    // The file still opens.
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let _ = MovDemuxer::open(cur).expect("open muxed file with two csgp");
}

#[test]
fn add_sample_to_group_rejects_wrong_index_count() {
    let samples = vec![MuxSample {
        data: vec![0u8; 8],
        duration: 1024,
        keyframe: true,
        composition_offset: 0,
    }];
    let mut m = MovMuxer::new();
    let id = m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        samples,
        &[],
    );
    // Track has 1 sample but we supply 2 indices.
    let err = m.add_sample_to_group(
        id,
        SampleToGroupWrite {
            grouping_type: *b"roll",
            grouping_type_parameter: None,
            indices: vec![1, 1],
        },
    );
    assert!(err.is_err(), "index count must match sample count");
}

#[test]
fn replacing_grouping_type_keeps_one_box() {
    let bytes = {
        let samples: Vec<MuxSample> = (0..2)
            .map(|_| MuxSample {
                data: vec![0u8; 8],
                duration: 1024,
                keyframe: true,
                composition_offset: 0,
            })
            .collect();
        let mut m = MovMuxer::new();
        let id = m.add_track(
            MuxTrackKind::Audio {
                format: *b"sowt",
                channels: 1,
                bits_per_sample: 16,
                sample_rate: 8000,
            },
            8000,
            samples,
            &[],
        );
        m.add_sample_to_group(
            id,
            SampleToGroupWrite {
                grouping_type: *b"roll",
                grouping_type_parameter: None,
                indices: vec![1, 1],
            },
        )
        .unwrap();
        // Second call with same grouping_type replaces, not appends.
        m.add_sample_to_group(
            id,
            SampleToGroupWrite {
                grouping_type: *b"roll",
                grouping_type_parameter: None,
                indices: vec![2, 2],
            },
        )
        .unwrap();
        m.encode_to_vec().unwrap()
    };
    assert_eq!(
        bytes.windows(4).filter(|w| *w == b"csgp").count(),
        1,
        "replacement must not duplicate the box"
    );
    // The replacement value (index 2) is what's encoded.
    let parsed = parse_csgp(first_csgp_body(&bytes)).unwrap();
    assert_eq!(parsed.group_index_for_sample(0), 2);
    assert_eq!(parsed.group_index_for_sample(1), 2);
}
