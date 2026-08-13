//! Public-API tests for the history screen: the conversation picker, the formats pane, the
//! selection-counted export chip, the refusal of an empty selection, the run counter and the
//! completion footer alert.
//!
//! Nothing here reads a real export: every export tree is synthesized in a tempdir (a
//! `mydata~<id>/json/` with a minimal `chat_history.json`), and every run drives the worker seam
//! or a real worker with the manifest parked in a tempdir, so the per-user data dir is never
//! touched.
//!
//! Render expectations are cross-checked against the cloudy-tui skill's Panel, Checkbox row,
//! Action chip, Tooltip, List / table, Footer alert and Pane focus sections, not against this
//! crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use exportsnap::app::{App, Tab};
use exportsnap::export::env::Environment;
use exportsnap::export::history::HtmlLinks;
use exportsnap::export::history_run::{HistoryFormat, HistoryReport, PlanSnapshot, RunEvent, RunOutcome};
use exportsnap::tui::alert::{AlertKind, RunAlert};
use exportsnap::tui::shell;
use exportsnap::tui::theme::{Palette, Tier};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

const EXPORT_ID: &str = "1784667002819";

// ---- fixtures ----

/// One chat message: the fixture's own sender, a created instant, and the optional conversation
/// title the picker's label rule reads (decision 64). Everything else stays at the loader's
/// default, exactly like the merge tests' entries.
fn chat_entry(created: &str, title: Option<&str>) -> String {
    let title_field = match title {
        Some(title) => format!(r#","Conversation Title":"{title}""#),
        None => String::new(),
    };
    format!(r#"{{"From":"fixture-sender","Created":"{created}"{title_field}}}"#)
}

/// `chat_history.json` in the exact spelling the loader reads: one array per conversation key.
fn write_chat_history(json_dir: &Path, conversations: &[(&str, Vec<String>)]) {
    fs::create_dir_all(json_dir).unwrap();
    let threads: Vec<String> = conversations.iter().map(|(key, entries)| format!(r#""{key}":[{}]"#, entries.join(","))).collect();
    fs::write(json_dir.join("chat_history.json"), format!("{{{}}}", threads.join(","))).unwrap();
}

/// One delivery: part 1 unpacked with its `json/`.
fn export_tree(conversations: &[(&str, Vec<String>)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    write_chat_history(&dir.path().join(format!("mydata~{EXPORT_ID}/json")), conversations);
    dir
}

/// The fixture's two-conversation export: one titled, one key-only — the pair the picker's label
/// rule must tell apart.
fn two_threads() -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("alice", vec![chat_entry("2021-03-04 09:00:00 UTC", Some("Alice's Thread"))]),
        ("bob", vec![chat_entry("2021-03-04 10:00:00 UTC", None)]),
    ]
}

// ---- drivers ----

fn press(app: &mut App, code: KeyCode) {
    press_with(app, code, KeyModifiers::NONE);
}

fn press_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_event(&Event::Key(KeyEvent::new(code, modifiers)));
}

/// The picker pane's interior cells of row `y` — the panel's padding and border sit at columns 0-1
/// and its right border at `PICKER_PANEL_WIDTH - 1`, so the full-width row also carries the
/// formats pane's cells.
fn picker_row(buffer: &Buffer, y: u16) -> String {
    (2..PICKER_PANEL_WIDTH - 1).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned()
}

/// The formats pane's interior cells of row `y`: its border and padding sit at
/// `PICKER_PANEL_WIDTH..=PICKER_PANEL_WIDTH + 1`, and its right border at the pane's edge.
fn formats_row(buffer: &Buffer, y: u16) -> String {
    (PICKER_PANEL_WIDTH + 2..buffer.area.width - 1).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned()
}

/// The picker pane's width at the 120x24 render these tests draw — the two panels sit side by
/// side, the picker taking its fixed budget and the formats pane the rest.
const PICKER_PANEL_WIDTH: u16 = 34;

/// Walks to the history tab with `→`, bounded for the reason `tests/shell.rs`'s `on_tab_in`
/// spells out in full: `→` is inert while a pane is descended, so an unbounded walk from a
/// descended screen never terminates.
fn on_history(app: &mut App) {
    for _ in 0..=Tab::ALL.len() {
        if app.active() == Tab::History {
            return;
        }
        press(app, KeyCode::Right);
    }
    panic!("could not reach the history tab from {:?}: is a pane descended and trapping the arrows?", app.active());
}

/// An app on the history tab with a real (tempdir) export tree behind it. The history screen
/// reads no machine probe, so the environment handed through is the default "nothing found,
/// nothing measured".
fn app_on_history(dir: &TempDir) -> App {
    let mut app =
        App::new(Tier::Full).with_source_environment(dir.path().to_path_buf(), Some(dir.path().join("out")), Environment::default());
    on_history(&mut app);
    app
}

/// Ticks until an alert lands, bounded by wall clock rather than by an iteration count — the
/// shape `tests/memories_screen.rs`'s `wait_for_alert` argues for in full, and the deadline's
/// only job is to stop a hang.
fn wait_for_alert(app: &mut App) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        app.tick();
        if app.history().alert().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("no alert arrived within 60s: the worker never sent a Finished event");
}

/// The picker's interior rows start below the panel's top border; the formats pane's below its
/// own. At 120x24 both panels sit side by side, so the picker's interior runs columns 2..32 and
/// the formats pane's columns 36..118 of each row.
const PICKER_ROWS_Y: u16 = 2;
/// Row 0 of the formats pane's interior: the first format toggle.
const FORMATS_ROWS_Y: u16 = 2;
/// The export chip's slot among the formats rows: the four toggles then the chip.
const CHIP_ROW: u16 = FORMATS_ROWS_Y + 4;
/// The row below the chip: the disabled chip's tooltip while it holds focus, and the run counter
/// otherwise. With the tooltip live the counter sits one row lower.
const COUNTER_ROW: u16 = CHIP_ROW + 1;
/// Where the chip's text starts: the pane's border, the padding cell, and the caret gutter.
const CHIP_X: u16 = PICKER_PANEL_WIDTH + 4;

/// Descends to the formats pane and parks the caret on the export chip. Requires the pane's
/// focus to be fresh (at the first toggle): the focus keeps its position across ascents, and the
/// walk below counts from row zero.
fn on_the_chip(app: &mut App) {
    press(app, KeyCode::Enter);
    for _ in 0..4 {
        press(app, KeyCode::Down);
    }
    assert!(app.history().descended(), "the fixture must leave the pane descended");
}

// ---- the picker ----

#[test]
fn the_picker_lists_every_conversation_with_title_or_key() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // Decision 64's label rule, one row each way: the titled conversation renders its title, the
    // untitled one its key.
    assert_eq!(picker_row(buffer, PICKER_ROWS_Y), "❯ [x] Alice's Thread");
    assert_eq!(picker_row(buffer, PICKER_ROWS_Y + 1), "  [x] bob");

    // The selection defaults to every conversation, so both checkboxes carry the accent mark —
    // and the mark is data, so it does not wait on focus: the blurred row's `x` is accent too.
    // The caret gutter is two cells, so the mark sits at column 5.
    assert_eq!(buffer[(5, PICKER_ROWS_Y)].symbol(), "x");
    assert_eq!(buffer[(5, PICKER_ROWS_Y)].style().fg, Some(palette.accent));
    assert_eq!(buffer[(5, PICKER_ROWS_Y + 1)].symbol(), "x");
    assert_eq!(buffer[(5, PICKER_ROWS_Y + 1)].style().fg, Some(palette.accent));

    // The focused row carries the caret and the label promotion; the blurred row neither.
    assert_eq!(buffer[(2, PICKER_ROWS_Y)].symbol(), "❯");
    assert_eq!(buffer[(2, PICKER_ROWS_Y + 1)].symbol(), " ");
}

#[test]
fn space_toggles_the_focused_conversation_and_the_chip_count_follows() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    // The caret is on the picker, so the formats pane's rows render blurred: the chip keeps its
    // count, the caret cell stays blank.
    assert_eq!(formats_row(terminal.backend().buffer(), CHIP_ROW), "   export 2");

    press(&mut app, KeyCode::Char(' '));
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(picker_row(buffer, PICKER_ROWS_Y), "❯ [ ] Alice's Thread");
    assert_eq!(formats_row(buffer, CHIP_ROW), "   export 1");

    // Space again restores it.
    press(&mut app, KeyCode::Char(' '));
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y), "❯ [x] Alice's Thread");
}

#[test]
fn enter_descends_and_space_toggles() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);

    // The load defaults the selection to every conversation, so the row starts checked. Enter
    // enters the detail (the master-detail grammar); it must not flip the row — a toggling
    // enter would render `[ ]` here. The blurred picker drops its caret for the two-space
    // gutter, which is how the render shows the pane lost focus.
    press(&mut app, KeyCode::Enter);
    assert!(app.history().descended(), "enter descends into the formats pane");
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(
        picker_row(terminal.backend().buffer(), PICKER_ROWS_Y),
        "  [x] Alice's Thread",
        "enter is the descend key, not the row toggle"
    );

    // Space is the row toggle the brief names — one flip on the focused row, and the chip
    // count follows.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char(' '));
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(picker_row(buffer, PICKER_ROWS_Y), "❯ [ ] Alice's Thread");
    assert_eq!(formats_row(buffer, CHIP_ROW), "   export 1");

    // Space again restores it.
    press(&mut app, KeyCode::Char(' '));
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y), "❯ [x] Alice's Thread");
    assert_eq!(formats_row(terminal.backend().buffer(), CHIP_ROW), "   export 2");
}

#[test]
fn t_toggles_every_conversation_in_one_press() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    // `t` is the batch toggle: the contract's hotkey algorithm reserves `a` for the action menu,
    // so toggle-all takes the algorithm's free first char of "toggle all" (the user ruling).
    press(&mut app, KeyCode::Char('t'));
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(picker_row(buffer, PICKER_ROWS_Y), "❯ [ ] Alice's Thread");
    assert_eq!(picker_row(buffer, PICKER_ROWS_Y + 1), "  [ ] bob");
    // Zero hides the count: with nothing selected the chip reads `export`, never `export 0`
    // (and it is disabled at zero anyway, so the count would be the one number that does
    // nothing).
    assert_eq!(formats_row(buffer, CHIP_ROW), "   export");

    // A second `t` selects everything again.
    press(&mut app, KeyCode::Char('t'));
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y), "❯ [x] Alice's Thread");

    // The modifier guard: `T` with SHIFT alone still toggles — terminals report the shifted
    // char with the shift modifier set — but a chord like ALT+t must not trip the batch toggle.
    press_with(&mut app, KeyCode::Char('T'), KeyModifiers::SHIFT);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y), "❯ [ ] Alice's Thread");
    press_with(&mut app, KeyCode::Char('t'), KeyModifiers::ALT);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y), "❯ [ ] Alice's Thread");
}

#[test]
fn the_picker_arrows_wrap_and_move_the_caret() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);

    // Down moves to bob; Down again wraps back to the top.
    press(&mut app, KeyCode::Down);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y), "  [x] Alice's Thread");
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y + 1), "❯ [x] bob");

    press(&mut app, KeyCode::Down);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y), "❯ [x] Alice's Thread");

    // Up from the top wraps to the bottom.
    press(&mut app, KeyCode::Up);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y + 1), "❯ [x] bob");
}

#[test]
fn the_picker_shows_the_house_empty_state_for_an_export_with_no_conversations() {
    let dir = export_tree(&[]);
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // The house empty state inside the pane: a rounded frame around a hint, minus the action
    // line — an empty conversation list has no key to offer, so the shared widget's hardcoded
    // "press ↵ to start" would advertise a run that starts nothing.
    let interior = |y: u16| (2..PICKER_PANEL_WIDTH - 1).map(|x| buffer[(x, y)].symbol()).collect::<String>();
    let hint_y = (PICKER_ROWS_Y..20)
        .find(|&y| interior(y).contains("no conversations"))
        .expect("the empty state names the condition somewhere in it");
    // `str::find` returns a BYTE index and the interior's border glyph is multi-byte, so count
    // the chars before it to land on the hint's first cell.
    let hint_byte = interior(hint_y).find("no conversations").expect("the hint is in the found row");
    let hint_x = interior(hint_y)[..hint_byte].chars().count() as u16 + 2;
    // The frame's left border sits one border cell and the three-cell inset left of the hint.
    assert_eq!(buffer[(hint_x - 4, hint_y - 1)].symbol(), "╭", "the hint sits inside the house frame");
    assert_eq!(buffer[(hint_x - 4, hint_y + 1)].symbol(), "╰", "the frame closes below the hint");
    assert_eq!(buffer[(hint_x, hint_y)].style().fg, Some(palette.text_dim));
    assert!((PICKER_ROWS_Y..20).all(|y| !interior(y).contains("press")), "no action line: an empty picker has no key to advertise");
}

#[test]
fn a_source_without_json_is_prose_the_pane_names() {
    // An empty tempdir: no parts, no flat `json/`. The picker holds the load's refusal instead of
    // a list, in the overview's never-fail pattern — and the selection stays empty, so the chip
    // refuses too.
    let dir = TempDir::new().unwrap();
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();

    // The refusal's prose is a `RunError` `Display` whose exact spelling names a machine-specific
    // path, so the pin is the stable clause.
    assert!(
        picker_row(terminal.backend().buffer(), PICKER_ROWS_Y).starts_with("no unpacked export part"),
        "{}",
        picker_row(terminal.backend().buffer(), PICKER_ROWS_Y)
    );
}

// ---- the formats pane and the refusal ----

#[test]
fn enter_descends_and_left_or_esc_ascends() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);

    press(&mut app, KeyCode::Enter);
    assert!(app.history().descended(), "enter descends into the formats pane");

    press(&mut app, KeyCode::Left);
    assert!(!app.history().descended(), "← ascends back to the picker");

    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    assert!(!app.history().descended(), "esc ascends like the other screens");
}

#[test]
fn the_formats_pane_toggles_formats_and_refuses_an_empty_selection() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    // Descend and switch every format off: space on each of the four toggles.
    press(&mut app, KeyCode::Enter);
    for _ in 0..4 {
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Down);
    }
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    for y in 0..4 {
        assert_eq!(formats_row(buffer, FORMATS_ROWS_Y + y), format!("  [ ] {}", ["html", "json", "text", "csv"][y as usize]));
    }

    // The chip now refuses with the format reason — the same reason the run's own guard gives.
    assert_eq!(formats_row(buffer, CHIP_ROW), "❯  export 2");
    assert_eq!(formats_row(buffer, COUNTER_ROW), "  └ pick at least one format");

    // Enter on the chip starts nothing.
    press(&mut app, KeyCode::Enter);
    assert!(!app.history().run_in_flight(), "a run with no format must not start");
}

#[test]
fn an_empty_conversation_selection_is_a_visible_refusal_not_a_run_of_everything() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    // Everything off, then park on the chip.
    press(&mut app, KeyCode::Char('t'));
    on_the_chip(&mut app);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // The refusal is CONTENT, not style: the tooltip row spells the fix in words, and the chip
    // reads faint on the raised fill. The two pins are the brief's refusal contract.
    assert_eq!(formats_row(buffer, CHIP_ROW), "❯  export");
    assert_eq!(formats_row(buffer, COUNTER_ROW), "  └ pick at least one conversation");
    assert_eq!(buffer[(CHIP_X, CHIP_ROW)].style().fg, Some(palette.text_faint));

    // Enter on the disabled chip is inert: no worker, no counter. The tooltip holds the row below
    // the chip, so the empty counter slot sits one row lower.
    press(&mut app, KeyCode::Enter);
    assert!(!app.history().run_in_flight(), "the empty selection must refuse rather than run everything");
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW + 1), "");
}

#[test]
fn the_tooltip_appears_only_while_the_disabled_chip_holds_focus() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    // All deselected, but the caret still on the picker: no tooltip yet.
    press(&mut app, KeyCode::Char('t'));
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "");

    // The tooltip is bound to the chip by identity, so it appears exactly when the caret parks on
    // the chip row — and vanishes when it leaves.
    on_the_chip(&mut app);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "  └ pick at least one conversation");

    press(&mut app, KeyCode::Up);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "");

    // Ascending with the caret still parked on the chip must drop the tooltip too: the pane is
    // blurred, so no row holds the caret. The formats focus keeps its position across the
    // ascent, so one `Down` from csv reaches the chip without touching the picker — and the
    // render's `descended` gate exists for exactly this state.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Esc);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "");
}

#[test]
fn q_ascends_from_the_formats_pane_without_arming_the_quit() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);

    press(&mut app, KeyCode::Enter);
    assert!(app.history().descended(), "enter descends into the formats pane");

    // `q` while descended is the back key, exactly like esc: it ascends and arms nothing — the
    // hint bar advertises `q back` off the same answer. Mirrors the memories screen's pin.
    press(&mut app, KeyCode::Char('q'));
    assert!(!app.history().descended(), "q ascends while descended");
    assert!(!app.is_quit_armed(), "that q armed nothing");
    assert!(app.is_running());
}

#[test]
fn the_tooltip_wraps_inside_the_pane_rather_than_clipping_at_the_narrow_floor() {
    // At 64 wide — the side-by-side floor — the formats pane has a 26-cell interior, 4 cells
    // narrower than the tooltip's single-line form, so the reason word-wraps into two complete
    // rows instead of clipping mid-word (finding 3).
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(64, 14)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    press(&mut app, KeyCode::Char('t'));
    on_the_chip(&mut app);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();

    // Side by side: the picker's 34 columns and the formats pane's 30, borders touching. The
    // pane's interior rows: four toggles, the chip, the wrapped tooltip, the counter slot.
    let row = |y: u16| (36..62).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned();
    assert_eq!(row(6), "❯  export");
    assert_eq!(row(7), "  └ pick at least one", "the reason's first segment keeps the leader");
    assert_eq!(row(8), "    conversation", "the continuation line indents to the leader's width");
    assert_eq!(row(9), "", "the counter slot follows the wrapped reason");
}

// ---- the run: counter, alert lifecycle, worker machinery ----

#[test]
fn enter_on_the_chip_starts_a_run_whose_counter_advances_per_written_conversation() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    on_the_chip(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    // Enter starts the real worker, which writes the manifest where the screen says — the override
    // parks it in a tempdir, so the per-user data dir is never touched.
    let manifest = TempDir::new().unwrap();
    app.history_mut().set_manifest_dir(manifest.path().to_path_buf());
    press(&mut app, KeyCode::Enter);
    assert!(app.history().run_in_flight(), "enter on the enabled chip starts the run");

    // While the run is live the chip is disabled, and with the caret on it the chip's reason takes
    // the tooltip row — the third of the refusal's three priorities.
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "  └ a run is already in flight");

    // The seam feeds the worker's events, exactly as the real worker would send them: the plan
    // names the total, each `Written` advances the counter (decision 63), `Finished` lands the
    // alert. The tooltip holds the row below the chip, so the live counter sits one row lower.
    let (sender, receiver) = mpsc::channel();
    sender.send(RunEvent::Planned(PlanSnapshot { conversations: 2 })).unwrap();
    app.with_history_channel(receiver);
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW + 1), "  0 of 2 conversations");

    sender.send(RunEvent::Written).unwrap();
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW + 1), "  1 of 2 conversations");

    sender.send(RunEvent::Written).unwrap();
    sender
        .send(RunEvent::Finished(RunOutcome::Completed(HistoryReport {
            conversations: 2,
            documents: 8,
            links: HtmlLinks::NoManifest,
            // The seam simulates the real worker having written html — the placeholder note
            // arms only when html was actually written.
            html_written: true,
        })))
        .unwrap();
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    // The run is over, so the chip is enabled again, the tooltip is gone, and the counter takes
    // the tooltip's row.
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "  2 of 2 conversations");

    let alert = app.history().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Info);
    assert!(alert.message.contains("2 conversations"), "{}", alert.message);
    assert!(alert.message.contains("8 documents"), "{}", alert.message);
    assert!(alert.message.contains("media links are placeholders"), "decision 62's note is stated once: {}", alert.message);
}

#[test]
fn x_dismisses_the_alert_and_a_new_run_resolves_it() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let (sender, receiver) = mpsc::channel();
    sender
        .send(RunEvent::Finished(RunOutcome::Completed(HistoryReport {
            conversations: 2,
            documents: 8,
            links: HtmlLinks::Manifest,
            html_written: true,
        })))
        .unwrap();
    app.with_history_channel(receiver);
    app.tick();
    assert!(app.history().alert().is_some());

    press(&mut app, KeyCode::Char('x'));
    assert!(app.history().alert().is_none(), "x dismisses the alert the footer is showing");

    // A new run resolves the completion alert and forgets the counter.
    app.history_mut().start_run_with(
        |_inputs, sender| {
            let _ = sender.send(RunEvent::Finished(RunOutcome::Completed(HistoryReport {
                conversations: 0,
                documents: 0,
                links: HtmlLinks::Manifest,
                html_written: false,
            })));
        },
        None,
    );
    wait_for_alert(&mut app);
    assert!(app.history().alert().is_some());
}

#[test]
fn a_worker_that_exits_after_finished_does_not_overwrite_the_outcome() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let (sender, receiver) = mpsc::channel();
    sender
        .send(RunEvent::Finished(RunOutcome::Completed(HistoryReport {
            conversations: 2,
            documents: 8,
            links: HtmlLinks::Manifest,
            html_written: true,
        })))
        .unwrap();
    // The worker has exited: its sender is gone, so the channel reads Disconnected once the
    // Finished event is drained. That must read as "the run is over", never as a panic — the
    // regression this pins replaced every completion alert with the panic alert on real runs.
    drop(sender);
    app.with_history_channel(receiver);
    app.tick();

    let alert = app.history().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Info, "the true outcome must survive the dead channel");
    app.tick();
    assert_eq!(app.history().alert().unwrap().kind, AlertKind::Info);
}

#[test]
fn a_channel_that_goes_dead_without_a_finished_event_reports_a_panic() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let (sender, receiver) = mpsc::channel();
    // The worker died without ever sending — not even its panic arm ran. The dead channel is
    // the one way the screen can still learn about it, and it must report a panic rather than
    // spin forever.
    drop(sender);
    app.with_history_channel(receiver);
    app.tick();

    let alert = app.history().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning);
    assert!(alert.message.contains("unexpectedly"), "{}", alert.message);
}

#[test]
fn a_worker_that_panics_still_yields_a_panic_alert() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    app.history_mut().start_run_with(|_inputs, _sender| panic!("boom"), None);

    // The panic is contained worker-side and reported as a Finished event; the screen must land
    // on the panic alert rather than spinning forever.
    wait_for_alert(&mut app);
    let alert = app.history().alert().unwrap();
    assert_eq!(alert.kind, AlertKind::Warning);
    assert!(alert.message.contains("unexpectedly"), "{}", alert.message);
    assert!(!app.history().run_in_flight(), "the run is over even though it failed");
}

#[test]
fn the_planning_spinner_shows_while_the_worker_has_not_planned() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let (sender, receiver) = mpsc::channel();
    app.with_history_channel(receiver);
    app.tick();
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();

    let spinner = formats_row(terminal.backend().buffer(), COUNTER_ROW);
    assert!(spinner.contains("planning"), "{spinner}");

    sender.send(RunEvent::Planned(PlanSnapshot { conversations: 2 })).unwrap();
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "  0 of 2 conversations");
}

#[test]
fn the_counter_and_the_completion_use_the_singular_for_one_conversation() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let (sender, receiver) = mpsc::channel();
    app.with_history_channel(receiver);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    // One planned conversation: the counter's noun takes the singular, the plural must not
    // leak onto it.
    sender.send(RunEvent::Planned(PlanSnapshot { conversations: 1 })).unwrap();
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "  0 of 1 conversation");

    sender.send(RunEvent::Written).unwrap();
    app.tick();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "  1 of 1 conversation");

    // The completion alert agrees: one conversation, never "1 conversations".
    sender
        .send(RunEvent::Finished(RunOutcome::Completed(HistoryReport {
            conversations: 1,
            documents: 4,
            links: HtmlLinks::Manifest,
            html_written: true,
        })))
        .unwrap();
    app.tick();
    let message = &app.history().alert().unwrap().message;
    assert!(message.contains("1 conversation"), "{message}");
    assert!(!message.contains("1 conversations"), "the plural must not leak onto the singular: {message}");
    assert!(message.contains("4 documents"), "{message}");
}

#[test]
fn the_counter_groups_four_digit_totals() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let (sender, receiver) = mpsc::channel();
    app.with_history_channel(receiver);
    sender.send(RunEvent::Planned(PlanSnapshot { conversations: 1000 })).unwrap();
    app.tick();
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(formats_row(terminal.backend().buffer(), COUNTER_ROW), "  0 of 1,000 conversations");
}

#[test]
fn the_placeholder_links_note_arms_only_when_html_was_written() {
    // A no-manifest run that wrote no html has no links to be placeholders: the clause is
    // absent rather than misnaming a silence (finding 5).
    let no_html =
        RunAlert::history_completion(&HistoryReport { conversations: 2, documents: 2, links: HtmlLinks::NoManifest, html_written: false });
    assert!(!no_html.message.contains("media links"), "{}", no_html.message);

    // The same report with html written states decision 62's note once.
    let with_html =
        RunAlert::history_completion(&HistoryReport { conversations: 2, documents: 8, links: HtmlLinks::NoManifest, html_written: true });
    assert!(with_html.message.contains("media links are placeholders"), "{}", with_html.message);
}

// ---- the screen's own selection goes into the run's inputs ----

#[test]
fn the_run_receives_exactly_the_selected_conversations_and_formats() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    on_the_chip(&mut app);

    // Toggle bob off, turn csv off, then run. The caret returns to the formats pane where it left
    // off — parked on the chip by `on_the_chip` — so one `Up` reaches csv; three `Down`s from the
    // chip would wrap to text instead.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Char(' '));

    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let inputs_seen = seen.clone();
    app.history_mut().start_run_with(
        move |inputs, sender| {
            *inputs_seen.lock().unwrap() = Some((inputs.conversations.clone(), inputs.formats.clone()));
            let _ = sender.send(RunEvent::Finished(RunOutcome::Completed(HistoryReport {
                conversations: inputs.conversations.len(),
                documents: inputs.conversations.len() * inputs.formats.len(),
                links: HtmlLinks::Manifest,
                html_written: inputs.formats.contains(&HistoryFormat::Html),
            })));
        },
        None,
    );
    wait_for_alert(&mut app);

    let (conversations, formats) = seen.lock().unwrap().clone().unwrap();
    assert_eq!(conversations.len(), 1, "only alice stays selected: {conversations:?}");
    assert_eq!(conversations.first().unwrap().as_str(), "alice");
    assert_eq!(formats.len(), 3, "exactly one format was toggled off: {formats:?}");
    assert!(
        formats.contains(&HistoryFormat::Text) && !formats.contains(&HistoryFormat::Csv),
        "csv must be the format that dropped, not any other: {formats:?}"
    );
}

// ---- the hint bar ----

#[test]
fn the_history_tab_hints_name_its_own_keys_while_the_picker_holds_rows() {
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let footer = (0..120).map(|x| terminal.backend().buffer()[(x, 23)].symbol()).collect::<String>();
    // The history tab's own keys take the middle groups: `t toggle all` and `space toggle`,
    // between the shell's universal `←→ switch` and `q quit` (finding 7).
    assert_eq!(footer.trim_end(), " ←→ switch   t toggle all   space toggle   q quit");
}

#[test]
fn the_toggle_hints_leave_the_footer_when_the_picker_has_no_rows() {
    // An export that LOADS but holds no conversations: the picker is empty, so `t` and `space`
    // have nothing to act on, and their hints are derived away — never copied per branch.
    let dir = export_tree(&[]);
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let footer = (0..120).map(|x| terminal.backend().buffer()[(x, 23)].symbol()).collect::<String>();
    assert_eq!(footer.trim_end(), " ←→ switch   q quit");
}

#[test]
fn the_footer_trims_from_the_right_and_keeps_the_quit_hint_at_narrow_widths() {
    // The full history set is 49 cells; at 42 the tail — `q quit` — used to clip entirely, its
    // six cells sitting beyond the frame (reviewer #2). The set trims from the right, the
    // escape hint last to go, so the escape stays reachable anywhere the frame renders.
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(42, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let footer = (0..42).map(|x| terminal.backend().buffer()[(x, 23)].symbol()).collect::<String>();
    assert_eq!(footer.trim_end(), " ←→ switch   t toggle all   q quit");
    // The picker-only floor, 34 cells: the same trimmed set fits exactly.
    let mut terminal = Terminal::new(TestBackend::new(34, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let footer = (0..34).map(|x| terminal.backend().buffer()[(x, 23)].symbol()).collect::<String>();
    assert_eq!(footer.trim_end(), " ←→ switch   t toggle all   q quit");
}

// ---- the picker's order and its truncation split ----

#[test]
fn the_picker_sorts_titles_case_insensitively_with_the_key_breaking_ties() {
    let dir = export_tree(&[
        ("key-b", vec![chat_entry("2021-03-04 09:00:00 UTC", Some("Banana"))]),
        ("key-a", vec![chat_entry("2021-03-04 10:00:00 UTC", Some("apple"))]),
        // A second thread shares the first's title; the keys decide between them.
        ("key-c", vec![chat_entry("2021-03-04 11:00:00 UTC", Some("Banana"))]),
    ]);
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();

    // Case-insensitively, `apple` precedes `Banana` — a byte-wise sort would put the capital
    // first — and the tied `Banana` pair resolves by key: `key-b` before `key-c`.
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y), "❯ [x] apple");
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y + 1), "  [x] Banana");
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y + 2), "  [x] Banana");
}

#[test]
fn the_picker_truncates_titles_as_prose_and_keys_as_identities() {
    // A title and a key-only row, both longer than the 24-cell label budget at this width: the
    // title takes the prose trailing cut, the key row keeps the identity middle cut (finding 11).
    let dir = export_tree(&[
        ("b~aB3xY9aB3xY9aB3xY9aB3xY9", vec![chat_entry("2021-03-04 09:00:00 UTC", None)]),
        ("key-zz", vec![chat_entry("2021-03-04 10:00:00 UTC", Some("The titled conversation chat"))]),
    ]);
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();

    // The key-only row sorts first (`b~` before `t`), and its label ends in id characters —
    // the middle cut preserved the tail, which is the identity's discriminating half.
    let key_row = picker_row(terminal.backend().buffer(), PICKER_ROWS_Y);
    assert_eq!(key_row, "❯ [x] b~aB3xY9aB3…aB3xY9aB3xY9");
    assert!(key_row.ends_with('9'), "the key row keeps the identity tail: {key_row}");
    // The title row takes the prose cut: the trailing ellipsis names what was cut.
    // 28 chars of title against a 24-cell budget: prose truncation keeps 23 chars + the ellipsis.
    assert_eq!(picker_row(terminal.backend().buffer(), PICKER_ROWS_Y + 1), "  [x] The titled conversation…");
}

// ---- the chip must survive the picker-only fallback ----

#[test]
fn the_chip_survives_the_picker_only_fallback_and_stays_reachable() {
    // At 50x8 the body is 6 rows — below the stacked arm's chip floor — so only the picker pane
    // renders, and the export chip (the run's only trigger) moves into the surviving pane rather
    // than vanishing with the formats pane (finding 12). The size banner claims the body's first
    // row, so the panel's interior runs rows 3..6: one conversation row, then the chip, with the
    // counter slot holding the pane's bottom row.
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(50, 8)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();

    let row = |y: u16| (2..49).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned();
    assert_eq!(row(3), "❯ [x] Alice's Thread");
    // The chip row carries the pane's caret gutter and the chip's own leading space, so the
    // label sits three cells in — the same offset the side-by-side pins use.
    assert_eq!(row(4), "   export 2", "the chip renders in the picker pane");
    assert_eq!(row(5), "", "the counter slot holds the pane's bottom row");

    // And the trigger works from there: the picker-only walk spans the rows and then the chip —
    // enter on the chip starts the run, with no descent into a pane that is not drawn (reviewer
    // #3), against the same enabled/disabled rules as the formats pane's chip.
    let manifest = TempDir::new().unwrap();
    app.history_mut().set_manifest_dir(manifest.path().to_path_buf());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(app.history().run_in_flight(), "the chip stays reachable when the formats pane drops");
}

#[test]
fn the_picker_only_walk_covers_the_chip_and_never_the_invisible_formats_rows() {
    // At 50 wide the formats pane is not drawn — the side-by-side floor is 64 — so the walk
    // spans the visible conversation rows and the export chip, and nothing else: no invisible
    // row takes focus (reviewer #3).
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(50, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();

    // Interior rows 2..21 at 24 high: the two conversation rows, then the chip and the counter.
    let row = |buffer: &Buffer, y: u16| (2..49).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned();
    assert_eq!(row(terminal.backend().buffer(), 2), "❯ [x] Alice's Thread");
    press(&mut app, KeyCode::Down);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(row(terminal.backend().buffer(), 2), "  [x] Alice's Thread");
    assert_eq!(row(terminal.backend().buffer(), 3), "❯ [x] bob");
    press(&mut app, KeyCode::Down);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(row(terminal.backend().buffer(), 20), "❯  export 2", "the walk reaches the chip at the picker-only geometry");
    press(&mut app, KeyCode::Down);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(row(terminal.backend().buffer(), 2), "❯ [x] Alice's Thread", "the walk wraps back to the first row");

    // Enter cannot descend into a pane that is not drawn: the caret stays on the picker, and
    // the next arrow still walks the visible rows rather than the invisible formats rows.
    press(&mut app, KeyCode::Enter);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(row(terminal.backend().buffer(), 2), "❯ [x] Alice's Thread", "enter at the picker-only geometry does not descend");
    press(&mut app, KeyCode::Down);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(row(terminal.backend().buffer(), 3), "❯ [x] bob", "the walk still owns the visible rows");
}

#[test]
fn the_disabled_chips_reason_renders_under_the_chip_in_the_picker_only_arm() {
    // The tooltip is formats-pane-bound; below the side-by-side floor that pane is not drawn, so
    // the surviving pane must spell the disabled chip's reason itself, under the chip, the
    // moment the walk reaches it (reviewer #3). The slot's height reserves the wrapped reason
    // whenever the chip is disabled, so the chip's row does not jump when the caret lands.
    let dir = export_tree(&two_threads());
    let mut app = app_on_history(&dir);
    let mut terminal = Terminal::new(TestBackend::new(50, 24)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    press(&mut app, KeyCode::Char('t')); // deselects everything: the chip disables
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();

    // Disabled with a one-line reason at this width — the pane spans the full frame, so its
    // 46-cell interior holds the reason whole: the slot is the chip, the reason, and the
    // counter, the chip sitting three rows up from the pane's bottom, its reason row blank
    // while the walk holds a conversation row.
    let row = |buffer: &Buffer, y: u16| (2..49).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned();
    assert_eq!(row(terminal.backend().buffer(), 19), "   export", "the disabled chip holds its row before the walk reaches it");
    assert_eq!(row(terminal.backend().buffer(), 20), "", "the reserved reason row stays blank until the walk holds the chip");

    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    assert_eq!(row(terminal.backend().buffer(), 19), "❯  export", "the walk's chip caret");
    assert_eq!(row(terminal.backend().buffer(), 20), "  └ pick at least one conversation");
}
