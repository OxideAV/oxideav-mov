//! The audio facts a sound sample description states, read the way
//! its format code says they must be read:
//!
//! * [`AudioSpecificConfig`] — the MPEG-4 Audio decoder configuration
//!   an `mp4a` description's `esds` carries (ISO/IEC 14496-3:2019
//!   §1.6.2.1 Table 1.19, §4.4.1 Tables 4.1 / 4.2). Its sampling
//!   frequency, channel configuration and SBR / PS signalling are the
//!   stream's real output rate and channel count; the sound
//!   description's own `numberOfChannels` / `sampleRate` fields are
//!   frequently the AAC core's (half) rate or a mono count for a
//!   parametric-stereo stream.
//! * [`EsDecoderConfig`] — the ISO/IEC 14496-1 §7.2.6.6
//!   DecoderConfigDescriptor around it (object type, bit rates).
//! * [`PcmLayout`] — the sample layout of an uncompressed sound
//!   format (QTFF 2012-08-14 Table 4-7 p. 176, version 1 fields
//!   p. 179, version 2 LPCM flags p. 183).

use crate::track::SampleDescription;

/// MSB-first bit reader over a byte slice (the `bslbf` / `uimsbf`
/// mnemonics of ISO/IEC 14496-3).
struct Bits<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        (self.buf.len() * 8).saturating_sub(self.pos)
    }

    fn read(&mut self, n: u32) -> Option<u32> {
        debug_assert!(n <= 32);
        if self.remaining() < n as usize {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.buf[self.pos / 8];
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | u32::from(bit);
            self.pos += 1;
        }
        Some(v)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        if self.remaining() < n {
            return None;
        }
        self.pos += n;
        Some(())
    }

    /// `byte_alignment()` relative to the start of the buffer (Table
    /// 4.2 Note 1: relative to the start of the AudioSpecificConfig).
    fn align(&mut self) -> Option<()> {
        let pad = (8 - self.pos % 8) % 8;
        self.skip(pad)
    }
}

/// ISO/IEC 14496-3:2019 Table 1.22, sampling frequency by
/// `samplingFrequencyIndex` (`0xd` / `0xe` reserved, `0xf` escape).
const SAMPLING_FREQUENCIES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// Table 1.20 `GetAudioObjectType()`.
fn audio_object_type(r: &mut Bits) -> Option<u8> {
    let aot = r.read(5)? as u8;
    if aot == 31 {
        Some(32 + r.read(6)? as u8)
    } else {
        Some(aot)
    }
}

/// `samplingFrequencyIndex` + the 24-bit escape. `None` for a
/// reserved index (the field is still consumed).
fn read_sampling_frequency(r: &mut Bits) -> Option<Option<u32>> {
    let idx = r.read(4)?;
    Some(if idx == 0xF {
        Some(r.read(24)?)
    } else {
        SAMPLING_FREQUENCIES.get(idx as usize).copied()
    })
}

/// Output-channel count of `channelConfiguration` per ISO/IEC
/// 14496-3:2019 Table 1.23 (`None` for 0 — defined by the
/// object-type-specific config — and for the reserved values).
pub fn channel_configuration_channels(channel_configuration: u8) -> Option<u16> {
    Some(match channel_configuration {
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 5,
        6 => 6,
        7 => 8,
        11 => 7,
        12 => 8,
        13 => 24,
        14 => 8,
        _ => return None,
    })
}

/// Speaker-element summary of a `program_config_element()` (ISO/IEC
/// 14496-3:2019 §4.4.1.1 Table 4.2) — the channel configuration of
/// an AudioSpecificConfig whose `channelConfiguration` is 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProgramConfig {
    /// Front speaker channels (one per single-channel element, two per
    /// channel-pair element).
    pub front_channels: u16,
    /// Side speaker channels.
    pub side_channels: u16,
    /// Back speaker channels.
    pub back_channels: u16,
    /// Low-frequency-enhancement channels.
    pub lfe_channels: u16,
}

impl ProgramConfig {
    /// Loudspeaker channels the program feeds (front + side + back +
    /// LFE; coupling channels and data elements produce no output).
    pub fn channel_count(&self) -> u16 {
        self.front_channels + self.side_channels + self.back_channels + self.lfe_channels
    }

    fn parse(r: &mut Bits) -> Option<Self> {
        r.skip(4 + 2 + 4)?; // element_instance_tag, object_type, sampling_frequency_index
        let front = r.read(4)?;
        let side = r.read(4)?;
        let back = r.read(4)?;
        let lfe = r.read(2)?;
        let assoc = r.read(3)?;
        let cc = r.read(4)?;
        if r.read(1)? == 1 {
            r.skip(4)?; // mono_mixdown_element_number
        }
        if r.read(1)? == 1 {
            r.skip(4)?; // stereo_mixdown_element_number
        }
        if r.read(1)? == 1 {
            r.skip(3)?; // matrix_mixdown_idx, pseudo_surround_enable
        }
        let mut elements = |n: u32| -> Option<u16> {
            let mut ch = 0u16;
            for _ in 0..n {
                let is_cpe = r.read(1)?;
                r.skip(4)?; // *_element_tag_select
                ch += if is_cpe == 1 { 2 } else { 1 };
            }
            Some(ch)
        };
        let front_channels = elements(front)?;
        let side_channels = elements(side)?;
        let back_channels = elements(back)?;
        r.skip(4 * lfe as usize)?;
        r.skip(4 * assoc as usize)?;
        r.skip(5 * cc as usize)?; // cc_element_is_ind_sw + valid_cc_element_tag_select
        r.align()?;
        let comment = r.read(8)?;
        r.skip(8 * comment as usize)?;
        Some(Self {
            front_channels,
            side_channels,
            back_channels,
            lfe_channels: lfe as u16,
        })
    }
}

/// The fields of an MPEG-4 Audio `AudioSpecificConfig()` (ISO/IEC
/// 14496-3:2019 §1.6.2.1 Table 1.19) that decide the decoded output:
/// object type, sampling frequency, channel configuration and the
/// SBR / PS presence signalling (explicit hierarchical, explicit
/// backward-compatible through the `0x2b7` / `0x548` sync extensions,
/// or left implicit).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AudioSpecificConfig {
    /// The audio object type of the underlying coded stream (Table
    /// 1.1; 2 = AAC LC). For the hierarchical SBR / PS signalling
    /// (first object type 5 or 29) this is the second, underlying
    /// object type.
    pub audio_object_type: u8,
    /// `extensionAudioObjectType`: 5 (SBR) when SBR is signalled —
    /// hierarchically or through the backward-compatible extension —
    /// 22 for the ER BSAC extension, 0 otherwise.
    pub extension_audio_object_type: u8,
    /// Sampling frequency of the underlying stream in Hz (Table 1.22
    /// or the 24-bit escape). `None` for a reserved index.
    pub sampling_frequency: Option<u32>,
    /// `channelConfiguration` (Table 1.23).
    pub channel_configuration: u8,
    /// `sbrPresentFlag`: `Some(true)` / `Some(false)` when signalled
    /// explicitly, `None` for the spec's `-1` (no explicit signalling;
    /// SBR may still be present implicitly in the payload).
    pub sbr_present: Option<bool>,
    /// `psPresentFlag`, with the same `None` = `-1` convention.
    pub ps_present: Option<bool>,
    /// `extensionSamplingFrequency` — the SBR output rate — when SBR
    /// is signalled present.
    pub extension_sampling_frequency: Option<u32>,
    /// The `program_config_element()` of a GA config whose
    /// `channelConfiguration` is 0.
    pub program_config: Option<ProgramConfig>,
}

/// GA object types that carry a `GASpecificConfig()` (Table 1.19
/// first switch).
fn is_ga(aot: u8) -> bool {
    matches!(aot, 1..=4 | 6 | 7 | 17 | 19..=23)
}

/// Error-resilient object types that carry `epConfig` (Table 1.19
/// second switch).
fn is_er(aot: u8) -> bool {
    matches!(aot, 17 | 19..=27 | 39)
}

impl AudioSpecificConfig {
    /// Parse an AudioSpecificConfig (the DecoderSpecificInfo payload of
    /// an `esds` whose objectTypeIndication is `0x40`). `None` when the
    /// leading fields do not fit. The backward-compatible SBR / PS
    /// extension at the end is read for the GA object types, whose
    /// specific config this parser walks; for the others it is left
    /// unread and the flags stay at their `-1` (`None`) defaults.
    pub fn parse(asc: &[u8]) -> Option<Self> {
        let mut r = Bits::new(asc);
        let mut aot = audio_object_type(&mut r)?;
        let sampling_frequency = read_sampling_frequency(&mut r)?;
        let channel_configuration = r.read(4)? as u8;
        let mut out = Self {
            audio_object_type: aot,
            sampling_frequency,
            channel_configuration,
            ..Self::default()
        };
        if aot == 5 || aot == 29 {
            out.extension_audio_object_type = 5;
            out.sbr_present = Some(true);
            if aot == 29 {
                out.ps_present = Some(true);
            }
            out.extension_sampling_frequency = read_sampling_frequency(&mut r)?;
            aot = audio_object_type(&mut r)?;
            out.audio_object_type = aot;
            if aot == 22 {
                r.skip(4)?; // extensionChannelConfiguration
            }
        }
        if !is_ga(aot) {
            return Some(out);
        }
        // GASpecificConfig (Table 4.1).
        let walked = (|| -> Option<()> {
            r.skip(1)?; // frameLengthFlag
            if r.read(1)? == 1 {
                r.skip(14)?; // coreCoderDelay
            }
            let extension_flag = r.read(1)?;
            if channel_configuration == 0 {
                out.program_config = Some(ProgramConfig::parse(&mut r)?);
            }
            if aot == 6 || aot == 20 {
                r.skip(3)?; // layerNr
            }
            if extension_flag == 1 {
                if aot == 22 {
                    r.skip(5 + 11)?; // numOfSubFrame, layer_length
                }
                if matches!(aot, 17 | 19 | 20 | 23) {
                    r.skip(3)?; // the three resilience flags
                }
                r.skip(1)?; // extensionFlag3
            }
            if is_er(aot) {
                let ep_config = r.read(2)?;
                if ep_config == 2 || ep_config == 3 {
                    // ErrorProtectionSpecificConfig() (§1.8.2.1) is
                    // not walked: stop before the trailing extension.
                    return None;
                }
            }
            Some(())
        })();
        if walked.is_none() {
            return Some(out);
        }
        // Backward-compatible explicit SBR / PS signalling.
        if r.remaining() >= 11
            && r.read(11)? == 0x2B7
            && out.extension_audio_object_type != 5
            && r.remaining() >= 5
        {
            let ext = audio_object_type(&mut r)?;
            if ext == 5 {
                out.extension_audio_object_type = 5;
                let sbr = r.read(1)? == 1;
                out.sbr_present = Some(sbr);
                if sbr {
                    out.extension_sampling_frequency = read_sampling_frequency(&mut r)?;
                }
                if r.remaining() >= 12 && r.read(11)? == 0x548 {
                    out.ps_present = Some(r.read(1)? == 1);
                }
            } else if ext == 22 {
                out.extension_audio_object_type = 22;
                let sbr = r.read(1)? == 1;
                out.sbr_present = Some(sbr);
                if sbr {
                    out.extension_sampling_frequency = read_sampling_frequency(&mut r)?;
                }
            }
        }
        Some(out)
    }

    /// The decoded output rate: the SBR `extensionSamplingFrequency`
    /// when SBR is signalled present, else the underlying sampling
    /// frequency. With implicit signalling (`sbr_present == None`) a
    /// decoder may still find SBR data and double the rate; the
    /// configuration cannot tell.
    pub fn output_sample_rate(&self) -> Option<u32> {
        if self.sbr_present == Some(true) {
            self.extension_sampling_frequency
                .or(self.sampling_frequency)
        } else {
            self.sampling_frequency
        }
    }

    /// Channels of the underlying coded stream: Table 1.23 for a
    /// non-zero `channelConfiguration`, the program config's
    /// loudspeaker channels for 0.
    pub fn coded_channels(&self) -> Option<u16> {
        if self.channel_configuration == 0 {
            self.program_config.map(|p| p.channel_count())
        } else {
            channel_configuration_channels(self.channel_configuration)
        }
    }

    /// `true` when parametric stereo turns the single channel element
    /// into two output channels (§1.6.6.2 – §1.6.6.4, Table 1.33):
    /// PS signalled present, or SBR signalled present with the PS
    /// flag left implicit — "the HE AAC v2 Profile decoder shall
    /// assume that PS data is available and decide the number of
    /// output channels to be two for a single channel element
    /// containing SBR data" (§1.6.6.3). An explicit
    /// `psPresentFlag = 0` keeps the original configuration.
    pub fn parametric_stereo_upmix(&self) -> bool {
        self.channel_configuration == 1
            && match self.ps_present {
                Some(ps) => ps,
                None => self.sbr_present == Some(true),
            }
    }

    /// The decoded output channel count: [`Self::coded_channels`],
    /// two for a parametric-stereo single channel element.
    pub fn output_channels(&self) -> Option<u16> {
        if self.parametric_stereo_upmix() {
            Some(2)
        } else {
            self.coded_channels()
        }
    }
}

/// The ISO/IEC 14496-1 §7.2.6.6 DecoderConfigDescriptor of an `esds`
/// elementary stream descriptor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EsDecoderConfig {
    /// `objectTypeIndication` (§7.2.6.6.2 Table 5; `0x40` = ISO/IEC
    /// 14496-3 audio, `0x66`–`0x68` = ISO/IEC 13818-7 AAC profiles).
    pub object_type_indication: u8,
    /// `streamType` (Table 6; 5 = audio).
    pub stream_type: u8,
    /// `bufferSizeDB` in bytes.
    pub buffer_size_db: u32,
    /// `maxBitrate` in bits per second.
    pub max_bitrate: u32,
    /// `avgBitrate` in bits per second; 0 for a variable bit rate.
    pub avg_bitrate: u32,
    /// The DecoderSpecificInfo payload (tag `0x05`), when present.
    pub decoder_specific_info: Option<Vec<u8>>,
}

/// §8.3.3 expandable-class header: `(tag, body offset, body length)`.
fn descriptor_header(buf: &[u8], pos: usize) -> Option<(u8, usize, usize)> {
    let tag = *buf.get(pos)?;
    let mut len = 0usize;
    let mut i = pos + 1;
    for _ in 0..4 {
        let b = *buf.get(i)?;
        i += 1;
        len = (len << 7) | usize::from(b & 0x7F);
        if b & 0x80 == 0 {
            return Some((tag, i, len));
        }
    }
    None
}

impl EsDecoderConfig {
    /// Walk an `ES_Descriptor` (tag `0x03`, §7.2.6.5 — skipping the
    /// optional dependsOn / URL / OCR fields its flags announce) to
    /// its DecoderConfigDescriptor (tag `0x04`). `descriptor` is the
    /// `esds` body after its version word. `None` when the structure
    /// does not parse.
    pub fn parse(descriptor: &[u8]) -> Option<Self> {
        let (tag, body, len) = descriptor_header(descriptor, 0)?;
        if tag != 0x03 {
            return None;
        }
        let end = body.checked_add(len)?.min(descriptor.len());
        let flags = *descriptor.get(body + 2)?;
        let mut pos = body + 3;
        if flags & 0x80 != 0 {
            pos += 2; // dependsOn_ES_ID
        }
        if flags & 0x40 != 0 {
            pos += 1 + usize::from(*descriptor.get(pos)?); // URLlength + URLstring
        }
        if flags & 0x20 != 0 {
            pos += 2; // OCR_ES_Id
        }
        while pos < end {
            let (t, b, l) = descriptor_header(descriptor, pos)?;
            if t == 0x04 {
                let f = descriptor.get(b..b.checked_add(13)?)?;
                let be32 = |o: usize| u32::from_be_bytes([f[o], f[o + 1], f[o + 2], f[o + 3]]);
                let mut out = Self {
                    object_type_indication: f[0],
                    stream_type: f[1] >> 2,
                    buffer_size_db: u32::from_be_bytes([0, f[2], f[3], f[4]]),
                    max_bitrate: be32(5),
                    avg_bitrate: be32(9),
                    decoder_specific_info: None,
                };
                let dcd_end = b.checked_add(l)?.min(end);
                let mut p = b + 13;
                while p < dcd_end {
                    let (t2, b2, l2) = descriptor_header(descriptor, p)?;
                    if t2 == 0x05 {
                        out.decoder_specific_info =
                            descriptor.get(b2..b2.checked_add(l2)?).map(<[u8]>::to_vec);
                        break;
                    }
                    p = b2.checked_add(l2)?;
                }
                return Some(out);
            }
            pos = b.checked_add(l)?;
        }
        None
    }

    /// The parsed AudioSpecificConfig, for an ISO/IEC 14496-3 stream
    /// (`objectTypeIndication` `0x40`) carrying one.
    pub fn audio_specific_config(&self) -> Option<AudioSpecificConfig> {
        if self.object_type_indication != 0x40 {
            return None;
        }
        AudioSpecificConfig::parse(self.decoder_specific_info.as_deref()?)
    }
}

/// Sample layout of uncompressed sound media.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PcmLayout {
    /// Bits per sample of one channel, all significant (the layouts
    /// with padding bits are not described).
    pub bits: u32,
    /// IEEE-754 floating point (32 / 64 bits).
    pub float: bool,
    /// Two's-complement signed integers (offset binary when `false`;
    /// ignored for float).
    pub signed: bool,
    /// Big-endian byte order (meaningless for 8-bit samples).
    pub big_endian: bool,
}

impl PcmLayout {
    /// A layout from its parts.
    pub fn new(bits: u32, float: bool, signed: bool, big_endian: bool) -> Self {
        Self {
            bits,
            float,
            signed,
            big_endian,
        }
    }

    /// The fleet codec id naming this layout (`pcm_s16be`,
    /// `pcm_f32le`, `pcm_u8`, …). `None` for widths no id exists for.
    pub fn codec_id(&self) -> Option<&'static str> {
        let be = self.big_endian;
        Some(match (self.float, self.signed, self.bits) {
            (true, _, 32) => pick(be, "pcm_f32be", "pcm_f32le"),
            (true, _, 64) => pick(be, "pcm_f64be", "pcm_f64le"),
            (true, _, _) => return None,
            (false, false, 8) => "pcm_u8",
            (false, true, 8) => "pcm_s8",
            (false, true, 16) => pick(be, "pcm_s16be", "pcm_s16le"),
            (false, true, 24) => pick(be, "pcm_s24be", "pcm_s24le"),
            (false, true, 32) => pick(be, "pcm_s32be", "pcm_s32le"),
            (false, true, 64) => pick(be, "pcm_s64be", "pcm_s64le"),
            (false, false, 16) => pick(be, "pcm_u16be", "pcm_u16le"),
            (false, false, 24) => pick(be, "pcm_u24be", "pcm_u24le"),
            (false, false, 32) => pick(be, "pcm_u32be", "pcm_u32le"),
            _ => return None,
        })
    }

    /// Inverse of [`Self::codec_id`].
    pub fn from_codec_id(id: &str) -> Option<Self> {
        let rest = id.strip_prefix("pcm_")?;
        let (kind, rest) = rest.split_at(1);
        let (bits, big_endian) = if let Some(b) = rest.strip_suffix("be") {
            (b, true)
        } else if let Some(b) = rest.strip_suffix("le") {
            (b, false)
        } else {
            (rest, true)
        };
        let bits: u32 = bits.parse().ok()?;
        let layout = match kind {
            "f" => Self::new(bits, true, true, big_endian),
            "s" => Self::new(bits, false, true, big_endian),
            "u" => Self::new(bits, false, false, big_endian),
            _ => return None,
        };
        // Only the spellings `codec_id` produces (8-bit ids carry no
        // byte order; wider ones must).
        (layout.codec_id() == Some(id)).then_some(layout)
    }
}

fn pick(be: bool, b: &'static str, l: &'static str) -> &'static str {
    if be { b } else { l }
}

/// The 16-bit value of a `wave` child `enda` atom: non-zero when the
/// `in24` / `in32` / `fl32` / `fl64` samples are little-endian.
///
/// QTFF 2012-08-14 pp. 187–188 says only that the byte order of
/// these formats depends on "codec-specific data associated with the
/// sample description". The `enda` child is the one such datum; its
/// meaning is pinned black-box (QuickTime files written with
/// `ffmpeg -c:a pcm_s24le` / `pcm_s24be` carry `enda` = 1 / 0 and
/// `ffprobe` reports little / big-endian respectively).
fn little_endian_flag(desc: &SampleDescription) -> Option<bool> {
    let wave = desc.si_decompression_param.as_ref()?;
    let enda = wave.child(b"enda")?;
    let v = u16::from_be_bytes([*enda.first()?, *enda.get(1)?]);
    Some(v != 0)
}

/// Sample width of a `raw ` / `twos` / `sowt` description: the
/// version-1 `bytesPerPacket` (QTFF p. 179: "for uncompressed audio,
/// the number of bytes in a sample for a single channel. This
/// replaces the older sampleSize field, which is set to 16"), the
/// version-2 constant bits per channel, else the version-0 sample
/// size.
fn integer_width(desc: &SampleDescription) -> u32 {
    if let Some(v2) = desc.sound_v2
        && v2.const_bits_per_channel != 0
    {
        return v2.const_bits_per_channel;
    }
    if let Some(v1) = desc.sound_v1
        && v1.bytes_per_packet != 0
        && desc.audio_compression_id == 0
    {
        return v1.bytes_per_packet.saturating_mul(8);
    }
    u32::from(desc.bits_per_sample)
}

/// The sample layout an uncompressed sound description states (QTFF
/// 2012-08-14 Table 4-7 p. 176 and pp. 186–188, version-2 LPCM flags
/// p. 183):
///
/// * `raw ` — 8-bit offset binary;
/// * `twos` — two's complement, big-endian ("16-bit big-endian
///   format"), 8 bits or the description's width;
/// * `sowt` — two's complement, little-endian;
/// * `in24` / `in32` / `fl32` / `fl64` — their named width, big-endian
///   unless the `wave` `enda` datum says little-endian;
/// * `lpcm` (version 2) — `constBitsPerChannel` and the
///   `formatSpecificFlags` float / big-endian / signed bits;
/// * the unspecified `0x00000000` / `NONE` codes — `raw ` or `twos` by
///   the sample size (Table 4-7).
///
/// `None` for compressed formats, a non-interleaved or fixed-point
/// `lpcm`, an `lpcm` whose samples sit in wider containers (padding
/// bits), and unspecified widths.
pub fn pcm_layout(desc: &SampleDescription) -> Option<PcmLayout> {
    let le_flag = || little_endian_flag(desc).unwrap_or(false);
    let layout = match &desc.format {
        b"raw " => PcmLayout::new(8, false, false, true),
        b"\0\0\0\0" | b"NONE" => match desc.bits_per_sample {
            8 => PcmLayout::new(8, false, false, true),
            16 => PcmLayout::new(16, false, true, true),
            _ => return None,
        },
        b"twos" => PcmLayout::new(integer_width(desc), false, true, true),
        b"sowt" => PcmLayout::new(integer_width(desc), false, true, false),
        b"in24" => PcmLayout::new(24, false, true, !le_flag()),
        b"in32" => PcmLayout::new(32, false, true, !le_flag()),
        b"fl32" => PcmLayout::new(32, true, true, !le_flag()),
        b"fl64" => PcmLayout::new(64, true, true, !le_flag()),
        b"lpcm" => {
            let v2 = desc.sound_v2?;
            let f = v2.format_specific_flags;
            if f.is_non_interleaved() || f.sample_fraction() != 0 {
                return None;
            }
            let bits = v2.const_bits_per_channel;
            // Samples in a wider container carry padding bits whose
            // placement (aligned high / low) no codec id names.
            if v2.const_bytes_per_audio_packet != 0 && v2.num_audio_channels != 0 {
                let frame = v2.const_bytes_per_audio_packet;
                if !frame.is_multiple_of(v2.num_audio_channels)
                    || (frame / v2.num_audio_channels).saturating_mul(8) != bits
                {
                    return None;
                }
            }
            PcmLayout::new(
                bits,
                f.is_float(),
                f.is_float() || f.is_signed_integer(),
                f.is_big_endian(),
            )
        }
        _ => return None,
    };
    layout.codec_id()?;
    Some(layout)
}

/// The codec id of a compressed sound format whose QTFF format code
/// names exactly one codec (Table 4-7 p. 176 – 177): `ima4` (IMA 4:1,
/// p. 187), `ulaw` / `alaw` (p. 187 – 188), the two Microsoft ACM
/// codes (ADPCM code 2, IMA ADPCM code 17), both MPEG-1 layer 3
/// codes, `ac-3`, and `alac`. `mp4a` is resolved from its `esds`
/// object type by the caller.
pub fn compressed_codec_id(format: &[u8; 4]) -> Option<&'static str> {
    Some(match format {
        b"ima4" => "adpcm_ima_qt",
        b"ulaw" => "pcm_mulaw",
        b"alaw" => "pcm_alaw",
        [0x6D, 0x73, 0x00, 0x02] => "adpcm_ms",
        [0x6D, 0x73, 0x00, 0x11] => "adpcm_ima_wav",
        [0x6D, 0x73, 0x00, 0x55] | b".mp3" => "mp3",
        b"ac-3" => "ac3",
        b"alac" => "alac",
        _ => return None,
    })
}

/// The `esds` elementary stream descriptor of a sound description —
/// carried directly in its extension area or inside the `wave`
/// siDecompressionParam (QTFF 2012-08-14 pp. 185 – 188) — parsed to
/// its DecoderConfigDescriptor.
pub fn es_decoder_config(desc: &SampleDescription) -> Option<EsDecoderConfig> {
    let descriptor = desc
        .esds
        .as_deref()
        .or_else(|| desc.si_decompression_param.as_ref()?.esds())?;
    EsDecoderConfig::parse(descriptor)
}

/// The core channel layout of an MPEG-4 Audio `channelConfiguration`
/// (ISO/IEC 14496-3:2019 Table 1.23) — the speaker set, named by the
/// layout whose positions it covers. Configurations core has no
/// layout for (7 = 7.1 front with left / right front-centre speakers,
/// 13 = 22.2, 14 = 7.1 with front height speakers) are
/// `DiscreteN(count)`.
#[cfg(feature = "registry")]
pub fn layout_for_channel_configuration(cc: u8) -> Option<oxideav_core::ChannelLayout> {
    use oxideav_core::ChannelLayout::*;
    Some(match cc {
        1 => Mono,
        2 => Stereo,
        3 => Surround30,
        4 => Surround40,
        5 => Surround50,
        6 => Surround51,
        11 => Surround61,
        12 => Surround71,
        7 | 13 | 14 => DiscreteN(channel_configuration_channels(cc)?),
        _ => return None,
    })
}

/// The core channel layout a `chan` audio channel layout atom names
/// through a predefined layout tag whose channel order is exactly the
/// core layout's canonical order (see the tag constants in
/// [`crate::media_meta`]).
///
/// * `Some(layout)` — mono, stereo, MPEG 3.0 A (L R C), 4.0 A
///   (L R C Cs), 5.0 A (L R C Ls Rs), 5.1 A (L R C LFE Ls Rs), 7.1 C
///   (L R C LFE Ls Rs Rls Rrs);
/// * `Some(DiscreteN(n))` — a known tag whose channel order differs
///   from every core layout's (the B / C / D orderings, MPEG 6.1 A,
///   7.1 A / B): the channels are there but core cannot name their
///   order;
/// * `None` — the bitmap and per-channel-description forms and
///   unknown tags (their label / bit semantics are not staged), which
///   leave the layout to the channel count.
#[cfg(feature = "registry")]
pub fn layout_for_chan(chan: &crate::media_meta::Chan) -> Option<oxideav_core::ChannelLayout> {
    use crate::media_meta::*;
    use oxideav_core::ChannelLayout::*;
    Some(match chan.layout_tag {
        TAG_MONO => Mono,
        TAG_STEREO => Stereo,
        TAG_MPEG_3_0_A => Surround30,
        TAG_MPEG_4_0_A => Surround40,
        TAG_MPEG_5_0_A => Surround50,
        TAG_MPEG_5_1_A => Surround51,
        TAG_MPEG_7_1_C => Surround71,
        TAG_MPEG_3_0_B | TAG_MPEG_4_0_B | TAG_MPEG_5_0_B | TAG_MPEG_5_0_C | TAG_MPEG_5_0_D
        | TAG_MPEG_5_1_B | TAG_MPEG_5_1_C | TAG_MPEG_5_1_D | TAG_MPEG_6_1_A | TAG_MPEG_7_1_A
        | TAG_MPEG_7_1_B => DiscreteN((chan.layout_tag & 0xFFFF) as u16),
        _ => return None,
    })
}

/// The predefined `chan` layout tag for a core layout — the inverse
/// of [`layout_for_chan`] over the layouts it names.
#[cfg(feature = "registry")]
pub fn chan_tag_for_layout(layout: oxideav_core::ChannelLayout) -> Option<u32> {
    use crate::media_meta::*;
    use oxideav_core::ChannelLayout::*;
    Some(match layout {
        Mono => TAG_MONO,
        Stereo => TAG_STEREO,
        Surround30 => TAG_MPEG_3_0_A,
        Surround40 => TAG_MPEG_4_0_A,
        Surround50 => TAG_MPEG_5_0_A,
        Surround51 => TAG_MPEG_5_1_A,
        Surround71 => TAG_MPEG_7_1_C,
        _ => return None,
    })
}

/// A framed `chan` atom (QTFF 2012-08-14 p. 186: version 0, flags 0,
/// then the big-endian layout structure) naming a predefined layout
/// tag — no bitmap, no channel descriptions.
pub fn chan_atom_for_tag(tag: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&24u32.to_be_bytes());
    out.extend_from_slice(b"chan");
    out.extend_from_slice(&0u32.to_be_bytes()); // version + flags
    out.extend_from_slice(&tag.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // bitmap
    out.extend_from_slice(&0u32.to_be_bytes()); // number of descriptions
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aac_lc_stereo() {
        // AOT 2, 44100 (idx 4), channelConfiguration 2.
        let a = AudioSpecificConfig::parse(&[0x12, 0x10]).unwrap();
        assert_eq!(a.audio_object_type, 2);
        assert_eq!(a.sampling_frequency, Some(44100));
        assert_eq!(a.channel_configuration, 2);
        assert_eq!(a.sbr_present, None);
        assert_eq!(a.output_sample_rate(), Some(44100));
        assert_eq!(a.output_channels(), Some(2));
    }

    #[test]
    fn hierarchical_sbr_and_ps() {
        // AOT 29 (PS), sfi 6 (24000), cc 1, ext sfi 3 (48000), AOT 2.
        let mut w = BitWriter::default();
        w.put(29, 5);
        w.put(6, 4);
        w.put(1, 4);
        w.put(3, 4);
        w.put(2, 5);
        w.put(0, 3); // GASpecificConfig: frameLength, dependsOnCore, extension
        let a = AudioSpecificConfig::parse(&w.finish()).unwrap();
        assert_eq!(a.audio_object_type, 2);
        assert_eq!(a.extension_audio_object_type, 5);
        assert_eq!(a.sbr_present, Some(true));
        assert_eq!(a.ps_present, Some(true));
        assert_eq!(a.output_sample_rate(), Some(48000));
        assert_eq!(a.output_channels(), Some(2));
    }

    #[test]
    fn backward_compatible_sbr_with_explicit_no_ps() {
        // AOT 2, 22050, mono; 0x2b7, AOT 5, sbr 1, 44100; 0x548, ps 0.
        let mut w = BitWriter::default();
        w.put(2, 5);
        w.put(7, 4);
        w.put(1, 4);
        w.put(0, 3);
        w.put(0x2B7, 11);
        w.put(5, 5);
        w.put(1, 1);
        w.put(4, 4);
        w.put(0x548, 11);
        w.put(0, 1);
        let a = AudioSpecificConfig::parse(&w.finish()).unwrap();
        assert_eq!(a.sbr_present, Some(true));
        assert_eq!(a.ps_present, Some(false));
        assert_eq!(a.output_sample_rate(), Some(44100));
        assert_eq!(a.output_channels(), Some(1));
    }

    #[test]
    fn program_config_element_counts_speakers() {
        // AOT 2, 48000, cc 0, then a PCE: 2 front (SCE + CPE), 1 side
        // CPE, 1 back SCE, 1 LFE.
        let mut w = BitWriter::default();
        w.put(2, 5);
        w.put(3, 4);
        w.put(0, 4);
        w.put(0, 3); // GASpecificConfig flags
        w.put(0, 4); // element_instance_tag
        w.put(1, 2); // object_type
        w.put(3, 4); // sampling_frequency_index
        w.put(2, 4);
        w.put(1, 4);
        w.put(1, 4);
        w.put(1, 2);
        w.put(0, 3);
        w.put(0, 4);
        w.put(0, 3); // no mixdowns
        w.put(0, 5); // front SCE
        w.put(0x10, 5); // front CPE
        w.put(0x10, 5); // side CPE
        w.put(0, 5); // back SCE
        w.put(0, 4); // lfe
        w.align();
        w.put(0, 8); // comment_field_bytes
        let a = AudioSpecificConfig::parse(&w.finish()).unwrap();
        let p = a.program_config.unwrap();
        assert_eq!(
            (
                p.front_channels,
                p.side_channels,
                p.back_channels,
                p.lfe_channels
            ),
            (3, 2, 1, 1)
        );
        assert_eq!(a.output_channels(), Some(7));
    }

    #[test]
    fn truncated_config_is_rejected() {
        assert_eq!(AudioSpecificConfig::parse(&[0x12]), None);
    }

    #[test]
    fn pcm_codec_ids_round_trip() {
        for id in [
            "pcm_u8",
            "pcm_s8",
            "pcm_s16be",
            "pcm_s16le",
            "pcm_s24be",
            "pcm_s24le",
            "pcm_s32be",
            "pcm_s32le",
            "pcm_f32be",
            "pcm_f32le",
            "pcm_f64be",
            "pcm_f64le",
            "pcm_u16le",
        ] {
            let l = PcmLayout::from_codec_id(id).unwrap();
            assert_eq!(l.codec_id(), Some(id));
        }
        assert_eq!(PcmLayout::from_codec_id("pcm_s16"), None);
        assert_eq!(PcmLayout::from_codec_id("pcm_mulaw"), None);
    }

    #[cfg(feature = "registry")]
    #[test]
    fn chan_layout_tags_map_to_core_layouts() {
        use crate::media_meta::*;
        use oxideav_core::ChannelLayout::*;
        let chan = |tag: u32| Chan {
            layout_tag: tag,
            ..Default::default()
        };
        assert_eq!(layout_for_chan(&chan(TAG_MONO)), Some(Mono));
        assert_eq!(layout_for_chan(&chan(TAG_MPEG_5_1_A)), Some(Surround51));
        assert_eq!(layout_for_chan(&chan(TAG_MPEG_7_1_C)), Some(Surround71));
        // Same speakers, another order: count only.
        assert_eq!(layout_for_chan(&chan(TAG_MPEG_5_1_D)), Some(DiscreteN(6)));
        assert_eq!(layout_for_chan(&chan(TAG_MPEG_6_1_A)), Some(DiscreteN(7)));
        assert_eq!(layout_for_chan(&chan(TAG_USE_CHANNEL_BITMAP)), None);
        assert_eq!(layout_for_chan(&chan(TAG_USE_CHANNEL_DESCRIPTIONS)), None);
        for l in [
            Mono, Stereo, Surround30, Surround40, Surround50, Surround51, Surround71,
        ] {
            let tag = chan_tag_for_layout(l).unwrap();
            assert_eq!(layout_for_chan(&chan(tag)), Some(l));
            let atom = chan_atom_for_tag(tag);
            assert_eq!(parse_chan(&atom[8..]).unwrap().layout_tag, tag);
        }
        assert_eq!(chan_tag_for_layout(Surround61), None);
        assert_eq!(layout_for_channel_configuration(7), Some(DiscreteN(8)));
        assert_eq!(layout_for_channel_configuration(11), Some(Surround61));
        assert_eq!(layout_for_channel_configuration(0), None);
    }

    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        bit: usize,
    }

    impl BitWriter {
        fn put(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                if self.bit.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let b = ((v >> i) & 1) as u8;
                let last = self.bytes.last_mut().unwrap();
                *last |= b << (7 - self.bit % 8);
                self.bit += 1;
            }
        }
        fn align(&mut self) {
            self.bit = self.bit.div_ceil(8) * 8;
        }
        fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }
}
