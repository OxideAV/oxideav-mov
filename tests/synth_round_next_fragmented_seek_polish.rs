//! Fragmented-MP4 seek polish — three correctness edge cases that
//! surface in real-world DASH/CMAF streams. Companion to round-21
//! (`tests/round_next_fragmented_seek.rs`) which covers the basic
//! ffmpeg-fixture case; this round handcrafts synthetic in-memory
//! containers to exercise the corner cases that one-track / one-trex
//! / zero-`tfdt` ffmpeg fixtures don't reach.
//!
//! Each fixture is a single self-contained byte vector. No `tests/
//! fixtures/` files are added.
//!
//! ── Edge cases covered ──
//!
//! 1. **Multi-`trex` per fragment.** ISO/IEC 14496-12 §8.8.3 places
//!    one `trex` per track inside `moov/mvex`. A `moof` may carry one
//!    `traf` per track, each of which must consume the matching `trex`
//!    (looked up by `track_ID`). The `synth_two_trex_two_traf_per_moof`
//!    builder emits a video track (tid=1) with default duration 100,
//!    default size 200 + an audio track (tid=2) with default duration
//!    1024, default size 64. Both trafs intentionally omit the
//!    per-fragment overrides so the cascade has to consult the
//!    matching `trex`. If the demuxer used the *first* trex for both
//!    trafs, audio samples would inherit video's defaults (dur=100,
//!    sz=200) and the assertion would fail.
//!
//! 2. **Negative `composition_time_offset` in `trun` (v=1).**
//!    §8.8.8.2 declares per-sample `sample_composition_time_offset`
//!    as `signed int(32)` when the trun's FullBox `version == 1`.
//!    The demuxer must thread the negative value through to
//!    `SampleEntry.composition_offset` so `pts() = dts + offset` can
//!    legitimately compute a *smaller* PTS than DTS for B-frame
//!    samples in DASH segments.
//!
//! 3. **Non-zero baseline `tfdt`.** §8.8.12 says
//!    `baseMediaDecodeTime` is the absolute decode time of the *first*
//!    sample of the track fragment. When seeking into a fragment whose
//!    `tfdt > 0`, the demuxer must offset the per-sample DTS by `tfdt`
//!    rather than restart from zero. The two-fragment fixture below
//!    declares fragment 1 at tfdt=0 (5 samples × 60000 ticks) and
//!    fragment 2 at tfdt=300000 (5 sec at 60kHz; 5 more samples ×
//!    60000 ticks). Seek to pts=420000 (7 s) must land on the first
//!    sample of fragment 2 (pts=300000), not the last sample of
//!    fragment 1 (pts=240000).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{
    MovDemuxer, TFHD_DEFAULT_BASE_IS_MOOF, TRUN_DATA_OFFSET_PRESENT,
    TRUN_FIRST_SAMPLE_FLAGS_PRESENT, TRUN_SAMPLE_CTS_OFFSET_PRESENT, TRUN_SAMPLE_SIZE_PRESENT,
};

// ─────────────────── shared synth helpers (multi-track) ───────────────────

fn build_trex(
    track_id: u32,
    default_sample_description_index: u32,
    default_sample_duration: u32,
    default_sample_size: u32,
    default_sample_flags: u32,
) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&track_id.to_be_bytes());
    p.extend_from_slice(&default_sample_description_index.to_be_bytes());
    p.extend_from_slice(&default_sample_duration.to_be_bytes());
    p.extend_from_slice(&default_sample_size.to_be_bytes());
    p.extend_from_slice(&default_sample_flags.to_be_bytes());
    p
}

fn build_mfhd(seq: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p
}

fn build_tfhd_dbim(track_id: u32) -> Vec<u8> {
    // default-base-is-moof, no overrides — cascade to trex.
    let flags = TFHD_DEFAULT_BASE_IS_MOOF;
    let mut p = Vec::new();
    p.extend_from_slice(&flags.to_be_bytes());
    p.extend_from_slice(&track_id.to_be_bytes());
    p
}

fn build_empty_table_payloads() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut stts = Vec::new();
    stts.extend_from_slice(&0u32.to_be_bytes());
    stts.extend_from_slice(&0u32.to_be_bytes());
    let mut stsc = Vec::new();
    stsc.extend_from_slice(&0u32.to_be_bytes());
    stsc.extend_from_slice(&0u32.to_be_bytes());
    let mut stsz = Vec::new();
    stsz.extend_from_slice(&0u32.to_be_bytes());
    stsz.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0 (table-follows)
    stsz.extend_from_slice(&0u32.to_be_bytes()); // count = 0
    let mut stco = Vec::new();
    stco.extend_from_slice(&0u32.to_be_bytes());
    stco.extend_from_slice(&0u32.to_be_bytes());
    (stts, stsc, stsz, stco)
}

fn build_trak_video(track_id: u32, w_px: u32, h_px: u32, media_ts: u32) -> Vec<u8> {
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(track_id, 0, w_px, h_px));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(media_ts, 0));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", w_px as u16, h_px as u16, &[]),
    );
    let (stts, stsc, stsz, stco) = build_empty_table_payloads();
    push_atom(&mut stbl, *b"stts", &stts);
    push_atom(&mut stbl, *b"stsc", &stsc);
    push_atom(&mut stbl, *b"stsz", &stsz);
    push_atom(&mut stbl, *b"stco", &stco);
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    trak
}

fn build_trak_audio(track_id: u32, channels: u16, bits: u16, sample_rate: u32) -> Vec<u8> {
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(track_id, 0, 0, 0));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(sample_rate, 0));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
    let mut minf = Vec::new();
    // smhd — 8-byte sound media header (4 ver+flags + 2 balance + 2 reserved).
    push_atom(&mut minf, *b"smhd", &[0u8; 8]);
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_audio(b"mp4a", channels, bits, sample_rate, &[]),
    );
    let (stts, stsc, stsz, stco) = build_empty_table_payloads();
    push_atom(&mut stbl, *b"stts", &stts);
    push_atom(&mut stbl, *b"stsc", &stsc);
    push_atom(&mut stbl, *b"stsz", &stsz);
    push_atom(&mut stbl, *b"stco", &stco);
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    trak
}

fn ftyp_iso5() -> Vec<u8> {
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(b"mp42");
    ftyp
}

// ───────────────────── Goal 1: multi-trex per moof ─────────────────────

/// Build a fixture with TWO tracks (video tid=1, audio tid=2), each
/// with its own `trex` declaring distinct defaults. A single `moof`
/// carries one `traf` per track; both `tfhd`s set only
/// `default-base-is-moof` and *omit* every other override so the
/// cascade falls to the matching `trex`.
///
/// The mdat layout places the video samples first, then the audio
/// samples, so each traf's data_offset can be computed deterministically.
fn build_multi_trex_two_traf_fixture() -> Vec<u8> {
    let video_dur: u32 = 100;
    let video_sz: u32 = 200;
    let audio_dur: u32 = 1024;
    let audio_sz: u32 = 64;
    let video_count: u32 = 5;
    let audio_count: u32 = 5;

    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_iso5());

    // moov with both trak + mvex/trex
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));
    push_atom(&mut moov, *b"trak", &build_trak_video(1, 320, 240, 600));
    push_atom(&mut moov, *b"trak", &build_trak_audio(2, 2, 16, 48000));

    let mut mvex = Vec::new();
    // trex 1 (video): dur=100, sz=200, sync (flags=0)
    push_atom(
        &mut mvex,
        *b"trex",
        &build_trex(1, 1, video_dur, video_sz, 0),
    );
    // trex 2 (audio): dur=1024, sz=64, sync (audio is implicitly sync)
    push_atom(
        &mut mvex,
        *b"trex",
        &build_trex(2, 1, audio_dur, audio_sz, 0),
    );
    push_atom(&mut moov, *b"mvex", &mvex);
    push_atom(&mut out, *b"moov", &moov);

    // moof: one traf for video, one for audio. Both tfhd carry only
    // default-base-is-moof; both truns carry only data_offset (no
    // per-sample fields). Sample counts come from cascade defaults.
    let tfhd_v = build_tfhd_dbim(1);
    let tfhd_a = build_tfhd_dbim(2);

    // trun payload size: ver_flags(4) + sample_count(4) + data_offset(4) = 12
    // box header(8) + payload(12) = 20
    // tfhd box: 8 + tfhd_v.len()
    // traf box: 8 + tfhd_box + trun_box = 8 + (8 + tfhd_v.len()) + 20
    // mfhd box: 8 + 8 = 16
    // moof: 8 + mfhd_box + traf_box_v + traf_box_a
    let trun_payload_len = 12u64;
    let trun_box_len = 8 + trun_payload_len;
    let tfhd_box_len = 8 + tfhd_v.len() as u64;
    let traf_box_len = 8 + tfhd_box_len + trun_box_len;
    let mfhd_box_len = 16u64;
    let moof_size = 8 + mfhd_box_len + 2 * traf_box_len;

    // mdat layout: [video samples (5×200=1000)][audio samples (5×64=320)]
    let video_total = (video_count as u64) * (video_sz as u64);
    let _audio_total = (audio_count as u64) * (audio_sz as u64);

    // data_offset is from the start of the enclosing moof (default-
    // base-is-moof). Video first byte = moof_size + 8 (mdat header).
    let video_data_off = (moof_size + 8) as i32;
    // Audio first byte = video first byte + video_total.
    let audio_data_off = (moof_size + 8 + video_total) as i32;

    fn build_trun_data_only(sample_count: u32, data_offset: i32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&TRUN_DATA_OFFSET_PRESENT.to_be_bytes()); // ver=0+flags
        p.extend_from_slice(&sample_count.to_be_bytes());
        p.extend_from_slice(&data_offset.to_be_bytes());
        p
    }

    let mut traf_v = Vec::new();
    push_atom(&mut traf_v, *b"tfhd", &tfhd_v);
    push_atom(
        &mut traf_v,
        *b"trun",
        &build_trun_data_only(video_count, video_data_off),
    );
    let mut traf_a = Vec::new();
    push_atom(&mut traf_a, *b"tfhd", &tfhd_a);
    push_atom(
        &mut traf_a,
        *b"trun",
        &build_trun_data_only(audio_count, audio_data_off),
    );

    let mut moof = Vec::new();
    push_atom(&mut moof, *b"mfhd", &build_mfhd(1));
    push_atom(&mut moof, *b"traf", &traf_v);
    push_atom(&mut moof, *b"traf", &traf_a);
    let actual_moof_size = (8 + moof.len()) as u64;
    assert_eq!(actual_moof_size, moof_size, "moof size estimate must match");
    push_atom(&mut out, *b"moof", &moof);

    // mdat: video bytes (each video sample fills with 'V'+i), then audio.
    let mut mdat_payload = Vec::new();
    for i in 0..video_count {
        mdat_payload.extend(std::iter::repeat(b'V' + (i as u8)).take(video_sz as usize));
    }
    for i in 0..audio_count {
        mdat_payload.extend(std::iter::repeat(b'a' + (i as u8)).take(audio_sz as usize));
    }
    push_atom(&mut out, *b"mdat", &mdat_payload);
    out
}

#[test]
fn multi_trex_each_traf_consumes_matching_track_id() {
    let bytes = build_multi_trex_two_traf_fixture();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open multi-trex fmp4");
    assert_eq!(d.tracks.len(), 2);
    assert_eq!(d.trex_defaults.len(), 2);
    assert!(d.is_fragmented());

    // Track 0 (video, tid=1): 5 samples, dur=100, sz=200
    let v = &d.tracks[0];
    assert!(v.is_video(), "track 0 must be video");
    assert_eq!(
        v.fragment_samples.len(),
        5,
        "video must materialise 5 fragment samples"
    );
    for (i, s) in v.fragment_samples.iter().enumerate() {
        assert_eq!(s.size, 200, "video sample {i} size from video trex");
        assert_eq!(s.duration, 100, "video sample {i} duration from video trex");
        assert_eq!(s.dts, (i as u64) * 100, "video sample {i} dts");
    }

    // Track 1 (audio, tid=2): 5 samples, dur=1024, sz=64. If the
    // demuxer mistakenly used video's trex, we'd see size=200 here.
    let a = &d.tracks[1];
    assert!(a.is_audio(), "track 1 must be audio");
    assert_eq!(
        a.fragment_samples.len(),
        5,
        "audio must materialise 5 fragment samples"
    );
    for (i, s) in a.fragment_samples.iter().enumerate() {
        assert_eq!(
            s.size, 64,
            "audio sample {i} size must come from AUDIO trex (got {} — looks like video trex defaults bled across)",
            s.size,
        );
        assert_eq!(
            s.duration, 1024,
            "audio sample {i} duration must come from AUDIO trex"
        );
        assert_eq!(s.dts, (i as u64) * 1024, "audio sample {i} dts");
    }
}

// ─────────────── Goal 2: negative composition_time_offset (v=1 trun) ───────────────

/// Build a single-fragment fixture whose `trun` is version=1 with
/// per-sample composition-time offsets, including negative values.
/// The on-disk encoding is `signed int(32)` per §8.8.8.2 (when the
/// trun's FullBox version == 1) — we encode `-100` as the two's-
/// complement bit pattern.
///
/// Sample DTS pattern: 0, 100, 200, 300, 400.
/// Composition offsets: 100, -100, 50, 0, 200.
/// → PTS:               100, 0,    250, 300, 600.
///
/// Note pts[1] < pts[0]: that's the canonical B-frame reorder pattern
/// the v1 signed trun was added to express.
fn build_negative_cts_fixture() -> Vec<u8> {
    let dur: u32 = 100;
    let sz: u32 = 64;
    let count: u32 = 5;
    let cts_offsets: [i32; 5] = [100, -100, 50, 0, 200];

    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_iso5());

    // moov + single video trak + mvex/trex (defaults: dur=100, sz=64,
    // sync flags=0).
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));
    push_atom(&mut moov, *b"trak", &build_trak_video(1, 320, 240, 600));
    let mut mvex = Vec::new();
    push_atom(&mut mvex, *b"trex", &build_trex(1, 1, dur, sz, 0));
    push_atom(&mut moov, *b"mvex", &mvex);
    push_atom(&mut out, *b"moov", &moov);

    // tfhd: default-base-is-moof, no overrides.
    let tfhd = build_tfhd_dbim(1);

    // trun (v=1): per-sample CTS offsets only (sizes / durations come
    // from trex). Flags = TRUN_DATA_OFFSET_PRESENT |
    // TRUN_SAMPLE_CTS_OFFSET_PRESENT.
    // Per-sample row width = 4 bytes (just CTS).
    let tr_flags = TRUN_DATA_OFFSET_PRESENT | TRUN_SAMPLE_CTS_OFFSET_PRESENT;
    // Sizing: tfhd box(8 + 8) = 16, trun box(8 + 4 + 4 + 4 + 5*4) = 40,
    // traf box(8 + 16 + 40) = 64, mfhd box = 16, moof size = 8 + 16 + 64 = 88.
    let trun_payload_len = 4 + 4 + 4 + (count as u64 * 4); // ver+flags + sc + do + rows
    let trun_box_len = 8 + trun_payload_len;
    let tfhd_box_len = 8 + tfhd.len() as u64;
    let traf_box_len = 8 + tfhd_box_len + trun_box_len;
    let mfhd_box_len = 16u64;
    let moof_size = 8 + mfhd_box_len + traf_box_len;
    let data_off = (moof_size + 8) as i32;

    let mut trun_payload = Vec::new();
    // version=1 in MSB, flags in low 24 bits.
    let ver_flags: u32 = (1u32 << 24) | tr_flags;
    trun_payload.extend_from_slice(&ver_flags.to_be_bytes());
    trun_payload.extend_from_slice(&count.to_be_bytes());
    trun_payload.extend_from_slice(&data_off.to_be_bytes());
    for cts in &cts_offsets {
        // Encode signed i32 → big-endian bytes (two's complement).
        trun_payload.extend_from_slice(&cts.to_be_bytes());
    }

    let mut traf = Vec::new();
    push_atom(&mut traf, *b"tfhd", &tfhd);
    push_atom(&mut traf, *b"trun", &trun_payload);
    let mut moof = Vec::new();
    push_atom(&mut moof, *b"mfhd", &build_mfhd(1));
    push_atom(&mut moof, *b"traf", &traf);
    let actual_moof_size = (8 + moof.len()) as u64;
    assert_eq!(actual_moof_size, moof_size, "moof size estimate must match");
    push_atom(&mut out, *b"moof", &moof);

    // mdat: 5 × 64 bytes
    let mut mdat_payload = Vec::new();
    for i in 0..count {
        mdat_payload.extend(std::iter::repeat(b'A' + (i as u8)).take(sz as usize));
    }
    push_atom(&mut out, *b"mdat", &mdat_payload);
    out
}

#[test]
fn negative_cts_offset_in_v1_trun_threads_through_to_pts() {
    let bytes = build_negative_cts_fixture();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open negative-CTS fmp4");
    assert!(d.is_fragmented());
    assert_eq!(d.tracks.len(), 1);
    let t = &d.tracks[0];
    assert_eq!(t.fragment_samples.len(), 5);

    let expected_dts: [u64; 5] = [0, 100, 200, 300, 400];
    let expected_cts: [i32; 5] = [100, -100, 50, 0, 200];
    let expected_pts: [i64; 5] = [100, 0, 250, 300, 600];

    for (i, s) in t.fragment_samples.iter().enumerate() {
        assert_eq!(s.dts, expected_dts[i], "sample {i} dts");
        assert_eq!(
            s.composition_offset, expected_cts[i],
            "sample {i} composition_offset"
        );
        assert_eq!(s.pts(), expected_pts[i], "sample {i} pts");
    }

    // Sample 1 has PTS < sample 0's PTS — that's the whole point of
    // signed CTS. Confirm the SampleEntry::pts() helper preserves the
    // ordering in the i64 surface.
    assert!(
        t.fragment_samples[1].pts() < t.fragment_samples[0].pts(),
        "B-frame reorder: sample 1's PTS must be earlier than sample 0's"
    );
}

#[test]
fn negative_cts_packet_pts_dts_matches_fragment_samples() {
    let bytes = build_negative_cts_fixture();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open negative-CTS fmp4");

    let expected_dts: [i64; 5] = [0, 100, 200, 300, 400];
    let expected_pts: [i64; 5] = [100, 0, 250, 300, 600];

    for i in 0..5 {
        let pkt = d
            .next_packet()
            .unwrap_or_else(|e| panic!("packet {i}: {e}"));
        assert_eq!(pkt.dts.unwrap_or(-1), expected_dts[i], "packet {i} dts");
        assert_eq!(pkt.pts.unwrap_or(-1), expected_pts[i], "packet {i} pts");
    }
}

// ─────────────── Goal 3: non-zero baseline tfdt + tfra-driven seek ───────────────

/// Build a two-fragment fixture with a tail `mfra/tfra/mfro` index.
/// Track 1 is a video track at 60kHz timescale (chosen to exercise
/// large `tfdt` values cleanly: 5 sec = 300000 ticks).
///
/// Layout:
///   moov (mvex/trex)
///   moof[1] + mdat[1]   — tfdt = 0,      5 sync samples × 60000 ticks each (DTS 0..240000)
///   moof[2] + mdat[2]   — tfdt = 300000, 5 sync samples × 60000 ticks each (DTS 300000..540000)
///   mfra (tfra carrying 10 entries: one per sync sample) + mfro
///
/// Note the gap between fragment 1's last DTS (240000) and fragment 2's
/// first DTS (300000): a real-world DASH segment with discontinuity (or
/// just timestamp re-anchoring at fragment boundaries — common when
/// fragments are produced by separate encoder runs and concatenated).
fn build_tfdt_baseline_fixture() -> (Vec<u8>, u64) {
    let media_ts: u32 = 60_000;
    let dur: u32 = 60_000; // 1 second per sample
    let sz: u32 = 100;
    let count_per_frag: u32 = 5;
    let frag2_tfdt: u64 = 300_000; // 5 seconds in
    let frag1_tfdt: u64 = 0;

    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_iso5());

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(media_ts, 0));
    push_atom(
        &mut moov,
        *b"trak",
        &build_trak_video(1, 320, 240, media_ts),
    );
    let mut mvex = Vec::new();
    // trex defaults: dur=60000, sz=100, sync (flags=0)
    push_atom(&mut mvex, *b"trex", &build_trex(1, 1, dur, sz, 0));
    push_atom(&mut moov, *b"mvex", &mvex);
    push_atom(&mut out, *b"moov", &moov);

    fn build_tfdt_v1(base: u64) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0x01_00_00_00u32.to_be_bytes()); // version=1
        p.extend_from_slice(&base.to_be_bytes());
        p
    }

    // Build a moof carrying tfhd + tfdt + trun (per-sample sizes so
    // we can verify the data offsets). All samples are sync — flag the
    // first via TRUN_FIRST_SAMPLE_FLAGS_PRESENT (= 0).
    fn build_one_moof(
        seq: u32,
        tfdt_v: u64,
        sample_count: u32,
        sz: u32,
        moof_start_in_file: u64,
    ) -> (Vec<u8>, Vec<u8>, Vec<u64>) {
        // mfhd (16) + traf
        // traf = tfhd(8+8=16) + tfdt v1 box(8 + 12 = 20) + trun(...)
        let tfhd = build_tfhd_dbim(1);
        let tfhd_box_len = 8 + tfhd.len() as u64;
        let tfdt_v = build_tfdt_v1(tfdt_v);
        let tfdt_box_len = 8 + tfdt_v.len() as u64;
        // trun: ver_flags(4) + sample_count(4) + data_offset(4) +
        //       first_sample_flags(4) + n*4(per-sample size)
        let tr_flags =
            TRUN_DATA_OFFSET_PRESENT | TRUN_FIRST_SAMPLE_FLAGS_PRESENT | TRUN_SAMPLE_SIZE_PRESENT;
        let trun_payload_len = 4 + 4 + 4 + 4 + (sample_count as u64 * 4);
        let trun_box_len = 8 + trun_payload_len;
        let traf_box_len = 8 + tfhd_box_len + tfdt_box_len + trun_box_len;
        let mfhd_box_len = 16u64;
        let moof_size = 8 + mfhd_box_len + traf_box_len;

        let data_off = (moof_size + 8) as i32;

        let mut trun_payload = Vec::new();
        trun_payload.extend_from_slice(&tr_flags.to_be_bytes()); // ver=0
        trun_payload.extend_from_slice(&sample_count.to_be_bytes());
        trun_payload.extend_from_slice(&data_off.to_be_bytes());
        trun_payload.extend_from_slice(&0u32.to_be_bytes()); // first_sample_flags=0 (sync)
        for _ in 0..sample_count {
            trun_payload.extend_from_slice(&sz.to_be_bytes());
        }

        let mut traf = Vec::new();
        push_atom(&mut traf, *b"tfhd", &tfhd);
        push_atom(&mut traf, *b"tfdt", &tfdt_v);
        push_atom(&mut traf, *b"trun", &trun_payload);

        let mut moof = Vec::new();
        push_atom(&mut moof, *b"mfhd", &build_mfhd(seq));
        push_atom(&mut moof, *b"traf", &traf);

        let mdat_payload_offset = moof_start_in_file + moof_size + 8;
        let mut sample_offsets = Vec::with_capacity(sample_count as usize);
        for i in 0..sample_count as u64 {
            sample_offsets.push(mdat_payload_offset + i * (sz as u64));
        }

        let mut mdat = Vec::new();
        for i in 0..sample_count {
            mdat.extend(std::iter::repeat(b'A' + (i as u8)).take(sz as usize));
        }

        // moof on the wire is `[size+type][payload]` and so is mdat.
        let mut moof_on_wire = Vec::new();
        push_atom(&mut moof_on_wire, *b"moof", &moof);
        let mut mdat_on_wire = Vec::new();
        push_atom(&mut mdat_on_wire, *b"mdat", &mdat);

        (moof_on_wire, mdat_on_wire, sample_offsets)
    }

    // Fragment 1
    let moof1_start = out.len() as u64;
    let (moof1, mdat1, _samp1_offs) =
        build_one_moof(1, frag1_tfdt, count_per_frag, sz, moof1_start);
    out.extend_from_slice(&moof1);
    out.extend_from_slice(&mdat1);

    // Fragment 2
    let moof2_start = out.len() as u64;
    let (moof2, mdat2, _samp2_offs) =
        build_one_moof(2, frag2_tfdt, count_per_frag, sz, moof2_start);
    out.extend_from_slice(&moof2);
    out.extend_from_slice(&mdat2);

    // Build mfra / tfra / mfro:
    // tfra v1 (64-bit time + moof_offset), 10 entries (5 from each
    // moof). The `time` field is the *presentation* time = DTS in this
    // file (no CTS offsets). length_size_of_traf_num / trun_num /
    // sample_num all = 0 (1 byte each).
    let mut tfra_payload = Vec::new();
    tfra_payload.extend_from_slice(&0x01_00_00_00u32.to_be_bytes()); // version=1
    tfra_payload.extend_from_slice(&1u32.to_be_bytes()); // track_id
    tfra_payload.extend_from_slice(&0u32.to_be_bytes()); // length_size word
    tfra_payload.extend_from_slice(&(count_per_frag * 2).to_be_bytes());
    // entries from frag 1
    for i in 0..count_per_frag as u64 {
        let time = frag1_tfdt + i * (dur as u64);
        tfra_payload.extend_from_slice(&time.to_be_bytes());
        tfra_payload.extend_from_slice(&moof1_start.to_be_bytes());
        tfra_payload.push(1); // traf_number
        tfra_payload.push(1); // trun_number
        tfra_payload.push((i + 1) as u8); // sample_number (1-based)
    }
    // entries from frag 2
    for i in 0..count_per_frag as u64 {
        let time = frag2_tfdt + i * (dur as u64);
        tfra_payload.extend_from_slice(&time.to_be_bytes());
        tfra_payload.extend_from_slice(&moof2_start.to_be_bytes());
        tfra_payload.push(1);
        tfra_payload.push(1);
        tfra_payload.push((i + 1) as u8);
    }

    let mut mfra_inner = Vec::new();
    push_atom(&mut mfra_inner, *b"tfra", &tfra_payload);
    // mfro carries the total mfra byte length (including the mfra
    // header AND mfro itself, per §8.8.11.2). We compute it after the
    // tfra is fixed:
    //   mfra = 8 (header) + tfra_box (8 + tfra_payload.len()) + mfro_box (8 + 8)
    let tfra_box_len = 8 + tfra_payload.len() as u64;
    let mfro_box_len = 16u64;
    let mfra_total = 8 + tfra_box_len + mfro_box_len;
    let mut mfro_payload = Vec::new();
    mfro_payload.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    mfro_payload.extend_from_slice(&(mfra_total as u32).to_be_bytes());
    push_atom(&mut mfra_inner, *b"mfro", &mfro_payload);
    push_atom(&mut out, *b"mfra", &mfra_inner);

    (out, frag2_tfdt)
}

#[test]
fn tfdt_baseline_threads_through_to_fragment2_dts() {
    let (bytes, frag2_tfdt) = build_tfdt_baseline_fixture();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open tfdt-baseline fixture");
    assert!(d.is_fragmented());
    let t = &d.tracks[0];
    assert_eq!(t.fragment_samples.len(), 10);

    // Fragment 1: dts 0..4 × 60000
    for i in 0..5usize {
        assert_eq!(
            t.fragment_samples[i].dts,
            (i as u64) * 60_000,
            "fragment 1 sample {i} dts"
        );
    }
    // Fragment 2: dts must START AT frag2_tfdt (300000), not climb
    // monotonically from the running 240000+60000=300000 cursor (here
    // they happen to coincide, but a tfdt with a *gap* would diverge —
    // see `tfdt_with_gap_does_not_climb_from_running_cursor` below).
    for i in 0..5usize {
        assert_eq!(
            t.fragment_samples[5 + i].dts,
            frag2_tfdt + (i as u64) * 60_000,
            "fragment 2 sample {i} dts"
        );
    }
}

#[test]
fn seek_into_late_fragment_with_tfdt_lands_correctly() {
    let (bytes, frag2_tfdt) = build_tfdt_baseline_fixture();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open tfdt-baseline fixture");

    // tfra is populated.
    assert!(!d.tfra_indexes.is_empty());
    assert_eq!(d.tfra_indexes[0].entries.len(), 10);

    // Target: 7 seconds in (= 420000 ticks). The largest tfra entry
    // whose time <= 420000 is the entry at time=420000 (which is
    // fragment 2's *third* sample at dts=300000+2*60000=420000).
    let target_pts: i64 = 420_000;
    let landed = d.seek_to(0, target_pts).expect("seek to 7s");
    assert_eq!(
        landed, 420_000,
        "seek_to(7s) should land at exactly DTS=420000 (frag2 sample 3)"
    );
    let pkt = d.next_packet().expect("packet after late-fragment seek");
    assert!(pkt.flags.keyframe, "must land on a sync sample");
    assert_eq!(pkt.dts.unwrap_or(-1), landed);
    assert_eq!(
        pkt.pts.unwrap_or(-1),
        420_000,
        "landed packet's PTS must equal target"
    );
    // Sanity: this MUST be a sample inside fragment 2 (DTS >= tfdt).
    assert!(
        landed as u64 >= frag2_tfdt,
        "seek to 7s must skip past fragment 1 entirely"
    );
}

/// Exercises a tfdt whose value does NOT coincide with the running
/// per-track DTS cursor — proves the demuxer trusts `tfdt` rather
/// than silently climbing from the previous fragment's last DTS.
///
/// Fragment 1: 5 samples × 60000 ticks → ends at DTS=300000 if extended.
/// Fragment 2 declares tfdt=600000 (10 sec — 5 sec gap from frag1's end).
/// If the demuxer ignored tfdt and climbed from the running cursor,
/// fragment 2's first sample would be at DTS=300000 instead of 600000.
fn build_tfdt_with_explicit_gap() -> Vec<u8> {
    let media_ts: u32 = 60_000;
    let dur: u32 = 60_000;
    let sz: u32 = 100;
    let count_per_frag: u32 = 5;
    // Pick frag2_tfdt that is decisively different from where the
    // running cursor (300000) would land. 600000 = 10 sec.
    let frag2_tfdt: u64 = 600_000;
    let frag1_tfdt: u64 = 0;

    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &ftyp_iso5());

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(media_ts, 0));
    push_atom(
        &mut moov,
        *b"trak",
        &build_trak_video(1, 320, 240, media_ts),
    );
    let mut mvex = Vec::new();
    push_atom(&mut mvex, *b"trex", &build_trex(1, 1, dur, sz, 0));
    push_atom(&mut moov, *b"mvex", &mvex);
    push_atom(&mut out, *b"moov", &moov);

    fn build_tfdt_v1(base: u64) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0x01_00_00_00u32.to_be_bytes());
        p.extend_from_slice(&base.to_be_bytes());
        p
    }

    fn build_one_moof(seq: u32, tfdt_v: u64, sample_count: u32, sz: u32) -> (Vec<u8>, Vec<u8>) {
        let tfhd = build_tfhd_dbim(1);
        let tfhd_box_len = 8 + tfhd.len() as u64;
        let tfdt_v_b = build_tfdt_v1(tfdt_v);
        let tfdt_box_len = 8 + tfdt_v_b.len() as u64;
        let tr_flags =
            TRUN_DATA_OFFSET_PRESENT | TRUN_FIRST_SAMPLE_FLAGS_PRESENT | TRUN_SAMPLE_SIZE_PRESENT;
        let trun_payload_len = 4 + 4 + 4 + 4 + (sample_count as u64 * 4);
        let trun_box_len = 8 + trun_payload_len;
        let traf_box_len = 8 + tfhd_box_len + tfdt_box_len + trun_box_len;
        let mfhd_box_len = 16u64;
        let moof_size = 8 + mfhd_box_len + traf_box_len;

        let data_off = (moof_size + 8) as i32;

        let mut trun_payload = Vec::new();
        trun_payload.extend_from_slice(&tr_flags.to_be_bytes());
        trun_payload.extend_from_slice(&sample_count.to_be_bytes());
        trun_payload.extend_from_slice(&data_off.to_be_bytes());
        trun_payload.extend_from_slice(&0u32.to_be_bytes()); // first_sample_flags=0 (sync)
        for _ in 0..sample_count {
            trun_payload.extend_from_slice(&sz.to_be_bytes());
        }

        let mut traf = Vec::new();
        push_atom(&mut traf, *b"tfhd", &tfhd);
        push_atom(&mut traf, *b"tfdt", &tfdt_v_b);
        push_atom(&mut traf, *b"trun", &trun_payload);

        let mut moof = Vec::new();
        push_atom(&mut moof, *b"mfhd", &build_mfhd(seq));
        push_atom(&mut moof, *b"traf", &traf);

        let mut mdat = Vec::new();
        for i in 0..sample_count {
            mdat.extend(std::iter::repeat(b'A' + (i as u8)).take(sz as usize));
        }

        let mut moof_on_wire = Vec::new();
        push_atom(&mut moof_on_wire, *b"moof", &moof);
        let mut mdat_on_wire = Vec::new();
        push_atom(&mut mdat_on_wire, *b"mdat", &mdat);

        (moof_on_wire, mdat_on_wire)
    }

    let (moof1, mdat1) = build_one_moof(1, frag1_tfdt, count_per_frag, sz);
    out.extend_from_slice(&moof1);
    out.extend_from_slice(&mdat1);
    let (moof2, mdat2) = build_one_moof(2, frag2_tfdt, count_per_frag, sz);
    out.extend_from_slice(&moof2);
    out.extend_from_slice(&mdat2);

    out
}

#[test]
fn tfdt_with_gap_does_not_climb_from_running_cursor() {
    let bytes = build_tfdt_with_explicit_gap();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open tfdt-with-gap fixture");
    assert!(d.is_fragmented());
    let t = &d.tracks[0];
    assert_eq!(t.fragment_samples.len(), 10);

    // Fragment 1 ends at DTS = 4*60000 = 240000.
    assert_eq!(t.fragment_samples[4].dts, 240_000);
    // The running cursor would be 300000 (240000 + 60000) but the
    // declared tfdt is 600000. If the demuxer ignored tfdt, sample 5
    // would be at 300000; if it consulted tfdt, sample 5 is at 600000.
    assert_eq!(
        t.fragment_samples[5].dts, 600_000,
        "fragment 2 first sample dts must reflect the declared tfdt (600000), \
         not climb from the previous fragment's running cursor (would be 300000)"
    );
    assert_eq!(t.fragment_samples[9].dts, 600_000 + 4 * 60_000);
}

#[test]
fn seek_to_first_sample_of_late_fragment_lands_at_tfdt_baseline() {
    // Target = exactly the tfdt of fragment 2 (5s). Should snap to
    // fragment 2's first sample, NOT to fragment 1's last sample.
    let (bytes, frag2_tfdt) = build_tfdt_baseline_fixture();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open tfdt-baseline fixture");

    let landed = d
        .seek_to(0, frag2_tfdt as i64)
        .expect("seek to fragment2 baseline");
    assert_eq!(
        landed, frag2_tfdt as i64,
        "seek to tfdt baseline should land exactly at the tfdt value"
    );
    let pkt = d.next_packet().expect("packet at tfdt boundary");
    assert!(pkt.flags.keyframe);
    assert_eq!(pkt.dts.unwrap_or(-1), frag2_tfdt as i64);
}
