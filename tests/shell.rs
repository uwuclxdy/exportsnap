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

/// ` exportsnap` (11 cells) + `  •  ` (5 cells) — where the first tab label starts.
const FIRST_TAB_COLUMN: u16 = 16;
/// `overview` (8 cells) + the 3-cell tab gap past [`FIRST_TAB_COLUMN`].
const SECOND_TAB_COLUMN: u16 = FIRST_TAB_COLUMN + 8 + 3;
/// `╭` + the border-token dash the panel title carries — where ` OVERVIEW ` starts.
const PANEL_TITLE_COLUMN: u16 = 2;

fn draw(app: &App, width: u16, height: u16) -> Terminal<TestBackend> {
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

fn header_row(app: &App, width: u16) -> String {
    let terminal = draw(app, width, 20);
    row(terminal.backend().buffer(), 0)
}

/// The header alone, at a version string fixed here rather than taken from the crate, so the
/// suppression-ladder widths below stay pinned to literals when the crate version moves.
/// `v9.9.9 ` is the same 7 cells the crate's own version currently occupies.
fn header_only(active: Tab, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(header::render(&Palette::new(Tier::Full), active, "9.9.9", width), frame.area());
        })
        .unwrap();
    row(terminal.backend().buffer(), 0)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn on_tab(tab: Tab) -> App {
    let mut app = App::new(Tier::Full);
    while app.active() != tab {
        press(&mut app, KeyCode::Right);
    }
    app
}

// ---- whole frame ----

#[test]
fn renders_header_body_panel_and_hint_bar() {
    let terminal = draw(&App::new(Tier::Full), 52, 6);
    assert_eq!(
        grid(terminal.backend().buffer()),
        [
            " exportsnap  •      overview   ›                    ",
            " ! terminal too small · enlarge for full layout     ",
            "╭─ OVERVIEW ───────────────────────────────────────╮",
            "│                                                  │",
            "╰──────────────────────────────────────────────────╯",
            " ←→ switch   q quit                                 ",
        ]
    );
}

// ---- header: tab labels + active styling (skill: Tab bar) ----

#[test]
fn header_renders_the_brand_all_six_tab_labels_and_the_version() {
    assert_eq!(
        header_row(&App::new(Tier::Full), 100),
        format!(
            " exportsnap  •  overview   memories   chat media   history   account   settings\
              {:>21}",
            format!("v{} ", env!("CARGO_PKG_VERSION"))
        )
    );
}

#[test]
fn active_tab_label_is_accent_bold_underlined_and_inactive_is_dim() {
    let terminal = draw(&App::new(Tier::Full), 100, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    let active = buffer[(FIRST_TAB_COLUMN, 0)].style();
    assert_eq!(active.fg, Some(palette.accent));
    assert!(active.add_modifier.contains(Modifier::BOLD));
    assert!(active.add_modifier.contains(Modifier::UNDERLINED));

    let inactive = buffer[(SECOND_TAB_COLUMN, 0)].style();
    assert_eq!(inactive.fg, Some(palette.text_dim));
    assert!(!inactive.add_modifier.contains(Modifier::BOLD));
    assert!(!inactive.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn the_underline_moves_with_the_active_tab() {
    let terminal = draw(&on_tab(Tab::Memories), 100, 20);
    let buffer = terminal.backend().buffer();

    assert!(!buffer[(FIRST_TAB_COLUMN, 0)].style().add_modifier.contains(Modifier::UNDERLINED));
    assert!(buffer[(SECOND_TAB_COLUMN, 0)].style().add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn no_underline_row_sits_beneath_the_tab_bar() {
    // The active label carries the underline as a text attribute; row 1 is already the panel.
    let terminal = draw(&App::new(Tier::Full), 100, 20);
    assert!(row(terminal.backend().buffer(), 1).starts_with("╭─ OVERVIEW "));
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
    assert_eq!(header_only(Tab::Overview, 78), " exportsnap  •      overview   ›                                              ");
}

#[test]
fn overflow_markers_track_which_neighbours_exist() {
    // A middle tab has both neighbours; the first and last each lose one marker, and the
    // marker's cell stays blank so the active label holds its column.
    assert_eq!(header_row(&on_tab(Tab::ChatMedia), 50), " exportsnap  •  ‹   chat media   ›                ");
    assert_eq!(header_row(&on_tab(Tab::Overview), 50), " exportsnap  •      overview   ›                  ");
    assert_eq!(header_row(&on_tab(Tab::Settings), 50), " exportsnap  •  ‹   settings                      ");
}

#[test]
fn overflow_markers_are_text_faint() {
    let terminal = draw(&on_tab(Tab::ChatMedia), 50, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    assert_eq!(buffer[(FIRST_TAB_COLUMN, 0)].symbol(), "‹");
    assert_eq!(buffer[(FIRST_TAB_COLUMN, 0)].style().fg, Some(palette.text_faint));
}

// ---- body panel (skill: Panel) ----

#[test]
fn the_panel_title_names_the_active_tab_in_uppercase() {
    for (tab, title) in [
        (Tab::Overview, "╭─ OVERVIEW ─"),
        (Tab::Memories, "╭─ MEMORIES ─"),
        (Tab::ChatMedia, "╭─ CHAT MEDIA ─"),
        (Tab::History, "╭─ HISTORY ─"),
        (Tab::Account, "╭─ ACCOUNT ─"),
        (Tab::Settings, "╭─ SETTINGS ─"),
    ] {
        let terminal = draw(&on_tab(tab), 60, 20);
        assert!(row(terminal.backend().buffer(), 1).starts_with(title), "{tab:?} should render {title}");
    }
}

#[test]
fn the_sole_panel_title_is_accent_2_bold_italic_on_a_line_strong_border() {
    let terminal = draw(&App::new(Tier::Full), 60, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    let title = buffer[(PANEL_TITLE_COLUMN + 1, 1)].style();
    assert_eq!(buffer[(PANEL_TITLE_COLUMN + 1, 1)].symbol(), "O");
    assert_eq!(title.fg, Some(palette.accent_2));
    assert!(title.add_modifier.contains(Modifier::BOLD));
    assert!(title.add_modifier.contains(Modifier::ITALIC));

    let corner = buffer[(0, 1)].style();
    assert_eq!(buffer[(0, 1)].symbol(), "╭");
    assert_eq!(corner.fg, Some(palette.line_strong));
}

#[test]
fn the_title_style_never_bleeds_into_the_border_break_dashes() {
    // Chrome owns every `─` cell: the dash before ` OVERVIEW ` and the first one after it both
    // carry the border token, with no title color, bold or italic on them.
    let terminal = draw(&App::new(Tier::Full), 60, 20);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    for x in [PANEL_TITLE_COLUMN - 1, PANEL_TITLE_COLUMN + 10] {
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
    let terminal = draw(&App::new(Tier::Full), 40, 20);
    assert_eq!(row(terminal.backend().buffer(), 19), " ←→ switch   q quit                     ");
}

#[test]
fn hint_keys_are_accent_and_labels_are_dim() {
    let terminal = draw(&App::new(Tier::Full), 40, 20);
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

    // The single-letter key too, not just the arrow run.
    assert_eq!(buffer[(13, 19)].symbol(), "q");
    assert_eq!(buffer[(13, 19)].style().fg, Some(palette.accent));
    assert!(buffer[(13, 19)].style().add_modifier.contains(Modifier::BOLD));
}

#[test]
fn the_footer_alert_replaces_the_hint_bar_in_place_while_armed() {
    let mut app = App::new(Tier::Full);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed());

    let terminal = draw(&app, 40, 20);
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

// ---- compact banner (skill: Patterns → Density) ----

#[test]
fn the_compact_banner_appears_below_fourteen_rows() {
    let terminal = draw(&App::new(Tier::Full), 60, 13);
    assert_eq!(row(terminal.backend().buffer(), 1), " ! terminal too small · enlarge for full layout             ");
}

#[test]
fn the_compact_banner_is_gone_at_fourteen_rows() {
    let terminal = draw(&App::new(Tier::Full), 60, 14);
    assert!(row(terminal.backend().buffer(), 1).starts_with("╭─ OVERVIEW "));
}

#[test]
fn the_compact_banner_keeps_its_full_copy_at_its_exact_width() {
    let terminal = draw(&App::new(Tier::Full), 47, 13);
    assert_eq!(row(terminal.backend().buffer(), 1), " ! terminal too small · enlarge for full layout");
}

#[test]
fn the_compact_banner_truncates_with_a_trailing_ellipsis_one_cell_narrower() {
    let terminal = draw(&App::new(Tier::Full), 46, 13);
    assert_eq!(row(terminal.backend().buffer(), 1), " ! terminal too small · enlarge for full layo…");
}

#[test]
fn the_compact_banner_survives_a_width_that_cuts_the_multibyte_separator() {
    // `·` sits at bytes 22..24 of the copy, so a byte-index truncation would slice `[..23]`
    // at this width and panic. Rendering it through the real draw path is the end-to-end
    // counterpart of the unit test on the truncator.
    let terminal = draw(&App::new(Tier::Full), 24, 13);
    assert_eq!(row(terminal.backend().buffer(), 1), " ! terminal too small ·…");
}

#[test]
fn the_compact_banner_tints_the_whole_row() {
    // The full-width semantic wash is what tells a banner apart from the glyph-only footer
    // alert, so every cell of the row carries it — trailing filler included.
    let terminal = draw(&App::new(Tier::Full), 60, 13);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    for x in 0..60 {
        let style = buffer[(x, 1)].style();
        assert_eq!(style.bg, Some(palette.warning), "column {x}");
        assert_eq!(style.fg, Some(palette.bg), "column {x}");
    }
}

// ---- palette resolver through a real render (theme todo: verify under both tiers) ----

#[test]
fn the_same_header_renders_each_tier_in_that_tier_s_own_colors() {
    for (tier, accent, text_dim) in
        [(Tier::Full, Color::Rgb(67, 171, 229), Color::Rgb(166, 173, 200)), (Tier::Compatible, Color::Indexed(75), Color::Indexed(145))]
    {
        let terminal = draw(&App::new(tier), 100, 20);
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(FIRST_TAB_COLUMN, 0)].style().fg, Some(accent), "{tier:?} active tab");
        assert_eq!(buffer[(SECOND_TAB_COLUMN, 0)].style().fg, Some(text_dim), "{tier:?} inactive tab");
    }
}

#[test]
fn the_panel_border_and_title_follow_the_tier_too() {
    for (tier, accent_2, line_strong) in
        [(Tier::Full, Color::Rgb(217, 119, 87), Color::Rgb(69, 71, 90)), (Tier::Compatible, Color::Indexed(173), Color::Indexed(240))]
    {
        let terminal = draw(&App::new(tier), 60, 20);
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
    let terminal = draw(&App::new(Tier::Full), 40, 20);
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
    let terminal = draw(&App::new(Tier::Compatible), 40, 20);
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
        let terminal = draw(&App::new(tier), 60, 13);
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
        let app = on_tab(tab);
        for (width, height) in sizes {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| shell::render(frame, &app)).unwrap_or_else(|error| panic!("{tab:?} at {width}x{height}: {error}"));
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
            let line = header::render(&Palette::new(Tier::Full), tab, "9.9.9", width);
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
            // 16 lead + the widest overflow form (`‹` + gap + `chat media` + gap + `›`) = 34.
            if width >= 34 {
                assert!(rendered.contains(tab.label()), "{tab:?} at width {width}: {rendered:?}");
            }
        }
    }
}
