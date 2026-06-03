//! Track Input Map atom (`imap`).
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01),
//! "Track Input Map Atoms" (pp. 51–53). The `imap` atom sits inside
//! `moov/trak` (QTFF Figure 2-6, p. 41) and tells the QuickTime engine
//! how to interpret data flowing into this track from its **non-primary
//! sources** — the tracks named by the parent track's `tref` references
//! of type `'ssrc'` (QTFF p. 50, Table 2-2: "the referenced track
//! should send its data to this track, rather than presenting it"). A
//! sprite track whose `'ssrc'` reference points at a video track, for
//! instance, can describe via `imap` exactly how that video data should
//! modulate one of its sprites — as a transform matrix, a clip region,
//! a sound-volume curve, a graphics mode, and so on.
//!
//! Layout per QTFF Figure 2-14 (p. 52):
//!
//! ```text
//! [size: 4][type = 'imap': 4]                  ── classic atom header
//!   ┌── one or more track input atoms ──┐
//!   │ [size: 4][type = ' in': 4]        │      ── QT-atom header
//!   │ [atom_id: 4]                      │
//!   │ [reserved: 2][child_count: 2]     │
//!   │ [reserved: 4]                     │
//!   │   ┌── input type atom (required) ┐│
//!   │   │ [size: 4][type = ' ty': 4]   ││
//!   │   │ [input_type: 4]              ││
//!   │   └──────────────────────────────┘│
//!   │   ┌── object ID atom (optional) ─┐│
//!   │   │ [size: 4][type = 'obid': 4]  ││
//!   │   │ [object_id: 4]               ││
//!   │   └──────────────────────────────┘│
//!   └────────────────────────────────────┘
//! ```
//!
//! Two FourCCs in the figure carry **leading 0x00 bytes**, NOT spaces
//! (QTFF p. 52, " in" / p. 53, " ty"): "note that the two leading bytes
//! must be set to 0x00". The on-disk bytes are `[0x00, 0x00, b'i',
//! b'n']` and `[0x00, 0x00, b't', b'y']` respectively. The `obid`
//! FourCC is a normal four-ASCII-byte type.
//!
//! Each track input atom corresponds to one entry of the parent track's
//! `'ssrc'` track-reference list: the input atom's `atom_id` is the
//! 1-based index into that `'ssrc'` entry list (QTFF p. 53: "the first
//! secondary input corresponds to the track input atom with an atom ID
//! value of 1; the second to the track input atom with an atom ID of
//! 2"). Callers walking the input map can resolve an entry's source
//! track via `parent.references[ssrc].track_ids[atom_id - 1]`.
//!
//! Input types defined in QTFF Table 2-3 (pp. 53–54):
//!
//! * `kTrackModifierTypeMatrix` (1) — a 3×3 transformation matrix.
//! * `kTrackModifierTypeClip` (2) — a QuickDraw clipping region.
//! * `kTrackModifierTypeVolume` (3) — an 8.8 fixed-point sound volume.
//! * `kTrackModifierTypeBalance` (4) — a 16-bit sound balance level.
//! * `kTrackModifierTypeGraphicsMode` (5) — a graphics-mode record
//!   (32-bit mode integer + RGB colour).
//! * `kTrackModifierObjectMatrix` (6) — a 3×3 matrix scoped to one
//!   object inside the track (e.g. one sprite of a sprite track).
//! * `kTrackModifierObjectGraphicsMode` (7) — a per-object
//!   graphics-mode record.
//! * `kTrackModifierTypeImage` (FourCC `'vide'`) — a compressed image
//!   payload for an object within the track. QTFF notes the legacy
//!   name `kTrackModifierTypeSpriteImage`.
//!
//! When the input modifies a sub-track object (input types 6, 7, or
//! `'vide'`) an [`ObjectId`] atom must accompany the [`InputType`] atom
//! to identify the object (QTFF p. 53).
//!
//! ISO BMFF (ISO/IEC 14496-12) does not standardise `imap`; it is a
//! QuickTime-only atom carried inside `trak`. The track-level field
//! stays `None` for MP4 / fMP4 / HEIF / AVIF inputs.

use std::io::{Read, Seek, SeekFrom};

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

use crate::atom::{read_atom_header, read_payload, AtomHeader, MAX_INMEMORY_ATOM_BODY};

/// QTFF p. 52 — track input atom FourCC `' in'`. The leading two
/// bytes are 0x00 (NOT ASCII space): "note that the two leading
/// bytes must be set to 0x00".
pub const TRACK_INPUT_ATOM: [u8; 4] = [0x00, 0x00, b'i', b'n'];

/// QTFF p. 53 — input type atom FourCC `' ty'`. Leading bytes are
/// 0x00.
pub const INPUT_TYPE_ATOM: [u8; 4] = [0x00, 0x00, b't', b'y'];

/// QTFF p. 53 — object ID atom FourCC `'obid'`.
pub const OBJECT_ID_ATOM: [u8; 4] = *b"obid";

/// QTFF Table 2-3 — 3×3 transformation matrix scoped to the whole
/// track's location and scaling.
pub const K_TRACK_MODIFIER_TYPE_MATRIX: u32 = 1;

/// QTFF Table 2-3 — QuickDraw clipping region scoped to the track's
/// shape.
pub const K_TRACK_MODIFIER_TYPE_CLIP: u32 = 2;

/// QTFF Table 2-3 — 8.8 fixed-point sound volume (track-level fade).
pub const K_TRACK_MODIFIER_TYPE_VOLUME: u32 = 3;

/// QTFF Table 2-3 — 16-bit sound-balance level (track-level pan).
pub const K_TRACK_MODIFIER_TYPE_BALANCE: u32 = 4;

/// QTFF Table 2-3 — graphics-mode record (32-bit mode + RGB colour)
/// scoped to the whole track.
pub const K_TRACK_MODIFIER_TYPE_GRAPHICS_MODE: u32 = 5;

/// QTFF Table 2-3 — 3×3 matrix scoped to one object inside the track
/// (e.g. one sprite). Requires an accompanying [`ObjectId`].
pub const K_TRACK_MODIFIER_OBJECT_MATRIX: u32 = 6;

/// QTFF Table 2-3 — per-object graphics-mode record. Requires an
/// accompanying [`ObjectId`].
pub const K_TRACK_MODIFIER_OBJECT_GRAPHICS_MODE: u32 = 7;

/// QTFF Table 2-3 — compressed image data targeting an object within
/// the track. The on-disk value is the FourCC `'vide'`, *not* a small
/// integer: QTFF reuses the video-media type marker as the input-type
/// identifier here. Requires an accompanying [`ObjectId`].
pub const K_TRACK_MODIFIER_TYPE_IMAGE: u32 = u32::from_be_bytes(*b"vide");

/// One leaf of the QTFF input-type taxonomy (Table 2-3, pp. 53–54).
///
/// The raw on-disk `u32` survives as [`InputTypeKind::Other`] for any
/// future-spec or vendor extension this enum does not enumerate, so a
/// well-formed `imap` whose `' ty'` carries an unrecognised identifier
/// still parses and surfaces the raw value to callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputTypeKind {
    /// `kTrackModifierTypeMatrix` (Table 2-3 value 1).
    Matrix,
    /// `kTrackModifierTypeClip` (Table 2-3 value 2).
    Clip,
    /// `kTrackModifierTypeVolume` (Table 2-3 value 3).
    Volume,
    /// `kTrackModifierTypeBalance` (Table 2-3 value 4).
    Balance,
    /// `kTrackModifierTypeGraphicsMode` (Table 2-3 value 5).
    GraphicsMode,
    /// `kTrackModifierObjectMatrix` (Table 2-3 value 6). Requires
    /// [`ObjectId`].
    ObjectMatrix,
    /// `kTrackModifierObjectGraphicsMode` (Table 2-3 value 7).
    /// Requires [`ObjectId`].
    ObjectGraphicsMode,
    /// `kTrackModifierTypeImage` — FourCC `'vide'`. Requires
    /// [`ObjectId`].
    Image,
    /// Anything not enumerated above. The raw 32-bit identifier is
    /// preserved so a caller dispatching on vendor extensions can
    /// inspect it without re-parsing.
    Other(u32),
}

impl InputTypeKind {
    /// Classify the raw on-disk identifier.
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            K_TRACK_MODIFIER_TYPE_MATRIX => Self::Matrix,
            K_TRACK_MODIFIER_TYPE_CLIP => Self::Clip,
            K_TRACK_MODIFIER_TYPE_VOLUME => Self::Volume,
            K_TRACK_MODIFIER_TYPE_BALANCE => Self::Balance,
            K_TRACK_MODIFIER_TYPE_GRAPHICS_MODE => Self::GraphicsMode,
            K_TRACK_MODIFIER_OBJECT_MATRIX => Self::ObjectMatrix,
            K_TRACK_MODIFIER_OBJECT_GRAPHICS_MODE => Self::ObjectGraphicsMode,
            r if r == K_TRACK_MODIFIER_TYPE_IMAGE => Self::Image,
            other => Self::Other(other),
        }
    }

    /// Return the underlying on-disk `u32`.
    pub fn raw(self) -> u32 {
        match self {
            Self::Matrix => K_TRACK_MODIFIER_TYPE_MATRIX,
            Self::Clip => K_TRACK_MODIFIER_TYPE_CLIP,
            Self::Volume => K_TRACK_MODIFIER_TYPE_VOLUME,
            Self::Balance => K_TRACK_MODIFIER_TYPE_BALANCE,
            Self::GraphicsMode => K_TRACK_MODIFIER_TYPE_GRAPHICS_MODE,
            Self::ObjectMatrix => K_TRACK_MODIFIER_OBJECT_MATRIX,
            Self::ObjectGraphicsMode => K_TRACK_MODIFIER_OBJECT_GRAPHICS_MODE,
            Self::Image => K_TRACK_MODIFIER_TYPE_IMAGE,
            Self::Other(r) => r,
        }
    }

    /// True for the three modifier identifiers QTFF p. 53 specifies as
    /// *per-object* (i.e. the accompanying object ID atom is required):
    /// `kTrackModifierObjectMatrix`, `kTrackModifierObjectGraphicsMode`,
    /// `kTrackModifierTypeImage`. Returns `false` for unrecognised
    /// identifiers ([`InputTypeKind::Other`]) so a caller's strict
    /// consistency check fails closed for unknown types.
    pub fn requires_object_id(self) -> bool {
        matches!(
            self,
            Self::ObjectMatrix | Self::ObjectGraphicsMode | Self::Image
        )
    }
}

/// One parsed [`InputType`] (` ty`) leaf — QTFF p. 53.
///
/// The atom is fixed 12 bytes on disk (4 + 4 + 4); the parser rejects
/// any other body length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputType {
    /// Typed dispatch over the raw identifier — see
    /// [`InputTypeKind::from_raw`].
    pub kind: InputTypeKind,
}

/// One parsed [`ObjectId`] (`obid`) leaf — QTFF p. 53.
///
/// The atom is fixed 12 bytes on disk (4 + 4 + 4); the parser rejects
/// any other body length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectId {
    /// Identifies the object inside the track that the parent
    /// [`TrackInputEntry`] modifies.
    pub id: u32,
}

/// One parsed track input atom (` in`) — QTFF Figure 2-14, pp. 52–53.
///
/// Each entry corresponds to one slot in the parent track's `'ssrc'`
/// track-reference list. The 1-based [`Self::atom_id`] is the index
/// into that list (QTFF p. 53).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInputEntry {
    /// 1-based index into the parent track's `'ssrc'` reference list.
    /// QTFF p. 53 prohibits the value 0 but documents the relationship
    /// to the `'ssrc'` slots ("the first secondary input corresponds
    /// to the track input atom with an atom ID value of 1").
    pub atom_id: u32,
    /// Required [`InputType`] (` ty`) leaf — QTFF p. 53 marks ` ty`
    /// with a `‡` "Required atom" annotation.
    pub input_type: InputType,
    /// Optional [`ObjectId`] (`obid`) leaf. Spec p. 53: "If the input
    /// is operating on an object within the track [..] an object ID
    /// atom must be included in the track input atom to identify the
    /// object." Required for [`InputTypeKind::requires_object_id`]
    /// identifiers; omitted otherwise.
    pub object_id: Option<ObjectId>,
}

/// Parsed Track Input Map atom (`imap`) — QTFF p. 51.
///
/// Carries one [`TrackInputEntry`] per non-primary track-reference
/// (`'ssrc'`) the parent track declares. The on-disk order matches
/// the file order of the ` in` children; consumers that need to
/// resolve an entry against the parent's reference list should
/// dispatch on [`TrackInputEntry::atom_id`], not list position
/// (writers are not strictly required by the spec to emit entries
/// in atom-id order).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackInputMap {
    /// All ` in` children of the `imap` container, in file order.
    pub entries: Vec<TrackInputEntry>,
}

impl TrackInputMap {
    /// Look up the [`TrackInputEntry`] whose `atom_id` matches the
    /// 1-based `'ssrc'` slot index `id`. Returns `None` if no entry
    /// names that slot (a legitimate case: writers may declare an
    /// `'ssrc'` reference without an accompanying `imap` entry, in
    /// which case the input is "no modification declared").
    pub fn entry_for_ssrc_slot(&self, id: u32) -> Option<&TrackInputEntry> {
        self.entries.iter().find(|e| e.atom_id == id)
    }
}

/// Parse an `imap` atom by streaming over its body.
///
/// The body holds one or more track input atoms (` in`). Each
/// track input atom carries a 12-byte QT-style header tail
/// (`atom_id` + reserved + `child_count` + reserved) immediately
/// after the classic 8-byte `[size][type]` header, followed by its
/// own child atoms (` ty` required, `obid` optional).
///
/// Errors:
///
/// * `Error::invalid` when an ` in` body is shorter than the 12-byte
///   QT-style header tail.
/// * `Error::invalid` when an ` in` carries no ` ty` (QTFF p. 52
///   marks the input type atom as required with a `‡`).
/// * `Error::invalid` when a ` ty` body is not exactly 4 bytes.
/// * `Error::invalid` when an `obid` body is not exactly 4 bytes.
/// * `Error::invalid` when an unexpected child FourCC appears (so the
///   spec's "exactly two child types" guarantee is enforced
///   structurally).
/// * `Error::invalid` when an input-type identifier in
///   [`InputTypeKind::requires_object_id`] is paired with no `obid`
///   (cross-field consistency QTFF p. 53 requires).
/// * `Error::invalid` when an ` in` declares a `child_count` that does
///   not match the number of children actually present.
pub fn parse_imap<R: Read + Seek + ?Sized>(r: &mut R, hdr: &AtomHeader) -> Result<TrackInputMap> {
    let body_len = hdr
        .payload_len()
        .ok_or_else(|| Error::invalid("MOV: imap: open-ended body not supported"))?;
    if body_len > MAX_INMEMORY_ATOM_BODY {
        return Err(Error::invalid(format!(
            "MOV: imap body of {body_len} bytes exceeds {MAX_INMEMORY_ATOM_BODY}-byte cap",
        )));
    }
    let body_end = hdr.payload_offset + body_len;
    r.seek(SeekFrom::Start(hdr.payload_offset))?;

    let mut entries = Vec::new();
    loop {
        let pos = r.stream_position()?;
        if pos >= body_end {
            break;
        }
        let child = match read_atom_header(r)? {
            Some(h) => h,
            None => break,
        };
        let child_end = child
            .total_size
            .map(|t| child.payload_offset + (t - child.header_len))
            .ok_or_else(|| Error::invalid("MOV: imap: open-ended child"))?;
        if child_end > body_end {
            return Err(Error::invalid(
                "MOV: imap: child atom extends beyond imap payload",
            ));
        }
        if child.fourcc != TRACK_INPUT_ATOM {
            return Err(Error::invalid(format!(
                "MOV: imap: unexpected child FourCC {:?} (expected ' in')",
                child.fourcc,
            )));
        }
        let body = read_payload(r, &child)?;
        entries.push(parse_track_input_entry(&body)?);
        r.seek(SeekFrom::Start(child_end))?;
    }
    Ok(TrackInputMap { entries })
}

/// Parse one track input atom (` in`) body — QTFF Figure 2-14, p. 52.
///
/// `body` should be the bytes *after* the classic `[size][type]`
/// 8-byte header (i.e. starting at the `atom_id` field). The QT-style
/// header tail is 12 bytes; what follows is a flat list of classic
/// child atoms (` ty` and optionally `obid`).
pub fn parse_track_input_entry(body: &[u8]) -> Result<TrackInputEntry> {
    if body.len() < 12 {
        return Err(Error::invalid(format!(
            "MOV: track input ' in' body of {} bytes < 12 (atom_id+reserved+child_count+reserved)",
            body.len(),
        )));
    }
    let atom_id = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    let reserved1 = u16::from_be_bytes([body[4], body[5]]);
    if reserved1 != 0 {
        return Err(Error::invalid(format!(
            "MOV: track input ' in' reserved1 = {reserved1} (spec fixes 0)",
        )));
    }
    let child_count = u16::from_be_bytes([body[6], body[7]]);
    let reserved2 = u32::from_be_bytes([body[8], body[9], body[10], body[11]]);
    if reserved2 != 0 {
        return Err(Error::invalid(format!(
            "MOV: track input ' in' reserved2 = {reserved2} (spec fixes 0)",
        )));
    }

    // Walk the inner classic atoms (' ty' required, 'obid' optional).
    let mut input_type: Option<InputType> = None;
    let mut object_id: Option<ObjectId> = None;
    let mut seen_children: u32 = 0;
    let mut cursor = 12usize;
    while cursor < body.len() {
        if body.len() - cursor < 8 {
            return Err(Error::invalid(
                "MOV: track input ' in': truncated child header",
            ));
        }
        let size = u32::from_be_bytes([
            body[cursor],
            body[cursor + 1],
            body[cursor + 2],
            body[cursor + 3],
        ]) as usize;
        if size < 8 {
            return Err(Error::invalid(format!(
                "MOV: track input ' in': child size {size} below 8-byte minimum",
            )));
        }
        if cursor + size > body.len() {
            return Err(Error::invalid(
                "MOV: track input ' in': child extends past ' in' body",
            ));
        }
        let mut fourcc = [0u8; 4];
        fourcc.copy_from_slice(&body[cursor + 4..cursor + 8]);
        let child_body = &body[cursor + 8..cursor + size];
        match fourcc {
            f if f == INPUT_TYPE_ATOM => {
                if input_type.is_some() {
                    return Err(Error::invalid(
                        "MOV: track input ' in' carries more than one ' ty'",
                    ));
                }
                if child_body.len() != 4 {
                    return Err(Error::invalid(format!(
                        "MOV: input type ' ty' body of {} bytes != 4",
                        child_body.len(),
                    )));
                }
                let raw = u32::from_be_bytes([
                    child_body[0],
                    child_body[1],
                    child_body[2],
                    child_body[3],
                ]);
                input_type = Some(InputType {
                    kind: InputTypeKind::from_raw(raw),
                });
            }
            f if f == OBJECT_ID_ATOM => {
                if object_id.is_some() {
                    return Err(Error::invalid(
                        "MOV: track input ' in' carries more than one 'obid'",
                    ));
                }
                if child_body.len() != 4 {
                    return Err(Error::invalid(format!(
                        "MOV: object id 'obid' body of {} bytes != 4",
                        child_body.len(),
                    )));
                }
                object_id = Some(ObjectId {
                    id: u32::from_be_bytes([
                        child_body[0],
                        child_body[1],
                        child_body[2],
                        child_body[3],
                    ]),
                });
            }
            other => {
                return Err(Error::invalid(format!(
                    "MOV: track input ' in': unexpected child FourCC {other:?} (expected ' ty' or 'obid')",
                )));
            }
        }
        seen_children = seen_children.saturating_add(1);
        cursor += size;
    }

    let input_type = input_type.ok_or_else(|| {
        Error::invalid("MOV: track input ' in' missing required ' ty' (input type) child")
    })?;

    // Cross-field consistency: object-scoped input types (QTFF Table
    // 2-3 values 6, 7, and 'vide') require an `obid` companion.
    if input_type.kind.requires_object_id() && object_id.is_none() {
        return Err(Error::invalid(format!(
            "MOV: track input ' in' input_type {:?} requires an 'obid' child",
            input_type.kind,
        )));
    }

    // Spec p. 52 declares an explicit `child_count` field. A writer
    // that reports a mismatching count has either truncated the
    // list (under-count) or padded it (over-count); either way we
    // refuse the entry so a malformed map can't silently disappear.
    if u32::from(child_count) != seen_children {
        return Err(Error::invalid(format!(
            "MOV: track input ' in' declares child_count = {child_count} but parsed {seen_children} child atoms",
        )));
    }

    Ok(TrackInputEntry {
        atom_id,
        input_type,
        object_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn classic_atom(fourcc: [u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        let size = (8 + body.len()) as u32;
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&fourcc);
        v.extend_from_slice(body);
        v
    }

    fn build_in(atom_id: u32, child_count: u16, children: &[u8]) -> Vec<u8> {
        // QT-style header tail then children.
        let mut body = Vec::new();
        body.extend_from_slice(&atom_id.to_be_bytes()); // atom_id
        body.extend_from_slice(&[0u8, 0u8]); // reserved
        body.extend_from_slice(&child_count.to_be_bytes()); // child_count
        body.extend_from_slice(&[0u8; 4]); // reserved
        body.extend_from_slice(children);
        classic_atom(TRACK_INPUT_ATOM, &body)
    }

    fn build_ty(raw: u32) -> Vec<u8> {
        classic_atom(INPUT_TYPE_ATOM, &raw.to_be_bytes())
    }

    fn build_obid(id: u32) -> Vec<u8> {
        classic_atom(OBJECT_ID_ATOM, &id.to_be_bytes())
    }

    fn build_imap(in_atoms: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        for a in in_atoms {
            body.extend_from_slice(a);
        }
        classic_atom(*b"imap", &body)
    }

    fn read_imap(buf: &[u8]) -> TrackInputMap {
        let mut r = Cursor::new(buf);
        let hdr = read_atom_header(&mut r)
            .unwrap()
            .expect("imap header at offset 0");
        parse_imap(&mut r, &hdr).unwrap()
    }

    #[test]
    fn input_type_kind_round_trips_known_identifiers() {
        for raw in 1u32..=7 {
            assert_eq!(InputTypeKind::from_raw(raw).raw(), raw);
        }
        // The 'vide' marker round-trips bit-exact via its FourCC u32.
        let vide = u32::from_be_bytes(*b"vide");
        assert_eq!(InputTypeKind::from_raw(vide), InputTypeKind::Image);
        assert_eq!(InputTypeKind::Image.raw(), vide);
        // Unknown identifier survives as Other(...) and round-trips.
        assert_eq!(InputTypeKind::from_raw(99), InputTypeKind::Other(99));
        assert_eq!(InputTypeKind::Other(99).raw(), 99);
    }

    #[test]
    fn input_type_kind_requires_object_id_only_for_per_object_types() {
        assert!(InputTypeKind::ObjectMatrix.requires_object_id());
        assert!(InputTypeKind::ObjectGraphicsMode.requires_object_id());
        assert!(InputTypeKind::Image.requires_object_id());
        assert!(!InputTypeKind::Matrix.requires_object_id());
        assert!(!InputTypeKind::Clip.requires_object_id());
        assert!(!InputTypeKind::Volume.requires_object_id());
        assert!(!InputTypeKind::Balance.requires_object_id());
        assert!(!InputTypeKind::GraphicsMode.requires_object_id());
        // Spec-unknown identifiers fail closed.
        assert!(!InputTypeKind::Other(99).requires_object_id());
    }

    #[test]
    fn parse_single_in_with_matrix_modifier() {
        let in_atom = {
            let mut children = Vec::new();
            children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_TYPE_MATRIX));
            build_in(1, 1, &children)
        };
        let buf = build_imap(&[in_atom]);
        let imap = read_imap(&buf);
        assert_eq!(imap.entries.len(), 1);
        let e = &imap.entries[0];
        assert_eq!(e.atom_id, 1);
        assert_eq!(e.input_type.kind, InputTypeKind::Matrix);
        assert!(e.object_id.is_none());
    }

    #[test]
    fn parse_in_with_object_id() {
        let in_atom = {
            let mut children = Vec::new();
            children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_OBJECT_MATRIX));
            children.extend_from_slice(&build_obid(42));
            build_in(2, 2, &children)
        };
        let buf = build_imap(&[in_atom]);
        let imap = read_imap(&buf);
        assert_eq!(imap.entries.len(), 1);
        let e = &imap.entries[0];
        assert_eq!(e.atom_id, 2);
        assert_eq!(e.input_type.kind, InputTypeKind::ObjectMatrix);
        assert_eq!(e.object_id, Some(ObjectId { id: 42 }));
    }

    #[test]
    fn parse_multiple_in_entries_in_file_order() {
        let in1 = {
            let mut children = Vec::new();
            children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_TYPE_VOLUME));
            build_in(1, 1, &children)
        };
        let in2 = {
            let mut children = Vec::new();
            children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_TYPE_BALANCE));
            build_in(2, 1, &children)
        };
        let in3 = {
            let mut children = Vec::new();
            children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_TYPE_IMAGE));
            children.extend_from_slice(&build_obid(7));
            build_in(3, 2, &children)
        };
        let buf = build_imap(&[in1, in2, in3]);
        let imap = read_imap(&buf);
        assert_eq!(imap.entries.len(), 3);
        assert_eq!(imap.entries[0].atom_id, 1);
        assert_eq!(imap.entries[0].input_type.kind, InputTypeKind::Volume);
        assert_eq!(imap.entries[1].atom_id, 2);
        assert_eq!(imap.entries[1].input_type.kind, InputTypeKind::Balance);
        assert_eq!(imap.entries[2].atom_id, 3);
        assert_eq!(imap.entries[2].input_type.kind, InputTypeKind::Image);
        assert_eq!(imap.entries[2].object_id, Some(ObjectId { id: 7 }));
    }

    #[test]
    fn entry_for_ssrc_slot_returns_match_by_atom_id() {
        let in1 = {
            let mut children = Vec::new();
            children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_TYPE_VOLUME));
            build_in(5, 1, &children)
        };
        let in2 = {
            let mut children = Vec::new();
            children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_TYPE_BALANCE));
            build_in(3, 1, &children)
        };
        let buf = build_imap(&[in1, in2]);
        let imap = read_imap(&buf);
        let hit = imap.entry_for_ssrc_slot(3).unwrap();
        assert_eq!(hit.atom_id, 3);
        assert_eq!(hit.input_type.kind, InputTypeKind::Balance);
        assert!(imap.entry_for_ssrc_slot(99).is_none());
    }

    #[test]
    fn missing_ty_child_is_rejected() {
        // ` in` body with QT header but no children at all.
        let in_atom = build_in(1, 0, &[]);
        let buf = build_imap(&[in_atom]);
        let mut r = Cursor::new(&buf);
        let hdr = read_atom_header(&mut r).unwrap().unwrap();
        assert!(parse_imap(&mut r, &hdr).is_err());
    }

    #[test]
    fn object_required_but_missing_is_rejected() {
        // Per-object modifier (ObjectMatrix) without an 'obid' child.
        let in_atom = {
            let mut children = Vec::new();
            children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_OBJECT_MATRIX));
            build_in(1, 1, &children)
        };
        let buf = build_imap(&[in_atom]);
        let mut r = Cursor::new(&buf);
        let hdr = read_atom_header(&mut r).unwrap().unwrap();
        let err = parse_imap(&mut r, &hdr).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("requires"), "got: {msg}");
    }

    #[test]
    fn ty_with_wrong_body_size_is_rejected() {
        // ' ty' with 5 bytes instead of 4.
        let bad_ty = classic_atom(INPUT_TYPE_ATOM, &[0, 0, 0, 1, 0]);
        let in_atom = build_in(1, 1, &bad_ty);
        let buf = build_imap(&[in_atom]);
        let mut r = Cursor::new(&buf);
        let hdr = read_atom_header(&mut r).unwrap().unwrap();
        assert!(parse_imap(&mut r, &hdr).is_err());
    }

    #[test]
    fn obid_with_wrong_body_size_is_rejected() {
        let bad_obid = classic_atom(OBJECT_ID_ATOM, &[0, 0, 0]);
        let mut children = Vec::new();
        children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_OBJECT_MATRIX));
        children.extend_from_slice(&bad_obid);
        // Tell the parser there are 2 children so it doesn't trip on
        // the count check before reaching the size check.
        let in_atom = build_in(1, 2, &children);
        let buf = build_imap(&[in_atom]);
        let mut r = Cursor::new(&buf);
        let hdr = read_atom_header(&mut r).unwrap().unwrap();
        assert!(parse_imap(&mut r, &hdr).is_err());
    }

    #[test]
    fn unknown_child_fourcc_in_in_is_rejected() {
        // A bogus child inside ` in`.
        let bogus = classic_atom(*b"xxxx", &[0u8; 4]);
        let mut children = Vec::new();
        children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_TYPE_MATRIX));
        children.extend_from_slice(&bogus);
        let in_atom = build_in(1, 2, &children);
        let buf = build_imap(&[in_atom]);
        let mut r = Cursor::new(&buf);
        let hdr = read_atom_header(&mut r).unwrap().unwrap();
        assert!(parse_imap(&mut r, &hdr).is_err());
    }

    #[test]
    fn unknown_child_fourcc_in_imap_is_rejected() {
        // imap body containing a stray non-' in' atom directly.
        let bogus = classic_atom(*b"xxxx", &[0u8; 4]);
        let buf = classic_atom(*b"imap", &bogus);
        let mut r = Cursor::new(&buf);
        let hdr = read_atom_header(&mut r).unwrap().unwrap();
        assert!(parse_imap(&mut r, &hdr).is_err());
    }

    #[test]
    fn empty_imap_returns_empty_entry_list() {
        // imap with no children is structurally fine — it just declares
        // "no input modifiers wired up". Spec p. 51 doesn't require
        // any minimum count.
        let buf = classic_atom(*b"imap", &[]);
        let mut r = Cursor::new(&buf);
        let hdr = read_atom_header(&mut r).unwrap().unwrap();
        let map = parse_imap(&mut r, &hdr).unwrap();
        assert!(map.entries.is_empty());
    }

    #[test]
    fn in_with_nonzero_reserved_is_rejected() {
        // reserved1 (16-bit) non-zero.
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes()); // atom_id
        body.extend_from_slice(&[0x00, 0x01]); // reserved1 = 1
        body.extend_from_slice(&1u16.to_be_bytes()); // child_count
        body.extend_from_slice(&[0u8; 4]); // reserved2
        body.extend_from_slice(&build_ty(K_TRACK_MODIFIER_TYPE_MATRIX));
        let in_atom = classic_atom(TRACK_INPUT_ATOM, &body);
        let buf = build_imap(&[in_atom]);
        let mut r = Cursor::new(&buf);
        let hdr = read_atom_header(&mut r).unwrap().unwrap();
        assert!(parse_imap(&mut r, &hdr).is_err());
    }

    #[test]
    fn in_with_child_count_mismatch_is_rejected() {
        // Reports 2 children but only ships 1.
        let mut children = Vec::new();
        children.extend_from_slice(&build_ty(K_TRACK_MODIFIER_TYPE_MATRIX));
        let in_atom = build_in(1, 2, &children);
        let buf = build_imap(&[in_atom]);
        let mut r = Cursor::new(&buf);
        let hdr = read_atom_header(&mut r).unwrap().unwrap();
        assert!(parse_imap(&mut r, &hdr).is_err());
    }

    #[test]
    fn parse_track_input_entry_rejects_truncated_qt_header() {
        // 11 bytes — one short of the 12-byte QT header tail.
        let body = vec![0u8; 11];
        assert!(parse_track_input_entry(&body).is_err());
    }

    #[test]
    fn input_type_atom_fourcc_starts_with_zero_bytes() {
        // Spec note QTFF p. 53 — "the two leading bytes must be set to
        // 0x00". This pins the on-disk constant rather than asserting
        // against ASCII.
        assert_eq!(INPUT_TYPE_ATOM, [0x00, 0x00, b't', b'y']);
        assert_eq!(TRACK_INPUT_ATOM, [0x00, 0x00, b'i', b'n']);
        assert_eq!(OBJECT_ID_ATOM, *b"obid");
    }
}
