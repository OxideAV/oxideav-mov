//! Round 440 — **external-data movies**, end to end.
//!
//! QTFF appendix "Defining Media Data Layouts": *"A QuickTime file can
//! reference media data stored in a number of files, including the
//! file itself"*, addressed *"by file offset, rather than by a data
//! structuring mechanism of a particular file format"*. The write side
//! is `MovMuxer::set_external_media` (samples live in a sidecar file;
//! the movie carries only the sample tables + a non-self `dref`
//! entry); the read side is
//! `MovDemuxer::set_data_reference_opener` (opt-in resolution — the
//! default `open` path can never touch the filesystem or network) and
//! the built-in sandboxed `dref_file_opener` local-file policy.

#![cfg(feature = "registry")]

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use oxideav_core::{Demuxer, Error, ReadSeek};
use oxideav_mov::{
    dref_file_opener, DataReference, DataReferenceWrite, ExternalSampleLocation, MovDemuxer,
    MovMuxer, MuxSample, MuxTrackKind,
};

/// Sidecar "external file" bytes + the per-sample locations of the
/// audio payloads placed inside it. Layout: 8 bytes of unrelated
/// preamble (the external file need not be a QuickTime file at all),
/// then three contiguous payloads, a gap, then a fourth payload.
fn sidecar() -> (Vec<u8>, Vec<ExternalSampleLocation>, Vec<Vec<u8>>) {
    let payloads: Vec<Vec<u8>> = vec![vec![0xA0; 6], vec![0xA1; 4], vec![0xA2; 5], vec![0xA3; 7]];
    let mut file = vec![0xEE; 8]; // preamble: not sample data
    let mut locations = Vec::new();
    for (i, p) in payloads.iter().enumerate() {
        if i == 3 {
            // Gap before the last payload ⇒ a second chunk.
            file.extend_from_slice(&[0xEE; 16]);
        }
        locations.push(ExternalSampleLocation {
            offset: file.len() as u64,
            size: p.len() as u32,
        });
        file.extend_from_slice(p);
    }
    (file, locations, payloads)
}

/// A movie with track 1 = local (in-file) video, track 2 = external
/// audio whose bytes live in `media.bin`.
fn mixed_movie(locations: &[ExternalSampleLocation]) -> Vec<u8> {
    let mut m = MovMuxer::new();
    let video: Vec<MuxSample> = (0..3)
        .map(|i| MuxSample {
            data: vec![0x10 + i as u8; 12],
            duration: 100,
            keyframe: i == 0,
            composition_offset: 0,
        })
        .collect();
    let v = m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 2,
            height: 2,
        },
        600,
        video,
        &[],
    );
    assert_eq!(v, 1);
    let audio: Vec<MuxSample> = (0..locations.len())
        .map(|_| MuxSample {
            data: Vec::new(),
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        audio,
        &[],
    );
    m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
        .expect("external table");
    m.set_external_media(a, 1, locations)
        .expect("mark external");
    m.encode_to_vec().expect("encode")
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).expect("open")
}

#[test]
fn external_track_resolves_through_opener() {
    let (file, locations, payloads) = sidecar();
    let mut d = open(mixed_movie(&locations));
    assert!(d.track_has_external_data(1));
    assert!(!d.track_has_external_data(0));
    assert!(!d.has_data_reference_opener());

    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    d.set_data_reference_opener(move |r| {
        calls2.fetch_add(1, Ordering::SeqCst);
        assert_eq!(*r, DataReference::Url("media.bin".into()));
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });
    assert!(d.has_data_reference_opener());

    let mut video_payloads = Vec::new();
    let mut audio_payloads = Vec::new();
    loop {
        match d.read_next() {
            Ok((stream, _sample, data)) => {
                if stream == 0 {
                    video_payloads.push(data);
                } else {
                    audio_payloads.push(data);
                }
            }
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(video_payloads.len(), 3);
    assert!(video_payloads
        .iter()
        .enumerate()
        .all(|(i, p)| *p == vec![0x10 + i as u8; 12]));
    assert_eq!(audio_payloads, payloads);
    // The opener resolved the data reference exactly once for the
    // whole track (per-(track, dref-entry) caching).
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn without_opener_external_samples_stay_recoverable_errors() {
    let (_file, locations, _payloads) = sidecar();
    let mut d = open(mixed_movie(&locations));
    let mut local = 0usize;
    let mut external_errors = 0usize;
    loop {
        match d.read_next() {
            Ok((stream, _s, _data)) => {
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
    assert_eq!(local, 3);
    assert_eq!(external_errors, locations.len());
}

#[test]
fn opener_failure_is_sticky_and_per_sample_recoverable() {
    let (_file, locations, _payloads) = sidecar();
    let mut d = open(mixed_movie(&locations));
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    d.set_data_reference_opener(move |_r| {
        calls2.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "sidecar is offline",
        ))
    });
    let mut local = 0usize;
    let mut failures = 0usize;
    loop {
        match d.read_next() {
            Ok((stream, ..)) => {
                assert_eq!(stream, 0);
                local += 1;
            }
            Err(Error::Eof) => break,
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("sidecar is offline"),
                    "cached text replays: {msg}"
                );
                failures += 1;
            }
        }
    }
    assert_eq!(local, 3);
    assert_eq!(failures, locations.len());
    // Sticky failure: the opener ran once, not once per sample.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn external_chunk_geometry_round_trips() {
    let (_file, locations, _payloads) = sidecar();
    let d = open(mixed_movie(&locations));
    let st = &d.tracks[1].sample_table;
    // Contiguous first three payloads coalesce into one chunk; the
    // gapped fourth opens a second chunk.
    assert_eq!(
        st.chunk_offsets,
        vec![locations[0].offset, locations[3].offset]
    );
    // Per-sample resolved offsets (0-based decode-order index) equal
    // the authored locations.
    for (i, l) in locations.iter().enumerate() {
        assert_eq!(
            d.sample_offset(1, i as u32).expect("sample offset"),
            l.offset,
            "sample {i} offset"
        );
    }
}

#[test]
fn co64_promotion_for_external_offsets_past_u32() {
    let mut m = MovMuxer::new();
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        vec![MuxSample {
            data: Vec::new(),
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        }],
        &[],
    );
    m.set_data_references(a, &[DataReferenceWrite::Url("huge.bin".into())])
        .unwrap();
    let big = (u32::MAX as u64) + 4096;
    m.set_external_media(
        a,
        1,
        &[ExternalSampleLocation {
            offset: big,
            size: 3,
        }],
    )
    .unwrap();
    let bytes = m.encode_to_vec().expect("encode");
    assert!(
        bytes.windows(4).any(|w| w == b"co64"),
        "a >4GiB external offset must promote the chunk-offset box to co64"
    );
    let d = open(bytes);
    assert_eq!(d.tracks[0].sample_table.chunk_offsets, vec![big]);
}

#[test]
fn dref_file_opener_resolves_relative_sidecar_on_disk() {
    let (file, locations, payloads) = sidecar();
    let dir = std::env::temp_dir().join(format!(
        "oxideav-mov-r440-ext-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("media.bin"), &file).expect("write sidecar");
    let movie_path = dir.join("movie.mov");
    std::fs::write(&movie_path, mixed_movie(&locations)).expect("write movie");

    let f = std::fs::File::open(&movie_path).expect("open movie");
    let mut d = MovDemuxer::open(Box::new(f) as Box<dyn ReadSeek>).expect("demux");
    d.set_data_reference_opener(dref_file_opener(Some(dir.clone())));
    let mut audio = Vec::new();
    loop {
        match d.read_next() {
            Ok((1, _s, data)) => audio.push(data),
            Ok(_) => {}
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(audio, payloads);
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dref_file_opener_sandbox_rejects_escapes_and_foreign_schemes() {
    let mut opener = dref_file_opener(Some(std::env::temp_dir()));
    let unsupported =
        |r: &DataReference,
         opener: &mut dyn FnMut(&DataReference) -> std::io::Result<Box<dyn ReadSeek>>|
         -> bool {
            matches!(
                opener(r).map(|_| ()),
                Err(e) if e.kind() == std::io::ErrorKind::Unsupported
            )
        };
    for bad in [
        "../escape.bin",
        "a/../../escape.bin",
        "/etc/hosts",
        "http://example.com/media.bin",
        "rtsp://example.com/stream",
        "c:\\windows\\media.bin",
    ] {
        assert!(
            unsupported(&DataReference::Url(bad.into()), &mut opener),
            "'{bad}' must be rejected as Unsupported"
        );
    }
    // Alias records and locationless urns are out of policy too.
    assert!(unsupported(
        &DataReference::Alias(vec![1, 2, 3]),
        &mut opener
    ));
    assert!(unsupported(
        &DataReference::Urn {
            name: "urn:x".into(),
            location: String::new()
        },
        &mut opener
    ));
    // A well-shaped relative name that simply doesn't exist fails with
    // a filesystem error, NOT Unsupported — the policy accepted it.
    let missing = opener(&DataReference::Url(
        "oxideav-mov-r440-definitely-missing.bin".into(),
    ));
    assert!(matches!(
        missing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    ));
    // With no base dir, every relative reference is rejected.
    let mut no_base = dref_file_opener(None);
    assert!(unsupported(
        &DataReference::Url("media.bin".into()),
        &mut no_base
    ));
}

#[test]
fn seek_and_packets_work_on_resolved_external_track() {
    let (file, locations, payloads) = sidecar();
    let mut d = open(mixed_movie(&locations));
    d.set_data_reference_opener(move |_r| {
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });
    // Seek the external audio stream (stream index 1) to its third
    // sample's dts (2 × 1024) and drain: packets must resume there
    // with the external bytes intact.
    let landed = d.seek_to(1, 2048).expect("seek");
    assert_eq!(landed, 2048);
    let mut audio = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => {
                if p.stream_index == 1 {
                    audio.push(p.data);
                }
            }
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(audio, payloads[2..].to_vec());
}

#[test]
fn faststart_and_interleaved_local_track_coexist_with_external() {
    use oxideav_mov::{ChunkStrategy, MoovPlacement};
    let (file, locations, payloads) = sidecar();
    for placement in [MoovPlacement::AfterMdat, MoovPlacement::BeforeMdat] {
        let mut m = MovMuxer::new()
            .with_moov_placement(placement)
            .with_chunk_strategy(ChunkStrategy::InterleaveByMovieTicks(600));
        let video: Vec<MuxSample> = (0..4)
            .map(|i| MuxSample {
                data: vec![0x40 + i as u8; 9],
                duration: 300,
                keyframe: true,
                composition_offset: 0,
            })
            .collect();
        m.add_track(
            MuxTrackKind::Video {
                format: *b"raw ",
                width: 2,
                height: 2,
            },
            600,
            video,
            &[],
        );
        let audio: Vec<MuxSample> = (0..locations.len())
            .map(|_| MuxSample {
                data: Vec::new(),
                duration: 1024,
                keyframe: true,
                composition_offset: 0,
            })
            .collect();
        let a = m.add_track(
            MuxTrackKind::Audio {
                format: *b"twos",
                channels: 1,
                bits_per_sample: 16,
                sample_rate: 8000,
            },
            8000,
            audio,
            &[],
        );
        m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
            .unwrap();
        m.set_external_media(a, 1, &locations).unwrap();
        let bytes = m.encode_to_vec().expect("encode");
        let mut d = open(bytes);
        assert_eq!(
            d.is_faststart(),
            placement == MoovPlacement::BeforeMdat,
            "placement {placement:?}"
        );
        let sidecar_bytes = file.clone();
        d.set_data_reference_opener(move |_r| {
            Ok(Box::new(Cursor::new(sidecar_bytes.clone())) as Box<dyn ReadSeek>)
        });
        let mut video_out = Vec::new();
        let mut audio_out = Vec::new();
        loop {
            match d.read_next() {
                Ok((0, _s, data)) => video_out.push(data),
                Ok((1, _s, data)) => audio_out.push(data),
                Ok(_) => unreachable!(),
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert_eq!(video_out.len(), 4);
        assert!(video_out
            .iter()
            .enumerate()
            .all(|(i, p)| *p == vec![0x40 + i as u8; 9]));
        assert_eq!(audio_out, payloads, "placement {placement:?}");
    }
}

#[test]
fn applied_edit_lists_compose_with_external_resolution() {
    use oxideav_mov::MuxEdit;
    let (file, locations, payloads) = sidecar();
    // Movie timescale = media timescale so the edit-list movie-tick
    // durations line up 1:1 with media ticks.
    let mut m = MovMuxer::new().with_movie_timescale(8000);
    let audio: Vec<MuxSample> = (0..locations.len())
        .map(|_| MuxSample {
            data: Vec::new(),
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        audio,
        &[],
    );
    m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
        .unwrap();
    m.set_external_media(a, 1, &locations).unwrap();
    // Trim the first sample: presentation starts at media time 1024
    // and spans the remaining three samples (3 × 1024 movie ticks at
    // the matched timescale).
    m.set_edit_list(a, &[MuxEdit::segment(3 * 1024, 1024)])
        .expect("edit list");
    let mut d = open(m.encode_to_vec().expect("encode"));
    d.apply_edit_lists(true);
    d.set_data_reference_opener(move |_r| {
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });
    let mut pts = Vec::new();
    let mut data = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => {
                pts.push(p.pts.expect("pts"));
                data.push(p.data);
            }
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    // The trimmed head sample is dropped; the edited timeline starts
    // at 0 and the surviving samples carry the external bytes.
    assert_eq!(pts, vec![0, 1024, 2048]);
    assert_eq!(data, payloads[1..].to_vec());
}

#[test]
fn compressed_movie_resource_carries_external_track() {
    // QTFF pp. 80-81: the whole movie resource (including the dref
    // table and external chunk offsets) survives the cmov round-trip;
    // external offsets are independent of this file's layout so the
    // mdat-first ordering constraint is trivially satisfied.
    let (file, locations, payloads) = sidecar();
    let mut m = MovMuxer::new().with_compressed_movie_resource(true);
    let audio: Vec<MuxSample> = (0..locations.len())
        .map(|_| MuxSample {
            data: Vec::new(),
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        audio,
        &[],
    );
    m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
        .unwrap();
    m.set_external_media(a, 1, &locations).unwrap();
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"cmov"), "cmov emitted");
    let mut d = open(bytes);
    assert!(d.compressed_movie_algorithm.is_some());
    assert!(d.track_has_external_data(0));
    d.set_data_reference_opener(move |_r| {
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });
    let mut audio_out = Vec::new();
    loop {
        match d.read_next() {
            Ok((_, _, data)) => audio_out.push(data),
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(audio_out, payloads);
}

#[test]
fn compact_sample_size_composes_with_external_locations() {
    // stz2 sizes must come from the external locations (samples carry
    // no in-file data): varying sizes 6/4/5/7 all fit the 4-bit form.
    let (file, locations, payloads) = sidecar();
    let mut m = MovMuxer::new();
    let audio: Vec<MuxSample> = (0..locations.len())
        .map(|_| MuxSample {
            data: Vec::new(),
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        audio,
        &[],
    );
    m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
        .unwrap();
    m.set_external_media(a, 1, &locations).unwrap();
    m.set_compact_sample_size(a, true).unwrap();
    let bytes = m.encode_to_vec().expect("encode");
    assert!(bytes.windows(4).any(|w| w == b"stz2"), "stz2 emitted");
    let mut d = open(bytes);
    use oxideav_mov::SampleSizeSource;
    assert!(matches!(
        d.sample_size_source(0),
        Some(SampleSizeSource::Stz2 { field_size: 4 })
    ));
    d.set_data_reference_opener(move |_r| {
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });
    let mut audio_out = Vec::new();
    loop {
        match d.read_next() {
            Ok((_, _, data)) => audio_out.push(data),
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(audio_out, payloads);
}

#[test]
fn dangling_data_reference_index_stays_lenient_local_and_skips_opener() {
    // Byte-patch the muxed movie's audio sample entry so its
    // data_reference_index points past the 1-entry dref table. The
    // long-standing `sample_data_in_file` contract treats a dangling
    // index as *local* (every writer that emits a broken index stores
    // its data locally), so the round-440 resolution path must never
    // engage: no "external" errors, and the opener is never consulted
    // even though one is attached.
    let (_file, locations, _payloads) = sidecar();
    let mut bytes = mixed_movie(&locations);
    let pos = bytes
        .windows(4)
        .position(|w| w == b"twos")
        .expect("twos entry");
    // data_reference_index sits 8 bytes after the format FourCC.
    bytes[pos + 10] = 0;
    bytes[pos + 11] = 9; // dangling: table has 1 entry
    let mut d = open(bytes);
    assert!(
        !d.track_has_external_data(1),
        "a dangling index classifies as local"
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    d.set_data_reference_opener(move |_r| {
        calls2.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Cursor::new(Vec::new())) as Box<dyn ReadSeek>)
    });
    loop {
        match d.read_next() {
            Ok(_) => {}
            Err(Error::Eof) => break,
            Err(e) => {
                // Reads resolve against *this* file's bytes (the
                // lenient-local contract); whether a given offset is
                // readable depends on the file layout, but no error
                // may mention the external path.
                let msg = format!("{e}");
                assert!(!msg.contains("external"), "external path engaged: {msg}");
            }
        }
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a dangling index never reaches the opener"
    );
}

#[test]
fn truncated_external_file_yields_recoverable_error() {
    let (file, locations, payloads) = sidecar();
    // Cut the sidecar short mid-way through the third payload: samples
    // 1-2 read fine, sample 3 truncates, sample 4 is past EOF.
    let cut = locations[2].offset as usize + 2;
    let short = file[..cut].to_vec();
    let mut d = open(mixed_movie(&locations));
    d.set_data_reference_opener(move |_r| {
        Ok(Box::new(Cursor::new(short.clone())) as Box<dyn ReadSeek>)
    });
    let mut audio = Vec::new();
    let mut errors = 0usize;
    loop {
        match d.read_next() {
            Ok((1, _s, data)) => audio.push(data),
            Ok(_) => {}
            Err(Error::Eof) => break,
            Err(e) => {
                assert!(format!("{e}").contains("truncated"), "unexpected: {e}");
                errors += 1;
            }
        }
    }
    assert_eq!(audio, payloads[..2].to_vec());
    assert_eq!(errors, 2, "samples 3 and 4 truncate recoverably");
}

#[test]
fn external_movie_equals_self_contained_movie_payload_stream() {
    // The same media authored (a) self-contained and (b) externally
    // must present identical per-track payload streams once resolved.
    let (file, locations, payloads) = sidecar();
    let mut self_contained = MovMuxer::new();
    let sc_samples: Vec<MuxSample> = payloads
        .iter()
        .map(|p| MuxSample {
            data: p.clone(),
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    self_contained.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        sc_samples,
        &[],
    );
    let mut d_sc = open(self_contained.encode_to_vec().expect("encode"));

    let mut ext = MovMuxer::new();
    let ext_samples: Vec<MuxSample> = (0..locations.len())
        .map(|_| MuxSample {
            data: Vec::new(),
            duration: 1024,
            keyframe: true,
            composition_offset: 0,
        })
        .collect();
    let a = ext.add_track(
        MuxTrackKind::Audio {
            format: *b"twos",
            channels: 1,
            bits_per_sample: 16,
            sample_rate: 8000,
        },
        8000,
        ext_samples,
        &[],
    );
    ext.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
        .unwrap();
    ext.set_external_media(a, 1, &locations).unwrap();
    let mut d_ext = open(ext.encode_to_vec().expect("encode"));
    d_ext.set_data_reference_opener(move |_r| {
        Ok(Box::new(Cursor::new(file.clone())) as Box<dyn ReadSeek>)
    });

    loop {
        let a = d_sc.next_packet();
        let b = d_ext.next_packet();
        match (a, b) {
            (Ok(pa), Ok(pb)) => {
                assert_eq!(pa.data, pb.data);
                assert_eq!(pa.pts, pb.pts);
                assert_eq!(pa.dts, pb.dts);
                assert_eq!(pa.duration, pb.duration);
                assert_eq!(pa.flags.keyframe, pb.flags.keyframe);
            }
            (Err(Error::Eof), Err(Error::Eof)) => break,
            (a, b) => panic!("streams diverge: {a:?} vs {b:?}"),
        }
    }
}
