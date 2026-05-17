//! HEIF / HEIC image-item write path.
//!
//! Companion to the existing [`crate::bmff_meta`] + [`crate::iprp`] /
//! [`crate::derived`] read surfaces: takes a caller-supplied set of
//! image items (HEVC / AV1 / JPEG-encoded master / thumbnail items plus
//! derived `grid` / `iovl` / `iden` / `tmap` items) and emits a
//! structurally-valid `.heic` / `.heif` file.
//!
//! ## Emitted layout
//!
//! ```text
//! ftyp                    — major brand 'heic'  (or 'mif1' / 'msf1')
//! meta { FullBox v=0 f=0
//!   hdlr  'pict'          — §8.4.3 handler_type 'pict'
//!   pitm  v=0             — primary item id (§8.11.4)
//!   iinf  v=0             — N × infe v2 (item_type FourCC)
//!   iref  v=0             — typed item refs (dimg/auxl/thmb/cdsc/base)
//!   iprp                  — ipco (flat property array) + N × ipma row
//!   iloc  v=1             — per-item location records
//!     construction_method 0 (file_offset) for coded-image items
//!     construction_method 1 (idat)        for derived (grid/iovl/iden/tmap)
//!   idat                  — inline data for derived items
//! }
//! mdat                    — concatenated coded-image item payloads
//! ```
//!
//! ## References
//!
//! - ISO/IEC 14496-12:2015 — §8.11 (meta), §8.11.3 (iloc), §8.11.4
//!   (pitm), §8.11.6 (iinf/infe), §8.11.11 (idat), §8.11.12 (iref),
//!   §8.11.14 (iprp).
//! - ISO/IEC 23008-12:2017 — §6 (item-property catalogue), §6.6
//!   (derived images: grid, iovl, iden, tmap).
//! - ISO/IEC 23000-22 (MIAF) — major-brand selection.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

use crate::derived::{TransformChain, TransformOp};
use crate::iprp::{Amve, AuxC, Cclv, ColrInfo, Imir, Irot, Ispe, LayerSelector, Mdcv, Pixi};

/// A single property attached to a HEIF image item.
///
/// Each variant maps to one well-known box type inside `ipco`. The
/// writer collects all properties into the flat `ipco` array (with
/// de-duplication), then emits `ipma` rows referencing them by 1-based
/// index. See ISO/IEC 23008-12 §6.5 for the property catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeifProperty {
    /// `ispe` — image spatial extents (HEIF §6.5.3). Mandatory per spec.
    Ispe(Ispe),
    /// `pixi` — pixel information (HEIF §6.5.6). Channel-depth list.
    Pixi(Pixi),
    /// `colr` — colour information (HEIF §6.5.5 / ISO BMFF §12.1.5).
    Colr(ColrInfo),
    /// `auxC` — auxiliary type (HEIF §7.5.1). Identifies alpha/depth.
    AuxC(AuxC),
    /// `lsel` — layer selector for multi-layer coded items (§6.5.11).
    Lsel(LayerSelector),
    /// `irot` — 90° CCW rotation steps (HEIF §6.5.10).
    Irot(Irot),
    /// `imir` — mirror axis (HEIF §6.5.12).
    Imir(Imir),
    /// `clli` — Content Light Level Information (§6.5.5 / HDR).
    Clli(crate::iprp::Clli),
    /// `mdcv` — Mastering Display Colour Volume.
    Mdcv(Mdcv),
    /// `cclv` — Content Colour Volume.
    Cclv(Cclv),
    /// `amve` — Ambient Viewing Environment.
    Amve(Amve),
    /// `hvcC`, `av1C`, or any other codec-config blob the caller has
    /// already serialised. Surfaced through `Other` so the writer
    /// doesn't have to ship its own HEVC/AV1 config emitter.
    Other {
        /// 4-byte FourCC of the property box.
        fourcc: [u8; 4],
        /// Raw box body bytes (NOT including the 8-byte header).
        payload: Vec<u8>,
    },
}

impl HeifProperty {
    /// The 4-byte FourCC the variant emits to disk.
    pub fn fourcc(&self) -> [u8; 4] {
        match self {
            HeifProperty::Ispe(_) => *b"ispe",
            HeifProperty::Pixi(_) => *b"pixi",
            HeifProperty::Colr(_) => *b"colr",
            HeifProperty::AuxC(_) => *b"auxC",
            HeifProperty::Lsel(_) => *b"lsel",
            HeifProperty::Irot(_) => *b"irot",
            HeifProperty::Imir(_) => *b"imir",
            HeifProperty::Clli(_) => *b"clli",
            HeifProperty::Mdcv(_) => *b"mdcv",
            HeifProperty::Cclv(_) => *b"cclv",
            HeifProperty::Amve(_) => *b"amve",
            HeifProperty::Other { fourcc, .. } => *fourcc,
        }
    }

    /// Serialise the property body (NOT including the 8-byte header).
    fn emit_body(&self) -> Vec<u8> {
        match self {
            HeifProperty::Ispe(i) => {
                let mut p = Vec::with_capacity(12);
                p.extend_from_slice(&0u32.to_be_bytes()); // FullBox ver+flags
                p.extend_from_slice(&i.width.to_be_bytes());
                p.extend_from_slice(&i.height.to_be_bytes());
                p
            }
            HeifProperty::Pixi(x) => {
                let mut p = Vec::with_capacity(5 + x.bits_per_channel.len());
                p.extend_from_slice(&0u32.to_be_bytes()); // FullBox ver+flags
                p.push(x.bits_per_channel.len() as u8);
                p.extend_from_slice(&x.bits_per_channel);
                p
            }
            HeifProperty::Colr(c) => match c {
                ColrInfo::Nclx {
                    primaries,
                    transfer,
                    matrix,
                    full_range,
                } => {
                    let mut p = Vec::with_capacity(11);
                    p.extend_from_slice(b"nclx");
                    p.extend_from_slice(&primaries.to_be_bytes());
                    p.extend_from_slice(&transfer.to_be_bytes());
                    p.extend_from_slice(&matrix.to_be_bytes());
                    p.push(if *full_range { 0x80 } else { 0x00 });
                    p
                }
                ColrInfo::RestrictedIcc(bytes) => {
                    let mut p = Vec::with_capacity(4 + bytes.len());
                    p.extend_from_slice(b"rICC");
                    p.extend_from_slice(bytes);
                    p
                }
                ColrInfo::UnrestrictedIcc(bytes) => {
                    let mut p = Vec::with_capacity(4 + bytes.len());
                    p.extend_from_slice(b"prof");
                    p.extend_from_slice(bytes);
                    p
                }
            },
            HeifProperty::AuxC(a) => {
                // FullBox header + NUL-terminated UTF-8 URN + reserved subtype.
                let mut p = Vec::with_capacity(4 + a.aux_type.len() + 1 + a.aux_subtype.len());
                p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
                p.extend_from_slice(a.aux_type.as_bytes());
                p.push(0);
                p.extend_from_slice(&a.aux_subtype);
                p
            }
            HeifProperty::Lsel(l) => {
                // Bare 2-byte u16, no FullBox header (§6.5.11).
                l.layer_id.to_be_bytes().to_vec()
            }
            HeifProperty::Irot(r) => {
                vec![r.steps & 0x03]
            }
            HeifProperty::Imir(m) => {
                vec![m.axis & 0x01]
            }
            HeifProperty::Clli(c) => {
                // Bare 4-byte form (no FullBox prefix).
                let mut p = Vec::with_capacity(4);
                p.extend_from_slice(&c.max_content_light_level.to_be_bytes());
                p.extend_from_slice(&c.max_pic_average_light_level.to_be_bytes());
                p
            }
            HeifProperty::Mdcv(m) => {
                // Bare 24-byte form.
                let mut p = Vec::with_capacity(24);
                for c in 0..3 {
                    p.extend_from_slice(&m.display_primaries[c].0.to_be_bytes());
                }
                for c in 0..3 {
                    p.extend_from_slice(&m.display_primaries[c].1.to_be_bytes());
                }
                p.extend_from_slice(&m.white_point.0.to_be_bytes());
                p.extend_from_slice(&m.white_point.1.to_be_bytes());
                p.extend_from_slice(&m.max_display_luminance.to_be_bytes());
                p.extend_from_slice(&m.min_display_luminance.to_be_bytes());
                p
            }
            HeifProperty::Cclv(c) => {
                let mut p = Vec::with_capacity(32);
                p.extend_from_slice(&0u32.to_be_bytes()); // FullBox ver+flags
                let mut flags: u8 = 0;
                if c.cancel_flag {
                    flags |= 0x80;
                }
                if c.persistence_flag {
                    flags |= 0x40;
                }
                if c.primaries.is_some() {
                    flags |= 0x20;
                }
                if c.min_luminance.is_some() {
                    flags |= 0x10;
                }
                if c.max_luminance.is_some() {
                    flags |= 0x08;
                }
                if c.avg_luminance.is_some() {
                    flags |= 0x04;
                }
                p.push(flags);
                if let Some(prims) = c.primaries {
                    for (x, _) in prims.iter() {
                        p.extend_from_slice(&x.to_be_bytes());
                    }
                    for (_, y) in prims.iter() {
                        p.extend_from_slice(&y.to_be_bytes());
                    }
                }
                if let Some(v) = c.min_luminance {
                    p.extend_from_slice(&v.to_be_bytes());
                }
                if let Some(v) = c.max_luminance {
                    p.extend_from_slice(&v.to_be_bytes());
                }
                if let Some(v) = c.avg_luminance {
                    p.extend_from_slice(&v.to_be_bytes());
                }
                p
            }
            HeifProperty::Amve(a) => {
                // FullBox header + 8-byte body = 12-byte total.
                let mut p = Vec::with_capacity(12);
                p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
                p.extend_from_slice(&a.ambient_illuminance.to_be_bytes());
                p.extend_from_slice(&a.ambient_light_x.to_be_bytes());
                p.extend_from_slice(&a.ambient_light_y.to_be_bytes());
                p
            }
            HeifProperty::Other { payload, .. } => payload.clone(),
        }
    }

    /// Whether the property is essential per HEIF §7.4.6.6. Property
    /// kinds that MUST be set essential by spec (e.g. `ispe`, `hvcC`,
    /// `pixi`, `auxC`, derived-image transforms) return `true`; the rest
    /// (colr, HDR metadata, lsel) return `false`.
    fn default_essential(&self) -> bool {
        match self {
            HeifProperty::Ispe(_)
            | HeifProperty::Pixi(_)
            | HeifProperty::AuxC(_)
            | HeifProperty::Irot(_)
            | HeifProperty::Imir(_) => true,
            HeifProperty::Other { fourcc, .. } => {
                matches!(fourcc, b"hvcC" | b"av1C" | b"avcC" | b"mskC" | b"clap")
            }
            _ => false,
        }
    }
}

/// Derivation algorithm for a derived HEIF image item.
///
/// Derived items reference their component items via `dimg` `iref`
/// entries; the writer takes the source-item id list separately on
/// [`HeifItem::component_ids`] so the same `Derivation` variant can
/// be reused across calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeifDerivation {
    /// `grid` — rectangular tile grid (HEIF §6.6.2.3.1). Source items
    /// are the tile coded-image items, in row-major sweep order.
    Grid {
        /// Row count in [1, 256] (the on-disk `rows_minus_one + 1`).
        rows: u16,
        /// Column count in [1, 256].
        cols: u16,
        /// Output canvas width in pixels.
        output_width: u32,
        /// Output canvas height in pixels.
        output_height: u32,
    },
    /// `iovl` — image overlay (HEIF §6.6.2.3.2). Source items are the
    /// per-layer coded-image items, in stacking order.
    Overlay {
        /// 16-bit-per-channel RGBA canvas background.
        canvas_fill_color: [u16; 4],
        /// Output canvas width.
        output_width: u32,
        /// Output canvas height.
        output_height: u32,
        /// Per-layer `(h_offset, v_offset)` in signed pixels.
        offsets: Vec<(i32, i32)>,
    },
    /// `iden` — identity derivation. Source items are the single inner
    /// coded-image item being re-presented through the derived item's
    /// own property associations. The `iden` body is empty per spec.
    Identity,
    /// `tmap` — tone-mapping (HEIF Amendment 1 §6.6.x). `payload`
    /// carries the algorithm bytes verbatim. Source items are the
    /// base coded-image item(s).
    ToneMap {
        /// Algorithm payload bytes the renderer interprets per
        /// algorithm catalogue.
        payload: Vec<u8>,
    },
}

impl HeifDerivation {
    /// Item-type FourCC for this derivation.
    pub fn item_type(&self) -> [u8; 4] {
        match self {
            HeifDerivation::Grid { .. } => *b"grid",
            HeifDerivation::Overlay { .. } => *b"iovl",
            HeifDerivation::Identity => *b"iden",
            HeifDerivation::ToneMap { .. } => *b"tmap",
        }
    }

    /// Serialise the derivation body — what goes inside the item's
    /// idat slot (and hence what an `iloc` cm=1 extent points at).
    pub fn emit_body(&self) -> Vec<u8> {
        match self {
            HeifDerivation::Grid {
                rows,
                cols,
                output_width,
                output_height,
            } => {
                // 16-bit dims layout (flags bit 0 == 0).
                let large = *output_width > 0xFFFF || *output_height > 0xFFFF;
                let mut p = Vec::with_capacity(if large { 12 } else { 8 });
                p.push(0); // version
                p.push(if large { 0x01 } else { 0x00 }); // flags
                p.push((*rows as u8).saturating_sub(1));
                p.push((*cols as u8).saturating_sub(1));
                if large {
                    p.extend_from_slice(&output_width.to_be_bytes());
                    p.extend_from_slice(&output_height.to_be_bytes());
                } else {
                    p.extend_from_slice(&(*output_width as u16).to_be_bytes());
                    p.extend_from_slice(&(*output_height as u16).to_be_bytes());
                }
                p
            }
            HeifDerivation::Overlay {
                canvas_fill_color,
                output_width,
                output_height,
                offsets,
            } => {
                let large = *output_width > 0xFFFF
                    || *output_height > 0xFFFF
                    || offsets.iter().any(|(h, v)| {
                        !(-0x8000..=0x7FFF).contains(h) || !(-0x8000..=0x7FFF).contains(v)
                    });
                let mut p = Vec::new();
                p.push(0); // version
                p.push(if large { 0x01 } else { 0x00 }); // flags
                for c in canvas_fill_color.iter() {
                    p.extend_from_slice(&c.to_be_bytes());
                }
                if large {
                    p.extend_from_slice(&output_width.to_be_bytes());
                    p.extend_from_slice(&output_height.to_be_bytes());
                    for (h, v) in offsets {
                        p.extend_from_slice(&h.to_be_bytes());
                        p.extend_from_slice(&v.to_be_bytes());
                    }
                } else {
                    p.extend_from_slice(&(*output_width as u16).to_be_bytes());
                    p.extend_from_slice(&(*output_height as u16).to_be_bytes());
                    for (h, v) in offsets {
                        p.extend_from_slice(&(*h as i16).to_be_bytes());
                        p.extend_from_slice(&(*v as i16).to_be_bytes());
                    }
                }
                p
            }
            HeifDerivation::Identity => Vec::new(),
            HeifDerivation::ToneMap { payload } => payload.clone(),
        }
    }
}

/// One image item being written to a HEIF file.
///
/// Coded-image items carry their bitstream in [`Self::data`]; derived
/// items leave `data == None` and populate [`Self::derivation`] +
/// [`Self::component_ids`]. Every item carries its own property list +
/// optional name.
#[derive(Clone, Debug)]
pub struct HeifItem {
    /// 1-based item id used by `pitm` / `iinf` / `iloc` / `iref` /
    /// `ipma`. Must be unique across all items written.
    pub item_id: u32,
    /// 4-byte item_type FourCC (`hvc1`, `av01`, `jpeg`, `grid`,
    /// `iovl`, `iden`, `tmap`, `Exif`, `mime`, …).
    pub item_type: [u8; 4],
    /// UTF-8 item_name (HEIF: typically "primary", "thumbnail",
    /// "alpha", or empty). Empty string is fine.
    pub item_name: String,
    /// Coded-image payload bytes for `hvc1`/`av01`/`jpeg`/etc. Set to
    /// `None` for derived items (grid/iovl/iden/tmap) — their body
    /// comes from `derivation.emit_body()` and goes into `idat`.
    pub data: Option<Vec<u8>>,
    /// Per-item ordered property list. The writer collects all
    /// properties across all items into the `ipco` flat array (with
    /// structural-equality de-duplication) and emits one `ipma` row
    /// per item.
    pub properties: Vec<HeifProperty>,
    /// Optional derivation algorithm (set for grid/iovl/iden/tmap
    /// items). `None` for ordinary coded-image items.
    pub derivation: Option<HeifDerivation>,
    /// For derived items: the component item ids the `dimg` `iref`
    /// row should reference (in row-major / stacking / single-target
    /// order per algorithm). Empty for ordinary coded-image items.
    pub component_ids: Vec<u32>,
}

impl HeifItem {
    /// Build a coded-image item (HEVC / AV1 / JPEG) carrying `data`
    /// bytes and an `ispe` property by default — the writer enforces
    /// ispe presence per HEIF §6.5.3 by re-adding one if the caller
    /// supplied none. Other properties (colr/pixi/hvcC/…) must be
    /// added by the caller.
    pub fn coded(item_id: u32, item_type: [u8; 4], data: Vec<u8>) -> Self {
        Self {
            item_id,
            item_type,
            item_name: String::new(),
            data: Some(data),
            properties: Vec::new(),
            derivation: None,
            component_ids: Vec::new(),
        }
    }

    /// Build a derived-image item (grid / iovl / iden / tmap). The
    /// writer's `idat` carries the emitted derivation bytes; `iloc`
    /// records construction-method 1 referencing the `idat` slot.
    pub fn derived(item_id: u32, derivation: HeifDerivation, component_ids: Vec<u32>) -> Self {
        let item_type = derivation.item_type();
        Self {
            item_id,
            item_type,
            item_name: String::new(),
            data: None,
            properties: Vec::new(),
            derivation: Some(derivation),
            component_ids,
        }
    }

    /// Set the item name. Useful for `"primary"` / `"thumbnail"` /
    /// `"alpha"` annotations the HEIF authoring tools emit.
    pub fn with_name<S: Into<String>>(mut self, name: S) -> Self {
        self.item_name = name.into();
        self
    }

    /// Append a property to this item's per-item property list. The
    /// writer's `ipco` de-duplication step keeps one copy across all
    /// items with the same structural value.
    pub fn with_property(mut self, prop: HeifProperty) -> Self {
        self.properties.push(prop);
        self
    }

    /// Append a property chain (one call per `TransformOp`).
    pub fn with_transform_chain(mut self, chain: &TransformChain) -> Self {
        for op in chain {
            self.properties.push(match op {
                TransformOp::Irot { steps } => HeifProperty::Irot(Irot { steps: *steps }),
                TransformOp::Imir { axis } => HeifProperty::Imir(Imir { axis: *axis }),
                TransformOp::Clap(c) => HeifProperty::Other {
                    fourcc: *b"clap",
                    payload: emit_clap_body(c),
                },
            });
        }
        self
    }
}

fn emit_clap_body(c: &crate::media_meta::Clap) -> Vec<u8> {
    let mut p = Vec::with_capacity(32);
    p.extend_from_slice(&c.clean_aperture_width_n.to_be_bytes());
    p.extend_from_slice(&c.clean_aperture_width_d.to_be_bytes());
    p.extend_from_slice(&c.clean_aperture_height_n.to_be_bytes());
    p.extend_from_slice(&c.clean_aperture_height_d.to_be_bytes());
    p.extend_from_slice(&c.horiz_off_n.to_be_bytes());
    p.extend_from_slice(&c.horiz_off_d.to_be_bytes());
    p.extend_from_slice(&c.vert_off_n.to_be_bytes());
    p.extend_from_slice(&c.vert_off_d.to_be_bytes());
    p
}

/// One per-item `ipma` row: `(item_id, [(1-based property index in ipco,
/// essential flag)])`.
type IpmaRow = (u32, Vec<(u16, bool)>);

/// One `iloc` data-extent record entry: `(item_id, offset, length)`.
/// `offset` is relative to either `idat` (for cm=1) or `mdat`-absolute
/// (for cm=0) depending on which list it lives in.
type IlocExtent = (u32, u64, u64);

/// Typed-reference entry the writer emits inside the `iref` container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeifItemReference {
    /// 4-byte reference type — common kinds:
    /// * `b"dimg"` — derived-image source (auto-generated by the writer
    ///   for grid/iovl/iden/tmap items via their `component_ids`).
    /// * `b"auxl"` — auxiliary plane → primary image link (alpha/depth).
    /// * `b"thmb"` — thumbnail → master image link.
    /// * `b"cdsc"` — content-description (Exif / XMP → image).
    /// * `b"base"` — pre-derived base coded image (HEIF §6.4.7).
    pub kind: [u8; 4],
    /// Source item id.
    pub from_id: u32,
    /// One or more target item ids.
    pub to_ids: Vec<u32>,
}

/// Builder that emits an `.heif` / `.heic` file from a list of
/// [`HeifItem`]s + extra typed item references.
///
/// Use [`Self::new`] / [`Self::add_item`] / [`Self::add_reference`] /
/// [`Self::set_primary`] / [`Self::write_to_vec`].
#[derive(Clone, Debug, Default)]
pub struct HeifWriter {
    items: Vec<HeifItem>,
    /// Caller-supplied extra references (auxl / thmb / cdsc / base /
    /// custom). The writer also auto-generates `dimg` references for
    /// every derived item via that item's `component_ids` list.
    references: Vec<HeifItemReference>,
    /// Primary item id (`pitm`). Required.
    primary_item: Option<u32>,
    /// Major brand to put on `ftyp`. Defaults to `heic` per the
    /// HEIC profile (ISO/IEC 23008-12 §B.4.1).
    major_brand: [u8; 4],
    /// Compatible brands. Defaults to [`heic`, `mif1`, `heim`, `heis`].
    compatible_brands: Vec<[u8; 4]>,
}

impl HeifWriter {
    /// Construct an empty HEIF writer with HEIC default brands.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            references: Vec::new(),
            primary_item: None,
            major_brand: *b"heic",
            compatible_brands: vec![*b"mif1", *b"heic"],
        }
    }

    /// Override the major brand (`heic` → `mif1`, `avif`, `msf1`, …).
    pub fn with_major_brand(mut self, brand: [u8; 4]) -> Self {
        self.major_brand = brand;
        self
    }

    /// Replace the compatible-brands list.
    pub fn with_compatible_brands(mut self, brands: Vec<[u8; 4]>) -> Self {
        self.compatible_brands = brands;
        self
    }

    /// Append an image item. Item IDs MUST be unique; the writer
    /// validates this at [`Self::write_to_vec`] time.
    pub fn add_item(&mut self, item: HeifItem) -> &mut Self {
        self.items.push(item);
        self
    }

    /// Append a typed item reference (auxl / thmb / cdsc / base /
    /// custom kind). `dimg` references are auto-generated for derived
    /// items and don't need to be added here.
    pub fn add_reference(&mut self, reference: HeifItemReference) -> &mut Self {
        self.references.push(reference);
        self
    }

    /// Set the primary item id. REQUIRED.
    pub fn set_primary(&mut self, item_id: u32) -> &mut Self {
        self.primary_item = Some(item_id);
        self
    }

    /// Two-pass build: lay out the file in memory so `iloc` byte
    /// offsets are accurate, then return the result.
    ///
    /// Errors:
    /// * primary item not set, or refers to an unknown item id;
    /// * duplicate item ids;
    /// * derived item with empty `component_ids` for a derivation
    ///   that requires at least one source.
    pub fn write_to_vec(&self) -> Result<Vec<u8>> {
        self.validate()?;

        // Collect all unique properties across all items into the
        // flat ipco array; remember each item's per-property indices.
        let (flat_props, ipma_rows) = self.build_ipco_and_ipma();

        // Build idat body: every derived item's emitted body goes
        // here (construction-method 1). Coded-image items go to mdat
        // (construction-method 0).
        let mut idat_body: Vec<u8> = Vec::new();
        let mut idat_offsets: Vec<IlocExtent> = Vec::new();
        let mut mdat_body: Vec<u8> = Vec::new();
        let mut mdat_offsets: Vec<IlocExtent> = Vec::new();
        for it in &self.items {
            if let Some(d) = &it.derivation {
                let body = d.emit_body();
                let off = idat_body.len() as u64;
                let len = body.len() as u64;
                idat_body.extend_from_slice(&body);
                idat_offsets.push((it.item_id, off, len));
            } else if let Some(data) = &it.data {
                let off = mdat_body.len() as u64;
                let len = data.len() as u64;
                mdat_body.extend_from_slice(data);
                mdat_offsets.push((it.item_id, off, len));
            }
        }

        // Build the meta-box children that don't depend on absolute
        // mdat offsets (everything except iloc).
        let hdlr = build_hdlr_pict();
        let pitm = build_pitm_v0(self.primary_item.unwrap_or(0));
        let iinf = build_iinf_v0(&self.items);
        let iref = build_iref(&self.items, &self.references);
        let iprp = build_iprp(&flat_props, &ipma_rows);
        let idat_atom = build_idat_atom(&idat_body);

        // ── Two-pass layout: predict the iloc body length without
        // an mdat_offset, then place the meta box right after ftyp,
        // and compute the absolute mdat offset, then re-emit iloc.
        //
        // The iloc body length depends only on the chosen field
        // widths (offset_size, length_size, base_offset_size,
        // index_size) and the number of items + extents, so we can
        // compute it without knowing the absolute mdat offset yet.
        // Then iterate once: place meta after ftyp, compute the
        // post-meta cursor = ftyp_size + meta_size; the mdat payload
        // starts at `cursor + mdat_header_len` (mdat_header_len = 8
        // for u32 sizes, 16 for u64).
        //
        // For derived items (cm=1), the absolute mdat offset is not
        // needed; the iloc offset is into idat.

        let ftyp = self.build_ftyp();

        // Compute pre-mdat meta byte length with a dummy 0 mdat_offset.
        // iloc uses fixed-width fields (size 4 or 8 per choice) so the
        // length doesn't change when we re-render with the real offset.
        let need_64bit_offsets = mdat_offsets
            .iter()
            .any(|(_, _, len)| *len > u32::MAX as u64);
        // We'll just always use offset_size=8 for safety so we don't
        // have to predict whether the file crosses 4 GiB. Tiny extra
        // bytes (4 per item) — fine for an image format.
        let offset_size: u8 = 8;
        let length_size: u8 = if need_64bit_offsets { 8 } else { 4 };

        let iloc_dummy = build_iloc_v1(
            &self.items,
            &mdat_offsets,
            &idat_offsets,
            0, // dummy mdat absolute offset
            offset_size,
            length_size,
        );

        let meta_size_dummy =
            build_meta_size(&hdlr, &pitm, &iinf, &iref, &iprp, &idat_atom, &iloc_dummy);

        let ftyp_size = ftyp.len() as u64;
        let mdat_header_len: u64 = if (mdat_body.len() as u64 + 8) > u32::MAX as u64 {
            16
        } else {
            8
        };
        let mdat_absolute_offset = ftyp_size + meta_size_dummy + mdat_header_len;

        // Re-emit iloc with the real absolute offset.
        let iloc_final = build_iloc_v1(
            &self.items,
            &mdat_offsets,
            &idat_offsets,
            mdat_absolute_offset,
            offset_size,
            length_size,
        );

        // Sanity check — iloc body must have the same size as the dummy
        // (the field widths are fixed, so the length must match).
        debug_assert_eq!(iloc_dummy.len(), iloc_final.len(), "iloc resize bug");

        // ── Pass 2: emit bytes.
        let mut out = Vec::new();
        out.extend_from_slice(&ftyp);

        // Build full meta with the real iloc.
        let meta = build_meta(&hdlr, &pitm, &iinf, &iref, &iprp, &idat_atom, &iloc_final);
        push_atom(&mut out, *b"meta", &meta);

        // Emit mdat last.
        emit_mdat(&mut out, &mdat_body);

        Ok(out)
    }

    fn validate(&self) -> Result<()> {
        let primary = self
            .primary_item
            .ok_or_else(|| Error::invalid("HEIF writer: primary item not set"))?;
        // Unique item ids.
        let mut seen = std::collections::BTreeSet::new();
        for it in &self.items {
            if !seen.insert(it.item_id) {
                return Err(Error::invalid(format!(
                    "HEIF writer: duplicate item id {}",
                    it.item_id
                )));
            }
        }
        if !self.items.iter().any(|i| i.item_id == primary) {
            return Err(Error::invalid(format!(
                "HEIF writer: primary item {primary} not in item list"
            )));
        }
        for it in &self.items {
            if let Some(deriv) = &it.derivation {
                let needs_components = matches!(
                    deriv,
                    HeifDerivation::Grid { .. }
                        | HeifDerivation::Overlay { .. }
                        | HeifDerivation::Identity
                        | HeifDerivation::ToneMap { .. }
                );
                if needs_components && it.component_ids.is_empty() {
                    return Err(Error::invalid(format!(
                        "HEIF writer: derived item {} has no component_ids",
                        it.item_id
                    )));
                }
                // Validate component ids reference existing items.
                for cid in &it.component_ids {
                    if !self.items.iter().any(|i| i.item_id == *cid) {
                        return Err(Error::invalid(format!(
                            "HEIF writer: derived item {} component {} not in item list",
                            it.item_id, cid
                        )));
                    }
                }
            } else if it.data.is_none() {
                return Err(Error::invalid(format!(
                    "HEIF writer: coded item {} has no data + no derivation",
                    it.item_id
                )));
            }
        }
        Ok(())
    }

    /// Walk every item's per-item property list, collecting unique
    /// (structurally-equal) properties into the flat `ipco` array.
    /// Returns the flat-prop array + the per-item `ipma` row
    /// (item_id → list of `(1-based index, essential)`).
    fn build_ipco_and_ipma(&self) -> (Vec<HeifProperty>, Vec<IpmaRow>) {
        let mut flat: Vec<HeifProperty> = Vec::new();
        let mut rows: Vec<IpmaRow> = Vec::with_capacity(self.items.len());
        for it in &self.items {
            let mut assocs: Vec<(u16, bool)> = Vec::with_capacity(it.properties.len());
            for prop in &it.properties {
                let essential = prop.default_essential();
                // Find existing matching property.
                let idx = if let Some(pos) = flat.iter().position(|p| p == prop) {
                    pos
                } else {
                    flat.push(prop.clone());
                    flat.len() - 1
                };
                assocs.push(((idx + 1) as u16, essential));
            }
            if !assocs.is_empty() {
                rows.push((it.item_id, assocs));
            }
        }
        (flat, rows)
    }

    fn build_ftyp(&self) -> Vec<u8> {
        // body: major(4) + minor(4) + compatible_brands.
        let mut body = Vec::new();
        body.extend_from_slice(&self.major_brand);
        body.extend_from_slice(&0u32.to_be_bytes()); // minor_version
        for b in &self.compatible_brands {
            body.extend_from_slice(b);
        }
        let mut out = Vec::with_capacity(8 + body.len());
        push_atom(&mut out, *b"ftyp", &body);
        out
    }
}

// ───────────────────────── meta-child builders ─────────────────────────

fn build_hdlr_pict() -> Vec<u8> {
    // FullBox header + pre_defined(4) + handler_type(4) + reserved[3](12)
    // + name (cstr).
    let mut p = Vec::with_capacity(25);
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    p.extend_from_slice(b"pict");
    p.extend_from_slice(&[0u8; 12]); // reserved
    p.push(0); // empty name cstr
    p
}

fn build_pitm_v0(item_id: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(6);
    if item_id <= u16::MAX as u32 {
        p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
        p.extend_from_slice(&(item_id as u16).to_be_bytes());
    } else {
        // version=1 (u32 item_id)
        let mut header = [0u8; 4];
        header[0] = 1;
        p.extend_from_slice(&header);
        p.extend_from_slice(&item_id.to_be_bytes());
    }
    p
}

fn build_iinf_v0(items: &[HeifItem]) -> Vec<u8> {
    let mut iinf = Vec::new();
    iinf.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
    iinf.extend_from_slice(&(items.len() as u16).to_be_bytes());
    for it in items {
        let infe_body = build_infe_v2(it);
        push_atom(&mut iinf, *b"infe", &infe_body);
    }
    iinf
}

fn build_infe_v2(item: &HeifItem) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(2); // version = 2 (u16 item_id + item_type)
    p.extend_from_slice(&[0, 0, 0]); // flags
    p.extend_from_slice(&(item.item_id as u16).to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes()); // item_protection_index
    p.extend_from_slice(&item.item_type);
    p.extend_from_slice(item.item_name.as_bytes());
    p.push(0); // NUL-terminator for item_name
               // For non-mime/uri item_types, no further fields.
    p
}

fn build_iref(items: &[HeifItem], extra_refs: &[HeifItemReference]) -> Vec<u8> {
    // Combine auto-generated dimg refs (for derived items) + extra refs.
    let mut all: Vec<HeifItemReference> = Vec::new();
    for it in items {
        if it.derivation.is_some() && !it.component_ids.is_empty() {
            all.push(HeifItemReference {
                kind: *b"dimg",
                from_id: it.item_id,
                to_ids: it.component_ids.clone(),
            });
        }
    }
    all.extend_from_slice(extra_refs);
    if all.is_empty() {
        return Vec::new();
    }
    let mut iref = Vec::new();
    iref.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
    for r in &all {
        // Each child box body: from_id u16 + ref_count u16 + N × to_id u16.
        let mut child_body = Vec::with_capacity(4 + 2 * r.to_ids.len());
        child_body.extend_from_slice(&(r.from_id as u16).to_be_bytes());
        child_body.extend_from_slice(&(r.to_ids.len() as u16).to_be_bytes());
        for tid in &r.to_ids {
            child_body.extend_from_slice(&(*tid as u16).to_be_bytes());
        }
        push_atom(&mut iref, r.kind, &child_body);
    }
    iref
}

fn build_iprp(flat_props: &[HeifProperty], ipma_rows: &[IpmaRow]) -> Vec<u8> {
    let mut iprp = Vec::new();
    // ipco — flat property array.
    let mut ipco = Vec::new();
    for p in flat_props {
        push_atom(&mut ipco, p.fourcc(), &p.emit_body());
    }
    push_atom(&mut iprp, *b"ipco", &ipco);

    // ipma — per-item rows. Choose v0 (8-bit indices) when every
    // index fits in 7 bits; v1 (16-bit indices, flags bit 0 set)
    // otherwise.
    let max_index = ipma_rows
        .iter()
        .flat_map(|(_, assocs)| assocs.iter().map(|(idx, _)| *idx))
        .max()
        .unwrap_or(0);
    let large_index = max_index > 0x7F;
    // Also use v1 (u32 item_ids) when any item id exceeds u16 range.
    let large_ids = ipma_rows.iter().any(|(id, _)| *id > u16::MAX as u32);
    let version: u8 = if large_ids { 1 } else { 0 };
    let flags: u32 = if large_index { 0x01 } else { 0x00 };

    let mut ipma = Vec::new();
    let mut hdr = [0u8; 4];
    hdr[0] = version;
    hdr[1..4].copy_from_slice(&flags.to_be_bytes()[1..4]);
    ipma.extend_from_slice(&hdr);
    ipma.extend_from_slice(&(ipma_rows.len() as u32).to_be_bytes());
    for (item_id, assocs) in ipma_rows {
        if version == 0 {
            ipma.extend_from_slice(&(*item_id as u16).to_be_bytes());
        } else {
            ipma.extend_from_slice(&item_id.to_be_bytes());
        }
        ipma.push(assocs.len() as u8);
        for (idx, essential) in assocs {
            if large_index {
                let mut raw = *idx & 0x7FFF;
                if *essential {
                    raw |= 0x8000;
                }
                ipma.extend_from_slice(&raw.to_be_bytes());
            } else {
                let mut byte = (*idx as u8) & 0x7F;
                if *essential {
                    byte |= 0x80;
                }
                ipma.push(byte);
            }
        }
    }
    push_atom(&mut iprp, *b"ipma", &ipma);
    iprp
}

fn build_idat_atom(idat_body: &[u8]) -> Vec<u8> {
    // idat is NOT a FullBox — just raw bytes.
    idat_body.to_vec()
}

/// Build a v1 `iloc` body. Always uses offset_size & length_size as
/// requested (the writer picks them based on whether mdat extents
/// exceed u32). `base_offset_size = 0`, `index_size = 0`.
fn build_iloc_v1(
    items: &[HeifItem],
    mdat_offsets: &[IlocExtent],
    idat_offsets: &[IlocExtent],
    mdat_absolute_offset: u64,
    offset_size: u8,
    length_size: u8,
) -> Vec<u8> {
    let mut p = Vec::new();
    // FullBox header: version=1, flags=0.
    let mut hdr = [0u8; 4];
    hdr[0] = 1;
    p.extend_from_slice(&hdr);
    // packed: (offset_size << 4) | length_size
    p.push((offset_size << 4) | length_size);
    // packed2: (base_offset_size << 4) | index_size
    // v1: index_size in low nibble. We don't use base/index, so 0.
    p.push(0x00);
    // item_count (u16 for v1).
    p.extend_from_slice(&(items.len() as u16).to_be_bytes());

    for it in items {
        // item_ID (u16 for v1; v2 would use u32).
        p.extend_from_slice(&(it.item_id as u16).to_be_bytes());
        // construction_method (u16 with low 4 bits used) — v1 field.
        let cm: u16 = if it.derivation.is_some() { 1 } else { 0 };
        p.extend_from_slice(&cm.to_be_bytes());
        // data_reference_index (u16) — always 0 (same file).
        p.extend_from_slice(&0u16.to_be_bytes());
        // base_offset (size base_offset_size) — none.
        // extent_count (u16).
        p.extend_from_slice(&1u16.to_be_bytes());
        // Per-extent: extent_index (index_size; absent here)
        //           + extent_offset (offset_size)
        //           + extent_length (length_size).
        let (off, len) = if it.derivation.is_some() {
            // cm=1 (idat): offset is into the idat box body.
            let row = idat_offsets
                .iter()
                .find(|(id, _, _)| *id == it.item_id)
                .copied()
                .unwrap_or((it.item_id, 0, 0));
            (row.1, row.2)
        } else {
            // cm=0 (file_offset): absolute file offset into mdat.
            let row = mdat_offsets
                .iter()
                .find(|(id, _, _)| *id == it.item_id)
                .copied()
                .unwrap_or((it.item_id, 0, 0));
            (mdat_absolute_offset + row.1, row.2)
        };
        write_uint(&mut p, off, offset_size);
        write_uint(&mut p, len, length_size);
    }
    p
}

fn write_uint(out: &mut Vec<u8>, value: u64, size: u8) {
    match size {
        4 => out.extend_from_slice(&(value as u32).to_be_bytes()),
        8 => out.extend_from_slice(&value.to_be_bytes()),
        _ => unreachable!("unsupported iloc width"),
    }
}

fn build_meta(
    hdlr: &[u8],
    pitm: &[u8],
    iinf: &[u8],
    iref: &[u8],
    iprp: &[u8],
    idat_body: &[u8],
    iloc: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // FullBox ver+flags
    push_atom(&mut body, *b"hdlr", hdlr);
    push_atom(&mut body, *b"pitm", pitm);
    push_atom(&mut body, *b"iinf", iinf);
    if !iref.is_empty() {
        push_atom(&mut body, *b"iref", iref);
    }
    push_atom(&mut body, *b"iprp", iprp);
    push_atom(&mut body, *b"iloc", iloc);
    if !idat_body.is_empty() {
        push_atom(&mut body, *b"idat", idat_body);
    }
    body
}

/// Same as [`build_meta`] but only returns the resulting outer atom
/// size (8 + body length) — used during the layout-sizing pass.
fn build_meta_size(
    hdlr: &[u8],
    pitm: &[u8],
    iinf: &[u8],
    iref: &[u8],
    iprp: &[u8],
    idat_body: &[u8],
    iloc: &[u8],
) -> u64 {
    let mut body_len = 4u64; // FullBox ver+flags
    body_len += 8 + hdlr.len() as u64;
    body_len += 8 + pitm.len() as u64;
    body_len += 8 + iinf.len() as u64;
    if !iref.is_empty() {
        body_len += 8 + iref.len() as u64;
    }
    body_len += 8 + iprp.len() as u64;
    body_len += 8 + iloc.len() as u64;
    if !idat_body.is_empty() {
        body_len += 8 + idat_body.len() as u64;
    }
    8 + body_len
}

fn emit_mdat(out: &mut Vec<u8>, body: &[u8]) {
    let total = 8u64 + body.len() as u64;
    if total > u32::MAX as u64 {
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(b"mdat");
        let extended = 16u64 + body.len() as u64;
        out.extend_from_slice(&extended.to_be_bytes());
    } else {
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(b"mdat");
    }
    out.extend_from_slice(body);
}

fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
    let size: u32 = (8 + body.len()) as u32;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&fourcc);
    out.extend_from_slice(body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmff_meta::{parse_bmff_meta, ItemDataLocation};
    use crate::iprp::{ColrInfo, Ispe, Pixi};
    use std::io::Cursor;

    /// Walk the file's `meta` box and return the parsed `BmffMeta`.
    fn parse_meta(bytes: &[u8]) -> crate::BmffMeta {
        use crate::atom::read_atom_header;
        let mut c = Cursor::new(bytes);
        loop {
            let hdr = read_atom_header(&mut c).unwrap().unwrap();
            if &hdr.fourcc == b"meta" {
                return parse_bmff_meta(&mut c, &hdr).unwrap().unwrap();
            }
            c.set_position(hdr.payload_offset + hdr.payload_len().unwrap());
        }
    }

    #[test]
    fn write_single_item_roundtrips() {
        let mut w = HeifWriter::new();
        let item = HeifItem::coded(1, *b"hvc1", b"FAKE_HEVC_PAYLOAD".to_vec())
            .with_property(HeifProperty::Ispe(Ispe {
                width: 64,
                height: 64,
            }))
            .with_property(HeifProperty::Pixi(Pixi {
                bits_per_channel: vec![8, 8, 8],
            }))
            .with_property(HeifProperty::Colr(ColrInfo::Nclx {
                primaries: 1,
                transfer: 13,
                matrix: 6,
                full_range: true,
            }));
        w.add_item(item).set_primary(1);
        let bytes = w.write_to_vec().unwrap();

        let meta = parse_meta(&bytes);
        assert_eq!(&meta.handler_type, b"pict");
        assert_eq!(meta.primary_item, Some(1));
        assert_eq!(meta.items.len(), 1);
        assert_eq!(&meta.items[0].item_type, b"hvc1");
        assert_eq!(meta.items[0].item_id, 1);

        // iloc → bytes resolve correctly to the original payload.
        match crate::bmff_meta::item_data(&meta, 1).unwrap() {
            ItemDataLocation::FileExtents(extents) => {
                assert_eq!(extents.len(), 1);
                let (off, len) = extents[0];
                assert_eq!(len as usize, b"FAKE_HEVC_PAYLOAD".len());
                let actual = &bytes[off as usize..(off + len) as usize];
                assert_eq!(actual, b"FAKE_HEVC_PAYLOAD");
            }
            other => panic!("expected FileExtents, got {other:?}"),
        }

        let props = meta.properties.as_ref().unwrap();
        let ispe = props.ispe_for(1).unwrap();
        assert_eq!(ispe.width, 64);
        assert_eq!(ispe.height, 64);
        let pixi = props.pixi_for(1).unwrap();
        assert_eq!(pixi.bits_per_channel, vec![8, 8, 8]);
        let colr = props.color_profile(1).unwrap();
        match colr {
            ColrInfo::Nclx {
                primaries,
                transfer,
                matrix,
                full_range,
            } => {
                assert_eq!(primaries, 1);
                assert_eq!(transfer, 13);
                assert_eq!(matrix, 6);
                assert!(full_range);
            }
            other => panic!("expected Nclx, got {other:?}"),
        }
    }

    #[test]
    fn write_grid_with_two_tiles_roundtrips() {
        let mut w = HeifWriter::new();
        // Two tile items + one grid derived.
        w.add_item(
            HeifItem::coded(1, *b"hvc1", b"TILE_A".to_vec())
                .with_property(HeifProperty::Ispe(Ispe {
                    width: 32,
                    height: 32,
                }))
                .with_property(HeifProperty::Pixi(Pixi {
                    bits_per_channel: vec![8, 8, 8],
                })),
        );
        w.add_item(
            HeifItem::coded(2, *b"hvc1", b"TILE_B".to_vec())
                .with_property(HeifProperty::Ispe(Ispe {
                    width: 32,
                    height: 32,
                }))
                .with_property(HeifProperty::Pixi(Pixi {
                    bits_per_channel: vec![8, 8, 8],
                })),
        );
        w.add_item(
            HeifItem::derived(
                3,
                HeifDerivation::Grid {
                    rows: 1,
                    cols: 2,
                    output_width: 64,
                    output_height: 32,
                },
                vec![1, 2],
            )
            .with_property(HeifProperty::Ispe(Ispe {
                width: 64,
                height: 32,
            })),
        );
        w.set_primary(3);
        let bytes = w.write_to_vec().unwrap();
        let meta = parse_meta(&bytes);
        assert_eq!(meta.primary_item, Some(3));
        assert_eq!(meta.items.len(), 3);
        // dimg ref auto-generated.
        let dimg = meta.derived_from(3);
        assert_eq!(dimg, vec![1, 2]);

        // Tile A resolves to "TILE_A".
        match crate::bmff_meta::item_data(&meta, 1).unwrap() {
            ItemDataLocation::FileExtents(ext) => {
                let (off, len) = ext[0];
                assert_eq!(&bytes[off as usize..(off + len) as usize], b"TILE_A");
            }
            _ => panic!(),
        }
        // Tile B → "TILE_B".
        match crate::bmff_meta::item_data(&meta, 2).unwrap() {
            ItemDataLocation::FileExtents(ext) => {
                let (off, len) = ext[0];
                assert_eq!(&bytes[off as usize..(off + len) as usize], b"TILE_B");
            }
            _ => panic!(),
        }
        // Grid item resolves to idat — parse it back.
        match crate::bmff_meta::item_data(&meta, 3).unwrap() {
            ItemDataLocation::Idat(body) => {
                let g = crate::derived::parse_grid(&body).unwrap();
                assert_eq!(g.rows, 1);
                assert_eq!(g.cols, 2);
                assert_eq!(g.output_width, 64);
                assert_eq!(g.output_height, 32);
            }
            _ => panic!(),
        }
    }
}
