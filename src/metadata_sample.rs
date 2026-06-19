//! ISO BMFF timed-metadata sample entries (ISO/IEC 14496-12 §12.3.3).
//!
//! A timed-metadata track (`hdlr` component subtype `meta`, with a
//! null media header `nmhd`) declares its sample format through one of
//! three `MetaDataSampleEntry` subclasses inside `stsd`:
//!
//! * `metx` — [`XMLMetaDataSampleEntry`]: XML metadata. Carries an
//!   optional `content_encoding`, a mandatory `namespace`, and an
//!   optional `schema_location` (all space-separated, NUL-terminated
//!   UTF-8 lists), followed by optional boxes (`btrt`, …).
//! * `mett` — [`TextMetaDataSampleEntry`]: free-form text metadata.
//!   Carries an optional `content_encoding`, a mandatory `mime_format`,
//!   then optional `btrt` and `txtC` (TextConfigBox) boxes.
//! * `urim` — [`URIMetaSampleEntry`]: URI-identified metadata. Carries
//!   a mandatory `uri ` box, an optional `uriI` (URIInitBox), and an
//!   optional `btrt`.
//!
//! All three extend `SampleEntry`, so the on-wire body starts with the
//! universal 8-byte tail of the SampleEntry header (6 reserved bytes +
//! 2-byte `data_reference_index`). `parse_*` here receives the body
//! **after** that 8-byte tail — i.e. starting at `content_encoding`
//! (`metx`/`mett`) or the first nested box (`urim`) — matching how
//! [`crate::track::parse_stsd`] slices a sample-description entry.
//!
//! Spec layout (ISO/IEC 14496-12:2015 §12.3.3.2):
//!
//! ```text
//! class MetaDataSampleEntry(codingname) extends SampleEntry(codingname) {
//!     Box[] other_boxes;   // optional
//! }
//! class XMLMetaDataSampleEntry()  extends MetaDataSampleEntry('metx') {
//!     string content_encoding;  // optional
//!     string namespace;
//!     string schema_location;   // optional
//!     BitRateBox();             // optional
//! }
//! class TextConfigBox() extends FullBox('txtC', 0, 0) { string text_config; }
//! class TextMetaDataSampleEntry() extends MetaDataSampleEntry('mett') {
//!     string content_encoding;  // optional
//!     string mime_format;
//!     BitRateBox();             // optional
//!     TextConfigBox();          // optional
//! }
//! aligned(8) class URIBox     extends FullBox('uri ', 0, 0) { string theURI; }
//! aligned(8) class URIInitBox extends FullBox('uriI', 0, 0) { uint8 uri_initialization_data[]; }
//! class URIMetaSampleEntry()  extends MetaDataSampleEntry('urim') {
//!     URIBox     the_label;
//!     URIInitBox init;  // optional
//!     BitRateBox();     // optional
//! }
//! ```
//!
//! `content_encoding` / `namespace` / `schema_location` may be omitted;
//! the spec models "omitted" as an empty (zero-length) NUL-terminated
//! string. Because `metx` packs up to three consecutive strings before
//! the first box, the number of leading strings is recovered by reading
//! NUL-terminated runs until the remaining bytes look like a box (a
//! 4-byte big-endian size followed by a printable FourCC) or run out.

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// BitRateBox (`btrt`, ISO/IEC 14496-12 §8.5.2.2). Signals the bit-rate
/// of an elementary stream for buffer configuration. Carried optionally
/// at the end of any [`MetadataSampleEntry`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BitRate {
    /// Size, in bytes, of the decoding buffer for the elementary stream.
    pub buffer_size_db: u32,
    /// Maximum rate in bits/second over any one-second window.
    pub max_bitrate: u32,
    /// Average rate in bits/second over the entire presentation.
    pub avg_bitrate: u32,
}

/// Parse a `btrt` BitRateBox payload (three 32-bit big-endian fields).
///
/// `btrt` is a plain `Box`, not a `FullBox`, so the payload begins at
/// `bufferSizeDB` with no version/flags prefix (ISO/IEC 14496-12
/// §8.5.2.2).
pub fn parse_btrt(payload: &[u8]) -> Result<BitRate> {
    if payload.len() < 12 {
        return Err(Error::invalid("MOV: btrt payload < 12 bytes"));
    }
    Ok(BitRate {
        buffer_size_db: u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
        max_bitrate: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
        avg_bitrate: u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]),
    })
}

/// Parsed timed-metadata sample entry. One variant per `MetaDataSampleEntry`
/// subclass recognised in `stsd`. The `format` FourCC that selected the
/// variant is preserved on the owning [`crate::track::SampleDescription`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataSampleEntry {
    /// `metx` — XML metadata sample entry.
    Xml(XmlMetadataSampleEntry),
    /// `mett` — text metadata sample entry.
    Text(TextMetadataSampleEntry),
    /// `urim` — URI metadata sample entry.
    Uri(UriMetadataSampleEntry),
}

impl MetadataSampleEntry {
    /// The optional BitRateBox (`btrt`) common to all three variants.
    pub fn bitrate(&self) -> Option<BitRate> {
        match self {
            MetadataSampleEntry::Xml(e) => e.bitrate,
            MetadataSampleEntry::Text(e) => e.bitrate,
            MetadataSampleEntry::Uri(e) => e.bitrate,
        }
    }
}

/// `metx` — XMLMetaDataSampleEntry (ISO/IEC 14496-12 §12.3.3.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct XmlMetadataSampleEntry {
    /// MIME type identifying the content encoding (e.g. `application/zip`);
    /// empty when the metadata is not encoded.
    pub content_encoding: String,
    /// Space-separated list of XML namespaces the samples conform to.
    pub namespace: String,
    /// Optional space-separated list of XML-schema URLs; empty when absent.
    pub schema_location: String,
    /// Optional bit-rate box.
    pub bitrate: Option<BitRate>,
}

/// `mett` — TextMetaDataSampleEntry (ISO/IEC 14496-12 §12.3.3.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextMetadataSampleEntry {
    /// MIME type identifying the content encoding; empty when not encoded.
    pub content_encoding: String,
    /// MIME type identifying the content format of the samples
    /// (e.g. `text/html`, `text/plain`).
    pub mime_format: String,
    /// `txtC` TextConfigBox initial text prepended to each sync sample;
    /// `None` when the box is absent.
    pub text_config: Option<String>,
    /// Optional bit-rate box.
    pub bitrate: Option<BitRate>,
}

/// `urim` — URIMetaSampleEntry (ISO/IEC 14496-12 §12.3.3.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UriMetadataSampleEntry {
    /// `uri ` box — the URI defining the form of the metadata.
    pub the_uri: String,
    /// `uriI` URIInitBox — opaque initialization data; `None` when absent.
    pub init: Option<Vec<u8>>,
    /// Optional bit-rate box.
    pub bitrate: Option<BitRate>,
}

/// Parsed `SubtitleSampleEntry` subclass (ISO/IEC 14496-12 §12.6.3).
/// Carried on a subtitle track (`hdlr` subtype `subt`), structurally a
/// close sibling of [`MetadataSampleEntry`] — both reuse the BitRateBox
/// (`btrt`) and, for the text variant, the TextConfigBox (`txtC`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubtitleSampleEntry {
    /// `stpp` — XML subtitle sample entry (e.g. TTML).
    Xml(XmlSubtitleSampleEntry),
    /// `sbtt` — text subtitle sample entry.
    Text(TextSubtitleSampleEntry),
}

impl SubtitleSampleEntry {
    /// The optional BitRateBox (`btrt`) common to both variants.
    pub fn bitrate(&self) -> Option<BitRate> {
        match self {
            SubtitleSampleEntry::Xml(e) => e.bitrate,
            SubtitleSampleEntry::Text(e) => e.bitrate,
        }
    }
}

/// `stpp` — XMLSubtitleSampleEntry (ISO/IEC 14496-12 §12.6.3.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct XmlSubtitleSampleEntry {
    /// Space-separated list of XML namespaces the samples conform to.
    pub namespace: String,
    /// Optional space-separated list of XML-schema URLs; empty when absent.
    pub schema_location: String,
    /// Media type(s) of auxiliary resources (images / fonts) stored as
    /// subtitle subsamples; empty when none are present.
    pub auxiliary_mime_types: String,
    /// Optional bit-rate box.
    pub bitrate: Option<BitRate>,
}

/// `sbtt` — TextSubtitleSampleEntry (ISO/IEC 14496-12 §12.6.3.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextSubtitleSampleEntry {
    /// MIME type identifying the content encoding; empty when not encoded.
    pub content_encoding: String,
    /// MIME type identifying the content format of the samples
    /// (e.g. `text/html`, `text/plain`).
    pub mime_format: String,
    /// `txtC` TextConfigBox initial text prepended to each sync sample;
    /// `None` when the box is absent.
    pub text_config: Option<String>,
    /// Optional bit-rate box.
    pub bitrate: Option<BitRate>,
}

/// Read a NUL-terminated UTF-8 string starting at `*pos`. Advances `*pos`
/// past the terminating NUL. Returns the string (excluding the NUL). When
/// no NUL is found before end-of-buffer, the remaining bytes are taken as
/// the string and `*pos` advanced to the end (lenient against a missing
/// terminator on the final field).
fn read_c_string(buf: &[u8], pos: &mut usize) -> String {
    let start = *pos;
    let mut end = start;
    while end < buf.len() && buf[end] != 0 {
        end += 1;
    }
    let s = String::from_utf8_lossy(&buf[start..end]).into_owned();
    // Skip the NUL when present; otherwise we consumed to the end.
    *pos = if end < buf.len() { end + 1 } else { end };
    s
}

/// Heuristic: do the bytes at `pos` look like the start of a child box?
/// A box begins with a 4-byte big-endian size (>= 8 and not absurd)
/// followed by a 4-byte FourCC of printable ASCII. Used to decide where
/// the leading NUL-terminated string run ends and the optional-box list
/// begins in `metx`/`mett`.
fn looks_like_box(buf: &[u8], pos: usize) -> bool {
    if pos + 8 > buf.len() {
        return false;
    }
    let size = u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
    if size < 8 || pos + size > buf.len() {
        // size==0 (extends to end) is also a legal box, but we never emit
        // it for these tiny boxes; treat only well-bounded boxes as such.
        return false;
    }
    // FourCC printable ASCII (0x20..=0x7E).
    buf[pos + 4..pos + 8]
        .iter()
        .all(|&b| (0x20..=0x7e).contains(&b))
}

/// Walk the optional box list at the tail of a metadata sample entry,
/// invoking `visit(fourcc, payload)` for each well-bounded child box.
fn walk_boxes<F>(buf: &[u8], mut pos: usize, mut visit: F) -> Result<()>
where
    F: FnMut(&[u8; 4], &[u8]) -> Result<()>,
{
    while pos + 8 <= buf.len() {
        let size =
            u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
        let (body_end, advance) = if size == 0 {
            (buf.len(), buf.len() - pos)
        } else if size < 8 || pos + size > buf.len() {
            break; // malformed tail — stop leniently
        } else {
            (pos + size, size)
        };
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&buf[pos + 4..pos + 8]);
        visit(&fc, &buf[pos + 8..body_end])?;
        pos += advance;
    }
    Ok(())
}

/// Parse a `txtC` TextConfigBox payload (FullBox: 4-byte version+flags,
/// then a NUL-terminated UTF-8 `text_config` string).
fn parse_txtc(payload: &[u8]) -> String {
    if payload.len() < 4 {
        return String::new();
    }
    let mut pos = 4;
    read_c_string(payload, &mut pos)
}

/// Parse a FullBox-wrapped NUL-terminated string (`uri `): 4-byte
/// version+flags then the string.
fn parse_fullbox_string(payload: &[u8]) -> String {
    if payload.len() < 4 {
        return String::new();
    }
    let mut pos = 4;
    read_c_string(payload, &mut pos)
}

/// Parse a `metx` XMLMetaDataSampleEntry body (after the SampleEntry
/// 8-byte tail). ISO/IEC 14496-12 §12.3.3.2.
///
/// The leading strings are `content_encoding` (optional), `namespace`
/// (mandatory), `schema_location` (optional). Because all three are
/// consecutive NUL-terminated strings with optional ones modelled as
/// empty, we read NUL-terminated runs until a child box begins (or the
/// buffer ends), then assign positionally: the last string read is
/// `schema_location` only if three were present; with two strings the
/// trailing one is `schema_location` when it is empty/box-bounded, but
/// the spec orders them encoding→namespace→schema, so we assign the
/// first up to three reads in that order.
pub fn parse_metx(body: &[u8]) -> Result<XmlMetadataSampleEntry> {
    let mut pos = 0usize;
    let mut strings: Vec<String> = Vec::new();
    while pos < body.len() && !looks_like_box(body, pos) && strings.len() < 3 {
        strings.push(read_c_string(body, &mut pos));
    }
    // Positional assignment per §12.3.3.2 field order.
    let mut entry = XmlMetadataSampleEntry::default();
    match strings.len() {
        0 => {}
        1 => entry.namespace = strings.pop().unwrap(),
        2 => {
            // content_encoding + namespace, OR namespace + schema_location.
            // The spec lists content_encoding first; treat two strings as
            // content_encoding then namespace (schema_location omitted).
            entry.namespace = strings.pop().unwrap();
            entry.content_encoding = strings.pop().unwrap();
        }
        _ => {
            entry.schema_location = strings.pop().unwrap();
            entry.namespace = strings.pop().unwrap();
            entry.content_encoding = strings.pop().unwrap();
        }
    }
    walk_boxes(body, pos, |fc, payload| {
        if fc == b"btrt" {
            entry.bitrate = Some(parse_btrt(payload)?);
        }
        Ok(())
    })?;
    Ok(entry)
}

/// Parse a `mett` TextMetaDataSampleEntry body (after the SampleEntry
/// 8-byte tail). ISO/IEC 14496-12 §12.3.3.2.
///
/// Leading strings: `content_encoding` (optional) then `mime_format`
/// (mandatory). A single string is taken as `mime_format`.
pub fn parse_mett(body: &[u8]) -> Result<TextMetadataSampleEntry> {
    let mut pos = 0usize;
    let mut strings: Vec<String> = Vec::new();
    while pos < body.len() && !looks_like_box(body, pos) && strings.len() < 2 {
        strings.push(read_c_string(body, &mut pos));
    }
    let mut entry = TextMetadataSampleEntry::default();
    match strings.len() {
        0 => {}
        1 => entry.mime_format = strings.pop().unwrap(),
        _ => {
            entry.mime_format = strings.pop().unwrap();
            entry.content_encoding = strings.pop().unwrap();
        }
    }
    walk_boxes(body, pos, |fc, payload| {
        match fc {
            b"btrt" => entry.bitrate = Some(parse_btrt(payload)?),
            b"txtC" => entry.text_config = Some(parse_txtc(payload)),
            _ => {}
        }
        Ok(())
    })?;
    Ok(entry)
}

/// Parse a `urim` URIMetaSampleEntry body (after the SampleEntry 8-byte
/// tail). ISO/IEC 14496-12 §12.3.3.2. The body is a sequence of boxes:
/// mandatory `uri `, optional `uriI`, optional `btrt`.
pub fn parse_urim(body: &[u8]) -> Result<UriMetadataSampleEntry> {
    let mut entry = UriMetadataSampleEntry::default();
    walk_boxes(body, 0, |fc, payload| {
        match fc {
            b"uri " => entry.the_uri = parse_fullbox_string(payload),
            b"uriI" => {
                // URIInitBox: FullBox header (4 bytes) then opaque data.
                let data = if payload.len() >= 4 {
                    payload[4..].to_vec()
                } else {
                    Vec::new()
                };
                entry.init = Some(data);
            }
            b"btrt" => entry.bitrate = Some(parse_btrt(payload)?),
            _ => {}
        }
        Ok(())
    })?;
    Ok(entry)
}

/// Dispatch a metadata sample-description body to the right variant by
/// its `format` FourCC. Returns `Ok(None)` for any FourCC that is not a
/// recognised `MetaDataSampleEntry` subclass.
pub fn parse_metadata_sample_entry(
    format: &[u8; 4],
    body: &[u8],
) -> Result<Option<MetadataSampleEntry>> {
    match format {
        b"metx" => Ok(Some(MetadataSampleEntry::Xml(parse_metx(body)?))),
        b"mett" => Ok(Some(MetadataSampleEntry::Text(parse_mett(body)?))),
        b"urim" => Ok(Some(MetadataSampleEntry::Uri(parse_urim(body)?))),
        _ => Ok(None),
    }
}

/// Parse a `stpp` XMLSubtitleSampleEntry body (after the SampleEntry
/// 8-byte tail). ISO/IEC 14496-12 §12.6.3.2. Leading strings:
/// `namespace` (mandatory), `schema_location` (optional),
/// `auxiliary_mime_types` (optional).
pub fn parse_stpp(body: &[u8]) -> Result<XmlSubtitleSampleEntry> {
    let mut pos = 0usize;
    let mut strings: Vec<String> = Vec::new();
    while pos < body.len() && !looks_like_box(body, pos) && strings.len() < 3 {
        strings.push(read_c_string(body, &mut pos));
    }
    let mut entry = XmlSubtitleSampleEntry::default();
    // Fields are positional in declaration order: namespace,
    // schema_location, auxiliary_mime_types. Assign as many as present.
    let mut it = strings.into_iter();
    if let Some(s) = it.next() {
        entry.namespace = s;
    }
    if let Some(s) = it.next() {
        entry.schema_location = s;
    }
    if let Some(s) = it.next() {
        entry.auxiliary_mime_types = s;
    }
    walk_boxes(body, pos, |fc, payload| {
        if fc == b"btrt" {
            entry.bitrate = Some(parse_btrt(payload)?);
        }
        Ok(())
    })?;
    Ok(entry)
}

/// Parse a `sbtt` TextSubtitleSampleEntry body (after the SampleEntry
/// 8-byte tail). ISO/IEC 14496-12 §12.6.3.2. Same shape as `mett`:
/// `content_encoding` (optional) then `mime_format` (mandatory), with
/// optional `btrt` + `txtC` boxes.
pub fn parse_sbtt(body: &[u8]) -> Result<TextSubtitleSampleEntry> {
    let mut pos = 0usize;
    let mut strings: Vec<String> = Vec::new();
    while pos < body.len() && !looks_like_box(body, pos) && strings.len() < 2 {
        strings.push(read_c_string(body, &mut pos));
    }
    let mut entry = TextSubtitleSampleEntry::default();
    match strings.len() {
        0 => {}
        1 => entry.mime_format = strings.pop().unwrap(),
        _ => {
            entry.mime_format = strings.pop().unwrap();
            entry.content_encoding = strings.pop().unwrap();
        }
    }
    walk_boxes(body, pos, |fc, payload| {
        match fc {
            b"btrt" => entry.bitrate = Some(parse_btrt(payload)?),
            b"txtC" => entry.text_config = Some(parse_txtc(payload)),
            _ => {}
        }
        Ok(())
    })?;
    Ok(entry)
}

/// Dispatch a subtitle sample-description body to the right variant by
/// its `format` FourCC. Returns `Ok(None)` for any FourCC that is not a
/// recognised `SubtitleSampleEntry` subclass.
pub fn parse_subtitle_sample_entry(
    format: &[u8; 4],
    body: &[u8],
) -> Result<Option<SubtitleSampleEntry>> {
    match format {
        b"stpp" => Ok(Some(SubtitleSampleEntry::Xml(parse_stpp(body)?))),
        b"sbtt" => Ok(Some(SubtitleSampleEntry::Text(parse_sbtt(body)?))),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn btrt_box() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&20u32.to_be_bytes());
        b.extend_from_slice(b"btrt");
        b.extend_from_slice(&100u32.to_be_bytes()); // buffer_size_db
        b.extend_from_slice(&200u32.to_be_bytes()); // max_bitrate
        b.extend_from_slice(&150u32.to_be_bytes()); // avg_bitrate
        b
    }

    #[test]
    fn parse_btrt_fields() {
        let b = btrt_box();
        let br = parse_btrt(&b[8..]).unwrap();
        assert_eq!(br.buffer_size_db, 100);
        assert_eq!(br.max_bitrate, 200);
        assert_eq!(br.avg_bitrate, 150);
    }

    #[test]
    fn parse_btrt_too_short() {
        assert!(parse_btrt(&[0, 0, 0, 1]).is_err());
    }

    #[test]
    fn metx_namespace_only() {
        let mut body = Vec::new();
        body.extend_from_slice(b""); // content_encoding omitted (empty)
        body.push(0);
        body.extend_from_slice(b"http://example.com/ns");
        body.push(0);
        body.extend_from_slice(b""); // schema_location omitted
        body.push(0);
        let e = parse_metx(&body).unwrap();
        assert_eq!(e.content_encoding, "");
        assert_eq!(e.namespace, "http://example.com/ns");
        assert_eq!(e.schema_location, "");
        assert!(e.bitrate.is_none());
    }

    #[test]
    fn metx_all_three_strings_and_btrt() {
        let mut body = Vec::new();
        body.extend_from_slice(b"application/zip");
        body.push(0);
        body.extend_from_slice(b"urn:ns");
        body.push(0);
        body.extend_from_slice(b"http://s/schema.xsd");
        body.push(0);
        body.extend_from_slice(&btrt_box());
        let e = parse_metx(&body).unwrap();
        assert_eq!(e.content_encoding, "application/zip");
        assert_eq!(e.namespace, "urn:ns");
        assert_eq!(e.schema_location, "http://s/schema.xsd");
        assert_eq!(e.bitrate.unwrap().avg_bitrate, 150);
    }

    #[test]
    fn metx_two_strings() {
        let mut body = Vec::new();
        body.extend_from_slice(b"application/zip");
        body.push(0);
        body.extend_from_slice(b"urn:ns");
        body.push(0);
        let e = parse_metx(&body).unwrap();
        assert_eq!(e.content_encoding, "application/zip");
        assert_eq!(e.namespace, "urn:ns");
        assert_eq!(e.schema_location, "");
    }

    #[test]
    fn mett_mime_and_txtc() {
        let mut body = Vec::new();
        body.extend_from_slice(b""); // content_encoding empty
        body.push(0);
        body.extend_from_slice(b"text/plain");
        body.push(0);
        // txtC box
        let mut txtc = Vec::new();
        let cfg = b"prefix-text";
        let size = 8 + 4 + cfg.len() + 1;
        txtc.extend_from_slice(&(size as u32).to_be_bytes());
        txtc.extend_from_slice(b"txtC");
        txtc.extend_from_slice(&0u32.to_be_bytes()); // version+flags
        txtc.extend_from_slice(cfg);
        txtc.push(0);
        body.extend_from_slice(&txtc);
        let e = parse_mett(&body).unwrap();
        assert_eq!(e.content_encoding, "");
        assert_eq!(e.mime_format, "text/plain");
        assert_eq!(e.text_config.as_deref(), Some("prefix-text"));
    }

    #[test]
    fn mett_single_string_is_mime() {
        let mut body = Vec::new();
        body.extend_from_slice(b"text/html");
        body.push(0);
        let e = parse_mett(&body).unwrap();
        assert_eq!(e.content_encoding, "");
        assert_eq!(e.mime_format, "text/html");
    }

    #[test]
    fn urim_uri_init_btrt() {
        let mut body = Vec::new();
        // uri box
        let uri = b"urn:mpeg:dash:event:2012";
        let usize_ = 8 + 4 + uri.len() + 1;
        body.extend_from_slice(&(usize_ as u32).to_be_bytes());
        body.extend_from_slice(b"uri ");
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(uri);
        body.push(0);
        // uriI box
        let init = [1u8, 2, 3, 4];
        let isize_ = 8 + 4 + init.len();
        body.extend_from_slice(&(isize_ as u32).to_be_bytes());
        body.extend_from_slice(b"uriI");
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&init);
        // btrt
        body.extend_from_slice(&btrt_box());
        let e = parse_urim(&body).unwrap();
        assert_eq!(e.the_uri, "urn:mpeg:dash:event:2012");
        assert_eq!(e.init.as_deref(), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(e.bitrate.unwrap().max_bitrate, 200);
    }

    #[test]
    fn dispatch_unknown_returns_none() {
        assert!(parse_metadata_sample_entry(b"avc1", &[]).unwrap().is_none());
    }

    #[test]
    fn dispatch_recognises_all_three() {
        let mut metx = Vec::new();
        metx.extend_from_slice(b"urn:ns");
        metx.push(0);
        assert!(matches!(
            parse_metadata_sample_entry(b"metx", &metx).unwrap(),
            Some(MetadataSampleEntry::Xml(_))
        ));
        let mut mett = Vec::new();
        mett.extend_from_slice(b"text/plain");
        mett.push(0);
        assert!(matches!(
            parse_metadata_sample_entry(b"mett", &mett).unwrap(),
            Some(MetadataSampleEntry::Text(_))
        ));
        let mut urim = Vec::new();
        let uri = b"x";
        urim.extend_from_slice(&((8 + 4 + uri.len() + 1) as u32).to_be_bytes());
        urim.extend_from_slice(b"uri ");
        urim.extend_from_slice(&0u32.to_be_bytes());
        urim.extend_from_slice(uri);
        urim.push(0);
        assert!(matches!(
            parse_metadata_sample_entry(b"urim", &urim).unwrap(),
            Some(MetadataSampleEntry::Uri(_))
        ));
    }

    #[test]
    fn bitrate_accessor() {
        let e = MetadataSampleEntry::Text(TextMetadataSampleEntry {
            bitrate: Some(BitRate {
                buffer_size_db: 1,
                max_bitrate: 2,
                avg_bitrate: 3,
            }),
            ..Default::default()
        });
        assert_eq!(e.bitrate().unwrap().avg_bitrate, 3);
    }

    #[test]
    fn stpp_namespace_schema_aux_and_btrt() {
        let mut body = Vec::new();
        body.extend_from_slice(b"http://www.w3.org/ns/ttml");
        body.push(0); // namespace
        body.extend_from_slice(b"ttml.xsd");
        body.push(0); // schema_location
        body.extend_from_slice(b"image/png font/ttf");
        body.push(0); // auxiliary_mime_types
        body.extend_from_slice(&btrt_box());
        let e = parse_stpp(&body).unwrap();
        assert_eq!(e.namespace, "http://www.w3.org/ns/ttml");
        assert_eq!(e.schema_location, "ttml.xsd");
        assert_eq!(e.auxiliary_mime_types, "image/png font/ttf");
        assert_eq!(e.bitrate.unwrap().avg_bitrate, 150);
    }

    #[test]
    fn stpp_namespace_only() {
        let mut body = Vec::new();
        body.extend_from_slice(b"urn:ns");
        body.push(0);
        let e = parse_stpp(&body).unwrap();
        assert_eq!(e.namespace, "urn:ns");
        assert_eq!(e.schema_location, "");
        assert_eq!(e.auxiliary_mime_types, "");
        assert!(e.bitrate.is_none());
    }

    #[test]
    fn sbtt_mime_and_txtc() {
        let mut body = Vec::new();
        body.push(0); // content_encoding empty
        body.extend_from_slice(b"text/plain");
        body.push(0);
        let cfg = b"WEBVTT";
        let size = 8 + 4 + cfg.len() + 1;
        body.extend_from_slice(&(size as u32).to_be_bytes());
        body.extend_from_slice(b"txtC");
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(cfg);
        body.push(0);
        let e = parse_sbtt(&body).unwrap();
        assert_eq!(e.content_encoding, "");
        assert_eq!(e.mime_format, "text/plain");
        assert_eq!(e.text_config.as_deref(), Some("WEBVTT"));
    }

    #[test]
    fn subtitle_dispatch_and_accessor() {
        let mut stpp = Vec::new();
        stpp.extend_from_slice(b"urn:ns");
        stpp.push(0);
        assert!(matches!(
            parse_subtitle_sample_entry(b"stpp", &stpp).unwrap(),
            Some(SubtitleSampleEntry::Xml(_))
        ));
        let mut sbtt = Vec::new();
        sbtt.extend_from_slice(b"text/plain");
        sbtt.push(0);
        assert!(matches!(
            parse_subtitle_sample_entry(b"sbtt", &sbtt).unwrap(),
            Some(SubtitleSampleEntry::Text(_))
        ));
        assert!(parse_subtitle_sample_entry(b"tx3g", &[]).unwrap().is_none());
        let e = SubtitleSampleEntry::Xml(XmlSubtitleSampleEntry {
            bitrate: Some(BitRate {
                buffer_size_db: 9,
                max_bitrate: 8,
                avg_bitrate: 7,
            }),
            ..Default::default()
        });
        assert_eq!(e.bitrate().unwrap().avg_bitrate, 7);
    }
}
