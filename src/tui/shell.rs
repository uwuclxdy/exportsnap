//! Frame composition: header row, body, footer row (cloudy-tui skill: App shell). The regions
//! are separated by spacing alone — the shell draws no dividers.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;

use super::screens::{account, chat_media, history, memories, overview, settings};
use super::theme::{Palette, glyph};
use super::{footer, header};
use crate::app::{App, Tab};
use crate::tui::format::truncate_prose;

/// Below this height the layout stops fitting (cloudy-tui skill: Patterns → Density).
pub(crate) const COMPACT_HEIGHT: u16 = 14;

/// The header owns exactly one row and the footer exactly one, at every size (cloudy-tui skill:
/// App shell; Footer alert replaces the hint bar in place rather than stacking above it).
///
/// These three are `pub(crate)` and are what the vertical [`Layout`] below is actually built from,
/// so a screen can derive how many rows its panel is guaranteed instead of restating the shell's
/// geometry as its own literals — see [`crate::tui::screens::overview`]'s height invariant. A
/// coupling assertion between two hardcoded numbers checks nothing, so these must stay the single
/// source rather than a copy that agrees today.
pub(crate) const HEADER_ROWS: u16 = 1;
pub(crate) const FOOTER_ROWS: u16 = 1;

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    // A copy rather than a borrow: the memories screen's stateful table needs the app mutable
    // while the palette is read.
    let palette = *app.palette();

    // The base surface, on the tier that has one. `try_init` enters the alternate screen,
    // which resets to the terminal's own background, so without this the `full` tier would
    // render catppuccin text over whatever the user's terminal happens to be.
    if let Some(surface) = palette.surface() {
        frame.render_widget(Block::new().style(Style::new().bg(surface)), area);
    }

    let [header_area, body_area, footer_area] =
        Layout::vertical([Constraint::Length(HEADER_ROWS), Constraint::Fill(1), Constraint::Length(FOOTER_ROWS)]).areas(area);

    // Below the header's own floor the tab strip can only render a clipped active label, so
    // the row would name the wrong tab. It gives up its row to the size banner instead —
    // off-contract placement (the Banner section puts it atop the body), taken because that
    // row is already lost while the body's are not.
    let header_fits = header_area.width >= header::min_width();

    if header_fits {
        frame.render_widget(
            header::render(&palette, app.active(), env!("CARGO_PKG_VERSION"), header_area.width, app.alt_held()),
            header_area,
        );
    } else {
        frame.render_widget(compact_banner(&palette, header_area.width), header_area);
    }
    // The hint set is the ACTIVE screen's, like the alert and the descended flag: two screens
    // can each hold one and there is one footer row, so `App` answers all three off the same tab
    // rather than the footer guessing. The history tab's top-level hints name its own keys
    // (`t toggle all`, `space toggle`) and derive them off the picker's state; the descended set
    // is universal, so it is chosen off `descended` rather than per tab. See `App`'s own docs
    // for why the active screen wins.
    let hints = if app.descended() {
        footer::descended_hints(&palette, footer_area.width)
    } else {
        match app.active() {
            Tab::History => footer::history_hints(&palette, app.history().picker_has_rows(), footer_area.width),
            // While a settings text input is being edited, arrows move the caret (not the
            // tab) and `q` types a letter, so the plain set would advertise keys that do
            // something else this frame — the edit set replaces it.
            Tab::Settings if app.settings().is_editing() => footer::settings_edit_hints(&palette, footer_area.width),
            _ => footer::plain_hints(&palette, footer_area.width),
        }
    };
    frame.render_widget(footer::render(&palette, app.is_quit_armed(), app.alert(), hints, footer_area.width), footer_area);

    // Recomputed from the live frame size every draw, so it self-clears on resize rather than
    // living on as a stored notification. At most one banner per frame (skill: Banner), so a
    // frame short on both axes says it once, in the header's row.
    let panel_area = if header_fits && area.height < COMPACT_HEIGHT {
        let [banner_area, rest] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(body_area);
        frame.render_widget(compact_banner(&palette, banner_area.width), banner_area);
        rest
    } else {
        body_area
    };

    match app.active() {
        Tab::Overview => overview::render(frame, &palette, app.overview(), panel_area),
        Tab::Memories => memories::render(frame, &palette, app.memories_mut(), panel_area),
        Tab::ChatMedia => chat_media::render(frame, &palette, app.chat_media_mut(), panel_area),
        Tab::History => history::render(frame, &palette, app.history_mut(), panel_area),
        Tab::Account => account::render(frame, &palette, app.account_mut(), panel_area),
        Tab::Settings => settings::render(frame, &palette, app.settings(), panel_area),
    }

    // The settings screen's DANGER toast renders last, over the finished frame: its glass
    // blend reads the buffer beneath it, which must be final by the time the toast draws
    // (cloudy-tui: Toast renders last). It floats on every tab while it lives — the app has
    // no tab-activity color channel, so the toast is the one notification surface.
    if let Some(toast) = app.settings().toast() {
        settings::render_toast(frame, &palette, toast, area);
    }
}

/// Full-width `WARNING` wash, for either size floor — the tint is what separates a banner
/// from the glyph-only footer alert. The contract names the tint and the leading ` ! ` glyph
/// but not the text color on top of the fill, so the row takes `BG` for legibility, matching
/// the dark-on-semantic treatment the contract uses for its inverse blocks.
///
/// Do NOT "restore" the ` ! ` glyph to the semantic color the Banner section names: on a
/// `WARNING` fill that paints `WARNING` on `WARNING` and the marker vanishes into its own
/// background. The contract's two clauses contradict each other here and legibility wins.
fn compact_banner(palette: &Palette, width: u16) -> Line<'static> {
    let text = format!(
        " {marker} terminal too small {separator} enlarge for full layout",
        marker = glyph::ALERT_MARKER,
        separator = glyph::CLAUSE_SEPARATOR,
    );

    // The style rides the `Line`, not the span: `Line::render_with_alignment` paints it across
    // the whole row before drawing any span, so the trailing cells carry the wash with no
    // width-sized padding string built per frame.
    Line::from(Span::raw(truncate_prose(&text, width as usize))).style(Style::new().fg(palette.bg).bg(palette.warning))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::format::truncate_prose;

    /// The exact banner copy, so the boundary cases below name real widths rather than a
    /// stand-in string that might not carry the multi-byte `·`.
    fn banner_text() -> String {
        format!(
            " {marker} terminal too small {separator} enlarge for full layout",
            marker = glyph::ALERT_MARKER,
            separator = glyph::CLAUSE_SEPARATOR,
        )
    }

    #[test]
    fn banner_copy_is_47_cells_over_48_bytes() {
        // The two differing is the whole reason the cut must be char-based; if the copy ever
        // loses its `·` the byte-boundary test below stops proving anything.
        let text = banner_text();
        assert_eq!(text.chars().count(), 47);
        assert_eq!(text.len(), 48);

        // `truncate_prose` measures cells now (it lives in `format.rs`), so a wide glyph in
        // the copy shortens the kept text instead of holing the wash. The copy is one cell per
        // char, which is why the char-count cut once needed this pin: keep pinning the copy's
        // shape so the boundary cases below keep exercising the cut with the real banner text,
        // multi-byte `·` included, and a future wide glyph in the copy reds here rather than
        // silently changing every pinned width above.
        assert_eq!(Span::raw(&text).width(), 47);
    }

    #[test]
    fn prose_shorter_than_the_width_is_left_alone() {
        let text = banner_text();
        assert_eq!(truncate_prose(&text, 47), text);
        assert_eq!(truncate_prose(&text, 100), text);
    }

    #[test]
    fn prose_one_cell_too_long_gains_a_trailing_ellipsis() {
        assert_eq!(truncate_prose(&banner_text(), 46), " ! terminal too small · enlarge for full layo…");
    }

    #[test]
    fn prose_keeps_shrinking_below_that() {
        assert_eq!(truncate_prose(&banner_text(), 45), " ! terminal too small · enlarge for full lay…");
    }

    #[test]
    fn a_cut_that_would_split_the_multibyte_separator_stays_whole() {
        // `·` occupies bytes 22..24, so a byte-index implementation would slice `&text[..23]`
        // here and panic on a non-char-boundary. This is the width that catches that.
        assert_eq!(truncate_prose(&banner_text(), 24), " ! terminal too small ·…");
    }

    #[test]
    fn a_single_cell_leaves_only_the_ellipsis() {
        assert_eq!(truncate_prose(&banner_text(), 1), "…");
    }

    #[test]
    fn a_zero_width_row_truncates_to_nothing() {
        assert_eq!(truncate_prose(&banner_text(), 0), "");
    }
}
