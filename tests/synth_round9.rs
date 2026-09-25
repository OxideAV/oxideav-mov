//! Round-9 acceptance: the built-in `file://` URL opener for
//! reference-movie alias chains.
//!
//! (The HEIF derived-image / primary-item-bytes coverage that used
//! to live here moved with the HEIF layer to `oxideav-heif`.)

#![cfg(feature = "registry")]

mod common;

use std::io::{Cursor, Write};

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::{open_file_url, MovDemuxer};

// The three integration tests below construct `file://` URLs from
// real filesystem paths. On Windows those paths use backslashes and
// drive-letter prefixes (e.g. `D:\foo`), which require URL escapes
// and a Windows-aware path-back-conversion that this round's
// `open_file_url` does not implement (see open_file_url module
// docs). The Unix shape (`file:///abs/path`) is exercised on Linux
// and macOS; the Windows shape is left for a follow-up round.
#[cfg(unix)]
#[test]
fn open_file_url_resolves_local_filesystem_alias() {
    use std::env;

    // 1) Write a real self-contained .mov to a tempfile.
    let mut bytes = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut bytes, *b"ftyp", &ftyp);
    push_atom(&mut bytes, *b"mdat", b"PAYLOAD!");
    let mdat_off: u32 = 28;

    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 30));
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 30, 320, 240));
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(600, 30));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    let mut stbl = Vec::new();
    push_atom(
        &mut stbl,
        *b"stsd",
        &build_stsd_video(b"avc1", 320, 240, &[]),
    );
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 30));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(8, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(mdat_off));
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut bytes, *b"moov", &moov);

    // Tempfile path inside std::env::temp_dir().
    let mut path = env::temp_dir();
    path.push(format!(
        "oxideav_mov_round9_target_{}.mov",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(&bytes).expect("write tempfile");
    }
    // Build a reference-movie file pointing at the tempfile via file://.
    let url = format!("file://{}", path.display());
    let mut alias_bytes = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"qt  ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"qt  ");
    push_atom(&mut alias_bytes, *b"ftyp", &ftyp);
    let mut moov2 = Vec::new();
    push_atom(&mut moov2, *b"mvhd", &build_mvhd(600, 0));
    // rmra/rmda/rdrf with url=file://.../target.mov
    let mut rmra = Vec::new();
    let mut rmda = Vec::new();
    let mut rdrf = Vec::new();
    rdrf.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
    rdrf.extend_from_slice(b"url ");
    let url_bytes = url.as_bytes();
    let mut url_with_nul = url_bytes.to_vec();
    url_with_nul.push(0);
    rdrf.extend_from_slice(&(url_with_nul.len() as u32).to_be_bytes());
    rdrf.extend_from_slice(&url_with_nul);
    push_atom(&mut rmda, *b"rdrf", &rdrf);
    push_atom(&mut rmra, *b"rmda", &rmda);
    push_atom(&mut moov2, *b"rmra", &rmra);
    push_atom(&mut alias_bytes, *b"moov", &moov2);

    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(alias_bytes));
    let d = MovDemuxer::open_with_aliases(cur, open_file_url).expect("file:// alias resolves");
    assert!(!d.tracks.is_empty(), "resolved target carries the track");
    // Cleanup.
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_file_url_rejects_non_file_scheme() {
    // open_file_url returns std::io::ErrorKind::Unsupported for
    // anything that's not file://. Match on the Result rather than
    // unwrap_err — `dyn ReadSeek` doesn't impl Debug.
    match open_file_url("http://example.com/foo.mov") {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::Unsupported),
        Ok(_) => panic!("expected Unsupported error for http://"),
    }
    match open_file_url("ftp://server/path") {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::Unsupported),
        Ok(_) => panic!("expected Unsupported error for ftp://"),
    }
}

#[cfg(unix)]
#[test]
fn open_file_url_decodes_percent_encoding() {
    // Build a tempfile whose name contains a literal space, then
    // percent-encode the space in the file:// URL. The opener must
    // decode `%20` and find the file.
    use std::env;
    let mut path = env::temp_dir();
    path.push(format!(
        "oxideav_mov_round9 with spaces_{}.bin",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(b"hello").expect("write");
    }
    let raw = path.display().to_string();
    let encoded = raw.replace(' ', "%20");
    let url = format!("file://{}", encoded);
    let mut handle = open_file_url(&url).expect("opener locates encoded path");
    use std::io::Read;
    let mut buf = Vec::new();
    handle.read_to_end(&mut buf).expect("read tempfile");
    assert_eq!(buf, b"hello");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_file_url_rejects_remote_host_for_safety() {
    // file://example.com/path is rejected so we don't accidentally read
    // from a network mount the user didn't authorise.
    match open_file_url("file://example.com/etc/passwd") {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::Unsupported),
        Ok(_) => panic!("expected Unsupported error for foreign-host file://"),
    }
}

#[cfg(unix)]
#[test]
fn open_file_url_accepts_localhost_authority() {
    // file://localhost/path is the canonical "this host" authority and
    // must resolve identically to file:///path. Verify by opening a
    // tempfile via the localhost form.
    use std::env;
    let mut path = env::temp_dir();
    path.push(format!(
        "oxideav_mov_round9_localhost_{}.bin",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(b"loopback").expect("write");
    }
    let url = format!("file://localhost{}", path.display());
    let mut handle = open_file_url(&url).expect("localhost-host file:// resolves");
    use std::io::Read;
    let mut buf = Vec::new();
    handle.read_to_end(&mut buf).expect("read");
    assert_eq!(buf, b"loopback");
    let _ = std::fs::remove_file(&path);
}
