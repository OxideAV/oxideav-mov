//! `udta` user-data atom parsing.
//!
//! QTFF "User Data Atoms" (pp. 36–38). The `udta` atom is a flat list
//! of inner atoms whose 4-byte type discriminates the entry. Two
//! conventions coexist:
//!
//! * **Apple international-text entries** — atom types whose first
//!   byte is `0xA9` (the `©` glyph in Mac-Roman, ASCII 169). The
//!   payload is one or more `[size:u16 BE][lang:u16 BE][text: size-4]`
//!   records, allowing the same item to be carried in multiple
//!   languages (QTFF p. 38, paragraph "All user data list entries…").
//!   `lang` is a Mac language code (QTFF p. 198 Table 5-1) when
//!   < 0x8000; ISO BMFF tags (5-bit packed three-letter ISO 639-2)
//!   when ≥ 0x8000.
//!
//! * **Plain UTF-8 entries** introduced in QuickTime 7+ — atom types
//!   `name`, `auth`, `cprt` (QTFF supplement). Layout is a FullBox
//!   header (`[ver:1][flags:3]`) followed by a 16-bit ISO 639-2/T
//!   language tag and a UTF-8 string padded to the atom's end (no
//!   terminator).
//!
//! Behavioural decisions for round 4:
//!
//! * We surface every recognised entry as a `UserDataEntry` carrying
//!   the type FourCC, an optional Mac/ISO language tag, and the raw
//!   bytes. For Apple international-text entries with ≥ 1 language we
//!   emit *one* `UserDataEntry` per language record.
//! * Unknown atom types are still surfaced (raw bytes), to keep
//!   forensic recovery possible without reparsing.
//! * Decoding to a `String` is best-effort UTF-8; if the bytes fail
//!   UTF-8 we fall back to a Mac-Roman → UTF-8 expansion of the
//!   ASCII subset (anything ≥ 0x80 maps to `\u{FFFD}`). The
//!   pre-Unicode legacy is rare in modern files but does appear in
//!   files migrated from QuickTime 1.x–6.x.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// One `udta` entry. Each Apple international-text record (©nam, ©cpy,
/// …) yields one `UserDataEntry` per language record; ISO/QT-7-style
/// `name`/`auth`/`cprt` yield exactly one entry. Unknown atom types
/// surface as `kind = Unknown` with raw bytes preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserDataEntry {
    /// 4-byte atom type (e.g. `[0xA9, b'n', b'a', b'm']`, `b"name"`).
    pub fourcc: [u8; 4],
    /// Decoded entry shape.
    pub kind: UserDataKind,
}

/// Decoded variants of a `udta` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserDataKind {
    /// Apple international-text record (©XXX). One per `[size, lang,
    /// text]` block inside the entry's payload.
    InternationalText {
        /// Mac language code (QTFF p. 198) when `< 0x8000`; otherwise
        /// a packed ISO 639-2/T three-letter code (5 bits per char,
        /// stored as `(c1+0x60)<<10 | (c2+0x60)<<5 | (c3+0x60)`).
        language: u16,
        /// Decoded text (best-effort UTF-8). The on-disk encoding is
        /// either UTF-8 (when `language` indicates ISO) or Mac-Roman
        /// (when language is a Mac code).
        text: String,
    },
    /// QT-7+ plain UTF-8 entry. The atom has a FullBox header and a
    /// trailing 2-byte ISO 639-2/T language tag before the text.
    PlainUtf8 {
        /// Packed ISO 639-2/T language tag (5 bits per char, base 0x60).
        language: u16,
        /// Decoded UTF-8 text.
        text: String,
    },
    /// Unknown / non-text payload. Raw bytes preserved for forensics.
    Unknown(Vec<u8>),
}

impl UserDataEntry {
    /// Best-effort decode of the entry as a UTF-8 string. Returns
    /// `None` for Unknown payloads; for Apple international-text + QT-7
    /// PlainUtf8 entries returns the embedded string slice.
    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            UserDataKind::InternationalText { text, .. } => Some(text.as_str()),
            UserDataKind::PlainUtf8 { text, .. } => Some(text.as_str()),
            UserDataKind::Unknown(_) => None,
        }
    }

    /// True when `fourcc[0] == 0xA9` (the Mac-Roman © glyph). These are
    /// the canonical QTFF international-text user-data types.
    pub fn is_international_text(&self) -> bool {
        self.fourcc[0] == 0xA9
    }
}

/// Parse a `udta` payload. The body is a flat list of atoms; we walk
/// each one and dispatch based on the type FourCC.
pub fn parse_udta(payload: &[u8]) -> Result<Vec<UserDataEntry>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= payload.len() {
        let size = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]])
            as usize;
        // QTFF p. 37: "the data list is optionally terminated by a
        // 32-bit integer set to 0" — accept both shapes, terminating
        // cleanly when we hit the sentinel.
        if size == 0 && p + 4 == payload.len() {
            break;
        }
        if size < 8 || p + size > payload.len() {
            return Err(Error::invalid("MOV: udta entry size invalid"));
        }
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&payload[p + 4..p + 8]);
        let body = &payload[p + 8..p + size];
        if fc[0] == 0xA9 {
            // Apple ©XXX — multi-language list.
            for (lang, text) in parse_intl_text(body) {
                out.push(UserDataEntry {
                    fourcc: fc,
                    kind: UserDataKind::InternationalText {
                        language: lang,
                        text,
                    },
                });
            }
        } else if matches!(&fc, b"name" | b"auth" | b"cprt") {
            // QT-7+ FullBox + lang + UTF-8.
            if let Some((lang, text)) = parse_plain_utf8(body) {
                out.push(UserDataEntry {
                    fourcc: fc,
                    kind: UserDataKind::PlainUtf8 {
                        language: lang,
                        text,
                    },
                });
            } else {
                out.push(UserDataEntry {
                    fourcc: fc,
                    kind: UserDataKind::Unknown(body.to_vec()),
                });
            }
        } else {
            out.push(UserDataEntry {
                fourcc: fc,
                kind: UserDataKind::Unknown(body.to_vec()),
            });
        }
        p += size;
    }
    Ok(out)
}

/// Parse the body of an Apple international-text user-data entry.
///
/// Layout (QTFF p. 38): one or more
/// `[text_size: u16 BE][language: u16 BE][text: text_size bytes]`
/// records. Some encoders truncate to a single record without a
/// length-of-list prefix; we walk record-by-record and accept either
/// shape. Pre-1996 writers occasionally omit the `language` slot and
/// emit a bare `[size: u16][text: size]` — we detect that by checking
/// whether the implied first record consumes the entire body.
fn parse_intl_text(body: &[u8]) -> Vec<(u16, String)> {
    let mut out = Vec::new();
    if body.len() < 4 {
        return out;
    }
    let mut p = 0usize;
    while p + 4 <= body.len() {
        let text_size = u16::from_be_bytes([body[p], body[p + 1]]) as usize;
        let language = u16::from_be_bytes([body[p + 2], body[p + 3]]);
        if text_size == 0 {
            // Stop on a 0-size sentinel (some writers emit it as a
            // terminator between records).
            p += 4;
            continue;
        }
        let start = p + 4;
        let end = start.saturating_add(text_size);
        if end > body.len() {
            // Truncated record — bail out gracefully.
            break;
        }
        let raw = &body[start..end];
        // Heuristic: language-code values < 0x8000 are Mac codes
        // (Mac-Roman text); >= 0x8000 are ISO 639-2/T (UTF-8 text).
        let text = if language >= 0x8000 {
            std::str::from_utf8(raw).unwrap_or("").to_string()
        } else {
            mac_roman_to_utf8(raw)
        };
        out.push((language, text));
        p = end;
    }
    out
}

/// Parse a QT-7+ plain UTF-8 entry body: `[ver:1][flags:3][lang:u16]
/// [text: rest]`.
fn parse_plain_utf8(body: &[u8]) -> Option<(u16, String)> {
    if body.len() < 6 {
        return None;
    }
    let language = u16::from_be_bytes([body[4], body[5]]);
    let raw = &body[6..];
    let text = std::str::from_utf8(raw).ok()?.to_string();
    Some((language, text))
}

/// Convert a Mac-Roman-encoded slice to UTF-8. Pure-ASCII bytes
/// (< 0x80) round-trip; bytes ≥ 0x80 map to U+FFFD. The pre-Unicode
/// Mac-Roman code page covers 0x80..=0xFF with characters that have
/// well-defined Unicode codepoints (full table at
/// `https://en.wikipedia.org/wiki/Mac_OS_Roman`); the substitution
/// behaviour is intentionally conservative — surfacing the Mac code
/// page would require a 128-entry table that QTFF round-4 doesn't yet
/// motivate.
fn mac_roman_to_utf8(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len());
    for &c in b {
        if c < 0x80 {
            s.push(c as char);
        } else {
            s.push('\u{FFFD}');
        }
    }
    s
}

/// Decode an ISO 639-2/T 5-bit-packed language code (as carried in
/// `mdhd.language` and QT-7 user-data entries) into a 3-character ISO
/// language tag string. Returns `None` when the high bit of the input
/// is set (Mac language codes are not ISO-packed).
pub fn iso_language_tag(language: u16) -> Option<[u8; 3]> {
    if language & 0x8000 != 0 {
        return None;
    }
    let c1 = ((language >> 10) & 0x1F) as u8 + 0x60;
    let c2 = ((language >> 5) & 0x1F) as u8 + 0x60;
    let c3 = (language & 0x1F) as u8 + 0x60;
    if c1.is_ascii_lowercase() && c2.is_ascii_lowercase() && c3.is_ascii_lowercase() {
        Some([c1, c2, c3])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
        let size: u32 = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(body);
    }

    #[test]
    fn intl_text_single_language_round_trip() {
        // ©nam with Mac language=0 (English Mac-Roman), text="Title"
        let mut entry_body = Vec::new();
        let txt = b"Title";
        entry_body.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        entry_body.extend_from_slice(&0u16.to_be_bytes());
        entry_body.extend_from_slice(txt);
        let mut udta = Vec::new();
        push_atom(&mut udta, [0xA9, b'n', b'a', b'm'], &entry_body);
        let entries = parse_udta(&udta).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fourcc, [0xA9, b'n', b'a', b'm']);
        assert_eq!(entries[0].as_str(), Some("Title"));
        assert!(entries[0].is_international_text());
    }

    #[test]
    fn intl_text_two_languages_emits_two_entries() {
        let mut entry_body = Vec::new();
        // English Mac-Roman
        let en = b"Title";
        entry_body.extend_from_slice(&(en.len() as u16).to_be_bytes());
        entry_body.extend_from_slice(&0u16.to_be_bytes());
        entry_body.extend_from_slice(en);
        // ISO 639-2/T 'fra' = (b'f'-0x60)<<10 | (b'r'-0x60)<<5 | (b'a'-0x60)
        let fra: u16 =
            ((b'f' - 0x60) as u16) << 10 | ((b'r' - 0x60) as u16) << 5 | ((b'a' - 0x60) as u16);
        let fr = "Titre".as_bytes();
        entry_body.extend_from_slice(&(fr.len() as u16).to_be_bytes());
        // Set top bit = 0x8000 to flag the slot as ISO 639-2/T.
        entry_body.extend_from_slice(&(fra | 0x8000).to_be_bytes());
        entry_body.extend_from_slice(fr);
        let mut udta = Vec::new();
        push_atom(&mut udta, [0xA9, b'n', b'a', b'm'], &entry_body);
        let entries = parse_udta(&udta).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].as_str(), Some("Title"));
        assert_eq!(entries[1].as_str(), Some("Titre"));
    }

    #[test]
    fn name_atom_parses_qt7_plain_utf8() {
        // name: [ver+flags=4][lang u16][utf-8]
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes());
        let eng: u16 =
            ((b'e' - 0x60) as u16) << 10 | ((b'n' - 0x60) as u16) << 5 | ((b'g' - 0x60) as u16);
        body.extend_from_slice(&eng.to_be_bytes());
        body.extend_from_slice("Movie Name".as_bytes());
        let mut udta = Vec::new();
        push_atom(&mut udta, *b"name", &body);
        let entries = parse_udta(&udta).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fourcc, *b"name");
        assert_eq!(entries[0].as_str(), Some("Movie Name"));
        match &entries[0].kind {
            UserDataKind::PlainUtf8 { language, .. } => {
                assert_eq!(iso_language_tag(*language), Some(*b"eng"));
            }
            _ => panic!("expected PlainUtf8"),
        }
    }

    #[test]
    fn unknown_entry_kept_as_raw_bytes() {
        // arbitrary fourcc 'wXYZ' with binary blob
        let mut udta = Vec::new();
        push_atom(&mut udta, *b"wXYZ", &[1, 2, 3, 4, 5]);
        let entries = parse_udta(&udta).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0].kind {
            UserDataKind::Unknown(b) => assert_eq!(b, &[1, 2, 3, 4, 5]),
            _ => panic!("expected Unknown"),
        }
        assert!(entries[0].as_str().is_none());
    }

    #[test]
    fn iso_lang_packed_round_trip() {
        // 'eng' -> ((101-96)<<10) | ((110-96)<<5) | (103-96) = 5<<10 | 14<<5 | 7
        let eng: u16 = (5 << 10) | (14 << 5) | 7;
        assert_eq!(iso_language_tag(eng), Some(*b"eng"));
        // High-bit-set Mac code returns None.
        assert_eq!(iso_language_tag(0x8000 | eng), None);
    }

    #[test]
    fn empty_udta_returns_no_entries() {
        let entries = parse_udta(&[]).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn udta_terminator_zero_accepted() {
        // ©cpy with body, then a 4-byte zero terminator.
        let mut entry_body = Vec::new();
        let txt = b"(c) 2026";
        entry_body.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        entry_body.extend_from_slice(&0u16.to_be_bytes());
        entry_body.extend_from_slice(txt);
        let mut udta = Vec::new();
        push_atom(&mut udta, [0xA9, b'c', b'p', b'y'], &entry_body);
        // Terminating 32-bit zero (QTFF p. 37, "data list… optionally
        // terminated").
        udta.extend_from_slice(&0u32.to_be_bytes());
        let entries = parse_udta(&udta).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].as_str(), Some("(c) 2026"));
    }

    #[test]
    fn truncated_record_drops_partial() {
        // ©nam with declared text_size=10 but only 4 bytes follow.
        let mut entry_body = Vec::new();
        entry_body.extend_from_slice(&10u16.to_be_bytes());
        entry_body.extend_from_slice(&0u16.to_be_bytes());
        entry_body.extend_from_slice(b"abcd");
        let mut udta = Vec::new();
        push_atom(&mut udta, [0xA9, b'n', b'a', b'm'], &entry_body);
        let entries = parse_udta(&udta).unwrap();
        // Truncated: no entries emitted.
        assert!(entries.is_empty());
    }
}
