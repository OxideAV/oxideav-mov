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
