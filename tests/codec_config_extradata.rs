//! Video / audio codec configuration records surface as the stream
//! extradata (the bare record, not the framed sample-description
//! extension atoms), and the registry muxer frames them back into the
//! right configuration atom so demux → mux → demux is byte-exact.
//!
//! Fixture `h264_2s.mov` (see `seek.rs`): ffprobe reports a 46-byte
//! AVCDecoderConfigurationRecord starting `01 64 00 0a ff e1 00 19`
//! (configurationVersion 1, High profile, level 1.0, 4-byte NAL
//! lengths, one SPS of 25 bytes).

#![cfg(feature = "registry")]

use std::fs::File;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use oxideav_core::{Demuxer, MediaType, ReadSeek, StreamInfo, WriteSeek};
use oxideav_mov::MovDemuxer;

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

fn fixture(name: &str) -> Option<Box<dyn ReadSeek>> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    File::open(&p)
        .ok()
        .map(|f| Box::new(f) as Box<dyn ReadSeek>)
}

#[test]
fn h264_extradata_is_the_avcc_record() {
    let Some(f) = fixture("h264_2s.mov") else {
        return;
    };
    let d = MovDemuxer::open(f).unwrap();
    let v = d
        .streams()
        .iter()
        .find(|s| s.params.media_type == MediaType::Video)
        .expect("video stream");
    let x = &v.params.extradata;
    assert_eq!(x.len(), 46, "ffprobe extradata_size");
    assert_eq!(&x[..8], &[0x01, 0x64, 0x00, 0x0A, 0xFF, 0xE1, 0x00, 0x19]);
    // SPS NAL header (nal_unit_type 7) right after its length.
    assert_eq!(x[8] & 0x1F, 7);
}

#[test]
fn h264_avcc_survives_registry_remux() {
    let Some(f) = fixture("h264_2s.mov") else {
        return;
    };
    let mut d = MovDemuxer::open(f).unwrap();
    let streams: Vec<StreamInfo> = d.streams().to_vec();
    let mut ctx = oxideav_core::RuntimeContext::new();
    oxideav_mov::registry::register(&mut ctx);
    let sink = SharedBuf::default();
    let out: Box<dyn WriteSeek> = Box::new(sink.clone());
    let mut mux = ctx.containers.open_muxer("mov", out, &streams).unwrap();
    mux.write_header().unwrap();
    while let Ok(p) = d.next_packet() {
        mux.write_packet(&p).unwrap();
    }
    mux.write_trailer().unwrap();
    let bytes = sink.0.lock().unwrap().get_ref().clone();
    let d2 = MovDemuxer::open(Box::new(Cursor::new(bytes))).unwrap();
    for (a, b) in streams.iter().zip(d2.streams()) {
        assert_eq!(a.params.codec_id, b.params.codec_id);
        assert_eq!(a.params.extradata, b.params.extradata, "stream {}", a.index);
    }
}
