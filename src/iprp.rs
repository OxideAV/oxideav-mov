//! ISO BMFF / HEIF item-properties container (`§8.11.14` in
//! ISO/IEC 14496-12, `§7.4.6` in ISO/IEC 23008-12 / HEIF).
//!
//! `iprp` carries item-level properties (colour, clean aperture,
//! pixel aspect ratio, dimensions, rotation, mirroring, auxiliary
//! type, etc.) shared across `meta`-box items via two children:
//!
//! ```text
//! iprp (Box)
//!     ipco (Box)            -- a flat array of property boxes
//!         colr / pasp / clap / pixi / ispe / imir / irot / auxC / ...
//!     ipma (FullBox, may repeat)
//!         per-row: item_ID (u16 or u32 depending on version)
//!                  association_count u8
//!                  association_count × { 1 or 2 bytes }
//!                      bit 7 = essential
//!                      remaining bits = 1-based property index into ipco
//! ```
//!
//! Property indices in `ipma` are **1-based** and reference the order
//! of property boxes inside `ipco`. The same property box can be shared
//! across many items (HEIF's wire-level optimisation for grids and
//! tile collections).
//!
//! We surface every common property box as a strongly-typed variant of
//! [`ItemProperty`] and any unrecognised one as `Other { fourcc,
//! payload }` so the caller can still inspect its raw bytes.
//!
//! References:
//! - ISO/IEC 14496-12:2015 §8.11.14 (ItemPropertiesBox).
//! - ISO/IEC 23008-12:2017 §7.4.6 (HEIF property catalogue).
//! - docs/image/heif/heif-fixtures-and-traces.md §4.1.

use crate::atom::{fourcc, read_payload, walk_children, AtomHeader};
use crate::media_meta::{
    parse_clap, parse_colr, parse_pasp, Clap, ColorParameters, ColorParametersKind, Pasp,
};
use std::io::{Read, Seek, SeekFrom};

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

pub const IPRP: [u8; 4] = fourcc("iprp");
pub const IPCO: [u8; 4] = fourcc("ipco");
pub const IPMA: [u8; 4] = fourcc("ipma");
pub const ISPE: [u8; 4] = fourcc("ispe");
pub const PIXI: [u8; 4] = fourcc("pixi");
pub const IROT: [u8; 4] = fourcc("irot");
pub const IMIR: [u8; 4] = fourcc("imir");
pub const AUXC: [u8; 4] = fourcc("auxC");
pub const COLR: [u8; 4] = fourcc("colr");
pub const PASP: [u8; 4] = fourcc("pasp");
pub const CLAP: [u8; 4] = fourcc("clap");
pub const CLLI: [u8; 4] = fourcc("clli");
pub const MDCV: [u8; 4] = fourcc("mdcv");
pub const CCLV: [u8; 4] = fourcc("cclv");

/// `ispe` — ImageSpatialExtentsProperty (HEIF §6.5.3.1). Carries the
/// pixel extent of the *encoded* picture (the consumer-visible size
/// may be smaller per a sibling `clap` property).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ispe {
    pub width: u32,
    pub height: u32,
}

/// `pixi` — PixelInformationProperty (HEIF §6.5.6). Per-channel bit
/// depth; `bits_per_channel.len()` is the channel count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pixi {
    pub bits_per_channel: Vec<u8>,
}

/// HEIF-canonical Pixel Information accessor (`pixi` per
/// ISO/IEC 23008-12 §6.5.6.3).
///
/// `channels[i]` is the bit depth of channel `i`; `channels.len()` is
/// the channel count (the on-disk `num_channels` field). Common shapes:
///
/// * `[8, 8, 8]` — 8-bit sRGB.
/// * `[8, 8, 8, 8]` — 8-bit RGBA.
/// * `[10, 10, 10]` — 10-bit HDR.
/// * `[8]` — single-channel monochrome / alpha mask.
///
/// Reshape of [`Pixi`] surfaced on layout plans so callers don't have
/// to walk `iprp` themselves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PixiInfo {
    /// Per-channel bit depth, in channel order.
    pub channels: Vec<u8>,
}

impl PixiInfo {
    /// Number of channels declared by the property (== `channels.len()`).
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }
}

impl From<&Pixi> for PixiInfo {
    fn from(p: &Pixi) -> Self {
        PixiInfo {
            channels: p.bits_per_channel.clone(),
        }
    }
}

/// `irot` — ImageRotation (HEIF §6.5.10). 90° counter-clockwise
/// rotation steps; valid values 0..=3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Irot {
    /// Number of 90° CCW rotation steps in [0, 3].
    pub steps: u8,
}

/// `imir` — ImageMirror (HEIF §6.5.12). Per HEIF 2017 / 2nd edition
/// the box body is one byte: bit 0 selects axis (0 = vertical mirror,
/// i.e. flip top-bottom; 1 = horizontal mirror, i.e. flip left-right).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Imir {
    /// 0 = vertical (flip top↔bottom), 1 = horizontal (flip left↔right).
    pub axis: u8,
}

/// `auxC` — AuxiliaryTypeProperty (HEIF §7.5.1). `aux_type` is the
/// URN identifying the auxiliary purpose of the item the association
/// applies to (e.g. `urn:mpeg:hevc:2015:auxid:1` for the alpha plane
/// of an HEVC item). `aux_subtype` is reserved trailing bytes that
/// some readers leave zero-length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuxC {
    pub aux_type: String,
    pub aux_subtype: Vec<u8>,
}

impl AuxC {
    /// Returns `true` when this auxC's URN identifies the auxiliary
    /// item as an alpha plane.
    ///
    /// HEIF / MIAF authoring tools use one of two URNs for alpha:
    ///
    /// * `urn:mpeg:hevc:2015:auxid:1` — HEIF §7.5.1 (HEVC alpha plane).
    /// * `urn:mpeg:mpegB:cicp:systems:auxiliary:alpha` — MIAF / ISO
    ///   23000-22 (codec-agnostic alpha plane URN).
    ///
    /// Both are recognised here; callers asking "is this the alpha
    /// channel for some image item?" should prefer this helper to
    /// pattern-matching on the URN literal.
    pub fn is_alpha(&self) -> bool {
        matches!(
            self.aux_type.as_str(),
            "urn:mpeg:hevc:2015:auxid:1" | "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha"
        )
    }

    /// Returns `true` when this auxC's URN identifies the auxiliary
    /// item as a depth map (HEIF §7.5.2.1).
    ///
    /// Recognised URNs:
    ///
    /// * `urn:mpeg:hevc:2015:auxid:2` — HEIF HEVC depth.
    /// * `urn:mpeg:mpegB:cicp:systems:auxiliary:depth` — MIAF
    ///   codec-agnostic depth URN.
    pub fn is_depth(&self) -> bool {
        matches!(
            self.aux_type.as_str(),
            "urn:mpeg:hevc:2015:auxid:2" | "urn:mpeg:mpegB:cicp:systems:auxiliary:depth"
        )
    }
}

/// `clli` — Content Light Level Information (ISO/IEC 23008-12 §6.5.x;
/// CTA-861 / SMPTE ST 2086 derived). Carries the maximum content light
/// level (MaxCLL) and the maximum frame-average light level (MaxFALL),
/// both expressed in candela per square metre (cd/m²) as unsigned 16-bit
/// integers. Sender-side metadata for HDR display tone-mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clli {
    /// Maximum content light level (MaxCLL), cd/m².
    pub max_content_light_level: u16,
    /// Maximum picture-average light level (MaxFALL), cd/m².
    pub max_pic_average_light_level: u16,
}

/// `mdcv` — Mastering Display Colour Volume (ISO/IEC 23008-12 §6.5.x;
/// SMPTE ST 2086 metadata in HEIF wrapping). Carries the chromaticity
/// of the three RGB display primaries plus the white point, and the
/// nominal display luminance range used to master the content. The
/// chromaticity values are in 0.00002 increments (i.e. divide by
/// 50 000 for CIE x/y); luminance values are in 0.0001 cd/m² units
/// (divide by 10 000 for cd/m²).
///
/// On-disk layout (40 bytes total):
///
/// ```text
/// display_primaries_x[c]  u16 BE       (c = 0..2; G, B, R order per ST 2086)
/// display_primaries_y[c]  u16 BE       (c = 0..2)
/// white_point_x           u16 BE
/// white_point_y           u16 BE
/// max_display_luminance   u32 BE       (0.0001 cd/m² steps)
/// min_display_luminance   u32 BE       (0.0001 cd/m² steps)
/// ```
///
/// `display_primaries[c]` here mirrors the on-disk indexing — i.e.
/// `display_primaries[0]` is the *green* primary, `[1]` blue, `[2]`
/// red. Callers wanting CIE-1931 (x, y) values should divide each u16
/// by 50 000.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mdcv {
    /// On-disk-ordered (x, y) chromaticity for each of the three
    /// display primaries — index 0 = G, 1 = B, 2 = R per SMPTE ST 2086.
    pub display_primaries: [(u16, u16); 3],
    /// (x, y) chromaticity of the mastering display's white point.
    pub white_point: (u16, u16),
    /// Maximum display luminance, in 0.0001 cd/m² steps.
    pub max_display_luminance: u32,
    /// Minimum display luminance, in 0.0001 cd/m² steps.
    pub min_display_luminance: u32,
}

/// `cclv` — Content Colour Volume (ISO/IEC 23008-12 §6.5.x).
///
/// `cclv` is HEVC SEI 144 transcribed into an `iprp` property and
/// describes the volume in CIE-1931 xy + nominal luminance space the
/// *content* is contained in (as opposed to `mdcv`'s display-mastering
/// volume). The on-disk shape is gated by a one-byte flags field whose
/// low three bits select which of the three sub-records are present:
///
/// ```text
/// ccv_cancel_flag         u1
/// ccv_persistence_flag    u1
/// ccv_primaries_present   u1   --|
/// ccv_min_luminance_present u1 --|
/// ccv_max_luminance_present u1 --|-> per-field-presence flags
/// ccv_avg_luminance_present u1 --|
/// reserved                u2
/// // optionally:
/// ccv_primaries_x[c]      i32 BE   c=0..2 (G,B,R); only when ccv_primaries_present
/// ccv_primaries_y[c]      i32 BE   c=0..2 (G,B,R); only when ccv_primaries_present
/// ccv_min_luminance       u32 BE             only when ccv_min_luminance_present
/// ccv_max_luminance       u32 BE             only when ccv_max_luminance_present
/// ccv_avg_luminance       u32 BE             only when ccv_avg_luminance_present
/// ```
///
/// We surface the basic shape: the four *_present flags + each optional
/// sub-record. Absent sub-records are `None`. The signed primaries are
/// kept signed because HEVC §D.2.39 requires the parser to interpret
/// them as `i32`; in practice they're constrained to `[-50000, 50000]`
/// (i.e. CIE-1931 range × 50 000 same as `mdcv`), but we don't enforce
/// that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cclv {
    /// Whether the content cancels any prior CCV signalling.
    pub cancel_flag: bool,
    /// Whether the CCV applies to subsequent frames until cancelled
    /// (`true`) or only the current frame (`false`).
    pub persistence_flag: bool,
    /// Content primaries (G/B/R order per HEVC §D.2.39); `None` when
    /// the `ccv_primaries_present_flag` bit is clear.
    pub primaries: Option<[(i32, i32); 3]>,
    /// Minimum content luminance, 0.0001 cd/m² steps; `None` when
    /// `ccv_min_luminance_value_present_flag` is clear.
    pub min_luminance: Option<u32>,
    /// Maximum content luminance, 0.0001 cd/m² steps; `None` when
    /// `ccv_max_luminance_value_present_flag` is clear.
    pub max_luminance: Option<u32>,
    /// Average content luminance, 0.0001 cd/m² steps; `None` when
    /// `ccv_avg_luminance_value_present_flag` is clear.
    pub avg_luminance: Option<u32>,
}

/// One property entry inside `ipco`.
///
/// `Other` is a fall-through for any box type we don't model
/// natively (typical example: `hvcC`, `av1C`, `lsel`). Callers can
/// still match on its fourcc and parse the raw payload themselves —
/// for instance, `hvcC` is parsed by
/// `oxideav-h265::HEVCDecoderConfigurationRecord` rather than by us.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemProperty {
    Colr(ColorParameters),
    Pasp(Pasp),
    Clap(Clap),
    Pixi(Pixi),
    Ispe(Ispe),
    Irot(Irot),
    Imir(Imir),
    AuxC(AuxC),
    Clli(Clli),
    Mdcv(Mdcv),
    Cclv(Cclv),
    Other { fourcc: [u8; 4], payload: Vec<u8> },
}

impl ItemProperty {
    /// FourCC of the underlying property box.
    pub fn fourcc(&self) -> [u8; 4] {
        match self {
            ItemProperty::Colr(_) => COLR,
            ItemProperty::Pasp(_) => PASP,
            ItemProperty::Clap(_) => CLAP,
            ItemProperty::Pixi(_) => PIXI,
            ItemProperty::Ispe(_) => ISPE,
            ItemProperty::Irot(_) => IROT,
            ItemProperty::Imir(_) => IMIR,
            ItemProperty::AuxC(_) => AUXC,
            ItemProperty::Clli(_) => CLLI,
            ItemProperty::Mdcv(_) => MDCV,
            ItemProperty::Cclv(_) => CCLV,
            ItemProperty::Other { fourcc, .. } => *fourcc,
        }
    }
}

/// One association (item ↔ property index) row from `ipma`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PropertyAssociation {
    /// 1-based index into the parent [`ItemProperties::properties`].
    /// `0` means "not present" and is filtered out at parse time, but
    /// resolvers must defensively check `index <= properties.len()`.
    pub index: u16,
    /// Whether a reader that doesn't recognise the property must
    /// reject the file (`true`) or silently ignore the association
    /// (`false`).
    pub essential: bool,
}

/// One `ipma` row: an item + its list of property associations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemPropertyAssociation {
    pub item_id: u32,
    pub associations: Vec<PropertyAssociation>,
}

/// Parsed `iprp` container.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemProperties {
    /// Flat array of property boxes parsed from `ipco` (1-based when
    /// resolving from `ipma`'s property indices, but stored 0-based
    /// here — [`ItemProperties::resolve`] does the index translation).
    pub properties: Vec<ItemProperty>,
    /// One row per item that has at least one association.
    pub associations: Vec<ItemPropertyAssociation>,
}

impl ItemProperties {
    /// Look up an item's associations by item-id.
    pub fn associations_for(&self, item_id: u32) -> Option<&ItemPropertyAssociation> {
        self.associations.iter().find(|a| a.item_id == item_id)
    }

    /// Resolve every property attached to `item_id`, in the order the
    /// `ipma` row lists them. Out-of-range indices are silently
    /// skipped (defensive; HEIF readers must tolerate forward-
    /// compatible authoring extensions).
    pub fn resolve(&self, item_id: u32) -> Vec<&ItemProperty> {
        let row = match self.associations_for(item_id) {
            Some(r) => r,
            None => return Vec::new(),
        };
        let mut out = Vec::with_capacity(row.associations.len());
        for a in &row.associations {
            if a.index == 0 {
                continue;
            }
            // 1-based → 0-based.
            if let Some(p) = self.properties.get((a.index as usize) - 1) {
                out.push(p);
            }
        }
        out
    }

    /// First `ispe` (image-spatial-extents) attached to the item.
    pub fn ispe_for(&self, item_id: u32) -> Option<Ispe> {
        for p in self.resolve(item_id) {
            if let ItemProperty::Ispe(i) = p {
                return Some(*i);
            }
        }
        None
    }

    /// First `colr` (colour-information) attached to the item.
    pub fn colr_for(&self, item_id: u32) -> Option<&ColorParameters> {
        for p in self.resolve(item_id) {
            if let ItemProperty::Colr(c) = p {
                return Some(c);
            }
        }
        None
    }

    /// HEIF colour-profile accessor: returns the first [`ColrInfo`]
    /// attached to `item_id`, walking the item's `ipma` row in order.
    ///
    /// Equivalent to [`Self::colr_for`] but reshapes the result into
    /// the HEIF-canonical [`ColrInfo`] enum so callers don't have to
    /// match on the QTFF-flavoured `ColorParametersKind` (which also
    /// surfaces the Apple-only `nclc` shape and a forensic `Other`
    /// fall-through). Returns `None` when the item has no `colr`
    /// associated, or when its `colr` is the QTFF `nclc` Apple
    /// variant — `nclc` is not a valid HEIF colour profile per
    /// ISO/IEC 14496-12 §12.1.5 (HEIF mandates `nclx` / `rICC` /
    /// `prof`).
    pub fn color_profile(&self, item_id: u32) -> Option<ColrInfo> {
        let cp = self.colr_for(item_id)?;
        ColrInfo::from_color_parameters(cp)
    }

    /// First `pixi` (pixel-information) attached to the item.
    pub fn pixi_for(&self, item_id: u32) -> Option<&Pixi> {
        for p in self.resolve(item_id) {
            if let ItemProperty::Pixi(x) = p {
                return Some(x);
            }
        }
        None
    }

    /// HEIF pixel-information accessor: returns the first [`PixiInfo`]
    /// (channel count + per-channel bit depth) attached to `item_id`.
    ///
    /// Per ISO/IEC 23008-12 §6.5.6.3 a HEIF item with pixel data SHOULD
    /// carry a `pixi` association declaring its channel count and the
    /// bit depth of each channel. Returns `None` when no `pixi` is
    /// associated. Companion to [`Self::color_profile`].
    pub fn pixi(&self, item_id: u32) -> Option<PixiInfo> {
        self.pixi_for(item_id).map(PixiInfo::from)
    }

    /// First `auxC` attached to the item.
    pub fn auxc_for(&self, item_id: u32) -> Option<&AuxC> {
        for p in self.resolve(item_id) {
            if let ItemProperty::AuxC(a) = p {
                return Some(a);
            }
        }
        None
    }

    /// HEIF auxiliary-type accessor: returns the first [`AuxC`]
    /// attached to `item_id` (the same value as [`Self::auxc_for`],
    /// but cloned for callers that don't want to keep `&self`
    /// borrowed across the resolve walk).
    pub fn auxc(&self, item_id: u32) -> Option<AuxC> {
        self.auxc_for(item_id).cloned()
    }

    /// First `clli` (Content Light Level Information) attached to the
    /// item. `None` when no `clli` association exists.
    pub fn clli(&self, item_id: u32) -> Option<Clli> {
        for p in self.resolve(item_id) {
            if let ItemProperty::Clli(c) = p {
                return Some(*c);
            }
        }
        None
    }

    /// First `mdcv` (Mastering Display Colour Volume) attached to the
    /// item. `None` when no `mdcv` association exists.
    pub fn mdcv(&self, item_id: u32) -> Option<Mdcv> {
        for p in self.resolve(item_id) {
            if let ItemProperty::Mdcv(m) = p {
                return Some(*m);
            }
        }
        None
    }

    /// First `cclv` (Content Colour Volume) attached to the item.
    /// `None` when no `cclv` association exists.
    pub fn cclv(&self, item_id: u32) -> Option<Cclv> {
        for p in self.resolve(item_id) {
            if let ItemProperty::Cclv(c) = p {
                return Some(*c);
            }
        }
        None
    }

    /// First rotation/mirror pair attached to the item, in canonical
    /// (rotation_then_mirror) order. Either component may be `None`.
    pub fn orientation_for(&self, item_id: u32) -> (Option<Irot>, Option<Imir>) {
        let mut rot = None;
        let mut mir = None;
        for p in self.resolve(item_id) {
            match p {
                ItemProperty::Irot(r) if rot.is_none() => rot = Some(*r),
                ItemProperty::Imir(m) if mir.is_none() => mir = Some(*m),
                _ => {}
            }
        }
        (rot, mir)
    }

    /// HEIF-strict resolver: same as [`Self::resolve`] but returns
    /// the offending fourcc on the first essential-bit-set
    /// association whose target property the renderer doesn't
    /// recognise (any `ItemProperty::Other`). Per HEIF §7.4.6.6 a
    /// reader that doesn't understand an essential property MUST
    /// reject the item; this helper lets callers opt in to that
    /// stricter behaviour. Returns `Ok(resolved_props)` on success.
    ///
    /// `recognised` lets the caller widen the "known" set beyond what
    /// this crate understands natively — for instance, an HEVC
    /// decoder caller can mark `hvcC` as recognised so an essential
    /// `hvcC` doesn't trip the gate. Pass an empty slice to use only
    /// the property variants this crate models natively.
    pub fn resolve_strict(
        &self,
        item_id: u32,
        recognised: &[[u8; 4]],
    ) -> std::result::Result<Vec<&ItemProperty>, [u8; 4]> {
        let row = match self.associations_for(item_id) {
            Some(r) => r,
            None => return Ok(Vec::new()),
        };
        let mut out = Vec::with_capacity(row.associations.len());
        for a in &row.associations {
            if a.index == 0 {
                continue;
            }
            let p = match self.properties.get((a.index as usize) - 1) {
                Some(p) => p,
                None => continue, // out-of-range, same as resolve()
            };
            // Surface the essential-bit gate only for `Other` (the
            // properties we don't model natively). Any property variant
            // we do model is by definition "recognised".
            if a.essential {
                if let ItemProperty::Other { fourcc, .. } = p {
                    if !recognised.contains(fourcc) {
                        return Err(*fourcc);
                    }
                }
            }
            out.push(p);
        }
        Ok(out)
    }
}

/// HEIF-canonical ColourInformationBox (`colr`) extraction.
///
/// Per ISO/IEC 14496-12 §12.1.5 the box body is a 4-byte
/// `colour_type` tag followed by a tag-specific record:
///
/// * `nclx` — three u16 indices (`colour_primaries`,
///   `transfer_characteristics`, `matrix_coefficients`) plus a 1-byte
///   field whose top bit is `full_range_flag`. 7-byte body.
/// * `rICC` — restricted ICC profile bytes. The "restricted" rule per
///   ISO 15076-1 limits the profile to a small, on-device-renderable
///   subset; we do not enforce it (we surface the bytes verbatim).
/// * `prof` — full / unrestricted ICC profile bytes.
///
/// HEIF (ISO/IEC 23008-12 §6.5.5) excludes the QTFF `nclc` Apple shape
/// from the legal `colour_type` set; consequently
/// [`parse_colr_payload`] rejects `nclc` with `Err(InvalidData)`. For
/// QTFF-flavoured tracks the existing
/// [`crate::media_meta::ColorParameters`] surface is the right choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColrInfo {
    /// On-the-wire colour-volume tag (HEIF §6.5.5.1.1). Indices match
    /// ISO/IEC 23001-8 (CICP). Common values:
    ///
    /// * `primaries == 1` → BT.709
    /// * `primaries == 9` → BT.2020
    /// * `primaries == 12` → Display P3 (CICP-12)
    /// * `transfer == 13` → sRGB
    /// * `transfer == 16` → SMPTE ST 2084 (PQ)
    /// * `transfer == 18` → ARIB STD-B67 (HLG)
    /// * `matrix == 0` → Identity (RGB)
    /// * `matrix == 1` → BT.709
    /// * `matrix == 5` → BT.601
    /// * `matrix == 9` → BT.2020 NC
    Nclx {
        primaries: u16,
        transfer: u16,
        matrix: u16,
        full_range: bool,
    },
    /// `rICC` — restricted ICC profile bytes, surfaced verbatim. The
    /// restricted-profile rule (ISO 15076-1) is enforced by the
    /// downstream colour engine, not by the parser.
    RestrictedIcc(Vec<u8>),
    /// `prof` — full / unrestricted ICC profile bytes.
    UnrestrictedIcc(Vec<u8>),
}

impl ColrInfo {
    /// 4-byte CICP tag identifying the on-disk variant: `b"nclx"`,
    /// `b"rICC"`, or `b"prof"`.
    pub fn colour_type(&self) -> [u8; 4] {
        match self {
            ColrInfo::Nclx { .. } => *b"nclx",
            ColrInfo::RestrictedIcc(_) => *b"rICC",
            ColrInfo::UnrestrictedIcc(_) => *b"prof",
        }
    }

    /// True when the variant carries an embedded ICC profile blob
    /// (either restricted `rICC` or unrestricted `prof`).
    pub fn is_icc(&self) -> bool {
        matches!(
            self,
            ColrInfo::RestrictedIcc(_) | ColrInfo::UnrestrictedIcc(_)
        )
    }

    /// Borrow the ICC profile bytes when the variant is `rICC` /
    /// `prof`; `None` for `nclx`.
    pub fn icc_bytes(&self) -> Option<&[u8]> {
        match self {
            ColrInfo::RestrictedIcc(b) | ColrInfo::UnrestrictedIcc(b) => Some(b),
            ColrInfo::Nclx { .. } => None,
        }
    }

    /// Convert a parsed `ColorParameters` (the QTFF/ISO-merged surface
    /// the demuxer surfaces in `ipco`) into the HEIF-canonical
    /// `ColrInfo`. Returns `None` for the Apple `nclc` shape (HEIF
    /// forbids it) or any forensic `Other` fall-through.
    pub fn from_color_parameters(cp: &ColorParameters) -> Option<Self> {
        match &cp.kind {
            ColorParametersKind::Nclx {
                primaries,
                transfer,
                matrix,
                full_range,
            } => Some(ColrInfo::Nclx {
                primaries: *primaries,
                transfer: *transfer,
                matrix: *matrix,
                full_range: *full_range,
            }),
            ColorParametersKind::Icc { kind, profile } => match kind {
                b"rICC" => Some(ColrInfo::RestrictedIcc(profile.clone())),
                b"prof" => Some(ColrInfo::UnrestrictedIcc(profile.clone())),
                _ => None,
            },
            ColorParametersKind::Nclc { .. } | ColorParametersKind::Other { .. } => None,
        }
    }
}

/// Parse a HEIF `colr` box payload into the canonical [`ColrInfo`].
///
/// Per ISO/IEC 14496-12 §12.1.5 the leading 4 bytes are a `colour_type`
/// tag selecting the body shape. HEIF (ISO/IEC 23008-12 §6.5.5.1)
/// admits three tags:
///
/// * `nclx` — 7 bytes after the tag: three u16 indices then one byte
///   whose top bit is `full_range_flag` (the remaining 7 bits are
///   reserved and MUST be zero, but the parser does not enforce
///   reserved-bit zeroing — many encoders leave them undefined).
/// * `rICC` — restricted ICC profile bytes (variable length).
/// * `prof` — full / unrestricted ICC profile bytes (variable length).
///
/// Returns `Err(InvalidData)` when:
///
/// * the body is shorter than 4 bytes (no tag),
/// * the tag is `nclx` and the body has < 7 trailing bytes,
/// * the tag is `nclc` (the Apple QTFF shape, forbidden by HEIF
///   §6.5.5.1 Note 1 — callers wanting Apple `nclc` should use
///   [`crate::media_meta::parse_colr`] instead),
/// * the tag is anything else (forward-compatible authoring should
///   stick to the documented set).
pub fn parse_colr_payload(payload: &[u8]) -> Result<ColrInfo> {
    if payload.len() < 4 {
        return Err(Error::invalid("HEIF: colr payload < 4 bytes (no tag)"));
    }
    let tag = &payload[..4];
    let body = &payload[4..];
    match tag {
        b"nclx" => {
            if body.len() < 7 {
                return Err(Error::invalid(
                    "HEIF: colr nclx body < 7 bytes (need 3×u16 indices + 1 flag byte)",
                ));
            }
            Ok(ColrInfo::Nclx {
                primaries: u16::from_be_bytes([body[0], body[1]]),
                transfer: u16::from_be_bytes([body[2], body[3]]),
                matrix: u16::from_be_bytes([body[4], body[5]]),
                full_range: (body[6] & 0x80) != 0,
            })
        }
        b"rICC" => Ok(ColrInfo::RestrictedIcc(body.to_vec())),
        b"prof" => Ok(ColrInfo::UnrestrictedIcc(body.to_vec())),
        b"nclc" => Err(Error::invalid(
            "HEIF: colr 'nclc' tag is the QTFF Apple shape, not legal in HEIF (use ISO 'nclx')",
        )),
        other => Err(Error::invalid(format!(
            "HEIF: colr unknown colour_type tag {:?}",
            std::str::from_utf8(other).unwrap_or("<non-utf8>")
        ))),
    }
}

/// Parse the body of an `iprp` container.
///
/// The reader is positioned at the `iprp` box's payload start; the
/// caller is responsible for snapping back to the parent's
/// `body_end` afterwards (the standard `walk_children` contract).
pub fn parse_iprp<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<ItemProperties> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;

    let mut props = Vec::new();
    let mut assocs: Vec<ItemPropertyAssociation> = Vec::new();

    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &IPCO => {
                let ipco_end = child.payload_offset + child.payload_len().unwrap_or(0);
                r.seek(SeekFrom::Start(child.payload_offset))?;
                walk_children(r, Some(ipco_end), |r, prop_hdr| {
                    let prop = parse_property_box(r, prop_hdr)?;
                    props.push(prop);
                    Ok(())
                })?;
            }
            t if t == &IPMA => {
                let body = read_payload(r, child)?;
                let rows = parse_ipma(&body)?;
                assocs.extend(rows);
            }
            _ => {}
        }
        Ok(())
    })?;

    Ok(ItemProperties {
        properties: props,
        associations: assocs,
    })
}

/// Parse one property box inside `ipco`.
fn parse_property_box<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
) -> Result<ItemProperty> {
    let body = read_payload(r, hdr)?;
    Ok(match &hdr.fourcc {
        t if t == &COLR => ItemProperty::Colr(parse_colr(&body)?),
        t if t == &PASP => ItemProperty::Pasp(parse_pasp(&body)?),
        t if t == &CLAP => ItemProperty::Clap(parse_clap(&body)?),
        t if t == &ISPE => ItemProperty::Ispe(parse_ispe(&body)?),
        t if t == &PIXI => ItemProperty::Pixi(parse_pixi(&body)?),
        t if t == &IROT => ItemProperty::Irot(parse_irot(&body)?),
        t if t == &IMIR => ItemProperty::Imir(parse_imir(&body)?),
        t if t == &AUXC => ItemProperty::AuxC(parse_auxc_payload(&body)?),
        t if t == &CLLI => ItemProperty::Clli(parse_clli_payload(&body)?),
        t if t == &MDCV => ItemProperty::Mdcv(parse_mdcv_payload(&body)?),
        t if t == &CCLV => ItemProperty::Cclv(parse_cclv_payload(&body)?),
        _ => ItemProperty::Other {
            fourcc: hdr.fourcc,
            payload: body,
        },
    })
}

/// `ispe` payload: 4-byte ver+flags then u32 width, u32 height.
fn parse_ispe(body: &[u8]) -> Result<Ispe> {
    if body.len() < 12 {
        return Err(Error::invalid("MOV: ispe payload < 12 bytes"));
    }
    let width = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    let height = u32::from_be_bytes([body[8], body[9], body[10], body[11]]);
    Ok(Ispe { width, height })
}

/// `pixi` payload: 4-byte ver+flags, u8 num_channels, num_channels × u8.
fn parse_pixi(body: &[u8]) -> Result<Pixi> {
    if body.len() < 5 {
        return Err(Error::invalid("MOV: pixi payload < 5 bytes"));
    }
    let n = body[4] as usize;
    if body.len() < 5 + n {
        return Err(Error::invalid("MOV: pixi truncated bits_per_channel"));
    }
    let bits_per_channel = body[5..5 + n].to_vec();
    Ok(Pixi { bits_per_channel })
}

/// `irot` payload: 1 byte; low 2 bits are the rotation step.
fn parse_irot(body: &[u8]) -> Result<Irot> {
    if body.is_empty() {
        return Err(Error::invalid("MOV: irot payload empty"));
    }
    Ok(Irot {
        steps: body[0] & 0x03,
    })
}

/// `imir` payload: 1 byte; low 1 bit is the axis.
fn parse_imir(body: &[u8]) -> Result<Imir> {
    if body.is_empty() {
        return Err(Error::invalid("MOV: imir payload empty"));
    }
    Ok(Imir {
        axis: body[0] & 0x01,
    })
}

/// Parse an `auxC` (AuxiliaryTypeProperty) payload.
///
/// On-disk shape per HEIF §7.5.1:
///
/// ```text
/// FullBox header              4 bytes (version=0, flags=0)
/// aux_type                    NUL-terminated UTF-8 URN
/// aux_subtype                 trailing reserved bytes (often empty)
/// ```
///
/// The `aux_type` URN identifies the auxiliary purpose of the item the
/// caller's `ipma` row associates this property with — most commonly an
/// alpha plane (`urn:mpeg:hevc:2015:auxid:1` or
/// `urn:mpeg:mpegB:cicp:systems:auxiliary:alpha`). See [`AuxC::is_alpha`]
/// + [`AuxC::is_depth`] for typed dispatch.
///
/// Returns `Err(InvalidData)` when the payload is shorter than the
/// 4-byte FullBox header.
pub fn parse_auxc_payload(body: &[u8]) -> Result<AuxC> {
    if body.len() < 4 {
        return Err(Error::invalid("MOV: auxC payload < 4 bytes"));
    }
    let after = &body[4..];
    let nul = after.iter().position(|&b| b == 0).unwrap_or(after.len());
    let aux_type = std::str::from_utf8(&after[..nul]).unwrap_or("").to_string();
    // Skip the NUL when present.
    let subtype_start = if nul < after.len() { nul + 1 } else { nul };
    let aux_subtype = after[subtype_start..].to_vec();
    Ok(AuxC {
        aux_type,
        aux_subtype,
    })
}

/// Parse a `clli` (ContentLightLevelInformation) payload.
///
/// On-disk shape per ISO/IEC 23008-12 §6.5.x (CTA-861.3 derived):
///
/// ```text
/// max_content_light_level         u16 BE   cd/m²
/// max_pic_average_light_level     u16 BE   cd/m²
/// ```
///
/// `clli` is a *fixed-size* property body — not a FullBox — so callers
/// should expect a 4-byte payload. Some authoring tools include a
/// FullBox version+flags prefix (4 bytes) before the payload; this
/// helper accepts both shapes by checking the body length.
///
/// Returns `Err(InvalidData)` when the body is shorter than 4 bytes
/// (the bare property), or shorter than 8 bytes when the body is
/// FullBox-prefixed.
pub fn parse_clli_payload(body: &[u8]) -> Result<Clli> {
    let p: &[u8] = match body.len() {
        4 => body,
        // FullBox-prefixed shape: 4-byte ver+flags then 4-byte body.
        8 => &body[4..],
        _ => {
            return Err(Error::invalid(format!(
                "MOV: clli payload must be 4 or 8 bytes, got {}",
                body.len()
            )))
        }
    };
    Ok(Clli {
        max_content_light_level: u16::from_be_bytes([p[0], p[1]]),
        max_pic_average_light_level: u16::from_be_bytes([p[2], p[3]]),
    })
}

/// Parse an `mdcv` (MasteringDisplayColourVolume) payload.
///
/// On-disk shape per ISO/IEC 23008-12 §6.5.x (SMPTE ST 2086 derived):
///
/// ```text
/// display_primaries_x[c]   u16 BE   c=0..2 (G, B, R per ST 2086)
/// display_primaries_y[c]   u16 BE   c=0..2
/// white_point_x            u16 BE
/// white_point_y            u16 BE
/// max_display_luminance    u32 BE   0.0001 cd/m² steps
/// min_display_luminance    u32 BE   0.0001 cd/m² steps
/// ```
///
/// 24 bytes for the six u16 chromaticities + 4 bytes for the white
/// point + 8 bytes for the luminance pair = **24 bytes total** for the
/// chromaticities and luminance section. Wait — the actual on-disk
/// layout is: 6 × u16 (12 bytes) + 2 × u16 (4 bytes) + 2 × u32 (8 bytes)
/// = **24 bytes**. Like `clli`, callers may also see a 4-byte FullBox
/// prefix yielding a 28-byte body.
///
/// Returns `Err(InvalidData)` when the body is shorter than 24 bytes
/// (bare property) or 28 bytes (FullBox-prefixed).
pub fn parse_mdcv_payload(body: &[u8]) -> Result<Mdcv> {
    let p: &[u8] = match body.len() {
        24 => body,
        28 => &body[4..],
        _ => {
            return Err(Error::invalid(format!(
                "MOV: mdcv payload must be 24 or 28 bytes, got {}",
                body.len()
            )))
        }
    };
    let mut display_primaries = [(0u16, 0u16); 3];
    for c in 0..3 {
        let x = u16::from_be_bytes([p[c * 2], p[c * 2 + 1]]);
        let y = u16::from_be_bytes([p[6 + c * 2], p[6 + c * 2 + 1]]);
        display_primaries[c] = (x, y);
    }
    let white_point = (
        u16::from_be_bytes([p[12], p[13]]),
        u16::from_be_bytes([p[14], p[15]]),
    );
    let max_display_luminance = u32::from_be_bytes([p[16], p[17], p[18], p[19]]);
    let min_display_luminance = u32::from_be_bytes([p[20], p[21], p[22], p[23]]);
    Ok(Mdcv {
        display_primaries,
        white_point,
        max_display_luminance,
        min_display_luminance,
    })
}

/// Parse a `cclv` (ContentColourVolume) payload.
///
/// On-disk shape per ISO/IEC 23008-12 §6.5.x (HEVC SEI 144 derived):
///
/// ```text
/// FullBox header                                 4 bytes (version, flags)
/// flags byte                                     1 byte:
///     bit 7 = ccv_cancel_flag
///     bit 6 = ccv_persistence_flag
///     bit 5 = ccv_primaries_present_flag
///     bit 4 = ccv_min_luminance_value_present_flag
///     bit 3 = ccv_max_luminance_value_present_flag
///     bit 2 = ccv_avg_luminance_value_present_flag
///     bit 1..0 = reserved (zero)
/// // optionally — skipped when the corresponding *_present bit is 0:
/// ccv_primaries_x[c]      i32 BE  (c = 0..2; G, B, R)
/// ccv_primaries_y[c]      i32 BE  (c = 0..2)
/// ccv_min_luminance_value u32 BE
/// ccv_max_luminance_value u32 BE
/// ccv_avg_luminance_value u32 BE
/// ```
///
/// Returns `Err(InvalidData)` when the body is shorter than 5 bytes
/// (the FullBox + flags) or when the present-flags drive a body length
/// the input doesn't satisfy.
pub fn parse_cclv_payload(body: &[u8]) -> Result<Cclv> {
    if body.len() < 5 {
        return Err(Error::invalid(
            "MOV: cclv payload < 5 bytes (FullBox + flags)",
        ));
    }
    // Skip the 4-byte FullBox version+flags header.
    let p = &body[4..];
    let flags = p[0];
    let cancel = (flags & 0x80) != 0;
    let persist = (flags & 0x40) != 0;
    let prim_present = (flags & 0x20) != 0;
    let min_present = (flags & 0x10) != 0;
    let max_present = (flags & 0x08) != 0;
    let avg_present = (flags & 0x04) != 0;
    let mut idx = 1usize;
    let primaries = if prim_present {
        if p.len() < idx + 24 {
            return Err(Error::invalid("MOV: cclv primaries truncated"));
        }
        let mut prims = [(0i32, 0i32); 3];
        for c in 0..3 {
            let x = i32::from_be_bytes([
                p[idx + c * 4],
                p[idx + c * 4 + 1],
                p[idx + c * 4 + 2],
                p[idx + c * 4 + 3],
            ]);
            let y = i32::from_be_bytes([
                p[idx + 12 + c * 4],
                p[idx + 12 + c * 4 + 1],
                p[idx + 12 + c * 4 + 2],
                p[idx + 12 + c * 4 + 3],
            ]);
            prims[c] = (x, y);
        }
        idx += 24;
        Some(prims)
    } else {
        None
    };
    let read_u32 =
        |p: &[u8], i: usize| -> u32 { u32::from_be_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]) };
    let min_luminance = if min_present {
        if p.len() < idx + 4 {
            return Err(Error::invalid("MOV: cclv min_luminance truncated"));
        }
        let v = read_u32(p, idx);
        idx += 4;
        Some(v)
    } else {
        None
    };
    let max_luminance = if max_present {
        if p.len() < idx + 4 {
            return Err(Error::invalid("MOV: cclv max_luminance truncated"));
        }
        let v = read_u32(p, idx);
        idx += 4;
        Some(v)
    } else {
        None
    };
    let avg_luminance = if avg_present {
        if p.len() < idx + 4 {
            return Err(Error::invalid("MOV: cclv avg_luminance truncated"));
        }
        let v = read_u32(p, idx);
        // idx += 4; // not needed; trailing bytes are tolerated.
        let _ = idx;
        Some(v)
    } else {
        None
    };
    Ok(Cclv {
        cancel_flag: cancel,
        persistence_flag: persist,
        primaries,
        min_luminance,
        max_luminance,
        avg_luminance,
    })
}

/// Parse `ipma` (FullBox) payload.
fn parse_ipma(body: &[u8]) -> Result<Vec<ItemPropertyAssociation>> {
    if body.len() < 8 {
        return Err(Error::invalid("MOV: ipma payload < 8 bytes"));
    }
    let version = body[0];
    let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
    let large_index = (flags & 0x01) != 0;
    let mut p = 4usize;
    let entry_count = u32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
    p += 4;

    let mut out = Vec::with_capacity(entry_count.min(1024) as usize);
    for _ in 0..entry_count {
        let item_id = if version == 0 {
            if p + 2 > body.len() {
                return Err(Error::invalid("MOV: ipma item_ID v0 truncated"));
            }
            let v = u16::from_be_bytes([body[p], body[p + 1]]) as u32;
            p += 2;
            v
        } else {
            if p + 4 > body.len() {
                return Err(Error::invalid("MOV: ipma item_ID v1 truncated"));
            }
            let v = u32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
            p += 4;
            v
        };
        if p >= body.len() {
            return Err(Error::invalid("MOV: ipma association_count missing"));
        }
        let assoc_count = body[p] as usize;
        p += 1;
        let mut associations = Vec::with_capacity(assoc_count);
        for _ in 0..assoc_count {
            if large_index {
                if p + 2 > body.len() {
                    return Err(Error::invalid("MOV: ipma 16-bit assoc truncated"));
                }
                let raw = u16::from_be_bytes([body[p], body[p + 1]]);
                p += 2;
                associations.push(PropertyAssociation {
                    index: raw & 0x7FFF,
                    essential: (raw & 0x8000) != 0,
                });
            } else {
                if p >= body.len() {
                    return Err(Error::invalid("MOV: ipma 8-bit assoc truncated"));
                }
                let raw = body[p];
                p += 1;
                associations.push(PropertyAssociation {
                    index: (raw & 0x7F) as u16,
                    essential: (raw & 0x80) != 0,
                });
            }
        }
        out.push(ItemPropertyAssociation {
            item_id,
            associations,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::read_atom_header;
    use std::io::Cursor;

    fn push_atom(out: &mut Vec<u8>, fourcc: &[u8; 4], body: &[u8]) {
        let size = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(fourcc);
        out.extend_from_slice(body);
    }

    fn ispe_body(w: u32, h: u32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&w.to_be_bytes());
        p.extend_from_slice(&h.to_be_bytes());
        p
    }

    fn pixi_body(bits: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.push(bits.len() as u8);
        p.extend_from_slice(bits);
        p
    }

    fn colr_nclx() -> Vec<u8> {
        // colr 'nclx' primaries=1 transfer=13 matrix=6 full_range=true
        let mut p = Vec::new();
        p.extend_from_slice(b"nclx");
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&13u16.to_be_bytes());
        p.extend_from_slice(&6u16.to_be_bytes());
        p.push(0x80); // full_range bit set
        p
    }

    fn build_ipco(props: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (fc, payload) in props {
            push_atom(&mut body, fc, payload);
        }
        body
    }

    fn build_ipma_v0(rows: &[(u16, &[(u8, bool)])]) -> Vec<u8> {
        // ver=0, flags=0 (8-bit indices)
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        body.extend_from_slice(&(rows.len() as u32).to_be_bytes());
        for (item_id, assocs) in rows {
            body.extend_from_slice(&item_id.to_be_bytes());
            body.push(assocs.len() as u8);
            for (idx, essential) in *assocs {
                let mut byte = idx & 0x7F;
                if *essential {
                    byte |= 0x80;
                }
                body.push(byte);
            }
        }
        body
    }

    fn build_iprp(ipco_body: &[u8], ipma_body: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        push_atom(&mut body, b"ipco", ipco_body);
        push_atom(&mut body, b"ipma", ipma_body);
        body
    }

    fn run_parse(iprp_body: &[u8]) -> ItemProperties {
        let mut wrapped = Vec::new();
        push_atom(&mut wrapped, b"iprp", iprp_body);
        let mut c = Cursor::new(wrapped);
        let hdr = read_atom_header(&mut c).unwrap().unwrap();
        parse_iprp(&mut c, &hdr).unwrap()
    }

    #[test]
    fn ipco_collects_property_boxes_in_order() {
        let ipco = build_ipco(&[
            (b"ispe", &ispe_body(256, 256)),
            (b"colr", &colr_nclx()),
            (b"pixi", &pixi_body(&[8, 8, 8])),
        ]);
        let ipma = build_ipma_v0(&[]);
        let iprp = build_iprp(&ipco, &ipma);
        let p = run_parse(&iprp);
        assert_eq!(p.properties.len(), 3);
        assert!(matches!(
            p.properties[0],
            ItemProperty::Ispe(Ispe {
                width: 256,
                height: 256
            })
        ));
        assert!(matches!(p.properties[1], ItemProperty::Colr(_)));
        assert!(matches!(p.properties[2], ItemProperty::Pixi(_)));
    }

    #[test]
    fn ipma_v0_8bit_indices_resolve_to_properties() {
        let ipco = build_ipco(&[
            (b"ispe", &ispe_body(64, 64)),
            (b"colr", &colr_nclx()),
            (b"pixi", &pixi_body(&[8, 8, 8])),
        ]);
        // item 1 → indices 1 (essential=true) and 3 (essential=false).
        let ipma = build_ipma_v0(&[(1, &[(1, true), (3, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let row = p.associations_for(1).unwrap();
        assert_eq!(row.associations.len(), 2);
        assert_eq!(row.associations[0].index, 1);
        assert!(row.associations[0].essential);
        assert_eq!(row.associations[1].index, 3);
        assert!(!row.associations[1].essential);
        let resolved = p.resolve(1);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].fourcc(), ISPE);
        assert_eq!(resolved[1].fourcc(), PIXI);
    }

    #[test]
    fn ipma_v1_16bit_indices() {
        let ipco = build_ipco(&[(b"ispe", &ispe_body(2, 2))]);
        let mut ipma = Vec::new();
        ipma.push(1u8); // version=1
        ipma.extend_from_slice(&[0, 0, 1]); // flags=1 → 16-bit indices
        ipma.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        ipma.extend_from_slice(&42u32.to_be_bytes()); // item_ID (u32 because v1)
        ipma.push(1); // association_count
        ipma.extend_from_slice(&0x8001u16.to_be_bytes()); // essential=1, idx=1
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let row = p.associations_for(42).unwrap();
        assert_eq!(row.associations.len(), 1);
        assert_eq!(row.associations[0].index, 1);
        assert!(row.associations[0].essential);
    }

    #[test]
    fn shared_property_used_by_multiple_items() {
        // Classic HEIF grid: 4 items, each pointing at the same hvcC,
        // colr, ispe, pixi indices. Models the corpus's
        // `still-image-grid-2x2` fixture with an `Other` for hvcC.
        let ipco = build_ipco(&[
            (b"hvcC", &[1u8, 2, 3, 4][..]),
            (b"ispe", &ispe_body(128, 128)),
            (b"colr", &colr_nclx()),
            (b"pixi", &pixi_body(&[8, 8, 8])),
        ]);
        let row = [(1, true), (2, true), (3, false), (4, false)];
        let ipma = build_ipma_v0(&[(2, &row), (3, &row), (4, &row), (5, &row)]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        for id in 2..=5u32 {
            let r = p.resolve(id);
            assert_eq!(r.len(), 4);
            assert_eq!(r[0].fourcc(), *b"hvcC");
            assert_eq!(r[1].fourcc(), ISPE);
            assert_eq!(r[2].fourcc(), COLR);
            assert_eq!(r[3].fourcc(), PIXI);
        }
        // Helpers
        assert_eq!(
            p.ispe_for(3),
            Some(Ispe {
                width: 128,
                height: 128
            })
        );
        assert!(p.colr_for(3).is_some());
    }

    #[test]
    fn irot_imir_auxc_decode() {
        let auxc_payload = {
            let mut v = Vec::new();
            v.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
            v.extend_from_slice(b"urn:mpeg:hevc:2015:auxid:1");
            v.push(0); // NUL
            v
        };
        let ipco = build_ipco(&[
            (b"irot", &[3u8][..]),
            (b"imir", &[1u8][..]),
            (b"auxC", &auxc_payload),
        ]);
        let ipma = build_ipma_v0(&[(1, &[(1, false), (2, false), (3, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let (rot, mir) = p.orientation_for(1);
        assert_eq!(rot.unwrap().steps, 3);
        assert_eq!(mir.unwrap().axis, 1);
        let auxc = p.auxc_for(1).unwrap();
        assert_eq!(auxc.aux_type, "urn:mpeg:hevc:2015:auxid:1");
    }

    #[test]
    fn out_of_range_index_silently_skipped() {
        let ipco = build_ipco(&[(b"ispe", &ispe_body(2, 2))]);
        // Index 99 is out of range — must not panic.
        let ipma = build_ipma_v0(&[(1, &[(99, false), (1, true)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let r = p.resolve(1);
        // Only one valid resolution.
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn empty_iprp_decodes() {
        let mut wrapped = Vec::new();
        push_atom(&mut wrapped, b"iprp", b"");
        let mut c = Cursor::new(wrapped);
        let hdr = read_atom_header(&mut c).unwrap().unwrap();
        let p = parse_iprp(&mut c, &hdr).unwrap();
        assert!(p.properties.is_empty());
        assert!(p.associations.is_empty());
    }

    #[test]
    fn ipma_assoc_count_zero_yields_empty_row() {
        let ipco = build_ipco(&[(b"ispe", &ispe_body(2, 2))]);
        let ipma = build_ipma_v0(&[(1, &[])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let row = p.associations_for(1).unwrap();
        assert!(row.associations.is_empty());
        assert!(p.resolve(1).is_empty());
    }

    #[test]
    fn resolve_strict_accepts_natively_modelled_essentials() {
        // `ispe` is essential and natively modelled; resolve_strict
        // succeeds with no extra recognised list.
        let ipco = build_ipco(&[(b"ispe", &ispe_body(64, 64))]);
        let ipma = build_ipma_v0(&[(1, &[(1, true)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let r = p.resolve_strict(1, &[]).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].fourcc(), ISPE);
    }

    #[test]
    fn resolve_strict_rejects_unrecognised_essential_other() {
        // hvcC is an `Other` (we don't natively decode it). When
        // marked essential and not in the recognised list, the strict
        // resolver returns the offending fourcc.
        let ipco = build_ipco(&[
            (b"ispe", &ispe_body(64, 64)),
            (b"hvcC", &[1u8, 2, 3, 4][..]),
        ]);
        let ipma = build_ipma_v0(&[(1, &[(1, false), (2, true)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        match p.resolve_strict(1, &[]) {
            Err(fc) => assert_eq!(fc, *b"hvcC"),
            Ok(_) => panic!("expected essential-bit failure"),
        }
        // Allow-list lets the caller pass essential `hvcC` through.
        let r = p.resolve_strict(1, &[*b"hvcC"]).unwrap();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn parse_colr_payload_nclx_bt709_srgb_bt601() {
        // Round-11 fixture: BT.709 primaries (1), sRGB transfer (13),
        // BT.601 matrix (5), full_range_flag = 0.
        let mut p = Vec::new();
        p.extend_from_slice(b"nclx");
        p.extend_from_slice(&1u16.to_be_bytes()); // primaries = BT.709
        p.extend_from_slice(&13u16.to_be_bytes()); // transfer = sRGB
        p.extend_from_slice(&5u16.to_be_bytes()); // matrix = BT.601
        p.push(0x00); // full_range = false
        let info = parse_colr_payload(&p).unwrap();
        match info {
            ColrInfo::Nclx {
                primaries,
                transfer,
                matrix,
                full_range,
            } => {
                assert_eq!(primaries, 1);
                assert_eq!(transfer, 13);
                assert_eq!(matrix, 5);
                assert!(!full_range);
            }
            other => panic!("expected Nclx, got {other:?}"),
        }
    }

    #[test]
    fn parse_colr_payload_nclx_full_range_flag_decodes() {
        let mut p = Vec::new();
        p.extend_from_slice(b"nclx");
        p.extend_from_slice(&9u16.to_be_bytes()); // primaries = BT.2020
        p.extend_from_slice(&16u16.to_be_bytes()); // transfer = PQ
        p.extend_from_slice(&9u16.to_be_bytes()); // matrix = BT.2020 NC
        p.push(0x80); // full_range_flag = 1
        let info = parse_colr_payload(&p).unwrap();
        match info {
            ColrInfo::Nclx { full_range, .. } => assert!(full_range),
            other => panic!("expected Nclx, got {other:?}"),
        }
    }

    #[test]
    fn parse_colr_payload_ricc_preserves_bytes_and_length() {
        // 24-byte synthetic profile body — the parser must surface it
        // verbatim and report the right colour_type tag.
        let mut p = Vec::new();
        p.extend_from_slice(b"rICC");
        let profile_bytes: Vec<u8> = (0u8..24).collect();
        p.extend_from_slice(&profile_bytes);
        let info = parse_colr_payload(&p).unwrap();
        assert_eq!(info.colour_type(), *b"rICC");
        assert!(info.is_icc());
        assert_eq!(info.icc_bytes().unwrap(), &profile_bytes[..]);
        assert_eq!(info.icc_bytes().unwrap().len(), 24);
        match info {
            ColrInfo::RestrictedIcc(b) => assert_eq!(b, profile_bytes),
            other => panic!("expected RestrictedIcc, got {other:?}"),
        }
    }

    #[test]
    fn parse_colr_payload_prof_preserves_bytes_and_length() {
        // 64-byte synthetic profile body — exercise the unrestricted
        // shape independently of rICC.
        let mut p = Vec::new();
        p.extend_from_slice(b"prof");
        let profile_bytes: Vec<u8> = (0u8..64).collect();
        p.extend_from_slice(&profile_bytes);
        let info = parse_colr_payload(&p).unwrap();
        assert_eq!(info.colour_type(), *b"prof");
        assert!(info.is_icc());
        assert_eq!(info.icc_bytes().unwrap().len(), 64);
        match info {
            ColrInfo::UnrestrictedIcc(b) => assert_eq!(b, profile_bytes),
            other => panic!("expected UnrestrictedIcc, got {other:?}"),
        }
    }

    #[test]
    fn parse_colr_payload_rejects_apple_nclc_for_heif() {
        let mut p = Vec::new();
        p.extend_from_slice(b"nclc");
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&1u16.to_be_bytes());
        assert!(parse_colr_payload(&p).is_err());
    }

    #[test]
    fn parse_colr_payload_rejects_unknown_tag() {
        let mut p = Vec::new();
        p.extend_from_slice(b"XXXX");
        p.extend_from_slice(&[0u8; 8]);
        assert!(parse_colr_payload(&p).is_err());
    }

    #[test]
    fn parse_colr_payload_rejects_truncated_nclx() {
        // nclx tag but body is only 5 bytes (need 7).
        let mut p = Vec::new();
        p.extend_from_slice(b"nclx");
        p.extend_from_slice(&[0u8; 5]);
        assert!(parse_colr_payload(&p).is_err());
    }

    #[test]
    fn color_profile_accessor_returns_nclx_for_associated_item() {
        // ipco carries [ispe, colr(nclx)]; ipma associates both with
        // item 1; color_profile(1) returns the nclx colour info.
        let ipco = build_ipco(&[(b"ispe", &ispe_body(64, 64)), (b"colr", &colr_nclx())]);
        let ipma = build_ipma_v0(&[(1, &[(1, true), (2, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let info = p.color_profile(1).expect("expected nclx profile");
        match info {
            ColrInfo::Nclx {
                primaries,
                transfer,
                matrix,
                full_range,
            } => {
                assert_eq!((primaries, transfer, matrix), (1, 13, 6));
                assert!(full_range);
            }
            other => panic!("expected Nclx, got {other:?}"),
        }
    }

    #[test]
    fn color_profile_accessor_returns_none_for_apple_nclc() {
        // colr 'nclc' is not a HEIF colour profile — the accessor
        // returns None even though the underlying ipma row points at
        // a `colr` property.
        let mut nclc = Vec::new();
        nclc.extend_from_slice(b"nclc");
        nclc.extend_from_slice(&1u16.to_be_bytes());
        nclc.extend_from_slice(&1u16.to_be_bytes());
        nclc.extend_from_slice(&1u16.to_be_bytes());
        let ipco = build_ipco(&[(b"colr", &nclc)]);
        let ipma = build_ipma_v0(&[(1, &[(1, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        assert!(p.color_profile(1).is_none());
    }

    #[test]
    fn color_profile_accessor_returns_none_when_no_colr() {
        let ipco = build_ipco(&[(b"ispe", &ispe_body(2, 2))]);
        let ipma = build_ipma_v0(&[(1, &[(1, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        assert!(p.color_profile(1).is_none());
    }

    #[test]
    fn color_profile_accessor_returns_icc_for_prof_payload() {
        // Build a `prof` colr payload (4-byte tag + 16 ICC bytes) and
        // verify the accessor surfaces it as UnrestrictedIcc.
        let mut prof = Vec::new();
        prof.extend_from_slice(b"prof");
        prof.extend_from_slice(&[0xAB; 16]);
        let ipco = build_ipco(&[(b"colr", &prof)]);
        let ipma = build_ipma_v0(&[(1, &[(1, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let info = p.color_profile(1).expect("expected prof profile");
        match info {
            ColrInfo::UnrestrictedIcc(b) => {
                assert_eq!(b.len(), 16);
                assert!(b.iter().all(|&x| x == 0xAB));
            }
            other => panic!("expected UnrestrictedIcc, got {other:?}"),
        }
    }

    #[test]
    fn pixi_accessor_returns_channel_bit_depths() {
        // sRGB-shaped pixi: 3 channels × 8 bits.
        let ipco = build_ipco(&[(b"pixi", &pixi_body(&[8, 8, 8]))]);
        let ipma = build_ipma_v0(&[(7, &[(1, true)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let info = p.pixi(7).expect("pixi associated");
        assert_eq!(info.num_channels(), 3);
        assert_eq!(info.channels, vec![8, 8, 8]);
    }

    #[test]
    fn pixi_accessor_returns_rgba_4_channels() {
        let ipco = build_ipco(&[(b"pixi", &pixi_body(&[8, 8, 8, 8]))]);
        let ipma = build_ipma_v0(&[(1, &[(1, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let info = p.pixi(1).expect("rgba pixi associated");
        assert_eq!(info.channels, vec![8, 8, 8, 8]);
    }

    #[test]
    fn pixi_accessor_returns_hdr_10bit_channels() {
        let ipco = build_ipco(&[(b"pixi", &pixi_body(&[10, 10, 10]))]);
        let ipma = build_ipma_v0(&[(2, &[(1, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let info = p.pixi(2).expect("hdr pixi associated");
        assert_eq!(info.channels, vec![10, 10, 10]);
    }

    #[test]
    fn pixi_accessor_returns_none_when_unassociated() {
        let ipco = build_ipco(&[(b"ispe", &ispe_body(2, 2))]);
        let ipma = build_ipma_v0(&[(1, &[(1, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        assert!(p.pixi(1).is_none());
    }

    #[test]
    fn resolve_strict_tolerates_non_essential_unknown_other() {
        // Non-essential unknown Other passes through silently — it's
        // the spec's "skip if you don't understand" path.
        let ipco = build_ipco(&[(b"ispe", &ispe_body(64, 64)), (b"xyzZ", &[0u8; 8][..])]);
        let ipma = build_ipma_v0(&[(1, &[(1, true), (2, false)])]);
        let p = run_parse(&build_iprp(&ipco, &ipma));
        let r = p.resolve_strict(1, &[]).unwrap();
        assert_eq!(r.len(), 2);
    }
}
