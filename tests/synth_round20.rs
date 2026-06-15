//! Round 20 — fragmented MP4 / fMP4 / DASH muxer acceptance.
//!
//! Builds a fragmented MP4 entirely in memory through
//! [`MovMuxer::with_fragmentation`] + [`MovMuxer::encode_fragmented_to_vec`]
//! then runs the result back through [`MovDemuxer`] (which already
//! handles `moof` per round 18) to confirm:
//!
//! * the emitted file is structurally a fragmented stream
//!   (`d.is_fragmented() == true`),
//! * the per-fragment `mfhd.sequence_number` climbs from 1
//!   monotonically,
//! * the total per-track sample count matches the input,
//! * per-sample byte payloads survive verbatim,
//! * `ffprobe -of json` accepts the output (skipped when ffprobe
//!   isn't on PATH).
//!
//! Spec: ISO/IEC 14496-12:2015 §8.8 (Movie Fragments) + ISO/IEC
//! 23009-1 §6.3.4.2 (DASH segment shape).

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{FragmentationMode, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

/// Build a recognisable 5-frame H.264-ish video sample list. Frame 0
/// is a keyframe; frames 1–4 are non-keyframes. Each frame has a
/// distinct byte pattern so the roundtrip can catch byte-level
/// corruption.
fn synth_5_video_samples() -> Vec<MuxSample> {
    (0..5)
        .map(|i| MuxSample {
            data: {
                let mut buf = vec![0u8; 40 + i * 2];
                for (j, b) in buf.iter_mut().enumerate() {
                    *b = ((i << 4) | (j & 0xF)) as u8;
                }
                buf
            },
            duration: 1000, // 1000 ticks @ 30000/s ≈ 30 fps
            keyframe: i == 0,
            composition_offset: 0,
        })
        .collect()
}

#[test]
fn fragmented_by_frame_count_emits_three_fragments_for_5_samples_n2() {
    let samples = synth_5_video_samples();
    let mut m = MovMuxer::new()
        .with_movie_timescale(600)
        .with_fragmentation(FragmentationMode::ByFrameCount(2));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 320,
            height: 240,
        },
        30000,
        samples.clone(),
        &[],
    );
    let bytes = m.encode_fragmented_to_vec().expect("encode fragmented MP4");

    // Roundtrip back through MovDemuxer (which walks moof per r18).
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
    let mut d = MovDemuxer::open(cur).expect("open fragmented MP4");

    // Structural surface.
    assert!(d.is_fragmented(), "demuxer should report is_fragmented");
    assert_eq!(d.tracks.len(), 1);
    assert_eq!(
        d.fragment_sequence_numbers,
        vec![1, 2, 3],
        "ByFrameCount(2) on 5 samples → 3 fragments"
    );
    assert_eq!(d.tracks[0].fragment_samples.len(), 5);

    // trex defaults visible.
    assert_eq!(d.trex_defaults.len(), 1);
    assert_eq!(d.trex_defaults[0].track_id, 1);

    // Walk all 5 packets and verify byte-for-byte the payloads
    // survive verbatim.
    for (i, sample_in) in samples.iter().enumerate() {
        let pkt = d
            .next_packet()
            .unwrap_or_else(|e| panic!("next_packet at sample {i}: {e:?}"));
        assert_eq!(
            pkt.data, sample_in.data,
            "byte-level mismatch at sample {i}"
        );
        assert_eq!(pkt.duration, Some(1000));
    }
    match d.next_packet() {
        Err(oxideav_core::Error::Eof) => {}
        other => panic!("expected Eof after 5 packets, got {other:?}"),
    }
}

#[test]
fn fragmented_by_duration_slices_along_primary_timebase() {
    // 6 samples × 1000 ticks each (in 30000 timescale). With a
    // threshold of 2000 ticks per fragment, we expect 3 fragments of
    // 2 samples each.
    let samples: Vec<MuxSample> = (0..6)
        .map(|i| MuxSample {
            data: vec![(0x70 + i) as u8; 32],
            duration: 1000,
            keyframe: i == 0,
            composition_offset: 0,
        })
        .collect();
    let mut m = MovMuxer::new()
        .with_movie_timescale(600)
        .with_fragmentation(FragmentationMode::ByDuration(2000));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 16,
            height: 16,
        },
        30000,
        samples.clone(),
        &[],
    );
    let bytes = m.encode_fragmented_to_vec().expect("encode fragmented");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open fragmented");

    assert_eq!(d.fragment_sequence_numbers, vec![1, 2, 3]);
    assert_eq!(d.tracks[0].fragment_samples.len(), 6);

    for (i, sample_in) in samples.iter().enumerate() {
        let pkt = d.next_packet().expect("next_packet");
        assert_eq!(pkt.data, sample_in.data, "mismatch at sample {i}");
    }
}

#[test]
fn fragmented_keyframe_flag_round_trips_via_trun_sample_flags() {
    let samples = synth_5_video_samples();
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(5));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 32,
            height: 32,
        },
        30000,
        samples.clone(),
        &[],
    );
    let bytes = m.encode_fragmented_to_vec().expect("encode");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open");
    // One fragment carrying 5 samples — verify per-sample keyframe
    // flag survived via the trun.sample_flags slot.
    assert_eq!(d.fragment_sequence_numbers, vec![1]);
    let frag_entries = &d.tracks[0].fragment_samples;
    assert_eq!(frag_entries.len(), 5);
    assert!(frag_entries[0].keyframe, "sample 0 should be a keyframe");
    for (i, e) in frag_entries.iter().enumerate().skip(1) {
        assert!(!e.keyframe, "sample {i} should be non-keyframe");
    }
}

#[test]
fn fragmented_dts_climbs_monotonically_across_fragment_boundaries() {
    let samples = synth_5_video_samples();
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 32,
            height: 32,
        },
        30000,
        samples,
        &[],
    );
    let bytes = m.encode_fragmented_to_vec().expect("encode");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open");
    let entries = &d.tracks[0].fragment_samples;
    // Fragment 1: samples 0, 1 → DTS 0, 1000.
    // Fragment 2: samples 2, 3 → DTS 2000, 3000.
    // Fragment 3: sample 4 → DTS 4000.
    let expected: &[u64] = &[0, 1000, 2000, 3000, 4000];
    assert_eq!(entries.len(), expected.len());
    for (i, e) in entries.iter().enumerate() {
        assert_eq!(e.dts, expected[i], "DTS at sample {i}");
    }
}

#[test]
fn fragmented_requires_fragmentation_mode() {
    let mut m = MovMuxer::new();
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 16,
            height: 16,
        },
        30000,
        vec![MuxSample {
            data: vec![0u8; 16],
            duration: 1000,
            keyframe: true,
            composition_offset: 0,
        }],
        &[],
    );
    assert!(
        m.encode_fragmented_to_vec().is_err(),
        "encode_fragmented_to_vec without with_fragmentation must error"
    );
}

#[test]
fn fragmented_by_frame_count_zero_rejected() {
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(0));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 16,
            height: 16,
        },
        30000,
        vec![MuxSample {
            data: vec![0u8; 16],
            duration: 1000,
            keyframe: true,
            composition_offset: 0,
        }],
        &[],
    );
    assert!(m.encode_fragmented_to_vec().is_err());
}

#[test]
fn fragmented_by_duration_zero_rejected() {
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByDuration(0));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 16,
            height: 16,
        },
        30000,
        vec![MuxSample {
            data: vec![0u8; 16],
            duration: 1000,
            keyframe: true,
            composition_offset: 0,
        }],
        &[],
    );
    assert!(m.encode_fragmented_to_vec().is_err());
}

#[test]
fn fragmented_empty_track_list_rejected() {
    let m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(1));
    assert!(m.encode_fragmented_to_vec().is_err());
}

#[test]
fn fragmented_track_with_zero_samples_rejected() {
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(1));
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
    assert!(m.encode_fragmented_to_vec().is_err());
}

#[test]
fn fragmented_audio_only_track_works() {
    // 8 PCM samples @ 8000 Hz, all keyframes. Fragmented by
    // ByFrameCount(3) → 3 fragments (3 + 3 + 2 samples).
    let samples: Vec<MuxSample> = (0..8)
        .map(|i| MuxSample {
            data: vec![(0x40 + i) as u8; 64],
            duration: 128,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(3));
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
    let bytes = m.encode_fragmented_to_vec().expect("encode");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open audio-only fragmented");
    assert!(d.is_fragmented());
    assert_eq!(d.fragment_sequence_numbers, vec![1, 2, 3]);
    assert_eq!(d.tracks[0].fragment_samples.len(), 8);
    for (i, sample_in) in samples.iter().enumerate() {
        let pkt = d.next_packet().expect("next_packet");
        assert_eq!(pkt.data, sample_in.data, "audio byte mismatch at {i}");
    }
}

#[test]
fn fragmented_init_segment_has_ftyp_then_moov_layout() {
    let samples = synth_5_video_samples();
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(5));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 32,
            height: 32,
        },
        30000,
        samples,
        &[],
    );
    let bytes = m.encode_fragmented_to_vec().expect("encode");
    // First atom is ftyp.
    assert_eq!(&bytes[4..8], b"ftyp");
    let ftyp_size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    // Next atom (after ftyp) must be moov per ISO BMFF §8.8 init
    // segment shape.
    assert_eq!(
        &bytes[ftyp_size + 4..ftyp_size + 8],
        b"moov",
        "init segment must lead ftyp + moov"
    );
    // Then at least one moof.
    let moov_size = u32::from_be_bytes([
        bytes[ftyp_size],
        bytes[ftyp_size + 1],
        bytes[ftyp_size + 2],
        bytes[ftyp_size + 3],
    ]) as usize;
    let after_moov = ftyp_size + moov_size;
    assert_eq!(&bytes[after_moov + 4..after_moov + 8], b"moof");
    // And the ftyp should declare iso5.
    assert!(
        bytes.windows(4).any(|w| w == b"iso5"),
        "ftyp should declare iso5 brand"
    );
    assert!(
        bytes.windows(4).any(|w| w == b"dash"),
        "ftyp should declare dash brand"
    );
}

// ─────────────────────── ffprobe oracle ───────────────────────

/// Locate `ffprobe` on PATH, returning `None` when absent so CI hosts
/// without ffmpeg installed don't fail the test.
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
fn ffprobe_accepts_fragmented_output() {
    let Some(ffprobe) = ffprobe_path() else {
        eprintln!("ffprobe not on PATH — skipping ffprobe oracle");
        return;
    };
    let samples = synth_5_video_samples();
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"mp4v",
            width: 320,
            height: 240,
        },
        30000,
        samples,
        &[],
    );
    let bytes = m.encode_fragmented_to_vec().expect("encode");

    let dir = tempdir();
    let path = dir.join("synth_fragmented.mp4");
    std::fs::write(&path, &bytes).expect("write synth fragmented file");

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
        "ffprobe rejected fragmented output: stderr={stderr}\nstdout={stdout}"
    );
    assert!(stdout.contains("\"streams\""), "missing streams: {stdout}");
    assert!(
        stdout.contains("\"codec_type\": \"video\""),
        "ffprobe should classify the stream as video: {stdout}"
    );
}

/// Cheap per-test temp dir.
fn tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let mut p = std::env::temp_dir();
    p.push(format!("oxideav-mov-r20-{pid}-{seq}"));
    std::fs::create_dir_all(&p).expect("mkdir tempdir");
    p
}
