//! Render tests for the app shell: header row, body panel, footer row, compact banner.
//!
//! Every expectation is cross-checked against the cloudy-tui skill's App shell, Tab bar,
//! Panel, Hint bar, Footer alert, Banner and Patterns → Density sections, not against this
//! crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use exportsnap::app::{App, Tab};
use exportsnap::tui::theme::{Palette, Tier};
use exportsnap::tui::{header, shell};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier};
use ratatui::text::Span;

/// ` exportsnap` (11 cells) + `  •  ` (5 cells) — where the first tab's word starts: the active
/// cue is styling alone (accent + bold + underline), so there is no glyph prefix.
const FIRST_TAB_COLUMN: u16 = 16;
/// The active first tab's word, at [`FIRST_TAB_COLUMN`] now that the active cue carries no glyph.
const ACTIVE_LABEL_COLUMN: u16 = FIRST_TAB_COLUMN;
/// The inactive second tab's word when the first tab is active: the active first tab is
/// `overview` (8 cells) and the gap is 3.
const INACTIVE_SECOND_COLUMN: u16 = FIRST_TAB_COLUMN + 8 + 3;
/// `╭` + the border-token dash the panel title carries — where ` OVERVIEW ` starts.
const PANEL_TITLE_COLUMN: u16 = 2;

fn draw(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| shell::render(frame, app)).unwrap();
    terminal
}

fn row(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect()
}

/// Exact symbol grid. `TestBackend::assert_buffer_lines` builds its expected buffer from
/// unstyled lines and then compares whole cells, so it can only be fed a frame that carries no
/// styling at all — every color the shell paints makes it fail. Layout drift is asserted here
/// on the exact grid; the styles have their own exact-value tests below.
fn grid(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height).map(|y| row(buffer, y)).collect()
}

fn header_row(app: &mut App, width: u16) -> String {
    let terminal = draw(app, width, 20);
    row(terminal.backend().buffer(), 0)
}

/// The header alone, at a version string fixed here rather than taken from the crate, so the
/// suppression-ladder widths below stay pinned to literals when the crate version moves.
/// `v9.9.9 ` is the same 7 cells the crate's own version currently occupies.
fn header_only(active: Tab, width: u16) -> String {
    header_only_held(active, width, false)
}

/// [`header_only`] with the jump-key overlay on (`⌥` held).
fn header_only_alt(active: Tab, width: u16) -> String {
    header_only_held(active, width, true)
}

fn header_only_held(active: Tab, width: u16, alt_held: bool) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(header::render(&Palette::new(Tier::Full), active, "9.9.9", width, alt_held, &[]), frame.area());
        })
        .unwrap();
    row(terminal.backend().buffer(), 0)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

/// A fresh app on `tab`, walked there with `→`. [`on_tab_in`] carries the bound and the reason
/// for it.
///
/// The walk arrives at every tab: no screen consumes `→` at top level. The history picker
/// descends with `enter` (`src/tui/screens/history.rs`'s picker keys), so `→` stays the shell's
/// tab key there like everywhere else.
fn on_tab(tab: Tab) -> App {
    let mut app = App::new(Tier::Full);
    on_tab_in(&mut app, tab);
    app
}

/// Walks `app` to `tab` with `→`, bounded by the tab count — the twin of `tests/chat_media_screen.rs`'s `on_tab` and `tests/memories_screen.rs`'s `on_memories`.
///
/// The bound is not decoration. `→` is INERT while a pane is descended: `Memories` and `ChatMedia` consume it and answer `true` (`src/tui/screens/memories.rs:431`, `src/tui/screens/chat_media.rs:472`), and the history formats pane does the same — so the shell's own `Right` arm never runs once any pane is descended and an unbounded walk from a descended app spins forever. That is the screen behaving correctly and the helper behaving badly, and this crate configures no nextest `terminate-after`, so the result is not a slow test but a wedged suite with no failing assertion to read. Until 2026-08-11 this helper was the unbounded `while` its two twins were written to warn about.
///
/// **Only one caller in this file reaches that state, and it does so on purpose.** [`on_tab`] and the tier loop in `the_panel_border_and_title_follow_the_tier_too` both build an `App::new` sitting on `Overview` with no run planned, `descended()` is `false` for all six tabs there (`src/app.rs:252`), and `Tab::next` wraps (`src/app.rs:66`), so every tab is at most five presses away and neither can trip the bound. The third caller is [`walking_off_a_descended_pane_panics_instead_of_spinning`], which descends a pane deliberately in order to trip it. Enumerate the callers before trusting that split — it has already gone stale once, when the pin below was added and this paragraph still said there were two.
///
/// **Both directions are pinned, at this file's own guard and against this file's own literal.** Termination is structural: a `for` over a finite range cannot spin, so no test adds confidence there. The range being too SMALL is the half that rots silently, and emptying it reds the walks loudly — 15 of this file's 38 tests at the 2026-08-11 measurement, naming four different origin/target pairs, since the literal carries both. Even the zero-distance walk reds: `if app.active() == tab` sits INSIDE the loop, so an empty range never reaches it. That property is the thing to re-derive; the count is not, because it moves with every test this file gains — it read 14 one revision ago, before the pin below existed. Re-measure rather than trusting either number, and note the twin in `tests/memories_screen.rs` does NOT behave this way: its pin survives its own emptied range, for the reason recorded there. The literal itself is pinned by [`walking_off_a_descended_pane_panics_instead_of_spinning`] below, which builds the descended pane this file's ordinary callers never produce. Each of the three guards carries its own pin and none stands for the others: `tests/chat_media_screen.rs`'s twin spells the same bytes by coincidence and `tests/memories_screen.rs`'s differs outright, so a drift in one is invisible to the other two.
fn on_tab_in(app: &mut App, tab: Tab) {
    for _ in 0..=Tab::ALL.len() {
        if app.active() == tab {
            return;
        }
        press(app, KeyCode::Right);
    }
    panic!("could not reach {tab:?} from {:?}: is a pane descended and trapping the arrows?", app.active());
}

/// [`on_tab_in`]'s panic arm: a walk off a descended pane gives up with a diagnosis instead of spinning.
///
/// The state no ordinary caller here produces, built on purpose. Every import is function-local, so nothing chat-media enters this file's surface — the shell is what these tests are about, and a descended pane is only the state the guard needs. It is reachable in a plan with no rows at all: `with_channel` sets `Run::Active` (`src/tui/screens/chat_media.rs:257`) and `plan_landed` fills the view regardless of row count (`:413`), so `has_table` is true and `tab` descends.
///
/// **`should_panic` on the WHOLE message, not a fragment.** The `on_tab_in(&mut app, Tab::ChatMedia)` walk in the setup would itself panic as `could not reach ChatMedia from …` if it ever failed, which a fragment like `is a pane descended and trapping the arrows?` would happily accept — the full literal is what keeps a setup failure from passing as the subject. Deleting the bound makes this test HANG rather than red, which is unavoidable rather than sloppy: the property under test is "does not hang", and nothing short of a timeout harness can red on its absence.
///
/// **What it reds on is narrower than "the literal drifting", so do not lean on it for more.** `should_panic` matches by CONTAINMENT, so only an edit INSIDE the expected substring reds; text added around the literal — a prefix, a suffix, an extra leading clause — leaves it green. Measured 2026-08-11: prefixing this literal left the whole suite green, while `arrows?` → `keys?` reds exactly this pin and nothing else. The full-literal choice above defeats a too-loose fragment match; it does not make the match exact, and those are the two independent directions of the same mechanic.
#[test]
#[should_panic(expected = "could not reach Memories from ChatMedia: is a pane descended and trapping the arrows?")]
fn walking_off_a_descended_pane_panics_instead_of_spinning() {
    use exportsnap::export::chat_run;
    use exportsnap::export::manifest::ExportId;
    use std::sync::mpsc;
    use tempfile::TempDir;

    let state = TempDir::new().unwrap();
    let mut app = App::new(Tier::Full);
    let (sender, receiver) = mpsc::channel();
    sender
        .send(chat_run::RunEvent::Planned(chat_run::PlanSnapshot {
            // Any id `ExportId::new` accepts (`src/export/manifest.rs:187`) — nothing here reads it
            // back, so it is spelled as the fixture it is rather than as a copy of the real export
            // id that no grep would ever reach.
            export_id: ExportId::new("shell-pin").unwrap(),
            manifest_dir: state.path().to_path_buf(),
            rows: Vec::new(),
            counts: chat_run::PlanCounts::default(),
        }))
        .unwrap();
    app.with_chat_media_channel(receiver);
    app.tick();

    // The trap is the whole fixture, so it is asserted rather than assumed. An app that failed to
    // descend would reach memories in five presses and the test would fail as "no panic", which
    // reads like a missing guard instead of a broken fixture.
    on_tab_in(&mut app, Tab::ChatMedia);
    // Descend the chat pane via `tab` — a pane key, so no caret walk onto the chip is needed.
    press(&mut app, KeyCode::Tab);
    assert!(app.chat_media().descended(), "the fixture must leave the pane descended, or the walk below is not trapped");

    // `sender` lives to end of scope, which is what keeps the channel connected across the tick
    // above; there is deliberately no `drop` after the walk, since the walk never returns.
    on_tab_in(&mut app, Tab::Memories);
}

// ---- whole frame ----

#[test]
fn renders_header_body_panel_and_hint_bar() {
    // Driven on `settings` rather than `overview`, `memories`, `chat media`, `history` or
    // `account`: those five own their own screens now, and the settings form is the sixth. At
    // 52 cells the panel interior is 48, under the form's 53-cell budget, so the
    // whole-or-not-at-all gate keeps the interior blank and this grid is the shell's pin.
    let terminal = draw(&mut on_tab(Tab::Settings), 52, 6);
    assert_eq!(
        grid(terminal.backend().buffer()),
        [
            " exportsnap  •  ‹   settings                        ",
            " ! terminal too small · enlarge for full layout     ",
            "╭─ SETTINGS ───────────────────────────────────────╮",
            "│                                                  │",
            "╰──────────────────────────────────────────────────╯",
            " ←→ switch   ? help   q quit                        ",
        ]
    );
}

// ---- header: tab labels + active styling (skill: Tab bar) ----

#[test]
fn header_renders_the_brand_all_six_tab_labels_and_the_version() {
    assert_eq!(
        header_row(&mut App::new(Tier::Full), 100),
        format!(
            " exportsnap  •  overview   memories   chat media   history   account   settings\
              {:>21}",
            format!("v{} ", env!("CARGO_PKG_VERSION"))
        )
    );
}

#[test]
fn active_tab_label_is_accent_bold_underlined_and_inactive_is_dim() {
    let terminal = draw(&mut App::new(Tier::Full), 100, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // The active cue is styling alone: the word starts at its own column and carries accent +
    // bold + underline, with no glyph in front.
    assert_eq!(buffer[(ACTIVE_LABEL_COLUMN, 0)].symbol(), "o", "no glyph prefix — the word starts at its own column");
    let active = buffer[(ACTIVE_LABEL_COLUMN, 0)].style();
    assert_eq!(active.fg, Some(palette.accent));
    assert!(active.add_modifier.contains(Modifier::BOLD));
    assert!(active.add_modifier.contains(Modifier::UNDERLINED));

    let inactive = buffer[(INACTIVE_SECOND_COLUMN, 0)].style();
    assert_eq!(inactive.fg, Some(palette.text_dim));
    assert!(!inactive.add_modifier.contains(Modifier::BOLD));
    assert!(!inactive.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn an_inactive_tab_with_activity_takes_the_activity_color_and_the_active_ignores_it() {
    use exportsnap::tui::alert::TabActivity;
    let palette = Palette::new(Tier::Full);
    let mut activity = [None; Tab::ALL.len()];
    activity[2] = Some(TabActivity::Success);

    let mut terminal = Terminal::new(TestBackend::new(120, 1)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(header::render(&palette, Tab::Overview, "9.9.9", 120, false, &activity), frame.area());
        })
        .unwrap();
    let buffer = terminal.backend().buffer();

    // "chat media" is the third tab: memories (8 cells) plus a 3-cell gap past the inactive second
    // tab. Its label takes the activity color — no underline rule beneath it (cloudy-tui: Tab bar →
    // Tab activity).
    let chat = INACTIVE_SECOND_COLUMN + 8 + 3;
    for x in chat..chat + 10 {
        assert_eq!(buffer[(x, 0)].style().fg, Some(palette.success), "success activity on cell ({x}, 0)");
        assert!(!buffer[(x, 0)].style().add_modifier.contains(Modifier::UNDERLINED), "no underline rule beneath an activity label");
        assert!(!buffer[(x, 0)].style().add_modifier.contains(Modifier::BOLD), "activity is color, not weight");
    }
    // The active tab keeps accent + underline, ignoring any activity.
    for x in ACTIVE_LABEL_COLUMN..ACTIVE_LABEL_COLUMN + 8 {
        assert_eq!(buffer[(x, 0)].style().fg, Some(palette.accent), "active label keeps accent");
    }
}

#[test]
fn an_inactive_tab_with_warning_activity_takes_the_warning_color() {
    use exportsnap::tui::alert::TabActivity;
    let palette = Palette::new(Tier::Full);
    let mut activity = [None; Tab::ALL.len()];
    activity[2] = Some(TabActivity::Warning);

    let mut terminal = Terminal::new(TestBackend::new(120, 1)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(header::render(&palette, Tab::Overview, "9.9.9", 120, false, &activity), frame.area());
        })
        .unwrap();
    let buffer = terminal.backend().buffer();

    // "chat media" is the third tab, the same column the success-activity pin above reads.
    let chat = INACTIVE_SECOND_COLUMN + 8 + 3;
    assert_eq!(buffer[(chat, 0)].style().fg, Some(palette.warning), "warning activity takes the warning color");
    assert!(!buffer[(chat, 0)].style().add_modifier.contains(Modifier::UNDERLINED), "no underline rule beneath an activity label");
}

#[test]
fn the_underline_moves_with_the_active_tab() {
    // Overview is now inactive (bare word, 8 cells), so the active memories tab's word starts
    // 3 cells past it — the active cue is styling alone, the word itself carries the underline.
    let terminal = draw(&mut on_tab(Tab::Memories), 100, 20);
    let buffer = terminal.backend().buffer();

    assert!(!buffer[(FIRST_TAB_COLUMN, 0)].style().add_modifier.contains(Modifier::UNDERLINED), "overview is now inactive");
    assert!(
        !buffer[(FIRST_TAB_COLUMN + 8 + 3 - 1, 0)].style().add_modifier.contains(Modifier::UNDERLINED),
        "the gap before memories carries no underline"
    );
    assert!(buffer[(FIRST_TAB_COLUMN + 8 + 3, 0)].style().add_modifier.contains(Modifier::UNDERLINED), "the memories word is underlined");
}

#[test]
fn no_underline_row_sits_beneath_the_tab_bar() {
    // The active label carries the underline as a text attribute; row 1 is already the panel.
    let terminal = draw(&mut on_tab(Tab::Settings), 100, 20);
    assert!(row(terminal.backend().buffer(), 1).starts_with("╭─ SETTINGS "));
}

// ---- header: right-edge suppression (skill: App shell → right-edge suppression priority) ----

#[test]
fn version_keeps_a_three_cell_gap_at_its_narrowest_fitting_width() {
    assert_eq!(header_only(Tab::Overview, 89), " exportsnap  •  overview   memories   chat media   history   account   settings   v9.9.9 ");
}

#[test]
fn version_drops_one_cell_before_it_would_crowd_the_last_tab() {
    assert_eq!(header_only(Tab::Overview, 88), " exportsnap  •  overview   memories   chat media   history   account   settings         ");
}

#[test]
fn tabs_survive_at_the_exact_width_the_full_strip_needs() {
    // Version long gone, every label still present: tabs never drop for the version's sake.
    assert_eq!(header_only(Tab::Overview, 79), " exportsnap  •  overview   memories   chat media   history   account   settings");
}

#[test]
fn tabs_collapse_to_the_overflow_form_one_cell_narrower() {
    assert_eq!(header_only(Tab::Overview, 78), format!("{}{}", " exportsnap  •      overview   ›", " ".repeat(46)));
}

#[test]
fn overflow_markers_track_which_neighbours_exist() {
    // A middle tab has both neighbours; the first and last each lose one marker, and the
    // marker's cell stays blank so the active label holds its column.
    assert_eq!(header_row(&mut on_tab(Tab::ChatMedia), 50), " exportsnap  •  ‹   chat media   ›                ");
    assert_eq!(header_row(&mut on_tab(Tab::Overview), 50), " exportsnap  •      overview   ›                  ");
    assert_eq!(header_row(&mut on_tab(Tab::Settings), 50), " exportsnap  •  ‹   settings                      ");
}

// ---- header: width floor (skill: Tab bar → Overflow; Patterns → Density) ----

#[test]
fn the_header_floor_is_the_widest_tab_label_plus_its_overflow_chrome() {
    // The derivation, spelled out so a longer tab label reds here first: 16 lead + the `‹`
    // marker + the 3-cell gap = 20 cells of chrome left of the active label, and the widest
    // active label is `chat media` at 10 — the active cue is styling alone, so no glyph adds
    // cells. A new label past 10 cells moves the floor, and with it every literal row below.
    //
    // Measured in cells, the unit the floor is built from — a char count would name the wrong
    // label the first time one carries a wide character.
    let widest = Tab::ALL.into_iter().max_by_key(|tab| Span::raw(tab.label()).width()).unwrap();

    assert_eq!(widest.label(), "chat media");
    assert_eq!(Span::raw(widest.label()).width(), 10);
    assert_eq!(header::min_width(), 30);
}

#[test]
fn every_active_label_renders_whole_at_the_floor() {
    // Exactly at the floor the trailing `   ›` run is the only thing the clip takes — the
    // active label, brand and leading marker all survive whole, for the widest label and the
    // narrowest alike.
    // Compared as one batch so a mutation shows every tab it broke, not just the first.
    let rendered: Vec<String> = Tab::ALL.into_iter().map(|tab| header_row(&mut on_tab(tab), 30)).collect();

    assert_eq!(
        rendered,
        [
            " exportsnap  •      overview  ",
            " exportsnap  •  ‹   memories  ",
            " exportsnap  •  ‹   chat media",
            " exportsnap  •  ‹   history   ",
            " exportsnap  •  ‹   account   ",
            " exportsnap  •  ‹   settings  ",
        ]
    );
}

#[test]
fn the_banner_takes_the_header_row_one_cell_below_the_floor() {
    // A clipped active label would name the wrong tab, so the row says the terminal is too
    // small instead. The body is untouched — the panel still owns row 1.
    let terminal = draw(&mut on_tab(Tab::Settings), 29, 20);
    let buffer = terminal.backend().buffer();

    assert_eq!(row(buffer, 0), " ! terminal too small · enla…");
    assert!(row(buffer, 1).starts_with("╭─ SETTINGS "), "{:?}", row(buffer, 1));
}

#[test]
fn the_header_banner_tints_its_whole_row() {
    // Same full-width wash the body banner carries: this is the banner, not a header that
    // happens to spell the copy.
    let terminal = draw(&mut App::new(Tier::Full), 29, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    for x in 0..29 {
        let style = buffer[(x, 0)].style();
        assert_eq!(style.bg, Some(palette.warning), "column {x}");
        assert_eq!(style.fg, Some(palette.contrast_text()), "column {x}");
    }
}

#[test]
fn a_frame_under_both_floors_carries_exactly_one_banner() {
    // Width below the header floor and height below the compact floor at once. The header row
    // is the one already lost, so it says it, and the body keeps every row it has.
    let terminal = draw(&mut on_tab(Tab::Settings), 29, 13);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // The wash is the banner's tell, so counting the rows that carry it counts the banners.
    let washed: Vec<u16> = (0..13).filter(|&y| buffer[(0, y)].style().bg == Some(palette.warning)).collect();
    assert_eq!(washed, [0], "rows carrying the banner wash");

    assert_eq!(row(buffer, 0), " ! terminal too small · enla…");
    assert!(row(buffer, 1).starts_with("╭─ SETTINGS "), "{:?}", row(buffer, 1));
}

#[test]
fn the_height_banner_still_takes_the_body_at_the_width_floor() {
    // At the width floor the two floors stop overlapping: the header renders for real and the
    // compact banner goes back to the top of the body where the contract puts it.
    let terminal = draw(&mut App::new(Tier::Full), 30, 13);
    let buffer = terminal.backend().buffer();

    assert_eq!(row(buffer, 0), " exportsnap  •      overview  ");
    assert_eq!(row(buffer, 1), " ! terminal too small · enlar…");
}

#[test]
fn overflow_markers_are_text_faint() {
    let terminal = draw(&mut on_tab(Tab::ChatMedia), 50, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    assert_eq!(buffer[(FIRST_TAB_COLUMN, 0)].symbol(), "‹");
    assert_eq!(buffer[(FIRST_TAB_COLUMN, 0)].style().fg, Some(palette.text_faint));
}

// ---- header: jump-key overlay (skill: Tab bar → Jump-key overlay) ----

#[test]
fn the_jump_index_overlay_renders_while_alt_is_held() {
    // Every tab gains a bracketed index flush against its label, the active tab's underline
    // intact.
    assert_eq!(
        header_only_alt(Tab::Overview, 107),
        " exportsnap  •  [1]overview   [2]memories   [3]chat media   [4]history   [5]account   [6]settings   v9.9.9 "
    );
}

#[test]
fn the_overlay_index_is_brackets_dim_and_digit_accent_bold() {
    let mut terminal = Terminal::new(TestBackend::new(109, 1)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(header::render(&Palette::new(Tier::Full), Tab::Overview, "9.9.9", 109, true, &[]), frame.area());
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // The active tab is `[1]overview`: the `[1]` sits flush against the label, past the lead.
    assert_eq!(buffer[(FIRST_TAB_COLUMN, 0)].symbol(), "[");
    assert_eq!(buffer[(FIRST_TAB_COLUMN, 0)].style().fg, Some(palette.text_dim));

    assert_eq!(buffer[(FIRST_TAB_COLUMN + 1, 0)].symbol(), "1");
    assert_eq!(buffer[(FIRST_TAB_COLUMN + 1, 0)].style().fg, Some(palette.accent));
    assert!(buffer[(FIRST_TAB_COLUMN + 1, 0)].style().add_modifier.contains(Modifier::BOLD));

    assert_eq!(buffer[(FIRST_TAB_COLUMN + 2, 0)].symbol(), "]");
    assert_eq!(buffer[(FIRST_TAB_COLUMN + 2, 0)].style().fg, Some(palette.text_dim));
}

#[test]
fn the_overlay_drops_before_the_version_when_the_row_runs_short() {
    // The overlay strip alone needs 99 cells; at 91 the plain strip plus the version still fits,
    // so holding `⌥` changes nothing here — the overlay is the first thing dropped, before the
    // version and long before the overflow form.
    assert_eq!(
        header_only_alt(Tab::Overview, 89),
        " exportsnap  •  overview   memories   chat media   history   account   settings   v9.9.9 "
    );
}

#[test]
fn the_overlay_drops_before_the_version_even_where_it_alone_would_fit() {
    // At 97 cells the indexed strip alone fits exactly (lead 16 + the six `[N]`-prefixed labels
    // and their gaps, 81), so a ladder that dropped the version to keep the overlay would return
    // the indexed strip here. The overlay is the first thing dropped, so holding `⌥` renders the
    // plain strip plus the version, byte-identical to no `⌥` held: a transient hint never triggers
    // a layout collapse.
    assert_eq!(
        header_only_alt(Tab::Overview, 97),
        format!("{}{:>18}", " exportsnap  •  overview   memories   chat media   history   account   settings", "v9.9.9 ")
    );
}

#[test]
fn an_alt_press_flows_through_shell_render_into_the_jump_index_overlay() {
    use ratatui::crossterm::event::ModifierKeyCode;

    // Every overlay test above calls `header::render` directly, so hardcoding the `app.alt_held()`
    // argument in `shell::render` to `false` leaves them all green. This drives a real ⌥ press
    // through `App::handle_event` and reads the overlay off the composed frame, so that mutation
    // reds here.
    let mut app = App::new(Tier::Full);
    app.handle_event(&Event::Key(KeyEvent::new(KeyCode::Modifier(ModifierKeyCode::LeftAlt), KeyModifiers::NONE)));
    assert!(app.alt_held(), "the press must land as the held modifier before the frame draws");

    let mut terminal = Terminal::new(TestBackend::new(109, 20)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    let header = row(terminal.backend().buffer(), 0);

    assert!(header.contains("[1]overview"), "the overlay reaches the header through the shell: {header}");
    assert!(header.contains("[6]settings"), "the last tab is indexed too: {header}");
}

// ---- body panel (skill: Panel) ----

#[test]
fn the_panel_title_names_the_active_tab_in_uppercase() {
    // `overview`, `memories`, `chat media`, `history` and `account` are absent on purpose: those
    // five own real screens now, and their own panel titles are pinned in `tests/overview.rs`,
    // `tests/memories_screen.rs`, `tests/chat_media_screen.rs`, `tests/history_screen.rs` and
    // `tests/account_screen.rs`. `settings` names its own panel title the same way, off the same
    // lowercase label — this is the sixth and last case, so it is worth checking it still matches
    // the `match` in `shell::render`.
    let terminal = draw(&mut on_tab(Tab::Settings), 60, 20);
    assert!(row(terminal.backend().buffer(), 1).starts_with("╭─ SETTINGS ─"), "settings should render its own panel title");
}

#[test]
fn the_sole_panel_title_is_accent_2_bold_italic_on_a_line_strong_border() {
    let terminal = draw(&mut on_tab(Tab::Settings), 60, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    let title = buffer[(PANEL_TITLE_COLUMN + 1, 1)].style();
    assert_eq!(buffer[(PANEL_TITLE_COLUMN + 1, 1)].symbol(), "S");
    assert_eq!(title.fg, Some(palette.accent_2));
    assert!(title.add_modifier.contains(Modifier::BOLD));
    assert!(title.add_modifier.contains(Modifier::ITALIC));

    let corner = buffer[(0, 1)].style();
    assert_eq!(buffer[(0, 1)].symbol(), "╭");
    assert_eq!(corner.fg, Some(palette.line_strong));
}

#[test]
fn the_title_style_never_bleeds_into_the_border_break_dashes() {
    // Chrome owns every `─` cell: the dash before ` SETTINGS ` and the first one after it both
    // carry the border token, with no title color, bold or italic on them. The title occupies its
    // own 10 cells from [`PANEL_TITLE_COLUMN`], so the trailing dash is the cell right after them
    // — derived from the label rather than written down, since the tab this runs on has moved
    // three times already.
    let terminal = draw(&mut on_tab(Tab::Settings), 60, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    let title_cells = u16::try_from(Tab::Settings.label().chars().count()).unwrap() + 2;
    for x in [PANEL_TITLE_COLUMN - 1, PANEL_TITLE_COLUMN + title_cells] {
        let cell = &buffer[(x, 1)];
        assert_eq!(cell.symbol(), "─", "column {x}");
        assert_eq!(cell.style().fg, Some(palette.line_strong), "column {x}");
        assert!(!cell.style().add_modifier.contains(Modifier::BOLD));
        assert!(!cell.style().add_modifier.contains(Modifier::ITALIC));
    }
}

// ---- footer (skill: Hint bar; Footer alert) ----

#[test]
fn the_hint_bar_owns_the_footer_row_while_the_quit_is_disarmed() {
    let terminal = draw(&mut App::new(Tier::Full), 40, 20);
    assert_eq!(row(terminal.backend().buffer(), 19), " ←→ switch   ? help   q quit            ");
}

#[test]
fn hint_keys_are_accent_and_labels_are_dim() {
    let terminal = draw(&mut App::new(Tier::Full), 40, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // Hotkey letters are one of the fixed accents that carry bold (DNA rule 4, Hierarchy
    // pairing 1). Pinned in both directions: the key is bold, the label beside it is not, so
    // a blanket bold over the whole row can't pass either.
    assert_eq!(buffer[(1, 19)].symbol(), "←");
    assert_eq!(buffer[(1, 19)].style().fg, Some(palette.accent));
    assert!(buffer[(1, 19)].style().add_modifier.contains(Modifier::BOLD));

    assert_eq!(buffer[(4, 19)].symbol(), "s");
    assert_eq!(buffer[(4, 19)].style().fg, Some(palette.text_dim));
    assert!(!buffer[(4, 19)].style().add_modifier.contains(Modifier::BOLD));

    // The single-letter universal keys too, not just the arrow run: `? help` then `q quit`.
    assert_eq!(buffer[(13, 19)].symbol(), "?");
    assert_eq!(buffer[(13, 19)].style().fg, Some(palette.accent));
    assert!(buffer[(13, 19)].style().add_modifier.contains(Modifier::BOLD));

    assert_eq!(buffer[(22, 19)].symbol(), "q");
    assert_eq!(buffer[(22, 19)].style().fg, Some(palette.accent));
    assert!(buffer[(22, 19)].style().add_modifier.contains(Modifier::BOLD));
}

#[test]
fn the_footer_names_the_modal_s_keys_while_a_modal_owns_input() {
    // The action menu: arrows move, enter picks, esc/q cancel — not the switch/quit pair the
    // top-level hint set advertises. `q` closes the menu, it never arms the quit, so the label
    // must read `back` (cloudy-tui: the back/quit label is context-aware while a modal is open).
    let mut app = App::new(Tier::Full);
    on_tab_in(&mut app, Tab::Memories);
    press(&mut app, KeyCode::Char('a'));
    assert!(matches!(app.modal(), Some(exportsnap::app::Modal::ActionMenu(_))));
    let terminal = draw(&mut app, 40, 20);
    assert_eq!(row(terminal.backend().buffer(), 19).trim_end(), " ↑↓ move   ↵ pick   esc cancel   q back");

    // The help modal: `?` now closes it, so the hint reads `close`, and the switch/quit pair is
    // gone — arrows are inert and `q` closes the modal without arming the quit.
    let mut app = App::new(Tier::Full);
    press(&mut app, KeyCode::Char('?'));
    assert!(matches!(app.modal(), Some(exportsnap::app::Modal::Help { .. })));
    let terminal = draw(&mut app, 40, 20);
    assert_eq!(row(terminal.backend().buffer(), 19).trim_end(), " ? close   esc cancel   q back");
}

#[test]
fn the_help_hint_advertises_scroll_keys_only_while_the_modal_scrolls() {
    // The memories screen's help modal holds 11 content lines (GLOBAL 5 + memories 3, two section
    // headers, one separator): at 40x20 its 12-row viewport fits them, so the hint reads
    // close/cancel/back alone; at 40x14 the 7-row viewport does not, and the compact `↑↓` run
    // joins the hint (cloudy-tui: Hint bar → density allowance for the compact run, modals keep
    // the spaced form in their own copy).
    let mut fits = App::new(Tier::Full);
    on_tab_in(&mut fits, Tab::Memories);
    press(&mut fits, KeyCode::Char('?'));
    let terminal = draw(&mut fits, 40, 20);
    assert_eq!(row(terminal.backend().buffer(), 19).trim_end(), " ? close   esc cancel   q back");

    let mut scrolls = App::new(Tier::Full);
    on_tab_in(&mut scrolls, Tab::Memories);
    press(&mut scrolls, KeyCode::Char('?'));
    let terminal = draw(&mut scrolls, 40, 14);
    assert_eq!(row(terminal.backend().buffer(), 13).trim_end(), " ↑↓ move   ? close   esc cancel   q back");
}

#[test]
fn the_footer_alert_replaces_the_hint_bar_in_place_while_armed() {
    let mut app = App::new(Tier::Full);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed());

    let terminal = draw(&mut app, 40, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    assert_eq!(row(buffer, 19), " ! press q again to quit                ");
    // Still exactly one footer row: the panel's bottom border sits directly above it.
    assert!(row(buffer, 18).starts_with('╰'));

    assert_eq!(buffer[(1, 19)].symbol(), "!");
    assert_eq!(buffer[(1, 19)].style().fg, Some(palette.warning));
    // A footer alert sits on the base surface and carries no semantic tint — that wash is the
    // banner's tell. Asserted against the tier's own surface, not `Reset`, which would pin
    // "nothing paints the base surface" as if it were the spec.
    assert_eq!(buffer[(1, 19)].style().bg, palette.surface(), "an alert sits on the base surface");
    assert_ne!(buffer[(1, 19)].style().bg, Some(palette.warning), "an alert carries no tint");
    assert_eq!(buffer[(3, 19)].style().fg, Some(palette.text_dim));
}

/// A run failure's message joins its statement to its fix with "; ", and the fix half is the
/// part the user acts on — so when the row cannot hold both, the fix half renders whole and the
/// statement half takes the visible prose cut, never a hard slice at the terminal edge. The fix
/// half stays path-free by construction (`src/tui/alert.rs`'s privacy rule), so the cut never
/// echoes the export's own bytes.
#[test]
fn a_long_run_alert_keeps_its_fix_clause_and_cuts_visibly() {
    use exportsnap::export::memories_run::{RunError, RunEvent, RunOutcome};
    use std::path::PathBuf;
    use std::sync::mpsc;

    // A source path long enough to push the fix clause past every width this test draws: the
    // failure message is "no mydata~ export part under {path}; point the source at the dir
    // holding the export's parts".
    let source = PathBuf::from("/mnt/data/snapshots/very-long-directory-name-keeps-going-past-the-row");
    let mut app = App::new(Tier::Full);
    on_tab_in(&mut app, Tab::Memories);
    let (sender, receiver) = mpsc::channel();
    app.with_memories_channel(receiver);
    sender.send(RunEvent::Finished(RunOutcome::Failed(RunError::NoExportId(source)))).unwrap();
    app.tick();
    assert!(app.memories().alert().is_some(), "the alert must be live before the frame draws");

    // Wide: the fix clause renders whole and the path half is the part that cuts, with the
    // ellipsis naming the cut.
    let terminal = draw(&mut app, 120, 24);
    let footer = row(terminal.backend().buffer(), 23);
    assert!(footer.contains("…; point the source at the dir holding the export's parts"), "{footer}");
    assert!(!footer.contains("keeps-going-past-the-row"), "the path half is the part that cuts: {footer}");

    // Narrow: the row is the fix clause with the statement half cut to its marker — the
    // semicolon idiom survives, so the ellipsis names the dropped error half.
    let terminal = draw(&mut app, 60, 24);
    let footer = row(terminal.backend().buffer(), 23);
    assert_eq!(footer.trim_end(), " ! …; point the source at the dir holding the export's parts", "{footer}");
}

// ---- compact banner (skill: Patterns → Density) ----

#[test]
fn the_compact_banner_appears_below_fourteen_rows() {
    let terminal = draw(&mut App::new(Tier::Full), 60, 13);
    assert_eq!(row(terminal.backend().buffer(), 1), " ! terminal too small · enlarge for full layout             ");
}

#[test]
fn the_compact_banner_is_gone_at_fourteen_rows() {
    let terminal = draw(&mut on_tab(Tab::Settings), 60, 14);
    assert!(row(terminal.backend().buffer(), 1).starts_with("╭─ SETTINGS "));
}

#[test]
fn the_compact_banner_keeps_its_full_copy_at_its_exact_width() {
    let terminal = draw(&mut App::new(Tier::Full), 47, 13);
    assert_eq!(row(terminal.backend().buffer(), 1), " ! terminal too small · enlarge for full layout");
}

#[test]
fn the_compact_banner_truncates_with_a_trailing_ellipsis_one_cell_narrower() {
    let terminal = draw(&mut App::new(Tier::Full), 46, 13);
    assert_eq!(row(terminal.backend().buffer(), 1), " ! terminal too small · enlarge for full layo…");
}

#[test]
fn the_compact_banner_survives_a_width_that_cuts_the_multibyte_separator() {
    // `·` sits at bytes 22..24 of the copy, so a byte-index truncation would slice `[..23]`
    // at this width and panic. Rendering it through the real draw path is the end-to-end
    // counterpart of the unit test on the truncator. 24 cells is under the header floor too,
    // so this is the header's banner — the one row the frame's single banner gets.
    let terminal = draw(&mut App::new(Tier::Full), 24, 13);
    assert_eq!(row(terminal.backend().buffer(), 0), " ! terminal too small ·…");
}

#[test]
fn the_compact_banner_tints_the_whole_row() {
    // The full-width semantic wash is what tells a banner apart from the glyph-only footer
    // alert, so every cell of the row carries it — trailing filler included.
    let terminal = draw(&mut App::new(Tier::Full), 60, 13);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    for x in 0..60 {
        let style = buffer[(x, 1)].style();
        assert_eq!(style.bg, Some(palette.warning), "column {x}");
        assert_eq!(style.fg, Some(palette.contrast_text()), "column {x}");
    }
}

// ---- palette resolver through a real render (theme todo: verify under both tiers) ----

#[test]
fn the_same_header_renders_each_tier_in_that_tier_s_own_colors() {
    for (tier, accent, text_dim) in
        [(Tier::Full, Color::Rgb(67, 171, 229), Color::Rgb(166, 173, 200)), (Tier::Compatible, Color::Indexed(75), Color::Indexed(145))]
    {
        let terminal = draw(&mut App::new(tier), 100, 20);
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(FIRST_TAB_COLUMN, 0)].style().fg, Some(accent), "{tier:?} active tab");
        assert_eq!(buffer[(INACTIVE_SECOND_COLUMN, 0)].style().fg, Some(text_dim), "{tier:?} inactive tab");
    }
}

#[test]
fn the_panel_border_and_title_follow_the_tier_too() {
    for (tier, accent_2, line_strong) in
        [(Tier::Full, Color::Rgb(217, 119, 87), Color::Rgb(69, 71, 90)), (Tier::Compatible, Color::Indexed(173), Color::Indexed(240))]
    {
        // Through the bounded helper, not a second unbounded walk: one spelling of the question, so
        // the guard cannot be present on one path and absent on the other.
        let mut app = App::new(tier);
        on_tab_in(&mut app, Tab::Memories);
        let terminal = draw(&mut app, 60, 20);
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(PANEL_TITLE_COLUMN + 1, 1)].style().fg, Some(accent_2), "{tier:?} panel title");
        assert_eq!(buffer[(0, 1)].style().fg, Some(line_strong), "{tier:?} panel border");
    }
}

// ---- base surface (DNA rule 3) ----

#[test]
fn the_full_tier_paints_the_base_surface_across_the_whole_frame() {
    // The alternate screen resets to the terminal's own background, so every cell the app
    // doesn't otherwise tint has to carry `BG` — header row, panel interior, border, footer.
    let terminal = draw(&mut App::new(Tier::Full), 40, 20);
    let buffer = terminal.backend().buffer();

    for y in 0..20 {
        for x in 0..40 {
            assert_eq!(buffer[(x, y)].style().bg, Some(Color::Rgb(30, 30, 46)), "cell ({x}, {y})");
        }
    }
}

#[test]
fn the_compatible_tier_leaves_the_terminal_background_alone() {
    // DNA rule 3 scopes unpainted surfaces to this tier only: elevation falls to borders and
    // color, and the user's own background shows through.
    let terminal = draw(&mut App::new(Tier::Compatible), 40, 20);
    let buffer = terminal.backend().buffer();

    for y in 0..20 {
        for x in 0..40 {
            assert_eq!(buffer[(x, y)].style().bg, Some(Color::Reset), "cell ({x}, {y})");
        }
    }
}

#[test]
fn the_compact_banner_wash_survives_the_base_surface_underneath_it() {
    // Ordering guard: the base fill is painted first, so a banner drawn before it would be
    // overwritten. Both tiers paint the semantic wash — it is not a surface fill.
    for (tier, warning) in [(Tier::Full, Color::Rgb(249, 226, 175)), (Tier::Compatible, Color::Indexed(223))] {
        let terminal = draw(&mut App::new(tier), 60, 13);
        let buffer = terminal.backend().buffer();
        for x in 0..60 {
            assert_eq!(buffer[(x, 1)].style().bg, Some(warning), "{tier:?} cell ({x}, 1)");
        }
    }
}

// ---- degenerate sizes ----

#[test]
fn every_tab_renders_without_panicking_at_degenerate_sizes() {
    // A floor, not coverage: a write past the area is the classic ratatui regression, and the
    // header's suppression ladder plus the compact split both do width/height arithmetic that
    // has to survive a zero or one-cell area.
    let sizes = [(0, 0), (0, 20), (20, 0), (1, 1), (1, 3), (3, 1), (2, 2), (4, 4), (16, 3), (17, 2), (255, 1), (1, 255), (500, 3)];

    for tab in Tab::ALL {
        let mut app = on_tab(tab);
        for (width, height) in sizes {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| shell::render(frame, &mut app)).unwrap_or_else(|error| panic!("{tab:?} at {width}x{height}: {error}"));
        }
    }
}

#[test]
fn the_header_never_leaves_a_hole_in_its_row() {
    // The gap span has to pad out to the right edge at every width; below the strip's minimum
    // the content simply overruns and ratatui clips it. A line shorter than its area would
    // leave cells the shell never paints. Asserting the *rendered* row's length instead would
    // be tautological — `row()` always yields one entry per column whether or not anything
    // wrote there.
    for tab in Tab::ALL {
        for width in 0..=120u16 {
            let line = header::render(&Palette::new(Tier::Full), tab, "9.9.9", width, false, &[]);
            assert!(line.width() >= width as usize, "{tab:?} at width {width}: line is {} cells", line.width());
        }
    }
}

#[test]
fn the_header_keeps_its_brand_and_active_label_once_they_fit() {
    // Drawing at every width is also the panic floor for the suppression ladder's arithmetic.
    for tab in Tab::ALL {
        for width in 0..=120u16 {
            let rendered = header_only(tab, width);

            if width >= 16 {
                assert!(rendered.starts_with(" exportsnap  •  "), "{tab:?} at width {width}: {rendered:?}");
            }
            // The floor is the header's own claim about where labels stop clipping, so an
            // understated one reds right here rather than shipping a lying header row.
            if width >= header::min_width() {
                assert!(rendered.contains(tab.label()), "{tab:?} at width {width}: {rendered:?}");
            }
        }
    }
}
