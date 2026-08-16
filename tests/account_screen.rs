//! Render tests for the account tab: the read-only master-detail over the export's account
//! metadata. The fixture exports are synthetic; the wire shapes are the real export's keys.
//!
//! The privacy pins are the point of this file: the location section renders counts only, a
//! planted identity, coordinate, IP, business id or name, or message body must never reach a
//! frame, and a broken file lands as absent rows with no error text on screen — the same
//! discard the overview's load path makes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use exportsnap::app::{App, RunDefaults, Tab};
use exportsnap::config::Config;
use exportsnap::export::env::Environment;
use exportsnap::tui::shell;
use exportsnap::tui::theme::{Palette, Tier};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

const EXPORT_ID: &str = "1784667002819";
/// The master (sections) pane's width at a terminal width: ~30% of the body, clamped 20-40 —
/// the growth the screen's `widgets::selector_panel_width` gives its 20-cell content floor. At
/// the 51-cell side-by-side floor this stays 20; at `WIDE` it grows to 36.
fn sections_panel_width(terminal_width: u16) -> u16 {
    ((usize::from(terminal_width) * 3) / 10).clamp(20, 40) as u16
}
/// Interior columns of the sections panel at `terminal_width`: one border plus one padding cell
/// in from each edge.
fn sections_interior(terminal_width: u16) -> std::ops::Range<u16> {
    2..(sections_panel_width(terminal_width) - 1)
}
/// Interior columns of the detail panel at `terminal_width`: one border plus one padding cell
/// in from each edge, the panel starting at the sections panel's edge.
fn detail_columns(terminal_width: u16) -> std::ops::Range<u16> {
    (sections_panel_width(terminal_width) + 2)..(terminal_width - 2)
}
/// The scrollbar column inside the detail panel at `WIDE`: the interior's right edge, which is
/// the panel's right padding column.
const DETAIL_SCROLLBAR_COLUMN: u16 = 118;
const FIRST_ROW: u16 = 2;
const WIDE: u16 = 120;
const TALL: u16 = 24;

// ---- fixtures ----

/// `account.json`: every identity row populated, plus one device and one login.
fn account_json() -> String {
    r#"{
        "Basic Information": {
            "Username": "fixture-user",
            "Name": "Fixture User",
            "Creation Date": "2019-05-04 10:00:00 UTC",
            "Registration IP": "203.0.113.7",
            "Country": "Fictionia",
            "Last Active": "2026-08-01"
        },
        "Device History": [{"Make": "Acme", "Model": "X1", "Start Time": "2019-05-04 10:00:00 UTC", "Device Type": "mobile"}],
        "Login History": [{"IP": "203.0.113.7", "Country": "Fictionia", "Created": "2019-05-04 10:00:00 UTC", "Status": "successful", "Device": "Acme X1"}]
    }"#
    .to_owned()
}

/// `friends.json`: every list present, with counts 2, 1, 1, 0, 1, 0, 1, 2.
fn friends_json() -> String {
    r#"{
        "Friends": [{"Username": "f1"}, {"Username": "f2"}],
        "Friend Requests Sent": [{"Username": "r1"}],
        "Blocked Users": [{"Username": "b1"}],
        "Deleted Friends": [],
        "Hidden Friend Suggestions": [{"Username": "h1"}],
        "Ignored Snapchatters": [],
        "Pending Requests": [{"Username": "p1"}],
        "Shortcuts": [{"Username": "s1"}, {"Username": "s2"}]
    }"#
    .to_owned()
}

/// `location_history.json`: every section populated with one entry. The strings below are the
/// privacy pins — a place name, a point's bytes, a coordinate pair, a business id, and a
/// business name — that must never render, while the counts must.
fn location_json() -> String {
    r#"{
        "Frequent Locations": [{"City": "Springfield"}],
        "Latest Location": [{"City": "Springfield"}],
        "Home, School & Work": {"Home": "Springfield"},
        "Daily Top Locations": ["Latitude, Longitude: 48.858844, 2.294351"],
        "Top Locations Per Six-Day Period": ["Latitude, Longitude: 48.858844, 2.294351"],
        "Location History": ["Latitude, Longitude: 48.858844, 2.294351"],
        "Businesses and places you may have visited": {"biz-0000000000000000000000": ["planted-business-name"]},
        "Actiomoji information from places you may have visited": [],
        "Areas you may have visited in the last two years": [{"Time": "2024-01-01 00:00:00 UTC", "City": "Springfield"}]
    }"#
    .to_owned()
}

/// `story_history.json`: two stories with known view and reply counts, plus a planted caption
/// in the friend-and-public list — a message body in a file the screen does read, so the
/// privacy pin kills a regression that renders text out of a read file.
fn stories_json() -> String {
    r#"{
        "Your Story Views": [
            {"Story Date": "2021-03-04 09:00:00 UTC", "Story Views": 12, "Story Replies": 3},
            {"Story Date": "2021-03-05 09:00:00 UTC", "Story Views": 7, "Story Replies": 1}
        ],
        "Friend and Public Story Views": ["planted-story-caption"]
    }"#
    .to_owned()
}

/// `user_profile.json`: the empty list the one observed export holds — no guessed element keys.
fn user_profile_json() -> String {
    r#"{"Subscriptions": []}"#.to_owned()
}

/// `chat_history.json` with a planted message body. The account screen never reads this file;
/// the privacy test pins that the body never renders.
fn chat_with_body_json() -> String {
    r#"{"conv_a": [{"From": "friend", "Created": "2021-01-01 00:00:00 UTC", "Content": "planted-message-body"}]}"#.to_owned()
}

/// One delivery with every account screen file written, plus a chat file carrying a planted
/// message body. `discover_parts` only reads names, so the zips can be empty files.
fn export_tree() -> TempDir {
    let dir = TempDir::new().unwrap();
    let json = dir.path().join(format!("mydata~{EXPORT_ID}/json"));
    fs::create_dir_all(&json).unwrap();
    fs::write(json.join("account.json"), account_json()).unwrap();
    fs::write(json.join("friends.json"), friends_json()).unwrap();
    fs::write(json.join("location_history.json"), location_json()).unwrap();
    fs::write(json.join("story_history.json"), stories_json()).unwrap();
    fs::write(json.join("user_profile.json"), user_profile_json()).unwrap();
    fs::write(json.join("chat_history.json"), chat_with_body_json()).unwrap();
    fs::write(dir.path().join(format!("mydata~{EXPORT_ID}-2.zip")), b"").unwrap();
    fs::write(dir.path().join(format!("mydata~{EXPORT_ID}-3.zip")), b"").unwrap();
    dir
}

// ---- harness ----

/// The app on the account tab against a real export tree.
fn app_on_account(dir: &Path) -> App {
    let mut app = App::new(Tier::Full).with_source_environment(
        dir.to_path_buf(),
        RunDefaults { out_root: dir.join("out"), ..RunDefaults::resolve(None, &Config::default(), dir) },
        Environment::default(),
    );
    for _ in 0..=Tab::ALL.len() {
        press(&mut app, KeyCode::Right);
        if app.active() == Tab::Account {
            break;
        }
    }
    assert_eq!(app.active(), Tab::Account, "the right-arrow walk reaches the account tab");
    app
}

/// One key press, like a frame in the real loop.
fn press(app: &mut App, code: KeyCode) {
    app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

/// Draws the app and returns the terminal.
fn draw(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| shell::render(frame, app)).unwrap();
    terminal
}

/// One panel interior row, trailing filler dropped.
fn cell_run(buffer: &Buffer, columns: std::ops::Range<u16>, y: u16) -> String {
    columns.map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned()
}

/// The sections pane's interior on row `y`.
fn sections_row(buffer: &Buffer, y: u16) -> String {
    cell_run(buffer, sections_interior(buffer.area.width), y)
}

/// The detail pane's interior on row `y` at a given terminal width.
fn detail_row(buffer: &Buffer, width: u16, y: u16) -> String {
    cell_run(buffer, detail_columns(width), y)
}

/// The whole frame as one string, for absence assertions.
fn frame_text(buffer: &Buffer) -> String {
    (0..buffer.area.height).map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>()).collect()
}

/// The fill pin: every panel spans its full allotted body height — its bottom border closes on
/// the body's last row, not capped under its content. Walks the frame for panel top-left corners
/// (`╭`), finds each panel's bottom border (`╰` in the same column), and requires it to close at
/// the body's bottom row (`height - 2`, one above the footer).
fn assert_panels_fill(buffer: &Buffer, top: u16) {
    for y in top..buffer.area.height {
        for x in 0..buffer.area.width {
            if buffer[(x, y)].symbol() != "╭" {
                continue;
            }
            let bottom = (y + 1..buffer.area.height)
                .find(|&by| buffer[(x, by)].symbol() == "╰")
                .unwrap_or_else(|| panic!("the panel at ({x}, {y}) has no bottom border"));
            assert_eq!(
                bottom,
                buffer.area.height - 2,
                "the panel at ({x}, {y}) closes at row {bottom}, not on the body's last row {}",
                buffer.area.height - 2
            );
        }
    }
}

#[test]
fn the_panels_fill_the_body_at_the_designed_sizes() {
    // The fill contract: a panel spans its allotted body height rather than sizing to its rows.
    // At both designed sizes the panes sit side by side, so the section list and the detail must
    // each close on the body's last row — the density pass used to cap both under their content.
    let dir = export_tree();
    let mut app = app_on_account(dir.path());
    for (width, height) in [(80, 24), (110, 32)] {
        let terminal = draw(&mut app, width, height);
        assert_panels_fill(terminal.backend().buffer(), 1);
    }
}

// ---- the sections pane ----

#[test]
fn the_five_sections_render_and_the_caret_wraps() {
    let dir = export_tree();
    let mut app = app_on_account(dir.path());
    let palette = Palette::new(Tier::Full);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();

    assert_eq!(sections_row(buffer, FIRST_ROW), "❯ account");
    assert_eq!(sections_row(buffer, FIRST_ROW + 1), "  friends");
    assert_eq!(sections_row(buffer, FIRST_ROW + 2), "  location");
    assert_eq!(sections_row(buffer, FIRST_ROW + 3), "  stories");
    assert_eq!(sections_row(buffer, FIRST_ROW + 4), "  subscriptions");
    // The caret is the focused pane's, accent and bold.
    assert_eq!(buffer[(2, FIRST_ROW)].symbol(), "❯");
    assert_eq!(buffer[(2, FIRST_ROW)].style().fg, Some(palette.accent));
    assert!(buffer[(2, FIRST_ROW)].style().add_modifier.contains(ratatui::style::Modifier::BOLD));
    // The focused label promotes: TEXT + bold; the blurred ones stay TEXT_DIM.
    assert_eq!(buffer[(4, FIRST_ROW)].style().fg, Some(palette.text));
    assert!(buffer[(4, FIRST_ROW)].style().add_modifier.contains(ratatui::style::Modifier::BOLD));
    assert_eq!(buffer[(4, FIRST_ROW + 1)].style().fg, Some(palette.text_dim));
    // The master pane is the focused one: LINE_STRONG border and an ACCENT_2 first-panel title.
    assert_eq!(buffer[(0, 1)].style().fg, Some(palette.line_strong));
    assert!(cell_run(buffer, sections_interior(WIDE), 1).starts_with(" SECTIONS"));
    assert_eq!(buffer[(3, 1)].style().fg, Some(palette.accent_2));
    assert!(buffer[(3, 1)].style().add_modifier.contains(ratatui::style::Modifier::ITALIC));
    assert!(buffer[(3, 1)].style().add_modifier.contains(ratatui::style::Modifier::BOLD));

    // Down walks to the last section and wraps; Up wraps back to the first.
    for _ in 0..4 {
        press(&mut app, KeyCode::Down);
    }
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(sections_row(buffer, FIRST_ROW + 4), "❯ subscriptions");
    press(&mut app, KeyCode::Down);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(sections_row(buffer, FIRST_ROW), "❯ account", "Down past the last section wraps");
    press(&mut app, KeyCode::Up);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(sections_row(buffer, FIRST_ROW + 4), "❯ subscriptions", "Up past the first section wraps");
}

// ---- the master-detail grammar ----

#[test]
fn enter_descends_into_the_detail_and_esc_ascends() {
    let dir = export_tree();
    let mut app = app_on_account(dir.path());
    let palette = Palette::new(Tier::Full);
    // The master pane's grown width at `WIDE`; the detail pane's interior starts two cells past
    // its edge (border + padding).
    let master = sections_panel_width(WIDE);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    // Top level: the detail pane is drawn blurred — LINE border, TEXT_DIM italic title — with
    // the account rows.
    assert_eq!(buffer[(master, 1)].style().fg, Some(palette.line));
    assert!(detail_row(buffer, WIDE, 1).starts_with(" ACCOUNT"));
    assert_eq!(buffer[(master + 3, 1)].style().fg, Some(palette.text_dim));
    assert!(buffer[(master + 3, 1)].style().add_modifier.contains(ratatui::style::Modifier::ITALIC));
    assert!(!buffer[(master + 3, 1)].style().add_modifier.contains(ratatui::style::Modifier::BOLD));
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW), "  username  fixture-user");

    press(&mut app, KeyCode::Enter);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    // Descended: the detail pane owns the caret — LINE_STRONG border, bold title — and the
    // caret has left the sections pane for the detail's first row.
    assert_eq!(buffer[(master, 1)].style().fg, Some(palette.line_strong));
    assert!(buffer[(master + 3, 1)].style().add_modifier.contains(ratatui::style::Modifier::BOLD));
    assert_eq!(buffer[(0, 1)].style().fg, Some(palette.line));
    assert_eq!(buffer[(2, FIRST_ROW)].symbol(), " ");
    assert_eq!(buffer[(master + 2, FIRST_ROW)].symbol(), "❯");
    assert_eq!(buffer[(master + 4, FIRST_ROW)].style().fg, Some(palette.text_dim));
    assert!(buffer[(master + 4, FIRST_ROW)].style().add_modifier.contains(ratatui::style::Modifier::BOLD));

    press(&mut app, KeyCode::Esc);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(2, FIRST_ROW)].symbol(), "❯", "esc returns the caret to the sections");
    // A blurred pane keeps its last-selected row's tint but drops the caret (contract: Pane
    // focus — the sections pane does the same through its List highlight).
    assert_eq!(buffer[(master + 2, FIRST_ROW)].symbol(), " ", "the blurred detail drops the caret");
    assert_eq!(buffer[(master + 2, FIRST_ROW)].style().bg, Some(palette.bg_hover), "the blurred detail keeps its selection's tint");
    assert_eq!(buffer[(master + 4, FIRST_ROW)].style().bg, Some(palette.bg_hover), "the tint runs through the label");
    assert_eq!(buffer[(master, 1)].style().fg, Some(palette.line));
}

#[test]
fn the_detail_walk_wraps_and_left_ascends_while_right_is_inert() {
    let dir = export_tree();
    let mut app = app_on_account(dir.path());
    press(&mut app, KeyCode::Enter);
    // The account section has 7 rows; the walk wraps.
    for _ in 0..6 {
        press(&mut app, KeyCode::Down);
    }
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 6), "❯ logins  1", "the caret walks the detail rows");
    press(&mut app, KeyCode::Down);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW), "❯ username  fixture-user", "Down past the last row wraps");
    press(&mut app, KeyCode::Up);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 6), "❯ logins  1", "Up past the first row wraps");
    // Right is inert while descended — it neither ascends nor leaves the tab.
    press(&mut app, KeyCode::Right);
    assert!(app.account().descended(), "→ while descended does not ascend");
    assert_eq!(app.active(), Tab::Account, "→ while descended does not switch tabs");
    // Left ascends.
    press(&mut app, KeyCode::Left);
    assert!(!app.account().descended());
}

// ---- the stats ----

/// Walks the sections pane down `steps` times and returns the rendered frame.
fn draw_section(app: &mut App, steps: usize) -> Terminal<TestBackend> {
    for _ in 0..steps {
        press(app, KeyCode::Down);
    }
    draw(app, WIDE, TALL)
}

#[test]
fn each_section_renders_its_stats() {
    let dir = export_tree();
    let mut app = app_on_account(dir.path());
    let palette = Palette::new(Tier::Full);

    let terminal = draw_section(&mut app, 0);
    let buffer = terminal.backend().buffer();
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 1), "  name  Fixture User");
    assert_eq!(
        detail_row(buffer, WIDE, FIRST_ROW + 2),
        "  created  2019-05-04",
        "the 2019 stamp is past the 30-day flip, so the age rule renders the ISO date"
    );
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 5), "  devices  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 6), "  logins  1");
    assert_eq!(buffer[(sections_panel_width(WIDE) + 13, FIRST_ROW + 2)].style().fg, Some(palette.text));

    let terminal = draw_section(&mut app, 1);
    let buffer = terminal.backend().buffer();
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW), "  friends  2", "the overview's own count, reused");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 1), "  requests sent  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 2), "  blocked  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 3), "  deleted  0");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 4), "  hidden suggestions  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 5), "  ignored  0");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 6), "  pending requests  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 7), "  shortcuts  2");

    let terminal = draw_section(&mut app, 1);
    let buffer = terminal.backend().buffer();
    assert!(detail_row(buffer, WIDE, 1).starts_with(" LOCATION"));
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW), "  frequent locations  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 1), "  latest location  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 2), "  home, school & work  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 3), "  daily top locations  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 4), "  six-day periods  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 5), "  location history  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 6), "  businesses visited  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 7), "  actiomoji info  0");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 8), "  areas visited  1");

    let terminal = draw_section(&mut app, 1);
    let buffer = terminal.backend().buffer();
    assert!(detail_row(buffer, WIDE, 1).starts_with(" STORIES"));
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW), "  posts  2");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 1), "  views  19", "the sum of both stories' views");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 2), "  replies  4", "the sum of both stories' replies");
    assert_eq!(
        detail_row(buffer, WIDE, FIRST_ROW + 3),
        "  friend & public stories  1",
        "the planted caption's list is counted, never rendered"
    );

    let terminal = draw_section(&mut app, 1);
    let buffer = terminal.backend().buffer();
    assert!(detail_row(buffer, WIDE, 1).starts_with(" SUBSCRIPTIONS"));
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW), "  subscriptions  0", "the empty list the observed export holds");
}

// ---- absent and broken files ----

#[test]
fn a_missing_file_renders_absent_rows() {
    let dir = TempDir::new().unwrap();
    let json = dir.path().join(format!("mydata~{EXPORT_ID}/json"));
    fs::create_dir_all(&json).unwrap();
    fs::write(json.join("friends.json"), friends_json()).unwrap();
    fs::write(dir.path().join(format!("mydata~{EXPORT_ID}-2.zip")), b"").unwrap();
    fs::write(dir.path().join(format!("mydata~{EXPORT_ID}-3.zip")), b"").unwrap();
    let mut app = app_on_account(dir.path());
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    for offset in 0..7 {
        assert!(detail_row(buffer, WIDE, FIRST_ROW + offset).ends_with("unknown"), "row {offset} is the absent word");
    }
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 2), "  created  unknown");
}

#[test]
fn a_broken_location_file_renders_absent_rows_without_an_error_surface() {
    let dir = export_tree();
    let json = dir.path().join(format!("mydata~{EXPORT_ID}/json"));
    fs::write(json.join("location_history.json"), "not json {").unwrap();
    let mut app = app_on_account(dir.path());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    // Every location row is the absent word — the read's error is discarded whole.
    for offset in 0..9 {
        assert!(detail_row(buffer, WIDE, FIRST_ROW + offset).ends_with("unknown"), "row {offset} is the absent word");
    }
    // And no error surface: neither the file's name nor the parse error's text reaches a frame.
    let text = frame_text(buffer);
    for forbidden in ["location_history", "not json", "unreadable", "expected", "line 1"] {
        assert!(!text.contains(forbidden), "the frame carries no error bytes ({forbidden:?})");
    }
}

#[test]
fn a_broken_file_of_each_other_section_renders_absent_rows_without_an_error_surface() {
    // The fold the location test pins, for the files it leaves out: friends, stories and
    // user_profile broken in turn — each file's own walk to its section, its own row count.
    for (file, steps, rows) in [("friends.json", 1, 8), ("story_history.json", 3, 4), ("user_profile.json", 4, 1)] {
        let dir = export_tree();
        fs::write(dir.path().join(format!("mydata~{EXPORT_ID}/json/{file}")), "not json {").unwrap();
        let mut app = app_on_account(dir.path());
        for _ in 0..steps {
            press(&mut app, KeyCode::Down);
        }
        let terminal = draw(&mut app, WIDE, TALL);
        let buffer = terminal.backend().buffer();
        for offset in 0..rows {
            assert!(detail_row(buffer, WIDE, FIRST_ROW + offset).ends_with("unknown"), "{file}: row {offset} is the absent word");
        }
        let text = frame_text(buffer);
        for forbidden in [file, "not json", "unreadable", "expected", "line 1"] {
            assert!(!text.contains(forbidden), "{file}: the frame carries no error bytes ({forbidden:?})");
        }
    }
}

#[test]
fn an_unparseable_creation_date_folds_the_account_section_whole() {
    let dir = export_tree();
    let json = dir.path().join(format!("mydata~{EXPORT_ID}/json"));
    // The model's Timestamp parse fails the whole file read (`model.rs`'s `?` on Creation
    // Date): the section folds, and the raw string never renders.
    fs::write(
        json.join("account.json"),
        account_json().replace("\"Creation Date\": \"2019-05-04 10:00:00 UTC\"", "\"Creation Date\": \"not a date\""),
    )
    .unwrap();
    let mut app = app_on_account(dir.path());
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    for offset in 0..7 {
        assert!(detail_row(buffer, WIDE, FIRST_ROW + offset).ends_with("unknown"), "row {offset} is the absent word");
    }
    let text = frame_text(buffer);
    for forbidden in ["not a date", "Creation Date", "expected", "line 1"] {
        assert!(!text.contains(forbidden), "the raw string never reaches a frame ({forbidden:?})");
    }
}

// ---- the privacy rule ----

/// Every planted byte class the screen reads but must never render: the fixture's identity, the
/// registration IP, the coordinate pair, the place name, the business id and name, and the two
/// planted bodies — one in a file the screen reads, one in a file it never reads.
const PRIVACY_PAYLOAD: [&str; 10] = [
    "fixture-user",
    "203.0.113.7",
    "planted-message-body",
    "planted-story-caption",
    "48.858844",
    "2.294351",
    "Latitude",
    "Springfield",
    "biz-0000000000000000000000",
    "planted-business-name",
];

/// Asserts the frame carries none of [`PRIVACY_PAYLOAD`] — minus the fixture's own identity,
/// which the account section legitimately renders as its own username row.
fn assert_no_privacy_payload(buffer: &Buffer, section: usize) {
    let text = frame_text(buffer);
    for forbidden in PRIVACY_PAYLOAD {
        if section == 0 && forbidden == "fixture-user" {
            continue;
        }
        assert!(!text.contains(forbidden), "the frame carries none of the planted privacy payload ({forbidden:?})");
    }
}

#[test]
fn no_identity_no_coordinate_no_ip_and_no_message_body_reach_the_screen() {
    let dir = export_tree();
    let mut app = app_on_account(dir.path());

    // The account section, descended: the fullest read of account.json, which carries the
    // planted registration IP and the fixture identity. The rows prove the file was read and
    // counted; the IP never renders.
    press(&mut app, KeyCode::Enter);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_no_privacy_payload(buffer, 0);
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW), "❯ username  fixture-user", "the account rows prove the file was read");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 5), "  devices  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 6), "  logins  1");

    // The location section, descended: the fullest read of the file that carries the planted
    // coordinate, the place name, and the business id and name.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_no_privacy_payload(buffer, 2);
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW), "❯ frequent locations  1");
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 6), "  businesses visited  1", "the business count proves the file was read");

    // The stories section, descended: the file that carries the planted caption — a body in a
    // file the screen does read.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    let terminal = draw(&mut app, WIDE, TALL);
    let buffer = terminal.backend().buffer();
    assert_no_privacy_payload(buffer, 3);
    assert_eq!(detail_row(buffer, WIDE, FIRST_ROW + 1), "  views  19", "the stories sums prove the file was read");
}

// ---- narrow frames ----

#[test]
fn the_layout_ladder_keeps_two_panels_and_then_the_sections() {
    let dir = export_tree();
    let mut app = app_on_account(dir.path());

    // Side by side at 51: the master pane's clamp-floor 20 cells plus the detail pane's 31,
    // exactly. The username fits (its budget is 27 − 2 − 8 − 2 = 15 cells against a 12-cell
    // fixture value, the truncation fit path); the row that must render whole is "created" with
    // its ISO date.
    let terminal = draw(&mut app, 51, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 1)].symbol(), "╭", "the sections panel opens at the body's left edge");
    assert_eq!(buffer[(sections_panel_width(51), 1)].symbol(), "╭", "the detail panel opens at the master's edge");
    assert_eq!(detail_row(buffer, 51, FIRST_ROW + 2), "  created  2019-05-04", "the timestamp row renders whole at the floor");
    // One cell short: the detail stacks full-width under the sections.
    let terminal = draw(&mut app, 50, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 1)].symbol(), "╭", "sections top border");
    assert_eq!(buffer[(0, 8)].symbol(), "╭", "the detail top border touches the sections' bottom");
    assert_eq!(sections_row(buffer, FIRST_ROW), "❯ account");
    assert_eq!(cell_run(buffer, 2..46, 11), "  created  2019-05-04", "the stacked detail's timestamp row");
    // The stacked floor: 31 cells is the detail pane's whole-or-not-at-all need.
    let terminal = draw(&mut app, 31, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 8)].symbol(), "╭", "31 is the stacked floor");
    // Below the floor: the sections pane alone, still naming the screen's content.
    let terminal = draw(&mut app, 30, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 8)].symbol(), "│", "the sections panel's border runs the body: no detail pane below the stacked floor");
    assert_eq!(sections_row(buffer, FIRST_ROW), "❯ account", "the master-only arm still names the screen");
    // And enter cannot descend into a pane that does not render.
    press(&mut app, KeyCode::Enter);
    assert!(!app.account().descended(), "enter is inert below the stacked floor");
    // 10 cells: even the section names cannot fit — the panel is a box, whole or not at all.
    let terminal = draw(&mut app, 10, TALL);
    let buffer = terminal.backend().buffer();
    assert_eq!(cell_run(buffer, 2..8, FIRST_ROW), "", "the interior stays blank when the names cannot fit");
}

#[test]
fn a_short_detail_scrolls_its_rows() {
    let dir = export_tree();
    let mut app = app_on_account(dir.path());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    // 9 rows tall: the compact shell leaves a 4-row detail. Nine location rows don't fit.
    let terminal = draw(&mut app, WIDE, 9);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(sections_panel_width(WIDE) + 2, 3)].symbol(), "❯", "the caret opens on the first row");
    for _ in 0..5 {
        press(&mut app, KeyCode::Down);
    }
    let terminal = draw(&mut app, WIDE, 9);
    let buffer = terminal.backend().buffer();
    // Selected row 5 of 9 in a 4-row viewport: the list scrolls to keep the selection on
    // screen — the walk reached row 5, the fold row is visible at the pane's top, and the
    // scrollbar thumb paints the interior's right column.
    assert_eq!(detail_row(buffer, WIDE, 3), "  daily top locations  1", "row 3 is the pane's top row now");
    assert_eq!(detail_row(buffer, WIDE, 5), "❯ location history  1", "the walk reached row 5");
    assert_eq!(buffer[(sections_panel_width(WIDE) + 2, 5)].symbol(), "❯", "the caret follows the walk past the fold");
    let thumb: Vec<u16> = (3..7).filter(|&y| buffer[(DETAIL_SCROLLBAR_COLUMN, y)].symbol() == "┃").collect();
    assert!(!thumb.is_empty(), "the scrollbar thumb paints the right column");
}
