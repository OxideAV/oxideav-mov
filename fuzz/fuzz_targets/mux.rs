#![no_main]

//! Round 429 — structure-aware fuzz of the **write side**: interpret
//! the fuzz bytes as a bounded mux recipe (tracks × samples ×
//! timescales × chunk strategy × moov placement), encode it, and
//! assert the round-trip invariants through our own demuxer.
//!
//! Unlike the `demux` target (whose contract is "malformed input must
//! error, never panic"), every recipe decoded here is *valid by
//! construction*, so the contract is strict:
//!
//!   * `encode_to_vec()` must succeed;
//!   * `MovDemuxer::open` must accept the produced bytes;
//!   * the demuxer must report the same track count, per-track sample
//!     count, and byte-identical per-track payload streams;
//!   * every resolved sample offset must land inside the file;
//!   * `is_faststart()` must equal the requested placement.
//!
//! The arithmetic under fire is the round-429 surface: the interleave
//! planner's period rescale (`period × media_ts / movie_ts`,
//! round-half-up, floor 1) across hostile timescale ratios, the
//! saturating decode-time accumulation, the k-way chunk merge, and
//! the faststart fixed-point `moov` sizing loop — plus the long-
//! standing stsc/stco run-length emission now that chunk counts are
//! arbitrary.

use libfuzzer_sys::fuzz_target;

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{ChunkStrategy, MoovPlacement, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

/// Bounded byte reader over the fuzz input.
struct Recipe<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Recipe<'a> {
    fn u8(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }
    fn u16(&mut self) -> u16 {
        u16::from_be_bytes([self.u8(), self.u8()])
    }
}

fuzz_target!(|data: &[u8]| {
    let mut r = Recipe { data, pos: 0 };

    // ── Decode a valid-by-construction mux recipe. ──
    let movie_ts = r.u16().max(1) as u32;
    let placement = if r.u8() & 1 == 0 {
        MoovPlacement::AfterMdat
    } else {
        MoovPlacement::BeforeMdat
    };
    let strategy = match r.u8() % 3 {
        0 => ChunkStrategy::SingleChunkPerTrack,
        1 => ChunkStrategy::InterleaveByMovieTicks(r.u16().max(1) as u32),
        _ => ChunkStrategy::InterleaveByMovieTicks(1), // one-sample floor
    };
    let n_tracks = 1 + (r.u8() % 3) as usize;

    let mut m = MovMuxer::new()
        .with_movie_timescale(movie_ts)
        .with_moov_placement(placement)
        .with_chunk_strategy(strategy);

    let mut inputs: Vec<Vec<Vec<u8>>> = Vec::with_capacity(n_tracks);
    for t in 0..n_tracks {
        let media_ts = r.u16().max(1) as u32;
        let n_samples = 1 + (r.u8() % 48) as usize;
        let is_video = r.u8() & 1 == 0;
        let mut samples = Vec::with_capacity(n_samples);
        let mut bytes = Vec::with_capacity(n_samples);
        for i in 0..n_samples {
            let dur = r.u16().max(1) as u32;
            let len = (r.u8() % 32) as usize + 1;
            let fill = r.u8();
            let offset = if is_video {
                // Signed composition offsets exercise the v1 ctts path.
                (r.u8() as i32 - 128) * dur.min(1 << 16) as i32
            } else {
                0
            };
            let data = vec![fill ^ (i as u8), 0x42, t as u8]
                .into_iter()
                .cycle()
                .take(len)
                .collect::<Vec<u8>>();
            bytes.push(data.clone());
            samples.push(MuxSample {
                data,
                duration: dur,
                keyframe: !is_video || i == 0 || r.u8() & 3 == 0,
                composition_offset: offset,
            });
        }
        inputs.push(bytes);
        if is_video {
            m.add_track(
                MuxTrackKind::Video {
                    format: *b"raw ",
                    width: 4,
                    height: 4,
                },
                media_ts,
                samples,
                &[],
            );
        } else {
            m.add_track(
                MuxTrackKind::Audio {
                    format: *b"twos",
                    channels: 1,
                    bits_per_sample: 16,
                    sample_rate: media_ts,
                },
                media_ts,
                samples,
                &[],
            );
        }
    }

    // ── Encode: every decoded recipe is valid, failure is a bug. ──
    let file = m.encode_to_vec().expect("valid recipe must encode");

    // ── Round-trip through our demuxer. ──
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file.clone()));
    let mut d = MovDemuxer::open(cur).expect("muxer output must demux");
    assert_eq!(d.tracks.len(), n_tracks, "track count");
    assert_eq!(
        d.is_faststart(),
        placement == MoovPlacement::BeforeMdat,
        "placement classification"
    );

    // Offsets resolve in-bounds for every sample of every track.
    for (ti, track_bytes) in inputs.iter().enumerate() {
        for (si, sample) in track_bytes.iter().enumerate() {
            let off = d
                .sample_offset(ti, si as u32)
                .expect("sample offset resolves") as usize;
            assert!(
                off + sample.len() <= file.len(),
                "sample extent inside the file"
            );
            assert_eq!(
                &file[off..off + sample.len()],
                sample.as_slice(),
                "bytes at resolved offset"
            );
        }
    }

    // Per-track payload streams are byte-identical, in order.
    let mut got: Vec<Vec<Vec<u8>>> = vec![Vec::new(); n_tracks];
    while let Ok(p) = d.next_packet() {
        got[p.stream_index as usize].push(p.data);
    }
    for (ti, (a, b)) in inputs.iter().zip(got.iter()).enumerate() {
        assert_eq!(a, b, "track {ti} payload stream");
    }
});
