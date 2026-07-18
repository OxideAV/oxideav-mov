# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Other

- Round 417 — **Saturating rescale across the edit-list mappers**: every `i128 → i64` narrowing in `media_pts_to_movie_pts` / `movie_pts_to_media_pts` / `edited_timing_for_sample` (and the demuxer's `edited_pts_to_media_pts` clamp paths) previously used a plain `as i64` cast — a version-1 `elst` carries free 64-bit `track_duration` / `media_time` fields, so hostile windows past `i64::MAX` (or a 16.16 `media_rate` of 1 blowing a delta up by 65536×) wrapped the result negative. All narrowings now clamp to `i64::MIN..=i64::MAX` (new module-wide `sat_i64`), keeping every mapping total and order-preserving on attacker-controlled input. 4 hostile-value tests (giant v1 movie windows on forward/dwell/timing paths, giant `media_time` on the inverse path, tiny-rate giant-delta forward saturation)

- Round 417 — **HEIF external-validator oracle drift fix**: newer `heif-info` builds stop before listing images when the synthetic test payload carries no real HEVC decoder configuration (`Invalid input: No 'hvcC' box`) and exit non-zero, so the `external_validators_accept_3item_heic` classifier saw neither a positive image-list marker nor a container-rejection signature and mis-filed the codec-level stop as a container rejection. The brand/MIME summary (`MIME type:` / `main brand:`) the tool prints after parsing `ftyp`/`meta` now counts as the container-level acceptance signal, and the no-positive-signal path prints the tool's combined output for diagnosis instead of failing silently

- Public-surface hygiene — `#[doc(hidden)]` on the internal `CodecResolverShim` feature-parity alias (registry and standalone arms); no semantic change. An audit of all ~1150 `pub` items found the rest of the surface is deliberate product API (typed atom/box accessors, demuxer/muxer entry points, walker toolkit, registry contract) and it stays visible

- Round 407 — **Fuzz harness extended to the sound-description surfaces + two hostile-allocation fixes it found.** The `demux` target now sweeps the round-407 typed accessors per sample description (`sound_v2` LPCM flag decode, `audio_sample_rate_hz`, `si_decompression_param` with a `parse_wave` serialise/re-parse **idempotence assertion**, direct `esds` / `flap` / Terminator) and was seeded with a muxed v2 lpcm+`wave` movie. Two OOM classes fell out and are fixed: (1) **attacker-declared read sizes** — `read_next` (and the chapter-text / `iloc`-extent readers) pre-allocated `vec![0; declared]` before reading, so a ~2.7 GB `stsz` constant size inside a 648-byte file allocated gigabytes up front; all sample/extent reads now go through a bounded `Take`-based reader whose buffer grows only as data actually arrives, erroring recoverably on the declared-vs-available shortfall (`iloc` extent-total pre-allocation capped at `MAX_INMEMORY_ATOM_BODY` too); (2) **attacker-declared sample counts** — the constant-size `stsz` count (and `stts` run counts) are free 32-bit fields with no byte-backed table behind them, so a forged ~1.7 billion made `open()` materialise the flattened sample queue (gigabytes + minutes of CPU); the declared total across tracks is now bounded by the input length (with a 1 Mi-sample floor so small external-`dref` reference movies still open) before materialisation, and the zero-row `trun` form (all per-sample-field flags clear, count equally free) is capped at 16 Mi samples. 3-minute local fuzz after the fixes: 1.39 M runs clean. 2 regression tests pin the giant-size and giant-count shapes

- Round 407 — **MovMuxer write-side Sound Sample Description version 2** (QTFF 2012-08-14 pp. 181–182), completing write symmetry for the QuickTime sound-description matrix (v0 / v1 / v2 / ISO `AudioSampleEntryV1`). `set_sound_description_v2(track_id, AudioEntryV2 { audio_sample_rate, const_bits_per_channel, format_specific_flags, const_bytes_per_audio_packet, const_lpcm_frames_per_audio_packet })` writes the QuickTime-7 high-resolution 56-byte body: version 2, the fixed back-compatibility constants in the version-0 positions (`always3`=3 / `always16`=16 / `alwaysMinus2`=-2 / `always0`=0 / `always65536`=65536 / `always7F000000`=0x7F000000 — an old reader sees a sane-shaped v1-style entry), `sizeOfStructOnly`=72 so codec-config `extra_stsd_atoms` land exactly where the read-side extension scan resumes, the **Float64 `audioSampleRate`** (rates past the 16.16 cap and non-integer pulled rates survive bit-exact), `numAudioChannels` derived from the track's channel count, and the [`LpcmFlags`] word. Validation: audio tracks only, finite positive rate required, mutually exclusive with `set_sound_description_v1` / `set_audio_entry_v1` in every ordering (re-setting v2 itself reconfigures), and the enclosing `stsd` FullBox stays version 0 (the version-1 promotion is ISO-entry-only). Honoured on the non-fragmented and fragmented init paths. 8 round-trip/validation tests including an on-wire `always*`-constant byte pin, a `wave{frma,esds,Terminator}` extension round-trip through `SiDecompressionParam::to_atom_bytes`, and a v0-entry direct-`esds`+Terminator round-trip

- Round 407 — **Sound sample-description extension atoms, typed** (QTFF 2012-08-14 pp. 183–187 "Sound Sample Description Extensions"), replacing the previously opaque treatment of the audio entry's trailing atom area. New decoders wired into the audio extension scan: **`wave`** siDecompressionParam (out-of-band decompressor configuration, *required* for `mp4a`) → `SampleDescription::si_decompression_param` (`SiDecompressionParam`) with the framed children in file order (`WaveChild`), `terminated` tracking the mandatory Terminator atom, typed `format()` (`frma` — data-format copied from the sample description) / `esds()` (descriptor bytes) / `child(fourcc)` accessors, and a lenient `non_atom_data` fallback carrying non-atom-shaped payloads verbatim (the WAV/AVI-sourced little-endian `WAVEFORMATEX` case — truncated/undersized child headers never error); **`esds`** MPEG-4 Elementary Stream Descriptor carried directly in the extension area (p. 186 — 32-bit version-zero validated, raw ISO/IEC 14496 descriptor surfaced on `SampleDescription::esds`, contents left to the codec layer); the deprecated **`flap`** siSlopeAndIntercept (p. 184 `SoundSlopeAndInterceptRecord` — four big-endian Float64 `slope`/`intercept`/`min_clip`/`max_clip`, legacy content only) → `SampleDescription::slope_and_intercept`; and the **Terminator** atom (type `0x00000000`, size 8 — p. 185) → `SampleDescription::extension_terminator`. Write symmetry: `SiDecompressionParam::to_payload_bytes` / `to_atom_bytes` (exact inverse of the new `parse_wave`, Terminator included), `WaveChild::format` / `elementary_stream_descriptor` builders for the p. 188 `mp4a` assembly, and `build_esds_atom` (inverse of `parse_esds`) — all suitable for a muxer track's `extra_stsd_atoms`. 5 unit tests including the mp4a `wave{frma,esds,Terminator}` shape, serialiser round-trips, and hostile non-atom fallback sweeps

- Round 407 — **Sound Sample Description version 2 read side** (QTFF 2012-08-14 edition, pp. 181–182 "Sound Sample Description (Version 2)"), completing the QuickTime sound-description version matrix v0/v1/v2 next to the ISO `AudioSampleEntryV1`. `parse_stsd` now decodes the QuickTime-7 high-resolution audio entry (format `lpcm` for uncompressed data; compressed formats keep their type code, normally `mp4a`): the version-0 field positions carry fixed back-compatibility constants on wire (`always3`/`always16`/`alwaysMinus2`/`always0`/`always65536`) and the real parameters follow — `sizeOfStructOnly` (extension-atom offset), **Float64 `audioSampleRate`** (rates past the 16.16 field's 65535 Hz cap), 32-bit `numAudioChannels`, `constBitsPerChannel`, `formatSpecificFlags`, `constBytesPerAudioPacket`, `constLPCMFramesPerAudioPacket` — surfaced typed on the new `SampleDescription::sound_v2` (`SoundV2`), with the legacy `channels` / `bits_per_sample` / `sample_rate` fields repopulated from the v2 truth. The LPCM flag word (p. 183 "LPCM flag values") decodes via the new `LpcmFlags` — is-float / big-endian / signed-integer / packed / aligned-high / non-interleaved / non-mixable / all-clear predicates, the 6-bit linear-PCM sample-fraction field, and the Apple Lossless source-bit-depth whole-word codes. Extension atoms are located at `sizeOfStructOnly` from the entry start (72 as specified; a grown struct skips the unknown tail, and hostile offsets — zero, inside the fixed struct, past the entry — fall back to the end of the fixed fields). New `audio_sample_rate_hz()` accessor prefers the finite v2 Float64 over `effective_sample_rate()`; `is_vbr()` documented to stay v1-only (the v2 on-wire `alwaysMinus2` is not the p. 102 VBR variant). Also new: the `folw` **Subtitle Follows** track-reference type (p. 187 — a sound track's default-subtitle pointer within its alternate group) classifies as `TrackRefKind::SubtitleFollows`. 7 unit tests including hostile `sizeOfStructOnly` / non-finite-rate sweeps and a truncated-body over-read pin

- Round 394 — **Applied edit-list packet timing** (QTFF Chapter 2 "Edit Atoms" pp. 46–48 / ISO/IEC 14496-12 §8.6.6): the demuxer has long *parsed* `edts/elst` and exposed the media↔movie mappers (`movie_pts_for` / `media_pts_for` / `edit_segments_for`), but `next_packet()` always emitted raw media-timeline timestamps. New opt-in `MovDemuxer::apply_edit_lists(true)` (query via `edit_lists_applied()`) switches `next_packet()` to the **edited (presentation) timeline**: samples whose presentation timestamp falls outside every edit segment are dropped (encoder-priming skip, trimmed media), a head empty edit delays every timestamp, a trim edit shifts them toward zero, a non-unity `media_rate` segment scales spacing *and* durations (rate 2.0 halves both, per the QTFF Chapter 5 pp. 226–227 consumption model), a dwell (rate 0) stretches its held sample across the segment window, and a segment that ends mid-sample clamps the final sample's emitted duration. Timestamps stay in the stream's media timescale (`Packet.time_base` unchanged; the edited origin is movie time 0 rescaled half-up). Tracks without an edit list follow the QTFF p. 47 "no edits" rule and are untouched. The per-sample core is the new `edit::edited_timing_for_sample` (→ `EditedTiming { pts, dts, duration }`, dts keeping the rate-scaled ctts composition offset) surfaced per-track via `MovDemuxer::edited_timing_for(track, &sample)` which works with or without the mode enabled. 12 muxer→demuxer round-trip tests (trim / empty / dwell / rate-2.0 / cross-timescale / B-frame ctts / multi-track / partial-sample clamping) + an ffprobe black-box oracle asserting our applied-edit pts sequence matches a real player's on a trimmed movie

- Round 394 — **External data-reference handling on demux** (QTFF p. 65 / ISO/IEC 14496-12 §8.7.2.1): a sample description whose 1-based `data_reference_index` resolves to a **non-self-referencing** `dref` entry declares its media bytes live in another file (the `0x000001` flag "means the media data is in the same file"), but `read_next` previously read the chunk offsets against the local file anyway — silently emitting whatever bytes sat at those offsets. Such samples now yield a **recoverable** `Unsupported` error naming the sample, track, and dref entry; the cursor advances first, so a movie mixing external and local tracks keeps demuxing the local ones (`next_packet` skips nothing silently — the caller sees one error per external sample and decides). New surface: `sample_data_in_file(track, &sample)` (per-sample resolution incl. multi-entry tables where only some descriptions are external) and `track_has_external_data(track)`. Absent/empty `dref`, an out-of-spec zero index, and dangling indices all stay lenient-local (the historical behaviour for every self-contained writer). Also fixed the shared test builder `build_stsd_video` still using the pre-round-394 width/height offsets. 3 hand-built-fixture tests (recoverable error + local-track continuation, per-sample query, self-pointing multi-entry table)

- Round 394 — **Fuzz harness extended to the applied edit-list surface**: after the existing open/accessor-sweep/drain/seek pass, the `demux` target now flips the demuxer onto the edited timeline (`apply_edit_lists(true)`) and runs a second bounded drain + seek — the per-sample `edited_timing_for_sample` mapper sits on the `next_packet` hot path and must survive attacker-controlled edit lists (zero timescales, dwells, extreme 16.16 rates, overflowing segment windows); the `edited_pts_to_media_pts` resolver is probed at `i64::MIN`/`MAX`/0 and an input-derived point per track. 60-second local smoke: 18M runs clean

- Round 394 — **Edit-derived `tkhd` / `mvhd` durations on write** (QTFF p. 41): the tkhd Duration field "is derived from the track's edits. The value of this field is equal to the sum of the durations of all of the track's edits. If there is no edit list, then the duration is the sum of the sample durations, converted into the movie timescale." The muxer previously always wrote the rescaled media duration, so a trimmed/delayed track (any `set_edit_list` caller) declared the wrong presentation length in both `tkhd` and (through the longest-track rule) `mvhd`. `track_movie_duration` now sums `MuxEdit.track_duration` when edits are present; the no-edit path is unchanged. 3 round-trip tests (edited sum incl. empty edits, no-edit media-derived pin, mixed-track longest-rule)

- Round 394 — **MovMuxer write-side sound sample-description versions**, closing the version matrix the read side already covers. `set_sound_description_v1(track_id, SoundV1, vbr)` writes the QTFF **`SoundDescriptionV1`** (p. 101): version 1 with the four 32-bit fixed-compression-ratio longs after the 20-byte v0 body; `vbr: true` selects the p. 102 VBR "third variant" (Compression ID `-2` — each sample a compressed frame). Codec-config `extra_stsd_atoms` follow at byte 36 where the read-side scan resumes. `set_audio_entry_v1(track_id, AudioEntryV1 { sampling_rate, channel_layout })` writes the ISO BMFF **`AudioSampleEntryV1`** (ISO/IEC 14496-12:2015 §12.2.3.2): `entry_version` = 1 in the same 20-byte fixed body, the enclosing `stsd` FullBox auto-promoted to version 1 per §8.5.2, an optional `srat` SamplingRateBox and an optional `chnl` ChannelLayout box (via the new `ChannelLayout::to_body_bytes`, the exact inverse of `parse_chnl`) emitted after the codec config. Validation: audio tracks only, the two layouts mutually exclusive, an `Explicit` channel layout must carry exactly one row per track channel (§12.2.4.2 sizes the read loop from the entry's `channelcount`), and `Defined { defined_layout: 0 }` is rejected (that wire value selects the explicit form). Both shapes honoured on the non-fragmented and fragmented paths; ffprobe black-box-validated. 9 round-trip/validation tests

- Round 394 — **ISO `AudioSampleEntryV1` + `srat` + `chnl` read side** (ISO/IEC 14496-12:2015 §12.2.3 / §12.2.4), completing the sound sample-description version matrix next to the QTFF v0/v1 layouts: `parse_stsd` now honours the `stsd` FullBox version — an audio entry with `entry_version == 1` inside a **version-1** `stsd` is an ISO `AudioSampleEntryV1` (`SampleDescription::iso_audio_entry_v1`), whose fixed body is the *same* 20 bytes as version 0 (the QTFF 16-byte `CompressionInfo` extension is NOT consumed — previously such files were misparsed with 16 bytes of trailing boxes swallowed into `SoundV1` fields). The QTFF `SoundDescriptionV1` path (version-0 `stsd`) is untouched. Two new trailing-box decoders surface typed: `parse_srat` (SamplingRateBox — the *actual* sampling rate when the 16.16 `samplerate` field can't represent it, exposed on `SampleDescription::sampling_rate` with the `effective_sample_rate()` accessor implementing the §12.2.3.3 override rule) and `parse_chnl` (ChannelLayout — `stream_structure` channel/object flags; `definedLayout != 0` → ISO/IEC 23001-8 ChannelConfiguration + 64-bit `omittedChannelsMap`; `definedLayout == 0` → one explicit `SpeakerPosition` per sample-entry channel with the code-126 azimuth/elevation pair; object-structured `object_count`) onto `SampleDescription::chnl` via the new `ChannelLayout` / `ChannelStructure` / `SpeakerPosition` types. 6 unit tests including truncation rejection and the QTFF-v1-unaffected pin

- Round 394 — **Edited-timeline seek**: with `apply_edit_lists(true)` enabled, `MovDemuxer::seek_to(stream, pts)` now interprets `pts` on the edited (presentation) timeline — the same contract applied-mode `next_packet()` emits — instead of raw media time. The seek resolves the edited timestamp back to media time through `movie_pts_to_media_pts` (QTFF pp. 46–48 / ISO/IEC 14496-12 §8.6.6), drives the existing sync-snap machinery, and returns the **edited dts of the first packet the applied mode will actually emit** (the landed sync sample may itself be dropped by the edit list — e.g. a segment whose `media_time` starts just past a keyframe — so the return contract "next packet's dts equals this" is preserved by peeking past dropped samples). Out-of-presentation targets clamp to the nearest presented media tick: inside an empty-edit window (or before the first presented segment) → the next presented segment's media start; past the end → the last presented segment's final media tick. The resolver is exposed as `MovDemuxer::edited_pts_to_media_pts(track, pts)`; the mode-off path is byte-for-byte the historical media-time seek (`seek_media_impl`). 7 tests (exact landing, sync-snap with sparse keyframes, empty-edit clamp, past-end clamp, dropped-sync reporting, mode-off contract, resolver clamps)

- Round 394 — **Video sample-description field-offset fix** (QTFF p. 92): both sides of the crate placed `width`/`height` at bytes 24..28 of the 70-byte fixed video `stsd` body — where the spec puts the *vertical resolution* — and consequently shifted `hres`/`vres` (written at 16..24 instead of 20..28), `frame_count` (28 instead of 32), `depth` (64 instead of 66) and `color_table_id` (66 instead of 68). Per the QTFF p. 92 field list (and the Chapter 5 worked example, where `0140`/`00F0` = 320×240 directly follow the spatial-quality dword), width/height sit at bytes 16..20, right after the two quality fields. The muxer and demuxer were mutually consistent so self-round-trips always passed; against *external* files the demuxer read 72-dpi resolution bytes as dimensions and external readers saw our files as 0×0 (caught black-box: ffprobe reported `unspecified size` on a muxed `raw ` video track). Demuxer now reads 16..20, muxer writes every field at its spec offset. Found by the round-394 ffprobe oracle; pinned by the updated `stsd_video_entry_extracts_dims` unit test and the oracle test in `tests/synth_round394_edited_timeline.rs`

- Round 379 — MovMuxer write-side **Track Group box** (`trgr`, ISO/IEC 14496-12 §8.3.4), the track-grouping membership declaration the demuxer has long read (`Track::track_groups` / `MovDemuxer::track_group_entries` / `track_groups` via `parse_trgr` / `parse_track_group_type`) but the muxer could never write. New `MovMuxer::set_track_groups(track_id, &[TrackGroupTypeEntry])` emits a `trgr` (TrackGroupBox) as a `trak` child (after `tref`, before `mdia`) — one framed `TrackGroupTypeBox` FullBox child per membership entry, the child's FourCC being the `track_group_type`. Tracks sharing a `(track_group_type, track_group_id)` pair belong to the same group (`TrackGroupTypeEntry::msrc(id)` builds the base-spec multi-source-presentation membership; vendor / derived-spec groups carry a type-specific `payload`). A track may belong to several groups. New `TrackGroupTypeEntry::to_body_bytes` / `to_framed_atom` are the exact inverses of `parse_track_group_type` / `parse_trgr`. Passing an empty slice removes the box. Round-trips onto `Track::track_groups`, and two tracks written with the same `msrc` id resolve together through the demuxer's `track_groups()` dual lookup. ISO BMFF-only (QTFF does not define it). 6 muxer round-trip tests + 3 serialiser unit tests

- Round 379 — MovMuxer write-side **Track Selection box** (`tsel`, ISO/IEC 14496-12 §8.10.3), the adaptive-switching descriptor the demuxer has long read (`Track::track_selection` / `MovDemuxer::track_selection` via `parse_tsel` / `find_tsel_in_udta`) but the muxer could never write. New `MovMuxer::set_track_selection(track_id, Some(TrackSelection))` emits the box into the track-level `udta` (alongside metadata + `kind` boxes), carrying the `switch_group` (tracks sharing a non-zero value are switchable alternatives within their `tkhd.alternate_group`) plus an `attributes` list of differentiating/descriptive FourCCs (`cdec` codec, `bitr` bitrate, … — the `TSEL_ATTR_*` constants). §8.10.3.1 allows at most one per track (`Quantity: Zero or one`). New `TrackSelection::to_body_bytes` is the exact inverse of `parse_tsel` (`[version=0][flags=0][switch_group:i32]` then each attribute FourCC, the list running to the end of the box). Passing `None` removes the box. Round-trips onto `Track::track_selection`; ISO BMFF-only (QTFF does not define it). 5 muxer round-trip tests + 2 serialiser unit tests

- Round 379 — MovMuxer write-side **Track Kind box** (`kind`, ISO/IEC 14496-12 §8.10.4), the track role/kind label the demuxer has long read (`Track::kinds` / `MovDemuxer::track_kinds` via `parse_kind` / `find_kinds_in_udta`) but the muxer could never write. New `MovMuxer::set_track_kinds(track_id, &[KindEntry])` emits the boxes into the track-level `udta` (the same container as track metadata, after any metadata items), one per `(schemeURI, value)` role pair — the canonical use is signalling a subtitle / caption track's intent against a WebVTT or DASH role scheme. §8.10.4.1 allows more than one per track (`Quantity: Zero or more`). New `KindEntry::to_body_bytes` is the exact inverse of `parse_kind` (`[version=0][flags=0][schemeURI\0][value\0]`; a `None` / empty value emits a bare terminator that reads back as `None`). The track `udta` now fires when *either* metadata or kinds are present, so the two coexist in one box. Passing an empty slice removes the kinds. Round-trips onto `Track::kinds`; `kind` is ISO BMFF-only (QTFF does not define it). 5 muxer round-trip tests + 3 serialiser unit tests

- Round 379 — MovMuxer write-side **Track Matte atom** (`matt` > `kmat`, QTFF pp. 44–45), a QuickTime-only `trak` child the demuxer has long read (`Track::matte` via `parse_matt` / `parse_kmat`) but the muxer could never write. New `MovMuxer::set_track_matte(track_id, Some(Matte))` emits a `matt` (wrapping a single framed `kmat` Compressed Matte) as a `trak` child carrying a coded blend matte — the FullBox `version`/`flags` header, a QTFF image description structure (the same on-disk shape as a video sample description, naming the codec), and the trailing compressed matte data. New `Matte::to_body_bytes` and `CompressedMatte::to_body_bytes` are the exact inverses of `parse_matt` / `parse_kmat` (the image description is emitted verbatim — its leading 4-byte size word the caller's responsibility, exactly as a video sample description carries its own size). Passing `None` removes the box. Round-trips onto `Track::matte`; QuickTime-only (ISO BMFF does not define it), so the fragmented init `moov` does not carry it. 4 muxer round-trip tests + 3 serialiser unit tests

- Round 379 — MovMuxer write-side **Track Clipping atom** (`clip` > `crgn`, QTFF pp. 43–44), a QuickTime-only `trak` child the demuxer has long read (`Track::clipping` via `parse_clip` / `parse_crgn`) but the muxer could never write. New `MovMuxer::set_track_clipping(track_id, Some(Clipping))` emits a `clip` (wrapping a single framed `crgn` Clipping Region) as a `trak` child carrying the QuickDraw bounding-box rectangle (`QdRect`, signed 16-bit top/left/bottom/right) plus an optional opaque scanline payload for a non-rectangular mask. New `Clipping::to_body_bytes` and `ClippingRegion::to_body_bytes` are the exact inverses of `parse_clip` / `parse_crgn` (the `crgn`'s `region_size` field is recomputed from the scanline length, so a caller-supplied stale value is corrected); a convenience `ClippingRegion::rectangular(QdRect)` builds the minimum legal region. Passing `None` removes the box. Round-trips onto `Track::clipping`; QuickTime-only (ISO BMFF does not define it), so the fragmented init `moov` does not carry it. 5 muxer round-trip tests + 3 serialiser unit tests

- Round 379 — MovMuxer write-side **Track Load Settings atom** (`load`, QTFF pp. 48–49), a QuickTime-only `trak` child the demuxer has long read (`Track::load` via `parse_load`) but the muxer could never write. New `MovMuxer::set_track_load_settings(track_id, Some(Load))` emits a `load` as an early `trak` child (after `tapt`, before `edts`) carrying the movie-timescale preload window (`preload_start_time` / `preload_duration`, with `0xFFFF_FFFF` = "to the end of the track"), the mutually-exclusive preload-mode flags (`LOAD_PRELOAD_ALWAYS` / `LOAD_PRELOAD_IF_ENABLED`), and the playback-quality hint bitfield (`LOAD_HINT_DOUBLE_BUFFER` / `LOAD_HINT_HIGH_QUALITY` plus any vendor bits). The new `Load::to_body_bytes` is the exact inverse of `parse_load` (four big-endian `u32`s; the atom is **not** a FullBox). Passing `None` removes the box. Round-trips onto `Track::load`; `load` is QuickTime-only (ISO BMFF does not define it), so the fragmented init `moov` does not carry it. 4 muxer round-trip tests + 1 serialiser unit test

- Round 379 — MovMuxer write-side **per-sample auxiliary sample-table boxes**, closing the demux↔mux symmetry gap on five `stbl`-scope tables the demuxer has long read (`parse_sdtp` / `parse_stdp` / `parse_padb` / `parse_stsh` / `parse_subs`, surfaced on `Track::sample_table` plus the typed `MovDemuxer` accessors) but the muxer could never write. (1) `MovMuxer::set_sample_dependencies(track_id, &[SdtpEntry])` emits a **Independent and Disposable Samples Box** (`sdtp`, ISO/IEC 14496-12 §8.6.4) — one packed dependency byte per sample, the four 2-bit fields (`is_leading` / `sample_depends_on` / `sample_is_depended_on` / `sample_has_redundancy`) packed MSB-first via the new `SdtpEntry::to_byte` (the exact inverse of `from_byte`; verified over all 256 byte values). (2) `set_degradation_priorities(track_id, &[u16])` emits a **Degradation Priority Box** (`stdp`, §8.5.3) — one 16-bit priority per sample. (3) `set_padding_bits(track_id, &[u8])` emits a **Padding Bits Box** (`padb`, §8.7.6) — one 3-bit `pad` value per sample, two rows packed per byte (`[reserved:1, pad1:3, reserved:1, pad2:3]`, reserved bits zero, trailing odd nibble zero-padded); values above 7 are rejected. (4) `set_shadow_sync_samples(track_id, &[StshEntry])` emits a **Shadow Sync Sample Box** (`stsh`, §8.6.3) — the muxer sorts the shadowed→sync pairs ascending by `shadowed_sample_number` (§8.6.3.1) and rejects duplicates / out-of-range numbers. (5) `set_sub_samples(track_id, &[SubSampleInfo])` emits a **Sub-Sample Information Box** (`subs`, §8.7.7) — the sparse per-sample table is sorted by `sample_number` then delta-coded (§8.7.7.3), auto-promoting from version 0 (16-bit `subsample_size`) to version 1 (32-bit) the moment any sub-sample exceeds 65535 bytes; duplicate / zero sample numbers are rejected. None of the five carries an on-disk count field (the row count is implied by the sample-size table); the muxer validates each table's length against the track's sample count. All five round-trip exactly through the demuxer and coexist in one `stbl`. Applies to the non-fragmented write path. 17 round-trip / validation tests

- Round 375 — MovMuxer write-side **Compact Sample Size Box** (`stz2`, ISO/IEC 14496-12 §8.7.3.3), closing the demux↔mux symmetry gap on the compact sample-size encoding the demuxer already read (`parse_stz2`, surfaced via `MovDemuxer::sample_size_source` as `SampleSizeSource::Stz2 { field_size }`) but the muxer only ever wrote as the wider `stsz`. New `MovMuxer::set_compact_sample_size(track_id, true)` opts a track into emitting `stz2` with the narrowest 4 / 8-bit `field_size` that fits every sample (the 4-bit form packs two values per byte MSB-first, zero-padding the final low nibble on an odd count per §8.7.3.3.2). The muxer falls back to `stsz` transparently whenever that would be at least as small — when the per-sample sizes are uniform (a table-less `stsz` is strictly smaller) or when the largest size exceeds 8 bits (the 16-bit `stz2` form saves nothing over the existing 32-bit `stsz` table here) — so enabling it is always safe and never enlarges output. Both forms round-trip onto the identical per-sample sizes through the demuxer's `SampleTable`, and `sample_size_source` reports which box (and the `stz2` `field_size`) carried them. Applies to the non-fragmented write path; the fragmented path carries sizes in `trun`. 7 round-trip / fallback tests

- Round 375 — MovMuxer write-side **ISO BMFF hint track** (ISO/IEC 14496-12 §12.4), a streaming-server packetization track, closing the demux↔mux symmetry gap on the hint media the demuxer already read (`Track::hmhd` via `parse_hmhd`, protocol-named `stsd` entries) but the muxer could never write. New `MuxTrackKind::Hint { protocol: [u8;4], description: Vec<u8>, hmhd: Hmhd }` emits a complete hint track: an `hdlr` with component subtype `hint`, an `hmhd` Hint Media Header Box (§12.4.2 — `maxPDUsize` / `avgPDUsize` / `maxbitrate` / `avgbitrate`), and a `stsd` whose single entry is a protocol-named HintSampleEntry (§12.4.3 — the entry FourCC is the `protocol` identifier such as `rtp ` / `srtp`, the body is opaque protocol-specific declarative data such as a `tims` timescale box). Each `MuxSample`'s data is the opaque per-packet hint record. New `Hmhd::to_body_bytes` is the exact inverse of `parse_hmhd` (the 20-byte `[ver+flags][maxPDU:u16][avgPDU:u16][maxbr:u32][avgbr:u32][reserved:u32]` body). A file written this way round-trips through the demuxer onto `Track::hmhd` and `SampleDescription::format` / `::extra` on both the non-fragmented and fragmented init paths; a `tref/hint` to the packetized media track resolves through `Track::references` (§12.4.1). 4 muxer round-trip tests + 1 serialiser unit test

- Round 375 — MovMuxer write-side **ISO BMFF timed-text track** (`stxt` SimpleTextSampleEntry, ISO/IEC 14496-12 §12.5), closing the demux↔mux symmetry gap on the timed-text sample entry the demuxer already read (`SampleDescription::simple_text` via `parse_stxt`) but the muxer could never write. New `MuxTrackKind::SimpleText { description: SimpleTextSampleEntry }` emits a complete timed-text track: an `hdlr` with component subtype `text`, an `nmhd` Null Media Header Box (§12.5.2 — timed-text tracks use a null media header, *not* the QuickTime `gmhd` of `MuxTrackKind::Text`), and a `stsd` whose single entry is a `stxt` PlainTextSampleEntry/SimpleTextSampleEntry. Each `MuxSample`'s data is the opaque per-sample text document. New `SimpleTextSampleEntry::to_body_bytes` is the exact inverse of `parse_stxt` — the leading NUL-terminated `content_encoding`? / `mime_format` strings and the `txtC` TextConfigBox / `btrt` BitRateBox child boxes (same shape as `mett` / `sbtt`). Round-trips through the demuxer onto `SampleDescription::simple_text` on both the non-fragmented and fragmented init paths; the `stxt`/`nmhd` shape distinguishes it from the QuickTime `MuxTrackKind::Text` chapter/overlay track (`text`/`gmhd`), the two disambiguated by the `stsd` FourCC. 3 muxer round-trip tests + 1 serialiser unit test

- Round 375 — MovMuxer write-side **ISO BMFF subtitle track** (`stpp` / `sbtt`, ISO/IEC 14496-12 §12.6), closing the demux↔mux symmetry gap on the subtitle sample entries the demuxer already read (`SampleDescription::subtitle` via `parse_subtitle_sample_entry`) but the muxer could never write. New `MuxTrackKind::Subtitle { description: SubtitleSampleEntry }` emits a complete subtitle track: an `hdlr` with component subtype `subt`, an `sthd` Subtitle Media Header Box (§12.6.2 — an empty FullBox), and a `stsd` whose single entry is a `stpp` (XMLSubtitleSampleEntry, e.g. TTML) or `sbtt` (TextSubtitleSampleEntry) — the FourCC taken from the variant. Each `MuxSample`'s data is the opaque per-sample subtitle document. New serialisers `SubtitleSampleEntry::to_body_bytes` / `format` and the per-variant `XmlSubtitleSampleEntry::to_body_bytes` / `TextSubtitleSampleEntry::to_body_bytes` are the exact inverses of `parse_stpp` / `parse_sbtt` — framing the leading NUL-terminated `namespace` / `schema_location` / `auxiliary_mime_types` (`stpp`) or `content_encoding` / `mime_format` (`sbtt`) strings (the trailing optional strings emitted only as far as needed so the read-side positional assignment reconstructs identical fields) and the `txtC` / `btrt` child boxes. Round-trips through the demuxer onto `SampleDescription::subtitle` on both the non-fragmented and fragmented init paths; structurally distinct from the QuickTime `MuxTrackKind::Text` chapter/overlay track. 6 muxer round-trip tests + 2 serialiser unit tests

- Round 375 — MovMuxer write-side **ISO BMFF timed-metadata track** (`metx` / `mett` / `urim`, ISO/IEC 14496-12 §12.3), closing the demux↔mux symmetry gap on the timed-metadata sample entries the demuxer already read (`SampleDescription::metadata` via `parse_metadata_sample_entry`) but the muxer could never write. New `MuxTrackKind::Metadata { description: MetadataSampleEntry }` emits a complete metadata track: an `hdlr` with component subtype `meta`, an `nmhd` Null Media Header Box (§8.4.5.2 — metadata tracks carry no specific media header), and a `stsd` whose single entry is a `metx` (XMLMetaDataSampleEntry), `mett` (TextMetaDataSampleEntry), or `urim` (URIMetaSampleEntry) — the FourCC taken from the variant. Each `MuxSample`'s data is the opaque per-sample metadata record (an "I-frame" carrying the complete metadata for its interval, §12.3.3.1). New serialisers `MetadataSampleEntry::to_body_bytes` / `format` and the per-variant `XmlMetadataSampleEntry::to_body_bytes` / `TextMetadataSampleEntry::to_body_bytes` / `UriMetadataSampleEntry::to_body_bytes` / `BitRate::to_body_bytes` are the exact inverses of `parse_metx` / `parse_mett` / `parse_urim` / `parse_btrt` — framing the leading NUL-terminated `content_encoding` / `namespace` / `schema_location` / `mime_format` strings (the string count chosen so the read side's positional assignment reconstructs the same fields) and the `txtC` TextConfigBox / `uri ` URIBox / `uriI` URIInitBox / `btrt` BitRateBox child boxes. A file written this way round-trips through the demuxer onto `SampleDescription::metadata` on both the non-fragmented and fragmented init paths; a media track's `tref/cdsc` to the metadata track resolves through `Track::references` (§12.3.1). 8 muxer round-trip tests + 4 serialiser unit tests

- Round 375 — MovMuxer write-side **gmhd/gmin + gmhd/text matrix override**, closing the demux↔mux symmetry gap on the Base Media Information Header fields the demuxer already read (`Track::gmhd`'s `gmin` / `text` slots via `parse_gmin` / `parse_text_header`) but the muxer always hard-coded. Earlier rounds wrote every time-code / text track's `gmhd/gmin` as the default (copy graphics mode, no opcolor, centred balance) and every text track's `gmhd/text` with the identity transformation matrix. `MovMuxer::set_track_gmin(track_id, Gmin)` now overrides the Generic Media Information header (QTFF p. 65) — the compositing `graphics_mode` (Table 4-2), the `opcolor` RGB triple the blend / transparent / straight-alpha-blend modes consult, and the stereo `balance` (8.8 signed fixed-point); it is accepted only on a track that carries a `gmhd` (time-code or text — a `vmhd`/`smhd` video / audio track is rejected). `MovMuxer::set_text_header_matrix(track_id, [i32; 9])` overrides the `gmhd/text` media-information header's 9-element transformation matrix (QTFF p. 144) that maps each text sample's local coordinates onto the movie canvas, in the `tkhd`/`text` fixed-point convention (16.16 for the six scale/skew/translate entries, 2.30 for the right-hand column); accepted only on a text track. Both overrides round-trip through `parse_gmin` / `parse_text_header` onto `Track::gmhd`; leaving either unset preserves the previous default output bit-for-bit. Honoured on the non-fragmented and fragmented init paths. Unknown track ids are rejected

- Round 372 — MovMuxer write-side **per-track language** — the packed `mdhd.language` ISO-639-2/T code (QTFF p. 197 / ISO/IEC 14496-12 §8.4.2.3) and the `elng` Extended Language Tag Box (§8.4.6), both previously read-only on the demuxer (`Track::mdhd.language`, `Track::extended_language`). Until now every muxed track wrote a hard-coded `"und"` (undetermined) language. `MovMuxer::set_track_language(track_id, packed_u16)` sets the `mdhd.language` field — pack a three-letter code with the existing `MovMetadata::iso_language(*b"eng")` (the same packing the read-side `iso_language_tag` inverts); the default is the new public `MDHD_LANGUAGE_UND` (`0x55C4` = `"und"`). `MovMuxer::set_track_extended_language(track_id, "en-US")` emits an `elng` box (`[ver+flags][NUL-terminated BCP 47 tag]`) in `mdia` after `hdlr` — the inverse of `parse_elng`; an empty string clears it (no box). Both round-trip onto the read side and are honoured on the non-fragmented and fragmented init paths; the two coexist (script-subtag tags like `zh-Hant-HK` survive). Unknown track ids are rejected

- Round 372 — MovMuxer write-side **chapter / text track** (QTFF pp. 108–110), the inverse of the demuxer's existing QuickTime `text`-track read path (`Track::is_text`, `Track::gmhd`, `parse_text_sample_description`) and chapter resolver (`MovDemuxer::chapters_for`). New `MuxTrackKind::Text { description: TextSampleDescription }` emits a complete text track: an `hdlr` with component subtype `text`, a `gmhd` base-media header (a `gmin` plus a `text` media-information atom carrying an identity transformation matrix), and a `stsd` whose single `text` entry carries the display configuration (`display_flags` / justification / fore-&-background RGB48 / font). Each `MuxSample`'s data is a `[length:u16][UTF-8 text]` record — build it with the new `chapter::encode_text_sample(text, encoding)` (inverse of `decode_text_sample_full`; an optional `encd` text-encoding-override trailer is appended when `encoding` is `Some`). New `TextSampleDescription::to_body_bytes` is the exact inverse of `parse_text_sample_description`. Combined with the new `tref` write path, a media track's `tref/chap` to one of these resolves through `MovDemuxer::chapters_for` to the per-sample titles (DTS-keyed `start_time` + `duration`), round-tripping Unicode titles and the `text_encoding`. Honoured on both the non-fragmented and fragmented init paths

- Round 372 — MovMuxer write-side **time-code track** (QTFF pp. 106–116), the inverse of the demuxer's existing `tmcd`-track read path (`Track::is_timecode`, `Track::gmhd`, the `tmcd` `stsd` description, `MovDemuxer::timecode_sample` / `start_timecode`). New `MuxTrackKind::Timecode { description: Tmcd, tcmi: Tcmi }` emits a complete time-code track: an `hdlr` with component subtype `tmcd`, a `gmhd` Base Media Information Header carrying a `gmin` plus a `tmcd > tcmi` Time-Code Media Information atom, and a `stsd` whose single `tmcd` entry carries the timing fields (`time_scale` / `frame_duration` / `number_of_frames` / flags + optional source-tape `name`). Each `MuxSample`'s data is a 4-byte packed timecode payload — build it with the new `Tmcd::encode_sample(&TimecodeSample)` (inverse of `decode_sample`: a `Counter` is a BE u32, a `Record` packs `[Hours][sign|Minutes][Seconds][Frames]`). New serialisers `Tmcd::to_sample_description_body` (inverse of `parse_tmcd_sample_description`, source-tape `name` atom included), `Tcmi::to_body_bytes` (inverse of `parse_tcmi`), and `Gmin::to_body_bytes` (inverse of `parse_gmin`) underpin it. The track round-trips through the demuxer onto `Track::gmhd` (`gmin` + `tcmi`), the `tmcd` sample description, and per-sample `timecode_sample`; combined with the new `tref` write path a media track's `tref/tmcd` resolves to it via `start_timecode`. Drop-frame / counter / negative-record forms all round-trip. Honoured on both the non-fragmented and fragmented init paths

- Round 372 — MovMuxer write-side **custom Data Reference Box** (`dref`, QTFF p. 65 / ISO/IEC 14496-12 §8.7.2), the inverse of the demuxer's existing `parse_dref`. By default every track's `dinf/dref` carries a single self-referencing `url ` entry (`flags=1`, "media is in this file"); `MovMuxer::set_data_references(track_id, &[DataReferenceWrite])` replaces that with an explicit table — e.g. to declare external `url ` / `urn ` storage locations for a reference movie. The new `DataReferenceWrite` enum mirrors the read-side `DataReference`: `SelfRef` (`url ` flags=1, empty data slot per §8.7.2), `Url(String)` (`url ` flags=0, NUL-terminated UTF-8 URL), and `Urn { name, location }` (`urn ` flags=0, NUL-terminated name then an optional NUL-terminated location). Because the muxer always lays samples into this file's own `mdat`, the table must contain **exactly one** `SelfRef`; the muxer points every sample entry's `data_reference_index` at its 1-based position (verified by a stsd-byte-offset test for both first- and second-position self-refs), so a non-conformant table (zero or several self-refs, or an empty list) is rejected. Both the non-fragmented and fragmented init paths honour the table; it round-trips through `parse_dref` onto `Track::data_references`. `DataReferenceWrite` is public on the crate root via `muxer`

- Round 372 — MovMuxer write-side **Track Aperture Modes box** (`tapt`, Apple "Movie Atoms"), the inverse of the demuxer's existing `parse_tapt`. `MovMuxer::set_track_aperture(track_id, Tapt)` attaches up to three aperture rectangles — `clef` (Clean Aperture), `prof` (Production Aperture), `enof` (Encoded Pixels) — to a previously-added **video** track, each emitted as a 12-byte `[ver+flags][width_fp][height_fp]` child sub-atom only when present. New `TaptDims::from_pixels(width, height)` builds a rectangle from integer dimensions (the conventional 16.16-fixed-point with zero fraction) and `TaptDims::to_body_bytes()` serialises the sub-atom body (exact inverse of `parse_tapt_dims`); both round-trip non-integer fixed-point values bit-exact. Rejects an unknown `track_id`, a non-video track (aperture modes apply only to a visual sample entry), and an all-`None` `Tapt`. A file written this way round-trips through `parse_tapt` onto `Track::tapt`. `TaptDims` is now public on the crate root via `media_meta`

- Round 372 — MovMuxer write-side **Track Reference Box** (`tref`, QTFF p. 50 / ISO/IEC 14496-12 §8.3.3), closing the demux↔mux symmetry gap on track-to-track relationships the demuxer already read (`Track::references`, `chapter_track_ref`, `timecode_track_ref`, `MovDemuxer::tref_track_indices`) but the muxer could never write. `MovMuxer::set_track_references(track_id, &[TrackReference])` attaches one child atom per [`TrackReference`] to a previously-added track, emitted as a `tref` between the track's `tkhd`/`edts` and its `mdia` (QTFF Figure 2-3 ordering). Each `TrackReference { reference_type: [u8;4], track_ids: Vec<u32> }` becomes one Track Reference Type Box whose FourCC names the relationship (`chap` chapter list, `tmcd` time-code track, `sync`, `scpt`, `hint`, `cdsc`, …) and whose body is the tightly-packed list of big-endian `u32` referenced track ids — the exact inverse of the read-side `parse_tref`. Convenience constructors `TrackReference::to(type, id)` / `::chapter(id)` / `::timecode(id)` cover the common single-target cases. Every referenced id is validated against the set of tracks added so far (a reference to id `0` or an out-of-range id is rejected; self-references are permitted since some reference types legitimately point at the declaring track), so the chapter / time-code track must be added before declaring the reference. Replaces any references previously attached to the same track; an empty list emits no `tref`. A file written this way round-trips through `parse_tref` onto `Track::references` and resolves through the typed accessors (`chapter_track_ref` / `timecode_track_ref` / `timecode_track_index`). Honoured on the non-fragmented write path; the fragmented init `moov` ignores it. `TrackReference` is public on the crate root via `muxer`

- Round 368 — MovMuxer write-side **visual sample-description extension boxes** (`pasp` / `colr` / `clap` / `fiel` / `gama`, ISO/IEC 14496-12 §12.1.4 / §12.1.5, QTFF p. 94), closing the demux↔mux symmetry gap on the video-`stsd` decoration boxes the demuxer already read but the muxer could never write. `MovMuxer::set_visual_extensions(track_id, VisualExtensions)` attaches a typed set to a previously-added **video** track; each populated `VisualExtensions` field emits its box into the video sample entry's trailing slot — after the 70-byte fixed body and after the codec-config `extra_stsd_atoms` passed to `add_track`, so a decoder-config box (`avcC` / `hvcC`) stays first by convention. Boxes are emitted in a stable canonical order (`colr`, `pasp`, `clap`, `fiel`, `gama`); box order inside a sample entry is not significant to a conformant reader. The typed fields are `Pasp` (Pixel Aspect Ratio §12.1.4.2), `ColorParameters` (`colr` — Apple `nclc` / ISO `nclx` with the §12.1.5 `full_range_flag` top bit / ICC `rICC`/`prof` / `Other`), `Clap` (Clean Aperture §12.1.4, signed `horiz_off_n` / `vert_off_n`), `Fiel` (Field Handling, QTFF p. 94), and a 16.16 fixed-point `gamma` `u32` (the same representation the demuxer surfaces on `SampleDescription::gamma`). New `Pasp::to_body_bytes` / `Clap::to_body_bytes` / `ColorParameters::to_body_bytes` / `Fiel::to_body_bytes` are the exact inverses of the existing `parse_pasp` / `parse_clap` / `parse_colr` / `parse_fiel` decoders (round-trip-tested per variant). A file written this way round-trips through the demuxer's `scan_video_extensions` onto `SampleDescription`'s `pasp` / `colr` / `clap` / `fiel` / `gamma` fields. `set_visual_extensions` rejects an unknown `track_id` and rejects a non-video track (visual extensions are only defined for a visual sample entry); an empty `VisualExtensions` emits nothing. `VisualExtensions` is public on the crate root via `muxer`

- Round 364 — MovMuxer write-side **sample-group description + classic carrier + Composition to Decode** boxes, closing three demux↔mux symmetry gaps. (1) `set_sample_group_description(track_id, SampleGroupDescriptionWrite)` emits the sibling `sgpd` (SampleGroupDescriptionBox, ISO/IEC 14496-12 §8.9.3) whose typed entries a `csgp`/`sbgp`'s per-sample `group_description_index` values reference — previously the muxer wrote the index mapping but never the descriptions, leaving a non-conformant file when a non-zero index was used. Written version 1 (constant `default_length` when entries are uniform, else a per-entry `description_length` prefix per §8.9.3.2 — version 0 is deprecated), inside `stbl` before the `sbgp`/`csgp` boxes (§8.9.3 containment order); one `sgpd` per `grouping_type` with replace-by-type. Typed §10 entry constructors `SampleGroupDescriptionWrite::{roll_entry, prol_entry, rap_entry, tele_entry, sap_entry}` mirror the read-side `decode_roll`/`decode_prol`/`decode_rap`/`decode_tele`/`decode_sap`; round-trips through `parse_sgpd`. (2) `add_sample_to_group_with_form(track_id, SampleToGroupWrite, SampleGroupBoxForm::Classic)` emits the widely-compatible run-length `sbgp` (SampleToGroupBox, §8.9.2 — version 1 when a `grouping_type_parameter` is supplied, else version 0) instead of the compact `csgp` (a 2020 addition some older readers don't parse); `add_sample_to_group` stays the `csgp`-emitting default. Both carry the identical mapping and round-trip (`sbgp` via `parse_sbgp`, `csgp` via `parse_csgp`). (3) `auto_cslg(track_id)` / `set_cslg(track_id, Cslg)` emit the Composition to Decode Box (`cslg`, §8.6.1.4) right after `ctts`: `auto_cslg` derives the five bounds — `compositionToDTSShift = max(0, -least)`, least/greatest `composition_offset`, and composition start/end time (`min CT` / `max (CT + duration)` over `CT_i = DTS_i + offset_i`) — from the track's per-sample offsets + durations; both auto-promote from version 0 (`int(32)`) to version 1 (`int(64)`) when a field leaves the signed-32-bit range. The derived bounds satisfy the demuxer's `cslg`/`ctts` cross-validation; round-trips through `parse_cslg`. `SampleGroupDescriptionWrite` / `SampleGroupBoxForm` and the previously-internal `decode_tele` / `decode_sap` / `TemporalLevel` / `StreamAccessPoint` / `parse_cslg` are now public on the crate root

- Round 360 — ISO BMFF **SimpleTextSampleEntry** (`stxt`, ISO/IEC 14496-12 §12.5.3) read path. A timed-text track (`hdlr` component subtype `text`, §12.5.1, null media header `nmhd`) whose `stsd` entry FourCC is `stxt` now populates the new `SampleDescription::simple_text` with a typed `SimpleTextSampleEntry { content_encoding, mime_format, text_config, bitrate }` — an optional `content_encoding` MIME string then a mandatory `mime_format`, followed by the optional `btrt` BitRateBox (§8.5.2.2 → `BitRate`) and `txtC` TextConfigBox (`text_config`), reusing the same NUL-string + box-walk decoding as the `mett` timed-metadata entry. Selected by the `stxt` FourCC, so it coexists on the same `text` handler with the QuickTime `text` description (an `stxt` entry leaves `SampleDescription::text` `None` and vice-versa). `parse_stxt` and the `SimpleTextSampleEntry` type are public on the crate root via `metadata_sample`

- Round 360 — QuickTime **Text Sample Description** read path (`text` format inside `stsd` on a classic `text`-handler track, QTFF pp. 108–110), via the new `text_sample` module. A track whose `hdlr` component subtype is `text` (`Hdlr::is_text()`) and whose `stsd` entry FourCC is `text` now populates `SampleDescription::text` with a typed `TextSampleDescription`: `display_flags` (the documented bits — don't-auto-scale `0x0002`, use-movie-bg-color `0x0008`, scroll-in/out/horizontal/reverse/continuous `0x0020`–`0x0200`, drop-shadow `0x1000`, anti-alias `0x2000`, key-text `0x4000` — exposed via `use_movie_background()` / `dont_auto_scale()` / `has_drop_shadow()` / `anti_aliased()` / `is_key_text()` / `is_scrolling()` accessors and `TEXT_FLAG_*` consts), `text_justification` (`TextJustification::Left` `0` / `Center` `1` / `Right` `-1` / `Other`), the 48-bit RGB `background_color` / `foreground_color` (`Rgb48 { red, green, blue }`), the `default_text_box` QuickDraw rectangle (`TextBox { top, left, bottom, right }`), `font_number`, the `font_face` style bitmask (`TEXT_FACE_BOLD`/`ITALIC`/`UNDERLINE`/`OUTLINE`/`SHADOW`/`CONDENSE`/`EXTEND`, with `is_bold()` / `is_italic()` / `is_underline()` accessors), and the trailing Pascal `text_name` font name (optional; conservative UTF-8 / Mac-Roman fallback decode). This is the *description* side of QuickTime text media — distinct from the per-sample `[length:u16][text][extensions]` payload decoded by `chapter::parse_text_sample_styles` and from the `gmhd/text` media-information header (`gmhd::TextHeader`). `parse_text_sample_description` and `TEXT_SAMPLE_DESC_FIXED_LEN` (43) are public on the crate root; non-`text` handlers leave the field `None`, and the raw body is preserved on `SampleDescription::extra` for round-trip / future style-extension readers

- Round 357 — typed ISO BMFF `tref` reference types. `TrackRefKind` now classifies the §8.3.3.3 reference types previously lumped into `Other`: `cdsc` (`ContentDescribes` — a descriptive / metadata track links to the content it describes, e.g. a timed-metadata track per §12.3.2 or an RTCP reception hint track), `font` (`Font`), `hind` (`HintDependency` — depends on a referenced hint track), `vdep` / `vplx` (`VideoDepth` / `VideoParallax` — auxiliary depth / parallax video), and `subt` (`Subtitle`). Matching accessors `content_describes_track_refs` / `font_track_refs` / `hint_dependency_track_refs` / `video_depth_track_refs` / `video_parallax_track_refs` / `subtitle_track_refs` mirror the existing `sync_track_refs` / `hint_track_refs` / … (0-valued slots filtered, declaration order preserved). The pre-existing QuickTime kinds (`chap` / `tmcd` / `scpt` / `ssrc` / `sync` / `hint` / `mpod`) are unchanged

- Round 357 — media-header-box classification + Extended Language Tag. Each track now records *which* media-header box its `minf` carried (ISO/IEC 14496-12 §8.4.5.1, "Exactly one specific media header shall be present") on the new `Track::media_header_kind` (`MediaHeaderKind`): `Video` (`vmhd`), `Sound` (`smhd`), `Hint` (`hmhd`), `Subtitle` (`sthd`, §12.6.2), `Null` (`nmhd`, §8.4.5.2), `Generic` (`gmhd`), or `None`. The two previously-unrecognised empty-FullBox variants — `sthd` (Subtitle Media Header) and `nmhd` (Null Media Header, used by timed-metadata tracks per §12.3.2) — are now walked and classified (they carry no payload fields beyond version+flags, so only their presence is signalled). Separately, the optional `elng` Extended Language Tag Box (§8.4.6) — an RFC 4646 / BCP 47 tag such as `"en-US"` that overrides the packed `mdhd.language` code when the two disagree — is parsed from its `mdia` slot onto `Track::extended_language` (`None` when absent). `parse_elng` and the `MediaHeaderKind` enum are public on the crate root

- Round 351 — typed `sgpd` sample-group description entries for the remaining standardized §10 grouping types (ISO/IEC 14496-12:2015): `'tele'` TemporalLevelEntry (§10.5.2 — `level_independently_decodable`; the temporal level *equals* the `sgpd` group-description index), `'sap '` SAPEntry (§10.6.2 — `dependent_flag` + 4-bit `SAP_type`), `'rash'` RateShareEntry (§10.2.2.2 — single- and multi-operation-point target rate shares, `maximum_bitrate` / `minimum_bitrate` / `discard_priority` trailer), and `'alst'` AlternativeStartupEntry (§10.3.2 — `roll_count` / `first_output_sample`, a `sample_offset` table, and the optional trailing `(num_output_samples, num_total_samples)` output-rate pieces). `decode_tele` / `decode_sap` / `decode_rash` / `decode_alst` plus the `TemporalLevel` / `StreamAccessPoint` / `RateShare` (`RateShareOperationPoint`) / `AlternativeStartup` (`AlternativeStartupPiece`) types are public on `sample_groups`. The fixed-width `'tele'` / `'sap '` join the deprecated-v0 implicit-size catalogue (1 byte each); the variable-length `'rash'` / `'alst'` require sgpd version-1+ on-disk sizing. New per-sample demuxer lookups `temporal_level_for` (returns `(level, independently_decodable)`), `stream_access_point_for`, `rate_share_for`, and `alternative_startup_for` mirror the existing `roll_distance_for` / `visual_random_access_for` resolution through the matching `sbgp` run. Truncation / misalignment / zero-operation-point bodies are rejected

- Round 347 — MovMuxer write-side **Apple QuickTime Metadata** (`moov/meta` = `hdlr` `mdta` + `keys` + `ilst`) emission: `set_apple_metadata(&[MovMetaItem])` attaches movie-level key-value metadata in the modern QuickTime Metadata shape (distinct from the legacy `udta` User Data Box driven by `set_metadata`). Each `MovMetaItem` becomes one `keys` declaration (`[size][namespace][key]`, namespace defaulting to the new `META_NAMESPACE_MDTA`) paired with one `ilst` entry whose `data` sub-atom carries the typed value — `MovMetaItem::utf8` (`META_TYPE_UTF8` = 1), `MovMetaItem::signed_int` (`META_TYPE_BE_SIGNED_INT` = 21, 32-bit BE `i32`), or `MovMetaItem::typed(namespace, key, type_code, bytes)` for an explicit namespace / well-known-type indicator / raw value (`META_TYPE_BE_UNSIGNED_INT` = 22, `META_TYPE_RAW` = 0 also exported). The 1-based `ilst` key-index references the parallel `keys` row, mirroring the read-side `parse_keys` / `parse_ilst`; duplicate keys each get their own slot in declaration order. A file written this way round-trips through the read side and surfaces on `MovDemuxer::meta` as `MetaKeyValue` (namespace / key / type-code / value preserved). The `meta` box is emitted after `moov/udta` when both are present and omitted when no Apple metadata is attached; the fragmented init `moov` ignores it. Accessors `MovMetaItem::namespace` / `key` / `type_code` / `value` exposed. `set_track_apple_metadata(track_id, &[MovMetaItem])` does the same at track scope (`trak/meta`, a trailing `trak` child after any `trak/udta`), round-tripping onto `Track::meta`; movie- and track-scope Apple metadata are independent and an unknown `track_id` is rejected
- Round 344 — ISO BMFF subtitle sample entries (`stpp` / `sbtt`, ISO/IEC 14496-12 §12.6.3) parsed at the demuxer surface and surfaced on the new `SampleDescription::subtitle` field for subtitle-handler tracks (`Hdlr::is_subtitle()`, `subt` subtype). `Xml` (XMLSubtitleSampleEntry — `namespace` / `schema_location` / `auxiliary_mime_types`) and `Text` (TextSubtitleSampleEntry — `content_encoding` / `mime_format` + `txtC` `text_config`) reuse the same `BitRate` (`btrt`) and `TextConfigBox` decoding as the metadata entries; `MetadataSampleEntry`-style `bitrate()` accessor included. `parse_stpp` / `parse_sbtt` / `parse_subtitle_sample_entry` are public. Non-subtitle handlers leave the field `None`
- Round 344 — Hint Media Header Box (`hmhd`, ISO/IEC 14496-12 §12.4.2.2) parsed into the typed `Hmhd { max_pdu_size, avg_pdu_size, max_bitrate, avg_bitrate }` and surfaced on `Track::hmhd` for hint tracks (`Hdlr::is_hint()`, `hint` component subtype). `parse_hmhd` is public; non-hint tracks leave the field `None`
- Round 344 — ISO BMFF timed-metadata sample entries (`metx` / `mett` / `urim`, ISO/IEC 14496-12 §12.3.3) parsed at the demuxer surface via the new `metadata_sample` module. A track whose `hdlr` component subtype is `meta` (`Hdlr::is_metadata()`) and whose `stsd` entry FourCC names one of the three `MetaDataSampleEntry` subclasses now populates `SampleDescription::metadata` with a typed `MetadataSampleEntry`: `Xml` (XMLMetaDataSampleEntry — `content_encoding` / `namespace` / `schema_location` NUL-terminated strings), `Text` (TextMetaDataSampleEntry — `content_encoding` / `mime_format` plus the optional `txtC` TextConfigBox `text_config`), and `Uri` (URIMetaSampleEntry — the `uri ` URIBox string plus the optional `uriI` URIInitBox opaque data). The optional `btrt` BitRateBox (§8.5.2.2) common to all three is decoded into `BitRate { buffer_size_db, max_bitrate, avg_bitrate }` and surfaced through `MetadataSampleEntry::bitrate()`. `parse_btrt` / `parse_metx` / `parse_mett` / `parse_urim` / `parse_metadata_sample_entry` are public; `Hdlr::is_hint()` (`hint` subtype, §12.4.1) is also added. Non-`meta` handlers leave the field `None`, so the existing video / audio / timecode parse paths are unaffected
- Round 334 — MovMuxer write-side user-data metadata (`udta`) emission at movie (`moov/udta`) and track (`trak/udta`) scope (QTFF pp. 36–38 / ISO/IEC 14496-12 §8.10.1): `set_metadata(&[MovMetadata])` and `set_track_metadata(track_id, &[MovMetadata])` attach User Data Box entries. `MovMetadata::intl_text(fourcc, language, text)` writes an Apple international-text record (`©XXX`) — multiple items sharing a FourCC coalesce into a single atom carrying one `[text_size:u16][language:u16][text]` record per language (QTFF p. 38), with `UTF8_INTL_TEXT_FLAG | MovMetadata::iso_language(tag)` selecting a UTF-8 body and a Mac language code selecting Mac-Roman. `MovMetadata::plain_utf8` covers the QuickTime-7+ `name`/`auth`/`cprt` FullBox+lang+UTF-8 shape; `MovMetadata::raw` writes an opaque FourCC body. All round-trip through the read side's `parse_udta` and surface on `MovDemuxer::user_data` (movie scope) / `Track::user_data` (track scope). No `udta` is written when a scope carries no metadata; `set_track_metadata` rejects an unknown track id
- Round 329 — MovMuxer write-side edit list (`edts > elst`) emission (QTFF p. 47 / ISO/IEC 14496-12 §8.6.6): `set_edit_list(track_id, &[MuxEdit])` attaches a per-track edit list emitted between `tkhd` and `mdia`; `MuxEdit::segment(track_duration, media_time)` / `MuxEdit::empty(track_duration)` cover the unity-rate-segment and empty-edit (encoder-priming-skip / start-delay) shapes. The `elst` auto-versions to v0 (32-bit) or v1 (64-bit, when any `track_duration > u32::MAX` or `media_time` falls outside `i32`); `media_time = -1` is the only legal negative value (the empty-edit sentinel) and any other negative is rejected. Round-trips through MovDemuxer's `parse_elst` with per-segment `track_duration` / `media_time` / `media_rate` preserved
- Round 325 — Sound Sample Description version-1 read path (QTFF p. 101 `SoundDescriptionV1`): `SampleDescription` now surfaces `audio_version`, `audio_compression_id`, and the typed `SoundV1` fixed-ratio fields (`samples_per_packet` / `bytes_per_packet` / `bytes_per_frame` / `bytes_per_sample`); `is_vbr()` decodes the variable-bit-rate "third variant" (version 1, Compression ID `-2`, QTFF p. 102). Short / truncated v1 bodies fall back to the version-0 extra-scan start without over-reading
- Round 319 — MovMuxer write-side `csgp` (CompactSampleToGroupBox) emission at `stbl` scope (ISO/IEC 14496-12:2020 §8.9.5): `add_sample_to_group(track_id, SampleToGroupWrite { grouping_type, grouping_type_parameter, indices })` attaches a per-sample group-description-index mapping; the muxer run-length-encodes it into the compact pattern form (one `pattern_length == 1` pattern per run) with minimum width selectors, and the existing `parse_csgp` read path expands it back to the exact per-sample assignment
- Round 315 — MovMuxer write-side `ctts` composition-offset emission (ISO/IEC 14496-12 §8.6.1.3): `MuxSample.composition_offset` (PTS − DTS) round-trips B-frame reorder through MovDemuxer; box omitted when all-zero, v0 when all-non-negative, v1 (signed) when any offset is negative

## [0.0.4](https://github.com/OxideAV/oxideav-mov/compare/v0.0.3...v0.0.4) - 2026-06-15

### Other

- Round 310 — MovMuxer write-side compressed-movie-resource (cmov) emission
- round 307 — MovMuxer write-side saiz/saio at traf (fragmented) scope
- mov r300: MovMuxer write-side saiz/saio at stbl scope (§8.7.8/§8.7.9)
- Round 293 — Sub Track box family (strk > stri + strd > stsg)
- csgp (Compact Sample to Group Box) — ISO/IEC 14496-12:2020 §8.9.5
- Round 283 — compressed movie resources decompressed and re-parsed end-to-end
- round 279: fix fuzz OOM — bound count-driven pre-allocations reachable from open
- round 279: parse mjht default Motion-JPEG Huffman table extension (QTFF p. 94 Table 3-2)
- Round 267 — default Motion-JPEG quantization table (mjqt) extension
- Round 264 — Field Handling (fiel) extension + typed gamma accessor
- Round 259 — Compressed Movie atom (cmov / dcom / cmvd) parsers
- Round 256 — typed chunk-walking primitive over stsc / stco / stsz
- drop release-plz.toml — use release-plz defaults across the workspace
- Round 246 — inverse edit-list mapper movie_pts → media_pts
- Round 243 — typed tref accessors for QTFF Table 2-2 reference kinds
- Round 240 — typed Gmin GraphicsMode / Balance accessors (QTFF Ch. 4)
- Round 234 — Padding Bits Box (padb) parser at stbl scope (ISO/IEC 14496-12 §8.7.6)
- Round 226 — Level Assignment Box (leva) parser at moov/mvex scope (ISO/IEC 14496-12 §8.8.13)
- Round 219 — Subsegment Index Box (ssix) parser, ISO/IEC 14496-12 §8.16.4
- Round 216 — Track Input Map atom (imap) parser (QTFF pp. 51-53)
- Round 210 — Degradation Priority Box (stdp) parser at stbl scope
- round 204: parse Compact Sample Size Box (stz2)

### Other

- Round 310 — `MovMuxer` write-side compressed-movie-resource emission (QTFF, 2001-03-01, pp. 80 – 81, "Allowing QuickTime to Compress the Movie Resource" / Table 2-5), closing the third README follow-up. Round 283 landed the full `cmov` read path plus the `compress_movie_resource` / `Cmov::to_body_bytes` building blocks, but the muxer never elected to compress the movie resource it wrote. New opt-in builder `MovMuxer::with_compressed_movie_resource(bool)` (default off) plus the `MovMuxer::compresses_movie_resource()` accessor: when enabled, the non-fragmented write path (`encode_to_vec` / `write_to`) still lays `ftyp` + `mdat` down first — so the `stco` / `co64` chunk offsets stay file-absolute and `mdat`-anchored exactly as in the uncompressed layout — but the trailing plain `moov` is replaced by a `moov` whose single child is a `cmov` carrying the zlib-deflated (`dcom = 'zlib'`, RFC 1950 via the `compcol` crate) movie resource plus its 32-bit uncompressed size in `cmvd`. Per QTFF p. 30 the complete movie resource is the full `moov` atom (its 8-byte header included), so the muxer compresses exactly that serialized atom; the output decompresses back to a byte-identical plain-`moov` file and round-trips through this crate's own `cmov` read path (the demuxer transparently decompresses on open, surfacing `MovDemuxer::compressed_movie_algorithm = Some(*b\"zlib\")`). Has no effect on the fragmented path (`encode_fragmented_to_vec`) — QTFF p. 81 describes movie-resource compression for the flatten-time movie atom, not per-fragment `moof` boxes. Pinned by 4 new unit tests in `src/muxer.rs` (flag default-off + builder set; the emitted `moov > cmov > dcom + cmvd` tree parses through `parse_cmov` with the `'zlib'` algorithm and a decompressed resource that is itself a complete `moov` atom whose length equals the `cmvd` size word; the decompressed resource is byte-identical to the plain-path `moov` atom; the plain path emits no `cmov`) plus 5 end-to-end integration tests in `tests/synth_round310_muxer_cmov.rs` (plain and compressed builds of the same movie open to identical mvhd / track / packet state through `MovDemuxer`, with the algorithm FourCC surfaced on one and `None` on the other; the compressed output is smaller than the plain output; the compressed output carries the `cmov` / `dcom` / `cmvd` tree; the plain output carries no `cmov`; the flag defaults off). No new public type — `with_compressed_movie_resource` / `compresses_movie_resource` extend the existing `MovMuxer` builder surface. Both the `registry` and `--no-default-features` standalone configurations build, fmt-check, and clippy clean. Test count 1038 → 1047.
- Round 307 — `MovMuxer` write-side `saiz` / `saio` emission at `traf` (fragmented) scope (ISO/IEC 14496-12 §8.7.8 / §8.7.9 / §8.8.14), closing the first README follow-up. Round 300 landed the `stbl`-scope (non-fragmented) write path via `MovMuxer::set_sample_aux`, but the fragmented write path (`encode_fragmented_to_vec` / `write_to_fragmented`) ignored the attached `SampleAuxStream`. This round emits the `traf`-scope form so ISO/IEC 23001-7 Common Encryption per-sample records (or any other writer-defined per-sample side channel) round-trip through a fragmented / CMAF / DASH-live output too. For each fragment, the per-sample auxiliary-information blobs corresponding to that fragment's samples are laid into the fragment's `mdat` *after* every track's sample data (contiguous per `traf` in track order), and the matching `traf` carries a `saiz` describing the per-sample sizes plus a single-entry `saio` whose offset is **relative to the enclosing `moof`** — per §8.8.14 the `traf`-scope `saio` offset is relative to the track-fragment base offset, and the muxer always sets `default-base-is-moof` on the `tfhd`, so that base is the `moof`'s first byte. `saiz` reuses the §8.7.8.2 uniform-`default_sample_info_size` vs per-sample-table selection from round 300 (now factored into a shared `build_saiz_blobs` / `build_saio_offset` core consumed by both scopes); the `(aux_info_type, aux_info_type_parameter)` discriminator (gated by `flags & 1`) rides on both boxes. No new public surface — `set_sample_aux` (added round 300) now feeds both write paths; its rejections (blob count vs sample count, >255-byte blob, unknown track id) are unchanged. Pinned by 6 new integration tests in `tests/synth_round307_fragmented_muxer_sample_aux.rs` (uniform-blob default-size form across two fragments with moof-relative byte verification; varying-blob per-sample-table form with discriminator round-trip; implicit-discriminator zero-pair match; no-stream-emits-no-traf-boxes; aux slab does not corrupt the per-fragment `trun.data_offset` sample reads; degenerate single-fragment slice). All round-trip through `MovDemuxer::fragment_sample_aux_info` (round 150 read path). Both the `registry` and `--no-default-features` standalone configurations build, fmt-check, and clippy clean. Test count 1032 → 1038.
- Round 300 — `MovMuxer` write-side `saiz` / `saio` emission at `stbl` scope (ISO/IEC 14496-12 §8.7.8 / §8.7.9), closing the first README follow-up. The round-147 read path consumed Sample Auxiliary Information boxes but the encoder never wrote them; producers that wanted to round-trip e.g. ISO/IEC 23001-7 Common Encryption per-sample records had to hand-author the boxes. New public `SampleAuxStream` struct (`aux_info_type: Option<[u8; 4]>`, `aux_info_type_parameter: u32`, `per_sample: Vec<Vec<u8>>`) plus `MovMuxer::set_sample_aux(track_id, stream) -> Result<()>` attaches an opaque per-sample auxiliary-information stream to a previously-added track. On the next non-fragmented `encode_to_vec` / `write_to`, each sample's blob is laid into `mdat` contiguously immediately after the track's sample data, a `saiz` describes the per-sample sizes (the §8.7.8.2 uniform `default_sample_info_size` form when every blob is the same non-zero length, the per-sample table otherwise; an all-empty stream emits an explicit zero table rather than the `default==0` "table follows" sentinel), and a single-entry `saio` (§8.7.9.3 — one offset for a contiguous slab) carries the *absolute* file offset of the first blob, auto-selecting v1 (64-bit) when the offset exceeds the u32 range. The matching `(aux_info_type, aux_info_type_parameter)` discriminator (gated by `flags & 1`) rides on both boxes so the pair resolves together on read; an absent discriminator emits `flags & 1 == 0` and matches the §8.7.8.1 implicit fallback (the all-zero pair). `set_sample_aux` rejects a blob count that disagrees with the track's sample count, a blob longer than 255 bytes (the §8.7.8.2 size table is u8-wide — reject rather than silently truncate), and an unknown track id. The fragmented write path ignores the stream (a future round can emit the `traf`-scope form). Pinned by 9 new unit tests in `src/muxer.rs` (default-size vs per-sample-table selection, all-empty-blobs zeros, v0/v1 `saio` selection with discriminator round-trip, and the three `set_sample_aux` rejections) plus 5 end-to-end integration tests in `tests/synth_round300_muxer_sample_aux.rs` (uniform + varying blobs round-trip through `MovDemuxer::sample_aux_info` with byte-exact slab verification at the `saio` offset, implicit-discriminator zero-pair match, no-stream-emits-no-boxes, and confirmation the aux slab does not corrupt the track's sample-data reads). New public surface: `SampleAuxStream`, `MovMuxer::set_sample_aux`.
- Round 290 — Compact Sample to Group Box (`csgp`) parsed and expanded into the existing sample-group model (ISO/IEC 14496-12:2020 §8.9.5; staged via `docs/container/isobmff/post-2015-additions.md`, the MPEG Working Group's machine-readable box catalogue, since the 2020/2022 ISO PDFs are paywalled). `csgp` is the dense post-2015 form of `sbgp`: it overloads the 24-bit `FullBox.flags` field into three 2-bit width selectors — `index_size_code` (`flags[0..1]`), `count_size_code` (`flags[2..3]`), `pattern_size_code` (`flags[4..5]`), each mapping a code to `4 << code` bits {0→4, 1→8, 2→16, 3→32} — plus a `grouping_type_parameter_present` bit (`flags[6]`); the body then carries `pattern_count` bit-packed `(pattern_length[i], sample_count[i])` pairs followed, for each pattern, by `pattern_length[i]` description indices. New `parse_csgp` decodes the variable-width MSB-first stream and expands each pattern across its `sample_count` samples (cycling the pattern indices sample-by-sample, RLE-coalescing equal neighbours so the entry vector stays bounded by the on-disk index count rather than the expanded sample total), producing a `SampleToGroup` byte-for-byte interchangeable with a v0/v1 `sbgp`. The demuxer wires `csgp` into the `stbl` walk alongside `sbgp` under the same "at most one per (grouping_type, grouping_type_parameter)" de-dup policy, so every existing per-sample typed lookup (`roll_distance_for`, `audio_preroll_for`, `visual_random_access_for`, `random_access_points`) resolves a `csgp`-authored track unchanged. The fragment-local-vs-global description-index msb is preserved verbatim through expansion and exposed via the new `split_csgp_index` helper + `CSGP_FRAGMENT_LOCAL_BIT` mask (new public surface: `parse_csgp`, `split_csgp_index`, `CsgpIndex`, `CSGP_FRAGMENT_LOCAL_BIT`, `CSGP` FourCC). Bounds: a zero `pattern_length` is rejected (would make the pattern-replay modulo undefined), and the bit reader returns a parse error rather than reading past the body on a truncated pattern or index table. Pinned by 12 new unit tests in `src/sample_groups.rs` (width-code mapping, single/multi-pattern expansion, RLE coalescing, grouping_type_parameter presence, 32-bit-wide indices, fragment-local msb split + preservation, zero-pattern-length / truncated-header / truncated-index-table rejection, bit-reader accounting) plus 1 end-to-end integration test in `tests/synth_round80_sample_groups.rs` (a `csgp`-authored audio track resolves the correct per-sample pre-roll distance through `MovDemuxer`). Test count 1019 → 1032.
- Round 283 — compressed movie resources decompressed and re-parsed end-to-end (QTFF, 2001-03-01, "Compressed Movie Resources" pp. 80 – 81 / Table 2-5; p. 30: "the movie atom contains only a single child atom—the compressed movie atom ('cmov'). When this child atom is uncompressed, its contents conform to the structure shown in the following illustration" — the standard uncompressed movie resource, a complete `moov` atom). The round-259 `cmov` / `dcom` / `cmvd` structural parsers now feed an actual decompression step: new `Cmov::decompress() -> Result<Vec<u8>>` inflates the `cmvd` payload through the workspace's `compcol` crate (RFC 1950 zlib, the conventional `dcom` FourCC; the spec names the field generically, so any other algorithm value returns `Error::invalid` carrying the verbatim bytes, letting a caller with its own decompressor drive `Cmvd::compressed_data` directly). Decompression is bounded against bombs on two layers: the decoder refuses to produce more than the declared 32-bit `cmvd` uncompressed size (QTFF p. 81 — "The first 32-bit integer in the compressed movie data atom indicates the uncompressed size of the movie resource" — makes the word authoritative, and a decoded length that does not equal it rejects as a writer error), and the demuxer-side wiring rejects any declared size over the crate-wide 64 MiB `MAX_INMEMORY_ATOM_BODY` cap before the decoder allocates toward it. The `moov` walker (`parse_moov`) recognises a `cmov` child, decompresses it, validates the decompressed buffer is a complete `moov` atom (header included, with an inner injection-robustness check mirroring the round-162 top-level guard: a forged inner size word declaring a body past the end of the buffer rejects), and re-enters the same walk over an in-memory cursor — every downstream field (tracks, sample tables, Apple/BMFF meta, udta, mvex/mehd/trex/leva, ctab, clip, rmra) populates exactly as for an uncompressed file, and chunk offsets in the decompressed `moov` address the host file's `mdat` unchanged (the writer compressed the movie resource of the final file, so `stco`/`co64` values are file-absolute). A second compression layer inside the decompressed resource is rejected with a spec-citing error (p. 30 describes the decompressed contents as the standard *uncompressed* structure), closing the recursion-bomb shape. New public surface: `MovDemuxer::compressed_movie_algorithm: Option<[u8; 4]>` records the `dcom` FourCC when the input stored its movie resource compressed (`None` for the common uncompressed layout — every other demuxer field reflects the decompressed resource); `MovDemuxer::probe_reference_movies` sees through the compression layer so a compressed reference movie still resolves its `rmra` alias list; writer-side counterparts `compress(movie_resource) -> Result<Cmov>` (re-exported as `compress_movie_resource`; rejects inputs over the 32-bit size field's range) and `Cmov::to_body_bytes()` (serializes the Table 2-5 `dcom` + `cmvd` children for wrapping in `cmov` / `moov` headers) provide round-trip fidelity for tests and a future muxer flatten-time option. `compcol` lands as a dependency with `default-features = false, features = ["zlib"]` in both the registry and standalone build paths. Pinned by 6 new unit tests in `src/cmov.rs` (compress→decompress round-trip; `to_body_bytes`→`parse_cmov` round-trip; non-zlib algorithm rejection; under-declared size hits the output cap without unbounded growth; over-declared size rejects as a length mismatch; corrupt non-RFC-1950 stream rejects cleanly) plus 8 end-to-end integration tests in `tests/synth_round283_cmov.rs` (a compressed movie opens and yields the identical packet to its uncompressed twin; the two layouts agree on mvhd/track/packet state with the algorithm FourCC surfaced on one and `None` on the other; non-zlib `dcom`, declared-size mismatch, nested `cmov`, non-`moov` decompressed resource, `u32::MAX` oversize declaration (rejected on arithmetic alone), and truncated compressed stream all fail the open cleanly). The daily fuzz harness reaches the decompression path automatically through `MovDemuxer::open`. Test count 1005 → 1019.
- Round 279 (fuzz fix) — closed scheduled-fuzz finding `oom-33f049eec4ac8b768b06765a75ed350bfeb5a331`: a `keys` atom declaring `entry_count = 0x0a0a0a0a` (≈168 M) drove `parse_keys`'s `Vec::with_capacity(entry_count)` into a single ~5.4 GB allocation (count × the 32-byte in-memory key tuple) before the per-entry loop could reject the truncated table — libFuzzer's malloc limit killed the process. Fix front-loads the byte-bound check already used by the `ssix` / `leva` parsers: a declared count whose minimum on-disk footprint (8 bytes/entry) exceeds the remaining body bytes is rejected on arithmetic alone, before any count-sized allocation. The same audit swept every `Vec::with_capacity(on-disk count)` site reachable from `MovDemuxer::open` and closed the four siblings with the same shape: `parse_chan` (caps the pre-allocation at the byte-backed entry count, preserving its lenient cap-and-stop parse), `parse_dref` (caps at `body/12` — its loop legitimately tolerates over-declared counts, so reject would change semantics), `parse_stsd` (caps at `body/16`, the QTFF p. 70 universal-header minimum), and `parse_sgpd` (caps the pre-allocation AND rejects the deprecated-v0 zero-implicit-size fallback when `remainder / entry_count` rounds to zero — that path previously pushed `entry_count` zero-length entries, an unbounded `Vec` growth the capacity cap alone would not stop). All other count-driven sites already validated the table's byte length before allocating (the `stts`/`stsc`/`stsz`/`stz2`/`stco`/`co64`/`ctts`/`stss`/`stsh`/`stdp`/`padb`/`subs`/`sbgp`/`saio`/`sidx`/`elst`/`trun`/`tfra`/`leva`/`ssix` family) or bound the count by a narrower type. Pinned by `tests/synth_round279_count_prealloc_oom.rs` (5 tests): the verbatim 82-byte reproducer through `MovDemuxer::open`, the `parse_keys` arithmetic rejection + exact-fit boundary acceptance, and end-to-end `sgpd`-v0 / `stsd` over-declared-count sweeps. Test count 1000 → 1005.
- Round 279 — default Motion-JPEG Huffman table (`mjht`) video sample-description extension parser, completing the QTFF p. 94 Table 3-2 "Video sample description extensions" chain (`gama` round 2 / round 264, `fiel` round 264, `mjqt` round 267). Table 3-2 names `mjht` as "the default Huffman table for a Motion-JPEG data stream"; it is the fallback a Motion-JPEG field consults when its own *Huffman table offset* is `0` (QTFF p. 95 Motion-JPEG format A APP1 marker, p. 96 format B header: "If this field is set to 0, check the image description for a default Huffman table"), where the image description is the sample description carrying this extension. The atom was previously opaque inside `SampleDescription::extra`. QTFF defines no internal structure for the body beyond that one-line description — the bytes are a JPEG `DHT` marker-segment body whose class/identifier nibble pair, 16 per-code-length counts, and symbol-value list per table are owned by the ISO JPEG specification, not by QTFF — so this container crate surfaces the raw bytes verbatim and leaves their interpretation to the Motion-JPEG codec, mirroring round 267's `mjqt` treatment exactly. New `media_meta` additions: a typed `Mjht { data: Vec<u8> }` struct preserving the body verbatim, the free function `parse_mjht(payload) -> Result<Mjht>` (no header / list-count / version-flags prologue, so the whole body is the table data; no length is rejected — Table 3-2 fixes no minimum, and a zero-byte body is a degenerate-but-valid declaration surfaced through `is_empty()`), `len()` / `is_empty()` accessors, and a new `Option<Mjht>` field on `SampleDescription` populated by the existing `scan_video_extensions` walker — purely additive, no caller code changes required. With `mjht` landed, every Table 3-2 extension now has a typed surface. 4 new unit tests in `src/media_meta.rs` plus 5 end-to-end demuxer integration tests in `tests/synth_round279_mjht.rs` pin the DHT round-trip, the empty-body-preserved case, the missing-`mjht`-leaves-field-unset baseline, an arbitrary-payload verbatim survival, and the `mjqt` + `mjht` coexistence case (each routing to its own typed field). ISO BMFF does not define `mjht`; it is QuickTime-only and stays absent for non-QTFF inputs. Test count 991 → 1000.
- Round 267 — default Motion-JPEG quantization table (`mjqt`) video sample-description extension parser, continuing the QTFF p. 94 Table 3-2 "Video sample description extensions" chain begun in round 264 (`fiel`). Table 3-2 names `mjqt` as "the default quantization table for a Motion-JPEG data stream"; it is the fallback a Motion-JPEG field consults when its own *quantization table offset* is `0` (QTFF p. 95 Motion-JPEG format A APP1 marker, p. 96 format B header: "If this field is set to 0, check the image description for a default quantization table"), where the image description is the sample description carrying this extension. The atom was previously opaque inside `SampleDescription::extra` — the round-2 visual-extension scan recognised `gama` / `pasp` / `clap` / `colr` and (round 264) `fiel` but left `mjqt` unparsed. QTFF defines no internal structure for the body beyond "the default quantization table for a Motion-JPEG data stream" — the bytes are a JPEG `DQT` marker-segment body whose precision/identifier nibble pair plus 64-entry zig-zag table per table are owned by the ISO JPEG specification, not by QTFF — so this container crate surfaces the raw bytes verbatim and leaves their interpretation to the Motion-JPEG codec, exactly as the `colr` ICC-profile payload and the codec-specific `extra` blob are surfaced opaque. New `media_meta` additions: a typed `Mjqt { data: Vec<u8> }` struct preserving the body verbatim for round-trip fidelity, the free function `parse_mjqt(payload) -> Result<Mjqt>` (the atom carries no QTFF-defined header, list-count, or version/flags prologue, so the whole body is the table data; no length is rejected — Table 3-2 fixes no minimum, and a zero-byte body is a degenerate-but-valid table declaration surfaced through the predicate rather than dropped), two accessors on `Mjqt` (`len()` returns the surfaced byte length; `is_empty()` reports the zero-length case so callers requiring a usable table can gate on it), and a new `Option<Mjqt>` field on `SampleDescription` populated by the existing `scan_video_extensions` walker. Purely additive — no existing parser surface changes and no behaviour change for any caller using `SampleDescription::extra` directly. Both the default `registry` feature and the `--no-default-features` standalone configuration build and pass. Pinned by 4 new unit tests in `src/media_meta.rs` (a representative single-table DQT body round-trips byte-for-byte; an empty body is preserved not rejected; an arbitrary non-DQT payload survives verbatim; the `Mjqt::default()` empty shape) plus 4 new integration tests in `tests/synth_round267_mjqt.rs` (a DQT body round-trips through the full demuxer surface; an empty `mjqt` round-trips as a present-but-empty declaration; the missing-`mjqt`-leaves-field-unset baseline; an arbitrary payload survives end-to-end). ISO BMFF (ISO/IEC 14496-12) does not define `mjqt` — it is a QuickTime-only video sample-description extension and stays absent for non-QTFF inputs reaching this demuxer.
- Round 264 — Field Handling (`fiel`) video sample-description extension parser plus typed gamma (`gama`) accessor (Apple QuickTime File Format Specification, 2001-03-01, p. 94, Table 3-2 "Video sample description extensions"). QTFF Table 3-2 lists `fiel` as a two-byte sample-description extension carrying a field count (byte 0: `1` progressive or `2` interlaced) and a field-ordering selector (byte 1, meaningful when count is 2: `0` "field ordering is unknown", `1` "T is displayed earliest, T is stored first in the file" — top-field first, `6` "B is displayed earliest, B is stored first in the file" — bottom-field first). The atom was previously unparsed; the existing round-2 visual-extension scan recognised `gama` / `pasp` / `clap` / `colr` but left `fiel` opaque inside the `SampleDescription::extra` blob, forcing downstream callers to re-parse the bytes by hand to drive a de-interlacer's display-order decision. The new module additions add a typed `Fiel { field_count: u8, field_ordering: u8 }` struct exposing both raw bytes for round-trip fidelity plus three accessors (`Fiel::is_interlaced()` returns `true` iff `field_count == 2`; `Fiel::is_spec_field_count()` returns `true` iff the count byte is one of the two spec-enumerated values; `Fiel::ordering()` returns the spec-named `FieldOrdering` variant or `None` when the byte is not enumerated), the matching `FieldOrdering` enum naming exactly the three values QTFF p. 94 lists (`Unknown` / `TopFieldFirst` / `BottomFieldFirst` — no fall-through variant since the spec leaves any other byte's meaning unspecified), `parse_fiel(payload) -> Result<Fiel>` (rejects any body length != 2 since `fiel` is fixed-width with no list or version-flags prologue; surfaces the two raw bytes verbatim so a non-spec writer's bytes survive parse), the `FIEL_BODY_LEN = 2` constant documenting the on-disk shape, and a new `Option<Fiel>` field on `SampleDescription` populated by the existing `scan_video_extensions` walker. The accessor for the round-2 `gama` field also lands: `SampleDescription::gamma_value() -> Option<f64>` returns the typed 16.16 fixed-point view of the raw `Option<u32>` field. QTFF p. 94 Table 3-2 describes `gama` as a "32-bit fixed-point number" without explicitly calling out the radix point but every other "32-bit fixed-point" value in QTFF Chapter 4 follows the QuickDraw 16.16 convention (mvhd `rate`, tkhd matrix `a`/`b`/`d`/`e`/`u`/`v`/`w`, `tapt` width/height, sound `balance`); the accessor divides the raw word by 65536.0 and returns `None` when the field is absent (no default substitution). Purely additive — no existing parser surface changes and no behaviour change for any caller using `SampleDescription::extra` or `SampleDescription::gamma` directly. Both the default `registry` feature and the `--no-default-features` standalone configuration build and pass. Pinned by 8 new unit tests in `src/media_meta.rs` (progressive round-trip; interlaced top-first / bottom-first / unknown round-trips; the unspec ordering byte fall-through across a representative sample of {2,3,4,5,7,9,0x55,0xFF}; the out-of-spec field-count preservation across {0,3,17,0x80,0xFF}; the body-length rejection across {0,1,3,4,8}; and a default-value spec compliance check) plus 6 new integration tests in `tests/synth_round264_fiel.rs` (each of the four field-count/ordering shapes through the full demuxer surface, the missing-`fiel`-leaves-field-unset baseline that also exercises the new `gamma_value` accessor end-to-end, and the missing-`gama` case verifying `gamma_value` returns `None` rather than substituting a default). ISO BMFF (ISO/IEC 14496-12) does not define `fiel` — it is a QuickTime-only video sample-description extension and stays absent for non-QTFF inputs reaching this demuxer.
- Round 259 — Compressed Movie atom (`cmov`) parser and its two subatoms — Data Compression atom (`dcom`) and Compressed Movie Data atom (`cmvd`) — at file scope (QTFF, 2001-03-01, "Compressed Movie Resources" pp. 80 – 81 / Table 2-5). Beginning with QuickTime 3 (QTFF p. 80 line "Beginning with QuickTime 3, it also became possible to compress the meta-data"), a writer may losslessly compress the movie resource itself; the resulting file's top-level `moov` atom carries a single `cmov` child instead of the usual `mvhd` + per-track structure, and `cmov` in turn carries one `dcom` (4-byte compression algorithm FourCC) and one `cmvd` (4-byte big-endian uncompressed size followed by the compressed payload to atom end). New `cmov` module with three layered parsers: `parse_dcom(payload) -> Result<Dcom>` validates the fixed-width 4-byte body and surfaces the algorithm FourCC verbatim through `Dcom::algorithm`; `parse_cmvd(payload) -> Result<Cmvd>` requires at least the 4-byte uncompressed-size word and exposes `Cmvd::uncompressed_size` plus the compressed-data tail as `Cmvd::compressed_data` (with `Cmvd::compressed_size()` symmetric accessor); `parse_cmov(payload) -> Result<Cmov>` walks the container body and pairs the parsed `dcom` and `cmvd` into a single `Cmov { dcom, cmvd }`. Constants `DCOM_BODY_LEN = 4`, `CMVD_MIN_BODY_LEN = 4`, and `DCOM_ALG_ZLIB = *b"zlib"` document the on-disk shape; QTFF p. 81 names the field generically as a "lossless data compression algorithm" identifier and does not mandate any particular FourCC, but `'zlib'` is field-observed and surfaced as a constant so callers comparing against it do not have to hand-build the byte literal. `Dcom::is_zlib()` returns the canonical-match predicate while preserving any vendor / future-spec algorithm bytes verbatim through `Dcom::algorithm`. The wrapper parser scope is deliberately narrow: it surfaces the on-disk structure of all three atoms but does not perform the decompression step — the decompressor named by the `dcom` algorithm FourCC is a separate concern that a later round wires in through the workspace's compression crate. Rejected at parse time: `dcom` body length != 4 (fixed-width with no list), `cmvd` body length < 4 (cannot encode the size word), `cmov` body that does not contain at least one `dcom` and one `cmvd` child, malformed child size words (< 8 declared or extending past the parent), and any error returned by the leaf parsers themselves. Duplicate `dcom` / `cmvd` inside one `cmov` is tolerated first-wins, matching the conservative-merge discipline of `parse_clip` and `parse_matt`; unknown sibling atoms inside `cmov` are tolerated but ignored, matching the forward-compat discipline of every other QTFF container in this crate. `size == 0` "extends to end of parent" (QTFF p. 19) is honoured on the trailing child so a writer that emits an open-ended `cmvd` round-trips correctly. ISO BMFF does not define `cmov`/`dcom`/`cmvd`; the parsers are reachable only from a `moov` walker that elects to inspect them and stay inert for plain MP4 / fMP4 / HEIF / AVIF inputs. Pinned by 19 unit tests in `src/cmov.rs` covering: `dcom` round-trip with the `'zlib'` algorithm and with a non-zlib FourCC where the predicate flips false; `dcom` short / long / empty body all reject; `cmvd` round-trip with a non-empty compressed payload; `cmvd` round-trip with `uncompressed_size = 0` and an empty payload (the only legal zero-byte tail); `cmvd` short / empty body reject; `cmvd` round-trips `u32::MAX` uncompressed size without sign confusion; `cmov` canonical Table 2-5 layout round-trips end-to-end; `cmov` reversed child order (`cmvd` before `dcom`) still parses (Table 2-5 illustrates a particular order but the spec does not mandate it); `cmov` unknown sibling atoms are skipped and the known `dcom` + `cmvd` pair still surfaces; `cmov` missing `dcom` or missing `cmvd` rejects with a spec-citing error; `cmov` empty body rejects; `cmov` duplicate child first-wins preserves the first occurrence and ignores the second; `cmov` open-ended trailing `cmvd` with declared `size == 0` consumes the rest of the buffer per QTFF p. 19; and `cmov` malformed child size word (< 8) stops the walk cleanly so the parser surfaces the resulting missing-child error rather than panicking. Both the default `registry` feature and the `--no-default-features` standalone configuration build and pass. Purely additive — no existing parser surface changes and no behaviour change for any caller; the `cmov` walker has not been wired into the `moov` top-level walker yet, so existing demuxer behaviour against a compressed-movie input is unchanged from before this round (a follow-up round can elect to invoke `parse_cmov` from the `moov` walker once a decompressor is wired up to feed the uncompressed inner movie back through the existing parser).
- Round 256 — typed chunk-walking primitive over QTFF p. 75 Sample-to-Chunk (`stsc`) + p. 78 Chunk Offset (`stco` / `co64`) + p. 76 Sample Size (`stsz`) tables. The `SampleTable::iter_samples` decode-order walker has always summed `stsz` sizes inside each chunk to locate per-sample byte offsets, but there was no public way to ask "which chunk holds sample N" or "what is the absolute file offset of sample N" without iterating every prior sample. The new accessors close that gap as a typed random-access surface. On `SampleTable`: `chunk_count` (length of `stco` / `co64`), `samples_in_chunk(chunk_1based)` (the `samples_per_chunk` of the `stsc` row that applies to the 1-based chunk, per QTFF p. 76 "Each table entry corresponds to a set of consecutive chunks…"), `sample_description_id_for_chunk(chunk_1based)` (the row's `sample_description_id`), `chunk_first_sample(chunk_1based)` (0-based decode-order index of the first sample in the chunk — sums `samples_per_chunk` across preceding chunks within and across `stsc` rows), `chunk_for_sample(sample_idx)` (random-access form of step 2 of QTFF p. 79 "Finding a Sample": scans `stsc` rows to find the chunk whose sample-range covers the 0-based decode-order `sample_idx`, returning `(chunk_1based, sample_offset_in_chunk_0based)`), `sample_size_at(sample_idx)` (uniform-or-table `stsz` / `stz2` lookup wrapped as one accessor), `sample_offset(sample_idx)` (mirrors all four steps end-to-end: chunk-base from `chunk_offsets` plus the sum of every earlier sample's size inside the chunk — companion of `iter_samples` but without iterating prior samples), and `chunk_byte_extent(chunk_1based)` (total file byte span of a chunk as `(start, end_exclusive)` for chunk-aligned prefetch or HTTP-range reads, per QTFF p. 74 "Chunks ... allow optimized data access"). Five corresponding `MovDemuxer` wrappers (`chunk_count`, `samples_in_chunk`, `chunk_for_sample`, `sample_offset`, `chunk_byte_extent`) take a 0-based `track_index` and otherwise delegate. Purely additive — existing iter-walker and `Packet` byte offsets are unchanged, no parsing behaviour is touched. Total functions across out-of-range inputs (zero or past-end chunk number, sample index past `sample_count`, malformed `samples_per_chunk == 0` row) — every error path returns `None` rather than panicking, so the bounded `Vec::get` discipline applied to the rest of the sample-table surface carries through to the new shape. Pinned by 9 unit tests in `src/sample_table.rs` (QTFF p. 76 Figure 2-35 worked-example layout: 3 `stsc` rows spanning 5 chunks — 3+3+1+1+1 samples — exercising every accessor plus the single-row "common case" shape, the empty-`stbl` fragmented-only shape, and the malformed `samples_per_chunk == 0` shape) plus 5 integration tests in `tests/synth_round256_chunk_walking.rs` against a hand-built QT file carrying the same QTFF p. 76 Figure 2-35 layout, verified end-to-end through the public `MovDemuxer` accessors with the demuxer-resolved offsets cross-checked against the actual mdat bytes at the resolved positions. The round-176 fuzz harness extends to call all five demuxer accessors with attacker-derived chunk numbers and sample indices (including zero, `chunk_count + 1`, and a value drawn from the input's first / second 32-bit words), so a fuzz input crafting a row count vs chunk-offset table mismatch reaches every accessor without panicking.
- Round 246 — inverse edit-list mapper `movie_pts → media_pts` symmetrically completing the round-74 / round-91 forward mapper (QTFF Chapter 2 "Edit Atoms" pp. 46 – 48 and Chapter 5 "Playing With Edit Lists" pp. 226 – 227). New free function `edit::movie_pts_to_media_pts(segments, movie_pts, movie_timescale, media_timescale) -> Option<i64>` is the inverse of `edit::media_pts_to_movie_pts`: a user asking "what media-sample is at movie-time T" gets the right media-PTS back, honouring every edit-list semantic the forward mapper already handles. Algorithm scans the resolved segment list in declaration order and matches each segment's half-open `[movie_time_start, movie_time_end)` window against the queried `movie_pts`; zero-duration segments collapse the window to the single boundary tick. On `EditSegmentKind::Empty` the helper returns `None` (the movie-time slice has silence/black per QTFF p. 47 and so has no media-time correspondence); on `EditSegmentKind::Dwell` it returns the held `media_time` (ISO/IEC 14496-12 §8.6.6.3 every movie-time tick in the segment maps to the same media frame); on `EditSegmentKind::Media` it converts the movie-time delta `Δmovie = movie_pts − movie_time_start` into a media-time delta via `Δmedia = Δmovie × media_ts × rate_fp / (movie_ts × 65536)` — the inverse of the round-91 forward formula, mirroring the QTFF p. 226 – 227 worked example (600 movie ticks at media_rate 2.0 consume 200 media ticks, so 1 movie tick at rate 2.0 advances the source by 2 media ticks). Rate is 16.16 fixed-point so the arithmetic stays integer end-to-end; rounding is half-up via `(num + denom/2) / denom`, matching the convention used everywhere else in this module. QTFF p. 48 forbids `media_rate <= 0` on a Media segment, so the helper rejects those segments on a per-segment basis and continues scanning. Negative `movie_pts` always returns `None` — the presentation timeline starts at movie tick 0. Two thin wrappers complete the surface: `Track::movie_pts_to_media_pts(movie_pts, movie_timescale, movie_duration)` resolves the track's edit segments against the supplied movie timescale and routes through the free function (symmetric with the existing `Track::media_pts_to_movie_pts`), and `MovDemuxer::media_pts_for(track_index, movie_pts)` is the demuxer-level inverse of `MovDemuxer::movie_pts_for(track_index, media_pts)` honouring the parsed `mvhd` timescale and duration. The typical caller is a seek-by-presentation-time entry point: convert the user's requested `movie_pts` to media-time then drive `MovDemuxer::seek_to(stream, pts)` (whose input is already media-PTS) with the resolved value. Pinned by 12 unit tests in `src/edit.rs` covering initial-empty-edit rejection, timescale rescaling, non-zero `media_time_start` segments, the §8.6.6.1 composition-shift zero-duration idiom, dwell hold-across-segment, single double-speed and half-speed worked examples, the full three-segment QTFF p. 226 – 227 worked example, negative-`movie_pts` rejection, zero-timescale rejection, per-segment `media_rate <= 0` rejection, and a forward/inverse round-trip across two unity-rate segments. 6 integration tests in `tests/synth_round246_inverse_pts_mapping.rs` cover the end-to-end demuxer wrapper against the same synth scaffold the round-91 forward integration tests use. The round-176 fuzz harness extends to call the new per-track `media_pts_for` accessor on the same three boundary `movie_pts` values it already probes for the forward mapper (`0` / `i64::MIN` / `i64::MAX`) plus a value derived from input bytes 8 – 15 so the fixed-point math runs against attacker-influenced inputs without panicking, matching the existing forward-direction fuzz coverage.
- Round 243 — typed accessors for the remaining QTFF Table 2-2 `tref` reference kinds (Apple QuickTime File Format Specification 2001-03-01 pp. 49 – 51, Figure 2-13 Track reference atom layout, Table 2-2 Track reference types). Round 240 left `Track::chapter_track_ref()` and `Track::timecode_track_ref()` typed but the remaining four kinds (`'sync'` / `'scpt'` / `'hint'` / `'ssrc'`) were reachable only via the generic `Track::track_refs_of_kind(TrackRefKind)` helper. The new symmetrical typed accessors are `Track::sync_track_refs()` (Table 2-2 row `'sync'` — synchronization, usually between a video and sound track), `Track::transcript_track_refs()` (row `'scpt'` — transcript, usually a text track), `Track::hint_track_refs()` (row `'hint'` — the source media tracks a hint track packetizes for RTP, QTFF "Hint Media" p. 145), and `Track::non_primary_source_track_refs()` (row `'ssrc'` — non-primary sources whose data the writing track consumes through its `imap`, QTFF pp. 51 – 53). Each returns the declaration-ordered list of 1-based `tkhd.track_id` values across every reference-type atom of that kind, with the spec p. 51 `0`-valued "unused-entry slot" sentinel filtered out. Demuxer-side track-id-to-index resolvers complete the surface: `MovDemuxer::track_index_for_id(track_id)` is the underlying lookup translating a 1-based `tkhd.track_id` to its 0-based index inside `MovDemuxer::tracks` (`None` when the id is `0`, when no track in the file declares that id, or when the file has no tracks at all); a generic `MovDemuxer::tref_track_indices(track_index, kind)` resolves every `tref/<kind>` reference declared by `track_index` to the 0-based peer indices; and five per-kind helpers `MovDemuxer::timecode_track_index(track_index) -> Option<usize>` / `sync_track_indices(track_index) -> Vec<usize>` / `transcript_track_indices` / `hint_track_indices` / `non_primary_source_track_indices` wrap that resolver for direct use. The 0-id slot and unresolvable ids (writer slip — pointed-at track absent from the file) are both filtered out at the demuxer resolver layer so callers receive only resolvable indices; declaration order is preserved across every reference-type atom of the requested kind. Round-176 fuzz harness coverage is unchanged — the harness already swept `track_refs_of_kind` per track via the existing `track_input_map` `'ssrc'` slot probe, and the new resolvers route through the same `track_refs_of_kind` machinery. Pinned by `tests/synth_round243_tref_typed.rs` (7 tests): typed accessors over the full menu of references on a four-track fixture; empty surfaces on tracks that declare no `tref`; `track_index_for_id` resolves and rejects appropriately; demuxer per-kind resolvers translate to 0-based indices; the resolver filters unresolvable and zero-slot rows; out-of-range `track_index` returns empty surfaces (total-function shape); declaration order preserved across multiple reference-type atoms of the same kind in one `tref`. All accessors are purely additive — no behaviour change for any existing caller, and the underlying `Track::references` raw surface stays public.
- Round 240 — typed compositing-mode and balance accessors on the round-5 `Gmin` struct (QTFF Chapter 4 "Basic Data Types" — Table 4-2 p. 200 "Graphics Modes" and the Balance paragraph p. 201). New [`GraphicsMode`] enum names every Table 4-2 mode — `Copy` (`0x0000`), `DitherCopy` (`0x0040`), `Blend` (`0x0020`), `Transparent` (`0x0024`), `StraightAlpha` (`0x0100`), `PremulWhiteAlpha` (`0x0101`), `PremulBlackAlpha` (`0x0102`), `Composition` (`0x0103`, tracks-only per spec), `StraightAlphaBlend` (`0x0104`) — plus an `Other(u16)` fall-through preserving any vendor / future-spec raw value so the parser does not commit to a meaning the spec has not bound. `GraphicsMode::raw()` round-trips back to the on-disk code 1:1 with `GraphicsMode::from_raw()`. `GraphicsMode::uses_opcolor()` reports the Table 4-2 "Uses opcolor" column (true for `Blend` / `Transparent` / `StraightAlphaBlend`; false for `Other`). New `Gmin::graphics_mode_kind()` returns the typed view of the raw `Gmin::graphics_mode: u16` field; `Gmin::balance_as_f32()` decodes the 16-bit 8.8 signed fixed-point `Gmin::balance: i16` field per p. 201 into the [-1.0, +1.0] real-valued balance setting (high-order 8 bits = integer portion, low-order 8 bits = fraction; negative = left, positive = right, zero = centered). The raw `graphics_mode` and `balance` fields stay public for callers that need the exact on-disk encoding for round-trip remuxing; the new accessors are purely additive and no `Gmin` parser behaviour changes. The round also corrects a stale doc-comment on `Gmin::graphics_mode` that paired `0x0100` with "transparent" — Table 4-2 fixes transparent at `0x0024` and reserves `0x0100` for straight alpha.
- Round 234 — Padding Bits Box (`padb`) parser at `stbl` scope (ISO/IEC 14496-12 §8.7.6). The FullBox records, for each sample, the number of bits at the end of the sample's media payload that are writer-inserted padding to round up to a whole-byte boundary (§8.7.6.1) — needed when a downstream stage must re-emit the original bit-stream verbatim. Unlike `sdtp` / `stdp`, the box carries its own `sample_count` field (§8.7.6.2) so the parse runs at walk time and does not depend on `stsz` / `stz2`. Layout per §8.7.6.2 is `[version:1][flags:3][sample_count:4]` then `((sample_count + 1) / 2)` packed bytes, each holding `[reserved:1, pad1:3, reserved:1, pad2:3]` most-significant nibble first; `pad1` covers sample `(i*2)+1` (1-based per §8.7.6.3) and `pad2` covers sample `(i*2)+2`. New `SampleTable::padb: Vec<u8>` field plus `SampleTable::sample_padding_bits(sample_idx)` and `MovDemuxer::sample_padding_bits(track, sample)` accessors return the 3-bit `pad` value (`0..=7`) 1:1 with what the writer emitted. For an odd `sample_count` the trailing low nibble of the final packed byte is the `pad2` slot for a non-existent "sample N+1" and is silently discarded. Rejected at open time: payload shorter than the 8-byte FullBox header + `sample_count` u32, unknown FullBox `version` (§8.7.6.2 spec-fixes at 0), non-zero `flags` (§8.7.6.2 spec-fixes at 0; silent acceptance would let a malformed writer leak undefined bits past the parser), body shorter than `(sample_count + 1) / 2` packed bytes (truncated table), and a non-zero `reserved` bit in either nibble of any packed byte (§8.7.6.2 spec-fixes both the 0x80 and 0x08 bits at 0). A duplicate `padb` inside one `stbl` is tolerated first-wins — §8.7.6.1 lists the box as `Quantity: Zero or one`; first-wins matches the conservative-merge policy applied to every other "at most once" stbl-scope box (`sdtp`, `stdp`, `sbgp`/`sgpd`, `saiz`/`saio`). QTFF does not define this box; it is ISO BMFF-only. The round-176 fuzz harness extends to call the new per-track `sample_padding_bits` accessor on a couple of attacker-influenced sample indices so a `padb`-carrying fuzz input reaches the parser and the bounded `Vec::get` accessor without panicking.
- Round 226 — Level Assignment Box (`leva`) parser at `moov/mvex` scope (ISO/IEC 14496-12 §8.8.13). NOTE: `fragment::parse_mvex` now returns `(Option<Mehd>, Vec<TrexDefaults>, Option<Leva>)` — a pre-1.0 breaking signature change for callers building a custom walker on top of the helper directly. The demuxer's `MovDemuxer::open` path absorbs the new shape internally; no behaviour change for callers using the high-level demuxer API. The FullBox names the *levels* the round-219 §8.16.4 Subsegment Index Box (`ssix`) references; adaptive-streaming clients pair the two so a temporal-scalability decoder can fetch only the base-layer level (§8.8.13.1). Layout per §8.8.13.2 is a 1-byte `level_count` followed by `level_count` rows of `(track_id[4], padding_flag[1 bit], assignment_type[7 bits])` plus a per-`assignment_type` trailer: type 0 carries a 4-byte `grouping_type` (sample-group assignment), type 1 carries `grouping_type[4] + grouping_type_parameter[4]` (parameterized sample-group assignment), types 2 and 3 carry no trailer (track-keyed assignment, §8.16.4 distinguishes the two on the consumer side), and type 4 carries a 4-byte `sub_track_id` (sub-track assignment, §8.14). New [`Leva`] / [`LevaLevel`] / [`AssignmentType`] types plus `Leva::level_count()`, `Leva::level(j)` (1-based per §8.8.13.3 "loop entry j"), and `Leva::track_ids()` (declaration-order de-duplicated `track_id` set — useful when wiring the box to a §8.8 track table). New `MovDemuxer::leva: Option<Leva>` field populated through `parse_mvex`; the field stays `None` when the file omits the box, when the file is a plain `.mov` (QTFF does not define `leva`), or when the file is a non-fragmented MP4 with no `mvex` container. Rejected at open time: unknown FullBox `version` (§8.8.13.2 spec-fixes at 0), payload shorter than the 5-byte FullBox header + `level_count` byte, `level_count` below 2 (§8.8.13.3 fixes the minimum at 2), body shorter than `level_count × 5` (every row carries at least the 5-byte `track_id + flag/type` prefix), a per-type trailer that overruns the remaining body, §8.8.13.3 ordering-rule violations ("The sequence of assignment_types is restricted to be a set of zero or more of type 2 or 3, followed by zero or more of exactly one type" — once a non-2/3 row appears, every subsequent non-2/3 row must carry the same `assignment_type`, and a 2/3 row may not follow a pinned tail-block row), and any trailing bytes past the declared row list (the box carries no list past `level_count`). `Reserved { raw }` rows surface unknown `assignment_type` values (`5..=127`) verbatim rather than rejecting so a future derived spec adding a new code does not break this parser; the reserved row consumes no trailer because the spec leaves the payload unspecified. A malformed writer emitting two `leva` boxes inside one `mvex` is tolerated first-wins (§8.8.13.1 fixes Quantity at Zero or one; first-wins matches the conservative-merge policy applied to `mehd`, `ctab`, `clip`, `pdin`, and the other singletons). QTFF does not define this box; it is ISO BMFF-only and stays absent for plain `.mov` inputs. The round-176 fuzz harness extends to walk the optional `MovDemuxer::leva` row list (capped at 64 to bound a writer cramming the max 255 rows) touching `level_count`, the per-row `track_id` / `padding_flag` / `assignment_type` accessors, `track_ids`, and the 1-based `level()` boundaries (0 and `level_count`+1) so the off-by-one path stays covered on every `leva`-carrying fuzz input.
- Round 219 — Subsegment Index Box (`ssix`) parser at file scope (ISO/IEC 14496-12 §8.16.4). The FullBox pairs one-to-one with the immediately preceding `sidx` box that indexes only leaf subsegments (`Quantity: 0 or 1` per associated `sidx`, §8.16.4.1) and partitions each subsegment into level-keyed *partial subsegments* — a compact "table of contents" that lets a DASH / CMAF client fetch only the bytes for a chosen Level Assignment Box (§8.8.13) level. Layout per §8.16.4.2 is `subsegment_count[4]`, then per subsegment a `range_count[4]` and `range_count`-many `(level[1], range_size[3])` rows. New [`Ssix`] / [`SsixSubsegment`] / [`SsixRange`] types plus `Ssix::subsegment_count()`, `Ssix::total_size_for(index)` (§8.16.4.1 "each byte assigned to a level" invariant — sums `range_size` across a subsegment's partial-subsegment chain, widened to `u64` because the per-range 24-bit limit doesn't bound a full subsegment), and `Ssix::partial_subsegment_offset(subsegment_start, index, range_index)` (walks the `range_size` chain from a caller-supplied subsegment start, typically sourced from the paired `sidx`'s `subsegment_offset` accessor). New `MovDemuxer::ssix: Vec<Ssix>` field (file order — every parsed box is surfaced even when it doesn't immediately follow a `sidx`) and `MovDemuxer::ssix_for_sidx(sidx_index)` accessor that resolves the §8.16.4.1 pairing rule: the demuxer's top-level walker records which `sidx` (if any) each `ssix` binds to at parse time, with O(1) lookup at runtime. Orphan `ssix` (out-of-order, or following something other than `sidx`) is still parsed and surfaced through the public Vec but is *not* bound to any `sidx`. A non-`sidx`/`ssix` top-level box between a `sidx` and the following `ssix` breaks the pairing window per §8.16.4.1's "the next box after the associated Segment Index box" rule. Rejected at open time: unknown FullBox `version` (spec fixes at 0, §8.16.4.2), payload shorter than the 8-byte FullBox header + `subsegment_count` u32, a declared `range_count` below 2 (§8.16.4.1: every byte must be assigned to a level so a single partial subsegment is illegal), a `subsegment_count` or `range_count` overrun, and any trailing bytes past the declared subsegment list (the box carries no list past the final subsegment so leftover bytes signal a malformed writer). The up-front bound on `subsegment_count` × 4 against remaining body bytes rejects a forged huge count before allocating `Vec::with_capacity`. QTFF does not define this box; it is ISO BMFF-only and stays absent for plain `.mov` inputs. The round-176 fuzz harness extends to walk every collected `ssix` entry (capped at 64 to bound pathological writers) through `total_size_for` / `partial_subsegment_offset` with attacker-influenced indices and overflow-prone anchor values, then exercises the `ssix_for_sidx` cross-reference path against each declared `sidx`.
- Round 216 — Track Input Map atom (`imap`) parser at per-track scope (QTFF pp. 51 – 53 / Figure 2-14). The atom describes how each non-primary-source (`tref` of type `'ssrc'`, QTFF p. 50 Table 2-2) modulates this track's presentation through a transform matrix, a QuickDraw clipping region, an 8.8 fixed-point volume curve, a sound-balance level, a graphics-mode record, or a per-object variant (sprite transform, sprite graphics mode, compressed image data). The body is a list of track input atoms (` in`, with the leading two bytes 0x00 per QTFF p. 52) each carrying a 12-byte QT-style header tail (`atom_id` + reserved + `child_count` + reserved) before its own classic-shaped child atoms (` ty` required, `obid` optional). The required ` ty` carries a 4-byte identifier classified into the new `InputTypeKind` enum across QTFF Table 2-3's eight values (`Matrix` / `Clip` / `Volume` / `Balance` / `GraphicsMode` / `ObjectMatrix` / `ObjectGraphicsMode` / `Image`, plus an `Other` fall-through preserving any vendor or future-spec raw value). `kTrackModifierTypeImage` is on-disk the FourCC `'vide'` — QTFF reuses the video-media-type marker as the input-type identifier — surfaced bit-exactly via `K_TRACK_MODIFIER_TYPE_IMAGE`. The three per-object identifiers (`ObjectMatrix` / `ObjectGraphicsMode` / `Image`) require an accompanying `obid` child; cross-field consistency QTFF p. 53 mandates is enforced at open time. The 1-based `TrackInputEntry::atom_id` indexes into the parent track's `'ssrc'` reference list. New `MovDemuxer::track_input_map(track)` and `Track::track_input_map()` accessors return the parsed `TrackInputMap`; `TrackInputMap::entry_for_ssrc_slot(id)` keys lookups on `atom_id` since writers are not strictly required to emit entries in slot order. Rejected at open time: an ` in` body shorter than the 12-byte QT-style header tail, non-zero values in either reserved field, missing required ` ty`, ` ty` or `obid` body that is not exactly 4 bytes, duplicate ` ty` or `obid` inside one ` in`, an unexpected child FourCC inside ` in` or inside `imap` itself, a `child_count` mismatch, and a per-object input-type identifier paired with no `obid`. Trak-scope first-wins duplicate-merge policy matches `tapt` / `load` / `cslg` / `clip` / `matt`. QuickTime-only — ISO BMFF does not define `imap`. The round-176 fuzz harness extends to walk every track's `imap` entry list (capped at 16 entries/track) and exercises the `entry_for_ssrc_slot` lookup against an attacker-influenced 1-based slot id derived from the input's first 32-bit word.
- Round 210 — Degradation Priority Box (`stdp`) parser at `stbl` scope (ISO/IEC 14496-12 §8.5.3). One 16-bit unsigned `priority` per sample, sized from the `stsz`/`stz2` `sample_count` (§8.5.3.1) — no on-disk count field, so the parse defers to after the stbl walk completes, mirroring the round-98 `sdtp` deferred-sizing path. New `SampleTable::stdp: Vec<u16>` field plus `SampleTable::sample_degradation_priority(sample_idx)` and `MovDemuxer::sample_degradation_priority(track, sample)` accessors return the raw 16-bit value 1:1 with what the writer emitted; §8.5.3.1 leaves the numeric meaning and acceptable range to derived specifications, so callers consult the spec carrying the `stdp` track to interpret the priority. Rejected at open time: payload shorter than the 4-byte FullBox header (without the guard the version/flags read would scrape uninitialised bytes), non-zero `flags` (§8.5.3.2 spec-fixes the box as `FullBox('stdp', version = 0, 0)`), body shorter than `sample_count × 2` bytes (truncated table). Trailing padding past the declared row count is silently ignored (some writers round the box up to an 8-byte boundary; §8.5.3.2 names exactly `sample_count` rows). A duplicate `stdp` inside one `stbl` is tolerated first-wins — §8.5.3 lists the box as `Quantity: Zero or one` and first-wins matches the conservative-merge policy applied to every other "at most once" stbl-scope box (`sdtp`, `sbgp`/`sgpd`, `saiz`/`saio`). QTFF does not define this box; it is ISO BMFF-only. The round-176 fuzz harness extends to call the new per-track `sample_degradation_priority` accessor on a couple of attacker-influenced sample indices so an `stdp`-carrying fuzz input reaches the deferred parse and the bounded `Vec::get` accessor without panicking.
- Round 204 — Compact Sample Size Box (`stz2`) parser at `stbl` scope (ISO/IEC 14496-12 §8.7.3.3). The on-disk `field_size` (4 / 8 / 16 bits) is decoded into the existing `stsz_table: Vec<u32>` so downstream `sample_count()` / `sample_size_at()` paths continue to work unchanged. New `SampleTable::sample_size_source: Option<SampleSizeSource>` field and `MovDemuxer::sample_size_source(track_index)` accessor surface which of `stsz` (§8.7.3.2) / `stz2` (§8.7.3.3) populated the table, preserving the on-disk encoding choice for round-tripping remuxers. The two boxes are mutually-exclusive per §8.7.3; a malformed writer emitting both is tolerated first-wins (matching the `sbgp`/`sgpd`/`saiz`/`saio` conservative-merge convention). Rejected at open time: `field_size` other than 4/8/16 (§8.7.3.3.2), non-zero 24-bit `reserved` word (§8.7.3.3.1), truncated entry table, body shorter than the 12-byte fixed header. 4-bit packing decodes MSB-first per §8.7.3.3.2 with a zero-pad nibble silently dropped for odd `sample_count`. The round-176 fuzz harness extends to call the new per-track `sample_size_source` accessor.
- Round 199 — Track Group Box (`trgr`) parser at per-track scope; new `Track::track_groups()` / `MovDemuxer::track_group_entries(track_index)` / `MovDemuxer::tracks_in_group(type, id)` / `MovDemuxer::track_groups()` accessors (ISO/IEC 14496-12 §8.3.4)

## [0.0.3](https://github.com/OxideAV/oxideav-mov/compare/v0.0.2...v0.0.3) - 2026-05-29

### Other

- Round 187 — reject extended-size atoms that overflow u64 (fuzz crash 353f)
- Round 182 — User-Type Box (uuid) parser at file scope
- scrub enumerated-denials disclaimer per audit doctrine
- Round 176 — cargo-fuzz harness for the QTFF / ISO BMFF demuxer
- Round 162 — injection-robustness for the atom walker
- Round 157 — Preview atom (pnot) parser, QTFF pp. 26-27 / Figure 1-7
- Round 150 — traf-scope saiz / saio sample-aux wiring (§8.7.8.1 / §8.7.9.1)
- Round 147 — Sample Auxiliary Information saiz / saio parsers (ISO/IEC 14496-12 §8.7.8 / §8.7.9, stbl-scope)
- parse matt + kmat (Track Matte / Compressed Matte) at track scope
- parse clip + crgn (Clipping atom / Clipping Region) at movie + track scope
- round 137: parse the Color Table atom (`ctab`) at movie scope
- Round 128 — Producer Reference Time Box (prft) parser, ISO/IEC 14496-12 §8.16.5
- Round 125 — Segment Type Box (styp) parser, ISO/IEC 14496-12 §8.16.2
- Round 122 — Track Kind box (kind) parser, ISO/IEC 14496-12 §8.10.4
- Round 118 — Sub-Sample Information Box (subs) parser, ISO/IEC 14496-12 §8.7.7

### Fixed

- Round 187 — `read_atom_header` now rejects any atom header whose
  declared `start + total_size` overflows `u64`. The post-round-182
  scheduled fuzz run (libFuzzer target `demux`) produced
  `crash-353fbd8c75a517f36da693fcea9b24d24240fc5e`: an 8-byte
  placeholder atom followed by a `size=1` extended-size atom whose
  `largesize = u64::MAX`. The top-level walker's
  `body_end = payload_offset + (total_size - header_len)`
  (`src/demuxer.rs:480`, mirrored in `src/atom.rs:263` for
  `walk_children` and `src/demuxer.rs:357` for
  `probe_reference_movies`) computed `24 + (u64::MAX - 16) =
  u64::MAX + 8` and panicked with `attempt to add with overflow`
  on debug builds. The new `checked_add` at the header read site
  is the single point that bounds every downstream `body_end`
  computation: once `start + total_size <= u64::MAX` is proven,
  the equivalent `payload_offset + (total_size - header_len)`
  also fits. Boundary case `start + largesize == u64::MAX`
  remains accepted; downstream layers then decide whether the
  body fits in the actual file.
  - New regression test
    `tests/synth_round187_extended_size_overflow.rs` replays the
    exact crash bytes through `MovDemuxer::open`, focuses the
    rejection on `read_atom_header` at a non-zero start, pins the
    `start + largesize == u64::MAX` boundary as still accepted at
    framing level, and exercises the same overflow shape nested
    inside a `moov` container so the `walk_children` arithmetic
    site is covered too.

### Added

- Round 182 — User-Type Box (`uuid`) parser at file scope.
  - New `src/uuid.rs` module decodes the ISO/IEC 14496-12:2015 §4.2 /
    §11.1 escape-type box: every body opens with a 16-byte UUID
    identifying the vendor extension followed by an opaque payload.
    The parser surfaces both verbatim — `Uuid::usertype` (the raw
    `[u8; 16]`) and `Uuid::payload` (the trailing bytes) — without
    committing the crate to any vendor schema, so callers can dispatch
    on the UUID bytes by exact match (PIFF tfxd /
    `6d1d9b05-42d5-44e6-80e2-141daff757b2`, PIFF tfrf /
    `d4807ef2-ca39-4695-8e54-26cb9e46a79f`, Sony XAVC, GoPro GPMF,
    etc.).
  - File-level `uuid` boxes surface on the demuxer as
    `MovDemuxer::file_uuids: Vec<Uuid>` collected in declaration
    order. §4.2's `Quantity: Zero or more` semantics is preserved
    rather than collapsed: a single file may carry several vendor
    extensions (e.g. tfxd + tfrf, Sony XAVC + GoPro GPMF) and there
    is no implied "first wins" rule, so each entry stays distinct.
  - Two diagnostic helpers ride along: `Uuid::usertype_string()` formats
    the UUID as the canonical RFC 4122 textual form
    `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`, and
    `Uuid::is_iso_reserved_namespace()` /
    `Uuid::iso_namespace_boxtype()` detect and decode the §11.1
    reserved-namespace pattern (`type ‖ 00 11 00 10 80 00 00 AA 00 38
    9B 71`) — the spec forbids writing standard boxes through the
    `'uuid'` escape, so a true result flags a non-conformant writer
    that promoted a normative box into the UUID space.
  - Parser refuses a body shorter than the 16-byte `usertype` prefix
    so a half-record can't silently disappear; an empty payload after
    the UUID is accepted (§4.2 puts no lower bound on payload length).
  - Top-level walker recognises `uuid` as an additional file-scope box
    alongside `pdin` / `sidx` / `styp` / `prft` / `pnot`, matching
    §4.2's "any top-level box" placement rule.
  - QTFF does not define `uuid` at the spec level, but real-world
    `.mov` files routinely embed user-type boxes from vendors that
    emit QuickTime containers, so the file-level field is populated
    for both QT MOV and ISO BMFF derivative inputs.
  - Fuzz harness extended to sweep the file-level `uuid` surface
    (capped at 64 entries per input to bound pathological-writer
    cases) — `usertype_string` / `is_iso_reserved_namespace` /
    `iso_namespace_boxtype` / `payload.len()` are exercised against
    attacker-supplied UUID prefixes.
  - 8 new integration tests in `tests/synth_round182_uuid.rs` cover
    single + multi-`uuid` decode, declaration-order preservation, empty-
    payload acceptance, truncated-prefix rejection at open time,
    reserved-namespace flagging with boxtype recovery, vendor-UUID
    non-reservation, and binary payload byte-for-byte round-trip.
  - 9 new lib-mod unit tests in `src/uuid.rs` cover the parser
    invariants and the §11.1 escape-pattern detector.

- Round 176 — cargo-fuzz harness for the demuxer.
  - New `fuzz/` cargo-fuzz crate with a single `demux` target. The
    target feeds arbitrary fuzz-supplied bytes through
    `MovDemuxer::open`, drains up to 256 packets via `next_packet`,
    touches every file-scope structural accessor (`ftyp`, `mvhd`,
    `pdin`, `sidx`, `styp`, `prft`, `ctab`, `pnot`, `clipping`, plus
    `brand_class` / `is_dash_segment` / `is_cmaf_segment` /
    `is_heic` / `is_avif` / `is_miaf` / `is_fragmented` /
    `is_faststart` / `alternate_groups` / `switch_groups`), sweeps
    every track (capped at 64 per input) through `track_load` /
    `track_selection` / `track_kinds` / `edit_segments_for` /
    `random_access_points`, and re-exercises the round-21 / round-91
    seek path via `seek_to(0, 0)`. Edit-list mapper is poked at
    `media_pts = 0 / i64::MIN / i64::MAX` so the round-74 / round-91
    fixed-point math gets adversarial values without depending on a
    crafted corpus.
  - Pairs with round 162's robustness work: the atom walker's 64 MiB
    `MAX_INMEMORY_ATOM_BODY` cap, the past-EOF top-level rejection,
    and the nested child-vs-parent envelope check are the safety
    invariants the fuzz target keeps exercised across the random
    input space.
  - New `.github/workflows/fuzz.yml` schedules a daily 30-minute
    `cargo fuzz run demux` via the org-level reusable fuzz workflow
    at `OxideAV/.github/.github/workflows/crate-fuzz.yml`.
  - Reference-movie alias resolution is intentionally excluded —
    `open_with_aliases` would resolve fuzz-supplied `rmra/url `
    strings against a caller-supplied opener, which would either
    reach out to the network or spin on file-system probes. The
    no-alias `MovDemuxer::open` path still walks every
    `rmra/rmda/rmdr/rmcs` parser so the reference-movie *parse* side
    is fully covered.

- Round 162 — injection-robustness defenses against forged size fields
  and OOM levers in the atom walker / top-level demuxer parser.
  - New `MAX_INMEMORY_ATOM_BODY` constant (64 MiB). `read_payload`
    refuses to allocate above this cap before the underlying
    `vec![0u8; n as usize]` call lands, so a forged extended `size`
    of (say) 8 GiB on a 1 KiB file errors as a clean parse error
    rather than a multi-GiB allocation that turns into an OOM
    process kill. The cap is generous: every metadata atom in QTFF /
    ISO BMFF that legitimately materialises into a `Vec<u8>` (ftyp,
    moov, mvhd, tkhd, mdhd, stsd, stts, stsc, stsz, stco, co64,
    stss, sdtp, subs, saiz, saio, sgpd, sbgp, tref, udta, meta,
    keys, ilst, kind, tsel, load, clip, crgn, matt, kmat, gama,
    pasp, clap, colr, chan, tapt, clef, prof, enof, pdin, sidx,
    styp, prft, pnot, ctab) stays well under a megabyte in practice.
    `mdat` (sample data, gigabytes legitimately) is never read via
    `read_payload` in this crate — only per-sample seek-and-read
    pulls bytes out of it — so the cap doesn't bound legitimate
    file size.
  - New `read_payload_bounded(r, hdr, max_remaining)` helper for
    callers that already know the maximum payload extent (a parent
    atom's remaining bytes, a known file length). Rejects above the
    envelope before allocating.
  - `MovDemuxer::open` and `MovDemuxer::probe_reference_movies` now
    reject any top-level atom whose declared `size` would extend past
    end-of-file. `walk_children` already enforced the same "child
    does not exceed parent" rule on nested atoms; the top-level
    walker now mirrors it so the cleanest layer of the demuxer is
    spec-bounded too.
  - New `tests/synth_round162_robustness.rs` (16 tests, four groups):
    forged 32-bit / 64-bit / one-byte-past-EOF top-level sizes are
    rejected; `read_payload` and `read_payload_bounded` reject above
    their respective caps and accept exactly at; a truncation sweep
    walks the baseline file byte-by-byte and asserts no panic / no
    OOM at any cut point; a 256-trial xorshift64* random-byte fuzz
    pass confirms hostile garbage never panics; a bogus nested-trak
    size pins `walk_children`'s existing rejection in place; and the
    degenerate `size == 0` (to-EOF) and empty-file cases surface
    cleanly.
  - All four atom-walker / read_payload public APIs are re-exported
    from the crate root for downstream tests / fuzz harnesses:
    `AtomHeader`, `read_atom_header`, `read_payload`,
    `read_payload_bounded`, `skip_payload`, `walk_children`,
    `MAX_INMEMORY_ATOM_BODY`.

- Round 157 — Preview atom (`pnot`) parser at file scope, Apple
  QuickTime File Format Specification (QTFF, 2001-03-01) pp. 26 – 27
  / Figure 1-7.
  - New `pnot` module: [`Pnot`] struct (`modification_date`,
    `version_number`, `atom_type`, `atom_index`) and [`parse_pnot`]
    parser. The atom is a 12-byte fixed-width record (no list / no
    variable section per QTFF Figure 1-7); any other body length is
    rejected at parse time so a truncated or extension-padded box
    can't silently lose its `atom_type` / `atom_index` fields.
    `version_number` is *not* rejected when non-zero — the spec
    fixes it at 0 but a writer that sets a stray value leaves the
    other fields readable, so the parser stays accepting and
    surfaces a `Pnot::is_known_version()` predicate for strict
    consumers. Same accept-and-flag treatment for an `atom_index`
    of 0 (the spec documents the field as 1-based but doesn't
    define a sentinel) via `Pnot::is_valid_index()`.
  - `Pnot::unix_seconds()` converts the Mac-classic
    `modification_date` (seconds since 1904-01-01T00:00:00Z, the
    same epoch QTFF's `mvhd` uses for creation / modification
    times per p. 32) to a Unix-epoch second count via the
    2 082 844 800 s offset, returning `None` for any pre-1970 Mac
    value. The `MAC_TO_UNIX_EPOCH_SECONDS` constant is `pub`-
    exported alongside `PNOT_BODY_LEN` (12) so external callers
    can do the same conversion or round-trip the byte width.
  - `MovDemuxer::pnot: Option<Pnot>` populated by the file-level
    walker. At most one `pnot` is kept per file — the parser
    silently drops duplicates (first-wins, matching the
    conservative-merge convention shared with `pdin` / `ctab` /
    `clip` / `mvhd`). The atom lives at file scope, *not* inside
    `moov`, per QTFF p. 26 — it appears between `ftyp` and `moov`
    in the top-level atom stream. ISO BMFF does not define `pnot`;
    MP4 / fMP4 / HEIF / AVIF inputs leave the field `None`.
  - Ten `src/pnot.rs` unit tests cover field round-trip,
    Mac-to-Unix epoch conversion (zero, known anchor, pre-1970
    `None`), non-`PICT` `atom_type` opacity, the
    `is_known_version` / `is_valid_index` predicates, a high-bit
    `atom_index` (0xFFFF), and the three rejection paths
    (truncated, empty, trailing bytes).
  - Six `tests/synth_round157_pnot.rs` integration tests build a
    minimal QuickTime file (`ftyp qt  ` + `pnot` + `moov` + `mdat`)
    and assert: byte-exact round-trip of all four fields through
    `MovDemuxer::open`; `pnot.is_none()` for files that omit the
    atom; first-wins on duplicate `pnot` emission; truncation and
    trailing-byte rejection at open time; and that a non-zero
    `version_number` parses (with the predicate flagging it).

- Round 150 — `traf`-scope wiring for the round-147 Sample Auxiliary
  Information Sizes / Offsets boxes, ISO/IEC 14496-12 §8.7.8.1 /
  §8.7.9.1.
  - `parse_traf` now also walks `saiz` / `saio` children inside each
    `traf`; returns a new [`TrafParse`] struct (`tfhd`, `tfdt`,
    `truns`, `saiz`, `saio`) instead of the round-18 3-tuple.
    `TrafRecord` carries the new `saiz: Vec<Saiz>` / `saio: Vec<Saio>`
    fields; duplicate boxes for the same `(aux_info_type,
    aux_info_type_parameter)` inside one `traf` are merged
    first-wins per §8.7.8.3 / §8.7.9.3.
  - `MovDemuxer::fragment_sample_aux_info(track_index) ->
    &[FragmentSampleAux]` surfaces per-fragment sample-aux records
    in on-disk fragment order. Each [`FragmentSampleAux`] carries
    the originating `mfhd.sequence_number`, the `tfhd.track_id`,
    the parsed `saiz` / `saio` boxes, and a `lookup(aux_info_type,
    aux_info_type_parameter) -> (Option<&Saiz>, Option<&Saio>)`
    accessor that mirrors the §8.7.8.1 zero-discriminator-match
    semantics of `SampleTable::sample_aux_for`.
  - `Track::fragment_sample_aux: Vec<FragmentSampleAux>` holds the
    per-track aggregation; populated by the demuxer's `moof` walker
    alongside `fragment_samples`. Empty for non-fragmented streams
    and for fragmented tracks whose `traf`s ship no `saiz` / `saio`.
  - Five `tests/synth_round150_traf_sample_aux.rs` integration tests
    build a two-fragment fMP4 with `saiz` + `saio` inside each
    `traf` (fragment 1 with `cenc` discriminator, fragment 2 with
    `cbcs`) and assert: one record per fragment, the originating
    `mfhd.sequence_number` is threaded through, discriminator
    lookups honour §8.7.8.1, the slice is empty for out-of-range
    tracks, the slice is empty when no `traf` ships sample-aux, and
    `stbl`-scope and `traf`-scope accessors don't collide on the
    same track.

### Changed

- `fragment::parse_traf`'s return type is now `Result<TrafParse>`
  (a new struct) rather than the round-18 `Result<(Option<Tfhd>,
  Option<u64>, Vec<Trun>)>` tuple. Out-of-tree callers of the public
  parser need to read fields off the struct instead of destructuring
  the tuple; the new fields `TrafParse::saiz` and `TrafParse::saio`
  carry the §8.7.8.1 / §8.7.9.1 per-fragment sample-aux boxes.

- Round 147 — Sample Auxiliary Information Sizes Box (`saiz`) +
  Sample Auxiliary Information Offsets Box (`saio`) parsers at
  `stbl` scope, ISO/IEC 14496-12 §8.7.8 / §8.7.9.
  - `parse_saiz(payload) -> Result<Saiz>` in the new `sample_aux`
    module. Layout per §8.7.8.2: `version[1]` (spec fixes at 0;
    unknown rejected) + `flags[3]` (low bit gates the
    discriminator pair; upper bits carried verbatim) + optional
    `aux_info_type[4]` + `aux_info_type_parameter[4]` (present iff
    `flags & 1`) + `default_sample_info_size[1]` + `sample_count[4]`
    + optional `sample_info_size[sample_count]` (present iff
    `default_sample_info_size == 0`). Rejected: payload < 4-byte
    FullBox header; version != 0; flags & 1 set with the pair
    absent; body shorter than the mandatory
    `default+sample_count` (5 bytes); per-sample table truncated
    below `sample_count`.
  - `parse_saio(payload) -> Result<Saio>` in the same module.
    Layout per §8.7.9.2: `version[1]` (spec defines v0 / v1) +
    `flags[3]` + optional discriminator pair (same gating as
    `saiz`) + `entry_count[4]` + offset table at the version's
    width (4 bytes per offset for v0, 8 bytes for v1). Rejected:
    payload < 4-byte FullBox header; version > 1; flags & 1 set
    with the pair absent; body shorter than `entry_count`;
    offset table truncated below the declared width × count;
    trailing bytes past the offset table (no padding by spec).
  - `Saiz { flags: u32, aux_info_type: Option<AuxInfoType>,
    default_sample_info_size: u8, sample_count: u32,
    sample_info_sizes: Vec<u8> }` with `size_for(sample_idx) ->
    Option<u32>` (honouring §8.7.8.3's "samples past sample_count
    have no auxiliary information" prefix rule) and `total_size()
    -> u64` (saturating sum, useful for chunk-scope integrity
    checks against `saio`'s offset chain).
  - `Saio { version: u8, flags: u32, aux_info_type:
    Option<AuxInfoType>, offsets: Vec<u64> }` with
    `is_single_chunk()` (§8.7.9.3's "all contiguous from the first
    offset" shortcut) and `offset_for(index) -> Option<u64>`.
  - `AuxInfoType { aux_info_type: [u8; 4], aux_info_type_parameter:
    u32 }` discriminator with `matches(&[u8; 4], u32) -> bool`.
    Boxes whose `flags & 1` bit is unset carry `aux_info_type:
    None`; §8.7.8.1's implicit-fallback rules (scheme_type for
    CENC-protected content, sample-entry type otherwise) are
    caller-side.
  - `SampleTable.saiz: Vec<Saiz>` + `SampleTable.saio: Vec<Saio>`
    fields, populated by `parse_stbl` from the new `saiz` / `saio`
    child handlers. §8.7.8.3 / §8.7.9.3 forbid duplicates for the
    same `(aux_info_type, aux_info_type_parameter)` pair within a
    single `stbl`; the walker silently drops duplicates first-wins
    (matches the `sbgp` / `sgpd` conservative-merge convention).
  - `SampleTable::sample_aux_for(aux_info_type,
    aux_info_type_parameter) -> (Option<&Saiz>, Option<&Saio>)`
    accessor. Boxes without an on-disk discriminator match against
    `(b"\0\0\0\0", 0)` via this surface, letting callers
    pre-resolve the implicit fallback before lookup.
  - `MovDemuxer::sample_aux_info(track, aux_info_type,
    aux_info_type_parameter) -> (Option<&Saiz>, Option<&Saio>)`
    public surface. Returns `(None, None)` for out-of-range tracks.
  - `SAIZ` / `SAIO` FourCC constants in `atom` module.
  - 14 in-module unit tests + 8 integration tests
    (`synth_round147_sample_aux.rs`) covering default-size and
    per-sample-size `saiz` round-trip through the full `stbl`
    walk, `saio` v0 and v1 64-bit offsets, the implicit
    discriminator matching `(b"\0\0\0\0", 0)`, multiple `(saiz,
    saio)` pairs distinguished by discriminator, duplicate
    first-wins on the same discriminator, malformed-box rejection
    at open time, and the "no `saiz`/`saio`" baseline.
  - `stbl`-scope only this round. The `traf`-scope form
    (fragmented-MP4 / CMAF / DASH live, §8.8 with `saiz` / `saio`
    inside `traf`) is deferred to a follow-up; the parser is
    container-agnostic so wiring it into the `traf` walker is the
    only remaining piece.
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
