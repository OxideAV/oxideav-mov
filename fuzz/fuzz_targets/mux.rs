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

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use oxideav_core::{CodecId, CodecParameters, Demuxer, Packet, ReadSeek, StreamInfo, TimeBase};
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
    // The same recipe as registry streams + packets, for the
    // streaming writer below.
    let mut streams: Vec<StreamInfo> = Vec::with_capacity(n_tracks);
    let mut packets: Vec<Vec<Packet>> = Vec::with_capacity(n_tracks);
    for t in 0..n_tracks {
        let media_ts = r.u16().max(1) as u32;
        let n_samples = 1 + (r.u8() % 48) as usize;
        let is_video = r.u8() & 1 == 0;
        let mut samples = Vec::with_capacity(n_samples);
        let mut bytes = Vec::with_capacity(n_samples);
        let time_base = TimeBase::new(1, media_ts as i64);
        let mut track_packets = Vec::with_capacity(n_samples);
        let mut dts = 0i64;
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
            track_packets.push(
                Packet::new(t as u32, time_base, data.clone())
                    .with_dts(dts)
                    .with_pts(dts + offset as i64)
                    .with_duration(dur as i64)
                    .with_keyframe(!is_video || i == 0),
            );
            dts += dur as i64;
            samples.push(
                MuxSample::default()
                    .with_data(data)
                    .with_duration(dur)
                    .with_keyframe(!is_video || i == 0 || r.u8() & 3 == 0)
                    .with_composition_offset(offset),
            );
        }
        inputs.push(bytes);
        packets.push(track_packets);
        let params = if is_video {
            let mut v = CodecParameters::video(CodecId::new("rawvideo"));
            v.width = Some(4);
            v.height = Some(4);
            v
        } else {
            let mut a = CodecParameters::audio(CodecId::new("pcm_u8")).channels(1);
            a.sample_rate = Some(media_ts);
            a
        };
        streams.push(StreamInfo {
            index: t as u32,
            time_base,
            duration: None,
            start_time: None,
            params,
        });
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
                // 8-bit mono: a one-byte sample is a whole frame, so a
                // recipe of constant-size one-tick samples is a
                // well-formed frame-counted sound table.
                MuxTrackKind::Audio {
                    format: *b"raw ",
                    channels: 1,
                    bits_per_sample: 8,
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

    // Per-track payload streams are byte-identical, in order. A
    // frame-counted sound table is read one table sample per packet
    // here so packets pair with the recipe's samples.
    d.set_sound_frames_per_packet(1)
        .expect("requeue one frame per packet");
    // Per-track payload streams are byte-identical, in order.
    let mut got: Vec<Vec<Vec<u8>>> = vec![Vec::new(); n_tracks];
    while let Ok(p) = d.next_packet() {
        got[p.stream_index as usize].push(p.data);
    }
    for (ti, (a, b)) in inputs.iter().zip(got.iter()).enumerate() {
        assert_eq!(a, b, "track {ti} payload stream");
    }

    // ── The streaming registry writer over the same recipe: packets
    //    interleaved round-robin, payloads written as they arrive, the
    //    sound track frame-counted, edit lists derived from the
    //    timestamps. Every stream's bytes must come back in order. ──
    let sink = Sink::default();
    let mut mux = oxideav_mov::registry::open_muxer(Box::new(sink.clone()), &streams)
        .expect("registry muxer opens");
    mux.write_header().expect("header");
    let longest = packets.iter().map(Vec::len).max().unwrap_or(0);
    for i in 0..longest {
        for track in &packets {
            if let Some(p) = track.get(i) {
                mux.write_packet(p).expect("packet");
            }
        }
    }
    mux.write_trailer().expect("trailer");
    let written = sink.0.lock().unwrap().get_ref().clone();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(written));
    let mut d = MovDemuxer::open(cur).expect("streamed output must demux");
    assert_eq!(d.tracks.len(), n_tracks, "streamed track count");
    let mut streamed: Vec<Vec<u8>> = vec![Vec::new(); n_tracks];
    while let Ok(p) = d.next_packet() {
        streamed[p.stream_index as usize].extend_from_slice(&p.data);
    }
    for (ti, (a, b)) in inputs.iter().zip(&streamed).enumerate() {
        assert_eq!(&a.concat(), b, "track {ti} streamed payload");
    }
    // The derived edit lists must hold up under the applied mode.
    d.apply_edit_lists(true);
    d.emit_never_presented(true);
    for ti in 0..n_tracks {
        let _ = d.seek_to(ti as u32, 0);
        while d.next_packet().is_ok() {}
    }
});

/// A seekable in-memory sink whose bytes stay reachable after the
/// boxed muxer takes ownership of the writer.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Cursor<Vec<u8>>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for Sink {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}
