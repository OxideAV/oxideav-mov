//! Sub Track box family (`strk` > `stri` + `strd` > `stsg`).
//!
//! ISO/IEC 14496-12 §8.14 ("Sub tracks", pp. 97–100 of the 2015
//! edition). Sub tracks assign *parts* of a track to alternate / switch
//! groups using the same numbering space as track-level alternate /
//! switch groups (`tkhd.alternate_group` + the `tsel` box). This is the
//! mechanism layered codecs such as SVC and MVC use to express media
//! alternatives that don't map cleanly onto whole-track boundaries
//! (§8.14.1): one coded track carries several temporal / quality /
//! spatial layers, and each layer is described as a sub track.
//!
//! ## Box hierarchy (§8.14.3 – §8.14.6)
//!
//! ```text
//! trak
//!  └─ udta
//!      └─ strk   Sub Track box           §8.14.3  (zero or more)
//!          ├─ stri   Sub Track Information   §8.14.4  (mandatory, one)
//!          └─ strd   Sub Track Definition    §8.14.5  (mandatory, one)
//!              └─ stsg  Sub Track Sample Group §8.14.6 (zero or more)
//! ```
//!
//! `strk` and `strd` are both bare `Box` wrappers with no own fields
//! (§8.14.3.2 / §8.14.5.2): `aligned(8) class SubTrack extends
//! Box('strk') {}` and likewise for `strd`. All the data lives in their
//! children. We surface a parsed [`SubTrack`] per `strk`, carrying its
//! mandatory [`SubTrackInformation`] (`stri`) plus the list of
//! [`SubTrackSampleGroup`] (`stsg`) entries collected from its `strd`.
//!
//! ## `stri` — Sub Track Information (§8.14.4)
//!
//! ```text
//! aligned(8) class SubTrackInformation
//!   extends FullBox('stri', version = 0, 0) {
//!     template int(16)          switch_group    = 0;
//!     template int(16)          alternate_group = 0;
//!     template unsigned int(32) sub_track_ID    = 0;
//!     unsigned int(32) attribute_list[];  // to the end of the box
//! }
//! ```
//!
//! `switch_group` and `alternate_group` are declared `template int(16)`
//! — signed 16-bit — and reuse the §8.10.3.4 / §8.14.4.3 semantics:
//! both default to `0` meaning "no information". A non-zero
//! `switch_group` groups sub tracks (and tracks) that can be switched
//! between during playback; a non-zero `alternate_group` groups mutually
//! exclusive alternatives. Per §8.14.4.3 (and §8.10.3.4) every member of
//! a switch group shares one alternate group. `sub_track_ID` is a
//! `template unsigned int(32)`: a non-zero value uniquely identifies the
//! sub track *locally within the track*; `0` (default) means "not
//! assigned".
//!
//! The `attribute_list` runs to the end of the box, four bytes per
//! attribute. §8.14.4.3 enumerates the same descriptive /
//! differentiating attribute FourCCs as the track-level `tsel` box
//! (§8.10.3.5) — `tesc` / `fgsc` / `cgsc` / `spsc` / `resc` / `vwsc` are
//! descriptive, `bitr` / `frar` / `nvws` are differentiating — so we
//! reuse [`crate::track_selection::ts_attribute_role`] for
//! classification rather than re-enumerating the table. Unknown FourCCs
//! are preserved verbatim.
//!
//! ## `stsg` — Sub Track Sample Group (§8.14.6)
//!
//! ```text
//! aligned(8) class SubTrackSampleGroupBox
//!   extends FullBox('stsg', 0, 0) {
//!     unsigned int(32) grouping_type;
//!     unsigned int(16) item_count;
//!     for (i = 0; i < item_count; i++)
//!         unsigned int(32) group_description_index;
//! }
//! ```
//!
//! This defines the sub track as one or more sample groups: it names a
//! `grouping_type` (matching a sibling `sgpd` / `sbgp` per §8.14.6.3)
//! and lists `item_count` indices into that grouping's Sample Group
//! Description box. A sub track is the union of the sample groups whose
//! description indices appear here.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

use crate::track_selection::{ts_attribute_role, TsAttributeRole};

/// Parsed `stri` Sub Track Information box (ISO/IEC 14496-12 §8.14.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubTrackInformation {
    /// `switch_group` (§8.14.4.3). Read as a signed 16-bit integer (the
    /// spec declares `template int(16)`). `0` means "no switching
    /// information"; a non-zero value groups sub tracks / tracks that
    /// can be switched between during playback.
    pub switch_group: i16,
    /// `alternate_group` (§8.14.4.3). Signed 16-bit `template int(16)`.
    /// `0` means "no information on relations to other tracks /
    /// sub-tracks"; a non-zero value groups mutually exclusive
    /// alternatives. Members of one switch group share an alternate
    /// group.
    pub alternate_group: i16,
    /// `sub_track_ID` (§8.14.4.3). Unsigned 32-bit. A non-zero value
    /// uniquely identifies this sub track locally within the track; `0`
    /// (default) means the sub track ID is not assigned.
    pub sub_track_id: u32,
    /// `attribute_list` (§8.14.4.3), in on-wire order, as raw FourCCs.
    /// Reuses the §8.10.3.5 attribute taxonomy (see
    /// [`crate::track_selection::ts_attribute_role`]); unknown FourCCs
    /// are preserved verbatim so vendor / future-spec attributes don't
    /// break the parser.
    pub attributes: Vec<[u8; 4]>,
}

impl SubTrackInformation {
    /// True when the box conveys any grouping information at all — a
    /// non-zero `switch_group`, a non-zero `alternate_group`, or at
    /// least one declared attribute. An all-default `stri` (all fields
    /// `0`, empty attribute list) carries no information per §8.14.4.3.
    pub fn is_informative(&self) -> bool {
        self.switch_group != 0
            || self.alternate_group != 0
            || self.sub_track_id != 0
            || !self.attributes.is_empty()
    }

    /// True when `attributes` contains the given FourCC. Linear scan;
    /// the spec doesn't bound the list length but real files carry a
    /// handful of entries.
    pub fn has_attribute(&self, fourcc: &[u8; 4]) -> bool {
        self.attributes.iter().any(|a| a == fourcc)
    }

    /// Iterator over `(fourcc, role)` pairs, classifying each attribute
    /// by its §8.10.3.5 descriptive / differentiating role (shared with
    /// the track-level `tsel` box per §8.14.4.3).
    pub fn typed_attributes(&self) -> impl Iterator<Item = ([u8; 4], TsAttributeRole)> + '_ {
        self.attributes.iter().map(|&a| (a, ts_attribute_role(&a)))
    }
}

/// Parsed `stsg` Sub Track Sample Group box (ISO/IEC 14496-12 §8.14.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubTrackSampleGroup {
    /// `grouping_type` (§8.14.6.3) — identifies the sample grouping.
    /// The value matches the `grouping_type` of the corresponding
    /// `sbgp` (Sample to Group) and `sgpd` (Sample Group Description)
    /// boxes for this track.
    pub grouping_type: [u8; 4],
    /// `group_description_index[]` (§8.14.6.3), one per `item_count`.
    /// Each is a 1-based index into the matching `sgpd` box's entries
    /// (the same indexing convention as `sbgp`). The sub track is the
    /// union of the sample groups these indices select.
    pub group_description_indices: Vec<u32>,
}

/// One parsed `strk` Sub Track box (ISO/IEC 14496-12 §8.14.3) and its
/// children — the mandatory `stri` plus every `stsg` found inside the
/// mandatory `strd`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubTrack {
    /// `stri` Sub Track Information (§8.14.4). §8.14.4.1 declares it
    /// `Mandatory: Yes, Quantity: One`; we surface the first one found
    /// and tolerate its absence by leaving the default value rather than
    /// rejecting the whole `udta` (a structurally-incomplete `strk` from
    /// a non-conforming writer shouldn't sink unrelated user data).
    pub information: SubTrackInformation,
    /// `stsg` Sub Track Sample Group entries (§8.14.6) collected from
    /// the `strk`'s mandatory `strd` (Sub Track Definition) child.
    /// §8.14.6.1 declares `Quantity: Zero or more`, so this may be empty
    /// even for a well-formed sub track.
    pub sample_groups: Vec<SubTrackSampleGroup>,
}

/// Parse a `stri` Sub Track Information box payload (§8.14.4.2).
///
/// Expects the FullBox header (`[version:1][flags:3]`) followed by
/// `switch_group:i16`, `alternate_group:i16`, `sub_track_ID:u32`, and an
/// attribute list running to the end of the box. Returns:
///
/// * `Error::invalid` when the payload is shorter than the 12-byte
///   minimum (`[ver+flags:4] + [switch_group:2] + [alternate_group:2] +
///   [sub_track_ID:4]`).
/// * `Error::invalid` when the trailing attribute-list region length is
///   not a multiple of 4 bytes (each attribute is exactly an
///   `unsigned int(32)` per §8.14.4.2).
/// * `Error::invalid` when the FullBox version is non-zero (§8.14.4.2
///   fixes `version = 0`; a future revision could change the layout, so
///   we refuse rather than misparse).
///
/// FullBox flags are accepted and ignored: §8.14.4.2 fixes them at `0`
/// but the crate is uniformly tolerant of arbitrary flag bits.
pub fn parse_stri(payload: &[u8]) -> Result<SubTrackInformation> {
    if payload.len() < 12 {
        return Err(Error::invalid(format!(
            "MOV: stri payload {} < 12 bytes",
            payload.len()
        )));
    }
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!("MOV: stri version {version} != 0")));
    }
    // payload[1..4] = flags (ignored).
    let switch_group = i16::from_be_bytes([payload[4], payload[5]]);
    let alternate_group = i16::from_be_bytes([payload[6], payload[7]]);
    let sub_track_id = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let tail = &payload[12..];
    if tail.len() % 4 != 0 {
        return Err(Error::invalid(format!(
            "MOV: stri attribute-list tail {} bytes not multiple of 4",
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
    Ok(SubTrackInformation {
        switch_group,
        alternate_group,
        sub_track_id,
        attributes,
    })
}

/// Parse a `stsg` Sub Track Sample Group box payload (§8.14.6.2).
///
/// Expects the FullBox header (`[version:1][flags:3]`) followed by
/// `grouping_type:u32`, `item_count:u16`, and `item_count` ×
/// `group_description_index:u32`. Returns:
///
/// * `Error::invalid` when the payload is shorter than the 10-byte
///   header (`[ver+flags:4] + [grouping_type:4] + [item_count:2]`).
/// * `Error::invalid` when the FullBox version is non-zero (§8.14.6.2
///   declares `version = 0`).
/// * `Error::invalid` when the declared `item_count` would read past
///   the end of the payload (4 bytes per index).
pub fn parse_stsg(payload: &[u8]) -> Result<SubTrackSampleGroup> {
    if payload.len() < 10 {
        return Err(Error::invalid(format!(
            "MOV: stsg payload {} < 10 bytes",
            payload.len()
        )));
    }
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!("MOV: stsg version {version} != 0")));
    }
    // payload[1..4] = flags (ignored).
    let mut grouping_type = [0u8; 4];
    grouping_type.copy_from_slice(&payload[4..8]);
    let item_count = u16::from_be_bytes([payload[8], payload[9]]) as usize;
    let needed = 10 + item_count * 4;
    if payload.len() < needed {
        return Err(Error::invalid(format!(
            "MOV: stsg item_count {} needs {} bytes, have {}",
            item_count,
            needed,
            payload.len()
        )));
    }
    let mut group_description_indices = Vec::with_capacity(item_count);
    for i in 0..item_count {
        let off = 10 + i * 4;
        group_description_indices.push(u32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]));
    }
    Ok(SubTrackSampleGroup {
        grouping_type,
        group_description_indices,
    })
}

/// Walk a `strd` (Sub Track Definition, §8.14.5) payload and collect
/// every `stsg` child. `strd` is a bare `Box` container whose body is a
/// flat list of `[size:4][type:4][body]` child boxes; `stsg` is the only
/// child the spec defines (§8.14.6, `Quantity: Zero or more`). Unknown
/// children are skipped. A malformed child size stops the walk.
fn collect_stsg_in_strd(strd_payload: &[u8]) -> Result<Vec<SubTrackSampleGroup>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= strd_payload.len() {
        let size = u32::from_be_bytes([
            strd_payload[p],
            strd_payload[p + 1],
            strd_payload[p + 2],
            strd_payload[p + 3],
        ]) as usize;
        if size < 8 || p + size > strd_payload.len() {
            break;
        }
        let fc = &strd_payload[p + 4..p + 8];
        if fc == b"stsg" {
            out.push(parse_stsg(&strd_payload[p + 8..p + size])?);
        }
        p += size;
    }
    Ok(out)
}

/// Parse a single `strk` Sub Track box (§8.14.3) payload. `strk` is a
/// bare `Box` container; its body is a flat list of child boxes, of
/// which §8.14 defines exactly two: the mandatory `stri` (§8.14.4) and
/// the mandatory `strd` (§8.14.5). We surface the first `stri` and the
/// `stsg` entries found inside the first `strd`. A malformed child size
/// stops the walk; a missing `stri` leaves the default
/// [`SubTrackInformation`] (tolerated rather than rejected so a
/// non-conforming `strk` doesn't sink the rest of `udta`).
pub fn parse_strk(strk_payload: &[u8]) -> Result<SubTrack> {
    let mut information = SubTrackInformation::default();
    let mut saw_stri = false;
    let mut sample_groups = Vec::new();
    let mut p = 0usize;
    while p + 8 <= strk_payload.len() {
        let size = u32::from_be_bytes([
            strk_payload[p],
            strk_payload[p + 1],
            strk_payload[p + 2],
            strk_payload[p + 3],
        ]) as usize;
        if size < 8 || p + size > strk_payload.len() {
            break;
        }
        let fc = &strk_payload[p + 4..p + 8];
        let body = &strk_payload[p + 8..p + size];
        if fc == b"stri" && !saw_stri {
            // §8.14.4.1: Quantity One — keep the first.
            information = parse_stri(body)?;
            saw_stri = true;
        } else if fc == b"strd" {
            sample_groups.extend(collect_stsg_in_strd(body)?);
        }
        p += size;
    }
    Ok(SubTrack {
        information,
        sample_groups,
    })
}

/// Scan a raw track-level `udta` payload for every `strk` Sub Track box
/// (§8.14.3) and parse each one.
///
/// `udta` is a flat atom list (ISO/IEC 14496-12 §8.10.1 / QTFF p. 37);
/// each child is the usual `[size:4][type:4][body]`. §8.14.3.1 declares
/// `strk` `Quantity: Zero or more` inside the track's `udta`, so we
/// collect all of them in file order. A truncated or malformed `strk`
/// body propagates the parse error; an unrelated malformed entry stops
/// the walk (mirrors [`crate::track_selection::find_tsel_in_udta`]).
pub fn find_sub_tracks_in_udta(udta_payload: &[u8]) -> Result<Vec<SubTrack>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= udta_payload.len() {
        let size = u32::from_be_bytes([
            udta_payload[p],
            udta_payload[p + 1],
            udta_payload[p + 2],
            udta_payload[p + 3],
        ]) as usize;
        // §8.10.1 / QTFF p. 37: udta may be terminated by a 32-bit zero.
        if size == 0 && p + 4 == udta_payload.len() {
            break;
        }
        if size < 8 || p + size > udta_payload.len() {
            break;
        }
        let fc = &udta_payload[p + 4..p + 8];
        if fc == b"strk" {
            out.push(parse_strk(&udta_payload[p + 8..p + size])?);
        }
        p += size;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fullbox(version: u8, flags: u32) -> Vec<u8> {
        let mut v = vec![version];
        let f = flags.to_be_bytes();
        v.extend_from_slice(&f[1..4]);
        v
    }

    fn build_stri(
        version: u8,
        flags: u32,
        switch_group: i16,
        alternate_group: i16,
        sub_track_id: u32,
        attrs: &[[u8; 4]],
    ) -> Vec<u8> {
        let mut p = fullbox(version, flags);
        p.extend_from_slice(&switch_group.to_be_bytes());
        p.extend_from_slice(&alternate_group.to_be_bytes());
        p.extend_from_slice(&sub_track_id.to_be_bytes());
        for a in attrs {
            p.extend_from_slice(a);
        }
        p
    }

    fn build_stsg(grouping: &[u8; 4], indices: &[u32]) -> Vec<u8> {
        let mut p = fullbox(0, 0);
        p.extend_from_slice(grouping);
        p.extend_from_slice(&(indices.len() as u16).to_be_bytes());
        for i in indices {
            p.extend_from_slice(&i.to_be_bytes());
        }
        p
    }

    fn push_box(out: &mut Vec<u8>, fourcc: &[u8; 4], body: &[u8]) {
        let size: u32 = (8 + body.len()) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(fourcc);
        out.extend_from_slice(body);
    }

    #[test]
    fn stri_minimal_all_defaults() {
        let p = build_stri(0, 0, 0, 0, 0, &[]);
        let s = parse_stri(&p).unwrap();
        assert_eq!(s.switch_group, 0);
        assert_eq!(s.alternate_group, 0);
        assert_eq!(s.sub_track_id, 0);
        assert!(s.attributes.is_empty());
        assert!(!s.is_informative());
    }

    #[test]
    fn stri_fields_and_signed_groups() {
        // switch_group = -3, alternate_group = -1 must read as signed.
        let p = build_stri(0, 0, -3, -1, 0x1234_5678, &[]);
        let s = parse_stri(&p).unwrap();
        assert_eq!(s.switch_group, -3);
        assert_eq!(s.alternate_group, -1);
        assert_eq!(s.sub_track_id, 0x1234_5678);
        assert!(s.is_informative());
    }

    #[test]
    fn stri_attribute_list_classified() {
        let p = build_stri(0, 0, 5, 7, 42, &[*b"tesc", *b"bitr", *b"frar", *b"XvNd"]);
        let s = parse_stri(&p).unwrap();
        assert_eq!(s.attributes.len(), 4);
        assert!(s.has_attribute(b"tesc"));
        assert!(s.has_attribute(b"bitr"));
        let roles: Vec<_> = s.typed_attributes().collect();
        assert_eq!(roles[0].1, TsAttributeRole::Descriptive); // tesc
        assert_eq!(roles[1].1, TsAttributeRole::Differentiating); // bitr
        assert_eq!(roles[2].1, TsAttributeRole::Differentiating); // frar
        assert_eq!(roles[3].1, TsAttributeRole::Unknown); // XvNd
    }

    #[test]
    fn stri_below_minimum_errors() {
        // 11 bytes: one short of the 12-byte minimum.
        assert!(parse_stri(&[0u8; 11]).is_err());
    }

    #[test]
    fn stri_nonzero_version_rejected() {
        let p = build_stri(1, 0, 0, 0, 0, &[]);
        assert!(parse_stri(&p).is_err());
    }

    #[test]
    fn stri_attribute_tail_not_multiple_of_four_errors() {
        let mut p = build_stri(0, 0, 1, 1, 1, &[*b"tesc"]);
        p.push(0xAB);
        assert!(parse_stri(&p).is_err());
    }

    #[test]
    fn stri_flags_ignored() {
        let p = build_stri(0, 0x00FF_FFFF, 9, 0, 0, &[*b"bitr"]);
        let s = parse_stri(&p).unwrap();
        assert_eq!(s.switch_group, 9);
        assert_eq!(s.attributes, vec![*b"bitr"]);
    }

    #[test]
    fn stsg_round_trip() {
        let p = build_stsg(b"roll", &[1, 2, 3]);
        let g = parse_stsg(&p).unwrap();
        assert_eq!(&g.grouping_type, b"roll");
        assert_eq!(g.group_description_indices, vec![1, 2, 3]);
    }

    #[test]
    fn stsg_empty_item_list() {
        let p = build_stsg(b"rap ", &[]);
        let g = parse_stsg(&p).unwrap();
        assert_eq!(&g.grouping_type, b"rap ");
        assert!(g.group_description_indices.is_empty());
    }

    #[test]
    fn stsg_below_header_errors() {
        assert!(parse_stsg(&[0u8; 9]).is_err());
    }

    #[test]
    fn stsg_item_count_overruns_payload() {
        // Declare 5 entries but supply none.
        let mut p = fullbox(0, 0);
        p.extend_from_slice(b"roll");
        p.extend_from_slice(&5u16.to_be_bytes());
        assert!(parse_stsg(&p).is_err());
    }

    #[test]
    fn stsg_nonzero_version_rejected() {
        let mut p = build_stsg(b"roll", &[1]);
        p[0] = 1;
        assert!(parse_stsg(&p).is_err());
    }

    #[test]
    fn strk_with_stri_and_strd_stsg() {
        // strk { stri, strd { stsg, stsg } }
        let stri = build_stri(0, 0, 2, 4, 9, &[*b"tesc"]);
        let stsg1 = build_stsg(b"roll", &[1, 2]);
        let stsg2 = build_stsg(b"rap ", &[7]);
        let mut strd = Vec::new();
        push_box(&mut strd, b"stsg", &stsg1);
        push_box(&mut strd, b"stsg", &stsg2);
        let mut strk = Vec::new();
        push_box(&mut strk, b"stri", &stri);
        push_box(&mut strk, b"strd", &strd);
        let st = parse_strk(&strk).unwrap();
        assert_eq!(st.information.switch_group, 2);
        assert_eq!(st.information.alternate_group, 4);
        assert_eq!(st.information.sub_track_id, 9);
        assert_eq!(st.information.attributes, vec![*b"tesc"]);
        assert_eq!(st.sample_groups.len(), 2);
        assert_eq!(&st.sample_groups[0].grouping_type, b"roll");
        assert_eq!(st.sample_groups[0].group_description_indices, vec![1, 2]);
        assert_eq!(&st.sample_groups[1].grouping_type, b"rap ");
        assert_eq!(st.sample_groups[1].group_description_indices, vec![7]);
    }

    #[test]
    fn strk_strd_without_stsg_yields_no_groups() {
        let stri = build_stri(0, 0, 1, 0, 0, &[]);
        let strd: Vec<u8> = Vec::new();
        let mut strk = Vec::new();
        push_box(&mut strk, b"stri", &stri);
        push_box(&mut strk, b"strd", &strd);
        let st = parse_strk(&strk).unwrap();
        assert_eq!(st.information.switch_group, 1);
        assert!(st.sample_groups.is_empty());
    }

    #[test]
    fn strk_ignores_unknown_children() {
        let stri = build_stri(0, 0, 3, 0, 0, &[]);
        let mut strk = Vec::new();
        push_box(&mut strk, b"junk", &[1, 2, 3, 4]);
        push_box(&mut strk, b"stri", &stri);
        let st = parse_strk(&strk).unwrap();
        assert_eq!(st.information.switch_group, 3);
    }

    #[test]
    fn find_sub_tracks_collects_every_strk() {
        // udta with a text entry then two strk boxes.
        let mut udta = Vec::new();
        push_box(
            &mut udta,
            &[0xA9, b'n', b'a', b'm'],
            b"\x00\x05\x00\x00Title",
        );

        let stri_a = build_stri(0, 0, 1, 1, 10, &[]);
        let mut strk_a = Vec::new();
        push_box(&mut strk_a, b"stri", &stri_a);
        push_box(&mut udta, b"strk", &strk_a);

        let stri_b = build_stri(0, 0, 2, 1, 20, &[*b"vwsc"]);
        let stsg_b = build_stsg(b"roll", &[3]);
        let mut strd_b = Vec::new();
        push_box(&mut strd_b, b"stsg", &stsg_b);
        let mut strk_b = Vec::new();
        push_box(&mut strk_b, b"stri", &stri_b);
        push_box(&mut strk_b, b"strd", &strd_b);
        push_box(&mut udta, b"strk", &strk_b);

        let subs = find_sub_tracks_in_udta(&udta).unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].information.sub_track_id, 10);
        assert_eq!(subs[1].information.sub_track_id, 20);
        assert_eq!(subs[1].information.attributes, vec![*b"vwsc"]);
        assert_eq!(subs[1].sample_groups.len(), 1);
        assert_eq!(subs[1].sample_groups[0].group_description_indices, vec![3]);
    }

    #[test]
    fn find_sub_tracks_none_when_absent() {
        let mut udta = Vec::new();
        push_box(
            &mut udta,
            &[0xA9, b'n', b'a', b'm'],
            b"\x00\x05\x00\x00Title",
        );
        assert!(find_sub_tracks_in_udta(&udta).unwrap().is_empty());
    }

    #[test]
    fn find_sub_tracks_handles_zero_terminator() {
        let stri = build_stri(0, 0, 4, 0, 0, &[]);
        let mut strk = Vec::new();
        push_box(&mut strk, b"stri", &stri);
        let mut udta = Vec::new();
        push_box(&mut udta, b"strk", &strk);
        udta.extend_from_slice(&0u32.to_be_bytes());
        let subs = find_sub_tracks_in_udta(&udta).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].information.switch_group, 4);
    }

    #[test]
    fn find_sub_tracks_propagates_inner_error() {
        // strk whose stri is truncated below the 12-byte minimum.
        let mut strk = Vec::new();
        push_box(&mut strk, b"stri", &[0u8; 6]);
        let mut udta = Vec::new();
        push_box(&mut udta, b"strk", &strk);
        assert!(find_sub_tracks_in_udta(&udta).is_err());
    }
}
