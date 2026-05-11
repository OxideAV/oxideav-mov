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

Decoding stays in codec crates; this crate calls
`oxideav_core::CodecResolver` to map sample-description FourCCs to
`CodecId`s and never opens a decoder itself (per
`docs/IMPLEMENTOR_ROUND.md` §"Crate-purpose discipline").

## Round 2 candidates

- Apple-specific atoms: `gama`, `clap`, `pasp`, the QT-pre-ICC
  `colr`, the `tapt` track-aperture-mode (clef/prof/enof), the
  `wave` audio sample-description extension, the `chan` audio
  channel atom.
- `tref` types (`chap`, `scpt`, `ssrc`, `tmcd`, `mpod`).
- `edts`/`elst` edit-list semantics (including `media_time = -1`
  empty edits).
- Reference movies (`rmra`/`rmda`) and alias data references.
- Apple-shaped `meta` atom layout.
- Multi-track muxer + faststart (`moov` before `mdat`).
- `ctts` (composition-time-to-sample) for B-frame-bearing video.

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
