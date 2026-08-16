//! The one-row app header: brand, tab bar, version (cloudy-tui skill: App shell → Default
//! header; Tab bar). The active tab carries its underline as a text attribute — there is no
//! separate underline row.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::alert::TabActivity;
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
/// active label — the brand lead, the `‹` marker, one [`TAB_GAP`] — plus the widest label with
/// its `●` cue ([`label_text`]). At exactly this width only the trailing `   ›` run falls off the
/// right edge, so the label itself is whole; a cell narrower and it starts losing characters.
#[must_use]
pub fn min_width() -> u16 {
    let lead: usize = lead_text().iter().map(String::as_str).map(cells).sum();
    let marker = format!("{}{TAB_GAP}", glyph::TAB_OVERFLOW_PREV);
    let widest = Tab::ALL.into_iter().map(|tab| cells(&label_text(tab, true))).max().unwrap_or(0);

    // A label past `u16::MAX` cells can never fit, so saturating keeps the row banner-only.
    u16::try_from(lead + cells(&marker) + widest).unwrap_or(u16::MAX)
}

/// Builds the header row for `width` cells, with the jump-key overlay when `alt_held` is true.
///
/// `activity` is one [`TabActivity`] per tab-bar position: an inactive tab carrying one renders
/// its label in the activity's semantic color instead of `TEXT_DIM` (cloudy-tui: Tab bar → Tab
/// activity). The active tab ignores it — being active is its own cue.
///
/// Right-edge suppression follows the contract's order (cloudy-tui: App shell → right-edge
/// suppression priority): the overlay drops first, then the version, and only then does the tab
/// strip collapse to the ` ‹   active   › ` overflow form. Tabs never drop. Once the version
/// is dropped it stays dropped — reinstating it after a collapse would make it reappear at a
/// *narrower* width than the one that hid it.
///
/// The ladder ends there. This builds a row at any width and never refuses one: below
/// [`min_width`] the overflow form simply overruns, and the active label loses characters
/// from the right until nothing of it is left.
#[must_use]
pub fn render(
    palette: &Palette, active: Tab, version: &str, width: u16, alt_held: bool, activity: &[Option<TabActivity>],
) -> Line<'static> {
    let width = width as usize;

    let lead = lead_spans(palette);
    let lead_width = total_width(&lead);

    let version = Span::styled(format!("v{version} "), Style::new().fg(palette.text_dim));
    let indexed = tab_spans(palette, active, true, activity);
    let plain = tab_spans(palette, active, false, activity);

    // The overlay is the first thing dropped, before the version and long before the overflow
    // form (cloudy-tui: Tab bar → Jump-key overlay — a transient hint must never trigger a layout
    // collapse). So the indexed strip is tried only WITH the version; when it does not fit, the
    // overlay is dropped outright and the plain strip's own version-or-not ladder runs. The version
    // never drops to keep a transient hint alive.
    let (tabs, version) = if alt_held && fits(lead_width, total_width(&indexed), version.width(), width) {
        (indexed, Some(version))
    } else if fits(lead_width, total_width(&plain), version.width(), width) {
        (plain, Some(version))
    } else if lead_width + total_width(&plain) <= width {
        (plain, None)
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

/// Whether `lead`, `tabs`, the version clearance and `version` fit in `width` together.
fn fits(lead: usize, tabs: usize, version: usize, width: usize) -> bool {
    lead + tabs + VERSION_CLEARANCE + version <= width
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

fn tab_spans(palette: &Palette, active: Tab, overlay: bool, activity: &[Option<TabActivity>]) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(Tab::ALL.len() * 4);
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(TAB_GAP));
        }
        let is_active = tab == active;
        if is_active {
            spans.push(active_marker(palette));
        }
        let jump = if overlay { tab.jump_index() } else { None };
        if let Some(digit) = jump {
            spans.extend(index_prefix(palette, digit));
        }
        let activity = activity.get(i).copied().flatten();
        spans.push(Span::styled(tab.label(), label_style(palette, is_active, activity)));
    }
    spans
}

/// Overflow form (Tab bar → Overflow): only the active label renders — `●` cue included — flanked
/// by markers for the tabs on either side. A marker disappears on the edge where no further tabs
/// exist, but its cell stays blank so the active label holds its column as the user moves across.
fn overflow_spans(palette: &Palette, active: Tab) -> Vec<Span<'static>> {
    let marker = Style::new().fg(palette.text_faint);
    let prev = if Tab::ALL.first() == Some(&active) { ' ' } else { glyph::TAB_OVERFLOW_PREV };
    let next = if Tab::ALL.last() == Some(&active) { ' ' } else { glyph::TAB_OVERFLOW_NEXT };

    vec![
        Span::styled(prev.to_string(), marker),
        Span::raw(TAB_GAP),
        active_marker(palette),
        Span::styled(active.label(), label_style(palette, true, None)),
        Span::raw(TAB_GAP),
        Span::styled(next.to_string(), marker),
    ]
}

/// The active tab's rendered label text: a leading `●` content cue that survives `NO_COLOR=1`,
/// where the accent, bold and underline all drop (design.md: TUI audit rulings — NO_COLOR content
/// cue). The panel title cannot carry the cue — memories and chat media both title their first
/// panel `setup` — so the cue has to live in the tab bar itself.
fn label_text(tab: Tab, active: bool) -> String {
    if active { format!("{} {}", glyph::STATUS_DOT_ACTIVE, tab.label()) } else { tab.label().to_string() }
}

/// The active tab's content cue, styled as part of the active label.
fn active_marker(palette: &Palette) -> Span<'static> {
    Span::styled(format!("{} ", glyph::STATUS_DOT_ACTIVE), Style::new().fg(palette.accent).bold())
}

/// One jump index `[N]` (cloudy-tui: Tab bar → Jump-key overlay): brackets `TEXT_DIM`, digit
/// `ACCENT + bold`. The caller places it flush against the label — no space.
fn index_prefix(palette: &Palette, digit: u8) -> [Span<'static>; 3] {
    let bracket = Style::new().fg(palette.text_dim);
    let digit_style = Style::new().fg(palette.accent).bold();
    [Span::styled("[", bracket), Span::styled(digit.to_string(), digit_style), Span::styled("]", bracket)]
}

/// A tab label's style. The active label is `ACCENT + bold + underline`; an inactive one is
/// `TEXT_DIM`, or the activity's semantic color when a background run left one (cloudy-tui: Tab
/// bar → Tab activity — the label takes the color, no underline rule beneath it).
fn label_style(palette: &Palette, active: bool, activity: Option<TabActivity>) -> Style {
    if active {
        Style::new().fg(palette.accent).bold().underlined()
    } else {
        match activity {
            Some(TabActivity::Success) => Style::new().fg(palette.success),
            Some(TabActivity::Warning) => Style::new().fg(palette.warning),
            Some(TabActivity::Danger) => Style::new().fg(palette.danger),
            None => Style::new().fg(palette.text_dim),
        }
    }
}
