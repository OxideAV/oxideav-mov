//! Round-18 acceptance: end-to-end fragmented MP4 / fMP4 / DASH-init
//! decode through `MovDemuxer`.
//!
//! Builds two synthetic fragmented containers entirely in memory and
//! drives them through the demuxer's packet stream to verify the
//! `moov + mvex/trex → moof/traf/tfhd/trun` cascade resolves
//! per-sample DTS / size / offset / keyframe correctly.
//!
//! Spec: ISO/IEC 14496-12:2015 §8.8 (Movie Fragments). The
//! per-fixture cite at the top of each builder identifies the
//! sub-section the fixture exercises.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{
    MovDemuxer, TFHD_DEFAULT_BASE_IS_MOOF, TFHD_DEFAULT_SAMPLE_DURATION_PRESENT,
    TFHD_DEFAULT_SAMPLE_SIZE_PRESENT, TRUN_DATA_OFFSET_PRESENT, TRUN_FIRST_SAMPLE_FLAGS_PRESENT,
    TRUN_SAMPLE_SIZE_PRESENT,
};

/// Build an `mvex/trex` mirroring ISO/IEC 14496-12 §8.8.3.2 with
/// per-track defaults that the `tfhd/trun` cascade can override.
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

/// Build a `tfhd` payload per ISO/IEC 14496-12 §8.8.7.2.
fn build_tfhd_default_base_is_moof_with_default_dur_sz(
    track_id: u32,
    default_dur: u32,
    default_sz: u32,
) -> Vec<u8> {
    let flags = TFHD_DEFAULT_BASE_IS_MOOF
        | TFHD_DEFAULT_SAMPLE_DURATION_PRESENT
        | TFHD_DEFAULT_SAMPLE_SIZE_PRESENT;
    let mut p = Vec::new();
    p.extend_from_slice(&flags.to_be_bytes());
    p.extend_from_slice(&track_id.to_be_bytes());
    p.extend_from_slice(&default_dur.to_be_bytes());
    p.extend_from_slice(&default_sz.to_be_bytes());
    p
}

/// Build a `tfhd` that carries no per-fragment overrides (relies on
/// `trex` for every default).
fn build_tfhd_default_base_is_moof_no_overrides(track_id: u32) -> Vec<u8> {
    let flags = TFHD_DEFAULT_BASE_IS_MOOF;
    let mut p = Vec::new();
    p.extend_from_slice(&flags.to_be_bytes());
    p.extend_from_slice(&track_id.to_be_bytes());
    p
}

/// Build a `trun` with the canonical "first-sample-is-keyframe,
/// per-sample sizes" shape used by ffmpeg's `-movflags +frag_keyframe`.
fn build_trun_data_offset_first_kf_per_sample_sizes(
    data_offset: i32,
    first_sample_flags: u32,
    sizes: &[u32],
) -> Vec<u8> {
    let flags =
        TRUN_DATA_OFFSET_PRESENT | TRUN_FIRST_SAMPLE_FLAGS_PRESENT | TRUN_SAMPLE_SIZE_PRESENT;
    let mut p = Vec::new();
    p.extend_from_slice(&flags.to_be_bytes());
    p.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
    p.extend_from_slice(&data_offset.to_be_bytes());
    p.extend_from_slice(&first_sample_flags.to_be_bytes());
    for sz in sizes {
        p.extend_from_slice(&sz.to_be_bytes());
    }
    p
}

/// Build a `mfhd` payload (§8.8.5.2).
fn build_mfhd(sequence_number: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&sequence_number.to_be_bytes());
    p
}

/// Build an empty stbl table — used for the "init segment" trak
/// where the in-moov sample tables are explicitly zero.
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

/// Build a single-fragment fMP4 with `mvex` + `trex` + a single
/// `moof` carrying one `traf/trun` of 5 samples. ISO/IEC 14496-12
/// §8.8.1 / §8.8.3 / §8.8.4. The mdat lives inside the moof's
/// expected base offset (`default-base-is-moof` + `data_offset`
/// into the post-moof region).
fn build_single_fragment_fmp4() -> (Vec<u8>, Vec<u32>) {
    let mut out = Vec::new();
    // ftyp with iso5 marker (the spec's required brand for
    // default-base-is-moof per §8.8.7.1 note).
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(b"mp42");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // --- moov with mvex/trex + empty-stbl trak ---
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 0, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 0));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    let (stts, stsc, stsz, stco) = build_empty_table_payloads();
    push_atom(&mut stbl, *b"stts", &stts);
    push_atom(&mut stbl, *b"stsc", &stsc);
    push_atom(&mut stbl, *b"stsz", &stsz);
    push_atom(&mut stbl, *b"stco", &stco);
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    let mut mvex = Vec::new();
    // trex declares default duration 100, default size 0 (we override),
    // default flags 0 (sync).
    push_atom(&mut mvex, *b"trex", &build_trex(1, 1, 100, 0, 0));
    push_atom(&mut moov, *b"mvex", &mvex);
    push_atom(&mut out, *b"moov", &moov);

    // --- moof with one traf ---
    let moof_start = out.len() as u64; // anchor for default-base-is-moof

    // Sample sizes inside this fragment (5 samples, 100 bytes each so
    // the mdat is 500 bytes).
    let sizes: Vec<u32> = vec![100, 100, 100, 100, 100];

    // The trun's `data_offset` is from moof_start (since the tfhd
    // uses default-base-is-moof). We don't yet know the exact value
    // until we know the moof size — but the moof payload is
    // entirely composed of mfhd + traf and we can compute it.
    //
    // We'll construct the moof, then place mdat immediately after it,
    // and make data_offset = (size of moof box) so the trun anchors
    // at the start of mdat-payload (which sits 8 bytes after mdat's
    // size+type header — so data_offset = moof_size + 8).
    //
    // Compute traf size first.
    let tfhd = build_tfhd_default_base_is_moof_with_default_dur_sz(1, 100, 0);
    // Data offset is the offset from start of moof to start of the
    // first sample's bytes. We'll place mdat directly after moof,
    // mdat header is 8 bytes, so data_offset = moof_size + 8.
    // We need to compute moof_size first by building everything with a
    // placeholder data_offset, then patching.
    //
    // Estimate moof_size:
    //   moof header: 8
    //   mfhd box:    8 + 8 = 16
    //   traf header: 8
    //   tfhd box:    8 + tfhd_payload.len()
    //   trun header: 8
    //   trun payload (data_offset:4 + first_sample_flags:4 + sample_count:4 +
    //                 ver_flags:4 + 5×size:4)
    //               = 4 + 4 + 4 + 4 + 20 = 36 bytes
    let trun_payload_len = 4 + 4 + 4 + 4 + (sizes.len() * 4); // ver_flags + sc + do + fsf + sizes
    let traf_payload_len = 8 + tfhd.len() + 8 + trun_payload_len;
    let moof_payload_len = 8 + 8 + 8 + traf_payload_len; // mfhd+traf headers
    let moof_size = 8 + moof_payload_len as u64;
    let data_offset = (moof_size + 8) as i32;

    let mut traf = Vec::new();
    push_atom(&mut traf, *b"tfhd", &tfhd);
    let trun =
        build_trun_data_offset_first_kf_per_sample_sizes(data_offset, 0 /* sync */, &sizes);
    push_atom(&mut traf, *b"trun", &trun);
    let mut moof = Vec::new();
    push_atom(&mut moof, *b"mfhd", &build_mfhd(1));
    push_atom(&mut moof, *b"traf", &traf);
    let actual_moof_size = (8 + moof.len()) as u64;
    assert_eq!(actual_moof_size, moof_size, "moof size estimate must match");
    push_atom(&mut out, *b"moof", &moof);

    // --- mdat ---
    let mut mdat_payload = Vec::new();
    for (i, &sz) in sizes.iter().enumerate() {
        mdat_payload.extend(std::iter::repeat(b'A' + i as u8).take(sz as usize));
    }
    push_atom(&mut out, *b"mdat", &mdat_payload);

    // Cross-check: the file's data_offset, applied to moof_start,
    // should land on the first byte of the mdat payload.
    let expected_first_sample_offset = moof_start + data_offset as u64;
    assert_eq!(
        expected_first_sample_offset,
        moof_start + moof_size + 8,
        "first sample should land at start of mdat payload"
    );

    (out, sizes)
}

#[test]
fn fragmented_single_moof_walks_all_samples() {
    let (bytes, sizes) = build_single_fragment_fmp4();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open fragmented fmp4");
    assert_eq!(d.tracks.len(), 1);
    assert!(d.is_fragmented(), "is_fragmented should report true");
    assert_eq!(d.trex_defaults.len(), 1);
    assert_eq!(d.fragment_sequence_numbers, vec![1]);
    assert_eq!(d.tracks[0].fragment_samples.len(), sizes.len());

    // Walk through every packet and verify size + monotonic DTS.
    let mut total = 0usize;
    let mut last_dts: i64 = -1;
    let mut first_kf_seen = false;
    while let Ok(pkt) = d.next_packet() {
        let i = total;
        assert_eq!(pkt.data.len(), sizes[i] as usize, "size at sample {i}");
        // Each sample should be filled with a distinct repeated byte.
        assert!(pkt.data.iter().all(|&b| b == pkt.data[0]));
        let dts = pkt.dts.unwrap();
        assert!(dts > last_dts);
        last_dts = dts;
        // First sample is sync via trun's first_sample_flags=0.
        // Subsequent samples take the trex's default_sample_flags=0
        // (also sync) — but the per-sample flags override only the
        // first when first_sample_flags_present is set. Test the
        // first sample.
        if !first_kf_seen {
            assert!(pkt.flags.keyframe);
            first_kf_seen = true;
        }
        assert_eq!(pkt.duration, Some(100));
        total += 1;
    }
    assert_eq!(total, sizes.len(), "all fragment samples must be emitted");
}

/// Build a multi-fragment fMP4 with TWO moofs back-to-back. The DTS
/// cursor must keep increasing across the moof boundary; the mfhd
/// sequence numbers must climb monotonically; the total sample count
/// is the sum of per-moof sample counts.
fn build_two_fragment_fmp4() -> (Vec<u8>, Vec<u32>) {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(b"mp42");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // moov with mvex/trex (trex provides per-sample defaults: dur 50)
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 0, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 0));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    let (stts, stsc, stsz, stco) = build_empty_table_payloads();
    push_atom(&mut stbl, *b"stts", &stts);
    push_atom(&mut stbl, *b"stsc", &stsc);
    push_atom(&mut stbl, *b"stsz", &stsz);
    push_atom(&mut stbl, *b"stco", &stco);
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    let mut mvex = Vec::new();
    // trex: default duration 50, default size 0 (we'll override via
    // trun per-sample sizes), default flags 0 (sync).
    push_atom(&mut mvex, *b"trex", &build_trex(1, 1, 50, 0, 0));
    push_atom(&mut moov, *b"mvex", &mvex);
    push_atom(&mut out, *b"moov", &moov);

    // -- Fragment 1: moof[1] + mdat (3 samples) --
    let frag1_sizes: Vec<u32> = vec![64, 64, 64];
    let frag1_moof_start = out.len() as u64;
    // Compute moof size and data_offset (no overrides, uses trex)
    let tfhd = build_tfhd_default_base_is_moof_no_overrides(1);
    let trun_payload_len = 4 + 4 + 4 + 4 + (frag1_sizes.len() * 4); // ver_flags+sc+do+fsf+sizes
    let traf_payload_len = 8 + tfhd.len() + 8 + trun_payload_len;
    let moof_payload_len = 8 + 8 + 8 + traf_payload_len;
    let moof_size = 8 + moof_payload_len as u64;
    let data_offset_1 = (moof_size + 8) as i32;
    let mut traf = Vec::new();
    push_atom(&mut traf, *b"tfhd", &tfhd);
    let trun = build_trun_data_offset_first_kf_per_sample_sizes(
        data_offset_1,
        0, /* sync */
        &frag1_sizes,
    );
    push_atom(&mut traf, *b"trun", &trun);
    let mut moof = Vec::new();
    push_atom(&mut moof, *b"mfhd", &build_mfhd(1));
    push_atom(&mut moof, *b"traf", &traf);
    push_atom(&mut out, *b"moof", &moof);
    let mut mdat_payload = Vec::new();
    for (i, &sz) in frag1_sizes.iter().enumerate() {
        mdat_payload.extend(std::iter::repeat(b'A' + i as u8).take(sz as usize));
    }
    push_atom(&mut out, *b"mdat", &mdat_payload);
    let _ = frag1_moof_start;

    // -- Fragment 2: moof[2] + mdat (4 samples, larger per-sample
    //    sizes via tfhd defaults for diversity) --
    let frag2_sizes: Vec<u32> = vec![80, 80, 80, 80];
    let frag2_moof_start = out.len() as u64;
    // Per-fragment override: tfhd carries default_sample_duration=75,
    // default_sample_size=80; trun omits per-sample fields so each
    // row reads back the cascade default.
    let tfhd2 = build_tfhd_default_base_is_moof_with_default_dur_sz(1, 75, 80);
    // trun with NO per-sample fields (purely cascade), but does have
    // a data_offset to position into the fragment's mdat. We can't
    // use TRUN_SAMPLE_SIZE_PRESENT here because we want the tfhd
    // default to be exercised — build a minimal trun.
    let trun2_payload_len = 4 + 4 + 4; // ver_flags + sample_count + data_offset
    let traf2_payload_len = 8 + tfhd2.len() + 8 + trun2_payload_len;
    let moof2_payload_len = 8 + 8 + 8 + traf2_payload_len;
    let moof2_size = 8 + moof2_payload_len as u64;
    let data_offset_2 = (moof2_size + 8) as i32;
    let mut traf2 = Vec::new();
    push_atom(&mut traf2, *b"tfhd", &tfhd2);
    let mut trun2_payload = Vec::new();
    trun2_payload.extend_from_slice(&TRUN_DATA_OFFSET_PRESENT.to_be_bytes()); // ver+flags
    trun2_payload.extend_from_slice(&(frag2_sizes.len() as u32).to_be_bytes()); // sample_count
    trun2_payload.extend_from_slice(&data_offset_2.to_be_bytes()); // data_offset
    push_atom(&mut traf2, *b"trun", &trun2_payload);
    let mut moof2 = Vec::new();
    push_atom(&mut moof2, *b"mfhd", &build_mfhd(2));
    push_atom(&mut moof2, *b"traf", &traf2);
    push_atom(&mut out, *b"moof", &moof2);
    let mut mdat2_payload = Vec::new();
    for (i, &sz) in frag2_sizes.iter().enumerate() {
        mdat2_payload.extend(std::iter::repeat(b'P' + i as u8).take(sz as usize));
    }
    push_atom(&mut out, *b"mdat", &mdat2_payload);
    let _ = frag2_moof_start;

    let mut all_sizes = frag1_sizes;
    all_sizes.extend(frag2_sizes);
    (out, all_sizes)
}

#[test]
fn fragmented_two_moofs_dts_climbs_monotonically() {
    let (bytes, sizes) = build_two_fragment_fmp4();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open two-fragment fmp4");
    assert_eq!(d.tracks.len(), 1);
    assert!(d.is_fragmented());
    assert_eq!(d.fragment_sequence_numbers, vec![1, 2]);
    assert_eq!(d.tracks[0].fragment_samples.len(), 7);

    // The DTS of fragment 2's first sample must be exactly
    // (3 × 50) = 150 (trex default duration), because fragment 1
    // contributed 3 samples × 50 ticks/sample.
    let frag2_first = &d.tracks[0].fragment_samples[3];
    assert_eq!(frag2_first.dts, 150);
    assert_eq!(frag2_first.duration, 75); // tfhd override
    assert_eq!(frag2_first.size, 80); // tfhd override

    // Walk all 7 packets and check monotonic DTS + correct payloads
    // (each sample is filled with a distinct repeated byte).
    let mut count = 0;
    let mut last_dts: i64 = -1;
    while let Ok(pkt) = d.next_packet() {
        let dts = pkt.dts.unwrap();
        assert!(dts > last_dts || dts == 0);
        last_dts = dts;
        assert_eq!(pkt.data.len(), sizes[count] as usize);
        assert!(pkt.data.iter().all(|&b| b == pkt.data[0]));
        count += 1;
    }
    assert_eq!(count, 7);
}

/// Mfhd sequence-number monotonicity is purely informational; the
/// demuxer surfaces the numbers but does not refuse a fragment with
/// a stale sequence. Verify the surfaced order matches the wire.
#[test]
fn mfhd_sequence_numbers_preserved_in_wire_order() {
    let (bytes, _) = build_two_fragment_fmp4();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).unwrap();
    assert_eq!(d.fragment_sequence_numbers.len(), 2);
    // Monotonic increase per §8.8.5.3.
    assert!(d.fragment_sequence_numbers[0] < d.fragment_sequence_numbers[1]);
}

/// `is_fragmented()` returns false for a plain non-fragmented file.
#[test]
fn non_fragmented_returns_false() {
    // Re-use the minimal fixture from synth_minimal_qt via a fresh
    // hand-roll. Build the same 1-sample plain MOV.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"mdat", b"PAYLOAD!");
    let mdat_payload_offset: u32 = 28;
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_payload_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).unwrap();
    assert!(!d.is_fragmented());
    assert!(d.trex_defaults.is_empty());
    assert!(d.fragment_sequence_numbers.is_empty());
}
