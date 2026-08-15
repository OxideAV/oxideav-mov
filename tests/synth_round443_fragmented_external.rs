//! Round 443 — **fragmented external-data semantics** (ISO/IEC
//! 14496-12 §8.8.7).
//!
//! §8.8.4.1 sanctions fragmented presentations whose media lives
//! outside the metadata file: "The data reference index is in the
//! sample description, so it is possible to build incremental
//! presentations where the media data is in files other than the file
//! containing the Movie Box." The §8.8.7.1 base-data-offset then
//! resolves against the data reference:
//!
//! * an **explicit** `base_data_offset` "is a data offset that is
//!   identical to a chunk offset in the Chunk Offset Box" — and a
//!   chunk offset addresses "its containing media file" (§8.7.5.3),
//!   i.e. the stream the effective `dref` entry designates;
//! * `default-base-is-moof` anchors at "the position of the first
//!   byte of the enclosing Movie Fragment Box" — a byte position of
//!   the fragment's own file, incompatible with a non-self entry;
//! * **inherited** anchoring ("the end of the data defined by the
//!   preceding track fragment") requires inheriting fragments to
//!   "all use the same data-reference (i.e., the data for these
//!   tracks must be in the same file)".
//!
//! Write side: `MovMuxer::encode_fragmented_to_vec` now honours
//! `set_external_media` (explicit-base trafs, one `trun` per
//! byte-contiguous location run). Read side: the resolved fragment
//! sample offsets flow through `set_data_reference_opener` exactly
//! like the non-fragmented r440 path.

#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom};

use oxideav_core::{Error, ReadSeek};
use oxideav_mov::{
    parse_moof, read_atom_header, DataReference, DataReferenceWrite, ExternalSampleLocation,
    FragmentationMode, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, TrafAddressing, TrafRecord,
};

/// Sidecar "external file": 6 bytes of unrelated preamble, then four
/// audio payloads — a gap before the third one. With the 4-video-frame
/// / 2-frames-per-fragment slicing below, fragment 1 carries audio
/// samples 0..3 (so the gap splits its traf into two truns) and
/// fragment 2 carries sample 3 alone.
fn sidecar() -> (Vec<u8>, Vec<ExternalSampleLocation>, Vec<Vec<u8>>) {
    let payloads: Vec<Vec<u8>> = vec![vec![0xB0; 5], vec![0xB1; 7], vec![0xB2; 4], vec![0xB3; 6]];
    let mut file = vec![0xEE; 6];
    let mut locations = Vec::new();
    for (i, p) in payloads.iter().enumerate() {
        if i == 2 {
            file.extend_from_slice(&[0xEE; 9]); // gap ⇒ second run
        }
        locations.push(ExternalSampleLocation {
            offset: file.len() as u64,
            size: p.len() as u32,
        });
        file.extend_from_slice(p);
    }
    (file, locations, payloads)
}

fn video_samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| MuxSample {
            data: vec![0x10 + i as u8; 12],
            duration: 100,
            keyframe: i % 2 == 0,
            composition_offset: 0,
        })
        .collect()
}

fn external_audio_samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|_| MuxSample {
            data: Vec::new(),
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

/// Fragmented movie: track 1 = local video (primary), track 2 =
/// external audio located in `media.bin`.
fn fragmented_mixed(locations: &[ExternalSampleLocation], frames_per_fragment: u32) -> Vec<u8> {
    let mut m =
        MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(frames_per_fragment));
    let v = m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 2,
            height: 2,
        },
        // Movie/primary timescale chosen so 100-tick video frames and
        // 1024-tick/8kHz audio frames slice together sensibly.
        600,
        video_samples(4),
        &[],
    );
    assert_eq!(v, 1);
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        external_audio_samples(locations.len()),
        &[],
    );
    m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
        .expect("external table");
    m.set_external_media(a, 1, locations)
        .expect("mark external");
    m.encode_fragmented_to_vec().expect("fragmented encode")
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).expect("open")
}

/// Walk every top-level `moof` of `bytes` and return its parsed trafs.
fn walk_moofs(bytes: &[u8]) -> Vec<Vec<TrafRecord>> {
    let mut r = Cursor::new(bytes.to_vec());
    let mut out = Vec::new();
    while let Some(hdr) = read_atom_header(&mut r).expect("atom header") {
        if &hdr.fourcc == b"moof" {
            let (_, trafs) = parse_moof(&mut r, &hdr).expect("parse moof");
            out.push(trafs);
        }
        let end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
        r.seek(SeekFrom::Start(end)).expect("seek");
    }
    out
}

// ───────────────────── write-side wire shape ─────────────────────

#[test]
fn external_trafs_use_explicit_base_local_trafs_stay_moof_relative() {
    let (_file, locations, _payloads) = sidecar();
    let bytes = fragmented_mixed(&locations, 2);
    let moofs = walk_moofs(&bytes);
    assert_eq!(moofs.len(), 2, "4 primary frames / 2 per fragment");
    for trafs in &moofs {
        assert_eq!(trafs.len(), 2);
        // Track 1 (local video): default-base-is-moof, one trun.
        assert_eq!(trafs[0].tfhd.track_id, 1);
        assert_eq!(trafs[0].tfhd.addressing(), TrafAddressing::MoofRelative);
        assert_eq!(trafs[0].tfhd.base_data_offset, None);
        assert_eq!(trafs[0].truns.len(), 1);
        // Track 2 (external audio): explicit base into media.bin.
        assert_eq!(trafs[1].tfhd.track_id, 2);
        assert_eq!(trafs[1].tfhd.addressing(), TrafAddressing::ExplicitBase);
        assert!(trafs[1].tfhd.base_data_offset.is_some());
    }
    // Fragment 1 covers audio samples 0..3 (time-window slicing along
    // the primary track's [0, 200)-tick boundary) with the sidecar
    // gap before sample 2: two truns, offsets relative to the
    // fragment's lowest offset (= location 0), and per-sample sizes
    // sourced from the locations, not the (empty) sample data.
    let f1 = &moofs[0][1];
    assert_eq!(f1.tfhd.base_data_offset, Some(locations[0].offset));
    assert_eq!(f1.truns.len(), 2);
    assert_eq!(f1.truns[0].data_offset, Some(0));
    let sizes: Vec<u32> = f1.truns[0]
        .samples
        .iter()
        .map(|s| s.sample_size.expect("size present"))
        .collect();
    assert_eq!(sizes, vec![locations[0].size, locations[1].size]);
    assert_eq!(
        f1.truns[1].data_offset,
        Some((locations[2].offset - locations[0].offset) as i32)
    );
    assert_eq!(f1.truns[1].samples.len(), 1);
    // Fragment 2 covers audio sample 3 alone: one trun anchored at
    // its own offset.
    let f2 = &moofs[1][1];
    assert_eq!(f2.tfhd.base_data_offset, Some(locations[3].offset));
    assert_eq!(f2.truns.len(), 1);
    assert_eq!(f2.truns[0].data_offset, Some(0));
    assert_eq!(f2.truns[0].samples.len(), 1);
}

// ───────────────────── read-side resolution ─────────────────────

#[test]
fn fragmented_external_round_trips_through_opener() {
    let (file, locations, payloads) = sidecar();
    let mut d = open(fragmented_mixed(&locations, 2));
    assert!(d.is_fragmented());
    assert!(d.track_has_external_data(1));
    assert!(!d.track_has_external_data(0));
    d.set_data_reference_opener(move |r| {
        assert_eq!(*r, DataReference::Url("media.bin".into()));
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut audio_dts = Vec::new();
    loop {
        match d.read_next() {
            Ok((0, s, data)) => {
                assert_eq!(s.keyframe, (s.index % 2) == 0);
                video.push(data);
            }
            Ok((1, s, data)) => {
                audio_dts.push(s.dts);
                assert_eq!(s.duration, 1024);
                audio.push(data);
            }
            Ok((n, ..)) => panic!("unexpected stream {n}"),
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(video.len(), 4);
    assert!(video
        .iter()
        .enumerate()
        .all(|(i, p)| *p == vec![0x10 + i as u8; 12]));
    assert_eq!(audio, payloads);
    // DTS keeps climbing across fragments (running cursor).
    assert_eq!(audio_dts, vec![0, 1024, 2048, 3072]);
}

#[test]
fn fragmented_external_without_opener_is_per_sample_recoverable() {
    let (_file, locations, _payloads) = sidecar();
    let mut d = open(fragmented_mixed(&locations, 2));
    let mut local = 0usize;
    let mut external_errors = 0usize;
    loop {
        match d.read_next() {
            Ok((stream, ..)) => {
                assert_eq!(stream, 0, "only the local track yields bytes");
                local += 1;
            }
            Err(Error::Eof) => break,
            Err(e) => {
                let msg = format!("{e}");
                assert!(msg.contains("external"), "unexpected error: {msg}");
                external_errors += 1;
            }
        }
    }
    assert_eq!(local, 4);
    assert_eq!(external_errors, locations.len());
}

/// A self-contained fragmented authoring and an external fragmented
/// authoring of the same media must present identical packet streams
/// once resolved (the r440 equivalence check, lifted to §8.8.7).
#[test]
fn fragmented_external_equivalence_with_self_contained() {
    let (file, locations, payloads) = sidecar();

    // (a) self-contained: audio bytes in each fragment's mdat.
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        payloads
            .iter()
            .map(|p| MuxSample {
                data: p.clone(),
                duration: 1024,
                keyframe: true,
                composition_offset: 0,
            })
            .collect(),
        &[],
    );
    let mut d_self = open(m.encode_fragmented_to_vec().expect("self-contained"));

    // (b) external: same media, bytes in the sidecar.
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        external_audio_samples(locations.len()),
        &[],
    );
    m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
        .expect("table");
    m.set_external_media(a, 1, &locations).expect("external");
    let mut d_ext = open(m.encode_fragmented_to_vec().expect("external"));
    d_ext.set_data_reference_opener(move |_r| {
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });

    loop {
        let a = d_self.read_next();
        let b = d_ext.read_next();
        match (a, b) {
            (Ok((sa, ea, da)), Ok((sb, eb, db))) => {
                assert_eq!(sa, sb);
                assert_eq!(ea.dts, eb.dts);
                assert_eq!(ea.duration, eb.duration);
                assert_eq!(ea.keyframe, eb.keyframe);
                assert_eq!(ea.composition_offset, eb.composition_offset);
                assert_eq!(da, db);
            }
            (Err(Error::Eof), Err(Error::Eof)) => break,
            (a, b) => panic!("streams diverged: {a:?} vs {b:?}"),
        }
    }
}

#[test]
fn fragmented_external_seek_lands_on_resolved_samples() {
    let (file, locations, payloads) = sidecar();
    let mut d = open(fragmented_mixed(&locations, 2));
    d.set_data_reference_opener(move |_r| {
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });
    // Drain, then seek the audio stream back to its third sample.
    while !matches!(d.read_next(), Err(Error::Eof)) {}
    use oxideav_core::Demuxer;
    d.seek_to(1, 2048).expect("fragmented seek");
    let (stream, s, data) = loop {
        match d.read_next() {
            Ok((1, s, data)) => break (1u32, s, data),
            Ok(_) => continue,
            Err(e) => panic!("unexpected error: {e}"),
        }
    };
    assert_eq!(stream, 1);
    assert_eq!(s.dts, 2048);
    assert_eq!(data, payloads[2]);
}

// ───────────────────── write-side validation ─────────────────────

#[test]
fn fragment_locations_spreading_past_i32_are_rejected() {
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        external_audio_samples(2),
        &[],
    );
    m.set_data_references(a, &[DataReferenceWrite::Url("huge.bin".into())])
        .expect("table");
    m.set_external_media(
        a,
        1,
        &[
            ExternalSampleLocation { offset: 0, size: 4 },
            ExternalSampleLocation {
                offset: (i32::MAX as u64) + 8,
                size: 4,
            },
        ],
    )
    .expect("external");
    let err = m.encode_fragmented_to_vec().expect_err("must reject");
    let msg = format!("{err}");
    assert!(msg.contains("signed 32-bit"), "unexpected error: {msg}");
}

// ───────────── read-side §8.8.7.1 mode enforcement ─────────────

/// Build a self-contained fragmented movie whose track keeps a
/// two-entry `dref` table `[SelfRef, Url]` with every sample entry
/// pointing at the SelfRef — then optionally byte-patch the sample
/// entries' `data_reference_index` from 1 to 2 (flipping the track
/// external without changing any box size) and/or zero the `tfhd`
/// flags of a chosen track (flipping default-base-is-moof off, i.e.
/// into inherited anchoring).
fn patched_fragmented(
    tracks: usize,
    patch_dri_track: Option<usize>,
    zero_tfhd_flags_track: Option<u32>,
) -> Vec<u8> {
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    for t in 0..tracks {
        let id = m.add_track(
            MuxTrackKind::Audio {
                format: if t == 0 { *b"twos" } else { *b"sowt" },
                channels: 1,
                bits_per_sample: 16,
                sample_rate: 8000,
            },
            8000,
            (0..2)
                .map(|i| MuxSample {
                    data: vec![0xC0 + (t as u8) * 0x10 + i as u8; 4],
                    duration: 1024,
                    keyframe: true,
                    composition_offset: 0,
                })
                .collect(),
            &[],
        );
        m.set_data_references(
            id,
            &[
                DataReferenceWrite::SelfRef,
                DataReferenceWrite::Url("m.bin".into()),
            ],
        )
        .expect("table");
    }
    let mut bytes = m.encode_fragmented_to_vec().expect("encode");
    if let Some(t) = patch_dri_track {
        // Sample-entry layout: [size:4][format:4][reserved:6][dri:2]
        // — patch the two dri bytes right after the 6 reserved bytes.
        let format: &[u8; 4] = if t == 0 { b"twos" } else { b"sowt" };
        let pos = bytes
            .windows(4)
            .position(|w| w == format)
            .expect("sample-entry format");
        assert_eq!(&bytes[pos + 10..pos + 12], &[0, 1], "dri was SelfRef");
        bytes[pos + 10] = 0;
        bytes[pos + 11] = 2;
    }
    if let Some(tid) = zero_tfhd_flags_track {
        // tfhd box layout: [size:4]["tfhd"][ver+flags:4][track_ID:4].
        let mut at = 0usize;
        while let Some(rel) = bytes[at..].windows(4).position(|w| w == b"tfhd") {
            let q = at + rel;
            let track_id = u32::from_be_bytes(bytes[q + 8..q + 12].try_into().unwrap());
            if track_id == tid {
                bytes[q + 5] = 0;
                bytes[q + 6] = 0;
                bytes[q + 7] = 0;
            }
            at = q + 4;
        }
    }
    bytes
}

#[test]
fn default_base_is_moof_with_external_dref_is_refused() {
    // Sanity: unpatched opens fine.
    assert!(MovDemuxer::open(
        Box::new(Cursor::new(patched_fragmented(1, None, None))) as Box<dyn ReadSeek>
    )
    .is_ok());
    // dri → the non-self Url entry while the tfhd still says
    // default-base-is-moof: §8.8.7.1 anchors moof-relative offsets at
    // a byte position of the fragment's own file.
    let err = match MovDemuxer::open(
        Box::new(Cursor::new(patched_fragmented(1, Some(0), None))) as Box<dyn ReadSeek>
    ) {
        Ok(_) => panic!("must refuse"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(msg.contains("default-base-is-moof"), "unexpected: {msg}");
}

#[test]
fn first_inheriting_traf_with_external_dref_is_refused() {
    // tfhd flags zeroed (inherited anchoring — the moof's own first
    // byte) + dri → non-self: refused per §8.8.7.1.
    let err = match MovDemuxer::open(
        Box::new(Cursor::new(patched_fragmented(1, Some(0), Some(1)))) as Box<dyn ReadSeek>,
    ) {
        Ok(_) => panic!("must refuse"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(msg.contains("inherits"), "unexpected: {msg}");
}

#[test]
fn inheriting_traf_with_mismatched_data_reference_is_refused() {
    // Two tracks: track 1 stays local default-base-is-moof; track 2
    // is flipped external *and* inherited — its inherited anchor
    // ("the end of the data defined by the preceding track fragment")
    // is a position in this file while its dref designates m.bin:
    // §8.8.7.1 requires inheriting fragments to use the same
    // data-reference as the fragment they inherit from.
    let err = match MovDemuxer::open(
        Box::new(Cursor::new(patched_fragmented(2, Some(1), Some(2)))) as Box<dyn ReadSeek>,
    ) {
        Ok(_) => panic!("must refuse"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(msg.contains("same data-reference"), "unexpected: {msg}");
}

#[test]
fn inheriting_traf_between_local_trafs_is_still_legal() {
    // Both tracks local; track 2's tfhd flags zeroed. Its inherited
    // anchor is the end of track 1's data — same file, same (self)
    // data reference — so the §8.8.7.1 check passes. (The baked
    // trun.data_offset values were authored moof-relative, so decoded
    // *payloads* are not asserted here; this pins that the mode
    // check itself accepts a legal all-local inheritance chain.)
    assert!(MovDemuxer::open(
        Box::new(Cursor::new(patched_fragmented(2, None, Some(2)))) as Box<dyn ReadSeek>
    )
    .is_ok());
}

// ───────── hand-built moof: inherited chaining in the external file ─────────

/// Compose an init segment (from the muxer, both tracks' sample
/// entries byte-patched to the non-self `dref` entry) with a
/// hand-built `moof` exercising the full §8.8.7.1 offset cascade
/// inside the *external* file:
///
/// * traf 1 (track 1): explicit `base_data_offset` B; trun 1 with
///   `data_offset = 0` (2 samples), trun 2 **without** a data offset
///   (chains "immediately after the data of the previous run",
///   §8.8.8.1);
/// * traf 2 (track 2): inherited anchoring, same `dref` value — legal
///   per §8.8.7.1 ("must all use the same data-reference"), anchored
///   at the end of traf 1's data *within the external file*; its trun
///   carries no data offset.
#[test]
fn inheriting_traf_with_matching_external_dref_chains_in_external_file() {
    let base: u64 = 32;
    let s: Vec<Vec<u8>> = vec![vec![0xD0; 3], vec![0xD1; 5], vec![0xD2; 4], vec![0xD3; 6]];
    let mut ext_file = vec![0xEE; base as usize];
    for p in &s {
        ext_file.extend_from_slice(p);
    }

    // Init segment: reuse the two-track fragmented build, patch BOTH
    // tracks' sample entries to dri=2, then truncate everything after
    // the init moov (drop the muxer's own moof+mdat stream).
    let mut bytes = patched_fragmented(2, Some(0), None);
    {
        // Patch track 2's dri as well (patched_fragmented only does
        // one track).
        let pos = bytes
            .windows(4)
            .position(|w| w == b"sowt")
            .expect("track 2 sample entry");
        bytes[pos + 10] = 0;
        bytes[pos + 11] = 2;
    }
    let mut r = Cursor::new(bytes.clone());
    let mut init_end = None;
    while let Some(hdr) = read_atom_header(&mut r).expect("hdr") {
        let end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
        if &hdr.fourcc == b"moov" {
            init_end = Some(end);
            break;
        }
        r.seek(SeekFrom::Start(end)).expect("seek");
    }
    let mut out = bytes[..init_end.expect("moov present") as usize].to_vec();

    // Hand-build the moof.
    fn push_atom(out: &mut Vec<u8>, fourcc: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&((body.len() as u32) + 8).to_be_bytes());
        out.extend_from_slice(fourcc);
        out.extend_from_slice(body);
    }
    fn trun(data_offset: Option<i32>, rows: &[(u32, u32)]) -> Vec<u8> {
        // duration-present | size-present | flags-present (+ optional
        // data-offset-present).
        let mut flags: u32 = 0x100 | 0x200 | 0x400;
        if data_offset.is_some() {
            flags |= 0x1;
        }
        let mut p = Vec::new();
        p.extend_from_slice(&flags.to_be_bytes());
        p.extend_from_slice(&(rows.len() as u32).to_be_bytes());
        if let Some(off) = data_offset {
            p.extend_from_slice(&off.to_be_bytes());
        }
        for (dur, size) in rows {
            p.extend_from_slice(&dur.to_be_bytes());
            p.extend_from_slice(&size.to_be_bytes());
            p.extend_from_slice(&0u32.to_be_bytes()); // sync
        }
        p
    }
    let mut moof = Vec::new();
    push_atom(&mut moof, b"mfhd", &{
        let mut p = vec![0u8; 4];
        p.extend_from_slice(&1u32.to_be_bytes());
        p
    });
    // traf 1: explicit base + two-run cascade.
    let mut traf1 = Vec::new();
    push_atom(&mut traf1, b"tfhd", &{
        let mut p = Vec::new();
        p.extend_from_slice(&0x000001u32.to_be_bytes()); // base-data-offset-present
        p.extend_from_slice(&1u32.to_be_bytes()); // track_ID
        p.extend_from_slice(&base.to_be_bytes());
        p
    });
    push_atom(
        &mut traf1,
        b"trun",
        &trun(
            Some(0),
            &[(1024, s[0].len() as u32), (1024, s[1].len() as u32)],
        ),
    );
    push_atom(
        &mut traf1,
        b"trun",
        &trun(None, &[(1024, s[2].len() as u32)]),
    );
    push_atom(&mut moof, b"traf", &traf1);
    // traf 2: inherited anchoring, same external dref.
    let mut traf2 = Vec::new();
    push_atom(&mut traf2, b"tfhd", &{
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // no flags: inherited
        p.extend_from_slice(&2u32.to_be_bytes()); // track_ID
        p
    });
    push_atom(
        &mut traf2,
        b"trun",
        &trun(None, &[(1024, s[3].len() as u32)]),
    );
    push_atom(&mut moof, b"traf", &traf2);
    push_atom(&mut out, b"moof", &moof);

    let mut d = open(out);
    assert!(d.track_has_external_data(0));
    assert!(d.track_has_external_data(1));
    d.set_data_reference_opener(move |r| {
        assert_eq!(*r, DataReference::Url("m.bin".into()));
        Ok(Box::new(Cursor::new(ext_file.clone())) as Box<dyn ReadSeek>)
    });
    let mut t1 = Vec::new();
    let mut t2 = Vec::new();
    loop {
        match d.read_next() {
            Ok((0, _s, data)) => t1.push(data),
            Ok((1, _s, data)) => t2.push(data),
            Ok((n, ..)) => panic!("unexpected stream {n}"),
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(t1, vec![s[0].clone(), s[1].clone(), s[2].clone()]);
    assert_eq!(t2, vec![s[3].clone()]);
}

// ───────── fragmented trun composition-time offsets (§8.8.8) ─────────

fn bframe_video_samples(offsets: &[i32]) -> Vec<MuxSample> {
    offsets
        .iter()
        .enumerate()
        .map(|(i, &cts)| MuxSample {
            data: vec![0x40 + i as u8; 10],
            duration: 100,
            keyframe: i == 0,
            composition_offset: cts,
        })
        .collect()
}

fn fragmented_video(offsets: &[i32]) -> Vec<u8> {
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 2,
            height: 2,
        },
        600,
        bframe_video_samples(offsets),
        &[],
    );
    m.encode_fragmented_to_vec().expect("fragmented encode")
}

#[test]
fn fragmented_trun_composition_offsets_round_trip_v0() {
    // All-non-negative reorder offsets (classic ctts v0 shape).
    let offsets = [100i32, 200, 0, 100];
    let bytes = fragmented_video(&offsets);
    let moofs = walk_moofs(&bytes);
    assert_eq!(moofs.len(), 2);
    let mut decoded = Vec::new();
    for trafs in &moofs {
        let trun = &trafs[0].truns[0];
        assert_eq!(trun.version, 0, "all-non-negative offsets stay v0");
        assert_ne!(
            trun.tr_flags & 0x000800,
            0,
            "sample-composition-time-offsets-present"
        );
        for s in &trun.samples {
            decoded.push(s.sample_cts_offset.expect("cts present"));
        }
    }
    assert_eq!(decoded, offsets);
    // The demuxer's fragment resolution carries the offsets onto the
    // sample entries: pts = dts + cts.
    let mut d = open(bytes);
    let mut seen = Vec::new();
    loop {
        match d.read_next() {
            Ok((0, s, _data)) => seen.push((s.dts, s.composition_offset)),
            Ok(_) => {}
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(
        seen,
        vec![(0, 100), (100, 200), (200, 0), (300, 100)],
        "dts climbs, cts preserved"
    );
}

#[test]
fn fragmented_trun_composition_offsets_promote_to_v1_when_negative() {
    // A negative offset (composition-to-decode shift already applied)
    // auto-promotes the trun to version 1, mirroring the ctts policy.
    let offsets = [0i32, 100, -50, 0];
    let bytes = fragmented_video(&offsets);
    let moofs = walk_moofs(&bytes);
    // Fragment 2 carries the negative offset; fragment 1 is v0.
    assert_eq!(moofs[0][0].truns[0].version, 0);
    assert_eq!(moofs[1][0].truns[0].version, 1);
    let mut decoded = Vec::new();
    for trafs in &moofs {
        for s in &trafs[0].truns[0].samples {
            decoded.push(s.sample_cts_offset.expect("cts present"));
        }
    }
    assert_eq!(decoded, offsets);
}

#[test]
fn fragmented_trun_omits_cts_field_when_all_zero() {
    // Wire stability: no reorder ⇒ the historical 12-byte-row trun,
    // no sample-composition-time-offsets-present bit.
    let bytes = fragmented_video(&[0, 0, 0, 0]);
    for trafs in &walk_moofs(&bytes) {
        let trun = &trafs[0].truns[0];
        assert_eq!(trun.tr_flags & 0x000800, 0);
        assert!(trun.samples.iter().all(|s| s.sample_cts_offset.is_none()));
    }
}

#[test]
fn fragmented_external_track_keeps_composition_offsets() {
    // The composition-offset axis rides the MuxSample even when the
    // byte axis lives in the external file (r440 contract), so an
    // external fragmented track reorders identically.
    let (file, locations, payloads) = sidecar();
    let cts = [0i32, 512, -256, 0];
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        cts.iter()
            .map(|&c| MuxSample {
                data: Vec::new(),
                duration: 1024,
                keyframe: true,
                composition_offset: c,
            })
            .collect(),
        &[],
    );
    m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
        .expect("table");
    m.set_external_media(a, 1, &locations).expect("external");
    let mut d = open(m.encode_fragmented_to_vec().expect("encode"));
    d.set_data_reference_opener(move |_r| {
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });
    let mut seen = Vec::new();
    loop {
        match d.read_next() {
            Ok((0, s, data)) => seen.push((s.composition_offset, data)),
            Ok(_) => unreachable!(),
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    let expect: Vec<(i32, Vec<u8>)> = cts.iter().copied().zip(payloads).collect();
    assert_eq!(seen, expect);
}
