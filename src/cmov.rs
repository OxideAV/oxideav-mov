//! Compressed Movie atom (`cmov`) and its two subatoms — the Data
//! Compression atom (`dcom`) and the Compressed Movie Data atom
//! (`cmvd`).
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01)
//! §"Compressed Movie Resources" (pp. 80–81; Table 2-5). Beginning with
//! QuickTime 3, a writer may losslessly compress the movie resource
//! itself; the resulting file's top-level `moov` atom carries a single
//! child `cmov` instead of the usual `mvhd` + per-track structure
//! (QTFF p. 80, line "the movie atom contains only a single child atom —
//! the compressed movie atom").
//!
//! ## On-disk layout (QTFF Table 2-5, p. 81)
//!
//! ```text
//! Movie atom ('moov')
//!   Compressed movie atom ('cmov')
//!     Data compression atom ('dcom')
//!       4 bytes  Compression algorithm FourCC
//!     Compressed movie data atom ('cmvd')
//!       4 bytes  Uncompressed size (u32 big-endian)
//!       N bytes  Compressed movie-resource bytes
//! ```
//!
//! ## Scope of this module
//!
//! The parsers surface the **on-disk structure** of all three atoms,
//! and [`Cmov::decompress`] performs the decompression step for the
//! conventional `'zlib'` algorithm through the workspace's `compcol`
//! crate (RFC 1950 stream). The `dcom` four-character code (commonly
//! `'zlib'` in field-observed files, but the spec does not mandate any
//! particular value) selects the decompressor; a FourCC this module
//! does not implement surfaces as an error carrying the verbatim
//! bytes, so a caller with its own decompressor can still drive
//! [`Cmvd::compressed_data`] directly. The writer-side counterparts
//! ([`compress`] / [`Cmov::to_body_bytes`]) build a spec-shaped `cmov`
//! body from an uncompressed movie resource so the pair round-trips.
//!
//! ## QTFF lineage and ISO BMFF
//!
//! ISO/IEC 14496-12 (ISO BMFF) does not define a compressed-movie
//! resource — `cmov`/`dcom`/`cmvd` are QuickTime-only. A plain MP4 /
//! fMP4 / HEIF / AVIF file will never carry these atoms and the parser
//! is reachable only from a `moov` walker that elects to inspect them.
//!
//! ## Validation
//!
//! Per spec:
//!
//! * `dcom` body is exactly 4 bytes — a single FourCC algorithm
//!   identifier (QTFF p. 81). Anything longer or shorter is a writer
//!   error: the field is fixed-width with no list or trailing data.
//! * `cmvd` body is at least 4 bytes — the leading 32-bit uncompressed
//!   size word (QTFF p. 81). A body shorter than 4 bytes cannot encode
//!   the size and is malformed; the trailing compressed-data run may
//!   legitimately be zero bytes only when the uncompressed size is also
//!   zero (an empty movie atom that nominally round-trips through the
//!   compressor).
//! * `cmov` must contain exactly one `dcom` and exactly one `cmvd`
//!   child per Table 2-5. Unknown sibling atoms inside `cmov` are
//!   tolerated but ignored, matching how every other QTFF container in
//!   this crate handles forward-compat children.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// On-disk byte length of a `dcom` body — a single 4-byte FourCC
/// identifying the compression algorithm (QTFF p. 81, Table 2-5).
/// Used as both the minimum and maximum: `dcom` is fixed-width with
/// no trailing data, so any deviation is a writer error.
pub const DCOM_BODY_LEN: usize = 4;

/// Minimum on-disk byte length of a `cmvd` body — the leading 4-byte
/// big-endian uncompressed size word (QTFF p. 81). The trailing
/// compressed-data run is variable-length and may be empty.
pub const CMVD_MIN_BODY_LEN: usize = 4;

/// Conventional `dcom` algorithm FourCC for zlib-compressed movie
/// resources. The QTFF spec (p. 81) names the field as a generic
/// "lossless data compression algorithm" identifier and does **not**
/// mandate any particular value; `'zlib'` is the value observed in
/// field-encountered files and is exposed here as a constant so a
/// caller comparing against it does not have to hand-build the byte
/// literal. The parser preserves whatever FourCC the writer supplied
/// without validating it against this list.
pub const DCOM_ALG_ZLIB: [u8; 4] = *b"zlib";

/// Parsed Data Compression atom (`dcom`, QTFF p. 81).
///
/// Carries the 4-byte FourCC that names the lossless compression
/// algorithm used for the matching `cmvd` payload. The spec does not
/// enumerate legal values; the parser surfaces the bytes verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dcom {
    /// FourCC of the compression algorithm. Compared with
    /// [`DCOM_ALG_ZLIB`] to recognise the canonical zlib variant.
    pub algorithm: [u8; 4],
}

impl Dcom {
    /// `true` when [`Self::algorithm`] matches the conventional
    /// `'zlib'` identifier ([`DCOM_ALG_ZLIB`]).
    pub fn is_zlib(&self) -> bool {
        self.algorithm == DCOM_ALG_ZLIB
    }
}

/// Parsed Compressed Movie Data atom (`cmvd`, QTFF p. 81).
///
/// Carries the declared uncompressed size of the original movie
/// resource and the bytes of the compressed payload that, once fed to
/// the algorithm named by the sibling [`Dcom`], reproduce the
/// uncompressed `moov` resource.
///
/// The parser **does not decompress**: see the module-level docs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmvd {
    /// Declared uncompressed byte length of the wrapped movie resource
    /// (QTFF p. 81: "The first 32-bit integer in the compressed movie
    /// data atom indicates the uncompressed size of the movie
    /// resource"). A decompressor is expected to allocate a buffer of
    /// at least this size; a writer that under-declares is malformed
    /// but the parser leaves that check to whoever invokes the
    /// decompressor (the truth is in the algorithm's own output, not
    /// in this field).
    pub uncompressed_size: u32,
    /// Compressed movie-resource bytes — exactly the tail of the
    /// `cmvd` body after the leading 4-byte size word. Empty when the
    /// writer round-trips a nominally-empty movie. Opaque to this
    /// parser; the decompressor named by the sibling `dcom` interprets
    /// these bytes.
    pub compressed_data: Vec<u8>,
}

impl Cmvd {
    /// Length in bytes of the on-disk compressed payload (the tail of
    /// the `cmvd` body after the leading 4-byte size word). Equivalent
    /// to `compressed_data.len()` and surfaced as a named accessor for
    /// symmetry with [`Cmvd::uncompressed_size`].
    pub fn compressed_size(&self) -> usize {
        self.compressed_data.len()
    }
}

impl Cmov {
    /// Decompress the wrapped movie resource (QTFF pp. 80 – 81).
    ///
    /// Dispatches on the [`Dcom::algorithm`] FourCC; the only
    /// algorithm implemented is the conventional `'zlib'`
    /// ([`DCOM_ALG_ZLIB`], RFC 1950) — any other FourCC returns
    /// `Error::invalid` carrying the verbatim bytes so the caller can
    /// route [`Cmvd::compressed_data`] to its own decompressor.
    ///
    /// Bounded against decompression bombs: the decoder refuses to
    /// produce more than the declared [`Cmvd::uncompressed_size`]
    /// bytes (QTFF p. 81: "The first 32-bit integer in the compressed
    /// movie data atom indicates the uncompressed size of the movie
    /// resource"), and a decoded stream whose length does not equal
    /// that declaration is rejected as a writer error — the size word
    /// exists precisely so a reader can pre-validate the output.
    ///
    /// On success the returned bytes are the complete uncompressed
    /// movie resource — per QTFF p. 30 ("When this child atom is
    /// uncompressed, its contents conform to the structure shown in
    /// the following illustration"), a full `moov` atom including its
    /// own size/type header.
    pub fn decompress(&self) -> Result<Vec<u8>> {
        if !self.dcom.is_zlib() {
            return Err(Error::invalid(format!(
                "MOV: cmov dcom algorithm '{}' is not implemented (only 'zlib'; QTFF p. 81 \
                 names the field generically)",
                String::from_utf8_lossy(&self.dcom.algorithm)
            )));
        }
        let declared = self.cmvd.uncompressed_size as u64;
        let decoded = compcol::vec::decompress_to_vec_capped::<compcol::zlib::Zlib>(
            &self.cmvd.compressed_data,
            declared,
        )
        .map_err(|e| Error::invalid(format!("MOV: cmvd zlib decompression failed: {e}")))?;
        if decoded.len() as u64 != declared {
            return Err(Error::invalid(format!(
                "MOV: cmvd decompressed to {} bytes but declared uncompressed size is {declared} \
                 (QTFF p. 81)",
                decoded.len()
            )));
        }
        Ok(decoded)
    }

    /// Serialize this `cmov`'s **body** — the two child atoms (`dcom`
    /// then `cmvd`, the QTFF Table 2-5 order) each wrapped in the
    /// standard 8-byte size/type header. The caller wraps the result
    /// in the `cmov` atom header and that in turn in the outer `moov`
    /// header to produce the complete compressed-movie layout of
    /// QTFF p. 81. Round-trips through [`parse_cmov`].
    pub fn to_body_bytes(&self) -> Vec<u8> {
        let cmvd_body_len = CMVD_MIN_BODY_LEN + self.cmvd.compressed_data.len();
        let mut out = Vec::with_capacity(8 + DCOM_BODY_LEN + 8 + cmvd_body_len);
        // dcom — fixed 12-byte atom (8-byte header + 4-byte FourCC).
        out.extend_from_slice(&((8 + DCOM_BODY_LEN) as u32).to_be_bytes());
        out.extend_from_slice(b"dcom");
        out.extend_from_slice(&self.dcom.algorithm);
        // cmvd — 8-byte header + 4-byte size word + compressed run.
        out.extend_from_slice(&((8 + cmvd_body_len) as u32).to_be_bytes());
        out.extend_from_slice(b"cmvd");
        out.extend_from_slice(&self.cmvd.uncompressed_size.to_be_bytes());
        out.extend_from_slice(&self.cmvd.compressed_data);
        out
    }
}

/// Compress an uncompressed movie resource into a [`Cmov`] using the
/// conventional `'zlib'` algorithm (QTFF p. 81; RFC 1950 via the
/// workspace's `compcol` crate) — the writer-side counterpart of
/// [`Cmov::decompress`].
///
/// `movie_resource` is the complete uncompressed movie resource (per
/// QTFF p. 30, a full `moov` atom including its own header). Returns
/// `Error::invalid` when the input exceeds `u32::MAX` bytes — the
/// `cmvd` size word is a 32-bit field (QTFF p. 81) and cannot declare
/// a larger resource.
pub fn compress(movie_resource: &[u8]) -> Result<Cmov> {
    let uncompressed_size = u32::try_from(movie_resource.len()).map_err(|_| {
        Error::invalid(format!(
            "MOV: movie resource {} bytes exceeds the 32-bit cmvd uncompressed-size field \
             (QTFF p. 81)",
            movie_resource.len()
        ))
    })?;
    let compressed_data = compcol::vec::compress_to_vec::<compcol::zlib::Zlib>(movie_resource)
        .map_err(|e| Error::invalid(format!("MOV: cmvd zlib compression failed: {e}")))?;
    Ok(Cmov {
        dcom: Dcom {
            algorithm: DCOM_ALG_ZLIB,
        },
        cmvd: Cmvd {
            uncompressed_size,
            compressed_data,
        },
    })
}

/// Parsed Compressed Movie atom (`cmov`, QTFF p. 81).
///
/// Container; carries one [`Dcom`] (the algorithm FourCC) and one
/// [`Cmvd`] (the size + compressed-data pair). Surfacing both as named
/// fields lets a caller hand `(cmov.dcom.algorithm, cmov.cmvd)` to a
/// downstream decompressor in a single move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmov {
    /// Data Compression atom (QTFF p. 81). Required — every
    /// spec-conformant `cmov` carries exactly one.
    pub dcom: Dcom,
    /// Compressed Movie Data atom (QTFF p. 81). Required — every
    /// spec-conformant `cmov` carries exactly one.
    pub cmvd: Cmvd,
}

/// Parse a `dcom` body (the bytes between the atom header and its
/// trailing edge — the caller has already consumed the 8-byte `[size,
/// type]` header).
///
/// Layout per QTFF p. 81: a single 4-byte FourCC. Returns
/// `Error::invalid` when the payload's length is not exactly
/// [`DCOM_BODY_LEN`] — the field is fixed-width with no list and no
/// variable section. The FourCC itself is not validated against any
/// enumeration; the spec names the field generically and the parser
/// carries the bytes through to [`Dcom::algorithm`] verbatim.
pub fn parse_dcom(payload: &[u8]) -> Result<Dcom> {
    if payload.len() != DCOM_BODY_LEN {
        return Err(Error::invalid(format!(
            "MOV: dcom body {} != expected {DCOM_BODY_LEN} bytes (QTFF p. 81)",
            payload.len()
        )));
    }
    let algorithm = [payload[0], payload[1], payload[2], payload[3]];
    Ok(Dcom { algorithm })
}

/// Parse a `cmvd` body (the bytes between the atom header and its
/// trailing edge — the caller has already consumed the 8-byte `[size,
/// type]` header).
///
/// Layout per QTFF p. 81: a 32-bit big-endian uncompressed size word
/// followed by the compressed payload that runs to the end of the
/// atom. Returns `Error::invalid` when the payload is shorter than
/// [`CMVD_MIN_BODY_LEN`] (4 bytes), which cannot encode the size word.
///
/// The compressed payload is allowed to be empty: a writer that
/// round-trips a nominally-empty movie may emit `uncompressed_size = 0`
/// with no trailing data, and we accept that without rejecting.
pub fn parse_cmvd(payload: &[u8]) -> Result<Cmvd> {
    if payload.len() < CMVD_MIN_BODY_LEN {
        return Err(Error::invalid(format!(
            "MOV: cmvd body {} < minimum {CMVD_MIN_BODY_LEN} bytes (QTFF p. 81)",
            payload.len()
        )));
    }
    let uncompressed_size = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let compressed_data = payload[CMVD_MIN_BODY_LEN..].to_vec();
    Ok(Cmvd {
        uncompressed_size,
        compressed_data,
    })
}

/// Parse a `cmov` body — the payload of the wrapper atom, which
/// contains a single `dcom` and a single `cmvd` child per QTFF p. 81
/// Table 2-5.
///
/// Returns:
///
/// * `Ok(Cmov)` when both children parse successfully and appear at
///   least once. If a writer duplicates either child, the first
///   occurrence wins (matching the first-wins discipline used by
///   [`crate::clip::parse_clip`] and [`crate::matte::parse_matt`]).
/// * `Error::invalid` when either child is missing or a child's body
///   fails its own size / shape validation.
///
/// Unknown sibling atoms inside `cmov` are tolerated but ignored, in
/// keeping with the forward-compat discipline of every other QTFF
/// container in this crate.
pub fn parse_cmov(payload: &[u8]) -> Result<Cmov> {
    let mut dcom: Option<Dcom> = None;
    let mut cmvd: Option<Cmvd> = None;

    let mut p = 0usize;
    while p + 8 <= payload.len() {
        let size = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]])
            as usize;
        // QTFF p. 19 — `size == 0` extends to end-of-parent. Inside a
        // `cmov` payload that means "the rest of this buffer".
        let span_end = if size == 0 {
            payload.len()
        } else if size < 8 || p + size > payload.len() {
            // Malformed child; stop the walk so the caller still gets
            // whichever child appeared earlier.
            break;
        } else {
            p + size
        };
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&payload[p + 4..p + 8]);
        let body = &payload[p + 8..span_end];
        match &fc {
            b"dcom" => {
                let parsed = parse_dcom(body)?;
                if dcom.is_none() {
                    dcom = Some(parsed);
                }
            }
            b"cmvd" => {
                let parsed = parse_cmvd(body)?;
                if cmvd.is_none() {
                    cmvd = Some(parsed);
                }
            }
            _ => {}
        }
        p = span_end;
    }
    let dcom =
        dcom.ok_or_else(|| Error::invalid("MOV: cmov atom contains no dcom child (QTFF p. 81)"))?;
    let cmvd =
        cmvd.ok_or_else(|| Error::invalid("MOV: cmov atom contains no cmvd child (QTFF p. 81)"))?;
    Ok(Cmov { dcom, cmvd })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap an arbitrary atom body in a 4+4 size/type header for use
    /// inside another container payload.
    fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
        let size: u32 = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(body);
    }

    fn build_cmvd_body(uncompressed_size: u32, compressed: &[u8]) -> Vec<u8> {
        let mut p = Vec::with_capacity(4 + compressed.len());
        p.extend_from_slice(&uncompressed_size.to_be_bytes());
        p.extend_from_slice(compressed);
        p
    }

    #[test]
    fn dcom_zlib_round_trip() {
        let body = b"zlib";
        let dcom = parse_dcom(body).unwrap();
        assert_eq!(dcom.algorithm, *b"zlib");
        assert!(dcom.is_zlib());
    }

    #[test]
    fn dcom_non_zlib_round_trip_with_predicate_false() {
        // QTFF p. 81 names the field generically; any FourCC parses,
        // and the predicate flags non-zlib values for a strict caller.
        let body = b"none";
        let dcom = parse_dcom(body).unwrap();
        assert_eq!(dcom.algorithm, *b"none");
        assert!(!dcom.is_zlib());
    }

    #[test]
    fn dcom_short_body_rejects() {
        let body = vec![0u8; DCOM_BODY_LEN - 1];
        assert!(parse_dcom(&body).is_err());
    }

    #[test]
    fn dcom_long_body_rejects() {
        // 5 bytes — one stray byte past the fixed-width record. `dcom`
        // carries no list, so any tail is malformed.
        let body = vec![0u8; DCOM_BODY_LEN + 1];
        assert!(parse_dcom(&body).is_err());
    }

    #[test]
    fn dcom_empty_body_rejects() {
        let body: Vec<u8> = Vec::new();
        assert!(parse_dcom(&body).is_err());
    }

    #[test]
    fn cmvd_round_trip_with_payload() {
        // Uncompressed size 1024, with 6 bytes of compressed payload.
        let compressed = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x42];
        let body = build_cmvd_body(1024, &compressed);
        let cmvd = parse_cmvd(&body).unwrap();
        assert_eq!(cmvd.uncompressed_size, 1024);
        assert_eq!(cmvd.compressed_data, compressed);
        assert_eq!(cmvd.compressed_size(), 6);
    }

    #[test]
    fn cmvd_empty_compressed_payload_round_trip() {
        // QTFF p. 81: the parser accepts a writer that round-trips a
        // nominally-empty movie. `uncompressed_size = 0` plus an empty
        // compressed-data run is legal on-disk.
        let body = build_cmvd_body(0, &[]);
        let cmvd = parse_cmvd(&body).unwrap();
        assert_eq!(cmvd.uncompressed_size, 0);
        assert!(cmvd.compressed_data.is_empty());
        assert_eq!(cmvd.compressed_size(), 0);
    }

    #[test]
    fn cmvd_short_body_rejects() {
        // 3 bytes — one short of the 4-byte uncompressed-size word.
        let body = vec![0u8; CMVD_MIN_BODY_LEN - 1];
        assert!(parse_cmvd(&body).is_err());
    }

    #[test]
    fn cmvd_empty_body_rejects() {
        let body: Vec<u8> = Vec::new();
        assert!(parse_cmvd(&body).is_err());
    }

    #[test]
    fn cmvd_max_uncompressed_size_round_trips() {
        // QTFF stores the uncompressed size as a u32. Confirm the
        // top-of-range value round-trips without sign confusion.
        let body = build_cmvd_body(u32::MAX, &[0x01, 0x02]);
        let cmvd = parse_cmvd(&body).unwrap();
        assert_eq!(cmvd.uncompressed_size, u32::MAX);
        assert_eq!(cmvd.compressed_data, [0x01, 0x02]);
    }

    /// Build a complete `cmov` *body* — i.e., what the caller passes
    /// to [`parse_cmov`] after consuming the wrapper 8-byte header.
    fn build_cmov_body(
        dcom_algorithm: [u8; 4],
        cmvd_uncompressed_size: u32,
        cmvd_compressed: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        push_atom(&mut out, *b"dcom", &dcom_algorithm);
        let cmvd_body = build_cmvd_body(cmvd_uncompressed_size, cmvd_compressed);
        push_atom(&mut out, *b"cmvd", &cmvd_body);
        out
    }

    #[test]
    fn cmov_canonical_layout_round_trip() {
        // QTFF Table 2-5: 'cmov' → 'dcom' (zlib) + 'cmvd' (size + data).
        let compressed = [0xCA, 0xFE, 0xBA, 0xBE];
        let body = build_cmov_body(DCOM_ALG_ZLIB, 4096, &compressed);
        let cmov = parse_cmov(&body).unwrap();
        assert_eq!(cmov.dcom.algorithm, DCOM_ALG_ZLIB);
        assert!(cmov.dcom.is_zlib());
        assert_eq!(cmov.cmvd.uncompressed_size, 4096);
        assert_eq!(cmov.cmvd.compressed_data, compressed);
    }

    #[test]
    fn cmov_reversed_child_order_round_trips() {
        // QTFF Table 2-5 illustrates dcom first then cmvd, but the
        // spec does not require that order. The parser collects by
        // FourCC, so a writer that emits cmvd before dcom must still
        // round-trip.
        let compressed = [0x00, 0x11, 0x22, 0x33];
        let mut body = Vec::new();
        let cmvd_body = build_cmvd_body(2048, &compressed);
        push_atom(&mut body, *b"cmvd", &cmvd_body);
        push_atom(&mut body, *b"dcom", &DCOM_ALG_ZLIB);
        let cmov = parse_cmov(&body).unwrap();
        assert_eq!(cmov.dcom.algorithm, DCOM_ALG_ZLIB);
        assert_eq!(cmov.cmvd.uncompressed_size, 2048);
        assert_eq!(cmov.cmvd.compressed_data, compressed);
    }

    #[test]
    fn cmov_unknown_sibling_atoms_ignored() {
        // Forward-compat: a `cmov` container may carry extra child
        // FourCCs we don't recognise. The parser must skip them and
        // still surface the known dcom + cmvd pair.
        let mut body = Vec::new();
        push_atom(&mut body, *b"dcom", &DCOM_ALG_ZLIB);
        push_atom(&mut body, *b"xxxx", &[0xAA; 16]);
        let cmvd_body = build_cmvd_body(512, &[0x42; 3]);
        push_atom(&mut body, *b"cmvd", &cmvd_body);
        push_atom(&mut body, *b"yyyy", &[0xBB; 4]);
        let cmov = parse_cmov(&body).unwrap();
        assert_eq!(cmov.dcom.algorithm, DCOM_ALG_ZLIB);
        assert_eq!(cmov.cmvd.uncompressed_size, 512);
        assert_eq!(cmov.cmvd.compressed_data, vec![0x42, 0x42, 0x42]);
    }

    #[test]
    fn cmov_missing_dcom_rejects() {
        // Only a `cmvd` present — `cmov` requires both per QTFF p. 81.
        let mut body = Vec::new();
        let cmvd_body = build_cmvd_body(256, &[]);
        push_atom(&mut body, *b"cmvd", &cmvd_body);
        assert!(parse_cmov(&body).is_err());
    }

    #[test]
    fn cmov_missing_cmvd_rejects() {
        // Only a `dcom` present — `cmov` requires both per QTFF p. 81.
        let mut body = Vec::new();
        push_atom(&mut body, *b"dcom", &DCOM_ALG_ZLIB);
        assert!(parse_cmov(&body).is_err());
    }

    #[test]
    fn cmov_empty_body_rejects() {
        let body: Vec<u8> = Vec::new();
        assert!(parse_cmov(&body).is_err());
    }

    #[test]
    fn cmov_duplicate_child_first_wins() {
        // QTFF Table 2-5 implies a single dcom + single cmvd. The
        // first-wins discipline matches `parse_clip` / `parse_matt` —
        // we preserve the first occurrence rather than silently
        // overwriting it.
        let mut body = Vec::new();
        push_atom(&mut body, *b"dcom", &DCOM_ALG_ZLIB);
        push_atom(&mut body, *b"dcom", b"none"); // duplicate — ignored
        let cmvd_body_a = build_cmvd_body(100, &[0x01]);
        push_atom(&mut body, *b"cmvd", &cmvd_body_a);
        let cmvd_body_b = build_cmvd_body(200, &[0x02]);
        push_atom(&mut body, *b"cmvd", &cmvd_body_b); // duplicate — ignored
        let cmov = parse_cmov(&body).unwrap();
        assert_eq!(cmov.dcom.algorithm, DCOM_ALG_ZLIB);
        assert!(cmov.dcom.is_zlib());
        assert_eq!(cmov.cmvd.uncompressed_size, 100);
        assert_eq!(cmov.cmvd.compressed_data, vec![0x01]);
    }

    #[test]
    fn cmov_child_with_zero_size_extends_to_end() {
        // QTFF p. 19: `size == 0` means "extend to end of parent".
        // Inside a cmov body that's "to end of buffer" — the final
        // child consumes the rest. Confirm we accept this on the
        // trailing cmvd while still surfacing the dcom that came
        // earlier.
        let mut body = Vec::new();
        push_atom(&mut body, *b"dcom", &DCOM_ALG_ZLIB);
        // Open-ended cmvd: size = 0, type = cmvd, then payload to EOF.
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(b"cmvd");
        body.extend_from_slice(&64u32.to_be_bytes()); // uncompressed_size
        body.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // compressed data
        let cmov = parse_cmov(&body).unwrap();
        assert_eq!(cmov.dcom.algorithm, DCOM_ALG_ZLIB);
        assert_eq!(cmov.cmvd.uncompressed_size, 64);
        assert_eq!(cmov.cmvd.compressed_data, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn compress_decompress_round_trips() {
        // Writer-side `compress` (QTFF p. 81 'zlib') feeds the
        // reader-side `Cmov::decompress` and reproduces the input
        // byte-for-byte, with the size word filled in correctly.
        let resource: Vec<u8> = (0u16..512).map(|i| (i % 251) as u8).collect();
        let cmov = compress(&resource).unwrap();
        assert!(cmov.dcom.is_zlib());
        assert_eq!(cmov.cmvd.uncompressed_size as usize, resource.len());
        assert_eq!(cmov.decompress().unwrap(), resource);
    }

    #[test]
    fn to_body_bytes_round_trips_through_parse_cmov() {
        // Serialized body (dcom + cmvd children per Table 2-5)
        // re-parses into an equal Cmov.
        let cmov = compress(b"movie resource bytes").unwrap();
        let body = cmov.to_body_bytes();
        let reparsed = parse_cmov(&body).unwrap();
        assert_eq!(reparsed, cmov);
        assert_eq!(reparsed.decompress().unwrap(), b"movie resource bytes");
    }

    #[test]
    fn decompress_non_zlib_algorithm_rejects() {
        // QTFF p. 81 names the dcom field generically; an algorithm
        // we don't implement must error rather than misinterpret the
        // payload as a zlib stream.
        let mut cmov = compress(b"payload").unwrap();
        cmov.dcom.algorithm = *b"none";
        assert!(cmov.decompress().is_err());
    }

    #[test]
    fn decompress_under_declared_size_rejects_without_unbounded_growth() {
        // A writer (or attacker) that declares a size smaller than
        // the stream actually inflates to must hit the output cap —
        // the declared word bounds the allocation (QTFF p. 81 makes
        // it the authoritative uncompressed size).
        let mut cmov = compress(&vec![0u8; 4096]).unwrap();
        cmov.cmvd.uncompressed_size = 16;
        assert!(cmov.decompress().is_err());
    }

    #[test]
    fn decompress_over_declared_size_rejects() {
        // The dual writer error: a declared size larger than what the
        // stream produces is a length mismatch, not a silent accept.
        let mut cmov = compress(b"twelve bytes").unwrap();
        cmov.cmvd.uncompressed_size += 1;
        assert!(cmov.decompress().is_err());
    }

    #[test]
    fn decompress_corrupt_stream_rejects() {
        // Garbage that is not an RFC 1950 stream errors cleanly.
        let cmov = Cmov {
            dcom: Dcom {
                algorithm: DCOM_ALG_ZLIB,
            },
            cmvd: Cmvd {
                uncompressed_size: 64,
                compressed_data: vec![0xFF, 0x00, 0xAB, 0xCD, 0xEF],
            },
        };
        assert!(cmov.decompress().is_err());
    }

    #[test]
    fn cmov_malformed_child_size_stops_walk_cleanly() {
        // A child claims size = 4 (less than the 8-byte header) — the
        // walker stops at that point. The dcom child we already parsed
        // is preserved; the missing cmvd surfaces as an error rather
        // than a panic.
        let mut body = Vec::new();
        push_atom(&mut body, *b"dcom", &DCOM_ALG_ZLIB);
        // Bogus child: size = 4, type = 'cmvd'. Walker rejects and
        // stops; cmvd is therefore never seen.
        body.extend_from_slice(&4u32.to_be_bytes());
        body.extend_from_slice(b"cmvd");
        assert!(parse_cmov(&body).is_err());
    }
}
