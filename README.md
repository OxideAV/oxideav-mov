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

## Follow-ups

- Edit-list mapper currently handles `media_rate ∈ {0, 0x0001_0000}`
  (dwell + unity rate). Non-unity rates (typical authoring example:
  segment played at 2.0×) surface in the `EditSegmentKind::Media`
  variant but the mapper falls back to identity on them — a future
  round needs the rate-scaled offset arithmetic and a fixture proving
  non-unity rate against an ffmpeg-encoded reference.
- A `next_packet`-side opt-in (`MovDemuxer::with_edit_list_pts()`?)
  that swaps the emitted `Packet::pts` from media-time to movie-time
  end-to-end, so consumers that don't want the explicit
  `movie_pts_for` call site get spec-correct presentation timing for
  free. Round 74 keeps the existing media-time PTS contract on
  `next_packet` to avoid a silent behaviour change.

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
