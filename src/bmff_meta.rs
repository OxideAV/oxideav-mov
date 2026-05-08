//! ISO BMFF `meta` box (`§8.11`) item-tracking surface.
//!
//! The QTFF / Apple `meta` shape (`hdlr` + `keys` + `ilst`) is parsed
//! by [`crate::media_meta`]; this module covers the *other* common
//! `meta` flavour: the ISO/IEC 14496-12 §8.11 file-level item directory
//! used by HEIF/HEIC, MIAF, AVIF, JPEG-XL/JXL, MPEG-7, etc.
//!
//! Layout per ISO/IEC 14496-12:2015(E) §8.11.1.2:
//!
//! ```text
//! meta { FullBox 'meta' v=0 f=0
//!     hdlr 'hdlr'                // mandatory — handler_type carries semantics
//!     pitm 'pitm'                // optional — primary item ID
//!     dinf 'dinf'                // optional — file/data references
//!     iloc 'iloc'                // optional — item location table
//!     ipro 'ipro'                // optional — item protection (rare; surface only)
//!     iinf 'iinf'                // optional — item info entries
//!     iref 'iref'                // optional — item references (sibling of tref)
//!     idat 'idat'                // optional — inline item data
//!     xml  'xml '                // optional — XML payload (UTF-8)
//!     bxml 'bxml'                // optional — binarised XML payload
//!     ...                        // any other handler-specific boxes
//! }
//! ```
//!
//! The on-disk records inside `iloc` and `iinf` are version-dependent —
//! we surface every documented version. Unknown / out-of-range items are
//! skipped silently rather than failing the whole parse: a HEIF file
//! that carries an `iref` reference type we don't know about should
//! still let callers walk its primary item.

use crate::atom::{fourcc, read_payload, walk_children, AtomHeader};
use std::io::{Read, Seek, SeekFrom};

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

// FourCCs documented in §8.11.
pub const PITM: [u8; 4] = fourcc("pitm");
pub const IINF: [u8; 4] = fourcc("iinf");
pub const ILOC: [u8; 4] = fourcc("iloc");
pub const IDAT: [u8; 4] = fourcc("idat");
pub const IREF: [u8; 4] = fourcc("iref");
pub const INFE: [u8; 4] = fourcc("infe");
pub const XML_: [u8; 4] = fourcc("xml ");
pub const BXML: [u8; 4] = fourcc("bxml");
pub const HDLR: [u8; 4] = fourcc("hdlr");
pub const IPRO: [u8; 4] = fourcc("ipro");

/// One extent inside an [`ItemLocation`]. Items may be split across
/// multiple extents; the resource is the concatenation of every
/// extent's data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemExtent {
    /// 1-based item-reference index (`extent_index`); used only when
    /// `construction_method == 2` (item_offset). Value `0` means
    /// "not present" (we carry it that way regardless of `index_size`).
    pub index: u64,
    /// Absolute offset from the data origin (file / `idat` / item).
    pub offset: u64,
    /// Length in bytes; `0` means "until end of source".
    pub length: u64,
}

/// One row of the §8.11.3 Item Location Box. Versions 0/1/2 are all
/// surfaced through this single struct; the version-specific bits
/// (large item-IDs, `construction_method`, extent indices) are
/// promoted to wide types so callers don't have to branch per version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemLocation {
    pub item_id: u32,
    /// Construction method per §8.11.3: 0 = file_offset, 1 = idat_offset,
    /// 2 = item_offset. Only present in v1/v2; in v0 we report 0.
    pub construction_method: u8,
    /// 0 = same file as this metadata; otherwise a 1-based index into
    /// the `dref` table.
    pub data_reference_index: u16,
    /// Base offset added to every extent's `offset`. `0` when
    /// `base_offset_size` is 0.
    pub base_offset: u64,
    pub extents: Vec<ItemExtent>,
}

/// One row of the §8.11.6 Item Information Box. The `infe` versions
/// 0–3 are merged here; v2/v3 carry an `item_type` FourCC, v0/v1 don't.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemInfoEntry {
    pub item_id: u32,
    pub item_protection_index: u16,
    /// `item_type` FourCC, present only on `infe` v2 / v3. Common
    /// values: `mime`, `uri `, `hvc1`, `av01`, `Exif`, `mime` (HEIF),
    /// `grid`, `iovl`, `iden`. Empty (`[0;4]`) on v0/v1.
    pub item_type: [u8; 4],
    /// UTF-8 symbolic name of the item (HEIF: `"Exif"`, `"image/jpeg"`, …).
    pub item_name: String,
    /// MIME type when `item_type == 'mime'`; empty otherwise.
    pub content_type: String,
    /// HTTP-style content encoding (`gzip` / `deflate` / …); empty for
    /// the common identity case.
    pub content_encoding: String,
    /// Absolute URI when `item_type == 'uri '`; empty otherwise.
    pub item_uri_type: String,
}

/// One typed entry of the §8.11.12 Item Reference Box. The reference
/// type is the inner box's FourCC (e.g. `dimg`, `cdsc`, `auxl`, `thmb`,
/// `iloc`, `fdel`, `mskC`); the `from_item_id` points at the source
/// item, and each `to_item_id` is a target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemReference {
    pub kind: [u8; 4],
    pub from_item_id: u32,
    pub to_item_ids: Vec<u32>,
}

/// Parsed ISO BMFF §8.11 `meta` box surface. All fields are optional;
/// a HEIF still-image file typically has `handler_type = "pict"`,
/// non-empty `iinf` + `iloc`, a single-entry `pitm`, and no `xml` /
/// `bxml`. An MPEG-7 metadata-only `.mp4` typically carries `xml ` or
/// `bxml` and no items.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BmffMeta {
    /// `hdlr.handler_type` from the `meta` box's mandatory hdlr child.
    /// Empty when the box has no `hdlr` (malformed but tolerated).
    pub handler_type: [u8; 4],
    /// Primary item id (`pitm`); `None` when absent.
    pub primary_item: Option<u32>,
    /// Item info entries (`iinf`).
    pub items: Vec<ItemInfoEntry>,
    /// Item location table (`iloc`).
    pub locations: Vec<ItemLocation>,
    /// Inline item data (`idat`); empty when absent.
    pub idat: Vec<u8>,
    /// XML payload from a child `xml ` box; empty when absent.
    pub xml: String,
    /// Binary-XML payload from a child `bxml` box; empty when absent.
    pub bxml: Vec<u8>,
    /// `iref` typed references between items.
    pub references: Vec<ItemReference>,
}

impl BmffMeta {
    /// Look up an item-info entry by item-id.
    pub fn find_item(&self, item_id: u32) -> Option<&ItemInfoEntry> {
        self.items.iter().find(|i| i.item_id == item_id)
    }

    /// Look up an item-location row by item-id.
    pub fn find_location(&self, item_id: u32) -> Option<&ItemLocation> {
        self.locations.iter().find(|l| l.item_id == item_id)
    }

    /// Resolve the primary item info entry (when both `pitm` and `iinf`
    /// are present).
    pub fn primary_item_info(&self) -> Option<&ItemInfoEntry> {
        self.primary_item.and_then(|id| self.find_item(id))
    }
}

/// Try to parse the body of an ISO BMFF `meta` box at the reader's
/// current position. Returns `Ok(None)` when the box obviously isn't
/// the §8.11 shape (no `hdlr` and none of the documented children).
///
/// Caller side-effects: the reader position on exit is unspecified; the
/// caller's atom walker is expected to seek to the parent's body_end.
pub fn parse_bmff_meta<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
) -> Result<Option<BmffMeta>> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    // ISO BMFF `meta` is a FullBox: skip the 4-byte ver+flags header.
    // Apple's meta lacks this prefix but parse_meta_atom handles its own
    // shape; we expect to be called only after Apple parse declined.
    let pos_now = r.stream_position()?;
    let remain = body_end.saturating_sub(pos_now);
    if remain < 4 {
        return Ok(None);
    }
    r.seek(SeekFrom::Start(pos_now + 4))?;

    let mut out = BmffMeta::default();
    let mut found_iso_marker = false;

    walk_children(r, Some(body_end), |r, child| {
        match &child.fourcc {
            t if t == &HDLR => {
                let body = read_payload(r, child)?;
                if let Some(ht) = parse_hdlr_handler_type(&body) {
                    out.handler_type = ht;
                    found_iso_marker = true;
                }
            }
            t if t == &PITM => {
                let body = read_payload(r, child)?;
                out.primary_item = Some(parse_pitm(&body)?);
                found_iso_marker = true;
            }
            t if t == &IINF => {
                let body = read_payload(r, child)?;
                out.items = parse_iinf(&body)?;
                found_iso_marker = true;
            }
            t if t == &ILOC => {
                let body = read_payload(r, child)?;
                out.locations = parse_iloc(&body)?;
                found_iso_marker = true;
            }
            t if t == &IDAT => {
                out.idat = read_payload(r, child)?;
                found_iso_marker = true;
            }
            t if t == &XML_ => {
                let body = read_payload(r, child)?;
                out.xml = parse_xml_box(&body);
                found_iso_marker = true;
            }
            t if t == &BXML => {
                let body = read_payload(r, child)?;
                out.bxml = parse_bxml_box(&body);
                found_iso_marker = true;
            }
            t if t == &IREF => {
                out.references = parse_iref(r, child)?;
                found_iso_marker = true;
            }
            _ => {}
        }
        Ok(())
    })?;

    if !found_iso_marker {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

/// Extract `handler_type` from a `hdlr` payload. We mirror the layout
/// from `crate::header::parse_hdlr` but stay byte-direct so the BMFF
/// parser doesn't pull QT-specific component_subtype interpretation.
fn parse_hdlr_handler_type(body: &[u8]) -> Option<[u8; 4]> {
    if body.len() < 12 {
        return None;
    }
    // [ver+flags : 4] [pre_defined : 4] [handler_type : 4] [reserved...]
    let mut ht = [0u8; 4];
    ht.copy_from_slice(&body[8..12]);
    Some(ht)
}

/// Parse a `pitm` payload. Version 0 = u16 item_id, version 1 = u32.
fn parse_pitm(body: &[u8]) -> Result<u32> {
    if body.is_empty() {
        return Err(Error::invalid("MOV: pitm payload empty"));
    }
    let version = body[0];
    let after_hdr = body
        .get(4..)
        .ok_or_else(|| Error::invalid("MOV: pitm payload < 4 bytes (FullBox header missing)"))?;
    match version {
        0 => {
            if after_hdr.len() < 2 {
                return Err(Error::invalid("MOV: pitm v0 truncated"));
            }
            Ok(u16::from_be_bytes([after_hdr[0], after_hdr[1]]) as u32)
        }
        1 => {
            if after_hdr.len() < 4 {
                return Err(Error::invalid("MOV: pitm v1 truncated"));
            }
            Ok(u32::from_be_bytes([
                after_hdr[0],
                after_hdr[1],
                after_hdr[2],
                after_hdr[3],
            ]))
        }
        v => Err(Error::invalid(format!("MOV: pitm unknown version {v}"))),
    }
}

/// Parse an `iloc` payload (versions 0, 1, 2).
fn parse_iloc(body: &[u8]) -> Result<Vec<ItemLocation>> {
    if body.len() < 6 {
        return Err(Error::invalid("MOV: iloc payload < 6 bytes"));
    }
    let version = body[0];
    if version > 2 {
        return Err(Error::invalid(format!(
            "MOV: iloc unknown version {version}"
        )));
    }
    // Skip 4-byte FullBox header.
    let mut p = 4usize;
    let pack = body
        .get(p)
        .copied()
        .ok_or_else(|| Error::invalid("MOV: iloc truncated"))?;
    let offset_size = (pack >> 4) as usize;
    let length_size = (pack & 0x0F) as usize;
    p += 1;
    let pack2 = body
        .get(p)
        .copied()
        .ok_or_else(|| Error::invalid("MOV: iloc truncated"))?;
    let base_offset_size = (pack2 >> 4) as usize;
    let index_size = if version == 1 || version == 2 {
        (pack2 & 0x0F) as usize
    } else {
        0
    };
    p += 1;
    if !is_valid_iloc_size(offset_size)
        || !is_valid_iloc_size(length_size)
        || !is_valid_iloc_size(base_offset_size)
        || !is_valid_iloc_size(index_size)
    {
        return Err(Error::invalid(
            "MOV: iloc offset/length/base/index size not in {0,4,8}",
        ));
    }
    let item_count = if version < 2 {
        if p + 2 > body.len() {
            return Err(Error::invalid("MOV: iloc item_count missing"));
        }
        let n = u16::from_be_bytes([body[p], body[p + 1]]) as u32;
        p += 2;
        n
    } else {
        if p + 4 > body.len() {
            return Err(Error::invalid("MOV: iloc item_count missing (v2)"));
        }
        let n = u32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
        p += 4;
        n
    };

    let mut out = Vec::with_capacity(item_count.min(1024) as usize);
    for _ in 0..item_count {
        let item_id = if version < 2 {
            if p + 2 > body.len() {
                return Err(Error::invalid("MOV: iloc item_id truncated"));
            }
            let id = u16::from_be_bytes([body[p], body[p + 1]]) as u32;
            p += 2;
            id
        } else {
            if p + 4 > body.len() {
                return Err(Error::invalid("MOV: iloc item_id truncated (v2)"));
            }
            let id = u32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
            p += 4;
            id
        };
        let construction_method = if version == 1 || version == 2 {
            if p + 2 > body.len() {
                return Err(Error::invalid("MOV: iloc construction_method truncated"));
            }
            let raw = u16::from_be_bytes([body[p], body[p + 1]]);
            p += 2;
            (raw & 0x0F) as u8
        } else {
            0
        };
        if p + 2 > body.len() {
            return Err(Error::invalid("MOV: iloc data_reference_index truncated"));
        }
        let data_reference_index = u16::from_be_bytes([body[p], body[p + 1]]);
        p += 2;
        let base_offset = read_iloc_uint(body, &mut p, base_offset_size)?;
        if p + 2 > body.len() {
            return Err(Error::invalid("MOV: iloc extent_count truncated"));
        }
        let extent_count = u16::from_be_bytes([body[p], body[p + 1]]);
        p += 2;
        let mut extents = Vec::with_capacity(extent_count as usize);
        for _ in 0..extent_count {
            let index = if (version == 1 || version == 2) && index_size > 0 {
                read_iloc_uint(body, &mut p, index_size)?
            } else {
                0
            };
            let offset = read_iloc_uint(body, &mut p, offset_size)?;
            let length = read_iloc_uint(body, &mut p, length_size)?;
            extents.push(ItemExtent {
                index,
                offset,
                length,
            });
        }
        out.push(ItemLocation {
            item_id,
            construction_method,
            data_reference_index,
            base_offset,
            extents,
        });
    }
    Ok(out)
}

fn is_valid_iloc_size(n: usize) -> bool {
    matches!(n, 0 | 4 | 8)
}

fn read_iloc_uint(body: &[u8], p: &mut usize, size: usize) -> Result<u64> {
    match size {
        0 => Ok(0),
        4 => {
            if *p + 4 > body.len() {
                return Err(Error::invalid("MOV: iloc uint(32) truncated"));
            }
            let v = u32::from_be_bytes([body[*p], body[*p + 1], body[*p + 2], body[*p + 3]]) as u64;
            *p += 4;
            Ok(v)
        }
        8 => {
            if *p + 8 > body.len() {
                return Err(Error::invalid("MOV: iloc uint(64) truncated"));
            }
            let v = u64::from_be_bytes([
                body[*p],
                body[*p + 1],
                body[*p + 2],
                body[*p + 3],
                body[*p + 4],
                body[*p + 5],
                body[*p + 6],
                body[*p + 7],
            ]);
            *p += 8;
            Ok(v)
        }
        _ => Err(Error::invalid("MOV: iloc field size not in {0,4,8}")),
    }
}

/// Parse an `iinf` payload — `entry_count` followed by `infe` boxes.
fn parse_iinf(body: &[u8]) -> Result<Vec<ItemInfoEntry>> {
    if body.is_empty() {
        return Err(Error::invalid("MOV: iinf payload empty"));
    }
    let version = body[0];
    if version > 1 {
        return Err(Error::invalid(format!(
            "MOV: iinf unknown version {version}"
        )));
    }
    let mut p = 4usize; // skip ver+flags
    let entry_count = if version == 0 {
        if p + 2 > body.len() {
            return Err(Error::invalid("MOV: iinf entry_count missing"));
        }
        let n = u16::from_be_bytes([body[p], body[p + 1]]) as u32;
        p += 2;
        n
    } else {
        if p + 4 > body.len() {
            return Err(Error::invalid("MOV: iinf entry_count missing (v1)"));
        }
        let n = u32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
        p += 4;
        n
    };

    let mut out = Vec::with_capacity(entry_count.min(1024) as usize);
    for _ in 0..entry_count {
        if p + 8 > body.len() {
            return Err(Error::invalid("MOV: infe header truncated"));
        }
        let size = u32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]) as usize;
        let fourcc = &body[p + 4..p + 8];
        if fourcc != b"infe" {
            // Unknown child — skip the box-sized run and continue.
            if size < 8 || p + size > body.len() {
                return Err(Error::invalid("MOV: iinf child size invalid"));
            }
            p += size;
            continue;
        }
        if size < 12 || p + size > body.len() {
            return Err(Error::invalid("MOV: infe size invalid"));
        }
        let infe_body = &body[p + 8..p + size];
        out.push(parse_infe(infe_body)?);
        p += size;
    }
    Ok(out)
}

/// Parse the body of an `infe` (Item Info Entry) box (post-header).
fn parse_infe(body: &[u8]) -> Result<ItemInfoEntry> {
    if body.len() < 4 {
        return Err(Error::invalid("MOV: infe < 4 bytes"));
    }
    let version = body[0];
    let mut p = 4usize; // skip ver+flags
    let mut entry = ItemInfoEntry::default();
    match version {
        0 | 1 => {
            // [item_ID:u16][item_protection_index:u16][item_name:cstr]
            // [content_type:cstr][content_encoding:cstr (optional)]
            // (v1 carries an extension we surface as raw skip)
            if p + 4 > body.len() {
                return Err(Error::invalid("MOV: infe v0/1 truncated"));
            }
            entry.item_id = u16::from_be_bytes([body[p], body[p + 1]]) as u32;
            entry.item_protection_index = u16::from_be_bytes([body[p + 2], body[p + 3]]);
            p += 4;
            entry.item_name = read_cstr(body, &mut p);
            entry.content_type = read_cstr(body, &mut p);
            if p < body.len() {
                entry.content_encoding = read_cstr(body, &mut p);
            }
            // v1 extension fields are intentionally ignored.
        }
        2 | 3 => {
            // v2: item_ID:u16, v3: item_ID:u32; both then carry
            // protection_index:u16, item_type:u32, item_name:cstr, …
            if version == 2 {
                if p + 2 > body.len() {
                    return Err(Error::invalid("MOV: infe v2 truncated"));
                }
                entry.item_id = u16::from_be_bytes([body[p], body[p + 1]]) as u32;
                p += 2;
            } else {
                if p + 4 > body.len() {
                    return Err(Error::invalid("MOV: infe v3 truncated"));
                }
                entry.item_id =
                    u32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
                p += 4;
            }
            if p + 6 > body.len() {
                return Err(Error::invalid("MOV: infe v2/3 hdr truncated"));
            }
            entry.item_protection_index = u16::from_be_bytes([body[p], body[p + 1]]);
            entry.item_type.copy_from_slice(&body[p + 2..p + 6]);
            p += 6;
            entry.item_name = read_cstr(body, &mut p);
            if &entry.item_type == b"mime" {
                entry.content_type = read_cstr(body, &mut p);
                if p < body.len() {
                    entry.content_encoding = read_cstr(body, &mut p);
                }
            } else if &entry.item_type == b"uri " {
                entry.item_uri_type = read_cstr(body, &mut p);
            }
        }
        v => return Err(Error::invalid(format!("MOV: infe unknown version {v}"))),
    }
    Ok(entry)
}

fn read_cstr(body: &[u8], p: &mut usize) -> String {
    let start = *p;
    while *p < body.len() && body[*p] != 0 {
        *p += 1;
    }
    let s = std::str::from_utf8(&body[start..*p])
        .unwrap_or("")
        .to_string();
    if *p < body.len() {
        *p += 1; // step past NUL
    }
    s
}

/// XML Box body — UTF-8 string, optionally BOM-prefixed (we strip a
/// leading UTF-8 BOM if present and otherwise return bytes lossily as
/// UTF-8). Per §8.11.2.1 a UTF-16 BOM signals UTF-16; we expose the
/// raw bytes (lossy) for that case rather than perform UTF-16 decoding
/// in this round.
fn parse_xml_box(body: &[u8]) -> String {
    // skip 4-byte ver+flags
    if body.len() < 4 {
        return String::new();
    }
    let mut s = &body[4..];
    if s.starts_with(b"\xEF\xBB\xBF") {
        s = &s[3..];
    }
    String::from_utf8_lossy(s).into_owned()
}

/// Binary XML Box body — opaque bytes.
fn parse_bxml_box(body: &[u8]) -> Vec<u8> {
    if body.len() < 4 {
        return Vec::new();
    }
    body[4..].to_vec()
}

/// Parse an `iref` container (variable per `version` in the FullBox
/// header — v0 uses u16 item-IDs, v1 uses u32).
fn parse_iref<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<Vec<ItemReference>> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut ver_flags = [0u8; 4];
    r.read_exact(&mut ver_flags)?;
    let version = ver_flags[0];

    let mut refs = Vec::new();
    walk_children(r, Some(body_end), |r, child| {
        let body = read_payload(r, child)?;
        let mut p = 0usize;
        let read_id = |buf: &[u8], p: &mut usize| -> Result<u32> {
            if version == 0 {
                if *p + 2 > buf.len() {
                    return Err(Error::invalid("MOV: iref v0 id truncated"));
                }
                let v = u16::from_be_bytes([buf[*p], buf[*p + 1]]) as u32;
                *p += 2;
                Ok(v)
            } else {
                if *p + 4 > buf.len() {
                    return Err(Error::invalid("MOV: iref v1 id truncated"));
                }
                let v = u32::from_be_bytes([buf[*p], buf[*p + 1], buf[*p + 2], buf[*p + 3]]);
                *p += 4;
                Ok(v)
            }
        };
        let from_id = read_id(&body, &mut p)?;
        if p + 2 > body.len() {
            return Err(Error::invalid("MOV: iref reference_count missing"));
        }
        let count = u16::from_be_bytes([body[p], body[p + 1]]);
        p += 2;
        let mut to_ids = Vec::with_capacity(count as usize);
        for _ in 0..count {
            to_ids.push(read_id(&body, &mut p)?);
        }
        refs.push(ItemReference {
            kind: child.fourcc,
            from_item_id: from_id,
            to_item_ids: to_ids,
        });
        Ok(())
    })?;
    Ok(refs)
}

/// Returns the absolute byte ranges for an item's data inside the
/// container file (construction_method == 0). Returns `None` when the
/// item uses any other construction method (idat / item_offset) or the
/// item_id is unknown.
pub fn file_extents_for_item(meta: &BmffMeta, item_id: u32) -> Option<Vec<(u64, u64)>> {
    let loc = meta.find_location(item_id)?;
    if loc.construction_method != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(loc.extents.len());
    for e in &loc.extents {
        out.push((loc.base_offset + e.offset, e.length));
    }
    Some(out)
}

/// Returns the item's data when stored inline in `idat`
/// (construction_method == 1). Returns `None` otherwise.
pub fn idat_bytes_for_item(meta: &BmffMeta, item_id: u32) -> Option<Vec<&[u8]>> {
    let loc = meta.find_location(item_id)?;
    if loc.construction_method != 1 {
        return None;
    }
    let mut out = Vec::with_capacity(loc.extents.len());
    for e in &loc.extents {
        let start = (loc.base_offset + e.offset) as usize;
        let end = if e.length == 0 {
            meta.idat.len()
        } else {
            start + e.length as usize
        };
        if start > meta.idat.len() || end > meta.idat.len() {
            return None;
        }
        out.push(&meta.idat[start..end]);
    }
    Some(out)
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

    fn pitm_v0(item_id: u16) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags = 0
        p.extend_from_slice(&item_id.to_be_bytes());
        p
    }

    fn hdlr_pict() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
        p.extend_from_slice(b"pict");
        p.extend_from_slice(&[0u8; 12]); // reserved [3]
        p.push(0); // empty name cstr
        p
    }

    /// Build an `iloc` v0 with one item, one extent, base_offset 0,
    /// offset 100, length 64. offset_size=4, length_size=4, base=0.
    fn iloc_v0_one_item(item_id: u16, off: u32, len: u32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
                                                  // pack: offset_size=4, length_size=4 → 0x44; base_offset_size=0, reserved=0 → 0x00
        p.push(0x44);
        p.push(0x00);
        p.extend_from_slice(&1u16.to_be_bytes()); // item_count
        p.extend_from_slice(&item_id.to_be_bytes()); // item_ID
        p.extend_from_slice(&0u16.to_be_bytes()); // dref index
                                                  // base_offset_size=0 → no base_offset bytes
        p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        p.extend_from_slice(&off.to_be_bytes());
        p.extend_from_slice(&len.to_be_bytes());
        p
    }

    /// Build a v2 `iinf` with one v2 `infe` (item_type='hvc1').
    fn iinf_v0_with_one_v2_infe(item_id: u16, item_type: &[u8; 4], item_name: &str) -> Vec<u8> {
        let mut infe_body = Vec::new();
        infe_body.push(2); // version
        infe_body.extend_from_slice(&[0, 0, 0]); // flags
        infe_body.extend_from_slice(&item_id.to_be_bytes());
        infe_body.extend_from_slice(&0u16.to_be_bytes()); // protection_index
        infe_body.extend_from_slice(item_type);
        infe_body.extend_from_slice(item_name.as_bytes());
        infe_body.push(0); // NUL

        let mut iinf = Vec::new();
        iinf.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        iinf.extend_from_slice(&1u16.to_be_bytes()); // entry_count
        push_atom(&mut iinf, b"infe", &infe_body);
        iinf
    }

    fn build_meta_atom_payload(children: Vec<(&'static [u8; 4], Vec<u8>)>) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // FullBox ver+flags
        for (fc, child_body) in &children {
            push_atom(&mut body, fc, child_body);
        }
        body
    }

    #[test]
    fn parses_pitm_v0() {
        assert_eq!(parse_pitm(&pitm_v0(7)).unwrap(), 7);
    }

    #[test]
    fn parses_iloc_v0_one_item_one_extent() {
        let body = iloc_v0_one_item(11, 0x100, 64);
        let v = parse_iloc(&body).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].item_id, 11);
        assert_eq!(v[0].extents.len(), 1);
        assert_eq!(v[0].extents[0].offset, 0x100);
        assert_eq!(v[0].extents[0].length, 64);
        assert_eq!(v[0].construction_method, 0);
    }

    #[test]
    fn parses_iinf_v0_with_v2_infe() {
        let body = iinf_v0_with_one_v2_infe(11, b"hvc1", "primary");
        let v = parse_iinf(&body).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].item_id, 11);
        assert_eq!(&v[0].item_type, b"hvc1");
        assert_eq!(v[0].item_name, "primary");
    }

    #[test]
    fn parse_bmff_meta_full_round_trip() {
        // Build a complete meta box: hdlr(pict) + pitm(11) + iinf(infe v2 hvc1) + iloc(item 11)
        let body = build_meta_atom_payload(vec![
            (b"hdlr", hdlr_pict()),
            (b"pitm", pitm_v0(11)),
            (b"iinf", iinf_v0_with_one_v2_infe(11, b"hvc1", "primary")),
            (b"iloc", iloc_v0_one_item(11, 0x200, 128)),
        ]);
        let mut wrapped = Vec::new();
        push_atom(&mut wrapped, b"meta", &body);
        let mut c = Cursor::new(wrapped);
        let hdr = read_atom_header(&mut c).unwrap().unwrap();
        let meta = parse_bmff_meta(&mut c, &hdr).unwrap().unwrap();
        assert_eq!(&meta.handler_type, b"pict");
        assert_eq!(meta.primary_item, Some(11));
        assert_eq!(meta.items.len(), 1);
        assert_eq!(meta.items[0].item_id, 11);
        assert_eq!(&meta.items[0].item_type, b"hvc1");
        assert_eq!(meta.locations.len(), 1);
        let loc = meta.find_location(11).unwrap();
        assert_eq!(loc.extents[0].offset, 0x200);
        assert_eq!(loc.extents[0].length, 128);
        // file_extents_for_item resolves construction_method=0 to (offset,len)
        assert_eq!(file_extents_for_item(&meta, 11), Some(vec![(0x200, 128)]));
    }

    #[test]
    fn parse_bmff_meta_with_idat() {
        let mut idat_body = Vec::new();
        idat_body.extend_from_slice(b"hello world");
        // iloc v1 with construction_method=1 (idat), offset=0 length=5 → "hello"
        let mut iloc = Vec::new();
        iloc.push(1); // version
        iloc.extend_from_slice(&[0, 0, 0]); // flags
        iloc.push(0x44); // offset_size=4, length_size=4
        iloc.push(0x00); // base_offset_size=0, index_size=0
        iloc.extend_from_slice(&1u16.to_be_bytes()); // item_count
        iloc.extend_from_slice(&7u16.to_be_bytes()); // item_id
                                                     // construction_method == 1 in low nibble of u16
        iloc.extend_from_slice(&1u16.to_be_bytes());
        iloc.extend_from_slice(&0u16.to_be_bytes()); // dref index
        iloc.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        iloc.extend_from_slice(&0u32.to_be_bytes()); // offset
        iloc.extend_from_slice(&5u32.to_be_bytes()); // length

        let body = build_meta_atom_payload(vec![
            (b"hdlr", hdlr_pict()),
            (b"iloc", iloc),
            (b"idat", idat_body),
        ]);
        let mut wrapped = Vec::new();
        push_atom(&mut wrapped, b"meta", &body);
        let mut c = Cursor::new(wrapped);
        let hdr = read_atom_header(&mut c).unwrap().unwrap();
        let meta = parse_bmff_meta(&mut c, &hdr).unwrap().unwrap();
        assert_eq!(meta.locations[0].construction_method, 1);
        let bytes = idat_bytes_for_item(&meta, 7).unwrap();
        assert_eq!(bytes.len(), 1);
        assert_eq!(bytes[0], b"hello");
    }

    #[test]
    fn parse_bmff_meta_with_xml() {
        let mut xml_body = Vec::new();
        xml_body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        xml_body.extend_from_slice(b"<?xml version=\"1.0\"?><x/>");
        let body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict()), (b"xml ", xml_body)]);
        let mut wrapped = Vec::new();
        push_atom(&mut wrapped, b"meta", &body);
        let mut c = Cursor::new(wrapped);
        let hdr = read_atom_header(&mut c).unwrap().unwrap();
        let meta = parse_bmff_meta(&mut c, &hdr).unwrap().unwrap();
        assert!(meta.xml.contains("<?xml"));
    }

    #[test]
    fn parse_bmff_meta_with_iref_dimg() {
        // iref v0: one 'dimg' single-item-reference box: from=2 → [3,4]
        let mut sirb_body = Vec::new();
        sirb_body.extend_from_slice(&2u16.to_be_bytes()); // from
        sirb_body.extend_from_slice(&2u16.to_be_bytes()); // to_count
        sirb_body.extend_from_slice(&3u16.to_be_bytes());
        sirb_body.extend_from_slice(&4u16.to_be_bytes());
        let mut iref_body = Vec::new();
        iref_body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        push_atom(&mut iref_body, b"dimg", &sirb_body);
        let body = build_meta_atom_payload(vec![(b"hdlr", hdlr_pict()), (b"iref", iref_body)]);
        let mut wrapped = Vec::new();
        push_atom(&mut wrapped, b"meta", &body);
        let mut c = Cursor::new(wrapped);
        let hdr = read_atom_header(&mut c).unwrap().unwrap();
        let meta = parse_bmff_meta(&mut c, &hdr).unwrap().unwrap();
        assert_eq!(meta.references.len(), 1);
        assert_eq!(&meta.references[0].kind, b"dimg");
        assert_eq!(meta.references[0].from_item_id, 2);
        assert_eq!(meta.references[0].to_item_ids, vec![3, 4]);
    }

    #[test]
    fn pitm_unknown_version_errors() {
        let mut p = vec![5u8, 0, 0, 0]; // version=5, garbage flags
        p.extend_from_slice(&0u16.to_be_bytes());
        assert!(parse_pitm(&p).is_err());
    }

    #[test]
    fn iloc_invalid_size_field_errors() {
        // pack byte = 0x55 → offset_size=5, length_size=5 (invalid)
        let mut p = vec![0u8, 0, 0, 0]; // ver+flags
        p.push(0x55);
        p.push(0);
        p.extend_from_slice(&0u16.to_be_bytes());
        assert!(parse_iloc(&p).is_err());
    }
}
