//! Round 429 — ffprobe black-box acceptance for the new write-side
//! layouts: time-interleaved chunking, moov-before-mdat (faststart)
//! placement, their combination, and the registry-muxer output.
//!
//! `ffprobe` is used strictly as an opaque validator binary: each
//! fixture must be accepted (exit 0 under `-v error` with no stderr
//! noise) and its packet list — stream index, file position, size —
//! must agree with our own demuxer's view of the same file. Tests
//! skip silently when `ffprobe` is not on `$PATH` (workspace CI).

#![cfg(feature = "registry")]

use std::io::Cursor;
use std::process::Command;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{ChunkStrategy, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

fn ffprobe_available() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn write_temp(bytes: &[u8], tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "oxideav-mov-r429-{tag}-{}.mov",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Oracle packet rows: (stream_index, file_pos, size).
fn ffprobe_packets(path: &std::path::Path) -> Vec<(u32, u64, u64)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=stream_index,pos,size",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        out.status.success(),
        "ffprobe rejected {path:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "ffprobe -v error must stay silent on {path:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            // ffprobe prints the fields in its own section order —
            // stream_index, size, pos — regardless of the order they
            // were requested in.
            let mut it = l.trim().trim_end_matches(',').split(',');
            let stream = it.next()?.parse().ok()?;
            let size = it.next()?.parse().ok()?;
            let pos = it.next()?.parse().ok()?;
            Some((stream, pos, size))
        })
        .collect()
}

/// Our own demuxer's packet rows in emission (file) order, via the
/// sample-offset resolver.
fn own_packets(bytes: Vec<u8>) -> Vec<(u32, u64, u64)> {
    let mut d = open(bytes);
    let mut counters = vec![0u32; d.streams().len()];
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        let idx = p.stream_index;
        let sample = counters[idx as usize];
        counters[idx as usize] += 1;
        let pos = d
            .sample_offset(idx as usize, sample)
            .expect("sample offset");
        out.push((idx, pos, p.data.len() as u64));
    }
    out
}

fn video_samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![0x11 + i as u8; 192], // 8×8 rgb24 "raw " frame
            duration: 100,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn audio_samples(n: usize) -> Vec<MuxSample> {
    // 800 ticks at ts 16000 = 50 ms per sample: 40 samples span the
    // same 2 s as the 20 video frames.
    (0..n)
        .map(|i| MuxSample {
            data: vec![0x99u8.wrapping_add(i as u8); 1600], // 800 × s16be
            duration: 800,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn two_track(strategy: ChunkStrategy, faststart: bool) -> Vec<u8> {
    let mut m = MovMuxer::new()
        .with_movie_timescale(1000)
        .with_chunk_strategy(strategy);
    if faststart {
        m = m.with_faststart();
    }
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 8,
            height: 8,
        },
        1000,
        video_samples(20),
        &[],
    );
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 16000,
        },
        16000,
        audio_samples(40),
        &[],
    );
    m.encode_to_vec().expect("encode")
}

fn assert_oracle_parity(bytes: Vec<u8>, tag: &str) {
    let path = write_temp(&bytes, tag);
    let oracle = ffprobe_packets(&path);
    std::fs::remove_file(&path).ok();
    let ours = own_packets(bytes);
    // Cross-stream emission order is scheduler policy (the oracle
    // interleaves by timestamp, we walk file offsets), so parity is
    // asserted per stream — identical (pos, size) sequences — plus as
    // a whole (same row multiset, nothing extra or missing).
    let streams: std::collections::BTreeSet<u32> = ours.iter().map(|r| r.0).collect();
    for s in streams {
        let a: Vec<_> = ours.iter().filter(|r| r.0 == s).collect();
        let b: Vec<_> = oracle.iter().filter(|r| r.0 == s).collect();
        assert_eq!(a, b, "{tag}: stream {s} (pos, size) rows must match");
    }
    let mut a = ours.clone();
    let mut b = oracle.clone();
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b, "{tag}: complete row multiset must match");
}

#[test]
fn oracle_accepts_interleaved_layout_with_matching_packets() {
    if !ffprobe_available() {
        return;
    }
    // 20 × 100 ticks at ts 1000 = 2 s video; 500-tick period = 0.5 s.
    assert_oracle_parity(
        two_track(ChunkStrategy::InterleaveByMovieTicks(500), false),
        "interleaved",
    );
}

#[test]
fn oracle_accepts_faststart_layout_with_matching_packets() {
    if !ffprobe_available() {
        return;
    }
    assert_oracle_parity(
        two_track(ChunkStrategy::SingleChunkPerTrack, true),
        "faststart",
    );
}

#[test]
fn oracle_accepts_faststart_interleaved_combination() {
    if !ffprobe_available() {
        return;
    }
    assert_oracle_parity(
        two_track(ChunkStrategy::InterleaveByMovieTicks(500), true),
        "faststart-interleaved",
    );
}

#[test]
fn oracle_sees_identical_packets_across_placements() {
    if !ffprobe_available() {
        return;
    }
    // The oracle's stream/size sequence (positions differ by design)
    // must be identical for both moov placements.
    let classic = two_track(ChunkStrategy::SingleChunkPerTrack, false);
    let fast = two_track(ChunkStrategy::SingleChunkPerTrack, true);
    let p1 = write_temp(&classic, "placement-classic");
    let p2 = write_temp(&fast, "placement-fast");
    let a = ffprobe_packets(&p1);
    let b = ffprobe_packets(&p2);
    std::fs::remove_file(&p1).ok();
    std::fs::remove_file(&p2).ok();
    let strip = |rows: &[(u32, u64, u64)]| -> Vec<(u32, u64)> {
        rows.iter().map(|&(s, _, z)| (s, z)).collect()
    };
    assert_eq!(strip(&a), strip(&b), "placement must not change content");
}
