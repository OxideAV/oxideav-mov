//! Round 429 — **registry `Muxer` integration** (dual-API): the `mov`
//! container is now writable through
//! `oxideav_core::ContainerRegistry::open_muxer` / the uniform
//! `register(&mut RuntimeContext)` entry point, alongside the direct
//! `MovMuxer` API.
//!
//! The adapter buffers packets (QTFF's non-fragmented layout needs the
//! complete sample tables before finalization) and encodes everything
//! at `write_trailer`. These tests drive the full demux → registry-mux
//! → demux loop and pin the state machine, the codec-tag mapping, and
//! duration/composition-offset recovery from packet timestamps.

#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use oxideav_core::{
    CodecId, CodecParameters, Demuxer, Packet, ReadSeek, StreamInfo, TimeBase, WriteSeek,
};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

/// A `WriteSeek` sink whose bytes stay reachable after the boxed muxer
/// takes ownership of the writer.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

impl SharedBuf {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().get_ref().clone()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

impl Seek for SharedBuf {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn registry() -> oxideav_core::RuntimeContext {
    let mut ctx = oxideav_core::RuntimeContext::new();
    oxideav_mov::registry::register(&mut ctx);
    ctx
}

/// Source movie: video track with B-frame-style composition offsets +
/// audio track, both with distinct payload bytes.
fn source_movie() -> Vec<u8> {
    let mut m = MovMuxer::new();
    // Decode order I P B B: ctts offsets displace pts from dts.
    let offsets = [200i32, 400, 0, 100, 200, 400, 0, 100];
    let video: Vec<MuxSample> = (0..8)
        .map(|i| MuxSample {
            data: vec![0x50 + i as u8; 40],
            duration: 200,
            keyframe: i == 0 || i == 4,
            composition_offset: offsets[i],
        })
        .collect();
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 8,
            height: 8,
        },
        2400,
        video,
        &[],
    );
    let audio: Vec<MuxSample> = (0..10)
        .map(|i| MuxSample {
            data: vec![0xC0u8.wrapping_add(i as u8); 13],
            duration: 512,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        audio,
        &[],
    );
    m.encode_to_vec().expect("encode source movie")
}

/// One drained packet: (stream, data, pts, dts).
type Drained = (u32, Vec<u8>, Option<i64>, Option<i64>);

fn drain(d: &mut MovDemuxer) -> Vec<Drained> {
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push((p.stream_index, p.data, p.pts, p.dts));
    }
    out
}

#[test]
fn demux_registry_mux_demux_round_trip() {
    let src = source_movie();
    let mut d = open(src);
    let streams: Vec<StreamInfo> = d.streams().to_vec();
    assert_eq!(streams.len(), 2);

    let ctx = registry();
    let sink = SharedBuf::default();
    let out: Box<dyn WriteSeek> = Box::new(sink.clone());
    let mut mux = ctx
        .containers
        .open_muxer("mov", out, &streams)
        .expect("open registry muxer");
    assert_eq!(mux.format_name(), "mov");
    mux.write_header().expect("header");
    let original = drain(&mut d);
    for (stream_index, data, pts, dts) in &original {
        let mut p = Packet::new(
            *stream_index,
            streams[*stream_index as usize].time_base,
            data.clone(),
        );
        p.pts = *pts;
        p.dts = *dts;
        p.flags.keyframe = true;
        mux.write_packet(&p).expect("packet");
    }
    mux.write_trailer().expect("trailer");

    // Re-demux the remuxed bytes: same streams, same per-track bytes,
    // same pts/dts sequences (durations were recovered from dts deltas
    // since the packets above carried none).
    let remuxed = sink.bytes();
    let mut d2 = open(remuxed);
    assert_eq!(d2.streams().len(), 2);
    let got = drain(&mut d2);
    for stream in 0..2u32 {
        let a: Vec<_> = original.iter().filter(|p| p.0 == stream).collect();
        let b: Vec<_> = got.iter().filter(|p| p.0 == stream).collect();
        assert_eq!(a.len(), b.len(), "stream {stream} packet count");
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.1, y.1, "stream {stream} payload bytes");
            assert_eq!(x.2, y.2, "stream {stream} pts");
            assert_eq!(x.3, y.3, "stream {stream} dts (ctts recovered)");
        }
    }
    // The remux carries the on-wire formats forward via the codec tag
    // (lowercased back to the QTFF spelling).
    let fmts: Vec<[u8; 4]> = d2
        .tracks
        .iter()
        .map(|t| t.primary_format().expect("format"))
        .collect();
    assert_eq!(fmts, vec![*b"raw ", *b"twos"]);
}

#[test]
fn state_machine_is_enforced() {
    let src = source_movie();
    let d = open(src);
    let streams: Vec<StreamInfo> = d.streams().to_vec();
    let ctx = registry();

    let sink = SharedBuf::default();
    let mut mux = ctx
        .containers
        .open_muxer("mov", Box::new(sink.clone()), &streams)
        .expect("open registry muxer");
    // Packet before header: rejected.
    let p = Packet::new(0, streams[0].time_base, vec![1, 2, 3]);
    assert!(mux.write_packet(&p).is_err(), "packet before header");
    // Trailer before header: rejected.
    assert!(mux.write_trailer().is_err(), "trailer before header");
    mux.write_header().expect("header");
    assert!(mux.write_header().is_err(), "double header");
    // Trailer with a stream that got no packets: rejected.
    mux.write_packet(&p).expect("packet for stream 0");
    assert!(mux.write_trailer().is_err(), "stream 1 received no packets");
}

#[test]
fn unknown_codec_without_tag_is_rejected_at_open() {
    let params = CodecParameters::video(CodecId::new("no-such-codec"));
    let streams = [StreamInfo {
        index: 0,
        time_base: TimeBase::new(1, 600),
        duration: None,
        start_time: None,
        params,
    }];
    let ctx = registry();
    let sink = SharedBuf::default();
    let err = ctx.containers.open_muxer("mov", Box::new(sink), &streams);
    assert!(err.is_err(), "unmappable codec identity must be rejected");
}

#[test]
fn missing_durations_are_recovered_from_dts_deltas() {
    // Hand-built packets with dts gaps 100,100,300 and no durations:
    // the trailer pass must derive stts runs (100,100,300,300) — the
    // last sample reuses the previous duration.
    let params = CodecParameters::video(CodecId::new("rawvideo"));
    let streams = [StreamInfo {
        index: 0,
        time_base: TimeBase::new(1, 600),
        duration: None,
        start_time: None,
        params,
    }];
    let ctx = registry();
    let sink = SharedBuf::default();
    let mut mux = ctx
        .containers
        .open_muxer("mov", Box::new(sink.clone()), &streams)
        .expect("open");
    mux.write_header().expect("header");
    for (i, dts) in [0i64, 100, 200, 500].iter().enumerate() {
        let mut p = Packet::new(0, streams[0].time_base, vec![i as u8; 16]);
        p.dts = Some(*dts);
        p.pts = Some(*dts);
        p.flags.keyframe = true;
        mux.write_packet(&p).expect("packet");
    }
    mux.write_trailer().expect("trailer");

    let mut d = open(sink.bytes());
    let got = drain(&mut d);
    let pts: Vec<i64> = got.iter().map(|p| p.2.expect("pts")).collect();
    assert_eq!(pts, vec![0, 100, 200, 500]);
    // Track duration reflects the recovered final duration (300).
    assert_eq!(d.tracks[0].mdhd.duration, 800);
}
