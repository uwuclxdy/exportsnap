//! Public-API tests for the memories screen: the run form, the live per-item progress table,
//! the completion footer alert, the run composition against a real tempdir export, and the
//! `--out` boundary.
//!
//! Nothing here reads a real export: every export tree is synthesized in a tempdir (a
//! `mydata~<id>/json/` with a minimal `memories_history.json`, a `memories/` dir holding a
//! couple of tiny JPEGs), and every manifest is opened with `open_in` so the per-user data dir
//! is never touched.
//!
//! Render expectations are cross-checked against the cloudy-tui skill's Panel, Form rows, Toggle
//! row, Action chip, List / table, Status pill, Progress bar, Footer alert, Empty state and Pane
//! focus sections, not against this crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use exportsnap::app::{App, RunDefaults};
use exportsnap::config::Config;
use exportsnap::export::env::Environment;
use exportsnap::export::local_fix::{FixReport, Leg, VideoOptions};
use exportsnap::export::manifest::{ExportId, ItemKind, Manifest, ResumeReport};
use exportsnap::export::memories_run::{self, PlanRow, PlanSnapshot, RunError, RunEvent, RunOutcome};
use exportsnap::tui::alert::AlertKind;
use exportsnap::tui::shell;
use exportsnap::tui::theme::{Palette, Tier};
use image::RgbImage;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use tempfile::TempDir;

const EXPORT_ID: &str = "1784667002819";

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

/// The 50-char place name the reach pins feed: long enough to middle-ellipsise in the 29-cell
/// column, distinctive enough that an assertion on its head/tail cannot match by accident.
const LONG_PLACE: &str = "Wurstelstand at the Danube River promenade, Vienna";

// ---- fixtures ----

fn uuid(seed: u32) -> String {
    format!("{seed:08x}-3ff7-45f1-95f9-a2fda6ba0f8e")
}

/// Writes a tiny solid JPEG into `dir/memories` named like a memory's main file.
fn write_main(dir: &Path, day: &str, seed: u32) {
    let memories = dir.join("memories");
    fs::create_dir_all(&memories).unwrap();
    let path = memories.join(format!("{day}_{}-main.jpg", uuid(seed)));
    RgbImage::new(WIDTH, HEIGHT).save_with_format(&path, image::ImageFormat::Jpeg).unwrap();
}

fn at(day: &str, time: &str) -> String {
    format!("{day} {time} UTC")
}

/// `memories_history.json` in the shape the parser expects: one `Saved Media` array of
/// `Date`/`Media Type`/`Location` entries. Built by hand (the schema types are deserialize-only)
/// in the exact spelling the observed export uses.
fn write_json(json_dir: &Path, rows: &[(&str, &str, &str)]) {
    fs::create_dir_all(json_dir).unwrap();
    let entries: Vec<String> = rows
        .iter()
        .map(|(date, media_type, location)| format!(r#"{{"Date":"{date}","Media Type":"{media_type}","Location":"{location}"}}"#))
        .collect();
    fs::write(json_dir.join("memories_history.json"), format!(r#"{{"Saved Media":[{}]}}"#, entries.join(","))).unwrap();
}

/// One delivery: part 1 unpacked with its `json/` and a `memories/` dir.
fn export_tree(name: &str, rows: &[(&str, &str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    write_json(&part.join("json"), rows);
    for (index, (day, _, _)) in rows.iter().enumerate() {
        // The filename carries only the day; the entry's time of day stays in the json.
        write_main(&part, day.split(' ').next().unwrap(), u32::try_from(index + 1).unwrap());
    }
    let _ = name;
    dir
}

/// A run's inputs against `dir`'s export, with the manifest parked in a tempdir of its own.
fn inputs(dir: &TempDir) -> (memories_run::RunInputs, TempDir) {
    let state = TempDir::new().unwrap();
    let inputs = memories_run::RunInputs {
        source: dir.path().to_path_buf(),
        out_root: dir.path().join("out"),
        manifest_dir: Some(state.path().to_path_buf()),
        video: VideoOptions { transcode: false, ffmpeg: None },
    };
    (inputs, state)
}

/// Runs `memories_run::run` synchronously and returns the events it sent.
fn collect(inputs: &memories_run::RunInputs) -> Vec<RunEvent> {
    let (sender, receiver) = mpsc::channel();
    memories_run::run(inputs, &sender);
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
    let dir = export_tree("composition", &[(&at("2021-01-15", "13:30:05"), "Image", ""), (&at("2021-02-20", "09:00:00"), "Image", "")]);
    let (inputs, _state) = inputs(&dir);
    let events = collect(&inputs);
    let snapshot = planned(&events);
    let report = report(finished(&events));

    assert_eq!(snapshot.export_id, ExportId::new(EXPORT_ID).unwrap());
    assert_eq!(snapshot.rows.len(), 2);
    // The rows carry identity, output name and leg — the three things a table cell renders.
    assert_eq!(snapshot.rows[0].source_id, uuid(1));
    assert_eq!(snapshot.rows[0].output_name, "20210115_133005.jpg");
    assert_eq!(report.fixed, 2, "both entries pair exactly and both fix");
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    // The manifest this run wrote is where the snapshot told the reader to look.
    let manifest = Manifest::open_in(&snapshot.manifest_dir, &snapshot.export_id).unwrap();
    assert_eq!(manifest.items(ItemKind::Memory).unwrap().len(), 2);
}

/// Decision 52's seed, pinned where it is actually WIRED rather than where its type lives.
///
/// `memories_run::prepare` reads a [`exportsnap::export::local_fix::RecordedOutputs`] between the
/// enrollment and the plan, and until this test that line was mutable to green: defaulting it left
/// the whole suite passing, on the leg decision 52b exists to cover. `tests/local_fix.rs` pins the
/// same behaviour but re-drives the composition's order itself, so it proves the plan can take a
/// seed and never that the composition still hands it one.
///
/// Two entries on one day and two files make an ambiguous bucket, so neither item may take its time
/// from its entry and both fall to the filename day's midnight, wanting one name. The survivor is
/// driven back to work through the manifest the snapshot names — the state a resume writes when a
/// user deletes an output — and then the other item's SOURCE leaves the export while its `Done` row
/// keeps the record naming the file it already wrote. Without the seed the survivor is planned onto
/// that file.
///
/// Asserted on the `PlanSnapshot` row rather than on bytes: `write_main` paints one solid colour for
/// every seed, so two outputs here are byte-identical and no digest could tell an overwrite from a
/// rewrite.
#[test]
fn a_departed_items_recorded_output_is_reserved_through_the_run_composition() {
    let dir = export_tree("seed", &[(&at("2021-01-15", "01:00:00"), "Image", ""), (&at("2021-01-15", "23:00:00"), "Image", "")]);
    let (inputs, _state) = inputs(&dir);
    let first = collect(&inputs);
    assert_eq!(report(finished(&first)).fixed, 2);

    let snapshot = planned(&first);
    assert_eq!(
        snapshot.rows.iter().map(|row| row.output_name.as_str()).collect::<Vec<_>>(),
        ["20210115_000000.jpg", "20210115_000000_2.jpg"],
        "the fixture is not two items landing on one second"
    );
    let departing = snapshot.rows[0].source_id.clone();
    let survivor = snapshot.rows[1].source_id.clone();
    let manifest = Manifest::open_in(&snapshot.manifest_dir, &snapshot.export_id).unwrap();
    manifest.reset(ItemKind::Memory, &survivor).unwrap();
    drop(manifest);

    // The departed item's row stays `Done` and keeps its record: `retire_absent` exempts `Done` and
    // decision 50 keeps the three output columns across a park. That record is the whole reservation.
    let main = dir.path().join(format!("mydata~{EXPORT_ID}/memories/2021-01-15_{departing}-main.jpg"));
    assert!(main.is_file(), "the fixture must remove a file that is there");
    fs::remove_file(&main).unwrap();

    let second = collect(&inputs);
    assert_eq!(report(finished(&second)).fixed, 1, "{:?}", report(finished(&second)).failed);
    let rewritten = planned(&second).rows.iter().find(|row| row.source_id == survivor).expect("the survivor is planned");
    assert_eq!(rewritten.output_name, "20210115_000000_2.jpg", "the survivor was planned onto the file the departed row still records");
}

/// Decision 52's seed read against its own resume sweep, on the leg that had no pin for either.
///
/// The twin of `a_newcomer_sorting_ahead_of_a_recorded_item_does_not_take_its_output_name` in
/// `tests/chat_media_screen.rs`. 52b is a ruling about both legs, and this is the leg whose seed
/// read was mutable to green one round ago, so it is the worse one to leave stated-but-unpinned.
///
/// **The newcomer is what separates the seed being READ from its being read in the right PLACE.**
/// Defaulting the seed reds the pin above; only moving the sweep ahead of the read reds this one.
///
/// - correct — every record is claimed before any derive, so seed 1 walks past both to `_3` and
///   seed 3 is handed back the `_2` it already finished at;
/// - sweep read AHEAD of the seed — seed 3's deleted output has already demoted and cleared its
///   record, so seed 1 takes `_2` and seed 3 is pushed to `_3`, off its own file.
///
/// **Why seed 1 is planned first, stated carefully because the obvious reason is wrong.** It is NOT
/// that item order is uuid order: `memories::reconcile` builds items in ENTRY order
/// (`memories.rs:765`) and each entry then pops the lowest UNCLAIMED uuid from its bucket
/// (`:768`). So the k-th entry carries the k-th lowest uuid, and the row carrying `uuid(1)` is
/// planned first wherever the newcomer's ENTRY sits in the json. That is stronger than an ordering
/// claim would be, and this fixture appends the new entry LAST precisely so a reader cannot take
/// json position as the mechanism. `sorted()` orders the discovered FILES, never the items.
///
/// Built by hand rather than through `export_tree`, which hardcodes `seed = index + 1` and so
/// cannot leave seed 1 free for a later arrival.
#[test]
fn a_newcomer_sorting_ahead_of_a_recorded_item_does_not_take_its_output_name() {
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    // Two entries on one day at two times: an ambiguous bucket, so neither may take its entry's
    // time and both fall to the filename day's midnight and want one name.
    let early = at("2021-01-15", "01:00:00");
    let late = at("2021-01-15", "23:00:00");
    write_json(&part.join("json"), &[(&early, "Image", ""), (&late, "Image", "")]);
    write_main(&part, "2021-01-15", 2);
    write_main(&part, "2021-01-15", 3);

    let (inputs, _state) = inputs(&dir);
    let first = collect(&inputs);
    assert_eq!(report(finished(&first)).fixed, 2, "{:?}", report(finished(&first)).failed);
    let named = |snapshot: &PlanSnapshot, seed: u32| {
        snapshot.rows.iter().find(|row| row.source_id == uuid(seed)).map(|row| row.output_name.clone())
    };
    let snapshot = planned(&first);
    assert_eq!(named(snapshot, 2).as_deref(), Some("20210115_000000.jpg"), "the fixture is not two items on one second");
    assert_eq!(named(snapshot, 3).as_deref(), Some("20210115_000000_2.jpg"), "the fixture is not two items on one second");

    // Only seed 3's OUTPUT goes: its row keeps the record until the sweep inside the next run
    // clears it, which is the window this test is about.
    fs::remove_file(dir.path().join("out/2021/01/20210115_000000_2.jpg")).unwrap();
    write_main(&part, "2021-01-15", 1);
    let noon = at("2021-01-15", "12:00:00");
    write_json(&part.join("json"), &[(&early, "Image", ""), (&late, "Image", ""), (&noon, "Image", "")]);

    let second = collect(&inputs);
    assert_eq!(report(finished(&second)).fixed, 2, "{:?}", report(finished(&second)).failed);
    let after = planned(&second);
    // The newcomer needs its own ENTRY and not just a file: an unclaimed file no entry pairs with
    // lands in `files_without_entry` and never becomes an item at all, which would leave a two-item
    // plan and every name below passing for the wrong reason.
    assert_eq!(after.rows.len(), 3, "the newcomer never became an item, so the names below prove nothing");
    assert_eq!(
        named(after, 3).as_deref(),
        Some("20210115_000000_2.jpg"),
        "the recorded item was pushed off the file it had already finished at"
    );
    assert_eq!(named(after, 1).as_deref(), Some("20210115_000000_3.jpg"), "the newcomer took a name a record already claimed");
}

#[test]
fn a_run_writes_nothing_outside_the_out_root() {
    let dir = export_tree("out-boundary", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let (mut inputs, _state) = inputs(&dir);
    // The out root is the source dir's sibling — the run may write under it and nowhere else.
    let out = dir.path().parent().unwrap().join("custom-out");
    inputs.out_root = out.clone();
    let events = collect(&inputs);
    let report = report(finished(&events));
    assert_eq!(report.fixed, 1);

    // The source tree gained nothing: it still holds exactly the one part dir.
    let source_entries: Vec<String> =
        fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(source_entries.len(), 1, "the source must stay read-only: {source_entries:?}");
    assert!(out.join("2021/01/20210115_133005.jpg").is_file(), "the fixed memory is under the out root");
}

#[test]
fn a_run_without_json_reports_no_memories_file() {
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    fs::create_dir_all(part.join("json")).unwrap();
    write_main(&part, "2021-01-15", 1);
    let (inputs, _state) = inputs(&dir);
    let events = collect(&inputs);

    match finished(&events) {
        RunOutcome::Failed(RunError::NoMemoriesFile) => {}
        other => panic!("expected NoMemoriesFile, got {other:?}"),
    }
}

#[test]
fn a_run_with_no_parts_reports_no_export_id() {
    let dir = TempDir::new().unwrap();
    let (inputs, _state) = inputs(&dir);
    let events = collect(&inputs);
    match finished(&events) {
        RunOutcome::Failed(RunError::NoExportId(_)) => {}
        other => panic!("expected NoExportId, got {other:?}"),
    }
}

#[test]
fn a_run_with_no_media_reports_no_memories_dir() {
    let dir = TempDir::new().unwrap();
    let part = dir.path().join(format!("mydata~{EXPORT_ID}"));
    write_json(&part.join("json"), &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let (inputs, _state) = inputs(&dir);
    let events = collect(&inputs);
    match finished(&events) {
        RunOutcome::Failed(RunError::NoMemoriesDir(_)) => {}
        other => panic!("expected NoMemoriesDir, got {other:?}"),
    }
}

#[test]
fn two_deliveries_in_one_source_dir_are_reported_not_guessed() {
    let dir = TempDir::new().unwrap();
    for id in ["mydata~111", "mydata~222"] {
        let part = dir.path().join(id);
        write_json(&part.join("json"), &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
        write_main(&part, "2021-01-15", 1);
    }
    let (inputs, _state) = inputs(&dir);
    let events = collect(&inputs);
    match finished(&events) {
        RunOutcome::Failed(RunError::SeveralExports { count: 2, .. }) => {}
        other => panic!("expected SeveralExports(2), got {other:?}"),
    }
}

#[test]
fn the_plan_orders_rows_like_the_entries_json() {
    let dir = export_tree("plan-order", &[(&at("2021-01-15", "13:30:05"), "Image", ""), (&at("2021-01-15", "01:00:00"), "Image", "")]);
    let (inputs, _state) = inputs(&dir);
    let events = collect(&inputs);
    let snapshot = planned(&events);

    let ids: Vec<&str> = snapshot.rows.iter().map(|row| row.source_id.as_str()).collect();
    assert_eq!(ids, [uuid(1), uuid(2)], "rows follow memories_history.json order, not the uuid sort");
}

// ---- the screen's tick machinery ----

fn press(app: &mut App, code: KeyCode) {
    app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

/// Bounded for the reason `chat_media_screen.rs`'s `on_tab` spells out in full: `→` is inert while
/// a pane is descended, so an unbounded walk from a descended screen never terminates.
///
/// **Both directions are pinned, at this guard and against its own literal.** Termination is structural: a `for` over a finite range cannot spin, so no test adds confidence there. The range being too SMALL is the half that rots silently, and emptying it reds the walks loudly — 21 of this file's 31 tests at the 2026-08-11 measurement, whole suite, `--no-fail-fast`. Re-derive that rather than trusting the count, which moves with every test this file gains.
///
/// **One walk deliberately survives that mutation, and it is the pin below — do not read its green as the pin being broken.** Every ORDINARY caller here starts on `Overview`, so the emptied range makes them panic `…from Overview`. The pin starts on `ChatMedia`, and the emptied range makes `on_memories` panic before the loop body runs, with `…from ChatMedia` — byte-for-byte the string the pin's `should_panic` expects, so it passes. That is a coincidence of this file only: both twins' pins DO red under their own emptied ranges, because their fixtures walk from `Overview` first. Measured 2026-08-11: 10 passed, 21 failed, pin among the passes. The literal is pinned by [`walking_off_a_descended_pane_panics_instead_of_spinning`] below. Reaching it needs a pane descended on a tab that is NOT memories, and the memories pane can never be both — `→` is trapped while it is descended and the `⌥` jump ascends the pane it leaves (`src/app.rs:422`) — so the fixture descends the CHAT MEDIA pane instead. Each of the three guards carries its own pin: this literal differs outright from the twins' in `tests/shell.rs` and `tests/chat_media_screen.rs`, so a drift here is invisible to both.
fn on_memories(app: &mut App) {
    for _ in 0..=exportsnap::app::Tab::ALL.len() {
        if app.active() == exportsnap::app::Tab::Memories {
            return;
        }
        press(app, KeyCode::Right);
    }
    panic!("could not reach the memories tab from {:?}: is a pane descended and trapping the arrows?", app.active());
}

/// [`on_memories`]'s panic arm: a walk off a descended pane gives up with a diagnosis instead of spinning.
///
/// The memories pane cannot be the trap for its own walk — `→` is inert only while descended, and this helper returns the moment the memories tab is active — so the fixture descends the CHAT MEDIA pane and walks from there. The chat-media import is function-local: it is the state the guard needs, not a surface this file tests. No export tree and no media are involved; a plan with no rows suffices, because `with_channel` sets `Run::Active` (`src/tui/screens/chat_media.rs:257`) and `plan_landed` fills the view regardless of row count (`:413`), so `has_table` is true and `enter` descends.
///
/// **`should_panic` on the WHOLE message, not a fragment**, so the origin tab and the diagnosis are both pinned. A fragment would also be satisfied by the `tests/shell.rs` and `tests/chat_media_screen.rs` twins, whose literals share the trailing clause; the full string is what makes this pin this file's own.
///
/// **What it reds on is narrower than "the literal drifting", so do not lean on it for more.** `should_panic` matches by CONTAINMENT, so only an edit INSIDE the expected substring reds; text added around the literal — a prefix, a suffix, an extra leading clause — leaves it green (measured 2026-08-11 on the `tests/shell.rs` twin: a prefix left the whole suite green, while `arrows?` → `keys?` red exactly that one pin). The full-literal choice above defeats a too-loose fragment match; it does not make the match exact. Note this pin is also the one that survives its own guard's emptied range, for the reason recorded at [`on_memories`] — between the two, it is a narrower instrument than its name suggests.
#[test]
#[should_panic(expected = "could not reach the memories tab from ChatMedia: is a pane descended and trapping the arrows?")]
fn walking_off_a_descended_pane_panics_instead_of_spinning() {
    use exportsnap::export::chat_run;

    let state = TempDir::new().unwrap();
    let mut app = App::new(Tier::Full);
    let (sender, receiver) = mpsc::channel();
    sender
        .send(chat_run::RunEvent::Planned(chat_run::PlanSnapshot {
            export_id: ExportId::new(EXPORT_ID).unwrap(),
            manifest_dir: state.path().to_path_buf(),
            rows: Vec::new(),
            counts: chat_run::PlanCounts::default(),
        }))
        .unwrap();
    app.with_chat_media_channel(receiver);
    app.tick();

    // The trap is the whole fixture, so it is asserted rather than assumed. An app that failed to
    // descend would reach the memories tab in five presses and the test would fail as "no panic",
    // which reads like a missing guard instead of a broken fixture.
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Enter);
    assert!(app.chat_media().descended(), "the fixture must leave the pane descended, or the walk below is not trapped");

    // `sender` lives to end of scope, which is what keeps the channel connected across the tick
    // above; there is deliberately no `drop` after the walk, since the walk never returns.
    on_memories(&mut app);
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

/// An app on the memories tab with a real (tempdir) export tree behind it and a disk-probe
/// environment handed in, so the form rows are deterministic.
fn app_on_memories(dir: &TempDir) -> App {
    let mut app = App::new(Tier::Full).with_source_environment(
        dir.path().to_path_buf(),
        RunDefaults { out_root: dir.path().join("out"), ..RunDefaults::resolve(None, &Config::default(), dir.path()) },
        Environment { ffmpeg: None, vlc: None, available_space: Some(3 * 1024 * 1024 * 1024), total_space: Some(5 * 1024 * 1024 * 1024) },
    );
    on_memories(&mut app);
    app
}

/// An app on the memories tab with a FIXED, short source — for render tests that assert on the
/// form's path rows. A tempdir's base differs per box (and per run), and a long base pushes the
/// source row's head-ellipsis across the truncation threshold, so any assertion on the path's
/// middle would be run-dependent. `/export` never truncates, so the rows are byte-stable.
fn app_on_fixed_source() -> App {
    let mut app = App::new(Tier::Full).with_source_environment(
        PathBuf::from("/export"),
        RunDefaults { out_root: PathBuf::from("/export/out"), ..RunDefaults::resolve(None, &Config::default(), Path::new("/export")) },
        Environment { ffmpeg: None, vlc: None, available_space: Some(3 * 1024 * 1024 * 1024), total_space: Some(5 * 1024 * 1024 * 1024) },
    );
    on_memories(&mut app);
    app
}

/// Feeds the screen a plan whose manifest really exists at `manifest_dir`, so the poll has
/// somewhere to read and the tick machinery runs for real. Returns the sender the caller keeps
/// to send the run's later events.
fn feed_plan(app: &mut App, manifest_dir: &Path, rows: Vec<PlanRow>) -> mpsc::Sender<RunEvent> {
    let (sender, receiver) = mpsc::channel();
    let plan = PlanSnapshot { export_id: ExportId::new(EXPORT_ID).unwrap(), manifest_dir: manifest_dir.to_path_buf(), rows };
    sender.send(RunEvent::Planned(plan)).unwrap();
    app.with_memories_channel(receiver);
    app.tick();
    sender
}

#[test]
fn the_idle_memories_tab_renders_the_form_and_the_empty_state() {
    // The source is fixed at `/export`, so the path rows are byte-stable on every box — a
    // tempdir's base length decides whether the head-ellipsis fires, which would make any
    // assertion on the path's middle run-dependent.
    let mut app = app_on_fixed_source();
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();

    // The two panels sit side by side (120 wide).
    assert!(row(buffer, 1).starts_with("╭─ SETUP ─"));
    assert!(row(buffer, 1).contains("PROGRESS"));
    // The form's five rows, static keys bolded, the disk bar showing 40% used (3 of 5 GiB free).
    // The focusable rows are ragged: label, exactly the ≥ 2-space gap, then the value — never
    // padded to a shared column (that alignment is for display-only rows).
    let source_row = cell_run(buffer, 2);
    assert!(source_row.contains("❯ source  /export"), "{source_row}");
    let output_row = cell_run(buffer, 3);
    assert!(output_row.contains("output dir  /export/out"), "{output_row}");
    assert!(cell_run(buffer, 4).contains("3.0 GiB"));
    assert!(cell_run(buffer, 4).contains("40%"));
    assert!(cell_run(buffer, 5).contains("transcode"));
    assert!(cell_run(buffer, 6).contains("start run"));
    // The empty state names the key that starts the run, centered in the full-height progress
    // panel: 20 interior rows, a 4-row frame, 8 above and 8 below.
    assert!(cell_run(buffer, 11).contains("no run yet"), "{:?}", cell_run(buffer, 11));
    assert!(cell_run(buffer, 12).contains("↵"), "{:?}", cell_run(buffer, 12));
    // The footer hint bar advertises the shell keys.
    assert!(row(buffer, 23).contains("←→ switch"), "{:?}", row(buffer, 23));

    // The toggle row's label, blurred — the caret is still on `source`. This and the promoted half
    // below are this screen's WIRING guard on the shared form-row widget, NOT a second tier pin:
    // the tier axis is pinned once, on the widget, by `the_focus_promoted_form_label_holds_both_tiers`.
    // `transcode` starts at column 4, after the panel's border, its padding cell and the caret gutter.
    let palette = Palette::new(Tier::Full);
    for x in 4..13 {
        assert_eq!(buffer[(x, 5)].style().fg, Some(palette.text_dim), "blurred toggle label, cell ({x}, 5)");
    }

    assert_eq!(app.active(), exportsnap::app::Tab::Memories);
    assert!(!app.is_quit_armed());
    assert!(!app.memories().descended());

    // …and promoted once the caret lands on it, which is the half a flattened palette kills.
    //
    // **What this catches is the CALL, and swapping it back is not what reds it.** Re-inlining a
    // copy of the widget here leaves the whole suite green, because that copy is byte-identical
    // today — measured at 725/725, not assumed. It is an equivalent mutant for the tests, and what
    // it really costs is every LATER edit to the widget. That part IS observable, and is what these
    // two lines are for: flattening the widget reds them while the screen calls it, and stops
    // reddening them the moment the screen carries its own copy (measured, both directions).
    //
    // The swap-back does red the LINT, since it orphans the `form_label` import under `-D warnings`.
    // Do not lean on that — it is a reachability artifact that goes silent the moment this screen
    // gains a second call site, which is exactly the state the chat twin is already in.
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    for x in 4..13 {
        assert_eq!(buffer[(x, 5)].style().fg, Some(palette.text), "focus-promoted toggle label, cell ({x}, 5)");
        assert!(buffer[(x, 5)].style().add_modifier.contains(Modifier::BOLD), "focus-promoted toggle label, cell ({x}, 5)");
    }
}

#[test]
fn the_progress_panel_fills_the_body_below_the_form_at_the_designed_sizes() {
    // The fill contract: the form keeps its `Length` rows at the top and the progress panel
    // takes the rest of the body, so its bottom border sits on the body's last row rather than
    // closing under the empty-state frame the density pass hugged it to. At both designed sizes
    // the panels stack (the location column keeps the side-by-side floor at 116).
    for (width, height) in [(80, 24), (110, 32)] {
        let mut app = app_on_fixed_source();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        // The form opens on the body's first row and closes after its seven rows, top-aligned.
        assert_eq!(buffer[(0, 1)].symbol(), "╭", "the form panel opens on the body's first row");
        assert_eq!(buffer[(0, 7)].symbol(), "╰", "the form keeps its Length rows at the top");
        // The progress panel opens right below the form and fills down to the body's last row.
        assert_eq!(buffer[(0, 8)].symbol(), "╭", "the progress panel opens on the row below the form");
        assert_eq!(buffer[(0, height - 2)].symbol(), "╰", "the progress panel's bottom border sits on the body's last row");
    }
}

#[test]
fn a_planned_run_renders_the_overall_bar_the_header_and_one_row_per_item() {
    let dir = export_tree("live-render", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let mut writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    // The poll reads real rows, so the manifest has to hold them — enroll all three ids.
    writer
        .enroll(&[
            exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(1), url: None },
            exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(2), url: None },
            exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(3), url: None },
        ])
        .unwrap();
    drop(writer);
    let sender = feed_plan(
        &mut app,
        state.path(),
        vec![
            PlanRow { source_id: uuid(1), output_name: "20210115_133005.jpg".to_owned(), place_name: None, leg: Leg::Image },
            PlanRow { source_id: uuid(2), output_name: "20210115_133005_2.jpg".to_owned(), place_name: None, leg: Leg::Image },
            PlanRow { source_id: uuid(3), output_name: "20210115_133005_3.jpg".to_owned(), place_name: None, leg: Leg::Image },
        ],
    );

    // Wide enough that the 19-char output name below still renders whole in the output column:
    // identity grows toward its 36-char uuid and the empty location column stays at its floor,
    // so the surplus reaches the output column and the name need not ellipsise.
    let mut terminal = Terminal::new(TestBackend::new(160, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    // The overall bar reads 0% with three rows pending (whole percentage).
    assert!(cell_run(buffer, 2).contains("0%"), "{:?}", cell_run(buffer, 2));
    // The header names the four columns.
    assert!(cell_run(buffer, 3).contains("IDENTITY"), "{:?}", cell_run(buffer, 3));
    assert!(cell_run(buffer, 3).contains("LOCATION"));
    assert!(cell_run(buffer, 3).contains("STATUS"));
    // Both rows render, each with a pending pill.
    let first = cell_run(buffer, 4);
    assert!(first.contains(&uuid(1)[..8]), "{first}");
    assert!(first.contains("[ pending ]"), "{first}");
    assert!(first.contains("20210115_133005.jpg"), "{first}");
    let second = cell_run(buffer, 5);
    assert!(second.contains("[ pending ]"), "{second}");

    // Mark one done through a real manifest write, then poll: the bar advances and the pill
    // flips, and the selection follows the tail. `mark_done` hashes the output, so the file has
    // to exist.
    let output = dir.path().join("out/2021/01/20210115_133005.jpg");
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, b"fixed").unwrap();
    let writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    writer.mark_done(ItemKind::Memory, &uuid(1), &output).unwrap();
    drop(writer);
    app.tick();

    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    // One of three done is a non-whole percentage: one decimal, per the contract.
    assert!(cell_run(buffer, 2).contains("33.3%"), "{:?}", cell_run(buffer, 2));
    assert!(cell_run(buffer, 4).contains("[ done ]"), "{:?}", cell_run(buffer, 4));

    // The selection follows the tail even while the form owns the caret; the caret glyph itself
    // renders only in the focused pane, so descend and redraw to see it on the last row.
    press(&mut app, KeyCode::Enter);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(cell_run(buffer, 6).contains("❯ 00000003"), "{:?}", cell_run(buffer, 6));
    assert!(!cell_run(buffer, 4).contains("❯"), "{:?}", cell_run(buffer, 4));
    press(&mut app, KeyCode::Esc);

    // The completion event turns the footer row into the INFO alert.
    let report = FixReport {
        resumed: exportsnap::export::manifest::ResumeReport {
            demoted: vec![],
            verified: 0,
            pending: 0,
            failed: 0,
            source_missing: 0,
            retired: 0,
            excluded: 0,
        },
        fixed: 1,
        failed: vec![],
        skipped: 2,
        deferred: 0,
        excluded: 0,
        notices: vec![],
    };
    sender.send(RunEvent::Finished(RunOutcome::Completed(report))).unwrap();
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row(buffer, 23).contains(" i run finished · 1 fixed · 2 skipped"), "{:?}", row(buffer, 23));
}

/// Every status that lands before the run's `Finished` event reaches the table, including the ones
/// committed in the same tick as the event itself.
///
/// `memories_run::run` commits each item as it goes and sends `Finished` only after `local_fix::run`
/// returns, so the tick that drains the finished event is the FIRST tick that could read the last
/// rows. That tick is also the last one this screen ever gets: `run_in_flight` goes false the moment
/// the event is drained, and the event loop stops ticking an idle screen. So a poll that runs only
/// while the worker is still working never reads those rows at all, and the rows they belong to stay
/// frozen — observed on a real pty as permanent `[ pending ]` cells beside a completion alert, with
/// the overall bar stuck at 97.1%.
///
/// The pins below are the two halves of that: the per-row pill and the overall bar (the bar counts
/// the statuses, so a screen that flipped the pills some other way still has to agree), plus
/// `run_in_flight` to hold the premise that no later tick can correct either.
#[test]
fn the_statuses_that_land_with_the_finished_event_still_reach_the_table() {
    let dir = export_tree("final-poll", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let mut writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    writer
        .enroll(&[
            exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(1), url: None },
            exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(2), url: None },
            exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(3), url: None },
        ])
        .unwrap();
    drop(writer);
    let sender = feed_plan(
        &mut app,
        state.path(),
        vec![
            PlanRow { source_id: uuid(1), output_name: "20210115_133005.jpg".to_owned(), place_name: None, leg: Leg::Image },
            PlanRow { source_id: uuid(2), output_name: "20210115_133005_2.jpg".to_owned(), place_name: None, leg: Leg::Image },
            PlanRow { source_id: uuid(3), output_name: "20210115_133005_3.jpg".to_owned(), place_name: None, leg: Leg::Image },
        ],
    );

    // `mark_done` hashes the output, so each one has to exist on disk first.
    let mark_done = |name: &str, id: &str| {
        let output = dir.path().join("out/2021/01").join(name);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, b"fixed").unwrap();
        let writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
        writer.mark_done(ItemKind::Memory, id, &output).unwrap();
    };

    // Mid-run: one item finishes and the ordinary poll picks it up. This is the vacuity guard for
    // the assertions below — without it a screen that never polled at all would read the same.
    mark_done("20210115_133005.jpg", &uuid(1));
    app.tick();
    let mut terminal = Terminal::new(TestBackend::new(160, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(cell_run(buffer, 2).contains("33.3%"), "{:?}", cell_run(buffer, 2));
    assert!(cell_run(buffer, 4).contains("[ done ]"), "{:?}", cell_run(buffer, 4));
    assert!(cell_run(buffer, 6).contains("[ pending ]"), "{:?}", cell_run(buffer, 6));

    // The user scrolls off the tail. `table_move` is what protects that selection — it clears
    // `follow_tail` on every move — and the caret assertions below pin that the final poll honours
    // the cleared flag instead of re-pinning the tail. `finish`'s own clearing is a different line
    // and is pinned separately, by `a_run_that_plans_and_finishes_in_one_tick_…` — the one state
    // where it is observable.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Up);
    assert!(app.memories().descended());

    // The last two items commit, then the worker sends its finished event — the real order, since
    // `local_fix::run` returns before the send.
    mark_done("20210115_133005_2.jpg", &uuid(2));
    mark_done("20210115_133005_3.jpg", &uuid(3));
    let report = FixReport {
        resumed: ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0, retired: 0, excluded: 0 },
        fixed: 3,
        failed: vec![],
        skipped: 0,
        deferred: 0,
        excluded: 0,
        notices: vec![],
    };
    sender.send(RunEvent::Finished(RunOutcome::Completed(report))).unwrap();
    app.tick();

    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(cell_run(buffer, 5).contains("[ done ]"), "the status that landed with the event: {:?}", cell_run(buffer, 5));
    assert!(cell_run(buffer, 6).contains("[ done ]"), "the status that landed with the event: {:?}", cell_run(buffer, 6));
    assert!(cell_run(buffer, 2).contains("100%"), "the bar counts what the pills show: {:?}", cell_run(buffer, 2));
    assert!(app.memories().alert().is_some(), "the completion alert is up beside those rows");

    // The selection stayed on the row the user scrolled to; the poll did not re-pin the tail.
    assert!(cell_run(buffer, 5).contains(&format!("❯ {}", &uuid(2)[..8])), "{:?}", cell_run(buffer, 5));
    assert!(!cell_run(buffer, 6).contains('❯'), "{:?}", cell_run(buffer, 6));

    // And the premise that makes the poll above the LAST one: the event loop stops ticking here.
    assert!(!app.memories().run_in_flight(), "the loop stops ticking, so no later tick can correct the table");
}

/// A run whose plan and completion are drained by ONE pump still renders its final statuses, and
/// adopts no selection the user never made.
///
/// The fast path a small export takes: everything happens inside one 80 ms tick, so the plan and the
/// finished event are in the channel together. The poll then runs for the first and only time after
/// `finish`, which is where the two halves below come from — the statuses must be real, and
/// `finish`'s clearing of `follow_tail` must hold, or the poll pins the tail on a table the user
/// never scrolled. This is the ONLY reachable state where that clearing is observable: every other
/// route to the transition tick has either already pinned the tail (follow on, selection at the tail
/// anyway) or had it cleared by `table_move`.
#[test]
fn a_run_that_plans_and_finishes_in_one_tick_renders_its_statuses_and_adopts_no_selection() {
    let dir = export_tree("one-tick", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let mut writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    writer
        .enroll(&[
            exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(1), url: None },
            exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(2), url: None },
        ])
        .unwrap();
    drop(writer);
    for (name, id) in [("20210115_133005.jpg", uuid(1)), ("20210115_133005_2.jpg", uuid(2))] {
        let output = dir.path().join("out/2021/01").join(name);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, b"fixed").unwrap();
        let writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
        writer.mark_done(ItemKind::Memory, &id, &output).unwrap();
    }

    // Both events are queued before the first tick, so one pump drains the pair.
    let (sender, receiver) = mpsc::channel();
    sender
        .send(RunEvent::Planned(PlanSnapshot {
            export_id: ExportId::new(EXPORT_ID).unwrap(),
            manifest_dir: state.path().to_path_buf(),
            rows: vec![
                PlanRow { source_id: uuid(1), output_name: "20210115_133005.jpg".to_owned(), place_name: None, leg: Leg::Image },
                PlanRow { source_id: uuid(2), output_name: "20210115_133005_2.jpg".to_owned(), place_name: None, leg: Leg::Image },
            ],
        }))
        .unwrap();
    sender.send(RunEvent::Finished(RunOutcome::Completed(one_fixed()))).unwrap();
    app.with_memories_channel(receiver);
    app.tick();

    let mut terminal = Terminal::new(TestBackend::new(160, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(cell_run(buffer, 2).contains("100%"), "{:?}", cell_run(buffer, 2));
    assert!(cell_run(buffer, 4).contains("[ done ]"), "{:?}", cell_run(buffer, 4));
    assert!(cell_run(buffer, 5).contains("[ done ]"), "{:?}", cell_run(buffer, 5));

    // The caret renders only in the focused pane, so descend before reading it. A run the user
    // watched from the form leaves the finished table with nothing selected.
    press(&mut app, KeyCode::Enter);
    assert!(app.memories().descended());
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    for y in [4, 5] {
        assert!(!cell_run(buffer, y).contains('❯'), "the finished table adopted a selection: {:?}", cell_run(buffer, y));
    }
}

/// One planned run with a live manifest behind it, and the sender for the run's later events.
///
/// The two pins below need the same live state and then break it in the same way at two different
/// moments, so the setup is shared and the divergence is the whole test. Both tempdirs are returned
/// because both must outlive the app.
fn planned_run_over_a_live_manifest(name: &str) -> (App, TempDir, TempDir, mpsc::Sender<RunEvent>) {
    let dir = export_tree(name, &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let mut writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    writer.enroll(&[exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(1), url: None }]).unwrap();
    drop(writer);
    let sender = feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: uuid(1), output_name: "20210115_133005.jpg".to_owned(), place_name: None, leg: Leg::Image }],
    );
    (app, dir, state, sender)
}

/// Replaces the manifest this screen polls with bytes sqlite will not open, so the next poll fails
/// for real rather than through an injected error.
fn corrupt_the_manifest(state: &TempDir) {
    let db = state.path().join(format!("{EXPORT_ID}.sqlite"));
    assert!(db.is_file(), "the fixture must corrupt a database that is there: {}", db.display());
    fs::write(&db, b"this is not a database, and sqlite must refuse to read it as one").unwrap();
}

/// A completion report the alert spells as `1 fixed`.
fn one_fixed() -> FixReport {
    FixReport {
        resumed: ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0, retired: 0, excluded: 0 },
        fixed: 1,
        failed: vec![],
        skipped: 0,
        deferred: 0,
        excluded: 0,
        notices: vec![],
    }
}

/// A manifest that goes unreadable MID-RUN ends the run and says so. The direction the pin below
/// would otherwise let a guard mutate away: a poll error that never reported anything at all would
/// satisfy that pin and leave a wedged run silent.
#[test]
fn a_manifest_that_goes_unreadable_mid_run_raises_its_own_failure() {
    let (mut app, _dir, state, _sender) = planned_run_over_a_live_manifest("poll-error-mid-run");
    assert!(app.memories().alert().is_none(), "the run is live and has reported nothing yet");

    corrupt_the_manifest(&state);
    app.tick();

    let alert = app.memories().alert().expect("a poll that cannot read the manifest ends the run");
    assert_eq!(alert.kind, AlertKind::Warning, "{}", alert.message);
    assert!(alert.message.contains("manifest"), "{}", alert.message);
    assert!(!app.memories().run_in_flight(), "the failed poll ends the run rather than wedging it");
}

/// The same failure on the FINISHING tick leaves the run's own verdict standing.
///
/// The last poll runs after `pump` has already published the worker's outcome, so its error arm is
/// the one path that can overwrite a verdict the run itself produced. The manifest's own message
/// tells the user to delete the file and redo the export, which over a run that completed cleanly is
/// destructive advice — 846 entries' worth on the observed export. The run's verdict wins; the
/// display fault is dropped.
///
/// **Ceiling**: the dropped error goes nowhere, because the screen has one alert slot and the crate
/// has no log to put the second fact in. Upgrade path is a second alert slot (or a status line) that
/// can carry a display fault alongside a run outcome.
#[test]
fn a_manifest_error_on_the_finishing_tick_leaves_the_runs_own_verdict_standing() {
    let (mut app, _dir, state, sender) = planned_run_over_a_live_manifest("poll-error-at-finish");

    // The real order: the worker's outcome is already queued when the manifest goes bad, exactly as
    // it would be for a run whose last commit landed and whose db was pulled out from under the
    // screen's reader before the next tick.
    corrupt_the_manifest(&state);
    sender.send(RunEvent::Finished(RunOutcome::Completed(one_fixed()))).unwrap();
    app.tick();

    let alert = app.memories().alert().expect("the completion alert");
    assert_eq!(alert.kind, AlertKind::Info, "the run succeeded, so the footer must not report a failure: {}", alert.message);
    assert!(alert.message.contains("1 fixed"), "{}", alert.message);
    assert!(!alert.message.contains("delete"), "a clean run must never be answered with delete-the-manifest advice: {}", alert.message);
}

#[test]
fn a_failed_run_raises_a_warning_alert_that_x_dismisses() {
    let dir = export_tree("alert-x", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let (sender, receiver) = mpsc::channel();
    sender.send(RunEvent::Finished(RunOutcome::Failed(RunError::NoMemoriesFile))).unwrap();
    app.with_memories_channel(receiver);
    app.tick();

    let alert = app.memories().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning);
    assert!(alert.message.contains("memories_history.json"), "{}", alert.message);

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row(buffer, 23).starts_with(" ! "), "{:?}", row(buffer, 23));

    // `x` dismisses; `x` with nothing live is inert.
    press(&mut app, KeyCode::Char('x'));
    assert!(app.memories().alert().is_none());
    assert!(app.is_running());
    press(&mut app, KeyCode::Char('x'));
    assert!(app.is_running(), "an inert x must not quit or move anything");
    assert_eq!(app.active(), exportsnap::app::Tab::Memories);
}

#[test]
fn x_dismisses_the_alert_even_when_the_quit_is_armed() {
    let dir = export_tree("alert-x-quit", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let (sender, receiver) = mpsc::channel();
    sender.send(RunEvent::Finished(RunOutcome::Failed(RunError::NoMemoriesFile))).unwrap();
    app.with_memories_channel(receiver);
    app.tick();

    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed());
    press(&mut app, KeyCode::Char('x'));
    assert!(app.memories().alert().is_none());
    assert!(app.is_running(), "the alert dismissal must not confirm the quit");
    assert!(app.is_quit_armed(), "x dismisses the alert and returns before the disarm, so the armed quit survives");
}

#[test]
fn entering_on_a_static_row_descends_and_q_ascends_without_arming_the_quit() {
    let dir = export_tree("descend", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), place_name: None, leg: Leg::Image }],
    );

    // Enter on the static source row descends.
    press(&mut app, KeyCode::Enter);
    assert!(app.memories().descended());

    // The descended hint set advertises every ascend key, esc included — never a dead binding.
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert!(row(terminal.backend().buffer(), 23).contains("esc back"), "{:?}", row(terminal.backend().buffer(), 23));

    // While descended, `←` ascends and `→` is inert.
    press(&mut app, KeyCode::Right);
    assert!(app.memories().descended(), "→ is inert while descended");
    press(&mut app, KeyCode::Left);
    assert!(!app.memories().descended(), "← ascends");

    // Descend again; `q` ascends like esc, because q is the back key here — and arms nothing.
    press(&mut app, KeyCode::Enter);
    assert!(app.memories().descended());
    press(&mut app, KeyCode::Char('q'));
    assert!(!app.memories().descended(), "q ascends while descended");
    assert!(!app.is_quit_armed(), "that q armed nothing");
    assert!(app.is_running());

    // And esc ascends too.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    assert!(!app.memories().descended());
}

#[test]
fn arrows_are_trapped_while_descended_but_the_alt_jump_still_lands() {
    let dir = export_tree("suspend", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), place_name: None, leg: Leg::Image }],
    );

    press(&mut app, KeyCode::Enter);
    assert!(app.memories().descended());
    press(&mut app, KeyCode::Left);
    assert_eq!(app.active(), exportsnap::app::Tab::Memories, "← ascends rather than switching tabs");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.active(), exportsnap::app::Tab::Memories, "→ is inert while descended");

    // The ⌥ jump ascends implicitly and lands.
    let jump = KeyEvent::new(KeyCode::Char('4'), KeyModifiers::ALT);
    app.handle_event(&Event::Key(jump));
    assert_eq!(app.active(), exportsnap::app::Tab::History);
    let _ = jump;
}

/// The delivery pin the toggle test cannot see: its flip assertions start from the default, so a
/// `with_environment` that ignored the resolved `transcode` and hardcoded `true` kept every
/// existing test green (measured on a full-suite mutation run, 2026-08-14). The resolved value is
/// the toggle's initial state, so composing with `false` must surface as `false` on the screen.
#[test]
fn the_resolved_transcode_is_the_toggle_initial_state() {
    let mut app = App::new(Tier::Full).with_source_environment(
        PathBuf::from("/export"),
        RunDefaults {
            out_root: PathBuf::from("/export/out"),
            transcode: false,
            ..RunDefaults::resolve(None, &Config::default(), Path::new("/export"))
        },
        Environment { ffmpeg: None, vlc: None, available_space: Some(3 * 1024 * 1024 * 1024), total_space: Some(5 * 1024 * 1024 * 1024) },
    );
    on_memories(&mut app);
    assert!(!app.memories().is_transcode_on(), "the resolved transcode=false must be the toggle's initial state");
}

#[test]
fn the_form_caret_walks_all_rows_and_enter_acts_on_toggle_and_start() {
    let dir = export_tree("form-keys", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    // Runs started from the screen keep their manifest here, so the production start path is
    // exercised end to end without touching the platform's per-user data dir.
    app.memories_mut().set_manifest_dir(state.path().to_path_buf());
    assert!(!app.memories().descended());

    // Down wraps to the start chip; enter there starts the run through the production path — a
    // real worker against the tempdir export, with the manifest parked beside it.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(app.memories().alert().is_none(), "a fresh run shows no alert");

    // The screen-driven run completes end to end: the worker fixes the memory, the poll sees
    // it, and the completion alert arrives through the real channel.
    wait_for_alert(&mut app);
    let alert = app.memories().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Info, "{}", alert.message);
    assert!(alert.message.contains("1 fixed"), "{}", alert.message);

    // Up to the toggle, enter flips it, space flips it back — both bindings on the one control.
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    assert!(!app.memories().is_transcode_on());
    press(&mut app, KeyCode::Char(' '));
    assert!(app.memories().is_transcode_on());
}

/// Ticks until an alert lands, bounded by WALL CLOCK rather than by an iteration count.
///
/// The distinction is not pedantry. A 500-iteration budget at 2 ms is not really denominated in
/// iterations: the sleep barely moves under contention, so the budget is a fixed deadline worth
/// 1.03 s unloaded, 1.16 s at 5x CPU oversubscription and 1.6 s starved on a single shared core.
/// The worker's completion time carries no such ceiling, and a full-suite run under load has put
/// these tests at 0.93 s against that ~1 s bound. So the old shape ran out exactly when the box
/// was slow enough to be worth waiting for, and it failed as "no alert arrived" — the assertion
/// under test — rather than as the timeout it actually was. The bound below stays generous because
/// its only job is to stop a hang, and a hang is what a bug here looks like.
///
/// **The deadline arm is deliberately unpinned, and that is a cost decision rather than an oversight.** Firing it needs a worker that never sends, which is 60 s of gate against a 3.5 s release suite (measured 2026-08-11), and parameterising the deadline so a test could pass a short one would move the untested boundary onto whoever supplies the real 60 s rather than remove it. The half that rots silently is the deadline being too SHORT, and that one is pinned below by `a_worker_slower_than_the_old_iteration_budget_still_lands_its_alert`. Termination is structural — the loop runs against a fixed `Instant` — so what is unpinned is the message alone. Do not read that as the deadline being decoration: without it a worker that never finishes wedges the suite, since this crate configures no nextest `terminate-after`.
fn wait_for_alert(app: &mut App) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        app.tick();
        if app.memories().alert().is_some() {
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
    let dir = export_tree("slow-worker", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    app.memories_mut().start_run_with(
        |_inputs, _sender| {
            std::thread::sleep(Duration::from_secs(3));
            panic!("a worker that outlives the old budget");
        },
        None,
    );
    // Vacuity guard: nothing has landed yet, so the wait below is what produces the alert.
    assert!(app.memories().alert().is_none());

    wait_for_alert(&mut app);
    let alert = app.memories().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning, "{}", alert.message);
}

#[test]
fn a_worker_that_exits_after_finished_does_not_overwrite_the_outcome() {
    let dir = export_tree("worker-exit", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
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
    // The worker has exited: its sender is gone, so the channel reads Disconnected once the
    // Finished event is drained. That must read as "the run is over", never as a panic — the
    // regression this pins replaced every completion alert with the panic alert on real runs.
    drop(sender);
    app.with_memories_channel(receiver);
    app.tick();

    let alert = app.memories().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Info, "the true outcome must survive the dead channel");
    assert!(alert.message.contains("1 fixed"), "{}", alert.message);

    // A second tick must not flip it either.
    app.tick();
    assert_eq!(app.memories().alert().unwrap().kind, AlertKind::Info);
}

#[test]
fn a_channel_that_goes_dead_without_a_finished_event_reports_a_panic() {
    let dir = export_tree("dead-channel", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let (sender, receiver) = mpsc::channel();
    // The worker died without ever sending — not even its panic arm ran. The dead channel is
    // the one way the screen can still learn about it, and it must report a panic rather than
    // spin forever.
    drop(sender);
    app.with_memories_channel(receiver);
    app.tick();

    let alert = app.memories().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning);
    assert!(alert.message.contains("unexpectedly"), "{}", alert.message);
}

#[test]
fn a_worker_that_panics_still_yields_a_panic_alert_and_no_stuck_spinner() {
    let dir = export_tree("panic-worker", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    app.memories_mut().start_run_with(|_inputs, _sender| panic!("boom"), None);

    // The panic is contained worker-side and reported as a Finished event; the screen must land
    // on the panic alert rather than spinning forever.
    wait_for_alert(&mut app);
    let alert = app.memories().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning);
    assert!(alert.message.contains("unexpectedly"), "{}", alert.message);

    // No stuck spinner: the progress panel shows the empty state, not the plan phase. The frame
    // is centered in the full-height panel, so the hint is its first interior row.
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let progress = cell_run(buffer, 11);
    assert!(progress.contains("no run yet"), "{progress}");
    assert!(!progress.contains('\u{280b}'), "{progress}");
}

#[test]
fn a_worker_that_panics_after_planning_keeps_the_table_and_reports_the_panic() {
    let dir = export_tree("panic-after-plan", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let manifest_dir = state.path().to_path_buf();
    app.memories_mut().start_run_with(
        move |_inputs, sender| {
            let _ = sender.send(RunEvent::Planned(PlanSnapshot {
                export_id: ExportId::new(EXPORT_ID).unwrap(),
                manifest_dir: manifest_dir.clone(),
                rows: vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), place_name: None, leg: Leg::Image }],
            }));
            panic!("boom");
        },
        None,
    );
    wait_for_alert(&mut app);
    let alert = app.memories().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning);
    assert!(alert.message.contains("unexpectedly"), "{}", alert.message);

    // The planned table is still there — the panic alert replaces the spinner, not the rows.
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(cell_run(buffer, 4).contains("[ pending ]"), "{:?}", cell_run(buffer, 4));
}

#[test]
fn the_focused_form_row_tint_reaches_the_padding_boundary() {
    let dir = export_tree("tint", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    // At 50 wide the panels stack and the form takes the full width, so its interior (46
    // cells) is wider than the row's own content (36) — exactly the case where a tint that
    // stops at the last span would show a gap before the padding boundary.
    let mut terminal = Terminal::new(TestBackend::new(50, 30)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // The interior runs from column 2 to 47 (border + padding on each side of the 50-cell
    // panel). The focused (first) form row's tint must reach column 47 — the padding boundary.
    for x in 2..48 {
        assert_eq!(buffer[(x, 2)].style().bg, Some(palette.bg_hover), "focused row column {x}");
    }
    // The padding columns stay on the base surface, and the unfocused row carries no tint.
    assert_ne!(buffer[(1, 2)].style().bg, Some(palette.bg_hover));
    assert_ne!(buffer[(48, 2)].style().bg, Some(palette.bg_hover));
    assert_ne!(buffer[(2, 3)].style().bg, Some(palette.bg_hover));
}

#[test]
fn the_form_path_rows_grow_in_the_full_width_arm_and_keep_the_narrow_floor() {
    let source = PathBuf::from(format!("/{}", "s".repeat(40)));
    let mut app = App::new(Tier::Full).with_source_environment(
        source.clone(),
        RunDefaults { out_root: source.join("out"), ..RunDefaults::resolve(None, &Config::default(), &source) },
        Environment { ffmpeg: None, vlc: None, available_space: Some(3 * 1024 * 1024 * 1024), total_space: Some(5 * 1024 * 1024 * 1024) },
    );
    on_memories(&mut app);
    let whole = source.to_string_lossy().into_owned();

    // Side by side (120 wide): the form panel is at its 36-cell interior, so the source row still
    // head-ellipsises to the narrow value column.
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let narrow = cell_run(terminal.backend().buffer(), 2);
    assert!(narrow.contains('…'), "the side-by-side source row still truncates: {narrow}");
    assert!(!narrow.contains(&whole), "the side-by-side source row does not show the whole path: {narrow}");

    // Stacked (80 wide): the form takes the full width, so the source row shows the whole path.
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let wide = cell_run(terminal.backend().buffer(), 2);
    assert!(wide.contains(&whole), "the full-width source row shows the whole path: {wide}");
    assert!(!wide.contains('…'), "the full-width source row does not ellipsise: {wide}");
}

#[test]
fn the_side_by_side_form_grows_to_fit_the_paths_and_keeps_the_table_floor() {
    // A 33-cell source and a 46-cell output dir: at 130 wide the side-by-side form grows from its
    // 40-cell floor to the 54-cell cap, so the source shows whole, the output head-ellipsises within
    // the capped interior, and the progress table keeps its 76-cell floor.
    let source = PathBuf::from(format!("/{}", "s".repeat(32)));
    let out_root = PathBuf::from(format!("/{}", "o".repeat(45)));
    let mut app = App::new(Tier::Full).with_source_environment(
        source.clone(),
        RunDefaults { out_root, ..RunDefaults::resolve(None, &Config::default(), &source) },
        Environment { ffmpeg: None, vlc: None, available_space: Some(3 * 1024 * 1024 * 1024), total_space: Some(5 * 1024 * 1024 * 1024) },
    );
    on_memories(&mut app);
    let whole = source.to_string_lossy().into_owned();

    let mut terminal = Terminal::new(TestBackend::new(130, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let source_row = cell_run(buffer, 2);
    assert!(source_row.contains(&whole), "the source shows whole: {source_row}");
    assert!(!source_row.contains('…'), "the source must not ellipsise: {source_row}");
    let output_row = cell_run(buffer, 3);
    assert!(output_row.contains('…'), "the output head-ellipsises within the cap: {output_row}");
    assert!(screen_text(buffer).contains("no run yet"), "the progress panel keeps its 76-cell floor");
}

#[test]
fn the_empty_state_action_line_names_a_key_that_actually_starts_the_run() {
    let dir = export_tree("empty-action", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    app.memories_mut().set_manifest_dir(state.path().to_path_buf());

    // The empty state says "press ↵ to start" — and with the caret on the first static row,
    // enter really does start the run (there is no table yet to descend into). The frame is
    // centered in the full-height panel, so the action line is its last interior row.
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert!(cell_run(terminal.backend().buffer(), 12).contains("press ↵ to start"), "{:?}", cell_run(terminal.backend().buffer(), 12));

    press(&mut app, KeyCode::Enter);
    wait_for_alert(&mut app);
    let alert = app.memories().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Info, "{}", alert.message);
    assert!(alert.message.contains("1 fixed"), "{}", alert.message);
}

#[test]
fn enter_on_a_static_row_descends_only_when_a_table_exists() {
    let dir = export_tree("static-enter", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    app.memories_mut().set_manifest_dir(state.path().to_path_buf());

    // No table yet: enter on Source starts the run rather than descending into nothing.
    press(&mut app, KeyCode::Enter);
    assert!(!app.memories().descended());
    assert!(app.memories().run_in_flight());

    // Wait for the plan so the table exists; a fresh run's enter then descends. The alert is what
    // the wait above is for, so check it landed rather than leaning on it silently — a helper that
    // gave up would otherwise leave `descended()` true off the plan alone and read as a pass.
    wait_for_alert(&mut app);
    assert!(app.memories().alert().is_some(), "the wait above must have produced the alert it waited for");
    press(&mut app, KeyCode::Enter);
    assert!(app.memories().descended(), "with a table live, enter on a static row descends");
}

#[test]
fn the_selected_form_row_keeps_its_tint_while_the_table_is_descended() {
    let dir = export_tree("blur-tint", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let mut writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    writer.enroll(&[exportsnap::export::manifest::NewItem { kind: ItemKind::Memory, source_id: &uuid(1), url: None }]).unwrap();
    drop(writer);
    let sender = feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), place_name: None, leg: Leg::Image }],
    );
    let _ = sender;
    let palette = Palette::new(Tier::Full);

    // The form owns the caret: the selected first row is tinted AND carries the caret.
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(2, 2)].style().bg, Some(palette.bg_hover));
    assert_eq!(buffer[(2, 2)].symbol(), "❯");

    // Descend: the form goes blurred — the caret drops, the tint stays (contract: blurred panes
    // preserve their last-selected row's tint).
    press(&mut app, KeyCode::Enter);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(2, 2)].style().bg, Some(palette.bg_hover), "the tint survives the blur");
    assert_ne!(buffer[(2, 2)].symbol(), "❯", "the caret drops when the pane blurs");
}

#[test]
fn the_disk_free_row_fits_its_wide_value_at_the_form_width_floor() {
    let dir = export_tree("disk-wide", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    // 1024 GiB free of 2048 GiB: "1024.0 GiB" + bar + " 50%" is the widest the row gets, and
    // it must fit the 40-cell side-by-side panel without clipping the trailing percent.
    // 1024 GiB prints as "1.0 TiB", so the WIDEST value is the one just under a unit boundary:
    // 511.9 GiB free of 1024.0 GiB reads "511.9 GiB" — 50% used, one decimal, a 9-cell value.
    let mut app = App::new(Tier::Full).with_source_environment(
        dir.path().to_path_buf(),
        RunDefaults { out_root: dir.path().join("out"), ..RunDefaults::resolve(None, &Config::default(), dir.path()) },
        Environment {
            ffmpeg: None,
            vlc: None,
            available_space: Some(511 * 1024 * 1024 * 1024 + 944 * 1024 * 1024),
            total_space: Some(1024 * 1024 * 1024 * 1024),
        },
    );
    on_memories(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let disk_row = cell_run(terminal.backend().buffer(), 4);
    assert!(disk_row.contains("50%"), "{disk_row}");
    assert!(disk_row.contains("511.9 GiB"), "{disk_row}");
    // The whole row survives at the pinned side-by-side width: the percent renders whole with
    // at least one cell of slack before the setup panel's border — a clipped "%" would sit
    // flush against it.
    let panel_gap = disk_row.split("│").nth(1).unwrap().trim_end();
    assert!(panel_gap.ends_with("50%"), "{disk_row}");
}

#[test]
fn the_completion_summary_hides_zero_counts() {
    let dir = export_tree("summary-zeros", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let (sender, receiver) = mpsc::channel();
    let report = FixReport {
        resumed: ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0, retired: 0, excluded: 0 },
        fixed: 0,
        failed: vec![],
        skipped: 3,
        deferred: 0,
        excluded: 0,
        notices: vec![],
    };
    sender.send(RunEvent::Finished(RunOutcome::Completed(report))).unwrap();
    drop(sender);
    app.with_memories_channel(receiver);
    app.tick();

    let alert = app.memories().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Info, "{}", alert.message);
    assert!(alert.message.contains("3 skipped"), "{}", alert.message);
    assert!(!alert.message.contains("0 fixed"), "a zero count must be hidden: {}", alert.message);
    assert!(!alert.message.contains("0 skipped"), "{}", alert.message);
}

#[test]
fn the_output_column_middle_ellipsises_keeping_both_ends() {
    let dir = export_tree("ext-keep", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    let sender = feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: uuid(1), output_name: "20210115_133005_2.jpg".to_owned(), place_name: None, leg: Leg::Image }],
    );
    let _ = sender;
    // A narrow table (stacked arm) forces the output column small enough to truncate. 80 is the
    // narrowest width that still renders the table at all (its interior floor is 72 cells with the
    // location column), and there the output column is 10 cells — the name has to truncate.
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    // At 80 wide the panels stack: form on top, table below. Find the row holding the id.
    let row_text =
        (0..30).map(|y| cell_run(buffer, y)).find(|line| line.contains(&uuid(1)[..8])).unwrap_or_else(|| panic!("no table row rendered"));
    let content = row_text.trim_end_matches('│').trim_end();
    // Middle-ellipsis cuts the middle, so both ends survive: the date prefix and the extension.
    assert!(content.ends_with("2021…2.jpg"), "both ends of the output name must survive: {row_text}");
}

#[test]
fn a_wide_progress_table_grows_the_identity_and_location_columns() {
    let mut app = app_on_fixed_source();
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    // A 36-char uuid and a 42-char place name: both fit their grown columns whole once the panel
    // is wide enough that output (here a 5-cell name) is whole and identity and location then
    // reach their content lengths.
    let place = "x".repeat(42);
    feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), place_name: Some(place.clone()), leg: Leg::Image }],
    );
    let mut terminal = Terminal::new(TestBackend::new(200, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let row_text =
        (0..24).map(|y| cell_run(buffer, y)).find(|line| line.contains(&uuid(1))).unwrap_or_else(|| panic!("no table row rendered"));
    assert!(row_text.contains(&uuid(1)), "the full 36-char uuid renders whole: {row_text}");
    assert!(row_text.contains(&place), "the 42-char place name renders whole: {row_text}");
    assert!(!row_text.contains('…'), "nothing ellipsises in the grown columns: {row_text}");
}

#[test]
fn the_narrow_progress_table_keeps_the_identity_floor_ellipsised() {
    let mut app = app_on_fixed_source();
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), place_name: None, leg: Leg::Image }],
    );
    // 76 wide: the stacked table's interior is its 72-cell floor, where the identity column keeps
    // its 18-cell floor and the uuid must still middle-ellipsise.
    let mut terminal = Terminal::new(TestBackend::new(76, 30)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let row_text =
        (0..30).map(|y| cell_run(buffer, y)).find(|line| line.contains(&uuid(1)[..8])).unwrap_or_else(|| panic!("no table row rendered"));
    assert!(row_text.contains('…'), "the 18-cell identity floor still ellipsises: {row_text}");
}

#[test]
fn the_stacked_progress_table_shows_the_full_output_name_at_110_wide() {
    // The user's 110x32 capture: the panels stack, the table interior is 106 cells, and the output
    // filename must render whole — its date prefix is the metadata this app restores, so it takes
    // its full width before identity or location grow. A long place name makes the old
    // identity-then-location order eat the surplus and squeeze output, so the pin is not vacuous.
    let mut app = app_on_fixed_source();
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow {
            source_id: uuid(1),
            output_name: "20240114_103000.mp4".to_owned(),
            place_name: Some(LONG_PLACE.to_owned()),
            leg: Leg::Image,
        }],
    );
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let row_text =
        (0..32).map(|y| cell_run(buffer, y)).find(|line| line.contains(&uuid(1))).unwrap_or_else(|| panic!("no table row rendered"));
    // The output span — from the filename to the row's end — carries no ellipsis; the location
    // column above it legitimately ellipsizes the long place name, so assert the span, not the row.
    let start = row_text.find("20240114_103000.mp4").unwrap_or_else(|| panic!("the full output name did not render: {row_text}"));
    assert!(!row_text[start..].contains('…'), "the output name ellipsised: {row_text}");
}

/// **Reach pin: the place name renders in the LOCATION column, middle-ellipsised.** A 50-char
/// name cuts to head + ellipsis + tail in the 29-cell column, a short one renders whole, and an
/// absent one renders as a blank column that still holds its width — the pill lands on the same
/// column whatever the name does.
#[test]
fn the_location_column_middle_ellipsises_long_place_names_and_holds_short_ones_whole() {
    let mut app = app_on_fixed_source();
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    feed_plan(
        &mut app,
        state.path(),
        vec![
            PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), place_name: Some(LONG_PLACE.to_owned()), leg: Leg::Image },
            PlanRow { source_id: uuid(2), output_name: "y.jpg".to_owned(), place_name: Some("Dresden".to_owned()), leg: Leg::Image },
            PlanRow { source_id: uuid(3), output_name: "z.jpg".to_owned(), place_name: None, leg: Leg::Image },
        ],
    );

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();

    // 29 cells is the observed range's lower bound: the 50-char name cuts to 14 + ellipsis + 14.
    let first = cell_run(buffer, 4);
    assert!(first.contains("Wurstelstand a…menade, Vienna"), "the ellipsised place name: {first}");
    assert!(!first.contains(LONG_PLACE), "the name must not render whole at this width: {first}");
    let second = cell_run(buffer, 5);
    // The row's identity cell always carries an ellipsis of its own, so the whole-row check would
    // be blind to the question: read only the slice between the name and the status pill.
    let short_name_start = second.find("Dresden").unwrap();
    let short_name_end = second.find("[ pending ]").unwrap();
    assert_eq!(second[short_name_start..short_name_end].trim(), "Dresden", "a short place name renders whole: {second}");
    assert!(!second[short_name_start..short_name_end].contains('…'), "nothing about a short name may ellipsise: {second}");
    let third = cell_run(buffer, 6);
    assert!(third.contains(&uuid(3)[..8]), "the nameless row is still a row: {third}");
    // The column holds its width whatever the name: the pill lands on the same column in all
    // three rows — the blank is a blank COLUMN, not a shrunken one. The column is a CELL count,
    // so the byte index `find` yields is the wrong measure: the form panel's left half carries
    // different multi-byte glyphs per row (the disk bar, the cycle chip), which shift the bytes
    // while the cells hold still.
    let pill_column = |line: &str| {
        let byte = line.find("[ pending ]").unwrap();
        line[..byte].chars().count()
    };
    assert_eq!(pill_column(&first), pill_column(&second), "the status pill must not shift between rows:\n{first}\n{second}");
    assert_eq!(pill_column(&second), pill_column(&third), "a blank location must hold its width:\n{second}\n{third}");
}

/// **Reach pin: the focused row's place name grows a tooltip right below it, with the FULL
/// name, only while the pane is descended.** A nameless row grows none, and the tooltip is an
/// item of its own — it never takes the caret or the highlight, and the rows below it shift.
#[test]
fn the_focused_rows_place_name_grows_a_tooltip_below_it_only_while_descended() {
    let mut app = app_on_fixed_source();
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    feed_plan(
        &mut app,
        state.path(),
        vec![
            PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), place_name: Some(LONG_PLACE.to_owned()), leg: Leg::Image },
            PlanRow { source_id: uuid(2), output_name: "y.jpg".to_owned(), place_name: None, leg: Leg::Image },
        ],
    );

    // The form owns the caret: no tooltip anywhere, on either row.
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    for y in 4..=8 {
        assert!(!cell_run(buffer, y).contains('└'), "a tooltip rendered while the form owned the caret: {:?}", cell_run(buffer, y));
    }

    // Descend: the selection follows the tail, which is the row WITHOUT a name — still no tooltip.
    press(&mut app, KeyCode::Enter);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    for y in 4..=8 {
        assert!(!cell_run(buffer, y).contains('└'), "a nameless row grew a tooltip: {:?}", cell_run(buffer, y));
    }

    // Up onto the named row: the full, un-ellipsised name lands as its own row right below it,
    // and the row below the tooltip is the shifted second row.
    press(&mut app, KeyCode::Up);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(cell_run(buffer, 4).contains("❯ 00000001"), "{:?}", cell_run(buffer, 4));
    let tooltip = cell_run(buffer, 5);
    assert!(tooltip.contains("└ "), "{tooltip}");
    assert!(tooltip.contains(LONG_PLACE), "the tooltip shows the FULL place name: {tooltip}");
    assert!(!tooltip.contains('…'), "the tooltip never ellipsises the name: {tooltip}");
    assert!(!tooltip.contains('❯'), "the tooltip must not carry the selection caret: {tooltip}");
    assert_ne!(buffer[(2, 5)].style().bg, Some(Palette::new(Tier::Full).bg_hover), "the tooltip must not take the highlight");
    assert!(cell_run(buffer, 6).contains(&uuid(2)[..8]), "the row below the tooltip is the shifted second row: {:?}", cell_run(buffer, 6));

    // Ascend with the selection still on the named row: the tooltip's gate is the pane's focus,
    // so it must vanish even though the row that grew it is still selected. Whole-frame sweep,
    // because the gate's absence would drop the tooltip below the row, not above the form.
    press(&mut app, KeyCode::Esc);
    assert!(!app.memories().descended(), "esc must ascend from the table");
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(!screen_text(buffer).contains('└'), "a tooltip rendered while the form owned the caret and a named row was selected");

    // Re-descend: the selection survived the ascend, so the tooltip returns — the phase above
    // measured the focus gate, not the selection moving.
    press(&mut app, KeyCode::Enter);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(cell_run(buffer, 5).contains(LONG_PLACE), "the tooltip must return on re-descend: {:?}", cell_run(buffer, 5));
}

/// **The privacy pin for decision 76.** A distinctive place-name token rides an entry's
/// `Location` string through a REAL run, and one item is doomed to fail so the run produces a
/// genuine error message and a manifest row with `last_error` set. The token must appear exactly
/// where it is supposed to — the two location cells — and nowhere else: not in the identity or
/// output columns, not in the alert, not in any path the run wrote, and not in any manifest
/// field.
#[test]
fn a_place_name_token_reaches_no_output_path_error_or_manifest_field() {
    const TOKEN: &str = "zqxhiddengullyzqx";
    // Distinct days, so each bucket holds one entry and one media file and the pairing is Exact:
    // an ambiguous bucket would make the capture fall to the file's day at midnight and the
    // occupied path below would never be the first item's write target.
    let dir =
        export_tree("place-privacy", &[(&at("2021-01-15", "13:30:05"), "Image", TOKEN), (&at("2021-02-20", "01:00:00"), "Image", TOKEN)]);
    // The first item's output path is pre-occupied by a directory, so its write fails
    // deterministically and the run really produces an error message and a `last_error` row.
    let out = dir.path().join("out");
    fs::create_dir_all(out.join("2021/01")).unwrap();
    fs::create_dir(out.join("2021/01/20210115_133005.jpg")).unwrap();
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    app.memories_mut().set_manifest_dir(state.path().to_path_buf());

    press(&mut app, KeyCode::Enter);
    wait_for_alert(&mut app);
    let alert = app.memories().alert().unwrap();
    let alert_kind = alert.kind;
    let alert_message = alert.message.clone();
    assert_eq!(alert_kind, AlertKind::Warning, "the occupied path must fail one item: {alert_message}");
    assert!(alert_message.contains("1 failed"), "the failing item must be the one the alert counts: {alert_message}");

    // The control, and it is what makes the sweeps below mean something: the token really is in
    // play — it rendered in exactly the two location cells and nowhere else on the whole screen.
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let text = screen_text(terminal.backend().buffer());
    assert_eq!(text.matches(TOKEN).count(), 2, "the token must reach the two location cells and nowhere else:\n{text}");

    // The good item really was written, so the tree scan below has a tree to scan.
    assert!(dir.path().join("out/2021/02/20210220_010000.jpg").is_file());
    let mut walk: Vec<_> = fs::read_dir(out).unwrap().flat_map(Result::ok).collect();
    while let Some(entry) = walk.pop() {
        assert!(!entry.file_name().to_string_lossy().contains(TOKEN), "a place name named a file or dir: {:?}", entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            walk.extend(fs::read_dir(entry.path()).unwrap().flat_map(Result::ok));
        }
    }

    // The manifest, `last_error` included: the failed row really carries one, so the sweep is live.
    let manifest = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    let rows = manifest.items(ItemKind::Memory).unwrap();
    assert!(rows.iter().any(|row| row.last_error.is_some()), "the failure must reach a manifest row's last_error");
    for item in rows {
        assert!(!item.source_id.contains(TOKEN), "{:?}", item.source_id);
        assert!(!item.last_error.as_deref().unwrap_or("").contains(TOKEN), "{:?}", item.last_error);
        if let Some(path) = &item.output_path {
            assert!(!path.to_string_lossy().contains(TOKEN), "a place name reached an output path: {path:?}");
        }
    }

    // The alert message itself, the one error text the run produced.
    assert!(!alert_message.contains(TOKEN), "a place name reached the alert: {alert_message}");
}

#[test]
fn a_zero_total_bar_shows_a_dash_not_an_ellipsis() {
    // The source is fixed so the row the bar shares with the form can never carry the
    // head-ellipsis a long tempdir path would introduce — that `…` is not the bar's, and the
    // assertion below is about the bar alone.
    let mut app = app_on_fixed_source();
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    let sender = feed_plan(&mut app, state.path(), vec![]);
    let _ = sender;
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let bar = cell_run(terminal.backend().buffer(), 2);
    assert!(bar.contains('—'), "a determinate bar with no items shows the no-value dash: {bar}");
    assert!(!bar.contains('…'), "the ellipsis is the indeterminate tell: {bar}");
}

#[test]
fn every_tab_renders_with_the_memories_screen_at_degenerate_sizes() {
    let sizes = [(0, 0), (1, 1), (4, 4), (16, 3), (17, 2), (255, 1), (1, 255), (500, 3)];
    for (width, height) in sizes {
        let mut app = App::new(Tier::Full).with_source_environment(
            std::path::PathBuf::from("/nope"),
            RunDefaults {
                out_root: std::path::PathBuf::from("/nope/out"),
                ..RunDefaults::resolve(None, &Config::default(), std::path::Path::new("/nope"))
            },
            Environment::default(),
        );
        on_memories(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap_or_else(|error| panic!("at {width}x{height}: {error}"));
    }
}
