//! Round 128 — Producer Reference Time Box (`prft`) decode.
//!
//! Exercises the file-level `prft` surface (ISO/IEC 14496-12 §8.16.5)
//! against a hand-built ISO BMFF file whose top-level carries one or
//! more `prft` boxes. The box lives next to `ftyp`, `styp`, `sidx`,
//! `moof`, `mdat` at file scope (not inside `moov`); the spec ties each
//! `prft` to the *next* `moof` in bitstream order (§8.16.5.1).
//!
//! These tests open via `MovDemuxer` and verify:
//! * the parsed `Prft` exposes `reference_track_id`, `ntp_timestamp`,
//!   and `media_time` byte-for-byte (both v0 and v1 widths);
//! * multiple `prft` boxes in a segmented stream are collected in
//!   file order (`Quantity: Zero or more`, §8.16.5.1);
//! * a file without `prft` reports an empty list;
//! * `first_prft()` surfaces the file's earliest producer time;
//! * `Prft::unix_micros()` converts the NTP timestamp to a microsecond
//!   Unix instant (RFC 5905 §6 + the 2 208 988 800 s NTP→Unix offset).

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, NTP_TO_UNIX_EPOCH_SECONDS};

/// Build a v0 `prft` FullBox payload (`version = 0`, `flags = 0`,
/// 32-bit media_time).
fn build_prft_v0_payload(reference_track_id: u32, ntp_timestamp: u64, media_time: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(20);
    p.push(0); // version
    p.extend_from_slice(&[0, 0, 0]); // flags
    p.extend_from_slice(&reference_track_id.to_be_bytes());
    p.extend_from_slice(&ntp_timestamp.to_be_bytes());
    p.extend_from_slice(&media_time.to_be_bytes());
    p
}

/// Build a v1 `prft` FullBox payload (`version = 1`, `flags = 0`,
/// 64-bit media_time).
fn build_prft_v1_payload(reference_track_id: u32, ntp_timestamp: u64, media_time: u64) -> Vec<u8> {
    let mut p = Vec::with_capacity(24);
    p.push(1); // version
    p.extend_from_slice(&[0, 0, 0]); // flags
    p.extend_from_slice(&reference_track_id.to_be_bytes());
    p.extend_from_slice(&ntp_timestamp.to_be_bytes());
    p.extend_from_slice(&media_time.to_be_bytes());
    p
}

/// Build a minimal one-video-track ISO BMFF file with `ftyp`, an
/// optional list of file-level `prft` payloads, `moov`, and `mdat`.
/// The `prft` boxes are emitted in order between `ftyp` and `moov` —
/// the §8.16.5.1 "before the following movie fragment box" placement
/// that a live writer would use.
fn build_isobmff_with_prft(prft_payloads: &[Vec<u8>]) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        push_atom(&mut out, *b"ftyp", &ftyp);

        for payload in prft_payloads {
            push_atom(&mut out, *b"prft", payload);
        }

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

#[test]
fn prft_v0_single_box_parses_byte_exact() {
    // 2024-01-01T00:00:00Z in NTP form: unix_secs 1_704_067_200 →
    // ntp_secs 1_704_067_200 + 2_208_988_800 = 3_913_056_000 → upper
    // 32 bits of the 64-bit NTP timestamp. Fractional part = 0.
    let ntp = 3_913_056_000u64 << 32;
    let payload = build_prft_v0_payload(1, ntp, 90_000);
    let file = build_isobmff_with_prft(&[payload]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open prft v0 fixture");

    assert_eq!(d.prft.len(), 1, "one prft box collected");
    let prft = &d.prft[0];
    assert_eq!(prft.version, 0);
    assert_eq!(prft.reference_track_id, 1);
    assert_eq!(prft.ntp_timestamp, ntp);
    assert_eq!(prft.media_time, 90_000);

    // Convenience accessors.
    assert_eq!(prft.ntp_seconds(), 3_913_056_000);
    assert_eq!(prft.ntp_fraction(), 0);
    assert_eq!(prft.unix_micros(), Some(1_704_067_200_000_000));

    // `first_prft()` mirrors `self.prft.first()`.
    assert_eq!(d.first_prft().map(|p| p.media_time), Some(90_000));
}

#[test]
fn prft_v1_wide_media_time_round_trips_through_demuxer() {
    // §8.16.5.2 v1 widens `media_time` to 64 bits — confirm a value
    // beyond the 32-bit range survives the open path intact.
    let ntp = (NTP_TO_UNIX_EPOCH_SECONDS << 32) | 0xDEAD_BEEFu64;
    let media_time: u64 = 0x1_0000_0001;
    let payload = build_prft_v1_payload(7, ntp, media_time);
    let file = build_isobmff_with_prft(&[payload]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open prft v1 fixture");

    assert_eq!(d.prft.len(), 1);
    let prft = &d.prft[0];
    assert_eq!(prft.version, 1);
    assert_eq!(prft.reference_track_id, 7);
    assert_eq!(prft.ntp_timestamp, ntp);
    assert_eq!(prft.media_time, media_time);
    // ntp_seconds == NTP_TO_UNIX_EPOCH_SECONDS → Unix epoch +
    // fractional-second contribution from 0xDEAD_BEEF / 2^32 s.
    assert_eq!(prft.ntp_seconds(), NTP_TO_UNIX_EPOCH_SECONDS as u32);
    assert_eq!(prft.ntp_fraction(), 0xDEAD_BEEF);
}

#[test]
fn multiple_prft_collected_in_file_order() {
    // Live writer emits one prft per fragment; the demuxer preserves
    // every one in declaration order so a caller can step through the
    // producer-time series alongside the moof stream.
    let ntp_a = NTP_TO_UNIX_EPOCH_SECONDS << 32; // exactly Unix epoch
    let ntp_b = (NTP_TO_UNIX_EPOCH_SECONDS + 1) << 32; // +1 s
    let ntp_c = (NTP_TO_UNIX_EPOCH_SECONDS + 2) << 32; // +2 s

    let payloads = vec![
        build_prft_v0_payload(1, ntp_a, 0),
        build_prft_v0_payload(1, ntp_b, 90_000),
        build_prft_v0_payload(1, ntp_c, 180_000),
    ];
    let file = build_isobmff_with_prft(&payloads);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open multi-prft fixture");

    assert_eq!(d.prft.len(), 3);
    assert_eq!(d.prft[0].media_time, 0);
    assert_eq!(d.prft[1].media_time, 90_000);
    assert_eq!(d.prft[2].media_time, 180_000);

    // unix_micros for each row: exact, +1 s, +2 s past Unix epoch.
    assert_eq!(d.prft[0].unix_micros(), Some(0));
    assert_eq!(d.prft[1].unix_micros(), Some(1_000_000));
    assert_eq!(d.prft[2].unix_micros(), Some(2_000_000));

    // first_prft() picks the earliest — index 0.
    assert_eq!(d.first_prft().unwrap().media_time, 0);
}

#[test]
fn prft_absent_yields_empty_list() {
    let file = build_isobmff_with_prft(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open file with no prft");
    assert!(d.prft.is_empty(), "no prft must surface as empty Vec");
    assert!(d.first_prft().is_none());
}

#[test]
fn prft_truncated_payload_rejects_at_open_time() {
    // Hand-craft a file whose `prft` body is one byte short of the
    // 4-byte FullBox header — the open path must reject so a half-box
    // can't be silently dropped.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"isom");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"prft", &[0u8; 3]); // 3-byte payload

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "a truncated prft payload must be rejected at open time"
    );
}

#[test]
fn prft_unknown_version_rejects_at_open_time() {
    // §8.16.5.2 defines only v0 and v1; an unknown version (2 here) is
    // a writer error or a forward-compatible extension we can't decode.
    // Build a v0-shaped body and patch the version byte to 2.
    let mut prft = build_prft_v0_payload(1, 0, 0);
    prft[0] = 2; // bogus version

    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"isom");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"prft", &prft);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "unknown prft version must be rejected at open time"
    );
}

#[test]
fn prft_trailing_bytes_rejects_at_open_time() {
    // `prft` has a fixed-width record and no trailing list — any extra
    // bytes past the spec-defined record indicate a corrupt or
    // non-standard writer extension and must reject.
    let mut prft = build_prft_v0_payload(1, 0, 42);
    prft.extend_from_slice(&[0u8; 4]); // stray tail

    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"isom");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"prft", &prft);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "trailing bytes past the prft record must be rejected at open time"
    );
}
