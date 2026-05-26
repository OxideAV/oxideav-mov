# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Round 144 — Track Matte atom (`matt`) + Compressed Matte atom (`kmat`)
  parsers, QTFF p. 44 / p. 45.
  - `parse_kmat(payload) -> Result<CompressedMatte>` in the new `matte`
    module. Layout per QTFF p. 45: `version[1]` (spec fixes at 0;
    unknown rejected) + `flags[3]` (spec "Set this field to 0";
    non-zero rejected) + image description structure (carved using the
    leading 4-byte size word per QTFF p. 70; minimum 16 bytes for
    size + format FourCC + 6 reserved + dref index) + trailing
    compressed matte data (opaque to the parser; surfaced verbatim for
    the codec the image description names). Rejected: payload < 8
    bytes; unknown `version`; non-zero `flags`; image-description size
    < 16; image-description size overrunning the body.
  - `parse_matt(payload) -> Result<Matte>` walks the `matt` wrapper's
    children, picking the single spec-defined `kmat` child per QTFF
    p. 44 Figure 2-9. Tolerates unknown sibling atoms (forward-compat);
    rejects a `matt` body with no `kmat` child; first-wins on
    duplicate `kmat` children (matches the conservative-merge policy
    applied to `clip` / `tapt` / `load` / `cslg` at track scope).
  - `Matte { compressed: CompressedMatte }`,
    `CompressedMatte { version: u8, flags: u32, image_description:
    Vec<u8>, matte_data: Vec<u8> }` typed surfaces, plus
    `CompressedMatte::data_format() -> Option<[u8; 4]>` (codec FourCC
    at offset 4 of the image description per QTFF p. 70 "Data format")
    and `CompressedMatte::image_description_size() -> u32` accessor.
  - `Track.matte: Option<Matte>` field, populated by `parse_trak` from
    a `moov/trak/matt` child atom. QTFF p. 41 Figure 2-6 places `matt`
    inside individual tracks (siblings of `tkhd` / `mdia` / `edts` /
    `tref` / `load` / `imap` / `clip` / `udta`); there is no
    movie-level matte (a movie's matte is the union of its tracks').
  - `MIN_IMAGE_DESCRIPTION_SIZE` public const (16) — the QTFF p. 70
    universal lower bound used by the parser to reject malformed
    embedded structures.
  - `MATT` / `KMAT` FourCC constants in `atom` module.
  - 14 in-module unit tests + 6 integration tests
    (`synth_round144_matte.rs`) covering minimum-shape round trip,
    extended image description carving, empty matte data, absent
    matte, duplicate-merge first-wins, malformed-kmat rejection at
    open time, and forward-compat sibling tolerance.
- Round 140 — Clipping atom (`clip`) + Clipping Region atom (`crgn`)
  parsers, QTFF p. 43 / p. 44.
  - `parse_crgn(payload) -> Result<ClippingRegion>` in the new `clip`
    module. Layout per QTFF p. 44: `region_size[2]` (u16 BE; counts
    itself plus the 8-byte bounding box, so minimum legal value is 10)
    + `bounding_box[8]` (QuickDraw `Rect` — four 16-bit BE signed
    integers in top/left/bottom/right order) + optional
    `region_size - 10`-byte opaque QuickDraw scanline tail. Rejected:
    payload < 10 bytes; `region_size < 10`; `region_size` overshoots
    payload length; trailing bytes past the declared `region_size`.
  - `parse_clip(payload) -> Result<Clipping>` walks the `clip`
    wrapper's children, picking the single spec-defined `crgn` child
    per QTFF p. 43 Figure 2-8. Tolerates unknown sibling atoms
    (forward-compat); rejects a `clip` body with no `crgn` child;
    first-wins on duplicate `crgn` children (matches the conservative-
    merge policy applied to `mvhd` / `pdin` / `ctab` elsewhere).
  - `Clipping { region: ClippingRegion }`,
    `ClippingRegion { region_size: u16, bounding_box: QdRect,
    region_data: Vec<u8> }`, and `QdRect { top, left, bottom, right:
    i16 }` typed surfaces. `QdRect::width()` / `height()` return
    `i32` to avoid sign-bit overflow on a rect spanning the full i16
    range; `is_empty()` follows the QuickDraw zero-or-negative-extent
    convention. `ClippingRegion::is_rectangular()` is true for the
    minimum legal region (no scanline data).
  - `MovDemuxer.clipping: Option<Clipping>` field, populated by the
    `moov` walker. QTFF places `clip` as a movie-level sibling of
    `mvhd` / `trak` / `udta` / `ctab`; at most one is kept per file
    with first-wins on duplicates.
  - `Track.clipping: Option<Clipping>` field, populated by `parse_trak`
    from the optional `moov/trak/clip` child. Track-level clipping is
    independent of movie-level clipping (both, either, or neither
    surface populated for any given file).
  - 14 unit tests (`clip::tests::…`) cover rectangular-region round-
    trip, scanline-tail preservation, signed-origin Rect decoding,
    empty-rect bounding box, the wrapper's single-`crgn` shape,
    duplicate-`crgn` first-wins, forward-compat unknown-sibling
    tolerance, missing-`crgn` rejection, inner parse-error
    propagation, `size == 0` trailing-child handling, and rejection
    of short payloads / `region_size < 10` / `region_size` overshoot
    / trailing-bytes corruption.
  - 7 integration tests (`synth_round140_clip.rs`) exercise the full
    demuxer-open surface against hand-built QuickTime files whose
    `moov` and `moov/trak` carry a `clip` at movie scope, track
    scope, both scopes, with a scanline tail, absent at both scopes,
    duplicated at movie scope (first-wins), and malformed (rejected
    at open time).
  - QTFF / Apple-only atom — ISO BMFF does not define `clip` or
    `crgn`; an MP4 / fMP4 / HEIF / AVIF file will not carry either
    and both demuxer fields stay `None`.

- Round 137 — Color Table atom (`ctab`) parser, QTFF p. 35.
  - `parse_ctab(payload) -> Result<Ctab>` in the new `ctab` module.
    Layout: `color_table_seed[4]` (must be 0) + `color_table_flags[2]`
    (must be 0x8000) + `color_table_size[2]` (zero-relative count;
    on-disk `N` ↔ `N+1` entries per QTFF p. 35) + N × 4-channel
    `[reserved:2][r:2][g:2][b:2]` color array. Rejected at open time:
    payload shorter than the 8-byte fixed header; non-zero
    `color_table_seed`; `color_table_flags != 0x8000`; body length
    that disagrees with the declared count (no padding, no trailing
    bytes — the color array runs to end-of-atom).
  - `Ctab { seed: u32, flags: u16, entries: Vec<ColorTableEntry> }`
    surfaces every entry verbatim; `color_count()` returns the
    typed entry count without the `u16 → u32` widening at the call
    site. `ColorTableEntry { reserved, red, green, blue: u16 }`
    preserves the on-disk `reserved` word (some authoring tools
    stash a Mac Toolbox `ColorSpec.value` index there even though
    QTFF fixes it at 0); `rgb8()` returns the high-byte 8-bit-per-
    channel triple for callers that don't need full 16-bit fidelity.
  - `MovDemuxer.ctab: Option<Ctab>` field, populated by the `moov`
    walker. QTFF places `ctab` as a movie-level sibling of `mvhd`
    and `trak`; at most one is kept per file with first-wins on the
    rare duplicate case (matching the `mvhd` / `pdin` conservative-
    merge convention).
  - 9 unit tests (`ctab::tests::…`) cover single-entry
    zero-relative-count corner, three-entry primary-RGB palette,
    full 256-entry palette, non-zero reserved-word preservation, and
    rejection of short payloads / non-zero seed / wrong flags /
    truncated color array / trailing bytes.
  - 6 integration tests (`synth_round137_ctab.rs`) exercise the
    full demuxer-open surface against a hand-built QuickTime file
    whose `moov` carries the `ctab` after the track: round-trips
    each of the spec-exercised shapes through `MovDemuxer::open` and
    confirms the absent / malformed / duplicate cases behave per
    spec.
  - QTFF / Apple-only atom — ISO BMFF does not define `ctab`; an
    MP4 / fMP4 / HEIF / AVIF file will not carry one and the
    demuxer's `ctab` field stays `None`.

- Round 128 — Producer Reference Time Box (`prft`) parser, ISO/IEC
  14496-12 §8.16.5.
  - `parse_prft(payload) -> Result<Prft>` in the new `prft` module.
    Layout per §8.16.5.2: FullBox header + `reference_track_ID[4]` +
    `ntp_timestamp[8]` + `media_time` (4 bytes under v0, 8 bytes under
    v1). Rejected: payload < 4-byte FullBox header; `version > 1`
    (§8.16.5.2 defines only v0 and v1); payload length not exactly
    `16` (v0) or `20` (v1) — the box has no list and no variable
    section, so any trailing bytes indicate corruption or an
    unparseable writer extension.
  - `Prft { version, reference_track_id: u32, ntp_timestamp: u64,
    media_time: u64 }` widens both `media_time` widths to `u64` and
    keeps the on-disk fields verbatim. Helpers: `ntp_seconds()` and
    `ntp_fraction()` decompose the NTP word into the RFC 5905 §6
    integer-seconds / fractional-seconds halves; `unix_micros()`
    converts to a microsecond Unix-epoch instant via the
    2 208 988 800 s NTP→Unix offset (constant
    `NTP_TO_UNIX_EPOCH_SECONDS`), returning `None` for pre-1970 NTP
    values.
  - `MovDemuxer.prft: Vec<Prft>` field, populated by the top-level
    walker. `Quantity: Zero or more` (§8.16.5.1); collected in file
    order so a caller stepping through a live segment stream sees every
    producer marker alongside its `moof`.
  - `MovDemuxer::first_prft() -> Option<&Prft>` surfaces the file's
    earliest producer time, which per §8.16.5.1 corresponds to the
    file's first movie fragment — the typical "catch up to live"
    anchor.
  - 11 unit tests (`prft::tests::…`) cover v0 / v1 round-trips, the NTP
    fraction → microseconds reduction, the pre-1970 `unix_micros`
    return, and the reject paths (unknown version, truncated header,
    truncated v0 / v1 body, trailing bytes, v0-extra-byte, plus a
    flags-nonzero tolerance check). 7 integration tests
    (`tests/synth_round128_prft.rs`) verify the full open-time path
    against synthetic ISO BMFF fixtures: single v0 / v1 boxes,
    multi-box file-order preservation, the empty-list / `first_prft()
    == None` case, and three reject paths (truncated, unknown version,
    trailing bytes).

- Round 125 — Segment Type Box (`styp`) parser, ISO/IEC 14496-12 §8.16.2.
  - `parse_styp(payload) -> Result<Styp>` in the new `styp` module.
    Layout per §8.16.2 (identical to §4.3 `ftyp` with the box-type
    FourCC switched): `major_brand[4]` + `minor_version[4]` +
    `compatible_brands[4]*` to end-of-box. Payloads shorter than the
    8-byte fixed header are rejected at open time, as is a
    `compatible_brands` tail length that is not a multiple of 4. An
    empty compatible-brands list is legal — a bare `[major][minor]`
    body is a valid segment-type box.
  - `Styp { major_brand: [u8;4], minor_version: u32, compatible_brands:
    Vec<[u8;4]> }` exposes the on-disk fields verbatim. Helpers:
    `has_brand(&[u8;4])` (major or compatible match);
    `is_dash_segment()` (true when any of `msdh` / `msix` / `risx`
    appear); `is_cmaf_segment()` (true when `cmfs` appears);
    `major_brand_class() -> BrandClass` shortcut into the existing
    classifier; `to_ftyp() -> Ftyp` conversion that lifts the box into
    the [`Ftyp`] shape so the rich `is_heic` / `is_avif` / `is_miaf`
    machinery defined on [`Ftyp`] becomes available for segment-level
    queries.
  - `MovDemuxer.styp: Vec<Styp>` field, populated by the top-level
    walker. `Quantity: Zero or more` (§8.16.2.1); collected in file
    order so a caller inspecting a concatenated segment stream can
    see every segment-boundary marker even though §8.16.2.1 permits
    ignoring any `styp` that isn't first.
  - `MovDemuxer::first_styp() -> Option<&Styp>` surfaces the
    §8.16.2.1 conformance declaration (the first `styp`, which is
    the only one a strict reader needs). `MovDemuxer::is_dash_segment()`
    and `MovDemuxer::is_cmaf_segment()` query the first `styp` for
    the DASH and CMAF segment-conformance brand families respectively.
  - `atom::STYP` FourCC constant added. The top-level walker
    dispatches on it alongside `FTYP`, `PDIN`, `SIDX`.
  - Seven synthetic-fixture integration tests
    (`tests/synth_round125_styp.rs`): DASH-major round-trip; multi-
    `styp` file-order preservation across a concatenated segment
    boundary; absence yields empty `Vec` + `false` classifiers;
    CMAF classifier on both major-brand and compatible-brand
    placement; truncated payload rejected at open time; unaligned
    compatible-brand tail rejected; empty compatible-brand list is
    legal. Plus 10 unit tests on the parser covering the header /
    tail bounds, `has_brand` match logic, DASH and CMAF classifier
    routes, `BrandClass` shortcut, `Ftyp` round-trip, and brand-
    order preservation.

- Round 122 — Track Kind box (`kind`) parser, ISO/IEC 14496-12 §8.10.4.
  - `parse_kind(payload) -> Result<KindEntry>` in the new `kind`
    module. Layout per §8.10.4.2: FullBox header (`version = 0`,
    `flags = 0`) followed by two NULL-terminated C strings —
    `schemeURI` and `value`. Unknown `version` (> 0) is rejected at
    open time; non-zero `flags` are accepted and ignored (consistent
    with how `parse_tsel` treats the §8.10.3.3 fixed-zero flags).
  - `KindEntry { scheme_uri: String, value: Option<String> }` —
    `value` surfaces as `None` when the box carries only a schemeURI
    (on-disk shape `[uri]\0\0`, the spec's §8.10.4.3 "URI identifies
    the kind itself" shape). A missing trailing NULL on either string
    is tolerated (the field runs to end-of-slice). UTF-8 decoding is
    best-effort via `String::from_utf8_lossy`, replacing malformed
    sequences with U+FFFD rather than rejecting the box.
  - `KindEntry::has_value()` — convenience predicate for "Some and
    non-empty", letting callers distinguish "URI-only kind" from
    "scheme + named value" in one call.
  - `find_kinds_in_udta(udta_payload) -> Result<Vec<KindEntry>>`
    collects every `kind` child of a track-level `udta` in file order.
    Unlike `find_tsel_in_udta` (which is first-match because `tsel` is
    `Quantity: Zero or one`), `kind` is `Quantity: Zero or more`
    (§8.10.4.1) so a track may legitimately carry multiple `kind`
    entries — one per role taxonomy (WebVTT, DASH, vendor-specific).
  - `Track.kinds: Vec<KindEntry>` field, populated by the per-`trak`
    walker. The `udta` body is re-walked for both `tsel` and `kind`
    in the same pass so the typed surfaces stay aligned with
    `Track.user_data` (which keeps the raw flat list for forensics).
  - `Track::track_kinds() -> &[KindEntry]` and
    `MovDemuxer::track_kinds(track_index) -> &[KindEntry]` accessors;
    the latter returns an empty slice for out-of-range indices and for
    `.mov` inputs (QTFF defines no `kind` equivalent — the box is ISO
    BMFF-only).
  - Six synthetic-fixture integration tests
    (`tests/synth_round122_track_kind.rs`): DASH role round-trip,
    WebVTT role round-trip, scheme-only / no-value, multi-entry
    file-order preservation, absence-from-udta yields empty slice,
    out-of-range index yields empty slice. Plus 14 unit tests on the
    parser covering version-rejection, truncated header, UTF-8
    fallback, NULL-terminator edge cases, and the udta-walker
    error-propagation contract.

- Round 118 — Sub-Sample Information Box (`subs`) parser, ISO/IEC
  14496-12 §8.7.7.
  - `parse_subs(payload) -> Result<Vec<SubSampleInfo>>` in the
    `sample_table` module. Layout per §8.7.7.2: FullBox header
    (`version` 0 or 1) + `entry_count`, then per row a
    `[sample_delta:4][subsample_count:2]` header followed by
    `subsample_count` sub-sample records of
    `[subsample_size:(2 if v0 else 4)][subsample_priority:1]
    [discardable:1][codec_specific_parameters:4]`. The sparse
    `sample_delta` is accumulated into an absolute 1-based
    `sample_number` (§8.7.7.3): the first row's delta is the difference
    from zero, each later row's from the previous row. A zero
    `sample_delta` (which would duplicate a sample number, or produce a
    0-numbered first sample) is rejected; unknown `version` (> 1) and a
    truncated record are rejected.
  - `SubSampleInfo` (one sparse row: `sample_number` + `subsamples`) and
    `SubSampleEntry` (`subsample_size` widened to `u32` across both
    versions, `subsample_priority`, `discardable`,
    `codec_specific_parameters`, plus `is_discardable()`) surfaced from
    `lib.rs`.
  - `SampleTable.subs: Vec<SubSampleInfo>` field, sorted ascending by
    `sample_number`. The `stbl` walker recognises `subs` as a child box;
    §8.7.7.1 permits more than one `subs` box per track (distinguished by
    `flags`), so rows from every box are merged — rows for the same
    sample concatenate their sub-sample lists in box order.
  - `SampleTable::sub_samples_for(sample_number)` (binary-searches the
    sorted table; a row that names a sample but lists zero sub-samples
    returns `Some(&[])`) and `MovDemuxer::sub_samples(track_index,
    sample_number)`. `sample_number` is **1-based**. QTFF does not define
    this box; it is ISO BMFF-only.

## [0.0.2](https://github.com/OxideAV/oxideav-mov/compare/v0.0.1...v0.0.2) - 2026-05-24

### Other

- Round 114 — Segment Index Box (sidx) parser, ISO/IEC 14496-12 §8.16.3
- Round 105 — Progressive Download Information Box (pdin) parser
- parse Shadow Sync Sample Box (stsh), ISO/IEC 14496-12 §8.6.3
- Round 98 — Independent and Disposable Samples Box (`sdtp`) parser
- Round 95 — Track Selection box (`tsel`) parser + demuxer wiring
- round 91: non-unity media_rate scaling in edit-list mapper
- Round 89 — Track Load Settings atom (`load`) parser + demuxer wiring
- Round 80 — sample groups (sbgp/sgpd) + typed roll/prol/rap lookups
- Round 74 — edit list (edts/elst) presentation-time honour + tkhd flags / alt-group surface
- Round 22 — HEIF / HEIC image-item WRITE path
- Fragmented-MP4 seek polish: regression coverage for 3 §8.8 edge cases
- Round 21 — fragmented-MP4 random-access seek via §8.8.10 tfra
- Implement Demuxer::seek_to on MovDemuxer
- Round 20 — fragmented MP4 / fMP4 / DASH muxer write side
- Round 19 — non-fragmented MovMuxer write side: ftyp+mdat+moov
- Round 18 — fragmented MP4 / fMP4 / DASH-init decode path
- Round 17 — lsel + ipro typed surfaces, cm=2 grid via primary_image_layout_with_input
- Round 16 — recursive cm=2 iloc resolver + index_size>0 + base iref
- Round 15 — Identity output_extent over TransformChain, HDR clli/mdcv/cclv on layout, amve + tmap
- Round 14 — HEIF auxC alpha-plane resolver + clli/mdcv/cclv HDR metadata
- Round 13 — iden TransformChain on Identity layout, pixi/colr surfaced, MIAF brand classification
- Round 12 — mdat-resident HEIF derivation payloads + per-tile/layer ispe validation
- Round 11 — HEIF colr typed extraction + image composition layout plan
- Round 10 — Windows file:// shape, dinf/dref item resolver, HEIF iden/iovl/grid renderers
- Round 9 — gate file:// integration tests on cfg(unix)
- Round 9 — HEIF grid/iovl payloads, primary-item helper, file:// opener
- Round 8 — HEIF iprp/ipco/ipma, meta-only files, iref resolver helpers
- Round 7 — ISO BMFF §8.11 meta box, multi-hop aliases, text-style trailers
- Round 6 — alias-chain following, tmcd-in-stsd, encd encoding override
- Round 5 — chapter resolution, gmhd extensions, mvhd v1 coverage
- Round 4 — udta user-data, dinf/dref data references, tkhd rotation
- Round 3 — chan layout map, tref accessors, rmra/mvex refusal, cslg

### Added

- Round 114 — Segment Index Box (`sidx`) parser, ISO/IEC 14496-12
  §8.16.3.
  - `parse_sidx(payload) -> Result<Sidx>` in the new `sidx` module.
    Layout per §8.16.3.2: FullBox header (`version` 0 or 1) +
    `reference_ID` + `timescale` + a version-width
    `(earliest_presentation_time, first_offset)` pair (32-bit for v0,
    64-bit for v1) + `reserved` + `reference_count`, then a 12-byte
    reference triple per subsegment packing `reference_type` (1 bit) /
    `referenced_size` (31 bit), `subsegment_duration` (32 bit), and
    `starts_with_SAP` (1 bit) / `SAP_type` (3 bit) / `SAP_delta_time`
    (28 bit). Unknown version (> 1) is rejected; a body whose length
    does not equal `reference_count × 12` (partial trailing reference
    or count overrun) is rejected.
  - `Sidx` / `SidxReference` / `ReferenceType` structs surfaced from
    `lib.rs`. The `references` list is preserved in file order;
    `earliest_presentation_time` / `first_offset` are widened to `u64`
    so the v0 and v1 widths share one type.
  - `MovDemuxer::sidx: Vec<Sidx>` field. The file-level walker
    recognises `sidx` as a top-level box (next to `ftyp` / `moov` /
    `mdat` / `moof` / `mfra`) regardless of placement; the box has
    `Quantity: Zero or more` (§8.16.3.1) so every one is collected in
    file order to support per-stream and hierarchical
    (`sidx`-of-`sidx`) indexes.
  - `Sidx::material_start(anchor)`, `Sidx::subsegment_offset(anchor,
    index)`, and `Sidx::subsegment_start_time(index)` accessors. The
    anchor is the first byte after the box (§8.16.3.1); subsegment
    byte offsets accumulate `referenced_size` from
    `material_start` (references are file-contiguous, §8.16.3.1) and
    subsegment presentation times accumulate `subsegment_duration`
    from `earliest_presentation_time` (durations are contiguous in
    presentation time, §8.16.3.1). Each guards against overflow and
    out-of-range index.
  - 12 unit tests in `sidx::tests` (v0 two-reference parse, v1 wide
    fields, `reference_type` index-bit decode, max-width bitfield
    round-trip, empty reference list, unknown-version reject,
    truncated-fixed-header reject, count-overrun reject,
    partial-trailing-reference reject, plus the three accessor
    walkers) and 2 demuxer-level tests (`top_level_sidx_collected_in_file_order`,
    `files_without_sidx_have_empty_vec`).
  - `SIDX` FourCC constant added to `atom`.

- Round 105 — Progressive Download Information Box (`pdin`) parser,
  ISO/IEC 14496-12 §8.1.3.
  - `parse_pdin(payload) -> Result<Pdin>` in the new `pdin` module.
    Layout per §8.1.3.2: FullBox header (`version = 0`, `flags = 0`)
    + `(rate:4, initial_delay:4) × N` pairs to end-of-box. No on-disk
    count field — the entry count is `body_len / 8`. Unknown version
    is rejected; a body length not a multiple of 8 (partial trailing
    entry) is rejected.
  - `Pdin` / `PdinEntry` structs surfaced from `lib.rs`. The
    `entries` list is preserved in writer order; §8.1.3.3 does not
    require any particular ordering by `rate`.
  - `MovDemuxer::pdin: Option<Pdin>` field. The file-level walker
    recognises `pdin` as a top-level box (next to `ftyp` / `moov` /
    `mdat`) regardless of placement; spec §8.1.3.1 recommends "as
    early as possible" but does not mandate it. A second `pdin` in
    the same file is ignored — the first one wins, preserving the
    spec's "early = more useful" guarantee.
  - `Pdin::initial_delay_for(download_rate) -> Option<u32>`
    implements §8.1.3.1's "linear interpolation between pairs, or …
    extrapolation from the first or last entry" rule. It brackets on
    a rate-sorted scratch view (so out-of-order writer pairs still
    interpolate correctly), interpolates linearly on the
    `(rate, delay)` line for an observed rate inside the bracket,
    and clamps to the first / last entry's delay when the observed
    rate falls outside the table — preserving the spec's "*upper*
    estimate" guarantee (lowest rate ↔ longest delay).
  - 12 unit tests in `pdin::tests` (round-trip with two entries,
    empty table, unknown version reject, truncated header reject,
    partial trailing entry reject, exact-match lookup, inside-bracket
    interpolation, below- and above-range clamping, lookup against
    empty table, unordered writer input still brackets correctly,
    parse→struct round-trip) + 7 integration tests in
    `tests/synth_round105_pdin.rs` (pre-`moov` placement, post-`moov`
    placement, no-`pdin` is `None`, file-level interpolation at
    observed rate, truncated payload rejection at open time, partial
    trailing entry rejection at open time, duplicate `pdin` keeps
    first).
  - `PDIN` FourCC constant added to `atom.rs`. QTFF does not define
    this box; it is ISO BMFF-only and never appears in `.mov` inputs.

- Round 102 — Shadow Sync Sample Box (`stsh`) parser, ISO/IEC
  14496-12 §8.6.3.
  - `parse_stsh(payload) -> Result<Vec<StshEntry>>` in the
    `sample_table` module. Layout per §8.6.3.2: FullBox header +
    `entry_count` + `entry_count × {shadowed_sample_number:4,
    sync_sample_number:4}`, both 1-based (the box shares `stss`'s
    sample-numbering convention). Entries that are not strictly
    increasing by `shadowed_sample_number` are rejected — §8.6.3.1
    requires the table sorted ascending, and duplicate shadowed
    numbers would make the lookup ambiguous.
  - `StshEntry` struct (`shadowed_sample_number`,
    `sync_sample_number`); new `stsh: Vec<StshEntry>` field on
    `SampleTable`.
  - `SampleTable::shadow_sync_for(shadowed_sample_number) ->
    Option<u32>` binary-searches the sorted table for an exact
    shadowed-sample match and returns the alternative sync sample's
    1-based number; `MovDemuxer::shadow_sync_sample(track,
    shadowed_sample_number) -> Option<u32>` mirrors it. The shadow
    sync sample *replaces* the shadowed one per §8.6.3.1 — after
    substitution the next sample sent is `shadowed_sample_number + 1`.
    This is optional seeking metadata; a track plays and seeks
    correctly when it is ignored.
  - 8 unit tests in `sample_table::tests` (round-trip, empty table,
    truncated table, short header, non-increasing / duplicate
    rejection, binary-search lookup, empty-table lookup) + 3
    integration tests in `tests/synth_round102_stsh.rs` (demuxer
    `shadow_sync_sample` lookup, empty-`stsh` no-op, non-monotonic
    rejection at open time).

- Round 98 — Independent and Disposable Samples Box (`sdtp`) parser,
  ISO/IEC 14496-12 §8.6.4.
  - `parse_sdtp(payload, sample_count) -> Result<Vec<SdtpEntry>>` in
    the `sample_table` module. The box carries no on-disk count field
    (§8.6.4.1 sizes it from the `stsz`/`stz2` sample count), so the
    demuxer defers the parse until after the `stbl` walk; a body
    shorter than the sample count is rejected.
  - `SdtpEntry` struct with the four 2-bit fields unpacked MSB-first
    into typed enums: `IsLeading`, `SampleDependsOn`,
    `SampleIsDependedOn`, `SampleHasRedundancy` (each covering all
    four §8.6.4.3 code-points, reserved included). Convenience
    predicates `SdtpEntry::is_independent()` (I-picture) and
    `SdtpEntry::is_disposable()` (skippable while rolling forward).
  - `MovDemuxer::sample_dependency(track, sample) -> Option<SdtpEntry>`
    and `SampleTable::sample_dependency(sample)` accessors; new
    `sdtp` field on `SampleTable`.

- Round 95 — Track Selection box (`tsel`) parser, ISO/IEC 14496-12
  §8.10.3 (pp. 72–74).
  - New `oxideav_mov::track_selection` module: `parse_tsel(payload)
    -> Result<TrackSelection>`, the `TrackSelection` struct
    (`switch_group: i32` per the spec's `template int(32)`, plus
    `attributes: Vec<[u8; 4]>` read to the end of the box), and the
    `find_tsel_in_udta(udta_payload) -> Result<Option<TrackSelection>>`
    helper that locates a `tsel` child inside a track-level `udta` body.
  - `TsAttributeRole` enum (`Descriptive` / `Differentiating` /
    `Unknown`) + the `ts_attribute_role(fourcc)` classifier function
    covering every §8.10.3.5 enumerated FourCC: six descriptive
    (`TSEL_ATTR_TEMPORAL_SCALABILITY` `tesc`,
    `TSEL_ATTR_FINE_GRAIN_SNR_SCALABILITY` `fgsc`,
    `TSEL_ATTR_COARSE_GRAIN_SNR_SCALABILITY` `cgsc`,
    `TSEL_ATTR_SPATIAL_SCALABILITY` `spsc`,
    `TSEL_ATTR_REGION_OF_INTEREST_SCALABILITY` `resc`,
    `TSEL_ATTR_VIEW_SCALABILITY` `vwsc`) + eight differentiating
    (`TSEL_ATTR_CODEC` `cdec`, `TSEL_ATTR_SCREEN_SIZE` `scsz`,
    `TSEL_ATTR_MAX_PACKET_SIZE` `mpsz`, `TSEL_ATTR_MEDIA_TYPE` `mtyp`,
    `TSEL_ATTR_MEDIA_LANGUAGE` `mela`, `TSEL_ATTR_BITRATE` `bitr`,
    `TSEL_ATTR_FRAME_RATE` `frar`, `TSEL_ATTR_NUMBER_OF_VIEWS` `nvws`).
  - Typed accessors on `TrackSelection`: `is_informative`,
    `has_attribute(&fourcc)`, `typed_attributes()` iterator returning
    `(fourcc, role)` pairs. Unknown attribute FourCCs are preserved
    verbatim so vendor / future-spec entries survive.
  - `Track::track_selection: Option<TrackSelection>` field populated
    during `parse_trak` (read from the track-level `udta` body in the
    same pass that builds `Track::user_data`);
    `Track::track_selection() -> Option<&TrackSelection>` accessor;
    mirror on `MovDemuxer::track_selection(track_index)
    -> Option<&TrackSelection>`.
  - `MovDemuxer::switch_groups() -> Vec<(i32, Vec<usize>)>` aggregates
    tracks by their `tsel.switch_group` for player ranking; tracks
    without a `tsel` AND tracks with `switch_group == 0` are excluded
    per §8.10.3.4 ("if this field is 0 … there is no information on
    whether the track can be used for switching"). Pairs with the
    existing `MovDemuxer::alternate_groups()` to expose the full
    spec hierarchy.
  - Parse-time guards: `Error::invalid` on payload < 8 bytes, on a
    non-zero `version` field (spec fixes `version = 0`), and on an
    attribute-list tail length that isn't a multiple of 4 (each
    attribute is exactly an `unsigned int(32)`). FullBox flags are
    accepted and ignored.
  - 15 unit tests in `track_selection::tests` + 8 integration tests in
    `tests/synth_round95_track_selection.rs` covering switch_group
    sign handling, descriptive/differentiating/unknown attribute
    classification, vendor-extension FourCC preservation, the
    `udta` walker (including zero-terminator handling and inner
    parse-error propagation), structural-error rejection, the
    "tsel present but uninformative" zero-state, the demuxer
    `switch_groups()` bucket map, and out-of-range track-index
    handling.
  - ISO BMFF-only box — QTFF does not define `tsel`.

- Round 91 — non-unity `media_rate` scaling in the edit-list mapper.
  `media_pts_to_movie_pts` (and the `MovDemuxer::movie_pts_for` /
  `Track::media_pts_to_movie_pts` wrappers on top of it) now honours
  any strictly-positive 16.16 `media_rate`. A `media_rate = 2.0`
  segment consumes twice as much media per movie tick — matching the
  QTFF p. 226–227 worked example. Forward map:
  `Δmovie = Δmedia × movie_ts × 65536 / (media_ts × rate_fp)`. Negative
  or zero `media_rate` on a Media segment is rejected per QTFF p. 48.
  Five unit tests in `edit::tests` and four integration tests in
  `tests/synth_round91_media_rate_scaling.rs` cover 2.0×, 0.5×, the
  full 3-segment QTFF example, the `media_rate ≤ 0` rejection path,
  and 2.0× composed after an initial empty edit.
- Round 89 — Track Load Settings atom (`load`) parser, QTFF p. 48
  Figure 2-12.
  - New `oxideav_mov::track_load` module: `parse_load(payload)
    -> Result<Load>`, the `Load` struct (`preload_start_time`,
    `preload_duration`, `preload_flags`, `default_hints`), bit-flag
    constants (`LOAD_PRELOAD_ALWAYS`, `LOAD_PRELOAD_IF_ENABLED`,
    `LOAD_HINT_DOUBLE_BUFFER`, `LOAD_HINT_HIGH_QUALITY`), and the
    `LOAD_PRELOAD_DURATION_TO_END` sentinel
    (`0xFFFF_FFFF` = preload to end of track).
  - Typed accessors on `Load`: `is_preload_to_end`,
    `preload_always`, `preload_if_enabled`, `hint_double_buffer`,
    `hint_high_quality`. Raw `default_hints` u32 preserved so
    vendor-private bits survive.
  - `Track::load: Option<Load>` field populated during `parse_trak`;
    `Track::load_settings() -> Option<&Load>` accessor; mirror on
    `MovDemuxer::track_load(track_index) -> Option<&Load>`.
  - 7 unit tests in `track_load::tests` + 5 integration tests in
    `tests/synth_round89_track_load.rs` covering canonical-field
    round-trip, `preload_duration == -1` "to end" sentinel, the two
    preload-flag bits, combined-hint bits with vendor extension,
    truncated/trailing-byte payloads, and out-of-range track-index
    handling.
  - QuickTime-only atom (no ISO BMFF counterpart per ISO/IEC
    14496-12).

- Round 80 — sample-group (`sbgp` / `sgpd`) parse + typed lookups
  (ISO/IEC 14496-12 §8.9 + §10).
  - New `oxideav_mov::sample_groups` module: `parse_sbgp`,
    `parse_sgpd`, `SampleToGroup`, `SampleGroupDescription`,
    `SampleGroupDescriptionEntry`. Handles all three on-disk
    versions of `sgpd` (deprecated v0 implicit-size with a
    per-typed-entry size catalogue; v1 `default_length` or
    per-row `description_length`; v2
    `default_sample_description_index`).
  - Typed entry decoders: `RollRecovery` for `'roll'` (§10.1.1.2,
    Visual / Audio RollRecoveryEntry), `AudioPreRoll` for
    `'prol'` (AAC + Opus codec-priming), `VisualRandomAccess`
    for `'rap '` (§10.4.2, open-GOP random-access points with
    `num_leading_samples_known` + `num_leading_samples`).
  - `SampleTable::sample_group` /
    `SampleTable::group_description_index_for_sample` resolve a
    sample's group_description_index (1-based) with v2
    `default_sample_description_index` fall-back when the
    `sbgp` returns 0 for a sample.
  - `MovDemuxer::roll_distance_for(track, sample) -> Option<i16>`,
    `MovDemuxer::audio_preroll_for(track, sample) -> Option<i16>`,
    `MovDemuxer::visual_random_access_for(track, sample) ->
    Option<VisualRandomAccess>`, and
    `MovDemuxer::random_access_points(track) -> Vec<u32>`
    (the latter unions `stss` with the `'rap '` grouping so
    open-GOP RAPs join the seek index).
  - 7 new integration tests in
    `tests/synth_round80_sample_groups.rs`: AAC pre-roll (-2048),
    Opus pre-roll (-3840), audio roll per-run runs, missing
    `sgpd` returns `None` instead of erroring, open-GOP RAP
    union with `stss`, no-`stss` "every sample is sync"
    coverage, and v2 `sgpd` `default_sample_description_index`
    fall-back.

- Round 74 — edit-list (`edts/elst`) **presentation-time honour**. The
  `elst` parser landed in round 2 but the parsed list was inert; the
  movie-time PTS of each sample was just the media-time PTS. Round 74
  threads the list through a typed segment resolver and exposes a
  mapping API so downstream callers (player / pipeline / muxer) can
  produce spec-correct presentation timestamps without re-walking the
  atom tree themselves.
  - New `oxideav_mov::EditSegment` + `EditSegmentKind { Empty, Dwell,
    Media }` types. An `EditSegment` carries
    `[movie_time_start, movie_time_end)` in movie-timescale ticks plus
    the kind classifying what the segment maps to (empty slot, dwell
    on a single media-time tick, or normal media playback with
    `media_time_start` + `media_rate`).
  - New `oxideav_mov::resolve_edit_segments(edits, movie_duration)`
    that walks an `EditList` and stamps absolute movie-time bounds on
    each entry. Handles four QTFF / ISO BMFF idioms:
    1. **Empty edits** (`media_time < 0`) → `EditSegmentKind::Empty`
       (QTFF p. 47 / ISO/IEC 14496-12 §8.6.6.3).
    2. **Dwell** (`media_rate == 0`, non-empty `media_time`) →
       `EditSegmentKind::Dwell` per §8.6.6.3.
    3. **Composition-shift** (zero `track_duration`, non-zero
       `media_time`) → zero-length `EditSegmentKind::Media` segment
       (§8.6.6.1 paragraph 2 — "in an empty initial movie of a
       fragmented movie file").
    4. **Implicit trailing empty edit** when `sum(track_duration) <
       mvhd.duration` → auto-appended `Empty` segment covering the
       gap (QTFF p. 47 last paragraph / §8.6.6.3).
  - New `oxideav_mov::media_pts_to_movie_pts(segments, media_pts,
    movie_timescale, media_timescale)` mapper. Walks the resolved
    segments in order, finds the one whose
    `[media_time_start, media_time_start + segment_media_duration)`
    contains `media_pts`, and rescales the in-segment offset from
    media-timescale ticks to movie-timescale ticks via the cross-rate
    `movie_timescale / media_timescale`. Returns `None` when the
    sample falls outside every non-empty segment (i.e. is dropped from
    the presentation timeline by the edit list).
  - New `Track::edit_segments(movie_timescale, movie_duration)` —
    typed accessor that returns a synthetic full-track Media segment
    when the track carries no `edts/elst` (matching the spec rule
    "in the absence of an edit list, the presentation of a track
    starts immediately"), so callers can drive a single code path
    regardless.
  - New `Track::media_pts_to_movie_pts(media_pts, movie_timescale,
    movie_duration)` — convenience wrapper around the free function.
  - New `MovDemuxer::movie_pts_for(track_index, media_pts)` and
    `MovDemuxer::edit_segments_for(track_index)` — demuxer-level
    convenience that picks up `mvhd.time_scale` + `mvhd.duration`
    automatically.
  - `Edit::is_dwell()` and `Edit::rate_f64()` helpers — `is_dwell`
    classifies `media_rate == 0` per §8.6.6.3; `rate_f64` decodes
    the 16.16 fixed-point rate into a plain `f64`.

- Round 74 — `tkhd.flags` and `tkhd.alternate_group` typed surface.
  The flags + alternate-group fields were parsed by round 1 but only
  reachable as raw integers on `Track::tkhd`. Round 74 adds named
  accessors so callers don't have to remember the QTFF p. 32 bit
  layout.
  - `Track::is_enabled()` — `tkhd.flags` bit 0 (the spec's `enabled`
    flag). Disabled tracks should not contribute to the default
    presentation per QTFF p. 31 / ISO/IEC 14496-12 §8.3.1.3.
  - `Track::participates_in_movie()` — bit 1 (`in_movie`).
  - `Track::participates_in_preview()` — bit 2 (`in_preview`).
  - `Track::participates_in_poster()` — bit 3 (`in_poster`).
  - `Track::alternate_group()` — surfaces `tkhd.alternate_group` (i16).
    Tracks with the same non-zero group id are mutually exclusive
    presentation candidates (typical case: multi-language audio
    tracks).
  - `MovDemuxer::presentation_tracks()` — iterator returning only
    tracks whose `tkhd.flags` carries both `enabled` and `in_movie`.
  - `MovDemuxer::alternate_groups()` — groups every track by its
    `alternate_group` field, returning a sorted
    `Vec<(group_id, Vec<track_index>)>` so a player can pick exactly
    one track per non-zero group at playback time.

- Round 74 — 10 new unit tests in `edit::tests` (`EditSegment` cumulative
  bounds, implicit trailing empty edit, dwell classification, mapper
  rescaling at differing timescales, drop-outside-edits, composition-
  shift, dwell-only-at-held-time, 16.16 rate decode) plus 9 new
  integration tests in `tests/synth_round74_edit_list_honour.rs`
  (initial-empty-edit shift, no-elst identity, implicit trailing
  empty, no-elst synthetic full-track segment, out-of-range track,
  full `tkhd.flags` surface, disabled-track exclusion from
  `presentation_tracks`, `alternate_groups` grouping, dwell mapper).
  Test count rises from 246 → 265.

- Round 22 — HEIF / HEIC image-item WRITE path. New
  `oxideav_mov::HeifWriter` / `HeifItem` / `HeifProperty` /
  `HeifDerivation` / `HeifItemReference` surface emits a
  structurally-valid `.heic` / `.heif` / `.avif` byte-stream from a
  caller-supplied list of coded-image items (HEVC / AV1 / JPEG / …)
  + derived-image items (`grid` / `iovl` / `iden` / `tmap`).
  - Property emission: writes one box per typed variant inside
    `ipco` (`ispe`, `pixi`, `colr` nclx / rICC / prof, `auxC`,
    `lsel`, `irot`, `imir`, `clli`, `mdcv`, `cclv`, `amve`, plus
    `Other { fourcc, payload }` for codec-config blobs like
    `hvcC` / `av1C`). Structurally-equal properties are de-duplicated
    so multiple items can share the same `ipco` entry, with per-item
    `ipma` rows pointing at the shared 1-based indices.
  - Derived items: emits the algorithm body into `idat` and uses
    `iloc` construction-method 1 to reference it; auto-generates
    the `dimg` `iref` row from `component_ids`. Coded items use
    `iloc` construction-method 0 with absolute file offsets into
    a trailing `mdat`.
  - Layout: two-pass build (sizing pass + emit pass) so `iloc`
    extents carry real absolute file offsets; `ftyp` picks the
    HEIC default brand set (`heic` major + `mif1`/`heic` compat)
    with caller overrides via `with_major_brand` /
    `with_compatible_brands`.
  - Coverage: in-module unit tests + `tests/synth_round22_heif_writer.rs`
    integration tests covering a 3-image HEIC (master + thumbnail +
    `grid` of two tiles), full property catalogue sweep, deterministic
    output, and external-validator acceptance (`ffprobe -v warning`
    confirms container structural validity).
  - Spec citations: ISO/IEC 14496-12 §8.11 (meta / pitm / iinf /
    iloc / iref / iprp), ISO/IEC 23008-12 §6.5 (property catalogue) +
    §6.6 (derived images), ISO/IEC 23000-22 (MIAF major brands).

- Fragmented-MP4 seek polish: regression tests covering three
  ISO/IEC 14496-12 §8.8 edge cases the round-21 ffmpeg fixtures
  don't reach. All three were already correctly implemented in
  rounds 18 + 21; this commit adds load-bearing coverage so future
  refactors can't silently break them.
  - `tests/synth_round_next_fragmented_seek_polish.rs` — 8 new
    handcrafted in-memory fixtures (no `tests/fixtures/` files).
    - **Multi-`trex` per fragment** (§8.8.3): two-track fixture
      (video tid=1 dur=100 sz=200 + audio tid=2 dur=1024 sz=64) with
      one `traf` per track in a single `moof`, both `tfhd`s carrying
      only `default-base-is-moof`. Asserts each track's samples
      consume the matching `trex` (not the first one). Bug-induction
      check confirms the test fails when the lookup is replaced with
      `trex_defaults.first()`.
    - **Negative `composition_time_offset`** (§8.8.8.2): single-
      fragment fixture with `trun` v=1 carrying CTS offsets
      `[100, -100, 50, 0, 200]` that produce the canonical B-frame
      reorder pattern (PTS[1] < PTS[0]). Asserts both `SampleEntry::
      composition_offset` and `SampleEntry::pts()` thread through
      correctly, plus that `next_packet().pts` matches. Bug-induction
      check (sign-bit strip) confirms test fails.
    - **Non-zero baseline `tfdt`** (§8.8.12): two-fragment fixture
      with `tfdt`-v1 declared on each fragment plus a tail
      `mfra/tfra/mfro` random-access index. Includes one variant
      (`tfdt_with_gap_does_not_climb_from_running_cursor`) where
      fragment 2's tfdt deliberately differs from the running DTS
      cursor (300000 cursor vs 600000 declared) so a tfdt-ignoring
      bug surfaces — without this guard the running cursor and the
      declared baseline coincide and the test would pass even with
      a buggy implementation. Seek tests confirm `seek_to(7s)` lands
      inside fragment 2 rather than at the end of fragment 1, and
      `seek_to(tfdt-baseline)` lands exactly on the fragment 2 first
      sample.

- Round 21 — fragmented-MP4 random-access seek via the ISO/IEC
  14496-12 §8.8.10 `tfra` index. `MovDemuxer::seek_to` now handles
  fragmented streams end-to-end instead of refusing them with
  `Error::Unsupported`. Pairs with round-20's stbl-based seek so the
  same `Demuxer::seek_to` surface works across both layouts.
  - `parse_tfra` (§8.8.10.3) — decodes the per-track random-access
    table. Handles both v0 (32-bit time/moof_offset) and v1 (64-bit)
    plus the three variable-width nibble fields
    (`length_size_of_traf_num`, `_trun_num`, `_sample_num` each
    ∈ {1,2,3,4} bytes).
  - `parse_mfro` (§8.8.11.2) — decodes the trailing
    `size_of_mfra` pointer.
  - `parse_mfra` (§8.8.9) — walks the `mfra` container, returning
    `(Vec<Tfra>, Option<Mfro>)`.
  - `parse_tfdt` (§8.8.12.2) — Track Fragment Decode Time, v0/v1.
    Threaded into `resolve_traf_samples` as the per-fragment DTS
    baseline so multi-moof streams (the common ffmpeg shape with
    a zero `tfdt` on the first moof and climbing values after)
    surface correctly through `next_packet`.
  - `MovDemuxer::tfra_indexes: Vec<Tfra>` — per-track random-access
    table populated at open time from a tail `mfra` box. Empty when
    the file is not fragmented or omits `mfra`.
  - `TrafRecord::tfdt: Option<u64>` — parsed `tfdt`
    baseMediaDecodeTime, when present.
  - `fragment::Tfra { track_id, entries }` +
    `TfraEntry { time, moof_offset, traf_number, trun_number,
    sample_number }`. Re-exported from the crate root.
  - `MovDemuxer::seek_to` (fragmented path): binary-searches the
    target track's `tfra` entries (§8.8.10.3 guarantees increasing
    `time`), picks the largest entry whose
    `time <= target_pts`, locates the matching sync sample in the
    flat queue by PTS-equality, and snaps `self.next`. Returns the
    landed DTS (matching the non-fragmented branch's contract).
    Fallback path when no `tfra` is present: linear scan of the
    round-18 flattened `fragment_samples` queue picking the latest
    sync sample at-or-before `pts`. Past-start lands on the first
    sync sample; past-end lands on the last `tfra` entry.
  - Open-time `tfra` back-patch: §8.8.10.3 makes `tfra` authoritative
    for random-access points but ffmpeg's fragmented writer
    sometimes omits `trun.first_sample_flags` on alternate moofs,
    leaving those samples carrying the per-fragment "non-sync"
    default. The walker now lifts the `keyframe` bit on every
    sample whose PTS matches a `tfra` entry so seek can still snap
    there.
  - Tests:
    - 6 new `fragment::tests` unit tests (`tfra` v0 / v1 / variable
      widths / truncated; `mfro` round-trip + truncated; `tfdt`
      v0 / v1 / truncated).
    - `tests/round_next_fragmented_seek.rs` — 8 integration tests
      against new ffmpeg-generated fixtures
      (`h264_frag_with_mfra.mp4` carries 6 `tfra` entries for a
      3 s × 10 fps × GOP=10 H.264 stream;
      `h264_frag_nomfra.mp4` exercises the fallback path). Covers
      tfra populated at open / absent leaves the field empty /
      seek lands exactly on a tfra entry / seek to zero re-snaps
      to the first moof / mid-gap snap-back to prior entry /
      fallback queue-scan / non-fragmented regression / past-end
      clamp.
    - `tests/seek.rs::seek_in_fragmented_returns_unsupported` flipped
      into `seek_in_fragmented_lands_at_keyframe`, asserting the
      round-21 acceptance behaviour.

- `MovDemuxer` now implements `oxideav_core::Demuxer::seek_to` for
  non-fragmented files. Walks the existing flattened sample queue
  filtered on the requested stream, picks the largest sync sample
  with `dts <= pts`, sets the per-demuxer `next` cursor so the
  subsequent `next_packet()` call emits that sample, and returns
  its DTS. Past-end requests clamp to the last sync sample; targets
  before the first keyframe land on the first sync sample. Algorithm
  per QTFF "Finding a Sample" (pp. 79–80); mirrors the in-tree
  `oxideav-mp4` reference at `crates/oxideav-mp4/src/demux.rs:2418`.
  Round 21 adds the fragmented-MP4 seek path on top (see above).

- Round 20 — fragmented MP4 / fMP4 / DASH muxer (ISO/IEC 14496-12
  §8.8 write side). Pairs with the round-18 `moof/traf/trun` decode
  path so the crate now round-trips fragmented streams in both
  directions.
  - `FragmentationMode { ByDuration(u64), ByFrameCount(u32) }` —
    opt-in fragmentation policy. Slices the primary (first-added)
    track's flat sample list into per-fragment runs along either
    accumulated media-timescale ticks (`ByDuration`) or accumulated
    sample count (`ByFrameCount`). Secondary tracks (audio paired to
    a video primary) snap to the same time boundary, rescaled into
    each track's own media timescale.
  - `MovMuxer::with_fragmentation(mode)` — opt-in. The non-fragmented
    `write_to` / `encode_to_vec` path is unchanged and ignores the
    setting.
  - `MovMuxer::write_to_fragmented::<W: Write>(&self, w)` and
    `MovMuxer::encode_fragmented_to_vec()` — emit the fragmented
    file. Layout produced:
    - `ftyp` — major `iso5`, compat `iso5` / `isom` / `mp42` /
      `dash` / `msdh` (ISO BMFF §8.8.7.1 note +
      ISO/IEC 23009-1 §6.3.4.2 DASH compatibility).
    - Init `moov` — `mvhd` (duration=0), per-track `trak` with
      empty `stbl` (`stts`/`stsc`/`stsz`/`stco` all
      `entry_count=0`), one `mvex/trex` per track. The `trex`'s
      `default_sample_flags` is `0x0001_0000` (non-sync) for video
      tracks and `0` (sync) for audio.
    - Media segments — one `moof` + `mdat` pair per fragment.
      `moof` carries `mfhd` (sequence number climbing from 1) plus
      one `traf` per track. Each `traf` is `tfhd` (with
      `default-base-is-moof` flag and no per-fragment defaults) +
      `trun` (with `data_offset` + `sample_duration` +
      `sample_size` + `sample_flags` flags set; per-sample rows
      carry the explicit duration / size / keyframe flag).
  - Two-pass moof sizing — the muxer first measures the moof byte
    length with placeholder data offsets, then re-emits with the
    real `trun.data_offset` values pointing at the first byte of
    each track's run inside the trailing `mdat` payload.
  - `MovMuxer::fragmentation_mode()` — read-back accessor for the
    configured policy.
  - Tests:
    - `synth_round20.rs`:
      - `fragmented_by_frame_count_emits_three_fragments_for_5_samples_n2`
        — 5-frame video, `ByFrameCount(2)` → 3 fragments
        (sequence numbers `[1, 2, 3]`), per-sample bytes survive
        verbatim through the demuxer.
      - `fragmented_by_duration_slices_along_primary_timebase`
        — 6 frames × 1000 ticks each, `ByDuration(2000)` →
        3 fragments of 2 samples.
      - `fragmented_keyframe_flag_round_trips_via_trun_sample_flags`
        — single-fragment 5-sample run; sample 0's `keyframe = true`
        and samples 1–4's `keyframe = false` survive the
        `trun.sample_flags` round-trip.
      - `fragmented_dts_climbs_monotonically_across_fragment_boundaries`
        — verifies the per-track DTS cursor advances through
        fragment boundaries (round-18 demuxer threads `dts_cursor`
        across moofs).
      - `fragmented_audio_only_track_works` — 8-sample audio-only
        fragmented MP4.
      - `fragmented_init_segment_has_ftyp_then_moov_layout`
        — byte-level check the wire order is `ftyp` → `moov` → first
        `moof` and that `iso5` + `dash` brand FourCCs appear.
      - `fragmented_requires_fragmentation_mode` /
        `fragmented_by_{frame_count,duration}_zero_rejected` /
        `fragmented_empty_track_list_rejected` /
        `fragmented_track_with_zero_samples_rejected` — error-path
        coverage.
      - `ffprobe_accepts_fragmented_output` — opt-in `ffprobe -v
        error -of json -show_format -show_streams` cross-check;
        no-op (with a stderr note) when ffprobe isn't on PATH.

- Round 19 — write-side `MovMuxer` for non-fragmented MOV/MP4. Builds
  a structurally-valid `ftyp` + `mdat` + `moov` file from per-track
  sample lists; the emitted bytes are accepted by `ffprobe -of json`
  and round-trip back through [`MovDemuxer`] with sample count,
  per-sample sizes, byte payloads, and keyframe flags preserved
  verbatim.
  - New `muxer` module exposing `MovMuxer`, `MuxSample`, and
    `MuxTrackKind { Video, Audio }`. The muxer emits, per ISO/IEC
    14496-12 sections cited in the module docstring:
    - `ftyp` (§4.3) — major `qt  `, minor 0x200, compat
      `qt  ` / `isom` / `mp42`.
    - `mdat` (§8.1.1) — auto-promotes to the 16-byte extended-size
      header when the body exceeds `u32::MAX`.
    - `moov/mvhd` (§8.2.2) — v0, identity matrix, rate=1.0,
      `next_track_id = tracks.len() + 1`.
    - `moov/trak/tkhd` (§8.3.2) — v0, flags
      `enabled|in_movie|in_preview = 0x07`, identity matrix,
      audio-track volume = 1.0 / video-track volume = 0.
    - `moov/trak/mdia/mdhd` (§8.4.2) — v0, language code `und`
      (0x55C4).
    - `moov/trak/mdia/hdlr` (§8.4.3) — `mhlr` / `vide`|`soun`,
      empty counted-Pascal name.
    - `moov/trak/mdia/minf/{vmhd|smhd}` (§12.1.2 / §12.2.2) — `vmhd`
      with no-lean-ahead flag for video, balance=0 `smhd` for audio.
    - `moov/trak/mdia/minf/dinf/dref/url` (§8.7.2) — single self-
      reference entry, `flags=1` ("data is in this file").
    - `moov/trak/mdia/minf/stbl/stsd` (§8.5.2 / QTFF p. 70) — single
      entry per track, `data_reference_index=1`. Video body carries
      hres/vres = 72.0 fixed-point, frame_count=1, depth=24,
      color_table_id=-1 plus the declared width/height; audio body
      carries v0 channels/bits/sample_rate. Callers may inject one
      or more codec-config extension atoms (e.g. `avcC`) via the
      `extra_stsd_atoms` slot which the muxer copies verbatim into
      the trailing portion of the entry.
    - `moov/trak/mdia/minf/stbl/stts` (§8.6.1.2) — run-length-
      encoded against per-sample `MuxSample::duration`.
    - `moov/trak/mdia/minf/stbl/stss` (§8.6.2) — emitted only when
      at least one sample is *not* a keyframe (preserves the
      QTFF p. 73 implicit "every-sample-keyframe" rule for audio).
    - `moov/trak/mdia/minf/stbl/stsc` (§8.7.4) — single chunk per
      track, `samples_per_chunk = track.sample_count`,
      `sample_description_id = 1`.
    - `moov/trak/mdia/minf/stbl/stsz` (§8.7.3) — uniform
      `sample_size` when all samples are the same length, otherwise
      `sample_size = 0` followed by the per-sample size table.
    - `moov/trak/mdia/minf/stbl/stco|co64` (§8.7.5) — `stco` when
      all chunk offsets fit in `u32`, `co64` otherwise (the muxer
      auto-promotes when the cumulative sample bytes push any
      track's chunk past 4 GiB).
  - Layout produced is `ftyp + mdat + moov` (mdat-before-moov);
    [`MovDemuxer::open`] already accepts both orderings, so the
    round-trip closes without a faststart pass. A symmetric
    faststart helper (`moov`-before-`mdat`, two-pass write) is on
    the round-20 menu.
  - `MovMuxer::write_to::<W: Write>` — emit to any `std::io::Write`.
  - `MovMuxer::encode_to_vec` — emit to a `Vec<u8>` for in-memory
    consumers (the testing path).
  - `MovMuxer::with_movie_timescale(ts)` — override the default
    movie timescale (600).
  - Tests:
    - `synth_round19.rs`:
      - `roundtrip_5_frame_video_mov_preserves_sample_count_and_bytes` —
        builds a 5-frame `mp4v` MOV (1 keyframe + 4 non-keyframes),
        demuxes back through `MovDemuxer`, verifies sample count,
        per-sample sizes, durations, keyframe flags, byte-level
        payloads, and the post-EOF `Error::Eof` contract.
      - `roundtrip_audio_only_mov_preserves_sample_table` — 3
        uniform-size `sowt` samples, asserts `stsz_default_size =
        Some(256)` and that `stss` is omitted (every-sample-keyframe
        implicit rule).
      - `roundtrip_two_track_video_plus_audio_preserves_both_streams`
        — 4 video + 2 audio samples in one file, verifies both
        track-id assignment (1 / 2) and per-track sample-table
        round-trip.
      - `empty_track_list_rejected` / `track_with_zero_samples_rejected`
        — error-path coverage.
      - `ffprobe_accepts_synth_video_only_mov` — invokes
        `ffprobe -v error -of json -show_format -show_streams` on
        the synth bytes, asserts exit success and one
        `"codec_type": "video"` stream.
      - `ffprobe_accepts_synth_video_plus_audio_mov` — same pattern
        with one video + one audio stream. Both ffprobe tests
        no-op (with a stderr note) when ffprobe isn't on `$PATH`.
    - 9 new unit tests in `muxer::tests` covering `ftyp` byte
      layout, empty-muxer / zero-sample rejection, stts run-length
      encoding, stss omit-when-all-keyframes, stss emission with
      keyframe-index resolution, and stsz uniform-vs-table
      dispatch.

- Round 18 — fragmented MP4 / fMP4 / DASH-init decode path landed. The
  demuxer used to refuse `moof` / `mvex` outright; this round
  implements the full ISO/IEC 14496-12 §8.8 cascade so a fragmented
  `qt  ` or `mp4` walks all samples cleanly through
  `MovDemuxer::next_packet`.
  - New `fragment` module with end-to-end parsers for the §8.8 box
    family: `mfhd` (§8.8.5), `mehd` (§8.8.2), `trex` (§8.8.3),
    `tfhd` (§8.8.7), `trun` (§8.8.8), `traf` (§8.8.6) and `moof`
    (§8.8.4) — plus the `tf_flags` / `tr_flags` bit constants
    (`TFHD_BASE_DATA_OFFSET_PRESENT`, `TFHD_DEFAULT_BASE_IS_MOOF`,
    `TFHD_DURATION_IS_EMPTY`, `TRUN_DATA_OFFSET_PRESENT`,
    `TRUN_FIRST_SAMPLE_FLAGS_PRESENT`, `TRUN_SAMPLE_*_PRESENT`)
    re-exported from the crate root.
  - `MovDemuxer::is_fragmented()` — true iff the file declares `mvex`
    or contains at least one `moof`.
  - `MovDemuxer::trex_defaults: Vec<TrexDefaults>` — per-track
    fragment defaults parsed from `moov/mvex/trex`.
  - `MovDemuxer::mehd: Option<Mehd>` — optional total fragmented
    presentation duration (§8.8.2).
  - `MovDemuxer::fragment_sequence_numbers: Vec<u32>` — the `mfhd`
    sequence number of every `moof` walked at open time, in wire
    order, so callers can spot dropped fragments.
  - `Track::fragment_samples: Vec<SampleEntry>` — samples appended
    by `moof/traf/trun` runs. Each entry's absolute file offset, DTS,
    duration, keyframe flag, sample-description-id, and composition
    offset are resolved through the `trun → tfhd → trex` defaults
    cascade. Shape-breaking field addition; the only literal-
    construction consumer is this crate's own tests (none touched).
  - `MovDemuxer::resolve_traf_samples` (exported as
    `oxideav_mov::resolve_traf_samples`) — pure helper that turns
    a `TrafRecord` plus the per-track `trex` defaults into a vector
    of `SampleEntry` with the correct `default-base-is-moof` /
    explicit `base_data_offset` / "end of previous traf" anchor
    semantics from §8.8.7.1.
  - Tests:
    - `synth_round18.rs`: 4 in-memory fixtures — single-moof
      walk, two-moof DTS monotonicity, mfhd sequence-number
      preservation, and a non-fragmented "false" classification
      check.
    - `ffmpeg_fragments_oracle.rs`: 2 opt-in tests that generate
      real `ffmpeg`-emitted fragmented MP4 (single-moof + multi-moof)
      and verify the demuxer's emitted packet count matches
      `ffprobe`'s `nb_read_packets`. Skipped (with a stderr note)
      when `ffmpeg`/`ffprobe` aren't on `$PATH`.
    - `synth_reference_and_fragments.rs`: the two long-standing
      rejection tests (`mvex_inside_moov_is_unsupported`,
      `top_level_moof_is_unsupported`) flipped to *acceptance*
      tests (`mvex_inside_moov_surfaces_trex_defaults`,
      `top_level_moof_with_mfhd_only_accepted`) — the round-3
      rejection was the user-visible blocker this round retires.

- Round 17 — long-pending typed-extraction gaps closed and r16's
  recursive `iloc` resolver wired into the layout planner so
  `construction_method == 2` (item_offset) primary items resolve
  transparently through `MovDemuxer::primary_image_layout_with_input`.
  - `iprp::LayerSelector { layer_id: u16 }` +
    `iprp::ItemProperty::Lsel(LayerSelector)` +
    `ItemProperties::lsel(item_id)` + `parse_lsel_payload` — typed
    extraction for the HEIF / ISO/IEC 23008-12 §6.5.11 LayerSelector
    property (was previously caught by the `Other` fall-through). The
    parser accepts both the bare 2-byte and FullBox-prefixed 6-byte
    on-disk shapes.
  - `ImageLayout::Identity { …, lsel: Option<LayerSelector> }` —
    extended `Identity` layout. The selector is populated from the
    inner item's `iprp` association so multi-layer-aware callers
    (SHVC / MV-HEVC) don't have to re-walk `iprp`. Shape-breaking
    field addition; the only consumer in this crate is the demuxer's
    own resolver.
  - `bmff_meta::ItemProtection { schemes: Vec<ProtectionScheme> }` +
    `ProtectionScheme { scheme_type, scheme_version, scheme_uri,
    original_format, raw_payload }` + `BmffMeta::item_protection() ->
    Option<&ItemProtection>` — typed surface for the previously
    parser-skipped `ipro` Item Protection Box (ISO/IEC 14496-12
    §8.11.5). One `ProtectionScheme` per `sinf` child preserves
    `frma.data_format`, `schm.scheme_type` / `scheme_version` /
    optional `scheme_uri`, and the verbatim `sinf` body in
    `raw_payload` for downstream DRM-aware callers.
    `ItemProtection::scheme_for_item_index` resolves the 1-based
    `infe.item_protection_index` field (with `0` meaning unprotected
    per the spec).
  - `MovDemuxer::primary_image_layout_with_input` now resolves
    `construction_method == 2` (item_offset) `grid` / `iovl` primary
    items end-to-end via r16's `resolve_item_bytes`. Previously the
    cm=2 path returned `None`; the cm=2 indirection is now walked
    transparently and the planner lands a `Grid` / `Overlay` plan as
    expected. Doc comment updated to reflect the new contract.
  - `BmffMeta { …, item_protection: Option<ItemProtection> }` —
    shape-breaking field addition. The only literal-construction
    consumers are this crate's own `derived` test fixtures (all
    updated).
  - 11 new tests (5 lsel — bare / FullBox / wrong-size / typed-variant
    presence / Identity layout surface, 4 ipro — single cenc /
    two-scheme + URI flag / absent / empty count, 1 cm=2 grid
    end-to-end through `primary_image_layout_with_input`, +1 unit
    test verifying `lsel` accessor returns None when unassociated).

- Round 16 — long-deferred `iloc` resolver gaps: recursive
  `construction_method == 2` (item_offset) walker with cycle
  detection, per-extent `extent_index` surfacing on `index_size > 0`,
  and HEIF `base` `iref` typed reference for pre-derived coded image
  surfaces.
  - `MovDemuxer::resolve_item_bytes(item_id) -> Result<Vec<u8>>` —
    recursive resolver that walks all three iloc construction methods
    (0 file extents / 1 idat / 2 item_offset) transparently. The
    cm=2 path sub-slices the source item's bytes per extent and uses
    the `iref iloc` reference list to pick the source item (or
    `extent_index` when `index_size > 0`). Cycle detection: a
    `HashSet<u32>` of visited item ids is threaded through the
    recursion; re-entry on a previously visited id aborts the resolve
    with `Error::invalid("MOV: iloc cycle through item N")`.
  - `ItemExtent::index: Option<u64>` (was `index: u64` with `0 ==
    absent`) — the per-extent `extent_index` field per ISO/IEC
    14496-12 §8.11.3 is now `Some(idx)` when the parent `iloc`
    carries `index_size > 0` and `None` otherwise. Shape-breaking
    field re-type; the only in-tree consumers are the cm=2 source-
    item picker and the `derived` synth-test fixtures (all updated).
  - `BmffMeta::base_image_for(item_id) -> Option<u32>` +
    `MovDemuxer::base_image_for(item_id)` — returns the base coded
    image id for a derived item per HEIF §6.4.7 (`base` iref). Used
    by HEIF authoring flows that pre-render an HDR variant alongside
    an SDR base.
  - `BmffMeta::typed_references() -> Vec<ItemReferenceType>` +
    `ItemReferenceType::{Base, Other}` — typed projection over
    `ItemReference` rows. Promotes `base` to its own variant (with
    `from_id` / `to_ids`) and surfaces every other reference kind
    through `Other { kind, from_id, to_ids }` with the FourCC
    preserved verbatim.
  - 8 new tests (3 cm=2 recursive resolver + cycle detection /
    self-cycle, 2 index_size>0 + cm=2 extent-index source picking,
    3 base iref + typed_references projection).

- Round 15 — HEIF transformative-property dimensional math on the
  `Identity` layout, HDR mastering metadata (`clli` / `mdcv` / `cclv`)
  surfaced on the layout itself, and HEIF tone-mapping property
  extraction (`amve`) plus a new `ImageLayout::ToneMap` variant for
  `tmap`-typed derivations.
  - `derived::ImageLayout::output_extent(&BmffMeta) -> Option<(u32, u32)>`
    + `derived::compute_post_transform_extent(base_w, base_h,
    &TransformChain)` — compose a `TransformChain` over a base
    `(w, h)` per HEIF §6.5.9 / §6.5.10 / §6.5.12 (clap shrinks to
    `(width_n / width_d, height_n / height_d)`; irot 90°/270° swaps
    axes; imir preserves dims). `Grid` / `Overlay` return
    `(canvas_w, canvas_h)`; `ToneMap` defers to the base item's
    extent.
  - `ImageLayout::Identity { …, clli: Option<Clli>, mdcv: Option<Mdcv>,
    cclv: Option<Cclv>, amve: Option<Amve> }` — extended Identity
    variant. The four HDR helper structs are populated from the
    inner item's `iprp` row alongside r13's `pixi` / `colr` so callers
    don't have to re-walk `iprp` themselves. Shape-breaking field
    addition; the only consumer in this crate is the demuxer's own
    resolver.
  - `iprp::Amve { ambient_illuminance, ambient_light_x,
    ambient_light_y }` — typed Ambient Viewing Environment property
    (HEIF Amd.1 / SMPTE ST 2108-1). `ambient_illuminance` is in
    0.0001 lux units; chromaticity values are in CIE-1931 ×50000 same
    as `mdcv`.
  - `iprp::ItemProperty::Amve(Amve)` + `ItemProperties::amve(item_id)`
    + `parse_amve_payload` — typed dispatch and accessor mirroring
    the r14 `clli` / `mdcv` / `cclv` surface. The parser accepts both
    the bare 8-byte and FullBox-prefixed 12-byte on-disk shapes.
  - `derived::TmapPayload { bytes: Vec<u8> }` +
    `derived::parse_tmap_payload` + `ImageLayout::ToneMap { item_id,
    base, params }` — `tmap` derived-image surface. The `tmap` item's
    single `dimg` target identifies the HDR base image being
    tone-mapped; the algorithm payload bytes are surfaced verbatim
    (the HEIF Amd.1 algorithm catalogue is broad and caller-driven —
    callers that target one specific algorithm can re-parse them
    against their own decoder).
  - `MovDemuxer::primary_image_layout_with_input` now also dispatches
    `tmap` primaries (mdat-resident algorithm payloads supported via
    the same construction-method resolver as grid/iovl).
  - 22 new tests (4 amve unit-parser + 8 transform-extent helpers +
    2 grid/identity output-extent + 2 HDR-on-Identity surface + 2
    tmap layout dispatch + 2 round-trip + 2 sanity).

- Round 14 — HEIF auxiliary-plane resolver surfacing `alpha_for` on
  the `Identity` layout, plus typed extraction of HDR mastering
  metadata (`clli` / `mdcv` / `cclv`) from `iprp`.
  - `iprp::AuxC::is_alpha()` + `is_depth()` — typed dispatch over the
    auxC URN string. Recognises both the HEIF `urn:mpeg:hevc:2015:auxid:1`
    (alpha) / `:auxid:2` (depth) URNs and the codec-agnostic MIAF
    `urn:mpeg:mpegB:cicp:systems:auxiliary:alpha` / `:depth` URNs.
  - `iprp::Clli { max_content_light_level, max_pic_average_light_level }`,
    `iprp::Mdcv { display_primaries: [(u16,u16);3], white_point,
    max_display_luminance, min_display_luminance }`,
    `iprp::Cclv { cancel_flag, persistence_flag, primaries,
    min/max/avg_luminance: Option<u32> }` — typed HDR property structs
    surfaced on `ItemProperty::Clli` / `Mdcv` / `Cclv` (no longer
    `ItemProperty::Other` fall-throughs).
  - `iprp::parse_auxc_payload`, `parse_clli_payload`, `parse_mdcv_payload`,
    `parse_cclv_payload` — public payload parsers callers can drive
    directly when they have a raw property body. `parse_clli_payload` /
    `parse_mdcv_payload` accept both the bare and FullBox-prefixed
    on-disk forms.
  - `ItemProperties::auxc(item_id)`, `clli(item_id)`, `mdcv(item_id)`,
    `cclv(item_id)` — typed accessors that walk the item's `ipma` row
    and return the first match.
  - `ImageLayout::Identity { ..., alpha_for: Option<u32> }` — the alpha
    auxiliary plane's master-image item id, resolved from the auxC
    URN + `auxl` iref (HEIF §7.5.1, MIAF Annex B). `None` when the
    item isn't an alpha plane or when no `auxl` iref is present.
    Shape-breaking field addition on the Identity variant.
  - 20 new tests (4 auxC alpha-plane resolver scenarios + 4 clli +
    4 mdcv + 5 cclv + 3 standalone parse_auxc_payload).

- Round 13 — HEIF iden transformative-property cascade composed onto
  the `Identity` layout, HEIF `pixi` channel-bit-depth surfaced on the
  layout plan, and MIAF / brand classification on `MovDemuxer`.
  - `derived::TransformOp { Clap(Clap), Irot { steps }, Imir { axis } }`
    + `derived::TransformChain = Vec<TransformOp>` — ordered chain of
    HEIF transformative properties (HEIF §6.5 / §6.6.2.1) emitted in
    spec order (`clap` → `irot` → `imir`).
  - `ImageLayout::Identity { item_id, transform: TransformChain,
    pixi: Option<PixiInfo>, color_profile: Option<ColrInfo> }` —
    extended Identity variant. `transform` composes the iden
    derivation's transformative properties (when the primary item is
    an `iden`) with the inner item's own — same-kind in both means
    the iden's wins (the derivation overrides the inner content's
    intrinsic transform). `pixi` and `color_profile` carry the inner
    item's `iprp/ipma`-bound values so callers don't have to re-walk
    `iprp` themselves. Shape-breaking field addition; the only
    consumer in this crate is the demuxer's own resolver.
  - `iprp::PixiInfo { channels: Vec<u8> }` + `PixiInfo::num_channels()`
    + `From<&Pixi> for PixiInfo` — HEIF-canonical Pixel Information
    accessor reshape.
  - `iprp::ItemProperties::pixi(item_id) -> Option<PixiInfo>` and the
    underlying `pixi_for` borrow accessor.
  - `header::BrandClass` — strongly-typed enum classifying every brand
    in the HEIF / MIAF / AVIF / ISO BMFF / MPEG-4 / QTFF registries
    (29 named variants + an `Other([u8; 4])` fall-through). Methods:
    `BrandClass::classify(&[u8; 4])`, `BrandClass::fourcc()`,
    `is_heic_family()`, `is_avif_family()`, `is_miaf_family()` (the
    last folds `mif1`/`mif2`/`MA1A`/`MA1B` plus the HEIC- and AVIF-
    family brands per HEIF §10 / AVIF §3).
  - `Ftyp::brand_class()` walks `major_brand` then `compatible_brands`
    in declaration order, classifying each.
  - `Ftyp::is_heic()`, `Ftyp::is_avif()`, `Ftyp::is_miaf()` —
    convenience predicates around `brand_class()`.
  - `MovDemuxer::brand_class() / is_heic() / is_avif() / is_miaf()` —
    same accessors lifted onto the demuxer (returning empty / false
    when the file has no `ftyp`).
  - 23 new tests (4 unit `pixi` accessors + 5 `BrandClass` /
    `Ftyp::is_*` accessors + 6 `derived` iden cascade tests + 7
    `synth_round13` integration tests + 1 round-11/12 test signature
    update). Total now 271 (was 248).

- Round 12 — HEIF derivation payloads resolved from `mdat`
  (`construction_method == 0`) and per-tile / per-layer `ispe`
  validation surfaced on the layout plan.
  - `MovDemuxer::primary_image_layout_with_input(&mut self) ->
    Option<ImageLayout>` — extends the round-11 pure-meta resolver to
    also handle `grid` / `iovl` payloads stored at file offsets
    (typical home: `mdat`). The pure-meta `primary_image_layout()`
    stays idat-only; the new variant takes `&mut self` so it can seek
    and read the file extents the `iloc` declares.
  - `derived::build_grid_layout(meta, item_id, payload_bytes)` and
    `build_overlay_layout(meta, item_id, payload_bytes)` — pure
    helpers that take pre-resolved derivation bytes (the path the
    mdat resolver uses internally). The `plan_*_layout` shortcuts
    keep working for the idat-only case.
  - `derived::IspeMismatch { item_id, expected_w, expected_h,
    actual_w, actual_h }` — surfaced in
    `ImageGridLayout::tile_size_warnings` for tiles whose `ispe`
    disagrees with the canonical first-tile extent (HEIF §6.6.2.3.3
    forbids the mismatch; we don't fail the plan, we let validators
    detect it). Also surfaced in `OverlayLayout::layer_size_warnings`
    for `iovl` layers that lack an `ispe` association.
  - `GridTilePlacement` gains `w`, `h` fields carrying the per-tile-
    declared `ispe` extent (== canonical for spec-compliant files;
    deviant per-tile `ispe` is preserved in the per-slot `(w, h)` and
    flagged via `tile_size_warnings`).
  - `OverlayLayer` gains `w`, `h` fields carrying the layer item's
    `ispe`; `(0, 0)` when the layer has no `ispe` association (also
    surfaced as a warning).
  - 8 new tests (5 unit + 3 round-12 integration). Total now 248
    (was 240).
  - Public surface added: `IspeMismatch`, `build_grid_layout`,
    `build_overlay_layout`, `MovDemuxer::primary_image_layout_with_input`.
    Per-round-11 the `Round 11` types are still `[Unreleased]`, so
    the field additions to `GridTilePlacement` / `OverlayLayer` /
    `ImageGridLayout` / `OverlayLayout` are not breaking releases.

- Round 11 — HEIF colour-profile typed extraction (`colr` →
  `ColrInfo`) and HEIF composition-plan helpers
  (`primary_image_layout()` → `ImageLayout::{Identity, Grid,
  Overlay}`).
  - `iprp::parse_colr_payload(payload) -> ColrInfo` — typed
    decoder for the ColourInformationBox per ISO/IEC 14496-12
    §12.1.5. Returns the HEIF-canonical `ColrInfo` enum:
    - `Nclx { primaries, transfer, matrix, full_range }` — per-CICP
      indices (ISO/IEC 23001-8) plus the `full_range_flag` bit.
    - `RestrictedIcc(Vec<u8>)` — `rICC` body bytes preserved
      verbatim.
    - `UnrestrictedIcc(Vec<u8>)` — `prof` body bytes preserved
      verbatim.
    The Apple QTFF `nclc` shape is rejected with `InvalidData` per
    HEIF §6.5.5.1 Note 1; QTFF tracks should keep using the
    existing `media_meta::parse_colr` surface.
  - `ItemProperties::color_profile(item_id) -> Option<ColrInfo>` —
    accessor that walks `ipma` for the bound item and reshapes the
    resolved `colr` into the HEIF-canonical enum (`None` for the
    Apple `nclc` variant or unrecognised forensic fall-throughs).
  - `derived::ImageGridLayout { canvas_w, canvas_h, tile_w,
    tile_h, rows, cols, tiles: Vec<GridTilePlacement> }` — `grid`
    composition plan; tile placements `(item_id, x, y)` come from
    walking `dimg` iref + first-tile `ispe` for the shared encoded
    extent.
  - `derived::OverlayLayout { canvas_w, canvas_h, canvas_fill_color,
    layers: Vec<OverlayLayer { item_id, x: i32, y: i32 }> }` —
    `iovl` composition plan; per-layer `(x, y)` come from the
    parsed `Overlay::offsets` in `dimg` target order.
  - `derived::ImageLayout::{Identity { item_id }, Grid(_),
    Overlay(_)}` — unified composition variant returned by the
    layout helpers. `iden` is treated as a pass-through to its
    inner `dimg` target so callers that decode through the regular
    codec path get the encoded image directly; bare coded items
    (`hvc1`, `av01`, `j2k1`, …) surface as `Identity { item_id =
    primary_item_id }`.
  - `derived::primary_image_layout_for(meta)` and
    `image_layout_for(meta, id)` planner helpers; the former
    dispatches off the file's `pitm`. Construction is
    `idat`-resident-only for `grid` / `iovl` payloads (the typical
    authoring shape).
  - `MovDemuxer::primary_image_layout() -> Option<ImageLayout>` —
    one-shot accessor that resolves the file's primary HEIF image
    into a composition plan from the top-level `meta` box. Returns
    `None` when the input has no `meta` (or no `pitm`, or the
    derivation can't be planned from `idat`).
  - 29 new tests (20 unit + 9 round-11 integration). Total now
    240 (was 211).
  - Public types added: `ColrInfo`, `ImageGridLayout`,
    `GridTilePlacement`, `OverlayLayout`, `OverlayLayer`,
    `ImageLayout`. New helpers: `parse_colr_payload`,
    `ItemProperties::color_profile`, `plan_grid_layout`,
    `plan_overlay_layout`, `primary_image_layout_for`,
    `image_layout_for`, `MovDemuxer::primary_image_layout`.

- Round 10 — Windows `file://` shape rules, meta-scope `dinf/dref`
  external file-reference resolution, HEIF `iden` / `iovl` / `grid`
  pixel renderers, and HEIF-strict `ipma` essential-bit enforcement.
  - `open_file_url` now decodes Windows `file:///C:/path` and the
    legacy `file:///C|/path` shapes (RFC 8089 Appendix E.2) into
    `C:\path` on Windows targets, with case-insensitive drive
    letters and forward-slash → backslash flipping. The Unix shape
    behaviour is unchanged. The conversion rule lives in a pure
    helper (`normalise_path_for_windows`) so the round-9 Unix CI
    keeps the Windows rule under continuous coverage even though the
    live opener path is `cfg(windows)`-gated.
  - `BmffMeta::data_references: Vec<DataReference>` parsed from a
    meta-scope `dinf/dref` (ISO/IEC 14496-12 §8.7) — populated from
    `url ` / `urn ` / `alis` / `rsrc` entries and the `flags & 1 ==
    1` self-ref shape. `BmffMeta::data_location(idx) ->
    DataLocation` and `BmffMeta::data_location_for_item(item_id)`
    resolve an `iloc` row's `data_reference_index` to one of
    `SameFile` / `External(&DataReference)` / `Unresolved`, surfacing
    HEIF/MIAF tile-bag-in-sidecar shapes to callers without forcing
    them to walk the atom tree by hand.
  - New `render` module with pure-Rust pixel renderers operating on
    a tightly-packed RGBA8 surface (`Rgba8Canvas`):
    - `render_iden(source, properties)` applies a HEIF `iden`
      derivation per §6.6.2.1 + §6.3, walking the resolved property
      list in spec order (`clap` crop → `irot` 90°-step CCW
      rotation → `imir` mirror).
    - `render_iovl(overlay, layers)` composes a layered canvas per
      §6.6.2.2.3 with straight-alpha Porter-Duff "source over
      destination" blending; honours negative offsets by clipping
      (per the spec's "Pixel locations with a negative offset value
      are not included" wording).
    - `render_grid(grid, tiles)` tiles row-major into the canvas
      per §6.6.2.3, trimming overshoot on the right / bottom.
    - `ispe_dimensions` convenience extracts the first `Ispe`
      dimensions from a property list.
  - `ItemProperties::resolve_strict(item_id, recognised)` —
    HEIF-strict resolver for the `ipma` essential-bit (§7.4.6.6):
    returns `Err(fourcc)` on the first essential-bit-set
    association whose target property is an `Other` not in the
    caller's recognised allow-list. Permits opt-in strict
    rejection for HEIF readers that need it without breaking the
    permissive `resolve` default.
  - 39 new tests (29 unit + 10 round-10 integration). Total now
    211 (was 172).
  - Public types added: `Rgba8Canvas`, `DataLocation`. New helpers:
    `render_iden`, `render_iovl`, `render_grid`, `ispe_dimensions`,
    `BmffMeta::data_location`, `BmffMeta::data_location_for_item`,
    `ItemProperties::resolve_strict`.

- Round 9 — HEIF derived-image payloads (`grid` / `iovl`),
  `pitm`-aware primary-item-bytes convenience helper, and a built-in
  `file://` URL opener for reference-movie alias chains.
  - New `derived` module with `parse_grid` / `parse_overlay` /
    `parse_overlay_with_source_count`. ISO/IEC 23008-12 §6.6.2.3.1
    (grid: rows/cols/output dimensions, both 16- and 32-bit shapes
    via the flags bit) and §6.6.2.3.2 (overlay: 4×u16 RGBA canvas
    fill, signed `(h_offset, v_offset)` offsets per layer) are both
    decoded. `parse_overlay` infers the layer count from the body's
    residual length, while `parse_overlay_with_source_count`
    validates against the caller-provided `dimg` target count.
    Public types: `Grid`, `Overlay`.
  - `bmff_meta::primary_item_data(meta) -> Option<ItemDataLocation>`
    walks `pitm` → `iloc` and returns the primary item's bytes (when
    `idat`-resident, concatenated across multi-extent items) or its
    file-extents (when `construction_method == 0`). Construction
    method 2 (`item_offset`) is surfaced via
    `ItemDataLocation::Other` so callers can dispatch their own
    indirection. Generic `item_data(meta, item_id)` covers the same
    surface for any item.
  - `bmff_meta::idat_bytes_concat` — convenience helper that joins
    the multi-extent `idat` slices [`idat_bytes_for_item`] returns
    into a single `Vec<u8>`, matching the common single-byte-string
    consumer (HEIF derived-image payloads, small inline metadata).
  - `demuxer::open_file_url` — built-in `file://` URL opener for
    [`MovDemuxer::open_with_aliases`]. Handles
    `file:///absolute/path`, `file://localhost/path`, and the legacy
    `file:rel-or-abs` shapes; rejects non-`file:` schemes and
    foreign-host authorities with `std::io::ErrorKind::Unsupported`
    so the alias chain falls through. Percent-decodes path segments
    so URL-encoded spaces (`%20`) resolve to real filesystem paths
    on macOS / Linux.
  - 24 new tests (10 `derived` unit + 13 `synth_round9` integration
    + 1 `_smoke` helper). Total now 172 (was 148).
  - Public types added: `Grid`, `Overlay`, `ItemDataLocation`. New
    helpers: `parse_grid`, `parse_overlay`,
    `parse_overlay_with_source_count`, `primary_item_data`,
    `item_data`, `idat_bytes_concat`, `open_file_url`.

- Round 8 — HEIF/HEIC item-properties container (`iprp`/`ipco`/`ipma`),
  meta-only files (no `moov` tracks), and `iref` typed-reference
  resolver helpers (`derived_from`, `auxiliary_for`, `thumbnail_of`,
  `describes`, plus inverse-direction `thumbnails_of_master` and
  `metadata_describing` lookups).
  - New `iprp` module with `ItemProperties { properties, associations }`.
    `parse_iprp` walks `ipco` (a flat array of property boxes) and
    every sibling `ipma` (FullBox v0/v1, both 8-bit and 16-bit
    association indices via the flags `&1` discriminator).
  - Strongly-typed property variants: `Colr`, `Pasp`, `Clap`, `Pixi`,
    `Ispe`, `Irot`, `Imir`, `AuxC`, plus `Other { fourcc, payload }`
    fall-through for any property box we don't model natively
    (`hvcC`, `av1C`, `lsel`, `clli`, `mdcv`, `cclv`, …). The fall-
    through path lets callers parse codec-config records via the
    appropriate codec crate without us pulling them as deps.
  - `ItemProperties::resolve(item_id) -> Vec<&ItemProperty>` resolves
    `ipma` 1-based property indices into `ipco` entries; out-of-range
    indices are silently skipped (forward-compatible).
  - Convenience helpers `ispe_for`, `colr_for`, `auxc_for`,
    `orientation_for(item_id) -> (Option<Irot>, Option<Imir>)`.
  - `BmffMeta::properties: Option<ItemProperties>` surfaced alongside
    the round-7 fields.
  - `BmffMeta` typed-reference helpers: `derived_from(id)` (`dimg`),
    `auxiliary_for(id)` (`auxl`), `thumbnail_of(id)` (`thmb`),
    `describes(id)` (`cdsc`); inverse `thumbnails_of_master(id)`,
    `metadata_describing(id)`; plus generic `refs_from(id, kind)` /
    `refs_to(id, kind)`.
  - `MovDemuxer::open` now succeeds on **meta-only HEIF/HEIC/AVIF
    still-image files** that ship without any `moov`. The previous
    `"MOV: no moov/mvhd found"` and `"MOV: moov contains no tracks"`
    errors are now relaxed when a top-level (`file_bmff_meta`) or
    movie-scope (`bmff_meta`) `meta` box is present. `mvhd` and
    `tracks` are surfaced as `None` / empty respectively, and
    `next_packet` returns `Eof` immediately so callers consume the
    item directory instead of the sample queue.
  - 13 new tests (8 `iprp` unit tests + 5 round-8 integration tests:
    moov-scope iprp resolution, meta-only HEIF open, `iref` resolver
    helpers, empty-meta open, ipma v1 16-bit indices). 148 tests
    total (was 135).
  - Public types added: `ItemProperties`, `ItemProperty`,
    `ItemPropertyAssociation`, `PropertyAssociation`, `Ispe`, `Pixi`,
    `Irot`, `Imir`, `AuxC`, `parse_iprp`.

- Round 7 — ISO BMFF §8.11 `meta` box parsing (HEIF/HEIC/MIAF/AVIF
  surface), multi-hop `rmra/url ` alias-chain following with cycle
  detection, and the QTFF text-sample style trailers
  (`styl`/`ftab`/`hlit`/`hclr`/`drpo`).
  - New `bmff_meta` module with `BmffMeta { handler_type, primary_item,
    items, locations, idat, xml, bxml, references }` plus
    `ItemExtent`, `ItemLocation`, `ItemInfoEntry`, `ItemReference`
    types. `pitm` v0/v1, `iloc` v0/v1/v2 (offset/length/base_offset/
    extent_index sized 0/4/8 each), `iinf` with `infe` v0/v1/v2/v3,
    `idat`, `xml `, `bxml`, and `iref` (v0 u16 ids, v1 u32 ids, all
    typed children) all decode.
  - `MovDemuxer` exposes `bmff_meta: Option<BmffMeta>` (movie-scope)
    and `file_bmff_meta: Option<BmffMeta>` (top-level scope, common
    for HEIF still-image files); `Track` exposes `bmff_meta:
    Option<BmffMeta>` (track-scope). The Apple key-value `meta` shape
    still wins when both interpretations of a single atom are valid;
    we only fall back to BMFF mode when the Apple parser declines.
  - `file_extents_for_item(meta, id)` / `idat_bytes_for_item(meta, id)`
    helpers resolve a HEIF item to its file-offset extents (when
    construction_method == 0) or to its inline `idat` slice (when
    construction_method == 1).
  - `MovDemuxer::open_with_aliases` / `open_with_aliases_resolver` now
    follow up to `MAX_ALIAS_DEPTH = 4` reference-movie hops with a
    visited-URL set so cycles are rejected before the depth cap is
    reached. Self-contained inputs still pass through untouched and
    the opener is never called for them.
  - `chapter::parse_text_sample_styles(data) -> (String, TextSampleStyles)`
    walks the trailing extension atoms of an Apple text sample and
    surfaces every documented styling record: `styl` style runs
    (start/end/font/face/size/RGBA), `hlit` highlight ranges, `hclr`
    highlight colour, `drpo` drop-shadow offsets (signed i16), and
    `ftab` font-table entries. The existing
    `decode_text_sample_full` (encd-only) is preserved unchanged.
  - Public types added: `BmffMeta`, `ItemExtent`, `ItemInfoEntry`,
    `ItemLocation`, `ItemReference`, `parse_bmff_meta`,
    `file_extents_for_item`, `idat_bytes_for_item`, `MAX_ALIAS_DEPTH`,
    `parse_text_sample_styles`, `TextSampleStyles`, `StyleRecord`,
    `ColorRgba`, `HighlightRange`, `HighlightColor`, `FontTableEntry`.

- Round 6 — alias-chain following (one hop), `tmcd` sample-description
  decode inside `stsd`, and `encd` text-encoding-override surfacing on
  chapter samples.
  - `MovDemuxer::open_with_aliases(input, opener)` and
    `open_with_aliases_resolver(input, opener, resolver)` follow a
    single `rmra/url ` reference hop when the input is a reference-
    only `.mov` (no inline tracks). Self-contained inputs pass through
    untouched (the opener is never invoked). Two-hop chains and
    unreachable URLs surface as `Unsupported` with an "alias chain
    exhausted" / inner-target error verbatim.
  - `MovDemuxer::probe_reference_movies(&mut dyn ReadSeek)` static
    helper exposes the parsed `rmra/rmda` list without committing to
    a full demuxer construction; lets callers introspect aliases
    before deciding whether to follow them.
  - `timecode` module with `Tmcd { flags, time_scale, frame_duration,
    number_of_frames, source_name }` plus convenience predicates
    (`is_drop_frame`, `is_24_hour_max`, `is_negatives_ok`,
    `is_counter`) and `TMCD_FLAG_*` constants. `parse_stsd` populates
    `SampleDescription::tmcd` for tracks whose handler is `tmcd` and
    whose entry FourCC is `tmcd`. The trailing source-reference
    user-data `name` atom (or `udta`-wrapped `name`) round-trips into
    `Tmcd::source_name`. Distinct from the round-5 `tmcd > tcmi` shape
    inside `gmhd` (which carries display-style fields, not timing).
  - `chapter::decode_text_sample_full` returns `(title, encoding_id)`
    by scanning for a trailing `encd` extension atom on Apple text
    samples (`[size:4]['encd'][text_encoding_id:u32]`, also accepts a
    FullBox-prefixed shape). `ChapterEntry::text_encoding` exposes
    the raw Mac `TextEncoding` constant — round 6 surfaces it without
    a built-in encoding-id-to-encoding-name table since the mapping
    table lives in CoreFoundation `TextCommon.h`.
  - 8 new unit tests (`chapter::encd_*`, `timecode::*`) plus 7 new
    integration tests (`synth_round6.rs`) covering tmcd-in-stsd
    decode, `encd` round-trip, alias-chain happy path, self-contained
    pass-through, exhausted-alias rejection, two-hop refusal, and the
    static `probe_reference_movies` helper.
- Round 5 — chapter-track resolution, per-MediaType `gmhd` extension parsing, and v1 `mvhd` integration coverage.
  - `chapter` module with `ChapterEntry { start_time, duration, title }`,
    `ChapterList { track_index, time_scale, entries }`, and a
    `decode_text_sample` helper. The Apple text-sample shape
    (`[u16 BE size][text bytes]` plus optional `encd/styl/hlit/hclr`
    extension trailers, which we ignore) decodes to a best-effort
    UTF-8 string; invalid UTF-8 falls back to a Mac-Roman → ASCII
    expansion (bytes ≥ 0x80 → U+FFFD).
  - `MovDemuxer::chapters_for(primary_track_index)` resolves the
    `tref/chap` reference into a fully-populated `ChapterList`. Single-
    hop: rejects self-cycles and chained chapter references with
    `InvalidData`; missing chapter track-id surfaces a
    `chapter track-id N not present in moov` error.
  - `gmhd` module with parsers for the per-MediaType base-media
    information sub-atoms: `gmin` (graphics_mode + opcolor + balance),
    `text` (9-element 16.16 / 2.30 transformation matrix used by text
    overlays), and `tmcd > tcmi` (font/face/size + bg/fg colors +
    optional Pascal-counted font name). All three slots populate a
    single `Gmhd` aggregate stored on `Track::gmhd`.
  - `Hdlr::is_text` / `is_subtitle` / `is_timecode` and matching
    `Track::is_text` / `is_subtitle` / `is_timecode` accessors,
    classifying `text` (chapter / overlay), `subt`/`sbtl` (BMFF
    subtitle), and `tmcd` (time-code) handler subtypes.
  - 11 new unit tests (`chapter::*`, `gmhd::*`) plus 5 new integration
    tests (`synth_chapters_and_gmhd.rs`) covering chapter resolution
    happy-path, no-tref → `Ok(None)`, self-cycle rejection, dangling
    track-id rejection, and a v1-`mvhd` 64-bit-duration round-trip.
- Round 4 — `udta` user-data subtree, `dinf/dref` data-reference parsing, and `tkhd` matrix rotation classification.
  - `user_data` module with `parse_udta`, `UserDataEntry`, and
    `UserDataKind` (InternationalText / PlainUtf8 / Unknown).
    Apple international-text records (©nam / ©cpy / ©day / ©dir /
    …) are unwound into one entry per language record; QT-7 plain
    `name` / `auth` / `cprt` are decoded as UTF-8 with their
    embedded ISO 639-2/T language tag preserved. `iso_language_tag`
    helper unpacks the 5-bits-per-char form into a 3-byte ASCII tag.
  - `MovDemuxer::user_data` (movie scope) and `Track::user_data`
    (track scope) populated from `moov/udta` and `trak/udta`.
  - `parse_dref` for the data-reference list inside `mdia/minf/dinf/dref`.
    Recognises `url ` (UTF-8), `urn ` (ISO BMFF two-string layout),
    `alis` / `rsrc` (opaque), and the `flags & 0x01` self-reference
    flag (returns `DataReference::SelfRef`). Surfaces on
    `Track::data_references()` plus `Track::is_self_contained()`.
  - `Tkhd::matrix` raw 9-element `i32` array plus
    `Tkhd::rotation()` → `TrackRotation` (`None`/`Rotate90`/
    `Rotate180`/`Rotate270`/`Other`) classifying the four cardinal
    orientations from the matrix's `[a b c d]` corner.
  - 13 new unit tests (`user_data`, `reference::parse_dref`,
    `header::tkhd_rotation_*`) + 2 new integration tests
    (`synth_user_data_dref_rotation.rs`).
- Round 3 — channel-layout map, tref accessors, reference-movie + fragment refusal, cslg.
  - `Chan` now parses the variable-length `AudioChannelDescription`
    list (20 bytes each: label + flags + 3 × f32 coordinates) into
    `Vec<ChanDescription>`, plus a `channel_mask()` accessor that
    resolves pre-defined `kAudioChannelLayoutTag_*` constants
    (Mono / Stereo / Quad / Pentagonal / Hexagonal / Octagonal /
    MPEG 3.0/4.0/5.0/5.1/6.1/7.1 in their A/B/C/D variants) to a
    USB-style FL|FR|FC|LFE|... bitmap, with `UseChannelDescriptions`
    OR-ing per-channel labels and `UseChannelBitmap` returning the
    raw bitmap unchanged.
  - `Track::chapter_track_ref()` / `Track::timecode_track_ref()` /
    `Track::track_refs_of_kind()` accessors surface chap / tmcd /
    arbitrary `tref` relationships as resolved 1-based track-ids.
  - `reference` module with `parse_rdrf` plus `ReferenceMovie` /
    `DataReference` types; demuxer parses `moov/rmra`/`rmda` and
    surfaces them on `MovDemuxer::reference_movies`. Reference-only
    movies (rmra without an in-file mdat or trak) are rejected with
    a clear `Unsupported` error pointing at the alias chain.
  - Fragmented MP4 detection: top-level `moof` and `moov/mvex` both
    refuse open with `Unsupported("...; use oxideav-mp4 for
    fragmented streams")`.
  - `cslg` parser (v0 / v1) at both `trak` and `stbl` scope, plus
    cross-validation of the parsed range against the `ctts` table —
    a contradiction (`ctts` deltas outside `cslg [least, greatest]`)
    rejects the file.
  - Standalone `Error::Unsupported` variant (mirrors
    `oxideav_core::Error::unsupported`).
  - 10 new unit tests + 8 new integration tests
    (`synth_chan_round3.rs`, `synth_reference_and_fragments.rs`).
- Round 2 — Apple-specific atoms + edit lists + composition timing.
  - `src/lib.rs` (was missing in round 1) declaring the public module
    surface and re-exporting the parsed types.
  - `src/standalone.rs` shim providing a self-contained
    `Error`/`Result`/`ReadSeek` API for `default-features = false`
    consumers.
  - `edts/elst` parser (`Edit` + `EditList`); supports v0 (32-bit) and
    v1 (64-bit) entries plus the `media_time = -1` empty-edit
    sentinel.
  - `ctts` composition-time-to-sample parser; surfaces signed
    `composition_offset` on `SampleEntry` so `pts() = dts + offset`.
  - `tref` track-reference parser with classified `TrackRefKind`
    (chap / tmcd / scpt / ssrc / sync / hint / mpod / other).
  - `tapt` Apple Track Aperture Mode Dimensions (clef/prof/enof
    sub-atoms each carrying 16.16 fixed-point pixel dimensions).
  - Apple-shaped `meta` atom parser (hdlr + keys + ilst), surfacing a
    flat `Vec<MetaKeyValue>` on `MovDemuxer` (movie-level) and
    `Track::meta` (track-level).
  - Visual sample-description extension scanner inside `stsd`:
    detects `gama` (16.16 gamma), `pasp` (pixel aspect ratio),
    `clap` (clean aperture), `colr` (Apple `nclc` *or* ISO `nclx`
    discrimination via the leading 4-byte `colorParameterType`).
  - Audio sample-description `chan` extension scanner (Apple Core
    Audio channel-layout tag + bitmap; per-channel descriptions kept
    as raw bytes for round 3).
  - `MovDemuxer::is_faststart()` probe — true when `moov` precedes
    `mdat` at top level.
  - 18 new unit tests + 4 new integration tests (`synth_video_extensions.rs`,
    `synth_edits_and_ctts.rs`, `synth_apple_meta.rs`).
- Round 1 — initial QTFF demuxer.
  - Atom walker over QTFF `[size:4][type:4]([ext_size:8])?[payload]`
    with `size==1` (extended 64-bit) and `size==0` (to-end-of-file)
    special cases.
  - `ftyp` brand detection (recognises `qt  ` major / compatible).
  - `moov` walk covering `mvhd`, per-track `tkhd`, `mdia/mdhd`,
    `mdia/hdlr`, and `minf/stbl/{stsd,stts,stsc,stsz,stco|co64,stss}`.
  - Sample iterator yielding `(index, file_offset, size, dts,
    duration, sample_description_id, keyframe)` records.
  - `oxideav_core::Demuxer` trait impl emitting `Packet` records.
  - `oxideav_core::register!("mov", ...)` entry point and
    `register_containers(&mut ContainerRegistry)` factory.
  - Default-on `registry` cargo feature (drop `oxideav-core` with
    `default-features = false`).
  - Hand-built minimal QTFF integration test (`synth_minimal_qt.rs`):
    1-track 1-sample movie with `qt  ` brand, `rle ` sample format,
    320×240 frame.
