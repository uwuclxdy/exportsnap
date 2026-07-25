//! The single footer row: the hint bar, or the footer alert that replaces it in place while
//! the 2-step quit is armed (cloudy-tui skill: Hint bar; Footer alert). Never both, never
//! stacked.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::theme::{Palette, glyph};

/// 3 spaces between hint groups, no glyph.
const HINT_GAP: &str = "   ";

/// The footer row for this frame.
///
/// The universal `a actions` / `? help` hints are deliberately absent: nothing is bound behind
/// those keys yet, so rendering them would advertise a binding that does nothing.
#[must_use]
pub fn render(palette: &Palette, quit_armed: bool) -> Line<'static> {
    if quit_armed { alert(palette) } else { hints(palette) }
}

fn hints(palette: &Palette) -> Line<'static> {
    // Hotkey letters are one of the fixed high-salience accents that carry bold (DNA rule 4;
    // Hierarchy pairing 1). The Hint bar section names the color only and is silent on weight,
    // so the general pairing governs rather than being overridden by the omission.
    let key = Style::new().fg(palette.accent).bold();
    let label = Style::new().fg(palette.text_dim);

    Line::from(vec![
        Span::raw(" "),
        // The hint bar's compact space-free arrow run — modals keep the spaced form.
        Span::styled(format!("{}{}", glyph::KEY_LEFT, glyph::KEY_RIGHT), key),
        Span::raw(" "),
        Span::styled("switch", label),
        Span::raw(HINT_GAP),
        Span::styled("q", key),
        Span::raw(" "),
        Span::styled("quit", label),
    ])
}

/// Glyph-only line on the base `BG`; the absent background tint is what tells it apart from a
/// banner.
fn alert(palette: &Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {} ", glyph::ALERT_MARKER), Style::new().fg(palette.warning)),
        Span::styled("press q again to quit", Style::new().fg(palette.text_dim)),
    ])
}
