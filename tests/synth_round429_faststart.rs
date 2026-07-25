//! Round 429 — **moov-before-mdat ("faststart") placement**
//! (`MoovPlacement::BeforeMdat`, QTFF "Optimizing … for Web Playback"
//! p. 365: "The important change … was for QuickTime to place global
//! movie information at the beginning of the file").
//!
//! Chunk offsets are file-absolute, so putting `moov` first makes them
//! depend on the `moov`'s own size; the muxer resolves that with a
//! fixed-point sizing pass. These tests pin the top-level atom order,
//! the demuxer's `is_faststart()` classification, offset correctness
//! (byte-level and through packet round-trips), the combination with
//! interleaved chunking and edit lists, and the documented `cmov`
//! incompatibility.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{
    ChunkStrategy, MoovPlacement, MovDemuxer, MovMuxer, MuxEdit, MuxSample, MuxTrackKind,
};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn video_samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![0x10 + i as u8; 24],
            duration: 100,
            keyframe: i % 4 == 0,
            composition_offset: 0,
        })
        .collect()
}

fn audio_samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![0x80u8.wrapping_add(i as u8); 9],
            duration: 800,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn two_track_muxer() -> MovMuxer {
    let mut m = MovMuxer::new();
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 2,
        },
        600,
        video_samples(12),
        &[],
    );
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        audio_samples(12),
        &[],
    );
    m
}

/// Top-level atom fourccs in file order.
fn top_level_atoms(bytes: &[u8]) -> Vec<[u8; 4]> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let mut fourcc = [0u8; 4];
        fourcc.copy_from_slice(&bytes[pos + 4..pos + 8]);
        out.push(fourcc);
        assert!(size >= 8, "classic atom size");
        pos += size;
    }
    assert_eq!(pos, bytes.len(), "atoms must tile the file exactly");
    out
}

fn drain(d: &mut MovDemuxer) -> Vec<(u32, Vec<u8>, Option<i64>)> {
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push((p.stream_index, p.data, p.pts));
    }
    out
}

#[test]
fn faststart_orders_ftyp_moov_mdat() {
    let bytes = two_track_muxer().with_faststart().encode_to_vec().unwrap();
    let atoms = top_level_atoms(&bytes);
    assert_eq!(atoms, vec![*b"ftyp", *b"moov", *b"mdat"]);
    // Default stays mdat-first.
    let bytes = two_track_muxer().encode_to_vec().unwrap();
    let atoms = top_level_atoms(&bytes);
    assert_eq!(atoms, vec![*b"ftyp", *b"mdat", *b"moov"]);
}

#[test]
fn demuxer_classifies_faststart() {
    let d = open(two_track_muxer().with_faststart().encode_to_vec().unwrap());
    assert!(d.is_faststart(), "moov-first file must classify faststart");
    let d = open(two_track_muxer().encode_to_vec().unwrap());
    assert!(!d.is_faststart(), "mdat-first file must not");
}

#[test]
fn builder_and_getter_round_trip() {
    let m = MovMuxer::new();
    assert_eq!(m.moov_placement(), MoovPlacement::AfterMdat);
    let m = m.with_moov_placement(MoovPlacement::BeforeMdat);
    assert_eq!(m.moov_placement(), MoovPlacement::BeforeMdat);
    let m = MovMuxer::new().with_faststart();
    assert_eq!(m.moov_placement(), MoovPlacement::BeforeMdat);
}

#[test]
fn packets_are_placement_invariant() {
    let mut fast = open(two_track_muxer().with_faststart().encode_to_vec().unwrap());
    let mut classic = open(two_track_muxer().encode_to_vec().unwrap());
    assert_eq!(
        drain(&mut fast),
        drain(&mut classic),
        "same packets, bytes and pts regardless of moov placement"
    );
}

#[test]
fn faststart_chunk_offsets_point_at_the_sample_bytes() {
    // Byte-level: the resolved sample offsets must address the exact
    // sample payloads inside the trailing mdat.
    let bytes = two_track_muxer().with_faststart().encode_to_vec().unwrap();
    let d = open(bytes.clone());
    for (stream, samples) in [(0usize, video_samples(12)), (1, audio_samples(12))] {
        for (i, s) in samples.iter().enumerate() {
            let off = d
                .sample_offset(stream, i as u32)
                .expect("sample offset resolved") as usize;
            assert_eq!(
                &bytes[off..off + s.data.len()],
                s.data.as_slice(),
                "stream {stream} sample {i} bytes at resolved offset"
            );
        }
    }
}

#[test]
fn faststart_combines_with_interleaving() {
    let bytes = two_track_muxer()
        .with_faststart()
        .with_chunk_strategy(ChunkStrategy::InterleaveByMovieTicks(300))
        .encode_to_vec()
        .unwrap();
    let atoms = top_level_atoms(&bytes);
    assert_eq!(atoms, vec![*b"ftyp", *b"moov", *b"mdat"]);
    let mut d = open(bytes);
    assert!(d.is_faststart());
    // 12 video samples of 100 ticks = 1200 ticks at ts 600 (2 s) ⇒ 4
    // half-second chunks of 3; 12 audio samples of 800 ticks = 9600 at
    // ts 8000 (1.2 s) ⇒ 3 chunks (5, 5, 2 media-samples? no: period
    // 300 movie ticks = 4000 media ticks = 5 samples ⇒ 5+5+2).
    assert_eq!(d.chunk_count(0), Some(4));
    assert_eq!(d.chunk_count(1), Some(3));
    assert_eq!(d.samples_in_chunk(1, 1), Some(5));
    assert_eq!(d.samples_in_chunk(1, 3), Some(2));
    // And the packet payloads still round-trip per track.
    let packets = drain(&mut d);
    let video: Vec<&Vec<u8>> = packets.iter().filter(|p| p.0 == 0).map(|p| &p.1).collect();
    let expect = video_samples(12);
    assert_eq!(video.len(), 12);
    for (got, want) in video.iter().zip(expect.iter()) {
        assert_eq!(*got, &want.data);
    }
}

#[test]
fn faststart_preserves_edit_lists() {
    let mut m = two_track_muxer().with_faststart();
    m.set_edit_list(
        1,
        &[
            MuxEdit::empty(300),
            MuxEdit::segment(600, 200), // trim 200 media ticks off the head
        ],
    )
    .expect("edit list accepted");
    let d = open(m.encode_to_vec().unwrap());
    assert!(d.is_faststart());
    let t = &d.tracks[0];
    assert_eq!(t.edit_start_delay(), 300);
    assert_eq!(t.edit_media_start(), Some(200));
}

#[test]
fn faststart_rejects_compressed_movie_resource() {
    let m = two_track_muxer()
        .with_faststart()
        .with_compressed_movie_resource(true);
    let err = m.encode_to_vec();
    assert!(
        err.is_err(),
        "cmov + moov-before-mdat must be rejected (compressed size would depend on offsets)"
    );
}

#[test]
fn faststart_keeps_aux_slab_addressable() {
    let blobs: Vec<Vec<u8>> = (0..12u8).map(|i| vec![i | 0x30; 5]).collect();
    let mut m = two_track_muxer().with_faststart();
    m.set_sample_aux(
        1,
        oxideav_mov::SampleAuxStream {
            aux_info_type: Some(*b"cenc"),
            aux_info_type_parameter: 0,
            per_sample: blobs.clone(),
        },
    )
    .expect("attach aux stream");
    let bytes = m.encode_to_vec().unwrap();
    let d = open(bytes.clone());
    let (saiz, saio) = d.sample_aux_info(0, b"cenc", 0);
    let saiz = saiz.expect("saiz present");
    let saio = saio.expect("saio present");
    assert_eq!(saiz.sample_count, 12);
    assert!(saio.is_single_chunk());
    let off = saio.offset_for(0).expect("slab offset") as usize;
    let mut expect = Vec::new();
    for b in &blobs {
        expect.extend_from_slice(b);
    }
    assert_eq!(&bytes[off..off + expect.len()], expect.as_slice());
}
