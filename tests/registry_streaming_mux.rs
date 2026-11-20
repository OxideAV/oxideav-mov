//! The registry muxer writes payloads as packets arrive: `ftyp`, a
//! `wide` placeholder, `mdat` (media interleaved in arrival order),
//! then `moov`. The `mdat` size is patched at the trailer and widens
//! over the `wide` atom past 4 GiB (QTFF 2012-08-14 p. 22), with
//! 64-bit chunk offsets.

#![cfg(feature = "registry")]

use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use oxideav_core::{
    CodecId, CodecParameters, Demuxer, Error, Packet, ReadSeek, StreamInfo, TimeBase, WriteSeek,
};
use oxideav_mov::MovDemuxer;

/// A seekable sink that keeps small writes and remembers only the
/// extent of large ones (read back as zeros).
#[derive(Clone, Default)]
struct Sparse(Arc<Mutex<SparseInner>>);

#[derive(Default)]
struct SparseInner {
    pos: u64,
    len: u64,
    kept: Vec<(u64, Vec<u8>)>,
    writes: usize,
}

const KEEP_BELOW: usize = 1 << 20;

impl Write for Sparse {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut s = self.0.lock().unwrap();
        let pos = s.pos;
        if buf.len() < KEEP_BELOW {
            s.kept.push((pos, buf.to_vec()));
        }
        s.pos += buf.len() as u64;
        s.len = s.len.max(s.pos);
        s.writes += 1;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for Sparse {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let mut s = self.0.lock().unwrap();
        s.pos = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::End(d) => (s.len as i64 + d) as u64,
            SeekFrom::Current(d) => (s.pos as i64 + d) as u64,
        };
        Ok(s.pos)
    }
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut s = self.0.lock().unwrap();
        let n = (buf.len() as u64).min(s.len.saturating_sub(s.pos)) as usize;
        let (start, end) = (s.pos, s.pos + n as u64);
        buf[..n].fill(0);
        // Later writes win, as on a file.
        for (at, bytes) in &s.kept {
            let lo = start.max(*at);
            let hi = end.min(at + bytes.len() as u64);
            if lo < hi {
                buf[(lo - start) as usize..(hi - start) as usize]
                    .copy_from_slice(&bytes[(lo - at) as usize..(hi - at) as usize]);
            }
        }
        s.pos = end;
        Ok(n)
    }
}

fn stream(index: u32, params: CodecParameters, den: i64) -> StreamInfo {
    StreamInfo {
        index,
        time_base: TimeBase::new(1, den),
        duration: None,
        start_time: None,
        params,
    }
}

fn video() -> CodecParameters {
    let mut v = CodecParameters::video(CodecId::new("rawvideo"));
    v.width = Some(16);
    v.height = Some(16);
    v
}

fn pcm() -> CodecParameters {
    let mut a = CodecParameters::audio(CodecId::new("pcm_s16le")).channels(2);
    a.sample_rate = Some(48000);
    a
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

fn atoms(file: &[u8]) -> Vec<([u8; 4], u64, u64)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= file.len() {
        let mut size = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = file[at + 4..at + 8].try_into().unwrap();
        if size == 1 {
            size = u64::from_be_bytes(file[at + 8..at + 16].try_into().unwrap());
        }
        out.push((kind, at as u64, size));
        at += size as usize;
    }
    out
}

#[test]
fn payloads_are_written_as_packets_arrive() {
    let streams = [stream(0, video(), 25), stream(1, pcm(), 48000)];
    let sink = Sparse::default();
    let out: Box<dyn WriteSeek> = Box::new(sink.clone());
    let mut m = oxideav_mov::registry::open_muxer(out, &streams).unwrap();
    m.write_header().unwrap();

    // One second: a video frame, then its 1920 sound frames, 25 times.
    let mut video_bytes = Vec::new();
    let mut sound_bytes = Vec::new();
    for i in 0..25u8 {
        let frame = vec![i; 100 + i as usize];
        video_bytes.push(frame.clone());
        m.write_packet(
            &Packet::new(0, streams[0].time_base, frame)
                .with_pts(i as i64)
                .with_dts(i as i64)
                .with_duration(1)
                .with_keyframe(i % 5 == 0),
        )
        .unwrap();
        let sound: Vec<u8> = (0..1920 * 4).map(|j| (j as u8) ^ i).collect();
        sound_bytes.extend_from_slice(&sound);
        m.write_packet(&Packet::new(1, streams[1].time_base, sound))
            .unwrap();
    }
    // Small payloads are gathered: nothing but whole blocks reaches
    // the output before the trailer.
    assert!(
        sink.0.lock().unwrap().writes < 4,
        "payload writes are blocked"
    );
    m.write_trailer().unwrap();
    assert!(m.write_trailer().is_err(), "already finalized");

    let mut file = Vec::new();
    let mut reader = sink.clone();
    reader.seek(SeekFrom::Start(0)).unwrap();
    reader.read_to_end(&mut file).unwrap();

    // ftyp, wide, mdat, moov — the mdat size covers the payload.
    let top = atoms(&file);
    let kinds: Vec<[u8; 4]> = top.iter().map(|a| a.0).collect();
    assert_eq!(kinds, [*b"ftyp", *b"wide", *b"mdat", *b"moov"]);
    assert_eq!(top[1].2, 8);
    let payload: u64 =
        video_bytes.iter().map(|v| v.len() as u64).sum::<u64>() + sound_bytes.len() as u64;
    assert_eq!(top[2].2, 8 + payload);

    let mut d = MovDemuxer::open(Box::new(Cursor::new(file)) as Box<dyn ReadSeek>).unwrap();
    assert!(!d.is_faststart());
    // Interleaved in arrival order: one chunk per packet run.
    assert_eq!(d.tracks[0].sample_table.chunk_offsets.len(), 25);
    assert_eq!(d.tracks[1].sample_table.chunk_offsets.len(), 25);
    assert_eq!(d.tracks[1].sample_table.sample_count(), 48000);
    assert!(d.tracks[1].sample_table.is_frame_counted());
    assert_eq!(d.tracks[0].sample_table.stss, [1, 6, 11, 16, 21]);

    let packets = drain(&mut d);
    let got_video: Vec<Vec<u8>> = packets
        .iter()
        .filter(|p| p.stream_index == 0)
        .map(|p| p.data.clone())
        .collect();
    assert_eq!(got_video, video_bytes);
    let got_sound: Vec<u8> = packets
        .iter()
        .filter(|p| p.stream_index == 1)
        .flat_map(|p| p.data.iter().copied())
        .collect();
    assert_eq!(got_sound, sound_bytes);
    // Packets come back in file order: video frame, then its sound.
    assert_eq!(packets[0].stream_index, 0);
    assert_eq!(packets[1].stream_index, 1);
    assert_eq!(packets[1].dts, Some(0));
    let second_video = packets.iter().position(|p| p.pts == Some(1)).unwrap();
    assert_eq!(packets[second_video + 1].dts, Some(1920));
}

#[test]
fn a_frame_split_across_interleaved_packets_is_rejoined() {
    let streams = [stream(0, pcm(), 48000), stream(1, video(), 25)];
    let sink = Sparse::default();
    let out: Box<dyn WriteSeek> = Box::new(sink.clone());
    let mut m = oxideav_mov::registry::open_muxer(out, &streams).unwrap();
    m.write_header().unwrap();
    let sound: Vec<u8> = (0..4000u32).map(|i| (i * 7) as u8).collect();
    // 4000 bytes = 1000 frames, cut at byte counts that split frames,
    // with a video packet between every two.
    let mut at = 0;
    for (i, cut) in [1usize, 2, 1001, 3, 2993].into_iter().enumerate() {
        m.write_packet(&Packet::new(
            0,
            streams[0].time_base,
            sound[at..at + cut].to_vec(),
        ))
        .unwrap();
        at += cut;
        m.write_packet(
            &Packet::new(1, streams[1].time_base, vec![0xEE; 10])
                .with_pts(i as i64)
                .with_keyframe(true),
        )
        .unwrap();
    }
    assert_eq!(at, 4000);
    m.write_trailer().unwrap();
    let mut file = Vec::new();
    let mut reader = sink.clone();
    reader.seek(SeekFrom::Start(0)).unwrap();
    reader.read_to_end(&mut file).unwrap();
    let mut d = MovDemuxer::open(Box::new(Cursor::new(file)) as Box<dyn ReadSeek>).unwrap();
    assert_eq!(d.tracks[0].sample_table.sample_count(), 1000);
    let got: Vec<u8> = drain(&mut d)
        .iter()
        .filter(|p| p.stream_index == 0)
        .flat_map(|p| p.data.iter().copied())
        .collect();
    assert_eq!(got, sound);
}

#[test]
fn a_stream_without_packets_is_refused_at_the_trailer() {
    let streams = [stream(0, video(), 25), stream(1, pcm(), 48000)];
    let out: Box<dyn WriteSeek> = Box::new(Sparse::default());
    let mut m = oxideav_mov::registry::open_muxer(out, &streams).unwrap();
    assert!(
        m.write_packet(&Packet::new(0, streams[0].time_base, vec![1]))
            .is_err()
    );
    m.write_header().unwrap();
    m.write_packet(&Packet::new(0, streams[0].time_base, vec![1]))
        .unwrap();
    // Less than one whole sound frame is no sound at all.
    m.write_packet(&Packet::new(1, streams[1].time_base, vec![1, 2, 3]))
        .unwrap();
    assert!(m.write_trailer().is_err());
}

/// Past 4 GiB of payload the `mdat` header takes its 64-bit form over
/// the `wide` placeholder and the chunk offsets are a `co64`.
#[test]
fn mdat_widens_over_the_wide_atom_past_4_gib() {
    let streams = [stream(0, video(), 25)];
    let sink = Sparse::default();
    let out: Box<dyn WriteSeek> = Box::new(sink.clone());
    let mut m = oxideav_mov::registry::open_muxer(out, &streams).unwrap();
    m.write_header().unwrap();
    // 66 frames of 64 MiB (discarded by the sink), then a small one
    // whose bytes are kept, past the 4 GiB mark.
    const BIG: usize = 64 << 20;
    let mut big = Packet::new(0, streams[0].time_base, vec![0u8; BIG]).with_keyframe(true);
    for i in 0..66i64 {
        big.pts = Some(i);
        big.dts = Some(i);
        m.write_packet(&big).unwrap();
    }
    drop(big);
    let tail = b"the frame past four gibibytes".to_vec();
    m.write_packet(
        &Packet::new(0, streams[0].time_base, tail.clone())
            .with_pts(66)
            .with_dts(66)
            .with_keyframe(true),
    )
    .unwrap();
    m.write_trailer().unwrap();

    let payload = 66 * BIG as u64 + tail.len() as u64;
    let mut reader = sink.clone();
    reader.seek(SeekFrom::Start(0)).unwrap();
    let mut head = [0u8; 44];
    reader.read_exact(&mut head).unwrap();
    assert_eq!(&head[4..8], b"ftyp");
    assert_eq!(u32::from_be_bytes(head[28..32].try_into().unwrap()), 1);
    assert_eq!(&head[32..36], b"mdat");
    assert_eq!(
        u64::from_be_bytes(head[36..44].try_into().unwrap()),
        16 + payload
    );

    let mut d = MovDemuxer::open(Box::new(sink.clone()) as Box<dyn ReadSeek>).unwrap();
    let table = &d.tracks[0].sample_table;
    assert_eq!(table.sample_count(), 67);
    // Contiguous packets share one chunk; its samples resolve past
    // the 32-bit range.
    assert_eq!(table.chunk_offsets, [44]);
    assert_eq!(d.sample_offset(0, 66), Some(44 + 66 * BIG as u64));
    assert_eq!(d.seek_to(0, 66).unwrap(), 66);
    assert_eq!(d.next_packet().unwrap().data, tail);
}
