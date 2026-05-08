//! Public demuxer entry point.
//!
//! Walks the QTFF box hierarchy once, builds per-track sample tables,
//! then emits packets one at a time in interleaved file-offset order
//! (round-robins across tracks the way QuickTime stores chunks). The
//! interleave choice is a behavioural decision the round-1 spec
//! constrains only loosely; we sort sample entries across all tracks
//! by file offset before emitting, which yields a globally
//! monotonically-increasing read pattern friendly to disk and
//! mmap-backed inputs.

use std::io::{Read, Seek, SeekFrom};

use crate::atom::{
    self, read_atom_header, read_payload, walk_children, AtomHeader, CO64, DREF, ELST, FREE, FTYP,
    GMHD, HDLR, MDHD, MDIA, MINF, MOOV, MVHD, SKIP, SMHD, STBL, STCO, STSC, STSD, STSS, STSZ, STTS,
    TKHD, TRAK, VMHD, WIDE,
};
use crate::header::{parse_ftyp, parse_hdlr, parse_mdhd, parse_mvhd, parse_tkhd, Ftyp, Mvhd};
use crate::sample_table::{
    parse_co64, parse_stco, parse_stsc, parse_stss, parse_stsz, parse_stts, SampleEntry,
    SampleTable,
};
use crate::track::{parse_stsd, Track};

#[cfg(feature = "registry")]
use oxideav_core::{
    CodecId, CodecParameters, CodecResolver, CodecTag, Demuxer, Error, NullCodecResolver, Packet,
    ProbeContext, ReadSeek, Result, StreamInfo, TimeBase,
};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, ReadSeek, Result};

/// Round-1 demuxer. Lifetime is bounded by the input reader; on
/// `open` we walk `moov` once and cache enough state to stream
/// packets without reseeking the index.
pub struct MovDemuxer {
    input: Box<dyn ReadSeek>,
    pub ftyp: Option<Ftyp>,
    pub mvhd: Option<Mvhd>,
    pub tracks: Vec<Track>,
    /// Pre-flattened sample queue, sorted by file offset for friendly
    /// I/O patterns. Each entry is `(stream_index, sample)`.
    samples: Vec<(u32, SampleEntry)>,
    /// Cursor into `samples` for the next packet to emit.
    next: usize,
    #[cfg(feature = "registry")]
    streams: Vec<StreamInfo>,
}

impl MovDemuxer {
    /// Parse the container header and build the sample-table index.
    pub fn open(mut input: Box<dyn ReadSeek>) -> Result<Self> {
        Self::open_with_resolver_inner(&mut input, None)?;
        // We re-seek to the start, then walk fresh. The internal
        // helper uses the fully-mutable `input` — easiest is to
        // delegate to the resolver-aware ctor with a null resolver.
        Self::open_with(input, &NULL_RESOLVER)
    }

    /// Parse the container, using `resolver` to map sample-description
    /// FourCCs to oxideav `CodecId`s.
    pub fn open_with(
        mut input: Box<dyn ReadSeek>,
        resolver: &dyn CodecResolverShim,
    ) -> Result<Self> {
        input.seek(SeekFrom::Start(0))?;
        let total_len = input.seek(SeekFrom::End(0))?;
        input.seek(SeekFrom::Start(0))?;

        let mut ftyp: Option<Ftyp> = None;
        let mut mvhd: Option<Mvhd> = None;
        let mut tracks: Vec<Track> = Vec::new();

        // Top-level walk — accept arbitrary order, recognise the
        // common atoms, skip everything else.
        loop {
            let pos = input.stream_position()?;
            if pos >= total_len {
                break;
            }
            let hdr = match read_atom_header(input.as_mut())? {
                Some(h) => h,
                None => break,
            };
            let body_end = hdr
                .total_size
                .map(|t| hdr.payload_offset + (t - hdr.header_len))
                .unwrap_or(total_len);

            match &hdr.fourcc {
                t if t == &FTYP => {
                    let payload = read_payload(input.as_mut(), &hdr)?;
                    ftyp = Some(parse_ftyp(&payload)?);
                }
                t if t == &MOOV => {
                    parse_moov(input.as_mut(), &hdr, body_end, &mut mvhd, &mut tracks)?;
                }
                t if t == &FREE || t == &SKIP || t == &WIDE => {
                    // free-space atoms — skip
                }
                _ => {
                    // mdat or unknown — ignored at the top level.
                }
            }

            input.seek(SeekFrom::Start(body_end))?;
        }

        if mvhd.is_none() {
            return Err(Error::invalid("MOV: no moov/mvhd found"));
        }
        if tracks.is_empty() {
            return Err(Error::invalid("MOV: moov contains no tracks"));
        }

        // Resolve codec ids per-track using the provided resolver
        // (only when the registry feature is on). The resolved
        // `CodecId` is stored alongside the per-track stream record.
        #[cfg(feature = "registry")]
        let streams = build_streams(&tracks, resolver);

        // Flatten sample tables into a globally offset-sorted queue.
        let mut samples: Vec<(u32, SampleEntry)> = Vec::new();
        for (track_idx, t) in tracks.iter().enumerate() {
            for sample in t.sample_table.iter_samples() {
                let s = sample?;
                samples.push((track_idx as u32, s));
            }
        }
        samples.sort_by_key(|(_, s)| s.offset);

        // Touch the resolver to silence unused warnings on the
        // standalone build path.
        let _ = resolver;

        Ok(Self {
            input,
            ftyp,
            mvhd,
            tracks,
            samples,
            next: 0,
            #[cfg(feature = "registry")]
            streams,
        })
    }

    // Stub used by `open()` to validate the container before we
    // recurse with the real ctor; bails if the input is too short
    // to even hold an `ftyp`.
    fn open_with_resolver_inner(input: &mut Box<dyn ReadSeek>, _: Option<()>) -> Result<()> {
        let pos = input.stream_position()?;
        let total = input.seek(SeekFrom::End(0))?;
        input.seek(SeekFrom::Start(pos))?;
        if total < 16 {
            return Err(Error::invalid("MOV: input too small to be a QTFF file"));
        }
        Ok(())
    }

    /// Read the next sample's bytes from the input. Returns
    /// `(stream_index, sample, data)`.
    pub fn read_next(&mut self) -> Result<(u32, SampleEntry, Vec<u8>)> {
        if self.next >= self.samples.len() {
            return Err(Error::Eof);
        }
        let (stream_idx, sample) = self.samples[self.next];
        self.next += 1;
        self.input.seek(SeekFrom::Start(sample.offset))?;
        let mut buf = vec![0u8; sample.size as usize];
        self.input.read_exact(&mut buf)?;
        Ok((stream_idx, sample, buf))
    }

    /// Whether more samples are available.
    pub fn remaining(&self) -> usize {
        self.samples.len().saturating_sub(self.next)
    }
}

#[cfg(feature = "registry")]
fn build_streams(tracks: &[Track], resolver: &dyn CodecResolver) -> Vec<StreamInfo> {
    let mut out = Vec::with_capacity(tracks.len());
    for (i, t) in tracks.iter().enumerate() {
        let mut params = if t.is_video() {
            CodecParameters::video(CodecId::new("unknown"))
        } else if t.is_audio() {
            CodecParameters::audio(CodecId::new("unknown"))
        } else {
            CodecParameters::data(CodecId::new("unknown"))
        };
        if let Some(format) = t.primary_format() {
            let tag = CodecTag::fourcc(&format);
            let mut ctx = ProbeContext::new(&tag);
            if let Some(desc) = t.sample_descriptions.first() {
                ctx = ctx.header(&desc.extra);
                if t.is_audio() {
                    ctx = ctx
                        .channels(desc.channels)
                        .bits(desc.bits_per_sample)
                        .sample_rate(desc.sample_rate);
                } else if t.is_video() {
                    ctx = ctx.width(desc.width as u32).height(desc.height as u32);
                }
            }
            if let Some(id) = resolver.resolve_tag(&ctx) {
                params.codec_id = id;
            }
            params = params.with_tag(tag);
            if t.is_audio() {
                if let Some(desc) = t.sample_descriptions.first() {
                    params.channels = Some(desc.channels);
                    params.sample_rate = Some(desc.sample_rate);
                    if !desc.extra.is_empty() {
                        params.extradata = desc.extra.clone();
                    }
                }
            } else if t.is_video() {
                if let Some(desc) = t.sample_descriptions.first() {
                    if desc.width != 0 {
                        params.width = Some(desc.width as u32);
                    }
                    if desc.height != 0 {
                        params.height = Some(desc.height as u32);
                    }
                    if !desc.extra.is_empty() {
                        params.extradata = desc.extra.clone();
                    }
                }
            }
        }
        let timescale = if t.mdhd.time_scale > 0 {
            t.mdhd.time_scale as i64
        } else {
            // Fallback per QTFF p. 56 — the media must declare a
            // non-zero time_scale; we degrade to 1 to keep the
            // pipeline running rather than reject the whole file.
            1
        };
        out.push(StreamInfo {
            index: i as u32,
            time_base: TimeBase::new(1, timescale),
            duration: Some(t.mdhd.duration as i64),
            start_time: Some(0),
            params,
        });
    }
    out
}

fn parse_moov<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    body_end: u64,
    mvhd: &mut Option<Mvhd>,
    tracks: &mut Vec<Track>,
) -> Result<()> {
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &MVHD => {
                let body = read_payload(r, child)?;
                *mvhd = Some(parse_mvhd(&body)?);
            }
            t if t == &TRAK => {
                let track = parse_trak(r, child)?;
                tracks.push(track);
            }
            _ => {}
        }
        Ok(())
    })
}

fn parse_trak<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<Track> {
    let mut track = Track::default();
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;

    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &TKHD => {
                let body = read_payload(r, child)?;
                track.tkhd = parse_tkhd(&body)?;
            }
            t if t == &MDIA => {
                parse_mdia(r, child, &mut track)?;
            }
            t if t == &ELST || t == &atom::EDTS => {
                // Edit list — round-2 candidate. Skipped here.
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(track)
}

fn parse_mdia<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    track: &mut Track,
) -> Result<()> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &MDHD => {
                let body = read_payload(r, child)?;
                track.mdhd = parse_mdhd(&body)?;
            }
            t if t == &HDLR => {
                let body = read_payload(r, child)?;
                track.hdlr = parse_hdlr(&body)?;
            }
            t if t == &MINF => {
                parse_minf(r, child, track)?;
            }
            _ => {}
        }
        Ok(())
    })
}

fn parse_minf<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    track: &mut Track,
) -> Result<()> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &VMHD || t == &SMHD || t == &GMHD || t == &DREF => {
                // Header / data-info — captured at the type level via hdlr.
            }
            t if t == &STBL => {
                parse_stbl(r, child, track)?;
            }
            _ => {}
        }
        Ok(())
    })
}

fn parse_stbl<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
    track: &mut Track,
) -> Result<()> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;

    let mut stsd_payload: Option<Vec<u8>> = None;
    let mut table = SampleTable::default();
    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &STSD => {
                stsd_payload = Some(read_payload(r, child)?);
            }
            t if t == &STTS => {
                let body = read_payload(r, child)?;
                table.stts = parse_stts(&body)?;
            }
            t if t == &STSC => {
                let body = read_payload(r, child)?;
                table.stsc = parse_stsc(&body)?;
            }
            t if t == &STSZ => {
                let body = read_payload(r, child)?;
                let (def, n, tab) = parse_stsz(&body)?;
                table.stsz_default_size = def;
                table.stsz_count = n;
                table.stsz_table = tab;
            }
            t if t == &STCO => {
                let body = read_payload(r, child)?;
                table.chunk_offsets = parse_stco(&body)?;
            }
            t if t == &CO64 => {
                let body = read_payload(r, child)?;
                table.chunk_offsets = parse_co64(&body)?;
            }
            t if t == &STSS => {
                let body = read_payload(r, child)?;
                table.stss = parse_stss(&body)?;
            }
            _ => {}
        }
        Ok(())
    })?;

    // stsd parses last because it needs `track.hdlr` to discriminate
    // video vs audio — `hdlr` has already been populated by
    // `parse_mdia` before `parse_minf` ran.
    if let Some(payload) = stsd_payload {
        track.sample_descriptions = parse_stsd(&payload, &track.hdlr)?;
    }
    track.sample_table = table;
    Ok(())
}

// ─────────────── CodecResolver shim for standalone builds ───────────────
//
// When the `registry` feature is off the `oxideav_core::CodecResolver`
// trait is not in scope. We provide a tiny ABI-compatible shim so the
// public surface of `MovDemuxer::open_with` stays unchanged across
// both builds — the standalone resolver simply returns nothing.

#[cfg(feature = "registry")]
pub use oxideav_core::CodecResolver as CodecResolverShim;

#[cfg(not(feature = "registry"))]
pub trait CodecResolverShim: Sync {}
#[cfg(not(feature = "registry"))]
impl<T: Sync> CodecResolverShim for T {}

#[cfg(feature = "registry")]
const NULL_RESOLVER: NullCodecResolver = NullCodecResolver;
#[cfg(not(feature = "registry"))]
const NULL_RESOLVER: () = ();

// ─────────────── Demuxer trait impl ───────────────

#[cfg(feature = "registry")]
impl Demuxer for MovDemuxer {
    fn format_name(&self) -> &str {
        "mov"
    }

    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        let (stream_idx, sample, data) = self.read_next()?;
        let stream = &self.streams[stream_idx as usize];
        let mut pkt = Packet::new(stream_idx, stream.time_base, data)
            .with_dts(sample.dts as i64)
            .with_pts(sample.dts as i64)
            .with_keyframe(sample.keyframe);
        if sample.duration > 0 {
            pkt = pkt.with_duration(sample.duration as i64);
        }
        Ok(pkt)
    }

    fn duration_micros(&self) -> Option<i64> {
        // mvhd.duration is in mvhd.time_scale ticks; convert to µs.
        let m = self.mvhd.as_ref()?;
        if m.time_scale == 0 {
            return None;
        }
        Some((m.duration as i128 * 1_000_000 / m.time_scale as i128) as i64)
    }
}

// ─────────────── ContainerRegistry hook ───────────────

#[cfg(feature = "registry")]
pub fn open(
    input: Box<dyn oxideav_core::ReadSeek>,
    resolver: &dyn CodecResolver,
) -> Result<Box<dyn Demuxer>> {
    let d = MovDemuxer::open_with(input, resolver)?;
    Ok(Box::new(d))
}

// ─────────────── Probe ───────────────

#[cfg(feature = "registry")]
pub fn probe(p: &oxideav_core::ProbeData) -> u8 {
    if p.buf.len() < 16 {
        return 0;
    }
    // ftyp at offset 0 with major/compat brand including 'qt  '
    if &p.buf[4..8] == b"ftyp" {
        // brand at 8..12, minor at 12..16, compat at 16..
        let major = &p.buf[8..12];
        if major == b"qt  " {
            return 100;
        }
        // Scan compat brands (brand entries are 4 bytes each) — bound
        // the scan by the size32 of the ftyp atom if present.
        let size = u32::from_be_bytes([p.buf[0], p.buf[1], p.buf[2], p.buf[3]]) as usize;
        let upper = size.min(p.buf.len()).max(16);
        let mut o = 16;
        while o + 4 <= upper {
            if &p.buf[o..o + 4] == b"qt  " {
                return 90;
            }
            o += 4;
        }
        // Generic 'ftyp' but not QT-branded — let oxideav-mp4 win.
        return 0;
    }
    // Bare 'moov' first (legacy QuickTime) — weak match.
    if &p.buf[4..8] == b"moov" {
        return 40;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Minimal demuxer-open round trip: a hand-built one-track,
    /// one-sample QTFF file with brand 'qt  '. The synthetic builder
    /// is shared with the integration test in `tests/synth_minimal_qt.rs`
    /// but kept duplicated here so the unit tests don't depend on
    /// it.
    fn build_minimal_qt() -> Vec<u8> {
        // Layout:
        //   ftyp (qt  )
        //   mdat (8 bytes payload "PAYLOAD!")
        //   moov / mvhd / trak / tkhd / mdia / mdhd / hdlr (vide)
        //                       / minf / vmhd / dinf / dref
        //                              / stbl / stsd / stts / stsc / stsz / stco
        let payload = b"PAYLOAD!"; // 8 bytes
        let mut out = Vec::new();

        // --- ftyp ---
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"qt  "); // major
        ftyp.extend_from_slice(&0u32.to_be_bytes()); // minor
        ftyp.extend_from_slice(b"qt  ");
        push_atom(&mut out, *b"ftyp", &ftyp);

        // --- mdat ---
        let mdat_offset = out.len() + 8; // payload offset for stco
        push_atom(&mut out, *b"mdat", payload);
        let _ = mdat_offset;

        // --- moov ---
        let mut moov = Vec::new();

        // mvhd v0 (100 bytes payload)
        let mut mvhd = vec![0u8; 100];
        mvhd[12..16].copy_from_slice(&600u32.to_be_bytes()); // time_scale
        mvhd[16..20].copy_from_slice(&30u32.to_be_bytes()); // duration
        mvhd[20..24].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // rate
        mvhd[24..26].copy_from_slice(&0x0100i16.to_be_bytes()); // volume
        mvhd[96..100].copy_from_slice(&2u32.to_be_bytes()); // next_track_id
        push_atom(&mut moov, *b"mvhd", &mvhd);

        // trak
        let mut trak = Vec::new();

        // tkhd v0
        let mut tkhd = vec![0u8; 84];
        tkhd[3] = 0x07; // flags = enabled+in-movie+in-preview
        tkhd[12..16].copy_from_slice(&1u32.to_be_bytes()); // track_id
        tkhd[20..24].copy_from_slice(&30u32.to_be_bytes()); // duration in movie ts
        tkhd[76..80].copy_from_slice(&((320u32) << 16).to_be_bytes()); // width
        tkhd[80..84].copy_from_slice(&((240u32) << 16).to_be_bytes()); // height
        push_atom(&mut trak, *b"tkhd", &tkhd);

        // mdia
        let mut mdia = Vec::new();

        // mdhd v0 (24 bytes)
        let mut mdhd = vec![0u8; 24];
        mdhd[12..16].copy_from_slice(&600u32.to_be_bytes()); // time_scale (matches video)
        mdhd[16..20].copy_from_slice(&30u32.to_be_bytes()); // duration
        push_atom(&mut mdia, *b"mdhd", &mdhd);

        // hdlr (with empty counted name → 25 bytes minimum)
        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        hdlr.extend_from_slice(b"mhlr"); // component_type
        hdlr.extend_from_slice(b"vide"); // component_subtype
        hdlr.extend_from_slice(&[0u8; 12]); // manuf+flags+flags_mask
        hdlr.push(0); // empty Pascal-string name
        push_atom(&mut mdia, *b"hdlr", &hdlr);

        // minf
        let mut minf = Vec::new();

        // vmhd (12-byte fixed: ver+flags + graphics_mode:2 + opcolor:6)
        let mut vmhd = vec![0u8; 12];
        vmhd[3] = 0x01; // no-lean-ahead
        push_atom(&mut minf, *b"vmhd", &vmhd);

        // stbl
        let mut stbl = Vec::new();

        // stsd: 1 entry, format='rle ', width=320, height=240
        let mut stsd = Vec::new();
        stsd.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        stsd.extend_from_slice(&1u32.to_be_bytes()); // n=1
        let entry_size: u32 = 86;
        stsd.extend_from_slice(&entry_size.to_be_bytes());
        stsd.extend_from_slice(b"rle "); // Apple Animation
        stsd.extend_from_slice(&[0u8; 6]); // reserved
        stsd.extend_from_slice(&1u16.to_be_bytes()); // dref index
        let mut vbody = vec![0u8; 70];
        vbody[24..26].copy_from_slice(&320u16.to_be_bytes());
        vbody[26..28].copy_from_slice(&240u16.to_be_bytes());
        stsd.extend_from_slice(&vbody);
        push_atom(&mut stbl, *b"stsd", &stsd);

        // stts: 1 entry (count=1, duration=30)
        let mut stts = Vec::new();
        stts.extend_from_slice(&0u32.to_be_bytes());
        stts.extend_from_slice(&1u32.to_be_bytes());
        stts.extend_from_slice(&1u32.to_be_bytes()); // count
        stts.extend_from_slice(&30u32.to_be_bytes()); // duration
        push_atom(&mut stbl, *b"stts", &stts);

        // stsc: 1 entry (first_chunk=1, samples_per_chunk=1, sd_id=1)
        let mut stsc = Vec::new();
        stsc.extend_from_slice(&0u32.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        push_atom(&mut stbl, *b"stsc", &stsc);

        // stsz: constant size = 8, count = 1
        let mut stsz = Vec::new();
        stsz.extend_from_slice(&0u32.to_be_bytes());
        stsz.extend_from_slice(&8u32.to_be_bytes());
        stsz.extend_from_slice(&1u32.to_be_bytes());
        push_atom(&mut stbl, *b"stsz", &stsz);

        // stco: 1 chunk at offset where mdat payload lives.
        // Compute that offset given the partial buffer state. ftyp size
        // = 8 + 12 = 20, mdat header = 8 bytes → mdat payload @ 28.
        let stco_payload_offset: u32 = 28;
        let mut stco = Vec::new();
        stco.extend_from_slice(&0u32.to_be_bytes());
        stco.extend_from_slice(&1u32.to_be_bytes());
        stco.extend_from_slice(&stco_payload_offset.to_be_bytes());
        push_atom(&mut stbl, *b"stco", &stco);

        push_atom(&mut minf, *b"stbl", &stbl);
        push_atom(&mut mdia, *b"minf", &minf);
        push_atom(&mut trak, *b"mdia", &mdia);
        push_atom(&mut moov, *b"trak", &trak);
        push_atom(&mut out, *b"moov", &moov);

        out
    }

    fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
        let size: u32 = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(body);
    }

    #[cfg(feature = "registry")]
    #[test]
    fn open_minimal_qt_yields_one_packet() {
        let bytes = build_minimal_qt();
        let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes.clone()));
        let mut d = MovDemuxer::open(cur).unwrap();
        assert!(d.ftyp.as_ref().unwrap().is_quicktime());
        assert_eq!(d.tracks.len(), 1);
        assert!(d.tracks[0].is_video());
        assert_eq!(d.tracks[0].primary_format(), Some(*b"rle "));
        // Demuxer trait surface
        assert_eq!(d.streams().len(), 1);
        let pkt = d.next_packet().unwrap();
        assert_eq!(pkt.stream_index, 0);
        assert_eq!(pkt.data, b"PAYLOAD!".to_vec());
        assert_eq!(pkt.dts, Some(0));
        assert!(pkt.flags.keyframe);
        // Past-the-end yields Eof
        assert!(matches!(d.next_packet(), Err(Error::Eof)));
    }

    #[cfg(feature = "registry")]
    #[test]
    fn probe_recognises_qt_brand() {
        let bytes = build_minimal_qt();
        let pd = oxideav_core::ProbeData {
            buf: &bytes,
            ext: Some("mov"),
        };
        assert_eq!(probe(&pd), 100);
    }

    #[cfg(feature = "registry")]
    #[test]
    fn probe_rejects_random_bytes() {
        let pd = oxideav_core::ProbeData {
            buf: &[0u8; 32],
            ext: None,
        };
        assert_eq!(probe(&pd), 0);
    }
}
