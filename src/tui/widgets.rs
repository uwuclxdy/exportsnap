//! Widget builders shared by the app shell and the per-tab screens.
//!
//! The lower half of this module is the run-screen kit: the rows, pills, bars, empty state and
//! progress table the memories and chat-media screens both draw. They are here rather than copied
//! into each screen because the contract they render is one contract — a second spelling of a status
//! pill is two places a column width has to stay true. What is NOT here is either screen's
//! composition: the two disagree about which form rows exist and about the counts line, and a shared
//! composer would have to take that disagreement as parameters.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::symbols::{block, line, shade};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use super::format::{binary_bytes, cells, middle_ellipsis, right_pad};
use super::theme::{Palette, glyph};
use crate::export::env::Environment;
use crate::export::manifest::ItemStatus;

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

/// The framed empty state's rows: the hint, the action line, and the frame's own two borders.
pub(crate) const EMPTY_STATE_ROWS: u16 = 4;

// ---- the run-screen kit ----

/// The caret gutter every focusable row leads with.
pub(crate) const CARET_GUTTER: usize = 2;
/// The gap between a ragged row's label and its value (contract: ≥ 2 spaces).
pub(crate) const LABEL_GAP: usize = 2;
/// Cells the disk-free usage bar occupies at its widest.
pub(crate) const DISK_BAR_CELLS: usize = 9;
/// Cells a progress table's identity column occupies (a middle-ellipsised id).
pub(crate) const IDENTITY_CELLS: usize = 18;
/// Cells a progress table's location column occupies (decision 76) — sized for the observed
/// place-name range, 29-42 chars: the shortest observed name renders whole, longer ones
/// middle-ellipsise and the focused row's tooltip shows the full name.
pub(crate) const LOCATION_CELLS: usize = 29;
/// Cells a progress table's status column occupies — the widest pill, `[ pending ]`.
pub(crate) const STATUS_CELLS: usize = 11;
/// The gap between two of a progress table's columns.
pub(crate) const COLUMN_GAP: usize = 2;
/// The narrowest the output column may be before the panel gives up on the whole table. Shared
/// with the two screens' table-interior floor, so the distribution never shrinks below what the
/// floor promises.
pub(crate) const OUTPUT_MIN: usize = 6;

/// The row's leading glyph: the selection caret in the focused pane, two blank cells otherwise.
pub(crate) fn caret(palette: &Palette, focused: bool) -> Span<'static> {
    if focused { Span::styled(format!("{} ", glyph::SELECTION_CARET), Style::new().fg(palette.accent).bold()) } else { Span::raw("  ") }
}

/// Pads a selected row's tint out to the panel's interior edge (contract: Tint extent — the
/// `BG_HOVER` tint spans the full content width, to the padding boundary).
///
/// A `Line` style inside a `Paragraph` paints only the spans' own cells: Paragraph renders lines
/// through `styled_graphemes`, which emits span cells alone. The tint therefore needs an explicit
/// filler span out to the panel's interior width, which is why every caller has to know that width.
pub(crate) fn tint_to_edge(mut line: Line<'static>, width: usize, palette: &Palette) -> Line<'static> {
    let fill = width.saturating_sub(line.width());
    if fill > 0 {
        line.spans.push(Span::styled(" ".repeat(fill), Style::new().bg(palette.bg_hover)));
    }
    line
}

/// One informational key:value row, ragged per the focusable-row grammar: the key is `TEXT_DIM +
/// bold` at all times (the static-key anchor, not a focus cue), and the value trails it with a
/// ≥ 2-space gap rather than padding to a shared column — these rows take the caret, and padded
/// focusable rows read as a static table.
pub(crate) fn static_row(
    palette: &Palette, caret: Span<'static>, label: &str, value: Vec<Span<'static>>, selected: bool, width: usize,
) -> Line<'static> {
    let mut spans = vec![caret, Span::styled(label.to_owned(), Style::new().fg(palette.text_dim).bold()), Span::raw("  ")];
    spans.extend(value);
    let line = Line::from(spans);
    if selected { tint_to_edge(line.style(Style::new().bg(palette.bg_hover)), width, palette) } else { line }
}

/// A non-focusable key:value display row (contract: Static key:value rows, the column-aligned
/// form): the key is `TEXT_DIM + bold` at all times (the static-key anchor, not a focus cue),
/// padded to `column` so its value stacks under the group's widest label, no colon, no caret, no
/// selection tint. The run screens' informational rows (source, output dir, disk free) render this
/// now that they are display-only (ruling: static informational rows are non-focusable); a
/// focusable row keeps [`static_row`]'s ragged grammar instead.
pub(crate) fn display_row(palette: &Palette, label: &str, column: usize, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:<column$}"), Style::new().fg(palette.text_dim).bold())];
    spans.extend(value);
    Line::from(spans)
}

/// The value budget a form's path row gets at `width`: the interior cells left after the caret,
/// the label and the gap, floored at `floor` so the narrow side-by-side form keeps its tight value
/// column (the two run screens' `PATH_CELLS`). In the full-width arms the row uses whatever the
/// panel actually leaves, so a path shows whole instead of truncating to the narrow column.
#[must_use]
pub(crate) fn path_budget(width: usize, label: &str, floor: usize) -> usize {
    width.saturating_sub(CARET_GUTTER + cells(label) + LABEL_GAP).max(floor)
}

/// The setup-form panel width a run screen's side-by-side arm should use, in cells: the screen's
/// narrow interior floor grown to fit its longest raw path value (source or output dir), then
/// capped so the progress table keeps its own interior floor. Callers still gate side-by-side on
/// the floor width (`floor_interior + CHROME_COLUMNS` plus the table floor), so a body too narrow
/// for that floor stacks full-width instead of blanking the form; when the cap binds the paths
/// head-ellipsize within it, when it does not they render whole.
#[must_use]
pub(crate) fn side_by_side_form_panel_width(
    body_width: usize, floor_interior: usize, widest_label: usize, longest_path: usize, table_interior_min: usize,
) -> usize {
    let form_interior = floor_interior.max(CARET_GUTTER + widest_label + LABEL_GAP + longest_path);
    let table_panel = table_interior_min + usize::from(CHROME_COLUMNS);
    (form_interior + usize::from(CHROME_COLUMNS)).min(body_width.saturating_sub(table_panel))
}

/// An INTERACTIVE row's label: `TEXT_DIM` blurred, promoted to `TEXT + bold` when the row is
/// focused (contract: Forms — the focused row's label promotes).
///
/// The sibling of [`static_row`]'s key and deliberately not the same span: a static key is
/// `TEXT_DIM + bold` at all times, where the bold is a fixed anchor, and here the bold IS the
/// current-row cue. Rendering either one in the other's treatment is a contract bug, which is why
/// the two spellings live side by side rather than one taking a flag.
pub(crate) fn form_label(palette: &Palette, label: &str, focused: bool) -> Span<'static> {
    if focused {
        Span::styled(label.to_owned(), Style::new().fg(palette.text).bold())
    } else {
        Span::styled(label.to_owned(), Style::new().fg(palette.text_dim))
    }
}

/// The cycle control (contract: Cycle row): bare lowercase words in 2-space gaps, the selected
/// one `ACCENT` — no bold, because the focused row's own label already carries the current-row
/// bold — wrapped in `[brackets]` **only while the row is focused**. The bracket is the focus
/// cue and the accent is the selection cue; unselected options are `TEXT_FAINT`.
///
/// One spelling of the grammar, so the chat-media and settings forms cannot drift apart on it.
/// The two-cell widening a focused row's brackets cause is the intended focus signal; each
/// form's interior budget reserves it (their `CYCLE_CELLS` constants restate the width from
/// their own rosters, the way the two screens' path rows restate theirs).
pub(crate) fn cycle_options(palette: &Palette, words: &[&'static str], selected: usize, focused: bool) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(words.len() * 2);
    for (index, word) in words.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        if index == selected {
            let style = Style::new().fg(palette.accent);
            if focused {
                spans.push(Span::styled(format!("[{word}]"), style));
            } else {
                spans.push(Span::styled((*word).to_owned(), style));
            }
        } else {
            spans.push(Span::styled((*word).to_owned(), Style::new().fg(palette.text_faint)));
        }
    }
    spans
}

/// The bare glyph run of a determinate bar: `█` fill in `fill_style`, `░` track in `LINE`.
pub(crate) fn bar_run(palette: &Palette, fill: usize, total: usize, fill_style: Style) -> Vec<Span<'static>> {
    let fill = fill.min(total);
    vec![Span::styled(block::FULL.repeat(fill), fill_style), Span::styled(shade::LIGHT.repeat(total - fill), palette.bar_track())]
}

/// The disk-free value plus the usage-role bar (decision 41: the banner is deferred, the bar
/// ships). The bar shows the USED share of the disk, because usage-role colors mean higher=worse.
///
/// The bar is the row's elastic part: the byte figure can run to "16384.0 PiB" (u64::MAX, 10
/// cells), and a fixed bar then pushes the trailing percent past the panel edge. The bar takes
/// whatever `budget` — the row's cells after caret, label and gap — leaves, capped at
/// [`DISK_BAR_CELLS`], so the percent never clips.
pub(crate) fn disk_free_value(palette: &Palette, environment: &Environment, budget: usize) -> Vec<Span<'static>> {
    let (Some(free), Some(total)) = (environment.available_space, environment.total_space) else {
        return vec![Span::styled("unknown", Style::new().fg(palette.warning))];
    };
    if total == 0 {
        return vec![Span::styled("unknown", Style::new().fg(palette.warning))];
    }
    // Clamped on the FREE share, before the subtraction, and the order is load-bearing rather than
    // stylistic. `Environment`'s fields are public, so `available_space > total_space` is a value a
    // caller can construct; the cast then saturates at 255 and `100u8 - 255u8` panics in debug and
    // wraps in release. Clamping the USED share after the subtraction would close the same hole and
    // silently move a percentage on sane input: Rust rounds half away from zero, so a free share of
    // 60.5 rounds to 61 and reports 39% used, where subtracting first gives 39.5 and reports 40%.
    let free_percent = (free as f64 / total as f64 * 100.0).round().clamp(0.0, 100.0) as u8;
    let used = 100 - free_percent;
    let free_text = binary_bytes(free);
    let percent = format!("{used}%");
    // One space either side of the bar; the bar shrinks first.
    let bar_cells = budget.saturating_sub(cells(&free_text) + cells(&percent) + 3).min(DISK_BAR_CELLS);
    let fill = usize::from(used) * bar_cells / 100;
    let mut spans = vec![Span::styled(free_text, Style::new().fg(palette.text)), Span::raw(" ")];
    spans.extend(bar_run(palette, fill, bar_cells, Style::new().fg(palette.usage_color(used))));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(percent, Style::new().fg(palette.text_dim)));
    spans
}

/// An action-only chip (contract: Action-only chip), in the CTA variant — accent at rest on the
/// raised fill, inverse block on focus — because starting a run is a screen's one primary action.
/// A disabled chip is focusable-but-inert and reads faint.
///
/// The rest and disabled fill is a raised surface, so it paints through
/// [`Palette::surface_raised`]: on the `compatible` tier it is unpainted (DNA rule 3) and the chip
/// carries no background at rest or disabled. The focused inverse block (`ACCENT` fill, `BG` text)
/// is not a surface fill and is unchanged on both tiers.
pub(crate) fn action_chip(palette: &Palette, label: &str, enabled: bool, focused: bool) -> Span<'static> {
    let (fg, bg, bold) = if !enabled {
        (palette.text_faint, palette.surface_raised(), false)
    } else if focused {
        (palette.bg, Some(palette.accent), true)
    } else {
        (palette.accent, palette.surface_raised(), true)
    };
    let mut style = Style::new().fg(fg);
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    Span::styled(format!(" {label} "), if bold { style.bold() } else { style })
}

/// The status pill (contract: Status pill): brackets `TEXT_DIM`, label semantic and bold, padded to
/// [`STATUS_CELLS`] so the column after it lines up whatever the status.
///
/// The words are the user-facing verbs, not the manifest's stored spellings: `source_missing` reads
/// `missing`, and `excluded` reads `dropped` — decision 44d's own word for what happens to a
/// thumbnail, which at seven characters also fits the column every other pill pads out to.
pub(crate) fn status_pill(palette: &Palette, status: ItemStatus) -> Vec<Span<'static>> {
    let label = match status {
        ItemStatus::Pending => "pending",
        ItemStatus::Done => "done",
        ItemStatus::Failed => "failed",
        ItemStatus::SourceMissing => "missing",
        ItemStatus::Retired => "retired",
        ItemStatus::Excluded => "dropped",
        // Never rendered: `Manifest::items` excludes claims (decision 63a), so no table row ever
        // carries one. The arm exists for exhaustiveness.
        ItemStatus::Claimed => "claimed",
    };
    let bracket = Style::new().fg(palette.text_dim);
    let width = 2 + label.len() + 2;
    let mut spans = vec![
        Span::styled("[ ", bracket),
        Span::styled(label, Style::new().fg(palette.status_pill(status)).bold()),
        Span::styled(" ]", bracket),
    ];
    if width < STATUS_CELLS {
        spans.push(Span::raw(" ".repeat(STATUS_CELLS - width)));
    }
    spans
}

/// The framed empty state (contract: Empty state): a hint line, then an action line naming the key
/// that starts the run, its glyph in `ACCENT`.
pub(crate) fn empty_state(frame: &mut Frame, palette: &Palette, inner: Rect, hint: &str) {
    const INSET: u16 = 3;

    let action = Line::from(vec![
        Span::styled("press ", Style::new().fg(palette.text_dim)),
        Span::styled(glyph::KEY_ENTER.to_string(), Style::new().fg(palette.accent).bold()),
        Span::styled(" to start", Style::new().fg(palette.text_dim)),
    ]);
    let width = u16::try_from(cells(hint).max(16) + 2 * usize::from(INSET) + 2).unwrap_or(u16::MAX);
    let frame_area = inner.centered(Constraint::Length(width), Constraint::Length(EMPTY_STATE_ROWS));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(palette.line))
        .padding(Padding::new(INSET, INSET, 0, 0));
    frame.render_widget(Paragraph::new(vec![Line::styled(hint, Style::new().fg(palette.text_dim)), action]).block(block), frame_area);
}

/// One row of a progress table, borrowed from whichever leg's plan produced it.
///
/// `identity` and `output` are both `&str` rather than a leg's own row type, which is what lets one
/// renderer serve two runs whose event payloads are their own. **`output` is a file NAME**: the
/// chat leg's output path carries a conversation directory derived from a friend's username, so a
/// path here would be a privacy defect and not merely a wide column.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProgressRow<'a> {
    pub identity: &'a str,
    /// The entry's place name (decision 76). `None` renders an empty cell and no tooltip; the chat
    /// leg has no such concept and passes `None` for every row.
    pub location: Option<&'a str>,
    pub output: &'a str,
    pub status: ItemStatus,
}

/// The three flexible widths a progress table's columns get, computed once per render so the
/// header and the rows split the same cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressColumns {
    pub identity: usize,
    pub location: usize,
    pub output: usize,
}

impl ProgressColumns {
    /// Splits the panel's interior width into the identity, location and output columns.
    ///
    /// The caret gutter, the three column gaps and the status pill are fixed; what is left feeds
    /// the three flexible columns, each floored at [`IDENTITY_CELLS`], [`LOCATION_CELLS`] and
    /// [`OUTPUT_MIN`]. The output filename is the deliverable — its date prefix is the metadata this
    /// app restores — so it takes its full width first. Identity and location are the flexible
    /// columns: they middle-ellipsize, and share whatever remains after the output is whole. A blank
    /// column (the chat leg has no place name) therefore keeps its floor instead of eating the
    /// surplus a sibling needs.
    #[must_use]
    pub(crate) fn for_width(width: usize, max_identity: usize, max_location: usize, max_output: usize) -> Self {
        let mut flexible = width.saturating_sub(CARET_GUTTER + 3 * COLUMN_GAP + STATUS_CELLS);
        let identity = IDENTITY_CELLS.min(flexible);
        flexible -= identity;
        let location = LOCATION_CELLS.min(flexible);
        flexible -= location;
        let output = OUTPUT_MIN.min(flexible);
        flexible -= output;
        let output_growth = flexible.min(max_output.saturating_sub(output));
        flexible -= output_growth;
        let identity_growth = flexible.min(max_identity.saturating_sub(identity));
        flexible -= identity_growth;
        let location_growth = flexible.min(max_location.saturating_sub(location));
        flexible -= location_growth;
        Self { identity: identity + identity_growth, location: location + location_growth, output: output + output_growth + flexible }
    }
}

/// The progress table's list and its scrollbar: caret gutter, middle-ellipsised identity, place
/// name, status pill, output name.
///
/// `columns` is the same [`ProgressColumns`] the header got, so the rows and their header stay
/// aligned however wide the panel is. The selected row's identity promotes to `TEXT + bold` only
/// while the pane is descended; the tint comes from the `List`'s highlight style, which paints the
/// background alone (contract: only the label promotes). A focused row that carries a place name
/// grows the `└ name` tooltip as a separate item right below it (decision 76) — a separate item,
/// never a second highlighted line, because the highlight style would tint that line too.
pub(crate) fn progress_list(
    frame: &mut Frame, palette: &Palette, rows: &[ProgressRow<'_>], descended: bool, state: &mut ListState, area: Rect,
    columns: ProgressColumns,
) {
    let mut items: Vec<ListItem<'_>> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let selected = descended && state.selected() == Some(index);
            let identity_style = if selected { Style::new().fg(palette.text).bold() } else { Style::new().fg(palette.text_dim) };
            let mut spans = vec![
                caret(palette, selected),
                Span::styled(middle_ellipsis(row.identity, columns.identity), identity_style),
                Span::raw("  "),
            ];
            // The empty name is the padded blank `middle_ellipsis` produces — a leg without the
            // concept (chat) shows an empty cell, never a placeholder.
            spans.push(Span::styled(middle_ellipsis(row.location.unwrap_or(""), columns.location), Style::new().fg(palette.text_dim)));
            spans.push(Span::raw("  "));
            spans.extend(status_pill(palette, row.status));
            spans.push(Span::raw("  "));
            // Middle-ellipsis: both ends of an output name carry meaning — the date prefix is the
            // metadata this app restores and the extension says what the file is — so the cut takes
            // the middle and both survive.
            spans.push(Span::styled(middle_ellipsis(row.output, columns.output), Style::new().fg(palette.text_dim)));
            ListItem::new(Line::from(spans))
        })
        .collect();
    // The tooltip is an item of its own, inserted right below the focused row, so the selection
    // index stays the real row's and the highlight never lands on the tooltip. The name is shown
    // whole — this is the one place the place name is never truncated.
    if descended
        && let Some(index) = state.selected()
        && let Some(name) = rows.get(index).and_then(|row| row.location)
    {
        items.insert(index + 1, ListItem::new(tooltip(palette, name)));
    }
    let item_count = items.len();
    let list = List::new(items).highlight_style(Style::new().bg(palette.bg_hover)).scroll_padding(3);
    frame.render_stateful_widget(&list, area, state);

    let viewport = usize::from(area.height);
    // The list area always spans the panel interior's full width, so its right edge is the panel's
    // right padding column — the scrollbar's home.
    list_scrollbar(frame, palette, item_count, state.offset(), viewport, area.right(), area);
}

/// The scrollbar a scrollable list grows in its panel's right padding column, so the content never
/// reflows when it appears (contract: Scrollbar). Shared by the progress table, the history
/// picker's list and the account screen's two lists — one spelling of the thumb/track pattern.
pub(crate) fn list_scrollbar(frame: &mut Frame, palette: &Palette, rows: usize, offset: usize, viewport: usize, column: u16, area: Rect) {
    // A one-row bar holds no signal: the thumb fills it at every offset, so it never moves.
    // The floor is ratatui's own degenerate guard kept in effect — with the begin/end caps set
    // the widget bails when the track minus the caps is empty, which a one-row area made true
    // incidentally; without the caps the guard has to be stated.
    if rows > viewport && viewport > 0 && area.height > 1 {
        let thumb = glyph::SCROLLBAR_THUMB.to_string();
        let track = glyph::SCROLLBAR_TRACK.to_string();
        // No begin/end symbols: ratatui's defaults are `▲`/`▼` arrow caps the contract has no
        // place for, and pointing the caps at the track glyph would spend two cells of the
        // track on chrome the thumb can then never reach — so the caps are removed outright
        // and the thumb owns the whole track.
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some(&track))
            .thumb_symbol(&thumb)
            .style(palette.bar_track())
            .thumb_style(Style::new().fg(palette.text_dim));
        // ratatui's `Scrollbar` models `position` as the first visible item and bottoms the
        // thumb out at `position == content_length - 1` (its `part_lengths` clamps the position
        // to that ceiling and scales both the thumb start and the thumb length by
        // `content_length - 1 + viewport`), while a list's offset tops out at
        // `content_length - viewport`. Stretch the offset onto the widget's position range so
        // the thumb reaches the track's end exactly at maximum scroll; an offset past the
        // range — a smaller viewport's offset persisting after a resize — clamps to the
        // bottom rather than overshooting it.
        let max_offset = rows - viewport;
        let position = offset.min(max_offset) * (rows - 1) / max_offset;
        let mut state = ScrollbarState::new(rows).position(position).viewport_content_length(viewport);
        frame.render_stateful_widget(scrollbar, Rect::new(column, area.y, 1, area.height), &mut state);
    }
}

/// The progress table's column header (contract: List / table — UPPERCASE TRACKED, the underline
/// rule dropped as the sanctioned variant). The 2-cell lead is the caret gutter the rows carry.
///
/// `columns` is the same [`ProgressColumns`] the rows got, so the header labels land on the same
/// cells the rows' columns do.
pub(crate) fn progress_header(palette: &Palette, columns: ProgressColumns) -> Line<'static> {
    let header = Style::new().fg(palette.text_dim);
    Line::from(vec![
        Span::raw("  "),
        Span::styled(right_pad("IDENTITY", columns.identity), header),
        Span::raw("  "),
        Span::styled(right_pad("LOCATION", columns.location), header),
        Span::raw("  "),
        Span::styled(right_pad("STATUS", STATUS_CELLS), header),
        Span::raw("  "),
        Span::styled("OUTPUT", header),
    ])
}

/// The overall determinate bar (contract: Progress bar, progress role): `ACCENT` fill, bare glyph
/// run, percentage label, sized to `width`.
///
/// "Integer when whole; one decimal otherwise" — 1 of 3 renders 33.3%, never 33% — and `—` for no
/// value at all, since the ellipsis is the indeterminate tell and belongs to the plan phase's
/// spinner. The label's cells are reserved before the bar run is sized: "33.3%" is 5 cells, and a
/// clipped percent reads as a different number.
pub(crate) fn overall_bar(palette: &Palette, done: usize, total: usize, width: usize) -> Line<'static> {
    let percent = done as f64 * 100.0 / total as f64;
    let label = if total == 0 {
        "—".to_owned()
    } else if percent.fract() == 0.0 {
        format!("{percent:.0}%")
    } else {
        format!("{percent:.1}%")
    };
    let bar_cells = width.saturating_sub(cells(&label) + 1);
    let fill = if total == 0 { 0 } else { (bar_cells as f64 * percent / 100.0).round() as usize };
    let mut bar = bar_run(palette, fill, bar_cells, palette.progress_fill());
    bar.push(Span::raw(" "));
    bar.push(Span::styled(label, Style::new().fg(palette.text_dim)));
    Line::from(bar)
}

/// The indeterminate plan phase: an inline spinner where the bar will sit, so the frame does not
/// jump when the plan lands.
pub(crate) fn planning_spinner(palette: &Palette, tick: usize) -> Line<'static> {
    let frame_char = glyph::SPINNER_FRAMES[tick % glyph::SPINNER_FRAMES.len()];
    Line::from(vec![
        Span::styled(frame_char.to_string(), Style::new().fg(palette.accent)),
        Span::styled(" planning", Style::new().fg(palette.text_dim)),
    ])
}

/// The `└ reason` tooltip a disabled row grows while it holds focus (contract: Tooltip). The leader
/// carries `LINE`, the reason `TEXT_FAINT`.
pub(crate) fn tooltip(palette: &Palette, reason: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("  ", Style::new().fg(palette.line)),
        Span::styled(format!("{} ", line::BOTTOM_LEFT), Style::new().fg(palette.line)),
        Span::styled(reason.to_owned(), Style::new().fg(palette.text_faint)),
    ])
}

// ---- action menu + help modal ----

/// The contract's reserved universal letters, never assignable to a row action (`a` actions, `x`
/// dismiss, `?` help, `q` back/quit).
const RESERVED_KEYS: [char; 4] = ['a', 'x', '?', 'q'];

/// Assigns single-letter hotkeys per the contract's algorithm (cloudy-tui: Action menu → Hotkey
/// assignment algorithm). For each action in source order, scan the first three alphabetic
/// characters of its display name, lowercased; the first that is neither claimed by an earlier
/// action nor one of [`RESERVED_KEYS`] becomes its hotkey. All three claimed → no hotkey
/// (arrow-select only). Pinned by `the_action_menu_hotkeys_follow_the_reserved_letter_algorithm`.
#[must_use]
pub(crate) fn assign_hotkeys(labels: &[&str]) -> Vec<Option<char>> {
    let mut claimed = std::collections::HashSet::new();
    labels
        .iter()
        .map(|label| {
            for ch in label.chars().filter(|c| c.is_ascii_alphabetic()).take(3).map(|c| c.to_ascii_lowercase()) {
                if !RESERVED_KEYS.contains(&ch) && !claimed.contains(&ch) {
                    claimed.insert(ch);
                    return Some(ch);
                }
            }
            None
        })
        .collect()
}

/// One help-modal section: a UPPERCASE TRACKED header and its `(key, action)` rows (cloudy-tui:
/// Help modal). The caller assembles the `GLOBAL` section and the per-screen section; the render
/// owns the styling.
#[derive(Debug, Clone)]
pub struct HelpSection<'a> {
    pub title: &'a str,
    pub rows: Vec<(&'a str, &'a str)>,
}

/// The action menu (cloudy-tui: Action menu): a pickable list of the context's actions, their
/// algorithm-assigned hotkeys right-aligned on a `TEXT_DIM` rail. The selected row takes the
/// standard caret + `BG_HOVER` + `TEXT + bold` treatment; selection wraps in the caller's key
/// handler. `labels` and `hotkeys` are the caller's captured state, `selected` the caret index.
pub(crate) fn render_action_menu(
    frame: &mut Frame, palette: &Palette, labels: &[&str], hotkeys: &[Option<char>], selected: usize, area: Rect,
) {
    if labels.is_empty() {
        return;
    }
    let max_label = labels.iter().map(|label| cells(label)).max().unwrap_or(0);
    let content_width = u16::try_from(CARET_GUTTER + max_label + 3 + 1).unwrap_or(u16::MAX);
    let content_height = u16::try_from(labels.len()).unwrap_or(u16::MAX);
    let inner = modal_shell(frame, palette, "actions", content_width, content_height, area);

    let hotkey_style = Style::new().fg(palette.text_dim);
    let rows: Vec<Line<'static>> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let is_selected = index == selected;
            let mut spans = vec![caret(palette, is_selected)];
            spans.push(Span::styled(
                (*label).to_owned(),
                if is_selected { Style::new().fg(palette.text).bold() } else { Style::new().fg(palette.text) },
            ));
            spans.push(Span::raw(" ".repeat(max_label - cells(label) + 3)));
            spans.push(Span::styled(hotkeys.get(index).copied().flatten().map_or_else(|| " ".to_owned(), |c| c.to_string()), hotkey_style));
            let line = Line::from(spans);
            if is_selected { tint_to_edge(line.style(Style::new().bg(palette.bg_hover)), usize::from(inner.width), palette) } else { line }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

/// The help modal (cloudy-tui: Help modal): a sectioned keymap reference. Section headers are
/// `TEXT_DIM` UPPERCASE TRACKED, each row's hotkey leads in `ACCENT + bold` with its action in
/// `TEXT` — the "read, not pick" shape that keeps this distinct from the action menu.
pub(crate) fn render_help_modal(frame: &mut Frame, palette: &Palette, sections: &[HelpSection<'_>], area: Rect) {
    let header_style = Style::new().fg(palette.text_dim);
    let key_style = Style::new().fg(palette.accent).bold();
    let action_style = Style::new().fg(palette.text);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut max_width = 0;
    for (index, section) in sections.iter().enumerate() {
        let title = section.title.to_uppercase();
        lines.push(Line::from(Span::styled(title.clone(), header_style)));
        max_width = max_width.max(cells(&title));
        for (key, action) in &section.rows {
            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled((*key).to_owned(), key_style),
                Span::raw("  "),
                Span::styled((*action).to_owned(), action_style),
            ]);
            max_width = max_width.max(line.width());
            lines.push(line);
        }
        if index + 1 < sections.len() {
            lines.push(Line::default());
        }
    }
    if lines.is_empty() {
        return;
    }
    let content_width = u16::try_from(max_width).unwrap_or(u16::MAX);
    let content_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let inner = modal_shell(frame, palette, "keys", content_width, content_height, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The shared modal shell (cloudy-tui: Modals): a rounded `ACCENT_2` border, an italic-only
/// UPPERCASE title in `TEXT_DIM`, and the base `BG` interior — no backdrop, the screen behind is
/// left untouched except the modal's own rect, which is cleared before the box draws. Sized to
/// `content_width`/`content_height` plus the `Padding::new(2, 2, 1, 1)` and the border, capped at
/// 60% of the terminal width, both dimensions clamped to `area` so a short terminal never gets a
/// write past its edge.
///
/// Content taller than the clamped area clips at the bottom. Both menus here are a handful of rows
/// at most, so scroll is a stated ceiling rather than built — the upgrade path is a `List` +
/// `ListState` once a menu outgrows a screen.
fn modal_shell(frame: &mut Frame, palette: &Palette, title: &str, content_width: u16, content_height: u16, area: Rect) -> Rect {
    let cap = u16::try_from(u32::from(area.width) * 3 / 5).unwrap_or(u16::MAX);
    let width = content_width.saturating_add(6).min(cap).min(area.width);
    let height = content_height.saturating_add(4).min(area.height);
    let rect = area.centered(Constraint::Length(width), Constraint::Length(height));
    frame.render_widget(Clear, rect);
    let border = Style::new().fg(palette.accent_2);
    let title = Line::from(vec![
        Span::styled(line::HORIZONTAL, border),
        Span::styled(format!(" {} ", title.to_uppercase()), Style::new().fg(palette.text_dim).italic()),
    ]);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(border).padding(Padding::new(2, 2, 1, 1)).title(title);
    // The base `BG` interior (cloudy-tui: Modals — interior fill, same as the screen). `Clear`
    // above resets the rect to the terminal default, so without this the full tier leaves a
    // darker hole behind the ACCENT_2 border; the fill is conditional because `surface()` answers
    // `None` on the compatible tier, where the terminal's own background is the interior.
    let block = if let Some(surface) = palette.surface() { block.style(Style::new().bg(surface)) } else { block };
    frame.render_widget(block.clone(), rect);
    block.inner(rect)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};

    use super::*;
    use crate::tui::theme::Tier;

    /// The focus promotion holds on both tiers, against per-tier colour literals.
    ///
    /// Nothing in [`form_label`] branches on the tier — `Palette` keeps its own `tier` field private
    /// so that it cannot — which makes the two tiers agreeing the expected result rather than a
    /// discovery, exactly as the overview's tier work found. The value is the pin, not the finding: a
    /// palette that flattened `TEXT` into `TEXT_DIM` on one column, or a promotion that stopped
    /// moving between the two roles, reds here on one of the two passes. Written as literals for
    /// that reason — `Palette::new(tier).text` would agree with itself whatever the palette
    /// resolved to, and could not tell a flattening from a match.
    ///
    /// **This is the one place the tier axis sits on the form-row grammar.** Both screens reach this
    /// span through this call, so a per-screen copy would pin the same bytes twice and drift apart
    /// on whichever one nobody edited.
    #[test]
    fn the_focus_promoted_form_label_holds_both_tiers() {
        for (tier, text, dim) in [
            (Tier::Full, Color::Rgb(205, 214, 244), Color::Rgb(166, 173, 200)),
            (Tier::Compatible, Color::Indexed(189), Color::Indexed(145)),
        ] {
            let palette = Palette::new(tier);

            let focused = form_label(&palette, "transcode", true);
            assert_eq!(focused.style.fg, Some(text), "{tier:?}: the focused label promotes to TEXT");
            assert!(focused.style.add_modifier.contains(Modifier::BOLD), "{tier:?}: bold is the current-row cue");

            let blurred = form_label(&palette, "transcode", false);
            assert_eq!(blurred.style.fg, Some(dim), "{tier:?}: a blurred interactive label stays TEXT_DIM");
            assert!(!blurred.style.add_modifier.contains(Modifier::BOLD), "{tier:?}: bold here would read as a static key's anchor");
        }
    }

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

    #[test]
    fn a_missing_disk_probe_renders_unknown_in_warning() {
        let environment = Environment { ffmpeg: None, vlc: None, available_space: None, total_space: None };
        let value = disk_free_value(&Palette::new(Tier::Full), &environment, 23);
        assert_eq!(value.len(), 1);
        assert_eq!(value[0].content.as_ref(), "unknown");
    }

    #[test]
    fn the_disk_bar_shows_the_used_share_of_the_disk() {
        // 3 of 5 GiB free is 40% used: the usage bar fills 3 of its 9 cells.
        let environment = Environment {
            ffmpeg: None,
            vlc: None,
            available_space: Some(3 * 1024 * 1024 * 1024),
            total_space: Some(5 * 1024 * 1024 * 1024),
        };
        let value = disk_free_value(&Palette::new(Tier::Full), &environment, 23);
        let text: String = value.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, "3.0 GiB ███░░░░░░ 40%");
    }

    /// `Environment`'s fields are public, so a free figure larger than the total is a value a caller
    /// can construct — and the naive `100 - (free/total*100) as u8` saturates the cast at 255 and
    /// then underflows, which panics in debug and wraps in release.
    ///
    /// The second half is the one worth keeping: the fix clamps the FREE share BEFORE subtracting,
    /// because clamping the used share afterwards closes the same hole and silently moves a
    /// percentage on ordinary input. Rust rounds half away from zero, so a free share of exactly
    /// 60.5% must report 39% used, not 40% — the boundary case that tells the two orderings apart.
    #[test]
    fn the_disk_bar_survives_a_free_figure_larger_than_the_total() {
        let palette = Palette::new(Tier::Full);
        let impossible = Environment { ffmpeg: None, vlc: None, available_space: Some(9_000), total_space: Some(1_000) };
        let value = disk_free_value(&palette, &impossible, 23);
        let text: String = value.iter().map(|span| span.content.as_ref()).collect();
        assert!(text.ends_with("0%"), "more free than total reads as nothing used rather than panicking: {text}");

        // The ordering witness, and it only witnesses anything at an EXACTLY representable `.5`
        // share. `100 - round(x)` and `round(100 - x)` agree everywhere else, so a share of
        // 60.498% or 60.547% — the first pair tried here — passes under both orderings and pins
        // nothing. Eighths of a power of two are exact in binary: 5/8 is 62.5%, which rounds away
        // from zero to 63 and reports 37% used, where subtracting first gives 37.5 and reports 38%.
        let share = |free: u64, total: u64| {
            let environment = Environment { ffmpeg: None, vlc: None, available_space: Some(free), total_space: Some(total) };
            let text: String = disk_free_value(&palette, &environment, 23).iter().map(|span| span.content.as_ref()).collect();
            text.rsplit(' ').next().unwrap().to_owned()
        };
        assert_eq!(share(5, 8), "37%", "the free share rounds up before the subtraction, so used is 37 and not 38");
        assert_eq!(share(3, 8), "62%", "and the same in the other direction: 62, not 63");
    }

    #[test]
    fn every_status_pill_occupies_exactly_the_status_column() {
        // STATUS_CELLS is the widest pill, `[ pending ]`; every other pill pads out to it, so the
        // output column never shifts between statuses — on either screen that draws one.
        let palette = Palette::new(Tier::Full);
        for status in ItemStatus::ALL {
            let width: usize = status_pill(&palette, status).iter().map(Span::width).sum();
            assert_eq!(width, STATUS_CELLS, "{status:?}");
        }
    }

    #[test]
    fn the_cycle_grammar_is_one_spelling_for_both_forms() {
        // The shared control's own pin: brackets only while the row is focused (the focus cue,
        // +2 cells), the selected word in ACCENT, the rest TEXT_FAINT. Each form's interior
        // budget re-derives the width from its own roster, so this only pins the grammar.
        let palette = Palette::new(Tier::Full);
        let words = ["merged", "both", "originals"];
        let blurred: usize = cycle_options(&palette, &words, 1, false).iter().map(Span::width).sum();
        let focused: usize = cycle_options(&palette, &words, 1, true).iter().map(Span::width).sum();
        assert_eq!(focused, blurred + 2, "the focused bracket pair is the focus signal");
        let spans = cycle_options(&palette, &words, 1, true);
        assert_eq!(spans[0].content.as_ref(), "merged");
        assert_eq!(spans[0].style.fg, Some(palette.text_faint));
        assert_eq!(spans[2].content.as_ref(), "[both]");
        assert_eq!(spans[2].style.fg, Some(palette.accent));
    }

    #[test]
    fn the_overall_bar_is_integer_when_whole_and_one_decimal_otherwise() {
        // The contract's own rule, and `—` for no value at all: the ellipsis is the indeterminate
        // tell and belongs to the plan phase's spinner.
        let palette = Palette::new(Tier::Full);
        let label = |done, total| {
            let line = overall_bar(&palette, done, total, 40);
            line.spans.last().unwrap().content.as_ref().to_owned()
        };
        assert_eq!(label(0, 3), "0%");
        assert_eq!(label(1, 3), "33.3%");
        assert_eq!(label(3, 3), "100%");
        assert_eq!(label(0, 0), "—");
    }

    #[test]
    fn the_path_budget_uses_the_remaining_width_and_keeps_the_floor() {
        // The side-by-side form's widest row (`output dir`) leaves exactly the caller's floor;
        // a shorter label gives its value the extra cells, and the floor never shrinks below the
        // caller's constant even past the panel edge.
        assert_eq!(path_budget(36, "output dir", 22), 22);
        assert_eq!(path_budget(36, "source", 22), 26);
        assert_eq!(path_budget(76, "source", 22), 66);
        assert_eq!(path_budget(10, "source", 22), 22);
    }

    #[test]
    fn the_progress_columns_grow_output_first_then_identity_and_location() {
        // At the narrow floor all three flexible columns sit at their minimums; surplus then grows
        // the output column to its full filename first, since its date prefix is the deliverable.
        // Identity and location share what remains and middle-ellipsize. The floor width is the
        // fixed chrome plus the three floors.
        let floor_width = CARET_GUTTER + 3 * COLUMN_GAP + STATUS_CELLS + IDENTITY_CELLS + LOCATION_CELLS + OUTPUT_MIN;
        assert_eq!(
            ProgressColumns::for_width(floor_width, 36, 42, 19),
            ProgressColumns { identity: IDENTITY_CELLS, location: LOCATION_CELLS, output: OUTPUT_MIN }
        );
        // +13 of surplus reaches the output column first: the full 19-cell name renders while
        // identity and location stay at their floors.
        assert_eq!(
            ProgressColumns::for_width(floor_width + 13, 36, 42, 19),
            ProgressColumns { identity: IDENTITY_CELLS, location: LOCATION_CELLS, output: 19 }
        );
        // Once output is whole, identity grows toward its id, then location toward its name.
        assert_eq!(
            ProgressColumns::for_width(floor_width + 31, 36, 42, 19),
            ProgressColumns { identity: 36, location: LOCATION_CELLS, output: 19 }
        );
        assert_eq!(ProgressColumns::for_width(floor_width + 40, 36, 42, 19), ProgressColumns { identity: 36, location: 38, output: 19 });
        // A view with no place name (chat) keeps location at its floor and still gives output its
        // full width first.
        assert_eq!(
            ProgressColumns::for_width(floor_width + 13, 20, 0, 19),
            ProgressColumns { identity: IDENTITY_CELLS, location: LOCATION_CELLS, output: 19 }
        );
    }

    #[test]
    fn the_action_chip_drops_its_raised_fill_on_the_compatible_tier() {
        // DNA rule 3: the compatible tier paints no surface fills, so the chip's rest and disabled
        // states carry no background; the full tier keeps the BG_RAISED fill. The focused inverse
        // block (ACCENT fill) is not a surface fill and stays on both tiers.
        let full = Palette::new(Tier::Full);
        let compatible = Palette::new(Tier::Compatible);

        assert_eq!(action_chip(&full, "start", true, false).style.bg, Some(full.bg_raised), "full rest fill");
        assert_eq!(action_chip(&compatible, "start", true, false).style.bg, None, "compatible rest paints none");
        assert_eq!(action_chip(&compatible, "start", false, false).style.bg, None, "compatible disabled paints none");
        assert_eq!(action_chip(&compatible, "start", true, true).style.bg, Some(compatible.accent), "focused inverse block is unchanged");
    }

    #[test]
    fn the_shared_empty_state_frame_sits_centred_in_its_panel() {
        // The shared empty state centers its frame inside the panel interior, so an odd leftover
        // splits 10/11 — the two pads differ by one, whichever side takes the extra. The frame's
        // width is the hint-or-action cells (the action line is 16, the floor) plus the 3-cell
        // inset on each side plus the two borders.
        let palette = Palette::new(Tier::Full);
        let mut terminal = Terminal::new(TestBackend::new(45, 16)).unwrap();
        terminal.draw(|frame| empty_state(frame, &palette, Rect::new(0, 0, 45, 16), "no deliveries")).unwrap();
        let buffer = terminal.backend().buffer();

        // 16 interior rows, a 4-row frame: 6 above and 6 below.
        let framed: String = (0..buffer.area.width).map(|x| buffer[(x, 6)].symbol()).collect();
        let left_pad = framed.chars().take_while(|c| *c == ' ').count();
        let right_pad = framed.len() - framed.trim_end().len();

        assert_eq!(framed.trim().chars().count(), 24, "frame width");
        assert!(left_pad.abs_diff(right_pad) <= 1, "pads {left_pad} and {right_pad} are not a centred split");
    }

    /// The first cell whose symbol equals `needle`, scanning row-major. Modal tests locate the
    /// caret and hotkeys by content rather than re-deriving the shell's centering math, which
    /// would only agree with itself.
    fn find_cell(buffer: &Buffer, needle: char) -> Option<(u16, u16)> {
        (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .find(|&(x, y)| buffer[(x, y)].symbol() == needle.to_string())
    }

    /// Whether any row carries `needle` as a contiguous run of cells.
    fn contains(buffer: &Buffer, needle: &str) -> bool {
        (0..buffer.area.height).any(|y| {
            let row: String = (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect();
            row.contains(needle)
        })
    }

    #[test]
    fn the_action_menu_hotkeys_follow_the_reserved_letter_algorithm() {
        // The contract's worked example, verbatim: the collision pushes each later action to its
        // next free letter, never overriding an earlier assignment.
        assert_eq!(
            assign_hotkeys(&["delete", "duplicate", "detach", "diff", "diff (renamed)"]),
            [Some('d'), Some('u'), Some('e'), Some('i'), Some('f')]
        );
    }

    #[test]
    fn a_reserved_letter_is_skipped_even_when_it_is_the_first_char() {
        // `a` and `q` are reserved, so an action whose name leads with one takes its second char.
        assert_eq!(assign_hotkeys(&["apply", "quiet"]), [Some('p'), Some('u')]);
    }

    #[test]
    fn an_action_whose_first_three_letters_are_all_claimed_gets_no_hotkey() {
        // `delete` claims d, `diff` claims i, `diff (renamed)` claims f — a fourth `diff`-worded
        // action has all three of d, i and f claimed and is arrow-select only.
        assert_eq!(assign_hotkeys(&["delete", "diff", "diff (renamed)", "diff again"]), [Some('d'), Some('i'), Some('f'), None]);
    }

    #[test]
    fn the_action_menu_renders_the_selected_caret_and_right_aligned_hotkeys() {
        let palette = Palette::new(Tier::Full);
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| render_action_menu(frame, &palette, &["delete", "duplicate"], &[Some('d'), Some('u')], 0, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();

        // The title, both labels and both hotkeys render; the modal border is the warm anchor.
        assert!(contains(buffer, "ACTIONS"), "the title rides the border break, uppercased");
        assert!(contains(buffer, "delete") && contains(buffer, "duplicate"), "both labels render");
        let (corner_x, corner_y) = find_cell(buffer, '╭').expect("the rounded modal border draws");
        assert_eq!(buffer[(corner_x, corner_y)].style().fg, Some(palette.accent_2), "the border is ACCENT_2");

        // The selected first row carries the caret, its label promotes to bold TEXT, and the row
        // below stays plain.
        let (caret_x, caret_y) = find_cell(buffer, '❯').expect("the selected row carries a caret");
        assert_eq!(buffer[(caret_x + 2, caret_y)].symbol(), "d", "the label starts past the caret gutter");
        assert_eq!(buffer[(caret_x + 2, caret_y)].style().fg, Some(palette.text));
        assert!(buffer[(caret_x + 2, caret_y)].style().add_modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(caret_x, caret_y + 1)].symbol(), " ", "the unselected row's gutter is blank");
        assert!(!buffer[(caret_x + 2, caret_y + 1)].style().add_modifier.contains(Modifier::BOLD));

        // The hotkey rail is the rightmost content cell, TEXT_DIM and never bold.
        let hotkey_x =
            (caret_x..buffer.area.width).rev().find(|&x| buffer[(x, caret_y)].symbol() == "d").expect("the row ends in its `d` hotkey");
        assert_eq!(buffer[(hotkey_x, caret_y)].style().fg, Some(palette.text_dim));
        assert!(!buffer[(hotkey_x, caret_y)].style().add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_modal_interior_fills_the_base_surface_on_full_and_not_on_compatible() {
        // The interior is the screen's own `BG`, not the terminal default `Clear` leaves — that
        // hole is the tell on the full tier, and the compatible tier paints no surface fill at
        // all. Pinned at the shell's own interior cell, not by re-deriving the centering math.
        for (tier, expected) in [(Tier::Full, Color::Rgb(30, 30, 46)), (Tier::Compatible, Color::Reset)] {
            let palette = Palette::new(tier);
            let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
            let inner = std::cell::Cell::new(Rect::new(0, 0, 0, 0));
            terminal.draw(|frame| inner.set(modal_shell(frame, &palette, "actions", 10, 2, frame.area()))).unwrap();
            let buffer = terminal.backend().buffer();
            let inner = inner.get();
            assert_eq!(buffer[(inner.x, inner.y)].style().bg, Some(expected), "{tier:?}: the modal interior must carry the base surface");
        }
    }

    #[test]
    fn the_help_modal_renders_uppercased_sections_with_accent_hotkeys() {
        let palette = Palette::new(Tier::Full);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let sections = [
            HelpSection { title: "global", rows: vec![("q", "back / quit"), ("?", "help")] },
            HelpSection { title: "memories", rows: vec![("↑ ↓", "move")] },
        ];
        terminal.draw(|frame| render_help_modal(frame, &palette, &sections, frame.area())).unwrap();
        let buffer = terminal.backend().buffer();

        assert!(contains(buffer, "KEYS"), "the modal title");
        assert!(contains(buffer, "GLOBAL"), "the section header is uppercased");
        assert!(contains(buffer, "MEMORIES"), "the screen section is uppercased");

        // The q hotkey leads its row in ACCENT + bold, its action trails in plain TEXT.
        let (q_x, q_y) = find_cell(buffer, 'q').expect("the q hotkey renders");
        assert_eq!(buffer[(q_x, q_y)].style().fg, Some(palette.accent));
        assert!(buffer[(q_x, q_y)].style().add_modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(q_x + 3, q_y)].symbol(), "b", "the action trails the hotkey by two cells");
        assert_eq!(buffer[(q_x + 3, q_y)].style().fg, Some(palette.text));
        assert!(!buffer[(q_x + 3, q_y)].style().add_modifier.contains(Modifier::BOLD));
    }
}
