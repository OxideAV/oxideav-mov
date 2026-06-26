//! Round 375 — `MovMuxer` write-side **Compact Sample Size Box** (`stz2`,
//! ISO/IEC 14496-12 §8.7.3.3). The demuxer already reads `stz2` with a
//! 4 / 8 / 16-bit `field_size` (`parse_stz2`, surfaced via
//! `MovDemuxer::sample_size_source`) but the muxer only ever wrote the
//! wider `stsz`. `MovMuxer::set_compact_sample_size(track_id, true)` opts
//! a track into the narrow form when it genuinely saves space, falling
//! back to `stsz` transparently otherwise.
//!
//! Each test builds a movie, re-opens it, and asserts the per-sample
//! sizes round-trip while `sample_size_source` reports the expected box.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, SampleSizeSource};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn video_track(m: &mut MovMuxer, sizes: &[usize]) -> u32 {
    let samples: Vec<MuxSample> = sizes
        .iter()
        .enumerate()
        .map(|(i, &n)| MuxSample {
            data: vec![(i as u8).wrapping_add(1); n],
            duration: 100,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 16,
            height: 16,
        },
        600,
        samples,
        &[],
    )
}

fn assert_sizes(d: &MovDemuxer, expected: &[usize]) {
    let st = &d.tracks[0].sample_table;
    assert_eq!(st.sample_count() as usize, expected.len());
    for (i, &n) in expected.iter().enumerate() {
        assert_eq!(st.sample_size_at(i as u32), Some(n as u32));
    }
}

#[test]
fn stz2_4bit_for_small_varied_sizes() {
    // All sizes 1..=15 ⇒ 4-bit field fits.
    let sizes = [3usize, 7, 1, 15, 9, 2];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, &sizes);
    m.set_compact_sample_size(id, true).expect("opt in");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"stz2"));
    assert!(!bytes.windows(4).any(|w| w == b"stsz"));

    let d = open(bytes);
    assert_sizes(&d, &sizes);
    assert_eq!(
        d.sample_size_source(0),
        Some(SampleSizeSource::Stz2 { field_size: 4 })
    );
}

#[test]
fn stz2_8bit_for_medium_varied_sizes() {
    // One size > 15 but all <= 255 ⇒ 8-bit field.
    let sizes = [10usize, 200, 42, 7];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, &sizes);
    m.set_compact_sample_size(id, true).expect("opt in");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"stz2"));

    let d = open(bytes);
    assert_sizes(&d, &sizes);
    assert_eq!(
        d.sample_size_source(0),
        Some(SampleSizeSource::Stz2 { field_size: 8 })
    );
}

#[test]
fn stz2_4bit_odd_count_zero_pads_last_nibble() {
    // Odd count exercises the §8.7.3.3.2 low-nibble zero-pad.
    let sizes = [5usize, 6, 7];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, &sizes);
    m.set_compact_sample_size(id, true).expect("opt in");
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    assert_sizes(&d, &sizes);
    assert_eq!(
        d.sample_size_source(0),
        Some(SampleSizeSource::Stz2 { field_size: 4 })
    );
}

#[test]
fn falls_back_to_stsz_when_sizes_too_large() {
    // A size > 255 cannot use the narrow stz2 form ⇒ stsz.
    let sizes = [10usize, 300, 20];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, &sizes);
    m.set_compact_sample_size(id, true).expect("opt in");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"stsz"));
    assert!(!bytes.windows(4).any(|w| w == b"stz2"));
    let d = open(bytes);
    assert_sizes(&d, &sizes);
    assert_eq!(d.sample_size_source(0), Some(SampleSizeSource::Stsz));
}

#[test]
fn falls_back_to_stsz_when_sizes_uniform() {
    // Uniform sizes ⇒ table-less stsz is already smaller than any stz2.
    let sizes = [8usize, 8, 8, 8];
    let mut m = MovMuxer::new();
    let id = video_track(&mut m, &sizes);
    m.set_compact_sample_size(id, true).expect("opt in");
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"stsz"));
    assert!(!bytes.windows(4).any(|w| w == b"stz2"));
    let d = open(bytes);
    assert_sizes(&d, &sizes);
    assert_eq!(d.sample_size_source(0), Some(SampleSizeSource::Stsz));
}

#[test]
fn default_is_stsz_without_opt_in() {
    let sizes = [3usize, 7, 1, 15];
    let mut m = MovMuxer::new();
    let _ = video_track(&mut m, &sizes);
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"stsz"));
    assert!(!bytes.windows(4).any(|w| w == b"stz2"));
    let d = open(bytes);
    assert_eq!(d.sample_size_source(0), Some(SampleSizeSource::Stsz));
}

#[test]
fn set_compact_sample_size_unknown_track_errors() {
    let mut m = MovMuxer::new();
    assert!(m.set_compact_sample_size(7, true).is_err());
}
