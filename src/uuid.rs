//! User-Type Box (`uuid`).
//!
//! ISO/IEC 14496-12:2015 §4.2 / §11.1 (pp. 6 – 7 and p. 153). The
//! User-Type Box is the spec's escape hatch for vendor-specific
//! extensions: any organisation can ship a private payload by writing
//! a Box whose `type` FourCC is `'uuid'` and whose first 16 bytes of
//! body are a full 128-bit Universal Unique Identifier (UUID). Per
//! §11.1, every normative box type also has an implicit ISO-reserved
//! UUID (composed of `type ‖ 00 11 00 10 80 00 00 AA 00 38 9B 71`),
//! but the spec forbids writing standard boxes through the `'uuid'`
//! escape so the typed surface of [`Uuid`] only describes user
//! extensions.
//!
//! Layout per §4.2:
//!
//! ```text
//! aligned(8) class Box(unsigned int(32) boxtype,
//!                      optional unsigned int(8)[16] extended_type) {
//!     unsigned int(32) size;
//!     unsigned int(32) type = boxtype;        // 'uuid' on the wire
//!     if (size == 1)        unsigned int(64) largesize;
//!     else if (size == 0)   /* extends to end of file */
//!     if (boxtype == 'uuid') unsigned int(8)[16] usertype = extended_type;
//!     /* payload follows: opaque to this parser */
//! }
//! ```
//!
//! This module decodes the on-disk body of a `uuid` box — i.e. the
//! 16-byte `usertype` UUID and the opaque trailing bytes after it.
//! The atom walker presents the body to us already stripped of the
//! 8-byte / 16-byte size+type header, so the body always begins with
//! the UUID.
//!
//! QTFF (Apple QuickTime File Format Specification, 2001-03-01) does
//! not define `uuid` at the spec level, but real-world `.mov` files
//! routinely embed user-type boxes from vendors that emit QuickTime
//! containers — Sony's XAVC metadata (`8974dbce-7be7-4c51-84f9-7148f9882554`),
//! GoPro's GPMF telemetry (`b934a4d0-bc31-4c00-8d12-1c4ae8c8a8de`),
//! the PIFF / CFF tfxd / tfrf live-DASH extensions
//! (`6d1d9b05-42d5-44e6-80e2-141daff757b2` /
//! `d4807ef2-ca39-4695-8e54-26cb9e46a79f`), and the AvenirVision /
//! Garmin VIRB camera streams. The parser surfaces the UUID verbatim
//! so callers can match by exact 16-byte string without committing
//! the crate to any vendor-private schema.
//!
//! [`Uuid`] is exposed at file scope (any top-level `uuid` boxes are
//! collected in declaration order on `MovDemuxer::file_uuids`). The
//! same on-disk shape can appear inside `moov`, `trak`, `udta`, and
//! other containers — those occurrences are not currently extracted;
//! a follow-up round can wire them through if a vendor schema lands
//! that needs the nested scope.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// On-disk byte length of the `usertype` UUID prefix that precedes
/// the `uuid` box's opaque payload (§4.2). Every well-formed `uuid`
/// body is at least this long.
pub const USERTYPE_LEN: usize = 16;

/// Parsed User-Type Box body (ISO/IEC 14496-12 §4.2 / §11.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uuid {
    /// 16-byte user-type identifier following the box header. The
    /// spec describes this as the 128-bit UUID assigned to the
    /// vendor extension; the parser preserves the raw bytes (big-
    /// endian, on-disk order) without enforcing any of the RFC 4122
    /// variant or version bits. Callers comparing against a known
    /// vendor UUID should compare all 16 bytes directly.
    pub usertype: [u8; 16],
    /// Opaque payload that follows the `usertype`. The contents are
    /// vendor-defined; this crate does not attempt to decode them.
    /// An empty `Vec` is legal — a `uuid` box may carry zero bytes
    /// beyond the UUID prefix (the spec puts no lower bound on the
    /// payload length).
    pub payload: Vec<u8>,
}

impl Uuid {
    /// Format `usertype` as the canonical RFC 4122 textual form
    /// `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX` (32 lowercase hex digits
    /// in five groups separated by hyphens). Useful for diagnostics
    /// and for comparing against vendor-published UUID strings; for
    /// byte-exact dispatch, compare [`Self::usertype`] directly to
    /// avoid the round-trip through string formatting.
    pub fn usertype_string(&self) -> String {
        let u = &self.usertype;
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            u[0], u[1], u[2], u[3],
            u[4], u[5],
            u[6], u[7],
            u[8], u[9],
            u[10], u[11], u[12], u[13], u[14], u[15],
        )
    }

    /// `true` when this box's `usertype` matches one of the
    /// ISO-reserved namespace UUIDs (i.e. the form
    /// `XXXXXXXX-0011-0010-8000-00AA00389B71`). §11.1 reserves this
    /// pattern for the auto-derived UUID of every normative box
    /// type; the same section forbids writing standard boxes through
    /// the `'uuid'` escape, so a true result here indicates a
    /// non-conformant writer (or an internal indexer that promoted
    /// a typed box into the UUID space for traversal purposes).
    pub fn is_iso_reserved_namespace(&self) -> bool {
        // Bytes 4..16 are the fixed 12-byte ISO suffix from §11.1.
        // Bytes 0..4 are the original 32-bit boxtype.
        self.usertype[4..]
            == [
                0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
            ]
    }

    /// Recover the four-byte boxtype encoded into an ISO-reserved
    /// namespace UUID, per §11.1's `form_uuid(type, ISO_12_bytes)`
    /// formula. Returns `Some(type)` only when
    /// [`Self::is_iso_reserved_namespace`] holds; returns `None`
    /// for vendor UUIDs that do not follow the ISO escape pattern.
    pub fn iso_namespace_boxtype(&self) -> Option<[u8; 4]> {
        if self.is_iso_reserved_namespace() {
            Some([
                self.usertype[0],
                self.usertype[1],
                self.usertype[2],
                self.usertype[3],
            ])
        } else {
            None
        }
    }
}

/// Parse a `uuid` payload.
///
/// The atom walker hands us the body with the box header already
/// stripped, so the body begins with the 16-byte `usertype` UUID
/// followed by the opaque payload. Returns `Error::invalid` when the
/// body is shorter than the 16-byte UUID prefix; an empty payload
/// after the UUID is accepted (the spec puts no lower bound on the
/// payload length).
pub fn parse_uuid(payload: &[u8]) -> Result<Uuid> {
    if payload.len() < USERTYPE_LEN {
        return Err(Error::invalid(format!(
            "MOV: uuid body {} < {USERTYPE_LEN}-byte usertype prefix",
            payload.len()
        )));
    }
    let mut usertype = [0u8; USERTYPE_LEN];
    usertype.copy_from_slice(&payload[..USERTYPE_LEN]);
    let payload_tail = payload[USERTYPE_LEN..].to_vec();
    Ok(Uuid {
        usertype,
        payload: payload_tail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `uuid` body: the 16-byte UUID followed by `payload`.
    fn build_uuid_body(usertype: [u8; 16], payload: &[u8]) -> Vec<u8> {
        let mut p = Vec::with_capacity(USERTYPE_LEN + payload.len());
        p.extend_from_slice(&usertype);
        p.extend_from_slice(payload);
        p
    }

    #[test]
    fn parses_uuid_with_payload() {
        // PIFF tfxd UUID (live-DASH timing extension).
        let id = [
            0x6d, 0x1d, 0x9b, 0x05, 0x42, 0xd5, 0x44, 0xe6, 0x80, 0xe2, 0x14, 0x1d, 0xaf, 0xf7,
            0x57, 0xb2,
        ];
        let body = build_uuid_body(id, b"vendor-payload-bytes");
        let u = parse_uuid(&body).unwrap();
        assert_eq!(u.usertype, id);
        assert_eq!(u.payload, b"vendor-payload-bytes");
    }

    #[test]
    fn parses_uuid_with_empty_payload() {
        // §4.2 places no lower bound on payload length; a body of
        // exactly 16 bytes (UUID only) is legal.
        let id = [0u8; 16];
        let body = build_uuid_body(id, &[]);
        let u = parse_uuid(&body).unwrap();
        assert_eq!(u.usertype, id);
        assert!(u.payload.is_empty());
    }

    #[test]
    fn truncated_usertype_rejected() {
        // 15 bytes — one short of the 16-byte UUID prefix.
        let body = vec![0u8; USERTYPE_LEN - 1];
        assert!(parse_uuid(&body).is_err());
    }

    #[test]
    fn empty_body_rejected() {
        // Zero-length body — the box header reserves no `usertype`,
        // which §4.2 makes mandatory whenever `boxtype == 'uuid'`.
        assert!(parse_uuid(&[]).is_err());
    }

    #[test]
    fn usertype_string_formats_with_hyphens() {
        // Canonical RFC 4122 textual form: 8-4-4-4-12 hex with
        // lowercase digits. PIFF tfxd UUID as a worked example.
        let id = [
            0x6d, 0x1d, 0x9b, 0x05, 0x42, 0xd5, 0x44, 0xe6, 0x80, 0xe2, 0x14, 0x1d, 0xaf, 0xf7,
            0x57, 0xb2,
        ];
        let u = Uuid {
            usertype: id,
            payload: Vec::new(),
        };
        assert_eq!(u.usertype_string(), "6d1d9b05-42d5-44e6-80e2-141daff757b2");
    }

    #[test]
    fn iso_namespace_detected_and_boxtype_recovered() {
        // §11.1 form: type ‖ 00 11 00 10 80 00 00 AA 00 38 9B 71.
        // Using 'free' (0x66 0x72 0x65 0x65) as the encoded boxtype.
        let id = [
            0x66, 0x72, 0x65, 0x65, 0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38,
            0x9B, 0x71,
        ];
        let u = Uuid {
            usertype: id,
            payload: Vec::new(),
        };
        assert!(u.is_iso_reserved_namespace());
        assert_eq!(u.iso_namespace_boxtype(), Some(*b"free"));
    }

    #[test]
    fn vendor_uuid_not_iso_namespace() {
        // The PIFF tfxd UUID is not an ISO-reserved namespace
        // entry — its 12-byte suffix differs from §11.1's pattern.
        let id = [
            0x6d, 0x1d, 0x9b, 0x05, 0x42, 0xd5, 0x44, 0xe6, 0x80, 0xe2, 0x14, 0x1d, 0xaf, 0xf7,
            0x57, 0xb2,
        ];
        let u = Uuid {
            usertype: id,
            payload: Vec::new(),
        };
        assert!(!u.is_iso_reserved_namespace());
        assert_eq!(u.iso_namespace_boxtype(), None);
    }

    #[test]
    fn payload_round_trips_arbitrary_bytes() {
        // The parser must not alter the trailing bytes — vendor
        // payloads may carry arbitrary binary (including embedded
        // nulls and high-bit bytes).
        let id = [0xab; 16];
        let payload: Vec<u8> = (0..=255u8).collect();
        let body = build_uuid_body(id, &payload);
        let u = parse_uuid(&body).unwrap();
        assert_eq!(u.payload, payload);
    }

    #[test]
    fn payload_length_matches_body_minus_usertype() {
        let id = [0x42; 16];
        let payload = vec![0xCC; 1024];
        let body = build_uuid_body(id, &payload);
        let u = parse_uuid(&body).unwrap();
        assert_eq!(u.payload.len(), body.len() - USERTYPE_LEN);
        assert_eq!(u.payload, payload);
    }
}
