//! HEIF derived-image renderers (`iden` identity transform, `iovl`
//! overlay composer, `grid` mosaic compositor).
//!
//! Per ISO/IEC 23008-12:2017 §6.6.2 the reconstructed image of a
//! derived item is produced by applying the item's `item_type`-
//! specific operation to its `dimg` source(s) AND THEN applying any
//! transformative item properties (`clap`, `irot`, `imir`)
//! associated with the derived item itself (§6.3). The renderers
//! here implement the second step in pure Rust over RGBA8 pixel
//! buffers; the first step (decoding the source HEVC items) is the
//! caller's responsibility — pull the bytes via [`crate::item_data`]
//! and decode through `oxideav-h265` (or any other codec crate).
//!
//! The renderers share a tiny pixel-buffer surface — [`Rgba8Canvas`]
//! — that we keep crate-local rather than reach into a bigger image
//! crate, because we don't want to grow a colour-management or
//! resampling dep just for the canvas-stack semantics HEIF needs.
//! Coverage:
//!
//! * [`render_iden`] applies the identity-derivation transformative
//!   property cascade per §6.6.2.1 + §6.3 + §6.5.10..§6.5.12 — `clap`
//!   crops the source first, then `irot` rotates 90° steps CCW, then
//!   `imir` flips. Property order is fixed by the spec; the function
//!   accepts the resolved property list verbatim from
//!   [`crate::ItemProperties::resolve`] and walks it once.
//! * [`render_iovl`] composes a layered canvas per §6.6.2.2.3 —
//!   per-pixel sRGB Porter-Duff "source over destination" with
//!   straight-alpha 16-bit-per-channel blending math, then
//!   round-to-8 at the boundary so the output canvas stays RGBA8.
//!   Negative `(h, v)` offsets clip the corresponding layer to the
//!   canvas without wrapping, per the spec's clipping rule.
//! * [`render_grid`] tiles row-major into the canvas per §6.6.2.3 —
//!   `tile_width = ceil(output_width / cols)`, `tile_height =
//!   ceil(output_height / rows)`, right-most column / bottom-most
//!   row cropped to fit. We do NOT resample tiles to a common size:
//!   the spec requires every tile to share the same encoded extent,
//!   so the renderer trusts the caller's pre-decoded buffers.

use crate::derived::{Grid, Overlay};
use crate::iprp::{Imir, Irot, Ispe, ItemProperty};
use crate::media_meta::Clap;

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Tightly-packed 8-bit RGBA pixel buffer used by the renderers.
///
/// The buffer is row-major, top-down, with stride =
/// `width * 4` bytes (no row padding). Construction validates the
/// `width × height × 4 == data.len()` invariant once so the
/// hot-path renderers can index without per-row bounds checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba8Canvas {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Rgba8Canvas {
    /// Build an opaque-black canvas of the given dimensions.
    pub fn new(width: u32, height: u32) -> Result<Self> {
        Self::filled(width, height, [0, 0, 0, 255])
    }

    /// Build a canvas filled with the given RGBA8 value.
    pub fn filled(width: u32, height: u32, fill: [u8; 4]) -> Result<Self> {
        let pixels = width
            .checked_mul(height)
            .ok_or_else(|| Error::invalid("HEIF render: width × height overflow"))?;
        let bytes = pixels
            .checked_mul(4)
            .ok_or_else(|| Error::invalid("HEIF render: pixel-byte count overflow"))?
            as usize;
        let mut data = Vec::with_capacity(bytes);
        for _ in 0..pixels {
            data.extend_from_slice(&fill);
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// Wrap an existing RGBA8 byte buffer. Returns `Err(InvalidData)`
    /// when the byte count doesn't match `width × height × 4`.
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        let needed = (width as usize)
            .checked_mul(height as usize)
            .and_then(|p| p.checked_mul(4))
            .ok_or_else(|| Error::invalid("HEIF render: wrap dimensions overflow"))?;
        if data.len() != needed {
            return Err(Error::invalid(format!(
                "HEIF render: RGBA8 buffer length {actual} != width({width}) × height({height}) × 4 = {needed}",
                actual = data.len(),
            )));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Read the pixel at `(x, y)` as `[r, g, b, a]`. Returns `None`
    /// when the coordinates are outside the canvas — callers don't
    /// have to bounds-check before calling.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let off = ((y as usize) * (self.width as usize) + x as usize) * 4;
        Some([
            self.data[off],
            self.data[off + 1],
            self.data[off + 2],
            self.data[off + 3],
        ])
    }
}

/// Apply the iden derivation per HEIF §6.6.2.1 to a single source
/// canvas. The `properties` slice is the property list `iprp` carries
/// for the `iden` item itself (the typical shape from
/// [`crate::ItemProperties::resolve`]); this function walks it once,
/// honouring the spec's order: `clap` first (crop), then `irot`
/// (rotate), then `imir` (mirror). Properties not in that set are
/// silently ignored — they belong to the source item, not the
/// derivation.
///
/// `source` is the *decoded* RGBA8 of the single `dimg` target;
/// returns the post-transform canvas. `clap` with denominators of
/// `0` returns `Err(InvalidData)` rather than divide by zero
/// (defensive — the spec implicitly forbids it but some authoring
/// tools have shipped it).
pub fn render_iden(source: &Rgba8Canvas, properties: &[&ItemProperty]) -> Result<Rgba8Canvas> {
    // Spec order — §6.3 last-NOTE on transformative property order:
    // "clap is applied before any rotation or mirroring". Then per
    // §6.5.10 NOTE 1: "irot is applied after clap". Then `imir` per
    // §6.5.12 NOTE on order ("mirroring is applied after rotation").
    let mut canvas = source.clone();
    if let Some(clap) = properties.iter().find_map(|p| match p {
        ItemProperty::Clap(c) => Some(*c),
        _ => None,
    }) {
        canvas = apply_clap(&canvas, &clap)?;
    }
    if let Some(irot) = properties.iter().find_map(|p| match p {
        ItemProperty::Irot(r) => Some(*r),
        _ => None,
    }) {
        canvas = apply_irot(&canvas, &irot);
    }
    if let Some(imir) = properties.iter().find_map(|p| match p {
        ItemProperty::Imir(m) => Some(*m),
        _ => None,
    }) {
        canvas = apply_imir(&canvas, &imir);
    }
    Ok(canvas)
}

/// Compose an `iovl` derived image per §6.6.2.2.3. Layers are
/// blended bottom-up over the canvas fill using straight-alpha
/// Porter-Duff "source over destination": each layer's RGB is
/// blended against the running canvas weighted by the layer's alpha,
/// and the canvas alpha tracks `1 - (1 - a_src) * (1 - a_dst)` so
/// transparent stacks combine the way HEIF readers expect.
///
/// Negative offsets are honoured — a layer that overhangs the
/// top/left of the canvas is clipped by the renderer (per the spec's
/// "Pixel locations with a negative offset value are not included
/// in the reconstructed image" wording). Same for layers that
/// overhang the bottom or right edges.
///
/// `layers.len()` must equal `overlay.offsets.len()`; otherwise an
/// `InvalidData` is returned. Each layer's `Rgba8Canvas` has its
/// own dimensions (HEIF does not require all layers to share an
/// encoded extent).
pub fn render_iovl(overlay: &Overlay, layers: &[Rgba8Canvas]) -> Result<Rgba8Canvas> {
    if overlay.offsets.len() != layers.len() {
        return Err(Error::invalid(format!(
            "HEIF iovl render: offsets.len()={off} != layers.len()={lay}",
            off = overlay.offsets.len(),
            lay = layers.len(),
        )));
    }
    // Reduce 16-bit-per-channel canvas fill to 8-bit for the canvas
    // (HEIF stores fill at 16 bpp but the rendered RGBA8 surface is
    // 8 bpp per channel). The conversion is `floor(v / 257)` so
    // 0xFFFF → 0xFF and 0x0000 → 0x00.
    let fill = [
        (overlay.canvas_fill_color[0] / 257) as u8,
        (overlay.canvas_fill_color[1] / 257) as u8,
        (overlay.canvas_fill_color[2] / 257) as u8,
        (overlay.canvas_fill_color[3] / 257) as u8,
    ];
    let mut canvas = Rgba8Canvas::filled(overlay.output_width, overlay.output_height, fill)?;
    for (layer, &(h_off, v_off)) in layers.iter().zip(overlay.offsets.iter()) {
        composite_layer(&mut canvas, layer, h_off, v_off);
    }
    Ok(canvas)
}

/// Tile a `grid` derived image per §6.6.2.3.3. `tiles` is in
/// row-major sweep order (top row left-to-right, then second row,
/// …); `tiles.len()` must equal `rows × cols`. Per the spec all
/// tiles share the same encoded `tile_width` × `tile_height`; the
/// reconstructed image is trimmed on the right and bottom to
/// `output_width` / `output_height` if the tiled extent overshoots.
///
/// We do not pad — the canvas is initially zero (transparent black)
/// and tiles overwrite their destination regions verbatim. The
/// caller picks the source-tile pre-decode (HEVC, AV1, JPEG-2000,
/// …) and supplies pre-rendered RGBA8 buffers.
///
/// Returns `Err(InvalidData)` when:
///
/// * `tiles.len() != rows × cols`
/// * tiles disagree on width/height (spec requires every tile to
///   carry the same encoded dimensions; we surface the broken file
///   rather than silently letterbox)
/// * `rows × tile_height < output_height` or `cols × tile_width <
///   output_width` (the tiled canvas wouldn't cover the output)
pub fn render_grid(grid: &Grid, tiles: &[Rgba8Canvas]) -> Result<Rgba8Canvas> {
    let expected = (grid.rows as usize)
        .checked_mul(grid.cols as usize)
        .ok_or_else(|| Error::invalid("HEIF grid render: rows × cols overflow"))?;
    if tiles.len() != expected {
        return Err(Error::invalid(format!(
            "HEIF grid render: tiles.len()={got} != rows({})×cols({}) = {expected}",
            grid.rows,
            grid.cols,
            got = tiles.len(),
        )));
    }
    if expected == 0 {
        return Err(Error::invalid("HEIF grid render: rows × cols == 0"));
    }
    let tile_w = tiles[0].width();
    let tile_h = tiles[0].height();
    for t in tiles.iter().skip(1) {
        if t.width() != tile_w || t.height() != tile_h {
            return Err(Error::invalid(
                "HEIF grid render: tiles disagree on dimensions (spec mandates a shared encoded extent)",
            ));
        }
    }
    let total_w = tile_w
        .checked_mul(grid.cols as u32)
        .ok_or_else(|| Error::invalid("HEIF grid render: tiled width overflow"))?;
    let total_h = tile_h
        .checked_mul(grid.rows as u32)
        .ok_or_else(|| Error::invalid("HEIF grid render: tiled height overflow"))?;
    if total_w < grid.output_width || total_h < grid.output_height {
        return Err(Error::invalid(format!(
            "HEIF grid render: tiled extent {total_w}×{total_h} cannot cover output {ow}×{oh}",
            ow = grid.output_width,
            oh = grid.output_height,
        )));
    }
    let mut canvas = Rgba8Canvas::new(grid.output_width, grid.output_height)?;
    for row in 0..grid.rows {
        for col in 0..grid.cols {
            let idx = (row as usize) * (grid.cols as usize) + col as usize;
            let tile = &tiles[idx];
            let x = (col as u32) * tile_w;
            let y = (row as u32) * tile_h;
            blit_opaque(&mut canvas, tile, x as i32, y as i32);
        }
    }
    Ok(canvas)
}

/// Apply a `clap` clean-aperture crop. The spec defines `clap` over
/// rational numbers with potentially-non-integer offsets; we
/// implement the integer-rounded centre-crop variant common in
/// authoring tools, which is the case ImageIO / libheif emit.
///
/// Centre-cropping equation (per ISO/IEC 14496-12 §12.1.4.1):
///
/// ```text
/// crop_w = clean_aperture_width_n  / clean_aperture_width_d
/// crop_h = clean_aperture_height_n / clean_aperture_height_d
/// pic_x  = (W - 1) / 2 + horiz_off_n / horiz_off_d
/// pic_y  = (H - 1) / 2 + vert_off_n  / vert_off_d
/// crop_x = round(pic_x - (crop_w - 1) / 2)
/// crop_y = round(pic_y - (crop_h - 1) / 2)
/// ```
///
/// Denominators of zero are rejected as `InvalidData`. Crop
/// rectangles partially outside the source are clipped to the
/// source's intersection rather than padded.
fn apply_clap(src: &Rgba8Canvas, clap: &Clap) -> Result<Rgba8Canvas> {
    if clap.clean_aperture_width_d == 0
        || clap.clean_aperture_height_d == 0
        || clap.horiz_off_d == 0
        || clap.vert_off_d == 0
    {
        return Err(Error::invalid("HEIF render: clap has zero denominator"));
    }
    let crop_w = clap.clean_aperture_width_n / clap.clean_aperture_width_d;
    let crop_h = clap.clean_aperture_height_n / clap.clean_aperture_height_d;
    if crop_w == 0 || crop_h == 0 {
        return Err(Error::invalid(
            "HEIF render: clap crop region has zero area",
        ));
    }
    // Per-spec centre coordinate. The `-1` adjustments yield the
    // same integer-pixel centre AVCC-style readers expect.
    let pic_x_int = ((src.width as i64) - 1) / 2;
    let pic_y_int = ((src.height as i64) - 1) / 2;
    let off_x = (clap.horiz_off_n as i64) / (clap.horiz_off_d as i64);
    let off_y = (clap.vert_off_n as i64) / (clap.vert_off_d as i64);
    let crop_w_i = crop_w as i64;
    let crop_h_i = crop_h as i64;
    let crop_x = pic_x_int + off_x - (crop_w_i - 1) / 2;
    let crop_y = pic_y_int + off_y - (crop_h_i - 1) / 2;

    // Clip to source.
    let src_w = src.width as i64;
    let src_h = src.height as i64;
    let x0 = crop_x.max(0);
    let y0 = crop_y.max(0);
    let x1 = (crop_x + crop_w_i).min(src_w);
    let y1 = (crop_y + crop_h_i).min(src_h);
    if x1 <= x0 || y1 <= y0 {
        return Err(Error::invalid(
            "HEIF render: clap crop region lies entirely outside source",
        ));
    }
    let out_w = (x1 - x0) as u32;
    let out_h = (y1 - y0) as u32;
    let mut out = Rgba8Canvas::new(out_w, out_h)?;
    let stride = (src.width as usize) * 4;
    let dst_stride = (out_w as usize) * 4;
    for y in 0..(out_h as usize) {
        let src_off = (y0 as usize + y) * stride + (x0 as usize) * 4;
        let dst_off = y * dst_stride;
        out.data[dst_off..dst_off + dst_stride]
            .copy_from_slice(&src.data[src_off..src_off + dst_stride]);
    }
    Ok(out)
}

/// Apply an `irot` rotation: `steps` × 90° counter-clockwise.
/// Rotation re-shapes the canvas: 90°/270° swap width and height,
/// 180° keeps them. Pixel data is rewritten verbatim.
fn apply_irot(src: &Rgba8Canvas, irot: &Irot) -> Rgba8Canvas {
    let steps = irot.steps & 3;
    if steps == 0 {
        return src.clone();
    }
    let (w, h) = (src.width, src.height);
    let (out_w, out_h) = if steps == 1 || steps == 3 {
        (h, w)
    } else {
        (w, h)
    };
    let mut out = Rgba8Canvas::filled(out_w, out_h, [0, 0, 0, 0]).expect("non-overflowing dims");
    let src_stride = (w as usize) * 4;
    let dst_stride = (out_w as usize) * 4;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let src_off = y * src_stride + x * 4;
            let (dx, dy) = match steps {
                // CCW by 90°: (x, y) → (y, W-1-x)
                1 => (y, w as usize - 1 - x),
                // 180°: (x, y) → (W-1-x, H-1-y)
                2 => (w as usize - 1 - x, h as usize - 1 - y),
                // CCW by 270° (== CW 90°): (x, y) → (H-1-y, x)
                3 => (h as usize - 1 - y, x),
                _ => unreachable!(),
            };
            let dst_off = dy * dst_stride + dx * 4;
            out.data[dst_off..dst_off + 4].copy_from_slice(&src.data[src_off..src_off + 4]);
        }
    }
    out
}

/// Apply an `imir` mirror: `axis = 0` flips the image vertically
/// (top↔bottom), `axis = 1` flips horizontally (left↔right). Per
/// HEIF §6.5.12.3.
fn apply_imir(src: &Rgba8Canvas, imir: &Imir) -> Rgba8Canvas {
    let (w, h) = (src.width, src.height);
    let mut out = Rgba8Canvas::filled(w, h, [0, 0, 0, 0]).expect("non-overflowing dims");
    let stride = (w as usize) * 4;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let (sx, sy) = match imir.axis & 1 {
                0 => (x, h as usize - 1 - y), // vertical mirror
                _ => (w as usize - 1 - x, y), // horizontal mirror
            };
            let src_off = sy * stride + sx * 4;
            let dst_off = y * stride + x * 4;
            out.data[dst_off..dst_off + 4].copy_from_slice(&src.data[src_off..src_off + 4]);
        }
    }
    out
}

/// Composite `layer` onto `canvas` at `(h_off, v_off)` using
/// straight-alpha Porter-Duff source-over-destination math. Pixels
/// outside the canvas are clipped (negative offsets and right/bottom
/// overhang both behave the same way per §6.6.2.2.3).
fn composite_layer(canvas: &mut Rgba8Canvas, layer: &Rgba8Canvas, h_off: i32, v_off: i32) {
    let cw = canvas.width as i64;
    let ch = canvas.height as i64;
    let lw = layer.width as i64;
    let lh = layer.height as i64;
    // Intersection of canvas and translated layer.
    let x0 = (h_off as i64).max(0);
    let y0 = (v_off as i64).max(0);
    let x1 = ((h_off as i64) + lw).min(cw);
    let y1 = ((v_off as i64) + lh).min(ch);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let canvas_w = canvas.width as usize;
    let layer_w = layer.width as usize;
    for cy in y0..y1 {
        for cx in x0..x1 {
            let lx = (cx - h_off as i64) as usize;
            let ly = (cy - v_off as i64) as usize;
            let src_off = ly * layer_w * 4 + lx * 4;
            let dst_off = (cy as usize) * canvas_w * 4 + (cx as usize) * 4;
            let src = [
                layer.data[src_off],
                layer.data[src_off + 1],
                layer.data[src_off + 2],
                layer.data[src_off + 3],
            ];
            let dst = [
                canvas.data[dst_off],
                canvas.data[dst_off + 1],
                canvas.data[dst_off + 2],
                canvas.data[dst_off + 3],
            ];
            let blended = blend_over(src, dst);
            canvas.data[dst_off..dst_off + 4].copy_from_slice(&blended);
        }
    }
}

/// Source-over-destination Porter-Duff blend with straight alpha.
/// All math is u32 to avoid intermediate overflow.
fn blend_over(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let sa = src[3] as u32;
    if sa == 0 {
        return dst;
    }
    if sa == 255 {
        return src;
    }
    let da = dst[3] as u32;
    // out_a = sa + da * (255 - sa) / 255
    let inv_sa = 255 - sa;
    let out_a = sa + (da * inv_sa + 127) / 255;
    if out_a == 0 {
        return [0, 0, 0, 0];
    }
    let mix_channel = |s: u8, d: u8| -> u8 {
        // straight alpha: out = (s*sa + d*da*(255-sa)/255) / out_a
        let s_term = (s as u32) * sa;
        let d_term = (d as u32) * da * inv_sa / 255;
        (((s_term + d_term) + (out_a / 2)) / out_a) as u8
    };
    [
        mix_channel(src[0], dst[0]),
        mix_channel(src[1], dst[1]),
        mix_channel(src[2], dst[2]),
        out_a as u8,
    ]
}

/// Straight blit of `tile` onto `canvas` at `(x, y)`. Pixels are
/// copied verbatim — no alpha blend (grids are opaque tiled
/// reconstructions). Out-of-bounds destination pixels are clipped.
fn blit_opaque(canvas: &mut Rgba8Canvas, tile: &Rgba8Canvas, x: i32, y: i32) {
    let cw = canvas.width as i64;
    let ch = canvas.height as i64;
    let tw = tile.width as i64;
    let th = tile.height as i64;
    let x0 = (x as i64).max(0);
    let y0 = (y as i64).max(0);
    let x1 = ((x as i64) + tw).min(cw);
    let y1 = ((y as i64) + th).min(ch);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let canvas_w = canvas.width as usize;
    let tile_w = tile.width as usize;
    let row_bytes = ((x1 - x0) as usize) * 4;
    for cy in y0..y1 {
        let ty = (cy - y as i64) as usize;
        let tx0 = (x0 - x as i64) as usize;
        let src_off = ty * tile_w * 4 + tx0 * 4;
        let dst_off = (cy as usize) * canvas_w * 4 + (x0 as usize) * 4;
        canvas.data[dst_off..dst_off + row_bytes]
            .copy_from_slice(&tile.data[src_off..src_off + row_bytes]);
    }
}

/// Take the [`Ispe`] dimensions a property carries, when present —
/// useful for the renderer's caller, which often needs to confirm
/// the source-buffer dimensions match the declared `ispe`. We
/// surface this as a tiny convenience.
pub fn ispe_dimensions(properties: &[&ItemProperty]) -> Option<(u32, u32)> {
    for p in properties {
        if let ItemProperty::Ispe(Ispe { width, height }) = p {
            return Some((*width, *height));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derived::Grid;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Rgba8Canvas {
        Rgba8Canvas::filled(width, height, rgba).unwrap()
    }

    fn checker(rgba_a: [u8; 4], rgba_b: [u8; 4]) -> Rgba8Canvas {
        // 4x4 checkerboard so the rotation/mirror tests can verify
        // exact pixel placement.
        let mut data = Vec::with_capacity(4 * 4 * 4);
        for y in 0..4 {
            for x in 0..4 {
                let pix = if (x + y) % 2 == 0 { rgba_a } else { rgba_b };
                data.extend_from_slice(&pix);
            }
        }
        Rgba8Canvas::from_rgba8(4, 4, data).unwrap()
    }

    #[test]
    fn canvas_filled_constructs_expected_byte_count() {
        let c = Rgba8Canvas::filled(8, 4, [1, 2, 3, 4]).unwrap();
        assert_eq!(c.width(), 8);
        assert_eq!(c.height(), 4);
        assert_eq!(c.data().len(), 8 * 4 * 4);
        assert_eq!(c.pixel(0, 0), Some([1, 2, 3, 4]));
        assert_eq!(c.pixel(7, 3), Some([1, 2, 3, 4]));
        assert_eq!(c.pixel(8, 0), None);
    }

    #[test]
    fn canvas_from_rgba8_rejects_size_mismatch() {
        // 4×3×4 = 48 bytes; we hand 47.
        assert!(Rgba8Canvas::from_rgba8(4, 3, vec![0u8; 47]).is_err());
    }

    #[test]
    fn iden_no_properties_returns_clone() {
        let s = solid(4, 4, [10, 20, 30, 40]);
        let out = render_iden(&s, &[]).unwrap();
        assert_eq!(out, s);
    }

    #[test]
    fn iden_irot_90ccw_rotates_pixels() {
        let s = checker([255, 0, 0, 255], [0, 255, 0, 255]);
        let prop = ItemProperty::Irot(Irot { steps: 1 });
        let out = render_iden(&s, &[&prop]).unwrap();
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 4);
        // CCW 90°: src(0,0) → out(0,3). src(0,0) is checker A (255,0,0,255).
        assert_eq!(out.pixel(0, 3), s.pixel(0, 0));
        assert_eq!(out.pixel(3, 0), s.pixel(3, 3));
    }

    #[test]
    fn iden_irot_180_inverts() {
        let s = checker([1, 2, 3, 255], [4, 5, 6, 255]);
        let prop = ItemProperty::Irot(Irot { steps: 2 });
        let out = render_iden(&s, &[&prop]).unwrap();
        assert_eq!(out.pixel(0, 0), s.pixel(3, 3));
        assert_eq!(out.pixel(3, 3), s.pixel(0, 0));
    }

    #[test]
    fn iden_imir_horizontal_flips_left_right() {
        let s = checker([1, 0, 0, 255], [0, 1, 0, 255]);
        let prop = ItemProperty::Imir(Imir { axis: 1 });
        let out = render_iden(&s, &[&prop]).unwrap();
        // axis=1 (horizontal): src(0, y) → out(W-1, y)
        assert_eq!(out.pixel(3, 0), s.pixel(0, 0));
        assert_eq!(out.pixel(0, 0), s.pixel(3, 0));
    }

    #[test]
    fn iden_imir_vertical_flips_top_bottom() {
        let s = checker([1, 0, 0, 255], [0, 1, 0, 255]);
        let prop = ItemProperty::Imir(Imir { axis: 0 });
        let out = render_iden(&s, &[&prop]).unwrap();
        // axis=0 (vertical): src(x, 0) → out(x, H-1)
        assert_eq!(out.pixel(0, 3), s.pixel(0, 0));
        assert_eq!(out.pixel(0, 0), s.pixel(0, 3));
    }

    #[test]
    fn iden_clap_centre_crop_2x2_from_4x4() {
        // 4x4 source, crop to 2x2 centred (no offset).
        let s = checker([10, 0, 0, 255], [0, 10, 0, 255]);
        let clap = Clap {
            clean_aperture_width_n: 2,
            clean_aperture_width_d: 1,
            clean_aperture_height_n: 2,
            clean_aperture_height_d: 1,
            horiz_off_n: 0,
            horiz_off_d: 1,
            vert_off_n: 0,
            vert_off_d: 1,
        };
        let prop = ItemProperty::Clap(clap);
        let out = render_iden(&s, &[&prop]).unwrap();
        assert_eq!(out.width(), 2);
        assert_eq!(out.height(), 2);
        // pic_x = (4-1)/2 = 1; crop_w = 2; crop_x = 1 - (2-1)/2 = 1.
        // So we expect src(1,1) ↔ out(0,0).
        assert_eq!(out.pixel(0, 0), s.pixel(1, 1));
    }

    #[test]
    fn iden_clap_zero_denominator_rejected() {
        let s = solid(4, 4, [0, 0, 0, 255]);
        let bad = Clap {
            clean_aperture_width_n: 2,
            clean_aperture_width_d: 0,
            clean_aperture_height_n: 2,
            clean_aperture_height_d: 1,
            horiz_off_n: 0,
            horiz_off_d: 1,
            vert_off_n: 0,
            vert_off_d: 1,
        };
        let prop = ItemProperty::Clap(bad);
        assert!(render_iden(&s, &[&prop]).is_err());
    }

    #[test]
    fn iden_clap_then_irot_then_imir_order() {
        // Property order in the slice should not matter; the renderer
        // applies them in spec order: clap → irot → imir.
        let s = checker([1, 0, 0, 255], [0, 1, 0, 255]);
        let clap = Clap {
            clean_aperture_width_n: 2,
            clean_aperture_width_d: 1,
            clean_aperture_height_n: 2,
            clean_aperture_height_d: 1,
            horiz_off_n: 0,
            horiz_off_d: 1,
            vert_off_n: 0,
            vert_off_d: 1,
        };
        let p_clap = ItemProperty::Clap(clap);
        let p_rot = ItemProperty::Irot(Irot { steps: 1 });
        let p_mir = ItemProperty::Imir(Imir { axis: 1 });
        let order_a = render_iden(&s, &[&p_clap, &p_rot, &p_mir]).unwrap();
        let order_b = render_iden(&s, &[&p_mir, &p_rot, &p_clap]).unwrap();
        assert_eq!(order_a, order_b);
    }

    #[test]
    fn iovl_canvas_fill_when_no_layers_overlap_pixel_at_origin() {
        // Single 1x1 transparent layer at (3, 3) — the (0, 0) pixel of
        // the canvas keeps the canvas fill.
        let layer = solid(1, 1, [255, 0, 0, 0]); // fully transparent
        let overlay = Overlay {
            canvas_fill_color: [16384, 16384, 16384, 65535], // grey opaque
            output_width: 8,
            output_height: 8,
            offsets: vec![(3, 3)],
        };
        let out = render_iovl(&overlay, &[layer]).unwrap();
        // 16384 / 257 = 63
        assert_eq!(out.pixel(0, 0), Some([63, 63, 63, 255]));
    }

    #[test]
    fn iovl_opaque_layer_overwrites_canvas() {
        let layer = solid(2, 2, [255, 0, 0, 255]);
        let overlay = Overlay {
            canvas_fill_color: [0, 0, 0, 65535],
            output_width: 4,
            output_height: 4,
            offsets: vec![(1, 1)],
        };
        let out = render_iovl(&overlay, &[layer]).unwrap();
        assert_eq!(out.pixel(1, 1), Some([255, 0, 0, 255]));
        assert_eq!(out.pixel(2, 2), Some([255, 0, 0, 255]));
        assert_eq!(out.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(out.pixel(3, 3), Some([0, 0, 0, 255]));
    }

    #[test]
    fn iovl_negative_offset_clips_layer_to_canvas() {
        let layer = solid(4, 4, [200, 100, 50, 255]);
        let overlay = Overlay {
            canvas_fill_color: [0, 0, 0, 65535],
            output_width: 4,
            output_height: 4,
            offsets: vec![(-2, -2)],
        };
        let out = render_iovl(&overlay, &[layer]).unwrap();
        // Top-left 2×2 of the canvas now shows the bottom-right 2×2 of
        // the layer (which is the same colour everywhere); bottom-right
        // 2×2 stays canvas fill.
        assert_eq!(out.pixel(0, 0), Some([200, 100, 50, 255]));
        assert_eq!(out.pixel(1, 1), Some([200, 100, 50, 255]));
        assert_eq!(out.pixel(2, 2), Some([0, 0, 0, 255]));
    }

    #[test]
    fn iovl_alpha_blends_with_canvas_fill() {
        // 50%-alpha red over opaque white canvas → mid-pink.
        let layer = solid(2, 2, [255, 0, 0, 128]);
        let overlay = Overlay {
            canvas_fill_color: [65535, 65535, 65535, 65535],
            output_width: 2,
            output_height: 2,
            offsets: vec![(0, 0)],
        };
        let out = render_iovl(&overlay, &[layer]).unwrap();
        let p = out.pixel(0, 0).unwrap();
        // R = (255*128 + 255*255*(255-128)/255) / out_a; out_a = 255.
        // → (32640 + 32385) / 255 = 64925 / 255 ≈ 254 (rounded)
        assert!(p[0] >= 250);
        assert!((120..=135).contains(&p[1]));
        assert!((120..=135).contains(&p[2]));
        assert_eq!(p[3], 255);
    }

    #[test]
    fn iovl_offsets_layer_count_mismatch_rejected() {
        let overlay = Overlay {
            canvas_fill_color: [0, 0, 0, 65535],
            output_width: 4,
            output_height: 4,
            offsets: vec![(0, 0), (1, 1)],
        };
        assert!(render_iovl(&overlay, &[solid(1, 1, [0, 0, 0, 255])]).is_err());
    }

    #[test]
    fn grid_2x2_tiles_cover_canvas() {
        let g = Grid {
            rows: 2,
            cols: 2,
            output_width: 256,
            output_height: 256,
        };
        let tiles = vec![
            solid(128, 128, [255, 0, 0, 255]),
            solid(128, 128, [0, 255, 0, 255]),
            solid(128, 128, [0, 0, 255, 255]),
            solid(128, 128, [255, 255, 0, 255]),
        ];
        let out = render_grid(&g, &tiles).unwrap();
        assert_eq!(out.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(out.pixel(200, 0), Some([0, 255, 0, 255]));
        assert_eq!(out.pixel(0, 200), Some([0, 0, 255, 255]));
        assert_eq!(out.pixel(200, 200), Some([255, 255, 0, 255]));
    }

    #[test]
    fn grid_trims_overshoot_on_right_and_bottom() {
        // 2x2 tiles of 100x100 each → 200x200 tiled extent; output is
        // 150x150. Right-most col and bottom-most row are clipped to
        // the output dims.
        let g = Grid {
            rows: 2,
            cols: 2,
            output_width: 150,
            output_height: 150,
        };
        let tiles = vec![
            solid(100, 100, [10, 0, 0, 255]),
            solid(100, 100, [0, 10, 0, 255]),
            solid(100, 100, [0, 0, 10, 255]),
            solid(100, 100, [10, 10, 0, 255]),
        ];
        let out = render_grid(&g, &tiles).unwrap();
        assert_eq!(out.width(), 150);
        assert_eq!(out.height(), 150);
        assert_eq!(out.pixel(120, 0), Some([0, 10, 0, 255]));
        assert_eq!(out.pixel(0, 120), Some([0, 0, 10, 255]));
        assert_eq!(out.pixel(120, 120), Some([10, 10, 0, 255]));
    }

    #[test]
    fn grid_tile_count_mismatch_rejected() {
        let g = Grid {
            rows: 2,
            cols: 2,
            output_width: 8,
            output_height: 8,
        };
        // Only 3 tiles supplied for a 2×2 = 4 grid.
        let tiles = vec![
            solid(4, 4, [0, 0, 0, 255]),
            solid(4, 4, [0, 0, 0, 255]),
            solid(4, 4, [0, 0, 0, 255]),
        ];
        assert!(render_grid(&g, &tiles).is_err());
    }

    #[test]
    fn grid_tile_dimension_mismatch_rejected() {
        let g = Grid {
            rows: 1,
            cols: 2,
            output_width: 8,
            output_height: 4,
        };
        let tiles = vec![solid(4, 4, [0, 0, 0, 255]), solid(5, 4, [0, 0, 0, 255])];
        assert!(render_grid(&g, &tiles).is_err());
    }

    #[test]
    fn grid_canvas_undersized_rejected() {
        // tiled extent 4x4 cannot cover output 5x5.
        let g = Grid {
            rows: 1,
            cols: 1,
            output_width: 5,
            output_height: 5,
        };
        let tiles = vec![solid(4, 4, [0, 0, 0, 255])];
        assert!(render_grid(&g, &tiles).is_err());
    }

    #[test]
    fn ispe_dimensions_picks_first_ispe() {
        let ispe = ItemProperty::Ispe(Ispe {
            width: 64,
            height: 32,
        });
        assert_eq!(ispe_dimensions(&[&ispe]), Some((64, 32)));
        assert_eq!(ispe_dimensions(&[]), None);
    }
}
