//! Sound tracks whose sample table counts **audio frames** (QTFF
//! 2012-08-14 pp. 179 – 180): uncompressed sound is one table sample
//! per frame, and fixed-ratio compressed sound not flagged with
//! Compression ID `-2` has tables that "appear to point to individual
//! uncompressed audio samples, each one byte in size" which the
//! version-1 fields convert into bytes.
//!
//! * the demuxer reads such a track chunk by chunk in bounded packets,
//!   seeks to the exact frame, and never queues one entry per frame;
//! * the registry muxer writes uncompressed sound that way, whatever
//!   the packet sizes it is fed;
//! * a black-box reader (`ffprobe` / `ffmpeg`, when installed) counts
//!   the same frames and decodes the same samples.

#![cfg(feature = "registry")]

mod common;

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};

use common::*;
use oxideav_core::{
    ChannelLayout, CodecId, CodecParameters, Demuxer, Error, Packet, ReadSeek, StreamInfo,
    TimeBase, WriteSeek,
};
use oxideav_mov::{FramePacketing, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for SharedBuf {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open")
}

fn drain(d: &mut MovDemuxer) -> Vec<Packet> {
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => out.push(p),
            Err(Error::Eof) => return out,
            Err(e) => panic!("{e}"),
        }
    }
}

/// Deterministic payload bytes.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn audio_stream(codec: &str, channels: u16, rate: u32) -> StreamInfo {
    let mut p = CodecParameters::audio(CodecId::new(codec)).channels(channels);
    p.sample_rate = Some(rate);
    if channels == 6 {
        p = p.channel_layout(ChannelLayout::Surround51);
    }
    StreamInfo {
        index: 0,
        time_base: TimeBase::new(1, rate as i64),
        duration: None,
        start_time: None,
        params: p,
    }
}

/// Mux `payload` through the registry muxer in packets of
/// `packet_bytes` bytes (the last one shorter).
fn mux_sound(stream: &StreamInfo, payload: &[u8], packet_bytes: usize) -> Vec<u8> {
    let buf = SharedBuf::default();
    let out: Box<dyn WriteSeek> = Box::new(buf.clone());
    let mut m = oxideav_mov::registry::open_muxer(out, std::slice::from_ref(stream)).unwrap();
    m.write_header().unwrap();
    for chunk in payload.chunks(packet_bytes) {
        m.write_packet(&Packet::new(0, stream.time_base, chunk.to_vec()))
            .unwrap();
    }
    m.write_trailer().unwrap();
    buf.0.lock().unwrap().get_ref().clone()
}

fn concat(packets: &[Packet]) -> Vec<u8> {
    packets
        .iter()
        .flat_map(|p| p.data.iter().copied())
        .collect()
}

#[test]
fn uncompressed_sound_is_read_in_bounded_packets() {
    let stream = audio_stream("pcm_s16le", 2, 48000);
    let payload = noise(48000 * 4, 7);
    // Packets that split frames: 4099 bytes is not a multiple of 4.
    let file = mux_sound(&stream, &payload, 4099);
    let mut d = open(file);

    let table = &d.tracks[0].sample_table;
    assert_eq!(table.stsz_default_size, Some(4), "one frame per sample");
    assert_eq!(table.sample_count(), 48000);
    assert!(table.is_frame_counted());
    assert_eq!(
        d.sound_packeting(0),
        Some(FramePacketing::new(1, 4, 1024)),
        "default packeting"
    );

    let packets = drain(&mut d);
    assert_eq!(concat(&packets), payload);
    let mut dts = 0i64;
    for p in &packets {
        assert_eq!(p.dts, Some(dts));
        assert_eq!(p.pts, Some(dts));
        assert!(p.flags.keyframe);
        let frames = p.data.len() as i64 / 4;
        assert!(frames <= 1024 && frames > 0);
        assert_eq!(p.duration, Some(frames));
        dts += frames;
    }
    assert_eq!(dts, 48000);
    assert!(packets.len() >= 47, "at most 1024 frames per packet");

    // One frame per packet on request, and wider packets.
    d.set_sound_frames_per_packet(1).unwrap();
    let single = drain(&mut d);
    assert_eq!(single.len(), 48000);
    assert!(single.iter().all(|p| p.data.len() == 4));
    assert_eq!(concat(&single), payload);
    d.set_sound_frames_per_packet(1 << 20).unwrap();
    assert_eq!(concat(&drain(&mut d)), payload);
}

#[test]
fn seek_lands_on_the_exact_sound_frame() {
    let stream = audio_stream("pcm_s24be", 2, 44100);
    let payload = noise(44100 * 6, 11);
    let mut d = open(mux_sound(&stream, &payload, 6 * 1000));
    for target in [0i64, 1, 1023, 1024, 12345, 44099] {
        assert_eq!(d.seek_to(0, target).unwrap(), target);
        let p = d.next_packet().unwrap();
        assert_eq!(p.dts, Some(target));
        let at = target as usize * 6;
        assert_eq!(&p.data[..], &payload[at..at + p.data.len()]);
        assert_eq!(p.duration, Some(p.data.len() as i64 / 6));
        // The stream carries on, gapless, to its end.
        let mut rest = p.data.clone();
        rest.extend(concat(&drain(&mut d)));
        assert_eq!(rest, &payload[at..]);
    }
    // Past the end: the last frame.
    assert_eq!(d.seek_to(0, 1 << 40).unwrap(), 44099);
    assert_eq!(d.next_packet().unwrap().data, &payload[44099 * 6..]);
}

/// A movie with one sound track described by `stsd`, `stsz` constant
/// size 1 and `count` one-tick samples, laid out in the given chunks
/// (`(samples, bytes)` each).
fn one_byte_table_movie(stsd: Vec<u8>, count: u32, chunks: &[(u32, usize)], rate: u32) -> Vec<u8> {
    let mut file = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut file, *b"ftyp", &ftyp);
    let total: usize = chunks.iter().map(|c| c.1).sum();
    let mdat_at = file.len() as u32 + 8;
    push_atom(&mut file, *b"mdat", &noise(total, 3));

    let mut stsc = Vec::new();
    stsc.extend_from_slice(&0u32.to_be_bytes());
    stsc.extend_from_slice(&(chunks.len() as u32).to_be_bytes());
    let mut stco = Vec::new();
    stco.extend_from_slice(&0u32.to_be_bytes());
    stco.extend_from_slice(&(chunks.len() as u32).to_be_bytes());
    let mut at = mdat_at;
    for (i, (samples, bytes)) in chunks.iter().enumerate() {
        stsc.extend_from_slice(&(i as u32 + 1).to_be_bytes());
        stsc.extend_from_slice(&samples.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stco.extend_from_slice(&at.to_be_bytes());
        at += *bytes as u32;
    }

    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &stsd);
    push_atom(&mut stbl, *b"stts", &build_stts_single(count, 1));
    push_atom(&mut stbl, *b"stsc", &stsc);
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(1, count));
    push_atom(&mut stbl, *b"stco", &stco);
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"smhd", &[0u8; 8]);
    push_atom(&mut minf, *b"stbl", &stbl);
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(rate, count));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
    push_atom(&mut mdia, *b"minf", &minf);
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, count, 0, 0));
    push_atom(&mut trak, *b"mdia", &mdia);
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(rate, count));
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut file, *b"moov", &moov);
    file
}

/// A version-1 sound description `stsd` (QTFF 2012-08-14 p. 179).
fn stsd_v1(format: &[u8; 4], channels: u16, compression_id: i16, v1: [u32; 4]) -> Vec<u8> {
    let mut extras = Vec::new();
    for f in v1 {
        extras.extend_from_slice(&f.to_be_bytes());
    }
    let mut stsd = build_stsd_audio(format, channels, 16, 22050, &extras);
    // Version sits at the head of the 20-byte sound body, the
    // compression ID 12 bytes in (body starts 24 bytes into the stsd).
    stsd[24..26].copy_from_slice(&1u16.to_be_bytes());
    stsd[36..38].copy_from_slice(&compression_id.to_be_bytes());
    stsd
}

fn mdat_payload(file: &[u8]) -> &[u8] {
    let at = file.windows(4).position(|w| w == b"mdat").unwrap();
    let size = u32::from_be_bytes(file[at - 4..at].try_into().unwrap()) as usize;
    &file[at + 4..at - 4 + size]
}

#[test]
fn fixed_ratio_compressed_sound_is_cut_at_whole_compressed_frames() {
    // 64 samples per 34-byte packet per channel, two channels: five
    // 68-byte compressed frames in two chunks (2 + 3 frames).
    let stsd = stsd_v1(b"ima4", 2, -1, [64, 34, 68, 2]);
    let file = one_byte_table_movie(stsd, 320, &[(128, 136), (192, 204)], 22050);
    let payload = mdat_payload(&file).to_vec();
    let mut d = open(file);
    assert_eq!(d.sound_packeting(0), Some(FramePacketing::new(64, 68, 64)));
    let packets = drain(&mut d);
    assert_eq!(packets.len(), 5);
    for (i, p) in packets.iter().enumerate() {
        assert_eq!(p.data.len(), 68);
        assert_eq!(p.dts, Some(i as i64 * 64));
        assert_eq!(p.duration, Some(64));
    }
    assert_eq!(concat(&packets), payload);

    // A seek inside a compressed frame starts at that frame.
    assert_eq!(d.seek_to(0, 64 * 3 + 17).unwrap(), 64 * 3);
    assert_eq!(d.next_packet().unwrap().data, &payload[3 * 68..4 * 68]);

    // `bytesPerFrame` left at zero: bytesPerPacket × channels.
    let stsd = stsd_v1(b"ima4", 2, -1, [64, 34, 0, 2]);
    let d = open(one_byte_table_movie(stsd, 320, &[(320, 340)], 22050));
    assert_eq!(d.sound_packeting(0), Some(FramePacketing::new(64, 68, 64)));
}

#[test]
fn one_byte_sample_tables_of_uncompressed_sound_take_the_frame_size() {
    // Version 0, 16-bit stereo, table samples one byte each: 300
    // frames of 4 bytes.
    let stsd = build_stsd_audio(b"twos", 2, 16, 22050, &[]);
    let file = one_byte_table_movie(stsd, 300, &[(100, 400), (200, 800)], 22050);
    let payload = mdat_payload(&file).to_vec();
    let mut d = open(file);
    assert_eq!(d.sound_packeting(0), Some(FramePacketing::new(1, 4, 1024)));
    let packets = drain(&mut d);
    assert_eq!(packets.len(), 2, "one packet per chunk");
    assert_eq!(concat(&packets), payload);
    assert_eq!(packets[1].dts, Some(100));
    assert_eq!(packets[1].duration, Some(200));

    // Version 1 uncompressed: one sample per packet, six bytes a frame.
    let stsd = stsd_v1(b"in24", 2, 0, [1, 3, 6, 2]);
    let file = one_byte_table_movie(stsd, 50, &[(50, 300)], 22050);
    let payload = mdat_payload(&file).to_vec();
    let mut d = open(file);
    assert_eq!(d.sound_packeting(0), Some(FramePacketing::new(1, 6, 1024)));
    assert_eq!(concat(&drain(&mut d)), payload);
}

#[test]
fn redefined_sample_tables_keep_one_frame_per_sample() {
    // Compression ID -2: each table sample is a compressed frame,
    // whatever its constant size and duration.
    let stsd = stsd_v1(b"ima4", 1, -2, [64, 0, 0, 2]);
    let d = open(one_byte_table_movie(stsd, 10, &[(10, 10)], 22050));
    assert_eq!(d.sound_packeting(0), None);
    assert_eq!(d.remaining(), 10);
}

#[test]
fn forged_frame_counts_stay_bounded() {
    // Four billion one-tick samples in one chunk of a kilobyte file.
    let stsd = build_stsd_audio(b"raw ", 1, 8, 22050, &[]);
    let file = one_byte_table_movie(stsd, u32::MAX, &[(u32::MAX, 16)], 22050);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    assert!(MovDemuxer::open(cur).is_err());

    // A count the queue can hold but the file cannot back: the open
    // succeeds without touching the bytes and reads fail cleanly.
    let stsd = build_stsd_audio(b"raw ", 1, 8, 22050, &[]);
    let mut d = open(one_byte_table_movie(stsd, 1 << 28, &[(1 << 28, 16)], 22050));
    assert_eq!(d.remaining(), 1 << 18);
    assert!(d.next_packet().is_err());
}

#[test]
fn direct_muxer_writes_frame_counted_sound() {
    let payload = noise(3 * 1000, 5);
    let samples: Vec<MuxSample> = payload
        .chunks(3 * 250)
        .map(|c| {
            MuxSample::default()
                .with_data(c.to_vec())
                .with_duration(250)
                .with_keyframe(true)
        })
        .collect();
    let kind = MuxTrackKind::Audio {
        format: *b"ulaw",
        channels: 3,
        bits_per_sample: 16,
        sample_rate: 8000,
    };
    let mut m = MovMuxer::new();
    let id = m.add_track(kind.clone(), 8000, samples.clone(), &[]);
    m.set_sound_frame_size(id, 3).unwrap();
    let mut d = open(m.encode_to_vec().unwrap());
    assert_eq!(d.tracks[0].sample_table.sample_count(), 1000);
    assert_eq!(d.tracks[0].mdhd.duration, 1000);
    assert_eq!(concat(&drain(&mut d)), payload);

    // Refusals: partial frames, durations that are not frame counts,
    // and the fragmented path.
    let mut m = MovMuxer::new();
    let id = m.add_track(kind.clone(), 8000, samples.clone(), &[]);
    assert!(m.set_sound_frame_size(id, 4).is_err());
    assert!(m.set_sound_frame_size(id, 0).is_err());
    assert!(m.set_sound_frame_size(id, 1).is_err());
    let mut m = MovMuxer::new().with_fragmentation(oxideav_mov::FragmentationMode::ByFrameCount(2));
    let id = m.add_track(kind, 8000, samples, &[]);
    m.set_sound_frame_size(id, 3).unwrap();
    assert!(m.encode_fragmented_to_vec().is_err());
}

// ─────────────── black-box reader ───────────────

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "oxideav-mov-sound-{}-{name}.mov",
        std::process::id()
    ));
    std::fs::write(&path, bytes).unwrap();
    path
}

fn ffprobe_stream(path: &std::path::Path) -> std::collections::HashMap<String, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe rejected {path:?}");
    assert!(
        out.stderr.is_empty(),
        "ffprobe complained about {path:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Every uncompressed flavour the registry muxer writes is read back
/// by a black-box reader with the same codec, rate, channel count and
/// frame count, and decodes to the very bytes that went in.
#[test]
fn black_box_reader_decodes_what_the_muxer_wrote() {
    if !have("ffprobe") || !have("ffmpeg") {
        eprintln!("skip: ffprobe / ffmpeg not installed");
        return;
    }
    // (codec id, bytes per sample, raw output format)
    let rows: &[(&str, usize, &str)] = &[
        ("pcm_u8", 1, "u8"),
        ("pcm_s8", 1, "s8"),
        ("pcm_s16be", 2, "s16be"),
        ("pcm_s16le", 2, "s16le"),
        ("pcm_s24be", 3, "s24be"),
        ("pcm_s24le", 3, "s24le"),
        ("pcm_s32be", 4, "s32be"),
        ("pcm_s32le", 4, "s32le"),
        ("pcm_f32be", 4, "f32be"),
        ("pcm_f32le", 4, "f32le"),
        ("pcm_f64be", 8, "f64be"),
        ("pcm_f64le", 8, "f64le"),
        ("pcm_mulaw", 1, "mulaw"),
        ("pcm_alaw", 1, "alaw"),
    ];
    for &(codec, width, raw) in rows {
        for (channels, rate) in [(1u16, 8000u32), (2, 44100), (6, 48000), (2, 96000)] {
            let frames = 3000usize;
            let frame = width * channels as usize;
            // Finite floats: whole-number samples in range.
            let payload: Vec<u8> = match raw {
                "f32be" | "f32le" | "f64be" | "f64le" => (0..frames * channels as usize)
                    .flat_map(|i| {
                        let v = ((i * 37 % 200) as f64 - 100.0) / 128.0;
                        match raw {
                            "f32be" => (v as f32).to_be_bytes().to_vec(),
                            "f32le" => (v as f32).to_le_bytes().to_vec(),
                            "f64be" => v.to_be_bytes().to_vec(),
                            _ => v.to_le_bytes().to_vec(),
                        }
                    })
                    .collect(),
                _ => noise(frames * frame, 99),
            };
            let stream = audio_stream(codec, channels, rate);
            let file = mux_sound(&stream, &payload, frame * 777 + 1);
            let name = format!("{codec}-{channels}-{rate}");
            let path = temp_file(&name, &file);
            let s = ffprobe_stream(&path);
            assert_eq!(s["codec_name"], codec, "{name}");
            assert_eq!(s["sample_rate"], rate.to_string(), "{name}");
            assert_eq!(s["channels"], channels.to_string(), "{name}");
            assert_eq!(s["nb_frames"], frames.to_string(), "{name}");
            assert_eq!(s["duration_ts"], frames.to_string(), "{name}");
            let out = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&path)
                .args(["-c:a", "copy", "-f", raw, "-"])
                .output()
                .expect("run ffmpeg");
            assert!(out.status.success(), "{name}");
            assert_eq!(out.stdout, payload, "{name}: decoded bytes");
            std::fs::remove_file(&path).ok();

            // And our own reader agrees.
            let mut d = open(file);
            assert_eq!(d.streams()[0].params.codec_id.as_str(), codec, "{name}");
            assert_eq!(d.streams()[0].params.sample_rate, Some(rate), "{name}");
            assert_eq!(d.streams()[0].params.resolved_channels(), Some(channels));
            assert_eq!(concat(&drain(&mut d)), payload, "{name}");
        }
    }
}

/// The one-byte sample tables of fixed-ratio compressed sound: a
/// black-box reader cuts the same packets.
#[test]
fn black_box_reader_cuts_the_same_compressed_frames() {
    if !have("ffprobe") {
        eprintln!("skip: ffprobe not installed");
        return;
    }
    let stsd = stsd_v1(b"ima4", 2, -1, [64, 34, 68, 2]);
    let file = one_byte_table_movie(stsd, 320, &[(128, 136), (192, 204)], 22050);
    let path = temp_file("ima4-one-byte", &file);
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=pos,size,dts,duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .expect("run ffprobe");
    std::fs::remove_file(&path).ok();
    assert!(out.status.success());
    let theirs: u64 = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split(',').nth(2)?.parse::<u64>().ok())
        .sum();
    let mut d = open(file);
    let ours: u64 = drain(&mut d).iter().map(|p| p.data.len() as u64).sum();
    assert_eq!(ours, 340);
    assert_eq!(theirs, ours, "bytes of sound data read from the chunks");
}
