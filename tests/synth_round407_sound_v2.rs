//! Round 407 — QTFF **Sound Sample Description version 2** write side
//! (`MovMuxer::set_sound_description_v2`, QTFF 2012-08-14 edition
//! pp. 181–182) plus the sound-description extension atoms the same
//! edition documents (`wave` siDecompressionParam / `esds` /
//! Terminator, pp. 183–187). Every shape round-trips through the
//! round-407 read side (`SampleDescription::sound_v2` / `LpcmFlags` /
//! `si_decompression_param` / `esds` / `extension_terminator`).

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    build_esds_atom, AudioEntryV1, AudioEntryV2, FragmentationMode, LpcmFlags, MovDemuxer,
    MovMuxer, MuxSample, MuxTrackKind, SoundV1, WaveChild,
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

fn add_audio(m: &mut MovMuxer, format: [u8; 4], channels: u16, extra: &[u8]) -> u32 {
    m.add_track(
        MuxTrackKind::Audio {
            format,
            channels,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        samples(4),
        extra,
    )
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

#[test]
fn v2_lpcm_high_resolution_roundtrip() {
    // The v2 raison d'être: a 192 kHz rate the 16.16 field cannot
    // carry, 24-bit packed signed big-endian lpcm, 6 channels.
    let flags =
        LpcmFlags(LpcmFlags::IS_BIG_ENDIAN | LpcmFlags::IS_SIGNED_INTEGER | LpcmFlags::IS_PACKED);
    let entry = AudioEntryV2 {
        audio_sample_rate: 192_000.0,
        const_bits_per_channel: 24,
        format_specific_flags: flags,
        const_bytes_per_audio_packet: 18,
        const_lpcm_frames_per_audio_packet: 1,
    };
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, *b"lpcm", 6, &[]);
    m.set_sound_description_v2(tid, entry).unwrap();
    let d = open(m.encode_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(&sd.format, b"lpcm");
    assert_eq!(sd.audio_version, 2);
    let v2 = sd.sound_v2.expect("v2 fields surfaced");
    assert_eq!(v2.size_of_struct_only, 72);
    assert_eq!(v2.audio_sample_rate, 192_000.0);
    assert_eq!(v2.num_audio_channels, 6);
    assert_eq!(v2.const_bits_per_channel, 24);
    assert_eq!(v2.format_specific_flags, flags);
    assert_eq!(v2.const_bytes_per_audio_packet, 18);
    assert_eq!(v2.const_lpcm_frames_per_audio_packet, 1);
    // Legacy fields carry the v2 truth, not the always* constants.
    assert_eq!(sd.channels, 6);
    assert_eq!(sd.bits_per_sample, 24);
    assert_eq!(sd.sample_rate, 192_000);
    assert_eq!(sd.audio_sample_rate_hz(), 192_000.0);
    // The on-wire alwaysMinus2 must not read back as the v1 VBR form.
    assert_eq!(sd.audio_compression_id, -2);
    assert!(!sd.is_vbr());
    assert!(sd.sound_v1.is_none());
    assert!(!sd.iso_audio_entry_v1);
}

#[test]
fn v2_non_integer_rate_survives_float64() {
    // A fractional rate (NTSC-pulled 44.1 kHz) is exactly the case
    // the Float64 field exists for; the integer mirror rounds.
    let entry = AudioEntryV2 {
        audio_sample_rate: 44_099.9560439560,
        ..AudioEntryV2::default()
    };
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, *b"lpcm", 2, &[]);
    m.set_sound_description_v2(tid, entry).unwrap();
    let d = open(m.encode_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.sound_v2.unwrap().audio_sample_rate, 44_099.9560439560);
    assert_eq!(sd.audio_sample_rate_hz(), 44_099.9560439560);
    assert_eq!(sd.sample_rate, 44_100, "integer mirror rounds half-up");
}

#[test]
fn v2_extensions_start_at_size_of_struct_only() {
    // Codec-config extra atoms must land at byte 72 of the entry —
    // where sizeOfStructOnly points — and read back typed: here a
    // wave{frma, esds, Terminator} plus a direct chan-free trailing
    // terminator check via the wave itself.
    let descriptor = [0x03, 0x19, 0x00, 0x02, 0x00];
    let wave = oxideav_mov::SiDecompressionParam {
        children: vec![
            WaveChild::format(*b"mp4a"),
            WaveChild::elementary_stream_descriptor(&descriptor),
        ],
        terminated: true,
        non_atom_data: Vec::new(),
    };
    let entry = AudioEntryV2 {
        audio_sample_rate: 96_000.0,
        ..AudioEntryV2::default()
    };
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, *b"mp4a", 2, &wave.to_atom_bytes());
    m.set_sound_description_v2(tid, entry).unwrap();
    let d = open(m.encode_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.sound_v2.unwrap().size_of_struct_only, 72);
    let w = sd.si_decompression_param.as_ref().expect("wave surfaced");
    assert_eq!(*w, wave, "wave round-trips exactly");
    assert_eq!(w.format(), Some(*b"mp4a"));
    assert_eq!(w.esds(), Some(&descriptor[..]));
    assert!(w.terminated);
}

#[test]
fn v0_entry_with_direct_esds_and_terminator() {
    // The extension atoms are not v2-only: a version-0 mp4a entry
    // carrying a direct esds + Terminator reads back typed too.
    let descriptor = [0x03, 0x02, 0xCC];
    let mut extra = build_esds_atom(&descriptor);
    extra.extend_from_slice(&8u32.to_be_bytes());
    extra.extend_from_slice(&[0u8; 4]); // Terminator
    let mut m = MovMuxer::new();
    add_audio(&mut m, *b"mp4a", 2, &extra);
    let d = open(m.encode_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.audio_version, 0);
    assert_eq!(sd.esds.as_deref(), Some(&descriptor[..]));
    assert!(sd.extension_terminator);
}

#[test]
fn v2_roundtrips_through_fragmented_path() {
    let entry = AudioEntryV2 {
        audio_sample_rate: 176_400.0,
        const_bits_per_channel: 32,
        format_specific_flags: LpcmFlags(LpcmFlags::IS_FLOAT | LpcmFlags::IS_PACKED),
        const_bytes_per_audio_packet: 8,
        const_lpcm_frames_per_audio_packet: 1,
    };
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    let tid = add_audio(&mut m, *b"lpcm", 2, &[]);
    m.set_sound_description_v2(tid, entry).unwrap();
    let d = open(m.encode_fragmented_to_vec().unwrap());
    let sd = &d.tracks[0].sample_descriptions[0];
    let v2 = sd.sound_v2.expect("v2 in fragmented init moov");
    assert_eq!(v2.audio_sample_rate, 176_400.0);
    assert_eq!(v2.const_bits_per_channel, 32);
    assert!(v2.format_specific_flags.is_float());
    assert!(v2.format_specific_flags.is_packed());
    assert!(!v2.format_specific_flags.is_big_endian());
}

#[test]
fn v2_wire_layout_carries_always_constants() {
    // Pin the on-wire back-compatibility constants (QTFF 2012-08-14
    // p. 181): locate the stsd entry and check the version-0
    // positions hold always3/always16/alwaysMinus2/always0/
    // always65536 and the 7F000000 word.
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, *b"lpcm", 2, &[]);
    m.set_sound_description_v2(
        tid,
        AudioEntryV2 {
            audio_sample_rate: 48_000.0,
            ..AudioEntryV2::default()
        },
    )
    .unwrap();
    let bytes = m.encode_to_vec().unwrap();
    let pos = bytes
        .windows(4)
        .position(|w| w == b"lpcm")
        .expect("lpcm entry present");
    let entry = &bytes[pos - 4..]; // entry starts at its size word
    let body = &entry[16..16 + 56];
    assert_eq!(u16::from_be_bytes([body[0], body[1]]), 2, "version");
    assert_eq!(u16::from_be_bytes([body[8], body[9]]), 3, "always3");
    assert_eq!(u16::from_be_bytes([body[10], body[11]]), 16, "always16");
    assert_eq!(i16::from_be_bytes([body[12], body[13]]), -2, "alwaysMinus2");
    assert_eq!(u16::from_be_bytes([body[14], body[15]]), 0, "always0");
    assert_eq!(
        u32::from_be_bytes([body[16], body[17], body[18], body[19]]),
        65536,
        "always65536"
    );
    assert_eq!(
        u32::from_be_bytes([body[20], body[21], body[22], body[23]]),
        72,
        "sizeOfStructOnly"
    );
    assert_eq!(
        u32::from_be_bytes([body[36], body[37], body[38], body[39]]),
        0x7F00_0000,
        "always7F000000"
    );
}

#[test]
fn v2_validation_and_mutual_exclusion() {
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
    let aud = add_audio(&mut m, *b"lpcm", 2, &[]);
    let ok = AudioEntryV2 {
        audio_sample_rate: 48_000.0,
        ..AudioEntryV2::default()
    };

    // Non-audio track / unknown id.
    assert!(m.set_sound_description_v2(vid, ok).is_err());
    assert!(m.set_sound_description_v2(99, ok).is_err());
    // Rate must be finite and positive.
    for bad in [0.0, -48_000.0, f64::NAN, f64::INFINITY] {
        assert!(m
            .set_sound_description_v2(
                aud,
                AudioEntryV2 {
                    audio_sample_rate: bad,
                    ..AudioEntryV2::default()
                },
            )
            .is_err());
    }
    // Mutual exclusion, every ordering. v2 after v1:
    m.set_sound_description_v1(aud, SoundV1::default(), false)
        .unwrap();
    assert!(m.set_sound_description_v2(aud, ok).is_err());
    // v1 / ISO-v1 after v2:
    let mut m2 = MovMuxer::new();
    let aud2 = add_audio(&mut m2, *b"lpcm", 2, &[]);
    m2.set_sound_description_v2(aud2, ok).unwrap();
    assert!(m2
        .set_sound_description_v1(aud2, SoundV1::default(), false)
        .is_err());
    assert!(m2
        .set_audio_entry_v1(aud2, AudioEntryV1::default())
        .is_err());
    // Re-setting v2 itself is allowed (idempotent reconfiguration).
    assert!(m2
        .set_sound_description_v2(
            aud2,
            AudioEntryV2 {
                audio_sample_rate: 96_000.0,
                ..AudioEntryV2::default()
            },
        )
        .is_ok());
    // v2 after ISO-v1:
    let mut m3 = MovMuxer::new();
    let aud3 = add_audio(&mut m3, *b"lpcm", 2, &[]);
    m3.set_audio_entry_v1(aud3, AudioEntryV1::default())
        .unwrap();
    assert!(m3.set_sound_description_v2(aud3, ok).is_err());
}

#[test]
fn v2_stsd_box_stays_version_0() {
    // The QTFF v2 sound description lives in a version-0 stsd (the
    // FullBox version-1 promotion is ISO AudioSampleEntryV1-only).
    let mut m = MovMuxer::new();
    let tid = add_audio(&mut m, *b"lpcm", 2, &[]);
    m.set_sound_description_v2(
        tid,
        AudioEntryV2 {
            audio_sample_rate: 48_000.0,
            ..AudioEntryV2::default()
        },
    )
    .unwrap();
    let bytes = m.encode_to_vec().unwrap();
    let pos = bytes
        .windows(4)
        .position(|w| w == b"stsd")
        .expect("stsd present");
    assert_eq!(bytes[pos + 4], 0, "stsd FullBox version");
}
