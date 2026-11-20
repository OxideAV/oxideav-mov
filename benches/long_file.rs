//! Long-file timing: a synthesised one-hour movie (30 fps video with
//! reordered frames + 48 kHz audio in 1024-frame packets + 48 kHz
//! 8-bit mono PCM) is muxed through the registry `Muxer`, re-opened,
//! drained and seeked. Prints wall-clock numbers; `harness = false`,
//! standard library only.
//!
//! ```text
//! cargo bench --bench long_file
//! MOV_BENCH_FILE=/path/to/file.mov cargo bench --bench long_file
//! ```
//!
//! With `MOV_BENCH_FILE` set, the synthetic movie is skipped and the
//! named file is opened / drained / seeked instead. `MOV_BENCH_MUX_ONLY`
//! stops after the mux timing (for a peak-memory reading of the mux
//! alone).

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use oxideav_core::{
    CodecId, CodecParameters, Demuxer, Error, Packet, ReadSeek, StreamInfo, TimeBase, WriteSeek,
};
use oxideav_mov::MovDemuxer;

const HOUR: i64 = 3600;

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

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// The packets of the synthetic hour, interleaved by time.
fn source_packets(streams: &[StreamInfo]) -> Vec<Packet> {
    let mut out = Vec::new();
    // Video: 30 fps on a 30000 timescale, I P B B reorder, a sync
    // sample every 30 frames, 600-byte payloads.
    let reorder = [1000i64, 3000, 0, 0];
    let video = (0..HOUR * 30).map(|i| {
        let dts = i * 1000;
        let off = if i % 30 == 0 {
            1000
        } else {
            reorder[(i % 4) as usize]
        };
        Packet::new(0, streams[0].time_base, vec![(i % 251) as u8; 600])
            .with_dts(dts)
            .with_pts(dts + off)
            .with_duration(1000)
            .with_keyframe(i % 30 == 0)
    });
    // Compressed audio: 1024-frame packets at 48 kHz, 200 bytes each.
    let audio = (0..HOUR * 48000 / 1024).map(|i| {
        Packet::new(1, streams[1].time_base, vec![(i % 241) as u8; 200])
            .with_dts(i * 1024)
            .with_pts(i * 1024)
            .with_duration(1024)
            .with_keyframe(true)
    });
    // PCM: 4800-frame (100 ms) packets of 8-bit mono.
    let pcm = (0..HOUR * 10).map(|i| {
        Packet::new(2, streams[2].time_base, vec![(i % 239) as u8; 4800])
            .with_dts(i * 4800)
            .with_pts(i * 4800)
            .with_duration(4800)
            .with_keyframe(true)
    });
    out.extend(video);
    out.extend(audio);
    out.extend(pcm);
    let key = |p: &Packet| {
        let t = p.dts.unwrap() as i128;
        (t * 1_000_000 / p.time_base.den() as i128, p.stream_index)
    };
    out.sort_by_key(key);
    out
}

fn synth_streams() -> Vec<StreamInfo> {
    let mut v = CodecParameters::video(CodecId::new("rawvideo"));
    v.width = Some(64);
    v.height = Some(64);
    let mut a = CodecParameters::audio(CodecId::new("aac")).channels(2);
    a.sample_rate = Some(48000);
    let mut p = CodecParameters::audio(CodecId::new("pcm_u8")).channels(1);
    p.sample_rate = Some(48000);
    let mk = |index: u32, time_base: TimeBase, params: CodecParameters| StreamInfo {
        index,
        time_base,
        duration: None,
        start_time: None,
        params,
    };
    vec![
        mk(0, TimeBase::new(1, 30000), v),
        mk(1, TimeBase::new(1, 48000), a),
        mk(2, TimeBase::new(1, 48000), p),
    ]
}

/// A sink that counts and discards: the mux timing measures the
/// muxer, not the growth of an in-memory file.
#[derive(Default)]
struct Discard {
    pos: u64,
    len: u64,
}

impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pos += buf.len() as u64;
        self.len = self.len.max(self.pos);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for Discard {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::End(d) => (self.len as i64 + d) as u64,
            SeekFrom::Current(d) => (self.pos as i64 + d) as u64,
        };
        Ok(self.pos)
    }
}

fn mux_into(out: Box<dyn WriteSeek>, streams: &[StreamInfo], packets: &[Packet]) {
    let mut m = oxideav_mov::registry::open_muxer(out, streams).unwrap();
    m.write_header().unwrap();
    for p in packets {
        m.write_packet(p).unwrap();
    }
    m.write_trailer().unwrap();
}

fn mux(streams: &[StreamInfo], packets: &[Packet]) -> Vec<u8> {
    let buf = SharedBuf::default();
    mux_into(Box::new(buf.clone()), streams, packets);
    Arc::try_unwrap(buf.0)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap()
        .into_inner()
}

fn open(bytes: &Arc<Vec<u8>>) -> MovDemuxer {
    struct View(Arc<Vec<u8>>);
    impl AsRef<[u8]> for View {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }
    let r: Box<dyn ReadSeek> = Box::new(Cursor::new(View(bytes.clone())));
    MovDemuxer::open(r).unwrap()
}

fn drain(d: &mut MovDemuxer) -> (u64, u64) {
    let (mut n, mut bytes) = (0u64, 0u64);
    loop {
        match d.next_packet() {
            Ok(p) => {
                n += 1;
                bytes += p.data.len() as u64;
            }
            Err(Error::Eof) => return (n, bytes),
            Err(e) => panic!("{e}"),
        }
    }
}

fn main() {
    let bytes = if let Ok(path) = std::env::var("MOV_BENCH_FILE") {
        std::fs::read(&path).unwrap()
    } else {
        let streams = synth_streams();
        let packets = source_packets(&streams);
        let payload: usize = packets.iter().map(|p| p.data.len()).sum();
        let runs: Vec<f64> = (0..3)
            .map(|_| {
                let t = Instant::now();
                mux_into(Box::new(Discard::default()), &streams, &packets);
                t.elapsed().as_secs_f64()
            })
            .collect();
        let s = median(runs);
        println!(
            "mux:   {} packets, {:.1} MiB payload in {:.3} s ({:.0} packets/s, {:.0} MiB/s)",
            packets.len(),
            payload as f64 / 1048576.0,
            s,
            packets.len() as f64 / s,
            payload as f64 / 1048576.0 / s
        );
        if std::env::var_os("MOV_BENCH_MUX_ONLY").is_some() {
            return;
        }
        mux(&streams, &packets)
    };
    let bytes = Arc::new(bytes);
    println!("file:  {:.1} MiB", bytes.len() as f64 / 1048576.0);

    let runs: Vec<f64> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let d = open(&bytes);
            let s = t.elapsed().as_secs_f64();
            std::hint::black_box(d.streams().len());
            s
        })
        .collect();
    println!("open:  {:.2} ms", median(runs) * 1e3);

    let mut n = 0;
    let runs: Vec<f64> = (0..3)
        .map(|_| {
            let mut d = open(&bytes);
            let t = Instant::now();
            let (c, _) = drain(&mut d);
            n = c;
            t.elapsed().as_secs_f64()
        })
        .collect();
    let s = median(runs);
    println!(
        "drain: {n} packets in {:.3} s ({:.0} packets/s)",
        s,
        n as f64 / s
    );

    let mut d = open(&bytes);
    let stream = &d.streams()[0];
    let span = stream
        .duration
        .unwrap_or(HOUR * stream.time_base.den())
        .max(1);
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let targets: Vec<i64> = (0..1000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % span as u64) as i64
        })
        .collect();
    let runs: Vec<f64> = (0..3)
        .map(|_| {
            let t = Instant::now();
            for &pts in &targets {
                let landed = d.seek_to(0, pts).unwrap();
                std::hint::black_box(landed);
                std::hint::black_box(d.next_packet().unwrap().data.len());
            }
            t.elapsed().as_secs_f64()
        })
        .collect();
    println!(
        "seek:  {:.1} µs per seek + first packet (stream 0)",
        median(runs) * 1e6 / targets.len() as f64
    );
}
