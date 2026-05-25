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

Decoding stays in codec crates; this crate calls
`oxideav_core::CodecResolver` to map sample-description FourCCs to
`CodecId`s and never opens a decoder itself (per
`docs/IMPLEMENTOR_ROUND.md` §"Crate-purpose discipline").

## Follow-ups

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
