# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
