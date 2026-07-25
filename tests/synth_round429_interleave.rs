//! Round 429 — **time-interleaved `mdat` chunking**
//! (`ChunkStrategy::InterleaveByMovieTicks`, QTFF "Interleaving Movie
//! Data" p. 358).
//!
//! The spec's guidance: "In order to get optimal movie playback, you
//! must create the movie with interleaved data … the data for a
//! particular time in the movie is close together in the file", with a
//! worked example cutting the audio into half-second chunks. These
//! tests build a 2-second video+audio movie, interleave it at 300
//! movie ticks (0.5 s at the default 600 movie timescale), and assert
//! through the demuxer that (a) each track really was cut into
//! multiple chunks with a run-length `stsc` / multi-entry `stco`,
//! (b) the chunks of the two tracks alternate in file order, and
//! (c) every per-track byte stream and timeline survives unchanged
//! relative to the single-chunk layout.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{ChunkStrategy, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

/// 20 video samples, 60 ticks each at timescale 600 ⇒ 2 s, 10 fps.
/// Every sample carries a distinct fill byte so cross-track byte
/// shuffles can't cancel out.
fn video_samples() -> Vec<MuxSample> {
    (0..20)
        .map(|i| MuxSample {
            data: vec![0x40 + i as u8; 32],
            duration: 60,
            keyframe: i % 5 == 0,
            composition_offset: 0,
        })
        .collect()
}

/// 40 audio samples, 400 ticks each at timescale 8000 ⇒ 2 s.
fn audio_samples() -> Vec<MuxSample> {
    (0..40)
        .map(|i| MuxSample {
            data: vec![0xA0u8.wrapping_add(i as u8); 11],
            duration: 400,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn build_two_track(strategy: ChunkStrategy) -> Vec<u8> {
    let mut m = MovMuxer::new().with_chunk_strategy(strategy);
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 2,
        },
        600,
        video_samples(),
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
        audio_samples(),
        &[],
    );
    m.encode_to_vec().expect("encode two-track MOV")
}

/// Drain all packets, returning (stream_index, data) in file/emission
/// order.
fn drain(d: &mut MovDemuxer) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push((p.stream_index, p.data));
    }
    out
}

#[test]
fn interleave_cuts_half_second_chunks_on_both_tracks() {
    let d = open(build_two_track(ChunkStrategy::InterleaveByMovieTicks(300)));
    // 2 s at a 0.5 s period ⇒ 4 chunks per track.
    assert_eq!(d.chunk_count(0), Some(4), "video chunk count");
    assert_eq!(d.chunk_count(1), Some(4), "audio chunk count");
    // 20 video samples over 4 chunks ⇒ 5 per chunk; 40 audio ⇒ 10.
    for c in 1..=4 {
        assert_eq!(d.samples_in_chunk(0, c), Some(5), "video chunk {c}");
        assert_eq!(d.samples_in_chunk(1, c), Some(10), "audio chunk {c}");
    }
}

#[test]
fn interleaved_chunks_alternate_in_file_order() {
    let bytes = build_two_track(ChunkStrategy::InterleaveByMovieTicks(300));
    let d = open(bytes);
    // Chunk starts: sample_offset of each chunk's first sample. Video
    // chunk k starts at sample 5k, audio chunk k at sample 10k. The
    // merged order must be v0 a0 v1 a1 v2 a2 v3 a3 (equal start times
    // fall back to add_track order — video first).
    let mut starts: Vec<(u64, u8)> = Vec::new();
    for k in 0..4u32 {
        starts.push((d.sample_offset(0, 5 * k).expect("video chunk start"), b'v'));
        starts.push((d.sample_offset(1, 10 * k).expect("audio chunk start"), b'a'));
    }
    let mut by_offset = starts.clone();
    by_offset.sort_by_key(|&(off, _)| off);
    let order: Vec<u8> = by_offset.iter().map(|&(_, t)| t).collect();
    assert_eq!(
        order,
        b"vavavava".to_vec(),
        "chunk starts must alternate video/audio in file order"
    );
}

#[test]
fn packet_emission_switches_tracks_per_chunk() {
    let mut d = open(build_two_track(ChunkStrategy::InterleaveByMovieTicks(300)));
    let packets = drain(&mut d);
    assert_eq!(packets.len(), 60);
    let transitions = packets.windows(2).filter(|w| w[0].0 != w[1].0).count();
    // v(5) a(10) v(5) a(10) v(5) a(10) v(5) a(10) ⇒ 7 switches; the
    // single-chunk layout would show exactly 1.
    assert_eq!(transitions, 7, "expected one switch per chunk boundary");
}

#[test]
fn per_track_bytes_and_timelines_match_single_chunk_layout() {
    let mut interleaved = open(build_two_track(ChunkStrategy::InterleaveByMovieTicks(300)));
    let mut single = open(build_two_track(ChunkStrategy::SingleChunkPerTrack));
    let a = drain(&mut interleaved);
    let b = drain(&mut single);
    for stream in 0..2u32 {
        let ax: Vec<&Vec<u8>> = a.iter().filter(|p| p.0 == stream).map(|p| &p.1).collect();
        let bx: Vec<&Vec<u8>> = b.iter().filter(|p| p.0 == stream).map(|p| &p.1).collect();
        assert_eq!(ax, bx, "stream {stream} bytes must be layout-invariant");
    }
    // Sample tables carry identical timing regardless of layout.
    let di = open(build_two_track(ChunkStrategy::InterleaveByMovieTicks(300)));
    let ds = open(build_two_track(ChunkStrategy::SingleChunkPerTrack));
    for stream in 0..2usize {
        let ti = &di.tracks[stream];
        let ts = &ds.tracks[stream];
        assert_eq!(ti.mdhd.duration, ts.mdhd.duration);
        assert_eq!(
            ti.sample_table.sample_count(),
            ts.sample_table.sample_count()
        );
    }
}

#[test]
fn single_chunk_default_is_unchanged() {
    let m = MovMuxer::new();
    assert_eq!(m.chunk_strategy(), ChunkStrategy::SingleChunkPerTrack);
    let d = open(build_two_track(ChunkStrategy::SingleChunkPerTrack));
    assert_eq!(d.chunk_count(0), Some(1));
    assert_eq!(d.chunk_count(1), Some(1));
}

#[test]
fn zero_period_is_rejected() {
    let mut m = MovMuxer::new().with_chunk_strategy(ChunkStrategy::InterleaveByMovieTicks(0));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 2,
        },
        600,
        video_samples(),
        &[],
    );
    assert!(m.encode_to_vec().is_err(), "period 0 must be rejected");
}

#[test]
fn tiny_period_floors_at_one_sample_per_chunk() {
    // A 1-tick period is far below any sample duration: every sample
    // becomes its own chunk and stsc compresses to a single run.
    let mut m = MovMuxer::new().with_chunk_strategy(ChunkStrategy::InterleaveByMovieTicks(1));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 2,
        },
        600,
        video_samples(),
        &[],
    );
    let d = open(m.encode_to_vec().expect("encode"));
    assert_eq!(d.chunk_count(0), Some(20), "one chunk per sample");
    for c in 1..=20 {
        assert_eq!(d.samples_in_chunk(0, c), Some(1));
    }
}

#[test]
fn interleave_keeps_aux_slab_contiguous_and_addressable() {
    // Attach a per-sample aux stream to the video track and interleave:
    // the slab must stay contiguous (single-entry saio, §8.7.9.3) and
    // land after the chunk data, byte-exact.
    let blobs: Vec<Vec<u8>> = (0..20u8).map(|i| vec![i ^ 0x5A; 7]).collect();
    let mut m = MovMuxer::new().with_chunk_strategy(ChunkStrategy::InterleaveByMovieTicks(300));
    let tid = m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 2,
        },
        600,
        video_samples(),
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
        audio_samples(),
        &[],
    );
    m.set_sample_aux(
        tid,
        oxideav_mov::SampleAuxStream {
            aux_info_type: Some(*b"cenc"),
            aux_info_type_parameter: 0,
            per_sample: blobs.clone(),
        },
    )
    .expect("attach aux stream");
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes.clone());
    let (saiz, saio) = d.sample_aux_info(0, b"cenc", 0);
    let saiz = saiz.expect("saiz present");
    let saio = saio.expect("saio present");
    assert_eq!(saiz.sample_count, 20);
    assert!(saio.is_single_chunk(), "aux slab must stay single-entry");
    let off = saio.offset_for(0).expect("slab offset") as usize;
    let mut expect = Vec::new();
    for b in &blobs {
        expect.extend_from_slice(b);
    }
    assert_eq!(&bytes[off..off + expect.len()], expect.as_slice());
}
