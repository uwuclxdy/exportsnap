//! The chat-media fix pass: the per-conversation output tree, and the plan that drives
//! [`super::local_fix::run`] over it.
//!
//! [`super::chat_media`] answers which files exist, which message named each one and what that
//! message said. This module answers where each file lands and what goes into it, and then hands a
//! [`Plan`] to the same [`super::local_fix::run`] the memories leg uses. **Nothing here composites,
//! stamps or writes**: the item-level sequence lives in `local_fix` exactly once, and the only
//! reason this module exists beside it is that the two legs disagree about the tree and about the
//! date chain, not about what fixing a file means.
//!
//! # The tree
//!
//! Decision 46a, `<out_root>/chat/<conversation>/<stem>.<ext>`, flat inside the conversation
//! folder. The `chat/` level is not decoration: both legs share one output root, and without it a
//! conversation key cleaning to `2021` would drop its files into the memories leg's own
//! `<out_root>/2021/` tree.
//!
//! Decision 46b, `<out_root>/chat/_no-conversation/YYYY/MM/<stem>.<ext>` for every file no message
//! names. One bucket, with the memories leg's year/month tree inside it because that bucket holds
//! 6877 of the observed export's 9465 files and a directory with 6877 entries is not one a file
//! browser opens. [`NO_CONVERSATION_DIR`] is **reserved**: a real conversation key that cleans to
//! that string is suffixed away from it rather than merged into it, since merging would file a real
//! thread's media under the name that means "no thread".
//!
//! Decision 46c, with overlay mode `both` the merged file lands in the item's own directory and the
//! export's two originals land in an `originals/` subfolder of that same directory. In the observed
//! export that subfolder can only ever appear under `_no-conversation/YYYY/MM/`, and that is a
//! consequence rather than a rule: only the zip family pairs, every zip file joins as
//! [`super::chat_media::Join::NoToken`] because `chat_history.json` can name nothing but the plain
//! `b` family, and a `NoToken` item is by definition one no message names.
//!
//! Decision 53, those copies keep the export's own filenames and take a `_2` where two of them fold
//! onto one name in one subfolder. The claim is [`super::local_fix::Outputs`], the same set the
//! merged file's own path comes out of, so every path this run writes is issued once. What that
//! reservation is worth, and what it does not buy without a record to adopt from, is at
//! [`super::local_fix::Originals`].
//!
//! # The overlay mode
//!
//! [`OverlayMode`] is decision 44b, and it is expressed entirely in the PLAN. `local_fix::fix`
//! neither takes a mode nor branches on one: the planner decides whether the item-level pass is
//! handed an overlay to composite ([`super::local_fix::SourceMedia::overlay`]) and whether the
//! export's own pair is kept ([`super::local_fix::Originals`]), and those two answers are the whole
//! of what the three modes disagree about. A mode flag threaded into `fix` would have put a fourth
//! branch inside the sequence both legs share, which is the one thing this module exists to avoid.
//!
//! Decision 44c, the sender and the timestamp go into the file's own metadata and its modification
//! time and into **nothing else**. No filename prefix, so a file a message named and a file none did
//! carry one filename shape — `YYYYMMDD_HHMMSS.<ext>`, the same shape the memories leg writes, with
//! the same `_2`/`_3` collision suffix.
//!
//! Decision 44d, a thumbnail is enrolled and never written. It goes to [`Plan::excluded`] and its
//! manifest row to [`crate::export::manifest::ItemStatus::Excluded`], which is counted apart from
//! done, failed, missing and retired for the reason that status exists: nothing is wrong with the
//! file and nothing will ever be written from it.
//!
//! # Dates, and the coordinate there is none of
//!
//! The chain is the memories one with its first step swapped: the message's own date — its
//! `Created`, then its `Created(microseconds)` where `Created` is empty — then the file's own
//! embedded timestamp, then the filename day at midnight.
//! [`super::chat_media::ChatMediaItem::date`] hands over the first and third unresolved precisely so
//! the second can sit between them, and this module is where that happens — through the same
//! [`super::local_fix`] route the memories leg reads an embedded timestamp with, so a JPEG's
//! `DateTimeOriginal` and an MP4's `mvhd` are read one way for the whole crate.
//!
//! **No coordinate is stamped and no timezone lookup runs, on any chat-media item.** That is the
//! export and not an omission: `chat_history.json` carries no location field anywhere, and no other
//! json in the export references a chat-media file at all, so there is no coordinate to take. Said
//! here rather than left for a reader to infer from an absent call — an absent call reads as
//! something nobody got to, and this one is a fact about the data.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::export::chat_media::{ChatMediaItem, MediaDate, Message, Reconciliation, Token};
use crate::export::local_fix::{self, Capture, DeferralReason, Deferred, Leg, Outputs, Plan, PlannedItem, RecordedOutputs, SourceMedia};
use crate::export::manifest::{ItemKind, Manifest, ManifestError};
use crate::export::model::{Attribution, ConversationId};

/// What decision 44b does with a chat-media pair's two files.
///
/// The three differ in exactly two independent answers — is the caption burned in, and are the
/// export's own two files kept — and every mode is one of the four combinations that is worth
/// having. The fourth ("neither") is not a mode: it would produce nothing, and `mark_done`
/// checksums an output, so an item with no output demotes on every resume.
///
/// | mode | composites | keeps the originals |
/// |---|---|---|
/// | [`Self::Merged`] | yes | no |
/// | [`Self::Both`] | yes | yes |
/// | [`Self::Originals`] | no | yes |
///
/// **Under [`Self::Originals`] the main is still repaired**: it goes through the ordinary fix pass —
/// stamped, dated, transcoded per [`super::local_fix::VideoOptions`] — and IS the manifest's output.
/// The only thing that does not happen is the burn. Copying the two files and writing no output was
/// never available: decision 46d refused to weaken the checksum guard for 44 thumbnails, so it is
/// not being weakened for an overlay mode either.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverlayMode {
    /// Composite the caption in and keep nothing else. One file per message, at the cost of a lossy
    /// generation with no original beside it.
    Merged,
    /// Composite the caption in AND keep the export's two files. Decision 44b's user pick and the
    /// default: nothing is lost, at the cost of doubled output for the ~7% of files that pair.
    #[default]
    Both,
    /// Keep the export's two files and never burn the caption in. The main is still repaired and is
    /// still the output; no photo browser will show the caption.
    Originals,
}

impl OverlayMode {
    /// Tab-bar order for the cycle control, and the order a screen walks with `space`.
    pub const ALL: [Self; 3] = [Self::Merged, Self::Both, Self::Originals];

    /// The lowercase word a control renders and a config file would spell.
    #[must_use]
    pub const fn as_word(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::Both => "both",
            Self::Originals => "originals",
        }
    }

    /// Whether the caption layer reaches the item-level pass at all.
    #[must_use]
    pub const fn composites(self) -> bool {
        matches!(self, Self::Merged | Self::Both)
    }

    /// Whether the export's own two files are copied into the `originals/` subfolder.
    #[must_use]
    pub const fn keeps_originals(self) -> bool {
        matches!(self, Self::Both | Self::Originals)
    }

    /// The next mode a `space` press lands on, wrapping (cloudy-tui: Cycle row).
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Merged => Self::Both,
            Self::Both => Self::Originals,
            Self::Originals => Self::Merged,
        }
    }
}

impl fmt::Display for OverlayMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_word())
    }
}

/// The level every chat-media output sits under, decision 46a.
///
/// Public because the chat media screen's `output dir` row names where this leg actually writes, and
/// a row spelling `chat` for itself would be a second copy of the tree's own shape.
pub const CHAT_DIR: &str = "chat";

/// Where a file no message names lands, decision 46b, and a name no cleaned conversation key is
/// allowed to take.
const NO_CONVERSATION_DIR: &str = "_no-conversation";

/// The subfolder the export's own two files are copied into, decision 46c.
const ORIGINALS_DIR: &str = "originals";

/// What a conversation key that cleans away to nothing is called.
///
/// Reachable from real data: [`ConversationId::new`] accepts `""` on purpose, and a key spelled
/// entirely out of characters this module refuses cleans to the same place.
const UNNAMED_DIR: &str = "_unnamed";

/// The longest a cleaned conversation directory name may be, in bytes.
///
/// Every filesystem this build targets allows at least 255 bytes per path component, so this is not
/// the filesystem's limit — it is a bound on untrusted json text that becomes a directory name. A
/// truncation that lands two distinct keys on one name is broken by the same suffix a cleaning
/// collision is, so shortening costs a suffix and never a merge.
const MAX_DIR_NAME: usize = 64;

/// What a character this module refuses becomes.
const REPLACEMENT: char = '_';

/// Windows device names, reserved as the STEM of a path component whatever extension follows it:
/// `CON`, `CON.txt` and `con.anything` all fail to open, on every Windows this build could run on.
///
/// `COM0` and `LPT0` are left out because they are not reserved on the NT line, and `CLOCK$` because
/// `$` is not a character [`portable`] keeps, so a key spelling it cannot reach this list.
const RESERVED_STEMS: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4",
    "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The directory name a conversation key becomes, before a collision with another key's is broken.
///
/// **This is a trust boundary.** The key is a top-level key of `chat_history.json`, so it is
/// arbitrary json text that this run turns into a path component under the output root. Four things
/// it must not be able to do, and what stops each:
///
/// - **Escape the output root.** `/`, `\`, `:` and `~` are not in [`portable`]'s allowlist, and `.`
///   is stripped from both ends, so `..`, `.`, `/etc/passwd` and `C:\x` all come out as a single
///   harmless component or as [`UNNAMED_DIR`]. The allowlist is what carries this rather than a list
///   of characters to strip: a deny list is a detector deciding what is safe, and a separator
///   spelling nobody anticipated passes one.
/// - **Produce a name Windows cannot open.** The reserved characters `<>:"|?*` and every control
///   byte fall outside the allowlist; a trailing dot or space cannot survive it either, since space
///   is not portable and dots are trimmed; and a [`RESERVED_STEMS`] device name is prefixed out of
///   the way rather than refused.
/// - **Produce an empty name.** `""`, `"."`, `".."` and a key made entirely of refused characters
///   all reach [`UNNAMED_DIR`].
/// - **Produce a name of unbounded length.** [`MAX_DIR_NAME`] caps it, in characters that are all
///   one byte by then, so the cap is a byte cap too.
///
/// What it deliberately does NOT do is guarantee two distinct keys get two distinct names — mapping
/// a character class onto one replacement cannot — which is why [`Conversations`] exists and why
/// this function is not the whole boundary on its own.
#[must_use]
pub fn dir_name(key: &str) -> String {
    let mapped: String = key.chars().map(|c| if portable(c) { c } else { REPLACEMENT }).take(MAX_DIR_NAME).collect();
    // Both ends: a trailing dot is unopenable on Windows and a leading one hides the directory on
    // unix, and trimming both is also what turns `.` and `..` into the empty string rather than into
    // a component this build would then join onto the output root.
    let trimmed = mapped.trim_matches('.');
    if trimmed.is_empty() {
        return UNNAMED_DIR.to_owned();
    }
    let stem = trimmed.split_once('.').map_or(trimmed, |(head, _)| head);
    if RESERVED_STEMS.iter().any(|reserved| stem.eq_ignore_ascii_case(reserved)) {
        return format!("{REPLACEMENT}{trimmed}");
    }
    trimmed.to_owned()
}

/// Where each conversation's output has actually been landing, read back out of the manifest.
///
/// [`Conversations`] mints a directory name per conversation KEY and breaks a collision between two
/// of them with an ordinal that is a position in this run's plan. That is stable against an ITEM
/// leaving the export and not against a whole CONVERSATION leaving: the key set itself changes, so a
/// survivor's ordinal moves and a resumed run files its remaining media into the tree already
/// holding another conversation's finished output. This is what closes that. The run reads the
/// directory each conversation's own rows already name and keeps it, rather than re-deriving one
/// from the key set every time.
///
/// **Which rows belong to a conversation is the RECONCILIATION's answer, never the path's shape.**
/// Decision 46b files every unit no message names under `_no-conversation/YYYY/MM/`, whose parent
/// directory is a month folder and not a conversation's, so a seed built by looking at what a path
/// looks like would claim one of those as a conversation directory. The join is a fact this run
/// already holds, and it is the only thing consulted here.
///
/// **Both halves of that answer, not only the items.** A row's `source_id` is either a joined unit
/// or a gap TOKEN, and both can carry an output record: a file a message names vanishing between two
/// runs drives the row it already finished to [`super::manifest::ItemStatus::SourceMissing`], which
/// takes it out of [`Reconciliation::items`] and into [`Reconciliation::missing`] under the same
/// identity. Reading items alone leaves that row attributable to nobody while it goes on naming a
/// real directory — and since it is still reserved below, the conversation is then reserved out of
/// its OWN directory and its remaining media starts a second one beside its finished output. That is
/// task 40's own harm class, and [`super::chat_media::MissingMedia::conversation`] exists so it
/// cannot happen.
///
/// **The recorded path is the item's own output** — [`super::local_fix::run`] hands
/// [`Manifest::mark_done`] [`super::local_fix::PlannedItem::output`] and nothing else — so its
/// parent IS the conversation's directory. Decision 46c's `originals/` copies sit one level further
/// down under overlay mode `both` and are never checked in, which is what makes the parent
/// unambiguous rather than a shape that depends on the mode.
///
/// **Every row carrying an output record seeds, whatever status it carries.** That is a wider set
/// than `done` and deliberately so: the manifest's own output-record rule is that the three output
/// columns survive a transition into a parked status ([`super::manifest::ItemStatus::SourceMissing`],
/// [`super::manifest::ItemStatus::Retired`], [`super::manifest::ItemStatus::Excluded`]) and are
/// cleared by the work ones ([`super::manifest::ItemStatus::Pending`],
/// [`super::manifest::ItemStatus::Failed`]), so a recorded path already means "a run finished this
/// row and nothing has driven it back to work". Asking `output_path.is_some()` asks that once;
/// naming a status list here would be a second spelling of the same rule, free to drift from it the
/// next time a status is added. On a parked row the record is HISTORY rather than a live claim about
/// disk — the user may have deleted the output tree — and that costs nothing, because what is kept
/// is the NAME a conversation's media groups under and not the existence of a file.
///
/// **The parked statuses are why that is not a second spelling of `done`, and the reason is dated.**
/// Until queue task 39 (2026-08-08) `mark_source_missing` nulled the three output columns, so a
/// vanished row named no directory and every seeding row really was `done`. That transition now KEEPS
/// the record, and this reader consumes exactly the set task 39 widened. Both halves feel it, and
/// each is pinned separately because narrowing them together hides the first behind the second:
///
/// - [`Self::named`] narrowed to `done` loses the conversation whose only finished file vanished, and
///   its remaining media starts a second directory —
///   `a_conversation_whose_only_finished_file_vanished_keeps_its_own_directory`.
/// - [`Self::occupied`] narrowed to `done` loses a RETIRED row's reservation, and a new key takes the
///   departed thread's directory — `a_retired_rows_record_still_reserves_the_directory_it_names`.
///   Reached by an ordinary chain: the file goes while the thread still names its token (park, record
///   kept), then the thread leaves the history (retire, record kept).
///
/// Narrowing BOTH at once still passes the first of those, since the row then drops out of the
/// reservation too and the answer coincides — measured, and the reason the two are pinned apart. The
/// one parked status that stays unreachable-with-a-record here is
/// [`super::manifest::ItemStatus::Excluded`], whose only producer is decision 44d's thumbnails, which
/// no message can name because [`super::chat_media`]'s history-token grammar admits the `b~` spelling
/// alone.
///
/// **Two rows of one conversation can disagree**, if a run before this rule split them, so
/// [`Self::named`] keeps every directory the conversation's rows name and [`Conversations::adopt`]
/// takes the lowest ADOPTABLE one in `Ord` order. Lowest, because that is a function of the SET of
/// directories those rows name: one row leaving cannot move the answer unless it was the last one
/// naming its directory, which is the same property this type exists to buy one layer up. "The first
/// row's" fails exactly that — the lowest source id leaving moves the answer while every other row
/// still names its own directory — and "the most recent" would rest on `updated_at`, which ties.
///
/// **Adoptable, and not merely lowest, and the difference is a whole conversation's tree.** A
/// candidate this run cannot take — one recorded under a different output root, or under the
/// `_no-conversation` bucket's month tree by a build whose shape differed — must fall through to the
/// next rather than stand in for the conversation. Reducing to a single minimum HERE would let one
/// unadoptable record that sorts low drop the adoption entirely and send the conversation back to
/// deriving from the key set, which is the outcome this whole type exists to prevent; `/a/chat/x`
/// sorting below `/b/chat/x` makes that reachable with no forged row at all. So the filter lives at
/// the only place that knows which root this run writes into, and this side keeps the candidates.
/// The `BTreeMap`/`BTreeSet` pair is what keeps both walks off a hash seed.
///
/// **[`Self::occupied`] is the other half, and it is about the conversations this run has NO item
/// for.** A conversation that left the export keeps its directory on disk and its rows go on naming
/// it, and nothing in this run's key set mentions it — so a NEW key cleaning onto that name would be
/// planned straight into a departed thread's tree, on top of files finished rows still claim. Every
/// directory any row records is therefore reserved before a name is derived, which costs a suffix and
/// never a merge. That set is deliberately NOT filtered by the join: attributing a row to a
/// conversation needs the reconciliation, but reserving a name the tree already contains does not.
///
/// **Reserving unconditionally is only safe because [`Self::named`] sees every attributable row.**
/// The reservation cannot tell a departed conversation's directory from a live one's; it just claims
/// the name. What keeps a live conversation from being reserved out of its own tree is that the
/// adopt pass runs FIRST and has already assigned it — which holds exactly as long as every row that
/// can carry an output record is attributable. Both are: a joined item through its message, a gap
/// token through [`super::chat_media::MissingMedia::conversation`]. Add a third producer of a
/// recorded path that the reconciliation cannot attribute and this reservation turns on it.
#[derive(Debug, Default)]
pub struct RecordedDirs {
    /// Every directory a conversation's own rows name, per conversation.
    named: BTreeMap<ConversationId, BTreeSet<PathBuf>>,
    /// Every directory ANY row of this kind names, the ones no conversation of this run owns
    /// included.
    occupied: BTreeSet<PathBuf>,
    /// The same rows read one layer down: the output PATH each row records, which is what stops a
    /// departing item shifting a later one onto a finished row's file. Decision 52, and it rides on
    /// this type rather than on a second read for the reason [`Self::read`] gives.
    outputs: RecordedOutputs,
}

impl RecordedDirs {
    /// Reads what `manifest` records for the units `reconciliation` names, and what it records at all.
    ///
    /// One whole-kind [`Manifest::items`] read rather than a point query per unit, for the reason
    /// [`Reconciliation::enroll`] gives about its own two: a real export would make that 9001 point
    /// queries. **One read for both layers**, so decision 52's per-item seed costs no second query:
    /// the rows are read once and [`RecordedOutputs::of`] takes its own answer out of them.
    ///
    /// **The read is unconditional, and it did not used to be.** It returned early when the
    /// reconciliation joined nothing — every export delivered without the chat category — on the
    /// grounds that [`plan`] derives no conversation directory there, so nothing could be adopted
    /// and nothing reserved. That is still true of the DIRECTORY layer and false of the item layer:
    /// with no history at all every file lands in the `_no-conversation` bucket, which is exactly
    /// where a day's items share one directory and collide by default. The early return would have
    /// left decision 52 unenforced on the one export shape that needs it most.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when the manifest read fails.
    pub fn read(reconciliation: &Reconciliation, manifest: &Manifest) -> Result<Self, ManifestError> {
        // Both halves of the reconciliation's own identity space, because both can name a row that
        // carries an output record: an item a message joined, and a gap TOKEN whose file vanished
        // between two runs — the two share one `source_id`, which is what makes a single map right
        // here rather than two lookups.
        let conversations: BTreeMap<&str, &ConversationId> = reconciliation
            .items
            .iter()
            .filter_map(|item| item.message().map(|message| (item.source_id(), &message.conversation)))
            .chain(reconciliation.missing.iter().map(|missing| (missing.token.as_str(), &missing.conversation)))
            .collect();

        let rows = manifest.items(ItemKind::ChatMedia)?;
        let mut recorded = Self { outputs: RecordedOutputs::of(&rows), ..Self::default() };
        for row in rows {
            let Some(dir) = row.output_path.as_deref().and_then(Path::parent) else { continue };
            recorded.occupied.insert(dir.to_path_buf());
            if let Some(conversation) = conversations.get(row.source_id.as_str()) {
                recorded.named.entry((*conversation).clone()).or_default().insert(dir.to_path_buf());
            }
        }
        Ok(recorded)
    }
}

/// The single component `dir` names directly under `root`, or `None` when it is not a child of it.
///
/// The whole of what makes a name off the manifest safe to join back onto the output root, and it is
/// the containment property rather than a cleaning pass — see [`Conversations::adopt`].
fn child_name<'a>(root: &Path, dir: &'a Path) -> Option<&'a str> {
    if dir.parent() != Some(root) {
        return None;
    }
    dir.file_name()?.to_str()
}

mod issued {
    //! The append-only set the suffix walk's soundness rests on, held by the compiler.
    //!
    //! [`super::Conversations::dir`] starts its walk from a hint and climbs until it finds a free
    //! name, which **skips every candidate below the hint**. That is sound only because a name, once
    //! handed out, can never be released: each skipped candidate was taken when it was checked and
    //! nothing can untake it. So the correctness of reading one field depends on another field never
    //! gaining a removal — a cross-function ownership contract, which `~/repos/CLAUDE.md` says goes
    //! in a guard type and not in prose.
    //!
    //! This module is what makes the counterexample fail to compile. The `BTreeSet` is private to
    //! this module rather than merely private to the struct, because a sibling in the parent module
    //! can reach a tuple field: `self.used.0.remove(..)` compiles from `Conversations` if the type
    //! is declared beside it — measured, `error[E0616]`. Same shape and same reason as `exif`'s
    //! `library` module.
    //!
    //! **What is left, stated so nobody reads more into this than it holds.** The rejection is from
    //! OUTSIDE this module only. An edit inside it can add a `remove`, or a second method that hands
    //! the set out, and the walk goes unsound with nothing objecting — so this narrows the blast
    //! radius to the few lines below rather than closing the hole. That is the same concession
    //! `exif`'s `library` module makes about its own five path-inferred entry points, and it is the
    //! house style because it is the truth: both bottom out in "subverting it is new code visible in
    //! a diff", and the difference between a guard type and a comment is how much code a reviewer
    //! has to read carefully, not whether the property can be broken at all.

    use std::collections::BTreeSet;

    /// Names already handed out, compared ascii-case-insensitively.
    ///
    /// Folding lives here rather than at each call site so the comparison cannot be done two ways:
    /// `Friend` and `friend` are one name to a filesystem that folds case, and treating them as two
    /// is what merged them into a single directory before decision 11's cross-platform rule was
    /// applied to this walk.
    #[derive(Default)]
    pub(super) struct IssuedNames(BTreeSet<String>);

    impl IssuedNames {
        /// Records `name` as taken, answering whether it was still free.
        ///
        /// The only method, deliberately: every operation this type does not expose is one the walk
        /// cannot be broken by. A reader is what a future caller would reach for first, and there is
        /// no caller for one — `claim`'s own answer is the only question anyone asks.
        pub(super) fn claim(&mut self, name: &str) -> bool {
            self.0.insert(name.to_ascii_lowercase())
        }
    }
}

/// The characters a cleaned name may keep: ascii alphanumerics, `-`, `_` and `.`.
///
/// An allowlist, and a narrow one. Every conversation key the export has been observed to write is a
/// Snapchat handle or a dashed uuid, both of which survive it whole; anything else is text this
/// build has no reason to reproduce in a path and every reason not to.
fn portable(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// One directory per conversation key, with collisions broken by position in the plan.
///
/// Two distinct keys can clean to one name — `a/b` and `a?b` both become `a_b` — and the second one
/// to appear gets `_2`, the third `_3`: **the suffix is a position in this plan, never the next free
/// name on disk**, so a resumed run recomputes the same answer instead of asking a half-written
/// output tree what is left.
///
/// That used to read "exactly as [`Plan::build`] breaks an output-name collision, and for exactly
/// the same reason", and decision 52 retired the comparison rather than this rule. Both planners now
/// break an output-NAME collision through [`super::local_fix::Outputs`], which asks the manifest
/// first and only falls back to a position; a directory name still has nothing above it to ask, so
/// the position is the whole answer here.
///
/// The residual that shape carries, stated because it is real: an item leaving the export shifts the
/// positions after it, so two keys that collided can swap names between runs. The manifest records
/// where a finished item actually landed, so a resume still verifies the right file; what moves is
/// where a NEW run would put an unfinished one.
/// **Both maps are keyed on the ascii-lowercased name while the name handed out keeps its own
/// case**, and the split is the whole point. `Friend` and `friend` clean to two distinct strings, so
/// a case-sensitive collision check calls them two directories — true on ext4 and **false on APFS
/// and NTFS**, which fold case, where they are one. Decision 11 is cross-platform, so the merge is a
/// real outcome and not a curiosity.
///
/// The merge used to cost a second thing on top of that, and decision 52 took it away: [`plan`]'s
/// output-name collision map was keyed case-DISTINCTLY, so two files landing on one
/// `YYYYMMDD_HHMMSS.<ext>` inside a folded directory each took the plain name and one overwrote the
/// other — two `Done` rows over one file, which [`crate::export::manifest::Manifest::resume`] then
/// demoted and rewrote alternately, for ever, which is the oscillation class this repo has already
/// shipped once. [`super::local_fix::Outputs`] folds its own claim set now, so the merge would cost
/// the grouping alone. **That is not a reason to stop folding here**: two threads sharing one
/// directory is the user-visible half and it is the half this type owns.
///
/// Folding only the KEY is what keeps `Friend` readable as `Friend` while making its neighbour
/// `friend_2`. Pinned by `two_keys_differing_only_in_case_get_two_directories_everywhere`.
///
/// **Names are assigned in sorted KEY order, not in the order items happen to arrive**, which
/// [`plan`] arranges by draining every conversation key through [`Self::dir`] before it plans a
/// single item. The difference matters because renaming a DIRECTORY is not the same class of harm as
/// renaming a file: under arrival order, one item leaving the export could change which of two
/// colliding conversations was seen first, swapping their two directories — and a resumed run would
/// then file a conversation's remaining media into another conversation's tree, with decision 44a's
/// grouping quietly no longer holding. Sorted order makes the assignment a function of the key SET
/// alone, so no item can move it. Pinned by
/// `a_conversation_keeps_its_directory_when_a_neighbours_item_leaves_the_export`.
///
/// A whole CONVERSATION leaving the export moves the key set itself, which sorted order cannot
/// absorb, and that case is closed one layer above rather than here: [`RecordedDirs`] hands over the
/// directory each conversation's own manifest rows already name and [`Self::adopt`] takes it back
/// before a single name is derived.
///
/// **Its mirror is the same defect and needs the other half of that read.** A departed conversation
/// is in nobody's key set, so nothing would claim its directory, and a NEW key cleaning onto that
/// name would be derived straight into a departed thread's tree — on top of files its finished rows
/// still name. So `adopt` reserves every directory the manifest records and not only the ones this
/// run can attribute. What is left deriving a fresh name is a key the recorded tree has no directory
/// for at all. What the newcomer would then do INSIDE that tree is a second question and it is
/// answered a layer down: [`super::local_fix::Outputs`] reserves the individual output paths those
/// finished rows record, so an item is not planned onto one of them either.
struct Conversations {
    root: PathBuf,
    /// The next ordinal to try for a folded cleaned name, so a long collision run costs one lookup
    /// rather than a scan.
    next: BTreeMap<String, u32>,
    /// Every name handed out. **The authority**, and not the same question as [`Self::next`]: a key
    /// spelling `a_2` verbatim collides with the suffix minted for a second `a`, and only a set of
    /// the names actually issued can see that. [`Self::next`] is a starting hint; this decides.
    ///
    /// [`issued::IssuedNames`] is why it is not a bare `BTreeSet` — see that module for the property
    /// the walk depends on and why it is a type rather than a sentence.
    used: issued::IssuedNames,
    assigned: BTreeMap<ConversationId, PathBuf>,
}

impl Conversations {
    fn new(root: PathBuf, recorded: &RecordedDirs) -> Self {
        // Claimed before any real key can be, which is what makes a key cleaning to it suffix away
        // rather than merge into the bucket for the files no message names. `claim` folds it like
        // every other name, so a key shouting `_NO-CONVERSATION` cannot take the bucket on a
        // filesystem that folds case either.
        let mut used = issued::IssuedNames::default();
        used.claim(NO_CONVERSATION_DIR);
        let mut conversations = Self { root, next: BTreeMap::new(), used, assigned: BTreeMap::new() };
        conversations.adopt(recorded);
        conversations
    }

    /// Takes back the directory each conversation's own manifest rows already name, then reserves
    /// every other directory the manifest records.
    ///
    /// Its position between the two claims around it is the whole of what it has to get right.
    /// [`NO_CONVERSATION_DIR`] is claimed BEFORE this, so a record naming the reserved bucket — a row
    /// an earlier build's tree shape wrote, or a hand-edited one — suffixes away from it exactly as a
    /// key spelling it does, instead of merging a real thread's media into the name that means "no
    /// thread". And everything here is claimed BEFORE [`Self::dir`] derives anything, so a key whose
    /// cleaned name spells an adopted or reserved one verbatim cannot land on top of it.
    ///
    /// Through [`issued::IssuedNames::claim`] rather than around it, for the reason that type folds
    /// at all: a name compared case-sensitively would hand `Friend` back to one conversation while
    /// leaving `friend` free for the next, which is one directory on APFS and NTFS.
    ///
    /// **The candidates are walked in `Ord` order and the first ADOPTABLE one is taken**, not the
    /// lowest — see [`RecordedDirs`] for what reducing to a minimum before the filter costs. A
    /// candidate is adoptable when it is a direct child of this run's chat root and its name is still
    /// free; a conversation whose every candidate is taken derives a fresh name, which is also the
    /// tie-break when two conversations were recorded under one directory (the first in sorted key
    /// order keeps it).
    ///
    /// **A record under another output root is not adoptable.** It names a directory this run is not
    /// writing into, so the ordinal it carries is a collision suffix for a collision the new root
    /// does not have. That is the whole of the reason: nothing here is a claim about what the resume
    /// sweep then does with those rows, which it measurably does not do — [`Manifest::resume`] hashes
    /// each row at its RECORDED path, so an old output tree still on disk verifies and those rows are
    /// never handed back as work.
    ///
    /// **The reservation pass claims and assigns nothing**, which is what makes it cheap enough to
    /// run over every recorded directory: it costs a departed conversation's neighbour a suffix and
    /// can never hand anybody a tree.
    ///
    /// **This is a second route to a path component under the output root, and it does not pass
    /// [`dir_name`].** What contains it is [`child_name`]: `Path::file_name` yields one `Normal`
    /// component, never `..` and never a root, so nothing joined here can leave the tree — and the
    /// candidate is required to equal a child of the root this run already computed. What it does not
    /// do is re-run the cleaner, so a name `dir_name` would have refused (a [`RESERVED_STEMS`] device
    /// name, one past [`MAX_DIR_NAME`]) can come back out of a row another build or a hand edit
    /// wrote. Conceded rather than closed: re-cleaning is not idempotent over an ordinal suffix, so
    /// it would rename the very directory this exists to keep.
    fn adopt(&mut self, recorded: &RecordedDirs) {
        for (conversation, candidates) in &recorded.named {
            for dir in candidates {
                let Some(name) = child_name(&self.root, dir) else { continue };
                if !self.used.claim(name) {
                    continue;
                }
                self.assigned.insert(conversation.clone(), self.root.join(name));
                break;
            }
        }
        for dir in &recorded.occupied {
            if let Some(name) = child_name(&self.root, dir) {
                // Already-claimed is the ordinary answer here: every adopted name is in this set too.
                self.used.claim(name);
            }
        }
    }

    fn dir(&mut self, conversation: &ConversationId) -> PathBuf {
        if let Some(dir) = self.assigned.get(conversation) {
            return dir.clone();
        }
        let cleaned = dir_name(conversation.as_str());
        let folded = cleaned.to_ascii_lowercase();
        let mut ordinal = self.next.get(&folded).copied().unwrap_or_default();
        // Terminates: every iteration raises the ordinal, the names it spells are all distinct, and
        // `used` is finite, so a free one is reached in at most `used.len() + 1` steps.
        let name = loop {
            let candidate = if ordinal == 0 { cleaned.clone() } else { format!("{cleaned}_{}", ordinal + 1) };
            ordinal += 1;
            if self.used.claim(&candidate) {
                break candidate;
            }
        };
        self.next.insert(folded, ordinal);
        let dir = self.root.join(name);
        self.assigned.insert(conversation.clone(), dir.clone());
        dir
    }
}

/// Works out what a chat-media run would write, without writing anything.
///
/// Reads the source files' own metadata for the items whose time falls back to it, which is the one
/// piece of I/O here and is best-effort for the reason [`Plan::build`] gives: a file that cannot be
/// read at plan time drops to the filename day and the real failure is reported when the fix step
/// reaches it, rather than one bad file taking down the whole plan.
///
/// `mode` is decision 44b, and it changes the plan rather than the pass — see [`OverlayMode`].
///
/// `recorded` is where this export's conversations have been landing so far, and a caller with no
/// manifest to read one out of passes [`RecordedDirs::default`] to get a first run's answer. It is
/// read in a window with an edge on each side and [`Plan::build`] states both: AFTER
/// [`Reconciliation::enroll`], whose `reset` clears the record of a row whose file came back, and
/// BEFORE [`super::local_fix::run`]'s resume sweep, which clears the record of an output the user
/// deleted. A cleared record names no directory at all, either way.
///
/// **What that ordering is worth is smaller than the sentence above used to claim**, and the reason
/// is the same one [`Plan::build`] gives at its own seed. A conversation whose record is cleared
/// re-derives from the key set, and where nothing collided that derivation answers with the very
/// name the record held — so the two coincide and the ordering is unobservable. They separate only
/// where the cleaned name collided, which is where the ordinal in the record is not what a fresh
/// derivation would produce. What IS pinned is the read itself:
/// `a_conversation_that_outlives_its_neighbour_keeps_its_own_directory` in
/// `tests/chat_media_screen.rs` drives [`super::chat_run::run`] and reds when this seed is
/// defaulted. THIS layer's position against either edge is pinned by nothing: both edge pins live one
/// component down on the item ordinal, where a collision is ordinary rather than merely possible.
/// Corrected here rather than left standing, because this file states the sharper rule two paragraphs
/// up and a reader meeting both takes the looser one as current.
///
/// **A mode no longer moves an output PATH, and that is a change rather than a fact that was always
/// true.** It used to: `local_fix`'s pass-through predicate folded in `SourceMedia::overlay`, which
/// [`OverlayMode::Originals`] withholds, so a PNG main that pairs came out `.png` under `originals`
/// and `.jpg` under the other two. Task 45 sends a composited alpha-capable main to PNG as well, so
/// [`local_fix::needs_its_own_format`] reads the extension alone and all three modes land at the same
/// NAME. What a mode still decides is the file at that name — `originals` withholds the layer, so
/// the pass copies the main byte for byte while `merged` and `both` burn the caption in — and, for
/// two of the three, the copies kept beside it. Unreachable from the observed export either way,
/// where only the zip family pairs and every zip main is a video.
///
/// The per-ITEM ordinal is answered the same way one layer down, and decision 52 is what made it so.
/// [`super::local_fix::Outputs`] hands an item back the path its own row already records and reserves
/// every path any row records before one is derived, so `recorded` seeds both layers off one read.
/// Until 2026-08-09 this layer re-derived: two items landing in one directory on one second took
/// `<stem>.<ext>` and `<stem>_2.<ext>`, the first left the export while the second was still owed
/// work, and the second was then planned onto `<stem>.<ext>` and wrote over the first's finished
/// output. The `_no-conversation` bucket is what made that ordinary rather than rare — 6413 items
/// share one directory and every one whose date falls all the way through to the filename takes
/// `YYYYMMDD_000000`.
#[must_use]
pub fn plan(reconciliation: &Reconciliation, out_root: impl AsRef<Path>, mode: OverlayMode, recorded: &RecordedDirs) -> Plan {
    let chat_root = out_root.as_ref().join(CHAT_DIR);
    let no_conversation = chat_root.join(NO_CONVERSATION_DIR);
    // The chat root rather than the out root, so a record this leg could not have written — a
    // memories-tree path, a path under an older out root — neither adopts nor reserves. Same root
    // the directory layer contains against, one component up from where it checks.
    let mut outputs = Outputs::new(chat_root.clone(), &recorded.outputs);
    let mut conversations = Conversations::new(chat_root, recorded);

    // Every conversation key the reconciliation names, in sorted order, assigned before a single
    // item is planned. A `BTreeSet` iterates in `Ord` order, which is what makes the assignment a
    // function of the key SET rather than of which item happened to be reached first — see
    // [`Conversations`] for what arrival order costs when an item leaves the export.
    let keys: BTreeSet<&ConversationId> =
        reconciliation.items.iter().filter_map(|item| item.message()).map(|message| &message.conversation).collect();
    for key in keys {
        conversations.dir(key);
    }

    let mut items = Vec::new();
    let mut deferred = Vec::new();
    let mut excluded = Vec::new();

    for item in &reconciliation.items {
        // Decision 44d, and it comes first so a thumbnail is never deferred over its format or
        // dropped over its date: whatever else is true of it, this build writes nothing from it.
        if item.media.file.token == Token::Thumbnail {
            excluded.push(item.source_id().to_owned());
            continue;
        }
        let mut defer = |reason| deferred.push(Deferred { source_id: item.source_id().to_owned(), reason });

        let Some(leg) = Leg::of(&item.media.file.extension) else {
            defer(DeferralReason::UnknownFormat);
            continue;
        };
        let overlay = item.media.overlay.as_ref().map(|file| file.path.clone());
        let media = SourceMedia {
            main: item.media.file.path.clone(),
            day: item.media.file.day,
            extension: item.media.file.extension.clone(),
            // Half of the overlay-mode seam: `originals` hands the item-level pass a main alone, so
            // nothing composites and nothing re-encodes. The layer itself is not dropped — it
            // travels in `originals` below.
            overlay: if mode.composites() { overlay.clone() } else { None },
        };
        let Some(capture) = capture_of(item, &media, leg) else {
            defer(DeferralReason::NoCalendarDate);
            continue;
        };

        let local = capture.local();
        let dir = match item.message() {
            Some(message) => conversations.dir(&message.conversation),
            None => no_conversation.join(local.format("%Y").to_string()).join(local.format("%m").to_string()),
        };
        let stem = local.format("%Y%m%d_%H%M%S").to_string();
        // The RESOLVED extension, so the PNGs that keep their own format and the JPEGs beside them
        // key their collisions on the names they actually take. Keyed by the whole path and not by
        // the name: two conversations may each hold a file that wants `20210304_000000.jpg`, and
        // those two do not collide.
        let extension = local_fix::output_extension(leg, &media);
        let output = outputs.path(item.source_id(), &dir, &stem, extension);

        items.push(PlannedItem {
            source_id: item.source_id().to_owned(),
            // The other half of the seam. Keyed on the EXPORT's overlay rather than on the one
            // `media` ended up with, which is the whole difference between `originals` (kept, not
            // composited) and `merged` (composited, not kept). Only where the export shipped a
            // layer is there an un-merged version to lose.
            //
            // Decision 53: the two copy paths come out of the same claim set the output above did,
            // so a second item whose export filename folds onto this one's takes a `_2` here rather
            // than the same path. `media.main` and not `item.media.file.path`, so the file the fix
            // step copies and the name this claims are one value.
            originals: match (mode.keeps_originals(), overlay) {
                (true, Some(overlay)) => outputs.kept(&dir.join(ORIGINALS_DIR), &media.main, overlay),
                _ => None,
            },
            media,
            leg,
            capture,
            // See the module docs: the export states no coordinate for chat media anywhere, so
            // there is nothing to stamp and nothing for the timezone lookup to resolve.
            location: None,
            attribution: item.message().map(attribution),
            output,
        });
    }

    Plan { kind: ItemKind::ChatMedia, items, deferred, excluded }
}

/// When a chat-media file was sent, working down the chain in the module docs. `None` when no step
/// of it yields a real calendar date.
///
/// The message's date is tried and DROPPED THROUGH rather than deferred on when it names no real
/// calendar day: [`crate::export::model::Timestamp`] is range-checked and not calendar-checked, so
/// `2021-02-30` reaches here, and a message spelling one is no reason to refuse a file the other two
/// steps can still date.
fn capture_of(item: &ChatMediaItem, media: &SourceMedia, leg: Leg) -> Option<Capture> {
    if let MediaDate::Message(created) = item.date()
        && let Some(utc) = local_fix::calendar(created)
    {
        return Some(Capture::from_message(utc));
    }
    // The file's own idea of when it was taken, read the one way the whole crate reads one. No
    // coordinate is passed, so an MP4 header time stays UTC with the offset saying so.
    if let Some(capture) = local_fix::embedded(leg, media, None) {
        return Some(capture);
    }
    Capture::from_day(media.day)
}

/// What decision 44c puts in the output's own metadata: who sent it, and which thread it arrived in.
///
/// Carried whole. The length bound belongs to the JPEG sink and lives in
/// [`crate::export::exif::Jpeg::stamp`], because the ceiling is the APP1 segment's and the video leg
/// has no equivalent — see [`Attribution`] for why capping the model type would be wrong.
fn attribution(message: &Message) -> Attribution {
    Attribution {
        sender: message.from.clone(),
        // Empty is absence, the same rule the model layer applies everywhere else.
        // `ConversationId::new` accepts `""` because the thread behind an empty key still holds its
        // records, but an empty string written into a metadata field is noise rather than a fact.
        conversation: (!message.conversation.as_str().is_empty()).then(|| message.conversation.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Conversations, NO_CONVERSATION_DIR, RecordedDirs, dir_name};
    use crate::export::model::ConversationId;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// The one root every assertion below joins onto.
    const CHAT_ROOT: &str = "/out/chat";

    fn assigned(keys: &[&str]) -> Vec<PathBuf> {
        assigned_with(&RecordedDirs::default(), keys)
    }

    /// [`assigned`] for a run that already has somewhere to put some of these conversations.
    fn assigned_with(recorded: &RecordedDirs, keys: &[&str]) -> Vec<PathBuf> {
        let mut conversations = Conversations::new(PathBuf::from(CHAT_ROOT), recorded);
        keys.iter().map(|key| conversations.dir(&ConversationId::new(*key))).collect()
    }

    /// What [`RecordedDirs::read`] builds, without a manifest to build it out of.
    ///
    /// `named` rows are `(conversation, directory)` and reach both halves exactly as a row does;
    /// `absent` are directories the manifest records for no conversation this run names, which is
    /// what a departed thread leaves behind.
    fn recorded(named: &[(&str, &str)], absent: &[&str]) -> RecordedDirs {
        let mut dirs = RecordedDirs::default();
        for (key, dir) in named {
            dirs.named.entry(ConversationId::new(*key)).or_default().insert(PathBuf::from(*dir));
            dirs.occupied.insert(PathBuf::from(*dir));
        }
        dirs.occupied.extend(absent.iter().map(PathBuf::from));
        dirs
    }

    #[test]
    fn a_key_that_cleans_to_the_reserved_bucket_is_suffixed_away_from_it() {
        assert_eq!(dir_name(NO_CONVERSATION_DIR), NO_CONVERSATION_DIR, "cleaning alone does not move it");
        // The bucket name is claimed before any key can be, so the key that spells it lands beside
        // the bucket rather than inside it.
        assert_eq!(assigned(&[NO_CONVERSATION_DIR]), [Path::new("/out/chat/_no-conversation_2")]);
    }

    #[test]
    fn a_key_spelling_a_minted_suffix_does_not_land_on_it() {
        // `a` and `a.` both clean to `a`, so the second takes the minted `a_2`. The third key IS
        // `a_2` and cleans to itself, which the ordinal counter alone cannot see: its count for
        // `a_2` is zero, so it would hand out the directory the second key is already using.
        assert_eq!(assigned(&["a", "a.", "a_2"]), [Path::new("/out/chat/a"), Path::new("/out/chat/a_2"), Path::new("/out/chat/a_2_2")]);
    }

    #[test]
    fn one_key_asked_for_twice_keeps_one_directory() {
        assert_eq!(assigned(&["friend", "friend"]), [Path::new("/out/chat/friend"), Path::new("/out/chat/friend")]);
    }

    /// Case-sensitivity is a property of the filesystem, not of the string, so a collision check that
    /// compares case-sensitively is right on ext4 and wrong on APFS and NTFS. Decision 11 is
    /// cross-platform. The assertion folds the issued names and counts them, because that is what the
    /// filesystem does — comparing them as Rust strings is the exact check that misses this.
    #[test]
    fn two_keys_differing_only_in_case_get_two_directories_everywhere() {
        let dirs = assigned(&["Friend", "friend", "FRIEND"]);
        let folded: BTreeSet<String> =
            dirs.iter().filter_map(|dir| dir.file_name()).map(|name| name.to_string_lossy().to_ascii_lowercase()).collect();
        assert_eq!(folded.len(), 3, "two of these share one directory once a filesystem folds case: {dirs:?}");

        // The fold is on the bookkeeping only: the name a user sees keeps the case the export wrote,
        // so this must not quietly become a lowercasing pass.
        assert_eq!(dirs[0].file_name().and_then(|name| name.to_str()), Some("Friend"));
        assert_eq!(dirs[2].file_name().and_then(|name| name.to_str()), Some("FRIEND_3"));
    }

    /// An adopted directory is only meaningful under the root it was recorded beneath: elsewhere its
    /// ordinal is a suffix for a collision that root does not have. The second half is the control —
    /// without it, an `adopt` that never adopted anything would read green.
    #[test]
    fn a_directory_recorded_under_another_out_root_is_not_adopted() {
        assert_eq!(assigned_with(&recorded(&[("a?b", "/elsewhere/chat/a_b_2")], &[]), &["a?b"]), [Path::new("/out/chat/a_b")]);
        assert_eq!(assigned_with(&recorded(&[("a?b", "/out/chat/a_b_2")], &[]), &["a?b"]), [Path::new("/out/chat/a_b_2")]);
    }

    /// The lowest ADOPTABLE candidate, not the lowest candidate: one this run cannot take has to fall
    /// through to the next rather than stand in for the conversation, which would drop the adoption
    /// and send it back to deriving from the key set.
    ///
    /// Both fixtures put the unadoptable candidate FIRST in `Ord` order, which is the only place it
    /// does harm — `_` (0x5F) sorts below `f` (0x66), and `/a` below `/out`. The second needs no
    /// forged row at all: it is one conversation with rows under an old output root and the live one.
    #[test]
    fn an_unadoptable_record_falls_through_to_the_next_candidate() {
        let bucketed =
            recorded(&[("friend-handle", "/out/chat/_no-conversation/2021/03"), ("friend-handle", "/out/chat/friend-handle_7")], &[]);
        assert_eq!(assigned_with(&bucketed, &["friend-handle"]), [Path::new("/out/chat/friend-handle_7")]);

        let stale = recorded(&[("friend-handle", "/a/chat/friend-handle_7"), ("friend-handle", "/out/chat/friend-handle_7")], &[]);
        assert_eq!(assigned_with(&stale, &["friend-handle"]), [Path::new("/out/chat/friend-handle_7")]);
    }

    /// A conversation that left the export is in nobody's key set, so nothing adopts its directory —
    /// and a new key cleaning onto that name would be derived straight into its tree, on top of files
    /// its finished rows still name. Reserving every recorded directory costs the newcomer a suffix.
    #[test]
    fn a_departed_conversations_directory_is_not_handed_to_a_new_key() {
        let left_behind = recorded(&[("a?b", "/out/chat/a_b_2")], &["/out/chat/a_b"]);
        assert_eq!(assigned_with(&left_behind, &["a?b", "a:b"]), [Path::new("/out/chat/a_b_2"), Path::new("/out/chat/a_b_3")]);
    }

    /// The reserved bucket is claimed before a single record is adopted, so a row naming it loses
    /// the same way a key spelling it does.
    ///
    /// Unreachable from a directory THIS build wrote — no real key can be handed the bucket name —
    /// and reachable from the store, which outlives the build that filled it.
    ///
    /// The third key is adopted, and it is what separates this from a run that adopted nothing at
    /// all: with only the first two, both expected values coincide with no-adoption and the test
    /// carries no evidence a record reached [`Conversations::dir`].
    #[test]
    fn a_record_naming_the_reserved_bucket_cannot_hand_it_to_a_conversation() {
        let forged = recorded(&[("friend", "/out/chat/_no-conversation"), ("other", "/out/chat/other_4")], &[]);
        assert_eq!(
            assigned_with(&forged, &["friend", NO_CONVERSATION_DIR, "other"]),
            [Path::new("/out/chat/friend"), Path::new("/out/chat/_no-conversation_2"), Path::new("/out/chat/other_4")]
        );
    }

    /// Every adopted name is claimed before any name is derived, and `used` is what sees it: the
    /// ordinal hint for `a_b_2` is zero, so a walk reading only that would hand out the directory the
    /// adopted conversation is already using.
    #[test]
    fn a_key_spelling_an_adopted_name_does_not_land_on_it() {
        let kept = recorded(&[("a?b", "/out/chat/a_b_2")], &[]);
        assert_eq!(
            assigned_with(&kept, &["a?b", "a_b_2", "a/b"]),
            [Path::new("/out/chat/a_b_2"), Path::new("/out/chat/a_b_2_2"), Path::new("/out/chat/a_b")]
        );
    }

    /// The adoption claims through the same fold every derived name does. Pinned here as well as at
    /// the derivation, because a fold dropped from `adopt` alone leaves both case tests above green.
    #[test]
    fn an_adopted_name_is_claimed_through_the_case_fold() {
        let kept = recorded(&[("A", "/out/chat/Friend")], &[]);
        assert_eq!(assigned_with(&kept, &["A", "friend"]), [Path::new("/out/chat/Friend"), Path::new("/out/chat/friend_2")]);
    }

    #[test]
    fn a_shouted_key_cannot_take_the_no_conversation_bucket_either() {
        // Same hole one layer over: the bucket is seeded lowercase, so a key that folds onto it has
        // to suffix away from it rather than merge into it on a case-folding filesystem.
        let dirs = assigned(&["_NO-CONVERSATION"]);
        assert_eq!(dirs[0].file_name().and_then(|name| name.to_str()), Some("_NO-CONVERSATION_2"));
    }
}
