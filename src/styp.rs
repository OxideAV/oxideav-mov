//! Segment Type Box (`styp`).
//!
//! ISO/IEC 14496-12:2015 §8.16.2 (p. 104). The Segment Type Box is a
//! file-level box that identifies a DASH / CMAF / HLS-fMP4 media
//! segment and declares the specifications with which it is compliant.
//! It is structurally identical to the `ftyp` box (§4.3): a
//! `major_brand` FourCC, a 32-bit `minor_version`, and a run of
//! 4-byte `compatible_brands` to end-of-box. The box type alone — not
//! the body shape — distinguishes a segment from a full file.
//!
//! Spec §8.16.2 verbatim guidance:
//!
//! * "If segments are stored in separate files (e.g. on a standard
//!   HTTP server) it is recommended that these 'segment files' contain
//!   a segment‐type box, which must be first if present, to enable
//!   identification of those files, and declaration of the
//!   specifications with which they are compliant."
//! * "A segment type has the same format as an 'ftyp' box [4.3],
//!   except that it takes the box type 'styp'."
//! * "The brands within it may include the same brands that were
//!   included in the 'ftyp' box that preceded the 'moov' box, and may
//!   also include additional brands to indicate the compatibility of
//!   this segment with various specification(s)."
//! * "Valid segment type boxes shall be the first box in a segment.
//!   Segment type boxes may be removed if segments are concatenated
//!   (e.g. to form a full file), but this is not required. Segment
//!   type boxes that are not first in their files may be ignored."
//!
//! Layout per ISO/IEC 14496-12 §8.16.2.2 (= §4.3.2 with the box type
//! switched):
//!
//! ```text
//! aligned(8) class SegmentTypeBox extends Box('styp') {
//!     unsigned int(32) major_brand;
//!     unsigned int(32) minor_version;
//!     unsigned int(32) compatible_brands[];    // to end of box
//! }
//! ```
//!
//! The box is `Container: File`, `Mandatory: No`, `Quantity: Zero or
//! more` (§8.16.2.1). QTFF does not define this box; it is an ISO
//! BMFF-only construct used by adaptive-streaming derived
//! specifications (DASH / CMAF / HLS fMP4).

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

use crate::header::{BrandClass, Ftyp};

/// Parsed Segment Type Box (ISO/IEC 14496-12 §8.16.2). On-disk shape
/// is identical to [`Ftyp`]; we keep the type distinct so callers can
/// tell at the type level whether a brand list came from a file's
/// initial `ftyp` or from a per-segment `styp`. The semantics differ
/// (§8.16.2.1 — `styp` declares segment-level conformance and may add
/// brands the parent `ftyp` did not advertise).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Styp {
    /// The `major_brand` FourCC for this segment.
    pub major_brand: [u8; 4],
    /// The 32-bit `minor_version` informative number (§4.3.3 — "not a
    /// version of the major brand").
    pub minor_version: u32,
    /// The `compatible_brands` list in on-disk order. May include the
    /// brands of the originating file's `ftyp` plus extra
    /// segment-conformance brands (§8.16.2.1).
    pub compatible_brands: Vec<[u8; 4]>,
}

impl Styp {
    /// Convert this segment-type box into an equivalently-shaped
    /// [`Ftyp`], so callers can reuse the rich brand-class machinery
    /// (`is_heic` / `is_avif_family` / `is_miaf_family` / per-brand
    /// `BrandClass` walks) defined on [`Ftyp`].
    pub fn to_ftyp(&self) -> Ftyp {
        Ftyp {
            major_brand: self.major_brand,
            minor_version: self.minor_version,
            compatible_brands: self.compatible_brands.clone(),
        }
    }

    /// Classify the major brand into a typed [`BrandClass`].
    pub fn major_brand_class(&self) -> BrandClass {
        BrandClass::classify(&self.major_brand)
    }

    /// True iff `brand` appears as either the major brand or anywhere
    /// in the compatible-brands list. Lets callers query for a single
    /// concrete FourCC without first converting to [`Ftyp`].
    pub fn has_brand(&self, brand: &[u8; 4]) -> bool {
        &self.major_brand == brand || self.compatible_brands.iter().any(|b| b == brand)
    }

    /// True iff this segment carries any of the three common DASH
    /// segment-conformance brands (`msdh` — media segment, `msix` —
    /// media segment with `sidx` index, `risx` — representation index
    /// segment). Defined in ISO/IEC 23009-1 (DASH) and listed in the
    /// ISO BMFF "Registered Brands" annex; they identify the segment
    /// as conformant to the DASH segment format.
    pub fn is_dash_segment(&self) -> bool {
        const DASH_SEGMENT_BRANDS: &[&[u8; 4]] = &[b"msdh", b"msix", b"risx"];
        DASH_SEGMENT_BRANDS.iter().any(|b| self.has_brand(b))
    }

    /// True iff this segment carries the CMAF segment-conformance
    /// brand `cmfs` (Common Media Application Format segment).
    pub fn is_cmaf_segment(&self) -> bool {
        self.has_brand(b"cmfs")
    }
}

/// Parse a `styp` payload.
///
/// Layout per ISO/IEC 14496-12 §8.16.2 (identical to §4.3 `ftyp`):
///
/// ```text
/// [major_brand:4][minor_version:4]
/// (compatible_brand:4) × N            # N = (payload_len - 8) / 4
/// ```
///
/// Returns `Error::invalid` when:
/// * the payload is shorter than the 8-byte fixed header
///   (`major_brand` + `minor_version`),
/// * the post-header body length is not a multiple of 4 (a partial
///   trailing compatible-brand FourCC indicates a truncated or
///   malformed box).
///
/// An empty compatible-brands list is legal — §8.16.2 inherits the
/// §4.3 form which permits zero compatible brands.
pub fn parse_styp(payload: &[u8]) -> Result<Styp> {
    if payload.len() < 8 {
        return Err(Error::invalid(format!(
            "MOV: styp payload {} < 8 bytes (major_brand + minor_version)",
            payload.len()
        )));
    }
    let mut major = [0u8; 4];
    major.copy_from_slice(&payload[..4]);
    let minor = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);

    let rest = &payload[8..];
    if rest.len() % 4 != 0 {
        return Err(Error::invalid(format!(
            "MOV: styp compatible-brands tail {} bytes not 4-aligned",
            rest.len()
        )));
    }
    let mut brands = Vec::with_capacity(rest.len() / 4);
    for chunk in rest.chunks_exact(4) {
        let mut b = [0u8; 4];
        b.copy_from_slice(chunk);
        brands.push(b);
    }
    Ok(Styp {
        major_brand: major,
        minor_version: minor,
        compatible_brands: brands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_styp(major: &[u8; 4], minor: u32, compat: &[&[u8; 4]]) -> Vec<u8> {
        let mut p = Vec::with_capacity(8 + 4 * compat.len());
        p.extend_from_slice(major);
        p.extend_from_slice(&minor.to_be_bytes());
        for b in compat {
            p.extend_from_slice(*b);
        }
        p
    }

    #[test]
    fn parses_major_minor_and_compatible_brands() {
        // A typical DASH init segment styp: major = 'iso5', minor = 0,
        // compatible = ['iso5', 'dash', 'msdh'].
        let p = build_styp(b"iso5", 0, &[b"iso5", b"dash", b"msdh"]);
        let s = parse_styp(&p).unwrap();
        assert_eq!(&s.major_brand, b"iso5");
        assert_eq!(s.minor_version, 0);
        assert_eq!(s.compatible_brands.len(), 3);
        assert_eq!(&s.compatible_brands[0], b"iso5");
        assert_eq!(&s.compatible_brands[1], b"dash");
        assert_eq!(&s.compatible_brands[2], b"msdh");
    }

    #[test]
    fn empty_compatible_brands_is_legal() {
        // §4.3 (inherited by §8.16.2) permits a zero-length
        // compatible-brands list — an 8-byte body is enough.
        let p = build_styp(b"msdh", 0x0001_0002, &[]);
        let s = parse_styp(&p).unwrap();
        assert_eq!(&s.major_brand, b"msdh");
        assert_eq!(s.minor_version, 0x0001_0002);
        assert!(s.compatible_brands.is_empty());
    }

    #[test]
    fn payload_under_eight_bytes_rejected() {
        // 7 bytes — one short of the 8-byte fixed header.
        let p = vec![0u8; 7];
        assert!(parse_styp(&p).is_err());
    }

    #[test]
    fn unaligned_compatible_brand_tail_rejected() {
        // 8-byte header + 3 trailing bytes (less than one full FourCC)
        // is a truncated box and must reject.
        let mut p = build_styp(b"msdh", 0, &[]);
        p.extend_from_slice(&[0, 0, 0]);
        assert!(parse_styp(&p).is_err());
    }

    #[test]
    fn has_brand_matches_major() {
        let s = Styp {
            major_brand: *b"iso5",
            minor_version: 0,
            compatible_brands: vec![*b"dash"],
        };
        assert!(s.has_brand(b"iso5"));
        assert!(s.has_brand(b"dash"));
        assert!(!s.has_brand(b"avif"));
    }

    #[test]
    fn is_dash_segment_detects_known_brands() {
        // msdh — media-segment brand.
        let a = Styp {
            major_brand: *b"iso5",
            minor_version: 0,
            compatible_brands: vec![*b"msdh"],
        };
        assert!(a.is_dash_segment());
        // msix — media-segment-with-index brand.
        let b = Styp {
            major_brand: *b"msix",
            minor_version: 0,
            compatible_brands: vec![],
        };
        assert!(b.is_dash_segment());
        // risx — representation-index segment.
        let c = Styp {
            major_brand: *b"iso6",
            minor_version: 0,
            compatible_brands: vec![*b"risx"],
        };
        assert!(c.is_dash_segment());
        // Plain ftyp brands shouldn't trigger.
        let d = Styp {
            major_brand: *b"isom",
            minor_version: 0,
            compatible_brands: vec![*b"iso2", *b"avc1"],
        };
        assert!(!d.is_dash_segment());
    }

    #[test]
    fn is_cmaf_segment_detects_cmfs() {
        let s = Styp {
            major_brand: *b"cmfs",
            minor_version: 0,
            compatible_brands: vec![],
        };
        assert!(s.is_cmaf_segment());
        let s2 = Styp {
            major_brand: *b"iso6",
            minor_version: 0,
            compatible_brands: vec![*b"cmfs"],
        };
        assert!(s2.is_cmaf_segment());
        let s3 = Styp {
            major_brand: *b"iso5",
            minor_version: 0,
            compatible_brands: vec![*b"dash"],
        };
        assert!(!s3.is_cmaf_segment());
    }

    #[test]
    fn to_ftyp_round_trips_field_set() {
        // Lift to an Ftyp and re-read the same fields — the conversion
        // exists so callers can reuse the rich BrandClass helpers
        // defined on Ftyp without re-implementing them on Styp.
        let s = Styp {
            major_brand: *b"avif",
            minor_version: 0,
            compatible_brands: vec![*b"mif1", *b"miaf"],
        };
        let f = s.to_ftyp();
        assert_eq!(f.major_brand, s.major_brand);
        assert_eq!(f.minor_version, s.minor_version);
        assert_eq!(f.compatible_brands, s.compatible_brands);
    }

    #[test]
    fn major_brand_class_routes_to_brandclass() {
        let s = Styp {
            major_brand: *b"heic",
            minor_version: 0,
            compatible_brands: vec![],
        };
        // Round-trip through the classifier — the typed enum is what
        // `BrandClass::classify` produces for 'heic'.
        assert_eq!(s.major_brand_class(), BrandClass::Heic);
    }

    #[test]
    fn round_trip_through_parser_preserves_brand_order() {
        let bytes = build_styp(b"iso5", 42, &[b"iso5", b"msdh", b"msix"]);
        let s = parse_styp(&bytes).unwrap();
        // Spec §4.3.3 / §8.16.2 says compatible_brands order is the
        // writer's; preserve it verbatim.
        let order: Vec<[u8; 4]> = s.compatible_brands.clone();
        assert_eq!(&order[0], b"iso5");
        assert_eq!(&order[1], b"msdh");
        assert_eq!(&order[2], b"msix");
    }
}
