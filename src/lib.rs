//! Apple QuickTime File Format (QTFF) demuxer.
//!
//! Pure-Rust parser for the Apple QuickTime container, the immediate
//! ancestor of ISO BMFF (ISO/IEC 14496-12) and the canonical `.mov`
//! file format. The demuxer walks the atom hierarchy once, builds
//! per-track sample tables, and exposes a packet-stream surface via
//! the [`Demuxer`] trait when the default `registry` cargo feature
//! is on (or a free-standing parsing API when it's off).
//!
//! Reference: Apple QuickTime File Format Specification (QTFF,
//! 2001-03-01) — primarily Chapters 1–3.

pub mod atom;
pub mod chapter;
pub mod clip;
pub mod cmov;
pub mod codec_config;
pub mod ctab;
pub mod demuxer;
pub mod edit;
pub mod fragment;
pub mod gmhd;
pub mod header;
pub mod kind;
pub mod leva;
pub mod matte;
pub mod media_meta;
pub mod metadata_sample;
pub mod muxer;
pub mod pdin;
pub mod pnot;
pub mod prft;
pub mod qt_metadata;
pub mod reference;
pub mod sample_aux;
pub mod sample_groups;
pub mod sample_table;
pub mod sidx;
pub mod ssix;
pub mod styp;
pub mod sub_track;
pub mod text_sample;
pub mod timecode;
pub mod track;
pub mod track_group;
pub mod track_input_map;
pub mod track_load;
pub mod track_selection;
pub mod user_data;
pub mod uuid;

#[cfg(feature = "registry")]
pub mod registry;

#[cfg(feature = "registry")]
mod video_config;

#[cfg(not(feature = "registry"))]
pub mod standalone;

pub use atom::{
    AtomHeader, MAX_INMEMORY_ATOM_BODY, read_atom_header, read_payload, read_payload_bounded,
    skip_payload, walk_children,
};
pub use chapter::{
    ChapterEntry, ChapterList, ColorRgba, FontTableEntry, HighlightColor, HighlightRange,
    StyleRecord, TextSampleStyles, decode_text_sample, decode_text_sample_full, encode_text_sample,
    parse_text_sample_styles,
};
pub use clip::{Clipping, ClippingRegion, QdRect, parse_clip, parse_crgn};
pub use cmov::{
    CMVD_MIN_BODY_LEN, Cmov, Cmvd, DCOM_ALG_ZLIB, DCOM_BODY_LEN, Dcom,
    compress as compress_movie_resource, parse_cmov, parse_cmvd, parse_dcom,
};
pub use codec_config::{config_atom_for_format, config_record_from_extensions};
pub use ctab::{ColorTableEntry, Ctab, parse_ctab};
pub use demuxer::{
    DataReferenceOpener, MAX_ALIAS_DEPTH, MovDemuxer, dref_file_opener, open_file_url,
};
pub use edit::{
    EMPTY_EDIT_MEDIA_TIME, Edit, EditList, EditSegment, EditSegmentKind, EditedTiming, Elst,
    MEDIA_RATE_ONE, edited_timing_for_sample, first_presented_media_time, initial_empty_duration,
    media_pts_to_movie_pts, movie_pts_to_media_pts, resolve_edit_segments, total_edit_duration,
};
pub use fragment::{
    Mehd, Mfhd, Mfro, TFHD_BASE_DATA_OFFSET_PRESENT, TFHD_DEFAULT_BASE_IS_MOOF,
    TFHD_DEFAULT_SAMPLE_DURATION_PRESENT, TFHD_DEFAULT_SAMPLE_FLAGS_PRESENT,
    TFHD_DEFAULT_SAMPLE_SIZE_PRESENT, TFHD_DURATION_IS_EMPTY,
    TFHD_SAMPLE_DESCRIPTION_INDEX_PRESENT, TRUN_DATA_OFFSET_PRESENT,
    TRUN_FIRST_SAMPLE_FLAGS_PRESENT, TRUN_SAMPLE_CTS_OFFSET_PRESENT, TRUN_SAMPLE_DURATION_PRESENT,
    TRUN_SAMPLE_FLAGS_PRESENT, TRUN_SAMPLE_SIZE_PRESENT, Tfhd, Tfra, TfraEntry, TrafAddressing,
    TrafParse, TrafRecord, TrexDefaults, Trun, TrunSample, parse_mehd, parse_mfhd, parse_mfra,
    parse_mfro, parse_moof, parse_mvex, parse_tfdt, parse_tfhd, parse_tfra, parse_traf, parse_trex,
    parse_trun, resolve_traf_samples, sample_flags_is_sync,
};
pub use gmhd::{
    Gmhd, Gmin, GraphicsMode, Tcmi, TextHeader, parse_gmin, parse_tcmi, parse_text_header,
};
pub use header::{
    BrandClass, Ftyp, Hdlr, Hmhd, Mdhd, MediaHeaderKind, Mvhd, Tkhd, TrackRotation, parse_elng,
    parse_hmhd,
};
pub use kind::{KindEntry, find_kinds_in_udta, parse_kind};
pub use leva::{AssignmentType, Leva, LevaLevel, parse_leva};
pub use matte::{CompressedMatte, MIN_IMAGE_DESCRIPTION_SIZE, Matte, parse_kmat, parse_matt};
pub use media_meta::{
    Chan, ChanDescription, Clap, ColorParameters, ColorParametersKind, Cslg, FIEL_BODY_LEN, Fiel,
    FieldOrdering, MetaKeyValue, Mjht, Mjqt, Pasp, Tapt, TaptDims, channel_mask_for_layout_tag,
    parse_cslg, parse_fiel, parse_mjht, parse_mjqt,
};
pub use metadata_sample::{
    BitRate, MetadataSampleEntry, SimpleTextSampleEntry, SubtitleSampleEntry,
    TextMetadataSampleEntry, TextSubtitleSampleEntry, UriMetadataSampleEntry,
    XmlMetadataSampleEntry, XmlSubtitleSampleEntry, parse_btrt, parse_metadata_sample_entry,
    parse_mett, parse_metx, parse_sbtt, parse_stpp, parse_stxt, parse_subtitle_sample_entry,
    parse_urim,
};
pub use muxer::{
    AudioEntryV1, AudioEntryV2, ChunkStrategy, DataReferenceWrite, ExternalSampleLocation,
    FragmentationMode, MDHD_LANGUAGE_UND, META_NAMESPACE_MDTA, META_TYPE_BE_SIGNED_INT,
    META_TYPE_BE_UNSIGNED_INT, META_TYPE_RAW, META_TYPE_UTF8, MoovPlacement, MovMetaItem,
    MovMetaValue, MovMetadata, MovMuxer, MuxEdit, MuxSample, MuxTrackKind, SampleAuxStream,
    SampleGroupBoxForm, SampleGroupDescriptionWrite, SampleToGroupWrite, TrackReference,
    UTF8_INTL_TEXT_FLAG, VisualExtensions,
};
pub use pdin::{Pdin, PdinEntry, parse_pdin};
pub use pnot::{MAC_TO_UNIX_EPOCH_SECONDS, PNOT_BODY_LEN, Pnot, parse_pnot};
pub use prft::{NTP_TO_UNIX_EPOCH_SECONDS, Prft, parse_prft};
pub use qt_metadata::{
    DecodedValue, LocaleIndicator, METADATA_HANDLER_MDTA, MetaItem, MetaKey, MetaValue, QtMetadata,
    WellKnownType, parse_qt_metadata,
};
pub use reference::{DataReference, ReferenceMovie, parse_dref};
pub use sample_aux::{AuxInfoType, FragmentSampleAux, Saio, Saiz, parse_saio, parse_saiz};
pub use sample_groups::{
    AudioPreRoll, CSGP_FRAGMENT_LOCAL_BIT, CsgpIndex, RollRecovery, SampleGroupDescription,
    SampleGroupDescriptionEntry, SampleToGroup, SampleToGroupEntry, StreamAccessPoint,
    TemporalLevel, VisualRandomAccess, decode_prol, decode_rap, decode_roll, decode_sap,
    decode_tele, parse_csgp, parse_sbgp, parse_sgpd, split_csgp_index,
};
pub use sample_table::{
    IsLeading, SampleDependsOn, SampleEntry, SampleHasRedundancy, SampleIsDependedOn,
    SampleSizeSource, SampleTable, SdtpEntry, StshEntry, SubSampleEntry, SubSampleInfo, parse_padb,
    parse_sdtp, parse_stdp, parse_stsh, parse_stz2, parse_subs,
};
pub use sidx::{ReferenceType, Sidx, SidxReference, parse_sidx};
pub use ssix::{Ssix, SsixRange, SsixSubsegment, parse_ssix};
pub use styp::{Styp, parse_styp};
pub use sub_track::{
    SubTrack, SubTrackInformation, SubTrackSampleGroup, find_sub_tracks_in_udta, parse_stri,
    parse_strk, parse_stsg,
};
pub use text_sample::{
    Rgb48, TEXT_FACE_BOLD, TEXT_FACE_CONDENSE, TEXT_FACE_EXTEND, TEXT_FACE_ITALIC,
    TEXT_FACE_OUTLINE, TEXT_FACE_SHADOW, TEXT_FACE_UNDERLINE, TEXT_FLAG_ANTI_ALIAS,
    TEXT_FLAG_CONTINUOUS_SCROLL, TEXT_FLAG_DONT_AUTO_SCALE, TEXT_FLAG_DROP_SHADOW,
    TEXT_FLAG_HORIZONTAL_SCROLL, TEXT_FLAG_KEY_TEXT, TEXT_FLAG_REVERSE_SCROLL, TEXT_FLAG_SCROLL_IN,
    TEXT_FLAG_SCROLL_OUT, TEXT_FLAG_USE_MOVIE_BG_COLOR, TEXT_SAMPLE_DESC_FIXED_LEN, TextBox,
    TextJustification, TextSampleDescription, parse_text_sample_description,
};
pub use timecode::{
    StartTimecode, TMCD_FLAG_24_HOUR, TMCD_FLAG_COUNTER, TMCD_FLAG_DROP_FRAME,
    TMCD_FLAG_NEGATIVES_OK, TimecodeRecord, TimecodeSample, Tmcd, parse_tmcd_sample_description,
};
pub use track::{
    ChannelLayout, ChannelStructure, LpcmFlags, SampleDescription, SiDecompressionParam,
    SoundSlopeAndIntercept, SoundV1, SoundV2, SpeakerPosition, Track, TrackRef, TrackRefKind,
    WaveChild, build_esds_atom, esds_decoder_specific_info, parse_chnl, parse_esds, parse_flap,
    parse_srat, parse_wave,
};
pub use track_group::{
    TRACK_GROUP_TYPE_MSRC, TrackGroupTypeEntry, parse_track_group_type, parse_trgr,
};
pub use track_input_map::{
    INPUT_TYPE_ATOM, InputType, InputTypeKind, K_TRACK_MODIFIER_OBJECT_GRAPHICS_MODE,
    K_TRACK_MODIFIER_OBJECT_MATRIX, K_TRACK_MODIFIER_TYPE_BALANCE, K_TRACK_MODIFIER_TYPE_CLIP,
    K_TRACK_MODIFIER_TYPE_GRAPHICS_MODE, K_TRACK_MODIFIER_TYPE_IMAGE, K_TRACK_MODIFIER_TYPE_MATRIX,
    K_TRACK_MODIFIER_TYPE_VOLUME, OBJECT_ID_ATOM, ObjectId, TRACK_INPUT_ATOM, TrackInputEntry,
    TrackInputMap, parse_imap, parse_track_input_entry,
};
pub use track_load::{
    LOAD_HINT_DOUBLE_BUFFER, LOAD_HINT_HIGH_QUALITY, LOAD_PRELOAD_ALWAYS,
    LOAD_PRELOAD_DURATION_TO_END, LOAD_PRELOAD_IF_ENABLED, Load, parse_load,
};
pub use track_selection::{
    TSEL_ATTR_BITRATE, TSEL_ATTR_COARSE_GRAIN_SNR_SCALABILITY, TSEL_ATTR_CODEC,
    TSEL_ATTR_FINE_GRAIN_SNR_SCALABILITY, TSEL_ATTR_FRAME_RATE, TSEL_ATTR_MAX_PACKET_SIZE,
    TSEL_ATTR_MEDIA_LANGUAGE, TSEL_ATTR_MEDIA_TYPE, TSEL_ATTR_NUMBER_OF_VIEWS,
    TSEL_ATTR_REGION_OF_INTEREST_SCALABILITY, TSEL_ATTR_SCREEN_SIZE, TSEL_ATTR_SPATIAL_SCALABILITY,
    TSEL_ATTR_TEMPORAL_SCALABILITY, TSEL_ATTR_VIEW_SCALABILITY, TrackSelection, TsAttributeRole,
    find_tsel_in_udta, parse_tsel, ts_attribute_role,
};
pub use user_data::{UserDataEntry, UserDataKind, iso_language_tag, parse_udta};
pub use uuid::{USERTYPE_LEN, Uuid, parse_uuid};
