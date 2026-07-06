//! Public demuxer entry point.
//!
//! Walks the QTFF box hierarchy once, builds per-track sample tables,
//! then emits packets one at a time in interleaved file-offset order
//! (round-robins across tracks the way QuickTime stores chunks). The
//! interleave choice is a behavioural decision the round-1 spec
//! constrains only loosely; we sort sample entries across all tracks
//! by file offset before emitting, which yields a globally
//! monotonically-increasing read pattern friendly to disk and
//! mmap-backed inputs.

use std::io::{Cursor, Read, Seek, SeekFrom};

use crate::atom::{
    read_atom_header, read_payload, walk_children, AtomHeader, CLEF, CLIP, CMOV, CO64, CSGP, CSLG,
    CTAB, CTTS, DINF, DREF, EDTS, ELNG, ELST, ENOF, FREE, FTYP, GMHD, GMIN, HDLR, HMHD, ILST, IMAP,
    KEYS, LOAD, MATT, MAX_INMEMORY_ATOM_BODY, MDAT, MDHD, MDIA, META, MFRA, MINF, MOOF, MOOV, MVEX,
    MVHD, NMHD, PADB, PDIN, PNOT, PRFT, PROF, RDRF, RMCD, RMCS, RMDA, RMDR, RMQU, RMRA, RMVC, SAIO,
    SAIZ, SBGP, SDTP, SGPD, SIDX, SKIP, SMHD, SSIX, STBL, STCO, STDP, STHD, STSC, STSD, STSH, STSS,
    STSZ, STTS, STYP, STZ2, SUBS, TAPT, TEXT, TKHD, TMCD, TRAK, TREF, TRGR, UDTA, UUID, VMHD, WIDE,
};
use crate::bmff_meta::{parse_bmff_meta, BmffMeta};
use crate::chapter::{decode_text_sample_full, ChapterEntry, ChapterList};
use crate::clip::{parse_clip, Clipping};
use crate::cmov::parse_cmov;
use crate::ctab::{parse_ctab, Ctab};
use crate::edit::{parse_elst, EditList};
use crate::fragment::{parse_mfra, parse_mvex, resolve_traf_samples, Mehd, Tfra, TrexDefaults};
use crate::gmhd::{parse_gmin, parse_tcmi, parse_text_header, Gmhd};
use crate::header::{
    parse_elng, parse_ftyp, parse_hdlr, parse_hmhd, parse_mdhd, parse_mvhd, parse_tkhd, Ftyp,
    MediaHeaderKind, Mvhd,
};
use crate::leva::Leva;
use crate::matte::parse_matt;
use crate::media_meta::{parse_cslg, parse_ilst, parse_keys, parse_tapt_dims, MetaKeyValue, Tapt};
use crate::pdin::{parse_pdin, Pdin};
use crate::pnot::{parse_pnot, Pnot};
use crate::prft::{parse_prft, Prft};
use crate::reference::{parse_dref, parse_rdrf, ReferenceMovie};
use crate::sample_aux::{parse_saio, parse_saiz};
use crate::sample_groups::{parse_csgp, parse_sbgp, parse_sgpd};
use crate::sample_table::{
    parse_co64, parse_ctts, parse_padb, parse_sdtp, parse_stco, parse_stdp, parse_stsc, parse_stsh,
    parse_stss, parse_stsz, parse_stts, parse_stz2, parse_subs, SampleEntry, SampleSizeSource,
    SampleTable, SubSampleInfo,
};
use crate::sidx::{parse_sidx, Sidx};
use crate::ssix::{parse_ssix, Ssix};
use crate::styp::{parse_styp, Styp};
use crate::timecode::{StartTimecode, TimecodeSample};
use crate::track::{parse_stsd, Track, TrackRef, TrackRefKind};
use crate::track_load::parse_load;
use crate::user_data::{parse_udta, UserDataEntry};
use crate::uuid::{parse_uuid, Uuid};

#[cfg(feature = "registry")]
use oxideav_core::{
    CodecId, CodecParameters, CodecResolver, CodecTag, Demuxer, Error, NullCodecResolver, Packet,
    ProbeContext, ReadSeek, Result, StreamInfo, TimeBase,
};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, ReadSeek, Result};

/// Maximum number of `rmra/url ` alias hops [`MovDemuxer::open_with_aliases`]
/// will follow before refusing the open. The cap matches the widely-used
/// QuickTime player heuristic (chains of more than ~4 hops indicate
/// either an authoring bug or a deliberate denial-of-service shape) and
/// is paired with a visited-URL set so cycles abort even before the
/// depth limit is reached.
pub const MAX_ALIAS_DEPTH: usize = 4;

/// Round-1 demuxer. Lifetime is bounded by the input reader; on
/// `open` we walk `moov` once and cache enough state to stream
/// packets without reseeking the index.
pub struct MovDemuxer {
    input: Box<dyn ReadSeek>,
    pub ftyp: Option<Ftyp>,
    pub mvhd: Option<Mvhd>,
    pub tracks: Vec<Track>,
    /// Movie-level Apple `meta` key-value pairs (when the file
    /// carries an Apple-shaped `meta` atom at moov scope).
    pub meta: Vec<MetaKeyValue>,
    /// Movie-level `udta` user-data entries (©nam, ©cpy, name, …) at
    /// `moov/udta` scope. Track-level `udta` is exposed through
    /// [`Track::user_data`].
    pub user_data: Vec<UserDataEntry>,
    /// Apple "reference movies" parsed from the optional `moov/rmra`
    /// container. When non-empty AND the file lacks an in-file `mdat`,
    /// `open()` fails with an `Unsupported` error; otherwise we keep
    /// the parsed alias list around so callers that treat `rmra` as
    /// purely informational can still inspect it.
    pub reference_movies: Vec<ReferenceMovie>,
    /// `dcom` compression-algorithm FourCC when the input stored its
    /// movie resource compressed (QTFF pp. 80 – 81: the top-level
    /// `moov` carried a single `cmov` child, which `open` decompressed
    /// and re-parsed transparently — every other field on this struct
    /// reflects the *decompressed* movie resource). `None` for the
    /// common uncompressed layout. The only FourCC the decompressor
    /// implements is the conventional `'zlib'`
    /// ([`crate::cmov::DCOM_ALG_ZLIB`]); any other value fails the
    /// open with a spec-citing error rather than landing here.
    pub compressed_movie_algorithm: Option<[u8; 4]>,
    /// Movie-level ISO BMFF §8.11 `meta` box, when the file's
    /// `moov/meta` is in the ISO/IEC 14496-12 (HEIF / MIAF / MPEG-7)
    /// shape rather than the Apple key-value shape (which lives in
    /// [`Self::meta`]). The two are mutually exclusive: a single
    /// `meta` atom can only be one shape at a time.
    pub bmff_meta: Option<BmffMeta>,
    /// File-level ISO BMFF §8.11 `meta` box, when the input's top
    /// level carries a `meta` atom (typical for HEIF / MIAF / AVIF /
    /// JPEG-XL still images). Independent of any `moov/meta`.
    pub file_bmff_meta: Option<BmffMeta>,
    /// True iff the first non-skip top-level atom after `ftyp` is
    /// `moov`, indicating the file is laid out for streaming
    /// ("faststart").
    faststart: bool,
    /// Progressive Download Information Box (ISO/IEC 14496-12 §8.1.3)
    /// when the file's top-level carries a `pdin`. `None` for QTFF and
    /// for any ISO BMFF file that omits this optional box. Spec
    /// recommends `pdin` appear as early as possible in the file for
    /// maximum utility (§8.1.3.1).
    pub pdin: Option<Pdin>,
    /// Top-level Segment Index Boxes (ISO/IEC 14496-12 §8.16.3), in
    /// file order. Each `sidx` indexes one media stream's subsegments
    /// for adaptive-streaming (DASH / CMAF) random access. A media
    /// segment may carry several (one per indexed stream, plus nested
    /// `sidx`-of-`sidx` references); the box has `Quantity: Zero or
    /// more`. Empty for QTFF and for non-segmented MP4s.
    pub sidx: Vec<Sidx>,
    /// Top-level Subsegment Index Boxes (ISO/IEC 14496-12 §8.16.4),
    /// in file order. Each `ssix` pairs one-to-one with the
    /// immediately preceding `sidx` (the spec sets `Quantity: 0 or 1`
    /// per associated leaf-indexing `sidx`, §8.16.4.1) and partitions
    /// every subsegment into level-keyed partial subsegments. Use
    /// [`Self::ssix_for_sidx`] to resolve which `ssix` (if any)
    /// describes a given `sidx`. Empty for QTFF and for any
    /// non-segmented or coarsely-segmented MP4 / fMP4.
    pub ssix: Vec<Ssix>,
    /// Parallel-to-`sidx` index map into `ssix`: when entry `i` is
    /// `Some(j)`, `sidx[i]` is described by `ssix[j]` per §8.16.4.1.
    /// When `None`, no `ssix` immediately follows that `sidx`.
    /// Orphan `ssix` entries (out-of-order, not following any sidx)
    /// are tail-appended to `ssix` but are not referenced from this
    /// map — they're surfaced for caller diagnostics only.
    ssix_for_sidx: Vec<Option<usize>>,
    /// Top-level Segment Type Boxes (ISO/IEC 14496-12 §8.16.2), in
    /// file order. The first entry — when present — is the conformance
    /// declaration for a DASH / CMAF / HLS-fMP4 media segment; spec
    /// §8.16.2.1 says any `styp` that isn't first in its file "may be
    /// ignored", but we preserve them all so callers building a
    /// diagnostic view of a concatenated segment stream don't lose
    /// information. Empty for QTFF and for non-segmented MP4s.
    pub styp: Vec<Styp>,
    /// Movie-level Color Table atom (QTFF p. 35), when the file's
    /// `moov` carries an optional `ctab` declaring a preferred
    /// indexed-color palette. Up to 256 4-channel (reserved/r/g/b)
    /// 16-bit entries. `None` for any file that omits this Apple-only
    /// atom (the typical case — ISO BMFF / fMP4 / HEIF / AVIF do not
    /// define `ctab`).
    pub ctab: Option<Ctab>,
    /// Movie-level Clipping atom (QTFF p. 43), when the file's `moov`
    /// carries an optional `clip` declaring a spatial mask for the
    /// movie as a whole. The wrapper contains a single `crgn` child
    /// (QTFF p. 44) whose QuickDraw region surfaces here. `None` for
    /// any file that omits this Apple-only atom (ISO BMFF does not
    /// define `clip`); per-track clipping (when present) surfaces
    /// through [`Track::clipping`] instead.
    pub clipping: Option<Clipping>,
    /// Top-level Producer Reference Time Boxes (ISO/IEC 14496-12
    /// §8.16.5), in file order. Each `prft` records the writer's UTC
    /// wall-clock instant (NTP format) at which the *next* movie
    /// fragment in bitstream order was produced (§8.16.5.1), paired with
    /// the corresponding media time on a reference track. `Quantity:
    /// Zero or more` — live DASH-LL / CMAF / HLS-fMP4 encoders emit one
    /// `prft` per fragment so consumers can derive producer-consumer
    /// rate alignment. Empty for QTFF, for non-segmented MP4s, and for
    /// non-live segmented streams.
    pub prft: Vec<Prft>,
    /// Top-level Preview atom (`pnot`) — QTFF p. 26 / Figure 1-7. An
    /// optional preflight pointer at a poster image (typically a
    /// top-level `PICT` atom) used by Finder / Open dialogs to render
    /// a representative thumbnail without instantiating the codec
    /// pipeline. `Quantity: 0 or 1` by convention; the parser keeps
    /// the first when a writer emits duplicates (matching the
    /// conservative-merge convention shared with `pdin` / `ctab` /
    /// `clip` / `mvhd`). ISO BMFF does not define this atom; an MP4
    /// / fMP4 / HEIF / AVIF file will not carry one and this field
    /// stays `None`.
    pub pnot: Option<Pnot>,
    /// Top-level User-Type Boxes (`uuid`) — ISO/IEC 14496-12 §4.2 /
    /// §11.1. The spec's escape hatch for vendor-specific extensions:
    /// every entry's `usertype` is a 16-byte UUID identifying the
    /// vendor schema, and `payload` is the opaque body. `Quantity:
    /// Zero or more` — a single `.mov` / `.mp4` may carry several
    /// (one per vendor extension, with overlap permitted), so the
    /// parser collects them in declaration order and leaves
    /// disambiguation to the caller. Well-known carriers include the
    /// PIFF tfxd / tfrf live-DASH timing extensions, Sony XAVC
    /// per-clip metadata, and GoPro GPMF telemetry. QTFF does not
    /// define `uuid` at spec level; in practice it routinely appears
    /// in vendor `.mov` files alongside the QuickTime atoms.
    pub file_uuids: Vec<Uuid>,
    /// Pre-flattened sample queue, sorted by file offset for friendly
    /// I/O patterns. Each entry is `(stream_index, sample)`.
    samples: Vec<(u32, SampleEntry)>,
    /// Cursor into `samples` for the next packet to emit.
    next: usize,
    /// When `true`, `next_packet()` applies each track's edit list to
    /// packet timing (QTFF pp. 46–48 / ISO/IEC 14496-12 §8.6.6):
    /// samples outside every edit are dropped and emitted timestamps
    /// live on the edited (presentation) timeline. Opt in via
    /// [`MovDemuxer::apply_edit_lists`]; the default (`false`)
    /// preserves the historical media-timeline packet contract.
    apply_edits: bool,
    /// Per-track resolved edit segments, cached when
    /// [`MovDemuxer::apply_edit_lists`] enables the edited timeline
    /// (parallel to `tracks`; empty while the mode is off).
    edited_segments: Vec<Vec<crate::EditSegment>>,
    /// Per-track `trex` defaults from `moov/mvex` (ISO/IEC 14496-12
    /// §8.8.3). Empty for non-fragmented streams. Round 18 surfaces
    /// the parsed records so callers can inspect the per-track
    /// fragment defaults; the demuxer itself uses them while
    /// resolving `moof/traf/trun` samples.
    pub trex_defaults: Vec<TrexDefaults>,
    /// `mvex/mehd` total fragmented duration in `mvhd.time_scale`
    /// ticks (§8.8.2). `None` when the file omits `mehd` — in
    /// which case the duration is the sum across all `moof`s.
    pub mehd: Option<Mehd>,
    /// `mvex/leva` Level Assignment Box (ISO/IEC 14496-12 §8.8.13).
    /// `None` when the file omits `leva` (the common case — the box
    /// is `Quantity: Zero or one` and only fragmented streams
    /// emitting paired `ssix` indices typically carry it). When
    /// present, the box names the levels the §8.16.4 Subsegment Index
    /// Box (`ssix`) references; pair via [`Self::ssix`] and
    /// [`Self::ssix_for_sidx`] for the adaptive-streaming workflow.
    /// QTFF does not define this box; it stays absent for plain
    /// `.mov` inputs.
    pub leva: Option<Leva>,
    /// `mfhd.sequence_number` of each `moof` walked at open time,
    /// in declaration order. Lets callers spot dropped fragments
    /// (the spec requires monotonic increase per §8.8.5.3); empty
    /// for non-fragmented streams.
    pub fragment_sequence_numbers: Vec<u32>,
    /// Parsed `mfra/tfra` rows (ISO/IEC 14496-12 §8.8.10), one entry
    /// per track that ships a Movie-Fragment Random Access index.
    /// Populated at open time by [`MovDemuxer::open_with`] from the
    /// tail `mfra` box; empty when the file is not fragmented or the
    /// optional `mfra` is absent. Drives the fragmented-seek path in
    /// [`MovDemuxer::seek_to_impl`] (§8.8.10.3).
    pub tfra_indexes: Vec<Tfra>,
    #[cfg(feature = "registry")]
    streams: Vec<StreamInfo>,
}

impl MovDemuxer {
    /// Parse the container header and build the sample-table index.
    pub fn open(mut input: Box<dyn ReadSeek>) -> Result<Self> {
        Self::open_with_resolver_inner(&mut input, None)?;
        // We re-seek to the start, then walk fresh. The internal
        // helper uses the fully-mutable `input` — easiest is to
        // delegate to the resolver-aware ctor with a null resolver.
        Self::open_with(input, &NULL_RESOLVER)
    }

    /// Open a QuickTime file, transparently following any `rmra/url `
    /// alias hops when the input file is a *reference movie*: a `.mov`
    /// whose `moov` carries only an `rmra` list and no inline tracks
    /// (QTFF "Reference Movies" §). The `opener` callback is invoked
    /// with each `url ` alias in order; the first URL it can open is
    /// re-parsed as a regular QuickTime file. If that resolved target
    /// is itself another reference movie, the resolver continues
    /// chasing the chain up to [`MAX_ALIAS_DEPTH`] hops, with a
    /// visited-URL set to detect cycles.
    ///
    /// Non-`url ` data references (`alis` / `rsrc`) are skipped — the
    /// opener never receives them. Returns `Unsupported` when:
    ///   * no alias has a usable `url ` reference, or
    ///   * the opener errors on every URL it sees, or
    ///   * the chain exceeds [`MAX_ALIAS_DEPTH`] hops, or
    ///   * the chain forms a cycle (a URL is revisited).
    ///
    /// `opener` returns its own error type via [`std::io::Error`]; an
    /// I/O failure on a single URL is treated the same as "URL not
    /// reachable" — the resolver moves on to the next alternate
    /// rather than fail the whole open.
    pub fn open_with_aliases<F>(input: Box<dyn ReadSeek>, opener: F) -> Result<Self>
    where
        F: FnMut(&str) -> std::io::Result<Box<dyn ReadSeek>>,
    {
        Self::open_with_aliases_resolver(input, opener, &NULL_RESOLVER)
    }

    /// Same as [`open_with_aliases`] but additionally takes a
    /// [`CodecResolverShim`] applied to the resolved alias target.
    pub fn open_with_aliases_resolver<F>(
        mut input: Box<dyn ReadSeek>,
        mut opener: F,
        resolver: &dyn CodecResolverShim,
    ) -> Result<Self>
    where
        F: FnMut(&str) -> std::io::Result<Box<dyn ReadSeek>>,
    {
        // Try to parse the input directly. The common case is a
        // self-contained file with an inline track — opening succeeds
        // immediately and we never touch the opener.
        input.seek(SeekFrom::Start(0))?;
        let refs = match Self::probe_reference_movies(input.as_mut()) {
            Ok(v) => v,
            Err(e) => {
                // The input is not even a recognisable QuickTime
                // container; bubble up the error as-is.
                return Err(e);
            }
        };
        // Fast path: there are tracks (or we couldn't tell) — let the
        // regular ctor handle it. We discriminate by attempting the
        // open() call; a reference-only file will surface Unsupported
        // and we recover by walking aliases.
        input.seek(SeekFrom::Start(0))?;
        match Self::open_with(input, resolver) {
            Ok(d) => Ok(d),
            Err(_e) if !refs.is_empty() => {
                // Multi-hop walk with a visited-URL set so cycles abort.
                let mut visited: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let mut depth = 0usize;
                let mut current_refs = refs;
                loop {
                    if depth >= MAX_ALIAS_DEPTH {
                        return Err(unsupported_error(format!(
                            "MOV: alias chain exceeds MAX_ALIAS_DEPTH={MAX_ALIAS_DEPTH}"
                        )));
                    }
                    // Try the alternates in order; first reachable URL wins.
                    let mut next_input: Option<Box<dyn ReadSeek>> = None;
                    let mut tried = 0usize;
                    let mut last_url: Option<String> = None;
                    for r in &current_refs {
                        let url = match r.data_ref.as_ref() {
                            Some(crate::reference::DataReference::Url(s)) => s.clone(),
                            _ => continue,
                        };
                        tried += 1;
                        if visited.contains(&url) {
                            return Err(unsupported_error(format!(
                                "MOV: alias chain cycle detected (revisit of '{url}')"
                            )));
                        }
                        match opener(url.as_str()) {
                            Ok(b) => {
                                visited.insert(url.clone());
                                last_url = Some(url);
                                next_input = Some(b);
                                break;
                            }
                            Err(_) => continue,
                        }
                    }
                    let mut nxt = match next_input {
                        Some(b) => b,
                        None => {
                            return Err(unsupported_error(format!(
                                "MOV: alias chain exhausted ({tried} alternate(s) tried, none \
                                 reachable)"
                            )));
                        }
                    };
                    // Probe the resolved target.
                    nxt.seek(SeekFrom::Start(0))?;
                    let nxt_refs = Self::probe_reference_movies(nxt.as_mut())?;
                    nxt.seek(SeekFrom::Start(0))?;
                    match Self::open_with(nxt, resolver) {
                        Ok(d) => return Ok(d),
                        Err(_e) if !nxt_refs.is_empty() => {
                            // Another reference-movie hop — descend.
                            depth += 1;
                            current_refs = nxt_refs;
                            let _ = last_url; // already added to visited
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Walk the input's top-level atoms looking for `moov/rmra` and
    /// return the parsed reference-movie list. Returns an empty vec
    /// when no `rmra` is present (the common case). Used by
    /// [`open_with_aliases`] to discover alternates without committing
    /// to a full demuxer construction. The reader's cursor is reset to
    /// the start on entry; on exit the cursor position is unspecified.
    pub fn probe_reference_movies(input: &mut dyn ReadSeek) -> Result<Vec<ReferenceMovie>> {
        input.seek(SeekFrom::Start(0))?;
        let total_len = input.seek(SeekFrom::End(0))?;
        input.seek(SeekFrom::Start(0))?;
        let mut refs = Vec::new();
        loop {
            let pos = input.stream_position()?;
            if pos >= total_len {
                break;
            }
            let hdr = match read_atom_header(input)? {
                Some(h) => h,
                None => break,
            };
            let body_end = hdr
                .total_size
                .map(|t| hdr.payload_offset + (t - hdr.header_len))
                .unwrap_or(total_len);
            // Same injection-robustness guard as `open_with` — a
            // forged top-level `size` past EOF is rejected before any
            // further parsing or allocation.
            if body_end > total_len {
                return Err(Error::invalid(format!(
                    "MOV: top-level atom '{}' extends to byte {body_end} past end-of-file {total_len}",
                    hdr.type_str(),
                )));
            }
            if hdr.fourcc == MOOV {
                input.seek(SeekFrom::Start(hdr.payload_offset))?;
                walk_children(input, Some(body_end), |r, child| {
                    if child.fourcc == RMRA {
                        refs = parse_rmra(r, child)?;
                    } else if child.fourcc == CMOV {
                        // QTFF pp. 80 – 81: a compressed movie
                        // resource may itself be a reference movie —
                        // decompress and scan the inner moov for the
                        // `rmra` so alias discovery sees through the
                        // compression layer.
                        let body = read_payload(r, child)?;
                        let (_, decoded, inner_hdr) = decompress_cmov_body(&body)?;
                        let inner_end = inner_hdr
                            .total_size
                            .map(|t| inner_hdr.payload_offset + (t - inner_hdr.header_len))
                            .unwrap_or(decoded.len() as u64);
                        let mut cur = Cursor::new(decoded);
                        cur.seek(SeekFrom::Start(inner_hdr.payload_offset))?;
                        walk_children(&mut cur, Some(inner_end), |r2, c2| {
                            if c2.fourcc == RMRA {
                                refs = parse_rmra(r2, c2)?;
                            }
                            Ok(())
                        })?;
                    }
                    Ok(())
                })?;
                break; // moov walked; no need to continue scanning.
            }
            input.seek(SeekFrom::Start(body_end))?;
        }
        Ok(refs)
    }

    /// Parse the container, using `resolver` to map sample-description
    /// FourCCs to oxideav `CodecId`s.
    pub fn open_with(
        mut input: Box<dyn ReadSeek>,
        resolver: &dyn CodecResolverShim,
    ) -> Result<Self> {
        input.seek(SeekFrom::Start(0))?;
        let total_len = input.seek(SeekFrom::End(0))?;
        input.seek(SeekFrom::Start(0))?;

        let mut ftyp: Option<Ftyp> = None;
        let mut mvhd: Option<Mvhd> = None;
        let mut tracks: Vec<Track> = Vec::new();
        let mut movie_meta: Vec<MetaKeyValue> = Vec::new();
        let mut movie_user_data: Vec<UserDataEntry> = Vec::new();
        let mut reference_movies: Vec<ReferenceMovie> = Vec::new();
        // QTFF pp. 80 – 81 compressed movie resource: records the
        // `cmov/dcom` algorithm FourCC when the `moov` walk found (and
        // transparently decompressed) a compressed movie resource.
        let mut compressed_movie_algorithm: Option<[u8; 4]> = None;
        let mut movie_bmff_meta: Option<BmffMeta> = None;
        let mut file_bmff_meta: Option<BmffMeta> = None;
        let mut mehd_box: Option<Mehd> = None;
        let mut trex_defaults: Vec<TrexDefaults> = Vec::new();
        // ISO/IEC 14496-12 §8.8.13 Level Assignment Box (`leva`).
        // FullBox inside `mvex`; `Quantity: Zero or one`. Pairs with
        // §8.16.4 `ssix` for adaptive-streaming level selection.
        // QTFF does not define this box; it stays `None` for `.mov`
        // inputs and non-fragmented MP4s.
        let mut leva_box: Option<Leva> = None;
        let mut fragment_sequence_numbers: Vec<u32> = Vec::new();
        let mut tfra_indexes: Vec<Tfra> = Vec::new();
        // ISO/IEC 14496-12 §8.1.3 Progressive Download Information
        // Box (`pdin`). File-level, optional, at most one per file.
        // QTFF doesn't define this box; it stays `None` for `.mov`
        // inputs and most legacy MP4s.
        let mut file_pdin: Option<Pdin> = None;
        // ISO/IEC 14496-12 §8.16.3 Segment Index Boxes (`sidx`).
        // File-level, "Zero or more" — collected in file order. QTFF
        // doesn't define this box; it stays empty for `.mov` inputs.
        let mut file_sidx: Vec<Sidx> = Vec::new();
        // ISO/IEC 14496-12 §8.16.4 Subsegment Index Boxes (`ssix`).
        // File-level, "Zero or one per associated leaf-indexing sidx"
        // (§8.16.4.1). Stored as a parallel vector to `file_sidx`: at
        // each `sidx` we push a `None`, then if the very next box is
        // `ssix` we replace that slot with `Some(parsed)`. This
        // preserves the §8.16.4.1 pairing rule ("the next box after
        // the associated Segment Index box") without storing a
        // separate index map. We then unzip the slot vector into
        // `file_ssix: Vec<Ssix>` (declaration order) plus an internal
        // index map for `ssix_for_sidx`. QTFF doesn't define this box;
        // both stay empty for `.mov` inputs.
        let mut file_ssix_slots: Vec<Option<Ssix>> = Vec::new();
        // Orphan `ssix` boxes — those that do not immediately follow a
        // `sidx` per §8.16.4.1. Still parsed and surfaced through the
        // public Vec (declaration order is preserved by tail-append),
        // but no `sidx` cross-reference is recorded for them.
        let mut file_ssix_orphans: Vec<Ssix> = Vec::new();
        // Track whether the previous top-level box was `sidx`; only
        // then does §8.16.4.1 permit an `ssix` to bind to it.
        let mut prev_was_sidx = false;
        // ISO/IEC 14496-12 §8.16.2 Segment Type Boxes (`styp`).
        // File-level, "Zero or more" — collected in file order so a
        // caller can inspect a concatenated segment stream's every
        // boundary marker (§8.16.2.1 says any `styp` not first in its
        // file "may be ignored", but we preserve them for diagnostics).
        // QTFF doesn't define this box; it stays empty for `.mov`
        // inputs and non-segmented MP4s.
        let mut file_styp: Vec<Styp> = Vec::new();
        // ISO/IEC 14496-12 §8.16.5 Producer Reference Time Boxes
        // (`prft`). File-level, "Zero or more"; live encoders emit one
        // before each `moof` so the wall-clock-to-media-time pairing
        // travels alongside the media data. Collected in file order so a
        // caller can step through every producer marker.
        let mut file_prft: Vec<Prft> = Vec::new();
        // QTFF p. 26 Preview atom (`pnot`). File-level, optional, at
        // most one by convention. Points at a poster image (typically a
        // top-level `PICT`) for thumbnail rendering. Keeps the first
        // when a writer emits duplicates (matching the `pdin` / `ctab`
        // conservative-merge convention).
        let mut file_pnot: Option<Pnot> = None;
        // ISO/IEC 14496-12 §4.2 / §11.1 User-Type Boxes (`uuid`).
        // File-level, "Zero or more" — collected in declaration order
        // because a single file may carry several vendor extensions
        // (PIFF tfxd + tfrf, Sony XAVC + GoPro GPMF, etc.) and there
        // is no implied "first wins" semantics; every entry is a
        // distinct vendor-schema record. QTFF does not define `uuid`
        // at the spec level but real-world `.mov` files routinely
        // carry them.
        let mut file_uuids: Vec<Uuid> = Vec::new();
        // QTFF p. 35 Color Table atom (`ctab`). Movie-level, optional,
        // "at most one" by convention. Keeps the first when a writer
        // emits duplicates.
        let mut movie_ctab: Option<Ctab> = None;
        // QTFF p. 43 Clipping atom (`clip`). Movie-level, optional,
        // "at most one" by convention (the spec figure shows a single
        // `clip` slot in the movie atom layout). Keeps the first when
        // a writer emits duplicates.
        let mut movie_clipping: Option<Clipping> = None;
        // Per-track running media-time cursor (DTS) for fragmented
        // playback. Indexed by track-id (not by track index); only
        // populated for tracks that actually receive `traf` runs.
        let mut track_dts_cursor: std::collections::HashMap<u32, u64> =
            std::collections::HashMap::new();
        // Per-track running sample index for fragmented playback.
        let mut track_sample_index_cursor: std::collections::HashMap<u32, u32> =
            std::collections::HashMap::new();
        // Faststart probe: track whether `moov` precedes `mdat` in
        // the top-level atom stream, ignoring `ftyp`, `free`, `skip`,
        // `wide`. Per QTFF "Movie Atom" — moov-first allows streaming
        // playback before the full file has been received.
        let mut seen_moov_before_mdat = false;
        let mut seen_mdat = false;

        // Top-level walk — accept arbitrary order, recognise the
        // common atoms, skip everything else.
        loop {
            let pos = input.stream_position()?;
            if pos >= total_len {
                break;
            }
            let hdr = match read_atom_header(input.as_mut())? {
                Some(h) => h,
                None => break,
            };
            let body_end = hdr
                .total_size
                .map(|t| hdr.payload_offset + (t - hdr.header_len))
                .unwrap_or(total_len);

            // Top-level injection-robustness check: a forged 32-bit
            // `size` field (or 64-bit extended size) on a small file
            // can declare body_end far beyond the actual end of the
            // input. Without this guard the per-arm `read_payload`
            // would attempt a multi-GiB allocation that fails as OOM
            // rather than a clean parse error. `walk_children` already
            // enforces this rule on nested atoms; we mirror it at the
            // top level. The check also rejects `payload_offset >
            // total_len` (header straddling EOF) and a wraparound
            // where `t - header_len` underflowed implicitly when the
            // declared size somehow slipped below `header_len` (the
            // size-floor check in `read_atom_header` already catches
            // this, but defence-in-depth is cheap).
            if body_end > total_len {
                return Err(Error::invalid(format!(
                    "MOV: top-level atom '{}' extends to byte {body_end} past end-of-file {total_len}",
                    hdr.type_str(),
                )));
            }

            match &hdr.fourcc {
                t if t == &FTYP => {
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    ftyp = Some(parse_ftyp(&payload)?);
                }
                t if t == &PDIN => {
                    // ISO/IEC 14496-12 §8.1.3 — at most one `pdin`
                    // per file. Silently keep the first when a writer
                    // emits duplicates (spec doesn't define
                    // override semantics, and dropping would lose
                    // earlier-is-better information per §8.1.3.1).
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    let parsed = parse_pdin(&payload)?;
                    if file_pdin.is_none() {
                        file_pdin = Some(parsed);
                    }
                }
                t if t == &SIDX => {
                    // ISO/IEC 14496-12 §8.16.3 — Segment Index Box.
                    // File-level, "Zero or more"; preserve every one in
                    // file order so callers can resolve hierarchical
                    // ("daisy-chain") indexes and per-stream indexes
                    // that share the segment.
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    file_sidx.push(parse_sidx(&payload)?);
                    // Open a new `ssix` slot — if the very next
                    // top-level box is `ssix` it binds here per
                    // §8.16.4.1; otherwise the slot stays `None`.
                    file_ssix_slots.push(None);
                    prev_was_sidx = true;
                    // The arm-end `prev_was_sidx = false` reset below
                    // would clobber the flag, so skip the common
                    // tail with an early `continue` after seeking.
                    input.seek(SeekFrom::Start(body_end))?;
                    continue;
                }
                t if t == &SSIX => {
                    // ISO/IEC 14496-12 §8.16.4 — Subsegment Index Box.
                    // File-level, "Zero or one per associated leaf-
                    // indexing `sidx`" (§8.16.4.1). When the previous
                    // top-level box was a `sidx`, bind this `ssix` to
                    // the trailing slot we opened there; otherwise the
                    // `ssix` is orphan / out-of-order — still parsed
                    // and surfaced via the public `ssix` Vec, but with
                    // no `sidx` cross-reference.
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    let parsed = parse_ssix(&payload)?;
                    if prev_was_sidx {
                        // Replace the trailing None we pushed when the
                        // sidx arm ran. The slot vector is non-empty
                        // here because `prev_was_sidx` is only set in
                        // the SIDX arm above.
                        let last = file_ssix_slots.len() - 1;
                        file_ssix_slots[last] = Some(parsed);
                    } else {
                        // Orphan `ssix` — keep the parsed value
                        // accessible through the public Vec but do not
                        // wire it back to any `sidx`. Stash a
                        // sentinel slot so the unzip preserves order.
                        // The slot vector remains the
                        // sidx-index-keyed view; the orphan goes into
                        // a parallel "tail" appended later.
                        file_ssix_orphans.push(parsed);
                    }
                    prev_was_sidx = false;
                    input.seek(SeekFrom::Start(body_end))?;
                    continue;
                }
                t if t == &STYP => {
                    // ISO/IEC 14496-12 §8.16.2 — Segment Type Box.
                    // File-level, "Zero or more"; same on-disk shape
                    // as `ftyp`. Preserve every one in file order even
                    // though §8.16.2.1 lets the parser ignore any that
                    // aren't first in their file — the writer's intent
                    // is still useful when inspecting a concatenated
                    // segment stream for boundary markers.
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    file_styp.push(parse_styp(&payload)?);
                }
                t if t == &PRFT => {
                    // ISO/IEC 14496-12 §8.16.5 — Producer Reference
                    // Time Box. File-level, "Zero or more". The box
                    // refers forward to the next `moof` in bitstream
                    // order (§8.16.5.1), so we preserve every one in
                    // file order — a live segment may carry several,
                    // one per movie fragment it ships.
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    file_prft.push(parse_prft(&payload)?);
                }
                t if t == &PNOT => {
                    // QTFF p. 26 — Preview atom. File-level, optional,
                    // at most one by convention. Silently keep the
                    // first when a writer emits duplicates (the spec
                    // doesn't define override semantics, and dropping
                    // would lose earlier-is-better information —
                    // matches the `pdin` / `ctab` policy).
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    let parsed = parse_pnot(&payload)?;
                    if file_pnot.is_none() {
                        file_pnot = Some(parsed);
                    }
                }
                t if t == &UUID => {
                    // ISO/IEC 14496-12 §4.2 / §11.1 — User-Type Box.
                    // File-level, "Zero or more"; every entry is a
                    // distinct vendor-schema record so we preserve
                    // declaration order rather than apply the
                    // "first wins" convention used by single-shot
                    // atoms like `pdin` / `pnot` / `ctab`. The body
                    // begins with the 16-byte `usertype` UUID
                    // followed by an opaque payload (§4.2 puts no
                    // lower bound on the payload length, so a body
                    // of exactly 16 bytes — UUID only — is legal).
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    file_uuids.push(parse_uuid(&payload)?);
                }
                t if t == &MOOV => {
                    if !seen_mdat {
                        seen_moov_before_mdat = true;
                    }
                    parse_moov(
                        input.as_mut(),
                        &hdr,
                        body_end,
                        &mut mvhd,
                        &mut tracks,
                        &mut movie_meta,
                        &mut movie_user_data,
                        &mut reference_movies,
                        &mut movie_bmff_meta,
                        &mut mehd_box,
                        &mut trex_defaults,
                        &mut leva_box,
                        &mut movie_ctab,
                        &mut movie_clipping,
                        &mut compressed_movie_algorithm,
                        // QTFF p. 30: a compressed movie resource is a
                        // single layer — the decompressed contents
                        // follow the standard uncompressed structure —
                        // so only the on-disk (outermost) `moov` may
                        // carry a `cmov`.
                        true,
                    )?;
                }
                t if t == &META => {
                    // File-level meta box — common in HEIF/HEIC/AVIF
                    // still-image files. The parser distinguishes the
                    // ISO BMFF §8.11 shape from the Apple key-value
                    // shape; only the former is meaningful at file
                    // scope (Apple `meta` is never written at file
                    // level in practice).
                    file_bmff_meta = parse_bmff_meta(input.as_mut(), &hdr)?;
                }
                t if t == &MDAT => {
                    seen_mdat = true;
                }
                t if t == &MOOF => {
                    // Movie Fragment Box (ISO/IEC 14496-12 §8.8.4).
                    // Round 18: parse `mfhd` + per-track `traf` → per-
                    // sample SampleEntry rows appended to each track's
                    // `fragment_samples` queue. Anchor:
                    // `moof_start` is the position of the size word
                    // (`pos`), which is the "first byte of the
                    // enclosing Movie Fragment Box" per §8.8.7.1.
                    let moof_start = pos;
                    let (mfhd_opt, trafs) = crate::fragment::parse_moof(input.as_mut(), &hdr)?;
                    // §8.8.5.3 — `mfhd.sequence_number` is mandatory.
                    // When the on-disk box is missing we fall back to 0
                    // for cross-referencing per-fragment sample-aux
                    // entries; the spec's monotonic-increase rule is
                    // already validated by callers walking the sequence
                    // vector directly.
                    let mfhd_seq = mfhd_opt.map(|m| m.sequence_number).unwrap_or(0);
                    if mfhd_opt.is_some() {
                        fragment_sequence_numbers.push(mfhd_seq);
                    }
                    // Anchor for the "previous traf end" within this
                    // moof. The very first traf with no
                    // base-data-offset and no default-base-is-moof
                    // defaults to "position of the first byte of the
                    // enclosing Movie Fragment Box" per §8.8.7.1.
                    let mut prev_traf_end = moof_start;
                    for traf in &trafs {
                        // Resolve track-id → track index.
                        let tid = traf.tfhd.track_id;
                        let track_idx = match tracks.iter().position(|t| t.tkhd.track_id == tid) {
                            Some(i) => i,
                            None => {
                                // Spec §8.8.7.3: track_ID must match a
                                // declared track. Refuse rather than
                                // silently drop.
                                return Err(Error::invalid(format!(
                                    "MOV: moof traf references unknown track_id {tid}"
                                )));
                            }
                        };
                        let trex = trex_defaults.iter().find(|t| t.track_id == tid);
                        // `tfdt` (§8.8.12), when present, is the absolute
                        // baseline; otherwise climb from the running
                        // cursor (round-18 behaviour).
                        let dts_cursor = traf
                            .tfdt
                            .unwrap_or_else(|| *track_dts_cursor.entry(tid).or_insert(0));
                        let sample_idx_cursor = *track_sample_index_cursor.entry(tid).or_insert(0);
                        let (samples, new_prev_traf_end, new_dts) = resolve_traf_samples(
                            traf,
                            trex,
                            moof_start,
                            prev_traf_end,
                            dts_cursor,
                            sample_idx_cursor,
                        )?;
                        let n_samples = samples.len() as u32;
                        tracks[track_idx].fragment_samples.extend(samples);
                        // §8.7.8.1 / §8.7.9.1 — surface any `saiz` /
                        // `saio` collected at this `traf`'s scope so
                        // callers can walk CMAF / CENC per-fragment
                        // sample-aux without re-parsing the file.
                        // Round 150.
                        if !traf.saiz.is_empty() || !traf.saio.is_empty() {
                            tracks[track_idx].fragment_sample_aux.push(
                                crate::sample_aux::FragmentSampleAux {
                                    mfhd_sequence_number: mfhd_seq,
                                    track_id: tid,
                                    saiz: traf.saiz.clone(),
                                    saio: traf.saio.clone(),
                                },
                            );
                        }
                        prev_traf_end = new_prev_traf_end;
                        track_dts_cursor.insert(tid, new_dts);
                        track_sample_index_cursor
                            .insert(tid, sample_idx_cursor.saturating_add(n_samples));
                    }
                }
                t if t == &MFRA => {
                    // Movie Fragment Random Access Box (§8.8.9).
                    // Lives at the end of the file (next to `mfro`).
                    // Walked here as a top-level child so we don't need
                    // a separate end-of-file pass — `mfra` is allowed
                    // anywhere at top scope per §8.8.9.1, and most
                    // writers emit it last.
                    let (tfras, _mfro) = parse_mfra(input.as_mut(), &hdr)?;
                    tfra_indexes.extend(tfras);
                }
                t if t == &FREE || t == &SKIP || t == &WIDE => {
                    // free-space atoms — skip
                }
                _ => {
                    // unknown — ignored at the top level.
                }
            }

            // §8.16.4.1 — `ssix` only binds to its `sidx` when it is
            // the very next top-level box. Any other top-level box
            // breaks the pairing window. The `SIDX` and `SSIX` arms
            // above use `continue` to skip this reset so their own
            // book-keeping survives.
            prev_was_sidx = false;
            input.seek(SeekFrom::Start(body_end))?;
        }

        if mvhd.is_none() {
            // A bare HEIF/HEIC/AVIF still-image file is allowed to
            // ship without any `moov` at all — its content is
            // entirely described by a top-level `meta` box. Accept
            // such files so callers can walk `file_bmff_meta` to
            // discover items, properties, and item-data extents.
            if file_bmff_meta.is_none() {
                return Err(Error::invalid("MOV: no moov/mvhd found"));
            }
        }
        if tracks.is_empty() {
            // Three valid "no tracks" shapes:
            //   * reference-movie file (`moov/rmra`) — tracks live in
            //     the referenced file (best surfaced as Unsupported so
            //     callers can fall back to alias resolution),
            //   * meta-only HEIF/HEIC/AVIF still-image files — the
            //     image data lives in `meta`/`iloc`, not in tracks,
            //   * any future shape that carries `mvhd` purely for
            //     timebase reasons but no media tracks (rare; we
            //     accept it silently when a `meta` is present).
            if !reference_movies.is_empty() {
                return Err(unsupported_error(format!(
                    "MOV: reference-movie container with {n} alternate(s); resolving \
                     external alias references is not supported",
                    n = reference_movies.len(),
                )));
            }
            if file_bmff_meta.is_none() && movie_bmff_meta.is_none() {
                return Err(Error::invalid("MOV: moov contains no tracks"));
            }
            // Otherwise: meta-only file, fall through. `samples` is
            // empty and `next_packet` will return `Eof` immediately;
            // callers consume `file_bmff_meta` / `bmff_meta` instead.
        }

        // Resolve codec ids per-track using the provided resolver
        // (only when the registry feature is on). The resolved
        // `CodecId` is stored alongside the per-track stream record.
        #[cfg(feature = "registry")]
        let streams = build_streams(&tracks, resolver);

        // Tfra-driven keyframe back-patch: ffmpeg's fragmented writer
        // emits a `tfra` entry per per-moof-leading sample but
        // *omits* `first_sample_flags` on alternate moofs, leaving
        // those samples carrying the per-fragment "non-sync" default.
        // §8.8.10.3 makes `tfra` authoritative for random-access
        // points, so walk every tfra row and lift the matching
        // sample's `keyframe` bit before flattening.
        for tfra in &tfra_indexes {
            let track_idx_opt = tracks.iter().position(|t| t.tkhd.track_id == tfra.track_id);
            if let Some(track_idx) = track_idx_opt {
                for entry in &tfra.entries {
                    // Match on `pts == entry.time` (tfra's `time` is
                    // composition / presentation time per §8.8.10.3).
                    let want_pts = entry.time as i64;
                    for s in tracks[track_idx].fragment_samples.iter_mut() {
                        if s.pts() == want_pts {
                            s.keyframe = true;
                        }
                    }
                }
            }
        }

        // Flatten sample tables into a globally offset-sorted queue.
        // For fragmented streams, the per-track stsz_count may be 0
        // (an "init segment" with no in-moov samples) while
        // `fragment_samples` carries the actual data; both sources
        // contribute to the flat queue.
        let mut samples: Vec<(u32, SampleEntry)> = Vec::new();
        for (track_idx, t) in tracks.iter().enumerate() {
            for sample in t.sample_table.iter_samples() {
                let s = sample?;
                samples.push((track_idx as u32, s));
            }
            for s in &t.fragment_samples {
                samples.push((track_idx as u32, *s));
            }
        }
        samples.sort_by_key(|(_, s)| s.offset);

        // Touch the resolver to silence unused warnings on the
        // standalone build path.
        let _ = resolver;

        // Unzip the `Vec<Option<Ssix>>` slot vector into:
        //   * `file_ssix`: every parsed Ssix in declaration order
        //     (bound ones first, then orphans tail-appended), exposed
        //     as the public Vec.
        //   * `file_ssix_for_sidx`: a parallel-to-`file_sidx`
        //     `Vec<Option<usize>>` indexing into `file_ssix`, so
        //     `ssix_for_sidx(sidx_index)` is an O(1) lookup.
        let mut file_ssix: Vec<Ssix> =
            Vec::with_capacity(file_ssix_slots.len() + file_ssix_orphans.len());
        let mut file_ssix_for_sidx: Vec<Option<usize>> = Vec::with_capacity(file_ssix_slots.len());
        for slot in file_ssix_slots {
            match slot {
                Some(s) => {
                    file_ssix_for_sidx.push(Some(file_ssix.len()));
                    file_ssix.push(s);
                }
                None => file_ssix_for_sidx.push(None),
            }
        }
        // Tail-append orphans so the public Vec still preserves
        // declaration order; they are not cross-referenced by
        // `ssix_for_sidx`.
        file_ssix.extend(file_ssix_orphans);

        Ok(Self {
            input,
            ftyp,
            mvhd,
            tracks,
            meta: movie_meta,
            user_data: movie_user_data,
            reference_movies,
            compressed_movie_algorithm,
            bmff_meta: movie_bmff_meta,
            file_bmff_meta,
            faststart: seen_moov_before_mdat,
            pdin: file_pdin,
            ctab: movie_ctab,
            clipping: movie_clipping,
            sidx: file_sidx,
            ssix: file_ssix,
            ssix_for_sidx: file_ssix_for_sidx,
            styp: file_styp,
            prft: file_prft,
            pnot: file_pnot,
            file_uuids,
            samples,
            next: 0,
            apply_edits: false,
            edited_segments: Vec::new(),
            trex_defaults,
            mehd: mehd_box,
            leva: leva_box,
            fragment_sequence_numbers,
            tfra_indexes,
            #[cfg(feature = "registry")]
            streams,
        })
    }

    /// `true` when the file has at least one `moof` box (i.e. is
    /// fragmented per ISO/IEC 14496-12 §8.8). Convenience accessor
    /// for callers that want to short-circuit "is this a DASH/
    /// fMP4 segment" decisions without inspecting
    /// `fragment_sequence_numbers` directly.
    pub fn is_fragmented(&self) -> bool {
        !self.fragment_sequence_numbers.is_empty() || !self.trex_defaults.is_empty()
    }

    /// True when the file is laid out for streaming playback
    /// ("faststart"): `moov` appears before any `mdat` at top level.
    /// `ftyp`, `free`, `skip`, `wide` atoms encountered before `moov`
    /// do not invalidate the faststart classification.
    pub fn is_faststart(&self) -> bool {
        self.faststart
    }

    /// Classify every brand declared by the file's `ftyp`. Empty when
    /// the file has no `ftyp` (a malformed-but-tolerated case the
    /// demuxer accepts because some early QTFF files predate `ftyp`).
    ///
    /// Order matches the on-wire order: `major_brand` first, then the
    /// declared `compatible_brands` in declaration order. Convenience
    /// helpers ([`Self::is_heic`], [`Self::is_avif`], [`Self::is_miaf`])
    /// query the same list with the family rules baked in.
    ///
    /// See [`crate::BrandClass`] for the brand registry.
    pub fn brand_class(&self) -> Vec<crate::header::BrandClass> {
        match &self.ftyp {
            Some(f) => f.brand_class(),
            None => Vec::new(),
        }
    }

    /// Whether the file declares any HEIC-family brand (`heic`,
    /// `heix`, `heim`, `heis`). Convenience wrapper around
    /// [`crate::Ftyp::is_heic`] that also handles the no-`ftyp` case.
    pub fn is_heic(&self) -> bool {
        self.ftyp.as_ref().map(|f| f.is_heic()).unwrap_or(false)
    }

    /// Whether the file declares any AVIF-family brand (`avif`,
    /// `avis`, `avio`).
    pub fn is_avif(&self) -> bool {
        self.ftyp.as_ref().map(|f| f.is_avif()).unwrap_or(false)
    }

    /// Whether the file declares any MIAF-family brand: explicit
    /// `mif1` / `mif2` markers, MIAF Annex A profiles (`MA1A` /
    /// `MA1B`), or any HEIC- / AVIF-family brand (each entails MIAF
    /// conformance per HEIF §10 / AVIF §3).
    pub fn is_miaf(&self) -> bool {
        self.ftyp.as_ref().map(|f| f.is_miaf()).unwrap_or(false)
    }

    /// The first Segment Type Box (`styp`) in the file, when present.
    /// Per ISO/IEC 14496-12 §8.16.2.1, a valid `styp` "shall be the
    /// first box in a segment"; this accessor surfaces that first
    /// declaration directly so DASH / CMAF callers don't have to index
    /// [`Self::styp`] by hand. Returns `None` for QTFF / non-segmented
    /// MP4s and for any file that omits `styp` entirely.
    pub fn first_styp(&self) -> Option<&Styp> {
        self.styp.first()
    }

    /// Whether the file's first Segment Type Box declares any of the
    /// DASH segment-conformance brands (`msdh` / `msix` / `risx`). A
    /// quick "is this a DASH media segment" classifier paired with
    /// [`Self::is_fragmented`].
    pub fn is_dash_segment(&self) -> bool {
        self.first_styp()
            .map(|s| s.is_dash_segment())
            .unwrap_or(false)
    }

    /// Whether the file's first Segment Type Box declares the CMAF
    /// segment-conformance brand `cmfs`.
    pub fn is_cmaf_segment(&self) -> bool {
        self.first_styp()
            .map(|s| s.is_cmaf_segment())
            .unwrap_or(false)
    }

    /// The first Producer Reference Time Box (`prft`) in the file, when
    /// present. ISO/IEC 14496-12 §8.16.5.1 ties every `prft` to the
    /// *next* movie fragment in bitstream order, so the first one
    /// describes the file's earliest fragment — typically the most
    /// useful single producer time for a live-stream catch-up
    /// computation. Returns `None` for QTFF, non-segmented MP4s, and
    /// non-live segmented streams that omit `prft`.
    pub fn first_prft(&self) -> Option<&Prft> {
        self.prft.first()
    }

    /// The Subsegment Index Box (`ssix`, ISO/IEC 14496-12 §8.16.4) that
    /// describes the Segment Index Box at `sidx_index` (0-based into
    /// [`Self::sidx`]), when present. §8.16.4.1 binds `ssix` to the
    /// immediately preceding `sidx`; the demuxer's top-level walker
    /// records that pairing at parse time so this lookup is O(1) and
    /// doesn't rely on the caller knowing on-disk box order.
    ///
    /// Returns `None` when:
    /// * `sidx_index` is out of range,
    /// * the addressed `sidx` is not followed by an `ssix` (the
    ///   default case for coarsely-segmented MP4 / fMP4 that does not
    ///   need a per-level partial-subsegment index), or
    /// * the file declares an orphan `ssix` (one that does not
    ///   immediately follow any `sidx`); such orphans are still
    ///   surfaced through the [`Self::ssix`] Vec but not bound to any
    ///   `sidx`.
    pub fn ssix_for_sidx(&self, sidx_index: usize) -> Option<&Ssix> {
        let j = (*self.ssix_for_sidx.get(sidx_index)?)?;
        self.ssix.get(j)
    }

    // Stub used by `open()` to validate the container before we
    // recurse with the real ctor; bails if the input is too short
    // to even hold an `ftyp`.
    fn open_with_resolver_inner(input: &mut Box<dyn ReadSeek>, _: Option<()>) -> Result<()> {
        let pos = input.stream_position()?;
        let total = input.seek(SeekFrom::End(0))?;
        input.seek(SeekFrom::Start(pos))?;
        if total < 16 {
            return Err(Error::invalid("MOV: input too small to be a QTFF file"));
        }
        Ok(())
    }

    /// Resolve the chapter list for the primary track at
    /// `primary_track_index` (a 0-based offset into [`Self::tracks`]).
    ///
    /// Returns `Ok(None)` when the primary track has no `tref/chap`
    /// reference (no chapters declared). Returns `Err(InvalidData)`
    /// when the chapter-track-id points at a track that doesn't exist
    /// in the file, or when a primary track names itself (a cycle that
    /// QTFF p. 51 forbids); we follow exactly one alias hop and refuse
    /// deeper chains. The chapter track's samples are read from the
    /// underlying input — the demuxer's sample cursor is preserved
    /// across the call.
    pub fn chapters_for(&mut self, primary_track_index: usize) -> Result<Option<ChapterList>> {
        let chap_track_id = match self.tracks.get(primary_track_index) {
            Some(t) => match t.chapter_track_ref() {
                Some(id) => id,
                None => return Ok(None),
            },
            None => return Err(Error::invalid("MOV: chapter primary index out of range")),
        };
        // Refuse self-reference.
        if let Some(primary) = self.tracks.get(primary_track_index) {
            if primary.tkhd.track_id == chap_track_id {
                return Err(Error::invalid(
                    "MOV: chapter track-id points at the primary track (cycle)",
                ));
            }
        }
        // Resolve track-id → track-index.
        let chap_index = self
            .tracks
            .iter()
            .position(|t| t.tkhd.track_id == chap_track_id)
            .ok_or_else(|| {
                Error::invalid(format!(
                    "MOV: chapter track-id {chap_track_id} not present in moov"
                ))
            })?;
        // The chapter target should itself not chain to another chapter
        // track — that would be an alias chain we explicitly forbid for
        // round 5.
        if self.tracks[chap_index].chapter_track_ref().is_some() {
            return Err(Error::invalid(
                "MOV: chapter track itself declares a chapter reference (alias chain)",
            ));
        }
        let time_scale = self.tracks[chap_index].mdhd.time_scale;
        // Walk the chapter track's samples in DTS order, reading each
        // sample's bytes and decoding as Apple text.
        let mut entries =
            Vec::with_capacity(self.tracks[chap_index].sample_table.sample_count() as usize);
        // Snapshot the iter-able sample list so we don't borrow `self`
        // mutably while reading the input.
        let samples: Vec<SampleEntry> = self.tracks[chap_index]
            .sample_table
            .iter_samples()
            .collect::<Result<Vec<_>>>()?;
        for s in samples {
            self.input.seek(SeekFrom::Start(s.offset))?;
            let mut buf = vec![0u8; s.size as usize];
            self.input.read_exact(&mut buf)?;
            let (title, text_encoding) = decode_text_sample_full(&buf)?;
            entries.push(ChapterEntry {
                start_time: s.dts,
                duration: s.duration,
                title,
                text_encoding,
            });
        }
        Ok(Some(ChapterList {
            track_index: chap_index as u32,
            time_scale,
            entries,
        }))
    }

    /// Resolve the file's primary HEIF image into an [`ImageLayout`]
    /// composition plan. Returns `None` when:
    ///
    /// * the input has no top-level `meta` box (it isn't a HEIF / MIAF
    ///   / AVIF / JPEG-XL file), or
    /// * the `meta` box has no `pitm`, or
    /// * the primary item is a `grid` / `iovl` whose payload lives in
    ///   `mdat` (`construction_method == 0`); use
    ///   [`Self::primary_image_layout_with_input`] for the mdat path,
    ///   or
    /// * the primary item isn't a recognised image-derivation
    ///   (`grid` / `iovl` / `iden`) or coded image type (`hvc1`,
    ///   `av01`, `j2k1`, …) — surfaced as `None` rather than an
    ///   error so callers that probe-and-fall-through don't have to
    ///   pattern-match on `InvalidData`.
    ///
    /// On the `Grid` / `Overlay` paths the per-tile / per-layer
    /// placement is computed once from the file's `iref dimg` and
    /// `iprp ispe` tables; on the `Identity` path the inner item id
    /// is surfaced directly so the caller can decode it through its
    /// usual codec path (`oxideav-h265`, `oxideav-av1`, …) and apply
    /// the iden item's transformative properties via
    /// [`crate::render_iden`].
    ///
    /// The lookup uses [`Self::file_bmff_meta`] (the top-level `meta`
    /// box). HEIF files store their primary image graph there;
    /// `moov/meta` (held in [`Self::bmff_meta`]) is the QTFF / movie-
    /// scope shape and is not consulted by this helper.
    pub fn primary_image_layout(&self) -> Option<crate::derived::ImageLayout> {
        let fm = self.file_bmff_meta.as_ref()?;
        crate::derived::primary_image_layout_for(fm)
    }

    /// Same as [`Self::primary_image_layout`] but also resolves
    /// `construction_method == 0` (mdat-resident) **and**
    /// `construction_method == 2` (item-resident, sub-slice of another
    /// item) `grid` / `iovl` derivation payloads by reading the file
    /// extents from the input.
    ///
    /// HEIF derived-image payloads are tiny fixed records (8 / 12
    /// bytes for `grid`, 12+ bytes for `iovl`); authoring tools
    /// overwhelmingly inline them in the meta box's `idat`, but the
    /// spec (ISO/IEC 14496-12 §8.11.3) permits placing them at any
    /// `construction_method == 0` extent — typically inside `mdat`.
    /// The pure-meta resolver [`Self::primary_image_layout`] returns
    /// `None` for that path because it has no input handle; this
    /// version takes `&mut self` so it can issue the seek+read for
    /// the file extents.
    ///
    /// `construction_method == 2` (item_offset) is also resolved here
    /// — the underlying read transparently sub-slices another item's
    /// resolved bytes via [`Self::resolve_item_bytes`], so an
    /// HEIF-grid primary whose payload lives at an offset inside
    /// another item lands a `Grid` plan as expected.
    ///
    /// Returns `None` for the same not-a-HEIF-file reasons as
    /// [`Self::primary_image_layout`].
    pub fn primary_image_layout_with_input(&mut self) -> Option<crate::derived::ImageLayout> {
        let pid = self.file_bmff_meta.as_ref()?.primary_item?;
        let info = self.file_bmff_meta.as_ref()?.find_item(pid)?;
        let item_type = info.item_type;
        match &item_type {
            b"grid" => {
                let bytes = self.read_derivation_payload_bytes(pid)?;
                let fm = self.file_bmff_meta.as_ref()?;
                match crate::derived::build_grid_layout(fm, pid, &bytes) {
                    Ok(g) => Some(crate::derived::ImageLayout::Grid(g)),
                    Err(_) => None,
                }
            }
            b"iovl" => {
                let bytes = self.read_derivation_payload_bytes(pid)?;
                let fm = self.file_bmff_meta.as_ref()?;
                match crate::derived::build_overlay_layout(fm, pid, &bytes) {
                    Ok(o) => Some(crate::derived::ImageLayout::Overlay(o)),
                    Err(_) => None,
                }
            }
            b"iden" => {
                let fm = self.file_bmff_meta.as_ref()?;
                // Defer to image_layout_for so the iden/inner cascade,
                // pixi, and color_profile fields are populated
                // identically to the pure-meta resolver.
                crate::derived::image_layout_for(fm, pid)
            }
            b"tmap" => {
                // Tone-mapping derivation: payload bytes may live in
                // mdat (construction_method == 0). Resolve via the same
                // path as grid/iovl, then surface a ToneMap variant
                // identical in shape to what `image_layout_for` would
                // produce on the idat path.
                let bytes = self.read_derivation_payload_bytes(pid).unwrap_or_default();
                let fm = self.file_bmff_meta.as_ref()?;
                let base = *fm.derived_from(pid).first()?;
                Some(crate::derived::ImageLayout::ToneMap {
                    item_id: pid,
                    base,
                    params: crate::derived::TmapPayload::from_bytes(bytes),
                })
            }
            _ => {
                let fm = self.file_bmff_meta.as_ref()?;
                crate::derived::image_layout_for(fm, pid)
            }
        }
    }

    /// Resolve a derivation item's payload bytes by inspecting its
    /// `iloc` `construction_method`:
    ///
    /// * `1` (idat) — concatenate the matching `idat` slices.
    /// * `0` (file extents) — seek to each extent in the input and
    ///   read its bytes.
    /// * any other (`2` / future) — `None` (caller's problem).
    fn read_derivation_payload_bytes(&mut self, item_id: u32) -> Option<Vec<u8>> {
        let fm = self.file_bmff_meta.as_ref()?;
        let loc = fm.find_location(item_id)?;
        match loc.construction_method {
            1 => crate::bmff_meta::idat_bytes_concat(fm, item_id),
            0 => {
                // Snapshot the extents (so we can drop the borrow on
                // self.file_bmff_meta before issuing the read).
                let extents: Vec<(u64, u64)> = loc
                    .extents
                    .iter()
                    .map(|e| (loc.base_offset + e.offset, e.length))
                    .collect();
                let mut total = 0usize;
                for &(_, len) in &extents {
                    total = total.checked_add(len as usize)?;
                }
                let mut out = Vec::with_capacity(total);
                for (off, len) in extents {
                    self.input.seek(SeekFrom::Start(off)).ok()?;
                    let mut chunk = vec![0u8; len as usize];
                    self.input.read_exact(&mut chunk).ok()?;
                    out.extend_from_slice(&chunk);
                }
                Some(out)
            }
            // construction_method == 2 (item_offset). Recursive
            // resolve via the public entry point so cycle detection
            // and depth-limiting kick in.
            _ => self.resolve_item_bytes(item_id).ok(),
        }
    }

    /// Resolve an item's bytes per ISO/IEC 14496-12 §8.11.3, including
    /// the `construction_method == 2` (item_offset) path which slices
    /// the bytes out of *another* item's resolved payload.
    ///
    /// Behaviour by `construction_method`:
    ///
    /// * `0` (file_offset) — concatenate the `(base_offset + offset,
    ///   length)` slices read directly from the input.
    /// * `1` (idat_offset) — slice the file's `meta/idat` payload at
    ///   `(base_offset + offset, length)` per extent.
    /// * `2` (item_offset) — recursively resolve the source item
    ///   (the **first** item in the file's `iref iloc` reference
    ///   targets, or the `extent_index`-selected one when
    ///   `index_size > 0`), then sub-slice the resulting bytes at
    ///   `(base_offset + offset, length)` per extent.
    ///
    /// Cycle detection: a `HashSet<u32>` of visited item ids is
    /// threaded through the recursion. A re-entry on a previously
    /// visited id aborts the resolve with [`Error::invalid`] rather
    /// than walking a self-referencing chain forever.
    ///
    /// Returns the concatenated payload bytes. Errors:
    ///
    /// * `Error::invalid("MOV: iloc cycle through items …")` on a
    ///   visited-set hit (item references itself transitively).
    /// * `Error::invalid("MOV: iloc item N has no entry")` when the
    ///   id isn't present in the file's `iloc` table.
    /// * `Error::invalid("MOV: iloc construction_method=2 source item
    ///   missing")` when cm=2 needs a source-item reference (via
    ///   `iref iloc` or extent_index) and the file lacks it.
    /// * I/O errors propagated from the underlying reader.
    pub fn resolve_item_bytes(&mut self, item_id: u32) -> Result<Vec<u8>> {
        let mut visited = std::collections::HashSet::new();
        self.resolve_item_bytes_inner(item_id, &mut visited)
    }

    fn resolve_item_bytes_inner(
        &mut self,
        item_id: u32,
        visited: &mut std::collections::HashSet<u32>,
    ) -> Result<Vec<u8>> {
        if !visited.insert(item_id) {
            return Err(Error::invalid(format!(
                "MOV: iloc cycle through item {item_id}"
            )));
        }
        let fm = self
            .file_bmff_meta
            .as_ref()
            .ok_or_else(|| Error::invalid("MOV: iloc resolve called without meta box"))?;
        let loc = fm
            .find_location(item_id)
            .ok_or_else(|| Error::invalid(format!("MOV: iloc item {item_id} has no entry")))?
            .clone();
        match loc.construction_method {
            0 => {
                let mut total = 0usize;
                for e in &loc.extents {
                    total = total
                        .checked_add(e.length as usize)
                        .ok_or_else(|| Error::invalid("MOV: iloc extent total overflow"))?;
                }
                let mut out = Vec::with_capacity(total);
                for e in &loc.extents {
                    let off = loc.base_offset.saturating_add(e.offset);
                    self.input.seek(SeekFrom::Start(off))?;
                    let mut chunk = vec![0u8; e.length as usize];
                    self.input.read_exact(&mut chunk)?;
                    out.extend_from_slice(&chunk);
                }
                Ok(out)
            }
            1 => {
                let fm = self.file_bmff_meta.as_ref().ok_or_else(|| {
                    Error::invalid("MOV: iloc cm=1 resolve lost meta-box reference")
                })?;
                crate::bmff_meta::idat_bytes_concat(fm, item_id).ok_or_else(|| {
                    Error::invalid(format!(
                        "MOV: iloc cm=1 idat resolve failed for item {item_id}"
                    ))
                })
            }
            2 => {
                // construction_method == 2: each extent is
                // `(extent_index?, offset, length)` *into another
                // item's* resolved payload.
                //
                // Source-item selection per §8.11.3: when the iloc's
                // index_size > 0 the per-extent `extent_index` is a
                // 1-based index into the `iref iloc` reference list
                // for this item (the source-item table). When
                // index_size == 0 the source is the single target of
                // the same `iref iloc` reference (HEIF authoring
                // tools that emit a single iloc-iref + many extents
                // all sub-slicing it).
                let iref_targets: Vec<u32> = fm.refs_from(item_id, b"iloc");
                let mut total = 0usize;
                for e in &loc.extents {
                    total = total
                        .checked_add(e.length as usize)
                        .ok_or_else(|| Error::invalid("MOV: iloc cm=2 extent total overflow"))?;
                }
                // Materialise each source-item resolution we need so
                // we don't recurse repeatedly for the same target.
                use std::collections::HashMap;
                let mut resolved_sources: HashMap<u32, Vec<u8>> = HashMap::new();
                let mut out = Vec::with_capacity(total);
                for e in &loc.extents {
                    let source_id = match e.index {
                        Some(idx) if idx > 0 => {
                            let i = (idx - 1) as usize;
                            *iref_targets.get(i).ok_or_else(|| {
                                Error::invalid(format!(
                                    "MOV: iloc cm=2 extent_index {idx} out of range for item {item_id}"
                                ))
                            })?
                        }
                        _ => {
                            // No per-extent index → take the single
                            // (or first) iref iloc target.
                            *iref_targets.first().ok_or_else(|| {
                                Error::invalid(format!(
                                    "MOV: iloc cm=2 source item missing for item {item_id}"
                                ))
                            })?
                        }
                    };
                    if let std::collections::hash_map::Entry::Vacant(slot) =
                        resolved_sources.entry(source_id)
                    {
                        let bytes = self.resolve_item_bytes_inner(source_id, visited)?;
                        slot.insert(bytes);
                    }
                    let src = &resolved_sources[&source_id];
                    let start = loc.base_offset.saturating_add(e.offset) as usize;
                    let end = if e.length == 0 {
                        src.len()
                    } else {
                        start
                            .checked_add(e.length as usize)
                            .ok_or_else(|| Error::invalid("MOV: iloc cm=2 sub-slice overflow"))?
                    };
                    if end > src.len() {
                        return Err(Error::invalid(format!(
                            "MOV: iloc cm=2 sub-slice out of range \
                             (item {item_id} → src {source_id}, end={end}, len={})",
                            src.len()
                        )));
                    }
                    out.extend_from_slice(&src[start..end]);
                }
                Ok(out)
            }
            other => Err(Error::invalid(format!(
                "MOV: iloc unknown construction_method {other}"
            ))),
        }
    }

    /// Pre-derived coded image base item (HEIF §6.4.7). Returns the
    /// base coded image's id when this item carries a `base` `iref`
    /// reference, otherwise `None`. Convenience alias for
    /// `self.file_bmff_meta.base_image_for(item_id)` that elides the
    /// `Option<&BmffMeta>` unwrap callers would otherwise have to do.
    pub fn base_image_for(&self, item_id: u32) -> Option<u32> {
        self.file_bmff_meta.as_ref()?.base_image_for(item_id)
    }

    /// Read the next sample's bytes from the input. Returns
    /// `(stream_index, sample, data)`.
    ///
    /// A sample whose data reference points at **external** media
    /// (its sample description's `data_reference_index` resolves to a
    /// non-self-referencing `dref` entry — QTFF p. 65 / ISO/IEC
    /// 14496-12 §8.7.2.1: the self-reference flag `0x000001` "means
    /// the media data is in the same file") returns an `Unsupported`
    /// error *after* advancing the cursor: the bytes at
    /// `sample.offset` in *this* file would be some other track's
    /// data, so emitting them would be silent corruption. The error
    /// is per-sample and recoverable — call again to continue with
    /// the next sample. See [`MovDemuxer::sample_data_in_file`] /
    /// [`MovDemuxer::track_has_external_data`].
    pub fn read_next(&mut self) -> Result<(u32, SampleEntry, Vec<u8>)> {
        if self.next >= self.samples.len() {
            return Err(Error::Eof);
        }
        let (stream_idx, sample) = self.samples[self.next];
        self.next += 1;
        if !self.sample_data_in_file(stream_idx as usize, &sample) {
            return Err(unsupported_error(format!(
                "MOV: sample {} of track {} references external media \
                 (non-self dref entry {}); resolve the data reference externally",
                sample.index,
                stream_idx,
                self.tracks[stream_idx as usize]
                    .sample_descriptions
                    .get(sample.sample_description_id.saturating_sub(1) as usize)
                    .map(|sd| sd.data_reference_index)
                    .unwrap_or(0),
            )));
        }
        self.input.seek(SeekFrom::Start(sample.offset))?;
        let mut buf = vec![0u8; sample.size as usize];
        self.input.read_exact(&mut buf)?;
        Ok((stream_idx, sample, buf))
    }

    /// Whether a sample's media bytes live in **this** file: its
    /// sample description's 1-based `data_reference_index` resolves
    /// to a self-referencing `dref` entry (QTFF p. 65 / ISO/IEC
    /// 14496-12 §8.7.2.1 flag `0x000001`). Tracks without a parsed
    /// `dref` table, an out-of-spec zero index, or a dangling index
    /// are treated as in-file (the lenient historical behaviour —
    /// every writer that omits the table stores its data locally).
    pub fn sample_data_in_file(&self, track_index: usize, sample: &SampleEntry) -> bool {
        let Some(track) = self.tracks.get(track_index) else {
            return true;
        };
        if track.data_references.is_empty() {
            return true;
        }
        let Some(sd) = track
            .sample_descriptions
            .get(sample.sample_description_id.saturating_sub(1) as usize)
        else {
            return true;
        };
        let Some(dri) = sd.data_reference_index.checked_sub(1) else {
            return true; // index 0 is out of spec; assume local
        };
        match track.data_references.get(dri as usize) {
            Some(crate::DataReference::SelfRef) => true,
            Some(_) => false,
            None => true, // dangling index; assume local
        }
    }

    /// Whether **any** of a track's sample descriptions reference
    /// external media (a non-self `dref` entry). Such a track's
    /// samples (or the subset using those descriptions) yield
    /// recoverable `Unsupported` errors from `next_packet()` /
    /// `read_next()` instead of bytes read from this file's `mdat`.
    pub fn track_has_external_data(&self, track_index: usize) -> bool {
        let Some(track) = self.tracks.get(track_index) else {
            return false;
        };
        if track.data_references.is_empty() {
            return false;
        }
        track.sample_descriptions.iter().any(|sd| {
            sd.data_reference_index
                .checked_sub(1)
                .and_then(|i| track.data_references.get(i as usize))
                .is_some_and(|r| !matches!(r, crate::DataReference::SelfRef))
        })
    }

    /// Whether more samples are available.
    pub fn remaining(&self) -> usize {
        self.samples.len().saturating_sub(self.next)
    }

    /// Map a media-timescale presentation timestamp on `track_index`
    /// through the track's edit list into the corresponding
    /// movie-timescale presentation timestamp. Returns `None` when the
    /// track index is out of range, when the sample's media-PTS falls
    /// outside every non-empty edit segment (i.e. the sample is dropped
    /// from the presentation timeline), or when the movie header is
    /// absent (no `mvhd` was parsed).
    ///
    /// This honours the edit list per QTFF Chapter 2 (pp. 46–48) and
    /// ISO/IEC 14496-12 §8.6.5 / §8.6.6 — including the empty-edit
    /// composition shift, dwell semantics, and the implicit trailing
    /// empty edit when `sum(elst.track_duration) < mvhd.duration`.
    ///
    /// `media_pts` is the value reported by `Packet::pts` / `Packet::dts`
    /// from this demuxer's `next_packet()` (both are in mdhd
    /// timescale). When the track carries no edit list, the call
    /// behaves as a 1:1 identity rescaled by `mvhd.time_scale /
    /// mdhd.time_scale`, matching the "no edits" rule (QTFF p. 47).
    pub fn movie_pts_for(&self, track_index: usize, media_pts: i64) -> Option<i64> {
        let track = self.tracks.get(track_index)?;
        let mvhd = self.mvhd.as_ref()?;
        track.media_pts_to_movie_pts(media_pts, mvhd.time_scale, Some(mvhd.duration))
    }

    /// Inverse of [`MovDemuxer::movie_pts_for`]. Resolve a
    /// movie-timescale presentation timestamp `movie_pts` to its
    /// corresponding media-timescale presentation timestamp on the
    /// track at `track_index`, honouring the track's edit list per
    /// QTFF Chapter 2 (pp. 46–48) and ISO/IEC 14496-12 §8.6.5 /
    /// §8.6.6 — including dwell, the §8.6.6.1 composition shift, and
    /// the implicit trailing empty edit when `sum(elst.track_duration)
    /// < mvhd.duration`.
    ///
    /// Returns `None` when the track index is out of range, when the
    /// movie header is absent (no `mvhd` was parsed), when the
    /// requested `movie_pts` is negative, when it lands inside an
    /// empty-edit window (the player would emit silence/black at that
    /// movie-time so no media-time correspondence exists), or when it
    /// falls past every edit segment.
    ///
    /// The typical caller is a seek-by-presentation-time entry point:
    /// the user requests "jump to 0:30 in the movie", the resolver
    /// converts the requested `movie_pts` to media-time, and the
    /// caller drives the per-track sample-table walker with the
    /// returned media-PTS to land on the right sample. Pairs with the
    /// existing `MovDemuxer::seek_to(stream, pts)` whose input is
    /// already media-PTS.
    pub fn media_pts_for(&self, track_index: usize, movie_pts: i64) -> Option<i64> {
        let track = self.tracks.get(track_index)?;
        let mvhd = self.mvhd.as_ref()?;
        track.movie_pts_to_media_pts(movie_pts, mvhd.time_scale, Some(mvhd.duration))
    }

    /// Resolve the per-track edit segments for `track_index` against
    /// the movie header. See [`crate::Track::edit_segments`].
    pub fn edit_segments_for(&self, track_index: usize) -> Option<Vec<crate::EditSegment>> {
        let track = self.tracks.get(track_index)?;
        let mvhd = self.mvhd.as_ref()?;
        Some(track.edit_segments(mvhd.time_scale, Some(mvhd.duration)))
    }

    /// Opt in (or back out) of **applied** edit-list packet timing.
    ///
    /// The default (`false`) keeps the historical contract: packets
    /// carry raw media-timeline timestamps and every sample in the
    /// sample tables is emitted, with the edit list merely *parsed*
    /// and exposed through [`MovDemuxer::movie_pts_for`] /
    /// [`MovDemuxer::media_pts_for`] / [`MovDemuxer::edit_segments_for`].
    ///
    /// With the mode enabled, `next_packet()` instead *applies* each
    /// track's edit list (QTFF Chapter 2 "Edit Atoms" pp. 46–48 /
    /// ISO/IEC 14496-12 §8.6.6) to the packets it emits:
    ///
    /// * A sample whose presentation timestamp falls outside every
    ///   edit segment is **dropped** — it is not part of the edited
    ///   presentation (e.g. encoder-priming samples skipped by a
    ///   head edit, or media trimmed off by a shortened segment).
    /// * Emitted `pts` / `dts` / `duration` live on the **edited
    ///   timeline**, still expressed in the stream's media timescale
    ///   (`Packet.time_base` is unchanged): a head empty edit delays
    ///   every timestamp, a trim edit shifts them toward zero, a
    ///   non-unity `media_rate` segment scales spacing and durations,
    ///   and a dwell edit stretches its held sample across the
    ///   segment window. See [`crate::edit::edited_timing_for_sample`]
    ///   for the exact per-segment rules.
    /// * Tracks without an edit list follow the "no edits" rule
    ///   (QTFF p. 47): the whole media plays from movie time 0, so
    ///   their packets are unchanged.
    ///
    /// Dropping is keyed on the sample's *presentation* time alone;
    /// a consumer that needs undisplayed decode dependencies (an
    /// edit starting on a non-sync frame) should keep the default
    /// mode and apply [`MovDemuxer::movie_pts_for`] itself. Files
    /// without a movie header (`mvhd`) have no movie timeline, so
    /// enabling the mode on one is a no-op.
    pub fn apply_edit_lists(&mut self, enable: bool) {
        let Some(mvhd) = self.mvhd.as_ref() else {
            self.apply_edits = false;
            return;
        };
        if enable && self.edited_segments.len() != self.tracks.len() {
            self.edited_segments = self
                .tracks
                .iter()
                .map(|t| t.edit_segments(mvhd.time_scale, Some(mvhd.duration)))
                .collect();
        }
        self.apply_edits = enable;
    }

    /// `true` when [`MovDemuxer::apply_edit_lists`] enabled the
    /// edited-timeline packet contract on this demuxer.
    pub fn edit_lists_applied(&self) -> bool {
        self.apply_edits
    }

    /// Project one sample of `track_index` onto the edited
    /// (presentation) timeline — the per-sample mapping used by the
    /// applied edit-list mode. Returns `None` when the sample is not
    /// presented by any edit segment (the applied mode drops it),
    /// when the track index is out of range, or when the file has no
    /// movie header. Usable regardless of whether the mode is
    /// enabled; see [`crate::edit::edited_timing_for_sample`].
    pub fn edited_timing_for(
        &self,
        track_index: usize,
        sample: &SampleEntry,
    ) -> Option<crate::edit::EditedTiming> {
        let track = self.tracks.get(track_index)?;
        let mvhd = self.mvhd.as_ref()?;
        // Use the cache when the mode has built it; otherwise resolve
        // on the fly so the helper works stand-alone.
        let owned;
        let segments: &[crate::EditSegment] = match self.edited_segments.get(track_index) {
            Some(segs) if self.edited_segments.len() == self.tracks.len() => segs,
            _ => {
                owned = track.edit_segments(mvhd.time_scale, Some(mvhd.duration));
                &owned
            }
        };
        crate::edit::edited_timing_for_sample(
            segments,
            sample.pts(),
            sample.dts as i64,
            sample.duration as i64,
            mvhd.time_scale,
            track.mdhd.time_scale,
        )
    }

    /// Iterator over `(track_index, &Track)` for tracks that should
    /// contribute to the *default presentation*: `tkhd` flag bit
    /// `enabled` is set AND `in_movie` is set (per QTFF pp. 31–32 /
    /// ISO/IEC 14496-12 §8.3.1.3). Chapter / hint / timecode tracks
    /// are still returned if their `tkhd.flags` carries those bits;
    /// callers that need a stricter "primary audio + video only"
    /// filter can layer on `Track::is_video` / `is_audio`.
    pub fn presentation_tracks(&self) -> impl Iterator<Item = (usize, &crate::Track)> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.is_enabled() && t.participates_in_movie())
    }

    /// Group the file's tracks by their `tkhd.alternate_group` field.
    /// Tracks with `alternate_group == 0` are not considered group
    /// members (per QTFF p. 33 / ISO/IEC 14496-12 §8.3.1.3) and are
    /// returned together under group id `0` if present at all.
    ///
    /// The return is `Vec<(group_id, Vec<track_index>)>` sorted by
    /// `group_id` ascending. Useful for muxers / players that need to
    /// pick exactly one track per non-zero group at playback time
    /// (e.g. one audio language track out of N).
    pub fn alternate_groups(&self) -> Vec<(i16, Vec<usize>)> {
        let mut by_group: std::collections::BTreeMap<i16, Vec<usize>> =
            std::collections::BTreeMap::new();
        for (idx, t) in self.tracks.iter().enumerate() {
            by_group.entry(t.alternate_group()).or_default().push(idx);
        }
        by_group.into_iter().collect()
    }

    /// Track Load Settings (QTFF p. 48) for `track_index`, when the
    /// track carries a `load` atom. `None` is the spec's "no preload
    /// hints declared" — the player should fall back to its own
    /// heuristics. See [`crate::track_load::Load`] for the typed
    /// preload-window + flag-bit accessors.
    pub fn track_load(&self, track_index: usize) -> Option<&crate::track_load::Load> {
        self.tracks.get(track_index)?.load_settings()
    }

    /// Track Input Map (QTFF pp. 51–53) for `track_index`, when the
    /// track carries an `imap` atom. `None` is the spec's "no input
    /// modifiers declared" — the track's `'ssrc'` references (if any)
    /// pass through their data unmodified. Each entry surfaces a typed
    /// `(input_type, object_id?)` pair plus a 1-based atom id that
    /// indexes into the parent track's `'ssrc'`-filtered reference
    /// list ([`Track::track_refs_of_kind`] with
    /// [`crate::track::TrackRefKind::NonPrimarySource`]). QTFF-only —
    /// ISO BMFF does not define `imap`, so the result is `None` for
    /// MP4 / fMP4 / HEIF / AVIF inputs.
    pub fn track_input_map(
        &self,
        track_index: usize,
    ) -> Option<&crate::track_input_map::TrackInputMap> {
        self.tracks.get(track_index)?.track_input_map()
    }

    /// Track Selection box (ISO/IEC 14496-12 §8.10.3) for
    /// `track_index`, when the track's `udta` carries a `tsel` child.
    /// `None` is the spec's "no switching information" sentinel: the
    /// player should fall back to ranking by `tkhd.alternate_group` +
    /// codec preference only. See
    /// [`crate::track_selection::TrackSelection`] for the typed
    /// `switch_group` + `attributes` accessors.
    pub fn track_selection(
        &self,
        track_index: usize,
    ) -> Option<&crate::track_selection::TrackSelection> {
        self.tracks.get(track_index)?.track_selection()
    }

    /// Sub Track boxes (ISO/IEC 14496-12 §8.14.3) for `track_index`,
    /// when the track's `udta` carries one or more `strk` children. The
    /// box is `Quantity: Zero or more` (§8.14.3.1) — the returned slice
    /// is in file order, and is empty when the track declares no sub
    /// tracks (the common case; sub tracks are used by layered codecs
    /// such as SVC / MVC). Each entry carries its mandatory `stri` Sub
    /// Track Information plus the `stsg` Sub Track Sample Group entries
    /// from its `strd`. QTFF does not define this box; for `.mov` inputs
    /// it is always empty.
    pub fn sub_tracks(&self, track_index: usize) -> &[crate::sub_track::SubTrack] {
        self.tracks
            .get(track_index)
            .map(|t| t.sub_tracks())
            .unwrap_or(&[])
    }

    /// Track Kind entries (ISO/IEC 14496-12 §8.10.4) for `track_index`,
    /// when the track's `udta` carries one or more `kind` children. The
    /// box is `Quantity: Zero or more` (§8.10.4.1) — the returned slice
    /// is in file order, and is empty when the track declares no kind.
    /// Each entry surfaces a `(schemeURI, value?)` pair (typically a
    /// WebVTT or DASH role tag for subtitle / caption / metadata
    /// tracks). QTFF does not define this box; for `.mov` inputs it is
    /// always empty.
    pub fn track_kinds(&self, track_index: usize) -> &[crate::kind::KindEntry] {
        self.tracks
            .get(track_index)
            .map(|t| t.track_kinds())
            .unwrap_or(&[])
    }

    /// Track Group Box membership entries (ISO/IEC 14496-12 §8.3.4) for
    /// `track_index`, in file order. Each entry surfaces one
    /// `(track_group_type, track_group_id)` pair declared by a FullBox
    /// child of the track's `trgr` container. Empty slice when the
    /// track has no `trgr` child. QTFF does not define this box; for
    /// plain `.mov` inputs the result is always empty. See
    /// [`Self::track_groups_for`] for the dual lookup ("which tracks
    /// share this `(type, id)` group?").
    pub fn track_group_entries(
        &self,
        track_index: usize,
    ) -> &[crate::track_group::TrackGroupTypeEntry] {
        self.tracks
            .get(track_index)
            .map(|t| t.track_groups())
            .unwrap_or(&[])
    }

    /// Resolve a 1-based `tkhd.track_id` to its 0-based index inside
    /// [`Self::tracks`]. Returns `None` when the file declares no track
    /// with that id, when the supplied id is `0` (QTFF p. 51 reserves
    /// `0` as the "no reference" sentinel for `tref` slots), or when
    /// the file is empty. This is the building block underneath the
    /// per-`tref`-type index resolvers (`sync_track_indices` /
    /// `transcript_track_indices` / `hint_track_indices` /
    /// `non_primary_source_track_indices` / `timecode_track_index`)
    /// and the existing `chapters_for` chapter-track resolver, but it
    /// is exported directly so external callers can resolve any other
    /// `tkhd.track_id` reference they may encounter (e.g. a
    /// `traf.tfhd.track_id`, a `tfra.track_id`, or an `mvex/trex`
    /// default-association id).
    pub fn track_index_for_id(&self, track_id: u32) -> Option<usize> {
        if track_id == 0 {
            return None;
        }
        self.tracks.iter().position(|t| t.tkhd.track_id == track_id)
    }

    /// Resolve every `tref/<kind>` reference declared by `track_index`
    /// to the 0-based index inside [`Self::tracks`] of the referenced
    /// peer. References whose 1-based `track_id` does not appear in
    /// the file (a writer slip — every QTFF `tref` slot SHOULD point
    /// at an in-file track per p. 49) and the spec-defined `0` "unused
    /// slot" sentinel (p. 51) are both filtered out so callers see
    /// only resolvable indices. The result preserves declaration order
    /// across every reference-type atom of the requested `kind` inside
    /// the source track's `tref`. Returns an empty `Vec` when
    /// `track_index` is out of range or when the track declares no
    /// reference of that kind.
    pub fn tref_track_indices(&self, track_index: usize, kind: TrackRefKind) -> Vec<usize> {
        let Some(track) = self.tracks.get(track_index) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for id in track.track_refs_of_kind(kind) {
            if let Some(idx) = self.track_index_for_id(id) {
                out.push(idx);
            }
        }
        out
    }

    /// 0-based track index of the `tref/tmcd` time-code track this
    /// `track_index` points at — QTFF p. 50 Table 2-2 row `'tmcd'`
    /// ("Time code. Usually references a time code track."). Returns
    /// `None` when the track declares no `'tmcd'` reference, when the
    /// referenced `track_id` does not appear in the file, or when
    /// `track_index` is out of range. Multiple `'tmcd'` references on
    /// one track are unusual but permitted; this accessor returns the
    /// first resolvable one (matching the existing
    /// [`Track::timecode_track_ref`] convention) — callers that need
    /// every entry should use [`Self::tref_track_indices`] with
    /// [`TrackRefKind::Timecode`].
    pub fn timecode_track_index(&self, track_index: usize) -> Option<usize> {
        self.tref_track_indices(track_index, TrackRefKind::Timecode)
            .into_iter()
            .next()
    }

    /// 0-based track indices of every `tref/sync` peer of
    /// `track_index` — QTFF p. 50 Table 2-2 row `'sync'`
    /// ("Synchronization. Usually between a video and sound track.
    /// Indicates that the two tracks are synchronized."). The
    /// reference may be reciprocated by the peer track listing this
    /// track as its own `'sync'` source; the spec leaves the
    /// directionality at the writer's discretion (p. 49 "uni-
    /// directional"; p. 50 "The reference can be from either track to
    /// the other, or there may be two references."). Result preserves
    /// declaration order; the 0-id slot and unresolvable ids are
    /// filtered out.
    pub fn sync_track_indices(&self, track_index: usize) -> Vec<usize> {
        self.tref_track_indices(track_index, TrackRefKind::Sync)
    }

    /// 0-based track indices of every `tref/scpt` peer of
    /// `track_index` — QTFF p. 50 Table 2-2 row `'scpt'`
    /// ("Transcript. Usually references a text track."). The result
    /// preserves declaration order; the 0-id slot and unresolvable ids
    /// are filtered out.
    pub fn transcript_track_indices(&self, track_index: usize) -> Vec<usize> {
        self.tref_track_indices(track_index, TrackRefKind::Transcript)
    }

    /// 0-based track indices of every `tref/hint` peer of
    /// `track_index` — QTFF p. 50 Table 2-2 row `'hint'` ("The
    /// referenced tracks contain the original media for this hint
    /// track."). A QuickTime hint track (RTP packetization metadata,
    /// QTFF "Hint Media" p. 145) names its source media tracks through
    /// these references so a streaming server can locate the bytes
    /// each packet hint cites without re-walking the file's codec
    /// tags. Result preserves declaration order; the 0-id slot and
    /// unresolvable ids are filtered out.
    pub fn hint_track_indices(&self, track_index: usize) -> Vec<usize> {
        self.tref_track_indices(track_index, TrackRefKind::Hint)
    }

    /// 0-based track indices of every `tref/ssrc` peer of
    /// `track_index` — QTFF p. 50 Table 2-2 row `'ssrc'`
    /// ("Nonprimary source. Indicates that the referenced track
    /// should send its data to this track, rather than presenting
    /// it."). The 1-based atom-id slots of the track's
    /// [`crate::track_input_map::TrackInputMap`] (when present) index
    /// into this list per QTFF p. 53 — so a caller resolving an
    /// `imap` entry's modulation target can pair the returned indices
    /// with the input-map entries directly. Result preserves
    /// declaration order; the 0-id slot and unresolvable ids are
    /// filtered out.
    pub fn non_primary_source_track_indices(&self, track_index: usize) -> Vec<usize> {
        self.tref_track_indices(track_index, TrackRefKind::NonPrimarySource)
    }

    /// All tracks that belong to the same §8.3.4 track group, given the
    /// group's `(track_group_type, track_group_id)` key. Returns the
    /// 0-based track indices in track-declaration order. A return of an
    /// empty `Vec` means no track in the file declares that membership
    /// (the spec doesn't reserve a sentinel for "absent group", so the
    /// caller must distinguish "valid group, no members" from "we
    /// haven't named that group" themselves).
    pub fn tracks_in_group(&self, track_group_type: [u8; 4], track_group_id: u32) -> Vec<usize> {
        let mut out = Vec::new();
        for (idx, t) in self.tracks.iter().enumerate() {
            for entry in t.track_groups() {
                if entry.track_group_type == track_group_type
                    && entry.track_group_id == track_group_id
                {
                    out.push(idx);
                    break;
                }
            }
        }
        out
    }

    /// Bucket the file's tracks by their declared `(track_group_type,
    /// track_group_id)` memberships. Returns
    /// `Vec<((type, id), Vec<track_index>)>` sorted ascending by
    /// `(track_group_type, track_group_id)`. A track that belongs to
    /// several groups appears in several buckets; a track with no
    /// `trgr` membership appears in none. Pairs with
    /// [`Self::tracks_in_group`] for single-bucket lookup and with
    /// [`Self::alternate_groups`] / [`Self::switch_groups`] for the
    /// other ISO BMFF track-relationship surfaces.
    #[allow(clippy::type_complexity)]
    pub fn track_groups(&self) -> Vec<(([u8; 4], u32), Vec<usize>)> {
        let mut by_group: std::collections::BTreeMap<([u8; 4], u32), Vec<usize>> =
            std::collections::BTreeMap::new();
        for (idx, t) in self.tracks.iter().enumerate() {
            for entry in t.track_groups() {
                by_group.entry(entry.key()).or_default().push(idx);
            }
        }
        // Dedup per-bucket — a single track that lists the same (type, id)
        // pair twice (legal per §8.3.4) should not appear twice in the
        // bucket, since the bucket is "set of member tracks" not
        // "multiset of declarations".
        for v in by_group.values_mut() {
            v.dedup();
        }
        by_group.into_iter().collect()
    }

    /// Group the file's tracks by ISO/IEC 14496-12 §8.10.3
    /// `switch_group`. Returns `Vec<(switch_group_id, Vec<track_index>)>`
    /// sorted ascending by switch-group id. Tracks without a `tsel`
    /// child OR with `tsel.switch_group == 0` are *excluded* — the
    /// spec is explicit (§8.10.3.4) that those values carry no
    /// switching information, so it would be wrong to bucket them
    /// together at switch-group 0.
    ///
    /// Switch groups nest *inside* alternate groups: two tracks with
    /// the same `switch_group` id but different
    /// `tkhd.alternate_group` values are a malformed input
    /// (§8.10.3.4 last sentence) and the caller is responsible for
    /// detecting that case. This helper just lists what the file
    /// declares; pair it with [`Self::alternate_groups`] for the full
    /// hierarchy.
    pub fn switch_groups(&self) -> Vec<(i32, Vec<usize>)> {
        let mut by_group: std::collections::BTreeMap<i32, Vec<usize>> =
            std::collections::BTreeMap::new();
        for (idx, t) in self.tracks.iter().enumerate() {
            if let Some(ts) = t.track_selection() {
                if ts.switch_group != 0 {
                    by_group.entry(ts.switch_group).or_default().push(idx);
                }
            }
        }
        by_group.into_iter().collect()
    }

    /// Look up the `'roll'` (§10.1.1.2) recovery distance for a
    /// specific sample on a track.
    ///
    /// Returns `None` when the track carries no `sbgp`/`sgpd` with
    /// `grouping_type == 'roll'`, when the sample is outside the
    /// grouping, or when the entry payload is malformed.
    ///
    /// Sign conventions per §10.1.1.3:
    /// * `roll_distance > 0` — recovery is complete `N` samples
    ///   **after** the marked sample (gradual-decoding-refresh).
    /// * `roll_distance < 0` — `|N|` samples **before** the marked
    ///   sample must be decoded first (audio whose output is only
    ///   correct after pre-rolling). The value `0` is reserved and
    ///   never emitted by a conforming encoder.
    pub fn roll_distance_for(&self, track_index: usize, sample_zero_based: u32) -> Option<i16> {
        let table = &self.tracks.get(track_index)?.sample_table;
        let idx = table
            .group_description_index_for_sample(&crate::atom::fourcc("roll"), sample_zero_based)?;
        let (_sbgp, sgpd) = table.sample_group(&crate::atom::fourcc("roll"))?;
        let entry = sgpd.entry(idx)?;
        crate::sample_groups::decode_roll(&entry.payload)
            .ok()
            .map(|r| r.roll_distance)
    }

    /// Look up the `'prol'` AudioPreRollEntry distance (§10.1.1.2)
    /// for a specific audio sample. This is the AAC / Opus codec-
    /// priming convention used by CMAF and DASH: after seeking to a
    /// sync sample, the player must back up by `|roll_distance|`
    /// audio frames before the decoder's output is valid.
    ///
    /// Returns `None` when the track has no `'prol'` grouping.
    pub fn audio_preroll_for(&self, track_index: usize, sample_zero_based: u32) -> Option<i16> {
        let table = &self.tracks.get(track_index)?.sample_table;
        let idx = table
            .group_description_index_for_sample(&crate::atom::fourcc("prol"), sample_zero_based)?;
        let (_sbgp, sgpd) = table.sample_group(&crate::atom::fourcc("prol"))?;
        let entry = sgpd.entry(idx)?;
        crate::sample_groups::decode_prol(&entry.payload)
            .ok()
            .map(|r| r.roll_distance)
    }

    /// Look up the `'rap '` VisualRandomAccessEntry (§10.4.2) for a
    /// specific sample on a video track.
    ///
    /// Spec note (§10.4.1): samples marked by `'rap '` **must** be
    /// random-access points, and may also be sync samples. So
    /// callers building a seek index can union the `stss` table with
    /// the `'rap '` grouping to enumerate every legitimate
    /// random-access entry point — including "open GOP" IDR-likes
    /// where some leading samples in decode order won't be decodable
    /// when entry happens at the RAP.
    ///
    /// Returns `None` when the track has no `'rap '` grouping or
    /// when the sample isn't covered.
    pub fn visual_random_access_for(
        &self,
        track_index: usize,
        sample_zero_based: u32,
    ) -> Option<crate::sample_groups::VisualRandomAccess> {
        let table = &self.tracks.get(track_index)?.sample_table;
        let idx = table
            .group_description_index_for_sample(&crate::atom::fourcc("rap "), sample_zero_based)?;
        let (_sbgp, sgpd) = table.sample_group(&crate::atom::fourcc("rap "))?;
        let entry = sgpd.entry(idx)?;
        crate::sample_groups::decode_rap(&entry.payload).ok()
    }

    /// Look up the `'tele'` TemporalLevelEntry (§10.5.2) for a sample.
    ///
    /// Returns `(temporal_level, level_independently_decodable)` where
    /// `temporal_level` is the 1-based `sgpd` group-description index
    /// (the spec defines the temporal level to *equal* that index).
    /// Returns `None` when the track has no `'tele'` grouping or the
    /// sample isn't covered.
    pub fn temporal_level_for(
        &self,
        track_index: usize,
        sample_zero_based: u32,
    ) -> Option<(u32, bool)> {
        let table = &self.tracks.get(track_index)?.sample_table;
        let idx = table
            .group_description_index_for_sample(&crate::atom::fourcc("tele"), sample_zero_based)?;
        let (_sbgp, sgpd) = table.sample_group(&crate::atom::fourcc("tele"))?;
        let entry = sgpd.entry(idx)?;
        let tele = crate::sample_groups::decode_tele(&entry.payload).ok()?;
        Some((idx, tele.level_independently_decodable))
    }

    /// Look up the `'sap '` SAPEntry (§10.6.2) for a specific sample.
    ///
    /// Returns `None` when the track has no `'sap '` grouping or when
    /// the sample isn't covered by the grouping.
    pub fn stream_access_point_for(
        &self,
        track_index: usize,
        sample_zero_based: u32,
    ) -> Option<crate::sample_groups::StreamAccessPoint> {
        let table = &self.tracks.get(track_index)?.sample_table;
        let idx = table
            .group_description_index_for_sample(&crate::atom::fourcc("sap "), sample_zero_based)?;
        let (_sbgp, sgpd) = table.sample_group(&crate::atom::fourcc("sap "))?;
        let entry = sgpd.entry(idx)?;
        crate::sample_groups::decode_sap(&entry.payload).ok()
    }

    /// Look up the `'rash'` RateShareEntry (§10.2.2.2) for a specific
    /// sample. The same rate-share record typically applies to many
    /// consecutive samples, so this resolves through the `sbgp` run.
    ///
    /// Returns `None` when the track has no `'rash'` grouping or when
    /// the sample isn't covered by the grouping.
    pub fn rate_share_for(
        &self,
        track_index: usize,
        sample_zero_based: u32,
    ) -> Option<crate::sample_groups::RateShare> {
        let table = &self.tracks.get(track_index)?.sample_table;
        let idx = table
            .group_description_index_for_sample(&crate::atom::fourcc("rash"), sample_zero_based)?;
        let (_sbgp, sgpd) = table.sample_group(&crate::atom::fourcc("rash"))?;
        let entry = sgpd.entry(idx)?;
        crate::sample_groups::decode_rash(&entry.payload).ok()
    }

    /// Look up the `'alst'` AlternativeStartupEntry (§10.3.2) for a
    /// specific sample.
    ///
    /// Returns `None` when the track has no `'alst'` grouping or when
    /// the sample isn't covered by the grouping.
    pub fn alternative_startup_for(
        &self,
        track_index: usize,
        sample_zero_based: u32,
    ) -> Option<crate::sample_groups::AlternativeStartup> {
        let table = &self.tracks.get(track_index)?.sample_table;
        let idx = table
            .group_description_index_for_sample(&crate::atom::fourcc("alst"), sample_zero_based)?;
        let (_sbgp, sgpd) = table.sample_group(&crate::atom::fourcc("alst"))?;
        let entry = sgpd.entry(idx)?;
        crate::sample_groups::decode_alst(&entry.payload).ok()
    }

    /// Return the union of sync samples (`stss`) and `'rap '`-marked
    /// samples (§10.4.1) for a track, expressed as 0-based sample
    /// indices in decode order.
    ///
    /// Both spec mechanisms identify legitimate random-access
    /// entry-points — `stss` enumerates closed GOPs (every sample
    /// after a sync point decodes correctly), and `'rap '` enumerates
    /// open GOPs (with optional `num_leading_samples` that the player
    /// must discard). A player can union the two lists to surface
    /// every entry-point the file's authoring tool exposed; this
    /// helper does it once for the caller.
    ///
    /// For tracks with an empty `stss` (the QTFF "every sample is a
    /// sync sample" implicit case), the returned vector lists every
    /// sample index. Empty otherwise.
    pub fn random_access_points(&self, track_index: usize) -> Vec<u32> {
        let track = match self.tracks.get(track_index) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let table = &track.sample_table;
        let total = table.stsz_count;

        let mut points: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        if table.stss.is_empty() {
            // QTFF p. 73 — empty stss means every sample is sync.
            for i in 0..total {
                points.insert(i);
            }
        } else {
            // stss is 1-based per the spec; normalise to 0-based.
            for &one_based in &table.stss {
                if one_based >= 1 && one_based <= total {
                    points.insert(one_based - 1);
                }
            }
        }
        // Union with 'rap ' grouping (open GOPs).
        if let Some((sbgp, _)) = table.sample_group(&crate::atom::fourcc("rap ")) {
            let mut cursor: u64 = 0;
            for run in &sbgp.entries {
                if run.group_description_index != 0 {
                    let end = (cursor + run.sample_count as u64).min(total as u64);
                    for i in cursor..end {
                        points.insert(i as u32);
                    }
                }
                cursor += run.sample_count as u64;
                if cursor >= total as u64 {
                    break;
                }
            }
        }
        points.into_iter().collect()
    }

    /// Look up the `sdtp` (Independent and Disposable Samples Box,
    /// ISO/IEC 14496-12 §8.6.4) row for a 0-based decode-order sample
    /// on a track.
    ///
    /// Returns `None` when the track carries no `sdtp` box or the
    /// sample index is past the table. The returned [`SdtpEntry`]
    /// exposes the four 2-bit fields plus the convenience predicates
    /// [`SdtpEntry::is_independent`] (I-picture, codec-agnostically)
    /// and [`SdtpEntry::is_disposable`] (no other sample depends on
    /// this one, so it may be skipped while rolling forward in
    /// trick-mode — §8.6.4.1).
    pub fn sample_dependency(
        &self,
        track_index: usize,
        sample_zero_based: u32,
    ) -> Option<crate::sample_table::SdtpEntry> {
        self.tracks
            .get(track_index)?
            .sample_table
            .sample_dependency(sample_zero_based)
    }

    /// Look up the `stdp` Degradation Priority Box (ISO/IEC 14496-12
    /// §8.5.3) value for a 0-based decode-order sample on a track.
    ///
    /// Returns `None` when the track carries no `stdp` box, the sample
    /// index is past the table, or `track_index` is out of range. The
    /// raw 16-bit value is returned unmodified — §8.5.3.1 leaves the
    /// numeric meaning and acceptable range to specifications derived
    /// from the base format, so callers consult the spec that defined
    /// the track to interpret the priority.
    ///
    /// This is optional metadata: a track plays and seeks correctly
    /// when it is ignored. Transports that selectively discard samples
    /// under load may use the priority to choose which samples to drop
    /// (§8.5.3.1).
    pub fn sample_degradation_priority(
        &self,
        track_index: usize,
        sample_zero_based: u32,
    ) -> Option<u16> {
        self.tracks
            .get(track_index)?
            .sample_table
            .sample_degradation_priority(sample_zero_based)
    }

    /// Look up the `padb` Padding Bits Box (ISO/IEC 14496-12 §8.7.6)
    /// `pad` field for a 0-based decode-order sample on a track.
    ///
    /// Returns `None` when the track carries no `padb` box, the sample
    /// index is past the table, or `track_index` is out of range. The
    /// returned value is the spec's 3-bit field in `0..=7` —
    /// "the number of bits at the end of sample (i*2)+1" or
    /// "(i*2)+2" per §8.7.6.3 — indicating how many trailing bits of
    /// the sample's media payload are writer-inserted padding to round
    /// up to a whole-byte boundary.
    ///
    /// This is optional metadata: callers that consume whole-byte
    /// samples ignore it. A re-emitter that needs to round-trip the
    /// original bit-level payload reads this and trims the padding
    /// before re-packing.
    pub fn sample_padding_bits(&self, track_index: usize, sample_zero_based: u32) -> Option<u8> {
        self.tracks
            .get(track_index)?
            .sample_table
            .sample_padding_bits(sample_zero_based)
    }

    /// Source of `track_index`'s sample-size data: `stsz` (§8.7.3.2),
    /// `stz2` (§8.7.3.3), or `None` when the `stbl` carries no
    /// sample-size box at all (a fragmented-only track whose sizes all
    /// come from `trun`). Round 204 — ISO/IEC 14496-12 §8.7.3 treats
    /// the two as mutually-exclusive alternatives and at most one
    /// appears in any given `stbl`; this accessor lets a remuxer that
    /// wants to round-trip the original on-disk encoding choice
    /// (e.g. preserve a compact-packed CMAF segment) detect it
    /// without re-parsing the box. The per-sample values downstream
    /// code consumes through [`SampleTable::sample_count`] and
    /// [`SampleTable::sample_size_at`] are identical regardless of
    /// which box populated the table.
    ///
    /// Returns `None` when `track_index` is out of range.
    pub fn sample_size_source(&self, track_index: usize) -> Option<SampleSizeSource> {
        self.tracks
            .get(track_index)?
            .sample_table
            .sample_size_source
    }

    /// Number of chunks declared by `stco` / `co64` for `track_index`
    /// — the length of the chunk-offset table (QTFF p. 78, ISO/IEC
    /// 14496-12 §8.7.5). Returns `None` when `track_index` is out of
    /// range; returns `Some(0)` for a fragmented-only track whose
    /// `stbl` carries no chunk-offset box (every sample comes from
    /// `trun` instead).
    pub fn chunk_count(&self, track_index: usize) -> Option<u32> {
        Some(self.tracks.get(track_index)?.sample_table.chunk_count())
    }

    /// Number of samples in 1-based chunk `chunk_1based` on
    /// `track_index`, per the `stsc` Sample-to-Chunk row that applies
    /// to it (QTFF p. 75 "Sample-to-chunk atom"). Returns `None` when
    /// `track_index` is out of range, `chunk_1based == 0` (chunk
    /// numbers are 1-based per QTFF p. 76), or `chunk_1based` exceeds
    /// the chunk-offset table.
    pub fn samples_in_chunk(&self, track_index: usize, chunk_1based: u32) -> Option<u32> {
        self.tracks
            .get(track_index)?
            .sample_table
            .samples_in_chunk(chunk_1based)
    }

    /// Resolve a 0-based decode-order `sample_idx` to its
    /// `(chunk_1based, sample_offset_in_chunk_0based)` location, per
    /// QTFF p. 79 "Finding a Sample" step 2 ("scan the
    /// sample-to-chunk atom to discover the chunk which contains the
    /// sample in question"). Random-access companion of the
    /// [`SampleTable::iter_samples`] walker — callers can locate any
    /// sample's chunk without iterating every prior sample. Returns
    /// `None` when `track_index` is out of range, `sample_idx >=
    /// sample_count`, or the `stsc` / chunk-offset tables are empty
    /// or malformed for the queried sample.
    pub fn chunk_for_sample(&self, track_index: usize, sample_idx: u32) -> Option<(u32, u32)> {
        self.tracks
            .get(track_index)?
            .sample_table
            .chunk_for_sample(sample_idx)
    }

    /// Absolute file byte offset of sample at 0-based decode-order
    /// index `sample_idx` on `track_index`, per the QTFF p. 79
    /// four-step "Finding a Sample" algorithm. Random-access
    /// companion of the [`SampleTable::iter_samples`] walker — the
    /// caller can read a single sample's bytes without iterating to
    /// it. Returns `None` when `track_index` is out of range,
    /// `sample_idx` is past the sample-count, or any per-sample size
    /// lookup fails.
    pub fn sample_offset(&self, track_index: usize, sample_idx: u32) -> Option<u64> {
        self.tracks
            .get(track_index)?
            .sample_table
            .sample_offset(sample_idx)
    }

    /// Read and decode the timecode-track sample at 0-based decode-order
    /// index `sample_idx` on `track_index` (QTFF p. 108 "Timecode Sample
    /// Data").
    ///
    /// The track's first sample description must carry a `tmcd` body (i.e.
    /// it is a timecode track, [`Track::is_timecode`]); the per-sample
    /// payload is read from `mdat` via [`Self::sample_offset`] and decoded
    /// by [`Tmcd::decode_sample`], yielding either a tape-counter value or
    /// a packed `[H:M:S:F]` record. Returns `None` when the track is not a
    /// timecode track / has no `tmcd` description, the index is out of
    /// range, or the byte offset cannot be resolved; `Err` only on an
    /// I/O failure reading the sample bytes.
    pub fn timecode_sample(
        &mut self,
        track_index: usize,
        sample_idx: u32,
    ) -> Result<Option<TimecodeSample>> {
        let tmcd = match self
            .tracks
            .get(track_index)
            .and_then(|t| t.sample_descriptions.first())
            .and_then(|d| d.tmcd.clone())
        {
            Some(t) => t,
            None => return Ok(None),
        };
        let offset = match self.sample_offset(track_index, sample_idx) {
            Some(o) => o,
            None => return Ok(None),
        };
        let size = match self
            .tracks
            .get(track_index)
            .and_then(|t| t.sample_table.sample_size_at(sample_idx))
        {
            Some(s) if s >= 4 => s.min(16) as usize,
            // A timecode sample is 4 bytes; anything shorter is malformed.
            _ => return Ok(None),
        };
        self.input.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; size];
        self.input.read_exact(&mut buf)?;
        Ok(Some(tmcd.decode_sample(&buf)?))
    }

    /// Resolve the **starting timecode** governing `track_index`.
    ///
    /// QTFF p. 222 ("Creating a Timecode Track…"): a timecode track holds
    /// one sample per contiguous source-tape segment, and that sample
    /// carries the timecode value at the *start* of the segment. Media
    /// tracks (video/audio) point at their timecode track with a
    /// `tref/tmcd` reference.
    ///
    /// This resolves the governing timecode track — `track_index` itself
    /// when it is a timecode track, otherwise its first `tref/tmcd`
    /// target ([`Self::timecode_track_index`]) — reads that track's sample
    /// 0, and returns the decoded value bundled with the timecode-system
    /// fields needed to interpret it ([`StartTimecode`]). Returns `None`
    /// when no timecode track governs `track_index` or its sample 0 is
    /// missing; `Err` only on an I/O failure.
    pub fn start_timecode(&mut self, track_index: usize) -> Result<Option<StartTimecode>> {
        // Pick the timecode track: self if it is one, else the tref/tmcd
        // target.
        let tc_track = match self.tracks.get(track_index) {
            Some(t) if t.is_timecode() => track_index,
            Some(_) => match self.timecode_track_index(track_index) {
                Some(i) => i,
                None => return Ok(None),
            },
            None => return Ok(None),
        };
        let tmcd = match self
            .tracks
            .get(tc_track)
            .and_then(|t| t.sample_descriptions.first())
            .and_then(|d| d.tmcd.clone())
        {
            Some(t) => t,
            None => return Ok(None),
        };
        let sample = match self.timecode_sample(tc_track, 0)? {
            Some(s) => s,
            None => return Ok(None),
        };
        Ok(Some(StartTimecode {
            timecode_track_index: tc_track,
            number_of_frames: tmcd.number_of_frames,
            time_scale: tmcd.time_scale,
            frame_duration: tmcd.frame_duration,
            drop_frame: tmcd.is_drop_frame(),
            sample,
        }))
    }

    /// Total byte extent of 1-based chunk `chunk_1based` on
    /// `track_index` — as `(start, end_exclusive)` absolute file
    /// coordinates — for chunk-aligned prefetch / HTTP-range reads
    /// (QTFF p. 74 "Chunks ... allow optimized data access"). Returns
    /// `None` when `track_index` is out of range, the chunk number
    /// is out of range, or any per-sample size lookup fails.
    pub fn chunk_byte_extent(&self, track_index: usize, chunk_1based: u32) -> Option<(u64, u64)> {
        self.tracks
            .get(track_index)?
            .sample_table
            .chunk_byte_extent(chunk_1based)
    }

    /// Look up the alternative sync sample for a shadowed sample via the
    /// `stsh` (Shadow Sync Sample Box, ISO/IEC 14496-12 §8.6.3).
    ///
    /// `shadowed_sample_number` is **1-based** (matching the `stss`
    /// numbering convention this box shares). Returns the 1-based
    /// `sync_sample_number` whose media data substitutes for the
    /// shadowed sample when a sync sample is needed at, or before, it —
    /// or `None` when the track carries no `stsh` box, or no entry
    /// shadows exactly that sample. The shadow sync sample *replaces*
    /// the shadowed one: after substitution the next sample sent is
    /// `shadowed_sample_number + 1` (§8.6.3.1). This is optional
    /// seeking metadata; a track plays and seeks correctly without it.
    pub fn shadow_sync_sample(
        &self,
        track_index: usize,
        shadowed_sample_number: u32,
    ) -> Option<u32> {
        self.tracks
            .get(track_index)?
            .sample_table
            .shadow_sync_for(shadowed_sample_number)
    }

    /// Look up the sub-sample structure of a **1-based** `sample_number`
    /// via the `subs` (Sub-Sample Information Box, ISO/IEC 14496-12
    /// §8.7.7). A sub-sample is a contiguous byte range of the sample
    /// whose precise meaning (e.g. NAL-unit boundaries for AVC/HEVC) is
    /// defined by the coding system named in the sample description.
    ///
    /// Returns `None` when the track carries no `subs` box, or when this
    /// sample is not named by any row (it has no sub-sample structure).
    /// A sample explicitly listed with zero sub-samples returns
    /// `Some(&[])`. This is optional metadata: a track decodes correctly
    /// when it is ignored.
    pub fn sub_samples(
        &self,
        track_index: usize,
        sample_number: u32,
    ) -> Option<&[crate::sample_table::SubSampleEntry]> {
        self.tracks
            .get(track_index)?
            .sample_table
            .sub_samples_for(sample_number)
    }

    /// Look up the Sample Auxiliary Information `(saiz, saio)` pair
    /// for `track_index` identified by the discriminator pair
    /// `(aux_info_type, aux_info_type_parameter)`, per ISO/IEC
    /// 14496-12 §8.7.8 / §8.7.9.
    ///
    /// Either side may be `None` (an `saiz` without a paired `saio`
    /// is invalid per §8.7.8.1 but writers occasionally emit one and
    /// not the other, so this returns them independently). Both
    /// `None` when the track has no sample-aux information matching
    /// the discriminator, or when the track index is out of range.
    ///
    /// Per §8.7.8.1 boxes whose `flags & 1` bit is unset (no on-disk
    /// discriminator) match an `aux_info_type` of `b"\0\0\0\0"` and
    /// `aux_info_type_parameter == 0` — callers should pre-resolve
    /// the implicit discriminator (CENC `scheme_type` for protected
    /// content, sample-entry FourCC otherwise) before calling here
    /// when the box's discriminator was implicit.
    ///
    /// This surface targets the `stbl`-scope (non-fragmented) form
    /// only. Fragmented streams carry `saiz` / `saio` at `traf`
    /// scope per §8.7.8.1 / §8.7.9.1; query those through
    /// [`Self::fragment_sample_aux_info`].
    pub fn sample_aux_info(
        &self,
        track_index: usize,
        aux_info_type: &[u8; 4],
        aux_info_type_parameter: u32,
    ) -> (
        Option<&crate::sample_aux::Saiz>,
        Option<&crate::sample_aux::Saio>,
    ) {
        match self.tracks.get(track_index) {
            None => (None, None),
            Some(t) => t
                .sample_table
                .sample_aux_for(aux_info_type, aux_info_type_parameter),
        }
    }

    /// Look up the Sample Auxiliary Information `(saiz, saio)` pair
    /// for `track_index` per fragment, identified by the discriminator
    /// pair `(aux_info_type, aux_info_type_parameter)`, at `traf` scope
    /// per ISO/IEC 14496-12 §8.7.8.1 / §8.7.9.1.
    ///
    /// Returns a slice of [`crate::sample_aux::FragmentSampleAux`] (one
    /// entry per fragment of this track that ships any sample-aux
    /// boxes); use [`crate::sample_aux::FragmentSampleAux::lookup`] on
    /// each entry to extract the `(saiz, saio)` matching the requested
    /// discriminator. Entries are returned in on-disk fragment order.
    ///
    /// The fragmented surface is intentionally a slice rather than a
    /// single `(saiz, saio)` pair: §8.8 fragments are independent
    /// per-fragment slabs of sample-aux data (e.g. CMAF / DASH-live
    /// CENC streams carry one sample-aux slab per fragment, each
    /// covering only that fragment's samples). Callers iterate to
    /// build a cross-fragment view.
    ///
    /// Empty slice when the track has no `traf`-scope sample-aux
    /// records, when the track index is out of range, or for non-
    /// fragmented streams. Stub-fragments that ship `saiz` / `saio`
    /// for a discriminator not matched by the requested pair are
    /// surfaced too — callers that want only matching fragments should
    /// filter by [`crate::sample_aux::FragmentSampleAux::lookup`]
    /// returning a non-`None` pair.
    pub fn fragment_sample_aux_info(
        &self,
        track_index: usize,
    ) -> &[crate::sample_aux::FragmentSampleAux] {
        match self.tracks.get(track_index) {
            None => &[],
            Some(t) => &t.fragment_sample_aux,
        }
    }

    /// Inner implementation of [`Demuxer::seek_to`]. Lives on the
    /// struct (not the trait impl) so it's reachable from the
    /// standalone (no-`registry`) build's tests too without needing
    /// the `Demuxer` trait in scope.
    ///
    /// `pts` is in the stream's mdhd timescale ticks (QTFF p. 56);
    /// the stbl sub-tables (`stts`/`stss`) speak the same unit, so
    /// no rescaling is required.
    ///
    /// Reports the actual landed *decode* timestamp (DTS), matching
    /// the value `next_packet()` will surface in `Packet.dts`. We
    /// chose DTS over composition PTS because B-frame-heavy video
    /// reorders display order — `next_packet().pts` may exceed the
    /// caller's request even though decode flow is correct. Reporting
    /// DTS lets the pipeline trust `seek_to`'s return as a
    /// "next packet's dts will equal this" contract.
    #[cfg(feature = "registry")]
    pub(crate) fn seek_to_impl(&mut self, stream_index: u32, pts: i64) -> Result<i64> {
        if !self.apply_edits {
            return self.seek_media_impl(stream_index, pts);
        }
        // Applied edit-list mode: `pts` is an **edited-timeline**
        // timestamp (what `next_packet()` emits in this mode), so
        // resolve it back to a media timestamp through the edit list
        // (QTFF pp. 46–48 / ISO/IEC 14496-12 §8.6.6) before driving
        // the media-time seek machinery. Edited timestamps with no
        // media correspondence (inside an empty-edit window, before
        // the first presented segment, or past the last) clamp to the
        // nearest presented media tick.
        let media_target = self.edited_pts_to_media_pts(stream_index as usize, pts);
        let raw_dts = self.seek_media_impl(stream_index, media_target)?;
        // Honour the "next packet's dts will equal this" contract on
        // the edited timeline: the landed sync sample (or samples
        // right after it) may itself be dropped by the edit list, so
        // report the edited dts of the first post-seek sample of this
        // stream that the applied mode will actually emit.
        for (sidx, s) in &self.samples[self.next..] {
            if *sidx != stream_index {
                continue;
            }
            if let Some(t) = self.edited_timing_for(stream_index as usize, s) {
                return Ok(t.dts);
            }
        }
        // Every remaining sample is dropped by the edit list; the
        // next read returns Eof, so the raw landing is as good a
        // report as any.
        Ok(raw_dts)
    }

    /// Resolve an **edited-timeline** timestamp (media-timescale
    /// ticks, the applied-mode packet contract) to the media
    /// timestamp that presents at that instant. Out-of-presentation
    /// inputs clamp to the nearest presented media tick: an edited
    /// time inside an empty-edit window (or before the first
    /// presented segment) resolves to the *next* presented segment's
    /// media start, and a past-the-end time resolves to the last
    /// presented segment's final media tick. Returns `pts.max(0)`
    /// unchanged when the track has no resolvable timeline (missing
    /// `mvhd` or zero timescales).
    pub fn edited_pts_to_media_pts(&self, track_index: usize, pts: i64) -> i64 {
        let fallback = pts.max(0);
        let Some(track) = self.tracks.get(track_index) else {
            return fallback;
        };
        let Some(mvhd) = self.mvhd.as_ref() else {
            return fallback;
        };
        let mds = track.mdhd.time_scale;
        let mvs = mvhd.time_scale;
        if mds == 0 || mvs == 0 {
            return fallback;
        }
        // Edited media ticks → movie ticks (half-up), the inverse of
        // the applied mode's movie→media rescale.
        let movie_pts = ((fallback as i128 * mvs as i128 + mds as i128 / 2) / mds as i128) as i64;
        let owned;
        let segments: &[crate::EditSegment] = match self.edited_segments.get(track_index) {
            Some(segs) if self.edited_segments.len() == self.tracks.len() => segs,
            _ => {
                owned = track.edit_segments(mvs, Some(mvhd.duration));
                &owned
            }
        };
        if let Some(m) = crate::edit::movie_pts_to_media_pts(segments, movie_pts, mvs, mds) {
            return m;
        }
        // No direct correspondence — clamp. First presented segment
        // whose window ends after the requested movie tick wins (the
        // player would next show its head); otherwise fall back to
        // the end of the last presented segment.
        const RATE_ONE: i128 = 0x0001_0000;
        let media_span = |seg: &crate::EditSegment, rate_fp: i128| -> i128 {
            let dur_movie = seg.movie_time_end as i128 - seg.movie_time_start as i128;
            let num = dur_movie * mds as i128 * rate_fp;
            let denom = mvs as i128 * RATE_ONE;
            (num + denom / 2) / denom
        };
        for seg in segments {
            if (seg.movie_time_end as i128) <= movie_pts as i128 {
                continue;
            }
            match seg.kind {
                crate::EditSegmentKind::Empty => continue,
                crate::EditSegmentKind::Dwell { media_time } => return media_time as i64,
                crate::EditSegmentKind::Media {
                    media_time_start,
                    media_rate,
                } => {
                    if media_rate <= 0 {
                        continue;
                    }
                    return media_time_start as i64;
                }
            }
        }
        for seg in segments.iter().rev() {
            match seg.kind {
                crate::EditSegmentKind::Empty => continue,
                crate::EditSegmentKind::Dwell { media_time } => return media_time as i64,
                crate::EditSegmentKind::Media {
                    media_time_start,
                    media_rate,
                } => {
                    if media_rate <= 0 {
                        continue;
                    }
                    let span = media_span(seg, media_rate as i128).max(1);
                    return (media_time_start as i128 + span - 1) as i64;
                }
            }
        }
        fallback
    }

    /// Media-timeline seek core (the historical `seek_to` semantics;
    /// see [`MovDemuxer::seek_to_impl`] for the applied edit-list
    /// wrapper).
    #[cfg(feature = "registry")]
    fn seek_media_impl(&mut self, stream_index: u32, pts: i64) -> Result<i64> {
        // 1. Range + media-type gate.
        let idx = stream_index as usize;
        let track = self.tracks.get(idx).ok_or_else(|| {
            Error::invalid(format!("MOV: stream index {stream_index} out of range"))
        })?;
        if !track.is_video() && !track.is_audio() {
            return Err(Error::invalid(format!(
                "MOV: stream {stream_index} is neither video nor audio; can't seek"
            )));
        }

        // 2. Fragmented MP4: route through the `tfra`-indexed seek
        // path. `tracks[stream].fragment_samples` was flattened into
        // `self.samples` at open time, so once we pick the right
        // sample we can re-use the same "snap the queue cursor"
        // mechanism as the non-fragmented branch. The pre-condition
        // is that a `tfra` index exists for the requested track — the
        // index gives us O(log N) random access without walking every
        // `moof` from `moov` forwards.
        if self.is_fragmented() {
            return self.seek_to_fragmented(stream_index, pts);
        }

        // 3. Walk the flattened sample queue, filtering by stream.
        //    Find the largest sync sample whose `dts <= pts`. The
        //    queue is already sorted by file offset, but per-track
        //    sample-index ordering matches decode order (chunks lay
        //    out samples sequentially), so the first such match per
        //    track also has monotonically increasing dts.
        //
        //    Past-end: when no sync sample has `dts <= pts`, fall
        //    back to the first sync sample in the track (typically
        //    sample 0). Past-start: when `pts` is negative or
        //    smaller than the first sample's dts, the first matching
        //    keyframe is still the best landing.
        let target_dts: i64 = pts.max(0);
        let mut best_cursor: Option<usize> = None;
        let mut best_dts: i64 = i64::MIN;
        for (i, (sidx, s)) in self.samples.iter().enumerate() {
            if *sidx != stream_index {
                continue;
            }
            if !s.keyframe {
                continue;
            }
            let s_dts = s.dts as i64;
            if s_dts <= target_dts && s_dts >= best_dts {
                best_cursor = Some(i);
                best_dts = s_dts;
            }
        }
        if best_cursor.is_none() {
            // No keyframe at-or-before target. Land on the *first*
            // keyframe of this stream (the spec guarantees sample 0
            // is implicitly a sync sample whenever `stss` is empty;
            // when `stss` is populated, the first listed entry is
            // sample 1).
            for (i, (sidx, s)) in self.samples.iter().enumerate() {
                if *sidx == stream_index && s.keyframe {
                    best_cursor = Some(i);
                    best_dts = s.dts as i64;
                    break;
                }
            }
        }
        let cursor = best_cursor.ok_or_else(|| {
            Error::unsupported(format!(
                "MOV: stream {stream_index} has no sync samples to seek to"
            ))
        })?;
        self.next = cursor;
        Ok(best_dts)
    }

    /// Fragmented-MP4 seek path — companion to [`Self::seek_to_impl`]
    /// when [`Self::is_fragmented`] is true.
    ///
    /// Algorithm per ISO/IEC 14496-12 §8.8.10 ("Track Fragment Random
    /// Access Box"):
    ///
    /// 1. Look up the target track's `tfra` index. If absent, fall
    ///    back to walking the flattened `self.samples` queue for the
    ///    largest sync sample at-or-before `pts`. The fallback works
    ///    because round-18's open-time `moof` walker already
    ///    materialised every fragment's samples into the queue —
    ///    only the *random-access* shortcut is missing without `tfra`.
    /// 2. With `tfra` present, binary-search the entries for the
    ///    largest `time <= target_pts` (saturating to entry 0 on
    ///    past-start, to the last entry on past-end). Each entry's
    ///    `time` is the *presentation* (composition) time of the sync
    ///    sample in the track's `mdhd.time_scale` per §8.8.10.3.
    /// 3. Locate the matching sample in `self.samples`. We match on
    ///    `sample.pts() == entry.time` (PTS = DTS +
    ///    `composition_offset`) and snap `self.next`.
    ///
    /// Returns the actual landed DTS of the chosen sync sample
    /// (matching `next_packet().dts` for the post-seek read), even
    /// though the tfra entry input is keyed on PTS. The DTS-return
    /// contract matches the non-fragmented branch above.
    #[cfg(feature = "registry")]
    fn seek_to_fragmented(&mut self, stream_index: u32, pts: i64) -> Result<i64> {
        let track_id = self
            .tracks
            .get(stream_index as usize)
            .map(|t| t.tkhd.track_id)
            .ok_or_else(|| Error::invalid("MOV: fragmented seek track index out of range"))?;
        let tfra = self.tfra_indexes.iter().find(|t| t.track_id == track_id);
        let target_pts: i64 = pts.max(0);

        // Sub-routine: scan `self.samples` for the sync sample whose
        // `pts <= target_pts` and is closest. Falls back to the first
        // sync sample if none qualifies. Returns `(cursor, dts)` so
        // the caller can report DTS even though the comparison is on
        // PTS.
        let snap_to_sync = |samples: &[(u32, SampleEntry)], target: i64| -> Option<(usize, i64)> {
            let mut best: Option<(usize, i64, i64)> = None; // (cursor, pts, dts)
            for (i, (sidx, s)) in samples.iter().enumerate() {
                if *sidx != stream_index || !s.keyframe {
                    continue;
                }
                let s_pts = s.pts();
                let s_dts = s.dts as i64;
                if s_pts <= target {
                    match best {
                        Some((_, bp, _)) if bp >= s_pts => {}
                        _ => best = Some((i, s_pts, s_dts)),
                    }
                }
            }
            if best.is_none() {
                for (i, (sidx, s)) in samples.iter().enumerate() {
                    if *sidx == stream_index && s.keyframe {
                        return Some((i, s.dts as i64));
                    }
                }
                return None;
            }
            best.map(|(c, _, d)| (c, d))
        };

        if let Some(t) = tfra {
            if t.entries.is_empty() {
                // Empty tfra (legal per spec, useless in practice) →
                // fall through to the generic queue scan.
                return self.seek_fragmented_queue_scan(stream_index, pts, snap_to_sync);
            }
            // §8.8.10.3: "the entries are stored in increasing order of
            // time" — binary search for the largest entry whose time
            // is <= target.
            let target_u: u64 = target_pts as u64;
            let pp = t.entries.partition_point(|e| e.time <= target_u);
            let pick = if pp == 0 {
                // Target precedes the first tfra entry — land on
                // entry 0 (first sync sample available).
                0
            } else {
                pp - 1
            };
            let entry = t.entries[pick];
            // Locate the sample in `self.samples` by matching the
            // tfra entry's presentation time against
            // `SampleEntry::pts()`. Spec-compliant tfra writers emit
            // one entry per sync sample with the PTS in the track's
            // `mdhd.time_scale`, so the match is exact.
            let mut hit: Option<(usize, i64)> = None;
            for (i, (sidx, s)) in self.samples.iter().enumerate() {
                if *sidx != stream_index || !s.keyframe {
                    continue;
                }
                if s.pts() == entry.time as i64 {
                    hit = Some((i, s.dts as i64));
                    break;
                }
            }
            // Spec-deviating files: writers occasionally drift the
            // tfra time off by a duration tick. Fall back to the
            // generic snap-to-sync scan so we still land *somewhere*
            // sensible instead of erroring.
            let (cursor, landed) = match hit {
                Some(v) => v,
                None => snap_to_sync(&self.samples, target_pts).ok_or_else(|| {
                    Error::unsupported(format!(
                        "MOV: fragmented stream {stream_index} has no sync sample matching tfra \
                         entry time={t}",
                        t = entry.time
                    ))
                })?,
            };
            self.next = cursor;
            Ok(landed)
        } else {
            // No tfra for this track — generic queue scan over the
            // round-18 fragment_samples union.
            self.seek_fragmented_queue_scan(stream_index, pts, snap_to_sync)
        }
    }

    /// Fragmented seek without a `tfra` index — falls back to a linear
    /// scan of `self.samples`. Slower than the indexed path but works
    /// for files whose authoring tool omitted `mfra` (bad practice
    /// per §8.8.9 but seen in the wild).
    #[cfg(feature = "registry")]
    fn seek_fragmented_queue_scan<F>(
        &mut self,
        stream_index: u32,
        pts: i64,
        snap_to_sync: F,
    ) -> Result<i64>
    where
        F: Fn(&[(u32, SampleEntry)], i64) -> Option<(usize, i64)>,
    {
        let target_pts: i64 = pts.max(0);
        let (cursor, landed) = snap_to_sync(&self.samples, target_pts).ok_or_else(|| {
            Error::unsupported(format!(
                "MOV: fragmented stream {stream_index} has no sync samples to seek to"
            ))
        })?;
        self.next = cursor;
        Ok(landed)
    }
}

/// Build an "unsupported" error in a way that works under both the
/// `registry` (uses `oxideav_core::Error::unsupported`) and standalone
/// (uses our local `Error::Unsupported`) builds.
fn unsupported_error(msg: impl Into<String>) -> Error {
    #[cfg(feature = "registry")]
    {
        Error::unsupported(msg)
    }
    #[cfg(not(feature = "registry"))]
    {
        Error::unsupported(msg)
    }
}

/// Built-in `file://` URL opener for [`MovDemuxer::open_with_aliases`].
///
/// Resolves a `file://`-scheme URL to a local-filesystem
/// `std::fs::File` and wraps it in a `Box<dyn ReadSeek>` for the
/// alias-resolver. URLs that don't start with `file://` (and the
/// degenerate `file:` shape used by some legacy authoring tools) are
/// rejected with [`std::io::ErrorKind::Unsupported`] so the alias
/// chain falls through to the next alternate rather than fail the
/// whole open.
///
/// The resolver does **not** try to interpret `host` parts: only
/// `file:///absolute/path`, `file://localhost/absolute/path`, and the
/// legacy `file:relative-or-absolute` forms are honoured. URL-encoded
/// characters (`%20`, etc.) are decoded byte-by-byte before being
/// fed to the filesystem, which matches the behaviour of macOS
/// QuickTime Player on alias-chain resolution. Multi-byte UTF-8
/// percent-encoded path components are forwarded verbatim — we don't
/// re-encode after decoding.
///
/// **Platform notes**:
///
/// * Unix: `file:///abs/path` and `file://localhost/abs/path` resolve
///   directly to the absolute filesystem path.
/// * Windows: `file:///C:/path` and `file:///C|/path` (legacy bar
///   shape) resolve to `C:\path` — the parser strips the leading `/`
///   that the URL form requires before the drive letter and
///   normalises forward slashes inside the path component to
///   backslashes. UNC shapes (`file://server/share/path`) are
///   rejected because they would silently cross network boundaries
///   the user didn't authorise; bring your own opener for those.
///   Drive letters are recognised case-insensitively (`a..z` /
///   `A..Z`).
///
/// Wire this in via:
///
/// ```ignore
/// use oxideav_mov::{open_file_url, MovDemuxer};
/// let f = std::fs::File::open("/path/to/local-aliases.mov")?;
/// let dem = MovDemuxer::open_with_aliases(Box::new(f), open_file_url)?;
/// ```
///
/// The opener is intentionally a free function (rather than a default
/// argument on `open_with_aliases`) so callers who only want
/// in-memory aliases pay nothing for it; consumers who want the
/// "common local-aliases case" pull it in explicitly.
pub fn open_file_url(url: &str) -> std::io::Result<Box<dyn ReadSeek>> {
    let path = file_url_to_path(url).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("MOV: open_file_url: not a local file:// URL: '{url}'"),
        )
    })?;
    let f = std::fs::File::open(&path)?;
    Ok(Box::new(f))
}

/// Decode a `file://`-scheme URL into a filesystem path, returning
/// `None` when the URL doesn't fit any of the recognised shapes:
///
/// * `file:///absolute/path`
/// * `file://localhost/absolute/path` (`localhost` host stripped)
/// * `file:absolute-or-relative-path` (legacy QuickTime alias shape)
///
/// Any URL with a non-empty, non-`localhost` host is rejected so
/// callers don't accidentally read from a network mount that the user
/// didn't authorise.
///
/// On Windows the leading `/` before a drive letter is stripped:
/// `file:///C:/Users/foo` becomes `C:\Users\foo`. The legacy bar
/// shape `file:///C|/Users/foo` is also accepted (the `|` is replaced
/// by `:`). Forward slashes inside the path are converted to
/// backslashes.
fn file_url_to_path(url: &str) -> Option<std::path::PathBuf> {
    // Lowercase scheme match (URL schemes are case-insensitive per
    // RFC 3986 §3.1).
    let rest = if url.len() >= 5 && url[..5].eq_ignore_ascii_case("file:") {
        &url[5..]
    } else {
        return None;
    };
    // Three shapes:
    //   file:///abs              → rest = "//"  + "/abs"
    //   file://host/abs          → rest = "//host/abs"
    //   file:rel-or-abs          → rest = "rel-or-abs"
    let path_str = if let Some(after_slashes) = rest.strip_prefix("//") {
        // Authority + path. Host must be empty or "localhost".
        let slash = after_slashes.find('/').unwrap_or(after_slashes.len());
        let host = &after_slashes[..slash];
        if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
            return None;
        }
        // Path is everything from the first slash onwards. Note that
        // when host is empty, `slash` == 0, so path_str starts with
        // a leading '/'.
        if slash >= after_slashes.len() {
            return None;
        }
        &after_slashes[slash..]
    } else {
        rest
    };
    // Percent-decode the path (defensive — the writer might URL-encode
    // spaces or special characters).
    let decoded = percent_decode_to_bytes(path_str)?;
    let s = String::from_utf8(decoded).ok()?;
    Some(std::path::PathBuf::from(normalise_path_for_target_os(&s)))
}

/// Per-target-OS path normalisation. On Windows, `file:///C:/foo`
/// arrives at this layer as `/C:/foo`; we strip the leading `/`
/// before the drive letter, accept the legacy `|` drive-letter
/// separator (RFC 8089 Appendix E.2), and flip forward slashes to
/// backslashes so the resulting `PathBuf` opens cleanly through the
/// Windows path APIs. On non-Windows targets the input is returned
/// unchanged.
fn normalise_path_for_target_os(s: &str) -> String {
    if cfg!(windows) {
        normalise_path_for_windows(s)
    } else {
        s.to_string()
    }
}

/// Pure helper exposed for cross-platform testing of the Windows
/// path-conversion rules even when the test host is Unix. The Unix
/// build never calls this on the live `file://` path, but it keeps
/// the rules verifiable in CI without requiring a Windows runner.
fn normalise_path_for_windows(s: &str) -> String {
    let bytes = s.as_bytes();
    // Detect a leading `/X:` or `/X|` shape (drive letter at pos 1,
    // separator at pos 2). When present, strip the leading `/` and
    // (later) replace `|` with `:`. The drive letter can be either
    // case.
    let mut start = 0usize;
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() {
        let sep = bytes[2];
        if sep == b':' || sep == b'|' {
            start = 1;
        }
    }
    let mut out = String::with_capacity(s.len() - start);
    for (i, ch) in s[start..].chars().enumerate() {
        // Flip the legacy `|` to `:` when it sits in the drive-
        // letter slot (index 1 of the trimmed string).
        if i == 1 && ch == '|' {
            out.push(':');
        } else if ch == '/' {
            out.push('\\');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Minimal RFC 3986 percent-decoder for the `file://` opener — accepts
/// `%XX` (uppercase or lowercase hex) and passes everything else
/// through. Returns `None` on a malformed `%` escape rather than
/// silently letting it through, matching the strict behaviour the
/// HTTP/file URL parsers in `url` and `percent-encoding` crates apply
/// (we don't pull either to keep this crate dep-free).
fn percent_decode_to_bytes(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_digit(bytes[i + 1])?;
            let lo = hex_digit(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    Some(out)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(feature = "registry")]
fn build_streams(tracks: &[Track], resolver: &dyn CodecResolver) -> Vec<StreamInfo> {
    let mut out = Vec::with_capacity(tracks.len());
    for (i, t) in tracks.iter().enumerate() {
        let mut params = if t.is_video() {
            CodecParameters::video(CodecId::new("unknown"))
        } else if t.is_audio() {
            CodecParameters::audio(CodecId::new("unknown"))
        } else {
            CodecParameters::data(CodecId::new("unknown"))
        };
        if let Some(format) = t.primary_format() {
            let tag = CodecTag::fourcc(&format);
            let mut ctx = ProbeContext::new(&tag);
            if let Some(desc) = t.sample_descriptions.first() {
                ctx = ctx.header(&desc.extra);
                if t.is_audio() {
                    ctx = ctx
                        .channels(desc.channels)
                        .bits(desc.bits_per_sample)
                        .sample_rate(desc.sample_rate);
                } else if t.is_video() {
                    ctx = ctx.width(desc.width as u32).height(desc.height as u32);
                }
            }
            if let Some(id) = resolver.resolve_tag(&ctx) {
                params.codec_id = id;
            }
            params = params.with_tag(tag);
            if t.is_audio() {
                if let Some(desc) = t.sample_descriptions.first() {
                    params.channels = Some(desc.channels);
                    params.sample_rate = Some(desc.sample_rate);
                    if !desc.extra.is_empty() {
                        params.extradata = desc.extra.clone();
                    }
                }
            } else if t.is_video() {
                if let Some(desc) = t.sample_descriptions.first() {
                    if desc.width != 0 {
                        params.width = Some(desc.width as u32);
                    }
                    if desc.height != 0 {
                        params.height = Some(desc.height as u32);
                    }
                    if !desc.extra.is_empty() {
                        params.extradata = desc.extra.clone();
                    }
                }
            }
        }
        let timescale = if t.mdhd.time_scale > 0 {
            t.mdhd.time_scale as i64
        } else {
            // Fallback per QTFF p. 56 — the media must declare a
            // non-zero time_scale; we degrade to 1 to keep the
            // pipeline running rather than reject the whole file.
            1
        };
        out.push(StreamInfo {
            index: i as u32,
            time_base: TimeBase::new(1, timescale),
            duration: Some(t.mdhd.duration as i64),
            start_time: Some(0),
            params,
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
/// Decompress a `cmov` atom body (QTFF pp. 80 – 81) and validate the
/// result is a complete movie resource — per QTFF p. 30 the
/// decompressed contents are the standard uncompressed structure,
/// whose outermost atom is `moov`. Returns the `dcom` algorithm
/// FourCC, the decompressed buffer, and the inner `moov`'s header
/// (offsets relative to the start of the buffer) so the caller can
/// re-enter the regular `moov` walk over an in-memory cursor.
fn decompress_cmov_body(body: &[u8]) -> Result<([u8; 4], Vec<u8>, AtomHeader)> {
    let cmov = parse_cmov(body)?;
    // Same in-memory ceiling as `read_payload` (round 162): the
    // decompressed movie resource is held in RAM in full, so a
    // declared uncompressed size past the crate-wide cap is rejected
    // before the decoder allocates toward it.
    if cmov.cmvd.uncompressed_size as u64 > MAX_INMEMORY_ATOM_BODY {
        return Err(Error::invalid(format!(
            "MOV: cmvd declares a {} byte movie resource, over the {MAX_INMEMORY_ATOM_BODY} \
             byte in-memory cap",
            cmov.cmvd.uncompressed_size
        )));
    }
    let decoded = cmov.decompress()?;
    let mut cur = Cursor::new(&decoded);
    let inner_hdr = read_atom_header(&mut cur)?.ok_or_else(|| {
        Error::invalid("MOV: decompressed movie resource too short for an atom header (QTFF p. 80)")
    })?;
    if inner_hdr.fourcc != MOOV {
        return Err(Error::invalid(format!(
            "MOV: decompressed movie resource starts with '{}', expected 'moov' (QTFF p. 30)",
            inner_hdr.type_str()
        )));
    }
    // Inner injection-robustness check, mirroring the top-level walk:
    // a forged size word on the decompressed moov must not declare a
    // body past the end of the buffer.
    let inner_end = inner_hdr
        .total_size
        .map(|t| inner_hdr.payload_offset + (t - inner_hdr.header_len))
        .unwrap_or(decoded.len() as u64);
    if inner_end > decoded.len() as u64 {
        return Err(Error::invalid(format!(
            "MOV: decompressed moov declares body end {inner_end} past the {} byte movie \
             resource",
            decoded.len()
        )));
    }
    Ok((cmov.dcom.algorithm, decoded, inner_hdr))
}

#[allow(clippy::too_many_arguments)]
fn parse_moov<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    body_end: u64,
    mvhd: &mut Option<Mvhd>,
    tracks: &mut Vec<Track>,
    meta: &mut Vec<MetaKeyValue>,
    user_data: &mut Vec<UserDataEntry>,
    reference_movies: &mut Vec<ReferenceMovie>,
    bmff_meta: &mut Option<BmffMeta>,
    mehd_out: &mut Option<Mehd>,
    trex_out: &mut Vec<TrexDefaults>,
    leva_out: &mut Option<Leva>,
    ctab_out: &mut Option<Ctab>,
    clipping_out: &mut Option<Clipping>,
    cmov_alg_out: &mut Option<[u8; 4]>,
    allow_cmov: bool,
) -> Result<()> {
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &CMOV => {
                // QTFF pp. 80 – 81 compressed movie resource. The
                // on-disk `moov` of a compressed-movie file carries a
                // single `cmov` child; decompressing it yields the
                // complete uncompressed movie resource (a full `moov`
                // atom, QTFF p. 30), which re-enters this same walk
                // over an in-memory cursor so every downstream field
                // (tracks, meta, mvex, …) populates exactly as for an
                // uncompressed file.
                if !allow_cmov {
                    // QTFF p. 30: "When this child atom is
                    // uncompressed, its contents conform to the
                    // structure shown in the following illustration"
                    // — the standard *uncompressed* layout. A second
                    // compression layer is not spec-conformant and
                    // would otherwise allow unbounded recursion.
                    return Err(Error::invalid(
                        "MOV: nested cmov inside a decompressed movie resource (QTFF p. 30 \
                         allows a single compression layer)",
                    ));
                }
                let body = read_payload(r, child)?;
                let (algorithm, decoded, inner_hdr) = decompress_cmov_body(&body)?;
                let inner_end = inner_hdr
                    .total_size
                    .map(|t| inner_hdr.payload_offset + (t - inner_hdr.header_len))
                    .unwrap_or(decoded.len() as u64);
                let mut cur = Cursor::new(decoded);
                parse_moov(
                    &mut cur,
                    &inner_hdr,
                    inner_end,
                    mvhd,
                    tracks,
                    meta,
                    user_data,
                    reference_movies,
                    bmff_meta,
                    mehd_out,
                    trex_out,
                    leva_out,
                    ctab_out,
                    clipping_out,
                    cmov_alg_out,
                    false,
                )?;
                *cmov_alg_out = Some(algorithm);
            }
            t if t == &MVHD => {
                let body = read_payload(r, child)?;
                *mvhd = Some(parse_mvhd(&body)?);
            }
            t if t == &TRAK => {
                let track = parse_trak(r, child)?;
                tracks.push(track);
            }
            t if t == &META => {
                // Try Apple shape first; fall back to ISO BMFF §8.11
                // shape when the Apple parser declines.
                if let Some(kv) = parse_meta_atom(r, child)? {
                    *meta = kv;
                } else if let Some(b) = parse_bmff_meta(r, child)? {
                    *bmff_meta = Some(b);
                }
            }
            t if t == &UDTA => {
                let body = read_payload(r, child)?;
                *user_data = parse_udta(&body)?;
            }
            t if t == &RMRA => {
                *reference_movies = parse_rmra(r, child)?;
            }
            t if t == &CTAB => {
                // QTFF p. 35 — at most one `ctab` per movie. Keep the
                // first when a malformed writer emits duplicates; the
                // spec does not define override semantics so first-wins
                // matches the conservative-merge policy applied to
                // other "at most once" movie-level atoms (mvhd, pdin).
                let body = read_payload(r, child)?;
                let parsed = parse_ctab(&body)?;
                if ctab_out.is_none() {
                    *ctab_out = Some(parsed);
                }
            }
            t if t == &CLIP => {
                // QTFF p. 43 — movie-level Clipping atom; single
                // `crgn` child (QTFF p. 44). The spec figure shows
                // one per movie; first-wins on the rare duplicate
                // case (same conservative-merge policy as mvhd /
                // pdin / ctab).
                let body = read_payload(r, child)?;
                let parsed = parse_clip(&body)?;
                if clipping_out.is_none() {
                    *clipping_out = Some(parsed);
                }
            }
            t if t == &MVEX => {
                // Movie-extends header (ISO/IEC 14496-12 §8.8.1) —
                // declares the file as fragmented. Round 18 parses
                // the optional `mehd` (total fragmented duration) and
                // the per-track `trex` defaults; round 226 adds the
                // optional `leva` (Level Assignment Box, §8.8.13)
                // pairing with the §8.16.4 `ssix` adaptive-streaming
                // index. First-wins on duplicate `leva` per the
                // §8.8.13.1 Quantity rule.
                let (mehd, trex, leva) = parse_mvex(r, child)?;
                if mehd.is_some() {
                    *mehd_out = mehd;
                }
                trex_out.extend(trex);
                if leva.is_some() && leva_out.is_none() {
                    *leva_out = leva;
                }
            }
            _ => {}
        }
        Ok(())
    })
}

/// Parse the `moov/rmra` container — a list of `rmda` descriptors,
/// each carrying a data reference plus optional qualifiers.
fn parse_rmra<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<Vec<ReferenceMovie>> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut out = Vec::new();
    walk_children(r, Some(body_end), |r, child| {
        if child.fourcc == RMDA {
            out.push(parse_rmda(r, child)?);
        }
        Ok(())
    })?;
    Ok(out)
}

fn parse_rmda<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<ReferenceMovie> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut out = ReferenceMovie::default();
    walk_children(r, Some(body_end), |r, child| {
        let payload = read_payload(r, child)?;
        match &child.fourcc {
            t if t == &RDRF => out.data_ref = Some(parse_rdrf(&payload)?),
            t if t == &RMDR && payload.len() >= 8 => {
                out.min_data_rate = Some(u32::from_be_bytes([
                    payload[4], payload[5], payload[6], payload[7],
                ]));
            }
            t if t == &RMQU && payload.len() >= 4 => {
                // `rmqu` is documented as just `[quality:4]` — no
                // FullBox prefix — but real-world writers emit both
                // shapes. We accept either by reading the trailing
                // 4 bytes when the payload is long enough.
                let off = if payload.len() >= 8 { 4 } else { 0 };
                out.quality = Some(u32::from_be_bytes([
                    payload[off],
                    payload[off + 1],
                    payload[off + 2],
                    payload[off + 3],
                ]));
            }
            t if t == &RMCS && payload.len() >= 8 => {
                out.cpu_speed = Some(u32::from_be_bytes([
                    payload[4], payload[5], payload[6], payload[7],
                ]));
            }
            t if t == &RMVC => {
                out.version_check = Some(payload.clone());
            }
            t if t == &RMCD && payload.len() >= 8 => {
                let mut fc = [0u8; 4];
                fc.copy_from_slice(&payload[4..8]);
                out.codec_check = Some(fc);
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(out)
}

fn parse_trak<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<Track> {
    let mut track = Track::default();
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;

    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &TKHD => {
                let body = read_payload(r, child)?;
                track.tkhd = parse_tkhd(&body)?;
            }
            t if t == &MDIA => {
                parse_mdia(r, child, &mut track)?;
            }
            t if t == &EDTS => {
                track.edits = parse_edts(r, child)?;
            }
            t if t == &TREF => {
                track.references = parse_tref(r, child)?;
            }
            // ISO/IEC 14496-12 §8.3.4 — Track Group Box. Container
            // whose children are FullBoxes whose FourCC is the
            // track_group_type. Quantity: Zero or one (§8.3.4.1), so
            // first-wins on the rare duplicate case (consistent with
            // `tapt` / `load` / `cslg` / `clip` / `matt` conservative-
            // merge policy at trak scope).
            t if t == &TRGR && track.track_groups.is_empty() => {
                track.track_groups = crate::track_group::parse_trgr(r, child)?;
            }
            t if t == &TAPT => {
                track.tapt = Some(parse_tapt(r, child)?);
            }
            t if t == &CLIP => {
                // QTFF p. 43 — track-level Clipping atom; single
                // `crgn` child (QTFF p. 44). Spec figure shows one
                // per track; first-wins on the rare duplicate case
                // (matches tapt / load / cslg conservative-merge
                // policy at this scope).
                let body = read_payload(r, child)?;
                let parsed = parse_clip(&body)?;
                if track.clipping.is_none() {
                    track.clipping = Some(parsed);
                }
            }
            t if t == &MATT => {
                // QTFF p. 44 — track-level Track Matte atom; single
                // `kmat` child (QTFF p. 45). Spec figure shows one
                // per track; first-wins on the rare duplicate case
                // (matches clip / tapt / load / cslg conservative-merge
                // policy at this scope). The atom is QuickTime-only;
                // ISO BMFF does not define it.
                let body = read_payload(r, child)?;
                let parsed = parse_matt(&body)?;
                if track.matte.is_none() {
                    track.matte = Some(parsed);
                }
            }
            // QTFF pp. 51–53 — track-level Track Input Map atom.
            // Container whose children are ` in` track input atoms
            // (one per `'ssrc'` reference). QTFF Figure 2-6 places
            // `imap` once per trak; first-wins on the rare duplicate
            // case (matches clip / matt / tapt / load / cslg
            // conservative-merge policy at this scope). The atom is
            // QuickTime-only; ISO BMFF does not define it.
            t if t == &IMAP && track.track_input_map.is_none() => {
                track.track_input_map = Some(crate::track_input_map::parse_imap(r, child)?);
            }
            t if t == &LOAD => {
                let body = read_payload(r, child)?;
                track.load = Some(parse_load(&body)?);
            }
            t if t == &CSLG => {
                let body = read_payload(r, child)?;
                track.cslg = Some(parse_cslg(&body)?);
            }
            t if t == &META => {
                if let Some(kv) = parse_meta_atom(r, child)? {
                    track.meta = kv;
                } else if let Some(b) = parse_bmff_meta(r, child)? {
                    track.bmff_meta = Some(b);
                }
            }
            t if t == &UDTA => {
                let body = read_payload(r, child)?;
                track.user_data = parse_udta(&body)?;
                // ISO/IEC 14496-12 §8.10.3 — `tsel` (Track Selection
                // box) lives inside track-level udta. We re-walk the
                // same buffer once to extract it as a typed surface
                // rather than leaving the raw bytes inside the flat
                // user_data list.
                track.track_selection = crate::track_selection::find_tsel_in_udta(&body)?;
                // ISO/IEC 14496-12 §8.14.3 — `strk` (Sub Track box)
                // lives inside track-level udta, `Quantity: Zero or
                // more`. Collect every sub track (its mandatory `stri`
                // plus `stsg` entries from its `strd`) as a typed
                // surface.
                track.sub_tracks = crate::sub_track::find_sub_tracks_in_udta(&body)?;
                // ISO/IEC 14496-12 §8.10.4 — `kind` (Track Kind) lives
                // inside the same track-level udta, `Quantity: Zero or
                // more`. Collect every entry as a typed surface.
                track.kinds = crate::kind::find_kinds_in_udta(&body)?;
            }
            _ => {}
        }
        Ok(())
    })?;
    // Cross-validate cslg against ctts when both are present. The ISO
    // BMFF guarantees ctts deltas fall inside [least, greatest] (§8.6.1.4);
    // a mismatch is suspicious so we surface it as `InvalidData`.
    if let Some(c) = track.cslg {
        if !track.sample_table.ctts.is_empty() {
            let mut min = i64::MAX;
            let mut max = i64::MIN;
            for e in &track.sample_table.ctts {
                let v = e.composition_offset as i64;
                if v < min {
                    min = v;
                }
                if v > max {
                    max = v;
                }
            }
            if min < c.least_decode_to_display_delta || max > c.greatest_decode_to_display_delta {
                return Err(Error::invalid(format!(
                    "MOV: ctts range [{min}, {max}] outside cslg [{}, {}]",
                    c.least_decode_to_display_delta, c.greatest_decode_to_display_delta,
                )));
            }
        }
    }
    Ok(track)
}

fn parse_edts<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<EditList> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut out = EditList::new();
    walk_children(r, Some(body_end), |r, child| {
        if child.fourcc == ELST {
            let body = read_payload(r, child)?;
            out = parse_elst(&body)?;
        }
        Ok(())
    })?;
    Ok(out)
}

fn parse_tref<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<Vec<TrackRef>> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut out = Vec::new();
    walk_children(r, Some(body_end), |r, child| {
        // Each child's payload is a tightly-packed list of u32 track ids.
        let payload = read_payload(r, child)?;
        let mut ids = Vec::with_capacity(payload.len() / 4);
        for chunk in payload.chunks_exact(4) {
            ids.push(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        out.push(TrackRef {
            kind: TrackRefKind::from_fourcc(&child.fourcc),
            fourcc: child.fourcc,
            track_ids: ids,
        });
        Ok(())
    })?;
    Ok(out)
}

fn parse_tapt<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<Tapt> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut out = Tapt::default();
    walk_children(r, Some(body_end), |r, child| {
        let body = read_payload(r, child)?;
        match &child.fourcc {
            t if t == &CLEF => out.clef = Some(parse_tapt_dims(&body)?),
            t if t == &PROF => out.prof = Some(parse_tapt_dims(&body)?),
            t if t == &ENOF => out.enof = Some(parse_tapt_dims(&body)?),
            _ => {}
        }
        Ok(())
    })?;
    Ok(out)
}

/// Parse an Apple-shaped `meta` atom. The QTFF / Apple iTunes layout
/// is `[hdlr (mdta)][keys][ilst]` (the `hdlr` may carry a different
/// 4-byte handler — we treat any handler the same way and look for a
/// `keys` table followed by an `ilst` value list). Returns `None` when
/// the atom doesn't carry the key-value structure (e.g. ISO BMFF
/// `meta` with `XMP_` / `bxml`).
///
/// QTFF documents the `meta` atom only by reference; the layout
/// surfaced here matches Apple's QuickTime developer guidance and
/// what `iTunes`/`MOV` writers emit in practice.
fn parse_meta_atom<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
) -> Result<Option<Vec<MetaKeyValue>>> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    // Apple's `meta` atom in `moov`/`trak` does NOT carry the leading
    // `[ver+flags=4]` FullBox header that ISO BMFF mandates. To stay
    // forgiving we *peek* at the next 8 bytes: if they look like a
    // valid sub-atom header (size ≥ 8 and inside body_end) we proceed
    // immediately; otherwise we skip the 4-byte FullBox header first.
    let pos_now = r.stream_position()?;
    let remain = body_end - pos_now;
    if remain >= 4 {
        let mut peek = [0u8; 8];
        if remain >= 8 {
            r.read_exact(&mut peek)?;
            r.seek(SeekFrom::Start(pos_now))?;
            let size = u32::from_be_bytes([peek[0], peek[1], peek[2], peek[3]]) as u64;
            if size < 8 || size > remain {
                // Not a valid sub-atom header — assume FullBox prefix
                // and consume 4 bytes.
                r.seek(SeekFrom::Start(pos_now + 4))?;
            }
        }
    }

    let mut keys: Vec<(String, [u8; 4])> = Vec::new();
    let mut pending_ilst: Option<Vec<u8>> = None;

    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &KEYS => {
                let body = read_payload(r, child)?;
                keys = parse_keys(&body)?;
            }
            t if t == &ILST => {
                pending_ilst = Some(read_payload(r, child)?);
            }
            _ => {}
        }
        Ok(())
    })?;

    if keys.is_empty() && pending_ilst.is_none() {
        return Ok(None);
    }
    let kv = match pending_ilst {
        Some(body) => parse_ilst(&body, &keys)?,
        None => Vec::new(),
    };
    Ok(Some(kv))
}

fn parse_mdia<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    track: &mut Track,
) -> Result<()> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &MDHD => {
                let body = read_payload(r, child)?;
                track.mdhd = parse_mdhd(&body)?;
            }
            t if t == &HDLR => {
                let body = read_payload(r, child)?;
                track.hdlr = parse_hdlr(&body)?;
            }
            t if t == &ELNG => {
                // Extended Language Tag Box (ISO/IEC 14496-12 §8.4.6) —
                // an optional `mdia` peer of the media header carrying a
                // BCP 47 language tag that overrides `mdhd.language`.
                let body = read_payload(r, child)?;
                track.extended_language = Some(parse_elng(&body)?);
            }
            t if t == &MINF => {
                parse_minf(r, child, track)?;
            }
            _ => {}
        }
        Ok(())
    })
}

fn parse_minf<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    track: &mut Track,
) -> Result<()> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &VMHD => {
                // Video Media Header Box (§12.1.2). Body has no
                // payload-affecting fields beyond what `Track::is_video`
                // already surfaces; record which media header was present.
                track.media_header_kind = MediaHeaderKind::Video;
            }
            t if t == &SMHD => {
                // Sound Media Header Box (§12.2.2). As `vmhd`, the typed
                // body carries no field we don't already surface via
                // `Track::is_audio`.
                track.media_header_kind = MediaHeaderKind::Sound;
            }
            t if t == &STHD => {
                // Subtitle Media Header Box (ISO/IEC 14496-12 §12.6.2) —
                // an empty FullBox (version 0, flags 0). Only its
                // presence classifies the track; record the kind.
                track.media_header_kind = MediaHeaderKind::Subtitle;
            }
            t if t == &NMHD => {
                // Null Media Header Box (ISO/IEC 14496-12 §8.4.5.2) —
                // an empty FullBox used by streams (e.g. timed metadata,
                // §12.3.2) for which no specific media header is defined.
                track.media_header_kind = MediaHeaderKind::Null;
            }
            t if t == &GMHD => {
                track.media_header_kind = MediaHeaderKind::Generic;
                track.gmhd = Some(parse_gmhd(r, child)?);
            }
            t if t == &HMHD => {
                // Hint Media Header Box (ISO/IEC 14496-12 §12.4.2) —
                // PDU-size / bit-rate buffering metadata for a hint
                // track. Surfaced on `Track::hmhd`.
                track.media_header_kind = MediaHeaderKind::Hint;
                let body = read_payload(r, child)?;
                track.hmhd = Some(parse_hmhd(&body)?);
            }
            t if t == &DINF => {
                parse_dinf(r, child, track)?;
            }
            t if t == &STBL => {
                parse_stbl(r, child, track)?;
            }
            _ => {}
        }
        Ok(())
    })
}

/// Parse a `gmhd` container — walks the immediate children and
/// extracts `gmin`, `text`, and `tmcd/tcmi` payloads into a [`Gmhd`].
fn parse_gmhd<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<Gmhd> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut out = Gmhd::default();
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &GMIN => {
                let body = read_payload(r, child)?;
                out.gmin = Some(parse_gmin(&body)?);
            }
            t if t == &TEXT => {
                let body = read_payload(r, child)?;
                out.text = Some(parse_text_header(&body)?);
            }
            t if t == &TMCD => {
                // `tmcd` inside `gmhd` is a container that wraps a
                // single `tcmi` child with the actual fields.
                let inner_end = child.payload_offset + child.payload_len().unwrap_or(0);
                r.seek(SeekFrom::Start(child.payload_offset))?;
                walk_children(r, Some(inner_end), |r, inner| {
                    if &inner.fourcc == b"tcmi" {
                        let body = read_payload(r, inner)?;
                        out.tcmi = Some(parse_tcmi(&body)?);
                    }
                    Ok(())
                })?;
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(out)
}

fn parse_dinf<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    track: &mut Track,
) -> Result<()> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    walk_children(r, Some(body_end), |r, child| {
        if child.fourcc == DREF {
            let body = read_payload(r, child)?;
            track.data_references = parse_dref(&body)?;
        }
        Ok(())
    })
}

/// Merge the rows of one parsed `subs` box into a track's running
/// (sample-number-sorted) table. A row whose `sample_number` already
/// exists — another `subs` box in the same `stbl` describing the same
/// sample (legal per §8.7.7.1 when `flags` differ) — appends its
/// sub-samples to the existing row in box order. Otherwise the row is
/// inserted at its sorted position so `sub_samples_for` can
/// binary-search. The per-box rows arrive already ascending, so each
/// insertion is at-or-after the previous one.
fn merge_subs(dst: &mut Vec<SubSampleInfo>, rows: Vec<SubSampleInfo>) {
    for row in rows {
        match dst.binary_search_by(|r| r.sample_number.cmp(&row.sample_number)) {
            Ok(i) => dst[i].subsamples.extend(row.subsamples),
            Err(i) => dst.insert(i, row),
        }
    }
}

fn parse_stbl<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    track: &mut Track,
) -> Result<()> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;

    let mut stsd_payload: Option<Vec<u8>> = None;
    // `sdtp` carries no on-disk count — it is sized from `stsz`/`stz2`
    // (ISO/IEC 14496-12 §8.6.4.1). Defer its parse until after the walk
    // so the sample count is known regardless of child order.
    let mut sdtp_payload: Option<Vec<u8>> = None;
    // `stdp` has the same deferred-sizing shape (§8.5.3.1 — row count
    // taken from the sample-size box) so we collect the raw payload and
    // parse after the walk completes.
    let mut stdp_payload: Option<Vec<u8>> = None;
    let mut table = SampleTable::default();
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &STSD => {
                stsd_payload = Some(read_payload(r, child)?);
            }
            t if t == &STTS => {
                let body = read_payload(r, child)?;
                table.stts = parse_stts(&body)?;
            }
            t if t == &STSC => {
                let body = read_payload(r, child)?;
                table.stsc = parse_stsc(&body)?;
            }
            // ISO/IEC 14496-12 §8.7.3 lists `stsz` and `stz2` as
            // mutually-exclusive alternatives — at most one of the two
            // appears in any given `stbl`. A malformed writer that
            // emits both is tolerated first-wins (whichever child the
            // walker reaches first populates the table; the second is
            // ignored). Matches the `sbgp`/`sgpd`/`saiz`/`saio`
            // conservative-merge convention.
            t if t == &STSZ && table.sample_size_source.is_none() => {
                let body = read_payload(r, child)?;
                let (def, n, tab) = parse_stsz(&body)?;
                table.stsz_default_size = def;
                table.stsz_count = n;
                table.stsz_table = tab;
                table.sample_size_source = Some(SampleSizeSource::Stsz);
            }
            // Round 204 — Compact Sample Size Box (§8.7.3.3). `stz2`
            // is the mutually-exclusive alternative to `stsz`:
            // first-of-the-two wins so a malformed writer emitting
            // both does not silently overwrite the parsed table with
            // the second box. §8.7.3.3 has no constant-size branch —
            // every entry is listed explicitly — so `stsz_default_size`
            // is unconditionally `None` and downstream code that walks
            // `stsz_table[index]` works unchanged.
            t if t == &STZ2 && table.sample_size_source.is_none() => {
                let body = read_payload(r, child)?;
                let (field_size, n, tab) = parse_stz2(&body)?;
                table.stsz_default_size = None;
                table.stsz_count = n;
                table.stsz_table = tab;
                table.sample_size_source = Some(SampleSizeSource::Stz2 { field_size });
            }
            t if t == &STCO => {
                let body = read_payload(r, child)?;
                table.chunk_offsets = parse_stco(&body)?;
            }
            t if t == &CO64 => {
                let body = read_payload(r, child)?;
                table.chunk_offsets = parse_co64(&body)?;
            }
            t if t == &STSS => {
                let body = read_payload(r, child)?;
                table.stss = parse_stss(&body)?;
            }
            t if t == &STSH => {
                let body = read_payload(r, child)?;
                table.stsh = parse_stsh(&body)?;
            }
            t if t == &SUBS => {
                // §8.7.7.1 permits more than one `subs` box per track
                // (distinguished by `flags`). Merge every box's rows by
                // sample number: rows for the same sample concatenate
                // their sub-sample lists in box order; the merged table
                // is sorted ascending so `sub_samples_for` can
                // binary-search. (Brands that require "only one `subs`
                // box per track" — E.4 — are a strict subset of this.)
                let body = read_payload(r, child)?;
                merge_subs(&mut table.subs, parse_subs(&body)?);
            }
            t if t == &CTTS => {
                let body = read_payload(r, child)?;
                table.ctts = parse_ctts(&body)?;
            }
            t if t == &SDTP => {
                sdtp_payload = Some(read_payload(r, child)?);
            }
            // ISO/IEC 14496-12 §8.5.3 — Degradation Priority Box. Sized
            // from `stsz`/`stz2` `sample_count` per §8.5.3.1 (no on-disk
            // count field), so we defer parsing until after the stbl
            // walk regardless of child order. §8.5.3 lists the box as
            // `Quantity: Zero or one`; a malformed writer emitting two
            // is tolerated first-wins (matches the conservative-merge
            // policy applied to the other "at most once" stbl-scope
            // boxes — `sdtp` itself, plus the various sample-aux boxes).
            t if t == &STDP && stdp_payload.is_none() => {
                stdp_payload = Some(read_payload(r, child)?);
            }
            // ISO/IEC 14496-12 §8.7.6 — Padding Bits Box. Carries its
            // own `sample_count` field (§8.7.6.2) so the parse does
            // not depend on `stsz`/`stz2` and can happen here at walk
            // time, unlike `sdtp` / `stdp` above. §8.7.6.1 lists the
            // box as `Quantity: Zero or one`; a malformed writer
            // emitting two is tolerated first-wins, matching the
            // policy applied to every other "at most once" stbl-scope
            // box.
            t if t == &PADB && table.padb.is_empty() => {
                let body = read_payload(r, child)?;
                table.padb = parse_padb(&body)?;
            }
            t if t == &CSLG => {
                let body = read_payload(r, child)?;
                track.cslg = Some(parse_cslg(&body)?);
            }
            t if t == &SBGP => {
                let body = read_payload(r, child)?;
                let sbgp = parse_sbgp(&body)?;
                // §8.9.2.3 — at most one `sbgp` per
                // `(grouping_type, grouping_type_parameter)` pair
                // inside a Sample Table Box. Drop the duplicate
                // silently rather than erroring; ffmpeg-authored
                // sgpd-without-sbgp + secondary sbgp shapes appear
                // in the wild.
                if !table.sbgp.iter().any(|s| {
                    s.grouping_type == sbgp.grouping_type
                        && s.grouping_type_parameter == sbgp.grouping_type_parameter
                }) {
                    table.sbgp.push(sbgp);
                }
            }
            t if t == &CSGP => {
                // Compact Sample to Group Box (post-2015,
                // docs/container/isobmff/post-2015-additions.md). It
                // is expanded to a plain `SampleToGroup` so the rest
                // of the crate treats it identically to `sbgp`. The
                // same "at most one per (grouping_type,
                // grouping_type_parameter)" rule applies; a `csgp`
                // and `sbgp` for the same pair would be a malformed
                // file, so first-wins (whichever atom appears first)
                // matches the duplicate policy used for `sbgp` above.
                let body = read_payload(r, child)?;
                let csgp = parse_csgp(&body)?;
                if !table.sbgp.iter().any(|s| {
                    s.grouping_type == csgp.grouping_type
                        && s.grouping_type_parameter == csgp.grouping_type_parameter
                }) {
                    table.sbgp.push(csgp);
                }
            }
            t if t == &SGPD => {
                let body = read_payload(r, child)?;
                let sgpd = parse_sgpd(&body)?;
                if !table
                    .sgpd
                    .iter()
                    .any(|s| s.grouping_type == sgpd.grouping_type)
                {
                    table.sgpd.push(sgpd);
                }
            }
            t if t == &SAIZ => {
                // §8.7.8.3 — at most one `saiz` per (aux_info_type,
                // aux_info_type_parameter) per containing box. First
                // wins on duplicates (matches the conservative-merge
                // policy applied to `sbgp` / `sgpd` above).
                let body = read_payload(r, child)?;
                let saiz = parse_saiz(&body)?;
                if !table
                    .saiz
                    .iter()
                    .any(|s| s.aux_info_type == saiz.aux_info_type)
                {
                    table.saiz.push(saiz);
                }
            }
            t if t == &SAIO => {
                // §8.7.9.3 — at most one `saio` per (aux_info_type,
                // aux_info_type_parameter) per containing box. First
                // wins on duplicates.
                let body = read_payload(r, child)?;
                let saio = parse_saio(&body)?;
                if !table
                    .saio
                    .iter()
                    .any(|s| s.aux_info_type == saio.aux_info_type)
                {
                    table.saio.push(saio);
                }
            }
            _ => {}
        }
        Ok(())
    })?;

    // `sdtp` is sized from the sample-size table (§8.6.4.1), which is
    // now fully parsed regardless of stbl child order.
    if let Some(payload) = sdtp_payload {
        table.sdtp = parse_sdtp(&payload, table.stsz_count)?;
    }
    // `stdp` is sized from the same sample-size box (§8.5.3.1).
    if let Some(payload) = stdp_payload {
        table.stdp = parse_stdp(&payload, table.stsz_count)?;
    }

    // stsd parses last because it needs `track.hdlr` to discriminate
    // video vs audio — `hdlr` has already been populated by
    // `parse_mdia` before `parse_minf` ran.
    if let Some(payload) = stsd_payload {
        track.sample_descriptions = parse_stsd(&payload, &track.hdlr)?;
    }
    track.sample_table = table;
    Ok(())
}

// ─────────────── CodecResolver shim for standalone builds ───────────────
//
// When the `registry` feature is off the `oxideav_core::CodecResolver`
// trait is not in scope. We provide a tiny ABI-compatible shim so the
// public surface of `MovDemuxer::open_with` stays unchanged across
// both builds — the standalone resolver simply returns nothing.

#[cfg(feature = "registry")]
pub use oxideav_core::CodecResolver as CodecResolverShim;

#[cfg(not(feature = "registry"))]
pub trait CodecResolverShim: Sync {}
#[cfg(not(feature = "registry"))]
impl<T: Sync> CodecResolverShim for T {}

#[cfg(feature = "registry")]
const NULL_RESOLVER: NullCodecResolver = NullCodecResolver;
#[cfg(not(feature = "registry"))]
const NULL_RESOLVER: () = ();

// ─────────────── Demuxer trait impl ───────────────

#[cfg(feature = "registry")]
impl Demuxer for MovDemuxer {
    fn format_name(&self) -> &str {
        "mov"
    }

    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        loop {
            let (stream_idx, sample, data) = self.read_next()?;
            let stream = &self.streams[stream_idx as usize];
            if self.apply_edits {
                // Applied edit-list mode (QTFF pp. 46–48 / ISO/IEC
                // 14496-12 §8.6.6): timestamps live on the edited
                // timeline; samples outside every edit are dropped.
                let Some(t) = self.edited_timing_for(stream_idx as usize, &sample) else {
                    continue;
                };
                let mut pkt = Packet::new(stream_idx, stream.time_base, data)
                    .with_dts(t.dts)
                    .with_pts(t.pts)
                    .with_keyframe(sample.keyframe);
                if t.duration > 0 {
                    pkt = pkt.with_duration(t.duration);
                }
                return Ok(pkt);
            }
            let mut pkt = Packet::new(stream_idx, stream.time_base, data)
                .with_dts(sample.dts as i64)
                .with_pts(sample.pts())
                .with_keyframe(sample.keyframe);
            if sample.duration > 0 {
                pkt = pkt.with_duration(sample.duration as i64);
            }
            return Ok(pkt);
        }
    }

    fn duration_micros(&self) -> Option<i64> {
        // mvhd.duration is in mvhd.time_scale ticks; convert to µs.
        let m = self.mvhd.as_ref()?;
        if m.time_scale == 0 {
            return None;
        }
        Some((m.duration as i128 * 1_000_000 / m.time_scale as i128) as i64)
    }

    /// Seek to the nearest sync sample at or before `pts` for
    /// `stream_index` (in the stream's `time_base`, i.e. mdhd
    /// timescale ticks). Returns the actual decode timestamp of the
    /// landed sample.
    ///
    /// Algorithm (QTFF "Finding a Sample", pp. 79–80, mirrors
    /// `oxideav-mp4`'s `Mp4Demuxer::seek_to` at `crates/oxideav-mp4/
    /// src/demux.rs:2418`):
    ///
    /// 1. Reject out-of-range / non-video / non-audio streams.
    /// 2. Reject fragmented streams (`is_fragmented()`); a moof-based
    ///    seek strategy is a follow-up.
    /// 3. Walk `stts` to find the largest sample index whose
    ///    cumulative `dts <= pts` (clamping past-end to the last
    ///    sample).
    /// 4. For video tracks with a non-empty `stss`, binary-search for
    ///    the largest sync sample at-or-before the target. Audio tracks
    ///    (and tracks that omit `stss` entirely, per QTFF p. 73) treat
    ///    every sample as a sync sample.
    /// 5. Locate that sample's position in the flat
    ///    `(stream_index, SampleEntry)` queue and set `self.next` so
    ///    that the next `next_packet()` call emits it.
    fn seek_to(&mut self, stream_index: u32, pts: i64) -> Result<i64> {
        self.seek_to_impl(stream_index, pts)
    }
}

// ─────────────── ContainerRegistry hook ───────────────

#[cfg(feature = "registry")]
pub fn open(
    input: Box<dyn oxideav_core::ReadSeek>,
    resolver: &dyn CodecResolver,
) -> Result<Box<dyn Demuxer>> {
    let d = MovDemuxer::open_with(input, resolver)?;
    Ok(Box::new(d))
}

// ─────────────── Probe ───────────────

#[cfg(feature = "registry")]
pub fn probe(p: &oxideav_core::ProbeData) -> u8 {
    if p.buf.len() < 16 {
        return 0;
    }
    // ftyp at offset 0 with major/compat brand including 'qt  '
    if &p.buf[4..8] == b"ftyp" {
        // brand at 8..12, minor at 12..16, compat at 16..
        let major = &p.buf[8..12];
        if major == b"qt  " {
            return 100;
        }
        // Scan compat brands (brand entries are 4 bytes each) — bound
        // the scan by the size32 of the ftyp atom if present.
        let size = u32::from_be_bytes([p.buf[0], p.buf[1], p.buf[2], p.buf[3]]) as usize;
        let upper = size.min(p.buf.len()).max(16);
        let mut o = 16;
        while o + 4 <= upper {
            if &p.buf[o..o + 4] == b"qt  " {
                return 90;
            }
            o += 4;
        }
        // Generic 'ftyp' but not QT-branded — let oxideav-mp4 win.
        return 0;
    }
    // Bare 'moov' first (legacy QuickTime) — weak match.
    if &p.buf[4..8] == b"moov" {
        return 40;
    }
    0
}

#[cfg(all(test, feature = "registry"))]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Minimal demuxer-open round trip: a hand-built one-track,
    /// one-sample QTFF file with brand 'qt  '. The synthetic builder
    /// is shared with the integration test in `tests/synth_minimal_qt.rs`
    /// but kept duplicated here so the unit tests don't depend on
    /// it.
    fn build_minimal_qt() -> Vec<u8> {
        // Layout:
        //   ftyp (qt  )
        //   mdat (8 bytes payload "PAYLOAD!")
        //   moov / mvhd / trak / tkhd / mdia / mdhd / hdlr (vide)
        //                       / minf / vmhd / dinf / dref
        //                              / stbl / stsd / stts / stsc / stsz / stco
        let payload = b"PAYLOAD!"; // 8 bytes
        let mut out = Vec::new();

        // --- ftyp ---
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  "); // major
        ftyp.extend_from_slice(&0u32.to_be_bytes()); // minor
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        // --- mdat ---
        let mdat_offset = out.len() + 8; // payload offset for stco
        push_atom(&mut out, *b"mdat", payload);
        let _ = mdat_offset;

        // --- moov ---
        let mut moov = Vec::new();

        // mvhd v0 (100 bytes payload)
        let mut mvhd = vec![0u8; 100];
        mvhd[12..16].copy_from_slice(&600u32.to_be_bytes()); // time_scale
        mvhd[16..20].copy_from_slice(&30u32.to_be_bytes()); // duration
        mvhd[20..24].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // rate
        mvhd[24..26].copy_from_slice(&0x0100i16.to_be_bytes()); // volume
        mvhd[96..100].copy_from_slice(&2u32.to_be_bytes()); // next_track_id
        push_atom(&mut moov, *b"mvhd", &mvhd);

        // trak
        let mut trak = Vec::new();

        // tkhd v0
        let mut tkhd = vec![0u8; 84];
        tkhd[3] = 0x07; // flags = enabled+in-movie+in-preview
        tkhd[12..16].copy_from_slice(&1u32.to_be_bytes()); // track_id
        tkhd[20..24].copy_from_slice(&30u32.to_be_bytes()); // duration in movie ts
        tkhd[76..80].copy_from_slice(&((320u32) << 16).to_be_bytes()); // width
        tkhd[80..84].copy_from_slice(&((240u32) << 16).to_be_bytes()); // height
        push_atom(&mut trak, *b"tkhd", &tkhd);

        // mdia
        let mut mdia = Vec::new();

        // mdhd v0 (24 bytes)
        let mut mdhd = vec![0u8; 24];
        mdhd[12..16].copy_from_slice(&600u32.to_be_bytes()); // time_scale (matches video)
        mdhd[16..20].copy_from_slice(&30u32.to_be_bytes()); // duration
        push_atom(&mut mdia, *b"mdhd", &mdhd);

        // hdlr (with empty counted name → 25 bytes minimum)
        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        hdlr.extend_from_slice(b"mhlr"); // component_type
        hdlr.extend_from_slice(b"vide"); // component_subtype
        hdlr.extend_from_slice(&[0u8; 12]); // manuf+flags+flags_mask
        hdlr.push(0); // empty Pascal-string name
        push_atom(&mut mdia, *b"hdlr", &hdlr);

        // minf
        let mut minf = Vec::new();

        // vmhd (12-byte fixed: ver+flags + graphics_mode:2 + opcolor:6)
        let mut vmhd = vec![0u8; 12];
        vmhd[3] = 0x01; // no-lean-ahead
        push_atom(&mut minf, *b"vmhd", &vmhd);

        // stbl
        let mut stbl = Vec::new();

        // stsd: 1 entry, format='rle ', width=320, height=240
        let mut stsd = Vec::new();
        stsd.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        stsd.extend_from_slice(&1u32.to_be_bytes()); // n=1
        let entry_size: u32 = 86;
        stsd.extend_from_slice(&entry_size.to_be_bytes());
        stsd.extend_from_slice(b"rle "); // Apple Animation
        stsd.extend_from_slice(&[0u8; 6]); // reserved
        stsd.extend_from_slice(&1u16.to_be_bytes()); // dref index
        let mut vbody = vec![0u8; 70];
        vbody[24..26].copy_from_slice(&320u16.to_be_bytes());
        vbody[26..28].copy_from_slice(&240u16.to_be_bytes());
        stsd.extend_from_slice(&vbody);
        push_atom(&mut stbl, *b"stsd", &stsd);

        // stts: 1 entry (count=1, duration=30)
        let mut stts = Vec::new();
        stts.extend_from_slice(&0u32.to_be_bytes());
        stts.extend_from_slice(&1u32.to_be_bytes());
        stts.extend_from_slice(&1u32.to_be_bytes()); // count
        stts.extend_from_slice(&30u32.to_be_bytes()); // duration
        push_atom(&mut stbl, *b"stts", &stts);

        // stsc: 1 entry (first_chunk=1, samples_per_chunk=1, sd_id=1)
        let mut stsc = Vec::new();
        stsc.extend_from_slice(&0u32.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        push_atom(&mut stbl, *b"stsc", &stsc);

        // stsz: constant size = 8, count = 1
        let mut stsz = Vec::new();
        stsz.extend_from_slice(&0u32.to_be_bytes());
        stsz.extend_from_slice(&8u32.to_be_bytes());
        stsz.extend_from_slice(&1u32.to_be_bytes());
        push_atom(&mut stbl, *b"stsz", &stsz);

        // stco: 1 chunk at offset where mdat payload lives.
        // Compute that offset given the partial buffer state. ftyp size
        // = 8 + 12 = 20, mdat header = 8 bytes → mdat payload @ 28.
        let stco_payload_offset: u32 = 28;
        let mut stco = Vec::new();
        stco.extend_from_slice(&0u32.to_be_bytes());
        stco.extend_from_slice(&1u32.to_be_bytes());
        stco.extend_from_slice(&stco_payload_offset.to_be_bytes());
        push_atom(&mut stbl, *b"stco", &stco);

        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);

        out
    }

    fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
        let size: u32 = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(body);
    }

    #[cfg(feature = "registry")]
    #[test]
    fn open_minimal_qt_yields_one_packet() {
        let bytes = build_minimal_qt();
        let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
        let mut d = MovDemuxer::open(cur).unwrap();
        assert!(d.ftyp.as_ref().unwrap().is_quicktime());
        assert_eq!(d.tracks.len(), 1);
        assert!(d.tracks[0].is_video());
        // The minimal movie writes a `vmhd`; the media-header kind is
        // surfaced and no `elng` peer is present.
        assert_eq!(d.tracks[0].media_header_kind, MediaHeaderKind::Video);
        assert!(d.tracks[0].extended_language.is_none());
        assert_eq!(d.tracks[0].primary_format(), Some(*b"rle "));
        // Demuxer trait surface
        assert_eq!(d.streams().len(), 1);
        let pkt = d.next_packet().unwrap();
        assert_eq!(pkt.stream_index, 0);
        assert_eq!(pkt.data, b"PAYLOAD!".to_vec());
        assert_eq!(pkt.dts, Some(0));
        assert!(pkt.flags.keyframe);
        // Past-the-end yields Eof
        assert!(matches!(d.next_packet(), Err(Error::Eof)));
    }

    #[test]
    fn parse_mdia_surfaces_nmhd_and_elng() {
        // Build a `mdia` for a timed-metadata track: hdlr(meta) + elng
        // + minf{ nmhd }. `parse_mdia` should record the null media
        // header kind and the extended-language tag.
        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        hdlr.extend_from_slice(b"mhlr");
        hdlr.extend_from_slice(b"meta"); // metadata handler
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.push(0); // empty Pascal-string name

        // elng: FullBox + "en-US\0"
        let mut elng = Vec::new();
        elng.extend_from_slice(&0u32.to_be_bytes());
        elng.extend_from_slice(b"en-US");
        elng.push(0);

        // nmhd: empty FullBox
        let nmhd = 0u32.to_be_bytes().to_vec();
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"nmhd", &nmhd);

        let mut mdia = Vec::new();
        let mdhd = vec![0u8; 24];
        push_atom(&mut mdia, *b"mdhd", &mdhd);
        push_atom(&mut mdia, *b"hdlr", &hdlr);
        push_atom(&mut mdia, *b"elng", &elng);
        push_atom(&mut mdia, *b"minf", &minf);

        let mut buf = Vec::new();
        push_atom(&mut buf, *b"mdia", &mdia);

        let mut cur = Cursor::new(buf);
        let hdr = read_atom_header(&mut cur).unwrap().unwrap();
        assert_eq!(hdr.fourcc, *b"mdia");
        let mut track = Track::default();
        parse_mdia(&mut cur, &hdr, &mut track).unwrap();

        assert!(track.hdlr.is_metadata());
        assert_eq!(track.media_header_kind, MediaHeaderKind::Null);
        assert_eq!(track.extended_language.as_deref(), Some("en-US"));
    }

    #[test]
    fn parse_mdia_surfaces_sthd_for_subtitle_track() {
        // Subtitle track: hdlr(subt) + minf{ sthd }.
        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(b"mhlr");
        hdlr.extend_from_slice(b"subt");
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.push(0);

        let sthd = 0u32.to_be_bytes().to_vec();
        let mut minf = Vec::new();
        push_atom(&mut minf, *b"sthd", &sthd);

        let mut mdia = Vec::new();
        push_atom(&mut mdia, *b"mdhd", &[0u8; 24]);
        push_atom(&mut mdia, *b"hdlr", &hdlr);
        push_atom(&mut mdia, *b"minf", &minf);

        let mut buf = Vec::new();
        push_atom(&mut buf, *b"mdia", &mdia);
        let mut cur = Cursor::new(buf);
        let hdr = read_atom_header(&mut cur).unwrap().unwrap();
        let mut track = Track::default();
        parse_mdia(&mut cur, &hdr, &mut track).unwrap();

        assert!(track.is_subtitle());
        assert_eq!(track.media_header_kind, MediaHeaderKind::Subtitle);
        assert!(track.extended_language.is_none());
    }

    /// Build a v0 `sidx` payload with a single media reference so the
    /// top-level walker test can inject it into a synthetic file.
    fn build_sidx_box() -> Vec<u8> {
        let mut body = Vec::new();
        body.push(0); // version 0
        body.extend_from_slice(&[0, 0, 0]); // flags
        body.extend_from_slice(&1u32.to_be_bytes()); // reference_ID = track 1
        body.extend_from_slice(&600u32.to_be_bytes()); // timescale
        body.extend_from_slice(&0u32.to_be_bytes()); // earliest_presentation_time
        body.extend_from_slice(&0u32.to_be_bytes()); // first_offset
        body.extend_from_slice(&[0, 0]); // reserved
        body.extend_from_slice(&1u16.to_be_bytes()); // reference_count = 1
                                                     // One media reference, starts with a SAP of type 1.
        let w0 = 0x0000_1000u32; // reference_type=0, referenced_size=4096
        let w2 = (1u32 << 31) | (1u32 << 28); // starts_with_SAP=1, SAP_type=1
        body.extend_from_slice(&w0.to_be_bytes());
        body.extend_from_slice(&30u32.to_be_bytes()); // subsegment_duration
        body.extend_from_slice(&w2.to_be_bytes());
        let mut out = Vec::new();
        push_atom(&mut out, *b"sidx", &body);
        out
    }

    #[cfg(feature = "registry")]
    #[test]
    fn top_level_sidx_collected_in_file_order() {
        // Prepend a `sidx` ahead of the minimal QT body. The walker
        // must collect it as a top-level box regardless of placement,
        // and the rest of the file must still demux normally.
        let mut bytes = build_sidx_box();
        bytes.extend_from_slice(&build_minimal_qt());
        let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
        let d = MovDemuxer::open(cur).unwrap();
        assert_eq!(d.sidx.len(), 1);
        let s = &d.sidx[0];
        assert_eq!(s.version, 0);
        assert_eq!(s.reference_id, 1);
        assert_eq!(s.timescale, 600);
        assert_eq!(s.references.len(), 1);
        assert_eq!(s.references[0].referenced_size, 4096);
        assert_eq!(s.references[0].subsegment_duration, 30);
        assert!(s.references[0].starts_with_sap);
        assert_eq!(s.references[0].sap_type, 1);
        // The track still parsed.
        assert_eq!(d.tracks.len(), 1);
    }

    #[test]
    fn files_without_sidx_have_empty_vec() {
        let bytes = build_minimal_qt();
        let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
        let d = MovDemuxer::open(cur).unwrap();
        assert!(d.sidx.is_empty());
    }

    #[cfg(feature = "registry")]
    #[test]
    fn probe_recognises_qt_brand() {
        let bytes = build_minimal_qt();
        let pd = oxideav_core::ProbeData {
            buf: &bytes,
            ext: Some("mov"),
        };
        assert_eq!(probe(&pd), 100);
    }

    #[cfg(feature = "registry")]
    #[test]
    fn probe_rejects_random_bytes() {
        let pd = oxideav_core::ProbeData {
            buf: &[0u8; 32],
            ext: None,
        };
        assert_eq!(probe(&pd), 0);
    }

    // ─── Windows file:// shape: portable rule unit-tests ───
    //
    // These exercise the pure helper that performs the path-conversion
    // step. They run on every host (including Unix CI), so the rules
    // are kept under continuous coverage even though the live opener
    // path is gated by `cfg(windows)`.

    #[test]
    fn windows_path_strips_leading_slash_before_drive() {
        // file:///C:/Users/foo → /C:/Users/foo  →  C:\Users\foo
        assert_eq!(
            normalise_path_for_windows("/C:/Users/foo"),
            "C:\\Users\\foo"
        );
    }

    #[test]
    fn windows_path_accepts_legacy_bar_drive_letter() {
        // file:///C|/Users/foo (RFC 8089 Appendix E.2) → C:\Users\foo
        assert_eq!(
            normalise_path_for_windows("/C|/Users/foo"),
            "C:\\Users\\foo"
        );
    }

    #[test]
    fn windows_path_lowercase_drive_letter_accepted() {
        assert_eq!(
            normalise_path_for_windows("/d:/data/x.mov"),
            "d:\\data\\x.mov"
        );
    }

    #[test]
    fn windows_path_without_drive_letter_keeps_leading_slash() {
        // No drive letter → no leading-slash strip; just slash flip.
        assert_eq!(
            normalise_path_for_windows("/no-drive/path"),
            "\\no-drive\\path"
        );
    }

    #[test]
    fn windows_path_relative_input_unchanged_except_separators() {
        // Legacy `file:rel/path` shape — no leading `/`, no drive
        // letter; just slash flip.
        assert_eq!(normalise_path_for_windows("rel/path"), "rel\\path");
    }

    #[test]
    fn file_url_to_path_unix_shapes_unchanged() {
        // The Unix-build path-rendering must keep working byte-identical.
        let p = file_url_to_path("file:///etc/hosts").unwrap();
        if cfg!(windows) {
            assert_eq!(p.to_string_lossy(), "\\etc\\hosts");
        } else {
            assert_eq!(p.to_string_lossy(), "/etc/hosts");
        }
        let p2 = file_url_to_path("file://localhost/etc/hosts").unwrap();
        if cfg!(windows) {
            assert_eq!(p2.to_string_lossy(), "\\etc\\hosts");
        } else {
            assert_eq!(p2.to_string_lossy(), "/etc/hosts");
        }
    }
}
