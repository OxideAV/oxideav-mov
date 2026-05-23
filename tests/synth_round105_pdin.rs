//! Round 105 — Progressive Download Information Box (`pdin`) decode.
//!
//! Exercises the file-level `pdin` surface (ISO/IEC 14496-12 §8.1.3)
//! against a hand-built ISO BMFF file whose top-level carries a `pdin`
//! box of `(rate, initial_delay)` pairs. The box lives next to `ftyp`,
//! `moov`, `mdat` at file scope (not inside `moov`), and the spec
//! recommends it appear as early as possible (§8.1.3.1) so the
//! receiver can act on it before any media is needed.
//!
//! These tests open via `MovDemuxer` and verify:
//! * the parsed `Pdin` exposes the `(rate, initial_delay)` pairs in
//!   file order,
//! * the linear-interpolation accessor brackets correctly between
//!   pairs and clamps to the first / last entry for out-of-range
//!   observed rates (§8.1.3.1: "by linear interpolation between
//!   pairs, or by extrapolation from the first or last entry"),
//! * a top-level box ordered after `moov` is still picked up,
//! * a file without `pdin` reports `None`.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Build a `pdin` FullBox payload (`version = 0`, `flags = 0`) carrying
/// the supplied `(rate, initial_delay)` pairs in order.
fn build_pdin_payload(pairs: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + 8 * pairs.len());
    p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
    for (rate, delay) in pairs {
        p.extend_from_slice(&rate.to_be_bytes());
        p.extend_from_slice(&delay.to_be_bytes());
    }
    p
}

/// Build a minimal one-video-track ISO BMFF file with `ftyp`, an
/// optional file-level `pdin` (controlled by the closure argument),
/// `moov`, and `mdat`. The `pdin_placement` closure decides whether
/// to emit the box, and where: returns `Some((position, payload))`
/// where `position` ∈ {"before_moov", "after_moov"}.
fn build_isobmff_with_optional_pdin(
    pdin_pairs: Option<&[(u32, u32)]>,
    after_moov: bool,
) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        push_atom(&mut out, *b"ftyp", &ftyp);

        // `pdin` before `moov` — the spec's "as early as possible"
        // recommendation (§8.1.3.1).
        if let Some(pairs) = pdin_pairs {
            if !after_moov {
                push_atom(&mut out, *b"pdin", &build_pdin_payload(pairs));
            }
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

        // `pdin` after `moov` — non-recommended placement but spec
        // §8.1.3.1 says "as early as possible" not "must be first";
        // the parser still needs to recognise it.
        if let Some(pairs) = pdin_pairs {
            if after_moov {
                push_atom(&mut out, *b"pdin", &build_pdin_payload(pairs));
            }
        }

        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    build_file(mdat_payload_offset)
}

#[test]
fn pdin_before_moov_parses_with_two_pairs() {
    let pairs = [(125_000u32, 2_000u32), (1_000_000u32, 250u32)];
    let file = build_isobmff_with_optional_pdin(Some(&pairs), false);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open pdin fixture");
    let pdin = d.pdin.as_ref().expect("pdin parsed");
    assert_eq!(pdin.entries.len(), 2);
    assert_eq!(pdin.entries[0].rate, 125_000);
    assert_eq!(pdin.entries[0].initial_delay, 2_000);
    assert_eq!(pdin.entries[1].rate, 1_000_000);
    assert_eq!(pdin.entries[1].initial_delay, 250);
}

#[test]
fn pdin_after_moov_still_picked_up() {
    // Spec §8.1.3.1 only *recommends* `pdin` go early; a writer that
    // emits it after `moov` is still standards-compliant.
    let pairs = [(64_000u32, 8_000u32)];
    let file = build_isobmff_with_optional_pdin(Some(&pairs), true);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open post-moov pdin fixture");
    let pdin = d.pdin.as_ref().expect("pdin parsed even when after moov");
    assert_eq!(pdin.entries.len(), 1);
    assert_eq!(pdin.entries[0].rate, 64_000);
    assert_eq!(pdin.entries[0].initial_delay, 8_000);
}

#[test]
fn pdin_absent_yields_none() {
    let file = build_isobmff_with_optional_pdin(None, false);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open file with no pdin");
    assert!(d.pdin.is_none(), "no pdin in input must surface as None");
}

#[test]
fn pdin_interpolates_initial_delay_at_observed_rate() {
    // Three pairs spanning the dial-up → broadband range.
    let pairs = [
        (100_000u32, 8_000u32),
        (500_000u32, 1_600u32),
        (2_000_000u32, 400u32),
    ];
    let file = build_isobmff_with_optional_pdin(Some(&pairs), false);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open pdin fixture");
    let pdin = d.pdin.as_ref().expect("pdin parsed");

    // Exact-match rate returns the bracket's lower-endpoint delay
    // (interpolation pos == 0). The accessor sorts a scratch copy by
    // rate before lookup, so file order doesn't affect bracket
    // selection — verified by `unordered_writer_input` in unit tests.
    assert_eq!(pdin.initial_delay_for(500_000), Some(1_600));

    // Observed rate at the bracket midpoint of (100k, 8000) → (500k,
    // 1600): rate 300_000, expected delay 8000 + (1600-8000)/2 = 4800.
    assert_eq!(pdin.initial_delay_for(300_000), Some(4_800));

    // Above-range observed rate clamps to the last entry's delay
    // (the shortest). §8.1.3.1: "extrapolation from the … last entry".
    assert_eq!(pdin.initial_delay_for(10_000_000), Some(400));

    // Below-range observed rate clamps to the first entry's delay
    // (the longest, preserving the upper-estimate guarantee).
    assert_eq!(pdin.initial_delay_for(50_000), Some(8_000));
}

#[test]
fn pdin_truncated_payload_rejects_at_open_time() {
    // Hand-craft a file whose `pdin` body is one byte short of the
    // 4-byte FullBox header.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"isom");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"pdin", &[0u8; 3]); // 3-byte payload

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "a truncated pdin payload must be rejected at open time"
    );
}

#[test]
fn pdin_partial_trailing_entry_rejects_at_open_time() {
    // Hand-craft a file whose `pdin` body is FullBox header + one
    // complete pair + 4 extra bytes (a half pair) — must reject so
    // half a pair can't be silently dropped.
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    payload.extend_from_slice(&500_000u32.to_be_bytes());
    payload.extend_from_slice(&1_000u32.to_be_bytes());
    payload.extend_from_slice(&[0u8; 4]); // half-entry tail

    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"isom");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"pdin", &payload);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(
        MovDemuxer::open(cur).is_err(),
        "a half-pair pdin tail must be rejected at open time"
    );
}

#[test]
fn pdin_duplicate_kept_first() {
    // Spec defines no override semantics for a second `pdin`; the
    // demuxer keeps the first to preserve the §8.1.3.1 "early =
    // useful" promise. Build a complete one-track file and inject a
    // second `pdin` after `moov` — the first one (before `moov`)
    // must win.
    let pairs1 = [(100u32, 1_000u32)];
    let pairs2 = [(999_999u32, 99u32)];

    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08";
    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        push_atom(&mut out, *b"ftyp", &ftyp);
        push_atom(&mut out, *b"pdin", &build_pdin_payload(&pairs1));

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
        // Second pdin — must be ignored (first one wins).
        push_atom(&mut out, *b"pdin", &build_pdin_payload(&pairs2));
        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };

    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    let file = build_file(mdat_payload_offset);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = MovDemuxer::open(cur).expect("open duplicate-pdin fixture");
    let pdin = d.pdin.as_ref().expect("pdin parsed");
    assert_eq!(pdin.entries.len(), 1);
    assert_eq!(pdin.entries[0].rate, 100);
    assert_eq!(pdin.entries[0].initial_delay, 1_000);
}
