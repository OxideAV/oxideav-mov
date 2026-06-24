//! Round 364 — `MovMuxer` write-side classic `sbgp` (SampleToGroupBox)
//! form at `stbl` scope (ISO/IEC 14496-12 §8.9.2).
//!
//! The muxer previously emitted only the compact `csgp`
//! (CompactSampleToGroupBox, a 2020 addition some older readers don't
//! parse). [`MovMuxer::add_sample_to_group_with_form`] with
//! [`SampleGroupBoxForm::Classic`] now emits the widely-compatible
//! run-length `sbgp` carrying the identical per-sample
//! `group_description_index` mapping. The default
//! [`MovMuxer::add_sample_to_group`] still emits `csgp`.
//!
//! These tests build a file through [`MovMuxer`] in the classic form,
//! confirm an `sbgp` (not `csgp`) appears in the stream, re-parse the
//! emitted body through [`oxideav_mov::parse_sbgp`] to verify the
//! run-length rows and `grouping_type_parameter` round-trip, and demux
//! the file back to prove the `stbl` stays structurally valid.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{
    parse_sbgp, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, SampleGroupBoxForm,
    SampleToGroupWrite,
};

fn build_audio_mov_classic(
    grouping_type: [u8; 4],
    grouping_type_parameter: Option<u32>,
    indices: Vec<u32>,
) -> Vec<u8> {
    let samples: Vec<MuxSample> = (0..indices.len())
        .map(|i| MuxSample {
            data: vec![(0xF0 | (i & 0x0F)) as u8; 8],
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
    m.add_sample_to_group_with_form(
        id,
        SampleToGroupWrite {
            grouping_type,
            grouping_type_parameter,
            indices,
        },
        SampleGroupBoxForm::Classic,
    )
    .expect("attach classic sbgp");
    m.encode_to_vec().expect("encode")
}

fn first_box_body<'a>(bytes: &'a [u8], fourcc: &[u8; 4]) -> &'a [u8] {
    let pos = bytes
        .windows(4)
        .position(|w| w == fourcc)
        .expect("box present in stream");
    let size = u32::from_be_bytes([
        bytes[pos - 4],
        bytes[pos - 3],
        bytes[pos - 2],
        bytes[pos - 1],
    ]) as usize;
    &bytes[pos + 4..(pos - 4) + size]
}

#[test]
fn classic_form_emits_sbgp_not_csgp() {
    let bytes = build_audio_mov_classic(*b"roll", None, vec![1, 1, 2, 2, 1]);
    assert!(bytes.windows(4).any(|w| w == b"sbgp"), "sbgp present");
    assert!(
        !bytes.windows(4).any(|w| w == b"csgp"),
        "no csgp in classic form"
    );
}

#[test]
fn sbgp_runs_roundtrip() {
    let indices = vec![1u32, 1, 2, 2, 1];
    let bytes = build_audio_mov_classic(*b"roll", None, indices.clone());
    let parsed = parse_sbgp(first_box_body(&bytes, b"sbgp")).expect("parse sbgp");
    assert_eq!(&parsed.grouping_type, b"roll");
    assert_eq!(parsed.grouping_type_parameter, 0);
    // Run-length: [2×1][2×2][1×1] ⇒ 3 rows.
    assert_eq!(parsed.entries.len(), 3);
    assert_eq!(parsed.entries[0].sample_count, 2);
    assert_eq!(parsed.entries[0].group_description_index, 1);
    assert_eq!(parsed.entries[1].sample_count, 2);
    assert_eq!(parsed.entries[1].group_description_index, 2);
    assert_eq!(parsed.entries[2].sample_count, 1);
    assert_eq!(parsed.entries[2].group_description_index, 1);
    // Per-sample lookup recovers the original assignment.
    assert_eq!(parsed.covered_samples(), indices.len() as u64);
    for (i, &want) in indices.iter().enumerate() {
        assert_eq!(parsed.group_index_for_sample(i as u32), want);
    }
}

#[test]
fn sbgp_v1_grouping_type_parameter_roundtrips() {
    let bytes = build_audio_mov_classic(*b"rap ", Some(0xABCD_1234), vec![1, 0, 2]);
    let body = first_box_body(&bytes, b"sbgp");
    assert_eq!(body[0], 1, "version 1 when grouping_type_parameter present");
    let parsed = parse_sbgp(body).unwrap();
    assert_eq!(parsed.grouping_type_parameter, 0xABCD_1234);
    assert_eq!(parsed.group_index_for_sample(0), 1);
    assert_eq!(parsed.group_index_for_sample(1), 0);
    assert_eq!(parsed.group_index_for_sample(2), 2);
}

#[test]
fn classic_sbgp_keeps_sample_data_intact() {
    let bytes = build_audio_mov_classic(*b"roll", None, vec![1, 1, 0, 1]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open classic-form file");
    for i in 0..4u8 {
        let pkt = d.next_packet().expect("packet");
        assert_eq!(pkt.data, vec![0xF0 | i; 8], "sample {i} bytes");
    }
}

#[test]
fn replacing_with_different_form_keeps_one_box() {
    // First attach compact, then replace same grouping_type with classic.
    let samples: Vec<MuxSample> = (0..3)
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
            indices: vec![1, 1, 1],
        },
    )
    .unwrap();
    m.add_sample_to_group_with_form(
        id,
        SampleToGroupWrite {
            grouping_type: *b"roll",
            grouping_type_parameter: None,
            indices: vec![2, 2, 2],
        },
        SampleGroupBoxForm::Classic,
    )
    .unwrap();
    let bytes = m.encode_to_vec().unwrap();
    // The replacement swapped compact→classic: now exactly one sbgp,
    // zero csgp.
    assert_eq!(bytes.windows(4).filter(|w| *w == b"sbgp").count(), 1);
    assert_eq!(bytes.windows(4).filter(|w| *w == b"csgp").count(), 0);
    let parsed = parse_sbgp(first_box_body(&bytes, b"sbgp")).unwrap();
    assert_eq!(parsed.group_index_for_sample(0), 2);
}
