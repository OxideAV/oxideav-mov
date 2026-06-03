//! Level Assignment Box (`leva`).
//!
//! ISO/IEC 14496-12:2015 §8.8.13 (pp. 63–64). A FullBox living
//! inside `moov/mvex` that names the *levels* the §8.16.4 Subsegment
//! Index Box (`ssix`) references. A level is a subset of the file's
//! fragmented data: samples mapped to level n may depend on any
//! samples of levels m ≤ n and shall not depend on any samples of
//! levels p > n (§8.8.13.1). Adaptive-streaming clients use the
//! pairing to fetch only the levels they need — for example, a
//! temporal-scalability decoder pulls the base-layer level and skips
//! the enhancement levels (§8.8.13.1 example).
//!
//! Layout per §8.8.13.2:
//!
//! ```text
//! aligned(8) class LevelAssignmentBox extends FullBox('leva', 0, 0) {
//!     unsigned int(8) level_count;
//!     for (j = 1; j <= level_count; j++) {
//!         unsigned int(32) track_id;
//!         unsigned int(1)  padding_flag;
//!         unsigned int(7)  assignment_type;
//!         if (assignment_type == 0) {
//!             unsigned int(32) grouping_type;
//!         } else if (assignment_type == 1) {
//!             unsigned int(32) grouping_type;
//!             unsigned int(32) grouping_type_parameter;
//!         } else if (assignment_type == 2) { }
//!         else if (assignment_type == 3) { }
//!         else if (assignment_type == 4) {
//!             unsigned int(32) sub_track_id;
//!         }
//!         // other assignment_type values are reserved
//!     }
//! }
//! ```
//!
//! §8.8.13.3 spec invariants enforced here:
//! * `level_count` shall be ≥ 2 ("level_count specifies the number of
//!   levels each fraction is grouped into. level_count shall be
//!   greater than or equal to 2.").
//! * `assignment_type` values above 4 are reserved.
//! * "The sequence of assignment_types is restricted to be a set of
//!   zero or more of type 2 or 3, followed by zero or more of exactly
//!   one type." A type 0/1/4 may follow a 2/3 prefix; once a non-2/3
//!   row appears, every subsequent row must carry the same
//!   `assignment_type` value.
//!
//! QTFF does not define `leva`; it is ISO BMFF-only and stays absent
//! for plain `.mov` inputs. The box appears at most once per file
//! ("Quantity: Zero or one", §8.8.13.1). When present it applies to
//! every movie fragment subsequent to the initial movie
//! (§8.8.13.1).

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// `assignment_type` value from one [`LevaLevel`] row (§8.8.13.2 /
/// §8.8.13.3). The on-disk field is 7 bits wide so legal values fall
/// in `0..=127`; the spec assigns semantics to `0..=4` only and
/// reserves the rest. Unknown values are surfaced as
/// [`AssignmentType::Reserved`] rather than rejected so a future
/// derived spec adding a new code does not break this parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignmentType {
    /// `assignment_type == 0` — sample groups specify levels.
    /// Samples mapped to different sample-group-description indexes
    /// of a particular sample grouping lie in different levels within
    /// the identified track; other tracks must have all their data
    /// in precisely one level (§8.8.13.3).
    SampleGroup {
        /// `grouping_type` FourCC keying into the §8.9.3 Sample Group
        /// Description Box.
        grouping_type: [u8; 4],
    },
    /// `assignment_type == 1` — as for type 0 except assignment is
    /// by a *parameterized* sample group (§8.8.13.3).
    ParameterizedSampleGroup {
        /// `grouping_type` FourCC.
        grouping_type: [u8; 4],
        /// `grouping_type_parameter` selecting one of the §8.9.2.3
        /// parameterised groupings sharing `grouping_type`.
        grouping_type_parameter: u32,
    },
    /// `assignment_type == 2` — level assignment is by track (see
    /// §8.16.4 Subsegment Index Box for the type 2 vs type 3
    /// processing distinction).
    Track,
    /// `assignment_type == 3` — level assignment is by track
    /// (see §8.16.4 for the type 2 vs type 3 distinction).
    TrackAlternate,
    /// `assignment_type == 4` — the level contains the samples for a
    /// sub-track named by `sub_track_id` (§8.8.13.3). Sub-tracks are
    /// specified through the §8.14 Sub Track Box; other tracks must
    /// have all their data in precisely one level.
    SubTrack {
        /// `sub_track_id` keying into the named sub-track.
        sub_track_id: u32,
    },
    /// `assignment_type` value outside the spec's `0..=4` range.
    /// §8.8.13.2 marks them reserved; surfaced verbatim so future
    /// derived specs can introduce new codes without breaking this
    /// parser. No row-trailer bytes are consumed for an unknown code
    /// because the spec does not specify a payload — a reserved code
    /// at parse time is treated as a row that carries only the
    /// `track_id` + flag/type byte. Callers that need to recognise
    /// a specific reserved code own that responsibility.
    Reserved {
        /// The on-disk 7-bit `assignment_type` value (`5..=127`).
        raw: u8,
    },
}

/// One level row from a [`Leva`] (§8.8.13.2). The 1-based level index
/// is the row's position in [`Leva::levels`] + 1 (§8.8.13.3:
/// "track_id for loop entry j specifies the track identifier of the
/// track assigned to level j").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevaLevel {
    /// `track_id` of the track assigned to this level (§8.8.13.3).
    pub track_id: u32,
    /// `padding_flag` — `true` when "a conforming fraction can be
    /// formed by concatenating any positive integer number of levels
    /// within a fraction and padding the last Media Data box by zero
    /// bytes up to the full size that is indicated in the header of
    /// the last Media Data box" (§8.8.13.3). `false` (the
    /// not-padded case) leaves that property unasserted.
    pub padding_flag: bool,
    /// Decoded `assignment_type` (the 7-bit on-disk value), carrying
    /// any per-type trailer.
    pub assignment_type: AssignmentType,
}

/// Parsed Level Assignment Box (ISO/IEC 14496-12 §8.8.13).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leva {
    /// Level rows in declaration order. `levels.len()` equals the
    /// on-disk `level_count` (§8.8.13.3), which is ≥ 2 by spec.
    pub levels: Vec<LevaLevel>,
}

impl Leva {
    /// Number of levels declared by this box — the on-disk
    /// `level_count` field (§8.8.13.3). Convenience accessor that
    /// matches the spec field name.
    pub fn level_count(&self) -> u8 {
        // `parse_leva` rejects a count above 255 by virtue of the
        // single-byte on-disk encoding, so the `as` cast is exact.
        self.levels.len() as u8
    }

    /// Look up the row assigned to spec-level `level` (1-based per
    /// §8.8.13.3 "loop entry j"). Returns `None` for `level == 0`
    /// (the spec numbers levels from 1) or any value past the
    /// declared `level_count`.
    pub fn level(&self, level: u8) -> Option<&LevaLevel> {
        if level == 0 {
            return None;
        }
        self.levels.get((level - 1) as usize)
    }

    /// Return every track id named by the box's `track_id` fields,
    /// in declaration order, de-duplicated first-occurrence-wins.
    /// Useful when wiring `leva` to a §8.8 track table: the caller
    /// needs the set of tracks the level scheme touches without
    /// caring about per-level repetition (a single track may carry
    /// several levels through `assignment_type == 0` sample
    /// grouping).
    pub fn track_ids(&self) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for lvl in &self.levels {
            if !out.contains(&lvl.track_id) {
                out.push(lvl.track_id);
            }
        }
        out
    }
}

/// Parse a `leva` payload.
///
/// Layout per ISO/IEC 14496-12 §8.8.13.2 — see the module-level docs.
///
/// Returns `Error::invalid` when:
/// * the payload is shorter than the 4-byte FullBox header + the
///   `level_count` u8,
/// * the FullBox `version` is non-zero (the spec fixes it at 0),
/// * `level_count < 2` (§8.8.13.3),
/// * the body is too short to hold every declared row (each row is
///   at least 5 bytes: `track_id` u32 + 1 byte holding
///   `padding_flag` + `assignment_type`; types 0 / 1 / 4 add a 4-
///   or 8-byte trailer),
/// * a row's `assignment_type` trailer overruns the remaining body,
/// * §8.8.13.3 ordering: the sequence of `assignment_type` values is
///   "a set of zero or more of type 2 or 3, followed by zero or more
///   of exactly one type". A type 0 / 1 / 4 row is therefore allowed
///   anywhere after the optional 2/3 prefix, but once a non-2/3 row
///   appears every subsequent non-2/3 row must carry the same
///   `assignment_type` value. The same row may not mix, e.g., a
///   `SampleGroup` with a later `ParameterizedSampleGroup`,
/// * any trailing bytes remain after the declared row list (the box
///   carries no list past `level_count` — leftover bytes signal a
///   malformed writer).
///
/// `Reserved { raw }` rows are accepted at the ordering check (a
/// reserved code is treated as the "exactly one type" the ordering
/// rule mentions); they consume only the 5-byte row prefix. A
/// derived spec adding a trailer for a new code will need to extend
/// this parser, but until then a forward-compatible writer that
/// emits a reserved code at the box tail does not break parsing.
pub fn parse_leva(payload: &[u8]) -> Result<Leva> {
    if payload.len() < 5 {
        return Err(Error::invalid(format!(
            "MOV: leva payload {} < 5-byte FullBox header + level_count",
            payload.len()
        )));
    }
    let version = payload[0];
    if version != 0 {
        return Err(Error::invalid(format!(
            "MOV: leva unknown version {version} (spec fixes at 0)"
        )));
    }
    // `flags` (payload[1..4]) is fixed at 0 by §8.8.13.2; vendors
    // occasionally set bits, so the parser tolerates them silently —
    // matching the `sidx` / `ssix` parser convention.

    let level_count = payload[4];
    if level_count < 2 {
        return Err(Error::invalid(format!(
            "MOV: leva level_count {level_count} < 2 (§8.8.13.3)"
        )));
    }

    // Bound the up-front allocation: every row carries at least 5
    // bytes (`track_id` u32 + 1 byte holding the flag+type). A body
    // shorter than `level_count * 5` after the header cannot hold
    // even the minimum-shape rows — refuse before allocating.
    let mut pos = 5usize;
    let remaining = payload.len() - pos;
    let min_rows_bytes = (level_count as u64) * 5;
    if min_rows_bytes > remaining as u64 {
        return Err(Error::invalid(format!(
            "MOV: leva level_count {level_count} needs ≥ {min_rows_bytes} body bytes \
             but only {remaining} remain",
        )));
    }

    let mut levels: Vec<LevaLevel> = Vec::with_capacity(level_count as usize);
    // §8.8.13.3 ordering tracker. None until we see the first non-
    // 2/3 row, then pinned to that row's discriminant.
    let mut pinned_kind: Option<AssignmentKind> = None;
    for j in 0..level_count {
        if pos + 5 > payload.len() {
            return Err(Error::invalid(format!(
                "MOV: leva row {} truncated reading track_id+flag/type",
                j + 1
            )));
        }
        let track_id = u32::from_be_bytes([
            payload[pos],
            payload[pos + 1],
            payload[pos + 2],
            payload[pos + 3],
        ]);
        let flag_type = payload[pos + 4];
        let padding_flag = (flag_type & 0x80) != 0;
        let assignment_type_raw = flag_type & 0x7F;
        pos += 5;

        let (assignment_type, kind) = match assignment_type_raw {
            0 => {
                if pos + 4 > payload.len() {
                    return Err(Error::invalid(format!(
                        "MOV: leva row {} type=0 truncated reading grouping_type",
                        j + 1
                    )));
                }
                let grouping_type = [
                    payload[pos],
                    payload[pos + 1],
                    payload[pos + 2],
                    payload[pos + 3],
                ];
                pos += 4;
                (
                    AssignmentType::SampleGroup { grouping_type },
                    AssignmentKind::SampleGroup,
                )
            }
            1 => {
                if pos + 8 > payload.len() {
                    return Err(Error::invalid(format!(
                        "MOV: leva row {} type=1 truncated reading grouping_type / \
                         grouping_type_parameter",
                        j + 1
                    )));
                }
                let grouping_type = [
                    payload[pos],
                    payload[pos + 1],
                    payload[pos + 2],
                    payload[pos + 3],
                ];
                let grouping_type_parameter = u32::from_be_bytes([
                    payload[pos + 4],
                    payload[pos + 5],
                    payload[pos + 6],
                    payload[pos + 7],
                ]);
                pos += 8;
                (
                    AssignmentType::ParameterizedSampleGroup {
                        grouping_type,
                        grouping_type_parameter,
                    },
                    AssignmentKind::ParameterizedSampleGroup,
                )
            }
            2 => (AssignmentType::Track, AssignmentKind::Track),
            3 => (
                AssignmentType::TrackAlternate,
                AssignmentKind::TrackAlternate,
            ),
            4 => {
                if pos + 4 > payload.len() {
                    return Err(Error::invalid(format!(
                        "MOV: leva row {} type=4 truncated reading sub_track_id",
                        j + 1
                    )));
                }
                let sub_track_id = u32::from_be_bytes([
                    payload[pos],
                    payload[pos + 1],
                    payload[pos + 2],
                    payload[pos + 3],
                ]);
                pos += 4;
                (
                    AssignmentType::SubTrack { sub_track_id },
                    AssignmentKind::SubTrack,
                )
            }
            other => (
                AssignmentType::Reserved { raw: other },
                AssignmentKind::Reserved { raw: other },
            ),
        };

        // §8.8.13.3 ordering enforcement. The sequence is "a set of
        // zero or more of type 2 or 3, followed by zero or more of
        // exactly one type". Types 2 and 3 may interleave freely in
        // the prefix; the first non-2/3 row pins the tail kind, and
        // every subsequent non-2/3 row must match that pin.
        let is_prefix_kind = matches!(kind, AssignmentKind::Track | AssignmentKind::TrackAlternate);
        if !is_prefix_kind {
            match pinned_kind {
                None => pinned_kind = Some(kind),
                Some(p) if p == kind => {}
                Some(p) => {
                    return Err(Error::invalid(format!(
                        "MOV: leva row {} assignment_type kind {:?} contradicts pinned kind \
                         {:?} (§8.8.13.3 sequence rule)",
                        j + 1,
                        kind,
                        p,
                    )));
                }
            }
        } else if pinned_kind.is_some() {
            // A type 2/3 row after a pinned non-2/3 row violates the
            // "zero or more of type 2 or 3, *followed by* zero or
            // more of exactly one type" structure: the 2/3 prefix
            // must precede the tail block.
            return Err(Error::invalid(format!(
                "MOV: leva row {} type=2/3 follows a pinned non-2/3 tail \
                 (§8.8.13.3 sequence rule)",
                j + 1,
            )));
        }

        levels.push(LevaLevel {
            track_id,
            padding_flag,
            assignment_type,
        });
    }

    if pos != payload.len() {
        return Err(Error::invalid(format!(
            "MOV: leva has {} bytes of unconsumed trailing data after \
             {level_count} levels",
            payload.len() - pos
        )));
    }

    Ok(Leva { levels })
}

/// Tag-only discriminant used internally to enforce §8.8.13.3's
/// ordering rule. `Reserved` rows are tagged by their raw on-disk
/// value so a tail block of repeated reserved-code rows is accepted
/// (the rule says "exactly one type", not "exactly one *spec-known*
/// type").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssignmentKind {
    SampleGroup,
    ParameterizedSampleGroup,
    Track,
    TrackAlternate,
    SubTrack,
    Reserved { raw: u8 },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `leva` body from a sequence of `(track_id,
    /// padding_flag, assignment_type byte, trailer bytes)` rows.
    fn build_leva(rows: &[(u32, bool, u8, Vec<u8>)]) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(0u8); // version
        p.extend_from_slice(&[0u8, 0, 0]); // flags
        p.push(rows.len() as u8); // level_count
        for (tid, pad, ty, trailer) in rows {
            p.extend_from_slice(&tid.to_be_bytes());
            let mut flag_type = ty & 0x7F;
            if *pad {
                flag_type |= 0x80;
            }
            p.push(flag_type);
            p.extend_from_slice(trailer);
        }
        p
    }

    #[test]
    fn parses_two_track_levels() {
        // Two rows, both assignment_type == 2 (track). No trailers.
        let p = build_leva(&[(1, false, 2, vec![]), (2, true, 2, vec![])]);
        let leva = parse_leva(&p).unwrap();
        assert_eq!(leva.level_count(), 2);
        assert_eq!(leva.levels[0].track_id, 1);
        assert!(!leva.levels[0].padding_flag);
        assert_eq!(leva.levels[0].assignment_type, AssignmentType::Track);
        assert_eq!(leva.levels[1].track_id, 2);
        assert!(leva.levels[1].padding_flag);
    }

    #[test]
    fn parses_sample_group_assignment() {
        // Type-0 row carries a 4-byte grouping_type.
        let p = build_leva(&[(1, false, 2, vec![]), (1, false, 0, b"roll".to_vec())]);
        let leva = parse_leva(&p).unwrap();
        assert_eq!(
            leva.levels[1].assignment_type,
            AssignmentType::SampleGroup {
                grouping_type: *b"roll"
            }
        );
    }

    #[test]
    fn parses_parameterized_sample_group() {
        // Type-1 row carries grouping_type + grouping_type_parameter.
        let mut trailer = Vec::new();
        trailer.extend_from_slice(b"tscl");
        trailer.extend_from_slice(&7u32.to_be_bytes());
        let p = build_leva(&[(1, false, 2, vec![]), (1, false, 1, trailer)]);
        let leva = parse_leva(&p).unwrap();
        assert_eq!(
            leva.levels[1].assignment_type,
            AssignmentType::ParameterizedSampleGroup {
                grouping_type: *b"tscl",
                grouping_type_parameter: 7,
            }
        );
    }

    #[test]
    fn parses_sub_track_assignment() {
        // Type-4 row carries a 4-byte sub_track_id.
        let trailer = 42u32.to_be_bytes().to_vec();
        let p = build_leva(&[(1, false, 2, vec![]), (1, false, 4, trailer)]);
        let leva = parse_leva(&p).unwrap();
        assert_eq!(
            leva.levels[1].assignment_type,
            AssignmentType::SubTrack { sub_track_id: 42 }
        );
    }

    #[test]
    fn type_3_track_alternate_decodes() {
        let p = build_leva(&[(1, false, 2, vec![]), (1, false, 3, vec![])]);
        let leva = parse_leva(&p).unwrap();
        assert_eq!(
            leva.levels[1].assignment_type,
            AssignmentType::TrackAlternate
        );
    }

    #[test]
    fn reserved_assignment_type_is_surfaced_verbatim() {
        // Type 5 is reserved per §8.8.13.2. Parser surfaces the raw
        // value rather than rejecting so a future derived spec
        // adding the code doesn't break this parser.
        let p = build_leva(&[(1, false, 2, vec![]), (1, false, 5, vec![])]);
        let leva = parse_leva(&p).unwrap();
        assert_eq!(
            leva.levels[1].assignment_type,
            AssignmentType::Reserved { raw: 5 }
        );
    }

    #[test]
    fn level_count_below_2_rejected() {
        // §8.8.13.3 spec-fixes the minimum at 2.
        let mut p = build_leva(&[(1, false, 2, vec![])]);
        // Patch level_count to 1 explicitly (build_leva would already
        // have written 1, but make the intent clear).
        p[4] = 1;
        assert!(parse_leva(&p).is_err());
    }

    #[test]
    fn level_count_zero_rejected() {
        let mut p = build_leva(&[(1, false, 2, vec![])]);
        p[4] = 0;
        assert!(parse_leva(&p).is_err());
    }

    #[test]
    fn unknown_version_rejected() {
        let mut p = build_leva(&[(1, false, 2, vec![]), (2, false, 2, vec![])]);
        p[0] = 1; // spec fixes version at 0
        assert!(parse_leva(&p).is_err());
    }

    #[test]
    fn truncated_header_rejected() {
        // Fewer than 5 bytes: spec needs ver+flags+level_count.
        let p = vec![0u8, 0, 0, 0];
        assert!(parse_leva(&p).is_err());
    }

    #[test]
    fn truncated_row_prefix_rejected() {
        // level_count = 2 but body holds only one full row.
        let mut p = build_leva(&[(1, false, 2, vec![]), (2, false, 2, vec![])]);
        // Drop the last row's last byte so the row-prefix read
        // can't complete.
        p.truncate(p.len() - 1);
        assert!(parse_leva(&p).is_err());
    }

    #[test]
    fn truncated_sample_group_trailer_rejected() {
        let mut p = build_leva(&[(1, false, 2, vec![]), (1, false, 0, b"roll".to_vec())]);
        // Drop the last 2 bytes of the grouping_type trailer.
        p.truncate(p.len() - 2);
        assert!(parse_leva(&p).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut p = build_leva(&[(1, false, 2, vec![]), (2, false, 2, vec![])]);
        p.extend_from_slice(&[0u8, 0]);
        assert!(parse_leva(&p).is_err());
    }

    #[test]
    fn ordering_rule_allows_2_3_prefix_then_one_tail_kind() {
        // 2, 3, 2 prefix + two type-0 rows sharing the same kind.
        let p = build_leva(&[
            (1, false, 2, vec![]),
            (2, false, 3, vec![]),
            (3, false, 2, vec![]),
            (4, false, 0, b"aaaa".to_vec()),
            (5, false, 0, b"bbbb".to_vec()),
        ]);
        let leva = parse_leva(&p).unwrap();
        assert_eq!(leva.level_count(), 5);
    }

    #[test]
    fn ordering_rule_mixed_tail_kinds_rejected() {
        // Tail-block kinds must match exactly: a type-0 row followed
        // by a type-1 row violates "exactly one type".
        let mut trailer1 = Vec::new();
        trailer1.extend_from_slice(b"bbbb");
        trailer1.extend_from_slice(&0u32.to_be_bytes());
        let p = build_leva(&[
            (1, false, 2, vec![]),
            (2, false, 0, b"aaaa".to_vec()),
            (3, false, 1, trailer1),
        ]);
        assert!(parse_leva(&p).is_err());
    }

    #[test]
    fn ordering_rule_track_after_pinned_tail_rejected() {
        // A type-2 row that follows a pinned non-2/3 tail row
        // contradicts "zero or more of type 2/3, *followed by* the
        // tail block".
        let p = build_leva(&[(1, false, 0, b"aaaa".to_vec()), (2, false, 2, vec![])]);
        assert!(parse_leva(&p).is_err());
    }

    #[test]
    fn padding_flag_round_trips_for_every_assignment_type() {
        // Confirms the high bit of the flag/type byte routes to
        // `padding_flag` and the low 7 bits to `assignment_type`
        // independently — a writer setting both `pad=true` and
        // `ty=4` must reach an `AssignmentType::SubTrack` row whose
        // `padding_flag == true`.
        let trailer = 99u32.to_be_bytes().to_vec();
        let p = build_leva(&[(1, false, 2, vec![]), (7, true, 4, trailer)]);
        let leva = parse_leva(&p).unwrap();
        assert!(leva.levels[1].padding_flag);
        assert_eq!(
            leva.levels[1].assignment_type,
            AssignmentType::SubTrack { sub_track_id: 99 }
        );
    }

    #[test]
    fn level_accessor_is_1_based_per_spec() {
        let p = build_leva(&[(11, false, 2, vec![]), (22, false, 2, vec![])]);
        let leva = parse_leva(&p).unwrap();
        assert!(leva.level(0).is_none());
        assert_eq!(leva.level(1).unwrap().track_id, 11);
        assert_eq!(leva.level(2).unwrap().track_id, 22);
        assert!(leva.level(3).is_none());
    }

    #[test]
    fn track_ids_dedupes_first_wins() {
        // Three rows: track 1, track 2, track 1 again — the helper
        // surfaces { 1, 2 } in first-occurrence order.
        let p = build_leva(&[
            (1, false, 2, vec![]),
            (2, false, 2, vec![]),
            (1, false, 2, vec![]),
        ]);
        let leva = parse_leva(&p).unwrap();
        assert_eq!(leva.track_ids(), vec![1, 2]);
    }

    #[test]
    fn level_count_max_255_fits_in_byte_accessor() {
        // 255 type-2 rows = the maximum encodable by the 1-byte
        // `level_count` field. Confirms the `as u8` cast in
        // `level_count()` is exact at the boundary and the parser
        // doesn't truncate.
        let rows: Vec<(u32, bool, u8, Vec<u8>)> = (0..255u32)
            .map(|i| (i + 1, false, 2u8, Vec::new()))
            .collect();
        let p = build_leva(&rows);
        let leva = parse_leva(&p).unwrap();
        assert_eq!(leva.level_count(), 255);
        assert_eq!(leva.levels.len(), 255);
    }
}
