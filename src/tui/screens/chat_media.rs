//! The chat media tab: a run form and the live per-item progress table (`docs/design.md`, TUI
//! screen map).
//!
//! # This is the first screen whose input holds usernames
//!
//! `chat_history.json` is keyed by conversation, and a conversation key IS a friend's username; the
//! run derives an output DIRECTORY name from one. Nothing on this screen may carry either. The table
//! shows a file's own id and its output file NAME — never the path, never the conversation key,
//! never a sender, a title, a message body or a coordinate. The counts line is integers. The alerts
//! are a typed [`RunError`]'s own `Display`, and those name only paths the user passed in.
//!
//! That is a property of what reaches this module, not a rule this module enforces on itself:
//! [`chat_run::PlanRow`] already drops the directory, and [`chat_run::PlanCounts`] is six numbers.
//! Pinned by `no_conversation_key_reaches_the_screen` in `tests/chat_media_screen.rs`, which drives
//! a real run against a synthetic export whose conversation key is a string the test then hunts for
//! in every rendered cell.
//!
//! # How a run is driven
//!
//! Identical to the memories screen and deliberately so: [`ChatMedia::start_run`] spawns a worker
//! running [`chat_run::run`], which is the manifest's **only writer**; this screen holds the other
//! end of the channel, opens its own [`Manifest`] connection when the planned snapshot lands, and
//! re-polls every row's status each tick. Quitting mid-run is safe by design.
//!
//! # Focus
//!
//! The form's caret walks all six rows. The three static rows are focusable-but-inert, so their
//! enter descends into the table (or, with no table yet, starts the run — the promise the empty
//! state's action line makes). `space` cycles the overlay mode and flips the transcode toggle;
//! `enter` mirrors it on both, per the contract's row-interaction grammar.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListState, Paragraph};

use crate::export::chat_fix::{CHAT_DIR, OverlayMode};
use crate::export::chat_run::{self, HistoryOutcome, PlanCounts, PlanRow, PlanSnapshot, RunError, RunEvent, RunInputs, RunOutcome};
use crate::export::env::Environment;
use crate::export::local_fix::VideoOptions;
use crate::export::manifest::{ItemKind, ItemStatus, Manifest};
use crate::tui::alert::RunAlert;
use crate::tui::format::{cells, head_ellipsis, plural, right_pad, truncate_prose};
use crate::tui::screens::overview::GUARANTEED_INTERIOR_ROWS;
use crate::tui::theme::{Palette, glyph};
use crate::tui::widgets::{
    self, CARET_GUTTER, IDENTITY_CELLS, LABEL_GAP, LOCATION_CELLS, PanelStyle, ProgressRow, STATUS_CELLS, action_chip, caret,
    cycle_options, disk_free_value, empty_state, form_label, overall_bar, panel, planning_spinner, progress_header, progress_list,
    static_row, tint_to_edge, tooltip,
};

// ---- layout budgets ----

/// Cells a path value is head-ellipsised to, matching the memories form so the two screens' path
/// rows read the same width.
const PATH_CELLS: usize = 22;
/// The widest form label (`overlay mode`), which sets where the ragged rows' values land.
const WIDEST_FORM_LABEL: usize = 12;
/// The narrowest the output column may be before the panel gives up on the whole table.
const OUTPUT_MIN: usize = 6;

/// Cells the overlay-mode cycle control occupies at its widest — every option's word, a 2-space gap
/// between each, and the two brackets the focused row wraps its selection in.
///
/// Computed from [`OverlayMode::ALL`] rather than written down, because the row's whole point is
/// that a fourth mode would be a compile error somewhere rather than a silently clipped word. A
/// `const fn` loop is what lets this stay a `const`.
const CYCLE_CELLS: usize = cycle_cells();

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

/// The form panel's interior cells at its widest row.
const FORM_INTERIOR: usize = CARET_GUTTER + WIDEST_FORM_LABEL + LABEL_GAP + widest_value();

const fn widest_value() -> usize {
    if CYCLE_CELLS > PATH_CELLS { CYCLE_CELLS } else { PATH_CELLS }
}

/// The table's interior cells when every column is at its narrowest. The location column is
/// shared chrome (decision 76), so the chat screen's floor grows by the same amount as the
/// memories screen's, even though no chat row ever fills one.
const TABLE_INTERIOR_MIN: usize = CARET_GUTTER + IDENTITY_CELLS + 2 + LOCATION_CELLS + 2 + STATUS_CELLS + 2 + OUTPUT_MIN;
/// The table's fixed rows on top of the list: the counts line, the overall bar, the header, and the
/// panel's two borders.
const TABLE_FLOOR_ROWS: u16 = 1 + 2 + 1 + 1 + widgets::BORDER_ROWS;

/// The form's rows, in caret order (the user's pick: the memories form plus the overlay-mode cycle).
///
/// The first three are informational: focus may land on them, but no key does anything there. Enter
/// on one descends into the table, which is the whole reason the caret can rest on them at all. The
/// last three are the interactive rows (contract: Cycle row; Toggle row; Action chip row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormRow {
    Source,
    Output,
    DiskFree,
    Overlay,
    Transcode,
    Start,
}

impl FormRow {
    const ALL: [Self; 6] = [Self::Source, Self::Output, Self::DiskFree, Self::Overlay, Self::Transcode, Self::Start];

    /// Where this row sits in [`Self::ALL`], resolved by IDENTITY rather than by position.
    ///
    /// The disabled-chip tooltip is gated on the start chip holding focus, and writing that as
    /// `ALL.len() - 1` says "the last row" instead — the two agree only while [`Self::Start`] is
    /// last. A `len - 1` index's discriminating growth is APPENDING, which is exactly how a form-row
    /// list grows (this array was built by taking the memories form's five rows and adding one), so
    /// a seventh row after `Start` would silently move the tooltip onto it with nothing red.
    /// `the_tooltip_is_bound_to_the_start_chip_by_identity` pins the binding.
    fn index(self) -> usize {
        Self::ALL.iter().position(|row| *row == self).unwrap_or(0)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Output => "output dir",
            Self::DiskFree => "disk free",
            Self::Overlay => "overlay mode",
            Self::Transcode => "transcode",
            Self::Start => "start run",
        }
    }
}

/// Whether the form's caret sits on this row, whichever pane owns it. The selected row keeps its
/// tint while the form is blurred (contract: blurred panes preserve the last-selected row's
/// `BG_HOVER` tint); only the caret and the bold promotion drop.
fn row_selected(chat: &ChatMedia, index: usize) -> bool {
    chat.form_focus == index
}

/// Whether this row renders as focused: selected AND the form pane owns the caret.
fn row_focused(chat: &ChatMedia, index: usize) -> bool {
    !chat.table.descended && chat.form_focus == index
}

// ---- the screen state ----

/// The chat media tab's state.
#[derive(Debug)]
pub struct ChatMedia {
    source: PathBuf,
    out_root: PathBuf,
    environment: Environment,
    overlay: OverlayMode,
    transcode: bool,
    run: Run,
    receiver: Option<Receiver<RunEvent>>,
    /// Where runs started from the screen keep their manifest. `None` resolves the platform's
    /// per-user data dir; tests set a tempdir so that dir is never touched.
    manifest_dir_override: Option<PathBuf>,
    spinner: usize,
    alert: Option<RunAlert>,
    form_focus: usize,
    table: TablePane,
}

/// One run's lifecycle. `view` is `None` while the worker is still preparing — the plan event fills
/// it, so the table appears exactly when the rows exist.
///
/// The view is boxed where the memories screen's is not: this one additionally carries
/// [`PlanCounts`], which takes `Active` past clippy's `large_enum_variant` threshold against an
/// `Idle` that holds nothing. One allocation, at the moment a plan lands, against ~240 bytes on a
/// long-lived screen.
#[derive(Debug)]
enum Run {
    Idle,
    Active { view: Option<Box<RunView>>, worker: Worker },
}

#[derive(Debug)]
enum Worker {
    Working,
    Finished,
}

/// Everything the table renders and the poll refreshes.
#[derive(Debug)]
struct RunView {
    rows: Vec<PlanRow>,
    counts: PlanCounts,
    /// One status per row, refreshed from the manifest every tick.
    statuses: Vec<ItemStatus>,
    /// This screen's own manifest connection. The worker writes through its own; WAL lets the two
    /// coexist, and every status transition is one autocommit statement, so a poll never sees a
    /// half-written row.
    manifest: Manifest,
    /// While set, each tick pins the selection to the newest row. Any scroll input by the user
    /// clears it; a new run re-enables it.
    follow_tail: bool,
}

/// The table pane's own focus and scroll state.
#[derive(Debug)]
struct TablePane {
    descended: bool,
    list: ListState,
}

impl ChatMedia {
    /// The state before any run: the source the app was pointed at, the output root the run will
    /// write into, what the machine can do, and the two run defaults the run starts at.
    ///
    /// `out_root`, `transcode` and `overlay` arrive RESOLVED — `--out` else the file's `out_dir`
    /// else the source-derived default, the file's `transcode` else on, and the file's
    /// `overlay_mode` else `both` (decision 66, in [`crate::app::RunDefaults::resolve`]) — decided
    /// once at startup and never re-derived here.
    ///
    /// The environment is handed in rather than probed here — `App::start` probes once and hands
    /// the answer to every screen, where a constructor that probed for itself cost a whole walk of
    /// `PATH` per screen. It is also the seam a render test uses to pin the disk-free row without
    /// reaching for the real filesystem.
    #[must_use]
    pub fn with_environment(source: PathBuf, out_root: PathBuf, environment: Environment, transcode: bool, overlay: OverlayMode) -> Self {
        Self {
            source,
            out_root,
            environment,
            overlay,
            transcode,
            run: Run::Idle,
            receiver: None,
            manifest_dir_override: None,
            spinner: 0,
            alert: None,
            form_focus: 0,
            table: TablePane { descended: false, list: ListState::default() },
        }
    }

    /// What the machine could do when this screen was built. Test-only: `App`'s own startup test
    /// reads it to pin that one probe reaches every screen.
    #[cfg(test)]
    pub(crate) const fn environment(&self) -> &Environment {
        &self.environment
    }

    /// The dir this screen reads and the output root it was handed, for the same reason as
    /// [`crate::tui::screens::memories::Memories::run_paths`].
    ///
    /// The second value is the root as handed in, NOT where this leg's files land: chat output goes
    /// under `out_root/`[`CHAT_DIR`], which the form row renders and [`chat_fix`] joins. So
    /// `chat-out` in the report is one level above the chat write root while `memories-out` is the
    /// memories write root, and that asymmetry is deliberate — the key reports the argument that
    /// reached the screen, which is what makes a dropped `--out` observable, and folding the leaf in
    /// would report a path through a render-layer constant instead.
    ///
    /// [`chat_fix`]: crate::export::chat_fix
    pub(crate) fn run_paths(&self) -> (&Path, &Path) {
        (&self.source, &self.out_root)
    }

    /// Swaps in a receiver the caller feeds, exactly the channel [`Self::start_run`] creates — the
    /// seam the render and tick tests drive.
    pub fn with_channel(&mut self, receiver: Receiver<RunEvent>) {
        self.receiver = Some(receiver);
        self.run = Run::Active { view: None, worker: Worker::Working };
    }

    /// Names where runs started from this screen keep their manifest — the seam state tests use so
    /// the platform's per-user data dir is never touched. The app never sets this.
    pub fn set_manifest_dir(&mut self, dir: PathBuf) {
        self.manifest_dir_override = Some(dir);
    }

    /// The run-completion alert, when one is live.
    #[must_use]
    pub const fn alert(&self) -> Option<&RunAlert> {
        self.alert.as_ref()
    }

    /// Whether the table pane owns the caret.
    #[must_use]
    pub const fn descended(&self) -> bool {
        self.table.descended
    }

    /// Which overlay mode the start chip's run would use (decision 44b).
    #[must_use]
    pub const fn overlay_mode(&self) -> OverlayMode {
        self.overlay
    }

    /// Whether the transcode toggle is on.
    #[must_use]
    pub const fn is_transcode_on(&self) -> bool {
        self.transcode
    }

    /// Returns the caret to the form. Called by esc and `←` from inside the screen, and by the app
    /// for `q` and the `⌥<digit>` jumps, which ascend implicitly.
    pub fn ascend(&mut self) {
        self.table.descended = false;
    }

    /// `true` when an alert was live and is now dismissed — the whole job of the `x` key. `x` with
    /// nothing showing is inert.
    pub fn dismiss_alert(&mut self) -> bool {
        self.alert.take().is_some()
    }

    /// Whether the start chip may trigger a run.
    fn start_enabled(&self) -> bool {
        match &self.run {
            Run::Idle => true,
            Run::Active { worker: Worker::Finished, .. } => true,
            Run::Active { worker: Worker::Working, .. } => false,
        }
    }

    /// Starts a run on a worker thread. The worker is the manifest's only writer; this screen polls
    /// through its own connection, so no state is shared across the threads but the file.
    fn start_run(&mut self) {
        let manifest_dir = match &self.manifest_dir_override {
            Some(dir) => Some(dir.clone()),
            None => match crate::export::manifest::manifest_dir() {
                Ok(dir) => Some(dir),
                Err(error) => {
                    self.finish(RunOutcome::Failed(RunError::Manifest(error)));
                    return;
                }
            },
        };
        self.start_run_with(chat_run::run, manifest_dir);
    }

    /// Starts a run whose worker runs `run` instead of the real pipeline — the seam tests use to
    /// drive the worker machinery (the thread, the panic containment, the channel) without the
    /// pipeline or the platform data dir.
    ///
    /// `run` receives the same inputs a real run gets and the channel the screen drains. It must
    /// send [`RunEvent::Finished`] on every path, exactly like [`chat_run::run`] does: a worker that
    /// exits without one leaves the screen to report a panic.
    pub fn start_run_with(&mut self, run: impl Fn(&RunInputs, &Sender<RunEvent>) + Send + 'static, manifest_dir: Option<PathBuf>) {
        // A new run resolves the previous completion alert and forgets the old table.
        self.alert = None;
        self.table.list = ListState::default();
        self.table.descended = false;
        self.run = Run::Active { view: None, worker: Worker::Working };

        let (sender, receiver) = std::sync::mpsc::channel();
        self.receiver = Some(receiver);
        let inputs = RunInputs {
            source: self.source.clone(),
            out_root: self.out_root.clone(),
            manifest_dir,
            // The startup snapshot answers where ffmpeg is — the file's `ffmpeg_path` or the PATH
            // probe (decision 66); the toggle decides whether it is used at all.
            video: VideoOptions { transcode: self.transcode, ffmpeg: self.environment.ffmpeg.clone() },
            overlay: self.overlay,
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

    /// Whether a run's worker is still live — the event loop asks before deciding whether to tick.
    #[must_use]
    pub fn run_in_flight(&self) -> bool {
        matches!(self.run, Run::Active { worker: Worker::Working, .. })
    }

    /// One event-loop tick: advance the spinner, drain the worker's channel, refresh the per-item
    /// statuses.
    ///
    /// The poll is gated on the state the tick STARTED in, not on the state [`Self::pump`] leaves
    /// behind, and the difference is the whole run's last statuses. Every item is committed to the
    /// manifest before the worker sends `Finished` (`chat_run::run` returns from its fix pass
    /// first), so the tick that drains that event is the first one that can read the final rows —
    /// and also the last one this screen ever gets, since [`Self::run_in_flight`] goes false with
    /// it and the loop stops ticking an idle screen. Reading the post-pump state instead skipped
    /// exactly that poll and left everything the run finished in its last frame frozen at
    /// `pending`, beside the completion alert, for good.
    pub fn tick(&mut self) {
        if !matches!(self.run, Run::Active { .. }) {
            return;
        }
        self.spinner = self.spinner.wrapping_add(1);
        let live = self.run_in_flight();
        self.pump();
        if live && matches!(self.run, Run::Active { view: Some(_), .. }) {
            self.poll();
        }
    }

    /// Whether the run still owes the user a verdict — the question a failure the SCREEN discovers
    /// has to ask before raising an alert of its own.
    ///
    /// Deliberately not [`Self::run_in_flight`], which happens to hold the same bytes today: that
    /// one answers "should the event loop keep ticking this screen", and the two only agree while
    /// [`Self::finish`] is the sole thing that both publishes an outcome and retires the worker. A
    /// caller asking about the ALERT SLOT gets its own predicate, so a future state that keeps
    /// ticking after a verdict lands cannot silently reopen the clobber this guards.
    fn outcome_unreported(&self) -> bool {
        matches!(self.run, Run::Active { worker: Worker::Working, .. })
    }

    /// Drains every event the worker has queued, in order.
    fn pump(&mut self) {
        loop {
            let event = match self.receiver.as_ref().map(|receiver| receiver.try_recv()) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Empty)) | None => break,
                // The sender is gone without a Finished event: the worker died abnormally and even
                // its panic arm never ran.
                Some(Err(TryRecvError::Disconnected)) => {
                    self.finish(RunOutcome::Failed(RunError::Panicked));
                    break;
                }
            };
            match event {
                RunEvent::Planned(snapshot) => self.plan_landed(snapshot),
                RunEvent::Finished(outcome) => self.finish(outcome),
            }
        }
    }

    /// The plan event: the manifest is enrolled by now, so this screen can open its own reader
    /// connection and the table can render.
    fn plan_landed(&mut self, snapshot: PlanSnapshot) {
        let manifest = match Manifest::open_in(&snapshot.manifest_dir, &snapshot.export_id) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.finish(RunOutcome::Failed(RunError::Manifest(error)));
                return;
            }
        };
        let rows = snapshot.rows;
        let statuses = vec![ItemStatus::Pending; rows.len()];
        if let Run::Active { view, .. } = &mut self.run {
            *view = Some(Box::new(RunView { rows, counts: snapshot.counts, statuses, manifest, follow_tail: true }));
        }
    }

    /// The final event, or a failure this screen discovered on its own side.
    fn finish(&mut self, outcome: RunOutcome) {
        self.alert = Some(summary(&outcome));
        if let Run::Active { view, worker } = &mut self.run {
            if let Some(view) = view {
                view.follow_tail = false;
            }
            *worker = Worker::Finished;
        }
        // The run is over, so nothing more will come down the channel — and the worker's sender is
        // about to be dropped with its thread. A dead channel must read as "the run is over", not as
        // a panic, so the receiver goes away with the run.
        self.receiver = None;
    }

    /// Re-reads every row's status off the manifest, then keeps the tail pinned while the run is
    /// live and the user has not scrolled up.
    ///
    /// The tail is pinned only while `follow_tail` holds, which [`Self::table_move`] clears on every
    /// move — that, not anything here, is what keeps a scrolled selection where the user put it.
    fn poll(&mut self) {
        let result = {
            let Run::Active { view: Some(view), .. } = &self.run else { return };
            view.manifest
                .items(ItemKind::ChatMedia)
                .map(|items| items.into_iter().map(|item| (item.source_id, item.status)).collect::<HashMap<String, ItemStatus>>())
        };
        match result {
            Ok(by_id) => {
                let Run::Active { view: Some(view), .. } = &mut self.run else { return };
                for (status, row) in view.statuses.iter_mut().zip(&view.rows) {
                    *status = by_id.get(&row.source_id).copied().unwrap_or(ItemStatus::Pending);
                }
                if view.follow_tail && !view.rows.is_empty() {
                    self.table.list.select(Some(view.rows.len() - 1));
                }
            }
            // Mid-run the manifest is the only thing that knows how far the run got, so a read it
            // cannot answer ends the run and says so. On the finishing tick the worker has already
            // published its own verdict into the one alert slot, and this failure must not take it:
            // the manifest's message tells the user to delete the file and redo the export, which
            // over a run that just completed cleanly is destructive advice.
            //
            // Ceiling: that late error is then dropped rather than reported, because there is one
            // alert slot and no log in this crate to put the second fact in. Upgrade path is a
            // second slot (or a status line) that can carry a display fault beside a run outcome.
            Err(error) => {
                if self.outcome_unreported() {
                    self.finish(RunOutcome::Failed(RunError::Manifest(error)));
                }
            }
        }
    }

    /// Handles one key while the chat media tab is active. `true` when the screen consumed it.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.table.descended { self.handle_table_key(key) } else { self.handle_form_key(key) }
    }

    /// The table pane owns the caret: arrows scroll it, esc or `←` ascends, `→` is inert.
    fn handle_table_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                self.table_move(if key.code == KeyCode::Up { -1 } else { 1 });
                true
            }
            KeyCode::Esc | KeyCode::Left if key.modifiers == KeyModifiers::NONE => {
                self.table.descended = false;
                true
            }
            KeyCode::Right if key.modifiers == KeyModifiers::NONE => true,
            _ => false,
        }
    }

    /// Moves the table's selection, wrapping at both ends. Any move stops the tail-follow — a `↓`
    /// that is already at the tail moves nothing and leaves the feed live.
    fn table_move(&mut self, delta: isize) {
        let len = match &self.run {
            Run::Active { view: Some(view), .. } => view.rows.len(),
            _ => 0,
        };
        if len == 0 {
            return;
        }
        let current = self.table.list.selected().unwrap_or(0) as isize;
        let at_tail = self.table.list.selected() == Some(len - 1);
        if at_tail && delta > 0 && matches!(&self.run, Run::Active { view: Some(view), .. } if view.follow_tail) {
            return;
        }
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.table.list.select(Some(next));
        if let Run::Active { view: Some(view), .. } = &mut self.run {
            view.follow_tail = false;
        }
    }

    /// The form pane owns the caret: arrows walk the rows (wrapping), enter acts on the focused row
    /// or descends, space activates the state controls.
    fn handle_form_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                let delta: isize = if key.code == KeyCode::Up { -1 } else { 1 };
                self.form_focus = (self.form_focus as isize + delta).rem_euclid(FormRow::ALL.len() as isize) as usize;
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                match FormRow::ALL[self.form_focus] {
                    FormRow::Source | FormRow::Output | FormRow::DiskFree => {
                        // A static row has no row action, so its enter descends into the table.
                        // With no table yet there is nothing to descend into, and the empty state's
                        // "press ↵ to start" promise is what the key does instead.
                        let has_table = matches!(&self.run, Run::Active { view: Some(_), .. });
                        if has_table {
                            self.table.descended = true;
                        } else if self.start_enabled() {
                            self.start_run();
                        }
                    }
                    // `enter` mirrors `space` on a cycle and on a toggle — neither has a separate
                    // commit step, so there is nothing else for it to mean.
                    FormRow::Overlay => self.overlay = self.overlay.next(),
                    FormRow::Transcode => self.transcode = !self.transcode,
                    FormRow::Start => {
                        if self.start_enabled() {
                            self.start_run();
                        }
                    }
                }
                true
            }
            KeyCode::Char(' ') if key.modifiers == KeyModifiers::NONE => {
                // `space` activates a state control and is deliberately NOT bound on the chip.
                match FormRow::ALL[self.form_focus] {
                    FormRow::Overlay => self.overlay = self.overlay.next(),
                    FormRow::Transcode => self.transcode = !self.transcode,
                    FormRow::Source | FormRow::Output | FormRow::DiskFree | FormRow::Start => {}
                }
                true
            }
            _ => false,
        }
    }
}

// ---- render ----

/// Draws the screen into `area`: the setup form and the progress table.
pub fn render(frame: &mut Frame, palette: &Palette, chat: &mut ChatMedia, area: Rect) {
    let tooltip_row = !chat.start_enabled() && row_focused(chat, FormRow::Start.index());
    let form_height = u16::try_from(FormRow::ALL.len() + usize::from(tooltip_row)).unwrap_or(u16::MAX) + widgets::BORDER_ROWS;

    let form_panel_width = FORM_INTERIOR as u16 + widgets::CHROME_COLUMNS;
    let table_panel_width = TABLE_INTERIOR_MIN as u16 + widgets::CHROME_COLUMNS;

    // The overview's layout ladder: side by side, stacked, then form-only as the last resort for a
    // frame too small for either. The table scrolls, so the stacked arm only needs its floor rows.
    // Both panels hug their content (decision 79): the form is its rows and the progress panel is
    // its table, spinner row or framed empty state, so neither carries a blank tail.
    let side_by_side = usize::from(area.width) >= usize::from(form_panel_width) + usize::from(table_panel_width);
    let stacked =
        !side_by_side && usize::from(area.width) >= usize::from(form_panel_width) && area.height >= form_height + TABLE_FLOOR_ROWS;

    let progress_height = progress_height(chat);

    if side_by_side {
        let [left, right] = Layout::horizontal([Constraint::Length(form_panel_width), Constraint::Fill(1)]).areas(area);
        render_form(frame, palette, chat, widgets::hug(left, form_height));
        render_progress(frame, palette, chat, widgets::hug(right, progress_height));
    } else if stacked {
        let [top, bottom] = Layout::vertical([Constraint::Length(form_height), Constraint::Fill(1)]).areas(area);
        render_form(frame, palette, chat, top);
        render_progress(frame, palette, chat, widgets::hug(bottom, progress_height));
    } else {
        render_form(frame, palette, chat, widgets::hug(area, form_height));
    }
}

/// The progress panel's content height (decision 79: the panel hugs what it holds): the framed
/// empty state, the single planning row, or the counts line + bar + header + one row per planned
/// item. Capped at the body by [`widgets::hug`], after which the list scrolls.
fn progress_height(chat: &ChatMedia) -> u16 {
    let content = match &chat.run {
        Run::Idle | Run::Active { view: None, worker: Worker::Finished } => widgets::EMPTY_STATE_ROWS,
        Run::Active { view: None, worker: Worker::Working } => 1,
        Run::Active { view: Some(view), .. } => 3 + u16::try_from(view.rows.len()).unwrap_or(u16::MAX),
    };
    content.saturating_add(widgets::BORDER_ROWS)
}

fn render_form(frame: &mut Frame, palette: &Palette, chat: &ChatMedia, area: Rect) {
    let block = panel(palette, "setup", PanelStyle { first: true, focused: !chat.table.descended });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole or not at all across the width, exactly like the overview's panels; down the height the
    // rows clip one at a time.
    if usize::from(inner.width) < FORM_INTERIOR {
        return;
    }
    frame.render_widget(Paragraph::new(form_panel(palette, chat, usize::from(inner.width))), inner);
}

/// The form's rows, one `Line` per row plus the disabled-chip tooltip. `width` is the panel's
/// interior width, which the selected rows' tint pads out to.
fn form_panel(palette: &Palette, chat: &ChatMedia, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::with_capacity(FormRow::ALL.len() + 1);
    for (index, row) in FormRow::ALL.into_iter().enumerate() {
        rows.push(form_row(palette, chat, row, index, width));
    }
    if !chat.start_enabled() && row_focused(chat, FormRow::Start.index()) {
        rows.push(tooltip(palette, "a run is already in flight"));
    }
    rows
}

fn form_row(palette: &Palette, chat: &ChatMedia, row: FormRow, index: usize, width: usize) -> Line<'static> {
    let selected = row_selected(chat, index);
    let focused = row_focused(chat, index);
    let caret = caret(palette, focused);

    match row {
        FormRow::Source => {
            let value = match chat.source.to_str().filter(|text| !text.is_empty()) {
                Some(path) => Span::styled(right_pad(&head_ellipsis(path, PATH_CELLS), PATH_CELLS), Style::new().fg(palette.text)),
                None => Span::styled(right_pad("—", PATH_CELLS), Style::new().fg(palette.text_faint)),
            };
            static_row(palette, caret, row.label(), vec![value], selected, width)
        }
        FormRow::Output => {
            // The `chat/` level is where this leg's output actually lands, and naming it here is
            // what stops the row reading as the memories tree (decision 46a).
            let shown = head_ellipsis(&chat.out_root.join(CHAT_DIR).to_string_lossy(), PATH_CELLS);
            static_row(
                palette,
                caret,
                row.label(),
                vec![Span::styled(right_pad(&shown, PATH_CELLS), Style::new().fg(palette.text))],
                selected,
                width,
            )
        }
        FormRow::DiskFree => {
            // The value budget is what the row has left after the caret, the label and the gap.
            let budget = width.saturating_sub(CARET_GUTTER + cells(row.label()) + LABEL_GAP);
            let value = disk_free_value(palette, &chat.environment, budget);
            static_row(palette, caret, row.label(), value, selected, width)
        }
        FormRow::Overlay => {
            let mut spans = vec![caret, form_label(palette, row.label(), focused), Span::raw("  ")];
            let words = OverlayMode::ALL.map(OverlayMode::as_word);
            let position = OverlayMode::ALL.iter().position(|mode| *mode == chat.overlay).unwrap_or(0);
            spans.extend(cycle_options(palette, &words, position, focused));
            let line = Line::from(spans);
            if selected { tint_to_edge(line.style(Style::new().bg(palette.bg_hover)), width, palette) } else { line }
        }
        FormRow::Transcode => {
            let mut spans = vec![caret, form_label(palette, row.label(), focused), Span::raw("  ")];
            spans.extend(palette.toggle(chat.transcode));
            let line = Line::from(spans);
            if selected { tint_to_edge(line.style(Style::new().bg(palette.bg_hover)), width, palette) } else { line }
        }
        FormRow::Start => Line::from(vec![caret, action_chip(palette, row.label(), chat.start_enabled(), focused)]),
    }
}

fn render_progress(frame: &mut Frame, palette: &Palette, chat: &mut ChatMedia, area: Rect) {
    let block = panel(palette, "progress", PanelStyle { first: false, focused: chat.table.descended });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if usize::from(inner.width) < TABLE_INTERIOR_MIN {
        return;
    }

    match &chat.run {
        Run::Idle => empty_state(frame, palette, inner, "no run yet"),
        Run::Active { view: None, worker: Worker::Working } => {
            frame.render_widget(Paragraph::new(planning_spinner(palette, chat.spinner)), inner);
        }
        Run::Active { view: None, worker: Worker::Finished } => empty_state(frame, palette, inner, "no run yet"),
        Run::Active { view: Some(view), .. } => render_table(frame, palette, view, &mut chat.table, inner),
    }
}

/// The live table: the counts line, the overall bar, the header, and the stateful row list.
fn render_table(frame: &mut Frame, palette: &Palette, view: &RunView, table: &mut TablePane, inner: Rect) {
    let [counts_area, bar_area, header_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(inner);

    frame.render_widget(Paragraph::new(counts_line(palette, &view.counts, usize::from(inner.width))), counts_area);

    let done = view.statuses.iter().filter(|&&status| status == ItemStatus::Done).count();
    frame.render_widget(Paragraph::new(overall_bar(palette, done, view.rows.len(), usize::from(inner.width))), bar_area);
    frame.render_widget(Paragraph::new(progress_header(palette)), header_area);

    let rows: Vec<ProgressRow<'_>> = view
        .rows
        .iter()
        .zip(&view.statuses)
        .map(|(row, status)| {
            // The chat leg has no place-name concept (decision 76's field is memories-only): the
            // column renders blank and no tooltip ever grows on this screen.
            ProgressRow { identity: &row.source_id, location: None, output: &row.output_name, status: *status }
        })
        .collect();
    progress_list(frame, palette, &rows, table.descended, &mut table.list, list_area, inner.right());
}

/// What the plan found and will not produce output for, above the table.
///
/// Integers and verbs, nothing else: a per-item detail here would have to name a conversation. Zero
/// counts are hidden (Patterns → Counts and plurals), so a clean plan reads `nothing set aside`
/// rather than four zeroes — and rather than a blank row, which reads as a rendering fault.
///
/// **The lower-bound qualifier leads the line and is not the thing that truncates.** With part of
/// the source unlisted every count is a floor, which `Reconciliation::enroll`'s own doc states, and
/// a number that is quietly wrong is worse than a number the reader never sees. So the correction
/// goes first and the counts take whatever width is left.
///
/// **A line that outgrows the row takes the visible prose cut, never a hard clip.** Whole clauses
/// render up to the cut, the first clause that no longer fits keeps a trailing ellipsis, and the
/// clauses after it drop — a `Line` past the panel edge clips mid-word with no marker, which reads
/// as a rendering fault. `width` is the panel's interior cells.
fn counts_line(palette: &Palette, counts: &PlanCounts, width: usize) -> Line<'static> {
    let mut clauses: Vec<(String, Style)> = Vec::new();
    if counts.partial {
        clauses.push(("some dirs unreadable, counts are lower bounds".to_owned(), Style::new().fg(palette.warning)));
    }
    let dim = Style::new().fg(palette.text_dim);
    for (count, one, many) in [
        (counts.unmatched_overlays, "overlay unmatched", "overlays unmatched"),
        (counts.excluded, "thumbnail dropped", "thumbnails dropped"),
        (counts.deferred, "item deferred", "items deferred"),
        (counts.missing_tokens, "message names a file the export has not got", "messages name files the export has not got"),
    ] {
        if count > 0 {
            clauses.push((format!("{count} {}", plural(count, one, many)), dim));
        }
    }
    // Not a count and not an error, and the two unattributed states are DIFFERENT facts rather than
    // one sentence with a cause bolted on. An export delivered without the chat category and one
    // whose history joined nothing both attribute nothing; only the first is "no chat history", and
    // saying so on the second contradicts the token-gap clause rendered beside it.
    //
    // Both strings name OBSERVABLES and neither names a comparison. "read" is a fact about the
    // load; "nothing attributed" is a fact about the items. A phrasing like "no file matched a
    // message" would be false for a history holding no messages at all — `{}` parses to an empty
    // conversation list, so no comparison ever runs — which is the same defect one level in.
    match counts.history {
        HistoryOutcome::Absent => clauses.push(("no chat history, nothing attributed".to_owned(), dim)),
        HistoryOutcome::JoinedNothing => clauses.push(("chat history read, nothing attributed".to_owned(), dim)),
        HistoryOutcome::Joined => {}
    }
    if clauses.is_empty() {
        return Line::from(Span::styled("nothing set aside", dim));
    }
    fit_clauses(palette, clauses, width)
}

/// The clauses joined by ` · `, whole while they fit, the prose cut on the first clause that no
/// longer does. The qualifier is clause zero, so it is never the clause that cuts — its style
/// (WARNING vs dim) and its position survive every width where the line renders at all.
fn fit_clauses(palette: &Palette, clauses: Vec<(String, Style)>, width: usize) -> Line<'static> {
    let dim = Style::new().fg(palette.text_dim);
    let separator = format!(" {} ", glyph::CLAUSE_SEPARATOR);
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(clauses.len() * 2);
    let mut used = 0;
    for (index, (text, style)) in clauses.into_iter().enumerate() {
        let text_cells = cells(&text);
        let lead = if index == 0 { 0 } else { cells(&separator) };
        if used + lead + text_cells > width {
            let budget = width.saturating_sub(used + lead);
            if budget > 0 {
                if lead > 0 {
                    spans.push(Span::styled(separator, dim));
                }
                spans.push(Span::styled(truncate_prose(&text, budget), style));
            } else if used < width {
                // The surviving prefix ends within two cells of the row edge, so the remaining
                // clauses drop with a bare ellipsis in the spare cells naming the cut. Without
                // it the row reads complete, and a dropped nonzero count reads as a zero count
                // — the quietly-wrong-number class the qualifier clause exists to prevent.
                spans.push(Span::styled(glyph::ELLIPSIS.to_string(), dim));
            } else if let Some(last) = spans.last_mut() {
                // The prefix fills the row exactly, so there is no spare cell: the marker
                // steals the final rendered clause's last cell, keeping the cut visible even
                // on a full row.
                let stolen = truncate_prose(last.content.as_ref(), last.width().saturating_sub(1));
                last.content = if stolen.is_empty() { glyph::ELLIPSIS.to_string().into() } else { stolen.into() };
            }
            return Line::from(spans);
        }
        if lead > 0 {
            spans.push(Span::styled(separator.clone(), dim));
        }
        spans.push(Span::styled(text, style));
        used += lead + text_cells;
    }
    Line::from(spans)
}

/// The footer alert a run outcome raises.
fn summary(outcome: &RunOutcome) -> RunAlert {
    match outcome {
        RunOutcome::Completed(report) => RunAlert::completion(report),
        RunOutcome::Failed(error) => RunAlert::failure(error),
    }
}

/// The form's rows must fit the body a panel is guaranteed at the compact floor, the same invariant
/// the overview's and the memories screen's panels rest on.
const _: () = assert!(FormRow::ALL.len() <= GUARANTEED_INTERIOR_ROWS as usize);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Tier;

    fn palette() -> Palette {
        Palette::new(Tier::Full)
    }

    /// The disabled-chip tooltip is bound to the START chip, not to whichever row happens to be
    /// last.
    ///
    /// `ALL.len() - 1` expressed the second thing while meaning the first, and the two agree only
    /// while `Start` is last — with appending as the natural growth direction for a form-row list.
    /// This asserts the binding by identity, so a row added after `Start` reds here instead of
    /// silently taking the tooltip and leaving the chip with no explanation for being inert.
    #[test]
    fn the_tooltip_is_bound_to_the_start_chip_by_identity() {
        assert_eq!(FormRow::Start.index(), FormRow::ALL.len() - 1, "they agree today, which is why the wrong one reads as correct");
        for (position, row) in FormRow::ALL.into_iter().enumerate() {
            assert_eq!(row.index(), position, "{row:?} must resolve to its own slot");
        }
    }

    #[test]
    fn the_cycle_row_reserves_its_brackets_at_every_width() {
        // The focused row is two cells wider than the blurred one, and the form's interior budget
        // has to hold the WIDER of the two or the brackets clip into the panel's padding at exactly
        // the moment they appear. Measured from the rendered spans rather than re-derived, since a
        // derivation of the constant would only agree with itself.
        let words = OverlayMode::ALL.map(OverlayMode::as_word);
        let position = OverlayMode::ALL.iter().position(|mode| *mode == OverlayMode::Both).unwrap_or(0);
        let blurred: usize = cycle_options(&palette(), &words, position, false).iter().map(Span::width).sum();
        let focused: usize = cycle_options(&palette(), &words, position, true).iter().map(Span::width).sum();
        assert_eq!(focused, blurred + 2, "the brackets are the focus cue and cost two cells");
        assert_eq!(focused, CYCLE_CELLS);
        assert!(FORM_INTERIOR >= CARET_GUTTER + WIDEST_FORM_LABEL + LABEL_GAP + focused, "the focused cycle row must fit the interior");
    }

    #[test]
    fn every_overlay_mode_is_reachable_by_cycling_and_the_walk_wraps() {
        // A `space` press lands on the next mode; three presses come home. Both halves matter: a
        // `next` that skipped one would leave a mode the screen cannot select at all, and one that
        // did not wrap would strand the user on the last option.
        let mut seen = vec![OverlayMode::default()];
        for _ in 0..OverlayMode::ALL.len() - 1 {
            seen.push(seen.last().copied().unwrap().next());
        }
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), OverlayMode::ALL.len(), "cycling must reach every mode");
        assert_eq!(OverlayMode::default().next().next().next(), OverlayMode::default(), "the walk wraps");
    }

    #[test]
    fn the_widest_form_label_really_is_the_widest() {
        // The constant decides where every ragged value lands, and a label growing past it would
        // push one row's value into the panel's padding while every other row stayed put.
        for row in FormRow::ALL {
            assert!(cells(row.label()) <= WIDEST_FORM_LABEL, "{} is wider than the budget", row.label());
        }
        assert!(FormRow::ALL.into_iter().any(|row| cells(row.label()) == WIDEST_FORM_LABEL), "the budget must be some label's width");
    }

    fn rendered(counts: &PlanCounts) -> String {
        // A width past any real panel: these tests pin the clauses' copy, and the fit is pinned
        // at the screen level (`tests/chat_media_screen.rs`).
        counts_line(&palette(), counts, usize::MAX).spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn a_clean_plan_says_so_rather_than_rendering_an_empty_row() {
        let counts = PlanCounts { history: HistoryOutcome::Joined, ..PlanCounts::default() };
        assert_eq!(rendered(&counts), "nothing set aside");
    }

    /// The row must never claim a cause its own observation cannot support.
    ///
    /// A history that was READ and matched nothing, and one that was never delivered, both end with
    /// no file attributed — and they are different facts. Rendering the second sentence on the first
    /// state is false on its own, and beside a token-gap count it is self-contradictory: the run
    /// says N messages named files it has not got and then says there is no chat history.
    ///
    /// Reachable without contrivance, which is why this is a state and not a hypothetical: a
    /// partially-extracted export whose `json/` part is unpacked and whose `chat_media` part is not
    /// sends every token to the gap list and joins none of them. Driven end to end by
    /// `an_export_whose_tokens_all_miss_does_not_claim_the_history_is_absent`.
    ///
    /// The copy additionally has to survive the OTHER sub-state of `JoinedNothing` — a history
    /// holding no messages at all — so it names the load and the outcome and never a comparison.
    /// `a_history_with_no_messages_is_not_described_as_an_unmatched_one` drives that one for real.
    #[test]
    fn a_history_that_matched_nothing_is_not_reported_as_an_absent_history() {
        let joined_nothing = PlanCounts { missing_tokens: 2, history: HistoryOutcome::JoinedNothing, ..PlanCounts::default() };
        let text = rendered(&joined_nothing);
        assert_eq!(text, "2 messages name files the export has not got · chat history read, nothing attributed");
        assert!(!text.contains("no chat history"), "the row would deny the clause beside it: {text}");
        assert!(!text.contains("matched"), "a history with no messages ran no comparison to report: {text}");

        // The absent case keeps its own words, so the two states stay distinguishable on the row.
        assert_eq!(rendered(&PlanCounts::default()), "no chat history, nothing attributed");
    }

    #[test]
    fn an_unreadable_dir_puts_the_lower_bound_qualifier_before_every_count() {
        // The qualifier is what a narrow panel must keep, so it leads. A count rendered without it
        // is a number that is quietly wrong, which is the failure this line exists to prevent.
        let counts =
            PlanCounts { unmatched_overlays: 224, excluded: 44, partial: true, history: HistoryOutcome::Joined, ..PlanCounts::default() };
        let text: String = counts_line(&palette(), &counts, usize::MAX).spans.iter().map(|span| span.content.as_ref()).collect();
        const QUALIFIER: &str = "some dirs unreadable, counts are lower bounds";
        assert!(text.starts_with(QUALIFIER), "{text}");
        // Exactly once: `starts_with` alone cannot see a second copy appended after the counts, and
        // a row that says the same correction twice reads as two different conditions.
        assert_eq!(text.matches(QUALIFIER).count(), 1, "{text}");
        assert_eq!(text, format!("{QUALIFIER} · 224 overlays unmatched · 44 thumbnails dropped"));
    }

    #[test]
    fn a_zero_count_is_hidden_and_a_count_of_one_is_singular() {
        let counts = PlanCounts { unmatched_overlays: 1, deferred: 0, history: HistoryOutcome::Joined, ..PlanCounts::default() };
        let text: String = counts_line(&palette(), &counts, usize::MAX).spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, "1 overlay unmatched");
    }

    #[test]
    fn an_export_with_no_chat_history_says_nothing_was_attributed() {
        let counts = PlanCounts::default();
        let text: String = counts_line(&palette(), &counts, usize::MAX).spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, "no chat history, nothing attributed");
    }
}
