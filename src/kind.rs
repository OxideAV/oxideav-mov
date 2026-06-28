//! Track Kind box (`kind`).
//!
//! ISO/IEC 14496-12 §8.10.4 ("Track kind", p. 74 of the 2015 edition).
//! `kind` labels a track with its **role** or **kind** — a semantic
//! descriptor distinct from the codec, the media-handler component
//! subtype, and the `tkhd.alternate_group` ranking. The canonical use
//! case is signalling the intent of a subtitle / caption track
//! ("captions", "subtitles", "descriptions", "metadata", …) using one
//! of the WebVTT or DASH role schemes (RFC 8216, MPEG-DASH).
//!
//! Layout per §8.10.4.2:
//!
//! ```text
//! aligned(8) class KindBox
//!   extends FullBox('kind', version = 0, 0) {
//!     string  schemeURI;
//!     string  value;
//! }
//! ```
//!
//! `schemeURI` and `value` are both NULL-terminated C strings
//! (§8.10.4.3 — the spec is explicit). The two fields together form a
//! `(scheme, value)` pair: when only the `schemeURI` is meaningful the
//! `value` is left empty (the on-disk encoding is then `[uri]\0\0`).
//! When `value` is non-empty the `schemeURI` identifies the **naming
//! scheme** the value is drawn from. The spec explicitly tolerates more
//! than one `kind` per track (Quantity: Zero or more) so a producer can
//! attach the same track to several role taxonomies in one file
//! (§8.10.4.1 — "More than one of these may occur in a track, with
//! different contents but with appropriate semantics").
//!
//! The container is the **track-level** `udta`
//! (`moov/trak/udta/kind`). QTFF (the Apple ancestor of ISO BMFF) does
//! not define this box; it is ISO BMFF-only and stays absent for plain
//! `.mov` inputs.
//!
//! ## Behavioural decisions
//!
//! * Both strings are decoded as UTF-8 (best-effort). The spec doesn't
//!   pin a character encoding for `string` — RFC 4646 / BCP 47 (the
//!   canonical scheme for subtitle role tags) restricts itself to ASCII
//!   anyway. Bytes that fail UTF-8 are surfaced via
//!   `String::from_utf8_lossy`, replacing each malformed sequence with
//!   U+FFFD rather than rejecting the box outright (the role-tag layer
//!   degrades gracefully when an opaque vendor scheme uses non-UTF-8).
//! * A `value` field that consists solely of its NULL terminator (the
//!   common shape when only `schemeURI` matters) is surfaced as `None`
//!   rather than `Some("")`. Callers can dispatch on the URI alone
//!   without first checking for emptiness.
//! * A missing trailing NULL on either string is accepted: the field
//!   simply runs to the end of its slice (the closing NULL is a
//!   convention, not a hard structural marker — without it we'd reject
//!   real files that omit it). The opening boundary on `value`, when
//!   `schemeURI` ran without a NULL, falls at the end of the box —
//!   yielding `None` for `value`.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Parsed `kind` Track Kind box (ISO/IEC 14496-12 §8.10.4).
///
/// Two fields per §8.10.4.2: a `schemeURI` (always present) and an
/// optional `value` (a name within that scheme).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindEntry {
    /// `schemeURI` per §8.10.4.3 — identifier of either the kind itself
    /// (when `value` is `None`) or the naming scheme `value` is drawn
    /// from. The most common modern shapes are WebVTT role URIs
    /// (e.g. `https://www.w3.org/TR/webvtt1/`) and DASH role URIs
    /// (e.g. `urn:mpeg:dash:role:2011`).
    pub scheme_uri: String,
    /// `value` per §8.10.4.3 — a name drawn from the scheme identified
    /// by `scheme_uri`. `None` when the box carries only a `schemeURI`
    /// (the on-disk shape is `[uri]\0\0`, with `value` being just its
    /// terminator).
    pub value: Option<String>,
}

impl KindEntry {
    /// True when `value` is `Some` and non-empty. Convenience for
    /// callers that want to distinguish "URI-only kind" from "scheme
    /// + named value" without re-checking emptiness.
    pub fn has_value(&self) -> bool {
        self.value.as_ref().is_some_and(|v| !v.is_empty())
    }

    /// Serialise into the `kind` FullBox body (ISO/IEC 14496-12
    /// §8.10.4.2) — the inverse of [`parse_kind`]. Layout:
    /// `[version=0:1][flags=0:3][schemeURI\0][value\0]`. Both strings are
    /// NUL-terminated; a `None` / empty `value` emits a bare terminator
    /// (`[uri]\0\0`), which `parse_kind` reads back as `value == None`.
    ///
    /// The `schemeURI` must not contain an embedded NUL (it would split
    /// the field on read); callers building a `KindEntry` from a role-tag
    /// string never introduce one.
    pub fn to_body_bytes(&self) -> Vec<u8> {
        let value = self.value.as_deref().unwrap_or("");
        let mut p = Vec::with_capacity(4 + self.scheme_uri.len() + value.len() + 2);
        p.extend_from_slice(&[0, 0, 0, 0]); // version 0 + flags 0
        p.extend_from_slice(self.scheme_uri.as_bytes());
        p.push(0);
        p.extend_from_slice(value.as_bytes());
        p.push(0);
        p
    }
}

/// Parse a `kind` Track Kind box payload (ISO/IEC 14496-12 §8.10.4.2).
///
/// Expects the FullBox header (`[version:1][flags:3]`) followed by two
/// NULL-terminated C strings: `schemeURI` and `value`. Returns:
///
/// * `Error::invalid` when the payload is shorter than the 4-byte
///   FullBox header.
/// * `Error::invalid` when the FullBox version field is non-zero
///   (§8.10.4.2 declares `version = 0`; future versions would change
///   the layout and we'd rather refuse than silently misparse).
///
/// FullBox flags are accepted and ignored: §8.10.4.2 fixes them at `0`
/// but real-world tolerance for arbitrary flag bits is consistent with
/// how this crate treats every other FullBox (see `parse_tsel`).
///
/// A missing trailing NULL on either string is accepted (see module
/// docs for the rationale).
pub fn parse_kind(payload: &[u8]) -> Result<KindEntry> {
    if payload.len() < 4 {
        return Err(Error::invalid(format!(
            "MOV: kind payload {} < 4 bytes",
            payload.len()
        )));
    }
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "MOV: kind version {} != 0",
            version
        )));
    }
    // payload[1..4] = flags (ignored).
    let tail = &payload[4..];
    let (scheme_bytes, after_scheme) = take_cstring(tail);
    let scheme_uri = String::from_utf8_lossy(scheme_bytes).into_owned();
    let value = if after_scheme.is_empty() {
        None
    } else {
        let (value_bytes, _) = take_cstring(after_scheme);
        if value_bytes.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(value_bytes).into_owned())
        }
    };
    Ok(KindEntry { scheme_uri, value })
}

/// Split a slice at the first NULL byte. Returns `(prefix_without_nul,
/// remainder_after_nul)`. When no NULL is present, the whole slice is
/// returned as the prefix and the remainder is empty.
fn take_cstring(b: &[u8]) -> (&[u8], &[u8]) {
    match b.iter().position(|&c| c == 0) {
        Some(i) => (&b[..i], &b[i + 1..]),
        None => (b, &[][..]),
    }
}

/// Scan a raw `udta` payload for every `kind` child and parse each.
///
/// `udta` is a flat atom list (§8.10.1) of `[size:4][type:4][body]`
/// records. §8.10.4.1 declares `Quantity: Zero or more` for `kind`, so
/// this returns every `kind` entry in file order (rather than the
/// first-match shape used by `find_tsel_in_udta`, since `tsel` is
/// `Quantity: Zero or one`). A truncated or malformed `kind` body
/// surfaces the parse error; truncated *other* children stop the walk
/// (mirroring `find_tsel_in_udta`) so an unrelated bad entry doesn't
/// silently swallow `kind` entries that follow.
pub fn find_kinds_in_udta(udta_payload: &[u8]) -> Result<Vec<KindEntry>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= udta_payload.len() {
        let size = u32::from_be_bytes([
            udta_payload[p],
            udta_payload[p + 1],
            udta_payload[p + 2],
            udta_payload[p + 3],
        ]) as usize;
        // §8.10.1 / QTFF p. 37: udta may be terminated by a 32-bit
        // zero. Treat that as end-of-list.
        if size == 0 && p + 4 == udta_payload.len() {
            break;
        }
        if size < 8 || p + size > udta_payload.len() {
            // Malformed entry — stop walking; the rest of the buffer
            // is untrustworthy.
            break;
        }
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&udta_payload[p + 4..p + 8]);
        if &fc == b"kind" {
            let body = &udta_payload[p + 8..p + size];
            out.push(parse_kind(body)?);
        }
        p += size;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_kind(version: u8, flags: u32, scheme: &str, value: Option<&str>) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(version);
        let f = flags.to_be_bytes();
        p.extend_from_slice(&f[1..4]);
        p.extend_from_slice(scheme.as_bytes());
        p.push(0);
        if let Some(v) = value {
            p.extend_from_slice(v.as_bytes());
            p.push(0);
        } else {
            // §8.10.4.2 — `value` is still emitted as a (possibly empty)
            // NULL-terminated string when the box carries no value.
            p.push(0);
        }
        p
    }

    fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
        let size: u32 = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(body);
    }

    #[test]
    fn scheme_only_round_trip() {
        // The "URI identifies the kind itself" shape — empty value.
        let p = build_kind(0, 0, "urn:mpeg:dash:role:2011", None);
        let k = parse_kind(&p).unwrap();
        assert_eq!(k.scheme_uri, "urn:mpeg:dash:role:2011");
        assert_eq!(k.value, None);
        assert!(!k.has_value());
    }

    #[test]
    fn scheme_and_value_round_trip() {
        // §8.10.4.1 — the more common shape: scheme + named value
        // (DASH role, "main").
        let p = build_kind(0, 0, "urn:mpeg:dash:role:2011", Some("main"));
        let k = parse_kind(&p).unwrap();
        assert_eq!(k.scheme_uri, "urn:mpeg:dash:role:2011");
        assert_eq!(k.value.as_deref(), Some("main"));
        assert!(k.has_value());
    }

    #[test]
    fn webvtt_role_subtitles_round_trip() {
        // The other common producer: WebVTT-style role tag.
        let p = build_kind(0, 0, "https://www.w3.org/TR/webvtt1/", Some("subtitles"));
        let k = parse_kind(&p).unwrap();
        assert_eq!(k.scheme_uri, "https://www.w3.org/TR/webvtt1/");
        assert_eq!(k.value.as_deref(), Some("subtitles"));
    }

    #[test]
    fn empty_value_after_scheme_surfaces_as_none() {
        // schemeURI = "foo", then a bare NULL for an empty value.
        let p = build_kind(0, 0, "foo", None);
        let k = parse_kind(&p).unwrap();
        assert_eq!(k.scheme_uri, "foo");
        assert!(k.value.is_none());
    }

    #[test]
    fn missing_trailing_null_on_value_accepted() {
        // schemeURI = "scheme\0", then "tail" runs without a closing
        // NULL. We accept the truncated form rather than rejecting.
        let mut p = Vec::new();
        p.extend_from_slice(&[0, 0, 0, 0]); // ver + flags
        p.extend_from_slice(b"scheme\0tail");
        let k = parse_kind(&p).unwrap();
        assert_eq!(k.scheme_uri, "scheme");
        assert_eq!(k.value.as_deref(), Some("tail"));
    }

    #[test]
    fn missing_trailing_null_on_scheme_yields_none_value() {
        // schemeURI runs to end of box without a NULL — value is None.
        let mut p = Vec::new();
        p.extend_from_slice(&[0, 0, 0, 0]); // ver + flags
        p.extend_from_slice(b"only-scheme");
        let k = parse_kind(&p).unwrap();
        assert_eq!(k.scheme_uri, "only-scheme");
        assert!(k.value.is_none());
    }

    #[test]
    fn non_zero_version_rejected() {
        // version = 1 reserved for a future spec revision; refuse.
        let p = build_kind(1, 0, "x", None);
        assert!(parse_kind(&p).is_err());
    }

    #[test]
    fn truncated_below_fullbox_header_errors() {
        let p = vec![0u8; 3];
        assert!(parse_kind(&p).is_err());
    }

    #[test]
    fn fullbox_flags_ignored() {
        // Non-zero flags are accepted (§8.10.4.2 fixes them at 0 but
        // we tolerate arbitrary bits, consistent with parse_tsel).
        let p = build_kind(0, 0x00FF_FFFF, "scheme", Some("v"));
        let k = parse_kind(&p).unwrap();
        assert_eq!(k.scheme_uri, "scheme");
        assert_eq!(k.value.as_deref(), Some("v"));
    }

    #[test]
    fn invalid_utf8_in_scheme_uses_replacement_char() {
        // 0xFF, 0xFE is invalid UTF-8 — must not abort the parse.
        let mut p = Vec::new();
        p.extend_from_slice(&[0, 0, 0, 0]);
        p.extend_from_slice(&[b'a', 0xFF, 0xFE, b'z', 0]); // scheme
        p.push(0); // empty value
        let k = parse_kind(&p).unwrap();
        // String::from_utf8_lossy collapses the malformed sequence to
        // one U+FFFD; the surrounding ASCII survives.
        assert!(k.scheme_uri.starts_with('a'));
        assert!(k.scheme_uri.ends_with('z'));
        assert!(k.scheme_uri.contains('\u{FFFD}'));
    }

    #[test]
    fn find_kinds_in_udta_returns_empty_when_absent() {
        let mut udta = Vec::new();
        // Sibling text entry but no `kind`.
        let intl_body = b"\x00\x05\x00\x00Title";
        push_atom(&mut udta, [0xA9, b'n', b'a', b'm'], intl_body);
        let kinds = find_kinds_in_udta(&udta).unwrap();
        assert!(kinds.is_empty());
    }

    #[test]
    fn find_kinds_in_udta_collects_all_in_file_order() {
        // §8.10.4.1 explicitly permits "more than one of these may
        // occur in a track" — verify we return every `kind` rather
        // than first-match.
        let mut udta = Vec::new();
        let k1 = build_kind(0, 0, "urn:mpeg:dash:role:2011", Some("main"));
        push_atom(&mut udta, *b"kind", &k1);
        let intl_body = b"\x00\x05\x00\x00Title";
        push_atom(&mut udta, [0xA9, b'n', b'a', b'm'], intl_body);
        let k2 = build_kind(0, 0, "https://www.w3.org/TR/webvtt1/", Some("subtitles"));
        push_atom(&mut udta, *b"kind", &k2);
        let kinds = find_kinds_in_udta(&udta).unwrap();
        assert_eq!(kinds.len(), 2);
        assert_eq!(kinds[0].scheme_uri, "urn:mpeg:dash:role:2011");
        assert_eq!(kinds[0].value.as_deref(), Some("main"));
        assert_eq!(kinds[1].scheme_uri, "https://www.w3.org/TR/webvtt1/");
        assert_eq!(kinds[1].value.as_deref(), Some("subtitles"));
    }

    #[test]
    fn find_kinds_propagates_inner_parse_error() {
        // A `kind` body shorter than the 4-byte FullBox header inside
        // a well-formed `udta` — the parse error must surface, not
        // get silently dropped.
        let mut udta = Vec::new();
        push_atom(&mut udta, *b"kind", &[0u8; 3]);
        assert!(find_kinds_in_udta(&udta).is_err());
    }

    #[test]
    fn find_kinds_in_udta_handles_zero_terminator() {
        // `kind` entry followed by a 32-bit zero terminator (§8.10.1
        // optional sentinel).
        let mut udta = Vec::new();
        let k = build_kind(0, 0, "urn:scheme", None);
        push_atom(&mut udta, *b"kind", &k);
        udta.extend_from_slice(&0u32.to_be_bytes());
        let kinds = find_kinds_in_udta(&udta).unwrap();
        assert_eq!(kinds.len(), 1);
        assert_eq!(kinds[0].scheme_uri, "urn:scheme");
    }

    #[test]
    fn to_body_bytes_is_parse_inverse_with_value() {
        let k = KindEntry {
            scheme_uri: "urn:mpeg:dash:role:2011".to_string(),
            value: Some("caption".to_string()),
        };
        let body = k.to_body_bytes();
        assert_eq!(parse_kind(&body).unwrap(), k);
    }

    #[test]
    fn to_body_bytes_is_parse_inverse_uri_only() {
        // value None ⇒ [uri]\0\0 ⇒ parses back as None.
        let k = KindEntry {
            scheme_uri: "https://www.w3.org/TR/webvtt1/".to_string(),
            value: None,
        };
        let body = k.to_body_bytes();
        let reparsed = parse_kind(&body).unwrap();
        assert_eq!(reparsed, k);
        assert!(!reparsed.has_value());
    }

    #[test]
    fn to_body_bytes_empty_value_round_trips_as_none() {
        // An empty Some("") is surfaced by parse_kind as None — assert
        // the body matches the None encoding so the round-trip is stable.
        let with_empty = KindEntry {
            scheme_uri: "urn:x".to_string(),
            value: Some(String::new()),
        };
        let as_none = KindEntry {
            scheme_uri: "urn:x".to_string(),
            value: None,
        };
        assert_eq!(with_empty.to_body_bytes(), as_none.to_body_bytes());
        assert_eq!(parse_kind(&with_empty.to_body_bytes()).unwrap(), as_none);
    }
}
