# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
