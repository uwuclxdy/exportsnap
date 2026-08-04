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
//! `output_path`, `checksum` and `bytes` are set exactly when the status is [`ItemStatus::Done`].
//! Every transition out of `Done` clears all three, because a checksum kept next to a status that
//! no longer means "these bytes are on disk" is worse than no checksum.
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
//!
//! Nothing here deletes a row, so a re-enumeration ([`Manifest::enroll`]) of the same export is
//! idempotent and never costs finished work.
//!
//! # Concurrency
//!
//! A [`Manifest`] owns one connection and is `Send` but not `Sync`. A concurrent downloader shares
//! one behind its own lock; two processes on one manifest is not a supported arrangement, and the
//! resume sweep is what makes an interrupted run's leftovers safe rather than any claim protocol.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::export::model::DownloadUrl;

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
}

impl ItemStatus {
    pub const ALL: [Self; 4] = [Self::Pending, Self::Done, Self::Failed, Self::SourceMissing];

    /// The word stored in the `status` column.
    #[must_use]
    pub const fn as_stored(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::SourceMissing => "source_missing",
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
    /// Set exactly when `status` is [`ItemStatus::Done`], as are `checksum` and `bytes`.
    pub output_path: Option<PathBuf>,
    pub checksum: Option<Checksum>,
    pub bytes: Option<u64>,
    /// Why the last attempt failed, or why there is no source, reduced to its prose tokens on the
    /// way in: a token survives only if it holds none of `/ = % & @` and is under 64 characters.
    pub last_error: Option<String>,
    pub url: Option<DownloadUrl>,
    /// Unix seconds of the last status transition.
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
            Self::CorruptRow { column, value } => write!(
                f,
                "the manifest's {column} column holds {value:?}, which this build cannot read; \
                 the file was edited outside exportsnap, so delete it to redo this export's downloads from scratch"
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
    /// # Errors
    ///
    /// Returns [`ManifestError::UnknownItem`] when nothing enrolled that item, and
    /// [`ManifestError::Sqlite`] when the write fails.
    pub fn mark_source_missing(&self, kind: ItemKind, source_id: &str, reason: &str) -> Result<(), ManifestError> {
        let reason = self.redacted(kind, source_id, reason)?;
        let changed = self
            .conn
            .execute(
                "UPDATE items SET status = ?1, output_path = NULL, checksum = NULL, bytes = NULL, last_error = ?2, \
                 updated_at = unixepoch() WHERE kind = ?3 AND source_id = ?4",
                params![ItemStatus::SourceMissing.as_stored(), reason, kind.as_stored(), source_id],
            )
            .map_err(|source| sqlite_error("record a missing source", &self.path, source))?;
        self.require_hit(changed, kind, source_id)
    }

    /// Puts an item back on the work list as if no run had ever touched it, retry count included.
    ///
    /// This is the way out of [`ItemStatus::SourceMissing`]: a caller that finds the media a
    /// previous run could not — in an export part that was not extracted yet — calls this.
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
    /// [`ItemStatus::SourceMissing`] is skipped because there is nothing to fetch.
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

        let mut report = ResumeReport { demoted, verified: 0, pending: 0, failed: 0, source_missing: 0 };
        for (status, count) in self.counts(kind)? {
            match status {
                ItemStatus::Done => report.verified = count,
                ItemStatus::Pending => report.pending = count,
                ItemStatus::Failed => report.failed = count,
                ItemStatus::SourceMissing => report.source_missing = count,
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
