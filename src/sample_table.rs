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
}

impl<'a> SampleIter<'a> {
    fn new(t: &'a SampleTable) -> Self {
        let cursor_in_chunk = t.chunk_offsets.first().copied().unwrap_or(0);
        let stts_remaining_in_run = t.stts.first().map(|e| e.sample_count).unwrap_or(0);
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
        let entry = SampleEntry {
            index: self.sample_idx,
            offset,
            size,
            dts: self.dts,
            duration: dur,
            sample_description_id: self.current_sample_description_id(),
            keyframe: kf,
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
        };
        let v: Vec<_> = table.iter_samples().collect::<Result<_>>().unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].offset, 512);
        assert_eq!(v[0].size, 42);
        assert_eq!(v[0].dts, 0);
        assert_eq!(v[0].duration, 100);
        assert!(v[0].keyframe);
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
}
