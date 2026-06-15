//! Round 307 — `MovMuxer` write-side `saiz` / `saio` emission at
//! `traf` (fragmented) scope (ISO/IEC 14496-12 §8.7.8 / §8.7.9 /
//! §8.8.14).
//!
//! Round 300 landed the `stbl`-scope (non-fragmented) write path via
//! [`MovMuxer::set_sample_aux`]; the fragmented write path still
//! ignored the attached stream. This round emits the `traf`-scope form:
//! each fragment lays its slice of the per-sample auxiliary-information
//! blobs into that fragment's `mdat` (after every track's sample data)
//! and the matching `traf` carries a `saiz` describing the per-sample
//! sizes plus a single-entry `saio` whose offset is **relative to the
//! enclosing `moof`** (§8.8.14 — the muxer always sets
//! `default-base-is-moof`).
//!
//! These tests build a fragmented file through [`MovMuxer`], demux it
//! back through [`MovDemuxer::fragment_sample_aux_info`], and confirm
//! the round-tripped `saiz` sizes, the `saio` offset, and the bytes the
//! (moof-relative) offset points at all match what was written.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    FragmentationMode, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, SampleAuxStream,
};

/// Locate every top-level `moof` box's absolute start offset by walking
/// the file's box list. The atom walker shape is
/// `[size:u32 BE][type:4][body...]`; this mirror is local to the test
/// so we can convert moof-relative `saio` offsets to absolute file
/// positions for byte verification.
fn moof_offsets(bytes: &[u8]) -> Vec<usize> {
    let mut offs = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= bytes.len() {
        let size = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let typ = &bytes[pos + 4..pos + 8];
        if typ == b"moof" {
            offs.push(pos);
        }
        if size < 8 {
            break; // size==0/1 special cases don't occur for top-level moof here
        }
        pos += size;
    }
    offs
}

/// Build a fragmented video MOV carrying a per-sample auxiliary-
/// information stream. `n` samples, sliced `frames_per_fragment` at a
/// time. Returns the encoded bytes plus the per-sample aux blobs.
fn build_fragmented_with_aux(
    n: usize,
    frames_per_fragment: u32,
    aux_info_type: Option<[u8; 4]>,
    aux_info_type_parameter: u32,
    blobs: Vec<Vec<u8>>,
) -> Vec<u8> {
    assert_eq!(blobs.len(), n);
    let samples: Vec<MuxSample> = (0..n)
        .map(|i| MuxSample {
            data: vec![(0x10 + i) as u8; 8 + (i % 4)],
            duration: 100,
            keyframe: i % frames_per_fragment as usize == 0,
            composition_offset: 0,
        })
        .collect();
    let mut m = MovMuxer::new()
        .with_movie_timescale(600)
        .with_fragmentation(FragmentationMode::ByFrameCount(frames_per_fragment));
    let id = m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 320,
            height: 240,
        },
        600,
        samples,
        &[],
    );
    m.set_sample_aux(
        id,
        SampleAuxStream {
            aux_info_type,
            aux_info_type_parameter,
            per_sample: blobs,
        },
    )
    .expect("attach sample-aux stream");
    m.encode_fragmented_to_vec()
        .expect("encode fragmented MOV with sample-aux")
}

#[test]
fn uniform_blobs_roundtrip_per_fragment_default_size_form() {
    // 6 samples, 3 per fragment ⇒ two fragments of 3.
    let blobs: Vec<Vec<u8>> = (0..6).map(|_| vec![0xABu8; 16]).collect();
    let bytes = build_fragmented_with_aux(6, 3, Some(*b"cenc"), 0, blobs.clone());

    let moofs = moof_offsets(&bytes);
    assert_eq!(moofs.len(), 2, "two media fragments expected");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
    let d = MovDemuxer::open(cur).expect("open fragmented muxed file");

    let entries = d.fragment_sample_aux_info(0);
    assert_eq!(entries.len(), 2, "one sample-aux entry per fragment");

    for (frag_idx, entry) in entries.iter().enumerate() {
        assert_eq!(entry.mfhd_sequence_number, (frag_idx as u32) + 1);
        let (saiz, saio) = entry.lookup(b"cenc", 0);
        let saiz = saiz.expect("saiz present for cenc");
        let saio = saio.expect("saio present for cenc");

        // Uniform 16-byte blobs ⇒ default-size form, 3 samples/fragment.
        assert_eq!(saiz.default_sample_info_size, 16);
        assert_eq!(saiz.sample_count, 3);
        assert!(saio.is_single_chunk());

        // saio offset is moof-relative; resolve against the moof start.
        let rel = saio.offset_for(0).expect("offset[0]") as usize;
        let abs = moofs[frag_idx] + rel;
        let expect: Vec<u8> = blobs[frag_idx * 3..frag_idx * 3 + 3]
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();
        assert_eq!(&bytes[abs..abs + expect.len()], expect.as_slice());
    }
}

#[test]
fn varying_blobs_roundtrip_per_fragment_table_form() {
    // 4 samples, 2 per fragment; per-fragment sizes differ ⇒ table form.
    let blobs = vec![vec![1u8; 8], vec![], vec![2u8; 24], vec![3u8; 5]];
    let bytes = build_fragmented_with_aux(4, 2, Some(*b"cbcs"), 9, blobs.clone());

    let moofs = moof_offsets(&bytes);
    assert_eq!(moofs.len(), 2);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
    let d = MovDemuxer::open(cur).expect("open fragmented muxed file");
    let entries = d.fragment_sample_aux_info(0);
    assert_eq!(entries.len(), 2);

    // Fragment 0: blobs [1u8;8], [] ⇒ sizes [8, 0] (table form).
    let (saiz0, saio0) = entries[0].lookup(b"cbcs", 9);
    let saiz0 = saiz0.expect("saiz frag0");
    let saio0 = saio0.expect("saio frag0");
    assert_eq!(saiz0.default_sample_info_size, 0);
    assert_eq!(saiz0.sample_count, 2);
    assert_eq!(saiz0.sample_info_sizes, vec![8u8, 0]);
    let a = saiz0.aux_info_type.expect("discriminator pair");
    assert_eq!(&a.aux_info_type, b"cbcs");
    assert_eq!(a.aux_info_type_parameter, 9);

    // Fragment 1: blobs [2u8;24], [3u8;5] ⇒ sizes [24, 5].
    let (saiz1, saio1) = entries[1].lookup(b"cbcs", 9);
    let saiz1 = saiz1.expect("saiz frag1");
    let saio1 = saio1.expect("saio frag1");
    assert_eq!(saiz1.sample_info_sizes, vec![24u8, 5]);

    // Byte verification, moof-relative.
    for (frag_idx, (saio, frag_blobs)) in [(saio0, &blobs[0..2]), (saio1, &blobs[2..4])]
        .iter()
        .enumerate()
    {
        let mut off = moofs[frag_idx] + saio.offset_for(0).expect("offset") as usize;
        for b in frag_blobs.iter() {
            assert_eq!(&bytes[off..off + b.len()], b.as_slice());
            off += b.len();
        }
    }
}

#[test]
fn implicit_discriminator_matches_zero_pair() {
    // No aux_info_type ⇒ flags & 1 clear ⇒ matches the all-zero pair.
    let blobs: Vec<Vec<u8>> = (0..4).map(|_| vec![0x77u8; 4]).collect();
    let bytes = build_fragmented_with_aux(4, 2, None, 0, blobs.clone());

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
    let d = MovDemuxer::open(cur).expect("open fragmented muxed file");
    let entries = d.fragment_sample_aux_info(0);
    assert_eq!(entries.len(), 2);

    for entry in entries {
        let (saiz, saio) = entry.lookup(&[0, 0, 0, 0], 0);
        let saiz = saiz.expect("saiz present for implicit discriminator");
        let _ = saio.expect("saio present for implicit discriminator");
        assert!(saiz.aux_info_type.is_none());
        assert_eq!(saiz.default_sample_info_size, 4);
        assert_eq!(saiz.sample_count, 2);
    }
}

#[test]
fn no_sample_aux_track_emits_no_traf_boxes() {
    // A plain fragmented track (no stream attached) carries no
    // traf-scope saiz/saio.
    let samples: Vec<MuxSample> = (0..4)
        .map(|i| MuxSample {
            data: vec![i as u8; 8],
            duration: 100,
            keyframe: i == 0,
            composition_offset: 0,
        })
        .collect();
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 16,
            height: 16,
        },
        600,
        samples,
        &[],
    );
    let bytes = m
        .encode_fragmented_to_vec()
        .expect("encode plain fragmented");

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let d = MovDemuxer::open(cur).expect("open fragmented muxed file");
    assert!(d.fragment_sample_aux_info(0).is_empty());
}

#[test]
fn aux_slab_does_not_corrupt_fragment_sample_data() {
    // The aux slab sits after the sample data in each mdat; verify the
    // demuxer still reads the actual per-fragment sample bytes via the
    // trun.data_offset path while the slab is present.
    use oxideav_core::Demuxer;
    let blobs: Vec<Vec<u8>> = (0..4).map(|_| vec![0xEEu8; 6]).collect();
    let bytes = build_fragmented_with_aux(4, 2, Some(*b"cenc"), 0, blobs);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    let mut d = MovDemuxer::open(cur).expect("open fragmented muxed file");

    // Sample i data was vec![0x10 + i; 8 + (i % 4)].
    for i in 0..4u8 {
        let pkt = d.next_packet().expect("packet");
        let want = vec![0x10 + i; 8 + (i as usize % 4)];
        assert_eq!(pkt.data, want, "sample {i} bytes intact across aux slab");
    }
}

#[test]
fn single_track_single_fragment_roundtrip() {
    // Degenerate slice: all samples in one fragment.
    let blobs = vec![vec![0x01u8; 3], vec![0x02; 3], vec![0x03; 3]];
    let bytes = build_fragmented_with_aux(3, 8, Some(*b"cenc"), 0, blobs.clone());

    let moofs = moof_offsets(&bytes);
    assert_eq!(moofs.len(), 1);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
    let d = MovDemuxer::open(cur).expect("open fragmented muxed file");
    let entries = d.fragment_sample_aux_info(0);
    assert_eq!(entries.len(), 1);

    let (saiz, saio) = entries[0].lookup(b"cenc", 0);
    let saiz = saiz.expect("saiz");
    let saio = saio.expect("saio");
    assert_eq!(saiz.default_sample_info_size, 3); // uniform 3-byte blobs
    assert_eq!(saiz.sample_count, 3);

    let abs = moofs[0] + saio.offset_for(0).expect("offset") as usize;
    let expect: Vec<u8> = blobs.iter().flat_map(|b| b.iter().copied()).collect();
    assert_eq!(&bytes[abs..abs + expect.len()], expect.as_slice());
}
