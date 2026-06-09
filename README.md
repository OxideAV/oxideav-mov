# oxideav-mov

Pure-Rust Apple QuickTime File Format (QTFF) demuxer.

QTFF is the immediate ancestor of ISO Base Media File Format
(ISO/IEC 14496-12), and MP4 is itself an ISO BMFF derivative. The
two share an atom/box hierarchy, but QTFF retains Apple-specific
semantics that ISO BMFF does not standardise. This crate is the
**dedicated container for QTFF inputs** and is a sibling — not a
child — of [`oxideav-mp4`].

Round 1 ships:

- atom walker over `[size:4][type:4]([ext_size:8])?[payload]` with
  `size==1` (extended 64-bit) and `size==0` (to-end-of-file) special
  cases per QTFF spec p. 19;
- `ftyp` brand detection (recognises `qt  ` major + compat);
- `moov` walk: `mvhd` + per-track `tkhd`, `mdia/mdhd`, `mdia/hdlr`,
  `minf/stbl/{stsd,stts,stsc,stsz,stco|co64,stss}`;
- per-sample iterator yielding `(index, file_offset, size, dts,
  duration, sample_description_id, keyframe)`;
- `oxideav_core::Demuxer` impl emitting `Packet` records;
- `register(ctx)` entry point + `register!("mov", ...)` so the
  framework's `oxideav-meta` build script wires this crate up
  automatically.

Round 18 adds fragmented MP4 / fMP4 / DASH-init decode (ISO/IEC
14496-12 §8.8) — `mvex/trex` per-track defaults plus full
`moof/traf/tfhd/trun` cascade resolution. A fragmented `.mov` or
`.mp4` now walks every sample through `MovDemuxer::next_packet`,
ffprobe-cross-checked.

Round 19 adds the **write side**: `MovMuxer` emits a non-fragmented
MOV/MP4 (`ftyp` + `mdat` + `moov`) carrying one or more video/audio
tracks. Output is `ffprobe -of json` accepted and round-trips
through `MovDemuxer` with sample count, per-sample sizes, payloads,
and keyframe flags preserved. Atom coverage: `ftyp`, `mvhd`, `tkhd`,
`mdhd`, `hdlr`, `vmhd`/`smhd`, `dinf`/`dref`/`url ` self-ref, `stsd`,
`stts` (run-length), `stss` (only when needed), `stsc`, `stsz`
(uniform-or-table), `stco`/`co64` (auto-promoted when the cumulative
chunk byte offset crosses 4 GiB).

Round 20 adds the **fragmented write side**:
`MovMuxer::with_fragmentation(ByDuration|ByFrameCount)` +
`encode_fragmented_to_vec()` emit an ISO/IEC 14496-12 §8.8 fragmented
MP4 / fMP4 / DASH segment stream — an init segment (`ftyp` with
`iso5`/`dash`/`msdh` brands + `moov` with empty stbl + `mvex/trex`)
followed by one media segment per fragment (`moof` with `mfhd` +
per-track `traf/tfhd/trun` + trailing `mdat`). Pairs with the
round-18 decode path so fragmented streams round-trip in both
directions.

Seek support: `MovDemuxer::seek_to(stream, pts)` walks the per-track
sample queue, snaps to the largest sync (`stss`) sample at-or-before
the target DTS, and resets the demuxer cursor so the next
`next_packet()` re-emits from that sample. Algorithm: QTFF "Finding a
Sample" (pp. 79–80), mirroring `oxideav-mp4`'s `Mp4Demuxer::seek_to`.

Round 80 wires **sample groups** (`sbgp` / `sgpd`) — ISO/IEC 14496-12
§8.9 — into the per-track sample table, and surfaces typed lookups
for the three well-known grouping types in §10:

- `'roll'` (§10.1.1.2) — VisualRollRecoveryEntry /
  AudioRollRecoveryEntry. `MovDemuxer::roll_distance_for(track,
  sample) -> Option<i16>` returns the signed roll distance for the
  caller's sample. Positive values mark gradual-decoding-refresh
  entry points; negative values mark audio streams whose decoder
  output is only correct after pre-rolling `|N|` frames.
- `'prol'` (§10.1.1.2) — AudioPreRollEntry, the AAC and Opus
  codec-priming convention used by CMAF/DASH/HLS. After seeking to
  a sync sample, the player must back up by
  `audio_preroll_for(track, sample)` frames before the decoder's
  output is valid.
- `'rap '` (§10.4.2) — VisualRandomAccessEntry. Marks open-GOP
  random-access points; the entry exposes `num_leading_samples`,
  the count of decode-order samples following the RAP that the
  player must discard when entering there.
  `MovDemuxer::random_access_points(track)` unions `stss` with the
  `'rap '` grouping so callers building a seek index get every
  legitimate entry point at once.

Sample-group parsing handles all three on-disk versions of `sgpd`:
v0 (deprecated implicit-size; fallback to per-typed-entry catalogue),
v1 (`default_length` or per-row `description_length`), and v2 (adds
`default_sample_description_index` so samples uncovered by `sbgp`
still resolve to a default group entry). Duplicate `sbgp`/`sgpd`
boxes with the same `grouping_type` inside a single `stbl` are
silently de-duped (spec §8.9.2.3 forbids them, but ffmpeg sometimes
emits two; we keep the first).

Round 98 parses the **Independent and Disposable Samples Box**
(`sdtp`) — ISO/IEC 14496-12 §8.6.4 — into the per-track sample table.
The box carries one packed byte per sample (no on-disk count field;
its row count equals the `stsz`/`stz2` sample count per §8.6.4.1, so
the parse is deferred until after the `stbl` walk regardless of child
order). Each byte unpacks MSB-first into four 2-bit fields
([`SdtpEntry`]): `is_leading`, `sample_depends_on`,
`sample_is_depended_on`, and `sample_has_redundancy`, each surfaced as
a typed enum ([`IsLeading`] / [`SampleDependsOn`] /
[`SampleIsDependedOn`] / [`SampleHasRedundancy`]) covering all four
§8.6.4.3 code-points including the reserved value. Two convenience
predicates ride along: `SdtpEntry::is_independent()`
(`sample_depends_on == 2`, a codec-agnostic I-picture flag that pairs
with `stss` as a random-access hint) and `SdtpEntry::is_disposable()`
(`sample_is_depended_on == 2`, samples that trick-mode roll-forward
may skip because no other sample depends on them). Surfaces on the
demuxer via `MovDemuxer::sample_dependency(track, sample) ->
Option<SdtpEntry>` and on the table via
`SampleTable::sample_dependency(sample)`. An `sdtp` body shorter than
the sample count is rejected at open time. The box is shared with QTFF
(it predates the ISO standardisation as the QuickTime sample-table
extension of the same name).

Round 105 parses the **Progressive Download Information Box** (`pdin`)
— ISO/IEC 14496-12 §8.1.3 — at file scope. The FullBox (`version = 0`,
`flags = 0`) carries `(rate, initial_delay)` pairs to end-of-box,
where `rate` is a download throughput estimate in bytes/sec and
`initial_delay` is the suggested initial playback delay in
milliseconds such that, if the download continues at `rate`, playback
will not stall (§8.1.3.3). The list has no on-disk count field — it
runs to end-of-box per the spec's `for (i=0; ; i++)` syntax in
§8.1.3.2 — so the parse sizes itself from the post-FullBox-header body
length and rejects a partial trailing entry (body length not a
multiple of 8). Surfaces on the demuxer via `MovDemuxer::pdin:
Option<Pdin>`. The `Pdin::initial_delay_for(download_rate)` accessor
implements §8.1.3.1's "linear interpolation between pairs, or …
extrapolation from the first or last entry" rule: it brackets on a
rate-sorted scratch view (§8.1.3.3 doesn't mandate any particular
ordering, so writer-emitted pairs may be out of order), interpolates
linearly on the `(rate, delay)` line for an observed rate inside the
bracket, and clamps to the first / last entry's delay when the
observed rate falls outside the table — keeping the spec's
"*upper* estimate" promise (lowest rate ↔ longest delay). The first
`pdin` box wins when a malformed writer emits duplicates (spec
§8.1.3.1 recommends `pdin` appear as early as possible, so the first
one is the more informative). Unknown version is rejected at open
time. QTFF doesn't define this box; it is ISO BMFF-only.

Round 114 parses the **Segment Index Box** (`sidx`) — ISO/IEC
14496-12 §8.16.3 — at file scope. The FullBox (`version` 0 or 1)
provides a compact index of one media stream's subsegments for
adaptive-streaming (DASH / CMAF) random access. It carries
`reference_ID` (the indexed stream / track), `timescale`, an
`earliest_presentation_time` + `first_offset` pair (32-bit under v0,
64-bit under v1 — both widened to `u64` in [`Sidx`]), then a
`reference_count`-long list of 12-byte references. Each
[`SidxReference`] unpacks the three bit-packed words into a typed
[`ReferenceType`] (`Media` direct-to-bytes vs `Index` nested-`sidx`),
a 31-bit `referenced_size`, a 32-bit `subsegment_duration`, and the
SAP triple `starts_with_sap` / `sap_type` (3 bit) / `sap_delta_time`
(28 bit) per §8.16.3.3 Table 4. Unknown version (> 1) is rejected at
open time, as is a body length that does not equal
`reference_count × 12` (a partial trailing reference or a count
overrun). Surfaces on the demuxer via `MovDemuxer::sidx: Vec<Sidx>` —
the file-level walker recognises `sidx` as a top-level box regardless
of placement and collects every one in file order, because the box is
`Quantity: Zero or more` (§8.16.3.1) and a segment may carry one per
indexed stream plus nested `sidx`-of-`sidx` references. Three
accessors resolve the index against the box's anchor point (the first
byte after the box, §8.16.3.1): `Sidx::material_start(anchor)` adds
`first_offset`; `Sidx::subsegment_offset(anchor, index)` accumulates
`referenced_size` along the file-contiguous reference chain; and
`Sidx::subsegment_start_time(index)` accumulates
`subsegment_duration` from `earliest_presentation_time` along the
presentation-time-contiguous timeline. QTFF doesn't define this box;
it is ISO BMFF-only and stays absent for plain `.mov` inputs.

Round 102 parses the **Shadow Sync Sample Box** (`stsh`) — ISO/IEC
14496-12 §8.6.3 — into the per-track sample table. The box is an
optional seeking aid: each [`StshEntry`] pairs a *shadowed*
(normally non-sync) sample with the alternative *sync* sample whose
media data substitutes for it when a sync sample is needed at, or
before, the shadowed one. Both numbers are 1-based, sharing `stss`'s
sample-numbering convention. The shadow sync sample *replaces*, not
augments, the sample it shadows (§8.6.3.1): after substitution the next
sample sent is `shadowed_sample_number + 1`. Surfaces on the demuxer
via `MovDemuxer::shadow_sync_sample(track, shadowed_sample_number) ->
Option<u32>` and on the table via `SampleTable::shadow_sync_for(...)`,
which binary-searches the (spec-required) ascending table for an exact
shadowed-sample match. A non-strictly-increasing or duplicate-keyed
table is rejected at open time. The box is purely a seek optimisation —
a track plays and seeks correctly when it is ignored.

Round 118 parses the **Sub-Sample Information Box** (`subs`) — ISO/IEC
14496-12 §8.7.7 — into the per-track sample table. A *sub-sample* is a
contiguous byte range of a sample (e.g. a NAL-unit boundary for
AVC/HEVC); the precise meaning is defined by the coding system named in
the sample description, so this crate surfaces the byte ranges and
leaves interpretation to the caller. The box is *sparsely* coded: each
row names a sample via a `sample_delta` from the previous row's sample
number and lists that sample's sub-samples; samples not named by any row
have no sub-sample structure. The parser accumulates the deltas into
absolute 1-based `sample_number`s (§8.7.7.3 — the first row's delta is
the difference from zero) and rejects a zero `sample_delta` (which would
duplicate a sample number or yield a 0-numbered first sample), an unknown
`version` (> 1), and a truncated record. Each [`SubSampleEntry`] carries
the `subsample_size` (16-bit under v0, 32-bit under v1 — both widened to
`u32`), `subsample_priority`, `discardable`, and
`codec_specific_parameters`, with an `is_discardable()` predicate
(§8.7.7.3 — a discardable sub-sample is not required to decode the
sample, e.g. SEI). §8.7.7.1 permits more than one `subs` box per track
(distinguished by `flags`); rows from every box are merged into one
ascending table, concatenating sub-sample lists for any shared sample.
Surfaces on the demuxer via `MovDemuxer::sub_samples(track,
sample_number) -> Option<&[SubSampleEntry]>` and on the table via
`SampleTable::sub_samples_for(sample_number)`, which binary-searches the
sorted table; a row naming a sample with zero sub-samples returns
`Some(&[])`. QTFF doesn't define this box; it is ISO BMFF-only.

Round 74 wires the **edit list** (`edts/elst`) into a presentation-time
mapping API: `MovDemuxer::movie_pts_for(track, media_pts)` translates a
sample's media-timescale PTS to its movie-timescale PTS by walking the
typed [`EditSegment`] list — handling empty edits, dwell
(`media_rate == 0`), the §8.6.6.1 composition-shift idiom, and the
implicit trailing empty edit when `sum(elst.track_duration) <
mvhd.duration` (QTFF pp. 46–48 / ISO/IEC 14496-12 §8.6.5–§8.6.6).
Tracks without an `edts/elst` get a synthetic full-track media segment
so the same mapper drives the no-edits "presentation starts
immediately" case. Round 74 also surfaces `tkhd.flags`
(`is_enabled` / `participates_in_movie` / `participates_in_preview` /
`participates_in_poster`) and `alternate_group`, plus
`MovDemuxer::presentation_tracks()` / `alternate_groups()`.

Round 91 generalises the mapper to **non-unity `media_rate`**: a
segment with `media_rate = 2.0` (16.16 fixed `0x0002_0000`) consumes
twice as much media per movie tick — exactly the QTFF "Playing With
Edit Lists" worked example on p. 226–227, where a 600 movie-tick edit
at rate 2.0 consumes 200 media ticks (movie_ts = 600, media_ts = 100).
Negative or zero `media_rate` on a `Media` segment is rejected on a
per-segment basis (QTFF p. 48: "this rate value cannot be 0 or
negative"); `media_rate == 0` paired with a non-empty `media_time`
is dwell and is handled by the [`EditSegmentKind::Dwell`] arm. The
fixed-point arithmetic is `Δmovie = Δmedia × movie_ts × 65536 /
(media_ts × rate_fp)` with half-up rounding (no spec-mandated
direction).

Round 22 adds the **HEIF/HEIC image-item WRITE path**:
[`HeifWriter`] emits a structurally-valid `.heic` / `.heif` /
`.avif` file from a list of [`HeifItem`]s, where each item carries
its coded bytes (HEVC / AV1 / JPEG / …), an item-id, an
item_type FourCC, and a per-item property list ([`HeifProperty`]
variants: `ispe`, `pixi`, `colr` nclx / rICC / prof, `auxC`,
`lsel`, `irot`, `imir`, `clli`, `mdcv`, `cclv`, `amve`, plus an
`Other { fourcc, payload }` fall-through for codec-config blobs
like `hvcC` / `av1C`). Derived items ([`HeifDerivation`]: `grid`,
`iovl`, `iden`, `tmap`) emit their derivation body into `idat`
(construction-method 1) and auto-generate the matching `dimg`
`iref` row from the caller-supplied `component_ids` list.
Property de-duplication: structurally-equal properties across
items collapse to one `ipco` entry referenced by N `ipma` rows.
Two-pass layout so the `iloc` extents carry real absolute file
offsets. Output round-trips through this crate's own
[`parse_bmff_meta`] / [`iprp`] / [`derived`] surfaces with every
item id, every property association, and every iref preserved,
and `ffprobe -v warning` accepts the container structure.

Round 89 wires the **Track Load Settings atom** (`load`) — QTFF p. 48,
Figure 2-12 — into the per-track parse. The 16-byte body carries a
movie-timescale preload window (`preload_start_time` +
`preload_duration`, with `-1` → "to end of track"), a
mutually-exclusive enable-mode flag pair (`PRELOAD_ALWAYS` /
`PRELOAD_IF_ENABLED`), and a quality-hint bitfield (`DOUBLE_BUFFER` /
`HIGH_QUALITY`; vendor-private bits survive on the raw u32). Surfaces
on the demuxer via `MovDemuxer::track_load(track_index) -> Option<&Load>`
and `Track::load_settings()`. The atom has no ISO BMFF counterpart —
QuickTime only.

Round 95 wires the **Track Selection box** (`tsel`) — ISO/IEC 14496-12
§8.10.3 (pp. 72–74) — into the per-track parse. `tsel` lives inside the
track-level `udta` and refines `tkhd.alternate_group` with a signed
32-bit switch-group id plus a list of attribute FourCCs that describe
or differentiate tracks inside that switch group. The §8.10.3.5
attribute set is enumerated as six descriptive (`tesc/fgsc/cgsc/spsc/
resc/vwsc`) + eight differentiating (`cdec/scsz/mpsz/mtyp/mela/bitr/
frar/nvws`); each entry classifies via [`TsAttributeRole`] and unknown
FourCCs survive raw so vendor / future-spec entries don't get lost.
Surfaces on the demuxer via `MovDemuxer::track_selection(track_index)`
and `Track::track_selection()`; `MovDemuxer::switch_groups()` returns a
bucket map sorted ascending by switch-group id (tracks without a `tsel`
AND tracks with `switch_group == 0` are excluded — both equivalent to
"no switching information declared" per §8.10.3.4). Pairs with the r74
`alternate_groups()` surface to expose the full alternate ⊇ switch
hierarchy. QTFF doesn't define this box; it is ISO BMFF-only.

Round 128 parses the **Producer Reference Time Box** (`prft`) — ISO/IEC
14496-12 §8.16.5 — at file scope. The FullBox (`version` 0 or 1)
records the writer's UTC wall-clock instant in NTP format (RFC 5905 §6)
at which the *next* movie fragment box in bitstream order was produced
(§8.16.5.1), paired with a media time on a reference track that
corresponds to the same instant. Live encoders (DASH-LL / CMAF /
HLS-fMP4) emit one before each `moof` so a paired live decoder can
recover the producer-consumer rate skew and bound drift over long
sessions. Layout: `reference_track_ID[4]` + `ntp_timestamp[8]` +
`media_time[4|8]` (32-bit under v0, 64-bit under v1; both widened to
`u64` in [`Prft`]). Surfaces on the demuxer via
`MovDemuxer::prft: Vec<Prft>` (file order — `Quantity: Zero or more`
per §8.16.5.1) plus `MovDemuxer::first_prft()` for the §8.16.5.1
typical "earliest producer time = the file's first fragment" shortcut.
Convenience accessors on [`Prft`]: `ntp_seconds()` / `ntp_fraction()`
decompose the NTP word into its RFC 5905 §6 halves, and
`unix_micros()` converts to a microsecond Unix-epoch instant via the
2 208 988 800 s NTP→Unix offset (returning `None` for any pre-1970
NTP value). Rejected at open time: unknown `version` (> 1), a payload
shorter than the fixed-width record for the declared version, and any
trailing bytes past that record (`prft` carries no list — extra bytes
indicate corruption or an unparseable writer extension). QTFF doesn't
define this box; it is ISO BMFF-only and stays empty for plain `.mov`
inputs.

Round 125 parses the **Segment Type Box** (`styp`) — ISO/IEC 14496-12
§8.16.2 — at file scope. The box has the same on-disk shape as `ftyp`
(`major_brand[4]` + `minor_version[4]` + `compatible_brands[4]*`),
distinguished by the box-type FourCC alone, and identifies a DASH /
CMAF / HLS-fMP4 media segment plus the specifications it conforms to
(§8.16.2.1: "If segments are stored in separate files … it is
recommended that these 'segment files' contain a segment-type box, …
to enable identification of those files, and declaration of the
specifications with which they are compliant"). `Quantity: Zero or
more`; the spec says any `styp` not first in its file "may be
ignored", but we collect every one in file order so a caller
inspecting a concatenated segment stream can see every
segment-boundary marker. Surfaces on the demuxer via
`MovDemuxer::styp: Vec<Styp>` (file order) plus three convenience
accessors: `first_styp()` (the §8.16.2.1 conformance declaration),
`is_dash_segment()` (true when the first `styp`'s brand list includes
any of `msdh` / `msix` / `risx`), and `is_cmaf_segment()` (true when
it includes `cmfs`). Each [`Styp`] exposes `major_brand` /
`minor_version` / `compatible_brands` plus a `has_brand(&[u8; 4])`
predicate, a `to_ftyp()` conversion that re-uses the [`Ftyp`]
brand-class machinery, and a `major_brand_class()` shortcut into
[`BrandClass`]. Payloads shorter than the 8-byte fixed header or
whose `compatible_brands` tail is not 4-aligned are rejected at open
time; an empty compatible-brands list is legal (a bare
`[major][minor]` body is a valid segment-type box). QTFF doesn't
define this box; it is ISO BMFF-only and stays absent for plain
`.mov` inputs.

Round 122 wires the **Track Kind box** (`kind`) — ISO/IEC 14496-12
§8.10.4 (p. 74) — into the per-track parse. `kind` lives inside the
track-level `udta` (`moov/trak/udta/kind`) and labels the track with a
semantic role expressed as a `(schemeURI, value?)` pair of
NULL-terminated C strings per §8.10.4.3. The box is `Quantity: Zero or
more` (§8.10.4.1), so a track may carry several `kind` entries — one
per role taxonomy. Each [`KindEntry`] exposes `scheme_uri` (e.g.
`urn:mpeg:dash:role:2011` or `https://www.w3.org/TR/webvtt1/`) and an
optional `value` (`None` when the box's on-disk shape is `[uri]\0\0`,
the §8.10.4.3 "URI identifies the kind itself" form) plus a
`has_value()` predicate. Both strings decode UTF-8 best-effort
(`String::from_utf8_lossy` — malformed bytes become U+FFFD rather than
rejecting the box); a missing trailing NULL on either string is
tolerated (the field runs to end-of-slice). Unknown version (> 0) is
rejected at open time. Surfaces on the demuxer via
`MovDemuxer::track_kinds(track_index) -> &[KindEntry]` and on the
track via `Track::track_kinds()`; both return an empty slice when the
track declares no `kind`. The `udta` body is re-walked once for both
`tsel` and `kind` so the typed surfaces stay aligned with the raw flat
[`crate::user_data`] list. QTFF doesn't define this box; it is ISO
BMFF-only and stays absent for plain `.mov` inputs.

Round 140 parses the **Clipping atom** (`clip`) and its sole defined
child the **Clipping Region atom** (`crgn`) — QTFF p. 43 / p. 44 —
at both movie scope (`moov/clip`) and per-track scope
(`moov/trak/clip`). The wrapper `clip` is a single-child container
whose body the parser scans for one `crgn`; the region itself is a
QuickDraw `Region` with a 16-bit byte-length count (`region_size`,
inclusive of itself + the bounding box, minimum `10`), an 8-byte
QuickDraw `Rect` bounding box (four 16-bit signed integers in
top/left/bottom/right order), and an optional opaque scanline tail
of `region_size - 10` bytes preserved verbatim for callers that want
a round-trip surface (QTFF doesn't document the scanline format).
Surfaces on the demuxer via `MovDemuxer::clipping: Option<Clipping>`
(movie scope) and `Track::clipping: Option<Clipping>` (track scope);
both follow the first-wins duplicate-merge policy shared with
`mvhd` / `pdin` / `ctab`. `QdRect::{width, height, is_empty}`
helpers expose the rect's derived shape (widths returned as `i32`
to absorb sign-bit overflow across the i16 range). ISO BMFF does
not define `clip` or `crgn`; an MP4 / fMP4 / HEIF / AVIF file will
not carry either and both fields stay `None`.

Round 144 parses the **Track Matte atom** (`matt`) and its sole
defined child the **Compressed Matte atom** (`kmat`) — QTFF p. 44 /
p. 45 — at per-track scope (`moov/trak/matt`). The wrapper `matt` is
a single-child container whose body the parser scans for one `kmat`;
the leaf carries a 1-byte version + 3-byte flags FullBox-style header
(spec fixes both at 0), a standard QTFF image description structure
(same on-disk shape as a video sample description per QTFF p. 70 +
pp. 92–94 — the parser carves it out using the 4-byte size word at
its head and surfaces the bytes verbatim), and a trailing
variable-length blob of compressed matte data interpreted by the
codec the image description names. Surfaces on the demuxer via
`Track::matte: Option<Matte>`; first-wins on the rare duplicate case
(shared with `clip` / `tapt` / `load` / `cslg`). The `kmat` parser
rejects unknown `version` (`!= 0`), non-zero `flags`, an image
description shorter than the 16-byte universal header (size + format
FourCC + 6 reserved + dref index), or a declared image-description
size that overruns the body — every rejection happens at open time
so a malformed matte never silently disappears.
`CompressedMatte::{data_format, image_description_size}` helpers
expose the codec FourCC and the carved structure length without
re-parsing the bytes. There is no movie-level matte — QTFF Figure
2-6 places `matt` only inside `trak`; a movie's matte is the union
of its tracks'. ISO BMFF does not define either atom; an MP4 / fMP4
/ HEIF / AVIF file will not carry them and `Track::matte` stays
`None`.

Round 147 parses the **Sample Auxiliary Information Sizes Box**
(`saiz`) and **Sample Auxiliary Information Offsets Box** (`saio`)
— ISO/IEC 14496-12 §8.7.8 / §8.7.9 — at `stbl` scope. The pair
carries per-sample auxiliary information stored *outside* the
sample data itself (e.g. ISO/IEC 23001-7 Common Encryption
sample-aux records, or any other writer-defined per-sample side
channel); the format and meaning of that data is owned by a
separate specification, so this crate decodes only the structural
envelope and surfaces it for caller interpretation. `saiz`
records the per-sample byte count (either a single
`default_sample_info_size` for uniform-size streams or a
per-sample `u8` table when `default_sample_info_size == 0`),
plus the discriminator pair `(aux_info_type,
aux_info_type_parameter)` when `flags & 1` is set. `saio` records
the file offsets to those bytes — either one offset per chunk /
track-fragment-run (matching the container's chunking) or a single
offset for the whole `stbl`. `saio` has two versions: v0 carries
32-bit offsets, v1 carries 64-bit. Surfaces on the demuxer via
`MovDemuxer::sample_aux_info(track, aux_info_type,
aux_info_type_parameter) -> (Option<&Saiz>, Option<&Saio>)` (the
two sides may exist independently — §8.7.8.1 requires a matching
`saio` for every `saiz` but writers sometimes emit only one) and
on the sample table via `SampleTable::sample_aux_for(...)`. Each
[`Saiz`] exposes `size_for(sample_idx) -> Option<u32>` (honouring
the §8.7.8.3 prefix rule — samples past `sample_count` have no
auxiliary information) and `total_size()` (saturating sum across
the table); each [`Saio`] exposes `is_single_chunk()` (true when
`entry_count == 1`, the §8.7.9.3 "all auxiliary information
contiguous from this offset" shortcut) and `offset_for(index)`.
Boxes without an on-disk discriminator (the `flags & 1` bit
unset) match an `aux_info_type` of `b"\0\0\0\0"` and
`aux_info_type_parameter == 0` via the accessor; the §8.7.8.1
implicit-fallback rules (scheme type for CENC-protected content,
sample-entry FourCC otherwise) are caller-side concerns. Rejected
at open time: unknown `saiz` version (spec fixes at 0), unknown
`saio` version (spec defines only v0 / v1), a body shorter than
the FullBox header, the `flags & 1` bit set with the
discriminator pair absent, a per-sample `saiz` size table
truncated below `sample_count`, an `saio` offset table truncated
below `entry_count × {4,8}`, and trailing bytes past the `saio`
offset table. Duplicate boxes for the same `(aux_info_type,
aux_info_type_parameter)` inside one `stbl` are silently merged
first-wins (spec forbids them per §8.7.8.3 / §8.7.9.3, matching
the `sbgp` / `sgpd` conservative-merge convention). Round 150
extends the same envelope decode to the `traf` (fragmented) scope
per §8.7.8.1 / §8.7.9.1: each `traf` is walked for its own `saiz`
/ `saio` pair and surfaced on the demuxer via
`MovDemuxer::fragment_sample_aux_info(track) ->
&[FragmentSampleAux]`, with one entry per fragment that ships any
sample-aux box (each entry carries the originating `mfhd`
sequence number plus a `lookup(aux_info_type,
aux_info_type_parameter) -> (Option<&Saiz>, Option<&Saio>)`
mirroring the `stbl`-scope accessor's match semantics). CMAF /
DASH-live / CENC fixtures that carry one sample-aux slab per
fragment now round-trip through the demuxer without external
parsing. QTFF does not define either box; both are ISO BMFF-only
and stay empty for plain `.mov` inputs.

Round 157 parses the **Preview atom** (`pnot`) — Apple QuickTime File
Format Specification (QTFF, 2001-03-01) pp. 26 – 27 / Figure 1-7 — at
file scope. The atom is a preflight thumbnail hint: it points at one of
the file's other top-level atoms (typically a `PICT` QuickDraw picture
stored after `moov`) and declares "this is the representative poster
image for the movie." A Finder / Open dialog can render the preview
without decoding any media samples and without instantiating the codec
pipeline. Layout is a fixed 12-byte body: `modification_date[4]` (Mac-
classic seconds since 1904-01-01T00:00:00Z, the same epoch QTFF's
`mvhd` uses for creation / modification times per p. 32) +
`version_number[2]` (spec-fixed at 0) + `atom_type[4]` (FourCC of the
previewed atom, typically `PICT` but any top-level FourCC is legal) +
`atom_index[2]` (1-based index into that atom type's instances; QTFF
p. 27 documents the typical value as 1). The parser rejects any body
length other than 12 (`pnot` carries no list per QTFF Figure 1-7 — a
truncated or padded body must reject so `atom_type` / `atom_index`
can't silently corrupt). `version_number != 0` and `atom_index == 0`
both *parse* (the spec's other fields stay readable) and surface
conformance signals via [`Pnot::is_known_version`] /
[`Pnot::is_valid_index`] for strict consumers. Convenience accessor
`Pnot::unix_seconds()` converts the Mac timestamp to Unix-epoch
seconds via the [`MAC_TO_UNIX_EPOCH_SECONDS`] offset
(2 082 844 800 s), returning `None` for any pre-1970 value. Surfaces
on the demuxer via `MovDemuxer::pnot: Option<Pnot>` populated by the
file-level walker — at most one `pnot` is kept per file, first-wins
on the rare duplicate case (matching the `pdin` / `ctab` / `clip` /
`mvhd` conservative-merge convention). ISO BMFF does not define this
atom; it is QuickTime-only and stays absent for MP4 / fMP4 / HEIF /
AVIF inputs.

Round 137 parses the **Color Table atom** (`ctab`) — QTFF p. 35 — at
movie scope. The atom is an optional Apple-only leaf that lists a
preferred 4-channel (reserved/red/green/blue) 16-bit palette of up to
256 entries (the count field is *zero-relative*: on-disk `size` of
`N` declares `N+1` entries per QTFF p. 35). The parser rejects the
spec-fixed seed (`!= 0`) and flags (`!= 0x8000`) values, plus any
body length that disagrees with the declared count. Each
[`ColorTableEntry`] preserves the on-disk `reserved` word verbatim
(some authoring tools stash a Mac Toolbox `ColorSpec.value` index
there even though the spec fixes it at 0) and exposes an `rgb8()`
helper that returns the high-byte 8-bit-per-channel triple from each
16-bit channel. Surfaces on the demuxer via `MovDemuxer::ctab:
Option<Ctab>` populated by the `moov` walker — at most one is kept
per file with first-wins on the rare duplicate case (matching the
`mvhd` / `pdin` conservative-merge convention). ISO BMFF does not
define this atom; it is QuickTime-only and stays absent for MP4 /
fMP4 / HEIF / AVIF inputs.

Round 162 tightens the **atom walker's injection-robustness**. The
parser is the kind of demuxer you can point at a network-supplied byte
stream, and a malformed shape must produce a clean `Err(...)` rather
than a panic, OOM kill, or runaway allocation. Two defenses land:

- [`read_payload`] now refuses to allocate above
  [`MAX_INMEMORY_ATOM_BODY`] (64 MiB). Every metadata atom that
  legitimately materialises into a `Vec<u8>` (ftyp / moov / mvhd /
  tkhd / mdhd / stsd / stts / stsc / stsz / stco / co64 / stss / sdtp
  / subs / saiz / saio / sgpd / sbgp / tref / udta / meta / keys /
  ilst / kind / tsel / load / clip / crgn / matt / kmat / gama / pasp
  / clap / colr / chan / tapt / clef / prof / enof / pdin / sidx /
  styp / prft / pnot / ctab) stays well under a megabyte in practice;
  `mdat` (gigabytes legitimately) is never read via `read_payload`
  in this crate — per-sample reads `seek` into it. So the cap bounds
  one in-memory atom body, not the file size. A forged extended `size`
  of (say) 8 GiB on a 1 KiB file now errors at the allocation site
  before `vec![0u8; n as usize]` lands. A companion
  [`read_payload_bounded`] helper lets callers express a tighter
  per-call envelope (parent atom's remaining bytes, known file
  length).
- `MovDemuxer::open` and `MovDemuxer::probe_reference_movies` now
  reject any top-level atom whose declared `size` extends past
  end-of-file. `walk_children` already enforced the same rule on
  nested atoms (a child's body cannot exceed its parent's payload);
  the top-level walker now mirrors it, so every layer of the
  demuxer is uniformly spec-bounded. The check is strict-greater:
  an atom whose body_end is *exactly* `total_len` is still accepted
  (the common case where `mdat` runs to the end of the file).

Pinned by `tests/synth_round162_robustness.rs` (16 tests, four
groups): forged 32-bit / 64-bit / one-byte-past-EOF top-level sizes
are rejected; `read_payload` and `read_payload_bounded` reject above
their respective caps and accept exactly at; a truncation sweep walks
the baseline file byte-by-byte (no panic / no OOM at any cut point); a
256-trial xorshift64* random-byte fuzz pass confirms hostile garbage
never panics; a bogus nested-trak size pins `walk_children`'s existing
rejection in place; degenerate `size == 0` (to-EOF) and empty-file
cases surface cleanly.

Round 21 adds **fragmented-MP4 seek** via the ISO/IEC 14496-12
§8.8.10 `tfra` (Track Fragment Random Access Box) at open time. The
demuxer walks `mfra/tfra/mfro` once and `seek_to` binary-searches
the per-track entries for the largest entry whose *presentation*
time is `<= target_pts` (§8.8.10.3 — `tfra` rows are PTS-keyed),
locating the matching sync sample in the flat queue and snapping
`self.next`. Files without `mfra` fall back to a linear scan of the
round-18 flattened `fragment_samples` queue. `tfdt` (§8.8.12) is
now also parsed so per-fragment DTS climbs from the writer-supplied
baseline rather than a re-zeroed cursor.

Round 182 parses the **User-Type Box** (`uuid`) — ISO/IEC 14496-12
§4.2 / §11.1 — at file scope. `uuid` is the spec's escape hatch for
vendor-specific extensions: every box body opens with a 16-byte UUID
identifying the vendor schema, followed by an opaque payload. The
parser surfaces both verbatim — [`Uuid::usertype`] (the raw `[u8; 16]`)
and `Uuid::payload` (the trailing bytes) — without committing the
crate to any vendor schema, so callers dispatch on the UUID bytes by
exact match (PIFF tfxd / tfrf live-DASH timing extensions, Sony XAVC
clip metadata, GoPro GPMF telemetry, etc.). The boxes surface on the
demuxer via `MovDemuxer::file_uuids: Vec<Uuid>` collected in
declaration order: §4.2's `Quantity: Zero or more` lets a single file
carry several vendor extensions (`tfxd` + `tfrf`, Sony XAVC + GoPro
GPMF, …) and there is no implied "first wins" rule, so each entry
stays distinct. Two diagnostic helpers ride along:
`Uuid::usertype_string()` formats the UUID as the canonical RFC 4122
textual form `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`, and
`Uuid::is_iso_reserved_namespace()` / `Uuid::iso_namespace_boxtype()`
detect and decode the §11.1 reserved-namespace pattern (`type ‖ 00 11
00 10 80 00 00 AA 00 38 9B 71`) — the spec forbids writing standard
boxes through the `'uuid'` escape, so a true result flags a non-
conformant writer that promoted a normative box into the UUID space.
A body shorter than the 16-byte `usertype` prefix is rejected at
open time so a half-record can't silently disappear; an empty payload
after the UUID is accepted (§4.2 puts no lower bound on payload
length). QTFF does not define `uuid` at the spec level but real-world
`.mov` files routinely embed user-type boxes from vendors that emit
QuickTime containers, so the file-level field is populated for both
QT MOV and ISO BMFF derivative inputs. The round-176 fuzz harness
extends to sweep every collected entry (capped at 64 per input to
bound pathological-writer cases) through `usertype_string` /
`is_iso_reserved_namespace` / `iso_namespace_boxtype` /
`payload.len()` so the adversarial UUID-prefix surface stays covered.

Round 176 adds a **cargo-fuzz harness** under `fuzz/` (target
`demux`). The target feeds arbitrary bytes through `MovDemuxer::open`,
drains up to 256 packets via `next_packet`, touches every file-scope
structural accessor (`ftyp` / `mvhd` / `pdin` / `sidx` / `styp` /
`prft` / `ctab` / `pnot` / `clipping`, plus the brand and segment-
classification predicates), sweeps every track through `track_load` /
`track_selection` / `track_kinds` / `edit_segments_for` /
`random_access_points`, pokes the round-74 / round-91 edit-list
mapper at `media_pts = 0 / i64::MIN / i64::MAX`, and re-exercises the
round-21 seek path via `seek_to(0, 0)`. The harness pairs with the
round-162 robustness invariants — the 64 MiB
`MAX_INMEMORY_ATOM_BODY` cap, the past-EOF top-level rejection, and
the nested child-vs-parent envelope check — and keeps them
exercised across the random-input space. A daily 30-minute run is
scheduled via `.github/workflows/fuzz.yml`. Reference-movie alias
resolution (`open_with_aliases`) is intentionally excluded so a fuzz-
supplied `rmra/url ` cannot reach the network or the file system;
the no-alias `MovDemuxer::open` path still walks every
`rmra/rmda/rmdr/rmcs` parser so the reference-movie *parse* side is
fully covered.

Round 204 parses the **Compact Sample Size Box** (`stz2`) — ISO/IEC
14496-12 §8.7.3.3 — at `stbl` scope, completing the §8.7.3 sample-size
surface. `stz2` is the on-disk-compact alternative to `stsz`: each
entry occupies a fixed `field_size` (4, 8 or 16 bits per §8.7.3.3.2)
rather than a full 32 bits, packing per-sample sizes for streams whose
sizes routinely fit in fewer bits (small text-track lines, low-bitrate
CMAF audio fragments, screen-recording IFRAME streams whose dropped
P/B frames sit under 256 bytes). Only one of `stsz` / `stz2` appears
in any given `stbl` per §8.7.3; a malformed writer that emits both is
tolerated first-wins, matching the `sbgp`/`sgpd`/`saiz`/`saio`
conservative-merge convention. The 4-bit packing decodes MSB-first
per §8.7.3.3.2 ("each byte contains two values: entry[i]<<4 +
entry[i+1]"), with the trailing zero-pad nibble silently dropped for
odd `sample_count` ("the last byte is padded with zeros"). Decoded
entries are widened to `u32` and stored in the existing
[`SampleTable::stsz_table`] so every downstream consumer
([`SampleTable::sample_count`], `SampleTable::sample_size_at`, the
sample iterator) continues to work unchanged regardless of which box
populated the table. A new [`SampleSizeSource`] enum + companion
[`SampleTable::sample_size_source`] field + companion
`MovDemuxer::sample_size_source(track_index)` accessor surface the
on-disk encoding choice — `Stsz`, `Stz2 { field_size }`, or `None`
when the `stbl` carries no sample-size box (fragmented-only tracks
whose sizes all come from `trun`) — so a round-tripping remuxer that
wants to preserve a compact-encoded segment can detect it without
re-parsing the box. Rejected at open time: `field_size` other than
4 / 8 / 16 (§8.7.3.3.2 enumerates exactly these three widths),
non-zero 24-bit `reserved` word (§8.7.3.3.1 spec-fixes it at 0),
truncated entry table (post-header byte count shorter than
`ceil(sample_count × field_size / 8)`), unknown FullBox `version`
(spec fixes at 0), body shorter than the 12-byte fixed header. QTFF
does not define this box; it is ISO BMFF-only and stays absent for
plain `.mov` inputs. The round-176 fuzz harness extends to call the
new per-track `sample_size_source` accessor so an attacker-supplied
`stbl` with both boxes, neither box, or a pathological `field_size`
value reaches the accessor without panicking.

Round 199 parses the **Track Group Box** (`trgr`) — ISO/IEC 14496-12
§8.3.4 (p. 27) — at per-track scope (`moov/trak/trgr`). The container
itself is empty-bodied (§8.3.4.2 `aligned(8) class TrackGroupBox('trgr') {}`)
and holds zero or more *track-group-type* FullBoxes whose FourCC is the
`track_group_type` and whose first u32 (after the FullBox header) is the
`track_group_id`. Two tracks whose `trgr` containers each carry a child
with the same FourCC and the same `track_group_id` belong to the same
track group (§8.3.4.3). The pair `(track_group_type, track_group_id)` is
the spec's group identifier — same type but different id means different
groups (two msrc-tagged participants in a video-telephony call), and
same id but different type also means different groups (an `'msrc'`
membership does not collide with a derived-spec group sharing the id).
Each [`TrackGroupTypeEntry`] surfaces the type FourCC, the id, the
FullBox version (rejected if non-zero per §8.3.4.2), the flags
low-24-bit (tolerated even when non-zero, matching `parse_kind` /
`parse_tsel`), and any type-specific tail bytes verbatim (§8.3.4.2 "the
remaining data may be specified for a particular track_group_type" —
empty for `'msrc'` and every other base-spec type, populated for
derived-spec or vendor extensions). The `is_msrc()` predicate marks the
§8.3.4.3 multi-source-presentation group, the only `track_group_type`
the base spec defines. Surfaces on the demuxer via four accessors:
`Track::track_groups()` and `MovDemuxer::track_group_entries(track)`
return the per-track entry list in file order;
`MovDemuxer::tracks_in_group(type, id)` returns every track that
declares membership of one specific `(type, id)` group; and
`MovDemuxer::track_groups()` returns every observed `(type, id)` bucket
sorted ascending, with per-bucket duplicate-track dedup (a track that
lists the same membership row twice — legal per §8.3.4 since the spec
does not forbid duplicate rows — appears once in the bucket). The
parser uses the existing `walk_children` machinery so every
round-162 / round-187 generic atom-header guardrail applies (past-EOF
rejection, 64-MiB `read_payload` cap, `size==1` + largesize overflow
check). `trgr` itself is `Quantity: Zero or one` per `trak` (§8.3.4.1);
a malformed writer that emits two `trgr` containers in one `trak` is
tolerated first-wins (matching `tapt` / `load` / `cslg` / `clip` /
`matt` conservative-merge policy at trak scope). §8.3.4.1 is explicit
that track groups indicate **shared characteristics or relationships**,
not **dependencies** — those stay the `tref` Track Reference Box's
job, and this parser does not blur the two surfaces. QTFF does not
define this box; it is ISO BMFF-only and stays empty for plain `.mov`
inputs. The round-176 fuzz harness extends to walk every track's
`trgr` entry list (capped at 16/track to bound pathological inputs)
plus the file-level `track_groups()` bucket aggregator and a
`tracks_in_group` probe derived from the input's first four bytes.

Round 187 closes the first finding from the scheduled fuzz harness:
`crash-353fbd8c75a517f36da693fcea9b24d24240fc5e` declared a `size=1`
extended-size atom with `largesize = u64::MAX` after an 8-byte
placeholder. The walker's `body_end = payload_offset + (total_size -
header_len)` overflowed `u64` on the addition step (debug builds
panicked; release builds would silently wrap). The fix anchors the
defence at `read_atom_header`: any header whose declared `start +
total_size` overflows `u64` is rejected before downstream layers
ever compute a `body_end`. Pinned by
`tests/synth_round187_extended_size_overflow.rs` (the verbatim
crash bytes, the focused header-level rejection, the
`start + largesize == u64::MAX` acceptance boundary, and the same
overflow shape nested inside a `moov` so `walk_children`'s
arithmetic site is covered too).

Round 210 parses the **Degradation Priority Box** (`stdp`) — ISO/IEC
14496-12 §8.5.3 — at `stbl` scope. The box carries one 16-bit
unsigned `priority` per sample; transports that selectively discard
samples under load (RTP stacks, bandwidth-adaptive segmenters) use
the value to choose which samples to drop. The on-disk table has no
count field — §8.5.3.1 sizes the row count from the `stsz`/`stz2`
`sample_count` — so the demuxer defers the parse until after the
`stbl` walk completes, mirroring the `sdtp` deferred-sizing path
landed in round 98. New [`SampleTable::stdp: Vec<u16>`] field plus
[`SampleTable::sample_degradation_priority(sample_idx)`] and
[`MovDemuxer::sample_degradation_priority(track, sample)`] accessors
surface the value 1:1 with what the writer emitted; the spec leaves
the numeric meaning and acceptable range to specifications derived
from the base format (§8.5.3.1, §8.5.3.3) so the raw `u16` is the
right surface to hand back to callers that consult the derived spec
carrying the `stdp` track. Rejected at open time: payload shorter
than the 4-byte FullBox header, non-zero `flags` (§8.5.3.2 defines
`FullBox('stdp', version = 0, 0)` — silent acceptance would let a
malformed writer leak undefined bits past the parser), and a body
shorter than `sample_count × 2` bytes (the truncated-table case
mirrors `sdtp`'s sizing guarantee). Trailing padding past the
declared row count is silently ignored — some writers round the box
up to an 8-byte boundary, and §8.5.3.2 names exactly `sample_count`
rows. A duplicate `stdp` inside the same `stbl` is tolerated
first-wins (§8.5.3 lists the box as `Quantity: Zero or one`;
first-wins matches the conservative-merge policy applied to every
other "at most once" stbl-scope box — `sdtp`, `sbgp`/`sgpd`,
`saiz`/`saio`, the sample-size boxes themselves). QTFF does not
define this box; it is ISO BMFF-only and stays empty for plain
`.mov` inputs. The round-176 fuzz harness extends to call the new
per-track `sample_degradation_priority` accessor on a couple of
attacker-influenced sample indices (zero plus a value derived from
the input's first 32-bit word) so an `stdp`-carrying fuzz input
reaches the deferred parse and the bounded `Vec::get` accessor
without panicking.

Round 216 parses the **Track Input Map atom** (`imap`) — Apple
QuickTime File Format Specification (QTFF, 2001-03-01) pp. 51 – 53 /
Figure 2-14 — at per-track scope (`moov/trak/imap`). The atom tells the
QuickTime engine how each non-primary source (a `tref` reference of
type `'ssrc'`, QTFF p. 50 Table 2-2) modulates this track's
presentation: a transform matrix on the track's location/scaling, a
QuickDraw clipping region scoped to the track's shape, an 8.8
fixed-point sound volume curve for fades, a 16-bit sound balance
level for panning, a graphics-mode record for visual fades, or a
per-object variant of any of the above scoped to one sub-track
construct (a sprite, a tween). `imap` is the only QT-atom-shaped
container the parser supports today: its body holds one or more track
input atoms (` in`, with the leading two bytes 0x00 per QTFF p. 52)
each of which carries a 12-byte QT-style header tail (`atom_id` +
reserved + `child_count` + reserved) before its own classic-shaped
child atoms (` ty` required, `obid` optional). The required ` ty`
input-type atom carries a 4-byte identifier classified into the
[`InputTypeKind`] enum across QTFF Table 2-3's eight values
(`Matrix` / `Clip` / `Volume` / `Balance` / `GraphicsMode` /
`ObjectMatrix` / `ObjectGraphicsMode` / `Image`, plus an
[`InputTypeKind::Other`] fall-through that preserves any vendor or
future-spec raw value). `kTrackModifierTypeImage` is on-disk the FourCC
`'vide'` — QTFF reuses the video-media-type marker as the input-type
identifier — surfaced bit-exactly via
[`K_TRACK_MODIFIER_TYPE_IMAGE`]. The three per-object identifiers
(`ObjectMatrix` / `ObjectGraphicsMode` / `Image`) require an
accompanying `obid` child carrying the object id, and the parser
enforces this cross-field consistency rule on QTFF p. 53 at open
time. The 1-based [`TrackInputEntry::atom_id`] indexes into the parent
track's `'ssrc'` reference list (QTFF p. 53: "the first secondary
input corresponds to the track input atom with an atom ID value of
1") — callers resolve an entry against the parent via
`track.track_refs_of_kind(NonPrimarySource)[atom_id - 1]`. Two
accessors land on the demuxer: `MovDemuxer::track_input_map(track)`
returns the parsed [`TrackInputMap`] (or `None` when the track omits
the atom), and `TrackInputMap::entry_for_ssrc_slot(id)` does the
atom-id-keyed lookup since writers are not strictly required to emit
entries in atom-id order. Rejected at open time: an ` in` body shorter
than the 12-byte QT-style header tail, non-zero values in either
reserved field, a missing required ` ty`, a ` ty` or `obid` body that
is not exactly 4 bytes, a duplicate ` ty` or `obid` inside one ` in`,
an unexpected child FourCC inside ` in` or inside `imap` itself, a
declared `child_count` that disagrees with the number of children
actually parsed, and a per-object input-type identifier paired with no
`obid`. The wrapper atom follows the trak-scope first-wins
duplicate-merge policy shared with `tapt` / `load` / `cslg` / `clip` /
`matt`. ISO BMFF does not define `imap` (it is QuickTime-only); for
plain MP4 / fMP4 / HEIF / AVIF inputs `MovDemuxer::track_input_map`
returns `None`. The round-176 fuzz harness extends to walk every
track's `imap` entry list (capped at 16 entries/track to bound
pathological inputs) and exercises the `entry_for_ssrc_slot` lookup
against an attacker-influenced 1-based slot id derived from the
input's first 32-bit word.

Round 219 parses the **Subsegment Index Box** (`ssix`) — ISO/IEC
14496-12 §8.16.4 — at file scope, completing the §8.16 segment-index
surface alongside the round-114 `sidx` parser. The FullBox pairs
one-to-one with the immediately preceding `sidx` box that indexes only
leaf subsegments (`Quantity: 0 or 1` per associated `sidx`,
§8.16.4.1) and partitions each subsegment into level-keyed *partial
subsegments* — a compact "table of contents" letting a DASH / CMAF
client fetch only the bytes for a chosen Level Assignment Box
(§8.8.13) level (e.g. the lowest temporal-scalability layer of a
multi-layer video segment). Layout per §8.16.4.2 is
`subsegment_count[4]`, then per subsegment a `range_count[4]` and
`range_count`-many `(level[1], range_size[3])` rows; the per-range
`range_size` is 24-bit unsigned per §8.16.4.2 / §8.16.4.3. Each
[`SsixSubsegment`] surfaces the partial-subsegment list verbatim and
[`Ssix`] exposes `subsegment_count()`, `total_size_for(index)`
(§8.16.4.1's "each byte assigned to a level" invariant — sums
`range_size` across a subsegment's chain, widened to `u64` because the
per-range 24-bit cap doesn't bound the whole subsegment), and
`partial_subsegment_offset(subsegment_start, index, range_index)`
(walks the `range_size` chain from a caller-supplied subsegment start,
typically sourced from the paired `sidx`'s
[`Sidx::subsegment_offset`]). Surfaces on the demuxer via
`MovDemuxer::ssix: Vec<Ssix>` (file order — every parsed box stays
visible) plus the `MovDemuxer::ssix_for_sidx(sidx_index)` accessor:
the demuxer's top-level walker records the §8.16.4.1 pairing at parse
time so the lookup is O(1) and doesn't rely on the caller knowing
on-disk box order. Orphan `ssix` (out-of-order or following something
other than `sidx`) is still parsed and surfaced through the public Vec
but is not bound to any `sidx`. A non-`sidx`/`ssix` top-level box
between a `sidx` and the following `ssix` breaks the pairing window
per §8.16.4.1's "the next box after the associated Segment Index box"
rule. Rejected at open time: unknown FullBox `version` (spec fixes at
0), payload shorter than the 8-byte FullBox header + `subsegment_count`
u32, a declared `range_count` below 2 (§8.16.4.1: every byte must be
assigned to a level so a single partial subsegment is illegal), a
`subsegment_count` or `range_count` overrun, and any trailing bytes
past the declared subsegment list (the box carries no list past the
final subsegment). The up-front bound on `subsegment_count × 4`
against remaining body bytes rejects a forged huge count before
allocating `Vec::with_capacity`. QTFF does not define this box; it is
ISO BMFF-only and stays absent for plain `.mov` inputs. The round-176
fuzz harness extends to walk every collected `ssix` entry (capped at
64 to bound pathological writers) through `total_size_for` /
`partial_subsegment_offset` with attacker-influenced indices and
overflow-prone anchor values, then exercises the `ssix_for_sidx`
cross-reference path against each declared `sidx`.

Round 226 parses the **Level Assignment Box** (`leva`) — ISO/IEC
14496-12 §8.8.13 — at `moov/mvex` scope, naming the *levels* the
round-219 §8.16.4 Subsegment Index Box (`ssix`) references.
Adaptive-streaming clients pair the two so a temporal-scalability
decoder can fetch only the bytes for the base-layer level and skip
the enhancement layers (§8.8.13.1). Layout per §8.8.13.2 is a 1-byte
`level_count` followed by `level_count` rows of `(track_id[4],
padding_flag[1 bit], assignment_type[7 bits])` plus a per-
`assignment_type` trailer: type 0 carries a 4-byte `grouping_type`
(sample-group assignment), type 1 carries
`grouping_type[4] + grouping_type_parameter[4]` (parameterized
sample-group assignment), types 2 and 3 carry no trailer (track-keyed
assignment; §8.16.4 distinguishes the two on the consumer side), and
type 4 carries a 4-byte `sub_track_id` (sub-track assignment, §8.14).
The new [`Leva`] / [`LevaLevel`] / [`AssignmentType`] types expose
`Leva::level_count()`, `Leva::level(j)` (1-based per §8.8.13.3 "loop
entry j"), and `Leva::track_ids()` (declaration-order de-duplicated
`track_id` set — useful when wiring the box to a §8.8 track table).
Surfaces on the demuxer via `MovDemuxer::leva: Option<Leva>`
populated through `parse_mvex`; the field stays `None` when the file
omits the box, when the file is a plain `.mov` (QTFF does not define
`leva`), or when the file is a non-fragmented MP4 with no `mvex`
container. Rejected at open time: unknown FullBox `version` (§8.8.13.2
spec-fixes at 0), payload shorter than the 5-byte FullBox header +
`level_count` byte, `level_count` below 2 (§8.8.13.3 fixes the
minimum at 2), body shorter than `level_count × 5` (every row carries
at least the 5-byte `track_id + flag/type` prefix), a per-type
trailer that overruns the remaining body, §8.8.13.3 ordering-rule
violations ("The sequence of assignment_types is restricted to be a
set of zero or more of type 2 or 3, followed by zero or more of
exactly one type" — once a non-2/3 row appears, every subsequent
non-2/3 row must carry the same `assignment_type`, and a 2/3 row may
not follow a pinned tail-block row), and any trailing bytes past the
declared row list. `Reserved { raw }` rows surface unknown
`assignment_type` values (`5..=127`) verbatim rather than rejecting
so a future derived spec adding a new code does not break this
parser; the reserved row consumes no trailer because the spec leaves
the payload unspecified. A malformed writer emitting two `leva`
boxes inside one `mvex` is tolerated first-wins (§8.8.13.1 fixes
Quantity at Zero or one; first-wins matches the conservative-merge
policy applied to `mehd`, `ctab`, `clip`, `pdin`, and the other
singletons). QTFF does not define this box; it is ISO BMFF-only and
stays absent for plain `.mov` inputs. The round-176 fuzz harness
extends to walk the optional `MovDemuxer::leva` row list (capped at
64 to bound a writer cramming the max 255 rows) touching
`level_count`, the per-row `track_id` / `padding_flag` /
`assignment_type` accessors, `track_ids`, and the 1-based `level()`
boundaries (0 and `level_count`+1) so the off-by-one path stays
covered on every `leva`-carrying fuzz input.

Round 234 parses the **Padding Bits Box** (`padb`) — ISO/IEC
14496-12 §8.7.6 — at `stbl` scope. The box records, for each sample,
how many bits at the end of the sample's media payload are
writer-inserted padding to round up to a whole-byte boundary; the
value matters when a downstream stage must re-emit the original
bit-stream verbatim (§8.7.6.1: "In some streams the media samples do
not occupy all bits of the bytes given by the sample size, and are
padded at the end to a byte boundary. In some cases, it is necessary
to record externally the number of padding bits used."). Unlike
`sdtp` / `stdp`, `padb` carries its own `sample_count` field on disk
(§8.7.6.2) so the parse runs at walk time and does not depend on
`stsz` / `stz2`. Layout per §8.7.6.2 is
`[version:1][flags:3][sample_count:4]` then `((sample_count + 1) /
2)` packed bytes, each holding `[reserved:1, pad1:3, reserved:1,
pad2:3]` most-significant nibble first; `pad1` covers sample
`(i*2)+1` (1-based per §8.7.6.3) and `pad2` covers sample `(i*2)+2`.
For an odd `sample_count` the trailing low nibble of the final byte
is the `pad2` slot for a non-existent "sample N+1" and is silently
discarded. New [`SampleTable::padb: Vec<u8>`] field plus
[`SampleTable::sample_padding_bits(sample_idx)`] and
[`MovDemuxer::sample_padding_bits(track, sample)`] accessors return
the 3-bit `pad` value (`0..=7`) 1:1 with what the writer emitted.
Rejected at open time: payload shorter than the 8-byte FullBox
header + `sample_count` u32, unknown FullBox `version` (§8.7.6.2
spec-fixes at 0), non-zero `flags` (§8.7.6.2 spec-fixes at 0; silent
acceptance would let a malformed writer leak undefined bits past the
parser), body shorter than `(sample_count + 1) / 2` packed bytes
(truncated table), and a non-zero `reserved` bit in either nibble of
any packed byte (§8.7.6.2 spec-fixes both the 0x80 and 0x08 bits at
0). A duplicate `padb` inside the same `stbl` is tolerated first-wins
(§8.7.6.1 lists the box as `Quantity: Zero or one`; first-wins
matches the conservative-merge policy applied to every other "at
most once" stbl-scope box — `sdtp`, `stdp`, `sbgp`/`sgpd`,
`saiz`/`saio`). QTFF does not define this box; it is ISO BMFF-only
and stays empty for plain `.mov` inputs. The round-176 fuzz harness
extends to call the new per-track `sample_padding_bits` accessor on
two attacker-influenced sample indices (zero plus a value derived
from the second 32-bit word of the input) so a `padb`-carrying fuzz
input reaches the parser and the bounded `Vec::get` accessor without
panicking.

Round 240 promotes the round-5 `Gmin::graphics_mode: u16` and
`Gmin::balance: i16` raw fields into typed accessors driven directly
by QTFF Chapter 4 "Basic Data Types" — Table 4-2 (p. 200, "Graphics
Modes") and the Balance paragraph (p. 201). The new `GraphicsMode`
enum surfaces every named mode in Table 4-2 — `Copy` (`0x0000`),
`DitherCopy` (`0x0040`), `Blend` (`0x0020`), `Transparent`
(`0x0024`), `StraightAlpha` (`0x0100`), `PremulWhiteAlpha`
(`0x0101`), `PremulBlackAlpha` (`0x0102`), `Composition` (`0x0103`,
documented as tracks-only), and `StraightAlphaBlend` (`0x0104`) —
plus an `Other(u16)` fall-through that preserves the raw 16-bit code
for vendor or future-spec values without committing the parser to a
meaning. `GraphicsMode::raw()` round-trips back to the on-disk code
1:1 with `from_raw()`, and `GraphicsMode::uses_opcolor()` reports the
Table 4-2 "Uses opcolor" column (true for `Blend`, `Transparent`,
`StraightAlphaBlend`; false for `Other` so a caller doesn't read
meaning into an opcolor the spec hasn't bound to the unknown code).
`Gmin::graphics_mode_kind()` is the typed view of `graphics_mode`;
`Gmin::balance_as_f32()` decodes the 16-bit 8.8 signed
fixed-point field per p. 201 into the real-valued [-1.0, +1.0]
balance setting (high-order 8 bits = integer portion, low-order 8
bits = fraction; negative = left, positive = right, zero =
centered). The raw `graphics_mode: u16` and `balance: i16` fields
stay exposed for callers that need the exact on-disk encoding for
round-trip remuxing; the new accessors are additive. This round also
corrects a doc-comment slip on `Gmin::graphics_mode` that paired
`0x0100` with "transparent" — Table 4-2 fixes the transparent code at
`0x0024` and reserves `0x0100` for straight alpha.

Round 243 extends the typed `tref` surface to cover every remaining
QTFF Table 2-2 (p. 50) reference kind — Apple QuickTime File Format
Specification (2001-03-01) pp. 49 – 51, Figure 2-13 "Track reference
atom" layout, Table 2-2 "Track reference types". Round 240 left
[`Track::chapter_track_ref`] and [`Track::timecode_track_ref`] typed
but the remaining four kinds (`'sync'` synchronization between peer
tracks, `'scpt'` transcript pairing with a text track, `'hint'` hint-
track source media for RTP packetization, `'ssrc'` non-primary source
modulating presentation via the round-216 `imap`) were reachable only
via the generic [`Track::track_refs_of_kind`] helper. The four new
symmetrical accessors are [`Track::sync_track_refs`],
[`Track::transcript_track_refs`], [`Track::hint_track_refs`], and
[`Track::non_primary_source_track_refs`]; each returns the
declaration-ordered list of 1-based `tkhd.track_id` values across
every reference-type atom of that kind, with the spec p. 51 `0`-valued
"unused-entry slot" sentinel filtered out so callers see only
resolvable ids. Demuxer-side track-id-to-index resolvers complete the
surface so callers no longer have to walk
`MovDemuxer::tracks.iter().position(|t| t.tkhd.track_id == id)` by
hand: [`MovDemuxer::track_index_for_id`] is the underlying lookup
that translates a 1-based `tkhd.track_id` to its 0-based index inside
[`MovDemuxer::tracks`] (returns `None` for the `0` sentinel and for
any id missing from the file); a generic
[`MovDemuxer::tref_track_indices`] resolves every `tref/<kind>`
reference declared by a `track_index` to the 0-based peer indices;
and five per-kind helpers — [`MovDemuxer::timecode_track_index`] (the
first resolvable entry as `Option<usize>`, matching the existing
[`Track::timecode_track_ref`] singleton shape),
[`MovDemuxer::sync_track_indices`],
[`MovDemuxer::transcript_track_indices`],
[`MovDemuxer::hint_track_indices`], and
[`MovDemuxer::non_primary_source_track_indices`] — wrap the generic
resolver. The 0-id slot and unresolvable ids (writer slip — the
pointed-at track is absent from the file) are both filtered out at
the demuxer resolver layer; declaration order is preserved across
every reference-type atom of the requested kind, so a track that
emits two separate `'hint'` rows surfaces all member ids in a single
declaration-ordered list. Out-of-range `track_index` returns the
empty surface (the accessors stay total functions). The
[`MovDemuxer::non_primary_source_track_indices`] resolver pairs
directly with the round-216 [`TrackInputMap::entry_for_ssrc_slot`]
lookup — the 1-based atom-id slots inside the `imap` index into the
ordered list returned by the resolver. The underlying
[`Track::references`] raw surface stays public; the new accessors are
purely additive and no parsing behaviour changes.

Round 246 completes the round-74 / round-91 edit-list mapper with its
**inverse direction**: `movie_pts → media_pts` (QTFF Chapter 2 "Edit
Atoms" pp. 46 – 48 and Chapter 5 "Playing With Edit Lists" pp. 226 –
227). The new free function
[`movie_pts_to_media_pts`]`(segments, movie_pts, movie_timescale,
media_timescale) -> Option<i64>` is the symmetric counterpart of
[`media_pts_to_movie_pts`]: the typical caller is a seek-by-
presentation-time entry point — the user requests "jump to 0:30 in the
movie", the helper converts that movie-time to a media-time, and the
caller drives the per-track sample walker (`MovDemuxer::seek_to`,
whose input is already media-PTS) with the resolved value. Two thin
wrappers complete the surface: [`Track::movie_pts_to_media_pts`]
resolves the track's edit segments against a supplied movie timescale
and routes through the free function (mirroring the existing
[`Track::media_pts_to_movie_pts`] symmetry), and
[`MovDemuxer::media_pts_for`] is the demuxer-level inverse of
[`MovDemuxer::movie_pts_for`] honouring the parsed `mvhd` timescale
and duration. Algorithm scans the resolved segment list in
declaration order and matches each segment's half-open
`[movie_time_start, movie_time_end)` window against the queried
`movie_pts`; zero-duration segments collapse to the single boundary
tick. [`EditSegmentKind::Empty`] returns `None` (the movie-time slice
emits silence/black per QTFF p. 47 and so has no media correspondence);
[`EditSegmentKind::Dwell`] returns the held `media_time` (ISO/IEC
14496-12 §8.6.6.3 every movie-time tick in the segment maps to the
same media frame); [`EditSegmentKind::Media`] inverts the round-91
formula via `Δmedia = Δmovie × media_ts × rate_fp / (movie_ts ×
65536)`, mirroring the QTFF p. 226 – 227 worked example (600 movie
ticks at media_rate 2.0 consume 200 media ticks, so 1 movie tick at
rate 2.0 advances the source by 2 media ticks). Rate stays 16.16
fixed-point so the arithmetic remains integer end-to-end; rounding is
half-up via `(num + denom/2) / denom` matching the convention used
everywhere else in this module. QTFF p. 48 forbids `media_rate <= 0`
on a Media segment so the helper rejects those segments on a per-
segment basis and continues scanning. Negative `movie_pts` always
returns `None` — the presentation timeline starts at movie tick 0.
The round-176 fuzz harness extends to call `media_pts_for` on the
same three boundary `movie_pts` values it already probes for the
forward mapper (`0` / `i64::MIN` / `i64::MAX`) plus a value derived
from input bytes 8 – 15, so the fixed-point math runs against
attacker-influenced inputs without panicking — matching the existing
forward-direction fuzz coverage.

Round 256 exposes the QTFF p. 79 "Finding a Sample" four-step
walker as a **typed random-access surface** over the `stsc`
Sample-to-Chunk (p. 75), `stco` / `co64` Chunk Offset (p. 78), and
`stsz` Sample Size (p. 76) tables. The decode-order
[`SampleTable::iter_samples`] walker has always summed `stsz` sizes
inside each chunk to locate per-sample byte offsets, but the only
way to ask "which chunk holds sample N" or "what is the absolute
file offset of sample N" without iterating every prior sample was to
re-implement the walker out-of-tree. The new accessors close that
gap. On [`SampleTable`]: [`SampleTable::chunk_count`] (length of the
`stco` / `co64` table), [`SampleTable::samples_in_chunk`] (the
`samples_per_chunk` of the `stsc` row that applies to 1-based
`chunk_1based`, per QTFF p. 76 "Each table entry corresponds to a
set of consecutive chunks…"), [`SampleTable::sample_description_id_for_chunk`]
(the row's `sample_description_id`),
[`SampleTable::chunk_first_sample`] (0-based decode-order index of
the first sample in the chunk — sums `samples_per_chunk` across
preceding chunks within and across `stsc` rows),
[`SampleTable::chunk_for_sample`] (the random-access form of step 2
of QTFF p. 79: scans `stsc` rows to find the chunk whose
sample-range covers 0-based decode-order `sample_idx`, returning
`(chunk_1based, sample_offset_in_chunk_0based)`),
[`SampleTable::sample_size_at`] (uniform-or-table `stsz` /
`stz2` lookup wrapped as one accessor), [`SampleTable::sample_offset`]
(mirrors all four steps end-to-end: chunk-base from `chunk_offsets`
plus the sum of every earlier sample's size inside the chunk —
companion of [`SampleTable::iter_samples`] but without iterating
prior samples), and [`SampleTable::chunk_byte_extent`] (total file
byte span of a chunk as `(start, end_exclusive)` for chunk-aligned
prefetch or HTTP-range reads, per QTFF p. 74 "Chunks ... allow
optimized data access"). Five corresponding [`MovDemuxer`]
wrappers ([`MovDemuxer::chunk_count`],
[`MovDemuxer::samples_in_chunk`], [`MovDemuxer::chunk_for_sample`],
[`MovDemuxer::sample_offset`], [`MovDemuxer::chunk_byte_extent`])
take a 0-based `track_index` and otherwise delegate. The accessors
are purely additive: existing iter-walker and `Packet` byte
offsets are unchanged, and no parsing behaviour is touched. Total
functions across out-of-range inputs (zero or past-end chunk
number, sample index past `sample_count`, malformed
`samples_per_chunk == 0` row) — every error path returns `None`
rather than panicking, so the bounded `Vec::get` discipline applied
to the rest of the sample-table surface carries through to the new
shape. Pinned by 9 unit tests in `src/sample_table.rs`
(QTFF p. 76 Figure 2-35 worked-example layout: 3 `stsc` rows
spanning 5 chunks — 3+3+1+1+1 samples — exercising every accessor
plus the single-row "common case" shape, the empty-`stbl`
fragmented-only shape, and the malformed `samples_per_chunk == 0`
shape) plus 5 integration tests in
`tests/synth_round256_chunk_walking.rs` against a hand-built QT
file carrying the same QTFF p. 76 Figure 2-35 layout, verified
end-to-end through the public [`MovDemuxer`] accessors with the
demuxer-resolved offsets cross-checked against the actual mdat
bytes at the resolved positions. The round-176 fuzz harness
extends to call all five demuxer accessors with attacker-derived
chunk numbers and sample indices (including zero, `chunk_count + 1`,
and a value drawn from the input's first / second 32-bit words),
so a fuzz input crafting a row count vs chunk-offset table mismatch
reaches every accessor without panicking.

Decoding stays in codec crates; this crate calls
`oxideav_core::CodecResolver` to map sample-description FourCCs to
`CodecId`s and never opens a decoder itself (per
`docs/IMPLEMENTOR_ROUND.md` §"Crate-purpose discipline").

Round 259 lands the Compressed Movie atom (`cmov`) parser and its
two subatoms, the Data Compression atom (`dcom`) and the Compressed
Movie Data atom (`cmvd`) — QuickTime File Format spec pp. 80 – 81 /
Table 2-5. Beginning with QuickTime 3 (p. 80), a writer may
losslessly compress the movie resource; the resulting file's
top-level `moov` carries a single `cmov` child whose `dcom`
identifies the compression algorithm (4-byte FourCC; `'zlib'` is the
field-observed value but the spec leaves the field generic) and
whose `cmvd` carries a 4-byte big-endian uncompressed size followed
by the compressed payload. New `cmov` module exposes [`parse_dcom`]
/ [`parse_cmvd`] / [`parse_cmov`] and the matching [`Dcom`] /
[`Cmvd`] / [`Cmov`] result types, plus `DCOM_BODY_LEN`,
`CMVD_MIN_BODY_LEN`, and `DCOM_ALG_ZLIB` constants. Scope is
deliberately narrow: the parser surfaces the on-disk structure of
all three atoms but does **not** perform the decompression step. A
follow-up round can wire the workspace's compression crate behind
the algorithm FourCC and feed the uncompressed inner movie back
through the existing parser; the current round keeps the surface as
a free-function entry point so the `moov` walker is unchanged and
no behaviour-change-on-compressed-input regression is possible. 19
unit tests in `src/cmov.rs` cover the canonical Table 2-5 layout,
reversed child order, unknown sibling atoms ignored, missing-child
rejection, duplicate-child first-wins, QTFF p. 19 open-ended
`size == 0` on the trailing child, the `u32::MAX` uncompressed-size
boundary, and short / empty body rejection on all three leaf
parsers. Both the `registry` feature and the standalone
configuration build and pass.

Round 264 lands the Field Handling (`fiel`) video sample-description
extension parser plus the typed gamma (`gama`) accessor — Apple
QuickTime File Format spec p. 94, Table 3-2 "Video sample
description extensions". Table 3-2 lists `fiel` as a two-byte
extension carrying a field count (`1` progressive, `2` interlaced)
and, when the count is 2, a field-ordering selector (`0` "field
ordering is unknown", `1` T-displayed-earliest/T-stored-first =
top-field first, `6` B-displayed-earliest/B-stored-first =
bottom-field first). The atom was previously opaque inside
[`SampleDescription::extra`]; round-2 recognised `gama` / `pasp` /
`clap` / `colr` but not `fiel`. New `media_meta` additions: typed
[`Fiel`] struct preserving both raw bytes for round-trip fidelity,
typed [`FieldOrdering`] enum naming exactly the three spec-listed
values (no fall-through variant — the spec leaves any other byte's
meaning unspecified), free function [`parse_fiel`] enforcing the
fixed 2-byte body length, `FIEL_BODY_LEN = 2` constant, and three
typed accessors on [`Fiel`]: `is_interlaced()` (`field_count == 2`),
`is_spec_field_count()` (`field_count ∈ {1, 2}`), and `ordering()`
returning the named [`FieldOrdering`] variant or `None` when the
byte is outside the enumerated set. The companion `gama` accessor
[`SampleDescription::gamma_value`] returns the typed 16.16
fixed-point view of the raw `Option<u32>` (divides by 65536.0,
returns `None` when the field is absent — no default substitution).
The 16.16 reading mirrors every other "32-bit fixed-point" QTFF
field in Chapter 4 (mvhd `rate`, tkhd matrix coefficients, `tapt`
width/height, sound `balance`). Both fields are populated by the
existing `scan_video_extensions` walker so no caller code changes
are required to receive them; the round is purely additive. 8 new
unit tests in `src/media_meta.rs` plus 6 end-to-end demuxer
integration tests in `tests/synth_round264_fiel.rs` pin every
spec-named shape, the out-of-spec field-count preservation across
{0, 3, 17, 0x80, 0xFF}, the unspec ordering-byte fall-through, and
the body-length rejection on lengths {0, 1, 3, 4, 8}. ISO BMFF does
not define `fiel`; it is a QuickTime-only extension and stays absent
for non-QTFF inputs reaching this demuxer.

## Follow-ups

- `MovMuxer` write-side `saiz` / `saio` emission at either `stbl` or
  `traf` scope: the round-150 read path consumes both placements
  (ISO/IEC 14496-12 §8.7.8.1 / §8.7.9.1) but the encoder never writes
  them. Producers that need to round-trip CENC sample-aux records
  through this crate currently have to hand-author the boxes.
- A `next_packet`-side opt-in (`MovDemuxer::with_edit_list_pts()`?)
  that swaps the emitted `Packet::pts` from media-time to movie-time
  end-to-end, so consumers that don't want the explicit
  `movie_pts_for` call site get spec-correct presentation timing for
  free. Round 74 keeps the existing media-time PTS contract on
  `next_packet` to avoid a silent behaviour change.
- ffmpeg-encoded `media_rate` round-trip fixture: round 91 validates
  the math against the QTFF worked example via synth fixtures, but a
  real `ffmpeg -filter:v setpts=PTS*2` reference would harden against
  rounding-convention drift.
- Compressed-movie (`cmov`) `moov`-walker integration: round 259
  ships `parse_cmov` / `parse_dcom` / `parse_cmvd` as free-function
  entry points, but the top-level `moov` walker still ignores the
  atom. Wiring it in needs a decompressor for the `dcom` algorithm
  FourCC (commonly `'zlib'`); the workspace's compression crate is
  the natural producer, and the uncompressed inner bytes feed back
  through the existing `moov` parser unchanged.

## Standalone build

`oxideav-core` is gated behind the default-on `registry` cargo
feature. Drop the framework dependency entirely with:

```toml
oxideav-mov = { version = "0.0", default-features = false }
```

The parser API (`atom`, `header`, `sample_table`, `track`) stays
available against a crate-local `Error`/`Result`; the `Demuxer`
trait impl and `register()` entry point disappear.

## License

MIT — see `LICENSE`.
