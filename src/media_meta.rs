//! Apple-specific media metadata atoms.
//!
//! This module covers the Apple-extended boxes that decorate visual
//! and audio sample descriptions plus track-level metadata:
//!
//! * `gama` — gamma 16.16 fixed-point (QTFF p. 94, Table 3-2).
//! * `clap` — Clean Aperture (ISO BMFF §12.1.4, also Apple).
//! * `pasp` — Pixel Aspect Ratio (ISO BMFF §12.1.4).
//! * `colr` — Colour Information (Apple `nclc` *or* ISO `nclx`,
//!   distinguished by the leading 4-byte `colorParameterType`).
//! * `tapt` — Apple Track Aperture Mode Dimensions (`clef`/`prof`/
//!   `enof`); each child carries a 16.16 fixed-point width × height.
//! * `chan` — Audio Channel Layout (Apple Core Audio Format extension);
//!   we surface the leading layout-tag fields and leave the variable-
//!   length channel-description list as raw bytes for round 3.
//! * Apple-shaped `meta` — `hdlr` (typically `mdta`) + `keys` + `ilst`
//!   key-value pairs. We surface a flat `Vec<MetaKeyValue>`.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Pixel Aspect Ratio (ISO BMFF §12.1.4.2). `hSpacing` / `vSpacing`
/// is the ratio of pixel-width to pixel-height in arbitrary units;
/// only the ratio matters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pasp {
    pub h_spacing: u32,
    pub v_spacing: u32,
}

/// Parse a `pasp` payload (8 bytes).
pub fn parse_pasp(payload: &[u8]) -> Result<Pasp> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: pasp payload < 8 bytes"));
    }
    Ok(Pasp {
        h_spacing: u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
        v_spacing: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
    })
}

/// Clean Aperture region (ISO BMFF §12.1.4). Eight 32-bit values
/// representing four fractions: width N/D, height N/D, horiz off N/D,
/// vert off N/D. The offset numerators are signed in the spec; we keep
/// them as `i32` so the sign survives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clap {
    pub clean_aperture_width_n: u32,
    pub clean_aperture_width_d: u32,
    pub clean_aperture_height_n: u32,
    pub clean_aperture_height_d: u32,
    pub horiz_off_n: i32,
    pub horiz_off_d: u32,
    pub vert_off_n: i32,
    pub vert_off_d: u32,
}

/// Parse a `clap` payload (32 bytes).
pub fn parse_clap(payload: &[u8]) -> Result<Clap> {
    if payload.len() < 32 {
        return Err(Error::invalid("MOV: clap payload < 32 bytes"));
    }
    let r32 =
        |o: usize| u32::from_be_bytes([payload[o], payload[o + 1], payload[o + 2], payload[o + 3]]);
    let i =
        |o: usize| i32::from_be_bytes([payload[o], payload[o + 1], payload[o + 2], payload[o + 3]]);
    Ok(Clap {
        clean_aperture_width_n: r32(0),
        clean_aperture_width_d: r32(4),
        clean_aperture_height_n: r32(8),
        clean_aperture_height_d: r32(12),
        horiz_off_n: i(16),
        horiz_off_d: r32(20),
        vert_off_n: i(24),
        vert_off_d: r32(28),
    })
}

/// Colour parameter atom payload variants.
///
/// The leading 4 bytes of a `colr` payload are a FourCC discriminator:
///
/// * `nclc` (Apple, QTFF) — three u16 indices: primaries, transfer,
///   matrix. 6 trailing bytes total.
/// * `nclx` (ISO BMFF §12.1.5) — same three u16 indices plus a 1-byte
///   field whose top bit is `full_range_flag`; 7 trailing bytes total.
/// * `rICC` / `prof` (ISO BMFF §12.1.5) — embedded ICC profile bytes,
///   surfaced as raw blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorParametersKind {
    Nclc {
        primaries: u16,
        transfer: u16,
        matrix: u16,
    },
    Nclx {
        primaries: u16,
        transfer: u16,
        matrix: u16,
        full_range: bool,
    },
    Icc {
        /// `rICC` (restricted) or `prof` (unrestricted).
        kind: [u8; 4],
        profile: Vec<u8>,
    },
    Other {
        kind: [u8; 4],
        body: Vec<u8>,
    },
}

/// Parsed `colr` atom; the `kind` discriminates the layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorParameters {
    pub kind: ColorParametersKind,
}

/// Parse a `colr` payload.
pub fn parse_colr(payload: &[u8]) -> Result<ColorParameters> {
    if payload.len() < 4 {
        return Err(Error::invalid("MOV: colr payload < 4 bytes (no type)"));
    }
    let mut t = [0u8; 4];
    t.copy_from_slice(&payload[..4]);
    let body = &payload[4..];
    let kind = match &t {
        b"nclc" => {
            if body.len() < 6 {
                return Err(Error::invalid("MOV: colr nclc < 6 bytes"));
            }
            ColorParametersKind::Nclc {
                primaries: u16::from_be_bytes([body[0], body[1]]),
                transfer: u16::from_be_bytes([body[2], body[3]]),
                matrix: u16::from_be_bytes([body[4], body[5]]),
            }
        }
        b"nclx" => {
            if body.len() < 7 {
                return Err(Error::invalid("MOV: colr nclx < 7 bytes"));
            }
            ColorParametersKind::Nclx {
                primaries: u16::from_be_bytes([body[0], body[1]]),
                transfer: u16::from_be_bytes([body[2], body[3]]),
                matrix: u16::from_be_bytes([body[4], body[5]]),
                full_range: (body[6] & 0x80) != 0,
            }
        }
        b"rICC" | b"prof" => ColorParametersKind::Icc {
            kind: t,
            profile: body.to_vec(),
        },
        _ => ColorParametersKind::Other {
            kind: t,
            body: body.to_vec(),
        },
    };
    Ok(ColorParameters { kind })
}

/// Apple Track Aperture Mode Dimensions (`tapt`).
///
/// `tapt` contains three optional sub-atoms, each carrying a 16.16
/// fixed-point width × height pair (8 bytes per sub-atom plus the
/// FullBox 4-byte version+flags header):
///
/// * `clef` — clean aperture dimensions
/// * `prof` — production aperture dimensions
/// * `enof` — encoded pixels dimensions
///
/// The dimensions are in pixels; integer portion = `value >> 16`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tapt {
    pub clef: Option<TaptDims>,
    pub prof: Option<TaptDims>,
    pub enof: Option<TaptDims>,
}

/// Width × height in 16.16 fixed-point pixels, as stored in tapt sub-atoms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaptDims {
    pub width_fp: u32,
    pub height_fp: u32,
}

impl TaptDims {
    pub fn width(&self) -> u32 {
        self.width_fp >> 16
    }
    pub fn height(&self) -> u32 {
        self.height_fp >> 16
    }
}

/// Parse a `tapt` sub-atom (clef/prof/enof) payload — `[ver+flags=4]
/// [width=4][height=4]`.
pub fn parse_tapt_dims(payload: &[u8]) -> Result<TaptDims> {
    if payload.len() < 12 {
        return Err(Error::invalid("MOV: tapt sub-atom payload < 12 bytes"));
    }
    Ok(TaptDims {
        width_fp: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
        height_fp: u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]),
    })
}

/// Audio Channel Layout (Apple `chan`). Round-2 surface: the leading
/// `[ver+flags=4][layout_tag=4][bitmap=4][n_descriptions=4]` plus the
/// raw description blob. A full layout-tag-to-mask mapping is round 3.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chan {
    pub layout_tag: u32,
    pub bitmap: u32,
    pub num_descriptions: u32,
    /// Raw bytes of the per-channel descriptions (each 20 bytes wide
    /// in CAF; we don't parse them in round 2).
    pub descriptions: Vec<u8>,
}

/// Parse a `chan` payload.
pub fn parse_chan(payload: &[u8]) -> Result<Chan> {
    if payload.len() < 16 {
        return Err(Error::invalid("MOV: chan payload < 16 bytes"));
    }
    let layout_tag = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let bitmap = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let num = u32::from_be_bytes([payload[12], payload[13], payload[14], payload[15]]);
    let descriptions = if payload.len() > 16 {
        payload[16..].to_vec()
    } else {
        Vec::new()
    };
    Ok(Chan {
        layout_tag,
        bitmap,
        num_descriptions: num,
        descriptions,
    })
}

/// One key-value pair from an Apple `meta` atom.
///
/// Keys come from the `keys` atom (a flat ordered list of
/// `[key_namespace:4][key_name: var]` records); values come from the
/// `ilst` atom — each list entry is itself an atom whose FourCC is the
/// 1-based key index, containing a `data` sub-atom with the typed
/// value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetaKeyValue {
    /// 4-byte key namespace (typically `mdta`).
    pub namespace: [u8; 4],
    /// UTF-8 key name (e.g. `"com.apple.quicktime.title"`).
    pub key: String,
    /// Apple ilst data type-code (1 = UTF-8, 21 = i8 BE int, etc.).
    pub type_code: u32,
    /// Raw value bytes (UTF-8 string when `type_code == 1`).
    pub value: Vec<u8>,
}

impl MetaKeyValue {
    /// Best-effort decode of the value as a UTF-8 string. Returns
    /// `None` for non-string type codes or invalid UTF-8.
    pub fn as_str(&self) -> Option<&str> {
        if self.type_code == 1 {
            std::str::from_utf8(&self.value).ok()
        } else {
            None
        }
    }
}

/// Parse the `keys` atom payload (Apple QuickTime `meta` shape).
///
/// Layout: `[ver+flags=4][entry_count=4]` followed by `entry_count`
/// records of `[size:4][namespace:4][key_value: size-8]`.
pub fn parse_keys(payload: &[u8]) -> Result<Vec<(String, [u8; 4])>> {
    if payload.len() < 8 {
        return Err(Error::invalid("MOV: keys payload < 8 bytes"));
    }
    let n = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let mut p = 8usize;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        if p + 8 > payload.len() {
            return Err(Error::invalid("MOV: keys entry truncated"));
        }
        let size = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]])
            as usize;
        if size < 8 || p + size > payload.len() {
            return Err(Error::invalid("MOV: keys entry size invalid"));
        }
        let mut ns = [0u8; 4];
        ns.copy_from_slice(&payload[p + 4..p + 8]);
        let key = std::str::from_utf8(&payload[p + 8..p + size])
            .map_err(|_| Error::invalid("MOV: keys entry not UTF-8"))?
            .to_string();
        out.push((key, ns));
        p += size;
    }
    Ok(out)
}

/// Parse the `ilst` atom payload, given a previously-parsed `keys`
/// table. Each ilst entry's atom type encodes a 1-based index into
/// the keys table; the entry's body contains a `data` sub-atom whose
/// payload is `[type_code:4][locale:4][value: rest]`.
///
/// Returns one `MetaKeyValue` per resolved key. Entries pointing to
/// out-of-range indices are silently dropped (lenient parse).
pub fn parse_ilst(payload: &[u8], keys: &[(String, [u8; 4])]) -> Result<Vec<MetaKeyValue>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < payload.len() {
        if p + 8 > payload.len() {
            return Err(Error::invalid("MOV: ilst entry truncated"));
        }
        let size = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]])
            as usize;
        if size < 8 || p + size > payload.len() {
            return Err(Error::invalid("MOV: ilst entry size invalid"));
        }
        let key_idx = u32::from_be_bytes([
            payload[p + 4],
            payload[p + 5],
            payload[p + 6],
            payload[p + 7],
        ]);
        // Walk inner atoms, looking for a `data` sub-atom.
        let entry_body = &payload[p + 8..p + size];
        let mut q = 0usize;
        let mut found: Option<(u32, Vec<u8>)> = None;
        while q + 8 <= entry_body.len() {
            let inner_size = u32::from_be_bytes([
                entry_body[q],
                entry_body[q + 1],
                entry_body[q + 2],
                entry_body[q + 3],
            ]) as usize;
            if inner_size < 8 || q + inner_size > entry_body.len() {
                break;
            }
            let inner_type = &entry_body[q + 4..q + 8];
            if inner_type == b"data" && inner_size >= 16 {
                let type_code = u32::from_be_bytes([
                    entry_body[q + 8],
                    entry_body[q + 9],
                    entry_body[q + 10],
                    entry_body[q + 11],
                ]);
                // 4-byte locale follows; the value starts at q+16.
                let value = entry_body[q + 16..q + inner_size].to_vec();
                found = Some((type_code, value));
                break;
            }
            q += inner_size;
        }
        if let (Some((type_code, value)), Some(idx)) = (
            found,
            (key_idx as usize)
                .checked_sub(1)
                .filter(|&i| i < keys.len()),
        ) {
            let (key, ns) = &keys[idx];
            out.push(MetaKeyValue {
                namespace: *ns,
                key: key.clone(),
                type_code,
                value,
            });
        }
        p += size;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasp_roundtrip() {
        let mut p = Vec::new();
        p.extend_from_slice(&16u32.to_be_bytes());
        p.extend_from_slice(&9u32.to_be_bytes());
        let v = parse_pasp(&p).unwrap();
        assert_eq!(v.h_spacing, 16);
        assert_eq!(v.v_spacing, 9);
    }

    #[test]
    fn clap_roundtrip() {
        let mut p = Vec::new();
        for n in [704u32, 1, 480, 1] {
            p.extend_from_slice(&n.to_be_bytes());
        }
        // negative horiz_off, positive vert_off
        p.extend_from_slice(&(-4i32).to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&8i32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        let c = parse_clap(&p).unwrap();
        assert_eq!(c.clean_aperture_width_n, 704);
        assert_eq!(c.horiz_off_n, -4);
        assert_eq!(c.vert_off_n, 8);
    }

    #[test]
    fn colr_nclc_apple_variant() {
        let mut p = Vec::new();
        p.extend_from_slice(b"nclc");
        p.extend_from_slice(&1u16.to_be_bytes()); // primaries (BT.709)
        p.extend_from_slice(&1u16.to_be_bytes()); // transfer
        p.extend_from_slice(&1u16.to_be_bytes()); // matrix
        let c = parse_colr(&p).unwrap();
        match c.kind {
            ColorParametersKind::Nclc {
                primaries,
                transfer,
                matrix,
            } => {
                assert_eq!((primaries, transfer, matrix), (1, 1, 1));
            }
            _ => panic!("expected nclc"),
        }
    }

    #[test]
    fn colr_nclx_iso_variant_full_range() {
        let mut p = Vec::new();
        p.extend_from_slice(b"nclx");
        p.extend_from_slice(&9u16.to_be_bytes()); // BT.2020
        p.extend_from_slice(&16u16.to_be_bytes()); // PQ
        p.extend_from_slice(&9u16.to_be_bytes()); // BT.2020 NC
        p.push(0x80); // full_range_flag = 1
        let c = parse_colr(&p).unwrap();
        match c.kind {
            ColorParametersKind::Nclx {
                primaries,
                transfer,
                matrix,
                full_range,
            } => {
                assert_eq!((primaries, transfer, matrix), (9, 16, 9));
                assert!(full_range);
            }
            _ => panic!("expected nclx"),
        }
    }

    #[test]
    fn tapt_dims_extract_int_pixels() {
        // ver+flags + (320 << 16) + (240 << 16)
        let mut p = vec![0u8; 12];
        p[4..8].copy_from_slice(&((320u32) << 16).to_be_bytes());
        p[8..12].copy_from_slice(&((240u32) << 16).to_be_bytes());
        let d = parse_tapt_dims(&p).unwrap();
        assert_eq!(d.width(), 320);
        assert_eq!(d.height(), 240);
    }

    #[test]
    fn chan_extracts_layout_tag() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&100u32.to_be_bytes()); // layout_tag = kAudioChannelLayoutTag_Stereo
        p.extend_from_slice(&0u32.to_be_bytes()); // bitmap
        p.extend_from_slice(&0u32.to_be_bytes()); // num_descriptions
        let c = parse_chan(&p).unwrap();
        assert_eq!(c.layout_tag, 100);
        assert_eq!(c.num_descriptions, 0);
    }

    #[test]
    fn keys_and_ilst_round_trip_simple() {
        // keys: 1 entry, namespace=mdta, key="com.test.title"
        let key = b"com.test.title";
        let mut keys = Vec::new();
        keys.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        keys.extend_from_slice(&1u32.to_be_bytes()); // count
        let entry_size: u32 = (8 + key.len()) as u32;
        keys.extend_from_slice(&entry_size.to_be_bytes());
        keys.extend_from_slice(b"mdta");
        keys.extend_from_slice(key);
        let parsed_keys = parse_keys(&keys).unwrap();
        assert_eq!(parsed_keys.len(), 1);
        assert_eq!(parsed_keys[0].0, "com.test.title");

        // ilst: 1 entry, type=index 1, body = data atom (utf-8 string "hi")
        let mut data_atom = Vec::new();
        let data_atom_size: u32 = 16 + 2;
        data_atom.extend_from_slice(&data_atom_size.to_be_bytes());
        data_atom.extend_from_slice(b"data");
        data_atom.extend_from_slice(&1u32.to_be_bytes()); // type_code = 1 (UTF-8)
        data_atom.extend_from_slice(&0u32.to_be_bytes()); // locale
        data_atom.extend_from_slice(b"hi");

        let mut ilst = Vec::new();
        let entry_size: u32 = (8 + data_atom.len()) as u32;
        ilst.extend_from_slice(&entry_size.to_be_bytes());
        ilst.extend_from_slice(&1u32.to_be_bytes()); // 1-based key index
        ilst.extend_from_slice(&data_atom);

        let kv = parse_ilst(&ilst, &parsed_keys).unwrap();
        assert_eq!(kv.len(), 1);
        assert_eq!(kv[0].key, "com.test.title");
        assert_eq!(kv[0].as_str(), Some("hi"));
    }
}
