//! `oxideav-core` integration.
//!
//! Wired up only when the default `registry` cargo feature is on.
//! Standalone consumers (no oxideav-core dep) skip this module and
//! reach the demuxer/muxer types directly via [`crate::MovDemuxer`] /
//! [`crate::MovMuxer`].
//!
//! Both halves of the dual-API convention live here: the
//! [`ContainerRegistry`] factories (`open` via probe/extension lookup,
//! [`open_muxer`]) and the direct [`crate::MovDemuxer`] /
//! [`crate::MovMuxer`] types remain equally supported entry points.

use crate::demuxer;
use crate::muxer::{MovMuxer, MuxSample, MuxTrackKind};

use oxideav_core::{
    CodecParameters, CodecTag, ContainerRegistry, Error, MediaType, Muxer, Packet, Result,
    StreamInfo, WriteSeek,
};

/// Install the QTFF demuxer + muxer into a [`ContainerRegistry`].
///
/// Registers:
///
/// * `mov` demuxer factory
/// * `mov` muxer factory ([`open_muxer`])
/// * `mov` / `qt` filename extensions → `mov` container
/// * `mov` content probe (recognises `ftyp qt  ` and `ftyp ...
///   compat: qt  ` patterns)
pub fn register_containers(reg: &mut ContainerRegistry) {
    reg.register_demuxer("mov", demuxer::open);
    reg.register_muxer("mov", open_muxer);
    reg.register_extension("mov", "mov");
    reg.register_extension("qt", "mov");
    reg.register_probe("mov", demuxer::probe);
}

/// Install the QTFF demuxer + muxer into an
/// [`oxideav_core::RuntimeContext`]. Convenience wrapper around
/// [`register_containers`] that matches the uniform
/// `register(&mut RuntimeContext)` entry point every sibling crate
/// exposes; `oxideav_meta::register_all` calls
/// `crate::__oxideav_entry(ctx)` which dispatches here.
pub fn register(ctx: &mut oxideav_core::RuntimeContext) {
    register_containers(&mut ctx.containers);
}

oxideav_core::register!("mov", register);

// ───────────────────────── muxer glue ─────────────────────────

/// Map a stream's codec identity to the QTFF sample-description
/// FourCC.
///
/// The codec-id table is consulted first: it carries the
/// case-sensitive QTFF spellings (sound formats per the QTFF sound
/// sample description tables — `twos`/`sowt`/`fl32`/… — and the
/// conventional ISO video entry types). When the id is unknown but the
/// stream carries a [`CodecTag::Fourcc`] (e.g. a remux straight out of
/// [`crate::MovDemuxer`], which tags every stream with its on-wire
/// format), the tag's bytes are used with alphabetic bytes folded to
/// lowercase — `CodecTag::fourcc` canonicalises to uppercase for
/// case-insensitive matching, and lowercase is the conventional QTFF
/// spelling being restored.
fn fourcc_for(params: &CodecParameters) -> Result<[u8; 4]> {
    let by_id: Option<&[u8; 4]> = match params.codec_id.as_str() {
        "h264" => Some(b"avc1"),
        "h265" | "hevc" => Some(b"hvc1"),
        "mpeg4video" => Some(b"mp4v"),
        "mjpeg" => Some(b"jpeg"),
        "rawvideo" => Some(b"raw "),
        "aac" => Some(b"mp4a"),
        "alac" => Some(b"alac"),
        "pcm_s16be" => Some(b"twos"),
        "pcm_s16le" => Some(b"sowt"),
        "pcm_u8" => Some(b"raw "),
        "pcm_f32be" => Some(b"fl32"),
        "pcm_f64be" => Some(b"fl64"),
        "pcm_s24be" => Some(b"in24"),
        "pcm_s32be" => Some(b"in32"),
        "pcm_mulaw" | "ulaw" => Some(b"ulaw"),
        "pcm_alaw" | "alaw" => Some(b"alaw"),
        _ => None,
    };
    if let Some(f) = by_id {
        return Ok(*f);
    }
    if let Some(CodecTag::Fourcc(f)) = params.tag {
        let mut out = f;
        for b in &mut out {
            *b = b.to_ascii_lowercase();
        }
        return Ok(out);
    }
    Err(Error::unsupported(format!(
        "MOV muxer: no QTFF sample-description format known for codec '{}' (no FourCC tag on the stream either)",
        params.codec_id.as_str()
    )))
}

/// One buffered input packet, timestamps in the stream's own
/// time base.
struct PendingSample {
    data: Vec<u8>,
    dts: i64,
    pts: i64,
    duration: Option<i64>,
    keyframe: bool,
}

/// Per-stream accumulation state for [`MovRegistryMuxer`].
struct PendingTrack {
    kind: MuxTrackKind,
    /// `mdhd.time_scale` — the stream time base's denominator.
    media_timescale: u32,
    /// Ticks-per-value multiplier — the stream time base's numerator,
    /// so `value × tick_num` is in `media_timescale` units.
    tick_num: i64,
    /// Codec-config extension atoms for the `stsd` entry, verbatim
    /// (the same framed-atoms blob the demuxer surfaces as
    /// `extradata`).
    extra: Vec<u8>,
    samples: Vec<PendingSample>,
    /// Synthesised decode time for the next packet that arrives
    /// without any timestamp of its own.
    next_auto_dts: i64,
}

/// [`Muxer`]-trait adapter over [`MovMuxer`] for the
/// [`ContainerRegistry`] path.
///
/// The QTFF non-fragmented layout needs the complete sample tables
/// before anything can be finalized, so packets are buffered:
/// `write_header` only validates state, `write_packet` accumulates,
/// and `write_trailer` performs the entire encode and single write to
/// the output. Missing per-packet durations are recovered at trailer
/// time from decode-timestamp deltas (last sample: previous duration,
/// or 1 tick).
pub struct MovRegistryMuxer {
    out: Box<dyn WriteSeek>,
    tracks: Vec<PendingTrack>,
    header_written: bool,
    finished: bool,
}

/// [`ContainerRegistry`] muxer factory (`OpenMuxerFn`) for the `mov`
/// format. Accepts video and audio streams; each stream's codec
/// identity must resolve to a QTFF sample-description FourCC (see the
/// registry-glue mapping) and its time base denominator becomes the
/// track's media timescale.
pub fn open_muxer(output: Box<dyn WriteSeek>, streams: &[StreamInfo]) -> Result<Box<dyn Muxer>> {
    if streams.is_empty() {
        return Err(Error::invalid("MOV muxer: at least one stream required"));
    }
    let mut tracks = Vec::with_capacity(streams.len());
    for s in streams {
        if s.time_base.num() <= 0 || s.time_base.den() <= 0 || s.time_base.den() > u32::MAX as i64 {
            return Err(Error::invalid(format!(
                "MOV muxer: stream {} time base {}/{} not usable as a media timescale",
                s.index,
                s.time_base.num(),
                s.time_base.den()
            )));
        }
        let format = fourcc_for(&s.params)?;
        let kind = match s.params.media_type {
            MediaType::Video => MuxTrackKind::Video {
                format,
                width: s.params.width.unwrap_or(0).min(u16::MAX as u32) as u16,
                height: s.params.height.unwrap_or(0).min(u16::MAX as u32) as u16,
            },
            MediaType::Audio => {
                let sample_rate = s.params.sample_rate.ok_or_else(|| {
                    Error::invalid(format!(
                        "MOV muxer: audio stream {} carries no sample rate",
                        s.index
                    ))
                })?;
                MuxTrackKind::Audio {
                    format,
                    channels: s.params.channels.unwrap_or(1),
                    bits_per_sample: 16,
                    sample_rate,
                }
            }
            other => {
                return Err(Error::unsupported(format!(
                    "MOV muxer: stream {} has media type {other:?}; only video and audio are accepted on the registry path",
                    s.index
                )))
            }
        };
        tracks.push(PendingTrack {
            kind,
            media_timescale: s.time_base.den() as u32,
            tick_num: s.time_base.num(),
            extra: s.params.extradata.clone(),
            samples: Vec::new(),
            next_auto_dts: 0,
        });
    }
    Ok(Box::new(MovRegistryMuxer {
        out: output,
        tracks,
        header_written: false,
        finished: false,
    }))
}

impl Muxer for MovRegistryMuxer {
    fn format_name(&self) -> &str {
        "mov"
    }

    fn write_header(&mut self) -> Result<()> {
        if self.finished {
            return Err(Error::invalid("MOV muxer: already finalized"));
        }
        if self.header_written {
            return Err(Error::invalid("MOV muxer: header already written"));
        }
        // Nothing hits the output yet: the non-fragmented QTFF layout
        // is finalized in one piece at write_trailer once the sample
        // tables are complete.
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::invalid(
                "MOV muxer: write_header must be called before write_packet",
            ));
        }
        if self.finished {
            return Err(Error::invalid("MOV muxer: already finalized"));
        }
        let idx = packet.stream_index as usize;
        let track = self.tracks.get_mut(idx).ok_or_else(|| {
            Error::invalid(format!(
                "MOV muxer: packet for unknown stream index {}",
                packet.stream_index
            ))
        })?;
        if packet.flags.header {
            // Codec-level header packets belong in the sample
            // description's codec-config atoms, not the sample stream.
            return Ok(());
        }
        let dts = packet.dts.or(packet.pts).unwrap_or(track.next_auto_dts);
        let pts = packet.pts.unwrap_or(dts);
        track.next_auto_dts = dts.saturating_add(packet.duration.unwrap_or(1).max(1));
        track.samples.push(PendingSample {
            data: packet.data.clone(),
            dts,
            pts,
            duration: packet.duration,
            keyframe: packet.flags.keyframe,
        });
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::invalid(
                "MOV muxer: write_header must be called before write_trailer",
            ));
        }
        if self.finished {
            return Err(Error::invalid("MOV muxer: already finalized"));
        }
        let mut m = MovMuxer::new();
        for (i, t) in self.tracks.iter().enumerate() {
            if t.samples.is_empty() {
                return Err(Error::invalid(format!(
                    "MOV muxer: stream {i} received no packets"
                )));
            }
            let n = t.samples.len();
            let mut samples = Vec::with_capacity(n);
            let mut prev_duration: i64 = 1;
            for (j, s) in t.samples.iter().enumerate() {
                // Recover a missing duration from the decode-time gap
                // to the next sample; the final sample reuses the
                // previous duration (QTFF sample durations must be
                // positive, so degrade to 1 tick at worst).
                let dur_ticks = match s.duration {
                    Some(d) if d > 0 => d,
                    _ if j + 1 < n => (t.samples[j + 1].dts - s.dts).max(1),
                    _ => prev_duration,
                };
                prev_duration = dur_ticks;
                let scaled_dur = dur_ticks
                    .saturating_mul(t.tick_num)
                    .clamp(1, u32::MAX as i64) as u32;
                let offset = (s.pts - s.dts).saturating_mul(t.tick_num);
                let composition_offset = offset.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                samples.push(MuxSample {
                    data: s.data.clone(),
                    duration: scaled_dur,
                    keyframe: s.keyframe,
                    composition_offset,
                });
            }
            m.add_track(t.kind.clone(), t.media_timescale, samples, &t.extra);
        }
        m.write_to(&mut self.out)?;
        self.out.flush().map_err(Error::from)?;
        self.finished = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_via_runtime_context_installs_container() {
        let mut ctx = oxideav_core::RuntimeContext::new();
        register(&mut ctx);
        assert_eq!(ctx.containers.container_for_extension("mov"), Some("mov"));
        assert_eq!(ctx.containers.container_for_extension("qt"), Some("mov"));
        assert!(
            ctx.containers.muxer_names().any(|n| n == "mov"),
            "muxer factory must be registered"
        );
    }
}
