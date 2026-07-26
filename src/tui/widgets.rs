//! Widget builders shared by the app shell and the per-tab screens.

use ratatui::style::Style;
use ratatui::symbols::line;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding};

use super::theme::Palette;

/// The two independent axes a panel sits on (cloudy-tui skill: Panel; Patterns → Pane focus).
/// Named fields rather than two positional `bool`s, so a call site cannot swap them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelStyle {
    /// The first bordered panel on the body takes an `ACCENT_2` title; every later one takes
    /// `TEXT_DIM`.
    pub first: bool,
    /// A focused pane takes `LINE_STRONG` and a bold title; a blurred one keeps the italic and
    /// drops the bold. A screen's sole content panel counts as focused; a read-only pane focus
    /// never descends into counts as blurred.
    pub focused: bool,
}

/// A bordered panel carrying the contract's `╭─ TITLE ─` break. `title` is uppercased here, so
/// callers pass it in the lowercase the rest of the app spells labels in.
#[must_use]
pub fn panel(palette: &Palette, title: &str, style: PanelStyle) -> Block<'static> {
    let border = Style::new().fg(if style.focused { palette.line_strong } else { palette.line });
    let title_color = if style.first { palette.accent_2 } else { palette.text_dim };
    let title_style = if style.focused { Style::new().fg(title_color).bold().italic() } else { Style::new().fg(title_color).italic() };

    Block::bordered().border_type(BorderType::Rounded).border_style(border).padding(Padding::new(1, 1, 0, 0)).title(Line::from(vec![
        // ratatui puts a title flush against the corner; this dash restores the contract's
        // `╭─ TITLE ─` break and carries the border token, because chrome owns every dash.
        Span::styled(line::HORIZONTAL, border),
        Span::styled(format!(" {} ", title.to_uppercase()), title_style),
    ]))
}

/// The narrowest panel that still renders `title` whole: both corners, the leading dash of the
/// break, and the space on each side of the title.
#[must_use]
pub fn min_width_for_title(title: &str) -> usize {
    Span::raw(title.to_uppercase()).width() + 5
}

/// Rows [`panel`]'s own border costs: one at the top, one at the bottom.
///
/// Unlike the shell's row constants this cannot be single-sourced — it restates ratatui's border
/// geometry, which only `Block::inner` knows and only at runtime. So it carries a test instead of a
/// promise; anything deriving a height budget from it is resting on that test.
pub(crate) const BORDER_ROWS: u16 = 2;

/// Columns [`panel`]'s chrome costs: one border plus one padding cell on each side.
///
/// Kept separate from [`BORDER_ROWS`] rather than spelled `BORDER_ROWS + 2`, even though the two are
/// numerically related today. A width budget expressed through a height constant is only correct
/// while a border happens to cost one cell on every side, and editing one would silently move the
/// other. Same test, same reason.
pub(crate) const CHROME_COLUMNS: u16 = 4;

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;
    use crate::tui::theme::Tier;

    #[test]
    fn a_panel_border_costs_exactly_the_rows_the_constant_names() {
        // The one term in the overview's height invariant that is a restatement rather than a
        // shared constant, so it gets checked against the widget that actually draws it.
        let block = panel(&Palette::new(Tier::Full), "title", PanelStyle { first: true, focused: true });
        let area = Rect::new(0, 0, 40, 10);

        assert_eq!(area.height - block.inner(area).height, BORDER_ROWS);
    }

    #[test]
    fn the_panel_chrome_costs_exactly_the_columns_the_constant_names() {
        // Vertical padding is zero (`Padding::new(1, 1, 0, 0)`), so the border rows are the whole
        // vertical cost while the horizontal cost carries the padding too. Both constants are pinned
        // against the same block, so a padding change cannot quietly land on the wrong axis.
        let block = panel(&Palette::new(Tier::Full), "title", PanelStyle { first: false, focused: false });
        let area = Rect::new(0, 0, 40, 10);
        let inner = block.inner(area);

        assert_eq!(area.width - inner.width, CHROME_COLUMNS, "two borders plus one padding cell each side");
        assert_eq!(area.height - inner.height, BORDER_ROWS, "no vertical padding");
    }
}
