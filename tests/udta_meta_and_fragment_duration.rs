//! Two reader facts pinned against a black-box reader's report of the
//! same files:
//!
//! * a QuickTime Metadata atom (`hdlr` `mdta` + `keys` + `ilst`) filed
//!   as a `meta` entry of the user data list — with the ISO FullBox
//!   word in front of its children — is read like one directly in
//!   `moov` / `trak`;
//! * a fragmented movie's stream and movie durations run to the end of
//!   the last fragment sample, not the (empty) `moov` sample tables.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, Error, ReadSeek};
use oxideav_mov::{
    FragmentationMode, MovDemuxer, MovMetaItem, MovMetadata, MovMuxer, MuxSample, MuxTrackKind,
};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).expect("open")
}

fn video_samples(n: usize) -> Vec<MuxSample> {
    (0..n)
        .map(|i| {
            MuxSample::default()
                .with_data(vec![i as u8; 10])
                .with_duration(100)
                .with_keyframe(i % 5 == 0)
        })
        .collect()
}

fn add_video(m: &mut MovMuxer, n: usize) -> u32 {
    m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 4,
        },
        2500,
        video_samples(n),
        &[],
    )
}

/// The body of the `meta` atom the muxer writes for `items`.
fn meta_body(items: &[MovMetaItem]) -> Vec<u8> {
    let mut m = MovMuxer::new();
    add_video(&mut m, 1);
    m.set_apple_metadata(items);
    let file = m.encode_to_vec().unwrap();
    let moov = file.windows(4).position(|w| w == b"moov").unwrap();
    let at = moov + file[moov..].windows(4).position(|w| w == b"meta").unwrap();
    let size = u32::from_be_bytes(file[at - 4..at].try_into().unwrap()) as usize;
    file[at + 4..at - 4 + size].to_vec()
}

#[test]
fn quicktime_metadata_filed_in_the_user_data_list_is_read() {
    let items = [
        MovMetaItem::utf8("com.example.key", "hello"),
        MovMetaItem::utf8("title", "T"),
    ];
    // FullBox word, then the children.
    let mut entry = vec![0u8; 4];
    entry.extend_from_slice(&meta_body(&items));

    let mut m = MovMuxer::new();
    let id = add_video(&mut m, 3);
    m.set_metadata(&[
        MovMetadata::intl_text(*b"\xA9nam", 0, "name"),
        MovMetadata::raw(*b"meta", entry.clone()),
    ]);
    m.set_track_metadata(id, &[MovMetadata::raw(*b"meta", entry.clone())])
        .unwrap();
    let d = open(m.encode_to_vec().unwrap());
    for meta in [&d.meta, &d.tracks[0].meta] {
        let pairs: Vec<(&str, &[u8])> = meta
            .iter()
            .map(|kv| (kv.key.as_str(), kv.value.as_slice()))
            .collect();
        assert_eq!(
            pairs,
            [("com.example.key", &b"hello"[..]), ("title", &b"T"[..])]
        );
    }
    assert!(d.qt_metadata.is_some());
    // The user data list itself is unchanged.
    assert_eq!(d.user_data.len(), 2);

    // A `meta` directly in `moov` wins over the user-data one.
    let mut m = MovMuxer::new();
    add_video(&mut m, 3);
    m.set_metadata(&[MovMetadata::raw(*b"meta", entry)]);
    m.set_apple_metadata(&[MovMetaItem::utf8("title", "direct")]);
    let d = open(m.encode_to_vec().unwrap());
    assert_eq!(d.meta.len(), 1);
    assert_eq!(d.meta[0].value, b"direct");

    // Not QuickTime-shaped, or damaged: ignored, never an error.
    for junk in [
        vec![0u8; 4],
        vec![0xFF; 40],
        b"\0\0\0\0\0\0\0\x10hdlr".to_vec(),
    ] {
        let mut m = MovMuxer::new();
        add_video(&mut m, 1);
        m.set_metadata(&[MovMetadata::raw(*b"meta", junk)]);
        let d = open(m.encode_to_vec().unwrap());
        assert!(d.meta.is_empty());
        assert!(d.qt_metadata.is_none());
    }
}

fn drain_durations(d: &mut MovDemuxer) -> Vec<i64> {
    let mut sums = vec![0i64; d.streams().len()];
    loop {
        match d.next_packet() {
            Ok(p) => sums[p.stream_index as usize] += p.duration.unwrap_or(0),
            Err(Error::Eof) => return sums,
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn fragmented_durations_run_to_the_last_fragment_sample() {
    // Our own fragmented output: 20 samples of 100 ticks at 2500.
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(5));
    add_video(&mut m, 20);
    let mut d = open(m.encode_fragmented_to_vec().unwrap());
    assert!(d.is_fragmented());
    assert_eq!(d.streams()[0].duration, Some(2000));
    assert_eq!(d.duration_micros(), Some(800_000));
    assert_eq!(drain_durations(&mut d), [2000]);

    // A black-box writer's fragmented movie (empty `moov`, no `mehd`):
    // `ffprobe` reports duration_ts 20480 at 1/10240 for its stream.
    let path = format!(
        "{}/tests/fixtures/h264_2s_frag.mov",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut d = open(std::fs::read(path).unwrap());
    assert!(d.is_fragmented());
    let stated: Vec<i64> = d.streams().iter().map(|s| s.duration.unwrap()).collect();
    assert_eq!(stated, [20480]);
    let micros = d.duration_micros().unwrap();
    assert!((1_900_000..=2_100_000).contains(&micros), "{micros}");
    assert_eq!(drain_durations(&mut d), stated);
}
