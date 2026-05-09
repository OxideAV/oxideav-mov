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
pub mod demuxer;
pub mod derived;
pub mod edit;
pub mod gmhd;
pub mod header;
pub mod iprp;
pub mod media_meta;
pub mod reference;
pub mod render;
pub mod sample_table;
pub mod timecode;
pub mod track;
pub mod user_data;

#[cfg(feature = "registry")]
pub mod registry;

#[cfg(not(feature = "registry"))]
pub mod standalone;

pub use bmff_meta::{
    file_extents_for_item, idat_bytes_concat, idat_bytes_for_item, item_data, parse_bmff_meta,
    primary_item_data, BmffMeta, DataLocation, ItemDataLocation, ItemExtent, ItemInfoEntry,
    ItemLocation, ItemReference,
};
pub use chapter::{
    decode_text_sample, decode_text_sample_full, parse_text_sample_styles, ChapterEntry,
    ChapterList, ColorRgba, FontTableEntry, HighlightColor, HighlightRange, StyleRecord,
    TextSampleStyles,
};
pub use demuxer::{open_file_url, MovDemuxer, MAX_ALIAS_DEPTH};
pub use derived::{
    build_grid_layout, build_overlay_layout, image_layout_for, parse_grid, parse_overlay,
    parse_overlay_with_source_count, plan_grid_layout, plan_overlay_layout,
    primary_image_layout_for, Grid, GridTilePlacement, ImageGridLayout, ImageLayout, IspeMismatch,
    Overlay, OverlayLayer, OverlayLayout, TransformChain, TransformOp,
};
pub use edit::{Edit, EditList};
pub use gmhd::{parse_gmin, parse_tcmi, parse_text_header, Gmhd, Gmin, Tcmi, TextHeader};
pub use header::{BrandClass, Ftyp, Hdlr, Mdhd, Mvhd, Tkhd, TrackRotation};
pub use iprp::{
    parse_colr_payload, parse_iprp, AuxC, ColrInfo, Imir, Irot, Ispe, ItemProperties, ItemProperty,
    ItemPropertyAssociation, Pixi, PixiInfo, PropertyAssociation,
};
pub use media_meta::{
    channel_mask_for_layout_tag, Chan, ChanDescription, Clap, ColorParameters, ColorParametersKind,
    Cslg, MetaKeyValue, Pasp, Tapt,
};
pub use reference::{parse_dref, DataReference, ReferenceMovie};
pub use render::{ispe_dimensions, render_grid, render_iden, render_iovl, Rgba8Canvas};
pub use sample_table::{SampleEntry, SampleTable};
pub use timecode::{
    parse_tmcd_sample_description, Tmcd, TMCD_FLAG_24_HOUR, TMCD_FLAG_COUNTER,
    TMCD_FLAG_DROP_FRAME, TMCD_FLAG_NEGATIVES_OK,
};
pub use track::{SampleDescription, Track, TrackRef, TrackRefKind};
pub use user_data::{iso_language_tag, parse_udta, UserDataEntry, UserDataKind};
