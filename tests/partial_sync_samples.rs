//! The QuickTime partial sync sample atom (`stps`, QTFF 2012-08-14
//! pp. 108 – 109): a table of sample numbers that "are not full sync
//! samples" and so are not listed in the sync sample atom. Read onto
//! the sample table, written by the muxer.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::{MovDemuxer, MovMuxer, MuxSample, MuxTrackKind, parse_stps};

fn movie() -> (MovMuxer, u32) {
    let mut m = MovMuxer::new();
    // Sync samples 1 and 7; the rest are not.
    let samples: Vec<MuxSample> = (0..12)
        .map(|i| {
            MuxSample::default()
                .with_data(vec![i as u8; 9])
                .with_duration(100)
                .with_keyframe(i % 6 == 0)
        })
        .collect();
    let id = m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 4,
            height: 4,
        },
        2500,
        samples,
        &[],
    );
    (m, id)
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).expect("open")
}

#[test]
fn partial_sync_samples_round_trip() {
    let (mut m, id) = movie();
    m.set_partial_sync_samples(id, &[10, 4]).unwrap();
    let file = m.encode_to_vec().unwrap();
    assert!(file.windows(4).any(|w| w == b"stps"));

    let mut d = open(file);
    assert_eq!(d.partial_sync_samples(0), [4, 10]);
    assert_eq!(d.partial_sync_samples(9), [] as [u32; 0]);
    let table = &d.tracks[0].sample_table;
    assert_eq!(table.stss, [1, 7]);
    assert!(table.is_partial_sync(3));
    assert!(table.is_partial_sync(9));
    assert!(!table.is_partial_sync(0));
    assert!(!table.is_partial_sync(4));
    assert!(!table.is_partial_sync(u32::MAX));

    // Not full sync samples: no keyframe flag, and a seek passes
    // them by for the sync sample before.
    let mut flags = Vec::new();
    while let Ok(p) = d.next_packet() {
        flags.push(p.flags.keyframe);
    }
    assert_eq!(flags.iter().filter(|&&k| k).count(), 2);
    assert!(!flags[3] && !flags[9]);
    assert_eq!(d.seek_to(0, 450).unwrap(), 0);
    assert_eq!(d.seek_to(0, 950).unwrap(), 600);

    // No atom unless asked for; an empty list removes it.
    let (m, _) = movie();
    assert!(!m.encode_to_vec().unwrap().windows(4).any(|w| w == b"stps"));
    let (mut m, id) = movie();
    m.set_partial_sync_samples(id, &[2]).unwrap();
    m.set_partial_sync_samples(id, &[]).unwrap();
    assert!(
        open(m.encode_to_vec().unwrap())
            .partial_sync_samples(0)
            .is_empty()
    );
}

#[test]
fn the_writer_refuses_what_the_atom_cannot_say() {
    let (mut m, id) = movie();
    // A sync sample cannot also be a partial sync sample.
    assert!(m.set_partial_sync_samples(id, &[1]).is_err());
    assert!(m.set_partial_sync_samples(id, &[7, 8]).is_err());
    // Sample numbers are 1-based and within the track; no duplicates.
    assert!(m.set_partial_sync_samples(id, &[0]).is_err());
    assert!(m.set_partial_sync_samples(id, &[13]).is_err());
    assert!(m.set_partial_sync_samples(id, &[5, 5]).is_err());
    assert!(m.set_partial_sync_samples(99, &[2]).is_err());
    assert!(m.set_partial_sync_samples(id, &[2, 12]).is_ok());
}

#[test]
fn the_reader_refuses_malformed_tables() {
    let table = |numbers: &[u32], declared: u32| -> Vec<u8> {
        let mut b = vec![0u8; 4];
        b.extend_from_slice(&declared.to_be_bytes());
        for n in numbers {
            b.extend_from_slice(&n.to_be_bytes());
        }
        b
    };
    assert_eq!(parse_stps(&table(&[2, 5, 9], 3)).unwrap(), [2, 5, 9]);
    assert_eq!(parse_stps(&table(&[], 0)).unwrap(), [] as [u32; 0]);
    // Trailing bytes past the declared count are ignored.
    assert_eq!(parse_stps(&table(&[2, 5, 9], 2)).unwrap(), [2, 5]);
    // Declared entries the body does not hold; a short header.
    assert!(parse_stps(&table(&[2], 2)).is_err());
    assert!(parse_stps(&table(&[], u32::MAX)).is_err());
    assert!(parse_stps(&[0; 7]).is_err());
    // Not strictly increasing.
    assert!(parse_stps(&table(&[5, 5], 2)).is_err());
    assert!(parse_stps(&table(&[9, 2], 2)).is_err());
}
