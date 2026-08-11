//! Write side: a non-fragmented `MovMuxer` that emits a
//! structurally-valid QuickTime / ISO BMFF file.
//!
//! Default layout (both axes configurable — [`MoovPlacement`] swaps
//! `moov` before `mdat` for the faststart web-playback order, and
//! [`ChunkStrategy`] switches the `mdat` body to the QTFF p. 358
//! time-ordered interleave):
//!
//! ```text
//! ┌─ ftyp           — major brand `qt  `, compat `qt  ` + `isom`
//! ├─ mdat           — sample bytes (default: one chunk per track,
//! │                   tracks emitted back-to-back in `add_track` order;
//! │                   interleaved: time-ordered chunks of ≤ the
//! │                   configured period, merged across tracks)
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
//! Later rounds grew the write surface well past this skeleton: edit
//! lists (`edts/elst`), composition offsets (`ctts` + `cslg`),
//! `mvex/trex` fragmentation ([`MovMuxer::with_fragmentation`]),
//! sample groups, sample-aux streams, metadata, and the layout axes
//! above. Codec-config blobs stay caller-supplied: pass pre-built
//! extension atoms as a track's `extra_stsd_atoms` (e.g. `avcC` for
//! H.264 in `avc1`); the muxer copies those bytes verbatim into the
//! trailing slot of the `stsd` entry — this is a container crate and
//! sample data is opaque.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

use crate::clip::Clipping;
use crate::gmhd::{Gmin, Tcmi};
use crate::header::Hmhd;
use crate::kind::KindEntry;
use crate::matte::Matte;
use crate::media_meta::{Clap, ColorParameters, Cslg, Fiel, Pasp, Tapt};
use crate::metadata_sample::{MetadataSampleEntry, SimpleTextSampleEntry, SubtitleSampleEntry};
use crate::sample_table::{SdtpEntry, StshEntry, SubSampleInfo};
use crate::text_sample::TextSampleDescription;
use crate::timecode::Tmcd;
use crate::track::{ChannelLayout, ChannelStructure, LpcmFlags, SoundV1};
use crate::track_group::TrackGroupTypeEntry;
use crate::track_load::Load;
use crate::track_selection::TrackSelection;
use std::io::Write;

/// One sample destined for a track. The muxer copies `data` into the
/// `mdat` body and emits the matching `stsz` size + `stts` duration +
/// `stss` keyframe-flag entries, plus a `ctts` composition-offset entry
/// when [`composition_offset`](Self::composition_offset) is non-zero.
#[derive(Clone, Debug, Default)]
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
    /// Composition-time offset (PTS − DTS) in media-timescale ticks, the
    /// per-sample `sample_offset` of the `ctts` Composition Time to
    /// Sample Box (ISO/IEC 14496-12 §8.6.1.3). Zero for every sample in
    /// a stream whose decode order equals presentation order (no
    /// B-frames); the muxer then omits the `ctts` box entirely. A
    /// non-zero value on any sample triggers a `ctts` covering the whole
    /// track. Signed: a negative offset is legal and forces the v1 form
    /// of the box (§8.6.1.3.1 — version 1 stores `sample_offset` as a
    /// signed `int(32)`); an all-non-negative track uses v0.
    pub composition_offset: i32,
}

/// Location of one sample inside an **external** media file.
///
/// QTFF's appendix "Defining Media Data Layouts" ("Using QuickTime
/// Files and Media Layouts", 2012-08-14 edition): *"A QuickTime file
/// can reference media data stored in a number of files, including the
/// file itself"* — and it addresses that media *"by file offset,
/// rather than by a data structuring mechanism of a particular file
/// format"*, so the referenced file may be any format at all (a raw
/// elementary stream, another movie's `mdat`, …). The chunk-offset
/// table of an external track stores exactly these offsets: per the
/// Chunk Offset Atoms section (QTFF p. 113) offsets are into the data
/// stream the sample description's `data_reference_index` designates,
/// and per the Media Atoms overview (p. 79) the sample tables give
/// each sample's "byte offset from the data reference".
///
/// Used by [`MovMuxer::set_external_media`]; `offset` is the sample's
/// first byte within the external file, `size` its byte length (what
/// the `stsz`/`stz2` table will carry).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExternalSampleLocation {
    /// Absolute byte offset of the sample within the external file.
    pub offset: u64,
    /// Sample length in bytes.
    pub size: u32,
}

/// Internal record of a track marked external via
/// [`MovMuxer::set_external_media`].
#[derive(Clone, Debug)]
struct ExternalMediaWrite {
    /// 1-based index into the track's custom data-reference table
    /// ([`MovMuxer::set_data_references`]) of the non-self entry the
    /// media lives behind. Every sample entry's `data_reference_index`
    /// is pointed here instead of at a self-reference.
    data_reference_index: u16,
    /// One location per sample, in sample (decode) order.
    locations: Vec<ExternalSampleLocation>,
}

/// Per-track sample-auxiliary-information stream destined for a
/// `stbl`-scope `saiz` + `saio` pair (ISO/IEC 14496-12 §8.7.8 /
/// §8.7.9).
///
/// A *sample-aux* stream carries one opaque byte blob per sample,
/// stored *outside* the sample data itself — the canonical use is the
/// ISO/IEC 23001-7 Common Encryption per-sample initialisation-vector
/// / sub-sample-encryption records, but the spec leaves the format and
/// meaning to a separate specification named by the `(aux_info_type,
/// aux_info_type_parameter)` discriminator (§8.7.8.1). The muxer
/// treats the bytes as opaque: it lays each sample's blob into `mdat`
/// (contiguously, right after the track's sample data), emits a
/// `saiz` describing the per-sample sizes, and a single-entry `saio`
/// whose absolute file offset points at the first blob (§8.7.9.3 —
/// "When in the Sample Table Box, the offsets are absolute … If
/// entry_count is one, then the Sample Auxiliary Information for all
/// Chunks … is contiguous in the file in chunk … order").
///
/// The read-side counterpart is
/// [`crate::sample_table::SampleTable::sample_aux_for`]; a file written
/// with this stream round-trips back through `MovDemuxer` with the same
/// per-sample sizes and the same discriminator pair.
#[derive(Clone, Debug)]
pub struct SampleAuxStream {
    /// `aux_info_type` discriminator (§8.7.8.3). When `None`, the
    /// muxer emits the box with the `flags & 1` bit clear, so the
    /// §8.7.8.1 implicit-fallback rules apply on read (scheme type for
    /// transformed content, sample-entry type otherwise). When `Some`,
    /// the box carries the explicit pair.
    pub aux_info_type: Option<[u8; 4]>,
    /// `aux_info_type_parameter` — the §8.7.8.3 "stream" sub-selector.
    /// Only emitted when `aux_info_type` is `Some` (the on-disk pair
    /// is gated by a single `flags & 1` bit covering both fields).
    pub aux_info_type_parameter: u32,
    /// One opaque byte blob per sample, in sample (decode) order. The
    /// length must equal the track's sample count; a zero-length blob
    /// is legal (§8.7.8.3 — "may be zero to indicate samples with no
    /// associated auxiliary information") and a sample with no blob is
    /// represented by an empty `Vec`.
    pub per_sample: Vec<Vec<u8>>,
}

/// Per-track sample-to-group assignment destined for a `stbl`-scope
/// `csgp` (CompactSampleToGroupBox, ISO/IEC 14496-12:2020 §8.9.5).
///
/// A sample-to-group mapping assigns each sample a 1-based
/// `group_description_index` into a sibling `sgpd` (Sample Group
/// Description Box) selected by `grouping_type`; index `0` means the
/// sample belongs to no group of that type. The classic carrier is
/// the run-length `sbgp` box, but ISO/IEC 14496-12:2020 added the
/// **compact** `csgp` form which replicates a small set of
/// per-sample-index *patterns* across the track. The muxer emits
/// `csgp` because it is the strictly more compact encoding of the same
/// information and its read path
/// ([`crate::sample_groups::parse_csgp`]) already expands it back to
/// the identical run-length list a `sbgp` would have produced.
///
/// The read-side counterpart is
/// [`crate::sample_table::SampleTable`]'s sample-group accessors; a
/// file written with this assignment round-trips back through
/// `MovDemuxer` with the same per-sample group-description indices.
///
/// Layout / flag-derived width selectors are documented in
/// `docs/container/isobmff/post-2015-additions.md`
/// ("`csgp` — Compact Sample to Group Box").
#[derive(Clone, Debug)]
pub struct SampleToGroupWrite {
    /// `grouping_type` FourCC (`roll`, `rap `, `prol`, …) naming the
    /// sibling `sgpd` whose descriptions these indices reference.
    pub grouping_type: [u8; 4],
    /// Optional `grouping_type_parameter` (§8.9.3). When `Some`, the
    /// emitted `csgp` sets the `grouping_type_parameter_present` flag
    /// bit and carries the 32-bit value; when `None`, the bit is clear
    /// and the field is omitted.
    pub grouping_type_parameter: Option<u32>,
    /// One 1-based `group_description_index` per sample, in sample
    /// (decode) order. The length must equal the track's sample count.
    /// A value of `0` assigns the sample to no group of this type
    /// (§8.9.3 — "the value 0 is reserved … means the sample is a
    /// member of no group"). Indices round-trip verbatim, including the
    /// `0x8000_0000` fragment-local msb if a caller chooses to set it
    /// (see [`crate::sample_groups::split_csgp_index`]).
    pub indices: Vec<u32>,
}

/// Which on-disk Sample-to-Group box carries a [`SampleToGroupWrite`].
///
/// Both forms encode the identical per-sample
/// `group_description_index` mapping and round-trip through the
/// demuxer's `SampleTable` sample-group accessors; they differ only in
/// wire encoding:
///
/// * [`Compact`](Self::Compact) — the `csgp` (CompactSampleToGroupBox,
///   ISO/IEC 14496-12:2020 §8.9.5). Replicates a small set of
///   per-sample-index *patterns* with minimum-width field selectors.
///   Smaller for repetitive mappings but a 2020 addition some older
///   readers don't parse.
/// * [`Classic`](Self::Classic) — the `sbgp` (SampleToGroupBox,
///   §8.9.2). A flat run-length table of
///   `[sample_count][group_description_index]` rows, understood by
///   every ISO BMFF reader since the box was introduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SampleGroupBoxForm {
    /// Emit a `csgp` (CompactSampleToGroupBox). The default.
    #[default]
    Compact,
    /// Emit a `sbgp` (SampleToGroupBox) — the widely-compatible
    /// run-length form.
    Classic,
}

/// The sibling Sample Group Description Box (`sgpd`, ISO/IEC 14496-12
/// §8.9.3) a [`SampleToGroupWrite`]'s indices reference.
///
/// A `csgp` / `sbgp` only carries a per-sample 1-based
/// `group_description_index`; the descriptions those indices select
/// live in a separate `sgpd` of the matching `grouping_type`. Without
/// the `sgpd` a non-zero index points at nothing and the file is
/// non-conformant (§8.9.3 — "The associated SampleToGroup shall
/// indicate the same value for the grouping type"). This struct is the
/// write-side counterpart that emits it.
///
/// Entries are 1-based: `entries[0]` is `group_description_index == 1`.
/// Each entry is the raw `SampleGroupEntry` payload for the
/// `grouping_type` (the §10 typed-entry constructors below build the
/// standard ones). The box is written **version 1** with a
/// `default_length` equal to the common entry length when every entry
/// is the same size, or with `default_length == 0` plus a per-entry
/// `description_length` prefix when the lengths vary (§8.9.3.2 — the
/// recommended encoding; version 0 is deprecated because its entries
/// carry no size).
///
/// The read-side counterpart is
/// [`crate::sample_groups::parse_sgpd`] /
/// [`crate::sample_groups::SampleGroupDescription`]; a file written
/// with this round-trips back through `MovDemuxer` with the same
/// per-entry payloads, and the typed decoders
/// ([`crate::sample_groups::decode_roll`], …) recover the structured
/// fields.
#[derive(Clone, Debug)]
pub struct SampleGroupDescriptionWrite {
    /// `grouping_type` FourCC — must equal the matching
    /// [`SampleToGroupWrite::grouping_type`].
    pub grouping_type: [u8; 4],
    /// One raw `SampleGroupEntry` payload per group description, in
    /// `group_description_index` order (entry 0 ⇒ index 1). Use the
    /// `roll_entry` / `prol_entry` / `rap_entry` / `tele_entry` /
    /// `sap_entry` constructors for the standard §10 types, or supply
    /// raw bytes for a codec-specific grouping.
    pub entries: Vec<Vec<u8>>,
}

impl SampleGroupDescriptionWrite {
    /// A `sgpd` of `grouping_type` with the given raw entry payloads
    /// (one per `group_description_index`, in 1-based order).
    pub fn new(grouping_type: [u8; 4], entries: Vec<Vec<u8>>) -> Self {
        Self {
            grouping_type,
            entries,
        }
    }

    /// A `'roll'` VisualRollRecoveryEntry / AudioRollRecoveryEntry
    /// payload (§10.1.1.2): `signed int(16) roll_distance`. Round-trips
    /// through [`crate::sample_groups::decode_roll`].
    pub fn roll_entry(roll_distance: i16) -> Vec<u8> {
        roll_distance.to_be_bytes().to_vec()
    }

    /// A `'prol'` AudioPreRollEntry payload (§10.1.1.2):
    /// `signed int(16) roll_distance`. Round-trips through
    /// [`crate::sample_groups::decode_prol`].
    pub fn prol_entry(roll_distance: i16) -> Vec<u8> {
        roll_distance.to_be_bytes().to_vec()
    }

    /// A `'rap '` VisualRandomAccessEntry payload (§10.4.2):
    /// `1 bit num_leading_samples_known | 7 bits num_leading_samples`.
    /// Round-trips through [`crate::sample_groups::decode_rap`].
    /// `num_leading_samples` is masked to 7 bits.
    pub fn rap_entry(num_leading_samples_known: bool, num_leading_samples: u8) -> Vec<u8> {
        let b = (u8::from(num_leading_samples_known) << 7) | (num_leading_samples & 0x7F);
        vec![b]
    }

    /// A `'tele'` TemporalLevelEntry payload (§10.5.2):
    /// `1 bit level_independently_decodable | 7 bits reserved`.
    /// Round-trips through [`crate::sample_groups::decode_tele`].
    pub fn tele_entry(level_independently_decodable: bool) -> Vec<u8> {
        vec![u8::from(level_independently_decodable) << 7]
    }

    /// A `'sap '` SAPEntry payload (§10.6.2):
    /// `1 bit dependent_flag | 3 bits reserved | 4 bits SAP_type`.
    /// Round-trips through [`crate::sample_groups::decode_sap`].
    /// `sap_type` is masked to 4 bits.
    pub fn sap_entry(dependent_flag: bool, sap_type: u8) -> Vec<u8> {
        let b = (u8::from(dependent_flag) << 7) | (sap_type & 0x0F);
        vec![b]
    }
}

/// How a track's Composition to Decode Box (`cslg`, ISO/IEC 14496-12
/// §8.6.1.4) is sourced when [`MovMuxer::set_cslg`] /
/// [`MovMuxer::auto_cslg`] is used.
///
/// `cslg` summarises the composition-vs-decode timeline of a track that
/// carries a `ctts` (B-frame reorder), letting a player derive the
/// presentation-timeline bounds without scanning every `ctts` run
/// (§8.6.1.4.1). It is written into the `stbl` right after the `ctts`
/// box (§6.2.3 box order).
#[derive(Clone, Copy, Debug)]
enum CslgWrite {
    /// Derive all five fields from the track's per-sample composition
    /// offsets + durations at layout time (see [`derive_cslg`]).
    Auto,
    /// Use the caller-supplied bounds verbatim.
    Explicit(Cslg),
}

/// One entry of a track's edit list, destined for an `edts > elst`
/// (Edit List Box, QTFF p. 47 / ISO/IEC 14496-12 §8.6.6).
///
/// An edit list maps movie-timescale presentation time to
/// media-timescale sample time, one segment at a time. The classic
/// uses are: an **empty edit** at the head of an audio track to skip
/// encoder priming/delay samples (`media_time = -1`), a **start
/// offset** that presents the track from a non-zero media time
/// (`media_time > 0`), and a **dwell** that holds one media frame for a
/// span of movie time (`media_rate = 0`).
///
/// The read-side counterpart is [`crate::edit::Edit`] /
/// [`crate::edit::parse_elst`]; a file written with these entries
/// round-trips back through `MovDemuxer` with the same per-segment
/// `track_duration` / `media_time` / `media_rate`.
#[derive(Clone, Copy, Debug)]
pub struct MuxEdit {
    /// Duration of this segment in **movie-timescale** units (the
    /// [`MovMuxer::with_movie_timescale`] scale, not the track's media
    /// timescale). QTFF p. 47: "the duration of this edit segment in
    /// units of the movie's time scale."
    pub track_duration: u64,
    /// Starting time within the media of this segment, in
    /// **media-timescale** units. `-1` marks an *empty edit* (QTFF
    /// p. 47): movie time advances with no media presented. Any other
    /// negative value is rejected by [`MovMuxer::set_edit_list`].
    pub media_time: i64,
    /// Relative playback rate as a 16.16 signed fixed-point number
    /// (`0x0001_0000` = unity 1.0×). `0` marks a *dwell* (hold the
    /// frame at `media_time`); QTFF p. 48 otherwise requires a
    /// strictly-positive rate. Zero and any positive value are
    /// written verbatim; negative rates are rejected by
    /// [`MovMuxer::set_edit_list`]. See [`MuxEdit::dwell`] /
    /// [`MuxEdit::segment_with_rate`].
    pub media_rate: i32,
}

impl MuxEdit {
    /// A normal unity-rate segment presenting media from `media_time`
    /// (media-timescale ticks) for `track_duration` (movie-timescale
    /// ticks).
    pub fn segment(track_duration: u64, media_time: i64) -> Self {
        Self {
            track_duration,
            media_time,
            media_rate: 0x0001_0000,
        }
    }

    /// An empty edit: advance movie time by `track_duration`
    /// (movie-timescale ticks) presenting no media. Used at the head of
    /// a track to delay its start relative to the movie, or to skip
    /// audio encoder-priming samples.
    pub fn empty(track_duration: u64) -> Self {
        Self {
            track_duration,
            media_time: -1,
            media_rate: 0x0001_0000,
        }
    }

    /// A *dwell*: hold the single media frame at `media_time`
    /// (media-timescale ticks) on screen for `track_duration`
    /// (movie-timescale ticks). The on-wire signal is `media_rate ==
    /// 0` (ISO/IEC 14496-12 §8.6.6.3; QTFF p. 48 declares 0 illegal
    /// but QuickTime-lineage files carry it — the read side
    /// classifies it as [`crate::edit::EditSegmentKind::Dwell`]
    /// either way).
    pub fn dwell(track_duration: u64, media_time: i64) -> Self {
        Self {
            track_duration,
            media_time,
            media_rate: 0,
        }
    }

    /// A presentation segment at an arbitrary 16.16 fixed-point
    /// `media_rate` ([`crate::edit::MEDIA_RATE_ONE`] = 1.0×): the
    /// segment consumes `rate × track_duration`-worth of media across
    /// `track_duration` movie ticks (QTFF Chapter 5 "Playing With
    /// Edit Lists" pp. 226–227 — 600 movie ticks at rate 2.0 consume
    /// 1200 media ticks). Rates must be strictly positive here; use
    /// [`MuxEdit::dwell`] for the rate-0 hold and [`MuxEdit::empty`]
    /// for the no-media edit ([`MovMuxer::set_edit_list`] rejects
    /// negative rates, which QTFF p. 48 forbids).
    pub fn segment_with_rate(track_duration: u64, media_time: i64, media_rate: i32) -> Self {
        Self {
            track_duration,
            media_time,
            media_rate,
        }
    }
}

/// Write-side configuration for an ISO BMFF `AudioSampleEntryV1`
/// (ISO/IEC 14496-12:2015 §12.2.3.2), set via
/// [`MovMuxer::set_audio_entry_v1`]. The fixed 20-byte body keeps the
/// version-0 shape (`entry_version` = 1 in its first two bytes); the
/// optional boxes below follow the codec-config `extra_stsd_atoms` in
/// the entry's trailing box area.
#[derive(Clone, Debug, Default)]
pub struct AudioEntryV1 {
    /// Emit a `srat` SamplingRateBox carrying the *actual* sampling
    /// rate (use when it exceeds what the 16.16 `samplerate` field
    /// can represent — the field then keeps a suitable integer
    /// multiple/division per §12.2.3.3, here the `MuxTrackKind::Audio`
    /// `sample_rate`). `None` ⇒ no `srat`.
    pub sampling_rate: Option<u32>,
    /// Emit a `chnl` ChannelLayout box (§12.2.4) documenting the
    /// channel/object assignment. An
    /// [`ChannelStructure::Explicit`](crate::ChannelStructure) layout
    /// must carry exactly one row per `MuxTrackKind::Audio` channel
    /// (the read-side loop count comes from the sample entry).
    /// `None` ⇒ no `chnl`.
    pub channel_layout: Option<ChannelLayout>,
}

/// Write-side configuration for a QTFF **Sound Sample Description
/// version 2** (QTFF 2012-08-14 pp. 181–182), set via
/// [`MovMuxer::set_sound_description_v2`]. The muxer derives the
/// remaining wire fields: the `always*` back-compatibility constants,
/// `sizeOfStructOnly` = 72 (extensions start right after the fixed
/// struct), and `numAudioChannels` from the track's channel count.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioEntryV2 {
    /// Audio frames per second (`audioSampleRate`, Float64) — use
    /// this form when the rate exceeds the 16.16 field's 65535 Hz
    /// cap or is not an integer. Must be finite and positive.
    pub audio_sample_rate: f64,
    /// Bits per channel, only if constant and only for uncompressed
    /// audio; `0` otherwise (`constBitsPerChannel`).
    pub const_bits_per_channel: u32,
    /// LPCM flag values (`formatSpecificFlags`) — see
    /// [`LpcmFlags`](crate::LpcmFlags).
    pub format_specific_flags: LpcmFlags,
    /// Bytes per audio packet, only if constant; else `0`
    /// (`constBytesPerAudioPacket`).
    pub const_bytes_per_audio_packet: u32,
    /// LPCM frames per audio packet, only if constant; else `0`
    /// (`constLPCMFramesPerAudioPacket`).
    pub const_lpcm_frames_per_audio_packet: u32,
}

/// Which audio sample-description layout a track's `stsd` entry uses.
#[derive(Clone, Debug, Default)]
enum AudioDescriptionWrite {
    /// QTFF p. 100 version-0 (20-byte fixed body). The default.
    #[default]
    V0,
    /// QTFF p. 101 `SoundDescriptionV1`: version 1 plus the four
    /// 32-bit fixed-compression-ratio fields; `vbr` selects the
    /// p. 102 "third variant" (Compression ID `-2`).
    QtffV1 { fields: SoundV1, vbr: bool },
    /// QTFF 2012-08-14 pp. 181–182 `SoundDescriptionV2`: version 2,
    /// the fixed `always*` constants in the version-0 positions,
    /// Float64 sample rate and LPCM descriptors appended (56-byte
    /// fixed body).
    QtffV2(AudioEntryV2),
    /// ISO/IEC 14496-12:2015 §12.2.3 `AudioSampleEntryV1`
    /// (`entry_version` = 1 inside a version-1 `stsd`, optional
    /// `srat` / `chnl` boxes).
    IsoV1(AudioEntryV1),
}

/// One write-side user-data metadata item, emitted into a `udta`
/// (User Data Box, QTFF pp. 36–38 / ISO/IEC 14496-12 §8.10.1).
///
/// Three shapes match the read-side [`crate::user_data::UserDataKind`]
/// so a file written with these items round-trips through
/// [`crate::user_data::parse_udta`] (and surfaces on
/// [`crate::demuxer::MovDemuxer::user_data`] for movie-level items or
/// [`crate::demuxer::Track::user_data`] for track-level items):
///
/// * [`MovMetadata::intl_text`] — an Apple international-text record
///   (`©XXX`). The on-disk layout is one or more
///   `[text_size: u16][language: u16][text: text_size bytes]` records
///   inside a single `©XXX` atom (QTFF p. 38). Multiple `intl_text`
///   items sharing the same FourCC are coalesced into **one** atom
///   carrying one record per language, preserving insertion order.
///   `language` is written verbatim: a Macintosh language code
///   (`< 0x8000`, QTFF p. 197) selects Mac-Roman decoding on read,
///   while a 5-bit-packed ISO 639-2/T tag OR'd with `0x8000` selects
///   UTF-8 (see [`MovMetadata::iso_language`]).
/// * [`MovMetadata::plain_utf8`] — a QuickTime-7+ UTF-8 entry
///   (`name` / `auth` / `cprt`). Layout is a FullBox header
///   (`[ver:1][flags:3]`), a 16-bit packed ISO 639-2/T language tag,
///   then the UTF-8 text with no terminator.
/// * [`MovMetadata::raw`] — an arbitrary FourCC carrying opaque bytes,
///   surfaced on read as [`crate::user_data::UserDataKind::Unknown`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MovMetadata {
    fourcc: [u8; 4],
    payload: MetaPayload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MetaPayload {
    /// Apple international-text: one `[text_size][lang][text]` record.
    /// Coalesced with sibling records sharing the same FourCC at build.
    IntlText { language: u16, text: String },
    /// QT-7+ FullBox + packed-ISO-lang + UTF-8.
    PlainUtf8 { language: u16, text: String },
    /// Opaque bytes under an arbitrary FourCC.
    Raw(Vec<u8>),
}

impl MovMetadata {
    /// An Apple international-text user-data item (`©XXX`).
    ///
    /// `fourcc` is the 4-byte atom type whose first byte is the
    /// Mac-Roman `©` glyph (`0xA9`) — e.g. `[0xA9, b'n', b'a', b'm']`
    /// for the title (`©nam`), `©cpy`, `©day`, `©ART`, … `language` is
    /// the Macintosh language code (QTFF p. 197). For a UTF-8 text body
    /// pass a 5-bit-packed ISO 639-2/T tag with the high bit set; see
    /// [`MovMetadata::iso_language`] for the packing.
    pub fn intl_text(fourcc: [u8; 4], language: u16, text: impl Into<String>) -> Self {
        Self {
            fourcc,
            payload: MetaPayload::IntlText {
                language,
                text: text.into(),
            },
        }
    }

    /// A QuickTime-7+ plain-UTF-8 user-data item. `fourcc` is one of
    /// `b"name"`, `b"auth"`, `b"cprt"` (the read path only decodes
    /// these three as UTF-8; any other FourCC surfaces as `Unknown`).
    /// `language` is a 5-bit-packed ISO 639-2/T tag (see
    /// [`MovMetadata::iso_language`]).
    pub fn plain_utf8(fourcc: [u8; 4], language: u16, text: impl Into<String>) -> Self {
        Self {
            fourcc,
            payload: MetaPayload::PlainUtf8 {
                language,
                text: text.into(),
            },
        }
    }

    /// An arbitrary user-data item carrying opaque bytes under `fourcc`.
    /// Surfaces on read as [`crate::user_data::UserDataKind::Unknown`].
    pub fn raw(fourcc: [u8; 4], bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            fourcc,
            payload: MetaPayload::Raw(bytes.into()),
        }
    }

    /// Pack a three-letter ISO 639-2/T language tag (e.g. `*b"eng"`,
    /// `*b"fra"`) into the 5-bit-per-character form QTFF / ISO BMFF use
    /// for `mdhd.language` and QuickTime-7 user-data entries. The result
    /// is the **bare** packed value (no high bit), the exact inverse of
    /// [`crate::user_data::iso_language_tag`]; each byte must be a
    /// lowercase ASCII letter (`a..=z`).
    ///
    /// Use it directly as the `language` of a [`MovMetadata::plain_utf8`]
    /// item (always UTF-8, no flag bit). For a UTF-8-bodied
    /// [`MovMetadata::intl_text`] item, OR in the [`UTF8_INTL_TEXT_FLAG`]
    /// (`0x8000`) so the read path decodes the body as UTF-8 rather than
    /// Mac-Roman; a Macintosh language code (`< 0x8000`, QTFF p. 197)
    /// selects Mac-Roman instead.
    pub fn iso_language(tag: [u8; 3]) -> u16 {
        let c1 = (tag[0].wrapping_sub(0x60) & 0x1F) as u16;
        let c2 = (tag[1].wrapping_sub(0x60) & 0x1F) as u16;
        let c3 = (tag[2].wrapping_sub(0x60) & 0x1F) as u16;
        (c1 << 10) | (c2 << 5) | c3
    }
}

/// High-bit flag OR'd into an international-text record's language slot
/// to mark its body as UTF-8 (rather than Mac-Roman) — QTFF p. 38 /
/// the read-side `parse_intl_text` heuristic (`language >= 0x8000`).
/// Combine with [`MovMetadata::iso_language`] for a UTF-8 `©XXX` body.
pub const UTF8_INTL_TEXT_FLAG: u16 = 0x8000;

/// Apple `ilst` `data` sub-atom **type-indicator** for a UTF-8 text
/// value (the "well-known type" set; QuickTime Metadata format). The
/// read-side [`crate::media_meta::MetaKeyValue::as_str`] decodes a
/// value as UTF-8 only when its `type_code` equals this.
pub const META_TYPE_UTF8: u32 = 1;
/// Apple `ilst` `data` type-indicator for a big-endian signed integer
/// (the value width — 1 / 2 / 4 / 8 bytes — is the `data` payload
/// length). Used by e.g. `com.apple.quicktime.…` numeric keys.
pub const META_TYPE_BE_SIGNED_INT: u32 = 21;
/// Apple `ilst` `data` type-indicator for a big-endian unsigned
/// integer (value width = payload length).
pub const META_TYPE_BE_UNSIGNED_INT: u32 = 22;
/// Apple `ilst` `data` type-indicator for raw / undefined bytes —
/// an opaque blob with no further structure imposed by the format.
pub const META_TYPE_RAW: u32 = 0;

/// The default `meta`-atom key namespace (Apple QuickTime Metadata
/// format). Almost every real-world `moov/meta` key is declared in the
/// `mdta` (metadata) namespace; reverse-DNS key names
/// (`com.apple.quicktime.title`, `com.android.version`, …) live here.
pub const META_NAMESPACE_MDTA: [u8; 4] = *b"mdta";

/// One movie-level Apple **QuickTime Metadata** key-value item, emitted
/// into a `moov/meta` box (`hdlr` `mdta` + `keys` + `ilst`) by
/// [`MovMuxer::set_apple_metadata`].
///
/// This is the modern QuickTime / iTunes-style key-value metadata shape
/// (distinct from the legacy `udta` User Data Box driven by
/// [`MovMetadata`]). Each item carries:
///
/// * a 4-byte key **namespace** (`namespace`, typically
///   [`META_NAMESPACE_MDTA`]),
/// * a UTF-8 **key** name (`key`, e.g. `"com.apple.quicktime.title"`),
/// * a typed **value** — its on-disk `data` sub-atom `type_code`
///   (one of [`META_TYPE_UTF8`] / [`META_TYPE_BE_SIGNED_INT`] /
///   [`META_TYPE_BE_UNSIGNED_INT`] / [`META_TYPE_RAW`], or any caller-
///   supplied indicator) plus the raw value bytes.
///
/// A file written with these items round-trips through the read-side
/// [`crate::media_meta::parse_keys`] / [`crate::media_meta::parse_ilst`]
/// and surfaces on [`crate::demuxer::MovDemuxer::meta`] as a
/// [`crate::media_meta::MetaKeyValue`] with the same `namespace`, `key`,
/// `type_code`, and `value`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MovMetaItem {
    namespace: [u8; 4],
    key: String,
    type_code: u32,
    value: Vec<u8>,
}

impl MovMetaItem {
    /// A UTF-8 text item in the `mdta` namespace — the common case
    /// (`type_code` = [`META_TYPE_UTF8`]). `key` is the reverse-DNS key
    /// name (e.g. `"com.apple.quicktime.title"`); `text` is the value.
    pub fn utf8(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            namespace: META_NAMESPACE_MDTA,
            key: key.into(),
            type_code: META_TYPE_UTF8,
            value: text.into().into_bytes(),
        }
    }

    /// A big-endian signed-integer item in the `mdta` namespace
    /// (`type_code` = [`META_TYPE_BE_SIGNED_INT`]). The value is written
    /// as a 4-byte big-endian `i32` — Apple permits 1/2/4/8-byte widths;
    /// this constructor emits the 32-bit form. Use
    /// [`MovMetaItem::typed`] for an explicit width.
    pub fn signed_int(key: impl Into<String>, value: i32) -> Self {
        Self {
            namespace: META_NAMESPACE_MDTA,
            key: key.into(),
            type_code: META_TYPE_BE_SIGNED_INT,
            value: value.to_be_bytes().to_vec(),
        }
    }

    /// A fully-explicit item: caller supplies the key `namespace`, the
    /// UTF-8 `key` name, the `data` `type_code` (well-known-type
    /// indicator), and the raw value `bytes`. Use this for namespaces
    /// other than `mdta`, or value type codes outside the
    /// [`META_TYPE_UTF8`] / `..._INT` / [`META_TYPE_RAW`] set.
    pub fn typed(
        namespace: [u8; 4],
        key: impl Into<String>,
        type_code: u32,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            namespace,
            key: key.into(),
            type_code,
            value: bytes.into(),
        }
    }

    /// The key namespace (4 bytes, typically [`META_NAMESPACE_MDTA`]).
    pub fn namespace(&self) -> [u8; 4] {
        self.namespace
    }

    /// The UTF-8 key name.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The `data` sub-atom type-indicator for the value.
    pub fn type_code(&self) -> u32 {
        self.type_code
    }

    /// The raw value bytes.
    pub fn value(&self) -> &[u8] {
        &self.value
    }
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
    /// Time-code track (QTFF pp. 106–116). Emits `hdlr.component_subtype
    /// = tmcd`, a `gmhd` base-media-information header (a `gmin` plus a
    /// `tmcd > tcmi` time-code media-information atom), and a `stsd`
    /// whose single `tmcd` entry carries the timing fields (`time_scale`
    /// / `frame_duration` / `number_of_frames` / flags). Each sample's
    /// `mdat` payload is a 4-byte packed timecode value (counter or
    /// `[H:M:S:F]` record). Typically referenced by a media track's
    /// `tref/tmcd`.
    Timecode {
        /// The `tmcd` sample description (timing fields + flags +
        /// optional source-tape name). Serialised via
        /// [`Tmcd::to_sample_description_body`].
        description: Tmcd,
        /// The `gmhd/tmcd/tcmi` time-code media-information atom
        /// (text-overlay font / colours / name). Serialised via
        /// [`Tcmi::to_body_bytes`].
        tcmi: Tcmi,
    },
    /// QuickTime **text** track (QTFF pp. 108–110), the carrier for
    /// chapter tracks. Emits `hdlr.component_subtype = text`, a `gmhd`
    /// base-media header with an identity-matrix `text` media-
    /// information atom, and a `stsd` whose single `text` entry carries
    /// the [`TextSampleDescription`] (display flags / justification /
    /// colours / font). Each sample's `mdat` payload is a `[length:u16]
    /// [UTF-8 text]` record (build it with
    /// [`crate::chapter::encode_text_sample`]). A chapter track is one
    /// of these referenced by a media track's `tref/chap`.
    Text {
        /// The `text` sample description (display config). Serialised via
        /// [`TextSampleDescription::to_body_bytes`].
        description: TextSampleDescription,
    },
    /// ISO BMFF **timed-metadata** track (ISO/IEC 14496-12 §12.3). Emits
    /// `hdlr.component_subtype = meta`, a `nmhd` Null Media Header Box
    /// (§8.4.5.2 — metadata tracks carry no specific media header), and a
    /// `stsd` whose single entry is a `metx` / `mett` / `urim`
    /// [`MetadataSampleEntry`] (the FourCC is taken from the variant).
    /// Each sample's `mdat` payload is the opaque per-sample metadata
    /// record (an "I-frame" carrying the complete metadata for its time
    /// interval, §12.3.3.1). A media track typically references it via
    /// `tref/cdsc`.
    Metadata {
        /// The `metx` / `mett` / `urim` sample entry. Serialised via
        /// [`MetadataSampleEntry::to_body_bytes`]; its `format()` selects
        /// the `stsd` entry FourCC.
        description: MetadataSampleEntry,
    },
    /// ISO BMFF **subtitle** track (ISO/IEC 14496-12 §12.6). Emits
    /// `hdlr.component_subtype = subt`, a `sthd` Subtitle Media Header Box
    /// (§12.6.2), and a `stsd` whose single entry is a `stpp` (XML, e.g.
    /// TTML) or `sbtt` (text) [`SubtitleSampleEntry`] (the FourCC is taken
    /// from the variant). Each sample's `mdat` payload is the opaque
    /// per-sample subtitle document. Distinct from the QuickTime
    /// [`MuxTrackKind::Text`] chapter / overlay track.
    Subtitle {
        /// The `stpp` / `sbtt` sample entry. Serialised via
        /// [`SubtitleSampleEntry::to_body_bytes`]; its `format()` selects
        /// the `stsd` entry FourCC.
        description: SubtitleSampleEntry,
    },
    /// ISO BMFF **timed-text** track (ISO/IEC 14496-12 §12.5). Emits
    /// `hdlr.component_subtype = text`, a `nmhd` Null Media Header Box
    /// (§12.5.2 — timed-text tracks use a null media header, *not* the
    /// QuickTime `gmhd` of [`MuxTrackKind::Text`]), and a `stsd` whose
    /// single entry is a `stxt` [`SimpleTextSampleEntry`]. Each sample's
    /// `mdat` payload is the opaque per-sample text document. The
    /// `stxt`/`nmhd` shape distinguishes it from the QuickTime `text`
    /// chapter/overlay track (which carries `gmhd` + a `text`
    /// description) — the demuxer disambiguates the two by the `stsd`
    /// FourCC (`stxt` vs `text`).
    SimpleText {
        /// The `stxt` sample entry. Serialised via
        /// [`SimpleTextSampleEntry::to_body_bytes`].
        description: SimpleTextSampleEntry,
    },
    /// ISO BMFF **hint** track (ISO/IEC 14496-12 §12.4), a streaming-
    /// server packetization track. Emits `hdlr.component_subtype = hint`,
    /// an `hmhd` Hint Media Header Box (§12.4.2 — protocol-independent PDU
    /// buffering metadata), and a `stsd` whose single entry is a
    /// protocol-named HintSampleEntry (§12.4.3 — the FourCC is the
    /// `protocol` identifier such as `rtp ` / `srtp`, body is opaque
    /// protocol-specific declarative data). Each sample's `mdat` payload
    /// is the opaque per-packet hint record. A hint track references the
    /// media track it packetizes via `tref/hint`.
    Hint {
        /// The hint sample entry's protocol FourCC (e.g. `*b"rtp "`).
        protocol: [u8; 4],
        /// Opaque protocol-specific sample-description body (already
        /// framed boxes or raw bytes), placed after the universal 16-byte
        /// SampleEntry header. Empty for a bare entry.
        description: Vec<u8>,
        /// The Hint Media Header Box fields (max/avg PDU size + bitrate).
        /// Serialised via [`Hmhd::to_body_bytes`].
        hmhd: Hmhd,
    },
}

/// Typed visual sample-description extension boxes for a video track
/// (ISO/IEC 14496-12 §12.1.4 / §12.1.5, QTFF p. 94 Table 3-2).
///
/// Attach via [`MovMuxer::set_visual_extensions`]. Each populated field
/// emits its box into the trailing slot of the video `stsd` entry —
/// after the 70-byte fixed body and after the codec-config
/// `extra_stsd_atoms` (so a decoder's `avcC` / `hvcC` stays first, the
/// conventional ordering). Every field defaults to `None`, in which
/// case its box is omitted. The emitted boxes round-trip back through
/// the demuxer's `scan_video_extensions` onto
/// [`crate::track::SampleDescription`]'s `pasp` / `colr` / `clap` /
/// `fiel` / `gamma` fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VisualExtensions {
    /// Pixel Aspect Ratio (`pasp`, §12.1.4.2).
    pub pasp: Option<Pasp>,
    /// Colour Information (`colr`, Apple `nclc` / ISO `nclx` / ICC,
    /// §12.1.5).
    pub colr: Option<ColorParameters>,
    /// Clean Aperture (`clap`, §12.1.4).
    pub clap: Option<Clap>,
    /// Field Handling (`fiel`, QTFF p. 94 Table 3-2). QuickTime-only.
    pub fiel: Option<Fiel>,
    /// Gamma (`gama`, QTFF p. 94 Table 3-2) as a 16.16 fixed-point
    /// value — the same `u32` representation the demuxer surfaces on
    /// `SampleDescription::gamma`.
    pub gamma: Option<u32>,
}

impl VisualExtensions {
    /// True when no extension box would be emitted.
    pub fn is_empty(&self) -> bool {
        self.pasp.is_none()
            && self.colr.is_none()
            && self.clap.is_none()
            && self.fiel.is_none()
            && self.gamma.is_none()
    }

    /// Serialise the populated extensions into a framed
    /// `[size:u32 BE][type:[u8;4]][body]` blob, in a fixed canonical
    /// order (`colr`, `pasp`, `clap`, `fiel`, `gama`). Box order inside
    /// a sample entry is not significant to a conformant reader, so a
    /// stable order is chosen for deterministic output.
    fn to_framed_atoms(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut push = |fourcc: &[u8; 4], body: &[u8]| {
            let size = (8 + body.len()) as u32;
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(fourcc);
            out.extend_from_slice(body);
        };
        if let Some(c) = &self.colr {
            push(b"colr", &c.to_body_bytes());
        }
        if let Some(p) = &self.pasp {
            push(b"pasp", &p.to_body_bytes());
        }
        if let Some(c) = &self.clap {
            push(b"clap", &c.to_body_bytes());
        }
        if let Some(f) = &self.fiel {
            push(b"fiel", &f.to_body_bytes());
        }
        if let Some(g) = self.gamma {
            push(b"gama", &g.to_be_bytes());
        }
        out
    }
}

impl TrackWrite {
    /// Byte length of sample `i` as the `stsz` / `stz2` table must
    /// report it: the external location's declared size for a track
    /// marked external ([`MovMuxer::set_external_media`] — such
    /// samples carry no in-file `data`), else the in-file payload
    /// length. `set_external_media` validates the location count
    /// against the sample count, so the index is in range for both
    /// sources.
    fn sample_size(&self, i: usize) -> u32 {
        match &self.external_media {
            Some(ext) => ext.locations[i].size,
            None => self.samples[i].data.len() as u32,
        }
    }
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
    /// Optional `stbl`-scope sample-auxiliary-information stream
    /// (ISO/IEC 14496-12 §8.7.8 / §8.7.9). When present, the muxer
    /// lays the per-sample blobs into `mdat` after the track's sample
    /// data and emits a matching `saiz` + single-entry `saio` pair.
    sample_aux: Option<SampleAuxStream>,
    /// Optional `stbl`-scope sample-to-group assignments (ISO/IEC
    /// 14496-12 §8.9.2 / §8.9.5). Each entry emits one `csgp`
    /// (CompactSampleToGroupBox) or `sbgp` (SampleToGroupBox) per its
    /// paired [`SampleGroupBoxForm`]; multiple entries (distinct
    /// `grouping_type`s) emit one box each, in insertion order.
    sample_to_groups: Vec<(SampleToGroupWrite, SampleGroupBoxForm)>,
    /// Optional `stbl`-scope sample-group **descriptions** (ISO/IEC
    /// 14496-12 §8.9.3). Each entry emits one `sgpd`
    /// (SampleGroupDescriptionBox) — the sibling box whose typed entries
    /// the `csgp`/`sbgp` per-sample indices reference. One `sgpd` per
    /// `grouping_type`, written before the `csgp` boxes per §8.9.3's
    /// containment order. Empty ⇒ no `sgpd` (a `csgp` then only carries
    /// the index mapping with the description supplied out-of-band).
    sample_group_descriptions: Vec<SampleGroupDescriptionWrite>,
    /// Optional `stbl`-scope Composition to Decode Box (`cslg`, ISO/IEC
    /// 14496-12 §8.6.1.4). When set the muxer emits a `cslg` after the
    /// `ctts` box, either auto-derived from the track's per-sample
    /// composition offsets + durations or carrying explicit bounds.
    /// `None` ⇒ no `cslg`.
    cslg: Option<CslgWrite>,
    /// Optional edit list (QTFF p. 47 / ISO/IEC 14496-12 §8.6.6). When
    /// non-empty, the muxer emits an `edts > elst` between `tkhd` and
    /// `mdia` inside the track's `trak`. Empty ⇒ no `edts` box (the
    /// implicit "entire media is used" default per QTFF p. 46).
    edits: Vec<MuxEdit>,
    /// Optional track-level user-data items (QTFF pp. 36–38 / ISO/IEC
    /// 14496-12 §8.10.1). When non-empty the muxer emits a `udta` as the
    /// last child of this track's `trak`. Empty ⇒ no `udta` box.
    metadata: Vec<MovMetadata>,
    /// Optional track-level Apple QuickTime Metadata items (the modern
    /// `trak/meta` = `hdlr` `mdta` + `keys` + `ilst` shape). When
    /// non-empty the muxer emits a `meta` as a trailing child of this
    /// track's `trak` (after `udta`). Empty ⇒ no `meta` box.
    apple_metadata: Vec<MovMetaItem>,
    /// Optional typed visual sample-description extension boxes
    /// (`pasp` / `colr` / `clap` / `fiel` / `gama`, ISO/IEC 14496-12
    /// §12.1.4 / §12.1.5, QTFF p. 94). Only meaningful for a video
    /// track; ignored on audio. Empty ⇒ no extension boxes.
    visual_extensions: VisualExtensions,
    /// Optional track-reference declarations (QTFF p. 50 / ISO/IEC
    /// 14496-12 §8.3.3 — Track Reference Box). When non-empty the muxer
    /// emits a `tref` between this track's `tkhd`/`edts` and its `mdia`,
    /// one child atom per [`TrackReference`] (FourCC = reference type,
    /// body = packed `u32` referenced track ids). Empty ⇒ no `tref`.
    track_references: Vec<TrackReference>,
    /// Optional Track Aperture Modes box (`tapt`, Apple "Movie Atoms").
    /// When `Some` the muxer emits a `tapt` (with whichever of `clef` /
    /// `prof` / `enof` children are populated) as a `trak` child. Only
    /// meaningful for a video track. `None` ⇒ no `tapt`.
    tapt: Option<Tapt>,
    /// Optional custom data-reference table (QTFF p. 65 / ISO/IEC
    /// 14496-12 §8.7.2). When empty the muxer writes the default single
    /// self-referencing `url ` entry (`flags=1`). When non-empty it
    /// writes exactly these entries (one of which must be the self-ref
    /// the sample entries point at). Set via
    /// [`MovMuxer::set_data_references`].
    data_references: Vec<DataReferenceWrite>,
    /// When `Some`, this track's media bytes live in an **external**
    /// file (QTFF appendix "Defining Media Data Layouts") — the muxer
    /// writes none of its sample bytes into this file's `mdat`, the
    /// chunk-offset table carries the caller-supplied offsets into the
    /// external file, and every sample entry's `data_reference_index`
    /// points at the designated non-self `dref` entry. Set via
    /// [`MovMuxer::set_external_media`].
    external_media: Option<ExternalMediaWrite>,
    /// Packed ISO-639-2/T media language for `mdhd.language` (QTFF
    /// p. 197 / ISO/IEC 14496-12 §8.4.2.3). Defaults to
    /// [`MDHD_LANGUAGE_UND`] (`"und"`). Set via
    /// [`MovMuxer::set_track_language`].
    media_language: u16,
    /// Optional RFC 4646 / BCP 47 extended language tag emitted as an
    /// `elng` box in `mdia` (§8.4.6) — e.g. `"en-US"`. `None` ⇒ no
    /// `elng`. Set via [`MovMuxer::set_track_extended_language`].
    extended_language: Option<String>,
    /// Optional override of the `gmhd/gmin` Generic Media Information
    /// header (QTFF p. 65) for a track whose `minf` carries a `gmhd`
    /// (time-code / text / generic media). `None` ⇒ the muxer writes a
    /// default `gmin` (copy graphics mode, no opcolor, centred balance).
    /// Set via [`MovMuxer::set_track_gmin`]. Ignored on `vmhd`/`smhd`
    /// tracks (video/audio carry no `gmhd`).
    gmin: Option<Gmin>,
    /// Optional override of the `gmhd/text` media-information header
    /// matrix (QTFF p. 144) for a text track. `None` ⇒ the muxer writes
    /// the identity transformation matrix. Set via
    /// [`MovMuxer::set_text_header_matrix`]. Only meaningful for a
    /// [`MuxTrackKind::Text`] track.
    text_header_matrix: Option<[i32; 9]>,
    /// When `true`, the muxer emits the sample-size table as a Compact
    /// Sample Size Box (`stz2`, ISO/IEC 14496-12 §8.7.3.3) using the
    /// narrowest 4 / 8 / 16-bit `field_size` that fits every sample,
    /// instead of the default `stsz`. Set via
    /// [`MovMuxer::set_compact_sample_size`]. Ignored when the track's
    /// sizes are all equal (a uniform `stsz` is already the most compact
    /// form) or when any size exceeds 16 bits (`stz2` cannot represent
    /// it). Defaults to `false`.
    compact_sample_size: bool,
    /// Optional `stbl`-scope Independent and Disposable Samples Box
    /// (`sdtp`, ISO/IEC 14496-12 §8.6.4). When non-empty the muxer emits
    /// one packed dependency byte per sample after the chunk-offset
    /// table; the row count must equal the track's sample count (§8.6.4.1
    /// — the box carries no on-disk count field). Empty ⇒ no `sdtp`. Set
    /// via [`MovMuxer::set_sample_dependencies`].
    sdtp: Vec<SdtpEntry>,
    /// Optional `stbl`-scope Degradation Priority Box (`stdp`, ISO/IEC
    /// 14496-12 §8.5.3). When non-empty the muxer emits one 16-bit
    /// priority per sample; the row count must equal the track's sample
    /// count (§8.5.3.1 — no on-disk count field). Empty ⇒ no `stdp`. Set
    /// via [`MovMuxer::set_degradation_priorities`].
    stdp: Vec<u16>,
    /// Optional `stbl`-scope Padding Bits Box (`padb`, ISO/IEC 14496-12
    /// §8.7.6). When non-empty the muxer emits one 3-bit `pad` field per
    /// sample (two rows packed per byte); the row count must equal the
    /// track's sample count and each value is `0..=7`. Empty ⇒ no `padb`.
    /// Set via [`MovMuxer::set_padding_bits`].
    padb: Vec<u8>,
    /// Optional `stbl`-scope Shadow Sync Sample Box (`stsh`, ISO/IEC
    /// 14496-12 §8.6.3). When non-empty the muxer emits the
    /// shadowed→sync sample-number pairs, sorted ascending by
    /// `shadowed_sample_number`. Empty ⇒ no `stsh`. Set via
    /// [`MovMuxer::set_shadow_sync_samples`].
    stsh: Vec<StshEntry>,
    /// Optional `stbl`-scope Sub-Sample Information Box (`subs`, ISO/IEC
    /// 14496-12 §8.7.7). When non-empty the muxer emits the sparse
    /// per-sample sub-sample table (delta-coded sample numbers). Empty ⇒
    /// no `subs`. Set via [`MovMuxer::set_sub_samples`].
    subs: Vec<SubSampleInfo>,
    /// Optional `trak`-scope Track Load Settings atom (`load`, QTFF
    /// pp. 48–49). When `Some` the muxer emits a `load` as an early
    /// `trak` child carrying the movie-timescale preload window + the
    /// preload-mode / quality-hint bitfields. `None` ⇒ no `load`. This
    /// is a QuickTime-only atom (ISO BMFF does not define it). Set via
    /// [`MovMuxer::set_track_load_settings`].
    load: Option<Load>,
    /// Optional `trak`-scope Clipping atom (`clip` > `crgn`, QTFF
    /// pp. 43–44). When `Some` the muxer emits a `clip` (wrapping a
    /// single `crgn` Clipping Region) as a `trak` child carrying the
    /// QuickDraw bounding box + optional scanline mask. `None` ⇒ no
    /// `clip`. QuickTime-only (ISO BMFF does not define it). Set via
    /// [`MovMuxer::set_track_clipping`].
    clipping: Option<Clipping>,
    /// Optional `trak`-scope Track Matte atom (`matt` > `kmat`, QTFF
    /// pp. 44–45). When `Some` the muxer emits a `matt` (wrapping a
    /// single `kmat` Compressed Matte) as a `trak` child carrying the
    /// blend matte's image description + compressed matte data. `None` ⇒
    /// no `matt`. QuickTime-only (ISO BMFF does not define it). Set via
    /// [`MovMuxer::set_track_matte`].
    matte: Option<Matte>,
    /// Optional ISO BMFF Track Kind boxes (`kind`, §8.10.4) emitted into
    /// the track-level `udta`. Each [`KindEntry`] labels the track with a
    /// `(schemeURI, value)` role pair (e.g. WebVTT / DASH subtitle
    /// roles); `Quantity: Zero or more`. Empty ⇒ no `kind` boxes. ISO
    /// BMFF-only (QTFF does not define it). Set via
    /// [`MovMuxer::set_track_kinds`].
    track_kinds: Vec<KindEntry>,
    /// Optional ISO BMFF Track Selection box (`tsel`, §8.10.3) emitted
    /// into the track-level `udta`. Carries the `switch_group` +
    /// differentiating/descriptive `attributes` that group tracks for
    /// adaptive switching across an alternate group. `Quantity: Zero or
    /// one`. `None` ⇒ no `tsel`. ISO BMFF-only. Set via
    /// [`MovMuxer::set_track_selection`].
    track_selection: Option<TrackSelection>,
    /// Optional ISO BMFF Track Group box (`trgr`, §8.3.4) emitted as a
    /// `trak` child. Each [`TrackGroupTypeEntry`] is one membership
    /// declaration (a FullBox whose FourCC is the `track_group_type`);
    /// tracks sharing a `(track_group_type, track_group_id)` pair belong
    /// to the same group. Empty ⇒ no `trgr`. ISO BMFF-only. Set via
    /// [`MovMuxer::set_track_groups`].
    track_groups: Vec<TrackGroupTypeEntry>,
    /// Which audio sample-description layout `build_stsd` writes for a
    /// [`MuxTrackKind::Audio`] track. Defaults to the 20-byte QTFF
    /// version-0 body; see [`MovMuxer::set_sound_description_v1`]
    /// (QTFF p. 101 / p. 102) and [`MovMuxer::set_audio_entry_v1`]
    /// (ISO/IEC 14496-12:2015 §12.2.3). Ignored on non-audio tracks.
    audio_description: AudioDescriptionWrite,
    /// Track display matrix override for `tkhd` (QTFF p. 42). `None`
    /// ⇒ identity. Set via [`MovMuxer::set_track_matrix`].
    tkhd_matrix: Option<[i32; 9]>,
    /// `tkhd` layer — the track's front-to-back compositing order
    /// (QTFF p. 42: tracks with lower layer values are displayed in
    /// front). Default 0. Set via [`MovMuxer::set_track_layer`].
    tkhd_layer: i16,
    /// `tkhd` alternate_group id (QTFF p. 42: tracks sharing a
    /// non-zero value are mutual alternates; only one plays at a
    /// time). Default 0 (no group). Set via
    /// [`MovMuxer::set_track_alternate_group`].
    tkhd_alternate_group: i16,
    /// `tkhd` volume override (8.8 fixed-point). `None` ⇒ the kind
    /// default (1.0 for audio, 0 for everything else). Set via
    /// [`MovMuxer::set_track_volume`].
    tkhd_volume: Option<i16>,
    /// `tkhd` 24-bit flags override (QTFF p. 42: enabled 0x1 /
    /// in-movie 0x2 / in-preview 0x4 / in-poster 0x8). `None` ⇒ the
    /// historical default 0x7. Set via [`MovMuxer::set_track_flags`].
    tkhd_flags: Option<u32>,
}

/// The packed `mdhd.language` value for `"und"` (undetermined) — the
/// default emitted when a track's language isn't set
/// (ISO/IEC 14496-12 §8.4.2.3: five-bit ISO-639-2/T codes biased by
/// `0x60`, packed `0_uuuuu_nnnnn_ddddd`).
pub const MDHD_LANGUAGE_UND: u16 = 0x55C4;

/// One track-reference declaration written by
/// [`MovMuxer::set_track_references`] (QTFF p. 50 / ISO/IEC 14496-12
/// §8.3.3 — Track Reference Type Box).
///
/// A `tref` carries one child atom per reference *type*; the child's
/// FourCC names the relationship (`chap` chapter list, `tmcd` time-code
/// track, `sync`, `scpt`, `hint`, `cdsc`, …) and its body is a tightly
/// packed list of `u32` referenced track ids. This type is the
/// write-side mirror of the read-side [`crate::track::TrackRef`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackReference {
    /// Reference-type FourCC (e.g. `*b"chap"`, `*b"tmcd"`).
    pub reference_type: [u8; 4],
    /// 1-based ids of the referenced tracks (the values returned by
    /// [`MovMuxer::add_track`]). At least one id should be present; an
    /// empty list emits an empty child atom (legal but inert).
    pub track_ids: Vec<u32>,
}

impl TrackReference {
    /// Build a reference of the given type to a single track.
    pub fn to(reference_type: [u8; 4], track_id: u32) -> Self {
        Self {
            reference_type,
            track_ids: vec![track_id],
        }
    }

    /// Build a `chap` (chapter-list) reference to a single text track.
    pub fn chapter(text_track_id: u32) -> Self {
        Self::to(*b"chap", text_track_id)
    }

    /// Build a `tmcd` (time-code) reference to a single time-code track.
    pub fn timecode(timecode_track_id: u32) -> Self {
        Self::to(*b"tmcd", timecode_track_id)
    }
}

/// One data-reference entry in a track's `dref` (Data Reference Box,
/// QTFF p. 65 / ISO/IEC 14496-12 §8.7.2), written by
/// [`MovMuxer::set_data_references`].
///
/// A `dref` lists the storage locations a track's media may live in;
/// each sample's chunk offset is interpreted relative to the location
/// its sample entry's `data_reference_index` selects. This type is the
/// write-side mirror of the read-side [`crate::reference::DataReference`].
///
/// Because [`MovMuxer`] always writes the track's sample bytes into the
/// file's own `mdat`, exactly one entry must be the [`SelfRef`] (the
/// "media is in this file" entry, `flags & 0x01 == 1`) — the muxer
/// points every sample entry's `data_reference_index` at it. Additional
/// [`Url`] / [`Urn`] entries are declared for readers / reference-movie
/// tooling but carry no in-file samples.
///
/// [`SelfRef`]: DataReferenceWrite::SelfRef
/// [`Url`]: DataReferenceWrite::Url
/// [`Urn`]: DataReferenceWrite::Urn
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataReferenceWrite {
    /// `url ` self-reference (`flags = 1`): the media bytes are in this
    /// file. Written with an empty data slot per §8.7.2.
    SelfRef,
    /// `url ` external reference (`flags = 0`): a NUL-terminated UTF-8
    /// URL naming the file the media lives in.
    Url(String),
    /// `urn ` external reference (`flags = 0`): a NUL-terminated UTF-8
    /// `name` followed by an optional NUL-terminated `location`
    /// (ISO/IEC 14496-12 §8.7.2).
    Urn {
        /// Required URN name.
        name: String,
        /// Optional URN location (empty ⇒ omitted second string).
        location: String,
    },
}

/// Fragmentation policy for [`MovMuxer::write_to_fragmented`].
///
/// Selects the rule used to slice each track's flat sample list into
/// per-fragment runs. The same rule applies to every track; per-track
/// fragmentation policies are out of scope for round 20.
///
/// Spec layout the rule produces: ISO/IEC 14496-12 §8.8.4 — an
/// initial segment (`ftyp` + `moov` with empty stbl tables + `mvex/
/// trex` defaults) followed by one media segment per fragment slice,
/// each carrying a `moof` (with `mfhd` + per-track `traf`) plus a
/// trailing `mdat` whose bytes the `trun` rows index into.
#[derive(Clone, Copy, Debug)]
pub enum FragmentationMode {
    /// Close the current fragment once its accumulated *primary-track
    /// media-timescale* duration meets or exceeds the threshold. The
    /// primary track is the first track added (typically video for
    /// A/V; audio-only files use the only track). Threshold is in
    /// primary-track media-timescale ticks.
    ByDuration(u64),
    /// Close the current fragment once its accumulated primary-track
    /// sample count meets the threshold. Audio tracks slice along the
    /// primary track's wall-clock duration so per-fragment audio
    /// counts vary.
    ByFrameCount(u32),
}

/// How the non-fragmented write path slices each track's samples into
/// `mdat` chunks (QTFF "Interleaving Movie Data", p. 358).
///
/// The sample-table result is always consistent — `stsc` describes the
/// runs (§8.7.4) and `stco`/`co64` list every chunk (§8.7.5) — so a
/// conformant reader accepts either strategy; the choice only affects
/// the physical byte order inside `mdat` and therefore the seek
/// pattern a player needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChunkStrategy {
    /// One chunk per track, tracks laid back-to-back in `add_track`
    /// order — the historical (round-19) layout. Fine for in-memory
    /// consumers; a streaming player of a multi-track file must seek
    /// between the far-apart per-track slabs.
    #[default]
    SingleChunkPerTrack,
    /// Time-ordered interleave, the layout QTFF p. 358 prescribes for
    /// "optimal movie playback": each track is cut into chunks of at
    /// most this many **movie-timescale ticks** (converted to the
    /// track's media timescale, minimum one sample per chunk), and the
    /// chunks of all tracks are merged into `mdat` ordered by their
    /// starting decode time, so data for a given movie time sits close
    /// together in the file. The spec's worked example uses
    /// half-second interleave runs — 300 ticks at the default
    /// 600-tick movie timescale. Ties (equal start times) fall back to
    /// `add_track` order. Must be > 0 (validated at encode time).
    InterleaveByMovieTicks(u32),
}

/// Where the non-fragmented write path places the `moov` atom relative
/// to the `mdat`.
///
/// QTFF's "Optimizing … for Web Playback" section (p. 365): "The
/// important change that took place to allow this [progressive
/// playback] to happen was for QuickTime to place global movie
/// information at the beginning of the file. Originally, this
/// information was at the end of the file." Atom order at the top
/// level is otherwise free — [`crate::MovDemuxer`] accepts both and
/// reports the layout via
/// [`is_faststart`](crate::MovDemuxer::is_faststart).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MoovPlacement {
    /// `ftyp` + `mdat` + `moov` — the historical default. Chunk
    /// offsets are independent of the `moov` size, and the compressed
    /// movie resource (`cmov`) option is available.
    #[default]
    AfterMdat,
    /// `ftyp` + `moov` + `mdat` — "faststart": a front-to-back reader
    /// has the complete sample tables before any sample bytes. Chunk
    /// offsets are resolved by a fixed-point sizing pass over the
    /// `moov`'s own length. Incompatible with
    /// [`MovMuxer::with_compressed_movie_resource`].
    BeforeMdat,
}

/// One resolved `mdat` chunk belonging to a track: an absolute file
/// offset (what `stco`/`co64` store, ISO/IEC 14496-12 §8.7.5 / QTFF
/// p. 78) plus the number of consecutive media-order samples laid down
/// there (what the matching `stsc` run describes, §8.7.4 / QTFF p. 76).
#[derive(Clone, Copy, Debug)]
struct TrackChunk {
    /// Absolute file offset of the chunk's first sample byte.
    offset: u64,
    /// Number of consecutive samples (in media order) in this chunk.
    sample_count: u32,
}

/// One planned span of the non-fragmented `mdat` payload, in file
/// order, before absolute offsets are assigned. The layout pass walks
/// the segment list once, accumulating a cursor, to resolve every
/// [`TrackChunk`] offset and aux-slab offset; the emit pass walks the
/// same list writing bytes, so the two can never disagree.
enum MdatSegment {
    /// A run of consecutive samples from one track.
    Chunk {
        track_idx: usize,
        first_sample: usize,
        sample_count: usize,
    },
    /// A track's complete sample-auxiliary-information slab
    /// (§8.7.8/§8.7.9) — always contiguous so the single-entry `saio`
    /// (§8.7.9.3) can address it.
    AuxSlab { track_idx: usize },
}

/// Writer-side counterpart of [`crate::demuxer::MovDemuxer`]. Builds a
/// non-fragmented MOV/MP4 carrying one or more video/audio tracks; the
/// emitted file is structurally accepted by `ffprobe -of json` and
/// round-trips back through `MovDemuxer` with the same per-track
/// sample count and per-sample sizes.
///
/// The default layout is `ftyp + mdat + moov` (mdat-before-moov);
/// [`MovMuxer::with_faststart`] / [`MovMuxer::with_moov_placement`]
/// swap `moov` in front for the QTFF p. 365 progressive-playback
/// order, and [`MovMuxer::with_chunk_strategy`] selects between the
/// one-chunk-per-track layout and the QTFF p. 358 time-ordered
/// interleave. The demuxer accepts every combination.
///
/// Round 20 also adds the fragmented-write path via
/// [`MovMuxer::with_fragmentation`] + [`MovMuxer::write_to_fragmented`]:
/// the emitted file is a DASH-/fMP4-compliant initial-segment +
/// media-segment stream (`ftyp` + `moov` with empty stbl +
/// `mvex/trex`, then one or more `moof` + `mdat` pairs).
pub struct MovMuxer {
    /// Movie-scope timescale used by `mvhd.duration` and every
    /// `tkhd.duration`. Defaults to 600 (the QTFF historical
    /// preference: divides cleanly into 24/25/30/29.97 fps) but
    /// callers can override via [`MovMuxer::with_movie_timescale`].
    movie_timescale: u32,
    tracks: Vec<TrackWrite>,
    /// Optional fragmentation policy — when `Some`, the muxer emits
    /// a fragmented layout (`ftyp` + init-`moov` + N × `moof`+`mdat`)
    /// rather than the default non-fragmented `ftyp` + `mdat` + `moov`.
    fragmentation: Option<FragmentationMode>,
    /// Chunking rule for the non-fragmented `mdat` (see
    /// [`ChunkStrategy`]). Defaults to the historical
    /// one-chunk-per-track layout. No effect on the fragmented path
    /// (fragment slicing is governed by [`FragmentationMode`]).
    chunk_strategy: ChunkStrategy,
    /// Where the non-fragmented write path places `moov` relative to
    /// `mdat` (see [`MoovPlacement`]). Defaults to the historical
    /// mdat-first layout. No effect on the fragmented path.
    moov_placement: MoovPlacement,
    /// When `true`, the non-fragmented write path losslessly compresses
    /// the serialized movie resource and emits it as a
    /// `moov > cmov > dcom + cmvd` tree (QTFF p. 81, "Allowing QuickTime
    /// to Compress the Movie Resource") instead of a plain `moov`.
    /// Defaults to `false`. Has no effect on the fragmented path.
    compress_movie_resource: bool,
    /// Optional movie-level user-data items (QTFF pp. 36–38 / ISO/IEC
    /// 14496-12 §8.10.1). When non-empty the muxer emits a `moov/udta`
    /// after the last `trak`. Empty ⇒ no `udta` box. Honoured on the
    /// non-fragmented path; the fragmented init `moov` ignores it.
    metadata: Vec<MovMetadata>,
    /// Optional movie-level Apple QuickTime Metadata items (the modern
    /// `moov/meta` = `hdlr` `mdta` + `keys` + `ilst` shape, distinct
    /// from the legacy `udta` in `metadata`). When non-empty the muxer
    /// emits a `moov/meta` after the last `trak` (and after `udta` when
    /// both are present). Empty ⇒ no `meta` box. Honoured on the
    /// non-fragmented path; the fragmented init `moov` ignores it.
    apple_metadata: Vec<MovMetaItem>,
    /// Movie display matrix written into `mvhd` (QTFF p. 33). `None`
    /// ⇒ identity. Set via [`MovMuxer::set_movie_matrix`].
    movie_matrix: Option<[i32; 9]>,
    /// The six QuickTime `mvhd` preview/poster/selection/current time
    /// fields (QTFF pp. 33–34), in declaration order: preview_time,
    /// preview_duration, poster_time, selection_time,
    /// selection_duration, current_time. All zero by default (the
    /// ISO BMFF `pre_defined` reading). Set via
    /// [`MovMuxer::set_movie_preview`] /
    /// [`MovMuxer::set_movie_poster_time`] /
    /// [`MovMuxer::set_movie_selection`] /
    /// [`MovMuxer::set_movie_current_time`].
    movie_times: [u32; 6],
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
            fragmentation: None,
            chunk_strategy: ChunkStrategy::default(),
            moov_placement: MoovPlacement::default(),
            compress_movie_resource: false,
            metadata: Vec::new(),
            apple_metadata: Vec::new(),
            movie_matrix: None,
            movie_times: [0; 6],
        }
    }

    /// Override the movie-scope timescale used by `mvhd.duration` and
    /// every `tkhd.duration`. Must be > 0.
    pub fn with_movie_timescale(mut self, ts: u32) -> Self {
        debug_assert!(ts > 0, "movie_timescale must be > 0");
        self.movie_timescale = ts.max(1);
        self
    }

    /// Opt-in fragmentation. Subsequent calls to
    /// [`MovMuxer::write_to_fragmented`] / [`encode_fragmented_to_vec`]
    /// will emit an ISO/IEC 14496-12 §8.8 fragmented layout: an init
    /// segment (`ftyp` + `moov` with empty `stbl` tables and a
    /// `mvex/trex` defaults block per track) followed by one media
    /// segment per fragment, each a `moof` (with `mfhd` +
    /// per-track `traf/tfhd/trun`) and a trailing `mdat` carrying the
    /// per-sample bytes.
    ///
    /// The non-fragmented [`MovMuxer::write_to`] / `encode_to_vec`
    /// methods remain available — they ignore this setting.
    ///
    /// [`encode_fragmented_to_vec`]: MovMuxer::encode_fragmented_to_vec
    pub fn with_fragmentation(mut self, mode: FragmentationMode) -> Self {
        self.fragmentation = Some(mode);
        self
    }

    /// Return the configured fragmentation policy (if any).
    pub fn fragmentation_mode(&self) -> Option<FragmentationMode> {
        self.fragmentation
    }

    /// Select the non-fragmented `mdat` chunking rule (QTFF
    /// "Interleaving Movie Data", p. 358). See [`ChunkStrategy`]; the
    /// default is the historical one-chunk-per-track layout. The
    /// fragmented write path ignores this setting — fragment slicing
    /// is governed by [`MovMuxer::with_fragmentation`].
    pub fn with_chunk_strategy(mut self, strategy: ChunkStrategy) -> Self {
        self.chunk_strategy = strategy;
        self
    }

    /// Return the configured non-fragmented chunking rule.
    pub fn chunk_strategy(&self) -> ChunkStrategy {
        self.chunk_strategy
    }

    /// Select where the non-fragmented write path places the `moov`
    /// atom (see [`MoovPlacement`]). [`MoovPlacement::BeforeMdat`] is
    /// the "faststart" web-playback layout (QTFF p. 365); the default
    /// keeps the historical `ftyp` + `mdat` + `moov` order.
    pub fn with_moov_placement(mut self, placement: MoovPlacement) -> Self {
        self.moov_placement = placement;
        self
    }

    /// Convenience for
    /// `with_moov_placement(MoovPlacement::BeforeMdat)` — emit the
    /// "faststart" layout (`ftyp` + `moov` + `mdat`).
    pub fn with_faststart(self) -> Self {
        self.with_moov_placement(MoovPlacement::BeforeMdat)
    }

    /// Return the configured `moov` placement.
    pub fn moov_placement(&self) -> MoovPlacement {
        self.moov_placement
    }

    /// Opt-in lossless compression of the movie resource on the
    /// non-fragmented write path (QTFF p. 81, "Allowing QuickTime to
    /// Compress the Movie Resource").
    ///
    /// When enabled, [`MovMuxer::write_to`] / [`encode_to_vec`] still
    /// lay out `ftyp` + `mdat` first (so the `stco` / `co64` chunk
    /// offsets stay file-absolute and `mdat`-anchored exactly as in the
    /// uncompressed layout), but the trailing `moov` is replaced by a
    /// `moov` whose single child is a `cmov` carrying the zlib-deflated
    /// (`dcom = 'zlib'`, RFC 1950) movie resource plus its 32-bit
    /// uncompressed size in `cmvd` (QTFF p. 81 Table 2-5). The complete
    /// uncompressed movie resource per QTFF p. 30 — the full `moov`
    /// atom, header included — is what gets compressed, so the output
    /// decompresses back to a byte-identical plain-`moov` file and
    /// round-trips through this crate's own [`crate::cmov`] read path
    /// (the demuxer transparently decompresses on open).
    ///
    /// Has no effect on the fragmented path
    /// ([`encode_fragmented_to_vec`]) — QTFF p. 81 describes
    /// movie-resource compression for the flatten-time movie atom, not
    /// per-fragment `moof` boxes.
    ///
    /// [`encode_to_vec`]: MovMuxer::encode_to_vec
    /// [`encode_fragmented_to_vec`]: MovMuxer::encode_fragmented_to_vec
    pub fn with_compressed_movie_resource(mut self, compress: bool) -> Self {
        self.compress_movie_resource = compress;
        self
    }

    /// Return whether movie-resource compression is enabled for the
    /// non-fragmented write path.
    pub fn compresses_movie_resource(&self) -> bool {
        self.compress_movie_resource
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
            sample_aux: None,
            sample_to_groups: Vec::new(),
            sample_group_descriptions: Vec::new(),
            cslg: None,
            edits: Vec::new(),
            metadata: Vec::new(),
            apple_metadata: Vec::new(),
            visual_extensions: VisualExtensions::default(),
            track_references: Vec::new(),
            tapt: None,
            data_references: Vec::new(),
            external_media: None,
            media_language: MDHD_LANGUAGE_UND,
            extended_language: None,
            gmin: None,
            text_header_matrix: None,
            compact_sample_size: false,
            sdtp: Vec::new(),
            stdp: Vec::new(),
            padb: Vec::new(),
            stsh: Vec::new(),
            subs: Vec::new(),
            load: None,
            clipping: None,
            matte: None,
            track_kinds: Vec::new(),
            track_selection: None,
            track_groups: Vec::new(),
            audio_description: AudioDescriptionWrite::V0,
            tkhd_matrix: None,
            tkhd_layer: 0,
            tkhd_alternate_group: 0,
            tkhd_volume: None,
            tkhd_flags: None,
        });
        self.tracks.len() as u32
    }

    /// Attach a `stbl`-scope sample-auxiliary-information stream to a
    /// previously-added track (ISO/IEC 14496-12 §8.7.8 / §8.7.9).
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The stream's `per_sample` length must equal the track's sample
    /// count; otherwise this returns an error and the track is left
    /// unchanged.
    ///
    /// On the next non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to), each sample's aux blob is laid
    /// into `mdat` contiguously after the track's sample data, a `saiz`
    /// describes the per-sample sizes, and a single-entry `saio` carries
    /// the absolute file offset of the first blob.
    ///
    /// On the next fragmented [`MovMuxer::encode_fragmented_to_vec`] /
    /// [`write_to_fragmented`](MovMuxer::write_to_fragmented), each
    /// fragment's slice of the stream is laid into that fragment's
    /// `mdat` (after every track's sample data) and the matching `traf`
    /// carries a `saiz` + single-entry `saio` per §8.7.8 / §8.7.9 /
    /// §8.8.14, with the `saio` offset relative to the enclosing `moof`
    /// (`default-base-is-moof`).
    ///
    /// Replaces any stream previously attached to the same track.
    pub fn set_sample_aux(&mut self, track_id: u32, stream: SampleAuxStream) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_sample_aux unknown track id {track_id}"
                ))
            })?;
        if self.tracks[idx].external_media.is_some() {
            return Err(Error::invalid(format!(
                "MOV muxer: set_sample_aux track {track_id} is marked external \
                 (set_external_media); the aux slab is laid into this file's mdat, \
                 which an external track does not touch"
            )));
        }
        let want = self.tracks[idx].samples.len();
        if stream.per_sample.len() != want {
            return Err(Error::invalid(format!(
                "MOV muxer: sample-aux stream has {} blobs but track {track_id} has {want} samples",
                stream.per_sample.len()
            )));
        }
        // The §8.7.8.2 `saiz` size table is u8-wide, so a single
        // sample's auxiliary-information record cannot exceed 255 bytes
        // through this stbl-scope writer. Reject rather than silently
        // truncate; callers with larger records split them or use a
        // derived mechanism.
        if let Some((i, b)) = stream
            .per_sample
            .iter()
            .enumerate()
            .find(|(_, b)| b.len() > u8::MAX as usize)
        {
            return Err(Error::invalid(format!(
                "MOV muxer: sample-aux blob {i} is {} bytes; saiz size table is u8-wide (max 255)",
                b.len()
            )));
        }
        self.tracks[idx].sample_aux = Some(stream);
        Ok(())
    }

    /// Attach a `stbl`-scope sample-to-group assignment to a
    /// previously-added track, emitted as a `csgp`
    /// (CompactSampleToGroupBox, ISO/IEC 14496-12:2020 §8.9.5).
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The assignment's `indices` length must equal the track's sample
    /// count; otherwise this returns an error and the track is left
    /// unchanged.
    ///
    /// On the next non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to), a `csgp` is emitted inside the
    /// track's `stbl` after the chunk-offset table. The muxer encodes
    /// the per-sample indices in the compact pattern form (one
    /// `pattern_length == 1` pattern per run of consecutive equal
    /// indices), which the read path
    /// ([`crate::sample_groups::parse_csgp`]) expands back to the exact
    /// per-sample assignment.
    ///
    /// Multiple calls with distinct `grouping_type`s accumulate (one
    /// `csgp` per call); a second call with a `grouping_type` already
    /// present **replaces** the prior assignment for that type.
    ///
    /// The `csgp` only carries the per-sample index mapping; the typed
    /// descriptions those indices reference live in a sibling `sgpd`
    /// emitted by [`MovMuxer::set_sample_group_description`]. Pair the
    /// two for a conformant file (a non-zero index with no matching
    /// `sgpd` entry points at nothing).
    ///
    /// To emit the widely-compatible classic run-length `sbgp`
    /// (SampleToGroupBox, §8.9.2) instead of the compact `csgp`, use
    /// [`MovMuxer::add_sample_to_group_with_form`] with
    /// [`SampleGroupBoxForm::Classic`].
    pub fn add_sample_to_group(
        &mut self,
        track_id: u32,
        assignment: SampleToGroupWrite,
    ) -> Result<()> {
        self.add_sample_to_group_with_form(track_id, assignment, SampleGroupBoxForm::Compact)
    }

    /// Attach a `stbl`-scope sample-to-group assignment, choosing
    /// whether it is carried by the compact `csgp` (§8.9.5) or the
    /// classic run-length `sbgp` (§8.9.2) — see [`SampleGroupBoxForm`].
    ///
    /// Identical to [`MovMuxer::add_sample_to_group`] in every other
    /// respect (index-count validation, replace-by-`grouping_type`,
    /// pairing with a sibling `sgpd`); the only difference is which box
    /// the per-sample index mapping is written into. Both forms
    /// round-trip back through `MovDemuxer` with the same per-sample
    /// group-description indices (`sbgp` via
    /// [`crate::sample_groups::parse_sbgp`], `csgp` via
    /// [`crate::sample_groups::parse_csgp`]).
    pub fn add_sample_to_group_with_form(
        &mut self,
        track_id: u32,
        assignment: SampleToGroupWrite,
        form: SampleGroupBoxForm,
    ) -> Result<()> {
        let idx = self.track_index(track_id, "add_sample_to_group")?;
        let want = self.tracks[idx].samples.len();
        if assignment.indices.len() != want {
            return Err(Error::invalid(format!(
                "MOV muxer: sample-to-group has {} indices but track {track_id} has {want} samples",
                assignment.indices.len()
            )));
        }
        // Replace-by-grouping_type so a caller correcting an assignment
        // does not leave two boxes naming the same `sgpd`.
        if let Some(slot) = self.tracks[idx]
            .sample_to_groups
            .iter_mut()
            .find(|(g, _)| g.grouping_type == assignment.grouping_type)
        {
            *slot = (assignment, form);
        } else {
            self.tracks[idx].sample_to_groups.push((assignment, form));
        }
        Ok(())
    }

    /// Attach a Sample Group Description Box (`sgpd`, ISO/IEC 14496-12
    /// §8.9.3) to a previously-added track — the sibling box whose typed
    /// entries a [`SampleToGroupWrite`]'s per-sample indices reference.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The `sgpd` is written **version 1** inside the track's `stbl`,
    /// immediately before the `csgp` boxes (§8.9.3's containment order).
    /// Entries are 1-based: `entries[0]` is the description selected by
    /// `group_description_index == 1`. When every entry is the same
    /// length the box uses a constant `default_length`; otherwise it
    /// uses `default_length == 0` with a per-entry `description_length`
    /// prefix (§8.9.3.2).
    ///
    /// Build the standard §10 entry payloads with
    /// [`SampleGroupDescriptionWrite::roll_entry`] /
    /// [`prol_entry`](SampleGroupDescriptionWrite::prol_entry) /
    /// [`rap_entry`](SampleGroupDescriptionWrite::rap_entry) /
    /// [`tele_entry`](SampleGroupDescriptionWrite::tele_entry) /
    /// [`sap_entry`](SampleGroupDescriptionWrite::sap_entry), or supply
    /// raw bytes for a codec-specific grouping.
    ///
    /// Multiple calls with distinct `grouping_type`s accumulate (one
    /// `sgpd` per call); a second call with a `grouping_type` already
    /// present **replaces** the prior description for that type (§8.9.3 —
    /// "at most one instance of this box with a particular grouping type
    /// in a Sample Table Box"). An empty `entries` list is rejected (a
    /// `sgpd` with no descriptions is pointless and some readers reject
    /// `entry_count == 0`).
    ///
    /// The file round-trips back through `MovDemuxer`:
    /// [`crate::sample_groups::parse_sgpd`] recovers the per-entry
    /// payloads and the typed decoders recover the structured fields.
    pub fn set_sample_group_description(
        &mut self,
        track_id: u32,
        description: SampleGroupDescriptionWrite,
    ) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_sample_group_description unknown track id {track_id}"
                ))
            })?;
        if description.entries.is_empty() {
            return Err(Error::invalid(format!(
                "MOV muxer: sgpd for track {track_id} must carry at least one entry"
            )));
        }
        // Replace-by-grouping_type so the track never carries two `sgpd`
        // boxes naming the same grouping_type (§8.9.3 forbids it).
        if let Some(slot) = self.tracks[idx]
            .sample_group_descriptions
            .iter_mut()
            .find(|d| d.grouping_type == description.grouping_type)
        {
            *slot = description;
        } else {
            self.tracks[idx].sample_group_descriptions.push(description);
        }
        Ok(())
    }

    /// Emit an auto-derived Composition to Decode Box (`cslg`, ISO/IEC
    /// 14496-12 §8.6.1.4) inside a previously-added track's `stbl`.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The five `cslg` fields are computed from the track's per-sample
    /// composition offsets (`MuxSample.composition_offset`) and
    /// durations at layout time:
    ///
    /// * `composition_to_dts_shift` — `max(0, -min_offset)`, the shift
    ///   that keeps every shifted CTS at or above its DTS (§8.6.1.4.3).
    /// * `least` / `greatest_decode_to_display_delta` — the min / max
    ///   composition offset across the track.
    /// * `composition_start_time` — the earliest composition time
    ///   (`min(DTS_i + offset_i)`).
    /// * `composition_end_time` — the latest composition time plus its
    ///   sample duration (`max(DTS_i + offset_i + duration_i)`).
    ///
    /// `cslg` only makes sense alongside a `ctts`; calling this on a
    /// track whose samples all have `composition_offset == 0` is a
    /// no-op-shaped box (all-zero deltas) but is still emitted so a
    /// caller that opts in always gets one. The box auto-promotes from
    /// version 0 (`int(32)`) to version 1 (`int(64)`) the moment any
    /// field leaves the signed-32-bit range. Round-trips through
    /// [`crate::media_meta::parse_cslg`].
    pub fn auto_cslg(&mut self, track_id: u32) -> Result<()> {
        let idx = self.track_index(track_id, "auto_cslg")?;
        self.tracks[idx].cslg = Some(CslgWrite::Auto);
        Ok(())
    }

    /// Emit a Composition to Decode Box (`cslg`, ISO/IEC 14496-12
    /// §8.6.1.4) with caller-supplied bounds inside a previously-added
    /// track's `stbl`.
    ///
    /// Use this when the bounds are already known (e.g. carried over
    /// from a source file) rather than re-derived from the samples; see
    /// [`MovMuxer::auto_cslg`] for the derive-from-samples path. The box
    /// auto-promotes to version 1 when any field leaves the signed
    /// 32-bit range. Round-trips through
    /// [`crate::media_meta::parse_cslg`].
    pub fn set_cslg(&mut self, track_id: u32, cslg: Cslg) -> Result<()> {
        let idx = self.track_index(track_id, "set_cslg")?;
        self.tracks[idx].cslg = Some(CslgWrite::Explicit(cslg));
        Ok(())
    }

    /// Resolve a 1-based `track_id` to a `self.tracks` index, or an
    /// error naming the calling method.
    fn track_index(&self, track_id: u32, method: &str) -> Result<usize> {
        (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!("MOV muxer: {method} unknown track id {track_id}"))
            })
    }

    /// Attach an edit list to a previously-added track, emitted as an
    /// `edts > elst` (Edit List Box, QTFF p. 47 / ISO/IEC 14496-12
    /// §8.6.6) between the track's `tkhd` and `mdia`.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// Each [`MuxEdit`] is one segment; see [`MuxEdit::segment`] /
    /// [`MuxEdit::empty`] for the common shapes. Passing an empty slice
    /// removes any previously-attached edit list (the track reverts to
    /// the implicit "entire media is used" default, no `edts` emitted).
    ///
    /// On the next non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to), the `elst` is auto-versioned:
    /// version 0 (32-bit `track_duration` + signed 32-bit `media_time`)
    /// when every entry fits, version 1 (64-bit + signed 64-bit, the
    /// ISO/IEC 14496-12 §8.6.6 extension) the moment any
    /// `track_duration` exceeds `u32::MAX` or any `media_time` falls
    /// outside `i32`. Either form round-trips through the read-side
    /// [`crate::edit::parse_elst`].
    ///
    /// Validation (entries are otherwise written verbatim):
    /// * `media_time` may be `-1` (the empty-edit sentinel) or any
    ///   non-negative value; any other negative value is rejected.
    /// * `media_rate` may be `0` (a dwell, [`MuxEdit::dwell`]) or any
    ///   positive 16.16 value; negative rates are rejected (QTFF
    ///   p. 48 forbids them and the read path drops such segments).
    /// * `track_duration` must be representable; the only hard limit is
    ///   the version-1 64-bit field, so no overflow check is needed.
    ///
    /// The fragmented write path ([`encode_fragmented_to_vec`]) emits
    /// the same `edts > elst` into the **init-segment** `trak` (the
    /// `moof`s only index samples, so the edit list must ride the
    /// init `moov` to reach players).
    ///
    /// [`encode_fragmented_to_vec`]: MovMuxer::encode_fragmented_to_vec
    /// Write this audio track's `stsd` entry as a QTFF
    /// **`SoundDescriptionV1`** (QTFF p. 101): version 1 with the four
    /// 32-bit fixed-compression-ratio fields appended after the
    /// 20-byte version-0 body. `vbr` selects the p. 102 VBR "third
    /// variant" — Compression ID `-2`, each sample a *compressed
    /// frame* (then only `samples_per_packet` / `bytes_per_sample`
    /// are meaningful; pass the other two as `0`).
    ///
    /// Errors on an unknown `track_id`, a non-audio track, or a track
    /// already configured as an ISO `AudioSampleEntryV1` (the two
    /// layouts are mutually exclusive). Round-trips onto
    /// [`crate::SampleDescription`]'s `audio_version` / `sound_v1` /
    /// `audio_compression_id` / `is_vbr()`.
    pub fn set_sound_description_v1(
        &mut self,
        track_id: u32,
        fields: SoundV1,
        vbr: bool,
    ) -> Result<()> {
        let t = self.audio_track_mut(track_id)?;
        if matches!(t.audio_description, AudioDescriptionWrite::IsoV1(_)) {
            return Err(Error::invalid(
                "MOV: track already uses an ISO AudioSampleEntryV1; QTFF v1 is exclusive",
            ));
        }
        if matches!(t.audio_description, AudioDescriptionWrite::QtffV2(_)) {
            return Err(Error::invalid(
                "MOV: track already uses a QTFF SoundDescriptionV2; v1 is exclusive",
            ));
        }
        t.audio_description = AudioDescriptionWrite::QtffV1 { fields, vbr };
        Ok(())
    }

    /// Write this audio track's `stsd` entry as a QTFF **Sound Sample
    /// Description version 2** (QTFF 2012-08-14 pp. 181–182): version
    /// 2, the fixed back-compatibility constants in the version-0
    /// field positions (`always3` = 3, `always16` = 16,
    /// `alwaysMinus2` = -2, `always0` = 0, `always65536` = 65536,
    /// `always7F000000` = 0x7F000000), then `sizeOfStructOnly` = 72,
    /// the Float64 `audioSampleRate`, `numAudioChannels` taken from
    /// the track's channel count, and the [`AudioEntryV2`] LPCM
    /// descriptors. The QuickTime-7 high-resolution form — use it for
    /// `lpcm` uncompressed audio, rates past the 16.16 field's
    /// 65535 Hz cap, non-integer rates, or channel layouts above two
    /// channels per track (the `chan` extension's v2 requirement,
    /// p. 186). Codec-config `extra_stsd_atoms` follow at byte 72
    /// where the read-side extension scan resumes.
    ///
    /// Errors on an unknown `track_id`, a non-audio track, a
    /// non-finite / non-positive `audio_sample_rate`, or a track
    /// already configured as a QTFF `SoundDescriptionV1` / ISO
    /// `AudioSampleEntryV1` (the layouts are mutually exclusive).
    /// Round-trips onto [`crate::SampleDescription`]'s `sound_v2` /
    /// `audio_version` / `audio_sample_rate_hz()`.
    pub fn set_sound_description_v2(&mut self, track_id: u32, entry: AudioEntryV2) -> Result<()> {
        if !(entry.audio_sample_rate.is_finite() && entry.audio_sample_rate > 0.0) {
            return Err(Error::invalid(
                "MOV: SoundDescriptionV2 audioSampleRate must be finite and positive",
            ));
        }
        let t = self.audio_track_mut(track_id)?;
        match t.audio_description {
            AudioDescriptionWrite::QtffV1 { .. } => Err(Error::invalid(
                "MOV: track already uses a QTFF SoundDescriptionV1; v2 is exclusive",
            )),
            AudioDescriptionWrite::IsoV1(_) => Err(Error::invalid(
                "MOV: track already uses an ISO AudioSampleEntryV1; QTFF v2 is exclusive",
            )),
            AudioDescriptionWrite::V0 | AudioDescriptionWrite::QtffV2(_) => {
                t.audio_description = AudioDescriptionWrite::QtffV2(entry);
                Ok(())
            }
        }
    }

    /// Write this audio track's `stsd` entry as an ISO BMFF
    /// **`AudioSampleEntryV1`** (ISO/IEC 14496-12:2015 §12.2.3.2):
    /// `entry_version` = 1 (same 20-byte fixed body as version 0),
    /// the enclosing `stsd` taking FullBox version 1 as §8.5.2
    /// requires, and optional `srat` SamplingRateBox / `chnl`
    /// ChannelLayout boxes emitted after the codec-config
    /// `extra_stsd_atoms` in the entry's trailing box area.
    ///
    /// Errors on an unknown `track_id`, a non-audio track, a track
    /// already configured as a QTFF `SoundDescriptionV1`, an
    /// [`ChannelStructure::Explicit`] layout whose row count differs
    /// from the track's channel count (§12.2.4.2 sizes the read loop
    /// from the sample entry's `channelcount`), or a
    /// [`ChannelStructure::Defined`] layout with `defined_layout ==
    /// 0` (that value on-wire selects the explicit-position form).
    /// Round-trips onto [`crate::SampleDescription`]'s
    /// `iso_audio_entry_v1` / `sampling_rate` / `chnl`.
    pub fn set_audio_entry_v1(&mut self, track_id: u32, entry: AudioEntryV1) -> Result<()> {
        let channels = {
            let t = self.audio_track_mut(track_id)?;
            if matches!(t.audio_description, AudioDescriptionWrite::QtffV1 { .. }) {
                return Err(Error::invalid(
                    "MOV: track already uses a QTFF SoundDescriptionV1; ISO v1 is exclusive",
                ));
            }
            if matches!(t.audio_description, AudioDescriptionWrite::QtffV2(_)) {
                return Err(Error::invalid(
                    "MOV: track already uses a QTFF SoundDescriptionV2; ISO v1 is exclusive",
                ));
            }
            match t.kind {
                MuxTrackKind::Audio { channels, .. } => channels,
                _ => unreachable!("audio_track_mut gated"),
            }
        };
        match &entry.channel_layout {
            Some(ChannelLayout {
                channels: Some(ChannelStructure::Explicit(rows)),
                ..
            }) if rows.len() != channels as usize => {
                return Err(Error::invalid(format!(
                    "MOV: chnl explicit layout has {} rows for {} channels",
                    rows.len(),
                    channels
                )));
            }
            Some(ChannelLayout {
                channels:
                    Some(ChannelStructure::Defined {
                        defined_layout: 0, ..
                    }),
                ..
            }) => {
                return Err(Error::invalid(
                    "MOV: chnl definedLayout 0 selects the explicit form; use Explicit",
                ));
            }
            _ => {}
        }
        let t = self.audio_track_mut(track_id)?;
        t.audio_description = AudioDescriptionWrite::IsoV1(entry);
        Ok(())
    }

    /// Shared lookup for the audio-description setters.
    fn audio_track_mut(&mut self, track_id: u32) -> Result<&mut TrackWrite> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|i| *i < self.tracks.len())
            .ok_or_else(|| Error::invalid(format!("MOV: unknown track id {track_id}")))?;
        let t = &mut self.tracks[idx];
        if !matches!(t.kind, MuxTrackKind::Audio { .. }) {
            return Err(Error::invalid(format!(
                "MOV: track {track_id} is not an audio track",
            )));
        }
        Ok(t)
    }

    pub fn set_edit_list(&mut self, track_id: u32, edits: &[MuxEdit]) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_edit_list unknown track id {track_id}"
                ))
            })?;
        // -1 is the only legal negative media_time (the empty-edit
        // sentinel, QTFF p. 47). Reject any other negative value rather
        // than write a media_time the read path would mis-resolve.
        if let Some((i, e)) = edits
            .iter()
            .enumerate()
            .find(|(_, e)| e.media_time < 0 && e.media_time != -1)
        {
            return Err(Error::invalid(format!(
                "MOV muxer: edit {i} has media_time {} (only -1 is a legal negative value: the empty-edit sentinel)",
                e.media_time
            )));
        }
        // QTFF p. 48: the media rate "cannot be 0 or negative" — with
        // the ISO/IEC 14496-12 §8.6.6.3 carve-out that 0 signals a
        // dwell. Negative rates have no defined semantics on either
        // side, and the read path rejects such segments per-segment,
        // so refuse to write one.
        if let Some((i, e)) = edits.iter().enumerate().find(|(_, e)| e.media_rate < 0) {
            return Err(Error::invalid(format!(
                "MOV muxer: edit {i} has negative media_rate {} (QTFF p. 48 forbids negative rates; use 0 for a dwell)",
                e.media_rate
            )));
        }
        self.tracks[idx].edits = edits.to_vec();
        Ok(())
    }

    /// Attach track-reference declarations to a previously-added track,
    /// emitted as a `tref` (Track Reference Box, QTFF p. 50 / ISO/IEC
    /// 14496-12 §8.3.3) between the track's `tkhd`/`edts` and its
    /// `mdia`.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// Each [`TrackReference`] becomes one child atom whose FourCC is
    /// the reference type (`chap`, `tmcd`, `sync`, …) and whose body is
    /// the packed list of referenced track ids. A track may carry
    /// several references of distinct types; pass them all in one call.
    ///
    /// Every referenced id is validated against the set of tracks added
    /// so far — a reference to an unknown or out-of-range track id is
    /// rejected and the track is left unchanged. (Self-references are
    /// permitted; some reference types legitimately point at the
    /// declaring track.) Referenced ids must therefore be added before
    /// this call; add the chapter / time-code track first, then declare
    /// the reference on the media track.
    ///
    /// Replaces any references previously attached to the same track.
    /// The references round-trip through the read-side `parse_tref` onto
    /// [`crate::track::Track::references`]. Honoured on the
    /// non-fragmented write path; the fragmented init `moov` ignores
    /// it.
    pub fn set_track_references(
        &mut self,
        track_id: u32,
        references: &[TrackReference],
    ) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_track_references unknown track id {track_id}"
                ))
            })?;
        let track_count = self.tracks.len() as u32;
        for r in references {
            for &id in &r.track_ids {
                if id == 0 || id > track_count {
                    return Err(Error::invalid(format!(
                        "MOV muxer: set_track_references type {:?} references unknown track id {id} (1..={track_count} valid)",
                        core::str::from_utf8(&r.reference_type).unwrap_or("????")
                    )));
                }
            }
        }
        self.tracks[idx].track_references = references.to_vec();
        Ok(())
    }

    /// Attach a Track Aperture Modes box (`tapt`) to a previously-added
    /// video track, emitted as a `trak` child.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The [`Tapt`] carries up to three aperture rectangles in 16.16
    /// fixed-point pixels — `clef` (Clean Aperture), `prof` (Production
    /// Aperture), `enof` (Encoded Pixels) — each emitted as a child sub-
    /// atom (`[ver+flags][width_fp][height_fp]`) only when present.
    /// Use [`TaptDims::from_pixels`] to build a rectangle from integer
    /// dimensions.
    ///
    /// Rejects an unknown `track_id`, a non-video track (aperture modes
    /// only apply to a visual track), and an all-`None` [`Tapt`] (no
    /// rectangle to write). The box round-trips through the read-side
    /// `parse_tapt` onto [`crate::track::Track::tapt`]. Replaces any
    /// aperture previously attached to the same track.
    pub fn set_track_aperture(&mut self, track_id: u32, tapt: Tapt) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_track_aperture unknown track id {track_id}"
                ))
            })?;
        if !matches!(self.tracks[idx].kind, MuxTrackKind::Video { .. }) {
            return Err(Error::invalid(format!(
                "MOV muxer: set_track_aperture track id {track_id} is not a video track (aperture modes apply only to a visual track)"
            )));
        }
        if tapt.clef.is_none() && tapt.prof.is_none() && tapt.enof.is_none() {
            return Err(Error::invalid(
                "MOV muxer: set_track_aperture given an empty Tapt (no clef/prof/enof rectangle)",
            ));
        }
        self.tracks[idx].tapt = Some(tapt);
        Ok(())
    }

    /// Set a custom data-reference table (`dref`) for a previously-added
    /// track (QTFF p. 65 / ISO/IEC 14496-12 §8.7.2).
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// By default a track's `dinf/dref` carries a single self-reference
    /// `url ` entry (`flags=1`, "media is in this file"); this method
    /// replaces that with the supplied list — e.g. to declare external
    /// `url ` / `urn ` storage locations for a reference movie.
    ///
    /// The list must contain **at most one**
    /// [`DataReferenceWrite::SelfRef`]. A track whose sample bytes the
    /// muxer lays into this file's own `mdat` (the default) needs
    /// exactly one — the muxer points every sample entry's
    /// `data_reference_index` at it (1-based, in list order); a table
    /// with **no** self-reference is only valid for a track whose
    /// media lives entirely in an external file, declared via
    /// [`MovMuxer::set_external_media`] (the missing-SelfRef case is
    /// validated at encode time so the two calls may come in either
    /// order). An empty list, or one with several self-refs, is
    /// rejected and the track is left unchanged.
    ///
    /// The table round-trips through the read-side `parse_dref` onto
    /// [`crate::track::Track::data_references`]. Replaces any table
    /// previously set on the same track.
    pub fn set_data_references(
        &mut self,
        track_id: u32,
        references: &[DataReferenceWrite],
    ) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_data_references unknown track id {track_id}"
                ))
            })?;
        if references.is_empty() {
            return Err(Error::invalid(
                "MOV muxer: set_data_references requires at least one entry \
                 (QTFF p. 94: the dref carries the table the sample entries index)",
            ));
        }
        let self_refs = references
            .iter()
            .filter(|r| matches!(r, DataReferenceWrite::SelfRef))
            .count();
        if self_refs > 1 {
            return Err(Error::invalid(format!(
                "MOV muxer: set_data_references allows at most one SelfRef entry; got {self_refs}"
            )));
        }
        // Re-validate a previously-declared external marking: its
        // 1-based index must still name a non-self entry of the new
        // table.
        if let Some(ext) = &self.tracks[idx].external_media {
            check_external_dref_entry(references, ext.data_reference_index)?;
        }
        self.tracks[idx].data_references = references.to_vec();
        Ok(())
    }

    /// Declare a previously-added track's media as living in an
    /// **external** file: the muxer writes none of its sample bytes
    /// into this file's `mdat`; instead the chunk-offset table
    /// (`stco` / `co64`) carries the caller-supplied byte offsets into
    /// the external file and every sample entry's
    /// `data_reference_index` points at `data_reference_index` — a
    /// 1-based entry of the track's custom data-reference table
    /// ([`MovMuxer::set_data_references`], which must have been set
    /// first) that is **not** the self-reference (a `url ` / `urn `
    /// naming the external storage).
    ///
    /// This is the QTFF appendix "Defining Media Data Layouts" shape:
    /// a movie may reference media "stored in a number of files,
    /// including the file itself", addressed "by file offset, rather
    /// than by a data structuring mechanism of a particular file
    /// format" — so the external file may be any format (a raw
    /// elementary stream, another movie, …) and `locations` give each
    /// sample's `(offset, size)` within it. Offsets past the 32-bit
    /// range auto-promote the movie's chunk-offset boxes to `co64`.
    /// Consecutive locations that are byte-contiguous coalesce into
    /// one chunk (run-length `stsc`); non-contiguous locations each
    /// open a new chunk.
    ///
    /// Requirements, validated here:
    /// * `locations.len()` must equal the track's sample count — each
    ///   [`MuxSample`] keeps supplying the timing / keyframe /
    ///   composition-offset axes; the location supplies offset + size.
    /// * Every one of the track's samples must carry **empty** `data`
    ///   (the bytes live in the external file; supplying them here
    ///   would be ambiguous).
    /// * The track must not carry a sample-auxiliary stream
    ///   ([`MovMuxer::set_sample_aux`]) — an aux slab is laid into
    ///   *this* file's `mdat`, which an external track does not touch.
    ///
    /// Validated at encode time: the fragmented write path rejects
    /// external tracks (movie-fragment data addressing is relative to
    /// the fragment's own file). The read-side counterpart is
    /// [`crate::demuxer::MovDemuxer::set_data_reference_opener`];
    /// without an opener the demuxer surfaces such samples as
    /// recoverable `Unsupported` errors.
    ///
    /// Calling again replaces the previous declaration.
    pub fn set_external_media(
        &mut self,
        track_id: u32,
        data_reference_index: u16,
        locations: &[ExternalSampleLocation],
    ) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_external_media unknown track id {track_id}"
                ))
            })?;
        let t = &self.tracks[idx];
        if t.data_references.is_empty() {
            return Err(Error::invalid(
                "MOV muxer: set_external_media requires a data-reference table — \
                 call set_data_references with the external url /urn  entry first",
            ));
        }
        check_external_dref_entry(&t.data_references, data_reference_index)?;
        if locations.len() != t.samples.len() {
            return Err(Error::invalid(format!(
                "MOV muxer: set_external_media has {} locations but track {track_id} has {} samples",
                locations.len(),
                t.samples.len()
            )));
        }
        if let Some(i) = t.samples.iter().position(|s| !s.data.is_empty()) {
            return Err(Error::invalid(format!(
                "MOV muxer: set_external_media track {track_id} sample {i} carries {} bytes of \
                 in-file data; external samples must be added with empty data (the bytes live in \
                 the external file)",
                t.samples[i].data.len()
            )));
        }
        if t.sample_aux.is_some() {
            return Err(Error::invalid(format!(
                "MOV muxer: set_external_media track {track_id} carries a sample-aux stream; \
                 the aux slab is laid into this file's mdat, which an external track does not touch"
            )));
        }
        self.tracks[idx].external_media = Some(ExternalMediaWrite {
            data_reference_index,
            locations: locations.to_vec(),
        });
        Ok(())
    }

    /// Set a previously-added track's media language — the packed
    /// ISO-639-2/T code written to `mdhd.language` (QTFF p. 197 / ISO/IEC
    /// 14496-12 §8.4.2.3).
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// Pack a three-letter code with [`MovMetadata::iso_language`] (e.g.
    /// `MovMetadata::iso_language(*b"eng")`); the default when unset is
    /// [`MDHD_LANGUAGE_UND`] (`"und"`). Round-trips through the read side
    /// onto `mdhd.language` (decodable via `iso_language_tag`). Rejects
    /// an unknown `track_id`.
    pub fn set_track_language(&mut self, track_id: u32, packed_language: u16) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_track_language unknown track id {track_id}"
                ))
            })?;
        self.tracks[idx].media_language = packed_language;
        Ok(())
    }

    /// Set a previously-added track's extended language tag, emitted as
    /// an `elng` (Extended Language Tag Box) in `mdia` (ISO/IEC
    /// 14496-12 §8.4.6).
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// `tag` is an RFC 4646 / BCP 47 tag such as `"en-US"` or
    /// `"zh-Hant"`; it overrides the packed `mdhd.language` when a reader
    /// honours it. Passing an empty string clears the `elng` (no box is
    /// written). Round-trips through `parse_elng` onto
    /// [`crate::track::Track::extended_language`]. Rejects an unknown
    /// `track_id`.
    pub fn set_track_extended_language(&mut self, track_id: u32, tag: &str) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_track_extended_language unknown track id {track_id}"
                ))
            })?;
        self.tracks[idx].extended_language = if tag.is_empty() {
            None
        } else {
            Some(tag.to_string())
        };
        Ok(())
    }

    /// Override the `gmhd/gmin` Generic Media Information header
    /// (QTFF p. 65) of a previously-added time-code or text track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The [`Gmin`] carries the compositing `graphics_mode` (Table 4-2),
    /// the `opcolor` RGB triple consulted by the blend/transparent modes,
    /// and the stereo `balance`. When this is not set, the muxer writes a
    /// default `gmin` (copy graphics mode, no opcolor, centred balance).
    /// Round-trips through `parse_gmin` onto
    /// [`crate::track::Track::gmhd`]'s `gmin` slot.
    ///
    /// Rejects an unknown `track_id` and a video/audio track (those carry
    /// a `vmhd`/`smhd` media header, not a `gmhd`).
    pub fn set_track_gmin(&mut self, track_id: u32, gmin: Gmin) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_track_gmin unknown track id {track_id}"
                ))
            })?;
        if !matches!(
            self.tracks[idx].kind,
            MuxTrackKind::Timecode { .. } | MuxTrackKind::Text { .. }
        ) {
            return Err(Error::invalid(format!(
                "MOV muxer: set_track_gmin on track {track_id} which carries no gmhd (only a time-code or text track has a Generic Media Information header)"
            )));
        }
        self.tracks[idx].gmin = Some(gmin);
        Ok(())
    }

    /// Override the `gmhd/text` media-information header transformation
    /// matrix (QTFF p. 144) of a previously-added text track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// `matrix` is a 9-element 3×3 row-major matrix in the `tkhd`/`text`
    /// fixed-point convention (16.16 for the six scale/skew/translate
    /// entries, 2.30 for the three right-hand-column entries) that maps
    /// each text sample's local coordinates onto the movie canvas. When
    /// this is not set, the muxer writes the identity matrix. Round-trips
    /// through `parse_text_header` onto
    /// [`crate::track::Track::gmhd`]'s `text` slot.
    ///
    /// Rejects an unknown `track_id` and a non-text track.
    pub fn set_text_header_matrix(&mut self, track_id: u32, matrix: [i32; 9]) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_text_header_matrix unknown track id {track_id}"
                ))
            })?;
        if !matches!(self.tracks[idx].kind, MuxTrackKind::Text { .. }) {
            return Err(Error::invalid(format!(
                "MOV muxer: set_text_header_matrix on track {track_id} which is not a text track (the gmhd/text header is defined only for a QuickTime text track)"
            )));
        }
        self.tracks[idx].text_header_matrix = Some(matrix);
        Ok(())
    }

    /// Opt a previously-added track into emitting its sample-size table as
    /// a Compact Sample Size Box (`stz2`, ISO/IEC 14496-12 §8.7.3.3)
    /// rather than the default Sample Size Box (`stsz`, §8.7.3.2).
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// When `enable` is `true` the muxer writes `stz2` with the narrowest
    /// 4 / 8-bit `field_size` that fits every sample whenever that is
    /// genuinely smaller than `stsz` — i.e. the per-sample sizes are not
    /// all equal (a uniform `stsz` already carries no table) and the
    /// largest size fits in 8 bits. Otherwise the muxer transparently
    /// falls back to `stsz`, so enabling this is always safe. Both forms
    /// round-trip onto the same per-sample sizes through the demuxer
    /// (`SampleTable`), and `MovDemuxer::sample_size_source` reports which
    /// box carried them. Rejects an unknown `track_id`.
    pub fn set_compact_sample_size(&mut self, track_id: u32, enable: bool) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_compact_sample_size unknown track id {track_id}"
                ))
            })?;
        self.tracks[idx].compact_sample_size = enable;
        Ok(())
    }

    /// Attach an Independent and Disposable Samples Box (`sdtp`, ISO/IEC
    /// 14496-12 §8.6.4) to a previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// One [`SdtpEntry`] per sample, in decode order; the slice length
    /// must equal the track's sample count (§8.6.4.1 — the box has no
    /// on-disk count field, its row count is implied by the sample-size
    /// table). An empty slice removes any previously-attached table.
    ///
    /// On the next non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to), the muxer emits a `sdtp` inside
    /// the track's `stbl` after the chunk-offset table (one packed byte
    /// per sample). Each entry's four 2-bit dependency fields pack
    /// MSB-first via [`SdtpEntry::to_byte`]; the file round-trips through
    /// the read-side [`crate::sample_table::parse_sdtp`] back onto
    /// `Track::sample_table.sdtp` and the typed
    /// [`crate::demuxer::MovDemuxer::sample_dependency`] accessor.
    ///
    /// The fragmented write path ignores this table (per-fragment
    /// dependency signalling rides the `trun` `sample_flags`).
    pub fn set_sample_dependencies(&mut self, track_id: u32, entries: &[SdtpEntry]) -> Result<()> {
        let idx = self.track_index(track_id, "set_sample_dependencies")?;
        let want = self.tracks[idx].samples.len();
        if !entries.is_empty() && entries.len() != want {
            return Err(Error::invalid(format!(
                "MOV muxer: sdtp has {} rows but track {track_id} has {want} samples",
                entries.len()
            )));
        }
        self.tracks[idx].sdtp = entries.to_vec();
        Ok(())
    }

    /// Attach a Degradation Priority Box (`stdp`, ISO/IEC 14496-12
    /// §8.5.3) to a previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// One 16-bit `priority` per sample, in decode order; the slice
    /// length must equal the track's sample count (§8.5.3.1 — no on-disk
    /// count field). An empty slice removes any previously-attached
    /// table. Higher values mark samples whose degradation has the
    /// greatest effect on quality (§8.5.3.3); the base spec fixes no
    /// numeric range, so the raw `u16` is written verbatim.
    ///
    /// On the next non-fragmented write the muxer emits a `stdp` inside
    /// the track's `stbl` after the chunk-offset table (and after any
    /// `sdtp`). Round-trips through the read-side
    /// [`crate::sample_table::parse_stdp`] back onto
    /// `Track::sample_table.stdp`.
    pub fn set_degradation_priorities(&mut self, track_id: u32, priorities: &[u16]) -> Result<()> {
        let idx = self.track_index(track_id, "set_degradation_priorities")?;
        let want = self.tracks[idx].samples.len();
        if !priorities.is_empty() && priorities.len() != want {
            return Err(Error::invalid(format!(
                "MOV muxer: stdp has {} rows but track {track_id} has {want} samples",
                priorities.len()
            )));
        }
        self.tracks[idx].stdp = priorities.to_vec();
        Ok(())
    }

    /// Attach a Padding Bits Box (`padb`, ISO/IEC 14496-12 §8.7.6) to a
    /// previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// One 3-bit `pad` value per sample (the count of padding bits at the
    /// end of the sample's media payload, §8.7.6.3); the slice length
    /// must equal the track's sample count and every value must be
    /// `0..=7` (the field is 3 bits). An empty slice removes any
    /// previously-attached table.
    ///
    /// On the next non-fragmented write the muxer emits a `padb` inside
    /// the track's `stbl` after the chunk-offset table, packing two rows
    /// per byte (`[reserved:1, pad1:3, reserved:1, pad2:3]`, MSB-first,
    /// §8.7.6.2). When the sample count is odd the trailing low nibble of
    /// the final byte is the `pad2` slot for a non-existent sample,
    /// written zero. Round-trips through the read-side
    /// [`crate::sample_table::parse_padb`] back onto
    /// `Track::sample_table.padb` and the
    /// [`crate::demuxer::MovDemuxer::sample_padding_bits`] accessor.
    pub fn set_padding_bits(&mut self, track_id: u32, pads: &[u8]) -> Result<()> {
        let idx = self.track_index(track_id, "set_padding_bits")?;
        let want = self.tracks[idx].samples.len();
        if !pads.is_empty() && pads.len() != want {
            return Err(Error::invalid(format!(
                "MOV muxer: padb has {} rows but track {track_id} has {want} samples",
                pads.len()
            )));
        }
        if let Some((i, &v)) = pads.iter().enumerate().find(|(_, &v)| v > 7) {
            return Err(Error::invalid(format!(
                "MOV muxer: padb pad value {v} at sample {i} exceeds the 3-bit field (max 7)"
            )));
        }
        self.tracks[idx].padb = pads.to_vec();
        Ok(())
    }

    /// Attach a Shadow Sync Sample Box (`stsh`, ISO/IEC 14496-12 §8.6.3)
    /// to a previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// Each [`StshEntry`] pairs a (normally non-sync) *shadowed* sample
    /// with an alternative *sync* sample whose media can substitute for
    /// it during seeking (§8.6.3.1); both numbers are 1-based. An empty
    /// slice removes any previously-attached table.
    ///
    /// The entries are sorted ascending by `shadowed_sample_number`
    /// before writing (§8.6.3.1 requires the on-disk table to be sorted);
    /// a duplicate `shadowed_sample_number` is rejected because the
    /// lookup would be ambiguous and the read-side
    /// [`crate::sample_table::parse_stsh`] rejects it. Both sample numbers
    /// must be within `1..=sample_count`.
    ///
    /// On the next non-fragmented write the muxer emits a `stsh` inside
    /// the track's `stbl` after the chunk-offset table. Round-trips
    /// through `parse_stsh` back onto `Track::sample_table.stsh` and the
    /// [`crate::demuxer::MovDemuxer::shadow_sync_sample`] accessor.
    pub fn set_shadow_sync_samples(&mut self, track_id: u32, entries: &[StshEntry]) -> Result<()> {
        let idx = self.track_index(track_id, "set_shadow_sync_samples")?;
        let want = self.tracks[idx].samples.len() as u32;
        let mut sorted = entries.to_vec();
        sorted.sort_by_key(|e| e.shadowed_sample_number);
        for w in sorted.windows(2) {
            if w[0].shadowed_sample_number == w[1].shadowed_sample_number {
                return Err(Error::invalid(format!(
                    "MOV muxer: stsh duplicate shadowed_sample_number {}",
                    w[0].shadowed_sample_number
                )));
            }
        }
        for e in &sorted {
            for (label, n) in [
                ("shadowed", e.shadowed_sample_number),
                ("sync", e.sync_sample_number),
            ] {
                if n == 0 || n > want {
                    return Err(Error::invalid(format!(
                        "MOV muxer: stsh {label}_sample_number {n} out of range 1..={want}"
                    )));
                }
            }
        }
        self.tracks[idx].stsh = sorted;
        Ok(())
    }

    /// Attach a Sub-Sample Information Box (`subs`, ISO/IEC 14496-12
    /// §8.7.7) to a previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The table is *sparse* — each [`SubSampleInfo`] names a 1-based
    /// `sample_number` and lists that sample's contiguous sub-sample byte
    /// ranges; samples not named have no sub-sample structure. An empty
    /// slice removes any previously-attached table.
    ///
    /// The rows are sorted ascending by `sample_number` before writing,
    /// then delta-coded (§8.7.7.3 — the first row's `sample_delta` is its
    /// difference from zero, each later row's is the difference from the
    /// previous). A duplicate or zero `sample_number` is rejected (the
    /// sparse delta coding cannot represent it, matching the read-side
    /// [`crate::sample_table::parse_subs`]). The box is written **version
    /// 1** (32-bit `subsample_size`) when any sub-sample exceeds 65535
    /// bytes, else **version 0** (16-bit), so the narrowest form that
    /// fits is chosen.
    ///
    /// On the next non-fragmented write the muxer emits a `subs` inside
    /// the track's `stbl` after the chunk-offset table. Round-trips
    /// through `parse_subs` back onto `Track::sample_table.subs` and the
    /// [`crate::demuxer::MovDemuxer::sub_samples`] accessor.
    pub fn set_sub_samples(&mut self, track_id: u32, rows: &[SubSampleInfo]) -> Result<()> {
        let idx = self.track_index(track_id, "set_sub_samples")?;
        let want = self.tracks[idx].samples.len() as u32;
        let mut sorted = rows.to_vec();
        sorted.sort_by_key(|r| r.sample_number);
        for r in &sorted {
            if r.sample_number == 0 || r.sample_number > want {
                return Err(Error::invalid(format!(
                    "MOV muxer: subs sample_number {} out of range 1..={want}",
                    r.sample_number
                )));
            }
        }
        for w in sorted.windows(2) {
            if w[0].sample_number == w[1].sample_number {
                return Err(Error::invalid(format!(
                    "MOV muxer: subs duplicate sample_number {}",
                    w[0].sample_number
                )));
            }
        }
        self.tracks[idx].subs = sorted;
        Ok(())
    }

    /// Attach a Track Load Settings atom (`load`, QTFF pp. 48–49) to a
    /// previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The [`Load`] carries the movie-timescale preload window
    /// (`preload_start_time` / `preload_duration`, with `0xFFFF_FFFF`
    /// meaning "to the end of the track"), the mutually-exclusive
    /// preload-mode flags ([`LOAD_PRELOAD_ALWAYS`] /
    /// [`LOAD_PRELOAD_IF_ENABLED`]), and the playback-quality hint
    /// bitfield ([`LOAD_HINT_DOUBLE_BUFFER`] / [`LOAD_HINT_HIGH_QUALITY`]
    /// plus any vendor bits). Passing `None` removes any
    /// previously-attached settings (no `load` box).
    ///
    /// On the next non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to), the muxer emits a `load` as an
    /// early `trak` child (after `tapt`, before `edts`). The 16-byte body
    /// is [`Load::to_body_bytes`] — the exact inverse of the read-side
    /// [`crate::track_load::parse_load`] — so the file round-trips onto
    /// `Track::load`. `load` is a QuickTime-only atom (ISO BMFF does not
    /// define it); the fragmented init `moov` does not carry it.
    ///
    /// [`Load`]: crate::track_load::Load
    /// [`LOAD_PRELOAD_ALWAYS`]: crate::track_load::LOAD_PRELOAD_ALWAYS
    /// [`LOAD_PRELOAD_IF_ENABLED`]: crate::track_load::LOAD_PRELOAD_IF_ENABLED
    /// [`LOAD_HINT_DOUBLE_BUFFER`]: crate::track_load::LOAD_HINT_DOUBLE_BUFFER
    /// [`LOAD_HINT_HIGH_QUALITY`]: crate::track_load::LOAD_HINT_HIGH_QUALITY
    pub fn set_track_load_settings(&mut self, track_id: u32, load: Option<Load>) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_load_settings")?;
        self.tracks[idx].load = load;
        Ok(())
    }

    /// Attach a Clipping atom (`clip` > `crgn`, QTFF pp. 43–44) to a
    /// previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The [`Clipping`] carries a single `crgn` Clipping Region — its
    /// QuickDraw bounding-box rectangle (`QdRect`, signed 16-bit
    /// top/left/bottom/right) plus an optional opaque scanline payload
    /// for a non-rectangular mask. Build a rectangular region with
    /// [`crate::clip::ClippingRegion::rectangular`]. Passing `None`
    /// removes any previously-attached clipping (no `clip` box).
    ///
    /// On the next non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to), the muxer emits a `clip` (with
    /// its lone framed `crgn` child) as a `trak` child. The bodies are
    /// [`Clipping::to_body_bytes`] / [`crate::clip::ClippingRegion::
    /// to_body_bytes`] — the exact inverses of the read-side
    /// [`crate::clip::parse_clip`] / `parse_crgn` (the `crgn`'s
    /// `region_size` is recomputed from the scanline length) — so the
    /// file round-trips onto `Track::clipping`. `clip` is a
    /// QuickTime-only atom (ISO BMFF does not define it); the fragmented
    /// init `moov` does not carry it.
    ///
    /// [`Clipping`]: crate::clip::Clipping
    pub fn set_track_clipping(&mut self, track_id: u32, clipping: Option<Clipping>) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_clipping")?;
        self.tracks[idx].clipping = clipping;
        Ok(())
    }

    /// Attach a Track Matte atom (`matt` > `kmat`, QTFF pp. 44–45) to a
    /// previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The [`Matte`] carries a single `kmat` Compressed Matte — a blend
    /// matte (a greyscale image that weights this track against the one
    /// below it during compositing) whose pixels are themselves coded:
    /// a QTFF image description structure (the same on-disk shape as a
    /// video sample description, naming the codec) followed by the
    /// compressed matte data. Passing `None` removes any
    /// previously-attached matte (no `matt` box).
    ///
    /// On the next non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to), the muxer emits a `matt` (with
    /// its lone framed `kmat` child) as a `trak` child. The bodies are
    /// [`Matte::to_body_bytes`] / [`crate::matte::CompressedMatte::
    /// to_body_bytes`] — the exact inverses of the read-side
    /// [`crate::matte::parse_matt`] / `parse_kmat` (the image description
    /// is written verbatim, its leading 4-byte size word the caller's
    /// responsibility, exactly as a video sample description carries its
    /// own size) — so the file round-trips onto `Track::matte`. `matt` is
    /// a QuickTime-only atom (ISO BMFF does not define it); the
    /// fragmented init `moov` does not carry it.
    ///
    /// [`Matte`]: crate::matte::Matte
    pub fn set_track_matte(&mut self, track_id: u32, matte: Option<Matte>) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_matte")?;
        self.tracks[idx].matte = matte;
        Ok(())
    }

    /// Attach ISO BMFF Track Kind boxes (`kind`, §8.10.4) to a
    /// previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// Each [`KindEntry`] labels the track with a `(schemeURI, value)`
    /// role pair — the canonical use is signalling a subtitle / caption
    /// track's intent ("captions" / "subtitles" / "descriptions" / …)
    /// against a WebVTT or DASH role scheme. §8.10.4.1 allows more than
    /// one per track (`Quantity: Zero or more`), so the slice may carry
    /// several. Passing an empty slice removes any previously-attached
    /// kinds.
    ///
    /// The boxes are emitted into the track-level `udta` (the same
    /// container as track metadata), after any metadata items, via
    /// [`KindEntry::to_body_bytes`] — the exact inverse of the read-side
    /// [`crate::kind::parse_kind`]. A file written this way round-trips
    /// onto `Track::kinds` (`MovDemuxer::track_kinds`). `kind` is ISO
    /// BMFF-only (QTFF does not define it).
    pub fn set_track_kinds(&mut self, track_id: u32, kinds: &[KindEntry]) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_kinds")?;
        self.tracks[idx].track_kinds = kinds.to_vec();
        Ok(())
    }

    /// Attach an ISO BMFF Track Selection box (`tsel`, §8.10.3) to a
    /// previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The [`TrackSelection`] carries the `switch_group` (tracks sharing
    /// a non-zero value are switchable alternatives within their
    /// `tkhd.alternate_group`) plus an `attributes` list of FourCCs
    /// declaring which media properties differentiate the group's members
    /// (`cdec` codec, `bitr` bitrate, …) — see the `TSEL_ATTR_*`
    /// constants. §8.10.3.1 allows at most one `tsel` per track
    /// (`Quantity: Zero or one`). Passing `None` removes any
    /// previously-attached selection.
    ///
    /// The box is emitted into the track-level `udta` (the same
    /// container as track metadata + `kind` boxes) via
    /// [`TrackSelection::to_body_bytes`] — the exact inverse of the
    /// read-side [`crate::track_selection::parse_tsel`]. A file written
    /// this way round-trips onto `Track::track_selection`
    /// (`MovDemuxer::track_selection`). `tsel` is ISO BMFF-only (QTFF
    /// does not define it).
    ///
    /// [`TrackSelection`]: crate::track_selection::TrackSelection
    pub fn set_track_selection(
        &mut self,
        track_id: u32,
        selection: Option<TrackSelection>,
    ) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_selection")?;
        self.tracks[idx].track_selection = selection;
        Ok(())
    }

    /// Attach ISO BMFF Track Group membership declarations (`trgr`,
    /// §8.3.4) to a previously-added track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// Each [`TrackGroupTypeEntry`] declares this track's membership of
    /// one group: a `(track_group_type, track_group_id)` pair shared by
    /// every track in the group (use [`TrackGroupTypeEntry::msrc`] for
    /// the base-spec multi-source-presentation group, §8.3.4.3). A track
    /// may belong to several groups, so the slice may carry more than one
    /// entry. Passing an empty slice removes any previously-attached
    /// groups.
    ///
    /// On the next non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to), the muxer emits a `trgr`
    /// (TrackGroupBox) as a `trak` child — one framed `TrackGroupTypeBox`
    /// FullBox child per entry, via
    /// [`TrackGroupTypeEntry::to_framed_atom`] — placed after `tref` and
    /// before `mdia`. The bodies are the exact inverses of the read-side
    /// [`crate::track_group::parse_trgr`] /
    /// [`crate::track_group::parse_track_group_type`], so the file
    /// round-trips onto `Track::track_groups`
    /// (`MovDemuxer::track_groups_for`). `trgr` is ISO BMFF-only (QTFF
    /// does not define it).
    pub fn set_track_groups(
        &mut self,
        track_id: u32,
        groups: &[TrackGroupTypeEntry],
    ) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_groups")?;
        self.tracks[idx].track_groups = groups.to_vec();
        Ok(())
    }

    /// Attach movie-level user-data metadata, emitted as a `moov/udta`
    /// after the last `trak` (QTFF pp. 36–38 / ISO/IEC 14496-12
    /// §8.10.1).
    ///
    /// The `items` are written in order; consecutive (and
    /// non-consecutive) [`MovMetadata::intl_text`] items sharing a
    /// FourCC are coalesced into a single `©XXX` atom carrying one
    /// language record each, per QTFF p. 38. A file written this way
    /// round-trips through the read side and surfaces on
    /// [`crate::demuxer::MovDemuxer::user_data`].
    ///
    /// Replaces any movie-level metadata set by a previous call. Only
    /// the non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to) path emits it; the fragmented
    /// init `moov` is left metadata-free.
    pub fn set_metadata(&mut self, items: &[MovMetadata]) {
        self.metadata = items.to_vec();
    }

    /// Override the movie display matrix written into `mvhd` (QTFF
    /// p. 33 "Matrix structure"; layout `[a b u; c d v; tx ty w]` —
    /// first 8 entries 16.16 fixed-point, trailing column 2.30).
    /// `None` restores the default identity matrix. Round-trips onto
    /// [`crate::Mvhd::matrix`] / [`crate::Mvhd::rotation`]. Applies
    /// to the non-fragmented `moov` and the fragmented init `moov`.
    pub fn set_movie_matrix(&mut self, matrix: Option<[i32; 9]>) {
        self.movie_matrix = matrix;
    }

    /// Set the QuickTime movie-preview window (QTFF p. 33): the
    /// movie-timescale tick the preview begins at and its duration.
    /// Zeros (the default) mean "no preview" and match the ISO BMFF
    /// `pre_defined` reading of the same bytes (§8.2.2). Round-trips
    /// onto [`crate::Mvhd::preview_time`] / `preview_duration`.
    pub fn set_movie_preview(&mut self, preview_time: u32, preview_duration: u32) {
        self.movie_times[0] = preview_time;
        self.movie_times[1] = preview_duration;
    }

    /// Set the movie poster frame's time value (QTFF p. 33).
    /// Round-trips onto [`crate::Mvhd::poster_time`].
    pub fn set_movie_poster_time(&mut self, poster_time: u32) {
        self.movie_times[2] = poster_time;
    }

    /// Set the current-selection window (QTFF pp. 33–34): start time
    /// value and duration in movie-timescale ticks. Round-trips onto
    /// [`crate::Mvhd::selection_time`] / `selection_duration`.
    pub fn set_movie_selection(&mut self, selection_time: u32, selection_duration: u32) {
        self.movie_times[3] = selection_time;
        self.movie_times[4] = selection_duration;
    }

    /// Set the current time position within the movie (QTFF p. 34).
    /// Round-trips onto [`crate::Mvhd::current_time`].
    pub fn set_movie_current_time(&mut self, current_time: u32) {
        self.movie_times[5] = current_time;
    }

    /// Override a track's `tkhd` display matrix (QTFF p. 42; layout
    /// `[a b u; c d v; tx ty w]`, first 8 entries 16.16 fixed-point,
    /// trailing column 2.30) — e.g. one of the four coarse-rotation
    /// matrices a rotated camera recording carries. `None` restores
    /// the identity default. Round-trips onto [`crate::Tkhd::matrix`]
    /// / [`crate::Tkhd::rotation`]. Honoured on the non-fragmented
    /// and fragmented (init-`trak`) paths.
    pub fn set_track_matrix(&mut self, track_id: u32, matrix: Option<[i32; 9]>) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_matrix")?;
        self.tracks[idx].tkhd_matrix = matrix;
        Ok(())
    }

    /// Set a track's `tkhd` layer — its front-to-back compositing
    /// order (QTFF p. 42: "tracks with lower layer values are
    /// displayed in front of tracks with higher layer values").
    /// Default 0. Round-trips onto [`crate::Tkhd::layer`].
    pub fn set_track_layer(&mut self, track_id: u32, layer: i16) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_layer")?;
        self.tracks[idx].tkhd_layer = layer;
        Ok(())
    }

    /// Set a track's `tkhd` alternate-group id (QTFF p. 42: tracks
    /// sharing a non-zero value are alternates for one another — only
    /// one member plays at a time, chosen by language/quality/etc).
    /// Default 0 = not part of any alternate group. Pairs with
    /// [`MovMuxer::set_track_selection`], whose `tsel` switch-group
    /// semantics operate *within* an alternate group. Round-trips
    /// onto [`crate::Tkhd::alternate_group`] and the demuxer's
    /// `alternate_groups()` fold.
    pub fn set_track_alternate_group(&mut self, track_id: u32, group: i16) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_alternate_group")?;
        self.tracks[idx].tkhd_alternate_group = group;
        Ok(())
    }

    /// Override a track's `tkhd` volume (8.8 fixed-point; 1.0 =
    /// `0x0100`). The default is the QTFF convention: 1.0 for audio
    /// tracks, 0 for everything else. Round-trips onto
    /// [`crate::Tkhd::volume`].
    pub fn set_track_volume(&mut self, track_id: u32, volume: i16) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_volume")?;
        self.tracks[idx].tkhd_volume = Some(volume);
        Ok(())
    }

    /// Override a track's 24-bit `tkhd` flags (QTFF p. 42: `0x1`
    /// enabled, `0x2` in-movie, `0x4` in-preview, `0x8` in-poster;
    /// the historical default is `0x7`). Values past 24 bits are
    /// rejected — the field shares its word with the version byte.
    /// Round-trips onto [`crate::Tkhd::flags`] and the demuxer's
    /// `presentation_tracks()` filter (a track without `0x1 | 0x2`
    /// drops out of the default presentation).
    pub fn set_track_flags(&mut self, track_id: u32, flags: u32) -> Result<()> {
        let idx = self.track_index(track_id, "set_track_flags")?;
        if flags > 0x00FF_FFFF {
            return Err(Error::invalid(format!(
                "MOV muxer: set_track_flags value {flags:#x} exceeds the 24-bit tkhd flags field"
            )));
        }
        self.tracks[idx].tkhd_flags = Some(flags);
        Ok(())
    }

    /// Attach track-level user-data metadata to a previously-added
    /// track, emitted as a `udta` that is the last child of the track's
    /// `trak` (QTFF pp. 36–38 / ISO/IEC 14496-12 §8.10.1).
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// Coalescing of same-FourCC international-text records is the same
    /// as [`MovMuxer::set_metadata`]; the result surfaces on
    /// [`crate::demuxer::Track::user_data`]. Replaces any track-level
    /// metadata from a previous call; returns an error (leaving the
    /// track unchanged) for an unknown `track_id`.
    pub fn set_track_metadata(&mut self, track_id: u32, items: &[MovMetadata]) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_track_metadata unknown track id {track_id}"
                ))
            })?;
        self.tracks[idx].metadata = items.to_vec();
        Ok(())
    }

    /// Attach movie-level **Apple QuickTime Metadata** items, emitted as
    /// a `moov/meta` box (`hdlr` `mdta` + `keys` + `ilst`) after the
    /// last `trak` (and after any `udta` from [`MovMuxer::set_metadata`]
    /// when both are present).
    ///
    /// This is the modern key-value metadata shape (e.g.
    /// `com.apple.quicktime.title`, `com.apple.quicktime.make`), distinct
    /// from the legacy `udta` User Data Box. Each [`MovMetaItem`] becomes
    /// one `keys` declaration (`[namespace][key]`) paired with one `ilst`
    /// entry whose `data` sub-atom carries the typed value. Duplicate
    /// keys are written verbatim (one `keys`/`ilst` slot each), in
    /// `items` order. A file written this way round-trips through the
    /// read side and surfaces on [`crate::demuxer::MovDemuxer::meta`].
    ///
    /// Replaces any Apple metadata set by a previous call. Only the
    /// non-fragmented [`MovMuxer::encode_to_vec`] /
    /// [`write_to`](MovMuxer::write_to) path emits it; the fragmented
    /// init `moov` is left metadata-free.
    pub fn set_apple_metadata(&mut self, items: &[MovMetaItem]) {
        self.apple_metadata = items.to_vec();
    }

    /// Attach **Apple QuickTime Metadata** items to a previously-added
    /// track, emitted as a `trak/meta` box (`hdlr` `mdta` + `keys` +
    /// `ilst`) that is a trailing child of the track's `trak` (after any
    /// track-level `udta` from [`MovMuxer::set_track_metadata`]).
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// The on-disk shape and round-trip semantics are identical to
    /// [`MovMuxer::set_apple_metadata`]; the result surfaces on
    /// [`crate::demuxer::Track::meta`]. Replaces any track-level Apple
    /// metadata from a previous call; returns an error (leaving the track
    /// unchanged) for an unknown `track_id`.
    pub fn set_track_apple_metadata(&mut self, track_id: u32, items: &[MovMetaItem]) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_track_apple_metadata unknown track id {track_id}"
                ))
            })?;
        self.tracks[idx].apple_metadata = items.to_vec();
        Ok(())
    }

    /// Attach typed visual sample-description extension boxes
    /// (`pasp` / `colr` / `clap` / `fiel` / `gama`, ISO/IEC 14496-12
    /// §12.1.4 / §12.1.5, QTFF p. 94) to a previously-added **video**
    /// track.
    ///
    /// `track_id` is the 1-based id returned by [`MovMuxer::add_track`].
    /// Each populated [`VisualExtensions`] field emits its box into the
    /// video `stsd` entry's trailing slot — after the 70-byte fixed body
    /// and after the codec-config `extra_stsd_atoms` passed to
    /// `add_track`, so a decoder-config box (`avcC` / `hvcC`) stays
    /// first. The boxes round-trip back onto
    /// [`crate::track::SampleDescription`]'s `pasp` / `colr` / `clap` /
    /// `fiel` / `gamma` fields. Replaces any extensions from a previous
    /// call.
    ///
    /// Returns an error (leaving the track unchanged) for an unknown
    /// `track_id` or when the track is an audio track — visual
    /// extensions are only defined for a visual sample entry.
    pub fn set_visual_extensions(&mut self, track_id: u32, ext: VisualExtensions) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: set_visual_extensions unknown track id {track_id}"
                ))
            })?;
        if !matches!(self.tracks[idx].kind, MuxTrackKind::Video { .. }) {
            return Err(Error::invalid(format!(
                "MOV muxer: set_visual_extensions on non-video track {track_id} (visual extensions are only defined for a visual sample entry)"
            )));
        }
        self.tracks[idx].visual_extensions = ext;
        Ok(())
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
        self.validate_data_reference_tables()?;

        // ── Pass 1: predict the file layout to compute mdat chunk
        //    offsets per track. The `ftyp` is fixed at 28 bytes
        //    (8-byte header + 20-byte body: 4 major + 4 minor + 3 ×
        //    4-byte compat brands). The `mdat` header is 8 bytes when
        //    the body fits in u32, 16 bytes otherwise (size==1 +
        //    extended u64).
        //
        //    Per-track mdat layout when a sample-aux stream is attached:
        //    `[sample data…][aux slab…]`, the aux blobs contiguous in
        //    sample order immediately after the track's sample data so a
        //    single-entry `saio` (§8.7.9.3) addresses the whole slab.
        let ftyp_size: u64 = 28;
        let segments = self.plan_mdat_segments()?;
        let mdat_body_len: u64 = segments.iter().map(|s| self.segment_len(s)).sum();
        let mdat_header_len: u64 = if mdat_body_len + 8 > u32::MAX as u64 {
            16
        } else {
            8
        };
        if self.moov_placement == MoovPlacement::BeforeMdat {
            return self.encode_faststart(&segments, ftyp_size, mdat_body_len, mdat_header_len);
        }
        let mdat_payload_offset = ftyp_size + mdat_header_len;

        // Resolve every chunk's / aux slab's absolute file offset by
        // walking the planned segments once. `stco`/`co64` and the
        // §8.7.9.3 Sample-Table-scope `saio` all require absolute
        // offsets.
        let (chunks, aux_offsets) = self.layout_segments(&segments, mdat_payload_offset);
        let need_co64 = chunks.iter().flatten().any(|c| c.offset > u32::MAX as u64);

        // ── Pass 2: emit bytes.
        let mut out =
            Vec::with_capacity((ftyp_size + mdat_header_len + mdat_body_len + 4096) as usize);
        out.extend_from_slice(&build_ftyp());
        emit_mdat_header(&mut out, mdat_body_len);
        self.emit_segments(&mut out, &segments);
        let moov = build_moov(self, &chunks, need_co64, &aux_offsets);
        if self.compress_movie_resource {
            // QTFF p. 30: the complete movie resource is the full `moov`
            // atom (its 8-byte header included). p. 81: compress that
            // resource losslessly and wrap the result in `moov > cmov >
            // dcom + cmvd`. The chunk offsets above are file-absolute
            // and anchored to `mdat` (laid down before `moov`), so they
            // remain valid regardless of the compressed `moov`'s size.
            let mut movie_resource = Vec::with_capacity(8 + moov.len());
            push_atom(&mut movie_resource, *b"moov", &moov);
            let cmov = crate::cmov::compress(&movie_resource)?;
            // moov > cmov > dcom + cmvd (QTFF p. 81 Table 2-5): wrap the
            // dcom/cmvd pair in a `cmov` atom, then that in the outer
            // `moov`.
            let mut compressed_moov = Vec::new();
            push_atom(&mut compressed_moov, *b"cmov", &cmov.to_body_bytes());
            push_atom(&mut out, *b"moov", &compressed_moov);
        } else {
            push_atom(&mut out, *b"moov", &moov);
        }
        Ok(out)
    }

    /// Faststart emission: `ftyp` + `moov` + `mdat`, the "global movie
    /// information at the beginning of the file" layout QTFF's
    /// "Optimizing … for Web Playback" section (p. 365) credits with
    /// enabling progressive playback — a reader that consumes the file
    /// front-to-back has the complete sample tables before any sample
    /// bytes arrive.
    ///
    /// Chunk offsets are file-absolute, so they now depend on the
    /// `moov`'s own byte size — which in turn depends only on the
    /// *widths* chosen for the offset-bearing boxes (`stco` vs `co64`,
    /// `saio` v0 vs v1), never on the offset values themselves. The
    /// sizing loop exploits that: build a candidate `moov`, recompute
    /// the mdat base from its size, and repeat until the size is
    /// stable. Widths only ever widen as offsets grow, so the loop is
    /// monotone and converges in a handful of iterations.
    fn encode_faststart(
        &self,
        segments: &[MdatSegment],
        ftyp_size: u64,
        mdat_body_len: u64,
        mdat_header_len: u64,
    ) -> Result<Vec<u8>> {
        if self.compress_movie_resource {
            // A zlib-compressed movie resource's size depends on the
            // byte *values* of the chunk offsets it contains, so the
            // fixed-point sizing above has no stable solution. QTFF's
            // own compressed-movie layout (p. 81) is described with the
            // movie data preceding the compressed resource.
            return Err(Error::invalid(
                "MOV muxer: moov-before-mdat placement cannot be combined with \
                 a compressed movie resource (cmov) — the compressed size would \
                 depend on the chunk offsets it stores",
            ));
        }
        let mut moov_atom_len: u64 = 0;
        for _ in 0..8 {
            let base = ftyp_size + moov_atom_len + mdat_header_len;
            let (chunks, aux_offsets) = self.layout_segments(segments, base);
            let need_co64 = chunks.iter().flatten().any(|c| c.offset > u32::MAX as u64);
            let moov = build_moov(self, &chunks, need_co64, &aux_offsets);
            let new_len = moov.len() as u64 + 8;
            if new_len == moov_atom_len {
                let mut out = Vec::with_capacity(
                    (ftyp_size + moov_atom_len + mdat_header_len + mdat_body_len) as usize,
                );
                out.extend_from_slice(&build_ftyp());
                push_atom(&mut out, *b"moov", &moov);
                emit_mdat_header(&mut out, mdat_body_len);
                self.emit_segments(&mut out, segments);
                return Ok(out);
            }
            moov_atom_len = new_len;
        }
        // Unreachable in practice: each iteration either stabilises or
        // widens at least one offset field, and there are finitely many.
        Err(Error::invalid(
            "MOV muxer: faststart moov sizing did not converge",
        ))
    }

    /// Cross-track validation of the data-reference tables, shared by
    /// the non-fragmented and fragmented encode entry points. A track
    /// that is *not* marked external must resolve a self-reference for
    /// its sample entries to point at — either the implicit default
    /// single-entry table (empty list) or a custom table carrying one
    /// `SelfRef` (QTFF p. 94: the self-reference flag "indicates that
    /// the media's data is in the same file as the movie atom").
    fn validate_data_reference_tables(&self) -> Result<()> {
        for (i, t) in self.tracks.iter().enumerate() {
            if t.external_media.is_none()
                && !t.data_references.is_empty()
                && !t
                    .data_references
                    .iter()
                    .any(|r| matches!(r, DataReferenceWrite::SelfRef))
            {
                return Err(Error::invalid(format!(
                    "MOV muxer: track {} has a data-reference table with no SelfRef entry but \
                     its samples are written into this file's mdat — add a SelfRef to the table \
                     or declare the track external via set_external_media",
                    i + 1
                )));
            }
        }
        Ok(())
    }

    /// Plan the non-fragmented `mdat` payload as an ordered segment
    /// list, per the configured [`ChunkStrategy`].
    ///
    /// * [`ChunkStrategy::SingleChunkPerTrack`] — one chunk per track
    ///   (all of the track's samples), tracks back-to-back in
    ///   `add_track` order, each track's aux slab (if any) directly
    ///   after its sample data: the historical round-19 layout.
    /// * [`ChunkStrategy::InterleaveByMovieTicks`] — time-ordered
    ///   interleave per QTFF p. 358; aux slabs follow the last chunk,
    ///   one contiguous slab per track in `add_track` order (keeping
    ///   the single-entry §8.7.9.3 `saio` contract).
    fn plan_mdat_segments(&self) -> Result<Vec<MdatSegment>> {
        match self.chunk_strategy {
            ChunkStrategy::SingleChunkPerTrack => {
                let mut segs = Vec::with_capacity(self.tracks.len() * 2);
                for (track_idx, t) in self.tracks.iter().enumerate() {
                    // External tracks contribute no mdat bytes — their
                    // chunk offsets point into the external file and
                    // are filled in by `layout_segments`.
                    if t.external_media.is_some() {
                        continue;
                    }
                    segs.push(MdatSegment::Chunk {
                        track_idx,
                        first_sample: 0,
                        sample_count: t.samples.len(),
                    });
                    if t.sample_aux.is_some() {
                        segs.push(MdatSegment::AuxSlab { track_idx });
                    }
                }
                Ok(segs)
            }
            ChunkStrategy::InterleaveByMovieTicks(period) => self.plan_interleaved(period),
        }
    }

    /// Time-ordered interleave plan (QTFF "Interleaving Movie Data",
    /// p. 358): cut each track into chunks spanning at most `period`
    /// movie-timescale ticks of decode time, then merge every track's
    /// chunk list by ascending chunk start time (movie ticks,
    /// round-half-up rescale from media ticks) so that data for a
    /// given movie time sits close together in the file. Per-track
    /// chunk order — and therefore per-track sample order — is
    /// preserved, which keeps `stsc`/`stco` chunk numbering (§8.7.4 /
    /// §8.7.5) monotone in file position.
    fn plan_interleaved(&self, period: u32) -> Result<Vec<MdatSegment>> {
        if period == 0 {
            return Err(Error::invalid(
                "MOV muxer: InterleaveByMovieTicks period must be > 0",
            ));
        }
        // Per-track chunk lists, each entry annotated with the chunk's
        // start time rescaled into movie ticks.
        struct PlannedRun {
            first_sample: usize,
            sample_count: usize,
            start_movie: u64,
        }
        let movie_ts = self.movie_timescale as u128;
        let mut per_track: Vec<Vec<PlannedRun>> = Vec::with_capacity(self.tracks.len());
        for t in &self.tracks {
            // External tracks have no bytes in this file to interleave;
            // their chunk lists come from the caller-supplied external
            // locations in `layout_segments`.
            if t.external_media.is_some() {
                per_track.push(Vec::new());
                continue;
            }
            let media_ts = t.media_timescale as u128;
            // The interleave period in this track's media timescale,
            // round-half-up, floored at 1 tick so zero-length periods
            // can't loop forever.
            let period_media: u64 =
                (((period as u128) * media_ts + movie_ts / 2) / movie_ts).max(1) as u64;
            let to_movie = |dts_media: u64| -> u64 {
                ((dts_media as u128 * movie_ts + media_ts / 2) / media_ts) as u64
            };
            let mut runs: Vec<PlannedRun> = Vec::new();
            let mut dts: u64 = 0; // decode time, media ticks
            let mut chunk_start_dts: u64 = 0;
            let mut first_sample: usize = 0;
            for (i, s) in t.samples.iter().enumerate() {
                dts = dts.saturating_add(s.duration as u64);
                // Close the chunk once it spans the period (or at the
                // end of the track).
                let last = i + 1 == t.samples.len();
                if last || dts - chunk_start_dts >= period_media {
                    runs.push(PlannedRun {
                        first_sample,
                        sample_count: i + 1 - first_sample,
                        start_movie: to_movie(chunk_start_dts),
                    });
                    first_sample = i + 1;
                    chunk_start_dts = dts;
                }
            }
            per_track.push(runs);
        }
        // K-way merge by (start_movie, track order). Walking cursors
        // rather than sorting guarantees per-track order is preserved
        // even when zero-duration samples make start times collide.
        let mut cursors = vec![0usize; per_track.len()];
        let total_runs: usize = per_track.iter().map(Vec::len).sum();
        let mut segs = Vec::with_capacity(total_runs + self.tracks.len());
        for _ in 0..total_runs {
            let mut best: Option<usize> = None;
            for (track_idx, runs) in per_track.iter().enumerate() {
                if cursors[track_idx] >= runs.len() {
                    continue;
                }
                let start = runs[cursors[track_idx]].start_movie;
                let better = match best {
                    None => true,
                    Some(b) => start < per_track[b][cursors[b]].start_movie,
                };
                if better {
                    best = Some(track_idx);
                }
            }
            let track_idx = best.expect("run count and cursors agree");
            let run = &per_track[track_idx][cursors[track_idx]];
            segs.push(MdatSegment::Chunk {
                track_idx,
                first_sample: run.first_sample,
                sample_count: run.sample_count,
            });
            cursors[track_idx] += 1;
        }
        // Aux slabs after all sample chunks, one contiguous slab per
        // track in `add_track` order — the single-entry `saio`
        // (§8.7.9.3) requires each track's aux info to be contiguous
        // in sample order, not adjacent to its sample data.
        for (track_idx, t) in self.tracks.iter().enumerate() {
            if t.sample_aux.is_some() {
                segs.push(MdatSegment::AuxSlab { track_idx });
            }
        }
        Ok(segs)
    }

    /// Byte length of one planned `mdat` segment.
    fn segment_len(&self, seg: &MdatSegment) -> u64 {
        match seg {
            MdatSegment::Chunk {
                track_idx,
                first_sample,
                sample_count,
            } => self.tracks[*track_idx].samples[*first_sample..*first_sample + *sample_count]
                .iter()
                .map(|s| s.data.len() as u64)
                .sum(),
            MdatSegment::AuxSlab { track_idx } => self.tracks[*track_idx]
                .sample_aux
                .as_ref()
                .map(|a| a.per_sample.iter().map(|b| b.len() as u64).sum())
                .unwrap_or(0),
        }
    }

    /// Walk the planned segments once from `base` (the absolute file
    /// offset of the first `mdat` payload byte), assigning absolute
    /// offsets: returns the per-track chunk lists (in per-track chunk
    /// order — segments preserve each track's media order) and the
    /// per-track aux-slab offsets.
    fn layout_segments(
        &self,
        segments: &[MdatSegment],
        base: u64,
    ) -> (Vec<Vec<TrackChunk>>, Vec<Option<u64>>) {
        let mut chunks: Vec<Vec<TrackChunk>> = vec![Vec::new(); self.tracks.len()];
        let mut aux_offsets: Vec<Option<u64>> = vec![None; self.tracks.len()];
        let mut cursor = base;
        for seg in segments {
            match seg {
                MdatSegment::Chunk {
                    track_idx,
                    sample_count,
                    ..
                } => chunks[*track_idx].push(TrackChunk {
                    offset: cursor,
                    sample_count: *sample_count as u32,
                }),
                MdatSegment::AuxSlab { track_idx } => aux_offsets[*track_idx] = Some(cursor),
            }
            cursor += self.segment_len(seg);
        }
        // External tracks: chunk offsets are into the *external* file
        // (QTFF p. 79 — each sample's "byte offset from the data
        // reference"), independent of this file's layout. Contiguous
        // locations coalesce into multi-sample chunks so the
        // run-length `stsc` stays compact.
        for (track_idx, t) in self.tracks.iter().enumerate() {
            if let Some(ext) = &t.external_media {
                chunks[track_idx] = coalesce_external_chunks(&ext.locations);
            }
        }
        (chunks, aux_offsets)
    }

    /// Append the planned segments' bytes to `out`, in segment order —
    /// the mirror of [`MovMuxer::layout_segments`].
    fn emit_segments(&self, out: &mut Vec<u8>, segments: &[MdatSegment]) {
        for seg in segments {
            match seg {
                MdatSegment::Chunk {
                    track_idx,
                    first_sample,
                    sample_count,
                } => {
                    for s in &self.tracks[*track_idx].samples
                        [*first_sample..*first_sample + *sample_count]
                    {
                        out.extend_from_slice(&s.data);
                    }
                }
                MdatSegment::AuxSlab { track_idx } => {
                    if let Some(aux) = &self.tracks[*track_idx].sample_aux {
                        for blob in &aux.per_sample {
                            out.extend_from_slice(blob);
                        }
                    }
                }
            }
        }
    }

    /// Emit the fragmented file to a writer.
    ///
    /// Requires [`MovMuxer::with_fragmentation`] to have been called.
    /// Layout: `ftyp` (with `iso5` / `dash` brands) → init `moov` (no
    /// in-stbl samples, `mvex/trex` per track) → one `moof` + `mdat`
    /// pair per fragment slice. Returns total bytes written.
    pub fn write_to_fragmented<W: Write>(&self, w: &mut W) -> Result<u64> {
        let bytes = self.encode_fragmented_to_vec()?;
        w.write_all(&bytes).map_err(Error::from)?;
        Ok(bytes.len() as u64)
    }

    /// Two-pass build of a fragmented MP4. Each fragment is laid out
    /// in two passes: a sizing pass that determines the `moof` byte
    /// length so the `trun.data_offset` can point at the first byte
    /// of the trailing `mdat`'s payload, then an emit pass that
    /// writes the boxes verbatim.
    ///
    /// Requires [`MovMuxer::with_fragmentation`]. Errors out when the
    /// fragmentation policy is `None`, when there are zero tracks,
    /// when any track has zero samples, or when the policy threshold
    /// is zero (would slice every sample into its own fragment for
    /// `ByFrameCount(0)`).
    pub fn encode_fragmented_to_vec(&self) -> Result<Vec<u8>> {
        let mode = self
            .fragmentation
            .ok_or_else(|| Error::invalid("MOV muxer: fragmentation policy not set"))?;
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
            if t.external_media.is_some() {
                return Err(Error::invalid(format!(
                    "MOV muxer: track {} is marked external (set_external_media); the \
                     fragmented write path addresses sample data relative to each \
                     fragment's own file and does not support external data references",
                    i + 1
                )));
            }
        }
        self.validate_data_reference_tables()?;
        match mode {
            FragmentationMode::ByDuration(0) => {
                return Err(Error::invalid(
                    "MOV muxer: ByDuration threshold must be > 0",
                ))
            }
            FragmentationMode::ByFrameCount(0) => {
                return Err(Error::invalid(
                    "MOV muxer: ByFrameCount threshold must be > 0",
                ))
            }
            _ => {}
        }

        // ── Slice each track's flat sample list into per-fragment
        //    runs. The primary track (index 0) drives the slice
        //    boundaries; secondary tracks (audio paired to a video
        //    primary) walk their samples until accumulated DTS in
        //    primary-track-timescale ticks crosses the primary's
        //    per-fragment boundary. Both rules degenerate to "every
        //    sample is its own fragment row of the only track" for a
        //    single-track input.
        let fragments = slice_fragments(&self.tracks, mode);

        // ── Pass 1: emit ftyp + init-moov (no samples yet).
        let mut out = Vec::new();
        out.extend_from_slice(&build_ftyp_fragmented());
        let init_moov = build_init_moov(self);
        push_atom(&mut out, *b"moov", &init_moov);

        // ── Pass 2: per-fragment moof + mdat with two sub-passes
        //    each so the trun.data_offset can be computed before the
        //    moof is emitted.
        let mut sequence_number: u32 = 1;
        for fragment in &fragments {
            // Skip empty fragments (can happen for trailing audio
            // when primary track ended before the audio did — those
            // residual audio rows roll into the final fragment, so
            // genuinely empty fragments don't occur, but the guard
            // is cheap).
            if fragment.iter().all(|r| r.samples.is_empty()) {
                continue;
            }
            // Sizing pass: build each traf with a placeholder
            // data_offset = 0, measure the resulting moof, then
            // recompute each traf with the correct offset.
            //
            // Per §8.8.7.1 with default-base-is-moof: each track's
            // base-data-offset = position of the enclosing moof's
            // first byte. Each trun.data_offset is then "offset from
            // the moof start to the run's first sample". With one
            // run per track per moof, that equals
            //   moof_size + 8 (mdat header) + (cumulative bytes of
            //   preceding tracks' samples in this fragment).
            let moof_size = measure_moof(sequence_number, fragment);
            let mut traf_data_offsets: Vec<i32> = Vec::with_capacity(fragment.len());
            let mut cumulative_in_mdat: u64 = 0;
            for run in fragment {
                let do_val = (moof_size + 8 + cumulative_in_mdat) as i32;
                traf_data_offsets.push(do_val);
                cumulative_in_mdat += run.samples.iter().map(|s| s.data.len() as u64).sum::<u64>();
            }
            // The auxiliary-information slabs (§8.7.8 / §8.7.9, §8.8.14)
            // sit in the same `mdat` *after* every track's sample data,
            // contiguous per traf in track order. Each traf's single
            // `saio` offset is moof-relative (default-base-is-moof), so
            // it is `moof_size + 8 (mdat header) + total_sample_bytes +
            // (cumulative aux bytes of preceding trafs)`.
            let total_sample_bytes = cumulative_in_mdat;
            let mut traf_saio_offsets: Vec<u64> = Vec::with_capacity(fragment.len());
            let mut cumulative_aux: u64 = 0;
            for run in fragment {
                let off = moof_size + 8 + total_sample_bytes + cumulative_aux;
                traf_saio_offsets.push(off);
                if let Some(blobs) = run.aux {
                    cumulative_aux += blobs.iter().map(|b| b.len() as u64).sum::<u64>();
                }
            }
            // Emit the moof with the real offsets.
            let moof = build_moof(
                sequence_number,
                fragment,
                &traf_data_offsets,
                &traf_saio_offsets,
            );
            debug_assert_eq!(
                (moof.len() as u64) + 8,
                moof_size,
                "moof sizing pass mismatch"
            );
            push_atom(&mut out, *b"moof", &moof);
            // Emit the mdat: all tracks' samples concatenated in track
            // order, then all tracks' aux slabs in track order (matching
            // the trun.data_offset and saio offsets baked in above).
            let mut mdat_payload: Vec<u8> = Vec::new();
            for run in fragment {
                for s in run.samples {
                    mdat_payload.extend_from_slice(&s.data);
                }
            }
            for run in fragment {
                if let Some(blobs) = run.aux {
                    for blob in blobs {
                        mdat_payload.extend_from_slice(blob);
                    }
                }
            }
            push_atom(&mut out, *b"mdat", &mdat_payload);
            sequence_number = sequence_number.saturating_add(1);
        }
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

fn build_moov(
    m: &MovMuxer,
    chunks: &[Vec<TrackChunk>],
    need_co64: bool,
    aux_offsets: &[Option<u64>],
) -> Vec<u8> {
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(m));
    for (idx, t) in m.tracks.iter().enumerate() {
        let trak = build_trak(
            t,
            (idx as u32) + 1,
            m.movie_timescale,
            &chunks[idx],
            need_co64,
            aux_offsets[idx],
        );
        push_atom(&mut moov, *b"trak", &trak);
    }
    // Movie-level user-data box after the last `trak` (QTFF p. 32,
    // Figure 2-1: `udta` follows the track atoms inside `moov`).
    if !m.metadata.is_empty() {
        push_atom(&mut moov, *b"udta", &build_udta(&m.metadata));
    }
    // Movie-level Apple QuickTime Metadata box (`hdlr`+`keys`+`ilst`),
    // after `udta`. Read side dispatches a `moov`-scope `meta` to the
    // Apple parser (`crate::media_meta::parse_keys` / `parse_ilst`).
    if !m.apple_metadata.is_empty() {
        push_atom(&mut moov, *b"meta", &build_meta(&m.apple_metadata));
    }
    moov
}

/// Build a movie-level Apple QuickTime Metadata box (`meta`) payload
/// from a list of [`MovMetaItem`]s. Emits the three Apple children in
/// order: `hdlr` (handler subtype `mdta`), `keys` (the ordered key
/// declarations), and `ilst` (the matching typed values).
///
/// No leading `[ver+flags]` FullBox header is written — Apple's
/// `moov`/`trak` `meta` omits it (the read-side `parse_meta_atom` peeks
/// the first child header and proceeds directly when it is a valid
/// sub-atom, which the `hdlr` first child always is).
///
/// The `ilst` entry at 1-based index `i` references the `keys`
/// declaration at the same index, mirroring
/// [`crate::media_meta::parse_ilst`]. Duplicate keys are emitted as
/// independent slots, preserving `items` order.
fn build_meta(items: &[MovMetaItem]) -> Vec<u8> {
    let mut meta = Vec::new();
    push_atom(&mut meta, *b"hdlr", &build_meta_hdlr());
    push_atom(&mut meta, *b"keys", &build_keys(items));
    push_atom(&mut meta, *b"ilst", &build_ilst(items));
    meta
}

/// Build the `hdlr` (Handler Reference Box) payload for an Apple
/// metadata `meta` box: handler/component subtype `mdta` (QTFF p. 57 /
/// ISO/IEC 14496-12 §8.4.3). Layout mirrors `build_hdlr` but with a
/// `mdta` subtype and an empty (zero-length, NUL-terminated) name —
/// matching the read-side `parse_hdlr` / `Hdlr::is_metadata`.
fn build_meta_hdlr() -> Vec<u8> {
    let mut p = Vec::with_capacity(25);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&0u32.to_be_bytes()); // pre_defined / component_type
    p.extend_from_slice(b"mdta"); // handler_type / component_subtype
    p.extend_from_slice(&[0u8; 12]); // reserved (manuf + flags + flags_mask)
    p.push(0); // counted-Pascal name length 0
    p
}

/// Build the `keys` (Metadata Item Keys Box) payload — a FullBox
/// header (`[ver+flags=4]`) + `[entry_count:4]` then one
/// `[size:4][namespace:4][key_value: size-8]` record per item, in
/// `items` order. Exactly the layout `crate::media_meta::parse_keys`
/// consumes.
fn build_keys(items: &[MovMetaItem]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&(items.len() as u32).to_be_bytes()); // entry_count
    for item in items {
        let key_bytes = item.key.as_bytes();
        let size = (8 + key_bytes.len()) as u32; // [size:4][ns:4][key]
        p.extend_from_slice(&size.to_be_bytes());
        p.extend_from_slice(&item.namespace);
        p.extend_from_slice(key_bytes);
    }
    p
}

/// Build the `ilst` (Metadata Item List Box) payload — one entry per
/// item, each `[entry_size:4][key_index:4]` followed by a single
/// `data` sub-atom `[size:4]['data'][type_code:4][locale:4][value]`.
/// `key_index` is the 1-based index into the parallel `keys` list, so
/// the read-side `parse_ilst` resolves it back to the same key.
fn build_ilst(items: &[MovMetaItem]) -> Vec<u8> {
    let mut p = Vec::new();
    for (i, item) in items.iter().enumerate() {
        // `data` sub-atom: [size:4]['data'][type:4][locale:4][value].
        let data_size = (16 + item.value.len()) as u32;
        let mut data = Vec::with_capacity(data_size as usize);
        data.extend_from_slice(&data_size.to_be_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&item.type_code.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes()); // locale = 0
        data.extend_from_slice(&item.value);
        // ilst entry: [entry_size:4][key_index:4][data...].
        let entry_size = (8 + data.len()) as u32;
        p.extend_from_slice(&entry_size.to_be_bytes());
        p.extend_from_slice(&((i as u32) + 1).to_be_bytes()); // 1-based key index
        p.extend_from_slice(&data);
    }
    p
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
    // 36-byte movie display matrix @ 36..72 (QTFF p. 33) — the
    // caller-supplied override or identity (a=1.0, d=1.0, w=1.0).
    write_movie_matrix(&mut p[36..72], m.movie_matrix.as_ref());
    // The six QuickTime preview/poster/selection/current time fields
    // @ 72..96 (QTFF pp. 33–34; zeros = the ISO BMFF `pre_defined`
    // reading).
    for (i, v) in m.movie_times.iter().enumerate() {
        p[72 + i * 4..76 + i * 4].copy_from_slice(&v.to_be_bytes());
    }
    p[96..100].copy_from_slice(&((m.tracks.len() as u32) + 1).to_be_bytes());
    p
}

/// Serialise a display matrix into a 36-byte `mvhd` / `tkhd` slot:
/// the caller's override verbatim, or the identity matrix
/// (a=1.0, d=1.0 in 16.16; w=1.0 in 2.30).
fn write_movie_matrix(slot: &mut [u8], matrix: Option<&[i32; 9]>) {
    match matrix {
        Some(mx) => {
            for (i, v) in mx.iter().enumerate() {
                slot[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
            }
        }
        None => {
            slot[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // a
            slot[16..20].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // d
            slot[32..36].copy_from_slice(&0x4000_0000u32.to_be_bytes()); // w (2.30)
        }
    }
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
    // QTFF p. 41 (tkhd duration): "this property is derived from the
    // track's edits. The value of this field is equal to the sum of
    // the durations of all of the track's edits. If there is no edit
    // list, then the duration is the sum of the sample durations,
    // converted into the movie timescale." `MuxEdit.track_duration`
    // is already in movie-timescale ticks. The movie header's
    // duration (longest-track rule) inherits the same edit awareness
    // through this helper.
    if !t.edits.is_empty() {
        return t.edits.iter().map(|e| e.track_duration).sum();
    }
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
    chunks: &[TrackChunk],
    need_co64: bool,
    aux_offset: Option<u64>,
) -> Vec<u8> {
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(t, track_id, movie_ts));
    // Track Aperture Modes box (`tapt`) — an early `trak` child (Apple
    // "Movie Atoms"). Carries the clean / production / encoded aperture
    // rectangles; only the populated children are emitted.
    if let Some(tapt) = &t.tapt {
        push_atom(&mut trak, *b"tapt", &build_tapt(tapt));
    }
    // Track Load Settings atom (`load`, QTFF pp. 48–49) — a QuickTime-
    // only early `trak` child carrying the movie-timescale preload
    // window + preload-mode / quality-hint bitfields. Emitted only when
    // the caller attached it via set_track_load_settings.
    if let Some(load) = &t.load {
        push_atom(&mut trak, *b"load", &load.to_body_bytes());
    }
    // Track Clipping atom (`clip` > `crgn`, QTFF pp. 43–44) — a
    // QuickTime-only `trak` child carrying the QuickDraw clipping region.
    // Emitted only when the caller attached it via set_track_clipping.
    if let Some(clipping) = &t.clipping {
        push_atom(&mut trak, *b"clip", &clipping.to_body_bytes());
    }
    // Track Matte atom (`matt` > `kmat`, QTFF pp. 44–45) — a
    // QuickTime-only `trak` child carrying a coded blend matte. Emitted
    // only when the caller attached it via set_track_matte.
    if let Some(matte) = &t.matte {
        push_atom(&mut trak, *b"matt", &matte.to_body_bytes());
    }
    // edts > elst between tkhd and mdia (QTFF p. 46, Figure 2-8: the
    // edit atom precedes the media atom inside a track atom).
    if !t.edits.is_empty() {
        push_atom(&mut trak, *b"edts", &build_edts(&t.edits));
    }
    // Track Reference Box after `edts`, before `mdia` (QTFF p. 41,
    // Figure 2-3: `tref` follows the edit atom and precedes the media
    // atom inside a track atom; ISO/IEC 14496-12 §8.3.3 places `tref`
    // among the optional `trak` children before `mdia`).
    if !t.track_references.is_empty() {
        push_atom(&mut trak, *b"tref", &build_tref(&t.track_references));
    }
    // Track Group box (`trgr`, ISO/IEC 14496-12 §8.3.4) — a `trak`
    // child after `tref`, before `mdia`. One framed TrackGroupTypeBox
    // FullBox child per membership entry. Emitted only when the caller
    // attached groups via set_track_groups.
    if !t.track_groups.is_empty() {
        let mut trgr = Vec::new();
        for entry in &t.track_groups {
            trgr.extend_from_slice(&entry.to_framed_atom());
        }
        push_atom(&mut trak, *b"trgr", &trgr);
    }
    push_atom(
        &mut trak,
        *b"mdia",
        &build_mdia(t, chunks, need_co64, aux_offset),
    );
    // Track-level user-data box as a trailing child of `trak` (QTFF
    // p. 41, Figure 2-3: `udta` is the trailing track-atom child). The
    // payload carries the QTFF metadata items plus any ISO BMFF §8.10.4
    // `kind` (Track Kind) boxes — both share the track-level `udta`
    // container, so they're emitted into one box. No `udta` when neither
    // is present.
    if !t.metadata.is_empty() || !t.track_kinds.is_empty() || t.track_selection.is_some() {
        let mut udta = build_udta(&t.metadata);
        // Append each `kind` child after the metadata items; the read
        // side (find_kinds_in_udta) walks the flat udta atom list.
        for k in &t.track_kinds {
            push_atom(&mut udta, *b"kind", &k.to_body_bytes());
        }
        // Append the single `tsel` Track Selection box (§8.10.3,
        // Quantity: Zero or one); find_tsel_in_udta returns the first.
        if let Some(sel) = &t.track_selection {
            push_atom(&mut udta, *b"tsel", &sel.to_body_bytes());
        }
        push_atom(&mut trak, *b"udta", &udta);
    }
    // Track-level Apple QuickTime Metadata box, after `udta`. Read side
    // dispatches a `trak`-scope `meta` to the same Apple parser.
    if !t.apple_metadata.is_empty() {
        push_atom(&mut trak, *b"meta", &build_meta(&t.apple_metadata));
    }
    trak
}

/// Build a `udta` (User Data Box) payload from a list of metadata
/// items (QTFF pp. 36–38 / ISO/IEC 14496-12 §8.10.1).
///
/// International-text (`©XXX`) items sharing a FourCC are coalesced
/// into one atom carrying one `[text_size:u16][language:u16][text]`
/// record per item, in the order they appear in `items` (QTFF p. 38:
/// "a list of text strings with associated language codes"). Each
/// `text_size` is the byte length of the text only (excludes the
/// 4-byte record header), matching the read-side `parse_intl_text`.
/// `plain_utf8` and `raw` items each emit a single atom.
fn build_udta(items: &[MovMetadata]) -> Vec<u8> {
    let mut udta = Vec::new();
    // First pass: gather, in first-seen order, the FourCC of every
    // international-text item so all its language records land in one
    // atom. Non-intl-text items are emitted inline in `items` order.
    let mut emitted_intl: Vec<[u8; 4]> = Vec::new();
    for item in items {
        match &item.payload {
            MetaPayload::IntlText { .. } => {
                if emitted_intl.contains(&item.fourcc) {
                    continue;
                }
                emitted_intl.push(item.fourcc);
                // Collect every intl-text record sharing this FourCC.
                let mut body = Vec::new();
                for rec in items {
                    if rec.fourcc != item.fourcc {
                        continue;
                    }
                    if let MetaPayload::IntlText { language, text } = &rec.payload {
                        let bytes = text.as_bytes();
                        let len = bytes.len().min(u16::MAX as usize) as u16;
                        body.extend_from_slice(&len.to_be_bytes());
                        body.extend_from_slice(&language.to_be_bytes());
                        body.extend_from_slice(&bytes[..len as usize]);
                    }
                }
                push_atom(&mut udta, item.fourcc, &body);
            }
            MetaPayload::PlainUtf8 { language, text } => {
                // FullBox header (ver=0, flags=0) + packed-ISO lang +
                // UTF-8 text (no terminator).
                let mut body = Vec::with_capacity(6 + text.len());
                body.extend_from_slice(&0u32.to_be_bytes());
                body.extend_from_slice(&language.to_be_bytes());
                body.extend_from_slice(text.as_bytes());
                push_atom(&mut udta, item.fourcc, &body);
            }
            MetaPayload::Raw(bytes) => {
                push_atom(&mut udta, item.fourcc, bytes);
            }
        }
    }
    udta
}

/// Build a `tapt` (Track Aperture Modes Box) payload — one child sub-
/// atom (`clef` / `prof` / `enof`) per populated aperture rectangle
/// (Apple "Movie Atoms"). Inverse of the read-side `parse_tapt`.
fn build_tapt(tapt: &Tapt) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(d) = &tapt.clef {
        push_atom(&mut out, *b"clef", &d.to_body_bytes());
    }
    if let Some(d) = &tapt.prof {
        push_atom(&mut out, *b"prof", &d.to_body_bytes());
    }
    if let Some(d) = &tapt.enof {
        push_atom(&mut out, *b"enof", &d.to_body_bytes());
    }
    out
}

/// Build a `tref` (Track Reference Box) payload — one child atom per
/// [`TrackReference`] (QTFF p. 50 / ISO/IEC 14496-12 §8.3.3).
///
/// Each child is `[size:4][reference_type:4][track_id:4]*` — a plain
/// (non-Full) box whose body is the tightly-packed list of big-endian
/// `u32` referenced track ids. Inverse of the read-side `parse_tref`.
fn build_tref(references: &[TrackReference]) -> Vec<u8> {
    let mut tref = Vec::new();
    for r in references {
        let mut body = Vec::with_capacity(r.track_ids.len() * 4);
        for &id in &r.track_ids {
            body.extend_from_slice(&id.to_be_bytes());
        }
        push_atom(&mut tref, r.reference_type, &body);
    }
    tref
}

/// Build the `edts` (Edit Box) payload — a single child `elst`.
fn build_edts(edits: &[MuxEdit]) -> Vec<u8> {
    let mut edts = Vec::new();
    push_atom(&mut edts, *b"elst", &build_elst(edits));
    edts
}

/// Build the `elst` (Edit List Box) payload (QTFF p. 47 / ISO/IEC
/// 14496-12 §8.6.6).
///
/// Layout: `[version:1][flags:3][entry_count:4]` then `entry_count`
/// triples. Version 0 packs `track_duration` (u32) + `media_time`
/// (signed i32) + `media_rate` (i32); version 1 widens the first two to
/// 64-bit. The version is auto-promoted to 1 the moment any
/// `track_duration` exceeds `u32::MAX` or any `media_time` falls outside
/// the signed-32-bit range, so the value is never truncated. Mirrors
/// the read path [`crate::edit::parse_elst`], which accepts both forms.
fn build_elst(edits: &[MuxEdit]) -> Vec<u8> {
    let need_v1 = edits.iter().any(|e| {
        e.track_duration > u32::MAX as u64
            || e.media_time > i32::MAX as i64
            || e.media_time < i32::MIN as i64
    });
    let mut p = Vec::with_capacity(8 + edits.len() * if need_v1 { 20 } else { 12 });
    p.push(if need_v1 { 1 } else { 0 }); // version
    p.extend_from_slice(&[0u8; 3]); // flags = 0 (QTFF p. 47)
    p.extend_from_slice(&(edits.len() as u32).to_be_bytes());
    for e in edits {
        if need_v1 {
            p.extend_from_slice(&e.track_duration.to_be_bytes());
            p.extend_from_slice(&e.media_time.to_be_bytes());
        } else {
            p.extend_from_slice(&(e.track_duration as u32).to_be_bytes());
            p.extend_from_slice(&(e.media_time as i32).to_be_bytes());
        }
        p.extend_from_slice(&e.media_rate.to_be_bytes());
    }
    p
}

fn build_tkhd(t: &TrackWrite, track_id: u32, movie_ts: u32) -> Vec<u8> {
    // ISO/IEC 14496-12 §8.3.2 — version 0 (32-bit times). 84 bytes
    // payload. Offsets cited from QTFF p. 41.
    let mut p = vec![0u8; 84];
    // version=0; 24-bit flags — the caller override or the historical
    // default enabled(1) + in_movie(2) + in_preview(4) = 7 (QTFF p. 42).
    let flags = t.tkhd_flags.unwrap_or(0x07);
    p[1] = (flags >> 16) as u8;
    p[2] = (flags >> 8) as u8;
    p[3] = flags as u8;
    // creation_time @ 4..8, modification_time @ 8..12 left zero.
    p[12..16].copy_from_slice(&track_id.to_be_bytes());
    // 4 bytes reserved @ 16..20.
    let dur = track_movie_duration(t, movie_ts).min(u32::MAX as u64) as u32;
    p[20..24].copy_from_slice(&dur.to_be_bytes());
    // 8 bytes reserved @ 24..32.
    // layer @ 32..34 (front-to-back compositing order) and
    // alternate_group @ 34..36 (QTFF p. 42), both default 0.
    p[32..34].copy_from_slice(&t.tkhd_layer.to_be_bytes());
    p[34..36].copy_from_slice(&t.tkhd_alternate_group.to_be_bytes());
    // volume @ 36..38: the caller override, else 1.0 for audio
    // tracks and 0 for visual per spec.
    let volume = t
        .tkhd_volume
        .unwrap_or(if matches!(t.kind, MuxTrackKind::Audio { .. }) {
            0x0100
        } else {
            0
        });
    p[36..38].copy_from_slice(&volume.to_be_bytes());
    // 2 bytes reserved @ 38..40.
    // 9-element display matrix @ 40..76 — override or identity.
    write_movie_matrix(&mut p[40..76], t.tkhd_matrix.as_ref());
    // width / height @ 76..84: 16.16 fixed-point, in pixels for video,
    // zero for audio.
    let (w_fp, h_fp) = match &t.kind {
        MuxTrackKind::Video { width, height, .. } => {
            ((*width as u32) << 16, (*height as u32) << 16)
        }
        // Audio, time-code, text, and metadata tracks carry no visual
        // dimensions.
        MuxTrackKind::Audio { .. }
        | MuxTrackKind::Timecode { .. }
        | MuxTrackKind::Text { .. }
        | MuxTrackKind::Metadata { .. }
        | MuxTrackKind::Subtitle { .. }
        | MuxTrackKind::SimpleText { .. }
        | MuxTrackKind::Hint { .. } => (0, 0),
    };
    p[76..80].copy_from_slice(&w_fp.to_be_bytes());
    p[80..84].copy_from_slice(&h_fp.to_be_bytes());
    p
}

fn build_mdia(
    t: &TrackWrite,
    chunks: &[TrackChunk],
    need_co64: bool,
    aux_offset: Option<u64>,
) -> Vec<u8> {
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(t));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(t));
    // Extended Language Tag Box after `hdlr`, before `minf` (ISO/IEC
    // 14496-12 §8.4.6 places `elng` among the `mdia` children).
    if let Some(tag) = &t.extended_language {
        push_atom(&mut mdia, *b"elng", &build_elng(tag));
    }
    push_atom(
        &mut mdia,
        *b"minf",
        &build_minf(t, chunks, need_co64, aux_offset),
    );
    mdia
}

/// Build an `elng` (Extended Language Tag Box) payload — `[ver+flags=4]
/// [NUL-terminated RFC 4646 / BCP 47 UTF-8 tag]` (ISO/IEC 14496-12
/// §8.4.6). Inverse of the read-side `parse_elng`.
fn build_elng(tag: &str) -> Vec<u8> {
    let bytes = tag.as_bytes();
    let mut p = Vec::with_capacity(4 + bytes.len() + 1);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(bytes);
    p.push(0); // NUL terminator
    p
}

fn build_mdhd(t: &TrackWrite) -> Vec<u8> {
    // ISO/IEC 14496-12 §8.4.2 — version 0. 24 bytes payload.
    let mut p = vec![0u8; 24];
    p[12..16].copy_from_slice(&t.media_timescale.to_be_bytes());
    let dur = track_media_duration(t).min(u32::MAX as u64) as u32;
    p[16..20].copy_from_slice(&dur.to_be_bytes());
    // language @ 20..22 — packed ISO-639-2/T (QTFF p. 197 / ISO BMFF
    // §8.4.2.3). Defaults to MDHD_LANGUAGE_UND ("und"); overridable via
    // set_track_language.
    p[20..22].copy_from_slice(&t.media_language.to_be_bytes());
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
        MuxTrackKind::Timecode { .. } => b"tmcd",
        MuxTrackKind::Text { .. } => b"text",
        MuxTrackKind::Metadata { .. } => b"meta",
        MuxTrackKind::Subtitle { .. } => b"subt",
        MuxTrackKind::SimpleText { .. } => b"text",
        MuxTrackKind::Hint { .. } => b"hint",
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

fn build_minf(
    t: &TrackWrite,
    chunks: &[TrackChunk],
    need_co64: bool,
    aux_offset: Option<u64>,
) -> Vec<u8> {
    let mut minf = Vec::new();
    match &t.kind {
        MuxTrackKind::Video { .. } => push_atom(&mut minf, *b"vmhd", &build_vmhd()),
        MuxTrackKind::Audio { .. } => push_atom(&mut minf, *b"smhd", &build_smhd()),
        MuxTrackKind::Timecode { tcmi, .. } => {
            push_atom(&mut minf, *b"gmhd", &build_gmhd_tmcd(tcmi, t.gmin.as_ref()));
        }
        MuxTrackKind::Text { .. } => {
            push_atom(
                &mut minf,
                *b"gmhd",
                &build_gmhd_text(t.gmin.as_ref(), t.text_header_matrix.as_ref()),
            );
        }
        MuxTrackKind::Metadata { .. } => {
            // Null Media Header Box (ISO/IEC 14496-12 §8.4.5.2): an empty
            // FullBox(version=0, flags=0). Metadata tracks carry no
            // specific media header.
            push_atom(&mut minf, *b"nmhd", &0u32.to_be_bytes());
        }
        MuxTrackKind::Subtitle { .. } => {
            // Subtitle Media Header Box (ISO/IEC 14496-12 §12.6.2): an
            // empty FullBox(version=0, flags=0).
            push_atom(&mut minf, *b"sthd", &0u32.to_be_bytes());
        }
        MuxTrackKind::SimpleText { .. } => {
            // Timed-text tracks use a Null Media Header Box (ISO/IEC
            // 14496-12 §12.5.2), distinct from the QuickTime text track's
            // `gmhd`.
            push_atom(&mut minf, *b"nmhd", &0u32.to_be_bytes());
        }
        MuxTrackKind::Hint { hmhd, .. } => {
            // Hint Media Header Box (ISO/IEC 14496-12 §12.4.2).
            push_atom(&mut minf, *b"hmhd", &hmhd.to_body_bytes());
        }
    }
    push_atom(&mut minf, *b"dinf", &build_dinf(&t.data_references));
    push_atom(
        &mut minf,
        *b"stbl",
        &build_stbl(t, chunks, need_co64, aux_offset),
    );
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

/// Build a `gmhd` (Base Media Information Header) payload for a time-
/// code track (QTFF pp. 64–65, 116): a `gmin` Generic Media Information
/// header followed by a `tmcd` container wrapping the supplied `tcmi`
/// Time-Code Media Information atom. The parsed shape round-trips
/// through the demuxer's `parse_gmhd` onto `Track::gmhd`
/// (`gmin` + `tcmi`).
fn build_gmhd_tmcd(tcmi: &Tcmi, gmin: Option<&Gmin>) -> Vec<u8> {
    let mut gmhd = Vec::new();
    // `gmin`: caller-supplied override, else the conventional default
    // (copy graphics mode, no opcolor, centred balance) for a non-visual
    // generic track.
    let gmin_body = gmin.copied().unwrap_or_default().to_body_bytes();
    push_atom(&mut gmhd, *b"gmin", &gmin_body);
    // `tmcd` container with a single `tcmi` child.
    let mut tmcd_box = Vec::new();
    push_atom(&mut tmcd_box, *b"tcmi", &tcmi.to_body_bytes());
    push_atom(&mut gmhd, *b"tmcd", &tmcd_box);
    gmhd
}

/// Build a `gmhd` payload for a QuickTime **text** track (QTFF p. 65):
/// a `gmin` Generic Media Information header plus a `text` media-
/// information atom carrying a 9-element identity transformation matrix
/// (36 bytes, no FullBox prefix). Round-trips through the demuxer's
/// `parse_gmhd` onto `Track::gmhd` (`gmin` + `text`).
fn build_gmhd_text(gmin: Option<&Gmin>, matrix: Option<&[i32; 9]>) -> Vec<u8> {
    let mut gmhd = Vec::new();
    let gmin_body = gmin.copied().unwrap_or_default().to_body_bytes();
    push_atom(&mut gmhd, *b"gmin", &gmin_body);
    // 9-element transformation matrix (36 bytes, no FullBox prefix). The
    // caller may override; the default is the 3×3 identity in 16.16 /
    // 2.30 fixed-point (a=d=1.0, w=1.0), the same convention as
    // `tkhd`/`text` header matrices.
    let mut text = vec![0u8; 36];
    let default_matrix: [i32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
    let m = matrix.unwrap_or(&default_matrix);
    for (i, &v) in m.iter().enumerate() {
        text[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    push_atom(&mut gmhd, *b"text", &text);
    gmhd
}

fn build_dinf(refs: &[DataReferenceWrite]) -> Vec<u8> {
    // ISO/IEC 14496-12 §8.7.1 — `dinf` wraps a single `dref`.
    let mut dref = Vec::new();
    // dref body: ver+flags(4) + entry_count(4) + N × entries.
    dref.extend_from_slice(&0u32.to_be_bytes());
    if refs.is_empty() {
        // Default: one self-reference `url ` entry, flags=1 (data is in
        // this file). Each child is a FullBox: ver(1)+flags(3) body.
        dref.extend_from_slice(&1u32.to_be_bytes());
        push_atom(&mut dref, *b"url ", &0x0000_0001u32.to_be_bytes());
    } else {
        dref.extend_from_slice(&(refs.len() as u32).to_be_bytes());
        for r in refs {
            match r {
                DataReferenceWrite::SelfRef => {
                    // `url ` with flags=1, empty data slot (§8.7.2).
                    push_atom(&mut dref, *b"url ", &0x0000_0001u32.to_be_bytes());
                }
                DataReferenceWrite::Url(url) => {
                    // ver=0 flags=0 then NUL-terminated UTF-8 URL.
                    let mut body = Vec::with_capacity(4 + url.len() + 1);
                    body.extend_from_slice(&0u32.to_be_bytes());
                    body.extend_from_slice(url.as_bytes());
                    body.push(0);
                    push_atom(&mut dref, *b"url ", &body);
                }
                DataReferenceWrite::Urn { name, location } => {
                    // ver=0 flags=0; NUL-terminated `name`, then an
                    // optional NUL-terminated `location` (§8.7.2).
                    let mut body = Vec::with_capacity(4 + name.len() + location.len() + 2);
                    body.extend_from_slice(&0u32.to_be_bytes());
                    body.extend_from_slice(name.as_bytes());
                    body.push(0);
                    if !location.is_empty() {
                        body.extend_from_slice(location.as_bytes());
                        body.push(0);
                    }
                    push_atom(&mut dref, *b"urn ", &body);
                }
            }
        }
    }
    let mut dinf = Vec::new();
    push_atom(&mut dinf, *b"dref", &dref);
    dinf
}

/// Build a `sdtp` (Independent and Disposable Samples Box) payload —
/// ISO/IEC 14496-12 §8.6.4.2. FullBox(version=0, 0) followed by one
/// packed dependency byte per sample (no on-disk count word; the row
/// count is implied by the sample-size table, §8.6.4.1). Each entry's
/// four 2-bit fields pack MSB-first via [`SdtpEntry::to_byte`] — the
/// exact inverse of the read-side [`crate::sample_table::parse_sdtp`].
fn build_sdtp(entries: &[SdtpEntry]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + entries.len());
    p.extend_from_slice(&[0, 0, 0, 0]); // version 0 + flags 0
    p.extend(entries.iter().map(SdtpEntry::to_byte));
    p
}

/// Build a `stdp` (Degradation Priority Box) payload — ISO/IEC
/// 14496-12 §8.5.3.2. FullBox(version=0, 0) followed by one 16-bit
/// `priority` per sample (no on-disk count word; the row count is
/// implied by the sample-size table, §8.5.3.1). The inverse of the
/// read-side [`crate::sample_table::parse_stdp`].
fn build_stdp(priorities: &[u16]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + priorities.len() * 2);
    p.extend_from_slice(&[0, 0, 0, 0]); // version 0 + flags 0
    for &prio in priorities {
        p.extend_from_slice(&prio.to_be_bytes());
    }
    p
}

/// Build a `padb` (Padding Bits Box) payload — ISO/IEC 14496-12
/// §8.7.6.2. FullBox(version=0, 0), a 32-bit `sample_count`, then
/// `⌈sample_count / 2⌉` packed bytes, each `[reserved:1, pad1:3,
/// reserved:1, pad2:3]` MSB-first (`pad1` ⇒ sample `(i*2)+1`, `pad2` ⇒
/// sample `(i*2)+2`, both 1-based). The reserved bits are written 0; an
/// odd sample count leaves the final `pad2` slot zero. The exact
/// inverse of the read-side [`crate::sample_table::parse_padb`]. Caller
/// guarantees every value is `0..=7`.
fn build_padb(pads: &[u8]) -> Vec<u8> {
    let n = pads.len();
    let packed = n.div_ceil(2);
    let mut p = Vec::with_capacity(8 + packed);
    p.extend_from_slice(&[0, 0, 0, 0]); // version 0 + flags 0
    p.extend_from_slice(&(n as u32).to_be_bytes());
    for chunk in pads.chunks(2) {
        // High nibble = pad1 (this chunk's first sample); low nibble =
        // pad2 (second sample, or 0 when this is the trailing odd byte).
        let pad1 = chunk[0] & 0x07;
        let pad2 = chunk.get(1).copied().unwrap_or(0) & 0x07;
        p.push((pad1 << 4) | pad2);
    }
    p
}

/// Build a `stsh` (Shadow Sync Sample Box) payload — ISO/IEC 14496-12
/// §8.6.3.2. FullBox(version=0, 0), a 32-bit `entry_count`, then
/// `entry_count × {shadowed_sample_number:4, sync_sample_number:4}`.
/// Caller guarantees the entries are sorted ascending by
/// `shadowed_sample_number` with no duplicates (§8.6.3.1). The inverse
/// of the read-side [`crate::sample_table::parse_stsh`].
fn build_stsh(entries: &[StshEntry]) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + entries.len() * 8);
    p.extend_from_slice(&[0, 0, 0, 0]); // version 0 + flags 0
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for e in entries {
        p.extend_from_slice(&e.shadowed_sample_number.to_be_bytes());
        p.extend_from_slice(&e.sync_sample_number.to_be_bytes());
    }
    p
}

/// Build a `subs` (Sub-Sample Information Box) payload — ISO/IEC
/// 14496-12 §8.7.7.2. FullBox(version, 0), a 32-bit `entry_count`, then
/// per row `[sample_delta:4][subsample_count:2]` followed by
/// `subsample_count` records of `[subsample_size:(2|4)]
/// [subsample_priority:1][discardable:1][codec_specific_parameters:4]`.
///
/// `version` is 1 (32-bit `subsample_size`) when any sub-sample exceeds
/// 65535 bytes, else 0 (16-bit) — the narrowest representable form. The
/// caller-sorted absolute `sample_number`s are converted back to the
/// sparse `sample_delta` coding (first row = difference from zero, each
/// later row = difference from the previous). The inverse of the
/// read-side [`crate::sample_table::parse_subs`].
fn build_subs(rows: &[SubSampleInfo]) -> Vec<u8> {
    // Pick the narrowest size width that fits every sub-sample.
    let need_v1 = rows
        .iter()
        .flat_map(|r| r.subsamples.iter())
        .any(|s| s.subsample_size > u32::from(u16::MAX));
    let version: u8 = if need_v1 { 1 } else { 0 };

    let mut p = Vec::new();
    p.push(version);
    p.extend_from_slice(&[0, 0, 0]); // flags 0
    p.extend_from_slice(&(rows.len() as u32).to_be_bytes());

    let mut prev_sample: u32 = 0;
    for r in rows {
        let delta = r.sample_number - prev_sample;
        prev_sample = r.sample_number;
        p.extend_from_slice(&delta.to_be_bytes());
        p.extend_from_slice(&(r.subsamples.len() as u16).to_be_bytes());
        for s in &r.subsamples {
            if version == 1 {
                p.extend_from_slice(&s.subsample_size.to_be_bytes());
            } else {
                p.extend_from_slice(&(s.subsample_size as u16).to_be_bytes());
            }
            p.push(s.subsample_priority);
            p.push(s.discardable);
            p.extend_from_slice(&s.codec_specific_parameters.to_be_bytes());
        }
    }
    p
}

fn build_stbl(
    t: &TrackWrite,
    chunks: &[TrackChunk],
    need_co64: bool,
    aux_offset: Option<u64>,
) -> Vec<u8> {
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &build_stsd(t));
    push_atom(&mut stbl, *b"stts", &build_stts(t));
    // Composition Time to Sample Box (ISO/IEC 14496-12 §8.6.1.3) —
    // emitted only when the track carries a non-zero composition offset
    // on at least one sample (PTS ≠ DTS, i.e. B-frame reordering). §8.6
    // orders it immediately after `stts` within `stbl`.
    if let Some(ctts_atom) = build_ctts(t) {
        push_atom(&mut stbl, *b"ctts", &ctts_atom);
    }
    // Composition to Decode Box (§8.6.1.4) — summarises the
    // composition-vs-decode timeline. §6.2.3 orders it right after
    // `ctts`. Emitted only when the caller opted in via auto_cslg /
    // set_cslg.
    if let Some(cslg_atom) = build_cslg(t) {
        push_atom(&mut stbl, *b"cslg", &cslg_atom);
    }
    if let Some(stss_atom) = build_stss(t) {
        push_atom(&mut stbl, *b"stss", &stss_atom);
    }
    push_atom(&mut stbl, *b"stsc", &build_stsc(chunks));
    push_sample_size_atom(&mut stbl, t);
    if need_co64 {
        push_atom(&mut stbl, *b"co64", &build_co64(chunks));
    } else {
        push_atom(&mut stbl, *b"stco", &build_stco(chunks));
    }
    // Optional per-sample dependency / priority / padding / shadow-sync /
    // sub-sample tables. Each is emitted only when the caller attached
    // it; all follow the chunk-offset table inside `stbl` (box order
    // inside a `stbl` is not significant to a conformant reader, but a
    // stable order is chosen for deterministic output).
    if !t.sdtp.is_empty() {
        push_atom(&mut stbl, *b"sdtp", &build_sdtp(&t.sdtp));
    }
    if !t.stdp.is_empty() {
        push_atom(&mut stbl, *b"stdp", &build_stdp(&t.stdp));
    }
    if !t.padb.is_empty() {
        push_atom(&mut stbl, *b"padb", &build_padb(&t.padb));
    }
    if !t.stsh.is_empty() {
        push_atom(&mut stbl, *b"stsh", &build_stsh(&t.stsh));
    }
    if !t.subs.is_empty() {
        push_atom(&mut stbl, *b"subs", &build_subs(&t.subs));
    }
    // Sample-auxiliary-information pair (ISO/IEC 14496-12 §8.7.8 /
    // §8.7.9). Emitted only when the track carries an aux stream; the
    // slab was laid into mdat starting at `aux_offset`.
    if let (Some(aux), Some(off)) = (&t.sample_aux, aux_offset) {
        push_atom(&mut stbl, *b"saiz", &build_saiz(aux));
        push_atom(&mut stbl, *b"saio", &build_saio(aux, off));
    }
    // Sample Group Description Box(es) (ISO/IEC 14496-12 §8.9.3) — one
    // `sgpd` per attached grouping_type, written before the
    // `sbgp`/`csgp` boxes that reference them (§8.9.3 containment order).
    for d in &t.sample_group_descriptions {
        push_atom(&mut stbl, *b"sgpd", &build_sgpd(d));
    }
    // Sample to Group Box(es) (ISO/IEC 14496-12 §8.9.2 / §8.9.5) — one
    // per attached grouping_type, after the sgpd boxes. The compact
    // `csgp` or the classic run-length `sbgp` per the caller's chosen
    // form.
    for (g, form) in &t.sample_to_groups {
        match form {
            SampleGroupBoxForm::Compact => push_atom(&mut stbl, *b"csgp", &build_csgp(g)),
            SampleGroupBoxForm::Classic => push_atom(&mut stbl, *b"sbgp", &build_sbgp(g)),
        }
    }
    stbl
}

/// Build a `sgpd` (SampleGroupDescriptionBox) payload — ISO/IEC
/// 14496-12 §8.9.3.2. Written **version 1**: when every entry shares
/// one length the box uses a constant `default_length` (no per-entry
/// prefix); otherwise `default_length == 0` with a `description_length`
/// `u32` before each entry. Version 1 is preferred over the deprecated
/// version 0 because the entry sizes are explicit on disk (§8.9.3.2
/// NOTE — version-0 entries carry no signalled size). Round-trips
/// through [`parse_sgpd`].
///
/// [`parse_sgpd`]: crate::sample_groups::parse_sgpd
fn build_sgpd(d: &SampleGroupDescriptionWrite) -> Vec<u8> {
    // A constant default_length is usable only when every entry is the
    // same length and that length is non-zero (default_length == 0 is
    // the sentinel for "variable, prefixed"). An empty entry list is
    // rejected upstream at set_sample_group_description.
    let first_len = d.entries.first().map(Vec::len).unwrap_or(0);
    let uniform = first_len != 0 && d.entries.iter().all(|e| e.len() == first_len);
    let default_length = if uniform { first_len as u32 } else { 0 };

    let mut p = Vec::new();
    p.push(1); // version 1
    p.extend_from_slice(&[0, 0, 0]); // flags
    p.extend_from_slice(&d.grouping_type);
    p.extend_from_slice(&default_length.to_be_bytes());
    p.extend_from_slice(&(d.entries.len() as u32).to_be_bytes()); // entry_count
    for entry in &d.entries {
        if default_length == 0 {
            // Variable-length: prefix each entry with its size.
            p.extend_from_slice(&(entry.len() as u32).to_be_bytes());
        }
        p.extend_from_slice(entry);
    }
    p
}

/// Minimal 2-bit size code (per `docs/container/isobmff/
/// post-2015-additions.md`, `f(code) = 4 << code` → {0→4,1→8,2→16,
/// 3→32}) that can represent every value in `vals`. Returns the code
/// `0..=3` and its bit width. An empty input or all-zero values still
/// needs at least the 4-bit minimum (code 0).
fn csgp_min_size_code(max_val: u32) -> (u32, u32) {
    for code in 0u32..=2 {
        let width = 4u32 << code; // 4, 8, 16
                                  // The widest representable value at `width` bits.
        let cap: u64 = (1u64 << width) - 1;
        if (max_val as u64) <= cap {
            return (code, width);
        }
    }
    (3, 32)
}

/// Append `value` to `bits` as a big-endian (MSB-first) field of
/// `width` bits, packed contiguously after the bits already present.
/// `bit_len` tracks the current bit cursor; the byte buffer grows as
/// needed and trailing bits in the final partial byte are left zero.
fn csgp_push_bits(bytes: &mut Vec<u8>, bit_len: &mut usize, value: u32, width: u32) {
    for i in (0..width).rev() {
        let bit = ((value >> i) & 1) as u8;
        let byte_idx = *bit_len >> 3;
        if byte_idx == bytes.len() {
            bytes.push(0);
        }
        if bit != 0 {
            bytes[byte_idx] |= 1 << (7 - (*bit_len & 7));
        }
        *bit_len += 1;
    }
}

/// Build a `csgp` (CompactSampleToGroupBox) payload from a per-sample
/// index assignment. ISO/IEC 14496-12:2020 §8.9.5, layout per
/// `docs/container/isobmff/post-2015-additions.md`.
///
/// Encoding strategy: run-length the per-sample indices, then emit one
/// pattern of `pattern_length == 1` per run — pattern `[idx]` replayed
/// `sample_count` times reproduces a run of `sample_count` equal
/// indices. This is the compact analogue of the run-length `sbgp`
/// rows and round-trips losslessly through [`parse_csgp`]: that reader
/// expands each pattern sample-by-sample and RLE-coalesces, recovering
/// the original runs. The three width selectors are chosen as the
/// minimum that fits (`pattern_length` is always 1 → 4-bit code 0;
/// `sample_count` sized to the longest run; index sized to the largest
/// description index).
///
/// [`parse_csgp`]: crate::sample_groups::parse_csgp
fn build_csgp(g: &SampleToGroupWrite) -> Vec<u8> {
    // Run-length the per-sample indices.
    let mut runs: Vec<(u32, u32)> = Vec::new(); // (sample_count, index)
    for &idx in &g.indices {
        match runs.last_mut() {
            Some(last) if last.1 == idx => last.0 += 1,
            _ => runs.push((1, idx)),
        }
    }

    let max_count = runs.iter().map(|r| r.0).max().unwrap_or(0);
    let max_index = runs.iter().map(|r| r.1).max().unwrap_or(0);
    // pattern_length is always 1 here → code 0 (4 bits) always suffices.
    let pattern_size_code = 0u32;
    let pattern_width = 4u32;
    let (count_size_code, count_width) = csgp_min_size_code(max_count);
    let (index_size_code, index_width) = csgp_min_size_code(max_index);
    let gtp_present = g.grouping_type_parameter.is_some();

    // flags (24-bit, LSB numbering): index[0..1] | count[2..3] |
    // pattern[4..5] | gtp_present[6].
    let flags: u32 = index_size_code
        | (count_size_code << 2)
        | (pattern_size_code << 4)
        | (u32::from(gtp_present) << 6);

    let mut p = Vec::new();
    p.push(0); // version 0
    p.extend_from_slice(&flags.to_be_bytes()[1..]); // 24-bit flags
    p.extend_from_slice(&g.grouping_type);
    if let Some(gtp) = g.grouping_type_parameter {
        p.extend_from_slice(&gtp.to_be_bytes());
    }
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes()); // pattern_count

    // The pattern table and the index table form one contiguous
    // MSB-first bitstream beginning right after `pattern_count`.
    let mut bits: Vec<u8> = Vec::new();
    let mut bit_len = 0usize;
    // Pattern table: pattern_length[i] (always 1), sample_count[i].
    for &(count, _idx) in &runs {
        csgp_push_bits(&mut bits, &mut bit_len, 1, pattern_width);
        csgp_push_bits(&mut bits, &mut bit_len, count, count_width);
    }
    // Index table: one index per pattern (pattern_length == 1).
    for &(_count, idx) in &runs {
        csgp_push_bits(&mut bits, &mut bit_len, idx, index_width);
    }
    p.extend_from_slice(&bits);
    p
}

/// Build a `sbgp` (SampleToGroupBox) payload from a per-sample index
/// assignment — ISO/IEC 14496-12 §8.9.2.2. The classic run-length form:
/// consecutive samples sharing one `group_description_index` collapse
/// into a single `[sample_count][group_description_index]` row. Version
/// 1 (carrying `grouping_type_parameter`) is used when the assignment
/// supplies one; version 0 otherwise. Round-trips through
/// [`parse_sbgp`].
///
/// [`parse_sbgp`]: crate::sample_groups::parse_sbgp
fn build_sbgp(g: &SampleToGroupWrite) -> Vec<u8> {
    // Run-length the per-sample indices into (sample_count, index) rows.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &idx in &g.indices {
        match runs.last_mut() {
            Some(last) if last.1 == idx => last.0 += 1,
            _ => runs.push((1, idx)),
        }
    }
    let version: u8 = if g.grouping_type_parameter.is_some() {
        1
    } else {
        0
    };
    let mut p = Vec::with_capacity(8 + if version == 1 { 4 } else { 0 } + runs.len() * 8);
    p.push(version);
    p.extend_from_slice(&[0, 0, 0]); // flags
    p.extend_from_slice(&g.grouping_type);
    if let Some(gtp) = g.grouping_type_parameter {
        p.extend_from_slice(&gtp.to_be_bytes());
    }
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes()); // entry_count
    for (count, idx) in runs {
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&idx.to_be_bytes());
    }
    p
}

/// Build a `saiz` (Sample Auxiliary Information Sizes Box) payload —
/// ISO/IEC 14496-12 §8.7.8.2. Uses the uniform
/// `default_sample_info_size` form when every blob is the same non-zero
/// length; otherwise emits the per-sample size table. Per-sample blob
/// lengths are validated to fit in a `u8` at [`MovMuxer::set_sample_aux`]
/// time (the spec's size table is `u8`-wide), so the cast here is
/// lossless.
fn build_saiz(aux: &SampleAuxStream) -> Vec<u8> {
    build_saiz_blobs(
        aux.aux_info_type,
        aux.aux_info_type_parameter,
        &aux.per_sample,
    )
}

/// Core `saiz` builder shared by the `stbl`-scope ([`build_saiz`]) and
/// `traf`-scope ([`build_traf`]) write paths. `blobs` is the per-sample
/// auxiliary-information list for the box's scope (the whole track for
/// `stbl`, a single fragment's samples for `traf`). The
/// `(aux_info_type, aux_info_type_parameter)` discriminator is emitted
/// only when `aux_info_type` is `Some` (gated by the `flags & 1` bit).
/// Per-sample blob lengths are validated to fit in a `u8` at
/// [`MovMuxer::set_sample_aux`] time, so the cast here is lossless.
fn build_saiz_blobs(
    aux_info_type: Option<[u8; 4]>,
    aux_info_type_parameter: u32,
    blobs: &[Vec<u8>],
) -> Vec<u8> {
    let flags: u32 = if aux_info_type.is_some() { 1 } else { 0 };
    let sizes: Vec<u8> = blobs
        .iter()
        .map(|b| b.len().min(u8::MAX as usize) as u8)
        .collect();
    let uniform = sizes.first().copied().unwrap_or(0);
    let all_same = !sizes.is_empty() && sizes.iter().all(|&s| s == uniform);
    // Use the default-size form only when uniform AND non-zero (a
    // zero default with a non-empty count is the "per-sample table
    // follows" sentinel; an all-empty-blob stream is encoded as the
    // per-sample table of zeros so the reader sees explicit sizes).
    let use_default = all_same && uniform != 0;

    let mut p = Vec::new();
    p.push(0); // version (spec fixes at 0)
    p.extend_from_slice(&flags.to_be_bytes()[1..4]); // 3-byte flags
    if let Some(t) = aux_info_type {
        p.extend_from_slice(&t);
        p.extend_from_slice(&aux_info_type_parameter.to_be_bytes());
    }
    if use_default {
        p.push(uniform); // default_sample_info_size
        p.extend_from_slice(&(blobs.len() as u32).to_be_bytes());
    } else {
        p.push(0); // default_sample_info_size = 0 ⇒ table follows
        p.extend_from_slice(&(blobs.len() as u32).to_be_bytes());
        p.extend_from_slice(&sizes);
    }
    p
}

/// Build a `saio` (Sample Auxiliary Information Offsets Box) payload —
/// ISO/IEC 14496-12 §8.7.9.2. The muxer always emits a single-entry
/// table (§8.7.9.3 — one offset for a contiguous slab) carrying the
/// absolute file offset of the first aux blob. Selects v1 (64-bit
/// offsets) automatically when the offset exceeds the 32-bit range, v0
/// otherwise. Carries the same `(aux_info_type, aux_info_type_parameter)`
/// discriminator (gated by `flags & 1`) as the matching `saiz` so the
/// pair resolves together on read (§8.7.9.3 — the pair semantics are
/// defined as in the SampleAuxiliaryInformationSizesBox, which §8.7.8.3
/// requires to match for a `(saiz, saio)` pair).
fn build_saio(aux: &SampleAuxStream, aux_offset: u64) -> Vec<u8> {
    build_saio_offset(aux.aux_info_type, aux.aux_info_type_parameter, aux_offset)
}

/// Core single-entry `saio` builder shared by the `stbl`-scope
/// ([`build_saio`]) and `traf`-scope ([`build_traf`]) write paths.
/// `aux_offset` is the offset the box should carry — *absolute* in the
/// `stbl` scope (§8.7.9.3) and *relative to the track-fragment base
/// offset* in the `traf` scope (§8.8.14; with `default-base-is-moof`
/// set, that base is the enclosing `moof`'s first byte). Selects v1
/// (64-bit) automatically when the offset exceeds the 32-bit range.
fn build_saio_offset(
    aux_info_type: Option<[u8; 4]>,
    aux_info_type_parameter: u32,
    aux_offset: u64,
) -> Vec<u8> {
    let need_v1 = aux_offset > u32::MAX as u64;
    let flags: u32 = if aux_info_type.is_some() { 1 } else { 0 };
    let mut p = Vec::new();
    p.push(if need_v1 { 1 } else { 0 }); // version
    p.extend_from_slice(&flags.to_be_bytes()[1..4]); // 3-byte flags
    if let Some(t) = aux_info_type {
        p.extend_from_slice(&t);
        p.extend_from_slice(&aux_info_type_parameter.to_be_bytes());
    }
    p.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
    if need_v1 {
        p.extend_from_slice(&aux_offset.to_be_bytes());
    } else {
        p.extend_from_slice(&(aux_offset as u32).to_be_bytes());
    }
    p
}

fn build_stsd(t: &TrackWrite) -> Vec<u8> {
    // ISO/IEC 14496-12 §8.5.2 / QTFF p. 70.
    // [ver+flags:4][entry_count:4]([size:4][format:4][rsrv:6]
    //     [data_reference_index:2][per-mediatype body][optional extra atoms])+
    //
    // The sample entry's `data_reference_index` points (1-based) at the
    // `dref` entry the sample chunk offsets are relative to. For the
    // default in-file layout that is the self-reference entry: 1 for
    // the implicit single-entry `dref`, else the 1-based position of
    // the lone `SelfRef` in a custom table set via
    // `set_data_references`. A track marked external
    // (`set_external_media`) instead points at its designated non-self
    // entry — the `url `/`urn ` naming the external storage.
    let dri = match &t.external_media {
        Some(ext) => ext.data_reference_index,
        None => self_ref_index(&t.data_references),
    };
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
            // Field offsets per the QTFF p. 92 order (width/height
            // directly follow the two quality fields; see also the
            // Chapter 5 worked example bytes).
            // width @ 16..18, height @ 18..20
            body[16..18].copy_from_slice(&width.to_be_bytes());
            body[18..20].copy_from_slice(&height.to_be_bytes());
            // hres @ 20..24 = 72.0 dpi (16.16 = 0x00480000)
            body[20..24].copy_from_slice(&0x0048_0000u32.to_be_bytes());
            // vres @ 24..28 = 72.0 dpi
            body[24..28].copy_from_slice(&0x0048_0000u32.to_be_bytes());
            // data_size @ 28..32 "must be set to 0" — left zero.
            // frame_count @ 32..34 = 1
            body[32..34].copy_from_slice(&1u16.to_be_bytes());
            // compressor_name @ 34..66: 32-byte Pascal string, left
            // zero (empty name).
            // depth @ 66..68 = 24 (typical for non-alpha video)
            body[66..68].copy_from_slice(&24u16.to_be_bytes());
            // color_table_id @ 68..70 = -1 (no color table)
            body[68..70].copy_from_slice(&(-1i16).to_be_bytes());
            e.extend_from_slice(&body);
            // Codec-config blobs (`avcC` / `hvcC` / …) come first by
            // convention, then the typed visual extension boxes.
            e.extend_from_slice(&t.extra_stsd_atoms);
            e.extend_from_slice(&t.visual_extensions.to_framed_atoms());
            wrap_stsd_entry(format, &e, dri)
        }
        MuxTrackKind::Audio {
            format,
            channels,
            bits_per_sample,
            sample_rate,
        } => {
            // Fixed body: 20 bytes for version 0 and for the ISO
            // AudioSampleEntryV1 (ISO/IEC 14496-12:2015 §12.2.3.2 —
            // entry_version 1 keeps the version-0 shape); 36 bytes for
            // the QTFF SoundDescriptionV1 (p. 101 — four fixed-ratio
            // longs appended); 56 bytes for the QTFF 2012-08-14
            // SoundDescriptionV2 (pp. 181–182).
            let body_len = match &t.audio_description {
                AudioDescriptionWrite::QtffV1 { .. } => 36,
                AudioDescriptionWrite::QtffV2(_) => 56,
                _ => 20,
            };
            let mut e = Vec::with_capacity(16 + body_len + t.extra_stsd_atoms.len());
            let mut body = vec![0u8; body_len];
            match &t.audio_description {
                AudioDescriptionWrite::V0 => {
                    // version=0, revision=0, vendor=0 left zero.
                }
                AudioDescriptionWrite::QtffV1 { fields, vbr } => {
                    // version @ 0..2 = 1 (QTFF p. 101).
                    body[0..2].copy_from_slice(&1u16.to_be_bytes());
                    // Compression ID @ 12..14: -2 flags the VBR third
                    // variant (QTFF p. 102), else 0.
                    if *vbr {
                        body[12..14].copy_from_slice(&(-2i16).to_be_bytes());
                    }
                    body[20..24].copy_from_slice(&fields.samples_per_packet.to_be_bytes());
                    body[24..28].copy_from_slice(&fields.bytes_per_packet.to_be_bytes());
                    body[28..32].copy_from_slice(&fields.bytes_per_frame.to_be_bytes());
                    body[32..36].copy_from_slice(&fields.bytes_per_sample.to_be_bytes());
                }
                AudioDescriptionWrite::QtffV2(v2) => {
                    // QTFF 2012-08-14 pp. 181–182: version 2, the
                    // fixed always* back-compatibility constants in
                    // the version-0 positions (an old reader sees
                    // ch=3 / bits=16 / compression=-2 / packet=0 /
                    // 16.16 rate 1.0), then the v2 fields. The
                    // common channels/bits/rate writes below are
                    // skipped for this layout.
                    body[0..2].copy_from_slice(&2u16.to_be_bytes());
                    // always3 / always16 / alwaysMinus2, then
                    // always0 @ 14..16 left zero and always65536.
                    body[8..10].copy_from_slice(&3u16.to_be_bytes());
                    body[10..12].copy_from_slice(&16u16.to_be_bytes());
                    body[12..14].copy_from_slice(&(-2i16).to_be_bytes());
                    body[16..20].copy_from_slice(&65536u32.to_be_bytes());
                    // sizeOfStructOnly: 16-byte universal header +
                    // 56-byte fixed struct — extensions follow
                    // immediately.
                    body[20..24].copy_from_slice(&72u32.to_be_bytes());
                    body[24..32].copy_from_slice(&v2.audio_sample_rate.to_be_bytes());
                    body[32..36].copy_from_slice(&(*channels as u32).to_be_bytes());
                    body[36..40].copy_from_slice(&0x7F00_0000u32.to_be_bytes());
                    body[40..44].copy_from_slice(&v2.const_bits_per_channel.to_be_bytes());
                    body[44..48].copy_from_slice(&v2.format_specific_flags.0.to_be_bytes());
                    body[48..52].copy_from_slice(&v2.const_bytes_per_audio_packet.to_be_bytes());
                    body[52..56]
                        .copy_from_slice(&v2.const_lpcm_frames_per_audio_packet.to_be_bytes());
                }
                AudioDescriptionWrite::IsoV1(_) => {
                    // entry_version @ 0..2 = 1 (§12.2.3.2); the next
                    // six bytes are reserved zero.
                    body[0..2].copy_from_slice(&1u16.to_be_bytes());
                }
            }
            if !matches!(&t.audio_description, AudioDescriptionWrite::QtffV2(_)) {
                body[8..10].copy_from_slice(&channels.to_be_bytes());
                body[10..12].copy_from_slice(&bits_per_sample.to_be_bytes());
                // packet_size @ 14..16 = 0.
                // sample_rate @ 16..20 — 16.16 fixed; QTFF caps the
                // integer portion at u16, so cap the rate to 65535 Hz
                // when needed.
                let sr = (*sample_rate).min(0xFFFF);
                body[16..20].copy_from_slice(&(sr << 16).to_be_bytes());
            }
            e.extend_from_slice(&body);
            e.extend_from_slice(&t.extra_stsd_atoms);
            if let AudioDescriptionWrite::IsoV1(v1) = &t.audio_description {
                // §12.2.3.2 optional trailing boxes, after the codec
                // config so a decoder-specific box stays first.
                if let Some(rate) = v1.sampling_rate {
                    let mut srat = vec![0u8; 4]; // FullBox v0+flags
                    srat.extend_from_slice(&rate.to_be_bytes());
                    push_atom(&mut e, *b"srat", &srat);
                }
                if let Some(chnl) = &v1.channel_layout {
                    push_atom(&mut e, *b"chnl", &chnl.to_body_bytes());
                }
            }
            wrap_stsd_entry(format, &e, dri)
        }
        MuxTrackKind::Timecode { description, .. } => {
            // `tmcd` sample-description body after the universal 16-byte
            // header (QTFF p. 106); the source-tape `name` atom (if any)
            // is appended by `to_sample_description_body`.
            let mut e = description.to_sample_description_body();
            e.extend_from_slice(&t.extra_stsd_atoms);
            wrap_stsd_entry(b"tmcd", &e, dri)
        }
        MuxTrackKind::Text { description } => {
            // QuickTime `text` sample-description body after the
            // universal 16-byte header (QTFF pp. 108–110).
            let mut e = description.to_body_bytes();
            e.extend_from_slice(&t.extra_stsd_atoms);
            wrap_stsd_entry(b"text", &e, dri)
        }
        MuxTrackKind::Metadata { description } => {
            // ISO BMFF timed-metadata sample entry (metx / mett / urim,
            // ISO/IEC 14496-12 §12.3.3) after the universal 16-byte
            // header; the FourCC is taken from the variant.
            let mut e = description.to_body_bytes();
            e.extend_from_slice(&t.extra_stsd_atoms);
            wrap_stsd_entry(&description.format(), &e, dri)
        }
        MuxTrackKind::Subtitle { description } => {
            // ISO BMFF subtitle sample entry (stpp / sbtt, ISO/IEC
            // 14496-12 §12.6.3) after the universal 16-byte header; the
            // FourCC is taken from the variant.
            let mut e = description.to_body_bytes();
            e.extend_from_slice(&t.extra_stsd_atoms);
            wrap_stsd_entry(&description.format(), &e, dri)
        }
        MuxTrackKind::SimpleText { description } => {
            // ISO BMFF SimpleTextSampleEntry (stxt, ISO/IEC 14496-12
            // §12.5.3) after the universal 16-byte header.
            let mut e = description.to_body_bytes();
            e.extend_from_slice(&t.extra_stsd_atoms);
            wrap_stsd_entry(b"stxt", &e, dri)
        }
        MuxTrackKind::Hint {
            protocol,
            description,
            ..
        } => {
            // ISO BMFF HintSampleEntry (ISO/IEC 14496-12 §12.4.3): the
            // entry FourCC is the protocol identifier, the body is opaque
            // protocol-specific declarative data after the universal
            // 16-byte header.
            let mut e = description.clone();
            e.extend_from_slice(&t.extra_stsd_atoms);
            wrap_stsd_entry(protocol, &e, dri)
        }
    };
    let mut stsd = Vec::with_capacity(8 + entry_body.len());
    // §8.5.2: the stsd FullBox takes version 1 when it contains an
    // ISO AudioSampleEntryV1 ("version is set to zero unless the box
    // contains an AudioSampleEntryV1, whereupon version must be 1").
    let stsd_version: u32 = match &t.audio_description {
        AudioDescriptionWrite::IsoV1(_) => 1 << 24,
        _ => 0,
    };
    stsd.extend_from_slice(&stsd_version.to_be_bytes()); // ver+flags
    stsd.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    stsd.extend_from_slice(&entry_body);
    stsd
}

/// Validate that 1-based `index` names an entry of `refs` that is not
/// the self-reference — the shape [`MovMuxer::set_external_media`]
/// requires (an external track's sample entries must point at a
/// `url ` / `urn ` naming the external storage).
fn check_external_dref_entry(refs: &[DataReferenceWrite], index: u16) -> Result<()> {
    let entry = index
        .checked_sub(1)
        .and_then(|i| refs.get(i as usize))
        .ok_or_else(|| {
            Error::invalid(format!(
                "MOV muxer: external data_reference_index {index} is out of range for a \
                 {}-entry data-reference table (indices are 1-based)",
                refs.len()
            ))
        })?;
    if matches!(entry, DataReferenceWrite::SelfRef) {
        return Err(Error::invalid(format!(
            "MOV muxer: external data_reference_index {index} names the SelfRef entry; \
             an external track must point at a url /urn  entry naming the external storage"
        )));
    }
    Ok(())
}

/// Coalesce per-sample external locations into [`TrackChunk`]s: a run
/// of byte-contiguous locations (each starting exactly where the
/// previous one ended) forms one chunk, matching the demuxer's
/// within-chunk offset recovery (chunk offset + sum of preceding
/// sample sizes). Any gap or backward jump opens a new chunk.
fn coalesce_external_chunks(locations: &[ExternalSampleLocation]) -> Vec<TrackChunk> {
    let mut out: Vec<TrackChunk> = Vec::new();
    let mut expected_next: u64 = 0;
    for l in locations {
        match out.last_mut() {
            Some(last) if l.offset == expected_next => last.sample_count += 1,
            _ => out.push(TrackChunk {
                offset: l.offset,
                sample_count: 1,
            }),
        }
        expected_next = l.offset.saturating_add(l.size as u64);
    }
    out
}

/// The 1-based `data_reference_index` a sample entry should carry —
/// the position of the lone self-reference in the track's `dref` table.
/// Defaults to 1 for the implicit single-entry table (empty list).
fn self_ref_index(refs: &[DataReferenceWrite]) -> u16 {
    if refs.is_empty() {
        return 1;
    }
    refs.iter()
        .position(|r| matches!(r, DataReferenceWrite::SelfRef))
        .map(|i| (i + 1) as u16)
        .unwrap_or(1)
}

/// Wrap a per-mediatype body in the universal 16-byte stsd entry
/// header: `[size:4][format:4][reserved:6][data_reference_index:2]`.
fn wrap_stsd_entry(format: &[u8; 4], body: &[u8], data_reference_index: u16) -> Vec<u8> {
    let entry_size: u32 = (16 + body.len()) as u32;
    let mut out = Vec::with_capacity(entry_size as usize);
    out.extend_from_slice(&entry_size.to_be_bytes());
    out.extend_from_slice(format);
    out.extend_from_slice(&[0u8; 6]); // 6 bytes reserved
    out.extend_from_slice(&data_reference_index.to_be_bytes());
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

/// Build the `ctts` Composition Time to Sample Box payload
/// (ISO/IEC 14496-12 §8.6.1.3.2).
///
/// Returns `None` when every sample's `composition_offset` is zero — the
/// box is then omitted and a reader applies the implicit `PTS == DTS`
/// rule (§8.6.1.3.1: "the composition times … are identical to the
/// decoding times … if this box is not present"). When at least one
/// offset is non-zero the box covers the whole track, run-length-encoding
/// consecutive samples that share an offset into `(sample_count,
/// sample_offset)` rows exactly as `stts` does for durations.
///
/// Version selection follows §8.6.1.3.1: version 0 stores
/// `sample_offset` as `unsigned int(32)` and therefore cannot represent
/// a negative offset, so the emitter promotes to version 1 (signed
/// `int(32)`) the moment any offset is negative; an all-non-negative
/// track stays on version 0 for maximum reader compatibility. Both
/// versions round-trip through [`crate::sample_table::parse_ctts`], which
/// normalises either form to the signed `i32` carried back on
/// [`crate::sample_table::SampleInfo::composition_offset`].
fn build_ctts(t: &TrackWrite) -> Option<Vec<u8>> {
    if t.samples.iter().all(|s| s.composition_offset == 0) {
        return None;
    }
    let need_v1 = t.samples.iter().any(|s| s.composition_offset < 0);
    // Run-length-encode consecutive samples sharing one offset.
    let mut runs: Vec<(u32, i32)> = Vec::new();
    for s in &t.samples {
        match runs.last_mut() {
            Some(last) if last.1 == s.composition_offset => last.0 += 1,
            _ => runs.push((1, s.composition_offset)),
        }
    }
    let version: u8 = if need_v1 { 1 } else { 0 };
    let mut p = Vec::with_capacity(8 + runs.len() * 8);
    p.push(version);
    p.extend_from_slice(&[0, 0, 0]); // flags (always 0)
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, offset) in runs {
        p.extend_from_slice(&count.to_be_bytes());
        // v0 stores the offset as unsigned int(32); for an
        // all-non-negative track the two's-complement big-endian bytes
        // of a non-negative `i32` equal its unsigned encoding, so the
        // same write is correct for both versions.
        p.extend_from_slice(&offset.to_be_bytes());
    }
    Some(p)
}

/// Derive a [`Cslg`] from a track's per-sample composition offsets and
/// durations (ISO/IEC 14496-12 §8.6.1.4.3). DTS is the running sum of
/// the preceding sample durations; CT_i = DTS_i + offset_i. Returns the
/// all-zero `Cslg` for an empty track.
fn derive_cslg(t: &TrackWrite) -> Cslg {
    if t.samples.is_empty() {
        return Cslg::default();
    }
    let mut dts: i64 = 0;
    let mut least = i64::MAX;
    let mut greatest = i64::MIN;
    let mut start = i64::MAX;
    let mut end = i64::MIN;
    for s in &t.samples {
        let offset = s.composition_offset as i64;
        let ct = dts + offset;
        least = least.min(offset);
        greatest = greatest.max(offset);
        start = start.min(ct);
        end = end.max(ct + s.duration as i64);
        dts += s.duration as i64;
    }
    // compositionToDTSShift keeps every shifted CTS at or above its DTS:
    // CTS_i + shift >= DTS_i  ⇔  shift >= -offset_i for all i  ⇔
    // shift >= -least. A non-negative shift is only needed when some
    // offset is negative; otherwise 0 (§8.6.1.4.3 — the value is 0 when
    // no reordering pulls a CTS below its DTS).
    let composition_to_dts_shift = (-least).max(0);
    Cslg {
        composition_to_dts_shift,
        least_decode_to_display_delta: least,
        greatest_decode_to_display_delta: greatest,
        composition_start_time: start,
        composition_end_time: end,
    }
}

/// Build a `cslg` (Composition to Decode Box) payload — ISO/IEC
/// 14496-12 §8.6.1.4.2. Five signed fields, version 0 (`int(32)`) when
/// every value fits the signed-32-bit range, else version 1
/// (`int(64)`). `None` when the track opted out (`t.cslg == None`).
/// Round-trips through [`parse_cslg`].
///
/// [`parse_cslg`]: crate::media_meta::parse_cslg
fn build_cslg(t: &TrackWrite) -> Option<Vec<u8>> {
    let cslg = match t.cslg? {
        CslgWrite::Auto => derive_cslg(t),
        CslgWrite::Explicit(c) => c,
    };
    let fields = [
        cslg.composition_to_dts_shift,
        cslg.least_decode_to_display_delta,
        cslg.greatest_decode_to_display_delta,
        cslg.composition_start_time,
        cslg.composition_end_time,
    ];
    let need_v1 = fields
        .iter()
        .any(|&v| v < i32::MIN as i64 || v > i32::MAX as i64);
    let mut p = Vec::with_capacity(4 + if need_v1 { 40 } else { 20 });
    p.push(if need_v1 { 1 } else { 0 }); // version
    p.extend_from_slice(&[0, 0, 0]); // flags
    for v in fields {
        if need_v1 {
            p.extend_from_slice(&v.to_be_bytes());
        } else {
            p.extend_from_slice(&(v as i32).to_be_bytes());
        }
    }
    Some(p)
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

fn build_stsc(chunks: &[TrackChunk]) -> Vec<u8> {
    // Sample-to-chunk (ISO/IEC 14496-12 §8.7.4 / QTFF p. 76): the table
    // is run-length compressed — one entry per run of consecutive
    // chunks sharing the same samples-per-chunk value; each entry names
    // the 1-based first chunk of its run. All samples use sample
    // description 1 (the muxer emits a single stsd entry per track).
    let mut entries: Vec<(u32, u32)> = Vec::new(); // (first_chunk, samples_per_chunk)
    for (i, c) in chunks.iter().enumerate() {
        if entries.last().map(|&(_, spc)| spc) != Some(c.sample_count) {
            entries.push(((i as u32) + 1, c.sample_count));
        }
    }
    let mut p = Vec::with_capacity(8 + entries.len() * 12);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes()); // entry_count
    for (first_chunk, samples_per_chunk) in entries {
        p.extend_from_slice(&first_chunk.to_be_bytes());
        p.extend_from_slice(&samples_per_chunk.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes()); // sample_description_id
    }
    p
}

/// Push the per-track sample-size box into `stbl`. Emits the Compact
/// Sample Size Box (`stz2`, ISO/IEC 14496-12 §8.7.3.3) when the track
/// opted in via `compact_sample_size` AND it would be smaller than the
/// default Sample Size Box (`stsz`, §8.7.3.2) — i.e. the sizes are not
/// uniform (a uniform `stsz` carries no table at all) and every size
/// fits in 4 or 8 bits. Otherwise emits `stsz`.
fn push_sample_size_atom(stbl: &mut Vec<u8>, t: &TrackWrite) {
    if let Some(body) = build_stz2(t) {
        push_atom(stbl, *b"stz2", &body);
    } else {
        push_atom(stbl, *b"stsz", &build_stsz(t));
    }
}

/// Build a `stz2` Compact Sample Size Box body (ISO/IEC 14496-12
/// §8.7.3.3) — `[ver+flags=4][reserved:24][field_size:8][count:u32]
/// [packed entries]`. Returns `None` (so the caller falls back to
/// `stsz`) when the track did not opt in, when the sizes are uniform (a
/// table-less `stsz` is already smaller), or when any size exceeds the
/// 8-bit field the narrow forms allow — the 16-bit `stz2` form is never
/// smaller than `stsz`'s 32-bit table by enough to matter here, so we
/// only emit the 4- and 8-bit narrow forms that genuinely save space.
fn build_stz2(t: &TrackWrite) -> Option<Vec<u8>> {
    if !t.compact_sample_size {
        return None;
    }
    let n = t.samples.len();
    let first = t.sample_size(0);
    if (0..n).all(|i| t.sample_size(i) == first) {
        // Uniform ⇒ `stsz` carries a single sample_size and no table,
        // strictly smaller than any `stz2`.
        return None;
    }
    let max = (0..n).map(|i| t.sample_size(i)).max().unwrap_or(0);
    // Narrowest field that fits every size. 4-bit fits 0..=15, 8-bit
    // 0..=255; larger sizes fall back to the 32-bit `stsz` table.
    let field_size: u8 = if max <= 0x0F {
        4
    } else if max <= 0xFF {
        8
    } else {
        return None;
    };
    let count = n as u32;
    let mut p = Vec::with_capacity(12 + n);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.push(0); // reserved high byte (part of the 24-bit reserved)
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved low 16 bits
    p.push(field_size);
    p.extend_from_slice(&count.to_be_bytes());
    match field_size {
        4 => {
            // Two values per byte, MSB-first; odd count zero-pads the
            // final low nibble (§8.7.3.3.2).
            let mut i = 0;
            while i < n {
                let hi = (t.sample_size(i) as u8) & 0x0F;
                let lo = if i + 1 < n {
                    (t.sample_size(i + 1) as u8) & 0x0F
                } else {
                    0
                };
                p.push((hi << 4) | lo);
                i += 2;
            }
        }
        8 => {
            for i in 0..n {
                p.push(t.sample_size(i) as u8);
            }
        }
        _ => unreachable!(),
    }
    Some(p)
}

fn build_stsz(t: &TrackWrite) -> Vec<u8> {
    // Uniform sample size if every sample is the same length;
    // per-sample table otherwise.
    let n = t.samples.len();
    let first = t.sample_size(0);
    let uniform = (0..n).all(|i| t.sample_size(i) == first);
    let count = n as u32;
    if uniform {
        let mut p = Vec::with_capacity(12);
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&first.to_be_bytes()); // sample_size
        p.extend_from_slice(&count.to_be_bytes());
        p
    } else {
        let mut p = Vec::with_capacity(12 + n * 4);
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0 ⇒ table follows
        p.extend_from_slice(&count.to_be_bytes());
        for i in 0..n {
            p.extend_from_slice(&t.sample_size(i).to_be_bytes());
        }
        p
    }
}

fn build_stco(chunks: &[TrackChunk]) -> Vec<u8> {
    // Chunk Offset Box (ISO/IEC 14496-12 §8.7.5 / QTFF p. 78): one
    // 32-bit absolute file offset per chunk, in chunk order.
    let mut p = Vec::with_capacity(8 + chunks.len() * 4);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&(chunks.len() as u32).to_be_bytes()); // entry_count
    for c in chunks {
        p.extend_from_slice(&(c.offset as u32).to_be_bytes());
    }
    p
}

fn build_co64(chunks: &[TrackChunk]) -> Vec<u8> {
    // 64-bit chunk-offset form (ISO/IEC 14496-12 §8.7.5), used when
    // any chunk starts beyond the 32-bit range.
    let mut p = Vec::with_capacity(8 + chunks.len() * 8);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&(chunks.len() as u32).to_be_bytes()); // entry_count
    for c in chunks {
        p.extend_from_slice(&c.offset.to_be_bytes());
    }
    p
}

fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
    let size: u32 = (8 + body.len()) as u32;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&fourcc);
    out.extend_from_slice(body);
}

// ────────────────────────── fragmented encoders ──────────────────────────

/// One track's slice for a single fragment. References the source
/// muxer's track (by index) plus the run of samples destined for this
/// fragment's `trun`.
struct FragmentRun<'a> {
    /// Index into `MovMuxer::tracks` — used to look up the track id
    /// (1-based: `track_idx + 1`) and media-timescale.
    track_idx: usize,
    samples: &'a [MuxSample],
    /// Per-sample auxiliary-information blobs for exactly the samples in
    /// [`Self::samples`] (ISO/IEC 14496-12 §8.7.8 / §8.7.9, §8.8.14).
    /// `None` when the owning track carries no `sample_aux` stream; when
    /// `Some`, the slice length equals `samples.len()` and indexes the
    /// same fragment-local samples. Drives the `traf`-scope `saiz` /
    /// `saio` pair and the trailing aux slab in the fragment's `mdat`.
    aux: Option<&'a [Vec<u8>]>,
    /// `(aux_info_type, aux_info_type_parameter)` discriminator copied
    /// from the track's [`SampleAuxStream`]. Only meaningful when
    /// [`Self::aux`] is `Some`.
    aux_info_type: Option<[u8; 4]>,
    aux_info_type_parameter: u32,
}

/// Slice every track's flat sample list into per-fragment runs per
/// the requested [`FragmentationMode`].
///
/// The output is `Vec<Vec<FragmentRun>>` — outer index = fragment
/// number, inner index = per-track run inside that fragment. Tracks
/// always appear in the same order they were added with
/// [`MovMuxer::add_track`]. Empty runs are preserved (e.g. when the
/// secondary track has no samples that overlap the primary's
/// fragment window) so per-fragment track ids stay contiguous.
fn slice_fragments(tracks: &[TrackWrite], mode: FragmentationMode) -> Vec<Vec<FragmentRun<'_>>> {
    if tracks.is_empty() {
        return Vec::new();
    }
    // Compute primary-track per-sample DTS table.
    let primary = &tracks[0];
    let primary_ts = primary.media_timescale.max(1) as u64;
    let mut primary_dts_starts: Vec<u64> = Vec::with_capacity(primary.samples.len() + 1);
    let mut dts: u64 = 0;
    for s in &primary.samples {
        primary_dts_starts.push(dts);
        dts = dts.saturating_add(s.duration as u64);
    }
    primary_dts_starts.push(dts); // sentinel = end DTS

    // Derive per-fragment primary-sample [start, end) index pairs
    // from the policy.
    let mut frag_primary_ranges: Vec<(usize, usize)> = Vec::new();
    match mode {
        FragmentationMode::ByDuration(threshold) => {
            let mut start = 0usize;
            let mut acc: u64 = 0;
            for (i, s) in primary.samples.iter().enumerate() {
                acc = acc.saturating_add(s.duration as u64);
                if acc >= threshold {
                    frag_primary_ranges.push((start, i + 1));
                    start = i + 1;
                    acc = 0;
                }
            }
            if start < primary.samples.len() {
                frag_primary_ranges.push((start, primary.samples.len()));
            }
        }
        FragmentationMode::ByFrameCount(n) => {
            let n = n as usize;
            let mut start = 0usize;
            while start < primary.samples.len() {
                let end = (start + n).min(primary.samples.len());
                frag_primary_ranges.push((start, end));
                start = end;
            }
        }
    }

    // Convert each fragment's [start, end) primary-sample range into
    // a primary-track *media-time* window so secondary tracks can be
    // sliced along the same time boundary. The last fragment swallows
    // any residual time so trailing secondary-track samples are not
    // dropped (per DASH §6.3.4.2 "media segments cover the entire
    // duration of the Representation").
    let mut fragments: Vec<Vec<FragmentRun<'_>>> = Vec::with_capacity(frag_primary_ranges.len());
    for (frag_idx, (p_start, p_end)) in frag_primary_ranges.iter().enumerate() {
        let is_last = frag_idx + 1 == frag_primary_ranges.len();
        let prim_start_dts = primary_dts_starts[*p_start];
        let prim_end_dts = if is_last {
            // Last fragment: sweep everything to the end.
            u64::MAX
        } else {
            primary_dts_starts[*p_end]
        };
        let mut runs: Vec<FragmentRun<'_>> = Vec::with_capacity(tracks.len());
        for (ti, t) in tracks.iter().enumerate() {
            // Resolve the [lo, hi) fragment-local sample range for this
            // track, then slice both the sample list and (when present)
            // the parallel auxiliary-information blob list with it.
            let (lo, hi) = if ti == 0 {
                (*p_start, *p_end)
            } else {
                // Walk the secondary track's flat sample list,
                // selecting samples whose DTS-start (computed in
                // its own timescale) lies within
                // [prim_start_dts, prim_end_dts) *after* rescaling
                // to the primary timescale.
                let sec_ts = t.media_timescale.max(1) as u64;
                let mut s_start: Option<usize> = None;
                let mut s_end: Option<usize> = None;
                let mut acc: u64 = 0;
                for (i, s) in t.samples.iter().enumerate() {
                    let start_in_prim = (acc * primary_ts + sec_ts / 2) / sec_ts;
                    if s_start.is_none() && start_in_prim >= prim_start_dts {
                        s_start = Some(i);
                    }
                    if start_in_prim >= prim_end_dts {
                        s_end = Some(i);
                        break;
                    }
                    acc = acc.saturating_add(s.duration as u64);
                }
                let lo = s_start.unwrap_or(t.samples.len());
                let hi = s_end.unwrap_or(t.samples.len());
                if lo <= hi {
                    (lo, hi)
                } else {
                    (0, 0)
                }
            };
            let run_slice = &t.samples[lo..hi];
            let (aux, aux_info_type, aux_info_type_parameter) = match &t.sample_aux {
                Some(stream) => (
                    Some(&stream.per_sample[lo..hi]),
                    stream.aux_info_type,
                    stream.aux_info_type_parameter,
                ),
                None => (None, None, 0),
            };
            runs.push(FragmentRun {
                track_idx: ti,
                samples: run_slice,
                aux,
                aux_info_type,
                aux_info_type_parameter,
            });
        }
        fragments.push(runs);
    }
    fragments
}

/// Build the fragmented-MP4 `ftyp` atom.
///
/// Brands chosen for ISO/IEC 23009-1 DASH compatibility:
///   * major = `iso5` (ISO BMFF §8.8.7.1 note — the brand that
///     requires `default-base-is-moof` semantics in `tfhd`).
///   * compatible = `iso5` + `isom` + `mp42` + `dash` + `msdh`.
fn build_ftyp_fragmented() -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 5 * 4);
    body.extend_from_slice(b"iso5");
    body.extend_from_slice(&0x0000_0200u32.to_be_bytes());
    body.extend_from_slice(b"iso5");
    body.extend_from_slice(b"isom");
    body.extend_from_slice(b"mp42");
    body.extend_from_slice(b"dash");
    body.extend_from_slice(b"msdh");
    let mut out = Vec::with_capacity(8 + body.len());
    push_atom(&mut out, *b"ftyp", &body);
    out
}

/// Build the init-segment `moov`. Per ISO/IEC 14496-12 §8.8.1, a
/// fragmented file's `moov` declares the global movie + per-track
/// structures (handler, codec config, timescale) but emits *empty*
/// sample tables — the per-fragment `moof`s carry the actual sample
/// indexing. Each track gets one `mvex/trex` defaults record.
fn build_init_moov(m: &MovMuxer) -> Vec<u8> {
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_init_mvhd(m));
    for (idx, t) in m.tracks.iter().enumerate() {
        let trak = build_init_trak(t, (idx as u32) + 1, m.movie_timescale);
        push_atom(&mut moov, *b"trak", &trak);
    }
    // mvex with one trex per track.
    let mut mvex = Vec::new();
    for (idx, t) in m.tracks.iter().enumerate() {
        let trex = build_trex(t, (idx as u32) + 1);
        push_atom(&mut mvex, *b"trex", &trex);
    }
    push_atom(&mut moov, *b"mvex", &mvex);
    moov
}

/// `mvhd` for the init segment. Same body as the non-fragmented
/// `build_mvhd` but with `duration = 0` per DASH §6.3.4.2 (the
/// presentation duration is reconstructed from the `moof` runs, not
/// declared in `mvhd`).
fn build_init_mvhd(m: &MovMuxer) -> Vec<u8> {
    let mut p = vec![0u8; 100];
    p[12..16].copy_from_slice(&m.movie_timescale.to_be_bytes());
    // duration = 0 (set by DASH consumers — the fragment runs carry
    // the actual playable duration). DASH spec note: "The duration
    // of the movie may not be known at the time of authoring".
    p[16..20].copy_from_slice(&0u32.to_be_bytes());
    p[20..24].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // rate=1.0
    p[24..26].copy_from_slice(&0x0100i16.to_be_bytes()); // volume=1.0
    write_movie_matrix(&mut p[36..72], m.movie_matrix.as_ref());
    for (i, v) in m.movie_times.iter().enumerate() {
        p[72 + i * 4..76 + i * 4].copy_from_slice(&v.to_be_bytes());
    }
    p[96..100].copy_from_slice(&((m.tracks.len() as u32) + 1).to_be_bytes());
    p
}

/// Build an init-segment `trak`. Same shape as `build_trak` but the
/// `stbl` body is empty per ISO BMFF §8.8.4 (no in-moov samples).
fn build_init_trak(t: &TrackWrite, track_id: u32, _movie_ts: u32) -> Vec<u8> {
    let mut trak = Vec::new();
    // tkhd: declare duration = 0 (set by fragments).
    push_atom(&mut trak, *b"tkhd", &build_init_tkhd(t, track_id));
    // edts > elst between tkhd and mdia, exactly as on the
    // non-fragmented path (QTFF p. 46, Figure 2-8) — a fragmented
    // presentation carries its edit list in the init-segment `moov`
    // (the `moof`s only index samples), so priming-skip / start-delay
    // edits set via `set_edit_list` must land here to reach players.
    if !t.edits.is_empty() {
        push_atom(&mut trak, *b"edts", &build_edts(&t.edits));
    }
    push_atom(&mut trak, *b"mdia", &build_init_mdia(t));
    trak
}

fn build_init_tkhd(t: &TrackWrite, track_id: u32) -> Vec<u8> {
    let mut p = vec![0u8; 84];
    let flags = t.tkhd_flags.unwrap_or(0x07);
    p[1] = (flags >> 16) as u8;
    p[2] = (flags >> 8) as u8;
    p[3] = flags as u8;
    p[12..16].copy_from_slice(&track_id.to_be_bytes());
    // duration = 0 for fragmented init segments.
    p[20..24].copy_from_slice(&0u32.to_be_bytes());
    p[32..34].copy_from_slice(&t.tkhd_layer.to_be_bytes());
    p[34..36].copy_from_slice(&t.tkhd_alternate_group.to_be_bytes());
    let volume = t
        .tkhd_volume
        .unwrap_or(if matches!(t.kind, MuxTrackKind::Audio { .. }) {
            0x0100
        } else {
            0
        });
    p[36..38].copy_from_slice(&volume.to_be_bytes());
    write_movie_matrix(&mut p[40..76], t.tkhd_matrix.as_ref());
    let (w_fp, h_fp) = match &t.kind {
        MuxTrackKind::Video { width, height, .. } => {
            ((*width as u32) << 16, (*height as u32) << 16)
        }
        MuxTrackKind::Audio { .. }
        | MuxTrackKind::Timecode { .. }
        | MuxTrackKind::Text { .. }
        | MuxTrackKind::Metadata { .. }
        | MuxTrackKind::Subtitle { .. }
        | MuxTrackKind::SimpleText { .. }
        | MuxTrackKind::Hint { .. } => (0, 0),
    };
    p[76..80].copy_from_slice(&w_fp.to_be_bytes());
    p[80..84].copy_from_slice(&h_fp.to_be_bytes());
    p
}

fn build_init_mdia(t: &TrackWrite) -> Vec<u8> {
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_init_mdhd(t));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(t));
    if let Some(tag) = &t.extended_language {
        push_atom(&mut mdia, *b"elng", &build_elng(tag));
    }
    push_atom(&mut mdia, *b"minf", &build_init_minf(t));
    mdia
}

fn build_init_mdhd(t: &TrackWrite) -> Vec<u8> {
    let mut p = vec![0u8; 24];
    p[12..16].copy_from_slice(&t.media_timescale.to_be_bytes());
    // duration = 0 for fragmented init segments.
    p[16..20].copy_from_slice(&0u32.to_be_bytes());
    p[20..22].copy_from_slice(&t.media_language.to_be_bytes());
    p
}

fn build_init_minf(t: &TrackWrite) -> Vec<u8> {
    let mut minf = Vec::new();
    match &t.kind {
        MuxTrackKind::Video { .. } => push_atom(&mut minf, *b"vmhd", &build_vmhd()),
        MuxTrackKind::Audio { .. } => push_atom(&mut minf, *b"smhd", &build_smhd()),
        MuxTrackKind::Timecode { tcmi, .. } => {
            push_atom(&mut minf, *b"gmhd", &build_gmhd_tmcd(tcmi, t.gmin.as_ref()));
        }
        MuxTrackKind::Text { .. } => {
            push_atom(
                &mut minf,
                *b"gmhd",
                &build_gmhd_text(t.gmin.as_ref(), t.text_header_matrix.as_ref()),
            );
        }
        MuxTrackKind::Metadata { .. } => {
            // Null Media Header Box (ISO/IEC 14496-12 §8.4.5.2): an empty
            // FullBox(version=0, flags=0). Metadata tracks carry no
            // specific media header.
            push_atom(&mut minf, *b"nmhd", &0u32.to_be_bytes());
        }
        MuxTrackKind::Subtitle { .. } => {
            // Subtitle Media Header Box (ISO/IEC 14496-12 §12.6.2): an
            // empty FullBox(version=0, flags=0).
            push_atom(&mut minf, *b"sthd", &0u32.to_be_bytes());
        }
        MuxTrackKind::SimpleText { .. } => {
            // Timed-text tracks use a Null Media Header Box (ISO/IEC
            // 14496-12 §12.5.2), distinct from the QuickTime text track's
            // `gmhd`.
            push_atom(&mut minf, *b"nmhd", &0u32.to_be_bytes());
        }
        MuxTrackKind::Hint { hmhd, .. } => {
            // Hint Media Header Box (ISO/IEC 14496-12 §12.4.2).
            push_atom(&mut minf, *b"hmhd", &hmhd.to_body_bytes());
        }
    }
    push_atom(&mut minf, *b"dinf", &build_dinf(&t.data_references));
    push_atom(&mut minf, *b"stbl", &build_init_stbl(t));
    minf
}

/// Init-segment `stbl`: same `stsd` as the non-fragmented build, but
/// `stts`/`stsc`/`stsz`/`stco` are all empty per ISO BMFF §8.8.4.
fn build_init_stbl(t: &TrackWrite) -> Vec<u8> {
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &build_stsd(t));
    push_atom(&mut stbl, *b"stts", &build_empty_stts());
    push_atom(&mut stbl, *b"stsc", &build_empty_stsc());
    push_atom(&mut stbl, *b"stsz", &build_empty_stsz());
    push_atom(&mut stbl, *b"stco", &build_empty_stco());
    stbl
}

fn build_empty_stts() -> Vec<u8> {
    let mut p = Vec::with_capacity(8);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&0u32.to_be_bytes()); // entry_count = 0
    p
}

fn build_empty_stsc() -> Vec<u8> {
    let mut p = Vec::with_capacity(8);
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p
}

fn build_empty_stsz() -> Vec<u8> {
    let mut p = Vec::with_capacity(12);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0
    p.extend_from_slice(&0u32.to_be_bytes()); // count = 0
    p
}

fn build_empty_stco() -> Vec<u8> {
    let mut p = Vec::with_capacity(8);
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p
}

/// One `trex` per track. ISO/IEC 14496-12 §8.8.3.2.
///
/// Default sample flags = `0x0001_0000` (`sample_is_non_sync_sample`
/// set) for video tracks — every per-fragment override that lands a
/// sync sample then needs to clear the bit via the `trun`'s
/// `first_sample_flags`. For audio we keep the default at 0 (sync)
/// since every audio sample is conventionally a sync sample.
fn build_trex(t: &TrackWrite, track_id: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(24);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&track_id.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes()); // default_sample_description_index
    p.extend_from_slice(&0u32.to_be_bytes()); // default_sample_duration (we send per-sample)
    p.extend_from_slice(&0u32.to_be_bytes()); // default_sample_size (per-sample)
    let default_flags = match &t.kind {
        MuxTrackKind::Video { .. } => 0x0001_0000u32, // non-sync default
        // Audio, time-code, text, and timed-metadata samples are each
        // independently decodable (a metadata sample is an "I-frame"
        // carrying the complete metadata for its interval, §12.3.3.1).
        MuxTrackKind::Audio { .. }
        | MuxTrackKind::Timecode { .. }
        | MuxTrackKind::Text { .. }
        | MuxTrackKind::Metadata { .. }
        | MuxTrackKind::Subtitle { .. }
        | MuxTrackKind::SimpleText { .. }
        | MuxTrackKind::Hint { .. } => 0u32,
    };
    p.extend_from_slice(&default_flags.to_be_bytes());
    p
}

/// Build the per-fragment `tfhd` box payload using
/// `default-base-is-moof` (no explicit base_data_offset; the spec's
/// "first byte of the enclosing Movie Fragment Box" anchor).
///
/// Per ISO/IEC 14496-12 §8.8.7: when every sample has explicit size +
/// duration + flags via `trun`, no per-fragment defaults are needed,
/// so we omit them all and the `tf_flags` carries only the
/// default-base-is-moof bit.
fn build_tfhd(track_id: u32) -> Vec<u8> {
    use crate::fragment::TFHD_DEFAULT_BASE_IS_MOOF;
    let flags = TFHD_DEFAULT_BASE_IS_MOOF;
    let mut p = Vec::with_capacity(8);
    p.extend_from_slice(&flags.to_be_bytes());
    p.extend_from_slice(&track_id.to_be_bytes());
    p
}

/// Build a `trun` carrying explicit per-sample size + duration +
/// flags for every sample in the run. ISO/IEC 14496-12 §8.8.8.2.
///
/// Layout produced (each per-sample row = 12 bytes):
///   `[ver+flags:4][sample_count:4][data_offset:4]`
///   N × `[duration:4][size:4][flags:4]`
///
/// `data_offset` is the trun-payload signed `i32`; the caller supplies
/// the precomputed value (it depends on the moof's total byte size,
/// which is fixed at this point).
fn build_trun(samples: &[MuxSample], data_offset: i32) -> Vec<u8> {
    use crate::fragment::{
        TRUN_DATA_OFFSET_PRESENT, TRUN_SAMPLE_DURATION_PRESENT, TRUN_SAMPLE_FLAGS_PRESENT,
        TRUN_SAMPLE_SIZE_PRESENT,
    };
    let flags = TRUN_DATA_OFFSET_PRESENT
        | TRUN_SAMPLE_DURATION_PRESENT
        | TRUN_SAMPLE_SIZE_PRESENT
        | TRUN_SAMPLE_FLAGS_PRESENT;
    let mut p = Vec::with_capacity(12 + samples.len() * 12);
    p.extend_from_slice(&flags.to_be_bytes()); // ver=0 + flags
    p.extend_from_slice(&(samples.len() as u32).to_be_bytes()); // sample_count
    p.extend_from_slice(&data_offset.to_be_bytes()); // data_offset (signed)
    for s in samples {
        p.extend_from_slice(&s.duration.to_be_bytes());
        p.extend_from_slice(&(s.data.len() as u32).to_be_bytes());
        // sample_flags: 0 (sync) for keyframes, 0x0001_0000 (non-sync)
        // otherwise. ISO/IEC 14496-12 §8.8.3.1 — `sample_is_non_sync_
        // sample` bit at 0x0001_0000.
        let f: u32 = if s.keyframe { 0 } else { 0x0001_0000 };
        p.extend_from_slice(&f.to_be_bytes());
    }
    p
}

/// Build a `traf` payload (per §8.8.6.2): `tfhd` + `trun`, plus the
/// optional `saiz` + `saio` pair (§8.7.8 / §8.7.9 at `traf` scope,
/// §8.8.14). `saio_offset` is the moof-relative offset of this traf's
/// auxiliary-information slab in the fragment's `mdat`; it is consulted
/// only when the run carries an aux stream.
fn build_traf(run: &FragmentRun<'_>, data_offset: i32, saio_offset: u64) -> Vec<u8> {
    let track_id = (run.track_idx as u32) + 1;
    let mut traf = Vec::new();
    push_atom(&mut traf, *b"tfhd", &build_tfhd(track_id));
    push_atom(&mut traf, *b"trun", &build_trun(run.samples, data_offset));
    // §8.8.14: when sample-auxiliary information rides in the fragment,
    // the `saio` offsets are relative to the track-fragment base offset;
    // `build_tfhd` always sets `default-base-is-moof`, so that base is
    // the enclosing `moof`'s first byte.
    if let Some(blobs) = run.aux {
        push_atom(
            &mut traf,
            *b"saiz",
            &build_saiz_blobs(run.aux_info_type, run.aux_info_type_parameter, blobs),
        );
        push_atom(
            &mut traf,
            *b"saio",
            &build_saio_offset(run.aux_info_type, run.aux_info_type_parameter, saio_offset),
        );
    }
    traf
}

/// Build the full `moof` payload for a fragment. The `data_offsets`
/// and `saio_offsets` slices are parallel to `fragment` and carry,
/// respectively, the precomputed `trun.data_offset` and the
/// moof-relative `saio` slab offset for each track's run.
fn build_moof(
    sequence_number: u32,
    fragment: &[FragmentRun<'_>],
    data_offsets: &[i32],
    saio_offsets: &[u64],
) -> Vec<u8> {
    debug_assert_eq!(fragment.len(), data_offsets.len());
    debug_assert_eq!(fragment.len(), saio_offsets.len());
    let mut moof = Vec::new();
    push_atom(&mut moof, *b"mfhd", &build_mfhd(sequence_number));
    for ((run, &data_offset), &saio_offset) in fragment.iter().zip(data_offsets).zip(saio_offsets) {
        push_atom(
            &mut moof,
            *b"traf",
            &build_traf(run, data_offset, saio_offset),
        );
    }
    moof
}

fn build_mfhd(sequence_number: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(8);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&sequence_number.to_be_bytes());
    p
}

/// Measure the byte length of the full `moof` box (header + payload)
/// for the supplied fragment. Used by the two-pass build to compute
/// each track's `trun.data_offset` (which depends on the moof's
/// total byte length).
fn measure_moof(sequence_number: u32, fragment: &[FragmentRun<'_>]) -> u64 {
    // Sizing pass uses placeholder data/saio offsets of 0. `trun`'s
    // data_offset is a fixed 4-byte slot, so its value never affects
    // the size. `saio`'s width is v0 (4-byte) for any offset ≤ u32::MAX;
    // the real moof-relative slab offsets the muxer produces stay well
    // under 4 GiB, so both the placeholder and the real value select v0
    // and the sizing pass matches the emit pass (pinned by the
    // `debug_assert_eq!` on `moof_size` in `encode_fragmented_to_vec`).
    let placeholder_data: Vec<i32> = vec![0; fragment.len()];
    let placeholder_saio: Vec<u64> = vec![0; fragment.len()];
    let moof = build_moof(
        sequence_number,
        fragment,
        &placeholder_data,
        &placeholder_saio,
    );
    8 + moof.len() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "registry")]
    use crate::demuxer::MovDemuxer;
    #[cfg(feature = "registry")]
    use oxideav_core::ReadSeek;
    #[cfg(feature = "registry")]
    use std::io::Cursor;

    fn synth_video_samples(n: usize) -> Vec<MuxSample> {
        (0..n)
            .map(|i| MuxSample {
                data: vec![(i & 0xFF) as u8; 16 + (i % 8)],
                duration: 1000,
                keyframe: i % 5 == 0,
                composition_offset: 0,
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
                    composition_offset: 0,
                },
                MuxSample {
                    data: vec![0; 4],
                    duration: 33,
                    keyframe: false,
                    composition_offset: 0,
                },
                MuxSample {
                    data: vec![0; 4],
                    duration: 33,
                    keyframe: false,
                    composition_offset: 0,
                },
            ],
            extra_stsd_atoms: Vec::new(),
            sample_aux: None,
            sample_to_groups: Vec::new(),
            sample_group_descriptions: Vec::new(),
            cslg: None,
            edits: Vec::new(),
            metadata: Vec::new(),
            apple_metadata: Vec::new(),
            visual_extensions: VisualExtensions::default(),
            track_references: Vec::new(),
            tapt: None,
            data_references: Vec::new(),
            external_media: None,
            media_language: MDHD_LANGUAGE_UND,
            extended_language: None,
            gmin: None,
            text_header_matrix: None,
            compact_sample_size: false,
            sdtp: Vec::new(),
            stdp: Vec::new(),
            padb: Vec::new(),
            stsh: Vec::new(),
            subs: Vec::new(),
            load: None,
            clipping: None,
            matte: None,
            track_kinds: Vec::new(),
            track_selection: None,
            track_groups: Vec::new(),
            audio_description: AudioDescriptionWrite::V0,
            tkhd_matrix: None,
            tkhd_layer: 0,
            tkhd_alternate_group: 0,
            tkhd_volume: None,
            tkhd_flags: None,
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

    /// Helper: build a single-video-track `TrackWrite` whose samples
    /// carry the given per-sample composition offsets (all 1000-tick
    /// duration, sample 0 a keyframe). Used by the `ctts` unit tests.
    fn track_with_offsets(offsets: &[i32]) -> TrackWrite {
        TrackWrite {
            kind: MuxTrackKind::Video {
                format: *b"avc1",
                width: 8,
                height: 8,
            },
            media_timescale: 30000,
            samples: offsets
                .iter()
                .enumerate()
                .map(|(i, &off)| MuxSample {
                    data: vec![(i & 0xFF) as u8; 8],
                    duration: 1000,
                    keyframe: i == 0,
                    composition_offset: off,
                })
                .collect(),
            extra_stsd_atoms: Vec::new(),
            sample_aux: None,
            sample_to_groups: Vec::new(),
            sample_group_descriptions: Vec::new(),
            cslg: None,
            edits: Vec::new(),
            metadata: Vec::new(),
            apple_metadata: Vec::new(),
            visual_extensions: VisualExtensions::default(),
            track_references: Vec::new(),
            tapt: None,
            data_references: Vec::new(),
            external_media: None,
            media_language: MDHD_LANGUAGE_UND,
            extended_language: None,
            gmin: None,
            text_header_matrix: None,
            compact_sample_size: false,
            sdtp: Vec::new(),
            stdp: Vec::new(),
            padb: Vec::new(),
            stsh: Vec::new(),
            subs: Vec::new(),
            load: None,
            clipping: None,
            matte: None,
            track_kinds: Vec::new(),
            track_selection: None,
            track_groups: Vec::new(),
            audio_description: AudioDescriptionWrite::V0,
            tkhd_matrix: None,
            tkhd_layer: 0,
            tkhd_alternate_group: 0,
            tkhd_volume: None,
            tkhd_flags: None,
        }
    }

    #[test]
    fn ctts_omitted_when_all_offsets_zero() {
        let t = track_with_offsets(&[0, 0, 0, 0]);
        assert!(
            build_ctts(&t).is_none(),
            "ctts must be omitted when PTS == DTS for every sample"
        );
    }

    #[test]
    fn ctts_v0_when_all_offsets_non_negative() {
        // Classic IBBP-style reorder: positive offsets only ⇒ version 0.
        let t = track_with_offsets(&[0, 2000, 1000, 1000, 0]);
        let body = build_ctts(&t).expect("ctts emitted");
        assert_eq!(body[0], 0, "version 0 expected for non-negative offsets");
        let n = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
        // Runs: (1,0) (1,2000) (2,1000) (1,0) ⇒ 4 runs.
        assert_eq!(n, 4);
        // First run.
        assert_eq!(
            u32::from_be_bytes([body[8], body[9], body[10], body[11]]),
            1
        );
        assert_eq!(
            u32::from_be_bytes([body[12], body[13], body[14], body[15]]),
            0
        );
        // Third run is the merged pair of 1000-offset samples.
        let r3_count = u32::from_be_bytes([body[24], body[25], body[26], body[27]]);
        let r3_off = u32::from_be_bytes([body[28], body[29], body[30], body[31]]);
        assert_eq!(r3_count, 2);
        assert_eq!(r3_off, 1000);
        assert_eq!(body.len(), 8 + 4 * 8);
    }

    #[test]
    fn ctts_promotes_to_v1_when_any_offset_negative() {
        let t = track_with_offsets(&[0, -1000, 1000]);
        let body = build_ctts(&t).expect("ctts emitted");
        assert_eq!(body[0], 1, "version 1 forced by a negative offset");
        // The negative offset round-trips as a signed int(32).
        let n = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
        assert_eq!(n, 3);
        // Run 1 (the second run): sample_count at body[16..20], signed
        // sample_offset at body[20..24].
        let r2_count = u32::from_be_bytes([body[16], body[17], body[18], body[19]]);
        let r2_off = i32::from_be_bytes([body[20], body[21], body[22], body[23]]);
        assert_eq!(r2_count, 1);
        assert_eq!(r2_off, -1000);
    }

    #[cfg(feature = "registry")]
    #[test]
    fn ctts_roundtrips_through_demuxer() {
        // Encode a reordered stream and confirm the demuxer reconstructs
        // each sample's PTS = DTS + composition_offset. Mixed signs force
        // the v1 form; the demuxer's parse_ctts normalises both.
        let offsets = [0i32, 3000, -1000, 2000, 0];
        let mut m = MovMuxer::new().with_movie_timescale(600);
        m.add_track(
            MuxTrackKind::Video {
                format: *b"mp4v",
                width: 64,
                height: 48,
            },
            30000,
            track_with_offsets(&offsets).samples,
            &[],
        );
        let bytes = m.encode_to_vec().expect("encode reordered MOV");

        let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
        let d = MovDemuxer::open(cur).expect("open reordered MOV");
        let st = &d.tracks[0].sample_table;
        assert!(!st.ctts.is_empty(), "ctts table parsed back");
        let entries: Vec<_> = st
            .iter_samples()
            .collect::<oxideav_core::Result<_>>()
            .unwrap();
        assert_eq!(entries.len(), offsets.len());
        let mut dts = 0i64;
        for (i, (entry, &off)) in entries.iter().zip(offsets.iter()).enumerate() {
            assert_eq!(
                entry.composition_offset, off,
                "composition offset at sample {i}"
            );
            assert_eq!(entry.pts(), dts + off as i64, "PTS at sample {i}");
            dts += entry.duration as i64;
        }
    }

    #[cfg(feature = "registry")]
    #[test]
    fn visual_extensions_roundtrip_through_demuxer() {
        use crate::media_meta::{ColorParameters, ColorParametersKind, FieldOrdering};
        // Build a one-sample video track decorated with every typed
        // visual extension box and confirm each surfaces back on the
        // demuxer's first sample description.
        let mut m = MovMuxer::new().with_movie_timescale(600);
        let tid = m.add_track(
            MuxTrackKind::Video {
                format: *b"mp4v",
                width: 1920,
                height: 1080,
            },
            30000,
            vec![MuxSample {
                data: vec![0u8; 16],
                duration: 1000,
                keyframe: true,
                composition_offset: 0,
            }],
            &[],
        );
        let ext = VisualExtensions {
            pasp: Some(Pasp {
                h_spacing: 40,
                v_spacing: 33,
            }),
            colr: Some(ColorParameters {
                kind: ColorParametersKind::Nclx {
                    primaries: 9,
                    transfer: 16,
                    matrix: 9,
                    full_range: true,
                },
            }),
            clap: Some(Clap {
                clean_aperture_width_n: 1916,
                clean_aperture_width_d: 1,
                clean_aperture_height_n: 1076,
                clean_aperture_height_d: 1,
                horiz_off_n: -2,
                horiz_off_d: 1,
                vert_off_n: 2,
                vert_off_d: 1,
            }),
            fiel: Some(Fiel {
                field_count: 2,
                field_ordering: 6,
            }),
            gamma: Some(0x0002_3333), // ~2.2 in 16.16
        };
        m.set_visual_extensions(tid, ext.clone())
            .expect("set visual extensions");
        let bytes = m
            .encode_to_vec()
            .expect("encode MOV with visual extensions");

        let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
        let d = MovDemuxer::open(cur).expect("open MOV with visual extensions");
        let sd = &d.tracks[0].sample_descriptions[0];
        assert_eq!(sd.pasp, ext.pasp, "pasp round-trips");
        assert_eq!(sd.colr, ext.colr, "colr round-trips");
        assert_eq!(sd.clap, ext.clap, "clap round-trips");
        assert_eq!(sd.fiel, ext.fiel, "fiel round-trips");
        assert_eq!(sd.gamma, ext.gamma, "gama round-trips");
        // Spot-check a typed accessor survived the round-trip.
        assert_eq!(
            sd.fiel.unwrap().ordering(),
            Some(FieldOrdering::BottomFieldFirst)
        );
    }

    #[cfg(feature = "registry")]
    #[test]
    fn visual_extensions_partial_only_emits_set_boxes() {
        use crate::media_meta::{ColorParameters, ColorParametersKind};
        // Only `colr` set ⇒ exactly one extension box, no pasp/clap/fiel.
        let mut m = MovMuxer::new().with_movie_timescale(600);
        let tid = m.add_track(
            MuxTrackKind::Video {
                format: *b"mp4v",
                width: 64,
                height: 48,
            },
            30000,
            vec![MuxSample {
                data: vec![0u8; 8],
                duration: 1000,
                keyframe: true,
                composition_offset: 0,
            }],
            &[],
        );
        let ext = VisualExtensions {
            colr: Some(ColorParameters {
                kind: ColorParametersKind::Nclc {
                    primaries: 1,
                    transfer: 1,
                    matrix: 1,
                },
            }),
            ..Default::default()
        };
        m.set_visual_extensions(tid, ext).expect("set colr only");
        let bytes = m.encode_to_vec().expect("encode");
        let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
        let d = MovDemuxer::open(cur).expect("open");
        let sd = &d.tracks[0].sample_descriptions[0];
        assert!(sd.colr.is_some(), "colr present");
        assert!(sd.pasp.is_none(), "no pasp emitted");
        assert!(sd.clap.is_none(), "no clap emitted");
        assert!(sd.fiel.is_none(), "no fiel emitted");
        assert!(sd.gamma.is_none(), "no gama emitted");
    }

    #[test]
    fn visual_extensions_rejected_on_audio_track() {
        let mut m = MovMuxer::new();
        let tid = m.add_track(
            MuxTrackKind::Audio {
                format: *b"mp4a",
                channels: 2,
                bits_per_sample: 16,
                sample_rate: 48000,
            },
            48000,
            vec![MuxSample {
                data: vec![0u8; 4],
                duration: 1024,
                keyframe: true,
                composition_offset: 0,
            }],
            &[],
        );
        let ext = VisualExtensions {
            pasp: Some(Pasp {
                h_spacing: 1,
                v_spacing: 1,
            }),
            ..Default::default()
        };
        assert!(
            m.set_visual_extensions(tid, ext).is_err(),
            "visual extensions are not valid on an audio sample entry"
        );
    }

    #[test]
    fn visual_extensions_unknown_track_id_errors() {
        let mut m = MovMuxer::new();
        assert!(m
            .set_visual_extensions(99, VisualExtensions::default())
            .is_err());
    }

    #[test]
    fn elst_v0_layout_unity_segment() {
        // A single unity-rate segment, all fields inside 32-bit ⇒ v0.
        let body = build_elst(&[MuxEdit::segment(6000, 0)]);
        assert_eq!(body[0], 0, "version 0 for 32-bit-fitting fields");
        assert_eq!(&body[1..4], &[0, 0, 0], "flags = 0");
        let n = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
        assert_eq!(n, 1);
        // 12-byte v0 entry: track_duration(u32) media_time(i32) rate(i32).
        assert_eq!(body.len(), 8 + 12);
        assert_eq!(
            u32::from_be_bytes([body[8], body[9], body[10], body[11]]),
            6000
        );
        assert_eq!(
            i32::from_be_bytes([body[12], body[13], body[14], body[15]]),
            0
        );
        assert_eq!(
            i32::from_be_bytes([body[16], body[17], body[18], body[19]]),
            0x0001_0000
        );
    }

    #[test]
    fn elst_empty_edit_writes_minus_one_media_time() {
        // An empty edit (media_time = -1) followed by a unity segment.
        let body = build_elst(&[MuxEdit::empty(1000), MuxEdit::segment(5000, 0)]);
        assert_eq!(body[0], 0);
        let n = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
        assert_eq!(n, 2);
        // First entry's media_time is the -1 sentinel.
        assert_eq!(
            i32::from_be_bytes([body[12], body[13], body[14], body[15]]),
            -1
        );
    }

    #[test]
    fn elst_promotes_to_v1_on_wide_duration() {
        // track_duration beyond u32::MAX forces the 64-bit v1 form.
        let body = build_elst(&[MuxEdit::segment(u32::MAX as u64 + 1, 0)]);
        assert_eq!(body[0], 1, "version 1 for >32-bit track_duration");
        // 20-byte v1 entry: track_duration(u64) media_time(i64) rate(i32).
        assert_eq!(body.len(), 8 + 20);
        assert_eq!(
            u64::from_be_bytes([
                body[8], body[9], body[10], body[11], body[12], body[13], body[14], body[15]
            ]),
            u32::MAX as u64 + 1
        );
    }

    #[test]
    fn set_edit_list_rejects_bad_negative_media_time() {
        let mut m = MovMuxer::new();
        m.add_track(
            MuxTrackKind::Video {
                format: *b"avc1",
                width: 8,
                height: 8,
            },
            30000,
            synth_video_samples(2),
            &[],
        );
        // -2 is not the empty-edit sentinel ⇒ rejected.
        assert!(m
            .set_edit_list(
                1,
                &[MuxEdit {
                    track_duration: 100,
                    media_time: -2,
                    media_rate: 0x0001_0000,
                }]
            )
            .is_err());
        // -1 (empty edit) is accepted.
        assert!(m.set_edit_list(1, &[MuxEdit::empty(100)]).is_ok());
        // Empty slice clears the list.
        assert!(m.set_edit_list(1, &[]).is_ok());
    }

    #[test]
    fn set_edit_list_unknown_track_id_errors() {
        let mut m = MovMuxer::new();
        m.add_track(
            MuxTrackKind::Video {
                format: *b"avc1",
                width: 8,
                height: 8,
            },
            30000,
            synth_video_samples(2),
            &[],
        );
        assert!(m.set_edit_list(2, &[MuxEdit::segment(100, 0)]).is_err());
        assert!(m.set_edit_list(0, &[MuxEdit::segment(100, 0)]).is_err());
    }

    #[cfg(feature = "registry")]
    #[test]
    fn edit_list_roundtrips_through_demuxer() {
        // Encode a track carrying a head empty-edit + a media-time
        // offset segment, then confirm the demuxer reads back the exact
        // per-segment track_duration / media_time / media_rate.
        let mut m = MovMuxer::new().with_movie_timescale(600);
        let tid = m.add_track(
            MuxTrackKind::Video {
                format: *b"mp4v",
                width: 64,
                height: 48,
            },
            30000,
            synth_video_samples(6),
            &[],
        );
        // Movie timescale is 600: a 1.0s empty edit = 600 ticks, then a
        // segment presenting media from media-time 1500 for 2.0s.
        let edits = [MuxEdit::empty(600), MuxEdit::segment(1200, 1500)];
        m.set_edit_list(tid, &edits).expect("attach edit list");
        let bytes = m.encode_to_vec().expect("encode MOV with edit list");

        let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
        let d = MovDemuxer::open(cur).expect("open MOV with edit list");
        let got = &d.tracks[0].edits;
        assert_eq!(got.len(), 2, "two edit segments parsed back");
        assert_eq!(got[0].track_duration, 600);
        assert_eq!(got[0].media_time, -1, "head is an empty edit");
        assert_eq!(got[1].track_duration, 1200);
        assert_eq!(got[1].media_time, 1500);
        assert_eq!(got[1].media_rate, 0x0001_0000);
    }

    #[cfg(feature = "registry")]
    #[test]
    fn no_edit_list_emits_no_edts() {
        // A track with no edit list must not carry an edts box (the
        // implicit "entire media is used" default, QTFF p. 46).
        let mut m = MovMuxer::new();
        m.add_track(
            MuxTrackKind::Video {
                format: *b"avc1",
                width: 16,
                height: 16,
            },
            30000,
            synth_video_samples(3),
            &[],
        );
        let bytes = m.encode_to_vec().expect("encode MOV");
        // edts FourCC must not appear anywhere in the file.
        assert!(
            !bytes.windows(4).any(|w| w == b"edts"),
            "no edts box when no edit list is set"
        );
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
                    composition_offset: 0,
                },
                MuxSample {
                    data: vec![0; 1024],
                    duration: 256,
                    keyframe: true,
                    composition_offset: 0,
                },
            ],
            extra_stsd_atoms: Vec::new(),
            sample_aux: None,
            sample_to_groups: Vec::new(),
            sample_group_descriptions: Vec::new(),
            cslg: None,
            edits: Vec::new(),
            metadata: Vec::new(),
            apple_metadata: Vec::new(),
            visual_extensions: VisualExtensions::default(),
            track_references: Vec::new(),
            tapt: None,
            data_references: Vec::new(),
            external_media: None,
            media_language: MDHD_LANGUAGE_UND,
            extended_language: None,
            gmin: None,
            text_header_matrix: None,
            compact_sample_size: false,
            sdtp: Vec::new(),
            stdp: Vec::new(),
            padb: Vec::new(),
            stsh: Vec::new(),
            subs: Vec::new(),
            load: None,
            clipping: None,
            matte: None,
            track_kinds: Vec::new(),
            track_selection: None,
            track_groups: Vec::new(),
            audio_description: AudioDescriptionWrite::V0,
            tkhd_matrix: None,
            tkhd_layer: 0,
            tkhd_alternate_group: 0,
            tkhd_volume: None,
            tkhd_flags: None,
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
            sample_aux: None,
            sample_to_groups: Vec::new(),
            sample_group_descriptions: Vec::new(),
            cslg: None,
            edits: Vec::new(),
            metadata: Vec::new(),
            apple_metadata: Vec::new(),
            visual_extensions: VisualExtensions::default(),
            track_references: Vec::new(),
            tapt: None,
            data_references: Vec::new(),
            external_media: None,
            media_language: MDHD_LANGUAGE_UND,
            extended_language: None,
            gmin: None,
            text_header_matrix: None,
            compact_sample_size: false,
            sdtp: Vec::new(),
            stdp: Vec::new(),
            padb: Vec::new(),
            stsh: Vec::new(),
            subs: Vec::new(),
            load: None,
            clipping: None,
            matte: None,
            track_kinds: Vec::new(),
            track_selection: None,
            track_groups: Vec::new(),
            audio_description: AudioDescriptionWrite::V0,
            tkhd_matrix: None,
            tkhd_layer: 0,
            tkhd_alternate_group: 0,
            tkhd_volume: None,
            tkhd_flags: None,
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
                    composition_offset: 0,
                },
                MuxSample {
                    data: vec![0; 64],
                    duration: 32,
                    keyframe: true,
                    composition_offset: 0,
                },
                MuxSample {
                    data: vec![0; 64],
                    duration: 32,
                    keyframe: true,
                    composition_offset: 0,
                },
            ],
            extra_stsd_atoms: Vec::new(),
            sample_aux: None,
            sample_to_groups: Vec::new(),
            sample_group_descriptions: Vec::new(),
            cslg: None,
            edits: Vec::new(),
            metadata: Vec::new(),
            apple_metadata: Vec::new(),
            visual_extensions: VisualExtensions::default(),
            track_references: Vec::new(),
            tapt: None,
            data_references: Vec::new(),
            external_media: None,
            media_language: MDHD_LANGUAGE_UND,
            extended_language: None,
            gmin: None,
            text_header_matrix: None,
            compact_sample_size: false,
            sdtp: Vec::new(),
            stdp: Vec::new(),
            padb: Vec::new(),
            stsh: Vec::new(),
            subs: Vec::new(),
            load: None,
            clipping: None,
            matte: None,
            track_kinds: Vec::new(),
            track_selection: None,
            track_groups: Vec::new(),
            audio_description: AudioDescriptionWrite::V0,
            tkhd_matrix: None,
            tkhd_layer: 0,
            tkhd_alternate_group: 0,
            tkhd_volume: None,
            tkhd_flags: None,
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
                    composition_offset: 0,
                },
                MuxSample {
                    data: vec![0; 17],
                    duration: 33,
                    keyframe: false,
                    composition_offset: 0,
                },
            ],
            extra_stsd_atoms: Vec::new(),
            sample_aux: None,
            sample_to_groups: Vec::new(),
            sample_group_descriptions: Vec::new(),
            cslg: None,
            edits: Vec::new(),
            metadata: Vec::new(),
            apple_metadata: Vec::new(),
            visual_extensions: VisualExtensions::default(),
            track_references: Vec::new(),
            tapt: None,
            data_references: Vec::new(),
            external_media: None,
            media_language: MDHD_LANGUAGE_UND,
            extended_language: None,
            gmin: None,
            text_header_matrix: None,
            compact_sample_size: false,
            sdtp: Vec::new(),
            stdp: Vec::new(),
            padb: Vec::new(),
            stsh: Vec::new(),
            subs: Vec::new(),
            load: None,
            clipping: None,
            matte: None,
            track_kinds: Vec::new(),
            track_selection: None,
            track_groups: Vec::new(),
            audio_description: AudioDescriptionWrite::V0,
            tkhd_matrix: None,
            tkhd_layer: 0,
            tkhd_alternate_group: 0,
            tkhd_volume: None,
            tkhd_flags: None,
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

    #[test]
    fn saiz_default_size_form_for_uniform_blobs() {
        let aux = SampleAuxStream {
            aux_info_type: Some(*b"cenc"),
            aux_info_type_parameter: 0,
            per_sample: vec![vec![0u8; 16]; 4],
        };
        let body = build_saiz(&aux);
        assert_eq!(body[0], 0); // version 0
        let parsed = crate::sample_aux::parse_saiz(&body).unwrap();
        assert_eq!(parsed.default_sample_info_size, 16);
        assert_eq!(parsed.sample_count, 4);
        assert!(parsed.sample_info_sizes.is_empty());
        let a = parsed.aux_info_type.unwrap();
        assert_eq!(&a.aux_info_type, b"cenc");
        assert_eq!(a.aux_info_type_parameter, 0);
    }

    #[test]
    fn saiz_per_sample_table_for_varying_blobs() {
        let aux = SampleAuxStream {
            aux_info_type: None,
            aux_info_type_parameter: 0,
            per_sample: vec![vec![0u8; 8], vec![], vec![0u8; 24]],
        };
        let body = build_saiz(&aux);
        let parsed = crate::sample_aux::parse_saiz(&body).unwrap();
        assert!(parsed.aux_info_type.is_none()); // flags & 1 unset
        assert_eq!(parsed.default_sample_info_size, 0);
        assert_eq!(parsed.sample_count, 3);
        assert_eq!(parsed.sample_info_sizes, vec![8u8, 0, 24]);
    }

    #[test]
    fn saiz_all_empty_blobs_use_per_sample_zeros() {
        // A uniform-but-zero stream must NOT use the default-size form
        // (default_sample_info_size == 0 is the "table follows"
        // sentinel); it emits an explicit table of zeros.
        let aux = SampleAuxStream {
            aux_info_type: None,
            aux_info_type_parameter: 0,
            per_sample: vec![vec![]; 3],
        };
        let body = build_saiz(&aux);
        let parsed = crate::sample_aux::parse_saiz(&body).unwrap();
        assert_eq!(parsed.default_sample_info_size, 0);
        assert_eq!(parsed.sample_count, 3);
        assert_eq!(parsed.sample_info_sizes, vec![0u8, 0, 0]);
    }

    #[test]
    fn saio_v0_single_entry_for_small_offset() {
        let aux = SampleAuxStream {
            aux_info_type: None,
            aux_info_type_parameter: 0,
            per_sample: vec![],
        };
        let body = build_saio(&aux, 0x1234);
        let parsed = crate::sample_aux::parse_saio(&body).unwrap();
        assert_eq!(parsed.version, 0);
        assert!(parsed.is_single_chunk());
        assert_eq!(parsed.offset_for(0), Some(0x1234));
        assert!(parsed.aux_info_type.is_none());
    }

    #[test]
    fn saio_v1_single_entry_for_large_offset() {
        let big = 0x1_0000_0000u64; // > u32::MAX
        let aux = SampleAuxStream {
            aux_info_type: Some(*b"cenc"),
            aux_info_type_parameter: 3,
            per_sample: vec![],
        };
        let body = build_saio(&aux, big);
        let parsed = crate::sample_aux::parse_saio(&body).unwrap();
        assert_eq!(parsed.version, 1);
        assert!(parsed.is_single_chunk());
        assert_eq!(parsed.offset_for(0), Some(big));
        // The discriminator pair rides along (flags & 1 set).
        let a = parsed.aux_info_type.expect("aux pair");
        assert_eq!(&a.aux_info_type, b"cenc");
        assert_eq!(a.aux_info_type_parameter, 3);
    }

    #[test]
    fn set_sample_aux_rejects_wrong_blob_count() {
        let mut m = MovMuxer::new();
        let id = m.add_track(
            MuxTrackKind::Audio {
                format: *b"sowt",
                channels: 2,
                bits_per_sample: 16,
                sample_rate: 44100,
            },
            44100,
            vec![MuxSample {
                data: vec![1, 2, 3],
                duration: 1024,
                keyframe: true,
                composition_offset: 0,
            }],
            &[],
        );
        let err = m.set_sample_aux(
            id,
            SampleAuxStream {
                aux_info_type: None,
                aux_info_type_parameter: 0,
                per_sample: vec![vec![0u8; 4], vec![0u8; 4]], // 2 blobs, 1 sample
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn set_sample_aux_rejects_oversize_blob() {
        let mut m = MovMuxer::new();
        let id = m.add_track(
            MuxTrackKind::Audio {
                format: *b"sowt",
                channels: 1,
                bits_per_sample: 16,
                sample_rate: 8000,
            },
            8000,
            vec![MuxSample {
                data: vec![1],
                duration: 1,
                keyframe: true,
                composition_offset: 0,
            }],
            &[],
        );
        let err = m.set_sample_aux(
            id,
            SampleAuxStream {
                aux_info_type: None,
                aux_info_type_parameter: 0,
                per_sample: vec![vec![0u8; 256]], // > 255 ⇒ u8 size table overflow
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn set_sample_aux_rejects_unknown_track() {
        let mut m = MovMuxer::new();
        let err = m.set_sample_aux(
            99,
            SampleAuxStream {
                aux_info_type: None,
                aux_info_type_parameter: 0,
                per_sample: vec![],
            },
        );
        assert!(err.is_err());
    }

    /// Encode a 1-track video movie, optionally compressing the movie
    /// resource. Helper for the `cmov` write-side unit tests.
    fn encode_video(compress: bool) -> Vec<u8> {
        let mut m = MovMuxer::new()
            .with_movie_timescale(600)
            .with_compressed_movie_resource(compress);
        m.add_track(
            MuxTrackKind::Video {
                format: *b"mp4v",
                width: 320,
                height: 240,
            },
            30000,
            synth_video_samples(6),
            &[],
        );
        m.encode_to_vec().expect("encode video MOV")
    }

    #[test]
    fn compress_movie_resource_flag_defaults_off() {
        assert!(!MovMuxer::new().compresses_movie_resource());
        assert!(MovMuxer::new()
            .with_compressed_movie_resource(true)
            .compresses_movie_resource());
    }

    #[test]
    fn compressed_write_emits_moov_cmov_dcom_cmvd_tree() {
        let bytes = encode_video(true);
        // The trailing moov wraps a single cmov child carrying dcom +
        // cmvd (QTFF p. 81 Table 2-5). Locate the moov atom and parse
        // its cmov via the crate's own reader.
        let moov_pos = bytes
            .windows(4)
            .position(|w| w == b"moov")
            .expect("moov FourCC present");
        // moov size word precedes the FourCC.
        let size_off = moov_pos - 4;
        let moov_size =
            u32::from_be_bytes(bytes[size_off..size_off + 4].try_into().unwrap()) as usize;
        let moov_body = &bytes[moov_pos + 4..size_off + moov_size];
        // First (only) child of moov is the cmov atom.
        let cmov_size = u32::from_be_bytes(moov_body[0..4].try_into().unwrap()) as usize;
        assert_eq!(&moov_body[4..8], b"cmov");
        let cmov_body = &moov_body[8..cmov_size];
        let cmov = crate::cmov::parse_cmov(cmov_body).expect("parse cmov body");
        assert_eq!(cmov.dcom.algorithm, crate::cmov::DCOM_ALG_ZLIB);
        // The decompressed resource is itself a complete moov atom.
        let resource = cmov.decompress().expect("decompress movie resource");
        assert_eq!(
            &resource[4..8],
            b"moov",
            "decompressed resource is a moov atom"
        );
        assert_eq!(
            resource.len() as u32,
            cmov.cmvd.uncompressed_size,
            "decompressed length equals the cmvd size word"
        );
    }

    #[test]
    fn compressed_resource_decompresses_to_the_plain_moov() {
        let plain = encode_video(false);
        let compressed = encode_video(true);

        // The plain output's trailing moov atom (header + body).
        let plain_moov_pos = plain
            .windows(4)
            .position(|w| w == b"moov")
            .expect("plain moov present");
        let plain_moov = &plain[plain_moov_pos - 4..];

        // Decompress the compressed output's movie resource.
        let cmov_moov_pos = compressed
            .windows(4)
            .position(|w| w == b"moov")
            .expect("compressed moov present");
        let cmov_pos = cmov_moov_pos + 4; // start of cmov atom (size word)
        let cmov_size =
            u32::from_be_bytes(compressed[cmov_pos..cmov_pos + 4].try_into().unwrap()) as usize;
        let cmov_body = &compressed[cmov_pos + 8..cmov_pos + cmov_size];
        let cmov = crate::cmov::parse_cmov(cmov_body).expect("parse cmov");
        let resource = cmov.decompress().expect("decompress");

        assert_eq!(
            resource, plain_moov,
            "decompressed resource is byte-identical to the plain moov atom"
        );
    }

    #[test]
    fn plain_write_emits_no_cmov() {
        let plain = encode_video(false);
        assert!(
            !plain.windows(4).any(|w| w == b"cmov"),
            "plain output must carry no cmov atom"
        );
    }

    // ───────────────────────── csgp builder ─────────────────────────

    /// Expand a built `csgp` back through the read path and assert the
    /// per-sample index assignment is recovered exactly.
    fn assert_csgp_roundtrip(grouping_type: [u8; 4], gtp: Option<u32>, indices: &[u32]) {
        let g = SampleToGroupWrite {
            grouping_type,
            grouping_type_parameter: gtp,
            indices: indices.to_vec(),
        };
        let body = build_csgp(&g);
        let parsed = crate::sample_groups::parse_csgp(&body).expect("parse_csgp(built csgp)");
        assert_eq!(parsed.grouping_type, grouping_type);
        assert_eq!(parsed.grouping_type_parameter, gtp.unwrap_or(0));
        // covered_samples must equal the sample count we encoded.
        assert_eq!(parsed.covered_samples(), indices.len() as u64);
        for (i, &want) in indices.iter().enumerate() {
            assert_eq!(
                parsed.group_index_for_sample(i as u32),
                want,
                "sample {i} index mismatch"
            );
        }
    }

    #[test]
    fn csgp_min_size_code_boundaries() {
        assert_eq!(csgp_min_size_code(0), (0, 4));
        assert_eq!(csgp_min_size_code(15), (0, 4)); // 4-bit max
        assert_eq!(csgp_min_size_code(16), (1, 8));
        assert_eq!(csgp_min_size_code(255), (1, 8)); // 8-bit max
        assert_eq!(csgp_min_size_code(256), (2, 16));
        assert_eq!(csgp_min_size_code(65535), (2, 16)); // 16-bit max
        assert_eq!(csgp_min_size_code(65536), (3, 32));
        assert_eq!(csgp_min_size_code(u32::MAX), (3, 32));
    }

    #[test]
    fn csgp_roundtrips_simple_runs() {
        // Two groups of two samples each: indices [1,1,2,2].
        assert_csgp_roundtrip(*b"roll", None, &[1, 1, 2, 2]);
    }

    #[test]
    fn csgp_roundtrips_alternating_indices() {
        // No coalescible runs — each sample differs from its neighbour.
        assert_csgp_roundtrip(*b"rap ", None, &[1, 0, 2, 0, 3]);
    }

    #[test]
    fn csgp_roundtrips_zero_index_no_group() {
        // Every sample assigned to "no group" (index 0).
        assert_csgp_roundtrip(*b"prol", None, &[0, 0, 0]);
    }

    #[test]
    fn csgp_roundtrips_with_grouping_type_parameter() {
        assert_csgp_roundtrip(*b"seig", Some(0xDEAD_BEEF), &[1, 2, 3, 1, 2, 3]);
    }

    #[test]
    fn csgp_widths_grow_with_value_magnitude() {
        // A long run forces a wider count field; a large index forces a
        // wider index field. Round-trip must still be exact.
        let mut idx = vec![5u32; 300]; // run length 300 > 255 ⇒ 16-bit count
        idx.push(70_000); // index > 65535 ⇒ 32-bit index field
        assert_csgp_roundtrip(*b"roll", None, &idx);

        // Confirm the flags actually encode the wider codes.
        let g = SampleToGroupWrite {
            grouping_type: *b"roll",
            grouping_type_parameter: None,
            indices: idx,
        };
        let body = build_csgp(&g);
        let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
        assert_eq!(flags & 0b11, 3, "index_size_code → 32-bit");
        assert_eq!((flags >> 2) & 0b11, 2, "count_size_code → 16-bit");
        assert_eq!((flags >> 4) & 0b11, 0, "pattern_size_code → 4-bit");
        assert_eq!((flags >> 6) & 1, 0, "gtp present bit clear");
    }

    #[test]
    fn csgp_preserves_fragment_local_msb() {
        // The 0x8000_0000 msb round-trips verbatim (a stbl-scope csgp
        // treats it as part of the index value).
        let raw = crate::sample_groups::CSGP_FRAGMENT_LOCAL_BIT | 7;
        assert_csgp_roundtrip(*b"roll", None, &[raw, raw, 7]);
    }

    #[test]
    fn build_stbl_emits_csgp_when_attached() {
        let mut t = track_with_offsets(&[0, 0, 0, 0]);
        t.sample_to_groups.push((
            SampleToGroupWrite {
                grouping_type: *b"roll",
                grouping_type_parameter: None,
                indices: vec![1, 1, 2, 2],
            },
            SampleGroupBoxForm::Compact,
        ));
        let chunks = [TrackChunk {
            offset: 0x40,
            sample_count: t.samples.len() as u32,
        }];
        let stbl = build_stbl(&t, &chunks, false, None);
        assert!(
            stbl.windows(4).any(|w| w == b"csgp"),
            "stbl must carry a csgp when an assignment is attached"
        );
    }

    #[test]
    fn stsc_run_length_compresses_equal_chunk_runs() {
        // Chunks of 3,3,1,1,1 samples ⇒ two stsc runs: (first_chunk 1,
        // spc 3) and (first_chunk 3, spc 1) — §8.7.4's run-length rule.
        let chunks: Vec<TrackChunk> = [
            (0x30u64, 3u32),
            (0x90, 3),
            (0xf0, 1),
            (0x110, 1),
            (0x130, 1),
        ]
        .iter()
        .map(|&(offset, sample_count)| TrackChunk {
            offset,
            sample_count,
        })
        .collect();
        let p = build_stsc(&chunks);
        let entry_count = u32::from_be_bytes(p[4..8].try_into().unwrap());
        assert_eq!(entry_count, 2);
        let row = |i: usize| -> (u32, u32, u32) {
            let b = &p[8 + i * 12..8 + (i + 1) * 12];
            (
                u32::from_be_bytes(b[0..4].try_into().unwrap()),
                u32::from_be_bytes(b[4..8].try_into().unwrap()),
                u32::from_be_bytes(b[8..12].try_into().unwrap()),
            )
        };
        assert_eq!(row(0), (1, 3, 1));
        assert_eq!(row(1), (3, 1, 1));
    }

    #[test]
    fn stco_and_co64_list_every_chunk_offset_in_order() {
        let chunks: Vec<TrackChunk> = [(0x30u64, 2u32), (0x60, 2), (0xa0, 1)]
            .iter()
            .map(|&(offset, sample_count)| TrackChunk {
                offset,
                sample_count,
            })
            .collect();
        let p = build_stco(&chunks);
        assert_eq!(u32::from_be_bytes(p[4..8].try_into().unwrap()), 3);
        for (i, expect) in [0x30u32, 0x60, 0xa0].iter().enumerate() {
            let got = u32::from_be_bytes(p[8 + i * 4..12 + i * 4].try_into().unwrap());
            assert_eq!(got, *expect);
        }
        let p64 = build_co64(&chunks);
        assert_eq!(u32::from_be_bytes(p64[4..8].try_into().unwrap()), 3);
        for (i, expect) in [0x30u64, 0x60, 0xa0].iter().enumerate() {
            let got = u64::from_be_bytes(p64[8 + i * 8..16 + i * 8].try_into().unwrap());
            assert_eq!(got, *expect);
        }
    }

    #[test]
    fn build_stbl_omits_csgp_when_none() {
        let t = track_with_offsets(&[0, 0, 0, 0]);
        let chunks = [TrackChunk {
            offset: 0x40,
            sample_count: t.samples.len() as u32,
        }];
        let stbl = build_stbl(&t, &chunks, false, None);
        assert!(
            !stbl.windows(4).any(|w| w == b"csgp"),
            "stbl must carry no csgp when no assignment is attached"
        );
    }

    // ── Round 440: external-media authoring ──────────────────────────

    /// n empty-data samples for an external track (timing only; the
    /// bytes live in the external file).
    fn external_samples(n: usize) -> Vec<MuxSample> {
        (0..n)
            .map(|_| MuxSample {
                data: Vec::new(),
                duration: 100,
                keyframe: true,
                composition_offset: 0,
            })
            .collect()
    }

    fn add_external_audio(m: &mut MovMuxer, n: usize) -> u32 {
        m.add_track(
            MuxTrackKind::Audio {
                format: *b"twos",
                channels: 1,
                bits_per_sample: 16,
                sample_rate: 8000,
            },
            8000,
            external_samples(n),
            &[],
        )
    }

    #[test]
    fn coalesce_external_chunks_merges_contiguous_runs() {
        let locs = [
            ExternalSampleLocation { offset: 0, size: 4 },
            ExternalSampleLocation { offset: 4, size: 4 }, // contiguous
            ExternalSampleLocation { offset: 8, size: 2 }, // contiguous
            ExternalSampleLocation {
                offset: 100,
                size: 4,
            }, // gap ⇒ new chunk
            ExternalSampleLocation {
                offset: 104,
                size: 4,
            }, // contiguous
            ExternalSampleLocation {
                offset: 50,
                size: 4,
            }, // backward ⇒ new chunk
        ];
        let chunks = coalesce_external_chunks(&locs);
        assert_eq!(chunks.len(), 3);
        assert_eq!((chunks[0].offset, chunks[0].sample_count), (0, 3));
        assert_eq!((chunks[1].offset, chunks[1].sample_count), (100, 2));
        assert_eq!((chunks[2].offset, chunks[2].sample_count), (50, 1));
    }

    #[test]
    fn set_external_media_requires_dref_table_first() {
        let mut m = MovMuxer::new();
        let a = add_external_audio(&mut m, 2);
        let err = m.set_external_media(
            a,
            1,
            &[
                ExternalSampleLocation { offset: 0, size: 4 },
                ExternalSampleLocation { offset: 4, size: 4 },
            ],
        );
        assert!(err.is_err(), "no data-reference table set yet");
    }

    #[test]
    fn set_external_media_rejects_self_ref_and_out_of_range_indices() {
        let mut m = MovMuxer::new();
        let a = add_external_audio(&mut m, 1);
        m.set_data_references(
            a,
            &[
                DataReferenceWrite::SelfRef,
                DataReferenceWrite::Url("media.bin".into()),
            ],
        )
        .unwrap();
        let locs = [ExternalSampleLocation { offset: 0, size: 4 }];
        assert!(
            m.set_external_media(a, 1, &locs).is_err(),
            "index 1 is the SelfRef"
        );
        assert!(
            m.set_external_media(a, 0, &locs).is_err(),
            "index 0 is out of spec"
        );
        assert!(
            m.set_external_media(a, 3, &locs).is_err(),
            "index 3 dangles"
        );
        assert!(
            m.set_external_media(a, 2, &locs).is_ok(),
            "index 2 is the url entry"
        );
    }

    #[test]
    fn set_external_media_rejects_location_count_mismatch_and_inline_data() {
        let mut m = MovMuxer::new();
        let a = add_external_audio(&mut m, 2);
        m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
            .unwrap();
        assert!(
            m.set_external_media(a, 1, &[ExternalSampleLocation { offset: 0, size: 4 }])
                .is_err(),
            "1 location for 2 samples must error"
        );
        // A track whose samples carry in-file bytes cannot be external.
        let mut m2 = MovMuxer::new();
        let b = m2.add_track(
            MuxTrackKind::Audio {
                format: *b"twos",
                channels: 1,
                bits_per_sample: 16,
                sample_rate: 8000,
            },
            8000,
            vec![MuxSample {
                data: vec![1, 2, 3],
                duration: 100,
                keyframe: true,
                composition_offset: 0,
            }],
            &[],
        );
        m2.set_data_references(b, &[DataReferenceWrite::Url("media.bin".into())])
            .unwrap();
        assert!(
            m2.set_external_media(b, 1, &[ExternalSampleLocation { offset: 0, size: 3 }])
                .is_err(),
            "non-empty sample data must error"
        );
    }

    #[test]
    fn external_track_conflicts_with_sample_aux_both_orders() {
        let aux = SampleAuxStream {
            aux_info_type: None,
            aux_info_type_parameter: 0,
            per_sample: vec![vec![1u8]],
        };
        let locs = [ExternalSampleLocation { offset: 0, size: 4 }];
        // aux first, then external.
        let mut m = MovMuxer::new();
        let a = add_external_audio(&mut m, 1);
        m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
            .unwrap();
        m.set_sample_aux(a, aux.clone()).unwrap();
        assert!(m.set_external_media(a, 1, &locs).is_err());
        // external first, then aux.
        let mut m2 = MovMuxer::new();
        let b = add_external_audio(&mut m2, 1);
        m2.set_data_references(b, &[DataReferenceWrite::Url("media.bin".into())])
            .unwrap();
        m2.set_external_media(b, 1, &locs).unwrap();
        assert!(m2.set_sample_aux(b, aux).is_err());
    }

    #[test]
    fn re_setting_dref_table_revalidates_external_marking() {
        let mut m = MovMuxer::new();
        let a = add_external_audio(&mut m, 1);
        m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
            .unwrap();
        m.set_external_media(a, 1, &[ExternalSampleLocation { offset: 0, size: 4 }])
            .unwrap();
        // Replacing the table so index 1 becomes the SelfRef must fail.
        assert!(m
            .set_data_references(
                a,
                &[
                    DataReferenceWrite::SelfRef,
                    DataReferenceWrite::Url("media.bin".into())
                ]
            )
            .is_err());
        // A replacement keeping index 1 external is fine.
        assert!(m
            .set_data_references(a, &[DataReferenceWrite::Url("other.bin".into())])
            .is_ok());
    }

    #[test]
    fn fragmented_encode_rejects_external_tracks() {
        let mut m = MovMuxer::new().with_fragmentation(FragmentationMode::ByFrameCount(1));
        let a = add_external_audio(&mut m, 1);
        m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
            .unwrap();
        m.set_external_media(a, 1, &[ExternalSampleLocation { offset: 0, size: 4 }])
            .unwrap();
        assert!(m.encode_fragmented_to_vec().is_err());
    }

    #[cfg(feature = "registry")]
    #[test]
    fn external_track_emits_no_mdat_bytes_and_external_offsets() {
        // One external audio track: mdat must be empty, stco must carry
        // the external offsets verbatim, and the sample entry's
        // data_reference_index must point at the url entry.
        let mut m = MovMuxer::new();
        let a = add_external_audio(&mut m, 3);
        m.set_data_references(a, &[DataReferenceWrite::Url("media.bin".into())])
            .unwrap();
        m.set_external_media(
            a,
            1,
            &[
                ExternalSampleLocation {
                    offset: 16,
                    size: 4,
                },
                ExternalSampleLocation {
                    offset: 20,
                    size: 4,
                }, // contiguous
                ExternalSampleLocation {
                    offset: 64,
                    size: 6,
                }, // gap
            ],
        )
        .unwrap();
        let bytes = m.encode_to_vec().expect("encode");
        // The mdat atom must be empty (header only, size 8) — no sample
        // byte of the external track lands in this file.
        let mdat_pos = bytes
            .windows(4)
            .position(|w| w == b"mdat")
            .expect("mdat present");
        let mdat_size = u32::from_be_bytes(bytes[mdat_pos - 4..mdat_pos].try_into().unwrap());
        assert_eq!(mdat_size, 8, "external-only movie must have an empty mdat");
        let mut d = MovDemuxer::open(Box::new(Cursor::new(bytes)) as Box<dyn ReadSeek>)
            .expect("demux own output");
        assert_eq!(d.tracks.len(), 1);
        assert!(d.track_has_external_data(0));
        assert_eq!(
            d.tracks[0].data_references,
            vec![crate::reference::DataReference::Url("media.bin".into())]
        );
        assert_eq!(d.tracks[0].sample_descriptions[0].data_reference_index, 1);
        // Chunk offsets are into the external file: 2 chunks (16, 64).
        let st = &d.tracks[0].sample_table;
        assert_eq!(st.chunk_offsets, vec![16, 64]);
        // Sizes come from the locations (0-based decode-order index).
        assert_eq!(st.sample_size_at(0), Some(4));
        assert_eq!(st.sample_size_at(2), Some(6));
        // Reading the samples without an opener stays a recoverable
        // per-sample Unsupported error.
        for _ in 0..3 {
            let err = d.read_next().expect_err("external sample without opener");
            let msg = format!("{err}");
            assert!(msg.contains("external"), "unexpected error text: {msg}");
        }
        assert!(matches!(d.read_next(), Err(Error::Eof)));
    }
}
