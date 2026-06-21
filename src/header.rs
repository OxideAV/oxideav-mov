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

    /// Walk this file-type box's `major_brand` + `compatible_brands`
    /// and classify each one against the well-known ISO BMFF / HEIF /
    /// MIAF / AVIF brand registry. Unknown brands are surfaced as
    /// [`BrandClass::Other`] with the raw 4-byte tag preserved so
    /// callers can still match on niche / vendor-specific brands.
    ///
    /// The order of the returned vector matches the on-wire order:
    /// `major_brand` first, then `compatible_brands` in declaration
    /// order. Duplicates (e.g. a brand listed both as major and as
    /// compatible) are preserved verbatim — readers that want a
    /// deduplicated set can collect into a `HashSet` themselves.
    pub fn brand_class(&self) -> Vec<BrandClass> {
        let mut out = Vec::with_capacity(1 + self.compatible_brands.len());
        out.push(BrandClass::classify(&self.major_brand));
        for b in &self.compatible_brands {
            out.push(BrandClass::classify(b));
        }
        out
    }

    /// Whether this `ftyp` carries any HEIC-family brand (`heic`,
    /// `heix`, `heim`, or `heis`). Per ISO/IEC 23008-12 §10.
    pub fn is_heic(&self) -> bool {
        self.brand_class().iter().any(BrandClass::is_heic_family)
    }

    /// Whether this `ftyp` carries any AVIF-family brand (`avif`,
    /// `avis`, `avio`). Per Alliance for Open Media AVIF spec
    /// (`https://aomediacodec.github.io/av1-avif/`) which delegates
    /// brand registration to MIAF (ISO/IEC 23000-22).
    pub fn is_avif(&self) -> bool {
        self.brand_class().iter().any(BrandClass::is_avif_family)
    }

    /// Whether this `ftyp` carries any MIAF brand — either the
    /// generic `mif1` / `mif2` brands (ISO/IEC 23000-22 §7.2) or any
    /// HEIC / AVIF derivative (which all entail MIAF conformance per
    /// §10).
    pub fn is_miaf(&self) -> bool {
        self.brand_class().iter().any(BrandClass::is_miaf_family)
    }
}

/// Classified file-type brand per the ISO BMFF / HEIF / MIAF / AVIF
/// registries. Used by [`Ftyp::brand_class`] to surface a strongly-
/// typed view of `ftyp::compatible_brands`.
///
/// The variants cover the brands that affect derivative-profile
/// behaviour in our parsers; everything else is preserved verbatim
/// in [`Self::Other`].
///
/// Spec sources:
/// - ISO/IEC 14496-12:2015 §8.5 (FileTypeBox / brands).
/// - ISO/IEC 14496-14 (`mp41`, `mp42`).
/// - ISO/IEC 23008-12:2017 §10 (HEIF brand registry: `heic`, `heix`,
///   `heim`, `heis`, `mif1`, `msf1`).
/// - ISO/IEC 23000-22 (MIAF: `mif1`, `mif2`, `MA1A`, `MA1B`).
/// - AOM AVIF spec (`avif`, `avis`, `avio`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BrandClass {
    /// `heic` — HEVC main-profile single-image (HEIF §10.2).
    Heic,
    /// `heix` — HEVC extended-profile single-image (HEIF §10.2.1).
    Heix,
    /// `heim` — HEVC multiview (HEIF §10.4).
    Heim,
    /// `heis` — HEVC scalable (HEIF §10.5).
    Heis,
    /// `hevc` — HEVC image sequence (HEIF §10.3).
    Hevc,
    /// `hevx` — HEVC extended image sequence (HEIF §10.3).
    Hevx,
    /// `avif` — AV1 single-image / collection (AOM AVIF).
    Avif,
    /// `avis` — AV1 image sequence (AOM AVIF).
    Avis,
    /// `avio` — AV1 sequence override (AOM AVIF).
    Avio,
    /// `mif1` — MIAF base (HEIF §10.1; ISO/IEC 23000-22 §7.2).
    Mif1,
    /// `mif2` — MIAF v2 base (ISO/IEC 23000-22 §7.2).
    Mif2,
    /// `msf1` — HEIF image sequence (HEIF §10.1).
    Msf1,
    /// `MA1A` — MIAF Annex A1 profile.
    Ma1a,
    /// `MA1B` — MIAF Annex A1 profile (basic).
    Ma1b,
    /// `mp41` — MPEG-4 v1 (ISO/IEC 14496-14).
    Mp41,
    /// `mp42` — MPEG-4 v2 (ISO/IEC 14496-14).
    Mp42,
    /// `isom` — ISO Base Media File Format reference (§8.5.2).
    Isom,
    /// `iso2` — ISOBMFF rev 2.
    Iso2,
    /// `iso3` — ISOBMFF rev 3.
    Iso3,
    /// `iso4` — ISOBMFF rev 4.
    Iso4,
    /// `iso5` — ISOBMFF rev 5.
    Iso5,
    /// `iso6` — ISOBMFF rev 6.
    Iso6,
    /// `iso7` — ISOBMFF rev 7.
    Iso7,
    /// `iso8` — ISOBMFF rev 8.
    Iso8,
    /// `iso9` — ISOBMFF rev 9.
    Iso9,
    /// `qt  ` — Apple QuickTime native brand.
    Qt,
    /// Unknown / vendor brand. The raw 4-byte tag is preserved so the
    /// caller can match on niche brands not modelled natively.
    Other([u8; 4]),
}

impl BrandClass {
    /// Map a raw 4-byte brand tag to a [`BrandClass`].
    pub fn classify(brand: &[u8; 4]) -> Self {
        match brand {
            b"heic" => BrandClass::Heic,
            b"heix" => BrandClass::Heix,
            b"heim" => BrandClass::Heim,
            b"heis" => BrandClass::Heis,
            b"hevc" => BrandClass::Hevc,
            b"hevx" => BrandClass::Hevx,
            b"avif" => BrandClass::Avif,
            b"avis" => BrandClass::Avis,
            b"avio" => BrandClass::Avio,
            b"mif1" => BrandClass::Mif1,
            b"mif2" => BrandClass::Mif2,
            b"msf1" => BrandClass::Msf1,
            b"MA1A" => BrandClass::Ma1a,
            b"MA1B" => BrandClass::Ma1b,
            b"mp41" => BrandClass::Mp41,
            b"mp42" => BrandClass::Mp42,
            b"isom" => BrandClass::Isom,
            b"iso2" => BrandClass::Iso2,
            b"iso3" => BrandClass::Iso3,
            b"iso4" => BrandClass::Iso4,
            b"iso5" => BrandClass::Iso5,
            b"iso6" => BrandClass::Iso6,
            b"iso7" => BrandClass::Iso7,
            b"iso8" => BrandClass::Iso8,
            b"iso9" => BrandClass::Iso9,
            b"qt  " => BrandClass::Qt,
            other => BrandClass::Other(*other),
        }
    }

    /// 4-byte FourCC of the underlying brand tag.
    pub fn fourcc(&self) -> [u8; 4] {
        match self {
            BrandClass::Heic => *b"heic",
            BrandClass::Heix => *b"heix",
            BrandClass::Heim => *b"heim",
            BrandClass::Heis => *b"heis",
            BrandClass::Hevc => *b"hevc",
            BrandClass::Hevx => *b"hevx",
            BrandClass::Avif => *b"avif",
            BrandClass::Avis => *b"avis",
            BrandClass::Avio => *b"avio",
            BrandClass::Mif1 => *b"mif1",
            BrandClass::Mif2 => *b"mif2",
            BrandClass::Msf1 => *b"msf1",
            BrandClass::Ma1a => *b"MA1A",
            BrandClass::Ma1b => *b"MA1B",
            BrandClass::Mp41 => *b"mp41",
            BrandClass::Mp42 => *b"mp42",
            BrandClass::Isom => *b"isom",
            BrandClass::Iso2 => *b"iso2",
            BrandClass::Iso3 => *b"iso3",
            BrandClass::Iso4 => *b"iso4",
            BrandClass::Iso5 => *b"iso5",
            BrandClass::Iso6 => *b"iso6",
            BrandClass::Iso7 => *b"iso7",
            BrandClass::Iso8 => *b"iso8",
            BrandClass::Iso9 => *b"iso9",
            BrandClass::Qt => *b"qt  ",
            BrandClass::Other(b) => *b,
        }
    }

    /// True for any `heic` / `heix` / `heim` / `heis` brand.
    pub fn is_heic_family(&self) -> bool {
        matches!(
            self,
            BrandClass::Heic | BrandClass::Heix | BrandClass::Heim | BrandClass::Heis
        )
    }

    /// True for any `avif` / `avis` / `avio` brand.
    pub fn is_avif_family(&self) -> bool {
        matches!(self, BrandClass::Avif | BrandClass::Avis | BrandClass::Avio)
    }

    /// True for any MIAF-family brand: the explicit `mif1` / `mif2`
    /// markers, the MIAF Annex A profiles (`MA1A` / `MA1B`), and any
    /// HEIC- or AVIF-family brand (each of which entails MIAF
    /// conformance per HEIF §10 / AVIF §3).
    pub fn is_miaf_family(&self) -> bool {
        matches!(
            self,
            BrandClass::Mif1 | BrandClass::Mif2 | BrandClass::Ma1a | BrandClass::Ma1b
        ) || self.is_heic_family()
            || self.is_avif_family()
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
    /// Raw 9-element 3×3 transformation matrix as stored on disk
    /// (QTFF p. 199 Figure 4-1). Layout:
    /// `[a b u; c d v; tx ty w]`. The first 8 are 16.16 fixed-point;
    /// the trailing column `[u v w]` is 2.30 fixed-point (only `w` is
    /// non-zero in normal display matrices, set to 1.0 = 0x40000000).
    /// Stored as 9 raw `i32` values to keep both the integer rotation
    /// classification and the 16.16 / 2.30 quirk available to callers.
    pub matrix: [i32; 9],
}

/// Coarse rotation classification of a `tkhd` matrix. We surface the
/// common 0/90/180/270 cases that almost every mobile camera writes;
/// any other matrix (skew, mirror, off-axis scale, …) falls into
/// `Other`. The classification is purely positional — it ignores
/// translation (`tx`/`ty`) so the result reflects the *visual*
/// orientation, not the placement on the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrackRotation {
    /// Identity: `[a d w] = [1 1 1]`, off-diagonals zero.
    #[default]
    None,
    /// 90° clockwise: `b = -1, c = 1, a = d = 0`.
    Rotate90,
    /// 180°: `a = d = -1, b = c = 0`.
    Rotate180,
    /// 270° clockwise (= 90° counter-clockwise): `b = 1, c = -1`.
    Rotate270,
    /// Anything else (skew, mirror, scale ≠ ±1).
    Other,
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

    /// Classify the `matrix` into a coarse rotation enum. The four
    /// recognised cases (None / 90 / 180 / 270) cover essentially
    /// every mobile-phone-rotated landscape recording; anything else
    /// returns [`TrackRotation::Other`].
    ///
    /// Evaluation looks only at the four 16.16 entries `[a, b, c, d]`
    /// against `±1.0` and zero. Translation, scale, and the 2.30
    /// trailing column are ignored.
    pub fn rotation(&self) -> TrackRotation {
        const ONE: i32 = 0x0001_0000;
        const NEG_ONE: i32 = -0x0001_0000;
        let a = self.matrix[0];
        let b = self.matrix[1];
        let c = self.matrix[3];
        let d = self.matrix[4];
        match (a, b, c, d) {
            (ONE, 0, 0, ONE) => TrackRotation::None,
            (0, ONE, NEG_ONE, 0) => TrackRotation::Rotate90,
            (NEG_ONE, 0, 0, NEG_ONE) => TrackRotation::Rotate180,
            (0, NEG_ONE, ONE, 0) => TrackRotation::Rotate270,
            _ => TrackRotation::Other,
        }
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
    need(payload, p, 36, "tkhd matrix")?;
    let mut matrix = [0i32; 9];
    for (i, slot) in matrix.iter_mut().enumerate() {
        let off = p + i * 4;
        *slot = i32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
    }
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
        matrix,
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
    /// Apple QuickTime text track (`text` subtype). Used by chapter
    /// tracks (per QTFF p. 51, "Chapter Lists") and subtitle / caption
    /// overlays. Distinct from ISO BMFF `subt` / `sbtl` / `text` which
    /// are timed-text variants `is_subtitle()` recognises separately.
    pub fn is_text(&self) -> bool {
        self.component_subtype == *b"text"
    }
    /// ISO BMFF subtitle/caption media: `subt` (general subtitle),
    /// `sbtl` (caption / closed-caption), `text` (BMFF timed text).
    pub fn is_subtitle(&self) -> bool {
        matches!(&self.component_subtype, b"subt" | b"sbtl")
    }
    /// QuickTime time-code track (`tmcd` subtype, QTFF p. 116).
    pub fn is_timecode(&self) -> bool {
        self.component_subtype == *b"tmcd"
    }
    /// ISO BMFF timed-metadata track (`meta` subtype, ISO/IEC 14496-12
    /// §12.3.2). Such a track uses a null media header (`nmhd`) and
    /// declares its samples through a `MetaDataSampleEntry` subclass
    /// (`metx` / `mett` / `urim`) in `stsd`. Distinct from the `meta`
    /// *box* (untimed file/movie/track metadata, §8.11) — this is the
    /// handler component subtype, not the box type.
    pub fn is_metadata(&self) -> bool {
        self.component_subtype == *b"meta"
    }
    /// ISO BMFF hint track (`hint` subtype, ISO/IEC 14496-12 §12.4.1):
    /// protocol-packetization metadata (e.g. RTP) referencing a source
    /// media track.
    pub fn is_hint(&self) -> bool {
        self.component_subtype == *b"hint"
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

/// Parsed `hmhd` Hint Media Header Box (ISO/IEC 14496-12 §12.4.2.2).
///
/// Present in the `minf` of a hint track (`hdlr` component subtype
/// `hint`); carries protocol-independent buffering metadata about the
/// stream's Protocol Data Units. All five on-wire fields follow the
/// 4-byte FullBox version+flags header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hmhd {
    /// Size, in bytes, of the largest PDU in this hint stream.
    pub max_pdu_size: u16,
    /// Average size of a PDU over the entire presentation.
    pub avg_pdu_size: u16,
    /// Maximum rate in bits/second over any one-second window.
    pub max_bitrate: u32,
    /// Average rate in bits/second over the entire presentation.
    pub avg_bitrate: u32,
}

/// Parse an `hmhd` payload (ISO/IEC 14496-12 §12.4.2.2). The body after
/// the 4-byte FullBox header is `maxPDUsize:u16 avgPDUsize:u16
/// maxbitrate:u32 avgbitrate:u32 reserved:u32` — 16 bytes total.
pub fn parse_hmhd(payload: &[u8]) -> Result<Hmhd> {
    need(payload, 0, 4 + 16, "hmhd fixed fields")?;
    Ok(Hmhd {
        max_pdu_size: u16::from_be_bytes([payload[4], payload[5]]),
        avg_pdu_size: u16::from_be_bytes([payload[6], payload[7]]),
        max_bitrate: read_u32(&payload[8..12]),
        avg_bitrate: read_u32(&payload[12..16]),
    })
}

/// Which media-header box (ISO/IEC 14496-12 §8.4.5.1) was present in a
/// track's `minf`. §8.4.5.1 mandates "Exactly one specific media header
/// shall be present" — the box's *type* classifies the media even though
/// the typed-header variants (`nmhd`/`sthd`) carry no payload fields. The
/// box type a track actually wrote is a useful classification signal that
/// the handler subtype alone doesn't always pin down (e.g. a generic
/// stream can use either `gmhd` or `nmhd`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaHeaderKind {
    /// No media-header box was found in `minf` (malformed per spec, but
    /// tolerated; the default).
    #[default]
    None,
    /// `vmhd` — Video Media Header Box (§12.1.2). Video tracks.
    Video,
    /// `smhd` — Sound Media Header Box (§12.2.2). Audio tracks.
    Sound,
    /// `hmhd` — Hint Media Header Box (§12.4.2). Hint tracks. The parsed
    /// fields are on [`crate::track::Track::hmhd`].
    Hint,
    /// `sthd` — Subtitle Media Header Box (§12.6.2). Subtitle tracks. An
    /// empty FullBox (version 0, flags 0); only its presence is signalled.
    Subtitle,
    /// `nmhd` — Null Media Header Box (§8.4.5.2). Streams for which no
    /// specific media header is identified (e.g. timed metadata, §12.3.2).
    /// An empty FullBox; only its presence is signalled.
    Null,
    /// `gmhd` — QuickTime Base Media Information Header Atom (QTFF p. 64).
    /// Generic media; the parsed extensions are on
    /// [`crate::track::Track::gmhd`].
    Generic,
}

/// Parse an `elng` Extended Language Tag Box (ISO/IEC 14496-12 §8.4.6).
///
/// `class ExtendedLanguageBox extends FullBox('elng', 0, 0) { string
/// extended_language; }` — the body after the 4-byte FullBox header is a
/// single NUL-terminated UTF-8 string holding an RFC 4646 (BCP 47)
/// language tag such as `"en-US"`, `"fr-FR"`, or `"zh-CN"`. The tag
/// overrides the packed `mdhd.language` code when the two disagree
/// (§8.4.6.1). A missing terminator is tolerated (the remaining bytes are
/// taken as the tag); the returned string excludes the terminating NUL.
pub fn parse_elng(payload: &[u8]) -> Result<String> {
    need(payload, 0, 4, "elng FullBox header")?;
    let body = &payload[4..];
    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    Ok(String::from_utf8_lossy(&body[..end]).into_owned())
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
    fn hmhd_parses_all_fields() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&1500u16.to_be_bytes()); // maxPDUsize
        p.extend_from_slice(&1200u16.to_be_bytes()); // avgPDUsize
        p.extend_from_slice(&5_000_000u32.to_be_bytes()); // maxbitrate
        p.extend_from_slice(&3_000_000u32.to_be_bytes()); // avgbitrate
        p.extend_from_slice(&0u32.to_be_bytes()); // reserved
        let h = parse_hmhd(&p).unwrap();
        assert_eq!(h.max_pdu_size, 1500);
        assert_eq!(h.avg_pdu_size, 1200);
        assert_eq!(h.max_bitrate, 5_000_000);
        assert_eq!(h.avg_bitrate, 3_000_000);
    }

    #[test]
    fn hmhd_too_short_errors() {
        assert!(parse_hmhd(&[0u8; 8]).is_err());
    }

    #[test]
    fn elng_parses_bcp47_tag() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // FullBox ver+flags
        p.extend_from_slice(b"en-US");
        p.push(0); // NUL terminator
        assert_eq!(parse_elng(&p).unwrap(), "en-US");
    }

    #[test]
    fn elng_tolerates_missing_terminator() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"zh-CN"); // no trailing NUL
        assert_eq!(parse_elng(&p).unwrap(), "zh-CN");
    }

    #[test]
    fn elng_empty_tag_is_empty_string() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.push(0); // empty NUL-terminated string
        assert_eq!(parse_elng(&p).unwrap(), "");
    }

    #[test]
    fn elng_too_short_for_fullbox_header_errors() {
        assert!(parse_elng(&[0u8; 3]).is_err());
    }

    #[test]
    fn hdlr_is_hint_and_is_metadata() {
        let mk = |sub: &[u8; 4]| Hdlr {
            component_type: *b"mhlr",
            component_subtype: *sub,
            component_manufacturer: [0; 4],
        };
        assert!(mk(b"hint").is_hint());
        assert!(!mk(b"hint").is_metadata());
        assert!(mk(b"meta").is_metadata());
        assert!(!mk(b"meta").is_hint());
        assert!(!mk(b"vide").is_hint());
    }

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

    /// Helper: build a v0 `tkhd` with a custom 9-element matrix
    /// (16.16 fixed for the first 8 entries, 2.30 for `w`).
    fn build_tkhd_with_matrix(matrix: [i32; 9]) -> Vec<u8> {
        let mut p = vec![0u8; 84];
        p[3] = 0x07;
        p[15] = 1;
        for (i, v) in matrix.iter().enumerate() {
            let off = 40 + i * 4;
            p[off..off + 4].copy_from_slice(&v.to_be_bytes());
        }
        // width × height
        p[76..80].copy_from_slice(&(320u32 << 16).to_be_bytes());
        p[80..84].copy_from_slice(&(240u32 << 16).to_be_bytes());
        p
    }

    #[test]
    fn tkhd_rotation_identity_recognised() {
        // identity: a=1.0, d=1.0, w=1.0 (2.30 = 0x40000000)
        let m = [
            0x0001_0000i32,
            0,
            0,
            0,
            0x0001_0000i32,
            0,
            0,
            0,
            0x4000_0000i32,
        ];
        let t = parse_tkhd(&build_tkhd_with_matrix(m)).unwrap();
        assert_eq!(t.matrix, m);
        assert_eq!(t.rotation(), TrackRotation::None);
    }

    #[test]
    fn tkhd_rotation_90_recognised() {
        // 90° CW: a=0, b=1, c=-1, d=0
        let m = [
            0,
            0x0001_0000i32,
            0,
            -0x0001_0000i32,
            0,
            0,
            0,
            0,
            0x4000_0000i32,
        ];
        let t = parse_tkhd(&build_tkhd_with_matrix(m)).unwrap();
        assert_eq!(t.rotation(), TrackRotation::Rotate90);
    }

    #[test]
    fn tkhd_rotation_180_recognised() {
        let m = [
            -0x0001_0000i32,
            0,
            0,
            0,
            -0x0001_0000i32,
            0,
            0,
            0,
            0x4000_0000i32,
        ];
        let t = parse_tkhd(&build_tkhd_with_matrix(m)).unwrap();
        assert_eq!(t.rotation(), TrackRotation::Rotate180);
    }

    #[test]
    fn tkhd_rotation_270_recognised() {
        // 270° CW: a=0, b=-1, c=1, d=0
        let m = [
            0,
            -0x0001_0000i32,
            0,
            0x0001_0000i32,
            0,
            0,
            0,
            0,
            0x4000_0000i32,
        ];
        let t = parse_tkhd(&build_tkhd_with_matrix(m)).unwrap();
        assert_eq!(t.rotation(), TrackRotation::Rotate270);
    }

    #[test]
    fn tkhd_rotation_skewed_falls_into_other() {
        // a=2.0 — non-unit scale → not a clean rotation.
        let m = [
            0x0002_0000i32,
            0,
            0,
            0,
            0x0001_0000i32,
            0,
            0,
            0,
            0x4000_0000i32,
        ];
        let t = parse_tkhd(&build_tkhd_with_matrix(m)).unwrap();
        assert_eq!(t.rotation(), TrackRotation::Other);
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

    fn ftyp_with(major: &[u8; 4], compat: &[&[u8; 4]]) -> Ftyp {
        Ftyp {
            major_brand: *major,
            minor_version: 0,
            compatible_brands: compat.iter().map(|b| **b).collect(),
        }
    }

    #[test]
    fn brand_class_classifies_known_tags_and_preserves_unknown() {
        assert_eq!(BrandClass::classify(b"heic"), BrandClass::Heic);
        assert_eq!(BrandClass::classify(b"heix"), BrandClass::Heix);
        assert_eq!(BrandClass::classify(b"avif"), BrandClass::Avif);
        assert_eq!(BrandClass::classify(b"mif1"), BrandClass::Mif1);
        assert_eq!(BrandClass::classify(b"MA1B"), BrandClass::Ma1b);
        assert_eq!(BrandClass::classify(b"qt  "), BrandClass::Qt);
        // Unknown vendor tag preserves its bytes verbatim.
        match BrandClass::classify(b"vNDR") {
            BrandClass::Other(b) => assert_eq!(b, *b"vNDR"),
            other => panic!("expected Other(vNDR), got {other:?}"),
        }
    }

    #[test]
    fn brand_class_fourcc_round_trips() {
        for tag in [
            b"heic", b"heix", b"heim", b"heis", b"avif", b"mif1", b"qt  ",
        ] {
            assert_eq!(BrandClass::classify(tag).fourcc(), *tag);
        }
        assert_eq!(BrandClass::Other(*b"vNDR").fourcc(), *b"vNDR");
    }

    #[test]
    fn ftyp_brand_class_walks_major_then_compatible() {
        let f = ftyp_with(b"heic", &[b"mif1"]);
        let classes = f.brand_class();
        assert_eq!(classes, vec![BrandClass::Heic, BrandClass::Mif1]);
    }

    #[test]
    fn ftyp_is_heic_detects_major_or_compatible_heic() {
        // major=heic alone
        assert!(ftyp_with(b"heic", &[]).is_heic());
        // compatible carries heix
        assert!(ftyp_with(b"mif1", &[b"heix"]).is_heic());
        // mif1 alone is not heic
        assert!(!ftyp_with(b"mif1", &[b"isom"]).is_heic());
        // qt isn't heic
        assert!(!ftyp_with(b"qt  ", &[b"qt  "]).is_heic());
    }

    #[test]
    fn ftyp_is_avif_detects_avif_brand_family() {
        assert!(ftyp_with(b"avif", &[b"mif1"]).is_avif());
        assert!(ftyp_with(b"mif1", &[b"avis"]).is_avif());
        assert!(!ftyp_with(b"heic", &[b"mif1"]).is_avif());
    }

    #[test]
    fn ftyp_is_miaf_recognises_explicit_and_derivative_brands() {
        // Explicit mif1.
        assert!(ftyp_with(b"mif1", &[]).is_miaf());
        // HEIC entails MIAF per HEIF §10.
        assert!(ftyp_with(b"heic", &[]).is_miaf());
        // AVIF entails MIAF per AVIF §3.
        assert!(ftyp_with(b"avif", &[]).is_miaf());
        // MA1A profile.
        assert!(ftyp_with(b"MA1A", &[]).is_miaf());
        // Plain isom is NOT MIAF.
        assert!(!ftyp_with(b"isom", &[b"mp42"]).is_miaf());
        // QT alone is not MIAF.
        assert!(!ftyp_with(b"qt  ", &[]).is_miaf());
    }
}
