//! Public-API tests for the chat media screen: the run form and its overlay-mode cycle, the counts
//! line, the live per-item progress table, the completion footer alert, the run composition against
//! a real tempdir export, and the privacy boundary this screen is the first to have.
//!
//! Nothing here reads a real export: every export tree is synthesized in a tempdir (a
//! `mydata~<id>/json/` with a hand-written `chat_history.json`, a `chat_media/` dir holding tiny
//! JPEGs and PNGs), and every manifest is opened with `open_in` so the per-user data dir is never
//! touched.
//!
//! Render expectations are cross-checked against the cloudy-tui skill's Panel, Form rows, Cycle row,
//! Toggle row, Action chip, List / table, Status pill, Progress bar, Footer alert, Empty state and
//! Pane focus sections, not against this crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use exportsnap::app::{App, Tab};
use exportsnap::export::LoadError;
use exportsnap::export::chat_fix::OverlayMode;
use exportsnap::export::chat_run::{self, HistoryOutcome, PlanCounts, PlanRow, PlanSnapshot, RunError, RunEvent, RunOutcome};
use exportsnap::export::env::Environment;
use exportsnap::export::local_fix::{FixReport, Leg, VideoOptions};
use exportsnap::export::manifest::{ExportId, ItemKind, ItemStatus, Manifest, NewItem, ResumeReport};
use exportsnap::tui::alert::AlertKind;
use exportsnap::tui::shell;
use exportsnap::tui::theme::{Palette, Tier};
use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use tempfile::TempDir;

const EXPORT_ID: &str = "1784667002819";
const DAY: &str = "2021-03-04";
const WIDTH: u32 = 16;
const HEIGHT: u32 = 12;

/// The conversation key every privacy assertion hunts for.
///
/// Spelled so it survives `dir_name` unchanged — ascii alphanumerics and `-` — because a key that
/// the cleaner rewrote would make the search prove nothing about the key that reached the screen.
/// It is also distinctive enough that no chrome, glyph or count can contain it by accident.
const SECRET_KEY: &str = "zqxfriendhandlezqx";

// ---- fixtures ----

fn media_id(seed: u32) -> String {
    format!("b~aB3xY9{seed:04}")
}

fn chat_media_dir(part: &Path) -> PathBuf {
    let dir = part.join("chat_media");
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A plain `b~<id>` JPEG, the shape 8005 of the observed export's files carry.
fn write_media(part: &Path, seed: u32) -> String {
    let stem = media_id(seed);
    let mut pixels = RgbImage::new(WIDTH, HEIGHT);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = Rgb([(x * 9) as u8, (y * 11) as u8, 40]);
    }
    pixels.save_with_format(chat_media_dir(part).join(format!("{DAY}_{stem}.jpg")), ImageFormat::Jpeg).unwrap();
    stem
}

/// A lone plain `overlay~<id>.png`: a caption layer nothing pairs, which the counts line reports.
fn write_unmatched_overlay(part: &Path, seed: u32) -> String {
    let stem = format!("overlay~aB3xY9{seed:04}");
    let mut pixels = RgbaImage::new(WIDTH, HEIGHT);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < WIDTH / 2 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 0, 0]) };
    }
    pixels.save_with_format(chat_media_dir(part).join(format!("{DAY}_{stem}.png")), ImageFormat::Png).unwrap();
    stem
}

/// A `thumbnail~<id>.jpg`, which decision 44d drops and the counts line reports.
fn write_thumbnail(part: &Path, seed: u32) -> String {
    let stem = format!("thumbnail~aB3xY9{seed:04}");
    RgbImage::new(WIDTH, HEIGHT).save_with_format(chat_media_dir(part).join(format!("{DAY}_{stem}.jpg")), ImageFormat::Jpeg).unwrap();
    stem
}

/// `chat_history.json` in the shape the parser expects: an object keyed by conversation, each value
/// an array of message records. Built by hand — the schema types are deserialize-only — in the exact
/// spelling the observed export uses.
fn write_history(json_dir: &Path, conversations: &[(&str, &[(&str, &str)])]) {
    fs::create_dir_all(json_dir).unwrap();
    let threads: Vec<String> = conversations
        .iter()
        .map(|(key, rows)| {
            let entries: Vec<String> = rows
                .iter()
                .map(|(created, media_ids)| {
                    format!(
                        r#"{{"From":"sender-handle","Media Type":"MEDIA","Created":"{created}","IsSender":false,"IsSaved":false,"Created(microseconds)":0,"Media IDs":"{media_ids}"}}"#
                    )
                })
                .collect();
            format!(r#""{key}":[{}]"#, entries.join(","))
        })
        .collect();
    fs::write(json_dir.join("chat_history.json"), format!("{{{}}}", threads.join(","))).unwrap();
}

/// One delivery: part 1 unpacked with its `json/` and a `chat_media/` dir.
fn export_tree(conversations: &[(&str, &[(&str, &str)])], seeds: &[u32]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    write_history(&part.join("json"), conversations);
    for seed in seeds {
        write_media(&part, *seed);
    }
    dir
}

fn inputs(dir: &TempDir, overlay: OverlayMode) -> (chat_run::RunInputs, TempDir) {
    let state = TempDir::new().unwrap();
    let inputs = chat_run::RunInputs {
        source: dir.path().to_path_buf(),
        out_root: dir.path().join("out"),
        manifest_dir: Some(state.path().to_path_buf()),
        video: VideoOptions { transcode: false, ffmpeg: None },
        overlay,
    };
    (inputs, state)
}

fn collect(inputs: &chat_run::RunInputs) -> Vec<RunEvent> {
    let (sender, receiver) = mpsc::channel();
    chat_run::run(inputs, &sender);
    drop(sender);
    receiver.try_iter().collect()
}

fn finished(events: &[RunEvent]) -> &RunOutcome {
    match events.last().unwrap() {
        RunEvent::Finished(outcome) => outcome,
        RunEvent::Planned(_) => panic!("no Finished event"),
    }
}

fn planned(events: &[RunEvent]) -> &PlanSnapshot {
    match events.first().unwrap() {
        RunEvent::Planned(snapshot) => snapshot,
        RunEvent::Finished(_) => panic!("no Planned event"),
    }
}

fn report(outcome: &RunOutcome) -> &FixReport {
    match outcome {
        RunOutcome::Completed(report) => report,
        RunOutcome::Failed(error) => panic!("run failed: {error}"),
    }
}

// ---- the run composition ----

#[test]
fn a_run_over_a_synthetic_export_plans_then_finishes_every_item() {
    let dir = export_tree(&[(SECRET_KEY, &[("2021-03-04 14:30:05 UTC", "b~aB3xY90001")])], &[1, 2]);
    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    let events = collect(&inputs);
    let snapshot = planned(&events);
    let report = report(finished(&events));

    assert_eq!(snapshot.export_id, ExportId::new(EXPORT_ID).unwrap());
    assert_eq!(snapshot.rows.len(), 2);
    assert_eq!(report.fixed, 2, "{:?}", report.failed);
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    // The named file lands under its conversation, the unnamed one in the reserved bucket — and the
    // ROW carries only the name either way.
    let named = snapshot.rows.iter().find(|row| row.source_id == media_id(1)).unwrap();
    assert_eq!(named.output_name, "20210304_143005.jpg");
    assert_eq!(named.leg, Leg::Image);
    let unnamed = snapshot.rows.iter().find(|row| row.source_id == media_id(2)).unwrap();
    assert_eq!(unnamed.output_name, "20210304_000000.jpg");

    assert!(dir.path().join(format!("out/chat/{SECRET_KEY}/20210304_143005.jpg")).is_file());
    assert!(dir.path().join("out/chat/_no-conversation/2021/03/20210304_000000.jpg").is_file());

    let manifest = Manifest::open_in(&snapshot.manifest_dir, &snapshot.export_id).unwrap();
    assert_eq!(manifest.items(ItemKind::ChatMedia).unwrap().len(), 2);
}

/// A whole conversation leaving the export does not move a survivor's output directory.
///
/// Two keys clean to `a_b`, so the first gets it and the second gets `a_b_2`. When the first leaves,
/// the key set that ordinal was a position in is one shorter, and re-deriving from it alone puts the
/// survivor's NEW media in `a_b` — a second directory for one thread, beside the one already holding
/// its finished output. What holds it still is the manifest, read back in `prepare`.
///
/// **Driven through `chat_run::run` and not through `chat_fix::plan`, and that is the point of it
/// being here.** The read is a line of the run composition; a test that re-drives the pieces in the
/// same order proves the plan can take a seed, never that the composition still hands it one.
#[test]
fn a_conversation_that_outlives_its_neighbour_keeps_its_own_directory() {
    let created = "2021-03-04 14:30:05 UTC";
    let later = "2021-03-04 15:45:07 UTC";
    let dir = export_tree(&[("a/b", &[(created, "b~aB3xY90001")]), ("a?b", &[(created, "b~aB3xY90002")])], &[1, 2]);
    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    let first = collect(&inputs);
    assert_eq!(report(finished(&first)).fixed, 2);
    assert!(dir.path().join("out/chat/a_b_2/20210304_143005.jpg").is_file(), "sorted key order puts `a?b` second");

    // `a/b` leaves the export outright — its file off disk and its thread out of the history — while
    // `a?b` gains one more file, which is the only item the second run owes any work for.
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    fs::remove_file(chat_media_dir(&part).join(format!("{DAY}_b~aB3xY90001.jpg"))).unwrap();
    write_media(&part, 3);
    write_history(&part.join("json"), &[("a?b", &[(created, "b~aB3xY90002"), (later, "b~aB3xY90003")])]);

    let second = collect(&inputs);
    assert_eq!(report(finished(&second)).fixed, 1, "{:?}", report(finished(&second)).failed);
    assert!(dir.path().join("out/chat/a_b_2/20210304_154507.jpg").is_file(), "the thread's new media left the tree holding its old");
    assert!(!dir.path().join("out/chat/a_b/20210304_154507.jpg").exists(), "and it started a second directory for one thread");
}

/// Decision 52's ITEM seed, pinned where it is wired and separated from the mere fact of the read.
///
/// `chat_run::prepare` reads one seed that carries both layers. The DIRECTORY half is pinned by the
/// test above; the item half was mutable to green here, reddening only `tests/chat_fix.rs`, which
/// re-drives the composition's order rather than driving `prepare`.
///
/// **Three items in ONE named conversation, not in the `_no-conversation` bucket.** Decision 46b's
/// bucket is where a collision on one second is ordinary and a conversation folder is where it is
/// merely possible, so the bucket is the more faithful shape — but building it needs a history that
/// joins nothing, and every helper here is one the file already uses for a named thread. A joined
/// item also takes the message's own timestamp instead of a midnight fallback, so the collision is
/// stated by the fixture rather than inherited from a date chain. The directory layer is held still
/// by the single key, so a red here can only be the item layer.
///
/// **The newcomer is what makes this an ORDERING pin and not just a read pin.** Seed 1 sorts ahead
/// of both recorded items (`Discovery::from_files` orders by id and `media_id` zero-pads), so:
///
/// - correct — every record is claimed before any derive, so seed 1 walks past both to `_3` and
///   seed 3 is handed back the `_2` it already finished at;
/// - seed defaulted — seed 1 takes the plain name, over the file seed 2 finished;
/// - seed read AFTER the resume sweep — seed 3's deleted output has already demoted and cleared its
///   record, so seed 1 takes `_2` and seed 3 is pushed to `_3`, off its own file.
///
/// The third is the one nothing else in this repo reaches, and it is why the assertion is on seed
/// 3's row rather than on the newcomer's. **It is measured rather than reasoned**: planting that
/// inversion and reading all three rows gives `_2` / plain / `_3` for seeds 1, 2 and 3 verbatim —
/// seed 2 keeps the plain name throughout, because its own record survives the sweep and is adopted
/// either way, which is what leaves the newcomer and the demoted item to fight over `_2`.
#[test]
fn a_newcomer_sorting_ahead_of_a_recorded_item_does_not_take_its_output_name() {
    let sent = "2021-03-04 14:30:05 UTC";
    let dir = export_tree(&[("friend-handle", &[(sent, "b~aB3xY90002"), (sent, "b~aB3xY90003")])], &[2, 3]);
    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    let first = collect(&inputs);
    assert_eq!(report(finished(&first)).fixed, 2, "{:?}", report(finished(&first)).failed);

    let named = |snapshot: &PlanSnapshot, seed: u32| {
        snapshot.rows.iter().find(|row| row.source_id == media_id(seed)).map(|row| row.output_name.clone())
    };
    let snapshot = planned(&first);
    assert_eq!(named(snapshot, 2).as_deref(), Some("20210304_143005.jpg"), "the fixture is not two items on one second");
    assert_eq!(named(snapshot, 3).as_deref(), Some("20210304_143005_2.jpg"), "the fixture is not two items on one second");

    // Only seed 3's OUTPUT goes: its row keeps the record until the sweep inside the next run
    // clears it, which is the window this test is about. Then a newcomer arrives that sorts ahead of
    // both, through the same history rewrite the test above already does.
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    fs::remove_file(dir.path().join("out/chat/friend-handle/20210304_143005_2.jpg")).unwrap();
    write_media(&part, 1);
    write_history(&part.join("json"), &[("friend-handle", &[(sent, "b~aB3xY90002"), (sent, "b~aB3xY90003"), (sent, "b~aB3xY90001")])]);

    let second = collect(&inputs);
    assert_eq!(report(finished(&second)).fixed, 2, "{:?}", report(finished(&second)).failed);
    let after = planned(&second);
    assert_eq!(
        named(after, 3).as_deref(),
        Some("20210304_143005_2.jpg"),
        "the recorded item was pushed off the file it had already finished at"
    );
    assert_eq!(named(after, 1).as_deref(), Some("20210304_143005_3.jpg"), "the newcomer took a name a record already claimed");
}

/// Decision 52's seed read against its own ENROLLMENT — the other side of the window, and the side
/// only this leg can reach.
///
/// The two tests above pin the seed against the resume sweep, which is the LATE edge of the window.
/// This one pins the EARLY edge: `chat_run::prepare` reads the seed after `Reconciliation::enroll`,
/// and enroll's `reset` is what clears the record of a row whose file came back. Read ahead of it and
/// the plan adopts a path the run is about to stop believing.
///
/// **A file that leaves and comes back is the only thing that separates the two orderings**, because
/// `reset` is the only writer that clears a record the enrollment can reach, and a row that never
/// parked has no record for it to clear. This leg reaches that state through the composition alone
/// and the memories leg does not — a `b~` token and its file share one `source_id`, so the file
/// vanishing parks the row it already finished (keeping the record, per queue task 39's decision 50)
/// and the file
/// returning resets it, both under the same row. On the memories leg the same removal changes the
/// entry's identity to a synthetic one and leaves the uuid row `Done`, which `retire_absent` exempts,
/// so no memories row carrying a record ever reaches enroll's reset arm. See `Plan::build`'s table.
///
/// The newcomer does here what it does above: seed 1 sorts ahead of both recorded items, so the
/// adopted answer and the derived one differ and the assertion can tell them apart.
///
/// - correct — the enrollment cleared seed 3's record before the read, so seed 1 takes the `_2` that
///   record no longer claims and seed 3 derives past both to `_3`;
/// - seed read AHEAD of the enrollment — seed 3's stale record still claims `_2`, so seed 3 is handed
///   it back and seed 1 is pushed to `_3`.
///
/// Note that this is the mirror image of the sweep pin above, where `_2`/`_3` is the CORRECT answer
/// for seeds 3 and 1: there the record must survive into the read, here it must not.
#[test]
fn a_returning_chat_file_has_its_record_cleared_before_the_seed_is_read() {
    let sent = "2021-03-04 14:30:05 UTC";
    let dir = export_tree(&[("friend-handle", &[(sent, "b~aB3xY90002"), (sent, "b~aB3xY90003")])], &[2, 3]);
    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    let first = collect(&inputs);
    assert_eq!(report(finished(&first)).fixed, 2, "{:?}", report(finished(&first)).failed);

    let named = |snapshot: &PlanSnapshot, seed: u32| {
        snapshot.rows.iter().find(|row| row.source_id == media_id(seed)).map(|row| row.output_name.clone())
    };
    let snapshot = planned(&first);
    assert_eq!(named(snapshot, 2).as_deref(), Some("20210304_143005.jpg"), "the fixture is not two items on one second");
    assert_eq!(named(snapshot, 3).as_deref(), Some("20210304_143005_2.jpg"), "the fixture is not two items on one second");

    // Seed 3's SOURCE leaves while the message naming it stays, so its token becomes a gap and its
    // own row parks at `SourceMissing` KEEPING the record — the state enroll's `reset` clears, and
    // the whole reason this fixture needs a run here rather than two runs and a hand-written row.
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    let source = chat_media_dir(&part).join(format!("{DAY}_{}.jpg", media_id(3)));
    assert!(source.is_file(), "the fixture must remove a file that is there");
    fs::remove_file(&source).unwrap();

    let second = collect(&inputs);
    assert_eq!(report(finished(&second)).fixed, 0, "{:?}", report(finished(&second)).failed);
    let parked = Manifest::open_in(&snapshot.manifest_dir, &snapshot.export_id).unwrap();
    let row = parked.item(ItemKind::ChatMedia, &media_id(3)).unwrap().expect("the vanished file's row is still enrolled");
    assert_eq!(row.status, ItemStatus::SourceMissing, "the fixture never parked the row whose reset this test is about");
    assert!(row.output_path.is_some(), "the park dropped the record, so the reset below would have nothing to clear");
    drop(parked);

    // The file comes back — which is what makes the enrollment reset it — and a newcomer sorting
    // ahead of both arrives with it, through the same history rewrite the tests above do.
    write_media(&part, 3);
    write_media(&part, 1);
    write_history(&part.join("json"), &[("friend-handle", &[(sent, "b~aB3xY90002"), (sent, "b~aB3xY90003"), (sent, "b~aB3xY90001")])]);

    let third = collect(&inputs);
    assert_eq!(report(finished(&third)).fixed, 2, "{:?}", report(finished(&third)).failed);
    let after = planned(&third);
    assert_eq!(after.rows.len(), 3, "the newcomer never became an item, so the names below prove nothing");
    assert_eq!(
        named(after, 1).as_deref(),
        Some("20210304_143005_2.jpg"),
        "the newcomer was held off by a record the enrollment had already cleared"
    );
    assert_eq!(
        named(after, 3).as_deref(),
        Some("20210304_143005_3.jpg"),
        "the returning file was handed back a record its own return cleared"
    );
}

/// **The privacy gate, at the boundary the screen reads from.** A conversation key is a friend's
/// username and it names an output DIRECTORY, so it must reach neither a table row nor the counts.
/// Asserted against the event the screen actually consumes, so a future field that carried a path
/// would fail here rather than in a render test that happened to be too narrow to show it.
#[test]
fn no_conversation_key_reaches_the_planned_event() {
    let dir = export_tree(&[(SECRET_KEY, &[("2021-03-04 14:30:05 UTC", "b~aB3xY90001")])], &[1, 2]);
    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    let events = collect(&inputs);
    let snapshot = planned(&events);

    // The key really is in play: the run wrote a directory named after it, so a screen that carried
    // the path WOULD leak it and this test is not passing vacuously.
    assert!(dir.path().join(format!("out/chat/{SECRET_KEY}")).is_dir(), "the fixture's key must reach the output tree");

    for row in &snapshot.rows {
        assert!(!row.output_name.contains(SECRET_KEY), "a conversation key reached an output name: {}", row.output_name);
        assert!(!row.output_name.contains('/'), "an output NAME must carry no path separator: {}", row.output_name);
        assert!(!row.source_id.contains(SECRET_KEY), "a conversation key reached a source id: {}", row.source_id);
        assert!(!row.source_id.contains("sender-handle"), "a sender reached a source id: {}", row.source_id);
    }
}

#[test]
fn the_counts_line_reports_every_absence_the_plan_found() {
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    // A token no file carries, an unmatched overlay, a thumbnail, and a format this build defers.
    write_history(&part.join("json"), &[(SECRET_KEY, &[("2021-03-04 14:30:05 UTC", "b~aB3xY90001 | b~aB3xY99999")])]);
    write_media(&part, 1);
    write_unmatched_overlay(&part, 7);
    write_thumbnail(&part, 8);
    fs::write(chat_media_dir(&part).join(format!("{DAY}_b~aB3xY90009.heic")), b"not a format this build reads").unwrap();

    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    let events = collect(&inputs);
    let counts = planned(&events).counts;

    assert_eq!(counts.unmatched_overlays, 1, "the lone overlay~ file is its own item and nothing paired it");
    assert_eq!(counts.excluded, 1, "the thumbnail is enrolled and never written");
    assert_eq!(counts.deferred, 1, "the heic is a format this build does not decode");
    assert_eq!(counts.missing_tokens, 1, "one token the history names has no file");
    assert_eq!(counts.history, HistoryOutcome::Joined, "a message named a file, so the run had something to attribute");
    assert!(!counts.partial, "every dir was listable, so the counts are exact");
}

/// An export delivered without the chat category still repairs every file it holds. Refusing would
/// decline work this build can genuinely do — the file's own name carries its day.
#[test]
fn an_export_with_no_chat_history_still_runs_and_says_nothing_was_attributed() {
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    fs::create_dir_all(part.join("json")).unwrap();
    write_media(&part, 1);

    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    let events = collect(&inputs);
    let counts = planned(&events).counts;
    assert_eq!(report(finished(&events)).fixed, 1);
    assert_eq!(counts.history, HistoryOutcome::Absent, "the export carried no chat_history.json at all");
    assert_eq!(counts.missing_tokens, 0, "with no history there is no token to be missing");
    assert!(dir.path().join("out/chat/_no-conversation/2021/03/20210304_000000.jpg").is_file());
}

/// The other sub-state of `JoinedNothing`: a history file that exists and holds no messages.
///
/// `{}` parses to `Some(ChatHistory { conversations: [] })`, so the run READ a history and simply
/// had nothing to compare — which is why the copy may not say a comparison happened. This is the
/// state three fixtures in this file already construct incidentally; here it is asserted on
/// purpose, because a string describing a comparison would be false in it and nothing else would
/// notice.
#[test]
fn a_history_with_no_messages_is_not_described_as_an_unmatched_one() {
    let dir = export_tree(&[], &[1]);
    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    let events = collect(&inputs);
    let counts = planned(&events).counts;

    assert_eq!(counts.history, HistoryOutcome::JoinedNothing, "the file was there and joined nothing — not Absent");
    assert_eq!(counts.missing_tokens, 0, "no message named anything, so nothing can be missing");

    let mut app = app_on_export(&dir);
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    feed_plan(&mut app, state.path(), Vec::new(), counts);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let line = cell_run(terminal.backend().buffer(), 2);
    assert!(line.contains("chat history read, nothing attributed"), "{line}");
    assert!(!line.contains("no chat history"), "the file was present: {line}");
    assert!(!line.contains("matched"), "no comparison ran, so none may be reported: {line}");
}

#[test]
fn a_run_with_no_chat_media_reports_no_chat_media_dir() {
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    write_history(&part.join("json"), &[(SECRET_KEY, &[("2021-03-04 14:30:05 UTC", "b~aB3xY90001")])]);
    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    match finished(&collect(&inputs)) {
        RunOutcome::Failed(RunError::NoChatMediaDir(_)) => {}
        other => panic!("expected NoChatMediaDir, got {other:?}"),
    }
}

#[test]
fn a_run_with_no_parts_reports_no_export_id() {
    let dir = TempDir::new().unwrap();
    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    match finished(&collect(&inputs)) {
        RunOutcome::Failed(RunError::NoExportId(_)) => {}
        other => panic!("expected NoExportId, got {other:?}"),
    }
}

#[test]
fn two_deliveries_in_one_source_dir_are_reported_not_guessed() {
    let dir = TempDir::new().unwrap();
    for id in ["mydata~111", "mydata~222"] {
        let part = dir.path().join(id);
        write_history(&part.join("json"), &[]);
        write_media(&part, 1);
    }
    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    match finished(&collect(&inputs)) {
        RunOutcome::Failed(RunError::SeveralExports { count: 2, .. }) => {}
        other => panic!("expected SeveralExports(2), got {other:?}"),
    }
}

/// The six [`RunError`] variants this crate formats itself interpolate a path the CALLER passed in
/// and a count, and nothing else.
///
/// **This carries a positive control because its predecessor did not, and was therefore vacuous.**
/// The old version fed `/export` into every variant and asserted the output held no `SECRET_KEY` —
/// which no `Display` impl could ever have violated, since the key was never an input. A clean
/// result there said nothing about the code. The marker below IS an input, so the assertion that it
/// appears proves the message is really being rendered and that a search would find a string if one
/// were present; only then does the absence of the key mean anything.
///
/// The four delegating variants are deliberately absent: their prose belongs to their source type,
/// not to this crate, and `a_json_error_over_a_conversation_keyed_file_names_no_key` measures the
/// one of them that can reach user data.
#[test]
fn a_run_errors_own_prose_names_only_the_callers_path() {
    const MARKER: &str = "zqxsourcepathmarkerzqx";
    let source = PathBuf::from(format!("/tmp/{MARKER}"));
    let errors = [
        RunError::NoExportId(source.clone()),
        RunError::SeveralExports { source: source.clone(), count: 2 },
        RunError::NoJsonDir(source.clone()),
        RunError::InvalidExportId(source.clone()),
        RunError::NoChatMediaDir(source),
    ];
    for error in errors {
        let text = error.to_string();
        // The control: the caller's own path really is rendered, so the sweep below is live.
        assert!(text.contains(MARKER), "the caller's path must reach the message: {text}");
        assert!(!text.contains(SECRET_KEY), "{text}");
        assert!(!text.ends_with('.'), "an alert message is a fragment, not a sentence: {text}");
    }
    // The one variant carrying no input at all still has to say something.
    let panicked = RunError::Panicked.to_string();
    assert!(!panicked.is_empty());
    assert!(!panicked.contains(SECRET_KEY));
}

/// Every [`RunError`] variant is named here, so an eleventh forces a decision rather than inheriting
/// a claim.
///
/// A witness catches the ADDITION and never the OMISSION — a variant dropped from the array above
/// still compiles — so this is not a substitute for that array, it is the thing that stops a new
/// variant being silently covered by the module's privacy claim. Run through the full gate rather
/// than `cargo check --tests`, which never type-checks an integration crate and reds in `src/`
/// instead, reading exactly like the witness firing.
#[test]
fn every_run_error_variant_is_classified_as_self_formatted_or_delegating() {
    fn delegates(error: &RunError) -> bool {
        match error {
            // Formatted here: a caller-supplied path and a count, pinned above.
            RunError::NoExportId(_)
            | RunError::SeveralExports { .. }
            | RunError::NoJsonDir(_)
            | RunError::InvalidExportId(_)
            | RunError::NoChatMediaDir(_)
            | RunError::Panicked => false,
            // Prose owned by the source type; see the module docs for the residual.
            RunError::Json(_) | RunError::Discover(_) | RunError::Scan(_) | RunError::Manifest(_) => true,
        }
    }
    assert!(!delegates(&RunError::Panicked));
    assert!(!delegates(&RunError::NoExportId(PathBuf::from("/export"))));
}

/// The chat leg's own end of the delegating route: a parse failure on the one export file keyed by
/// usernames, driven all the way through `chat_run` rather than through the loader.
///
/// The loader's redaction and its full marker battery live in `tests/export.rs`, where the property
/// belongs — this asserts the composition on top of it does not undo the guarantee, which is a
/// different claim and the one this screen depends on.
///
/// **Both markers now have to be absent, and that is a change of subject rather than a stronger
/// assertion.** This test used to assert the payload was PRESENT, as a control proving serde really
/// quoted the offending value back; that reading was correct while the value reached the message.
/// Now that the loader strips it, the surviving control has to be something the redaction keeps —
/// the file name, the expectation and the position — because a control that asserts the redacted
/// thing is present would pin the defect the fix removed.
#[test]
fn a_json_error_over_a_conversation_keyed_file_names_neither_key_nor_value() {
    const PAYLOAD: &str = "zqxpayloadmarkerzqx";
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    let json = part.join("json");
    fs::create_dir_all(&json).unwrap();
    // A conversation whose value is a string where an array of records belongs.
    fs::write(json.join("chat_history.json"), format!(r#"{{"{SECRET_KEY}":"{PAYLOAD}"}}"#)).unwrap();
    write_media(&part, 1);

    let (inputs, _state) = inputs(&dir, OverlayMode::Both);
    let events = collect(&inputs);
    let RunOutcome::Failed(error) = finished(&events) else { panic!("a mistyped chat_history.json must fail the load") };
    let RunError::Json(source) = error else { panic!("expected a Json load error, got {error:?}") };
    let text = error.to_string();

    // The control, and it is what makes the two sweeps below mean something: the parser really did
    // object to THIS value, which the un-redacted `LoadError` source still proves. Without it a
    // clean sweep is indistinguishable from a run that never reached the parser at all.
    let LoadError::Json { source: raw, .. } = source else { panic!("expected the json arm, got {source:?}") };
    assert!(raw.to_string().contains(PAYLOAD), "the marker never reached serde, so this proves nothing: {raw}");

    // What survives, so the message is still diagnosable.
    assert!(text.contains("chat_history.json"), "{text}");
    assert!(text.contains("expected"), "{text}");
    assert!(text.contains("line 1 column"), "{text}");

    // Neither the conversation key nor the value it held reaches a footer-bound message.
    assert!(!text.contains(SECRET_KEY), "a conversation key reached a footer-bound error message: {text}");
    assert!(!text.contains(PAYLOAD), "the file's own value reached a footer-bound error message: {text}");
}

// ---- the screen ----

fn press(app: &mut App, code: KeyCode) {
    app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

/// Walks to `tab` with `→`, bounded by the tab count.
///
/// The bound is not decoration: `→` is INERT while a pane is descended (the contract traps the
/// arrows there), so an unbounded walk from a descended screen spins forever. That is the screen
/// behaving correctly and the helper behaving badly, and an unbounded loop turns it into a 600-second
/// hang with no failing assertion to read.
///
/// **Both directions are pinned, at this guard and against its own literal.** Termination is structural: a `for` over a finite range cannot spin, so no test adds confidence there. The range being too SMALL is pinned by the walks that go through it — emptying it reds them loudly, 20 of this file's 33 tests at the 2026-08-11 measurement. Re-derive that rather than trusting the count, which moves with every test this file gains. The pin below is among those reds, because its `app_on_export` setup walks from `Overview` first; the twin in `tests/memories_screen.rs` is the one exception in the crate, for the reason recorded there. The literal is pinned by [`walking_off_a_descended_pane_panics_instead_of_spinning`] below, which descends this screen's own pane and then walks off it. The twins in `tests/shell.rs` and `tests/memories_screen.rs` carry their own pins against their own literals; this one spells the same bytes as the shell's by coincidence, not by sharing a constant, so a drift in either is invisible to the other.
fn on_tab(app: &mut App, tab: Tab) {
    for _ in 0..=Tab::ALL.len() {
        if app.active() == tab {
            return;
        }
        press(app, KeyCode::Right);
    }
    panic!("could not reach {tab:?} from {:?}: is a pane descended and trapping the arrows?", app.active());
}

fn row(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect()
}

fn cell_run(buffer: &Buffer, y: u16) -> String {
    row(buffer, y).trim_end().to_owned()
}

fn screen_text(buffer: &Buffer) -> String {
    (0..buffer.area.height).map(|y| row(buffer, y)).collect::<Vec<_>>().join("\n")
}

/// The COLUMN a run of cells spells `needle` at, and the only one.
///
/// Deliberately not `str::find` on the flattened row: that answers in BYTES, and the caret, the
/// panel border and the clause separator sitting ahead of these runs are all multi-byte, so the two
/// answers disagree by several cells on exactly the rows a colour assertion reads — measured, at 4
/// cells on the form's cycle row and 8 on the counts line.
///
/// **A second occurrence is a panic rather than a first-match win.** An absent needle and a
/// wide glyph inside one already fail loudly; a duplicated run would not, and silently colouring
/// the wrong occurrence is the one failure a reader could not tell from a pass. Nothing on these
/// rows duplicates today, so the check is what keeps that true.
fn column_of(buffer: &Buffer, y: u16, needle: &str) -> u16 {
    let symbols: Vec<&str> = (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect();
    let starts: Vec<usize> =
        symbols.windows(needle.chars().count()).enumerate().filter(|(_, run)| run.concat() == needle).map(|(start, _)| start).collect();
    match starts.as_slice() {
        [start] => u16::try_from(*start).unwrap(),
        [] => panic!("{needle:?} is not on row {y}: {:?}", cell_run(buffer, y)),
        several => panic!("{needle:?} is on row {y} at columns {several:?}, so no single run is the subject: {:?}", cell_run(buffer, y)),
    }
}

/// Asserts every cell of the `needle` run on row `y` carries `fg`, so a colour claim is anchored to
/// the text it is about rather than to a column number layout drift would silently move.
fn assert_run_fg(buffer: &Buffer, y: u16, needle: &str, fg: Color, what: &str) {
    let start = column_of(buffer, y, needle);
    for offset in 0..u16::try_from(needle.chars().count()).unwrap() {
        let x = start + offset;
        assert_eq!(buffer[(x, y)].style().fg, Some(fg), "{what}: cell ({x}, {y}) of {needle:?}");
    }
}

fn environment() -> Environment {
    Environment { ffmpeg: None, vlc: None, available_space: Some(3 * 1024 * 1024 * 1024), total_space: Some(5 * 1024 * 1024 * 1024) }
}

/// An app on the chat media tab with a FIXED, short source, so the form's path rows are byte-stable
/// on every box — a tempdir's base length decides whether the head-ellipsis fires.
///
/// `tier` is a parameter rather than a second helper because the tier decides only which colour
/// column the palette resolves to; every other property this fixture exists for is the same on both.
fn app_on_fixed_source(tier: Tier) -> App {
    let mut app = App::new(tier).with_source_environment(PathBuf::from("/export"), Some(PathBuf::from("/export/out")), environment());
    on_tab(&mut app, Tab::ChatMedia);
    app
}

fn app_on_export(dir: &TempDir) -> App {
    let mut app = App::new(Tier::Full).with_source_environment(dir.path().to_path_buf(), Some(dir.path().join("out")), environment());
    on_tab(&mut app, Tab::ChatMedia);
    app
}

/// Feeds the screen a plan whose manifest really exists at `manifest_dir`, so the poll has somewhere
/// to read and the tick machinery runs for real.
fn feed_plan(app: &mut App, manifest_dir: &Path, rows: Vec<PlanRow>, counts: PlanCounts) -> mpsc::Sender<RunEvent> {
    let (sender, receiver) = mpsc::channel();
    let plan = PlanSnapshot { export_id: ExportId::new(EXPORT_ID).unwrap(), manifest_dir: manifest_dir.to_path_buf(), rows, counts };
    sender.send(RunEvent::Planned(plan)).unwrap();
    app.with_chat_media_channel(receiver);
    app.tick();
    sender
}

fn clean_counts() -> PlanCounts {
    PlanCounts { history: HistoryOutcome::Joined, ..PlanCounts::default() }
}

#[test]
fn the_idle_chat_media_tab_renders_the_form_and_the_empty_state() {
    let mut app = app_on_fixed_source(Tier::Full);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();

    assert!(row(buffer, 1).starts_with("╭─ SETUP ─"), "{:?}", row(buffer, 1));
    assert!(row(buffer, 1).contains("PROGRESS"));

    // The six rows the user picked, in caret order. Focusable rows are ragged: label, exactly the
    // ≥ 2-space gap, then the value — never padded to a shared column.
    assert!(cell_run(buffer, 2).contains("❯ source  /export"), "{:?}", cell_run(buffer, 2));
    // The output row names the `chat/` level this leg actually writes into (decision 46a), so it
    // cannot be read as the memories tree.
    assert!(cell_run(buffer, 3).contains("output dir  /export/out/chat"), "{:?}", cell_run(buffer, 3));
    assert!(cell_run(buffer, 4).contains("3.0 GiB"));
    assert!(cell_run(buffer, 4).contains("40%"));
    assert!(cell_run(buffer, 5).contains("overlay mode"), "{:?}", cell_run(buffer, 5));
    assert!(cell_run(buffer, 6).contains("transcode"));
    assert!(cell_run(buffer, 7).contains("start run"));

    assert!(cell_run(buffer, 11).contains("no run yet"), "{:?}", cell_run(buffer, 11));
    assert!(cell_run(buffer, 12).contains("press ↵ to start"), "{:?}", cell_run(buffer, 12));
    assert!(row(buffer, 23).contains("←→ switch"), "{:?}", row(buffer, 23));
    assert!(!app.chat_media().descended());
}

/// The cycle row's contract: every option renders as a bare word, the selected one is bracketed
/// ONLY while the row holds focus, and `space` walks it.
#[test]
fn the_overlay_cycle_brackets_its_selection_only_while_the_row_is_focused() {
    let mut app = app_on_fixed_source(Tier::Full);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    // Blurred (the caret starts on `source`): every option is a bare word, no brackets anywhere.
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let blurred = cell_run(terminal.backend().buffer(), 5);
    assert!(blurred.contains("overlay mode  merged  both  originals"), "{blurred}");
    assert!(!blurred.contains('['), "a blurred cycle row carries its selection by color alone: {blurred}");

    // Focused: the default is bracketed, and only the default.
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.chat_media().overlay_mode(), OverlayMode::Both);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let focused = cell_run(terminal.backend().buffer(), 5);
    assert!(focused.contains("overlay mode  merged  [both]  originals"), "{focused}");

    // `space` cycles; `enter` mirrors it on a state control.
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.chat_media().overlay_mode(), OverlayMode::Originals);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert!(
        cell_run(terminal.backend().buffer(), 5).contains("merged  both  [originals]"),
        "{:?}",
        cell_run(terminal.backend().buffer(), 5)
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.chat_media().overlay_mode(), OverlayMode::Merged, "enter mirrors space on a cycle row");
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.chat_media().overlay_mode(), OverlayMode::Both, "the walk wraps back to the default");

    // The toggle below it still works, and `space` on the chip does nothing (chips take enter only).
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    assert!(!app.chat_media().is_transcode_on());

    // This screen's SECOND call into the shared form-row widget, and unobserved until this line: the
    // cycle row above covers the first, and a frame taken with the caret anywhere else renders this
    // label BLURRED, where the promoted and flattened treatments agree. Same wiring guard and same
    // reason as the memories twin; the tier axis stays pinned once, on the widget.
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_run_fg(terminal.backend().buffer(), 6, "transcode", Palette::new(Tier::Full).text, "focus-promoted toggle label");

    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    assert!(!app.chat_media().is_transcode_on(), "space on the start chip must not reach the toggle");
}

#[test]
fn a_planned_run_renders_the_counts_line_the_bar_the_header_and_one_row_per_item() {
    let dir = export_tree(&[], &[1]);
    let mut app = app_on_export(&dir);
    let state = TempDir::new().unwrap();
    let mut writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    writer
        .enroll(&[
            NewItem { kind: ItemKind::ChatMedia, source_id: &media_id(1), url: None },
            NewItem { kind: ItemKind::ChatMedia, source_id: &media_id(2), url: None },
        ])
        .unwrap();
    drop(writer);

    let sender = feed_plan(
        &mut app,
        state.path(),
        vec![
            PlanRow { source_id: media_id(1), output_name: "20210304_143005.jpg".to_owned(), leg: Leg::Image },
            PlanRow { source_id: media_id(2), output_name: "20210304_000000.jpg".to_owned(), leg: Leg::Image },
        ],
        PlanCounts { unmatched_overlays: 224, excluded: 44, history: HistoryOutcome::Joined, ..PlanCounts::default() },
    );

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();

    // The counts line sits ABOVE the bar, so the absences read before the progress does.
    let counts = cell_run(buffer, 2);
    assert!(counts.contains("224 overlays unmatched"), "{counts}");
    assert!(counts.contains("44 thumbnails dropped"), "{counts}");
    assert!(cell_run(buffer, 3).contains("0%"), "{:?}", cell_run(buffer, 3));
    assert!(cell_run(buffer, 4).contains("IDENTITY"), "{:?}", cell_run(buffer, 4));
    assert!(cell_run(buffer, 4).contains("STATUS"));
    assert!(cell_run(buffer, 5).contains("[ pending ]"), "{:?}", cell_run(buffer, 5));
    assert!(cell_run(buffer, 5).contains("20210304_143005.jpg"), "{:?}", cell_run(buffer, 5));

    // A real manifest write flips the pill and advances the bar.
    let output = dir.path().join("out/chat/x/20210304_143005.jpg");
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, b"fixed").unwrap();
    let writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    writer.mark_done(ItemKind::ChatMedia, &media_id(1), &output).unwrap();
    drop(writer);
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(cell_run(buffer, 3).contains("50%"), "{:?}", cell_run(buffer, 3));
    assert!(cell_run(buffer, 5).contains("[ done ]"), "{:?}", cell_run(buffer, 5));

    let report = FixReport {
        resumed: ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0, retired: 0, excluded: 0 },
        fixed: 1,
        failed: vec![],
        skipped: 1,
        deferred: 0,
        excluded: 44,
        notices: vec![],
    };
    sender.send(RunEvent::Finished(RunOutcome::Completed(report))).unwrap();
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert!(
        row(terminal.backend().buffer(), 23).contains(" i run finished · 1 fixed · 1 skipped · 44 dropped"),
        "{:?}",
        row(terminal.backend().buffer(), 23)
    );
}

// ---- the compatible tier ----
//
// Every other render test on this screen drives `Tier::Full`. Nothing in the widget code branches on
// the tier — `Palette` keeps its own `tier` field private so that it cannot — which makes the two
// tiers agreeing the expected result rather than a discovery. The frames below capture it
// anyway, because low-by-construction is exactly the claim a frame either confirms or kills, and the
// overview screen's tier work is what established that the two tiers can disagree at all.
//
// **Every FOREGROUND expectation is a per-tier literal: four roles — `ACCENT`, `TEXT_FAINT`,
// `WARNING`, `TEXT_DIM` — spelled out once per tier, and no role shares a value across the two.**
// That is what separates these from a text-only pin: brackets are ascii and a clause is a clause, so
// glyphs alone would read identically whatever the palette resolved to, while a palette that
// flattened the two tiers into one column reds on one of the two passes.
//
// Two expectations here are deliberately NOT literals — the focused row's `BG_HOVER` and the
// promoted label's `TEXT` — because both derive from `Palette::new(tier)` and so cannot detect a
// flattening at all. Measured on the first: moving `compatible::BG_HOVER` from 236 to 240 leaves
// every test here green and reds `tests/theme.rs`'s two literal pins instead. Each earns its place
// by guarding something no colour literal can see — the `BG_HOVER` guards which half of
// `row_focused` held, the `TEXT` guards whether this screen still routes its interactive labels
// through the shared widget — and the literal for each constant is held elsewhere, in
// `tests/theme.rs` and in `widgets`'s own `the_focus_promoted_form_label_holds_both_tiers`.

/// The cycle row's `[brackets]` and its `ACCENT`/`TEXT_FAINT` split, on both tiers.
///
/// The row is walked to before the frame is taken, because a cycle row wears its brackets **only
/// while focused** — a frame with the caret still on `source` shows none, so a bracket assertion
/// there would be asserting a bug.
///
/// **The two frame assertions pin the two halves of `row_focused = row_selected && !descended`, and
/// that is why both are here.** The brackets in the text assertion need `focused`, so a walk landing
/// short reds there — measured, with the walk cut to two: the row renders `overlay mode  merged
/// both  originals` and no caret. The tint read off the bracket's own cells needs `selected`, the
/// other half, and is independently killable. An `overlay_mode() == Both` check would pin neither:
/// `Both` is the `#[default]` and no key pressed here changes it, so it holds whatever the caret
/// did — it passes verbatim with the walk two rows short. It is deliberately absent; don't add it
/// back as a landing guard.
#[test]
fn the_cycle_rows_brackets_and_selection_colours_survive_the_compatible_tier() {
    for (tier, accent, faint) in
        [(Tier::Full, Color::Rgb(67, 171, 229), Color::Rgb(127, 132, 156)), (Tier::Compatible, Color::Indexed(75), Color::Indexed(102))]
    {
        let mut app = app_on_fixed_source(tier);
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }

        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();

        assert!(cell_run(buffer, 5).contains("overlay mode  merged  [both]  originals"), "{tier:?}: {:?}", cell_run(buffer, 5));
        assert_eq!(
            buffer[(column_of(buffer, 5, "[both]"), 5)].style().bg,
            Some(Palette::new(tier).bg_hover),
            "{tier:?}: the brackets are on a row that is really focused"
        );

        assert_run_fg(buffer, 5, "[both]", accent, &format!("{tier:?} selected option"));
        assert_run_fg(buffer, 5, "merged", faint, &format!("{tier:?} unselected option"));
        assert_run_fg(buffer, 5, "originals", faint, &format!("{tier:?} unselected option"));
    }
}

/// The counts line's lower-bound qualifier, on both tiers.
///
/// The qualifier is the only clause on that line carrying a semantic colour instead of the dim
/// default, so it is the one a tier could quietly flatten. An ordinary count clause rides in the
/// same fixture on purpose: both colours come off one frame, so a palette that collapsed `WARNING`
/// into `TEXT_DIM` on this tier reds here rather than reading as a pass.
#[test]
fn the_counts_lines_lower_bound_qualifier_survives_the_compatible_tier() {
    const QUALIFIER: &str = "some dirs unreadable, counts are lower bounds";
    for (tier, warning, dim) in
        [(Tier::Full, Color::Rgb(249, 226, 175), Color::Rgb(166, 173, 200)), (Tier::Compatible, Color::Indexed(223), Color::Indexed(145))]
    {
        let mut app = app_on_fixed_source(tier);
        let state = TempDir::new().unwrap();
        let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
        feed_plan(&mut app, state.path(), Vec::new(), PlanCounts { partial: true, unmatched_overlays: 2, ..clean_counts() });

        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();

        let line = cell_run(buffer, 2);
        assert!(line.contains(&format!("{QUALIFIER} · 2 overlays unmatched")), "{tier:?}: {line}");
        assert_run_fg(buffer, 2, QUALIFIER, warning, &format!("{tier:?} lower-bound qualifier"));
        assert_run_fg(buffer, 2, "2 overlays unmatched", dim, &format!("{tier:?} ordinary clause"));
    }
}

/// The stacked and form-only layout arms, on both tiers.
///
/// **Both arms render in this file alone, and every frame either had ever been taken at was
/// `Tier::Full`.** The two privacy sweeps are what drive them, through `app_on_export`, which runs a
/// real worker end to end — so crossing the tier there would double a slow test to cover a property
/// no run touches. This fixture takes the same two sizes with no run behind it at all.
///
/// The arms are asserted by where the table panel LANDS rather than by a size, because a size alone
/// proves nothing about which branch of the ladder ran: stacked puts `PROGRESS` below the form and
/// side by side puts it on the same row, so a width that quietly stopped being narrow enough would
/// otherwise pass here as a stacked frame.
///
/// **The label expectation is a WIRING guard and not a second tier pin**, per the section note
/// above: it derives from `Palette::new(tier)`, so a palette that flattened `TEXT` into `TEXT_DIM`
/// passes it. That flattening is `widgets`'s `the_focus_promoted_form_label_holds_both_tiers`'s job,
/// once, on the widget itself. What this catches instead is the screen ceasing to route its
/// interactive labels through that widget, which no colour literal anywhere would notice.
#[test]
fn the_stacked_and_form_only_arms_render_on_both_tiers() {
    for tier in [Tier::Full, Tier::Compatible] {
        let palette = Palette::new(tier);
        let mut app = app_on_fixed_source(tier);
        // Onto the overlay row, so the form carries a focus-promoted interactive label at all.
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }

        // Stacked: too narrow to hold both panels side by side, tall enough for the table's floor.
        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row(buffer, 1).starts_with("╭─ SETUP ─"), "{tier:?}: {:?}", row(buffer, 1));
        assert!(!row(buffer, 1).contains("PROGRESS"), "{tier:?}: the arm is stacked, not side by side: {:?}", row(buffer, 1));
        assert!(screen_text(buffer).contains("PROGRESS"), "{tier:?}: the table panel sits below the form, not nowhere");
        assert_run_fg(buffer, 5, "overlay mode", palette.text, &format!("{tier:?} focus-promoted label"));

        // Form-only: narrower than one form panel's own floor, so the table is dropped outright.
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row(buffer, 1).starts_with("╭─ SETUP ─"), "{tier:?}: {:?}", row(buffer, 1));
        assert!(!screen_text(buffer).contains("PROGRESS"), "{tier:?}: the form-only arm draws no table panel");
    }
}

/// **The privacy gate, at the pixels.** A real run against an export whose conversation key is
/// distinctive, then a sweep of every rendered cell for that key. This is the assertion that would
/// fail if a directory name, a sender or a title reached a table cell, a counts clause or the alert.
#[test]
fn no_conversation_key_reaches_the_screen() {
    let dir = export_tree(&[(SECRET_KEY, &[("2021-03-04 14:30:05 UTC", "b~aB3xY90001")])], &[1, 2]);
    let mut app = app_on_export(&dir);
    let state = TempDir::new().unwrap();
    app.chat_media_mut().set_manifest_dir(state.path().to_path_buf());

    press(&mut app, KeyCode::Enter);
    wait_for_alert(&mut app);

    // The run really did name a directory after the key, so the search below is not vacuous.
    assert!(dir.path().join(format!("out/chat/{SECRET_KEY}")).is_dir());

    // Every width the screen has a layout for: side by side, stacked, and form-only.
    for (width, height) in [(120, 24), (60, 30), (40, 20)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
        let text = screen_text(terminal.backend().buffer());
        assert!(!text.contains(SECRET_KEY), "a conversation key rendered at {width}x{height}:\n{text}");
        assert!(!text.contains("sender-handle"), "a sender rendered at {width}x{height}:\n{text}");
    }

    // And the completion alert, which reaches the footer verbatim.
    let alert = app.chat_media().alert().unwrap();
    assert!(!alert.message.contains(SECRET_KEY), "{}", alert.message);
    assert_eq!(alert.kind, AlertKind::Info, "{}", alert.message);
    assert!(alert.message.contains("2 fixed"), "{}", alert.message);
}

/// **The failing branch of the privacy sweep**, which the all-success one structurally cannot reach.
///
/// `FixError::Create` renders `could not create {path}`, and on this leg that path is
/// `<out_root>/chat/<cleaned conversation key>/…` — so a failed chat item puts a friend's username
/// into `Failure::reason` verbatim. The only thing between it and the footer is that
/// `RunAlert::completion` reads `failed.len()` and never a reason. That guard is deliberate and,
/// until this test, entirely unpinned: every `FixReport` in this file was built `failed: vec![]`, so
/// the sweep ran on the one branch where no key-bearing string was ever constructed — a fixture
/// holding constant the exact dimension its own assertion names.
///
/// The failure is induced without touching permissions: a FILE pre-occupying the conversation's
/// output directory makes `create_dir_all` fail deterministically. A `chmod 000` fixture no-ops
/// under root and varies by filesystem, which buys a flake or a silent skip instead of a test.
#[test]
fn a_failing_chat_item_keeps_its_conversation_out_of_the_alert() {
    let dir = export_tree(&[(SECRET_KEY, &[("2021-03-04 14:30:05 UTC", "b~aB3xY90001")])], &[1]);
    let mut app = app_on_export(&dir);
    let state = TempDir::new().unwrap();
    app.chat_media_mut().set_manifest_dir(state.path().to_path_buf());

    // Occupy the conversation's own output directory with a file, so `make_parent` cannot create it.
    let chat_root = dir.path().join("out").join("chat");
    fs::create_dir_all(&chat_root).unwrap();
    fs::write(chat_root.join(SECRET_KEY), b"not a directory").unwrap();

    press(&mut app, KeyCode::Enter);
    wait_for_alert(&mut app);

    // The vacuity guard, and the reason the sweep below means anything: the run really did produce a
    // failure message carrying the conversation key. Without this the test would pass on a run that
    // simply succeeded.
    let alert = app.chat_media().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning, "{}", alert.message);

    // …and the guard holds: the count reaches the footer, the reason does not.
    assert!(alert.message.contains("1 failed"), "{}", alert.message);
    assert!(!alert.message.contains(SECRET_KEY), "a conversation key reached the footer alert: {}", alert.message);

    for (width, height) in [(120, 24), (60, 30), (40, 20)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
        let text = screen_text(terminal.backend().buffer());
        assert!(!text.contains(SECRET_KEY), "a conversation key rendered at {width}x{height} on the failing branch:\n{text}");
    }
}

/// Ticks until an alert lands, bounded by WALL CLOCK rather than by an iteration count.
///
/// The distinction is not pedantry. A run here encodes real images on a worker thread while nextest
/// runs the rest of the suite in parallel, so how long the worker takes moves with the box. An
/// iteration count does not move with it: the 2 ms sleep barely stretches under contention, so
/// counting to 500 is a fixed deadline of about a second rather than a budget that scales with the
/// thing it bounds (the pin below carries the measurements). Those two together are the bug — the
/// count ran out precisely when the box was slow enough to be worth waiting for, and the failure
/// read as the assertion under test rather than as the timeout it was. The bound stays generous
/// because its only job is to stop a hang, and a hang is what a bug here looks like.
///
/// **The deadline arm is deliberately unpinned, and that is a cost decision rather than an oversight.** Firing it needs a worker that never sends, which is 60 s of gate against a 3.5 s release suite (measured 2026-08-11), and parameterising the deadline so a test could pass a short one would move the untested boundary onto whoever supplies the real 60 s rather than remove it. The half that rots silently is the deadline being too SHORT, and that one is pinned below by `a_worker_slower_than_the_old_iteration_budget_still_lands_its_alert`. Termination is structural — the loop runs against a fixed `Instant` — so what is unpinned is the message alone. Do not read that as the deadline being decoration: without it a worker that never finishes wedges the suite, since this crate configures no nextest `terminate-after`.
fn wait_for_alert(app: &mut App) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        app.tick();
        if app.chat_media().alert().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("no alert arrived within 60s: the worker never sent a Finished event");
}

/// A worker slower than the old iteration budget still gets waited for.
///
/// This is the only thing separating the wall-clock bound from the iteration count it replaced:
/// every other test's worker finishes in tens of milliseconds, so both shapes pass them. The stall
/// is 3 s against a budget measured at 1.03 s unloaded and 1.6 s starved on a shared core — a
/// 1.9x margin, which keeps this green on a loaded box while still reddening if the bound goes
/// back to counting iterations.
#[test]
fn a_worker_slower_than_the_old_iteration_budget_still_lands_its_alert() {
    let dir = export_tree(&[], &[]);
    let mut app = app_on_export(&dir);
    app.chat_media_mut().start_run_with(
        |_inputs, _sender| {
            std::thread::sleep(Duration::from_secs(3));
            panic!("a worker that outlives the old budget");
        },
        None,
    );
    // Vacuity guard: nothing has landed yet, so the wait below is what produces the alert.
    assert!(app.chat_media().alert().is_none());

    wait_for_alert(&mut app);
    let alert = app.chat_media().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning, "{}", alert.message);
}

/// The screen-driven run honours the cycle row: switching to `originals` before pressing start means
/// the caption is never burned in and the export's two files are kept.
#[test]
fn the_start_chip_runs_under_the_overlay_mode_the_cycle_row_shows() {
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    write_history(&part.join("json"), &[]);
    // A zip pair, the only family that pairs, so the mode has something to act on.
    let media_dir = chat_media_dir(&part);
    let mut pixels = RgbImage::new(WIDTH, HEIGHT);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = Rgb([(x * 9) as u8, (y * 11) as u8, 40]);
    }
    pixels.save_with_format(media_dir.join(format!("{DAY}_media~vantsnap-0000004.zip.a1b2c3d.jpg")), ImageFormat::Jpeg).unwrap();
    let mut overlay = RgbaImage::new(WIDTH, HEIGHT);
    for (x, _, pixel) in overlay.enumerate_pixels_mut() {
        *pixel = if x < WIDTH / 2 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 0, 0]) };
    }
    overlay.save_with_format(media_dir.join(format!("{DAY}_overlay~vantsnap-0000004.zip.a1b2c3d.png")), ImageFormat::Png).unwrap();

    let mut app = app_on_export(&dir);
    let state = TempDir::new().unwrap();
    app.chat_media_mut().set_manifest_dir(state.path().to_path_buf());

    // Walk to the cycle row and pick `originals`.
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.chat_media().overlay_mode(), OverlayMode::Originals);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    wait_for_alert(&mut app);
    assert_eq!(app.chat_media().alert().unwrap().kind, AlertKind::Info, "{}", app.chat_media().alert().unwrap().message);

    let out = dir.path().join("out/chat/_no-conversation/2021/03");
    assert!(out.join("originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png").is_file(), "originals mode keeps the pair");
    let written = image::open(out.join("20210304_000000.jpg")).unwrap().to_rgb8();
    assert!(written.get_pixel(2, 2).0[0] < 60, "the caption was not burned in: {:?}", written.get_pixel(2, 2));
}

// ---- alert routing across two screens ----

/// Two screens can each hold an alert and there is one footer row, so the row shows the ACTIVE
/// screen's and `x` dismisses that same one. A memories alert must not appear on the chat tab, and
/// dismissing one must not touch the other.
#[test]
fn the_footer_shows_the_active_screens_alert_and_x_dismisses_that_one() {
    let dir = export_tree(&[], &[1]);
    let mut app = app_on_export(&dir);

    let (chat_sender, chat_receiver) = mpsc::channel();
    chat_sender.send(RunEvent::Finished(RunOutcome::Failed(RunError::NoChatMediaDir(PathBuf::from("/export"))))).unwrap();
    drop(chat_sender);
    app.with_chat_media_channel(chat_receiver);

    let (memories_sender, memories_receiver) = mpsc::channel();
    memories_sender
        .send(exportsnap::export::memories_run::RunEvent::Finished(exportsnap::export::memories_run::RunOutcome::Failed(
            exportsnap::export::memories_run::RunError::NoMemoriesFile,
        )))
        .unwrap();
    drop(memories_sender);
    app.with_memories_channel(memories_receiver);
    app.tick();

    // Both are live; the chat tab is active, so the footer carries the chat one.
    assert!(app.memories().alert().is_some());
    assert!(app.chat_media().alert().is_some());
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let footer = row(terminal.backend().buffer(), 23);
    assert!(footer.contains("no chat media under"), "the active screen's alert is the one on the row: {footer}");
    assert!(!footer.contains("memories_history.json"), "the other screen's alert must not appear here: {footer}");

    // `x` dismisses the one on the row, and only that one.
    press(&mut app, KeyCode::Char('x'));
    assert!(app.chat_media().alert().is_none());
    assert!(app.memories().alert().is_some(), "x must not reach a screen the user is not looking at");
    assert!(app.is_running());

    // Switch to memories: its own alert is now the one on the row, and `x` there clears it.
    press(&mut app, KeyCode::Left);
    assert_eq!(app.active(), Tab::Memories);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert!(row(terminal.backend().buffer(), 23).contains("memories_history.json"), "{:?}", row(terminal.backend().buffer(), 23));
    press(&mut app, KeyCode::Char('x'));
    assert!(app.memories().alert().is_none());

    // With nothing live `x` is inert: it neither quits nor moves the tab.
    press(&mut app, KeyCode::Char('x'));
    assert!(app.is_running());
    assert_eq!(app.active(), Tab::Memories);
}

/// An alert raised on a tab the user is not looking at waits rather than vanishing — the cost of
/// the active-screen rule, pinned so it stays a cost and not a leak.
#[test]
fn an_alert_raised_on_a_background_tab_survives_until_its_tab_is_visited() {
    let dir = export_tree(&[], &[1]);
    let mut app = app_on_export(&dir);
    let (sender, receiver) = mpsc::channel();
    sender.send(RunEvent::Finished(RunOutcome::Failed(RunError::Panicked))).unwrap();
    drop(sender);
    app.with_chat_media_channel(receiver);
    app.tick();

    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Left);
    assert_eq!(app.active(), Tab::Overview);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert!(!row(terminal.backend().buffer(), 23).contains("unexpectedly"), "{:?}", row(terminal.backend().buffer(), 23));
    assert!(app.chat_media().alert().is_some(), "the alert is waiting, not gone");

    // `x` on a tab with no alert is inert and must not clear the waiting one.
    press(&mut app, KeyCode::Char('x'));
    assert!(app.chat_media().alert().is_some());

    on_tab(&mut app, Tab::ChatMedia);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert!(row(terminal.backend().buffer(), 23).contains("unexpectedly"), "{:?}", row(terminal.backend().buffer(), 23));
}

// ---- focus and the worker ----

#[test]
fn entering_on_a_static_row_descends_and_q_ascends_without_arming_the_quit() {
    let dir = export_tree(&[], &[1]);
    let mut app = app_on_export(&dir);
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: media_id(1), output_name: "x.jpg".to_owned(), leg: Leg::Image }],
        clean_counts(),
    );

    press(&mut app, KeyCode::Enter);
    assert!(app.chat_media().descended());

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert!(row(terminal.backend().buffer(), 23).contains("esc back"), "{:?}", row(terminal.backend().buffer(), 23));

    press(&mut app, KeyCode::Right);
    assert!(app.chat_media().descended(), "→ is inert while descended");
    press(&mut app, KeyCode::Left);
    assert!(!app.chat_media().descended(), "← ascends");

    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('q'));
    assert!(!app.chat_media().descended(), "q ascends while descended");
    assert!(!app.is_quit_armed(), "that q armed nothing");

    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    assert!(!app.chat_media().descended());
}

/// [`on_tab`]'s panic arm: a walk off a descended pane gives up with a diagnosis instead of spinning.
///
/// What the three guards exist to prevent is a walk that never terminates; here the pane is descended, `→` is inert, and the helper gives up. The twins in `tests/shell.rs` and `tests/memories_screen.rs` each carry the same pin against their own literal, since the three panics are three independent strings.
///
/// **`should_panic` on the WHOLE message, not a fragment**, so the target tab and the diagnosis are both pinned. `app_on_export` itself calls `on_tab(&mut app, Tab::ChatMedia)`, whose panic would read `could not reach ChatMedia from …` and would satisfy a fragment like `is a pane descended and trapping the arrows?` — the full literal is what keeps a setup failure from passing as the subject. Deleting the bound makes this test HANG rather than red, which is unavoidable rather than sloppy: the property under test is "does not hang", and nothing short of a timeout harness can red on its absence.
///
/// **What it reds on is narrower than "the literal drifting", so do not lean on it for more.** `should_panic` matches by CONTAINMENT, so only an edit INSIDE the expected substring reds; text added around the literal — a prefix, a suffix, an extra leading clause — leaves it green (measured 2026-08-11 on the `tests/shell.rs` twin, whose literal is byte-identical: a prefix left the whole suite green, while `arrows?` → `keys?` red exactly that one pin). The full-literal choice above defeats a too-loose fragment match; it does not make the match exact.
#[test]
#[should_panic(expected = "could not reach Memories from ChatMedia: is a pane descended and trapping the arrows?")]
fn walking_off_a_descended_pane_panics_instead_of_spinning() {
    let dir = export_tree(&[], &[1]);
    let mut app = app_on_export(&dir);
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: media_id(1), output_name: "x.jpg".to_owned(), leg: Leg::Image }],
        clean_counts(),
    );

    // The trap is the whole fixture, so it is asserted rather than assumed. An app that failed to
    // descend would reach the memories tab in five `→` presses (`Tab::next` wraps, `src/app.rs:66`)
    // and the test would fail as "no panic", which reads like a missing guard instead of a broken
    // fixture.
    press(&mut app, KeyCode::Enter);
    assert!(app.chat_media().descended(), "the fixture must leave the pane descended, or the walk below is not trapped");
    assert_eq!(app.active(), Tab::ChatMedia, "the walk has to START somewhere other than its target");

    on_tab(&mut app, Tab::Memories);
}

/// The `⌥` jump ascends the pane it is LEAVING, on either screen.
///
/// Both branches of the app's per-screen ascend are exercised, which together carry the invariant
/// that makes the "ascend only the active screen" reading safe: **no screen is ever left descended
/// on a tab the user is not on**, because every route off a descended pane (`←`, `esc`, `q`, the
/// jump) ascends it and `→` is inert. So two panes cannot be descended at once, and a jump that
/// ascended every screen would be indistinguishable here — the reason to ascend only the active one
/// is that ascending a screen the user is not on is a state change nobody asked for.
#[test]
fn the_alt_jump_ascends_the_pane_it_leaves_on_either_screen() {
    let dir = export_tree(&[], &[1]);
    let mut app = app_on_export(&dir);
    let chat_state = TempDir::new().unwrap();
    let memories_state = TempDir::new().unwrap();
    let _chat_writer = Manifest::open_in(chat_state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    let _memories_writer = Manifest::open_in(memories_state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();

    feed_plan(
        &mut app,
        chat_state.path(),
        vec![PlanRow { source_id: media_id(1), output_name: "x.jpg".to_owned(), leg: Leg::Image }],
        clean_counts(),
    );
    press(&mut app, KeyCode::Enter);
    assert!(app.chat_media().descended());

    // `→` is trapped while descended, so the jump is the only way off this tab that is not a `←`.
    press(&mut app, KeyCode::Right);
    assert_eq!(app.active(), Tab::ChatMedia, "→ is inert while descended");
    app.handle_event(&Event::Key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT)));
    assert_eq!(app.active(), Tab::Overview);
    assert!(!app.chat_media().descended(), "the jump ascended the chat pane it left");

    // The same on the memories screen, through its own plan.
    let (sender, receiver) = mpsc::channel();
    sender
        .send(exportsnap::export::memories_run::RunEvent::Planned(exportsnap::export::memories_run::PlanSnapshot {
            export_id: ExportId::new(EXPORT_ID).unwrap(),
            manifest_dir: memories_state.path().to_path_buf(),
            rows: vec![exportsnap::export::memories_run::PlanRow {
                source_id: "2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e".to_owned(),
                output_name: "y.jpg".to_owned(),
                leg: Leg::Image,
            }],
        }))
        .unwrap();
    app.with_memories_channel(receiver);
    app.tick();
    let _ = sender;

    on_tab(&mut app, Tab::Memories);
    press(&mut app, KeyCode::Enter);
    assert!(app.memories().descended());
    app.handle_event(&Event::Key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT)));
    assert_eq!(app.active(), Tab::ChatMedia);
    assert!(!app.memories().descended(), "the jump ascended the memories pane it left");
    assert!(!app.chat_media().descended(), "and landed on a chat pane that was already ascended");
}

#[test]
fn a_worker_that_panics_still_yields_a_panic_alert_and_no_stuck_spinner() {
    let dir = export_tree(&[], &[1]);
    let mut app = app_on_export(&dir);
    app.chat_media_mut().start_run_with(|_inputs, _sender| panic!("boom"), None);

    wait_for_alert(&mut app);
    let alert = app.chat_media().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning);
    assert!(alert.message.contains("unexpectedly"), "{}", alert.message);

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let progress = cell_run(terminal.backend().buffer(), 11);
    assert!(progress.contains("no run yet"), "{progress}");
    assert!(!progress.contains('\u{280b}'), "{progress}");
}

#[test]
fn a_worker_that_exits_after_finished_does_not_overwrite_the_outcome() {
    let dir = export_tree(&[], &[1]);
    let mut app = app_on_export(&dir);
    let (sender, receiver) = mpsc::channel();
    let report = FixReport {
        resumed: ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0, retired: 0, excluded: 0 },
        fixed: 1,
        failed: vec![],
        skipped: 0,
        deferred: 0,
        excluded: 0,
        notices: vec![],
    };
    sender.send(RunEvent::Finished(RunOutcome::Completed(report))).unwrap();
    drop(sender);
    app.with_chat_media_channel(receiver);
    app.tick();

    let alert = app.chat_media().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Info, "the true outcome must survive the dead channel");
    app.tick();
    assert_eq!(app.chat_media().alert().unwrap().kind, AlertKind::Info);
}

#[test]
fn a_channel_that_goes_dead_without_a_finished_event_reports_a_panic() {
    let dir = export_tree(&[], &[1]);
    let mut app = app_on_export(&dir);
    let (sender, receiver) = mpsc::channel();
    drop(sender);
    app.with_chat_media_channel(receiver);
    app.tick();

    let alert = app.chat_media().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning);
    assert!(alert.message.contains("unexpectedly"), "{}", alert.message);
}

#[test]
fn the_focused_form_row_tint_reaches_the_padding_boundary() {
    let mut app = app_on_fixed_source(Tier::Full);
    // At 55 wide the panels stack and the form takes the full width, so its interior is wider than
    // the row's own content — exactly the case where a tint that stops at the last span shows a gap
    // before the padding boundary.
    let mut terminal = Terminal::new(TestBackend::new(55, 30)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    for x in 2..53 {
        assert_eq!(buffer[(x, 2)].style().bg, Some(palette.bg_hover), "focused row column {x}");
    }
    assert_ne!(buffer[(1, 2)].style().bg, Some(palette.bg_hover));
    assert_ne!(buffer[(53, 2)].style().bg, Some(palette.bg_hover));
    assert_ne!(buffer[(2, 3)].style().bg, Some(palette.bg_hover));
}

#[test]
fn every_tab_renders_with_the_chat_media_screen_at_degenerate_sizes() {
    let sizes = [(0, 0), (1, 1), (4, 4), (16, 3), (17, 2), (255, 1), (1, 255), (500, 3), (45, 14)];
    for (width, height) in sizes {
        let mut app =
            App::new(Tier::Full).with_source_environment(PathBuf::from("/nope"), Some(PathBuf::from("/nope/out")), Environment::default());
        on_tab(&mut app, Tab::ChatMedia);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap_or_else(|error| panic!("at {width}x{height}: {error}"));
    }
}
