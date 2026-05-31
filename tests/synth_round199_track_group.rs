//! Round 199 — Track Group Box (`trgr`) parser wiring.
//!
//! ISO/IEC 14496-12 §8.3.4 ("Track Group Box", p. 27 of the 2015
//! edition). `trgr` sits inside `trak` (alongside `tkhd` / `mdia` /
//! `edts` / `tref` / `udta`) and holds zero or more *track-group-type*
//! FullBoxes. Each child's FourCC is the `track_group_type`, and its
//! first u32 after the FullBox header is the `track_group_id`. Two
//! tracks that contain a child with the same FourCC and the same
//! `track_group_id` belong to the same track group.
//!
//! Round 199 wires the parser into the per-`trak` walk and surfaces:
//!
//! * `Track::track_groups()` — per-track membership list, file order.
//! * `MovDemuxer::track_group_entries(track_index)` — same, by index.
//! * `MovDemuxer::tracks_in_group(track_group_type, track_group_id)` —
//!   the dual lookup: "which tracks share this group?".
//! * `MovDemuxer::track_groups()` — all `(type, id)` buckets, sorted.
//!
//! The box is ISO BMFF-only — QTFF defines no equivalent — so a plain
//! `.mov` input that omits `trak/trgr` returns empty surfaces, which
//! the absence-case test below exercises.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, TrackGroupTypeEntry, TRACK_GROUP_TYPE_MSRC};

/// Build a track-group-type FullBox body — `[ver:1][flags:3][id:4][tail]`.
/// The body is what goes *inside* the per-child atom (size + FourCC
/// header is added by `build_trgr_child` / `push_atom`).
fn build_track_group_body(track_group_id: u32, payload_tail: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&[0u8; 4]); // ver=0 + flags=0
    b.extend_from_slice(&track_group_id.to_be_bytes());
    b.extend_from_slice(payload_tail);
    b
}

/// Build a `trgr` container atom carrying the given (FourCC, body)
/// children, in order. The wrapper's size + `trgr` FourCC header is
/// added by `push_atom` when the caller installs it inside a `trak`.
fn build_trgr_container(children: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (fc, b) in children {
        push_atom(&mut body, *fc, b);
    }
    body
}

/// Build a one-video-track QTFF file with an optional track-level
/// `trgr` carriage. `trgr_children` lets the caller stack multiple
/// track-group-type children (§8.3.4 — the container's children are
/// unconstrained). mvhd ts = 600, mdhd ts = 600, 4×30-tick samples.
fn build_qt_with_trgr(trgr_children: &[([u8; 4], &[u8])]) -> Vec<u8> {
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

        if !trgr_children.is_empty() {
            let trgr_body = build_trgr_container(trgr_children);
            push_atom(&mut trak, *b"trgr", &trgr_body);
        }

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

/// Build a two-track (video + audio) fMP4-style file where each `trak`
/// may carry its own `trgr` children. `t0_trgr` and `t1_trgr` are the
/// child lists for the two tracks (empty = no `trgr` on that track).
/// Used for the `tracks_in_group` / `track_groups` dual-lookup tests.
fn build_qt_two_tracks_with_trgr(
    t0_trgr: &[([u8; 4], &[u8])],
    t1_trgr: &[([u8; 4], &[u8])],
) -> Vec<u8> {
    let mdat_payload = b"\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A\x0B\x0C\x0D\x0E\x0F\x10";
    let build_file = |chunk_off_a: u32, chunk_off_b: u32| -> Vec<u8> {
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  ");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        let mut moov = Vec::new();
        push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 120));

        // Track 0 — video.
        let mut trak0 = Vec::new();
        push_atom(&mut trak0, *b"tkhd", &build_tkhd(1, 120, 320, 240));
        let mut mdia0 = Vec::new();
        push_atom(&mut mdia0, *b"mdhd", &build_mdhd(600, 120));
        push_atom(&mut mdia0, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
        let mut minf0 = Vec::new();
        push_atom(&mut minf0, *b"vmhd", &build_vmhd());
        let mut stbl0 = Vec::new();
        push_atom(
            &mut stbl0,
            *b"stsd",
            &build_stsd_video(b"avc1", 320, 240, &[]),
        );
        push_atom(&mut stbl0, *b"stts", &build_stts_single(4, 30));
        push_atom(&mut stbl0, *b"stsc", &build_stsc_single(4));
        push_atom(&mut stbl0, *b"stsz", &build_stsz_constant(2, 4));
        push_atom(&mut stbl0, *b"stco", &build_stco_single(chunk_off_a));
        push_atom(&mut minf0, *b"stbl", &stbl0);
        push_atom(&mut mdia0, *b"minf", &minf0);
        push_atom(&mut trak0, *b"mdia", &mdia0);
        if !t0_trgr.is_empty() {
            let body = build_trgr_container(t0_trgr);
            push_atom(&mut trak0, *b"trgr", &body);
        }
        push_atom(&mut moov, *b"trak", &trak0);

        // Track 1 — audio.
        let mut trak1 = Vec::new();
        push_atom(&mut trak1, *b"tkhd", &build_tkhd(2, 120, 0, 0));
        let mut mdia1 = Vec::new();
        push_atom(&mut mdia1, *b"mdhd", &build_mdhd(600, 120));
        push_atom(&mut mdia1, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
        let mut minf1 = Vec::new();
        // smhd body — 8 bytes (ver/flags + balance + reserved).
        let smhd = vec![0u8; 8];
        push_atom(&mut minf1, *b"smhd", &smhd);
        let mut stbl1 = Vec::new();
        push_atom(
            &mut stbl1,
            *b"stsd",
            &build_stsd_audio(b"mp4a", 2, 16, 48_000, &[]),
        );
        push_atom(&mut stbl1, *b"stts", &build_stts_single(4, 30));
        push_atom(&mut stbl1, *b"stsc", &build_stsc_single(4));
        push_atom(&mut stbl1, *b"stsz", &build_stsz_constant(2, 4));
        push_atom(&mut stbl1, *b"stco", &build_stco_single(chunk_off_b));
        push_atom(&mut minf1, *b"stbl", &stbl1);
        push_atom(&mut mdia1, *b"minf", &minf1);
        push_atom(&mut trak1, *b"mdia", &mdia1);
        if !t1_trgr.is_empty() {
            let body = build_trgr_container(t1_trgr);
            push_atom(&mut trak1, *b"trgr", &body);
        }
        push_atom(&mut moov, *b"trak", &trak1);

        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", mdat_payload);
        out
    };
    let pass1 = build_file(0, 0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let mdat_payload_offset: u32 = mdat_fourcc_pos + 4;
    // Both tracks chunk into the same mdat — track 1's chunk starts
    // 8 bytes after track 0's (each is 4 samples × 2 bytes = 8 bytes).
    build_file(mdat_payload_offset, mdat_payload_offset + 8)
}

#[test]
fn trgr_msrc_single_membership_round_trips() {
    // §8.3.4.3 — the base spec's only registered track_group_type.
    let body = build_track_group_body(7, &[]);
    let bytes = build_qt_with_trgr(&[(*b"msrc", &body)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open with msrc");
    let entries: &[TrackGroupTypeEntry] = d.track_group_entries(0);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].track_group_type, TRACK_GROUP_TYPE_MSRC);
    assert_eq!(entries[0].track_group_id, 7);
    assert!(entries[0].is_msrc());
    assert_eq!(entries[0].key(), (TRACK_GROUP_TYPE_MSRC, 7));
    assert!(entries[0].payload.is_empty());
}

#[test]
fn trgr_multiple_children_preserved_in_file_order() {
    // §8.3.4 doesn't constrain the number of children — a single `trgr`
    // may carry a base-spec `msrc` membership alongside a derived-spec
    // group (e.g. ISO/IEC 14496-15 stereo-view-id). The parser must
    // preserve every child in declaration order.
    let msrc_body = build_track_group_body(11, &[]);
    let stvw_body = build_track_group_body(22, &[0x01, 0x02]);
    let bytes = build_qt_with_trgr(&[(*b"msrc", &msrc_body), (*b"stvw", &stvw_body)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open multi-child trgr");
    let entries = d.track_group_entries(0);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].key(), (*b"msrc", 11));
    assert_eq!(entries[1].key(), (*b"stvw", 22));
    assert_eq!(entries[1].payload, vec![0x01, 0x02]);
    assert!(entries[0].is_msrc());
    assert!(!entries[1].is_msrc());
}

#[test]
fn trgr_absent_from_trak_yields_empty_slice() {
    // No `trgr` child — `track_group_entries` returns an empty slice,
    // and `track_groups` returns an empty list. QTFF files (the
    // common .mov case) hit this branch every time.
    let bytes = build_qt_with_trgr(&[]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open trgr-less");
    assert!(d.track_group_entries(0).is_empty());
    assert!(d.track_groups().is_empty());
    assert!(d.tracks_in_group(TRACK_GROUP_TYPE_MSRC, 1).is_empty());
}

#[test]
fn trgr_out_of_range_track_index_returns_empty_slice() {
    let body = build_track_group_body(1, &[]);
    let bytes = build_qt_with_trgr(&[(*b"msrc", &body)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open");
    assert!(d.track_group_entries(42).is_empty());
}

#[test]
fn trgr_tracks_in_group_finds_co_members() {
    // Two tracks both declare `(msrc, 5)` → both appear in the same
    // group bucket. The §8.3.4.3 example use case: the audio + video
    // tracks of one video-telephony participant share an `msrc` id.
    let body = build_track_group_body(5, &[]);
    let bytes = build_qt_two_tracks_with_trgr(&[(*b"msrc", &body)], &[(*b"msrc", &body)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open paired msrc");
    let in_group = d.tracks_in_group(TRACK_GROUP_TYPE_MSRC, 5);
    assert_eq!(in_group, vec![0, 1]);
}

#[test]
fn trgr_tracks_in_group_distinguishes_separate_ids() {
    // Two `msrc` memberships with *different* ids — neither track
    // belongs to the other's group. This is the §8.3.4.3 multi-
    // participant example: each participant has its own msrc id, so
    // the pairs do not collapse.
    let body_a = build_track_group_body(1, &[]);
    let body_b = build_track_group_body(2, &[]);
    let bytes = build_qt_two_tracks_with_trgr(&[(*b"msrc", &body_a)], &[(*b"msrc", &body_b)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open distinct msrc");
    assert_eq!(d.tracks_in_group(TRACK_GROUP_TYPE_MSRC, 1), vec![0]);
    assert_eq!(d.tracks_in_group(TRACK_GROUP_TYPE_MSRC, 2), vec![1]);
    assert!(d.tracks_in_group(TRACK_GROUP_TYPE_MSRC, 3).is_empty());
}

#[test]
fn trgr_tracks_in_group_distinguishes_type_from_id() {
    // Same `track_group_id` (= 9) but different `track_group_type`
    // FourCCs — the spec is explicit (§8.3.4.3) that the pair
    // identifies the group, so these are *not* the same group.
    let body_msrc = build_track_group_body(9, &[]);
    let body_vend = build_track_group_body(9, &[]);
    let bytes = build_qt_two_tracks_with_trgr(&[(*b"msrc", &body_msrc)], &[(*b"vend", &body_vend)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open type-disambiguating");
    assert_eq!(d.tracks_in_group(TRACK_GROUP_TYPE_MSRC, 9), vec![0]);
    assert_eq!(d.tracks_in_group(*b"vend", 9), vec![1]);
}

#[test]
fn trgr_track_groups_buckets_sorted_ascending() {
    // `track_groups()` returns buckets sorted ascending by
    // `(track_group_type, track_group_id)`. Place two memberships in
    // different orders on the two tracks and confirm the result is
    // deterministic regardless of declaration order.
    let msrc_3 = build_track_group_body(3, &[]);
    let msrc_1 = build_track_group_body(1, &[]);
    let bytes = build_qt_two_tracks_with_trgr(&[(*b"msrc", &msrc_3)], &[(*b"msrc", &msrc_1)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open");
    let groups = d.track_groups();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].0, (*b"msrc", 1));
    assert_eq!(groups[0].1, vec![1]);
    assert_eq!(groups[1].0, (*b"msrc", 3));
    assert_eq!(groups[1].1, vec![0]);
}

#[test]
fn trgr_track_groups_dedupes_within_a_bucket() {
    // A track that lists the same `(type, id)` pair twice (legal per
    // §8.3.4 — the spec doesn't forbid duplicate rows) should appear
    // exactly once in the `track_groups()` bucket. The per-track
    // `track_group_entries` slice still surfaces both rows.
    let body = build_track_group_body(4, &[]);
    let bytes = build_qt_with_trgr(&[(*b"msrc", &body), (*b"msrc", &body)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open dup-membership");
    // Two entries on the track.
    assert_eq!(d.track_group_entries(0).len(), 2);
    // But one entry in the bucket.
    let groups = d.track_groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0, (TRACK_GROUP_TYPE_MSRC, 4));
    assert_eq!(groups[0].1, vec![0]);
}

#[test]
fn trgr_vendor_type_payload_round_trips_verbatim() {
    // A derived-spec / vendor track_group_type may carry trailing
    // bytes after `track_group_id` (§8.3.4.2 "the remaining data may
    // be specified for a particular track_group_type"). The parser
    // surfaces them verbatim in `payload`.
    let tail = b"\xCA\xFE\xBA\xBE\x00\x11\x22\x33";
    let body = build_track_group_body(0x1234_5678, tail);
    let bytes = build_qt_with_trgr(&[(*b"xvnd", &body)]);
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open vendor trgr");
    let entries = d.track_group_entries(0);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].track_group_type, *b"xvnd");
    assert_eq!(entries[0].track_group_id, 0x1234_5678);
    assert_eq!(entries[0].payload, tail.to_vec());
    assert!(!entries[0].is_msrc());
}

#[test]
fn trgr_truncated_child_body_is_rejected_at_open() {
    // A `trgr` child whose body is only 4 bytes — one short of the
    // 8-byte fixed record (FullBox header + `track_group_id`). The
    // parser must reject at open time rather than silently produce
    // a half-populated entry.
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
    // Use an obviously-bogus chunk offset — we won't reach the
    // sample read because `open` must error first.
    push_atom(&mut stbl, *b"stco", &build_stco_single(0));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);

    // Build a `trgr` containing a single msrc child with a 4-byte
    // body (below the 8-byte fixed record).
    let mut trgr_body = Vec::new();
    push_atom(&mut trgr_body, *b"msrc", &[0u8; 4]);
    push_atom(&mut trak, *b"trgr", &trgr_body);

    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    push_atom(&mut out, *b"mdat", b"\x01\x02\x03\x04\x05\x06\x07\x08");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    assert!(MovDemuxer::open(cur).is_err());
}

#[test]
fn trgr_first_wins_on_duplicate_container() {
    // §8.3.4.1 declares `Quantity: Zero or one` for the `trgr`
    // container. A malformed writer that emits two `trgr` children
    // inside one `trak` should be tolerated — we keep the first
    // and ignore the second (matching the `tapt` / `load` / `cslg`
    // / `clip` / `matt` conservative-merge policy at trak scope).
    let body_a = build_track_group_body(100, &[]);
    let body_b = build_track_group_body(200, &[]);

    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");

    let build_file = |chunk_offset: u32| -> Vec<u8> {
        let mut out = Vec::new();
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
        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        let trgr_a = build_trgr_container(&[(*b"msrc", &body_a)]);
        push_atom(&mut trak, *b"trgr", &trgr_a);
        let trgr_b = build_trgr_container(&[(*b"msrc", &body_b)]);
        push_atom(&mut trak, *b"trgr", &trgr_b);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);
        push_atom(&mut out, *b"mdat", b"\x01\x02\x03\x04\x05\x06\x07\x08");
        out
    };
    let pass1 = build_file(0);
    let mdat_fourcc_pos = pass1.windows(4).position(|w| w == b"mdat").unwrap() as u32;
    let bytes = build_file(mdat_fourcc_pos + 4);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open dup-trgr");
    let entries = d.track_group_entries(0);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].track_group_id, 100);
}
