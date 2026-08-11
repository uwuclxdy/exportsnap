//! The memories tab: a run form and the live per-item progress table (`docs/design.md`, TUI
//! screen map).
//!
//! **Metadata only, exactly like the overview.** The table shows a memory's uuid, its manifest
//! status and its output file name. No coordinate, no message text, no username ever reaches this
//! module — the run composition in `export::memories_run` is what feeds it, and the poll reads
//! statuses off the manifest, which holds no user content.
//!
//! # How a run is driven
//!
//! [`Memories::start_run`] spawns a worker thread running `export::memories_run::run`, which is
//! the manifest's **only writer**. This screen holds the other end of the channel and opens its
//! own [`Manifest`] connection when the planned snapshot lands; each event-loop tick drains the
//! channel and re-polls every row's status (sqlite WAL lets one reader and one writer coexist
//! across connections). Quitting mid-run is safe by design: every item is committed to the
//! manifest as it finishes, and the next run's resume sweep re-verifies what it finds.
//!
//! # Focus
//!
//! The form's caret walks all five rows (the three static rows are focusable-but-inert, per the
//! contract's Disabled-row grammar). Enter on a static row descends into the table pane, which is
//! read-only but focusable for scrolling; with no table yet (no run planned) there is nothing to
//! descend into, so enter starts the run instead — the promise the empty state's action line
//! makes. esc or `←` ascends, `→` is inert while descended. The selection caret renders only in
//! the focused pane; the selected form row keeps its tint while the form is blurred. While a run
//! is live the table follows its tail until the user scrolls up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListState, Paragraph};

use crate::export::env::Environment;
use crate::export::local_fix::{VideoOptions, default_out_root};
use crate::export::manifest::{ItemKind, ItemStatus, Manifest};
use crate::export::memories_run::{self, PlanRow, PlanSnapshot, RunError, RunEvent, RunInputs, RunOutcome};
use crate::tui::alert::RunAlert;
use crate::tui::format::{cells, head_ellipsis, right_pad};
use crate::tui::screens::overview::GUARANTEED_INTERIOR_ROWS;
use crate::tui::theme::Palette;
use crate::tui::widgets::{
    self, CARET_GUTTER, IDENTITY_CELLS, LABEL_GAP, PanelStyle, ProgressRow, STATUS_CELLS, action_chip, caret, disk_free_value, empty_state,
    overall_bar, panel, planning_spinner, progress_header, progress_list, static_row, tint_to_edge, tooltip,
};

// ---- layout budgets ----

/// Cells a path value is head-ellipsised to. The form's value column is this wide, so the source
/// and the output dir rows hold their width whatever the machine's actual paths are.
const PATH_CELLS: usize = 22;
/// The widest form label, which sets where the ragged rows' widest value lands.
const WIDEST_FORM_LABEL: usize = 10;
/// The narrowest the output column may be before the panel gives up on the whole table.
const OUTPUT_MIN: usize = 6;

/// The form panel's interior cells at the widest ragged row (`output dir` + gap + value).
const FORM_INTERIOR: usize = CARET_GUTTER + WIDEST_FORM_LABEL + LABEL_GAP + PATH_CELLS;
/// The table's interior cells when every column is at its narrowest.
const TABLE_INTERIOR_MIN: usize = CARET_GUTTER + IDENTITY_CELLS + 2 + STATUS_CELLS + 2 + OUTPUT_MIN;
/// The table's fixed rows on top of the list: overall bar, header, and the panel's two borders.
const TABLE_FLOOR_ROWS: u16 = 2 + 1 + 1 + widgets::BORDER_ROWS;

/// The form's rows, in caret order.
///
/// The first three are informational: focus may land on them, but no key does anything there.
/// Enter on one descends into the table, which is the whole reason the caret can rest on them at
/// all. The last two are the interactive rows (contract: Toggle row; Action chip row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormRow {
    Source,
    Output,
    DiskFree,
    Transcode,
    Start,
}

impl FormRow {
    const ALL: [Self; 5] = [Self::Source, Self::Output, Self::DiskFree, Self::Transcode, Self::Start];

    const fn label(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Output => "output dir",
            Self::DiskFree => "disk free",
            Self::Transcode => "transcode",
            Self::Start => "start run",
        }
    }

    /// Where this row sits in [`Self::ALL`], resolved by IDENTITY rather than by position.
    ///
    /// The disabled-chip tooltip is gated on the start chip holding focus, and writing that as
    /// `ALL.len() - 1` says "the last row" instead — the two agree only while [`Self::Start`] is
    /// last. A `len - 1` index's discriminating growth is APPENDING, which is how a form-row list
    /// grows, so a sixth row after `Start` would silently take the tooltip with nothing red.
    /// `the_tooltip_is_bound_to_the_start_chip_by_identity` pins the binding on both screens.
    fn index(self) -> usize {
        Self::ALL.iter().position(|row| *row == self).unwrap_or(0)
    }
}

/// Whether the form's caret sits on this row, whichever pane owns it. The selected row keeps its
/// tint while the form is blurred (contract: blurred panes preserve the last-selected row's
/// `BG_HOVER` tint); only the caret and the bold promotion drop.
fn row_selected(memories: &Memories, index: usize) -> bool {
    memories.form_focus == index
}

/// Whether this row renders as focused: selected AND the form pane owns the caret.
fn row_focused(memories: &Memories, index: usize) -> bool {
    !memories.table.descended && memories.form_focus == index
}

// ---- the screen state ----

/// The memories tab's state.
#[derive(Debug)]
pub struct Memories {
    source: PathBuf,
    out_root: PathBuf,
    environment: Environment,
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

/// One run's lifecycle. `view` is `None` while the worker is still preparing — the plan event
/// fills it, so the table appears exactly when the rows exist.
#[derive(Debug)]
enum Run {
    Idle,
    Active { view: Option<RunView>, worker: Worker },
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
    /// One status per row, refreshed from the manifest every tick.
    statuses: Vec<ItemStatus>,
    /// This screen's own manifest connection. The worker writes through its own; WAL lets the
    /// two coexist, and every status transition is one autocommit statement, so a poll never sees
    /// a half-written row.
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

impl Memories {
    /// The state before any run: the source the app was pointed at, the output root the run will
    /// write into, and what the machine can do. `out_root` resolves to [`default_out_root`] when no
    /// `--out` was passed.
    ///
    /// The environment is handed in rather than probed here — `App::start` probes once and hands
    /// the answer to every screen, where a constructor that probed for itself cost a whole walk of
    /// `PATH` per screen. It is also the seam a render test uses to pin the disk-free row without
    /// reaching for the real filesystem.
    #[must_use]
    pub fn with_environment(source: PathBuf, out_root: Option<PathBuf>, environment: Environment) -> Self {
        let out_root = out_root.unwrap_or_else(|| default_out_root(&source));
        Self {
            source,
            out_root,
            environment,
            transcode: true,
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

    /// The dir this screen reads and the root it writes under — what
    /// [`crate::app::App::source_report`] reports for it. This screen runs against the export and
    /// writes files, so the argument reaching it is worth observing separately from the overview's
    /// copy: they are two fields set by one call, and one can be blanked without the other.
    pub(crate) fn run_paths(&self) -> (&Path, &Path) {
        (&self.source, &self.out_root)
    }

    /// Swaps in a receiver the caller feeds, exactly the channel [`Self::start_run`] creates —
    /// the seam the render and tick tests drive. The planned and finished events then flow
    /// through the real `tick` machinery.
    pub fn with_channel(&mut self, receiver: Receiver<RunEvent>) {
        self.receiver = Some(receiver);
        self.run = Run::Active { view: None, worker: Worker::Working };
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

    /// Whether the table pane owns the caret.
    #[must_use]
    pub const fn descended(&self) -> bool {
        self.table.descended
    }

    /// Whether the transcode toggle is on — the value the start chip's run uses.
    #[must_use]
    pub const fn is_transcode_on(&self) -> bool {
        self.transcode
    }

    /// Returns the caret to the form. Called by esc and `←` from inside the screen, and by the
    /// app for `q` and the `⌥<digit>` jumps, which ascend implicitly.
    pub fn ascend(&mut self) {
        self.table.descended = false;
    }

    /// `true` when an alert was live and is now dismissed — the whole job of the `x` key. `x`
    /// with nothing showing is inert.
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

    /// Starts a run on a worker thread. The worker is the manifest's only writer; this screen
    /// polls through its own connection, so no state is shared across the threads but the file.
    /// Resolves where the manifest lives, then hands the machinery to [`Self::start_run_with`].
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
        self.start_run_with(memories_run::run, manifest_dir);
    }

    /// Starts a run whose worker runs `run` instead of the real pipeline — the seam tests use to
    /// drive the worker machinery (the thread, the panic containment, the channel) without the
    /// pipeline or the platform data dir.
    ///
    /// `run` receives the same inputs a real run gets and the channel the screen drains. It must
    /// send [`RunEvent::Finished`] on every path, exactly like [`memories_run::run`] does: a
    /// worker that exits without one leaves the screen to report a panic.
    pub fn start_run_with(&mut self, run: impl Fn(&RunInputs, &Sender<RunEvent>) + Send + 'static, manifest_dir: Option<PathBuf>) {
        // A new run resolves the previous completion alert and forgets the old table.
        self.alert = None;
        self.table.list = ListState::default();
        self.table.descended = false;
        self.run = Run::Active { view: None, worker: Worker::Working };

        let (sender, receiver) = std::sync::mpsc::channel();
        self.receiver = Some(receiver);
        let inputs = memories_run::RunInputs {
            source: self.source.clone(),
            out_root: self.out_root.clone(),
            manifest_dir,
            // `probe` answers where ffmpeg is; the toggle decides whether it is used at all.
            video: VideoOptions { transcode: self.transcode, ..VideoOptions::probe() },
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
        matches!(self.run, Run::Active { worker: Worker::Working, .. })
    }

    /// One event-loop tick: advance the spinner, drain the worker's channel, refresh the
    /// per-item statuses. Only called while a run is live; an idle screen has nothing to
    /// advance or poll.
    pub fn tick(&mut self) {
        if !matches!(self.run, Run::Active { .. }) {
            return;
        }
        self.spinner = self.spinner.wrapping_add(1);
        self.pump();
        if matches!(self.run, Run::Active { view: Some(_), worker: Worker::Working }) {
            self.poll();
        }
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
            *view = Some(RunView { rows, statuses, manifest, follow_tail: true });
        }
    }

    /// The final event, or a failure this screen discovered on its own side (a manifest it could
    /// not read, a worker that died silently).
    fn finish(&mut self, outcome: RunOutcome) {
        self.alert = Some(summary(&outcome));
        if let Run::Active { view, worker } = &mut self.run {
            if let Some(view) = view {
                view.follow_tail = false;
            }
            *worker = Worker::Finished;
        }
        // The run is over, so nothing more will come down the channel — and the worker's sender
        // is about to be dropped with its thread. A dead channel must read as "the run is over",
        // not as a panic, so the receiver goes away with the run. Without this, the next
        // `try_recv` after a Finished event returns Disconnected and overwrites the true outcome
        // with the panic alert on every run.
        self.receiver = None;
    }

    /// Re-reads every row's status off the manifest, then keeps the tail pinned while the run is
    /// live and the user has not scrolled up.
    fn poll(&mut self) {
        let result = {
            let Run::Active { view: Some(view), .. } = &self.run else { return };
            view.manifest.items(ItemKind::Memory).map(|items| {
                let by_id: HashMap<String, ItemStatus> = items.into_iter().map(|item| (item.source_id, item.status)).collect();
                by_id
            })
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
            Err(error) => self.finish(RunOutcome::Failed(RunError::Manifest(error))),
        }
    }

    /// Handles one key while the memories tab is active. `true` when the screen consumed it.
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

    /// Moves the table's selection, wrapping at both ends. Any move stops the tail-follow — a
    /// `↓` that is already at the tail moves nothing and leaves the feed live.
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
        // A `↓` at the tail while the feed is live does nothing — the next poll moves the tail
        // itself, so there is nothing for the key to do.
        if at_tail && delta > 0 && matches!(&self.run, Run::Active { view: Some(view), .. } if view.follow_tail) {
            return;
        }
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.table.list.select(Some(next));
        if let Run::Active { view: Some(view), .. } = &mut self.run {
            view.follow_tail = false;
        }
    }

    /// The form pane owns the caret: arrows walk the rows (wrapping), enter acts on the focused
    /// row or descends, space flips the toggle.
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
                        // With no table yet (no run planned) there is nothing to descend into,
                        // and the empty state's "press ↵ to start" promise is what the key does
                        // instead.
                        let has_table = matches!(&self.run, Run::Active { view: Some(_), .. });
                        if has_table {
                            self.table.descended = true;
                        } else if self.start_enabled() {
                            self.start_run();
                        }
                    }
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
                // `space` mirrors `enter` on the toggle; it is not bound on chips.
                if FormRow::ALL[self.form_focus] == FormRow::Transcode {
                    self.transcode = !self.transcode;
                }
                true
            }
            _ => false,
        }
    }
}

// ---- render ----

/// Draws the screen into `area`: the setup form and the progress table.
pub fn render(frame: &mut Frame, palette: &Palette, memories: &mut Memories, area: Rect) {
    // The tooltip row appears only while the disabled start chip holds focus, so the form's
    // height is known before its rows are built — the rows need the panel's interior width for
    // the focused-row tint, which only exists once the layout has run.
    let tooltip = !memories.start_enabled() && row_focused(memories, FormRow::Start.index());
    let form_height = u16::try_from(FormRow::ALL.len() + usize::from(tooltip)).unwrap_or(u16::MAX) + widgets::BORDER_ROWS;

    let form_panel_width = FORM_INTERIOR as u16 + widgets::CHROME_COLUMNS;
    let table_panel_width = TABLE_INTERIOR_MIN as u16 + widgets::CHROME_COLUMNS;

    // The overview's layout ladder: side by side, stacked, then form-only as the last resort for
    // a frame too small for either. The table scrolls, so the stacked arm only needs its floor
    // rows rather than the whole table's height.
    let side_by_side = usize::from(area.width) >= usize::from(form_panel_width) + usize::from(table_panel_width);
    let stacked =
        !side_by_side && usize::from(area.width) >= usize::from(form_panel_width) && area.height >= form_height + TABLE_FLOOR_ROWS;

    if side_by_side {
        let [left, right] = Layout::horizontal([Constraint::Length(form_panel_width), Constraint::Fill(1)]).areas(area);
        render_form(frame, palette, memories, left);
        render_progress(frame, palette, memories, right);
    } else if stacked {
        let [top, bottom] = Layout::vertical([Constraint::Length(form_height), Constraint::Fill(1)]).areas(area);
        render_form(frame, palette, memories, top);
        render_progress(frame, palette, memories, bottom);
    } else {
        render_form(frame, palette, memories, area);
    }
}

fn render_form(frame: &mut Frame, palette: &Palette, memories: &Memories, area: Rect) {
    let block = panel(palette, "setup", PanelStyle { first: true, focused: !memories.table.descended });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole or not at all across the width, exactly like the overview's panels; down the height
    // the rows clip one at a time.
    if usize::from(inner.width) < FORM_INTERIOR {
        return;
    }
    let rows = form_panel(palette, memories, usize::from(inner.width));
    frame.render_widget(Paragraph::new(rows), inner);
}

/// The form's rows, one `Line` per row plus the disabled-chip tooltip. `width` is the panel's
/// interior width, which the selected rows' tint pads out to.
fn form_panel(palette: &Palette, memories: &Memories, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::with_capacity(FormRow::ALL.len() + 1);
    for (index, row) in FormRow::ALL.into_iter().enumerate() {
        rows.push(form_row(palette, memories, row, index, width));
    }
    // The disabled chip's reason, only while the chip has focus (contract: Disabled row).
    if !memories.start_enabled() && row_focused(memories, FormRow::Start.index()) {
        rows.push(tooltip(palette, "a run is already in flight"));
    }
    rows
}

fn form_row(palette: &Palette, memories: &Memories, row: FormRow, index: usize, width: usize) -> Line<'static> {
    let selected = row_selected(memories, index);
    let focused = row_focused(memories, index);
    let caret = caret(palette, focused);

    match row {
        FormRow::Source => {
            let value = match memories.source.to_str().filter(|text| !text.is_empty()) {
                Some(path) => Span::styled(right_pad(&head_ellipsis(path, PATH_CELLS), PATH_CELLS), Style::new().fg(palette.text)),
                None => Span::styled(right_pad("—", PATH_CELLS), Style::new().fg(palette.text_faint)),
            };
            static_row(palette, caret, row.label(), vec![value], selected, width)
        }
        FormRow::Output => {
            let shown = head_ellipsis(&memories.out_root.to_string_lossy(), PATH_CELLS);
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
            let value = disk_free_value(palette, &memories.environment, budget);
            static_row(palette, caret, row.label(), value, selected, width)
        }
        FormRow::Transcode => {
            let label = if focused {
                Span::styled(row.label(), Style::new().fg(palette.text).bold())
            } else {
                Span::styled(row.label(), Style::new().fg(palette.text_dim))
            };
            let mut spans = vec![caret, label, Span::raw("  ")];
            spans.extend(palette.toggle(memories.transcode));
            let line = Line::from(spans);
            if selected { tint_to_edge(line.style(Style::new().bg(palette.bg_hover)), width, palette) } else { line }
        }
        FormRow::Start => Line::from(vec![caret, action_chip(palette, row.label(), memories.start_enabled(), focused)]),
    }
}

fn render_progress(frame: &mut Frame, palette: &Palette, memories: &mut Memories, area: Rect) {
    let block = panel(palette, "progress", PanelStyle { first: false, focused: memories.table.descended });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if usize::from(inner.width) < TABLE_INTERIOR_MIN {
        return;
    }

    match &memories.run {
        Run::Idle => empty_state(frame, palette, inner, "no run yet"),
        Run::Active { view: None, worker: Worker::Working } => {
            frame.render_widget(Paragraph::new(planning_spinner(palette, memories.spinner)), inner);
        }
        Run::Active { view: None, worker: Worker::Finished } => {
            empty_state(frame, palette, inner, "no run yet");
        }
        Run::Active { view: Some(view), .. } => {
            render_table(frame, palette, view, &mut memories.table, inner);
        }
    }
}

/// The live table: overall bar, header, and the stateful row list with its scrollbar.
fn render_table(frame: &mut Frame, palette: &Palette, view: &RunView, table: &mut TablePane, inner: Rect) {
    let [bar_area, header_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(inner);

    let done = view.statuses.iter().filter(|&&status| status == ItemStatus::Done).count();
    frame.render_widget(Paragraph::new(overall_bar(palette, done, view.rows.len(), usize::from(inner.width))), bar_area);
    frame.render_widget(Paragraph::new(progress_header(palette)), header_area);

    let rows: Vec<ProgressRow<'_>> = view
        .rows
        .iter()
        .zip(&view.statuses)
        .map(|(row, status)| ProgressRow { identity: &row.source_id, output: &row.output_name, status: *status })
        .collect();
    progress_list(frame, palette, &rows, table.descended, &mut table.list, list_area, inner.right());
}

/// The footer alert a run outcome raises.
fn summary(outcome: &RunOutcome) -> RunAlert {
    match outcome {
        RunOutcome::Completed(report) => RunAlert::completion(report),
        RunOutcome::Failed(error) => RunAlert::failure(error),
    }
}

/// The form's rows must fit the body a panel is guaranteed at the compact floor, the same
/// invariant the overview's panels rest on.
const _: () = assert!(FormRow::ALL.len() <= GUARANTEED_INTERIOR_ROWS as usize);

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_disk_free_row_fits_its_widest_value_into_the_form_budget() {
        // The widest byte figure this build prints is "16384.0 PiB" (u64::MAX), 10 cells — a
        // fixed 9-cell bar would push the trailing percent past the 36-cell interior by one.
        // The bar is elastic, so the row always fits its budget.
        let environment =
            Environment { ffmpeg: None, vlc: None, available_space: Some(10_000 * 1024_u64.pow(5)), total_space: Some(u64::MAX) };
        let budget = FORM_INTERIOR - CARET_GUTTER - cells("disk free") - LABEL_GAP;
        let value = disk_free_value(&Palette::new(crate::tui::theme::Tier::Full), &environment, budget);
        let width: usize = value.iter().map(Span::width).sum();
        assert!(width <= budget, "row is {width} cells, over the {budget}-cell budget");
        assert!(value.last().unwrap().content.as_ref().ends_with('%'), "the percent survives");
    }

    #[test]
    fn a_truncated_output_name_keeps_its_extension() {
        // The output column's cut is head-ellipsis: the leaf — the extension — is the point, so
        // it must survive whatever budget the column has.
        assert_eq!(head_ellipsis("20210115_143005.jpg", 20), "20210115_143005.jpg");
        assert_eq!(head_ellipsis("20210115_143005_2.jpg", 12), "…43005_2.jpg");
        assert!(head_ellipsis("20210115_143005_2.jpg", 12).ends_with(".jpg"));
    }
}
