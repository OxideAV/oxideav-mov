//! Round 364 — `MovMuxer` write-side `sgpd` (SampleGroupDescriptionBox)
//! emission at `stbl` scope (ISO/IEC 14496-12 §8.9.3).
//!
//! The crate already *reads* `sgpd` (`parse_sgpd` recovers the per-entry
//! payloads and the typed decoders — `decode_roll`, `decode_rap`,
//! `decode_tele`, `decode_sap`, `decode_prol` — recover the structured
//! fields). Round 364 lets the muxer *write* it, closing the
//! demux↔mux symmetry gap where the write side emitted a `csgp`
//! (per-sample group-description **indices**) but no sibling `sgpd`
//! (the **descriptions** those indices reference): a non-zero index
//! with no matching `sgpd` entry points at nothing.
//!
//! [`MovMuxer::set_sample_group_description`] attaches a typed
//! [`SampleGroupDescriptionWrite`] to a track; one `sgpd` per
//! `grouping_type` is written inside the `stbl` immediately before the
//! `csgp` boxes (§8.9.3 containment order). The box is version 1: a
//! constant `default_length` when every entry is the same size,
//! otherwise `default_length == 0` with a per-entry `description_length`
//! prefix.
//!
//! These tests build a file through [`MovMuxer`], demux it back through
//! [`MovDemuxer`] to prove the `stbl` stays valid, and re-parse the
//! emitted `sgpd` body through [`parse_sgpd`] + the typed decoders to
//! verify each entry round-trips exactly.
//!
//! Layout reference: `docs/container/isobmff/bmff.txt` §8.9.3.2.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{
    decode_rap, decode_roll, decode_sap, decode_tele, parse_sgpd, MovDemuxer, MovMuxer, MuxSample,
    MuxTrackKind, SampleGroupDescriptionWrite, SampleToGroupWrite,
};

/// Build an N-sample audio MOV with a sample-to-group assignment plus
/// its sibling group description on the only track. Returns the bytes.
fn build_mov_with_sgpd(
    sample_count: usize,
    desc: SampleGroupDescriptionWrite,
    indices: Vec<u32>,
) -> Vec<u8> {
    let grouping_type = desc.grouping_type;
    let samples: Vec<MuxSample> = (0..sample_count)
        .map(|i| MuxSample {
            data: vec![(0xC0 | (i & 0x0F)) as u8; 8],
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
    m.set_sample_group_description(id, desc)
        .expect("attach sgpd");
    m.add_sample_to_group(
        id,
        SampleToGroupWrite {
            grouping_type,
            grouping_type_parameter: None,
            indices,
        },
    )
    .expect("attach csgp");
    m.encode_to_vec().expect("encode MOV with sgpd")
}

/// Slice the first `sgpd` box body out of a muxed file (skipping the
/// 8-byte box header). Returns the payload `parse_sgpd` would parse.
fn first_sgpd_body(bytes: &[u8]) -> &[u8] {
    let pos = bytes
        .windows(4)
        .position(|w| w == b"sgpd")
        .expect("sgpd present in stream");
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
fn sgpd_uniform_roll_entries_roundtrip() {
    // Two 'roll' descriptions (both 2 bytes ⇒ uniform default_length).
    let desc = SampleGroupDescriptionWrite::new(
        *b"roll",
        vec![
            SampleGroupDescriptionWrite::roll_entry(-1),
            SampleGroupDescriptionWrite::roll_entry(3),
        ],
    );
    let bytes = build_mov_with_sgpd(3, desc, vec![1, 2, 1]);

    assert!(bytes.windows(4).any(|w| w == b"sgpd"));

    let parsed = parse_sgpd(first_sgpd_body(&bytes)).expect("parse emitted sgpd");
    assert_eq!(&parsed.grouping_type, b"roll");
    assert_eq!(parsed.version, 1);
    assert_eq!(parsed.default_length, 2, "uniform entries ⇒ default_length");
    assert_eq!(parsed.entries.len(), 2);

    let e1 = parsed.entry(1).expect("index 1");
    assert_eq!(decode_roll(&e1.payload).unwrap().roll_distance, -1);
    let e2 = parsed.entry(2).expect("index 2");
    assert_eq!(decode_roll(&e2.payload).unwrap().roll_distance, 3);
    // Reserved-index 0 is "no group".
    assert!(parsed.entry(0).is_none());
}

#[test]
fn sgpd_variable_length_entries_use_description_length() {
    // Mix a 2-byte 'roll' shape with a 5-byte raw blob ⇒ variable
    // lengths ⇒ default_length == 0 with per-entry description_length.
    let desc = SampleGroupDescriptionWrite::new(
        *b"roll",
        vec![
            SampleGroupDescriptionWrite::roll_entry(7),
            vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01],
        ],
    );
    let bytes = build_mov_with_sgpd(2, desc, vec![1, 2]);

    let parsed = parse_sgpd(first_sgpd_body(&bytes)).expect("parse emitted sgpd");
    assert_eq!(
        parsed.default_length, 0,
        "variable entries ⇒ no default_length"
    );
    assert_eq!(parsed.entries.len(), 2);
    assert_eq!(parsed.entries[0].payload, vec![0x00, 0x07]);
    assert_eq!(
        parsed.entries[1].payload,
        vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01]
    );
}

#[test]
fn sgpd_rap_tele_sap_entries_decode() {
    // 'rap ' typed entry round-trips through decode_rap.
    let rap = SampleGroupDescriptionWrite::new(
        *b"rap ",
        vec![SampleGroupDescriptionWrite::rap_entry(true, 5)],
    );
    let bytes = build_mov_with_sgpd(1, rap, vec![1]);
    let parsed = parse_sgpd(first_sgpd_body(&bytes)).unwrap();
    let v = decode_rap(&parsed.entry(1).unwrap().payload).unwrap();
    assert!(v.num_leading_samples_known);
    assert_eq!(v.num_leading_samples, 5);

    // 'tele' typed entry round-trips through decode_tele.
    let tele = SampleGroupDescriptionWrite::new(
        *b"tele",
        vec![SampleGroupDescriptionWrite::tele_entry(true)],
    );
    let bytes = build_mov_with_sgpd(1, tele, vec![1]);
    let parsed = parse_sgpd(first_sgpd_body(&bytes)).unwrap();
    assert!(
        decode_tele(&parsed.entry(1).unwrap().payload)
            .unwrap()
            .level_independently_decodable
    );

    // 'sap ' typed entry round-trips through decode_sap.
    let sap = SampleGroupDescriptionWrite::new(
        *b"sap ",
        vec![SampleGroupDescriptionWrite::sap_entry(false, 3)],
    );
    let bytes = build_mov_with_sgpd(1, sap, vec![1]);
    let parsed = parse_sgpd(first_sgpd_body(&bytes)).unwrap();
    let v = decode_sap(&parsed.entry(1).unwrap().payload).unwrap();
    assert!(!v.dependent);
    assert_eq!(v.sap_type, 3);
}

#[test]
fn sgpd_and_csgp_pair_keeps_sample_data_intact() {
    let desc = SampleGroupDescriptionWrite::new(
        *b"roll",
        vec![SampleGroupDescriptionWrite::roll_entry(-2)],
    );
    let bytes = build_mov_with_sgpd(4, desc, vec![1, 1, 0, 1]);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open muxed file with sgpd+csgp");
    for i in 0..4u8 {
        let pkt = d.next_packet().expect("packet");
        assert_eq!(pkt.data, vec![0xC0 | i; 8], "sample {i} bytes");
    }
}

#[test]
fn sgpd_precedes_csgp_in_stbl() {
    let desc = SampleGroupDescriptionWrite::new(
        *b"roll",
        vec![SampleGroupDescriptionWrite::roll_entry(1)],
    );
    let bytes = build_mov_with_sgpd(2, desc, vec![1, 1]);
    let sgpd_pos = bytes.windows(4).position(|w| w == b"sgpd").unwrap();
    let csgp_pos = bytes.windows(4).position(|w| w == b"csgp").unwrap();
    assert!(
        sgpd_pos < csgp_pos,
        "sgpd must come before csgp (§8.9.3 order)"
    );
}

#[test]
fn multiple_grouping_types_emit_multiple_sgpd() {
    let samples: Vec<MuxSample> = (0..3)
        .map(|i| MuxSample {
            data: vec![(0xD0 | i) as u8; 6],
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
    m.set_sample_group_description(
        id,
        SampleGroupDescriptionWrite::new(
            *b"roll",
            vec![SampleGroupDescriptionWrite::roll_entry(1)],
        ),
    )
    .unwrap();
    m.set_sample_group_description(
        id,
        SampleGroupDescriptionWrite::new(
            *b"rap ",
            vec![SampleGroupDescriptionWrite::rap_entry(false, 0)],
        ),
    )
    .unwrap();
    let bytes = m.encode_to_vec().unwrap();
    assert_eq!(
        bytes.windows(4).filter(|w| *w == b"sgpd").count(),
        2,
        "one sgpd per grouping_type"
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let _ = MovDemuxer::open(cur).expect("open muxed file with two sgpd");
}

#[test]
fn replacing_grouping_type_keeps_one_sgpd() {
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
    m.set_sample_group_description(
        id,
        SampleGroupDescriptionWrite::new(
            *b"roll",
            vec![SampleGroupDescriptionWrite::roll_entry(1)],
        ),
    )
    .unwrap();
    // Second call with same grouping_type replaces, not appends.
    m.set_sample_group_description(
        id,
        SampleGroupDescriptionWrite::new(
            *b"roll",
            vec![SampleGroupDescriptionWrite::roll_entry(9)],
        ),
    )
    .unwrap();
    let bytes = m.encode_to_vec().unwrap();
    assert_eq!(
        bytes.windows(4).filter(|w| *w == b"sgpd").count(),
        1,
        "replacement must not duplicate the box"
    );
    let parsed = parse_sgpd(first_sgpd_body(&bytes)).unwrap();
    assert_eq!(
        decode_roll(&parsed.entry(1).unwrap().payload)
            .unwrap()
            .roll_distance,
        9
    );
}

#[test]
fn empty_sgpd_entries_rejected() {
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
    let err =
        m.set_sample_group_description(id, SampleGroupDescriptionWrite::new(*b"roll", vec![]));
    assert!(err.is_err(), "empty entry list must be rejected");
}

#[test]
fn set_sgpd_rejects_unknown_track() {
    let mut m = MovMuxer::new();
    let err = m.set_sample_group_description(
        99,
        SampleGroupDescriptionWrite::new(
            *b"roll",
            vec![SampleGroupDescriptionWrite::roll_entry(0)],
        ),
    );
    assert!(err.is_err(), "unknown track id must be rejected");
}
