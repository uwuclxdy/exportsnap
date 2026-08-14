//! The settings screen (task §6): per-row value and provenance render pins, the write-back
//! contract — `config::write` is the one path, and a restart reads the file state a commit
//! wrote — and the DANGER toast a failed write raises, visible rather than silent.
//!
//! The screen is driven DIRECTLY through `Settings::with_layers` with hand-built layers, never
//! through `App::start`: the startup probe would overwrite `detected_ffmpeg` with the real
//! machine's answer, which is exactly the layer these tests need to control. `App::new` is used
//! only for the app-level wiring the screen cannot show alone — the `q`/`x` suspension, the
//! jump, the tick aging, the hint set, and the shell's toast-on-any-tab.
//!
//! Expected rows are derived from the form's own budgets (tests/chat_media_screen.rs is the
//! twin form): the caret gutter 2, the labels' own lengths, the 25-cell value slot, the
//! ` · word` clause, and the 76-cell interior at width 80. Provenance claims follow decision
//! 66's precedence (flag > config > detection > default), never the effective value.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier};
use tempfile::TempDir;

use exportsnap::app::{App, Tab};
use exportsnap::config::{self, Config};
use exportsnap::export::chat_fix::OverlayMode;
use exportsnap::tui::screens::settings::{Settings, SettingsLayers};
use exportsnap::tui::theme::{Palette, Tier};
use exportsnap::tui::{screens::settings, shell};

/// The all-defaults layers over a scratch config dir: no flags, no file keys, the detection
/// answering `Full` and a detected ffmpeg — the shapes the direct-render pins start from.
fn layers(dir: Option<&Path>) -> SettingsLayers {
    SettingsLayers {
        config_dir: dir.map(Path::to_path_buf),
        cli_out: None,
        cli_tier: None,
        config: Config::default(),
        detected_tier: Tier::Full,
        detected_ffmpeg: Some(PathBuf::from("/detected/ffmpeg")),
    }
}

/// A screen over a scratch config dir, with the `/export` source the output row's default
/// derives from — the same one delivery `App::with_source_environment` makes.
fn settings_in(dir: &TempDir) -> Settings {
    let mut settings = Settings::with_layers(layers(Some(dir.path())));
    settings.set_source(PathBuf::from("/export"));
    settings
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_event(&Event::Key(key(code)));
}

fn press_alt(app: &mut App, code: KeyCode) {
    app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::ALT)));
}

/// A fresh app walked onto the settings tab with `→`, the twin of `tests/shell.rs`'s `on_tab`.
fn on_settings() -> App {
    let mut app = App::new(Tier::Full);
    for _ in 0..=Tab::ALL.len() {
        if app.active() == Tab::Settings {
            return app;
        }
        press(&mut app, KeyCode::Right);
    }
    panic!("could not reach settings");
}

fn draw(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| shell::render(frame, app)).unwrap();
    terminal
}

/// The screen alone, into the whole frame — the panel's form rows land at `y = 1 + row`.
fn draw_screen(settings: &Settings, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| settings::render(frame, &Palette::new(Tier::Full), settings, frame.area())).unwrap();
    terminal
}

fn render_80(settings: &Settings) -> Terminal<TestBackend> {
    draw_screen(settings, 80, 8)
}

fn row(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect()
}

/// A row's expected text: `prefix` plus the padding out to the panel interior width.
fn padded(prefix: &str, fill: usize) -> String {
    format!("{prefix}{}", " ".repeat(fill))
}

/// A full buffer row of the direct render: the panel's border and left padding, the content,
/// and the right padding and border.
fn panel_row(prefix: &str, fill: usize) -> String {
    format!("│ {} │", padded(prefix, fill))
}

// ---- the rows (direct render, deterministic layers) ----

#[test]
fn the_form_renders_every_row_with_value_and_provenance() {
    let dir = TempDir::new().unwrap();
    let config = Config {
        out_dir: Some(PathBuf::from("/file/out")),
        theme: Some(Tier::Full),
        ffmpeg_path: None,
        transcode: None,
        overlay_mode: None,
    };
    let mut settings = Settings::with_layers(SettingsLayers { config, ..layers(Some(dir.path())) });
    settings.set_source(PathBuf::from("/export"));

    let terminal = render_80(&settings);
    let buffer = terminal.backend().buffer();
    assert_eq!(row(buffer, 0), format!("╭─ SETTINGS {}╮", "─".repeat(67)));
    assert_eq!(row(buffer, 1), panel_row(&format!("{} · file", padded("❯ output dir  /file/out", 16)), 30));
    assert_eq!(row(buffer, 2), panel_row("  theme  full  compatible · file", 44));
    assert_eq!(row(buffer, 3), panel_row(&format!("{} · detection", padded("  ffmpeg path  /detected/ffmpeg", 9)), 24));
    assert_eq!(row(buffer, 4), panel_row("  transcode  ─● · default", 51));
    assert_eq!(row(buffer, 5), panel_row("  overlay mode  merged  both  originals · default", 27));

    let palette = Palette::new(Tier::Full);
    // The focused row: the accent caret, the value in TEXT, the clause faint, the tint to the
    // panel's interior edge.
    assert_eq!(buffer[(2, 1)].symbol(), "❯");
    assert_eq!(buffer[(2, 1)].style().fg, Some(palette.accent));
    assert!(buffer[(2, 1)].style().add_modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(16, 1)].style().fg, Some(palette.text));
    assert_eq!(buffer[(42, 1)].symbol(), "·");
    assert_eq!(buffer[(42, 1)].style().fg, Some(palette.text_faint));
    assert_eq!(buffer[(72, 1)].style().bg, Some(palette.bg_hover));
    // The theme cycle: the effective tier is the selected word in accent, the other faint.
    assert_eq!(buffer[(11, 2)].style().fg, Some(palette.accent));
    assert_eq!(buffer[(18, 2)].style().fg, Some(palette.text_faint));
}

#[test]
fn a_flag_overridden_row_shows_the_flag_value_and_reads_as_overridden() {
    let dir = TempDir::new().unwrap();
    // The file ALSO holds values: the flag wins on both rows, and the theme row shows the
    // EFFECTIVE tier (compatible) even though the file says full — the clause is what names
    // the override.
    let config = Config { out_dir: Some(PathBuf::from("/file/out")), theme: Some(Tier::Full), ..Config::default() };
    let mut settings = Settings::with_layers(SettingsLayers {
        cli_out: Some(PathBuf::from("/flag/out")),
        cli_tier: Some(Tier::Compatible),
        config,
        ..layers(Some(dir.path()))
    });
    settings.set_source(PathBuf::from("/export"));

    let terminal = render_80(&settings);
    let buffer = terminal.backend().buffer();
    assert_eq!(row(buffer, 1), panel_row(&format!("{} · flag", padded("❯ output dir  /flag/out", 16)), 30));
    assert_eq!(row(buffer, 2), panel_row("  theme  full  compatible · flag", 44));
    assert_eq!(
        buffer[(18, 2)].style().fg,
        Some(Palette::new(Tier::Full).accent),
        "the effective tier, not the file's, is the selected cycle word"
    );
    // The untouched rows keep their own values and clauses.
    assert_eq!(row(buffer, 3), panel_row(&format!("{} · detection", padded("  ffmpeg path  /detected/ffmpeg", 9)), 24));
    assert_eq!(row(buffer, 4), panel_row("  transcode  ─● · default", 51));
}

#[test]
fn the_ffmpeg_row_with_nothing_reports_not_found_without_a_clause() {
    let dir = TempDir::new().unwrap();
    let mut settings = Settings::with_layers(SettingsLayers { detected_ffmpeg: None, ..layers(Some(dir.path())) });
    settings.set_source(PathBuf::from("/export"));

    let terminal = render_80(&settings);
    let buffer = terminal.backend().buffer();
    assert_eq!(row(buffer, 3), panel_row("  ffmpeg path  not found", 52));
    assert_eq!(
        buffer[(17, 3)].style().fg,
        Some(Palette::new(Tier::Full).accent),
        "the not-found answer reads like any other blurred value"
    );
}

#[test]
fn the_form_stays_blank_below_the_interior_budget() {
    let dir = TempDir::new().unwrap();
    let settings = settings_in(&dir);
    // 52 wide: the interior is 48 cells, under the 53-cell widest-row budget, so the
    // whole-or-not-at-all gate keeps every row blank rather than clipping a value.
    let terminal = draw_screen(&settings, 52, 8);
    let buffer = terminal.backend().buffer();
    for y in 1..7 {
        assert_eq!(row(buffer, y), format!("│{}│", padded("", 50)), "row {y}");
    }
}

// ---- the write-back contract ----

#[test]
fn a_commit_writes_the_file_and_a_restart_reads_it_back() {
    let dir = TempDir::new().unwrap();
    let mut settings = settings_in(&dir);

    // `enter` opens the focused output dir row, the letters land, `enter` commits the draft.
    settings.handle_key(key(KeyCode::Enter));
    for c in ['/', 'n', 'e', 'w'] {
        settings.handle_key(key(KeyCode::Char(c)));
    }
    settings.handle_key(key(KeyCode::Enter));

    // §6 restart-verify: the file holds the commit, and a fresh screen over the loaded config
    // renders the value it wrote, with the clause re-derived to ` · file`.
    let config = config::load(dir.path()).unwrap();
    assert_eq!(config.out_dir.as_deref(), Some(Path::new("/new")));
    let mut restarted = Settings::with_layers(SettingsLayers { config, ..layers(Some(dir.path())) });
    restarted.set_source(PathBuf::from("/export"));
    let expected = panel_row(&format!("{} · file", padded("❯ output dir  /new", 21)), 30);
    assert_eq!(row(render_80(&restarted).backend().buffer(), 1), expected);
}

#[test]
fn an_empty_commit_drops_the_key_and_the_row_shows_the_default() {
    let dir = TempDir::new().unwrap();
    let mut settings = Settings::with_layers(SettingsLayers {
        config: Config { out_dir: Some(PathBuf::from("/file/out")), ..Config::default() },
        ..layers(Some(dir.path()))
    });
    settings.set_source(PathBuf::from("/export"));

    // ctrl+w kills the seeded draft whole; `enter` commits the empty draft, which drops the
    // key rather than writing a path that names nothing.
    settings.handle_key(key(KeyCode::Enter));
    settings.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    settings.handle_key(key(KeyCode::Enter));

    let config = config::load(dir.path()).unwrap();
    assert_eq!(config.out_dir, None, "an empty commit removes the key");
    let mut restarted = Settings::with_layers(SettingsLayers { config, ..layers(Some(dir.path())) });
    restarted.set_source(PathBuf::from("/export"));
    let expected = panel_row(&format!("{} · default", padded("❯ output dir  /export/exportsnap-out", 3)), 27);
    assert_eq!(row(render_80(&restarted).backend().buffer(), 1), expected, "the row falls back to the source-derived default");
}

#[test]
fn a_cycle_commit_writes_the_next_value_through_the_file() {
    let dir = TempDir::new().unwrap();
    let mut settings = settings_in(&dir);

    settings.handle_key(key(KeyCode::Down)); // theme
    settings.handle_key(key(KeyCode::Enter));
    let config = config::load(dir.path()).unwrap();
    assert_eq!(config.theme, Some(Tier::Full.next()), "the theme row wrote the next tier");
    // The row shows the committed tier as selected the same frame — the in-place layer swap.
    let terminal = render_80(&settings);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(18, 2)].style().fg, Some(Palette::new(Tier::Full).accent));
    assert_eq!(row(buffer, 2), panel_row("❯ theme  full  [compatible] · file", 42));

    for _ in 0..3 {
        settings.handle_key(key(KeyCode::Down)); // ffmpeg, transcode, overlay
    }
    settings.handle_key(key(KeyCode::Enter));
    let config = config::load(dir.path()).unwrap();
    assert_eq!(config.overlay_mode, Some(OverlayMode::default().next()), "the overlay row wrote the next mode");
}

#[test]
fn a_flag_pinned_state_row_writes_nothing_on_press() {
    let dir = TempDir::new().unwrap();
    // The flag pins the effective tier to `full`, while the file layer holds `full` too: a
    // press writing the effective successor (`full` -> `compatible`) would flip the file to
    // `compatible`. It must leave the file untouched, and raise no toast for a write that
    // never happened — the row's ` · flag` clause is the only announcement. The file layer
    // is written first and the screen's layer holds the same config: the restart-reads-back
    // contract the commit tests pin.
    let file_config = Config { theme: Some(Tier::Full), ..Config::default() };
    config::write(dir.path(), &file_config).unwrap();
    let mut settings =
        Settings::with_layers(SettingsLayers { cli_tier: Some(Tier::Full), config: file_config, ..layers(Some(dir.path())) });
    settings.set_source(PathBuf::from("/export"));

    settings.handle_key(key(KeyCode::Down)); // theme
    settings.handle_key(key(KeyCode::Enter));
    assert_eq!(config::load(dir.path()).unwrap().theme, Some(Tier::Full), "the press writes nothing past the flag's pin");
    assert!(!settings.toast_live(), "an inert press raises no toast");
    let terminal = render_80(&settings);
    assert_eq!(row(terminal.backend().buffer(), 2), panel_row("❯ theme  [full]  compatible · flag", 42));
}

#[test]
fn the_transcode_toggle_flips_the_file_layer() {
    let dir = TempDir::new().unwrap();
    let mut settings = settings_in(&dir);

    for _ in 0..3 {
        settings.handle_key(key(KeyCode::Down)); // transcode
    }
    settings.handle_key(key(KeyCode::Enter));
    assert_eq!(config::load(dir.path()).unwrap().transcode, Some(false));
    // The row flips to the off knob and the clause moves to ` · file` the same frame.
    let terminal = render_80(&settings);
    let buffer = terminal.backend().buffer();
    assert_eq!(row(buffer, 4), panel_row("❯ transcode  ○─ · file", 54));
    // `space` mirrors `enter` on the state rows: another press writes the flip back.
    settings.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(config::load(dir.path()).unwrap().transcode, Some(true));
}

#[test]
fn an_edit_seeds_the_draft_from_the_file_layer_not_the_effective_value() {
    let dir = TempDir::new().unwrap();
    // The flag overrides the row, but a commit writes the FILE layer: the draft must be the
    // file's value, and committing it rewrites the same key.
    let mut settings = Settings::with_layers(SettingsLayers {
        cli_out: Some(PathBuf::from("/flag/out")),
        config: Config { out_dir: Some(PathBuf::from("/file/out")), ..Config::default() },
        ..layers(Some(dir.path()))
    });
    settings.set_source(PathBuf::from("/export"));

    settings.handle_key(key(KeyCode::Enter));
    let terminal = render_80(&settings);
    let buffer = terminal.backend().buffer();
    assert!(row(buffer, 1).starts_with("│ ✎ output dir  /file/out"), "the draft is the FILE's value, not the flag's");
    assert!(row(buffer, 1).contains(" · flag"), "the clause still names the override while editing");

    settings.handle_key(key(KeyCode::Enter));
    assert_eq!(
        config::load(dir.path()).unwrap().out_dir.as_deref(),
        Some(Path::new("/file/out")),
        "the commit rewrote the file layer's value"
    );
}

#[test]
fn the_caret_wraps_around_the_form() {
    let dir = TempDir::new().unwrap();
    let mut settings = settings_in(&dir);

    // Down five times lands back on the output dir row: `enter` then opens an edit.
    for _ in 0..5 {
        settings.handle_key(key(KeyCode::Down));
    }
    settings.handle_key(key(KeyCode::Enter));
    assert!(settings.is_editing(), "the caret wrapped from the overlay row to the output dir row");
    settings.handle_key(key(KeyCode::Esc));

    // Up from the first row wraps to the last: `enter` acts on the overlay row there.
    settings.handle_key(key(KeyCode::Up));
    settings.handle_key(key(KeyCode::Enter));
    assert_eq!(config::load(dir.path()).unwrap().overlay_mode, Some(OverlayMode::default().next()), "Up wrapped onto the overlay row");
}

// ---- the form below the compact height ----

#[test]
fn the_form_scrolls_with_the_focus_at_nine_rows_high() {
    // 9 terminal rows: the shell's size banner eats one body row (shell.rs), so the panel
    // interior is 4 rows for 5 form rows. The caret must stay on a row the panel draws, and
    // the list slides once the focus walks past the visible span — the contract's
    // scroll-follow, not a clipped caret (cloudy-tui: Text input — the cursor marks the
    // caret, which must sit on a row the panel actually shows).
    let mut app = on_settings();
    let terminal = draw(&mut app, 80, 9);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(2, 3)].symbol(), "❯");
    assert!(row(buffer, 3).contains("output dir"));
    for (label, y) in [("theme", 4), ("ffmpeg path", 5), ("transcode", 6), ("overlay mode", 6)] {
        press(&mut app, KeyCode::Down);
        let terminal = draw(&mut app, 80, 9);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, y)].symbol(), "❯", "the caret sits on the focused row at y {y}");
        assert!(row(buffer, y).contains(label), "the focused {label} row is visible at y {y}");
    }
    // With the focus on the last row the view has scrolled: the first rows are off the panel
    // above, not clipped below — the scroll-follow's visible span.
    let terminal = draw(&mut app, 80, 9);
    let buffer = terminal.backend().buffer();
    assert!(!row(buffer, 3).contains("output dir"), "the first row scrolled off above the view");

    // The native cursor marks the caret on the same scrolled row: at height 9 (offset 0)
    // and height 7 (offset 1 — the view slid for the ffmpeg row) it stays on a row the
    // panel draws, never on one clipped off.
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    let terminal = draw(&mut app, 80, 9);
    assert_eq!(terminal.backend().cursor_position(), Position::new(17, 5), "the native cursor sits on the caret row at height 9");
    let terminal = draw(&mut app, 80, 7);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(2, 4)].symbol(), "✎", "the edit glyph sits on the scrolled row");
    assert!(row(buffer, 4).contains("ffmpeg path"));
    assert_eq!(terminal.backend().cursor_position(), Position::new(17, 4), "the native cursor follows the scroll");
}

#[test]
fn a_wide_char_draft_places_the_cursor_and_tag_in_display_cells() {
    // The caret is a char index, but the native cursor and the draft window are display
    // cells: a 2-cell char moves the cursor one cell further than a char count would, and a
    // window bounded in cells keeps the provenance clause inside the panel (cloudy-tui: the
    // model tracks a character column, the render converts to display cells before placing
    // the native cursor).
    let dir = TempDir::new().unwrap();
    let mut settings = settings_in(&dir);
    settings.handle_key(key(KeyCode::Enter));
    settings.handle_key(key(KeyCode::Char('中')));
    settings.handle_key(key(KeyCode::Char('A')));
    let terminal = render_80(&settings);
    let buffer = terminal.backend().buffer();
    assert_eq!(
        terminal.backend().cursor_position(),
        Position::new(19, 1),
        "the cursor lands 3 cells into the slot — a char count would land at 18"
    );
    // `row` reads each cell's symbol, and a wide char's continuation cell is an empty " ",
    // so the expected row spells the slot "中 A".
    assert_eq!(row(buffer, 1), panel_row(&format!("✎ output dir  中 A{} · default", " ".repeat(22)), 27));

    // 23 more wide chars: the window slides to keep the caret visible, holding at most
    // VALUE_CELLS cells, so the clause stays at its column instead of being pushed past the
    // panel edge.
    for _ in 0..23 {
        settings.handle_key(key(KeyCode::Char('中')));
    }
    let terminal = render_80(&settings);
    let buffer = terminal.backend().buffer();
    assert_eq!(terminal.backend().cursor_position(), Position::new(40, 1), "the caret stays at the last cell of the slid window");
    assert_eq!(buffer[(42, 1)].symbol(), "·", "the provenance clause stays inside the interior");
    // The window holds 12 wide chars, not 24 — a char-bounded window would spill 47 cells
    // and push the clause past the panel edge.
    assert_eq!(row(buffer, 1), panel_row(&format!("✎ output dir  {}{} · default", "中 ".repeat(12), " "), 27));

    // A mid-draft caret is where the window's cell cap binds: 25 chars of wide text after
    // the cut would spill 50 cells, so the window must hold 12 wide chars (24 cells) plus
    // one pad cell, and the clause stays at its column.
    let mut settings = settings_in(&dir);
    settings.handle_key(key(KeyCode::Enter));
    for _ in 0..40 {
        settings.handle_key(key(KeyCode::Char('中')));
    }
    for _ in 0..13 {
        settings.handle_key(key(KeyCode::Left));
    }
    let terminal = render_80(&settings);
    let buffer = terminal.backend().buffer();
    assert_eq!(
        terminal.backend().cursor_position(),
        Position::new(40, 1),
        "the caret keeps its display-cell offset within the mid-draft window"
    );
    assert_eq!(buffer[(42, 1)].symbol(), "·", "the clause stays put with wide chars still in the draft");
    assert_eq!(row(buffer, 1), panel_row(&format!("✎ output dir  {}{} · default", "中 ".repeat(12), " "), 27));
}

// ---- the DANGER toast ----

#[test]
fn a_failed_write_raises_a_danger_toast_that_x_dismisses() {
    let mut app = on_settings();
    press(&mut app, KeyCode::Down); // theme
    press(&mut app, KeyCode::Enter); // commit_cycle: no config dir to write to
    assert!(app.settings().toast_live());

    // The toast over the finished frame: the bar in DANGER at the 2-cell inset, the title
    // TEXT + bold, the reason TEXT_DIM, the glass over whatever each cell sat on.
    let terminal = draw(&mut app, 80, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);
    assert_eq!(buffer[(18, 2)].symbol(), "┃");
    assert_eq!(buffer[(18, 3)].symbol(), "┃");
    assert_eq!(buffer[(18, 2)].style().fg, Some(palette.danger));
    assert_eq!(buffer[(20, 2)].symbol(), "c", "the title");
    assert_eq!(buffer[(20, 2)].style().fg, Some(palette.text));
    assert!(buffer[(20, 2)].style().add_modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(20, 3)].symbol(), "n", "the reason");
    assert_eq!(buffer[(20, 3)].style().fg, Some(palette.text_dim));
    assert!(!buffer[(20, 3)].style().add_modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(76, 3)].symbol(), "…", "the capped reason truncates");
    // The glass over a blurred row's reset cell blends to the BG fallback; over the focused
    // theme row's tint it blends with the hover wash.
    assert_eq!(buffer[(20, 2)].style().bg, Some(Color::Rgb(20, 20, 32)));
    assert_eq!(buffer[(20, 3)].style().bg, Some(Color::Rgb(23, 23, 34)));
    assert_eq!(buffer[(77, 2)].symbol(), " ", "the padding cell");
    assert_eq!(buffer[(77, 2)].style().bg, Some(Color::Rgb(20, 20, 32)));

    press(&mut app, KeyCode::Char('x'));
    assert!(!app.settings().toast_live(), "x dismisses the toast");
    let terminal = draw(&mut app, 80, 20);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(20, 2)].symbol(), "r", "the covered cell reads again");
}

#[test]
fn a_write_failure_names_the_file_and_the_fix() {
    let dir = TempDir::new().unwrap();
    // A FILE standing where the config dir should be: create_private_dir fails on it, and the
    // error names the file so the toast says what to fix.
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"a file where the config dir should be").unwrap();
    let message = config::write(&blocked, &Config::default()).unwrap_err().to_string();
    assert!(message.starts_with("could not write config"), "{message}");
    assert!(message.contains("blocked"), "{message}");
}

#[test]
fn a_successful_write_clears_the_failure_toast() {
    let dir = TempDir::new().unwrap();
    // A FILE standing where the config dir should be: the first commit fails and raises the
    // DANGER toast. Clearing the obstruction, the same commit succeeds — and the success is
    // the failure's cure, so the toast must go with the cause that raised it, not linger
    // until the next failure or dismissal.
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"a file where the config dir should be").unwrap();
    let mut settings = Settings::with_layers(layers(Some(&blocked)));
    settings.set_source(PathBuf::from("/export"));

    settings.handle_key(key(KeyCode::Down)); // theme
    settings.handle_key(key(KeyCode::Enter));
    assert!(settings.toast_live(), "the failed write raises the toast");

    std::fs::remove_file(&blocked).unwrap();
    settings.handle_key(key(KeyCode::Enter));
    assert!(!settings.toast_live(), "the successful write resolves the toast");
    assert_eq!(config::load(&blocked).unwrap().theme, Some(Tier::Full.next()), "the second commit really wrote");
}

#[test]
fn the_toast_ages_out_after_six_seconds_of_ticks() {
    let mut app = on_settings();
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(app.settings().toast_live());

    for _ in 0..74 {
        app.tick();
    }
    assert!(app.settings().toast_live(), "74 of the 75 ticks keep it live");
    app.tick();
    assert!(!app.settings().toast_live(), "the 75th tick ends the 6-second DANGER lifetime");
}

#[test]
fn the_toast_floats_over_the_other_tabs() {
    let mut app = on_settings();
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Left); // settings → account
    assert_eq!(app.active(), Tab::Account);

    let terminal = draw(&mut app, 80, 20);
    assert_eq!(terminal.backend().buffer()[(18, 2)].symbol(), "┃", "the toast floats over another tab's frame");
}

// ---- the app-level wiring ----

#[test]
fn q_types_a_letter_while_editing_and_never_arms_the_quit() {
    let mut app = on_settings();
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.settings().is_editing(), "q is a letter while the input is being edited");
    assert!(!app.is_quit_armed());
    let terminal = draw(&mut app, 80, 20);
    assert!(row(terminal.backend().buffer(), 2).starts_with("│ ✎ output dir  q"), "the suspended q typed into the draft");

    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed(), "q arms the 2-step quit once the edit ends");
    press(&mut app, KeyCode::Char('q'));
    assert!(!app.is_running(), "the second q quits");
}

#[test]
fn x_types_a_letter_while_editing_and_dismisses_afterwards() {
    let mut app = on_settings();
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter); // cycle commit → toast
    assert!(app.settings().toast_live());
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter); // edit
    press(&mut app, KeyCode::Char('x'));
    assert!(app.settings().toast_live(), "x types while editing");
    assert!(app.settings().is_editing());

    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('x'));
    assert!(!app.settings().toast_live(), "x dismisses once the edit ends");
}

#[test]
fn a_jump_away_and_back_preserves_the_edit_session() {
    let mut app = on_settings();
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('a'));
    press_alt(&mut app, KeyCode::Char('1'));
    assert_eq!(app.active(), Tab::Overview);
    assert!(app.settings().is_editing(), "a jump away does not cancel the edit");

    press_alt(&mut app, KeyCode::Char('6'));
    assert_eq!(app.active(), Tab::Settings);
    assert!(app.settings().is_editing(), "the session survives the round trip");
    let terminal = draw(&mut app, 80, 20);
    assert!(row(terminal.backend().buffer(), 2).starts_with("│ ✎ output dir  a"), "the draft survived the round trip");
}

#[test]
fn the_settings_edit_hints_replace_the_switch_set_while_editing() {
    let mut app = on_settings();
    assert_eq!(row(draw(&mut app, 80, 20).backend().buffer(), 19), padded(" ←→ switch   q quit", 61));

    press(&mut app, KeyCode::Enter);
    assert_eq!(row(draw(&mut app, 80, 20).backend().buffer(), 19), padded(" ←→ move   ↵ commit   esc cancel", 48));
}
