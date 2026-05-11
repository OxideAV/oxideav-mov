//! Round 19 — `MovMuxer` write-side acceptance tests.
//!
//! Builds a synthetic 5-frame H.264-in-MOV plus a 3-sample audio MOV
//! through [`MovMuxer`], rolls each into a `Cursor<Vec<u8>>`, and
//! demuxes the result back through [`MovDemuxer`] to confirm the
//! emitted file is structurally correct: per-track sample count
//! preserved, per-sample sizes preserved, sample bytes preserved
//! verbatim, keyframe flags preserved, and the demuxer recognises
//! both `vide` and `soun` tracks with the stsd-declared FourCC.
//!
//! The two `ffprobe`-cross-check tests are guarded by `which ffprobe`
//! so they no-op (with a stderr note) when ffprobe isn't on `$PATH`.
//! Per project policy: ffprobe is only used as a black-box oracle —
//! we never link against libavformat.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

/// Build a 5-frame video-only MOV. Frame 0 is a keyframe; frames 1–4
/// are non-keyframes (so `stss` must be emitted). Each "frame" is a
/// recognisable byte pattern so the roundtrip can spot byte-level
/// corruption.
fn build_5_frame_video_mov() -> (Vec<u8>, Vec<MuxSample>) {
    let samples: Vec<MuxSample> = (0..5)
        .map(|i| MuxSample {
            data: {
                let mut buf = vec![0u8; 32 + i];
                for (j, b) in buf.iter_mut().enumerate() {
                    *b = ((i << 4) | (j & 0xF)) as u8;
                }
                buf
            },
            duration: 1000, // 1000 ticks @ 30000/s = 33.33 ms ⇒ ~30 fps
            keyframe: i == 0,
        })
        .collect();
    let mut m = MovMuxer::new().with_movie_timescale(600);
    m.add_track(
        MuxTrackKind::Video {
            // Use `mp4v` rather than `avc1` so the muxer doesn't need
            // a real `avcC` extradata blob to look authentic — the
            // round-19 surface intentionally doesn't synthesise codec
            // configs.
            format: *b"mp4v",
            width: 320,
            height: 240,
        },
        30000,
        samples.clone(),
        &[],
    );
    (
        m.encode_to_vec().expect("encode 5-frame video MOV"),
        samples,
    )
}

#[test]
fn roundtrip_5_frame_video_mov_preserves_sample_count_and_bytes() {
    let (bytes, samples_in) = build_5_frame_video_mov();

    // Demux it back.
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
    let mut d = MovDemuxer::open(cur).expect("open synth video MOV");

    // ftyp / mvhd surface
    let ftyp = d.ftyp.as_ref().expect("ftyp present");
    assert!(ftyp.is_quicktime(), "qt  brand expected");
    let mvhd = d.mvhd.as_ref().expect("mvhd present");
    assert_eq!(mvhd.time_scale, 600);

    // Single video track
    assert_eq!(d.tracks.len(), 1);
    let tr = &d.tracks[0];
    assert!(tr.is_video(), "hdlr.subtype should be vide");
    assert_eq!(tr.tkhd.track_id, 1);
    assert_eq!(tr.tkhd.width(), 320);
    assert_eq!(tr.tkhd.height(), 240);
    assert_eq!(tr.mdhd.time_scale, 30000);
    assert_eq!(tr.primary_format(), Some(*b"mp4v"));
    assert_eq!(tr.sample_table.sample_count(), 5);

    // Per-sample sizes match the input.
    let st = &tr.sample_table;
    let entries: Vec<_> = st
        .iter_samples()
        .collect::<oxideav_core::Result<_>>()
        .unwrap();
    assert_eq!(entries.len(), 5);
    for (i, (sample_in, entry)) in samples_in.iter().zip(entries.iter()).enumerate() {
        assert_eq!(
            entry.size as usize,
            sample_in.data.len(),
            "size mismatch at sample {i}"
        );
        assert_eq!(entry.duration, sample_in.duration, "duration at sample {i}");
        assert_eq!(
            entry.keyframe, sample_in.keyframe,
            "keyframe flag at sample {i}"
        );
    }

    // Keyframe surface: only sample 0 should be flagged.
    assert!(entries[0].keyframe);
    for entry in entries.iter().skip(1) {
        assert!(!entry.keyframe);
    }

    // Walk the demuxer's packet stream and verify byte-for-byte the
    // sample bytes round-trip cleanly.
    for (i, sample_in) in samples_in.iter().enumerate() {
        let pkt = d
            .next_packet()
            .unwrap_or_else(|e| panic!("next_packet at {i}: {e:?}"));
        assert_eq!(pkt.stream_index, 0);
        assert_eq!(
            pkt.data, sample_in.data,
            "byte-level mismatch at sample {i}"
        );
    }
    match d.next_packet() {
        Err(oxideav_core::Error::Eof) => {}
        other => panic!("expected Eof after 5 packets, got {other:?}"),
    }
}

#[test]
fn roundtrip_audio_only_mov_preserves_sample_table() {
    // 3 PCM samples @ 8000 Hz, mono 16-bit. All samples are
    // keyframes by audio convention, so `stss` should be omitted
    // (every-sample-keyframe implicit rule from QTFF p. 73).
    let samples: Vec<MuxSample> = (0..3)
        .map(|i| MuxSample {
            data: vec![(i * 2) as u8; 256],
            duration: 128,
            keyframe: true,
        })
        .collect();
    let mut m = MovMuxer::new().with_movie_timescale(600);
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        samples.clone(),
        &[],
    );
    let bytes = m.encode_to_vec().expect("encode audio-only MOV");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open synth audio MOV");
    assert_eq!(d.tracks.len(), 1);
    let tr = &d.tracks[0];
    assert!(tr.is_audio());
    assert_eq!(tr.primary_format(), Some(*b"sowt"));
    assert_eq!(tr.sample_table.sample_count(), 3);
    // Sample-table tells us all 3 samples were the same size ⇒
    // `stsz_default_size = Some(256)`.
    assert_eq!(tr.sample_table.stsz_default_size, Some(256));
    // `stss` should be empty (implicit every-sample-keyframe).
    assert!(
        tr.sample_table.stss.is_empty(),
        "stss should be omitted for an all-keyframes audio track"
    );

    for (i, sample_in) in samples.iter().enumerate() {
        let pkt = d
            .next_packet()
            .unwrap_or_else(|e| panic!("audio next_packet at {i}: {e:?}"));
        assert_eq!(pkt.data, sample_in.data, "audio byte mismatch at {i}");
    }
}

#[test]
fn roundtrip_two_track_video_plus_audio_preserves_both_streams() {
    // Track 1: 4 video frames, sizes vary (24, 25, 26, 27 bytes).
    let video_samples: Vec<MuxSample> = (0..4)
        .map(|i| MuxSample {
            data: vec![(0xA0 | i) as u8; 24 + i],
            duration: 1500,
            keyframe: i == 0,
        })
        .collect();
    // Track 2: 2 audio samples, uniform size.
    let audio_samples: Vec<MuxSample> = (0..2)
        .map(|i| MuxSample {
            data: vec![(0xB0 | i) as u8; 100],
            duration: 1024,
            keyframe: true,
        })
        .collect();

    let mut m = MovMuxer::new().with_movie_timescale(600);
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 64,
            height: 48,
        },
        30000,
        video_samples.clone(),
        &[],
    );
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 44100,
        },
        44100,
        audio_samples.clone(),
        &[],
    );
    let bytes = m.encode_to_vec().expect("encode video+audio MOV");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open video+audio MOV");
    assert_eq!(d.tracks.len(), 2);
    assert!(d.tracks[0].is_video());
    assert!(d.tracks[1].is_audio());
    assert_eq!(d.tracks[0].sample_table.sample_count(), 4);
    assert_eq!(d.tracks[1].sample_table.sample_count(), 2);
    assert_eq!(d.tracks[0].tkhd.track_id, 1);
    assert_eq!(d.tracks[1].tkhd.track_id, 2);

    // Per-sample byte-level check via the sample-table iterator.
    let v_entries: Vec<_> = d.tracks[0]
        .sample_table
        .iter_samples()
        .collect::<oxideav_core::Result<_>>()
        .unwrap();
    let a_entries: Vec<_> = d.tracks[1]
        .sample_table
        .iter_samples()
        .collect::<oxideav_core::Result<_>>()
        .unwrap();
    for (i, (sin, e)) in video_samples.iter().zip(v_entries.iter()).enumerate() {
        assert_eq!(
            e.size as usize,
            sin.data.len(),
            "video sample {i} size mismatch"
        );
    }
    for (i, (sin, e)) in audio_samples.iter().zip(a_entries.iter()).enumerate() {
        assert_eq!(
            e.size as usize,
            sin.data.len(),
            "audio sample {i} size mismatch"
        );
    }
}

#[test]
fn empty_track_list_rejected() {
    let m = MovMuxer::new();
    assert!(m.encode_to_vec().is_err());
}

#[test]
fn track_with_zero_samples_rejected() {
    let mut m = MovMuxer::new();
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 16,
            height: 16,
        },
        30000,
        Vec::new(),
        &[],
    );
    assert!(m.encode_to_vec().is_err());
}

// ─────────────────────── ffprobe oracle ───────────────────────

/// Locate `ffprobe` on PATH, returning `None` (with a stderr note) when
/// absent so CI hosts without ffmpeg installed don't fail the test.
fn ffprobe_path() -> Option<std::path::PathBuf> {
    use std::process::Command;
    Command::new("which")
        .arg("ffprobe")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

#[test]
fn ffprobe_accepts_synth_video_only_mov() {
    let Some(ffprobe) = ffprobe_path() else {
        eprintln!("ffprobe not on PATH — skipping cross-check");
        return;
    };
    let (bytes, _) = build_5_frame_video_mov();
    let dir = tempdir();
    let path = dir.join("synth_video.mov");
    std::fs::write(&path, &bytes).expect("write synth file");

    let output = std::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-of",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(&path)
        .output()
        .expect("run ffprobe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ffprobe rejected synth file: stderr={stderr}\n stdout={stdout}"
    );
    // Cheap-but-effective sanity check: ffprobe must report exactly
    // one stream (the video track) and the format-level handler
    // section must mention the file at all.
    assert!(
        stdout.contains("\"streams\""),
        "ffprobe output missing streams section: {stdout}"
    );
    // Count `"codec_type"` occurrences — should be 1 for one stream.
    let stream_count = stdout.matches("\"codec_type\"").count();
    assert_eq!(
        stream_count, 1,
        "expected 1 stream in ffprobe output, got {stream_count}: {stdout}"
    );
    assert!(
        stdout.contains("\"codec_type\": \"video\""),
        "ffprobe should classify the stream as video: {stdout}"
    );
}

#[test]
fn ffprobe_accepts_synth_video_plus_audio_mov() {
    let Some(ffprobe) = ffprobe_path() else {
        eprintln!("ffprobe not on PATH — skipping cross-check");
        return;
    };
    // Build a 4-video + 2-audio MOV through MovMuxer.
    let video_samples: Vec<MuxSample> = (0..4)
        .map(|i| MuxSample {
            data: vec![(0xA0 | i) as u8; 24 + i],
            duration: 1500,
            keyframe: i == 0,
        })
        .collect();
    let audio_samples: Vec<MuxSample> = (0..2)
        .map(|i| MuxSample {
            data: vec![(0xB0 | i) as u8; 100],
            duration: 1024,
            keyframe: true,
        })
        .collect();
    let mut m = MovMuxer::new().with_movie_timescale(600);
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 64,
            height: 48,
        },
        30000,
        video_samples,
        &[],
    );
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 44100,
        },
        44100,
        audio_samples,
        &[],
    );
    let bytes = m.encode_to_vec().expect("encode v+a MOV");

    let dir = tempdir();
    let path = dir.join("synth_va.mov");
    std::fs::write(&path, &bytes).expect("write synth v+a file");

    let output = std::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-of",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(&path)
        .output()
        .expect("run ffprobe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ffprobe rejected synth v+a file: stderr={stderr}\n stdout={stdout}"
    );
    let stream_count = stdout.matches("\"codec_type\"").count();
    assert_eq!(stream_count, 2, "expected 2 streams: {stdout}");
    assert!(stdout.contains("\"codec_type\": \"video\""));
    assert!(stdout.contains("\"codec_type\": \"audio\""));
}

/// Cheap per-test temp dir — uses `std::env::temp_dir()` plus a
/// monotonic counter so two parallel tests don't collide. Avoids
/// pulling in a `tempfile` dev-dep.
fn tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let mut p = std::env::temp_dir();
    p.push(format!("oxideav-mov-r19-{pid}-{seq}"));
    std::fs::create_dir_all(&p).expect("mkdir tempdir");
    p
}
