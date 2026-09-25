//! QuickTime Metadata atom (`meta`) — the full QTFF 2012 "Metadata"
//! chapter (pp. 129 – 143): handler (`hdlr`), header (`mhdr`), country
//! and language list sets (`ctry` / `lang`), item keys (`keys`), and
//! the item list (`ilst`) whose items carry an optional `itif` (item
//! id + flags), an optional `name`, and one or more `data` value atoms
//! each tagged with a type indicator and a locale indicator.
//!
//! [`crate::media_meta::MetaKeyValue`] is the flat "first value per
//! key" view that predates this module; [`QtMetadata::to_key_values`]
//! produces exactly that view, so the demuxer's `meta` field keeps
//! its historical shape while [`QtMetadata`] surfaces everything the
//! atom actually carries.
//!
//! Spec references (QTFF, 2012-08-14 edition):
//! * p. 129 Metadata Atom / p. 130 Figure 3-1 (layout);
//! * p. 130 Metadata Handler Atom — handler type should be `mdta`;
//! * p. 131 Metadata Header Atom (`mhdr`, `nextItemID`);
//! * p. 132 Extensibility — `free` / `uuid` may appear between the
//!   `meta` children, never between `ilst` items; unrecognised atoms
//!   inside the item list are ignored;
//! * p. 133 Country List Atom (`ctry`), p. 134 Language List Atom
//!   (`lang`) — 1-based indexes, at most 255 lists;
//! * p. 135 Metadata Item Keys Atom (`keys`);
//! * p. 137 Metadata Item List Atom (`ilst`) / p. 138 Metadata Item
//!   Atom (item type = 1-based key index; `itif`, `name`, `data[]`);
//! * p. 139 Value Atom / Type Indicator / Locale Indicator (Table 3-3);
//! * p. 141 Item Information Atom (`itif`) and Name atom (`name`);
//! * p. 142 Data Atom Structure and Data Ordering (most specific
//!   value first);
//! * p. 143 Table 3-5 Well-known data types.

use crate::media_meta::MetaKeyValue;
use crate::user_data::iso_language_tag;
#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Handler type the spec expects in a QuickTime metadata atom
/// (p. 130: "A reader parsing a metadata atom should confirm the
/// handler type … is `mdta` before interpreting any other structures").
pub const METADATA_HANDLER_MDTA: [u8; 4] = *b"mdta";

/// Well-known value types (p. 143, Table 3-5). The type indicator's
/// top byte selects the type set — only `0` (this table) is defined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WellKnownType {
    /// 0 — "Reserved for use where no type needs to be indicated".
    Reserved,
    /// 1 — UTF-8 "without any count or NULL terminator".
    Utf8,
    /// 2 — UTF-16 (UTF-16BE).
    Utf16,
    /// 3 — Shift-JIS ("deprecated unless it is needed for special
    /// Japanese characters").
    ShiftJis,
    /// 4 — UTF-8 sort variant ("for sorting only").
    Utf8Sort,
    /// 5 — UTF-16 sort variant.
    Utf16Sort,
    /// 13 — JPEG in a JFIF wrapper.
    Jpeg,
    /// 14 — PNG in a PNG wrapper.
    Png,
    /// 21 — big-endian signed integer in 1, 2, 3 or 4 bytes.
    BeSignedInt,
    /// 22 — big-endian unsigned integer in 1, 2, 3 or 4 bytes.
    BeUnsignedInt,
    /// 23 — big-endian IEEE 754 32-bit float.
    BeFloat32,
    /// 24 — big-endian IEEE 754 64-bit float.
    BeFloat64,
    /// 27 — Windows bitmap.
    Bmp,
    /// 28 — a block of data having the structure of the Metadata atom.
    QuickTimeMetadata,
}

impl WellKnownType {
    /// Map a well-known type code (the low 24 bits of a type
    /// indicator whose top byte is 0) to its variant.
    pub fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            0 => Self::Reserved,
            1 => Self::Utf8,
            2 => Self::Utf16,
            3 => Self::ShiftJis,
            4 => Self::Utf8Sort,
            5 => Self::Utf16Sort,
            13 => Self::Jpeg,
            14 => Self::Png,
            21 => Self::BeSignedInt,
            22 => Self::BeUnsignedInt,
            23 => Self::BeFloat32,
            24 => Self::BeFloat64,
            27 => Self::Bmp,
            28 => Self::QuickTimeMetadata,
            _ => return None,
        })
    }

    /// The on-disk type code.
    pub fn code(self) -> u32 {
        match self {
            Self::Reserved => 0,
            Self::Utf8 => 1,
            Self::Utf16 => 2,
            Self::ShiftJis => 3,
            Self::Utf8Sort => 4,
            Self::Utf16Sort => 5,
            Self::Jpeg => 13,
            Self::Png => 14,
            Self::BeSignedInt => 21,
            Self::BeUnsignedInt => 22,
            Self::BeFloat32 => 23,
            Self::BeFloat64 => 24,
            Self::Bmp => 27,
            Self::QuickTimeMetadata => 28,
        }
    }
}

/// One half (country or language) of a value's locale indicator
/// (p. 139, Table 3-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocaleIndicator {
    /// `0` — "the default value of this datum for any locale not
    /// explicitly listed".
    Default,
    /// `1 ..= 255` — 1-based index into the `ctry` / `lang` list atom.
    ListIndex(u8),
    /// Anything else — an immediate ISO 3166 country code (two ASCII
    /// letters, big-endian) or a packed ISO 639-2/T language code.
    Code(u16),
}

impl LocaleIndicator {
    /// Classify a raw 16-bit indicator.
    pub fn from_u16(v: u16) -> Self {
        match v {
            0 => Self::Default,
            1..=255 => Self::ListIndex(v as u8),
            other => Self::Code(other),
        }
    }

    /// The raw 16-bit indicator.
    pub fn to_u16(self) -> u16 {
        match self {
            Self::Default => 0,
            Self::ListIndex(i) => i as u16,
            Self::Code(c) => c,
        }
    }
}

/// A decoded value (see [`MetaValue::decode`]).
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedValue {
    /// UTF-8 / UTF-16 text (types 1, 2, 4, 5).
    Text(String),
    /// Big-endian signed integer (type 21; 1 – 4 bytes per the spec,
    /// 8 accepted too).
    SignedInt(i64),
    /// Big-endian unsigned integer (type 22).
    UnsignedInt(u64),
    /// IEEE 754 float32 (type 23).
    Float32(f32),
    /// IEEE 754 float64 (type 24).
    Float64(f64),
    /// A nested metadata atom (type 28).
    Metadata(Box<QtMetadata>),
}

/// One `data` value atom (p. 142): `[type:4][locale:4][value]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetaValue {
    /// Raw 32-bit type indicator: top byte = type set (must be 0), low
    /// 24 bits = well-known type code.
    pub type_indicator: u32,
    /// Country half of the locale indicator (upper 16 bits).
    pub country: u16,
    /// Language half of the locale indicator (lower 16 bits).
    pub language: u16,
    /// The value bytes, formatted as the type requires.
    pub value: Vec<u8>,
}

impl MetaValue {
    /// Type-set byte of the indicator (p. 139: must be 0).
    pub fn type_set(&self) -> u8 {
        (self.type_indicator >> 24) as u8
    }

    /// Well-known type code (the low 24 bits).
    pub fn type_code(&self) -> u32 {
        self.type_indicator & 0x00FF_FFFF
    }

    /// The well-known type, when the type set is 0 and the code is in
    /// Table 3-5.
    pub fn well_known_type(&self) -> Option<WellKnownType> {
        if self.type_set() != 0 {
            return None;
        }
        WellKnownType::from_code(self.type_code())
    }

    /// The country indicator, classified.
    pub fn country_indicator(&self) -> LocaleIndicator {
        LocaleIndicator::from_u16(self.country)
    }

    /// The language indicator, classified.
    pub fn language_indicator(&self) -> LocaleIndicator {
        LocaleIndicator::from_u16(self.language)
    }

    /// Whether this is the locale-neutral default value (both halves
    /// zero).
    pub fn is_default_locale(&self) -> bool {
        self.country == 0 && self.language == 0
    }

    /// Decode the value per its well-known type. `None` for the
    /// picture / opaque types (`Reserved`, `ShiftJis`, `Jpeg`, `Png`,
    /// `Bmp`), for unknown type codes, for a non-zero type set, and for
    /// a body that does not fit the type (invalid UTF-8, odd UTF-16
    /// length, an integer wider than 8 bytes, a float of the wrong
    /// width, a nested atom that fails to parse).
    pub fn decode(&self) -> Option<DecodedValue> {
        use WellKnownType as W;
        let v = &self.value;
        Some(match self.well_known_type()? {
            W::Utf8 | W::Utf8Sort => DecodedValue::Text(std::str::from_utf8(v).ok()?.to_owned()),
            W::Utf16 | W::Utf16Sort => {
                if v.len() % 2 != 0 {
                    return None;
                }
                let units: Vec<u16> = v
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                DecodedValue::Text(String::from_utf16(&units).ok()?)
            }
            W::BeSignedInt => {
                if v.is_empty() || v.len() > 8 {
                    return None;
                }
                // Sign-extend from the stored width.
                let mut acc: i64 = if v[0] & 0x80 != 0 { -1 } else { 0 };
                for &b in v {
                    acc = (acc << 8) | b as i64;
                }
                DecodedValue::SignedInt(acc)
            }
            W::BeUnsignedInt => {
                if v.is_empty() || v.len() > 8 {
                    return None;
                }
                let mut acc: u64 = 0;
                for &b in v {
                    acc = (acc << 8) | b as u64;
                }
                DecodedValue::UnsignedInt(acc)
            }
            W::BeFloat32 => {
                let a: [u8; 4] = v.as_slice().try_into().ok()?;
                DecodedValue::Float32(f32::from_be_bytes(a))
            }
            W::BeFloat64 => {
                let a: [u8; 8] = v.as_slice().try_into().ok()?;
                DecodedValue::Float64(f64::from_be_bytes(a))
            }
            // One level per call: parsing never decodes values, so
            // following a chain of nested atoms is the caller's loop,
            // never this function's recursion.
            W::QuickTimeMetadata => DecodedValue::Metadata(Box::new(parse_qt_metadata(v).ok()?)),
            W::Reserved | W::ShiftJis | W::Jpeg | W::Png | W::Bmp => return None,
        })
    }

    /// Text view: the decoded string for the UTF-8 / UTF-16 types.
    pub fn as_text(&self) -> Option<String> {
        match self.decode()? {
            DecodedValue::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// One `keys` entry (p. 135): a namespace and the key bytes whose
/// structure "depends upon the key namespace" — reverse-DNS UTF-8 for
/// `mdta`, a user-data FourCC such as `©cpy` for `udta` (Figure 3-3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetaKey {
    /// Key namespace (`mdta`, `udta`, …).
    pub namespace: [u8; 4],
    /// Raw key bytes (`key_size - 8` of them).
    pub value: Vec<u8>,
}

impl MetaKey {
    /// The key as UTF-8, when it is valid UTF-8 (always the case for
    /// `mdta` reverse-DNS keys; a `udta` FourCC with a Mac-Roman `©`
    /// byte is not).
    pub fn name(&self) -> Option<&str> {
        std::str::from_utf8(&self.value).ok()
    }

    /// The key rendered for display: UTF-8 when valid, otherwise each
    /// byte as a Latin-1 character (so `©cpy` reads as such).
    pub fn display_name(&self) -> String {
        match self.name() {
            Some(s) => s.to_owned(),
            None => self.value.iter().map(|&b| b as char).collect(),
        }
    }
}

/// One `ilst` item (p. 138).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetaItem {
    /// The item atom's type, which "should be set equal to the index of
    /// the key" (1-based) in the `keys` atom. Preserved verbatim even
    /// when out of range so callers can diagnose dangling items.
    pub key_index: u32,
    /// `itif` item id, when the item carries an item information atom.
    pub item_id: Option<u32>,
    /// `itif` flags (24 bits; "no flags are currently defined").
    pub item_flags: u32,
    /// `name` atom body, when present ("not user visible; a way to
    /// refer to metadata items").
    pub name: Option<String>,
    /// The value atoms in file order — per p. 142 "from the most
    /// specific data to the most general".
    pub values: Vec<MetaValue>,
}

impl MetaItem {
    /// The first value (the most specific one per Data Ordering), if
    /// any.
    pub fn first_value(&self) -> Option<&MetaValue> {
        self.values.first()
    }
}

/// A parsed QuickTime metadata atom.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QtMetadata {
    /// `hdlr` handler type (`mdta` per p. 130), when a handler atom is
    /// present.
    pub handler_type: Option<[u8; 4]>,
    /// `hdlr` human-readable name (may be empty).
    pub handler_name: String,
    /// `mhdr` `nextItemID`, when a metadata header atom is present.
    pub next_item_id: Option<u32>,
    /// `ctry` lists (1-based when indexed from a locale indicator);
    /// each country is its ISO 3166 two-letter code.
    pub country_lists: Vec<Vec<[u8; 2]>>,
    /// `lang` lists (1-based when indexed); each language is a packed
    /// ISO 639-2/T code (see [`iso_language_tag`]).
    pub language_lists: Vec<Vec<u16>>,
    /// `keys` entries in declaration order (index 0 here is key 1 on
    /// disk).
    pub keys: Vec<MetaKey>,
    /// `ilst` items in file order.
    pub items: Vec<MetaItem>,
    /// Whether a `keys` atom was present (an empty table is still a
    /// table).
    pub has_keys: bool,
    /// Whether an `ilst` atom was present.
    pub has_item_list: bool,
}

impl QtMetadata {
    /// Whether the atom is the QuickTime key-value shape at all: a
    /// `mdta` handler, or a `keys` / `ilst` child. An ISO BMFF §8.11
    /// item-based `meta` (`hdlr pict` + `pitm` / `iinf` / …) is not.
    pub fn is_quicktime_shape(&self) -> bool {
        self.handler_type == Some(METADATA_HANDLER_MDTA) || self.has_keys || self.has_item_list
    }

    /// Whether the handler type is the `mdta` the spec expects.
    pub fn has_mdta_handler(&self) -> bool {
        self.handler_type == Some(METADATA_HANDLER_MDTA)
    }

    /// The key an item refers to (1-based `key_index`).
    pub fn key_for(&self, item: &MetaItem) -> Option<&MetaKey> {
        (item.key_index as usize)
            .checked_sub(1)
            .and_then(|i| self.keys.get(i))
    }

    /// Items whose key has the given name (several items may share a
    /// key; each is a separate slot).
    pub fn items_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a MetaItem> + 'a {
        self.items
            .iter()
            .filter(move |it| self.key_for(it).and_then(MetaKey::name) == Some(name))
    }

    /// The first item whose key has the given name.
    pub fn item_named(&self, name: &str) -> Option<&MetaItem> {
        self.items
            .iter()
            .find(|it| self.key_for(it).and_then(MetaKey::name) == Some(name))
    }

    /// Resolve a value's country indicator to the set of ISO 3166
    /// codes it names: empty for the default, the referenced `ctry`
    /// list for an index (empty when the index dangles), or the single
    /// immediate code.
    pub fn countries_for(&self, value: &MetaValue) -> Vec<[u8; 2]> {
        match value.country_indicator() {
            LocaleIndicator::Default => Vec::new(),
            LocaleIndicator::ListIndex(i) => self
                .country_lists
                .get(i as usize - 1)
                .cloned()
                .unwrap_or_default(),
            LocaleIndicator::Code(c) => vec![c.to_be_bytes()],
        }
    }

    /// Resolve a value's language indicator to the packed ISO 639-2/T
    /// codes it names (same rules as [`Self::countries_for`]).
    pub fn languages_for(&self, value: &MetaValue) -> Vec<u16> {
        match value.language_indicator() {
            LocaleIndicator::Default => Vec::new(),
            LocaleIndicator::ListIndex(i) => self
                .language_lists
                .get(i as usize - 1)
                .cloned()
                .unwrap_or_default(),
            LocaleIndicator::Code(c) => vec![c],
        }
    }

    /// [`Self::languages_for`] unpacked to three-letter tags (codes
    /// that do not unpack to lowercase ASCII are dropped).
    pub fn language_tags_for(&self, value: &MetaValue) -> Vec<[u8; 3]> {
        self.languages_for(value)
            .into_iter()
            .filter_map(iso_language_tag)
            .collect()
    }

    /// Whether a value matches a requested locale per p. 140: a
    /// country matches "if either (a) the value to be matched to is 0
    /// (default) or (b) the codes are equal", a list matches when the
    /// requested code "is a member of that list", and a locale matches
    /// "if both country and language match". Pass `None` for a half
    /// the caller does not care about (it then matches everything).
    pub fn value_matches_locale(
        &self,
        value: &MetaValue,
        country: Option<[u8; 2]>,
        language: Option<u16>,
    ) -> bool {
        let country_ok = match (value.country_indicator(), country) {
            (LocaleIndicator::Default, _) | (_, None) => true,
            (_, Some(want)) => self.countries_for(value).contains(&want),
        };
        let language_ok = match (value.language_indicator(), language) {
            (LocaleIndicator::Default, _) | (_, None) => true,
            (_, Some(want)) => self.languages_for(value).contains(&want),
        };
        country_ok && language_ok
    }

    /// The first value of `item` that matches the requested locale
    /// (values are ordered most-specific-first, so this is the p. 142
    /// "stop searching once it finds a value it can display" rule
    /// without the type filter).
    pub fn value_for_locale<'a>(
        &self,
        item: &'a MetaItem,
        country: Option<[u8; 2]>,
        language: Option<u16>,
    ) -> Option<&'a MetaValue> {
        item.values
            .iter()
            .find(|v| self.value_matches_locale(v, country, language))
    }

    /// The flat historical view: one [`MetaKeyValue`] per item that
    /// resolves to a key and carries at least one value, taking the
    /// first (most specific) value. Items with a dangling key index or
    /// no `data` atom are dropped, exactly as
    /// [`crate::media_meta::parse_ilst`] does.
    pub fn to_key_values(&self) -> Vec<MetaKeyValue> {
        self.items
            .iter()
            .filter_map(|it| {
                let key = self.key_for(it)?;
                let v = it.first_value()?;
                Some(MetaKeyValue {
                    namespace: key.namespace,
                    key: key.display_name(),
                    type_code: v.type_indicator,
                    value: v.value.clone(),
                })
            })
            .collect()
    }
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

/// Iterate `[size:4][type:4][body]` children of a flat byte body.
/// `size == 0` (to end) is accepted for the last child; `size == 1`
/// (64-bit) is accepted when the extended size fits. Truncated or
/// impossible sizes end the walk with an error.
fn walk_flat(
    body: &[u8],
    what: &str,
    mut f: impl FnMut(&[u8; 4], &[u8]) -> Result<()>,
) -> Result<()> {
    let mut p = 0usize;
    while p < body.len() {
        if p + 8 > body.len() {
            return Err(Error::invalid(format!(
                "MOV: {what} child header truncated"
            )));
        }
        let mut size = be32(body, p).unwrap_or(0) as usize;
        let mut hdr = 8usize;
        let mut fourcc = [0u8; 4];
        fourcc.copy_from_slice(&body[p + 4..p + 8]);
        if size == 0 {
            size = body.len() - p;
        } else if size == 1 {
            let hi = be32(body, p + 8).ok_or_else(|| {
                Error::invalid(format!("MOV: {what} child extended size truncated"))
            })? as u64;
            let lo = be32(body, p + 12).unwrap_or(0) as u64;
            let ext = (hi << 32) | lo;
            if ext < 16 || ext > (body.len() - p) as u64 {
                return Err(Error::invalid(format!(
                    "MOV: {what} child extended size {ext} invalid"
                )));
            }
            size = ext as usize;
            hdr = 16;
        }
        if size < hdr || p + size > body.len() {
            return Err(Error::invalid(format!(
                "MOV: {what} child '{}' size {size} invalid",
                fourcc.iter().map(|&b| b as char).collect::<String>()
            )));
        }
        f(&fourcc, &body[p + hdr..p + size])?;
        p += size;
    }
    Ok(())
}

/// Parse a `ctry` or `lang` list atom body (`[ver+flags:4]
/// [entry_count:4]` then `entry_count` × `[count:2][code:2 × count]`).
fn parse_locale_lists(body: &[u8], what: &str) -> Result<Vec<Vec<u16>>> {
    // Normative layout (pp. 133 – 134 field lists): a full atom, so
    // `[ver+flags:4]` precedes `entry_count`.
    match parse_locale_lists_from(body, 4, what) {
        Ok((lists, consumed)) if consumed == body.len() => return Ok(lists),
        Ok(_) | Err(_) => {}
    }
    // The worked examples (Table 3-1 / Table 3-2, `atom_size` 26)
    // omit the version/flags word the field lists mandate. Accept
    // that spelling only when it accounts for every byte of the body.
    if let Ok((lists, consumed)) = parse_locale_lists_from(body, 0, what) {
        if consumed == body.len() {
            return Ok(lists);
        }
    }
    // Neither spelling fits: report against the normative one.
    parse_locale_lists_from(body, 4, what).map(|(l, _)| l)
}

/// Parse `[entry_count:4]` at `start` followed by `entry_count` lists;
/// returns the lists and the number of body bytes consumed.
fn parse_locale_lists_from(
    body: &[u8],
    start: usize,
    what: &str,
) -> Result<(Vec<Vec<u16>>, usize)> {
    if body.len() < start + 4 {
        return Err(Error::invalid(format!(
            "MOV: {what} payload < {} bytes",
            start + 4
        )));
    }
    let n = be32(body, start).unwrap_or(0);
    let remaining = body.len() - (start + 4);
    // Every list is at least its 2-byte count.
    if (n as u64).saturating_mul(2) > remaining as u64 {
        return Err(Error::invalid(format!(
            "MOV: {what} entry_count {n} cannot fit in {remaining} body bytes"
        )));
    }
    let mut out = Vec::with_capacity(n as usize);
    let mut p = start + 4;
    for _ in 0..n {
        let count = be16(body, p)
            .ok_or_else(|| Error::invalid(format!("MOV: {what} list count truncated")))?
            as usize;
        p += 2;
        if p + count * 2 > body.len() {
            return Err(Error::invalid(format!(
                "MOV: {what} list of {count} codes truncated"
            )));
        }
        let mut list = Vec::with_capacity(count);
        for i in 0..count {
            list.push(be16(body, p + i * 2).unwrap_or(0));
        }
        p += count * 2;
        out.push(list);
    }
    Ok((out, p))
}

fn parse_keys_atom(body: &[u8]) -> Result<Vec<MetaKey>> {
    if body.len() < 8 {
        return Err(Error::invalid("MOV: keys payload < 8 bytes"));
    }
    let n = be32(body, 4).unwrap_or(0);
    let remaining = body.len() - 8;
    if (n as u64).saturating_mul(8) > remaining as u64 {
        return Err(Error::invalid(format!(
            "MOV: keys entry_count {n} cannot fit in {remaining} body bytes"
        )));
    }
    let mut out = Vec::with_capacity(n as usize);
    let mut p = 8usize;
    for _ in 0..n {
        let size =
            be32(body, p).ok_or_else(|| Error::invalid("MOV: keys entry truncated"))? as usize;
        if size < 8 || p + size > body.len() {
            return Err(Error::invalid("MOV: keys entry size invalid"));
        }
        let mut namespace = [0u8; 4];
        namespace.copy_from_slice(&body[p + 4..p + 8]);
        out.push(MetaKey {
            namespace,
            value: body[p + 8..p + size].to_vec(),
        });
        p += size;
    }
    Ok(out)
}

fn parse_item(key_index: u32, body: &[u8]) -> Result<MetaItem> {
    let mut item = MetaItem {
        key_index,
        ..Default::default()
    };
    walk_flat(body, "ilst item", |fourcc, child| {
        match fourcc {
            b"data" => {
                if child.len() < 8 {
                    return Err(Error::invalid("MOV: ilst data atom < 8 bytes"));
                }
                let locale = be32(child, 4).unwrap_or(0);
                item.values.push(MetaValue {
                    type_indicator: be32(child, 0).unwrap_or(0),
                    country: (locale >> 16) as u16,
                    language: (locale & 0xFFFF) as u16,
                    value: child[8..].to_vec(),
                });
            }
            b"itif" => {
                if child.len() < 8 {
                    return Err(Error::invalid("MOV: ilst itif atom < 8 bytes"));
                }
                item.item_flags = be32(child, 0).unwrap_or(0) & 0x00FF_FFFF;
                item.item_id = be32(child, 4);
            }
            b"name" => {
                if child.len() < 4 {
                    return Err(Error::invalid("MOV: ilst name atom < 4 bytes"));
                }
                item.name = Some(String::from_utf8_lossy(&child[4..]).into_owned());
            }
            // p. 132: unrecognised atoms inside the item list are
            // ignored.
            _ => {}
        }
        Ok(())
    })?;
    Ok(item)
}

fn parse_ilst_atom(body: &[u8]) -> Result<Vec<MetaItem>> {
    let mut items = Vec::new();
    walk_flat(body, "ilst", |fourcc, child| {
        let key_index = u32::from_be_bytes(*fourcc);
        items.push(parse_item(key_index, child)?);
        Ok(())
    })?;
    Ok(items)
}

/// Whether a `meta` body starts with a FullBox `[version:1][flags:3]`
/// word (ISO BMFF §8.11.1 shape) rather than directly with a child
/// atom (the QTFF shape, Figure 3-1). Decided by peeking: when the
/// first four bytes read as a child size that fits the body, the body
/// starts with a child.
fn fullbox_prefix_len(body: &[u8]) -> usize {
    if body.len() < 8 {
        return 0;
    }
    let size = be32(body, 0).unwrap_or(0) as usize;
    if size >= 8 && size <= body.len() {
        0
    } else {
        4
    }
}

/// Parse a `meta` atom body (everything after the `[size][type]`
/// header; a leading FullBox word is detected and skipped). Every
/// child the spec defines is decoded; `free` / `skip` / `uuid` and
/// unknown children are ignored (p. 132 Extensibility). Children that
/// belong to the ISO BMFF item model (`pitm`, `iinf`, …) are likewise
/// ignored — callers check [`QtMetadata::is_quicktime_shape`].
pub fn parse_qt_metadata(body: &[u8]) -> Result<QtMetadata> {
    let mut m = QtMetadata::default();
    let start = fullbox_prefix_len(body);
    let body = &body[start.min(body.len())..];
    walk_flat(body, "meta", |fourcc, child| {
        match fourcc {
            b"hdlr" => {
                // [ver+flags:4][pre_defined:4][handler_type:4]
                // [reserved:12][name: NUL-terminated UTF-8]
                if child.len() >= 12 {
                    let mut t = [0u8; 4];
                    t.copy_from_slice(&child[8..12]);
                    m.handler_type = Some(t);
                }
                if child.len() > 24 {
                    let name = &child[24..];
                    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                    m.handler_name = String::from_utf8_lossy(&name[..end]).into_owned();
                }
            }
            b"mhdr" => {
                m.next_item_id = be32(child, 4);
            }
            b"ctry" => {
                m.country_lists = parse_locale_lists(child, "ctry")?
                    .into_iter()
                    .map(|l| l.into_iter().map(u16::to_be_bytes).collect())
                    .collect();
            }
            b"lang" => {
                m.language_lists = parse_locale_lists(child, "lang")?;
            }
            b"keys" => {
                m.keys = parse_keys_atom(child)?;
                m.has_keys = true;
            }
            b"ilst" => {
                m.items = parse_ilst_atom(child)?;
                m.has_item_list = true;
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + body.len());
        out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(fourcc);
        out.extend_from_slice(body);
        out
    }

    fn hdlr(handler: &[u8; 4], name: &str) -> Vec<u8> {
        let mut b = vec![0u8; 8];
        b.extend_from_slice(handler);
        b.extend_from_slice(&[0u8; 12]);
        b.extend_from_slice(name.as_bytes());
        b.push(0);
        atom(b"hdlr", &b)
    }

    fn keys(entries: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut b = vec![0u8; 4];
        b.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (ns, key) in entries {
            b.extend_from_slice(&((8 + key.len()) as u32).to_be_bytes());
            b.extend_from_slice(*ns);
            b.extend_from_slice(key);
        }
        atom(b"keys", &b)
    }

    fn data(type_code: u32, country: u16, language: u16, value: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&type_code.to_be_bytes());
        b.extend_from_slice(&(((country as u32) << 16) | language as u32).to_be_bytes());
        b.extend_from_slice(value);
        atom(b"data", &b)
    }

    fn item(key_index: u32, children: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = children.concat();
        atom(&key_index.to_be_bytes(), &body)
    }

    fn lists(fourcc: &[u8; 4], lists: &[&[u16]]) -> Vec<u8> {
        let mut b = vec![0u8; 4];
        b.extend_from_slice(&(lists.len() as u32).to_be_bytes());
        for l in lists {
            b.extend_from_slice(&(l.len() as u16).to_be_bytes());
            for c in *l {
                b.extend_from_slice(&c.to_be_bytes());
            }
        }
        atom(fourcc, &b)
    }

    fn code(c: &[u8; 2]) -> u16 {
        u16::from_be_bytes(*c)
    }

    fn pack(tag: &[u8; 3]) -> u16 {
        let c = |b: u8| (b - 0x60) as u16;
        (c(tag[0]) << 10) | (c(tag[1]) << 5) | c(tag[2])
    }

    #[test]
    fn table_3_1_and_3_2_examples_decode() {
        // Table 3-1: two country lists (US, UK) / (JP, US, FR) — 26
        // bytes including the atom header.
        let ctry = lists(
            b"ctry",
            &[
                &[code(b"US"), code(b"UK")],
                &[code(b"JP"), code(b"US"), code(b"FR")],
            ],
        );
        assert_eq!(
            ctry.len(),
            30,
            "full atom: 8 header + 4 ver/flags + 4 count + 14"
        );
        // Table 3-2: (eng 5575, fra 6721, deu 4277) / (spa 19969, por 16882).
        assert_eq!(pack(b"eng"), 5575);
        assert_eq!(pack(b"fra"), 6721);
        assert_eq!(pack(b"deu"), 4277);
        assert_eq!(pack(b"spa"), 19969);
        assert_eq!(pack(b"por"), 16882);
        let lang = lists(b"lang", &[&[5575, 6721, 4277], &[19969, 16882]]);
        assert_eq!(lang.len(), 30);

        let body = [hdlr(b"mdta", ""), ctry.clone(), lang.clone()].concat();
        let m = parse_qt_metadata(&body).unwrap();
        assert_eq!(
            m.country_lists,
            vec![vec![*b"US", *b"UK"], vec![*b"JP", *b"US", *b"FR"]]
        );
        assert_eq!(
            m.language_lists,
            vec![vec![5575, 6721, 4277], vec![19969, 16882]]
        );
        assert!(m.has_mdta_handler());
        assert!(m.is_quicktime_shape());

        // The printed examples spell the atoms WITHOUT the version /
        // flags word (atom_size 26); that layout decodes identically
        // when it accounts for the whole body.
        let bare = |fourcc: &[u8; 4], full: &[u8]| {
            let mut b = full[..8].to_vec();
            b[3] -= 4;
            b.extend_from_slice(&full[12..]);
            assert_eq!(b.len(), 26);
            assert_eq!(&b[4..8], fourcc);
            b
        };
        let body = [
            hdlr(b"mdta", ""),
            bare(b"ctry", &ctry),
            bare(b"lang", &lang),
        ]
        .concat();
        let m2 = parse_qt_metadata(&body).unwrap();
        assert_eq!(m2.country_lists, m.country_lists);
        assert_eq!(m2.language_lists, m.language_lists);
    }

    #[test]
    fn figure_3_3_keys_with_two_namespaces() {
        let body = [
            hdlr(b"mdta", "metadata"),
            keys(&[
                (b"mdta", b"com.apple.quicktime.copyright"),
                (b"mdta", b"com.apple.quicktime.author"),
                (b"udta", &[0xA9, b'c', b'p', b'y']),
            ]),
        ]
        .concat();
        let m = parse_qt_metadata(&body).unwrap();
        assert_eq!(m.handler_name, "metadata");
        assert_eq!(m.keys.len(), 3);
        assert_eq!(m.keys[0].name(), Some("com.apple.quicktime.copyright"));
        assert_eq!(m.keys[2].namespace, *b"udta");
        assert_eq!(m.keys[2].name(), None);
        assert_eq!(m.keys[2].display_name(), "©cpy");
    }

    #[test]
    fn item_with_itif_name_and_ordered_multi_locale_values() {
        let body = [
            hdlr(b"mdta", ""),
            atom(b"mhdr", &[0, 0, 0, 0, 0, 0, 0, 7]),
            lists(
                b"ctry",
                &[&[code(b"DE"), code(b"GB"), code(b"FR"), code(b"IT")]],
            ),
            lists(b"lang", &[&[pack(b"deu"), pack(b"fra")]]),
            keys(&[(b"mdta", b"com.apple.quicktime.title")]),
            atom(
                b"ilst",
                &item(
                    1,
                    &[
                        atom(b"itif", &[0, 0, 0, 0, 0, 0, 0, 6]),
                        atom(b"name", &[0, 0, 0, 0, b't', b'i', b't']),
                        // Most specific first: (list 1, list 1), then
                        // (CA, fra immediate), then (0, eng), then default.
                        data(1, 1, 1, b"Titel"),
                        data(1, code(b"CA"), pack(b"fra"), b"Titre"),
                        data(1, 0, pack(b"eng"), b"Title"),
                        data(1, 0, 0, b"Default"),
                    ],
                ),
            ),
        ]
        .concat();
        let m = parse_qt_metadata(&body).unwrap();
        assert_eq!(m.next_item_id, Some(7));
        let it = m.item_named("com.apple.quicktime.title").unwrap();
        assert_eq!(it.item_id, Some(6));
        assert_eq!(it.item_flags, 0);
        assert_eq!(it.name.as_deref(), Some("tit"));
        assert_eq!(it.values.len(), 4);

        let v0 = &it.values[0];
        assert_eq!(v0.country_indicator(), LocaleIndicator::ListIndex(1));
        assert_eq!(m.countries_for(v0), vec![*b"DE", *b"GB", *b"FR", *b"IT"]);
        assert_eq!(m.language_tags_for(v0), vec![*b"deu", *b"fra"]);
        let v1 = &it.values[1];
        assert_eq!(v1.country_indicator(), LocaleIndicator::Code(code(b"CA")));
        assert_eq!(m.countries_for(v1), vec![*b"CA"]);
        assert_eq!(m.language_tags_for(v1), vec![*b"fra"]);
        assert!(it.values[3].is_default_locale());

        // Table 3-4 matching: a German speaker in Italy gets the list
        // value; a French speaker in Canada gets the CA/fra value; an
        // English speaker anywhere gets the eng value; a Spanish
        // speaker in Spain falls through to the default.
        let pick = |c: &[u8; 2], l: &[u8; 3]| {
            m.value_for_locale(it, Some(*c), Some(pack(l)))
                .unwrap()
                .as_text()
                .unwrap()
        };
        assert_eq!(pick(b"IT", b"deu"), "Titel");
        assert_eq!(pick(b"CA", b"fra"), "Titre");
        assert_eq!(pick(b"US", b"eng"), "Title");
        assert_eq!(pick(b"ES", b"spa"), "Default");
        // Country-only request: any language.
        assert_eq!(
            m.value_for_locale(it, Some(*b"GB"), None)
                .unwrap()
                .as_text()
                .unwrap(),
            "Titel"
        );

        // Flat view takes the first (most specific) value.
        let kv = m.to_key_values();
        assert_eq!(kv.len(), 1);
        assert_eq!(kv[0].key, "com.apple.quicktime.title");
        assert_eq!(kv[0].as_str(), Some("Titel"));
    }

    #[test]
    fn well_known_types_decode() {
        let mv = |t: u32, v: &[u8]| MetaValue {
            type_indicator: t,
            country: 0,
            language: 0,
            value: v.to_vec(),
        };
        assert_eq!(
            mv(1, b"abc").decode(),
            Some(DecodedValue::Text("abc".into()))
        );
        assert_eq!(
            mv(4, b"abc").decode(),
            Some(DecodedValue::Text("abc".into()))
        );
        assert_eq!(
            mv(2, &[0, b'h', 0, b'i']).decode(),
            Some(DecodedValue::Text("hi".into()))
        );
        assert_eq!(mv(2, &[0, b'h', 0]).decode(), None, "odd UTF-16 length");
        assert_eq!(mv(21, &[0xFF]).decode(), Some(DecodedValue::SignedInt(-1)));
        assert_eq!(
            mv(21, &[0x00, 0x80]).decode(),
            Some(DecodedValue::SignedInt(128))
        );
        assert_eq!(
            mv(21, &[0xFF, 0xFF, 0xFE]).decode(),
            Some(DecodedValue::SignedInt(-2)),
            "3-byte width per Table 3-5"
        );
        assert_eq!(
            mv(22, &[0x80]).decode(),
            Some(DecodedValue::UnsignedInt(128))
        );
        assert_eq!(
            mv(22, &[1, 0, 0, 0]).decode(),
            Some(DecodedValue::UnsignedInt(1 << 24))
        );
        assert_eq!(mv(22, &[0; 9]).decode(), None, "wider than 8 bytes");
        assert_eq!(
            mv(23, &1.5f32.to_be_bytes()).decode(),
            Some(DecodedValue::Float32(1.5))
        );
        assert_eq!(
            mv(24, &(-2.25f64).to_be_bytes()).decode(),
            Some(DecodedValue::Float64(-2.25))
        );
        assert_eq!(mv(24, &[0; 4]).decode(), None, "float64 needs 8 bytes");
        assert_eq!(mv(13, b"\xFF\xD8").decode(), None, "JPEG stays opaque");
        assert_eq!(mv(13, b"").well_known_type(), Some(WellKnownType::Jpeg));
        assert_eq!(mv(99, b"").well_known_type(), None);
        assert_eq!(
            mv(0x0100_0001, b"x").well_known_type(),
            None,
            "type set 1 is reserved"
        );
        assert_eq!(mv(0x0100_0001, b"x").type_set(), 1);
        for code in [0, 1, 2, 3, 4, 5, 13, 14, 21, 22, 23, 24, 27, 28] {
            assert_eq!(WellKnownType::from_code(code).unwrap().code(), code);
        }
    }

    #[test]
    fn nested_metadata_atom_value_decodes_one_level_per_call() {
        let inner = [
            hdlr(b"mdta", ""),
            keys(&[(b"mdta", b"inner.key")]),
            atom(b"ilst", &item(1, &[data(1, 0, 0, b"inner")])),
        ]
        .concat();
        let outer = [
            hdlr(b"mdta", ""),
            keys(&[(b"mdta", b"outer.key")]),
            atom(b"ilst", &item(1, &[data(28, 0, 0, &inner)])),
        ]
        .concat();
        let m = parse_qt_metadata(&outer).unwrap();
        match m.items[0].values[0].decode() {
            Some(DecodedValue::Metadata(n)) => {
                assert_eq!(
                    n.item_named("inner.key").unwrap().values[0]
                        .as_text()
                        .unwrap(),
                    "inner"
                );
            }
            other => panic!("expected nested metadata, got {other:?}"),
        }
        // A chain of nested atoms is walked one level per decode call.
        let mut blob = inner;
        for _ in 0..6 {
            blob = [
                hdlr(b"mdta", ""),
                keys(&[(b"mdta", b"k")]),
                atom(b"ilst", &item(1, &[data(28, 0, 0, &blob)])),
            ]
            .concat();
        }
        let mut cur = parse_qt_metadata(&blob).unwrap();
        let mut hops = 0;
        while let Some(DecodedValue::Metadata(n)) = cur.items[0].values[0].decode() {
            cur = *n;
            hops += 1;
        }
        assert_eq!(hops, 6);
        assert_eq!(
            cur.item_named("inner.key").unwrap().values[0]
                .as_text()
                .unwrap(),
            "inner"
        );
    }

    #[test]
    fn iso_item_based_meta_is_not_quicktime_shape() {
        let body = [
            [0u8, 0, 0, 0].to_vec(), // FullBox word
            hdlr(b"pict", ""),
            atom(b"pitm", &[0, 0, 0, 0, 0, 1]),
        ]
        .concat();
        let m = parse_qt_metadata(&body).unwrap();
        assert_eq!(m.handler_type, Some(*b"pict"));
        assert!(!m.is_quicktime_shape());
        assert!(m.to_key_values().is_empty());
    }

    #[test]
    fn free_and_uuid_between_children_are_skipped() {
        let body = [
            hdlr(b"mdta", ""),
            atom(b"free", &[0; 12]),
            keys(&[(b"mdta", b"k")]),
            atom(b"uuid", &[7; 20]),
            atom(b"ilst", &item(1, &[data(1, 0, 0, b"v")])),
        ]
        .concat();
        let m = parse_qt_metadata(&body).unwrap();
        assert_eq!(m.to_key_values()[0].as_str(), Some("v"));
    }

    #[test]
    fn dangling_key_index_and_unknown_item_children_survive() {
        let body = [
            hdlr(b"mdta", ""),
            keys(&[(b"mdta", b"k")]),
            atom(
                b"ilst",
                &[
                    item(9, &[data(1, 0, 0, b"orphan")]),
                    item(1, &[atom(b"zzzz", &[1, 2, 3]), data(1, 0, 0, b"ok")]),
                ]
                .concat(),
            ),
        ]
        .concat();
        let m = parse_qt_metadata(&body).unwrap();
        assert_eq!(m.items.len(), 2);
        assert_eq!(m.items[0].key_index, 9);
        assert!(m.key_for(&m.items[0]).is_none());
        let kv = m.to_key_values();
        assert_eq!(kv.len(), 1);
        assert_eq!(kv[0].as_str(), Some("ok"));
    }

    #[test]
    fn hostile_counts_are_refused_before_allocation() {
        let mut ctry = vec![0u8; 4];
        ctry.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(parse_locale_lists(&ctry, "ctry").is_err());
        let mut ctry = vec![0u8; 4];
        ctry.extend_from_slice(&1u32.to_be_bytes());
        ctry.extend_from_slice(&500u16.to_be_bytes()); // 500 codes, none present
        assert!(parse_locale_lists(&ctry, "ctry").is_err());
        let mut k = vec![0u8; 4];
        k.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(parse_keys_atom(&k).is_err());
        // Item child with an impossible size.
        let mut bad = Vec::new();
        bad.extend_from_slice(&100u32.to_be_bytes());
        bad.extend_from_slice(b"data");
        assert!(parse_item(1, &bad).is_err());
        // Extended-size child that overruns.
        let mut ext = Vec::new();
        ext.extend_from_slice(&1u32.to_be_bytes());
        ext.extend_from_slice(b"data");
        ext.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(parse_item(1, &ext).is_err());
    }

    #[test]
    fn fullbox_prefix_detection() {
        assert_eq!(fullbox_prefix_len(&hdlr(b"mdta", "")), 0);
        let mut iso = vec![0u8; 4];
        iso.extend_from_slice(&hdlr(b"mdta", ""));
        assert_eq!(fullbox_prefix_len(&iso), 4);
        assert_eq!(fullbox_prefix_len(&[1, 2, 3]), 0);
    }
}
