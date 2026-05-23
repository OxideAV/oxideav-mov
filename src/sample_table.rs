//! QuickTime sample table parsing.
//!
//! Apple QTFF Chapter 2, "Sample Atoms" (pp. 67–80). The sample
//! table (`stbl`) is the canonical index into the `mdat`. The
//! demuxer combines four required tables:
//!
//! * `stts` — time-to-sample (sample-count, sample-duration runs).
//! * `stsc` — sample-to-chunk (first-chunk, samples-per-chunk,
//!   sample-description-id runs).
//! * `stsz` — sample-size (constant size, OR per-sample table).
//! * `stco` / `co64` — chunk offsets (32-bit or 64-bit).
//!
//! Plus the optional `stss` (sync-samples / keyframes; QTFF p. 73).
//!
//! The walking algorithm is QTFF-spec verbatim ("Finding a Sample",
//! p. 79):
//!
//! 1. examine `stts` to find the sample number for a given time;
//! 2. scan `stsc` to find the chunk that contains that sample;
//! 3. extract the chunk's offset from `stco`;
//! 4. sum sample sizes for all earlier samples in that chunk to
//!    locate the byte offset.
//!
//! `Iterator<Item = SampleEntry>` is the public surface: callers
//! consume samples in decode order.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

use crate::sample_groups::{SampleGroupDescription, SampleToGroup};

/// `stts` time-to-sample table entry.
#[derive(Clone, Copy, Debug)]
pub struct SttsEntry {
    pub sample_count: u32,
    pub sample_duration: u32,
}

/// `stsc` sample-to-chunk table entry.
#[derive(Clone, Copy, Debug)]
pub struct StscEntry {
    /// 1-based first chunk number this entry applies to (QTFF p. 76).
    pub first_chunk: u32,
    pub samples_per_chunk: u32,
    pub sample_description_id: u32,
}

/// `ctts` composition-time-to-sample table entry. ISO BMFF
/// §8.6.1.3.2 — version 0 carries unsigned offsets, version 1 carries
/// signed offsets. We always store the signed form; for v0 streams,
/// values are non-negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CttsEntry {
    pub sample_count: u32,
    /// Composition-time offset added to each of the run's samples'
    /// DTS to produce its PTS: `pts = dts + composition_offset`.
    pub composition_offset: i32,
}

/// `is_leading` field of an `sdtp` entry (ISO/IEC 14496-12 §8.6.4.3).
/// A *leading* sample has a composition time before its reference
/// I-picture; whether it is decodable when entering at the reference
/// sample depends on its decode-dependencies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsLeading {
    /// `0` — the leading nature of this sample is unknown.
    Unknown,
    /// `1` — leading sample with a dependency before the referenced
    /// I-picture (therefore *not* decodable from the reference).
    LeadingUndecodable,
    /// `2` — not a leading sample.
    NotLeading,
    /// `3` — leading sample with no dependency before the referenced
    /// I-picture (therefore decodable from the reference).
    LeadingDecodable,
}

/// `sample_depends_on` field of an `sdtp` entry (§8.6.4.3): does this
/// sample depend on others for its decoding?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleDependsOn {
    /// `0` — the dependency of this sample is unknown.
    Unknown,
    /// `1` — this sample does depend on others (not an I-picture).
    DependsOnOthers,
    /// `2` — this sample does not depend on others (an I-picture).
    Independent,
    /// `3` — reserved.
    Reserved,
}

/// `sample_is_depended_on` field of an `sdtp` entry (§8.6.4.3): do
/// other samples depend on this one? When `Disposable`, the sample
/// may be dropped during trick-mode roll-forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleIsDependedOn {
    /// `0` — the dependency of other samples on this one is unknown.
    Unknown,
    /// `1` — other samples may depend on this one (not disposable).
    NotDisposable,
    /// `2` — no other sample depends on this one (disposable).
    Disposable,
    /// `3` — reserved.
    Reserved,
}

/// `sample_has_redundancy` field of an `sdtp` entry (§8.6.4.3): does
/// this sample carry redundant (multiple) codings of the data at this
/// time-instant?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleHasRedundancy {
    /// `0` — it is unknown whether redundant coding is present.
    Unknown,
    /// `1` — there is redundant coding in this sample.
    Redundant,
    /// `2` — there is no redundant coding in this sample.
    NotRedundant,
    /// `3` — reserved.
    Reserved,
}

/// One per-sample row of the `sdtp` (Independent and Disposable
/// Samples) Box — ISO/IEC 14496-12 §8.6.4. Each on-disk byte packs
/// the four 2-bit fields, MSB-first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SdtpEntry {
    pub is_leading: IsLeading,
    pub sample_depends_on: SampleDependsOn,
    pub sample_is_depended_on: SampleIsDependedOn,
    pub sample_has_redundancy: SampleHasRedundancy,
}

impl SdtpEntry {
    /// Decode one packed `sdtp` byte: `is_leading` in bits 7..6,
    /// `sample_depends_on` in bits 5..4, `sample_is_depended_on` in
    /// bits 3..2, `sample_has_redundancy` in bits 1..0 (§8.6.4.2).
    pub fn from_byte(b: u8) -> Self {
        let il = match (b >> 6) & 0x3 {
            0 => IsLeading::Unknown,
            1 => IsLeading::LeadingUndecodable,
            2 => IsLeading::NotLeading,
            _ => IsLeading::LeadingDecodable,
        };
        let sdo = match (b >> 4) & 0x3 {
            0 => SampleDependsOn::Unknown,
            1 => SampleDependsOn::DependsOnOthers,
            2 => SampleDependsOn::Independent,
            _ => SampleDependsOn::Reserved,
        };
        let sido = match (b >> 2) & 0x3 {
            0 => SampleIsDependedOn::Unknown,
            1 => SampleIsDependedOn::NotDisposable,
            2 => SampleIsDependedOn::Disposable,
            _ => SampleIsDependedOn::Reserved,
        };
        let shr = match b & 0x3 {
            0 => SampleHasRedundancy::Unknown,
            1 => SampleHasRedundancy::Redundant,
            2 => SampleHasRedundancy::NotRedundant,
            _ => SampleHasRedundancy::Reserved,
        };
        Self {
            is_leading: il,
            sample_depends_on: sdo,
            sample_is_depended_on: sido,
            sample_has_redundancy: shr,
        }
    }

    /// True when this sample is independently decodable (an I-picture):
    /// `sample_depends_on == 2` (§8.6.4.3). Pairs with `stss` as a
    /// codec-agnostic random-access hint.
    pub fn is_independent(&self) -> bool {
        self.sample_depends_on == SampleDependsOn::Independent
    }

    /// True when no other sample depends on this one
    /// (`sample_is_depended_on == 2`); such samples may be skipped
    /// while rolling forward during trick-mode (§8.6.4.1, §8.6.4.3).
    pub fn is_disposable(&self) -> bool {
        self.sample_is_depended_on == SampleIsDependedOn::Disposable
    }
}

/// Parsed sample table for a single track.
#[derive(Clone, Debug, Default)]
pub struct SampleTable {
    pub stts: Vec<SttsEntry>,
    pub stsc: Vec<StscEntry>,
    /// `Some(constant_size)` when `stsz` declares a single uniform
    /// sample size; `None` means look up sample N's size in
    /// `stsz_table[N]` (0-based).
    pub stsz_default_size: Option<u32>,
    pub stsz_count: u32,
    pub stsz_table: Vec<u32>,
    /// Chunk offsets in *file* (input) byte coordinates. May be u64
    /// when `co64` was used.
    pub chunk_offsets: Vec<u64>,
    /// Sync sample numbers (1-based per QTFF p. 73). Empty means
    /// "every sample is a keyframe" (the spec's implicit-sync case).
    pub stss: Vec<u32>,
    /// `ctts` composition-time offsets per sample run. Empty means
    /// "no composition offsets" — DTS == PTS for every sample.
    pub ctts: Vec<CttsEntry>,
    /// `sbgp` Sample-to-Group Boxes (ISO/IEC 14496-12 §8.9.2). One
    /// per `(grouping_type, grouping_type_parameter)` pair found
    /// inside `stbl`. Empty when the track carries no sample
    /// groupings.
    pub sbgp: Vec<SampleToGroup>,
    /// `sgpd` Sample-Group-Description Boxes (ISO/IEC 14496-12
    /// §8.9.3). Keyed by the same `grouping_type` as the matching
    /// [`SampleToGroup`].
    pub sgpd: Vec<SampleGroupDescription>,
    /// `sdtp` Independent and Disposable Samples Box rows (ISO/IEC
    /// 14496-12 §8.6.4). One [`SdtpEntry`] per sample, in decode
    /// order. Empty when the track carries no `sdtp` box. The on-disk
    /// table has no count field — its length equals the `stsz`/`stz2`
    /// sample count (§8.6.4.1).
    pub sdtp: Vec<SdtpEntry>,
}

/// One entry in the iterator output: enough to read the sample bytes
/// from the input stream.
#[derive(Clone, Copy, Debug)]
pub struct SampleEntry {
    /// 0-based sample index inside the track.
    pub index: u32,
    /// Absolute file byte offset of the sample.
    pub offset: u64,
    /// Sample size in bytes.
    pub size: u32,
    /// Decode timestamp in media-timescale ticks.
    pub dts: u64,
    /// Sample duration in media-timescale ticks.
    pub duration: u32,
    /// Sample-description-table index (1-based, per QTFF stsc).
    pub sample_description_id: u32,
    /// True when this sample is listed in `stss` (or the table is empty).
    pub keyframe: bool,
    /// Composition offset (PTS - DTS), zero when `ctts` is absent.
    /// Signed so that v1 ctts streams with negative offsets survive.
    pub composition_offset: i32,
}

impl SampleEntry {
    /// Presentation timestamp = DTS + composition_offset (saturating
    /// to keep the i64 surface monotonic for downstream code).
    pub fn pts(&self) -> i64 {
        (self.dts as i64).saturating_add(self.composition_offset as i64)
    }
}

impl SampleTable {
    /// Total sample count, derived from `stsz` (`stsz_count` is
    /// authoritative per QTFF p. 76).
    pub fn sample_count(&self) -> u32 {
        self.stsz_count
    }

    /// Iterate samples in decode order.
    pub fn iter_samples(&self) -> SampleIter<'_> {
        SampleIter::new(self)
    }

    /// `sdtp` row for a 0-based decode-order sample index, or `None`
    /// when the track carries no `sdtp` box (or the index is past the
    /// table). ISO/IEC 14496-12 §8.6.4.
    pub fn sample_dependency(&self, sample_idx: u32) -> Option<SdtpEntry> {
        self.sdtp.get(sample_idx as usize).copied()
    }

    /// Look up the [`SampleToGroup`] / [`SampleGroupDescription`] pair
    /// for a specific `grouping_type` FourCC. Returns `(sbgp,
    /// sgpd_for_group)` when present. The `sbgp` borrow alone is
    /// enough to ask "which group index does sample N belong to";
    /// the `sgpd` carries the typed payload for that group index.
    ///
    /// ISO/IEC 14496-12 §8.9.2 promises **at most one** `sbgp` /
    /// `sgpd` per `grouping_type` inside a single Sample Table Box,
    /// so the first match in each `Vec` is authoritative.
    pub fn sample_group<'a>(
        &'a self,
        grouping_type: &[u8; 4],
    ) -> Option<(&'a SampleToGroup, &'a SampleGroupDescription)> {
        let sbgp = self
            .sbgp
            .iter()
            .find(|s| &s.grouping_type == grouping_type)?;
        let sgpd = self
            .sgpd
            .iter()
            .find(|s| &s.grouping_type == grouping_type)?;
        Some((sbgp, sgpd))
    }

    /// Resolve the 1-based `group_description_index` for the sample
    /// at `sample_zero_based`, applying the §8.9.3.3
    /// `default_sample_description_index` fall-back when the `sbgp`
    /// returns 0. Returns `None` only when neither the `sbgp` nor
    /// the v2 `sgpd` default associates this sample with a group.
    pub fn group_description_index_for_sample(
        &self,
        grouping_type: &[u8; 4],
        sample_zero_based: u32,
    ) -> Option<u32> {
        let (sbgp, sgpd) = self.sample_group(grouping_type)?;
        let raw = sbgp.group_index_for_sample(sample_zero_based);
        if raw != 0 {
            return Some(raw);
        }
        // §8.9.3.3 — version-2 `sgpd` may specify a default index for
        // samples with no explicit sbgp row. Pre-v2 boxes have this
        // field forced to zero.
        let default_idx = sgpd.default_sample_description_index;
        if default_idx == 0 {
            None
        } else {
            Some(default_idx)
        }
    }
}

/// Parse `stts` payload (8-byte fixed header `[ver+flags][n_entries]`
/// followed by `n_entries × (count, duration)` u32 pairs).
pub fn parse_stts(payload: &[u8]) -> Result<Vec<SttsEntry>> {
    need(payload, 0, 8, "stts header")?;
    let n = read_u32(&payload[4..]);
    let body = &payload[8..];
    if body.len() < (n as usize) * 8 {
        return Err(Error::invalid("MOV: stts truncated table"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        let off = i * 8;
        out.push(SttsEntry {
            sample_count: read_u32(&body[off..]),
            sample_duration: read_u32(&body[off + 4..]),
        });
    }
    Ok(out)
}

/// Parse `stsc` payload (8-byte fixed header followed by `n × 12` bytes).
pub fn parse_stsc(payload: &[u8]) -> Result<Vec<StscEntry>> {
    need(payload, 0, 8, "stsc header")?;
    let n = read_u32(&payload[4..]);
    let body = &payload[8..];
    if body.len() < (n as usize) * 12 {
        return Err(Error::invalid("MOV: stsc truncated table"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        let off = i * 12;
        out.push(StscEntry {
            first_chunk: read_u32(&body[off..]),
            samples_per_chunk: read_u32(&body[off + 4..]),
            sample_description_id: read_u32(&body[off + 8..]),
        });
    }
    // QTFF requires first_chunk fields to be strictly increasing
    // (each entry covers a half-open run starting at `first_chunk`
    // and ending at the next entry's `first_chunk - 1`).
    for w in out.windows(2) {
        if w[1].first_chunk <= w[0].first_chunk {
            return Err(Error::invalid(
                "MOV: stsc first_chunk not strictly increasing",
            ));
        }
    }
    if let Some(first) = out.first() {
        if first.first_chunk != 1 {
            return Err(Error::invalid("MOV: stsc first entry's first_chunk != 1"));
        }
    }
    Ok(out)
}

/// Parse `stsz` payload.
///
/// Layout (QTFF p. 77 Figure 2-36): `[ver+flags=4][sample_size=4]
/// [number_of_entries=4][optional sample_size_table = N×4]`.
/// When `sample_size != 0`, samples are uniform and the table is absent.
pub fn parse_stsz(payload: &[u8]) -> Result<(Option<u32>, u32, Vec<u32>)> {
    need(payload, 0, 12, "stsz header")?;
    let sample_size = read_u32(&payload[4..]);
    let count = read_u32(&payload[8..]);
    if sample_size != 0 {
        return Ok((Some(sample_size), count, Vec::new()));
    }
    let body = &payload[12..];
    if body.len() < (count as usize) * 4 {
        return Err(Error::invalid("MOV: stsz truncated table"));
    }
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..(count as usize) {
        out.push(read_u32(&body[i * 4..]));
    }
    Ok((None, count, out))
}

/// Parse `stco` (32-bit chunk offsets).
pub fn parse_stco(payload: &[u8]) -> Result<Vec<u64>> {
    need(payload, 0, 8, "stco header")?;
    let n = read_u32(&payload[4..]);
    let body = &payload[8..];
    if body.len() < (n as usize) * 4 {
        return Err(Error::invalid("MOV: stco truncated table"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        out.push(read_u32(&body[i * 4..]) as u64);
    }
    Ok(out)
}

/// Parse `co64` (64-bit chunk offsets, ISO BMFF extension QTFF p. 78
/// note).
pub fn parse_co64(payload: &[u8]) -> Result<Vec<u64>> {
    need(payload, 0, 8, "co64 header")?;
    let n = read_u32(&payload[4..]);
    let body = &payload[8..];
    if body.len() < (n as usize) * 8 {
        return Err(Error::invalid("MOV: co64 truncated table"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        out.push(read_u64(&body[i * 8..]));
    }
    Ok(out)
}

/// Parse `ctts` composition-time-to-sample payload.
///
/// Layout per ISO BMFF §8.6.1.3.2: `[version:1][flags:3][n:4]` then
/// `n × {sample_count:4, sample_offset:4}`. Version-0 sample_offset
/// is unsigned; version-1 is signed. We normalise to `i32` (v0
/// offsets are guaranteed positive — the cast is a no-op).
pub fn parse_ctts(payload: &[u8]) -> Result<Vec<CttsEntry>> {
    need(payload, 0, 8, "ctts header")?;
    let version = payload[0];
    let n = read_u32(&payload[4..]);
    let body = &payload[8..];
    if body.len() < (n as usize) * 8 {
        return Err(Error::invalid("MOV: ctts truncated table"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        let off = i * 8;
        let count = read_u32(&body[off..]);
        let offset = match version {
            0 => read_u32(&body[off + 4..]) as i32,
            _ => i32::from_be_bytes([body[off + 4], body[off + 5], body[off + 6], body[off + 7]]),
        };
        out.push(CttsEntry {
            sample_count: count,
            composition_offset: offset,
        });
    }
    Ok(out)
}

/// Parse `sdtp` (Independent and Disposable Samples Box) payload.
///
/// Layout per ISO/IEC 14496-12 §8.6.4.2: `[version:1][flags:3]` then
/// one packed byte per sample (no on-disk count field — the row count
/// equals the `stsz`/`stz2` `sample_count`, §8.6.4.1). `sample_count`
/// is passed in by the caller from the already-parsed sample-size
/// table. The body must hold at least `sample_count` bytes; trailing
/// padding (some muxers round up) is ignored.
pub fn parse_sdtp(payload: &[u8], sample_count: u32) -> Result<Vec<SdtpEntry>> {
    need(payload, 0, 4, "sdtp header")?;
    let body = &payload[4..];
    let n = sample_count as usize;
    if body.len() < n {
        return Err(Error::invalid("MOV: sdtp truncated table"));
    }
    Ok(body[..n].iter().map(|&b| SdtpEntry::from_byte(b)).collect())
}

/// Parse `stss` sync-sample table (1-based sample numbers).
pub fn parse_stss(payload: &[u8]) -> Result<Vec<u32>> {
    need(payload, 0, 8, "stss header")?;
    let n = read_u32(&payload[4..]);
    let body = &payload[8..];
    if body.len() < (n as usize) * 4 {
        return Err(Error::invalid("MOV: stss truncated table"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        out.push(read_u32(&body[i * 4..]));
    }
    Ok(out)
}

/// Iterator over samples in decode order. Walks `stsc` and the
/// chunk-offset table to compute per-sample byte offsets, summing
/// `stsz` sizes inside each chunk. `stts` runs are unwound to feed
/// per-sample timestamps.
pub struct SampleIter<'a> {
    table: &'a SampleTable,
    /// 0-based sample index of the next sample to emit.
    sample_idx: u32,
    /// Walking state for stsc.
    stsc_entry: usize,
    /// 1-based current chunk index.
    chunk_idx: u32,
    /// 0-based offset of the next sample inside the current chunk.
    sample_in_chunk: u32,
    /// Cached file offset for the next sample inside the current chunk.
    cursor_in_chunk: u64,
    /// stts walking state.
    stts_entry: usize,
    stts_remaining_in_run: u32,
    dts: u64,
    /// stss walking state — index into `table.stss` for next-keyframe.
    stss_idx: usize,
    /// ctts walking state.
    ctts_entry: usize,
    ctts_remaining_in_run: u32,
}

impl<'a> SampleIter<'a> {
    fn new(t: &'a SampleTable) -> Self {
        let cursor_in_chunk = t.chunk_offsets.first().copied().unwrap_or(0);
        let stts_remaining_in_run = t.stts.first().map(|e| e.sample_count).unwrap_or(0);
        let ctts_remaining_in_run = t.ctts.first().map(|e| e.sample_count).unwrap_or(0);
        Self {
            table: t,
            sample_idx: 0,
            stsc_entry: 0,
            chunk_idx: 1,
            sample_in_chunk: 0,
            cursor_in_chunk,
            stts_entry: 0,
            stts_remaining_in_run,
            dts: 0,
            stss_idx: 0,
            ctts_entry: 0,
            ctts_remaining_in_run,
        }
    }

    /// Returns the composition offset for the current sample and
    /// advances the ctts walker. Returns 0 when ctts is empty.
    fn advance_ctts(&mut self) -> i32 {
        if self.table.ctts.is_empty() {
            return 0;
        }
        loop {
            if self.ctts_entry >= self.table.ctts.len() {
                return 0;
            }
            if self.ctts_remaining_in_run == 0 {
                self.ctts_entry += 1;
                if self.ctts_entry >= self.table.ctts.len() {
                    return 0;
                }
                self.ctts_remaining_in_run = self.table.ctts[self.ctts_entry].sample_count;
                continue;
            }
            let off = self.table.ctts[self.ctts_entry].composition_offset;
            self.ctts_remaining_in_run -= 1;
            return off;
        }
    }

    fn samples_per_current_chunk(&self) -> u32 {
        // Find the stsc entry whose first_chunk <= current_chunk and
        // either is the last entry, or the next entry's first_chunk
        // is strictly greater than `current_chunk`.
        if self.stsc_entry >= self.table.stsc.len() {
            return 0;
        }
        self.table.stsc[self.stsc_entry].samples_per_chunk
    }

    fn current_sample_description_id(&self) -> u32 {
        if self.stsc_entry >= self.table.stsc.len() {
            1
        } else {
            self.table.stsc[self.stsc_entry].sample_description_id
        }
    }

    fn current_sample_size(&self) -> Option<u32> {
        if let Some(s) = self.table.stsz_default_size {
            return Some(s);
        }
        self.table.stsz_table.get(self.sample_idx as usize).copied()
    }

    fn advance_stts(&mut self) -> u32 {
        // Returns the duration of the sample being emitted, then
        // bumps the dts cursor.
        loop {
            if self.stts_entry >= self.table.stts.len() {
                return 0;
            }
            if self.stts_remaining_in_run == 0 {
                self.stts_entry += 1;
                if self.stts_entry >= self.table.stts.len() {
                    return 0;
                }
                self.stts_remaining_in_run = self.table.stts[self.stts_entry].sample_count;
                continue;
            }
            let dur = self.table.stts[self.stts_entry].sample_duration;
            self.stts_remaining_in_run -= 1;
            return dur;
        }
    }

    fn is_keyframe(&mut self) -> bool {
        if self.table.stss.is_empty() {
            return true;
        }
        let want_one_based = self.sample_idx + 1;
        // stss is sorted ascending per QTFF p. 73 ("strictly increasing").
        while self.stss_idx < self.table.stss.len()
            && self.table.stss[self.stss_idx] < want_one_based
        {
            self.stss_idx += 1;
        }
        self.stss_idx < self.table.stss.len() && self.table.stss[self.stss_idx] == want_one_based
    }

    fn maybe_advance_chunk(&mut self) {
        let per = self.samples_per_current_chunk();
        if per == 0 {
            return;
        }
        if self.sample_in_chunk >= per {
            // Advance to the next chunk.
            self.chunk_idx += 1;
            self.sample_in_chunk = 0;
            // Did we cross into a new stsc entry?
            if self.stsc_entry + 1 < self.table.stsc.len() {
                let next_first = self.table.stsc[self.stsc_entry + 1].first_chunk;
                if self.chunk_idx >= next_first {
                    self.stsc_entry += 1;
                }
            }
            // Prime the cursor for the new chunk.
            let chunk_index_0 = (self.chunk_idx as usize).saturating_sub(1);
            if let Some(off) = self.table.chunk_offsets.get(chunk_index_0).copied() {
                self.cursor_in_chunk = off;
            }
        }
    }
}

impl Iterator for SampleIter<'_> {
    type Item = Result<SampleEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.sample_idx >= self.table.stsz_count {
            return None;
        }
        self.maybe_advance_chunk();
        let size = match self.current_sample_size() {
            Some(s) => s,
            None => {
                return Some(Err(Error::invalid(
                    "MOV: sample index past end of stsz table",
                )))
            }
        };
        // Verify chunk pointer is valid.
        let chunk_index_0 = (self.chunk_idx as usize).saturating_sub(1);
        if chunk_index_0 >= self.table.chunk_offsets.len() {
            return Some(Err(Error::invalid(
                "MOV: stsc references chunk beyond stco range",
            )));
        }
        let offset = self.cursor_in_chunk;
        let dur = self.advance_stts();
        let kf = self.is_keyframe();
        let composition_offset = self.advance_ctts();
        let entry = SampleEntry {
            index: self.sample_idx,
            offset,
            size,
            dts: self.dts,
            duration: dur,
            sample_description_id: self.current_sample_description_id(),
            keyframe: kf,
            composition_offset,
        };
        // Bump cursors.
        self.sample_idx += 1;
        self.sample_in_chunk += 1;
        self.cursor_in_chunk += size as u64;
        self.dts += dur as u64;
        Some(Ok(entry))
    }
}

// ─────────────── helpers ───────────────

#[inline]
fn read_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

#[inline]
fn read_u64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[inline]
fn need(buf: &[u8], offset: usize, n: usize, what: &'static str) -> Result<()> {
    if offset + n > buf.len() {
        Err(Error::invalid(format!("MOV: short read for {what}")))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_stts_payload(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (c, d) in entries {
            p.extend_from_slice(&c.to_be_bytes());
            p.extend_from_slice(&d.to_be_bytes());
        }
        p
    }

    #[test]
    fn stts_round_trip() {
        let p = build_stts_payload(&[(4, 3), (2, 1), (3, 2)]); // QTFF p.73 Fig 2-30
        let v = parse_stts(&p).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].sample_count, 4);
        assert_eq!(v[2].sample_duration, 2);
    }

    #[test]
    fn stsc_rejects_decreasing_first_chunk() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&2u32.to_be_bytes());
        for (fc, spc, id) in &[(1u32, 1u32, 1u32), (1u32, 1u32, 1u32)] {
            p.extend_from_slice(&fc.to_be_bytes());
            p.extend_from_slice(&spc.to_be_bytes());
            p.extend_from_slice(&id.to_be_bytes());
        }
        assert!(parse_stsc(&p).is_err());
    }

    #[test]
    fn stsz_constant_size_no_table() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&100u32.to_be_bytes()); // sample_size = 100
        p.extend_from_slice(&5u32.to_be_bytes()); // count = 5
        let (def, n, t) = parse_stsz(&p).unwrap();
        assert_eq!(def, Some(100));
        assert_eq!(n, 5);
        assert!(t.is_empty());
    }

    #[test]
    fn stsz_per_sample_table() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0 → table follows
        p.extend_from_slice(&3u32.to_be_bytes()); // count = 3
        for s in &[10u32, 20, 30] {
            p.extend_from_slice(&s.to_be_bytes());
        }
        let (def, n, t) = parse_stsz(&p).unwrap();
        assert!(def.is_none());
        assert_eq!(n, 3);
        assert_eq!(t, vec![10, 20, 30]);
    }

    #[test]
    fn stco_round_trip() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&2u32.to_be_bytes());
        p.extend_from_slice(&1024u32.to_be_bytes());
        p.extend_from_slice(&2048u32.to_be_bytes());
        let v = parse_stco(&p).unwrap();
        assert_eq!(v, vec![1024, 2048]);
    }

    #[test]
    fn iter_single_sample_table() {
        // 1 chunk, 1 sample, no stss → keyframe by implication.
        let table = SampleTable {
            stts: vec![SttsEntry {
                sample_count: 1,
                sample_duration: 100,
            }],
            stsc: vec![StscEntry {
                first_chunk: 1,
                samples_per_chunk: 1,
                sample_description_id: 1,
            }],
            stsz_default_size: Some(42),
            stsz_count: 1,
            stsz_table: vec![],
            chunk_offsets: vec![512],
            stss: vec![],
            ctts: vec![],
            sbgp: vec![],
            sgpd: vec![],
            sdtp: vec![],
        };
        let v: Vec<_> = table.iter_samples().collect::<Result<_>>().unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].offset, 512);
        assert_eq!(v[0].size, 42);
        assert_eq!(v[0].dts, 0);
        assert_eq!(v[0].duration, 100);
        assert!(v[0].keyframe);
        assert_eq!(v[0].composition_offset, 0);
    }

    #[test]
    fn ctts_v0_unsigned_offsets_round_trip() {
        // 2 entries: 3 samples @ +1, 1 sample @ +5
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
        p.extend_from_slice(&2u32.to_be_bytes());
        p.extend_from_slice(&3u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&5u32.to_be_bytes());
        let v = parse_ctts(&p).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].sample_count, 3);
        assert_eq!(v[0].composition_offset, 1);
        assert_eq!(v[1].composition_offset, 5);
    }

    #[test]
    fn ctts_v1_negative_offset_round_trip() {
        // ver=1, single entry 2 × -3
        let mut p = Vec::new();
        p.push(1);
        p.extend_from_slice(&[0, 0, 0]);
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&2u32.to_be_bytes()); // count
        p.extend_from_slice(&(-3i32).to_be_bytes());
        let v = parse_ctts(&p).unwrap();
        assert_eq!(v[0].composition_offset, -3);
    }

    #[test]
    fn iter_with_ctts_offsets_pts() {
        // 4 samples; ctts run [3 × +10, 1 × +0] → PTS = DTS + offset.
        let table = SampleTable {
            stts: vec![SttsEntry {
                sample_count: 4,
                sample_duration: 50,
            }],
            stsc: vec![StscEntry {
                first_chunk: 1,
                samples_per_chunk: 4,
                sample_description_id: 1,
            }],
            stsz_default_size: Some(10),
            stsz_count: 4,
            stsz_table: vec![],
            chunk_offsets: vec![100],
            stss: vec![],
            ctts: vec![
                CttsEntry {
                    sample_count: 3,
                    composition_offset: 10,
                },
                CttsEntry {
                    sample_count: 1,
                    composition_offset: 0,
                },
            ],
            sbgp: vec![],
            sgpd: vec![],
            sdtp: vec![],
        };
        let v: Vec<_> = table.iter_samples().collect::<Result<_>>().unwrap();
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].composition_offset, 10);
        assert_eq!(v[2].composition_offset, 10);
        assert_eq!(v[3].composition_offset, 0);
        assert_eq!(v[3].pts(), 150); // dts=150, off=0
        assert_eq!(v[2].pts(), 110); // dts=100, off=10
    }

    #[test]
    fn iter_two_chunks_two_samples_each() {
        // 2 chunks of 2 samples each, sizes 10, 20, 30, 40.
        let table = SampleTable {
            stts: vec![SttsEntry {
                sample_count: 4,
                sample_duration: 50,
            }],
            stsc: vec![StscEntry {
                first_chunk: 1,
                samples_per_chunk: 2,
                sample_description_id: 1,
            }],
            stsz_default_size: None,
            stsz_count: 4,
            stsz_table: vec![10, 20, 30, 40],
            chunk_offsets: vec![1000, 2000],
            stss: vec![1, 3],
            ctts: vec![],
            sbgp: vec![],
            sgpd: vec![],
            sdtp: vec![],
        };
        let v: Vec<_> = table.iter_samples().collect::<Result<_>>().unwrap();
        assert_eq!(v.len(), 4);
        // Chunk 1 starts at 1000: samples 0 (offset 1000), 1 (offset 1010)
        assert_eq!(v[0].offset, 1000);
        assert_eq!(v[1].offset, 1010);
        // Chunk 2 starts at 2000: samples 2 (offset 2000), 3 (offset 2030)
        assert_eq!(v[2].offset, 2000);
        assert_eq!(v[3].offset, 2030);
        assert_eq!(v[0].dts, 0);
        assert_eq!(v[3].dts, 150);
        assert!(v[0].keyframe);
        assert!(!v[1].keyframe);
        assert!(v[2].keyframe);
        assert!(!v[3].keyframe);
    }

    #[test]
    fn sdtp_entry_field_packing_msb_first() {
        // is_leading=2, depends_on=1, is_depended_on=2, redundancy=2
        //   → 0b10_01_10_10 = 0x9A (§8.6.4.2 — fields MSB-first).
        let e = SdtpEntry::from_byte(0b10_01_10_10);
        assert_eq!(e.is_leading, IsLeading::NotLeading);
        assert_eq!(e.sample_depends_on, SampleDependsOn::DependsOnOthers);
        assert_eq!(e.sample_is_depended_on, SampleIsDependedOn::Disposable);
        assert_eq!(e.sample_has_redundancy, SampleHasRedundancy::NotRedundant);
        assert!(!e.is_independent());
        assert!(e.is_disposable());
    }

    #[test]
    fn sdtp_entry_all_zero_is_all_unknown() {
        let e = SdtpEntry::from_byte(0);
        assert_eq!(e.is_leading, IsLeading::Unknown);
        assert_eq!(e.sample_depends_on, SampleDependsOn::Unknown);
        assert_eq!(e.sample_is_depended_on, SampleIsDependedOn::Unknown);
        assert_eq!(e.sample_has_redundancy, SampleHasRedundancy::Unknown);
        assert!(!e.is_independent());
        assert!(!e.is_disposable());
    }

    #[test]
    fn sdtp_entry_i_picture_independent() {
        // depends_on=2 (I-picture), is_depended_on=1 (not disposable):
        //   0b00_10_01_00 = 0x24.
        let e = SdtpEntry::from_byte(0b00_10_01_00);
        assert!(e.is_independent());
        assert!(!e.is_disposable());
    }

    #[test]
    fn parse_sdtp_sized_from_stsz_count() {
        // FullBox header + 3 packed bytes, sized by the caller's count.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
        p.push(0b00_10_01_00); // I-frame, not disposable
        p.push(0b00_01_10_00); // P-frame, disposable
        p.push(0b00_01_01_00); // P-frame, not disposable
        let v = parse_sdtp(&p, 3).unwrap();
        assert_eq!(v.len(), 3);
        assert!(v[0].is_independent());
        assert!(!v[0].is_disposable());
        assert!(!v[1].is_independent());
        assert!(v[1].is_disposable());
        assert!(!v[2].is_independent());
        assert!(!v[2].is_disposable());
    }

    #[test]
    fn parse_sdtp_ignores_trailing_padding() {
        // 2 samples but 4 padded bytes present — only the first 2 count.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.push(0b00_10_01_00);
        p.push(0b00_01_10_00);
        p.push(0); // padding
        p.push(0); // padding
        let v = parse_sdtp(&p, 2).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].is_independent());
        assert!(v[1].is_disposable());
    }

    #[test]
    fn parse_sdtp_truncated_table_errors() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.push(0b00_10_01_00); // only 1 byte, claim 3 samples
        assert!(parse_sdtp(&p, 3).is_err());
    }

    #[test]
    fn parse_sdtp_zero_samples_is_empty() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        let v = parse_sdtp(&p, 0).unwrap();
        assert!(v.is_empty());
    }
}
