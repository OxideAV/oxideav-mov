//! Apple-specific media metadata atoms.
//!
//! This module covers the Apple-extended boxes that decorate visual
//! and audio sample descriptions plus track-level metadata:
//!
//! * `gama` — gamma 16.16 fixed-point (QTFF p. 94, Table 3-2).
//! * `fiel` — Field Handling (QTFF p. 94, Table 3-2). Two 8-bit
//!   integers — field count + ordering — surfaced as the typed
//!   [`Fiel`] / [`FieldOrdering`] pair. QuickTime-only; ISO BMFF
//!   does not define this sample-description extension.
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

/// Field handling — `fiel` video sample-description extension
/// (QTFF p. 94, Table 3-2).
///
/// Two 8-bit integers that tell a downstream display pipeline how a
/// sample is broken into discrete fields. Used both at decode time
/// (when a decompressor component honours the declaration) and at
/// presentation time (to decide which field draws first when
/// rendering interlaced video).
///
/// * The first byte is the **field count**, legally `1` (progressive)
///   or `2` (interlaced). Other values are out-of-spec but Apple's
///   Toolbox historically forwarded them verbatim to the
///   decompressor, so the parser surfaces the raw byte alongside the
///   typed [`Fiel::is_interlaced`] accessor for callers that want to
///   distinguish "spec-conformant interlaced" from "writer noise".
/// * The second byte is the **field ordering**. The spec
///   enumerates exactly three legal values when the field count is
///   2 — `0` (unknown), `1` (T first), `6` (B first) — and is silent
///   on the meaning of any other value, including the byte's
///   contents when the field count is 1. The parser therefore
///   keeps the raw byte and maps it through [`FieldOrdering`] only
///   when the spec assigns the value a name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fiel {
    /// Number of fields per QuickTime sample. `1` is progressive
    /// (the sample is one whole frame); `2` is interlaced (the
    /// sample is two adjacent compressed fields). The spec
    /// enumerates only those two; any other byte is preserved
    /// verbatim and is flagged as not-spec by
    /// [`Fiel::is_spec_field_count`].
    pub field_count: u8,
    /// Raw second byte. When [`Fiel::field_count`] is `2` this picks
    /// between the three documented [`FieldOrdering`] variants;
    /// otherwise the byte is undefined by the spec but kept here
    /// for round-trip fidelity.
    pub field_ordering: u8,
}

impl Fiel {
    /// True when `field_count == 2` (interlaced).
    pub fn is_interlaced(&self) -> bool {
        self.field_count == 2
    }

    /// True when `field_count` is one of the two spec-enumerated
    /// values (`1` progressive or `2` interlaced). Anything else
    /// is a writer convention or a corrupted byte.
    pub fn is_spec_field_count(&self) -> bool {
        matches!(self.field_count, 1 | 2)
    }

    /// Typed view of [`Fiel::field_ordering`].
    ///
    /// Returns `None` when the byte is not one of the three values
    /// the spec enumerates (`0` / `1` / `6`). The unenumerated bytes
    /// (and the second byte of a progressive declaration) are
    /// undefined per QTFF p. 94 and surfaced as `None` rather than
    /// invented out of whole cloth.
    pub fn ordering(&self) -> Option<FieldOrdering> {
        match self.field_ordering {
            0 => Some(FieldOrdering::Unknown),
            1 => Some(FieldOrdering::TopFieldFirst),
            6 => Some(FieldOrdering::BottomFieldFirst),
            _ => None,
        }
    }
}

/// Spec-enumerated values of the `fiel` field-ordering byte
/// (QTFF p. 94, Table 3-2). The three named variants are the
/// **only** values the spec assigns meaning to; any other byte is
/// out-of-spec and surfaced as `None` by [`Fiel::ordering`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOrdering {
    /// Spec value `0`: "field ordering is unknown" — the writer
    /// declared the sample interlaced but did not commit to which
    /// field is topmost / earliest. Players typically fall back to a
    /// motion-adaptive heuristic.
    Unknown,
    /// Spec value `1`: "T is displayed earliest, T is stored first
    /// in the file" — top-field first.
    TopFieldFirst,
    /// Spec value `6`: "B is displayed earliest, B is stored first
    /// in the file" — bottom-field first.
    BottomFieldFirst,
}

/// On-disk byte length of a `fiel` body — exactly two 8-bit integers
/// per QTFF p. 94, Table 3-2. Used as both the minimum and the
/// maximum: `fiel` is fixed-width with no trailing data.
pub const FIEL_BODY_LEN: usize = 2;

/// Parse a `fiel` body (QTFF p. 94, Table 3-2).
///
/// `payload` is the bytes inside the atom — the caller has already
/// stripped the 8-byte `[size, type]` header. The body must be
/// exactly [`FIEL_BODY_LEN`] bytes; anything else is a writer error
/// (the spec is fixed-width with no list or version-flags prologue).
pub fn parse_fiel(payload: &[u8]) -> Result<Fiel> {
    if payload.len() != FIEL_BODY_LEN {
        return Err(Error::invalid(format!(
            "MOV: fiel payload length {} != 2 bytes (QTFF p. 94: field_count + field_ordering)",
            payload.len()
        )));
    }
    Ok(Fiel {
        field_count: payload[0],
        field_ordering: payload[1],
    })
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

/// Audio Channel Layout (Apple `chan`).
///
/// QTFF 2001-03 doesn't define `chan`; Apple's "Audio Channel Layouts"
/// chapter of the QuickTime/Core Audio reference does. The on-disk
/// layout matches `AudioChannelLayout` from `<CoreAudioTypes/CoreAudioTypes.h>`:
///
/// ```text
/// [ver+flags = 4]                        // FullBox header
/// [mChannelLayoutTag : u32]              // kAudioChannelLayoutTag_*
/// [mChannelBitmap    : u32]              // used when tag = 0x10000 (UseChannelBitmap)
/// [mNumberChannelDescriptions : u32]
/// repeat mNumberChannelDescriptions times: 20 bytes
///   [mChannelLabel   : u32]
///   [mChannelFlags   : u32]
///   [mCoordinates[3] : 3 × f32]
/// ```
///
/// Each per-description record is 20 bytes wide (4 + 4 + 12). The
/// `mChannelLayoutTag` either selects a pre-defined layout (e.g.
/// `Stereo = 100`, `_5_1 = 121`) — in which case `mNumberChannelDescriptions`
/// is required to be 0 — or carries the special sentinel
/// `kAudioChannelLayoutTag_UseChannelDescriptions = 0` (every channel
/// is fully described) or `kAudioChannelLayoutTag_UseChannelBitmap =
/// 0x10000` (the `mChannelBitmap` is authoritative, descriptions
/// absent).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chan {
    pub layout_tag: u32,
    pub bitmap: u32,
    pub num_descriptions: u32,
    /// Parsed per-channel descriptions (when present). Each entry is
    /// 20 bytes on disk (label + flags + 3 × f32 coordinates).
    pub channel_descriptions: Vec<ChanDescription>,
    /// Raw bytes of the per-channel descriptions, retained for
    /// downstream consumers that want the on-disk form.
    pub descriptions: Vec<u8>,
}

impl Chan {
    /// Resolve to a USB-style channel-mask bitmap.
    ///
    /// * For pre-defined layout tags we apply the canonical CoreAudio
    ///   tag → label set translation (see [`channel_mask_for_layout_tag`]).
    /// * For `UseChannelBitmap` we return `Some(self.bitmap)` directly.
    /// * For `UseChannelDescriptions` we OR each description's
    ///   `(1 << (label - 1))` bit (label 1 = Left, 2 = Right, …).
    /// * Unknown tags yield `None`.
    pub fn channel_mask(&self) -> Option<u32> {
        match self.layout_tag {
            TAG_USE_CHANNEL_BITMAP => Some(self.bitmap),
            TAG_USE_CHANNEL_DESCRIPTIONS => {
                let mut m = 0u32;
                for d in &self.channel_descriptions {
                    if d.label >= 1 && d.label <= 32 {
                        m |= 1u32 << (d.label - 1);
                    }
                }
                Some(m)
            }
            tag => channel_mask_for_layout_tag(tag),
        }
    }

    /// Channel count implied by the layout. The lower 16 bits of a
    /// pre-defined layout tag carry the channel count
    /// (`kAudioChannelLayoutTag_Stereo = (101 << 16) | 2`); when the
    /// tag is `UseChannelDescriptions` we return the description count.
    pub fn channel_count(&self) -> u32 {
        if self.layout_tag == TAG_USE_CHANNEL_DESCRIPTIONS {
            self.num_descriptions
        } else if self.layout_tag == TAG_USE_CHANNEL_BITMAP {
            self.bitmap.count_ones()
        } else {
            self.layout_tag & 0xFFFF
        }
    }
}

/// One per-channel description record (20 bytes on disk).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChanDescription {
    /// `mChannelLabel` — the [`AudioChannelLabel`] enum value
    /// identifying which channel this is (1 = Left, 2 = Right, …).
    pub label: u32,
    /// `mChannelFlags` — IEEE-754-style flag bits (bit 0 = "rectangular
    /// coordinates", bit 1 = "spherical coordinates", bit 2 = "meters").
    pub flags: u32,
    /// `mCoordinates[0..3]` — 3-D position when `flags` indicates
    /// rectangular or spherical coords. Stored as raw `f32` triple.
    pub coordinates: [f32; 3],
}

impl Eq for ChanDescription {}

/// Apple Core Audio sentinel: descriptions are authoritative.
pub const TAG_USE_CHANNEL_DESCRIPTIONS: u32 = 0;
/// Apple Core Audio sentinel: `bitmap` is authoritative.
pub const TAG_USE_CHANNEL_BITMAP: u32 = 0x0001_0000;

// Pre-defined `kAudioChannelLayoutTag_*` constants we surface (the
// commonly-encountered ones; the full CoreAudio list has ~150). Each
// tag's low 16 bits carry the channel count, the high 16 bits the
// layout id. See `<CoreAudioTypes/CoreAudioTypes.h>`.
pub const TAG_MONO: u32 = (100 << 16) | 1;
pub const TAG_STEREO: u32 = (101 << 16) | 2;
pub const TAG_STEREO_HEADPHONES: u32 = (102 << 16) | 2;
pub const TAG_MATRIX_STEREO: u32 = (103 << 16) | 2;
pub const TAG_MID_SIDE: u32 = (104 << 16) | 2;
pub const TAG_XY: u32 = (105 << 16) | 2;
pub const TAG_BINAURAL: u32 = (106 << 16) | 2;
pub const TAG_AMBISONIC_B_FORMAT: u32 = (107 << 16) | 4;
pub const TAG_QUADRAPHONIC: u32 = (108 << 16) | 4;
pub const TAG_PENTAGONAL: u32 = (109 << 16) | 5;
pub const TAG_HEXAGONAL: u32 = (110 << 16) | 6;
pub const TAG_OCTAGONAL: u32 = (111 << 16) | 8;
pub const TAG_CUBE: u32 = (112 << 16) | 8;
pub const TAG_MPEG_3_0_A: u32 = (113 << 16) | 3; // L R C
pub const TAG_MPEG_3_0_B: u32 = (114 << 16) | 3; // C L R
pub const TAG_MPEG_4_0_A: u32 = (115 << 16) | 4; // L R C Cs
pub const TAG_MPEG_4_0_B: u32 = (116 << 16) | 4; // C L R Cs
pub const TAG_MPEG_5_0_A: u32 = (117 << 16) | 5; // L R C Ls Rs
pub const TAG_MPEG_5_0_B: u32 = (118 << 16) | 5; // L R Ls Rs C
pub const TAG_MPEG_5_0_C: u32 = (119 << 16) | 5; // L C R Ls Rs
pub const TAG_MPEG_5_0_D: u32 = (120 << 16) | 5; // C L R Ls Rs
pub const TAG_MPEG_5_1_A: u32 = (121 << 16) | 6; // L R C LFE Ls Rs
pub const TAG_MPEG_5_1_B: u32 = (122 << 16) | 6; // L R Ls Rs C LFE
pub const TAG_MPEG_5_1_C: u32 = (123 << 16) | 6; // L C R Ls Rs LFE
pub const TAG_MPEG_5_1_D: u32 = (124 << 16) | 6; // C L R Ls Rs LFE
pub const TAG_MPEG_6_1_A: u32 = (125 << 16) | 7; // L R C LFE Ls Rs Cs
pub const TAG_MPEG_7_1_A: u32 = (126 << 16) | 8; // L R C LFE Ls Rs Lc Rc
pub const TAG_MPEG_7_1_B: u32 = (127 << 16) | 8; // C Lc Rc L R Ls Rs LFE
pub const TAG_MPEG_7_1_C: u32 = (128 << 16) | 8; // L R C LFE Ls Rs Rls Rrs

// Bit positions for the `channel_mask` USB-style bitmap. The
// CoreAudio `AudioChannelLabel` enum values 1..=18 line up with the
// well-known WAVEFORMATEXTENSIBLE channel-mask bits (1<<0 = FrontLeft,
// etc.) which is what `Chan::channel_mask` exports.
const FL: u32 = 1 << 0; // Front Left
const FR: u32 = 1 << 1; // Front Right
const FC: u32 = 1 << 2; // Front Center
const LFE: u32 = 1 << 3; // Low Frequency
const BL: u32 = 1 << 4; // Back Left
const BR: u32 = 1 << 5; // Back Right
const FLC: u32 = 1 << 6; // Front Left of Center
const FRC: u32 = 1 << 7; // Front Right of Center
const BC: u32 = 1 << 8; // Back Center
const SL: u32 = 1 << 9; // Side Left
const SR: u32 = 1 << 10; // Side Right

/// Map a pre-defined `kAudioChannelLayoutTag_*` to a USB-style
/// channel-mask bitmap (bit 0 = Front Left, bit 1 = Front Right, …).
/// Returns `None` for layouts that don't have a mask analogue (e.g.
/// `Cube`, `AmbisonicBFormat`, headphones / matrix stereo variants
/// that share the plain Stereo mask, …); the caller can fall back to
/// the raw `layout_tag`.
pub fn channel_mask_for_layout_tag(tag: u32) -> Option<u32> {
    match tag {
        TAG_MONO => Some(FC),
        TAG_STEREO | TAG_STEREO_HEADPHONES | TAG_MATRIX_STEREO => Some(FL | FR),
        TAG_MID_SIDE | TAG_XY | TAG_BINAURAL => Some(FL | FR),
        TAG_QUADRAPHONIC => Some(FL | FR | BL | BR),
        TAG_PENTAGONAL => Some(FL | FR | FC | BL | BR),
        TAG_HEXAGONAL => Some(FL | FR | FC | BL | BR | BC),
        TAG_OCTAGONAL => Some(FL | FR | FC | BL | BR | BC | SL | SR),
        TAG_MPEG_3_0_A | TAG_MPEG_3_0_B => Some(FL | FR | FC),
        TAG_MPEG_4_0_A | TAG_MPEG_4_0_B => Some(FL | FR | FC | BC),
        TAG_MPEG_5_0_A | TAG_MPEG_5_0_B | TAG_MPEG_5_0_C | TAG_MPEG_5_0_D => {
            Some(FL | FR | FC | SL | SR)
        }
        TAG_MPEG_5_1_A | TAG_MPEG_5_1_B | TAG_MPEG_5_1_C | TAG_MPEG_5_1_D => {
            Some(FL | FR | FC | LFE | SL | SR)
        }
        TAG_MPEG_6_1_A => Some(FL | FR | FC | LFE | SL | SR | BC),
        TAG_MPEG_7_1_A => Some(FL | FR | FC | LFE | SL | SR | FLC | FRC),
        TAG_MPEG_7_1_B => Some(FL | FR | FC | LFE | SL | SR | FLC | FRC),
        TAG_MPEG_7_1_C => Some(FL | FR | FC | LFE | SL | SR | BL | BR),
        _ => None,
    }
}

/// Parse a `chan` payload.
pub fn parse_chan(payload: &[u8]) -> Result<Chan> {
    if payload.len() < 16 {
        return Err(Error::invalid("MOV: chan payload < 16 bytes"));
    }
    let layout_tag = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let bitmap = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let num = u32::from_be_bytes([payload[12], payload[13], payload[14], payload[15]]);
    let descriptions_blob = if payload.len() > 16 {
        payload[16..].to_vec()
    } else {
        Vec::new()
    };
    // Parse the variable-length AudioChannelDescription list (20 B each).
    // We are lenient: if the declared count exceeds the available bytes
    // we cap it and stop parsing rather than fail the whole atom (the
    // raw bytes stay in `descriptions` for forensic recovery).
    const REC: usize = 20;
    let mut parsed = Vec::with_capacity(num as usize);
    let cap = (descriptions_blob.len() / REC).min(num as usize);
    for i in 0..cap {
        let off = i * REC;
        let label = u32::from_be_bytes([
            descriptions_blob[off],
            descriptions_blob[off + 1],
            descriptions_blob[off + 2],
            descriptions_blob[off + 3],
        ]);
        let flags = u32::from_be_bytes([
            descriptions_blob[off + 4],
            descriptions_blob[off + 5],
            descriptions_blob[off + 6],
            descriptions_blob[off + 7],
        ]);
        let c0 = f32::from_be_bytes([
            descriptions_blob[off + 8],
            descriptions_blob[off + 9],
            descriptions_blob[off + 10],
            descriptions_blob[off + 11],
        ]);
        let c1 = f32::from_be_bytes([
            descriptions_blob[off + 12],
            descriptions_blob[off + 13],
            descriptions_blob[off + 14],
            descriptions_blob[off + 15],
        ]);
        let c2 = f32::from_be_bytes([
            descriptions_blob[off + 16],
            descriptions_blob[off + 17],
            descriptions_blob[off + 18],
            descriptions_blob[off + 19],
        ]);
        parsed.push(ChanDescription {
            label,
            flags,
            coordinates: [c0, c1, c2],
        });
    }
    Ok(Chan {
        layout_tag,
        bitmap,
        num_descriptions: num,
        channel_descriptions: parsed,
        descriptions: descriptions_blob,
    })
}

/// Composition-shift least-greatest atom (`cslg`).
///
/// ISO BMFF §8.6.1.4 / QTFF Apple supplement. `cslg` lets a player
/// derive the presentation time-line bounds without scanning every
/// `ctts` entry, and it carries the offset between composition time
/// and decode time for the first sample. Two on-disk versions:
///
/// * Version 0 — five `i32` fields.
/// * Version 1 — five `i64` fields (used when any value exceeds 31 bits).
///
/// Fields, in spec order:
///
/// 1. `composition_to_dts_shift` — value to add to a CT to obtain the
///    DTS so that all DTS values are non-negative.
/// 2. `least_decode_to_display_delta` — minimum value of `composition_offset`.
/// 3. `greatest_decode_to_display_delta` — maximum value of `composition_offset`.
/// 4. `composition_start_time` — earliest CT in the track.
/// 5. `composition_end_time` — latest CT + sample-duration in the track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cslg {
    pub composition_to_dts_shift: i64,
    pub least_decode_to_display_delta: i64,
    pub greatest_decode_to_display_delta: i64,
    pub composition_start_time: i64,
    pub composition_end_time: i64,
}

/// Parse a `cslg` payload.
pub fn parse_cslg(payload: &[u8]) -> Result<Cslg> {
    if payload.len() < 4 {
        return Err(Error::invalid("MOV: cslg payload < 4 bytes"));
    }
    let version = payload[0];
    let body = &payload[4..];
    let (size, want) = match version {
        0 => (4usize, 4 * 5),
        1 => (8usize, 8 * 5),
        v => return Err(Error::invalid(format!("MOV: cslg unknown version {v}"))),
    };
    if body.len() < want {
        return Err(Error::invalid("MOV: cslg truncated table"));
    }
    let read = |o: usize| -> i64 {
        if size == 4 {
            i32::from_be_bytes([body[o], body[o + 1], body[o + 2], body[o + 3]]) as i64
        } else {
            i64::from_be_bytes([
                body[o],
                body[o + 1],
                body[o + 2],
                body[o + 3],
                body[o + 4],
                body[o + 5],
                body[o + 6],
                body[o + 7],
            ])
        }
    };
    Ok(Cslg {
        composition_to_dts_shift: read(0),
        least_decode_to_display_delta: read(size),
        greatest_decode_to_display_delta: read(size * 2),
        composition_start_time: read(size * 3),
        composition_end_time: read(size * 4),
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
        p.extend_from_slice(&100u32.to_be_bytes()); // layout_tag = legacy plain "100"
        p.extend_from_slice(&0u32.to_be_bytes()); // bitmap
        p.extend_from_slice(&0u32.to_be_bytes()); // num_descriptions
        let c = parse_chan(&p).unwrap();
        assert_eq!(c.layout_tag, 100);
        assert_eq!(c.num_descriptions, 0);
        assert!(c.channel_descriptions.is_empty());
    }

    #[test]
    fn chan_stereo_layout_tag_maps_to_fl_fr() {
        // kAudioChannelLayoutTag_Stereo = (101 << 16) | 2 → mask FL|FR.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&TAG_STEREO.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        let c = parse_chan(&p).unwrap();
        assert_eq!(c.channel_count(), 2);
        assert_eq!(c.channel_mask(), Some(FL | FR));
    }

    #[test]
    fn chan_5_1_layout_tag_maps_to_full_mask() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&TAG_MPEG_5_1_A.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        let c = parse_chan(&p).unwrap();
        assert_eq!(c.channel_count(), 6);
        assert_eq!(c.channel_mask(), Some(FL | FR | FC | LFE | SL | SR));
    }

    #[test]
    fn chan_use_channel_bitmap_returns_bitmap() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&TAG_USE_CHANNEL_BITMAP.to_be_bytes());
        let want_mask = FL | FR | FC | LFE;
        p.extend_from_slice(&want_mask.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        let c = parse_chan(&p).unwrap();
        assert_eq!(c.channel_count(), 4);
        assert_eq!(c.channel_mask(), Some(want_mask));
    }

    #[test]
    fn chan_use_channel_descriptions_or_labels() {
        // num=2: label 1 (Left), label 2 (Right) — full 20 B records.
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&TAG_USE_CHANNEL_DESCRIPTIONS.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // bitmap
        p.extend_from_slice(&2u32.to_be_bytes()); // num_descriptions
        for label in [1u32, 2u32] {
            p.extend_from_slice(&label.to_be_bytes());
            p.extend_from_slice(&0u32.to_be_bytes()); // flags
            p.extend_from_slice(&0f32.to_be_bytes());
            p.extend_from_slice(&0f32.to_be_bytes());
            p.extend_from_slice(&0f32.to_be_bytes());
        }
        let c = parse_chan(&p).unwrap();
        assert_eq!(c.channel_count(), 2);
        assert_eq!(c.channel_descriptions.len(), 2);
        assert_eq!(c.channel_descriptions[0].label, 1);
        assert_eq!(c.channel_descriptions[1].label, 2);
        assert_eq!(c.channel_mask(), Some(FL | FR));
    }

    #[test]
    fn cslg_v0_round_trip() {
        // Five i32 fields: shift=0, least=-3, greatest=10, start=0, end=300
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
        p.extend_from_slice(&0i32.to_be_bytes());
        p.extend_from_slice(&(-3i32).to_be_bytes());
        p.extend_from_slice(&10i32.to_be_bytes());
        p.extend_from_slice(&0i32.to_be_bytes());
        p.extend_from_slice(&300i32.to_be_bytes());
        let c = parse_cslg(&p).unwrap();
        assert_eq!(c.composition_to_dts_shift, 0);
        assert_eq!(c.least_decode_to_display_delta, -3);
        assert_eq!(c.greatest_decode_to_display_delta, 10);
        assert_eq!(c.composition_end_time, 300);
    }

    #[test]
    fn cslg_v1_round_trip_64bit() {
        let mut p = Vec::new();
        p.push(1); // version
        p.extend_from_slice(&[0, 0, 0]);
        for v in [0i64, -3, 10, 0, 300_000_000_000i64] {
            p.extend_from_slice(&v.to_be_bytes());
        }
        let c = parse_cslg(&p).unwrap();
        assert_eq!(c.composition_end_time, 300_000_000_000);
        assert_eq!(c.least_decode_to_display_delta, -3);
    }

    #[test]
    fn cslg_unknown_version_errors() {
        let mut p = Vec::new();
        p.push(2);
        p.extend_from_slice(&[0, 0, 0]);
        assert!(parse_cslg(&p).is_err());
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

    // ────────────────────────── `fiel` ────────────────────────────

    #[test]
    fn fiel_progressive_round_trip() {
        // QTFF p. 94: field_count=1 → progressive sample. The second
        // byte is undefined; we keep it raw and surface `None` from
        // the typed accessor.
        let f = parse_fiel(&[1, 0]).unwrap();
        assert_eq!(f.field_count, 1);
        assert_eq!(f.field_ordering, 0);
        assert!(!f.is_interlaced());
        assert!(f.is_spec_field_count());
        // 0 is a spec-named ordering value (Unknown), even though
        // the spec describes it as meaningful only when count=2 —
        // the typed accessor does not gate on field_count to keep
        // the mapping a pure function of the raw byte.
        assert_eq!(f.ordering(), Some(FieldOrdering::Unknown));
    }

    #[test]
    fn fiel_interlaced_top_field_first() {
        // QTFF p. 94: "1 – T is displayed earliest, T is stored first
        // in the file."
        let f = parse_fiel(&[2, 1]).unwrap();
        assert!(f.is_interlaced());
        assert!(f.is_spec_field_count());
        assert_eq!(f.ordering(), Some(FieldOrdering::TopFieldFirst));
    }

    #[test]
    fn fiel_interlaced_bottom_field_first() {
        // QTFF p. 94: "6 – B is displayed earliest, B is stored first
        // in the file."
        let f = parse_fiel(&[2, 6]).unwrap();
        assert!(f.is_interlaced());
        assert_eq!(f.ordering(), Some(FieldOrdering::BottomFieldFirst));
    }

    #[test]
    fn fiel_interlaced_unknown_ordering() {
        // QTFF p. 94: "0 – field ordering is unknown" — interlaced
        // sample without a documented field order.
        let f = parse_fiel(&[2, 0]).unwrap();
        assert!(f.is_interlaced());
        assert_eq!(f.ordering(), Some(FieldOrdering::Unknown));
    }

    #[test]
    fn fiel_unspec_ordering_byte_surfaces_none() {
        // Bytes 2/3/4/5/7..=255 are not enumerated by the spec;
        // the typed accessor returns None and the raw byte is
        // preserved on the struct for round-trip fidelity.
        for raw in [2u8, 3, 4, 5, 7, 9, 0x55, 0xFF] {
            let f = parse_fiel(&[2, raw]).unwrap();
            assert_eq!(f.field_ordering, raw);
            assert_eq!(
                f.ordering(),
                None,
                "byte 0x{raw:02X} should not be spec-named"
            );
        }
    }

    #[test]
    fn fiel_out_of_spec_field_count_preserved() {
        // QTFF p. 94 enumerates only field_count ∈ {1, 2}. A writer
        // that emits 0 (or 3, 17, etc.) is out-of-spec; the parser
        // preserves the byte for diagnostics and flags it through
        // `is_spec_field_count`.
        for raw in [0u8, 3, 17, 0x80, 0xFF] {
            let f = parse_fiel(&[raw, 0]).unwrap();
            assert_eq!(f.field_count, raw);
            assert!(!f.is_spec_field_count());
            // `is_interlaced` is a strict equality on 2, so any
            // non-2 byte — including the spec-progressive byte — is
            // not "interlaced" by the typed accessor's definition.
            assert!(!f.is_interlaced());
        }
    }

    #[test]
    fn fiel_rejects_wrong_payload_length() {
        // QTFF p. 94 fixes the body at exactly 2 bytes; the parser
        // rejects every other length (1, 3, 4, …) rather than
        // tolerating silent truncation / trailing data.
        for len in [0usize, 1, 3, 4, 8] {
            let body = vec![0u8; len];
            let err = parse_fiel(&body).unwrap_err();
            assert!(format!("{err}").contains("fiel payload length"));
        }
    }

    #[test]
    fn fiel_default_is_progressive_unknown() {
        // Default Fiel is the all-zero byte pair; the typed
        // accessor still returns the spec-named `Unknown` for the
        // ordering byte and `false` for `is_spec_field_count`
        // because 0 isn't in {1, 2}.
        let f = Fiel::default();
        assert_eq!(f.field_count, 0);
        assert_eq!(f.field_ordering, 0);
        assert!(!f.is_interlaced());
        assert!(!f.is_spec_field_count());
        assert_eq!(f.ordering(), Some(FieldOrdering::Unknown));
    }
}
