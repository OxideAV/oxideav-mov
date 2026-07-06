//! Round 394 — **applied** edit-list packet timing (QTFF Chapter 2
//! "Edit Atoms" pp. 46–48 / ISO/IEC 14496-12 §8.6.6).
//!
//! The demuxer has long *parsed* edit lists and exposed the
//! media↔movie PTS mappers (`movie_pts_for` / `media_pts_for`), but
//! `next_packet()` always kept the raw media-timeline contract. Round
//! 394 adds the opt-in `MovDemuxer::apply_edit_lists(true)` mode:
//! packets then carry **edited-timeline** timestamps (still in the
//! stream's media timescale), samples outside every edit segment are
//! dropped, non-unity `media_rate` segments scale spacing/durations,
//! dwell edits stretch their held sample, and head empty edits delay
//! every timestamp.
//!
//! Every test builds a movie through [`MovMuxer`] (`set_edit_list`)
//! and re-opens it through [`MovDemuxer`], asserting the emitted
//! packet timing — a full write→read black-box loop.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, Packet, ReadSeek};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxEdit, MuxSample, MuxTrackKind};

/// `n` uniform samples of `dur` media ticks each, all keyframes.
fn uniform_samples(n: usize, dur: u32) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![i as u8; 4],
            duration: dur,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn audio_kind() -> MuxTrackKind {
    MuxTrackKind::Audio {
        format: *b"lpcm",
        channels: 2,
        bits_per_sample: 16,
        sample_rate: 48000,
    }
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn drain(d: &mut MovDemuxer) -> Vec<Packet> {
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push(p);
    }
    out
}

/// Build a single-track movie (movie timescale == media timescale ==
/// 1000 unless overridden) with the given edits, and drain it in
/// applied-edit-list mode.
fn drain_edited(
    edits: &[MuxEdit],
    samples: Vec<MuxSample>,
    movie_ts: u32,
    media_ts: u32,
) -> Vec<Packet> {
    let mut m = MovMuxer::new().with_movie_timescale(movie_ts);
    let tid = m.add_track(audio_kind(), media_ts, samples, &[]);
    if !edits.is_empty() {
        m.set_edit_list(tid, edits).expect("set_edit_list");
    }
    let bytes = m.encode_to_vec().expect("encode");
    let mut d = open(bytes);
    d.apply_edit_lists(true);
    assert!(d.edit_lists_applied());
    drain(&mut d)
}

#[test]
fn no_edit_list_packets_unchanged() {
    // "No edits" rule (QTFF p. 47): the whole media plays from movie
    // time 0 — applied mode must not perturb timing.
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    m.add_track(audio_kind(), 1000, uniform_samples(5, 100), &[]);
    let bytes = m.encode_to_vec().unwrap();

    let mut plain = open(bytes.clone());
    let plain_pkts = drain(&mut plain);
    let mut edited = open(bytes);
    edited.apply_edit_lists(true);
    let edited_pkts = drain(&mut edited);

    assert_eq!(plain_pkts.len(), edited_pkts.len());
    for (a, b) in plain_pkts.iter().zip(edited_pkts.iter()) {
        assert_eq!(a.pts, b.pts);
        assert_eq!(a.dts, b.dts);
        assert_eq!(a.duration, b.duration);
    }
}

#[test]
fn trim_edit_drops_head_and_shifts_pts() {
    // segment(track_duration=800 movie ticks, media_time=200): the
    // edited presentation is media [200, 1000) played at movie 0.
    // Samples 0/1 (pts 0/100) are outside → dropped; sample 2
    // (pts 200) presents at edited pts 0.
    let pkts = drain_edited(
        &[MuxEdit::segment(800, 200)],
        uniform_samples(10, 100),
        1000,
        1000,
    );
    assert_eq!(pkts.len(), 8, "two priming samples must be dropped");
    for (i, p) in pkts.iter().enumerate() {
        assert_eq!(p.pts, Some(i as i64 * 100));
        assert_eq!(p.dts, Some(i as i64 * 100));
        assert_eq!(p.duration, Some(100));
    }
}

#[test]
fn head_empty_edit_delays_timestamps() {
    // empty(500) then segment(500, 0): media plays from movie tick
    // 500. Every edited pts is the media pts plus the 500-tick delay.
    let pkts = drain_edited(
        &[MuxEdit::empty(500), MuxEdit::segment(500, 0)],
        uniform_samples(5, 100),
        1000,
        1000,
    );
    assert_eq!(pkts.len(), 5);
    for (i, p) in pkts.iter().enumerate() {
        assert_eq!(p.pts, Some(500 + i as i64 * 100));
        assert_eq!(p.duration, Some(100));
    }
}

#[test]
fn head_empty_edit_rescales_across_timescales() {
    // Movie timescale 600 vs media timescale 1000: a 300-movie-tick
    // empty edit is a 500-media-tick delay on the edited timeline.
    let pkts = drain_edited(
        &[MuxEdit::empty(300), MuxEdit::segment(300, 0)],
        uniform_samples(5, 100),
        600,
        1000,
    );
    assert_eq!(pkts.len(), 5);
    assert_eq!(pkts[0].pts, Some(500));
    assert_eq!(pkts[4].pts, Some(900));
}

#[test]
fn rate_two_segment_halves_spacing_and_duration() {
    // One segment at rate 2.0 consuming 1000 media ticks in 500 movie
    // ticks (QTFF pp. 226–227 consumption model): presentation
    // spacing and durations halve.
    let pkts = drain_edited(
        &[MuxEdit {
            track_duration: 500,
            media_time: 0,
            media_rate: 0x0002_0000,
        }],
        uniform_samples(10, 100),
        1000,
        1000,
    );
    assert_eq!(pkts.len(), 10);
    for (i, p) in pkts.iter().enumerate() {
        assert_eq!(p.pts, Some(i as i64 * 50));
        assert_eq!(p.duration, Some(50));
    }
}

#[test]
fn dwell_segment_stretches_held_sample() {
    // segment(200, 0) then a dwell (rate 0) holding media tick 200
    // for 300 movie ticks. Samples 0/1 play normally; sample 2
    // (pts 200) is held across the 300-tick window; samples 3+ are
    // not presented by any segment → dropped.
    let pkts = drain_edited(
        &[
            MuxEdit::segment(200, 0),
            MuxEdit {
                track_duration: 300,
                media_time: 200,
                media_rate: 0,
            },
        ],
        uniform_samples(5, 100),
        1000,
        1000,
    );
    assert_eq!(pkts.len(), 3);
    assert_eq!(pkts[0].pts, Some(0));
    assert_eq!(pkts[1].pts, Some(100));
    assert_eq!(pkts[2].pts, Some(200));
    assert_eq!(pkts[2].duration, Some(300), "dwell spans its window");
}

#[test]
fn trailing_partial_sample_duration_clamped() {
    // segment(750, 200): media [200, 950). The last kept sample
    // (pts 900, natural end 1000) is trimmed mid-sample → its edited
    // duration shrinks to 50.
    let pkts = drain_edited(
        &[MuxEdit::segment(750, 200)],
        uniform_samples(10, 100),
        1000,
        1000,
    );
    assert_eq!(pkts.len(), 8);
    let last = pkts.last().unwrap();
    assert_eq!(last.pts, Some(700));
    assert_eq!(last.duration, Some(50));
}

#[test]
fn mid_sample_edit_start_drops_partial_head_sample() {
    // media_time=250 lands mid-sample-2: membership is keyed on the
    // sample's presentation timestamp, so sample 2 (pts 200 < 250) is
    // dropped and sample 3 (pts 300) presents at edited pts 50.
    let pkts = drain_edited(
        &[MuxEdit::segment(750, 250)],
        uniform_samples(10, 100),
        1000,
        1000,
    );
    assert_eq!(pkts[0].pts, Some(50));
    assert_eq!(pkts[0].dts, Some(50));
    assert_eq!(pkts.len(), 7);
}

#[test]
fn bframe_composition_offsets_survive_trim() {
    // Video-style track carrying a uniform +100 ctts composition
    // offset (dts 0..500, pts 100..600). A trim edit presenting media
    // [200, 700) drops the first sample (pts 100) and shifts every
    // kept pts/dts by −200 while preserving the offset.
    let samples: Vec<MuxSample> = (0..6)
        .map(|i| MuxSample {
            data: vec![i as u8; 4],
            duration: 100,
            keyframe: i == 0,
            composition_offset: 100,
        })
        .collect();
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 64,
            height: 64,
        },
        1000,
        samples,
        &[],
    );
    // Media pts run 100..700; present [200, 700) at movie 0.
    m.set_edit_list(tid, &[MuxEdit::segment(500, 200)]).unwrap();
    let bytes = m.encode_to_vec().unwrap();
    let mut d = open(bytes);
    d.apply_edit_lists(true);
    let pkts = drain(&mut d);
    // Samples with media pts 200,300,400,500,600 are kept (pts 100 is
    // outside; media pts 700 ≥ segment end 700 is outside).
    assert_eq!(pkts.len(), 5);
    for (i, p) in pkts.iter().enumerate() {
        let pts = i as i64 * 100;
        assert_eq!(p.pts, Some(pts));
        assert_eq!(p.dts, Some(pts - 100), "composition offset preserved");
    }
}

#[test]
fn helper_matches_emitted_packets() {
    // `edited_timing_for` (usable without enabling the mode) must
    // agree with what applied-mode packets carry.
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(audio_kind(), 1000, uniform_samples(6, 100), &[]);
    m.set_edit_list(tid, &[MuxEdit::empty(250), MuxEdit::segment(400, 100)])
        .unwrap();
    let bytes = m.encode_to_vec().unwrap();

    let plain = open(bytes.clone());
    let mut expected = Vec::new();
    for s in plain.tracks[0].sample_table.iter_samples() {
        let s = s.unwrap();
        if let Some(t) = plain.edited_timing_for(0, &s) {
            expected.push(t);
        }
    }

    let mut edited = open(bytes);
    edited.apply_edit_lists(true);
    let pkts = drain(&mut edited);
    assert_eq!(pkts.len(), expected.len());
    assert!(!pkts.is_empty());
    for (p, t) in pkts.iter().zip(expected.iter()) {
        assert_eq!(p.pts, Some(t.pts));
        assert_eq!(p.dts, Some(t.dts));
    }
}

#[test]
fn mode_can_be_disabled_again() {
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(audio_kind(), 1000, uniform_samples(4, 100), &[]);
    m.set_edit_list(tid, &[MuxEdit::segment(200, 200)]).unwrap();
    let bytes = m.encode_to_vec().unwrap();
    let mut d = open(bytes);
    d.apply_edit_lists(true);
    d.apply_edit_lists(false);
    assert!(!d.edit_lists_applied());
    let pkts = drain(&mut d);
    // Default contract: all 4 samples, raw media timestamps.
    assert_eq!(pkts.len(), 4);
    assert_eq!(pkts[0].pts, Some(0));
    assert_eq!(pkts[3].pts, Some(300));
}

/// Black-box oracle: `ffprobe` (when present on `$PATH`) applies edit
/// lists while demuxing, so its packet pts list for a trimmed movie
/// must match our applied-mode output. Skips silently when the binary
/// is unavailable (e.g. workspace CI).
#[test]
fn ffprobe_oracle_agrees_on_trimmed_timeline() {
    use std::process::Command;
    let available = Command::new("ffprobe")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !available {
        return;
    }

    // Keyframe-only video track (every sample sync) so the oracle's
    // decode-dependency handling can't diverge from presentation
    // membership: dts 0..900, trim edit presents media [200, 1000).
    // Uncompressed `raw ` 8×8 rgb24 frames keep the oracle's codec
    // probing happy (192 bytes = 8×8×3 per sample).
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let samples: Vec<MuxSample> = (0..10)
        .map(|i| MuxSample {
            data: vec![i as u8; 192],
            duration: 100,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    let tid = m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 8,
            height: 8,
        },
        1000,
        samples,
        &[],
    );
    m.set_edit_list(tid, &[MuxEdit::segment(800, 200)]).unwrap();
    let bytes = m.encode_to_vec().unwrap();

    let path = std::env::temp_dir().join(format!(
        "oxideav-mov-r394-elst-{}.mov",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &bytes).unwrap();
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=pts",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .expect("run ffprobe");
    assert!(
        out.status.success(),
        "ffprobe failed on {path:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_file(&path).ok();
    let oracle_pts: Vec<i64> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse::<i64>().ok())
        .filter(|&v| v >= 0)
        .collect();

    let mut d = open(bytes);
    d.apply_edit_lists(true);
    let ours: Vec<i64> = drain(&mut d).iter().filter_map(|p| p.pts).collect();
    assert_eq!(ours, oracle_pts, "applied-edit pts must match ffprobe");
}

#[test]
fn multi_track_edits_apply_independently() {
    // Track A trims 200 ticks; track B has no edits. Applied mode
    // must edit A's packets while leaving B untouched.
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let a = m.add_track(audio_kind(), 1000, uniform_samples(6, 100), &[]);
    let _b = m.add_track(audio_kind(), 1000, uniform_samples(6, 100), &[]);
    m.set_edit_list(a, &[MuxEdit::segment(400, 200)]).unwrap();
    let bytes = m.encode_to_vec().unwrap();
    let mut d = open(bytes);
    d.apply_edit_lists(true);
    let pkts = drain(&mut d);
    let a_pkts: Vec<&Packet> = pkts.iter().filter(|p| p.stream_index == 0).collect();
    let b_pkts: Vec<&Packet> = pkts.iter().filter(|p| p.stream_index == 1).collect();
    assert_eq!(a_pkts.len(), 4, "media [200,600) keeps 4 samples");
    assert_eq!(a_pkts[0].pts, Some(0));
    assert_eq!(b_pkts.len(), 6);
    assert_eq!(b_pkts[0].pts, Some(0));
    assert_eq!(b_pkts[5].pts, Some(500));
}
