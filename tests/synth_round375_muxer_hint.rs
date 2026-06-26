//! Round 375 — `MovMuxer` write-side **ISO BMFF hint track** (ISO/IEC
//! 14496-12 §12.4), a streaming-server packetization track. The demuxer
//! already reads a `hint`-handler track's `hmhd` Hint Media Header
//! (`parse_hmhd` onto `Track::hmhd`) and its protocol-named `stsd` entry;
//! round 375 lets the muxer *write* a complete hint track via the new
//! `MuxTrackKind::Hint`.
//!
//! Each test builds a movie through [`MovMuxer`], re-opens it through
//! [`MovDemuxer`], and asserts the `hint` handler, the `hmhd` header,
//! the protocol-named sample-entry FourCC + opaque body, and a
//! `tref/hint` to the packetized media track round trip.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::ReadSeek;
use oxideav_mov::{Hmhd, MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, TrackReference};

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

fn rtp_hmhd() -> Hmhd {
    Hmhd {
        max_pdu_size: 1450,
        avg_pdu_size: 1200,
        max_bitrate: 4_000_000,
        avg_bitrate: 2_500_000,
    }
}

#[test]
fn hint_track_handler_and_hmhd_roundtrip() {
    let mut m = MovMuxer::new();
    let _ = m.add_track(
        MuxTrackKind::Hint {
            protocol: *b"rtp ",
            description: Vec::new(),
            hmhd: rtp_hmhd(),
        },
        90_000,
        samples(&[b"packet-a", b"packet-b"]),
        &[],
    );
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"hmhd"));
    assert!(bytes.windows(4).any(|w| w == b"rtp "));

    let d = open(bytes);
    let t = &d.tracks[0];
    assert!(t.hdlr.is_hint());
    assert_eq!(t.hmhd.expect("hmhd parsed"), rtp_hmhd());
    let sd = &t.sample_descriptions[0];
    assert_eq!(sd.format, *b"rtp ");
    assert_eq!(t.sample_table.sample_count(), 2);
}

#[test]
fn hint_track_with_opaque_description_roundtrips() {
    let mut m = MovMuxer::new();
    // Opaque protocol-specific declarative body (e.g. a tims/timescale
    // box). Treated as raw bytes by the muxer; preserved on read.
    let mut tims = Vec::new();
    let body = 90_000u32.to_be_bytes();
    let size = (8 + body.len()) as u32;
    tims.extend_from_slice(&size.to_be_bytes());
    tims.extend_from_slice(b"tims");
    tims.extend_from_slice(&body);
    let _ = m.add_track(
        MuxTrackKind::Hint {
            protocol: *b"rtp ",
            description: tims.clone(),
            hmhd: rtp_hmhd(),
        },
        90_000,
        samples(&[b"p"]),
        &[],
    );
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"tims"));
    let d = open(bytes);
    let sd = &d.tracks[0].sample_descriptions[0];
    assert_eq!(sd.format, *b"rtp ");
    // The opaque declarative body is preserved on the read-side extra.
    assert_eq!(sd.extra, tims);
}

#[test]
fn hint_track_references_media_track_via_tref_hint() {
    let mut m = MovMuxer::new();
    let vid = m.add_track(
        MuxTrackKind::Video {
            format: *b"avc1",
            width: 640,
            height: 480,
        },
        600,
        samples(&[b"frame"]),
        &[],
    );
    let hint = m.add_track(
        MuxTrackKind::Hint {
            protocol: *b"rtp ",
            description: Vec::new(),
            hmhd: rtp_hmhd(),
        },
        90_000,
        samples(&[b"pkt"]),
        &[],
    );
    // The hint track points at the media track it packetizes (§12.4.1).
    m.set_track_references(hint, &[TrackReference::to(*b"hint", vid)])
        .expect("set tref");
    let bytes = m.encode_to_vec().expect("encode");
    let d = open(bytes);
    assert_eq!(d.tracks.len(), 2);
    assert!(d.tracks[1].hdlr.is_hint());
    let refs = &d.tracks[1].references;
    assert!(refs
        .iter()
        .any(|r| r.fourcc == *b"hint" && r.track_ids.contains(&vid)));
}

#[test]
fn hint_roundtrips_through_fragmented_path() {
    use oxideav_mov::FragmentationMode;
    let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(2));
    let _ = m.add_track(
        MuxTrackKind::Hint {
            protocol: *b"rtp ",
            description: Vec::new(),
            hmhd: rtp_hmhd(),
        },
        90_000,
        samples(&[b"a", b"b", b"c"]),
        &[],
    );
    let bytes = m.encode_fragmented_to_vec().expect("encode fragmented");
    assert!(bytes.windows(4).any(|w| w == b"hmhd"));
    assert!(bytes.windows(4).any(|w| w == b"rtp "));
    let d = open(bytes);
    assert!(d.tracks[0].hdlr.is_hint());
    assert_eq!(d.tracks[0].hmhd.expect("hmhd"), rtp_hmhd());
    assert_eq!(d.tracks[0].sample_descriptions[0].format, *b"rtp ");
}
