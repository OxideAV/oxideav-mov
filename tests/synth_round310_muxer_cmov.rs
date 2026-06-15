//! Round 310 — `MovMuxer` write-side compressed-movie-resource
//! emission (QTFF, 2001-03-01, pp. 80 – 81, "Allowing QuickTime to
//! Compress the Movie Resource" / Table 2-5).
//!
//! Round 283 landed the full `cmov` read path plus the
//! `compress_movie_resource` / `Cmov::to_body_bytes` building blocks,
//! but the muxer never elected to compress the movie resource it wrote.
//! This round wires an opt-in `MovMuxer::with_compressed_movie_resource`
//! flag: when set, the non-fragmented write path replaces the trailing
//! plain `moov` with a `moov > cmov > dcom + cmvd` tree carrying the
//! zlib-deflated movie resource.
//!
//! Per QTFF p. 30 the complete movie resource is the full `moov` atom
//! (header included); compressing exactly that means the output
//! decompresses back to a byte-identical plain-`moov` file. These tests
//! build the same movie twice — once plain, once compressed — and
//! assert both open to identical track / packet state through this
//! crate's own [`MovDemuxer`] (which transparently decompresses on
//! open), plus that the compressed output is smaller and surfaces the
//! `'zlib'` algorithm FourCC while the plain output reports `None`.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, DCOM_ALG_ZLIB};

/// Build a deterministic multi-sample video movie. `compress` selects
/// the plain vs. compressed-movie-resource write path. Returns the
/// encoded bytes plus the input samples.
fn build_video_mov(compress: bool) -> (Vec<u8>, Vec<MuxSample>) {
    // Highly-redundant payloads so the movie resource (and the sample
    // bytes themselves) deflate well — keeps the "compressed is smaller"
    // assertion robust across zlib levels.
    let samples: Vec<MuxSample> = (0..8u32)
        .map(|i| MuxSample {
            data: vec![0xAB; 64],
            duration: 1000,
            keyframe: i == 0,
            composition_offset: 0,
        })
        .collect();

    let mut m = MovMuxer::new()
        .with_movie_timescale(600)
        .with_compressed_movie_resource(compress);
    assert_eq!(m.compresses_movie_resource(), compress);

    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 320,
            height: 240,
        },
        30000,
        samples.clone(),
        &[],
    );

    let bytes = m.encode_to_vec().expect("encode video MOV");
    (bytes, samples)
}

/// Assert a demuxed file matches the expected sample set byte-for-byte.
fn assert_roundtrip(bytes: &[u8], samples_in: &[MuxSample], expect_alg: Option<[u8; 4]>) {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.to_vec()));
    let mut d = MovDemuxer::open(cur).expect("open muxed file");

    assert_eq!(
        d.compressed_movie_algorithm, expect_alg,
        "compressed_movie_algorithm surface"
    );

    // mvhd / track surface survives the (de)compression round-trip.
    let mvhd = d.mvhd.as_ref().expect("mvhd present");
    assert_eq!(mvhd.time_scale, 600);
    assert_eq!(d.tracks.len(), 1);
    let tr = &d.tracks[0];
    assert!(tr.is_video());
    assert_eq!(tr.tkhd.track_id, 1);
    assert_eq!(tr.tkhd.width(), 320);
    assert_eq!(tr.tkhd.height(), 240);
    assert_eq!(tr.sample_table.sample_count(), samples_in.len() as u32);

    // Packet bytes round-trip exactly — chunk offsets stay valid even
    // though the trailing moov was compressed (mdat is laid down first).
    for (i, sample_in) in samples_in.iter().enumerate() {
        let pkt = d
            .next_packet()
            .unwrap_or_else(|e| panic!("next_packet at {i}: {e:?}"));
        assert_eq!(pkt.stream_index, 0);
        assert_eq!(pkt.data, sample_in.data, "byte mismatch at sample {i}");
    }
    match d.next_packet() {
        Err(oxideav_core::Error::Eof) => {}
        other => panic!(
            "expected Eof after {} packets, got {other:?}",
            samples_in.len()
        ),
    }
}

#[test]
fn compressed_movie_resource_roundtrips_identically_to_plain() {
    let (plain, samples) = build_video_mov(false);
    let (compressed, _) = build_video_mov(true);

    // Plain path: no cmov layer, algorithm surface absent.
    assert_roundtrip(&plain, &samples, None);

    // Compressed path: transparently decompressed on open, surfaces the
    // 'zlib' algorithm FourCC, and yields identical sample bytes.
    assert_roundtrip(&compressed, &samples, Some(DCOM_ALG_ZLIB));
}

#[test]
fn compressed_output_is_smaller_than_plain() {
    let (plain, _) = build_video_mov(false);
    let (compressed, _) = build_video_mov(true);
    assert!(
        compressed.len() < plain.len(),
        "compressed movie resource should shrink the file: {} vs {} bytes",
        compressed.len(),
        plain.len()
    );
}

#[test]
fn plain_output_carries_no_cmov() {
    let (plain, _) = build_video_mov(false);
    // A plain file's top-level moov contains mvhd/trak, never a cmov
    // child — the FourCC must not appear anywhere in the byte stream.
    assert!(
        !contains(&plain, b"cmov"),
        "plain output must not carry a cmov atom"
    );
}

#[test]
fn compressed_output_carries_the_cmov_tree() {
    let (compressed, _) = build_video_mov(true);
    // The compressed file's moov wraps a cmov > dcom + cmvd tree.
    assert!(contains(&compressed, b"cmov"), "cmov atom expected");
    assert!(contains(&compressed, b"dcom"), "dcom atom expected");
    assert!(contains(&compressed, b"cmvd"), "cmvd atom expected");
}

#[test]
fn compression_flag_off_by_default() {
    let m = MovMuxer::new();
    assert!(!m.compresses_movie_resource());
}

/// True iff `needle` appears anywhere in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
