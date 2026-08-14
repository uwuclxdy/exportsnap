//! Memories: the media a Snapchat export leaves on disk, the entries `memories_history.json`
//! names, and the join between the two.
//!
//! The join is by DATE BUCKET, because the export offers nothing better. An entry carries five
//! keys and none of them is an id; every download link in the one observed export is `""`; and the
//! `memories.html` index sitting beside the media repeats the date already in each filename and
//! carries nothing else. So an entry is matched to media by the day it happened on — or one of the
//! two adjacent days, for an entry the exact day could not serve (decision 75) — and the kind of
//! media it is, and a bucket holding several of each pairs them arbitrarily.
//!
//! What that costs is recorded rather than hidden. A [`Pairing`] says whether an item was the one
//! entry and the one media set in its bucket ([`Pairing::Exact`]) or one of several
//! ([`Pairing::Ambiguous`]), because what a later pass may stamp onto a file depends on which it
//! got. An entry no media matched becomes a manifest row of its own at
//! [`ItemStatus::SourceMissing`] rather than a number in a summary: the observed export names 836
//! memories and holds media for 746, and a run that quietly finishes 746 reads as a clean run.
//!
//! Framework-free like the rest of `export/`: nothing here writes an output file, composites an
//! overlay, or knows a screen exists.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDate};

use crate::export::manifest::{ItemKind, ItemStatus, Manifest, ManifestError, NewItem};
use crate::export::model::{DownloadUrl, MediaKind, Memories, Memory, Timestamp, fixed_width, in_range, split_three};
use crate::export::walk::{Walk, walk};

pub use crate::export::walk::UnreadableDir;

/// The directory name media discovery walks for, at any depth under the source root.
///
/// It recurs at several paths in the one observed export — beside the extracted parts, inside one
/// of them, and under an OS-deduplicated `memories (1)/` — so this is a search, not a fixed
/// location.
const MEMORIES_DIR: &str = "memories";

// ---- validated primitives ----

/// A calendar day: what a memory filename spells and what an entry's [`Timestamp`] reduces to.
///
/// Range-checked rather than calendar-checked, exactly like [`Timestamp`]: `2021-02-30` parses.
/// The join's ±1-day window is the one place this type does date arithmetic
/// ([`Self::neighbors`]), and it converts fallibly through a real calendar: a day that names no
/// real date has no neighbors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day {
    year: u16,
    month: u8,
    day: u8,
}

impl Day {
    /// Parses `YYYY-MM-DD`, the form a memory filename leads with.
    ///
    /// # Examples
    ///
    /// ```
    /// use exportsnap::export::memories::Day;
    ///
    /// assert_eq!(Day::parse("2020-07-28").unwrap().to_string(), "2020-07-28");
    /// assert!(Day::parse("2020-7-28").is_none());
    /// ```
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (year, month, day) = split_three(text, '-')?;
        Some(Self {
            year: fixed_width(year, 4)?,
            month: in_range(fixed_width(month, 2)?, 1, 12)?,
            day: in_range(fixed_width(day, 2)?, 1, 31)?,
        })
    }

    #[must_use]
    pub const fn year(self) -> u16 {
        self.year
    }

    #[must_use]
    pub const fn month(self) -> u8 {
        self.month
    }

    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }

    /// The two calendar days either side of this one, the next day first.
    ///
    /// The join's ±1-day window (decision 75) is the only date arithmetic this type does, so it is
    /// fallible the same way [`Timestamp`]'s conversion to a date crate is (the design's ceiling:
    /// a range-checked day handed to a real calendar converts fallibly). A day that parses but
    /// names no real date — `2021-02-30` — has no neighbors rather than an invented day.
    #[must_use]
    fn neighbors(self) -> Option<[Self; 2]> {
        let date = NaiveDate::from_ymd_opt(i32::from(self.year), u32::from(self.month), u32::from(self.day))?;
        let next = date.succ_opt().and_then(Self::from_date)?;
        let previous = date.pred_opt().and_then(Self::from_date)?;
        Some([next, previous])
    }

    /// The [`Day`] a real calendar date names, or `None` when this type cannot hold its year.
    ///
    /// Same shape as [`Timestamp::from_epoch_ms`]'s conversion: the `u16` year is the type's
    /// ceiling, so a neighbor reaching past it (`65535-12-31`'s successor) is absence, never a
    /// truncated year.
    fn from_date(date: NaiveDate) -> Option<Self> {
        Some(Self { year: u16::try_from(date.year()).ok()?, month: u8::try_from(date.month()).ok()?, day: u8::try_from(date.day()).ok()? })
    }
}

/// Takes the UTC calendar date verbatim, which is the join's load-bearing assumption: the entry's
/// `Date` is UTC and the filename's day is whatever zone Snapchat named the file in. The zones
/// disagree, measured at n=2 — 36 of 846 entries (and 36 of 836) sit exactly one day off their
/// media, at filename-coverage level. The join's ±1-day window (decision 75) is the bound: a file
/// two days out still pairs with nothing.
impl From<Timestamp> for Day {
    fn from(timestamp: Timestamp) -> Self {
        Self { year: timestamp.year(), month: timestamp.month(), day: timestamp.day() }
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { year, month, day } = self;
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}

/// Which layer of one memory a file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// The memory itself: every `.mp4` and every `.jpg` in the observed export.
    Main,
    /// The caption or sticker layer drawn over it, and every `.png` in the observed export.
    Overlay,
}

impl Role {
    pub const ALL: [Self; 2] = [Self::Main, Self::Overlay];

    /// The word the filename spells after the uuid.
    #[must_use]
    pub const fn as_suffix(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Overlay => "overlay",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| raw.eq_ignore_ascii_case(role.as_suffix()))
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_suffix())
    }
}

/// Which sort of media a bucket holds: the half of the join key that is not the day.
///
/// [`Self::Unknown`] is a single bucket for every word and every extension this build cannot
/// place, so two dissimilar unknowns falling on one day would pair with each other. That is the
/// honest cost of a three-way key, and it is unexercised on the observed export where every entry
/// is `Image` or `Video` and every main file is `.mp4` or `.jpg`. The alternative is a key
/// carrying the raw word, which an entry's `Media Type` and a filename's extension can never agree
/// on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryKind {
    Image,
    Video,
    Unknown,
}

impl MemoryKind {
    /// Extensions observed on a main file, plus the two spellings that sit next to them.
    ///
    /// `png` is here for a `-main.png` this export does not contain; every observed `.png` is an
    /// overlay, and an overlay is never bucketed.
    const IMAGE: [&'static str; 3] = ["jpg", "jpeg", "png"];
    const VIDEO: [&'static str; 1] = ["mp4"];

    /// What a file of this extension holds. Ascii-case-insensitive; an extension this build does
    /// not know is [`Self::Unknown`] rather than an error, so the file is still reported.
    #[must_use]
    pub fn from_extension(extension: &str) -> Self {
        if Self::VIDEO.iter().any(|known| extension.eq_ignore_ascii_case(known)) {
            Self::Video
        } else if Self::IMAGE.iter().any(|known| extension.eq_ignore_ascii_case(known)) {
            Self::Image
        } else {
            Self::Unknown
        }
    }

    /// What an entry's `Media Type` word says it is. Every other word — every chat and snap word
    /// included — is [`Self::Unknown`], because none of them names a memory.
    #[must_use]
    pub fn from_media_type(media_type: &MediaKind) -> Self {
        match media_type {
            MediaKind::Image => Self::Image,
            MediaKind::Video => Self::Video,
            MediaKind::Text | MediaKind::Media | MediaKind::Status | MediaKind::Note | MediaKind::Sticker | MediaKind::Other(_) => {
                Self::Unknown
            }
        }
    }
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Unknown => "unknown",
        })
    }
}

/// The day-and-kind pair an entry and a media set have to share to pair at all.
///
/// Keying on the day alone was measured against the same export and is strictly worse: 393 buckets
/// against 479, and 187 entries in a 1:1 bucket against 267. The surplus is the same either way,
/// so the kind buys precision in the pairing without changing what is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bucket {
    pub day: Day,
    pub kind: MemoryKind,
}

impl Bucket {
    /// The bucket an entry falls in, or `None` when it carries no date to bucket by.
    ///
    /// Public because a later pass has to re-derive the same key to ask a question about the whole
    /// bucket — decision 32's "does every entry here name one place" — and a second spelling of
    /// the key would drift from this one silently.
    #[must_use]
    pub fn of(memory: &Memory) -> Option<Self> {
        Some(Self { day: Day::from(memory.date?), kind: MemoryKind::from_media_type(&memory.media_type) })
    }
}

impl fmt::Display for Bucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { day, kind } = self;
        write!(f, "{day} {kind}")
    }
}

// ---- files on disk ----

/// One media file in a `memories` dir, with its name parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFile {
    /// Where it sits, as the walk found it.
    pub path: PathBuf,
    /// The day the filename leads with. The only date a memory file carries.
    pub day: Day,
    /// Snapchat's own id for the memory, shared by a main and its overlay. This is the manifest's
    /// `source_id` for an entry that pairs with it.
    pub uuid: String,
    pub role: Role,
    /// Verbatim, as the name spells it. [`MemoryKind::from_extension`] lowercases to classify and
    /// this does not, so a file this build cannot place is reported as it is on disk rather than as
    /// a bucket verdict.
    pub extension: String,
}

impl MemoryFile {
    /// Parses `YYYY-MM-DD_<uuid>-<role>.<ext>` out of `path`'s file name.
    ///
    /// `None` for any other shape. `memories.html`, which sits in every memories dir, is the one
    /// observed case; a rejected name is counted and carried by [`Discovery::unparsed`] rather than
    /// dropped, because a media file this build cannot read is one nobody would notice missing.
    ///
    /// # Examples
    ///
    /// ```
    /// use exportsnap::export::memories::{MemoryFile, Role};
    ///
    /// let file = MemoryFile::parse("/x/memories/2020-07-28_2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e-main.mp4").unwrap();
    /// assert_eq!(file.uuid, "2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e");
    /// assert_eq!(file.role, Role::Main);
    /// assert_eq!(file.extension, "mp4");
    ///
    /// assert!(MemoryFile::parse("/x/memories/memories.html").is_none());
    /// ```
    #[must_use]
    pub fn parse(path: impl AsRef<Path>) -> Option<Self> {
        let path = path.as_ref();
        let name = path.file_name().and_then(OsStr::to_str)?;
        let (stem, extension) = name.rsplit_once('.')?;
        let (day, rest) = stem.split_once('_')?;
        // The uuid holds dashes of its own, so the role has to come off the right-hand end.
        let (uuid, role) = rest.rsplit_once('-')?;
        if !is_uuid(uuid) {
            return None;
        }
        Some(Self {
            path: path.to_path_buf(),
            day: Day::parse(day)?,
            uuid: uuid.to_owned(),
            role: Role::parse(role)?,
            extension: extension.to_owned(),
        })
    }
}

/// Whether `text` is shaped like the 36-character dashed uuid a memory filename carries.
///
/// Ascii-alphanumeric rather than hex-only on purpose: the id is opaque to this crate, the export
/// is n=1, and refusing a real filename over a character class costs a media file. What the check
/// has to do is tell a uuid apart from any other dash-bearing stem, which the length and the four
/// dash positions already do.
///
/// It is also what makes [`synthetic_source_id`] collision-proof, structurally rather than by
/// naming convention: a synthetic id is rejected here, and this is the same predicate that decides
/// which filenames yield a uuid at all.
fn is_uuid(text: &str) -> bool {
    const DASHES: [usize; 4] = [8, 13, 18, 23];
    const LENGTH: usize = 36;

    text.len() == LENGTH
        && text.bytes().enumerate().all(|(index, byte)| if DASHES.contains(&index) { byte == b'-' } else { byte.is_ascii_alphanumeric() })
}

/// One memory's files: the media itself and, when the export carried one, the layer drawn over it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryMedia {
    pub main: MemoryFile,
    /// `None` is the common answer: 161 of the observed export's 746 memories carry an overlay,
    /// split 77 image mains and 84 video mains. The export holds 162 overlay files, not 161; the
    /// odd one has no main and lands in [`Discovery::orphan_overlays`] instead.
    pub overlay: Option<MemoryFile>,
}

impl MemoryMedia {
    /// The id the main and the overlay share, and the manifest's `source_id` for a paired entry.
    #[must_use]
    pub fn uuid(&self) -> &str {
        &self.main.uuid
    }

    /// The bucket this media set can pair inside.
    #[must_use]
    pub fn bucket(&self) -> Bucket {
        Bucket { day: self.main.day, kind: MemoryKind::from_extension(&self.main.extension) }
    }
}

/// Two files claiming the same memory and the same role.
///
/// Reported rather than deduped. All 908 basenames are unique across the observed export's three
/// memories dirs, so this is the shape of a re-download or a half-merged copy, and quietly picking
/// one would hide the fact that a second file exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    pub uuid: String,
    pub role: Role,
    /// The file the pairing used: first in the order [`Discovery::from_files`] sorts into.
    pub kept: PathBuf,
    /// The ones it did not, in the same order.
    pub ignored: Vec<PathBuf>,
}

/// What a walk of the source root found in every `memories` dir under it.
///
/// Public fields, and [`Self::from_files`] takes the file list directly, so a caller that already
/// knows the answers can build one without a filesystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// One per `-main` file, ordered by uuid.
    pub media: Vec<MemoryMedia>,
    /// Overlays whose main is not in the export. An overlay without a main is nothing to composite
    /// and nothing to pair, so it is counted here rather than turned into an item.
    pub orphan_overlays: Vec<MemoryFile>,
    /// Names in a `memories` dir this build's grammar does not read, ordered by path.
    pub unparsed: Vec<PathBuf>,
    /// Ordered by uuid, then role.
    pub duplicates: Vec<Duplicate>,
    /// Directories under the root the walk could not list, ordered by path. Reported rather than
    /// fatal: a source root on a mounted filesystem carries dirs the export has nothing to do with
    /// and this user cannot read — `lost+found` is 0700 and root-owned on every ext4 mount — and
    /// failing the whole scan over one of those hides every memory that WAS found. The root itself
    /// is the exception and still errors, because that one is the caller's own argument.
    pub unreadable: Vec<UnreadableDir>,
}

impl Discovery {
    /// The pairing pass on its own, split from the walk so it can be driven without a filesystem.
    ///
    /// `read_dir` order is whatever the filesystem says and differs between runs and between
    /// machines, and it is kept out of two answers. [`Self::media`] comes out in uuid order because
    /// the map it is built from is keyed and iterated that way, which is what makes an ambiguous
    /// bucket hand its files to entries the same way on every run. Separately, the files are sorted
    /// before that map is filled, so which of several files claiming one memory is kept — and which
    /// are reported as ignored — is settled by path rather than by whichever dir the walk reached
    /// first.
    #[must_use]
    pub fn from_files(files: Vec<MemoryFile>, unparsed: Vec<PathBuf>) -> Self {
        Self::from_walk(files, unparsed, Vec::new())
    }

    /// [`Self::from_files`] plus the dirs the walk could not list.
    #[must_use]
    pub fn from_walk(files: Vec<MemoryFile>, mut unparsed: Vec<PathBuf>, mut unreadable: Vec<UnreadableDir>) -> Self {
        let mut kept: BTreeMap<(String, Role), MemoryFile> = BTreeMap::new();
        let mut duplicates: BTreeMap<(String, Role), Duplicate> = BTreeMap::new();

        for file in sorted(files) {
            let key = (file.uuid.clone(), file.role);
            match kept.get(&key).map(|first| first.path.clone()) {
                Some(first) => {
                    let duplicate = duplicates.entry(key).or_insert_with(|| Duplicate {
                        uuid: file.uuid.clone(),
                        role: file.role,
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

        let mut mains = Vec::new();
        let mut overlays: BTreeMap<String, MemoryFile> = BTreeMap::new();
        for ((uuid, role), file) in kept {
            match role {
                Role::Main => mains.push(file),
                Role::Overlay => {
                    overlays.insert(uuid, file);
                }
            }
        }

        let media = mains
            .into_iter()
            .map(|main| {
                let overlay = overlays.remove(&main.uuid);
                MemoryMedia { main, overlay }
            })
            .collect();

        unparsed.sort();
        unreadable.sort_by(|left, right| left.dir.cmp(&right.dir));
        Self {
            media,
            orphan_overlays: overlays.into_values().collect(),
            unparsed,
            duplicates: duplicates.into_values().collect(),
            unreadable,
        }
    }
}

/// Total order over the discovered files. The uuid is unique per memory and the role splits its two
/// files, so only a genuine duplicate falls through to the path.
fn sorted(mut files: Vec<MemoryFile>) -> Vec<MemoryFile> {
    files.sort_by(|left, right| (&left.uuid, left.role, &left.path).cmp(&(&right.uuid, right.role, &right.path)));
    files
}

/// The source root itself could not be listed.
///
/// Only ever the root. A directory underneath it that cannot be listed is [`Discovery::unreadable`]
/// instead, because the root is the caller's own argument and everything below it is the export's
/// own shape. Named apart from [`crate::export::zip::DiscoverError`] so a caller reaching for both
/// needs no alias.
#[derive(Debug)]
pub struct ScanError {
    /// The root that was being listed.
    pub dir: PathBuf,
    /// What the filesystem said.
    pub source: io::Error,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not list {} looking for the export's memories dirs: {}; point the source at the dir holding the extracted export parts",
            self.dir.display(),
            self.source
        )
    }
}

impl Error for ScanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Every media file in every dir named `memories` under `root`, at any depth, paired up.
///
/// A directory the walk cannot list is recorded in [`Discovery::unreadable`] and the walk carries
/// on. Aborting instead would report zero memories for a source root that merely happens to sit on
/// a filesystem with a `lost+found` on it, and reporting the skip answers the same question that an
/// abort was there to answer — which dir, and why — without throwing away everything that was
/// found. `root` keeps the hard error: that one is the caller's own argument, and a run that cannot
/// read it has nothing to report at all.
///
/// # Errors
///
/// Returns [`ScanError`] when `root` cannot be listed.
pub fn discover(root: impl AsRef<Path>) -> Result<Discovery, ScanError> {
    let root = root.as_ref();
    let Walk { files, unparsed, unreadable } =
        walk(root, MEMORIES_DIR, |path| MemoryFile::parse(path)).map_err(|source| ScanError { dir: root.to_path_buf(), source })?;
    Ok(Discovery::from_walk(files, unparsed, unreadable))
}

// ---- reconciliation ----

/// What an entry got out of its bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pairing {
    /// The bucket held exactly one entry and exactly one media set, so the two belong together.
    Exact(MemoryMedia),
    /// The bucket held several, and which entry got which media set is arbitrary. Carried rather
    /// than resolved: nothing in the export decides it, and a later pass has to know it is working
    /// from a guess before it stamps anything derived from the entry onto the file.
    Ambiguous(MemoryMedia),
    /// The export names this memory and holds no media for it.
    Missing(MissingReason),
}

/// Why an entry paired with nothing. Every spelling reaches the manifest's `last_error` column
/// through [`Manifest::mark_source_missing`], so all of them stay plain prose.
///
/// [`Self::Unscanned`] is the one that says the run does not know, and it outranks the other two
/// for a reason worth stating: [`ItemStatus::SourceMissing`] is never handed back as work, so a
/// verdict written under it is durable. Asserting "no media exists" off a scan that could not read
/// part of the source is an assertion the run never established, and the row would outlive the
/// permission problem that caused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingReason {
    /// The bucket named more entries than the export holds media for. All 90 of the observed
    /// export's unpaired entries are these; only 746 mains exist for 836 entries, so at least 90
    /// pair with nothing under any window.
    NoMedia,
    /// The entry carries no usable `Date`, so it falls in no bucket at all. Unobserved: all 836
    /// entries in the one export date cleanly.
    NoDate,
    /// Part of the source could not be listed, so this entry's media may exist and simply never
    /// have been seen. Scan-wide rather than per-entry — nothing can say whether THIS memory was in
    /// the dir that could not be read without reading it — so one unreadable dir qualifies every
    /// unpaired entry in the run.
    ///
    /// The cost of that coarseness, stated rather than hidden: a source root that happens to carry
    /// a `lost+found` turns all 90 genuinely-absent entries into "may exist", which under-claims in
    /// the safe direction. The alternative is asserting absence the run cannot support, which is
    /// the direction that loses data.
    Unscanned,
}

impl MissingReason {
    /// Every reason, so a caller checking or rendering the whole set names none of them itself.
    ///
    /// Same shape as [`Role::ALL`] and [`crate::export::manifest::ItemStatus::ALL`], and it carries
    /// the same residual, measured rather than assumed: an array literal's length is independent of
    /// the enum's variant count, so an enum with a fourth variant and a three-element `ALL` compiles
    /// clean. Having one list beats having two, but the list is not self-policing.
    ///
    /// Two exhaustive matches are what make a variant impossible to add SILENTLY, and both are
    /// compile errors rather than assertions: the `Display` arm below, and the witness in
    /// `a_missing_reason_says_which_gap_it_is_in_prose_the_manifest_can_store`. Neither proves this
    /// array is complete — an author can answer both and still leave `ALL` short — so what is
    /// guaranteed is that adding a reason stops the build twice, next to this line each time.
    pub const ALL: [Self; 3] = [Self::NoMedia, Self::NoDate, Self::Unscanned];
}

impl fmt::Display for MissingReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoMedia => "the export holds no memory media for this entry's day and kind",
            Self::NoDate => "the entry carries no date, so no memory media can be matched to it",
            Self::Unscanned => "part of the source could not be listed, so media for this entry may exist but was never seen",
        })
    }
}

/// One `memories_history.json` entry and what the filesystem had for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryItem {
    /// Position in the `saved_media` of the [`Memories`] this reconciliation was built from, so a
    /// later pass can read back the date, the coordinates and the media type this join does not
    /// carry.
    pub entry_index: usize,
    /// The manifest's `source_id`: the media's uuid when the entry paired, and a spelling
    /// [`is_uuid`] rejects when it did not.
    pub source_id: String,
    /// The entry's signed url. Both url fields are `""` in the one observed export, so `None` is
    /// the normal answer; `Media Download Url` wins over `Download Link` when both are present,
    /// which is a guess neither this export nor Snapchat's documentation settles.
    pub url: Option<DownloadUrl>,
    pub pairing: Pairing,
}

impl MemoryItem {
    /// The media this entry paired with, whether exactly or ambiguously.
    #[must_use]
    pub fn media(&self) -> Option<&MemoryMedia> {
        match &self.pairing {
            Pairing::Exact(media) | Pairing::Ambiguous(media) => Some(media),
            Pairing::Missing(_) => None,
        }
    }
}

/// The entries, the media, and everything the join could not place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    /// One per entry, in the order `memories_history.json` lists them. Every entry is here, paired
    /// or not.
    pub items: Vec<MemoryItem>,
    /// Media whose bucket held fewer entries than files. Zero in the observed export, and kept
    /// rather than dropped because the opposite of the 90 missing entries is a memory the history
    /// forgot.
    pub files_without_entry: Vec<MemoryMedia>,
    pub orphan_overlays: Vec<MemoryFile>,
    pub unparsed: Vec<PathBuf>,
    pub duplicates: Vec<Duplicate>,
    /// Carried through from [`Discovery::unreadable`]: while any of these stand, every count in
    /// [`Self::report`] is a lower bound rather than a total.
    pub unreadable: Vec<UnreadableDir>,
}

/// The counts a CLI line or the memories screen prints after a reconciliation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconciliationReport {
    /// Entries in `memories_history.json`.
    pub entries: usize,
    /// Main files on disk: paired plus [`Reconciliation::files_without_entry`]. Not the file count
    /// of the memories dirs, which also holds the overlays.
    pub files: usize,
    pub paired_exact: usize,
    pub paired_ambiguous: usize,
    pub source_missing: usize,
    pub files_without_entry: usize,
    pub unparsed_names: usize,
    pub orphan_overlays: usize,
    /// Media files set aside because another file already claimed that memory and role — the sum of
    /// every [`Duplicate::ignored`], not a count of memories or of uuids. Three copies of one main
    /// is one [`Duplicate`] and two files here, and a memory whose main AND overlay are both
    /// duplicated contributes to this once per file rather than once.
    pub duplicate_files: usize,
    /// Directories the walk could not list. Zero on a healthy export; non-zero means some of the
    /// tree was never looked at, so every count above is a lower bound.
    pub unreadable_dirs: usize,
}

impl Reconciliation {
    /// The counts, derived rather than tallied along the way so they cannot drift from the items.
    #[must_use]
    pub fn report(&self) -> ReconciliationReport {
        let mut report = ReconciliationReport {
            entries: self.items.len(),
            files_without_entry: self.files_without_entry.len(),
            unparsed_names: self.unparsed.len(),
            orphan_overlays: self.orphan_overlays.len(),
            duplicate_files: self.duplicates.iter().map(|duplicate| duplicate.ignored.len()).sum(),
            unreadable_dirs: self.unreadable.len(),
            ..ReconciliationReport::default()
        };
        for item in &self.items {
            match item.pairing {
                Pairing::Exact(_) => report.paired_exact += 1,
                Pairing::Ambiguous(_) => report.paired_ambiguous += 1,
                Pairing::Missing(_) => report.source_missing += 1,
            }
        }
        report.files = report.paired_exact + report.paired_ambiguous + report.files_without_entry;
        report
    }

    /// Records every entry in `manifest` and marks the unpaired ones [`ItemStatus::SourceMissing`].
    ///
    /// One row per ENTRY, never one per file. The 90 memories the observed export names and holds
    /// no media for are the reason: a bare "746 of 836" reads as success, while a row each is what
    /// makes the gap addressable one memory at a time.
    ///
    /// Two transitions the row's existing status decides rather than this reconciliation: an item
    /// that was [`ItemStatus::SourceMissing`] or [`ItemStatus::Retired`] and pairs now goes back on
    /// the work list through [`Manifest::reset`].
    ///
    /// **The `SourceMissing` half serves a producer that does not exist yet** — a downloader marking
    /// a PAIRED item's source missing — and nothing in this module can reach it, because the only
    /// `mark_source_missing` here is on a synthetic id. In particular it is NOT what serves the
    /// export part that had not been extracted yet: an entry that was unpaired and pairs now
    /// changes `source_id` from the synthetic one to the media's uuid, so it arrives as a NEW row
    /// at [`ItemStatus::Pending`] and never touches `reset`. `an_entry_whose_media_turned_up_goes_
    /// back_on_the_work_list` is that case and reset does not fire in it. **The `Retired` half this
    /// module reaches on its own**, and it is what makes the sweep below reversible: a uuid row
    /// retired when its media went away is named again the moment the media is back, under the id it
    /// already carries. Pinned by `a_retired_memory_whose_media_came_back_goes_back_on_the_work_list`.
    ///
    /// Nothing here guards a finished item against being un-finished, because no reconciliation can
    /// name one: a [`Pairing::Missing`] item's `source_id` is a synthetic one, a synthetic id is
    /// only ever [`ItemStatus::SourceMissing`], and [`Manifest::pending`] — the only way an item
    /// reaches [`Manifest::mark_done`] — never offers those. A guard for it would be a check no
    /// input can reach, which is the shape a mutation cannot tell from no check at all. The sweep
    /// below is the one place a finished row IS reachable and it exempts [`ItemStatus::Done`]
    /// there, in the manifest, where both legs get the same answer.
    ///
    /// **An entry that crosses between paired and unpaired between two runs changes `source_id`,
    /// and the row it leaves behind is retired rather than left standing.** The two states are
    /// different ids by construction — the media's uuid one way, a synthetic id the other — so the
    /// manifest holds two rows for one memory for as long as both are named, and
    /// [`Manifest::retire_absent`] is what closes the one this run cannot name. Without it the cost
    /// was up to one stale row PER ENTRY, in an ordinary case rather than an exotic one: a first run
    /// before the media part is extracted gives all 836 entries a synthetic `SourceMissing` row, and
    /// the second run adds 746 uuid rows, so a screen read `source_missing: 836` against a real gap
    /// of 90. It leaned the unsafe way too — a `Pending` row under a uuid whose file is gone was
    /// offered as work no run could finish. Pinned by
    /// `a_row_whose_identity_changed_between_runs_is_retired`.
    ///
    /// **The sweep does not run while [`Self::unreadable`] is non-empty**, which is the manifest's
    /// rule rather than this leg's: a dir that could not be listed is not evidence a memory is gone,
    /// and retiring on that evidence would park rows for media that is merely unseen. Same reasoning
    /// as [`MissingReason::Unscanned`], and the guard is scan-wide for the same reason.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when a manifest read or write fails.
    pub fn enroll(&self, manifest: &mut Manifest) -> Result<(), ManifestError> {
        let rows: Vec<NewItem<'_>> =
            self.items.iter().map(|item| NewItem { kind: ItemKind::Memory, source_id: &item.source_id, url: item.url.as_ref() }).collect();
        manifest.enroll(&rows)?;

        for item in &self.items {
            match &item.pairing {
                Pairing::Missing(reason) => {
                    manifest.mark_source_missing(ItemKind::Memory, &item.source_id, &reason.to_string())?;
                }
                Pairing::Exact(_) | Pairing::Ambiguous(_) => {
                    // `Retired` belongs here for the same reason `SourceMissing` does, and it is the
                    // arm that makes retiring reversible: a memory whose media went away and came
                    // back is named again, under the uuid its retired row already carries, and
                    // neither status is ever offered as work.
                    let status = manifest.item(ItemKind::Memory, &item.source_id)?.map(|row| row.status);
                    if matches!(status, Some(ItemStatus::SourceMissing | ItemStatus::Retired)) {
                        manifest.reset(ItemKind::Memory, &item.source_id)?;
                    }
                }
            }
        }

        // Every entry is an item, paired or not, so this names the whole enumeration and what is
        // left is what no `memories_history.json` entry answers for any more.
        let named: BTreeSet<&str> = self.items.iter().map(|item| item.source_id.as_str()).collect();
        manifest.retire_absent(ItemKind::Memory, &named, &self.unreadable)
    }
}

/// Joins `memories`' entries to `discovery`'s media, one bucket at a time.
///
/// A bucket holding one entry and one media set pairs exactly; anything else that pairs at all
/// pairs ambiguously, including the one entry of a bucket holding two files. Entries a bucket
/// cannot serve become [`Pairing::Missing`] and media no entry claimed lands in
/// [`Reconciliation::files_without_entry`], so neither side of a disagreement is dropped.
///
/// Two claim passes, both in the export's own order (decision 75): the exact-day claim first, then
/// the adjacent-day claim for entries the first pass left unpaired. An adjacent-day claim is
/// always [`Pairing::Ambiguous`] — the file's day disagrees with the entry's, so `Exact`'s "the
/// bucket held one of each, they belong together" contract cannot hold — and entries still
/// unmatched after both passes keep the [`MissingReason`] precedence below, unchanged.
#[must_use]
pub fn reconcile(memories: &Memories, discovery: Discovery) -> Reconciliation {
    let Discovery { media, orphan_overlays, unparsed, duplicates, unreadable } = discovery;

    let mut unclaimed: BTreeMap<Bucket, VecDeque<MemoryMedia>> = BTreeMap::new();
    for one in media {
        unclaimed.entry(one.bucket()).or_default().push_back(one);
    }
    // Taken before anything is popped: "exactly one" is a fact about the bucket, not about what is
    // left in it by the time the last entry looks.
    let files_per_bucket: BTreeMap<Bucket, usize> = unclaimed.iter().map(|(bucket, queue)| (*bucket, queue.len())).collect();

    let mut entries_per_bucket: BTreeMap<Bucket, usize> = BTreeMap::new();
    for bucket in memories.saved_media.iter().filter_map(Bucket::of) {
        *entries_per_bucket.entry(bucket).or_default() += 1;
    }

    let scan_incomplete = !unreadable.is_empty();
    let mut items = Vec::with_capacity(memories.saved_media.len());
    // Pass 1: the exact-day claim, in the export's own order. An entry that claims nothing here is
    // NOT verdicted yet — pass 2 may still pair it across a day boundary — so its bucket is
    // recorded for that pass rather than re-derived: `Bucket::of` is the one spelling of the key
    // and a second one written here could drift from it.
    let mut widen: Vec<(usize, &Memory, Bucket)> = Vec::new();
    for (entry_index, memory) in memories.saved_media.iter().enumerate() {
        let url = memory.media_download_url.clone().or_else(|| memory.download_link.clone());
        let bucket = Bucket::of(memory);
        let claimed = bucket.and_then(|bucket| {
            let media = draw(&bucket, &mut unclaimed)?;
            let alone = entries_per_bucket.get(&bucket) == Some(&1) && files_per_bucket.get(&bucket) == Some(&1);
            let source_id = media.uuid().to_owned();
            Some((source_id, if alone { Pairing::Exact(media) } else { Pairing::Ambiguous(media) }))
        });
        let (source_id, pairing) = match claimed {
            Some(claimed) => claimed,
            None => {
                if let Some(bucket) = bucket {
                    widen.push((entry_index, memory, bucket));
                }
                // An incomplete scan outranks `NoMedia`, because the manifest never hands a
                // source-missing row back as work: "no media exists" written off a scan that could
                // not read part of the source is a durable claim the run did not establish. It does
                // NOT outrank `NoDate`, which is a fact about the entry rather than about the
                // filesystem — an entry carrying no date pairs with nothing however much of the
                // source was read, so reporting it as unscanned would send a reader to fix
                // permissions that would not have helped.
                let reason = match (memory.date.is_none(), scan_incomplete) {
                    (true, _) => MissingReason::NoDate,
                    (false, true) => MissingReason::Unscanned,
                    (false, false) => MissingReason::NoMedia,
                };
                (synthetic_source_id(entry_index), Pairing::Missing(reason))
            }
        };
        items.push(MemoryItem { entry_index, source_id, url, pairing });
    }

    // Pass 2: the adjacent-day claim (decision 75). Snapchat writes the filename's day in a
    // different zone than the manifest's UTC date, so 36 of 846 entries land one bucket off their
    // own file, at filename-coverage level (n=2: 52 per export have no same-day filename, 16 none
    // within ±1). The window draws +1 day first, then -1, and only what pass 1 left: an entry
    // with a same-day file has already taken it, and an entry with none can only touch what no
    // exact-day claim wanted.
    for (entry_index, memory, bucket) in widen {
        let media = bucket.day.neighbors().and_then(|[next, previous]| {
            let next_bucket = Bucket { day: next, kind: bucket.kind };
            let previous_bucket = Bucket { day: previous, kind: bucket.kind };
            draw(&next_bucket, &mut unclaimed).or_else(|| draw(&previous_bucket, &mut unclaimed))
        });
        if let Some(media) = media {
            let url = memory.media_download_url.clone().or_else(|| memory.download_link.clone());
            // `entry_index` is the position this item was pushed at in pass 1, which iterated the
            // same list this entry came from.
            items[entry_index] = MemoryItem { entry_index, source_id: media.uuid().to_owned(), url, pairing: Pairing::Ambiguous(media) };
        }
    }

    Reconciliation {
        items,
        files_without_entry: unclaimed.into_values().flatten().collect(),
        orphan_overlays,
        unparsed,
        duplicates,
        unreadable,
    }
}

/// Draws one media set from `unclaimed` for `bucket`, if an earlier claim left one.
///
/// The one spelling of a claim, shared by the exact-day and the adjacent-day passes so neither can
/// disagree with the other about what claiming means.
fn draw(bucket: &Bucket, unclaimed: &mut BTreeMap<Bucket, VecDeque<MemoryMedia>>) -> Option<MemoryMedia> {
    unclaimed.get_mut(bucket)?.pop_front()
}

/// The manifest `source_id` for an entry no media paired with.
///
/// Per entry rather than one tally, so a later run that finds the media can address exactly that
/// memory. It cannot collide with a real uuid and that is structural rather than a naming
/// convention: [`is_uuid`] rejects this spelling, and `is_uuid` is the same predicate that decides
/// which filenames carry a uuid in the first place. Stable across re-runs of one
/// `memories_history.json` because a position in `Saved Media` is; nothing inside an entry is
/// unique enough to key on, since unpaired entries share days, kinds and coordinates with each
/// other by construction.
fn synthetic_source_id(entry_index: usize) -> String {
    format!("unpaired-entry-{entry_index}")
}

#[cfg(test)]
mod tests {
    use super::{Day, is_uuid, synthetic_source_id};

    const UUID: &str = "2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e";

    #[test]
    fn a_uuid_is_thirty_six_characters_with_dashes_in_four_fixed_places() {
        assert!(is_uuid(UUID));
        // Alphanumeric, not hex: a real filename is never refused over a character class.
        assert!(is_uuid("2ca92zzz-3ff7-45f1-95f9-a2fda6ba0f8e"));

        assert!(!is_uuid(&UUID[..35]), "one character short");
        assert!(!is_uuid(&format!("{UUID}a")), "one character long");
        assert!(!is_uuid(&UUID.replace('-', "0")), "no dashes at all");
        assert!(!is_uuid("2ca92da1_3ff7-45f1-95f9-a2fda6ba0f8e"), "a dash in the wrong place");
        assert!(!is_uuid("2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8."), "a character outside the alphabet");
        // `_` and `-` are the two the alphabet is most likely to be widened by, and a dash outside
        // the four fixed places has to fail as loudly as any other stray character.
        assert!(!is_uuid("2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8_"), "an underscore is outside the alphabet too");
        assert!(!is_uuid("2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8-"), "a fifth dash is not a uuid");
        assert!(!is_uuid(""));
    }

    #[test]
    fn no_synthetic_source_id_is_uuid_shaped() {
        // The lengths that could reach 36 characters, plus the boundaries around them.
        for index in [0, 1, 9, 90, 835, 836, 1_000_000, usize::MAX] {
            let synthetic = synthetic_source_id(index);
            assert!(!is_uuid(&synthetic), "{synthetic} would collide with a real memory uuid");
        }
        assert_eq!(synthetic_source_id(90), "unpaired-entry-90");
    }

    #[test]
    fn a_day_names_the_two_neighbor_days_across_rollovers() {
        let day = |text: &str| Day::parse(text).unwrap();
        // Month rollover.
        assert_eq!(day("2021-01-31").neighbors(), Some([day("2021-02-01"), day("2021-01-30")]));
        // Non-leap February, and the leap year beside it.
        assert_eq!(day("2021-02-28").neighbors(), Some([day("2021-03-01"), day("2021-02-27")]));
        assert_eq!(day("2020-02-29").neighbors(), Some([day("2020-03-01"), day("2020-02-28")]));
        // The 28 → 29 successor only exists in a leap year; a successor that hardcodes February
        // at 28 days would pass every other pin here.
        assert_eq!(day("2024-02-28").neighbors(), Some([day("2024-02-29"), day("2024-02-27")]));
        // Year rollover.
        assert_eq!(day("2020-12-31").neighbors(), Some([day("2021-01-01"), day("2020-12-30")]));
    }

    #[test]
    fn a_day_that_names_no_real_calendar_date_has_no_neighbors() {
        // `2021-02-30` parses — the day is range-checked, not calendar-checked — and a day that
        // does not exist must not invent neighbors or panic on the way there.
        assert_eq!(Day::parse("2021-02-30").unwrap().neighbors(), None);
    }
}
