//! Crash-safe per-export state: what a run still owes, what it finished, and what it could not
//! find media for at all.
//!
//! One sqlite database per export, named by the export's own id ([`ExportId`], the `<id>` half of
//! a `mydata~<id>` part name) and living in the platform's per-user data dir. Per-export rather
//! than one global database with an export column: a corrupt manifest then costs one export's
//! progress, and deleting an export's state is deleting one file. The database holds signed
//! download urls, which are secrets, so it is created `0600` before sqlite writes a byte to it and
//! never lands in the repo or the output tree.
//!
//! # What a row is
//!
//! One `items` table with an [`ItemKind`] discriminator, not a memories-only schema. Phase 3's
//! chat media and phase 4's history export resume through exactly these semantics, and a
//! discriminator is what keeps them from needing a migration to get them. `(kind, source_id)` is
//! the identity; `source_id` is whatever the export side already calls the record, so nothing here
//! invents an id.
//!
//! # What the output record means
//!
//! `output_path`, `checksum` and `bytes` are one RECORD between them: what a run wrote, and what it
//! hashed as it checked the file in. They are set on [`ItemStatus::Done`] and on the three PARKED
//! statuses — [`ItemStatus::SourceMissing`], [`ItemStatus::Retired`] and [`ItemStatus::Excluded`] —
//! when an earlier run finished the row before a later one parked it. Nothing is in flight under any
//! of the three: no run is part-way through that file, so the digest is not describing bytes about
//! to be overwritten, and dropping the record would drop the only pointer to a file nothing here
//! deletes. [`Manifest::mark_source_missing`], [`Manifest::exclude`] and
//! [`Manifest::retire_absent`] therefore leave all three standing (user's call, 2026-08-08, over
//! deleting the file and over keeping it unrecorded).
//!
//! The two WORK statuses clear all three instead. [`ItemStatus::Pending`] and
//! [`ItemStatus::Failed`] are where a half-written output lives, and a checksum next to bytes
//! something is about to overwrite is exactly what a checksum must never be allowed to describe, so
//! [`Manifest::mark_failed`], [`Manifest::reset`] and [`Manifest::resume`]'s demotion null them.
//!
//! **On a parked row the record is history, never a live claim about what is on disk.** Re-hashing
//! is the only thing that makes it current, and [`ItemStatus::Done`] is the only status
//! [`Manifest::resume`] re-hashes. So a parked row's file may be gone, or may hold another item's
//! bytes: the output tree belongs to the user and nothing here owns it, and this crate re-derives
//! each item's output path from the items present in the CURRENT run rather than reading it back off
//! the row ([`crate::export::chat_fix`] plans the collision-suffixed names that way), so an item
//! leaving the export can move a later one onto a name a parked row still claims. Neither is
//! detected and neither is meant to be — the record says what a run did, and only the status says
//! whether it still holds.
//!
//! **A present checksum is therefore not "this row is finished work".** Only `status == Done` says
//! that; a reader wanting finished work reads the status, and a reader wanting "some run wrote an
//! output for this row" reads the three fields. Pinned by
//! `a_parked_row_carrying_a_checksum_is_never_re_verified_as_finished_work`, which deletes the output
//! and asserts the row goes on naming it.
//!
//! # Resume contract
//!
//! [`Manifest::resume`] is the first thing a second run calls, once per [`ItemKind`] it is about
//! to work on. Per status:
//!
//! - [`ItemStatus::Done`] — the recorded output is re-hashed IN FULL and compared against the
//!   stored checksum and length. An item that agrees is skipped for the rest of the run; one whose
//!   file changed, vanished, or cannot be read is demoted to `Pending` with its checksum cleared
//!   and reported as a [`Demotion`]. This is the only place the re-verify runs. It is a full hash
//!   rather than a length check on purpose: a same-length rewrite is exactly the corruption a
//!   length check cannot see, and a manifest that lies about finished work is the one failure a
//!   resume cannot recover from.
//! - [`ItemStatus::Pending`] — untouched. It is the work list.
//! - [`ItemStatus::Failed`] — untouched. [`Manifest::pending`] hands it back while its
//!   `retry_count` is under the cap the caller passes, so an ordinary failure is retried and a
//!   permanently broken item parks itself instead of spinning.
//! - [`ItemStatus::SourceMissing`] — untouched, and never handed back as work. It is counted in
//!   the [`ResumeReport`] instead, which is the whole point of the state: the observed export has
//!   836 memory entries against 746 media files, and a run that silently succeeds at 746 reads as
//!   a clean run. A caller that later finds the media — an export part that was not extracted
//!   yet — calls [`Manifest::reset`] to put the item back on the work list.
//! - [`ItemStatus::Retired`] — untouched, never handed back as work, and counted apart from the
//!   gap above. [`Manifest::retire_absent`] is what puts a row here: an enumeration that can no
//!   longer name the row at all. Same way back as the gap, [`Manifest::reset`], for the run that
//!   finds the source again.
//! - [`ItemStatus::Excluded`] — untouched, never handed back as work, and counted apart from both
//!   of the above. [`Manifest::exclude`] is what puts a row here: a source that is present
//!   and readable and that this build deliberately writes no output for. Same way back,
//!   [`Manifest::reset`].
//!
//! Nothing here deletes a row, so a re-enumeration ([`Manifest::enroll`]) of the same export is
//! idempotent and never costs finished work. Retiring is what a row that outlived its source gets
//! instead of a delete: "gone since the first run" and "never in the export" stay different facts,
//! and a screen can count both.
//!
//! # Concurrency
//!
//! A [`Manifest`] owns one connection and is `Send` but not `Sync`. A concurrent downloader shares
//! one behind its own lock; two processes on one manifest is not a supported arrangement, and the
//! resume sweep is what makes an interrupted run's leftovers safe rather than any claim protocol.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::export::model::DownloadUrl;
use crate::export::walk::UnreadableDir;

/// Reverse-domain parts handed to [`ProjectDirs`]; only the last is used on linux.
const QUALIFIER: &str = "dev";
const ORGANIZATION: &str = "uwuclxdy";
const APPLICATION: &str = "exportsnap";

/// Manifests get their own subdir of the data dir so a future config or cache file cannot be
/// mistaken for one export's state.
const MANIFEST_SUBDIR: &str = "manifests";

/// The schema this build writes and is willing to read. Forward-only: a database carrying anything
/// else is refused rather than opened, because a silent open would let an older build write rows a
/// newer one already gave a different meaning.
const SCHEMA_VERSION: i32 = 1;

/// The `meta` key pinning which export a database belongs to.
const EXPORT_ID_KEY: &str = "export_id";

/// Blake3 hashes ~1 GB/s per core, so the read buffer, not the hash, is what a full re-verify
/// waits on.
const READ_BUFFER: usize = 64 * 1024;

/// Every column of `items`, in the order [`RawItem::from_row`] reads them.
///
/// A constant rather than a literal per query so a column added to one `SELECT` cannot be missed
/// in another. It is interpolated into sql, which is safe only because it is a constant: every
/// value in this module is bound, never formatted.
const ITEM_COLUMNS: &str = "kind, source_id, status, retry_count, url, output_path, checksum, bytes, last_error, updated_at";

const INSTALL_SQL: &str = "\
CREATE TABLE meta (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE items (
    kind        TEXT    NOT NULL,
    source_id   TEXT    NOT NULL,
    status      TEXT    NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    url         TEXT,
    output_path TEXT,
    checksum    TEXT,
    bytes       INTEGER,
    last_error  TEXT,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (kind, source_id)
) STRICT;

CREATE INDEX items_by_status ON items (kind, status);
";

// ---- identity ----

/// The `<id>` half of a `mydata~<id>` export part: Snapchat's own id for one delivery, and what
/// names the manifest file.
///
/// It comes from [`crate::export::zip::PartName::parse`] rather than from a hash of the source
/// path, because it survives the export dir being moved. The character set is restricted because
/// the id becomes a filename: `mydata~..` is a directory a filesystem accepts and a path segment
/// that escapes the manifest dir.
///
/// # Examples
///
/// ```
/// use exportsnap::export::manifest::ExportId;
/// use exportsnap::export::zip::PartName;
///
/// let part = PartName::parse("mydata~1784667002819").unwrap();
/// assert_eq!(ExportId::new(&part.id).unwrap().as_str(), "1784667002819");
///
/// assert!(ExportId::new("..").is_none());
/// assert!(ExportId::new("").is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExportId(String);

impl ExportId {
    /// `None` for an empty id or one carrying anything outside ascii alphanumerics, `-` and `_`.
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        let usable = !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        usable.then(|| Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExportId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---- item vocabulary ----

/// Which pipeline an item belongs to.
///
/// The discriminator that lets one table carry phase 2's memories, phase 3's chat media and phase
/// 4's history exports without a schema change between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKind {
    Memory,
    ChatMedia,
    HistoryExport,
}

impl ItemKind {
    pub const ALL: [Self; 3] = [Self::Memory, Self::ChatMedia, Self::HistoryExport];

    /// The word stored in the `kind` column.
    #[must_use]
    pub const fn as_stored(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::ChatMedia => "chat_media",
            Self::HistoryExport => "history_export",
        }
    }

    fn from_stored(raw: &str) -> Result<Self, ManifestError> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_stored() == raw)
            .ok_or_else(|| ManifestError::CorruptRow { column: Column::Kind, value: raw.to_owned() })
    }
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_stored())
    }
}

/// Where an item stands. See the module's resume contract for what a second run does with each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemStatus {
    /// Enumerated and still owed.
    Pending,
    /// Finished, with the bytes on disk hashed and recorded.
    Done,
    /// An attempt failed and said why. `retry_count` counts the recorded failures.
    Failed,
    /// The export names this item but holds no media for it: no file on disk and no usable
    /// download url. Not a failure of the run and not work it can retry, so it is reported rather
    /// than retried or hidden.
    SourceMissing,
    /// An earlier run enrolled this row and the export no longer names it at all — not as an item
    /// and not as a gap. Not work, and not part of the gap above: [`Self::SourceMissing`] is an
    /// entry the export still names and holds nothing for, while this is a row whose whole record
    /// left the export. Kept rather than deleted so the two stay tellable apart, and so a screen can
    /// say how many items vanished since the first run. [`Manifest::retire_absent`] is the only
    /// producer.
    Retired,
    /// Enrolled, and deliberately never written. The source is present and readable and this build
    /// produces no output for it at all — decision 44d's dropped chat-media thumbnails are the only
    /// producer today, through [`Manifest::exclude`].
    ///
    /// Counted apart from every neighbour because it is a different fact from each of them. Not
    /// [`Self::Done`]: this build writes nothing for it, so a resume has nothing to re-hash — a row
    /// an EARLIER build wrote keeps that output and its record across the transition, which is the
    /// module's rule for every parked status and still not a reason to call this finished. Not
    /// [`Self::Failed`]: no attempt was made and retrying would change nothing. Not
    /// [`Self::SourceMissing`] or [`Self::Retired`]: the source is right there, and it is this
    /// build's own rule rather than the export that decides the row produces nothing. Folding it
    /// into any of those would report a gap the export does not have.
    ///
    /// [`Manifest::reset`] is the way back, for the build whose rules change.
    Excluded,
}

impl ItemStatus {
    pub const ALL: [Self; 6] = [Self::Pending, Self::Done, Self::Failed, Self::SourceMissing, Self::Retired, Self::Excluded];

    /// The word stored in the `status` column.
    #[must_use]
    pub const fn as_stored(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::SourceMissing => "source_missing",
            Self::Retired => "retired",
            Self::Excluded => "excluded",
        }
    }

    fn from_stored(raw: &str) -> Result<Self, ManifestError> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_stored() == raw)
            .ok_or_else(|| ManifestError::CorruptRow { column: Column::Status, value: raw.to_owned() })
    }
}

impl fmt::Display for ItemStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_stored())
    }
}

/// A blake3 digest of a finished output file.
///
/// Blake3 rather than a sha2 because the resume contract re-hashes every finished byte, and on a
/// multi-gigabyte memories dir that choice is the difference between a fast resume and a slow one.
/// It detects corruption and truncation; Snapchat publishes no digests, so there is nothing here
/// to authenticate against and speed is the binding constraint rather than cryptographic pedigree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checksum(blake3::Hash);

impl Checksum {
    /// Hashes `path` in one pass, returning the digest and the number of bytes it covered so the
    /// two can never be recorded out of step.
    ///
    /// # Errors
    ///
    /// Returns the io error from opening or reading `path`.
    pub fn of_file(path: impl AsRef<Path>) -> io::Result<(Self, u64)> {
        let file = fs::File::open(path)?;
        let mut hasher = blake3::Hasher::new();
        let bytes = io::copy(&mut BufReader::with_capacity(READ_BUFFER, file), &mut hasher)?;
        Ok((Self(hasher.finalize()), bytes))
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex().to_string()
    }

    /// `None` for anything that is not 64 hex characters.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        blake3::Hash::from_hex(text).ok().map(Self)
    }
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_hex())
    }
}

// ---- rows ----

/// A record the export names and a run may owe work for.
#[derive(Debug, Clone)]
pub struct NewItem<'a> {
    pub kind: ItemKind,
    /// Whatever the export side already calls this record.
    pub source_id: &'a str,
    /// The signed download url, when the export carried one. Every url in the one observed export
    /// is empty, so `None` is the normal answer there and the media has to come off disk instead.
    pub url: Option<&'a DownloadUrl>,
}

/// One item as the manifest holds it.
///
/// `Debug` is derived and stays safe because [`DownloadUrl`] redacts itself; that is the whole
/// reason the url is not a `String` here.
#[derive(Debug, Clone)]
pub struct Item {
    pub kind: ItemKind,
    pub source_id: String,
    pub status: ItemStatus,
    /// Recorded failures, bumped by [`Manifest::mark_failed`] and cleared by [`Manifest::reset`].
    pub retry_count: u32,
    /// Where a run wrote this item's output, with `checksum` and `bytes` describing that file.
    ///
    /// Set on [`ItemStatus::Done`] and on a parked row an earlier run finished; cleared on
    /// [`ItemStatus::Pending`] and [`ItemStatus::Failed`]. See the module's output-record rule. It
    /// says an output exists, never that the row is finished work — read `status` for that.
    pub output_path: Option<PathBuf>,
    pub checksum: Option<Checksum>,
    pub bytes: Option<u64>,
    /// Why the last attempt failed, or why there is no source, reduced to its prose tokens on the
    /// way in: a token survives only if it holds none of `/ = % & @` and is under 64 characters.
    pub last_error: Option<String>,
    pub url: Option<DownloadUrl>,
    /// Unix seconds of the last status transition, or of the last change to what the row records
    /// under an unchanged status — [`Manifest::mark_source_missing`] restamps a gap row whose reason
    /// changed, and [`Manifest::mark_failed`] restamps every recorded attempt.
    ///
    /// What it is NOT is the last run: none of the three parked writers rewrites a row it would
    /// leave unchanged, which is what keeps "when did this vanish from the export" answerable off a
    /// column every run's re-derivation would otherwise touch. Each names its own pin.
    pub updated_at: i64,
}

/// Why the resume sweep took an item back off the finished pile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemotionReason {
    /// The recorded output is not there any more.
    Vanished,
    /// It is there and its bytes are not the ones that were recorded.
    Changed,
    /// It is there and could not be read, so it cannot be trusted either.
    Unreadable,
    /// The row claims to be finished without a checksum to check it against. This module never
    /// writes that combination; a database edited by hand can hold it.
    Incomplete,
}

impl fmt::Display for DemotionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Vanished => "its output file is gone",
            Self::Changed => "its output file no longer matches the recorded checksum",
            Self::Unreadable => "its output file could not be read",
            Self::Incomplete => "the manifest recorded it finished without a checksum",
        })
    }
}

/// One item the resume sweep put back on the work list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demotion {
    pub kind: ItemKind,
    pub source_id: String,
    pub reason: DemotionReason,
}

/// What one [`ItemKind`] looked like after a resume swept it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeReport {
    /// Items whose recorded output no longer holds up, each back on the work list.
    pub demoted: Vec<Demotion>,
    /// Items whose output re-hashed to exactly what was recorded. These are skipped.
    pub verified: u64,
    /// Items still owed, demotions included.
    pub pending: u64,
    /// Items parked on a recorded failure, whatever the caller's retry cap.
    pub failed: u64,
    /// Items the export names but holds no media for. Report this; a run that quietly finishes
    /// without it looks complete.
    pub source_missing: u64,
    /// Items a later enumeration could no longer name at all, so their source left the export
    /// between two runs. Counted apart from [`Self::source_missing`] because the two are different
    /// facts and only the first is a gap in the export as it stands.
    pub retired: u64,
    /// Items this build deliberately writes no output for. Counted apart from both of the above
    /// because nothing is missing and nothing vanished: the source is present and the build chose
    /// not to write it, so a run reporting these as a gap would send a user looking for media that
    /// is exactly where it always was.
    pub excluded: u64,
}

// ---- errors ----

/// A column whose stored text can fail to parse back into its type.
///
/// A closed set, and the url column is deliberately not in it: `url` is read back as an opaque
/// [`DownloadUrl`] and never parsed, so no failure path can carry a signed url into an error
/// message. Same reasoning as [`crate::export::model::Field`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Kind,
    Status,
    Checksum,
    RetryCount,
    Bytes,
}

impl Column {
    const fn as_sql(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::Status => "status",
            Self::Checksum => "checksum",
            Self::RetryCount => "retry_count",
            Self::Bytes => "bytes",
        }
    }
}

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_sql())
    }
}

/// What is wrong with an output path a caller handed to [`Manifest::mark_done`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathProblem {
    Relative,
    NotUtf8,
}

/// Something went wrong reaching or reading the manifest.
///
/// [`Self::Output`] is the one recoverable member: the file may be back on the next attempt, so a
/// caller retries it. The rest mean the environment, the database, or the call is wrong, and
/// retrying reproduces them.
#[derive(Debug)]
pub enum ManifestError {
    /// No per-user data dir: the platform gave no home directory to put one in.
    NoDataDir,
    /// The manifest dir or its database file could not be created.
    Create { path: PathBuf, source: io::Error },
    /// Sqlite refused an operation. A broken database or a bug here, not bad input.
    Sqlite { op: &'static str, path: PathBuf, source: rusqlite::Error },
    /// The database carries a schema version this build does not know.
    FutureSchema { path: PathBuf, found: i32, supported: i32 },
    /// The database belongs to a different export than the one it was opened for.
    WrongExport { path: PathBuf, found: String, wanted: String },
    /// The database says which schema it carries but not which export, which this build never
    /// writes.
    MissingExportPin { path: PathBuf },
    /// A stored value cannot be read back into its type.
    CorruptRow { column: Column, value: String },
    /// The output file handed to [`Manifest::mark_done`] could not be read to hash it.
    Output { path: PathBuf, source: io::Error },
    /// An output path the manifest cannot store or re-resolve on a later run.
    OutputPath { path: PathBuf, problem: PathProblem },
    /// A transition named an item no run ever enrolled.
    UnknownItem { kind: ItemKind, source_id: String },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDataDir => write!(
                f,
                "no per-user data directory to keep the download manifest in; set HOME (or XDG_DATA_HOME) so resume state has somewhere private to live"
            ),
            Self::Create { path, source } => {
                write!(f, "could not create the manifest at {}: {source}; check the directory is writable", path.display())
            }
            Self::Sqlite { op, path, source } => write!(
                f,
                "could not {op} in the manifest at {}: {source}; if this repeats, delete that file to redo this export's downloads from scratch",
                path.display()
            ),
            Self::FutureSchema { path, found, supported } => write!(
                f,
                "the manifest at {} was written with schema version {found} and this build reads {supported}; \
                 upgrade exportsnap, or delete that file to redo this export's downloads from scratch",
                path.display()
            ),
            Self::WrongExport { path, found, wanted } => write!(
                f,
                "the manifest at {} holds export {found}, not {wanted}; it was renamed or copied, so move it back or delete it",
                path.display()
            ),
            Self::MissingExportPin { path } => write!(
                f,
                "the manifest at {} carries no export id; it was edited outside exportsnap, so delete it to redo this \
                 export's downloads from scratch",
                path.display()
            ),
            // Two causes reach this and the message must not pick one: a NEWER build wrote a word
            // this one does not know (every status added since is exactly that), or the file was
            // edited outside exportsnap. Only the second is repaired by deleting, and prescribing a
            // delete for the first destroys resumable state that upgrading would have read fine.
            Self::CorruptRow { column, value } => write!(
                f,
                "the manifest's {column} column holds {value:?}, which this build cannot read; a newer exportsnap may have \
                 written it, or the file was edited outside exportsnap — upgrade first, and delete that file to redo this \
                 export's downloads from scratch only if upgrading does not help"
            ),
            Self::Output { path, source } => write!(f, "could not read {} to check it in: {source}", path.display()),
            Self::OutputPath { path, problem } => match problem {
                PathProblem::Relative => write!(
                    f,
                    "the manifest stores absolute output paths only and {} is relative; a resume runs from a different \
                     working directory than the run that wrote it, so join it onto the output root first",
                    path.display()
                ),
                PathProblem::NotUtf8 => write!(
                    f,
                    "the manifest stores output paths as text and {} is not valid utf-8; rename it or pick an output \
                     directory whose name is",
                    path.display()
                ),
            },
            Self::UnknownItem { kind, source_id } => {
                write!(f, "no {kind} item {source_id:?} in the manifest; enroll it before recording work against it")
            }
        }
    }
}

impl Error for ManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Create { source, .. } | Self::Output { source, .. } => Some(source),
            Self::Sqlite { source, .. } => Some(source),
            Self::NoDataDir
            | Self::FutureSchema { .. }
            | Self::WrongExport { .. }
            | Self::MissingExportPin { .. }
            | Self::CorruptRow { .. }
            | Self::OutputPath { .. }
            | Self::UnknownItem { .. } => None,
        }
    }
}

// ---- the manifest ----

/// The directory manifests live in, under the platform's per-user data dir.
///
/// # Errors
///
/// Returns [`ManifestError::NoDataDir`] when the platform names no home directory.
pub fn manifest_dir() -> Result<PathBuf, ManifestError> {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .map(|dirs| dirs.data_dir().join(MANIFEST_SUBDIR))
        .ok_or(ManifestError::NoDataDir)
}

/// One export's resume state.
#[derive(Debug)]
pub struct Manifest {
    conn: Connection,
    path: PathBuf,
}

impl Manifest {
    /// Opens (creating it if needed) the manifest for `export` in the per-user data dir.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when the data dir cannot be resolved or created, or when the
    /// database cannot be opened, migrated, or matched to `export`.
    pub fn open(export: &ExportId) -> Result<Self, ManifestError> {
        Self::open_in(manifest_dir()?, export)
    }

    /// Opens the manifest for `export` in `dir`, which the caller owns.
    ///
    /// The database file is `0600` before sqlite touches it, and on unix sqlite gives its `-wal`
    /// and `-shm` sidecars the main file's mode, so the whole set stays owner-only.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when `dir` or the database cannot be created, when the database
    /// carries a schema version this build does not read, or when it belongs to another export.
    pub fn open_in(dir: impl AsRef<Path>, export: &ExportId) -> Result<Self, ManifestError> {
        let dir = dir.as_ref();
        create_private_dir(dir).map_err(|source| ManifestError::Create { path: dir.to_path_buf(), source })?;

        let path = dir.join(format!("{export}.sqlite"));
        reserve_private(&path).map_err(|source| ManifestError::Create { path: path.clone(), source })?;

        let conn = Connection::open(&path).map_err(|source| sqlite_error("open the database", &path, source))?;
        let mut manifest = Self { conn, path };
        manifest.configure()?;
        manifest.migrate(export)?;
        Ok(manifest)
    }

    /// The database file backing this manifest.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records `items`, leaving whatever progress a previous run made against them intact.
    ///
    /// Idempotent: re-enumerating the same export refreshes urls and adds rows that are new, and
    /// touches no status. One transaction, because a per-item commit turns an 836-entry export
    /// into 836 fsyncs.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Sqlite`] when the write fails.
    pub fn enroll(&mut self, items: &[NewItem<'_>]) -> Result<(), ManifestError> {
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| sqlite_error("enroll items", &path, source))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO items (kind, source_id, status, retry_count, url, updated_at) \
                     VALUES (?1, ?2, ?3, 0, ?4, unixepoch()) \
                     ON CONFLICT (kind, source_id) DO UPDATE SET url = COALESCE(excluded.url, url)",
                )
                .map_err(|source| sqlite_error("enroll items", &path, source))?;
            for item in items {
                stmt.execute(params![
                    item.kind.as_stored(),
                    item.source_id,
                    ItemStatus::Pending.as_stored(),
                    item.url.map(DownloadUrl::expose)
                ])
                .map_err(|source| sqlite_error("enroll items", &path, source))?;
            }
        }
        tx.commit().map_err(|source| sqlite_error("enroll items", &path, source))
    }

    /// Checks a finished item in: hashes `output` and records the digest, its length and its path.
    ///
    /// The manifest hashes the file itself rather than taking a caller's digest, so what it
    /// records is what is on disk and the resume sweep compares like with like.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::OutputPath`] for a relative or non-utf-8 path,
    /// [`ManifestError::Output`] when `output` cannot be read, [`ManifestError::UnknownItem`] when
    /// nothing enrolled that item, and [`ManifestError::Sqlite`] when the write fails.
    pub fn mark_done(&self, kind: ItemKind, source_id: &str, output: impl AsRef<Path>) -> Result<(), ManifestError> {
        let output = output.as_ref();
        let stored = stored_path(output)?;
        let (checksum, bytes) = Checksum::of_file(output).map_err(|source| ManifestError::Output { path: output.to_path_buf(), source })?;
        // Sqlite's INTEGER is signed, and a length the column cannot hold has to fail the check-in
        // rather than saturate: a stored length that disagrees with the file demotes the item on
        // every resume for ever.
        let bytes = i64::try_from(bytes).map_err(|_| ManifestError::Output {
            path: output.to_path_buf(),
            source: io::Error::other("larger than the manifest can record a length for"),
        })?;

        let changed = self
            .conn
            .execute(
                "UPDATE items SET status = ?1, output_path = ?2, checksum = ?3, bytes = ?4, last_error = NULL, \
                 updated_at = unixepoch() WHERE kind = ?5 AND source_id = ?6",
                params![ItemStatus::Done.as_stored(), stored, checksum.to_hex(), bytes, kind.as_stored(), source_id],
            )
            .map_err(|source| sqlite_error("record a finished item", &self.path, source))?;
        self.require_hit(changed, kind, source_id)
    }

    /// Records a failed attempt and bumps the item's retry count.
    ///
    /// **Clears the output record, unlike the parked statuses**, because `Failed` is a WORK status:
    /// the item comes back through [`Self::pending`] and the next attempt overwrites whatever is at
    /// that path, so the recorded digest would be describing bytes about to change. Pinned by
    /// `a_finished_row_driven_back_to_work_by_a_failure_drops_the_record_and_keeps_the_file`.
    ///
    /// `note` is reduced to its plainly-prose tokens before it is stored. An http client's error
    /// message routinely carries the url it was fetching — `reqwest`'s does — and a signed url in
    /// `last_error` is the same secret as the url column with none of its protection. The
    /// reduction is an allowlist rather than a url detector, so a spelling nobody anticipated is
    /// dropped instead of passed: a token survives only if it holds none of `/ = % & @` and is no
    /// longer than 64 characters, which also means a unix path in a note is dropped with it.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::UnknownItem`] when nothing enrolled that item, and
    /// [`ManifestError::Sqlite`] when the write fails.
    pub fn mark_failed(&self, kind: ItemKind, source_id: &str, note: &str) -> Result<(), ManifestError> {
        let note = self.redacted(kind, source_id, note)?;
        let changed = self
            .conn
            .execute(
                "UPDATE items SET status = ?1, retry_count = retry_count + 1, output_path = NULL, checksum = NULL, \
                 bytes = NULL, last_error = ?2, updated_at = unixepoch() WHERE kind = ?3 AND source_id = ?4",
                params![ItemStatus::Failed.as_stored(), note, kind.as_stored(), source_id],
            )
            .map_err(|source| sqlite_error("record a failed attempt", &self.path, source))?;
        self.require_hit(changed, kind, source_id)
    }

    /// Records that the export names this item but holds no media for it.
    ///
    /// Not an attempt, so the retry count is left alone. `reason` is reduced to prose exactly like
    /// [`Self::mark_failed`]'s note; the same channel gets the same guard.
    ///
    /// **Not a statement about output either**, which is why the three output columns are absent
    /// from the statement below rather than nulled by it: this says the SOURCE is gone, and a row an
    /// earlier run finished still has that run's file on disk under the digest it was checked in
    /// with. That is the whole chat-media case — a message's file vanishing between two runs drives
    /// the row it already finished straight here — and nulling the columns would drop the only
    /// pointer to a file nothing in this crate deletes. See the module's output-record rule; pinned
    /// by `a_finished_row_parked_as_a_gap_keeps_its_output_and_the_record_of_it`.
    ///
    /// **Re-stating a gap a row already carries touches nothing**, the property [`Self::exclude`]
    /// and [`Self::retire_absent`] pay for and for the same reason: both media legs call this once
    /// per gap row on EVERY run — [`crate::export::memories::Reconciliation::enroll`] for the
    /// observed export's 90 unpaired entries, [`crate::export::chat_media`] for its own — so an
    /// unconditional statement would rewrite `updated_at`, documented on [`Item::updated_at`], on
    /// every run for every gap row, turning it into the last RUN for exactly the rows that answer
    /// "when did this vanish".
    ///
    /// The condition is `status <> ?1 OR last_error IS NOT ?2` rather than the status alone,
    /// because unlike those two this note is CALLER TEXT and one row's reason genuinely changes
    /// between runs. **Both legs**, not just the memories one: each has an `Unscanned` reason
    /// chosen once per run off the walk's unreadable list and displacing that leg's filesystem
    /// verdict for every row of the run — [`crate::export::memories::MissingReason::Unscanned`] and
    /// [`crate::export::chat_media::MissingReason::Unscanned`] — so a run that hits one unlistable
    /// directory writes it everywhere and the next clean run writes the real reason for the same
    /// rows. A status-only guard would freeze the stale reason with the status column reading
    /// correct.
    ///
    /// **What generalizes is narrower than "the predicate is the SET list", which queue task 49
    /// corrected to the axis this module already draws to decide which notes reach the redactor.**
    /// **Among the statements this module makes CONDITIONAL**, a column belongs in the guard when its
    /// value comes from this run's observation, because two runs of one build can then disagree about
    /// it — that is `reason` here, and the `Unscanned` paragraph above is the whole case. A module
    /// constant is the other half of the axis: no run computes it, so no two runs can disagree, and
    /// [`Self::exclude`] and [`Self::retire_absent`] guard on the status alone for that reason rather
    /// than by oversight. What a constant pays instead is a ceiling written on the constant itself,
    /// which both carry: rewording one reaches no row an earlier run already parked. The trade and
    /// its rejected alternative are in design.md's manifest notes.
    ///
    /// **The leading clause is load-bearing and the rule is wrong without it.** Two writers here put
    /// run-derived text in `last_error` under no guard at all — [`Self::mark_failed`] and
    /// [`Self::resume`]'s demotion, whose note is a third origin again, neither caller text nor
    /// module constant but a [`DemotionReason`] this run computed. Two more null the column just as
    /// unconditionally ([`Self::mark_done`], [`Self::reset`]); `NULL` is neither text nor
    /// run-derived, so they are outside what this rule is about and are named here only so the next
    /// reader counting `UPDATE`s does not think one was missed. All four are unconditional because
    /// they record an EVENT rather than re-derive a standing
    /// verdict, so there is no repeat to skip. Reading the rule as a bald universal and adding a note
    /// clause to [`Self::mark_failed`] would be actively harmful: that statement also does
    /// `retry_count = retry_count + 1`, so an identically-repeating failure would stop incrementing
    /// and [`Self::pending`]'s `retry_count < ?4` would go on offering the row for ever.
    ///
    /// `IS NOT` rather than `<>` because `last_error` is nullable and `<>` against `NULL` is
    /// `NULL`, which is not true, so the plain operator would skip a row whose note has to be
    /// written. **No call here stores a note-less gap row** — every one of them writes a note — so
    /// that is a future writer's footgun closed rather than a live one, and the pin below reaches
    /// the row by editing the database rather than through this API. Pinned by
    /// `re_marking_a_gap_row_with_the_same_reason_leaves_it_untouched`,
    /// `a_gap_row_is_written_whenever_its_status_or_its_reason_differs`,
    /// `a_gap_row_carrying_no_note_is_given_one_rather_than_read_as_already_saying_it`, and — over
    /// two whole runs of the leg that pays for this — `memories`'
    /// `two_runs_over_an_unchanged_export_leave_a_gap_rows_timestamp_alone`.
    ///
    /// The read below is what keeps [`ManifestError::UnknownItem`] answerable once a zero row count
    /// no longer implies the row is absent — [`Self::exclude`]'s shape, and required rather than
    /// decorative: a bare `require_hit` would call every already-parked row unknown.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::UnknownItem`] when nothing enrolled that item,
    /// [`ManifestError::CorruptRow`] when the read's stored status no longer parses, and
    /// [`ManifestError::Sqlite`] when a read or the write fails.
    pub fn mark_source_missing(&self, kind: ItemKind, source_id: &str, reason: &str) -> Result<(), ManifestError> {
        let reason = self.redacted(kind, source_id, reason)?;
        let changed = self
            .conn
            .execute(
                "UPDATE items SET status = ?1, last_error = ?2, \
                 updated_at = unixepoch() WHERE kind = ?3 AND source_id = ?4 AND (status <> ?1 OR last_error IS NOT ?2)",
                params![ItemStatus::SourceMissing.as_stored(), reason, kind.as_stored(), source_id],
            )
            .map_err(|source| sqlite_error("record a missing source", &self.path, source))?;
        if changed != 0 {
            return Ok(());
        }
        // Nothing changed means one of exactly two things, since the only row the statement above
        // can miss is one already carrying both values it writes: the row already says this, or
        // nothing enrolled it. Only a read tells them apart, and the status column alone answers it
        // — a row present here can only be `SourceMissing`, so a full-row read would materialize an
        // output path, a url and a checksum the verdict never looks at.
        let stored: Option<String> = self
            .conn
            .query_row("SELECT status FROM items WHERE kind = ?1 AND source_id = ?2", params![kind.as_stored(), source_id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|source| sqlite_error("record a missing source", &self.path, source))?;
        match stored {
            Some(stored) if ItemStatus::from_stored(&stored)? == ItemStatus::SourceMissing => Ok(()),
            _ => Err(ManifestError::UnknownItem { kind, source_id: source_id.to_owned() }),
        }
    }

    /// Records that this build writes no output for these items at all.
    ///
    /// Not an attempt and not a gap, so the retry count is left alone. The note is a constant this
    /// module owns rather than caller text, for the reason [`Self::retire_absent`]'s note gives: a
    /// fixed string holds no secret, so it skips the redactor and the per-row url read that would
    /// pay for it.
    ///
    /// **Writing `Excluded` over an already-`Excluded` row touches nothing.** That is the same
    /// property [`Self::retire_absent`] pays for and it is load-bearing for the same reason: an
    /// excluded row is re-derived by every later run from the same rule, so a statement matching it
    /// unconditionally would rewrite `updated_at` — documented on [`Item::updated_at`] as the last
    /// time the row's own state moved, and explicitly not as the last run — on every run, for every
    /// excluded row, turning that column into exactly the thing it says it is not. The
    /// `status <> ?1` clause is what makes the write conditional, and the read below is what keeps
    /// [`ManifestError::UnknownItem`] answerable once a zero row count no longer implies the row is
    /// absent. Pinned by `excluding_an_already_excluded_row_leaves_it_untouched`.
    ///
    /// **The note is deliberately not a second half of that clause**, which is where this parts
    /// company with [`Self::mark_source_missing`]'s `status <> ?1 OR last_error IS NOT ?2`: that
    /// note is caller text two runs of one build genuinely disagree about, and this one is a
    /// constant no run computes. The consequence a reader has to be able to see is that rewording
    /// the constant reaches no already-excluded row, and it is written on the constant rather than
    /// here, because that is the line someone rewording it is looking at. Pinned by
    /// `an_already_excluded_rows_note_is_frozen_at_the_run_that_parked_it`.
    ///
    /// Every other status is overwritten, [`ItemStatus::Done`] included: the plan deciding this row
    /// produces nothing is a statement about the row as it stands rather than about what an earlier
    /// build did with it. What it is NOT a statement about is the file that earlier build already
    /// wrote, so a finished row keeps its output path, its checksum and its length across the
    /// transition — the module's rule for every parked status, and the reason those three columns
    /// are absent from the statement below. Nothing is orphaned: the file stays on disk because
    /// nothing here deletes one, and the row goes on naming it. Pinned by
    /// `a_finished_row_this_build_stops_writing_keeps_its_output_and_the_record_of_it`.
    ///
    /// **One transaction for the whole set**, the same shape [`Self::retire_absent`] uses and for the
    /// same reason: a per-item commit is a per-item fsync, and both of these can touch every row of a
    /// kind. The two used to disagree on that question and no longer do.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::UnknownItem`] when nothing enrolled one of the items — the whole
    /// transaction rolls back — [`ManifestError::CorruptRow`] when the read's stored status no
    /// longer parses, and [`ManifestError::Sqlite`] when a read or a write fails.
    pub fn exclude(&mut self, kind: ItemKind, source_ids: &[String]) -> Result<(), ManifestError> {
        /// What an excluded row's `last_error` says.
        ///
        /// **Rewording this strands every row an earlier run already excluded.** The guard below
        /// skips such a row on its status alone, and nothing else in this module rewrites a parked
        /// row's note in place, so an existing database goes on carrying the old sentence with its
        /// status column reading correct. Queue task 49 ruled that cost worth paying rather than
        /// joining the note to the guard: repairing the note that way restamps `updated_at`, the one
        /// column here holding a fact no other column can reconstruct, and this note holds none —
        /// `status` already says exactly this. Upgrade path, if a reword ever has to reach old rows:
        /// add `OR last_error IS NOT ?2` the way [`Manifest::mark_source_missing`] has it (`IS NOT`
        /// because the column is nullable) and accept the restamp. A reword is loud rather than
        /// silent because `an_already_excluded_rows_note_is_frozen_at_the_run_that_parked_it` pins
        /// these bytes.
        ///
        /// **A stranded note is only ever replaced by the row LEAVING this status.** Every statement
        /// in this module that writes `last_error` also writes `status`, and the one that would land
        /// back on `Excluded` is the guarded statement below, so nothing refreshes the note in place.
        /// [`Manifest::reset`] is the deliberate route out; [`Manifest::retire_absent`] is an
        /// incidental one and is reachable — an excluded row the export stops naming is not exempt
        /// from that sweep, which is the settled answer rather than a missed exemption, pinned by
        /// `a_vanished_excluded_row_is_retired`.
        const EXCLUDED_NOTE: &str = "this build writes no output for this item";

        if source_ids.is_empty() {
            return Ok(());
        }
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| sqlite_error("record excluded items", &path, source))?;
        {
            let mut write = tx
                .prepare(
                    "UPDATE items SET status = ?1, last_error = ?2, \
                     updated_at = unixepoch() WHERE kind = ?3 AND source_id = ?4 AND status <> ?1",
                )
                .map_err(|source| sqlite_error("record excluded items", &path, source))?;
            // The status column alone, not a whole row. This runs once per already-excluded item on
            // every run after the first, and a full-row read would materialize an output path, a url
            // and a checksum the verdict never looks at — the narrowing `retire_absent`'s own warning
            // asks for, which is safe here only because nothing pushes the status filter into sql.
            let mut status_of = tx
                .prepare("SELECT status FROM items WHERE kind = ?1 AND source_id = ?2")
                .map_err(|source| sqlite_error("record excluded items", &path, source))?;

            for source_id in source_ids {
                let changed = write
                    .execute(params![ItemStatus::Excluded.as_stored(), EXCLUDED_NOTE, kind.as_stored(), source_id])
                    .map_err(|source| sqlite_error("record excluded items", &path, source))?;
                if changed != 0 {
                    continue;
                }
                // Nothing changed means one of exactly two things, since the only row the statement
                // above can miss is one already carrying the status it writes: the row is already
                // excluded, or nothing enrolled it. Only a read tells them apart.
                let stored: Option<String> = status_of
                    .query_row(params![kind.as_stored(), source_id], |row| row.get(0))
                    .optional()
                    .map_err(|source| sqlite_error("record excluded items", &path, source))?;
                match stored {
                    Some(stored) if ItemStatus::from_stored(&stored)? == ItemStatus::Excluded => {}
                    _ => return Err(ManifestError::UnknownItem { kind, source_id: source_id.clone() }),
                }
            }
        }
        tx.commit().map_err(|source| sqlite_error("record excluded items", &path, source))
    }

    /// Puts an item back on the work list as if no run had ever touched it, retry count included.
    ///
    /// This is the way out of [`ItemStatus::SourceMissing`], of [`ItemStatus::Retired`] and of
    /// [`ItemStatus::Excluded`]: a caller that finds the media a previous run could not — in an
    /// export part that was not extracted yet — calls this, and so does a build whose rule about
    /// what to write has changed. None of the three is ever offered as work, so a row that becomes
    /// workable again needs this call or it stays parked for ever.
    ///
    /// **This is where a parked row's output record ends**, and it is the only place it does. The
    /// three parked statuses keep it, `Pending` is a WORK status and cannot: the item is about to be
    /// re-fixed and its output overwritten, so the digest would describe bytes on their way out. The
    /// file an earlier run wrote is not deleted, only unrecorded — a caller wanting it kept has to
    /// read it off the row before calling this. Pinned by
    /// `a_finished_row_reset_to_pending_drops_the_record_and_keeps_the_file`.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::UnknownItem`] when nothing enrolled that item, and
    /// [`ManifestError::Sqlite`] when the write fails.
    pub fn reset(&self, kind: ItemKind, source_id: &str) -> Result<(), ManifestError> {
        let changed = self
            .conn
            .execute(
                "UPDATE items SET status = ?1, retry_count = 0, output_path = NULL, checksum = NULL, bytes = NULL, \
                 last_error = NULL, updated_at = unixepoch() WHERE kind = ?2 AND source_id = ?3",
                params![ItemStatus::Pending.as_stored(), kind.as_stored(), source_id],
            )
            .map_err(|source| sqlite_error("reset an item", &self.path, source))?;
        self.require_hit(changed, kind, source_id)
    }

    /// Retires every row of `kind` this run's enumeration cannot name, leaving the finished ones
    /// alone.
    ///
    /// `named` is every `source_id` the caller's reconciliation can produce — the items it found
    /// AND the gaps it recorded — so a row outside it is one nothing can ever finish or report on
    /// again: whatever it was enrolled for is not in the export any more, under any identity. That
    /// is the only way out of an enrolled row that outlived its source, and both media legs reach
    /// it through this one rule rather than each writing their own, which is what keeps them from
    /// drifting apart.
    ///
    /// Two statuses are left alone. [`ItemStatus::Done`], because its output is on disk and was
    /// checksum-verified when it was checked in, so the run genuinely did that work and the source
    /// disappearing afterwards does not un-do it; [`Self::resume`] is what still re-checks those
    /// bytes. And [`ItemStatus::Retired`], because it is already the answer this sweep would write —
    /// see the comment on the filter for what re-writing it would cost.
    ///
    /// **A row this sweep DOES retire keeps whatever output record it had**, which is why the three
    /// output columns are absent from the statement below. The `Done` exemption is not what saves an
    /// earlier run's file from being forgotten — the module's rule for every parked status is what
    /// does that, and a row reaching `Retired` through [`Self::mark_source_missing`] or
    /// [`Self::exclude`] carries its path, digest and length the whole way. What the exemption still
    /// buys on top of it is the re-verify: [`Self::resume`] re-hashes `Done` and nothing else, so a
    /// retired row's recorded bytes are a record of what a run wrote rather than a live claim about
    /// what is on disk now. Pinned by
    /// `a_finished_row_parked_then_retired_keeps_its_output_and_the_record_of_it`.
    ///
    /// **[`ItemStatus::Excluded`] is deliberately NOT a third exemption**, and the choice is pinned
    /// by `a_vanished_excluded_row_is_retired`. Excluding is a decision about output; this sweep is
    /// a fact about the source. An excluded thumbnail whose file left the export is gone, not merely
    /// unwritten, and `Retired` is the only status that records when. Exempting it would leave the
    /// row claiming an enrolled source that is not there, which is the exact state this sweep
    /// exists to close. It costs no churn either, which is what separates it from the `Retired`
    /// case: the row this writes is `Retired`, and that one IS exempt, so the rewrite happens once
    /// and never again.
    ///
    /// **`unreadable` is the guard, and it is why this takes the walk's own list rather than a
    /// caller's verdict about it.** A directory that could not be listed is not evidence a file is
    /// gone, so one entry here stops the whole sweep — the same reasoning that gives
    /// [`crate::export::memories::MissingReason::Unscanned`] precedence over asserting absence.
    /// Scan-wide rather than per-row, because nothing can say whether THIS row's file was in the dir
    /// that could not be read without reading it. A partial scan therefore sweeps nothing and says
    /// nothing new: what says why is the caller's own unreadable list, which every reconciliation
    /// carries and reports.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Sqlite`] when a read or write fails and
    /// [`ManifestError::CorruptRow`] when a stored value no longer parses.
    pub fn retire_absent(&mut self, kind: ItemKind, named: &BTreeSet<&str>, unreadable: &[UnreadableDir]) -> Result<(), ManifestError> {
        /// What a retired row's `last_error` says. A constant this module owns rather than caller
        /// text, so it skips the note redactor every caller-supplied note goes through: there is no
        /// secret in a fixed string, and no per-row url read to pay for on a sweep that can touch
        /// every row of a kind.
        ///
        /// **Rewording this strands every row an earlier sweep already retired**, the same ceiling
        /// [`Manifest::exclude`]'s own note carries and reached one layer up: the `Retired` arm of
        /// the selection filter below keeps such a row out of the statement entirely, so the note is
        /// frozen at the sweep that wrote it and no later run revisits it. Queue task 49 ruled that
        /// cost worth paying here for a sharper reason than at `exclude` — repairing the note means
        /// restamping `updated_at`, and on a retired row that column is the only thing that can
        /// RECONSTRUCT when the row vanished, while this note reconstructs nothing — `status` already
        /// says it. That asymmetry is the whole argument, and it is deliberately not "one of them is
        /// on a screen": no screen renders either column today, `ResumeReport::retired` has no reader
        /// in `src/` at all, and the ruling would be the same if both were rendered. Upgrade path, if
        /// a reword ever has to reach old
        /// rows: give the filter's `Retired` arm a note comparison instead of a blanket exemption,
        /// and accept the restamp on every row it lets through. A reword is loud rather than silent
        /// because `an_already_retired_rows_note_is_frozen_at_the_sweep_that_retired_it` pins these
        /// bytes.
        ///
        /// **A stranded note is only ever replaced by the row LEAVING this status.** Every statement
        /// in this module that writes `last_error` also writes `status`, and the one that would land
        /// back on `Retired` is this sweep, whose filter exempts `Retired`, so nothing refreshes the
        /// note in place. [`Manifest::reset`] is the deliberate route out. [`Manifest::exclude`]'s
        /// `status <> ?1` does admit a `Retired` row at the SQL level, but its only caller feeds it a
        /// plan built from sources that are present, and a retired row's source is gone by
        /// definition — so that path is unproven in either direction rather than known-live, and
        /// nothing should be built on it.
        const RETIRED_NOTE: &str = "the export no longer holds a source for this item";

        if !unreadable.is_empty() {
            return Ok(());
        }
        // Exempting `Retired` is what makes this sweep idempotent, and the cost of leaving it out is
        // not the wasted writes. A retired row is unnamed BY DEFINITION — being unnamed is why it was
        // retired — so it re-enters this list on every later run, and the statement below would reset
        // `updated_at`, which [`Item::updated_at`] documents as the last time the row's own state
        // moved. This sweep's note is a constant, so a row it re-writes moved in no way at all:
        // rewriting the field there turns it into the last RUN, and "when did this vanish from the
        // export" is the half of a retired row only that field can answer.
        // Pinned by `retiring_leaves_an_already_retired_row_untouched`.
        //
        // The exemption's other consequence, ruled on rather than overlooked (queue task 49): it is
        // also what freezes an already-retired row's NOTE, so rewording `RETIRED_NOTE` reaches none
        // of them. The ceiling for that is written on the constant, where whoever rewords it is
        // looking. Note the arms are not interchangeable — `Done` is exempt for its own reason,
        // given in the rustdoc above, and carries no retirement note to strand.
        let stale: Vec<String> = self
            .items(kind)?
            .into_iter()
            .filter(|item| !matches!(item.status, ItemStatus::Done | ItemStatus::Retired) && !named.contains(item.source_id.as_str()))
            .map(|item| item.source_id)
            .collect();
        if stale.is_empty() {
            return Ok(());
        }

        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| sqlite_error("retire items the export no longer names", &path, source))?;
        {
            let mut stmt = tx
                .prepare(
                    "UPDATE items SET status = ?1, last_error = ?2, \
                     updated_at = unixepoch() WHERE kind = ?3 AND source_id = ?4",
                )
                .map_err(|source| sqlite_error("retire items the export no longer names", &path, source))?;
            for source_id in &stale {
                stmt.execute(params![ItemStatus::Retired.as_stored(), RETIRED_NOTE, kind.as_stored(), source_id])
                    .map_err(|source| sqlite_error("retire items the export no longer names", &path, source))?;
            }
        }
        tx.commit().map_err(|source| sqlite_error("retire items the export no longer names", &path, source))
    }

    /// One item, or `None` when nothing enrolled it.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Sqlite`] when the read fails and [`ManifestError::CorruptRow`]
    /// when a stored value no longer parses.
    pub fn item(&self, kind: ItemKind, source_id: &str) -> Result<Option<Item>, ManifestError> {
        let sql = format!("SELECT {ITEM_COLUMNS} FROM items WHERE kind = ?1 AND source_id = ?2");
        let raw = self
            .conn
            .query_row(&sql, params![kind.as_stored(), source_id], RawItem::from_row)
            .optional()
            .map_err(|source| sqlite_error("read an item", &self.path, source))?;
        raw.map(Item::try_from).transpose()
    }

    /// Items of `kind` this run still owes, ordered by source id.
    ///
    /// [`ItemStatus::Pending`] and [`ItemStatus::Failed`] items whose recorded failure count is
    /// below `max_attempts`. [`ItemStatus::Done`] is skipped, which is what a resume buys, and
    /// [`ItemStatus::SourceMissing`], [`ItemStatus::Retired`] and [`ItemStatus::Excluded`] are
    /// skipped because there is nothing to fetch or write under any of them. The statement names
    /// the two statuses it wants rather than excluding the ones it does not, so a status added
    /// later is out of the work list until someone puts it in deliberately.
    ///
    /// The cap is compared against never-attempted items too, whose count is zero, so a
    /// `max_attempts` of 0 offers no work at all rather than offering the untried ones: it reads as
    /// "no attempts allowed", not "no retries allowed".
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Sqlite`] when the read fails and [`ManifestError::CorruptRow`]
    /// when a stored value no longer parses.
    pub fn pending(&self, kind: ItemKind, max_attempts: u32) -> Result<Vec<Item>, ManifestError> {
        let sql =
            format!("SELECT {ITEM_COLUMNS} FROM items WHERE kind = ?1 AND status IN (?2, ?3) AND retry_count < ?4 ORDER BY source_id");
        let mut stmt = self.conn.prepare(&sql).map_err(|source| sqlite_error("list pending items", &self.path, source))?;
        let rows = stmt
            .query_map(
                params![kind.as_stored(), ItemStatus::Pending.as_stored(), ItemStatus::Failed.as_stored(), max_attempts],
                RawItem::from_row,
            )
            .map_err(|source| sqlite_error("list pending items", &self.path, source))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| sqlite_error("list pending items", &self.path, source))?
            .into_iter()
            .map(Item::try_from)
            .collect()
    }

    /// Every item of `kind`, ordered by source id.
    ///
    /// The read a live-progress screen polls each tick: the manifest is the run's only writer and
    /// every status transition is one autocommit statement, so a reader on its own connection sees
    /// whole rows at whatever point the run has reached.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Sqlite`] when the read fails and [`ManifestError::CorruptRow`]
    /// when a stored value no longer parses.
    pub fn items(&self, kind: ItemKind) -> Result<Vec<Item>, ManifestError> {
        let sql = format!("SELECT {ITEM_COLUMNS} FROM items WHERE kind = ?1 ORDER BY source_id");
        let mut stmt = self.conn.prepare(&sql).map_err(|source| sqlite_error("list items", &self.path, source))?;
        let rows = stmt
            .query_map(params![kind.as_stored()], RawItem::from_row)
            .map_err(|source| sqlite_error("list items", &self.path, source))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| sqlite_error("list items", &self.path, source))?
            .into_iter()
            .map(Item::try_from)
            .collect()
    }

    /// Runs the resume contract over one [`ItemKind`] and reports what it found.
    ///
    /// Every finished item is re-hashed in full and demoted if its bytes disagree with what was
    /// recorded; see the module docs for what happens to the other statuses.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Sqlite`] when a read or write fails and
    /// [`ManifestError::CorruptRow`] when a stored value no longer parses.
    pub fn resume(&mut self, kind: ItemKind) -> Result<ResumeReport, ManifestError> {
        let demoted: Vec<Demotion> = self
            .completed(kind)?
            .into_iter()
            .filter_map(|item| item.demotion_reason().map(|reason| Demotion { kind, source_id: item.source_id, reason }))
            .collect();

        if !demoted.is_empty() {
            let path = self.path.clone();
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| sqlite_error("demote unverified items", &path, source))?;
            {
                let mut stmt = tx
                    .prepare(
                        "UPDATE items SET status = ?1, output_path = NULL, checksum = NULL, bytes = NULL, \
                         last_error = ?2, updated_at = unixepoch() WHERE kind = ?3 AND source_id = ?4",
                    )
                    .map_err(|source| sqlite_error("demote unverified items", &path, source))?;
                for item in &demoted {
                    stmt.execute(params![ItemStatus::Pending.as_stored(), item.reason.to_string(), item.kind.as_stored(), item.source_id])
                        .map_err(|source| sqlite_error("demote unverified items", &path, source))?;
                }
            }
            tx.commit().map_err(|source| sqlite_error("demote unverified items", &path, source))?;
        }

        let mut report = ResumeReport { demoted, verified: 0, pending: 0, failed: 0, source_missing: 0, retired: 0, excluded: 0 };
        for (status, count) in self.counts(kind)? {
            match status {
                ItemStatus::Done => report.verified = count,
                ItemStatus::Pending => report.pending = count,
                ItemStatus::Failed => report.failed = count,
                ItemStatus::SourceMissing => report.source_missing = count,
                ItemStatus::Retired => report.retired = count,
                ItemStatus::Excluded => report.excluded = count,
            }
        }
        Ok(report)
    }

    fn configure(&self) -> Result<(), ManifestError> {
        // A crash mid-run must not cost the whole manifest, and WAL is also what gives the
        // sidecars the main file's 0600. `synchronous = NORMAL` under WAL can lose the last few
        // commits to a power cut but never corrupts, and a lost commit costs one re-download that
        // the resume sweep would have caught anyway.
        //
        // The answering row is read and dropped: a filesystem that refuses WAL leaves a rollback
        // journal, which is equally crash-safe and inherits the mode the same way, so it is not
        // worth failing an otherwise usable manifest over.
        let _: String = self
            .conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .map_err(|source| sqlite_error("switch the database to WAL", &self.path, source))?;

        self.conn
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(|source| sqlite_error("set the database's durability", &self.path, source))?;
        Ok(())
    }

    fn migrate(&mut self, export: &ExportId) -> Result<(), ManifestError> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|source| sqlite_error("read the schema version", &self.path, source))?;

        match version {
            0 => self.install(export)?,
            v if v == SCHEMA_VERSION => self.check_export(export)?,
            found => return Err(ManifestError::FutureSchema { path: self.path.clone(), found, supported: SCHEMA_VERSION }),
        }
        Ok(())
    }

    /// Creates the schema, pins which export it belongs to, and stamps the version — all in one
    /// transaction.
    ///
    /// The export pin has to land inside it. `user_version` lives in the database header and moves
    /// with the transaction, so an install that commits the version without the pin is not a
    /// half-written file the next open repairs: it is a complete-looking schema whose
    /// [`Self::check_export`] finds no pin, and every later open refuses it. That needs no crash to
    /// reach — an `INSERT` that fails on a full disk gets there — which is why one transaction
    /// covers all three rather than the DDL alone. The tables are created without `IF NOT EXISTS`
    /// so a break in that promise is loud.
    fn install(&mut self, export: &ExportId) -> Result<(), ManifestError> {
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| sqlite_error("install the schema", &path, source))?;
        tx.execute_batch(INSTALL_SQL).map_err(|source| sqlite_error("install the schema", &path, source))?;
        tx.execute("INSERT INTO meta (key, value) VALUES (?1, ?2)", params![EXPORT_ID_KEY, export.as_str()])
            .map_err(|source| sqlite_error("record which export this manifest is for", &path, source))?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(|source| sqlite_error("stamp the schema version", &path, source))?;
        tx.commit().map_err(|source| sqlite_error("install the schema", &path, source))
    }

    fn check_export(&self, export: &ExportId) -> Result<(), ManifestError> {
        let found: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![EXPORT_ID_KEY], |row| row.get(0))
            .optional()
            .map_err(|source| sqlite_error("read which export this manifest is for", &self.path, source))?;
        match found {
            Some(found) if found == export.as_str() => Ok(()),
            Some(found) => Err(ManifestError::WrongExport { path: self.path.clone(), found, wanted: export.to_string() }),
            // `install` writes the pin in the same transaction as the schema, so this build cannot
            // produce a pinless database. One edited outside exportsnap can, and it needs its own
            // message: reporting it as a rename would name a cause that never happened.
            None => Err(ManifestError::MissingExportPin { path: self.path.clone() }),
        }
    }

    /// Every finished item of `kind`, with what the manifest says its output should be.
    ///
    /// **The `status = 'done'` filter is what scopes the re-verify**, not the columns being
    /// populated. Since parked rows keep their output record, "has a checksum" and "is finished
    /// work" are different questions, and selecting on the columns instead would demote a
    /// [`ItemStatus::SourceMissing`] or [`ItemStatus::Excluded`] row back onto the work list the
    /// first time its output moved. Pinned by
    /// `a_parked_row_carrying_a_checksum_is_never_re_verified_as_finished_work`.
    fn completed(&self, kind: ItemKind) -> Result<Vec<Completed>, ManifestError> {
        let mut stmt = self
            .conn
            .prepare("SELECT source_id, output_path, checksum, bytes FROM items WHERE kind = ?1 AND status = ?2 ORDER BY source_id")
            .map_err(|source| sqlite_error("list finished items", &self.path, source))?;
        let rows = stmt
            .query_map(params![kind.as_stored(), ItemStatus::Done.as_stored()], |row| {
                Ok(Completed {
                    source_id: row.get(0)?,
                    output_path: row.get::<_, Option<String>>(1)?.map(PathBuf::from),
                    checksum: row.get::<_, Option<String>>(2)?,
                    bytes: row.get(3)?,
                })
            })
            .map_err(|source| sqlite_error("list finished items", &self.path, source))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|source| sqlite_error("list finished items", &self.path, source))
    }

    fn counts(&self, kind: ItemKind) -> Result<Vec<(ItemStatus, u64)>, ManifestError> {
        let mut stmt = self
            .conn
            .prepare("SELECT status, count(*) FROM items WHERE kind = ?1 GROUP BY status")
            .map_err(|source| sqlite_error("count items", &self.path, source))?;
        let rows = stmt
            .query_map(params![kind.as_stored()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(|source| sqlite_error("count items", &self.path, source))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| sqlite_error("count items", &self.path, source))?
            .into_iter()
            .map(|(status, count)| Ok((ItemStatus::from_stored(&status)?, count.unsigned_abs())))
            .collect()
    }

    /// `note` with the item's own url taken out of it, on top of the shape pass.
    ///
    /// Reading the url costs one extra statement per recorded failure, which is what buys the
    /// identity pass in [`must_redact`]: a bare signature carries no url punctuation, so only an
    /// exact match against the url this row holds can catch it. An item with no url — every memory
    /// in the one observed export — falls back to the shape pass alone, and an item with a url is
    /// exactly the case where there is a secret to lose.
    fn redacted(&self, kind: ItemKind, source_id: &str, note: &str) -> Result<String, ManifestError> {
        let url: Option<String> = self
            .conn
            .query_row("SELECT url FROM items WHERE kind = ?1 AND source_id = ?2", params![kind.as_stored(), source_id], |row| row.get(0))
            .optional()
            .map_err(|source| sqlite_error("read an item's url to redact a note against it", &self.path, source))?
            .flatten();
        Ok(redact_note(note, url.as_deref()))
    }

    fn require_hit(&self, changed: usize, kind: ItemKind, source_id: &str) -> Result<(), ManifestError> {
        if changed == 0 {
            return Err(ManifestError::UnknownItem { kind, source_id: source_id.to_owned() });
        }
        Ok(())
    }
}

// ---- row plumbing ----

/// A finished item's recorded output, as the resume sweep reads it back.
struct Completed {
    source_id: String,
    output_path: Option<PathBuf>,
    checksum: Option<String>,
    bytes: Option<i64>,
}

impl Completed {
    /// `None` when the bytes on disk are exactly what was recorded.
    fn demotion_reason(&self) -> Option<DemotionReason> {
        let (Some(path), Some(hex), Some(bytes)) = (&self.output_path, &self.checksum, self.bytes) else {
            return Some(DemotionReason::Incomplete);
        };
        let Some(expected) = Checksum::from_hex(hex) else {
            return Some(DemotionReason::Incomplete);
        };
        match Checksum::of_file(path) {
            Ok((actual, actual_bytes)) if actual == expected && i64::try_from(actual_bytes) == Ok(bytes) => None,
            Ok(_) => Some(DemotionReason::Changed),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Some(DemotionReason::Vanished),
            Err(_) => Some(DemotionReason::Unreadable),
        }
    }
}

/// One `items` row as sqlite hands it over, before the closed types are parsed back out.
struct RawItem {
    kind: String,
    source_id: String,
    status: String,
    retry_count: i64,
    url: Option<String>,
    output_path: Option<String>,
    checksum: Option<String>,
    bytes: Option<i64>,
    last_error: Option<String>,
    updated_at: i64,
}

impl RawItem {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            kind: row.get(0)?,
            source_id: row.get(1)?,
            status: row.get(2)?,
            retry_count: row.get(3)?,
            url: row.get(4)?,
            output_path: row.get(5)?,
            checksum: row.get(6)?,
            bytes: row.get(7)?,
            last_error: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }
}

impl TryFrom<RawItem> for Item {
    type Error = ManifestError;

    fn try_from(raw: RawItem) -> Result<Self, Self::Error> {
        let checksum = raw
            .checksum
            .map(|hex| Checksum::from_hex(&hex).ok_or(ManifestError::CorruptRow { column: Column::Checksum, value: hex }))
            .transpose()?;
        let bytes = raw
            .bytes
            .map(|count| u64::try_from(count).map_err(|_| ManifestError::CorruptRow { column: Column::Bytes, value: count.to_string() }))
            .transpose()?;
        Ok(Self {
            kind: ItemKind::from_stored(&raw.kind)?,
            source_id: raw.source_id,
            status: ItemStatus::from_stored(&raw.status)?,
            retry_count: u32::try_from(raw.retry_count)
                .map_err(|_| ManifestError::CorruptRow { column: Column::RetryCount, value: raw.retry_count.to_string() })?,
            output_path: raw.output_path.map(PathBuf::from),
            checksum,
            bytes,
            last_error: raw.last_error,
            url: raw.url.map(DownloadUrl::new),
            updated_at: raw.updated_at,
        })
    }
}

fn sqlite_error(op: &'static str, path: &Path, source: rusqlite::Error) -> ManifestError {
    ManifestError::Sqlite { op, path: path.to_path_buf(), source }
}

/// An output path the manifest can store and a later run can re-resolve.
fn stored_path(output: &Path) -> Result<&str, ManifestError> {
    if !output.is_absolute() {
        return Err(ManifestError::OutputPath { path: output.to_path_buf(), problem: PathProblem::Relative });
    }
    output.to_str().ok_or_else(|| ManifestError::OutputPath { path: output.to_path_buf(), problem: PathProblem::NotUtf8 })
}

/// Whether a whitespace-separated piece of a note has to be replaced before it is stored.
///
/// Two passes, and they close different holes.
///
/// The **shape pass** is an allowlist, not a url detector, and that difference is the point: a
/// detector looking for `://` keeps `cf-st.sc-cdn.net/d/x?sig=…` and a percent-encoded spelling of
/// the same url, because neither carries the shape it was told to look for. A token survives only
/// if it holds none of `URL_PUNCTUATION` and is no longer than `MAX_TOKEN`, so a spelling nobody
/// anticipated is dropped rather than passed. Its cost is deliberate and worth stating exactly: a
/// unix path in a note goes too, since `/` is what makes a url path expressible, and a failed row's
/// `output_path` is NULL, so the note is the only place it could have been. What still carries the
/// path is the error handed back to the caller — [`ManifestError::Output`] names it — so whoever
/// ran the operation sees it; what is lost is the path in the note a later run reads off the row.
///
/// The shape pass alone is still only a guess about shapes, and one shape defeats it — a bare
/// signature lifted out of its url (`sig` is base62, so it holds no url punctuation at all and can
/// sit well under the length cap). The **identity pass** closes that by searching each token for
/// the secret's own bytes: [`secret_fragments`] cuts this item's stored url into its alphanumeric
/// runs, and any token containing one of them is replaced.
///
/// The direction matters and getting it backwards is why this is written down. Asking whether the
/// *url contains the token* fails the moment the token wears adjacent punctuation, since the url
/// holds no comma, quote, paren or full stop — and none of those is url punctuation either, so the
/// shape pass passes them too. Searching the token for the *fragment* cannot be defeated that way,
/// because what is searched for is the secret itself; punctuation around it, or a longer token
/// wrapped around it, changes nothing. Pinned by
/// `a_signature_wearing_ordinary_punctuation_is_still_stripped`.
///
/// A spelling that alters the secret's bytes (percent-encoding) defeats the identity pass by
/// construction and is caught by the shape pass instead; the two cover different halves.
/// `MIN_SECRET_FRAGMENT` bounds over-redaction, not under-redaction: it is the length below which
/// a run shared with the url is a common word rather than a secret.
///
/// **What neither pass catches, named rather than hoped away**: a secret the manifest was never
/// told. A bearer token, a cookie, or another item's url handed in as a note is not matchable
/// against anything this row knows, and no rule at this layer can find it. The guard covers the
/// url column's own secret and url-shaped text; it is not a general secret scrubber, and a caller
/// putting unrelated credentials in a failure note defeats it by construction.
fn must_redact(token: &str, fragments: &[&str]) -> bool {
    /// Characters a url needs to spell a path, a query, a percent-escape, or userinfo. Ordinary
    /// error prose ("connection reset", "timed out", "os error 2", "HTTP 403") holds none.
    const URL_PUNCTUATION: [char; 5] = ['/', '=', '%', '&', '@'];
    /// Nothing in an error message is legitimately this long. Mirrors the redactor's own
    /// `--max-alnum-run` posture: an opaque run past a length is assumed to be payload.
    const MAX_TOKEN: usize = 64;
    token.len() > MAX_TOKEN || token.contains(URL_PUNCTUATION) || fragments.iter().any(|fragment| token.contains(fragment))
}

/// Below this, a run shared with the url is a word both happen to contain ("download") rather than
/// a secret. It bounds how much prose the identity pass eats, not what it catches.
const MIN_SECRET_FRAGMENT: usize = 12;

/// The alphanumeric runs of `url` long enough to be worth hiding.
///
/// Splitting on non-alphanumerics is what makes the match independent of how the caller spelled the
/// secret: the `sig` value comes out as its own run whatever punctuation delimited it in the url,
/// and whatever punctuation surrounds it in the note.
fn secret_fragments(url: &str) -> Vec<&str> {
    url.split(|c: char| !c.is_ascii_alphanumeric()).filter(|run| run.len() >= MIN_SECRET_FRAGMENT).collect()
}

/// `note` with every token [`must_redact`] rejects replaced, whitespace layout kept.
///
/// `url` is the item's own stored url, when it has one; see [`must_redact`] for what that buys.
///
/// Redaction is per whitespace-separated token, so punctuation hugging a url goes with it: the
/// `(` and `):` around a `reqwest` message's url are part of the same token and are replaced along
/// with it. That is deliberate — splitting punctuation off the token would mean deciding which
/// punctuation is part of a url, which is the detector this exists to avoid being.
fn redact_note(note: &str, url: Option<&str>) -> String {
    const REDACTED: &str = "<redacted>";

    let fragments = url.map(secret_fragments).unwrap_or_default();
    let mut out = String::with_capacity(note.len());
    for chunk in note.split_inclusive(char::is_whitespace) {
        let token = chunk.trim_end();
        if token.is_empty() || !must_redact(token, &fragments) {
            out.push_str(token);
        } else {
            out.push_str(REDACTED);
        }
        out.push_str(&chunk[token.len()..]);
    }
    out
}

/// Creates `dir` and its parents, owner-only where the platform has modes.
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::fs::Permissions;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    const OWNER_ONLY: u32 = 0o700;

    fs::DirBuilder::new().recursive(true).mode(OWNER_ONLY).create(dir)?;
    // Tightening an existing dir mirrors what `reserve_private` does for an existing file; the two
    // halves of one control disagreeing is how a gap survives review. What a loose dir leaks is the
    // set of export ids in its filenames rather than any url, since the databases inside stay 0600.
    if fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, Permissions::from_mode(OWNER_ONLY))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// Puts the database file on disk at `0600` before sqlite opens it.
///
/// Creating it here rather than chmod-ing after sqlite does means there is no window in which the
/// file exists world-readable, which for a file about to hold signed download urls is the whole
/// point. Sqlite copies the main database's mode onto the `-wal` and `-shm` sidecars it creates
/// later, so they are covered by this too — pinned by `the_manifest_and_its_sidecars_are_owner_only`
/// rather than trusted, since it is sqlite's behavior and not this crate's.
///
/// An existing file from an older build is tightened instead, which has a window but is strictly
/// better than leaving it loose.
#[cfg(unix)]
fn reserve_private(path: &Path) -> io::Result<()> {
    use std::fs::{OpenOptions, Permissions};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    const OWNER_ONLY: u32 = 0o600;

    match OpenOptions::new().write(true).create_new(true).mode(OWNER_ONLY).open(path) {
        Ok(_) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
                fs::set_permissions(path, Permissions::from_mode(OWNER_ONLY))?;
            }
            Ok(())
        }
        Err(source) => Err(source),
    }
}

/// Windows has no unix mode bits, and this build sets no ACL: the database inherits the ACL of the
/// per-user data dir, which on a default install already excludes other users. An explicit no-op
/// rather than a missing branch — a real ACL story needs `windows-sys` and a review of its own.
#[cfg(not(unix))]
fn reserve_private(_path: &Path) -> io::Result<()> {
    Ok(())
}
