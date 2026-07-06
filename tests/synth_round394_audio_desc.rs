//! Round 394 — MovMuxer write-side **sound sample-description
//! versions**: the QTFF `SoundDescriptionV1` (QTFF p. 101, including
//! the p. 102 VBR "third variant") via
//! `MovMuxer::set_sound_description_v1`, and the ISO BMFF
//! `AudioSampleEntryV1` with `srat` SamplingRateBox + `chnl`
//! ChannelLayout (ISO/IEC 14496-12:2015 §12.2.3 / §12.2.4) via
//! `MovMuxer::set_audio_entry_v1`. Every shape round-trips through
//! the round-394 read side (`iso_audio_entry_v1` / `sampling_rate` /
//! `chnl` / `sound_v1` / `is_vbr`).

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    AudioEntryV1, ChannelLayout, ChannelStructure, FragmentationMode, MovDemuxer, MovMuxer,
    MuxSample, MuxTrackKind, SoundV1, SpeakerPosition,
};

fn samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![i as u8; 8],
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn add_audio(m: &mut MovMuxer, channels: u16) -> u32 {
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"mp4a",
            channels,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        samples(4),
        &[],
    )
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

#[test]
fn default_stays_version0() {
    let mut m = MovMuxer::new();
    add_audio(&mut m, 2);
    let d = open(m.encode_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.audio_version, 0);
    assert!(!sd.iso_audio_entry_v1);
    assert_eq!(sd.sound_v1, None);
    assert_eq!(sd.sample_rate, 48000);
}

#[test]
fn qtff_v1_fixed_ratio_roundtrip() {
    let fields = SoundV1 {
        samples_per_packet: 1024,
        bytes_per_packet: 384,
        bytes_per_frame: 768,
        bytes_per_sample: 2,
    };
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, 2);
    m.set_sound_description_v1(tid, fields, false).unwrap();
    let d = open(m.encode_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.audio_version, 1);
    assert!(!sd.iso_audio_entry_v1);
    assert_eq!(sd.sound_v1, Some(fields));
    assert_eq!(sd.audio_compression_id, 0);
    assert!(!sd.is_vbr());
    assert_eq!(sd.channels, 2);
    assert_eq!(sd.sample_rate, 48000);
}

#[test]
fn qtff_v1_vbr_third_variant_roundtrip() {
    // QTFF p. 102: VBR flags Compression ID -2; only
    // samples_per_packet / bytes_per_sample are meaningful.
    let fields = SoundV1 {
        samples_per_packet: 1152,
        bytes_per_packet: 0,
        bytes_per_frame: 0,
        bytes_per_sample: 2,
    };
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, 2);
    m.set_sound_description_v1(tid, fields, true).unwrap();
    let d = open(m.encode_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.audio_compression_id, -2);
    assert!(sd.is_vbr());
    assert_eq!(sd.sound_v1, Some(fields));
}

#[test]
fn qtff_v1_keeps_extra_stsd_atoms_after_36_byte_body() {
    // A codec-config atom appended after the 36-byte v1 body must
    // land in `extra` unharmed (the read side starts its scan at 36).
    let extra = {
        let mut a = Vec::new();
        a.extend_from_slice(&12u32.to_be_bytes());
        a.extend_from_slice(b"xcfg");
        a.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        a
    };
    let mut m = MovMuxer::new();
    let tid = m.add_track(
        MuxTrackKind::Audio {
            format: *b"mp4a",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        samples(2),
        &extra,
    );
    m.set_sound_description_v1(tid, SoundV1::default(), false)
        .unwrap();
    let d = open(m.encode_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.audio_version, 1);
    assert_eq!(sd.extra, extra);
}

#[test]
fn iso_v1_srat_and_defined_chnl_roundtrip() {
    let chnl = ChannelLayout {
        stream_structure: ChannelLayout::CHANNEL_STRUCTURED,
        channels: Some(ChannelStructure::Defined {
            defined_layout: 6,
            omitted_channels_map: 0x21,
        }),
        object_count: None,
    };
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, 6);
    m.set_audio_entry_v1(
        tid,
        AudioEntryV1 {
            sampling_rate: Some(96_000),
            channel_layout: Some(chnl.clone()),
        },
    )
    .unwrap();
    let bytes = m.encode_to_vec().unwrap();
    assert!(bytes.windows(4).any(|w| w == b"srat"));
    assert!(bytes.windows(4).any(|w| w == b"chnl"));
    let d = open(bytes);
    let sd = &d.tracks[0].sample_descriptions[0];
    assert!(sd.iso_audio_entry_v1);
    assert_eq!(sd.audio_version, 1);
    assert_eq!(sd.sound_v1, None, "no QTFF fixed-ratio fields");
    assert_eq!(sd.sample_rate, 48000, "16.16 field keeps the timescale");
    assert_eq!(sd.sampling_rate, Some(96_000));
    assert_eq!(sd.effective_sample_rate(), 96_000);
    assert_eq!(sd.chnl.as_ref(), Some(&chnl));
    assert_eq!(sd.channels, 6);
}

#[test]
fn iso_v1_explicit_positions_roundtrip() {
    let chnl = ChannelLayout {
        stream_structure: ChannelLayout::CHANNEL_STRUCTURED | ChannelLayout::OBJECT_STRUCTURED,
        channels: Some(ChannelStructure::Explicit(vec![
            SpeakerPosition {
                speaker_position: 2,
                azimuth: None,
                elevation: None,
            },
            SpeakerPosition {
                speaker_position: 126,
                azimuth: Some(-30),
                elevation: Some(10),
            },
        ])),
        object_count: Some(3),
    };
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, 2);
    m.set_audio_entry_v1(
        tid,
        AudioEntryV1 {
            sampling_rate: None,
            channel_layout: Some(chnl.clone()),
        },
    )
    .unwrap();
    let d = open(m.encode_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    assert!(sd.iso_audio_entry_v1);
    assert_eq!(sd.sampling_rate, None);
    assert_eq!(sd.effective_sample_rate(), 48000);
    assert_eq!(sd.chnl.as_ref(), Some(&chnl));
}

#[test]
fn iso_v1_roundtrips_through_fragmented_path() {
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    let tid = add_audio(&mut m, 2);
    m.set_audio_entry_v1(
        tid,
        AudioEntryV1 {
            sampling_rate: Some(192_000),
            channel_layout: None,
        },
    )
    .unwrap();
    let bytes = m.encode_fragmented_to_vec().unwrap();
    let d = open(bytes);
    let sd = &d.tracks[0].sample_descriptions[0];
    assert!(sd.iso_audio_entry_v1);
    assert_eq!(sd.sampling_rate, Some(192_000));
}

#[test]
fn validation_rejects_bad_configs() {
    let mut m = MovMuxer::new();
    let vid = m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 64,
            height: 64,
        },
        30000,
        samples(1),
        &[],
    );
    let aud = add_audio(&mut m, 2);

    // Non-audio track.
    assert!(m
        .set_sound_description_v1(vid, SoundV1::default(), false)
        .is_err());
    assert!(m.set_audio_entry_v1(vid, AudioEntryV1::default()).is_err());
    // Unknown id.
    assert!(m.set_audio_entry_v1(99, AudioEntryV1::default()).is_err());
    // Explicit layout row count must match the channel count.
    assert!(m
        .set_audio_entry_v1(
            aud,
            AudioEntryV1 {
                sampling_rate: None,
                channel_layout: Some(ChannelLayout {
                    stream_structure: ChannelLayout::CHANNEL_STRUCTURED,
                    channels: Some(ChannelStructure::Explicit(vec![SpeakerPosition {
                        speaker_position: 2,
                        azimuth: None,
                        elevation: None,
                    }])),
                    object_count: None,
                }),
            },
        )
        .is_err());
    // definedLayout == 0 on-wire selects the explicit form.
    assert!(m
        .set_audio_entry_v1(
            aud,
            AudioEntryV1 {
                sampling_rate: None,
                channel_layout: Some(ChannelLayout {
                    stream_structure: ChannelLayout::CHANNEL_STRUCTURED,
                    channels: Some(ChannelStructure::Defined {
                        defined_layout: 0,
                        omitted_channels_map: 0,
                    }),
                    object_count: None,
                }),
            },
        )
        .is_err());
    // Mutual exclusion, both orders.
    m.set_audio_entry_v1(aud, AudioEntryV1::default()).unwrap();
    assert!(m
        .set_sound_description_v1(aud, SoundV1::default(), false)
        .is_err());
    let mut m2 = MovMuxer::new();
    let aud2 = add_audio(&mut m2, 2);
    m2.set_sound_description_v1(aud2, SoundV1::default(), true)
        .unwrap();
    assert!(m2
        .set_audio_entry_v1(aud2, AudioEntryV1::default())
        .is_err());
}

#[test]
fn iso_v1_stsd_box_takes_version_1() {
    // §8.5.2: the enclosing stsd FullBox must take version 1. Locate
    // the stsd atom in the emitted bytes and check its version byte.
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, 2);
    m.set_audio_entry_v1(tid, AudioEntryV1::default()).unwrap();
    let bytes = m.encode_to_vec().unwrap();
    let pos = bytes
        .windows(4)
        .position(|w| w == b"stsd")
        .expect("stsd present");
    assert_eq!(bytes[pos + 4], 1, "stsd FullBox version");
}
