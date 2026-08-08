//! Public-API tests for `exportsnap::export::chat_media`: the two filename families, the zip stem
//! pairing, the `Media IDs` join against `chat_history.json`, and what reaches the manifest.
//!
//! **Every tree here is synthetic and none of them is the `fixtures/` one.** That is not a
//! preference: the redactor rewrites `Media IDs` into 15-character tokens carrying no `~`, so no
//! fixture entry spells a `b~<id>` token and a fixture-driven join test cannot exercise the join at
//! all. Filenames are synthesized in the test, directories are tempdirs, and every manifest is
//! opened with `open_in` so the per-user data dir is never touched.
//!
//! The shapes below mirror the observed export's SHAPE rather than its counts — a zip pair, a plain
//! overlay that pairs with nothing, a named `b` file, an unnamed one, and a token with no file —
//! because n=1 makes this export's totals a hint and not a contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use exportsnap::export::chat_media::{
    ChatMedia, ChatMediaFile, ChatMediaItem, Day, Discovery, Family, Join, MediaDate, Message, MessageRef, MissingReason, Reconciliation,
    Token, UnreadableDir, discover, reconcile,
};
use exportsnap::export::manifest::{ExportId, Item, ItemKind, ItemStatus, Manifest};
use exportsnap::export::model::{ChatHistory, ConversationId, Field, Timestamp, Username};
use exportsnap::export::schema;
use tempfile::TempDir;

/// The 13-digit id shape the one observed export used.
const EXPORT_ID: &str = "1784667002819";

/// The 8-character word every one of the observed export's 928 zip filenames shares.
const ZIP_WORD: &str = "vantsnap";

/// A one-to-one thread's key is the friend's own handle; a group's is a bare uuid. Decision 44a
/// names an output directory after whichever of the two the export carried, so both shapes are
/// exercised.
const SOLO_KEY: &str = "friend-handle";
const GROUP_KEY: &str = "3f2e1d0c-b9a8-4756-8433-2211aabbccdd";

const DIR: &str = "/export/chat_media";

/// A distinct alphanumeric id per `seed`, in the shape a plain filename carries.
fn id(seed: u32) -> String {
    format!("aB3xY9{seed:04}")
}

fn plain_at(dir: &str, day: &str, token: Token, seed: u32, extension: &str) -> ChatMediaFile {
    let name = format!("{day}_{}~{}.{extension}", token.as_word(), id(seed));
    ChatMediaFile::parse(Path::new(dir).join(name)).expect("the synthesized name parses")
}

fn plain(day: &str, token: Token, seed: u32, extension: &str) -> ChatMediaFile {
    plain_at(DIR, day, token, seed, extension)
}

/// A `b` file, which is the only family `chat_history.json` can name.
fn bare(day: &str, seed: u32) -> ChatMediaFile {
    plain(day, Token::B, seed, "jpg")
}

fn zip_at(dir: &str, day: &str, token: Token, seed: u32, extension: &str) -> ChatMediaFile {
    let name = format!("{day}_{}~{ZIP_WORD}-{:07}.zip.a1b2c3d.{extension}", token.as_word(), seed);
    ChatMediaFile::parse(Path::new(dir).join(name)).expect("the synthesized name parses")
}

fn zip(day: &str, token: Token, seed: u32, extension: &str) -> ChatMediaFile {
    zip_at(DIR, day, token, seed, extension)
}

/// `chat_history.json` conversations, built through the real schema-to-model path so the
/// reconciliation never sees a state the loader could not produce.
///
/// Each row is one conversation: its key, then the messages in it.
fn history_from(rows: Vec<(&str, Vec<schema::ChatEntry>)>) -> ChatHistory {
    let conversations: BTreeMap<String, Vec<schema::ChatEntry>> =
        rows.into_iter().map(|(key, entries)| (key.to_owned(), entries)).collect();
    ChatHistory::try_from(schema::ChatHistory { conversations }).expect("the synthesized entries parse")
}

/// [`history_from`] for the tests that care only about which message named what: one message per
/// `Media IDs` value, every other field left at the loader's own default.
fn history(rows: &[(&str, &[&str])]) -> ChatHistory {
    history_from(rows.iter().map(|(key, lists)| (*key, lists.iter().map(|media_ids| names(media_ids)).collect())).collect())
}

/// One message naming whatever `media_ids` spells.
fn names(media_ids: &str) -> schema::ChatEntry {
    schema::ChatEntry { media_type: "MEDIA".to_owned(), media_ids: media_ids.to_owned(), ..schema::ChatEntry::default() }
}

/// The UTC instant `text` spells, through the same parser the loader uses.
fn at(text: &str) -> Timestamp {
    Timestamp::parse(Field::Created, text).expect("the synthesized timestamp parses")
}

fn reconciled(rows: &[(&str, &[&str])], files: Vec<ChatMediaFile>) -> Reconciliation {
    reconcile(&history(rows), Discovery::from_files(files, Vec::new()))
}

fn source_ids(reconciliation: &Reconciliation) -> Vec<&str> {
    reconciliation.items.iter().map(ChatMediaItem::source_id).collect()
}

fn item_of<'a>(reconciliation: &'a Reconciliation, source_id: &str) -> &'a ChatMediaItem {
    reconciliation.items.iter().find(|item| item.source_id() == source_id).expect("the item is in the reconciliation")
}

fn join_of<'a>(reconciliation: &'a Reconciliation, source_id: &str) -> &'a Join {
    &item_of(reconciliation, source_id).join
}

/// The message that named `source_id`, or a failure naming what it got instead.
fn message_of<'a>(reconciliation: &'a Reconciliation, source_id: &str) -> &'a Message {
    item_of(reconciliation, source_id).message().expect("the item joined to a message")
}

// ---- the filename grammar ----

#[test]
fn a_plain_name_and_a_zip_name_parse_into_their_parts() {
    let plain = ChatMediaFile::parse("/export/chat_media/2021-03-04_b~aB3xY9.jpg").unwrap();
    assert_eq!(plain.day, Day::parse("2021-03-04").unwrap());
    assert_eq!(plain.token, Token::B);
    assert_eq!(plain.family, Family::Plain { id: "aB3xY9".to_owned() });
    assert_eq!(plain.extension, "jpg");
    assert_eq!(plain.path, PathBuf::from("/export/chat_media/2021-03-04_b~aB3xY9.jpg"));
    // The plain id drops the day, because it IS the token a message spells.
    assert_eq!(plain.id, "b~aB3xY9");
    assert_eq!(plain.history_token(), Some("b~aB3xY9"));

    let zip = ChatMediaFile::parse("/export/chat_media/2021-03-04_overlay~vantsnap-1234567.zip.a1b2c3d.png").unwrap();
    assert_eq!(zip.token, Token::Overlay);
    assert_eq!(zip.family, Family::Zip { mid: "vantsnap-1234567".to_owned(), hash: "a1b2c3d".to_owned() });
    assert_eq!(zip.extension, "png");
    // The zip id keeps the day, because the day is half of the measured pairing key, and it drops
    // the role word, because that is the half a pair swaps.
    assert_eq!(zip.id, "2021-03-04_vantsnap-1234567.zip.a1b2c3d");
    assert_eq!(zip.history_token(), None, "no json names a zip file");
}

#[test]
fn a_role_worded_plain_file_carries_no_token_any_json_can_name() {
    // The prefix `chat_history.json` writes is always the literal `b`, so these three are
    // unreachable from the history however well-formed their names are.
    for token in [Token::Media, Token::Overlay, Token::Thumbnail] {
        let file = plain("2021-03-04", token, 1, "mp4");
        assert_eq!(file.id, format!("{}~{}", token.as_word(), id(1)));
        assert_eq!(file.history_token(), None, "{token}");
    }
    assert_eq!(bare("2021-03-04", 1).history_token(), Some(format!("b~{}", id(1)).as_str()));
}

#[test]
fn every_shape_the_filename_grammar_rejects_stays_unparsed() {
    for name in [
        // An index file beside the media, the one shape memories discovery also rejects.
        "index.html".to_owned(),
        // The role an earlier reading of the export listed and the census found zero files for.
        format!("2021-03-04_metadata~{}.json", id(1)),
        format!("2021-03-04-b~{}.jpg", id(1)),
        format!("2021-3-04_b~{}.jpg", id(1)),
        format!("2021-13-04_b~{}.jpg", id(1)),
        format!("2021-03-32_b~{}.jpg", id(1)),
        format!("2021-03-04_b-{}.jpg", id(1)),
        format!("2021-03-04_{}.jpg", id(1)),
        "2021-03-04_b~.jpg".to_owned(),
        format!("2021-03-04_b~{}", id(1)),
        // A token is matched whole, never by prefix: `bb` is not `b` and `overlayx` is not
        // `overlay`. A `starts_with` classifier would read all three of these as real files and
        // hand them an id spelled with the token it thought it saw.
        format!("2021-03-04_bb~{}.jpg", id(1)),
        format!("2021-03-04_xmedia~{}.mp4", id(1)),
        format!("2021-03-04_overlayx~{}.png", id(1)),
        // A dot inside the id, which is what tells the plain tail apart from a zip one.
        "2021-03-04_b~aB3.xY9.jpg".to_owned(),
        // Zip shapes that are almost the zip family.
        format!("2021-03-04_media~{ZIP_WORD}.zip.a1b2c3d.mp4"),
        format!("2021-03-04_media~{ZIP_WORD}-12x4567.zip.a1b2c3d.mp4"),
        format!("2021-03-04_media~{ZIP_WORD}-1234567.zip..mp4"),
        String::new(),
    ] {
        assert!(ChatMediaFile::parse(Path::new(DIR).join(&name)).is_none(), "{name:?} should not parse");
    }
}

#[test]
fn a_shouted_token_normalizes_rather_than_forking_the_identity() {
    let shouted = ChatMediaFile::parse("/export/chat_media/2021-03-04_B~aB3xY9.JPG").unwrap();
    assert_eq!(shouted.token, Token::B);
    assert_eq!(shouted.id, "b~aB3xY9", "one id per file, whatever case the name shouts it in");
    assert_eq!(shouted.extension, "JPG", "reported as it is on disk");
}

// ---- pairing ----

#[test]
fn a_zip_media_and_its_overlay_pair_on_the_stem_the_role_word_swaps() {
    let media = zip("2021-03-04", Token::Media, 1, "mp4");
    let overlay = zip("2021-03-04", Token::Overlay, 1, "png");
    assert_eq!(media.id, overlay.id, "the stem is what pairs, and only the role word differs");

    let discovery = Discovery::from_files(vec![overlay.clone(), media.clone()], Vec::new());

    assert_eq!(discovery.media.len(), 1);
    assert_eq!(discovery.media[0].file, media);
    assert_eq!(discovery.media[0].overlay, Some(overlay));
    assert!(discovery.unmatched_overlays.is_empty());
}

#[test]
fn two_zip_pairs_on_one_day_pair_within_their_own_mid() {
    let discovery = Discovery::from_files(
        vec![
            zip("2021-03-04", Token::Overlay, 2, "png"),
            zip("2021-03-04", Token::Media, 1, "mp4"),
            zip("2021-03-04", Token::Overlay, 1, "png"),
            zip("2021-03-04", Token::Media, 2, "mp4"),
        ],
        Vec::new(),
    );

    assert_eq!(discovery.media.len(), 2);
    for unit in &discovery.media {
        let overlay = unit.overlay.as_ref().expect("both pairs are complete");
        assert_eq!(overlay.id, unit.file.id, "no overlay crossed into the other pair");
        assert_eq!(overlay.token, Token::Overlay);
    }
    assert!(discovery.unmatched_overlays.is_empty());
}

#[test]
fn the_same_zip_mid_on_two_days_is_two_pairs_rather_than_one() {
    // The day is in the pairing key because the census measured it there. Drop it and these four
    // files collapse into one pair plus two duplicates.
    let discovery = Discovery::from_files(
        vec![
            zip("2021-03-04", Token::Media, 1, "mp4"),
            zip("2021-03-04", Token::Overlay, 1, "png"),
            zip("2021-03-05", Token::Media, 1, "mp4"),
            zip("2021-03-05", Token::Overlay, 1, "png"),
        ],
        Vec::new(),
    );

    assert_eq!(discovery.media.len(), 2);
    assert!(discovery.media.iter().all(|unit| unit.overlay.is_some()));
    assert!(discovery.duplicates.is_empty(), "{:?}", discovery.duplicates);
}

#[test]
fn a_role_worded_overlay_pairs_with_nothing_and_lands_in_the_unmatched_bucket() {
    // Same day, same seed, so every field a heuristic could key on agrees — and the census says the
    // plain family's id sets are pairwise disjoint, so this must still not pair.
    let discovery =
        Discovery::from_files(vec![plain("2021-03-04", Token::Media, 1, "mp4"), plain("2021-03-04", Token::Overlay, 1, "png")], Vec::new());

    assert_eq!(discovery.media.len(), 1);
    assert_eq!(discovery.media[0].file.token, Token::Media);
    assert_eq!(discovery.media[0].overlay, None, "the plain family pairs on nothing, day and id notwithstanding");
    assert_eq!(discovery.unmatched_overlays.len(), 1);
    assert_eq!(discovery.unmatched_overlays[0].token, Token::Overlay);
}

/// The census says the trailing hash is identical within all 464 observed pairs, so this shape is
/// unobserved and the choice it forces is real: pair on `(day, mid)` and let the hashes disagree, or
/// treat the hash as part of the identity and refuse. This build refuses — two files whose stems
/// differ are not one pair — which errs toward an extra unmatched overlay rather than toward
/// compositing one snap's overlay onto another's media.
#[test]
fn zip_halves_whose_trailing_hash_differs_are_not_one_pair() {
    let media = ChatMediaFile::parse("/export/chat_media/2021-03-04_media~vantsnap-1234567.zip.a1b2c3d.mp4").unwrap();
    let overlay = ChatMediaFile::parse("/export/chat_media/2021-03-04_overlay~vantsnap-1234567.zip.9999999.png").unwrap();
    assert_ne!(media.id, overlay.id, "the hash is part of the identity, so the stems are not the same stem");

    let discovery = Discovery::from_files(vec![media, overlay.clone()], Vec::new());

    assert_eq!(discovery.media.len(), 1);
    assert_eq!(discovery.media[0].overlay, None, "no overlay is composited onto media it may not belong to");
    assert_eq!(discovery.unmatched_overlays, vec![overlay]);
}

#[test]
fn a_zip_overlay_whose_media_half_is_absent_is_unmatched_too() {
    let overlay = zip("2021-03-04", Token::Overlay, 1, "png");
    let discovery = Discovery::from_files(vec![zip("2021-03-04", Token::Media, 2, "mp4"), overlay.clone()], Vec::new());

    assert_eq!(discovery.media.len(), 1, "the media file with no overlay is ordinary");
    assert_eq!(discovery.media[0].overlay, None);
    assert_eq!(discovery.unmatched_overlays, vec![overlay]);
}

#[test]
fn two_files_claiming_one_id_are_reported_rather_than_deduped() {
    let first = plain_at("/export/a/chat_media", "2021-03-04", Token::B, 1, "jpg");
    let second = plain_at("/export/b/chat_media", "2021-03-04", Token::B, 1, "jpg");
    let discovery = Discovery::from_files(vec![second.clone(), first.clone()], Vec::new());

    assert_eq!(discovery.media.len(), 1, "one unit, however many copies of its file exist");
    assert_eq!(discovery.duplicates.len(), 1);
    assert_eq!(discovery.duplicates[0].id, format!("b~{}", id(1)));
    assert!(!discovery.duplicates[0].overlay);
    assert_eq!(discovery.duplicates[0].kept, first.path);
    assert_eq!(discovery.duplicates[0].ignored, vec![second.path]);
    assert_eq!(discovery.media[0].file.path, first.path, "the pairing used the file the report names");
}

#[test]
fn a_zip_pair_split_across_two_dirs_pairs_and_is_not_a_duplicate() {
    let media = zip_at("/export/a/chat_media", "2021-03-04", Token::Media, 1, "mp4");
    let overlay = zip_at("/export/b/chat_media", "2021-03-04", Token::Overlay, 1, "png");
    let discovery = Discovery::from_files(vec![media, overlay], Vec::new());

    assert_eq!(discovery.media.len(), 1);
    assert!(discovery.media[0].overlay.is_some());
    assert!(discovery.duplicates.is_empty());
}

#[test]
fn pairing_does_not_depend_on_the_order_the_walk_found_the_files() {
    // The duplicate is what makes the order observable at all: with none, the maps the pairing is
    // built from already impose id order whatever the input did.
    let files = vec![
        zip("2021-03-04", Token::Overlay, 3, "png"),
        zip("2021-03-04", Token::Media, 3, "mp4"),
        plain_at("/export/b/chat_media", "2021-03-04", Token::B, 1, "jpg"),
        plain_at("/export/a/chat_media", "2021-03-04", Token::B, 1, "jpg"),
        plain("2021-03-04", Token::Overlay, 2, "png"),
    ];
    let mut reversed = files.clone();
    reversed.reverse();

    let forwards = Discovery::from_files(files, Vec::new());
    let backwards = Discovery::from_files(reversed, Vec::new());
    assert_eq!(forwards, backwards, "read_dir order must not decide which file the pairing keeps");
    assert_eq!(forwards.duplicates.len(), 1);
    assert!(forwards.duplicates[0].kept.starts_with("/export/a"), "{:?}", forwards.duplicates[0].kept);
    assert_eq!(forwards.unmatched_overlays.len(), 1);
}

// ---- the walk ----

#[test]
fn discovery_reads_every_chat_media_dir_at_any_depth_and_nothing_beside_them() {
    let root = TempDir::new().unwrap();
    let name = |seed: u32| format!("2021-03-04_b~{}.jpg", id(seed));

    for (dir, seed) in
        [("chat_media", 1), ("mydata~1784667002819-3/chat_media", 2), ("chat_media (1)/chat_media", 3), ("a/b/c/chat_media", 4)]
    {
        let dir = root.path().join(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name(seed)), b"x").unwrap();
        fs::write(dir.join("index.html"), b"<html>").unwrap();
    }
    // A media file with a chat-media-shaped name outside a `chat_media` dir belongs to no pipeline
    // this module drives.
    let memories = root.path().join("memories");
    fs::create_dir_all(&memories).unwrap();
    fs::write(memories.join(name(9)), b"x").unwrap();

    let discovery = discover(root.path()).unwrap();

    let found: Vec<&str> = discovery.media.iter().map(ChatMedia::source_id).collect();
    let expected: Vec<String> = (1..=4).map(|seed| format!("b~{}", id(seed))).collect();
    assert_eq!(found, expected, "the dir NAME is what gates discovery, not the depth");
    assert_eq!(discovery.unparsed.len(), 4, "one index.html per dir, carried rather than dropped");
    assert!(discovery.unparsed.iter().all(|path| path.ends_with("index.html")), "{:?}", discovery.unparsed);
    assert!(discovery.duplicates.is_empty());
    assert!(discovery.unmatched_overlays.is_empty());
    assert!(discovery.unreadable.is_empty());
}

#[test]
fn a_root_that_cannot_be_listed_names_itself_and_says_what_to_do() {
    let root = TempDir::new().unwrap();
    let missing = root.path().join("not-here");

    let error = discover(&missing).unwrap_err();
    assert_eq!(error.dir, missing);
    let rendered = error.to_string();
    assert!(rendered.contains(&missing.display().to_string()), "{rendered}");
    assert!(rendered.contains("chat_media dirs"), "{rendered}");
}

/// A source root on a real mount carries dirs this user cannot read and the export has nothing to do
/// with — `lost+found` is 0700 and root-owned on every ext4 mount. Aborting the scan over one of
/// those would report zero chat media for an export that is entirely fine.
#[cfg(unix)]
#[test]
fn a_directory_the_walk_cannot_list_is_reported_and_the_rest_of_the_scan_survives() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().unwrap();
    let dir = root.path().join("chat_media");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("2021-03-04_b~{}.jpg", id(1))), b"x").unwrap();

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

    assert_eq!(discovery.media.len(), 1, "the file that WAS found still comes back");
    assert_eq!(discovery.unreadable.len(), 1);
    assert_eq!(discovery.unreadable[0].dir, locked);
    assert_eq!(discovery.unreadable[0].kind, io::ErrorKind::PermissionDenied);

    // Restore the mode so the tempdir can clean itself up.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
}

/// The walk's symlink rule is a mechanism claim whose counterexample compiles, and the walk is now
/// shared with `memories`, so a change made for one pipeline reaches both. `tests/memories.rs` holds
/// the twin of this test and the full account of what swapping `entry.file_type()` for
/// `Path::is_dir` costs: not a hang, but the same file rediscovered at forty-odd paths in under a
/// millisecond, which only a duplicate count can see.
#[cfg(unix)]
#[test]
fn a_symlink_loop_does_not_make_the_walk_re_enter_itself() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("chat_media");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("2021-03-04_b~{}.jpg", id(1))), b"x").unwrap();
    std::os::unix::fs::symlink(root.path(), dir.join("loop")).unwrap();

    let discovery = discover(root.path()).unwrap();

    assert_eq!(discovery.media.len(), 1);
    assert!(discovery.duplicates.is_empty(), "one file found once, not once per re-entry: {:?}", discovery.duplicates);
    // The link is not a dir to descend and not a media filename, so it lands here — and at the
    // SHALLOW path, which is what says the walk never went through it.
    assert_eq!(discovery.unparsed, vec![dir.join("loop")]);
    assert!(discovery.unreadable.is_empty());
}

// ---- the join ----

#[test]
fn a_b_file_a_message_names_joins_to_that_message() {
    let token = format!("b~{}", id(1));
    let reconciliation = reconciled(&[("alice", &[""]), ("bob", &["", &token])], vec![bare("2021-03-04", 1)]);

    // Conversations are sorted by key, so `bob` is index 1 and the token is its second message.
    assert_eq!(message_of(&reconciliation, &token).at, MessageRef { conversation: 1, message: 1 });
    assert!(reconciliation.missing.is_empty());
}

#[test]
fn a_b_file_no_message_names_is_unnamed_rather_than_missing() {
    let named = format!("b~{}", id(1));
    let reconciliation = reconciled(&[("alice", &[&named])], vec![bare("2021-03-04", 1), bare("2021-03-05", 2)]);

    assert!(join_of(&reconciliation, &named).is_named());
    // 5417 of the observed export's 8005 `b` files are this. Not a gap and not an error: the file is
    // right there, the history just never mentioned it.
    assert_eq!(join_of(&reconciliation, &format!("b~{}", id(2))), &Join::Unnamed);
    assert!(!join_of(&reconciliation, &format!("b~{}", id(2))).is_named());
    assert!(reconciliation.missing.is_empty(), "a file with no message is not a missing file");
}

#[test]
fn a_file_whose_family_no_json_can_name_is_recorded_apart_from_one_nobody_named() {
    let reconciliation = reconciled(
        &[("alice", &[])],
        vec![
            bare("2021-03-04", 1),
            plain("2021-03-04", Token::Media, 2, "mp4"),
            plain("2021-03-04", Token::Overlay, 3, "png"),
            plain("2021-03-04", Token::Thumbnail, 4, "jpg"),
            zip("2021-03-04", Token::Media, 5, "mp4"),
        ],
    );

    // One total order over every unit, both families and the unmatched overlay together, rather
    // than the two runs the discovery hands over. A screen paging through these gets a stable list.
    assert_eq!(
        source_ids(&reconciliation),
        [
            zip("2021-03-04", Token::Media, 5, "mp4").id,
            format!("b~{}", id(1)),
            format!("media~{}", id(2)),
            format!("overlay~{}", id(3)),
            format!("thumbnail~{}", id(4)),
        ]
    );

    assert_eq!(join_of(&reconciliation, &format!("b~{}", id(1))), &Join::Unnamed, "a `b` file could have been named");
    for id in [format!("media~{}", id(2)), format!("overlay~{}", id(3)), format!("thumbnail~{}", id(4))] {
        assert_eq!(join_of(&reconciliation, &id), &Join::NoToken, "{id}");
    }
    assert_eq!(join_of(&reconciliation, &zip("2021-03-04", Token::Media, 5, "mp4").id), &Join::NoToken);
}

#[test]
fn an_unmatched_overlay_is_an_item_of_its_own_rather_than_a_file_the_run_forgets() {
    let reconciliation = reconciled(&[("alice", &[])], vec![plain("2021-03-04", Token::Overlay, 1, "png")]);

    assert_eq!(source_ids(&reconciliation), [format!("overlay~{}", id(1))]);
    assert_eq!(reconciliation.items[0].media.overlay, None);
    assert_eq!(reconciliation.items[0].join, Join::NoToken);
}

#[test]
fn a_history_token_with_no_file_on_disk_becomes_the_gap() {
    let present = format!("b~{}", id(1));
    let absent = format!("b~{}", id(2));
    let reconciliation = reconciled(&[("alice", &[&present, &absent])], vec![bare("2021-03-04", 1)]);

    assert_eq!(source_ids(&reconciliation), [present.as_str()]);
    assert_eq!(reconciliation.missing.len(), 1);
    assert_eq!(reconciliation.missing[0].token, absent);
    assert_eq!(reconciliation.missing[0].message, MessageRef { conversation: 0, message: 1 });
    assert_eq!(reconciliation.missing[0].reason, MissingReason::NoFile);
}

#[test]
fn a_media_ids_list_names_every_token_it_carries() {
    let tokens: Vec<String> = (1..=3).map(|seed| format!("b~{}", id(seed))).collect();
    // The observed delimiter is `" | "`; the spacing is not what carries the meaning.
    let list = format!("{} | {}|{}", tokens[0], tokens[1], tokens[2]);
    let reconciliation = reconciled(&[("alice", &[&list])], vec![bare("2021-03-04", 1), bare("2021-03-04", 2)]);

    assert!(reconciliation.items.iter().all(|item| item.join.is_named()), "{:?}", reconciliation.items);
    assert_eq!(reconciliation.missing.len(), 1, "the third token names no file");
    assert_eq!(reconciliation.missing[0].token, tokens[2]);
}

#[test]
fn an_empty_media_ids_value_names_nothing() {
    // Every message that carries no media has this, which is 6857 of the observed export's 8090
    // entries. A bare split would turn each into a token spelled `""`.
    let reconciliation = reconciled(&[("alice", &["", " | ", "|"])], vec![bare("2021-03-04", 1)]);

    assert_eq!(join_of(&reconciliation, &format!("b~{}", id(1))), &Join::Unnamed);
    assert!(reconciliation.missing.is_empty(), "{:?}", reconciliation.missing);
}

/// Which message wins decides the file's timestamp, its sender and the conversation a later pass
/// files it under, so "the first one" has to hold for the whole carried record and not only for the
/// position it came from.
#[test]
fn a_token_two_messages_name_carries_the_first_messages_own_facts() {
    let token = format!("b~{}", id(1));
    let first = schema::ChatEntry {
        from: "first-sender".to_owned(),
        created: "2021-03-04 09:15:00 UTC".to_owned(),
        is_sender: false,
        ..names(&token)
    };
    let second = schema::ChatEntry {
        from: "second-sender".to_owned(),
        created: "2021-03-06 22:00:00 UTC".to_owned(),
        conversation_title: Some("the group".to_owned()),
        is_sender: true,
        ..names(&token)
    };
    // Sorted by key, so `alice` is the first conversation and its message is the one that wins.
    let history = history_from(vec![("alice", vec![first]), ("bob", vec![second])]);

    let reconciliation = reconcile(&history, Discovery::from_files(vec![bare("2021-03-04", 1)], Vec::new()));

    // The whole record in ONE assertion, deliberately: field-at-a-time asserts abort at the first
    // one that fires, so a build taking the second message would red on its position and never
    // reach the fields this test is named for.
    assert_eq!(
        *message_of(&reconciliation, &token),
        Message {
            at: MessageRef { conversation: 0, message: 0 },
            conversation: ConversationId::new("alice"),
            conversation_title: None,
            from: Username::new("first-sender"),
            is_sender: false,
            created: Some(at("2021-03-04 09:15:00 UTC")),
        }
    );
    assert_eq!(item_of(&reconciliation, &token).date(), MediaDate::Message(at("2021-03-04 09:15:00 UTC")));
}

#[test]
fn a_token_the_grammar_cannot_read_is_surfaced_rather_than_made_a_gap_row() {
    let reconciliation = reconciled(&[("alice", &["not-a-token | b~ | media~xyz | b~has.dot"])], vec![bare("2021-03-04", 1)]);

    let surfaced: Vec<&str> = reconciliation.unparsed_tokens.iter().map(|token| token.token.as_str()).collect();
    assert_eq!(surfaced, ["b~", "b~has.dot", "media~xyz", "not-a-token"], "ordered by token, deduped, verbatim");
    assert!(reconciliation.unparsed_tokens.iter().all(|token| token.message == MessageRef { conversation: 0, message: 0 }));
    // None of them becomes a manifest row: a gap row nothing can ever clear is worse than a report.
    assert!(reconciliation.missing.is_empty(), "{:?}", reconciliation.missing);
}

/// The filename side normalizes a shouted token (`B~x` on disk yields id `b~x`). The history side has
/// to normalize identically or one thing forks into two rows — `b~x` `Pending` from the file and
/// `B~x` `SourceMissing` from the gap.
#[test]
fn a_shouted_token_in_the_history_lands_on_the_same_row_as_the_file() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let file = ChatMediaFile::parse("/export/chat_media/2021-03-04_B~aB3xY9.JPG").unwrap();
    let reconciliation = reconcile(&history(&[("alice", &["B~aB3xY9"])]), Discovery::from_files(vec![file], Vec::new()));

    assert_eq!(message_of(&reconciliation, "b~aB3xY9").at, MessageRef { conversation: 0, message: 0 });
    assert!(reconciliation.missing.is_empty());

    reconciliation.enroll(&mut manifest).unwrap();
    assert_eq!(enrolled(&manifest), [("b~aB3xY9".to_owned(), ItemStatus::Pending)], "one thing, one row");
}

/// `parse_history_token`'s by-construction argument names this counterexample, and a claim carried
/// only in prose is the defect class that produced the parking bug. A `b~` name in the ZIP shape
/// really does parse as `(Token::B, Family::Zip)`, and its id is day-prefixed, so no validated token
/// can spell it.
#[test]
fn a_b_shaped_zip_name_is_not_a_history_token() {
    let odd = ChatMediaFile::parse("/export/chat_media/2021-03-04_b~ab-1.zip.cd.jpg").unwrap();

    assert_eq!(odd.token, Token::B);
    assert_eq!(odd.family, Family::Zip { mid: "ab-1".to_owned(), hash: "cd".to_owned() });
    assert_eq!(odd.id, "2021-03-04_ab-1.zip.cd");
    assert_eq!(odd.history_token(), None, "the family half of the guard is what stops a day-prefixed id reaching the join");

    // And a message spelling that id cannot park the file's row.
    let reconciliation = reconcile(&history(&[("alice", &[&odd.id])]), Discovery::from_files(vec![odd.clone()], Vec::new()));
    assert_eq!(join_of(&reconciliation, &odd.id), &Join::NoToken);
    assert!(reconciliation.missing.is_empty());
    assert_eq!(reconciliation.unparsed_tokens.len(), 1, "the id is surfaced as unreadable, not minted into a gap");
}

/// The join map is a `BTreeMap` collect, and a collect keeps the LAST value on a duplicate key — so
/// two items sharing a token would leave the earlier one unjoinable with no gap row and no trace,
/// the same silent shape as the parking bug one function down. This pins the invariant the map
/// depends on, at the public API, over the three ways an id could be minted twice: a duplicate copy
/// by path, an unmatched overlay, and a zip pair.
#[test]
fn no_two_items_can_claim_one_history_token() {
    let mut files = vec![
        plain_at("/export/a/chat_media", "2021-03-04", Token::B, 1, "jpg"),
        plain_at("/export/b/chat_media", "2021-03-04", Token::B, 1, "jpg"),
        plain("2021-03-05", Token::B, 2, "mp4"),
        plain("2021-03-04", Token::Overlay, 3, "png"),
        zip("2021-03-04", Token::Media, 4, "mp4"),
        zip("2021-03-04", Token::Overlay, 4, "png"),
    ];
    files.extend((10..20).map(|seed| bare("2021-03-06", seed)));

    let reconciliation = reconcile(&history(&[("alice", &[])]), Discovery::from_files(files, Vec::new()));

    let tokens: Vec<&str> = reconciliation.items.iter().filter_map(|item| item.media.file.history_token()).collect();
    let distinct: BTreeSet<&str> = tokens.iter().copied().collect();
    assert_eq!(distinct.len(), tokens.len(), "two items claiming one token would silently drop one from the join: {tokens:?}");
    assert_eq!(tokens.len(), 12, "one per `b` file, the duplicate copy collapsed into its own row");
}

#[test]
fn two_messages_naming_one_absent_token_leave_one_gap_row() {
    let absent = format!("b~{}", id(9));
    let reconciliation = reconciled(&[("alice", &[&absent]), ("bob", &[&absent])], Vec::new());

    // One token, one row. A second message naming it must not enrol it twice, and the reference
    // kept is the first, matching what a present file gets in
    // `a_token_two_messages_name_carries_the_first_messages_own_facts`.
    assert_eq!(reconciliation.missing.len(), 1);
    assert_eq!(reconciliation.missing[0].token, absent);
    assert_eq!(reconciliation.missing[0].message, MessageRef { conversation: 0, message: 0 });

    // The KEY is a separate assertion from the reference above, because it is a separate field that
    // a later edit can move on its own — measured: making the conversation last-namer-wins while
    // leaving `MessageRef` first-wins passes every other test in this crate. It has to agree with
    // the reference for the reason the two exist together: `chat_fix` adopts a directory per
    // conversation, and a gap row attributing to `bob` while the file that turns up next run
    // attributes to `alice` would hand one thread the directory holding the other's output.
    // `alice` rather than `bob` because the parser sorts conversations by key, so this is also what
    // a present file gets from the `Join::Unnamed` guard.
    assert_eq!(reconciliation.missing[0].conversation, ConversationId::new("alice"));
}

/// The census found the filename's day equal to the message's `Created` date for all 2588 matches,
/// which is a fact about the export rather than a rule the join may lean on. The join is a string
/// equality over the token and reads neither date, so a disagreement joins anyway.
///
/// **The join half of this pin locks in an absence and no mutation can kill it**, because there is
/// no date comparison there to break — it exists so that ADDING one stops being silent. The date
/// half is a live assertion: where the two disagree the message wins, because it is the one that
/// states a time of day.
#[test]
fn a_filename_day_that_disagrees_with_the_message_date_still_joins() {
    let token = format!("b~{}", id(1));
    let entry = schema::ChatEntry { created: "2019-11-30 12:41:51 UTC".to_owned(), ..names(&token) };
    let history = history_from(vec![("alice", vec![entry])]);

    // The file says 2021-03-04 and the message says 2019-11-30.
    let reconciliation = reconcile(&history, Discovery::from_files(vec![bare("2021-03-04", 1)], Vec::new()));

    assert_eq!(message_of(&reconciliation, &token).at, MessageRef { conversation: 0, message: 0 });
    assert!(reconciliation.missing.is_empty(), "a date disagreement is not a missing file");
    assert_eq!(item_of(&reconciliation, &token).date(), MediaDate::Message(at("2019-11-30 12:41:51 UTC")));
}

/// A dir the walk could not list means the run cannot say a file does not exist — it can only say it
/// never saw one. `SourceMissing` is never handed back as work, so the verdict is durable and has to
/// stop short of the claim.
#[test]
fn an_incomplete_scan_reports_unscanned_rather_than_asserting_no_file_exists() {
    let history = history(&[("alice", &[&format!("b~{}", id(1))])]);
    let locked = vec![UnreadableDir { dir: PathBuf::from("/export/mydata~1/chat_media"), kind: io::ErrorKind::PermissionDenied }];

    let complete = reconcile(&history, Discovery::from_files(Vec::new(), Vec::new()));
    assert_eq!(complete.missing[0].reason, MissingReason::NoFile, "a complete scan can assert absence");

    let incomplete = reconcile(&history, Discovery::from_walk(Vec::new(), Vec::new(), locked));
    assert_eq!(incomplete.missing[0].reason, MissingReason::Unscanned);
    assert_eq!(incomplete.unreadable.len(), 1, "and the reconciliation says why the verdict is qualified");
}

#[test]
fn a_missing_reason_says_which_gap_it_is_in_prose_the_manifest_can_store() {
    for reason in MissingReason::ALL {
        // Exhaustiveness witness. `ALL` alone cannot make a new variant loud — an array literal's
        // length is independent of the enum's variant count — while this match is a compile error
        // the moment a variant is added. It catches the ADDITION, not a short `ALL`. Never collapse
        // the arms to `_ => {}`.
        match reason {
            MissingReason::NoFile | MissingReason::Unscanned => {}
        }
        let rendered = reason.to_string();
        // The manifest replaces any token holding url punctuation or running past 64 characters, so
        // a reason spelled with either would reach `last_error` as `<redacted>`.
        assert!(rendered.split_whitespace().all(|token| token.len() <= 64 && !token.contains(['/', '=', '%', '&', '@'])), "{rendered}");
    }
    let spellings = MissingReason::ALL.map(|reason| reason.to_string());
    assert_eq!(BTreeSet::from(spellings.clone()).len(), spellings.len(), "each reason reads differently: {spellings:?}");
}

// ---- what a join carries ----

/// The four facts task 32 puts on the item, read back off both thread shapes the export has. The
/// conversation is asserted as the KEY rather than the title because that is what an output
/// directory is named from, and a title is null on every one-to-one thread.
#[test]
fn a_named_file_carries_its_messages_time_sender_and_conversation() {
    let sent_token = format!("b~{}", id(1));
    let received_token = format!("b~{}", id(2));
    let renamed_token = format!("b~{}", id(3));
    // The two dimensions are crossed on purpose: a message SENT in the one-to-one thread against
    // one RECEIVED in the group. Pair them the other way — sent-and-group, received-and-solo — and
    // a build reading either field off the other passes every assertion below.
    let sent = schema::ChatEntry {
        from: "my-handle".to_owned(),
        created: "2021-03-04 09:15:00 UTC".to_owned(),
        is_sender: true,
        ..names(&sent_token)
    };
    let received = schema::ChatEntry {
        from: "friend-handle".to_owned(),
        created: "2021-03-05 22:41:07 UTC".to_owned(),
        conversation_title: Some("weekend plans".to_owned()),
        is_sender: false,
        ..names(&received_token)
    };
    // The same thread after somebody renamed it. `Conversation Title` is written per MESSAGE, so
    // one key really does carry two titles — which is both why the title is read off the record
    // rather than off the thread, and why decision 44a names a directory after the stable key.
    // Without this row every fixture here has one title per thread, and a build reading the
    // thread's FIRST record's title passes the whole suite.
    let renamed = schema::ChatEntry {
        from: "friend-handle".to_owned(),
        created: "2021-03-06 08:02:00 UTC".to_owned(),
        conversation_title: Some("trip planning".to_owned()),
        is_sender: false,
        ..names(&renamed_token)
    };
    let history = history_from(vec![(GROUP_KEY, vec![received, renamed]), (SOLO_KEY, vec![sent])]);

    let reconciliation =
        reconcile(&history, Discovery::from_files(vec![bare("2021-03-04", 1), bare("2021-03-05", 2), bare("2021-03-06", 3)], Vec::new()));

    let message = message_of(&reconciliation, &sent_token);
    assert_eq!(message.conversation, ConversationId::new(SOLO_KEY));
    assert_eq!(message.conversation_title, None, "a one-to-one thread carries no title, and none is invented");
    assert_eq!(message.from, Username::new("my-handle"));
    assert!(message.is_sender, "which side sent it is carried apart from `From`, which nothing establishes the meaning of");
    assert_eq!(item_of(&reconciliation, &sent_token).date(), MediaDate::Message(at("2021-03-04 09:15:00 UTC")));

    let message = message_of(&reconciliation, &received_token);
    // Sorted by key, so the group thread is conversation 0 — and the position is still carried, for
    // everything about the message this struct does not hold.
    assert_eq!(message.at, MessageRef { conversation: 0, message: 0 });
    assert_eq!(message.conversation, ConversationId::new(GROUP_KEY), "the key a directory is named from, not the title");
    assert_eq!(message.conversation_title, Some("weekend plans".to_owned()));
    assert_eq!(message.from, Username::new("friend-handle"));
    assert!(!message.is_sender);
    assert_eq!(item_of(&reconciliation, &received_token).date(), MediaDate::Message(at("2021-03-05 22:41:07 UTC")));

    // Same thread, same key, its own title: the title is the record's, never the thread's.
    let renamed = message_of(&reconciliation, &renamed_token);
    assert_eq!(renamed.at, MessageRef { conversation: 0, message: 1 });
    assert_eq!(renamed.conversation, ConversationId::new(GROUP_KEY), "one key across the rename");
    assert_eq!(renamed.conversation_title, Some("trip planning".to_owned()));
    assert_ne!(renamed.conversation_title, message.conversation_title, "two titles under one conversation key");
}

/// The 6877 files no message names, which is 73% of the export. Their date is the day their own
/// filename leads with — the census proved that day equal to the message's own wherever a message
/// exists, and it is the only date left where none does.
#[test]
fn a_file_no_message_names_is_dated_by_the_day_in_its_filename() {
    let named = format!("b~{}", id(1));
    let entry = schema::ChatEntry { created: "2021-03-04 09:15:00 UTC".to_owned(), ..names(&named) };
    let reconciliation = reconcile(
        &history_from(vec![(SOLO_KEY, vec![entry])]),
        Discovery::from_files(
            vec![
                bare("2021-03-04", 1),
                bare("2021-03-05", 2),
                plain("2021-03-06", Token::Overlay, 3, "png"),
                zip("2021-03-07", Token::Media, 4, "mp4"),
            ],
            Vec::new(),
        ),
    );

    // One per join state that is not `Named`: a `b` file nobody named, and two whose family no json
    // can reach at all.
    for (source_id, day) in [
        (format!("b~{}", id(2)), "2021-03-05"),
        (format!("overlay~{}", id(3)), "2021-03-06"),
        (zip("2021-03-07", Token::Media, 4, "mp4").id, "2021-03-07"),
    ] {
        let item = item_of(&reconciliation, &source_id);
        assert!(item.message().is_none(), "{source_id} carries a message nothing named it in");
        assert_eq!(item.date(), MediaDate::Filename(Day::parse(day).unwrap()), "{source_id}");
    }
    // And the file a message DID name is not dated this way, or the fallback would be the only
    // answer this test could ever see.
    assert_eq!(item_of(&reconciliation, &named).date(), MediaDate::Message(at("2021-03-04 09:15:00 UTC")));
}

/// A message can name a file and date nothing: the export writes `""` for a value it has none of.
/// Unobserved on a matched message, expressible, and the join survives it — only the date falls
/// back, while the sender and the conversation stay exactly as known as they were.
///
/// **It also pins what the record's OTHER date may not do**, from both sides, because `Message`
/// deliberately does not carry `Created(microseconds)` and a later build might carry it without
/// reading why. Both rows below set the epoch EXPLICITLY rather than inheriting `names`' zero — a
/// fixture that leans on the default cannot tell "the zero case is handled" from "a zero happened
/// to be lying there:
///
/// - a real epoch present and `Created` empty must still fall back, or the unstamped instant this
///   module never read has become the answer
/// - an epoch of `0`, which is what serde hands back for an ABSENT key, must not become a
///   message-stated 1970 date that outranks the filename day — the one shape where carrying the
///   field would be worse than dropping it
#[test]
fn a_message_that_names_a_file_without_dating_it_leaves_it_on_the_filename_day() {
    let dated_epoch = format!("b~{}", id(1));
    let absent_epoch = format!("b~{}", id(2));
    // 2020-07-26 in milliseconds, the shape `tests/export.rs` pins off the real fixture.
    let with_epoch = schema::ChatEntry {
        from: "friend-handle".to_owned(),
        created: String::new(),
        created_epoch: 1_595_778_485_675,
        ..names(&dated_epoch)
    };
    let without_epoch =
        schema::ChatEntry { from: "friend-handle".to_owned(), created: String::new(), created_epoch: 0, ..names(&absent_epoch) };
    let reconciliation = reconcile(
        &history_from(vec![(SOLO_KEY, vec![with_epoch, without_epoch])]),
        Discovery::from_files(vec![bare("2021-03-04", 1), bare("2021-03-05", 2)], Vec::new()),
    );

    // The two observables first, and the dated-epoch row before the zero row, so that a build
    // promoting the epoch reds HERE — naming the row it broke — rather than at a field-level
    // assertion below that would abort the body before either date was ever read.
    assert_eq!(
        item_of(&reconciliation, &dated_epoch).date(),
        MediaDate::Filename(Day::parse("2021-03-04").unwrap()),
        "a real epoch this module never carried must not become the date"
    );
    assert_eq!(
        item_of(&reconciliation, &absent_epoch).date(),
        MediaDate::Filename(Day::parse("2021-03-05").unwrap()),
        "an absent epoch is an absence, never 1970"
    );

    let message = message_of(&reconciliation, &dated_epoch);
    assert_eq!(message.created, None);
    assert_eq!(message.from, Username::new("friend-handle"), "an undated message still says who sent it");
}

// ---- the census shape ----

/// Builds a corpus at the observed export's shape and cardinality, plus the history that names it.
/// Every file goes through `ChatMediaFile::parse` on a synthesized name; no filesystem.
fn census_corpus() -> (Vec<ChatMediaFile>, ChatHistory) {
    let day = "2021-03-04";
    let mut files = Vec::with_capacity(9465);
    let mut b_tokens = Vec::with_capacity(8005);

    let parse = |name: String| ChatMediaFile::parse(Path::new(DIR).join(name)).expect("the synthesized name parses");

    // plain `b`: 8005, with the extension split, which is the only place the non-jpg tail is pinned.
    for (extension, count) in [("jpg", 5523), ("mp4", 2403), ("png", 59), ("gif", 15), ("webp", 5)] {
        for _ in 0..count {
            let seed = b_tokens.len();
            files.push(parse(format!("{day}_b~bid{seed:06}.{extension}")));
            b_tokens.push(format!("b~bid{seed:06}"));
        }
    }
    // The role-worded plain remainder: 264 + (117 + 107) + 44, which pairs with nothing. One running
    // seed across all four batches, never one per batch — the census says these id sets are pairwise
    // disjoint, and a per-batch counter would mint `overlay~…000000` twice (once png, once webp) and
    // synthesize 107 duplicates that the real export does not have.
    let mut role_seed = 0;
    for (token, extension, count) in
        [(Token::Media, "mp4", 264), (Token::Overlay, "png", 117), (Token::Overlay, "webp", 107), (Token::Thumbnail, "jpg", 44)]
    {
        for _ in 0..count {
            files.push(parse(format!("{day}_{}~rid{role_seed:06}.{extension}", token.as_word())));
            role_seed += 1;
        }
    }
    // zip: 464 pairs, both halves sharing `(day, mid, hash)`.
    for index in 0..464 {
        for (token, extension) in [(Token::Media, "mp4"), (Token::Overlay, "png")] {
            files.push(parse(format!("{day}_{}~{ZIP_WORD}-{index:07}.zip.a1b2c3d.{extension}", token.as_word())));
        }
    }

    // 2611 distinct tokens: 2588 naming a `b` file that exists, 23 naming none. The census recorded
    // two SEPARATE splits over the 2588 matched messages — 1931 one-to-one against 657 group, and
    // 1041 sent against 1547 received — and no cross-tabulation of the two, so which of the sent
    // messages fall in which thread below is this file's own arrangement and asserts nothing about
    // the export.
    let mut solo = Vec::new();
    let mut group = Vec::new();
    for (index, token) in b_tokens[..2588].iter().enumerate() {
        // Dated on the corpus's own day, which is what makes the 2588-of-2588 filename-equals-
        // message result something the test re-derives instead of assuming.
        let entry = schema::ChatEntry {
            created: format!("{day} 09:15:00 UTC"),
            from: "friend-handle".to_owned(),
            is_sender: index < 1041,
            ..names(token)
        };
        if index < 1931 {
            solo.push(entry);
        } else {
            group.push(schema::ChatEntry { conversation_title: Some("the group".to_owned()), ..entry });
        }
    }
    // The 23 tokens no file carries. Which thread names them is not something the census measured.
    solo.extend((0..23).map(|index| names(&format!("b~absent{index:06}"))));

    (files, history_from(vec![(GROUP_KEY, group), (SOLO_KEY, solo)]))
}

/// The code reproduces the census on census-shaped data.
///
/// **Provenance: every figure below is handed down from the orchestrator's shape-only census of the
/// real export (n=1, 2026-08-05), and is NOT measured here.** Neither this test nor anyone who can
/// run it may read `/mnt/hdd-1/`. So this proves the pipeline reproduces the recorded counts on data
/// SHAPED like the export — strictly weaker than re-measuring it, and worth having because it is the
/// strongest claim available: a change that silently reclassifies a whole family shows up as a
/// number that moved.
#[test]
fn the_census_shape_reproduces_its_recorded_counts() {
    let (files, history) = census_corpus();
    assert_eq!(files.len(), 9465, "the corpus is the census total, or nothing below means anything");

    let discovery = Discovery::from_files(files, Vec::new());
    assert_eq!(discovery.media.len(), 8777, "8005 b + 264 media + 44 thumbnail + 464 zip media");
    assert_eq!(discovery.media.iter().filter(|unit| unit.overlay.is_some()).count(), 464, "only the zip family pairs");
    assert_eq!(discovery.unmatched_overlays.len(), 224, "the whole role-worded overlay family, which pairs with nothing");
    assert!(discovery.unparsed.is_empty(), "zero files fall outside the two grammars");
    assert!(discovery.duplicates.is_empty());
    assert!(discovery.unreadable.is_empty());

    let reconciliation = reconcile(&history, discovery);

    assert_eq!(reconciliation.items.len(), 9001, "8777 units + 224 unmatched overlays");
    let count = |wanted: fn(&Join) -> bool| reconciliation.items.iter().filter(|item| wanted(&item.join)).count();
    assert_eq!(count(|join| matches!(join, Join::Named(_))), 2588);
    assert_eq!(count(|join| matches!(join, Join::Unnamed)), 5417, "8005 b files less the 2588 a message names");
    // 996, NOT the 1460 the file-count prose invites: 1460 counts FILES (532 role-worded + 928 zip),
    // and the 464 zip overlays are not items — they ride on their media half. As items it is
    // 264 media + 44 thumbnail + 464 zip media + 224 unmatched overlays.
    assert_eq!(count(|join| matches!(join, Join::NoToken)), 996);
    assert_eq!(reconciliation.missing.len(), 23, "the tokens no file on disk carries");
    assert!(reconciliation.unparsed_tokens.is_empty(), "every one of the 2611 observed tokens is well formed");

    // What the joins carry, cross-checked per conversation against the census's own two splits.
    let named: Vec<(&ChatMediaItem, &Message)> =
        reconciliation.items.iter().filter_map(|item| item.message().map(|message| (item, message))).collect();
    assert_eq!(named.len(), 2588, "one carried message per matched file");
    let in_thread = |key: &str| -> Vec<&(&ChatMediaItem, &Message)> {
        named.iter().filter(|(_, message)| message.conversation.as_str() == key).collect()
    };

    let group = in_thread(GROUP_KEY);
    assert_eq!(group.len(), 657, "the group thread's share of the matches");
    assert!(group.iter().all(|(_, message)| message.conversation_title.is_some()), "a group match names its thread");
    let solo = in_thread(SOLO_KEY);
    assert_eq!(solo.len(), 1931, "and the one-to-one thread's");
    assert!(solo.iter().all(|(_, message)| message.conversation_title.is_none()), "a one-to-one match has no title to name");

    assert_eq!(named.iter().filter(|(_, message)| message.is_sender).count(), 1041, "sent");
    assert_eq!(named.iter().filter(|(_, message)| !message.is_sender).count(), 1547, "received");

    // The 2588-of-2588 result re-derived rather than assumed: a matched file is dated by its
    // message, and that date falls on the day its own filename leads with.
    for (item, message) in &named {
        let created = message.created.expect("every matched message in the corpus is dated");
        assert_eq!(item.date(), MediaDate::Message(created));
        assert_eq!(Day::from(created), item.media.file.day);
    }

    // 6413 ITEMS, not the 6877 FILES the fallback covers, and the gap is the same one the `NoToken`
    // count above carries: a zip pair's overlay half is not an item, it rides on its media half.
    // 5417 unnamed + 996 no-token here; + 464 zip overlays as files.
    assert_eq!(
        reconciliation.items.iter().filter(|item| matches!(item.date(), MediaDate::Filename(_))).count(),
        6413,
        "every file no message dated falls back to the day in its filename"
    );

    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    reconciliation.enroll(&mut manifest).unwrap();
    assert_eq!(manifest.items(ItemKind::ChatMedia).unwrap().len(), 9024, "9001 units + 23 gap tokens");
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

/// Read back through the manifest's own listing, never through the reconciliation that wrote it.
fn enrolled(manifest: &Manifest) -> Vec<(String, ItemStatus)> {
    manifest.items(ItemKind::ChatMedia).unwrap().into_iter().map(|item| (item.source_id, item.status)).collect()
}

fn row(manifest: &Manifest, source_id: &str) -> Item {
    manifest.item(ItemKind::ChatMedia, source_id).unwrap().expect("the row is enrolled")
}

#[test]
fn enrollment_gives_every_unit_and_every_gap_token_a_row_of_its_own() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let absent = format!("b~{}", id(9));
    let reconciliation = reconciled(
        &[("alice", &[&format!("b~{}", id(1)), &absent])],
        vec![
            bare("2021-03-04", 1),
            bare("2021-03-05", 2),
            plain("2021-03-04", Token::Overlay, 3, "png"),
            zip("2021-03-04", Token::Media, 4, "mp4"),
            zip("2021-03-04", Token::Overlay, 4, "png"),
        ],
    );

    reconciliation.enroll(&mut manifest).unwrap();

    // Five files, four units — the zip overlay rides on its media — plus the token with no file.
    assert_eq!(
        enrolled(&manifest),
        [
            (zip("2021-03-04", Token::Media, 4, "mp4").id, ItemStatus::Pending),
            (format!("b~{}", id(1)), ItemStatus::Pending),
            (format!("b~{}", id(2)), ItemStatus::Pending),
            (absent.clone(), ItemStatus::SourceMissing),
            (format!("overlay~{}", id(3)), ItemStatus::Pending),
        ]
    );
    assert!(row(&manifest, &absent).last_error.unwrap().contains("carries the id this message names"));
    // Chat media has no download links anywhere in the export, so no row can carry one.
    assert!(manifest.items(ItemKind::ChatMedia).unwrap().iter().all(|item| item.url.is_none()));

    let report = manifest.resume(ItemKind::ChatMedia).unwrap();
    assert_eq!(report.source_missing, 1);
    assert_eq!(report.pending, 4);
    // The gap is never handed back as work, so a run cannot spin on it.
    let owed: Vec<String> = manifest.pending(ItemKind::ChatMedia, 3).unwrap().into_iter().map(|item| item.source_id).collect();
    assert!(!owed.contains(&absent), "{owed:?}");
    assert_eq!(owed.len(), 4);

    // Re-running the same export changes nothing.
    reconciliation.enroll(&mut manifest).unwrap();
    assert_eq!(manifest.resume(ItemKind::ChatMedia).unwrap().source_missing, 1);
}

/// The manifest key is `(kind, source_id)` and `enroll` upserts, so two files claiming one id would
/// silently become one row with no trace of the second. They do not: the pairing keeps one and names
/// the other in `Discovery::duplicates`, and this reads the row count back through the manifest
/// rather than trusting that.
#[test]
fn a_duplicate_file_enrolls_one_row_and_the_copy_it_set_aside_is_still_named() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let first = plain_at("/export/a/chat_media", "2021-03-04", Token::B, 1, "jpg");
    let second = plain_at("/export/b/chat_media", "2021-03-04", Token::B, 1, "jpg");
    let reconciliation = reconcile(&history(&[("alice", &[])]), Discovery::from_files(vec![second.clone(), first.clone()], Vec::new()));

    reconciliation.enroll(&mut manifest).unwrap();

    assert_eq!(enrolled(&manifest), [(format!("b~{}", id(1)), ItemStatus::Pending)], "one id, one row");
    assert_eq!(reconciliation.duplicates.len(), 1, "and the copy the row does not cover is reported, not lost");
    assert_eq!(reconciliation.duplicates[0].kept, first.path);
    assert_eq!(reconciliation.duplicates[0].ignored, vec![second.path]);
}

/// `Media IDs` is untrusted json, and a token is not obliged to spell the `b~<id>` grammar. A token
/// spelling some OTHER file's `source_id` — `overlay~<id>`, `media~<id>`, a zip stem — must not be
/// able to park that file's row.
///
/// Before the boundary check this ran `SourceMissing` / `Pending` / `SourceMissing` on three
/// enrolls of identical input, and after run 3 the manifest offered no work at all for a file
/// sitting on disk. **Three runs is the load-bearing part**: one run catches only the first flip,
/// and two runs land on the `Pending` beat and read green.
#[test]
fn a_message_naming_a_file_no_token_can_reach_never_parks_that_files_row() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let overlay = plain("2021-03-04", Token::Overlay, 1, "png");
    let claimed = overlay.id.clone();
    let reconciliation = reconcile(&history(&[("alice", &[&claimed])]), Discovery::from_files(vec![overlay], Vec::new()));

    // The overlap the bug needed: the history names a string some item already answers to.
    assert_eq!(reconciliation.items.len(), 1);
    assert_eq!(reconciliation.items[0].source_id(), claimed);
    assert!(reconciliation.missing.is_empty(), "a token no file could ever carry is not this file's gap");

    let mut seen = Vec::new();
    for _ in 0..3 {
        reconciliation.enroll(&mut manifest).unwrap();
        seen.push(enrolled(&manifest));
    }

    let stable = vec![(claimed.clone(), ItemStatus::Pending)];
    assert_eq!(seen, vec![stable.clone(), stable.clone(), stable], "a present file's row must not oscillate");
    let owed: Vec<String> = manifest.pending(ItemKind::ChatMedia, 3).unwrap().into_iter().map(|item| item.source_id).collect();
    assert_eq!(owed, [claimed], "and it is still offered as work after three runs");
}

#[test]
fn nothing_enrolls_under_the_memories_kind() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();

    reconciled(&[("alice", &[&format!("b~{}", id(9))])], vec![bare("2021-03-04", 1)]).enroll(&mut manifest).unwrap();

    assert_eq!(manifest.items(ItemKind::ChatMedia).unwrap().len(), 2);
    assert!(manifest.items(ItemKind::Memory).unwrap().is_empty(), "the kind discriminator is what keeps two pipelines in one table");
}

/// The ceiling `memories::Reconciliation::enroll` documents, closed here: a gap token and the file
/// that turns up for it are the SAME `source_id`, so the second run walks the row back onto the work
/// list instead of stranding it and enrolling a second one.
#[test]
fn a_token_whose_file_turned_up_goes_back_on_the_work_list() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let token = format!("b~{}", id(1));
    let rows: &[(&str, &[&str])] = &[("alice", &[&token])];

    // First run: the export part holding the media had not been extracted yet.
    reconciled(rows, Vec::new()).enroll(&mut manifest).unwrap();
    assert_eq!(enrolled(&manifest), [(token.clone(), ItemStatus::SourceMissing)]);

    // Second run, with the part extracted.
    reconciled(rows, vec![bare("2021-03-04", 1)]).enroll(&mut manifest).unwrap();

    assert_eq!(enrolled(&manifest), [(token.clone(), ItemStatus::Pending)], "one token, one row, across both runs");
    assert_eq!(row(&manifest, &token).retry_count, 0);
    assert!(row(&manifest, &token).last_error.is_none(), "and the stale reason does not outlive the gap");
    let owed: Vec<String> = manifest.pending(ItemKind::ChatMedia, 3).unwrap().into_iter().map(|item| item.source_id).collect();
    assert_eq!(owed, [token]);
}

#[test]
fn re_enrolling_leaves_a_finished_item_alone() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let token = format!("b~{}", id(1));
    let reconciliation = reconciled(&[("alice", &[&token])], vec![bare("2021-03-04", 1)]);
    reconciliation.enroll(&mut manifest).unwrap();

    let output = workspace.dir.path().join("2021-03-04.jpg");
    fs::write(&output, b"repaired bytes").unwrap();
    manifest.mark_done(ItemKind::ChatMedia, &token, &output).unwrap();
    let finished = row(&manifest, &token);

    reconciliation.enroll(&mut manifest).unwrap();

    let after = row(&manifest, &token);
    assert_eq!(after.status, ItemStatus::Done, "a re-enroll must not un-finish work");
    assert_eq!(after.output_path, finished.output_path);
    assert_eq!(after.checksum, finished.checksum, "the checksum the resume sweep compares against");
    assert!(manifest.pending(ItemKind::ChatMedia, 3).unwrap().is_empty(), "and it is not offered as work again");
}

/// A file a message NAMES, gone between two runs: the shared identity carries the row across, so it
/// lands back at `SourceMissing` under the same `source_id`.
///
/// **This is the 27% case.** Its twin below is the other 73%, and the two names have to say which is
/// which — this test used to be called `a_file_that_vanished_lands_back_at_source_missing_under_the_
/// same_row`, which reads as the general statement and is not.
#[test]
fn a_vanished_file_a_message_names_lands_back_at_source_missing_under_the_same_row() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let token = format!("b~{}", id(1));
    let rows: &[(&str, &[&str])] = &[("alice", &[&token])];

    reconciled(rows, vec![bare("2021-03-04", 1)]).enroll(&mut manifest).unwrap();
    reconciled(rows, Vec::new()).enroll(&mut manifest).unwrap();

    assert_eq!(enrolled(&manifest), [(token, ItemStatus::SourceMissing)]);
}

/// The other 73%, and the case the shared identity above cannot cover: a file no message names has
/// no token, so once it vanishes nothing in a reconciliation can name it — absent from `items`
/// because there is no file, absent from `missing` because there is no token. That is the row the
/// manifest sweep retires. On the observed export it is every file no message names: 5417 unnamed
/// `b` + 532 role-worded + 928 zip, 6877 of 9465.
///
/// **The finished row beside them is the exemption, in the same fixture**: it is unnamed by exactly
/// the same enumeration, so a sweep that took every unnamed row would red here.
#[test]
fn a_vanished_file_no_message_names_is_retired() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let quiet: &[(&str, &[&str])] = &[("alice", &[])];
    let finished = format!("b~{}", id(3));

    reconciled(quiet, vec![bare("2021-03-04", 1), plain("2021-03-04", Token::Overlay, 2, "png"), bare("2021-03-05", 3)])
        .enroll(&mut manifest)
        .unwrap();
    let output = workspace.dir.path().join("2021-03-05.jpg");
    fs::write(&output, b"repaired bytes").unwrap();
    manifest.mark_done(ItemKind::ChatMedia, &finished, &output).unwrap();

    // Second run: every file is gone and no message ever mentioned any of them.
    reconciled(quiet, Vec::new()).enroll(&mut manifest).unwrap();

    assert_eq!(
        enrolled(&manifest),
        [(format!("b~{}", id(1)), ItemStatus::Retired), (finished, ItemStatus::Done), (format!("overlay~{}", id(2)), ItemStatus::Retired),],
        "the rows no run could finish are retired; the finished one is not"
    );
    assert!(manifest.pending(ItemKind::ChatMedia, 3).unwrap().is_empty(), "and nothing unfinishable is offered as work");
    assert_eq!(manifest.resume(ItemKind::ChatMedia).unwrap().retired, 2);
}

/// The guard, in this leg: a dir the walk could not list is not evidence the file is gone, so the
/// row this run cannot name SURVIVES rather than being retired.
#[test]
fn a_vanished_file_keeps_its_row_while_a_directory_could_not_be_listed() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let quiet: &[(&str, &[&str])] = &[("alice", &[])];
    let token = format!("b~{}", id(1));

    reconciled(quiet, vec![bare("2021-03-04", 1)]).enroll(&mut manifest).unwrap();

    // Second run: nothing found AND part of the source could not be listed, so "the file is gone" is
    // a claim this run never established.
    let locked = vec![UnreadableDir { dir: PathBuf::from("/export/mydata~1/chat_media"), kind: io::ErrorKind::PermissionDenied }];
    let unscanned = reconcile(&history(quiet), Discovery::from_walk(Vec::new(), Vec::new(), locked));
    assert_eq!(unscanned.unreadable.len(), 1, "the fixture is the partial-scan case");
    unscanned.enroll(&mut manifest).unwrap();

    assert_eq!(enrolled(&manifest), [(token.clone(), ItemStatus::Pending)], "a file merely never seen must not retire its row");

    // The same enumeration with the scan complete retires it, so it is the guard that left the row
    // standing rather than the sweep never reaching this fixture.
    reconciled(quiet, Vec::new()).enroll(&mut manifest).unwrap();
    assert_eq!(enrolled(&manifest), [(token, ItemStatus::Retired)]);
}

/// Retiring is reversible: the file comes back under the id its retired row already carries, so the
/// row goes back on the work list instead of sitting parked for ever.
#[test]
fn a_retired_file_that_came_back_goes_back_on_the_work_list() {
    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let quiet: &[(&str, &[&str])] = &[("alice", &[])];
    let token = format!("b~{}", id(1));

    reconciled(quiet, vec![bare("2021-03-04", 1)]).enroll(&mut manifest).unwrap();
    reconciled(quiet, Vec::new()).enroll(&mut manifest).unwrap();
    assert_eq!(enrolled(&manifest), [(token.clone(), ItemStatus::Retired)], "the run that unmounted the part");

    // Third run, with the part back.
    reconciled(quiet, vec![bare("2021-03-04", 1)]).enroll(&mut manifest).unwrap();

    assert_eq!(enrolled(&manifest), [(token.clone(), ItemStatus::Pending)]);
    assert_eq!(row(&manifest, &token).retry_count, 0);
    assert!(row(&manifest, &token).last_error.is_none(), "and the retirement note does not outlive the retirement");
    let owed: Vec<String> = manifest.pending(ItemKind::ChatMedia, 3).unwrap().into_iter().map(|item| item.source_id).collect();
    assert_eq!(owed, [token]);
}

/// The convention `Reconciliation::enroll`'s ordering rests on, pinned where it is PRODUCED rather
/// than where it is checked: no gap token can spell an item's own `source_id`. `parse_history_token`
/// is what buys it, over every shape a file's id can take.
#[test]
fn a_gap_token_and_an_item_id_are_never_the_same_string() {
    let overlay = plain("2021-03-04", Token::Overlay, 3, "png");
    let zip_media = zip("2021-03-04", Token::Media, 4, "mp4");
    // Every spelling an id can take, handed back to the history as a token. The role-worded ones are
    // driven off `Token::ALL` rather than hand-listed, so a fifth role word cannot be added without
    // this witness considering it — a hand-rolled list is what missed a member in this crate before.
    // `Token::B` is excluded because its two cases are spelled out below, one file on disk and one
    // not, and those are the pair the disjointness question actually turns on.
    let role_worded = Token::ALL.iter().filter(|token| **token != Token::B).map(|token| format!("{token}~{}", id(5)));
    let spellings: Vec<String> =
        role_worded.chain([overlay.id.clone(), zip_media.id.clone(), format!("b~{}", id(1)), format!("b~{}", id(9))]).collect();
    let named: Vec<&str> = spellings.iter().map(String::as_str).collect();
    let files = vec![bare("2021-03-04", 1), overlay, zip_media, zip("2021-03-04", Token::Overlay, 4, "png")];

    let reconciliation = reconcile(&history(&[("alice", named.as_slice())]), Discovery::from_files(files, Vec::new()));

    let items: BTreeSet<&str> = reconciliation.items.iter().map(ChatMediaItem::source_id).collect();
    let gaps: BTreeSet<&str> = reconciliation.missing.iter().map(|missing| missing.token.as_str()).collect();
    // The pin first, then the control that says it was not vacuous: a sibling assertion above it
    // would abort the body before the named one ever ran.
    assert!(items.is_disjoint(&gaps), "items {items:?} and gaps {gaps:?} share a source id");
    assert_eq!(gaps.len(), 1, "the fixture has to produce a gap or it pins nothing: {gaps:?}");
    // Everything but the two `b~` spellings, derived from the fixture so a new role word moves both
    // sides of this at once instead of redding a literal nobody can place.
    assert_eq!(
        reconciliation.unparsed_tokens.len(),
        spellings.len() - 2,
        "the spellings that never reach the manifest at all: {:?}",
        reconciliation.unparsed_tokens
    );
}

/// The guard on that convention, driven from the state that would break it: a `Reconciliation` whose
/// gap token spells one of its own items' ids. `enroll` would then mark that row source-missing AND
/// reset it in one call, and which one won would be decided by where the `Manifest::items` read sits
/// — an ordering nothing pins. So the overlap stops the run at the call it would race, instead of
/// surfacing later as a row that flips status on alternate runs, which is how it was found the first
/// time.
///
/// Debug-only because the guard is a `debug_assert`: the release leg compiles the check out, and this
/// test with it.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "spells an item's own source id")]
fn an_overlapping_gap_token_and_item_id_stop_the_run_rather_than_racing_the_read() {
    // Named here rather than at the top of the file: the release leg compiles this test out, and an
    // import only it needs would be an unused one there.
    use exportsnap::export::chat_media::MissingMedia;

    let workspace = Workspace::new();
    let mut manifest = workspace.open();
    let file = bare("2021-03-04", 1);
    let token = file.id.clone();
    // Exactly what loosening `parse_history_token` would let `reconcile` build.
    let overlapping = Reconciliation {
        items: vec![ChatMediaItem { media: ChatMedia { file, overlay: None }, join: Join::Unnamed }],
        missing: vec![MissingMedia {
            token,
            message: MessageRef { conversation: 0, message: 0 },
            conversation: ConversationId::new("friend"),
            reason: MissingReason::NoFile,
        }],
        unparsed_tokens: Vec::new(),
        unparsed: Vec::new(),
        duplicates: Vec::new(),
        unreadable: Vec::new(),
    };

    let _ = overlapping.enroll(&mut manifest);
}
