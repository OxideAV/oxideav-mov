//! Clipping Region Atom (`crgn`) and its wrapper Clipping atom (`clip`).
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01),
//! §"Clipping Atoms" (p. 43) and §"Clipping Region Atoms" (p. 44; the
//! shared spec figure is Figure 2-8 on p. 43). QuickTime carries an
//! optional spatial mask for both the **movie as a whole**
//! (`moov/clip`) and **per individual track** (`moov/trak/clip`); in
//! both positions the `clip` atom is a single-child container whose
//! sole child is a `crgn` Clipping Region atom. The region itself is a
//! QuickDraw `Region` structure: a 16-bit size in bytes, an 8-byte
//! bounding box, and an optional opaque scanline payload that is
//! present only when the mask is more complex than a single rectangle.
//!
//! The QTFF (Apple) ancestry pre-dates ISO BMFF (ISO/IEC 14496-12);
//! ISO BMFF does not define `clip` or `crgn`. An MP4 / fMP4 / HEIF /
//! AVIF file will not carry one and the surfaced [`Clipping`] field
//! stays `None`. The atom is QuickTime-only.
//!
//! ## On-disk layout (QTFF p. 43, Figure 2-8)
//!
//! ```text
//! Bytes                       Field
//! 4                           Atom size                  ('clip' wrapper)
//! 4                           Type = 'clip'
//!   4                         Atom size                  ('crgn' leaf)
//!   4                         Type = 'crgn'
//!     2                       Region size                (u16 BE)
//!     8                       Region boundary box        (QuickDraw Rect)
//!     Variable                Clipping region data       (scanline payload)
//! ```
//!
//! The **region size** field on disk is the byte count of the whole
//! QuickDraw `Region`, **including** the 2 bytes that the field itself
//! occupies AND the 8-byte bounding box. The minimum legal value is
//! therefore `10` — a rectangular region with no scanline payload.
//! Larger values declare an additional `region_size - 10`-byte run of
//! opaque QuickDraw scanline data describing a polygonal / multi-rect
//! mask; the parser surfaces those bytes verbatim
//! ([`ClippingRegion::region_data`]) rather than trying to decode the
//! QuickDraw scanline format, which has no QTFF documentation and is
//! historical OS-level Toolbox state.
//!
//! ## Bounding box
//!
//! The 8-byte bounding box is a QuickDraw `Rect`: four 16-bit
//! big-endian signed integers in `(top, left, bottom, right)` order
//! (QTFF p. 312 — Common Data Types: "Rectangle: four 16-bit integers"
//! — combined with the QuickDraw native field order that pre-dates the
//! spec; signed-ness follows the QuickDraw Toolbox convention so masks
//! anchored above / left of origin round-trip correctly).
//!
//! ## Validation
//!
//! Per spec:
//!
//! * The `crgn` body must be at least 10 bytes (`region_size` + Rect).
//! * `region_size` must be at least 10 — the field counts itself plus
//!   the bounding box, so any smaller value is a malformed writer.
//! * `region_size` may not exceed the body length (`region_size - 10`
//!   bytes of scanline data must fit inside the atom).
//!
//! The wrapper `clip` atom validation is structural — it must contain
//! exactly one `crgn` child. Unknown sibling atoms inside `clip` are
//! tolerated but ignored, matching how every other QTFF container in
//! this crate handles forward-compat children.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// QuickDraw `Rect` — four 16-bit big-endian signed integers in
/// `(top, left, bottom, right)` order. Signed so a mask anchored
/// above / left of the screen origin can be represented without
/// reinterpretation; in practice every QTFF writer keeps the fields
/// non-negative.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QdRect {
    /// Top edge in pixel coordinates.
    pub top: i16,
    /// Left edge in pixel coordinates.
    pub left: i16,
    /// Bottom edge in pixel coordinates (exclusive — the QuickDraw
    /// convention; `bottom - top` is the height).
    pub bottom: i16,
    /// Right edge in pixel coordinates (exclusive — `right - left`
    /// is the width).
    pub right: i16,
}

impl QdRect {
    /// Width of the rectangle in pixels (`right - left`). Returned as
    /// `i32` to avoid sign-bit overflow on a rect whose `right` is
    /// near `i16::MAX` and `left` is near `i16::MIN`.
    pub fn width(&self) -> i32 {
        self.right as i32 - self.left as i32
    }

    /// Height of the rectangle in pixels (`bottom - top`). Returned
    /// as `i32` for the same reason as [`width`](Self::width).
    pub fn height(&self) -> i32 {
        self.bottom as i32 - self.top as i32
    }

    /// True when the rectangle has zero or negative extent on either
    /// axis (QuickDraw's empty-rect convention; nothing is drawn).
    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

/// Parsed `crgn` Clipping Region atom (QTFF p. 44).
///
/// Carries the bounding box of the QuickDraw region plus any optional
/// scanline payload that describes a non-rectangular mask shape. A
/// `region_size` of exactly 10 means the region is the bounding-box
/// rectangle itself (no scanline data — `region_data` is empty).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClippingRegion {
    /// QuickDraw `rgnSize` (`region_size`) field — number of bytes in
    /// the entire region structure, **including** the 2-byte size
    /// field and the 8-byte bounding box. Always `>= 10` (a rectangle
    /// is the minimum legal region).
    pub region_size: u16,
    /// QuickDraw `rgnBBox` — the rectangular bound of the mask.
    pub bounding_box: QdRect,
    /// Optional scanline payload describing the in-bound region
    /// shape. Empty when `region_size == 10` (rectangular mask). For
    /// `region_size > 10` this slice has length `region_size - 10`
    /// and contains opaque QuickDraw region data; QTFF does not
    /// document the scanline format so the parser surfaces it as
    /// bytes for callers that want to round-trip the atom unchanged.
    pub region_data: Vec<u8>,
}

impl ClippingRegion {
    /// True when the region is exactly its bounding-box rectangle
    /// (no scanline data). Equivalent to `region_size == 10`.
    pub fn is_rectangular(&self) -> bool {
        self.region_size == 10 && self.region_data.is_empty()
    }
}

/// Parsed `clip` Clipping atom (QTFF p. 43).
///
/// Single-child container whose sole spec-defined child is a `crgn`
/// Clipping Region. Surfaced as the parsed region directly to keep
/// the call site short; the wrapper container itself carries no
/// additional fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Clipping {
    /// The single `crgn` child (QTFF p. 43, Figure 2-8). Required
    /// when the parent `clip` atom is present at all — there is no
    /// other legal child shape.
    pub region: ClippingRegion,
}

/// Parse a `crgn` body (the bytes between the atom header and its
/// trailing edge — the caller has already consumed the 8-byte `[size,
/// type]` header).
///
/// `payload.len()` must be at least 10 (the fixed `region_size` +
/// `bounding_box` header) and must be at least `region_size` once
/// `region_size` is decoded. Trailing bytes beyond `region_size` are
/// rejected — the spec has the region run to the end of the atom, so
/// any tail data signals a malformed writer.
pub fn parse_crgn(payload: &[u8]) -> Result<ClippingRegion> {
    if payload.len() < 10 {
        return Err(Error::invalid(format!(
            "MOV: crgn payload too short ({} bytes; need at least 10 for region_size + bbox)",
            payload.len()
        )));
    }
    let region_size = u16::from_be_bytes([payload[0], payload[1]]);
    if region_size < 10 {
        return Err(Error::invalid(format!(
            "MOV: crgn region_size {region_size} < 10 (QTFF p. 44: field counts itself + 8-byte bbox)"
        )));
    }
    let region_size_usz = region_size as usize;
    if region_size_usz > payload.len() {
        return Err(Error::invalid(format!(
            "MOV: crgn region_size {region_size} exceeds payload length {}",
            payload.len()
        )));
    }
    if region_size_usz < payload.len() {
        return Err(Error::invalid(format!(
            "MOV: crgn payload has {} trailing bytes after declared region_size {region_size}",
            payload.len() - region_size_usz
        )));
    }
    let top = i16::from_be_bytes([payload[2], payload[3]]);
    let left = i16::from_be_bytes([payload[4], payload[5]]);
    let bottom = i16::from_be_bytes([payload[6], payload[7]]);
    let right = i16::from_be_bytes([payload[8], payload[9]]);
    let bounding_box = QdRect {
        top,
        left,
        bottom,
        right,
    };
    // region_data is the tail past the 10-byte fixed header. Empty
    // for a rectangular region; opaque QuickDraw scanline bytes
    // otherwise.
    let region_data = payload[10..region_size_usz].to_vec();
    Ok(ClippingRegion {
        region_size,
        bounding_box,
        region_data,
    })
}

/// Parse a `clip` body — the payload of the wrapper atom, which
/// contains a single `crgn` child per QTFF p. 43 Figure 2-8.
///
/// Returns:
///
/// * `Error::invalid` when the payload contains no `crgn` child
///   (the wrapper atom is meaningless without one).
/// * `Error::invalid` when a `crgn` child fails to parse (the inner
///   error is surfaced verbatim).
///
/// Unknown sibling atoms inside `clip` are tolerated and ignored
/// (forward-compat: a future spec revision could add helper children
/// without breaking existing parsers).
pub fn parse_clip(payload: &[u8]) -> Result<Clipping> {
    let mut region: Option<ClippingRegion> = None;
    let mut p = 0usize;
    while p + 8 <= payload.len() {
        let size = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]])
            as usize;
        // QTFF p. 19 — `size == 0` extends to end-of-parent. Inside a
        // `clip` payload that means "the rest of this buffer", which
        // gives us a valid run for the trailing child.
        let span_end = if size == 0 {
            payload.len()
        } else if size < 8 || p + size > payload.len() {
            // Malformed child; stop the walk so the caller still gets
            // the parsed `crgn` if it appeared earlier.
            break;
        } else {
            p + size
        };
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&payload[p + 4..p + 8]);
        if &fc == b"crgn" {
            let body = &payload[p + 8..span_end];
            let parsed = parse_crgn(body)?;
            // First-wins on duplicates: §"Clipping Atoms" diagrams a
            // single `crgn` child; the spec does not define merge
            // semantics so we preserve the first occurrence rather
            // than silently overwriting it with whatever comes later.
            if region.is_none() {
                region = Some(parsed);
            }
        }
        p = span_end;
    }
    let region = region
        .ok_or_else(|| Error::invalid("MOV: clip atom contains no crgn child (QTFF p. 43)"))?;
    Ok(Clipping { region })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `crgn` *body* (post-atom-header) with the given
    /// bounding box and optional scanline tail.
    fn build_crgn_body(bbox: (i16, i16, i16, i16), tail: &[u8]) -> Vec<u8> {
        let region_size = 10u16 + tail.len() as u16;
        let mut p = Vec::with_capacity(region_size as usize);
        p.extend_from_slice(&region_size.to_be_bytes());
        p.extend_from_slice(&bbox.0.to_be_bytes());
        p.extend_from_slice(&bbox.1.to_be_bytes());
        p.extend_from_slice(&bbox.2.to_be_bytes());
        p.extend_from_slice(&bbox.3.to_be_bytes());
        p.extend_from_slice(tail);
        p
    }

    /// Wrap an arbitrary atom body in a 4+4 size/type header for use
    /// inside another container payload.
    fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
        let size: u32 = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(body);
    }

    #[test]
    fn rectangular_region_round_trip() {
        // Minimum legal region — just the bounding-box rectangle.
        let body = build_crgn_body((10, 20, 110, 220), &[]);
        assert_eq!(body.len(), 10);
        let crgn = parse_crgn(&body).unwrap();
        assert_eq!(crgn.region_size, 10);
        assert_eq!(crgn.bounding_box.top, 10);
        assert_eq!(crgn.bounding_box.left, 20);
        assert_eq!(crgn.bounding_box.bottom, 110);
        assert_eq!(crgn.bounding_box.right, 220);
        assert_eq!(crgn.bounding_box.width(), 200);
        assert_eq!(crgn.bounding_box.height(), 100);
        assert!(!crgn.bounding_box.is_empty());
        assert!(crgn.region_data.is_empty());
        assert!(crgn.is_rectangular());
    }

    #[test]
    fn region_with_scanline_tail_preserved() {
        // 4 bytes of opaque scanline payload — surfaced verbatim.
        let scanline = [0xAA, 0xBB, 0xCC, 0xDD];
        let body = build_crgn_body((0, 0, 50, 50), &scanline);
        assert_eq!(body.len(), 14);
        let crgn = parse_crgn(&body).unwrap();
        assert_eq!(crgn.region_size, 14);
        assert_eq!(
            crgn.bounding_box,
            QdRect {
                top: 0,
                left: 0,
                bottom: 50,
                right: 50
            }
        );
        assert_eq!(crgn.region_data, scanline);
        assert!(!crgn.is_rectangular());
    }

    #[test]
    fn negative_origin_signed_decoding() {
        // QuickDraw Rect signed-ness check — negative top/left round
        // through as i16 rather than wrapping to a huge u16.
        let body = build_crgn_body((-32, -64, 128, 256), &[]);
        let crgn = parse_crgn(&body).unwrap();
        assert_eq!(crgn.bounding_box.top, -32);
        assert_eq!(crgn.bounding_box.left, -64);
        assert_eq!(crgn.bounding_box.bottom, 128);
        assert_eq!(crgn.bounding_box.right, 256);
        assert_eq!(crgn.bounding_box.width(), 320);
        assert_eq!(crgn.bounding_box.height(), 160);
    }

    #[test]
    fn empty_bounding_box_surfaced() {
        // top == bottom and left == right — QuickDraw empty-rect
        // convention. The parser accepts the shape; the caller can
        // decide what to do with a zero-area mask.
        let body = build_crgn_body((10, 10, 10, 10), &[]);
        let crgn = parse_crgn(&body).unwrap();
        assert_eq!(crgn.bounding_box.width(), 0);
        assert_eq!(crgn.bounding_box.height(), 0);
        assert!(crgn.bounding_box.is_empty());
        assert!(crgn.is_rectangular());
    }

    #[test]
    fn rejects_payload_below_10_bytes() {
        let body = vec![0u8; 9];
        let err = parse_crgn(&body).unwrap_err();
        assert!(format!("{err}").contains("crgn payload too short"));
    }

    #[test]
    fn rejects_region_size_below_10() {
        // region_size = 8 violates the "field + bbox" invariant.
        let mut body = build_crgn_body((0, 0, 1, 1), &[]);
        body[0] = 0x00;
        body[1] = 0x08;
        let err = parse_crgn(&body).unwrap_err();
        assert!(format!("{err}").contains("region_size 8 < 10"));
    }

    #[test]
    fn rejects_region_size_overshooting_payload() {
        // region_size = 12 in a 10-byte body claims 2 bytes of
        // scanline data that aren't there.
        let mut body = build_crgn_body((0, 0, 1, 1), &[]);
        body[0] = 0x00;
        body[1] = 0x0C;
        let err = parse_crgn(&body).unwrap_err();
        assert!(format!("{err}").contains("exceeds payload length"));
    }

    #[test]
    fn rejects_trailing_bytes_past_region_size() {
        // region_size = 10 but 4 bytes of trailing junk follow the
        // bounding box. Spec-malformed writer.
        let mut body = build_crgn_body((0, 0, 1, 1), &[]);
        body.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let err = parse_crgn(&body).unwrap_err();
        assert!(format!("{err}").contains("trailing bytes"));
    }

    #[test]
    fn parse_clip_wraps_single_crgn_child() {
        // Wrapper `clip` payload carrying one `crgn` child — the
        // canonical movie-level shape.
        let crgn_body = build_crgn_body((5, 10, 105, 210), &[]);
        let mut clip_payload = Vec::new();
        push_atom(&mut clip_payload, *b"crgn", &crgn_body);
        let clip = parse_clip(&clip_payload).unwrap();
        assert_eq!(clip.region.region_size, 10);
        assert_eq!(clip.region.bounding_box.top, 5);
        assert_eq!(clip.region.bounding_box.left, 10);
        assert_eq!(clip.region.bounding_box.bottom, 105);
        assert_eq!(clip.region.bounding_box.right, 210);
        assert_eq!(clip.region.bounding_box.width(), 200);
        assert_eq!(clip.region.bounding_box.height(), 100);
    }

    #[test]
    fn parse_clip_first_wins_on_duplicate_crgn() {
        // Spec figure shows a single `crgn` child; if a malformed
        // writer emits two, the first one wins (matches the mvhd /
        // pdin / ctab conservative-merge convention elsewhere in
        // the crate).
        let first = build_crgn_body((1, 1, 11, 11), &[]);
        let second = build_crgn_body((99, 99, 199, 199), &[]);
        let mut clip_payload = Vec::new();
        push_atom(&mut clip_payload, *b"crgn", &first);
        push_atom(&mut clip_payload, *b"crgn", &second);
        let clip = parse_clip(&clip_payload).unwrap();
        assert_eq!(clip.region.bounding_box.top, 1);
        assert_eq!(clip.region.bounding_box.right, 11);
    }

    #[test]
    fn parse_clip_tolerates_unknown_sibling_atoms() {
        // Forward-compat: a hypothetical future helper child sitting
        // alongside `crgn` must not break the parse.
        let crgn_body = build_crgn_body((0, 0, 100, 100), &[]);
        let mut clip_payload = Vec::new();
        push_atom(&mut clip_payload, *b"xxxx", &[0xAA, 0xBB, 0xCC]);
        push_atom(&mut clip_payload, *b"crgn", &crgn_body);
        push_atom(&mut clip_payload, *b"yyyy", &[0x11, 0x22]);
        let clip = parse_clip(&clip_payload).unwrap();
        assert_eq!(clip.region.bounding_box.bottom, 100);
        assert_eq!(clip.region.bounding_box.right, 100);
    }

    #[test]
    fn parse_clip_rejects_missing_crgn_child() {
        // A `clip` atom with no `crgn` child is meaningless per the
        // spec figure (the wrapper exists only to host the region).
        let mut clip_payload = Vec::new();
        push_atom(&mut clip_payload, *b"xxxx", &[0u8; 4]);
        let err = parse_clip(&clip_payload).unwrap_err();
        assert!(format!("{err}").contains("no crgn child"));
    }

    #[test]
    fn parse_clip_surfaces_inner_crgn_error() {
        // A `crgn` body with region_size < 10 must surface the parse
        // error rather than be silently dropped.
        let bad_crgn = vec![0u8, 5, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut clip_payload = Vec::new();
        push_atom(&mut clip_payload, *b"crgn", &bad_crgn);
        assert!(parse_clip(&clip_payload).is_err());
    }

    #[test]
    fn parse_clip_handles_size_zero_trailing_child() {
        // §QTFF p. 19 — `size == 0` extends to end-of-parent. A
        // writer that emits a trailing `crgn` with size=0 should
        // still be recoverable.
        let crgn_body = build_crgn_body((3, 4, 13, 14), &[]);
        let mut clip_payload = Vec::new();
        // Manual emit with size=0.
        clip_payload.extend_from_slice(&0u32.to_be_bytes());
        clip_payload.extend_from_slice(b"crgn");
        clip_payload.extend_from_slice(&crgn_body);
        let clip = parse_clip(&clip_payload).unwrap();
        assert_eq!(clip.region.bounding_box.top, 3);
        assert_eq!(clip.region.bounding_box.right, 14);
    }
}
