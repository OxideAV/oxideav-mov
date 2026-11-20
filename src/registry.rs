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

use crate::audio_params::PcmLayout;
use crate::demuxer;
use crate::muxer::{
    AudioEntryV2, ExternalSampleLocation, MovMetadata, MovMuxer, MuxEdit, MuxSample, MuxTrackKind,
    VisualExtensions,
};
use crate::track::LpcmFlags;

use std::io::SeekFrom;

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
/// case-sensitive QTFF spellings (compressed sound formats per the
/// QTFF sound sample description tables — `ulaw` / `alaw` / … — and
/// the conventional ISO video entry types; PCM is written by
/// [`audio_description`]). When the id is unknown but the
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

/// How an audio stream's sound sample description is written.
struct AudioPlan {
    format: [u8; 4],
    channels: u16,
    /// The version-0 sample size field.
    bits: u16,
    v2: Option<AudioEntryV2>,
    /// A framed `chan` extension atom, or empty.
    chan: Vec<u8>,
    /// Bytes of one audio frame when the sample table counts frames
    /// (uncompressed sound and the one-byte-per-sample companded
    /// formats), `None` when each packet is one table sample.
    frame_bytes: Option<u32>,
}

/// The sound sample description that states an audio stream's facts
/// (the inverse of the demuxer's reading):
///
/// * 8-bit and 16-bit PCM in at most two channels at a 16-bit rate
///   keep the classic version-0 codes (QTFF 2012-08-14 pp. 176 – 178:
///   `raw ` 8-bit offset binary, `twos` big-endian / 8-bit signed,
///   `sowt` little-endian, "8 or 16 bits per sample");
/// * every other PCM layout — wider samples, little-endian 24 / 32-bit
///   or float, more channels, higher rates — is an `lpcm` version-2
///   description (pp. 181 – 183) whose format-specific flags and
///   constant bits per channel spell the layout;
/// * other codecs keep their format code; more than two channels or a
///   rate past the 16.16 field take the version-2 layout, which those
///   need ("Audio channel layouts with more than two channels per
///   track require … a version 2 sound sample description", p. 186);
/// * a stream channel layout with a predefined `chan` tag is written
///   as a `chan` extension atom.
fn audio_description(params: &CodecParameters, sample_rate: u32) -> Result<AudioPlan> {
    let channels = params.resolved_channels().unwrap_or(1);
    let chan = params
        .channel_layout
        .filter(|l| l.channel_count() == channels)
        .and_then(crate::audio_params::chan_tag_for_layout)
        .map(crate::audio_params::chan_atom_for_tag)
        .unwrap_or_default();
    let classic = channels <= 2 && sample_rate <= 0xFFFF;
    if let Some(l) = PcmLayout::from_codec_id(params.codec_id.as_str()) {
        let v0 = match (l.float, l.signed, l.bits, l.big_endian) {
            (false, false, 8, _) => Some((*b"raw ", 8)),
            (false, true, 8, _) => Some((*b"twos", 8)),
            (false, true, 16, true) => Some((*b"twos", 16)),
            (false, true, 16, false) => Some((*b"sowt", 16)),
            _ => None,
        };
        let frame_bytes = Some(l.bits / 8 * u32::from(channels));
        if let (Some((format, bits)), true) = (v0, classic) {
            return Ok(AudioPlan {
                format,
                channels,
                bits,
                v2: None,
                chan,
                frame_bytes,
            });
        }
        let mut flags = LpcmFlags::IS_PACKED;
        if l.float {
            flags |= LpcmFlags::IS_FLOAT;
        } else if l.signed {
            flags |= LpcmFlags::IS_SIGNED_INTEGER;
        }
        if l.big_endian && l.bits > 8 {
            flags |= LpcmFlags::IS_BIG_ENDIAN;
        }
        return Ok(AudioPlan {
            format: *b"lpcm",
            channels,
            bits: 16,
            v2: Some(AudioEntryV2 {
                audio_sample_rate: f64::from(sample_rate),
                const_bits_per_channel: l.bits,
                format_specific_flags: LpcmFlags(flags),
                const_bytes_per_audio_packet: l.bits / 8 * u32::from(channels),
                const_lpcm_frames_per_audio_packet: 1,
            }),
            chan,
            frame_bytes,
        });
    }
    let format = fourcc_for(params)?;
    let v2 = (!classic).then(|| AudioEntryV2 {
        audio_sample_rate: f64::from(sample_rate),
        ..AudioEntryV2::default()
    });
    // QTFF 2012-08-14 pp. 187 – 188: the companded formats hold one
    // byte per sample per channel.
    let frame_bytes = matches!(&format, b"ulaw" | b"alaw").then_some(u32::from(channels));
    Ok(AudioPlan {
        format,
        channels,
        bits: 16,
        v2,
        chan,
        frame_bytes,
    })
}

/// The `colr` record stating a colour signal; `None` when the signal
/// says nothing. A signalled range needs the `nclx` record (ISO/IEC
/// 14496-12 §12.1.5, with its full-range flag); with the range left
/// open the QuickTime `nclc` record (QTFF 2012-08-14 p. 162), which
/// has no range field, says exactly as much.
fn colr_for(signal: &oxideav_core::ColorSignal) -> Option<crate::media_meta::ColorParameters> {
    use crate::media_meta::ColorParametersKind;
    if signal.is_unspecified() {
        return None;
    }
    let primaries = u16::from(signal.primaries.code_point());
    let transfer = u16::from(signal.transfer.code_point());
    let matrix = u16::from(signal.matrix.code_point());
    let kind = match signal.range {
        oxideav_core::ColorRange::Full | oxideav_core::ColorRange::Limited => {
            ColorParametersKind::Nclx {
                primaries,
                transfer,
                matrix,
                full_range: signal.range == oxideav_core::ColorRange::Full,
            }
        }
        _ => ColorParametersKind::Nclc {
            primaries,
            transfer,
            matrix,
        },
    };
    Some(crate::media_meta::ColorParameters { kind })
}

/// One input packet already written to the output: where its bytes
/// are, and its timestamps in the stream's own time base.
struct WrittenSample {
    offset: u64,
    size: u32,
    dts: i64,
    pts: i64,
    duration: Option<i64>,
    keyframe: bool,
    /// Decode-only: not part of the presentation (decoder priming, a
    /// trimmed head).
    discard: bool,
}

/// Per-stream accumulation state for [`MovRegistryMuxer`].
struct PendingTrack {
    kind: MuxTrackKind,
    /// `mdhd.time_scale` — the stream time base's denominator.
    media_timescale: u32,
    /// Ticks-per-value multiplier — the stream time base's numerator,
    /// so `value × tick_num` is in `media_timescale` units.
    tick_num: i64,
    /// The stream time base's denominator (the media timescale,
    /// except for frame-counted sound, whose timescale is the sample
    /// rate).
    time_base_den: i64,
    /// Codec-config extension atoms for the `stsd` entry: the
    /// stream's extradata (a bare configuration record, as the
    /// demuxer surfaces it) framed as the format's configuration atom
    /// by [`crate::codec_config::config_atom_for_format`].
    extra: Vec<u8>,
    /// The QTFF version-2 sound description the track is written
    /// with (`lpcm` and multichannel / high-rate audio).
    sound_v2: Option<AudioEntryV2>,
    /// See [`AudioPlan::frame_bytes`].
    frame_bytes: Option<u32>,
    /// The stream's language tag, written to `mdhd` (a three-letter
    /// ISO 639-2/T code) or to an `elng` (anything longer).
    language: Option<String>,
    /// The video stream's colour signal as a `colr` record, when any
    /// of it is specified.
    color: Option<crate::media_meta::ColorParameters>,
    /// One row per packet written (per run of frames for a
    /// frame-counted sound track).
    written: Vec<WrittenSample>,
    /// Frame-counted sound: the bytes of a frame whose rest arrives
    /// with the next packet.
    carry: Vec<u8>,
    /// Frame-counted sound: the first packet's presentation time.
    frames_start: Option<i64>,
    /// Synthesised decode time for the next packet that arrives
    /// without any timestamp of its own.
    next_auto_dts: i64,
}

/// [`Muxer`]-trait adapter over [`MovMuxer`] for the
/// [`ContainerRegistry`] path.
///
/// Packet payloads go to the output as they arrive; only a
/// table row per packet is kept. `write_header` writes `ftyp`, a
/// `wide` placeholder and the `mdat` header; `write_packet` appends
/// the payload (so the media is interleaved in arrival order, a chunk
/// per run of one stream's packets); `write_trailer` patches the
/// `mdat` size — widening the header over the `wide` atom when the
/// payload passes 4 GiB (QTFF 2012-08-14 p. 22) — and appends the
/// `moov`. Missing per-packet durations are recovered at trailer time
/// from decode-timestamp deltas (last sample: previous duration, or 1
/// tick).
pub struct MovRegistryMuxer {
    out: Box<dyn WriteSeek>,
    tracks: Vec<PendingTrack>,
    header_written: bool,
    finished: bool,
    /// Output position of the first byte this muxer writes.
    base: u64,
    /// Bytes handed to the muxer so far, relative to `base`: the
    /// file offset of the next payload byte.
    pos: u64,
    /// Payload bytes not yet passed to `out`.
    block: Vec<u8>,
}

/// Movie timescale of the written file (QTFF's customary 600).
const MOVIE_TIMESCALE: u32 = 600;

/// An instant as `ticks / timescale` seconds.
#[derive(Clone, Copy)]
struct Instant {
    ticks: i128,
    timescale: i128,
}

impl Instant {
    fn cmp_seconds(&self, other: &Self) -> std::cmp::Ordering {
        (self.ticks * other.timescale).cmp(&(other.ticks * self.timescale))
    }
}

/// What a stream's packets present: from when, starting where in the
/// media, for how long.
struct PresentedSpan {
    /// Presentation time of the earliest presented packet.
    start: Instant,
    /// Media time (media timescale ticks from the first packet's
    /// decode time) of that packet.
    media_start: i64,
    /// Presented duration in media timescale ticks.
    duration: i128,
    /// The media timescale (`duration`'s unit; the start instant has
    /// its own).
    media_timescale: i128,
}

impl PresentedSpan {
    /// The edit list placing this span on the movie timeline (QTFF
    /// 2012-08-14 pp. 46 – 48): an empty edit for the time before the
    /// stream starts, then one segment entering the media at its
    /// first presented instant — skipping decode-only leading packets
    /// and the reordering delay of the composition offsets. Empty
    /// when the media plays from its start at the origin, which needs
    /// no edit list.
    fn edits(&self, origin: &Instant) -> Vec<MuxEdit> {
        let movie = i128::from(MOVIE_TIMESCALE);
        let ts = self.start.timescale;
        // start − origin, in movie ticks, to the nearest tick.
        let delay_num = self.start.ticks * origin.timescale - origin.ticks * ts;
        let delay_den = ts * origin.timescale;
        let delay = ((delay_num * movie + delay_den / 2) / delay_den).max(0) as u64;
        if delay == 0 && self.media_start == 0 {
            return Vec::new();
        }
        // The whole presented span, rounded up to a movie tick.
        let media = self.media_timescale;
        let duration = ((self.duration * movie + media - 1) / media).max(1) as u64;
        let mut edits = Vec::with_capacity(2);
        if delay > 0 {
            edits.push(MuxEdit::empty(delay));
        }
        edits.push(MuxEdit::segment(duration, self.media_start));
        edits
    }
}

impl PendingTrack {
    /// The span this stream's packets present, from their timestamps
    /// (decode-only packets — the `discard` flag — present nothing).
    /// `None` when no packet is presented.
    fn presented(&self) -> Option<PresentedSpan> {
        let timescale = i128::from(self.media_timescale);
        if let Some(frame) = self.frame_bytes {
            // One tick per frame at the sample rate; the first
            // packet's time is in the stream's time base.
            let frames: i128 = self
                .written
                .iter()
                .map(|w| i128::from(w.size / frame))
                .sum();
            let start = i128::from(self.frames_start?) * i128::from(self.tick_num);
            let base = i128::from(self.time_base_den);
            return Some(PresentedSpan {
                start: Instant {
                    ticks: start,
                    timescale: base,
                },
                media_start: 0,
                duration: frames,
                media_timescale: timescale,
            });
        }
        let first_dts = self.written.first()?.dts;
        let presented = || self.written.iter().filter(|w| !w.discard);
        let start = presented().map(|w| w.pts).min()?;
        let end = presented()
            .map(|w| w.pts.saturating_add(w.duration.unwrap_or(0).max(0)))
            .max()?;
        let tick = i128::from(self.tick_num);
        Some(PresentedSpan {
            start: Instant {
                ticks: i128::from(start) * tick,
                timescale,
            },
            media_start: i64::try_from((i128::from(start) - i128::from(first_dts)).max(0) * tick)
                .ok()?,
            duration: (i128::from(end) - i128::from(start)).max(0) * tick,
            media_timescale: timescale,
        })
    }
}

/// File offset of the `wide` placeholder: right after the 28-byte
/// `ftyp`.
const WIDE_AT: u64 = 28;
/// File offset of the first `mdat` payload byte: `ftyp`, `wide`, and
/// the 8-byte `mdat` header.
const PAYLOAD_AT: u64 = WIDE_AT + 16;
/// Payload bytes gathered before they are passed to the output.
const BLOCK: usize = 256 * 1024;
/// Most bytes one frame-run row may grow to by absorbing the packets
/// that follow it contiguously.
const MAX_RUN_BYTES: u32 = 1 << 20;

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
        let mut v2 = None;
        let mut chan = Vec::new();
        let mut frame_bytes = None;
        let kind = match s.params.media_type {
            MediaType::Video => MuxTrackKind::Video {
                format: fourcc_for(&s.params)?,
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
                let plan = audio_description(&s.params, sample_rate)?;
                v2 = plan.v2;
                chan = plan.chan;
                frame_bytes = plan.frame_bytes;
                MuxTrackKind::Audio {
                    format: plan.format,
                    channels: plan.channels,
                    bits_per_sample: plan.bits,
                    sample_rate,
                }
            }
            other => {
                return Err(Error::unsupported(format!(
                    "MOV muxer: stream {} has media type {other:?}; only video and audio are accepted on the registry path",
                    s.index
                )));
            }
        };
        let format = match &kind {
            MuxTrackKind::Video { format, .. } | MuxTrackKind::Audio { format, .. } => *format,
            _ => unreachable!("only video and audio kinds are built above"),
        };
        let mut extra = crate::codec_config::config_atom_for_format(&format, &s.params.extradata);
        // Extradata passed through as raw extension atoms may carry
        // its own `chan` already.
        if !crate::codec_config::has_top_level_atom(&extra, b"chan") {
            extra.extend_from_slice(&chan);
        }
        // A frame-counted table has one tick per frame: its media
        // timescale is the sample rate whatever the stream's time
        // base.
        let frame_rate = match (&kind, frame_bytes) {
            (MuxTrackKind::Audio { sample_rate, .. }, Some(_)) => Some(*sample_rate),
            _ => None,
        };
        tracks.push(PendingTrack {
            kind,
            media_timescale: frame_rate.unwrap_or(s.time_base.den() as u32),
            tick_num: s.time_base.num(),
            time_base_den: s.time_base.den(),
            extra,
            sound_v2: v2,
            frame_bytes,
            language: s.params.language.clone().filter(|l| !l.is_empty()),
            color: (s.params.media_type == MediaType::Video)
                .then(|| colr_for(&s.params.color_signal))
                .flatten(),
            written: Vec::new(),
            carry: Vec::new(),
            frames_start: None,
            next_auto_dts: 0,
        });
    }
    Ok(Box::new(MovRegistryMuxer {
        out: output,
        tracks,
        header_written: false,
        finished: false,
        base: 0,
        pos: 0,
        block: Vec::new(),
    }))
}

impl MovRegistryMuxer {
    /// Append payload bytes at the current file position.
    fn put(&mut self, data: &[u8]) -> Result<()> {
        if self.block.len() + data.len() > BLOCK {
            self.flush_block()?;
        }
        if data.len() >= BLOCK {
            self.out.write_all(data).map_err(Error::from)?;
        } else {
            self.block.extend_from_slice(data);
        }
        self.pos += data.len() as u64;
        Ok(())
    }

    fn flush_block(&mut self) -> Result<()> {
        self.out.write_all(&self.block).map_err(Error::from)?;
        self.block.clear();
        Ok(())
    }
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
        self.base = self.out.stream_position().map_err(Error::from)?;
        let mut head = crate::muxer::build_ftyp();
        debug_assert_eq!(head.len() as u64, WIDE_AT);
        head.extend_from_slice(&8u32.to_be_bytes());
        head.extend_from_slice(b"wide");
        // Size 0 — "extends to the end of the file" — until the
        // trailer states the real one.
        head.extend_from_slice(&0u32.to_be_bytes());
        head.extend_from_slice(b"mdat");
        self.put(&head)?;
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
        let offset = self.pos;
        if let Some(frame) = track.frame_bytes {
            // Frame-counted sound: the packets are one continuous run
            // of frames, a tick each at the sample rate. Only whole
            // frames are written; the split frame's head waits for
            // its rest.
            let frame = frame as usize;
            if track.frames_start.is_none() {
                track.frames_start = Some(packet.pts.or(packet.dts).unwrap_or(0));
            }
            let carried = std::mem::take(&mut track.carry);
            let total = carried.len() + packet.data.len();
            let whole = total - total % frame;
            if whole == 0 {
                track.carry = carried;
                track.carry.extend_from_slice(&packet.data);
                return Ok(());
            }
            let from_packet = whole - carried.len();
            track.carry.extend_from_slice(&packet.data[from_packet..]);
            let size = u32::try_from(whole)
                .map_err(|_| Error::invalid(format!("MOV muxer: stream {idx} packet too large")))?;
            match track.written.last_mut() {
                Some(last)
                    if last.offset + u64::from(last.size) == offset
                        && last.size.saturating_add(size) <= MAX_RUN_BYTES =>
                {
                    last.size += size
                }
                _ => track.written.push(WrittenSample {
                    offset,
                    size,
                    dts: 0,
                    pts: 0,
                    duration: None,
                    keyframe: true,
                    discard: false,
                }),
            }
            self.put(&carried)?;
            return self.put(&packet.data[..from_packet]);
        }
        let size = u32::try_from(packet.data.len())
            .map_err(|_| Error::invalid(format!("MOV muxer: stream {idx} packet too large")))?;
        let dts = packet.dts.or(packet.pts).unwrap_or(track.next_auto_dts);
        let pts = packet.pts.unwrap_or(dts);
        track.next_auto_dts = dts.saturating_add(packet.duration.unwrap_or(1).max(1));
        track.written.push(WrittenSample {
            offset,
            size,
            dts,
            pts,
            duration: packet.duration,
            keyframe: packet.flags.keyframe,
            discard: packet.flags.discard,
        });
        self.put(&packet.data)
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
        if let Some(i) = self.tracks.iter().position(|t| t.written.is_empty()) {
            return Err(Error::invalid(format!(
                "MOV muxer: stream {i} received no packets"
            )));
        }
        let mut m = MovMuxer::new().with_movie_timescale(MOVIE_TIMESCALE);
        let spans: Vec<Option<PresentedSpan>> =
            self.tracks.iter().map(PendingTrack::presented).collect();
        // The presentation starts at the earliest presented instant
        // of any stream.
        let origin = spans
            .iter()
            .flatten()
            .map(|s| s.start)
            .min_by(|a, b| a.cmp_seconds(b));
        for (t, span) in self.tracks.iter_mut().zip(&spans) {
            // The rows move into the encoder: a failed finalization is
            // terminal either way.
            let written = std::mem::take(&mut t.written);
            let n = written.len();
            let locations: Vec<ExternalSampleLocation> = written
                .iter()
                .map(|w| {
                    ExternalSampleLocation::default()
                        .with_offset(w.offset)
                        .with_size(w.size)
                })
                .collect();
            let mut samples = Vec::with_capacity(n);
            let mut prev_duration: i64 = 1;
            for (j, s) in written.iter().enumerate() {
                if let Some(frame) = t.frame_bytes {
                    samples.push(
                        MuxSample::default()
                            .with_duration(s.size / frame)
                            .with_keyframe(true),
                    );
                    continue;
                }
                // Recover a missing duration from the decode-time gap
                // to the next sample; the final sample reuses the
                // previous duration (QTFF sample durations must be
                // positive, so degrade to 1 tick at worst).
                let dur_ticks = match s.duration {
                    Some(d) if d > 0 => d,
                    _ if j + 1 < n => (written[j + 1].dts - s.dts).max(1),
                    _ => prev_duration,
                };
                prev_duration = dur_ticks;
                let scaled_dur = dur_ticks
                    .saturating_mul(t.tick_num)
                    .clamp(1, u32::MAX as i64) as u32;
                let offset = (s.pts - s.dts).saturating_mul(t.tick_num);
                let composition_offset = offset.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                samples.push(
                    MuxSample::default()
                        .with_duration(scaled_dur)
                        .with_keyframe(s.keyframe)
                        .with_composition_offset(composition_offset),
                );
            }
            let id = m.add_track(t.kind.clone(), t.media_timescale, samples, &t.extra);
            m.set_written_media(id, locations)?;
            if let Some(frame) = t.frame_bytes {
                m.set_sound_frame_size(id, frame)?;
            }
            if let Some(v2) = t.sound_v2 {
                m.set_sound_description_v2(id, v2)?;
            }
            if let (Some(span), Some(origin)) = (span, &origin) {
                let edits = span.edits(origin);
                if !edits.is_empty() {
                    m.set_edit_list(id, &edits)?;
                }
            }
            if let Some(language) = &t.language {
                match <[u8; 3]>::try_from(language.as_bytes()) {
                    Ok(code) if code.iter().all(u8::is_ascii_lowercase) => {
                        m.set_track_language(id, MovMetadata::iso_language(code))?
                    }
                    _ => m.set_track_extended_language(id, language)?,
                }
            }
            if let Some(colr) = &t.color {
                m.set_visual_extensions(
                    id,
                    VisualExtensions::default().with_colr(Some(colr.clone())),
                )?;
            }
        }
        let moov = m.encode_movie_resource()?;
        self.flush_block()?;

        // State the `mdat` size: in its 32-bit field, or — past
        // 4 GiB — in an extended header started 8 bytes earlier, over
        // the `wide` placeholder.
        let payload = self.pos - PAYLOAD_AT;
        let io = |e: std::io::Error| Error::from(e);
        if let Ok(size) = u32::try_from(payload + 8) {
            self.out
                .seek(SeekFrom::Start(self.base + WIDE_AT + 8))
                .map_err(io)?;
            self.out.write_all(&size.to_be_bytes()).map_err(io)?;
        } else {
            self.out
                .seek(SeekFrom::Start(self.base + WIDE_AT))
                .map_err(io)?;
            let mut header = Vec::with_capacity(16);
            header.extend_from_slice(&1u32.to_be_bytes());
            header.extend_from_slice(b"mdat");
            header.extend_from_slice(&(payload + 16).to_be_bytes());
            self.out.write_all(&header).map_err(io)?;
        }
        self.out
            .seek(SeekFrom::Start(self.base + self.pos))
            .map_err(io)?;
        self.out.write_all(&moov).map_err(io)?;
        self.out.flush().map_err(io)?;
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
