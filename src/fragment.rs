//! ISO BMFF movie-fragment atoms (§8.8 of ISO/IEC 14496-12:2015).
//!
//! Fragmented MP4 (and fragmented `qt  `, since the wire-shape is
//! identical) extend the presentation past whatever is described in
//! `moov` by emitting *top-level* `moof` atoms after the initial
//! `moov`. Each `moof` carries:
//!
//! ```text
//! moof
//! ├── mfhd                   sequence_number              §8.8.5
//! └── traf (one per track)                                §8.8.6
//!     ├── tfhd               track-fragment defaults      §8.8.7
//!     ├── trun (zero+)       track-run sample table       §8.8.8
//!     └── (sdtp / saio / sbgp / subs — not consumed here)
//! ```
//!
//! `mvex` inside `moov` provides per-track *fragment defaults* via
//! `trex` (§8.8.3): a default sample-description-index, sample
//! duration, sample size, and sample flags. `tfhd` overrides each of
//! those on a per-fragment basis; `trun` overrides each of *those* on
//! a per-sample basis. The cascade is exactly:
//!
//! ```text
//! per-sample (trun)  >  per-fragment (tfhd)  >  per-track (trex)
//! ```
//!
//! Round 18 wires the cascade end-to-end so each `moof`'s samples
//! land in the right track's flat sample queue. The QuickTime
//! demuxer was previously refusing fragmented input with an
//! `Unsupported` error; this round flips the rejection into a real
//! decode path.
//!
//! Spec references throughout this module are to ISO/IEC 14496-12:2015
//! unless otherwise noted; the QTFF spec does not define `mvex` /
//! `moof` / `traf`, so the parser falls back to ISO BMFF semantics
//! whenever it sees those atoms.

use std::io::{Read, Seek, SeekFrom};

use crate::atom::{
    read_payload, walk_children, AtomHeader, LEVA, MEHD, MFRO, SAIO, SAIZ, TFRA, TRAF, TREX,
};
use crate::leva::{parse_leva, Leva};
use crate::sample_aux::{parse_saio, parse_saiz, Saio, Saiz};
use crate::sample_table::SampleEntry;

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

// ─────────────── tfhd flag bits (§8.8.7.1) ───────────────

/// `tfhd` flag: explicit `base_data_offset` field is present.
pub const TFHD_BASE_DATA_OFFSET_PRESENT: u32 = 0x000001;
/// `tfhd` flag: `sample_description_index` field is present.
pub const TFHD_SAMPLE_DESCRIPTION_INDEX_PRESENT: u32 = 0x000002;
/// `tfhd` flag: `default_sample_duration` field is present.
pub const TFHD_DEFAULT_SAMPLE_DURATION_PRESENT: u32 = 0x000008;
/// `tfhd` flag: `default_sample_size` field is present.
pub const TFHD_DEFAULT_SAMPLE_SIZE_PRESENT: u32 = 0x000010;
/// `tfhd` flag: `default_sample_flags` field is present.
pub const TFHD_DEFAULT_SAMPLE_FLAGS_PRESENT: u32 = 0x000020;
/// `tfhd` flag: the fragment carries no samples (`duration_is_empty`).
pub const TFHD_DURATION_IS_EMPTY: u32 = 0x010000;
/// `tfhd` flag: when `base_data_offset_present == 0`, anchor the
/// per-fragment offset at the start of the enclosing `moof` rather
/// than at the end of the previous `traf`. Required under `iso5`.
pub const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x020000;

// ─────────────── trun flag bits (§8.8.8.1) ───────────────

/// `trun` flag: `data_offset` field is present.
pub const TRUN_DATA_OFFSET_PRESENT: u32 = 0x000001;
/// `trun` flag: `first_sample_flags` field is present (overrides
/// the default flags for the run's first sample only).
pub const TRUN_FIRST_SAMPLE_FLAGS_PRESENT: u32 = 0x000004;
/// `trun` flag: each sample carries its own duration.
pub const TRUN_SAMPLE_DURATION_PRESENT: u32 = 0x000100;
/// `trun` flag: each sample carries its own size.
pub const TRUN_SAMPLE_SIZE_PRESENT: u32 = 0x000200;
/// `trun` flag: each sample carries its own flags.
pub const TRUN_SAMPLE_FLAGS_PRESENT: u32 = 0x000400;
/// `trun` flag: each sample carries a composition-time offset.
pub const TRUN_SAMPLE_CTS_OFFSET_PRESENT: u32 = 0x000800;

// ─────────────── parsed records ───────────────

/// `trex` per-track defaults for fragmented playback (§8.8.3).
///
/// One record per track, mandatory inside `mvex` when fragments
/// exist. The defaults are consumed by `tfhd` and `trun` whenever
/// those carriers omit the matching field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrexDefaults {
    /// Track this record applies to (`tkhd.track_id`).
    pub track_id: u32,
    /// 1-based index into `stbl/stsd` used when `tfhd` does not
    /// specify one.
    pub default_sample_description_index: u32,
    /// Default per-sample duration (in media-timescale ticks).
    pub default_sample_duration: u32,
    /// Default per-sample size (in bytes).
    pub default_sample_size: u32,
    /// Default packed sample-flags word (sync / dependency / leading
    /// bits — see ISO/IEC 14496-12 §8.8.3.1 "sample flags field").
    pub default_sample_flags: u32,
}

impl TrexDefaults {
    /// `default_sample_flags`'s `sample_is_non_sync_sample` bit clear
    /// → the sample is implicitly a sync sample. Used by `trun` when
    /// it omits per-sample flags.
    pub fn default_is_sync(&self) -> bool {
        sample_flags_is_sync(self.default_sample_flags)
    }
}

/// `mfhd` parsed payload (§8.8.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mfhd {
    /// Per-fragment sequence number (§8.8.5.3). Readers may verify
    /// integrity by checking that the value increments
    /// monotonically across the `moof` stream.
    pub sequence_number: u32,
}

/// Parsed `mehd` payload (§8.8.2.2). Optional; gives the total
/// fragmented-movie duration in `mvhd.time_scale` ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mehd {
    /// `fragment_duration` per §8.8.2.3 — total length of the
    /// fragmented presentation. 64-bit-wide regardless of the on-disk
    /// version field; the v0 32-bit encoding is widened.
    pub fragment_duration: u64,
}

/// `tfhd` parsed payload (§8.8.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tfhd {
    /// `tf_flags` byte triple (the lower 24 bits of the FullBox
    /// `[ver+flags]` word). The bits are documented as the `TFHD_*`
    /// constants on this module.
    pub tf_flags: u32,
    /// `track_ID` per §8.8.7.2 (matches `tkhd.track_id`).
    pub track_id: u32,
    /// Optional `base_data_offset` field; `None` unless
    /// `TFHD_BASE_DATA_OFFSET_PRESENT` is set.
    pub base_data_offset: Option<u64>,
    /// Optional `sample_description_index` — 1-based override.
    pub sample_description_index: Option<u32>,
    /// Optional per-fragment default sample duration.
    pub default_sample_duration: Option<u32>,
    /// Optional per-fragment default sample size.
    pub default_sample_size: Option<u32>,
    /// Optional per-fragment default sample flags.
    pub default_sample_flags: Option<u32>,
}

/// Per-sample row inside a `trun` (§8.8.8.2). Each field is
/// optional at the box level; when absent we fall back to the
/// `tfhd` default, then the `trex` default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrunSample {
    /// Per-sample duration when `TRUN_SAMPLE_DURATION_PRESENT`.
    pub sample_duration: Option<u32>,
    /// Per-sample byte size when `TRUN_SAMPLE_SIZE_PRESENT`.
    pub sample_size: Option<u32>,
    /// Per-sample flags word when `TRUN_SAMPLE_FLAGS_PRESENT`.
    pub sample_flags: Option<u32>,
    /// Per-sample composition-time offset when
    /// `TRUN_SAMPLE_CTS_OFFSET_PRESENT`. The on-disk encoding is
    /// version-dependent (v0 unsigned, v1 signed) — we always
    /// store the signed view.
    pub sample_cts_offset: Option<i32>,
}

/// `trun` parsed payload (§8.8.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trun {
    /// FullBox `version` (0 = unsigned `sample_cts_offset`; 1 =
    /// signed). The semantic of unsigned-vs-signed mirrors the
    /// `ctts` layout — see ISO/IEC 14496-12 §8.6.1.3.2.
    pub version: u8,
    /// `tr_flags` (lower 24 bits of `[ver+flags]`).
    pub tr_flags: u32,
    /// `data_offset` — when present, applied on top of the `tfhd`'s
    /// implicit-or-explicit base offset. Signed per §8.8.8.2.
    pub data_offset: Option<i32>,
    /// `first_sample_flags` — overrides the per-fragment / per-track
    /// default flags for the run's *first* sample only.
    pub first_sample_flags: Option<u32>,
    /// Per-sample rows. Length is the `sample_count` from §8.8.8.2.
    pub samples: Vec<TrunSample>,
}

// ─────────────── parsers ───────────────

/// Parse the 8-byte `mfhd` payload per §8.8.5.2.
pub fn parse_mfhd(payload: &[u8]) -> Result<Mfhd> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: mfhd payload < 8 bytes"));
    }
    // [ver+flags:4][sequence_number:4]
    let sequence_number = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    Ok(Mfhd { sequence_number })
}

/// Parse a `mehd` payload (§8.8.2.2). Accepts version 0 (32-bit
/// `fragment_duration`) and version 1 (64-bit).
pub fn parse_mehd(payload: &[u8]) -> Result<Mehd> {
    if payload.len() < 4 {
        return Err(Error::invalid("MOV: mehd payload < 4 bytes"));
    }
    let version = payload[0];
    let fragment_duration = match version {
        0 => {
            if payload.len() < 8 {
                return Err(Error::invalid("MOV: mehd v0 payload < 8 bytes"));
            }
            u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]) as u64
        }
        1 => {
            if payload.len() < 12 {
                return Err(Error::invalid("MOV: mehd v1 payload < 12 bytes"));
            }
            u64::from_be_bytes([
                payload[4],
                payload[5],
                payload[6],
                payload[7],
                payload[8],
                payload[9],
                payload[10],
                payload[11],
            ])
        }
        _ => return Err(Error::invalid("MOV: mehd unsupported version")),
    };
    Ok(Mehd { fragment_duration })
}

/// Parse a single `trex` record per §8.8.3.2.
pub fn parse_trex(payload: &[u8]) -> Result<TrexDefaults> {
    if payload.len() < 24 {
        return Err(Error::invalid("MOV: trex payload < 24 bytes"));
    }
    // [ver+flags:4][track_ID:4][def_sdi:4][def_dur:4][def_sz:4][def_flags:4]
    let track_id = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let default_sample_description_index =
        u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let default_sample_duration =
        u32::from_be_bytes([payload[12], payload[13], payload[14], payload[15]]);
    let default_sample_size =
        u32::from_be_bytes([payload[16], payload[17], payload[18], payload[19]]);
    let default_sample_flags =
        u32::from_be_bytes([payload[20], payload[21], payload[22], payload[23]]);
    Ok(TrexDefaults {
        track_id,
        default_sample_description_index,
        default_sample_duration,
        default_sample_size,
        default_sample_flags,
    })
}

/// Walk a `mvex` container, returning the parsed `mehd` (optional),
/// the per-track `trex` defaults (one per fragmented track), and the
/// optional `leva` (Level Assignment Box) introduced by §8.8.13.
///
/// Unknown children are ignored — derived ISO BMFF specs occasionally
/// add new sub-atoms inside `mvex`; the round-18 demuxer only needs
/// `mehd` + `trex` to walk the fragments, while round 226 surfaces
/// the optional `leva` for callers pairing it with §8.16.4 `ssix`.
///
/// `leva` is `Quantity: Zero or one` per §8.8.13.1; a malformed
/// writer emitting two `leva` boxes inside one `mvex` is tolerated
/// first-wins (matching the `ctab` / `clip` / `pdin` first-wins
/// conservative-merge policy).
pub fn parse_mvex<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
) -> Result<(Option<Mehd>, Vec<TrexDefaults>, Option<Leva>)> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut mehd: Option<Mehd> = None;
    let mut trex: Vec<TrexDefaults> = Vec::new();
    let mut leva: Option<Leva> = None;
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &MEHD => {
                let body = read_payload(r, child)?;
                mehd = Some(parse_mehd(&body)?);
            }
            t if t == &TREX => {
                let body = read_payload(r, child)?;
                trex.push(parse_trex(&body)?);
            }
            t if t == &LEVA => {
                let body = read_payload(r, child)?;
                let parsed = parse_leva(&body)?;
                // First-wins on duplicate `leva`: §8.8.13.1 fixes
                // Quantity at Zero or one, and ignoring the rest
                // matches the conservative-merge policy applied to
                // other singletons.
                if leva.is_none() {
                    leva = Some(parsed);
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok((mehd, trex, leva))
}

/// Parse a `tfhd` payload per §8.8.7.2. The optional fields are
/// gated by `tf_flags` bits and consumed in declaration order.
pub fn parse_tfhd(payload: &[u8]) -> Result<Tfhd> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: tfhd payload < 8 bytes"));
    }
    // [ver+flags:4][track_ID:4]([base_data_offset:8])?([sample_description_index:4])?
    // ([default_sample_duration:4])?([default_sample_size:4])?
    // ([default_sample_flags:4])?
    let ver_flags = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let tf_flags = ver_flags & 0x00FF_FFFF;
    let track_id = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let mut off = 8usize;
    let base_data_offset = if tf_flags & TFHD_BASE_DATA_OFFSET_PRESENT != 0 {
        if off + 8 > payload.len() {
            return Err(Error::invalid("MOV: tfhd base_data_offset truncated"));
        }
        let v = u64::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
            payload[off + 4],
            payload[off + 5],
            payload[off + 6],
            payload[off + 7],
        ]);
        off += 8;
        Some(v)
    } else {
        None
    };
    let sample_description_index = if tf_flags & TFHD_SAMPLE_DESCRIPTION_INDEX_PRESENT != 0 {
        if off + 4 > payload.len() {
            return Err(Error::invalid(
                "MOV: tfhd sample_description_index truncated",
            ));
        }
        let v = u32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
        off += 4;
        Some(v)
    } else {
        None
    };
    let default_sample_duration = if tf_flags & TFHD_DEFAULT_SAMPLE_DURATION_PRESENT != 0 {
        if off + 4 > payload.len() {
            return Err(Error::invalid(
                "MOV: tfhd default_sample_duration truncated",
            ));
        }
        let v = u32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
        off += 4;
        Some(v)
    } else {
        None
    };
    let default_sample_size = if tf_flags & TFHD_DEFAULT_SAMPLE_SIZE_PRESENT != 0 {
        if off + 4 > payload.len() {
            return Err(Error::invalid("MOV: tfhd default_sample_size truncated"));
        }
        let v = u32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
        off += 4;
        Some(v)
    } else {
        None
    };
    let default_sample_flags = if tf_flags & TFHD_DEFAULT_SAMPLE_FLAGS_PRESENT != 0 {
        if off + 4 > payload.len() {
            return Err(Error::invalid("MOV: tfhd default_sample_flags truncated"));
        }
        let v = u32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
        // off += 4; // last field — no further consumers
        Some(v)
    } else {
        None
    };
    Ok(Tfhd {
        tf_flags,
        track_id,
        base_data_offset,
        sample_description_index,
        default_sample_duration,
        default_sample_size,
        default_sample_flags,
    })
}

/// Parse a `trun` payload per §8.8.8.2. The per-sample row size is
/// determined by the count of bits set in the upper byte of
/// `tr_flags` (each present field is a single `u32`).
pub fn parse_trun(payload: &[u8]) -> Result<Trun> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: trun payload < 8 bytes"));
    }
    let version = payload[0];
    let tr_flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
    let sample_count = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let mut off = 8usize;
    let data_offset = if tr_flags & TRUN_DATA_OFFSET_PRESENT != 0 {
        if off + 4 > payload.len() {
            return Err(Error::invalid("MOV: trun data_offset truncated"));
        }
        let v = i32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
        off += 4;
        Some(v)
    } else {
        None
    };
    let first_sample_flags = if tr_flags & TRUN_FIRST_SAMPLE_FLAGS_PRESENT != 0 {
        if off + 4 > payload.len() {
            return Err(Error::invalid("MOV: trun first_sample_flags truncated"));
        }
        let v = u32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
        off += 4;
        Some(v)
    } else {
        None
    };
    // Compute the per-sample row size up front: one u32 per present
    // optional. The `sample_cts_offset` is also a u32 (signed for v1,
    // unsigned for v0) — same width either way.
    let want_dur = tr_flags & TRUN_SAMPLE_DURATION_PRESENT != 0;
    let want_sz = tr_flags & TRUN_SAMPLE_SIZE_PRESENT != 0;
    let want_flags = tr_flags & TRUN_SAMPLE_FLAGS_PRESENT != 0;
    let want_cts = tr_flags & TRUN_SAMPLE_CTS_OFFSET_PRESENT != 0;
    let row_size =
        (want_dur as usize + want_sz as usize + want_flags as usize + want_cts as usize) * 4;
    if row_size
        .checked_mul(sample_count as usize)
        .and_then(|t| off.checked_add(t))
        .map(|tot| tot > payload.len())
        .unwrap_or(true)
    {
        return Err(Error::invalid("MOV: trun per-sample rows truncated"));
    }
    let mut samples = Vec::with_capacity(sample_count as usize);
    for _ in 0..sample_count {
        let mut s = TrunSample {
            sample_duration: None,
            sample_size: None,
            sample_flags: None,
            sample_cts_offset: None,
        };
        if want_dur {
            s.sample_duration = Some(u32::from_be_bytes([
                payload[off],
                payload[off + 1],
                payload[off + 2],
                payload[off + 3],
            ]));
            off += 4;
        }
        if want_sz {
            s.sample_size = Some(u32::from_be_bytes([
                payload[off],
                payload[off + 1],
                payload[off + 2],
                payload[off + 3],
            ]));
            off += 4;
        }
        if want_flags {
            s.sample_flags = Some(u32::from_be_bytes([
                payload[off],
                payload[off + 1],
                payload[off + 2],
                payload[off + 3],
            ]));
            off += 4;
        }
        if want_cts {
            let raw = u32::from_be_bytes([
                payload[off],
                payload[off + 1],
                payload[off + 2],
                payload[off + 3],
            ]);
            s.sample_cts_offset = Some(if version == 0 {
                // Unsigned in v0 — store the bit pattern as i32. Per
                // §8.8.8.3 the unsigned encoding is legacy; values
                // > i32::MAX would wrap, but real-world writers stay
                // well under that.
                raw as i32
            } else {
                raw as i32
            });
            off += 4;
        }
        samples.push(s);
    }
    Ok(Trun {
        version,
        tr_flags,
        data_offset,
        first_sample_flags,
        samples,
    })
}

/// Walk a `traf` container, collecting the single `tfhd`, any
/// number of `trun`s, the optional `tfdt`
/// (Track Fragment Decode Time, §8.8.12), and any `saiz` / `saio`
/// (Sample Auxiliary Information Sizes / Offsets, §8.7.8 / §8.7.9)
/// children. The order on the wire is `[tfhd][tfdt?][trun ...]` per
/// §8.8.6.2; the walker does not enforce ordering — derived specs add
/// sibling boxes (`sdtp` / `subs` / `sbgp`) that the round-18 demuxer
/// ignores. Sample-aux boxes are §8.7.8.1 / §8.7.9.1 allowed in both
/// `stbl` (non-fragmented) and `traf` (fragmented) scope; we surface
/// them here so the demuxer can route CMAF / CENC per-fragment
/// sample-aux to a fragmented accessor that mirrors the `stbl`-scope
/// [`crate::MovDemuxer::sample_aux_info`] path.
pub fn parse_traf<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<TrafParse> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut tfhd: Option<Tfhd> = None;
    let mut tfdt: Option<u64> = None;
    let mut truns: Vec<Trun> = Vec::new();
    let mut saiz: Vec<Saiz> = Vec::new();
    let mut saio: Vec<Saio> = Vec::new();
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            b"tfhd" => {
                let body = read_payload(r, child)?;
                tfhd = Some(parse_tfhd(&body)?);
            }
            b"tfdt" => {
                let body = read_payload(r, child)?;
                tfdt = Some(parse_tfdt(&body)?);
            }
            b"trun" => {
                let body = read_payload(r, child)?;
                truns.push(parse_trun(&body)?);
            }
            t if t == &SAIZ => {
                // §8.7.8.3 — at most one `saiz` per (aux_info_type,
                // aux_info_type_parameter) per containing box; first
                // wins on duplicates, matching the `stbl`-scope merge
                // policy in `demuxer::parse_stbl`.
                let body = read_payload(r, child)?;
                let s = parse_saiz(&body)?;
                if !saiz.iter().any(|x| x.aux_info_type == s.aux_info_type) {
                    saiz.push(s);
                }
            }
            t if t == &SAIO => {
                // §8.7.9.3 — at most one `saio` per (aux_info_type,
                // aux_info_type_parameter) per containing box; first
                // wins on duplicates.
                let body = read_payload(r, child)?;
                let s = parse_saio(&body)?;
                if !saio.iter().any(|x| x.aux_info_type == s.aux_info_type) {
                    saio.push(s);
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(TrafParse {
        tfhd,
        tfdt,
        truns,
        saiz,
        saio,
    })
}

/// Output of [`parse_traf`]. Carries the rich `traf` body — the
/// `tfhd` defaults, the optional `tfdt`, the run table, and any
/// `saiz` / `saio` sample-auxiliary-information boxes living at
/// `traf` scope (§8.7.8.1 / §8.7.9.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafParse {
    /// Mandatory `tfhd` (§8.8.7) — only `None` when the `traf` is
    /// malformed (the demuxer treats `None` as "skip this fragment").
    pub tfhd: Option<Tfhd>,
    /// Optional `tfdt` `baseMediaDecodeTime` per §8.8.12.
    pub tfdt: Option<u64>,
    /// Zero or more `trun` runs in declaration order.
    pub truns: Vec<Trun>,
    /// `saiz` boxes at `traf` scope, in declaration order. Empty
    /// when the fragment carries no sample-auxiliary-information
    /// sizes. At most one per `(aux_info_type,
    /// aux_info_type_parameter)` pair per §8.7.8.3.
    pub saiz: Vec<Saiz>,
    /// `saio` boxes at `traf` scope, in declaration order. Empty
    /// when the fragment carries no sample-auxiliary-information
    /// offsets. At most one per `(aux_info_type,
    /// aux_info_type_parameter)` pair per §8.7.9.3.
    pub saio: Vec<Saio>,
}

/// Parse a `tfdt` payload per ISO/IEC 14496-12 §8.8.12.2.
///
/// On-disk layout:
/// ```text
/// [ver+flags:4]
/// version == 0 → [baseMediaDecodeTime:4]
/// version == 1 → [baseMediaDecodeTime:8]
/// ```
///
/// Returns the absolute decode-time of the *first* sample of the
/// fragment in the enclosing track's `mdhd.time_scale`. Threaded
/// into [`resolve_traf_samples`] as the `track_media_time` argument
/// so per-fragment DTS climbs from the writer-supplied baseline
/// rather than from a re-zeroed cursor.
pub fn parse_tfdt(payload: &[u8]) -> Result<u64> {
    if payload.len() < 4 {
        return Err(Error::invalid("MOV: tfdt payload < 4 bytes"));
    }
    let version = payload[0];
    match version {
        0 => {
            if payload.len() < 8 {
                return Err(Error::invalid("MOV: tfdt v0 payload < 8 bytes"));
            }
            Ok(u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]) as u64)
        }
        1 => {
            if payload.len() < 12 {
                return Err(Error::invalid("MOV: tfdt v1 payload < 12 bytes"));
            }
            Ok(u64::from_be_bytes([
                payload[4],
                payload[5],
                payload[6],
                payload[7],
                payload[8],
                payload[9],
                payload[10],
                payload[11],
            ]))
        }
        _ => Err(Error::invalid("MOV: tfdt unsupported version")),
    }
}

/// Parse a `moof` container, returning the parsed `mfhd` and one
/// `(tfhd, truns)` tuple per `traf` child.
///
/// The `moof_offset` argument is the absolute byte position of the
/// enclosing `moof`'s **start** (i.e. the position of its size word,
/// not its payload). Some `tfhd` shapes anchor their data offsets at
/// the start of the `moof` (`default-base-is-moof`); callers thread
/// this offset back into [`resolve_traf_samples`] alongside the
/// parsed records.
pub fn parse_moof<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
) -> Result<(Option<Mfhd>, Vec<TrafRecord>)> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut mfhd: Option<Mfhd> = None;
    let mut trafs: Vec<TrafRecord> = Vec::new();
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            b"mfhd" => {
                let body = read_payload(r, child)?;
                mfhd = Some(parse_mfhd(&body)?);
            }
            t if t == &TRAF => {
                let parsed = parse_traf(r, child)?;
                if let Some(tfhd) = parsed.tfhd {
                    trafs.push(TrafRecord {
                        tfhd,
                        tfdt: parsed.tfdt,
                        truns: parsed.truns,
                        saiz: parsed.saiz,
                        saio: parsed.saio,
                    });
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok((mfhd, trafs))
}

/// One parsed `traf` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafRecord {
    /// The mandatory `tfhd` defaults.
    pub tfhd: Tfhd,
    /// Optional `tfdt` (Track Fragment Decode Time, §8.8.12). When
    /// present, supplies the absolute `baseMediaDecodeTime` of the
    /// first sample in this fragment in `mdhd.time_scale` ticks; the
    /// demuxer threads it into [`resolve_traf_samples`] as
    /// `track_media_time` so per-fragment DTS climbs from the
    /// writer-supplied baseline rather than a re-zeroed cursor.
    pub tfdt: Option<u64>,
    /// Zero or more `trun` runs in declaration order.
    pub truns: Vec<Trun>,
    /// `saiz` Sample Auxiliary Information Sizes Boxes (ISO/IEC
    /// 14496-12 §8.7.8) declared at this `traf`'s scope. Empty when
    /// the fragment carries no per-fragment sample-aux sizes. At most
    /// one per `(aux_info_type, aux_info_type_parameter)` pair per
    /// §8.7.8.3; duplicates are dropped silently (first wins) by
    /// [`parse_traf`].
    pub saiz: Vec<Saiz>,
    /// `saio` Sample Auxiliary Information Offsets Boxes (ISO/IEC
    /// 14496-12 §8.7.9) declared at this `traf`'s scope. Empty when
    /// the fragment carries no per-fragment sample-aux offsets. At
    /// most one per `(aux_info_type, aux_info_type_parameter)` pair
    /// per §8.7.9.3; duplicates are dropped silently (first wins) by
    /// [`parse_traf`].
    pub saio: Vec<Saio>,
}

/// Resolve a `traf`'s on-disk samples into [`SampleEntry`]s ready
/// to splice into a [`crate::Track`]'s flat sample queue.
///
/// `moof_start` is the byte position of the **start** of the
/// enclosing `moof` (the spec's "first byte of the enclosing Movie
/// Fragment Box", §8.8.7.1 — the anchor point for
/// `default-base-is-moof`).
///
/// `trex` is the per-track defaults record (or `None` when the file
/// has no `mvex/trex` for this track — illegal per §8.8.3 but
/// real-world writers occasionally emit it; we fall back to all-
/// zero defaults so playback continues rather than fail outright).
///
/// `track_media_time` is the accumulated DTS at the start of this
/// fragment in media-timescale ticks. The caller threads it across
/// `moof`s so DTS keeps climbing monotonically through the
/// fragmented playback. The function returns the *new*
/// `track_media_time` (caller stores it for the next `moof`).
pub fn resolve_traf_samples(
    traf: &TrafRecord,
    trex: Option<&TrexDefaults>,
    moof_start: u64,
    prev_traf_end: u64,
    track_media_time: u64,
    starting_sample_index: u32,
) -> Result<(Vec<SampleEntry>, u64, u64)> {
    // 1. Per-fragment base offset (`base-data-offset`).
    let base_data_offset = if traf.tfhd.tf_flags & TFHD_BASE_DATA_OFFSET_PRESENT != 0 {
        traf.tfhd
            .base_data_offset
            .ok_or_else(|| Error::invalid("MOV: tfhd flag bit set but no base_data_offset"))?
    } else if traf.tfhd.tf_flags & TFHD_DEFAULT_BASE_IS_MOOF != 0 {
        // §8.8.7.1: "the base-data-offset for this track fragment is
        // the position of the first byte of the enclosing Movie
        // Fragment Box".
        moof_start
    } else {
        // §8.8.7.1: "the default is the end of the data defined by
        // the preceding track fragment". For the very first track
        // fragment in a moof the spec defaults to "the position of
        // the first byte of the enclosing Movie Fragment Box".
        prev_traf_end
    };

    // 2. Per-fragment defaults (tfhd → trex → 0).
    let frag_default_duration = traf
        .tfhd
        .default_sample_duration
        .or_else(|| trex.map(|t| t.default_sample_duration))
        .unwrap_or(0);
    let frag_default_size = traf
        .tfhd
        .default_sample_size
        .or_else(|| trex.map(|t| t.default_sample_size))
        .unwrap_or(0);
    let frag_default_flags = traf
        .tfhd
        .default_sample_flags
        .or_else(|| trex.map(|t| t.default_sample_flags))
        .unwrap_or(0);
    let frag_sdi = traf
        .tfhd
        .sample_description_index
        .or_else(|| trex.map(|t| t.default_sample_description_index))
        .unwrap_or(1);

    // 3. duration_is_empty short-circuit.
    if traf.tfhd.tf_flags & TFHD_DURATION_IS_EMPTY != 0 {
        return Ok((Vec::new(), prev_traf_end, track_media_time));
    }

    // 4. Walk each `trun` in declaration order.
    let mut out: Vec<SampleEntry> = Vec::new();
    let mut dts = track_media_time;
    let mut sample_index = starting_sample_index;
    let mut prev_run_end_offset: Option<u64> = None;
    for trun in &traf.truns {
        // Per §8.8.8.1: "If the data-offset is not present, then
        // the data for this run starts immediately after the data of
        // the previous run, or at the base-data-offset defined by the
        // track fragment header if this is the first run".
        let run_base = if let Some(off) = trun.data_offset {
            apply_signed_offset(base_data_offset, off)?
        } else if let Some(end) = prev_run_end_offset {
            end
        } else {
            base_data_offset
        };
        let mut cursor = run_base;
        for (i, srow) in trun.samples.iter().enumerate() {
            let size = srow.sample_size.unwrap_or(frag_default_size);
            let duration = srow.sample_duration.unwrap_or(frag_default_duration);
            // Composition-time offset — only present when the row
            // carries one (per the trun's flags); otherwise 0.
            let composition_offset = srow.sample_cts_offset.unwrap_or(0);
            // Sample-flags resolution: trun row → first_sample_flags
            // (only sample 0 of the run) → fragment default → trex
            // default.
            let flags = if let Some(f) = srow.sample_flags {
                f
            } else if i == 0 && trun.tr_flags & TRUN_FIRST_SAMPLE_FLAGS_PRESENT != 0 {
                trun.first_sample_flags.unwrap_or(frag_default_flags)
            } else {
                frag_default_flags
            };
            let keyframe = sample_flags_is_sync(flags);
            out.push(SampleEntry {
                index: sample_index,
                offset: cursor,
                size,
                dts,
                duration,
                sample_description_id: frag_sdi,
                keyframe,
                composition_offset,
            });
            sample_index = sample_index.saturating_add(1);
            dts = dts.saturating_add(duration as u64);
            cursor = cursor.saturating_add(size as u64);
        }
        prev_run_end_offset = Some(cursor);
    }

    let new_prev_traf_end = prev_run_end_offset.unwrap_or(base_data_offset);
    Ok((out, new_prev_traf_end, dts))
}

/// Apply a signed 32-bit offset to an unsigned 64-bit base, refusing
/// to underflow. ISO/IEC 14496-12 §8.8.8.2 declares `data_offset`
/// as `signed int(32)` so the value can legally be negative (e.g.
/// when a run sub-slices an earlier `mdat`).
fn apply_signed_offset(base: u64, off: i32) -> Result<u64> {
    if off >= 0 {
        base.checked_add(off as u64)
            .ok_or_else(|| Error::invalid("MOV: trun data_offset overflow"))
    } else {
        let mag = off.unsigned_abs() as u64;
        base.checked_sub(mag)
            .ok_or_else(|| Error::invalid("MOV: trun data_offset underflow"))
    }
}

/// Test the `sample_is_non_sync_sample` bit of a sample-flags word
/// (§8.8.3.1). Bit position is the LSB of the second-to-last byte:
/// `0x0001_0000` masks the byte that carries both the 3-bit
/// padding and the 1-bit non-sync flag.
///
/// Per the spec layout:
/// ```text
/// bit(4)  reserved = 0;
/// uint(2) is_leading;
/// uint(2) sample_depends_on;
/// uint(2) sample_is_depended_on;
/// uint(2) sample_has_redundancy;
/// bit(3)  sample_padding_value;
/// bit(1)  sample_is_non_sync_sample;
/// uint(16) sample_degradation_priority;
/// ```
/// → `sample_is_non_sync_sample` is bit 16 of the packed u32
///   (the LSB of the third byte from the right when read MSB-first).
pub fn sample_flags_is_sync(flags: u32) -> bool {
    (flags & 0x0001_0000) == 0
}

// ─────────────── tfra / mfra / mfro (§8.8.9–§8.8.11) ───────────────

/// One row of a `tfra` (Track Fragment Random Access Box, §8.8.10.2).
///
/// Each entry pinpoints one *sync* sample inside a fragmented stream:
/// `time` is the sample's decode-time-of-sync in the track's `mdhd`
/// timescale and `moof_offset` is the absolute byte offset of the
/// `moof` that contains the sample. The triple
/// `(traf_number, trun_number, sample_number)` is 1-based and
/// uniquely identifies the sample inside that `moof`.
///
/// The on-disk byte widths of the trailing three fields are encoded
/// in the parent `tfra`'s `length_size_of_*` nibbles (each ∈ {1,2,3,4}
/// bytes per §8.8.10.3); the parser widens them all to `u32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TfraEntry {
    /// Decode-time of the indexed sync sample in `mdhd.time_scale`
    /// ticks. Monotonically increases across entries within a single
    /// `tfra` per §8.8.10.3 ("the entries are stored in increasing
    /// order of time").
    pub time: u64,
    /// Absolute byte offset of the enclosing `moof` from the start
    /// of the file (the "first byte of the enclosing Movie Fragment
    /// Box" per §8.8.10.3, matching the `moof_start` anchor used by
    /// `resolve_traf_samples`).
    pub moof_offset: u64,
    /// 1-based index of the `traf` inside the moof that contains the
    /// indexed sample.
    pub traf_number: u32,
    /// 1-based index of the `trun` inside the `traf` that contains
    /// the indexed sample.
    pub trun_number: u32,
    /// 1-based index of the sample within the `trun`'s sample list.
    pub sample_number: u32,
}

/// Parsed `tfra` box per ISO/IEC 14496-12 §8.8.10. One `tfra` per
/// track that has fragmented random-access entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tfra {
    /// Track this index applies to (matches `tkhd.track_id`).
    pub track_id: u32,
    /// Per-entry decode-time and byte-offset table.
    pub entries: Vec<TfraEntry>,
}

/// Parsed `mfro` box per ISO/IEC 14496-12 §8.8.11. Always the very
/// last top-level box; carries the total byte length of the
/// preceding `mfra`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mfro {
    /// Total byte length of the immediately-preceding `mfra` box
    /// (including the 8-byte `mfra` header itself, and the `mfro`
    /// trailer). Used to locate `mfra` without scanning forward from
    /// `moov`.
    pub size: u32,
}

/// Parse a `tfra` payload per §8.8.10.3.
///
/// On-disk layout:
/// ```text
/// [ver+flags:4]
/// [track_ID:4]
/// [reserved:26 bits = 0]
/// [length_size_of_traf_num:2 bits]
/// [length_size_of_trun_num:2 bits]
/// [length_size_of_sample_num:2 bits]
/// [number_of_entry:4]
/// per entry:
///   v0 → [time:4][moof_offset:4]
///   v1 → [time:8][moof_offset:8]
///   [traf_number:1..4][trun_number:1..4][sample_number:1..4]
/// ```
///
/// The three `length_size_*` 2-bit fields each encode "byte width
/// minus 1", so 0 → 1 byte, 3 → 4 bytes. The parser widens every
/// field to `u32` on read.
pub fn parse_tfra(payload: &[u8]) -> Result<Tfra> {
    if payload.len() < 16 {
        return Err(Error::invalid("MOV: tfra payload < 16 bytes"));
    }
    let version = payload[0];
    let track_id = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let len_word = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let length_size_of_traf_num = (((len_word >> 4) & 0x3) + 1) as usize;
    let length_size_of_trun_num = (((len_word >> 2) & 0x3) + 1) as usize;
    let length_size_of_sample_num = ((len_word & 0x3) + 1) as usize;
    let number_of_entry = u32::from_be_bytes([payload[12], payload[13], payload[14], payload[15]]);

    let time_off_width: usize = match version {
        0 => 8,  // 4 + 4
        1 => 16, // 8 + 8
        _ => return Err(Error::invalid("MOV: tfra unsupported version")),
    };
    let per_entry = time_off_width
        + length_size_of_traf_num
        + length_size_of_trun_num
        + length_size_of_sample_num;
    let want = per_entry
        .checked_mul(number_of_entry as usize)
        .and_then(|t| t.checked_add(16))
        .ok_or_else(|| Error::invalid("MOV: tfra entry table overflow"))?;
    if payload.len() < want {
        return Err(Error::invalid("MOV: tfra entry table truncated"));
    }
    let mut entries = Vec::with_capacity(number_of_entry as usize);
    let mut off = 16usize;
    for _ in 0..number_of_entry {
        let (time, moof_offset) = match version {
            0 => {
                let t = u32::from_be_bytes([
                    payload[off],
                    payload[off + 1],
                    payload[off + 2],
                    payload[off + 3],
                ]) as u64;
                let m = u32::from_be_bytes([
                    payload[off + 4],
                    payload[off + 5],
                    payload[off + 6],
                    payload[off + 7],
                ]) as u64;
                off += 8;
                (t, m)
            }
            _ => {
                let t = u64::from_be_bytes([
                    payload[off],
                    payload[off + 1],
                    payload[off + 2],
                    payload[off + 3],
                    payload[off + 4],
                    payload[off + 5],
                    payload[off + 6],
                    payload[off + 7],
                ]);
                let m = u64::from_be_bytes([
                    payload[off + 8],
                    payload[off + 9],
                    payload[off + 10],
                    payload[off + 11],
                    payload[off + 12],
                    payload[off + 13],
                    payload[off + 14],
                    payload[off + 15],
                ]);
                off += 16;
                (t, m)
            }
        };
        let traf_number = read_var_be_u32(&payload[off..off + length_size_of_traf_num]);
        off += length_size_of_traf_num;
        let trun_number = read_var_be_u32(&payload[off..off + length_size_of_trun_num]);
        off += length_size_of_trun_num;
        let sample_number = read_var_be_u32(&payload[off..off + length_size_of_sample_num]);
        off += length_size_of_sample_num;
        entries.push(TfraEntry {
            time,
            moof_offset,
            traf_number,
            trun_number,
            sample_number,
        });
    }
    Ok(Tfra { track_id, entries })
}

/// Parse an `mfro` payload per §8.8.11.2. Always 8 bytes:
/// `[ver+flags:4][size:4]`.
pub fn parse_mfro(payload: &[u8]) -> Result<Mfro> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: mfro payload < 8 bytes"));
    }
    let size = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    Ok(Mfro { size })
}

/// Walk an `mfra` container, returning every `tfra` row plus the
/// trailing `mfro` (when present).
///
/// `mfra` carries zero or more `tfra` boxes (one per track with a
/// random-access index) followed by exactly one `mfro` (§8.8.9.2).
/// Unknown children are silently skipped — derived ISO BMFF specs
/// occasionally add new sub-boxes inside `mfra` and we want
/// forward compatibility.
pub fn parse_mfra<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
) -> Result<(Vec<Tfra>, Option<Mfro>)> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut tfras: Vec<Tfra> = Vec::new();
    let mut mfro: Option<Mfro> = None;
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &TFRA => {
                let body = read_payload(r, child)?;
                tfras.push(parse_tfra(&body)?);
            }
            t if t == &MFRO => {
                let body = read_payload(r, child)?;
                mfro = Some(parse_mfro(&body)?);
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok((tfras, mfro))
}

/// Read a variable-width big-endian unsigned integer (1..=4 bytes)
/// into a `u32`. Used by `parse_tfra` for the `traf_number`,
/// `trun_number`, and `sample_number` fields whose widths are
/// declared by the `length_size_of_*` nibbles.
fn read_var_be_u32(bytes: &[u8]) -> u32 {
    let mut v: u32 = 0;
    for &b in bytes {
        v = (v << 8) | (b as u32);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_fullbox(version_flags: u32, body: &[u8]) -> Vec<u8> {
        let mut p = Vec::with_capacity(4 + body.len());
        p.extend_from_slice(&version_flags.to_be_bytes());
        p.extend_from_slice(body);
        p
    }

    #[test]
    fn mfhd_round_trip() {
        let body = 42u32.to_be_bytes();
        let p = build_fullbox(0, &body);
        let m = parse_mfhd(&p).unwrap();
        assert_eq!(m.sequence_number, 42);
    }

    #[test]
    fn mehd_v0_round_trip() {
        let body = 1234u32.to_be_bytes();
        let p = build_fullbox(0, &body);
        let m = parse_mehd(&p).unwrap();
        assert_eq!(m.fragment_duration, 1234);
    }

    #[test]
    fn mehd_v1_round_trip() {
        let body = 0x1_0000_0000u64.to_be_bytes();
        let p = build_fullbox(0x01_00_00_00, &body); // version=1
        let m = parse_mehd(&p).unwrap();
        assert_eq!(m.fragment_duration, 0x1_0000_0000);
    }

    #[test]
    fn trex_round_trip() {
        let mut body = Vec::new();
        body.extend_from_slice(&7u32.to_be_bytes()); // track_id
        body.extend_from_slice(&2u32.to_be_bytes()); // sdi
        body.extend_from_slice(&90u32.to_be_bytes()); // dur
        body.extend_from_slice(&512u32.to_be_bytes()); // sz
        body.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // flags (non-sync)
        let p = build_fullbox(0, &body);
        let t = parse_trex(&p).unwrap();
        assert_eq!(t.track_id, 7);
        assert_eq!(t.default_sample_description_index, 2);
        assert_eq!(t.default_sample_duration, 90);
        assert_eq!(t.default_sample_size, 512);
        assert_eq!(t.default_sample_flags, 0x0001_0000);
        assert!(!t.default_is_sync());
    }

    #[test]
    fn tfhd_minimum_flags_only_track_id() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags = 0
        p.extend_from_slice(&3u32.to_be_bytes()); // track_id
        let t = parse_tfhd(&p).unwrap();
        assert_eq!(t.track_id, 3);
        assert_eq!(t.tf_flags, 0);
        assert!(t.base_data_offset.is_none());
        assert!(t.default_sample_size.is_none());
    }

    #[test]
    fn tfhd_default_base_is_moof_decoded() {
        let mut p = Vec::new();
        p.extend_from_slice(&TFHD_DEFAULT_BASE_IS_MOOF.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        let t = parse_tfhd(&p).unwrap();
        assert_ne!(t.tf_flags & TFHD_DEFAULT_BASE_IS_MOOF, 0);
    }

    #[test]
    fn tfhd_all_optional_fields_present() {
        let mut p = Vec::new();
        let flags = TFHD_BASE_DATA_OFFSET_PRESENT
            | TFHD_SAMPLE_DESCRIPTION_INDEX_PRESENT
            | TFHD_DEFAULT_SAMPLE_DURATION_PRESENT
            | TFHD_DEFAULT_SAMPLE_SIZE_PRESENT
            | TFHD_DEFAULT_SAMPLE_FLAGS_PRESENT;
        p.extend_from_slice(&flags.to_be_bytes()); // ver+flags
        p.extend_from_slice(&5u32.to_be_bytes()); // track_id
        p.extend_from_slice(&0x1234_5678u64.to_be_bytes()); // base_data_offset
        p.extend_from_slice(&2u32.to_be_bytes()); // sdi
        p.extend_from_slice(&90u32.to_be_bytes()); // default_sample_duration
        p.extend_from_slice(&512u32.to_be_bytes()); // default_sample_size
        p.extend_from_slice(&0xDEAD_BEEFu32.to_be_bytes()); // default_sample_flags
        let t = parse_tfhd(&p).unwrap();
        assert_eq!(t.base_data_offset, Some(0x1234_5678));
        assert_eq!(t.sample_description_index, Some(2));
        assert_eq!(t.default_sample_duration, Some(90));
        assert_eq!(t.default_sample_size, Some(512));
        assert_eq!(t.default_sample_flags, Some(0xDEAD_BEEF));
    }

    #[test]
    fn trun_explicit_per_sample_dur_sz_only() {
        // 3 samples; flags: data_offset + sample_duration + sample_size
        let tr_flags =
            TRUN_DATA_OFFSET_PRESENT | TRUN_SAMPLE_DURATION_PRESENT | TRUN_SAMPLE_SIZE_PRESENT;
        let mut p = Vec::new();
        p.extend_from_slice(&tr_flags.to_be_bytes()); // ver+flags (ver=0)
        p.extend_from_slice(&3u32.to_be_bytes()); // sample_count
        p.extend_from_slice(&100i32.to_be_bytes()); // data_offset
        for (d, s) in &[(10u32, 1024u32), (12, 2048), (8, 512)] {
            p.extend_from_slice(&d.to_be_bytes());
            p.extend_from_slice(&s.to_be_bytes());
        }
        let t = parse_trun(&p).unwrap();
        assert_eq!(t.samples.len(), 3);
        assert_eq!(t.data_offset, Some(100));
        assert_eq!(t.samples[0].sample_duration, Some(10));
        assert_eq!(t.samples[2].sample_size, Some(512));
        assert!(t.samples[0].sample_flags.is_none());
    }

    #[test]
    fn trun_first_sample_flags_override() {
        let tr_flags = TRUN_DATA_OFFSET_PRESENT
            | TRUN_FIRST_SAMPLE_FLAGS_PRESENT
            | TRUN_SAMPLE_DURATION_PRESENT;
        let mut p = Vec::new();
        p.extend_from_slice(&tr_flags.to_be_bytes());
        p.extend_from_slice(&2u32.to_be_bytes()); // sample_count
        p.extend_from_slice(&0i32.to_be_bytes()); // data_offset = 0
        p.extend_from_slice(&0u32.to_be_bytes()); // first_sample_flags = 0 (sync)
        p.extend_from_slice(&10u32.to_be_bytes()); // sample[0].duration
        p.extend_from_slice(&10u32.to_be_bytes()); // sample[1].duration
        let t = parse_trun(&p).unwrap();
        assert_eq!(t.first_sample_flags, Some(0));
        assert_eq!(t.samples[0].sample_duration, Some(10));
        assert!(t.samples[0].sample_flags.is_none());
    }

    #[test]
    fn resolve_traf_samples_default_base_is_moof() {
        // tfhd flags = default-base-is-moof (no explicit base);
        // trex provides default duration 90 and size 512.
        let tfhd = Tfhd {
            tf_flags: TFHD_DEFAULT_BASE_IS_MOOF,
            track_id: 1,
            base_data_offset: None,
            sample_description_index: None,
            default_sample_duration: None,
            default_sample_size: None,
            default_sample_flags: None,
        };
        let trex = TrexDefaults {
            track_id: 1,
            default_sample_description_index: 1,
            default_sample_duration: 90,
            default_sample_size: 512,
            default_sample_flags: 0,
        };
        // single trun with data_offset = 200, 3 samples, no per-sample fields
        let trun = Trun {
            version: 0,
            tr_flags: TRUN_DATA_OFFSET_PRESENT,
            data_offset: Some(200),
            first_sample_flags: None,
            samples: vec![
                TrunSample {
                    sample_duration: None,
                    sample_size: None,
                    sample_flags: None,
                    sample_cts_offset: None,
                };
                3
            ],
        };
        let rec = TrafRecord {
            tfhd,
            tfdt: None,
            truns: vec![trun],
            saiz: Vec::new(),
            saio: Vec::new(),
        };
        let moof_start = 1000u64;
        let (samples, end, dts) =
            resolve_traf_samples(&rec, Some(&trex), moof_start, 0, 0, 0).unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].offset, 1000 + 200);
        assert_eq!(samples[0].size, 512);
        assert_eq!(samples[1].offset, 1000 + 200 + 512);
        assert_eq!(samples[2].dts, 180);
        assert!(samples[0].keyframe); // trex flags = 0 → sync
        assert_eq!(dts, 270);
        assert_eq!(end, 1000 + 200 + 512 * 3);
    }

    #[test]
    fn resolve_traf_samples_per_sample_sizes_dur() {
        // No trex; per-sample dur+sz come from the trun rows.
        let tfhd = Tfhd {
            tf_flags: TFHD_BASE_DATA_OFFSET_PRESENT,
            track_id: 2,
            base_data_offset: Some(2000),
            sample_description_index: Some(1),
            default_sample_duration: None,
            default_sample_size: None,
            default_sample_flags: None,
        };
        let trun = Trun {
            version: 0,
            tr_flags: TRUN_SAMPLE_DURATION_PRESENT | TRUN_SAMPLE_SIZE_PRESENT,
            data_offset: None,
            first_sample_flags: None,
            samples: vec![
                TrunSample {
                    sample_duration: Some(33),
                    sample_size: Some(1024),
                    sample_flags: None,
                    sample_cts_offset: None,
                },
                TrunSample {
                    sample_duration: Some(33),
                    sample_size: Some(2048),
                    sample_flags: None,
                    sample_cts_offset: None,
                },
            ],
        };
        let rec = TrafRecord {
            tfhd,
            tfdt: None,
            truns: vec![trun],
            saiz: Vec::new(),
            saio: Vec::new(),
        };
        let (samples, _, dts) = resolve_traf_samples(&rec, None, 0, 0, 1000, 5).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].offset, 2000);
        assert_eq!(samples[0].size, 1024);
        assert_eq!(samples[0].dts, 1000);
        assert_eq!(samples[0].index, 5);
        assert_eq!(samples[1].offset, 2000 + 1024);
        assert_eq!(samples[1].index, 6);
        assert_eq!(dts, 1000 + 33 + 33);
    }

    #[test]
    fn resolve_traf_samples_first_sample_flags_marks_keyframe() {
        // trex declares non-sync default; first_sample_flags = 0
        // forces sample 0 to be a keyframe and the rest non-sync.
        let tfhd = Tfhd {
            tf_flags: TFHD_DEFAULT_BASE_IS_MOOF,
            track_id: 1,
            base_data_offset: None,
            sample_description_index: None,
            default_sample_duration: None,
            default_sample_size: None,
            default_sample_flags: None,
        };
        let trex = TrexDefaults {
            track_id: 1,
            default_sample_description_index: 1,
            default_sample_duration: 1,
            default_sample_size: 1,
            default_sample_flags: 0x0001_0000, // non-sync
        };
        let trun = Trun {
            version: 0,
            tr_flags: TRUN_FIRST_SAMPLE_FLAGS_PRESENT | TRUN_DATA_OFFSET_PRESENT,
            data_offset: Some(0),
            first_sample_flags: Some(0), // sync
            samples: vec![
                TrunSample {
                    sample_duration: None,
                    sample_size: None,
                    sample_flags: None,
                    sample_cts_offset: None,
                };
                3
            ],
        };
        let rec = TrafRecord {
            tfhd,
            tfdt: None,
            truns: vec![trun],
            saiz: Vec::new(),
            saio: Vec::new(),
        };
        let (samples, _, _) = resolve_traf_samples(&rec, Some(&trex), 0, 0, 0, 0).unwrap();
        assert_eq!(samples.len(), 3);
        assert!(samples[0].keyframe);
        assert!(!samples[1].keyframe);
        assert!(!samples[2].keyframe);
    }

    #[test]
    fn resolve_traf_samples_duration_is_empty_short_circuit() {
        let tfhd = Tfhd {
            tf_flags: TFHD_DURATION_IS_EMPTY,
            track_id: 1,
            base_data_offset: None,
            sample_description_index: None,
            default_sample_duration: None,
            default_sample_size: None,
            default_sample_flags: None,
        };
        let rec = TrafRecord {
            tfhd,
            tfdt: None,
            truns: Vec::new(),
            saiz: Vec::new(),
            saio: Vec::new(),
        };
        let (samples, _, dts) = resolve_traf_samples(&rec, None, 0, 100, 500, 0).unwrap();
        assert!(samples.is_empty());
        assert_eq!(dts, 500); // unchanged
    }

    #[test]
    fn apply_signed_offset_handles_negative() {
        assert_eq!(apply_signed_offset(1000, -100).unwrap(), 900);
        assert_eq!(apply_signed_offset(0, 0).unwrap(), 0);
        assert!(apply_signed_offset(0, -1).is_err());
    }

    #[test]
    fn sample_flags_sync_bit() {
        assert!(sample_flags_is_sync(0));
        assert!(!sample_flags_is_sync(0x0001_0000));
        // The other bits don't affect the sync classification.
        assert!(sample_flags_is_sync(0xFFFE_FFFF));
    }

    #[test]
    fn tfhd_truncated_optional_field_errors() {
        // TFHD with base-data-offset flag but no field after track_id.
        let mut p = Vec::new();
        p.extend_from_slice(&TFHD_BASE_DATA_OFFSET_PRESENT.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes()); // track_id
                                                  // Missing 8-byte base_data_offset
        assert!(parse_tfhd(&p).is_err());
    }

    #[test]
    fn trun_truncated_per_sample_rows_errors() {
        // Promise 5 samples × (4-byte duration), provide only 2.
        let mut p = Vec::new();
        p.extend_from_slice(&TRUN_SAMPLE_DURATION_PRESENT.to_be_bytes());
        p.extend_from_slice(&5u32.to_be_bytes()); // sample_count
        p.extend_from_slice(&10u32.to_be_bytes());
        p.extend_from_slice(&10u32.to_be_bytes());
        assert!(parse_trun(&p).is_err());
    }

    #[test]
    fn tfra_v0_single_entry_default_widths() {
        // §8.8.10: v0 → 32-bit time + 32-bit moof_offset; default
        // length_size_of_* nibbles = 0 → 1 byte each for traf_num /
        // trun_num / sample_num.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags (ver=0)
        p.extend_from_slice(&7u32.to_be_bytes()); // track_id
        p.extend_from_slice(&0u32.to_be_bytes()); // length_size word
        p.extend_from_slice(&1u32.to_be_bytes()); // number_of_entry
        p.extend_from_slice(&12345u32.to_be_bytes()); // time
        p.extend_from_slice(&5675u32.to_be_bytes()); // moof_offset
        p.push(1); // traf_number
        p.push(1); // trun_number
        p.push(1); // sample_number
        let t = parse_tfra(&p).unwrap();
        assert_eq!(t.track_id, 7);
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].time, 12345);
        assert_eq!(t.entries[0].moof_offset, 5675);
        assert_eq!(t.entries[0].traf_number, 1);
        assert_eq!(t.entries[0].trun_number, 1);
        assert_eq!(t.entries[0].sample_number, 1);
    }

    #[test]
    fn tfra_v1_multi_entry_64bit_time_offset() {
        let mut p = Vec::new();
        p.extend_from_slice(&0x01_00_00_00u32.to_be_bytes()); // version=1
        p.extend_from_slice(&1u32.to_be_bytes()); // track_id
        p.extend_from_slice(&0u32.to_be_bytes()); // length_size word (all 1 byte)
        p.extend_from_slice(&2u32.to_be_bytes()); // n
                                                  // entry 0
        p.extend_from_slice(&1u64.to_be_bytes()); // time
        p.extend_from_slice(&100u64.to_be_bytes()); // moof_offset
        p.extend_from_slice(&[1, 1, 1]);
        // entry 1
        p.extend_from_slice(&500u64.to_be_bytes());
        p.extend_from_slice(&8200u64.to_be_bytes());
        p.extend_from_slice(&[1, 2, 1]);
        let t = parse_tfra(&p).unwrap();
        assert_eq!(t.entries.len(), 2);
        assert_eq!(t.entries[1].time, 500);
        assert_eq!(t.entries[1].moof_offset, 8200);
        assert_eq!(t.entries[1].trun_number, 2);
    }

    #[test]
    fn tfra_variable_width_fields() {
        // length_size_of_traf_num=1 (2 bytes), trun=2 (3 bytes),
        // sample=3 (4 bytes). Encoded as 0b01 01 10 11 in bits [5:0],
        // so the low 6 bits = 0b011011 = 0x1B.
        let len_word: u32 = (1 << 4) | (2 << 2) | 3;
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&len_word.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes()); // n
        p.extend_from_slice(&10u32.to_be_bytes()); // time
        p.extend_from_slice(&20u32.to_be_bytes()); // moof_offset
        p.extend_from_slice(&[0, 7]); // traf_number = 7 (2 bytes)
        p.extend_from_slice(&[0, 0, 9]); // trun_number = 9 (3 bytes)
        p.extend_from_slice(&[0, 0, 0, 42]); // sample_number = 42 (4 bytes)
        let t = parse_tfra(&p).unwrap();
        assert_eq!(t.entries[0].traf_number, 7);
        assert_eq!(t.entries[0].trun_number, 9);
        assert_eq!(t.entries[0].sample_number, 42);
    }

    #[test]
    fn tfra_truncated_table_errors() {
        // n=5 but only one entry's worth of data after the header.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&5u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&[1, 1, 1]);
        assert!(parse_tfra(&p).is_err());
    }

    #[test]
    fn mfro_parses_size() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&67u32.to_be_bytes()); // size
        let m = parse_mfro(&p).unwrap();
        assert_eq!(m.size, 67);
    }

    #[test]
    fn mfro_truncated_errors() {
        let p = vec![0u8; 4];
        assert!(parse_mfro(&p).is_err());
    }

    #[test]
    fn tfdt_v0_round_trip() {
        // version 0 → 32-bit baseMediaDecodeTime
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&5120u32.to_be_bytes());
        assert_eq!(parse_tfdt(&p).unwrap(), 5120);
    }

    #[test]
    fn tfdt_v1_round_trip() {
        let mut p = Vec::new();
        p.extend_from_slice(&0x01_00_00_00u32.to_be_bytes()); // ver=1
        p.extend_from_slice(&0x1_0000_0000u64.to_be_bytes());
        assert_eq!(parse_tfdt(&p).unwrap(), 0x1_0000_0000);
    }

    #[test]
    fn tfdt_truncated_errors() {
        let p = vec![0u8; 2];
        assert!(parse_tfdt(&p).is_err());
        // v0 with only 4 bytes (no baseline payload)
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        assert!(parse_tfdt(&p).is_err());
    }
}
