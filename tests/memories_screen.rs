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

use exportsnap::app::App;
use exportsnap::export::env::Environment;
use exportsnap::export::local_fix::{FixReport, Leg, VideoOptions};
use exportsnap::export::manifest::{ExportId, ItemKind, Manifest, ResumeReport};
use exportsnap::export::memories_run::{self, PlanRow, PlanSnapshot, RunError, RunEvent, RunOutcome};
use exportsnap::tui::screens::memories::AlertKind;
use exportsnap::tui::shell;
use exportsnap::tui::theme::{Palette, Tier};
use image::RgbImage;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

const EXPORT_ID: &str = "1784667002819";

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

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

fn on_memories(app: &mut App) {
    while app.active() != exportsnap::app::Tab::Memories {
        press(app, KeyCode::Right);
    }
}

fn row(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect()
}

fn cell_run(buffer: &Buffer, y: u16) -> String {
    row(buffer, y).trim_end().to_owned()
}

/// An app on the memories tab with a real (tempdir) export tree behind it and a disk-probe
/// environment handed in, so the form rows are deterministic.
fn app_on_memories(dir: &TempDir) -> App {
    let mut app = App::new(Tier::Full).with_memories_environment(
        dir.path().to_path_buf(),
        Some(dir.path().join("out")),
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
    let mut app = App::new(Tier::Full).with_memories_environment(
        PathBuf::from("/export"),
        Some(PathBuf::from("/export/out")),
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
    // The empty state names the key that starts the run.
    assert!(cell_run(buffer, 11).contains("no run yet"), "{:?}", cell_run(buffer, 11));
    assert!(cell_run(buffer, 12).contains("↵"), "{:?}", cell_run(buffer, 12));
    // The footer hint bar advertises the shell keys.
    assert!(row(buffer, 23).contains("←→ switch"), "{:?}", row(buffer, 23));
    assert_eq!(app.active(), exportsnap::app::Tab::Memories);
    assert!(!app.is_quit_armed());
    assert!(!app.memories().descended());
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
            PlanRow { source_id: uuid(1), output_name: "20210115_133005.jpg".to_owned(), leg: Leg::Image },
            PlanRow { source_id: uuid(2), output_name: "20210115_133005_2.jpg".to_owned(), leg: Leg::Image },
            PlanRow { source_id: uuid(3), output_name: "20210115_133005_3.jpg".to_owned(), leg: Leg::Image },
        ],
    );

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    // The overall bar reads 0% with three rows pending (whole percentage).
    assert!(cell_run(buffer, 2).contains("0%"), "{:?}", cell_run(buffer, 2));
    // The header names the three columns.
    assert!(cell_run(buffer, 3).contains("IDENTITY"), "{:?}", cell_run(buffer, 3));
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
        resumed: exportsnap::export::manifest::ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0 },
        fixed: 1,
        failed: vec![],
        skipped: 2,
        deferred: 0,
        notices: vec![],
    };
    sender.send(RunEvent::Finished(RunOutcome::Completed(report))).unwrap();
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row(buffer, 23).contains(" i run finished · 1 fixed · 2 skipped"), "{:?}", row(buffer, 23));
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
    feed_plan(&mut app, state.path(), vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), leg: Leg::Image }]);

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
    feed_plan(&mut app, state.path(), vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), leg: Leg::Image }]);

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

/// Ticks until an alert lands, bounded — a worker's Finished event arrives asynchronously.
fn wait_for_alert(app: &mut App) {
    for _ in 0..500 {
        app.tick();
        if app.memories().alert().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("no alert arrived within the wait");
}

#[test]
fn a_worker_that_exits_after_finished_does_not_overwrite_the_outcome() {
    let dir = export_tree("worker-exit", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let (sender, receiver) = mpsc::channel();
    let report = FixReport {
        resumed: ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0 },
        fixed: 1,
        failed: vec![],
        skipped: 0,
        deferred: 0,
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

    // No stuck spinner: the progress panel shows the empty state, not the plan phase.
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
                rows: vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), leg: Leg::Image }],
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
fn the_empty_state_action_line_names_a_key_that_actually_starts_the_run() {
    let dir = export_tree("empty-action", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    app.memories_mut().set_manifest_dir(state.path().to_path_buf());

    // The empty state says "press ↵ to start" — and with the caret on the first static row,
    // enter really does start the run (there is no table yet to descend into).
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

    // Wait for the plan so the table exists; a fresh run's enter then descends.
    wait_for_alert(&mut app);
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
    let sender = feed_plan(&mut app, state.path(), vec![PlanRow { source_id: uuid(1), output_name: "x.jpg".to_owned(), leg: Leg::Image }]);
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
    let mut app = App::new(Tier::Full).with_memories_environment(
        dir.path().to_path_buf(),
        Some(dir.path().join("out")),
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
        resumed: ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0 },
        fixed: 0,
        failed: vec![],
        skipped: 3,
        deferred: 0,
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
fn the_output_column_keeps_the_extension_under_head_ellipsis() {
    let dir = export_tree("ext-keep", &[(&at("2021-01-15", "13:30:05"), "Image", "")]);
    let mut app = app_on_memories(&dir);
    let state = TempDir::new().unwrap();
    let _writer = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    let sender = feed_plan(
        &mut app,
        state.path(),
        vec![PlanRow { source_id: uuid(1), output_name: "20210115_133005_2.jpg".to_owned(), leg: Leg::Image }],
    );
    let _ = sender;
    // A narrow table (stacked arm) forces the output column small enough to truncate.
    let mut terminal = Terminal::new(TestBackend::new(50, 30)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    // At 50 wide the panels stack: form on top, table below. Find the row holding the id.
    let row_text =
        (0..30).map(|y| cell_run(buffer, y)).find(|line| line.contains(&uuid(1)[..8])).unwrap_or_else(|| panic!("no table row rendered"));
    let content = row_text.trim_end_matches('│').trim_end();
    assert!(content.ends_with(".jpg"), "the extension must survive the cut: {row_text}");
    assert!(content.contains('…'), "the name must actually be truncated at this width: {row_text}");
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
        let mut app = App::new(Tier::Full).with_memories_environment(
            std::path::PathBuf::from("/nope"),
            Some(std::path::PathBuf::from("/nope/out")),
            Environment::default(),
        );
        on_memories(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| shell::render(frame, &mut app)).unwrap_or_else(|error| panic!("at {width}x{height}: {error}"));
    }
}
