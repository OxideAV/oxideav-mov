//! Round 80 — sample-group (`sbgp` / `sgpd`) decode + typed lookups.
//!
//! Exercises the round-80 surface against hand-built QT files that
//! carry the three well-known grouping types specified in ISO/IEC
//! 14496-12 §10:
//!
//! * `'roll'` (§10.1.1.2) — VisualRollRecoveryEntry / AudioRollRecoveryEntry.
//! * `'prol'` (§10.1.1.2) — AudioPreRollEntry (AAC / Opus codec priming).
//! * `'rap '` (§10.4.2) — VisualRandomAccessEntry (open GOP markers).
//!
//! Each test builds a single-track MOV with the grouping atoms slid
//! into `stbl`, opens it via `MovDemuxer`, and verifies that the
//! per-sample typed-lookup helpers (`roll_distance_for`,
//! `audio_preroll_for`, `visual_random_access_for`,
//! `random_access_points`) agree with the underlying table state.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a 4-sample audio QT file with the caller-supplied `sbgp` /
/// `sgpd` atoms appended at the end of `stbl`. mvhd / mdhd are both
/// 600-tick timescale, 4 × 30 = 120 ticks total.
fn build_audio_qt_with_sample_groups(
    sbgp_atoms: &[(u32, Vec<u8>)],
    sgpd_atoms: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  ");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
        let mut trak = Vec::new();
        push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 120, 0, 0));
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 120));
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
        let mut minf = Vec::new();
        // smhd (minimal): 8-byte body — flags + balance + reserved.
        push_atom(&mut minf, *b"smhd", &[0u8; 8]);
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_audio(b"mp4a", 2, 16, 48000, &[]),
        );
        push_atom(&mut stbl, *b"stts", &build_stts_single(4, 30));
        push_atom(&mut stbl, *b"stsc", &build_stsc_single(4));
        push_atom(&mut stbl, *b"stsz", &build_stsz_constant(2, 4));
        push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
        for (_grouping_type, payload) in sbgp_atoms {
            push_atom(&mut stbl, *b"sbgp", payload);
        }
        for (_grouping_type, payload) in sgpd_atoms {
            push_atom(&mut stbl, *b"sgpd", payload);
        }
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

/// Build a 4-sample video QT file with a non-empty `stss` plus the
/// caller-supplied sample-group atoms. mvhd / mdhd = 600 ticks.
fn build_video_qt_with_sample_groups(
    stss_sample_ids_one_based: &[u32],
    sbgp_atoms: &[(u32, Vec<u8>)],
    sgpd_atoms: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_stss = || -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&(stss_sample_ids_one_based.len() as u32).to_be_bytes());
        for id in stss_sample_ids_one_based {
            p.extend_from_slice(&id.to_be_bytes());
        }
        p
    };

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  ");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
        let mut trak = Vec::new();
        push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 120, 320, 240));
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 120));
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"vmhd", &build_vmhd());
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_video(b"avc1", 320, 240, &[]),
        );
        push_atom(&mut stbl, *b"stts", &build_stts_single(4, 30));
        push_atom(&mut stbl, *b"stsc", &build_stsc_single(4));
        push_atom(&mut stbl, *b"stsz", &build_stsz_constant(2, 4));
        push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
        if !stss_sample_ids_one_based.is_empty() {
            push_atom(&mut stbl, *b"stss", &build_stss());
        }
        for (_grouping_type, payload) in sbgp_atoms {
            push_atom(&mut stbl, *b"sbgp", payload);
        }
        for (_grouping_type, payload) in sgpd_atoms {
            push_atom(&mut stbl, *b"sgpd", payload);
        }
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

/// Build a v0 `sbgp` payload: `[ver+flags][grouping_type][entry_count]
/// entry_count × (sample_count, group_description_index)`.
fn build_sbgp_v0(grouping_type: &[u8; 4], runs: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(grouping_type);
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, idx) in runs {
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&idx.to_be_bytes());
    }
    p
}

/// Build a v1 `sgpd` payload with `default_length` set so every entry
/// is exactly that many bytes.
fn build_sgpd_v1_fixed(
    grouping_type: &[u8; 4],
    default_length: u32,
    entries: &[Vec<u8>],
) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(1u8); // version
    p.extend_from_slice(&[0, 0, 0]); // flags
    p.extend_from_slice(grouping_type);
    p.extend_from_slice(&default_length.to_be_bytes());
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for e in entries {
        assert_eq!(
            e.len() as u32,
            default_length,
            "fixture entry width mismatch"
        );
        p.extend_from_slice(e);
    }
    p
}

#[test]
fn audio_preroll_resolves_aac_codec_priming_distance() {
    // 4-sample audio track. AAC pre-roll convention: 2048 frames.
    // sbgp/'prol': 4 × 1 (all 4 samples map to entry 1).
    // sgpd/'prol' v1: 1 entry, signed i16 = -2048.
    let sbgp = build_sbgp_v0(b"prol", &[(4, 1)]);
    let sgpd = build_sgpd_v1_fixed(b"prol", 2, &[(-2048i16).to_be_bytes().to_vec()]);
    let bytes = build_audio_qt_with_sample_groups(
        &[(u32::from_be_bytes(*b"prol"), sbgp)],
        &[(u32::from_be_bytes(*b"prol"), sgpd)],
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open audio prol fixture");

    for s in 0..4u32 {
        assert_eq!(
            d.audio_preroll_for(0, s),
            Some(-2048),
            "sample {s} pre-roll"
        );
    }
    // Sample 999 is outside the table — sbgp covers exactly 4.
    assert_eq!(d.audio_preroll_for(0, 999), None);
    // 'roll' grouping is not present on this fixture.
    assert_eq!(d.roll_distance_for(0, 0), None);
}

#[test]
fn roll_distance_per_run_audio_roll_recovery() {
    // Two-run sbgp: first 2 samples → group 1 (roll_distance = -3),
    // last 2 samples → group 2 (roll_distance = -5).
    let sbgp = build_sbgp_v0(b"roll", &[(2, 1), (2, 2)]);
    let sgpd = build_sgpd_v1_fixed(
        b"roll",
        2,
        &[
            (-3i16).to_be_bytes().to_vec(),
            (-5i16).to_be_bytes().to_vec(),
        ],
    );
    let bytes = build_audio_qt_with_sample_groups(
        &[(u32::from_be_bytes(*b"roll"), sbgp)],
        &[(u32::from_be_bytes(*b"roll"), sgpd)],
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open audio roll fixture");

    assert_eq!(d.roll_distance_for(0, 0), Some(-3));
    assert_eq!(d.roll_distance_for(0, 1), Some(-3));
    assert_eq!(d.roll_distance_for(0, 2), Some(-5));
    assert_eq!(d.roll_distance_for(0, 3), Some(-5));
}

#[test]
fn missing_sgpd_yields_none_even_when_sbgp_present() {
    // sbgp on its own — no matching sgpd. The spec (§8.9.3.1) says
    // every sbgp must have a paired sgpd; we return None rather than
    // erroring on malformed authoring.
    let sbgp = build_sbgp_v0(b"roll", &[(4, 1)]);
    let bytes = build_audio_qt_with_sample_groups(&[(u32::from_be_bytes(*b"roll"), sbgp)], &[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open sbgp-only fixture");
    assert_eq!(d.roll_distance_for(0, 0), None);
}

#[test]
fn rap_open_gop_marks_extra_random_access_point() {
    // 4-sample video; only sample 1 (one-based) is in stss. Mark
    // sample 3 (one-based) as an open RAP via sbgp/'rap '.
    let sbgp = build_sbgp_v0(b"rap ", &[(2, 0), (1, 1), (1, 0)]);
    // 'rap ' entry = 1 byte: top-bit = known + 7-bit num_leading_samples.
    // Set known=1, num_leading_samples=2 → 0x82.
    let sgpd = build_sgpd_v1_fixed(b"rap ", 1, &[vec![0x82]]);
    let bytes = build_video_qt_with_sample_groups(
        &[1],
        &[(u32::from_be_bytes(*b"rap "), sbgp)],
        &[(u32::from_be_bytes(*b"rap "), sgpd)],
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open rap fixture");

    // visual_random_access_for: sample 2 (zero-based) is the marked RAP.
    let v = d.visual_random_access_for(0, 2).expect("rap entry");
    assert!(v.num_leading_samples_known);
    assert_eq!(v.num_leading_samples, 2);
    // Samples 0, 1, 3 are not marked.
    assert!(d.visual_random_access_for(0, 0).is_none());
    assert!(d.visual_random_access_for(0, 1).is_none());
    assert!(d.visual_random_access_for(0, 3).is_none());

    // random_access_points = stss {0} ∪ rap {2} = [0, 2].
    let raps = d.random_access_points(0);
    assert_eq!(raps, vec![0, 2]);
}

#[test]
fn random_access_points_empty_stss_means_every_sample() {
    // No stss → QTFF p. 73 implicit-sync. random_access_points lists
    // every sample. Adding a sbgp/'rap ' on top doesn't change the
    // set (it's already complete).
    let bytes = build_video_qt_with_sample_groups(&[], &[], &[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open no-stss fixture");
    let raps = d.random_access_points(0);
    assert_eq!(raps, vec![0, 1, 2, 3]);
}

#[test]
fn v2_sgpd_default_index_falls_back_when_sbgp_says_zero() {
    // sbgp marks all 4 samples as group 0 (no explicit group). v2
    // sgpd carries default_sample_description_index=1, so the
    // per-sample lookup should fall back to entry 1 rather than None.
    let sbgp = build_sbgp_v0(b"prol", &[(4, 0)]);
    // v2 sgpd: ver=2, default_length=2, default_sd_index=1,
    // entry_count=1, body = i16(-960).
    let mut sgpd = Vec::new();
    sgpd.push(2u8);
    sgpd.extend_from_slice(&[0, 0, 0]);
    sgpd.extend_from_slice(b"prol");
    sgpd.extend_from_slice(&2u32.to_be_bytes()); // default_length
    sgpd.extend_from_slice(&1u32.to_be_bytes()); // default_sd_index
    sgpd.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    sgpd.extend_from_slice(&(-960i16).to_be_bytes());

    let bytes = build_audio_qt_with_sample_groups(
        &[(u32::from_be_bytes(*b"prol"), sbgp)],
        &[(u32::from_be_bytes(*b"prol"), sgpd)],
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open v2 sgpd default fixture");

    for s in 0..4u32 {
        assert_eq!(d.audio_preroll_for(0, s), Some(-960));
    }
}

/// Build a 4-sample audio QT file carrying a `csgp` (Compact Sample
/// to Group) atom plus its paired `sgpd`. Mirrors
/// [`build_audio_qt_with_sample_groups`] but pushes a `csgp` fourcc.
fn build_audio_qt_with_csgp(csgp_payload: &[u8], sgpd_payload: &[u8]) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  ");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
        let mut trak = Vec::new();
        push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 120, 0, 0));
        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 120));
        push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"smhd", &[0u8; 8]);
        let mut stbl = Vec::new();
        push_atom(
            &mut stbl,
            *b"stsd",
            &build_stsd_audio(b"mp4a", 2, 16, 48000, &[]),
        );
        push_atom(&mut stbl, *b"stts", &build_stts_single(4, 30));
        push_atom(&mut stbl, *b"stsc", &build_stsc_single(4));
        push_atom(&mut stbl, *b"stsz", &build_stsz_constant(2, 4));
        push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
        push_atom(&mut stbl, *b"csgp", csgp_payload);
        push_atom(&mut stbl, *b"sgpd", sgpd_payload);
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    build_file(mdat_fourcc_pos + 4)
}

#[test]
fn csgp_compact_group_resolves_per_sample_through_demuxer() {
    // 4-sample audio. csgp: one pattern of length 2 (indices 1,2)
    // replayed across all 4 samples → 1,2,1,2. sgpd/'prol' v1: two
    // entries, roll_distance -100 (entry 1) and -200 (entry 2).
    // index_size_code=1 (8-bit), count_size_code=1, pattern_size_code=1.
    let mut csgp = Vec::new();
    csgp.push(0u8); // version
                    // flags: index_code=1 (bits0..1), count_code=1 (bits2..3),
                    // pattern_code=1 (bits4..5), gtp_present=0.
    let flags: u32 = 1 | (1 << 2) | (1 << 4);
    csgp.extend_from_slice(&flags.to_be_bytes()[1..4]);
    csgp.extend_from_slice(b"prol"); // grouping_type
    csgp.extend_from_slice(&1u32.to_be_bytes()); // pattern_count
                                                 // pattern table (all 8-bit): pattern_length=2, sample_count=4.
    csgp.push(2); // pattern_length[0]
    csgp.push(4); // sample_count[0]
                  // index table (8-bit each): 1, 2.
    csgp.push(1);
    csgp.push(2);

    let sgpd = build_sgpd_v1_fixed(
        b"prol",
        2,
        &[
            (-100i16).to_be_bytes().to_vec(),
            (-200i16).to_be_bytes().to_vec(),
        ],
    );

    let bytes = build_audio_qt_with_csgp(&csgp, &sgpd);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open csgp fixture");

    // Pattern 1,2,1,2 → samples 0,2 → entry 1 (-100); 1,3 → entry 2 (-200).
    assert_eq!(d.audio_preroll_for(0, 0), Some(-100));
    assert_eq!(d.audio_preroll_for(0, 1), Some(-200));
    assert_eq!(d.audio_preroll_for(0, 2), Some(-100));
    assert_eq!(d.audio_preroll_for(0, 3), Some(-200));
    // Sample beyond the covered 4 → no group.
    assert_eq!(d.audio_preroll_for(0, 4), None);
}

#[test]
fn opus_negative_preroll_round_trips_signed() {
    // Opus codec-priming convention: 80 ms @ 48 kHz = 3840 samples.
    // Spec roll_distance must be signed; -3840 packs as 0xF100.
    let sbgp = build_sbgp_v0(b"prol", &[(4, 1)]);
    let sgpd = build_sgpd_v1_fixed(b"prol", 2, &[(-3840i16).to_be_bytes().to_vec()]);
    let bytes = build_audio_qt_with_sample_groups(
        &[(u32::from_be_bytes(*b"prol"), sbgp)],
        &[(u32::from_be_bytes(*b"prol"), sgpd)],
    );
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open opus prol fixture");

    assert_eq!(d.audio_preroll_for(0, 0), Some(-3840));
}
