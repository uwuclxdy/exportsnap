//! The whole memories run in one call: what the memories screen drives.
//!
//! The screen's job is to render what this module reports, so the pipeline here is one function
//! with a progress channel and no framework: discover the export's parts, read its json, walk its
//! memories dirs, join the two, enroll the result in the manifest, plan every output path, run the
//! fix pass, and report the outcome.
//!
//! # What a caller gets, and when
//!
//! [`run`] sends exactly one [`RunEvent::Planned`] — after the manifest is enrolled, before any
//! output is written — and then one [`RunEvent::Finished`] on every path, setup errors included.
//! The manifest this function opens is the run's **only writer**. A screen that wants live
//! per-item progress opens its own [`Manifest`] connection and polls
//! [`Manifest::items`](crate::export::manifest::Manifest::items) each tick; sqlite's WAL mode lets
//! one reader and one writer coexist across connections, and every status transition is a single
//! autocommit statement, so the reader always sees whole rows. The planned event's `manifest_dir`
//! is where the reader opens its own connection — the same file the writer is using.
//!
//! # Errors are typed, never panics
//!
//! Every state the screen has words for — no export part, no `json/`, no memories, an unreadable
//! manifest — is a [`RunError`] variant with a `Display` a footer alert can carry verbatim. The
//! one residual is a genuine bug panicking mid-run; [`run`] is still guaranteed to send
//! [`RunEvent::Finished`] because the worker thread that calls it wraps it in `catch_unwind`
//! (see `src/tui/screens/memories.rs`), which is what "never panics" means at the composition
//! boundary.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::export::local_fix::{self, DEFAULT_MAX_ATTEMPTS, FixReport, Leg, Plan, RecordedOutputs, VideoOptions};
use crate::export::manifest::{ExportId, ItemKind, Manifest, ManifestError};
use crate::export::memories::{self, ScanError, reconcile};
use crate::export::zip::{DiscoverError, discover_parts};
use crate::export::{ExportJson, LoadError};

/// Everything a run needs, gathered by the caller.
#[derive(Debug, Clone)]
pub struct RunInputs {
    /// The dir holding the export's parts (or the parts themselves).
    pub source: PathBuf,
    /// Where the fixed memories land. Decision 33: `--out=<dir>` or
    /// [`crate::export::local_fix::default_out_root`].
    pub out_root: PathBuf,
    /// Where the manifest lives. `None` resolves the platform's per-user data dir; a test passes
    /// its own tempdir so the real data dir is never touched.
    pub manifest_dir: Option<PathBuf>,
    /// What the run may do to a video's pixels.
    pub video: VideoOptions,
}

/// One row of the progress table, decided at plan time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    /// The manifest's identity: the media's uuid.
    pub source_id: String,
    /// The output file's name, e.g. `20210115_143005.jpg`. Not the path: the year and month
    /// directories are implied chrome, and the name is what a table row can hold.
    pub output_name: String,
    /// Which leg fixes it: decides the kind a row reports.
    pub leg: Leg,
}

/// Everything the screen needs once the plan is built.
#[derive(Debug, Clone)]
pub struct PlanSnapshot {
    pub export_id: ExportId,
    /// Where the manifest this run writes lives — where a polling reader opens its own connection.
    pub manifest_dir: PathBuf,
    /// One per paired, fixable memory, in `memories_history.json` order.
    pub rows: Vec<PlanRow>,
}

/// One message from the worker to the screen.
#[derive(Debug)]
pub enum RunEvent {
    /// The plan is built and the manifest is enrolled: the table can render.
    Planned(PlanSnapshot),
    /// The run is over, however it ended.
    Finished(RunOutcome),
}

/// How a run ended.
#[derive(Debug)]
pub enum RunOutcome {
    /// The fix pass finished; the report carries the counts.
    Completed(FixReport),
    /// The run could not start, or its state store broke mid-run.
    Failed(RunError),
}

/// Every reason a run does not produce fixed files.
#[derive(Debug)]
pub enum RunError {
    /// Nothing under the source is shaped like a `mydata~<id>` part, so there is no export id to
    /// name the manifest by.
    NoExportId(PathBuf),
    /// More than one delivery shares the source dir; which one the run means would be a guess.
    SeveralExports { source: PathBuf, count: usize },
    /// Parts exist but none is unpacked with a `json/` dir.
    NoJsonDir(PathBuf),
    /// The part's id cannot name a manifest file (a dir named `mydata~..`, for instance).
    InvalidExportId(PathBuf),
    /// The `json/` dir is there and did not load.
    Json(LoadError),
    /// The export holds no `memories_history.json`: the memories category was not included when it
    /// was requested, which is a user's choice rather than a broken export.
    NoMemoriesFile,
    /// The media walk found no `memories` dir holding anything this build reads.
    NoMemoriesDir(PathBuf),
    /// The source root could not be listed looking for export parts.
    Discover(DiscoverError),
    /// The source root could not be listed looking for memories dirs.
    Scan(ScanError),
    /// The manifest could not be opened, enrolled, or written. The one mid-run failure: the state
    /// store itself is broken, so nothing can be recorded against it.
    Manifest(ManifestError),
    /// A bug in the pipeline unwound the worker. Not an input state; present so a caller can say
    /// something instead of spinning forever.
    Panicked,
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoExportId(source) => {
                write!(f, "no mydata~ export part under {}; point the source at the dir holding the export's parts", source.display())
            }
            Self::SeveralExports { source, count } => {
                write!(f, "{count} exports share {}; point the source at the dir holding the one to fix", source.display())
            }
            Self::NoJsonDir(source) => {
                write!(f, "no unpacked export part with a json/ dir under {}; extract the export's zips first", source.display())
            }
            Self::InvalidExportId(source) => write!(
                f,
                "the export part under {} names an id this build cannot keep a manifest for; rename the part dir",
                source.display()
            ),
            Self::Json(error) => write!(f, "{error}"),
            Self::NoMemoriesFile => {
                write!(f, "this export holds no memories_history.json; the memories category was not ticked when the export was requested")
            }
            Self::NoMemoriesDir(source) => {
                write!(f, "no memory media under {}; extract the export's memories dirs first", source.display())
            }
            Self::Discover(error) => write!(f, "{error}"),
            Self::Scan(error) => write!(f, "{error}"),
            Self::Manifest(error) => write!(f, "{error}"),
            Self::Panicked => write!(f, "the run stopped unexpectedly; this is a bug in exportsnap, not in your data"),
        }
    }
}

impl Error for RunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Discover(error) => Some(error),
            Self::Scan(error) => Some(error),
            Self::Manifest(error) => Some(error),
            Self::NoExportId(_)
            | Self::SeveralExports { .. }
            | Self::NoJsonDir(_)
            | Self::InvalidExportId(_)
            | Self::NoMemoriesFile
            | Self::NoMemoriesDir(_)
            | Self::Panicked => None,
        }
    }
}

/// Drives one memories run, reporting progress over `events`.
///
/// Sends [`RunEvent::Planned`] once the plan exists (the manifest is enrolled by then), then
/// [`RunEvent::Finished`] on every path. Never returns an error: the outcome travels in the
/// events, so a caller's worker thread has one thing to forward and nothing to map.
pub fn run(inputs: &RunInputs, events: &Sender<RunEvent>) {
    let outcome = match prepare(inputs) {
        Ok(mut prepared) => {
            let _ = events.send(RunEvent::Planned(prepared.snapshot.clone()));
            match local_fix::run(&prepared.plan, &mut prepared.manifest, DEFAULT_MAX_ATTEMPTS, &inputs.video) {
                Ok(report) => RunOutcome::Completed(report),
                Err(error) => RunOutcome::Failed(RunError::Manifest(error)),
            }
        }
        Err(error) => RunOutcome::Failed(error),
    };
    let _ = events.send(RunEvent::Finished(outcome));
}

/// The half of the run before any output is written: the plan and an enrolled manifest.
struct Prepared {
    snapshot: PlanSnapshot,
    plan: Plan,
    manifest: Manifest,
}

/// Everything up to and including the plan. Fails with a [`RunError`] for every state the screen
/// has words for.
fn prepare(inputs: &RunInputs) -> Result<Prepared, RunError> {
    // The export id names the manifest, so the first fact a run needs is the delivery's id. Taken
    // from the part dir name rather than from a hash of the source path, so the id survives the
    // export dir being moved.
    let groups = discover_parts(&inputs.source).map_err(RunError::Discover)?;
    let group = match groups.as_slice() {
        [] => return Err(RunError::NoExportId(inputs.source.clone())),
        [group] => group,
        several => return Err(RunError::SeveralExports { source: inputs.source.clone(), count: several.len() }),
    };
    let Some(export_id) = ExportId::new(&group.id) else {
        return Err(RunError::InvalidExportId(inputs.source.clone()));
    };

    // Only the first part carried `json/` in the one export observed, so every unpacked part is
    // walked rather than assuming which one has it.
    let Some(json_dir) = group.extracted.iter().find_map(|part| part.json_dir.as_deref()) else {
        return Err(RunError::NoJsonDir(inputs.source.clone()));
    };
    let export = ExportJson::load_dir(json_dir).map_err(RunError::Json)?;
    let Some(memories) = export.memories else {
        return Err(RunError::NoMemoriesFile);
    };

    let discovery = memories::discover(&inputs.source).map_err(RunError::Scan)?;
    let has_anything = !discovery.media.is_empty()
        || !discovery.orphan_overlays.is_empty()
        || !discovery.unparsed.is_empty()
        || !discovery.duplicates.is_empty();
    if !has_anything {
        return Err(RunError::NoMemoriesDir(inputs.source.clone()));
    }

    let reconciliation = reconcile(&memories, discovery);
    let manifest_dir = match &inputs.manifest_dir {
        Some(dir) => dir.clone(),
        None => crate::export::manifest::manifest_dir().map_err(RunError::Manifest)?,
    };
    let mut manifest = Manifest::open_in(&manifest_dir, &export_id).map_err(RunError::Manifest)?;
    reconciliation.enroll(&mut manifest).map_err(RunError::Manifest)?;

    // Read after the enrollment and before the plan, which is the only window where it is the state
    // the run will actually work from — the same ordering `chat_run::prepare` takes for its own seed.
    // The LATE edge is the one a test holds: the resume sweep inside `local_fix::run` drops the record
    // of an output the user deleted, so it has to land AFTER this or the rewrite shifts onto a
    // neighbour's path. `a_newcomer_sorting_ahead_of_a_recorded_item_does_not_take_its_output_name` in
    // `tests/memories_screen.rs` reds when it does.
    //
    // **The early edge holds by convention here and nothing reds if it moves.** Enroll's `reset`
    // clears the record of a row whose source came back, but under THIS build no memories row
    // carrying a record ever reaches that arm, so moving this line above the enrollment is an
    // equivalent mutant on this leg while being a real defect on the chat one — `Plan::build`'s
    // rustdoc carries the invariant, its proof and the measurement.
    //
    // Two changes reopen it and neither is exotic: a producer that parks a PAIRED row (the downloader
    // `memories::Reconciliation::enroll` says its `SourceMissing` arm exists for), or a non-empty
    // `Plan::excluded` on this leg, since `Manifest::exclude` keeps a record and `retire_absent`
    // carries an `Excluded` row on to `Retired` still holding it. Either one makes this position
    // load-bearing with no test to say so.
    let recorded = RecordedOutputs::read(&manifest, ItemKind::Memory).map_err(RunError::Manifest)?;
    let plan = Plan::build(&memories, &reconciliation, &inputs.out_root, &recorded);
    let rows = plan
        .items
        .iter()
        .map(|item| PlanRow {
            source_id: item.source_id.clone(),
            output_name: item.output.file_name().and_then(|name| name.to_str()).unwrap_or("?").to_owned(),
            leg: item.leg,
        })
        .collect();

    Ok(Prepared { snapshot: PlanSnapshot { export_id, manifest_dir, rows }, plan, manifest })
}
