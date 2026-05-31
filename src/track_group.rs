//! Track Group Box (`trgr`).
//!
//! ISO/IEC 14496-12 §8.3.4 ("Track Group Box", p. 27 of the 2015
//! edition). `trgr` is a container that sits inside `trak` and holds
//! zero or more *track-group-type* FullBoxes. Each contained child has
//!
//! * the FourCC of the box = the *track group type*,
//! * a 32-bit `track_group_id` immediately after the FullBox header.
//!
//! Two tracks that contain a child with the **same FourCC and the same
//! `track_group_id`** belong to the same track group. The pair
//! `(track_group_type, track_group_id)` is the spec's identifier
//! (§8.3.4.3).
//!
//! Box layout per §8.3.4.2:
//!
//! ```text
//! aligned(8) class TrackGroupBox('trgr') {
//! }
//! aligned(8) class TrackGroupTypeBox(unsigned int(32) track_group_type)
//!   extends FullBox(track_group_type, version = 0, flags = 0) {
//!     unsigned int(32) track_group_id;
//!     // the remaining data may be specified for a particular
//!     // track_group_type
//! }
//! ```
//!
//! `trgr` is `Quantity: Zero or one` (§8.3.4.1), but the *children* are
//! unconstrained — a single `trgr` may carry several track-group-type
//! boxes (e.g. one `'msrc'` plus one vendor-specific group), and a
//! given child type may even repeat (the spec doesn't forbid it). The
//! parser collects every child in file order.
//!
//! Spec-defined `track_group_type` values:
//!
//! * `'msrc'` (§8.3.4.3) — multi-source presentation. Tracks sharing
//!   this group are mapped as originating from the same source (e.g.
//!   the audio and video of one participant in a video-telephony call).
//!
//! Derived specifications register additional types (ISO/IEC 14496-15,
//! ISO/IEC 23008-12, etc.) — this crate does not pre-enumerate them but
//! surfaces the trailing payload bytes verbatim so caller code can
//! interpret them when needed.
//!
//! The §8.3.4.1 note is explicit: track groups indicate **shared
//! characteristics or relationships**, not **dependencies** — those are
//! the Track Reference Box's job (`tref`). This parser does not blur
//! the two surfaces; consumers that need a dependency edge should keep
//! using [`crate::track::TrackRef`].
//!
//! QTFF (the Apple ancestor of ISO BMFF) does not define `trgr`; the
//! per-track field stays empty for plain `.mov` inputs.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

use crate::atom::{read_payload, walk_children, AtomHeader};
use std::io::{Read, Seek, SeekFrom};

/// `track_group_type` FourCC for the §8.3.4.3 multi-source-presentation
/// group. A common appearance is a recorded video-telephony session
/// where each participant's audio and video tracks share a single
/// `'msrc'` group id.
pub const TRACK_GROUP_TYPE_MSRC: [u8; 4] = *b"msrc";

/// One `(track_group_type, track_group_id)` membership declaration.
///
/// One [`TrackGroupTypeEntry`] is produced per FullBox child of a
/// `trgr` container. The `payload` slice carries any
/// type-specific bytes that follow the 4-byte `track_group_id` —
/// §8.3.4.2's "the remaining data may be specified for a particular
/// `track_group_type`". For `'msrc'` (and most groups in practice) the
/// payload is empty; vendor or derived-spec groups can ship extra
/// fields here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackGroupTypeEntry {
    /// FourCC of the child FullBox = the *track group type* per
    /// §8.3.4.3. `'msrc'` is the only value defined in the base spec
    /// (see [`TRACK_GROUP_TYPE_MSRC`]). Derived specifications register
    /// additional values.
    pub track_group_type: [u8; 4],
    /// Identifier shared by every track that belongs to this group.
    /// Two tracks with the same `(track_group_type, track_group_id)`
    /// pair are in the same group (§8.3.4.3).
    pub track_group_id: u32,
    /// FullBox version. §8.3.4.2 fixes it at 0; the parser rejects
    /// non-zero values so a future schema change can't silently
    /// misparse.
    pub version: u8,
    /// FullBox flags low-24-bit value. §8.3.4.2 fixes it at 0 but
    /// real-world tolerance for arbitrary flag bits is consistent with
    /// how this crate treats every other FullBox (see [`parse_kind`]).
    ///
    /// [`parse_kind`]: crate::kind::parse_kind
    pub flags: u32,
    /// Type-specific trailing bytes after `track_group_id`, surfaced
    /// verbatim. Empty for `'msrc'` and every other base-spec type.
    pub payload: Vec<u8>,
}

impl TrackGroupTypeEntry {
    /// Convenience predicate: true when this entry declares membership
    /// of a §8.3.4.3 multi-source-presentation group.
    pub fn is_msrc(&self) -> bool {
        self.track_group_type == TRACK_GROUP_TYPE_MSRC
    }

    /// `(track_group_type, track_group_id)` — the §8.3.4.3 group
    /// identifier. Two tracks whose entry lists each contain this pair
    /// belong to the same group.
    pub fn key(&self) -> ([u8; 4], u32) {
        (self.track_group_type, self.track_group_id)
    }
}

/// Parse one `TrackGroupTypeBox` FullBox payload.
///
/// `track_group_type` is the FourCC the caller read from the box
/// header; the payload starts at the first byte after the box header.
/// Layout: `[version:1][flags:3][track_group_id:4][type_specific:..]`.
///
/// Errors:
/// * `Error::invalid` when the body is shorter than the 8-byte fixed
///   record (FullBox header + `track_group_id`).
/// * `Error::invalid` when the FullBox version field is non-zero —
///   §8.3.4.2 declares `version = 0` and a future revision would
///   change the field layout.
///
/// Non-zero flag bits are tolerated (the spec fixes them at zero but
/// every other FullBox in this crate accepts arbitrary flag values for
/// real-world robustness; see [`parse_kind`]).
///
/// [`parse_kind`]: crate::kind::parse_kind
pub fn parse_track_group_type(
    track_group_type: [u8; 4],
    payload: &[u8],
) -> Result<TrackGroupTypeEntry> {
    if payload.len() < 8 {
        return Err(Error::invalid(format!(
            "MOV: trgr child payload {} < 8 bytes",
            payload.len()
        )));
    }
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "MOV: trgr child version {version} != 0"
        )));
    }
    let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
    let track_group_id = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    Ok(TrackGroupTypeEntry {
        track_group_type,
        track_group_id,
        version,
        flags,
        payload: payload[8..].to_vec(),
    })
}

/// Parse a `trgr` Track Group Box's children into a vector of
/// `(track_group_type, track_group_id)` membership declarations.
///
/// `trgr`'s body is a flat list of FullBoxes whose FourCC *is* the
/// `track_group_type`. The walker uses the existing [`walk_children`]
/// machinery so all generic atom-header guardrails (size==0, size==1
/// extended size, past-EOF rejection — see [`crate::atom`]) apply.
///
/// Children are returned in file order; duplicate `(type, id)` pairs
/// are not deduplicated since §8.3.4 does not forbid them (a single
/// track may legitimately appear in two membership rows of the same
/// type for different derived-spec purposes).
pub fn parse_trgr<R: Read + Seek + ?Sized>(
    r: &mut R,
    hdr: &AtomHeader,
) -> Result<Vec<TrackGroupTypeEntry>> {
    let body_end = hdr.payload_offset + hdr.payload_len().unwrap_or(0);
    r.seek(SeekFrom::Start(hdr.payload_offset))?;
    let mut out = Vec::new();
    walk_children(r, Some(body_end), |r, child| {
        let body = read_payload(r, child)?;
        let entry = parse_track_group_type(child.fourcc, &body)?;
        out.push(entry);
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn build_fullbox(
        fourcc: &[u8; 4],
        version: u8,
        flags: u32,
        track_group_id: u32,
        payload_tail: &[u8],
    ) -> Vec<u8> {
        // Returns the *atom-on-disk* bytes ([size:4][fourcc:4][body]).
        let mut body = Vec::new();
        body.push(version);
        let f = flags.to_be_bytes();
        body.extend_from_slice(&f[1..4]);
        body.extend_from_slice(&track_group_id.to_be_bytes());
        body.extend_from_slice(payload_tail);
        let mut atom = Vec::new();
        let size: u32 = (8 + body.len()) as u32;
        atom.extend_from_slice(&size.to_be_bytes());
        atom.extend_from_slice(fourcc);
        atom.extend_from_slice(&body);
        atom
    }

    fn build_trgr(children: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        for c in children {
            body.extend_from_slice(c);
        }
        let mut atom = Vec::new();
        let size: u32 = (8 + body.len()) as u32;
        atom.extend_from_slice(&size.to_be_bytes());
        atom.extend_from_slice(b"trgr");
        atom.extend_from_slice(&body);
        atom
    }

    fn header_at_offset_zero(buf: &[u8]) -> AtomHeader {
        let mut r = Cursor::new(buf);
        crate::atom::read_atom_header(&mut r)
            .unwrap()
            .expect("atom header at offset 0")
    }

    #[test]
    fn parse_track_group_type_msrc_round_trip() {
        // §8.3.4.3 base type with no trailing payload.
        let body = {
            let mut b = Vec::new();
            b.extend_from_slice(&[0u8; 4]); // ver=0 + flags=0
            b.extend_from_slice(&42u32.to_be_bytes());
            b
        };
        let entry = parse_track_group_type(*b"msrc", &body).unwrap();
        assert_eq!(entry.track_group_type, *b"msrc");
        assert_eq!(entry.track_group_id, 42);
        assert_eq!(entry.version, 0);
        assert_eq!(entry.flags, 0);
        assert!(entry.payload.is_empty());
        assert!(entry.is_msrc());
        assert_eq!(entry.key(), (*b"msrc", 42));
    }

    #[test]
    fn parse_track_group_type_preserves_payload_tail() {
        // Derived-spec type with 6 extra bytes after `track_group_id`.
        let body = {
            let mut b = Vec::new();
            b.extend_from_slice(&[0u8; 4]);
            b.extend_from_slice(&7u32.to_be_bytes());
            b.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x42, 0x00]);
            b
        };
        let entry = parse_track_group_type(*b"vend", &body).unwrap();
        assert_eq!(entry.track_group_type, *b"vend");
        assert_eq!(entry.track_group_id, 7);
        assert_eq!(entry.payload, vec![0xDE, 0xAD, 0xBE, 0xEF, 0x42, 0x00]);
        assert!(!entry.is_msrc());
    }

    #[test]
    fn parse_track_group_type_truncated_below_fixed_record_errors() {
        // 7 bytes — one short of [ver+flags:4][id:4] = 8.
        let body = vec![0u8; 7];
        assert!(parse_track_group_type(*b"msrc", &body).is_err());
    }

    #[test]
    fn parse_track_group_type_unknown_version_rejected() {
        let body = {
            let mut b = Vec::new();
            b.push(1); // version=1, reserved
            b.extend_from_slice(&[0u8; 3]);
            b.extend_from_slice(&3u32.to_be_bytes());
            b
        };
        assert!(parse_track_group_type(*b"msrc", &body).is_err());
    }

    #[test]
    fn parse_track_group_type_nonzero_flags_tolerated() {
        // §8.3.4.2 fixes flags at 0 but we mirror parse_kind /
        // parse_tsel and accept arbitrary bits.
        let body = {
            let mut b = Vec::new();
            b.push(0);
            b.extend_from_slice(&[0xAB, 0xCD, 0xEF]);
            b.extend_from_slice(&9u32.to_be_bytes());
            b
        };
        let entry = parse_track_group_type(*b"msrc", &body).unwrap();
        assert_eq!(entry.flags, 0x00AB_CDEF);
        assert_eq!(entry.track_group_id, 9);
    }

    #[test]
    fn parse_trgr_single_msrc_child() {
        let msrc = build_fullbox(b"msrc", 0, 0, 100, &[]);
        let trgr = build_trgr(&[msrc]);
        let hdr = header_at_offset_zero(&trgr);
        let mut r = Cursor::new(&trgr);
        let entries = parse_trgr(&mut r, &hdr).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key(), (*b"msrc", 100));
        assert!(entries[0].is_msrc());
    }

    #[test]
    fn parse_trgr_multiple_children_preserves_order() {
        // Mix of `msrc` and a derived-spec FourCC; the file order
        // must be preserved (§8.3.4 does not forbid multiple
        // children — and derived specs routinely declare new types
        // that share the container with `msrc`).
        let msrc = build_fullbox(b"msrc", 0, 0, 1, &[]);
        let vend = build_fullbox(b"vend", 0, 0, 2, &[0xAA, 0xBB]);
        let trgr = build_trgr(&[msrc, vend]);
        let hdr = header_at_offset_zero(&trgr);
        let mut r = Cursor::new(&trgr);
        let entries = parse_trgr(&mut r, &hdr).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key(), (*b"msrc", 1));
        assert_eq!(entries[1].key(), (*b"vend", 2));
        assert_eq!(entries[1].payload, vec![0xAA, 0xBB]);
    }

    #[test]
    fn parse_trgr_empty_body_returns_empty_list() {
        // §8.3.4.1 allows zero children (the container exists but
        // contributes no membership).
        let trgr = build_trgr(&[]);
        let hdr = header_at_offset_zero(&trgr);
        let mut r = Cursor::new(&trgr);
        let entries = parse_trgr(&mut r, &hdr).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_trgr_propagates_child_parse_error() {
        // A child whose body is only 4 bytes (below the 8-byte fixed
        // record) — the inner error must surface, not get silently
        // swallowed.
        let mut bad_child = Vec::new();
        bad_child.extend_from_slice(&12u32.to_be_bytes()); // size = 12
        bad_child.extend_from_slice(b"msrc");
        bad_child.extend_from_slice(&[0u8; 4]); // body, only 4 bytes
        let trgr = build_trgr(&[bad_child]);
        let hdr = header_at_offset_zero(&trgr);
        let mut r = Cursor::new(&trgr);
        assert!(parse_trgr(&mut r, &hdr).is_err());
    }

    #[test]
    fn parse_trgr_duplicate_pair_kept_in_order() {
        // §8.3.4 doesn't forbid two identical (type, id) rows; the
        // parser preserves them rather than dedupe. Derived specs
        // can re-use a single membership across two purposes.
        let a = build_fullbox(b"msrc", 0, 0, 11, &[]);
        let b = build_fullbox(b"msrc", 0, 0, 11, &[]);
        let trgr = build_trgr(&[a, b]);
        let hdr = header_at_offset_zero(&trgr);
        let mut r = Cursor::new(&trgr);
        let entries = parse_trgr(&mut r, &hdr).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key(), entries[1].key());
    }
}
