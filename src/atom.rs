//! QuickTime atom walker.
//!
//! Apple QuickTime File Format Specification (QTFF, 2001-03-01),
//! §"Atoms" / §"Atom Structure" (pp. 17–19) defines an atom as a
//! `[size: u32 BE][type: 4 ASCII bytes][payload]` record.
//!
//! Two special size values:
//!
//! * `size == 1` — the next 8 bytes are a 64-bit big-endian
//!   `extended size`, used for media-data atoms larger than 2^32 bytes
//!   (see QTFF p. 19, "Extended Size").
//! * `size == 0` — the atom extends to the end of the file
//!   (top-level only, QTFF p. 19, Figure 1-2 right-most case).
//!
//! All multi-byte integers are big-endian (QTFF p. 17, "Atoms"
//! paragraph 3).
//!
//! The QTFF is the immediate ancestor of ISO BMFF (ISO/IEC 14496-12),
//! and the `[size][type]` framing is identical across the two — but
//! QTFF retains semantics (Apple-pre-ICC `colr`, `gama`, `clap`,
//! `pasp`, reference movies, edit lists with media-time = -1 empties)
//! that have no ISO BMFF equivalent. The walker here is intentionally
//! framing-only and emits semantic-neutral records the upper layers
//! interpret.

use std::io::{Read, Seek, SeekFrom};

#[cfg(feature = "registry")]
use oxideav_core::{Error, Result};

#[cfg(not(feature = "registry"))]
use crate::standalone::{Error, Result};

/// Decoded atom header. The `payload_offset` is the byte position
/// inside the input where the atom body begins (i.e. the position
/// immediately after the header — 8 bytes for a regular atom, 16
/// bytes for one with an extended 64-bit size).
#[derive(Clone, Copy, Debug)]
pub struct AtomHeader {
    /// FourCC type (4 bytes, big-endian on disk).
    pub fourcc: [u8; 4],
    /// Total atom size in bytes including the header. `None` means
    /// "to end of file" (size == 0 case from QTFF p. 19).
    pub total_size: Option<u64>,
    /// Bytes consumed by the header (8 or 16).
    pub header_len: u64,
    /// Byte offset (from start of input) of the payload.
    pub payload_offset: u64,
}

impl AtomHeader {
    /// Print the FourCC as a string slice when ASCII-printable;
    /// returns `"????"` if any byte is non-printable.
    pub fn type_str(&self) -> &str {
        std::str::from_utf8(&self.fourcc).unwrap_or("????")
    }

    /// Payload length in bytes, or `None` for an open-ended atom.
    pub fn payload_len(&self) -> Option<u64> {
        self.total_size.map(|t| t - self.header_len)
    }

    /// True if this is a container atom in the standard QTFF tree we
    /// recurse into. Listed verbatim from QTFF Chapters 2–3
    /// (`moov`/`trak`/`mdia`/`minf`/`stbl`/`edts`/`udta`/`dinf`/`tref`)
    /// plus the QT-specific track-clip / track-matte containers.
    pub fn is_known_container(&self) -> bool {
        matches!(
            &self.fourcc,
            b"moov"
                | b"trak"
                | b"mdia"
                | b"minf"
                | b"stbl"
                | b"edts"
                | b"udta"
                | b"dinf"
                | b"tref"
                | b"clip"
                | b"matt"
                | b"imap"
                | b"rmra"
                | b"rmda"
        )
    }
}

/// Read the next atom header at the reader's current position.
///
/// Returns `Ok(None)` on a clean EOF (zero bytes available before
/// reading any of the header). Returns `Err(InvalidData)` on a
/// truncated header (some but not all of the 8 / 16 bytes available).
pub fn read_atom_header<R: Read + Seek + ?Sized>(r: &mut R) -> Result<Option<AtomHeader>> {
    let start = r.stream_position()?;

    let mut hdr = [0u8; 8];
    let mut got = 0;
    while got < 8 {
        match r.read(&mut hdr[got..]) {
            Ok(0) => {
                if got == 0 {
                    return Ok(None);
                } else {
                    return Err(Error::invalid("MOV: truncated atom header"));
                }
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    let mut fourcc = [0u8; 4];
    fourcc.copy_from_slice(&hdr[4..8]);

    let (total_size, header_len) = match size32 {
        0 => (None, 8u64),
        1 => {
            let mut ext = [0u8; 8];
            r.read_exact(&mut ext)?;
            let large = u64::from_be_bytes(ext);
            if large < 16 {
                return Err(Error::invalid(
                    "MOV: extended atom size below 16-byte minimum",
                ));
            }
            (Some(large), 16u64)
        }
        n => {
            if (n as u64) < 8 {
                return Err(Error::invalid("MOV: atom size below 8-byte minimum"));
            }
            (Some(n as u64), 8u64)
        }
    };

    // Reject any header whose declared end byte would overflow `u64`.
    // This is the single point that bounds every downstream
    // `payload_offset + payload_len()` / `body_end` computation: once
    // we have proven `start + total_size <= u64::MAX`, the equivalent
    // form `payload_offset + (total_size - header_len)` also fits,
    // so the top-level walker, `walk_children`, and the per-atom
    // `body_end` helpers can no longer integer-overflow on a forged
    // extended size near `u64::MAX`. The fuzz target's crash input
    // `00000008 00000000  00000001 09ffffff ffffffff ffffffff …`
    // exercises exactly this path: an 8-byte placeholder atom
    // followed by a `size=1` extended-size atom whose `largesize`
    // is `u64::MAX`, which without this check overflows
    // `payload_offset + (u64::MAX - 16)` in `src/demuxer.rs` line 480
    // and `src/atom.rs` line 263 (debug builds panic; release builds
    // silently wrap and would compute a body_end far below
    // `total_len`, masking the malformed input).
    if let Some(t) = total_size {
        if start.checked_add(t).is_none() {
            return Err(Error::invalid(format!(
                "MOV: atom '{}' declared size {t} from offset {start} overflows u64",
                std::str::from_utf8(&fourcc).unwrap_or("????"),
            )));
        }
    }

    Ok(Some(AtomHeader {
        fourcc,
        total_size,
        header_len,
        payload_offset: start + header_len,
    }))
}

/// Skip the rest of an atom's payload in a seekable reader.
///
/// For a known-size atom this jumps `payload_len()` bytes forward.
/// For an open-ended atom (size == 0) this seeks to the end of the
/// stream.
pub fn skip_payload<R: Seek + ?Sized>(r: &mut R, h: &AtomHeader) -> Result<()> {
    match h.payload_len() {
        Some(0) => Ok(()),
        Some(n) => {
            r.seek(SeekFrom::Current(n as i64))?;
            Ok(())
        }
        None => {
            r.seek(SeekFrom::End(0))?;
            Ok(())
        }
    }
}

/// Upper bound on a single atom body read into memory via
/// [`read_payload`].
///
/// `read_payload` materialises the entire body into a `Vec<u8>` so the
/// per-atom parsers can index into it. A declared size larger than this
/// almost certainly means a forged / corrupt size field rather than a
/// legitimate metadata atom: `ftyp` / `moov` / `mvhd` / `tkhd` / `mdhd` /
/// `stsd` / `stts` / `stsc` / `stsz` / `stco` / `co64` / `stss` / `sdtp` /
/// `subs` / `saiz` / `saio` / `sgpd` / `sbgp` / `tref` / `udta` / `meta` /
/// `keys` / `ilst` / `kind` / `tsel` / `load` / `clip` / `crgn` / `matt` /
/// `kmat` / `gama` / `pasp` / `clap` / `colr` / `chan` / `tapt` / `clef` /
/// `prof` / `enof` / `pdin` / `sidx` / `styp` / `prft` / `pnot` / `ctab`
/// are all well under a megabyte in real files. Even an extremely long
/// `keys`/`ilst` Apple metadata block stays under a few hundred KiB.
///
/// `mdat` (the sample-data atom — gigabytes legitimately) is **never**
/// read via [`read_payload`] in this crate: per-sample reads `seek` into
/// `mdat` and pull only the bytes for the requested sample (see
/// [`crate::demuxer::MovDemuxer::next_packet`]). So capping
/// [`read_payload`] at 64 MiB does not bound the legitimate file
/// size — only a single in-memory metadata atom.
///
/// The constant is `pub` so test fixtures can reason about the limit.
pub const MAX_INMEMORY_ATOM_BODY: u64 = 64 * 1024 * 1024;

/// Read the full body of an atom (size must be known).
///
/// Refuses to allocate more than [`MAX_INMEMORY_ATOM_BODY`] bytes — a
/// forged or corrupt extended `size` field on a small file would
/// otherwise drive a single `vec![0u8; n as usize]` into a multi-GiB
/// allocation that fails as an OOM rather than a clean parse error.
pub fn read_payload<R: Read + ?Sized>(r: &mut R, h: &AtomHeader) -> Result<Vec<u8>> {
    let n = h
        .payload_len()
        .ok_or_else(|| Error::invalid("MOV: cannot read open-ended atom body"))?;
    if n > MAX_INMEMORY_ATOM_BODY {
        return Err(Error::invalid(format!(
            "MOV: atom '{}' declares {n}-byte body, refusing in-memory read above {MAX_INMEMORY_ATOM_BODY}-byte cap",
            h.type_str(),
        )));
    }
    let mut buf = vec![0u8; n as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Read the full body of an atom whose declared size has already been
/// bounded against an outer envelope (a file length or a parent atom's
/// payload).
///
/// Differs from [`read_payload`] in that the caller passes the maximum
/// number of bytes still legitimately available at the cursor; the
/// helper bails before allocating when the atom's `payload_len()`
/// claims more than `max_remaining`. This catches forged-size attacks
/// that would otherwise pass the per-atom [`MAX_INMEMORY_ATOM_BODY`]
/// cap (e.g. a 32 MiB declaration on a 1 KiB file).
pub fn read_payload_bounded<R: Read + ?Sized>(
    r: &mut R,
    h: &AtomHeader,
    max_remaining: u64,
) -> Result<Vec<u8>> {
    let n = h
        .payload_len()
        .ok_or_else(|| Error::invalid("MOV: cannot read open-ended atom body"))?;
    if n > max_remaining {
        return Err(Error::invalid(format!(
            "MOV: atom '{}' body of {n} bytes exceeds {max_remaining}-byte envelope (truncated or forged size)",
            h.type_str(),
        )));
    }
    read_payload(r, h)
}

/// Walk the immediate children of a container atom, calling `visit`
/// for each child header. The reader is left at the byte just past
/// the parent atom's payload on success.
///
/// `parent_payload_end` should be the absolute byte position of the
/// first byte after the parent's payload (`payload_offset +
/// payload_len`). Pass `None` for an open-ended container (the walker
/// will read until EOF).
pub fn walk_children<R, F>(r: &mut R, parent_payload_end: Option<u64>, mut visit: F) -> Result<()>
where
    R: Read + Seek + ?Sized,
    F: FnMut(&mut R, &AtomHeader) -> Result<()>,
{
    loop {
        let pos = r.stream_position()?;
        if let Some(end) = parent_payload_end {
            if pos >= end {
                break;
            }
        }
        let hdr = match read_atom_header(r)? {
            Some(h) => h,
            None => break,
        };
        let body_end = hdr
            .total_size
            .map(|t| hdr.payload_offset + (t - hdr.header_len))
            .or(parent_payload_end)
            .ok_or_else(|| Error::invalid("MOV: open-ended atom inside container"))?;
        // Validate the child does not exceed the parent.
        if let Some(end) = parent_payload_end {
            if body_end > end {
                return Err(Error::invalid(
                    "MOV: child atom extends beyond parent payload",
                ));
            }
        }
        visit(r, &hdr)?;
        // Snap to the end of this atom regardless of how the visitor left
        // the cursor — robustness against visitors that read partial fields.
        r.seek(SeekFrom::Start(body_end))?;
    }
    Ok(())
}

/// Construct a FourCC from a 4-character string literal.
pub const fn fourcc(s: &str) -> [u8; 4] {
    let b = s.as_bytes();
    [b[0], b[1], b[2], b[3]]
}

// Common QTFF atom types. The list mirrors those declared in the
// Apple spec we actually parse in round 1; less-common atoms
// (`clip`/`crgn`/`matt`/`kmat`/`load`/`imap`, `rmra`/`rmda`, the
// Apple-specific `pasp`/`gama`/`clap`/`colr`) are recognised by the
// walker but parsed in later rounds.
pub const FTYP: [u8; 4] = fourcc("ftyp");
pub const MOOV: [u8; 4] = fourcc("moov");
/// Round 105 — Progressive Download Information Box (ISO/IEC 14496-12
/// §8.1.3). File-level FullBox of `(rate, initial_delay)` pairs.
pub const PDIN: [u8; 4] = fourcc("pdin");
pub const MVHD: [u8; 4] = fourcc("mvhd");
pub const TRAK: [u8; 4] = fourcc("trak");
pub const TKHD: [u8; 4] = fourcc("tkhd");
pub const EDTS: [u8; 4] = fourcc("edts");
pub const ELST: [u8; 4] = fourcc("elst");
pub const MDIA: [u8; 4] = fourcc("mdia");
pub const MDHD: [u8; 4] = fourcc("mdhd");
pub const HDLR: [u8; 4] = fourcc("hdlr");
pub const MINF: [u8; 4] = fourcc("minf");
pub const VMHD: [u8; 4] = fourcc("vmhd");
pub const SMHD: [u8; 4] = fourcc("smhd");
pub const GMHD: [u8; 4] = fourcc("gmhd");
pub const DINF: [u8; 4] = fourcc("dinf");
pub const DREF: [u8; 4] = fourcc("dref");
pub const STBL: [u8; 4] = fourcc("stbl");
pub const STSD: [u8; 4] = fourcc("stsd");
pub const STTS: [u8; 4] = fourcc("stts");
pub const STSS: [u8; 4] = fourcc("stss");
pub const STSH: [u8; 4] = fourcc("stsh"); // Shadow Sync Sample Box
pub const STSC: [u8; 4] = fourcc("stsc");
pub const STSZ: [u8; 4] = fourcc("stsz");
pub const STCO: [u8; 4] = fourcc("stco");
pub const CO64: [u8; 4] = fourcc("co64");
pub const CTTS: [u8; 4] = fourcc("ctts");
pub const SDTP: [u8; 4] = fourcc("sdtp"); // Independent and Disposable Samples Box
pub const SUBS: [u8; 4] = fourcc("subs"); // Sub-Sample Information Box
pub const SAIZ: [u8; 4] = fourcc("saiz"); // Sample Auxiliary Information Sizes Box
pub const SAIO: [u8; 4] = fourcc("saio"); // Sample Auxiliary Information Offsets Box
pub const MDAT: [u8; 4] = fourcc("mdat");
pub const FREE: [u8; 4] = fourcc("free");
pub const SKIP: [u8; 4] = fourcc("skip");
pub const WIDE: [u8; 4] = fourcc("wide");
pub const UDTA: [u8; 4] = fourcc("udta");
pub const TREF: [u8; 4] = fourcc("tref");
pub const PNOT: [u8; 4] = fourcc("pnot");
/// Round 89 — Track Load Settings atom (QTFF p. 48).
pub const LOAD: [u8; 4] = fourcc("load");

// Apple-specific / round-2 atoms.
pub const GAMA: [u8; 4] = fourcc("gama");
pub const PASP: [u8; 4] = fourcc("pasp");
pub const CLAP: [u8; 4] = fourcc("clap");
pub const COLR: [u8; 4] = fourcc("colr");
pub const TAPT: [u8; 4] = fourcc("tapt");
pub const CLEF: [u8; 4] = fourcc("clef");
pub const PROF: [u8; 4] = fourcc("prof");
pub const ENOF: [u8; 4] = fourcc("enof");
pub const CHAN: [u8; 4] = fourcc("chan");
pub const META: [u8; 4] = fourcc("meta");
pub const KEYS: [u8; 4] = fourcc("keys");
pub const ILST: [u8; 4] = fourcc("ilst");

/// Round 137 — Color Table atom (QTFF p. 35). Movie-level optional
/// leaf atom carrying a Macintosh 16-bit-per-channel palette of up to
/// 256 entries; QuickTime players use it when targeting indexed-color
/// displays. ISO BMFF does not define this atom.
pub const CTAB: [u8; 4] = fourcc("ctab");

/// Round 140 — Clipping atom (QTFF p. 43) and its single Clipping
/// Region child (QTFF p. 44). `clip` is the wrapper container that may
/// appear at either movie level (`moov/clip`) or track level
/// (`moov/trak/clip`); `crgn` is the leaf inside it carrying the
/// QuickDraw region (size + bounding-box rect + opaque scanline tail).
/// ISO BMFF does not define either atom.
pub const CLIP: [u8; 4] = fourcc("clip");
pub const CRGN: [u8; 4] = fourcc("crgn");

/// Round 144 — Track Matte atom (QTFF p. 44) and its single Compressed
/// Matte child (QTFF p. 45). `matt` is the wrapper container that lives
/// inside a `trak` atom (QTFF p. 41 Figure 2-6 places it alongside
/// `tkhd` / `mdia` / `edts` / `tref` / `load` / `imap` / `clip` /
/// `udta`); `kmat` is the leaf inside it carrying the FullBox header,
/// image description structure and compressed matte data. ISO BMFF
/// does not define either atom.
pub const MATT: [u8; 4] = fourcc("matt");
pub const KMAT: [u8; 4] = fourcc("kmat");

// Round-3: Reference-movie atoms (Apple QTFF "Reference Movies", p. 39+).
pub const RMRA: [u8; 4] = fourcc("rmra"); // reference movie list (top of moov)
pub const RMDA: [u8; 4] = fourcc("rmda"); // single reference movie descriptor
pub const RDRF: [u8; 4] = fourcc("rdrf"); // data reference (alias / URL)
pub const RMDR: [u8; 4] = fourcc("rmdr"); // data rate qualifier
pub const RMQU: [u8; 4] = fourcc("rmqu"); // quality qualifier
pub const RMCS: [u8; 4] = fourcc("rmcs"); // CPU speed qualifier
pub const RMVC: [u8; 4] = fourcc("rmvc"); // version-check qualifier
pub const RMCD: [u8; 4] = fourcc("rmcd"); // codec qualifier

// Round-3: Fragmented-MP4 atoms (ISO BMFF §8.16; we recognise + reject).
pub const MVEX: [u8; 4] = fourcc("mvex"); // movie-extends header inside moov
pub const TREX: [u8; 4] = fourcc("trex"); // track-extends defaults inside mvex
pub const MEHD: [u8; 4] = fourcc("mehd"); // movie-extends header
pub const MOOF: [u8; 4] = fourcc("moof"); // movie fragment (top-level)
pub const TRAF: [u8; 4] = fourcc("traf"); // track fragment inside moof

// Round-21: Movie-fragment random-access atoms (ISO BMFF §8.8.9–§8.8.11).
pub const MFRA: [u8; 4] = fourcc("mfra"); // movie-fragment random-access box (end-of-file)
pub const TFRA: [u8; 4] = fourcc("tfra"); // track-fragment random-access entries (per track)
pub const MFRO: [u8; 4] = fourcc("mfro"); // movie-fragment random-access offset (size_of_mfra)

// Round-114: Segment Index Box (ISO/IEC 14496-12 §8.16.3). File-level
// FullBox indexing one media stream's subsegments for DASH / CMAF.
pub const SIDX: [u8; 4] = fourcc("sidx");

// Round-125: Segment Type Box (ISO/IEC 14496-12 §8.16.2). File-level
// box that identifies a DASH / CMAF media segment — same on-disk shape
// as `ftyp`, distinguished by the box-type FourCC alone. `Quantity:
// Zero or more`; when present, "shall be the first box in a segment"
// (§8.16.2.1).
pub const STYP: [u8; 4] = fourcc("styp");

// Round-128: Producer Reference Time Box (ISO/IEC 14496-12 §8.16.5).
// File-level FullBox that records the writer's UTC wall-clock instant
// (NTP format, RFC 5905) at which the next movie fragment was produced,
// paired with the corresponding media time on a reference track. Used by
// live DASH-LL / CMAF / HLS-fMP4 encoders so consumers can keep
// production and consumption rates aligned over long sessions.
pub const PRFT: [u8; 4] = fourcc("prft");

// Round-3: Composition-shift-least-greatest atom.
pub const CSLG: [u8; 4] = fourcc("cslg");

// Round-80: Sample-group atoms (ISO/IEC 14496-12 §8.9).
pub const SBGP: [u8; 4] = fourcc("sbgp"); // Sample-to-Group Box
pub const SGPD: [u8; 4] = fourcc("sgpd"); // Sample-Group-Description Box

// Round-4: Data information sub-atoms.
pub const URL_: [u8; 4] = fourcc("url ");
pub const URN_: [u8; 4] = fourcc("urn ");
pub const ALIS: [u8; 4] = fourcc("alis");
pub const RSRC: [u8; 4] = fourcc("rsrc");

// Round-4: Base media (gmhd) sub-atoms — `gmin` (graphics-mode header)
// and `text` / `tmcd` per-MediaType extensions inside `gmhd`.
pub const GMIN: [u8; 4] = fourcc("gmin");
pub const TEXT: [u8; 4] = fourcc("text");
pub const TMCD: [u8; 4] = fourcc("tmcd");

/// Round 182 — User-Type Box (ISO/IEC 14496-12 §4.2 / §11.1). The
/// escape hatch for vendor-specific extensions: the box's body starts
/// with a 16-byte UUID identifying the vendor schema, followed by an
/// opaque payload. May appear at file scope (typical) or nested inside
/// any container that allows arbitrary boxes. QTFF does not define the
/// box at spec level but real-world `.mov` files routinely embed one
/// (Sony XAVC, GoPro GPMF, PIFF tfxd / tfrf, GoPro CAME, etc.).
pub const UUID: [u8; 4] = fourcc("uuid");

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parse_simple_8byte_header() {
        // size=16, type='moov', body=8 bytes
        let mut buf = Vec::new();
        buf.extend_from_slice(&16u32.to_be_bytes());
        buf.extend_from_slice(b"moov");
        buf.extend_from_slice(&[0u8; 8]);
        let mut c = Cursor::new(buf);
        let h = read_atom_header(&mut c).unwrap().unwrap();
        assert_eq!(h.fourcc, *b"moov");
        assert_eq!(h.total_size, Some(16));
        assert_eq!(h.header_len, 8);
        assert_eq!(h.payload_offset, 8);
        assert_eq!(h.payload_len(), Some(8));
    }

    #[test]
    fn parse_extended_64bit_size() {
        // size=1, type='mdat', extended_size=24, body=8 bytes
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(b"mdat");
        buf.extend_from_slice(&24u64.to_be_bytes());
        buf.extend_from_slice(&[0u8; 8]);
        let mut c = Cursor::new(buf);
        let h = read_atom_header(&mut c).unwrap().unwrap();
        assert_eq!(h.fourcc, *b"mdat");
        assert_eq!(h.total_size, Some(24));
        assert_eq!(h.header_len, 16);
        assert_eq!(h.payload_offset, 16);
        assert_eq!(h.payload_len(), Some(8));
    }

    #[test]
    fn parse_open_ended_size_zero() {
        // size=0 means "to end of file"
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(b"mdat");
        buf.extend_from_slice(&[0u8; 32]);
        let mut c = Cursor::new(buf);
        let h = read_atom_header(&mut c).unwrap().unwrap();
        assert_eq!(h.fourcc, *b"mdat");
        assert_eq!(h.total_size, None);
        assert_eq!(h.header_len, 8);
        assert_eq!(h.payload_offset, 8);
        assert!(h.payload_len().is_none());
    }

    #[test]
    fn clean_eof_yields_none() {
        let mut c = Cursor::new(Vec::<u8>::new());
        assert!(read_atom_header(&mut c).unwrap().is_none());
    }

    #[test]
    fn truncated_header_errors() {
        let mut c = Cursor::new(vec![0, 0, 0, 16]); // 4 bytes, missing fourcc
        assert!(read_atom_header(&mut c).is_err());
    }

    #[test]
    fn invalid_size_below_minimum_errors() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&7u32.to_be_bytes()); // < 8 bytes — invalid
        buf.extend_from_slice(b"moov");
        let mut c = Cursor::new(buf);
        assert!(read_atom_header(&mut c).is_err());
    }

    #[test]
    fn walk_children_visits_each_child_once() {
        // moov(16) wrapping: mvhd(8) + trak(8)
        let mut buf = Vec::new();
        // mvhd, empty 0-byte payload → size=8
        buf.extend_from_slice(&8u32.to_be_bytes());
        buf.extend_from_slice(b"mvhd");
        // trak, empty 0-byte payload → size=8
        buf.extend_from_slice(&8u32.to_be_bytes());
        buf.extend_from_slice(b"trak");
        let mut c = Cursor::new(buf);

        let mut seen: Vec<[u8; 4]> = Vec::new();
        walk_children(&mut c, Some(16), |_, h| {
            seen.push(h.fourcc);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, vec![*b"mvhd", *b"trak"]);
    }
}
