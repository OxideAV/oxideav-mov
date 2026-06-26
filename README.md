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
  with typed per-sample lookups for every standardized §10 grouping
  type — `'roll'` / `'prol'` / `'rap '` (random-access), `'tele'`
  (temporal level), `'sap '` (Stream Access Point), `'rash'`
  (rate share), and `'alst'` (alternative startup sequence).
- Per-sample iterator yielding `(index, file_offset, size, dts,
  duration, sample_description_id, keyframe)`, plus a random-access
  surface (`chunk_for_sample`, `sample_offset`, `chunk_byte_extent`, …)
  implementing QTFF "Finding a Sample" without iterating prior samples.
- Edit lists (`edts/elst`): `movie_pts_for` / `media_pts_for` map
  between media- and movie-timescale PTS, handling empty / dwell /
  composition-shift edits and non-unity `media_rate`. `next_packet`
  keeps the media-time PTS contract.
- Track relationships: typed `tref` accessors for every QTFF reference
  kind (`chap`, `tmcd`, `sync`, `scpt`, `hint`, `ssrc`) plus the ISO
  BMFF §8.3.3.3 reference types `cdsc` (content-describes), `font`,
  `hind` (hint dependency), `vdep` / `vplx` (auxiliary depth / parallax
  video), and `subt` — with track-id → index resolvers; `tsel`
  (track selection / switch groups),
  `kind`, `trgr` (track groups), and `strk` sub-tracks. `tkhd` flags
  + `alternate_group` surfaced via `presentation_tracks()` /
  `alternate_groups()` / `switch_groups()`.
- Video sample-description extensions: `gama`, `pasp`, `clap`, `colr`,
  `fiel` (field handling), and the default Motion-JPEG `mjqt` / `mjht`
  tables (surfaced verbatim).
- Timecode tracks (`tmcd`): the `stsd` sample description (time scale /
  frame duration / frames-per-second / drop-frame-etc flags / structured
  source-tape `name` per QTFF p. 224) plus per-sample **Timecode Sample
  Data** decoding (QTFF p. 108) — `timecode_sample(track, idx)` reads a
  sample's `mdat` payload into either a 32-bit tape `Counter` value or a
  packed `[H:M:S:F]` `Record` (with sign), and `start_timecode(track)`
  resolves a media track's governing timecode through its `tref/tmcd`
  reference. `TimecodeRecord::to_frames` / `from_frames` give
  non-drop-frame conversions (drop-frame absolute counts are out of
  scope — the SMPTE-12M skip rule is outside the QTFF spec).
- QuickTime Text Sample Description (`text` format inside `stsd` on a
  classic `text`-handler track, QTFF pp. 108–110): the display config of
  a QuickTime text track is decoded into a typed `TextSampleDescription`
  on `SampleDescription::text` — `display_flags` (drop-shadow / anti-alias
  / scroll / key-text / use-movie-background, surfaced via accessors),
  `text_justification` (left / center / right), the 48-bit RGB
  fore/background colours (`Rgb48`), the `default_text_box` rectangle, the
  `font_number` / `font_face` style bitmask (bold / italic / underline /
  …), and the trailing Pascal font `text_name`. Distinct from the
  per-sample text payload decoded by `parse_text_sample_styles` and from
  the `gmhd/text` media-information header. Non-`text` handlers leave the
  field `None`.
- Sound sample-description versioning: version-0 (uncompressed-sample)
  and version-1 (QTFF p. 101 `SoundDescriptionV1`) layouts. The four
  fixed-ratio fields (`samples_per_packet`, `bytes_per_packet`,
  `bytes_per_frame`, `bytes_per_sample`) surface typed via
  [`SoundV1`]; `audio_version` + `audio_compression_id` are exposed,
  and `is_vbr()` decodes the variable-bit-rate "third variant"
  (version 1, Compression ID `-2`, QTFF p. 102). The Apple `chan`
  channel layout is parsed; codec-private blobs (`wave` /
  `esds` / `frma`) stay opaque for the codec crates.
- Timed-metadata sample entries (ISO/IEC 14496-12 §12.3.3): a `meta`-
  handler track's `stsd` entry (`Hdlr::is_metadata()`) is decoded into a
  typed `MetadataSampleEntry` on `SampleDescription::metadata` — `metx`
  (XML: `content_encoding` / `namespace` / `schema_location`), `mett`
  (text: `content_encoding` / `mime_format` plus the `txtC` TextConfigBox
  `text_config`), and `urim` (URI: the `uri ` URIBox string plus the
  optional `uriI` URIInitBox data). The optional `btrt` BitRateBox
  (§8.5.2.2) is decoded to `BitRate { buffer_size_db, max_bitrate,
  avg_bitrate }` and surfaced via `MetadataSampleEntry::bitrate()`.
- Timed-text simple sample entry (ISO/IEC 14496-12 §12.5.3): a `text`-
  handler track's `stsd` entry whose FourCC is `stxt` is decoded into a
  typed `SimpleTextSampleEntry` on `SampleDescription::simple_text` —
  `content_encoding` / `mime_format` plus the optional `btrt` BitRateBox
  and `txtC` TextConfigBox. Selected by the `stxt` FourCC, so it coexists
  on the same `text` handler with the QuickTime `text` description above.
- Subtitle sample entries (ISO/IEC 14496-12 §12.6.3): a `subt`-handler
  track's `stsd` entry (`Hdlr::is_subtitle()`) is decoded into a typed
  `SubtitleSampleEntry` on `SampleDescription::subtitle` — `stpp`
  (XML: `namespace` / `schema_location` / `auxiliary_mime_types`) and
  `sbtt` (text: `content_encoding` / `mime_format` + `txtC`), sharing
  the `btrt` BitRateBox decoding.
- Hint tracks (`hdlr.is_hint()`): the Hint Media Header Box (`hmhd`,
  ISO/IEC 14496-12 §12.4.2) is decoded into `Hmhd { max_pdu_size,
  avg_pdu_size, max_bitrate, avg_bitrate }` on `Track::hmhd`.
- Media-header classification (§8.4.5.1): every track records which
  media-header box its `minf` carried — `vmhd` / `smhd` / `hmhd` /
  `sthd` (Subtitle Media Header, §12.6.2) / `nmhd` (Null Media Header,
  §8.4.5.2, used by timed-metadata tracks) / `gmhd` — on
  `Track::media_header_kind` (`MediaHeaderKind`). The optional `elng`
  Extended Language Tag Box (§8.4.6) is parsed onto
  `Track::extended_language` (an RFC 4646 / BCP 47 tag such as
  `"en-US"` that overrides the packed `mdhd.language`).
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
`auto_cslg(track_id)` / `set_cslg(track_id, Cslg)` add the matching
Composition to Decode Box (`cslg`, §8.6.1.4) right after the `ctts`:
`auto_cslg` derives the five bounds (`compositionToDTSShift`, least /
greatest `composition_offset`, composition start / end time) from the
track's per-sample offsets + durations; both auto-promote to v1 when a
field leaves the signed-32-bit range. Round-trips through `parse_cslg`
and satisfies the demuxer's `cslg`/`ctts` cross-validation.

- `set_edit_list(track_id, &[MuxEdit])` emits an `edts/elst` (QTFF
  p. 47 / §8.6.6) between `tkhd` and `mdia`. `MuxEdit::segment` is a
  unity-rate presentation segment; `MuxEdit::empty` is a head empty
  edit (`media_time == -1`) for encoder-priming-skip or start-delay.
  The `elst` auto-promotes from v0 (32-bit) to v1 (64-bit) the moment a
  `track_duration` exceeds 4 GiB-ticks or a `media_time` leaves the
  signed-32-bit range; entries round-trip through the read-side
  `parse_elst`. No `edts` is written when a track has no edit list.
- `with_fragmentation(ByDuration | ByFrameCount)` +
  `encode_fragmented_to_vec()` emit a fragmented MP4 / fMP4 / DASH
  segment stream (init segment + one media segment per fragment).
- `set_sample_aux(track_id, SampleAuxStream)` writes `saiz` / `saio`
  on both the non-fragmented (`stbl`-scope, absolute offset) and
  fragmented (`traf`-scope, moof-relative offset) paths — e.g. for
  Common Encryption per-sample records.
- `add_sample_to_group(track_id, SampleToGroupWrite)` writes a `csgp`
  (CompactSampleToGroupBox, ISO/IEC 14496-12:2020 §8.9.5) per
  `grouping_type` at `stbl` scope. The per-sample group-description
  indices are run-length-encoded into the compact pattern form (one
  `pattern_length == 1` pattern per run) with minimum-width field
  selectors; the `parse_csgp` read path expands them back to the exact
  per-sample assignment. `add_sample_to_group_with_form(…,
  SampleGroupBoxForm::Classic)` instead writes the widely-compatible
  run-length `sbgp` (SampleToGroupBox, §8.9.2) — same mapping, the form
  every ISO BMFF reader understands (round-trips through `parse_sbgp`).
- `set_sample_group_description(track_id, SampleGroupDescriptionWrite)`
  writes the sibling `sgpd` (SampleGroupDescriptionBox, §8.9.3) that a
  `csgp`/`sbgp`'s indices reference — without it a non-zero index points
  at nothing. Written version 1 (constant `default_length` when entries
  are uniform, else a per-entry `description_length` prefix), before the
  `sbgp`/`csgp` boxes per §8.9.3 ordering. Typed §10 entry constructors
  (`roll_entry` / `prol_entry` / `rap_entry` / `tele_entry` /
  `sap_entry`) mirror the read-side decoders; round-trips through
  `parse_sgpd`.
- `set_metadata(&[MovMetadata])` (movie scope, `moov/udta`) and
  `set_track_metadata(track_id, &[MovMetadata])` (track scope,
  `trak/udta`) emit a User Data Box (QTFF pp. 36–38 / §8.10.1).
  `MovMetadata::intl_text` writes an Apple international-text record
  (`©XXX`); same-FourCC items coalesce into one atom carrying one
  per-language record (set `UTF8_INTL_TEXT_FLAG | iso_language(tag)` for
  a UTF-8 body, a Mac language code for Mac-Roman). `plain_utf8`
  (`name` / `auth` / `cprt`) and `raw` (opaque FourCC) cover the QT-7+
  and unknown shapes. All round-trip through `parse_udta` and surface on
  `MovDemuxer::user_data` / `Track::user_data`.
- `set_apple_metadata(&[MovMetaItem])` emits the modern Apple **QuickTime
  Metadata** box (`moov/meta` = `hdlr` `mdta` + `keys` + `ilst`), distinct from
  the legacy `udta` above. Each `MovMetaItem` becomes one `keys` declaration
  (`[namespace][key]`, namespace defaulting to `mdta`) paired with one `ilst`
  entry whose `data` sub-atom carries the typed value: `MovMetaItem::utf8`
  (type-code 1), `::signed_int` (type-code 21, 32-bit BE), or `::typed` for an
  explicit namespace / type-indicator / raw bytes. Duplicate keys each get their
  own `keys`/`ilst` slot; the 1-based `ilst` key-index references the matching
  `keys` row, so the read-side `parse_keys` / `parse_ilst` resolve every item
  back onto `MovDemuxer::meta` (namespace / key / type-code / value preserved).
  Coexists with `udta` when both are set; no `meta` box when empty.
  `set_track_apple_metadata(track_id, &[MovMetaItem])` does the same at track
  scope (`trak/meta`, surfacing on `Track::meta`); movie and track scopes are
  independent.
- `set_visual_extensions(track_id, VisualExtensions)` attaches the typed
  visual sample-description extension boxes to a **video** track (ISO/IEC
  14496-12 §12.1.4 / §12.1.5, QTFF p. 94): `pasp` (Pixel Aspect Ratio),
  `colr` (Colour Information — Apple `nclc` / ISO `nclx` with the
  `full_range_flag` top bit / ICC / `Other`), `clap` (Clean Aperture, with
  signed `horiz_off_n` / `vert_off_n`), `fiel` (Field Handling), and `gama`
  (a 16.16 fixed-point gamma). Each populated `VisualExtensions` field
  emits its box into the video `stsd` entry's trailing slot — after the
  70-byte fixed body and after the codec-config `extra_stsd_atoms`, so a
  decoder-config box (`avcC` / `hvcC`) stays first — in a stable canonical
  order (`colr`, `pasp`, `clap`, `fiel`, `gama`). The new
  `Pasp`/`Clap`/`ColorParameters`/`Fiel` `to_body_bytes` serialisers are
  the exact inverses of the read-side `parse_*` decoders, so the file
  round-trips through the demuxer's `scan_video_extensions` back onto
  `SampleDescription`'s `pasp` / `colr` / `clap` / `fiel` / `gamma`. An
  empty set writes nothing; a non-video track or unknown `track_id` is
  rejected.
- `set_track_references(track_id, &[TrackReference])` emits a `tref`
  (Track Reference Box, QTFF p. 50 / §8.3.3) between a track's
  `tkhd`/`edts` and its `mdia`, one child atom per reference type
  (FourCC = relationship, body = packed `u32` referenced track ids).
  `TrackReference::chapter(id)` / `::timecode(id)` / `::to(type, id)`
  cover the common cases (`chap`, `tmcd`, `sync`, `scpt`, `cdsc`, …);
  every referenced id is validated against the tracks added so far
  (self-references allowed). Round-trips through `parse_tref` onto
  `Track::references` and the typed `chapter_track_ref` /
  `timecode_track_ref` / `timecode_track_index` accessors.
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
