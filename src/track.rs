//! Per-track aggregation: the `tkhd` + `mdhd` + `hdlr` + sample
//! description + sample table for a single QTFF track.
//!
//! The `stsd` (sample description) atom is parsed just enough to
//! pull out the data-format FourCC of its first entry — that is
//! what gets handed to `oxideav_core::CodecResolver` to map to a
//! `CodecId`. Per-codec config blobs (e.g. `avcC`/`hvcC`/`esds`/
//! Apple's `wave` audio extension) are captured as raw bytes in
//! [`SampleDescription::extra`] for downstream codec crates.

use crate::bmff_meta::BmffMeta;
use crate::clip::Clipping;
use crate::edit::{
    media_pts_to_movie_pts, movie_pts_to_media_pts, resolve_edit_segments, EditList, EditSegment,
};
use crate::gmhd::Gmhd;
use crate::header::{Hdlr, Hmhd, Mdhd, MediaHeaderKind, Tkhd};
use crate::kind::KindEntry;
use crate::matte::Matte;
use crate::media_meta::{
    parse_chan, parse_clap, parse_colr, parse_fiel, parse_mjht, parse_mjqt, parse_pasp, Chan, Clap,
    ColorParameters, Cslg, Fiel, MetaKeyValue, Mjht, Mjqt, Pasp, Tapt,
};
use crate::metadata_sample::{
    parse_metadata_sample_entry, parse_stxt, parse_subtitle_sample_entry, MetadataSampleEntry,
    SimpleTextSampleEntry, SubtitleSampleEntry,
};
use crate::reference::DataReference;
use crate::sample_table::{SampleEntry, SampleTable};
use crate::text_sample::{
    parse_text_sample_description, TextSampleDescription, TEXT_SAMPLE_DESC_FIXED_LEN,
};
use crate::timecode::{parse_tmcd_sample_description, Tmcd};
use crate::track_load::Load;
use crate::track_selection::TrackSelection;
use crate::user_data::UserDataEntry;

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Track-reference relationship (`tref` child). Round-2 surfaces the
/// reference type plus the related-track-id list; later rounds may
/// resolve them to actual `Track` references on the demuxer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackRef {
    /// FourCC of the reference type (e.g. `chap`, `tmcd`, `scpt`,
    /// `ssrc`, `sync`, `hint`, `mpod`).
    pub kind: TrackRefKind,
    /// The 4-byte FourCC as bytes (kept for unknown reference types).
    pub fourcc: [u8; 4],
    /// Related track ids (1-based; 0 is permitted per QTFF p. 51).
    pub track_ids: Vec<u32>,
}

/// High-level discriminator for [`TrackRef::kind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackRefKind {
    /// `chap` — chapter list (typically references a text track).
    Chapter,
    /// `tmcd` — time code track.
    Timecode,
    /// `scpt` — transcript / script.
    Transcript,
    /// `ssrc` — non-primary source for an `imap`.
    NonPrimarySource,
    /// `sync` — sync between tracks.
    Sync,
    /// `hint` — hint-track source media (RTP).
    Hint,
    /// `mpod` — MPEG-DASH / MPEG-4 OD reference.
    Mpod,
    /// `cdsc` — Content Describes (ISO/IEC 14496-12 §8.3.3.3). This
    /// (descriptive / metadata) track describes the referenced content
    /// track; e.g. a timed-metadata track links to the media it
    /// annotates (§12.3.2), and an RTCP reception hint track links to
    /// its RTP reception hint track.
    ContentDescribes,
    /// `font` — this track uses fonts carried/defined in the referenced
    /// track (ISO/IEC 14496-12 §8.3.3.3).
    Font,
    /// `hind` — this track depends on the referenced hint track and
    /// should only be used if that hint track is used (§8.3.3.3); the
    /// referenced tracks shall themselves be hint tracks.
    HintDependency,
    /// `vdep` — this track carries auxiliary depth video information for
    /// the referenced video track (§8.3.3.3).
    VideoDepth,
    /// `vplx` — this track carries auxiliary parallax video information
    /// for the referenced video track (§8.3.3.3).
    VideoParallax,
    /// `subt` — this track carries subtitle / timed-text / overlay
    /// graphical information for the referenced track or its alternate
    /// group (§8.3.3.3).
    Subtitle,
    /// Anything else (vendor-specific / from a derived specification).
    Other,
}

impl TrackRefKind {
    pub fn from_fourcc(f: &[u8; 4]) -> Self {
        match f {
            b"chap" => Self::Chapter,
            b"tmcd" => Self::Timecode,
            b"scpt" => Self::Transcript,
            b"ssrc" => Self::NonPrimarySource,
            b"sync" => Self::Sync,
            b"hint" => Self::Hint,
            b"mpod" => Self::Mpod,
            b"cdsc" => Self::ContentDescribes,
            b"font" => Self::Font,
            b"hind" => Self::HintDependency,
            b"vdep" => Self::VideoDepth,
            b"vplx" => Self::VideoParallax,
            b"subt" => Self::Subtitle,
            _ => Self::Other,
        }
    }
}

/// The four fixed-compression-ratio fields a Sound Sample Description
/// **version 1** appends after the version-0 fixed fields (QTFF p. 101,
/// `SoundDescriptionV1`). All four are 32-bit big-endian unsigned
/// integers; each is `0` when "not used" (a reader detects the
/// not-used case by `samples_per_packet == 0`, per QTFF p. 101).
///
/// The fields are taken directly from the Sound Manager
/// `CompressionInfo` structure and describe the fixed compression ratio
/// of constant-bit-rate audio codecs:
///
/// * `samples_per_packet` — uncompressed samples in one packet.
/// * `bytes_per_packet` — resulting compressed bytes for **one** channel.
/// * `bytes_per_frame` — compressed bytes for **all** channels
///   (`channels * bytes_per_packet`).
/// * `bytes_per_sample` — size of one uncompressed sample.
///
/// For the VBR third variant (QTFF p. 102, `compression_id == -2`) only
/// `samples_per_packet` and `bytes_per_sample` are meaningful; the other
/// two are reserved and set to `0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoundV1 {
    /// Number of uncompressed samples in a packet.
    pub samples_per_packet: u32,
    /// Compressed bytes for one channel.
    pub bytes_per_packet: u32,
    /// Compressed bytes for all channels (`channels * bytes_per_packet`).
    pub bytes_per_frame: u32,
    /// Size of one uncompressed sample.
    pub bytes_per_sample: u32,
}

/// ISO BMFF `ChannelLayout` box (`chnl`, ISO/IEC 14496-12:2015
/// §12.2.4) — documents the assignment of channels (and/or audio
/// objects) in the stream. Carried inside an audio sample entry;
/// signalling more than 2 channels requires an `AudioSampleEntryV1`
/// (§12.2.4.1). A stream may be channel-structured, object-structured,
/// both, or neither (`stream_structure` flags 1 / 2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelLayout {
    /// `stream_structure` flag field: bit 0 (`1`) = the stream
    /// carries channels, bit 1 (`2`) = the stream carries objects.
    /// All other bits are reserved (§12.2.4.3).
    pub stream_structure: u8,
    /// Channel-structure payload, present when `stream_structure & 1`.
    pub channels: Option<ChannelStructure>,
    /// Object count, present when `stream_structure & 2`.
    pub object_count: Option<u8>,
}

impl ChannelLayout {
    /// §12.2.4.2 `channelStructured` flag.
    pub const CHANNEL_STRUCTURED: u8 = 1;
    /// §12.2.4.2 `objectStructured` flag.
    pub const OBJECT_STRUCTURED: u8 = 2;
}

/// The channel-structured half of a [`ChannelLayout`] (§12.2.4.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelStructure {
    /// `definedLayout != 0`: a ChannelConfiguration from ISO/IEC
    /// 23001-8 plus a 64-bit bitmap of channels omitted from this
    /// track (1-bit = absent; `0` = the standard layout is fully
    /// present, §12.2.4.3).
    Defined {
        /// ISO/IEC 23001-8 ChannelConfiguration index.
        defined_layout: u8,
        /// LSB-first omitted-channels bitmap.
        omitted_channels_map: u64,
    },
    /// `definedLayout == 0`: one explicit speaker position per channel
    /// (the count comes from the sample entry's `channelcount`).
    Explicit(Vec<SpeakerPosition>),
}

/// One explicit speaker position row of a [`ChannelStructure::Explicit`]
/// layout (§12.2.4.2): an ISO/IEC 23001-8 OutputChannelPosition, with
/// `speaker_position == 126` carrying an explicit azimuth/elevation
/// pair (degrees, as defined for LoudspeakerAzimuth / -Elevation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpeakerPosition {
    /// OutputChannelPosition code; `126` = explicit position.
    pub speaker_position: u8,
    /// Azimuth in degrees, only for `speaker_position == 126`.
    pub azimuth: Option<i16>,
    /// Elevation in degrees, only for `speaker_position == 126`.
    pub elevation: Option<i8>,
}

/// One sample-description-table entry. QTFF p. 70 ("Sample
/// Description Atoms") — the first 16 bytes are universal:
/// `[size:4][format:4][reserved:6][data_reference_index:2]`. Per-
/// media-type fields follow (Video Sample Description: pp. 92–94,
/// Sound Sample Description: pp. 100–102) and are kept here as
/// raw bytes plus parsed dims/sample-rate when we recognise the
/// media type.
#[derive(Clone, Debug, Default)]
pub struct SampleDescription {
    pub format: [u8; 4],
    pub data_reference_index: u16,
    /// Width in pixels (video sample descriptions only).
    pub width: u16,
    /// Height in pixels (video sample descriptions only).
    pub height: u16,
    /// Audio: number of channels (1 = mono, 2 = stereo).
    pub channels: u16,
    /// Audio: bits per sample.
    pub bits_per_sample: u16,
    /// Audio: sample rate (16.16 fixed-point, integer portion in
    /// QTFF v0; matches `mdhd.time_scale` per QTFF p. 100 last
    /// paragraph).
    pub sample_rate: u32,
    /// Codec-specific blob that follows the sample-description
    /// fixed fields (everything after byte 86 for video, after byte
    /// 36 for audio v0). Suitable for handing as extradata to a
    /// codec.
    pub extra: Vec<u8>,

    // ─────── Round-2 video extension atoms ───────
    /// `gama` — gamma 16.16 fixed-point (QTFF p. 94, Table 3-2:
    /// "32-bit fixed-point number"). `None` when absent. The raw
    /// word is preserved verbatim; see [`SampleDescription::gamma_value`]
    /// for the typed 16.16 → `f64` accessor.
    pub gamma: Option<u32>,
    /// `pasp` — pixel aspect ratio.
    pub pasp: Option<Pasp>,
    /// `clap` — clean aperture.
    pub clap: Option<Clap>,
    /// `colr` — colour parameters (Apple `nclc` or ISO `nclx`).
    pub colr: Option<ColorParameters>,
    /// `fiel` — Field Handling (QTFF p. 94, Table 3-2). Surfaces
    /// the field count + ordering; `None` when the sample
    /// description carries no `fiel` extension (the implicit
    /// "progressive" case). QuickTime-only; ISO BMFF samples
    /// arriving via this decoder will not set this field.
    pub fiel: Option<Fiel>,
    /// `mjqt` — default Motion-JPEG quantization table (QTFF p. 94,
    /// Table 3-2). Surfaces the raw `DQT` data a Motion-JPEG field
    /// defers to when its own quantization-table offset is `0` (QTFF
    /// p. 95 / p. 96); `None` when the sample description carries no
    /// `mjqt` extension. QuickTime-only — ISO BMFF samples arriving
    /// via this decoder will not set this field.
    pub mjqt: Option<Mjqt>,
    /// `mjht` — default Motion-JPEG Huffman table (QTFF p. 94,
    /// Table 3-2). Surfaces the raw `DHT` data a Motion-JPEG field
    /// defers to when its own Huffman-table offset is `0` (QTFF
    /// p. 95 / p. 96); `None` when the sample description carries no
    /// `mjht` extension. QuickTime-only — ISO BMFF samples arriving
    /// via this decoder will not set this field.
    pub mjht: Option<Mjht>,

    // ─────── Round-2 audio extension atoms ───────
    /// `chan` — Apple Core Audio channel layout (raw fields surfaced).
    pub chan: Option<Chan>,

    // ─────── Round-325 sound sample description version ───────
    /// Sound Sample Description format version (QTFF p. 100). `0` for
    /// the classic uncompressed-sample layout, `1` for the QuickTime-3
    /// extension carrying the fixed-ratio [`SoundV1`] fields. Populated
    /// only for audio sample descriptions; left at the `0` default for
    /// video / timecode / other handlers.
    pub audio_version: u16,
    /// Sound Sample Description Compression ID (QTFF p. 100). Normally
    /// `0`; a value of `-2` (surfaced here as the signed reinterpretation
    /// of the on-wire `0xFFFE`) flags the VBR "third variant" (QTFF
    /// p. 102): the sample table then documents *compressed frames*, not
    /// uncompressed samples. [`SampleDescription::is_vbr`] decodes this.
    pub audio_compression_id: i16,
    /// `samples_per_packet` / `bytes_per_packet` / `bytes_per_frame` /
    /// `bytes_per_sample` from a Sound Sample Description **version 1**
    /// (QTFF p. 101, `SoundDescriptionV1`). `None` for version-0
    /// descriptions and for non-audio handlers. These let a reader work
    /// out the fixed compression ratio (or, for VBR, the constant
    /// samples-per-packet / bytes-per-sample) without instantiating the
    /// decompressor.
    pub sound_v1: Option<SoundV1>,

    // ─────── Round-394 ISO audio sample entry v1 ───────
    /// `true` when this audio entry is an ISO BMFF
    /// `AudioSampleEntryV1` (ISO/IEC 14496-12:2015 §12.2.3):
    /// `entry_version == 1` inside a `stsd` whose FullBox version is
    /// also 1. The fixed 20-byte body layout matches version 0 (the
    /// QTFF 16-byte `SoundDescriptionV1` extension is *not* present);
    /// optional `srat` / `chnl` boxes may follow. Distinguished from
    /// the QTFF version-1 sound description (which lives in a
    /// version-0 `stsd` and sets [`SampleDescription::sound_v1`]).
    pub iso_audio_entry_v1: bool,
    /// `srat` SamplingRateBox (§12.2.3.2): the *actual* sampling rate,
    /// overriding the entry's 16.16 `samplerate` field (which then
    /// holds a suitable integer multiple/division — typically the
    /// media timescale). Only valid inside an `AudioSampleEntryV1`;
    /// parsed leniently wherever it appears. See
    /// [`SampleDescription::effective_sample_rate`].
    pub sampling_rate: Option<u32>,
    /// `chnl` ChannelLayout box (§12.2.4), when the audio entry
    /// documents its channel/object assignment.
    pub chnl: Option<ChannelLayout>,

    // ─────── Round-6 timecode extension ───────
    /// Parsed `tmcd` sample-description body — populated only when the
    /// track's handler is a time-code track (`hdlr.is_timecode()`) and
    /// the entry's format FourCC is `tmcd`. See [`Tmcd`].
    pub tmcd: Option<Tmcd>,

    // ─────── Round-344 timed-metadata sample entry ───────
    /// Parsed ISO BMFF `MetaDataSampleEntry` (`metx` / `mett` / `urim`,
    /// ISO/IEC 14496-12 §12.3.3) — populated only when the track's
    /// handler is a timed-metadata track (`hdlr.is_metadata()`) and the
    /// entry's format FourCC names one of those subclasses. `None` for
    /// every other handler / format. See [`MetadataSampleEntry`].
    pub metadata: Option<MetadataSampleEntry>,

    /// Parsed ISO BMFF `SubtitleSampleEntry` (`stpp` / `sbtt`, ISO/IEC
    /// 14496-12 §12.6.3) — populated only when the track's handler is a
    /// subtitle track (`hdlr.is_subtitle()`, `subt` subtype) and the
    /// entry's format FourCC names one of those subclasses. `None` for
    /// every other handler / format. See [`SubtitleSampleEntry`].
    pub subtitle: Option<SubtitleSampleEntry>,

    // ─────── Round-360 QuickTime Text Sample Description ───────
    /// Parsed QuickTime Text Sample Description (`text` format FourCC,
    /// QTFF pp. 108–110) — populated only when the track's handler is a
    /// classic QuickTime text track (`hdlr.is_text()`) and the entry's
    /// format FourCC is `text`. Carries display flags, justification,
    /// fore/background colours, the default text box, font face/number,
    /// and the trailing Pascal font name. `None` for every other handler
    /// / format. See [`TextSampleDescription`].
    pub text: Option<TextSampleDescription>,

    /// Parsed ISO BMFF `SimpleTextSampleEntry` (`stxt`, ISO/IEC
    /// 14496-12 §12.5.3) — populated only when the track's handler is a
    /// timed-text track (`hdlr.is_text()`) and the entry's format FourCC
    /// is `stxt`. Carries `content_encoding` / `mime_format` plus the
    /// optional `btrt` BitRateBox and `txtC` TextConfigBox. Distinct from
    /// the QuickTime [`TextSampleDescription`] (`text` FourCC) carried on
    /// the same handler type. `None` for every other handler / format.
    pub simple_text: Option<SimpleTextSampleEntry>,
}

impl SampleDescription {
    /// Typed view of [`SampleDescription::gamma`] as a floating-point
    /// gamma value.
    ///
    /// QTFF p. 94 Table 3-2 describes the `gama` payload as a "32-bit
    /// fixed-point number indicating the gamma level at which the
    /// image was captured." The spec does not call out the radix
    /// point's position explicitly in that line, but every other
    /// QuickTime "32-bit fixed-point" value in the same chapter
    /// (matrix coefficients `a` / `b` / `d` / `e`, mvhd `rate`,
    /// `tapt` width / height — all 16.16) follows the QuickDraw
    /// convention of 16 integer + 16 fractional bits, and the
    /// values observed by ProRes / DV-encoding pipelines (`0x00023333`
    /// ≈ 2.2) round-trip cleanly under that interpretation. The
    /// accessor therefore divides by 65536.0, returning `None` when
    /// the field is absent.
    ///
    /// Callers that need the unscaled wire value should read
    /// [`SampleDescription::gamma`] directly.
    pub fn gamma_value(&self) -> Option<f64> {
        self.gamma.map(|g| g as f64 / 65536.0)
    }

    /// Whether this audio sample description flags the variable-bit-rate
    /// "third variant" (QTFF p. 102): a version-1 sound description whose
    /// Compression ID is `-2`. When true, each sample in the track is a
    /// *compressed frame* of audio and the sample-size table documents
    /// the per-frame compressed sizes (which vary for VBR) rather than a
    /// fixed uncompressed-sample size.
    ///
    /// Returns `false` for version-0 descriptions, for video / timecode
    /// handlers, and for any audio description whose Compression ID is
    /// not `-2`.
    pub fn is_vbr(&self) -> bool {
        self.audio_version == 1 && self.audio_compression_id == -2
    }

    /// The audio entry's actual sampling rate in Hz: the `srat`
    /// SamplingRateBox value when present (ISO/IEC 14496-12:2015
    /// §12.2.3.3 — "when a SamplingRateBox is present, [the
    /// samplerate field] is a suitable integer multiple or division
    /// of the actual sampling rate"), otherwise the integer portion
    /// of the entry's 16.16 `samplerate` field.
    pub fn effective_sample_rate(&self) -> u32 {
        self.sampling_rate.unwrap_or(self.sample_rate)
    }
}

/// One track's accumulated state.
#[derive(Clone, Debug, Default)]
pub struct Track {
    pub tkhd: Tkhd,
    pub mdhd: Mdhd,
    pub hdlr: Hdlr,
    /// Sample-description table — at least one entry per QTFF p. 69.
    pub sample_descriptions: Vec<SampleDescription>,
    pub sample_table: SampleTable,
    /// `edts/elst` edit list, when present. Empty list means "no
    /// edits" — the track plays its media start-to-end.
    pub edits: EditList,
    /// `tref` references this track makes to other tracks
    /// (chapter / timecode / etc).
    pub references: Vec<TrackRef>,
    /// Apple Track Aperture Mode Dimensions (`tapt`); `None` when
    /// the track has no `tapt` atom.
    pub tapt: Option<Tapt>,
    /// `cslg` composition-shift-least-greatest atom (when present),
    /// from `stbl` or `trak` scope. Lets a player short-circuit the
    /// `ctts` scan when computing presentation-time bounds.
    pub cslg: Option<Cslg>,
    /// Track-level Apple `meta` key-value pairs, when present.
    pub meta: Vec<MetaKeyValue>,
    /// Track-level ISO BMFF §8.11 `meta` box, when the track's
    /// `meta` atom is in the ISO/IEC 14496-12 shape rather than the
    /// Apple key-value shape (mutually exclusive with [`Self::meta`]).
    pub bmff_meta: Option<BmffMeta>,
    /// Track-level `udta` user-data entries, when present. Same atom
    /// shape as the movie-level `udta` (©nam / ©cpy / `name` / etc.);
    /// see [`crate::user_data::parse_udta`] for the layout.
    pub user_data: Vec<UserDataEntry>,
    /// Track-level data references parsed from `mdia/minf/dinf/dref`.
    /// One entry per `dref` child atom; the most common shape is a
    /// single `SelfRef` indicating the media is in the same file as
    /// the moov (the demuxer's only currently-supported shape — but
    /// surfacing the parsed list lets callers detect external-alias
    /// tracks without having to walk the atom tree themselves).
    pub data_references: Vec<DataReference>,
    /// Parsed `gmhd` (base-media information header) extension atoms
    /// — `gmin`, `text`, `tmcd/tcmi` (round 5). `None` when the track
    /// uses a typed media header (`vmhd`/`smhd`) instead.
    pub gmhd: Option<Gmhd>,
    /// Parsed `hmhd` Hint Media Header Box (ISO/IEC 14496-12 §12.4.2),
    /// found in the `minf` of a hint track (`hdlr.is_hint()`). Carries
    /// the stream's PDU-size / bit-rate buffering metadata. `None` for
    /// every non-hint track. Round 344.
    pub hmhd: Option<Hmhd>,
    /// Parsed `load` atom (Track Load Settings, QTFF p. 48). `None`
    /// when the track has no `load` child; defaults to "no preload
    /// hints declared" and the player should fall back to its own
    /// heuristics. Round 89.
    pub load: Option<Load>,
    /// Parsed `tsel` Track Selection box (ISO/IEC 14496-12 §8.10.3),
    /// found inside the track-level `udta`. `None` when no `tsel` is
    /// present — equivalent to "no switching information declared"
    /// per §8.10.3.4. Round 95.
    pub track_selection: Option<TrackSelection>,
    /// Parsed `strk` Sub Track boxes (ISO/IEC 14496-12 §8.14.3) found
    /// inside the track-level `udta`. §8.14.3.1 declares the box
    /// `Quantity: Zero or more`, so a track may declare several sub
    /// tracks (one per coded layer for SVC / MVC-style media). Each
    /// entry carries its mandatory `stri` Sub Track Information plus the
    /// `stsg` Sub Track Sample Group entries from its `strd`. Empty when
    /// the track carries no `strk`. ISO BMFF-only — QTFF does not define
    /// this box. Round 293.
    pub sub_tracks: Vec<crate::sub_track::SubTrack>,
    /// Parsed `kind` Track Kind entries (ISO/IEC 14496-12 §8.10.4) from
    /// the track-level `udta`. Empty when no `kind` child is present.
    /// §8.10.4.1 declares the box `Quantity: Zero or more`, so a track
    /// may carry several `kind` entries simultaneously (different
    /// taxonomies labelling the same track). Round 122.
    pub kinds: Vec<KindEntry>,
    /// Parsed `trgr` Track Group Box children (ISO/IEC 14496-12 §8.3.4)
    /// — one entry per FullBox child of the (at most one per `trak`)
    /// `trgr` container, in file order. Each entry is a
    /// `(track_group_type, track_group_id)` membership declaration; two
    /// tracks whose lists contain matching pairs belong to the same
    /// group. Empty when the track carries no `trgr`. ISO BMFF-only —
    /// QTFF does not define this box. Round 199.
    pub track_groups: Vec<crate::track_group::TrackGroupTypeEntry>,
    /// Parsed track-level Clipping atom (QTFF p. 43), when the track's
    /// `trak` carries an optional `clip` declaring a spatial mask
    /// scoped to this individual track (independent of the movie-level
    /// [`crate::MovDemuxer::clipping`]). The wrapper contains a single
    /// `crgn` child whose QuickDraw region surfaces here. `None` for
    /// any track that omits this Apple-only atom (ISO BMFF does not
    /// define `clip`). Round 140.
    pub clipping: Option<Clipping>,
    /// Parsed track-level Track Matte atom (QTFF p. 44), when the
    /// track's `trak` carries an optional `matt` declaring a visual
    /// blending mask scoped to this individual track. The wrapper
    /// contains a single `kmat` Compressed Matte child (QTFF p. 45)
    /// whose FullBox header, image description structure and
    /// compressed matte data surface here. The matte is composited
    /// against the track's video at presentation time; the spec does
    /// not define a movie-level matte (a movie's matte is the union
    /// of its tracks'). `None` for any track that omits this
    /// Apple-only atom (ISO BMFF does not define `matt`). Round 144.
    pub matte: Option<Matte>,
    /// Parsed track-level Track Input Map atom (QTFF pp. 51–53), when
    /// the track's `trak` carries an optional `imap` describing how
    /// each `'ssrc'` (non-primary source) track-reference modulates
    /// this track's presentation (transform matrix, clip region,
    /// volume, balance, graphics mode, per-object variants). The
    /// 1-based [`crate::track_input_map::TrackInputEntry::atom_id`]
    /// indexes into [`Self::references`] filtered by
    /// [`TrackRefKind::NonPrimarySource`]. `None` for tracks that omit
    /// this Apple-only atom (ISO BMFF does not define `imap`).
    /// Round 216.
    pub track_input_map: Option<crate::track_input_map::TrackInputMap>,
    /// Samples appended by `moof/traf/trun` fragment runs (ISO/IEC
    /// 14496-12 §8.8). Empty for non-fragmented streams. Each
    /// entry already has its absolute file offset, DTS, duration,
    /// keyframe flag, sample-description-id and composition offset
    /// resolved via the tfhd → trex defaults cascade. Round 18
    /// builds these from `mvex/trex` + `moof/traf/tfhd/trun` so a
    /// fragmented `qt  ` or `mp4` plays straight through
    /// [`crate::MovDemuxer::next_packet`].
    pub fragment_samples: Vec<SampleEntry>,
    /// Which media-header box was found in this track's `minf`
    /// (ISO/IEC 14496-12 §8.4.5.1, "Exactly one specific media header
    /// shall be present"). Surfaces the box *type* the producer wrote —
    /// `vmhd` / `smhd` / `hmhd` / `sthd` / `nmhd` / `gmhd` — which is a
    /// finer classification signal than the handler subtype alone (a
    /// generic stream can legally pick either `gmhd` or `nmhd`; a
    /// subtitle track uses `sthd`). [`MediaHeaderKind::None`] when no
    /// media-header box was present (malformed but tolerated). The two
    /// empty-FullBox variants (`nmhd`/`sthd`) carry no further fields;
    /// the parsed-payload variants surface on [`Self::hmhd`] / [`Self::gmhd`].
    /// Round 357.
    pub media_header_kind: MediaHeaderKind,
    /// Extended language tag (`elng`, ISO/IEC 14496-12 §8.4.6) — an
    /// RFC 4646 (BCP 47) language tag such as `"en-US"` parsed from the
    /// optional `elng` peer of the media header inside `mdia`. Overrides
    /// the packed [`Mdhd::language`] code when the two disagree
    /// (§8.4.6.1). `None` when the track carries no `elng` box (the
    /// common case; callers should fall back to `mdhd.language`).
    /// Round 357.
    pub extended_language: Option<String>,
    /// Per-fragment sample-auxiliary-information records collected
    /// from each `traf` that names this track (ISO/IEC 14496-12
    /// §8.7.8.1 / §8.7.9.1, `traf` scope per §8.8.6). Empty for
    /// non-fragmented streams and for fragmented tracks that ship no
    /// `saiz` / `saio` boxes inside their `traf` containers.
    ///
    /// Order matches the on-disk fragment order (i.e. the order in
    /// which `moof`s appear in the file, which is also the order in
    /// [`crate::MovDemuxer::fragment_sequence_numbers`]). Use
    /// [`crate::MovDemuxer::fragment_sample_aux_info`] to slice this
    /// by discriminator pair across all fragments for a single track.
    /// Round 150.
    pub fragment_sample_aux: Vec<crate::sample_aux::FragmentSampleAux>,
}

impl Track {
    /// Track type label `"vide"` / `"soun"` / unknown FourCC, derived
    /// from the `hdlr` component subtype.
    pub fn type_str(&self) -> &str {
        std::str::from_utf8(&self.hdlr.component_subtype).unwrap_or("????")
    }

    /// True for tracks whose hdlr carries `vide`.
    pub fn is_video(&self) -> bool {
        self.hdlr.is_video()
    }

    /// True for tracks whose hdlr carries `soun`.
    pub fn is_audio(&self) -> bool {
        self.hdlr.is_audio()
    }

    /// True for QuickTime `text` tracks (chapter lists, simple
    /// overlays). See [`Hdlr::is_text`].
    pub fn is_text(&self) -> bool {
        self.hdlr.is_text()
    }

    /// True for ISO BMFF subtitle / caption tracks (`subt` / `sbtl`).
    pub fn is_subtitle(&self) -> bool {
        self.hdlr.is_subtitle()
    }

    /// True for `tmcd` time-code tracks.
    pub fn is_timecode(&self) -> bool {
        self.hdlr.is_timecode()
    }

    /// First sample description's data-format FourCC. The QTFF
    /// guarantees at least one entry exists when the track has
    /// data (p. 69).
    pub fn primary_format(&self) -> Option<[u8; 4]> {
        self.sample_descriptions.first().map(|d| d.format)
    }

    /// 1-based track-id of the *chapter* track this track points at
    /// (`tref/chap`), if any. Returns the first track-id of the
    /// matching reference; multiple-chap tracks are unusual but
    /// permitted by QTFF.
    pub fn chapter_track_ref(&self) -> Option<u32> {
        self.references
            .iter()
            .find(|r| r.kind == TrackRefKind::Chapter)
            .and_then(|r| r.track_ids.first().copied())
            .filter(|&id| id != 0)
    }

    /// 1-based track-id of the *timecode* track this track points at
    /// (`tref/tmcd`), if any.
    pub fn timecode_track_ref(&self) -> Option<u32> {
        self.references
            .iter()
            .find(|r| r.kind == TrackRefKind::Timecode)
            .and_then(|r| r.track_ids.first().copied())
            .filter(|&id| id != 0)
    }

    /// All `tref` reference track-ids of the given kind. Useful when
    /// a track references several others (e.g. multiple `hint` track
    /// references for an RTP source).
    pub fn track_refs_of_kind(&self, kind: TrackRefKind) -> Vec<u32> {
        self.references
            .iter()
            .filter(|r| r.kind == kind)
            .flat_map(|r| r.track_ids.iter().copied())
            .filter(|&id| id != 0)
            .collect()
    }

    /// 1-based track-ids of every track this track declares a `tref/sync`
    /// reference to — QTFF p. 50 Table 2-2 row `'sync'`
    /// ("Synchronization. Usually between a video and sound track.
    /// Indicates that the two tracks are synchronized."). Each entry is
    /// a peer the writer pinned for tight A/V lockstep. The reference
    /// is directional from this track to the listed peers; spec note on
    /// p. 50 records that the relationship may be reciprocated by the
    /// peer track listing this track as a `'sync'` source as well. A
    /// 0-valued slot (permitted on p. 51 for "unused entries") is
    /// filtered out so callers see only resolvable track-ids. The
    /// result preserves declaration order across every `'sync'`
    /// reference-type atom inside the track's `tref`.
    pub fn sync_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::Sync)
    }

    /// 1-based track-ids of every track this track declares a
    /// `tref/scpt` reference to — QTFF p. 50 Table 2-2 row `'scpt'`
    /// ("Transcript. Usually references a text track."). The writer
    /// pairs the track with a sibling text track that carries a
    /// transcribed dialogue / narration line stream. As with every
    /// other `tref` accessor on this type, a 0-valued slot is filtered
    /// out and the result preserves declaration order across every
    /// `'scpt'` reference-type atom inside `tref`.
    pub fn transcript_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::Transcript)
    }

    /// 1-based track-ids of every track this track declares a
    /// `tref/hint` reference to — QTFF p. 50 Table 2-2 row `'hint'`
    /// ("The referenced tracks contain the original media for this
    /// hint track."). A QuickTime hint track (RTP packetization
    /// metadata, QTFF "Hint Media" p. 145) names its source media
    /// tracks through this reference so a streaming server can locate
    /// the bytes each packet hint cites without re-walking the file's
    /// codec tags. As with the other `tref` accessors a 0-valued slot
    /// is filtered out and the result preserves declaration order
    /// across every `'hint'` reference-type atom inside `tref`.
    pub fn hint_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::Hint)
    }

    /// 1-based track-ids of every track this track declares a
    /// `tref/ssrc` reference to — QTFF p. 50 Table 2-2 row `'ssrc'`
    /// ("Nonprimary source. Indicates that the referenced track should
    /// send its data to this track, rather than presenting it. The
    /// referencing track will use the data to modify how it presents
    /// its data."). The atom-id-indexed [`crate::track_input_map::TrackInputMap`]
    /// (when this track also carries an `imap`) describes how each
    /// 1-based slot in this list modulates the track's presentation
    /// (transform matrix, clip region, volume, balance, graphics mode,
    /// per-object variants). As with the other `tref` accessors a
    /// 0-valued slot is filtered out and the result preserves
    /// declaration order across every `'ssrc'` reference-type atom
    /// inside `tref`.
    pub fn non_primary_source_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::NonPrimarySource)
    }

    /// 1-based track-ids of every track this track *describes* via a
    /// `tref/cdsc` Content-Describes reference (ISO/IEC 14496-12
    /// §8.3.3.3). A descriptive or metadata track points at the content
    /// track it annotates — e.g. a timed-metadata track (§12.3.2) or an
    /// RTCP reception hint track linking to its RTP reception hint
    /// track. As with the other `tref` accessors a 0-valued slot is
    /// filtered out and declaration order is preserved across every
    /// `'cdsc'` reference-type atom inside `tref`.
    pub fn content_describes_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::ContentDescribes)
    }

    /// 1-based track-ids of every track this track draws fonts from via
    /// a `tref/font` reference (ISO/IEC 14496-12 §8.3.3.3) — e.g. a
    /// timed-text / subtitle track naming the track that carries the
    /// font resources it renders with. 0-valued slots filtered;
    /// declaration order preserved.
    pub fn font_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::Font)
    }

    /// 1-based track-ids of every hint track this track depends on via a
    /// `tref/hind` Hint-Dependency reference (ISO/IEC 14496-12
    /// §8.3.3.3): this track should only be used if the referenced hint
    /// track is also used (e.g. layered IP multicast over RTP). The
    /// referenced tracks are themselves hint tracks. 0-valued slots
    /// filtered; declaration order preserved.
    pub fn hint_dependency_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::HintDependency)
    }

    /// 1-based track-ids of every video track this track supplies
    /// auxiliary **depth** information for via a `tref/vdep` reference
    /// (ISO/IEC 14496-12 §8.3.3.3). 0-valued slots filtered; declaration
    /// order preserved.
    pub fn video_depth_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::VideoDepth)
    }

    /// 1-based track-ids of every video track this track supplies
    /// auxiliary **parallax** information for via a `tref/vplx`
    /// reference (ISO/IEC 14496-12 §8.3.3.3). 0-valued slots filtered;
    /// declaration order preserved.
    pub fn video_parallax_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::VideoParallax)
    }

    /// 1-based track-ids of every track this track supplies subtitle /
    /// timed-text / overlay-graphical information for via a `tref/subt`
    /// reference (ISO/IEC 14496-12 §8.3.3.3). The reference may target
    /// the listed track or any track in the alternate group it belongs
    /// to. 0-valued slots filtered; declaration order preserved.
    pub fn subtitle_track_refs(&self) -> Vec<u32> {
        self.track_refs_of_kind(TrackRefKind::Subtitle)
    }

    /// Track-level `dref` data-reference list. Empty when the track
    /// has no `dinf/dref` atom (legal per QTFF, in which case the
    /// media is implicitly self-referential).
    pub fn data_references(&self) -> &[DataReference] {
        &self.data_references
    }

    /// True when the track's `dref` list contains *only* self-
    /// references (or is empty). External-alias tracks return false
    /// here; callers can then refuse to emit packets for them or fall
    /// back to alias resolution.
    pub fn is_self_contained(&self) -> bool {
        self.data_references
            .iter()
            .all(|d| matches!(d, DataReference::SelfRef))
    }

    /// True when the track's `tkhd.flags` bit 0 (`enabled`) is set.
    /// Disabled tracks should not contribute to the default
    /// presentation (QTFF p. 31, ISO/IEC 14496-12 §8.3.1.3). When the
    /// track-header atom is absent (a malformed but tolerated case) we
    /// default to `true` — most file producers always emit `tkhd` and
    /// callers that need stricter handling can inspect `tkhd.flags`
    /// directly.
    pub fn is_enabled(&self) -> bool {
        // QTFF "Track Header Atom" pp. 31–32 layout: the low byte of
        // the 24-bit flags carries `0x01 = enabled`, `0x02 = in_movie`,
        // `0x04 = in_preview`, `0x08 = in_poster`.
        (self.tkhd.flags & 0x01) != 0
    }

    /// True when `tkhd.flags` bit 1 (`in_movie`) is set — the track
    /// participates in the movie's main presentation. QTFF p. 32.
    pub fn participates_in_movie(&self) -> bool {
        (self.tkhd.flags & 0x02) != 0
    }

    /// True when `tkhd.flags` bit 2 (`in_preview`) is set — the track
    /// participates in the movie's preview. QTFF p. 32.
    pub fn participates_in_preview(&self) -> bool {
        (self.tkhd.flags & 0x04) != 0
    }

    /// True when `tkhd.flags` bit 3 (`in_poster`) is set — the track
    /// participates in the movie's poster (single-frame still). QTFF
    /// p. 32.
    pub fn participates_in_poster(&self) -> bool {
        (self.tkhd.flags & 0x08) != 0
    }

    /// `tkhd.alternate_group` — non-zero when the track belongs to an
    /// alternate group (one of several mutually-exclusive playback
    /// options, e.g. multi-language audio tracks). Zero means "not a
    /// member of any alternate group" (QTFF p. 33, ISO/IEC 14496-12
    /// §8.3.1.3). The on-wire field is signed; we surface it raw.
    pub fn alternate_group(&self) -> i16 {
        self.tkhd.alternate_group
    }

    /// Parsed [`Load`] (Track Load Settings, QTFF p. 48), when the
    /// track carries a `load` atom. Players use it to decide whether
    /// and when to preload the track into memory and how to budget
    /// I/O against the `default_hints` bits.
    pub fn load_settings(&self) -> Option<&Load> {
        self.load.as_ref()
    }

    /// Parsed [`TrackSelection`] (ISO/IEC 14496-12 §8.10.3), when the
    /// track's `udta` carries a `tsel` child. The box refines
    /// [`Self::alternate_group`] with a finer-grained switch group and
    /// a list of typed attribute FourCCs the player can use to rank
    /// peer tracks at session start and during runtime switching.
    pub fn track_selection(&self) -> Option<&TrackSelection> {
        self.track_selection.as_ref()
    }

    /// Parsed `strk` Sub Track boxes (ISO/IEC 14496-12 §8.14.3) from the
    /// track-level `udta`. Empty slice when the track declares no sub
    /// tracks; the box is `Quantity: Zero or more` (§8.14.3.1), so a
    /// layered-codec track may surface several entries in file order.
    /// Each [`crate::sub_track::SubTrack`] carries its mandatory `stri`
    /// Sub Track Information plus any `stsg` Sub Track Sample Group
    /// entries from its `strd`.
    pub fn sub_tracks(&self) -> &[crate::sub_track::SubTrack] {
        &self.sub_tracks
    }

    /// Parsed `kind` Track Kind entries (ISO/IEC 14496-12 §8.10.4) from
    /// the track-level `udta`. Empty slice when the track has no `kind`
    /// child; the box is `Quantity: Zero or more` (§8.10.4.1) so a
    /// caller may receive any number of entries, in file order.
    pub fn track_kinds(&self) -> &[KindEntry] {
        &self.kinds
    }

    /// Parsed `trgr` Track Group Box children (ISO/IEC 14496-12 §8.3.4)
    /// for this track. Each entry is one `(track_group_type,
    /// track_group_id)` membership declaration — two tracks whose lists
    /// share a `(type, id)` pair belong to the same group. Empty slice
    /// when the track has no `trgr` child (the common case for plain
    /// MP4 / fMP4 / `.mov` inputs that don't use track grouping); the
    /// box is itself `Quantity: Zero or one` (§8.3.4.1) but its
    /// children are unconstrained.
    pub fn track_groups(&self) -> &[crate::track_group::TrackGroupTypeEntry] {
        &self.track_groups
    }

    /// Parsed Track Input Map atom (QTFF pp. 51–53), when the track's
    /// `trak` carries an optional `imap`. Each entry describes how one
    /// `'ssrc'` (non-primary-source) track reference modulates this
    /// track's presentation; resolve an entry against the parent's
    /// reference list via [`crate::track_input_map::TrackInputEntry::atom_id`]
    /// (1-based index into the `'ssrc'` entries). `None` for tracks
    /// that omit this Apple-only atom (ISO BMFF does not define `imap`).
    pub fn track_input_map(&self) -> Option<&crate::track_input_map::TrackInputMap> {
        self.track_input_map.as_ref()
    }

    /// Resolve the track's `edts/elst` edit list into the sequence of
    /// movie-timescale [`EditSegment`]s it describes. When the list is
    /// empty (no `edts` atom present), returns a single synthetic
    /// segment covering the entire track media — this is the spec's
    /// "absence of an edit list" → "presentation starts immediately"
    /// rule (QTFF p. 47 / ISO/IEC 14496-12 §8.6.5.1 last paragraph),
    /// so callers can drive the same mapper path regardless of whether
    /// the file declares an explicit elst.
    ///
    /// `movie_duration` lets the resolver append the implicit trailing
    /// empty edit when the explicit edits sum to less than the movie
    /// header's declared duration. Pass `None` to disable this (e.g.
    /// when working with a single track in isolation).
    pub fn edit_segments(
        &self,
        movie_timescale: u32,
        movie_duration: Option<u64>,
    ) -> Vec<EditSegment> {
        if self.edits.is_empty() {
            // Synthesize a one-segment list covering the track's full
            // media duration. Convert `mdhd.duration` (in media-
            // timescale ticks) into movie-timescale ticks.
            if movie_timescale == 0 || self.mdhd.time_scale == 0 {
                return Vec::new();
            }
            let dur_media = self.mdhd.duration;
            let dur_movie = (dur_media as u128 * movie_timescale as u128
                + (self.mdhd.time_scale as u128 / 2))
                / self.mdhd.time_scale as u128;
            let dur_movie = dur_movie as u64;
            return vec![EditSegment {
                movie_time_start: 0,
                movie_time_end: dur_movie,
                kind: crate::edit::EditSegmentKind::Media {
                    media_time_start: 0,
                    media_rate: 0x0001_0000,
                },
            }];
        }
        resolve_edit_segments(&self.edits, movie_duration)
    }

    /// Map a media-timescale presentation timestamp `media_pts` for
    /// this track to its corresponding movie-timescale presentation
    /// timestamp via the track's edit list. Returns `None` when the
    /// sample is dropped by every non-empty edit segment (e.g. a
    /// sample whose PTS falls outside every `[media_time, media_time +
    /// segment_duration)` window).
    ///
    /// `movie_timescale` is the movie-header timescale
    /// (`Mvhd::time_scale`). Callers that don't have an `mvhd`
    /// available can pass the track's own `mdhd.time_scale`, but the
    /// returned value will then be in *media-timescale* ticks (since
    /// the empty-edit rescaling becomes a no-op).
    pub fn media_pts_to_movie_pts(
        &self,
        media_pts: i64,
        movie_timescale: u32,
        movie_duration: Option<u64>,
    ) -> Option<i64> {
        let segs = self.edit_segments(movie_timescale, movie_duration);
        media_pts_to_movie_pts(&segs, media_pts, movie_timescale, self.mdhd.time_scale)
    }

    /// Inverse of [`Track::media_pts_to_movie_pts`]. Maps a
    /// movie-timescale presentation timestamp `movie_pts` back to its
    /// corresponding media-timescale presentation timestamp via the
    /// track's edit list. Returns `None` when the queried `movie_pts`
    /// falls inside an empty-edit window (no media correspondence),
    /// past the end of the resolved segment list, or before the
    /// timeline starts (negative `movie_pts`).
    ///
    /// `movie_timescale` is the movie-header timescale
    /// (`Mvhd::time_scale`). The seek-by-presentation-time entry
    /// point: walk the per-track sample queue keyed on the
    /// `Some(media_pts)` returned here when the caller knows the
    /// desired movie-time tick.
    pub fn movie_pts_to_media_pts(
        &self,
        movie_pts: i64,
        movie_timescale: u32,
        movie_duration: Option<u64>,
    ) -> Option<i64> {
        let segs = self.edit_segments(movie_timescale, movie_duration);
        movie_pts_to_media_pts(&segs, movie_pts, movie_timescale, self.mdhd.time_scale)
    }
}

/// Parse a `stsd` payload: count + N × per-entry record. Layout per
/// QTFF p. 70 figure 2-27.
pub fn parse_stsd(payload: &[u8], hdlr: &Hdlr) -> Result<Vec<SampleDescription>> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: stsd payload < 8 bytes"));
    }
    // FullBox version: 0 for every classic table; 1 signals that an
    // audio entry may be an ISO `AudioSampleEntryV1` (ISO/IEC
    // 14496-12:2015 §8.5.2 / §12.2.3 — "version is set to zero unless
    // the box contains an AudioSampleEntryV1, whereupon version must
    // be 1").
    let stsd_version = payload[0];
    let n = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    // Allocate for the byte-backed entry count, not the declared one:
    // each sample description occupies at least 16 bytes (the QTFF
    // p. 70 universal header, `size >= 16` enforced below), so cap the
    // pre-allocation at what the body can actually hold —
    // `Vec::with_capacity` must not turn a forged count into a
    // multi-gigabyte allocation. An overdeclared count still errors in
    // the loop when it runs out of bytes.
    let mut out = Vec::with_capacity((n as usize).min((payload.len() - 8) / 16));
    let mut p = 8usize;
    for _ in 0..n {
        if p + 16 > payload.len() {
            return Err(Error::invalid("MOV: stsd entry truncated"));
        }
        let size = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]]);
        if size < 16 || (p + size as usize) > payload.len() {
            return Err(Error::invalid("MOV: stsd entry size invalid"));
        }
        let mut format = [0u8; 4];
        format.copy_from_slice(&payload[p + 4..p + 8]);
        // 6 bytes reserved
        let dref = u16::from_be_bytes([payload[p + 14], payload[p + 15]]);
        let mut entry = SampleDescription {
            format,
            data_reference_index: dref,
            ..SampleDescription::default()
        };

        let body_off = p + 16;
        let body_end = p + size as usize;
        let body = &payload[body_off..body_end];

        if hdlr.is_video() && body.len() >= 70 {
            // Video sample description (QTFF p. 92):
            //   ver:2 rev:2 vendor:4 temp_q:4 spatial_q:4
            //   width:2 height:2 hres:4 vres:4 data_size:4 frame_count:2
            //   compressor_name:32 depth:2 color_table_id:2
            // → 70 bytes of fixed fields; extras (e.g. avcC/clap/colr)
            //   follow.
            entry.width = u16::from_be_bytes([body[16], body[17]]);
            entry.height = u16::from_be_bytes([body[18], body[19]]);
            entry.extra = body[70..].to_vec();
            scan_video_extensions(&mut entry)?;
        } else if hdlr.is_timecode() && &format == b"tmcd" && body.len() >= 20 {
            // Time-code sample description (QTFF p. 106). Distinct from
            // the `tmcd` container inside `gmhd` (round 5, see
            // `Gmhd::tcmi`) which wraps display-style fields. The
            // `tmcd` *inside stsd* carries:
            //   reserved:u32  flags:u32
            //   time_scale:u32  frame_duration:u32
            //   number_of_frames:u8  reserved:24-bit
            //   [optional source-reference user data atom]
            entry.tmcd = Some(parse_tmcd_sample_description(body)?);
            // Keep the trailing source-reference bytes in `extra` so
            // future rounds can also surface ftab/style atoms.
            entry.extra = body[20..].to_vec();
        } else if hdlr.is_metadata() && matches!(&format, b"metx" | b"mett" | b"urim") {
            // ISO BMFF timed-metadata sample entry (ISO/IEC 14496-12
            // §12.3.3). The SampleEntry base contributes the universal
            // 16-byte header already consumed above (`size`/`format`/
            // 6 reserved/`data_reference_index`); `body` therefore starts
            // at the subclass-specific fields, exactly what
            // `parse_metadata_sample_entry` expects.
            entry.metadata = parse_metadata_sample_entry(&format, body)?;
            // Preserve the raw body for any caller that needs the exact
            // wire bytes (e.g. round-trip muxing) alongside the typed view.
            entry.extra = body.to_vec();
        } else if hdlr.is_subtitle() && matches!(&format, b"stpp" | b"sbtt") {
            // ISO BMFF subtitle sample entry (ISO/IEC 14496-12 §12.6.3),
            // carried on a `subt`-handler track. Like the metadata
            // entries above, `body` starts at the subclass-specific
            // fields after the universal 16-byte SampleEntry header.
            entry.subtitle = parse_subtitle_sample_entry(&format, body)?;
            entry.extra = body.to_vec();
        } else if hdlr.is_text() && &format == b"text" && body.len() >= TEXT_SAMPLE_DESC_FIXED_LEN {
            // QuickTime Text Sample Description (QTFF pp. 108–110). A
            // classic `text`-handler track declares its display config
            // (flags, justification, fore/background colour, default
            // text box, font face/number, Pascal font name) here. Distinct
            // from the per-sample text payload (`[len:u16][text][exts]`)
            // decoded by `chapter::parse_text_sample_styles`, and from the
            // `gmhd/text` media-information header (`gmhd::TextHeader`).
            entry.text = Some(parse_text_sample_description(body)?);
            // Preserve the raw body so a round-trip muxer / future style
            // extension reader retains the exact wire bytes.
            entry.extra = body.to_vec();
        } else if hdlr.is_text() && &format == b"stxt" {
            // ISO BMFF SimpleTextSampleEntry (ISO/IEC 14496-12 §12.5.3),
            // a timed-text track variant carried on the same `text`
            // handler as the QuickTime `text` description but selected by
            // the `stxt` FourCC. `body` starts at the subclass-specific
            // fields after the universal 16-byte SampleEntry header.
            entry.simple_text = Some(parse_stxt(body)?);
            entry.extra = body.to_vec();
        } else if hdlr.is_audio() && body.len() >= 20 {
            // Sound sample description v0 (QTFF p. 100):
            //   ver:2 rev:2 vendor:4 channels:2 sample_size:2
            //   compression_id:2 packet_size:2 sample_rate:4
            // → 20 bytes; v1 adds 16 bytes more (samples_per_packet,
            //   bytes_per_packet, bytes_per_frame, bytes_per_sample).
            let version = u16::from_be_bytes([body[0], body[1]]);
            entry.audio_version = version;
            entry.channels = u16::from_be_bytes([body[8], body[9]]);
            entry.bits_per_sample = u16::from_be_bytes([body[10], body[11]]);
            // Compression ID (QTFF p. 100): a signed 16-bit field. `0`
            // for the common case; `-2` (on-wire `0xFFFE`) flags the VBR
            // third variant on a version-1 description.
            entry.audio_compression_id = i16::from_be_bytes([body[12], body[13]]);
            entry.sample_rate = u32::from_be_bytes([body[16], body[17], body[18], body[19]]) >> 16;
            // Sample rate is 16.16; integer portion lives in the high 16 bits.
            // Version 1 (QTFF p. 101) appends four 32-bit fixed-ratio
            // fields after the 20-byte version-0 fixed body; surface them
            // typed and start the codec `extra` scan past them.
            let extra_start = match version {
                0 => 20usize,
                // ISO BMFF AudioSampleEntryV1 (ISO/IEC 14496-12:2015
                // §12.2.3.2): entry_version 1 is only legal inside a
                // version-1 stsd, and its fixed body is the *same*
                // 20 bytes as version 0 — the QTFF 16-byte
                // CompressionInfo extension is NOT present. Optional
                // srat / chnl boxes follow in the trailing box area.
                1 if stsd_version == 1 => {
                    entry.iso_audio_entry_v1 = true;
                    20
                }
                1 if body.len() >= 36 => {
                    entry.sound_v1 = Some(SoundV1 {
                        samples_per_packet: u32::from_be_bytes([
                            body[20], body[21], body[22], body[23],
                        ]),
                        bytes_per_packet: u32::from_be_bytes([
                            body[24], body[25], body[26], body[27],
                        ]),
                        bytes_per_frame: u32::from_be_bytes([
                            body[28], body[29], body[30], body[31],
                        ]),
                        bytes_per_sample: u32::from_be_bytes([
                            body[32], body[33], body[34], body[35],
                        ]),
                    });
                    36
                }
                _ => 20,
            };
            if body.len() > extra_start {
                entry.extra = body[extra_start..].to_vec();
            }
            scan_audio_extensions(&mut entry)?;
        } else {
            // Unknown handler — keep whatever follows the universal 16-byte
            // header. Useful for `subt`/`tmcd`/`meta` tracks in later rounds.
            entry.extra = body.to_vec();
        }

        out.push(entry);
        p = body_end;
    }
    Ok(out)
}

/// Scan the `extra` blob of a video sample description for the
/// well-known atom-style extensions (`gama`, `pasp`, `clap`, `colr`,
/// `fiel`, `mjqt`, `mjht`).
/// Recognised atoms are extracted into typed fields; the original
/// `extra` blob is left intact so codec-specific bytes (e.g. `avcC`,
/// `hvcC`) remain available for downstream consumers.
fn scan_video_extensions(entry: &mut SampleDescription) -> Result<()> {
    let buf = entry.extra.clone();
    walk_atoms(&buf, |fourcc, payload| {
        match fourcc {
            b"gama" if payload.len() >= 4 => {
                entry.gamma = Some(u32::from_be_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                ]));
            }
            b"pasp" => {
                entry.pasp = Some(parse_pasp(payload)?);
            }
            b"clap" => {
                entry.clap = Some(parse_clap(payload)?);
            }
            b"colr" => {
                entry.colr = Some(parse_colr(payload)?);
            }
            b"fiel" => {
                // QTFF p. 94, Table 3-2: two 8-bit integers —
                // field_count + field_ordering. Surface as the typed
                // pair; the parser rejects any other body length.
                entry.fiel = Some(parse_fiel(payload)?);
            }
            b"mjqt" => {
                // QTFF p. 94, Table 3-2: default Motion-JPEG
                // quantization table. Surface the raw DQT bytes
                // verbatim; the JPEG codec owns their interpretation.
                entry.mjqt = Some(parse_mjqt(payload)?);
            }
            b"mjht" => {
                // QTFF p. 94, Table 3-2: default Motion-JPEG Huffman
                // table. Surface the raw DHT bytes verbatim; the JPEG
                // codec owns their interpretation.
                entry.mjht = Some(parse_mjht(payload)?);
            }
            _ => {}
        }
        Ok(())
    })
}

/// Scan the `extra` blob of an audio sample description for `chan`
/// (and only `chan` in round 2 — codec-specific extensions such as
/// `wave` / `esds` stay opaque for downstream codec crates).
fn scan_audio_extensions(entry: &mut SampleDescription) -> Result<()> {
    let buf = entry.extra.clone();
    let channel_count = entry.channels;
    walk_atoms(&buf, |fourcc, payload| {
        match fourcc {
            b"chan" => {
                entry.chan = Some(parse_chan(payload)?);
            }
            b"srat" => {
                entry.sampling_rate = Some(parse_srat(payload)?);
            }
            b"chnl" => {
                entry.chnl = Some(parse_chnl(payload, channel_count)?);
            }
            _ => {}
        }
        Ok(())
    })
}

/// Parse a `srat` SamplingRateBox body (ISO/IEC 14496-12:2015
/// §12.2.3.2): a FullBox (version 0) wrapping one 32-bit integer, the
/// actual sampling rate of the audio media.
pub fn parse_srat(payload: &[u8]) -> Result<u32> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: srat body < 8 bytes"));
    }
    if payload[0] != 0 {
        return Err(Error::invalid("MOV: srat version != 0"));
    }
    Ok(u32::from_be_bytes([
        payload[4], payload[5], payload[6], payload[7],
    ]))
}

/// Parse a `chnl` ChannelLayout box body (ISO/IEC 14496-12:2015
/// §12.2.4.2). `channel_count` is the enclosing audio sample entry's
/// `channelcount` — the explicit-position loop runs once per channel
/// ("channelCount comes from the sample entry").
pub fn parse_chnl(payload: &[u8], channel_count: u16) -> Result<ChannelLayout> {
    if payload.len() < 5 {
        return Err(Error::invalid("MOV: chnl body < 5 bytes"));
    }
    if payload[0] != 0 {
        return Err(Error::invalid("MOV: chnl version != 0"));
    }
    let stream_structure = payload[4];
    let mut p = 5usize;
    let mut out = ChannelLayout {
        stream_structure,
        channels: None,
        object_count: None,
    };
    if stream_structure & ChannelLayout::CHANNEL_STRUCTURED != 0 {
        let defined_layout = *payload
            .get(p)
            .ok_or_else(|| Error::invalid("MOV: chnl truncated at definedLayout"))?;
        p += 1;
        if defined_layout == 0 {
            // One explicit OutputChannelPosition per channel; code 126
            // appends an azimuth (i16) + elevation (i8) pair.
            let mut rows = Vec::with_capacity((channel_count as usize).min(payload.len()));
            for _ in 0..channel_count {
                let pos = *payload
                    .get(p)
                    .ok_or_else(|| Error::invalid("MOV: chnl truncated in speaker positions"))?;
                p += 1;
                let (azimuth, elevation) = if pos == 126 {
                    if p + 3 > payload.len() {
                        return Err(Error::invalid("MOV: chnl truncated in explicit position"));
                    }
                    let az = i16::from_be_bytes([payload[p], payload[p + 1]]);
                    let el = payload[p + 2] as i8;
                    p += 3;
                    (Some(az), Some(el))
                } else {
                    (None, None)
                };
                rows.push(SpeakerPosition {
                    speaker_position: pos,
                    azimuth,
                    elevation,
                });
            }
            out.channels = Some(ChannelStructure::Explicit(rows));
        } else {
            if p + 8 > payload.len() {
                return Err(Error::invalid("MOV: chnl truncated at omittedChannelsMap"));
            }
            let omitted = u64::from_be_bytes([
                payload[p],
                payload[p + 1],
                payload[p + 2],
                payload[p + 3],
                payload[p + 4],
                payload[p + 5],
                payload[p + 6],
                payload[p + 7],
            ]);
            p += 8;
            out.channels = Some(ChannelStructure::Defined {
                defined_layout,
                omitted_channels_map: omitted,
            });
        }
    }
    if stream_structure & ChannelLayout::OBJECT_STRUCTURED != 0 {
        let n = *payload
            .get(p)
            .ok_or_else(|| Error::invalid("MOV: chnl truncated at object_count"))?;
        out.object_count = Some(n);
    }
    Ok(out)
}

/// Walk the top-level atoms inside an in-memory buffer. The callback
/// receives the FourCC and the atom's payload (no header). Unknown /
/// truncated atoms are silently dropped to stay forgiving against
/// malformed extras.
fn walk_atoms<F>(buf: &[u8], mut visit: F) -> Result<()>
where
    F: FnMut(&[u8; 4], &[u8]) -> Result<()>,
{
    let mut p = 0usize;
    while p + 8 <= buf.len() {
        let size = u32::from_be_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
        if size == 0 {
            // size==0 ⇒ extends to end of containing buffer.
            let mut fc = [0u8; 4];
            fc.copy_from_slice(&buf[p + 4..p + 8]);
            visit(&fc, &buf[p + 8..])?;
            break;
        }
        if size < 8 || p + size > buf.len() {
            // Malformed; bail out lenient.
            break;
        }
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&buf[p + 4..p + 8]);
        visit(&fc, &buf[p + 8..p + size])?;
        p += size;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vide_hdlr() -> Hdlr {
        Hdlr {
            component_type: *b"mhlr",
            component_subtype: *b"vide",
            component_manufacturer: [0; 4],
        }
    }

    fn soun_hdlr() -> Hdlr {
        Hdlr {
            component_type: *b"mhlr",
            component_subtype: *b"soun",
            component_manufacturer: [0; 4],
        }
    }

    #[test]
    fn stsd_video_entry_extracts_dims() {
        // Build one stsd entry: size=86 (16 universal + 70 video fixed),
        // format='avc1', dims 1920×1080.
        let mut p = Vec::new();
        // ver+flags
        p.extend_from_slice(&0u32.to_be_bytes());
        // n_entries=1
        p.extend_from_slice(&1u32.to_be_bytes());
        // entry: size=86, format='avc1'
        let entry_size: u32 = 86;
        p.extend_from_slice(&entry_size.to_be_bytes());
        p.extend_from_slice(b"avc1");
        // 6 reserved
        p.extend_from_slice(&[0u8; 6]);
        // data_reference_index=1
        p.extend_from_slice(&1u16.to_be_bytes());
        // 70-byte video fixed body. width @ offset 16, height @ 18
        // (QTFF p. 92: width/height follow the two quality fields).
        let mut body = vec![0u8; 70];
        body[16..18].copy_from_slice(&1920u16.to_be_bytes());
        body[18..20].copy_from_slice(&1080u16.to_be_bytes());
        p.extend_from_slice(&body);

        let v = parse_stsd(&p, &vide_hdlr()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(&v[0].format, b"avc1");
        assert_eq!(v[0].data_reference_index, 1);
        assert_eq!(v[0].width, 1920);
        assert_eq!(v[0].height, 1080);
    }

    #[test]
    fn stsd_audio_entry_extracts_rate_channels() {
        // size = 16 + 20 = 36 ; format='sowt' (16-bit LE PCM) ; ch=2, bits=16, rate=44100<<16
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        let entry_size: u32 = 36;
        p.extend_from_slice(&entry_size.to_be_bytes());
        p.extend_from_slice(b"sowt");
        p.extend_from_slice(&[0u8; 6]);
        p.extend_from_slice(&1u16.to_be_bytes());
        // 20-byte sound v0 body
        let mut body = vec![0u8; 20];
        // version=0
        // channels @ 8..10 = 2
        body[8..10].copy_from_slice(&2u16.to_be_bytes());
        // bits @ 10..12 = 16
        body[10..12].copy_from_slice(&16u16.to_be_bytes());
        // sample_rate @ 16..20 = 44100 << 16
        body[16..20].copy_from_slice(&((44100u32) << 16).to_be_bytes());
        p.extend_from_slice(&body);

        let v = parse_stsd(&p, &soun_hdlr()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(&v[0].format, b"sowt");
        assert_eq!(v[0].channels, 2);
        assert_eq!(v[0].bits_per_sample, 16);
        assert_eq!(v[0].sample_rate, 44100);
        // Version-0 description: no v1 fields, not VBR.
        assert_eq!(v[0].audio_version, 0);
        assert_eq!(v[0].sound_v1, None);
        assert!(!v[0].is_vbr());
    }

    /// Build an stsd payload carrying a single audio entry whose
    /// version-0 fixed body has `version`, `compression_id`, and the
    /// optional version-1 fixed-ratio fields set. `v1` supplies the four
    /// version-1 longs (and forces a 36-byte body); when `None` the body
    /// is the 20-byte version-0 form.
    fn audio_stsd(version: u16, compression_id: i16, v1: Option<[u32; 4]>) -> Vec<u8> {
        let body_len = if v1.is_some() { 36 } else { 20 };
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&1u32.to_be_bytes()); // n_entries
        let entry_size = (16 + body_len) as u32;
        p.extend_from_slice(&entry_size.to_be_bytes());
        p.extend_from_slice(b"ms\x00\x11"); // arbitrary compressed format
        p.extend_from_slice(&[0u8; 6]); // reserved
        p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        let mut body = vec![0u8; body_len];
        body[0..2].copy_from_slice(&version.to_be_bytes());
        body[8..10].copy_from_slice(&1u16.to_be_bytes()); // channels=1
        body[10..12].copy_from_slice(&16u16.to_be_bytes()); // bits=16
        body[12..14].copy_from_slice(&compression_id.to_be_bytes());
        body[16..20].copy_from_slice(&((22050u32) << 16).to_be_bytes());
        if let Some([spp, bpp, bpf, bps]) = v1 {
            body[20..24].copy_from_slice(&spp.to_be_bytes());
            body[24..28].copy_from_slice(&bpp.to_be_bytes());
            body[28..32].copy_from_slice(&bpf.to_be_bytes());
            body[32..36].copy_from_slice(&bps.to_be_bytes());
        }
        p.extend_from_slice(&body);
        p
    }

    /// Build a **version-1 stsd** carrying a single ISO
    /// `AudioSampleEntryV1` (ISO/IEC 14496-12:2015 §12.2.3.2):
    /// entry_version=1, the same 20-byte fixed body as version 0, and
    /// the given trailing boxes.
    fn iso_audio_v1_stsd(channels: u16, trailing: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(1); // stsd FullBox version = 1
        p.extend_from_slice(&[0, 0, 0]); // flags
        p.extend_from_slice(&1u32.to_be_bytes()); // n_entries
        let entry_size = (16 + 20 + trailing.len()) as u32;
        p.extend_from_slice(&entry_size.to_be_bytes());
        p.extend_from_slice(b"fLaC");
        p.extend_from_slice(&[0u8; 6]); // reserved
        p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        let mut body = vec![0u8; 20];
        body[0..2].copy_from_slice(&1u16.to_be_bytes()); // entry_version = 1
        body[8..10].copy_from_slice(&channels.to_be_bytes());
        body[10..12].copy_from_slice(&16u16.to_be_bytes());
        body[16..20].copy_from_slice(&((48000u32) << 16).to_be_bytes());
        p.extend_from_slice(&body);
        p.extend_from_slice(trailing);
        p
    }

    fn framed(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut a = Vec::with_capacity(8 + body.len());
        a.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
        a.extend_from_slice(fourcc);
        a.extend_from_slice(body);
        a
    }

    #[test]
    fn iso_audio_entry_v1_has_no_qtff_extension_bytes() {
        // §12.2.3.2: entry_version 1 in a version-1 stsd keeps the
        // 20-byte fixed body — the QTFF 16-byte CompressionInfo
        // extension must NOT be consumed, so a trailing box starting
        // at byte 20 parses as a box (here: srat).
        let mut srat_body = vec![0u8; 8]; // version+flags, rate
        srat_body[4..8].copy_from_slice(&96_000u32.to_be_bytes());
        let trailing = framed(b"srat", &srat_body);
        let p = iso_audio_v1_stsd(2, &trailing);
        let v = parse_stsd(&p, &soun_hdlr()).unwrap();
        assert!(v[0].iso_audio_entry_v1);
        assert_eq!(v[0].audio_version, 1);
        assert_eq!(v[0].sound_v1, None, "no QTFF v1 fixed-ratio fields");
        assert_eq!(v[0].sample_rate, 48000);
        assert_eq!(v[0].sampling_rate, Some(96_000));
        assert_eq!(v[0].effective_sample_rate(), 96_000);
    }

    #[test]
    fn qtff_v1_in_version0_stsd_still_wins() {
        // The QTFF SoundDescriptionV1 detection must be unaffected:
        // stsd version 0 + body version 1 keeps the 36-byte layout.
        let p = audio_stsd(1, 0, Some([1024, 384, 384, 2]));
        let v = parse_stsd(&p, &soun_hdlr()).unwrap();
        assert!(!v[0].iso_audio_entry_v1);
        assert!(v[0].sound_v1.is_some());
    }

    #[test]
    fn chnl_defined_layout_with_omitted_map() {
        // definedLayout != 0 → ChannelConfiguration + 64-bit omitted
        // bitmap (§12.2.4.2 else-branch).
        let mut body = vec![0u8; 4]; // FullBox version+flags
        body.push(ChannelLayout::CHANNEL_STRUCTURED);
        body.push(6); // definedLayout = 6 (a 23001-8 configuration)
        body.extend_from_slice(&0x0000_0000_0000_0021u64.to_be_bytes());
        let trailing = framed(b"chnl", &body);
        let p = iso_audio_v1_stsd(4, &trailing);
        let v = parse_stsd(&p, &soun_hdlr()).unwrap();
        let chnl = v[0].chnl.as_ref().expect("chnl parsed");
        assert_eq!(chnl.stream_structure, 1);
        assert_eq!(
            chnl.channels,
            Some(ChannelStructure::Defined {
                defined_layout: 6,
                omitted_channels_map: 0x21,
            })
        );
        assert_eq!(chnl.object_count, None);
    }

    #[test]
    fn chnl_explicit_positions_and_objects() {
        // definedLayout == 0 → one speaker position per channel (count
        // from the sample entry), code 126 carrying azimuth/elevation;
        // plus the object-structured half.
        let mut body = vec![0u8; 4];
        body.push(ChannelLayout::CHANNEL_STRUCTURED | ChannelLayout::OBJECT_STRUCTURED);
        body.push(0); // definedLayout = 0 → explicit positions
        body.push(2); // channel 1: position 2
        body.push(126); // channel 2: explicit
        body.extend_from_slice(&(-30i16).to_be_bytes()); // azimuth
        body.push(10u8); // elevation +10
        body.push(3); // object_count = 3
        let trailing = framed(b"chnl", &body);
        let p = iso_audio_v1_stsd(2, &trailing);
        let v = parse_stsd(&p, &soun_hdlr()).unwrap();
        let chnl = v[0].chnl.as_ref().expect("chnl parsed");
        assert_eq!(
            chnl.channels,
            Some(ChannelStructure::Explicit(vec![
                SpeakerPosition {
                    speaker_position: 2,
                    azimuth: None,
                    elevation: None,
                },
                SpeakerPosition {
                    speaker_position: 126,
                    azimuth: Some(-30),
                    elevation: Some(10),
                },
            ]))
        );
        assert_eq!(chnl.object_count, Some(3));
    }

    #[test]
    fn chnl_truncated_rows_error() {
        // Declares explicit positions but runs out of bytes mid-row.
        let mut body = vec![0u8; 4];
        body.push(ChannelLayout::CHANNEL_STRUCTURED);
        body.push(0);
        body.push(126); // explicit position missing its az/el bytes
        assert!(parse_chnl(&body, 2).is_err());
        // srat too short / bad version.
        assert!(parse_srat(&[0u8; 4]).is_err());
        assert!(parse_srat(&[1, 0, 0, 0, 0, 0, 0, 1]).is_err());
    }

    #[test]
    fn stsd_audio_v1_surfaces_fixed_ratio_fields() {
        // QTFF p. 101 version-1 sound description: the four 32-bit
        // fixed-ratio longs follow the 20-byte version-0 body.
        let p = audio_stsd(1, 0, Some([1024, 384, 384, 2]));
        let v = parse_stsd(&p, &soun_hdlr()).unwrap();
        assert_eq!(v[0].audio_version, 1);
        assert_eq!(v[0].audio_compression_id, 0);
        assert_eq!(
            v[0].sound_v1,
            Some(SoundV1 {
                samples_per_packet: 1024,
                bytes_per_packet: 384,
                bytes_per_frame: 384,
                bytes_per_sample: 2,
            })
        );
        assert!(!v[0].is_vbr());
    }

    #[test]
    fn stsd_audio_vbr_third_variant_flagged() {
        // QTFF p. 102: a version-1 description with Compression ID == -2
        // marks the VBR third variant. On-wire that is `0xFFFE`.
        let p = audio_stsd(1, -2, Some([1152, 0, 0, 2]));
        let v = parse_stsd(&p, &soun_hdlr()).unwrap();
        assert_eq!(v[0].audio_version, 1);
        assert_eq!(v[0].audio_compression_id, -2);
        assert!(v[0].is_vbr());
        // Per QTFF p. 102 only samples_per_packet + bytes_per_sample are
        // meaningful for VBR; the other two are reserved zero.
        let sv1 = v[0].sound_v1.unwrap();
        assert_eq!(sv1.samples_per_packet, 1152);
        assert_eq!(sv1.bytes_per_sample, 2);
        assert_eq!(sv1.bytes_per_packet, 0);
        assert_eq!(sv1.bytes_per_frame, 0);
    }

    #[test]
    fn stsd_audio_v1_short_body_does_not_over_read() {
        // A description declaring version 1 but whose body is only the
        // 20-byte version-0 size (the four v1 longs are absent / truncated)
        // must not over-read past the body: no SoundV1 is surfaced and the
        // extra scan starts at 20, not 36.
        let p = audio_stsd(1, 0, None); // 20-byte body, version=1
        let v = parse_stsd(&p, &soun_hdlr()).unwrap();
        assert_eq!(v[0].audio_version, 1);
        assert_eq!(v[0].sound_v1, None);
        assert!(!v[0].is_vbr());
    }

    // ─────────────── tref reference-type classification ───────────────

    #[test]
    fn track_ref_kind_classifies_iso_bmff_types() {
        assert_eq!(
            TrackRefKind::from_fourcc(b"cdsc"),
            TrackRefKind::ContentDescribes
        );
        assert_eq!(TrackRefKind::from_fourcc(b"font"), TrackRefKind::Font);
        assert_eq!(
            TrackRefKind::from_fourcc(b"hind"),
            TrackRefKind::HintDependency
        );
        assert_eq!(TrackRefKind::from_fourcc(b"vdep"), TrackRefKind::VideoDepth);
        assert_eq!(
            TrackRefKind::from_fourcc(b"vplx"),
            TrackRefKind::VideoParallax
        );
        assert_eq!(TrackRefKind::from_fourcc(b"subt"), TrackRefKind::Subtitle);
        // Pre-existing QuickTime types still classify.
        assert_eq!(TrackRefKind::from_fourcc(b"chap"), TrackRefKind::Chapter);
        assert_eq!(TrackRefKind::from_fourcc(b"hint"), TrackRefKind::Hint);
        // Unknown stays Other.
        assert_eq!(TrackRefKind::from_fourcc(b"xxxx"), TrackRefKind::Other);
    }

    fn track_with_ref(fourcc: &[u8; 4], ids: Vec<u32>) -> Track {
        Track {
            references: vec![TrackRef {
                kind: TrackRefKind::from_fourcc(fourcc),
                fourcc: *fourcc,
                track_ids: ids,
            }],
            ..Track::default()
        }
    }

    #[test]
    fn cdsc_accessor_returns_described_track_ids() {
        // A metadata track describing content track 1, with a 0-valued
        // (unused) slot that must be filtered out.
        let t = track_with_ref(b"cdsc", vec![1, 0]);
        assert_eq!(t.content_describes_track_refs(), vec![1]);
        // Other typed accessors don't pick up the cdsc reference.
        assert!(t.subtitle_track_refs().is_empty());
        assert!(t.font_track_refs().is_empty());
    }

    #[test]
    fn iso_bmff_ref_accessors_route_by_kind() {
        assert_eq!(track_with_ref(b"font", vec![3]).font_track_refs(), vec![3]);
        assert_eq!(
            track_with_ref(b"hind", vec![4, 5]).hint_dependency_track_refs(),
            vec![4, 5]
        );
        assert_eq!(
            track_with_ref(b"vdep", vec![2]).video_depth_track_refs(),
            vec![2]
        );
        assert_eq!(
            track_with_ref(b"vplx", vec![6]).video_parallax_track_refs(),
            vec![6]
        );
        assert_eq!(
            track_with_ref(b"subt", vec![7]).subtitle_track_refs(),
            vec![7]
        );
    }
}
