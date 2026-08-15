//! The settings tab (task §6): a five-row form over the config file, and the DANGER toast a
//! failed write raises.
//!
//! # The layers
//!
//! Every row's effective value is derived at render time from the raw layers — flag, file,
//! detection, default (decision 66) — never copied per branch. That is what lets the
//! provenance clause say where a value CAME from: the flag and the file are handed in as the
//! binary's startup values, detection is re-answered per row off the probe capture, and the
//! two defaults are the runs' own (the tier sniff's `Compatible`, the out root's source
//! derivation, transcode on, overlay both).
//!
//! The file layer is the one the form WRITES. Every commit rounds through
//! [`crate::config::write`] and replaces `layers.config` in place on success, so the rows keep
//! reading the file state they wrote (§6's restart-verify: `config::load` returns it next
//! startup). A failed write is a DANGER toast, never silent.
//!
//! # This screen's keys
//!
//! The form is never a pane: `descended` stays false and `alert` stays `None`, so the shell's
//! guards and the footer hint set are the only outside readers. `enter` opens a path row for
//! editing (`✎` + the native cursor) and acts on the three state rows; `esc` or moving to
//! another row exits an edit, discarding the draft (cloudy-tui: Text input — edit off by
//! default, `enter` toggles it, `esc` exits). A flag-pinned row refuses the edit exactly
//! like a pinned state row refuses its press: the ` · flag` clause is the one announcement.

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::config::{self, Config};
use crate::export::chat_fix::OverlayMode;
use crate::export::local_fix::default_out_root;
use crate::tui::format::{cells, head_ellipsis, truncate_prose};
use crate::tui::screens::overview::GUARANTEED_INTERIOR_ROWS;
use crate::tui::theme::{Palette, Tier, glyph};
use crate::tui::widgets::{self, CARET_GUTTER, LABEL_GAP, PanelStyle, caret, cycle_options, form_label, panel, tint_to_edge};

// ---- layout budgets ----

/// The narrow floor of a text row's value slot: the fewest cells it may occupy before the clause
/// gap. The ffmpeg path row is the widest text row — the longest label with the widest clause —
/// and it must fit the 53-cell interior a 57-column terminal leaves (`57 - CHROME_COLUMNS`): the
/// caret's 2, the label's 11, the label gap's 2 and the `   · detection` clause's 14 leave 24.
/// A wider panel raises the slot's ceiling so a longer path shows whole (see [`value_budget`]);
/// this only stops it shrinking below the 57-column case.
const VALUE_CELLS: usize = 24;

/// The overlay cycle's width — every mode's word, the 2-space gaps, and the two brackets a
/// focused row adds — which is the widest value any row renders. Computed from
/// [`OverlayMode::ALL`] rather than written down, because the row's whole point is that a
/// fourth mode would be a compile error somewhere rather than a silently clipped word. A
/// `const fn` loop is what lets this stay a `const`: `array::map` is not const on the pinned
/// toolchain (rustc 1.97.1, E0658), the same reason chat_media's twin restates this loop
/// instead of sharing it.
const fn cycle_cells() -> usize {
    let mut total = 2; // the brackets the focused row adds around its selection
    let mut index = 0;
    while index < OverlayMode::ALL.len() {
        if index > 0 {
            total += 2; // the gap between two options
        }
        total += OverlayMode::ALL[index].as_word().len();
        index += 1;
    }
    total
}

/// Cells between the value slot and the provenance clause. The contract pins cycle options
/// at exactly 2-space gaps and its trailing informational chip (the stepper's `recommended
/// N`) sits at 3+, so the clause takes 3 — one more than an option gap — to keep
/// `   · file` from reading as a fourth option.
const TAG_GAP: usize = 3;

/// The form panel's interior cells at its widest row, derived per row rather than summed from
/// the widest label, value and clause: no row carries all three at once, and the sum
/// overestimates by two — which would blank the form at 57 columns. The ffmpeg path row
/// (widest label with the widest clause) and the focused overlay cycle (widest value) both
/// bind at 53.
const FORM_INTERIOR: usize = widest_row_cells();

/// The narrowest width the polish pass verified: 57 columns minus the panel's chrome. The
/// widest row must fit its interior or the whole-or-not-at-all gate blanks the form at exactly
/// the width that has to stay readable; `the_form_renders_at_57_columns_with_the_clause_gap`
/// pins the rows themselves.
const _: () = assert!(FORM_INTERIOR <= 57 - widgets::CHROME_COLUMNS as usize);

const fn widest_row_cells() -> usize {
    let mut widest = 0;
    let mut index = 0;
    while index < FormRow::ALL.len() {
        let row = FormRow::ALL[index];
        let width = CARET_GUTTER + row.label().len() + LABEL_GAP + row_value_cells(row) + row_clause_cells(row);
        if width > widest {
            widest = width;
        }
        index += 1;
    }
    widest
}

/// The widest value a row renders: the text rows at their ellipsised slot, the cycles at
/// their natural widths — the theme cycle's `[full]  compatible` (18), the compatible tier's
/// `[on]` toggle (4, the wider of the two toggle forms), and the overlay cycle's full 25 with
/// the two brackets a focused row adds (the bracket pair is the focus cue, so the budget
/// reserves it).
const fn row_value_cells(row: FormRow) -> usize {
    match row {
        FormRow::OutputDir | FormRow::Ffmpeg => VALUE_CELLS,
        FormRow::Theme => 18,
        FormRow::Transcode => 4,
        FormRow::Overlay => cycle_cells(),
    }
}

/// The widest clause a row can carry — the gap, the `·` and the word — except the ffmpeg row
/// with nothing at all, which renders no clause rather than inventing one.
const fn row_clause_cells(row: FormRow) -> usize {
    let word = match row {
        FormRow::OutputDir => "default",
        FormRow::Theme | FormRow::Ffmpeg => "detection",
        FormRow::Transcode | FormRow::Overlay => "default",
    };
    TAG_GAP + 2 + word.len()
}

/// The form scrolls with the focus below the height where all five rows fit (see `render`),
/// so this pins only that the shell's compact height shows the whole form without scrolling.
/// Below it the shell's size banner eats one body row (shell.rs), so the honest guarantee is
/// one less than `GUARANTEED_INTERIOR_ROWS`'s own terms — the banner row was never
/// subtracted.
const _: () = assert!((FormRow::ALL.len() as u16) < GUARANTEED_INTERIOR_ROWS);

// ---- the rows ----

/// The form's five rows, in caret order (decision 40: every row something reads — the two
/// path rows write keys the runs consume, the theme row the tier resolver, and the transcode
/// and overlay rows the run defaults; a row nothing reads is not rendered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormRow {
    OutputDir,
    Theme,
    Ffmpeg,
    Transcode,
    Overlay,
}

impl FormRow {
    const ALL: [Self; 5] = [Self::OutputDir, Self::Theme, Self::Ffmpeg, Self::Transcode, Self::Overlay];

    /// The lowercase word the form's key column spells. `const` so the width budget
    /// ([`widest_row_cells`]) derives from the roster itself.
    const fn label(self) -> &'static str {
        match self {
            Self::OutputDir => "output dir",
            Self::Theme => "theme",
            Self::Ffmpeg => "ffmpeg path",
            Self::Transcode => "transcode",
            Self::Overlay => "overlay mode",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|row| *row == self).unwrap_or(0)
    }
}

/// Which raw layer a row's effective value comes from, derived from the layers rather than
/// copied per branch — a flag that wins reports `Flag` whether or not the file also holds a
/// value, because decision 66's precedence IS the derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provenance {
    Flag,
    File,
    Detection,
    Default,
}

impl Provenance {
    /// The lowercase word the row's clause spells. Every row carries the ` · ` clause except
    /// the ffmpeg row with nothing at all, which renders no clause rather than inventing a
    /// word for "nothing".
    fn word(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::File => "file",
            Self::Detection => "detection",
            Self::Default => "default",
        }
    }
}

// ---- the screen ----

/// The settings screen: the five-row form over [`SettingsLayers`], one text edit session at
/// a time, and the DANGER toast a failed write raises.
#[derive(Debug)]
pub struct Settings {
    /// The `--source` the runs read, which the output row's default derives from. Delivered
    /// once by `App::with_source_environment`, like every other source consumer.
    source: PathBuf,
    /// The raw layers. A successful commit replaces `layers.config` in place, so the rows
    /// keep reading the file state they wrote (§6's restart-verify).
    layers: SettingsLayers,
    /// The caret's row among [`FormRow::ALL`].
    form_focus: usize,
    /// The one live text-edit session. `None` on every other row — a second session would
    /// need a second row's keys to reach a distinct field.
    editing: Option<EditSession>,
    /// The DANGER toast, or `None`. One slot: this screen is the only producer and raises one
    /// at a time, so the contract's ≤ 3-stack collapses to the common case.
    toast: Option<Toast>,
}

impl Settings {
    /// A settings screen over already-resolved layers — the binary's `App::start`, or a
    /// test's own scratch state. The source starts empty and lands with
    /// `App::with_source_environment`'s hand-off.
    #[must_use]
    pub fn with_layers(layers: SettingsLayers) -> Self {
        Self { source: PathBuf::new(), layers, form_focus: 0, editing: None, toast: None }
    }

    /// The `--source` the runs read, delivered once by the app's source hand-off. The output
    /// row's default derives from it at render time, so a commit that drops the file key shows
    /// the source-derived default the same frame.
    pub fn set_source(&mut self, source: PathBuf) {
        self.source = source;
    }

    /// Whether a text field is being edited — the app's `q`/`x` suspension and the shell's
    /// hint set both read it.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Whether the toast is live — the app's run loop keeps ticking while it is, so the
    /// DANGER lifetime elapses in real time even with no run in flight.
    #[must_use]
    pub fn toast_live(&self) -> bool {
        self.toast.is_some()
    }

    /// The live toast, for the shell to render last.
    #[must_use]
    pub(crate) fn toast(&self) -> Option<&Toast> {
        self.toast.as_ref()
    }

    /// Ages the toast one tick; the app calls this on its 80 ms loop while it is live.
    pub(crate) fn tick(&mut self) {
        let Some(toast) = self.toast.as_mut() else { return };
        if toast.ticks_left <= 1 {
            self.toast = None;
        } else {
            toast.ticks_left -= 1;
        }
    }

    /// Dismisses the toast, answering whether there was one — the `x` key's job. `x` with no
    /// toast is inert, so the app's guard falls through to the alert dismissal.
    pub(crate) fn dismiss_toast(&mut self) -> bool {
        self.toast.take().is_some()
    }

    /// The screen's own keys, entered while the tab is active. Edit-mode keys go to the
    /// session; otherwise the caret walks the rows (wrapping), `enter` opens a path row for
    /// editing and acts on the three state rows, and `space` mirrors it on the state rows —
    /// the chat-media form's row-interaction grammar.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.editing.is_some() {
            return self.handle_edit_key(key);
        }
        match key.code {
            KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                let delta: isize = if key.code == KeyCode::Up { -1 } else { 1 };
                self.form_focus = (self.form_focus as isize + delta).rem_euclid(FormRow::ALL.len() as isize) as usize;
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                match FormRow::ALL[self.form_focus] {
                    FormRow::OutputDir | FormRow::Ffmpeg => self.begin_edit(FormRow::ALL[self.form_focus]),
                    // `enter` mirrors `space` on a cycle and a toggle — neither has a separate
                    // commit step, so there is nothing else for it to mean.
                    FormRow::Theme | FormRow::Overlay => self.commit_cycle(FormRow::ALL[self.form_focus]),
                    FormRow::Transcode => self.commit_toggle(),
                }
                true
            }
            KeyCode::Char(' ') if key.modifiers == KeyModifiers::NONE => {
                match FormRow::ALL[self.form_focus] {
                    FormRow::OutputDir | FormRow::Ffmpeg => {}
                    FormRow::Theme | FormRow::Overlay => self.commit_cycle(FormRow::ALL[self.form_focus]),
                    FormRow::Transcode => self.commit_toggle(),
                }
                true
            }
            _ => false,
        }
    }

    /// Opens the field for editing, seeding the draft from the FILE layer — never the
    /// effective value: a row overridden by the probe edits what a commit would write, and
    /// committing empty drops the key. The caret lands at the end, so a `Backspace` reaches
    /// the last character of the existing value.
    ///
    /// A flag-pinned row refuses to open at all — the state rows' policy (commit_cycle's
    /// guard): an editor over the flag's value would let a commit gain the file a line the
    /// row never shows, since it keeps reading ` · flag`, which is the silent write the
    /// polish pass closes. The clause names the pin; the press is inert.
    fn begin_edit(&mut self, row: FormRow) {
        debug_assert!(matches!(row, FormRow::OutputDir | FormRow::Ffmpeg), "{row:?}");
        if self.provenance(row) == Some(Provenance::Flag) {
            return;
        }
        let draft = match row {
            FormRow::OutputDir => self.layers.config.out_dir.as_ref().map(|path| path.to_string_lossy().into_owned()),
            FormRow::Ffmpeg => self.layers.config.ffmpeg_path.as_ref().map(|path| path.to_string_lossy().into_owned()),
            FormRow::Theme | FormRow::Transcode | FormRow::Overlay => None,
        }
        .unwrap_or_default();
        let caret = draft.chars().count();
        self.editing = Some(EditSession { row, draft, caret });
    }

    /// One editing key against the live session: printable chars insert at the caret,
    /// `←`/`→`/home/end move it, backspace/delete/ctrl-w delete, `enter` commits the draft,
    /// `esc` and a row move discard it (cloudy-tui: moving to another row exits edit mode,
    /// cancelling like `esc`). A ⌥-jump never reaches this screen — the app routes it before
    /// the tab — so the session survives a jump away and back.
    fn handle_edit_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                let Some(session) = self.editing.take() else { return false };
                self.commit_input(session.row, session.draft);
                true
            }
            KeyCode::Esc if key.modifiers == KeyModifiers::NONE => {
                // Cancel: the draft is discarded; the next `enter` starts from the file layer.
                self.editing = None;
                true
            }
            KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                self.editing = None;
                let delta: isize = if key.code == KeyCode::Up { -1 } else { 1 };
                self.form_focus = (self.form_focus as isize + delta).rem_euclid(FormRow::ALL.len() as isize) as usize;
                true
            }
            _ => {
                let Some(session) = self.editing.as_mut() else { return false };
                edit_session_key(session, key)
            }
        }
    }

    /// Commits a finished path edit through [`crate::config::write`]. An empty draft drops
    /// the key, so the row falls back to its default or detection layer instead of writing a
    /// path that names nothing (config.rs refuses empty paths the same way).
    ///
    /// The write-level half of the flag pin, and the backstop: `begin_edit` refuses to open a
    /// session on a pinned row, so a live session on one cannot exist — the guard still
    /// refuses the write itself, so no route gains the file a line from a pinned row's commit.
    fn commit_input(&mut self, row: FormRow, draft: String) {
        if self.provenance(row) == Some(Provenance::Flag) {
            return;
        }
        let mut config = self.layers.config.clone();
        match row {
            FormRow::OutputDir => config.out_dir = option_path(&draft),
            FormRow::Ffmpeg => config.ffmpeg_path = option_path(&draft),
            FormRow::Theme | FormRow::Transcode | FormRow::Overlay => return,
        }
        self.write(config);
    }

    /// Cycles the focused state row, writing the next value after the EFFECTIVE one. A row the
    /// flag pins writes nothing: the press would write the flag's own successor over the same
    /// constant, churning the file under a value the row never shows — so the guard below
    /// makes it inert, and the row's ` · flag` clause names the pin.
    fn commit_cycle(&mut self, row: FormRow) {
        if self.provenance(row) == Some(Provenance::Flag) {
            return;
        }
        let mut config = self.layers.config.clone();
        match row {
            FormRow::Theme => config.theme = Some(self.effective_tier().next()),
            FormRow::Overlay => config.overlay_mode = Some(self.effective_overlay().next()),
            FormRow::OutputDir | FormRow::Ffmpeg | FormRow::Transcode => return,
        }
        self.write(config);
    }

    /// Flips the transcode default, writing the flip through the file layer.
    fn commit_toggle(&mut self) {
        let mut config = self.layers.config.clone();
        config.transcode = Some(!self.effective_transcode());
        self.write(config);
    }

    /// Writes the file layer through `config::write` — the one write-back path (config.rs
    /// owns the temp-and-rename posture) — and swaps it into the layers only on success, so
    /// the rows keep reading the file state they wrote. A failed write is a DANGER toast,
    /// never silent; a successful one clears any live failure toast, because the DANGER
    /// resolves with the cause that raised it (cloudy-tui: a failed write is a surfaced
    /// error, and the toast is this app's one notification surface).
    fn write(&mut self, config: Config) {
        let message = match &self.layers.config_dir {
            Some(dir) => match config::write(dir, &config) {
                Ok(()) => {
                    self.layers.config = config;
                    self.toast = None;
                    return;
                }
                Err(error) => error.to_string(),
            },
            None => "no config dir to write; run exportsnap with a home dir set".to_owned(),
        };
        self.toast = Some(Toast { message, ticks_left: DANGER_TOAST_TICKS });
    }

    // ---- the effective values ----

    /// The out root the runs would use with these layers: the flag, then the file, then the
    /// `--source`-derived default (decision 66; the default's own resolver runs in `App`).
    #[must_use]
    pub fn effective_out_root(&self) -> PathBuf {
        self.layers.cli_out.clone().or_else(|| self.layers.config.out_dir.clone()).unwrap_or_else(|| default_out_root(&self.source))
    }

    /// The tier the tier resolver would resolve with these layers: flag, file, then the
    /// startup `$COLORTERM` sniff (decision 66).
    #[must_use]
    pub fn effective_tier(&self) -> Tier {
        self.layers.cli_tier.or(self.layers.config.theme).unwrap_or(self.layers.detected_tier)
    }

    /// The ffmpeg the startup would use: the file's path, then the probe's own answer.
    #[must_use]
    pub fn effective_ffmpeg(&self) -> Option<&Path> {
        self.layers.config.ffmpeg_path.as_deref().or(self.layers.detected_ffmpeg.as_deref())
    }

    /// The transcode default: the file's answer, else on (decision 66).
    #[must_use]
    pub fn effective_transcode(&self) -> bool {
        self.layers.config.transcode.unwrap_or(true)
    }

    /// The overlay default: the file's answer, else both (decision 66).
    #[must_use]
    pub fn effective_overlay(&self) -> OverlayMode {
        self.layers.config.overlay_mode.unwrap_or_default()
    }

    /// The row's provenance clause, derived from the raw layers at render time so a commit
    /// shows its effect immediately: the file layer moves and the clause re-derives with it.
    fn provenance(&self, row: FormRow) -> Option<Provenance> {
        match row {
            FormRow::OutputDir => {
                if self.layers.cli_out.is_some() {
                    Some(Provenance::Flag)
                } else if self.layers.config.out_dir.is_some() {
                    Some(Provenance::File)
                } else {
                    Some(Provenance::Default)
                }
            }
            FormRow::Theme => {
                if self.layers.cli_tier.is_some() {
                    Some(Provenance::Flag)
                } else if self.layers.config.theme.is_some() {
                    Some(Provenance::File)
                } else {
                    Some(Provenance::Detection)
                }
            }
            FormRow::Ffmpeg => {
                if self.layers.config.ffmpeg_path.is_some() {
                    Some(Provenance::File)
                } else if self.layers.detected_ffmpeg.is_some() {
                    Some(Provenance::Detection)
                } else {
                    None
                }
            }
            FormRow::Transcode => {
                if self.layers.config.transcode.is_some() {
                    Some(Provenance::File)
                } else {
                    Some(Provenance::Default)
                }
            }
            FormRow::Overlay => {
                if self.layers.config.overlay_mode.is_some() {
                    Some(Provenance::File)
                } else {
                    Some(Provenance::Default)
                }
            }
        }
    }
}

/// The raw layers the settings screen reads and the binary resolves once at startup
/// (decision 66: flag > config > detection > default). Everything here is a startup value,
/// never derived from the effective answer — a derivation after the fact cannot name which
/// layer won, because the resolvers' output is not "which source".
#[derive(Debug, Clone)]
pub struct SettingsLayers {
    /// The dir `config::write` stages the file in, when the platform resolved one.
    pub config_dir: Option<PathBuf>,
    /// `--out=<dir>`. Highest precedence on the output row.
    pub cli_out: Option<PathBuf>,
    /// `--theme=<tier>`. Highest precedence on the theme row.
    pub cli_tier: Option<Tier>,
    /// The config file's keys, as loaded.
    pub config: Config,
    /// What tier detection answers with no flag and no file — the pure `$COLORTERM` sniff.
    pub detected_tier: Tier,
    /// The tool probe's own ffmpeg answer, captured before the config merge in
    /// [`crate::app::App::start_with`] — the detection layer for the ffmpeg row, which would
    /// read the merged value wrong after a commit replaces the file layer.
    pub detected_ffmpeg: Option<PathBuf>,
}

impl SettingsLayers {
    /// The all-defaults layers of a bare `App::new` — no flag, no file, and the detection
    /// answers the caller already resolved. Not `const`: `Config::default()` isn't.
    #[must_use]
    pub fn defaults_for(tier: Tier) -> Self {
        Self { config_dir: None, cli_out: None, cli_tier: None, config: Config::default(), detected_tier: tier, detected_ffmpeg: None }
    }
}

/// One live text edit: the row being edited, the draft, and the caret as a CHAR index into
/// it. Chars rather than bytes so a wide or multi-byte character never splits a grapheme.
#[derive(Debug)]
struct EditSession {
    row: FormRow,
    draft: String,
    caret: usize,
}

/// A path drafted from the user's text, or `None` when the draft is empty — an empty draft
/// drops the key rather than writing a path that names nothing.
fn option_path(draft: &str) -> Option<PathBuf> {
    if draft.is_empty() { None } else { Some(PathBuf::from(draft)) }
}

/// The draft's visible window, in display cells: `start` is the first visible char and
/// `caret_cells` the caret's display-cell offset within the window. The model counts the
/// caret in chars ([`EditSession::caret`]); a wide char (CJK, emoji) is 2 cells, so a char
/// count would place the native cursor mid-char and could push the provenance clause past
/// the slot's edge into the panel padding (cloudy-tui: the model tracks a character column,
/// the render converts to display cells before placing the native cursor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DraftWindow {
    /// Char index the window starts at.
    start: usize,
    /// The caret's display-cell offset within the window.
    caret_cells: usize,
}

/// The window keeps the caret visible, ending one cell after it when the slot allows: the
/// desired start cell is `caret + 1 - visible`, and the window starts at the first char
/// whose start cell reaches it. A wide char straddling the cut is never split — the cut
/// moves before it, showing one more cell of history than the budget names. The window's
/// own end is bounded at the slot by [`draft_window_text`].
fn draft_window(draft: &str, caret: usize, visible_cells: usize) -> DraftWindow {
    // The caret is a CHAR index, so the slice must land on the caret-th char's byte offset,
    // never on the char itself — `&draft[..caret]` would split a wide char mid-glyph.
    let caret_byte = draft.char_indices().nth(caret).map(|(byte, _)| byte).unwrap_or(draft.len());
    let caret_cells = cells(&draft[..caret_byte]);
    let desired = (caret_cells + 1).saturating_sub(visible_cells);
    let mut start = caret;
    let mut start_cells = 0;
    for (index, ch) in draft.chars().take(caret + 1).enumerate() {
        if start_cells >= desired {
            start = index;
            break;
        }
        start_cells += cells(&ch.to_string());
    }
    DraftWindow { start, caret_cells: caret_cells.saturating_sub(start_cells) }
}

/// The window's text: the chars from `window.start` while their cells stay within
/// `visible_cells`, ending before a char that would overflow — a wide char is included
/// whole or not at all.
fn draft_window_text(draft: &str, window: DraftWindow, visible_cells: usize) -> String {
    draft
        .chars()
        .skip(window.start)
        .scan(0usize, |used, ch| {
            let width = cells(&ch.to_string());
            if *used + width > visible_cells {
                None
            } else {
                *used += width;
                Some(ch)
            }
        })
        .collect()
}

/// One editing key against the draft. `false` for a key the field does not own, so the
/// shell's own bindings still see it.
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

/// The DANGER toast a failed write raises — this screen's only toast, so one slot rather
/// than the contract's ≤ 3-stack. The contract's geometry, glass blend and lifetime apply;
/// `x` dismisses it from any tab, and the app's tick loop ages it while it is live.
#[derive(Debug)]
pub(crate) struct Toast {
    message: String,
    ticks_left: u32,
}

/// 6 seconds of DANGER toast at the app's 80 ms tick (cloudy-tui: Toast — DANGER lingers
/// 6 s). Written down rather than computed from `app::TICK` so the two modules need no shared
/// constant for a ratio; the coupling test below reds if either side moves.
const DANGER_TOAST_TICKS: u32 = 75;

// ---- rendering ----

/// Draws the settings form into `area` — the panel the shell hands this tab.
pub fn render(frame: &mut Frame, palette: &Palette, settings: &Settings, area: Rect) {
    let block = panel(palette, "settings", PanelStyle { first: true, focused: true });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole-or-not-at-all, like the run screens' forms: below the widest row's budget the
    // rows would clip into the panel's padding, so the panel stays blank instead of lying.
    if usize::from(inner.width) < FORM_INTERIOR {
        return;
    }
    // The form scrolls with the focus once the rows outgrow the panel's interior, so the
    // caret row stays on screen below the shell's compact height: the view starts at an
    // offset that keeps the focused row last, and only slides once the focus walks past the
    // visible span (cloudy-tui: Text input — the cursor marks the caret, which must sit on a
    // row the panel actually draws).
    let visible_rows = usize::from(inner.height).min(FormRow::ALL.len());
    if visible_rows == 0 {
        return;
    }
    let offset = (settings.form_focus as isize - (visible_rows as isize - 1))
        .max(0)
        .min(FormRow::ALL.len() as isize - visible_rows as isize) as usize;
    let rows = form_panel(palette, settings, usize::from(inner.width)).into_iter().skip(offset).take(visible_rows).collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(rows), inner);

    // The native cursor sits at the caret while a text input is being edited (cloudy-tui:
    // Text input — the terminal's own cursor marks the position). Nothing sets one otherwise.
    if let Some(session) = &settings.editing {
        let budget = value_budget(session.row, settings.provenance(session.row), usize::from(inner.width));
        let window = draft_window(&session.draft, session.caret, budget);
        let value_x = inner.x + (CARET_GUTTER + cells(session.row.label()) + LABEL_GAP + window.caret_cells) as u16;
        // The offset never reaches past the caret row: it clamps at the focus, and while a
        // session is live the session's row IS the focus row — `begin_edit` opens only the
        // focused row and a row move closes the session first — so the subtraction cannot
        // underflow.
        let caret_y = inner.y + (session.row.index() - offset) as u16;
        frame.set_cursor_position(Position::new(value_x, caret_y));
    }
}

fn form_panel(palette: &Palette, settings: &Settings, width: usize) -> Vec<Line<'static>> {
    FormRow::ALL.into_iter().map(|row| form_row(palette, settings, row, width)).collect()
}

fn form_row(palette: &Palette, settings: &Settings, row: FormRow, width: usize) -> Line<'static> {
    let focused = settings.form_focus == row.index();
    match row {
        FormRow::OutputDir => {
            input_row(palette, settings, row, focused, settings.effective_out_root().to_string_lossy().into_owned(), width)
        }
        FormRow::Ffmpeg => input_row(
            palette,
            settings,
            row,
            focused,
            settings.effective_ffmpeg().map(|path| path.to_string_lossy().into_owned()).unwrap_or_else(|| "not found".to_owned()),
            width,
        ),
        FormRow::Theme => {
            let words = Tier::ALL.map(Tier::as_name);
            let selected = Tier::ALL.iter().position(|tier| *tier == settings.effective_tier()).unwrap_or(0);
            state_row(palette, settings, row, focused, cycle_options(palette, &words, selected, focused), width)
        }
        FormRow::Overlay => {
            let words = OverlayMode::ALL.map(OverlayMode::as_word);
            let selected = OverlayMode::ALL.iter().position(|mode| *mode == settings.effective_overlay()).unwrap_or(0);
            state_row(palette, settings, row, focused, cycle_options(palette, &words, selected, focused), width)
        }
        FormRow::Transcode => state_row(palette, settings, row, focused, palette.toggle(settings.effective_transcode()), width),
    }
}

/// The value slot a text row gets at `width`: the interior cells left after the caret, the label,
/// the gap and the row's actual provenance clause, floored at [`VALUE_CELLS`] so the 57-column
/// form keeps its widest row whole. A row with no clause (ffmpeg with nothing detected) gets all
/// of the remaining width for its value.
fn value_budget(row: FormRow, provenance: Option<Provenance>, width: usize) -> usize {
    let clause = provenance.map_or(0, |provenance| TAG_GAP + 2 + provenance.word().len());
    width.saturating_sub(CARET_GUTTER + cells(row.label()) + LABEL_GAP + clause).max(VALUE_CELLS)
}

/// One path row. Blurred, the effective value reads in `ACCENT` — an actionable affordance,
/// the chat-media form's path treatment; focused, `TEXT`. A live edit swaps in the draft
/// window with the `✎` glyph for the caret; an empty draft shows the effective value as an
/// ellipsised, `…`-marked placeholder naming what committing empty would apply, a draft in
/// `TEXT` beside it. The value is ragged like a state row's: it takes its natural width up to
/// [`value_budget`] and the provenance clause trails it at its 3-cell gap, never padded flush
/// against the panel edge.
fn input_row(palette: &Palette, settings: &Settings, row: FormRow, focused: bool, effective: String, width: usize) -> Line<'static> {
    let editing = settings.editing.as_ref().filter(|session| session.row == row);
    let budget = value_budget(row, settings.provenance(row), width);
    // The edit glyph replaces the caret while the field is being edited (cloudy-tui: Text
    // input — `✎` plus the native cursor; the caret returns when the edit exits).
    let caret_span = if editing.is_some() {
        Span::styled(format!("{} ", glyph::EDIT_GLYPH), Style::new().fg(palette.accent).bold())
    } else {
        caret(palette, focused)
    };
    let mut spans = vec![caret_span, form_label(palette, row.label(), focused), Span::raw("  ")];
    let value_span = if let Some(session) = editing {
        let placement = draft_window(&session.draft, session.caret, budget);
        let window = draft_window_text(&session.draft, placement, budget);
        if window.is_empty() {
            // The draft is empty: the placeholder names what committing it would apply — the
            // effective value, or the honest "not found" when nothing was ever detected —
            // ellipsised to the budget exactly like the idle row, and closed by a trailing `…`
            // marking it as placeholder rather than text the user typed. The marker is a
            // content cue, so it survives NO_COLOR where the dim-vs-text contrast dies
            // (design.md: anything that must stay legible without color needs a content cue,
            // not an attribute).
            let shown = head_ellipsis(&effective, budget - 1);
            let marked = format!("{shown}{}", glyph::ELLIPSIS);
            Span::styled(marked, Style::new().fg(palette.text_faint))
        } else {
            Span::styled(window, Style::new().fg(palette.text))
        }
    } else {
        Span::styled(head_ellipsis(&effective, budget), Style::new().fg(if focused { palette.text } else { palette.accent }))
    };
    spans.push(value_span);
    spans.extend(provenance_tag(palette, settings.provenance(row)));
    let line = Line::from(spans);
    if focused { tint_to_edge(line.style(Style::new().bg(palette.bg_hover)), width, palette) } else { line }
}

/// One state row (cycle or toggle): the caret, the promoted label, the value spans and the
/// provenance clause, tinted when selected. The value comes ready-made — the cycle builds it
/// through [`cycle_options`], the toggle through [`Palette::toggle`] — so the two controls
/// share one row grammar.
fn state_row(
    palette: &Palette, settings: &Settings, row: FormRow, focused: bool, value: Vec<Span<'static>>, width: usize,
) -> Line<'static> {
    let mut spans = vec![caret(palette, focused), form_label(palette, row.label(), focused), Span::raw("  ")];
    spans.extend(value);
    spans.extend(provenance_tag(palette, settings.provenance(row)));
    let line = Line::from(spans);
    if focused { tint_to_edge(line.style(Style::new().bg(palette.bg_hover)), width, palette) } else { line }
}

/// The `   · word` clause naming the row's provenance, all `TEXT_FAINT` in one span. The
/// [`TAG_GAP`] run before the `·` keeps the clause from reading as a fourth cycle option —
/// one cell more than the options' own 2-space gaps, at the stepper chip's 3+ spacing. `None`
/// renders nothing — no bare word in a key slot, and no invented word for "nothing" (the
/// separator is [`glyph::CLAUSE_SEPARATOR`], never a literal).
fn provenance_tag(palette: &Palette, provenance: Option<Provenance>) -> Vec<Span<'static>> {
    match provenance {
        Some(provenance) => {
            let clause = format!("{}{} {}", " ".repeat(TAG_GAP), glyph::CLAUSE_SEPARATOR, provenance.word());
            vec![Span::styled(clause, Style::new().fg(palette.text_faint))]
        }
        None => Vec::new(),
    }
}

/// The toast, drawn over the finished frame (cloudy-tui: Toast — renders last). Top-right,
/// 2 cells inset, no border: the `┃` bar in `DANGER`, the title `TEXT + bold`, the reason
/// `TEXT_DIM`, the box's background the 75% `BG_SUNKEN` glass blend over whatever each cell
/// sat on.
pub(crate) fn render_toast(frame: &mut Frame, palette: &Palette, toast: &Toast, area: Rect) {
    const INSET: u16 = 2;
    const WIDTH_CAP: usize = 60;

    // Content-fit width, capped at min(60, terminal width − 4) — the 4 is the inset on each
    // side. The bar and the two padding cells cost 3 of those cells; the longest line gets
    // the rest.
    let width_cap = usize::from(area.width).saturating_sub(2 * usize::from(INSET)).min(WIDTH_CAP);
    if width_cap < 4 {
        return; // not even the bar plus one content cell fit
    }
    let line_cap = width_cap - 3;
    let title = truncate_prose("could not save settings", line_cap);
    let detail = truncate_prose(&toast.message, line_cap);
    let width = u16::try_from(cells(&title).max(cells(&detail)) + 3).unwrap_or(u16::MAX);
    let x = area.width.saturating_sub(INSET + width);

    let buffer = frame.buffer_mut();
    for (row, (text, bold)) in [(&title, true), (&detail, false)].into_iter().enumerate() {
        let y = area.y + INSET + row as u16;
        let content: Vec<char> = text.chars().collect();
        for dx in 0..width {
            let Some(cell) = buffer.cell_mut(Position::new(x + dx, y)) else { continue };
            // The glass is read off the cell BEFORE it is painted: 75% BG_SUNKEN over
            // whatever sits beneath, an unknown or reset under-bg counting as BG (theme.rs's
            // own rule). The under-glyph is cleared in the same breath — a toast covers text.
            cell.set_bg(palette.toast_bg(cell.style().bg));
            let dx = usize::from(dx);
            if dx == 0 {
                cell.set_char(glyph::TOAST_BAR);
                cell.set_fg(palette.danger);
            } else if dx >= 2 && dx < content.len() + 2 {
                cell.set_char(content[dx - 2]);
                cell.set_fg(if bold { palette.text } else { palette.text_dim });
                if bold {
                    cell.set_style(Style::new().bold());
                }
            } else {
                cell.set_char(' ');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_danger_toast_lifetime_is_six_seconds_at_the_app_tick() {
        assert_eq!(crate::app::TICK * DANGER_TOAST_TICKS, Duration::from_secs(6));
    }

    #[test]
    fn the_value_budget_grows_with_the_panel_and_floors_at_value_cells() {
        // At the 57-column interior (53) the ffmpeg row — the widest label with the widest
        // clause — gets exactly the 24-cell floor. A wide panel hands a row its full remaining
        // width, and a row with no clause keeps all of it.
        assert_eq!(value_budget(FormRow::Ffmpeg, Some(Provenance::Detection), 53), VALUE_CELLS);
        assert_eq!(value_budget(FormRow::OutputDir, Some(Provenance::Default), 106), 80);
        assert_eq!(value_budget(FormRow::Ffmpeg, None, 76), 76 - CARET_GUTTER - cells("ffmpeg path") - LABEL_GAP);
    }

    #[test]
    fn the_draft_window_keeps_the_caret_in_view() {
        assert_eq!(draft_window("", 0, 25), DraftWindow { start: 0, caret_cells: 0 });
        assert_eq!(draft_window(&"a".repeat(24), 24, 25), DraftWindow { start: 0, caret_cells: 24 }, "fits the slot: no windowing");
        assert_eq!(draft_window(&"a".repeat(25), 25, 25), DraftWindow { start: 1, caret_cells: 24 }, "caret at the slot's last cell");
        assert_eq!(draft_window(&"a".repeat(29), 29, 25), DraftWindow { start: 5, caret_cells: 24 }, "caret at the text's end");
        // The window end stays one cell after the caret, so the caret column never clips —
        // including a caret exactly one past a draft that fills the slot, which must scroll
        // one cell to reveal itself. `a` is 1 cell, so the char index and the cell count
        // agree and the shape matches the old char-based formula exactly.
        for len in 0..40 {
            let draft = "a".repeat(len);
            for caret in 0..=len {
                let window = draft_window(&draft, caret, 25);
                assert!(window.start <= caret, "start within the draft");
                assert!(window.caret_cells < 25, "caret within the slot");
                let text = draft_window_text(&draft, window, 25);
                assert!(cells(&text) <= 25, "window never exceeds the slot");
                assert_eq!(text.is_empty(), len == 0, "a non-empty draft always shows the caret's char");
            }
        }
    }

    #[test]
    fn a_wide_char_straddling_the_cut_stays_whole() {
        // The caret's slot offset is display cells and the window is bounded in cells too:
        // a char count would place the native cursor mid-char and let a window of wide chars
        // push the provenance clause past the slot's edge (cloudy-tui: the model tracks a
        // character column, the render converts to display cells before placing the cursor).
        assert_eq!(draft_window(&"中".repeat(5), 5, 4), DraftWindow { start: 4, caret_cells: 2 });
        assert_eq!(
            draft_window_text(&"中".repeat(5), DraftWindow { start: 4, caret_cells: 2 }, 4),
            "中",
            "the cut moves before the wide char, not through it"
        );
        assert_eq!(draft_window("中", 1, 25), DraftWindow { start: 0, caret_cells: 2 });
        assert_eq!(
            draft_window("A中B", 3, 2),
            DraftWindow { start: 2, caret_cells: 1 },
            "the cut lands on a char boundary, after the wide char, never through it"
        );
        assert_eq!(draft_window_text("A中B", DraftWindow { start: 2, caret_cells: 1 }, 2), "B");
        // A trailing wide char that cannot fit shows nothing rather than hiding the caret
        // mid-char: the window starts AT the caret, which stays visible at the slot's edge.
        assert_eq!(draft_window("A中", 2, 2), DraftWindow { start: 2, caret_cells: 0 });
        assert_eq!(draft_window_text("A中", DraftWindow { start: 2, caret_cells: 0 }, 2), "");
    }

    #[test]
    fn the_edit_keys_insert_delete_and_move_the_caret() {
        let mut session = EditSession { row: FormRow::OutputDir, draft: String::new(), caret: 0 };
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(edit_session_key(&mut session, key(KeyCode::Char('a'))));
        assert!(edit_session_key(&mut session, key(KeyCode::Char('b'))));
        assert_eq!(session.draft, "ab");
        assert_eq!(session.caret, 2);
        assert!(edit_session_key(&mut session, key(KeyCode::Left)));
        assert!(edit_session_key(&mut session, key(KeyCode::Char('X'))));
        assert_eq!(session.draft, "aXb");
        assert!(edit_session_key(&mut session, key(KeyCode::Backspace)));
        assert_eq!(session.draft, "ab");
        assert!(edit_session_key(&mut session, key(KeyCode::Delete)));
        assert_eq!(session.draft, "a");
        assert!(edit_session_key(&mut session, key(KeyCode::Home)));
        assert!(edit_session_key(&mut session, key(KeyCode::Char('q'))));
        assert_eq!(session.draft, "qa", "the suspended q key types a letter");
        assert!(edit_session_key(&mut session, key(KeyCode::End)));
        assert_eq!(session.caret, 2);
        let ctrl_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert!(!edit_session_key(&mut session, ctrl_v), "a ctrl chord the field does not own");
    }

    #[test]
    fn ctrl_w_kills_the_word_before_the_caret() {
        let mut session = EditSession { row: FormRow::OutputDir, draft: "one two three".to_owned(), caret: 13 };
        let ctrl_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(edit_session_key(&mut session, ctrl_w));
        assert_eq!(session.draft, "one two ", "the word and the space before it, like readline");
        assert_eq!(session.caret, 8);
        assert!(edit_session_key(&mut session, ctrl_w));
        assert_eq!(session.draft, "one ");
        assert_eq!(session.caret, 4);
        assert!(edit_session_key(&mut session, ctrl_w));
        assert_eq!(session.draft, "");
        assert_eq!(session.caret, 0);
    }

    #[test]
    fn a_pinned_text_rows_commit_writes_nothing() {
        // The write-level half of the pin, exercised directly: `begin_edit` refuses to open a
        // session on a flag-pinned row, so `commit_input` can only meet one through a direct
        // call — and must still refuse the write, so no route gains the file a line from a
        // pinned row's commit while the row keeps reading ` · flag`.
        let dir = tempfile::TempDir::new().unwrap();
        let mut settings = Settings::with_layers(SettingsLayers {
            config_dir: Some(dir.path().to_path_buf()),
            cli_out: Some(PathBuf::from("/flag/out")),
            ..SettingsLayers::defaults_for(crate::tui::theme::Tier::Full)
        });

        settings.commit_input(FormRow::OutputDir, "/evil".to_owned());

        assert_eq!(crate::config::load(dir.path()).unwrap().out_dir, None, "the pinned row's commit gains the file nothing");
        assert_eq!(settings.provenance(FormRow::OutputDir), Some(Provenance::Flag), "the clause still names the pin");
    }

    #[test]
    fn a_multi_byte_character_never_splits() {
        // The caret is a char index: a wide char is 2 display cells but one caret step, so
        // backspacing over it removes it whole, and the draft is intact between edits.
        let mut session = EditSession { row: FormRow::OutputDir, draft: "a中b".to_owned(), caret: 3 };
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(edit_session_key(&mut session, key(KeyCode::Backspace)));
        assert_eq!(session.draft, "a中");
        assert_eq!(session.caret, 2);
        assert!(edit_session_key(&mut session, key(KeyCode::Left)));
        assert!(edit_session_key(&mut session, key(KeyCode::Backspace)));
        assert_eq!(session.draft, "中");
        assert_eq!(session.caret, 0);
    }
}
