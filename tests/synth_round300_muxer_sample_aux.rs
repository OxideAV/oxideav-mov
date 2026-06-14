//! Round 300 — `MovMuxer` write-side `saiz` / `saio` emission at
//! `stbl` scope (ISO/IEC 14496-12 §8.7.8 / §8.7.9).
//!
//! The round-147 read path consumes `stbl`-scope sample-auxiliary-
//! information boxes; round 300 lets the muxer *write* them. A track
//! carrying a [`SampleAuxStream`] lays each sample's opaque aux blob
//! into `mdat` (contiguously, right after the track's sample data),
//! emits a `saiz` describing the per-sample sizes, and a single-entry
//! `saio` whose absolute file offset points at the first blob
//! (§8.7.9.3 — "When in the Sample Table Box, the offsets are absolute
//! … If entry_count is one, then the Sample Auxiliary Information for
//! all Chunks … is contiguous in the file").
//!
//! These tests build a file through [`MovMuxer`], demux it back through
//! [`MovDemuxer`], and confirm the round-tripped `saiz` sizes and
//! `saio` offset both match what was written, and that the bytes the
//! offset points at equal the original blobs.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, SampleAuxStream};

/// Build a 4-sample audio MOV carrying a sample-aux stream. Returns the
/// encoded bytes plus the per-sample aux blobs that were written.
fn build_audio_mov_with_aux(
    aux_info_type: Option<[u8; 4]>,
    aux_info_type_parameter: u32,
    blobs: Vec<Vec<u8>>,
) -> Vec<u8> {
    let samples: Vec<MuxSample> = (0..blobs.len())
        .map(|i| MuxSample {
            data: vec![(0xA0 | i) as u8; 8],
            duration: 1024,
            keyframe: true,
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
    m.set_sample_aux(
        id,
        SampleAuxStream {
            aux_info_type,
            aux_info_type_parameter,
            per_sample: blobs,
        },
    )
    .expect("attach sample-aux stream");
    m.encode_to_vec().expect("encode audio MOV with sample-aux")
}

#[test]
fn uniform_blobs_roundtrip_via_default_size_form() {
    let blobs = vec![
        vec![0x11u8; 16],
        vec![0x22; 16],
        vec![0x33; 16],
        vec![0x44; 16],
    ];
    let bytes = build_audio_mov_with_aux(Some(*b"cenc"), 0, blobs.clone());

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
    let d = MovDemuxer::open(cur).expect("open muxed file");

    let (saiz, saio) = d.sample_aux_info(0, b"cenc", 0);
    let saiz = saiz.expect("saiz present for cenc discriminator");
    let saio = saio.expect("saio present for cenc discriminator");

    // Uniform 16-byte blobs ⇒ default-size form.
    assert_eq!(saiz.default_sample_info_size, 16);
    assert_eq!(saiz.sample_count, 4);
    for i in 0..4 {
        assert_eq!(saiz.size_for(i), Some(16));
    }

    // Single contiguous slab.
    assert!(saio.is_single_chunk());
    let off = saio.offset_for(0).expect("offset[0]") as usize;

    // The bytes at the absolute offset are the concatenated blobs.
    let mut expect = Vec::new();
    for b in &blobs {
        expect.extend_from_slice(b);
    }
    assert_eq!(&bytes[off..off + expect.len()], expect.as_slice());
}

#[test]
fn varying_blobs_roundtrip_via_per_sample_table() {
    let blobs = vec![vec![1u8; 8], vec![], vec![2u8; 24], vec![3u8; 1]];
    let bytes = build_audio_mov_with_aux(Some(*b"cbcs"), 7, blobs.clone());

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
    let d = MovDemuxer::open(cur).expect("open muxed file");

    let (saiz, saio) = d.sample_aux_info(0, b"cbcs", 7);
    let saiz = saiz.expect("saiz present");
    let saio = saio.expect("saio present");

    assert_eq!(saiz.default_sample_info_size, 0); // table form
    assert_eq!(saiz.sample_count, 4);
    assert_eq!(saiz.sample_info_sizes, vec![8u8, 0, 24, 1]);
    assert_eq!(saiz.size_for(0), Some(8));
    assert_eq!(saiz.size_for(1), Some(0));
    assert_eq!(saiz.size_for(2), Some(24));
    assert_eq!(saiz.size_for(3), Some(1));

    // The discriminator pair round-trips.
    let a = saiz.aux_info_type.expect("aux pair");
    assert_eq!(&a.aux_info_type, b"cbcs");
    assert_eq!(a.aux_info_type_parameter, 7);

    // Each sample's bytes land contiguously from the saio offset.
    let mut off = saio.offset_for(0).expect("offset[0]") as usize;
    for b in &blobs {
        assert_eq!(&bytes[off..off + b.len()], b.as_slice());
        off += b.len();
    }
}

#[test]
fn implicit_discriminator_matches_zero_pair() {
    // No aux_info_type ⇒ flags & 1 clear ⇒ matches the all-zero pair.
    let blobs = vec![vec![9u8; 4]; 3];
    let bytes = build_audio_mov_with_aux(None, 0, blobs.clone());

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
    let d = MovDemuxer::open(cur).expect("open muxed file");

    // Implicit discriminator: look up the zero pair.
    let (saiz, saio) = d.sample_aux_info(0, &[0, 0, 0, 0], 0);
    let saiz = saiz.expect("saiz present for implicit discriminator");
    let saio = saio.expect("saio present for implicit discriminator");
    assert!(saiz.aux_info_type.is_none());
    assert_eq!(saiz.default_sample_info_size, 4);
    assert_eq!(saiz.sample_count, 3);

    let off = saio.offset_for(0).expect("offset[0]") as usize;
    assert_eq!(&bytes[off..off + 4], &[9, 9, 9, 9]);
}

#[test]
fn no_sample_aux_track_emits_no_boxes() {
    // A plain track (no stream attached) carries no saiz/saio.
    let samples = vec![MuxSample {
        data: vec![0u8; 8],
        duration: 1024,
        keyframe: true,
    }];
    let mut m = MovMuxer::new();
    m.add_track(
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
    let bytes = m.encode_to_vec().expect("encode plain audio MOV");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open muxed file");
    let (saiz, saio) = d.sample_aux_info(0, b"cenc", 0);
    assert!(saiz.is_none());
    assert!(saio.is_none());
}

#[test]
fn aux_slab_does_not_corrupt_sample_data_offsets() {
    // Verify the demuxer still reads the track's actual sample bytes
    // correctly even though the aux slab sits between this track's
    // samples and the next chunk region.
    let blobs = vec![vec![0xEEu8; 5]; 2];
    let bytes = build_audio_mov_with_aux(Some(*b"cenc"), 0, blobs);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open muxed file");

    // Sample 0 data was vec![0xA0; 8]; sample 1 was vec![0xA1; 8].
    let pkt0 = d.next_packet().expect("packet 0");
    assert_eq!(pkt0.data, vec![0xA0u8; 8]);
    let pkt1 = d.next_packet().expect("packet 1");
    assert_eq!(pkt1.data, vec![0xA1u8; 8]);
}
