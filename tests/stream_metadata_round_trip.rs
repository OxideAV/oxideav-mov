//! Stream metadata the container states, read into the stream's
//! `CodecParameters` and written back by the registry muxer: the
//! track language (`mdhd` packed ISO 639-2/T code, `elng` extended
//! tag) and the video colour signal (`colr` `nclx` / `nclc`). Plus the
//! starting timecode of a movie under both readings of the sample
//! integer.

#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};

use oxideav_core::{
    ColorPrimaries, ColorRange, ColorSignal, Demuxer, MatrixCoefficients, Packet, ReadSeek,
    StreamInfo, TransferCharacteristics, WriteSeek,
};
use oxideav_mov::{
    ColorParameters, ColorParametersKind, MovDemuxer, MovMetadata, MovMuxer, MuxSample,
    MuxTrackKind, Tcmi, TimecodeSample, Tmcd, TrackReference, VisualExtensions,
};

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
    MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).expect("open")
}

fn samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| {
            MuxSample::default()
                .with_data(vec![i as u8; 12])
                .with_duration(100)
                .with_keyframe(true)
        })
        .collect()
}

fn video(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 4,
        },
        2500,
        samples(3),
        &[],
    )
}

fn audio(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Audio {
            format: *b".mp3",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        samples(3),
        &[],
    )
}

/// Demux → registry mux → bytes.
fn remux(d: &mut MovDemuxer) -> Vec<u8> {
    let streams: Vec<StreamInfo> = d.streams().to_vec();
    let sink = SharedBuf::default();
    let out: Box<dyn WriteSeek> = Box::new(sink.clone());
    let mut m = oxideav_mov::registry::open_muxer(out, &streams).unwrap();
    m.write_header().unwrap();
    while let Ok(p) = d.next_packet() {
        let p: Packet = p;
        m.write_packet(&p).unwrap();
    }
    m.write_trailer().unwrap();
    sink.0.lock().unwrap().get_ref().clone()
}

fn signal(range: ColorRange, p: u8, t: u8, m: u8) -> ColorSignal {
    ColorSignal::new(
        range,
        ColorPrimaries(p),
        TransferCharacteristics(t),
        MatrixCoefficients(m),
    )
}

#[test]
fn colour_signal_is_read_from_colr_and_written_back() {
    let cases = [
        (
            ColorParametersKind::Nclx {
                primaries: 9,
                transfer: 16,
                matrix: 9,
                full_range: false,
            },
            signal(ColorRange::Limited, 9, 16, 9),
        ),
        (
            ColorParametersKind::Nclx {
                primaries: 1,
                transfer: 13,
                matrix: 5,
                full_range: true,
            },
            signal(ColorRange::Full, 1, 13, 5),
        ),
        // The QuickTime record has no range field.
        (
            ColorParametersKind::Nclc {
                primaries: 1,
                transfer: 1,
                matrix: 6,
            },
            signal(ColorRange::Unspecified, 1, 1, 6),
        ),
    ];
    for (kind, expected) in cases {
        let mut m = MovMuxer::new();
        let id = video(&mut m);
        let colr = ColorParameters { kind: kind.clone() };
        m.set_visual_extensions(
            id,
            VisualExtensions::default().with_colr(Some(colr.clone())),
        )
        .unwrap();
        let mut d = open(m.encode_to_vec().unwrap());
        assert_eq!(d.streams()[0].params.color_signal, expected);

        let d2 = open(remux(&mut d));
        assert_eq!(d2.streams()[0].params.color_signal, expected);
        assert_eq!(d2.tracks[0].sample_descriptions[0].colr, Some(colr));
    }

    // No `colr`, an ICC profile, or an index past eight bits: nothing
    // is claimed, and nothing is written back.
    let mut m = MovMuxer::new();
    video(&mut m);
    let mut d = open(m.encode_to_vec().unwrap());
    assert!(d.streams()[0].params.color_signal.is_unspecified());
    let d2 = open(remux(&mut d));
    assert_eq!(d2.tracks[0].sample_descriptions[0].colr, None);

    for kind in [
        ColorParametersKind::Icc {
            kind: *b"prof",
            profile: vec![1, 2, 3, 4],
        },
        ColorParametersKind::Nclc {
            primaries: 300,
            transfer: 1,
            matrix: 1,
        },
    ] {
        let mut m = MovMuxer::new();
        let id = video(&mut m);
        m.set_visual_extensions(
            id,
            VisualExtensions::default().with_colr(Some(ColorParameters { kind })),
        )
        .unwrap();
        let d = open(m.encode_to_vec().unwrap());
        assert!(d.streams()[0].params.color_signal.is_unspecified());
    }
}

#[test]
fn track_language_is_read_and_written_back() {
    let mut m = MovMuxer::new();
    let v = video(&mut m);
    let a = audio(&mut m);
    let b = audio(&mut m);
    let _und = audio(&mut m);
    m.set_track_language(v, MovMetadata::iso_language(*b"fra"))
        .unwrap();
    m.set_track_language(a, MovMetadata::iso_language(*b"eng"))
        .unwrap();
    // The extended tag overrides the packed code.
    m.set_track_extended_language(a, "en-US").unwrap();
    m.set_track_language(b, MovMetadata::iso_language(*b"jpn"))
        .unwrap();
    let mut d = open(m.encode_to_vec().unwrap());
    let languages = |d: &MovDemuxer| -> Vec<Option<String>> {
        d.streams()
            .iter()
            .map(|s| s.params.language.clone())
            .collect()
    };
    let expected = [
        Some("fra".to_string()),
        Some("en-US".to_string()),
        Some("jpn".to_string()),
        None,
    ];
    assert_eq!(languages(&d), expected);

    let d2 = open(remux(&mut d));
    assert_eq!(languages(&d2), expected);
    assert_eq!(d2.tracks[1].extended_language.as_deref(), Some("en-US"));
    assert_eq!(d2.tracks[0].extended_language, None);
    assert_eq!(
        d2.tracks[2].mdhd.language,
        MovMetadata::iso_language(*b"jpn")
    );
}

/// A timecode track whose single sample is the 32-bit value `raw`.
fn movie_with_timecode(raw: u32, fps: u8) -> MovDemuxer {
    let mut m = MovMuxer::new();
    let v = video(&mut m);
    let tc = m.add_track(
        MuxTrackKind::Timecode {
            description: Tmcd {
                flags: 0,
                time_scale: 2500,
                frame_duration: 100,
                number_of_frames: fps,
                source_name: None,
            },
            tcmi: Tcmi::default(),
        },
        2500,
        vec![
            MuxSample::default()
                .with_data(raw.to_be_bytes().to_vec())
                .with_duration(300)
                .with_keyframe(true),
        ],
        &[],
    );
    m.set_track_references(v, &[TrackReference::timecode(tc)])
        .unwrap();
    open(m.encode_to_vec().unwrap())
}

#[test]
fn starting_timecode_as_a_record_or_a_frame_number() {
    // A packed 10:20:30:12 record.
    let mut d = movie_with_timecode(0x0A14_1E0C, 25);
    let start = d.start_timecode(0).unwrap().unwrap();
    assert!(matches!(start.sample, TimecodeSample::Record(_)));
    let r = start.timecode().unwrap();
    assert_eq!((r.hours, r.minutes, r.seconds, r.frames), (10, 20, 30, 12));
    assert_eq!(start.frame_number(), Some((36000 + 1200 + 30) * 25 + 12));

    // 01:00:00:00 at 25 frames per second stored as its frame number.
    let mut d = movie_with_timecode(90000, 25);
    let start = d.start_timecode(0).unwrap().unwrap();
    assert_eq!(start.frame_number(), Some(90000));
    let r = start.timecode().unwrap();
    assert_eq!((r.hours, r.minutes, r.seconds, r.frames), (1, 0, 0, 0));
}

/// A black-box writer asked for a starting timecode stores a frame
/// number; `timecode()` reads back what was asked for.
#[test]
fn black_box_written_timecode_reads_back() {
    let ok = |tool: &str| {
        Command::new(tool)
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !ok("ffmpeg") {
        eprintln!("skip: ffmpeg not installed");
        return;
    }
    for (asked, fps, expected) in [
        ("01:00:00:00", 25, (1u8, 0u8, 0u8, 0u8)),
        ("10:20:30:12", 25, (10, 20, 30, 12)),
        ("00:59:59:23", 24, (0, 59, 59, 23)),
    ] {
        let path = std::env::temp_dir().join(format!(
            "oxideav-mov-tc-{}-{}.mov",
            std::process::id(),
            asked.replace(':', "")
        ));
        let status = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("color=c=black:s=16x16:r={fps}:d=0.2"))
            .args(["-c:v", "mjpeg", "-timecode", asked])
            .arg(&path)
            .status()
            .expect("run ffmpeg");
        if !status.success() {
            eprintln!("skip: ffmpeg could not write {path:?}");
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let mut d = open(bytes);
        let start = d.start_timecode(0).unwrap().expect("a timecode track");
        assert_eq!(start.number_of_frames, fps as u8, "{asked}");
        let r = start.timecode().unwrap();
        assert_eq!(
            (r.hours, r.minutes, r.seconds, r.frames),
            expected,
            "{asked}"
        );
    }
}
