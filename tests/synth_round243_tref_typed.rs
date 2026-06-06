//! Round 243 — typed accessors for the remaining QTFF Table 2-2
//! `tref` reference kinds (`sync` / `scpt` / `hint` / `ssrc`), plus the
//! demuxer-side track-id-to-index resolvers that translate every
//! `tref` row into 0-based indices inside [`MovDemuxer::tracks`].
//!
//! The QTFF specification, Apple QuickTime File Format Specification
//! (2001-03-01) pp. 49–51 (Track Reference Atoms, Figure 2-13,
//! Table 2-2), enumerates six standard `tref` reference-type FourCCs:
//!
//! * `'tmcd'` — Time code (existing surface).
//! * `'chap'` — Chapter or scene list (existing surface).
//! * `'sync'` — Synchronization. Usually between a video and sound
//!   track.
//! * `'scpt'` — Transcript. Usually references a text track.
//! * `'ssrc'` — Nonprimary source.
//! * `'hint'` — Hint-track source media.
//!
//! Round 240 left `Track::chapter_track_ref()` and
//! `Track::timecode_track_ref()` typed but the remaining four kinds
//! were reachable only via the generic `Track::track_refs_of_kind()`
//! helper. Round 243 adds the four-symmetric typed
//! `Track::sync_track_refs()` / `Track::transcript_track_refs()` /
//! `Track::hint_track_refs()` /
//! `Track::non_primary_source_track_refs()` accessors, plus the
//! demuxer-side `MovDemuxer::track_index_for_id(track_id)` lookup and
//! per-kind `Mov::sync_track_indices(track_index)` /
//! `MovDemuxer::transcript_track_indices` /
//! `MovDemuxer::hint_track_indices` /
//! `MovDemuxer::non_primary_source_track_indices` /
//! `MovDemuxer::timecode_track_index` resolvers that translate the
//! 1-based `track_id` rows into 0-based [`MovDemuxer::tracks`]
//! indices. A generic `MovDemuxer::tref_track_indices(track_index,
//! kind)` underpins each.
//!
//! The 0-id slot (QTFF p. 51 "Unused entries in the atom may have a
//! track ID value of 0") and unresolvable ids (writer slip — the
//! pointed-at id is missing from the file) are filtered out so
//! callers see only resolvable indices. Order is preserved across
//! every reference-type atom of the requested kind inside the source
//! track's `tref`.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, TrackRefKind};

/// Build a `tref` payload carrying the given reference rows. Each row
/// is one reference-type atom — a FourCC header followed by a packed
/// list of big-endian u32 track-ids.
fn build_tref(rows: &[([u8; 4], &[u32])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (fc, ids) in rows {
        let mut body = Vec::with_capacity(ids.len() * 4);
        for id in *ids {
            body.extend_from_slice(&id.to_be_bytes());
        }
        push_atom(&mut out, *fc, &body);
    }
    out
}

/// Build a minimal video-track `trak` carrying the supplied `tref`
/// payload. mvhd/mdhd timescale = 600; one 8-byte sample at the
/// caller-supplied chunk offset.
fn build_video_trak(track_id: u32, tref_payload: &[u8], chunk_offset: u32) -> Vec<u8> {
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(track_id, 60, 320, 240));
    if !tref_payload.is_empty() {
        push_atom(&mut trak, *b"tref", tref_payload);
    }
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 60));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 60));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    trak
}

/// Build a minimal audio-track `trak` with the supplied `tref`. Same
/// 8-byte chunk pattern as `build_video_trak`.
fn build_audio_trak(track_id: u32, tref_payload: &[u8], chunk_offset: u32) -> Vec<u8> {
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(track_id, 60, 0, 0));
    if !tref_payload.is_empty() {
        push_atom(&mut trak, *b"tref", tref_payload);
    }
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 60));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"smhd", &[0u8; 8]);
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_audio(b"mp4a", 2, 16, 48000, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 60));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    trak
}

/// Build a four-track movie:
///   * track 1 (video) declares `tref` with `sync→2`, `hint→4`,
///     `scpt→3`, `ssrc→3`, `tmcd→4`, plus a `0`-valued slot and a
///     non-resident `99` to exercise the spec p. 51 / writer-slip
///     filtering on every per-kind accessor.
///   * track 2 (audio) declares no `tref`.
///   * track 3 (video) declares no `tref`.
///   * track 4 (video) declares no `tref`.
fn build_four_track_movie() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    // mdat: 4 × 8-byte payloads, one per track.
    let mut mdat_body = Vec::new();
    mdat_body.extend_from_slice(b"TRACK1!!");
    mdat_body.extend_from_slice(b"TRACK2!!");
    mdat_body.extend_from_slice(b"TRACK3!!");
    mdat_body.extend_from_slice(b"TRACK4!!");
    let mdat_payload_off = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &mdat_body);
    let t1_off = mdat_payload_off;
    let t2_off = mdat_payload_off + 8;
    let t3_off = mdat_payload_off + 16;
    let t4_off = mdat_payload_off + 24;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 60));

    // Track 1 `tref` rows: `sync→[2]`, `hint→[4]`, `scpt→[3]`,
    // `ssrc→[3, 0]` (the 0 is the spec p. 51 unused-slot sentinel),
    // `tmcd→[4, 99]` (the 99 has no corresponding track — a writer
    // slip the resolver must filter out).
    let tref1 = build_tref(&[
        (*b"sync", &[2u32]),
        (*b"hint", &[4u32]),
        (*b"scpt", &[3u32]),
        (*b"ssrc", &[3u32, 0u32]),
        (*b"tmcd", &[4u32, 99u32]),
    ]);
    push_atom(&mut moov, *b"trak", &build_video_trak(1, &tref1, t1_off));
    push_atom(&mut moov, *b"trak", &build_audio_trak(2, &[], t2_off));
    push_atom(&mut moov, *b"trak", &build_video_trak(3, &[], t3_off));
    push_atom(&mut moov, *b"trak", &build_video_trak(4, &[], t4_off));

    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn typed_track_ref_accessors_resolve_track_ids() {
    let bytes = build_four_track_movie();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open four-track fixture");
    assert_eq!(d.tracks.len(), 4);

    // Track 1 declares the full menu of references; the 0-valued slot
    // (QTFF p. 51 unused-entry sentinel) gets filtered out so the
    // returned lists only carry resolvable 1-based track-ids.
    let t1 = &d.tracks[0];
    assert_eq!(t1.sync_track_refs(), vec![2u32]);
    assert_eq!(t1.hint_track_refs(), vec![4u32]);
    assert_eq!(t1.transcript_track_refs(), vec![3u32]);
    assert_eq!(t1.non_primary_source_track_refs(), vec![3u32]);
    assert_eq!(t1.timecode_track_ref(), Some(4u32));
}

#[test]
fn empty_typed_accessors_when_no_tref_declared() {
    let bytes = build_four_track_movie();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open four-track fixture");

    // Tracks 2/3/4 carry no `tref` — every typed accessor returns the
    // empty surface (`Vec::new()` for the multi-entry kinds, `None`
    // for the single-entry chapter / timecode kinds).
    for ti in [1, 2, 3] {
        let t = &d.tracks[ti];
        assert!(t.sync_track_refs().is_empty());
        assert!(t.hint_track_refs().is_empty());
        assert!(t.transcript_track_refs().is_empty());
        assert!(t.non_primary_source_track_refs().is_empty());
        assert_eq!(t.chapter_track_ref(), None);
        assert_eq!(t.timecode_track_ref(), None);
    }
}

#[test]
fn track_index_for_id_resolves_and_rejects() {
    let bytes = build_four_track_movie();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open four-track fixture");

    assert_eq!(d.track_index_for_id(1), Some(0));
    assert_eq!(d.track_index_for_id(2), Some(1));
    assert_eq!(d.track_index_for_id(3), Some(2));
    assert_eq!(d.track_index_for_id(4), Some(3));
    // The 0 sentinel (QTFF p. 51 unused-entry slot) maps to `None`.
    assert_eq!(d.track_index_for_id(0), None);
    // A `track_id` the file doesn't declare — a writer slip — also
    // returns `None`.
    assert_eq!(d.track_index_for_id(99), None);
}

#[test]
fn demuxer_resolves_each_kind_to_zero_based_indices() {
    let bytes = build_four_track_movie();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open four-track fixture");

    // Track 1 carries every kind; the resolvers translate the 1-based
    // track-ids declared in `tref` into the 0-based `tracks` indices.
    assert_eq!(d.sync_track_indices(0), vec![1]); // sync → track-id 2 → index 1
    assert_eq!(d.hint_track_indices(0), vec![3]); // hint → track-id 4 → index 3
    assert_eq!(d.transcript_track_indices(0), vec![2]); // scpt → track-id 3 → index 2
                                                        // ssrc → [3, 0]; the `0` sentinel is filtered out so a single
                                                        // index survives.
    assert_eq!(d.non_primary_source_track_indices(0), vec![2]);
    // timecode_track_index returns the first resolvable entry; the
    // second slot (track-id 99) is unresolvable and filtered out.
    assert_eq!(d.timecode_track_index(0), Some(3));
    // tref_track_indices on every kind returns the same as the
    // per-kind helper (the per-kind helpers are documented as thin
    // wrappers).
    assert_eq!(d.tref_track_indices(0, TrackRefKind::Sync), vec![1]);
    assert_eq!(d.tref_track_indices(0, TrackRefKind::Hint), vec![3]);
    assert_eq!(d.tref_track_indices(0, TrackRefKind::Transcript), vec![2]);
    assert_eq!(
        d.tref_track_indices(0, TrackRefKind::NonPrimarySource),
        vec![2]
    );
    assert_eq!(d.tref_track_indices(0, TrackRefKind::Timecode), vec![3]);
}

#[test]
fn demuxer_resolvers_filter_unresolvable_and_zero_slots() {
    // Build a single-track movie whose `tref` carries one row per
    // kind where every entry is either `0` (the QTFF p. 51 unused-
    // slot sentinel) or a `track_id` the file does not declare
    // (`9999`). The per-kind index resolvers must return empty `Vec`s.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mdat_payload_off = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", b"FILLER!!");
    let chunk_off = mdat_payload_off;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 60));

    let tref = build_tref(&[
        (*b"sync", &[0u32, 9999u32]),
        (*b"hint", &[9999u32]),
        (*b"scpt", &[0u32]),
        (*b"ssrc", &[9999u32, 0u32]),
        (*b"tmcd", &[9999u32]),
        (*b"chap", &[9999u32]),
    ]);
    push_atom(&mut moov, *b"trak", &build_video_trak(1, &tref, chunk_off));
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open filter fixture");
    assert_eq!(d.tracks.len(), 1);

    assert!(d.sync_track_indices(0).is_empty());
    assert!(d.hint_track_indices(0).is_empty());
    assert!(d.transcript_track_indices(0).is_empty());
    assert!(d.non_primary_source_track_indices(0).is_empty());
    assert_eq!(d.timecode_track_index(0), None);

    // Track-level typed accessors mirror the same filtering: the
    // 0-slot sentinel is discarded so callers see only resolvable
    // 1-based track-ids (the unresolvable `9999` is still present at
    // the track-level surface because the writer did emit it; the
    // demuxer-level resolver is the layer that decides whether the
    // pointed-at track is actually in the file).
    let t1 = &d.tracks[0];
    assert_eq!(t1.sync_track_refs(), vec![9999u32]);
    assert_eq!(t1.hint_track_refs(), vec![9999u32]);
    assert_eq!(t1.transcript_track_refs(), Vec::<u32>::new()); // single 0-slot
    assert_eq!(t1.non_primary_source_track_refs(), vec![9999u32]);
    assert_eq!(t1.timecode_track_ref(), Some(9999u32));
    assert_eq!(t1.chapter_track_ref(), Some(9999u32));
}

#[test]
fn out_of_range_track_index_returns_empty_surfaces() {
    let bytes = build_four_track_movie();
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open four-track fixture");

    // Out-of-range `track_index` returns empty surfaces on every
    // per-kind index resolver — the accessor stays a total function
    // so a caller iterating `0..usize::MAX` (silly but defensible)
    // never panics.
    assert!(d.sync_track_indices(999).is_empty());
    assert!(d.hint_track_indices(999).is_empty());
    assert!(d.transcript_track_indices(999).is_empty());
    assert!(d.non_primary_source_track_indices(999).is_empty());
    assert_eq!(d.timecode_track_index(999), None);
    assert!(d.tref_track_indices(999, TrackRefKind::Sync).is_empty());
}

#[test]
fn order_preserved_across_multiple_rows_of_same_kind() {
    // QTFF p. 49 allows multiple reference-type atoms of the same
    // kind inside one `tref` (e.g. "multiple hint references for an
    // RTP source"). The typed Track-level accessor and the demuxer-
    // level resolver both preserve declaration order across every
    // row.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);

    let mut mdat_body = Vec::new();
    mdat_body.extend_from_slice(b"TRACK1!!");
    mdat_body.extend_from_slice(b"TRACK2!!");
    mdat_body.extend_from_slice(b"TRACK3!!");
    mdat_body.extend_from_slice(b"TRACK4!!");
    let mdat_payload_off = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &mdat_body);

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 60));

    // Two separate `hint` rows: first `[3]`, then `[4, 2]`. Joined
    // declaration order is `[3, 4, 2]`.
    let tref = build_tref(&[(*b"hint", &[3u32]), (*b"hint", &[4u32, 2u32])]);
    push_atom(
        &mut moov,
        *b"trak",
        &build_video_trak(1, &tref, mdat_payload_off),
    );
    push_atom(
        &mut moov,
        *b"trak",
        &build_audio_trak(2, &[], mdat_payload_off + 8),
    );
    push_atom(
        &mut moov,
        *b"trak",
        &build_video_trak(3, &[], mdat_payload_off + 16),
    );
    push_atom(
        &mut moov,
        *b"trak",
        &build_video_trak(4, &[], mdat_payload_off + 24),
    );
    push_atom(&mut out, *b"moov", &moov);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(out));
    let d = MovDemuxer::open(cur).expect("open multi-row fixture");
    assert_eq!(d.tracks.len(), 4);

    // Track-level typed accessor: declaration order preserved.
    assert_eq!(d.tracks[0].hint_track_refs(), vec![3u32, 4u32, 2u32]);
    // Demuxer-level index resolver: same order, translated to
    // 0-based indices (3 → idx 2, 4 → idx 3, 2 → idx 1).
    assert_eq!(d.hint_track_indices(0), vec![2usize, 3usize, 1usize]);
}
