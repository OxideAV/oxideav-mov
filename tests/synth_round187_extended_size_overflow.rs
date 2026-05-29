//! Round 187 — `largesize` u64 overflow regression for the atom walker.
//!
//! `cargo-fuzz` (target `demux`, harness `fuzz/fuzz_targets/demux.rs`)
//! produced crash-353fbd8c75a517f36da693fcea9b24d24240fc5e on the
//! scheduled run that landed after round 182:
//!
//! ```text
//! 00 00 00 08 00 00 00 00         # atom #1: size=8, type=\0\0\0\0
//! 00 00 00 01 09 ff ff ff         # atom #2: size=1 (extended), type=\tÿÿÿ
//! ff ff ff ff ff ff ff ff         # largesize = u64::MAX
//! …trailing garbage…
//! ```
//!
//! The top-level walker computes
//! `body_end = hdr.payload_offset + (total_size - hdr.header_len)` in
//! `src/demuxer.rs:480` (and the matching arithmetic lives in
//! `src/atom.rs:263` for `walk_children` and in `src/demuxer.rs:357`
//! for `probe_reference_movies`). When `total_size = u64::MAX` and
//! `payload_offset = 24` (the second atom starts at offset 8, its
//! 16-byte header lands payload at 24), the addition
//! `24 + (u64::MAX - 16) = u64::MAX + 8` overflows `u64`. Debug builds
//! panic with `attempt to add with overflow`; release builds silently
//! wrap to a small value, then either pass the past-EOF guard (because
//! the wrapped result is below `total_len`) or trigger an unrelated
//! parse error far from the actual cause.
//!
//! Round 187 closes this at the source: `read_atom_header` now
//! rejects any header whose declared `start + total_size` overflows
//! `u64`. Every downstream `payload_offset + payload_len()` /
//! `body_end` arithmetic site inherits the bound automatically: once
//! we know `start + total_size <= u64::MAX`, the algebraically equal
//! `payload_offset + (total_size - header_len)` also fits.
//!
//! Coverage shape:
//!
//! * `crash_353f_input_does_not_panic` — replays the exact crash bytes
//!   end-to-end through `MovDemuxer::open` and asserts a clean `Err`
//!   surfaces. This pins the fuzz finding so future regressions in
//!   the walker re-fail this test rather than silently re-introducing
//!   the panic.
//! * `extended_size_overflows_u64_rejected_at_header` — focuses the
//!   defense on the new `read_atom_header` check. A second atom at
//!   offset 8 with `largesize = u64::MAX` is rejected before the
//!   walker ever computes `body_end`.
//! * `extended_size_one_below_overflow_accepted` — boundary check:
//!   `start + largesize = u64::MAX` exactly is fine (the `checked_add`
//!   guard is overflow-only, not "near the limit"). We can't prove
//!   acceptance via the full demuxer (the body would extend past the
//!   actual 8 KiB file), so we drive `read_atom_header` directly and
//!   assert it returns `Ok(Some(_))` with the declared size intact.
//! * `extended_size_overflow_at_walk_children` — the same overflow
//!   shape inside a nested container (a synthetic `moov` whose only
//!   child is a `size=1` atom with `largesize=u64::MAX`). Without the
//!   header-level guard, `walk_children`'s
//!   `hdr.payload_offset + (t - hdr.header_len)` (atom.rs:263)
//!   would overflow on the same arithmetic. After the fix the child
//!   header is rejected before `walk_children` reaches that line.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{read_atom_header, MovDemuxer};

/// The exact 91-byte input libFuzzer produced. Encoded as a Rust
/// literal so the regression travels with the test rather than as
/// an opaque fixture file.
fn crash_353f_input() -> Vec<u8> {
    let mut v = Vec::with_capacity(91);
    // Atom #1: size=8, fourcc=\0\0\0\0. Eight bytes, no body.
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00]);
    // Atom #2: size=1 (extended), fourcc=\t\xff\xff\xff (non-ASCII).
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x09, 0xff, 0xff, 0xff]);
    // largesize = u64::MAX
    v.extend_from_slice(&[0xff; 8]);
    // Trailing bytes from the libFuzzer reproducer (irrelevant to the
    // crash — the walker never reaches them — but kept verbatim so
    // this fixture matches the on-disk artifact byte-for-byte).
    v.extend_from_slice(&[0xff; 64]);
    v.extend_from_slice(&[0x00, 0x00, 0xfe]);
    v
}

#[test]
fn crash_353f_input_does_not_panic() {
    let bytes = crash_353f_input();
    let rs: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    // Before round 187 this panics on a debug build with
    // `attempt to add with overflow` inside `MovDemuxer::open`.
    let res = MovDemuxer::open(rs);
    assert!(
        res.is_err(),
        "fuzz crash input must surface as Err, not Ok(_); panic-free is the headline contract"
    );
}

#[test]
fn extended_size_overflows_u64_rejected_at_header() {
    // Drive `read_atom_header` directly with a `size=1 largesize=u64::MAX`
    // atom anchored at a non-zero start. The header must be rejected
    // before any body_end arithmetic happens downstream.
    let mut bytes = Vec::new();
    // Leading 8 bytes to push `start` to 8 — exactly the shape of the
    // libFuzzer crash, isolated.
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00]);
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(b"uuid");
    bytes.extend_from_slice(&u64::MAX.to_be_bytes());
    let mut cur = Cursor::new(bytes);
    // Skip atom #1.
    use std::io::Seek;
    cur.seek(std::io::SeekFrom::Start(8)).unwrap();
    let err = read_atom_header(&mut cur).expect_err("u64 overflow must be rejected at header read");
    let msg = format!("{err}");
    assert!(
        msg.contains("overflow") && msg.contains("uuid"),
        "expected u64-overflow rejection naming the atom, got: {msg}"
    );
}

#[test]
fn extended_size_one_below_overflow_accepted() {
    // Boundary case: `start + largesize == u64::MAX` is still
    // representable, so `checked_add` returns `Some(_)` and the
    // header is accepted. We can't drive the full demuxer here (the
    // body would extend past the 16-byte cursor) but `read_atom_header`
    // must accept the framing and let downstream layers decide what
    // to do with it.
    let largesize = u64::MAX; // start is 0, so start + largesize = u64::MAX
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(b"mdat");
    bytes.extend_from_slice(&largesize.to_be_bytes());
    let mut cur = Cursor::new(bytes);
    let hdr = read_atom_header(&mut cur)
        .expect("header at start=0 with largesize=u64::MAX does not overflow")
        .expect("a 16-byte header is present");
    assert_eq!(hdr.fourcc, *b"mdat");
    assert_eq!(hdr.total_size, Some(u64::MAX));
    assert_eq!(hdr.header_len, 16);
    assert_eq!(hdr.payload_offset, 16);
}

#[test]
fn extended_size_overflow_at_walk_children() {
    // The same overflow shape nested inside a container. `walk_children`
    // (`src/atom.rs:263`) carries the same body_end arithmetic, so
    // without the header-level guard it would re-trigger the panic.
    // After round 187 the offending child header is rejected before
    // `walk_children` computes `payload_offset + (t - header_len)`.
    let mut child = Vec::new();
    child.extend_from_slice(&1u32.to_be_bytes());
    child.extend_from_slice(b"trak");
    child.extend_from_slice(&u64::MAX.to_be_bytes());

    let mut moov = Vec::new();
    // Container header: `size` = 8 + child.len(), type=`moov`.
    let total = 8u32 + child.len() as u32;
    moov.extend_from_slice(&total.to_be_bytes());
    moov.extend_from_slice(b"moov");
    moov.extend_from_slice(&child);

    let mut file = Vec::new();
    // Minimal `ftyp` so the demuxer accepts the brand handshake. We
    // craft it directly rather than via the test helper because the
    // demuxer is happy with a single-brand `ftyp` ahead of the moov.
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    let ftyp_size = (8 + ftyp.len()) as u32;
    file.extend_from_slice(&ftyp_size.to_be_bytes());
    file.extend_from_slice(b"ftyp");
    file.extend_from_slice(&ftyp);
    file.extend_from_slice(&moov);

    let rs: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let res = MovDemuxer::open(rs);
    assert!(
        res.is_err(),
        "nested largesize=u64::MAX must surface Err, not panic; got Ok"
    );
}
