//! Round 417 — **discard-flagged never-presented media** in the
//! applied edit-list mode (QTFF Chapter 2 "Edit Atoms" pp. 46–48).
//!
//! The edit list describes what is *presented*; media outside every
//! segment is still often *required for decoding* — the sync sample a
//! head-trimmed segment's first presented frame depends on, or the
//! encoder-priming audio an empty edit skips. The applied mode alone
//! drops those samples; the new opt-in
//! `MovDemuxer::emit_never_presented(true)` emits them with the packet
//! **discard flag** set and timing extrapolated from the nearest
//! presenting segment (head-trimmed media lands at negative edited
//! pts — it decodes before the presentation starts).
//!
//! Every test builds a movie through [`MovMuxer`] (`set_edit_list`)
//! and re-opens it through [`MovDemuxer`] — a full write→read loop.

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

/// Build a single-track movie with the given edits and drain it in
/// applied-edit-list mode with discard emission enabled.
fn drain_with_discards(edits: &[MuxEdit], samples: Vec<MuxSample>) -> Vec<Packet> {
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(audio_kind(), 1000, samples, &[]);
    m.set_edit_list(tid, edits).expect("set_edit_list");
    let bytes = m.encode_to_vec().expect("encode");
    let mut d = open(bytes);
    d.apply_edit_lists(true);
    d.emit_never_presented(true);
    assert!(d.never_presented_emitted());
    drain(&mut d)
}

#[test]
fn head_trim_emits_priming_samples_discard_flagged_at_negative_pts() {
    // segment(800, 200): presented media is [200, 1000). Samples 0/1
    // (media pts 0/100) are decode-only priming: emitted first,
    // discard-flagged, at extrapolated pts -200 / -100.
    let pkts = drain_with_discards(&[MuxEdit::segment(800, 200)], uniform_samples(10, 100));
    assert_eq!(pkts.len(), 10, "nothing may be silently dropped");
    assert!(pkts[0].is_discard());
    assert_eq!(pkts[0].pts, Some(-200));
    assert!(pkts[1].is_discard());
    assert_eq!(pkts[1].pts, Some(-100));
    for (i, p) in pkts[2..].iter().enumerate() {
        assert!(!p.is_discard(), "presented packet {i} must not be discard");
        assert_eq!(p.pts, Some(i as i64 * 100));
        assert_eq!(p.duration, Some(100));
    }
    // dts strictly monotone across the discard→presented boundary.
    let dts: Vec<i64> = pkts.iter().map(|p| p.dts.unwrap()).collect();
    assert!(
        dts.windows(2).all(|w| w[0] < w[1]),
        "dts not monotone: {dts:?}"
    );
}

#[test]
fn tail_trim_emits_trailing_samples_discard_flagged_past_end() {
    // segment(500, 0): presented media is [0, 500). Samples 5..9 are
    // never presented; they extrapolate past the presentation end.
    let pkts = drain_with_discards(&[MuxEdit::segment(500, 0)], uniform_samples(10, 100));
    assert_eq!(pkts.len(), 10);
    for (i, p) in pkts.iter().enumerate() {
        assert_eq!(p.pts, Some(i as i64 * 100));
        assert_eq!(p.is_discard(), i >= 5, "packet {i} discard flag");
    }
}

#[test]
fn empty_edit_delay_plus_trim_extrapolates_from_shifted_segment() {
    // empty(300) + segment(700, 300): presentation starts at edited
    // tick 300 showing media [300, 1000). Head samples 0..2 are
    // discard-flagged and extrapolate against the shifted segment:
    // media 0 → 300 + (0 - 300) = 0… i.e. the priming run leads
    // straight into the presented run on the edited timeline.
    let pkts = drain_with_discards(
        &[MuxEdit::empty(300), MuxEdit::segment(700, 300)],
        uniform_samples(10, 100),
    );
    assert_eq!(pkts.len(), 10);
    for (i, p) in pkts.iter().enumerate() {
        assert_eq!(p.pts, Some(i as i64 * 100), "packet {i}");
        assert_eq!(p.is_discard(), i < 3, "packet {i} discard flag");
    }
}

#[test]
fn dwell_only_list_still_drops_unpresented_samples() {
    // A dwell-only list holds one frame; there is no presenting Media
    // segment to extrapolate against, so the other samples stay
    // dropped even with discard emission enabled.
    let pkts = drain_with_discards(
        &[MuxEdit {
            track_duration: 600,
            media_time: 0,
            media_rate: 0,
        }],
        uniform_samples(4, 100),
    );
    assert_eq!(pkts.len(), 1, "only the held sample is emitted");
    assert!(!pkts[0].is_discard());
    assert_eq!(pkts[0].pts, Some(0));
    assert_eq!(pkts[0].duration, Some(600));
}

#[test]
fn mode_off_keeps_dropping_and_no_discard_flags() {
    // Applied mode without emit_never_presented: historical contract.
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(audio_kind(), 1000, uniform_samples(10, 100), &[]);
    m.set_edit_list(tid, &[MuxEdit::segment(800, 200)]).unwrap();
    let bytes = m.encode_to_vec().unwrap();
    let mut d = open(bytes);
    d.apply_edit_lists(true);
    assert!(!d.never_presented_emitted());
    let pkts = drain(&mut d);
    assert_eq!(pkts.len(), 8);
    assert!(pkts.iter().all(|p| !p.is_discard()));
}

#[test]
fn raw_media_mode_ignores_discard_switch() {
    // Without apply_edit_lists the raw media timeline emits
    // everything unflagged; the discard switch changes nothing.
    let mut m = MovMuxer::new().with_movie_timescale(1000);
    let tid = m.add_track(audio_kind(), 1000, uniform_samples(6, 100), &[]);
    m.set_edit_list(tid, &[MuxEdit::segment(300, 300)]).unwrap();
    let bytes = m.encode_to_vec().unwrap();
    let mut d = open(bytes);
    d.emit_never_presented(true);
    let pkts = drain(&mut d);
    assert_eq!(pkts.len(), 6);
    assert!(pkts.iter().all(|p| !p.is_discard()));
    assert_eq!(pkts[0].pts, Some(0));
}

#[test]
fn seek_reports_discard_dts_when_landing_on_never_presented_sync() {
    // Video track, keyframe only on sample 0, presented media is
    // [700, 1000) (segment(300, 700)). Seeking to edited pts 0
    // resolves to media 700, but the sync snap lands on sample 0 —
    // which no edit presents. With discard emission on, that sample
    // IS the next packet: seek must report its extrapolated dts
    // (0 + (0 - 700) = -700). With it off, the first *presented*
    // sample's dts (0) is reported instead.
    let samples: Vec<MuxSample> = (0..10)
        .map(|i| MuxSample {
            data: vec![i as u8; 4],
            duration: 100,
            keyframe: i == 0,
            composition_offset: 0,
        })
        .collect();
    let build = || {
        let mut m = MovMuxer::new().with_movie_timescale(1000);
        let tid = m.add_track(
            MuxTrackKind::Video {
                format: *b"avc1",
                width: 64,
                height: 48,
            },
            1000,
            samples.clone(),
            &[],
        );
        m.set_edit_list(tid, &[MuxEdit::segment(300, 700)]).unwrap();
        m.encode_to_vec().unwrap()
    };

    let mut d = open(build());
    d.apply_edit_lists(true);
    d.emit_never_presented(true);
    let landed = d.seek_to(0, 0).expect("seek");
    assert_eq!(landed, -700);
    let first = d.next_packet().expect("first packet after seek");
    assert_eq!(first.dts, Some(-700));
    assert!(first.is_discard());

    let mut d = open(build());
    d.apply_edit_lists(true);
    let landed = d.seek_to(0, 0).expect("seek");
    assert_eq!(landed, 0);
    let first = d.next_packet().expect("first packet after seek");
    assert_eq!(first.dts, Some(0));
    assert!(!first.is_discard());
}
