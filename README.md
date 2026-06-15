# oxideav-mov

Pure-Rust Apple QuickTime File Format (QTFF) demuxer + muxer.

QTFF is the immediate ancestor of ISO Base Media File Format
(ISO/IEC 14496-12); MP4 is itself an ISO BMFF derivative. The two
share an atom/box hierarchy, but QTFF retains Apple-specific semantics
ISO BMFF does not standardise. This crate is the **dedicated container
for QTFF inputs** and is a sibling — not a child — of
[`oxideav-mp4`]. It also reads and writes the ISO BMFF feature set
shared with MP4 / fMP4 / DASH / CMAF, so QuickTime-only and ISO-only
boxes both surface (with QuickTime-only fields staying empty on `.mov`
and ISO-only fields staying empty on `.mov`).

Decoding stays in codec crates: this crate calls
`oxideav_core::CodecResolver` to map sample-description FourCCs to
`CodecId`s and never opens a decoder itself.

## Demuxer

- Atom walker over `[size:4][type:4]([ext_size:8])?[payload]` with
  `size==1` (extended 64-bit) and `size==0` (to-EOF) special cases.
- `ftyp` / `styp` brand detection (`qt  `, plus the DASH / CMAF
  segment-brand predicates); `pdin`, `sidx`, `ssix`, `prft`, `pnot`,
  `ctab` at file scope; user-type (`uuid`) vendor-extension capture.
- `moov` walk: `mvhd`, per-track `tkhd` / `mdia` (`mdhd`, `hdlr`) /
  `minf/stbl`, plus the QuickTime `clip` / `crgn`, `matt` / `kmat`,
  `load`, `imap`, and Color Table atoms.
- Sample tables: `stsd`, `stts`, `stsc`, `stsz` / `stz2` (compact),
  `stco` / `co64`, `stss`, `stsh` (shadow sync), `sdtp`, `stdp`,
  `padb`, `subs` (sub-samples), `saiz` / `saio` (sample-aux), and
  sample groups (`sbgp` / `sgpd` v0/v1/v2 plus the compact `csgp`)
  with typed `'roll'` / `'prol'` / `'rap '` lookups.
- Per-sample iterator yielding `(index, file_offset, size, dts,
  duration, sample_description_id, keyframe)`, plus a random-access
  surface (`chunk_for_sample`, `sample_offset`, `chunk_byte_extent`, …)
  implementing QTFF "Finding a Sample" without iterating prior samples.
- Edit lists (`edts/elst`): `movie_pts_for` / `media_pts_for` map
  between media- and movie-timescale PTS, handling empty / dwell /
  composition-shift edits and non-unity `media_rate`. `next_packet`
  keeps the media-time PTS contract.
- Track relationships: typed `tref` accessors for every QTFF reference
  kind (`chap`, `tmcd`, `sync`, `scpt`, `hint`, `ssrc`) with track-id
  → index resolvers; `tsel` (track selection / switch groups),
  `kind`, `trgr` (track groups), and `strk` sub-tracks. `tkhd` flags
  + `alternate_group` surfaced via `presentation_tracks()` /
  `alternate_groups()` / `switch_groups()`.
- Video sample-description extensions: `gama`, `pasp`, `clap`, `colr`,
  `fiel` (field handling), and the default Motion-JPEG `mjqt` / `mjht`
  tables (surfaced verbatim).
- Fragmented MP4 / fMP4 / DASH: `mvex/trex` defaults +
  `moof/traf/tfhd/trun` cascade, `tfdt` baseline DTS, `leva` level
  assignment, and per-fragment sample-aux. `mfra/tfra/mfro`-driven
  fragmented seek (PTS-keyed binary search, linear fallback).
- Compressed movie resources (`cmov` / `dcom` / `cmvd`): zlib-inflated
  transparently on open (bounded by the declared uncompressed size),
  re-entering the same `moov` walk;
  `compressed_movie_algorithm` surfaces the `dcom` FourCC.
- Reference movies (`rmra/rmda/rmdr/rmcs`): parsed; alias resolution
  is opt-in (`open_with_aliases`) so the default `open` path can't
  reach the network / filesystem.

## Seek

`MovDemuxer::seek_to(stream, pts)` walks the per-track sample queue,
snaps to the largest sync (`stss`) sample at-or-before the target DTS,
and resets the cursor so the next `next_packet()` re-emits from there.
Fragmented files use `tfra` when present.

## Muxer

`MovMuxer` emits a non-fragmented MOV/MP4 (`ftyp` + `mdat` + `moov`)
carrying one or more video/audio tracks, round-tripping through
`MovDemuxer` with sample count, sizes, payloads, and keyframe flags
preserved. `stco` auto-promotes to `co64` when chunk offsets cross
4 GiB. Per-sample composition offsets (`MuxSample.composition_offset`,
PTS − DTS) emit a `ctts` Composition Time to Sample Box (§8.6.1.3):
omitted when every offset is zero, version 0 for an all-non-negative
track, auto-promoted to version 1 (signed `int(32)`) the moment any
offset is negative — so B-frame reorder round-trips PTS exactly.

- `with_fragmentation(ByDuration | ByFrameCount)` +
  `encode_fragmented_to_vec()` emit a fragmented MP4 / fMP4 / DASH
  segment stream (init segment + one media segment per fragment).
- `set_sample_aux(track_id, SampleAuxStream)` writes `saiz` / `saio`
  on both the non-fragmented (`stbl`-scope, absolute offset) and
  fragmented (`traf`-scope, moof-relative offset) paths — e.g. for
  Common Encryption per-sample records.
- `with_compressed_movie_resource()` (opt-in) compresses the trailing
  `moov` into a `cmov` tree; `mdat` is written first so chunk offsets
  stay absolute.

## HEIF / HEIC write path

[`HeifWriter`] emits a structurally-valid `.heic` / `.heif` / `.avif`
file from a list of [`HeifItem`]s (coded bytes + item type + per-item
property list: `ispe`, `pixi`, `colr`, `auxC`, `lsel`, `irot`,
`imir`, `clli`, `mdcv`, `cclv`, `amve`, plus `Other` for codec-config
blobs). Derived items (`grid`, `iovl`, `iden`, `tmap`) emit into
`idat` with auto-generated `dimg` `iref` rows. Property de-dup, a
two-pass layout for real `iloc` extents, and round-trip through this
crate's own `parse_bmff_meta` / `iprp` / `derived` surfaces.

## Robustness

The walker is injection-hardened for network-supplied input:

- `read_payload` refuses to allocate above
  [`MAX_INMEMORY_ATOM_BODY`] (64 MiB); `mdat` is never read this way
  (per-sample reads `seek` into it). Count-driven parsers
  (`stsd`, `keys`, `chan`, `dref`, `sgpd`, …) reject any declared
  count whose minimum on-disk footprint exceeds the remaining body
  before allocating.
- Top-level and nested atoms whose declared `size` extends past EOF,
  or whose `start + size` overflows `u64`, are rejected at the header.

Pinned by synthetic robustness test suites (forged sizes, truncation
sweeps, random-byte fuzz, extended-size overflow).

## Fuzzing

A `cargo-fuzz` harness under `fuzz/` (target `demux`) feeds arbitrary
bytes through `MovDemuxer::open`, drains up to 256 packets, touches
every file-scope and per-track accessor, exercises the edit-list
mapper at boundary PTS values, and re-runs the seek path. Alias
resolution is excluded so a fuzz input can't reach the network or
filesystem. A daily 30-minute run is scheduled.

## Standalone build

`oxideav-core` is gated behind the default-on `registry` cargo
feature. Drop the framework dependency with:

```toml
oxideav-mov = { version = "0.0", default-features = false }
```

The parser API (`atom`, `header`, `sample_table`, `track`) stays
available against a crate-local `Error`/`Result`; the `Demuxer`
trait impl and `register()` entry point disappear.

## License

MIT — see `LICENSE`.
