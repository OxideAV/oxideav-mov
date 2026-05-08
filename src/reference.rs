//! Apple QuickTime "reference movies" (`rmra` / `rmda`).
//!
//! A *reference movie* is a thin `.mov` whose `moov` contains an `rmra`
//! atom describing one or more *external* movies via Apple alias
//! records or URLs. The `mdat` either lives in the referenced file or
//! is absent entirely. Players resolve the alias chain at open time.
//!
//! On-disk layout (QTFF "Reference Movies" §):
//!
//! ```text
//! moov / rmra (container)
//!   rmda (container, repeated)
//!     rdrf  — data reference: [ver+flags=4][type:4][size:4][data: size]
//!     rmdr  — data rate qualifier: [ver+flags=4][rate:4]
//!     rmqu  — quality qualifier: [ver+flags=4][quality:4]
//!     rmcs  — CPU speed qualifier
//!     rmvc  — version-check qualifier
//!     rmcd  — codec qualifier
//! ```
//!
//! `rdrf`'s 4-byte type FourCC discriminates the reference payload:
//!
//! * `url ` — a NUL-terminated UTF-8 URL.
//! * `alis` — a Mac OS alias record (binary blob, opaque to us).
//! * `rsrc` — a Mac OS resource fork reference.
//!
//! We don't follow the references — round-3 surfaces the parsed
//! descriptors and the demuxer raises an `Unsupported` error when an
//! `rmra` is the only thing in `moov` (i.e. no in-file `mdat`).

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// One alternate-data reference (`rmda` payload).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReferenceMovie {
    /// Parsed `rdrf` data reference (the alias or URL pointing at the
    /// external media).
    pub data_ref: Option<DataReference>,
    /// `rmdr` data-rate qualifier (bps); `None` when absent.
    pub min_data_rate: Option<u32>,
    /// `rmqu` quality qualifier; `None` when absent. Higher = better.
    pub quality: Option<u32>,
    /// `rmcs` CPU-speed qualifier (relative units).
    pub cpu_speed: Option<u32>,
    /// `rmvc` version-check qualifier (raw 12-byte body kept since the
    /// gestalt-based selector requires Mac-specific knowledge).
    pub version_check: Option<Vec<u8>>,
    /// `rmcd` codec qualifier (4-byte FourCC).
    pub codec_check: Option<[u8; 4]>,
}

/// Decoded `rdrf` payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataReference {
    /// `url ` — NUL-terminated UTF-8 URL.
    Url(String),
    /// `alis` — opaque Mac OS alias record.
    Alias(Vec<u8>),
    /// `rsrc` — Mac OS resource-fork reference (opaque).
    Resource(Vec<u8>),
    /// Anything else — kept as `(type FourCC, raw body)` for forensics.
    Other([u8; 4], Vec<u8>),
}

/// Parse an `rdrf` payload. Layout per QTFF "Data Reference Atom":
/// `[ver+flags=4][type:4][data_size:4][data: data_size]`.
pub fn parse_rdrf(payload: &[u8]) -> Result<DataReference> {
    if payload.len() < 12 {
        return Err(Error::invalid("MOV: rdrf payload < 12 bytes"));
    }
    let mut t = [0u8; 4];
    t.copy_from_slice(&payload[4..8]);
    let size = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]) as usize;
    if size > payload.len() - 12 {
        return Err(Error::invalid("MOV: rdrf data size beyond payload"));
    }
    let body = &payload[12..12 + size];
    Ok(match &t {
        b"url " => {
            // Strip a single trailing NUL the encoder may have appended.
            let trimmed = match body.last() {
                Some(0) => &body[..body.len() - 1],
                _ => body,
            };
            let s = std::str::from_utf8(trimmed)
                .map_err(|_| Error::invalid("MOV: rdrf URL not UTF-8"))?
                .to_string();
            DataReference::Url(s)
        }
        b"alis" => DataReference::Alias(body.to_vec()),
        b"rsrc" => DataReference::Resource(body.to_vec()),
        _ => DataReference::Other(t, body.to_vec()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rdrf_url_parses_and_strips_trailing_nul() {
        let url = b"http://example.com/foo.mov\0";
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(b"url ");
        p.extend_from_slice(&(url.len() as u32).to_be_bytes());
        p.extend_from_slice(url);
        match parse_rdrf(&p).unwrap() {
            DataReference::Url(s) => assert_eq!(s, "http://example.com/foo.mov"),
            _ => panic!("expected URL"),
        }
    }

    #[test]
    fn rdrf_alis_kept_opaque() {
        let body = b"\xDE\xAD\xBE\xEF";
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"alis");
        p.extend_from_slice(&(body.len() as u32).to_be_bytes());
        p.extend_from_slice(body);
        match parse_rdrf(&p).unwrap() {
            DataReference::Alias(b) => assert_eq!(b, body),
            _ => panic!("expected Alias"),
        }
    }

    #[test]
    fn rdrf_too_short_errors() {
        assert!(parse_rdrf(&[0u8; 8]).is_err());
    }
}
