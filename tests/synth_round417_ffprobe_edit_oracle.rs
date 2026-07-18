//! Round 417 — ffprobe black-box parity for edit-list timelines,
//! including a **priming/delay** fixture and a **dwell** fixture
//! (QTFF pp. 46–48).
//!
//! `ffprobe` (an opaque validator binary) applies edit lists while
//! demuxing: an initial empty edit delays every timestamp and a head
//! trim shifts them toward zero. Empirically (probed against the
//! installed build) it emits only the *presented* samples for
//! all-keyframe media — never-presented head/tail samples are dropped
//! outright — so the oracle contract here is: our applied-mode
//! **presented** (non-discard) pts sequence must equal the oracle's
//! complete packet list one-to-one, and our round-417 discard
//! emission must account for exactly the samples the oracle dropped.
//! The dwell fixture is asserted at the level the oracle supports
//! (container accepted, every sample survives) since the installed
//! build plays a dwell on the raw media timeline; our own dwell
//! contract is pinned in-tree. Tests skip silently when `ffprobe` is
//! not on `$PATH` (e.g. workspace CI).

#![cfg(feature = "registry")]

use std::io::Cursor;
use std::process::Command;

use oxideav_core::{Demuxer, Packet, ReadSeek};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxEdit, MuxSample, MuxTrackKind};

fn ffprobe_available() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Keyframe-only uncompressed `raw ` 8×8 rgb24 video samples (192
/// bytes each) so the oracle's codec probing can't interfere with
/// container-timeline observations.
fn raw_video_samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![i as u8; 192],
            duration: 100,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn raw_video_kind() -> MuxTrackKind {
    MuxTrackKind::Video {
        format: *b"raw ",
        width: 8,
        height: 8,
    }
}

fn build_movie(edits: &[MuxEdit], n: usize) -> Vec<u8> {
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(raw_video_kind(), 1000, raw_video_samples(n), &[]);
    m.set_edit_list(tid, edits).unwrap();
    m.encode_to_vec().unwrap()
}

fn write_temp(bytes: &[u8], tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "oxideav-mov-r417-{tag}-{}.mov",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, bytes).unwrap();
    path
}

fn ffprobe_packet_pts(path: &std::path::Path) -> Vec<i64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=pts",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        out.status.success(),
        "ffprobe failed on {path:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse::<i64>().ok())
        .collect()
}

fn drain(d: &mut MovDemuxer) -> Vec<Packet> {
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push(p);
    }
    out
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

/// Parity core: our applied-mode **presented** pts sequence must
/// match the oracle's complete packet list one-to-one, and the
/// discard emission must account for exactly the samples the oracle
/// dropped (the oracle emits presented samples only for all-keyframe
/// media).
fn assert_presented_pts_parity(edits: &[MuxEdit], n: usize, tag: &str) {
    let bytes = build_movie(edits, n);
    let path = write_temp(&bytes, tag);
    let oracle = ffprobe_packet_pts(&path);
    std::fs::remove_file(&path).ok();

    let mut d = open(bytes);
    d.apply_edit_lists(true);
    d.emit_never_presented(true);
    let pkts = drain(&mut d);
    let presented: Vec<i64> = pkts
        .iter()
        .filter(|p| !p.is_discard())
        .filter_map(|p| p.pts)
        .collect();
    assert_eq!(
        presented, oracle,
        "{tag}: presented applied-mode pts must match ffprobe"
    );
    // Nothing lost: discard packets are exactly the oracle's dropped
    // samples.
    assert_eq!(
        pkts.len(),
        n,
        "{tag}: discard emission must surface every sample"
    );
    assert_eq!(
        pkts.iter().filter(|p| p.is_discard()).count(),
        n - oracle.len(),
        "{tag}: discard count must equal the oracle's dropped-sample count"
    );
}

#[test]
fn ffprobe_full_parity_on_priming_delay_fixture() {
    if !ffprobe_available() {
        return;
    }
    // Priming/delay fixture: 300-tick start delay (empty edit) then
    // media [0, 700) presented — the oracle emits the 7 presented
    // samples delayed to pts 300..900 and drops the 3 tail samples;
    // we surface those discard-flagged.
    assert_presented_pts_parity(
        &[MuxEdit::empty(300), MuxEdit::segment(700, 0)],
        10,
        "priming-delay",
    );
}

#[test]
fn ffprobe_full_parity_on_head_trim_fixture() {
    if !ffprobe_available() {
        return;
    }
    // Head trim: media [200, 1000) presented from movie 0. The
    // oracle drops the two priming samples (all-keyframe media needs
    // no decode priming); we surface them discard-flagged at
    // negative pts.
    assert_presented_pts_parity(&[MuxEdit::segment(800, 200)], 10, "head-trim");
}

#[test]
fn ffprobe_full_parity_on_delay_plus_trim_fixture() {
    if !ffprobe_available() {
        return;
    }
    // Combined: 200-tick delay + head trim at media 300.
    assert_presented_pts_parity(
        &[MuxEdit::empty(200), MuxEdit::segment(700, 300)],
        10,
        "delay-plus-trim",
    );
}

#[test]
fn ffprobe_accepts_dwell_fixture_and_all_samples_survive() {
    if !ffprobe_available() {
        return;
    }
    // Dwell fixture: hold sample 0 for 600 movie ticks. The installed
    // oracle build does not model the dwell (it plays the fixture on
    // the raw media timeline: 4 packets at pts 0..300), so the
    // black-box assertions stay at the level it supports: the
    // container is accepted and every sample survives demuxing.
    let bytes = build_movie(&[MuxEdit::dwell(600, 0)], 4);
    let path = write_temp(&bytes, "dwell");
    let oracle = ffprobe_packet_pts(&path);
    std::fs::remove_file(&path).ok();
    assert_eq!(
        oracle.len(),
        4,
        "oracle must surface all 4 samples of the dwell fixture"
    );

    // Our own dwell contract on the same fixture (QTFF p. 48 rate-0
    // hold / ISO/IEC 14496-12 §8.6.6.3): the held sample stretches
    // across the whole dwell window; the other samples are
    // never-presented and have no presenting segment to extrapolate
    // against, so they stay dropped even with discard emission on.
    let mut d = open(bytes);
    d.apply_edit_lists(true);
    d.emit_never_presented(true);
    let pkts = drain(&mut d);
    assert_eq!(pkts.len(), 1);
    assert!(!pkts[0].is_discard());
    assert_eq!(pkts[0].pts, Some(0));
    assert_eq!(pkts[0].duration, Some(600));
}
