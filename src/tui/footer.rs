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
use crate::tui::format::{cells, truncate_prose};

/// 3 spaces between hint groups, no glyph.
const HINT_GAP: &str = "   ";

/// The footer row for this frame: the caller's hint set, unless an alert or the armed-quit prompt
/// claims the row. The hint set is computed per tab by the shell (cloudy-tui: a hint advertises
/// only keys that do something), so `render` takes it ready-made rather than choosing between
/// hardcoded sets here. `width` is the footer row's cells: the run alert fits its message into
/// them, since a `Line` past the row clips at the terminal edge with no marker.
#[must_use]
pub fn render(palette: &Palette, quit_armed: bool, alert: Option<&RunAlert>, hints: Line<'static>, width: u16) -> Line<'static> {
    if quit_armed {
        quit_alert(palette)
    } else if let Some(alert) = alert {
        completion_alert(palette, alert, width)
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

/// The fallback hint set for tabs with no tab-specific keys: the shell's switch hint, then the
/// universal `a actions` / `? help` pair, then `q quit`. The `a actions` group is DERIVED from
/// `has_actions` — a screen with no actions drops it, so the hint never advertises a key that
/// opens nothing (cloudy-tui: a hint advertises only keys that do something).
pub(crate) fn plain_hints(palette: &Palette, has_actions: bool, width: u16) -> Line<'static> {
    // The hint bar's compact space-free arrow run — modals keep the spaced form.
    let arrows = format!("{}{}", glyph::KEY_LEFT, glyph::KEY_RIGHT);
    let mut groups: Vec<(&str, &str)> = vec![(arrows.as_str(), "switch")];
    if has_actions {
        groups.push(("a", "actions"));
    }
    groups.push(("?", "help"));
    groups.push(("q", "quit"));
    hint_line(palette, &trim_hints(&groups, width))
}

/// The hint set while the active screen's table pane is descended: arrows move the caret, `←`,
/// `esc` and `q` all ascend, and `→` is inert so it gets no hint. This is the generic descended
/// set, chosen for every screen except the history tab, whose formats pane binds `space` and `↵`
/// and so takes [`history_descended_hints`] instead.
pub(crate) fn descended_hints(palette: &Palette, width: u16) -> Line<'static> {
    let up_down = format!("{}{}", glyph::KEY_UP, glyph::KEY_DOWN);
    let left = glyph::KEY_LEFT.to_string();
    hint_line(palette, &trim_hints(&[(&up_down, "move"), (&left, "back"), ("esc", "back"), ("?", "help"), ("q", "back")], width))
}

/// The hint set while the history tab's formats pane is descended: arrows move the caret,
/// `space` toggles the focused format, `↵` toggles a format or runs the export on the chip, and
/// `←`/`esc`/`q` ascend. The generic descended set advertises neither of the two pane-specific
/// keys, which is why this exists rather than the shell reusing [`descended_hints`] per tab. The
/// `↵` label names both what it does, since the key toggles on a format row and exports on the
/// chip — a single `export` label would lie on the four checkbox rows.
pub(crate) fn history_descended_hints(palette: &Palette, width: u16) -> Line<'static> {
    let up_down = format!("{}{}", glyph::KEY_UP, glyph::KEY_DOWN);
    let left = glyph::KEY_LEFT.to_string();
    let enter = glyph::KEY_ENTER.to_string();
    hint_line(
        palette,
        &trim_hints(
            &[
                (&up_down, "move"),
                ("space", "toggle"),
                (&enter, "toggle / export"),
                (&left, "back"),
                ("esc", "back"),
                ("?", "help"),
                ("q", "back"),
            ],
            width,
        ),
    )
}

/// The hint set while the action menu owns input: arrows move the caret (wrapping), `↵` picks, and
/// `esc`/`q` both cancel. Deliberately no `←→ switch`, `? help` or `q quit` group — while a modal is
/// open those keys are the modal's to ignore, so advertising them would mislabel what the key does
/// this frame (cloudy-tui: Modals — the footer hint bar is context-aware while a modal is open).
pub(crate) fn action_menu_hints(palette: &Palette, width: u16) -> Line<'static> {
    let up_down = format!("{}{}", glyph::KEY_UP, glyph::KEY_DOWN);
    let enter = glyph::KEY_ENTER.to_string();
    hint_line(palette, &trim_hints(&[(&up_down, "move"), (&enter, "pick"), ("esc", "cancel"), ("q", "back")], width))
}

/// The hint set while the help modal owns input: `?`, `esc` and `q` all close it, and the compact
/// `↑↓` run scrolls it — but only while the content is taller than the viewport, so the hint never
/// advertises a scroll key that does nothing this frame (cloudy-tui: a hint advertises only keys
/// that do something; Hint bar → density allowance for the compact run). No universal `a actions`
/// / `? help` pair and no switch/quit group — the modal owns every key this frame, so the labels
/// name what those keys actually do now.
pub(crate) fn help_hints(palette: &Palette, scrollable: bool, width: u16) -> Line<'static> {
    let up_down = format!("{}{}", glyph::KEY_UP, glyph::KEY_DOWN);
    let mut groups: Vec<(&str, &str)> = Vec::new();
    if scrollable {
        groups.push((up_down.as_str(), "move"));
    }
    groups.push(("?", "close"));
    groups.push(("esc", "cancel"));
    groups.push(("q", "back"));
    hint_line(palette, &trim_hints(&groups, width))
}

/// The history tab's top-level hint set: the shell's switch and quit groups, plus the picker's
/// own keys. The toggle hints are DERIVED from the picker holding rows for them to act on —
/// derived, never copied per branch — so a failed or empty load leaves a row that advertises
/// nothing (finding 7).
pub(crate) fn history_hints(palette: &Palette, picker_has_rows: bool, width: u16) -> Line<'static> {
    let arrows = format!("{}{}", glyph::KEY_LEFT, glyph::KEY_RIGHT);
    let mut groups: Vec<(&str, &str)> = vec![(arrows.as_str(), "switch")];
    if picker_has_rows {
        groups.push(("t", "toggle all"));
        groups.push(("space", "toggle"));
        groups.push(("a", "actions"));
    }
    groups.push(("?", "help"));
    groups.push(("q", "quit"));
    hint_line(palette, &trim_hints(&groups, width))
}

/// The settings tab's hint set while a text input is being edited: arrows move the caret
/// rather than the tab, `↵` commits the draft, `esc` cancels it, and `q` types a letter — so
/// the switch/quit set would advertise keys that do something else this frame, and the edit
/// set replaces it (cloudy-tui: a hint advertises only keys that do something).
pub(crate) fn settings_edit_hints(palette: &Palette, width: u16) -> Line<'static> {
    let arrows = format!("{}{}", glyph::KEY_LEFT, glyph::KEY_RIGHT);
    let enter = glyph::KEY_ENTER.to_string();
    hint_line(palette, &trim_hints(&[(&arrows, "move"), (&enter, "commit"), ("esc", "cancel")], width))
}

/// The armed 2-step quit prompt: glyph-only line on the base `BG`; the absent background tint is
/// what tells it apart from a banner. Short by construction, so it needs no cut.
fn quit_alert(palette: &Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {} ", glyph::ALERT_MARKER), Style::new().fg(palette.warning)),
        Span::styled("press q again to quit", Style::new().fg(palette.text_dim)),
    ])
}

/// The run-completion alert: ` i ` for a clean run, ` ! ` for one that needs attention
/// (`WARNING`) or failed (`DANGER`), message in `TEXT_DIM`, exactly like the quit prompt a
/// glyph-only line on the base surface.
///
/// The message is fit into the row: a `Line` past the row clips at the terminal edge with no
/// marker, which is a cut the reader cannot tell from the message ending there.
fn completion_alert(palette: &Palette, alert: &RunAlert, width: u16) -> Line<'static> {
    let (marker, color) = match alert.kind {
        AlertKind::Info => (glyph::ALERT_MARKER_INFO, palette.info),
        AlertKind::Warning => (glyph::ALERT_MARKER, palette.warning),
        AlertKind::Danger => (glyph::ALERT_MARKER, palette.danger),
    };
    // The marker's own three cells — space, glyph, space; the message gets every remaining
    // cell, no right inset.
    let budget = usize::from(width).saturating_sub(3);
    Line::from(vec![
        Span::styled(format!(" {marker} "), Style::new().fg(color)),
        Span::styled(fit_alert_message(&alert.message, budget), Style::new().fg(palette.text_dim)),
    ])
}

/// The alert message fit to `budget` cells, visible cut included (contract: prose cut, trailing
/// ellipsis).
///
/// The failure messages join a statement to its fix with `"; "`, and the fix half is the part
/// the user acts on — so when the row cannot hold both, the fix half renders whole and the
/// statement half takes the cut. That split respects the alert module's privacy rule by
/// construction: the fix half never carries the export's own bytes, so the cut cannot surface
/// one. Messages without the statement;fix idiom (completions, load errors) take the plain
/// prose cut.
fn fit_alert_message(message: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if cells(message) <= budget {
        return message.to_owned();
    }
    if let Some((head, tail)) = message.rsplit_once("; ") {
        let tail_cells = cells(tail);
        // The fix half plus its `"; "` lead, rendered whole; the statement half keeps what the
        // row leaves, its ellipsis included.
        if tail_cells + 2 < budget {
            return format!("{}; {tail}", truncate_prose(head, budget - tail_cells - 2));
        }
        // The fix half alone nearly fills the row: it renders with a leading ellipsis naming
        // the dropped error half.
        return format!("…{}", truncate_prose(tail, budget.saturating_sub(1)));
    }
    truncate_prose(message, budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one message shape every `RunError` failure spells: a statement joined to its fix.
    const STATEMENT_FIX: &str =
        "no mydata~ export part under /mnt/data/snapshots/very-long-directory-name; point the source at the dir holding the export's parts";

    #[test]
    fn a_message_that_fits_its_budget_renders_whole() {
        assert_eq!(fit_alert_message("run finished · 1 fixed", 22), "run finished · 1 fixed");
        assert_eq!(fit_alert_message(STATEMENT_FIX, cells(STATEMENT_FIX)), STATEMENT_FIX);
    }

    #[test]
    fn an_overflowing_statement_fix_message_keeps_the_fix_half_whole_and_cuts_the_statement() {
        // 116 cells, a wide footer's budget: the fix half (54 cells plus its "; " lead)
        // renders whole, the statement takes the prose cut into what remains, and the cut is
        // named by the ellipsis.
        let fit = fit_alert_message(STATEMENT_FIX, 116);
        assert!(fit.ends_with("; point the source at the dir holding the export's parts"), "{fit}");
        assert!(fit.starts_with("no mydata~ export part under "), "{fit}");
        assert!(fit.contains("…; point the source"), "the cut sits between the halves: {fit}");
        assert_eq!(cells(&fit), 116, "the fit never overruns the row: {fit}");
        assert!(!fit.contains("very-long-directory-name"), "the statement half is the part that cuts: {fit}");
    }

    #[test]
    fn a_fix_half_that_nearly_fills_the_row_renders_with_a_leading_ellipsis() {
        // A 56-cell budget holds the fix half alone: the ellipsis names the dropped error
        // half, so the cut stays visible even when nothing of the statement survives.
        let fit = fit_alert_message(STATEMENT_FIX, 56);
        assert_eq!(fit, "…point the source at the dir holding the export's parts");
    }

    #[test]
    fn a_message_without_the_statement_fix_idiom_takes_the_plain_prose_cut() {
        let fit = fit_alert_message("run finished · 12 fixed · 3 failed", 20);
        assert_eq!(fit, "run finished · 12 f…");
        assert_eq!(fit_alert_message("run finished · 12 fixed · 3 failed", 0), "");
    }

    #[test]
    fn a_zero_budget_fits_nothing_on_either_branch() {
        // The plain branch pins `""` at budget 0; the statement;fix split must answer the same
        // rather than overrunning with the leading ellipsis it would render at any real budget.
        assert_eq!(fit_alert_message(STATEMENT_FIX, 0), "");
    }

    #[test]
    fn a_fix_clause_longer_than_the_row_is_itself_prose_cut_with_the_marker() {
        // 35 cells: even the fix half cannot render whole, so it takes the prose cut behind
        // the leading ellipsis — the cut stays visible in both halves.
        let message = "no memory media under /somewhere; extract the export's memories dirs first";
        let fit = fit_alert_message(message, 35);
        assert!(fit.starts_with("…extract the export's memories dir"), "{fit}");
        assert!(fit.ends_with('…'), "{fit}");
        assert_eq!(cells(&fit), 35);
    }
}
