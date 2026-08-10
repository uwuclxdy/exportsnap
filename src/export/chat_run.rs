//! The whole chat-media run in one call: what the chat media screen drives.
//!
//! The memories leg's [`super::memories_run`] one file over, with the same contract and three real
//! differences. Discover the export's parts, read its json, walk its `chat_media` dirs, join them to
//! `chat_history.json`, enroll the result in the manifest, plan every output path, run the fix pass,
//! report the outcome.
//!
//! # What a caller gets, and when
//!
//! [`run`] sends exactly one [`RunEvent::Planned`] — after the manifest is enrolled, before any
//! output is written — and then one [`RunEvent::Finished`] on every path, setup errors included.
//! The manifest this function opens is the run's **only writer**; a screen wanting live per-item
//! progress opens its own [`Manifest`] connection and polls
//! [`Manifest::items`](crate::export::manifest::Manifest::items) each tick, exactly as the memories
//! screen does and for the same reason (sqlite WAL, one autocommit statement per transition).
//!
//! # Where this leg differs from the memories one
//!
//! - **A missing `chat_history.json` is not a failure.** The memories leg refuses without
//!   `memories_history.json` because the entry IS the metadata; here the file's own name carries its
//!   day, and 6877 of the observed export's 9465 files are named by no message anyway. So an export
//!   delivered without the chat category still repairs every file it holds, filed under
//!   `_no-conversation/`, and [`HistoryOutcome`] is what says so. Refusing instead would decline
//!   work this build can genuinely do.
//! - **The plan carries counts.** [`PlanCounts`] is what the screen renders above the progress
//!   table: the items nothing paired, the thumbnails decision 44d drops, the formats this build
//!   defers, and the history tokens no file carries. All four are absences, which is the class this
//!   tool exists to report rather than absorb.
//! - **Overlay mode.** [`RunInputs::overlay`] is decision 44b, and it reaches the pass only through
//!   [`chat_fix::plan`] — see [`OverlayMode`].
//!
//! # Privacy
//!
//! **This is the first run composition whose input holds usernames.** A `chat_history.json`
//! conversation key IS a friend's username, and [`chat_fix::dir_name`] turns one into a directory
//! name.
//!
//! Two of the three things that leave this module carry neither the key nor a sender:
//! [`PlanRow::output_name`] is a file name with the directory deliberately dropped, and
//! [`PlanCounts`] is integers and one enum. Both are pinned by
//! `no_conversation_key_reaches_the_planned_event` in `tests/chat_media_screen.rs`.
//!
//! **The third — [`RunError`]'s prose — is established in two halves, because it is owned in two
//! places.** The six variants this module formats itself interpolate nothing but a path the CALLER
//! passed in and a count, which is checkable by reading the `Display` impl below and is pinned by
//! `a_run_errors_own_prose_names_only_the_callers_path`. The four that delegate —
//! [`RunError::Json`], [`RunError::Discover`], [`RunError::Scan`], [`RunError::Manifest`] — render
//! whatever their source type renders, which is a property of those types and not of this one.
//!
//! [`RunError::Json`] was the live leak: `serde_json` quotes the offending VALUE back, and for
//! `chat_history.json` that value is a message body. **It is now closed at the loader**, where the
//! property belongs — `crate::export::LoadError`'s `Display` strips the contents of every delimited
//! run out of the arm that can carry one, so the expectation and the position survive and the value
//! does not. Only the `Category::Data` arm: serde's own `classify()` already isolates the messages
//! holding caller text, and redacting the syntax arm too cost four punctuation diagnostics for no
//! privacy gain. The battery lives in `tests/export.rs`; the chat leg's own end of it is
//! `a_json_error_over_a_conversation_keyed_file_names_neither_key_nor_value`, which asserts the
//! composition on top does not undo the guarantee.
//!
//! Not closed, and not this module's to close: [`crate::export::LoadError::Invalid`] wraps the
//! crate's own `ParseError`, which interpolates its offending value with `{:?}` exactly as serde
//! does. Its `Field` set is deliberately restricted to metadata so a message body cannot reach it —
//! but "metadata" there includes a coordinate, and whether a lat/long belongs in a footer alert
//! looks like a question nobody has been asked rather than one that was answered.
//!
//! # Errors are typed, never panics
//!
//! Every state the screen has words for is a [`RunError`] variant with a `Display` a footer alert
//! can carry verbatim. The one residual is a genuine bug panicking mid-run; [`run`] is still
//! guaranteed to send [`RunEvent::Finished`] because the worker thread that calls it wraps it in
//! `catch_unwind` (see `src/tui/screens/chat_media.rs`).

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::export::chat_fix::{self, OverlayMode};
use crate::export::chat_media::{self, ChatScanError, Reconciliation, reconcile};
use crate::export::local_fix::{self, DEFAULT_MAX_ATTEMPTS, FixReport, Leg, Plan, VideoOptions};
use crate::export::manifest::{ExportId, Manifest, ManifestError};
use crate::export::model::ChatHistory;
use crate::export::zip::{DiscoverError, discover_parts};
use crate::export::{ExportJson, LoadError};

/// Everything a run needs, gathered by the caller.
#[derive(Debug, Clone)]
pub struct RunInputs {
    /// The dir holding the export's parts (or the parts themselves).
    pub source: PathBuf,
    /// Where the fixed chat media lands, under a `chat/` level of its own (decision 46a).
    pub out_root: PathBuf,
    /// Where the manifest lives. `None` resolves the platform's per-user data dir; a test passes
    /// its own tempdir so the real data dir is never touched.
    pub manifest_dir: Option<PathBuf>,
    /// What the run may do to a video's pixels.
    pub video: VideoOptions,
    /// What it does with a pair's caption layer (decision 44b).
    pub overlay: OverlayMode,
}

/// One row of the progress table, decided at plan time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    /// The manifest's identity: the chat-media unit's file id (`b~<id>`, or the zip family's
    /// `<day>_<mid>.zip.<hash>`). An opaque id off a FILENAME — never a conversation key.
    pub source_id: String,
    /// The output file's name, e.g. `20210304_120400.jpg`. **Not the path**: the conversation
    /// directory is derived from a friend's username, so the path is the one thing on this leg a
    /// table cell may not hold.
    pub output_name: String,
    /// Which leg fixes it.
    pub leg: Leg,
}

/// What the plan found and will not produce output for, as counts.
///
/// **Every field is a LOWER BOUND when [`Self::partial`] is set**, which is
/// [`Reconciliation::unreadable`] being non-empty: a dir that could not be listed may hold the file
/// a token names, the media half of an unmatched overlay, or anything else. A screen renders that
/// qualifier rather than a number that is quietly wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanCounts {
    /// Files that are a caption layer nothing paired: the whole plain `overlay~` family, plus any
    /// zip overlay whose media half is absent. Each is planned as an item in its own right.
    pub unmatched_overlays: usize,
    /// Thumbnails, enrolled and deliberately never written (decision 44d).
    pub excluded: usize,
    /// Items left to a later build, their manifest rows still pending: a format this build does not
    /// decode, or a name carrying no real calendar date.
    pub deferred: usize,
    /// `Media IDs` tokens the history names and no file carries — the chat analogue of the memories
    /// gap, one manifest row each rather than a number in a summary.
    pub missing_tokens: usize,
    /// What the run had to attribute from, and how far it got.
    pub history: HistoryOutcome,
    /// Part of the source could not be listed, so every count above is a lower bound.
    pub partial: bool,
}

/// Whether this export carried a chat history, and whether it joined anything.
///
/// **One field rather than a `has_history` bool beside an `attributed` one**, because those two
/// carry a dependency — [`Self::Absent`] implies nothing joined — and two independent bools that
/// must agree is a state a caller can record out of step. It is the same shape
/// [`super::local_fix::Originals`] exists to prevent one struct over.
///
/// The distinction is load-bearing rather than decorative: a screen reporting "no chat history"
/// off a run that read one and matched nothing states a CAUSE its observation does not support,
/// and can contradict the token-gap count rendered beside it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HistoryOutcome {
    /// The export carried no `chat_history.json` at all — delivered without the chat category.
    /// Every item lands in `_no-conversation/`, dated by its own filename.
    #[default]
    Absent,
    /// A history was read and no file carries a sender or a conversation.
    ///
    /// **Two sub-states, deliberately not split.** Either the history named files this run did not
    /// discover — a partially-extracted export whose `json/` part is unpacked and whose `chat_media`
    /// part is not, where every `Media IDs` token becomes a gap row — or it held no messages to
    /// name anything with (`{}`, or threads with empty record arrays). A fourth variant would buy
    /// nothing: what a screen has to say is the same in both, and splitting them would put two
    /// fields in agreement-or-not territory again. What the copy must NOT do is describe a
    /// comparison, since in the second sub-state none happened.
    JoinedNothing,
    /// At least one file carries a sender and a conversation.
    Joined,
}

/// Everything the screen needs once the plan is built.
#[derive(Debug, Clone)]
pub struct PlanSnapshot {
    pub export_id: ExportId,
    /// Where the manifest this run writes lives — where a polling reader opens its own connection.
    pub manifest_dir: PathBuf,
    /// One per plannable unit, in the reconciliation's own (source-id sorted) order.
    pub rows: Vec<PlanRow>,
    pub counts: PlanCounts,
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
///
/// Deliberately WITHOUT a "no chat history" variant — see the module docs. The set is otherwise the
/// memories leg's, because the states before the media walk are the same states.
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
    /// The media walk found no `chat_media` dir holding anything this build reads.
    NoChatMediaDir(PathBuf),
    /// The source root could not be listed looking for export parts.
    Discover(DiscoverError),
    /// The source root could not be listed looking for `chat_media` dirs.
    Scan(ChatScanError),
    /// The manifest could not be opened, enrolled, read back, or written. The one mid-run failure:
    /// the state store itself is broken, so nothing can be recorded against it.
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
            Self::NoChatMediaDir(source) => {
                write!(f, "no chat media under {}; extract the export's chat_media dirs first", source.display())
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
            | Self::NoChatMediaDir(_)
            | Self::Panicked => None,
        }
    }
}

/// Drives one chat-media run, reporting progress over `events`.
///
/// Sends [`RunEvent::Planned`] once the plan exists (the manifest is enrolled by then), then
/// [`RunEvent::Finished`] on every path. Never returns an error: the outcome travels in the events,
/// so a caller's worker thread has one thing to forward and nothing to map.
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
    // An absent `chat_history.json` is an export delivered without the chat category, not a broken
    // one: with no conversations every file joins as `Unnamed`/`NoToken` and lands in the
    // `_no-conversation/` bucket, dated by its own filename. See the module docs.
    //
    // **Whether there WAS one is captured here and nowhere else.** The substitution below erases the
    // difference between "no history file" and "a history that joined nothing", and no later reader
    // can recover it — which is what let a screen state a cause its own observation did not support.
    let had_history = export.chat_history.is_some();
    let history = export.chat_history.unwrap_or(ChatHistory { conversations: Vec::new() });

    let discovery = chat_media::discover(&inputs.source).map_err(RunError::Scan)?;
    let has_anything = !discovery.media.is_empty()
        || !discovery.unmatched_overlays.is_empty()
        || !discovery.unparsed.is_empty()
        || !discovery.duplicates.is_empty();
    if !has_anything {
        return Err(RunError::NoChatMediaDir(inputs.source.clone()));
    }

    let reconciliation = reconcile(&history, discovery);
    let manifest_dir = match &inputs.manifest_dir {
        Some(dir) => dir.clone(),
        None => crate::export::manifest::manifest_dir().map_err(RunError::Manifest)?,
    };
    let mut manifest = Manifest::open_in(&manifest_dir, &export_id).map_err(RunError::Manifest)?;
    reconciliation.enroll(&mut manifest).map_err(RunError::Manifest)?;

    // Read after the enrollment and before the plan, which is the only window where it is the state
    // the run will actually work from: the enrollment is what resets a row whose file came back, and
    // the resume sweep inside `local_fix::run` is what drops the record of an output the user
    // deleted — and that one has to land AFTER this, so the rewrite goes back into the directory it
    // was written into rather than starting a second one for the same thread. One read serves both
    // layers: the same rows carry decision 52's per-item output paths, and the same ordering
    // argument applies to them one component down.
    //
    // **Both edges are held by a test on this leg, and by different ones**, which is what makes them
    // separable rather than one claim. Each stays green under the other's mutation. Both live in
    // `tests/chat_media_screen.rs`:
    //   `a_returning_chat_file_has_its_record_cleared_before_the_seed_is_read`
    //     reds if this read moves above the enrollment;
    //   `a_newcomer_sorting_ahead_of_a_recorded_item_does_not_take_its_output_name`
    //     reds if the resume sweep moves above this read.
    let recorded = chat_fix::RecordedDirs::read(&reconciliation, &manifest).map_err(RunError::Manifest)?;
    let plan = chat_fix::plan(&reconciliation, &inputs.out_root, inputs.overlay, &recorded);
    let counts = counts(&reconciliation, &plan, had_history);
    let rows = plan
        .items
        .iter()
        .map(|item| PlanRow {
            source_id: item.source_id.clone(),
            // The NAME alone. `item.output` is `<out_root>/chat/<cleaned conversation key>/…`, and
            // that middle component is a friend's username — the one thing on this leg that may not
            // reach a rendered cell.
            output_name: item.output.file_name().and_then(|name| name.to_str()).unwrap_or("?").to_owned(),
            leg: item.leg,
        })
        .collect();

    Ok(Prepared { snapshot: PlanSnapshot { export_id, manifest_dir, rows, counts }, plan, manifest })
}

/// What the screen reports above the progress table.
///
/// The unmatched-overlay count is derived rather than carried: an item whose own file is a caption
/// layer is by construction one nothing paired, because [`chat_media::Discovery::from_walk`] hands
/// every paired overlay to its media file and only the leftovers become items of their own. Deriving
/// it here keeps one answer rather than a field that can disagree with the items beside it.
///
/// [`HistoryOutcome`] is the one thing that CANNOT be derived from the reconciliation, which is why
/// `had_history` is threaded in from the load: a reconciliation over an empty history and one over
/// an absent history are the same value.
fn counts(reconciliation: &Reconciliation, plan: &Plan, had_history: bool) -> PlanCounts {
    let joined = reconciliation.items.iter().any(|item| item.message().is_some());
    PlanCounts {
        unmatched_overlays: reconciliation.items.iter().filter(|item| item.media.file.token.is_overlay()).count(),
        excluded: plan.excluded.len(),
        deferred: plan.deferred.len(),
        missing_tokens: reconciliation.missing.len(),
        history: match (had_history, joined) {
            (false, _) => HistoryOutcome::Absent,
            (true, false) => HistoryOutcome::JoinedNothing,
            (true, true) => HistoryOutcome::Joined,
        },
        partial: !reconciliation.unreadable.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::path::PathBuf;

    use super::{HistoryOutcome, Plan, Reconciliation, counts};
    use crate::export::chat_media::{ChatMedia, ChatMediaFile, ChatMediaItem, Join, Message, MessageRef, UnreadableDir};
    use crate::export::manifest::ItemKind;
    use crate::export::model::ConversationId;

    /// A reconciliation built by hand, so the mapping below can be driven without a filesystem.
    ///
    /// Every field these tests move is `pub`, which is what lets the `partial` wiring be pinned at
    /// the line that computes it rather than through a `chmod 000` fixture — one that no-ops under
    /// root, differs per filesystem, and would either flake or silently skip.
    fn reconciliation(named: bool, unreadable: bool) -> Reconciliation {
        let file = ChatMediaFile::parse(PathBuf::from("/x/chat_media/2021-03-04_b~aB3xY9.jpg")).expect("the name parses");
        let join = if named {
            Join::Named(Message {
                at: MessageRef { conversation: 0, message: 0 },
                conversation: ConversationId::new("friend"),
                conversation_title: None,
                from: None,
                is_sender: false,
                created: None,
                created_epoch_ms: None,
            })
        } else {
            Join::Unnamed
        };
        Reconciliation {
            items: vec![ChatMediaItem { media: ChatMedia { file, overlay: None }, join }],
            missing: Vec::new(),
            unparsed_tokens: Vec::new(),
            unparsed: Vec::new(),
            duplicates: Vec::new(),
            unreadable: if unreadable {
                vec![UnreadableDir { dir: PathBuf::from("/x/locked"), kind: ErrorKind::PermissionDenied }]
            } else {
                Vec::new()
            },
        }
    }

    fn empty_plan() -> Plan {
        Plan { kind: ItemKind::ChatMedia, items: Vec::new(), deferred: Vec::new(), excluded: Vec::new() }
    }

    /// The mapping the F1 defect lived in: `had_history` is the only input that can tell an absent
    /// history from one that joined nothing, and no reconciliation carries it. Pinned at the
    /// mapping site rather than only through the rendered string, because this is where the two
    /// facts are combined and where a future edit would collapse them again.
    #[test]
    fn the_history_outcome_needs_both_inputs_and_collapses_neither() {
        let joined = counts(&reconciliation(true, false), &empty_plan(), true);
        assert_eq!(joined.history, HistoryOutcome::Joined);

        // A history was read and nothing carries a message. NOT `Absent` — the file was there.
        let read = counts(&reconciliation(false, false), &empty_plan(), true);
        assert_eq!(read.history, HistoryOutcome::JoinedNothing);

        // No history file at all. The reconciliation is IDENTICAL to the one above, which is the
        // whole reason the flag has to be threaded in from the load.
        let absent = counts(&reconciliation(false, false), &empty_plan(), false);
        assert_eq!(absent.history, HistoryOutcome::Absent);

        // `had_history == false` with something joined is unrepresentable in practice — nothing can
        // join without a history — and the mapping answers `Absent` rather than inventing a fourth
        // state, so a caller that got it wrong cannot produce a value the screen has no words for.
        assert_eq!(counts(&reconciliation(true, false), &empty_plan(), false).history, HistoryOutcome::Absent);
    }

    /// `partial` is what turns every count on the screen into a lower bound, and until this test it
    /// was reachable only through a synthesized `PlanCounts` — the wiring from
    /// `Reconciliation::unreadable` to the field was pinned nowhere.
    #[test]
    fn an_unreadable_dir_is_what_makes_the_counts_a_lower_bound() {
        assert!(!counts(&reconciliation(true, false), &empty_plan(), true).partial, "a complete scan reports exact counts");
        assert!(counts(&reconciliation(true, true), &empty_plan(), true).partial, "one unlistable dir qualifies the whole run");
    }
}
