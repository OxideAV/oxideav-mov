//! Round 279 regression: scheduled-fuzz finding
//! `oom-33f049eec4ac8b768b06765a75ed350bfeb5a331` — a `keys` atom
//! declaring `entry_count = 0x0a0a0a0a` (≈168 M) drove
//! `Vec::with_capacity(entry_count)` into a single ~5.4 GB allocation
//! (168 430 090 × the 32-byte in-memory key tuple) before the
//! per-entry loop could reject the obviously-truncated table.
//!
//! The fix front-loads the byte-bound check that already exists in the
//! `ssix` / `leva` parsers: a declared count whose minimum on-disk
//! footprint exceeds the remaining body bytes is rejected before any
//! count-sized allocation. The same audit closed the sibling
//! count-driven pre-allocation sites reachable from
//! `MovDemuxer::open`: `parse_chan`, `parse_dref`, `parse_stsd`, and
//! `parse_sgpd` (which additionally could be pushed into unbounded
//! `Vec` growth through its deprecated-v0 zero-implicit-size
//! fallback).

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::media_meta::parse_keys;
use oxideav_mov::MovDemuxer;

/// The verbatim libFuzzer reproducer (82 bytes).
const CRASH: &[u8] = &[
    0x00, 0x00, 0x00, 0x3c, 0x6d, 0x6f, 0x6f, 0x76, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x61,
    0x00, 0x00, 0x00, 0x2a, 0x6d, 0x65, 0x74, 0x61, 0x00, 0x00, 0x00, 0x11, 0x6b, 0x65, 0x79, 0x73,
    0x6d, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x00,
    0x6d, 0x00, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a,
    0x0a, 0x0a, 0x0a, 0x0a, 0x00, 0x6d, 0x00, 0x00, 0x00, 0x08, 0x6d, 0x65, 0x74, 0x61, 0x00, 0x00,
    0x61, 0x00,
];

#[test]
fn fuzz_oom_reproducer_no_longer_allocates() {
    // Pre-fix this OOM-killed the process at `Vec::with_capacity`;
    // post-fix the demuxer walks the file without any count-sized
    // allocation. The malformed `keys` table is dropped by the lenient
    // `meta` walker, so `open` itself may succeed — the assertion is
    // simply that we get *an* answer cheaply.
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(CRASH.to_vec()));
    let _ = MovDemuxer::open(cur);
}

#[test]
fn keys_overdeclared_count_rejected_before_allocation() {
    // 8-byte header + 12 bytes of body, but a declared count of
    // 0x0a0a0a0a entries. Each entry needs >= 8 bytes, so the count
    // can never fit — the parser must reject on arithmetic alone.
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    payload.extend_from_slice(&0x0a0a_0a0au32.to_be_bytes()); // count
    payload.extend_from_slice(&[0u8; 12]);
    assert!(parse_keys(&payload).is_err());
}

#[test]
fn keys_count_exactly_fitting_still_accepted() {
    // Boundary: a count whose minimum footprint exactly equals the
    // remaining bytes parses fine (one minimal 8-byte entry: size = 8,
    // namespace `mdta`, empty key string).
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    payload.extend_from_slice(&1u32.to_be_bytes()); // count = 1
    payload.extend_from_slice(&8u32.to_be_bytes()); // entry size = 8
    payload.extend_from_slice(b"mdta"); // namespace
    let keys = parse_keys(&payload).expect("boundary count accepted");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].0, "");
    assert_eq!(&keys[0].1, b"mdta");
}

/// Build a minimal valid-walker file wrapping one `stbl` child so the
/// per-track parse reaches the box under test.
fn wrap_in_stbl(stbl_children: &[u8]) -> Vec<u8> {
    fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], payload: &[u8]) {
        out.extend_from_slice(&((payload.len() as u32) + 8).to_be_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(payload);
    }
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mut mvhd = vec![0u8; 100];
    mvhd[15] = 100; // timescale
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &mvhd);

    let mut tkhd = vec![0u8; 84];
    tkhd[23] = 1; // track id
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &tkhd);

    let mut mdhd = vec![0u8; 24];
    mdhd[15] = 100;
    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&0u32.to_be_bytes());
    hdlr.extend_from_slice(b"mhlr");
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0u8; 12]);
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &mdhd);
    push_atom(&mut mdia, *b"hdlr", &hdlr);

    let mut minf = Vec::new();
    let mut stbl = Vec::new();
    stbl.extend_from_slice(stbl_children);
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn sgpd_v0_overdeclared_count_rejected_not_grown() {
    // Deprecated v0 `sgpd` with an unknown grouping_type and a count
    // far above the body's bytes previously pushed `entry_count`
    // zero-length entries (unbounded Vec growth). Now rejected at
    // open time; the demuxer drops the malformed track-level box but
    // must complete without ballooning memory.
    let mut sgpd = Vec::new();
    sgpd.extend_from_slice(&0u32.to_be_bytes()); // version 0 + flags
    sgpd.extend_from_slice(b"zzzz"); // unknown grouping_type
    sgpd.extend_from_slice(&0x0a0a_0a0au32.to_be_bytes()); // entry_count
    sgpd.extend_from_slice(&[0u8; 4]); // 4 body bytes << count

    let mut stbl_children = Vec::new();
    stbl_children.extend_from_slice(&((sgpd.len() as u32) + 8).to_be_bytes());
    stbl_children.extend_from_slice(b"sgpd");
    stbl_children.extend_from_slice(&sgpd);

    let bytes = wrap_in_stbl(&stbl_children);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    // Whether open errors or tolerates the dropped box, it must
    // return promptly without a count-sized allocation.
    let _ = MovDemuxer::open(cur);
}

#[test]
fn stsd_overdeclared_count_capped_not_preallocated() {
    // `stsd` declaring u32::MAX entries over an 16-byte body: the loop
    // rejects it as truncated, and the pre-allocation must stay capped
    // at the byte-backed bound (1 entry) rather than count-sized.
    let mut stsd = Vec::new();
    stsd.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    stsd.extend_from_slice(&u32::MAX.to_be_bytes()); // entry count
    stsd.extend_from_slice(&16u32.to_be_bytes()); // entry size
    stsd.extend_from_slice(b"abcd"); // format
    stsd.extend_from_slice(&[0u8; 8]); // reserved + dref index

    let mut stbl_children = Vec::new();
    stbl_children.extend_from_slice(&((stsd.len() as u32) + 8).to_be_bytes());
    stbl_children.extend_from_slice(b"stsd");
    stbl_children.extend_from_slice(&stsd);

    let bytes = wrap_in_stbl(&stbl_children);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let _ = MovDemuxer::open(cur);
}
