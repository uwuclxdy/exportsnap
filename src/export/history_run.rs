//! The whole history export in one call: what the history screen drives.
//!
//! [`super::chat_run`]'s shape, with the work swapped: there is no media walk and no fix pass, only
//! the merged history the json already holds, planned into directories and written out as four
//! documents per conversation (decision 58). Discover the export's parts, read its json, merge chat
//! and snap, plan every document path, enroll one directory-claim row per conversation (decision
//! 63a), write the documents, report the outcome.
//!
//! # What a caller gets, and when
//!
//! [`run`] sends exactly one [`RunEvent::Planned`] — after the plan exists and the claims are
//! enrolled, before any document is written — and then one [`RunEvent::Finished`] on every path,
//! setup errors included. There is no per-item manifest poll on this leg: the run is one-shot
//! idempotent over json that is already local, with no resume and no per-item status (decision 63),
//! so the screen counts events rather than rows.
//!
//! # The directory claim (decision 63a)
//!
//! A conversation's directory is reserved off manifest ROWS, and nothing walks the output tree. A
//! directory that exists only because a history run created it therefore has to be a row or a later
//! chat-media run hands its name to a different conversation — the one row per conversation this run
//! enrolls, naming the directory it claimed and nothing else, through
//! [`Manifest::claim_directories`]. The run's own planning reads the same claims back (a second run
//! lands in the same directories), and the chat-media planner's occupancy seed reads them through
//! the one [`chat_fix::RecordedDirs`] read, which is what keeps the reservation true.
//!
//! # The no-manifest run (decision 62)
//!
//! A source naming no `mydata~*` part group mints no [`ExportId`], so there is no manifest to read
//! links from and no manifest to claim into. The run proceeds: directories derive from the key set
//! alone, every media reference in an html document renders as the inert placeholder the writer
//! answers with, and [`HistoryReport::links`] states that ONCE rather than per message — the same
//! [`HtmlLinks`] value the writer hands back per document. A source that DOES name a part whose id
//! cannot name a manifest is a different state and is refused ([`RunError::InvalidExportId`]), the
//! same call `chat_run` makes: the arm here is for the absent group, never the unusable id.
//!
//! # Privacy
//!
//! **This run's inputs hold usernames**, and none of them leaves it through the events: the
//! snapshot and the report are counts and one enum, and [`RunError`]'s prose is checkable below.
//! The one arm to read twice is [`RunError::Write`]: an `io::Error` carries the OS's message and
//! NOTHING names the path, because the directory a document lands in is derived from a conversation
//! key (decision 49) — the same reason the chat leg's [`chat_run::PlanRow`] drops the directory
//! from a rendered row.
//!
//! # Errors are typed, never panics
//!
//! Every state the screen has words for is a [`RunError`] variant with a `Display` a footer alert
//! can carry verbatim. The one residual is a genuine bug panicking mid-run; [`run`] sends
//! [`RunEvent::Finished`] on every non-panicking path, and once the history screen's worker lands
//! it wraps the call in `catch_unwind` exactly as the memories and chat-media screens do — task 80
//! wires that thread, and until then no caller of this module exists but a test channel.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crate::export::chat_fix::{CHAT_DIR, Conversations, RecordedDirs};
use crate::export::chat_media;
use crate::export::history::{self, Document, Html, HtmlLinks};
use crate::export::local_fix::{OutRootError, Outputs};
use crate::export::manifest::{DirectoryClaim, ExportId, Manifest, ManifestError};
use crate::export::model::{ChatHistory, ConversationId, SnapHistory};
use crate::export::zip::{DiscoverError, discover_parts};
use crate::export::{ExportJson, LoadError};

/// Everything a run needs, gathered by the caller.
#[derive(Debug, Clone)]
pub struct RunInputs {
    /// The dir holding the export's parts (or the parts themselves).
    pub source: PathBuf,
    /// Where the documents land, under a `chat/` level shared with the chat-media leg (decision 60).
    pub out_root: PathBuf,
    /// Where the manifest lives. `None` resolves the platform's per-user data dir; a test passes
    /// its own tempdir so the real data dir is never touched.
    pub manifest_dir: Option<PathBuf>,
}

/// The four document paths of one conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Documents {
    pub json: PathBuf,
    pub text: PathBuf,
    pub csv: PathBuf,
    pub html: PathBuf,
}

/// One conversation's plan: where its history documents land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The conversation key, as the merged history spells it.
    pub key: ConversationId,
    /// The directory decision 60 puts the documents in, under `<out>/chat/`.
    pub directory: PathBuf,
    pub documents: Documents,
}

/// Where every conversation's history documents land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPlan {
    /// One per conversation, in sorted key order.
    pub entries: Vec<HistoryEntry>,
}

/// Why the history plan could not be built.
#[derive(Debug)]
pub enum PlanError {
    /// The recorded directories could not be read off the manifest.
    Manifest(ManifestError),
    /// The out root could not be made absolute — a path the platform cannot name a directory with.
    OutRoot(OutRootError),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => write!(f, "{error}"),
            Self::OutRoot(error) => write!(f, "{error}"),
        }
    }
}

impl Error for PlanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::OutRoot(error) => Some(error),
        }
    }
}

/// Plans where each conversation's history documents land (decisions 60, 63a).
///
/// The history leg's entry into the shared directory machinery: the untrusted-key cleaner and its
/// append-only collision breaker are the chat-media planner's own code, driven here with the
/// history leg's key set and an attribution map derived from `chat_history.json` alone (for the
/// plain family the manifest `source_id` IS the history token) — nothing here touches the media
/// walk or `chat_fix`'s private reconciliation types, and the cleaner is not copied.
///
/// `manifest` is `None` when the source names no `mydata~*` part group (decision 62): nothing is
/// recorded and nothing can be, so every directory derives from the key set alone.
///
/// The four document names go through the same reservation the media outputs' do
/// ([`Outputs`]): a document never ADOPTS — no manifest row records one, the claim records the
/// directory — but a name any recorded path already claims is suffixed away from rather than
/// written over, which is what stops `history.<ext>` from resting on the coincidence that no
/// media stem spells one (decision 60).
///
/// # Errors
///
/// Returns [`PlanError::Manifest`] when the recorded-directory read fails, and
/// [`PlanError::OutRoot`] when the out root cannot be made absolute.
pub fn plan(
    keys: &BTreeSet<ConversationId>, attribution: &BTreeMap<String, &ConversationId>, out_root: impl AsRef<Path>,
    manifest: Option<&Manifest>,
) -> Result<HistoryPlan, PlanError> {
    let recorded = match manifest {
        Some(manifest) => RecordedDirs::read_by_tokens(attribution, manifest).map_err(PlanError::Manifest)?,
        None => RecordedDirs::default(),
    };
    // Canonicalized the way the chat-media planner canonicalizes, so a relative `--out` and an
    // absolute recorded directory name the same root — the property the adoption and the
    // reservation both depend on.
    let root = out_root.as_ref();
    let out_root = std::path::absolute(root).map_err(|_| OutRootError { root: root.to_path_buf() }).map_err(PlanError::OutRoot)?;
    let chat_root = out_root.join(CHAT_DIR);
    let mut conversations = Conversations::new(chat_root.clone(), &recorded);
    let mut outputs = Outputs::new(chat_root, recorded.outputs());

    // A `BTreeSet` iterates in `Ord` order, which is what keeps the collision assignment a function
    // of the key SET — the same requirement the chat-media planner documents on its own walk.
    let entries = keys
        .iter()
        .map(|key| {
            let directory = conversations.dir(key);
            let documents = Documents {
                json: outputs.reserve(&directory, "history", "json"),
                text: outputs.reserve(&directory, "history", "txt"),
                csv: outputs.reserve(&directory, "history", "csv"),
                html: outputs.reserve(&directory, "history", "html"),
            };
            HistoryEntry { key: key.clone(), directory, documents }
        })
        .collect();
    Ok(HistoryPlan { entries })
}

/// Everything the screen needs once the plan is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanSnapshot {
    /// Conversations the run will write documents for — the counter's denominator (decision 63).
    pub conversations: usize,
}

/// One message from the worker to the screen.
#[derive(Debug)]
pub enum RunEvent {
    /// The plan is built and the claims are enrolled.
    Planned(PlanSnapshot),
    /// The run is over, however it ended.
    Finished(RunOutcome),
}

/// How a run ended.
#[derive(Debug)]
pub enum RunOutcome {
    /// The documents are on disk; the report carries the counts.
    Completed(HistoryReport),
    /// The run could not start, or its state store or the output broke mid-run.
    Failed(RunError),
}

/// What one history run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryReport {
    /// Conversations written, one directory each.
    pub conversations: usize,
    /// Documents written, four per conversation.
    pub documents: usize,
    /// What the html media links did (decision 62), stated once here rather than per message: a
    /// run whose source names no `mydata~*` part group had no manifest to read, so every media
    /// reference in its documents renders as a placeholder.
    pub links: HtmlLinks,
}

/// Every reason a run does not produce history documents.
#[derive(Debug)]
pub enum RunError {
    /// More than one delivery shares the source dir; which export the claims belong to would be a
    /// guess.
    SeveralExports { source: PathBuf, count: usize },
    /// Parts exist but none is unpacked with a `json/` dir, and a source with no parts holds no
    /// `json/` of its own.
    NoJsonDir(PathBuf),
    /// The export holds neither `chat_history.json` nor `snap_history.json`: the chat category was
    /// not included when it was requested, which is a user's choice rather than a broken export.
    NoHistory(PathBuf),
    /// The part's id cannot name a manifest file (a dir named `mydata~..`, for instance). NOT the
    /// no-manifest arm: that one is for a source naming no part group at all (decision 62), and
    /// this one is the same refusal `chat_run` makes — a source every other leg refuses must not
    /// read as a clean history run with placeholder links and no claim rows.
    InvalidExportId(PathBuf),
    /// The `json/` dir is there and did not load.
    Json(LoadError),
    /// The source root could not be listed looking for export parts.
    Discover(DiscoverError),
    /// The manifest could not be opened, read, or claimed into. The one mid-run failure: the state
    /// store itself is broken, so nothing can be recorded against it.
    Manifest(ManifestError),
    /// The plan could not be built.
    Plan(PlanError),
    /// A document could not be written. **The `io::Error` carries the OS's message and no path**:
    /// the directory a document lands in is derived from a conversation key, and an error message
    /// never echoes the export's own bytes (decision 49).
    Write(io::Error),
    /// A bug in the pipeline unwound the worker. Not an input state; present so a caller can say
    /// something instead of spinning forever.
    Panicked,
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SeveralExports { source, count } => {
                write!(f, "{count} exports share {}; point the source at the dir holding the one to export", source.display())
            }
            Self::NoJsonDir(source) => {
                write!(f, "no unpacked export part with a json/ dir under {}; extract the export's zips first", source.display())
            }
            Self::NoHistory(source) => write!(
                f,
                "no chat_history.json or snap_history.json under {}; the chat category was not ticked when the export was requested",
                source.display()
            ),
            Self::InvalidExportId(source) => write!(
                f,
                "the export part under {} names an id this build cannot keep a manifest for; rename the part dir",
                source.display()
            ),
            Self::Json(error) => write!(f, "{error}"),
            Self::Discover(error) => write!(f, "{error}"),
            Self::Manifest(error) => write!(f, "{error}"),
            Self::Plan(error) => write!(f, "{error}"),
            Self::Write(source) => write!(f, "could not write a history document: {source}"),
            Self::Panicked => write!(f, "the run stopped unexpectedly; this is a bug in exportsnap, not in your data"),
        }
    }
}

impl Error for RunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Discover(error) => Some(error),
            Self::Manifest(error) => Some(error),
            Self::Plan(error) => Some(error),
            Self::Write(source) => Some(source),
            Self::SeveralExports { .. } | Self::NoJsonDir(_) | Self::NoHistory(_) | Self::InvalidExportId(_) | Self::Panicked => None,
        }
    }
}

/// Drives one history run, reporting progress over `events`.
///
/// Sends [`RunEvent::Planned`] once the plan exists and the claims are enrolled, then
/// [`RunEvent::Finished`] on every path. Never returns an error: the outcome travels in the
/// events, so a caller's worker thread has one thing to forward and nothing to map.
pub fn run(inputs: &RunInputs, events: &Sender<RunEvent>) {
    let outcome = match prepare(inputs) {
        Ok(prepared) => {
            let _ = events.send(RunEvent::Planned(prepared.snapshot));
            match write(&prepared) {
                Ok(report) => RunOutcome::Completed(report),
                Err(error) => RunOutcome::Failed(error),
            }
        }
        Err(error) => RunOutcome::Failed(error),
    };
    let _ = events.send(RunEvent::Finished(outcome));
}

/// The half of the run before any document is written: the plan, the enrolled claims, and the
/// documents to write.
struct Prepared {
    snapshot: PlanSnapshot,
    manifest: Option<Manifest>,
    plan: HistoryPlan,
    documents: BTreeMap<ConversationId, Document>,
}

/// Everything up to and including the plan and the claim enrollment. Fails with a [`RunError`]
/// for every state the screen has words for.
fn prepare(inputs: &RunInputs) -> Result<Prepared, RunError> {
    // The export id names the manifest, so the first fact a run needs is the delivery's id. For
    // history the id is optional (decision 62): a source naming no `mydata~*` part group has no
    // manifest to read links from, and the run degrades to placeholders instead of refusing.
    let groups = discover_parts(&inputs.source).map_err(RunError::Discover)?;
    let group = match groups.as_slice() {
        [] => None,
        [group] => Some(group),
        several => return Err(RunError::SeveralExports { source: inputs.source.clone(), count: several.len() }),
    };
    let json_dir: PathBuf = match group {
        Some(group) => group
            .extracted
            .iter()
            .find_map(|part| part.json_dir.as_deref())
            .ok_or_else(|| RunError::NoJsonDir(inputs.source.clone()))?
            .to_path_buf(),
        // No part group: the export was extracted flat into the source (or the part dirs were
        // renamed away), which is exactly the run decision 62's no-manifest arm exists for. The
        // json dir is the source's own.
        None => {
            let candidate = inputs.source.join("json");
            if candidate.is_dir() { candidate } else { return Err(RunError::NoJsonDir(inputs.source.clone())) }
        }
    };
    let export = ExportJson::load_dir(&json_dir).map_err(RunError::Json)?;
    if export.chat_history.is_none() && export.snap_history.is_none() {
        return Err(RunError::NoHistory(inputs.source.clone()));
    }
    // An absent file is an empty history, the same substitution `chat_run` makes for its own
    // absent `chat_history.json`.
    let chat = export.chat_history.unwrap_or(ChatHistory { conversations: Vec::new() });
    let snap = export.snap_history.unwrap_or(SnapHistory { conversations: Vec::new() });

    let merged = history::merge(&chat, &snap);
    let keys: BTreeSet<ConversationId> = merged.threads.iter().map(|thread| thread.id.clone()).collect();
    let attribution = chat_media::history_attribution(&chat);
    let documents: BTreeMap<ConversationId, Document> =
        merged.threads.into_iter().map(|thread| (thread.id.clone(), Document::from_thread(thread))).collect();

    let mut manifest = match group {
        Some(group) => {
            // A part whose id cannot name a manifest is refused, the same call `chat_run` makes:
            // the no-manifest arm below is for a source naming NO part group (decision 62), not
            // for an unusable id — degrading silently here would report a clean run with
            // placeholder links and no claim rows over a source every other leg refuses.
            let Some(export_id) = ExportId::new(&group.id) else {
                return Err(RunError::InvalidExportId(inputs.source.clone()));
            };
            let manifest_dir = match &inputs.manifest_dir {
                Some(dir) => dir.clone(),
                None => crate::export::manifest::manifest_dir().map_err(RunError::Manifest)?,
            };
            Some(Manifest::open_in(&manifest_dir, &export_id).map_err(RunError::Manifest)?)
        }
        None => None,
    };

    // The read inside `plan` has to land before the claim write: the claims this run enrolls are
    // this run's own plan, and the next run's read is what adopts them back.
    let plan = plan(&keys, &attribution, &inputs.out_root, manifest.as_ref()).map_err(RunError::Plan)?;
    if let Some(manifest) = manifest.as_mut() {
        let claims: Vec<DirectoryClaim<'_>> =
            plan.entries.iter().map(|entry| DirectoryClaim { source_id: entry.key.as_str(), directory: &entry.directory }).collect();
        manifest.claim_directories(&claims).map_err(RunError::Manifest)?;
    }

    Ok(Prepared { snapshot: PlanSnapshot { conversations: plan.entries.len() }, manifest, plan, documents })
}

/// The documents' half of the run: render and land every one of the plan's documents.
fn write(prepared: &Prepared) -> Result<HistoryReport, RunError> {
    let mut links = None;
    for entry in &prepared.plan.entries {
        // Both the plan and the map are built from the same merged history in `prepare`, so a miss
        // is a bug rather than an input state.
        let Some(document) = prepared.documents.get(&entry.key) else {
            return Err(RunError::Panicked);
        };
        fs::create_dir_all(&entry.directory).map_err(RunError::Write)?;

        let json = history::write_json(document).map_err(|source| RunError::Write(io::Error::other(source)))?;
        fs::write(&entry.documents.json, json).map_err(RunError::Write)?;
        fs::write(&entry.documents.text, history::write_text(document)).map_err(RunError::Write)?;
        fs::write(&entry.documents.csv, history::write_csv(document)).map_err(RunError::Write)?;
        let Html { html, links: rendered } = history::write_html(document, prepared.manifest.as_ref()).map_err(RunError::Manifest)?;
        fs::write(&entry.documents.html, html).map_err(RunError::Write)?;
        links = Some(rendered);
    }
    // Every document in one run shares the manifest, so every render answers the same links value.
    // With nothing to render, `write_html`'s own rule answers for the run: manifest present means
    // links resolve where a row is done.
    let links = links.unwrap_or(if prepared.manifest.is_some() { HtmlLinks::Manifest } else { HtmlLinks::NoManifest });
    Ok(HistoryReport { conversations: prepared.plan.entries.len(), documents: prepared.plan.entries.len() * 4, links })
}
