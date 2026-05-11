//! Round-18 ffmpeg-oracle acceptance: drive real `ffmpeg`-emitted
//! fragmented MP4 through `MovDemuxer` and verify the demuxer
//! walks every sample listed by `ffprobe`.
//!
//! The fixture is **generated at test time** rather than checked
//! into the repo — running these tests is gated on the host having
//! `ffmpeg` + `ffprobe` on `$PATH`, so the suite is a no-op on
//! environments without the binaries (notably the workspace CI
//! image). They serve as a local-development cross-check.
//!
//! Spec: ISO/IEC 14496-12:2015 §8.8.

#![cfg(feature = "registry")]

use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::MovDemuxer;

/// Return `Some(PathBuf)` when both `ffmpeg` and `ffprobe` resolve
/// on `$PATH`, otherwise `None` (the caller silently skips its
/// test body).
fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        && Command::new("ffprobe")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("oxideav-mov-r18-{nonce}-{name}"))
}

fn nb_packets(path: &std::path::Path) -> usize {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-count_packets"])
        .args([
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "default=nokey=1:noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        out.status.success(),
        "ffprobe nb_read_packets failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim()
        .parse::<usize>()
        .unwrap_or_else(|e| panic!("ffprobe count parse failed: {s:?}: {e}"))
}

/// Generate a single-fragment frag_keyframe MP4 with empty_moov.
/// `ffmpeg -movflags +frag_keyframe+empty_moov` is the canonical
/// "DASH-style" output that places all samples behind a single
/// `moof` after an init-segment `moov`.
fn generate_single_moof(path: &std::path::Path) {
    let status = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "testsrc=duration=1:size=64x48:rate=10"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "999"])
        .args(["-movflags", "+frag_keyframe+empty_moov"])
        .args(["-f", "mp4"])
        .arg(path)
        .status()
        .expect("spawn ffmpeg");
    assert!(status.success(), "ffmpeg single-fragment generation failed");
}

/// Generate a multi-fragment MP4 where each fragment holds ~one
/// sample (so `nb_packets == n_moofs` for this fixture).
fn generate_multi_moof(path: &std::path::Path) {
    let status = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "testsrc=duration=1:size=64x48:rate=10"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "999"])
        .args(["-movflags", "+frag_keyframe+empty_moov+default_base_moof"])
        .args(["-frag_duration", "100000"])
        .args(["-f", "mp4"])
        .arg(path)
        .status()
        .expect("spawn ffmpeg");
    assert!(status.success(), "ffmpeg multi-fragment generation failed");
}

#[test]
fn ffmpeg_single_fragment_packet_count_matches_ffprobe() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg/ffprobe not on PATH — skipping ffmpeg oracle test");
        return;
    }
    let path = temp_path("single-fragment.mp4");
    generate_single_moof(&path);
    let _g = scopeguard::ScopeGuard::with_value(path.clone(), |p| {
        let _ = std::fs::remove_file(p);
    });

    let bytes = std::fs::read(&path).expect("read fixture");
    let want = nb_packets(&path);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open ffmpeg-emitted fragmented mp4");

    assert!(
        d.is_fragmented(),
        "ffmpeg fragment output must be classified fragmented"
    );
    // For each video stream, count packets we can extract.
    let mut got = 0usize;
    while let Ok(_pkt) = d.next_packet() {
        got += 1;
    }
    assert_eq!(
        got, want,
        "demuxer's emitted packet count must match ffprobe's nb_read_packets"
    );
}

#[test]
fn ffmpeg_multi_fragment_packet_count_matches_ffprobe() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg/ffprobe not on PATH — skipping ffmpeg oracle test");
        return;
    }
    let path = temp_path("multi-fragment.mp4");
    generate_multi_moof(&path);
    let _g = scopeguard::ScopeGuard::with_value(path.clone(), |p| {
        let _ = std::fs::remove_file(p);
    });

    let bytes = std::fs::read(&path).expect("read fixture");
    let want = nb_packets(&path);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open ffmpeg multi-moof");

    assert!(d.is_fragmented());
    // With -frag_duration ≪ duration, we expect multiple moofs:
    assert!(
        d.fragment_sequence_numbers.len() > 1,
        "multi-fragment fixture should produce more than one moof, got {}",
        d.fragment_sequence_numbers.len()
    );
    // Sequence numbers must be monotonically increasing.
    for w in d.fragment_sequence_numbers.windows(2) {
        assert!(w[0] < w[1], "mfhd sequence_number must increase");
    }
    let mut got = 0usize;
    while let Ok(_pkt) = d.next_packet() {
        got += 1;
    }
    assert_eq!(
        got, want,
        "demuxer's emitted packet count must match ffprobe's nb_read_packets"
    );
}

// Lightweight in-tree scope-guard so we don't pull a dev-dep just for
// temp-file cleanup. Mirrors the surface of `scopeguard::ScopeGuard`.
mod scopeguard {
    pub struct ScopeGuard<T, F>
    where
        F: FnMut(T),
    {
        value: Option<T>,
        drop: F,
    }
    impl<T, F: FnMut(T)> ScopeGuard<T, F> {
        pub fn with_value(value: T, drop: F) -> Self {
            Self {
                value: Some(value),
                drop,
            }
        }
    }
    impl<T, F: FnMut(T)> Drop for ScopeGuard<T, F> {
        fn drop(&mut self) {
            if let Some(v) = self.value.take() {
                (self.drop)(v);
            }
        }
    }
}
