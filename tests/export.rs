//! Public-API tests for `exportsnap::export`: the serde schema, the validated model types, and
//! the `json/` dir loader.
//!
//! **Fixture literals die on a regeneration.** Every `user_N` / `conv_N` handle, every
//! `redacted-xxxxxx` token, and every date below is read from the `fixtures/` tree currently on
//! disk. The redactor seeds its handles and its one date offset randomly per run
//! (`docs/handoff-state.md`, decision 20), so re-running it invalidates all of them at once.
//! That is expected: rewrite the literals from the new fixtures, do not loosen the assertions.
//!
//! **Fixture arrays are truncated to their first 25 elements.** No count below is a real export
//! total; the true lengths live in `fixtures/_redaction_report.json`.
//!
//! `fixtures/` is gitignored, so CI has none. Every test that reads it prints a notice and
//! returns instead of failing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use exportsnap::export::model::{
    Account, ChatMessage, ConversationId, DownloadUrl, Field, Friend, LocationPoint, MediaKind, MessageText, ParseErrorKind, Timestamp,
    Username,
};
use exportsnap::export::{ExportJson, LoadError, SCHEMA_FILES, schema};

fn fixtures_root() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    dir.is_dir().then_some(dir)
}

/// `None` only when `fixtures/` itself is absent, which is the CI case and the one that skips.
///
/// A `fixtures/` that exists but does not hold exactly one `mydata~*/json` panics instead. Those
/// are different situations and collapsing them is how ten tests quietly become no-ops after a
/// regeneration lands a differently-shaped tree — green, locally, with nothing to notice.
fn fixture_json_dir() -> Option<PathBuf> {
    let root = fixtures_root()?;
    let mut parts: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            let is_part = path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("mydata~"));
            (is_part && path.join("json").is_dir()).then_some(path)
        })
        .collect();
    parts.sort();
    assert_eq!(
        parts.len(),
        1,
        "{} exists but holds {} `mydata~*/json` dirs, expected exactly 1 — fix the tree or this helper",
        root.display(),
        parts.len()
    );
    parts.pop().map(|part| part.join("json"))
}

/// Binds the fixture dir, or prints why the test did nothing and returns.
macro_rules! json_dir_or_skip {
    () => {
        match fixture_json_dir() {
            Some(dir) => dir,
            None => {
                println!("skipping: fixtures/ is absent (gitignored, so CI never has it)");
                return;
            }
        }
    };
}

fn load_fixture(dir: &Path) -> ExportJson {
    ExportJson::load_dir(dir).expect("the fixture export must load")
}

/// A scratch dir under cargo's own test tmpdir, emptied first so a rerun starts clean.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

// ---- validated primitives (no fixtures needed) ----

#[test]
fn timestamp_parses_the_export_form_into_components() {
    let stamp = Timestamp::parse(Field::Created, "2020-08-02 12:45:39 UTC").unwrap();
    assert_eq!(stamp.year(), 2020);
    assert_eq!(stamp.month(), 8);
    assert_eq!(stamp.day(), 2);
    assert_eq!(stamp.hour(), 12);
    assert_eq!(stamp.minute(), 45);
    assert_eq!(stamp.second(), 39);
    assert_eq!(stamp.to_string(), "2020-08-02 12:45:39 UTC");
}

/// `Created(microseconds)`'s value read as the instant it actually names.
///
/// Every literal here is a boundary the conversion can be got wrong at, and each fails as a
/// different value rather than as a shared `None`:
///
/// - the UNIT. The key says microseconds and the wire holds milliseconds, so the same integer read
///   as microseconds lands in 1970 and read as seconds lands past year 52000.
/// - the TRUNCATION. `…_675` is 675ms past the second and the second stays `05`.
/// - the SIGN. A negative value is pre-1970 rather than an error, and it truncates toward the past,
///   so `-1` is 1969-12-31 23:59:59 and not 1970-01-01 00:00:00.
/// - the two `None` arms, which are different refusals: chrono has no date at all at `i64::MIN`,
///   while the year-100000 case is a date chrono holds happily and [`Timestamp`] cannot, its year
///   being a `u16`. A build dropping the `u16` check would truncate that year rather than refuse it.
#[test]
fn timestamp_reads_a_millisecond_epoch_as_the_instant_it_names() {
    let stamp = Timestamp::from_epoch_ms(1_595_778_485_675).unwrap();
    assert_eq!(stamp.year(), 2020);
    assert_eq!(stamp.month(), 7);
    assert_eq!(stamp.day(), 26);
    assert_eq!(stamp.hour(), 15);
    assert_eq!(stamp.minute(), 48);
    assert_eq!(stamp.second(), 5, "675ms past the second truncates down, never up");

    assert_eq!(Timestamp::from_epoch_ms(0).unwrap().to_string(), "1970-01-01 00:00:00 UTC", "the conversion itself has no sentinel");
    assert_eq!(Timestamp::from_epoch_ms(-1).unwrap().to_string(), "1969-12-31 23:59:59 UTC");

    assert_eq!(Timestamp::from_epoch_ms(i64::MIN), None, "outside every calendar");
    // 3.09e15 ms is roughly year 99000: a date chrono represents and this type's `u16` year cannot.
    assert_eq!(Timestamp::from_epoch_ms(3_090_000_000_000_000), None, "a year no `u16` holds is refused, not truncated");
}

/// The zero sentinel is the MODEL's rule and not the chat leg's, so it has to hold on the snap
/// record too — the two carry the same key off the same exporter, and a divergence would only
/// surface once something joined the two histories.
#[test]
fn a_snap_reads_both_spellings_of_an_absent_epoch_as_absence() {
    let snap = |created_epoch| {
        exportsnap::export::model::Snap::try_from(schema::SnapEntry { created_epoch, ..schema::SnapEntry::default() }).unwrap()
    };
    assert_eq!(snap(None).created_epoch_ms, None, "the key missing");
    assert_eq!(snap(Some(0)).created_epoch_ms, None, "the key present, holding the export's own empty spelling");
    assert_eq!(snap(Some(1_596_380_554_698)).created_epoch_ms, Some(1_596_380_554_698), "and a stated one survives verbatim");
    // The rule is about the ENCODING's empty spelling, not about which instants are plausible, so a
    // negative passes: it is a value the field states. Widening to `<= 0` would make this integer a
    // plausibility filter while the `Created` string beside it stays none — and `Timestamp::parse`
    // honours "1900-01-01 00:00:00 UTC" while `local_fix::system_time` deliberately writes mtimes
    // on both sides of the epoch, so this crate has no floor for it to be consistent with.
    assert_eq!(snap(Some(-1)).created_epoch_ms, Some(-1), "only `0` is the sentinel");
}

#[test]
fn timestamp_ordering_is_chronological() {
    let older = Timestamp::parse(Field::Created, "2019-12-31 23:59:59 UTC").unwrap();
    let newer = Timestamp::parse(Field::Created, "2020-01-01 00:00:00 UTC").unwrap();
    assert!(older < newer);
    assert!(
        Timestamp::parse(Field::Created, "2020-01-01 00:00:00 UTC").unwrap()
            < Timestamp::parse(Field::Created, "2020-01-01 00:01:00 UTC").unwrap()
    );
}

#[test]
fn timestamp_rejects_every_shape_the_export_does_not_write() {
    let rejected = [
        ("no zone", "2020-08-02 12:45:39"),
        ("a zone that is not utc", "2020-08-02 12:45:39 PST"),
        ("an offset zone", "2020-08-02 12:45:39 +0200"),
        ("iso separator", "2020-08-02T12:45:39 UTC"),
        ("month 13", "2020-13-02 12:45:39 UTC"),
        ("month 0", "2020-00-02 12:45:39 UTC"),
        ("day 32", "2020-08-32 12:45:39 UTC"),
        ("day 0", "2020-08-00 12:45:39 UTC"),
        ("hour 24", "2020-08-02 24:45:39 UTC"),
        ("minute 60", "2020-08-02 12:60:39 UTC"),
        ("second 60", "2020-08-02 12:45:60 UTC"),
        ("unpadded month", "2020-8-02 12:45:39 UTC"),
        ("two-digit year", "20-08-02 12:45:39 UTC"),
        ("signed component", "2020-+8-02 12:45:39 UTC"),
        ("a fourth date part", "2020-08-02-01 12:45:39 UTC"),
        ("empty", ""),
        ("free text", "redacted-q990pb"),
    ];
    for (why, text) in rejected {
        assert!(Timestamp::parse(Field::Created, text).is_err(), "{why}: {text:?} should not parse");
    }
}

#[test]
fn a_rejected_timestamp_names_its_field_and_its_value() {
    let error = Timestamp::parse(Field::CreationTimestamp, "not a date").unwrap_err();
    assert_eq!(error.field(), Field::CreationTimestamp);
    assert_eq!(error.kind(), ParseErrorKind::Timestamp);
    assert_eq!(error.value(), "not a date");
    // The value is deliberately absent from the rendered form: this string reaches a footer alert
    // and `Field` admits `Location`, so what used to render here could be a coordinate. It stays
    // reachable through `value()` for a caller with somewhere safe to put it.
    // `"not a date"` is 10 characters — the SHAPE, not the value. A bare drop would render the
    // same sentence for an empty string, an ISO-8601 date and 400 KB of garbage, and this arm
    // carries no line/column to fall back on the way the serde one does.
    assert_eq!(error.to_string(), "Creation Timestamp: expected a \"YYYY-MM-DD HH:MM:SS UTC\" timestamp, got 10 chars");
    assert!(!error.to_string().contains("not a date"), "the offending value must not reach a footer-bound message");
}

/// **A rejected coordinate must not put the coordinate on screen.**
///
/// `ParseError`'s `Display` reaches a footer alert through `LoadError::Invalid`, and `Field` admits
/// `Location` — so the value it renders can be a lat/long. The restriction on `Field` was a decision
/// that a message body cannot reach this error; it was never a decision that a location may.
///
/// The reason this is not the same fix as the serde one, and must not become it: the format
/// specification is itself quoted, so a delimiter scan over the rendered message would strip the
/// thing the user actually needs. Both halves are asserted here — the coordinate gone, the form that
/// tells you how to write one intact.
#[test]
fn a_rejected_coordinate_names_the_form_it_wanted_and_never_the_coordinate() {
    // Past the pole, so it is REJECTED — but the longitude beside it is a real one, which is the
    // point: a rejected pair still carries a usable location and the error still renders it.
    let error = LocationPoint::parse(Field::Location, "Latitude, Longitude: 91.858844, 2.294351").unwrap_err();
    let rendered = error.to_string();

    // The control: the parser really did take this value, so the sweep below is not vacuous.
    assert_eq!(error.value(), "Latitude, Longitude: 91.858844, 2.294351");

    for fragment in ["91.858844", "2.294351"] {
        assert!(!rendered.contains(fragment), "a coordinate reached a footer-bound message: {rendered}");
    }
    // What must survive: the field, and the quoted form spec a delimiter scan would have eaten.
    assert!(rendered.starts_with("Location: expected"), "{rendered}");
    assert!(rendered.contains(r#""Latitude, Longitude: <lat>, <lon>""#), "the form spec is the diagnostic: {rendered}");
    assert!(rendered.contains("-90..=90"), "{rendered}");
    // The shape, in place of the value. Length is what the ruling kept; the ceiling on
    // `ParseError::fmt` records that for THIS kind it separates the likely drifts from each
    // other not at all — a locale decimal comma, a separator swap and a well-formed pair are
    // all 16 characters.
    assert!(rendered.ends_with("got 40 chars"), "{rendered}");
}

#[test]
fn location_point_parses_the_export_form() {
    let point = LocationPoint::parse(Field::Location, "Latitude, Longitude: 51.5, -0.125").unwrap();
    assert_eq!(point.latitude(), 51.5);
    assert_eq!(point.longitude(), -0.125);
}

#[test]
fn location_point_label_is_case_insensitive_but_still_required() {
    assert!(LocationPoint::parse(Field::Location, "LATITUDE, LONGITUDE: 1.0, 2.0").is_ok());
    assert!(LocationPoint::parse(Field::Location, "Lat, Lon: 1.0, 2.0").is_err());
    assert!(LocationPoint::parse(Field::Location, "1.0, 2.0").is_err());
}

#[test]
fn location_point_rejects_coordinates_outside_the_globe() {
    let rejected = [
        ("latitude past the pole", "Latitude, Longitude: 90.1, 0.0"),
        ("latitude past the other pole", "Latitude, Longitude: -90.1, 0.0"),
        ("longitude past the meridian", "Latitude, Longitude: 0.0, 180.1"),
        ("longitude past the other meridian", "Latitude, Longitude: 0.0, -180.1"),
        ("not a number", "Latitude, Longitude: north, east"),
        ("nan", "Latitude, Longitude: NaN, 0.0"),
        ("infinite", "Latitude, Longitude: inf, 0.0"),
        ("one coordinate", "Latitude, Longitude: 1.0"),
    ];
    for (why, text) in rejected {
        assert!(LocationPoint::parse(Field::Location, text).is_err(), "{why}: {text:?} should not parse");
    }
    assert!(LocationPoint::parse(Field::Location, "Latitude, Longitude: 90.0, 180.0").is_ok());
    assert!(LocationPoint::parse(Field::Location, "Latitude, Longitude: -90.0, -180.0").is_ok());
}

#[test]
fn a_rejected_coordinate_pair_names_its_kind() {
    let error = LocationPoint::parse(Field::Location, "Latitude, Longitude: 91.0, 0.0").unwrap_err();
    assert_eq!(error.kind(), ParseErrorKind::Coordinates);
    assert_eq!(error.field(), Field::Location);
}

#[test]
fn media_kind_keeps_the_words_it_knows_and_carries_the_ones_it_does_not() {
    // Kept as hand-written literals rather than folded into `model.rs`'s `KNOWN`-driven round
    // trip: `KNOWN` has no `pub`, so a test here cannot iterate it, and these independent
    // expectations are what catches a member being DELETED from `KNOWN` — the round trip only
    // cross-checks whatever currently IS in `KNOWN` against itself, so a deletion shrinks its own
    // loop rather than reding it.
    assert_eq!(MediaKind::from_wire("TEXT"), MediaKind::Text);
    assert_eq!(MediaKind::from_wire("MEDIA"), MediaKind::Media);
    assert_eq!(MediaKind::from_wire("STATUS"), MediaKind::Status);
    assert_eq!(MediaKind::from_wire("NOTE"), MediaKind::Note);
    assert_eq!(MediaKind::from_wire("STICKER"), MediaKind::Sticker);
    assert_eq!(MediaKind::from_wire("IMAGE"), MediaKind::Image);
    assert_eq!(MediaKind::from_wire("VIDEO"), MediaKind::Video);
    assert_eq!(MediaKind::from_wire("PHOTO"), MediaKind::Other("PHOTO".to_owned()));

    for word in ["TEXT", "MEDIA", "STATUS", "NOTE", "STICKER", "IMAGE", "VIDEO", "PHOTO"] {
        assert_eq!(MediaKind::from_wire(word).as_wire(), word);
    }
}

#[test]
fn the_word_is_matched_without_regard_to_case_and_other_still_keeps_its_spelling() {
    // Title case is memories' spelling and shouting is chat's and snap's; both name one variant.
    assert_eq!(MediaKind::from_wire("Image"), MediaKind::Image);
    assert_eq!(MediaKind::from_wire("Video"), MediaKind::Video);
    assert_eq!(MediaKind::from_wire("text"), MediaKind::Text);
    assert_eq!(MediaKind::from_wire("sTiCkEr"), MediaKind::Sticker);

    // A placed word comes back in the canonical spelling, never the caller's.
    assert_eq!(MediaKind::from_wire("Image").as_wire(), "IMAGE");
    // An unplaced one keeps its own, case included, because the spelling is all that is known.
    assert_eq!(MediaKind::from_wire("Photo"), MediaKind::Other("Photo".to_owned()));
    assert_eq!(MediaKind::from_wire("Photo").as_wire(), "Photo");
    assert_eq!(MediaKind::from_wire(""), MediaKind::Other(String::new()));
}

/// The three real `Media Type` vocabularies, one file at a time
/// (`docs/design.md`, observed export shape; n=1).
#[test]
fn every_media_type_word_the_real_export_writes_lands_where_it_belongs() {
    // chat_history.json. `SHARE` and the whole `STATUS…` family are their own words: the match is
    // against the whole word and never a prefix, so none of them folds into `Status`.
    for (word, expected) in [
        ("TEXT", MediaKind::Text),
        ("MEDIA", MediaKind::Media),
        ("NOTE", MediaKind::Note),
        ("STICKER", MediaKind::Sticker),
        ("STATUS", MediaKind::Status),
        ("SHARE", MediaKind::Other("SHARE".to_owned())),
        ("STATUSSAVETOCAMERAROLL", MediaKind::Other("STATUSSAVETOCAMERAROLL".to_owned())),
        ("STATUSPARTICIPANTADDED", MediaKind::Other("STATUSPARTICIPANTADDED".to_owned())),
        ("STATUSERASEDSNAPMESSAGE", MediaKind::Other("STATUSERASEDSNAPMESSAGE".to_owned())),
        ("STATUSNAMECHANGED", MediaKind::Other("STATUSNAMECHANGED".to_owned())),
    ] {
        assert_eq!(MediaKind::from_wire(word), expected, "chat_history.json writes {word}");
    }

    // snap_history.json.
    assert_eq!(MediaKind::from_wire("IMAGE"), MediaKind::Image);
    assert_eq!(MediaKind::from_wire("VIDEO"), MediaKind::Video);

    // memories_history.json, title case. These two are the reason the match ignores case at all:
    // `export::memories` buckets by the kind, and `Other` buckets as unknown.
    assert_eq!(MediaKind::from_wire("Image"), MediaKind::Image);
    assert_eq!(MediaKind::from_wire("Video"), MediaKind::Video);
}

#[test]
fn secrets_never_reach_a_debug_line() {
    let url = DownloadUrl::new("https://cf-st.sc-cdn.net/some/signed/path?sig=deadbeef");
    assert_eq!(format!("{url:?}"), "DownloadUrl(<redacted>)");
    assert_eq!(url.expose(), "https://cf-st.sc-cdn.net/some/signed/path?sig=deadbeef");

    let text = MessageText::new("see you at the usual place");
    assert_eq!(format!("{text:?}"), "MessageText(<redacted>)");
    assert_eq!(text.expose(), "see you at the usual place");

    // The wrappers are what protects a struct that derives Debug over them.
    let message = ChatMessage::try_from(schema::ChatEntry {
        from: "user_9".to_owned(),
        media_type: "TEXT".to_owned(),
        content: Some("see you at the usual place".to_owned()),
        ..Default::default()
    })
    .unwrap();
    let rendered = format!("{message:?}");
    assert!(!rendered.contains("usual place"), "{rendered}");
    assert!(rendered.contains("MessageText(<redacted>)"), "{rendered}");
}

#[test]
fn an_empty_string_is_absence_not_a_value() {
    let friend = Friend::try_from(schema::FriendEntry {
        username: "user_1".to_owned(),
        display_name: String::new(),
        creation_timestamp: String::new(),
        last_modified_timestamp: "2018-01-13 12:23:18 UTC".to_owned(),
        source: String::new(),
    })
    .unwrap();
    assert_eq!(friend.username, Username::new("user_1"));
    assert_eq!(friend.display_name, None);
    assert_eq!(friend.created, None);
    assert_eq!(friend.last_modified.unwrap().to_string(), "2018-01-13 12:23:18 UTC");
    assert_eq!(friend.source, None);
}

#[test]
fn a_non_empty_value_that_will_not_parse_is_an_error() {
    let error = Friend::try_from(schema::FriendEntry {
        username: "user_1".to_owned(),
        creation_timestamp: "13/01/2018".to_owned(),
        ..Default::default()
    })
    .unwrap_err();
    assert_eq!(error.field(), Field::CreationTimestamp);
    assert_eq!(error.value(), "13/01/2018");
}

#[test]
fn a_missing_section_parses_as_an_empty_one() {
    let account = Account::try_from(serde_json::from_str::<schema::Account>("{}").unwrap()).unwrap();
    assert_eq!(account.basics.username, None);
    assert_eq!(account.basics.created, None);
    assert!(account.device_history.is_empty());
    assert!(account.logins.is_empty());
    assert!(account.associated_accounts.is_empty());
}

#[test]
fn an_empty_handle_never_becomes_a_username() {
    // A `Username("")` is a join key that matches nothing useful and is indistinguishable from a
    // real one, so the constructor refuses it and every holder is an `Option`.
    assert_eq!(Username::new(""), None);
    assert_eq!(Username::new("user_1").unwrap().as_str(), "user_1");

    let friend = Friend::try_from(schema::FriendEntry { username: String::new(), ..Default::default() }).unwrap();
    assert_eq!(friend.username, None);

    let message = ChatMessage::try_from(schema::ChatEntry { from: String::new(), ..Default::default() }).unwrap();
    assert_eq!(message.from, None);

    // A conversation key is the opposite call: it is an opaque map key, so an empty one still
    // names a thread that holds records, and refusing it would discard them.
    assert_eq!(ConversationId::new("").as_str(), "");
}

#[test]
fn a_section_whose_element_shape_is_unknown_does_not_fail_the_load() {
    // `App Interactions` is `[]` in the one observed export. If it were typed from its sibling
    // `Web Interactions`, an export that populates it with objects would fail the WHOLE load,
    // because `load_dir` is fail-fast.
    let dir = scratch("unknown-element-shape");
    fs::write(dir.join("user_profile.json"), br#"{"Interactions":{"Web Interactions":["a"],"App Interactions":[{"App":"x","Count":2}]}}"#)
        .unwrap();

    let profile = ExportJson::load_dir(&dir).unwrap().user_profile.unwrap();
    assert_eq!(profile.web_interactions, ["a"]);
}

#[test]
fn schema_files_is_sorted_deduplicated_and_names_every_observed_file() {
    // No fixed count here: membership is a union of what's been observed, not a contract
    // (`docs/design.md`), so a length pinned as a magic number would rot on the next observation.
    let mut sorted = SCHEMA_FILES;
    sorted.sort_unstable();
    assert_eq!(sorted, SCHEMA_FILES, "SCHEMA_FILES must stay sorted");
    let unique: std::collections::BTreeSet<_> = SCHEMA_FILES.iter().collect();
    assert_eq!(unique.len(), SCHEMA_FILES.len(), "SCHEMA_FILES must hold no duplicate");

    // The full literal, pinned unconditionally. `fixtures/` is gitignored and untracked, so
    // every fixture-backed test below this one skips in CI — this is the only SCHEMA_FILES check
    // that runs there, and the only one that catches a member's SPELLING drifting rather than
    // just its arity or sort order. The two-place edit this forces on a newly observed name IS
    // the point, not a cost to apologise for: the red is the prompt to ask whether the new name
    // is real before absorbing it, the same role `ItemKind::ALL`/`ItemStatus::ALL` play at
    // `tests/manifest.rs:615-617`. `memories_history.json` and `in_app_reports.json` are the two
    // names the 2026-07-26 and 2026-08-04 exports disagree on; every other name below is shared.
    assert_eq!(
        SCHEMA_FILES,
        [
            "account.json",
            "account_history.json",
            "bitmoji.json",
            "chat_history.json",
            "custom_sticker.json",
            "email_campaign_history.json",
            "feature_emails.json",
            "friends.json",
            "in_app_reports.json",
            "location_history.json",
            "memories_history.json",
            "ranking.json",
            "snap_ads.json",
            "snap_history.json",
            "snap_pro.json",
            "snapchat_ai.json",
            "snapchat_plus.json",
            "story_history.json",
            "terms_history.json",
            "user_profile.json",
        ]
    );
}

/// Reads the redactor's `REAL_SCHEMA_FILENAMES` tuple out of `tools/test_redact_export.py` by
/// text, via `CARGO_MANIFEST_DIR` (`tools/` is tracked, so this reaches the file in CI too, unlike
/// anything gated on `fixtures/`). The scan is line-oriented, section-scoped the same way
/// `ignored_advisories` in `tests/supply_chain.rs` scopes to `[advisories]`: it reads a literal
/// only when trimming a line leaves that literal alone on it, between the line where
/// `REAL_SCHEMA_FILENAMES = (` starts and the line that is exactly `)`. What a line outside that
/// shape does is this function's problem, not this comment's: the empty-guard in
/// `schema_files_and_the_redactors_real_schema_filenames_agree` is what turns an unreadable file
/// into a red instead of a silent "agrees with nothing".
///
/// Two boundaries this scan does not close, named rather than assumed impossible:
/// - the anchor line's `continue` skips the rest of that same line unconditionally. If
///   `REAL_SCHEMA_FILENAMES = (` and its entries — or its closing `)` — ever land on one line (the
///   tuple collapsed onto fewer lines than it holds today), the skipped content is not merely
///   missed: the scan stays "inside" past the real close and can read unrelated later lines as
///   names, a wrong result rather than an empty one, which the empty-guard cannot be relied on to
///   catch. Nothing about today's 20-name tuple makes this unreachable; it is simply not the file's
///   current shape.
/// - the equality check below compares `BTreeSet`s, so it sees membership, not multiplicity: a name
///   duplicated only inside `REAL_SCHEMA_FILENAMES`, where the duplicate already matches a name
///   `SCHEMA_FILES` also holds, changes neither set and is invisible here.
///   `schema_files_is_sorted_deduplicated_and_names_every_observed_file` already catches a
///   duplicate on the `SCHEMA_FILES` side; task 35 is scoped to a name landing on only one side,
///   not to multiplicity on either, so this is a stated gap in that scope, not a silent one.
fn real_schema_filenames() -> Vec<String> {
    let body = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/test_redact_export.py")).unwrap();
    let mut inside = false;
    let mut names = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if !inside {
            inside = trimmed.starts_with("REAL_SCHEMA_FILENAMES = (");
            continue;
        }
        if trimmed == ")" {
            break;
        }
        if let Some(name) = trimmed.strip_prefix('"').and_then(|rest| rest.strip_suffix("\",").or_else(|| rest.strip_suffix('"'))) {
            names.push(name.to_owned());
        }
    }
    names
}

#[test]
fn schema_files_and_the_redactors_real_schema_filenames_agree() {
    // Task 35: `SCHEMA_FILES` above and the redactor's `REAL_SCHEMA_FILENAMES`
    // (`tools/test_redact_export.py`) are two independently maintained copies of the same union of
    // real export schema filenames, and nothing but this test compares them. The redactor's own
    // tests run only via `python3 -m unittest discover -s tools -p 'test*.py'`, which `cargo.sh`
    // never invokes and CI does not run, so a pin living only on the Python side would never catch
    // a name landing on one side and not the other. This test lives on the Rust side instead, in
    // the suite `cargo.sh` always runs.
    let scraped = real_schema_filenames();
    // Guards the SCRAPE, not the content: if the tuple gets renamed, reformatted onto one line, or
    // the file moves, this is what turns "found nothing, so nothing disagreed" into a red instead
    // of a silent green that only means the extraction broke.
    assert!(
        !scraped.is_empty(),
        "found no REAL_SCHEMA_FILENAMES entries in tools/test_redact_export.py: either \
         `REAL_SCHEMA_FILENAMES = (` was not found verbatim, or the tuple closed before any \
         quoted name was read"
    );

    let rust_side: std::collections::BTreeSet<&str> = SCHEMA_FILES.iter().copied().collect();
    let python_side: std::collections::BTreeSet<&str> = scraped.iter().map(String::as_str).collect();
    assert_eq!(
        rust_side, python_side,
        "SCHEMA_FILES (src/export/mod.rs) and REAL_SCHEMA_FILENAMES (tools/test_redact_export.py) \
         disagree — update both, they are meant to hold the same union"
    );
}

// ---- the loader's own failure modes (no fixtures needed) ----

#[test]
fn a_file_snapchat_did_not_ship_is_absence_not_failure() {
    let dir = scratch("only-one-file");
    fs::write(dir.join("ranking.json"), br#"{"Statistics":{"Snapscore":"1"}}"#).unwrap();

    let loaded = ExportJson::load_dir(&dir).unwrap();
    assert_eq!(loaded.ranking.as_ref().unwrap().statistics.snapscore, "1");
    assert!(loaded.account.is_none());
    assert!(loaded.chat_history.is_none());
    assert!(loaded.friends.is_none());
    assert!(loaded.memories.is_none());
    assert!(loaded.snap_history.is_none());
    assert!(loaded.user_profile.is_none());
    assert!(loaded.snap_ads.is_none());
}

#[test]
fn a_file_that_is_not_json_fails_the_load_and_names_itself() {
    let dir = scratch("broken-json");
    fs::write(dir.join("friends.json"), b"{ not json").unwrap();

    let error = ExportJson::load_dir(&dir).unwrap_err();
    let LoadError::Json { file, .. } = &error else {
        panic!("expected a Json error, got {error:?}");
    };
    assert_eq!(*file, "friends.json");
    assert!(error.to_string().starts_with("friends.json is not valid json ("), "{error}");
    assert!(error.to_string().contains("re-extract"), "{error}");
}

#[test]
fn well_formed_json_in_the_wrong_shape_is_not_told_to_re_extract() {
    // `serde_json::Error` covers broken bytes and a type mismatch on perfectly good json. Only
    // the first is worth re-unzipping; sending someone round that loop for the second wastes it.
    let dir = scratch("wrong-shape-json");
    fs::write(dir.join("story_history.json"), br#"{"Your Story Views":[{"Story Views":null}]}"#).unwrap();

    let error = ExportJson::load_dir(&dir).unwrap_err();
    let LoadError::Json { file, .. } = &error else {
        panic!("expected a Json error, got {error:?}");
    };
    assert_eq!(*file, "story_history.json");
    let rendered = error.to_string();
    assert!(rendered.contains("valid json in a shape this build does not know"), "{rendered}");
    assert!(rendered.contains("needs a parser update"), "{rendered}");
    assert!(!rendered.contains("re-extract"), "{rendered}");
    assert!(rendered.contains("line 1 column"), "{rendered}");
}

/// **A parse failure must not put the file's own content on screen.**
///
/// `LoadError`'s `Display` reaches a footer alert verbatim on two screens, and for
/// `chat_history.json` the value serde quotes back is a message body. This is the loader's property,
/// not a screen's, which is why it is pinned here.
///
/// **Non-vacuous by construction, and that is the whole design of the test.** Each case asserts the
/// marker IS in the raw `serde_json::Error` and is NOT in the rendered `LoadError`. The first half
/// proves the marker reached the parser and that serde really did quote it back — without it, a
/// clean second half would be indistinguishable from a fixture the parser never objected to. The
/// two together also pin *the redaction* as the thing that removed it, rather than some accident of
/// the message shape.
///
/// The battery covers every delimited position a value can reach through THIS loader: a string, a
/// float (a coordinate is a float, and `location_history.json` is full of them), an integer, and an
/// integer past `u64` — which does not take the exotic route it looks like it should, see below.
///
/// **The last case is the one the others cannot catch.** Every quote-free marker passes with the
/// escape handling deleted, because the naive scan closes on the value's own closing quote. Only a
/// marker containing a `"` — which `{:?}` renders as `\"` — separates a correct redactor from one
/// that stops early and leaks the tail. A json string carrying an escaped quote is routine.
///
/// **`Unexpected::Other` is deliberately absent from this battery, and its absence is measured.**
/// `serde_core`'s `visit_i128`/`visit_u128` defaults do build an `Other` payload holding the real
/// value, but `serde_json` 1.0.151 only reaches them from `do_deserialize_i128`/`do_deserialize_u128`
/// (`de.rs:356`/`:388`, wired at `:1514-1515`), which run when the TARGET field is 128-bit — not
/// when the input is large. This crate declares no `i128` or `u128` field, so the route is
/// unreachable here; an over-`u64` literal against any of our types overflows to `f64` and arrives
/// as `Unexpected::Float`, which the case below pins by asserting on the float rendering rather than
/// on the digits written into the file. The `Other` shape is still covered directly, as a redactor
/// unit test in `src/export/mod.rs`, so the rule stays right for whoever adds the first 128-bit
/// field.
#[test]
fn a_parse_error_names_the_shape_it_wanted_and_never_the_value_it_got() {
    // Each marker sits in a `chat_history.json` conversation value, where an array of records
    // belongs, so serde reports the offending value against `expected a sequence`.
    let cases: [(&str, &str, &str); 5] = [
        ("string", "zqxstringmarkerzqx", r#"{"conv":"zqxstringmarkerzqx"}"#),
        ("float", "48.858844", r#"{"conv":48.858844}"#),
        ("integer", "20210304141500", r#"{"conv":20210304141500}"#),
        // Past u64. The marker is the FLOAT serde actually reports, not the digits in the file:
        // asserting on the literal would pin a value the parser never echoes and the control would
        // fail — which is how the unreachable-`Other` finding above was measured rather than argued.
        ("over u64", "1.7014118346046923", r#"{"conv":170141183460469231731687303715884105728}"#),
        // The bypass: `{:?}` escapes the embedded quote, putting three quotes in the message.
        ("string with a quote", "zqxquotedzqx", r#"{"conv":"he said \"zqxquotedzqx\" loudly"}"#),
    ];

    for (label, marker, body) in cases {
        let dir = scratch(&format!("leak-{}", label.replace(' ', "-")));
        fs::write(dir.join("chat_history.json"), body).unwrap();

        let error = ExportJson::load_dir(&dir).unwrap_err();
        let LoadError::Json { file, source } = &error else {
            panic!("{label}: expected a Json error, got {error:?}");
        };
        assert_eq!(*file, "chat_history.json", "{label}");

        // The control. Without this the assertion below could pass on a parser that never saw the
        // marker at all.
        let raw = source.to_string();
        assert!(raw.contains(marker), "{label}: the marker never reached serde, so this case proves nothing — raw was {raw}");

        let rendered = error.to_string();
        assert!(!rendered.contains(marker), "{label}: the file's own value reached a footer-bound message — {rendered}");
        // What the user asked to keep: the expectation and the position.
        assert!(rendered.contains("expected"), "{label}: the expectation is the diagnosable half — {rendered}");
        assert!(rendered.contains("line 1 column"), "{label}: the position must survive — {rendered}");
        assert!(rendered.contains("needs a parser update"), "{label}: {rendered}");
    }
}

/// The syntax arm carries a POSITION and none of the input, which is why it is not redacted.
///
/// Renamed from `…_carries_no_delimited_run`, which was **already inaccurate under the old code**
/// rather than invalidated by this round: a syntax error carried ``expected `:`` before the
/// redaction too, and what it never carried was input. The old name described one output of one
/// redaction pass; this one describes the property, which is why it survives the arm changing.
///
/// **With the arm un-redacted this is now the GUARD, not a belt-and-braces check.** Its
/// `!contains(marker)` is the only runtime check that serde's syntax messages never echo the input;
/// before the ruling the redaction stood behind it and a failure here would still have been caught
/// downstream. Nothing stands behind it now, so it should not be read as redundant with a redaction
/// that no longer applies to this arm and pruned on that basis.
///
/// It is what would notice if serde ever put input into a non-`Message` code, which is the residual
/// the `Display` impl records — and only for the shapes driven here. It cannot reach the `Io` case,
/// which no fixture can construct while the parse is `from_slice`.
#[test]
fn a_syntax_error_carries_its_position_and_none_of_the_input() {
    let dir = scratch("leak-syntax");
    fs::write(dir.join("chat_history.json"), br#"{"conv": zqxsyntaxmarkerzqx}"#).unwrap();

    let error = ExportJson::load_dir(&dir).unwrap_err();
    let rendered = error.to_string();
    assert!(rendered.contains("is not valid json"), "{rendered}");
    assert!(rendered.contains("re-extract"), "{rendered}");
    assert!(rendered.contains("line 1 column"), "the position is what makes a syntax error actionable: {rendered}");
    assert!(!rendered.contains("zqxsyntaxmarkerzqx"), "a syntax error must never echo the input: {rendered}");
}

/// **The syntax arm must keep the punctuation it names.**
///
/// Flipped from a test that pinned the COST of redacting this arm. The redaction is gone, so the
/// same four messages are now the thing to protect rather than the price of a rule: re-introducing
/// `strip_delimited` here is what reds this.
///
/// Four constants wrap punctuation in backticks (`serde_json` 1.0.151 `error.rs:358-363`), and all
/// four are ordinary malformed-json outcomes. `ExpectedDoubleQuote` is the sharpest case — its
/// payload is a quote nested inside a backtick run, so a quote-first redactor opens a run it never
/// closes and truncates the message rather than merely blanking a character.
#[test]
fn the_syntax_arm_keeps_the_punctuation_it_names() {
    let dir = scratch("syntax-punctuation");

    fs::write(dir.join("chat_history.json"), br#"{"conv" 1}"#).unwrap();
    let missing_colon = ExportJson::load_dir(&dir).unwrap_err().to_string();
    assert!(missing_colon.contains("expected `:`"), "the wanted character is the diagnostic: {missing_colon}");
    assert!(!missing_colon.contains('\u{2026}'), "redaction must not reach this arm: {missing_colon}");
    assert!(missing_colon.contains("line 1 column"), "{missing_colon}");

    // Between object members, not inside an array: an array element that is not a record fails as a
    // Data error first and never reaches the comma.
    fs::write(dir.join("chat_history.json"), br#"{"a":[] "b":[]}"#).unwrap();
    let missing_comma = ExportJson::load_dir(&dir).unwrap_err().to_string();
    assert!(missing_comma.contains("expected `,` or `}`"), "{missing_comma}");
    assert!(!missing_comma.contains('\u{2026}'), "{missing_comma}");
}

#[test]
fn a_value_the_model_cannot_validate_fails_the_load_and_names_the_field() {
    let dir = scratch("bad-timestamp");
    fs::write(dir.join("memories_history.json"), br#"{"Saved Media":[{"Date":"yesterday"}]}"#).unwrap();

    let error = ExportJson::load_dir(&dir).unwrap_err();
    let LoadError::Invalid { file, source } = &error else {
        panic!("expected an Invalid error, got {error:?}");
    };
    assert_eq!(*file, "memories_history.json");
    assert_eq!(source.field(), Field::Date);
    assert_eq!(source.value(), "yesterday");
    assert_eq!(error.to_string(), "memories_history.json: Date: expected a \"YYYY-MM-DD HH:MM:SS UTC\" timestamp, got 9 chars");
    assert!(!error.to_string().contains("yesterday"), "the offending value must not reach a footer-bound message");
}

// ---- the fixture tree ----

#[test]
fn every_json_file_in_the_fixture_tree_parses_as_json() {
    let Some(root) = fixtures_root() else {
        println!("skipping: fixtures/ is absent (gitignored, so CI never has it)");
        return;
    };
    let mut seen = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                let bytes = fs::read(&path).unwrap();
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .unwrap_or_else(|err| panic!("{} is not valid json: {err}", path.display()));
                seen.push(path.strip_prefix(&root).unwrap().to_owned());
            }
        }
    }
    seen.sort();
    // This fixture tree holds 19 schema files (the 2026-07-26 export's set, on disk right now)
    // + the two media listings + the redaction report — a count of THIS fixture, not of
    // `SCHEMA_FILES`, which is a 20-name union no single export's tree ever holds all of.
    assert_eq!(seen.len(), 22, "found {seen:?}");
    assert!(seen.contains(&PathBuf::from("_redaction_report.json")));
    assert!(seen.contains(&PathBuf::from("listings/chat_media.json")));
    assert!(seen.contains(&PathBuf::from("listings/memories.json")));
}

#[test]
fn the_fixture_json_dir_holds_only_known_schema_files() {
    // Subset, not equality: which names appear is the requester's category choice
    // (`docs/design.md`, n=2), so a fixture built from one export legitimately omits names
    // another export contributed to the union — the fixture on disk right now is the OLDER
    // export, so it holds no `in_app_reports.json` and that is expected, not a gap to fill.
    let dir = json_dir_or_skip!();
    let mut names: Vec<String> =
        fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort(); // keeps a failure's `{name}` message deterministic across runs
    assert!(!names.is_empty(), "the fixture json dir must not be empty");
    for name in &names {
        assert!(SCHEMA_FILES.contains(&name.as_str()), "{name} is not a known schema file");
    }
}

#[test]
fn loading_the_fixture_dir_fills_every_modelled_field() {
    let dir = json_dir_or_skip!();
    let loaded = load_fixture(&dir);
    assert!(loaded.account.is_some());
    assert!(loaded.account_history.is_some());
    assert!(loaded.bitmoji.is_some());
    assert!(loaded.chat_history.is_some());
    assert!(loaded.custom_sticker.is_some());
    assert!(loaded.email_campaign_history.is_some());
    assert!(loaded.feature_emails.is_some());
    assert!(loaded.friends.is_some());
    assert!(loaded.location_history.is_some());
    assert!(loaded.memories.is_some());
    assert!(loaded.ranking.is_some());
    assert!(loaded.snap_ads.is_some());
    assert!(loaded.snap_history.is_some());
    assert!(loaded.snap_pro.is_some());
    assert!(loaded.snapchat_ai.is_some());
    assert!(loaded.snapchat_plus.is_some());
    assert!(loaded.story_history.is_some());
    assert!(loaded.terms_history.is_some());
    assert!(loaded.user_profile.is_some());
}

// ---- modelled files ----

#[test]
fn account_carries_the_owner_the_devices_and_the_logins() {
    let dir = json_dir_or_skip!();
    let account = load_fixture(&dir).account.unwrap();

    assert_eq!(account.basics.username, Username::new("user_1"));
    assert_eq!(account.basics.name.as_deref(), Some("redacted-kx0kes"));
    assert_eq!(account.basics.created.unwrap().to_string(), "2016-09-29 09:22:31 UTC");
    // Free text in the observed export, deliberately not a Timestamp.
    assert_eq!(account.basics.last_active.as_deref(), Some("redacted-sqvmoz"));

    assert_eq!(account.device.make.as_deref(), Some("redacted-4165gu"));
    assert_eq!(account.device.os_type.as_deref(), Some("redacted-shox0j"));

    assert_eq!(account.device_history.len(), 10);
    assert_eq!(account.device_history[0].started.unwrap().to_string(), "2019-02-13 13:01:02 UTC");
    assert_eq!(account.device_history[0].model.as_deref(), Some("redacted-94vvpq"));

    assert_eq!(account.logins.len(), 14);
    assert_eq!(account.logins[0].created.unwrap().to_string(), "2019-12-13 12:54:45 UTC");
    assert_eq!(account.logins[0].country.as_deref(), Some("redacted-0gb9zg"));

    assert_eq!(account.associated_accounts.len(), 9);
    assert_eq!(account.associated_accounts[0].user_id.as_deref(), Some("8494a2c0-f33f-6b12-5914-5c8cbbd3f674"));
    assert_eq!(account.associated_accounts[0].requested.unwrap().to_string(), "2019-09-22 23:34:12 UTC");
    assert_eq!(account.associated_accounts[0].last_seen.unwrap().to_string(), "2019-09-22 23:34:12 UTC");
}

#[test]
fn user_profile_carries_the_engagement_counts_and_the_ad_id() {
    let dir = json_dir_or_skip!();
    let profile = load_fixture(&dir).user_profile.unwrap();

    assert_eq!(profile.created.unwrap().to_string(), "2016-09-29 09:22:31 UTC");
    assert_eq!(profile.country.as_deref(), Some("redacted-di0m8i"));
    // Empty on the wire, so absent here rather than `Some("")`.
    assert_eq!(profile.account_creation_country, None);
    assert_eq!(profile.platform_version, None);
    assert_eq!(profile.in_app_language, None);
    assert_eq!(profile.cohort_age.as_deref(), Some("redacted-01a1r2"));

    let occurrences: Vec<u64> = profile.engagement.iter().map(|event| event.occurrences).collect();
    assert_eq!(occurrences, [61, 5, 7, 76, 98, 7]);
    assert_eq!(profile.engagement[0].event, "redacted-ulwza2");

    assert_eq!(profile.time_spent_breakdown.len(), 3);
    assert_eq!(profile.web_interactions.len(), 25);
    assert_eq!(profile.web_interactions[0], "redacted-u4db8t");
    // `App Interactions` has no model field: it is `[]` in the one observed export, so its
    // element shape is unknown and it stays an untyped schema passthrough.
    assert_eq!(profile.mobile_ad_id.as_deref(), Some("00000000-0000-0000-0000-000000000000"));
}

#[test]
fn friends_splits_into_its_eight_relationship_lists() {
    let dir = json_dir_or_skip!();
    let friends = load_fixture(&dir).friends.unwrap();

    // Truncated at 25 by the redactor; `_redaction_report.json` says the real list holds 100.
    assert_eq!(friends.friends.len(), 25);
    assert_eq!(friends.requests_sent.len(), 25);
    assert_eq!(friends.blocked.len(), 2);
    assert_eq!(friends.deleted.len(), 11);
    assert_eq!(friends.hidden_suggestions.len(), 6);
    assert_eq!(friends.ignored.len(), 8);
    assert_eq!(friends.pending_requests.len(), 2);
    assert_eq!(friends.shortcuts.len(), 1);

    let first = &friends.friends[0];
    assert_eq!(first.username, Username::new("user_80"));
    assert_eq!(first.display_name, None);
    assert_eq!(first.created.unwrap().to_string(), "2018-01-13 12:23:18 UTC");
    assert_eq!(first.last_modified.unwrap().to_string(), "2018-01-13 12:23:18 UTC");
    assert_eq!(first.source.as_deref(), Some("redacted-0h79jm"));

    // A friend suggestion has no timestamps at all, which is the empty-string-is-absence path.
    let hidden = &friends.hidden_suggestions[0];
    assert_eq!(hidden.username, Username::new("user_191"));
    assert_eq!(hidden.display_name.as_deref(), Some("user_192"));
    assert_eq!(hidden.created, None);
    assert_eq!(hidden.last_modified, None);

    assert_eq!(friends.shortcuts[0].name.as_deref(), Some("redacted-2drfif"));
    assert_eq!(friends.shortcuts[0].created.unwrap().to_string(), "2016-10-18 04:15:36 UTC");
}

#[test]
fn memories_carry_a_date_a_coordinate_pair_and_two_urls() {
    let dir = json_dir_or_skip!();
    let memories = load_fixture(&dir).memories.unwrap();

    assert_eq!(memories.saved_media.len(), 25);
    let first = &memories.saved_media[0];
    assert_eq!(first.date.unwrap().to_string(), "2020-07-28 12:41:51 UTC");
    // Memories' media-type words fall outside the allowlist the redactor preserves verbatim, so
    // the real word is unknown here and lands in `Other`. That is all the fixture can show: the
    // redactor keys its synthetic tokens on the json path, so two identical real words at
    // different paths come out different, and 25 distinct tokens are not 25 distinct words.
    assert_eq!(first.media_type, MediaKind::Other("redacted-q990pb".to_owned()));
    let point = first.location.unwrap();
    assert_eq!(point.latitude(), 1.1);
    assert_eq!(point.longitude(), 1.0);
    // The redactor strips the signed urls, so the whole fixture exercises the absent path.
    assert!(first.download_link.is_none());
    assert!(first.media_download_url.is_none());

    let second = memories.saved_media[1].location.unwrap();
    assert_eq!(second.latitude(), 1.1);
    assert_eq!(second.longitude(), 1.5);

    assert!(memories.saved_media.iter().all(|memory| memory.location.is_some()));
    assert!(memories.saved_media.iter().all(|memory| memory.date.is_some()));
}

#[test]
fn chat_history_groups_messages_by_conversation() {
    let dir = json_dir_or_skip!();
    let chat = load_fixture(&dir).chat_history.unwrap();

    assert_eq!(chat.conversations.len(), 72);
    // BTreeMap order: uuid-keyed group threads sort ahead of the `user_N` one-to-ones.
    assert_eq!(chat.conversations[0].id.as_str(), "04a27bf5-7f17-10a8-13df-3b88b8b3119a");
    assert_eq!(chat.conversations.last().unwrap().id.as_str(), "user_8");

    let thread = chat.conversations.iter().find(|conversation| conversation.id.as_str() == "user_11").expect("the user_11 thread");
    assert_eq!(thread.records.len(), 4);

    let opener = &thread.records[0];
    assert_eq!(opener.from, Username::new("user_11"));
    assert_eq!(opener.media_type, MediaKind::Status);
    assert_eq!(opener.created.unwrap().to_string(), "2020-07-26 15:48:05 UTC");
    // The same instant `created` spells above, to the second: the export states both and the
    // redactor's constant shift preserves their difference, so this agreement is the real export's.
    assert_eq!(opener.created_epoch_ms, Some(1_595_778_485_675));
    assert_eq!(opener.content, None);
    assert_eq!(opener.conversation_title, None);
    assert!(opener.is_sender);
    assert!(!opener.is_saved);
    assert_eq!(opener.media_ids, None);

    let text = &thread.records[2];
    assert_eq!(text.media_type, MediaKind::Text);
    assert_eq!(text.content.as_ref().unwrap().expose(), "redacted-g3bpgg");
    assert!(text.is_saved);
    assert!(!text.is_sender);

    // A media message: empty content becomes absence, and the media id survives verbatim.
    let media = &thread.records[3];
    assert_eq!(media.from, Username::new("user_9"));
    assert_eq!(media.media_type, MediaKind::Media);
    assert_eq!(media.content, None);
    assert_eq!(media.media_ids.as_deref(), Some("redacted-nyh3ho"));
    assert_eq!(media.created.unwrap().to_string(), "2018-09-21 09:13:02 UTC");
    assert_eq!(media.created_epoch_ms, Some(1_537_521_182_599));

    // A group thread is the only place a conversation title appears.
    let group = chat
        .conversations
        .iter()
        .find(|conversation| conversation.id.as_str() == "2d2abd90-e42d-07f6-0637-00309ecf8b2b")
        .expect("the group thread");
    assert_eq!(group.records[0].conversation_title.as_deref(), Some("conv_1"));
    assert_eq!(group.records[0].from, Username::new("user_12"));

    let tally =
        tally(chat.conversations.iter().flat_map(|conversation| conversation.records.iter().map(|message| message.media_type.clone())));
    assert_eq!(tally.get(&MediaKind::Text), Some(&618));
    assert_eq!(tally.get(&MediaKind::Media), Some(&279));
    assert_eq!(tally.get(&MediaKind::Note), Some(&15));
    assert_eq!(tally.get(&MediaKind::Sticker), Some(&9));
    assert_eq!(tally.get(&MediaKind::Status), Some(&5));
    assert_eq!(tally.get(&MediaKind::Image), None);
    // Five media-type words this parser has never seen, one message each.
    let unknown: usize = tally.iter().filter(|(kind, _)| matches!(kind, MediaKind::Other(_))).map(|(_, count)| *count).sum();
    assert_eq!(unknown, 5);
}

#[test]
fn snap_history_shares_its_conversation_ids_with_chat_history() {
    let dir = json_dir_or_skip!();
    let loaded = load_fixture(&dir);
    let snaps = loaded.snap_history.unwrap();
    let chat = loaded.chat_history.unwrap();

    assert_eq!(snaps.conversations.len(), 62);

    let thread = snaps.conversations.iter().find(|conversation| conversation.id.as_str() == "user_32").expect("the user_32 thread");
    assert_eq!(thread.records.len(), 4);
    let first = &thread.records[0];
    assert_eq!(first.from, Username::new("user_32"));
    assert_eq!(first.media_type, MediaKind::Image);
    assert_eq!(first.created.unwrap().to_string(), "2020-08-02 15:02:34 UTC");
    assert_eq!(first.created_epoch_ms, Some(1_596_380_554_698));
    assert_eq!(first.conversation_title, None);
    assert!(first.is_sender);

    let tally = tally(snaps.conversations.iter().flat_map(|conversation| conversation.records.iter().map(|snap| snap.media_type.clone())));
    assert_eq!(tally.get(&MediaKind::Image), Some(&561));
    assert_eq!(tally.get(&MediaKind::Video), Some(&253));
    assert_eq!(tally.len(), 2, "snaps are only ever images or videos here");

    // The join phase 3 depends on: a snap thread id is a chat thread id.
    let chat_ids: Vec<&str> = chat.conversations.iter().map(|conversation| conversation.id.as_str()).collect();
    let shared = snaps.conversations.iter().filter(|conversation| chat_ids.contains(&conversation.id.as_str())).count();
    assert_eq!(shared, 57);
    assert_eq!(snaps.conversations.len() - shared, 5, "five threads snapped but never chatted");
}

fn tally<T: Ord>(values: impl Iterator<Item = T>) -> BTreeMap<T, usize> {
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    counts
}

// ---- typed passthroughs ----

#[test]
fn the_thirteen_passthrough_files_land_with_their_fields_typed() {
    let dir = json_dir_or_skip!();
    let loaded = load_fixture(&dir);

    let history = loaded.account_history.unwrap();
    assert_eq!(history.display_name_change.len(), 10);
    assert_eq!(history.display_name_change[0].date, "2020-02-11 05:54:28 UTC");
    assert_eq!(history.display_name_change[0].display_name, "user_2");
    assert_eq!(history.email_change.len(), 2);
    assert_eq!(history.mobile_number_change.len(), 2);
    assert_eq!(history.password_change.len(), 4);
    assert_eq!(history.download_my_data_reports.len(), 3);
    assert!(history.two_factor_authentication.is_empty());

    let bitmoji = loaded.bitmoji.unwrap();
    assert_eq!(bitmoji.analytics.app_open_count, 6);
    assert_eq!(bitmoji.analytics.outfit_save_count, 6);
    assert_eq!(bitmoji.analytics.share_count, 4);
    assert_eq!(bitmoji.analytics.avatar_gender, "redacted-zkmsdx");
    assert_eq!(bitmoji.basic_information.account_creation_date, "2016-09-29 09:25:58 UTC");
    assert_eq!(bitmoji.basic_information.email, "");

    let stickers = loaded.custom_sticker.unwrap();
    assert_eq!(stickers.my_custom_stickers.len(), 25);
    assert_eq!(stickers.my_custom_stickers[0].sticker_id, "redacted-byxyg5");
    assert_eq!(stickers.my_custom_stickers[0].content, "x+x-xxxxxxxxxxxx.png");

    let campaigns = loaded.email_campaign_history.unwrap();
    assert_eq!(campaigns.subscriptions.len(), 7);
    assert_eq!(campaigns.subscriptions[0].email_campaign, "redacted-f2e24l");
    assert!(campaigns.history.is_empty());

    assert!(loaded.feature_emails.unwrap().email_used_to_join.is_empty());

    let location = loaded.location_history.unwrap();
    assert_eq!(location.frequent_locations.len(), 2);
    assert_eq!(location.frequent_locations[0].region, "013");
    assert_eq!(location.latest_location.len(), 1);
    assert_eq!(location.home_school_work.len(), 5);
    assert!(location.home_school_work.values().all(String::is_empty));
    // Keyed by an opaque business id, so it is a map and not a fixed-shape container.
    assert_eq!(location.businesses_visited.len(), 2);
    let visits = &location.businesses_visited["key_6"];
    assert_eq!(visits.len(), 2);
    assert_eq!(visits[0], serde_json::json!(["redacted-qyw2q4", "redacted-llu9g7"]));
    assert!(location.businesses_visited["key_7"].is_empty());
    assert_eq!(location.areas_visited.len(), 12);
    assert_eq!(location.areas_visited[1].postal_code, "4906");

    let ranking = loaded.ranking.unwrap();
    assert_eq!(ranking.statistics.snapscore, "redacted-k6l5ci");
    // Snapchat ships these counts as strings.
    assert_eq!(ranking.statistics.total_friends, "627");
    assert_eq!(ranking.statistics.accounts_followed, "4");
    assert_eq!(ranking.spotlight, [serde_json::json!(0), serde_json::json!({})]);

    let ads = loaded.snap_ads.unwrap();
    assert_eq!(ads.organization_members.len(), 1);
    assert_eq!(ads.organization_members[0].display_name, "user_2");
    assert_eq!(ads.organization_members[0].organization_name, "redacted-kyojid");

    let pro = loaded.snap_pro.unwrap();
    assert_eq!(pro.profile.created, "2019-01-31 05:09:17 UTC");
    assert_eq!(pro.profile.profile_photo, "https://cf-st.sc-cdn.net/REDACTED/REDACTED/REDACTED");
    assert_eq!(pro.profile.hero_image, "");

    let ai = loaded.snapchat_ai.unwrap();
    assert_eq!(ai.my_ai_content.len(), 4);
    assert_eq!(ai.my_ai_content[0].timestamp, "2020-05-09 19:30:47 UTC");
    assert_eq!(ai.my_ai_content[0].ip_address, "");
    assert!(ai.my_ai_memory.is_empty());

    let plus = loaded.snapchat_plus.unwrap();
    assert_eq!(plus.subscriptions.len(), 1);
    // The only float leaf in the whole export.
    assert_eq!(plus.subscriptions[0].price, 4.3);
    assert_eq!(plus.subscriptions[0].purchase_date, "2016-11-13 11:05:46 UTC");
    assert_eq!(plus.subscriptions[0].end_date, "2016-11-20 11:05:21 UTC");

    let stories = loaded.story_history.unwrap();
    assert_eq!(stories.your_story_views.len(), 25);
    assert_eq!(stories.your_story_views[0].story_date, "2020-08-02 13:01:02 UTC");
    assert_eq!(stories.your_story_views[0].story_views, 1);
    assert_eq!(stories.your_story_views[0].story_replies, 3);
    assert!(stories.friend_and_public_story_views.is_empty());

    let terms = loaded.terms_history.unwrap();
    assert_eq!(terms.terms_acceptance_history.len(), 7);
    assert_eq!(terms.terms_acceptance_history[0].version, "redacted-6po7wf");
    assert_eq!(terms.terms_acceptance_history[0].acceptance_date, "2016-09-29 09:22:31 UTC");
    assert!(terms.spectacles_user_agreement.is_empty());
}
