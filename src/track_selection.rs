//! Track Selection box (`tsel`).
//!
//! ISO/IEC 14496-12 §8.10.3 ("Track Selection Box", pp. 72–74 of the
//! 2015 edition). `tsel` is the ISO-BMFF mechanism for ranking tracks
//! inside an alternate group: the `tkhd.alternate_group` field already
//! identifies which tracks are mutually-exclusive playback candidates,
//! and `tsel` refines that into a finer-grained **switch group** plus a
//! list of typed attribute FourCCs that describe — or differentiate
//! between — the tracks in that switch group.
//!
//! The spec uses the words "alternate" and "switch" with specific
//! meanings (§8.10.3.1):
//!
//! * **alternate group** — every track in the group is a candidate for
//!   media selection at session start (e.g. pick one of N audio
//!   languages). Cross-track switching mid-session may or may not be
//!   meaningful.
//! * **switch group** — a refinement: every track in the switch group
//!   is *also* available for switching during playback (e.g. between
//!   multiple bitrate ladders of the same codec at the same frame
//!   size). One alternate group can contain several switch groups, but
//!   every switch group is contained inside exactly one alternate
//!   group (§8.10.3.4 last sentence: "tracks that belong to the same
//!   switch group shall belong to the same alternate group").
//!
//! Layout per §8.10.3.3:
//!
//! ```text
//! aligned(8) class TrackSelectionBox
//!   extends FullBox('tsel', version = 0, 0) {
//!     template int(32) switch_group = 0;
//!     unsigned int(32) attribute_list[]; // to end of the box
//! }
//! ```
//!
//! The container is the track-level `udta` (`moov/trak/udta/tsel`),
//! NOT `udta/moov` and NOT directly inside `trak`. QTFF (the Apple
//! ancestor) does not define this box; it is ISO BMFF-only.
//!
//! `switch_group` is read as a *signed* 32-bit integer per the spec's
//! `template int(32)` declaration. A value of `0` (or an absent
//! `tsel`) means "no information about switching" (§8.10.3.4). Non-
//! zero values group tracks for switching: two tracks share a switch
//! group when their `switch_group` integers are equal AND both tracks
//! sit in the same alternate group on their `tkhd`.
//!
//! `attribute_list` is read to the end of the box, four bytes per
//! attribute, in encoder-supplied order (the spec doesn't impose a
//! canonical ordering). The well-known attribute set is enumerated in
//! §8.10.3.5 — six **descriptive** attributes characterising the track,
//! and eight **differentiating** attributes pointing the player at a
//! field elsewhere in the file that distinguishes the track from its
//! switch-group peers. Unknown attribute FourCCs are surfaced raw so
//! future spec amendments don't break the parser.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

// ─────── §8.10.3.5 Attributes ───────────────────────────────────────

/// Descriptive — "the track can be temporally scaled" (§8.10.3.5).
pub const TSEL_ATTR_TEMPORAL_SCALABILITY: [u8; 4] = *b"tesc";
/// Descriptive — "the track can be scaled in terms of quality"
/// (fine-grain SNR scalability, §8.10.3.5).
pub const TSEL_ATTR_FINE_GRAIN_SNR_SCALABILITY: [u8; 4] = *b"fgsc";
/// Descriptive — "the track can be scaled in terms of quality"
/// (coarse-grain SNR scalability, §8.10.3.5).
pub const TSEL_ATTR_COARSE_GRAIN_SNR_SCALABILITY: [u8; 4] = *b"cgsc";
/// Descriptive — "the track can be spatially scaled" (§8.10.3.5).
pub const TSEL_ATTR_SPATIAL_SCALABILITY: [u8; 4] = *b"spsc";
/// Descriptive — "the track can be region-of-interest scaled"
/// (§8.10.3.5).
pub const TSEL_ATTR_REGION_OF_INTEREST_SCALABILITY: [u8; 4] = *b"resc";
/// Descriptive — "the track can be scaled in terms of number of
/// views" (§8.10.3.5).
pub const TSEL_ATTR_VIEW_SCALABILITY: [u8; 4] = *b"vwsc";

/// Differentiating — "codec" (§8.10.3.5). Pointer: the Sample Entry
/// inside the media track's Sample Description box.
pub const TSEL_ATTR_CODEC: [u8; 4] = *b"cdec";
/// Differentiating — "screen size" (§8.10.3.5). Pointer: the `width`
/// and `height` fields of a Visual Sample Entry.
pub const TSEL_ATTR_SCREEN_SIZE: [u8; 4] = *b"scsz";
/// Differentiating — "max packet size" (§8.10.3.5). Pointer: the
/// `maxpacketsize` field of an RTP Hint Sample Entry.
pub const TSEL_ATTR_MAX_PACKET_SIZE: [u8; 4] = *b"mpsz";
/// Differentiating — "media type" (§8.10.3.5). Pointer: the
/// `handlertype` field of the media track's Handler box.
pub const TSEL_ATTR_MEDIA_TYPE: [u8; 4] = *b"mtyp";
/// Differentiating — "media language" (§8.10.3.5). Pointer: the
/// `language` field of the Media Header box.
pub const TSEL_ATTR_MEDIA_LANGUAGE: [u8; 4] = *b"mela";
/// Differentiating — "bitrate" (§8.10.3.5). Pointer: total sample
/// bytes divided by `tkhd.duration` (computed by the player).
pub const TSEL_ATTR_BITRATE: [u8; 4] = *b"bitr";
/// Differentiating — "frame rate" (§8.10.3.5). Pointer: total sample
/// count divided by `tkhd.duration` (computed by the player).
pub const TSEL_ATTR_FRAME_RATE: [u8; 4] = *b"frar";
/// Differentiating — "number of views" (§8.10.3.5). Pointer: number
/// of views in the sub-track.
pub const TSEL_ATTR_NUMBER_OF_VIEWS: [u8; 4] = *b"nvws";

/// Spec-defined attribute role (§8.10.3.5). Descriptive attributes
/// **characterise** the track they appear on; differentiating
/// attributes **distinguish** the track from other tracks in the same
/// alternate / switch group by naming a field elsewhere in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TsAttributeRole {
    /// "Descriptive" per §8.10.3.5 ("Descriptive attributes
    /// characterize the tracks they modify").
    Descriptive,
    /// "Differentiating" per §8.10.3.5 ("differentiating attributes
    /// differentiate between tracks that belong to the same alternate
    /// or switch groups").
    Differentiating,
    /// Attribute FourCC not enumerated by §8.10.3.5 — typically a
    /// future spec amendment or a vendor extension. Surfaced so
    /// callers can dispatch on the raw FourCC themselves.
    Unknown,
}

/// Classify an attribute FourCC by its §8.10.3.5 role.
pub fn ts_attribute_role(fourcc: &[u8; 4]) -> TsAttributeRole {
    match fourcc {
        b"tesc" | b"fgsc" | b"cgsc" | b"spsc" | b"resc" | b"vwsc" => TsAttributeRole::Descriptive,
        b"cdec" | b"scsz" | b"mpsz" | b"mtyp" | b"mela" | b"bitr" | b"frar" | b"nvws" => {
            TsAttributeRole::Differentiating
        }
        _ => TsAttributeRole::Unknown,
    }
}

/// Parsed `tsel` Track Selection box (ISO/IEC 14496-12 §8.10.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackSelection {
    /// `switch_group` per §8.10.3.4. Read as a signed 32-bit integer
    /// (the spec declares `template int(32) switch_group`). Zero or
    /// "tsel absent" both mean "no switching information"; non-zero
    /// values group tracks for switching across the parent alternate
    /// group.
    pub switch_group: i32,
    /// `attribute_list` (§8.10.3.4), in the on-wire order, as raw
    /// FourCCs. The full enumerated set is exported as
    /// `TSEL_ATTR_*` constants in this module; unknown FourCCs are
    /// preserved verbatim so vendor extensions don't break the parser.
    pub attributes: Vec<[u8; 4]>,
}

impl TrackSelection {
    /// True when the box conveys any switching information at all —
    /// either a non-zero `switch_group` or at least one declared
    /// attribute. A `tsel` with `switch_group == 0` AND an empty
    /// `attributes` list is structurally equivalent to having no
    /// `tsel` at all per §8.10.3.4.
    pub fn is_informative(&self) -> bool {
        self.switch_group != 0 || !self.attributes.is_empty()
    }

    /// True when `attributes` contains the given FourCC. Linear scan;
    /// the spec doesn't bound the list length but real files carry
    /// 0–4 entries.
    pub fn has_attribute(&self, fourcc: &[u8; 4]) -> bool {
        self.attributes.iter().any(|a| a == fourcc)
    }

    /// Iterator over `(fourcc, role)` pairs — useful for callers that
    /// want to dispatch on the §8.10.3.5 descriptive/differentiating
    /// distinction without re-importing `ts_attribute_role`.
    pub fn typed_attributes(&self) -> impl Iterator<Item = ([u8; 4], TsAttributeRole)> + '_ {
        self.attributes.iter().map(|&a| (a, ts_attribute_role(&a)))
    }
}

/// Parse a `tsel` Track Selection box payload (ISO/IEC 14496-12
/// §8.10.3.3).
///
/// Expects the FullBox header (`[version:1][flags:3]`) followed by the
/// 4-byte `switch_group` and an attribute list that runs to the end of
/// the box. Returns:
///
/// * `Error::invalid` when the payload is shorter than the 8-byte
///   minimum (`[ver+flags:4] + [switch_group:4]`).
/// * `Error::invalid` when the trailing attribute-list region length
///   is not a multiple of 4 bytes (each attribute is exactly an
///   `unsigned int(32)` per §8.10.3.3).
/// * `Error::invalid` when the FullBox version field is non-zero
///   (§8.10.3.3 declares `version = 0`; future versions would change
///   the layout and we'd rather refuse than silently misparse).
///
/// FullBox flags are accepted and ignored: §8.10.3.3 fixes them at
/// `0` but real-world tolerance for arbitrary flag bits is consistent
/// with how we treat every other FullBox in this crate.
pub fn parse_tsel(payload: &[u8]) -> Result<TrackSelection> {
    if payload.len() < 8 {
        return Err(Error::invalid(format!(
            "MOV: tsel payload {} < 8 bytes",
            payload.len()
        )));
    }
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "MOV: tsel version {} != 0",
            version
        )));
    }
    // payload[1..4] = flags (ignored).
    let switch_group = i32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let tail = &payload[8..];
    if tail.len() % 4 != 0 {
        return Err(Error::invalid(format!(
            "MOV: tsel attribute-list tail {} bytes not multiple of 4",
            tail.len()
        )));
    }
    let n = tail.len() / 4;
    let mut attributes = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * 4;
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&tail[off..off + 4]);
        attributes.push(fc);
    }
    Ok(TrackSelection {
        switch_group,
        attributes,
    })
}

/// Scan a raw `udta` payload for a `tsel` child and parse it.
///
/// `udta` is a flat atom list (QTFF p. 37 / ISO/IEC 14496-12 §8.10.1
/// "User Data box"); each child is the usual `[size:4][type:4][body]`.
/// The function walks every entry, returns the first `tsel` it finds
/// parsed via [`parse_tsel`], or `None` when no `tsel` child is
/// present. A truncated or malformed `tsel` body propagates the parse
/// error; truncated *other* children are tolerated (the existing
/// [`crate::user_data::parse_udta`] behaviour) so an unrelated bad
/// entry doesn't hide a valid `tsel` further along.
pub fn find_tsel_in_udta(udta_payload: &[u8]) -> Result<Option<TrackSelection>> {
    let mut p = 0usize;
    while p + 8 <= udta_payload.len() {
        let size = u32::from_be_bytes([
            udta_payload[p],
            udta_payload[p + 1],
            udta_payload[p + 2],
            udta_payload[p + 3],
        ]) as usize;
        // QTFF p. 37 / §8.10.1: udta may be terminated by a 32-bit
        // zero. Treat that as end-of-list.
        if size == 0 && p + 4 == udta_payload.len() {
            break;
        }
        if size < 8 || p + size > udta_payload.len() {
            // Malformed entry — stop walking (the rest of the buffer
            // is untrustworthy). If we found no tsel before this
            // point we return None.
            break;
        }
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&udta_payload[p + 4..p + 8]);
        if &fc == b"tsel" {
            let body = &udta_payload[p + 8..p + size];
            return parse_tsel(body).map(Some);
        }
        p += size;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tsel(version: u8, flags: u32, switch_group: i32, attrs: &[[u8; 4]]) -> Vec<u8> {
        let mut p = Vec::with_capacity(8 + attrs.len() * 4);
        p.push(version);
        let f = flags.to_be_bytes();
        p.extend_from_slice(&f[1..4]);
        p.extend_from_slice(&switch_group.to_be_bytes());
        for a in attrs {
            p.extend_from_slice(a);
        }
        p
    }

    #[test]
    fn empty_attribute_list_round_trips() {
        // switch_group = 7, no attributes.
        let p = build_tsel(0, 0, 7, &[]);
        let t = parse_tsel(&p).unwrap();
        assert_eq!(t.switch_group, 7);
        assert!(t.attributes.is_empty());
        assert!(t.is_informative());
    }

    #[test]
    fn switch_group_zero_with_no_attributes_is_uninformative() {
        let p = build_tsel(0, 0, 0, &[]);
        let t = parse_tsel(&p).unwrap();
        assert_eq!(t.switch_group, 0);
        assert!(t.attributes.is_empty());
        assert!(!t.is_informative());
    }

    #[test]
    fn switch_group_is_signed() {
        // -1 (0xFFFF_FFFF on the wire) must be read as -1, not
        // u32::MAX.
        let p = build_tsel(0, 0, -1, &[]);
        let t = parse_tsel(&p).unwrap();
        assert_eq!(t.switch_group, -1);
    }

    #[test]
    fn descriptive_attributes_classify_correctly() {
        let p = build_tsel(
            0,
            0,
            1,
            &[
                TSEL_ATTR_TEMPORAL_SCALABILITY,
                TSEL_ATTR_FINE_GRAIN_SNR_SCALABILITY,
                TSEL_ATTR_COARSE_GRAIN_SNR_SCALABILITY,
                TSEL_ATTR_SPATIAL_SCALABILITY,
                TSEL_ATTR_REGION_OF_INTEREST_SCALABILITY,
                TSEL_ATTR_VIEW_SCALABILITY,
            ],
        );
        let t = parse_tsel(&p).unwrap();
        assert_eq!(t.attributes.len(), 6);
        for (_fc, role) in t.typed_attributes() {
            assert_eq!(role, TsAttributeRole::Descriptive);
        }
        assert!(t.has_attribute(&TSEL_ATTR_TEMPORAL_SCALABILITY));
    }

    #[test]
    fn differentiating_attributes_classify_correctly() {
        let p = build_tsel(
            0,
            0,
            2,
            &[
                TSEL_ATTR_CODEC,
                TSEL_ATTR_SCREEN_SIZE,
                TSEL_ATTR_MAX_PACKET_SIZE,
                TSEL_ATTR_MEDIA_TYPE,
                TSEL_ATTR_MEDIA_LANGUAGE,
                TSEL_ATTR_BITRATE,
                TSEL_ATTR_FRAME_RATE,
                TSEL_ATTR_NUMBER_OF_VIEWS,
            ],
        );
        let t = parse_tsel(&p).unwrap();
        assert_eq!(t.attributes.len(), 8);
        for (_fc, role) in t.typed_attributes() {
            assert_eq!(role, TsAttributeRole::Differentiating);
        }
        assert!(t.has_attribute(&TSEL_ATTR_CODEC));
        assert!(t.has_attribute(&TSEL_ATTR_BITRATE));
    }

    #[test]
    fn mixed_known_and_unknown_attributes_preserved_in_order() {
        // Two known + one vendor-specific FourCC in the middle.
        let vendor = *b"XvNd";
        let p = build_tsel(
            0,
            0,
            42,
            &[TSEL_ATTR_BITRATE, vendor, TSEL_ATTR_MEDIA_LANGUAGE],
        );
        let t = parse_tsel(&p).unwrap();
        assert_eq!(t.attributes.len(), 3);
        assert_eq!(t.attributes[0], TSEL_ATTR_BITRATE);
        assert_eq!(t.attributes[1], vendor);
        assert_eq!(t.attributes[2], TSEL_ATTR_MEDIA_LANGUAGE);
        assert_eq!(ts_attribute_role(&vendor), TsAttributeRole::Unknown);
    }

    #[test]
    fn truncated_box_below_header_errors() {
        // 7 bytes: shorter than the 4 + 4 = 8 byte minimum.
        let p = vec![0u8; 7];
        assert!(parse_tsel(&p).is_err());
    }

    #[test]
    fn non_multiple_of_four_tail_errors() {
        // Header (8 bytes) plus 5 bytes of attribute-list tail —
        // a structural error per §8.10.3.3.
        let mut p = build_tsel(0, 0, 1, &[TSEL_ATTR_CODEC]);
        p.push(0x55);
        assert!(parse_tsel(&p).is_err());
    }

    #[test]
    fn non_zero_version_rejected() {
        // version = 1 reserved for a future spec revision; we refuse.
        let p = build_tsel(1, 0, 0, &[]);
        assert!(parse_tsel(&p).is_err());
    }

    #[test]
    fn fullbox_flags_ignored() {
        // Non-zero flags don't change the parse — they're consumed by
        // the FullBox header and we treat them as opaque.
        let p = build_tsel(0, 0x00FF_FFFF, 99, &[TSEL_ATTR_CODEC]);
        let t = parse_tsel(&p).unwrap();
        assert_eq!(t.switch_group, 99);
        assert_eq!(t.attributes, vec![TSEL_ATTR_CODEC]);
    }

    fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
        let size: u32 = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(body);
    }

    #[test]
    fn find_tsel_in_udta_picks_up_tsel_child() {
        // udta carrying a sibling text entry plus a tsel.
        let mut udta = Vec::new();
        let intl_body = b"\x00\x05\x00\x00Title";
        push_atom(&mut udta, [0xA9, b'n', b'a', b'm'], intl_body);
        let body = build_tsel(0, 0, 5, &[TSEL_ATTR_BITRATE]);
        push_atom(&mut udta, *b"tsel", &body);
        let parsed = find_tsel_in_udta(&udta).unwrap().unwrap();
        assert_eq!(parsed.switch_group, 5);
        assert_eq!(parsed.attributes, vec![TSEL_ATTR_BITRATE]);
    }

    #[test]
    fn find_tsel_in_udta_returns_none_when_absent() {
        let mut udta = Vec::new();
        let intl_body = b"\x00\x05\x00\x00Title";
        push_atom(&mut udta, [0xA9, b'n', b'a', b'm'], intl_body);
        assert!(find_tsel_in_udta(&udta).unwrap().is_none());
    }

    #[test]
    fn find_tsel_in_udta_handles_zero_terminator() {
        // tsel entry followed by a 32-bit zero terminator (QTFF p. 37
        // optional sentinel).
        let mut udta = Vec::new();
        let body = build_tsel(0, 0, 9, &[]);
        push_atom(&mut udta, *b"tsel", &body);
        udta.extend_from_slice(&0u32.to_be_bytes());
        let parsed = find_tsel_in_udta(&udta).unwrap().unwrap();
        assert_eq!(parsed.switch_group, 9);
    }

    #[test]
    fn find_tsel_propagates_inner_parse_error() {
        // tsel entry with a truncated body (4 bytes, below the 8-byte
        // minimum) — the inner parse must surface the error rather
        // than silently dropping the entry.
        let mut udta = Vec::new();
        push_atom(&mut udta, *b"tsel", &[0u8; 4]);
        assert!(find_tsel_in_udta(&udta).is_err());
    }

    #[test]
    fn role_classifier_matches_spec_enumeration() {
        // Sanity: every documented FourCC classifies correctly.
        for fc in [b"tesc", b"fgsc", b"cgsc", b"spsc", b"resc", b"vwsc"] {
            assert_eq!(ts_attribute_role(fc), TsAttributeRole::Descriptive);
        }
        for fc in [
            b"cdec", b"scsz", b"mpsz", b"mtyp", b"mela", b"bitr", b"frar", b"nvws",
        ] {
            assert_eq!(ts_attribute_role(fc), TsAttributeRole::Differentiating);
        }
        assert_eq!(ts_attribute_role(b"ZZZZ"), TsAttributeRole::Unknown);
    }
}
