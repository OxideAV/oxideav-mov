//! Per-track aggregation: the `tkhd` + `mdhd` + `hdlr` + sample
//! description + sample table for a single QTFF track.
//!
//! The `stsd` (sample description) atom is parsed just enough to
//! pull out the data-format FourCC of its first entry — that is
//! what gets handed to `oxideav_core::CodecResolver` to map to a
//! `CodecId`. Per-codec config blobs (e.g. `avcC`/`hvcC`/`esds`/
//! Apple's `wave` audio extension) are captured as raw bytes in
//! [`SampleDescription::extra`] for downstream codec crates.

use crate::header::{Hdlr, Mdhd, Tkhd};
use crate::sample_table::SampleTable;

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// One sample-description-table entry. QTFF p. 70 ("Sample
/// Description Atoms") — the first 16 bytes are universal:
/// `[size:4][format:4][reserved:6][data_reference_index:2]`. Per-
/// media-type fields follow (Video Sample Description: pp. 92–94,
/// Sound Sample Description: pp. 100–102) and are kept here as
/// raw bytes plus parsed dims/sample-rate when we recognise the
/// media type.
#[derive(Clone, Debug, Default)]
pub struct SampleDescription {
    pub format: [u8; 4],
    pub data_reference_index: u16,
    /// Width in pixels (video sample descriptions only).
    pub width: u16,
    /// Height in pixels (video sample descriptions only).
    pub height: u16,
    /// Audio: number of channels (1 = mono, 2 = stereo).
    pub channels: u16,
    /// Audio: bits per sample.
    pub bits_per_sample: u16,
    /// Audio: sample rate (16.16 fixed-point, integer portion in
    /// QTFF v0; matches `mdhd.time_scale` per QTFF p. 100 last
    /// paragraph).
    pub sample_rate: u32,
    /// Codec-specific blob that follows the sample-description
    /// fixed fields (everything after byte 86 for video, after byte
    /// 36 for audio v0). Suitable for handing as extradata to a
    /// codec.
    pub extra: Vec<u8>,
}

/// One track's accumulated state.
#[derive(Clone, Debug, Default)]
pub struct Track {
    pub tkhd: Tkhd,
    pub mdhd: Mdhd,
    pub hdlr: Hdlr,
    /// Sample-description table — at least one entry per QTFF p. 69.
    pub sample_descriptions: Vec<SampleDescription>,
    pub sample_table: SampleTable,
}

impl Track {
    /// Track type label `"vide"` / `"soun"` / unknown FourCC, derived
    /// from the `hdlr` component subtype.
    pub fn type_str(&self) -> &str {
        std::str::from_utf8(&self.hdlr.component_subtype).unwrap_or("????")
    }

    /// True for tracks whose hdlr carries `vide`.
    pub fn is_video(&self) -> bool {
        self.hdlr.is_video()
    }

    /// True for tracks whose hdlr carries `soun`.
    pub fn is_audio(&self) -> bool {
        self.hdlr.is_audio()
    }

    /// First sample description's data-format FourCC. The QTFF
    /// guarantees at least one entry exists when the track has
    /// data (p. 69).
    pub fn primary_format(&self) -> Option<[u8; 4]> {
        self.sample_descriptions.first().map(|d| d.format)
    }
}

/// Parse a `stsd` payload: count + N × per-entry record. Layout per
/// QTFF p. 70 figure 2-27.
pub fn parse_stsd(payload: &[u8], hdlr: &Hdlr) -> Result<Vec<SampleDescription>> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: stsd payload < 8 bytes"));
    }
    let _ver_flags = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let n = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let mut out = Vec::with_capacity(n as usize);
    let mut p = 8usize;
    for _ in 0..n {
        if p + 16 > payload.len() {
            return Err(Error::invalid("MOV: stsd entry truncated"));
        }
        let size = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]]);
        if size < 16 || (p + size as usize) > payload.len() {
            return Err(Error::invalid("MOV: stsd entry size invalid"));
        }
        let mut format = [0u8; 4];
        format.copy_from_slice(&payload[p + 4..p + 8]);
        // 6 bytes reserved
        let dref = u16::from_be_bytes([payload[p + 14], payload[p + 15]]);
        let mut entry = SampleDescription {
            format,
            data_reference_index: dref,
            ..SampleDescription::default()
        };

        let body_off = p + 16;
        let body_end = p + size as usize;
        let body = &payload[body_off..body_end];

        if hdlr.is_video() && body.len() >= 70 {
            // Video sample description (QTFF p. 92):
            //   ver:2 rev:2 vendor:4 temp_q:4 spatial_q:4
            //   width:2 height:2 hres:4 vres:4 data_size:4 frame_count:2
            //   compressor_name:32 depth:2 color_table_id:2
            // → 70 bytes of fixed fields; extras (e.g. avcC/clap/colr)
            //   follow.
            entry.width = u16::from_be_bytes([body[24], body[25]]);
            entry.height = u16::from_be_bytes([body[26], body[27]]);
            entry.extra = body[70..].to_vec();
        } else if hdlr.is_audio() && body.len() >= 20 {
            // Sound sample description v0 (QTFF p. 100):
            //   ver:2 rev:2 vendor:4 channels:2 sample_size:2
            //   compression_id:2 packet_size:2 sample_rate:4
            // → 20 bytes; v1 adds 16 bytes more (samples_per_packet,
            //   bytes_per_packet, bytes_per_frame, bytes_per_sample).
            let version = u16::from_be_bytes([body[0], body[1]]);
            entry.channels = u16::from_be_bytes([body[8], body[9]]);
            entry.bits_per_sample = u16::from_be_bytes([body[10], body[11]]);
            entry.sample_rate = u32::from_be_bytes([body[16], body[17], body[18], body[19]]) >> 16;
            // Sample rate is 16.16; integer portion lives in the high 16 bits.
            let extra_start = match version {
                0 => 20usize,
                1 if body.len() >= 36 => 36,
                _ => 20,
            };
            if body.len() > extra_start {
                entry.extra = body[extra_start..].to_vec();
            }
        } else {
            // Unknown handler — keep whatever follows the universal 16-byte
            // header. Useful for `subt`/`tmcd`/`meta` tracks in later rounds.
            entry.extra = body.to_vec();
        }

        out.push(entry);
        p = body_end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vide_hdlr() -> Hdlr {
        Hdlr {
            component_type: *b"mhlr",
            component_subtype: *b"vide",
            component_manufacturer: [0; 4],
        }
    }

    fn soun_hdlr() -> Hdlr {
        Hdlr {
            component_type: *b"mhlr",
            component_subtype: *b"soun",
            component_manufacturer: [0; 4],
        }
    }

    #[test]
    fn stsd_video_entry_extracts_dims() {
        // Build one stsd entry: size=86 (16 universal + 70 video fixed),
        // format='avc1', dims 1920×1080.
        let mut p = Vec::new();
        // ver+flags
        p.extend_from_slice(&0u32.to_be_bytes());
        // n_entries=1
        p.extend_from_slice(&1u32.to_be_bytes());
        // entry: size=86, format='avc1'
        let entry_size: u32 = 86;
        p.extend_from_slice(&entry_size.to_be_bytes());
        p.extend_from_slice(b"avc1");
        // 6 reserved
        p.extend_from_slice(&[0u8; 6]);
        // data_reference_index=1
        p.extend_from_slice(&1u16.to_be_bytes());
        // 70-byte video fixed body. width @ offset 24, height @ 26.
        let mut body = vec![0u8; 70];
        body[24..26].copy_from_slice(&1920u16.to_be_bytes());
        body[26..28].copy_from_slice(&1080u16.to_be_bytes());
        p.extend_from_slice(&body);

        let v = parse_stsd(&p, &vide_hdlr()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(&v[0].format, b"avc1");
        assert_eq!(v[0].data_reference_index, 1);
        assert_eq!(v[0].width, 1920);
        assert_eq!(v[0].height, 1080);
    }

    #[test]
    fn stsd_audio_entry_extracts_rate_channels() {
        // size = 16 + 20 = 36 ; format='sowt' (16-bit LE PCM) ; ch=2, bits=16, rate=44100<<16
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        let entry_size: u32 = 36;
        p.extend_from_slice(&entry_size.to_be_bytes());
        p.extend_from_slice(b"sowt");
        p.extend_from_slice(&[0u8; 6]);
        p.extend_from_slice(&1u16.to_be_bytes());
        // 20-byte sound v0 body
        let mut body = vec![0u8; 20];
        // version=0
        // channels @ 8..10 = 2
        body[8..10].copy_from_slice(&2u16.to_be_bytes());
        // bits @ 10..12 = 16
        body[10..12].copy_from_slice(&16u16.to_be_bytes());
        // sample_rate @ 16..20 = 44100 << 16
        body[16..20].copy_from_slice(&((44100u32) << 16).to_be_bytes());
        p.extend_from_slice(&body);

        let v = parse_stsd(&p, &soun_hdlr()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(&v[0].format, b"sowt");
        assert_eq!(v[0].channels, 2);
        assert_eq!(v[0].bits_per_sample, 16);
        assert_eq!(v[0].sample_rate, 44100);
    }
}
