//! A movie that is still being written or downloaded ends inside a
//! top-level atom: the `mdat` of a moov-first file whose media has
//! not all arrived, a fragment in progress. Once the movie atom has
//! been read such a file opens (flagged `is_truncated`), every sample
//! whose bytes are present reads back, and the first missing one
//! fails cleanly. Cut anywhere, nothing panics.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{FragmentationMode, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind};

fn movie() -> MovMuxer {
    let mut m = MovMuxer::new();
    let video: Vec<MuxSample> = (0..24)
        .map(|i| {
            MuxSample::default()
                .with_data(vec![i as u8 + 1; 50 + i])
                .with_duration(100)
                .with_keyframe(i % 6 == 0)
        })
        .collect();
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 4,
        },
        2400,
        video,
        &[],
    );
    let audio: Vec<MuxSample> = (0..20)
        .map(|i| {
            MuxSample::default()
                .with_data(vec![0x80 | i as u8; 30])
                .with_duration(1024)
                .with_keyframe(true)
        })
        .collect();
    m.add_track(
        MuxTrackKind::Audio {
            format: *b".mp3",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        audio,
        &[],
    );
    m
}

fn try_open(bytes: &[u8]) -> Option<MovDemuxer> {
    MovDemuxer::open(Box::new(Cursor::new(bytes.to_vec())) as Box<dyn ReadSeek>).ok()
}

/// Packets read until the first error, as (stream, bytes).
fn read_all(d: &mut MovDemuxer) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push((p.stream_index, p.data));
    }
    out
}

fn top_level_end(file: &[u8], kind: &[u8; 4]) -> usize {
    let mut at = 0usize;
    loop {
        let size = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        if &file[at + 4..at + 8] == kind {
            return at + size;
        }
        at += size;
    }
}

#[test]
fn moov_first_file_plays_as_far_as_its_media_has_arrived() {
    let file = movie().with_faststart().encode_to_vec().unwrap();
    let whole = read_all(&mut try_open(&file).unwrap());
    assert_eq!(whole.len(), 44);
    let moov_end = top_level_end(&file, b"moov");

    let mut seen_partial = false;
    for cut in 0..file.len() {
        let Some(mut d) = try_open(&file[..cut]) else {
            assert!(cut < moov_end + 8, "cut {cut}: the movie atom is complete");
            continue;
        };
        assert!(cut >= moov_end, "cut {cut}: opened without a movie atom");
        // Cut exactly between `moov` and `mdat`, the file is whole as
        // far as it goes; inside the `mdat`, it is truncated.
        assert_eq!(d.is_truncated(), cut > moov_end, "cut {cut}");
        assert!(d.is_faststart());
        assert_eq!(d.streams().len(), 2);
        let got = read_all(&mut d);
        // A prefix of the whole file's packets, each one complete.
        assert!(got.len() <= whole.len());
        assert_eq!(got[..], whole[..got.len()], "cut {cut}");
        seen_partial |= !got.is_empty() && got.len() < whole.len();
    }
    assert!(seen_partial);
}

#[test]
fn fragmented_file_plays_its_complete_fragments() {
    let file = movie()
        .with_fragmentation(FragmentationMode::ByFrameCount(6))
        .with_fragment_index()
        .encode_fragmented_to_vec()
        .unwrap();
    let whole = read_all(&mut try_open(&file).unwrap());
    assert_eq!(whole.len(), 44);
    let moov_end = top_level_end(&file, b"moov");

    let mut counts = std::collections::BTreeSet::new();
    for cut in 0..file.len() {
        let Some(mut d) = try_open(&file[..cut]) else {
            assert!(cut < moov_end, "cut {cut}: the movie atom is complete");
            continue;
        };
        let got = read_all(&mut d);
        assert!(got.len() <= whole.len());
        assert_eq!(got[..], whole[..got.len()], "cut {cut}");
        counts.insert(got.len());
        // A seek never panics on the shortened movie.
        let _ = d.seek_to(0, 1200);
        let _ = d.next_packet();
    }
    // Fragment by fragment: several distinct packet counts between
    // none and all.
    assert!(counts.len() > 4, "{counts:?}");
    assert!(counts.contains(&0));
}

#[test]
fn moov_last_file_needs_its_movie_atom() {
    // mdat first: until the trailing `moov` is complete there is
    // nothing to read the media with.
    let file = movie().encode_to_vec().unwrap();
    let moov_end = top_level_end(&file, b"moov");
    assert_eq!(moov_end, file.len());
    for cut in (0..file.len()).step_by(7) {
        assert!(try_open(&file[..cut]).is_none(), "cut {cut}");
    }
    assert!(!try_open(&file).unwrap().is_truncated());
}
