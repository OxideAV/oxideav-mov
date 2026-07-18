# oxideav-mov

[![CI](https://github.com/OxideAV/oxideav-mov/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-mov/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-mov.svg)](https://crates.io/crates/oxideav-mov) [![docs.rs](https://docs.rs/oxideav-mov/badge.svg)](https://docs.rs/oxideav-mov) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

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
  composition-shift edits and non-unity `media_rate`. By default
  `next_packet` keeps the media-time PTS contract; opt-in
  `apply_edit_lists(true)` **applies** the edit list to packet timing
  (QTFF pp. 46–48 / §8.6.6) — samples outside every edit are dropped,
  timestamps move onto the edited presentation timeline (still in the
  stream's media timescale), rate-N segments scale spacing and
  durations, dwells stretch their held sample, and a mid-sample trim
  clamps the final duration. Per-sample mapping surfaces via
  `edited_timing_for` / `edit::edited_timing_for_sample`
  (`EditedTiming { pts, dts, duration }`), validated against an
  ffprobe black-box oracle. Opt-in `emit_never_presented(true)` keeps
  decode-only media (trimmed heads/tails a presented frame depends on,
  priming audio) instead of dropping it: such samples emit
  **discard-flagged** with timing extrapolated from the nearest
  presenting segment (negative edited pts for head trims), and an
  edited seek landing on a never-presented sync sample reports its
  extrapolated dts. Typed list surface: `parse_elst_full` (`Elst`
  preserves the FullBox version/flags), `Track::elst_version`, and the
  summary accessors `edit_start_delay` / `edit_media_start` /
  `edit_total_duration` — all mappers saturate on hostile 64-bit
  elst fields.
- Track relationships: typed `tref` accessors for every QTFF reference
  kind (`chap`, `tmcd`, `sync`, `scpt`, `hint`, `ssrc`) plus the ISO
  BMFF §8.3.3.3 reference types `cdsc` (content-describes), `font`,
  `hind` (hint dependency), `vdep` / `vplx` (auxiliary depth / parallax
  video), `subt`, and the QTFF 2012-08-14 `folw` Subtitle Follows
  (a sound track's default-subtitle pointer within its alternate
  group) — with track-id → index resolvers; `tsel`
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
- Sound sample-description versioning: version-0 (uncompressed-sample),
  version-1 (QTFF p. 101 `SoundDescriptionV1`) and version-2
  (QTFF 2012-08-14 pp. 181–182 `SoundDescriptionV2`) layouts. The four
  fixed-ratio fields (`samples_per_packet`, `bytes_per_packet`,
  `bytes_per_frame`, `bytes_per_sample`) surface typed via
  [`SoundV1`]; `audio_version` + `audio_compression_id` are exposed,
  and `is_vbr()` decodes the variable-bit-rate "third variant"
  (version 1, Compression ID `-2`, QTFF p. 102). The QuickTime-7
  high-resolution v2 form (`lpcm` uncompressed / `mp4a`-style
  compressed) surfaces typed via [`SoundV2`] — Float64
  `audio_sample_rate` (rates past the 16.16 field's 65535 Hz cap),
  32-bit `num_audio_channels`, the constant-if-nonzero
  bits/bytes/frames-per-packet descriptors, and the `lpcm`
  `format_specific_flags` decoded by [`LpcmFlags`] (float /
  endianness / signed / packed / aligned-high / non-interleaved /
  non-mixable / all-clear bits, the fixed-point sample-fraction
  field, and the Apple Lossless source-depth codes) — with
  extension atoms located via `sizeOfStructOnly` (hostile offsets
  fall back safely) and `audio_sample_rate_hz()` picking the best
  available rate source. The ISO BMFF side
  (§12.2.3/§12.2.4) is read too: an `AudioSampleEntryV1` in a
  version-1 `stsd` (same 20-byte fixed body, no QTFF extension) sets
  `iso_audio_entry_v1`, with the `srat` SamplingRateBox
  (`effective_sample_rate()` override) and the `chnl` ChannelLayout
  (defined ISO/IEC 23001-8 configuration + omitted-channels map, or
  explicit per-channel speaker positions, plus object counts)
  surfaced typed. The Apple `chan`
  channel layout is parsed, and the QTFF 2012-08-14 sound-description
  extension atoms decode typed: `wave` siDecompressionParam
  (required for `mp4a`; `frma` / `esds` / Terminator children with
  `format()` / `esds()` accessors, non-atom `WAVEFORMATEX`-style
  payloads kept verbatim), a directly-carried `esds`, the deprecated
  `flap` siSlopeAndIntercept record, and the `0x00000000` Terminator
  atom — write-side inverses (`SiDecompressionParam::to_atom_bytes`,
  `WaveChild` builders, `build_esds_atom`) plug into a muxer track's
  `extra_stsd_atoms`. Elementary-stream-descriptor *contents* stay
  opaque for the codec crates.
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
- External data references (`dinf/dref`, §8.7.2): a sample whose
  description points at a non-self `dref` entry yields a recoverable
  `Unsupported` error instead of silently emitting local bytes;
  local tracks in the same movie keep demuxing.
  `sample_data_in_file` / `track_has_external_data` expose the
  resolution.

## Seek

`MovDemuxer::seek_to(stream, pts)` walks the per-track sample queue,
snaps to the largest sync (`stss`) sample at-or-before the target DTS,
and resets the cursor so the next `next_packet()` re-emits from there.
Fragmented files use `tfra` when present. With `apply_edit_lists(true)`
the input is an edited-timeline timestamp (resolved back to media time
via the edit list, clamping out-of-presentation targets) and the return
value is the edited dts of the first packet the mode will emit —
`edited_pts_to_media_pts` exposes the resolver.

## Muxer

`MovMuxer` emits a non-fragmented MOV/MP4 (`ftyp` + `mdat` + `moov`)
carrying one or more video / audio / time-code / text / timed-metadata /
subtitle / timed-text / hint tracks,
round-tripping through `MovDemuxer` with sample count, sizes, payloads,
and keyframe flags preserved. `stco` auto-promotes to `co64` when chunk offsets cross
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
  edit (`media_time == -1`) for encoder-priming-skip or start-delay;
  `MuxEdit::dwell` holds one frame (`media_rate == 0`, §8.6.6.3) and
  `MuxEdit::segment_with_rate` takes any positive 16.16 rate
  (negative rates rejected per QTFF p. 48).
  The `elst` auto-promotes from v0 (32-bit) to v1 (64-bit) the moment a
  `track_duration` exceeds 4 GiB-ticks or a `media_time` leaves the
  signed-32-bit range; entries round-trip through the read-side
  `parse_elst`. No `edts` is written when a track has no edit list.
  The fragmented path emits the same `edts` into the init-segment
  `trak`, so a fragmented presentation keeps its edits.
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
- `MuxTrackKind::Timecode { description, tcmi }` writes a full QTFF
  time-code track (QTFF pp. 106–116): a `tmcd`-subtype `hdlr`, a `gmhd`
  base-media header (`gmin` + `tmcd > tcmi`), and a `tmcd` `stsd`
  carrying the timing fields. Each `MuxSample` is a 4-byte packed
  timecode payload built with `Tmcd::encode_sample` (counter or
  `[H:M:S:F]` record). Round-trips onto `Track::gmhd` / the `tmcd`
  sample description / `timecode_sample`; with a media track's
  `tref/tmcd` it resolves via `start_timecode`. (Serialisers
  `Tmcd::to_sample_description_body` / `Tcmi::to_body_bytes` /
  `Gmin::to_body_bytes` are the read-side inverses.)
- `MuxTrackKind::Text { description }` writes a QuickTime text track
  (the chapter-track carrier, QTFF pp. 108–110): a `text`-subtype
  `hdlr`, a `gmhd` header (`gmin` + identity-matrix `text`), and a
  `text` `stsd` (`TextSampleDescription::to_body_bytes`). Each
  `MuxSample` is a `[length:u16][UTF-8 text]` record from
  `encode_text_sample`. With a media track's `tref/chap` the titles
  resolve through `MovDemuxer::chapters_for` (DTS-keyed start + duration,
  Unicode and `encd` encoding preserved).
- `MuxTrackKind::Metadata { description }` writes an ISO BMFF timed-
  metadata track (ISO/IEC 14496-12 §12.3): a `meta`-subtype `hdlr`, an
  `nmhd` Null Media Header Box (§8.4.5.2), and a `stsd` whose single
  entry is a `metx` / `mett` / `urim` `MetadataSampleEntry` (the FourCC
  comes from the variant). New `MetadataSampleEntry::to_body_bytes` (+
  per-variant `XmlMetadataSampleEntry` / `TextMetadataSampleEntry` /
  `UriMetadataSampleEntry` / `BitRate` serialisers) are the exact
  inverses of `parse_metx` / `parse_mett` / `parse_urim` / `parse_btrt`,
  framing the `content_encoding` / `namespace` / `schema_location`
  strings, the `txtC` / `uri ` / `uriI` / `btrt` child boxes. Each
  `MuxSample` is the opaque per-sample metadata record. Round-trips onto
  `SampleDescription::metadata` on the non-fragmented and fragmented
  paths; a media track's `tref/cdsc` to it resolves through
  `Track::references`.
- `MuxTrackKind::Subtitle { description }` writes an ISO BMFF subtitle
  track (ISO/IEC 14496-12 §12.6): a `subt`-subtype `hdlr`, an `sthd`
  Subtitle Media Header Box (§12.6.2), and a `stsd` whose single entry
  is a `stpp` (XML, e.g. TTML) or `sbtt` (text) `SubtitleSampleEntry`.
  New `SubtitleSampleEntry::to_body_bytes` / `format` (+ per-variant
  `XmlSubtitleSampleEntry` / `TextSubtitleSampleEntry` serialisers) are
  the exact inverses of `parse_stpp` / `parse_sbtt`, framing the
  `namespace` / `schema_location` / `auxiliary_mime_types` (stpp) or
  `content_encoding` / `mime_format` (sbtt) strings and the `txtC` /
  `btrt` child boxes. Round-trips onto `SampleDescription::subtitle` on
  the non-fragmented and fragmented paths. Distinct from the QuickTime
  `MuxTrackKind::Text` chapter/overlay track.
- `MuxTrackKind::SimpleText { description }` writes an ISO BMFF timed-
  text track (ISO/IEC 14496-12 §12.5): a `text`-subtype `hdlr`, an
  `nmhd` Null Media Header Box (§12.5.2 — timed-text tracks use a null
  media header, *not* the QuickTime `gmhd`), and a `stsd` whose single
  entry is a `stxt` `SimpleTextSampleEntry`. New
  `SimpleTextSampleEntry::to_body_bytes` is the exact inverse of
  `parse_stxt` (`content_encoding` / `mime_format` strings + `txtC` /
  `btrt` child boxes). Round-trips onto `SampleDescription::simple_text`
  on the non-fragmented and fragmented paths; the `stxt`/`nmhd` shape
  distinguishes it from the QuickTime `MuxTrackKind::Text` track
  (`text`/`gmhd`) — the demuxer disambiguates by the `stsd` FourCC.
- `MuxTrackKind::Hint { protocol, description, hmhd }` writes an ISO BMFF
  hint track (ISO/IEC 14496-12 §12.4), a streaming-server packetization
  track: a `hint`-subtype `hdlr`, an `hmhd` Hint Media Header Box
  (§12.4.2 — max/avg PDU size + bitrate, via the new
  `Hmhd::to_body_bytes`), and a `stsd` whose single entry is a
  protocol-named HintSampleEntry (§12.4.3 — FourCC = `protocol`
  identifier such as `rtp `, body = opaque protocol declarative data).
  The opaque `description` body round-trips onto `SampleDescription::extra`
  and the header onto `Track::hmhd`; a `tref/hint` to the packetized
  media track resolves through `Track::references`. Honoured on the
  non-fragmented and fragmented paths.
- `set_track_language(track_id, packed)` sets `mdhd.language` (pack a
  three-letter ISO-639-2 code with `MovMetadata::iso_language`; default
  `MDHD_LANGUAGE_UND` = `"und"`), and `set_track_extended_language(
  track_id, "en-US")` emits an `elng` Extended Language Tag Box (BCP 47;
  empty string clears it). Both round-trip onto `Track::mdhd.language` /
  `Track::extended_language`.
- `set_track_aperture(track_id, Tapt)` emits a `tapt` (Track Aperture
  Modes box) on a video track, carrying whichever of `clef` (Clean
  Aperture) / `prof` (Production Aperture) / `enof` (Encoded Pixels)
  rectangles are populated (`TaptDims::from_pixels` / `to_body_bytes`,
  16.16 fixed-point). Round-trips through `parse_tapt` onto
  `Track::tapt`; non-video tracks and an empty `Tapt` are rejected.
- `set_data_references(track_id, &[DataReferenceWrite])` replaces a
  track's default single self-referencing `dref` (`url ` flags=1) with
  an explicit table — `DataReferenceWrite::{SelfRef, Url, Urn}` — to
  declare external `url ` / `urn ` storage (reference movies). Must
  contain exactly one `SelfRef`; the muxer points every sample entry's
  `data_reference_index` at it. Round-trips through `parse_dref` onto
  `Track::data_references`.
- `set_track_gmin(track_id, Gmin)` overrides the `gmhd/gmin` Generic
  Media Information header (QTFF p. 65) of a time-code / text track —
  the compositing `graphics_mode` (Table 4-2), the `opcolor` triple the
  blend / transparent modes consult, and the stereo `balance` (8.8
  fixed-point). `set_text_header_matrix(track_id, [i32; 9])` overrides
  the `gmhd/text` transformation matrix (QTFF p. 144) of a text track
  (`tkhd`-convention 16.16 / 2.30 fixed-point). Defaults stay the copy
  graphics mode + centred balance + identity matrix; both overrides
  round-trip through `parse_gmin` / `parse_text_header` onto
  `Track::gmhd`. Rejected on a video / audio track (no `gmhd`) and, for
  the matrix, on a non-text track.
- `set_compact_sample_size(track_id, true)` opts a track into emitting
  its sample-size table as a Compact Sample Size Box (`stz2`, ISO/IEC
  14496-12 §8.7.3.3) with the narrowest 4 / 8-bit `field_size` that fits
  every sample, instead of the default 32-bit `stsz`. Transparently
  falls back to `stsz` when that would be smaller (uniform sizes — a
  table-less `stsz` — or any size above 8 bits), so enabling it is always
  safe. Both forms round-trip onto the same per-sample sizes; the
  read-side `MovDemuxer::sample_size_source` reports which box (and the
  `stz2` `field_size`) carried them.
- Per-sample auxiliary sample-table boxes (`stbl` scope, written after
  the chunk-offset table): `set_sample_dependencies(track_id,
  &[SdtpEntry])` writes the Independent and Disposable Samples Box
  (`sdtp`, §8.6.4) one packed dependency byte per sample (`SdtpEntry::
  to_byte`, the inverse of `from_byte`); `set_degradation_priorities(
  track_id, &[u16])` writes the Degradation Priority Box (`stdp`,
  §8.5.3); `set_padding_bits(track_id, &[u8])` writes the Padding Bits
  Box (`padb`, §8.7.6, two 3-bit rows per byte, values `0..=7`);
  `set_shadow_sync_samples(track_id, &[StshEntry])` writes the Shadow
  Sync Sample Box (`stsh`, §8.6.3, auto-sorted ascending by
  `shadowed_sample_number`); and `set_sub_samples(track_id,
  &[SubSampleInfo])` writes the Sub-Sample Information Box (`subs`,
  §8.7.7, sparse rows sorted + delta-coded, auto-promoting to version 1
  when a sub-sample exceeds 65535 bytes). None carries an on-disk count
  field — each table's length is validated against the track's sample
  count — and all round-trip through `parse_sdtp` / `parse_stdp` /
  `parse_padb` / `parse_stsh` / `parse_subs` back onto
  `Track::sample_table`.
- `set_track_load_settings(track_id, Some(Load))` emits a `load` (Track
  Load Settings atom, QTFF pp. 48–49) as an early `trak` child carrying
  the movie-timescale preload window (`preload_start_time` /
  `preload_duration`, `0xFFFF_FFFF` = to-end) plus the preload-mode
  (`LOAD_PRELOAD_ALWAYS` / `_IF_ENABLED`) and quality-hint
  (`LOAD_HINT_DOUBLE_BUFFER` / `_HIGH_QUALITY`) bitfields. `Load::
  to_body_bytes` is the inverse of `parse_load` (a non-FullBox 16-byte
  body). QuickTime-only; round-trips onto `Track::load`.
- `set_track_clipping(track_id, Some(Clipping))` emits a `clip` > `crgn`
  (Track Clipping atom, QTFF pp. 43–44) as a `trak` child carrying the
  QuickDraw clipping region — a signed-16-bit `QdRect` bounding box plus
  an optional opaque scanline mask. `Clipping::to_body_bytes` /
  `ClippingRegion::to_body_bytes` are the inverses of `parse_clip` /
  `parse_crgn` (the `crgn` `region_size` is recomputed);
  `ClippingRegion::rectangular(QdRect)` builds the minimum region.
  QuickTime-only; round-trips onto `Track::clipping`.
- `set_track_matte(track_id, Some(Matte))` emits a `matt` > `kmat`
  (Track Matte atom, QTFF pp. 44–45) as a `trak` child carrying a coded
  blend matte — a FullBox header, a QTFF image description naming the
  codec, and the compressed matte data. `Matte::to_body_bytes` /
  `CompressedMatte::to_body_bytes` are the inverses of `parse_matt` /
  `parse_kmat`. QuickTime-only; round-trips onto `Track::matte`.
- `set_track_kinds(track_id, &[KindEntry])` emits ISO BMFF Track Kind
  boxes (`kind`, §8.10.4) into the track-level `udta` — one per
  `(schemeURI, value)` role pair (WebVTT / DASH subtitle roles), `Zero
  or more` per track. `KindEntry::to_body_bytes` is the inverse of
  `parse_kind`. The track `udta` carries metadata + kinds together;
  round-trips onto `Track::kinds`.
- `set_track_selection(track_id, Some(TrackSelection))` emits an ISO
  BMFF Track Selection box (`tsel`, §8.10.3) into the track-level `udta`
  — the `switch_group` plus differentiating/descriptive attribute
  FourCCs (`TSEL_ATTR_*`) that group tracks for adaptive switching.
  `TrackSelection::to_body_bytes` is the inverse of `parse_tsel`;
  coexists with metadata + `kind` in one `udta`; round-trips onto
  `Track::track_selection`.
- `set_track_groups(track_id, &[TrackGroupTypeEntry])` emits an ISO BMFF
  Track Group box (`trgr`, §8.3.4) as a `trak` child — one framed
  `TrackGroupTypeBox` per membership entry; tracks sharing a
  `(track_group_type, track_group_id)` pair are one group
  (`TrackGroupTypeEntry::msrc(id)` for the base-spec source group).
  `to_body_bytes` / `to_framed_atom` invert `parse_track_group_type` /
  `parse_trgr`; round-trips onto `Track::track_groups`.
- `set_sound_description_v1(track_id, SoundV1, vbr)` writes a QTFF
  `SoundDescriptionV1` (p. 101) audio entry — version 1 with the four
  fixed-compression-ratio longs, `vbr` selecting the p. 102 VBR "third
  variant" (Compression ID `-2`). `set_sound_description_v2(track_id,
  AudioEntryV2)` writes the QuickTime-7 high-resolution
  `SoundDescriptionV2` (QTFF 2012-08-14 pp. 181–182): the `always*`
  back-compatibility constants, `sizeOfStructOnly` = 72, Float64
  `audioSampleRate` (rates past 65535 Hz and non-integer rates
  survive bit-exact), channel count widened to 32 bits, and the
  `LpcmFlags` / const-descriptor words — use it for `lpcm` and >2-
  channel layouts. `set_audio_entry_v1(track_id,
  AudioEntryV1)` instead writes an ISO BMFF `AudioSampleEntryV1`
  (§12.2.3.2): `entry_version` 1, the `stsd` auto-promoted to FullBox
  version 1, plus optional `srat` / `chnl` boxes
  (`ChannelLayout::to_body_bytes` inverting `parse_chnl`). All three
  mutually exclusive, audio-only, explicit channel layouts validated
  against the channel count; all honoured on the fragmented path.
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
mapper at boundary PTS values, re-runs the seek path, then flips on
the applied edit-list mode for a second drain + edited-seek pass
(probing `edited_pts_to_media_pts` at boundary values). Alias
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
