//! The one-row app header: brand, tab bar, version (cloudy-tui skill: App shell → Default
//! header; Tab bar). The active tab carries its underline as a text attribute — there is no
//! separate underline row.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::theme::{Palette, glyph};
use crate::app::Tab;

const BRAND: &str = "exportsnap";
/// Tab labels are 3-space separated, both in the normal row and around the overflow markers.
const TAB_GAP: &str = "   ";
/// A right-aligned column keeps at least this much clearance from the content to its left
/// (cloudy-tui skill: Patterns → Spacing), so the version drops rather than crowding a tab.
const VERSION_CLEARANCE: usize = 3;

/// The narrowest row that can still say which tab is active.
///
/// Derived from the overflow form rather than picked: everything that form puts left of the
/// active label — the brand lead, the `‹` marker, one [`TAB_GAP`] — plus the widest label.
/// At exactly this width only the trailing `   ›` run falls off the right edge, so the label
/// itself is whole; a cell narrower and it starts losing characters.
#[must_use]
pub fn min_width() -> u16 {
    let lead: usize = lead_text().iter().map(String::as_str).map(cells).sum();
    let marker = format!("{}{TAB_GAP}", glyph::TAB_OVERFLOW_PREV);
    let widest = Tab::ALL.into_iter().map(|tab| cells(tab.label())).max().unwrap_or(0);

    // A label past `u16::MAX` cells can never fit, so saturating keeps the row banner-only.
    u16::try_from(lead + cells(&marker) + widest).unwrap_or(u16::MAX)
}

/// Builds the header row for `width` cells.
///
/// Right-edge suppression follows the contract's order: the version drops first, then the tab
/// strip collapses to the ` ‹   active   › ` overflow form. Tabs never drop. Once the version
/// is dropped it stays dropped — reinstating it after a collapse would make it reappear at a
/// *narrower* width than the one that hid it.
///
/// The ladder ends there. This builds a row at any width and never refuses one: below
/// [`min_width`] the overflow form simply overruns, and the active label loses characters
/// from the right until nothing of it is left.
#[must_use]
pub fn render(palette: &Palette, active: Tab, version: &str, width: u16) -> Line<'static> {
    let width = width as usize;

    let lead = lead_spans(palette);
    let lead_width = total_width(&lead);

    let version = Span::styled(format!("v{version} "), Style::new().fg(palette.text_dim));
    let tabs = tab_spans(palette, active);
    let tabs_width = total_width(&tabs);

    let (tabs, version) = if lead_width + tabs_width + VERSION_CLEARANCE + version.width() <= width {
        (tabs, Some(version))
    } else if lead_width + tabs_width <= width {
        (tabs, None)
    } else {
        (overflow_spans(palette, active), None)
    };

    let version_width = version.as_ref().map_or(0, Span::width);
    let gap = width.saturating_sub(lead_width + total_width(&tabs) + version_width);

    let mut spans = lead;
    spans.extend(tabs);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(version);
    Line::from(spans)
}

fn total_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// Cell width, through the same unicode-aware measure the rendered spans carry, so a label
/// that is not one byte per cell still sizes [`min_width`] correctly.
fn cells(text: &str) -> usize {
    Span::raw(text).width()
}

/// The brand run's text, split from its styling so [`min_width`] measures the very strings
/// [`render`] draws.
fn lead_text() -> [String; 2] {
    [format!(" {BRAND}"), format!("  {}  ", glyph::HEADER_SEPARATOR)]
}

fn lead_spans(palette: &Palette) -> Vec<Span<'static>> {
    let [brand, separator] = lead_text();

    vec![Span::styled(brand, Style::new().fg(palette.accent_2).bold()), Span::styled(separator, Style::new().fg(palette.text_dim))]
}

fn tab_spans(palette: &Palette, active: Tab) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(Tab::ALL.len() * 2);
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(TAB_GAP));
        }
        spans.push(Span::styled(tab.label(), label_style(palette, tab == active)));
    }
    spans
}

/// Overflow form (Tab bar → Overflow): only the active label renders, flanked by markers for
/// the tabs on either side. A marker disappears on the edge where no further tabs exist, but
/// its cell stays blank so the active label holds its column as the user moves across tabs.
fn overflow_spans(palette: &Palette, active: Tab) -> Vec<Span<'static>> {
    let marker = Style::new().fg(palette.text_faint);
    let prev = if Tab::ALL.first() == Some(&active) { ' ' } else { glyph::TAB_OVERFLOW_PREV };
    let next = if Tab::ALL.last() == Some(&active) { ' ' } else { glyph::TAB_OVERFLOW_NEXT };

    vec![
        Span::styled(prev.to_string(), marker),
        Span::raw(TAB_GAP),
        Span::styled(active.label(), label_style(palette, true)),
        Span::raw(TAB_GAP),
        Span::styled(next.to_string(), marker),
    ]
}

fn label_style(palette: &Palette, active: bool) -> Style {
    if active { Style::new().fg(palette.accent).bold().underlined() } else { Style::new().fg(palette.text_dim) }
}
