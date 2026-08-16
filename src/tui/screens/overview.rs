//! The overview tab: two read-only panels side by side, an export summary and the environment
//! (`docs/design.md`, TUI screen map).
//!
//! **Metadata only.** Counts, year ranges, tool statuses, byte figures and the source path the
//! user named on the command line. No message text, no filename out of the export, no username, no
//! download url ever reaches this module — the load path deliberately discards the errors that
//! could carry one (see [`Counts::of`]).
//!
//! Neither panel has a cursor — nothing on this screen is focusable — but the summary is the
//! screen's primary and renders `LINE_STRONG` while the environment stays `LINE` (ruling: one
//! panel strong; a sole summary panel in the narrow fallback counts as focused per the contract).
//! The summary's empty state carries one hotkey, which opens the source-path input this screen
//! holds while it is live.

use std::io;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};

use crate::export::env::{Environment, Tool};
use crate::export::model::{self, Conversation, ParseError, Timestamp};
use crate::export::read_model;
use crate::export::schema;
use crate::export::zip::{PartGroup, discover_parts};
use crate::tui::shell;
use crate::tui::theme::{Palette, glyph};
use crate::tui::widgets::{self, EMPTY_STATE_ROWS, PanelStyle, min_width_for_title, panel};

/// Both border columns plus the panel's 1-cell horizontal padding on each side, taken from the
/// widget that draws them rather than restated here — the width-axis twin of
/// [`GUARANTEED_INTERIOR_ROWS`]'s terms, and a second literal would check nothing the same way.
const PANEL_CHROME: usize = widgets::CHROME_COLUMNS as usize;
/// A display row's gap between the label column and the value column ("≥ 2 spaces").
const LABEL_GAP: usize = 2;
/// The empty state's inset inside its own frame, matching the contract's example.
const EMPTY_STATE_INSET: usize = 3;
/// The narrow floor of the source row's value: the fewest cells it occupies before head-ellipsising.
/// A wider panel hands the value the interior width left after the label column (in `render_panel`),
/// so a short path shows whole; this only stops the column shrinking below the whole-or-not-at-all
/// budget, which keeps the responsive breakpoint from moving with how deep the source dir sits.
const SOURCE_PATH_CELLS: usize = 18;

const DISK_FREE_LABEL: &str = "disk free";
const SOURCE_LABEL: &str = "source";

/// The key that opens the source-path input from the summary's empty state. `s` for "set source";
/// not one of the reserved `a`/`x`/`?`/`q` (cloudy-tui: Action menu → Hotkey assignment).
const SOURCE_HOTKEY: char = 's';

/// Every row the summary panel can render, in report order.
///
/// Single source for three things that must agree: the label text, the width of the value column,
/// and the panel's maximum row count. A rename or a new row is one edit here, and nothing
/// downstream can be left spelling the old one — the value column pads without truncating, so a
/// desynced label would silently push its own value out of the column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryRow {
    Parts,
    /// Rendered only when the delivery has a gap.
    Missing,
    Memories,
    Chats,
    Snaps,
    Friends,
}

impl SummaryRow {
    const ALL: [Self; 6] = [Self::Parts, Self::Missing, Self::Memories, Self::Chats, Self::Snaps, Self::Friends];

    const fn label(self) -> &'static str {
        match self {
            Self::Parts => "parts",
            Self::Missing => "missing",
            Self::Memories => "memories",
            Self::Chats => "chats",
            Self::Snaps => "snaps",
            Self::Friends => "friends",
        }
    }
}

/// Interior rows a body panel is guaranteed **at or above** the compact floor.
///
/// Rows clip down the height rather than blanking the panel, which is only honest while every row
/// either panel can render fits this. Checked rather than asserted in prose: add a seventh summary
/// row and the build stops, which is the point at which the panel earns the contract's scrollbar
/// instead of this invariant.
///
/// Where each term comes from, because that is what decides whether the check means anything:
/// `COMPACT_HEIGHT`, `HEADER_ROWS` and `FOOTER_ROWS` are the shell's own — its vertical layout is
/// built from those three bindings, so this cannot drift from the geometry it describes.
/// [`widgets::BORDER_ROWS`] is the one restatement; const evaluation cannot reach `Block::inner`,
/// so it carries its own test instead.
///
/// **The size banner is not subtracted, and that is exact rather than slack.** A banner reaches a
/// BODY row only through `shell::render`'s `header_fits && area.height < COMPACT_HEIGHT` arm, so a
/// body banner and a frame at or above the floor are mutually exclusive — at exactly
/// `COMPACT_HEIGHT` that `<` is false. The other banner path takes the HEADER's row and leaves the
/// body whole. Both are pinned by `every_summary_row_survives_the_compact_floor`, because this
/// paragraph is reasoning about another file and reasoning is what quietly goes stale.
///
/// Below the floor the interior shrinks to `h - 5` — header row, footer row, banner row, two borders
/// — so h11 is the last height that shows all six summary rows and clipping starts at h10 (h9 for an
/// ordinary five-row delivery, which has no `missing` row). Deliberately uncovered here: the banner
/// is up at those heights saying the terminal is too small, which is the whole reason that floor
/// exists. Those four figures are pinned by `row_clipping_begins_one_row_below_the_last_height_that_fits`
/// rather than left as arithmetic in a comment.
///
/// Scope: this covers the two arms where one panel owns the whole body — the overview's panels
/// and the memories screen's form-only arm both assert against it. The stacked arms split the
/// body, so what keeps those safe is their own height gates, not this constant.
pub(crate) const GUARANTEED_INTERIOR_ROWS: u16 = shell::COMPACT_HEIGHT - shell::HEADER_ROWS - shell::FOOTER_ROWS - widgets::BORDER_ROWS;
const _: () = assert!(SummaryRow::ALL.len() as u16 <= GUARANTEED_INTERIOR_ROWS);
const _: () = assert!(ENVIRONMENT_ROWS <= GUARANTEED_INTERIOR_ROWS);

/// One row per tool, plus `disk free` and `source`.
const ENVIRONMENT_ROWS: u16 = Tool::ALL.len() as u16 + 2;

/// Everything the overview renders, read once at startup.
#[derive(Debug)]
pub struct Overview {
    source: Option<PathBuf>,
    parts: Parts,
    counts: Counts,
    environment: Environment,
    /// The live source-path input, `Some` while the summary's empty-state hotkey has it open.
    editing: Option<EditSession>,
}

impl Overview {
    /// The state before anything has been read: no source named, nothing found, nothing measured.
    /// `App::start` replaces it before the first frame, so this is what a render test or a pre-load
    /// frame draws.
    #[must_use]
    pub fn unloaded() -> Self {
        Self { source: None, parts: Parts::None, counts: Counts::absent(), environment: Environment::default(), editing: None }
    }

    /// What the machine could do when this screen was built. Test-only: `App`'s own startup test
    /// reads it to pin that one probe reaches every screen.
    #[cfg(test)]
    pub(crate) const fn environment(&self) -> &Environment {
        &self.environment
    }

    /// Reads `source_dir` once: which export parts sit there, what an unpacked `json/` holds, and
    /// what the machine can do.
    ///
    /// Never fails. An export that is absent, ambiguous, unlisted or unreadable is a normal state
    /// the screen has words for — pointing at one is something the user does after launch, not a
    /// precondition for drawing a frame.
    #[must_use]
    pub fn load(source_dir: impl AsRef<Path>) -> Self {
        let source_dir = source_dir.as_ref();
        Self::load_with(source_dir, Environment::probe(source_dir))
    }

    /// [`Self::load`] with the environment already probed — the seam a test drives to pin
    /// ffmpeg-present against ffmpeg-absent without reaching for the real `PATH`.
    #[must_use]
    pub fn load_with(source_dir: impl AsRef<Path>, environment: Environment) -> Self {
        let source = source_dir.as_ref().to_path_buf();
        let (parts, counts) = match discover_parts(&source) {
            // The error's TEXT is dropped — a one-line value slot cannot hold it, and the empty
            // state names the fix instead. Its KIND is not: that carries no path and no content, and
            // it is the difference between two failures with different fixes. Upgrade path for the
            // full text: the footer alert, once a second alert exists to justify wiring dismissal.
            Err(error) if error.source.kind() == io::ErrorKind::NotFound => (Parts::Missing, Counts::absent()),
            Err(_) => (Parts::Unreadable, Counts::absent()),
            Ok(groups) => match groups.as_slice() {
                [] => (Parts::None, Counts::absent()),
                [group] => (Parts::of(group), Counts::of(group)),
                several => (Parts::Several(several.len()), Counts::absent()),
            },
        };

        Self { source: Some(source), parts, counts, environment, editing: None }
    }

    /// This screen's share of the `--print-source` report: the dir it was built against, what the
    /// read of it found, and the free space measured on that dir's filesystem. See
    /// [`crate::app::App::source_report`] for the format and for why the three screens each
    /// contribute their own lines.
    ///
    /// Lives here rather than in `App` because [`Parts`] and its numbers are private, and publishing
    /// an enum plus five variants to format four lines elsewhere is the larger surface.
    ///
    /// The numeric keys are emitted only by the states that measured them. A `zips=0` under
    /// `parts=missing`, or a `free=0` where no `statvfs` succeeded, would be a confident wrong answer
    /// where the truth is that nothing was counted — the same distinction [`Totals`] keeps for the
    /// json counts and [`Environment`] for its two space figures.
    pub(crate) fn report(&self) -> String {
        let found = match self.parts {
            Parts::Missing => "parts=missing".to_owned(),
            Parts::Unreadable => "parts=unreadable".to_owned(),
            Parts::None => "parts=none".to_owned(),
            Parts::Several(exports) => format!("parts=several\nexports={exports}"),
            Parts::One { zips, unpacked, missing } => format!("parts=one\nzips={zips}\nunpacked={unpacked}\nmissing={missing}"),
        };
        // Measured on the SOURCE's filesystem, not the output root's — `App::start` probes here and
        // re-measures at the out root for the media screens, so these two keys are the only report
        // of the argument this screen was probed against.
        let space: String = [("free", self.environment.available_space), ("total", self.environment.total_space)]
            .into_iter()
            .filter_map(|(key, bytes)| bytes.map(|bytes| format!("{key}={bytes}\n")))
            .collect();
        // `unloaded` is the pre-read state and never reaches the flag, which prints off a started
        // app; an empty value keeps the key set the same either way rather than dropping a line.
        let source = self.source.as_deref().unwrap_or(Path::new(""));
        format!("source={source:?}\n{found}\n{space}")
    }

    /// Whether the source-path input is open — the app's `q`/`x`/`?`/`a` suspension reads it,
    /// exactly like the settings form's.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// This screen's keys. While the path input is open, its editing keys are consumed and a key it
    /// does not own — a `⌥<digit>` jump above all — comes back unhandled so the shell sees it; with
    /// the input closed, [`SOURCE_HOTKEY`] opens it from the summary's empty state.
    pub fn handle_key(&mut self, key: KeyEvent) -> OverviewKey {
        if self.editing.is_some() {
            return self.handle_edit_key(key);
        }
        if self.is_empty()
            && matches!(key.code, KeyCode::Char(c) if c.to_ascii_lowercase() == SOURCE_HOTKEY)
            && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            // Seed the draft from the current source so a typo is corrected rather than retyped; a
            // first-time open has no source and starts empty.
            let draft = self.source.as_ref().map_or_else(String::new, |path| path.to_string_lossy().into_owned());
            let caret = draft.chars().count();
            self.editing = Some(EditSession { draft, caret });
            return OverviewKey::Handled;
        }
        OverviewKey::Unhandled
    }

    /// One key while the path input is open: `enter` commits a non-empty draft as the source to
    /// re-probe, `esc` cancels, and the editing keys edit the draft. A key the field does not own —
    /// a `⌥<digit>` jump above all — comes back [`OverviewKey::Unhandled`], so the shell's own
    /// bindings still see it (cloudy-tui: `⌥<digit>` never suspends, live in edit mode). That is
    /// what lets a jump leave the input with its draft suspended instead of trapping the user until
    /// `esc` discards it.
    fn handle_edit_key(&mut self, key: KeyEvent) -> OverviewKey {
        match key.code {
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                let Some(session) = self.editing.take() else { return OverviewKey::Unhandled };
                if session.draft.is_empty() { OverviewKey::Handled } else { OverviewKey::Reprobbed(PathBuf::from(session.draft)) }
            }
            KeyCode::Esc if key.modifiers == KeyModifiers::NONE => {
                self.editing = None;
                OverviewKey::Handled
            }
            _ => {
                let Some(session) = self.editing.as_mut() else { return OverviewKey::Unhandled };
                if edit_session_key(session, key) { OverviewKey::Handled } else { OverviewKey::Unhandled }
            }
        }
    }

    /// Whether the summary is showing its empty state rather than rows — the only state the hotkey
    /// advertises from.
    fn is_empty(&self) -> bool {
        !matches!(self.parts, Parts::One { .. })
    }
}

/// What the source dir holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parts {
    /// The dir is not there. Kept apart from [`Self::Unreadable`] because a typo in `--source` is
    /// the likeliest failure of the lot, and "unreadable" both misdiagnoses it as a permissions or
    /// IO fault and answers with the step the user just took.
    Missing,
    /// The dir is there and could not be listed.
    Unreadable,
    /// Nothing in it is shaped like a Snapchat delivery.
    None,
    /// Several deliveries share the dir, so which one the screen is about would be a guess. It
    /// reports how many instead of picking one.
    Several(usize),
    /// Exactly one delivery.
    One { zips: usize, unpacked: usize, missing: usize },
}

impl Parts {
    fn of(group: &PartGroup) -> Self {
        Self::One { zips: group.zips.len(), unpacked: group.extracted.len(), missing: group.missing_parts().len() }
    }
}

/// The `json/` dir's four counts, each decided by its own file.
///
/// Every count is optional because `ExportJson` holds every file it models optionally: a `json/`
/// that arrived without `chat_history.json` must not report `0` chats, which is a confident wrong
/// answer where the truth is "that section is not here". [`Section::Loaded`]`(0)` is the section
/// being present and genuinely empty, and only that renders a zero. A malformed file reads
/// `unreadable` for its own count alone — its siblings keep their own answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    memories: Section,
    /// Earliest and latest year across the memories that carry a date; meaningful only when
    /// `memories` is [`Section::Loaded`].
    years: Option<(u16, u16)>,
    chats: Section,
    snaps: Section,
    /// Accepted friends only. Blocked, deleted and pending lists are their own thing and belong on
    /// the account tab, not folded into one headline number here.
    friends: Section,
}

/// One count's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// The file behind the section is not in the `json/` dir.
    Absent,
    /// The file is there and did not load.
    Unreadable,
    /// The file loaded to this count.
    Loaded(usize),
}

impl Counts {
    /// No `json/` to read: every section absent.
    fn absent() -> Self {
        Self { memories: Section::Absent, years: None, chats: Section::Absent, snaps: Section::Absent, friends: Section::Absent }
    }

    fn of(group: &PartGroup) -> Self {
        // Only the first part carried `json/` in the one export observed, so this walks every
        // unpacked part rather than assuming which one has it.
        let Some(json_dir) = group.extracted.iter().find_map(|part| part.json_dir.as_deref()) else {
            return Self::absent();
        };

        // Each file is read on its own, so a malformed one lands as `Unreadable` for its own count
        // while the rest still resolve. The error text never reaches the screen: a `ParseError`
        // carries the offending value, and `Field::Location` makes that value a coordinate pair, so
        // the word `unreadable` is the whole report.
        let (memories, years) = match read_model::<schema::MemoriesHistory, model::Memories>(json_dir, "memories_history.json") {
            Ok(None) => (Section::Absent, None),
            Ok(Some(history)) => {
                let years = year_span(history.saved_media.iter().filter_map(|memory| memory.date));
                (Section::Loaded(history.saved_media.len()), years)
            }
            Err(_) => (Section::Unreadable, None),
        };

        Self {
            memories,
            years,
            chats: count_section::<schema::ChatHistory, model::ChatHistory>(json_dir, "chat_history.json", |history| {
                records(&history.conversations)
            }),
            snaps: count_section::<schema::SnapHistory, model::SnapHistory>(json_dir, "snap_history.json", |history| {
                records(&history.conversations)
            }),
            friends: count_section::<schema::Friends, model::Friends>(json_dir, "friends.json", |friends| friends.friends.len()),
        }
    }
}

/// Reads one model file into a count: absent, unreadable, or the loaded count `count` derives.
fn count_section<S, M>(json_dir: &Path, file: &'static str, count: impl FnOnce(&M) -> usize) -> Section
where
    S: serde::de::DeserializeOwned,
    M: TryFrom<S, Error = ParseError>,
{
    match read_model::<S, M>(json_dir, file) {
        Ok(None) => Section::Absent,
        Ok(Some(model)) => Section::Loaded(count(&model)),
        Err(_) => Section::Unreadable,
    }
}

fn records<T>(conversations: &[Conversation<T>]) -> usize {
    conversations.iter().map(|conversation| conversation.records.len()).sum()
}

fn year_span(dates: impl Iterator<Item = Timestamp>) -> Option<(u16, u16)> {
    dates.fold(None, |span, date| {
        let year = date.year();
        Some(span.map_or((year, year), |(first, last)| (first.min(year), last.max(year))))
    })
}

// ---- the source-path input ----

/// What [`Overview::handle_key`] did with a key, so the shell can route tab-switching and the
/// re-probe off one answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverviewKey {
    /// The key was not this screen's; the shell's own bindings should see it.
    Unhandled,
    /// The screen handled the key.
    Handled,
    /// `enter` committed a non-empty path; re-probe the source at it.
    Reprobbed(PathBuf),
}

/// One live source-path edit: the draft and the caret as a CHAR index into it. Chars rather than
/// bytes so a wide or multi-byte character never splits a grapheme (the same model the settings
/// form keeps; that one is private to settings.rs, which is out of scope for this screen).
#[derive(Debug)]
struct EditSession {
    draft: String,
    caret: usize,
}

/// One editing key against the draft (cloudy-tui: Text input — edit grammar). Mirrors the settings
/// form's `edit_session_key` for the same reason as [`EditSession`]: settings.rs is out of scope.
/// `false` for a key the field does not own — a `⌥<digit>` jump above all — so the shell's own
/// bindings still see it.
fn edit_session_key(session: &mut EditSession, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            let mut chars: Vec<char> = session.draft.chars().collect();
            chars.insert(session.caret.min(chars.len()), c);
            session.draft = chars.into_iter().collect();
            session.caret += 1;
            true
        }
        KeyCode::Backspace if key.modifiers == KeyModifiers::NONE => {
            if session.caret > 0 {
                let mut chars: Vec<char> = session.draft.chars().collect();
                chars.remove(session.caret - 1);
                session.draft = chars.into_iter().collect();
                session.caret -= 1;
            }
            true
        }
        KeyCode::Delete if key.modifiers == KeyModifiers::NONE => {
            let mut chars: Vec<char> = session.draft.chars().collect();
            if session.caret < chars.len() {
                chars.remove(session.caret);
                session.draft = chars.into_iter().collect();
            }
            true
        }
        KeyCode::Char('w') if key.modifiers == KeyModifiers::CONTROL => {
            let mut chars: Vec<char> = session.draft.chars().collect();
            let mut start = session.caret.min(chars.len());
            while start > 0 && chars[start - 1] == ' ' {
                start -= 1;
            }
            while start > 0 && chars[start - 1] != ' ' {
                start -= 1;
            }
            chars.drain(start..session.caret.min(chars.len()));
            session.draft = chars.into_iter().collect();
            session.caret = start;
            true
        }
        KeyCode::Left if key.modifiers == KeyModifiers::NONE => {
            session.caret = session.caret.saturating_sub(1);
            true
        }
        KeyCode::Right if key.modifiers == KeyModifiers::NONE => {
            session.caret = (session.caret + 1).min(session.draft.chars().count());
            true
        }
        KeyCode::Home if key.modifiers == KeyModifiers::NONE => {
            session.caret = 0;
            true
        }
        KeyCode::End if key.modifiers == KeyModifiers::NONE => {
            session.caret = session.draft.chars().count();
            true
        }
        _ => false,
    }
}

/// The byte offset of the `caret`-th char, or the draft's length past its end.
fn char_byte(draft: &str, caret: usize) -> usize {
    draft.char_indices().nth(caret).map(|(byte, _)| byte).unwrap_or(draft.len())
}

/// The visible window of a draft path and the caret's cell offset within it.
///
/// The caret is a CHAR index; the window is measured in CELLS (a wide char is two). It keeps the
/// caret inside `budget` cells by dropping leading chars, so a long pasted path shows its tail and
/// a caret moved left shows its context. Mirrors the settings form's private `draft_window` /
/// `draft_window_text` pair — settings.rs is out of scope for this slice, so the overview's input
/// reimplements the two as one return.
fn input_window(draft: &str, caret: usize, budget: usize) -> (String, usize) {
    if cells(draft) <= budget {
        return (draft.to_owned(), cells(&draft[..char_byte(draft, caret)]));
    }
    let caret_byte = char_byte(draft, caret);
    let caret_cells = cells(&draft[..caret_byte]);
    let desired = (caret_cells + 1).saturating_sub(budget);
    let mut start = caret;
    let mut start_cells = 0;
    for (index, ch) in draft.chars().take(caret + 1).enumerate() {
        if start_cells >= desired {
            start = index;
            break;
        }
        start_cells += cells(&ch.to_string());
    }
    let text: String = draft
        .chars()
        .skip(start)
        .scan(0usize, |used, ch| {
            let width = cells(&ch.to_string());
            if *used + width > budget {
                None
            } else {
                *used += width;
                Some(ch)
            }
        })
        .collect();
    (text, caret_cells.saturating_sub(start_cells))
}

// ---- render ----

/// Draws the screen into `area`.
pub fn render(frame: &mut Frame, palette: &Palette, overview: &Overview, area: Rect) {
    let summary = summary_panel(palette, overview);
    let environment = environment_panel(palette, overview);

    let first = PanelStyle { first: true, focused: true };
    let second = PanelStyle { first: false, focused: false };

    let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(area);
    let side_by_side = usize::from(left.width) >= summary.min_width() && usize::from(right.width) >= environment.min_width();

    // Columns are what run out first on a narrow terminal, and rows are what is left, so a body too
    // narrow for two columns stacks them instead of dropping one (cloudy-tui `mobile.md`: "stack,
    // don't truncate", and its prescription for side-by-side pairs is to stack every column
    // full-width). Hiding a pane is a named anti-pattern there — an invisible loss is worse than a
    // stacked one — so the summary-only arm below is the last resort and not the narrow answer.
    //
    // The breakpoint is derived from what the panels need rather than set at `mobile.md`'s
    // illustrative 60 columns: it has to be the width at which side-by-side actually stops working,
    // which moves with the copy.
    let stack_height = summary.height() + environment.height();
    let widest = summary.min_width().max(environment.min_width());
    let stacked = usize::from(area.width) >= widest && area.height >= stack_height;

    if side_by_side {
        render_panel(frame, palette, summary, left, first);
        render_panel(frame, palette, environment, right, second);
    } else if stacked {
        // The summary takes exactly its rows and the environment takes the rest, so the borders
        // touch with no gap between them (0-cell panel gaps in every mode) and no blank band opens
        // up in the middle of the body.
        let [top, bottom] = Layout::vertical([Constraint::Length(summary.height()), Constraint::Fill(1)]).areas(area);
        render_panel(frame, palette, summary, top, first);
        render_panel(frame, palette, environment, bottom, second);
    } else {
        // Neither layout fits. The summary is the screen's primary content, so it takes what there
        // is; below its own minimum it renders its box and no rows, per `render_panel`.
        render_panel(frame, palette, summary, area, first);
    }
}

/// One of the screen's panels, built before the layout runs so its own width need is known.
struct ScreenPanel {
    title: &'static str,
    body: Body,
    /// The environment panel's source path, deferred because its value is head-ellipsised to the
    /// panel's actual interior width rather than a fixed cell count. `None` on the summary panel
    /// and on an environment with no source (whose `—` row is fixed-width and stays in `body`).
    source: Option<String>,
}

/// A panel's interior.
enum Body {
    /// Display rows.
    Rows(Vec<Line<'static>>),
    /// The framed empty state that replaces the rows when there is nothing to count.
    Empty { hint: String, action: Line<'static> },
    /// The empty state with the source-path input open: the hint stays, the input replaces the
    /// action line, and the native cursor marks the caret.
    Input { hint: String, draft: String, caret: usize },
}

impl ScreenPanel {
    /// The panel width this content needs to render whole.
    fn min_width(&self) -> usize {
        let source = self.source.as_ref().map_or(0, |_| environment_label_column() + SOURCE_PATH_CELLS);
        min_width_for_title(self.title).max(PANEL_CHROME + self.body.width()).max(PANEL_CHROME + source)
    }

    /// The panel height that shows every row this content has, borders included.
    fn height(&self) -> u16 {
        self.body.height() + u16::from(self.source.is_some()) + widgets::BORDER_ROWS
    }
}

impl Body {
    /// Interior cells needed.
    fn width(&self) -> usize {
        match self {
            Self::Rows(lines) => lines.iter().map(Line::width).max().unwrap_or(0),
            Self::Empty { hint, action } => cells(hint).max(action.width()) + 2 * EMPTY_STATE_INSET + 2,
            // The input is ellipsised to whatever the panel leaves, so it never widens the panel
            // past the action line that advertised the hotkey.
            Self::Input { hint, .. } => cells(hint).max(source_action_cells()) + 2 * EMPTY_STATE_INSET + 2,
        }
    }

    /// Interior rows to show everything this body holds.
    fn height(&self) -> u16 {
        match self {
            Self::Rows(lines) => u16::try_from(lines.len()).unwrap_or(u16::MAX),
            Self::Empty { .. } | Self::Input { .. } => EMPTY_STATE_ROWS,
        }
    }

    /// Interior rows below which nothing renders at all.
    ///
    /// Rows and the empty state differ here, and that is the real axis of this screen's
    /// all-or-nothing rule — not width versus height. A row list degrades honestly one row at a
    /// time, since every row that renders is still whole. The empty-state frame does not: cut a row
    /// off it and the box is either unclosed or missing a line of the copy that is its entire
    /// content, so it is all or nothing.
    ///
    /// The row list's laxity is only sound because `GUARANTEED_INTERIOR_ROWS` makes clipping
    /// unreachable above the compact floor, and that is a compile-time assertion rather than a
    /// comment about another file.
    fn min_height(&self) -> u16 {
        match self {
            Self::Rows(_) => 1,
            Self::Empty { .. } | Self::Input { .. } => EMPTY_STATE_ROWS,
        }
    }
}

fn render_panel(frame: &mut Frame, palette: &Palette, content: ScreenPanel, area: Rect, style: PanelStyle) {
    let block = panel(palette, content.title, style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // The deferred source row's narrow floor counts toward the whole-or-not-at-all width, so the
    // panel still refuses to clip it below [`SOURCE_PATH_CELLS`].
    let mut need = content.body.width();
    if content.source.is_some() {
        need = need.max(environment_label_column() + SOURCE_PATH_CELLS);
    }

    // Whole or not at all, across the width. A row clipped mid-way hides its value beside a label
    // that is still there, which reads as "no value" rather than as "no room" — the one failure a
    // read-only panel must not have, and the contract offers no horizontal-overflow mechanism for a
    // key:value row. Down the height, see `Body::min_height`.
    if usize::from(inner.width) < need || inner.height < content.body.min_height() {
        return;
    }

    match content.body {
        Body::Rows(mut lines) => {
            if let Some(source) = content.source {
                // The source is this panel's last row: head-ellipsise it to whatever the interior
                // actually leaves after the label column, so a short path shows whole.
                let budget = usize::from(inner.width).saturating_sub(environment_label_column()).max(SOURCE_PATH_CELLS);
                let column = environment_label_column();
                lines.push(row(palette, SOURCE_LABEL, column, vec![value_span(palette, head_ellipsis(&source, budget))]));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        Body::Empty { hint, action } => {
            let width = u16::try_from(need).unwrap_or(u16::MAX);
            let frame_area = inner.centered(Constraint::Length(width), Constraint::Length(EMPTY_STATE_ROWS));
            let copy = Style::new().fg(palette.text_dim);

            // Hint line plus an action line naming the hotkey. The contract colors the action
            // line's HOTKEY LETTER `ACCENT` and its LABEL `TEXT_DIM`, which is what `source_action`
            // builds.
            frame.render_widget(Paragraph::new(vec![Line::styled(hint, copy), action]).block(empty_block(palette)), frame_area);
        }
        Body::Input { hint, draft, caret } => {
            let width = u16::try_from(need).unwrap_or(u16::MAX);
            let frame_area = inner.centered(Constraint::Length(width), Constraint::Length(EMPTY_STATE_ROWS));
            let copy = Style::new().fg(palette.text_dim);

            // The input replaces the action line: `✎ ` plus the draft window, the native cursor at
            // the caret (cloudy-tui: Text input — the terminal's own cursor marks the caret).
            let budget = usize::from(width).saturating_sub(2 + 2 * EMPTY_STATE_INSET + 2);
            let (window, caret_cells) = input_window(&draft, caret, budget);
            let edit = Span::styled(format!("{} ", glyph::EDIT_GLYPH), Style::new().fg(palette.accent).bold());
            let value = Span::styled(window, Style::new().fg(palette.text));
            frame.render_widget(
                Paragraph::new(vec![Line::styled(hint, copy), Line::from(vec![edit, value])]).block(empty_block(palette)),
                frame_area,
            );

            // The caret cell: the frame's left border, the inset, the edit glyph, then the caret's
            // offset within the window. Row 1 is the input (row 0 is the hint).
            let caret_x = frame_area.x
                + 1
                + u16::try_from(EMPTY_STATE_INSET).unwrap_or(u16::MAX)
                + 2
                + u16::try_from(caret_cells).unwrap_or(u16::MAX);
            frame.set_cursor_position(Position::new(caret_x, frame_area.y + 2));
        }
    }
}

/// The empty state's rounded frame (cloudy-tui: Empty state): `LINE` border, the 3-cell inset on
/// each side, no vertical padding. Shared by the [`Body::Empty`] and [`Body::Input`] arms.
fn empty_block(palette: &Palette) -> Block<'static> {
    let inset = u16::try_from(EMPTY_STATE_INSET).unwrap_or(u16::MAX);
    Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(palette.line)).padding(Padding::new(inset, inset, 0, 0))
}

// ---- export summary panel ----

fn summary_panel(palette: &Palette, overview: &Overview) -> ScreenPanel {
    let hint = match overview.parts {
        Parts::Missing => "source dir not found".to_owned(),
        Parts::Unreadable => "source dir unreadable".to_owned(),
        Parts::None => "no export found".to_owned(),
        Parts::Several(count) => format!("{count} exports found here"),
        Parts::One { zips, unpacked, missing } => {
            return ScreenPanel {
                title: "export summary",
                body: Body::Rows(summary_rows(palette, zips, unpacked, missing, overview.counts)),
                source: None,
            };
        }
    };

    let body = match &overview.editing {
        Some(session) => Body::Input { hint, draft: session.draft.clone(), caret: session.caret },
        None => Body::Empty { hint, action: source_action(palette) },
    };

    ScreenPanel { title: "export summary", body, source: None }
}

/// The empty state's action copy: the hotkey letter in `ACCENT`, the rest `TEXT_DIM` (cloudy-tui:
/// Empty state — action line).
fn source_action(palette: &Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled("press ", Style::new().fg(palette.text_dim)),
        Span::styled(SOURCE_HOTKEY.to_string(), Style::new().fg(palette.accent).bold()),
        Span::styled(" to set source", Style::new().fg(palette.text_dim)),
    ])
}

/// The action line's cell width, independent of styling: the hotkey is one ascii cell whatever the
/// letter, so [`source_action`]'s three parts sum the same with or without a palette.
fn source_action_cells() -> usize {
    cells("press ") + 1 + cells(" to set source")
}

fn summary_rows(palette: &Palette, zips: usize, unpacked: usize, missing: usize, counts: Counts) -> Vec<Line<'static>> {
    let column = label_column(SummaryRow::ALL.map(SummaryRow::label));
    let mut rows = vec![summary_row(palette, SummaryRow::Parts, column, value_span(palette, parts_text(zips, unpacked)))];

    // An element with no status shows nothing (Patterns → Status purity), so a complete delivery
    // carries no `missing` row at all.
    if missing > 0 {
        let text = format!("{} {}", grouped(missing), plural(missing, "part", "parts"));
        rows.push(summary_row(palette, SummaryRow::Missing, column, Span::styled(text, Style::new().fg(palette.danger))));
    }

    // Resolved together rather than row by row, because the numeric column's padding below needs
    // the widest of the three bare counts before any of them is laid out.
    let (memories, chats, snaps, friends) = json_values(palette, counts);

    // Left-pad the three bare counts to their shared widest width so their right edges line up
    // (Patterns → Numeric column alignment: ragged-left, anchored at the label, never right-
    // justified against the cell edge).
    //
    // `memories` is excluded by ROW KIND, not by what its data happens to look like this frame: its
    // value slot carries a count followed by an optional year clause, so it is a composite row and
    // never a member of the numeric column. Padding it would shove that whole clause rightwards.
    let numeric_width = [&chats, &snaps, &friends].into_iter().map(|(text, _)| cells(text)).max().unwrap_or(0);

    rows.push(json_row(palette, SummaryRow::Memories, column, memories, 0));
    for (kind, value) in [(SummaryRow::Chats, chats), (SummaryRow::Snaps, snaps), (SummaryRow::Friends, friends)] {
        rows.push(json_row(palette, kind, column, value, numeric_width));
    }
    rows
}

/// A resolved value plus the color its text carries, held apart from its `Span` so a column can be
/// measured before it is styled.
type JsonValue = (String, Color);

/// The four values the `json/` dir feeds, in report order, each decided by its own section.
fn json_values(palette: &Palette, counts: Counts) -> (JsonValue, JsonValue, JsonValue, JsonValue) {
    // `—` is "this number is not available" — whether because no part is unpacked, or because the
    // file behind one section was not in the `json/` that is. `unreadable` is the third, different
    // thing: json that is there and did not load. A `0` only ever means a section that IS there and
    // holds nothing.
    let memories = match counts.memories {
        Section::Absent => ("—".to_owned(), palette.text_faint),
        Section::Unreadable => ("unreadable".to_owned(), palette.danger),
        Section::Loaded(count) => (memories_text(count, counts.years), palette.text),
    };

    (memories, section_value(palette, counts.chats), section_value(palette, counts.snaps), section_value(palette, counts.friends))
}

/// One bare count's value: the `—` absent token, the `unreadable` word, or the grouped number.
fn section_value(palette: &Palette, section: Section) -> JsonValue {
    match section {
        Section::Absent => ("—".to_owned(), palette.text_faint),
        Section::Unreadable => ("unreadable".to_owned(), palette.danger),
        Section::Loaded(count) => (grouped(count), palette.text),
    }
}

fn summary_row(palette: &Palette, kind: SummaryRow, column: usize, value: Span<'static>) -> Line<'static> {
    row(palette, kind.label(), column, vec![value])
}

fn json_row(palette: &Palette, kind: SummaryRow, column: usize, (text, color): JsonValue, pad_to: usize) -> Line<'static> {
    summary_row(palette, kind, column, Span::styled(left_pad(&text, pad_to), Style::new().fg(color)))
}

/// `5 zips · 1 unpacked`, dropping either clause when its count is zero (Patterns → Counts and
/// plurals). A [`PartGroup`] always holds at least one part, so both clauses cannot vanish at once.
fn parts_text(zips: usize, unpacked: usize) -> String {
    let clauses: Vec<String> = [(zips, "zip", "zips"), (unpacked, "unpacked", "unpacked")]
        .into_iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, one, many)| format!("{} {}", grouped(count), plural(count, one, many)))
        .collect();

    clauses.join(&format!(" {} ", glyph::CLAUSE_SEPARATOR))
}

/// `1,284 · 2019-2021`, collapsing a one-year span to the single year and dropping the span
/// entirely when no memory carries a date.
///
/// `memories == 0` renders `0`, which is the contract applied rather than departed from: "Counts and
/// plurals → zero hides the count" is scoped to a count-plus-noun inside running copy, which
/// [`parts_text`] is and this is not. A value slot has its own no-value token, `—`, and it already
/// means something else here.
fn memories_text(memories: usize, years: Option<(u16, u16)>) -> String {
    let count = grouped(memories);
    match years {
        None => count,
        Some((first, last)) if first == last => format!("{count} {} {first}", glyph::CLAUSE_SEPARATOR),
        Some((first, last)) => format!("{count} {} {first}-{last}", glyph::CLAUSE_SEPARATOR),
    }
}

// ---- environment panel ----

fn environment_panel(palette: &Palette, overview: &Overview) -> ScreenPanel {
    let column = environment_label_column();
    let mut rows: Vec<Line<'static>> =
        Tool::ALL.into_iter().map(|tool| row(palette, tool.command(), column, tool_pill(palette, &overview.environment, tool))).collect();

    rows.push(row(
        palette,
        DISK_FREE_LABEL,
        column,
        vec![match overview.environment.available_space {
            Some(bytes) => value_span(palette, binary_bytes(bytes)),
            // A probe that failed is a charged state, not an absent value, so it takes `WARNING`
            // rather than the `—` a not-yet-read count carries.
            None => Span::styled("unknown", Style::new().fg(palette.warning)),
        }],
    ));

    // The source row is deferred when there IS a source: its value is head-ellipsised to the
    // panel's actual interior width at render time, so a short path shows whole. With no source,
    // the fixed `—` placeholder stays in the eager rows.
    let source = match &overview.source {
        Some(path) => Some(path.display().to_string()),
        None => {
            rows.push(row(palette, SOURCE_LABEL, column, vec![Span::styled("—", Style::new().fg(palette.text_faint))]));
            None
        }
    };

    ScreenPanel { title: "environment", body: Body::Rows(rows), source }
}

/// Status pill (component: Status pill): brackets `TEXT_DIM`, label semantic and bold. A missing
/// tool is `WARNING` and never `DANGER` — every one of these is optional and the pipeline degrades
/// without it (`docs/design.md`, decision 2).
fn tool_pill(palette: &Palette, environment: &Environment, tool: Tool) -> Vec<Span<'static>> {
    let (label, color) = if environment.tool(tool).is_some() { ("present", palette.success) } else { ("missing", palette.warning) };
    let bracket = Style::new().fg(palette.text_dim);

    vec![Span::styled("[ ", bracket), Span::styled(label, Style::new().fg(color).bold()), Span::styled(" ]", bracket)]
}

fn environment_label_column() -> usize {
    Tool::ALL.into_iter().map(|tool| cells(tool.command())).chain([cells(DISK_FREE_LABEL), cells(SOURCE_LABEL)]).max().unwrap_or(0)
        + LABEL_GAP
}

// ---- row building ----

/// One static key:value display row. The key stays `TEXT_DIM + bold` at all times — that bold is a
/// permanent anchor against the value, not a focus cue, and nothing here is focusable anyway.
///
/// Values stack in one column: these are non-selectable display rows, the only kind the contract
/// column-aligns.
fn row(palette: &Palette, label: &str, column: usize, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:<column$}"), Style::new().fg(palette.text_dim).bold())];
    spans.extend(value);
    Line::from(spans)
}

fn value_span(palette: &Palette, text: String) -> Span<'static> {
    Span::styled(text, Style::new().fg(palette.text))
}

fn label_column<const N: usize>(labels: [&str; N]) -> usize {
    labels.into_iter().map(cells).max().unwrap_or(0) + LABEL_GAP
}

/// Shared cell-aware formatting; see [`crate::tui::format`].
use crate::tui::format::{binary_bytes, cells, grouped, head_ellipsis, left_pad, plural};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parts_clause_drops_whichever_count_is_zero() {
        assert_eq!(parts_text(5, 1), "5 zips · 1 unpacked");
        assert_eq!(parts_text(5, 0), "5 zips");
        assert_eq!(parts_text(0, 3), "3 unpacked");
        assert_eq!(parts_text(1, 1), "1 zip · 1 unpacked");
    }

    #[test]
    fn a_single_year_span_renders_as_that_year_alone() {
        assert_eq!(memories_text(12, Some((2021, 2021))), "12 · 2021");
        assert_eq!(memories_text(1284, Some((2019, 2024))), "1,284 · 2019-2024");
        assert_eq!(memories_text(7, None), "7");
        // A section that IS there and holds nothing renders a zero, not the `—` an absent one gets.
        assert_eq!(memories_text(0, None), "0");
    }

    #[test]
    fn the_year_span_takes_the_extremes_in_any_order() {
        let stamp = |year: u16| Timestamp::parse(crate::export::model::Field::Date, &format!("{year}-05-04 10:00:00 UTC")).ok();

        assert_eq!(year_span([2021, 2019, 2024, 2020].into_iter().filter_map(stamp)), Some((2019, 2024)));
        assert_eq!(year_span([2019].into_iter().filter_map(stamp)), Some((2019, 2019)));
        assert_eq!(year_span(std::iter::empty()), None);
    }

    #[test]
    fn the_label_columns_are_the_widest_label_plus_the_gap() {
        // Derived rather than picked, so a longer label moves the column instead of clipping.
        assert_eq!(label_column(SummaryRow::ALL.map(SummaryRow::label)), 10);
        assert_eq!(environment_label_column(), 11);
    }

    #[test]
    fn every_summary_row_kind_carries_its_own_label() {
        // The column above is derived from this list, and every rendered row goes through
        // `SummaryRow::label`, so a rename cannot desync the two. This pins that the list is the
        // full set and holds no duplicate, which is what would silently mis-size the column.
        let labels = SummaryRow::ALL.map(SummaryRow::label);
        assert_eq!(labels, ["parts", "missing", "memories", "chats", "snaps", "friends"]);

        let mut unique = labels.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "a duplicate label would hide a row kind");

        // Second witness; `SummaryRow::label` above is the first. Survives it being weakened to a
        // wildcard. Residual and rationale: `MissingReason::ALL`, src/export/memories.rs. Never
        // collapse to `_ => {}`.
        for row in SummaryRow::ALL {
            match row {
                SummaryRow::Parts
                | SummaryRow::Missing
                | SummaryRow::Memories
                | SummaryRow::Chats
                | SummaryRow::Snaps
                | SummaryRow::Friends => {}
            }
        }
    }

    #[test]
    fn the_hotkey_opens_the_input_and_enter_commits_a_reprobe() {
        let mut overview = Overview::unloaded();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        assert!(matches!(overview.handle_key(key(KeyCode::Char('s'))), OverviewKey::Handled));
        assert!(overview.is_editing());

        for ch in ['/', 't', 'm', 'p'] {
            assert!(matches!(overview.handle_key(key(KeyCode::Char(ch))), OverviewKey::Handled));
        }
        assert!(matches!(overview.handle_key(key(KeyCode::Enter)), OverviewKey::Reprobbed(path) if path.as_path() == Path::new("/tmp")));
        assert!(!overview.is_editing(), "the commit closes the input");
    }

    #[test]
    fn esc_cancels_the_input_without_reprobing() {
        let mut overview = Overview::unloaded();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        overview.handle_key(key(KeyCode::Char('s')));
        overview.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(overview.handle_key(key(KeyCode::Esc)), OverviewKey::Handled));
        assert!(!overview.is_editing());
    }

    #[test]
    fn an_alt_digit_while_editing_is_unhandled_so_the_jump_escape_hatch_fires() {
        // The input must not trap the user: `⌥<digit>` is the escape hatch that jumps tabs while
        // live in edit mode (cloudy-tui: `⌥<digit>` never suspends). It comes back `Unhandled`, the
        // input stays open, and the draft is neither polluted with the digit nor discarded.
        let mut overview = Overview::unloaded();
        let key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(matches!(overview.handle_key(key), OverviewKey::Handled));
        assert!(matches!(overview.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)), OverviewKey::Handled));

        let alt_1 = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT);
        assert!(matches!(overview.handle_key(alt_1), OverviewKey::Unhandled), "⌥1 is the shell's jump key, not the field's");
        assert!(overview.is_editing(), "the input stays open through an unhandled jump key");

        // The jump neither inserted the digit nor cancelled the draft: committing still yields just `/`.
        assert!(matches!(
            overview.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            OverviewKey::Reprobbed(path) if path.as_path() == Path::new("/")
        ));
    }

    #[test]
    fn the_hotkey_is_inert_when_the_summary_has_rows() {
        let mut overview = Overview {
            source: None,
            parts: Parts::One { zips: 1, unpacked: 0, missing: 0 },
            counts: Counts::absent(),
            environment: Environment::default(),
            editing: None,
        };
        let key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(matches!(overview.handle_key(key), OverviewKey::Unhandled));
        assert!(!overview.is_editing());
    }

    #[test]
    fn the_input_window_keeps_the_caret_in_view() {
        assert_eq!(input_window("", 0, 10), (String::new(), 0));
        assert_eq!(input_window("/tmp/x", 6, 10), ("/tmp/x".to_owned(), 6));
        // A long draft head-truncates to the budget and keeps the end caret visible.
        let (text, caret) = input_window(&"a".repeat(20), 20, 10);
        assert!(cells(&text) <= 10, "{text:?}");
        assert!(caret < 10, "caret stays inside the window: {caret}");
    }

    #[test]
    fn the_source_action_names_the_hotkey() {
        assert_eq!(source_action_cells(), 21);
    }

    #[test]
    fn the_source_action_styles_the_hotkey_in_accent() {
        let palette = Palette::new(crate::tui::theme::Tier::Full);
        let line = source_action(&palette);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].content.as_ref(), "s", "the hotkey letter is its own span");
        assert_eq!(line.spans[1].style.fg, Some(palette.accent), "the hotkey is ACCENT");
        assert_eq!(line.spans[0].style.fg, Some(palette.text_dim));
        assert_eq!(line.spans[2].style.fg, Some(palette.text_dim));
    }
}
