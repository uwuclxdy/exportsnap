//! The history tab: the conversation picker and the formats pane it descends into
//! (`docs/design.md`, TUI screen map; decisions 58-64).
//!
//! The master-detail split both run screens use, with the roles swapped: the picker is the master
//! (it lists the export's conversations) and the formats pane is the detail (the four format
//! checkboxes and the export chip). Focus descends with `enter`, the master-detail grammar the
//! memories screen uses to enter its table: the checkbox rows are the master list, so `enter`
//! enters the detail and `space` toggles a row. `←` and esc ascend; `→` is inert while descended
//! and stays the tab key on the picker — the shell's arrow walk crosses this screen exactly like
//! the others (`tests/app.rs`'s `right_arrow_walks_forward_through_every_tab` pins that).
//!
//! # Identity on the picker, nowhere else
//!
//! The picker rows carry conversation TITLES where the export wrote one and the conversation key
//! otherwise (decision 64), so a one-to-one thread's key is a friend's username — identity the
//! contract explicitly allows on the picker itself. It goes no further: the selection the run
//! receives is a set of keys the planner already knows, the picker's failure prose is a
//! [`RunError`] `Display` written to be read here (no path derived from a conversation key,
//! decision 49), and the completion alert is built from counts alone. This module renders nothing
//! from a message body and never will.
//!
//! # How a run is driven
//!
//! [`History::start_run`] spawns a worker thread running `export::history_run::run` wrapped in
//! `catch_unwind`, exactly as the memories and chat-media screens do. There is no per-item poll on
//! this leg (decision 63): the counter's denominator comes from the planned snapshot and each
//! [`RunEvent::Written`] advances it. The empty selection is REFUSED rather than run as
//! "everything": the chip is disabled with a tooltip, and the run's own guards hold the same line
//! ([`RunError::NoSelection`], [`RunError::NoFormats`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::line;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Padding, Paragraph, Wrap};

use crate::export::history::conversation_title;
use crate::export::history_run::{self, HistoryFormat, PlanSnapshot, RunError, RunEvent, RunInputs, RunOutcome};
use crate::export::local_fix::default_out_root;
use crate::export::model::ConversationId;
use crate::tui::alert::RunAlert;
use crate::tui::format::{cells, grouped, middle_ellipsis, plural, truncate_prose};
use crate::tui::screens::overview::GUARANTEED_INTERIOR_ROWS;
use crate::tui::theme::Palette;
use crate::tui::widgets::{
    self, CARET_GUTTER, PanelStyle, action_chip, caret, form_label, list_scrollbar, panel, planning_spinner, tint_to_edge,
};

// ---- layout budgets ----

/// The picker panel's interior cells at the widest row: caret, checkbox, gap, and a
/// middle-ellipsised label.
const PICKER_INTERIOR: usize = CARET_GUTTER + 3 + 1 + 24;
/// The picker panel's width — the master side of the split, so it takes a fixed budget and the
/// formats pane takes what is left.
const PICKER_PANEL_WIDTH: u16 = PICKER_INTERIOR as u16 + widgets::CHROME_COLUMNS;
/// The formats pane's interior floor: the caret gutter and the counter "999 of 999
/// conversations".
///
/// The counter is the pane's width driver, and its digits are the information, so nothing in the
/// row is truncatable: a 4-digit total clips at the panel edge, which is the stated ceiling of a
/// fixed-width pane over a head-ellipsised row that would misread the count. The pane's widest
/// row is the disabled chip's tooltip — "  └ pick at least one conversation" is 34 cells — which
/// word-wraps inside the floor rather than clipping, so the reason stays complete at every width
/// where the pane renders.
const FORMATS_INTERIOR: usize = CARET_GUTTER + 24;
/// The formats pane's width.
const FORMATS_PANEL_WIDTH: u16 = FORMATS_INTERIOR as u16 + widgets::CHROME_COLUMNS;
/// The checkbox column: brackets and the mark, one cell each.
const CHECKBOX_CELLS: usize = 3;

/// The formats pane's rows, in caret order: the four format toggles and the export chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatsRow {
    Html,
    Json,
    Text,
    Csv,
    Export,
}

impl FormatsRow {
    const ALL: [Self; 5] = [Self::Html, Self::Json, Self::Text, Self::Csv, Self::Export];

    /// The format this row toggles; `None` on the export chip.
    const fn format(self) -> Option<HistoryFormat> {
        match self {
            Self::Html => Some(HistoryFormat::Html),
            Self::Json => Some(HistoryFormat::Json),
            Self::Text => Some(HistoryFormat::Text),
            Self::Csv => Some(HistoryFormat::Csv),
            Self::Export => None,
        }
    }

    /// Where this row sits in [`Self::ALL`], resolved by IDENTITY rather than by position.
    ///
    /// The disabled-chip tooltip is gated on the export chip holding focus, and writing that as
    /// `ALL.len() - 1` says "the last row" instead — the two agree only while [`Self::Export`] is
    /// last. `the_tooltip_is_bound_to_the_export_chip_by_identity` pins the binding.
    fn index(self) -> usize {
        Self::ALL.iter().position(|row| *row == self).unwrap_or(0)
    }
}

/// Whether this row renders as focused: selected AND the formats pane owns the caret.
fn row_focused(history: &History, index: usize) -> bool {
    history.descended && history.formats_focus == index
}

/// The checkbox column's spans: `[x]` in `ACCENT` when checked, `[ ]` in `TEXT_DIM` brackets
/// either way (cloudy-tui skill: Checkbox row). The mark is data, so it never waits on focus.
fn checkbox(palette: &Palette, checked: bool) -> Vec<Span<'static>> {
    let bracket = Style::new().fg(palette.text_dim);
    if checked {
        vec![Span::styled("[", bracket), Span::styled("x", Style::new().fg(palette.accent)), Span::styled("]", bracket)]
    } else {
        vec![Span::styled("[ ]", bracket)]
    }
}

// ---- the screen state ----

/// The history tab's state.
#[derive(Debug)]
pub struct History {
    source: PathBuf,
    out_root: PathBuf,
    picker: Picker,
    picker_list: ListState,
    /// The selected conversation keys — the run's `conversations` input. Defaults to every key the
    /// picker loads; the chip counts it and refuses to run empty (decision 59).
    selected: BTreeSet<ConversationId>,
    /// The selected document formats, defaulting to all four (decision 58).
    formats: BTreeSet<HistoryFormat>,
    formats_focus: usize,
    /// Whether the formats pane owns the caret.
    descended: bool,
    /// Whether the formats pane renders this frame — `area.width` at or above the side-by-side
    /// floor, as `render` last saw it. Render-derived state the key handlers read back: the
    /// picker-only arm's walk grows to cover the export chip, and enter cannot descend into a
    /// pane that is not drawn (reviewer #3).
    formats_pane_visible: bool,
    run: Run,
    receiver: Option<Receiver<RunEvent>>,
    /// Where runs started from the screen keep their manifest. `None` resolves the platform's
    /// per-user data dir; tests set a tempdir so that dir is never touched.
    manifest_dir_override: Option<PathBuf>,
    spinner: usize,
    alert: Option<RunAlert>,
    /// The counter's live value: done and planned totals. `None` while no plan has landed.
    progress: Option<(usize, usize)>,
}

/// One run's lifecycle.
#[derive(Debug)]
enum Run {
    Idle,
    Active { worker: Worker },
}

#[derive(Debug)]
enum Worker {
    Working,
    Finished,
}

/// What the picker has to show. [`History::with_environment`] reads eagerly, so a live app never
/// holds [`Self::Unloaded`] — it exists so a test can build a screen that reads nothing.
#[derive(Debug)]
enum Picker {
    Unloaded,
    Loaded { rows: Vec<PickerRow> },
    Failed(RunError),
}

/// One conversation the picker lists: its key, the label decision 64 gives it, and whether that
/// label is a real title rather than the key itself — the truncation rule splits on that (titles
/// take a prose cut, keys keep the identity middle cut).
#[derive(Debug)]
struct PickerRow {
    key: ConversationId,
    label: String,
    is_title: bool,
}

impl History {
    /// The state before any load: no filesystem read. [`Self::with_environment`] is what a real
    /// app builds with.
    #[must_use]
    pub fn new() -> Self {
        Self {
            source: PathBuf::new(),
            out_root: PathBuf::new(),
            picker: Picker::Unloaded,
            picker_list: ListState::default(),
            selected: BTreeSet::new(),
            formats: BTreeSet::from(HistoryFormat::ALL),
            formats_focus: 0,
            descended: false,
            // The side-by-side assumption: a screen built at the app's usual geometry, corrected
            // by the first render.
            formats_pane_visible: true,
            run: Run::Idle,
            receiver: None,
            manifest_dir_override: None,
            spinner: 0,
            alert: None,
            progress: None,
        }
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// The state against a real source: the picker loaded eagerly through the run's own
    /// parse/merge path ([`history_run::load_threads`]), so the conversation list exists before
    /// any run starts (decision 61) and the load failure is a state the pane has words for —
    /// the overview's never-fail pattern. `out_root` resolves to [`default_out_root`] when no
    /// `--out` was passed. This screen has no [`crate::export::env::Environment`]: it reads no
    /// machine probe and shows no disk-free row.
    #[must_use]
    pub fn with_environment(source: PathBuf, out_root: Option<PathBuf>) -> Self {
        let out_root = out_root.unwrap_or_else(|| default_out_root(&source));
        let (picker, selected) = match history_run::load_threads(&source) {
            Ok(loaded) => {
                let mut rows: Vec<PickerRow> = loaded
                    .merged
                    .threads
                    .into_iter()
                    .map(|thread| {
                        let title = conversation_title(&thread.records);
                        let is_title = title.is_some();
                        let label = title.map(ToOwned::to_owned).unwrap_or_else(|| thread.id.as_str().to_owned());
                        PickerRow { key: thread.id, label, is_title }
                    })
                    .collect();
                // The picker orders conversations by their label — the title where the export
                // wrote one, else the key — case-insensitively, with the key breaking ties (two
                // threads can share a title). The export's raw order would scatter a titled
                // thread away from the friends it reads as.
                rows.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()).then_with(|| a.key.as_str().cmp(b.key.as_str())));
                let selected = rows.iter().map(|row| row.key.clone()).collect();
                (Picker::Loaded { rows }, selected)
            }
            Err(error) => (Picker::Failed(error), BTreeSet::new()),
        };
        let mut picker_list = ListState::default();
        if let Picker::Loaded { rows } = &picker
            && !rows.is_empty()
        {
            picker_list.select(Some(0));
        }
        Self { source, out_root, picker, picker_list, selected, ..Self::new() }
    }

    /// The dir this screen reads and the root it writes under — what
    /// [`crate::app::App::source_report`] reports for it.
    pub(crate) fn run_paths(&self) -> (&Path, &Path) {
        (&self.source, &self.out_root)
    }

    /// Swaps in a receiver the caller feeds, exactly the channel [`Self::start_run_with`]
    /// creates — the seam the render and tick tests drive.
    pub fn with_channel(&mut self, receiver: Receiver<RunEvent>) {
        self.receiver = Some(receiver);
        self.progress = None;
        self.run = Run::Active { worker: Worker::Working };
    }

    /// Names where runs started from this screen keep their manifest — the seam state tests use
    /// so the platform's per-user data dir is never touched. The app never sets this.
    pub fn set_manifest_dir(&mut self, dir: PathBuf) {
        self.manifest_dir_override = Some(dir);
    }

    /// The run-completion alert, when one is live.
    #[must_use]
    pub const fn alert(&self) -> Option<&RunAlert> {
        self.alert.as_ref()
    }

    /// Whether the formats pane owns the caret.
    #[must_use]
    pub const fn descended(&self) -> bool {
        self.descended
    }

    /// Whether the picker holds rows for its keys to act on — the predicate the shell's hint
    /// derivation answers off (a failed or empty load leaves `space` and `t` with nothing to
    /// toggle, so their hints drop).
    #[must_use]
    pub fn picker_has_rows(&self) -> bool {
        matches!(&self.picker, Picker::Loaded { rows } if !rows.is_empty())
    }

    /// Returns the caret to the picker. Called by esc and `←` from inside the screen, and by the
    /// app for `q` and the `⌥<digit>` jumps, which ascend implicitly.
    pub fn ascend(&mut self) {
        self.descended = false;
    }

    /// `true` when an alert was live and is now dismissed — the whole job of the `x` key. `x`
    /// with nothing showing is inert.
    pub fn dismiss_alert(&mut self) -> bool {
        self.alert.take().is_some()
    }

    /// Whether the export chip may trigger a run.
    fn start_enabled(&self) -> bool {
        !self.selected.is_empty() && !self.formats.is_empty() && matches!(self.run, Run::Idle | Run::Active { worker: Worker::Finished })
    }

    /// Why the export chip is disabled, when it is — the tooltip's reason, one spelling with the
    /// run's own refusal ([`RunError::NoSelection`], [`RunError::NoFormats`]). Selection first:
    /// "pick at least one conversation" is the answer the brief's empty-selection refusal names.
    fn chip_reason(&self) -> Option<&'static str> {
        if self.selected.is_empty() {
            return Some("pick at least one conversation");
        }
        if self.formats.is_empty() {
            return Some("pick at least one format");
        }
        if !self.start_enabled() {
            return Some("a run is already in flight");
        }
        None
    }

    /// Starts a run on a worker thread, mirroring the memories screen: the worker is the
    /// manifest's only writer, this screen holds the other end of the channel, and `catch_unwind`
    /// turns a genuine bug panic into a [`RunError::Panicked`] event so the screen is never left
    /// spinning.
    fn start_run(&mut self) {
        let manifest_dir = match &self.manifest_dir_override {
            // The seam tests name a tempdir so the platform's per-user data dir is never touched.
            Some(dir) => Some(dir.clone()),
            None => match crate::export::manifest::manifest_dir() {
                Ok(dir) => Some(dir),
                Err(error) => {
                    self.finish(RunOutcome::Failed(RunError::Manifest(error)));
                    return;
                }
            },
        };
        self.start_run_with(history_run::run, manifest_dir);
    }

    /// Starts a run whose worker runs `run` instead of the real pipeline — the seam tests use to
    /// drive the worker machinery (the thread, the panic containment, the channel) without the
    /// pipeline or the platform data dir.
    ///
    /// `run` receives the same inputs a real run gets and the channel the screen drains. It must
    /// send [`RunEvent::Finished`] on every path, exactly like [`history_run::run`] does: a worker
    /// that exits without one leaves the screen to report a panic.
    pub fn start_run_with(&mut self, run: impl Fn(&RunInputs, &Sender<RunEvent>) + Send + 'static, manifest_dir: Option<PathBuf>) {
        // A new run resolves the previous completion alert and forgets the counter.
        self.alert = None;
        self.progress = None;
        self.run = Run::Active { worker: Worker::Working };

        let (sender, receiver) = std::sync::mpsc::channel();
        self.receiver = Some(receiver);
        let inputs = history_run::RunInputs {
            source: self.source.clone(),
            out_root: self.out_root.clone(),
            manifest_dir,
            conversations: self.selected.clone(),
            formats: self.formats.clone(),
        };
        std::thread::spawn(move || {
            // `run` sends Finished on every path, errors included; the catch turns a genuine bug
            // panic into one too, so the screen is never left spinning on a worker that died
            // silently.
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&inputs, &sender)));
            if ran.is_err() {
                let _ = sender.send(RunEvent::Finished(RunOutcome::Failed(RunError::Panicked)));
            }
        });
    }

    /// Whether a run's worker is still live — the event loop asks before deciding whether to
    /// tick, so an idle screen blocks on input instead of redrawing every 80 ms.
    #[must_use]
    pub fn run_in_flight(&self) -> bool {
        matches!(self.run, Run::Active { worker: Worker::Working })
    }

    /// One event-loop tick: advance the spinner and drain the worker's channel. Only called while
    /// a run is live; an idle screen has nothing to advance.
    pub fn tick(&mut self) {
        if !matches!(self.run, Run::Active { .. }) {
            return;
        }
        self.spinner = self.spinner.wrapping_add(1);
        self.pump();
    }

    /// Drains every event the worker has queued, in order.
    fn pump(&mut self) {
        loop {
            let event = match self.receiver.as_ref().map(|receiver| receiver.try_recv()) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Empty)) | None => break,
                // The sender is gone without a Finished event: the worker died abnormally and
                // even its panic arm never ran.
                Some(Err(TryRecvError::Disconnected)) => {
                    self.finish(RunOutcome::Failed(RunError::Panicked));
                    break;
                }
            };
            match event {
                RunEvent::Planned(snapshot) => self.plan_landed(snapshot),
                RunEvent::Written => {
                    // The counter advances once per conversation (decision 63), clamped so a seam
                    // that over-reports cannot overtake the planned total.
                    if let Some((done, total)) = &mut self.progress {
                        *done = (*done + 1).min(*total);
                    }
                }
                RunEvent::Finished(outcome) => self.finish(outcome),
            }
        }
    }

    /// The plan event: the total is known, so the counter can render — 0 of N, then advancing.
    fn plan_landed(&mut self, snapshot: PlanSnapshot) {
        self.progress = Some((0, snapshot.conversations));
    }

    /// The final event, or a failure this screen discovered on its own side (a worker that died
    /// silently). The counter is kept as it stands: a run that failed halfway shows how far it
    /// got, and the alert says why.
    fn finish(&mut self, outcome: RunOutcome) {
        self.alert = Some(summary(&outcome));
        if let Run::Active { worker } = &mut self.run {
            *worker = Worker::Finished;
        }
        // The run is over, so nothing more will come down the channel — and the worker's sender
        // is about to be dropped with its thread. A dead channel must read as "the run is over",
        // not as a panic, so the receiver goes away with the run. Without this, the next
        // `try_recv` after a Finished event returns Disconnected and overwrites the true outcome
        // with the panic alert on every run.
        self.receiver = None;
    }

    /// Handles one key while the history tab is active. `true` when the screen consumed it.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // The formats pane's existence is render-derived: a resize below the floor is delivered
        // as an event, and a key can land before the next draw normalizes `descended`. A stale
        // descent must not walk rows that will not render (reviewer #3).
        if self.descended && self.formats_pane_visible { self.handle_formats_key(key) } else { self.handle_picker_key(key) }
    }

    /// The picker owns the caret: arrows walk the rows (wrapping), `space` toggles the focused
    /// row (the brief names it directly), `t` toggles every row, and `enter` descends into the
    /// formats pane — the master-detail grammar, `enter` enters the detail as it does on the
    /// memories screen. `→` stays the shell's tab key here, so it is deliberately NOT consumed.
    fn handle_picker_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                let delta: isize = if key.code == KeyCode::Up { -1 } else { 1 };
                self.picker_move(delta);
                true
            }
            KeyCode::Char(' ') if key.modifiers == KeyModifiers::NONE => {
                self.toggle_focused();
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                // Descend only where a pane exists to descend into. Below the side-by-side floor
                // the formats rows do not render, so enter cannot drop the caret into them
                // (reviewer #3); there it triggers the chip directly when the walk holds it.
                if self.formats_pane_visible {
                    self.descended = true;
                } else if self.walk_on_chip() && self.start_enabled() {
                    self.start_run();
                }
                true
            }
            // `t` toggles every row: the batch affordance the checkbox grammar hangs off the
            // letter (task 80's brief names it directly). The contract's hotkey algorithm
            // reserves `a` for the action menu — which this screen has no menu to auto-scope
            // into — so the batch toggle takes the algorithm's free first char of "toggle
            // all". Case-insensitive like `q` and `x`, so caps lock cannot strand the batch
            // toggle.
            KeyCode::Char('t' | 'T') if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                self.toggle_all();
                true
            }
            _ => false,
        }
    }

    /// Moves the picker's selection, wrapping at both ends.
    fn picker_move(&mut self, delta: isize) {
        let rows = match &self.picker {
            Picker::Loaded { rows } => rows.len(),
            _ => 0,
        };
        // The picker-only arm's walk ends at the export chip — the run's only trigger at that
        // geometry — so it spans the rows plus the chip's walk position. The formats rows are
        // not part of it: they do not render below the side-by-side floor, and nothing
        // unrenderable takes focus (reviewer #3).
        let len = rows + usize::from(!self.formats_pane_visible);
        if len == 0 {
            return;
        }
        let current = self.picker_list.selected().unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.picker_list.select(Some(next));
    }

    /// The number of loaded conversation rows.
    fn picker_rows(&self) -> usize {
        match &self.picker {
            Picker::Loaded { rows } => rows.len(),
            _ => 0,
        }
    }

    /// Whether the picker-only walk holds the export chip's position — the run's trigger, which
    /// joins the walk when the formats pane drops (reviewer #3).
    fn walk_on_chip(&self) -> bool {
        !self.formats_pane_visible && self.picker_list.selected() == Some(self.picker_rows())
    }

    /// Toggles the focused row's conversation in the selection.
    fn toggle_focused(&mut self) {
        let index = self.picker_list.selected().unwrap_or(0);
        let Picker::Loaded { rows } = &self.picker else { return };
        let Some(row) = rows.get(index) else { return };
        if self.selected.contains(&row.key) {
            self.selected.remove(&row.key);
        } else {
            self.selected.insert(row.key.clone());
        }
    }

    /// Toggles every listed conversation in the selection, in one press.
    fn toggle_all(&mut self) {
        let Picker::Loaded { rows } = &self.picker else { return };
        let all_selected = rows.iter().all(|row| self.selected.contains(&row.key));
        for row in rows {
            if all_selected {
                self.selected.remove(&row.key);
            } else {
                self.selected.insert(row.key.clone());
            }
        }
    }

    /// The formats pane owns the caret: arrows walk the rows (wrapping), space and enter toggle
    /// the focused format or trigger the chip, esc or `←` ascends, `→` is inert.
    fn handle_formats_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                let delta: isize = if key.code == KeyCode::Up { -1 } else { 1 };
                self.formats_focus = (self.formats_focus as isize + delta).rem_euclid(FormatsRow::ALL.len() as isize) as usize;
                true
            }
            KeyCode::Char(' ') if key.modifiers == KeyModifiers::NONE => {
                if let Some(format) = FormatsRow::ALL[self.formats_focus].format() {
                    self.toggle_format(format);
                }
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                match FormatsRow::ALL[self.formats_focus].format() {
                    Some(format) => self.toggle_format(format),
                    // The chip: `space` is not bound on chips (it stays reserved for state
                    // controls), so enter is the only key that triggers it.
                    None => {
                        if self.start_enabled() {
                            self.start_run();
                        }
                    }
                }
                true
            }
            KeyCode::Esc | KeyCode::Left if key.modifiers == KeyModifiers::NONE => {
                self.descended = false;
                true
            }
            KeyCode::Right if key.modifiers == KeyModifiers::NONE => true,
            _ => false,
        }
    }

    fn toggle_format(&mut self, format: HistoryFormat) {
        if self.formats.contains(&format) {
            self.formats.remove(&format);
        } else {
            self.formats.insert(format);
        }
    }
}

// ---- render ----

/// Draws the screen into `area`: the conversation picker and the formats pane.
///
/// The ladder has two arms, not three: the formats pane's rows need 26 interior cells, which
/// side-by-side's fixed width always gives them and no stacked width can (a stacked pane would
/// get `width - 34 - 4`, which stays under 26 for every width below the side-by-side floor), so
/// below that floor the screen is the picker alone with the export chip and the counter slot
/// moved into its last two rows — the run's only trigger stays reachable instead of vanishing
/// with the pane it normally lives in (the auditor's chip-survival rule). The picker's own width
/// gate is the floor below which nothing renders at all.
pub fn render(frame: &mut Frame, palette: &Palette, history: &mut History, area: Rect) {
    let side_by_side = usize::from(area.width) >= usize::from(PICKER_PANEL_WIDTH + FORMATS_PANEL_WIDTH);

    // The pane's existence is a render-derived fact the handlers read back: below the floor,
    // `descended` cannot survive — the formats rows it walks do not render there, and a resize
    // out of side-by-side must not leave the caret on rows that are gone (reviewer #3).
    history.formats_pane_visible = side_by_side;
    if !side_by_side {
        history.descended = false;
    }

    if side_by_side {
        let [left, right] = Layout::horizontal([Constraint::Length(PICKER_PANEL_WIDTH), Constraint::Fill(1)]).areas(area);
        render_picker(frame, palette, history, left, false);
        render_formats(frame, palette, history, right);
    } else {
        render_picker(frame, palette, history, area, true);
    }
}

/// Draws the conversation picker into `area`. `chip_in_pane` is the picker-only fallback: the
/// formats pane has dropped, so the pane gives its last rows to the export chip, the disabled
/// chip's wrapped reason, and the counter slot — the run's only trigger stays reachable instead
/// of vanishing with the pane it normally lives in (the auditor's chip-survival rule), and the
/// reason reads in the surviving pane because the tooltip is formats-pane-bound (reviewer #3).
/// The list yields those rows, so nothing overlaps.
fn render_picker(frame: &mut Frame, palette: &Palette, history: &mut History, area: Rect, chip_in_pane: bool) {
    let block = panel(palette, "conversations", PanelStyle { first: true, focused: !history.descended });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole or not at all across the width, exactly like the other screens' panels; down the
    // height the rows clip one at a time.
    if usize::from(inner.width) < PICKER_INTERIOR {
        return;
    }

    // The slot's height follows the disabled chip's wrapped reason, not the walk's focus: the
    // pane's rows must not jump when the caret reaches the chip. The reason's text itself
    // renders only while the walk holds the chip (contract: Disabled row).
    let tooltip = if chip_in_pane && !history.start_enabled() {
        history.chip_reason().map(|reason| wrapped_tooltip(palette, reason, usize::from(inner.width))).unwrap_or_default()
    } else {
        Vec::new()
    };
    let pane_content_height = if chip_in_pane { inner.height.saturating_sub(2 + tooltip.len() as u16) } else { inner.height };
    let content_area = Rect { height: pane_content_height, ..inner };

    match &history.picker {
        Picker::Unloaded => {}
        Picker::Failed(error) => {
            // The failure prose is a `RunError` `Display` written to be read here, wrapped so a
            // long refusal never clips mid-word.
            frame.render_widget(
                Paragraph::new(Line::styled(error.to_string(), Style::new().fg(palette.text_dim))).wrap(Wrap { trim: true }),
                content_area,
            );
        }
        Picker::Loaded { rows } => {
            if rows.is_empty() {
                empty_picker(frame, palette, content_area, "no conversations");
            } else {
                let label_budget = usize::from(inner.width).saturating_sub(CARET_GUTTER + CHECKBOX_CELLS + 1);
                let items: Vec<ListItem<'_>> = rows
                    .iter()
                    .enumerate()
                    .map(|(index, row)| {
                        // The caret and the label promotion belong to the pane that owns the caret;
                        // the tint comes from the List's highlight style, which paints the selected
                        // row's background at any focus (contract: blurred panes keep the tint).
                        let focused = !history.descended && history.picker_list.selected() == Some(index);
                        let mut spans = vec![caret(palette, focused)];
                        spans.extend(checkbox(palette, history.selected.contains(&row.key)));
                        spans.push(Span::raw(" "));
                        // Titles read as prose and take the prose cut; a key-only row's label IS
                        // the identity, so it keeps the identity middle cut that preserves both
                        // ends (finding 11).
                        let label =
                            if row.is_title { truncate_prose(&row.label, label_budget) } else { middle_ellipsis(&row.label, label_budget) };
                        spans.push(form_label(palette, &label, focused));
                        ListItem::new(Line::from(spans))
                    })
                    .collect();
                let list = List::new(items).highlight_style(Style::new().bg(palette.bg_hover)).scroll_padding(3);
                // The picker-only walk can hold the chip's position, one past the list — the
                // `ListState` is the walk's storage, so the render clamps it for the list's sake
                // and restores it after; the chip's caret is the walk's own render.
                let walk = history.picker_list.selected();
                if walk.is_some_and(|position| position >= rows.len()) {
                    history.picker_list.select(None);
                }
                frame.render_stateful_widget(&list, content_area, &mut history.picker_list);
                history.picker_list.select(walk);

                let viewport = usize::from(content_area.height);
                list_scrollbar(frame, palette, rows.len(), history.picker_list.offset(), viewport, inner.right(), content_area);
            }
        }
    }

    // The picker-only fallback's chip slot: the caret and the counter stay the pane's, so the
    // run's trigger reads the same anywhere the screen fits. The chip's caret follows the
    // picker-only walk (the formats-pane caret rule cannot reach this arm), and the disabled
    // chip's reason renders under the chip while the walk holds it — the formats-pane tooltip
    // is pane-bound, so the surviving pane must spell it itself (reviewer #3).
    if chip_in_pane {
        let slot_rows = inner.height.saturating_sub(pane_content_height);
        if slot_rows > 0 {
            let focused = history.walk_on_chip();
            let mut lines = vec![Line::from(vec![
                caret(palette, focused),
                action_chip(palette, &chip_label(history), history.start_enabled(), focused),
            ])];
            if focused && let Some(reason) = history.chip_reason() {
                lines.extend(wrapped_tooltip(palette, reason, usize::from(inner.width)));
            }
            lines.push(progress_slot(palette, history));
            frame.render_widget(Paragraph::new(lines), Rect { y: inner.y + pane_content_height, height: slot_rows, ..inner });
        }
    }
}

fn render_formats(frame: &mut Frame, palette: &Palette, history: &History, area: Rect) {
    let block = panel(palette, "formats", PanelStyle { first: false, focused: history.descended });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if usize::from(inner.width) < FORMATS_INTERIOR {
        return;
    }

    let rows = formats_panel(palette, history, usize::from(inner.width));
    frame.render_widget(Paragraph::new(rows), inner);
}

/// The formats pane's rows: the four toggles, the export chip, the disabled chip's tooltip while
/// it holds focus, and the run's counter slot. `width` is the panel's interior width, which the
/// selected rows' tint pads out to.
fn formats_panel(palette: &Palette, history: &History, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::with_capacity(FormatsRow::ALL.len() + 2);
    for (index, row) in FormatsRow::ALL.into_iter().enumerate() {
        rows.push(formats_row(palette, history, row, index, width));
    }
    // The disabled chip's reason, only while the chip has focus (contract: Disabled row). The
    // priority is the chip's own: the run refuses "nothing selected" before "a run is live".
    // The reason renders wrapped — the interior floor is narrower than the row's widest — so the
    // pane's height reserves the wrapped count before the layout runs.
    if history.formats_focus == FormatsRow::Export.index()
        && history.descended
        && let Some(reason) = history.chip_reason()
    {
        rows.extend(wrapped_tooltip(palette, reason, width));
    }
    rows.push(progress_slot(palette, history));
    rows
}

fn formats_row(palette: &Palette, history: &History, row: FormatsRow, index: usize, width: usize) -> Line<'static> {
    let selected = history.formats_focus == index;
    let focused = row_focused(history, index);
    let mut spans = vec![caret(palette, focused)];
    match row.format() {
        Some(format) => {
            // Caret, then the checkbox, then the row content (skill: Checkbox row — the caret
            // sits to the LEFT of the checkbox).
            spans.extend(checkbox(palette, history.formats.contains(&format)));
            spans.push(Span::raw(" "));
            spans.push(form_label(palette, format.label(), focused));
        }
        // The chip's row carries no tint: the chip is its own block, and the caret marks it.
        None => spans.push(action_chip(palette, &chip_label(history), history.start_enabled(), focused)),
    }
    let line = Line::from(spans);
    if selected && row.format().is_some() { tint_to_edge(line.style(Style::new().bg(palette.bg_hover)), width, palette) } else { line }
}

/// The run's progress in the pane's fixed slot, so the frame never jumps (decision 63's counter):
/// the planning spinner while the worker prepares, "N of M conversations" once the plan lands,
/// nothing at all while idle.
///
/// The counter takes the same leading gutter as every other row of the pane, so it reads as one
/// column with the toggles and the chip rather than as a flush line under them.
fn progress_slot(palette: &Palette, history: &History) -> Line<'static> {
    let mut line = match (&history.run, &history.progress) {
        (Run::Active { worker: Worker::Working }, None) => planning_spinner(palette, history.spinner),
        (_, Some((done, total))) => Line::from(vec![Span::styled(
            format!("{} of {} {}", grouped(*done), grouped(*total), plural(*total, "conversation", "conversations")),
            Style::new().fg(palette.text_dim),
        )]),
        _ => Line::default(),
    };
    line.spans.insert(0, caret(palette, false));
    line
}

/// The footer alert a run outcome raises.
fn summary(outcome: &RunOutcome) -> RunAlert {
    match outcome {
        RunOutcome::Completed(report) => RunAlert::history_completion(report),
        RunOutcome::Failed(error) => RunAlert::failure(error),
    }
}

/// The formats pane's rows must fit the body a panel is guaranteed at the compact floor, the
/// same invariant the other screens' forms rest on. The wrapped tooltip can reach two rows —
/// the floor's 22-cell reason budget holds "pick at least one conversation" as two — so the
/// pane's worst case is the four toggles, the chip, two tooltip rows, and the counter slot,
/// and the row below the pane's bottom clips before any of those do. The picker-only arm's
/// chip slot — chip, wrapped reason, counter — is a subset of that count, so the same bound
/// covers its row claim against the same floor.
const _: () = assert!(FormatsRow::ALL.len() + 3 <= GUARANTEED_INTERIOR_ROWS as usize);

/// The chip's label: `export N` while anything is selected, `export` alone when the selection is
/// empty (finding 6 — zero hides the count rather than reading "export 0", and the chip is
/// disabled at zero anyway). The count takes the grouped form like the counter, and the picker
/// is the only other place a count shows.
fn chip_label(history: &History) -> String {
    let selected = history.selected.len();
    if selected == 0 { "export".to_owned() } else { format!("export {}", grouped(selected)) }
}

/// The tooltip's word-wrapped form. The shared `tooltip` widget renders one line, and the pane's
/// widest row is the disabled chip's reason — "  └ pick at least one conversation" is 34 cells
/// against the 26-cell interior floor at the side-by-side cutoff — so the shared form clips
/// mid-word there. Continuation lines indent to the leader's width; the pane's height reserves
/// the wrapped row count before the layout runs.
fn wrapped_tooltip(palette: &Palette, reason: &str, width: usize) -> Vec<Line<'static>> {
    // The leader's cells: two-space pad, then the corner and its space.
    const LEADER: usize = 4;
    let mut lines = Vec::new();
    for (index, segment) in wrap_words(reason, width.saturating_sub(LEADER)).into_iter().enumerate() {
        let mut spans = vec![Span::styled(if index == 0 { "  " } else { "    " }, Style::new().fg(palette.line))];
        if index == 0 {
            spans.push(Span::styled(format!("{} ", line::BOTTOM_LEFT), Style::new().fg(palette.line)));
        }
        spans.push(Span::styled(segment, Style::new().fg(palette.text_faint)));
        lines.push(Line::from(spans));
    }
    lines
}

/// Word-wraps `text` at word boundaries to `budget` cells, never splitting a word. The three
/// refusal reasons' longest word is "conversation" (12 cells) against the 22-cell floor, so no
/// reason can overflow — and an overflowing word would render complete rather than clip
/// mid-word, which is the reading the wrap exists to protect.
fn wrap_words(text: &str, budget: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split(' ') {
        if current.is_empty() {
            current.push_str(word);
        } else if cells(&format!("{current} {word}")) <= budget {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The house empty state for a picker that loaded nothing, minus the action line: an empty
/// conversation list has no key to offer, so the shared widget's hardcoded "press ↵ to start"
/// would advertise a run that starts nothing. The frame and the hint are the shared shape.
fn empty_picker(frame: &mut Frame, palette: &Palette, inner: Rect, hint: &str) {
    const INSET: u16 = 3;
    const ROWS: u16 = 3;
    let width = u16::try_from(cells(hint).max(16) + 2 * usize::from(INSET) + 2).unwrap_or(u16::MAX);
    let frame_area = inner.centered(Constraint::Length(width), Constraint::Length(ROWS));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(palette.line))
        .padding(Padding::new(INSET, INSET, 0, 0));
    frame.render_widget(Paragraph::new(Line::styled(hint, Style::new().fg(palette.text_dim))).block(block), frame_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The disabled-chip tooltip is bound to the EXPORT chip, not to whichever row happens to be
    /// last. `ALL.len() - 1` expressed the second thing while meaning the first, and the two agree
    /// only while `Export` is last — with appending as the natural growth direction for a
    /// row list. This asserts the binding by identity, so a row added after `Export` reds here
    /// instead of silently taking the tooltip and leaving the chip with no explanation for being
    /// inert.
    #[test]
    fn the_tooltip_is_bound_to_the_export_chip_by_identity() {
        assert_eq!(FormatsRow::Export.index(), FormatsRow::ALL.len() - 1, "they agree today, which is why the wrong one reads as correct");
        for (position, row) in FormatsRow::ALL.into_iter().enumerate() {
            assert_eq!(row.index(), position, "{row:?} must resolve to its own slot");
        }
    }

    #[test]
    fn the_format_rows_pair_with_the_decision_58_formats_in_order() {
        for (row, format) in FormatsRow::ALL[..4].iter().zip(HistoryFormat::ALL) {
            assert_eq!(row.format(), Some(format), "{row:?} must pair with {format:?} in order");
        }
    }
}
