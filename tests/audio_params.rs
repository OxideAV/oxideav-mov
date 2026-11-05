//! Every audio stream's `CodecParameters` carries the facts its sound
//! sample description states once read the way its format code says:
//! the PCM codec id and sample format of the uncompressed formats
//! (version 0 / 1 / 2 descriptions, the `lpcm` flags, the `wave`
//! byte-order datum), the output sample rate / channel count / layout
//! of an `mp4a` AudioSpecificConfig (SBR and PS signalling included),
//! and the layout a `chan` atom names. Fixtures are written by
//! black-box encoders (`tests/fixtures/params/gen.sh`); each expected
//! row is what `ffprobe -show_streams` reports for the file.

#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use oxideav_core::{
    ChannelLayout, CodecParameters, Demuxer, Packet, ReadSeek, SampleFormat, StreamInfo, WriteSeek,
};
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

/// `(file, codec id, sample format)` — every QTFF uncompressed /
/// format-coded sound format.
#[test]
fn pcm_codec_ids_and_sample_formats_match_ffprobe() {
    use SampleFormat::*;
    // ffprobe reports the 24-bit layouts through a widened s32 and the
    // compressed ones through s16 decoders; the description states a
    // packed 24-bit sample (`S24`) and nothing for compressed data
    // (the decoder's call).
    let rows: &[(&str, &str, Option<SampleFormat>)] = &[
        ("pcm_u8.mov", "pcm_u8", Some(U8)),
        ("pcm_s8.mov", "pcm_s8", Some(S8)),
        ("pcm_s16be.mov", "pcm_s16be", Some(S16)),
        ("pcm_s16le.mov", "pcm_s16le", Some(S16)),
        ("pcm_s24be.mov", "pcm_s24be", Some(S24)),
        ("pcm_s24le.mov", "pcm_s24le", Some(S24)),
        ("pcm_s32be.mov", "pcm_s32be", Some(S32)),
        ("pcm_s32le.mov", "pcm_s32le", Some(S32)),
        ("pcm_f32be.mov", "pcm_f32be", Some(F32)),
        ("pcm_f32le.mov", "pcm_f32le", Some(F32)),
        ("pcm_f64be.mov", "pcm_f64be", Some(F64)),
        ("pcm_f64le.mov", "pcm_f64le", Some(F64)),
        ("pcm_mulaw.mov", "pcm_mulaw", None),
        ("pcm_alaw.mov", "pcm_alaw", None),
        ("adpcm_ima_qt.mov", "adpcm_ima_qt", None),
    ];
    for &(file, id, fmt) in rows {
        let p = params(file);
        assert_eq!(p.codec_id.as_str(), id, "{file}");
        assert_eq!(p.sample_format, fmt, "{file}");
        assert_eq!((p.sample_rate, p.channels), (Some(8000), Some(1)), "{file}");
        assert_eq!(p.channel_layout, Some(ChannelLayout::Mono), "{file}");
    }
}

/// Version-2 `lpcm` descriptions: the format-specific flags name the
/// byte order / signedness / float, the Float64 rate passes 65535 Hz.
#[test]
fn lpcm_version_2_matches_ffprobe() {
    use SampleFormat::*;
    let rows: &[(&str, &str, SampleFormat, u32, u16)] = &[
        ("lpcm-s16le-6ch.mov", "pcm_s16le", S16, 48000, 6),
        ("lpcm-f32le-6ch.mov", "pcm_f32le", F32, 48000, 6),
        ("lpcm-s24be-6ch.mov", "pcm_s24be", S24, 48000, 6),
        ("lpcm-s16le-96k.mov", "pcm_s16le", S16, 96000, 2),
    ];
    for &(file, id, fmt, rate, ch) in rows {
        let p = params(file);
        assert_eq!(p.codec_id.as_str(), id, "{file}");
        assert_eq!(p.sample_format, Some(fmt), "{file}");
        assert_eq!(
            (p.sample_rate, p.channels),
            (Some(rate), Some(ch)),
            "{file}"
        );
    }
    // The 5.1 files describe their speakers through a `chan` channel
    // bitmap, whose bit assignment is not staged: the layout is left
    // to the count (which resolves to the 5.1 ffprobe reports).
    let p = params("lpcm-s16le-6ch.mov");
    assert_eq!(p.channel_layout, None);
    assert_eq!(p.resolved_layout(), Some(ChannelLayout::Surround51));
    assert_eq!(
        params("lpcm-s16le-96k.mov").channel_layout,
        Some(ChannelLayout::Stereo)
    );
}

/// `(file, sample_rate, channels, layout)` for AAC: the
/// AudioSpecificConfig, not the sound description, decides.
#[test]
fn aac_params_match_ffprobe() {
    use ChannelLayout::*;
    let rows: &[(&str, u32, u16, Option<ChannelLayout>)] = &[
        ("aac-mono.mov", 44100, 1, Some(Mono)),
        ("aac-3.0.mov", 48000, 3, Some(Surround30)),
        ("aac-4.0.mov", 48000, 4, Some(Surround40)),
        ("aac-5.0.mov", 48000, 5, Some(Surround50)),
        ("aac-5.1.mov", 48000, 6, Some(Surround51)),
        ("aac-7.1.mov", 48000, 8, Some(Surround71)),
        // channelConfiguration 0: the program_config_element counts
        // the speakers (ffprobe: 2.1, 6.1(back), 7.1(wide)); its
        // element groups name no core layout.
        ("aac-pce-2.1.mov", 48000, 3, None),
        ("aac-pce-6.1.mov", 48000, 7, None),
        ("aac-pce-7.1wide.mov", 48000, 8, None),
        // HE-AAC: the SBR extension sampling frequency is the output
        // rate (the core runs at 22050 Hz); a mono HE-AAC stream with
        // the PS flag left implicit opens two output channels, as
        // does the explicit PS signalling of HE-AAC v2.
        ("aac-he-stereo.mov", 44100, 2, Some(Stereo)),
        ("aac-he-mono.mov", 44100, 2, Some(Stereo)),
        ("aac-he-v2.mov", 44100, 2, Some(Stereo)),
    ];
    for &(file, rate, ch, layout) in rows {
        let p = params(file);
        assert_eq!(p.codec_id.as_str(), "aac", "{file}");
        assert_eq!(
            (p.sample_rate, p.channels),
            (Some(rate), Some(ch)),
            "{file}"
        );
        assert_eq!(p.channel_layout, layout, "{file}");
        assert!(p.bit_rate.is_some_and(|b| b > 0), "{file}: esds avgBitrate");
        // The extradata stays the AudioSpecificConfig, verbatim.
        let asc = oxideav_mov::AudioSpecificConfig::parse(&p.extradata)
            .unwrap_or_else(|| panic!("{file}: extradata is not an AudioSpecificConfig"));
        assert_eq!(asc.output_sample_rate(), Some(rate), "{file}");
    }
}

/// The SBR / PS signalling the HE-AAC configurations carry.
#[test]
fn he_aac_signalling_is_read() {
    let asc =
        |file: &str| oxideav_mov::AudioSpecificConfig::parse(&params(file).extradata).unwrap();
    let he = asc("aac-he-stereo.mov");
    assert_eq!(he.audio_object_type, 2);
    assert_eq!(he.extension_audio_object_type, 5);
    assert_eq!(he.sampling_frequency, Some(22050));
    assert_eq!(he.extension_sampling_frequency, Some(44100));
    assert_eq!((he.sbr_present, he.ps_present), (Some(true), None));
    let v2 = asc("aac-he-v2.mov");
    assert_eq!(v2.channel_configuration, 1);
    assert_eq!((v2.sbr_present, v2.ps_present), (Some(true), Some(true)));
    let mono = asc("aac-he-mono.mov");
    assert_eq!(mono.channel_configuration, 1);
    assert_eq!((mono.sbr_present, mono.ps_present), (Some(true), None));
    assert!(mono.parametric_stereo_upmix());
    // The LC encoder signals the absence of SBR explicitly (the
    // backward-compatible extension with sbrPresentFlag = 0).
    let lc = asc("aac-mono.mov");
    assert_eq!(lc.extension_audio_object_type, 5);
    assert_eq!((lc.sbr_present, lc.ps_present), (Some(false), None));
    assert!(!lc.parametric_stereo_upmix());
}

/// A `WriteSeek` sink whose bytes stay reachable after the boxed muxer
/// takes ownership of the writer.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

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

/// Streams and packet payloads of one demuxed file.
type Remuxed = (Vec<StreamInfo>, Vec<Vec<u8>>);

/// Demux a fixture, write it back through the registry muxer, demux
/// the result: `(original, remuxed)` streams and packet payloads.
fn remux(name: &str) -> (Remuxed, Remuxed) {
    fn drain(d: &mut MovDemuxer) -> Vec<Packet> {
        let mut out = Vec::new();
        while let Ok(p) = d.next_packet() {
            out.push(p);
        }
        out
    }
    let path = format!(
        "{}/tests/fixtures/params/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let f: Box<dyn ReadSeek> = Box::new(std::fs::File::open(&path).unwrap());
    let mut d = MovDemuxer::open(f).unwrap();
    let streams = d.streams().to_vec();
    let packets = drain(&mut d);
    let mut ctx = oxideav_core::RuntimeContext::new();
    oxideav_mov::registry::register(&mut ctx);
    let sink = SharedBuf::default();
    let out: Box<dyn WriteSeek> = Box::new(sink.clone());
    let mut mux = ctx.containers.open_muxer("mov", out, &streams).unwrap();
    mux.write_header().unwrap();
    for p in &packets {
        mux.write_packet(p).unwrap();
    }
    mux.write_trailer().unwrap();
    let bytes = sink.0.lock().unwrap().get_ref().clone();
    let mut d2 = MovDemuxer::open(Box::new(Cursor::new(bytes))).unwrap();
    let streams2 = d2.streams().to_vec();
    let packets2 = drain(&mut d2);
    (
        (streams, packets.into_iter().map(|p| p.data).collect()),
        (streams2, packets2.into_iter().map(|p| p.data).collect()),
    )
}

/// The muxer writes the same facts back: a remuxed stream demuxes to
/// the same codec id, rate, channel count, layout, sample format and
/// extradata, with identical payloads.
#[test]
fn audio_params_survive_a_registry_remux() {
    for file in [
        "pcm_u8.mov",
        "pcm_s8.mov",
        "pcm_s16be.mov",
        "pcm_s16le.mov",
        "pcm_s24be.mov",
        "pcm_s24le.mov",
        "pcm_s32le.mov",
        "pcm_f32be.mov",
        "pcm_f64le.mov",
        "pcm_mulaw.mov",
        "pcm_alaw.mov",
        "lpcm-s24be-6ch.mov",
        "lpcm-f32le-6ch.mov",
        "lpcm-s16le-96k.mov",
        "aac-mono.mov",
        "aac-5.1.mov",
        "aac-7.1.mov",
        "aac-pce-6.1.mov",
        "aac-he-mono.mov",
        "aac-he-v2.mov",
    ] {
        let ((s1, p1), (s2, p2)) = remux(file);
        let (a, b) = (&s1[0].params, &s2[0].params);
        assert_eq!(a.codec_id, b.codec_id, "{file}");
        assert_eq!(a.sample_rate, b.sample_rate, "{file}");
        assert_eq!(a.channels, b.channels, "{file}");
        assert_eq!(a.channel_layout, b.channel_layout, "{file}");
        assert_eq!(a.sample_format, b.sample_format, "{file}");
        assert_eq!(a.extradata, b.extradata, "{file}");
        assert_eq!(p1, p2, "{file}: payloads");
    }
}

/// A layout the channel count alone does not imply travels in a
/// `chan` atom: 4.0 (L R C Cs) instead of the 4-channel quad default.
#[test]
fn chan_layout_is_written_and_read_back() {
    let ((s1, _), (s2, _)) = remux("lpcm-s16le-96k.mov");
    assert_eq!(s1[0].params.channel_layout, Some(ChannelLayout::Stereo));
    assert_eq!(s2[0].params.channel_layout, Some(ChannelLayout::Stereo));
    let ((s1, _), (s2, _)) = remux("aac-4.0.mov");
    assert_eq!(s1[0].params.channel_layout, Some(ChannelLayout::Surround40));
    assert_eq!(s2[0].params.channel_layout, Some(ChannelLayout::Surround40));
}
