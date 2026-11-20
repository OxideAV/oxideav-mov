//! The registry muxer places each stream on the movie timeline from
//! its packets' timestamps: an empty edit for a stream that starts
//! late, and a segment entering the media at its first presented
//! instant (skipping decode-only leading packets and the reordering
//! delay). A movie read with its edit lists applied — decode-only
//! media included — and written back presents exactly as before.

#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};

use oxideav_core::{
    CodecId, CodecParameters, Demuxer, Error, Packet, ReadSeek, StreamInfo, TimeBase, WriteSeek,
};
use oxideav_mov::{Edit, MovDemuxer, MovMuxer, MuxEdit, MuxSample, MuxTrackKind};

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for SharedBuf {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>).expect("open")
}

fn drain(d: &mut MovDemuxer) -> Vec<Packet> {
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => out.push(p),
            Err(Error::Eof) => return out,
            Err(e) => panic!("{e}"),
        }
    }
}

fn mux(streams: &[StreamInfo], packets: &[Packet]) -> Vec<u8> {
    let sink = SharedBuf::default();
    let out: Box<dyn WriteSeek> = Box::new(sink.clone());
    let mut m = oxideav_mov::registry::open_muxer(out, streams).unwrap();
    m.write_header().unwrap();
    for p in packets {
        m.write_packet(p).unwrap();
    }
    m.write_trailer().unwrap();
    sink.0.lock().unwrap().get_ref().clone()
}

/// Video with a two-frame reordering delay and an edit entering at
/// the first composition time; compressed audio whose first packet
/// is priming, skipped by its edit.
fn source() -> Vec<u8> {
    let mut m = MovMuxer::new().with_movie_timescale(48000);
    let offsets = [200i32, 500, 100, 200, 500, 100, 200, 200];
    let video: Vec<MuxSample> = (0..8)
        .map(|i| {
            MuxSample::default()
                .with_data(vec![0x10 + i as u8; 40])
                .with_duration(100)
                .with_keyframe(i % 3 == 0)
                .with_composition_offset(offsets[i])
        })
        .collect();
    let v = m.add_track(
        MuxTrackKind::Video {
            format: *b"raw ",
            width: 8,
            height: 8,
        },
        1000,
        video,
        &[],
    );
    m.set_edit_list(v, &[MuxEdit::segment(800 * 48, 200)])
        .unwrap();
    let audio: Vec<MuxSample> = (0..40)
        .map(|i| {
            MuxSample::default()
                .with_data(vec![0x80 + i as u8; 20])
                .with_duration(1024)
                .with_keyframe(true)
        })
        .collect();
    let a = m.add_track(
        MuxTrackKind::Audio {
            format: *b".mp3",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        audio,
        &[],
    );
    m.set_edit_list(a, &[MuxEdit::segment(39 * 1024, 1024)])
        .unwrap();
    m.encode_to_vec().unwrap()
}

/// (stream, pts, dts, discard flag, payload length) of one packet.
type Timing = (u32, Option<i64>, Option<i64>, bool, usize);

fn timing(packets: &[Packet]) -> Vec<Timing> {
    packets
        .iter()
        .map(|p| (p.stream_index, p.pts, p.dts, p.flags.discard, p.data.len()))
        .collect()
}

#[test]
fn an_edited_movie_written_back_presents_as_before() {
    let mut d = open(source());
    d.apply_edit_lists(true);
    d.emit_never_presented(true);
    let streams = d.streams().to_vec();
    let packets = drain(&mut d);
    // The priming packet is there, flagged, ahead of time zero.
    let priming = packets.iter().find(|p| p.stream_index == 1).unwrap();
    assert!(priming.flags.discard);
    assert_eq!(priming.pts, Some(-1024));

    let mut d2 = open(mux(&streams, &packets));
    // The same edits, re-derived from the timestamps.
    assert_eq!(d2.tracks[0].edits.len(), 1);
    assert_eq!(d2.tracks[0].edits[0].media_time, 200);
    assert_eq!(d2.tracks[1].edits.len(), 1);
    assert_eq!(d2.tracks[1].edits[0].media_time, 1024);
    d2.apply_edit_lists(true);
    d2.emit_never_presented(true);
    let mut again = drain(&mut d2);
    // Packets come back in file order, which the streaming muxer
    // makes arrival order: identical, timing and all.
    assert_eq!(timing(&again), timing(&packets));
    again.retain(|p| !p.flags.discard);
    assert_eq!(again.iter().filter(|p| p.stream_index == 1).count(), 39);
}

#[test]
fn media_time_packets_keep_their_presentation_times() {
    // Read without applying the edits: packets carry media times, and
    // the written movie presents them at those times — the video's
    // first frame 0.2 s in (its composition offset), the audio at 0.
    let mut d = open(source());
    let streams = d.streams().to_vec();
    let packets = drain(&mut d);
    let mut d2 = open(mux(&streams, &packets));
    let video_edits: &[Edit] = &d2.tracks[0].edits;
    assert_eq!(video_edits.len(), 2);
    assert_eq!(video_edits[0].media_time, -1, "an empty edit first");
    assert_eq!(video_edits[0].track_duration, 120, "0.2 s at 600");
    assert_eq!(video_edits[1].media_time, 200);
    assert!(d2.tracks[1].edits.is_empty(), "audio plays from its start");

    d2.apply_edit_lists(true);
    let presented = drain(&mut d2);
    let first_video = presented.iter().find(|p| p.stream_index == 0).unwrap();
    assert_eq!(first_video.pts, Some(200));
    let first_audio = presented.iter().find(|p| p.stream_index == 1).unwrap();
    assert_eq!(first_audio.pts, Some(0));
}

#[test]
fn a_stream_that_starts_late_gets_an_empty_edit() {
    let mut v = CodecParameters::video(CodecId::new("rawvideo"));
    v.width = Some(4);
    v.height = Some(4);
    let mut a = CodecParameters::audio(CodecId::new("pcm_s16le")).channels(1);
    a.sample_rate = Some(8000);
    let stream = |index, params, den| StreamInfo {
        index,
        time_base: TimeBase::new(1, den),
        duration: None,
        start_time: None,
        params,
    };
    let streams = [stream(0, v, 25), stream(1, a, 8000)];
    let mut packets = Vec::new();
    // Video from 10 s; uncompressed sound from 10.5 s.
    for i in 0..25i64 {
        packets.push(
            Packet::new(0, streams[0].time_base, vec![i as u8; 16])
                .with_pts(250 + i)
                .with_dts(250 + i)
                .with_duration(1)
                .with_keyframe(true),
        );
    }
    packets.push(Packet::new(1, streams[1].time_base, vec![7u8; 8000]).with_pts(84000));
    let mut d = open(mux(&streams, &packets));
    // The movie starts at the earliest stream: no edit there.
    assert!(d.tracks[0].edits.is_empty());
    let sound = &d.tracks[1].edits;
    assert_eq!(sound.len(), 2);
    assert_eq!((sound[0].media_time, sound[0].track_duration), (-1, 300));
    assert_eq!((sound[1].media_time, sound[1].track_duration), (0, 300));

    d.apply_edit_lists(true);
    let first_sound = drain(&mut d)
        .into_iter()
        .find(|p| p.stream_index == 1)
        .unwrap();
    assert_eq!(first_sound.pts, Some(4000), "half a second in");
}

// ─────────────── black-box reader ───────────────

fn ffprobe_rows(path: &std::path::Path) -> Option<Vec<String>> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=index,codec_name,start_pts,duration_ts,nb_frames",
            "-of",
            "csv",
        ])
        .arg(path)
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with("stream,"))
            .map(str::to_string)
            .collect()
    })
}

/// A black-box reader reports the same start, duration and frame
/// count for each stream of a fixture and of its edited-mode remux.
#[test]
fn black_box_reader_sees_the_same_presentation_after_a_remux() {
    if Command::new("ffprobe").arg("-version").output().is_err() {
        eprintln!("skip: ffprobe not installed");
        return;
    }
    for name in ["aac_2s.mov", "h264_2s.mov"] {
        let src = std::path::PathBuf::from(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ));
        let mut d = open(std::fs::read(&src).unwrap());
        d.apply_edit_lists(true);
        d.emit_never_presented(true);
        let streams = d.streams().to_vec();
        let packets = drain(&mut d);
        let remuxed = mux(&streams, &packets);
        let dst =
            std::env::temp_dir().join(format!("oxideav-mov-edits-{}-{name}", std::process::id()));
        std::fs::write(&dst, &remuxed).unwrap();
        let (Some(a), Some(b)) = (ffprobe_rows(&src), ffprobe_rows(&dst)) else {
            eprintln!("skip: ffprobe could not read {name}");
            std::fs::remove_file(&dst).ok();
            return;
        };
        std::fs::remove_file(&dst).ok();
        assert_eq!(a, b, "{name}");
    }
}
