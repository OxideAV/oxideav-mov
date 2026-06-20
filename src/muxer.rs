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
    /// strictly-positive rate. The muxer writes the value verbatim so a
    /// caller may emit a dwell (`0`) or a non-unity rate.
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
    /// 14496-12:2020 §8.9.5). Each entry emits one `csgp`
    /// (CompactSampleToGroupBox); multiple entries (distinct
    /// `grouping_type`s) emit one `csgp` each, in insertion order.
    sample_to_groups: Vec<SampleToGroupWrite>,
    /// Optional edit list (QTFF p. 47 / ISO/IEC 14496-12 §8.6.6). When
    /// non-empty, the muxer emits an `edts > elst` between `tkhd` and
    /// `mdia` inside the track's `trak`. Empty ⇒ no `edts` box (the
    /// implicit "entire media is used" default per QTFF p. 46).
    edits: Vec<MuxEdit>,
    /// Optional track-level user-data items (QTFF pp. 36–38 / ISO/IEC
    /// 14496-12 §8.10.1). When non-empty the muxer emits a `udta` as the
    /// last child of this track's `trak`. Empty ⇒ no `udta` box.
    metadata: Vec<MovMetadata>,
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
            compress_movie_resource: false,
            metadata: Vec::new(),
            apple_metadata: Vec::new(),
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
            edits: Vec::new(),
            metadata: Vec::new(),
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
    /// present **replaces** the prior assignment for that type. The
    /// muxer does not emit the sibling `sgpd` — the caller supplies the
    /// group descriptions through `extra` `stbl` content or a follow-up
    /// API; `csgp` only carries the index mapping.
    pub fn add_sample_to_group(
        &mut self,
        track_id: u32,
        assignment: SampleToGroupWrite,
    ) -> Result<()> {
        let idx = (track_id as usize)
            .checked_sub(1)
            .filter(|&i| i < self.tracks.len())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV muxer: add_sample_to_group unknown track id {track_id}"
                ))
            })?;
        let want = self.tracks[idx].samples.len();
        if assignment.indices.len() != want {
            return Err(Error::invalid(format!(
                "MOV muxer: sample-to-group has {} indices but track {track_id} has {want} samples",
                assignment.indices.len()
            )));
        }
        // Replace-by-grouping_type so a caller correcting an assignment
        // does not leave two `csgp` boxes naming the same `sgpd`.
        if let Some(slot) = self.tracks[idx]
            .sample_to_groups
            .iter_mut()
            .find(|g| g.grouping_type == assignment.grouping_type)
        {
            *slot = assignment;
        } else {
            self.tracks[idx].sample_to_groups.push(assignment);
        }
        Ok(())
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
    /// * `track_duration` must be representable; the only hard limit is
    ///   the version-1 64-bit field, so no overflow check is needed.
    ///
    /// The fragmented write path ([`encode_fragmented_to_vec`]) ignores
    /// edit lists — fMP4 segments carry presentation timing in the
    /// init-segment `moov`; a follow-up can emit the init-`trak` `edts`.
    ///
    /// [`encode_fragmented_to_vec`]: MovMuxer::encode_fragmented_to_vec
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
        self.tracks[idx].edits = edits.to_vec();
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
        //
        //    Per-track mdat layout when a sample-aux stream is attached:
        //    `[sample data…][aux slab…]`, the aux blobs contiguous in
        //    sample order immediately after the track's sample data so a
        //    single-entry `saio` (§8.7.9.3) addresses the whole slab.
        let ftyp_size: u64 = 28;
        let track_sample_bytes =
            |t: &TrackWrite| -> u64 { t.samples.iter().map(|s| s.data.len() as u64).sum::<u64>() };
        let track_aux_bytes = |t: &TrackWrite| -> u64 {
            t.sample_aux
                .as_ref()
                .map(|a| a.per_sample.iter().map(|b| b.len() as u64).sum::<u64>())
                .unwrap_or(0)
        };
        let mdat_body_len: u64 = self
            .tracks
            .iter()
            .map(|t| track_sample_bytes(t) + track_aux_bytes(t))
            .sum();
        let mdat_header_len: u64 = if mdat_body_len + 8 > u32::MAX as u64 {
            16
        } else {
            8
        };
        let mdat_payload_offset = ftyp_size + mdat_header_len;

        // Per-track chunk offset = where the track's sample data
        // begins; the aux slab (if any) starts at
        // `chunk_offset + track_sample_bytes`. Both are absolute file
        // offsets — exactly what `stco`/`co64` and the §8.7.9.3
        // Sample-Table-scope `saio` require.
        let mut chunk_offsets = Vec::with_capacity(self.tracks.len());
        let mut aux_offsets: Vec<Option<u64>> = Vec::with_capacity(self.tracks.len());
        let mut cursor = mdat_payload_offset;
        for t in &self.tracks {
            chunk_offsets.push(cursor);
            cursor += track_sample_bytes(t);
            if t.sample_aux.is_some() {
                aux_offsets.push(Some(cursor));
            } else {
                aux_offsets.push(None);
            }
            cursor += track_aux_bytes(t);
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
            if let Some(aux) = &t.sample_aux {
                for blob in &aux.per_sample {
                    out.extend_from_slice(blob);
                }
            }
        }
        let moov = build_moov(self, &chunk_offsets, need_co64, &aux_offsets);
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
        }
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
    chunk_offsets: &[u64],
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
            chunk_offsets[idx],
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
    aux_offset: Option<u64>,
) -> Vec<u8> {
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(t, track_id, movie_ts));
    // edts > elst between tkhd and mdia (QTFF p. 46, Figure 2-8: the
    // edit atom precedes the media atom inside a track atom).
    if !t.edits.is_empty() {
        push_atom(&mut trak, *b"edts", &build_edts(&t.edits));
    }
    push_atom(
        &mut trak,
        *b"mdia",
        &build_mdia(t, chunk_offset, need_co64, aux_offset),
    );
    // Track-level user-data box as the last child of `trak` (QTFF
    // p. 41, Figure 2-3: `udta` is the trailing track-atom child).
    if !t.metadata.is_empty() {
        push_atom(&mut trak, *b"udta", &build_udta(&t.metadata));
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

fn build_mdia(
    t: &TrackWrite,
    chunk_offset: u64,
    need_co64: bool,
    aux_offset: Option<u64>,
) -> Vec<u8> {
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(t));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(t));
    push_atom(
        &mut mdia,
        *b"minf",
        &build_minf(t, chunk_offset, need_co64, aux_offset),
    );
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

fn build_minf(
    t: &TrackWrite,
    chunk_offset: u64,
    need_co64: bool,
    aux_offset: Option<u64>,
) -> Vec<u8> {
    let mut minf = Vec::new();
    match &t.kind {
        MuxTrackKind::Video { .. } => push_atom(&mut minf, *b"vmhd", &build_vmhd()),
        MuxTrackKind::Audio { .. } => push_atom(&mut minf, *b"smhd", &build_smhd()),
    }
    push_atom(&mut minf, *b"dinf", &build_dinf());
    push_atom(
        &mut minf,
        *b"stbl",
        &build_stbl(t, chunk_offset, need_co64, aux_offset),
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

fn build_stbl(
    t: &TrackWrite,
    chunk_offset: u64,
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
    // Sample-auxiliary-information pair (ISO/IEC 14496-12 §8.7.8 /
    // §8.7.9). Emitted only when the track carries an aux stream; the
    // slab was laid into mdat starting at `aux_offset`.
    if let (Some(aux), Some(off)) = (&t.sample_aux, aux_offset) {
        push_atom(&mut stbl, *b"saiz", &build_saiz(aux));
        push_atom(&mut stbl, *b"saio", &build_saio(aux, off));
    }
    // Compact Sample to Group Box(es) (ISO/IEC 14496-12:2020 §8.9.5) —
    // one `csgp` per attached grouping_type, after the sample-aux pair.
    for g in &t.sample_to_groups {
        push_atom(&mut stbl, *b"csgp", &build_csgp(g));
    }
    stbl
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
    p[36..40].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    p[52..56].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    p[68..72].copy_from_slice(&0x4000_0000u32.to_be_bytes());
    p[96..100].copy_from_slice(&((m.tracks.len() as u32) + 1).to_be_bytes());
    p
}

/// Build an init-segment `trak`. Same shape as `build_trak` but the
/// `stbl` body is empty per ISO BMFF §8.8.4 (no in-moov samples).
fn build_init_trak(t: &TrackWrite, track_id: u32, _movie_ts: u32) -> Vec<u8> {
    let mut trak = Vec::new();
    // tkhd: declare duration = 0 (set by fragments).
    push_atom(&mut trak, *b"tkhd", &build_init_tkhd(t, track_id));
    push_atom(&mut trak, *b"mdia", &build_init_mdia(t));
    trak
}

fn build_init_tkhd(t: &TrackWrite, track_id: u32) -> Vec<u8> {
    let mut p = vec![0u8; 84];
    p[3] = 0x07; // flags
    p[12..16].copy_from_slice(&track_id.to_be_bytes());
    // duration = 0 for fragmented init segments.
    p[20..24].copy_from_slice(&0u32.to_be_bytes());
    if matches!(t.kind, MuxTrackKind::Audio { .. }) {
        p[36..38].copy_from_slice(&0x0100i16.to_be_bytes());
    }
    p[40..44].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    p[56..60].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    p[72..76].copy_from_slice(&0x4000_0000u32.to_be_bytes());
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

fn build_init_mdia(t: &TrackWrite) -> Vec<u8> {
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_init_mdhd(t));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(t));
    push_atom(&mut mdia, *b"minf", &build_init_minf(t));
    mdia
}

fn build_init_mdhd(t: &TrackWrite) -> Vec<u8> {
    let mut p = vec![0u8; 24];
    p[12..16].copy_from_slice(&t.media_timescale.to_be_bytes());
    // duration = 0 for fragmented init segments.
    p[16..20].copy_from_slice(&0u32.to_be_bytes());
    p[20..22].copy_from_slice(&0x55C4u16.to_be_bytes());
    p
}

fn build_init_minf(t: &TrackWrite) -> Vec<u8> {
    let mut minf = Vec::new();
    match &t.kind {
        MuxTrackKind::Video { .. } => push_atom(&mut minf, *b"vmhd", &build_vmhd()),
        MuxTrackKind::Audio { .. } => push_atom(&mut minf, *b"smhd", &build_smhd()),
    }
    push_atom(&mut minf, *b"dinf", &build_dinf());
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
        MuxTrackKind::Audio { .. } => 0u32,           // sync default
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
            edits: Vec::new(),
            metadata: Vec::new(),
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
            edits: Vec::new(),
            metadata: Vec::new(),
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
            edits: Vec::new(),
            metadata: Vec::new(),
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
            edits: Vec::new(),
            metadata: Vec::new(),
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
            edits: Vec::new(),
            metadata: Vec::new(),
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
            edits: Vec::new(),
            metadata: Vec::new(),
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
        t.sample_to_groups.push(SampleToGroupWrite {
            grouping_type: *b"roll",
            grouping_type_parameter: None,
            indices: vec![1, 1, 2, 2],
        });
        let stbl = build_stbl(&t, 0x40, false, None);
        assert!(
            stbl.windows(4).any(|w| w == b"csgp"),
            "stbl must carry a csgp when an assignment is attached"
        );
    }

    #[test]
    fn build_stbl_omits_csgp_when_none() {
        let t = track_with_offsets(&[0, 0, 0, 0]);
        let stbl = build_stbl(&t, 0x40, false, None);
        assert!(
            !stbl.windows(4).any(|w| w == b"csgp"),
            "stbl must carry no csgp when no assignment is attached"
        );
    }
}
