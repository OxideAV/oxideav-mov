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
    /// `urn ` — ISO BMFF Uniform Resource Name (NUL-terminated name +
    /// optional NUL-terminated location, both UTF-8). Round-4 surfaces
    /// the parsed pair; either string may be empty.
    Urn { name: String, location: String },
    /// `alis` — opaque Mac OS alias record.
    Alias(Vec<u8>),
    /// `rsrc` — Mac OS resource-fork reference (opaque).
    Resource(Vec<u8>),
    /// Self-reference: `dref` entry whose `flags & 0x000001 == 1`. The
    /// media bytes are inside the same file as the moov; no body is
    /// stored. Round-4 surfaces this so callers can shortcut alias
    /// resolution when no external lookup is required.
    SelfRef,
    /// Anything else — kept as `(type FourCC, raw body)` for forensics.
    Other([u8; 4], Vec<u8>),
}

/// Parse a `dref` payload (the data-reference atom inside `dinf`).
///
/// Layout per QTFF p. 65 Figure 2-24: `[ver:1][flags:3][n:u32]` then
/// `n` entries, each formatted as a child atom — `[size:u32][type:4]
/// [ver:1][flags:3][data: size-12]`. A child entry's `flags & 0x01`
/// being set marks it a *self-reference* (the media is in the same
/// file); the data slot is then empty by spec, even though some
/// writers still emit the bytes (we accept either shape).
///
/// Standard data-reference types:
///
/// * `url ` — UTF-8 URL, optionally NUL-terminated.
/// * `urn ` — ISO BMFF NUL-terminated name, optional NUL-terminated
///   location. Both decoded as UTF-8.
/// * `alis` — Mac OS alias record (opaque).
/// * `rsrc` — Mac OS resource-fork reference (opaque).
pub fn parse_dref(payload: &[u8]) -> Result<Vec<DataReference>> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: dref payload < 8 bytes"));
    }
    let n = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize;
    // Allocate for the byte-backed entry count, not the declared one:
    // each child reference atom occupies at least 12 bytes (`size >= 12`
    // enforced below), and the lenient loop below already tolerates a
    // count larger than the body holds — but `Vec::with_capacity` must
    // not turn a forged count into a multi-gigabyte allocation.
    let mut out = Vec::with_capacity(n.min((payload.len() - 8) / 12));
    let mut p = 8usize;
    while p < payload.len() && out.len() < n {
        if p + 8 > payload.len() {
            return Err(Error::invalid("MOV: dref entry truncated header"));
        }
        let size = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]])
            as usize;
        if size < 12 || p + size > payload.len() {
            return Err(Error::invalid("MOV: dref entry size invalid"));
        }
        let mut t = [0u8; 4];
        t.copy_from_slice(&payload[p + 4..p + 8]);
        let flags = (payload[p + 9] as u32) << 16
            | (payload[p + 10] as u32) << 8
            | (payload[p + 11] as u32);
        let body = &payload[p + 12..p + size];
        let dref = if flags & 0x01 != 0 {
            // Self-reference: the data slot is *spec'd* to be empty
            // but encoders sometimes still emit the URL pointing at
            // the host file. Either way we return SelfRef.
            DataReference::SelfRef
        } else {
            decode_dref_body(&t, body)
        };
        out.push(dref);
        p += size;
    }
    Ok(out)
}

fn decode_dref_body(t: &[u8; 4], body: &[u8]) -> DataReference {
    match t {
        b"url " => {
            let trimmed = match body.last() {
                Some(0) => &body[..body.len() - 1],
                _ => body,
            };
            match std::str::from_utf8(trimmed) {
                Ok(s) => DataReference::Url(s.to_string()),
                Err(_) => DataReference::Other(*t, body.to_vec()),
            }
        }
        b"urn " => {
            // ISO BMFF §8.7.2: two NUL-terminated UTF-8 strings —
            // `name` (required) followed by `location` (optional).
            let mut split = body.splitn(2, |b| *b == 0);
            let name = split.next().unwrap_or(&[]);
            let location = split.next().unwrap_or(&[]);
            // Trim a trailing NUL on `location` when present.
            let loc_trimmed = match location.last() {
                Some(0) => &location[..location.len() - 1],
                _ => location,
            };
            DataReference::Urn {
                name: std::str::from_utf8(name).unwrap_or("").to_string(),
                location: std::str::from_utf8(loc_trimmed).unwrap_or("").to_string(),
            }
        }
        b"alis" => DataReference::Alias(body.to_vec()),
        b"rsrc" => DataReference::Resource(body.to_vec()),
        _ => DataReference::Other(*t, body.to_vec()),
    }
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

    #[test]
    fn dref_self_reference_url_recognised() {
        // dref: ver+flags=0, n=1; one child: size=12, type='url ',
        // ver=0, flags=0x000001 (self-ref), no body.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&1u32.to_be_bytes()); // n
        let mut child = Vec::new();
        child.extend_from_slice(&12u32.to_be_bytes()); // size
        child.extend_from_slice(b"url ");
        child.push(0); // version
        child.extend_from_slice(&[0, 0, 1]); // flags=0x000001
        p.extend_from_slice(&child);
        let v = parse_dref(&p).unwrap();
        assert_eq!(v.len(), 1);
        assert!(matches!(v[0], DataReference::SelfRef));
    }

    #[test]
    fn dref_external_url_round_trip() {
        // dref: ver+flags=0, n=1; child: type='url ', flags=0, body =
        // "http://example.com/clip.mov\0"
        let url = b"http://example.com/clip.mov\0";
        let mut child = Vec::new();
        let size = 12 + url.len() as u32;
        child.extend_from_slice(&size.to_be_bytes());
        child.extend_from_slice(b"url ");
        child.push(0); // ver
        child.extend_from_slice(&[0, 0, 0]); // flags = 0 (external)
        child.extend_from_slice(url);
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&child);
        let v = parse_dref(&p).unwrap();
        assert_eq!(v.len(), 1);
        match &v[0] {
            DataReference::Url(s) => assert_eq!(s, "http://example.com/clip.mov"),
            other => panic!("expected Url, got {other:?}"),
        }
    }

    #[test]
    fn dref_urn_two_strings_round_trip() {
        // urn payload: "urn:isbn:000\0http://x/loc\0"
        let body = b"urn:isbn:000\0http://x/loc\0";
        let mut child = Vec::new();
        let size = 12 + body.len() as u32;
        child.extend_from_slice(&size.to_be_bytes());
        child.extend_from_slice(b"urn ");
        child.push(0);
        child.extend_from_slice(&[0, 0, 0]);
        child.extend_from_slice(body);
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&child);
        let v = parse_dref(&p).unwrap();
        assert_eq!(v.len(), 1);
        match &v[0] {
            DataReference::Urn { name, location } => {
                assert_eq!(name, "urn:isbn:000");
                assert_eq!(location, "http://x/loc");
            }
            other => panic!("expected Urn, got {other:?}"),
        }
    }

    #[test]
    fn dref_alias_kept_opaque() {
        let body = b"\xCA\xFEALIASBYTES";
        let mut child = Vec::new();
        let size = 12 + body.len() as u32;
        child.extend_from_slice(&size.to_be_bytes());
        child.extend_from_slice(b"alis");
        child.push(0);
        child.extend_from_slice(&[0, 0, 0]);
        child.extend_from_slice(body);
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&child);
        let v = parse_dref(&p).unwrap();
        match &v[0] {
            DataReference::Alias(b) => assert_eq!(b, body),
            other => panic!("expected Alias, got {other:?}"),
        }
    }

    #[test]
    fn dref_truncated_entry_errors() {
        // Declares 2 entries but only carries 1 worth.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&2u32.to_be_bytes()); // n=2
        let mut child = Vec::new();
        child.extend_from_slice(&12u32.to_be_bytes());
        child.extend_from_slice(b"url ");
        child.push(0);
        child.extend_from_slice(&[0, 0, 1]);
        p.extend_from_slice(&child);
        // No second entry.
        let res = parse_dref(&p);
        // Either it returns 1 entry (lenient) or errors. Tighten by
        // checking lt declared count.
        if let Ok(v) = res {
            assert!(v.len() < 2);
        }
    }
}
