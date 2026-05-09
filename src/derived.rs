//! HEIF derived-image payloads (`grid`, `iovl`).
//!
//! HEIF (ISO/IEC 23008-12 §6.6.2.3) introduces "derived" image items
//! whose payloads are tiny fixed-format records describing how a
//! viewer should reconstruct a final image from one or more
//! contributing source items. Two derivation kinds are common in the
//! corpus:
//!
//! * `grid` — a row-major mosaic of equally-sized tiles. The 16-byte
//!   payload (8 bytes when `flags & 1 == 0`) carries `(rows, cols,
//!   output_width, output_height)`. Tile order matches the targets of
//!   the sibling `dimg` `iref` from the grid item.
//! * `iovl` — an arbitrary-stack overlay. The payload carries a 4×u16
//!   RGBA canvas fill, the output canvas dimensions, and one signed
//!   `(h_offset, v_offset)` pair per source image (matching the
//!   `dimg` target order).
//!
//! Both bodies are typically stored inline in `idat`
//! (`construction_method == 1`); some authoring tools place them in
//! `mdat` instead. The caller picks the right [`BmffMeta`] resolver
//! ([`crate::idat_bytes_for_item`] vs [`crate::file_extents_for_item`])
//! and feeds the resulting bytes to [`parse_grid`] / [`parse_overlay`].
//!
//! On-disk layout per ISO/IEC 23008-12 §6.6.2.3.1 (grid):
//!
//! ```text
//! version            u8         (must be 0)
//! flags              u8         (bit 0: dimensions are 32-bit, else 16-bit)
//! rows_minus_one     u8
//! cols_minus_one     u8
//! output_width       u16 or u32 (per flags bit 0)
//! output_height      u16 or u32
//! ```
//!
//! On-disk layout per ISO/IEC 23008-12 §6.6.2.3.2 (overlay):
//!
//! ```text
//! version            u8         (must be 0)
//! flags              u8         (bit 0: 32-bit dims+offsets, else 16-bit)
//! canvas_fill[4]     u16 each   (R, G, B, A — 16-bit per channel)
//! output_width       u16 or u32
//! output_height      u16 or u32
//! per source (in dimg target order):
//!     h_offset       i16 or i32
//!     v_offset       i16 or i32
//! ```
//!
//! The `iovl` parser uses the matching `dimg` target list (looked up
//! through [`BmffMeta::derived_from`]) to know how many `(h, v)` pairs
//! to consume; callers can supply that count explicitly via
//! [`parse_overlay_with_source_count`] when they want to validate a
//! payload against the file's own iref topology.

use crate::bmff_meta::{idat_bytes_concat, BmffMeta, ItemDataLocation};

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Decoded `grid` derived-image payload (ISO/IEC 23008-12 §6.6.2.3.1).
///
/// `rows`/`cols` are the actual count (already adjusted from the
/// on-disk `*_minus_one` form). `output_width`/`output_height` give
/// the rendered canvas size; tile sizes are *not* declared by the
/// payload itself — the renderer derives them from the contributing
/// items' `ispe` properties.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Grid {
    /// Row count in [1, 256]. (`rows_minus_one + 1`.)
    pub rows: u16,
    /// Column count in [1, 256]. (`cols_minus_one + 1`.)
    pub cols: u16,
    /// Output canvas width in pixels.
    pub output_width: u32,
    /// Output canvas height in pixels.
    pub output_height: u32,
}

/// Decoded `iovl` derived-image payload (ISO/IEC 23008-12 §6.6.2.3.2).
///
/// `canvas_fill_color` is the 4×u16 RGBA fill used for any pixel of
/// the output canvas not covered by a layer. `offsets` carries one
/// signed `(h_offset, v_offset)` pair per source image — the order
/// matches the targets of the `dimg` reference *from* the overlay
/// item (see [`crate::BmffMeta::derived_from`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    /// 16-bit-per-channel RGBA background colour.
    pub canvas_fill_color: [u16; 4],
    /// Output canvas width in pixels.
    pub output_width: u32,
    /// Output canvas height in pixels.
    pub output_height: u32,
    /// Per-layer `(h_offset, v_offset)` in signed pixels, in `dimg`
    /// target order. Negative values are valid — a layer can hang off
    /// the canvas edges and is clipped by the renderer.
    pub offsets: Vec<(i32, i32)>,
}

/// Parse a `grid` derived-image payload (HEIF §6.6.2.3.1). The body is
/// 8 bytes (16-bit dimensions) or 12 bytes (32-bit dimensions) — both
/// are valid per the spec; flag bit 0 selects between them. The 16-bit
/// shape is what authoring tools emit in practice; the 32-bit shape is
/// reserved for output canvases > 65535 px.
///
/// Returns `Err(InvalidData)` when:
///
/// * the payload is shorter than the version+flags+rows+cols header,
/// * the version byte is non-zero (the spec forbids forward-compat
///   bumps without a re-mint of the FourCC),
/// * the body is shorter than the dimension fields select.
pub fn parse_grid(body: &[u8]) -> Result<Grid> {
    if body.len() < 4 {
        return Err(Error::invalid("HEIF: grid payload < 4 bytes"));
    }
    let version = body[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "HEIF: grid version {version} not supported (spec mandates 0)"
        )));
    }
    let flags = body[1];
    let large = flags & 0x01 != 0;
    let rows = body[2] as u16 + 1;
    let cols = body[3] as u16 + 1;
    let (output_width, output_height) = if large {
        if body.len() < 12 {
            return Err(Error::invalid("HEIF: grid 32-bit body < 12 bytes"));
        }
        let w = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
        let h = u32::from_be_bytes([body[8], body[9], body[10], body[11]]);
        (w, h)
    } else {
        if body.len() < 8 {
            return Err(Error::invalid("HEIF: grid 16-bit body < 8 bytes"));
        }
        let w = u16::from_be_bytes([body[4], body[5]]) as u32;
        let h = u16::from_be_bytes([body[6], body[7]]) as u32;
        (w, h)
    };
    Ok(Grid {
        rows,
        cols,
        output_width,
        output_height,
    })
}

/// Parse an `iovl` overlay derived-image payload (HEIF §6.6.2.3.2)
/// when the caller already knows how many layers (= `dimg` targets)
/// to expect. The header (version=0, flags, 4×u16 canvas fill,
/// output_width/height) is fixed; the per-layer offsets are sized by
/// the flag bit and counted by `source_count`.
///
/// `source_count` MUST equal the number of `dimg` targets the iref
/// declares for the overlay item — when in doubt, look it up via
/// [`crate::BmffMeta::derived_from`] before calling this. We don't
/// reach back into the meta from here on purpose (the same parser
/// also services hand-built fixtures and tests where iref isn't
/// involved).
pub fn parse_overlay_with_source_count(body: &[u8], source_count: usize) -> Result<Overlay> {
    if body.len() < 12 {
        return Err(Error::invalid("HEIF: iovl payload < 12 bytes"));
    }
    let version = body[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "HEIF: iovl version {version} not supported (spec mandates 0)"
        )));
    }
    let flags = body[1];
    let large = flags & 0x01 != 0;
    let mut canvas_fill_color = [0u16; 4];
    for (i, c) in canvas_fill_color.iter_mut().enumerate() {
        let off = 2 + 2 * i;
        *c = u16::from_be_bytes([body[off], body[off + 1]]);
    }
    let mut p = 10usize;
    let (output_width, output_height) = if large {
        if body.len() < p + 8 {
            return Err(Error::invalid("HEIF: iovl 32-bit dims truncated"));
        }
        let w = u32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
        let h = u32::from_be_bytes([body[p + 4], body[p + 5], body[p + 6], body[p + 7]]);
        p += 8;
        (w, h)
    } else {
        if body.len() < p + 4 {
            return Err(Error::invalid("HEIF: iovl 16-bit dims truncated"));
        }
        let w = u16::from_be_bytes([body[p], body[p + 1]]) as u32;
        let h = u16::from_be_bytes([body[p + 2], body[p + 3]]) as u32;
        p += 4;
        (w, h)
    };
    let stride = if large { 8 } else { 4 };
    let needed = source_count.checked_mul(stride).ok_or_else(|| {
        Error::invalid("HEIF: iovl source_count × stride overflow (rejecting payload)")
    })?;
    if body.len() < p + needed {
        return Err(Error::invalid(format!(
            "HEIF: iovl offsets truncated (need {needed} bytes for {source_count} layers, have {})",
            body.len() - p
        )));
    }
    let mut offsets = Vec::with_capacity(source_count);
    for _ in 0..source_count {
        let (h, v) = if large {
            let h = i32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
            let v = i32::from_be_bytes([body[p + 4], body[p + 5], body[p + 6], body[p + 7]]);
            p += 8;
            (h, v)
        } else {
            let h = i16::from_be_bytes([body[p], body[p + 1]]) as i32;
            let v = i16::from_be_bytes([body[p + 2], body[p + 3]]) as i32;
            p += 4;
            (h, v)
        };
        offsets.push((h, v));
    }
    Ok(Overlay {
        canvas_fill_color,
        output_width,
        output_height,
        offsets,
    })
}

/// Parse an `iovl` overlay payload, inferring the layer count from
/// the body's residual length. Useful when the caller doesn't yet
/// know the iref topology (e.g. when validating a hand-rolled
/// fixture). The inferred count is the largest `n` such that the
/// header + `n` offset records exactly fits the body; an offset
/// stride that doesn't divide the residual cleanly is rejected as
/// `InvalidData`.
pub fn parse_overlay(body: &[u8]) -> Result<Overlay> {
    if body.len() < 12 {
        return Err(Error::invalid("HEIF: iovl payload < 12 bytes"));
    }
    let flags = body[1];
    let large = flags & 0x01 != 0;
    // Header size: 1 ver + 1 flags + 4×u16 fill + 2×{u16|u32} dims.
    let hdr_len = 2 + 8 + if large { 8 } else { 4 };
    let stride = if large { 8 } else { 4 };
    if body.len() < hdr_len {
        return Err(Error::invalid("HEIF: iovl header truncated"));
    }
    let residual = body.len() - hdr_len;
    if residual % stride != 0 {
        return Err(Error::invalid(format!(
            "HEIF: iovl trailer length {residual} is not a multiple of {stride} (cannot infer layer count)"
        )));
    }
    parse_overlay_with_source_count(body, residual / stride)
}

/// Per-tile slot inside an [`ImageGridLayout`].
///
/// `(item_id, x, y, w, h)`: the contributing tile item plus the
/// top-left canvas coordinate and the *actual* per-tile encoded extent
/// the file's `iprp/ipma` declares for it. Per HEIF §6.6.2.3.3 every
/// tile in a grid MUST share the same `(w, h)` (== the canonical
/// [`ImageGridLayout::tile_w`] / [`ImageGridLayout::tile_h`]); when an
/// authoring tool emits a malformed file with mismatched per-tile
/// `ispe`, the deviation surfaces in [`ImageGridLayout::tile_size_warnings`]
/// and the per-slot `(w, h)` carries the *file-declared* extent so the
/// caller can decide between using the canonical stride and trusting
/// the malformed `ispe`.
///
/// The order matches the row-major sweep of the `dimg` reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridTilePlacement {
    /// Item id of the tile (target of the grid item's `dimg` iref).
    pub item_id: u32,
    /// X coordinate of the tile's top-left, in canvas pixels.
    pub x: u32,
    /// Y coordinate of the tile's top-left, in canvas pixels.
    pub y: u32,
    /// Tile width in pixels as declared by *this* tile's `ispe`. Equal
    /// to [`ImageGridLayout::tile_w`] for a spec-compliant grid; can
    /// differ on a malformed file (in which case the discrepancy is
    /// reported via [`ImageGridLayout::tile_size_warnings`]). Falls
    /// back to the canonical `tile_w` when the tile carries no `ispe`.
    pub w: u32,
    /// Tile height in pixels as declared by *this* tile's `ispe`.
    /// Mirror of [`Self::w`] for the height axis.
    pub h: u32,
}

/// One mismatched-`ispe` entry surfaced by [`ImageGridLayout::tile_size_warnings`]
/// or [`OverlayLayout::layer_size_warnings`].
///
/// HEIF §6.6.2.3.3 mandates that every tile in a `grid` derived image
/// share the same encoded extent, and §6.6.2.2.3 mandates the same
/// thing for an `iovl` overlay's per-layer canvas: the layer's `ispe`
/// width/height define how many pixels the layer contributes
/// to the canvas at its `(x, y)` origin. A file that violates either
/// rule is malformed; we surface the offending item id + the canonical
/// `(w, h)` we expected versus what the file's `ispe` actually
/// declares, so the renderer can choose between trusting the canonical
/// stride and accepting the malformed extent.
///
/// We DON'T fail the plan on a mismatch: tolerant readers (most viewer
/// pipelines) prefer to render with the canonical stride and let the
/// per-tile decode produce whatever pixels the bitstream actually
/// holds; strict pipelines (e.g. validators) can iterate
/// [`ImageGridLayout::tile_size_warnings`] and reject the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IspeMismatch {
    /// Item id of the offending tile / layer.
    pub item_id: u32,
    /// Canonical `(width, height)` — for grids, this is the first
    /// tile's `ispe` (the planner's `tile_w` / `tile_h`); for overlays,
    /// it's whatever the layer's spec-mandated extent is (currently the
    /// raw layer `ispe` is the only source-of-truth, so we surface the
    /// per-layer ispe verbatim and a mismatch only fires when the same
    /// layer item appears with two conflicting `ispe` records).
    pub expected_w: u32,
    /// Canonical height; pairs with [`Self::expected_w`].
    pub expected_h: u32,
    /// File-declared `(width, height)` for this item — what the file's
    /// own `ispe` association actually says. `(0, 0)` when the item has
    /// no `ispe` association at all (a separate failure mode the spec
    /// also forbids; we surface it as a mismatch with zero-extent so
    /// callers see a single unified warning channel).
    pub actual_w: u32,
    /// Mirror of [`Self::actual_w`] for the height axis.
    pub actual_h: u32,
}

/// HEIF `grid` composition plan.
///
/// Pure metadata — no decoded pixel data is involved. The renderer
/// (e.g. `oxideav-h265`) decodes each `tiles[i].item_id` into an
/// `Rgba8Canvas` and then blits it at `(tiles[i].x, tiles[i].y)` to
/// reconstruct the canvas. The plan trusts the file's own `ispe`
/// associations: `tile_w` / `tile_h` are read from the first tile
/// item's `ispe`. Every subsequent tile's `ispe` is then validated
/// against `(tile_w, tile_h)`; mismatches are surfaced through
/// [`Self::tile_size_warnings`] so callers that want strict validation
/// can opt in without us failing the plan unilaterally (the renderer's
/// existing `render_grid` enforces the rule on the decoded-buffer
/// side, so a tolerant viewer pipeline can still reach a final canvas
/// when individual tiles land at the canonical stride).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageGridLayout {
    /// Output canvas width in pixels (from the `grid` payload).
    pub canvas_w: u32,
    /// Output canvas height in pixels (from the `grid` payload).
    pub canvas_h: u32,
    /// Tile width in pixels (from the first tile's `ispe`).
    pub tile_w: u32,
    /// Tile height in pixels (from the first tile's `ispe`).
    pub tile_h: u32,
    /// Row count (from the `grid` payload's `rows_minus_one + 1`).
    pub rows: u16,
    /// Column count (from the `grid` payload's `cols_minus_one + 1`).
    pub cols: u16,
    /// Per-tile placements in row-major sweep order. Length is
    /// `rows × cols`.
    pub tiles: Vec<GridTilePlacement>,
    /// One entry per tile whose `ispe` doesn't equal the canonical
    /// `(tile_w, tile_h)` — empty for a spec-compliant file. See
    /// [`IspeMismatch`] for the semantic.
    pub tile_size_warnings: Vec<IspeMismatch>,
}

/// One layer entry in an [`OverlayLayout`].
///
/// The `(w, h)` carry the layer's encoded extent as declared by its
/// `iprp/ipma`-bound `ispe` property. Per HEIF §6.6.2.2 a layer's
/// `(x, y, w, h)` rectangle is what the compositor blends into the
/// canvas; a layer whose `ispe` is missing falls back to `(0, 0)`
/// extents (and surfaces in [`OverlayLayout::layer_size_warnings`] so
/// the caller can detect the under-described case).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayLayer {
    /// Item id of the source layer (target of the overlay item's
    /// `dimg` iref).
    pub item_id: u32,
    /// Signed horizontal pixel offset of the layer's top-left from
    /// the canvas top-left. Matches the `iovl` payload's `h_offset`
    /// field width (16-bit when `flags & 1 == 0`, 32-bit otherwise),
    /// promoted to `i32` for caller convenience.
    pub x: i32,
    /// Signed vertical pixel offset of the layer's top-left from
    /// the canvas top-left.
    pub y: i32,
    /// Layer width in pixels, from the layer item's `ispe`. `0` when
    /// the layer has no `ispe` association (the warning is surfaced
    /// in [`OverlayLayout::layer_size_warnings`]).
    pub w: u32,
    /// Layer height in pixels, from the layer item's `ispe`. Mirror
    /// of [`Self::w`] for the height axis.
    pub h: u32,
}

/// HEIF `iovl` composition plan.
///
/// Layers appear in `dimg` target order (== iref-declared stacking
/// order, bottom-most first). Each layer's `(x, y)` is the on-canvas
/// origin of the layer's top-left — negative values are valid (the
/// layer overhangs the top/left and the renderer clips).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayLayout {
    /// Output canvas width in pixels (from the `iovl` payload).
    pub canvas_w: u32,
    /// Output canvas height in pixels (from the `iovl` payload).
    pub canvas_h: u32,
    /// 16-bits-per-channel RGBA canvas fill, surfaced verbatim from
    /// the `iovl` payload so callers can apply their own colour-space
    /// conversion (or downcast to 8 bpp like the existing
    /// [`crate::render_iovl`] renderer does).
    pub canvas_fill_color: [u16; 4],
    /// Layer list in `dimg` target order.
    pub layers: Vec<OverlayLayer>,
    /// One entry per layer item that has no `ispe` association — the
    /// HEIF authoring guidance is to always associate `ispe` with each
    /// item that contributes pixels to a derivation. Per-layer w/h is
    /// `(0, 0)` for any item listed here. Empty for a spec-compliant
    /// file. (We don't surface "two layers had different `ispe`" as a
    /// warning the way the grid path does, because each iovl layer is
    /// an independent rectangle — there's no canonical extent the
    /// other layers should match.)
    pub layer_size_warnings: Vec<IspeMismatch>,
}

/// Resolved composition plan for a HEIF primary image.
///
/// Returned by [`crate::MovDemuxer::primary_image_layout`] when the
/// file's `pitm` points at an `iden` / `iovl` / `grid` derived item.
/// The variant tells the caller how to combine the tile / layer items
/// into the final canvas; the variant-internal fields carry the
/// composition coordinates so the caller can decode each contributing
/// item once and place it at the right pixel-space location.
///
/// HEIF leaves the actual pixel composition to the application layer:
/// our renderer (`render_grid` / `render_iovl` / `render_iden`) takes
/// pre-decoded `Rgba8Canvas` buffers, and the layout helper here only
/// computes *where* each one goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageLayout {
    /// Primary item is itself a coded image (`hvc1` / `av01` / `j2k1`
    /// / etc.) — no derivation, just decode `item_id` and you have
    /// the picture.
    Identity { item_id: u32 },
    /// Primary item is a `grid` derived image. Tile items live in
    /// `layout.tiles` in row-major order; decode each one and blit at
    /// `(x, y)`.
    Grid(ImageGridLayout),
    /// Primary item is an `iovl` overlay derived image. Decode each
    /// layer item and composite over the canvas in the order
    /// `layout.layers` lists.
    Overlay(OverlayLayout),
}

/// Build an [`ImageGridLayout`] from an already-resolved `grid`
/// payload — use this when the caller has resolved the derivation
/// bytes themselves (e.g. via [`crate::file_extents_for_item`] +
/// `Read + Seek` for `construction_method == 0` items, where the
/// payload lives in `mdat` rather than `idat`).
///
/// Mirrors [`plan_grid_layout`] in every aspect except the byte
/// resolver, so the two paths produce equal layouts for equal
/// payloads. See [`plan_grid_layout`] for the validation contract.
///
/// Surfaces per-tile `ispe` mismatches in
/// [`ImageGridLayout::tile_size_warnings`] (does not fail the plan;
/// strict callers iterate the warnings vec).
pub fn build_grid_layout(
    meta: &BmffMeta,
    item_id: u32,
    payload_bytes: &[u8],
) -> Result<ImageGridLayout> {
    let grid = parse_grid(payload_bytes)?;
    let dimg_targets = meta.derived_from(item_id);
    let expected = (grid.rows as usize)
        .checked_mul(grid.cols as usize)
        .ok_or_else(|| Error::invalid("HEIF grid plan: rows × cols overflow"))?;
    if dimg_targets.len() != expected {
        return Err(Error::invalid(format!(
            "HEIF grid plan: dimg target count {} != rows({})×cols({}) = {expected}",
            dimg_targets.len(),
            grid.rows,
            grid.cols,
        )));
    }
    let props = meta.properties.as_ref().ok_or_else(|| {
        Error::invalid("HEIF grid plan: file has no iprp; cannot infer tile dimensions")
    })?;
    let first_tile = *dimg_targets.first().ok_or_else(|| {
        Error::invalid("HEIF grid plan: dimg target list is empty (need at least one tile)")
    })?;
    let first_ispe = props.ispe_for(first_tile).ok_or_else(|| {
        Error::invalid(format!(
            "HEIF grid plan: first tile item {first_tile} has no ispe association"
        ))
    })?;
    if first_ispe.width == 0 || first_ispe.height == 0 {
        return Err(Error::invalid(
            "HEIF grid plan: first tile ispe has zero dimensions",
        ));
    }
    let tile_w = first_ispe.width;
    let tile_h = first_ispe.height;
    let mut tiles = Vec::with_capacity(expected);
    let mut tile_size_warnings = Vec::new();
    for (idx, &tile_id) in dimg_targets.iter().enumerate() {
        let row = (idx as u32) / (grid.cols as u32);
        let col = (idx as u32) % (grid.cols as u32);
        let x = col
            .checked_mul(tile_w)
            .ok_or_else(|| Error::invalid("HEIF grid plan: tile x overflow"))?;
        let y = row
            .checked_mul(tile_h)
            .ok_or_else(|| Error::invalid("HEIF grid plan: tile y overflow"))?;
        // Per-tile ispe lookup. Missing ispe is treated as
        // (0, 0) for the warning channel; the per-slot extent
        // falls back to the canonical (tile_w, tile_h) so the
        // tolerant render path works without special-casing.
        let (slot_w, slot_h) = match props.ispe_for(tile_id) {
            Some(this) => {
                if this.width != tile_w || this.height != tile_h {
                    tile_size_warnings.push(IspeMismatch {
                        item_id: tile_id,
                        expected_w: tile_w,
                        expected_h: tile_h,
                        actual_w: this.width,
                        actual_h: this.height,
                    });
                }
                (this.width, this.height)
            }
            None => {
                tile_size_warnings.push(IspeMismatch {
                    item_id: tile_id,
                    expected_w: tile_w,
                    expected_h: tile_h,
                    actual_w: 0,
                    actual_h: 0,
                });
                (tile_w, tile_h)
            }
        };
        tiles.push(GridTilePlacement {
            item_id: tile_id,
            x,
            y,
            w: slot_w,
            h: slot_h,
        });
    }
    Ok(ImageGridLayout {
        canvas_w: grid.output_width,
        canvas_h: grid.output_height,
        tile_w,
        tile_h,
        rows: grid.rows,
        cols: grid.cols,
        tiles,
        tile_size_warnings,
    })
}

/// Build an [`ImageGridLayout`] for an `item_id` whose `infe.item_type
/// == 'grid'` and whose `idat` carries a [`Grid`] payload.
///
/// Lookups: the contributing tile-item ids come from
/// `BmffMeta::derived_from(item_id)` (= the targets of the grid
/// item's `dimg` iref). Tile dimensions come from the first tile's
/// `ispe` property — every tile must share that extent per HEIF
/// §6.6.2.3.3, and any per-tile `ispe` that disagrees is surfaced via
/// [`ImageGridLayout::tile_size_warnings`].
///
/// Returns `Err(InvalidData)` when:
///
/// * the item has no `idat`-resident grid payload (callers wanting the
///   `construction_method == 0` / mdat path should resolve the bytes
///   themselves and call [`build_grid_layout`] directly, or use
///   [`crate::MovDemuxer::primary_image_layout`] which dispatches both
///   construction methods automatically);
/// * the `dimg` reference list disagrees with `rows × cols`;
/// * the first tile has no `ispe` association (so we can't infer
///   tile dimensions).
///
/// The variants `tile_w` / `tile_h` are filled from the first tile's
/// `ispe`; `tiles[i].x` / `tiles[i].y` are computed from the tile's
/// row/column index using these dimensions; `tiles[i].w` / `tiles[i].h`
/// carry the per-tile-declared `ispe` extent (== canonical for spec-
/// compliant files).
pub fn plan_grid_layout(meta: &BmffMeta, item_id: u32) -> Result<ImageGridLayout> {
    let raw = idat_bytes_concat(meta, item_id).ok_or_else(|| {
        Error::invalid(format!(
            "HEIF grid plan: item {item_id} has no idat-resident payload"
        ))
    })?;
    build_grid_layout(meta, item_id, &raw)
}

/// Build an [`OverlayLayout`] from an already-resolved `iovl` payload.
/// Companion to [`build_grid_layout`] for the overlay code-path.
///
/// Each layer's `(w, h)` comes from the layer item's `iprp/ipma`-bound
/// `ispe`; layers without an `ispe` association land in
/// [`OverlayLayout::layer_size_warnings`] with `(0, 0)` extents (the
/// authoring-spec violation HEIF §6.5.3 forbids).
pub fn build_overlay_layout(
    meta: &BmffMeta,
    item_id: u32,
    payload_bytes: &[u8],
) -> Result<OverlayLayout> {
    let dimg_targets = meta.derived_from(item_id);
    let overlay = parse_overlay_with_source_count(payload_bytes, dimg_targets.len())?;
    if overlay.offsets.len() != dimg_targets.len() {
        return Err(Error::invalid(format!(
            "HEIF iovl plan: offsets count {} != dimg target count {}",
            overlay.offsets.len(),
            dimg_targets.len(),
        )));
    }
    let mut layers = Vec::with_capacity(dimg_targets.len());
    let mut layer_size_warnings = Vec::new();
    for (id, (x, y)) in dimg_targets.iter().zip(overlay.offsets.iter()) {
        let (w, h) = match meta.properties.as_ref().and_then(|p| p.ispe_for(*id)) {
            Some(i) => (i.width, i.height),
            None => {
                layer_size_warnings.push(IspeMismatch {
                    item_id: *id,
                    expected_w: 0,
                    expected_h: 0,
                    actual_w: 0,
                    actual_h: 0,
                });
                (0, 0)
            }
        };
        layers.push(OverlayLayer {
            item_id: *id,
            x: *x,
            y: *y,
            w,
            h,
        });
    }
    Ok(OverlayLayout {
        canvas_w: overlay.output_width,
        canvas_h: overlay.output_height,
        canvas_fill_color: overlay.canvas_fill_color,
        layers,
        layer_size_warnings,
    })
}

/// Build an [`OverlayLayout`] for an `item_id` whose `infe.item_type
/// == 'iovl'` and whose `idat` carries an [`Overlay`] payload.
///
/// The contributing layer-item ids come from
/// `BmffMeta::derived_from(item_id)` (= the targets of the overlay
/// item's `dimg` iref). Each layer's `(x, y)` comes from the parsed
/// [`Overlay::offsets`] in the same index order; each layer's `(w, h)`
/// comes from the layer item's `iprp/ipma`-bound `ispe`.
///
/// Returns `Err(InvalidData)` when:
///
/// * the item has no `idat`-resident overlay payload (mdat-resident
///   payloads are dispatched through
///   [`crate::MovDemuxer::primary_image_layout`] or by calling
///   [`build_overlay_layout`] directly with hand-resolved bytes);
/// * the `dimg` target count and the parsed offsets count disagree
///   (HEIF §6.6.2.2.3 mandates one offset pair per layer).
pub fn plan_overlay_layout(meta: &BmffMeta, item_id: u32) -> Result<OverlayLayout> {
    let raw = idat_bytes_concat(meta, item_id).ok_or_else(|| {
        Error::invalid(format!(
            "HEIF iovl plan: item {item_id} has no idat-resident payload"
        ))
    })?;
    build_overlay_layout(meta, item_id, &raw)
}

/// Resolve the file's primary image into an [`ImageLayout`] composition
/// plan. The discriminator is the primary item's `infe.item_type`:
///
/// * `grid` → [`ImageLayout::Grid`] (parsed from the item's `idat` and
///   the `dimg` iref)
/// * `iovl` → [`ImageLayout::Overlay`] (parsed from the item's `idat`
///   and the `dimg` iref)
/// * `iden` → [`ImageLayout::Identity`] with the *target* of the
///   identity item's `dimg` (per HEIF §6.6.2.1: an `iden` item is a
///   trivial pass-through to its single `dimg` source).
/// * any other coded `item_type` (`hvc1`, `av01`, `j2k1`, …) →
///   [`ImageLayout::Identity { item_id = primary_item_id }`].
///
/// Returns `None` when the file has no `pitm`, when the `pitm` points
/// at an item with no `iinf` row, or when the primary item is a `grid`
/// / `iovl` whose plan can't be built (e.g. `idat` missing — the
/// caller can fall back to `BmffMeta::find_item` + `item_data` to
/// inspect the bytes themselves). On a tolerant parse the helper
/// silently degrades to `None`.
///
/// `iden` is treated as a pass-through: we surface the inner image
/// item id rather than the `iden` item itself, because the caller
/// usually wants the tile-sized canvas the inner item carries (the
/// `iden` derivation only adds transformative properties on top, which
/// the caller applies via [`crate::render_iden`] after decoding).
pub fn primary_image_layout_for(meta: &BmffMeta) -> Option<ImageLayout> {
    let pid = meta.primary_item?;
    image_layout_for(meta, pid)
}

/// Same as [`primary_image_layout_for`] but for an arbitrary item.
/// Useful when the caller drives layout for a non-primary derived
/// image (e.g. a thumbnail's grid).
pub fn image_layout_for(meta: &BmffMeta, item_id: u32) -> Option<ImageLayout> {
    let info = meta.find_item(item_id)?;
    match &info.item_type {
        b"grid" => match plan_grid_layout(meta, item_id) {
            Ok(g) => Some(ImageLayout::Grid(g)),
            Err(_) => None,
        },
        b"iovl" => match plan_overlay_layout(meta, item_id) {
            Ok(o) => Some(ImageLayout::Overlay(o)),
            Err(_) => None,
        },
        b"iden" => {
            // Per §6.6.2.1: an iden item has exactly one dimg source;
            // the rendered output is that source with the iden item's
            // own transformative properties applied.
            let targets = meta.derived_from(item_id);
            targets
                .first()
                .map(|id| ImageLayout::Identity { item_id: *id })
        }
        // Any other item_type (hvc1, av01, j2k1, …) is a coded image
        // taken as-is. v0/v1 infe rows have a zero item_type — also
        // surface them as Identity so legacy HEIF authoring lands on
        // the obvious decode-then-show path.
        _ => Some(ImageLayout::Identity { item_id }),
    }
}

/// Convenience wrapper to look up an item's [`ItemDataLocation`] from
/// inside the `derived` planner — re-exported so test code doesn't
/// have to import the full `bmff_meta` surface to check that grid /
/// iovl payloads are reachable. Returns the same value as
/// [`crate::item_data`] would.
///
/// (Kept as a thin shim because the planner tests need it; library
/// callers should reach for [`crate::item_data`] directly.)
#[allow(dead_code)]
fn item_location_kind(meta: &BmffMeta, item_id: u32) -> Option<ItemDataLocation> {
    crate::bmff_meta::item_data(meta, item_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid16(rows_minus_one: u8, cols_minus_one: u8, w: u16, h: u16) -> Vec<u8> {
        let mut p = vec![
            0u8, /*ver*/
            0,   /*flags=16-bit dims*/
            rows_minus_one,
            cols_minus_one,
        ];
        p.extend_from_slice(&w.to_be_bytes());
        p.extend_from_slice(&h.to_be_bytes());
        p
    }

    fn grid32(rows_minus_one: u8, cols_minus_one: u8, w: u32, h: u32) -> Vec<u8> {
        let mut p = vec![
            0u8, /*ver*/
            1,   /*flags=32-bit dims*/
            rows_minus_one,
            cols_minus_one,
        ];
        p.extend_from_slice(&w.to_be_bytes());
        p.extend_from_slice(&h.to_be_bytes());
        p
    }

    fn overlay16(fill: [u16; 4], w: u16, h: u16, layers: &[(i16, i16)]) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(0); // version
        p.push(0); // flags = 16-bit
        for c in fill {
            p.extend_from_slice(&c.to_be_bytes());
        }
        p.extend_from_slice(&w.to_be_bytes());
        p.extend_from_slice(&h.to_be_bytes());
        for (hh, vv) in layers {
            p.extend_from_slice(&hh.to_be_bytes());
            p.extend_from_slice(&vv.to_be_bytes());
        }
        p
    }

    #[test]
    fn grid_16bit_2x2_256x256_matches_corpus() {
        // The corpus `still-image-grid-2x2` fixture: rows=2, cols=2,
        // output_dims=256x256.
        let g = parse_grid(&grid16(1, 1, 256, 256)).unwrap();
        assert_eq!(g.rows, 2);
        assert_eq!(g.cols, 2);
        assert_eq!(g.output_width, 256);
        assert_eq!(g.output_height, 256);
    }

    #[test]
    fn grid_32bit_dims() {
        let g = parse_grid(&grid32(0, 0, 70_000, 5)).unwrap();
        assert_eq!(g.rows, 1);
        assert_eq!(g.cols, 1);
        assert_eq!(g.output_width, 70_000);
        assert_eq!(g.output_height, 5);
    }

    #[test]
    fn grid_unknown_version_rejected() {
        let mut p = grid16(1, 1, 256, 256);
        p[0] = 1; // version=1 is reserved
        assert!(parse_grid(&p).is_err());
    }

    #[test]
    fn grid_too_short_rejected() {
        // Header says 16-bit dims (8 bytes) but body is 5.
        let mut p = vec![0u8, 0, 0, 0];
        p.push(0);
        assert!(parse_grid(&p).is_err());
    }

    #[test]
    fn grid_32bit_too_short_rejected() {
        // Flags say 32-bit dims (12 bytes) but body has only 8.
        let p = vec![0u8, 1, 0, 0, 0, 0, 0, 0];
        assert!(parse_grid(&p).is_err());
    }

    #[test]
    fn overlay_16bit_corpus_shape() {
        // The corpus `still-image-overlay` fixture: fill=(16384,
        // 16384, 16384, 65535), 2 layers (base + stamp at (96, 96)),
        // output 256x256.
        let body = overlay16([16384, 16384, 16384, 65535], 256, 256, &[(0, 0), (96, 96)]);
        let o = parse_overlay(&body).unwrap();
        assert_eq!(o.canvas_fill_color, [16384, 16384, 16384, 65535]);
        assert_eq!(o.output_width, 256);
        assert_eq!(o.output_height, 256);
        assert_eq!(o.offsets, vec![(0, 0), (96, 96)]);
    }

    #[test]
    fn overlay_explicit_source_count_negative_offset() {
        let body = overlay16([0; 4], 8, 8, &[(-3, 4)]);
        let o = parse_overlay_with_source_count(&body, 1).unwrap();
        assert_eq!(o.offsets, vec![(-3, 4)]);
    }

    #[test]
    fn overlay_inferred_layer_count_misaligned_rejected() {
        // 16-bit overlay header is 14 bytes; trailing 5 bytes can't be
        // divided into 4-byte stride records, so inference must fail.
        let mut body = overlay16([0; 4], 1, 1, &[(0, 0)]);
        body.push(0xAA); // junk byte
        assert!(parse_overlay(&body).is_err());
    }

    #[test]
    fn overlay_unknown_version_rejected() {
        let mut body = overlay16([0; 4], 1, 1, &[]);
        body[0] = 1;
        assert!(parse_overlay(&body).is_err());
    }

    #[test]
    fn overlay_truncated_offsets_rejected() {
        // Declare 3 layers but only carry 1 worth of bytes.
        let body = overlay16([0; 4], 1, 1, &[(0, 0)]);
        assert!(parse_overlay_with_source_count(&body, 3).is_err());
    }

    #[test]
    fn overlay_32bit_dims_round_trip() {
        let mut body = Vec::new();
        body.push(0); // ver
        body.push(1); // flags = 32-bit
        for c in [10u16, 20, 30, 40] {
            body.extend_from_slice(&c.to_be_bytes());
        }
        body.extend_from_slice(&100_000u32.to_be_bytes());
        body.extend_from_slice(&50_000u32.to_be_bytes());
        // one layer at (-1000, 1000)
        body.extend_from_slice(&(-1000i32).to_be_bytes());
        body.extend_from_slice(&1000i32.to_be_bytes());
        let o = parse_overlay(&body).unwrap();
        assert_eq!(o.canvas_fill_color, [10, 20, 30, 40]);
        assert_eq!(o.output_width, 100_000);
        assert_eq!(o.output_height, 50_000);
        assert_eq!(o.offsets, vec![(-1000, 1000)]);
    }

    // ─── round-11 layout planners (in-memory BmffMeta fixtures) ───

    use crate::bmff_meta::{ItemExtent, ItemInfoEntry, ItemLocation, ItemReference};
    use crate::iprp::{
        Ispe, ItemProperties, ItemProperty, ItemPropertyAssociation, PropertyAssociation,
    };

    #[allow(clippy::too_many_arguments)]
    fn make_grid_meta(
        primary_id: u32,
        rows_minus_one: u8,
        cols_minus_one: u8,
        out_w: u16,
        out_h: u16,
        tile_w: u32,
        tile_h: u32,
        tile_ids: &[u32],
    ) -> BmffMeta {
        // Build the grid payload (16-bit dims) and embed it inline in idat.
        let payload = grid16(rows_minus_one, cols_minus_one, out_w, out_h);
        let payload_len = payload.len() as u64;
        // ipco: a single Ispe property the tiles share.
        let ispe_prop = ItemProperty::Ispe(Ispe {
            width: tile_w,
            height: tile_h,
        });
        let mut associations = Vec::new();
        for &tid in tile_ids {
            associations.push(ItemPropertyAssociation {
                item_id: tid,
                associations: vec![PropertyAssociation {
                    index: 1,
                    essential: true,
                }],
            });
        }
        let properties = ItemProperties {
            properties: vec![ispe_prop],
            associations,
        };
        // iinf: grid item + N tile items
        let mut items = Vec::new();
        items.push(ItemInfoEntry {
            item_id: primary_id,
            item_type: *b"grid",
            ..Default::default()
        });
        for &tid in tile_ids {
            items.push(ItemInfoEntry {
                item_id: tid,
                item_type: *b"hvc1",
                ..Default::default()
            });
        }
        // iloc: only the grid item is in idat for this fixture; tiles
        // would also have iloc rows in a real file but we don't need
        // them for the planner.
        let locations = vec![ItemLocation {
            item_id: primary_id,
            construction_method: 1,
            data_reference_index: 0,
            base_offset: 0,
            extents: vec![ItemExtent {
                index: 0,
                offset: 0,
                length: payload_len,
            }],
        }];
        // dimg iref: from grid item → tile items in raster order.
        let references = vec![ItemReference {
            kind: *b"dimg",
            from_item_id: primary_id,
            to_item_ids: tile_ids.to_vec(),
        }];
        BmffMeta {
            handler_type: *b"pict",
            primary_item: Some(primary_id),
            items,
            locations,
            idat: payload,
            xml: String::new(),
            bxml: Vec::new(),
            references,
            properties: Some(properties),
            data_references: Vec::new(),
        }
    }

    #[test]
    fn plan_grid_2x2_64x64_lays_out_tiles_at_corners() {
        // 2×2 grid of 64×64 tiles → 128×128 canvas, tile slots at
        // (0,0), (64,0), (0,64), (64,64).
        let tile_ids = vec![10, 11, 12, 13];
        let meta = make_grid_meta(2, 1, 1, 128, 128, 64, 64, &tile_ids);
        let plan = plan_grid_layout(&meta, 2).unwrap();
        assert_eq!(plan.canvas_w, 128);
        assert_eq!(plan.canvas_h, 128);
        assert_eq!(plan.tile_w, 64);
        assert_eq!(plan.tile_h, 64);
        assert_eq!(plan.rows, 2);
        assert_eq!(plan.cols, 2);
        assert_eq!(plan.tiles.len(), 4);
        assert_eq!(
            plan.tiles[0],
            GridTilePlacement {
                item_id: 10,
                x: 0,
                y: 0,
                w: 64,
                h: 64,
            }
        );
        assert_eq!(
            plan.tiles[1],
            GridTilePlacement {
                item_id: 11,
                x: 64,
                y: 0,
                w: 64,
                h: 64,
            }
        );
        assert_eq!(
            plan.tiles[2],
            GridTilePlacement {
                item_id: 12,
                x: 0,
                y: 64,
                w: 64,
                h: 64,
            }
        );
        assert_eq!(
            plan.tiles[3],
            GridTilePlacement {
                item_id: 13,
                x: 64,
                y: 64,
                w: 64,
                h: 64,
            }
        );
        // Spec-compliant grid: no per-tile ispe mismatches.
        assert!(plan.tile_size_warnings.is_empty());
    }

    #[test]
    fn plan_grid_dimg_count_mismatch_rejected() {
        // grid says 2×2 = 4 tiles but only 3 dimg targets supplied.
        let tile_ids = vec![10, 11, 12];
        let meta = make_grid_meta(2, 1, 1, 128, 128, 64, 64, &tile_ids);
        assert!(plan_grid_layout(&meta, 2).is_err());
    }

    #[test]
    fn plan_grid_missing_ispe_rejected() {
        // Build a fixture with empty iprp so the first tile has no ispe.
        let mut meta = make_grid_meta(2, 0, 0, 64, 64, 64, 64, &[10]);
        meta.properties = Some(ItemProperties::default());
        assert!(plan_grid_layout(&meta, 2).is_err());
    }

    fn make_iovl_meta(
        primary_id: u32,
        canvas: (u16, u16),
        fill: [u16; 4],
        layers: &[(u32, i16, i16)],
    ) -> BmffMeta {
        // Build the iovl payload (16-bit shape, 16-bit dims & offsets).
        let mut payload = Vec::new();
        payload.push(0); // version
        payload.push(0); // flags = 16-bit
        for c in fill {
            payload.extend_from_slice(&c.to_be_bytes());
        }
        payload.extend_from_slice(&canvas.0.to_be_bytes());
        payload.extend_from_slice(&canvas.1.to_be_bytes());
        for (_, x, y) in layers {
            payload.extend_from_slice(&x.to_be_bytes());
            payload.extend_from_slice(&y.to_be_bytes());
        }
        let payload_len = payload.len() as u64;
        let layer_ids: Vec<u32> = layers.iter().map(|(id, _, _)| *id).collect();
        let mut items = Vec::new();
        items.push(ItemInfoEntry {
            item_id: primary_id,
            item_type: *b"iovl",
            ..Default::default()
        });
        for &lid in &layer_ids {
            items.push(ItemInfoEntry {
                item_id: lid,
                item_type: *b"hvc1",
                ..Default::default()
            });
        }
        let locations = vec![ItemLocation {
            item_id: primary_id,
            construction_method: 1,
            data_reference_index: 0,
            base_offset: 0,
            extents: vec![ItemExtent {
                index: 0,
                offset: 0,
                length: payload_len,
            }],
        }];
        let references = vec![ItemReference {
            kind: *b"dimg",
            from_item_id: primary_id,
            to_item_ids: layer_ids,
        }];
        BmffMeta {
            handler_type: *b"pict",
            primary_item: Some(primary_id),
            items,
            locations,
            idat: payload,
            xml: String::new(),
            bxml: Vec::new(),
            references,
            properties: None,
            data_references: Vec::new(),
        }
    }

    #[test]
    fn plan_overlay_3_layers_keeps_dimg_order_and_offsets() {
        let layers = [(20u32, 0i16, 0i16), (21, 50, 50), (22, -10, 100)];
        let meta = make_iovl_meta(2, (256, 256), [0, 0, 0, 65535], &layers);
        let plan = plan_overlay_layout(&meta, 2).unwrap();
        assert_eq!(plan.canvas_w, 256);
        assert_eq!(plan.canvas_h, 256);
        assert_eq!(plan.canvas_fill_color, [0, 0, 0, 65535]);
        assert_eq!(plan.layers.len(), 3);
        // No iprp / ispe in the make_iovl_meta fixture → every layer
        // surfaces a (0, 0)-extent warning (per HEIF §6.5.3 every
        // layer should carry an `ispe`; we don't fail the plan, just
        // surface the omission).
        assert_eq!(plan.layer_size_warnings.len(), 3);
        assert_eq!(
            plan.layers[0],
            OverlayLayer {
                item_id: 20,
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            }
        );
        assert_eq!(
            plan.layers[1],
            OverlayLayer {
                item_id: 21,
                x: 50,
                y: 50,
                w: 0,
                h: 0,
            }
        );
        assert_eq!(
            plan.layers[2],
            OverlayLayer {
                item_id: 22,
                x: -10,
                y: 100,
                w: 0,
                h: 0,
            }
        );
    }

    #[test]
    fn primary_image_layout_dispatches_grid() {
        let tile_ids = vec![10, 11, 12, 13];
        let meta = make_grid_meta(2, 1, 1, 128, 128, 64, 64, &tile_ids);
        match primary_image_layout_for(&meta) {
            Some(ImageLayout::Grid(g)) => {
                assert_eq!(g.tiles.len(), 4);
                assert_eq!(g.canvas_w, 128);
            }
            other => panic!("expected Grid, got {other:?}"),
        }
    }

    #[test]
    fn primary_image_layout_dispatches_overlay() {
        let layers = [(20u32, 0i16, 0i16), (21, 96, 96)];
        let meta = make_iovl_meta(2, (256, 256), [16384, 16384, 16384, 65535], &layers);
        match primary_image_layout_for(&meta) {
            Some(ImageLayout::Overlay(o)) => {
                assert_eq!(o.layers.len(), 2);
                assert_eq!(o.canvas_fill_color, [16384, 16384, 16384, 65535]);
            }
            other => panic!("expected Overlay, got {other:?}"),
        }
    }

    #[test]
    fn primary_image_layout_dispatches_iden_to_inner_target() {
        // iden item 7 with one dimg target -> item 9 (an hvc1).
        let meta = BmffMeta {
            handler_type: *b"pict",
            primary_item: Some(7),
            items: vec![
                ItemInfoEntry {
                    item_id: 7,
                    item_type: *b"iden",
                    ..Default::default()
                },
                ItemInfoEntry {
                    item_id: 9,
                    item_type: *b"hvc1",
                    ..Default::default()
                },
            ],
            locations: Vec::new(),
            idat: Vec::new(),
            xml: String::new(),
            bxml: Vec::new(),
            references: vec![ItemReference {
                kind: *b"dimg",
                from_item_id: 7,
                to_item_ids: vec![9],
            }],
            properties: None,
            data_references: Vec::new(),
        };
        match primary_image_layout_for(&meta) {
            Some(ImageLayout::Identity { item_id }) => assert_eq!(item_id, 9),
            other => panic!("expected Identity, got {other:?}"),
        }
    }

    #[test]
    fn primary_image_layout_dispatches_coded_image_to_identity() {
        // Plain hvc1 primary item — no derivation, surfaces as
        // Identity with the primary item id verbatim.
        let meta = BmffMeta {
            handler_type: *b"pict",
            primary_item: Some(5),
            items: vec![ItemInfoEntry {
                item_id: 5,
                item_type: *b"hvc1",
                ..Default::default()
            }],
            ..Default::default()
        };
        match primary_image_layout_for(&meta) {
            Some(ImageLayout::Identity { item_id }) => assert_eq!(item_id, 5),
            other => panic!("expected Identity, got {other:?}"),
        }
    }

    #[test]
    fn primary_image_layout_returns_none_when_no_pitm() {
        let meta = BmffMeta::default();
        assert!(primary_image_layout_for(&meta).is_none());
    }

    // ─── round-12: per-tile / per-layer ispe validation ───

    /// Build a grid fixture where one tile has a *different* ispe from
    /// the others. The shared base `iprp` carries TWO ispe properties:
    /// one (idx 1) at the canonical `(tile_w, tile_h)`, and a second
    /// (idx 2) at `(deviant_w, deviant_h)`. The deviant tile is
    /// associated to property idx 2 instead of idx 1.
    #[allow(clippy::too_many_arguments)]
    fn make_grid_meta_with_deviant_tile(
        primary_id: u32,
        rows_minus_one: u8,
        cols_minus_one: u8,
        out_w: u16,
        out_h: u16,
        tile_w: u32,
        tile_h: u32,
        tile_ids: &[u32],
        deviant_idx: usize,
        deviant_w: u32,
        deviant_h: u32,
    ) -> BmffMeta {
        let payload = grid16(rows_minus_one, cols_minus_one, out_w, out_h);
        let payload_len = payload.len() as u64;
        // ipco: two ispe properties (canonical at idx 1, deviant at idx 2).
        let canonical = ItemProperty::Ispe(Ispe {
            width: tile_w,
            height: tile_h,
        });
        let deviant = ItemProperty::Ispe(Ispe {
            width: deviant_w,
            height: deviant_h,
        });
        let mut associations = Vec::new();
        for (i, &tid) in tile_ids.iter().enumerate() {
            let idx = if i == deviant_idx { 2 } else { 1 };
            associations.push(ItemPropertyAssociation {
                item_id: tid,
                associations: vec![PropertyAssociation {
                    index: idx,
                    essential: true,
                }],
            });
        }
        let properties = ItemProperties {
            properties: vec![canonical, deviant],
            associations,
        };
        let mut items = Vec::new();
        items.push(ItemInfoEntry {
            item_id: primary_id,
            item_type: *b"grid",
            ..Default::default()
        });
        for &tid in tile_ids {
            items.push(ItemInfoEntry {
                item_id: tid,
                item_type: *b"hvc1",
                ..Default::default()
            });
        }
        let locations = vec![ItemLocation {
            item_id: primary_id,
            construction_method: 1,
            data_reference_index: 0,
            base_offset: 0,
            extents: vec![ItemExtent {
                index: 0,
                offset: 0,
                length: payload_len,
            }],
        }];
        let references = vec![ItemReference {
            kind: *b"dimg",
            from_item_id: primary_id,
            to_item_ids: tile_ids.to_vec(),
        }];
        BmffMeta {
            handler_type: *b"pict",
            primary_item: Some(primary_id),
            items,
            locations,
            idat: payload,
            xml: String::new(),
            bxml: Vec::new(),
            references,
            properties: Some(properties),
            data_references: Vec::new(),
        }
    }

    #[test]
    fn plan_grid_surfaces_per_tile_ispe_mismatch_warning() {
        // 2×2 grid, four 64×64 tiles; the LAST tile (item 13) has a
        // 30×64 ispe — typical "right-edge truncation" malformed shape.
        let tile_ids = vec![10u32, 11, 12, 13];
        let meta =
            make_grid_meta_with_deviant_tile(2, 1, 1, 128, 128, 64, 64, &tile_ids, 3, 30, 64);
        let plan = plan_grid_layout(&meta, 2).unwrap();
        // Canonical extent is still 64×64 (the first tile drives it).
        assert_eq!(plan.tile_w, 64);
        assert_eq!(plan.tile_h, 64);
        // Per-slot extents: first three at canonical, last at deviant.
        assert_eq!((plan.tiles[0].w, plan.tiles[0].h), (64, 64));
        assert_eq!((plan.tiles[1].w, plan.tiles[1].h), (64, 64));
        assert_eq!((plan.tiles[2].w, plan.tiles[2].h), (64, 64));
        assert_eq!((plan.tiles[3].w, plan.tiles[3].h), (30, 64));
        // One warning, naming the offending tile + canonical-vs-actual.
        assert_eq!(plan.tile_size_warnings.len(), 1);
        let w = plan.tile_size_warnings[0];
        assert_eq!(w.item_id, 13);
        assert_eq!((w.expected_w, w.expected_h), (64, 64));
        assert_eq!((w.actual_w, w.actual_h), (30, 64));
    }

    #[test]
    fn plan_grid_surfaces_warning_for_missing_per_tile_ispe() {
        // Build a grid where the FIRST tile has an ispe (so canonical
        // dims can be inferred) but a later tile is associated with NO
        // ispe at all. The planner shouldn't reject — it warns and
        // falls back to canonical extents for the per-slot record.
        let tile_ids = vec![10u32, 11];
        let mut meta = make_grid_meta(2, 0, 1, 128, 64, 64, 64, &tile_ids);
        // Strip tile 11's association row so it has no ispe.
        if let Some(props) = meta.properties.as_mut() {
            props.associations.retain(|a| a.item_id != 11);
        }
        let plan = plan_grid_layout(&meta, 2).unwrap();
        assert_eq!(plan.tiles.len(), 2);
        // Canonical fallback for the missing-ispe tile.
        assert_eq!((plan.tiles[1].w, plan.tiles[1].h), (64, 64));
        // One warning at (0, 0) extents, marking the absence.
        assert_eq!(plan.tile_size_warnings.len(), 1);
        assert_eq!(plan.tile_size_warnings[0].item_id, 11);
        assert_eq!(
            (
                plan.tile_size_warnings[0].actual_w,
                plan.tile_size_warnings[0].actual_h
            ),
            (0, 0)
        );
    }

    #[test]
    fn plan_overlay_surfaces_per_layer_ispe_when_present() {
        // Build an iovl fixture where each layer DOES have an ispe;
        // the planner copies the per-layer (w, h) into OverlayLayer.
        // Two layers: 64×64 and 32×16.
        let mut meta = make_iovl_meta(2, (256, 256), [0; 4], &[(20, 0, 0), (21, 50, 50)]);
        // Inject iprp with two ispe properties + per-layer associations.
        meta.properties = Some(ItemProperties {
            properties: vec![
                ItemProperty::Ispe(Ispe {
                    width: 64,
                    height: 64,
                }),
                ItemProperty::Ispe(Ispe {
                    width: 32,
                    height: 16,
                }),
            ],
            associations: vec![
                ItemPropertyAssociation {
                    item_id: 20,
                    associations: vec![PropertyAssociation {
                        index: 1,
                        essential: true,
                    }],
                },
                ItemPropertyAssociation {
                    item_id: 21,
                    associations: vec![PropertyAssociation {
                        index: 2,
                        essential: true,
                    }],
                },
            ],
        });
        let plan = plan_overlay_layout(&meta, 2).unwrap();
        assert_eq!((plan.layers[0].w, plan.layers[0].h), (64, 64));
        assert_eq!((plan.layers[1].w, plan.layers[1].h), (32, 16));
        // No layer is missing an ispe → no warnings.
        assert!(plan.layer_size_warnings.is_empty());
    }

    // ─── round-12: build_*_layout with caller-resolved bytes (mdat path) ───

    #[test]
    fn build_grid_layout_with_external_payload_bytes() {
        // The make_grid_meta fixture stores the payload in idat, but
        // build_grid_layout doesn't care — it accepts the bytes
        // verbatim. This is the path the demuxer takes for mdat-
        // resident grid items (construction_method == 0): resolve the
        // bytes from the file's mdat using file_extents_for_item, then
        // call build_grid_layout with the resolved bytes.
        let tile_ids = vec![10u32, 11, 12, 13];
        let meta = make_grid_meta(2, 1, 1, 128, 128, 64, 64, &tile_ids);
        // Hand-crafted payload (independent of the meta's idat).
        let payload = grid16(1, 1, 128, 128);
        let plan = build_grid_layout(&meta, 2, &payload).unwrap();
        assert_eq!(plan.canvas_w, 128);
        assert_eq!(plan.canvas_h, 128);
        assert_eq!(plan.tile_w, 64);
        assert_eq!(plan.tile_h, 64);
        assert_eq!(plan.tiles.len(), 4);
    }

    #[test]
    fn build_overlay_layout_with_external_payload_bytes() {
        let mut meta = make_iovl_meta(2, (256, 256), [0; 4], &[(20, 0, 0), (21, 96, 96)]);
        meta.properties = None; // simulate "no iprp / no ispe" path
                                // Hand-crafted payload (could have come from mdat).
        let payload = overlay16([0; 4], 256, 256, &[(0, 0), (96, 96)]);
        let plan = build_overlay_layout(&meta, 2, &payload).unwrap();
        assert_eq!(plan.layers.len(), 2);
        assert_eq!((plan.layers[0].w, plan.layers[0].h), (0, 0));
        assert_eq!(plan.layer_size_warnings.len(), 2);
    }
}
