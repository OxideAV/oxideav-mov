//! Shared QTFF synth-builder helpers used across integration tests.
//!
//! These helpers emit the minimum byte layout each test fixture needs;
//! they do *not* aim to validate every QTFF rule. The goal is for tests
//! to remain hand-readable: each call corresponds to one atom in the
//! resulting file. Field offsets cite QTFF (2001-03-01) figures.

#![allow(dead_code)] // Each integration test uses a different subset.

/// Emit a classic 8-byte-header atom: `[size:u32 BE][type:[u8;4]][body...]`.
pub fn push_atom(out: &mut Vec<u8>, fourcc: [u8; 4], body: &[u8]) {
    let size: u32 = (8 + body.len()) as u32;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&fourcc);
    out.extend_from_slice(body);
}

/// Build a baseline `mvhd` v0 with `time_scale = ts`, `duration = dur`.
pub fn build_mvhd(ts: u32, dur: u32) -> Vec<u8> {
    // QTFF p. 33 Figure 2-3.
    let mut p = vec![0u8; 100];
    p[12..16].copy_from_slice(&ts.to_be_bytes());
    p[16..20].copy_from_slice(&dur.to_be_bytes());
    p[20..24].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    p[24..26].copy_from_slice(&0x0100i16.to_be_bytes()); // volume 1.0
    p[96..100].copy_from_slice(&2u32.to_be_bytes()); // next_track_id
    p
}

/// Build a baseline `tkhd` v0 with `track_id`, `dur` (movie ts),
/// `w_px` × `h_px` (video) — pass `0×0` for audio tracks.
pub fn build_tkhd(track_id: u32, dur: u32, w_px: u32, h_px: u32) -> Vec<u8> {
    build_tkhd_flags(track_id, dur, w_px, h_px, 0x07, 0)
}

/// Build a `tkhd` v0 with the caller-supplied 24-bit `flags`
/// (`enabled|in_movie|in_preview|in_poster` = 0x07 / 0x0F is the usual
/// default) and `alternate_group` (i16). Useful for r74 tests of
/// disabled-track / alt-group semantics.
pub fn build_tkhd_flags(
    track_id: u32,
    dur: u32,
    w_px: u32,
    h_px: u32,
    flags_low_byte: u8,
    alternate_group: i16,
) -> Vec<u8> {
    // QTFF p. 41 Figure 2-7.
    let mut p = vec![0u8; 84];
    p[3] = flags_low_byte;
    p[12..16].copy_from_slice(&track_id.to_be_bytes());
    p[20..24].copy_from_slice(&dur.to_be_bytes());
    // alternate_group lives at v0 offset 34..36 (after duration:4,
    // reserved:8, layer:2).
    p[34..36].copy_from_slice(&alternate_group.to_be_bytes());
    // Identity 3×3 matrix at offset 40 (a=1.0, d=1.0, w=1.0 — 16.16 /
    // 16.16 / 2.30); QTFF p. 199 Figure 4-1.
    p[40..44].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // a
    p[56..60].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // d
    p[72..76].copy_from_slice(&0x4000_0000u32.to_be_bytes()); // w
    p[76..80].copy_from_slice(&(w_px << 16).to_be_bytes());
    p[80..84].copy_from_slice(&(h_px << 16).to_be_bytes());
    p
}

/// Build a baseline `mdhd` v0 with `time_scale = ts`, `duration = dur`.
pub fn build_mdhd(ts: u32, dur: u32) -> Vec<u8> {
    // QTFF p. 55 Figure 2-16.
    let mut p = vec![0u8; 24];
    p[12..16].copy_from_slice(&ts.to_be_bytes());
    p[16..20].copy_from_slice(&dur.to_be_bytes());
    p
}

/// Build a baseline `hdlr` declaring the given component subtype
/// (`vide` / `soun` / `mdta` / etc.).
pub fn build_hdlr(component_type: &[u8; 4], component_subtype: &[u8; 4]) -> Vec<u8> {
    // QTFF p. 57 Figure 2-17.
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(component_type);
    p.extend_from_slice(component_subtype);
    p.extend_from_slice(&[0u8; 12]); // manuf + flags + flags_mask
    p.push(0); // counted-Pascal-string name length 0
    p
}

/// Build a `vmhd` (video media header).
pub fn build_vmhd() -> Vec<u8> {
    // QTFF p. 64 Figure 2-19.
    let mut p = vec![0u8; 12];
    p[3] = 0x01; // flags = no-lean-ahead
    p
}

/// Build a `stsd` carrying a single video sample description with
/// the given format FourCC, dimensions, and an optional `extras`
/// atom blob appended after the 70-byte fixed video body.
pub fn build_stsd_video(format: &[u8; 4], w: u16, h: u16, extras: &[u8]) -> Vec<u8> {
    // QTFF p. 70 Figure 2-27 + p. 92 Video Sample Description.
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    p.extend_from_slice(&1u32.to_be_bytes()); // entry count
    let entry_size: u32 = (86 + extras.len()) as u32;
    p.extend_from_slice(&entry_size.to_be_bytes());
    p.extend_from_slice(format);
    p.extend_from_slice(&[0u8; 6]); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    let mut vbody = vec![0u8; 70];
    vbody[24..26].copy_from_slice(&w.to_be_bytes());
    vbody[26..28].copy_from_slice(&h.to_be_bytes());
    p.extend_from_slice(&vbody);
    p.extend_from_slice(extras);
    p
}

/// Build a `stsd` carrying a single audio sample description.
pub fn build_stsd_audio(
    format: &[u8; 4],
    channels: u16,
    bits: u16,
    sample_rate: u32,
    extras: &[u8],
) -> Vec<u8> {
    // QTFF p. 100 (Sound Sample Description, v0).
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes());
    let entry_size: u32 = (16 + 20 + extras.len()) as u32;
    p.extend_from_slice(&entry_size.to_be_bytes());
    p.extend_from_slice(format);
    p.extend_from_slice(&[0u8; 6]);
    p.extend_from_slice(&1u16.to_be_bytes()); // dref index
    let mut sbody = vec![0u8; 20];
    sbody[8..10].copy_from_slice(&channels.to_be_bytes());
    sbody[10..12].copy_from_slice(&bits.to_be_bytes());
    sbody[16..20].copy_from_slice(&(sample_rate << 16).to_be_bytes());
    p.extend_from_slice(&sbody);
    p.extend_from_slice(extras);
    p
}

/// Build a constant-size `stsz` (`sample_size` non-zero, table absent).
pub fn build_stsz_constant(sample_size: u32, count: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&sample_size.to_be_bytes());
    p.extend_from_slice(&count.to_be_bytes());
    p
}

/// Build an `stsc` with a single entry (`first_chunk=1, samples_per_chunk, sample_description_id=1`).
pub fn build_stsc_single(samples_per_chunk: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes()); // entry count
    p.extend_from_slice(&1u32.to_be_bytes()); // first_chunk
    p.extend_from_slice(&samples_per_chunk.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes()); // sample_description_id
    p
}

/// Build a single-run `stts` (count, duration).
pub fn build_stts_single(count: u32, duration: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(&count.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p
}

/// Build a single-entry `stco` pointing at `chunk_offset`.
pub fn build_stco_single(chunk_offset: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(&chunk_offset.to_be_bytes());
    p
}
