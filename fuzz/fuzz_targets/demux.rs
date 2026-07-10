#![no_main]

//! Demux arbitrary fuzz-supplied bytes through the QuickTime File
//! Format (QTFF) / ISO Base Media File Format demuxer.
//!
//! The contract under test is purely that the calls *return*: a
//! malformed stream yields `Err(Error::…)`, a well-formed one yields
//! `Ok(_)` packets until `Error::Eof`, and neither path may panic,
//! abort, integer-overflow (in a debug build), index out of bounds,
//! or attempt an attacker-controlled `Vec::with_capacity` /
//! `vec![0; n]` allocation that exceeds what the input could
//! possibly back. Return values are intentionally discarded.
//!
//! The QTFF + ISO BMFF attack surface this exercises:
//!
//!   * The box-tree walker, which descends `moov > trak > mdia >
//!     minf > stbl > ...` and the parallel `moof > traf > trun`
//!     fragmented-MP4 tree, where every level is a
//!     `size:u32 / type:FourCC [/ largesize:u64]` length-prefixed
//!     container (QTFF p. 19 / ISO/IEC 14496-12 §4.2). `size:u32 = 0`
//!     ("to EOF") and `size:u32 = 1` ("largesize follows") are the
//!     two box sentinels that have historically defeated naive
//!     parsers. Round 162 hardened the top-level walker against any
//!     declared size that would extend past end-of-file; this target
//!     keeps that invariant exercised over the random-input space.
//!   * The `read_payload` allocation cap (64 MiB) — round 162 added
//!     a ceiling so a forged extended `size` of (say) 8 GiB on a
//!     1 KiB file errors at the allocation site before `vec![0u8;
//!     n as usize]` lands. The fuzz target sprays attacker-controlled
//!     size words at the walker so any unguarded `read_payload`
//!     call site would surface as an OOM or panic.
//!   * Sample-table expansion — `stts`, `stsc`, `stsz`/`stz2`,
//!     `stco`/`co64`, `stss`, `stsh`, `ctts`, `sdtp`, `stdp`, `padb`,
//!     `subs`, `sbgp`/`sgpd`, `saiz`/`saio` all have attacker-controlled
//!     entry counts that drive allocations and per-sample arithmetic.
//!   * Fragmented MP4 — `tfhd` per-track defaults, `tfdt` base media
//!     decode time, `trun` per-sample overrides, and the round-21
//!     `mfra/tfra/mfro` random-access index, all of which compose
//!     into the absolute file-offset arithmetic that locates each
//!     fragment's payload bytes.
//!   * Edit list (`edts/elst`) — signed `media_time` plus
//!     fixed-point `media_rate`, with segment durations in the
//!     (possibly zero) movie timescale. The round-74 / round-91
//!     mapper has to survive degenerate edits without panicking.
//!   * Sample-entry inner parsers — `avcC`, `hvcC`, `av1C`, `vpcC`,
//!     `dfLa`, `dOps`, `dac3`, `dec3`, and the BER-encoded `esds`
//!     descriptor chain, all walked under an outer
//!     `data_reference_index` the input chooses.
//!   * QTFF-specific atoms — Apple `udta` (`©nam`/`©ART`/…),
//!     `gmhd > gmin/text/tmcd`, `clip/crgn`, `matt/kmat`, `load`,
//!     `tapt/clef/prof/enof`, `ctab`, `pnot`, plus the
//!     chapter-text-track decoder (the only place this crate
//!     interprets sample *bytes*, not just sample-table offsets).
//!   * ISO BMFF-only file-scope boxes — `pdin`, `sidx`, `styp`,
//!     `prft`. Their parsers see arbitrary attacker-supplied counts
//!     and version words and must reject every malformed shape at
//!     open time.
//!   * Metadata — 3GPP `udta` boxes (`titl`/`auth`/…), iTunes-style
//!     `meta`/`keys`/`ilst` (whose `item > data` inner shape is
//!     itself a recursive box tree), `BmffMeta` `iprp`/`iloc`/`iref`
//!     graph traversal.
//!   * `seek_to(0, 0)` re-exercises the sample-table walker from a
//!     random offset, including the `tfra` binary search on
//!     fragmented inputs.
//!
//! `MovDemuxer::open` is the no-resolver entry point: a successful
//! open hands back a demuxer whose `next_packet` then walks every
//! sample / fragment. We cap the per-input packet count so a
//! pathological valid stream can't dominate fuzz time.
//!
//! Reference-movie alias resolution is intentionally NOT exercised
//! here — `open_with_aliases` would resolve `rmra/url ` chains
//! against a caller-supplied opener, and fuzz input controls the
//! URL string, so a too-permissive opener would either reach out to
//! the network or spin on file-system probes. The no-alias path
//! still walks every `rmra/rmda/rmdr/rmcs` parser, so the *parse*
//! side of the reference-movie surface is covered.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use oxideav_core::{Demuxer, ReadSeek};

use oxideav_mov::demuxer::MovDemuxer;

/// Bound on how many packets we drain per fuzz input. A pathological
/// but legitimate stream (e.g. a one-sample-per-chunk track with a
/// long `stsz` table) could otherwise spin the fuzzer on a single
/// many-packet track instead of exploring the input space.
const MAX_PACKETS_PER_INPUT: usize = 256;

fuzz_target!(|data: &[u8]| {
    // Skip trivially-short inputs — the smallest legal QTFF / ISO
    // BMFF file has at least an 8-byte `ftyp` box header (or a
    // legacy bare-`moov` 8-byte header), so anything shorter can't
    // even pass the outermost box read.
    if data.len() < 8 {
        return;
    }
    let rs: Box<dyn ReadSeek> = Box::new(Cursor::new(data.to_vec()));
    let Ok(mut dmx) = MovDemuxer::open(rs) else {
        return;
    };

    // Touch the metadata + streams accessors once. These are
    // populated entirely by the open() path but exercising the
    // accessors catches any post-open invariant the parser might
    // have left in an inconsistent state. The `Demuxer` impl's
    // `streams()` reaches into the `streams` field; the public
    // fields below reach into the file-scope structural surfaces.
    let _ = dmx.streams().len();
    let _ = dmx.meta.len();
    let _ = dmx.user_data.len();
    let ntracks = dmx.tracks.len();

    // Touch the round-162 / round-105 / round-114 / round-125 /
    // round-128 / round-137 / round-157 file-scope surfaces. Each
    // field is an immediate value populated by the open() path,
    // so the access is cheap; landing here at all asserts the
    // parser populated them in a consistent shape rather than
    // leaving the demuxer half-built. The `is_*` predicates also
    // walk the brand list once.
    let _ = dmx.pdin.is_some();
    let _ = dmx.sidx.len();
    // Round-219 Subsegment Index Box (`ssix`, ISO/IEC 14496-12 §8.16.4)
    // surface. The boxes pair one-to-one with `sidx` per §8.16.4.1; we
    // walk the public Vec (capped at 64 to bound pathological writers)
    // touching `total_size_for` / `partial_subsegment_offset` on a
    // couple of attacker-influenced indices, then exercise the
    // `ssix_for_sidx` cross-reference path against each declared
    // `sidx`. This keeps the deferred-pairing book-keeping covered
    // even when the fuzz input declares dozens of out-of-order or
    // orphan ssix entries.
    let nssix = dmx.ssix.len().min(64);
    for si in 0..nssix {
        let s = &dmx.ssix[si];
        let _ = s.subsegment_count();
        let probe = if data.len() >= 4 {
            u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize
        } else {
            0
        };
        let _ = s.total_size_for(probe);
        let _ = s.partial_subsegment_offset(0, probe, 0);
        let _ = s.partial_subsegment_offset(u64::MAX, 0, 0);
    }
    for si in 0..dmx.sidx.len().min(64) {
        let _ = dmx.ssix_for_sidx(si);
    }
    // Round-226 Level Assignment Box (`leva`, ISO/IEC 14496-12
    // §8.8.13) surface. Quantity is Zero or one and the box lives
    // inside `moov/mvex`; the parser already rejects malformed
    // shapes at open time, so reaching here means we have a parsed
    // `Leva`. Walk the row list (capped at 64 to bound a writer that
    // crammed the maximum 255 rows) touching the per-row accessor
    // surfaces (`level_count`, `level()`, `track_ids()`) plus the
    // 1-based `level()` boundaries (0 and `level_count`+1) so the
    // off-by-one path stays covered on every fuzz input that ships a
    // valid `leva`.
    if let Some(ref leva) = dmx.leva {
        let _ = leva.level_count();
        let nlevels = leva.levels.len().min(64);
        for li in 0..nlevels {
            let _ = leva.levels[li].track_id;
            let _ = leva.levels[li].padding_flag;
            let _ = leva.levels[li].assignment_type;
        }
        let _ = leva.level(0);
        let _ = leva.level(leva.level_count());
        let _ = leva.level(leva.level_count().saturating_add(1));
        let _ = leva.track_ids().len();
    }
    let _ = dmx.styp.len();
    let _ = dmx.prft.len();
    let _ = dmx.ctab.is_some();
    let _ = dmx.pnot.is_some();
    let _ = dmx.clipping.is_some();
    // Round-182 file-level `uuid` surface (ISO/IEC 14496-12 §4.2 /
    // §11.1). Each entry is independent so we walk them all, capping
    // at 64 to bound fuzz time on a pathological writer that emits
    // thousands of duplicate vendor headers. The `usertype_string`
    // call exercises the canonical RFC 4122 formatter; the
    // `is_iso_reserved_namespace` branch exercises the §11.1 escape-
    // pattern check on the attacker-supplied UUID prefix.
    let nuuids = dmx.file_uuids.len().min(64);
    for ui in 0..nuuids {
        let u = &dmx.file_uuids[ui];
        let _ = u.usertype_string();
        let _ = u.is_iso_reserved_namespace();
        let _ = u.iso_namespace_boxtype();
        let _ = u.payload.len();
    }
    let _ = dmx.ftyp.is_some();
    let _ = dmx.mvhd.is_some();
    let _ = dmx.is_fragmented();
    let _ = dmx.is_faststart();
    let _ = dmx.is_heic();
    let _ = dmx.is_avif();
    let _ = dmx.is_miaf();
    let _ = dmx.first_styp().is_some();
    let _ = dmx.first_prft().is_some();
    let _ = dmx.is_dash_segment();
    let _ = dmx.is_cmaf_segment();
    let _ = dmx.brand_class().len();
    let _ = dmx.alternate_groups().len();
    let _ = dmx.switch_groups().len();
    // Round-199 file-level `trgr` aggregate surface (ISO/IEC 14496-12
    // §8.3.4). The bucket map walks every track's per-trak entry list
    // once; per-bucket dedup keeps it bounded. Touch the dual-lookup
    // path with an attacker-style key derived from the input's first
    // four bytes so the `tracks_in_group` matcher is exercised against
    // a random-but-not-fully-empty FourCC.
    let tgroups = dmx.track_groups();
    let _ = tgroups.len();
    if data.len() >= 4 {
        let mut probe_type = [0u8; 4];
        probe_type.copy_from_slice(&data[..4]);
        let _ = dmx.tracks_in_group(probe_type, 0).len();
    }
    let _ = dmx.tracks_in_group(*b"msrc", 1).len();

    // Per-track accessor sweep. We touch every track but cap the
    // count so a pathological `mvhd` with thousands of `trak`
    // entries can't dominate fuzz time. The accessor calls
    // exercise the round-89 / round-95 / round-122 typed-lookup
    // paths plus the round-74 edit-segment surface.
    let ntracks = ntracks.min(64);
    for ti in 0..ntracks {
        let _ = dmx.track_load(ti);
        let _ = dmx.track_selection(ti);
        let _ = dmx.track_kinds(ti);
        // Round-216 Track Input Map atom (`imap`, QTFF pp. 51-53)
        // accessor — surfaces the parsed `' in'` entries when the
        // track carries an `imap` child of `trak`. The accessor walks
        // QT-style track input atoms with their own ` ty` and
        // optional `obid` children; exercise the slot lookup path on
        // a small attacker-influenced atom id so the sparse list
        // search stays covered on an `imap`-carrying fuzz input.
        if let Some(imap) = dmx.track_input_map(ti) {
            for e in imap.entries.iter().take(16) {
                let _ = e.atom_id;
                let _ = e.input_type.kind;
                let _ = e.object_id;
            }
            let probe_slot = if data.len() >= 4 {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]])
            } else {
                1
            };
            let _ = imap.entry_for_ssrc_slot(probe_slot);
        }
        // Round-407 sound sample-description v2 + extension-atom
        // surfaces (QTFF 2012-08-14 pp. 181–187). The v2 Float64
        // sample rate, LPCM flag word and sizeOfStructOnly-driven
        // extension scan all consume attacker-controlled bytes at
        // open time; the typed accessors below must hold on any
        // parse that survived. `parse_wave` must be idempotent
        // through its own serialiser (children / terminator /
        // non-atom fallback all round-trip bit-exact).
        if let Some(track) = dmx.tracks.get(ti) {
            for sd in track.sample_descriptions.iter().take(8) {
                let _ = sd.audio_sample_rate_hz();
                let _ = sd.is_vbr();
                if let Some(v2) = sd.sound_v2 {
                    let _ = v2.format_specific_flags.is_float();
                    let _ = v2.format_specific_flags.sample_fraction();
                    let _ = v2.size_of_struct_only;
                }
                if let Some(w) = &sd.si_decompression_param {
                    let _ = w.format();
                    let _ = w.esds();
                    let reparsed = oxideav_mov::parse_wave(&w.to_payload_bytes());
                    assert_eq!(&reparsed, w, "wave serialise/parse not idempotent");
                }
                let _ = sd.esds.as_deref();
                let _ = sd.slope_and_intercept;
                let _ = sd.extension_terminator;
            }
        }
        // Round-199 per-track Track Group Box (`trgr`) entry list.
        let entries = dmx.track_group_entries(ti);
        let _ = entries.len();
        for e in entries.iter().take(16) {
            let _ = e.key();
            let _ = e.is_msrc();
            let _ = e.payload.len();
        }
        let _ = dmx.edit_segments_for(ti);
        let _ = dmx.random_access_points(ti).len();
        // Round-204 sample-size-source discriminator (ISO/IEC 14496-12
        // §8.7.3). Exercises the `stsz` / `stz2` enum surface so an
        // attacker-supplied stbl with both boxes, neither box, or a
        // pathological field_size value reaches the accessor without
        // panicking. The discriminator is `Option<SampleSizeSource>`;
        // the `Some(Stz2 { field_size })` path leaks the on-disk
        // field width so a fuzzer-generated stz2 surfaces here.
        let _ = dmx.sample_size_source(ti);
        // Round-210 Degradation Priority Box (`stdp`, §8.5.3)
        // per-sample accessor. The table is sized from `stsz`/`stz2`
        // so a fuzz input that constructs a runt sample-size box
        // alongside an oversize stdp must reach the deferred parse
        // without panicking. Probe a handful of indices, including
        // zero and a value derived from the input bytes so the
        // bounded `Vec::get` path stays exercised on every input.
        let _ = dmx.sample_degradation_priority(ti, 0);
        if data.len() >= 4 {
            let probe = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let _ = dmx.sample_degradation_priority(ti, probe);
        }
        // Round-234 Padding Bits Box (`padb`, §8.7.6) per-sample
        // accessor. The box carries its own `sample_count` field so
        // a malformed writer can declare a count that disagrees with
        // `stsz`/`stz2`; the bounded `Vec::get` in the accessor must
        // survive any sample index attackers reach for. Probe zero
        // plus a value derived from the input bytes.
        let _ = dmx.sample_padding_bits(ti, 0);
        if data.len() >= 8 {
            let probe = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            let _ = dmx.sample_padding_bits(ti, probe);
        }
        // Exercise the round-74 / round-91 edit-list mapper on a
        // couple of attacker-influenced media_pts values. The
        // mapper has to survive any value, including i64::MIN /
        // MAX, without panicking on the fixed-point math.
        let _ = dmx.movie_pts_for(ti, 0);
        let _ = dmx.movie_pts_for(ti, i64::MIN);
        let _ = dmx.movie_pts_for(ti, i64::MAX);
        // Round 246 inverse mapper: probe boundary values plus a
        // value derived from the input bytes. The mapper must
        // survive every i64 input on the fixed-point math without
        // panicking, just like the forward direction.
        let _ = dmx.media_pts_for(ti, 0);
        let _ = dmx.media_pts_for(ti, i64::MIN);
        let _ = dmx.media_pts_for(ti, i64::MAX);
        if data.len() >= 16 {
            let probe = i64::from_le_bytes([
                data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
            ]);
            let _ = dmx.media_pts_for(ti, probe);
        }
        // Round 256 chunk-walking primitives. The accessors decode
        // QTFF p. 75 `stsc` rows against the p. 78 `stco` / `co64`
        // chunk-offset table and the p. 76 `stsz` sample-size box;
        // an attacker-supplied combination (rows whose
        // `samples_per_chunk == 0`, chunk-offset shorter than the
        // last row's first_chunk, summed offsets overflowing u64) is
        // the surface they must survive without panicking. Probe
        // chunk-count zero plus an attacker-derived chunk-number,
        // and the same for the sample-keyed accessors.
        let cc = dmx.chunk_count(ti).unwrap_or(0);
        let _ = dmx.samples_in_chunk(ti, 0);
        let _ = dmx.samples_in_chunk(ti, 1);
        let _ = dmx.chunk_for_sample(ti, 0);
        let _ = dmx.sample_offset(ti, 0);
        let _ = dmx.chunk_byte_extent(ti, 0);
        let _ = dmx.chunk_byte_extent(ti, 1);
        if cc > 0 {
            let _ = dmx.chunk_byte_extent(ti, cc);
            // Beyond the chunk-offset table — accessor must return None
            // rather than panic on the index-out-of-range path.
            let _ = dmx.chunk_byte_extent(ti, cc.saturating_add(1));
            let _ = dmx.samples_in_chunk(ti, cc);
            let _ = dmx.samples_in_chunk(ti, cc.saturating_add(1));
        }
        if data.len() >= 4 {
            let probe_chunk = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let _ = dmx.samples_in_chunk(ti, probe_chunk);
            let _ = dmx.chunk_byte_extent(ti, probe_chunk);
        }
        if data.len() >= 8 {
            let probe_sample = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            let _ = dmx.chunk_for_sample(ti, probe_sample);
            let _ = dmx.sample_offset(ti, probe_sample);
        }
    }

    // Drain packets up to MAX_PACKETS_PER_INPUT. The loop terminates
    // on the first error (Eof, invalid, ...) — fuzz inputs are
    // expected to crash the sample-table walker more often than they
    // demux cleanly, so a bounded loop is plenty.
    for _ in 0..MAX_PACKETS_PER_INPUT {
        if dmx.next_packet().is_err() {
            break;
        }
    }

    // Re-exercise the seek path. seek_to(0, 0) is the cheapest
    // possible call — it lands on the first sync sample of stream 0
    // (if any) — and runs the `stss` / `tfra` / sample-offset
    // machinery from a random offset. If the file had no streams
    // this returns Err; that's fine.
    let _ = dmx.seek_to(0, 0);

    // Round-394 applied edit-list surface. Flip the demuxer onto the
    // edited timeline and drain a second bounded packet run: the
    // per-sample `edited_timing_for_sample` mapper now sits on the
    // `next_packet` hot path and must survive attacker-controlled
    // edit lists (zero timescales, dwells, rates near i32::MIN/MAX,
    // segment windows that overflow the movie duration) for every
    // emitted sample. The edited seek resolver
    // (`edited_pts_to_media_pts`) is probed at both boundary values
    // and an input-derived point before re-running `seek_to`, whose
    // input is now an edited-timeline timestamp.
    dmx.apply_edit_lists(true);
    let _ = dmx.edit_lists_applied();
    for ti in 0..ntracks {
        let _ = dmx.edited_pts_to_media_pts(ti, 0);
        let _ = dmx.edited_pts_to_media_pts(ti, i64::MIN);
        let _ = dmx.edited_pts_to_media_pts(ti, i64::MAX);
        if data.len() >= 16 {
            let probe = i64::from_le_bytes([
                data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
            ]);
            let _ = dmx.edited_pts_to_media_pts(ti, probe);
        }
    }
    let _ = dmx.seek_to(0, 0);
    for _ in 0..MAX_PACKETS_PER_INPUT {
        if dmx.next_packet().is_err() {
            break;
        }
    }
});
