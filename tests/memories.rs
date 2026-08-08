//! Public-API tests for `exportsnap::export::memories`: the filename grammar, overlay pairing, the
//! day-and-kind join against `memories_history.json`, and what reaches the manifest.
//!
//! Nothing here reads a real export. Every filename is synthesized in the test, every directory is
//! a tempdir, and every manifest is opened with `open_in` so the per-user data dir is never
//! touched. The fixture tree is not used either: its `Media Type` values are all redacted, so no
//! fixture entry can exercise image-against-video routing at all.
//!
//! The shapes below mirror the observed export's SHAPE rather than its counts — a 1:1 bucket, an
//! n:n bucket, a bucket short of files, and a bucket with a file no entry claims — because n=1
//! makes this export's totals a hint and not a contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use exportsnap::export::manifest::{ExportId, ItemKind, ItemStatus, Manifest};
use exportsnap::export::memories::{
    Bucket, Day, Discovery, Duplicate, MemoryFile, MemoryKind, MemoryMedia, MissingReason, Pairing, Reconciliation, Role, UnreadableDir,
    discover, reconcile,
};
use exportsnap::export::model::{MediaKind, Memories};
use exportsnap::export::schema;
use tempfile::TempDir;

/// The 13-digit id shape the one observed export used.
const EXPORT_ID: &str = "1784667002819";

/// A distinct 36-character dashed uuid per `seed`, in the shape a memory filename carries.
fn uuid(seed: u32) -> String {
    format!("{seed:08x}-3ff7-45f1-95f9-a2fda6ba0f8e")
}

fn media_file(dir: &str, day: &str, seed: u32, role: &str, extension: &str) -> MemoryFile {
    let name = format!("{day}_{}-{role}.{extension}", uuid(seed));
    MemoryFile::parse(Path::new(dir).join(name)).expect("the synthesized name parses")
}

fn main_file(day: &str, seed: u32, extension: &str) -> MemoryFile {
    media_file("/export/memories", day, seed, "main", extension)
}

fn overlay_file(day: &str, seed: u32) -> MemoryFile {
    media_file("/export/memories", day, seed, "overlay", "png")
}

/// `memories_history.json` entries, built through the real schema-to-model path so the
/// reconciliation never sees a state the loader could not produce.
fn entries(rows: &[(&str, &str)]) -> Memories {
    let saved_media = rows
        .iter()
        .map(|(date, media_type)| schema::SavedMediaEntry {
            date: (*date).to_owned(),
            media_type: (*media_type).to_owned(),
            ..schema::SavedMediaEntry::default()
        })
        .collect();
    Memories::try_from(schema::MemoriesHistory { saved_media }).expect("the synthesized entries parse")
}

/// `YYYY-MM-DD` as a whole `Date` value, with a time nothing in the join reads.
fn at(day: &str) -> String {
    format!("{day} 12:41:51 UTC")
}

fn reconciled(rows: &[(&str, &str)], files: Vec<MemoryFile>) -> Reconciliation {
    reconcile(&entries(rows), Discovery::from_files(files, Vec::new()))
}

fn source_ids(reconciliation: &Reconciliation) -> Vec<&str> {
    reconciliation.items.iter().map(|item| item.source_id.as_str()).collect()
}

// ---- the filename grammar ----

#[test]
fn a_main_and_an_overlay_filename_parse_into_their_parts() {
    let main = MemoryFile::parse("/export/memories/2020-07-28_2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e-main.mp4").unwrap();
    assert_eq!(main.day, Day::parse("2020-07-28").unwrap());
    assert_eq!(main.uuid, "2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e");
    assert_eq!(main.role.as_suffix(), "main");
    assert_eq!(main.extension, "mp4");
    assert_eq!(main.path, PathBuf::from("/export/memories/2020-07-28_2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e-main.mp4"));

    let overlay = MemoryFile::parse("/export/memories/2020-07-28_2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e-overlay.png").unwrap();
    assert_eq!(overlay.role.as_suffix(), "overlay");
    assert_eq!(overlay.uuid, main.uuid, "a main and its overlay share the id");
}

#[test]
fn the_extension_decides_the_kind_and_survives_verbatim() {
    assert_eq!(MemoryKind::from_extension("mp4"), MemoryKind::Video);
    assert_eq!(MemoryKind::from_extension("MP4"), MemoryKind::Video);
    assert_eq!(MemoryKind::from_extension("jpg"), MemoryKind::Image);
    assert_eq!(MemoryKind::from_extension("png"), MemoryKind::Image);
    // Not dropped and not guessed at: an extension this build cannot place still yields a file.
    assert_eq!(MemoryKind::from_extension("heic"), MemoryKind::Unknown);

    let odd = main_file("2020-07-28", 1, "HEIC");
    assert_eq!(odd.extension, "HEIC", "reported as it is on disk, not as a bucket verdict");
    assert_eq!(MemoryKind::from_extension(&odd.extension), MemoryKind::Unknown);
}

#[test]
fn an_entry_word_decides_the_other_side_of_the_key() {
    assert_eq!(MemoryKind::from_media_type(&MediaKind::Image), MemoryKind::Image);
    assert_eq!(MemoryKind::from_media_type(&MediaKind::Video), MemoryKind::Video);
    // Chat and snap words name no memory, so none of them can key a memory bucket.
    for word in [MediaKind::Text, MediaKind::Media, MediaKind::Status, MediaKind::Note, MediaKind::Sticker] {
        assert_eq!(MemoryKind::from_media_type(&word), MemoryKind::Unknown, "{word:?}");
    }
    assert_eq!(MemoryKind::from_media_type(&MediaKind::Other("SHARE".to_owned())), MemoryKind::Unknown);
}

#[test]
fn every_shape_the_filename_grammar_rejects_stays_unparsed() {
    let id = "2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e";
    for name in [
        // The one observed rejection: every memories dir holds this index file.
        "memories.html".to_owned(),
        format!("2020-07-28_{id}-thumbnail.jpg"),
        format!("2020-07-28_{id}.jpg"),
        format!("2020-07-28-{id}-main.jpg"),
        format!("2020-7-28_{id}-main.jpg"),
        format!("2020-13-28_{id}-main.jpg"),
        format!("2020-07-32_{id}-main.jpg"),
        format!("2020-07-28_{}-main.jpg", &id[..35]),
        format!("2020-07-28_{}a-main.jpg", id),
        format!("2020-07-28_{}-main", id),
        "2020-07-28_holiday-main.jpg".to_owned(),
        String::new(),
    ] {
        assert!(MemoryFile::parse(Path::new("/export/memories").join(&name)).is_none(), "{name:?} should not parse");
    }
}

#[test]
fn the_role_is_matched_without_regard_to_case() {
    let shouted = MemoryFile::parse("/export/memories/2020-07-28_2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e-MAIN.MP4").unwrap();
    assert_eq!(shouted.role.as_suffix(), "main");
    assert_eq!(shouted.extension, "MP4");
}

#[test]
fn every_role_is_named_in_all() {
    // Second witness; `Role::as_suffix` and the main/overlay bucketing match
    // (src/export/memories.rs) are the first. Survives either being weakened to a wildcard.
    // Residual and rationale: the `MissingReason::ALL` witness in
    // `a_missing_reason_says_which_gap_it_is_in_prose_the_manifest_can_store`. Never collapse to
    // `_ => {}`.
    for role in Role::ALL {
        match role {
            Role::Main | Role::Overlay => {}
        }
    }
}

// ---- overlay pairing ----

#[test]
fn a_main_and_its_overlay_pair_on_the_shared_uuid() {
    let discovery = Discovery::from_files(vec![overlay_file("2020-07-28", 1), main_file("2020-07-28", 1, "jpg")], Vec::new());

    assert_eq!(discovery.media.len(), 1);
    assert_eq!(discovery.media[0].uuid(), uuid(1));
    assert_eq!(discovery.media[0].overlay.as_ref().map(|file| file.path.clone()), Some(overlay_file("2020-07-28", 1).path));
    assert!(discovery.orphan_overlays.is_empty());
}

#[test]
fn a_main_with_no_overlay_is_ordinary_and_an_overlay_with_no_main_is_reported() {
    let discovery = Discovery::from_files(vec![main_file("2020-07-28", 1, "mp4"), overlay_file("2020-07-29", 2)], Vec::new());

    assert_eq!(discovery.media.len(), 1);
    assert_eq!(discovery.media[0].overlay, None, "an overlay-less memory is the common case");
    assert_eq!(discovery.orphan_overlays.len(), 1);
    assert_eq!(discovery.orphan_overlays[0].uuid, uuid(2));
}

#[test]
fn two_files_claiming_one_memory_and_role_are_reported_rather_than_deduped() {
    let first = media_file("/export/a/memories", "2020-07-28", 1, "main", "mp4");
    let second = media_file("/export/b/memories", "2020-07-28", 1, "main", "mp4");
    let discovery = Discovery::from_files(vec![second.clone(), first.clone()], Vec::new());

    assert_eq!(discovery.media.len(), 1, "one memory, however many copies of its file exist");
    assert_eq!(discovery.duplicates.len(), 1);
    assert_eq!(discovery.duplicates[0].uuid, uuid(1));
    assert_eq!(discovery.duplicates[0].kept, first.path);
    assert_eq!(discovery.duplicates[0].ignored, vec![second.path]);
    assert_eq!(discovery.media[0].main.path, first.path, "the pairing used the file the report names");
}

#[test]
fn a_main_and_its_overlay_in_different_dirs_pair_and_are_not_a_duplicate() {
    let main = media_file("/export/a/memories", "2020-07-28", 1, "main", "mp4");
    let overlay = media_file("/export/b/memories", "2020-07-28", 1, "overlay", "png");
    let discovery = Discovery::from_files(vec![main, overlay], Vec::new());

    assert_eq!(discovery.media.len(), 1);
    assert!(discovery.media[0].overlay.is_some());
    assert!(discovery.duplicates.is_empty());
}

#[test]
fn pairing_does_not_depend_on_the_order_the_walk_found_the_files() {
    // The duplicate is what makes the order observable at all: with none, the map the pairing is
    // built from already imposes uuid order whatever the input did.
    let files = vec![
        main_file("2020-07-28", 3, "mp4"),
        overlay_file("2020-07-28", 3),
        media_file("/export/b/memories", "2020-07-28", 1, "main", "mp4"),
        media_file("/export/a/memories", "2020-07-28", 1, "main", "mp4"),
        main_file("2020-07-28", 2, "mp4"),
    ];
    let mut reversed = files.clone();
    reversed.reverse();

    let forwards = Discovery::from_files(files, Vec::new());
    let backwards = Discovery::from_files(reversed, Vec::new());
    assert_eq!(forwards, backwards, "read_dir order must not decide which entry gets which file");
    assert_eq!(forwards.duplicates.len(), 1);
    assert!(forwards.duplicates[0].kept.starts_with("/export/a"), "{:?}", forwards.duplicates[0].kept);

    // And the order it settles on is the one an ambiguous bucket hands out in.
    let uuids: Vec<&str> = forwards.media.iter().map(MemoryMedia::uuid).collect();
    assert_eq!(uuids, [uuid(1), uuid(2), uuid(3)]);
}

// ---- the walk ----

#[test]
fn discovery_reads_every_memories_dir_at_any_depth_and_nothing_beside_them() {
    let root = TempDir::new().unwrap();
    let name = |seed: u32| format!("2020-07-28_{}-main.mp4", uuid(seed));

    for (dir, seed) in [("memories", 1), ("mydata~1784667002819-3/memories", 2), ("memories (1)/memories", 3), ("a/b/c/memories", 4)] {
        let dir = root.path().join(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name(seed)), b"x").unwrap();
        fs::write(dir.join("memories.html"), b"<html>").unwrap();
    }
    // A media file with a memory-shaped name outside a `memories` dir belongs to another pipeline.
    let chat_media = root.path().join("chat_media");
    fs::create_dir_all(&chat_media).unwrap();
    fs::write(chat_media.join(name(9)), b"x").unwrap();

    let discovery = discover(root.path()).unwrap();

    let found: Vec<&str> = discovery.media.iter().map(MemoryMedia::uuid).collect();
    assert_eq!(found, [uuid(1), uuid(2), uuid(3), uuid(4)], "the dir NAME is what gates discovery, not the depth");
    assert_eq!(discovery.unparsed.len(), 4, "one memories.html per dir, carried rather than dropped");
    assert!(discovery.unparsed.iter().all(|path| path.ends_with("memories.html")), "{:?}", discovery.unparsed);
    assert!(discovery.duplicates.is_empty());
    assert!(discovery.orphan_overlays.is_empty());
}

#[test]
fn a_root_that_cannot_be_listed_names_itself_and_says_what_to_do() {
    let root = TempDir::new().unwrap();
    let missing = root.path().join("not-here");

    let error = discover(&missing).unwrap_err();
    assert_eq!(error.dir, missing);
    let rendered = error.to_string();
    assert!(rendered.contains(&missing.display().to_string()), "{rendered}");
    assert!(rendered.contains("memories dirs"), "{rendered}");
}

/// A source root on a real mount carries dirs this user cannot read and the export has nothing to
/// do with — `lost+found` is 0700 and root-owned on every ext4 mount. Aborting the scan over one of
/// those would report zero memories for an export that is entirely fine.
#[cfg(unix)]
#[test]
fn a_directory_the_walk_cannot_list_is_reported_and_the_rest_of_the_scan_survives() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().unwrap();
    let memories = root.path().join("memories");
    fs::create_dir_all(&memories).unwrap();
    fs::write(memories.join(format!("2020-07-28_{}-main.mp4", uuid(1))), b"x").unwrap();

    let locked = root.path().join("lost+found");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        // Running as root, or on a filesystem that ignores mode bits: the probe cannot be armed,
        // and asserting anyway would pin the harness rather than the walk.
        println!("skipping: this user can read a 0000 directory, so the skip path is unreachable here");
        return;
    }

    let discovery = discover(root.path()).unwrap();

    assert_eq!(discovery.media.len(), 1, "the memory that WAS found still comes back");
    assert_eq!(discovery.unreadable.len(), 1);
    assert_eq!(discovery.unreadable[0].dir, locked);
    assert_eq!(discovery.unreadable[0].kind, io::ErrorKind::PermissionDenied);

    // Restore the mode so the tempdir can clean itself up.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
}

/// The walk's symlink rule is a mechanism claim whose counterexample compiles: swapping
/// `entry.file_type()` for `Path::is_dir` re-enters a link that points at its own ancestor.
///
/// **What that costs is not a hang.** The kernel stops resolving a link chain at `MAXSYMLINKS`, so
/// the walk gives up around forty re-entries in and still returns — measured: ELOOP at 41
/// components, 603 characters against a 4096 `PATH_MAX`, so it is the link count that bounds it and
/// not path length. It finishes in under a millisecond, which is why neither wall-clock nor a
/// timeout can carry this test. What it leaves behind is the same memory discovered once per
/// re-entry at forty-odd different paths, i.e. `duplicates`, which is the assertion that does carry
/// it. Counting `unparsed` cannot: the deepest re-entry is itself the one that fails, so it lands
/// its own `loop` there and the count is 1 either way — only the PATH tells the two apart.
#[cfg(unix)]
#[test]
fn a_symlink_loop_does_not_make_the_walk_re_enter_itself() {
    let root = TempDir::new().unwrap();
    let memories = root.path().join("memories");
    fs::create_dir_all(&memories).unwrap();
    fs::write(memories.join(format!("2020-07-28_{}-main.mp4", uuid(1))), b"x").unwrap();
    // A link straight back to the root, sitting inside the dir the walk is about to read.
    std::os::unix::fs::symlink(root.path(), memories.join("loop")).unwrap();

    let discovery = discover(root.path()).unwrap();

    assert_eq!(discovery.media.len(), 1);
    assert!(discovery.duplicates.is_empty(), "one memory found once, not once per re-entry: {:?}", discovery.duplicates);
    // The link is not a dir to descend and not a memory filename, so it lands here — and at the
    // SHALLOW path, which is what says the walk never went through it.
    assert_eq!(discovery.unparsed, vec![memories.join("loop")]);
    assert!(discovery.unreadable.is_empty());
}

// ---- the join ----

#[test]
fn a_bucket_holding_one_entry_and_one_media_set_pairs_exactly() {
    let reconciliation = reconciled(&[(&at("2020-07-28"), "Image")], vec![main_file("2020-07-28", 1, "jpg")]);

    assert_eq!(reconciliation.items.len(), 1);
    assert!(matches!(reconciliation.items[0].pairing, Pairing::Exact(_)), "{:?}", reconciliation.items[0].pairing);
    assert_eq!(source_ids(&reconciliation), [uuid(1)], "a paired item is identified by its media's uuid");
    assert!(reconciliation.files_without_entry.is_empty());
}

#[test]
fn a_bucket_holding_several_of_each_pairs_ambiguously() {
    let reconciliation = reconciled(
        &[(&at("2020-07-28"), "Video"), (&at("2020-07-28"), "Video")],
        vec![main_file("2020-07-28", 1, "mp4"), main_file("2020-07-28", 2, "mp4")],
    );

    let pairings: Vec<bool> = reconciliation.items.iter().map(|item| matches!(item.pairing, Pairing::Ambiguous(_))).collect();
    assert_eq!(pairings, [true, true], "two of each: which entry got which file is a guess");
    assert_eq!(source_ids(&reconciliation), [uuid(1), uuid(2)]);
    assert!(reconciliation.files_without_entry.is_empty());
}

#[test]
fn a_bucket_short_of_media_pairs_ambiguously_and_leaves_the_surplus_entries_missing() {
    let reconciliation = reconciled(
        &[(&at("2020-07-28"), "Image"), (&at("2020-07-28"), "Image"), (&at("2020-07-28"), "Image")],
        vec![main_file("2020-07-28", 1, "jpg")],
    );

    // The one file could have belonged to any of the three, so even the pairing that happened is
    // not exact.
    assert!(matches!(reconciliation.items[0].pairing, Pairing::Ambiguous(_)), "{:?}", reconciliation.items[0].pairing);
    assert_eq!(reconciliation.items[1].pairing, Pairing::Missing(MissingReason::NoMedia));
    assert_eq!(reconciliation.items[2].pairing, Pairing::Missing(MissingReason::NoMedia));
    assert_eq!(source_ids(&reconciliation), [&uuid(1), "unpaired-entry-1", "unpaired-entry-2"]);
}

#[test]
fn media_no_entry_claimed_is_reported_rather_than_dropped() {
    let reconciliation =
        reconciled(&[(&at("2020-07-28"), "Video")], vec![main_file("2020-07-28", 1, "mp4"), main_file("2020-07-28", 2, "mp4")]);

    assert!(matches!(reconciliation.items[0].pairing, Pairing::Ambiguous(_)));
    assert_eq!(reconciliation.files_without_entry.len(), 1);
    assert_eq!(reconciliation.files_without_entry[0].uuid(), uuid(2));
}

#[test]
fn an_entry_and_a_file_of_different_kinds_on_one_day_do_not_pair() {
    // Same day, and keying on the day alone would pair these two. The kind is what stops it: the
    // export's own word says image and the file on disk is a video.
    let reconciliation = reconciled(&[(&at("2020-07-28"), "Image")], vec![main_file("2020-07-28", 1, "mp4")]);

    assert_eq!(reconciliation.items[0].pairing, Pairing::Missing(MissingReason::NoMedia));
    assert_eq!(reconciliation.files_without_entry.len(), 1);
    assert_eq!(reconciliation.files_without_entry[0].bucket(), Bucket { day: Day::parse("2020-07-28").unwrap(), kind: MemoryKind::Video });
}

#[test]
fn one_day_with_both_kinds_pairs_each_side_within_its_own_bucket() {
    let reconciliation = reconciled(
        &[(&at("2020-07-28"), "Video"), (&at("2020-07-28"), "Image")],
        vec![main_file("2020-07-28", 1, "jpg"), main_file("2020-07-28", 2, "mp4")],
    );

    assert_eq!(reconciliation.items[0].media().map(MemoryMedia::uuid), Some(uuid(2).as_str()), "the video entry took the mp4");
    assert_eq!(reconciliation.items[1].media().map(MemoryMedia::uuid), Some(uuid(1).as_str()), "the image entry took the jpg");
    // Each bucket held one of each, so neither pairing is a guess.
    assert!(reconciliation.items.iter().all(|item| matches!(item.pairing, Pairing::Exact(_))));
}

#[test]
fn an_entry_with_no_date_is_source_missing_rather_than_bucketed() {
    let reconciliation = reconciled(&[("", "Image"), (&at("2020-07-28"), "Image")], vec![main_file("2020-07-28", 1, "jpg")]);

    assert_eq!(reconciliation.items[0].pairing, Pairing::Missing(MissingReason::NoDate));
    assert_eq!(reconciliation.items[0].source_id, "unpaired-entry-0");
    // And it does not eat the media the dated entry on that day is owed.
    assert!(matches!(reconciliation.items[1].pairing, Pairing::Exact(_)), "{:?}", reconciliation.items[1].pairing);
}

#[test]
fn an_entry_whose_word_names_no_memory_buckets_as_unknown() {
    let reconciliation = reconciled(&[(&at("2020-07-28"), "Sticker")], vec![main_file("2020-07-28", 1, "jpg")]);

    assert_eq!(reconciliation.items[0].pairing, Pairing::Missing(MissingReason::NoMedia));
    assert_eq!(reconciliation.files_without_entry.len(), 1);
}

/// A dir the walk could not list means the run cannot say media does not exist — it can only say it
/// never saw any. `SourceMissing` is never handed back as work, so the verdict is durable and has to
/// stop short of the claim.
#[test]
fn an_incomplete_scan_reports_unscanned_rather_than_asserting_no_media_exists() {
    let entries = entries(&[(&at("2020-07-28"), "Image")]);
    let files = Vec::new();
    let locked = vec![UnreadableDir { dir: PathBuf::from("/export/mydata~1/memories"), kind: io::ErrorKind::PermissionDenied }];

    let complete = reconcile(&entries, Discovery::from_files(files.clone(), Vec::new()));
    assert_eq!(complete.items[0].pairing, Pairing::Missing(MissingReason::NoMedia), "a complete scan can assert absence");

    let incomplete = reconcile(&entries, Discovery::from_walk(files, Vec::new(), locked));
    assert_eq!(incomplete.items[0].pairing, Pairing::Missing(MissingReason::Unscanned));
    assert!(incomplete.report().unreadable_dirs > 0, "and the report says why the verdict is qualified");
}

/// `NoDate` is a fact about the entry, not about the filesystem: it pairs with nothing however much
/// of the source was read, so an unreadable dir must not relabel it and send a reader off to fix
/// permissions that would have changed nothing.
#[test]
fn an_undated_entry_stays_undated_even_when_the_scan_was_incomplete() {
    let entries = entries(&[("", "Image")]);
    let locked = vec![UnreadableDir { dir: PathBuf::from("/export/locked"), kind: io::ErrorKind::PermissionDenied }];

    let reconciliation = reconcile(&entries, Discovery::from_walk(Vec::new(), Vec::new(), locked));

    assert_eq!(reconciliation.items[0].pairing, Pairing::Missing(MissingReason::NoDate));
}

#[test]
fn an_unreadable_dir_renders_for_a_reader_rather_than_as_a_debug_dump() {
    let unreadable = UnreadableDir { dir: PathBuf::from("/export/lost+found"), kind: io::ErrorKind::PermissionDenied };
    let rendered = unreadable.to_string();

    assert!(rendered.contains("/export/lost+found"), "{rendered}");
    assert!(rendered.contains("permission denied"), "{rendered}");
    assert!(!rendered.contains("PermissionDenied"), "the ErrorKind's Debug spelling is not prose: {rendered}");
}

#[test]
fn a_missing_reason_says_which_gap_it_is_in_prose_the_manifest_can_store() {
    // Driven off `ALL` rather than a literal list: a variant this file names itself is one a fourth
    // variant slips past, which is the hand-listed-set shape that already cost a counter collision
    // one layer up in `the_report_counts_every_side_of_the_join`.
    for reason in MissingReason::ALL {
        // Exhaustiveness witness. `ALL` alone cannot make a new variant loud — an array literal's
        // length is independent of the enum's variant count, so a four-variant enum with a
        // three-element `ALL` compiles clean and this loop would just skip the new one. This match
        // is a compile error the moment a variant is added, and a test binary that will not build
        // is a red suite, which is the guarantee `ALL` is missing.
        //
        // What it catches is the ADDITION, not the omission: an author can add an arm here and
        // still leave `ALL` short. That residual is real and is why this comment says so rather
        // than claiming the set is policed. Never collapse the arms to `_ => {}`.
        match reason {
            MissingReason::NoMedia | MissingReason::NoDate | MissingReason::Unscanned => {}
        }
        let rendered = reason.to_string();
        // The manifest replaces any token holding url punctuation or running past 64 characters,
        // so a reason spelled with either would reach `last_error` as `<redacted>`.
        assert!(rendered.split_whitespace().all(|token| token.len() <= 64 && !token.contains(['/', '=', '%', '&', '@'])), "{rendered}");
    }
    let spellings = MissingReason::ALL.map(|reason| reason.to_string());
    assert_eq!(BTreeSet::from(spellings.clone()).len(), spellings.len(), "each reason reads differently: {spellings:?}");
}

/// Every dimension gets its OWN cardinality, because a counter sharing a value with another counter
/// cannot tell the two apart under a swap. All ten, and no two alike: 8 entries, 7 files, 1 exact,
/// 2 ambiguous, 5 missing, 4 unclaimed, 6 unparsed, 3 orphan overlays, 9 duplicate files,
/// 10 unreadable dirs. The two derived values check out against the rest — 8 = 1 + 2 + 5 entries,
/// 7 = 1 + 2 + 4 main files — so a swap cannot hide inside the arithmetic either.
///
/// This enumeration is load-bearing: the first attempt at it left ambiguous and missing both on 2,
/// and swapping their arms in `report` stayed green.
#[test]
fn the_report_counts_every_side_of_the_join() {
    let mut files = vec![
        // A 1:1 bucket, with an overlay.
        main_file("2020-07-28", 1, "jpg"),
        overlay_file("2020-07-28", 1),
        // An n:n bucket.
        main_file("2020-07-29", 2, "mp4"),
        main_file("2020-07-29", 3, "mp4"),
    ];
    // Four mains on a day no entry names at all.
    files.extend((10..14).map(|seed| main_file("2020-07-31", seed, "mp4")));
    // Three overlays whose mains are nowhere.
    files.extend((20..23).map(|seed| overlay_file("2020-08-01", seed)));

    let rows = [
        (at("2020-07-28"), "Image"),
        (at("2020-07-29"), "Video"),
        (at("2020-07-29"), "Video"),
        // A bucket short of files entirely, five times over.
        (at("2020-07-30"), "Image"),
        (at("2020-07-30"), "Image"),
        (at("2020-07-30"), "Image"),
        (at("2020-07-30"), "Image"),
        (at("2020-07-30"), "Image"),
    ];
    let rows: Vec<(&str, &str)> = rows.iter().map(|(date, word)| (date.as_str(), *word)).collect();

    let unparsed: Vec<PathBuf> = (0..6).map(|n| PathBuf::from(format!("/export/memories/index-{n}.html"))).collect();
    let unreadable: Vec<UnreadableDir> = (0..10)
        .map(|n| UnreadableDir { dir: PathBuf::from(format!("/export/locked-{n}")), kind: io::ErrorKind::PermissionDenied })
        .collect();

    let mut discovery = Discovery::from_walk(files, unparsed, unreadable);
    // Two duplicate records holding nine ignored files between them, so the counter cannot be
    // reading the record count.
    discovery.duplicates.push(Duplicate {
        uuid: uuid(7),
        role: Role::Main,
        kept: PathBuf::from("/export/a/memories/kept.mp4"),
        ignored: (0..6).map(|n| PathBuf::from(format!("/export/b/memories/ignored-{n}.mp4"))).collect(),
    });
    discovery.duplicates.push(Duplicate {
        uuid: uuid(8),
        role: Role::Overlay,
        kept: PathBuf::from("/export/a/memories/kept.png"),
        ignored: (0..3).map(|n| PathBuf::from(format!("/export/b/memories/ignored-{n}.png"))).collect(),
    });
    let report = reconcile(&entries(&rows), discovery).report();

    assert_eq!(report.entries, 8);
    assert_eq!(report.files, 7, "main files only: the overlays are not items");
    assert_eq!(report.paired_exact, 1);
    assert_eq!(report.paired_ambiguous, 2);
    assert_eq!(report.source_missing, 5);
    assert_eq!(report.files_without_entry, 4);
    assert_eq!(report.unparsed_names, 6);
    assert_eq!(report.orphan_overlays, 3);
    assert_eq!(report.duplicate_files, 9, "files set aside, not duplicate records and not uuids");
    assert_eq!(report.unreadable_dirs, 10);
    assert_eq!(report.paired_exact + report.paired_ambiguous + report.source_missing, report.entries);
}

// ---- the manifest ----

struct Workspace {
    dir: TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self { dir: TempDir::new().unwrap() }
    }

    fn open(&self) -> Manifest {
        Manifest::open_in(self.dir.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap()
    }
}

fn status_of(manifest: &Manifest, source_id: &str) -> Option<ItemStatus> {
    manifest.item(ItemKind::Memory, source_id).unwrap().map(|item| item.status)
}

#[test]
fn enrollment_gives_every_entry_a_row_and_marks_the_unpaired_ones_missing() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let reconciliation = reconciled(
        &[(&at("2020-07-28"), "Image"), (&at("2020-07-29"), "Image"), (&at("2020-07-29"), "Image")],
        vec![main_file("2020-07-28", 1, "jpg"), main_file("2020-07-29", 2, "jpg")],
    );

    reconciliation.enroll(&mut manifest).unwrap();

    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::Pending));
    assert_eq!(status_of(&manifest, &uuid(2)), Some(ItemStatus::Pending));
    assert_eq!(status_of(&manifest, "unpaired-entry-2"), Some(ItemStatus::SourceMissing), "a row per entry, never a bare count");

    let report = manifest.resume(ItemKind::Memory).unwrap();
    assert_eq!(report.source_missing, 1);
    assert_eq!(report.pending, 2);
    // The gap is never handed back as work, so a run cannot spin on it.
    let owed: Vec<String> = manifest.pending(ItemKind::Memory, 3).unwrap().into_iter().map(|item| item.source_id).collect();
    assert_eq!(owed, [uuid(1), uuid(2)]);

    // Re-running the same export changes nothing.
    reconciliation.enroll(&mut manifest).unwrap();
    assert_eq!(status_of(&manifest, "unpaired-entry-2"), Some(ItemStatus::SourceMissing));
    assert_eq!(manifest.resume(ItemKind::Memory).unwrap().source_missing, 1);
}

#[test]
fn re_enrolling_leaves_a_finished_item_alone_even_when_its_media_is_gone() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let paired = reconciled(&[(&at("2020-07-28"), "Image")], vec![main_file("2020-07-28", 1, "jpg")]);
    paired.enroll(&mut manifest).unwrap();

    let output = workspace.dir.path().join("2020-07-28.jpg");
    fs::write(&output, b"repaired bytes").unwrap();
    manifest.mark_done(ItemKind::Memory, &uuid(1), &output).unwrap();

    // The source file disappears — a part unmounted, a dir moved — and the export is re-scanned.
    let vanished = reconciled(&[(&at("2020-07-28"), "Image")], Vec::new());
    vanished.enroll(&mut manifest).unwrap();

    // The unpaired entry now carries a synthetic id, so it enrolls a row of its own and the uuid
    // row is never named again. What holds it at `Done` is the sweep's exemption, since the uuid row
    // is exactly the kind of unnamed row `a_row_whose_identity_changed_between_runs_is_retired`
    // watches get retired.
    //
    // **The STATUS assertion is the one sensitive to that exemption; the two under it are not.** A
    // parked row keeps its output record too, so a retired row would satisfy them just as well —
    // they say the record survived, not that the exemption is what saved it. Keeping them anyway,
    // because "the sweep left the whole row alone" is the claim, and the checksum only means
    // re-verified while the status is `Done`.
    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::Done), "finished bytes stay finished");
    assert_eq!(status_of(&manifest, "unpaired-entry-0"), Some(ItemStatus::SourceMissing));
    let item = manifest.item(ItemKind::Memory, &uuid(1)).unwrap().unwrap();
    assert_eq!(item.output_path, Some(output));
    assert!(item.checksum.is_some(), "and the record of what that run wrote survives with it");
}

#[test]
fn an_entry_whose_media_turned_up_goes_back_on_the_work_list() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    // First run: the part holding the media had not been extracted yet. This is served by the
    // NEW-ROW path, not by `Manifest::reset` — the entry's `source_id` changes from the synthetic
    // one to the media's uuid, so the second run enrols a fresh `Pending` row and the reset arm of
    // `enroll` is never taken. `a_source_missing_item_that_paired_again_is_reset_rather_than_left_
    // parked` owns that arm.
    reconciled(&[(&at("2020-07-28"), "Image")], Vec::new()).enroll(&mut manifest).unwrap();
    assert_eq!(status_of(&manifest, "unpaired-entry-0"), Some(ItemStatus::SourceMissing));

    // Second run, with the part extracted. The entry now pairs, under its media's uuid.
    reconciled(&[(&at("2020-07-28"), "Image")], vec![main_file("2020-07-28", 1, "jpg")]).enroll(&mut manifest).unwrap();

    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::Pending));
    let owed: Vec<String> = manifest.pending(ItemKind::Memory, 3).unwrap().into_iter().map(|item| item.source_id).collect();
    assert_eq!(owed, [uuid(1)], "the found media is work again, and the stale synthetic row is not");
    // The observed export's worst case in miniature: a first run before extraction gives every entry
    // a synthetic gap row, and the second run has to retire the ones that paired or the gap count
    // stays at the entry count for ever.
    assert_eq!(status_of(&manifest, "unpaired-entry-0"), Some(ItemStatus::Retired));
    assert_eq!(manifest.resume(ItemKind::Memory).unwrap().source_missing, 0, "the gap the second run really has");
}

/// The positive twin of `re_enrolling_leaves_a_finished_item_alone_even_when_its_media_is_gone`:
/// that one owns the `Missing` arm of `enroll`, this one owns the `Exact | Ambiguous` arm, and
/// between them they say a second run of a finished export re-downloads nothing.
#[test]
fn a_second_run_of_a_finished_export_leaves_every_done_row_alone() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let row = [(at("2020-07-28"), "Image")];
    let row: Vec<(&str, &str)> = row.iter().map(|(date, word)| (date.as_str(), *word)).collect();
    let paired = reconciled(&row, vec![main_file("2020-07-28", 1, "jpg")]);

    paired.enroll(&mut manifest).unwrap();
    let output = workspace.dir.path().join("2020-07-28.jpg");
    fs::write(&output, b"repaired bytes").unwrap();
    manifest.mark_done(ItemKind::Memory, &uuid(1), &output).unwrap();
    let finished = manifest.item(ItemKind::Memory, &uuid(1)).unwrap().unwrap();

    // The ordinary resume: same export, same media, everything still paired.
    paired.enroll(&mut manifest).unwrap();

    let after = manifest.item(ItemKind::Memory, &uuid(1)).unwrap().unwrap();
    assert_eq!(after.status, ItemStatus::Done, "a re-enroll must not un-finish work");
    assert_eq!(after.output_path, finished.output_path);
    assert_eq!(after.checksum, finished.checksum, "the checksum the resume sweep compares against");
    assert_eq!(after.bytes, finished.bytes);
    assert!(manifest.pending(ItemKind::Memory, 3).unwrap().is_empty(), "and it is not offered as work again");
    assert_eq!(manifest.resume(ItemKind::Memory).unwrap().verified, 1);
}

#[test]
fn an_entry_enrolls_the_url_the_export_carried_for_it() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let saved_media = vec![
        schema::SavedMediaEntry {
            date: at("2020-07-28"),
            media_type: "Image".to_owned(),
            download_link: "https://sc.example/landing?id=1".to_owned(),
            media_download_url: "https://cf-st.example/d/media?sig=DIRECT".to_owned(),
            ..schema::SavedMediaEntry::default()
        },
        schema::SavedMediaEntry {
            date: at("2020-07-29"),
            media_type: "Image".to_owned(),
            download_link: "https://sc.example/landing?id=2".to_owned(),
            ..schema::SavedMediaEntry::default()
        },
        schema::SavedMediaEntry { date: at("2020-07-30"), media_type: "Image".to_owned(), ..schema::SavedMediaEntry::default() },
    ];
    let memories = Memories::try_from(schema::MemoriesHistory { saved_media }).unwrap();
    let files = vec![main_file("2020-07-28", 1, "jpg"), main_file("2020-07-29", 2, "jpg"), main_file("2020-07-30", 3, "jpg")];

    reconcile(&memories, Discovery::from_files(files, Vec::new())).enroll(&mut manifest).unwrap();

    let url_of = |source_id: &str| manifest.item(ItemKind::Memory, source_id).unwrap().unwrap().url.map(|url| url.expose().to_owned());
    // `Media Download Url` wins where both are present; which of the two a downloader should use
    // is unsettled, and the manifest holds one.
    assert_eq!(url_of(&uuid(1)).as_deref(), Some("https://cf-st.example/d/media?sig=DIRECT"));
    assert_eq!(url_of(&uuid(2)).as_deref(), Some("https://sc.example/landing?id=2"));
    // Every url in the one observed export is `""`, which is absence rather than a value.
    assert_eq!(url_of(&uuid(3)), None);
}

/// A memory whose media went away between two runs re-enrolls under a synthetic id, and the uuid row
/// it leaves behind is retired: this run's enumeration cannot name it at all, so no run could ever
/// finish it.
///
/// **The finished row beside it is the exemption, in the same fixture.** It is unnamed by exactly the
/// same enumeration, so a sweep that took every unnamed row would red here rather than one file away.
#[test]
fn a_row_whose_identity_changed_between_runs_is_retired() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let rows = [(at("2020-07-28"), "Image"), (at("2020-07-29"), "Image")];
    let rows: Vec<(&str, &str)> = rows.iter().map(|(date, word)| (date.as_str(), *word)).collect();

    reconciled(&rows, vec![main_file("2020-07-28", 1, "jpg"), main_file("2020-07-29", 2, "jpg")]).enroll(&mut manifest).unwrap();
    let output = workspace.dir.path().join("2020-07-29.jpg");
    fs::write(&output, b"repaired bytes").unwrap();
    manifest.mark_done(ItemKind::Memory, &uuid(2), &output).unwrap();

    // Both files go away: each entry re-enrolls under a synthetic id and neither uuid is named again.
    reconciled(&rows, Vec::new()).enroll(&mut manifest).unwrap();

    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::Retired), "the row no entry answers for any more");
    assert_eq!(status_of(&manifest, &uuid(2)), Some(ItemStatus::Done), "finished bytes are not swept away with it");

    let report = manifest.resume(ItemKind::Memory).unwrap();
    assert_eq!(report.retired, 1);
    assert_eq!(report.source_missing, 2, "one row per entry, and the retired uuid is not one of them");
    assert_eq!(report.verified, 1);
    assert!(manifest.pending(ItemKind::Memory, 3).unwrap().is_empty(), "and no run is offered work it cannot finish");
}

/// The guard, in this leg: a dir the walk could not list is not evidence the media is gone, so the
/// row this run cannot name SURVIVES rather than being retired. Without it an export sitting next to
/// one unreadable directory would retire rows for media that is merely unseen.
#[test]
fn a_memory_whose_media_vanished_keeps_its_row_while_a_directory_could_not_be_listed() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let row = [(at("2020-07-28"), "Image")];
    let row: Vec<(&str, &str)> = row.iter().map(|(date, word)| (date.as_str(), *word)).collect();

    reconciled(&row, vec![main_file("2020-07-28", 1, "jpg")]).enroll(&mut manifest).unwrap();

    // Second run: no media paired AND part of the source could not be listed, so "the media is gone"
    // is a claim this run never established.
    let locked = vec![UnreadableDir { dir: PathBuf::from("/export/mydata~1/memories"), kind: io::ErrorKind::PermissionDenied }];
    let unscanned = reconcile(&entries(&row), Discovery::from_walk(Vec::new(), Vec::new(), locked));
    assert_eq!(unscanned.items[0].pairing, Pairing::Missing(MissingReason::Unscanned), "the fixture is the unscanned case");
    unscanned.enroll(&mut manifest).unwrap();

    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::Pending), "media merely never seen must not retire its row");
    assert_eq!(manifest.resume(ItemKind::Memory).unwrap().retired, 0);

    // The same enumeration with the scan complete retires it, so it is the guard that left the row
    // standing rather than the sweep never reaching this fixture.
    reconciled(&row, Vec::new()).enroll(&mut manifest).unwrap();
    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::Retired));
}

/// Retiring is reversible, which is what makes it safe to do at all: the media comes back, the entry
/// pairs again under the uuid the retired row already carries, and the row goes back on the work
/// list instead of sitting parked for ever.
#[test]
fn a_retired_memory_whose_media_came_back_goes_back_on_the_work_list() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let row = [(at("2020-07-28"), "Image")];
    let row: Vec<(&str, &str)> = row.iter().map(|(date, word)| (date.as_str(), *word)).collect();
    let paired = reconciled(&row, vec![main_file("2020-07-28", 1, "jpg")]);

    paired.enroll(&mut manifest).unwrap();
    reconciled(&row, Vec::new()).enroll(&mut manifest).unwrap();
    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::Retired), "the run that unmounted the part");

    // Third run, with the part back.
    paired.enroll(&mut manifest).unwrap();

    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::Pending));
    let item = manifest.item(ItemKind::Memory, &uuid(1)).unwrap().unwrap();
    assert_eq!(item.retry_count, 0);
    assert!(item.last_error.is_none(), "and the retirement note does not outlive the retirement");
    let owed: Vec<String> = manifest.pending(ItemKind::Memory, 3).unwrap().into_iter().map(|item| item.source_id).collect();
    assert_eq!(owed, [uuid(1)]);
}

/// **This fixture is in a state 16a's own pipeline cannot produce, and that is stated rather than
/// hidden.** `reconcile` only ever marks a SYNTHETIC id source-missing, so nothing in this crate
/// today puts a uuid row in `SourceMissing`; the producer is the downloader a later task adds, which
/// marks a paired item's source missing when it can neither read the file nor fetch the url. The
/// test drives `mark_source_missing` directly because that is exactly the call that downloader will
/// make. It is NOT the unextracted-part case — see
/// `an_entry_whose_media_turned_up_goes_back_on_the_work_list`, where the entry comes back under a
/// new uuid row and `reset` never fires.
#[test]
fn a_source_missing_item_that_paired_again_is_reset_rather_than_left_parked() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let paired = reconciled(&[(&at("2020-07-28"), "Image")], vec![main_file("2020-07-28", 1, "jpg")]);

    // The same uuid goes missing and comes back, which is the transition a plain re-enroll cannot
    // make: `enroll` never touches a status.
    paired.enroll(&mut manifest).unwrap();
    manifest.mark_source_missing(ItemKind::Memory, &uuid(1), "gone for now").unwrap();
    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::SourceMissing));

    paired.enroll(&mut manifest).unwrap();
    assert_eq!(status_of(&manifest, &uuid(1)), Some(ItemStatus::Pending));
    assert_eq!(manifest.item(ItemKind::Memory, &uuid(1)).unwrap().unwrap().retry_count, 0);
}
