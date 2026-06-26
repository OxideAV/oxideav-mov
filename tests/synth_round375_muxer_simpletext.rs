//! Round 375 — `MovMuxer` write-side **ISO BMFF timed-text track**
//! (`stxt` SimpleTextSampleEntry, ISO/IEC 14496-12 §12.5). The demuxer
//! already reads a `text`-handler track's `nmhd` null media header and a
//! `stxt` `stsd` sample entry onto `SampleDescription::simple_text`
//! (`parse_stxt`); round 375 lets the muxer *write* a complete timed-text
//! track via the new `MuxTrackKind::SimpleText`.
//!
//! The `stxt` / `nmhd` shape is structurally distinct from the QuickTime
//! `MuxTrackKind::Text` chapter/overlay track (which carries `gmhd` + a
//! `text` description); the demuxer disambiguates the two by the `stsd`
//! FourCC.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{BitRate, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, SimpleTextSampleEntry};

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

fn samples(payloads: &[&[u8]]) -> Vec<MuxSample> {
    payloads
        .iter()
        .map(|p| MuxSample {
            data: p.to_vec(),
            duration: 100,
            keyframe: true,
            composition_offset: 0,
        })
        .collect()
}

fn simpletext_track(m: &mut MovMuxer, desc: SimpleTextSampleEntry, payloads: &[&[u8]]) -> u32 {
    m.add_track(
        MuxTrackKind::SimpleText { description: desc },
        1000,
        samples(payloads),
        &[],
    )
}

#[test]
fn stxt_simple_text_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = SimpleTextSampleEntry {
        content_encoding: String::new(),
        mime_format: "text/plain".into(),
        text_config: None,
        bitrate: None,
    };
    let _ = simpletext_track(&mut m, entry.clone(), &[b"hello", b"world"]);
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"nmhd"));
    assert!(bytes.windows(4).any(|w| w == b"stxt"));

    let d = open(bytes);
    let t = &d.tracks[0];
    // `text` handler but the stxt FourCC selects the timed-text path.
    assert!(t.hdlr.is_text());
    let sd = &t.sample_descriptions[0];
    assert_eq!(sd.format, *b"stxt");
    assert_eq!(sd.simple_text.as_ref().expect("simple_text"), &entry);
    // The stxt path must NOT also populate the QuickTime text description.
    assert!(sd.text.is_none());
    let st = &t.sample_table;
    assert_eq!(st.sample_count(), 2);
}

#[test]
fn stxt_with_encoding_config_and_bitrate_roundtrips() {
    let mut m = MovMuxer::new();
    let entry = SimpleTextSampleEntry {
        content_encoding: "application/gzip".into(),
        mime_format: "text/html".into(),
        text_config: Some("<style/>".into()),
        bitrate: Some(BitRate {
            buffer_size_db: 1024,
            max_bitrate: 16_000,
            avg_bitrate: 8_000,
        }),
    };
    let _ = simpletext_track(&mut m, entry.clone(), &[b"<p>x</p>"]);
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"txtC"));
    assert!(bytes.windows(4).any(|w| w == b"btrt"));
    let d = open(bytes);
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.format, *b"stxt");
    assert_eq!(sd.simple_text.as_ref().expect("simple_text"), &entry);
}

#[test]
fn stxt_roundtrips_through_fragmented_path() {
    use oxideav_mov::FragmentationMode;
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    let entry = SimpleTextSampleEntry {
        content_encoding: String::new(),
        mime_format: "text/plain".into(),
        text_config: None,
        bitrate: None,
    };
    let _ = simpletext_track(&mut m, entry.clone(), &[b"s1", b"s2", b"s3"]);
    let bytes = m.encode_fragmented_to_vec().expect("encode fragmented");
    assert!(bytes.windows(4).any(|w| w == b"nmhd"));
    assert!(bytes.windows(4).any(|w| w == b"stxt"));
    let d = open(bytes);
    assert_eq!(d.tracks[0].sample_descriptions[0].format, *b"stxt");
    assert_eq!(
        d.tracks[0].sample_descriptions[0]
            .simple_text
            .as_ref()
            .expect("simple_text"),
        &entry
    );
}
