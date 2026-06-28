//! Round 379 — `MovMuxer` write-side **per-sample auxiliary
//! sample-table boxes**: the Independent and Disposable Samples Box
//! (`sdtp`, ISO/IEC 14496-12 §8.6.4), the Degradation Priority Box
//! (`stdp`, §8.5.3), the Padding Bits Box (`padb`, §8.7.6), the Shadow
//! Sync Sample Box (`stsh`, §8.6.3), and the Sub-Sample Information Box
//! (`subs`, §8.7.7).
//!
//! The demuxer has long read all five (`parse_sdtp` / `parse_stdp` /
//! `parse_padb` / `parse_stsh` / `parse_subs`, surfaced on
//! `Track::sample_table` + the typed `MovDemuxer` accessors), but the
//! muxer never wrote them. Each test builds a movie with one of the new
//! `set_*` setters, re-opens it through `MovDemuxer`, and asserts the
//! per-sample values round-trip exactly.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    IsLeading, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, SampleDependsOn, SampleHasRedundancy,
    SampleIsDependedOn, SdtpEntry, StshEntry, SubSampleEntry, SubSampleInfo,
};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn video_track(m: &mut MovMuxer, n: usize) -> u32 {
    let samples: Vec<MuxSample> = (0..n)
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

// ---------------------------------------------------------------- sdtp

#[test]
fn sdtp_round_trips_per_sample_dependencies() {
    let entries = [
        SdtpEntry {
            is_leading: IsLeading::NotLeading,
            sample_depends_on: SampleDependsOn::Independent,
            sample_is_depended_on: SampleIsDependedOn::NotDisposable,
            sample_has_redundancy: SampleHasRedundancy::NotRedundant,
        },
        SdtpEntry {
            is_leading: IsLeading::LeadingDecodable,
            sample_depends_on: SampleDependsOn::DependsOnOthers,
            sample_is_depended_on: SampleIsDependedOn::Disposable,
            sample_has_redundancy: SampleHasRedundancy::Redundant,
        },
        SdtpEntry {
            is_leading: IsLeading::Unknown,
            sample_depends_on: SampleDependsOn::Unknown,
            sample_is_depended_on: SampleIsDependedOn::Unknown,
            sample_has_redundancy: SampleHasRedundancy::Unknown,
        },
    ];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, entries.len());
    m.set_sample_dependencies(id, &entries).expect("set sdtp");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"sdtp"));

    let d = open(bytes);
    let st = &d.tracks[0].sample_table;
    assert_eq!(st.sdtp.len(), entries.len());
    for (i, e) in entries.iter().enumerate() {
        assert_eq!(st.sample_dependency(i as u32), Some(*e));
        // Also through the demuxer's typed accessor.
        assert_eq!(d.sample_dependency(0, i as u32), Some(*e));
    }
}

#[test]
fn sdtp_byte_round_trips_all_field_codes() {
    // Every packed byte 0..=255 must survive to_byte∘from_byte.
    for b in 0u8..=255 {
        assert_eq!(SdtpEntry::from_byte(b).to_byte(), b);
    }
}

#[test]
fn sdtp_wrong_count_rejected() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 3);
    let one = [SdtpEntry::from_byte(0)];
    assert!(m.set_sample_dependencies(id, &one).is_err());
}

#[test]
fn sdtp_empty_removes_table() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 2);
    m.set_sample_dependencies(
        id,
        &[SdtpEntry::from_byte(0x88), SdtpEntry::from_byte(0x44)],
    )
    .expect("set");
    m.set_sample_dependencies(id, &[]).expect("clear");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(!bytes.windows(4).any(|w| w == b"sdtp"));
}

// ---------------------------------------------------------------- stdp

#[test]
fn stdp_round_trips_per_sample_priorities() {
    let prios = [0u16, 1, 100, 65535, 7];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, prios.len());
    m.set_degradation_priorities(id, &prios).expect("set stdp");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"stdp"));

    let d = open(bytes);
    let st = &d.tracks[0].sample_table;
    assert_eq!(st.stdp, prios);
    for (i, &p) in prios.iter().enumerate() {
        assert_eq!(d.sample_degradation_priority(0, i as u32), Some(p));
    }
}

#[test]
fn stdp_wrong_count_rejected() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 4);
    assert!(m.set_degradation_priorities(id, &[1, 2]).is_err());
}

// ---------------------------------------------------------------- padb

#[test]
fn padb_round_trips_even_count() {
    let pads = [0u8, 7, 3, 4];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, pads.len());
    m.set_padding_bits(id, &pads).expect("set padb");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"padb"));

    let d = open(bytes);
    let st = &d.tracks[0].sample_table;
    assert_eq!(st.padb, pads);
    for (i, &p) in pads.iter().enumerate() {
        assert_eq!(d.sample_padding_bits(0, i as u32), Some(p));
    }
}

#[test]
fn padb_round_trips_odd_count_zero_pads_last_nibble() {
    let pads = [1u8, 2, 5];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, pads.len());
    m.set_padding_bits(id, &pads).expect("set padb");
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    assert_eq!(d.tracks[0].sample_table.padb, pads);
}

#[test]
fn padb_value_above_seven_rejected() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 2);
    assert!(m.set_padding_bits(id, &[3, 8]).is_err());
}

// ---------------------------------------------------------------- stsh

#[test]
fn stsh_round_trips_and_sorts() {
    // Supply out-of-order; the muxer must sort by shadowed_sample_number.
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 6);
    let entries = [
        StshEntry {
            shadowed_sample_number: 5,
            sync_sample_number: 1,
        },
        StshEntry {
            shadowed_sample_number: 3,
            sync_sample_number: 1,
        },
        StshEntry {
            shadowed_sample_number: 4,
            sync_sample_number: 3,
        },
    ];
    m.set_shadow_sync_samples(id, &entries).expect("set stsh");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"stsh"));

    let d = open(bytes);
    let st = &d.tracks[0].sample_table;
    // Sorted ascending by shadowed number.
    assert_eq!(
        st.stsh,
        [
            StshEntry {
                shadowed_sample_number: 3,
                sync_sample_number: 1
            },
            StshEntry {
                shadowed_sample_number: 4,
                sync_sample_number: 3
            },
            StshEntry {
                shadowed_sample_number: 5,
                sync_sample_number: 1
            },
        ]
    );
    assert_eq!(d.shadow_sync_sample(0, 4), Some(3));
    assert_eq!(d.shadow_sync_sample(0, 99), None);
}

#[test]
fn stsh_duplicate_shadowed_rejected() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 4);
    let entries = [
        StshEntry {
            shadowed_sample_number: 2,
            sync_sample_number: 1,
        },
        StshEntry {
            shadowed_sample_number: 2,
            sync_sample_number: 1,
        },
    ];
    assert!(m.set_shadow_sync_samples(id, &entries).is_err());
}

#[test]
fn stsh_out_of_range_rejected() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 3);
    let entries = [StshEntry {
        shadowed_sample_number: 9,
        sync_sample_number: 1,
    }];
    assert!(m.set_shadow_sync_samples(id, &entries).is_err());
}

// ---------------------------------------------------------------- subs

#[test]
fn subs_round_trips_v0_sparse_table() {
    let rows = vec![
        SubSampleInfo {
            sample_number: 1,
            subsamples: vec![
                SubSampleEntry {
                    subsample_size: 4,
                    subsample_priority: 7,
                    discardable: 0,
                    codec_specific_parameters: 0xDEAD_BEEF,
                },
                SubSampleEntry {
                    subsample_size: 12,
                    subsample_priority: 0,
                    discardable: 1,
                    codec_specific_parameters: 0,
                },
            ],
        },
        SubSampleInfo {
            sample_number: 3,
            subsamples: vec![SubSampleEntry {
                subsample_size: 8,
                subsample_priority: 1,
                discardable: 0,
                codec_specific_parameters: 42,
            }],
        },
    ];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 4);
    m.set_sub_samples(id, &rows).expect("set subs");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"subs"));

    let d = open(bytes);
    let st = &d.tracks[0].sample_table;
    assert_eq!(st.subs, rows);
    // Typed accessor by sample number.
    let s1 = d.sub_samples(0, 1).expect("sample 1 subs");
    assert_eq!(s1.len(), 2);
    assert_eq!(s1[0].subsample_size, 4);
    assert_eq!(s1[0].codec_specific_parameters, 0xDEAD_BEEF);
    assert!(d.sub_samples(0, 2).is_none());
}

#[test]
fn subs_promotes_to_v1_for_large_subsamples() {
    // A sub-sample > 65535 forces version 1 (32-bit size on disk).
    let rows = vec![SubSampleInfo {
        sample_number: 1,
        subsamples: vec![SubSampleEntry {
            subsample_size: 100_000,
            subsample_priority: 0,
            discardable: 0,
            codec_specific_parameters: 0,
        }],
    }];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 2);
    m.set_sub_samples(id, &rows).expect("set subs");
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    assert_eq!(d.tracks[0].sample_table.subs, rows);
}

#[test]
fn subs_sorts_and_delta_codes() {
    // Out-of-order rows must be sorted then delta-coded so the read side
    // recovers the same absolute sample numbers.
    let rows = vec![
        SubSampleInfo {
            sample_number: 4,
            subsamples: vec![],
        },
        SubSampleInfo {
            sample_number: 2,
            subsamples: vec![SubSampleEntry {
                subsample_size: 3,
                subsample_priority: 0,
                discardable: 0,
                codec_specific_parameters: 0,
            }],
        },
    ];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 5);
    m.set_sub_samples(id, &rows).expect("set subs");
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    let got = &d.tracks[0].sample_table.subs;
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].sample_number, 2);
    assert_eq!(got[1].sample_number, 4);
}

#[test]
fn subs_duplicate_sample_number_rejected() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 4);
    let rows = vec![
        SubSampleInfo {
            sample_number: 2,
            subsamples: vec![],
        },
        SubSampleInfo {
            sample_number: 2,
            subsamples: vec![],
        },
    ];
    assert!(m.set_sub_samples(id, &rows).is_err());
}

#[test]
fn all_five_boxes_coexist_in_one_stbl() {
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, 4);
    m.set_sample_dependencies(
        id,
        &[
            SdtpEntry::from_byte(0x88),
            SdtpEntry::from_byte(0x44),
            SdtpEntry::from_byte(0x44),
            SdtpEntry::from_byte(0x88),
        ],
    )
    .expect("sdtp");
    m.set_degradation_priorities(id, &[10, 20, 30, 40])
        .expect("stdp");
    m.set_padding_bits(id, &[0, 1, 2, 3]).expect("padb");
    m.set_shadow_sync_samples(
        id,
        &[StshEntry {
            shadowed_sample_number: 2,
            sync_sample_number: 1,
        }],
    )
    .expect("stsh");
    m.set_sub_samples(
        id,
        &[SubSampleInfo {
            sample_number: 1,
            subsamples: vec![SubSampleEntry {
                subsample_size: 8,
                subsample_priority: 0,
                discardable: 0,
                codec_specific_parameters: 0,
            }],
        }],
    )
    .expect("subs");

    let bytes = m.encode_to_vec().expect("encode");
    for fourcc in [b"sdtp", b"stdp", b"padb", b"stsh", b"subs"] {
        assert!(
            bytes.windows(4).any(|w| w == fourcc),
            "missing {:?}",
            std::str::from_utf8(fourcc).unwrap()
        );
    }

    let d = open(bytes);
    let st = &d.tracks[0].sample_table;
    assert_eq!(st.sdtp.len(), 4);
    assert_eq!(st.stdp, [10, 20, 30, 40]);
    assert_eq!(st.padb, [0, 1, 2, 3]);
    assert_eq!(st.stsh.len(), 1);
    assert_eq!(st.subs.len(), 1);
}
