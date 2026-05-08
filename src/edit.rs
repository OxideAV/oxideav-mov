//! Edit-list (`edts` / `elst`) parsing.
//!
//! QTFF Chapter 2, "Edit Atoms" (pp. 46–48). The edit list maps
//! presentation time in the movie's timeline to media time in the
//! track's media. Three quirks make this non-trivial:
//!
//! 1. A `media_time` of -1 marks an *empty edit* — the player should
//!    insert silence/black for the edit's `track_duration` before
//!    consuming any media. The last edit must never be empty (QTFF
//!    p. 47, "Media time").
//! 2. A `track_duration` of 0 with a non-empty `media_time` is a no-op
//!    pause — some authoring tools emit it. We accept it.
//! 3. `media_rate` is a 16.16 signed fixed-point number; the spec
//!    forbids 0 or negative (QTFF p. 48, "Media rate"), but we accept
//!    them as parsed and let the caller decide whether to reject.
//!
//! ISO BMFF (ISO/IEC 14496-12) defines the same atom plus a v1 layout
//! with 64-bit `track_duration` + 64-bit `media_time`. We parse both.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// A single edit list entry.
///
/// `media_time = -1` marks an *empty* edit (per QTFF p. 47); the
/// helper [`Edit::is_empty`] surfaces that condition. `track_duration`
/// is in *movie* timescale units; `media_time` is in *media* timescale
/// units (mdhd.time_scale).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    /// Duration of this edit in movie-timescale units.
    pub track_duration: u64,
    /// Starting position in the media. Negative values indicate an
    /// empty edit; we store as `i64` so v0 sentinels survive.
    pub media_time: i64,
    /// 16.16 signed fixed-point relative playback rate. 0x0001_0000
    /// (= 1.0) is normal speed.
    pub media_rate: i32,
}

impl Edit {
    /// True for an *empty edit* (QTFF p. 47): the player should emit
    /// `track_duration` of fill before any media is consumed.
    pub fn is_empty(&self) -> bool {
        self.media_time < 0
    }
}

/// A track's edit list — the ordered sequence of [`Edit`] entries.
pub type EditList = Vec<Edit>;

/// Parse an `elst` payload.
///
/// Layout per QTFF Figure 2-11 (p. 47): `[ver+flags=4][n=4]` followed
/// by `n × {track_duration, media_time, media_rate}` triples; entry
/// width is 12 for version-0, 20 for version-1 (ISO BMFF extension).
pub fn parse_elst(payload: &[u8]) -> Result<EditList> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: elst payload < 8 bytes"));
    }
    let version = payload[0];
    let n = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let body = &payload[8..];
    let entry_size = match version {
        0 => 12usize,
        1 => 20usize,
        v => return Err(Error::invalid(format!("MOV: elst unknown version {v}"))),
    };
    let need = (n as usize)
        .checked_mul(entry_size)
        .ok_or_else(|| Error::invalid("MOV: elst entry count overflow"))?;
    if body.len() < need {
        return Err(Error::invalid("MOV: elst truncated table"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        let off = i * entry_size;
        let edit = match version {
            0 => Edit {
                track_duration: u32::from_be_bytes([
                    body[off],
                    body[off + 1],
                    body[off + 2],
                    body[off + 3],
                ]) as u64,
                // media_time is signed (sentinel value -1 = empty).
                media_time: i32::from_be_bytes([
                    body[off + 4],
                    body[off + 5],
                    body[off + 6],
                    body[off + 7],
                ]) as i64,
                media_rate: i32::from_be_bytes([
                    body[off + 8],
                    body[off + 9],
                    body[off + 10],
                    body[off + 11],
                ]),
            },
            _ => Edit {
                track_duration: u64::from_be_bytes([
                    body[off],
                    body[off + 1],
                    body[off + 2],
                    body[off + 3],
                    body[off + 4],
                    body[off + 5],
                    body[off + 6],
                    body[off + 7],
                ]),
                media_time: i64::from_be_bytes([
                    body[off + 8],
                    body[off + 9],
                    body[off + 10],
                    body[off + 11],
                    body[off + 12],
                    body[off + 13],
                    body[off + 14],
                    body[off + 15],
                ]),
                media_rate: i32::from_be_bytes([
                    body[off + 16],
                    body[off + 17],
                    body[off + 18],
                    body[off + 19],
                ]),
            },
        };
        out.push(edit);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_elst_v0(entries: &[(u32, i32, i32)]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver=0 + flags
        p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (dur, mt, rate) in entries {
            p.extend_from_slice(&dur.to_be_bytes());
            p.extend_from_slice(&mt.to_be_bytes());
            p.extend_from_slice(&rate.to_be_bytes());
        }
        p
    }

    #[test]
    fn empty_edit_marker_recognised() {
        // Empty edit (mt = -1) followed by a 200-tick segment at media 0.
        let p = build_elst_v0(&[(100, -1, 0x0001_0000), (200, 0, 0x0001_0000)]);
        let v = parse_elst(&p).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].is_empty());
        assert_eq!(v[0].track_duration, 100);
        assert!(!v[1].is_empty());
        assert_eq!(v[1].media_time, 0);
        assert_eq!(v[1].media_rate, 0x0001_0000);
    }

    #[test]
    fn version1_entries_widen_to_64bit() {
        // ver=1, one entry: dur=u64::MAX-1, mt=-2, rate=0x10000
        let mut p = Vec::new();
        p.push(1); // version
        p.extend_from_slice(&[0, 0, 0]); // flags
        p.extend_from_slice(&1u32.to_be_bytes());
        let dur: u64 = u64::MAX - 1;
        p.extend_from_slice(&dur.to_be_bytes());
        let mt: i64 = -2;
        p.extend_from_slice(&mt.to_be_bytes());
        let rate: i32 = 0x0001_0000;
        p.extend_from_slice(&rate.to_be_bytes());
        let v = parse_elst(&p).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].track_duration, dur);
        assert_eq!(v[0].media_time, mt);
        assert!(v[0].is_empty());
    }

    #[test]
    fn truncated_table_errors() {
        // Declares 4 entries but only carries 2 worth of bytes.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&4u32.to_be_bytes());
        p.extend_from_slice(&[0u8; 24]); // 2 entries
        assert!(parse_elst(&p).is_err());
    }

    #[test]
    fn unknown_version_errors() {
        let mut p = Vec::new();
        p.push(2); // bad version
        p.extend_from_slice(&[0, 0, 0]);
        p.extend_from_slice(&0u32.to_be_bytes());
        assert!(parse_elst(&p).is_err());
    }
}
