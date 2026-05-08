# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
