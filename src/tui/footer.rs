//! The single footer row: the hint bar, the footer alert that replaces it while the 2-step quit
//! is armed, or the run-completion alert (cloudy-tui skill: Hint bar; Footer alert). Exactly one
//! of the three per frame — never stacked.
//!
//! Precedence: the armed-quit prompt is a transient needs-action, so it wins over the completion
//! alert; the completion alert replaces the hints. The hint set itself is per tab: the shell
//! answers it off the active screen and passes it ready-made, so the row never advertises a key
//! that does something else on this frame.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::theme::{Palette, glyph};
use crate::tui::alert::{AlertKind, RunAlert};
use crate::tui::format::cells;

/// 3 spaces between hint groups, no glyph.
const HINT_GAP: &str = "   ";

/// The footer row for this frame: the caller's hint set, unless an alert or the armed-quit prompt
/// claims the row. The hint set is computed per tab by the shell (cloudy-tui: a hint advertises
/// only keys that do something), so `render` takes it ready-made rather than choosing between
/// hardcoded sets here.
#[must_use]
pub fn render(palette: &Palette, quit_armed: bool, alert: Option<&RunAlert>, hints: Line<'static>) -> Line<'static> {
    if quit_armed {
        quit_alert(palette)
    } else if let Some(alert) = alert {
        completion_alert(palette, alert)
    } else {
        hints
    }
}

/// One hint set's spans: each group's key in accent bold and its label in dim, run together by
/// [`HINT_GAP`]. Every set on the row is one of these lines, so the groups' styling cannot drift
/// between sets.
fn hint_line(palette: &Palette, groups: &[(&str, &str)]) -> Line<'static> {
    let key = Style::new().fg(palette.accent).bold();
    let label = Style::new().fg(palette.text_dim);
    let mut spans = vec![Span::raw(" ")];
    for (index, (key_char, action)) in groups.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(HINT_GAP));
        }
        spans.push(Span::styled((*key_char).to_owned(), key));
        spans.push(Span::raw(" "));
        spans.push(Span::styled((*action).to_owned(), label));
    }
    Line::from(spans)
}

/// Drops hint groups from the right until the set fits `width`, keeping the last group — the
/// escape hint — to the end. The history set's full 49 cells clips `q quit` across the
/// picker-only arm's own frame range (34-48), and the escape hint is the one a narrow frame
/// must not lose (reviewer #2).
fn trim_hints<'a>(groups: &'a [(&'a str, &'a str)], width: u16) -> Vec<(&'a str, &'a str)> {
    let Some((escape, head)) = groups.split_last() else {
        return Vec::new();
    };
    let mut kept = head.to_vec();
    while !kept.is_empty() {
        let mut candidate = kept.clone();
        candidate.push(*escape);
        if hint_width(&candidate) <= usize::from(width) {
            break;
        }
        kept.pop();
    }
    kept.push(*escape);
    kept
}

/// One hint set's cells: the leading space, then each group's key, its space, and its label,
/// joined by [`HINT_GAP`] — exactly the spans [`hint_line`] renders.
fn hint_width(groups: &[(&str, &str)]) -> usize {
    1 + groups
        .iter()
        .enumerate()
        .map(|(index, (key, label))| cells(key) + 1 + cells(label) + usize::from(index > 0) * HINT_GAP.len())
        .sum::<usize>()
}

/// The fallback hint set for tabs with no tab-specific keys. The universal `a actions` /
/// `? help` hints are deliberately absent: `a` is reserved for the action menu, which no screen
/// implements — the history tab's toggle-all binds `t`, the contract's hotkey-algorithm letter,
/// rather than colliding with the reserved `a` — so rendering `a actions` would advertise a
/// binding that does nothing, and `? help` is not bound at all.
pub(crate) fn plain_hints(palette: &Palette, width: u16) -> Line<'static> {
    // The hint bar's compact space-free arrow run — modals keep the spaced form.
    let arrows = format!("{}{}", glyph::KEY_LEFT, glyph::KEY_RIGHT);
    hint_line(palette, &trim_hints(&[(&arrows, "switch"), ("q", "quit")], width))
}

/// The hint set while the active screen's table pane is descended: arrows move the caret, `←`,
/// `esc` and `q` all ascend, and `→` is inert so it gets no hint. Identical on every screen, so
/// the shell chooses it off `descended` rather than per tab.
pub(crate) fn descended_hints(palette: &Palette, width: u16) -> Line<'static> {
    let up_down = format!("{}{}", glyph::KEY_UP, glyph::KEY_DOWN);
    let left = glyph::KEY_LEFT.to_string();
    hint_line(palette, &trim_hints(&[(&up_down, "move"), (&left, "back"), ("esc", "back"), ("q", "back")], width))
}

/// The history tab's top-level hint set: the shell's switch and quit groups, plus the picker's
/// own keys. The toggle hints are DERIVED from the picker holding rows for them to act on —
/// derived, never copied per branch — so a failed or empty load leaves a row that advertises
/// nothing (finding 7).
pub(crate) fn history_hints(palette: &Palette, picker_has_rows: bool, width: u16) -> Line<'static> {
    let arrows = format!("{}{}", glyph::KEY_LEFT, glyph::KEY_RIGHT);
    if picker_has_rows {
        hint_line(palette, &trim_hints(&[(&arrows, "switch"), ("t", "toggle all"), ("space", "toggle"), ("q", "quit")], width))
    } else {
        hint_line(palette, &trim_hints(&[(&arrows, "switch"), ("q", "quit")], width))
    }
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
