//! Frame composition: header row, body, footer row (cloudy-tui skill: App shell). The regions
//! are separated by spacing alone — the shell draws no dividers.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::symbols::line;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding};

use super::theme::{Palette, glyph};
use super::{footer, header};
use crate::app::App;

/// Below this height the layout stops fitting (cloudy-tui skill: Patterns → Density).
const COMPACT_HEIGHT: u16 = 14;

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let palette = app.palette();

    // The base surface, on the tier that has one. `try_init` enters the alternate screen,
    // which resets to the terminal's own background, so without this the `full` tier would
    // render catppuccin text over whatever the user's terminal happens to be.
    if let Some(surface) = palette.surface() {
        frame.render_widget(Block::new().style(Style::new().bg(surface)), area);
    }

    let [header_area, body_area, footer_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);

    // Below the header's own floor the tab strip can only render a clipped active label, so
    // the row would name the wrong tab. It gives up its row to the size banner instead —
    // off-contract placement (the Banner section puts it atop the body), taken because that
    // row is already lost while the body's are not.
    let header_fits = header_area.width >= header::min_width();

    if header_fits {
        frame.render_widget(header::render(palette, app.active(), env!("CARGO_PKG_VERSION"), header_area.width), header_area);
    } else {
        frame.render_widget(compact_banner(palette, header_area.width), header_area);
    }
    frame.render_widget(footer::render(palette, app.is_quit_armed()), footer_area);

    // Recomputed from the live frame size every draw, so it self-clears on resize rather than
    // living on as a stored notification. At most one banner per frame (skill: Banner), so a
    // frame short on both axes says it once, in the header's row.
    let panel_area = if header_fits && area.height < COMPACT_HEIGHT {
        let [banner_area, rest] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(body_area);
        frame.render_widget(compact_banner(palette, banner_area.width), banner_area);
        rest
    } else {
        body_area
    };

    frame.render_widget(panel(palette, app.active().label()), panel_area);
}

/// The screen's sole content panel, so it takes `LINE_STRONG` (it owns the cursor) and, being
/// the first panel on the body, an `ACCENT_2` title.
fn panel(palette: &Palette, title: &str) -> Block<'static> {
    let border = Style::new().fg(palette.line_strong);

    Block::bordered().border_type(BorderType::Rounded).border_style(border).padding(Padding::new(1, 1, 0, 0)).title(Line::from(vec![
        // ratatui puts a title flush against the corner; this dash restores the contract's
        // `╭─ TITLE ─` break and carries the border token, because chrome owns every dash.
        Span::styled(line::HORIZONTAL, border),
        Span::styled(format!(" {} ", title.to_uppercase()), Style::new().fg(palette.accent_2).bold().italic()),
    ]))
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

/// Trailing-ellipsis truncation for prose (cloudy-tui skill: Patterns → Truncation). Without
/// it the banner clips mid-word on a narrow terminal, which is exactly the terminal it renders
/// on.
///
/// Cuts on `char` boundaries, never byte indices: the copy carries a multi-byte `·`, and a
/// byte-index slice landing inside it panics. Char count is the cell width here because every
/// char in the banner copy is one cell wide — this is not a general display-width truncator.
fn truncate_prose(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }

    let mut truncated: String = text.chars().take(width - 1).collect();
    truncated.push(glyph::ELLIPSIS);
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // `truncate_prose` counts chars and calls them cells, which holds only while the copy
        // is one cell per char. Two rows ride on that now: the compact body row and the header
        // row below its width floor. A wide glyph costs both the trailing `…`, which the
        // char-count cut pushes past the edge where it is dropped, and the full-width wash,
        // which holes at the glyph's continuation cell. ratatui resets that cell after writing
        // the symbol, so the hole lands at any width, not only where the row overruns. Pinned
        // rather than assumed, since the wash is what separates a banner from a footer alert.
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
