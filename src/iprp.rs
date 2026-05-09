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
use crate::media_meta::{parse_clap, parse_colr, parse_pasp, Clap, ColorParameters, Pasp};
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

/// One property entry inside `ipco`.
///
/// `Other` is a fall-through for any box type we don't model
/// natively (typical examples: `hvcC`, `av1C`, `lsel`, `clli`,
/// `mdcv`, `cclv`). Callers can still match on its fourcc and parse
/// the raw payload themselves — for instance, `hvcC` is parsed by
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

    /// First `auxC` attached to the item.
    pub fn auxc_for(&self, item_id: u32) -> Option<&AuxC> {
        for p in self.resolve(item_id) {
            if let ItemProperty::AuxC(a) = p {
                return Some(a);
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
        t if t == &AUXC => ItemProperty::AuxC(parse_auxc(&body)?),
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

/// `auxC` payload: 4-byte ver+flags, NUL-terminated UTF-8 aux_type,
/// optional trailing aux_subtype bytes.
fn parse_auxc(body: &[u8]) -> Result<AuxC> {
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
