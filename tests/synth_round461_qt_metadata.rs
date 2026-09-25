//! Round-461 acceptance: the full QuickTime Metadata atom (QTFF 2012
//! "Metadata", pp. 129 – 143) both directions.
//!
//! * Read: `hdlr` / `mhdr` / `ctry` / `lang` / `keys` / `ilst` with
//!   `itif`, `name` and several locale-tagged `data` values per item
//!   surface on [`MovDemuxer::qt_metadata`] / [`Track::qt_metadata`]
//!   as a [`QtMetadata`]; the historical flat [`MovDemuxer::meta`]
//!   view stays exactly what it was (first value per key).
//! * Write: [`MovMetaItem`] values carry countries / languages; the
//!   muxer derives the `ctry` / `lang` list sets, the `mhdr` header
//!   and the `itif` / `name` children, and everything round-trips
//!   through our own parser.
//! * Locale matching follows p. 140 (default matches everything, a
//!   list matches by membership, a locale matches when both halves
//!   match) and p. 142 Data Ordering (most specific first).
//! * Black box: `ffprobe` (an opaque validator binary) still reads the
//!   file and reports the key; skipped when it is not installed.

#![cfg(feature = "registry")]

use std::io::Cursor;
use std::process::Command;

use oxideav_core::ReadSeek;
use oxideav_mov::{
    DecodedValue, LocaleIndicator, MovDemuxer, MovMetaItem, MovMetaValue, MovMetadata, MovMuxer,
    MuxSample, MuxTrackKind, QtMetadata, WellKnownType, META_NAMESPACE_MDTA,
    META_TYPE_BE_UNSIGNED_INT, META_TYPE_UTF8,
};

fn one_audio_track(m: &mut MovMuxer) -> u32 {
    // Raw PCM so black-box validators can open the sample data.
    let samples = vec![MuxSample {
        data: vec![0u8; 4 * 480],
        duration: 480,
        keyframe: true,
        composition_offset: 0,
    }];
    m.add_track(
        MuxTrackKind::Audio {
            format: *b"sowt",
            channels: 2,
            bits_per_sample: 16,
            sample_rate: 48000,
        },
        48000,
        samples,
        &[],
    )
}

const EU: [[u8; 2]; 4] = [*b"DE", *b"GB", *b"FR", *b"IT"];
const DE_FR: [[u8; 3]; 2] = [*b"deu", *b"fra"];

/// The Table 3-4 example set: a title with a list-locale value, an
/// immediate-locale value, a language-only value and a default; a
/// second item sharing the same country list; an integer item with an
/// id and a name.
fn items() -> Vec<MovMetaItem> {
    vec![
        MovMetaItem::utf8("com.apple.quicktime.title", "Titel")
            .with_locale(&EU, &DE_FR)
            .with_value(
                MovMetaValue::utf8("Titre")
                    .for_countries(&[*b"CA"])
                    .for_languages(&[*b"fra"]),
            )
            .with_value(MovMetaValue::utf8("Title").for_languages(&[*b"eng"]))
            .with_value(MovMetaValue::utf8("Default"))
            .named("title")
            .with_item_id(3),
        MovMetaItem::utf8("com.apple.quicktime.album", "Album EU").with_locale(&EU, &[]),
        MovMetaItem::typed(
            META_NAMESPACE_MDTA,
            "com.example.rating",
            META_TYPE_BE_UNSIGNED_INT,
            vec![0x01, 0x02],
        )
        .with_item_id(9),
    ]
}

fn build(track_items: Option<&[MovMetaItem]>) -> Vec<u8> {
    let mut m = MovMuxer::new();
    let tid = one_audio_track(&mut m);
    m.set_apple_metadata(&items());
    if let Some(ti) = track_items {
        m.set_track_apple_metadata(tid, ti).unwrap();
    }
    let mut out = Cursor::new(Vec::new());
    m.write_to(&mut out).unwrap();
    out.into_inner()
}

fn open(bytes: Vec<u8>) -> MovDemuxer {
    let cur: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
    MovDemuxer::open(cur).expect("open")
}

fn pack(tag: &[u8; 3]) -> u16 {
    MovMetadata::iso_language(*tag)
}

#[test]
fn movie_metadata_round_trips_every_child() {
    let d = open(build(None));
    let qm: &QtMetadata = d.qt_metadata.as_ref().expect("qt_metadata");
    assert!(qm.has_mdta_handler());
    assert!(qm.is_quicktime_shape());
    // mhdr: one past the largest item id in use.
    assert_eq!(qm.next_item_id, Some(10));
    // One shared country list, one language list.
    assert_eq!(qm.country_lists, vec![EU.to_vec()]);
    assert_eq!(qm.language_lists, vec![vec![pack(b"deu"), pack(b"fra")]]);
    assert_eq!(qm.keys.len(), 3);
    assert_eq!(qm.keys[0].name(), Some("com.apple.quicktime.title"));
    assert_eq!(qm.keys[0].namespace, META_NAMESPACE_MDTA);
    assert_eq!(qm.items.len(), 3);

    let title = qm.item_named("com.apple.quicktime.title").unwrap();
    assert_eq!(title.key_index, 1);
    assert_eq!(title.item_id, Some(3));
    assert_eq!(title.name.as_deref(), Some("title"));
    assert_eq!(title.values.len(), 4);
    let v = &title.values;
    assert_eq!(v[0].country_indicator(), LocaleIndicator::ListIndex(1));
    assert_eq!(v[0].language_indicator(), LocaleIndicator::ListIndex(1));
    assert_eq!(qm.countries_for(&v[0]), EU.to_vec());
    assert_eq!(qm.language_tags_for(&v[0]), DE_FR.to_vec());
    assert_eq!(
        v[1].country_indicator(),
        LocaleIndicator::Code(u16::from_be_bytes(*b"CA"))
    );
    assert_eq!(
        v[1].language_indicator(),
        LocaleIndicator::Code(pack(b"fra"))
    );
    assert_eq!(v[2].country_indicator(), LocaleIndicator::Default);
    assert_eq!(qm.language_tags_for(&v[2]), vec![*b"eng"]);
    assert!(v[3].is_default_locale());
    assert_eq!(v[3].well_known_type(), Some(WellKnownType::Utf8));
    assert_eq!(v[0].as_text().as_deref(), Some("Titel"));

    let album = qm.item_named("com.apple.quicktime.album").unwrap();
    assert_eq!(album.item_id, None);
    assert_eq!(album.name, None);
    assert_eq!(
        album.values[0].country_indicator(),
        LocaleIndicator::ListIndex(1)
    );
    assert_eq!(
        album.values[0].language_indicator(),
        LocaleIndicator::Default
    );

    let rating = qm.item_named("com.example.rating").unwrap();
    assert_eq!(rating.item_id, Some(9));
    assert_eq!(
        rating.values[0].decode(),
        Some(DecodedValue::UnsignedInt(0x0102))
    );
}

#[test]
fn locale_matching_follows_table_3_4() {
    let d = open(build(None));
    let qm = d.qt_metadata.as_ref().unwrap();
    let title = qm.item_named("com.apple.quicktime.title").unwrap();
    let pick = |c: Option<[u8; 2]>, l: Option<&[u8; 3]>| {
        qm.value_for_locale(title, c, l.map(pack))
            .unwrap()
            .as_text()
            .unwrap()
    };
    // German speaker in Italy → the DE/GB/FR/IT × deu/fra value.
    assert_eq!(pick(Some(*b"IT"), Some(b"deu")), "Titel");
    // English speaker in Germany: the list value needs deu/fra, the
    // language-only eng value matches.
    assert_eq!(pick(Some(*b"DE"), Some(b"eng")), "Title");
    // French speaker in Canada → the immediate CA/fra value.
    assert_eq!(pick(Some(*b"CA"), Some(b"fra")), "Titre");
    // Spanish speaker in Spain → nothing specific, the default.
    assert_eq!(pick(Some(*b"ES"), Some(b"spa")), "Default");
    // Country-only request: first value whose country half matches.
    assert_eq!(pick(Some(*b"FR"), None), "Titel");
    assert_eq!(pick(Some(*b"US"), None), "Title");
    // No constraints at all: the first (most specific) value.
    assert_eq!(pick(None, None), "Titel");
}

#[test]
fn flat_meta_view_is_unchanged_first_value_per_key() {
    let d = open(build(None));
    assert_eq!(d.meta.len(), 3);
    assert_eq!(d.meta[0].key, "com.apple.quicktime.title");
    assert_eq!(d.meta[0].type_code, META_TYPE_UTF8);
    assert_eq!(d.meta[0].as_str(), Some("Titel"));
    assert_eq!(d.meta[1].as_str(), Some("Album EU"));
    assert_eq!(d.meta[2].type_code, META_TYPE_BE_UNSIGNED_INT);
    assert_eq!(d.meta[2].value, vec![0x01, 0x02]);
    assert_eq!(d.qt_metadata.as_ref().unwrap().to_key_values(), d.meta);
}

#[test]
fn track_level_metadata_surfaces_on_the_track() {
    let track_items = vec![
        MovMetaItem::utf8("com.apple.quicktime.description", "cam A")
            .with_value(MovMetaValue::utf8("cam A (fr)").for_languages(&[*b"fra"])),
    ];
    let d = open(build(Some(&track_items)));
    let t = &d.tracks[0];
    let qm = t.qt_metadata.as_ref().expect("track qt_metadata");
    assert!(qm.country_lists.is_empty());
    assert!(
        qm.language_lists.is_empty(),
        "a single language is an immediate code, not a list"
    );
    let it = qm.item_named("com.apple.quicktime.description").unwrap();
    assert_eq!(it.values.len(), 2);
    assert_eq!(qm.language_tags_for(&it.values[1]), vec![*b"fra"]);
    assert_eq!(t.meta.len(), 1);
    assert_eq!(t.meta[0].as_str(), Some("cam A"));
    // Movie-level metadata is separate.
    assert_eq!(d.qt_metadata.as_ref().unwrap().items.len(), 3);
}

#[test]
fn write_is_deterministic_and_list_sets_are_shared() {
    let a = build(None);
    let b = build(None);
    assert_eq!(a, b);
    // Exactly one `ctry` and one `lang` atom in the file: both items
    // reference the same EU list.
    let count = |needle: &[u8]| a.windows(needle.len()).filter(|w| *w == needle).count();
    assert_eq!(count(b"ctry"), 1);
    assert_eq!(count(b"lang"), 1);
    assert_eq!(count(b"mhdr"), 1);
    assert_eq!(count(b"itif"), 2);
    assert_eq!(count(b"name"), 1);
}

fn ffprobe_available() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn ffprobe_still_reads_the_multi_locale_file() {
    if !ffprobe_available() {
        eprintln!("ffprobe not installed; skipping black-box check");
        return;
    }
    let bytes = build(None);
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "oxideav-mov-r461-qtmeta-{}.mov",
        std::process::id()
    ));
    std::fs::write(&path, &bytes).unwrap();
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format_tags", "-of", "json"])
        .arg(&path)
        .output()
        .expect("run ffprobe");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "ffprobe refused the file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = String::from_utf8_lossy(&out.stdout);
    // Every item is reported — including the ones carrying `itif` /
    // `name` children and several values — because the writer leads
    // each item with its `data` atoms.
    for key in [
        "com.apple.quicktime.title",
        "com.apple.quicktime.album",
        "com.example.rating",
    ] {
        assert!(json.contains(key), "ffprobe did not report {key}: {json}");
    }
}
