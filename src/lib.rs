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
pub mod demuxer;
pub mod edit;
pub mod gmhd;
pub mod header;
pub mod media_meta;
pub mod reference;
pub mod sample_table;
pub mod track;
pub mod user_data;

#[cfg(feature = "registry")]
pub mod registry;

#[cfg(not(feature = "registry"))]
pub mod standalone;

pub use chapter::{decode_text_sample, ChapterEntry, ChapterList};
pub use demuxer::MovDemuxer;
pub use edit::{Edit, EditList};
pub use gmhd::{parse_gmin, parse_tcmi, parse_text_header, Gmhd, Gmin, Tcmi, TextHeader};
pub use header::{Ftyp, Hdlr, Mdhd, Mvhd, Tkhd, TrackRotation};
pub use media_meta::{
    channel_mask_for_layout_tag, Chan, ChanDescription, Clap, ColorParameters, ColorParametersKind,
    Cslg, MetaKeyValue, Pasp, Tapt,
};
pub use reference::{parse_dref, DataReference, ReferenceMovie};
pub use sample_table::{SampleEntry, SampleTable};
pub use track::{SampleDescription, Track, TrackRef, TrackRefKind};
pub use user_data::{iso_language_tag, parse_udta, UserDataEntry, UserDataKind};
