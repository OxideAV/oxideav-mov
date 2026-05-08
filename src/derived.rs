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
}
