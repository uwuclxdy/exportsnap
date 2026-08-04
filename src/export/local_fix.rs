//! The local-fix pass: turning the memories an export already holds media for into dated,
//! located, composited files under an output root.
//!
//! This is the path this box's export actually needs. Every one of its 836 download links is
//! empty, so nothing can be fetched and the media on disk is all there is. What that media is
//! missing is everything the entry knows: the capture date (a downloaded file carries the day it
//! was downloaded), the coordinates, and the caption layer that ships as a separate file.
//!
//! # What may be stamped, and from where
//!
//! [`crate::export::memories`] pairs entries to media by date bucket and records whether the
//! pairing was the only one possible ([`Pairing::Exact`]) or one of several
//! ([`Pairing::Ambiguous`]). Decision 32 turns that distinction into two rules that point opposite
//! ways:
//!
//! - **GPS** is stamped from the entry whenever **every entry in the bucket names the same place**,
//!   checked per bucket at run time rather than assumed. An arbitrary assignment inside a bucket
//!   where everyone agrees cannot pick the wrong coordinate. Measured on the real export: of 163
//!   ambiguous n:n buckets covering 463 entries, 151 have every entry at one identical location.
//! - **Time** is stamped from the entry **only in an exact bucket**. 85 of those 163 buckets span
//!   an hour or more, so an arbitrary assignment gets the time badly wrong. Everywhere else the
//!   time falls back to the file's own embedded timestamp, then to the day in its filename.
//!
//! # Non-destructive by construction
//!
//! Decision 33: output lands under [`default_out_root`], and the source is only ever read. A bad
//! run is deleted rather than recovered, and the manifest's checksum resume can tell a finished
//! file from a corrupted one precisely because the file it hashes is one this run created.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};

use crate::export::exif::{ExifError, Jpeg, Stamp};
use crate::export::manifest::{ItemKind, Manifest, ManifestError, ResumeReport};
use crate::export::memories::{Bucket, Day, MemoryMedia, Pairing, Reconciliation};
use crate::export::model::{LocationPoint, Memories, Memory, Timestamp};
use crate::export::overlay::{self, OverlayError};
use crate::export::timezone;

/// The directory a run writes into, under the source the user pointed at.
///
/// Beside the export rather than over it: the export is the user's only copy and this pass reads
/// it.
const OUT_DIR: &str = "exportsnap-out";

/// Every image this build writes is a JPEG, whatever the source was, because that is what keeps a
/// PNG out of `little_exif` (see [`crate::export::exif`]).
const OUTPUT_EXTENSION: &str = "jpg";

/// Extensions the image leg reads. A main outside this set is deferred rather than attempted.
const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

/// Extensions whose bytes can be copied straight through instead of re-encoded.
const VERBATIM_EXTENSIONS: [&str; 2] = ["jpg", "jpeg"];

/// Where a run writes when the caller names no output root.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use exportsnap::export::local_fix::default_out_root;
///
/// assert_eq!(default_out_root(Path::new("/data/export")), Path::new("/data/export/exportsnap-out"));
/// ```
#[must_use]
pub fn default_out_root(source: impl AsRef<Path>) -> PathBuf {
    source.as_ref().join(OUT_DIR)
}

// ---- when a memory was taken ----

/// What told the run when a memory was taken.
///
/// Worth carrying rather than collapsing into the datetime: [`Self::Filename`] is a day with a
/// midnight bolted on, not a capture time, and a screen that presents it as one claims precision
/// the run does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimeSource {
    /// The entry's own `Date`, which only an exact bucket may hand over.
    Entry,
    /// The `-main` file's own `DateTimeOriginal`, `CreateDate` or `ModifyDate`.
    Embedded,
    /// The day the filename leads with, at midnight. The last resort.
    Filename,
}

impl fmt::Display for TimeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Entry => "the memory's own entry",
            Self::Embedded => "the file's embedded timestamp",
            Self::Filename => "the day in the filename",
        })
    }
}

/// When a memory was taken, as well as the run can tell, and what told it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capture {
    local: NaiveDateTime,
    offset: Option<FixedOffset>,
    source: TimeSource,
}

impl Capture {
    /// Local wall-clock time: what goes in `DateTimeOriginal`, in the output filename, and in the
    /// year and month directories.
    #[must_use]
    pub const fn local(self) -> NaiveDateTime {
        self.local
    }

    /// The offset [`Self::local`] is at, when the run could work it out from GPS or read it off
    /// the file.
    #[must_use]
    pub const fn offset(self) -> Option<FixedOffset> {
        self.offset
    }

    #[must_use]
    pub const fn source(self) -> TimeSource {
        self.source
    }

    /// The instant, for the file's modification time.
    ///
    /// With no offset the local time is read as UTC. That is the honest fallback rather than a
    /// guess: an unstated wall time has no other anchor, and inventing one from the machine's own
    /// zone would make the same input produce different mtimes on different boxes.
    #[must_use]
    pub fn instant(self) -> DateTime<Utc> {
        match self.offset {
            Some(offset) => offset.from_local_datetime(&self.local).earliest().map_or_else(|| self.local.and_utc(), |at| at.to_utc()),
            None => self.local.and_utc(),
        }
    }

    /// The entry's UTC instant, moved into local time when a coordinate places it.
    ///
    /// The offset is always stated on this path, `+00:00` included, because the instant itself is
    /// exactly known either way: with GPS the wall time is real local time, and without it the
    /// wall time is UTC, so saying so keeps the instant recoverable from the file.
    fn from_entry(utc: NaiveDateTime, location: Option<LocationPoint>) -> Self {
        let offset = location.and_then(|location| timezone::offset(location, utc)).unwrap_or_else(|| Utc.fix());
        Self { local: utc.and_utc().with_timezone(&offset).naive_local(), offset: Some(offset), source: TimeSource::Entry }
    }
}

/// The calendar instant of a [`Timestamp`], or `None` when it names no real one.
///
/// [`Timestamp`] is range-checked rather than calendar-checked, so `2021-02-30` parses. This is
/// the first caller handing one to a date crate, which the design says has to convert fallibly.
fn calendar(timestamp: Timestamp) -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(i32::from(timestamp.year()), u32::from(timestamp.month()), u32::from(timestamp.day()))?.and_hms_opt(
        u32::from(timestamp.hour()),
        u32::from(timestamp.minute()),
        u32::from(timestamp.second()),
    )
}

/// The filename's day at midnight. [`Day`] is range-checked the same way, so this is fallible for
/// the same reason.
fn midnight(day: Day) -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(i32::from(day.year()), u32::from(day.month()), u32::from(day.day()))?.and_hms_opt(0, 0, 0)
}

// ---- decision 32: whether a bucket may hand over its location ----

/// Whether every entry in one bucket names the same place.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Agreement {
    /// Every entry seen so far names exactly this location.
    On(LocationPoint),
    /// Two entries name different places, or one of them names none at all. An entry with no
    /// location splits the bucket rather than abstaining: if the file in hand might really be that
    /// entry's, stamping somebody else's coordinate onto it is the error this rule exists to
    /// prevent.
    Split,
}

/// What each bucket agrees on, over the whole entry list.
///
/// Taken across every entry rather than only the paired ones, because what decision 32 asks about
/// is the bucket the arbitrary assignment was drawn from, and an unpaired entry is one of the
/// candidates that assignment could have picked differently.
fn agreements(memories: &Memories) -> BTreeMap<Bucket, Agreement> {
    let mut agreements: BTreeMap<Bucket, Agreement> = BTreeMap::new();
    for memory in &memories.saved_media {
        let Some(bucket) = Bucket::of(memory) else { continue };
        let next = match (agreements.get(&bucket), memory.location) {
            (None, Some(location)) => Agreement::On(location),
            (Some(Agreement::On(seen)), Some(location)) if *seen == location => Agreement::On(location),
            (None | Some(_), _) => Agreement::Split,
        };
        agreements.insert(bucket, next);
    }
    agreements
}

// ---- the plan ----

/// One memory this run is going to fix, and everything it needs to do it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedItem {
    /// The manifest's identity for this memory: the media's uuid.
    pub source_id: String,
    /// Where the entry sits in `memories_history.json`.
    pub entry_index: usize,
    pub media: MemoryMedia,
    pub capture: Capture,
    /// The coordinate this item is allowed to carry, after decision 32. `None` means it gets none,
    /// whether because the entry had none or because its bucket disagreed.
    pub location: Option<LocationPoint>,
    /// Where the fixed copy lands.
    pub output: PathBuf,
}

/// Why a paired memory is not in [`Plan::items`].
///
/// None of these is a failure and none is written to the manifest: the rows stay
/// [`crate::export::manifest::ItemStatus::Pending`], so whichever leg can handle them picks them
/// up untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeferralReason {
    /// The main file is a video. Task 17's leg.
    Video,
    /// The main file's extension names no format this build decodes.
    UnknownFormat,
    /// Nothing in the export names a real calendar date for this memory, so there is no year and
    /// month to file it under. Unobserved, and reachable because [`Day`] is range-checked rather
    /// than calendar-checked: `2021-02-30` is a filename this build parses and no date.
    NoCalendarDate,
}

impl fmt::Display for DeferralReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Video => "the memory is a video, which the image pass does not touch",
            Self::UnknownFormat => "the memory's file is in a format this build does not decode",
            Self::NoCalendarDate => "no real calendar date could be worked out for this memory, so it has no year and month to file under",
        })
    }
}

/// A paired memory this pass left alone, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferred {
    pub source_id: String,
    pub reason: DeferralReason,
}

/// Every output path a run will write, worked out before it writes any of them.
///
/// Planned whole rather than item by item because output names collide and the collision has to be
/// broken the same way on every run. Two memories on one day with no usable time both want
/// `20210115_000000.jpg`; picking the next free name off disk would hand a resumed run a different
/// answer than the first one, depending on which of the two had finished. The suffix is a position
/// in this list instead, so it does not move.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// One per paired memory whose media this build can fix, in `memories_history.json` order.
    pub items: Vec<PlannedItem>,
    /// Paired memories this pass will not touch, in the same order.
    pub deferred: Vec<Deferred>,
}

impl Plan {
    /// Works out what a run would write, without writing anything.
    ///
    /// Reads the source files' own EXIF for the items whose time falls back to it, which is the
    /// one piece of I/O here. That read is best-effort: a file that cannot be read at plan time
    /// drops to the filename date and the real failure is reported when the fix step reaches it,
    /// rather than one bad file taking down the whole plan.
    #[must_use]
    pub fn build(memories: &Memories, reconciliation: &Reconciliation, out_root: impl AsRef<Path>) -> Self {
        let out_root = out_root.as_ref();
        let agreements = agreements(memories);
        let mut items = Vec::new();
        let mut deferred = Vec::new();
        let mut taken: BTreeMap<String, u32> = BTreeMap::new();

        for item in &reconciliation.items {
            let (Some(media), Some(memory)) = (item.media(), memories.saved_media.get(item.entry_index)) else {
                continue;
            };
            let mut defer = |reason| deferred.push(Deferred { source_id: item.source_id.clone(), reason });

            if !matches(&media.main.extension, &IMAGE_EXTENSIONS) {
                defer(if media.main.extension.eq_ignore_ascii_case("mp4") { DeferralReason::Video } else { DeferralReason::UnknownFormat });
                continue;
            }

            let location = permitted_location(&item.pairing, memory, &agreements);
            let Some(capture) = capture_of(&item.pairing, memory, media, location) else {
                defer(DeferralReason::NoCalendarDate);
                continue;
            };

            let stem = capture.local.format("%Y%m%d_%H%M%S").to_string();
            let ordinal = taken.entry(stem.clone()).or_default();
            let output = output_path(out_root, capture.local, &stem, *ordinal);
            *ordinal += 1;

            items.push(PlannedItem {
                source_id: item.source_id.clone(),
                entry_index: item.entry_index,
                media: media.clone(),
                capture,
                location,
                output,
            });
        }

        Self { items, deferred }
    }
}

/// The coordinate decision 32 lets this item carry.
fn permitted_location(pairing: &Pairing, memory: &Memory, agreements: &BTreeMap<Bucket, Agreement>) -> Option<LocationPoint> {
    match pairing {
        // One entry and one media set: the coordinate belongs to this file and nothing was chosen.
        Pairing::Exact(_) => memory.location,
        // Which entry got which file was arbitrary, so the coordinate is only safe where every
        // candidate named the same one.
        Pairing::Ambiguous(_) => match agreements.get(&Bucket::of(memory)?) {
            Some(Agreement::On(location)) => Some(*location),
            Some(Agreement::Split) | None => None,
        },
        Pairing::Missing(_) => None,
    }
}

/// When this item was taken, working down decision 32's fallback chain. `None` when no step of it
/// yields a real calendar date.
fn capture_of(pairing: &Pairing, memory: &Memory, media: &MemoryMedia, location: Option<LocationPoint>) -> Option<Capture> {
    let from_entry = match pairing {
        Pairing::Exact(_) => memory.date.and_then(calendar),
        // An ambiguous bucket can span hours, so its entry's time says nothing about this file.
        Pairing::Ambiguous(_) | Pairing::Missing(_) => None,
    };
    if let Some(utc) = from_entry {
        return Some(Capture::from_entry(utc, location));
    }

    // The file's own idea of when it was taken. Snapchat's downloads usually carry the download
    // date instead, which is why this is second and not first.
    if let Ok(jpeg) = Jpeg::read(&media.main.path)
        && let Some(local) = jpeg.embedded_time()
    {
        return Some(Capture { local, offset: jpeg.embedded_offset(), source: TimeSource::Embedded });
    }

    // The day in the filename, which is the only date left. Midnight is a placeholder rather than
    // a claim, which is what [`TimeSource::Filename`] exists to say.
    Some(Capture { local: midnight(media.main.day)?, offset: None, source: TimeSource::Filename })
}

/// `<root>/YYYY/MM/YYYYMMDD_HHMMSS.jpg`, with `_2`, `_3` and so on for a name already claimed.
fn output_path(root: &Path, local: NaiveDateTime, stem: &str, ordinal: u32) -> PathBuf {
    let name = if ordinal == 0 { format!("{stem}.{OUTPUT_EXTENSION}") } else { format!("{stem}_{}.{OUTPUT_EXTENSION}", ordinal + 1) };
    root.join(local.format("%Y").to_string()).join(local.format("%m").to_string()).join(name)
}

/// Ascii-case-insensitive membership, matching how the rest of the export layer reads an
/// extension.
fn matches(extension: &str, known: &[&str]) -> bool {
    known.iter().any(|candidate| extension.eq_ignore_ascii_case(candidate))
}

// ---- running it ----

/// What one local-fix pass did.
#[derive(Debug, Clone, PartialEq)]
pub struct FixReport {
    /// What the resume sweep found before any work started.
    pub resumed: ResumeReport,
    /// Items written and checked into the manifest by this run.
    pub fixed: usize,
    /// Items that failed and were recorded as such, each retryable next run.
    pub failed: Vec<Failure>,
    /// Planned items the manifest did not offer as work: already finished, or parked past the
    /// retry cap. This is what a resume saves.
    pub skipped: usize,
    /// Paired memories left to another leg. See [`Plan::deferred`].
    pub deferred: usize,
}

/// One item a run could not fix, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub source_id: String,
    /// The message that also went into the manifest's `last_error`, where it is redacted on the
    /// way in.
    pub reason: String,
}

/// Fixes everything in `plan` the manifest still owes, checking each result in as it lands.
///
/// Runs [`Manifest::resume`] first, so a previous run's finished output is re-hashed and anything
/// that no longer matches goes back on the work list. An item the manifest does not offer is
/// skipped without being read, which is what makes a resume cheap.
///
/// Per-item failures are recorded against the item and the run carries on: one unreadable file out
/// of 746 must not cost the other 745, and the manifest is what makes each one addressable
/// afterwards. A manifest failure is different in kind — the state store itself is broken — and
/// stops the run.
///
/// # Errors
///
/// Returns [`ManifestError`] when the manifest cannot be read or written.
pub fn run(plan: &Plan, manifest: &mut Manifest, max_attempts: u32) -> Result<FixReport, ManifestError> {
    let resumed = manifest.resume(ItemKind::Memory)?;
    let owed: BTreeSet<String> = manifest.pending(ItemKind::Memory, max_attempts)?.into_iter().map(|item| item.source_id).collect();

    let mut report = FixReport { resumed, fixed: 0, failed: Vec::new(), skipped: 0, deferred: plan.deferred.len() };
    for item in &plan.items {
        if !owed.contains(&item.source_id) {
            report.skipped += 1;
            continue;
        }
        match fix(item) {
            Ok(()) => {
                manifest.mark_done(ItemKind::Memory, &item.source_id, &item.output)?;
                report.fixed += 1;
            }
            Err(error) => {
                let reason = error.to_string();
                manifest.mark_failed(ItemKind::Memory, &item.source_id, &reason)?;
                report.failed.push(Failure { source_id: item.source_id.clone(), reason });
            }
        }
    }
    Ok(report)
}

/// Composites, stamps, writes and dates one memory.
///
/// Nothing is left half-written: the output is one `fs::write` of a finished buffer, so a failure
/// before it leaves no file at all and a failure after it leaves a complete file whose date the
/// next run corrects.
///
/// # Errors
///
/// Returns [`FixError`] when any step fails.
pub fn fix(item: &PlannedItem) -> Result<(), FixError> {
    // A main that is already a JPEG and carries no overlay is copied byte for byte. Re-encoding it
    // would spend a generation of lossy compression for nothing, and the copy is also what keeps
    // whatever EXIF the source carried around for `stamp` to read and preserve.
    let bytes = if item.media.overlay.is_none() && matches(&item.media.main.extension, &VERBATIM_EXTENSIONS) {
        fs::read(&item.media.main.path).map_err(|source| FixError::Read { path: item.media.main.path.clone(), source })?
    } else {
        overlay::compose(&item.media.main.path, item.media.overlay.as_ref().map(|file| file.path.as_path()))?
    };

    // The gate, and it runs before anything else looks at the bytes. Ordering matters: reading the
    // dimensions first means a corrupt file is reported by the image decoder, whose message is
    // about a byte count rather than about the file being unusable, and the guard never gets to say
    // what it refused. A `.jpg` that is really something else fails here, not further in.
    let mut jpeg = Jpeg::new(bytes).map_err(|source| ExifError::NotJpeg { path: item.media.main.path.clone(), source })?;
    let (width, height) = overlay::dimensions(jpeg.as_bytes())?;
    jpeg.stamp(&Stamp { local: item.capture.local(), offset: item.capture.offset(), location: item.location, width, height })?;

    if let Some(parent) = item.output.parent() {
        fs::create_dir_all(parent).map_err(|source| FixError::Create { path: parent.to_path_buf(), source })?;
    }
    jpeg.write(&item.output)?;
    set_modified(&item.output, item.capture.instant()).map_err(|source| FixError::Touch { path: item.output.clone(), source })
}

/// Sets a file's modification time, which is what a photo browser sorts and groups by.
fn set_modified(path: &Path, instant: DateTime<Utc>) -> io::Result<()> {
    let times = fs::FileTimes::new().set_modified(system_time(instant));
    fs::OpenOptions::new().write(true).open(path)?.set_times(times)
}

/// A UTC instant as the standard library spells one, on both sides of the epoch.
fn system_time(instant: DateTime<Utc>) -> SystemTime {
    let seconds = instant.timestamp();
    match u64::try_from(seconds) {
        Ok(seconds) => UNIX_EPOCH + Duration::from_secs(seconds),
        Err(_) => UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs()),
    }
}

/// Something went wrong fixing one memory. Every variant is per-item and retryable; none of them
/// means the run should stop.
#[derive(Debug)]
pub enum FixError {
    Read { path: PathBuf, source: io::Error },
    Compose { source: OverlayError },
    Metadata { source: ExifError },
    Create { path: PathBuf, source: io::Error },
    Touch { path: PathBuf, source: io::Error },
}

impl From<OverlayError> for FixError {
    fn from(source: OverlayError) -> Self {
        Self::Compose { source }
    }
}

impl From<ExifError> for FixError {
    fn from(source: ExifError) -> Self {
        Self::Metadata { source }
    }
}

impl fmt::Display for FixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "could not read {}: {source}", path.display()),
            Self::Compose { source } => write!(f, "{source}"),
            Self::Metadata { source } => write!(f, "{source}"),
            Self::Create { path, source } => {
                write!(f, "could not create {}: {source}; check the output directory is writable", path.display())
            }
            Self::Touch { path, source } => write!(f, "wrote {} but could not set its date: {source}", path.display()),
        }
    }
}

impl Error for FixError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Create { source, .. } | Self::Touch { source, .. } => Some(source),
            Self::Compose { source } => Some(source),
            Self::Metadata { source } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::NaiveDate;

    use super::{Capture, TimeSource, output_path};

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day).unwrap().and_hms_opt(hour, minute, second).unwrap()
    }

    #[test]
    fn an_output_path_is_year_month_and_the_local_wall_time() {
        let local = at(2021, 1, 15, 14, 30, 5);
        assert_eq!(output_path(Path::new("/out"), local, "20210115_143005", 0), Path::new("/out/2021/01/20210115_143005.jpg"));
    }

    #[test]
    fn a_name_a_previous_item_already_claimed_gets_a_counted_suffix() {
        let local = at(2021, 1, 15, 0, 0, 0);
        // The suffix counts from two, so the first file of a colliding set keeps the plain name and
        // nobody has to work out that `_1` means "the second one".
        assert_eq!(output_path(Path::new("/out"), local, "20210115_000000", 1), Path::new("/out/2021/01/20210115_000000_2.jpg"));
        assert_eq!(output_path(Path::new("/out"), local, "20210115_000000", 2), Path::new("/out/2021/01/20210115_000000_3.jpg"));
    }

    #[test]
    fn a_capture_with_no_offset_reads_its_wall_time_as_utc() {
        let capture = Capture { local: at(2021, 6, 1, 12, 0, 0), offset: None, source: TimeSource::Filename };
        assert_eq!(capture.instant().to_rfc3339(), "2021-06-01T12:00:00+00:00");
    }

    #[test]
    fn a_capture_with_an_offset_resolves_back_to_the_instant_it_came_from() {
        let utc = at(2021, 6, 1, 10, 0, 0);
        let capture = Capture::from_entry(utc, None);
        assert_eq!(capture.source(), TimeSource::Entry);
        // No coordinate: the wall time stays UTC and the offset says so, so the instant survives.
        assert_eq!(capture.local(), utc);
        assert_eq!(capture.offset().map(|offset| offset.local_minus_utc()), Some(0));
        assert_eq!(capture.instant().naive_utc(), utc);
    }
}
