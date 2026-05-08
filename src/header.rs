//! Movie / track / media header atoms.
//!
//! Parsers for `ftyp`, `mvhd`, `tkhd`, `mdhd`, `hdlr` based on Apple
//! QuickTime File Format Specification (QTFF, 2001-03-01):
//!
//! * `mvhd` — pp. 33–35 (Figure 2-3, 100 bytes payload, version 0)
//! * `tkhd` — pp. 41–43 (Figure 2-7, 84 bytes payload, version 0)
//! * `mdhd` — pp. 55–57 (Figure 2-16, 24 bytes payload, version 0)
//! * `hdlr` — pp. 57–58 (Figure 2-17, 24 bytes fixed + counted name)
//!
//! ISO BMFF (ISO/IEC 14496-12) inherits these atoms with an additional
//! version 1 layout where the 32-bit time fields become 64-bit. QTFF
//! itself only defines version 0; we accept version 1 for cross-
//! compatibility because real-world `.mov` writers (notably ffmpeg
//! when emitting `ftyp` `qt  ` brand) frequently use v1 fields.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Parsed `ftyp` atom (QTFF p. 24, "Overview of the File Format" plus
/// ISO BMFF §4.3 — the spec was added in QTFF supplements after 2001
/// and the "qt  " brand is the canonical QuickTime marker).
#[derive(Clone, Debug, Default)]
pub struct Ftyp {
    pub major_brand: [u8; 4],
    pub minor_version: u32,
    pub compatible_brands: Vec<[u8; 4]>,
}

impl Ftyp {
    /// Returns `true` when this `ftyp` declares the `qt  ` brand
    /// (either as major or in the compatible-brands list). The
    /// trailing two ASCII spaces are part of the brand.
    pub fn is_quicktime(&self) -> bool {
        const QT: [u8; 4] = *b"qt  ";
        self.major_brand == QT || self.compatible_brands.contains(&QT)
    }
}

/// Parse an `ftyp` payload.
pub fn parse_ftyp(payload: &[u8]) -> Result<Ftyp> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: ftyp payload < 8 bytes"));
    }
    let mut major = [0u8; 4];
    major.copy_from_slice(&payload[..4]);
    let minor = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);

    let rest = &payload[8..];
    if rest.len() % 4 != 0 {
        return Err(Error::invalid("MOV: ftyp compatible-brands not 4-aligned"));
    }
    let mut brands = Vec::with_capacity(rest.len() / 4);
    for chunk in rest.chunks_exact(4) {
        let mut b = [0u8; 4];
        b.copy_from_slice(chunk);
        brands.push(b);
    }
    Ok(Ftyp {
        major_brand: major,
        minor_version: minor,
        compatible_brands: brands,
    })
}

/// Parsed `mvhd` (movie header). Time-related fields are reported in
/// the movie's `time_scale`. QTFF p. 33: `time_scale` is the number
/// of time units per second; `duration` is in time-scale units.
#[derive(Clone, Debug, Default)]
pub struct Mvhd {
    pub version: u8,
    pub creation_time: u64,
    pub modification_time: u64,
    pub time_scale: u32,
    pub duration: u64,
    /// 16.16 fixed-point preferred playback rate (1.0 = 0x00010000).
    pub rate: u32,
    /// 8.8 fixed-point preferred volume (1.0 = 0x0100).
    pub volume: i16,
    pub next_track_id: u32,
}

/// Parse the payload of an `mvhd` atom.
pub fn parse_mvhd(payload: &[u8]) -> Result<Mvhd> {
    if payload.len() < 4 {
        return Err(Error::invalid("MOV: mvhd payload < 4 bytes (no version)"));
    }
    let version = payload[0];
    let mut p = 4usize; // skip 1 version + 3 flags

    let (creation_time, modification_time, time_scale, duration, end_offset) = match version {
        0 => {
            need(payload, p, 16, "mvhd v0 fixed fields")?;
            let creation = read_u32(&payload[p..]) as u64;
            p += 4;
            let modif = read_u32(&payload[p..]) as u64;
            p += 4;
            let ts = read_u32(&payload[p..]);
            p += 4;
            let dur = read_u32(&payload[p..]) as u64;
            p += 4;
            (creation, modif, ts, dur, p)
        }
        1 => {
            // ISO BMFF v1: 64-bit times + duration
            need(payload, p, 28, "mvhd v1 fixed fields")?;
            let creation = read_u64(&payload[p..]);
            p += 8;
            let modif = read_u64(&payload[p..]);
            p += 8;
            let ts = read_u32(&payload[p..]);
            p += 4;
            let dur = read_u64(&payload[p..]);
            p += 8;
            (creation, modif, ts, dur, p)
        }
        v => return Err(Error::invalid(format!("MOV: mvhd unknown version {v}"))),
    };
    p = end_offset;

    need(payload, p, 4 + 2, "mvhd rate+volume")?;
    let rate = read_u32(&payload[p..]);
    p += 4;
    let volume = i16::from_be_bytes([payload[p], payload[p + 1]]);
    p += 2;
    // 10 bytes reserved
    p += 10;
    // 36-byte matrix
    p += 36;
    // 24 bytes pre-defined (preview/poster/selection/current)
    p += 24;
    need(payload, p, 4, "mvhd next_track_id")?;
    let next_track_id = read_u32(&payload[p..]);

    Ok(Mvhd {
        version,
        creation_time,
        modification_time,
        time_scale,
        duration,
        rate,
        volume,
        next_track_id,
    })
}

/// Parsed `tkhd` (track header). Times are in the *movie* time scale
/// (QTFF p. 42, `Duration` paragraph).
#[derive(Clone, Debug, Default)]
pub struct Tkhd {
    pub version: u8,
    pub flags: u32,
    pub creation_time: u64,
    pub modification_time: u64,
    pub track_id: u32,
    pub duration: u64,
    pub layer: i16,
    pub alternate_group: i16,
    pub volume: i16,
    /// 16.16 fixed-point track width (pixels for video, 0 for audio).
    pub width_fp: u32,
    /// 16.16 fixed-point track height.
    pub height_fp: u32,
}

impl Tkhd {
    pub fn enabled(&self) -> bool {
        (self.flags & 0x0001) != 0
    }
    pub fn width(&self) -> u32 {
        self.width_fp >> 16
    }
    pub fn height(&self) -> u32 {
        self.height_fp >> 16
    }
}

/// Parse a `tkhd` payload.
pub fn parse_tkhd(payload: &[u8]) -> Result<Tkhd> {
    if payload.len() < 4 {
        return Err(Error::invalid("MOV: tkhd payload < 4 bytes"));
    }
    let version = payload[0];
    let flags = (payload[1] as u32) << 16 | (payload[2] as u32) << 8 | (payload[3] as u32);
    let mut p = 4usize;

    let (creation, modif, track_id, duration) = match version {
        0 => {
            need(payload, p, 20, "tkhd v0 fixed fields")?;
            let c = read_u32(&payload[p..]) as u64;
            p += 4;
            let m = read_u32(&payload[p..]) as u64;
            p += 4;
            let id = read_u32(&payload[p..]);
            p += 4;
            // 4 bytes reserved
            p += 4;
            let d = read_u32(&payload[p..]) as u64;
            p += 4;
            (c, m, id, d)
        }
        1 => {
            need(payload, p, 32, "tkhd v1 fixed fields")?;
            let c = read_u64(&payload[p..]);
            p += 8;
            let m = read_u64(&payload[p..]);
            p += 8;
            let id = read_u32(&payload[p..]);
            p += 4;
            p += 4;
            let d = read_u64(&payload[p..]);
            p += 8;
            (c, m, id, d)
        }
        v => return Err(Error::invalid(format!("MOV: tkhd unknown version {v}"))),
    };
    // 8 bytes reserved
    p += 8;
    need(payload, p, 8, "tkhd layer/alt-group/volume/reserved")?;
    let layer = i16::from_be_bytes([payload[p], payload[p + 1]]);
    p += 2;
    let alt_group = i16::from_be_bytes([payload[p], payload[p + 1]]);
    p += 2;
    let volume = i16::from_be_bytes([payload[p], payload[p + 1]]);
    p += 2;
    p += 2; // reserved
    p += 36; // matrix
    need(payload, p, 8, "tkhd width+height")?;
    let width_fp = read_u32(&payload[p..]);
    p += 4;
    let height_fp = read_u32(&payload[p..]);

    Ok(Tkhd {
        version,
        flags,
        creation_time: creation,
        modification_time: modif,
        track_id,
        duration,
        layer,
        alternate_group: alt_group,
        volume,
        width_fp,
        height_fp,
    })
}

/// Parsed `mdhd` (media header). Times are in the *media* time scale
/// (QTFF pp. 56, last paragraph).
#[derive(Clone, Debug, Default)]
pub struct Mdhd {
    pub version: u8,
    pub creation_time: u64,
    pub modification_time: u64,
    pub time_scale: u32,
    pub duration: u64,
    /// ISO 639-2/T language code packed as five-bit chars + 0x60 base
    /// (QTFF p. 197, "Language Code Values").
    pub language: u16,
    pub quality: u16,
}

/// Parse an `mdhd` payload.
pub fn parse_mdhd(payload: &[u8]) -> Result<Mdhd> {
    if payload.len() < 4 {
        return Err(Error::invalid("MOV: mdhd payload < 4 bytes"));
    }
    let version = payload[0];
    let mut p = 4usize;

    let (creation, modif, ts, dur) = match version {
        0 => {
            need(payload, p, 16, "mdhd v0 fixed fields")?;
            let c = read_u32(&payload[p..]) as u64;
            p += 4;
            let m = read_u32(&payload[p..]) as u64;
            p += 4;
            let t = read_u32(&payload[p..]);
            p += 4;
            let d = read_u32(&payload[p..]) as u64;
            p += 4;
            (c, m, t, d)
        }
        1 => {
            need(payload, p, 28, "mdhd v1 fixed fields")?;
            let c = read_u64(&payload[p..]);
            p += 8;
            let m = read_u64(&payload[p..]);
            p += 8;
            let t = read_u32(&payload[p..]);
            p += 4;
            let d = read_u64(&payload[p..]);
            p += 8;
            (c, m, t, d)
        }
        v => return Err(Error::invalid(format!("MOV: mdhd unknown version {v}"))),
    };
    need(payload, p, 4, "mdhd language+quality")?;
    let language = u16::from_be_bytes([payload[p], payload[p + 1]]);
    p += 2;
    let quality = u16::from_be_bytes([payload[p], payload[p + 1]]);

    Ok(Mdhd {
        version,
        creation_time: creation,
        modification_time: modif,
        time_scale: ts,
        duration: dur,
        language,
        quality,
    })
}

/// Parsed `hdlr`. The 4-byte `component_subtype` discriminates the
/// track type (`vide` / `soun` / `subt` / `text` / `tmcd` / `meta`
/// / …); see QTFF p. 58 paragraph 4.
#[derive(Clone, Debug, Default)]
pub struct Hdlr {
    pub component_type: [u8; 4],
    pub component_subtype: [u8; 4],
    pub component_manufacturer: [u8; 4],
}

impl Hdlr {
    pub fn is_video(&self) -> bool {
        self.component_subtype == *b"vide"
    }
    pub fn is_audio(&self) -> bool {
        self.component_subtype == *b"soun"
    }
}

/// Parse an `hdlr` payload.
///
/// QTFF p. 57 Figure 2-17 layout: 1 version + 3 flags + 4 component
/// type + 4 component subtype + 4 component manufacturer + 4 component
/// flags + 4 component flags mask + counted-Pascal-string name
/// (length byte then bytes; ISO BMFF re-uses the same slot for a NUL-
/// terminated UTF-8 name — we accept both and don't surface the name
/// in round 1).
pub fn parse_hdlr(payload: &[u8]) -> Result<Hdlr> {
    need(payload, 0, 4 + 5 * 4, "hdlr fixed fields")?;
    let mut ct = [0u8; 4];
    ct.copy_from_slice(&payload[4..8]);
    let mut cs = [0u8; 4];
    cs.copy_from_slice(&payload[8..12]);
    let mut cm = [0u8; 4];
    cm.copy_from_slice(&payload[12..16]);
    Ok(Hdlr {
        component_type: ct,
        component_subtype: cs,
        component_manufacturer: cm,
    })
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

    #[test]
    fn ftyp_qt_brand_recognised() {
        // major='qt  ', minor=0x200
        let mut p = Vec::new();
        p.extend_from_slice(b"qt  ");
        p.extend_from_slice(&0x0200u32.to_be_bytes());
        p.extend_from_slice(b"qt  ");
        let f = parse_ftyp(&p).unwrap();
        assert_eq!(f.major_brand, *b"qt  ");
        assert_eq!(f.minor_version, 0x200);
        assert!(f.is_quicktime());
        assert_eq!(f.compatible_brands.len(), 1);
    }

    #[test]
    fn ftyp_isom_with_qt_compat_recognised() {
        let mut p = Vec::new();
        p.extend_from_slice(b"isom");
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"qt  ");
        p.extend_from_slice(b"mp42");
        let f = parse_ftyp(&p).unwrap();
        assert!(f.is_quicktime());
    }

    #[test]
    fn ftyp_too_short_errors() {
        assert!(parse_ftyp(&[0u8; 4]).is_err());
    }

    #[test]
    fn mvhd_v0_round_trip() {
        // Build a synthetic v0 mvhd payload (100 bytes).
        let mut p = vec![0u8; 100];
        p[0] = 0; // version
                  // flags = 0
                  // creation_time @ 4..8 = 1
        p[7] = 1;
        // modification_time @ 8..12 = 2
        p[11] = 2;
        // time_scale @ 12..16 = 600
        p[14..16].copy_from_slice(&600u16.to_be_bytes());
        // duration @ 16..20 = 1200
        p[18..20].copy_from_slice(&1200u16.to_be_bytes());
        // rate @ 20..24 = 0x00010000
        p[20..24].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        // volume @ 24..26 = 0x0100
        p[24..26].copy_from_slice(&0x0100i16.to_be_bytes());
        // next_track_id @ 96..100 = 2
        p[99] = 2;
        let mvhd = parse_mvhd(&p).unwrap();
        assert_eq!(mvhd.version, 0);
        assert_eq!(mvhd.time_scale, 600);
        assert_eq!(mvhd.duration, 1200);
        assert_eq!(mvhd.rate, 0x0001_0000);
        assert_eq!(mvhd.next_track_id, 2);
    }

    #[test]
    fn tkhd_v0_dims_round_trip() {
        // 84-byte v0 payload.
        let mut p = vec![0u8; 84];
        // flags = 0x000007 (enabled + in-movie + in-preview)
        p[3] = 0x07;
        // track_id @ 12..16 = 1
        p[15] = 1;
        // width @ 76..80 = 320 << 16
        let w = 320u32 << 16;
        p[76..80].copy_from_slice(&w.to_be_bytes());
        // height @ 80..84 = 240 << 16
        let h = 240u32 << 16;
        p[80..84].copy_from_slice(&h.to_be_bytes());
        let t = parse_tkhd(&p).unwrap();
        assert!(t.enabled());
        assert_eq!(t.track_id, 1);
        assert_eq!(t.width(), 320);
        assert_eq!(t.height(), 240);
    }

    #[test]
    fn mdhd_v0_round_trip() {
        // 24-byte v0 payload.
        let mut p = vec![0u8; 24];
        // time_scale @ 12..16 = 30000
        p[12..16].copy_from_slice(&30000u32.to_be_bytes());
        // duration @ 16..20 = 60000
        p[16..20].copy_from_slice(&60000u32.to_be_bytes());
        let m = parse_mdhd(&p).unwrap();
        assert_eq!(m.time_scale, 30000);
        assert_eq!(m.duration, 60000);
    }

    #[test]
    fn hdlr_subtype_classifies_track() {
        let mut p = vec![0u8; 24];
        p[8..12].copy_from_slice(b"vide");
        let h = parse_hdlr(&p).unwrap();
        assert!(h.is_video());
        assert!(!h.is_audio());
    }
}
