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
pub mod bmff_meta;
pub mod chapter;
pub mod clip;
pub mod ctab;
pub mod demuxer;
pub mod derived;
pub mod edit;
pub mod fragment;
pub mod gmhd;
pub mod header;
pub mod heif_write;
pub mod iprp;
pub mod kind;
pub mod leva;
pub mod matte;
pub mod media_meta;
pub mod muxer;
pub mod pdin;
pub mod pnot;
pub mod prft;
pub mod reference;
pub mod render;
pub mod sample_aux;
pub mod sample_groups;
pub mod sample_table;
pub mod sidx;
pub mod ssix;
pub mod styp;
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

#[cfg(not(feature = "registry"))]
pub mod standalone;

pub use atom::{
    read_atom_header, read_payload, read_payload_bounded, skip_payload, walk_children, AtomHeader,
    MAX_INMEMORY_ATOM_BODY,
};
pub use bmff_meta::{
    file_extents_for_item, idat_bytes_concat, idat_bytes_for_item, item_data, parse_bmff_meta,
    primary_item_data, BmffMeta, DataLocation, ItemDataLocation, ItemExtent, ItemInfoEntry,
    ItemLocation, ItemProtection, ItemReference, ItemReferenceType, ProtectionScheme,
};
pub use chapter::{
    decode_text_sample, decode_text_sample_full, parse_text_sample_styles, ChapterEntry,
    ChapterList, ColorRgba, FontTableEntry, HighlightColor, HighlightRange, StyleRecord,
    TextSampleStyles,
};
pub use clip::{parse_clip, parse_crgn, Clipping, ClippingRegion, QdRect};
pub use ctab::{parse_ctab, ColorTableEntry, Ctab};
pub use demuxer::{open_file_url, MovDemuxer, MAX_ALIAS_DEPTH};
pub use derived::{
    build_grid_layout, build_overlay_layout, compute_post_transform_extent, image_layout_for,
    parse_grid, parse_overlay, parse_overlay_with_source_count, parse_tmap_payload,
    plan_grid_layout, plan_overlay_layout, primary_image_layout_for, Grid, GridTilePlacement,
    ImageGridLayout, ImageLayout, IspeMismatch, Overlay, OverlayLayer, OverlayLayout, TmapPayload,
    TransformChain, TransformOp,
};
pub use edit::{
    media_pts_to_movie_pts, resolve_edit_segments, Edit, EditList, EditSegment, EditSegmentKind,
};
pub use fragment::{
    parse_mehd, parse_mfhd, parse_mfra, parse_mfro, parse_moof, parse_mvex, parse_tfdt, parse_tfhd,
    parse_tfra, parse_traf, parse_trex, parse_trun, resolve_traf_samples, sample_flags_is_sync,
    Mehd, Mfhd, Mfro, Tfhd, Tfra, TfraEntry, TrafParse, TrafRecord, TrexDefaults, Trun, TrunSample,
    TFHD_BASE_DATA_OFFSET_PRESENT, TFHD_DEFAULT_BASE_IS_MOOF, TFHD_DEFAULT_SAMPLE_DURATION_PRESENT,
    TFHD_DEFAULT_SAMPLE_FLAGS_PRESENT, TFHD_DEFAULT_SAMPLE_SIZE_PRESENT, TFHD_DURATION_IS_EMPTY,
    TFHD_SAMPLE_DESCRIPTION_INDEX_PRESENT, TRUN_DATA_OFFSET_PRESENT,
    TRUN_FIRST_SAMPLE_FLAGS_PRESENT, TRUN_SAMPLE_CTS_OFFSET_PRESENT, TRUN_SAMPLE_DURATION_PRESENT,
    TRUN_SAMPLE_FLAGS_PRESENT, TRUN_SAMPLE_SIZE_PRESENT,
};
pub use gmhd::{parse_gmin, parse_tcmi, parse_text_header, Gmhd, Gmin, Tcmi, TextHeader};
pub use header::{BrandClass, Ftyp, Hdlr, Mdhd, Mvhd, Tkhd, TrackRotation};
pub use heif_write::{HeifDerivation, HeifItem, HeifItemReference, HeifProperty, HeifWriter};
pub use iprp::{
    parse_amve_payload, parse_auxc_payload, parse_cclv_payload, parse_clli_payload,
    parse_colr_payload, parse_iprp, parse_lsel_payload, parse_mdcv_payload, Amve, AuxC, Cclv, Clli,
    ColrInfo, Imir, Irot, Ispe, ItemProperties, ItemProperty, ItemPropertyAssociation,
    LayerSelector, Mdcv, Pixi, PixiInfo, PropertyAssociation,
};
pub use kind::{find_kinds_in_udta, parse_kind, KindEntry};
pub use leva::{parse_leva, AssignmentType, Leva, LevaLevel};
pub use matte::{parse_kmat, parse_matt, CompressedMatte, Matte, MIN_IMAGE_DESCRIPTION_SIZE};
pub use media_meta::{
    channel_mask_for_layout_tag, Chan, ChanDescription, Clap, ColorParameters, ColorParametersKind,
    Cslg, MetaKeyValue, Pasp, Tapt,
};
pub use muxer::{FragmentationMode, MovMuxer, MuxSample, MuxTrackKind};
pub use pdin::{parse_pdin, Pdin, PdinEntry};
pub use pnot::{parse_pnot, Pnot, MAC_TO_UNIX_EPOCH_SECONDS, PNOT_BODY_LEN};
pub use prft::{parse_prft, Prft, NTP_TO_UNIX_EPOCH_SECONDS};
pub use reference::{parse_dref, DataReference, ReferenceMovie};
pub use render::{ispe_dimensions, render_grid, render_iden, render_iovl, Rgba8Canvas};
pub use sample_aux::{parse_saio, parse_saiz, AuxInfoType, FragmentSampleAux, Saio, Saiz};
pub use sample_groups::{
    decode_prol, decode_rap, decode_roll, parse_sbgp, parse_sgpd, AudioPreRoll, RollRecovery,
    SampleGroupDescription, SampleGroupDescriptionEntry, SampleToGroup, SampleToGroupEntry,
    VisualRandomAccess,
};
pub use sample_table::{
    parse_sdtp, parse_stdp, parse_stsh, parse_stz2, parse_subs, IsLeading, SampleDependsOn,
    SampleEntry, SampleHasRedundancy, SampleIsDependedOn, SampleSizeSource, SampleTable, SdtpEntry,
    StshEntry, SubSampleEntry, SubSampleInfo,
};
pub use sidx::{parse_sidx, ReferenceType, Sidx, SidxReference};
pub use ssix::{parse_ssix, Ssix, SsixRange, SsixSubsegment};
pub use styp::{parse_styp, Styp};
pub use timecode::{
    parse_tmcd_sample_description, Tmcd, TMCD_FLAG_24_HOUR, TMCD_FLAG_COUNTER,
    TMCD_FLAG_DROP_FRAME, TMCD_FLAG_NEGATIVES_OK,
};
pub use track::{SampleDescription, Track, TrackRef, TrackRefKind};
pub use track_group::{
    parse_track_group_type, parse_trgr, TrackGroupTypeEntry, TRACK_GROUP_TYPE_MSRC,
};
pub use track_input_map::{
    parse_imap, parse_track_input_entry, InputType, InputTypeKind, ObjectId, TrackInputEntry,
    TrackInputMap, INPUT_TYPE_ATOM, K_TRACK_MODIFIER_OBJECT_GRAPHICS_MODE,
    K_TRACK_MODIFIER_OBJECT_MATRIX, K_TRACK_MODIFIER_TYPE_BALANCE, K_TRACK_MODIFIER_TYPE_CLIP,
    K_TRACK_MODIFIER_TYPE_GRAPHICS_MODE, K_TRACK_MODIFIER_TYPE_IMAGE, K_TRACK_MODIFIER_TYPE_MATRIX,
    K_TRACK_MODIFIER_TYPE_VOLUME, OBJECT_ID_ATOM, TRACK_INPUT_ATOM,
};
pub use track_load::{
    parse_load, Load, LOAD_HINT_DOUBLE_BUFFER, LOAD_HINT_HIGH_QUALITY, LOAD_PRELOAD_ALWAYS,
    LOAD_PRELOAD_DURATION_TO_END, LOAD_PRELOAD_IF_ENABLED,
};
pub use track_selection::{
    find_tsel_in_udta, parse_tsel, ts_attribute_role, TrackSelection, TsAttributeRole,
    TSEL_ATTR_BITRATE, TSEL_ATTR_COARSE_GRAIN_SNR_SCALABILITY, TSEL_ATTR_CODEC,
    TSEL_ATTR_FINE_GRAIN_SNR_SCALABILITY, TSEL_ATTR_FRAME_RATE, TSEL_ATTR_MAX_PACKET_SIZE,
    TSEL_ATTR_MEDIA_LANGUAGE, TSEL_ATTR_MEDIA_TYPE, TSEL_ATTR_NUMBER_OF_VIEWS,
    TSEL_ATTR_REGION_OF_INTEREST_SCALABILITY, TSEL_ATTR_SCREEN_SIZE, TSEL_ATTR_SPATIAL_SCALABILITY,
    TSEL_ATTR_TEMPORAL_SCALABILITY, TSEL_ATTR_VIEW_SCALABILITY,
};
pub use user_data::{iso_language_tag, parse_udta, UserDataEntry, UserDataKind};
pub use uuid::{parse_uuid, Uuid, USERTYPE_LEN};
