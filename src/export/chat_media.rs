//! Chat media: the files a Snapchat export leaves in its `chat_media` dirs, the `Media IDs` tokens
//! `chat_history.json` names, and the join between the two.
//!
//! Nothing here is bucketed by date, and that is the whole difference from [`super::memories`]. A
//! chat-media filename carries an id, so the join is a string equality and the pairing is a stem
//! match — no arbitrary pairing, no [`super::memories::Pairing::Ambiguous`] analogue, because a
//! stem either matches or it does not.
//!
//! **The filenames are two families, not one pattern with a role vocabulary.** All 9465 files
//! across the observed export's three `chat_media` dirs fall into exactly these, with nothing left
//! over and no basename repeated between dirs:
//!
//! - **plain**, `YYYY-MM-DD_<token>~<id>.<ext>`, 8537 files. [`Token::B`] is 8005 of them and
//!   **carries no overlay concept at all** — every id is distinct and no second file ever shares
//!   one. The role-worded remainder is `media` 264, `overlay` 224, `thumbnail` 44.
//! - **zip**, `YYYY-MM-DD_<role>~<word>-<digits>.zip.<hash>.<ext>`, 928 files, `media` 464 and
//!   `overlay` 464.
//!
//! Only the zip family pairs, and it pairs perfectly: 464 of 464 stems match on the day and the mid
//! with the role word swapped. The role-worded plain family pairs on **nothing** — its `media`,
//! `overlay` and `thumbnail` id sets are pairwise disjoint, with zero exact matches and zero shared
//! prefixes at 8, 16, 24, 32 or 40 characters — so all 224 plain overlays land in
//! [`Discovery::unmatched_overlays`] and no heuristic is run to rescue them. There is no ambiguity
//! here for a date bucket or a thumbnail diff to resolve, and discovery stays a filename-and-listing
//! operation that opens no file.
//!
//! `chat_history.json` is the only json that references a media file and it reaches 27% of them.
//! Its `Media IDs` is a `" | "`-separated list of `b~<id>` tokens — the prefix is always the literal
//! `b`, so a token IS a plain-[`Token::B`] file's stem. Of the observed export's 2611 tokens, 2588
//! join to a file and 23 do not; in the other direction 5417 of the 8005 `b` files are named by no
//! message, and the 532 role-worded plus 928 zip files are named by nothing at all. All three states
//! are recorded — [`Join::Named`], [`Join::Unnamed`], [`Join::NoToken`] — because "did not join" and
//! "cannot be joined" are different facts about a file, and the 23 tokens with no file become
//! manifest rows of their own the way memories' 90 entries do rather than a number in a summary.
//!
//! **What a join carries is separate from which message it found.** [`Join::Named`] holds a
//! [`Message`]: the conversation key, the thread's title, the sender, the direction, and the
//! `Created` instant, all copied onto the item. The position is kept alongside them
//! ([`Message::at`]) for anything not carried, and it is deliberately not the only thing kept. A
//! position is meaningful only against the exact [`ChatHistory`] this join ran against, and an
//! in-range stale one reads back the WRONG message without failing — which, for the build that
//! names an output directory after the conversation, files a stranger's media under a friend's
//! name. So the conversation travels as the export's own [`ConversationId`] key rather than as an
//! index into a list that has to be passed around beside it, and the human `Conversation Title`
//! travels next to it rather than instead of it: a title is written per message, so a group renamed
//! mid-thread carries two of them under one key, and only the key is stable enough to name a
//! directory after.
//!
//! **A file no message names is dated by the day in its own filename.** [`ChatMediaItem::date`]
//! answers with a [`MediaDate`], and its two variants are the two different facts rather than one
//! field a caller has to remember the provenance of: `Message` for the 2588 files a message names,
//! `Filename` for the other 6877 — 6413 of them as ITEMS, a zip pair's overlay half riding on the
//! media it pairs with rather than answering for itself. The census makes the fallback sound and
//! also bounds it: for all 2588 matched files the filename's `YYYY-MM-DD` equals the message's
//! `Created` date, which establishes the day and says nothing whatever about the time of day.
//!
//! What this module will not do is collapse those two into one instant. The memories leg's chain is
//! the entry's time, then the file's own embedded timestamp, then the filename day at midnight, and
//! the middle step opens the file — which discovery here never does. Resolving to a single value
//! would also pre-empt that middle step: a stamping pass would have to unpick this module's answer
//! to slot the embedded read in above it, and the two legs would disagree about a chain they should
//! share. Rejected alternative: a resolved instant with a `TimeSource` tag beside it, midnight
//! standing in for the unjoined files' unknown time. It reads better at one call site, at the cost
//! of inventing a time of day inside the layer whose whole job is to report what the export says.
//!
//! The record's OTHER date, `Created(microseconds)`, IS carried beside its `Created`, and is the
//! second source [`ChatMediaItem::date`] tries. What blocked it was never the date rule but the
//! type: a bare `i64` under `#[serde(default)]` hands an absent key back as `0`, a well-formed 1970
//! instant nothing above the deserializer can tell from a stated one, so carrying it would have
//! promoted a missing field into a message-stated date outranking the filename day. Making the
//! absence expressible at the schema boundary — `Option<i64>` on `ChatEntry` and `SnapEntry` alike,
//! with `0` read as the field's own empty spelling the way `""` is for `Created` — removes the
//! reason rather than working around it. The full account is on [`Message::created_epoch_ms`].
//!
//! Framework-free like the rest of `export/`: nothing here writes an output file, composites an
//! overlay, or knows a screen exists. Where the output lands is a later task's question.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::export::manifest::{ItemKind, ItemStatus, Manifest, ManifestError, NewItem};
use crate::export::model::{ChatHistory, ConversationId, Timestamp, Username};
use crate::export::walk::{Walk, walk};

pub use crate::export::memories::Day;
pub use crate::export::walk::UnreadableDir;

/// The directory name media discovery walks for, at any depth under the source root.
///
/// It recurs at several paths in the one observed export, so this is a search, not a fixed
/// location.
const CHAT_MEDIA_DIR: &str = "chat_media";

/// What the zip family spells between its mid and its hash.
const ZIP_INFIX: &str = ".zip.";

/// The delimiter `chat_history.json` puts between two `Media IDs` tokens.
///
/// The observed spelling is `" | "`; splitting on the bar and trimming reads that and any spacing
/// variant of it. Every id in the one observed export is ascii-alphanumeric, so no observed token
/// contains a bar — but `Media IDs` is untrusted json and that is an observation, not a rule this
/// build may lean on, so here is what happens when one does. `"b~ab|cd"` splits into `"b~ab"` and
/// `"cd"`; the second fails [`parse_history_token`] and is surfaced as an
/// [`UnparsedToken`], while the first is a well-formed token that could join to a real file no
/// message actually named. That residual is accepted rather than fixed: reading a bar as a
/// delimiter is the only reading the observed export supports, and the alternative — treating a
/// whole `Media IDs` value as one token — would drop every genuine multi-token row, of which the
/// export holds 1233.
const MEDIA_ID_SEPARATOR: char = '|';

/// The tokens a `Media IDs` value names, split by the shared delimiter.
///
/// Splitting on the bar and trimming reads the observed `" | "` and any spacing variant of it, and
/// drops the empty tokens a separator-only row produces. `reconcile`'s join and the history writer's
/// html links use this same rule through this one function, so the delimiter has one spelling in the
/// crate rather than two.
pub fn media_tokens(media_ids: &str) -> impl Iterator<Item = &str> {
    media_ids.split(MEDIA_ID_SEPARATOR).map(str::trim).filter(|token| !token.is_empty())
}

// ---- the filename grammar ----

/// The word a chat-media filename spells before the `~`.
///
/// The four below are the whole observed vocabulary. **`metadata~` is not among them**: an earlier
/// reading of the export listed it as a role and the census found zero files, so a name spelling it
/// lands in [`Discovery::unparsed`] where someone will see it rather than in a role that models
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Token {
    /// `b`, 8005 of the observed export's 8537 plain files and the only token any json names.
    /// Never an overlay and never half of a pair.
    B,
    /// `media`, 264 plain files (all mp4) and 464 zip files.
    Media,
    /// `overlay`, 224 plain files (png 117, webp 107) and 464 zip files. Only the zip half ever
    /// pairs with anything.
    Overlay,
    /// `thumbnail`, 44 plain files, all jpg. Pairs with nothing and is named by nothing.
    Thumbnail,
}

impl Token {
    pub const ALL: [Self; 4] = [Self::B, Self::Media, Self::Overlay, Self::Thumbnail];

    /// The word the filename spells.
    #[must_use]
    pub const fn as_word(self) -> &'static str {
        match self {
            Self::B => "b",
            Self::Media => "media",
            Self::Overlay => "overlay",
            Self::Thumbnail => "thumbnail",
        }
    }

    /// Whether a file with this token is a layer drawn over another file rather than media of its
    /// own.
    #[must_use]
    pub const fn is_overlay(self) -> bool {
        matches!(self, Self::Overlay)
    }

    /// Ascii-case-insensitive, matching [`super::memories::Role::parse`]. Every observed token is
    /// lowercase and [`Self::as_word`] is what an id is rebuilt from, so a shouted spelling
    /// normalizes rather than forking the identity.
    fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|token| raw.eq_ignore_ascii_case(token.as_word()))
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_word())
    }
}

/// Which of the two filename families a name belongs to, with the parts that family carries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    /// `<token>~<id>`: 8537 of the observed export's 9465 files.
    Plain {
        /// Ascii-alphanumeric, 46 or 51 characters under [`Token::B`] and 36 or 40 under the
        /// role words. Held verbatim, with no length check: the `~` and the token vocabulary
        /// already tell this family apart from the zip one, so a length rule would only cost a
        /// real file the day Snapchat picks a new one.
        id: String,
    },
    /// `<word>-<digits>.zip.<hash>`: 928 files, and the only family that pairs.
    Zip {
        /// `<word>-<digits>`, the half a pair shares. The 8-character word is one constant value
        /// across all 928 observed files.
        mid: String,
        /// The 7 characters after `.zip.`, identical within all 464 observed pairs.
        ///
        /// It is part of [`ChatMediaFile::id`], which makes this build's pairing key `(day, mid,
        /// hash)` where the census measured `(day, mid)`. The two agree on every observed file and
        /// differ only on a shape nothing has seen: two halves whose hashes disagree pair under the
        /// looser key and do not under this one. Refusing is the direction that cannot composite one
        /// snap's overlay onto another snap's media, and the cost of being wrong is one extra entry
        /// in [`Discovery::unmatched_overlays`]. Pinned by
        /// `zip_halves_whose_trailing_hash_differs_are_not_one_pair`.
        hash: String,
    },
}

impl Family {
    fn parse(tail: &str) -> Option<Self> {
        match tail.split_once(ZIP_INFIX) {
            Some((mid, hash)) => {
                let (word, digits) = mid.split_once('-')?;
                let shaped = is_alphanumeric_run(word) && !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
                (shaped && is_alphanumeric_run(hash)).then(|| Self::Zip { mid: mid.to_owned(), hash: hash.to_owned() })
            }
            None => is_alphanumeric_run(tail).then(|| Self::Plain { id: tail.to_owned() }),
        }
    }
}

/// Whether `text` is a non-empty run of ascii alphanumerics, which is what every id, word and hash
/// in either family is made of.
///
/// Ascii-alphanumeric rather than hex or a fixed length on purpose: these ids are opaque to this
/// crate, the export is n=1, and refusing a real filename over a character class costs a media
/// file. What the check has to do is tell the plain family's tail apart from the zip family's,
/// which the `-` and the `.` in the latter already do.
fn is_alphanumeric_run(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// One `Media IDs` token, validated into the exact spelling a plain-[`Token::B`] filename carries,
/// or `None` for anything else.
///
/// **This is a trust boundary and it exists because skipping it was a bug.** `Media IDs` is
/// arbitrary text off `chat_history.json`. Without this check a token was compared against the join
/// map and, on a miss, minted straight into a [`MissingMedia`] whose `token` becomes a manifest
/// `source_id` — so a token spelling some OTHER file's id (`overlay~<id>`, `media~<id>`, a zip stem)
/// marked a row that a present file owned, and because [`Reconciliation::enroll`] reads its parked
/// set before writing, the row then alternated `SourceMissing`/`Pending` on every other run and was
/// offered as work on neither beat. Pinned by
/// `a_message_naming_a_file_no_token_can_reach_never_parks_that_files_row`.
///
/// What the check buys, stated as the convention it is rather than the guarantee the old comment
/// claimed: **a validated token can only ever be claimed by a file that is already in the join
/// map.** A `b~<alphanumeric>` spelling is reachable only from a `(Token::B, Family::Plain)` file,
/// because every other plain token spells its own word into [`ChatMediaFile::id`] and every zip id
/// is day-prefixed and carries `.zip.`; and every such file registers in the map unconditionally.
/// The counterexample worth checking is a `b~` name in the ZIP shape —
/// `2021-03-04_b~ab-1.zip.cd.jpg` really does parse as `(Token::B, Family::Zip)` — and its id is
/// `2021-03-04_ab-1.zip.cd`, which no validated token can spell. **The compiler rejects none of
/// this**, so it is a convention pinned by tests, not an invariant: see
/// `a_b_shaped_zip_name_is_not_a_history_token`.
///
/// The prefix normalizes ascii-case exactly as [`ChatMediaFile::parse`] does, so a shouted `B~x` in
/// the history and a shouted `B~x` on disk land on ONE row rather than forking into two. The id
/// half stays verbatim on both sides, because an id is opaque and case-significant.
fn parse_history_token(raw: &str) -> Option<String> {
    let (prefix, id) = raw.split_once('~')?;
    if Token::parse(prefix) != Some(Token::B) || !is_alphanumeric_run(id) {
        return None;
    }
    Some(format!("{}~{id}", Token::B.as_word()))
}

/// One media file in a `chat_media` dir, with its name parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMediaFile {
    /// Where it sits, as the walk found it.
    pub path: PathBuf,
    /// The day the filename leads with. For all 2588 files a message names, this equals the
    /// message's own `Created` date, which is what makes it a sound date for the 6877 no message
    /// names — [`MediaDate::Filename`], where it is the answer rather than a cross-check.
    pub day: Day,
    pub token: Token,
    pub family: Family,
    /// What this file pairs on and what the manifest records it under: `<token>~<id>` for the
    /// plain family, `<day>_<mid>.zip.<hash>` for the zip one.
    ///
    /// **The plain form deliberately drops the day and the zip form deliberately keeps it.** The
    /// plain form is exactly the token `chat_history.json` names, so a token whose file is missing
    /// today and present tomorrow keeps one manifest row across both runs instead of enrolling a
    /// second one. That is one half of what [`super::memories::Reconciliation::enroll`] documents,
    /// and it is the half a shared identity can close on its own; the other — a file no message
    /// names at all — needs the manifest sweep [`Reconciliation::enroll`] runs.
    ///
    /// The zip form keeps the day because the day is part of the pairing key. The census measured
    /// `(day, mid)` — 464 of 464 pairs match on it — and this build keys on `(day, mid, hash)`,
    /// which is STRICTER: the two agree on every observed pair, since the hash is identical within
    /// all 464, and differ only where two halves disagree, where refusing to pair is the direction
    /// that cannot composite one snap's caption onto another snap's media. Nothing establishes that
    /// a mid is unique on its own, so dropping the day is not available either way.
    ///
    /// The zip form drops the ROLE word, which is what lets a pair's two halves share one id. The
    /// cost, unobserved because the observed zip roles are only `media` and `overlay`: a third
    /// non-overlay zip role on one `(day, mid, hash)` — a `thumbnail~…zip…` beside a `media~…zip…` —
    /// would collide on [`Discovery::from_walk`]'s key and surface as a [`Duplicate`] rather than
    /// pair. Reported, not dropped, which is the right failure for a shape nobody has seen.
    pub id: String,
    /// Verbatim, as the name spells it.
    pub extension: String,
}

impl ChatMediaFile {
    /// Parses either family out of `path`'s file name.
    ///
    /// `None` for any other shape, and a rejected name is carried by [`Discovery::unparsed`] rather
    /// than dropped, because a media file this build cannot read is one nobody would notice
    /// missing.
    ///
    /// # Examples
    ///
    /// ```
    /// use exportsnap::export::chat_media::{ChatMediaFile, Family, Token};
    ///
    /// let plain = ChatMediaFile::parse("/x/chat_media/2021-03-04_b~aB3xY9.jpg").unwrap();
    /// assert_eq!(plain.token, Token::B);
    /// assert_eq!(plain.id, "b~aB3xY9");
    ///
    /// let zip = ChatMediaFile::parse("/x/chat_media/2021-03-04_overlay~vantsnap-1234567.zip.a1b2c3d.png").unwrap();
    /// assert_eq!(zip.family, Family::Zip { mid: "vantsnap-1234567".to_owned(), hash: "a1b2c3d".to_owned() });
    /// assert_eq!(zip.id, "2021-03-04_vantsnap-1234567.zip.a1b2c3d");
    ///
    /// assert!(ChatMediaFile::parse("/x/chat_media/index.html").is_none());
    /// ```
    #[must_use]
    pub fn parse(path: impl AsRef<Path>) -> Option<Self> {
        let path = path.as_ref();
        let name = path.file_name().and_then(OsStr::to_str)?;
        let (stem, extension) = name.rsplit_once('.')?;
        let (day, rest) = stem.split_once('_')?;
        let day = Day::parse(day)?;
        let (token, tail) = rest.split_once('~')?;
        let token = Token::parse(token)?;
        let family = Family::parse(tail)?;
        let id = match &family {
            Family::Plain { id } => format!("{}~{id}", token.as_word()),
            Family::Zip { mid, hash } => format!("{day}_{mid}{ZIP_INFIX}{hash}"),
        };
        Some(Self { path: path.to_path_buf(), day, token, family, id, extension: extension.to_owned() })
    }

    /// The `b~<id>` token a `Media IDs` list would spell for this file, or `None` when no json can
    /// name it at all.
    ///
    /// Both halves of the guard are load-bearing. The token has to be [`Token::B`] because that is
    /// the only prefix `chat_history.json` ever writes, and the family has to be plain because
    /// [`Self::id`] means something else entirely under the zip family — a `b~` zip name is
    /// unobserved and would otherwise hand the join a day-prefixed string to look up.
    #[must_use]
    pub fn history_token(&self) -> Option<&str> {
        matches!((self.token, &self.family), (Token::B, Family::Plain { .. })).then_some(self.id.as_str())
    }
}

// ---- files on disk ----

/// One chat-media unit: a file, and the overlay the zip family pairs to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMedia {
    pub file: ChatMediaFile,
    /// `None` for every plain file, which pairs on nothing, and for a zip media file whose overlay
    /// half is absent. The 464 observed zip pairs are the only `Some`.
    pub overlay: Option<ChatMediaFile>,
}

impl ChatMedia {
    /// The manifest's `source_id` for this unit: [`ChatMediaFile::id`] of the file it leads with.
    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.file.id
    }
}

/// Two files claiming one [`ChatMediaFile::id`] and one role.
///
/// Reported rather than deduped. All 9465 observed basenames are unique across the three
/// `chat_media` dirs and every id inside a token class is distinct, so this is the shape of a
/// re-download, a half-merged copy, or an id that turns out not to be unique after all — and
/// quietly picking one would hide every one of those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    pub id: String,
    /// Whether the files claiming it are overlays. An overlay and the media it pairs with share an
    /// id under the zip family, so the id alone does not name a collision.
    pub overlay: bool,
    /// The file the pairing used: first in the order [`Discovery::from_files`] sorts into.
    pub kept: PathBuf,
    /// The ones it did not, in the same order.
    pub ignored: Vec<PathBuf>,
}

/// What a walk of the source root found in every `chat_media` dir under it.
///
/// Public fields, and [`Self::from_files`] takes the file list directly, so a caller that already
/// knows the answers can build one without a filesystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// One per media file, with its overlay where the zip family paired one. Ordered by
    /// [`ChatMedia::source_id`].
    pub media: Vec<ChatMedia>,
    /// Overlay files no media file claimed: the whole plain `overlay~` family — 224 in the observed
    /// export, which the census says pairs with nothing — plus any zip overlay whose media half is
    /// absent, of which the export holds none. Ordered by id.
    ///
    /// Kept apart from [`Self::media`] rather than mixed into it so a screen can name the fallback
    /// without re-deriving it, and enrolled all the same: an overlay is a file the run has to
    /// account for, and dropping 224 of them would be the same silent success the memories gap
    /// exists to prevent.
    pub unmatched_overlays: Vec<ChatMediaFile>,
    /// Names in a `chat_media` dir this build's grammar does not read, ordered by path.
    pub unparsed: Vec<PathBuf>,
    /// Ordered by id, then by role.
    pub duplicates: Vec<Duplicate>,
    /// Directories under the root the walk could not list, ordered by path. Reported rather than
    /// fatal — a source root on a real mount carries dirs this user cannot read and the export has
    /// nothing to do with — and while any of these stand, every absence this module reports is a
    /// lower bound.
    pub unreadable: Vec<UnreadableDir>,
}

impl Discovery {
    /// The pairing pass on its own, split from the walk so it can be driven without a filesystem.
    #[must_use]
    pub fn from_files(files: Vec<ChatMediaFile>, unparsed: Vec<PathBuf>) -> Self {
        Self::from_walk(files, unparsed, Vec::new())
    }

    /// [`Self::from_files`] plus the dirs the walk could not list.
    ///
    /// `read_dir` order is whatever the filesystem says and differs between runs and between
    /// machines, and it is kept out of both answers. The files are sorted before the map is filled,
    /// so which of several files claiming one id is kept — and which are reported as ignored — is
    /// settled by path rather than by whichever dir the walk reached first; and the results come
    /// out in id order because the maps they are built from are keyed and iterated that way.
    #[must_use]
    pub fn from_walk(files: Vec<ChatMediaFile>, mut unparsed: Vec<PathBuf>, mut unreadable: Vec<UnreadableDir>) -> Self {
        let mut kept: BTreeMap<(String, bool), ChatMediaFile> = BTreeMap::new();
        let mut duplicates: BTreeMap<(String, bool), Duplicate> = BTreeMap::new();

        for file in sorted(files) {
            let key = (file.id.clone(), file.token.is_overlay());
            match kept.get(&key).map(|first| first.path.clone()) {
                Some(first) => {
                    let duplicate = duplicates.entry(key).or_insert_with(|| Duplicate {
                        id: file.id.clone(),
                        overlay: file.token.is_overlay(),
                        kept: first,
                        ignored: Vec::new(),
                    });
                    duplicate.ignored.push(file.path);
                }
                None => {
                    kept.insert(key, file);
                }
            }
        }

        let mut leaders = Vec::new();
        let mut overlays: BTreeMap<String, ChatMediaFile> = BTreeMap::new();
        for ((id, overlay), file) in kept {
            if overlay {
                overlays.insert(id, file);
            } else {
                leaders.push(file);
            }
        }

        // A plain overlay's id spells its own token, so no media file can carry it and every one of
        // them falls through to `unmatched_overlays`. That is the census result rather than a
        // special case in the code: the role-worded family pairs on nothing.
        let media = leaders
            .into_iter()
            .map(|file| {
                let overlay = overlays.remove(&file.id);
                ChatMedia { file, overlay }
            })
            .collect();

        unparsed.sort();
        unreadable.sort_by(|left, right| left.dir.cmp(&right.dir));
        Self {
            media,
            unmatched_overlays: overlays.into_values().collect(),
            unparsed,
            duplicates: duplicates.into_values().collect(),
            unreadable,
        }
    }
}

/// Total order over the discovered files. The id and the role split every observed file, so only a
/// genuine duplicate falls through to the path.
fn sorted(mut files: Vec<ChatMediaFile>) -> Vec<ChatMediaFile> {
    files.sort_by(|left, right| (&left.id, left.token.is_overlay(), &left.path).cmp(&(&right.id, right.token.is_overlay(), &right.path)));
    files
}

/// The source root itself could not be listed.
///
/// Only ever the root; a directory underneath it that cannot be listed is [`Discovery::unreadable`]
/// instead. Named apart from [`super::memories::ScanError`] and [`super::zip::DiscoverError`] so a
/// caller reaching for more than one needs no alias.
#[derive(Debug)]
pub struct ChatScanError {
    /// The root that was being listed.
    pub dir: PathBuf,
    /// What the filesystem said.
    pub source: io::Error,
}

impl fmt::Display for ChatScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not list {} looking for the export's chat_media dirs: {}; point the source at the dir holding the extracted export parts",
            self.dir.display(),
            self.source
        )
    }
}

impl Error for ChatScanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Every media file in every dir named `chat_media` under `root`, at any depth, paired up.
///
/// # Errors
///
/// Returns [`ChatScanError`] when `root` cannot be listed.
pub fn discover(root: impl AsRef<Path>) -> Result<Discovery, ChatScanError> {
    let root = root.as_ref();
    let Walk { files, unparsed, unreadable } = walk(root, CHAT_MEDIA_DIR, |path| ChatMediaFile::parse(path))
        .map_err(|source| ChatScanError { dir: root.to_path_buf(), source })?;
    Ok(Discovery::from_walk(files, unparsed, unreadable))
}

// ---- reconciliation ----

/// Where a `Media IDs` token was named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageRef {
    /// Position in [`ChatHistory::conversations`], which the parser sorts by conversation id.
    pub conversation: usize,
    /// Position in that conversation's records, in the order the export listed them.
    pub message: usize,
}

/// The message that named a file, reduced to what a later pass may stamp onto it.
///
/// Every field is a value rather than a lookup, and [`Self::at`] is what reaches the rest of the
/// record. See the module docs for why the conversation is carried as a key and not as the index
/// that found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Where the message sits in the [`ChatHistory`] this join ran against, for whatever that
    /// record holds and this struct does not. `Media Type` is the live one: it splits the observed
    /// export's 2588 matches into `MEDIA` 2509, `NOTE` 77, `STICKER` 1 and `SHARESAVEDSTORY` 1, and
    /// a pass wanting to tell an audio note from a photo has no other route to it.
    ///
    /// **Spending it means re-reading THAT history, not a freshly parsed one.** A position is
    /// meaningful only against the value it was taken from, and a stale one still in range reads
    /// back a different message without failing — which is exactly why the fields below are values
    /// and not a second index. So a pass needing more than they hold takes this and its own
    /// `ChatHistory` together, or gets the field it needs carried here beside them.
    pub at: MessageRef,
    /// The export's own conversation key: a friend's username for a one-to-one thread and a uuid
    /// for a group. Decision 44a names an output directory after this — filesystem-cleaned and
    /// collision-suffixed, both of which are the output task's job and neither of which happens
    /// here.
    pub conversation: ConversationId,
    /// The thread's own name, `None` where the export wrote none.
    ///
    /// Read per RECORD, not per thread: the export writes this on every message, so a group renamed
    /// mid-thread carries two titles under one [`Self::conversation`] key. That is also why decision
    /// 44a names a directory after the key and not after this — a key is stable and a title is not.
    ///
    /// The census's 657 group against 1931 one-to-one matches is a split it DEFINED by this field
    /// being non-null, so it counts how many matches carry a title and is not evidence that a title
    /// means a group. Reading `None` as "one-to-one" is a further guess on top of a definition,
    /// which is why nothing in this crate branches on it.
    pub conversation_title: Option<String>,
    /// `From`, as the message spells it, and `None` for the empty string.
    ///
    /// Carried beside [`Self::is_sender`] rather than resolved with it into one "who sent this"
    /// answer. Nothing observed establishes what `From` holds on a row the account owner sent, and
    /// a build that assumed would attribute 1041 of the observed export's 2588 matches off an
    /// inference it has no way to check.
    pub from: Option<Username>,
    /// Whether the account owner sent it: 1041 of the observed export's 2588 matched messages
    /// against 1547 received.
    pub is_sender: bool,
    /// The message's own `Created`, a UTC instant. `None` where the export wrote `""`, which is
    /// unobserved on a matched message and still leaves the file dated — by
    /// [`Self::created_epoch_ms`] where the record spells one, and by
    /// [`ChatMediaItem::date`]'s filename fallback where it does not.
    pub created: Option<Timestamp>,
    /// The record's OTHER date, `Created(microseconds)`, in the milliseconds it actually holds.
    /// `None` where the export omitted the key or wrote its `0` spelling of absence; see
    /// [`super::model::ChatMessage::created_epoch_ms`] for why both read the same.
    ///
    /// Carried raw rather than as a [`Timestamp`], and resolved only by [`ChatMediaItem::date`]:
    /// the conversion is fallible ([`Timestamp::from_epoch_ms`]) and this struct's job is to report
    /// what the record said, not to decide what survives.
    ///
    /// **[`Self::created`] outranks it, and the ordering costs almost nothing either way.** The two
    /// agree to the second on every row of the observed export (`docs/design.md`, 13-digit values),
    /// so where both exist this field adds only sub-second precision — which this crate's own writer
    /// discards before it reaches a file, [`super::local_fix`]'s `system_time` reducing an instant
    /// to `timestamp()` seconds. What it buys is the rows where `Created` is empty and this is not,
    /// where the alternative is the filename day at midnight. So the order is a preference for the
    /// spelling the export states in full, not a claim that this one is worse.
    pub created_epoch_ms: Option<i64>,
}

/// What `chat_history.json` had to say about one discovered file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Join {
    /// A message's `Media IDs` names it, and this is what that message said. The FIRST such
    /// message: all 2611 observed tokens are distinct, so a second one naming the same file is
    /// unobserved, and keeping the first makes which one wins a fact about the export's own order
    /// rather than about a hash seed.
    ///
    /// **Which message wins now settles more than a position.** It is where the item's timestamp,
    /// its sender and its conversation come from, so a build that kept the LAST would stamp a
    /// different sender and file the media under a different conversation. Pinned by
    /// `a_token_two_messages_name_carries_the_first_messages_own_facts`.
    Named(Message),
    /// It carries a token a message could have named and none did. 5417 of the observed export's
    /// 8005 `b` files.
    Unnamed,
    /// Its family carries no id any json references: the 532 role-worded plain files and the 928
    /// zip files, none of which `chat_history.json` can reach. Distinct from [`Self::Unnamed`]
    /// because "nobody named it" and "nobody could" are different facts, and only the first is
    /// worth looking into.
    NoToken,
}

impl Join {
    /// Whether a message claimed this file.
    #[must_use]
    pub const fn is_named(&self) -> bool {
        matches!(self, Self::Named(_))
    }
}

/// The date a file is dated by, and what said so.
///
/// Two variants rather than an instant with a provenance flag beside it: the two are different
/// facts, and a caller that can read one as the other will stamp a filename day with a message
/// time's confidence. [`Self::Message`] is a UTC instant a message stated; [`Self::Filename`] is a
/// calendar day carrying no time of day at all.
///
/// There is no absent case. Every discovered file's name leads with a day — [`ChatMediaFile::parse`]
/// rejects one that does not — so the fallback is always there to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDate {
    /// An instant the message that named the file stated: its `Created`, or its
    /// `Created(microseconds)` where `Created` is empty. 2588 of the observed export's 9465 files,
    /// every one of them by `Created` — a matched message with only the epoch is expressible and
    /// unobserved.
    Message(Timestamp),
    /// The day the filename leads with, along with any matched message stating no date at all.
    ///
    /// The other 6877 FILES, which is 6413 [`ChatMediaItem`]s: a zip pair's 464 overlay halves are
    /// answered for by the media they pair with rather than asking on their own, the same gap
    /// [`Reconciliation::enroll`] accounts for one way and the `NoToken` census count another. The
    /// census measured this day equal to the message's own date for all 2588 files where both
    /// exist, which is what makes it sound at day granularity and is the whole of what it
    /// establishes.
    Filename(Day),
}

/// One discovered unit and what the history said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMediaItem {
    pub media: ChatMedia,
    pub join: Join,
}

impl ChatMediaItem {
    /// The manifest's `source_id`.
    #[must_use]
    pub fn source_id(&self) -> &str {
        self.media.source_id()
    }

    /// The message that named this file, or `None` for the ones no message did.
    #[must_use]
    pub const fn message(&self) -> Option<&Message> {
        match &self.join {
            Join::Named(message) => Some(message),
            Join::Unnamed | Join::NoToken => None,
        }
    }

    /// What dates this file, and what said so.
    ///
    /// The message's `Created`, then its `Created(microseconds)`, then the day in the filename —
    /// which covers a file no message names, a message that names one carrying neither date, and an
    /// epoch naming an instant [`Timestamp`] cannot hold. The first two are both instants the
    /// MESSAGE stated, so both answer [`MediaDate::Message`]; only the third is derived from the
    /// file itself. See [`Message::created_epoch_ms`] for why `Created` is tried first.
    ///
    /// Not a resolved instant: see the module docs for why the embedded-timestamp step of the
    /// stamping chain belongs between the message's date and the filename day, and so cannot be
    /// applied from here.
    #[must_use]
    pub fn date(&self) -> MediaDate {
        let stated =
            self.message().and_then(|message| message.created.or_else(|| message.created_epoch_ms.and_then(Timestamp::from_epoch_ms)));
        match stated {
            Some(created) => MediaDate::Message(created),
            None => MediaDate::Filename(self.media.file.day),
        }
    }
}

/// Why a token the history names has no file.
///
/// Every spelling reaches the manifest's `last_error` column through
/// [`Manifest::mark_source_missing`], so all of them stay plain prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingReason {
    /// No file in any `chat_media` dir carries the token. 23 of the observed export's 2611.
    NoFile,
    /// Part of the source could not be listed, so the file may exist and simply never have been
    /// seen. Scan-wide rather than per-token — nothing can say whether THIS file was in the dir
    /// that could not be read without reading it — so one unreadable dir qualifies every unmatched
    /// token in the run.
    ///
    /// It outranks [`Self::NoFile`] for the reason [`super::memories::MissingReason::Unscanned`]
    /// gives: [`ItemStatus::SourceMissing`] is never handed back as work, so a verdict written
    /// under it is durable, and "no file exists" written off a scan that could not read part of the
    /// source is a claim the run never established.
    Unscanned,
}

impl MissingReason {
    pub const ALL: [Self; 2] = [Self::NoFile, Self::Unscanned];
}

impl fmt::Display for MissingReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoFile => "no chat media file in the export carries the id this message names",
            Self::Unscanned => "part of the source could not be listed, so the file this message names may exist but was never seen",
        })
    }
}

/// A `Media IDs` token this build's grammar cannot read as a media id.
///
/// Surfaced rather than turned into a [`MissingMedia`]. A token that is not `b~<alphanumeric>` names
/// nothing this pipeline can go looking for, and minting it into a gap row would put a permanent
/// `SourceMissing` entry in the manifest that no run can ever clear — and, when the spelling happens
/// to match a real file's id, would park that file's own row. Zero in the observed export, where
/// every one of the 2611 tokens is well formed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnparsedToken {
    /// Verbatim, as the message spelled it.
    pub token: String,
    /// The first message that spelled it.
    pub message: MessageRef,
}

/// A `Media IDs` token with no file on disk: the chat-media analogue of the memories gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingMedia {
    /// The `b~<id>` token, which is also the manifest's `source_id` for it — the same identity the
    /// file would enroll under if a later run finds it.
    pub token: String,
    /// The first message that named it.
    pub message: MessageRef,
    /// The thread that message arrived in, carried as a key for the reason [`Message::conversation`]
    /// is: [`Self::message`] is a position, and resolving one back into a key is a second lookup that
    /// can name a different thread than the position does.
    ///
    /// **A gap row is still a row with an output record**, which is what makes this load-bearing
    /// rather than symmetry. A file a message names vanishing between two runs drives the row it
    /// already finished to [`ItemStatus::SourceMissing`], and since 2026-08-08 that transition KEEPS
    /// the output path — so the row goes on naming a real directory while dropping out of
    /// [`Reconciliation::items`]. Without a key here, nothing downstream can say whose directory that
    /// is; [`super::chat_fix::RecordedDirs`] is the reader that needs it.
    pub conversation: ConversationId,
    pub reason: MissingReason,
}

/// The files, the tokens, and everything the join could not place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    /// One per discovered unit, ordered by [`ChatMediaItem::source_id`]. Every unit is here,
    /// including the unmatched overlays [`Discovery::unmatched_overlays`] set aside.
    pub items: Vec<ChatMediaItem>,
    /// Tokens the history names and no file carries, ordered by token. Every one of these is a
    /// well-formed `b~<id>` spelling; a token that is not goes to [`Self::unparsed_tokens`].
    pub missing: Vec<MissingMedia>,
    /// Tokens the history names that this build's grammar cannot read, ordered by token.
    pub unparsed_tokens: Vec<UnparsedToken>,
    pub unparsed: Vec<PathBuf>,
    pub duplicates: Vec<Duplicate>,
    /// Carried through from [`Discovery::unreadable`].
    pub unreadable: Vec<UnreadableDir>,
}

impl Reconciliation {
    /// Records every unit and every unmatched token in `manifest`, and marks the tokens
    /// [`ItemStatus::SourceMissing`].
    ///
    /// One row per unit and one per gap token, never a bare count: 23 tokens the export names and
    /// holds no file for read as a clean run when they are a number, and as work when they are
    /// rows.
    ///
    /// No row carries a url, and that is the export rather than an omission: chat media has no
    /// download links anywhere, so every byte this pipeline can ever have is already on disk.
    ///
    /// **A gap token and its file share one identity**, so the transition the memories leg cannot
    /// make is ordinary here: a token enrolled `SourceMissing` by a run that had not extracted the
    /// media part yet goes back on the work list through [`Manifest::reset`] the moment its file
    /// turns up, under the same row. Pinned by
    /// `a_token_whose_file_turned_up_goes_back_on_the_work_list`. A row this leg
    /// [`Manifest::retire_absent`] retired comes back the same way, which is what makes the sweep
    /// below reversible rather than terminal.
    ///
    /// **A file that vanishes between two runs is what the sweep is for**, and it is the case the
    /// shared identity above does NOT cover. A file no message names is absent from [`Self::items`]
    /// once it is gone (no file) and absent from [`Self::missing`] too (no token), so nothing in a
    /// reconciliation can name its row and it used to sit at `Pending` for ever, offered as work no
    /// run could finish. On the observed export that is every file no message names — 5417 unnamed
    /// `b` plus 532 role-worded plus 928 zip, 6877 of 9465 — against the 2588 the identity carries
    /// across. The 27% figure was the reach of the identity fix, never of the problem. Pinned from
    /// both sides: `a_vanished_file_a_message_names_lands_back_at_source_missing_under_the_same_row`
    /// and `a_vanished_file_no_message_names_is_retired`.
    ///
    /// The sweep's rule lives in the manifest rather than here, so this leg and the memories one
    /// cannot answer it differently, and **it does not fire while [`Self::unreadable`] is
    /// non-empty**, for the reason [`MissingReason::Unscanned`] exists: a dir that could not be
    /// listed is not evidence a file is gone.
    ///
    /// This makes TWO [`Manifest::items`] reads, not one: the parked-status read below, and one
    /// inside [`Manifest::retire_absent`]. Both are whole-kind reads rather than a point query per
    /// unit, which is what matters — a real export makes that 9001 point queries — but neither is
    /// narrow, and each materializes an output path, a url and a checksum per row that the verdict
    /// never looks at. A projection like [`Manifest`]'s own finished-item read is the upgrade path,
    /// and it is left undone deliberately rather than unnoticed.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when a manifest read or write fails.
    pub fn enroll(&self, manifest: &mut Manifest) -> Result<(), ManifestError> {
        let rows: Vec<NewItem<'_>> = self
            .items
            .iter()
            .map(|item| NewItem { kind: ItemKind::ChatMedia, source_id: item.source_id(), url: None })
            .chain(self.missing.iter().map(|missing| NewItem { kind: ItemKind::ChatMedia, source_id: &missing.token, url: None }))
            .collect();
        manifest.enroll(&rows)?;

        // Every id this reconciliation can answer for, which is also what the sweep at the bottom
        // measures a row against: an enrolled row outside this set is one the export no longer
        // names under any identity.
        //
        // The `debug_assert` is the load-bearing half. A token that were both an item and a gap
        // would be marked source-missing AND reset by the loops below, so which one won would be
        // decided by where the `Manifest::items` read sits relative to them — an ordering nothing
        // pins and nothing should have to. This is not hypothetical: a `missing` token is only
        // checked against the join map, and until `parse_history_token` existed any string could
        // reach it, including one spelling a present file's `source_id`, which that read ordering
        // then guaranteed would NOT be reset on the run that parked it.
        //
        // **Two guards make the sets disjoint and they have to move together**, which is why naming
        // only the trust boundary here was wrong. `parse_history_token` narrows a token to the
        // `b~<alnum>` spelling, and that is a SUBSET of the item-id space rather than disjoint from
        // it — `ChatMediaFile::parse` mints exactly that spelling for a `(Token::B, Family::Plain)`
        // file. What closes it is the second fact: `ChatMediaFile::history_token` hands a file to the
        // join map under exactly the condition that produces that spelling, so every item whose id a
        // token could spell is already IN the map and routes to `Named` rather than to `missing`.
        // Tighten either one alone and the sets overlap — add a condition to `history_token` and a
        // `b~X` file drops out of the map while its item keeps `source_id == "b~X"`. Neither is
        // enforced by the compiler. Pinned by `a_gap_token_and_an_item_id_are_never_the_same_string`,
        // which covers both conjuncts because its `b~` file is on disk, and on the guard itself by
        // `an_overlapping_gap_token_and_item_id_stop_the_run_rather_than_racing_the_read`.
        let item_ids: BTreeSet<&str> = self.items.iter().map(ChatMediaItem::source_id).collect();
        debug_assert!(
            !self.missing.iter().any(|missing| item_ids.contains(missing.token.as_str())),
            "a gap token spells an item's own source id, so this call would mark that row missing AND reset it, and the read below would \
             decide which of the two won"
        );
        let named: BTreeSet<&str> = item_ids.iter().copied().chain(self.missing.iter().map(|missing| missing.token.as_str())).collect();

        // Read before anything is marked, so what this sweep sees is where the PREVIOUS run left
        // each row rather than what the loops below are about to write.
        let parked: BTreeSet<String> = manifest
            .items(ItemKind::ChatMedia)?
            .into_iter()
            .filter(|row| matches!(row.status, ItemStatus::SourceMissing | ItemStatus::Retired))
            .map(|row| row.source_id)
            .collect();

        for missing in &self.missing {
            manifest.mark_source_missing(ItemKind::ChatMedia, &missing.token, &missing.reason.to_string())?;
        }
        for item in self.items.iter().filter(|item| parked.contains(item.source_id())) {
            manifest.reset(ItemKind::ChatMedia, item.source_id())?;
        }
        manifest.retire_absent(ItemKind::ChatMedia, &named, &self.unreadable)
    }
}

/// Joins `history`'s `Media IDs` tokens to `discovery`'s files, and carries what each joined
/// message said onto the file it named.
///
/// A string equality, one token at a time. Nothing is bucketed, nothing is guessed, and a token
/// that matches no file becomes a [`MissingMedia`] rather than being dropped. A joined item carries
/// its [`Message`] by value, so nothing downstream needs `history` again to know when a file was
/// sent, by whom, or in which conversation — the facts a later pass stamps. Everything else the
/// record holds stays reachable only by spending [`Message::at`] against this same value, which is
/// a narrower claim than "read once" and is the one that is true.
#[must_use]
pub fn reconcile(history: &ChatHistory, discovery: Discovery) -> Reconciliation {
    let Discovery { media, unmatched_overlays, unparsed, duplicates, unreadable } = discovery;

    let mut items: Vec<ChatMediaItem> = media
        .into_iter()
        .chain(unmatched_overlays.into_iter().map(|file| ChatMedia { file, overlay: None }))
        .map(|media| {
            let join = if media.file.history_token().is_some() { Join::Unnamed } else { Join::NoToken };
            ChatMediaItem { media, join }
        })
        .collect();
    items.sort_by(|left, right| left.source_id().cmp(right.source_id()));

    let mut named: Vec<(usize, Message)> = Vec::new();
    let mut missing: BTreeMap<String, (MessageRef, ConversationId)> = BTreeMap::new();
    let mut unparsed_tokens: BTreeMap<String, MessageRef> = BTreeMap::new();
    {
        // Collecting into a map keeps the LAST value on a duplicate key, which would silently leave
        // the earlier item unjoinable. No two items can share a token — a leader's id is unique
        // through `kept`'s `(id, is_overlay)` key, and `overlays.remove` runs for every leader, so
        // an unmatched overlay cannot carry a leader's id either — and that is a convention the
        // compiler does not enforce, so it is asserted rather than trusted. Pinned by
        // `no_two_items_can_claim_one_history_token`.
        let by_token: BTreeMap<&str, usize> =
            items.iter().enumerate().filter_map(|(index, item)| Some((item.media.file.history_token()?, index))).collect();
        debug_assert_eq!(
            by_token.len(),
            items.iter().filter(|item| item.media.file.history_token().is_some()).count(),
            "two items share one history token, so the join would drop one of them without a trace"
        );
        for (conversation, thread) in history.conversations.iter().enumerate() {
            for (message, record) in thread.records.iter().enumerate() {
                let Some(raw) = record.media_ids.as_deref() else { continue };
                for raw_token in media_tokens(raw) {
                    let at = MessageRef { conversation, message };
                    // The trust boundary: anything that is not a `b~<id>` spelling never reaches the
                    // manifest as a `source_id`. See `parse_history_token` for what skipping this
                    // cost.
                    let Some(token) = parse_history_token(raw_token) else {
                        unparsed_tokens.entry(raw_token.to_owned()).or_insert(at);
                        continue;
                    };
                    match by_token.get(token.as_str()) {
                        // Minted here, from the one `record` this position names, rather than
                        // handed on as the position for someone downstream to read back. There is
                        // no second lookup that could name a different message than `at` does.
                        Some(&index) => named.push((
                            index,
                            Message {
                                at,
                                conversation: thread.id.clone(),
                                conversation_title: record.conversation_title.clone(),
                                from: record.from.clone(),
                                is_sender: record.is_sender,
                                created: record.created,
                                created_epoch_ms: record.created_epoch_ms,
                            },
                        )),
                        None => {
                            // First namer wins, the same rule `Join::Named` applies one arm over, so
                            // the key recorded here is the one the item itself would have carried.
                            missing.entry(token).or_insert_with(|| (at, thread.id.clone()));
                        }
                    }
                }
            }
        }
    }

    for (index, message) in named {
        // The index came from an enumerate over this same vector, which nothing has resized since.
        // The `Unnamed` guard is what makes the FIRST message the one whose facts the item carries;
        // see [`Join::Named`] for what a later one would overwrite.
        if let Some(item) = items.get_mut(index)
            && matches!(item.join, Join::Unnamed)
        {
            item.join = Join::Named(message);
        }
    }

    let reason = if unreadable.is_empty() { MissingReason::NoFile } else { MissingReason::Unscanned };
    let missing =
        missing.into_iter().map(|(token, (message, conversation))| MissingMedia { token, message, conversation, reason }).collect();
    let unparsed_tokens = unparsed_tokens.into_iter().map(|(token, message)| UnparsedToken { token, message }).collect();

    Reconciliation { items, missing, unparsed_tokens, unparsed, duplicates, unreadable }
}

#[cfg(test)]
mod tests {
    use super::{Family, Token, is_alphanumeric_run};

    #[test]
    fn an_id_is_a_run_of_ascii_alphanumerics_and_nothing_else() {
        assert!(is_alphanumeric_run("aB3"));
        assert!(is_alphanumeric_run("0"));

        assert!(!is_alphanumeric_run(""), "an empty tail is not an id");
        // The three characters the zip family spells its own tail with. Each has to fail here, or
        // a zip name whose `.zip.` split went wrong would be read as a plain id.
        assert!(!is_alphanumeric_run("ab-cd"));
        assert!(!is_alphanumeric_run("ab.cd"));
        assert!(!is_alphanumeric_run("ab~cd"));
        assert!(!is_alphanumeric_run("ab_cd"));
    }

    #[test]
    fn the_two_families_are_told_apart_by_the_tail_alone() {
        assert_eq!(Family::parse("aB3xY9"), Some(Family::Plain { id: "aB3xY9".to_owned() }));
        assert_eq!(
            Family::parse("vantsnap-1234567.zip.a1b2c3d"),
            Some(Family::Zip { mid: "vantsnap-1234567".to_owned(), hash: "a1b2c3d".to_owned() })
        );

        // Shapes that spell `.zip.` and are not the zip family.
        assert_eq!(Family::parse("vantsnap.zip.a1b2c3d"), None, "no digits half");
        assert_eq!(Family::parse("vantsnap-12x4567.zip.a1b2c3d"), None, "the digits half is not digits");
        assert_eq!(Family::parse("vantsnap-1234567.zip."), None, "no hash");
        assert_eq!(Family::parse("-1234567.zip.a1b2c3d"), None, "no word");
        assert_eq!(Family::parse(""), None);
    }

    #[test]
    fn every_token_is_named_in_all() {
        // Second witness to `Token::as_word`'s match; survives either being weakened to a wildcard.
        // Never collapse to `_ => {}`. Same residual as `super::memories::Role::ALL`: an array
        // literal's length is independent of the enum's variant count, so this proves a new variant
        // stops the build here, not that `ALL` is complete.
        for token in Token::ALL {
            match token {
                Token::B | Token::Media | Token::Overlay | Token::Thumbnail => {}
            }
            assert_eq!(Token::parse(token.as_word()), Some(token));
        }
        assert_eq!(Token::parse("metadata"), None, "the role the census found zero files for");
        assert_eq!(Token::parse("B"), Some(Token::B), "matched without regard to case");
    }
}
