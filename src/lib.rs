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
pub mod demuxer;
pub mod edit;
pub mod header;
pub mod media_meta;
pub mod sample_table;
pub mod track;

#[cfg(feature = "registry")]
pub mod registry;

#[cfg(not(feature = "registry"))]
pub mod standalone;

pub use demuxer::MovDemuxer;
pub use edit::{Edit, EditList};
pub use header::{Ftyp, Hdlr, Mdhd, Mvhd, Tkhd};
pub use media_meta::{Chan, Clap, ColorParameters, ColorParametersKind, MetaKeyValue, Pasp, Tapt};
pub use sample_table::{SampleEntry, SampleTable};
pub use track::{SampleDescription, Track, TrackRef, TrackRefKind};
