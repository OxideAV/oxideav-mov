//! Round 372 — `MovMuxer` write-side custom Data Reference Box (`dref`,
//! QTFF p. 65 / ISO/IEC 14496-12 §8.7.2). The demuxer already reads
//! `dref` onto `Track::data_references`; round 372 lets the muxer write
//! an external `url ` / `urn ` table via
//! [`MovMuxer::set_data_references`], with the sample entries'
//! `data_reference_index` pointed at the lone self-reference.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{
    DataReference, DataReferenceWrite, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind,
};

fn one_sample() -> Vec<MuxSample> {
    vec![MuxSample {
        data: vec![0x44u8; 8],
        duration: 1024,
        keyframe: true,
        composition_offset: 0,
    }]
}

fn add_video(m: &mut MovMuxer) -> u32 {
    m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 320,
            height: 240,
        },
        30000,
        one_sample(),
        &[],
    )
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open muxed file")
}

/// Read the 1-based `data_reference_index` from a track's first `stsd`
/// sample entry. The entry layout is `[size:4][format:4][reserved:6]
/// [data_reference_index:2]…`; we locate it via the `avc1` FourCC.
fn first_stsd_dri(bytes: &[u8]) -> u16 {
    let pos = bytes
        .windows(4)
        .position(|w| w == b"avc1")
        .expect("avc1 entry");
    // data_reference_index is 8 bytes after the format FourCC.
    u16::from_be_bytes([bytes[pos + 10], bytes[pos + 11]])
}

#[test]
fn default_dref_is_single_self_reference() {
    let mut m = MovMuxer::new();
    let _ = add_video(&mut m);
    let d = open(m.encode_to_vec().expect("encode"));
    let refs = d.tracks[0].data_references();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0], DataReference::SelfRef);
}

#[test]
fn external_url_alongside_self_ref_roundtrips() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    m.set_data_references(
        v,
        &[
            DataReferenceWrite::SelfRef,
            DataReferenceWrite::Url("http://example.com/media.mov".into()),
        ],
    )
    .expect("set dref");
    let bytes = m.encode_to_vec().expect("encode");

    // Self-ref is the first entry ⇒ data_reference_index == 1.
    assert_eq!(first_stsd_dri(&bytes), 1);

    let d = open(bytes);
    let refs = d.tracks[0].data_references();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0], DataReference::SelfRef);
    assert_eq!(
        refs[1],
        DataReference::Url("http://example.com/media.mov".into())
    );
}

#[test]
fn self_ref_index_tracks_its_position() {
    // Self-ref placed second ⇒ sample entries must point at index 2.
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    m.set_data_references(
        v,
        &[
            DataReferenceWrite::Url("file:///external.mov".into()),
            DataReferenceWrite::SelfRef,
        ],
    )
    .expect("set dref");
    let bytes = m.encode_to_vec().expect("encode");
    assert_eq!(first_stsd_dri(&bytes), 2);

    let d = open(bytes);
    let refs = d.tracks[0].data_references();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0], DataReference::Url("file:///external.mov".into()));
    assert_eq!(refs[1], DataReference::SelfRef);
}

#[test]
fn urn_with_location_roundtrips() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    m.set_data_references(
        v,
        &[
            DataReferenceWrite::SelfRef,
            DataReferenceWrite::Urn {
                name: "urn:example:media".into(),
                location: "http://cdn.example.com/".into(),
            },
        ],
    )
    .expect("set dref");
    let d = open(m.encode_to_vec().expect("encode"));
    let refs = d.tracks[0].data_references();
    assert_eq!(
        refs[1],
        DataReference::Urn {
            name: "urn:example:media".into(),
            location: "http://cdn.example.com/".into(),
        }
    );
}

#[test]
fn urn_without_location_roundtrips() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    m.set_data_references(
        v,
        &[
            DataReferenceWrite::SelfRef,
            DataReferenceWrite::Urn {
                name: "urn:example:bare".into(),
                location: String::new(),
            },
        ],
    )
    .expect("set dref");
    let d = open(m.encode_to_vec().expect("encode"));
    let refs = d.tracks[0].data_references();
    assert_eq!(
        refs[1],
        DataReference::Urn {
            name: "urn:example:bare".into(),
            location: String::new(),
        }
    );
}

#[test]
fn samples_still_read_back_with_external_dref() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    m.set_data_references(
        v,
        &[
            DataReferenceWrite::SelfRef,
            DataReferenceWrite::Url("http://example.com/x.mov".into()),
        ],
    )
    .expect("set dref");
    let mut d = open(m.encode_to_vec().expect("encode"));
    let pkt = d.next_packet().expect("packet");
    assert_eq!(pkt.data, vec![0x44u8; 8]);
}

#[test]
fn zero_self_refs_rejected() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    let err = m.set_data_references(v, &[DataReferenceWrite::Url("http://example.com/".into())]);
    assert!(err.is_err(), "a table with no self-ref must error");
}

#[test]
fn multiple_self_refs_rejected() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    let err = m.set_data_references(
        v,
        &[DataReferenceWrite::SelfRef, DataReferenceWrite::SelfRef],
    );
    assert!(err.is_err(), "two self-refs must error");
}

#[test]
fn empty_table_rejected() {
    let mut m = MovMuxer::new();
    let v = add_video(&mut m);
    let err = m.set_data_references(v, &[]);
    assert!(err.is_err(), "empty table must error (no self-ref)");
}

#[test]
fn unknown_track_id_rejected() {
    let mut m = MovMuxer::new();
    let _ = add_video(&mut m);
    let err = m.set_data_references(9, &[DataReferenceWrite::SelfRef]);
    assert!(err.is_err(), "unknown track id must error");
}
