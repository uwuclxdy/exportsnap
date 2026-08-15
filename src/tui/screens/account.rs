//! The account tab: a read-only master-detail over the export's account metadata (decision 65).
//!
//! The master pane lists the five sections — account, friends, location, stories,
//! subscriptions — and the detail pane shows the selected section's counts and metadata. Focus
//! descends with `enter`, the master-detail grammar the history screen uses: `←` and `esc`
//! ascend, and `→` is inert while descended. On the section list `→` is the shell's tab key and
//! is deliberately not consumed, so the arrow walk crosses this screen like the others
//! (`tests/app.rs`'s `right_arrow_walks_forward_through_every_tab` pins that).
//!
//! # Aggregates only, and no error surface
//!
//! **No coordinate, no other identity, and no message body reaches this screen.** The location
//! section renders counts alone — never a place name, an address, a business id, or a
//! coordinate pair — and the account section renders no IP (`registration_ip` is deliberately
//! not a row). The load path discards every error that could carry such a byte: each of the
//! five files is read independently and `.ok().flatten()`ed, so a file that is missing or
//! broken lands as an absent section whose rows all render `—`. The screen never reads the
//! files that hold message bodies, so that half of the rule holds by construction; the planted
//! byte classes are pinned absent by `tests/account_screen.rs`.
//!
//! The overview's numbers are reused, not re-aggregated: the friends section's count is the
//! same `friends.friends.len()` the summary panel reports, and the stories section reads the
//! same typed `schema::StoryHistory` passthrough `ExportJson` holds.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use crate::export::local_fix::calendar;
use crate::export::model::{self, Timestamp};
use crate::export::schema;
use crate::export::zip::discover_parts;
use crate::export::{read_model, read_schema};
use crate::tui::format::{cells, grouped, middle_ellipsis, truncate_prose};
use crate::tui::screens::overview::GUARANTEED_INTERIOR_ROWS;
use crate::tui::theme::Palette;
use crate::tui::widgets::{self, CARET_GUTTER, LABEL_GAP, PanelStyle, caret, form_label, list_scrollbar, panel, static_row};

// ---- layout budgets ----

/// Cells the timestamp value's widest shape occupies: the absolute ISO date the age rule
/// renders at 30 days and past — the relative forms are all shorter. The one value that must
/// never clip: a date cut mid-way misreads, where a truncated username keeps both ends. Pinned
/// against the age helper's ISO form by a test.
const TIMESTAMP_CELLS: usize = 10;

/// The master-detail contract's selector clamp floor (cloudy-tui Master-detail): the master
/// pane is at least 20 cells wide, whatever its content needs.
const SELECTOR_CLAMP_FLOOR: u16 = 20;

/// The master pane's interior cells: the caret gutter plus the longest section name, never
/// below the clamp floor's own interior (the floor's 20 cells minus the panel chrome).
fn sections_interior() -> usize {
    let need = CARET_GUTTER + Section::ALL.iter().map(|section| cells(section.label())).max().unwrap_or(0);
    need.max(usize::from(SELECTOR_CLAMP_FLOOR) - usize::from(widgets::CHROME_COLUMNS))
}

/// The master pane's width: its interior need plus the panel chrome.
fn sections_panel_width() -> u16 {
    u16::try_from(sections_interior() + usize::from(widgets::CHROME_COLUMNS)).unwrap_or(u16::MAX)
}

/// Interior cells the detail pane's widest row needs, caret and gap included. Counts are short
/// and text truncates at render, so the only value with a real floor is the timestamp's
/// widest shape.
fn detail_interior() -> usize {
    CARET_GUTTER
        + LABEL_GAP
        + Section::ALL
            .iter()
            .flat_map(|section| section_rows(*section))
            .map(|spec| cells(spec.label) + usize::from(spec.kind == ValueKind::Timestamp) * TIMESTAMP_CELLS)
            .max()
            .unwrap_or(0)
}

/// The detail pane's floor width: its interior need plus the panel chrome.
fn detail_panel_min_width() -> u16 {
    u16::try_from(detail_interior() + usize::from(widgets::CHROME_COLUMNS)).unwrap_or(u16::MAX)
}

/// Body rows the stacked arm needs to show both panels whole: the section list and the tallest
/// detail, borders included. Below that the screen is the section list alone.
fn stacked_height() -> u16 {
    let tallest = Section::ALL.iter().map(|section| section_rows(*section).len()).max().unwrap_or(0);
    u16::try_from(tallest + Section::ALL.len() + 2 * usize::from(widgets::BORDER_ROWS)).unwrap_or(u16::MAX)
}

/// The master pane's height: the five section rows plus the panel borders.
fn sections_panel_height() -> u16 {
    u16::try_from(Section::ALL.len() + usize::from(widgets::BORDER_ROWS)).unwrap_or(u16::MAX)
}

// ---- sections ----

/// The five sections, in caret order (decision 65).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Account,
    Friends,
    Location,
    Stories,
    Subscriptions,
}

impl Section {
    const ALL: [Self; 5] = [Self::Account, Self::Friends, Self::Location, Self::Stories, Self::Subscriptions];

    /// The master row's label and the detail pane's title.
    const fn label(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Friends => "friends",
            Self::Location => "location",
            Self::Stories => "stories",
            Self::Subscriptions => "subscriptions",
        }
    }
}

/// The kinds a detail value can take: what decides how it truncates and whether it carries a
/// width floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    /// A grouped count.
    Count,
    /// A timestamp's fixed shape, the detail's only value with a width floor.
    Timestamp,
    /// Free-form text, prose-cut at render.
    Text,
    /// An identity, middle-cut at render so both ends survive.
    Identity,
}

/// One detail row: its label and value kind. The static single source for the detail's labels
/// and its width floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowSpec {
    label: &'static str,
    kind: ValueKind,
}

/// The account section's rows. `registration_ip` is deliberately not rendered: it is neither a
/// stat nor identity this screen needs to show.
const ACCOUNT_ROWS: [RowSpec; 7] = [
    RowSpec { label: "username", kind: ValueKind::Identity },
    RowSpec { label: "name", kind: ValueKind::Text },
    RowSpec { label: "created", kind: ValueKind::Timestamp },
    RowSpec { label: "country", kind: ValueKind::Text },
    RowSpec { label: "last active", kind: ValueKind::Text },
    RowSpec { label: "devices", kind: ValueKind::Count },
    RowSpec { label: "logins", kind: ValueKind::Count },
];

/// The friends section's rows: every list `friends.json` holds, accepted friends first (the
/// count the overview's summary panel also reports).
const FRIENDS_ROWS: [RowSpec; 8] = [
    RowSpec { label: "friends", kind: ValueKind::Count },
    RowSpec { label: "requests sent", kind: ValueKind::Count },
    RowSpec { label: "blocked", kind: ValueKind::Count },
    RowSpec { label: "deleted", kind: ValueKind::Count },
    RowSpec { label: "hidden suggestions", kind: ValueKind::Count },
    RowSpec { label: "ignored", kind: ValueKind::Count },
    RowSpec { label: "pending requests", kind: ValueKind::Count },
    RowSpec { label: "shortcuts", kind: ValueKind::Count },
];

/// The location section's rows: counts only, never a place name or a coordinate (the privacy
/// rule).
const LOCATION_ROWS: [RowSpec; 9] = [
    RowSpec { label: "frequent locations", kind: ValueKind::Count },
    RowSpec { label: "latest location", kind: ValueKind::Count },
    RowSpec { label: "home, school & work", kind: ValueKind::Count },
    RowSpec { label: "daily top locations", kind: ValueKind::Count },
    RowSpec { label: "six-day periods", kind: ValueKind::Count },
    RowSpec { label: "location history", kind: ValueKind::Count },
    RowSpec { label: "businesses visited", kind: ValueKind::Count },
    RowSpec { label: "actiomoji info", kind: ValueKind::Count },
    RowSpec { label: "areas visited", kind: ValueKind::Count },
];

/// The stories section's rows: the user's own story counts plus the friend-and-public entries
/// count.
const STORIES_ROWS: [RowSpec; 4] = [
    RowSpec { label: "posts", kind: ValueKind::Count },
    RowSpec { label: "views", kind: ValueKind::Count },
    RowSpec { label: "replies", kind: ValueKind::Count },
    RowSpec { label: "friend & public stories", kind: ValueKind::Count },
];

/// The subscriptions section's single row. The one observed export holds an empty list, so the
/// element shape is unobserved; the count is the whole typed model (`docs/design.md`).
const SUBSCRIPTIONS_ROWS: [RowSpec; 1] = [RowSpec { label: "subscriptions", kind: ValueKind::Count }];

/// The rows a section's detail shows, in display order.
fn section_rows(section: Section) -> &'static [RowSpec] {
    match section {
        Section::Account => &ACCOUNT_ROWS,
        Section::Friends => &FRIENDS_ROWS,
        Section::Location => &LOCATION_ROWS,
        Section::Stories => &STORIES_ROWS,
        Section::Subscriptions => &SUBSCRIPTIONS_ROWS,
    }
}

// ---- state ----

/// The account tab's state.
#[derive(Debug)]
pub struct Account {
    source: PathBuf,
    /// Each section's file, read independently: a missing or broken file is `None` and the
    /// section's rows all render `—`. The read's error text never reaches the screen.
    account: Option<model::Account>,
    friends: Option<model::Friends>,
    location: Option<schema::LocationHistory>,
    stories: Option<schema::StoryHistory>,
    subscriptions: Option<model::UserProfile>,
    section_list: ListState,
    detail_list: ListState,
    /// Whether the detail pane owns the caret.
    descended: bool,
    /// Whether the detail pane renders this frame — `area` at or above the stacked floor, as
    /// `render` last saw it. Render-derived state the key handlers read back: enter cannot
    /// descend into a pane that is not drawn (reviewer #3).
    detail_pane_visible: bool,
}

impl Account {
    /// The state against a source dir: each of the five sections' files read eagerly and
    /// independently. A source with no unpacked delivery reads nothing — every section absent.
    #[must_use]
    pub fn with_environment(source: PathBuf) -> Self {
        // The json dir is discovered exactly like the overview's: exactly one delivery in the
        // source dir, its first unpacked part's `json/`. No delivery, an unreadable dir, or
        // several deliveries means no read at all — the overview's rule, so two screens cannot
        // disagree about which export they are reading.
        let json_dir = discover_parts(&source).ok().and_then(|groups| match groups.as_slice() {
            [group] => group.extracted.iter().find_map(|part| part.json_dir.clone()),
            _ => None,
        });
        let (account, friends, location, stories, subscriptions) = match json_dir.as_deref() {
            // `.ok().flatten()`: a read error — the file unreadable, not json, or invalid —
            // is discarded whole, so a broken file lands as an absent section and its error
            // text, which could carry an offending value's bytes, never reaches the screen
            // (the privacy rule).
            Some(dir) => (
                read_model::<schema::Account, _>(dir, "account.json").ok().flatten(),
                read_model::<schema::Friends, _>(dir, "friends.json").ok().flatten(),
                read_schema::<schema::LocationHistory>(dir, "location_history.json").ok().flatten(),
                read_schema::<schema::StoryHistory>(dir, "story_history.json").ok().flatten(),
                read_model::<schema::UserProfile, _>(dir, "user_profile.json").ok().flatten(),
            ),
            None => (None, None, None, None, None),
        };
        let mut section_list = ListState::default();
        section_list.select(Some(0));
        // The detail opens on its first row too: the caret starts on the section list, and the
        // first descend must find a row selected.
        let mut detail_list = ListState::default();
        detail_list.select(Some(0));
        Self {
            source,
            account,
            friends,
            location,
            stories,
            subscriptions,
            section_list,
            detail_list,
            descended: false,
            // The side-by-side assumption, corrected by the first render.
            detail_pane_visible: true,
        }
    }

    /// The dir this screen reads — what [`crate::app::App::source_report`] reports for it.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Whether the detail pane owns the caret.
    #[must_use]
    pub const fn descended(&self) -> bool {
        self.descended
    }

    /// Returns the caret to the section list. Called by `esc` and `←` from inside the screen,
    /// and by the app for `q` and the `⌥<digit>` jumps, which ascend implicitly.
    pub fn ascend(&mut self) {
        self.descended = false;
    }

    /// Handles one key while the account tab is active. `true` when the screen consumed it.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // The detail pane's existence is render-derived: a resize below the floor is delivered
        // as an event, and a key can land before the next draw normalizes `descended`. A stale
        // descent must not walk rows that will not render (reviewer #3).
        if self.descended { self.handle_detail_key(key) } else { self.handle_sections_key(key) }
    }

    /// The section list owns the caret: arrows walk the sections (wrapping) and enter descends
    /// into the detail pane — the master-detail grammar. `→` stays the shell's tab key here,
    /// so it is deliberately NOT consumed.
    fn handle_sections_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                let delta: isize = if key.code == KeyCode::Up { -1 } else { 1 };
                self.move_section(delta);
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                // Descend only where a pane exists to descend into: below the stacked floor
                // the detail rows do not render, so enter cannot drop the caret onto them
                // (reviewer #3).
                if self.detail_pane_visible {
                    self.descended = true;
                }
                true
            }
            _ => false,
        }
    }

    /// Moves the section selection, wrapping at both ends. A new section is a new detail, so
    /// the detail's selection resets to its first row.
    fn move_section(&mut self, delta: isize) {
        let current = self.section_list.selected().unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(Section::ALL.len() as isize) as usize;
        self.section_list.select(Some(next));
        self.detail_list.select(Some(0));
    }

    /// The selected section.
    fn section(&self) -> Section {
        Section::ALL[self.section_list.selected().unwrap_or(0)]
    }

    /// The detail pane owns the caret: arrows walk its rows (wrapping) — the pane's only
    /// interaction, scroll — and `esc` or `←` ascends. `→` is inert; enter too, since a
    /// read-only row has nothing to toggle.
    fn handle_detail_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                let delta: isize = if key.code == KeyCode::Up { -1 } else { 1 };
                let len = section_rows(self.section()).len() as isize;
                let current = self.detail_list.selected().unwrap_or(0) as isize;
                self.detail_list.select(Some((current + delta).rem_euclid(len) as usize));
                true
            }
            KeyCode::Esc | KeyCode::Left if key.modifiers == KeyModifiers::NONE => {
                self.descended = false;
                true
            }
            KeyCode::Right | KeyCode::Enter if key.modifiers == KeyModifiers::NONE => true,
            _ => false,
        }
    }
}

// ---- render ----

/// Draws the screen into `area`: the section list and the selected section's detail.
///
/// The ladder mirrors the overview's: side by side when the body is wide enough for both
/// panels, stacked full-width when the rows fit, and the section list alone below that — the
/// arm that still names the screen's content (the summary-only rule). The breakpoints are
/// derived from the panels' own copy needs, not picked widths.
pub fn render(frame: &mut Frame, palette: &Palette, account: &mut Account, area: Rect) {
    let side_by_side = usize::from(area.width) >= usize::from(sections_panel_width()) + usize::from(detail_panel_min_width());
    let stacked = !side_by_side && usize::from(area.width) >= usize::from(detail_panel_min_width()) && area.height >= stacked_height();

    // The pane's existence is a render-derived fact the handlers read back: below the stacked
    // floor `descended` cannot survive — the detail rows it walks do not render there, and a
    // resize out of the two-panel arms must not leave the caret on rows that are gone
    // (reviewer #3).
    account.detail_pane_visible = side_by_side || stacked;
    if !account.detail_pane_visible {
        account.descended = false;
    }

    if side_by_side {
        let [left, right] = Layout::horizontal([Constraint::Length(sections_panel_width()), Constraint::Fill(1)]).areas(area);
        // Both panes hug their content (decision 79): the section list is its five rows and the
        // detail is the selected section's rows, so neither carries a blank tail.
        render_sections(frame, palette, account, widgets::hug(left, sections_panel_height()));
        render_detail(frame, palette, account, widgets::hug(right, detail_height(account.section())));
    } else if stacked {
        // Both panes take exactly their rows, so the borders touch with no gap between them
        // (0-cell panel gaps in every mode).
        let [top, bottom] =
            Layout::vertical([Constraint::Length(sections_panel_height()), Constraint::Length(detail_height(account.section()))])
                .areas(area);
        render_sections(frame, palette, account, top);
        render_detail(frame, palette, account, bottom);
    } else {
        render_sections(frame, palette, account, widgets::hug(area, sections_panel_height()));
    }
}

/// The detail pane's height for the selected section: its rows plus the borders. The pane hugs
/// this (decision 79) and scrolls its list once the body offers less.
fn detail_height(section: Section) -> u16 {
    u16::try_from(section_rows(section).len() + usize::from(widgets::BORDER_ROWS)).unwrap_or(u16::MAX)
}

/// Draws the section list into `area`.
fn render_sections(frame: &mut Frame, palette: &Palette, account: &mut Account, area: Rect) {
    let block = panel(palette, "sections", PanelStyle { first: true, focused: !account.descended });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole or not at all across the width, exactly like the other screens' panels: a section
    // name clipped mid-way misreads. Down the height the rows clip one at a time.
    if usize::from(inner.width) < sections_interior() {
        return;
    }

    let items: Vec<ListItem<'_>> = Section::ALL
        .iter()
        .enumerate()
        .map(|(index, section)| {
            // The caret and the label promotion belong to the pane that owns the caret; the
            // tint comes from the List's highlight style, which paints the selected row's
            // background at any focus (contract: blurred panes keep the tint).
            let focused = !account.descended && account.section_list.selected() == Some(index);
            ListItem::new(Line::from(vec![caret(palette, focused), form_label(palette, section.label(), focused)]))
        })
        .collect();
    let list = List::new(items).highlight_style(Style::new().bg(palette.bg_hover)).scroll_padding(3);
    frame.render_stateful_widget(&list, inner, &mut account.section_list);

    let viewport = usize::from(inner.height);
    list_scrollbar(frame, palette, Section::ALL.len(), account.section_list.offset(), viewport, inner.right(), inner);
}

/// Draws the selected section's detail into `area`.
fn render_detail(frame: &mut Frame, palette: &Palette, account: &mut Account, area: Rect) {
    let block = panel(palette, account.section().label(), PanelStyle { first: false, focused: account.descended });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = detail_rows(palette, account, usize::from(inner.width));

    // The walk is clamped to the section's row count; a state that points past the rows (a
    // section switch before the reset lands) renders no selection until the first key heals
    // it — except that nothing unrenderable takes focus (reviewer #3), so the render clamps.
    if account.detail_list.selected().is_some_and(|selected| selected >= rows.len()) && !rows.is_empty() {
        account.detail_list.select(Some(0));
    }

    let items: Vec<ListItem<'_>> = rows.into_iter().map(ListItem::new).collect();
    // The rows tint and caret themselves via `static_row`; the List has no highlight style,
    // which would double-paint the same selection.
    let list = List::new(items).scroll_padding(3);
    frame.render_stateful_widget(&list, inner, &mut account.detail_list);

    let viewport = usize::from(inner.height);
    list_scrollbar(frame, palette, section_rows(account.section()).len(), account.detail_list.offset(), viewport, inner.right(), inner);
}

/// One detail row: the caret, the `TEXT_DIM + bold` key, the value, and the selected row's
/// tint — the static-row grammar of the run screens.
fn detail_row(
    palette: &Palette, account: &Account, spec: &RowSpec, index: usize, value: Vec<Span<'static>>, width: usize,
) -> Line<'static> {
    // The tint follows the list's selection alone, so a blurred pane keeps its last-selected
    // row's tint (contract: Pane focus); the caret and the bold promotion belong to the pane
    // that owns the caret, which is descended only.
    let selected = account.detail_list.selected() == Some(index);
    let focused = account.descended && selected;
    static_row(palette, caret(palette, focused), spec.label, value, selected, width)
}

/// A row's value before formatting, borrowed from the loaded file so no frame clones it.
#[derive(Debug, Clone, Copy)]
enum Value<'a> {
    Count(usize),
    Stamp(Timestamp),
    Text(&'a str),
    Identity(&'a str),
}

/// The detail's rows for the selected section. A section whose file is absent or broken — the
/// `.ok().flatten()` swallowed the read — renders its spec rows with the absent token, never
/// an error surface (the privacy rule).
fn detail_rows(palette: &Palette, account: &Account, width: usize) -> Vec<Line<'static>> {
    let section = account.section();
    let values: Option<Vec<Option<Value<'_>>>> = match section {
        Section::Account => account.account.as_ref().map(account_values),
        Section::Friends => account.friends.as_ref().map(friends_values),
        Section::Location => account.location.as_ref().map(location_values),
        Section::Stories => account.stories.as_ref().map(stories_values),
        Section::Subscriptions => account.subscriptions.as_ref().map(subscriptions_values),
    };
    // The clock for the age rule: a real `now` at render. `None` would take a broken clock or
    // a clock before the epoch, and the fixed shape is then the honest fallback (the tests pin
    // the age helper directly with a fixed `now`).
    let now = now_clock();
    section_rows(section)
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            // Each row's value truncates against its own budget: the row is ragged, so the
            // value column is not shared.
            let budget = width.saturating_sub(CARET_GUTTER + cells(spec.label) + LABEL_GAP);
            let spans = values
                .as_ref()
                .and_then(|rows| rows[index].as_ref())
                .map_or_else(|| vec![absent(palette)], |value| value_spans(palette, value, budget, now));
            detail_row(palette, account, spec, index, spans, width)
        })
        .collect()
}

/// The absent value's token: the same `—` the overview's value slots use.
fn absent(palette: &Palette) -> Span<'static> {
    Span::styled("—", Style::new().fg(palette.text_faint))
}

/// One value's spans: counts stay whole and grouped (a count that outgrows the panel clips at
/// its edge), a timestamp renders per the age rule, and text truncates — prose takes the prose
/// cut, identity keeps both ends.
fn value_spans(palette: &Palette, value: &Value<'_>, budget: usize, now: Option<Timestamp>) -> Vec<Span<'static>> {
    let span = match value {
        Value::Count(count) => Span::styled(grouped(*count), Style::new().fg(palette.text)),
        Value::Stamp(stamp) => {
            // The contract's age rule (cloudy-tui Time formatting); a broken clock falls back
            // to the fixed shape.
            let text = now.map_or_else(|| stamp.to_string(), |now| stamp_text(*stamp, now));
            Span::styled(text, Style::new().fg(palette.text))
        }
        Value::Text(text) => Span::styled(truncate_prose(text, budget), Style::new().fg(palette.text)),
        Value::Identity(text) => Span::styled(middle_ellipsis(text, budget), Style::new().fg(palette.text)),
    };
    vec![span]
}

/// The contract's age rule (cloudy-tui Time formatting): relative under 30 days — the largest
/// whole unit with a count of at least one — and the absolute ISO date at 30 days and past. A
/// stamp the calendar rejects has no elapsed time and a future stamp has a nonsense negative
/// age; both render the absolute date. `now` is the caller's clock, so the tests pin both arms.
fn stamp_text(stamp: Timestamp, now: Timestamp) -> String {
    let Some(age) = age_seconds(stamp, now) else { return iso_date(stamp) };
    if age < 0 {
        return iso_date(stamp);
    }
    let minutes = age / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days >= 30 {
        return iso_date(stamp);
    }
    if days < 7 {
        return format!("{days}d ago");
    }
    format!("{}w ago", days / 7)
}

/// The current instant as a [`Timestamp`], for the age rule. `std`'s clock, not chrono's: the
/// crate's chrono dependency has no `clock` feature, and the model's epoch conversion does the
/// rest.
fn now_clock() -> Option<Timestamp> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis();
    Timestamp::from_epoch_ms(i64::try_from(millis).unwrap_or(i64::MAX))
}

/// Whole elapsed seconds from `stamp` to `now`, through the crate's calendar conversion — the
/// same fallible path [`calendar`] exists for, so an unvalidated date (a day the calendar
/// rejects, like 2021-02-30) has no elapsed time rather than a guessed one.
fn age_seconds(stamp: Timestamp, now: Timestamp) -> Option<i64> {
    let from = calendar(stamp)?;
    let to = calendar(now)?;
    Some((to - from).num_seconds())
}

/// The absolute arm's shape: the stamp's date alone.
fn iso_date(stamp: Timestamp) -> String {
    format!("{:04}-{:02}-{:02}", stamp.year(), stamp.month(), stamp.day())
}

// ---- per-section values ----

fn account_values(account: &model::Account) -> Vec<Option<Value<'_>>> {
    let basics = &account.basics;
    vec![
        basics.username.as_ref().map(|username| Value::Identity(username.as_str())),
        basics.name.as_ref().map(|name| Value::Text(name)),
        basics.created.map(Value::Stamp),
        basics.country.as_ref().map(|country| Value::Text(country)),
        basics.last_active.as_ref().map(|last_active| Value::Text(last_active)),
        Some(Value::Count(account.device_history.len())),
        Some(Value::Count(account.logins.len())),
    ]
}

fn friends_values(friends: &model::Friends) -> Vec<Option<Value<'_>>> {
    vec![
        Some(Value::Count(friends.friends.len())),
        Some(Value::Count(friends.requests_sent.len())),
        Some(Value::Count(friends.blocked.len())),
        Some(Value::Count(friends.deleted.len())),
        Some(Value::Count(friends.hidden_suggestions.len())),
        Some(Value::Count(friends.ignored.len())),
        Some(Value::Count(friends.pending_requests.len())),
        Some(Value::Count(friends.shortcuts.len())),
    ]
}

fn location_values(location: &schema::LocationHistory) -> Vec<Option<Value<'_>>> {
    vec![
        Some(Value::Count(location.frequent_locations.len())),
        Some(Value::Count(location.latest_location.len())),
        Some(Value::Count(location.home_school_work.len())),
        Some(Value::Count(location.daily_top_locations.len())),
        Some(Value::Count(location.top_locations_per_six_day_period.len())),
        Some(Value::Count(location.location_history.len())),
        Some(Value::Count(location.businesses_visited.len())),
        Some(Value::Count(location.actiomoji_information.len())),
        Some(Value::Count(location.areas_visited.len())),
    ]
}

fn stories_values(stories: &schema::StoryHistory) -> Vec<Option<Value<'_>>> {
    // The sums are saturating folds: `Iterator::sum` panics on overflow in debug builds, and
    // the counts come from json.
    let views = stories.your_story_views.iter().fold(0u64, |sum, entry| sum.saturating_add(entry.story_views));
    let replies = stories.your_story_views.iter().fold(0u64, |sum, entry| sum.saturating_add(entry.story_replies));
    vec![
        Some(Value::Count(stories.your_story_views.len())),
        Some(Value::Count(usize::try_from(views).unwrap_or(usize::MAX))),
        Some(Value::Count(usize::try_from(replies).unwrap_or(usize::MAX))),
        Some(Value::Count(stories.friend_and_public_story_views.len())),
    ]
}

fn subscriptions_values(profile: &model::UserProfile) -> Vec<Option<Value<'_>>> {
    vec![Some(Value::Count(profile.subscriptions))]
}

// ---- compile-time floors ----

/// The master-only arm renders the section list into the whole body: the list must fit the
/// compact shell's guaranteed interior rows, like the overview's fixed-height summary.
const _: () = assert!(Section::ALL.len() as u16 <= GUARANTEED_INTERIOR_ROWS);

#[cfg(test)]
mod tests {
    use super::*;

    /// The timestamp floor is the one value width the layout gate cannot derive from the row
    /// copy — it restates the age rule's absolute form, so it carries a test instead of a
    /// promise.
    #[test]
    fn the_timestamp_value_floor_matches_the_iso_form() {
        let stamp = Timestamp::parse(crate::export::model::Field::Date, "2021-02-30 10:00:00 UTC").unwrap();
        assert_eq!(TIMESTAMP_CELLS, cells(&iso_date(stamp)));
    }

    /// The layout budgets restate the row copy's shape, so they carry a test instead of a
    /// promise. The master pane sits on the contract's clamp floor, above its own 19-cell
    /// content need.
    #[test]
    fn the_layout_budgets_follow_from_the_copy() {
        assert_eq!(
            sections_interior(),
            (CARET_GUTTER + cells("subscriptions")).max(usize::from(SELECTOR_CLAMP_FLOOR) - usize::from(widgets::CHROME_COLUMNS))
        );
        assert_eq!(usize::from(sections_panel_width()), SELECTOR_CLAMP_FLOOR as usize);
        // The widest pair is a count row's label — "friend & public stories" — now that the
        // timestamp's shape shrank to its ISO form: the formula floors only the timestamp
        // rows, and a count that outgrows the panel clips at its edge (the documented
        // exception). The timestamp floor still exists as the formula's one value floor.
        assert_eq!(detail_interior(), CARET_GUTTER + LABEL_GAP + cells("friend & public stories"));
        assert_eq!(usize::from(detail_panel_min_width()), detail_interior() + usize::from(widgets::CHROME_COLUMNS));
        // The tallest detail is the location section's nine rows.
        assert_eq!(usize::from(stacked_height()), LOCATION_ROWS.len() + Section::ALL.len() + 2 * usize::from(widgets::BORDER_ROWS));
    }

    /// The age rule's arms and boundaries, against a fixed clock so both arms are pinned:
    /// relative under 30 days with the largest whole unit, absolute ISO at 30 days and past,
    /// and the absolute form for the instants the calendar or the clock cannot vouch for.
    #[test]
    fn the_age_rule_renders_relative_under_thirty_days_and_iso_at_thirty() {
        let now = Timestamp::parse(crate::export::model::Field::Date, "2026-08-13 12:00:00 UTC").unwrap();
        let stamp = |s: &str| Timestamp::parse(crate::export::model::Field::Date, s).unwrap();
        // A minute's floor: under a minute is "0m ago", not an ISO date.
        assert_eq!(stamp_text(stamp("2026-08-13 11:59:01 UTC"), now), "0m ago");
        assert_eq!(stamp_text(stamp("2026-08-13 11:55:00 UTC"), now), "5m ago");
        assert_eq!(stamp_text(stamp("2026-08-13 10:00:00 UTC"), now), "2h ago");
        assert_eq!(stamp_text(stamp("2026-08-10 12:00:00 UTC"), now), "3d ago");
        // 7 days and up render the week unit; 29 days is the last relative day.
        assert_eq!(stamp_text(stamp("2026-08-06 12:00:00 UTC"), now), "1w ago");
        assert_eq!(stamp_text(stamp("2026-07-30 12:00:00 UTC"), now), "2w ago");
        assert_eq!(stamp_text(stamp("2026-07-15 12:00:00 UTC"), now), "4w ago");
        // 30 days and past flip to the absolute ISO date — the fixture's 2019 stamp is the
        // same arm, which is what the screen test pins.
        assert_eq!(stamp_text(stamp("2026-07-14 12:00:00 UTC"), now), "2026-07-14");
        assert_eq!(stamp_text(stamp("2019-05-04 10:00:00 UTC"), now), "2019-05-04");
        // A future stamp and one the calendar rejects have no honest age: absolute.
        assert_eq!(stamp_text(stamp("2026-08-13 12:00:01 UTC"), now), "2026-08-13");
        assert_eq!(stamp_text(stamp("2021-02-30 10:00:00 UTC"), now), "2021-02-30");
    }
}
