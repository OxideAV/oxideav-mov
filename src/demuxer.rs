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

use std::io::{Read, Seek, SeekFrom};

use crate::atom::{
    read_atom_header, read_payload, walk_children, AtomHeader, CLEF, CO64, CSLG, CTTS, DINF, DREF,
    EDTS, ELST, ENOF, FREE, FTYP, GMHD, GMIN, HDLR, ILST, KEYS, MDAT, MDHD, MDIA, META, MINF, MOOF,
    MOOV, MVEX, MVHD, PROF, RDRF, RMCD, RMCS, RMDA, RMDR, RMQU, RMRA, RMVC, SKIP, SMHD, STBL, STCO,
    STSC, STSD, STSS, STSZ, STTS, TAPT, TEXT, TKHD, TMCD, TRAK, TREF, UDTA, VMHD, WIDE,
};
use crate::bmff_meta::{parse_bmff_meta, BmffMeta};
use crate::chapter::{decode_text_sample_full, ChapterEntry, ChapterList};
use crate::edit::{parse_elst, EditList};
use crate::gmhd::{parse_gmin, parse_tcmi, parse_text_header, Gmhd};
use crate::header::{parse_ftyp, parse_hdlr, parse_mdhd, parse_mvhd, parse_tkhd, Ftyp, Mvhd};
use crate::media_meta::{parse_cslg, parse_ilst, parse_keys, parse_tapt_dims, MetaKeyValue, Tapt};
use crate::reference::{parse_dref, parse_rdrf, ReferenceMovie};
use crate::sample_table::{
    parse_co64, parse_ctts, parse_stco, parse_stsc, parse_stss, parse_stsz, parse_stts,
    SampleEntry, SampleTable,
};
use crate::track::{parse_stsd, Track, TrackRef, TrackRefKind};
use crate::user_data::{parse_udta, UserDataEntry};

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
    /// Pre-flattened sample queue, sorted by file offset for friendly
    /// I/O patterns. Each entry is `(stream_index, sample)`.
    samples: Vec<(u32, SampleEntry)>,
    /// Cursor into `samples` for the next packet to emit.
    next: usize,
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
            if hdr.fourcc == MOOV {
                input.seek(SeekFrom::Start(hdr.payload_offset))?;
                walk_children(input, Some(body_end), |r, child| {
                    if child.fourcc == RMRA {
                        refs = parse_rmra(r, child)?;
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
        let mut movie_bmff_meta: Option<BmffMeta> = None;
        let mut file_bmff_meta: Option<BmffMeta> = None;
        let mut has_mvex = false;
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

            match &hdr.fourcc {
                t if t == &FTYP => {
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    ftyp = Some(parse_ftyp(&payload)?);
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
                        &mut has_mvex,
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
                    // Fragmented MP4 — QTFF doesn't define `moof`; this is
                    // ISO BMFF §8.16. We refuse rather than silently
                    // produce a partial decode. The hint points at our
                    // sibling crate (oxideav-mp4) which is the right home
                    // for fragmented streams.
                    return Err(unsupported_error(
                        "MOV: fragmented MP4 ('moof') is unsupported by the QuickTime demuxer; \
                         use oxideav-mp4 for fragmented streams",
                    ));
                }
                t if t == &FREE || t == &SKIP || t == &WIDE => {
                    // free-space atoms — skip
                }
                _ => {
                    // unknown — ignored at the top level.
                }
            }

            input.seek(SeekFrom::Start(body_end))?;
        }

        if has_mvex {
            // `mvex` inside `moov` declares the file as fragmented (ISO
            // BMFF §8.16.1) — even when `moof` boxes haven't been seen
            // yet at top-level walk. Reject for the same reason as
            // top-level `moof`.
            return Err(unsupported_error(
                "MOV: 'mvex' indicates a fragmented MP4; use oxideav-mp4 for fragmented streams",
            ));
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

        // Flatten sample tables into a globally offset-sorted queue.
        let mut samples: Vec<(u32, SampleEntry)> = Vec::new();
        for (track_idx, t) in tracks.iter().enumerate() {
            for sample in t.sample_table.iter_samples() {
                let s = sample?;
                samples.push((track_idx as u32, s));
            }
        }
        samples.sort_by_key(|(_, s)| s.offset);

        // Touch the resolver to silence unused warnings on the
        // standalone build path.
        let _ = resolver;

        Ok(Self {
            input,
            ftyp,
            mvhd,
            tracks,
            meta: movie_meta,
            user_data: movie_user_data,
            reference_movies,
            bmff_meta: movie_bmff_meta,
            file_bmff_meta,
            faststart: seen_moov_before_mdat,
            samples,
            next: 0,
            #[cfg(feature = "registry")]
            streams,
        })
    }

    /// True when the file is laid out for streaming playback
    /// ("faststart"): `moov` appears before any `mdat` at top level.
    /// `ftyp`, `free`, `skip`, `wide` atoms encountered before `moov`
    /// do not invalidate the faststart classification.
    pub fn is_faststart(&self) -> bool {
        self.faststart
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

    /// Read the next sample's bytes from the input. Returns
    /// `(stream_index, sample, data)`.
    pub fn read_next(&mut self) -> Result<(u32, SampleEntry, Vec<u8>)> {
        if self.next >= self.samples.len() {
            return Err(Error::Eof);
        }
        let (stream_idx, sample) = self.samples[self.next];
        self.next += 1;
        self.input.seek(SeekFrom::Start(sample.offset))?;
        let mut buf = vec![0u8; sample.size as usize];
        self.input.read_exact(&mut buf)?;
        Ok((stream_idx, sample, buf))
    }

    /// Whether more samples are available.
    pub fn remaining(&self) -> usize {
        self.samples.len().saturating_sub(self.next)
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
    has_mvex: &mut bool,
) -> Result<()> {
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
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
            t if t == &MVEX => {
                // Fragment header — defer the rejection to `open_with`
                // so we can also surface info about which fragmented
                // pieces (mehd/trex) are present, useful for diagnostics.
                *has_mvex = true;
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
            t if t == &TAPT => {
                track.tapt = Some(parse_tapt(r, child)?);
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
            t if t == &VMHD || t == &SMHD => {
                // Typed per-MediaType media-information header. The
                // handler type already classifies the track for us;
                // the body has no payload-affecting fields beyond what
                // round 1 already surfaces via `Track::is_video` /
                // `Track::is_audio`.
            }
            t if t == &GMHD => {
                track.gmhd = Some(parse_gmhd(r, child)?);
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

fn parse_stbl<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    track: &mut Track,
) -> Result<()> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;

    let mut stsd_payload: Option<Vec<u8>> = None;
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
            t if t == &STSZ => {
                let body = read_payload(r, child)?;
                let (def, n, tab) = parse_stsz(&body)?;
                table.stsz_default_size = def;
                table.stsz_count = n;
                table.stsz_table = tab;
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
            t if t == &CTTS => {
                let body = read_payload(r, child)?;
                table.ctts = parse_ctts(&body)?;
            }
            t if t == &CSLG => {
                let body = read_payload(r, child)?;
                track.cslg = Some(parse_cslg(&body)?);
            }
            _ => {}
        }
        Ok(())
    })?;

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
        let (stream_idx, sample, data) = self.read_next()?;
        let stream = &self.streams[stream_idx as usize];
        let mut pkt = Packet::new(stream_idx, stream.time_base, data)
            .with_dts(sample.dts as i64)
            .with_pts(sample.pts())
            .with_keyframe(sample.keyframe);
        if sample.duration > 0 {
            pkt = pkt.with_duration(sample.duration as i64);
        }
        Ok(pkt)
    }

    fn duration_micros(&self) -> Option<i64> {
        // mvhd.duration is in mvhd.time_scale ticks; convert to µs.
        let m = self.mvhd.as_ref()?;
        if m.time_scale == 0 {
            return None;
        }
        Some((m.duration as i128 * 1_000_000 / m.time_scale as i128) as i64)
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
}
