//! MPEG-4 audio (`mp4a`) in QuickTime: the stream extradata must be the
//! `esds` DecoderSpecificInfo — the ISO/IEC 14496-3 §1.6.2.1
//! `AudioSpecificConfig` — not the raw sound-description extension
//! atoms (`wave` / `esds` / Terminator). Handing the atoms to the AAC
//! decoder made it read `audioObjectType 0` and refuse the stream.
//!
//! Fixture `aac_2s.mov`: 2 s of stereo 44.1 kHz AAC-LC (see `seek.rs`);
//! its ASC, as reported by `ffprobe -show_streams -show_data`, is
//! `12 10 56 e5 00` (AAC-LC, 44.1 kHz, stereo + the §1.6.5
//! `syncExtensionType 0x2b7` trailer with `sbrPresentFlag = 0`).

#![cfg(feature = "registry")]

use std::fs::File;
use std::path::PathBuf;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{esds_decoder_specific_info, MovDemuxer};

#[test]
fn mp4a_extradata_is_the_audio_specific_config() {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/aac_2s.mov");
    if !p.exists() {
        return;
    }
    let f: Box<dyn ReadSeek> = Box::new(File::open(&p).unwrap());
    let d = MovDemuxer::open(f).unwrap();
    let audio = d
        .streams()
        .iter()
        .find(|s| s.params.media_type == oxideav_core::MediaType::Audio)
        .expect("audio stream");
    assert_eq!(audio.params.extradata, [0x12, 0x10, 0x56, 0xE5, 0x00]);
    assert_eq!(audio.params.sample_rate, Some(44_100));
    assert_eq!(audio.params.channels, Some(2));
}

/// ES_Descriptor (0x03) → DecoderConfigDescriptor (0x04) → DSI (0x05),
/// optionally with the ES_Descriptor flag fields and multi-byte
/// (`0x80`-continued) sizes.
fn es_descriptor(dsi: &[u8], flags: u8, long_sizes: bool) -> Vec<u8> {
    let size = |n: usize| -> Vec<u8> {
        if long_sizes {
            vec![0x80, 0x80, 0x80, n as u8]
        } else {
            vec![n as u8]
        }
    };
    let mut dsi_d = vec![0x05];
    dsi_d.extend(size(dsi.len()));
    dsi_d.extend_from_slice(dsi);
    let mut dcd_body = vec![0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    dcd_body.extend(dsi_d);
    let mut dcd = vec![0x04];
    dcd.extend(size(dcd_body.len()));
    dcd.extend(dcd_body);
    let mut es_body = vec![0x00, 0x01, flags];
    if flags & 0x80 != 0 {
        es_body.extend([0, 7]);
    }
    if flags & 0x40 != 0 {
        es_body.extend([3, b'a', b'b', b'c']);
    }
    if flags & 0x20 != 0 {
        es_body.extend([0, 9]);
    }
    es_body.extend(dcd);
    es_body.extend([0x06, 0x01, 0x02]); // SLConfigDescriptor
    let mut es = vec![0x03];
    es.extend(size(es_body.len()));
    es.extend(es_body);
    es
}

#[test]
fn dsi_extraction_handles_flags_and_long_sizes() {
    let asc = [0x11, 0x90];
    for flags in [0x00, 0x80, 0x40, 0x20, 0xE0] {
        for long in [false, true] {
            assert_eq!(
                esds_decoder_specific_info(&es_descriptor(&asc, flags, long)).as_deref(),
                Some(&asc[..]),
                "flags {flags:#x} long {long}"
            );
        }
    }
}

#[test]
fn dsi_extraction_rejects_malformed_input() {
    assert_eq!(esds_decoder_specific_info(&[]), None);
    assert_eq!(esds_decoder_specific_info(&[0x04, 0x02, 0, 0]), None);
    let mut truncated = es_descriptor(&[0x12, 0x10], 0, false);
    truncated.truncate(10);
    assert_eq!(esds_decoder_specific_info(&truncated), None);
}
