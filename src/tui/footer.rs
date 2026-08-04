//! The single footer row: the hint bar, the footer alert that replaces it while the 2-step quit
//! is armed, or the run-completion alert (cloudy-tui skill: Hint bar; Footer alert). Exactly one
//! of the three per frame — never stacked.
//!
//! Precedence: the armed-quit prompt is a transient needs-action, so it wins over the completion
//! alert; the completion alert replaces the hints; the descended-pane hint set replaces the plain
//! one so the row never advertises a key that does something else on this frame.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::theme::{Palette, glyph};
use crate::tui::screens::memories::{AlertKind, RunAlert};

/// 3 spaces between hint groups, no glyph.
const HINT_GAP: &str = "   ";

/// The footer row for this frame.
#[must_use]
pub fn render(palette: &Palette, quit_armed: bool, alert: Option<&RunAlert>, descended: bool) -> Line<'static> {
    if quit_armed {
        quit_alert(palette)
    } else if let Some(alert) = alert {
        completion_alert(palette, alert)
    } else if descended {
        descended_hints(palette)
    } else {
        hints(palette)
    }
}

/// The plain hint set. The universal `a actions` / `? help` hints are deliberately absent: nothing
/// is bound behind those keys yet, so rendering them would advertise a binding that does nothing.
fn hints(palette: &Palette) -> Line<'static> {
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

/// The hint set while the memories table pane is descended: arrows scroll it, `←`, `esc` and `q`
/// all ascend, and `→` is inert so it gets no hint.
fn descended_hints(palette: &Palette) -> Line<'static> {
    let key = Style::new().fg(palette.accent).bold();
    let label = Style::new().fg(palette.text_dim);

    Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{}{}", glyph::KEY_UP, glyph::KEY_DOWN), key),
        Span::raw(" "),
        Span::styled("scroll", label),
        Span::raw(HINT_GAP),
        Span::styled(glyph::KEY_LEFT.to_string(), key),
        Span::raw(" "),
        Span::styled("back", label),
        Span::raw(HINT_GAP),
        Span::styled("esc", key),
        Span::raw(" "),
        Span::styled("back", label),
        Span::raw(HINT_GAP),
        Span::styled("q", key),
        Span::raw(" "),
        Span::styled("back", label),
    ])
}

/// The armed 2-step quit prompt: glyph-only line on the base `BG`; the absent background tint is
/// what tells it apart from a banner.
fn quit_alert(palette: &Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {} ", glyph::ALERT_MARKER), Style::new().fg(palette.warning)),
        Span::styled("press q again to quit", Style::new().fg(palette.text_dim)),
    ])
}

/// The run-completion alert: ` i ` for a clean run, ` ! ` for one with failures, message in
/// `TEXT_DIM`, exactly like the quit prompt a glyph-only line on the base surface.
fn completion_alert(palette: &Palette, alert: &RunAlert) -> Line<'static> {
    let (marker, color) = match alert.kind {
        AlertKind::Info => (glyph::ALERT_MARKER_INFO, palette.info),
        AlertKind::Warning => (glyph::ALERT_MARKER, palette.warning),
    };
    Line::from(vec![
        Span::styled(format!(" {marker} "), Style::new().fg(color)),
        Span::styled(alert.message.clone(), Style::new().fg(palette.text_dim)),
    ])
}
