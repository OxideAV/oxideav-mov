//! Round-19 write side: a non-fragmented `MovMuxer` that emits a
//! structurally-valid QuickTime / ISO BMFF file.
//!
//! Layout produced (round 19):
//!
//! ```text
//! ┌─ ftyp           — major brand `qt  `, compat `qt  ` + `isom`
//! ├─ mdat           — interleaved sample bytes (one chunk per track,
//! │                   tracks emitted back-to-back in `add_track` order)
//! └─ moov
//!    ├─ mvhd        — movie header v0 (32-bit times)
//!    ├─ trak …
//!    │  ├─ tkhd
//!    │  ├─ mdia
//!    │  │  ├─ mdhd
//!    │  │  ├─ hdlr  — `mhlr` / `vide`|`soun`
//!    │  │  └─ minf
//!    │  │     ├─ vmhd (video) | smhd (audio)
//!    │  │     ├─ dinf/dref/url    — self-reference (flags=1)
//!    │  │     └─ stbl
//!    │  │        ├─ stsd
//!    │  │        ├─ stts
//!    │  │        ├─ stss          — only when at least one non-
//!    │  │        │                   keyframe sample exists
//!    │  │        ├─ stsc
//!    │  │        ├─ stsz
//!    │  │        └─ stco | co64
//!    │  └─ ⋯
//!    └─ ⋯
//! ```
//!
//! Spec citations:
//!
//! * `ftyp`               — ISO/IEC 14496-12 §4.3.
//! * `mvhd`               — ISO/IEC 14496-12 §8.2.2 / QTFF p. 33.
//! * `trak`/`tkhd`        — ISO/IEC 14496-12 §8.3.1/§8.3.2 / QTFF p. 41.
//! * `mdia`/`mdhd`/`hdlr` — ISO/IEC 14496-12 §8.4.1/§8.4.2/§8.4.3.
//! * `minf`/`vmhd`/`smhd` — ISO/IEC 14496-12 §8.4.4 / §12.1.2 / §12.2.2.
//! * `dinf`/`dref`/`url`  — ISO/IEC 14496-12 §8.7.1/§8.7.2.
//! * `stbl`               — ISO/IEC 14496-12 §8.5.
//! * `stsd`               — ISO/IEC 14496-12 §8.5.2 / QTFF p. 70.
//! * `stts` / `stss`      — ISO/IEC 14496-12 §8.6.1.2 / §8.6.2.
//! * `stsc`               — ISO/IEC 14496-12 §8.7.4 / QTFF p. 76.
//! * `stsz`               — ISO/IEC 14496-12 §8.7.3 / QTFF p. 77.
//! * `stco` / `co64`      — ISO/IEC 14496-12 §8.7.5.
//! * `mdat`               — ISO/IEC 14496-12 §8.1.1.
//!
//! Round-19 scope deliberately stops short of edit lists, composition
//! offsets (`ctts`), `mvex/trex` fragmentation, or the ProRes / HEVC /
//! Opus codec-config blobs. Callers may pass a pre-built `extra`
//! payload to a track to inject a codec-specific extension atom (e.g.
//! `avcC` for H.264 in `avc1`); the muxer copies those bytes verbatim
//! into the trailing slot of the `stsd` entry.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

use std::io::Write;

/// One sample destined for a track. The muxer copies `data` into the
/// `mdat` body and emits the matching `stsz` size + `stts` duration +
/// `stss` keyframe-flag entries.
#[derive(Clone, Debug)]
pub struct MuxSample {
    /// Raw sample bytes — one access unit (NAL, AAC frame, PCM run).
    pub data: Vec<u8>,
    /// Sample duration in the track's media timescale.
    pub duration: u32,
    /// True when this sample is a sync sample (random-access point).
    /// For audio this is conventionally true for every sample; for
    /// video it should be true on every IDR / keyframe and false on
    /// every B/P frame.
    pub keyframe: bool,
}

/// Per-track media kind dispatch — drives `hdlr.component_subtype`,
/// the `vmhd`/`smhd` choice, and the `stsd` body shape.
#[derive(Clone, Debug)]
pub enum MuxTrackKind {
    /// Video track. Emits `hdlr.component_subtype = vide`, `vmhd`, and
    /// a `stsd` whose entry carries the 70-byte video sample
    /// description with `width` / `height` populated.
    Video {
        /// Sample-description format FourCC (`avc1`, `hvc1`, `apch`,
        /// `mp4v`, …).
        format: [u8; 4],
        width: u16,
        height: u16,
    },
    /// Audio track. Emits `hdlr.component_subtype = soun`, `smhd`, and
    /// a `stsd` whose entry carries the 20-byte v0 sound sample
    /// description.
    Audio {
        format: [u8; 4],
        channels: u16,
        bits_per_sample: u16,
        sample_rate: u32,
    },
}

/// Internal per-track accumulator the muxer mutates as `add_track`
/// is called. The actual layout pass runs in [`MovMuxer::write_to`].
struct TrackWrite {
    kind: MuxTrackKind,
    /// Per-track media timescale (ticks per second). For video this
    /// is typically a frame-aligned scale (e.g. 30000 for 29.97 fps);
    /// for audio it equals the sample rate.
    media_timescale: u32,
    samples: Vec<MuxSample>,
    /// Optional codec-specific extension atom blob appended after the
    /// 70-byte (video) or 20-byte (audio) fixed body inside the
    /// matching `stsd` entry. Already framed as one or more
    /// `[size:u32 BE][type:[u8;4]][body...]` records.
    extra_stsd_atoms: Vec<u8>,
}

/// Writer-side counterpart of [`crate::demuxer::MovDemuxer`]. Builds a
/// non-fragmented MOV/MP4 carrying one or more video/audio tracks; the
/// emitted file is structurally accepted by `ffprobe -of json` and
/// round-trips back through `MovDemuxer` with the same per-track
/// sample count and per-sample sizes.
///
/// This round produces the layout `ftyp + mdat + moov` (mdat-before-
/// moov). The demuxer accepts both orderings; a follow-up round can
/// add a faststart helper that swaps `moov` to before `mdat` after
/// building the chunk-offset table.
pub struct MovMuxer {
    /// Movie-scope timescale used by `mvhd.duration` and every
    /// `tkhd.duration`. Defaults to 600 (the QTFF historical
    /// preference: divides cleanly into 24/25/30/29.97 fps) but
    /// callers can override via [`MovMuxer::with_movie_timescale`].
    movie_timescale: u32,
    tracks: Vec<TrackWrite>,
}

impl Default for MovMuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl MovMuxer {
    /// Construct an empty muxer with the default movie timescale (600).
    pub fn new() -> Self {
        Self {
            movie_timescale: 600,
            tracks: Vec::new(),
        }
    }

    /// Override the movie-scope timescale used by `mvhd.duration` and
    /// every `tkhd.duration`. Must be > 0.
    pub fn with_movie_timescale(mut self, ts: u32) -> Self {
        debug_assert!(ts > 0, "movie_timescale must be > 0");
        self.movie_timescale = ts.max(1);
        self
    }

    /// Append a track. Returns the resulting 1-based track id.
    ///
    /// `extra_stsd_atoms` is an already-framed list of codec
    /// extension atoms (e.g. one `avcC` atom for H.264). Pass `&[]`
    /// when the codec needs no extradata in `stsd`.
    pub fn add_track(
        &mut self,
        kind: MuxTrackKind,
        media_timescale: u32,
        samples: Vec<MuxSample>,
        extra_stsd_atoms: &[u8],
    ) -> u32 {
        debug_assert!(media_timescale > 0, "media_timescale must be > 0");
        self.tracks.push(TrackWrite {
            kind,
            media_timescale: media_timescale.max(1),
            samples,
            extra_stsd_atoms: extra_stsd_atoms.to_vec(),
        });
        self.tracks.len() as u32
    }

    /// Emit the file to a writer.
    ///
    /// Layout: `ftyp` (28 bytes, fixed in this round) → `mdat`
    /// (8-byte header + one chunk per track in track order) → `moov`.
    /// Returns the total bytes written.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<u64> {
        let bytes = self.encode_to_vec()?;
        w.write_all(&bytes).map_err(Error::from)?;
        Ok(bytes.len() as u64)
    }

    /// Two-pass build: first lay out the file in memory so chunk
    /// offsets are known, then return the result. Used by both
    /// `write_to` and the integration tests' in-memory roundtrip.
    pub fn encode_to_vec(&self) -> Result<Vec<u8>> {
        if self.tracks.is_empty() {
            return Err(Error::invalid("MOV muxer: at least one track required"));
        }
        for (i, t) in self.tracks.iter().enumerate() {
            if t.samples.is_empty() {
                return Err(Error::invalid(format!(
                    "MOV muxer: track {} has zero samples",
                    i + 1
                )));
            }
        }

        // ── Pass 1: predict the file layout to compute mdat chunk
        //    offsets per track. The `ftyp` is fixed at 28 bytes
        //    (8-byte header + 20-byte body: 4 major + 4 minor + 3 ×
        //    4-byte compat brands). The `mdat` header is 8 bytes when
        //    the body fits in u32, 16 bytes otherwise (size==1 +
        //    extended u64).
        let ftyp_size: u64 = 28;
        let mdat_body_len: u64 = self
            .tracks
            .iter()
            .map(|t| t.samples.iter().map(|s| s.data.len() as u64).sum::<u64>())
            .sum();
        let mdat_header_len: u64 = if mdat_body_len + 8 > u32::MAX as u64 {
            16
        } else {
            8
        };
        let mdat_payload_offset = ftyp_size + mdat_header_len;

        // Per-track chunk offset = mdat payload offset + cumulative
        // bytes of preceding tracks' samples.
        let mut chunk_offsets = Vec::with_capacity(self.tracks.len());
        let mut cursor = mdat_payload_offset;
        for t in &self.tracks {
            chunk_offsets.push(cursor);
            cursor += t.samples.iter().map(|s| s.data.len() as u64).sum::<u64>();
        }
        let need_co64 = chunk_offsets.iter().any(|&o| o > u32::MAX as u64);

        // ── Pass 2: emit bytes.
        let mut out = Vec::with_capacity((cursor + 4096) as usize);
        out.extend_from_slice(&build_ftyp());
        emit_mdat_header(&mut out, mdat_body_len);
        for t in &self.tracks {
            for s in &t.samples {
                out.extend_from_slice(&s.data);
            }
        }
        let moov = build_moov(self, &chunk_offsets, need_co64);
        push_atom(&mut out, *b"moov", &moov);
        Ok(out)
    }
}

// ─────────────────────────── encoders ───────────────────────────

fn build_ftyp() -> Vec<u8> {
    // Body: major(4) + minor(4) + compatible_brands.
    // We pick `qt  ` as major and list `qt  ` + `isom` + `mp42` for
    // broad downstream tooling acceptance. Total body = 4+4+12 = 20
    // bytes ⇒ atom size = 8+20 = 28 bytes.
    let mut body = Vec::with_capacity(20);
    body.extend_from_slice(b"qt  ");
    body.extend_from_slice(&0x0000_0200u32.to_be_bytes()); // minor 0x200 (Apple convention)
    body.extend_from_slice(b"qt  ");
    body.extend_from_slice(b"isom");
    body.extend_from_slice(b"mp42");
    let mut out = Vec::with_capacity(8 + body.len());
    push_atom(&mut out, *b"ftyp", &body);
    out
}

/// Emit the `mdat` header into `out`, big enough for `body_len`. Uses
/// the 16-byte extended header when `8 + body_len > u32::MAX`.
fn emit_mdat_header(out: &mut Vec<u8>, body_len: u64) {
    let total = 8u64 + body_len;
    if total > u32::MAX as u64 {
        // Extended size form: size32 = 1, then 64-bit size.
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(b"mdat");
        let extended = 16u64 + body_len;
        out.extend_from_slice(&extended.to_be_bytes());
    } else {
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(b"mdat");
    }
}

fn build_moov(m: &MovMuxer, chunk_offsets: &[u64], need_co64: bool) -> Vec<u8> {
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(m));
    for (idx, t) in m.tracks.iter().enumerate() {
        let trak = build_trak(
            t,
            (idx as u32) + 1,
            m.movie_timescale,
            chunk_offsets[idx],
            need_co64,
        );
        push_atom(&mut moov, *b"trak", &trak);
    }
    moov
}

fn build_mvhd(m: &MovMuxer) -> Vec<u8> {
    // ISO/IEC 14496-12 §8.2.2 — version 0 (32-bit times). 100 bytes
    // payload. Layout offsets cited from QTFF p. 33.
    let mut p = vec![0u8; 100];
    // version = 0, flags = 0 already in p[0..4]
    // creation_time @ 4..8, modification_time @ 8..12 left zero.
    p[12..16].copy_from_slice(&m.movie_timescale.to_be_bytes());
    let total_dur = total_duration_in_movie_ts(m);
    let dur32 = total_dur.min(u32::MAX as u64) as u32;
    p[16..20].copy_from_slice(&dur32.to_be_bytes());
    // rate @ 20..24 = 1.0
    p[20..24].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    // volume @ 24..26 = 1.0
    p[24..26].copy_from_slice(&0x0100i16.to_be_bytes());
    // 10 bytes reserved @ 26..36 left zero.
    // Identity 36-byte matrix @ 36..72 (a=1.0, d=1.0, w=1.0).
    p[36..40].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // a
    p[52..56].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // d
    p[68..72].copy_from_slice(&0x4000_0000u32.to_be_bytes()); // w (2.30)
                                                              // 24 bytes pre-defined @ 72..96 left zero (preview/poster/sel/cur).
    p[96..100].copy_from_slice(&((m.tracks.len() as u32) + 1).to_be_bytes());
    p
}

/// Total movie-scope duration: max over per-track movie-scope
/// durations. Per-track movie duration = sum of media durations
/// rescaled into movie timescale.
fn total_duration_in_movie_ts(m: &MovMuxer) -> u64 {
    m.tracks
        .iter()
        .map(|t| track_movie_duration(t, m.movie_timescale))
        .max()
        .unwrap_or(0)
}

fn track_media_duration(t: &TrackWrite) -> u64 {
    t.samples.iter().map(|s| s.duration as u64).sum()
}

fn track_movie_duration(t: &TrackWrite, movie_ts: u32) -> u64 {
    let media_dur = track_media_duration(t);
    if t.media_timescale == 0 {
        return 0;
    }
    // Round-half-up rescale. movie_ts and media_timescale are u32 so
    // the multiplication fits in u128 without overflow.
    let num = (media_dur as u128) * (movie_ts as u128);
    let den = t.media_timescale as u128;
    ((num + den / 2) / den) as u64
}

fn build_trak(
    t: &TrackWrite,
    track_id: u32,
    movie_ts: u32,
    chunk_offset: u64,
    need_co64: bool,
) -> Vec<u8> {
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(t, track_id, movie_ts));
    push_atom(&mut trak, *b"mdia", &build_mdia(t, chunk_offset, need_co64));
    trak
}

fn build_tkhd(t: &TrackWrite, track_id: u32, movie_ts: u32) -> Vec<u8> {
    // ISO/IEC 14496-12 §8.3.2 — version 0 (32-bit times). 84 bytes
    // payload. Offsets cited from QTFF p. 41.
    let mut p = vec![0u8; 84];
    // version=0; flags = enabled(1) + in_movie(2) + in_preview(4) = 7.
    p[3] = 0x07;
    // creation_time @ 4..8, modification_time @ 8..12 left zero.
    p[12..16].copy_from_slice(&track_id.to_be_bytes());
    // 4 bytes reserved @ 16..20.
    let dur = track_movie_duration(t, movie_ts).min(u32::MAX as u64) as u32;
    p[20..24].copy_from_slice(&dur.to_be_bytes());
    // 8 bytes reserved @ 24..32.
    // layer @ 32..34 = 0, alternate_group @ 34..36 = 0.
    // volume @ 36..38: 1.0 for audio tracks, 0 for visual per spec.
    if matches!(t.kind, MuxTrackKind::Audio { .. }) {
        p[36..38].copy_from_slice(&0x0100i16.to_be_bytes());
    }
    // 2 bytes reserved @ 38..40.
    // Identity 9-element matrix @ 40..76 (a=1.0, d=1.0, w=1.0).
    p[40..44].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // a
    p[56..60].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // d
    p[72..76].copy_from_slice(&0x4000_0000u32.to_be_bytes()); // w (2.30)
                                                              // width / height @ 76..84: 16.16 fixed-point, in pixels for video,
                                                              // zero for audio.
    let (w_fp, h_fp) = match &t.kind {
        MuxTrackKind::Video { width, height, .. } => {
            ((*width as u32) << 16, (*height as u32) << 16)
        }
        MuxTrackKind::Audio { .. } => (0, 0),
    };
    p[76..80].copy_from_slice(&w_fp.to_be_bytes());
    p[80..84].copy_from_slice(&h_fp.to_be_bytes());
    p
}

fn build_mdia(t: &TrackWrite, chunk_offset: u64, need_co64: bool) -> Vec<u8> {
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(t));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(t));
    push_atom(&mut mdia, *b"minf", &build_minf(t, chunk_offset, need_co64));
    mdia
}

fn build_mdhd(t: &TrackWrite) -> Vec<u8> {
    // ISO/IEC 14496-12 §8.4.2 — version 0. 24 bytes payload.
    let mut p = vec![0u8; 24];
    p[12..16].copy_from_slice(&t.media_timescale.to_be_bytes());
    let dur = track_media_duration(t).min(u32::MAX as u64) as u32;
    p[16..20].copy_from_slice(&dur.to_be_bytes());
    // language @ 20..22 = 0x55C4 (= 0b10101 01110 00100 = "und",
    // QTFF p. 197 / ISO BMFF §8.4.2.3: ASCII "und" packed five-bit
    // chars + 0x60 base ⇒ ('u'-0x60)=0x15, ('n'-0x60)=0xE, ('d'-
    // 0x60)=0x4 ⇒ 0b0_10101_01110_00100 = 0x55C4).
    p[20..22].copy_from_slice(&0x55C4u16.to_be_bytes());
    // quality @ 22..24 = 0.
    p
}

fn build_hdlr(t: &TrackWrite) -> Vec<u8> {
    // ISO/IEC 14496-12 §8.4.3 / QTFF p. 57.
    // [ver+flags:4][component_type:4][component_subtype:4]
    //   [component_manufacturer:4][component_flags:4]
    //   [component_flags_mask:4][counted-Pascal name].
    let subtype: &[u8; 4] = match &t.kind {
        MuxTrackKind::Video { .. } => b"vide",
        MuxTrackKind::Audio { .. } => b"soun",
    };
    let mut p = Vec::with_capacity(25);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(b"mhlr");
    p.extend_from_slice(subtype);
    p.extend_from_slice(&[0u8; 12]); // manuf + flags + flags_mask
    p.push(0); // counted-Pascal name length 0 (BMFF readers also accept
               // this as a NUL-terminated empty UTF-8 string)
    p
}

fn build_minf(t: &TrackWrite, chunk_offset: u64, need_co64: bool) -> Vec<u8> {
    let mut minf = Vec::new();
    match &t.kind {
        MuxTrackKind::Video { .. } => push_atom(&mut minf, *b"vmhd", &build_vmhd()),
        MuxTrackKind::Audio { .. } => push_atom(&mut minf, *b"smhd", &build_smhd()),
    }
    push_atom(&mut minf, *b"dinf", &build_dinf());
    push_atom(&mut minf, *b"stbl", &build_stbl(t, chunk_offset, need_co64));
    minf
}

fn build_vmhd() -> Vec<u8> {
    // ISO/IEC 14496-12 §12.1.2: ver=0, flags=1 (no-lean-ahead per QTFF).
    // 12 bytes payload: ver+flags(4) + graphicsmode(2) + opcolor(3×2).
    let mut p = vec![0u8; 12];
    p[3] = 0x01;
    p
}

fn build_smhd() -> Vec<u8> {
    // ISO/IEC 14496-12 §12.2.2. 8 bytes payload: ver+flags(4) +
    // balance(2) + reserved(2). balance = 0 (centre).
    vec![0u8; 8]
}

fn build_dinf() -> Vec<u8> {
    // ISO/IEC 14496-12 §8.7.1 — `dinf` wraps a single `dref`.
    let mut dref = Vec::new();
    // dref body: ver+flags(4) + entry_count(4) + N × entries.
    dref.extend_from_slice(&0u32.to_be_bytes());
    dref.extend_from_slice(&1u32.to_be_bytes()); // 1 entry
                                                 // One self-reference `url ` entry with flags=1 (data is in this file).
    let mut url_body = Vec::with_capacity(4);
    url_body.extend_from_slice(&0x0000_0001u32.to_be_bytes());
    push_atom(&mut dref, *b"url ", &url_body);
    let mut dinf = Vec::new();
    push_atom(&mut dinf, *b"dref", &dref);
    dinf
}

fn build_stbl(t: &TrackWrite, chunk_offset: u64, need_co64: bool) -> Vec<u8> {
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &build_stsd(t));
    push_atom(&mut stbl, *b"stts", &build_stts(t));
    if let Some(stss_atom) = build_stss(t) {
        push_atom(&mut stbl, *b"stss", &stss_atom);
    }
    push_atom(&mut stbl, *b"stsc", &build_stsc(t));
    push_atom(&mut stbl, *b"stsz", &build_stsz(t));
    if need_co64 {
        push_atom(&mut stbl, *b"co64", &build_co64(chunk_offset));
    } else {
        push_atom(&mut stbl, *b"stco", &build_stco(chunk_offset as u32));
    }
    stbl
}

fn build_stsd(t: &TrackWrite) -> Vec<u8> {
    // ISO/IEC 14496-12 §8.5.2 / QTFF p. 70.
    // [ver+flags:4][entry_count:4]([size:4][format:4][rsrv:6]
    //     [data_reference_index:2][per-mediatype body][optional extra atoms])+
    let entry_body = match &t.kind {
        MuxTrackKind::Video {
            format,
            width,
            height,
        } => {
            let mut e = Vec::with_capacity(16 + 70 + t.extra_stsd_atoms.len());
            // Universal 16-byte prefix is added by `wrap_stsd_entry`.
            // Video sample description body (70 bytes, QTFF p. 92):
            //   ver:2 rev:2 vendor:4 temp_q:4 spatial_q:4
            //   width:2 height:2 hres:4 vres:4 data_size:4 frame_count:2
            //   compressor_name:32 depth:2 color_table_id:2
            let mut body = vec![0u8; 70];
            // hres @ 16..20 = 72.0 (16.16 = 0x00480000)
            body[16..20].copy_from_slice(&0x0048_0000u32.to_be_bytes());
            // vres @ 20..24 = 72.0
            body[20..24].copy_from_slice(&0x0048_0000u32.to_be_bytes());
            // frame_count @ 28..30 = 1
            body[28..30].copy_from_slice(&1u16.to_be_bytes());
            // depth @ 64..66 = 24 (typical for non-alpha video)
            body[64..66].copy_from_slice(&24u16.to_be_bytes());
            // color_table_id @ 66..68 = -1 (no color table)
            body[66..68].copy_from_slice(&(-1i16).to_be_bytes());
            body[24..26].copy_from_slice(&width.to_be_bytes());
            body[26..28].copy_from_slice(&height.to_be_bytes());
            e.extend_from_slice(&body);
            e.extend_from_slice(&t.extra_stsd_atoms);
            wrap_stsd_entry(format, &e)
        }
        MuxTrackKind::Audio {
            format,
            channels,
            bits_per_sample,
            sample_rate,
        } => {
            let mut e = Vec::with_capacity(16 + 20 + t.extra_stsd_atoms.len());
            let mut body = vec![0u8; 20];
            // version=0, revision=0, vendor=0 left zero.
            body[8..10].copy_from_slice(&channels.to_be_bytes());
            body[10..12].copy_from_slice(&bits_per_sample.to_be_bytes());
            // compression_id @ 12..14 = 0; packet_size @ 14..16 = 0.
            // sample_rate @ 16..20 — 16.16 fixed; QTFF caps the integer
            // portion at u16, so cap the rate to 65535 Hz when needed.
            let sr = (*sample_rate).min(0xFFFF);
            body[16..20].copy_from_slice(&(sr << 16).to_be_bytes());
            e.extend_from_slice(&body);
            e.extend_from_slice(&t.extra_stsd_atoms);
            wrap_stsd_entry(format, &e)
        }
    };
    let mut stsd = Vec::with_capacity(8 + entry_body.len());
    stsd.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    stsd.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    stsd.extend_from_slice(&entry_body);
    stsd
}

/// Wrap a per-mediatype body in the universal 16-byte stsd entry
/// header: `[size:4][format:4][reserved:6][data_reference_index:2]`.
fn wrap_stsd_entry(format: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let entry_size: u32 = (16 + body.len()) as u32;
    let mut out = Vec::with_capacity(entry_size as usize);
    out.extend_from_slice(&entry_size.to_be_bytes());
    out.extend_from_slice(format);
    out.extend_from_slice(&[0u8; 6]); // 6 bytes reserved
    out.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index = 1
    out.extend_from_slice(body);
    out
}

fn build_stts(t: &TrackWrite) -> Vec<u8> {
    // Run-length-encode consecutive samples with the same duration.
    // ISO/IEC 14496-12 §8.6.1.2.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for s in &t.samples {
        match runs.last_mut() {
            Some(last) if last.1 == s.duration => last.0 += 1,
            _ => runs.push((1, s.duration)),
        }
    }
    let mut p = Vec::with_capacity(8 + runs.len() * 8);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, dur) in runs {
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&dur.to_be_bytes());
    }
    p
}

/// Returns `Some(payload)` when at least one sample is *not* a
/// keyframe (so the implicit "every sample is a keyframe" rule needs
/// to be replaced by an explicit `stss`); `None` otherwise.
fn build_stss(t: &TrackWrite) -> Option<Vec<u8>> {
    let any_non_kf = t.samples.iter().any(|s| !s.keyframe);
    if !any_non_kf {
        return None;
    }
    let kf_indices: Vec<u32> = t
        .samples
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            if s.keyframe {
                Some((i as u32) + 1)
            } else {
                None
            }
        })
        .collect();
    let mut p = Vec::with_capacity(8 + kf_indices.len() * 4);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&(kf_indices.len() as u32).to_be_bytes());
    for k in kf_indices {
        p.extend_from_slice(&k.to_be_bytes());
    }
    Some(p)
}

fn build_stsc(t: &TrackWrite) -> Vec<u8> {
    // Single-chunk-per-track layout: one stsc entry covering all
    // samples in chunk 1.
    let mut p = Vec::with_capacity(8 + 12);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    p.extend_from_slice(&1u32.to_be_bytes()); // first_chunk
    p.extend_from_slice(&(t.samples.len() as u32).to_be_bytes()); // samples_per_chunk
    p.extend_from_slice(&1u32.to_be_bytes()); // sample_description_id
    p
}

fn build_stsz(t: &TrackWrite) -> Vec<u8> {
    // Uniform sample size if every sample is the same length;
    // per-sample table otherwise.
    let first = t.samples[0].data.len() as u32;
    let uniform = t.samples.iter().all(|s| (s.data.len() as u32) == first);
    let count = t.samples.len() as u32;
    if uniform {
        let mut p = Vec::with_capacity(12);
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&first.to_be_bytes()); // sample_size
        p.extend_from_slice(&count.to_be_bytes());
        p
    } else {
        let mut p = Vec::with_capacity(12 + (count as usize) * 4);
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0 ⇒ table follows
        p.extend_from_slice(&count.to_be_bytes());
        for s in &t.samples {
            p.extend_from_slice(&(s.data.len() as u32).to_be_bytes());
        }
        p
    }
}

fn build_stco(chunk_offset: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(12);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    p.extend_from_slice(&chunk_offset.to_be_bytes());
    p
}

fn build_co64(chunk_offset: u64) -> Vec<u8> {
    let mut p = Vec::with_capacity(16);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    p.extend_from_slice(&chunk_offset.to_be_bytes());
    p
}

fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
    let size: u32 = (8 + body.len()) as u32;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&fourcc);
    out.extend_from_slice(body);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_video_samples(n: usize) -> Vec<MuxSample> {
        (0..n)
            .map(|i| MuxSample {
                data: vec![(i & 0xFF) as u8; 16 + (i % 8)],
                duration: 1000,
                keyframe: i % 5 == 0,
            })
            .collect()
    }

    #[test]
    fn ftyp_size_is_28_bytes_with_qt_major() {
        let v = build_ftyp();
        assert_eq!(v.len(), 28);
        assert_eq!(&v[4..8], b"ftyp");
        assert_eq!(&v[8..12], b"qt  ");
    }

    #[test]
    fn empty_muxer_is_an_error() {
        let m = MovMuxer::new();
        assert!(m.encode_to_vec().is_err());
    }

    #[test]
    fn track_with_zero_samples_is_an_error() {
        let mut m = MovMuxer::new();
        m.add_track(
            MuxTrackKind::Video {
                format: *b"avc1",
                width: 320,
                height: 240,
            },
            30000,
            Vec::new(),
            &[],
        );
        assert!(m.encode_to_vec().is_err());
    }

    #[test]
    fn stts_runlength_encodes_uniform_durations() {
        let t = TrackWrite {
            kind: MuxTrackKind::Video {
                format: *b"avc1",
                width: 8,
                height: 8,
            },
            media_timescale: 1000,
            samples: vec![
                MuxSample {
                    data: vec![0; 4],
                    duration: 33,
                    keyframe: true,
                },
                MuxSample {
                    data: vec![0; 4],
                    duration: 33,
                    keyframe: false,
                },
                MuxSample {
                    data: vec![0; 4],
                    duration: 33,
                    keyframe: false,
                },
            ],
            extra_stsd_atoms: Vec::new(),
        };
        let stts = build_stts(&t);
        // ver+flags(4) | entry_count=1(4) | run: count=3, duration=33 (8) = 16 bytes total.
        let n = u32::from_be_bytes([stts[4], stts[5], stts[6], stts[7]]);
        assert_eq!(n, 1);
        let count = u32::from_be_bytes([stts[8], stts[9], stts[10], stts[11]]);
        let dur = u32::from_be_bytes([stts[12], stts[13], stts[14], stts[15]]);
        assert_eq!(count, 3);
        assert_eq!(dur, 33);
        assert_eq!(stts.len(), 8 + 8);
    }

    #[test]
    fn stss_omitted_when_all_keyframes() {
        let t = TrackWrite {
            kind: MuxTrackKind::Audio {
                format: *b"sowt",
                channels: 2,
                bits_per_sample: 16,
                sample_rate: 44100,
            },
            media_timescale: 44100,
            samples: vec![
                MuxSample {
                    data: vec![0; 1024],
                    duration: 256,
                    keyframe: true,
                },
                MuxSample {
                    data: vec![0; 1024],
                    duration: 256,
                    keyframe: true,
                },
            ],
            extra_stsd_atoms: Vec::new(),
        };
        assert!(build_stss(&t).is_none());
    }

    #[test]
    fn stss_emitted_when_any_non_keyframe_present() {
        let t = TrackWrite {
            kind: MuxTrackKind::Video {
                format: *b"avc1",
                width: 8,
                height: 8,
            },
            media_timescale: 1000,
            samples: synth_video_samples(11),
            extra_stsd_atoms: Vec::new(),
        };
        // synth_video_samples marks i % 5 == 0 as keyframes ⇒ 0, 5, 10
        let body = build_stss(&t).expect("stss should be emitted");
        let n = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
        assert_eq!(n, 3);
        // 1-based indices 1, 6, 11.
        let kf1 = u32::from_be_bytes([body[8], body[9], body[10], body[11]]);
        assert_eq!(kf1, 1);
        let kf3 = u32::from_be_bytes([body[16], body[17], body[18], body[19]]);
        assert_eq!(kf3, 11);
    }

    #[test]
    fn stsz_uniform_when_all_samples_same_size() {
        let t = TrackWrite {
            kind: MuxTrackKind::Audio {
                format: *b"sowt",
                channels: 1,
                bits_per_sample: 16,
                sample_rate: 8000,
            },
            media_timescale: 8000,
            samples: vec![
                MuxSample {
                    data: vec![0; 64],
                    duration: 32,
                    keyframe: true,
                },
                MuxSample {
                    data: vec![0; 64],
                    duration: 32,
                    keyframe: true,
                },
                MuxSample {
                    data: vec![0; 64],
                    duration: 32,
                    keyframe: true,
                },
            ],
            extra_stsd_atoms: Vec::new(),
        };
        let stsz = build_stsz(&t);
        assert_eq!(stsz.len(), 12);
        assert_eq!(u32::from_be_bytes([stsz[4], stsz[5], stsz[6], stsz[7]]), 64);
        assert_eq!(
            u32::from_be_bytes([stsz[8], stsz[9], stsz[10], stsz[11]]),
            3
        );
    }

    #[test]
    fn stsz_per_sample_when_sizes_vary() {
        let t = TrackWrite {
            kind: MuxTrackKind::Video {
                format: *b"avc1",
                width: 8,
                height: 8,
            },
            media_timescale: 1000,
            samples: vec![
                MuxSample {
                    data: vec![0; 16],
                    duration: 33,
                    keyframe: true,
                },
                MuxSample {
                    data: vec![0; 17],
                    duration: 33,
                    keyframe: false,
                },
            ],
            extra_stsd_atoms: Vec::new(),
        };
        let stsz = build_stsz(&t);
        // sample_size = 0 → table follows
        assert_eq!(u32::from_be_bytes([stsz[4], stsz[5], stsz[6], stsz[7]]), 0);
        assert_eq!(
            u32::from_be_bytes([stsz[8], stsz[9], stsz[10], stsz[11]]),
            2
        );
        assert_eq!(stsz.len(), 12 + 2 * 4);
    }
}
