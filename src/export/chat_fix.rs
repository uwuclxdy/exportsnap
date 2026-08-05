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
//! The chain is the memories one with its first step swapped: the message's `Created`, then the
//! file's own embedded timestamp, then the filename day at midnight.
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
use std::path::{Path, PathBuf};

use crate::export::chat_media::{ChatMediaItem, MediaDate, Message, Reconciliation, Token};
use crate::export::local_fix::{self, Capture, DeferralReason, Deferred, Leg, Plan, PlannedItem, SourceMedia};
use crate::export::manifest::ItemKind;
use crate::export::model::{Attribution, ConversationId};

/// The level every chat-media output sits under, decision 46a.
const CHAT_DIR: &str = "chat";

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
    //! is declared beside it. Same shape and same reason as `exif`'s `library` module.

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
/// to appear gets `_2`, the third `_3`, exactly as [`Plan::build`] breaks an output-name collision
/// and for exactly the same reason: **the suffix is a position in this plan, never the next free
/// name on disk**, so a resumed run recomputes the same answer instead of asking a half-written
/// output tree what is left.
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
/// The merge is only the first half of what that would cost. [`plan`]'s output-name collision map is
/// keyed on the joined path, which stays case-DISTINCT, so two files landing on one
/// `YYYYMMDD_HHMMSS.<ext>` inside a folded directory would each take the plain name and one would
/// overwrite the other — leaving two `Done` rows pointing at one file, which
/// [`crate::export::manifest::Manifest::resume`] then demotes and rewrites alternately, for ever.
/// That is the oscillation class this repo has already shipped once, arriving through the filesystem
/// instead of through an id spelling.
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
/// The case that remains, and it is routed to `docs/todo.md` rather than closed here: a whole
/// conversation leaving the export still renames the neighbour it was colliding with, because the
/// key set itself changed.
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
    fn new(root: PathBuf) -> Self {
        // Claimed before any real key can be, which is what makes a key cleaning to it suffix away
        // rather than merge into the bucket for the files no message names. `claim` folds it like
        // every other name, so a key shouting `_NO-CONVERSATION` cannot take the bucket on a
        // filesystem that folds case either.
        let mut used = issued::IssuedNames::default();
        used.claim(NO_CONVERSATION_DIR);
        Self { root, next: BTreeMap::new(), used, assigned: BTreeMap::new() }
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
#[must_use]
pub fn plan(reconciliation: &Reconciliation, out_root: impl AsRef<Path>) -> Plan {
    let chat_root = out_root.as_ref().join(CHAT_DIR);
    let no_conversation = chat_root.join(NO_CONVERSATION_DIR);
    let mut conversations = Conversations::new(chat_root);

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
    // Keyed by the whole candidate path, not by the name: two conversations may each hold a file
    // that wants `20210304_000000.jpg`, and those two do not collide.
    let mut taken: BTreeMap<PathBuf, u32> = BTreeMap::new();

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
        let media = SourceMedia {
            main: item.media.file.path.clone(),
            day: item.media.file.day,
            extension: item.media.file.extension.clone(),
            overlay: item.media.overlay.as_ref().map(|file| file.path.clone()),
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
        // The RESOLVED extension, so decision 47's copied-through PNGs and the JPEGs beside them
        // key their collisions on the names they actually take.
        let extension = local_fix::output_extension(leg, &media);
        let ordinal = taken.entry(dir.join(format!("{stem}.{extension}"))).or_default();
        let output = dir.join(local_fix::output_name(&stem, &extension, *ordinal));
        *ordinal += 1;

        items.push(PlannedItem {
            source_id: item.source_id().to_owned(),
            // Only where a composite consumed two files is there an un-merged version to keep.
            originals: media.overlay.is_some().then(|| dir.join(ORIGINALS_DIR)),
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
    use super::{Conversations, NO_CONVERSATION_DIR, dir_name};
    use crate::export::model::ConversationId;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn assigned(keys: &[&str]) -> Vec<PathBuf> {
        let mut conversations = Conversations::new(PathBuf::from("/out/chat"));
        keys.iter().map(|key| conversations.dir(&ConversationId::new(*key))).collect()
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

    #[test]
    fn a_shouted_key_cannot_take_the_no_conversation_bucket_either() {
        // Same hole one layer over: the bucket is seeded lowercase, so a key that folds onto it has
        // to suffix away from it rather than merge into it on a case-folding filesystem.
        let dirs = assigned(&["_NO-CONVERSATION"]);
        assert_eq!(dirs[0].file_name().and_then(|name| name.to_str()), Some("_NO-CONVERSATION_2"));
    }
}
