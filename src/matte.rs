//! Track Matte atom (`matt`) and its sole child Compressed Matte atom
//! (`kmat`).
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01),
//! §"Track Matte Atoms" (p. 44) and §"Compressed Matte Atoms" (p. 45);
//! shared spec figure Figure 2-9 on p. 45. A track matte is an
//! Apple-specific per-track *visual blend mask*: the compressed matte
//! image is composited against the track's video frame when the track
//! is presented. The wrapper `matt` atom is a single-child container
//! whose sole defined child is a `kmat` Compressed Matte atom; the
//! `kmat` body in turn carries a version+flags FullBox-style header, a
//! standard QTFF image description structure (the same shape as a
//! video sample description per QTFF p. 70 / pp. 92–94), and a
//! variable-length blob of compressed matte data interpreted by the
//! codec the image description names.
//!
//! ## On-disk layout (QTFF p. 45, Figure 2-9)
//!
//! ```text
//! Bytes                       Field
//! 4                           Atom size                  ('matt' wrapper)
//! 4                           Type = 'matt'
//!   4                         Atom size                  ('kmat' leaf)
//!   4                         Type = 'kmat'
//!     1                       Version
//!     3                       Flags                      (set to 0 per spec)
//!     Variable                Matte image description    (standard image desc)
//!     Variable                Matte data                 (compressed bytes)
//! ```
//!
//! The image description structure begins with its own 4-byte size word
//! (QTFF p. 70 "Sample description size"), so the parser can carve the
//! description out of the body without further knowledge of which
//! codec it names; whatever bytes follow the description run to the
//! end of the `kmat` atom and are the compressed matte data, surfaced
//! verbatim. We treat the image description as opaque bytes too — its
//! per-codec extensions (e.g. `pasp` / `colr` / `clap`) are decoded
//! elsewhere in this crate for video sample descriptions and are not
//! re-implemented here, because the matte's role is to feed the
//! caller-chosen codec rather than to drive the QTFF media pipeline.
//!
//! ## Scope (track-level only)
//!
//! QTFF p. 41 Figure 2-6 places `matt` inside an individual `trak`
//! atom alongside `tkhd` / `mdia` / `edts` / `tref` / `load` / `imap`
//! / `clip` / `udta`. There is no movie-level matte atom: a movie's
//! visual blending is the union of its tracks' mattes. The demuxer
//! therefore surfaces this atom only via [`crate::Track`], not via
//! [`crate::MovDemuxer`] directly.
//!
//! ## ISO BMFF
//!
//! ISO BMFF (ISO/IEC 14496-12) does not define either `matt` or
//! `kmat`. An MP4 / fMP4 / HEIF / AVIF file will not carry these
//! atoms and the surfaced [`Matte`] field stays `None`.
//!
//! ## Validation
//!
//! Per spec p. 45:
//!
//! * The `kmat` body must be at least 8 bytes (1 version + 3 flags +
//!   4-byte image description size word).
//! * `version` is a single byte; the spec fixes it at 0 (the field is
//!   "the version of this compressed matte atom"). Unknown versions
//!   are rejected so a future writer's extension cannot silently
//!   drop matte data.
//! * `flags` must be 0 per the spec line "Three bytes of space for
//!   flags. Set this field to 0." A non-zero value is treated as a
//!   malformed writer and rejected.
//! * The 4-byte image description size word must be at least 16 (its
//!   own size + format FourCC + 6 reserved + 2-byte data reference
//!   index per QTFF p. 70), and must not exceed the bytes remaining
//!   in the body.
//!
//! The wrapper `matt` validation is structural — it must contain
//! exactly one `kmat` child. Unknown sibling atoms inside `matt` are
//! tolerated but ignored, matching how every other QTFF container in
//! this crate handles forward-compat children (the same policy as
//! `clip` / `crgn`).

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Minimum on-disk image description size per QTFF p. 70: 4-byte size
/// field + 4-byte data format + 6 reserved bytes + 2-byte
/// data_reference_index = 16. Any value below this is malformed
/// regardless of which media type the description names.
pub const MIN_IMAGE_DESCRIPTION_SIZE: u32 = 16;

/// Parsed `kmat` Compressed Matte atom (QTFF p. 45).
///
/// Carries the FullBox-style 1-byte version + 3-byte flags header, the
/// opaque QTFF image description structure (same shape as a video
/// sample description per QTFF p. 70 — surfaced as bytes so callers
/// can hand it to the codec it names), and the trailing compressed
/// matte data bytes (interpreted by the codec the image description
/// identifies; opaque to this parser per spec line "The compressed
/// matte data, which is of variable length.").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompressedMatte {
    /// Single-byte `version` field (QTFF p. 45). Always 0 in spec-
    /// conformant writers; non-zero values are rejected at parse time.
    pub version: u8,
    /// Three-byte `flags` field (QTFF p. 45). Spec line: "Set this
    /// field to 0." Non-zero values are rejected at parse time.
    pub flags: u32,
    /// Raw image description structure as bytes — the same on-disk
    /// shape as a video sample description (QTFF p. 70 + pp. 92–94).
    /// The first 4 bytes are the structure's own size field; the
    /// next 4 are the codec FourCC. Kept verbatim so a caller that
    /// wants to feed the matte to its codec round-trip the bytes
    /// unchanged.
    pub image_description: Vec<u8>,
    /// Compressed matte data (opaque to the demuxer; interpreted by
    /// the codec named in the image description). May be empty if the
    /// writer chose to emit the image description alone, though the
    /// spec figure implies the field is normally populated.
    pub matte_data: Vec<u8>,
}

impl CompressedMatte {
    /// The 4-byte codec FourCC at offset 4 of the image description
    /// structure (QTFF p. 70 "Data format"). Returns `None` when the
    /// image description is shorter than 8 bytes — which would have
    /// been rejected at parse time, so this is just a defensive
    /// accessor for callers manipulating constructed values.
    pub fn data_format(&self) -> Option<[u8; 4]> {
        if self.image_description.len() < 8 {
            return None;
        }
        let mut out = [0u8; 4];
        out.copy_from_slice(&self.image_description[4..8]);
        Some(out)
    }

    /// The declared on-disk size of the image description structure
    /// (QTFF p. 70 "Sample description size"). Returns the parsed
    /// `image_description.len()` as `u32`; the parser checks the
    /// declared field equals the stored slice length so the two are
    /// interchangeable.
    pub fn image_description_size(&self) -> u32 {
        self.image_description.len() as u32
    }

    /// Serialise into the `kmat` atom body — the exact inverse of
    /// [`parse_kmat`]. Layout (QTFF p. 45): `[version:1][flags:3]`, then
    /// the image description structure (preceded by its own 4-byte size
    /// word which the parser reads back as `image_description[0..4]`),
    /// then the compressed matte data.
    ///
    /// `version` / `flags` are written from the stored fields; a
    /// spec-conformant caller leaves both 0 (the parser rejects non-zero
    /// on read). The image description is emitted verbatim — the caller
    /// is responsible for the 4-byte size word at its head matching the
    /// structure length, exactly as a video sample description carries
    /// its own size (QTFF p. 70).
    pub fn to_body_bytes(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(4 + self.image_description.len() + self.matte_data.len());
        p.push(self.version);
        p.push(((self.flags >> 16) & 0xFF) as u8);
        p.push(((self.flags >> 8) & 0xFF) as u8);
        p.push((self.flags & 0xFF) as u8);
        p.extend_from_slice(&self.image_description);
        p.extend_from_slice(&self.matte_data);
        p
    }
}

impl Matte {
    /// Serialise into the `matt` atom payload — a single framed `kmat`
    /// child (`[size:u32][\"kmat\"][kmat body]`, QTFF p. 45 Figure 2-9).
    /// The inverse of [`parse_matt`] (which surfaces the lone `kmat`
    /// directly onto [`Matte::compressed`]).
    pub fn to_body_bytes(&self) -> Vec<u8> {
        let kmat = self.compressed.to_body_bytes();
        let mut p = Vec::with_capacity(8 + kmat.len());
        p.extend_from_slice(&((8 + kmat.len()) as u32).to_be_bytes());
        p.extend_from_slice(b"kmat");
        p.extend_from_slice(&kmat);
        p
    }
}

/// Parsed `matt` Track Matte atom (QTFF p. 44).
///
/// Single-child container whose sole spec-defined child is a `kmat`
/// Compressed Matte atom. Surfaced as the parsed compressed matte
/// directly to keep the call site short; the wrapper container itself
/// carries no additional fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Matte {
    /// The single `kmat` child (QTFF p. 45, Figure 2-9). Required
    /// when the parent `matt` atom is present at all — there is no
    /// other legal child shape.
    pub compressed: CompressedMatte,
}

/// Parse a `kmat` body (the bytes between the atom header and its
/// trailing edge — the caller has already consumed the 8-byte `[size,
/// type]` header).
///
/// `payload.len()` must be at least 8 (1-byte version + 3-byte flags +
/// 4-byte image description size word). The image description size
/// word must declare a length of at least [`MIN_IMAGE_DESCRIPTION_SIZE`]
/// (QTFF p. 70 minimum: size + format + reserved + dref_index = 16) and
/// must not exceed the bytes remaining after the FullBox header.
pub fn parse_kmat(payload: &[u8]) -> Result<CompressedMatte> {
    if payload.len() < 8 {
        return Err(Error::invalid(format!(
            "MOV: kmat payload too short ({} bytes; need at least 8 for ver/flags + image-desc size word)",
            payload.len()
        )));
    }
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "MOV: kmat unknown version {version} (QTFF p. 45 fixes the field at 0)"
        )));
    }
    let flags = (payload[1] as u32) << 16 | (payload[2] as u32) << 8 | (payload[3] as u32);
    if flags != 0 {
        return Err(Error::invalid(format!(
            "MOV: kmat flags={flags:#06x} != 0 (QTFF p. 45: 'Set this field to 0')"
        )));
    }
    let id_size = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    if id_size < MIN_IMAGE_DESCRIPTION_SIZE {
        return Err(Error::invalid(format!(
            "MOV: kmat image-description size {id_size} < {MIN_IMAGE_DESCRIPTION_SIZE} (QTFF p. 70 minimum for size+format+reserved+dref_index)"
        )));
    }
    let id_size_usz = id_size as usize;
    // The size word counts itself; the image description occupies the
    // run starting at offset 4 (right after the FullBox 1+3 header)
    // and must fit inside the remaining body.
    if 4 + id_size_usz > payload.len() {
        return Err(Error::invalid(format!(
            "MOV: kmat image-description size {id_size} exceeds body remainder {} (payload {} bytes, header 4)",
            payload.len() - 4,
            payload.len()
        )));
    }
    let image_description = payload[4..4 + id_size_usz].to_vec();
    let matte_data = payload[4 + id_size_usz..].to_vec();
    Ok(CompressedMatte {
        version,
        flags,
        image_description,
        matte_data,
    })
}

/// Parse a `matt` body — the payload of the wrapper atom, which
/// contains a single `kmat` child per QTFF p. 45 Figure 2-9.
///
/// Returns:
///
/// * `Error::invalid` when the payload contains no `kmat` child
///   (the wrapper atom is meaningless without one).
/// * `Error::invalid` when a `kmat` child fails to parse (the inner
///   error is surfaced verbatim).
///
/// Unknown sibling atoms inside `matt` are tolerated and ignored
/// (forward-compat: a future spec revision could add helper children
/// without breaking existing parsers — same policy as `clip`).
pub fn parse_matt(payload: &[u8]) -> Result<Matte> {
    let mut compressed: Option<CompressedMatte> = None;
    let mut p = 0usize;
    while p + 8 <= payload.len() {
        let size = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]])
            as usize;
        // QTFF p. 19 — `size == 0` extends to end-of-parent. Inside a
        // `matt` payload that means "the rest of this buffer", which
        // gives us a valid run for the trailing child (matches the
        // `clip` walker's tolerance for the same writer shape).
        let span_end = if size == 0 {
            payload.len()
        } else if size < 8 || p + size > payload.len() {
            // Malformed child; stop the walk so the caller still gets
            // the parsed `kmat` if it appeared earlier.
            break;
        } else {
            p + size
        };
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&payload[p + 4..p + 8]);
        if &fc == b"kmat" {
            let body = &payload[p + 8..span_end];
            let parsed = parse_kmat(body)?;
            // First-wins on duplicates: §"Track Matte Atoms" diagrams a
            // single `kmat` child; the spec does not define merge
            // semantics so we preserve the first occurrence rather
            // than silently overwriting it (matches the `clip` / `mvhd`
            // / `pdin` / `ctab` conservative-merge convention).
            if compressed.is_none() {
                compressed = Some(parsed);
            }
        }
        p = span_end;
    }
    let compressed = compressed
        .ok_or_else(|| Error::invalid("MOV: matt atom contains no kmat child (QTFF p. 44)"))?;
    Ok(Matte { compressed })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal image description structure (QTFF p. 70): a
    /// 16-byte stub with the caller-supplied codec FourCC. The body
    /// after the universal 16-byte header is zero-padded out to
    /// `extra_size` extra bytes when requested; nothing in the parser
    /// inspects those bytes.
    fn build_image_description(fourcc: &[u8; 4], extra_size: usize) -> Vec<u8> {
        let total = MIN_IMAGE_DESCRIPTION_SIZE as usize + extra_size;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(total as u32).to_be_bytes()); // size
        out.extend_from_slice(fourcc); // data format
        out.extend_from_slice(&[0u8; 6]); // reserved
        out.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        out.extend(std::iter::repeat(0u8).take(extra_size));
        out
    }

    /// Build a `kmat` body (post-atom-header) with the given version,
    /// flags, image description structure and trailing matte data.
    fn build_kmat_body(version: u8, flags: u32, image_desc: &[u8], matte_data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + image_desc.len() + matte_data.len());
        out.push(version);
        out.push(((flags >> 16) & 0xff) as u8);
        out.push(((flags >> 8) & 0xff) as u8);
        out.push((flags & 0xff) as u8);
        out.extend_from_slice(image_desc);
        out.extend_from_slice(matte_data);
        out
    }

    /// Build a `matt` wrapper body carrying a single `kmat` child.
    fn build_matt_body(kmat_body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        push_atom(&mut out, *b"kmat", kmat_body);
        out
    }

    fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
        let size: u32 = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(body);
    }

    #[test]
    fn kmat_minimum_image_description_round_trips() {
        // Smallest spec-legal description (16 bytes, no extensions);
        // 4 bytes of opaque matte data follow.
        let image_desc = build_image_description(b"raw ", 0);
        let matte_data = [0xAA, 0xBB, 0xCC, 0xDD];
        let body = build_kmat_body(0, 0, &image_desc, &matte_data);
        let parsed = parse_kmat(&body).unwrap();
        assert_eq!(parsed.version, 0);
        assert_eq!(parsed.flags, 0);
        assert_eq!(parsed.image_description, image_desc);
        assert_eq!(parsed.matte_data, matte_data);
        assert_eq!(parsed.data_format(), Some(*b"raw "));
        assert_eq!(parsed.image_description_size(), 16);
    }

    #[test]
    fn kmat_with_extended_image_description_carves_correctly() {
        // 16-byte universal header + 70-byte trailing payload — the
        // total image description is 86 bytes. The size field at the
        // start tells the parser where matte data begins regardless
        // of the per-codec extension shape.
        let image_desc = build_image_description(b"jpeg", 70);
        assert_eq!(image_desc.len(), 86);
        let matte_data: Vec<u8> = (0..32).collect();
        let body = build_kmat_body(0, 0, &image_desc, &matte_data);
        let parsed = parse_kmat(&body).unwrap();
        assert_eq!(parsed.image_description_size(), 86);
        assert_eq!(parsed.matte_data, matte_data);
        assert_eq!(parsed.data_format(), Some(*b"jpeg"));
    }

    #[test]
    fn kmat_with_no_matte_data_accepted() {
        // The spec's "Variable" matte data field has no lower bound;
        // an empty tail (image description only) is a valid kmat body.
        let image_desc = build_image_description(b"alis", 0);
        let body = build_kmat_body(0, 0, &image_desc, &[]);
        let parsed = parse_kmat(&body).unwrap();
        assert!(parsed.matte_data.is_empty());
        assert_eq!(parsed.image_description, image_desc);
    }

    #[test]
    fn kmat_rejects_short_payload() {
        let body = [0u8; 7];
        let err = parse_kmat(&body).unwrap_err();
        assert!(format!("{err}").contains("kmat payload too short"));
    }

    #[test]
    fn kmat_rejects_unknown_version() {
        let image_desc = build_image_description(b"raw ", 0);
        let body = build_kmat_body(1, 0, &image_desc, &[]);
        let err = parse_kmat(&body).unwrap_err();
        assert!(format!("{err}").contains("kmat unknown version 1"));
    }

    #[test]
    fn kmat_rejects_non_zero_flags() {
        let image_desc = build_image_description(b"raw ", 0);
        let body = build_kmat_body(0, 0x000123, &image_desc, &[]);
        let err = parse_kmat(&body).unwrap_err();
        // Flags expression in error formats as 0x0123 via {:#06x}.
        assert!(format!("{err}").contains("kmat flags="));
    }

    #[test]
    fn kmat_rejects_image_description_below_minimum() {
        // Hand-craft a body whose size word claims 15 — one byte below
        // the QTFF p. 70 minimum.
        let mut body = Vec::new();
        body.push(0); // version
        body.extend_from_slice(&[0, 0, 0]); // flags
        body.extend_from_slice(&15u32.to_be_bytes()); // bad size
        body.extend(std::iter::repeat(0u8).take(15));
        let err = parse_kmat(&body).unwrap_err();
        assert!(format!("{err}").contains("image-description size 15"));
    }

    #[test]
    fn kmat_rejects_image_description_overshooting_payload() {
        // Size word claims a description longer than what's left in
        // the body.
        let mut body = Vec::new();
        body.push(0);
        body.extend_from_slice(&[0, 0, 0]);
        body.extend_from_slice(&32u32.to_be_bytes()); // claims 32 bytes
        body.extend(std::iter::repeat(0u8).take(20)); // only 20 follow
        let err = parse_kmat(&body).unwrap_err();
        assert!(format!("{err}").contains("exceeds body remainder"));
    }

    #[test]
    fn matt_wraps_single_kmat_child() {
        let image_desc = build_image_description(b"smc ", 0);
        let matte = b"\x10\x20\x30\x40";
        let kmat_body = build_kmat_body(0, 0, &image_desc, matte);
        let matt_body = build_matt_body(&kmat_body);
        let parsed = parse_matt(&matt_body).unwrap();
        assert_eq!(parsed.compressed.image_description, image_desc);
        assert_eq!(parsed.compressed.matte_data, matte);
        assert_eq!(parsed.compressed.data_format(), Some(*b"smc "));
    }

    #[test]
    fn matt_first_wins_on_duplicate_kmat() {
        let id_a = build_image_description(b"AAAA", 0);
        let id_b = build_image_description(b"BBBB", 0);
        let kmat_a = build_kmat_body(0, 0, &id_a, b"first");
        let kmat_b = build_kmat_body(0, 0, &id_b, b"second");
        let mut matt_body = Vec::new();
        push_atom(&mut matt_body, *b"kmat", &kmat_a);
        push_atom(&mut matt_body, *b"kmat", &kmat_b);
        let parsed = parse_matt(&matt_body).unwrap();
        assert_eq!(parsed.compressed.data_format(), Some(*b"AAAA"));
        assert_eq!(parsed.compressed.matte_data, b"first");
    }

    #[test]
    fn matt_tolerates_unknown_sibling_atoms() {
        // Forward-compat: a hypothetical future helper child sitting
        // alongside `kmat` must not break the parse.
        let image_desc = build_image_description(b"raw ", 0);
        let kmat_body = build_kmat_body(0, 0, &image_desc, b"data");
        let mut matt_body = Vec::new();
        push_atom(&mut matt_body, *b"xxxx", &[0xCA, 0xFE, 0xBA, 0xBE]);
        push_atom(&mut matt_body, *b"kmat", &kmat_body);
        push_atom(&mut matt_body, *b"yyyy", &[0x01, 0x02]);
        let parsed = parse_matt(&matt_body).unwrap();
        assert_eq!(parsed.compressed.data_format(), Some(*b"raw "));
        assert_eq!(parsed.compressed.matte_data, b"data");
    }

    #[test]
    fn matt_rejects_missing_kmat_child() {
        let mut matt_body = Vec::new();
        push_atom(&mut matt_body, *b"xxxx", &[0u8; 4]);
        let err = parse_matt(&matt_body).unwrap_err();
        assert!(format!("{err}").contains("no kmat child"));
    }

    #[test]
    fn matt_surfaces_inner_kmat_error() {
        // A kmat body with bad version must surface the parse error
        // rather than be silently dropped.
        let image_desc = build_image_description(b"raw ", 0);
        let bad = build_kmat_body(0xFF, 0, &image_desc, &[]);
        let mut matt_body = Vec::new();
        push_atom(&mut matt_body, *b"kmat", &bad);
        assert!(parse_matt(&matt_body).is_err());
    }

    #[test]
    fn matt_handles_size_zero_trailing_child() {
        // §QTFF p. 19 — `size == 0` extends to end-of-parent. A
        // writer that emits a trailing `kmat` with size=0 should
        // still be recoverable.
        let image_desc = build_image_description(b"raw ", 0);
        let kmat_body = build_kmat_body(0, 0, &image_desc, b"tail");
        let mut matt_body = Vec::new();
        matt_body.extend_from_slice(&0u32.to_be_bytes()); // size=0
        matt_body.extend_from_slice(b"kmat");
        matt_body.extend_from_slice(&kmat_body);
        let parsed = parse_matt(&matt_body).unwrap();
        assert_eq!(parsed.compressed.matte_data, b"tail");
    }

    #[test]
    fn kmat_to_body_bytes_is_parse_inverse() {
        let image_desc = build_image_description(b"png ", 4);
        let cm = CompressedMatte {
            version: 0,
            flags: 0,
            image_description: image_desc,
            matte_data: vec![1, 2, 3, 4, 5],
        };
        let body = cm.to_body_bytes();
        assert_eq!(parse_kmat(&body).unwrap(), cm);
    }

    #[test]
    fn kmat_to_body_bytes_empty_matte_data() {
        let cm = CompressedMatte {
            version: 0,
            flags: 0,
            image_description: build_image_description(b"raw ", 0),
            matte_data: Vec::new(),
        };
        let body = cm.to_body_bytes();
        let reparsed = parse_kmat(&body).unwrap();
        assert_eq!(reparsed, cm);
        assert_eq!(reparsed.data_format(), Some(*b"raw "));
        assert!(reparsed.matte_data.is_empty());
    }

    #[test]
    fn matt_to_body_bytes_is_parse_inverse() {
        let matte = Matte {
            compressed: CompressedMatte {
                version: 0,
                flags: 0,
                image_description: build_image_description(b"jpeg", 8),
                matte_data: vec![0xAA; 16],
            },
        };
        let body = matte.to_body_bytes();
        assert_eq!(parse_matt(&body).unwrap(), matte);
    }
}
