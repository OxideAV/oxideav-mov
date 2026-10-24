//! Codec configuration records carried as sample-description
//! extension atoms, and their mapping to / from the fleet's
//! `CodecParameters::extradata` convention.
//!
//! A QuickTime sample description (QTFF 2012-08-14 pp. 92 – 94 video,
//! pp. 100 – 102 / 185 – 188 sound) appends extension atoms after its
//! fixed fields. The decoder needs exactly one of them — the codec's
//! configuration record — not the whole framed atom list. The fleet
//! convention for that record (shared with the ISO BMFF demuxer) is:
//!
//! | atom   | where                      | extradata                                   |
//! |--------|----------------------------|---------------------------------------------|
//! | `avcC` | `avc1` / `avc3` video      | the AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1; the box is a plain `Box`, §5.4.2.1, so the body *is* the record) |
//! | `hvcC` | `hvc1` / `hev1` video      | the HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 §8.3.3.1) |
//! | `av1C` | `av01` video               | the AV1CodecConfigurationRecord (AV1-ISOBMFF binding §2.3) |
//! | `vpcC` | `vp08` / `vp09` video      | the box body *including* its FullBox version/flags word |
//! | `esds` | `mp4a` (direct or `wave`)  | the ES_Descriptor's DecoderSpecificInfo (ISO/IEC 14496-1 §7.2.6.7) |
//! | `dOps` | `Opus`                     | `OpusHead` magic followed by the box body |
//! | `dfLa` | `fLaC`                     | the box body after its FullBox version/flags word |
//! | `alac` | `alac` (direct or `wave`)  | the magic cookie after the FullBox version/flags word (ALACMagicCookieDescription) |
//! | `dac3` / `dec3` | `ac-3` / `ec-3` (direct or `wave`) | the box body verbatim |
//!
//! [`config_record_from_extensions`] is the demux direction and
//! [`config_atom_for_format`] the exact inverse used by the muxer.

/// The 8-byte magic the fleet's Opus decoder expects in front of a
/// `dOps` body.
const OPUS_HEAD_MAGIC: &[u8; 8] = b"OpusHead";

/// Walk a buffer of framed `[size:u32][type:4][payload]` atoms,
/// returning `(fourcc, payload)` pairs. A size of 0 extends to the end
/// of the buffer; malformed trailing bytes stop the walk.
fn atoms(buf: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= buf.len() {
        let size = u32::from_be_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
        let mut fc = [0u8; 4];
        fc.copy_from_slice(&buf[p + 4..p + 8]);
        let end = if size == 0 { buf.len() } else { p + size };
        if size != 0 && (size < 8 || end > buf.len()) {
            break;
        }
        out.push((fc, &buf[p + 8..end]));
        p = end;
    }
    out
}

/// Turn one recognised configuration atom into extradata, per the
/// convention in the module docs. `None` for atoms that are not codec
/// configuration records (or are too short to be one).
fn record_from_atom(fourcc: &[u8; 4], payload: &[u8]) -> Option<Vec<u8>> {
    match fourcc {
        b"avcC" | b"hvcC" | b"av1C" | b"vpcC" | b"dac3" | b"dec3" => Some(payload.to_vec()),
        b"dOps" => {
            let mut v = Vec::with_capacity(8 + payload.len());
            v.extend_from_slice(OPUS_HEAD_MAGIC);
            v.extend_from_slice(payload);
            Some(v)
        }
        b"dfLa" | b"alac" => payload.get(4..).map(<[u8]>::to_vec),
        b"esds" => crate::track::esds_decoder_specific_info(payload.get(4..)?),
        _ => None,
    }
}

/// Extract the codec configuration record from a sample description's
/// extension-atom blob (`SampleDescription::extra`), searching the
/// top-level atoms first and then the children of a `wave`
/// siDecompressionParam atom (QTFF 2012-08-14 p. 185, where QuickTime
/// sound descriptions carry `esds`, `alac` and `dac3`). Returns `None`
/// when no configuration atom is present.
pub fn config_record_from_extensions(extra: &[u8]) -> Option<Vec<u8>> {
    let top = atoms(extra);
    if let Some(r) = top.iter().find_map(|(fc, p)| record_from_atom(fc, p)) {
        return Some(r);
    }
    top.iter()
        .filter(|(fc, _)| fc == b"wave")
        .flat_map(|(_, p)| atoms(p))
        .find_map(|(fc, p)| record_from_atom(&fc, p))
}

/// Frame one atom.
fn frame(fourcc: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let len: usize = 8 + parts.iter().map(|p| p.len()).sum::<usize>();
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.extend_from_slice(fourcc);
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

/// Minimal ISO/IEC 14496-1 §7.2.6.5 ES_Descriptor wrapping an MPEG-4
/// Audio DecoderSpecificInfo: ES_ID 0, no optional fields, a
/// DecoderConfigDescriptor (§7.2.6.6, objectTypeIndication `0x40`
/// = ISO/IEC 14496-3 audio, streamType 5 = audio, upStream 0,
/// reserved 1) and an SLConfigDescriptor (§7.3.2.3.1) with
/// predefined = 2 (the value §7.3.2.3.2 reserves for MP4 files).
fn audio_es_descriptor(dsi: &[u8]) -> Option<Vec<u8>> {
    fn put(out: &mut Vec<u8>, tag: u8, body: &[u8]) -> Option<()> {
        // §8.3.3 expandable size: 7-bit groups, high bit = more.
        let n = body.len();
        if n >= 1 << 28 {
            return None;
        }
        out.push(tag);
        out.extend_from_slice(&[
            0x80 | ((n >> 21) & 0x7F) as u8,
            0x80 | ((n >> 14) & 0x7F) as u8,
            0x80 | ((n >> 7) & 0x7F) as u8,
            (n & 0x7F) as u8,
        ]);
        out.extend_from_slice(body);
        Some(())
    }
    let mut dcd = vec![0x40, (0x05 << 2) | 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    put(&mut dcd, 0x05, dsi)?;
    let mut es = vec![0x00, 0x00, 0x00]; // ES_ID = 0, flags = 0, streamPriority = 0
    put(&mut es, 0x04, &dcd)?;
    put(&mut es, 0x06, &[0x02])?;
    let mut out = Vec::new();
    put(&mut out, 0x03, &es)?;
    Some(out)
}

/// `true` when `buf` is entirely a sequence of well-formed framed
/// atoms whose types are printable — i.e. already a sample-description
/// extension blob (as a caller of the direct muxer API would pass it)
/// rather than a bare configuration record.
fn is_framed_atom_list(buf: &[u8]) -> bool {
    let mut p = 0usize;
    while p < buf.len() {
        if p + 8 > buf.len() {
            return false;
        }
        let size = u32::from_be_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
        if size < 8 || p + size > buf.len() {
            return false;
        }
        if !buf[p + 4..p + 8].iter().all(|b| (0x20..0x7F).contains(b)) {
            return false;
        }
        p += size;
    }
    !buf.is_empty()
}

/// Inverse of [`config_record_from_extensions`]: frame `extradata` (in
/// the fleet convention above) as the configuration atom the given
/// sample-description `format` carries, ready to append to the
/// description's extension area. Extradata that is already a framed
/// atom list, or a format with no configuration atom in the table, is
/// returned verbatim; empty extradata yields an empty blob.
pub fn config_atom_for_format(format: &[u8; 4], extradata: &[u8]) -> Vec<u8> {
    if extradata.is_empty() || is_framed_atom_list(extradata) {
        return extradata.to_vec();
    }
    const VF0: &[u8] = &[0, 0, 0, 0];
    let framed = match format {
        b"avc1" | b"avc3" => Some(frame(b"avcC", &[extradata])),
        b"hvc1" | b"hev1" => Some(frame(b"hvcC", &[extradata])),
        b"av01" => Some(frame(b"av1C", &[extradata])),
        b"vp08" | b"vp09" => Some(frame(b"vpcC", &[extradata])),
        b"ac-3" => Some(frame(b"dac3", &[extradata])),
        b"ec-3" => Some(frame(b"dec3", &[extradata])),
        b"fLaC" => Some(frame(b"dfLa", &[VF0, extradata])),
        b"alac" => Some(frame(b"alac", &[VF0, extradata])),
        b"Opus" => Some(frame(
            b"dOps",
            &[extradata.strip_prefix(OPUS_HEAD_MAGIC).unwrap_or(extradata)],
        )),
        b"mp4a" => audio_es_descriptor(extradata).map(|es| frame(b"esds", &[VF0, &es])),
        _ => None,
    };
    framed.unwrap_or_else(|| extradata.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(fc: &[u8; 4], body: &[u8]) -> Vec<u8> {
        frame(fc, &[body])
    }

    /// A real AVCDecoderConfigurationRecord shape (ISO/IEC 14496-15
    /// §5.3.3.1): version 1, High profile, one SPS, one PPS.
    const AVCC: &[u8] = &[
        0x01, 0x64, 0x00, 0x0D, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x64, 0x00, 0x0D, 0x01, 0x00, 0x03,
        0x68, 0xEB, 0xE3,
    ];

    #[test]
    fn avcc_record_is_the_box_body() {
        let mut extra = atom(b"colr", b"nclx\x00\x01\x00\x01\x00\x01");
        extra.extend(atom(b"avcC", AVCC));
        extra.extend(atom(b"pasp", &[0, 0, 0, 1, 0, 0, 0, 1]));
        assert_eq!(config_record_from_extensions(&extra).unwrap(), AVCC);
    }

    #[test]
    fn hvcc_av1c_vpcc_dac3_dec3_bodies_verbatim() {
        for fc in [b"hvcC", b"av1C", b"vpcC", b"dac3", b"dec3"] {
            let body = [1u8, 2, 3, 4, 5, 6];
            let extra = atom(fc, &body);
            assert_eq!(
                config_record_from_extensions(&extra).unwrap(),
                body,
                "{fc:?}"
            );
        }
    }

    #[test]
    fn dops_gets_opushead_magic() {
        let body = [0u8, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0];
        let r = config_record_from_extensions(&atom(b"dOps", &body)).unwrap();
        assert_eq!(&r[..8], b"OpusHead");
        assert_eq!(&r[8..], body);
    }

    #[test]
    fn dfla_and_alac_drop_fullbox_word() {
        let body = [0u8, 0, 0, 0, 0x80, 0, 0, 0x22, 9, 9];
        assert_eq!(
            config_record_from_extensions(&atom(b"dfLa", &body)).unwrap(),
            &body[4..]
        );
        assert_eq!(
            config_record_from_extensions(&atom(b"alac", &body)).unwrap(),
            &body[4..]
        );
        assert!(config_record_from_extensions(&atom(b"dfLa", &[0, 0])).is_none());
    }

    #[test]
    fn wave_children_are_searched() {
        // QuickTime ALAC layout (ALACMagicCookieDescription
        // "Compatibility"): wave { frma 'alac', alac FullBox, term }.
        let cookie = [0u8, 0, 0x10, 0, 0, 0x10, 0x28, 0x0A, 0x0E, 0x02];
        let mut wave = atom(b"frma", b"alac");
        wave.extend(atom(b"alac", &[&[0u8, 0, 0, 0][..], &cookie].concat()));
        wave.extend([0, 0, 0, 8, 0, 0, 0, 0]);
        let extra = atom(b"wave", &wave);
        assert_eq!(config_record_from_extensions(&extra).unwrap(), cookie);
        // ... and dac3 inside wave (ffmpeg's AC-3-in-MOV layout).
        let mut wave = atom(b"frma", b"ac-3");
        wave.extend(atom(b"dac3", &[0x10, 0x3D, 0xE0]));
        let extra = atom(b"wave", &wave);
        assert_eq!(
            config_record_from_extensions(&extra).unwrap(),
            [0x10, 0x3D, 0xE0]
        );
    }

    #[test]
    fn esds_yields_dsi_and_round_trips() {
        let asc = [0x12u8, 0x10];
        let es = audio_es_descriptor(&asc).unwrap();
        let framed = config_atom_for_format(b"mp4a", &asc);
        assert_eq!(framed, atom(b"esds", &[&[0u8, 0, 0, 0][..], &es].concat()));
        assert_eq!(config_record_from_extensions(&framed).unwrap(), asc);
    }

    #[test]
    fn no_config_atom_is_none() {
        let extra = atom(b"colr", b"nclx\x00\x01\x00\x01\x00\x01");
        assert!(config_record_from_extensions(&extra).is_none());
        assert!(config_record_from_extensions(&[]).is_none());
        assert!(config_record_from_extensions(&[1, 2, 3]).is_none());
    }

    #[test]
    fn mux_side_is_the_exact_inverse() {
        let cases: &[(&[u8; 4], Vec<u8>)] = &[
            (b"avc1", AVCC.to_vec()),
            (b"hvc1", vec![1, 2, 3, 4]),
            (b"av01", vec![0x81, 0x00, 0x0C, 0x00]),
            (b"vp09", vec![1, 0, 0, 0, 0, 10, 0x80, 2, 2, 2, 0, 0]),
            (b"ac-3", vec![0x10, 0x3D, 0xE0]),
            (b"ec-3", vec![0x10, 0x3D, 0xE0, 0x00]),
            (b"fLaC", vec![0x80, 0, 0, 0x22, 1, 2]),
            (b"alac", vec![0, 0, 0x10, 0, 0, 0x10]),
            (b"Opus", [&b"OpusHead"[..], &[0, 2, 1, 0x38]].concat()),
            (b"mp4a", vec![0x11, 0x90]),
        ];
        for (fmt, rec) in cases {
            let framed = config_atom_for_format(fmt, rec);
            assert!(is_framed_atom_list(&framed), "{fmt:?}");
            assert_eq!(
                &config_record_from_extensions(&framed).unwrap(),
                rec,
                "{fmt:?}"
            );
        }
    }

    #[test]
    fn mux_side_passes_framed_or_unknown_through() {
        let framed = atom(b"avcC", AVCC);
        assert_eq!(config_atom_for_format(b"avc1", &framed), framed);
        assert_eq!(config_atom_for_format(b"jpeg", &[1, 2, 3]), [1, 2, 3]);
        assert!(config_atom_for_format(b"avc1", &[]).is_empty());
    }
}
