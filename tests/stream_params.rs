//! Every stream's `CodecParameters` carries the output-describing
//! fields its sample description, configuration record and sample
//! tables state: the decoded-picture layout, output size and nominal
//! frame rate of video; the sample format of uncompressed audio.
//! Fixtures are tiny clips written by black-box ffmpeg encoders
//! (`tests/fixtures/params/gen.sh`); each expected row is what
//! `ffprobe -show_streams` reports for the file.

#![cfg(feature = "registry")]

use oxideav_core::{CodecParameters, Demuxer, PixelFormat, ReadSeek, SampleFormat};
use oxideav_mov::MovDemuxer;

fn params(name: &str) -> CodecParameters {
    let path = format!(
        "{}/tests/fixtures/params/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let f: Box<dyn ReadSeek> = Box::new(std::fs::File::open(&path).unwrap());
    let d = MovDemuxer::open(f).unwrap();
    d.streams()[0].params.clone()
}

/// `(file, pix_fmt, width, height, r_frame_rate)`.
type VideoRow = (&'static str, PixelFormat, u32, u32, (i64, i64));

#[test]
fn video_params_match_ffprobe() {
    use PixelFormat::*;
    let rows: &[VideoRow] = &[
        ("h264-yuv420p.mov", Yuv420P, 32, 18, (25, 1)),
        ("h264-yuv420p10le.mov", Yuv420P10Le, 32, 18, (25, 1)),
        ("h264-yuv422p.mov", Yuv422P, 32, 18, (25, 1)),
        ("h264-yuv444p.mov", Yuv444P, 32, 18, (25, 1)),
        ("h264-ntsc.mov", Yuv420P, 32, 18, (30000, 1001)),
        ("hevc-gray.mov", Gray8, 32, 18, (25, 1)),
        ("hevc-yuv420p.mov", Yuv420P, 32, 18, (25, 1)),
        ("hevc-yuv420p10le.mov", Yuv420P10Le, 32, 18, (25, 1)),
        ("hevc-yuv422p10le.mov", Yuv422P10Le, 32, 18, (25, 1)),
        ("hevc-yuv444p.mov", Yuv444P, 32, 18, (25, 1)),
        ("vp9-yuv420p.mp4", Yuv420P, 32, 18, (25, 1)),
        ("vp9-yuv420p10le.mp4", Yuv420P10Le, 32, 18, (25, 1)),
        ("vp9-yuv422p.mp4", Yuv422P, 32, 18, (25, 1)),
        ("vp9-yuv444p.mp4", Yuv444P, 32, 18, (25, 1)),
        ("av1-yuv420p.mp4", Yuv420P, 64, 64, (25, 1)),
        ("av1-yuv420p10le.mp4", Yuv420P10Le, 64, 64, (25, 1)),
    ];
    for &(file, pf, w, h, (num, den)) in rows {
        let p = params(file);
        assert_eq!(p.pixel_format, Some(pf), "{file}");
        assert_eq!((p.width, p.height), (Some(w), Some(h)), "{file}");
        let fr = p
            .frame_rate
            .unwrap_or_else(|| panic!("{file}: no frame rate"));
        assert_eq!((fr.num, fr.den), (num, den), "{file}");
    }
}

#[test]
fn pcm_params_match_ffprobe() {
    use SampleFormat::*;
    // ffprobe: s16 / s16 / s32 (24 bits_per_sample) / s32 / flt / dbl
    // / u8 / u8 (pcm_s8). ffmpeg widens 24-bit samples to s32 and
    // reports pcm_s8 through its u8 path; the description states a
    // packed 24-bit and a signed 8-bit layout, which `S24` / `S8` name.
    let rows = [
        ("pcm_s16be.mov", S16),
        ("pcm_s16le.mov", S16),
        ("pcm_s24be.mov", S24),
        ("pcm_s32be.mov", S32),
        ("pcm_f32be.mov", F32),
        ("pcm_f64be.mov", F64),
        ("pcm_u8.mov", U8),
        ("pcm_s8.mov", S8),
    ];
    for (file, fmt) in rows {
        let p = params(file);
        assert_eq!(p.sample_format, Some(fmt), "{file}");
        assert_eq!((p.sample_rate, p.channels), (Some(8000), Some(1)), "{file}");
    }
}
