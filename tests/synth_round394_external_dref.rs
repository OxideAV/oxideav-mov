//! Round 394 — **external data-reference handling** on demux. A
//! sample description whose `data_reference_index` resolves to a
//! non-self-referencing `dref` entry declares that its media bytes
//! live in *another* file (QTFF p. 65 / ISO/IEC 14496-12 §8.7.2.1 —
//! the `0x000001` self-reference flag "means the media data is in the
//! same file"). The demuxer previously read the chunk offsets against
//! the local file anyway, silently emitting whatever bytes sat there.
//! Now such samples yield a recoverable `Unsupported` error (the
//! cursor advances, so a mixed movie keeps demuxing its local
//! tracks), and the surface is queryable via `sample_data_in_file` /
//! `track_has_external_data`.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::MovDemuxer;

/// `dref` with entry 1 = external URL (flags = 0), entry 2 = self.
fn dref_external_first() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&2u32.to_be_bytes()); // n = 2
    let url = b"http://example.com/media.mov\0";
    let size = (12 + url.len()) as u32;
    p.extend_from_slice(&size.to_be_bytes());
    p.extend_from_slice(b"url ");
    p.extend_from_slice(&[0, 0, 0, 0]); // ver + flags = 0 (external)
    p.extend_from_slice(url);
    p.extend_from_slice(&12u32.to_be_bytes());
    p.extend_from_slice(b"url ");
    p.extend_from_slice(&[0, 0, 0, 1]); // ver + flags = 1 (self)
    p
}

/// `dref` with a single self-referencing entry.
fn dref_self_only() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(&12u32.to_be_bytes());
    p.extend_from_slice(b"url ");
    p.extend_from_slice(&[0, 0, 0, 1]);
    p
}

/// One-video-track `trak` whose stsd points at `dref` entry `dri`.
fn build_trak(track_id: u32, dref: &[u8], dri: u16, chunk_offset: u32) -> Vec<u8> {
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(track_id, 30, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut dinf = Vec::new();
    push_atom(&mut dinf, *b"dref", dref);
    push_atom(&mut minf, *b"dinf", &dinf);
    let mut stbl = Vec::new();
    // Patch the data_reference_index inside the stsd entry: the
    // common builder writes `1`; the field sits at entry offset 14
    // (after size:4 + format:4 + reserved:6), i.e. stsd payload
    // offset 8 + 14.
    let mut stsd = build_stsd_video(b"avc1", 320, 240, &[]);
    stsd[22..24].copy_from_slice(&dri.to_be_bytes());
    push_atom(&mut stbl, *b"stsd", &stsd);
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(chunk_offset));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    trak
}

/// Two tracks sharing the 8-byte mdat: track 1 references external
/// media (dref entry 1, flags 0), track 2 is fully local.
fn build_mixed_movie() -> Vec<u8> {
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"mdat", b"LOCALPAY");
    let payload_off: u32 = 28; // ftyp(20) + mdat header(8)

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));
    push_atom(
        &mut moov,
        *b"trak",
        &build_trak(1, &dref_external_first(), 1, payload_off),
    );
    push_atom(
        &mut moov,
        *b"trak",
        &build_trak(2, &dref_self_only(), 1, payload_off),
    );
    push_atom(&mut out, *b"moov", &moov);
    out
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open")
}

#[test]
fn external_sample_yields_recoverable_unsupported() {
    let mut d = open(build_mixed_movie());
    assert!(d.track_has_external_data(0));
    assert!(!d.track_has_external_data(1));

    // Drain: the external track's sample errors (recoverably); the
    // local track's sample still comes through with its bytes.
    let mut local_payloads = Vec::new();
    let mut external_errors = 0;
    loop {
        match d.next_packet() {
            Ok(p) => local_payloads.push((p.stream_index, p.data.clone())),
            Err(e) => {
                let msg = format!("{e}");
                if msg.contains("external media") {
                    external_errors += 1;
                    continue; // recoverable: keep demuxing
                }
                break; // Eof
            }
        }
    }
    assert_eq!(external_errors, 1);
    assert_eq!(local_payloads.len(), 1);
    assert_eq!(local_payloads[0].0, 1);
    assert_eq!(local_payloads[0].1, b"LOCALPAY");
}

#[test]
fn sample_data_in_file_reflects_dref_target() {
    let d = open(build_mixed_movie());
    let ext_sample = d.tracks[0]
        .sample_table
        .iter_samples()
        .next()
        .unwrap()
        .unwrap();
    let loc_sample = d.tracks[1]
        .sample_table
        .iter_samples()
        .next()
        .unwrap()
        .unwrap();
    assert!(!d.sample_data_in_file(0, &ext_sample));
    assert!(d.sample_data_in_file(1, &loc_sample));
}

#[test]
fn self_pointing_dri_in_multi_entry_table_stays_local() {
    // Same two-entry dref, but the stsd points at entry 2 (the
    // self-reference): everything demuxes locally.
    let mut out = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut out, *b"ftyp", &ftyp);
    push_atom(&mut out, *b"mdat", b"LOCALPAY");
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));
    push_atom(
        &mut moov,
        *b"trak",
        &build_trak(1, &dref_external_first(), 2, 28),
    );
    push_atom(&mut out, *b"moov", &moov);

    let mut d = open(out);
    assert!(!d.track_has_external_data(0));
    let p = d.next_packet().expect("local sample demuxes");
    assert_eq!(p.data, b"LOCALPAY");
}
