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
use crate::edit::{media_pts_to_movie_pts, resolve_edit_segments, EditList, EditSegment};
use crate::gmhd::Gmhd;
use crate::header::{Hdlr, Mdhd, Tkhd};
use crate::kind::KindEntry;
use crate::matte::Matte;
use crate::media_meta::{
    parse_chan, parse_clap, parse_colr, parse_pasp, Chan, Clap, ColorParameters, Cslg,
    MetaKeyValue, Pasp, Tapt,
};
use crate::reference::DataReference;
use crate::sample_table::{SampleEntry, SampleTable};
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
    /// Anything else (`subt`, `cdsc`, vendor-specific, …).
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
            _ => Self::Other,
        }
    }
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
    /// `gama` — 16.16 fixed-point gamma; `None` when absent.
    pub gamma: Option<u32>,
    /// `pasp` — pixel aspect ratio.
    pub pasp: Option<Pasp>,
    /// `clap` — clean aperture.
    pub clap: Option<Clap>,
    /// `colr` — colour parameters (Apple `nclc` or ISO `nclx`).
    pub colr: Option<ColorParameters>,

    // ─────── Round-2 audio extension atoms ───────
    /// `chan` — Apple Core Audio channel layout (raw fields surfaced).
    pub chan: Option<Chan>,

    // ─────── Round-6 timecode extension ───────
    /// Parsed `tmcd` sample-description body — populated only when the
    /// track's handler is a time-code track (`hdlr.is_timecode()`) and
    /// the entry's format FourCC is `tmcd`. See [`Tmcd`].
    pub tmcd: Option<Tmcd>,
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
}

/// Parse a `stsd` payload: count + N × per-entry record. Layout per
/// QTFF p. 70 figure 2-27.
pub fn parse_stsd(payload: &[u8], hdlr: &Hdlr) -> Result<Vec<SampleDescription>> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: stsd payload < 8 bytes"));
    }
    let _ver_flags = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let n = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let mut out = Vec::with_capacity(n as usize);
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
            entry.width = u16::from_be_bytes([body[24], body[25]]);
            entry.height = u16::from_be_bytes([body[26], body[27]]);
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
        } else if hdlr.is_audio() && body.len() >= 20 {
            // Sound sample description v0 (QTFF p. 100):
            //   ver:2 rev:2 vendor:4 channels:2 sample_size:2
            //   compression_id:2 packet_size:2 sample_rate:4
            // → 20 bytes; v1 adds 16 bytes more (samples_per_packet,
            //   bytes_per_packet, bytes_per_frame, bytes_per_sample).
            let version = u16::from_be_bytes([body[0], body[1]]);
            entry.channels = u16::from_be_bytes([body[8], body[9]]);
            entry.bits_per_sample = u16::from_be_bytes([body[10], body[11]]);
            entry.sample_rate = u32::from_be_bytes([body[16], body[17], body[18], body[19]]) >> 16;
            // Sample rate is 16.16; integer portion lives in the high 16 bits.
            let extra_start = match version {
                0 => 20usize,
                1 if body.len() >= 36 => 36,
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
/// well-known atom-style extensions (`gama`, `pasp`, `clap`, `colr`).
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
    walk_atoms(&buf, |fourcc, payload| {
        if fourcc == b"chan" {
            entry.chan = Some(parse_chan(payload)?);
        }
        Ok(())
    })
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
        // 70-byte video fixed body. width @ offset 24, height @ 26.
        let mut body = vec![0u8; 70];
        body[24..26].copy_from_slice(&1920u16.to_be_bytes());
        body[26..28].copy_from_slice(&1080u16.to_be_bytes());
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
    }
}
