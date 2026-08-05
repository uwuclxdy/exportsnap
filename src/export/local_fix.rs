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
//! # Images and videos take different routes to the same place
//!
//! An image is composited and stamped entirely in pure Rust and lands as a JPEG. A video's
//! **metadata is written in pure Rust too**, always, whatever else happened to it — one metadata
//! code path, one set of properties. What differs is the pixels:
//!
//! - **Transcoding on** (the default, see [`VideoOptions`]) **and ffmpeg installed**: ffmpeg
//!   re-encodes the HEVC every memory video carries into H.264, burning the caption layer in on
//!   the way past, since a re-encode is already touching every frame.
//! - **Transcoding off, or no ffmpeg**: the video's bytes are copied and only the metadata is
//!   patched. Nothing re-encodes video pixels, and the run reports both what it did not transcode
//!   and the overlay it therefore could not draw ([`Notice`]).
//!
//! # Non-destructive by construction
//!
//! Decision 33: output lands under [`default_out_root`], and the source is only ever read. A bad
//! run is deleted rather than recovered, and the manifest's checksum resume can tell a finished
//! file from a corrupted one precisely because the file it hashes is one this run created.
//!
//! # The item-level pass is shared, the planning is not
//!
//! [`Plan::build`] is the memories planner and it is the only memories-specific thing left here.
//! [`PlannedItem`], [`fix`] and [`run`] are leg-agnostic: they take a [`SourceMedia`], a
//! [`Capture`], a coordinate that may be `None` and an output path, and they neither know nor ask
//! which enumeration produced them. [`super::chat_fix`] is the second planner, and it fills the
//! same [`Plan`] rather than growing a second copy of the composite-stamp-write-date sequence —
//! two copies of that sequence would be two places a metadata rule has to be kept true.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};

use crate::export::env::{self, Tool};
use crate::export::exif::{ExifError, Jpeg, Stamp};
use crate::export::ffmpeg::{self, FfmpegError};
use crate::export::manifest::{ItemKind, Manifest, ManifestError, ResumeReport};
use crate::export::memories::{Bucket, Day, Pairing, Reconciliation};
use crate::export::model::{Attribution, LocationPoint, Memories, Memory, Timestamp};
use crate::export::overlay::{self, OverlayError};
use crate::export::timezone;
use crate::export::video::{LocationAtom, Mp4, NotMp4, VideoError, VideoStamp};

/// The directory a run writes into, under the source the user pointed at.
///
/// Beside the export rather than over it: the export is the user's only copy and this pass reads
/// it.
const OUT_DIR: &str = "exportsnap-out";

/// Extensions the image leg reads. A main outside this set is deferred rather than attempted.
///
/// **The ceiling, and its upgrade path.** The image leg admits exactly these three today, and adding
/// a fourth is not one edit — it needs two separate answers, because they have different
/// user-visible outcomes:
///
/// 1. **Does it copy through?** That is [`PASS_THROUGH_EXTENSIONS`], and it decides whether the
///    output keeps the source's own format and bytes or becomes a JPEG.
/// 2. **Can this build stamp it?** That is whether [`crate::export::exif`] can write metadata into
///    it at all, which today means "is it a JPEG" and nothing else.
///
/// **Today's three answer the pair as a package only by coincidence**: `jpg`/`jpeg` are stamped and
/// not copied through, `png` is copied through and not stamped. A fourth could want both — a format
/// this build learns to write metadata into — or neither, in which case it belongs nowhere near this
/// list and stays deferred at [`Leg::of`]. Reading the current pairing as a rule is the mistake this
/// paragraph exists to stop.
///
/// A format added here but NOT to [`PASS_THROUGH_EXTENSIONS`] reaches [`crate::export::exif::Jpeg`]
/// and is refused by name, one item at a time. That failure is chosen: the alternative is silently
/// re-encoding a format nobody validated into a JPEG and keeping no original, which is the defect
/// class decision 47 exists to close, one format over.
const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

/// Extensions the video leg reads.
const VIDEO_EXTENSIONS: [&str; 1] = ["mp4"];

/// The image formats this build copies through under their OWN extension instead of as a JPEG.
///
/// The question is whether the OUTPUT is still a JPEG, not whether the bytes are re-encoded — a
/// `.jpg` main with no overlay is copied through too, and is still stamped. A `.png` cannot be,
/// because this build writes EXIF only into a JPEG.
///
/// **Read as a membership set and nowhere as a position.** It used to be indexed for the output
/// extension, which made its LENGTH load-bearing at a call site that never mentioned the length: the
/// two readings agreed only while it held exactly one member, and a second one added at the front
/// would have had [`output_extension`] answer with the wrong format's name while `passes_through`
/// admitted the right one. [`output_extension`] now takes the item's own extension instead, so this
/// may grow at either end and nothing downstream moves.
///
/// Growing it is the FIRST of the two questions [`IMAGE_EXTENSIONS`] sets out; a format has to be in
/// that list to reach this one at all.
const PASS_THROUGH_EXTENSIONS: [&str; 1] = ["png"];

/// What a transcode writes before the finished file replaces it.
///
/// ffmpeg needs a seekable file to mux an MP4 into, so it is the one step of the pass that cannot
/// be a single `fs::write` of a finished buffer. Pointing it at a scratch name beside the output
/// and writing the real output afterwards is what keeps a failed item from leaving a half-made
/// video where the manifest will later hash one. Leading dot so a file browser does not show a
/// crashed run's leftovers as memories.
const SCRATCH_PREFIX: &str = ".exportsnap-transcoding-";

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
    /// The `Created` of the chat message that named the file: the chat-media leg's first step, and
    /// the one thing in either chain with no twin on the other side. Kept apart from [`Self::Entry`]
    /// rather than reworded to cover both, because the two are different records in different files
    /// and a user reading "the memory's own entry" against a chat photo learns something false.
    Message,
    /// The `-main` file's own metadata: an image's `DateTimeOriginal`, `CreateDate` or
    /// `ModifyDate`, a video's `mvhd` creation time.
    Embedded,
    /// The day the filename leads with, at midnight. The last resort.
    Filename,
}

impl fmt::Display for TimeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Entry => "the memory's own entry",
            Self::Message => "the message that sent it",
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

    /// A known UTC instant, moved into local time when a coordinate places it.
    ///
    /// The offset is always stated on this path, `+00:00` included, because the instant itself is
    /// exactly known either way: with GPS the wall time is real local time, and without it the
    /// wall time is UTC, so saying so keeps the instant recoverable from the file.
    pub(crate) fn from_utc(utc: NaiveDateTime, location: Option<LocationPoint>, source: TimeSource) -> Self {
        let offset = location.and_then(|location| timezone::offset(location, utc)).unwrap_or_else(|| Utc.fix());
        Self { local: utc.and_utc().with_timezone(&offset).naive_local(), offset: Some(offset), source }
    }

    /// The entry's own `Date`, which an exact bucket hands over.
    fn from_entry(utc: NaiveDateTime, location: Option<LocationPoint>) -> Self {
        Self::from_utc(utc, location, TimeSource::Entry)
    }

    /// The `Created` of the message that named a chat-media file.
    ///
    /// No location argument, and that is the chat leg's whole GPS story rather than an omission:
    /// `chat_history.json` carries no coordinate field anywhere, so there is nothing to place the
    /// instant with and the wall time stays UTC with the offset saying so.
    pub(crate) fn from_message(utc: NaiveDateTime) -> Self {
        Self::from_utc(utc, None, TimeSource::Message)
    }

    /// The day in the filename at midnight: the last step of both legs' chains, and the only one
    /// that invents a time of day. `None` when the day names no real calendar date.
    pub(crate) fn from_day(day: Day) -> Option<Self> {
        Some(Self { local: midnight(day)?, offset: None, source: TimeSource::Filename })
    }
}

// ---- which leg fixes an item ----

/// Which half of the pass fixes an item, and so what it comes out as.
///
/// Carried on the plan rather than re-derived at fix time because the output path depends on it and
/// the whole run's paths are decided before a byte is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Leg {
    /// Composited and stamped in pure Rust.
    Image,
    /// Optionally re-encoded by ffmpeg, then stamped in pure Rust either way.
    Video,
}

impl Leg {
    /// The extension every one of this leg's outputs carries.
    ///
    /// **The default, not the whole answer for the image leg** — [`output_extension`] is, and it is
    /// what every output path and every collision key goes through. An image that is composited or
    /// re-encoded comes out as JPEG whatever went in, which is also what keeps a PNG out of
    /// `little_exif` (see [`crate::export::exif`]); a PNG with nothing to composite is copied through
    /// under its own extension instead, per decision 47. Videos come out as MP4 whether they were
    /// re-encoded or copied, since both routes end in an MP4 container.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Image => "jpg",
            Self::Video => "mp4",
        }
    }

    /// Which leg reads a main with this extension, or `None` for a format this build does not read.
    pub(crate) fn of(extension: &str) -> Option<Self> {
        if matches(extension, &IMAGE_EXTENSIONS) {
            Some(Self::Image)
        } else if matches(extension, &VIDEO_EXTENSIONS) {
            Some(Self::Video)
        } else {
            None
        }
    }
}

impl fmt::Display for Leg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Image => "image",
            Self::Video => "video",
        })
    }
}

// ---- what a run is allowed to do to a video ----

/// What the run may do to a video's pixels.
///
/// Not a CLI flag: nothing invokes this pass from `main.rs` yet, and a parsed flag with no consumer
/// is dead code. This is the seam the memories screen wires a toggle into when it lands, which is
/// the same call already recorded for `--out=<dir>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoOptions {
    /// Whether to re-encode HEVC into H.264. **On in [`Self::probe`]**, because every memory video
    /// in the observed export is `hvc1` and Windows plus older players routinely will not decode
    /// it, so the default has to produce files that play. Turning it off means no video pixel is
    /// re-encoded at all — and therefore that no overlay is burned in, since burning one is a
    /// re-encode.
    pub transcode: bool,
    /// Where ffmpeg is, from [`crate::export::env::locate`]. `None` degrades exactly like
    /// [`Self::transcode`] being off, and the run says which of the two it was.
    pub ffmpeg: Option<PathBuf>,
}

impl VideoOptions {
    /// The defaults a real run uses: transcoding on, ffmpeg looked up on `PATH`.
    ///
    /// There is deliberately no `Default` impl. One would have to answer `None` for ffmpeg without
    /// probing, which reads as "transcoding on" while behaving as "transcoding off" — the exact
    /// trap this pass exists to report rather than hide.
    #[must_use]
    pub fn probe() -> Self {
        Self { transcode: true, ffmpeg: env::locate(Tool::Ffmpeg) }
    }

    /// The ffmpeg to run, or why there will not be one.
    fn transcoder(&self) -> Result<&Path, TranscodeSkip> {
        match (self.transcode, self.ffmpeg.as_deref()) {
            (false, _) => Err(TranscodeSkip::OptedOut),
            (true, None) => Err(TranscodeSkip::NoFfmpeg),
            (true, Some(ffmpeg)) => Ok(ffmpeg),
        }
    }
}

/// The calendar instant of a [`Timestamp`], or `None` when it names no real one.
///
/// [`Timestamp`] is range-checked rather than calendar-checked, so `2021-02-30` parses. This is
/// the first caller handing one to a date crate, which the design says has to convert fallibly.
pub(crate) fn calendar(timestamp: Timestamp) -> Option<NaiveDateTime> {
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

/// The source bytes one planned item is built from, with no leg-specific record attached.
///
/// **Both legs plan against this, which is what lets one [`fix`] serve both.** A memory's
/// `-main`/`-overlay` pair and a chat-media unit's file plus its zip-paired overlay reduce to the
/// same four facts, and the item-level pass asks nothing beyond them: it composites, stamps, writes
/// and dates. What each leg's own discovery type carries past this — a uuid, a date bucket, a
/// filename family, a history token — decides which items exist and where they land, and both of
/// those questions are answered before a byte is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMedia {
    /// The file the output is made from.
    pub main: PathBuf,
    /// The day [`Self::main`]'s own filename leads with, which is the last step of either leg's
    /// date chain. Carried here rather than re-parsed, because the two legs spell a filename
    /// differently and each has already parsed its own.
    pub day: Day,
    /// [`Self::main`]'s extension, verbatim as its name spells it. Carried rather than re-derived
    /// from the path, so the leg reads back the same string its own discovery parsed.
    pub extension: String,
    /// The caption layer composited over it, where the export ships one as a separate file.
    pub overlay: Option<PathBuf>,
}

/// One item this run is going to fix, and everything it needs to do it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedItem {
    /// The manifest's identity: a memory's uuid, or a chat-media unit's file id.
    pub source_id: String,
    pub media: SourceMedia,
    /// Which half of the pass fixes it. Also what decides [`Self::output`]'s extension.
    pub leg: Leg,
    pub capture: Capture,
    /// The coordinate this item is allowed to carry, after decision 32. `None` means it gets none,
    /// whether because the entry had none, because its bucket disagreed, or — on the chat-media
    /// leg, always — because the export states no coordinate for chat media anywhere.
    pub location: Option<LocationPoint>,
    /// Who sent it and in which thread, decision 44c: metadata only, never the filename. `None` on
    /// the memories leg, which has neither.
    pub attribution: Option<Attribution>,
    /// Where the fixed copy lands.
    pub output: PathBuf,
    /// The directory the export's own files are copied into verbatim, beside the output.
    ///
    /// Decision 44b's `both` overlay mode, which only the chat-media leg runs: the merged file is
    /// what the run produces and the originals are what it refuses to lose. `None` on the memories
    /// leg, whose composite is the only artifact it has ever written.
    pub originals: Option<PathBuf>,
}

/// Why a paired memory is not in [`Plan::items`].
///
/// None of these is a failure and none is written to the manifest: the rows stay
/// [`crate::export::manifest::ItemStatus::Pending`], so whichever leg can handle them picks them
/// up untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeferralReason {
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
    /// Which manifest rows this plan is about. Carried here rather than passed to [`run`] beside
    /// it, so a plan and the kind it transitions cannot be handed over out of step.
    pub kind: ItemKind,
    /// One per item whose media this build can fix, in the enumeration's own order.
    pub items: Vec<PlannedItem>,
    /// Items this pass will not touch, in the same order.
    pub deferred: Vec<Deferred>,
    /// Source ids this plan deliberately produces no output for, in the same order.
    ///
    /// Decision 44d's dropped chat-media thumbnails. Different from [`Self::deferred`] in the one
    /// way that matters to a resume: a deferred row stays
    /// [`crate::export::manifest::ItemStatus::Pending`] so a later build can pick it up, while these
    /// are written [`crate::export::manifest::ItemStatus::Excluded`] and taken off the work list, so
    /// nothing re-offers them every run for ever. Always empty on the memories leg.
    pub excluded: Vec<String>,
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

            let Some(leg) = Leg::of(&media.main.extension) else {
                defer(DeferralReason::UnknownFormat);
                continue;
            };

            let source = SourceMedia {
                main: media.main.path.clone(),
                day: media.main.day,
                extension: media.main.extension.clone(),
                overlay: media.overlay.as_ref().map(|file| file.path.clone()),
            };
            let location = permitted_location(&item.pairing, memory, &agreements);
            let Some(capture) = capture_of(&item.pairing, memory, &source, leg, location) else {
                defer(DeferralReason::NoCalendarDate);
                continue;
            };

            // Keyed by the whole file name rather than the stem: two memories collide only when
            // they would land on one path, and a video and an image on the same second do not. The
            // extension is the RESOLVED one, so a copied-through PNG and a stamped JPEG on the same
            // second do not claim each other's suffix.
            let stem = capture.local.format("%Y%m%d_%H%M%S").to_string();
            let extension = output_extension(leg, &source);
            let ordinal = taken.entry(format!("{stem}.{extension}")).or_default();
            let output = output_path(out_root, capture.local, &stem, &extension, *ordinal);
            *ordinal += 1;

            items.push(PlannedItem {
                source_id: item.source_id.clone(),
                media: source,
                leg,
                capture,
                location,
                // A memory has no sender and no thread, so nothing reaches the two metadata fields
                // decision 44c defines and whatever a source file already held in them survives.
                attribution: None,
                output,
                // The memories leg has always written its composite and nothing else; decision 44b's
                // `both` mode is the chat leg's, and widening it here would change what a memories
                // run produces.
                originals: None,
            });
        }

        Self { kind: ItemKind::Memory, items, deferred, excluded: Vec::new() }
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
fn capture_of(pairing: &Pairing, memory: &Memory, media: &SourceMedia, leg: Leg, location: Option<LocationPoint>) -> Option<Capture> {
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
    if let Some(capture) = embedded(leg, media, location) {
        return Some(capture);
    }

    // The day in the filename, which is the only date left. Midnight is a placeholder rather than
    // a claim, which is what [`TimeSource::Filename`] exists to say.
    Capture::from_day(media.day)
}

/// The capture time the main file carries in its own metadata, if it carries one.
///
/// The two containers say different things and are read accordingly, which is the whole reason
/// this is not one call. A JPEG's `DateTimeOriginal` is a **local wall time** with the zone in a
/// separate tag that may be absent. An MP4 header time is a **UTC instant** with no zone field at
/// all, so it goes through the same conversion an entry's `Date` does and comes out in the zone the
/// coordinate places it in.
pub(crate) fn embedded(leg: Leg, media: &SourceMedia, location: Option<LocationPoint>) -> Option<Capture> {
    match leg {
        Leg::Image => {
            let jpeg = Jpeg::read(&media.main).ok()?;
            Some(Capture { local: jpeg.embedded_time()?, offset: jpeg.embedded_offset(), source: TimeSource::Embedded })
        }
        // Reads the movie box alone rather than the whole file: this runs at plan time for every
        // video whose time falls back to it, and those same files are read in full again when the
        // run reaches them.
        Leg::Video => {
            let utc = crate::export::video::header_time(&media.main)?;
            Some(Capture::from_utc(utc.naive_utc(), location, TimeSource::Embedded))
        }
    }
}

/// Whether this item's bytes are copied through under their own extension rather than becoming a
/// JPEG.
///
/// **Decision 47**: a PNG with nothing to composite is not re-encoded, so its transparency survives
/// — `image`'s flatten drops the alpha channel rather than compositing it, and with no main behind
/// the layer there is nothing for it to show through to. That is the rule the video leg already
/// follows: nothing is touched when there is nothing to touch it for.
///
/// **Both the plan and [`fix_image`] ask this one function**, which is what keeps the extension a
/// name claims and the bytes that land under it from disagreeing. The plan needs it because every
/// output path AND every collision key is decided before a byte is written; `fix_image` needs it
/// because it decides whether to encode. Pinned at both call sites rather than only here: a fixture
/// with a lone PNG and one with a lone JPEG make the two answers differ.
#[must_use]
pub(crate) fn passes_through(leg: Leg, media: &SourceMedia) -> bool {
    leg == Leg::Image && media.overlay.is_none() && matches(&media.extension, &PASS_THROUGH_EXTENSIONS)
}

/// The extension an item's output carries.
///
/// Worked out at PLAN time and never at fix time, because the collision key and the output name both
/// depend on it: an extension decided later would let two items that collide stop colliding, which
/// moves a `_2` suffix between runs.
///
/// The pass-through arm answers with the item's OWN extension rather than with a member of
/// [`PASS_THROUGH_EXTENSIONS`], so that list's length is not load-bearing here — see the constant for
/// what indexing it cost. **Normalized to lower case**, which is the load-bearing half of that: the
/// membership test is ascii-case-insensitive, so a `.PNG` source is admitted, and answering with its
/// own spelling would put `.PNG` in the output path while the same file spelled `.png` produced
/// `.png`. Both planners key their collision map on this string, so a divergence there moves output
/// paths rather than staying cosmetic. Pinned by
/// `a_shouted_extension_is_normalized_rather_than_carried_into_the_output_path`.
#[must_use]
pub(crate) fn output_extension(leg: Leg, media: &SourceMedia) -> Cow<'_, str> {
    if !passes_through(leg, media) {
        return Cow::Borrowed(leg.extension());
    }
    // Borrowed on the common path — every observed name is already lower case — and owned only when
    // normalizing actually changes something.
    if media.extension.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(media.extension.to_ascii_lowercase())
    } else {
        Cow::Borrowed(media.extension.as_str())
    }
}

/// `<stem>.<ext>`, with `_2`, `_3` and so on for a name an earlier position in the plan claimed.
///
/// The suffix counts from two, so the first file of a colliding set keeps the plain name and nobody
/// has to work out that `_1` means "the second one". Shared by both legs: they disagree about which
/// directory an item lands in and about what a collision is keyed on, and not at all about how a
/// broken collision is spelled.
///
/// Takes the resolved extension rather than the [`Leg`], so a caller cannot key a collision on one
/// answer and emit a name carrying another.
pub(crate) fn output_name(stem: &str, extension: &str, ordinal: u32) -> String {
    if ordinal == 0 { format!("{stem}.{extension}") } else { format!("{stem}_{}.{extension}", ordinal + 1) }
}

/// The memories tree: `<root>/YYYY/MM/YYYYMMDD_HHMMSS.<ext>`.
fn output_path(root: &Path, local: NaiveDateTime, stem: &str, extension: &str, ordinal: u32) -> PathBuf {
    root.join(local.format("%Y").to_string()).join(local.format("%m").to_string()).join(output_name(stem, extension, ordinal))
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
    /// Items left to another leg. See [`Plan::deferred`].
    pub deferred: usize,
    /// Items this build deliberately writes nothing for, each parked
    /// [`crate::export::manifest::ItemStatus::Excluded`]. See [`Plan::excluded`].
    pub excluded: usize,
    /// What the run finished but did not do in full, per item. See [`Notice`].
    pub notices: Vec<ItemNotice>,
}

/// One item a run could not fix, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub source_id: String,
    /// The message that also went into the manifest's `last_error`, where it is redacted on the
    /// way in.
    pub reason: String,
}

/// A [`Notice`] against the memory it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemNotice {
    pub source_id: String,
    pub notice: Notice,
}

/// Something a run finished an item without doing.
///
/// Not a failure and not written to the manifest: the item is done, the output is on disk, and its
/// row is `Done`. What a notice records is that the file is less repaired than a run with more
/// available could have made it, which is a thing a user gets to know rather than discover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Notice {
    /// The video's pixels were passed through untouched, so it is still in whatever codec the
    /// export shipped — HEVC, on every memory video observed.
    NotTranscoded(TranscodeSkip),
    /// It carries an overlay and nothing burned it in. Only ever reported alongside
    /// [`Self::NotTranscoded`], because burning a caption into a video **is** a re-encode and a run
    /// that is not transcoding must not pay one.
    OverlayNotBurned,
    /// The video already carries a `udta` LOCATION atom, which both readers resolve ahead of
    /// anything writable here, so the coordinate was skipped rather than written where it would be
    /// read past. Unobserved on real memory videos, whose `©eng` sentinel is not one of these.
    LocationShadowed(LocationAtom),
    /// Its bytes were copied through under their own extension, so it carries no capture metadata at
    /// all — decision 47's PNG pass-through. The date still reaches the file's own modification
    /// time; the sender and the conversation reach nothing.
    ///
    /// Reported for the reason every notice is: an item finished with less repair than a fuller run
    /// could have given it is a thing a user gets to be told, not to discover by opening the file.
    NotStamped,
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotTranscoded(TranscodeSkip::OptedOut) => f.write_str("kept in its original codec, since transcoding is off"),
            Self::NotTranscoded(TranscodeSkip::NoFfmpeg) => {
                f.write_str("kept in its original codec, since ffmpeg is not installed; install it and re-run to transcode")
            }
            Self::OverlayNotBurned => {
                f.write_str("its caption layer was not drawn in, because burning one into a video needs the re-encode this run did not do")
            }
            Self::LocationShadowed(atom) => write!(
                f,
                "its coordinates were left alone: it already carries a {atom} location atom, which every reader resolves \
                 ahead of anything this build can write, so a second one would be ignored"
            ),
            Self::NotStamped => f.write_str(
                "copied through as a PNG so its transparency survives, which means the capture date reached only the file's \
                 own date and the sender and conversation reached nothing: this build writes that metadata into JPEG alone",
            ),
        }
    }
}

/// Why a video's pixels were not re-encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TranscodeSkip {
    /// The caller turned transcoding off.
    OptedOut,
    /// ffmpeg is not on `PATH`. Decision 2's degrade: an optional tool being absent costs a
    /// capability, never the run.
    NoFfmpeg,
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
pub fn run(plan: &Plan, manifest: &mut Manifest, max_attempts: u32, video: &VideoOptions) -> Result<FixReport, ManifestError> {
    // Before the sweep, not after it: an excluded row has to already be excluded when `resume`
    // counts the statuses and when `pending` reads the work list, or the first run of a plan reports
    // every one of them as owed work it then never does. Idempotent from the second run on —
    // `mark_excluded` leaves an already-excluded row's timestamp alone — and a no-op loop on the
    // memories leg, whose `excluded` is always empty.
    manifest.exclude(plan.kind, &plan.excluded)?;
    let resumed = manifest.resume(plan.kind)?;
    let owed: BTreeSet<String> = manifest.pending(plan.kind, max_attempts)?.into_iter().map(|item| item.source_id).collect();

    let mut report = FixReport {
        resumed,
        fixed: 0,
        failed: Vec::new(),
        skipped: 0,
        deferred: plan.deferred.len(),
        excluded: plan.excluded.len(),
        notices: Vec::new(),
    };
    for item in &plan.items {
        if !owed.contains(&item.source_id) {
            report.skipped += 1;
            continue;
        }
        match fix(item, video) {
            Ok(notices) => {
                manifest.mark_done(plan.kind, &item.source_id, &item.output)?;
                report.fixed += 1;
                report.notices.extend(notices.into_iter().map(|notice| ItemNotice { source_id: item.source_id.clone(), notice }));
            }
            Err(error) => {
                let reason = error.to_string();
                manifest.mark_failed(plan.kind, &item.source_id, &reason)?;
                report.failed.push(Failure { source_id: item.source_id.clone(), reason });
            }
        }
    }
    Ok(report)
}

/// Composites or transcodes, stamps, writes and dates one memory.
///
/// The OUTPUT is never left half-written: it is one `fs::write` of a finished buffer, so a failure
/// before it leaves no file at all and a failure after it leaves a complete file whose date the
/// next run corrects. A transcode is the one step that cannot be a buffer, and it writes to a
/// scratch name that is removed however this returns.
///
/// **That claim is about the output and does not extend to [`keep_originals`]' copies**, which are
/// `fs::copy` and carry the same create-truncate-write window `fs::write` does. What makes the
/// difference is what the manifest checks: it hashes [`PlannedItem::output`] and nothing else, so a
/// truncated original is not something a later resume can notice. The window is a crash mid-copy
/// and the cost is one un-merged file, which the source still holds; closing it would mean hashing
/// the copies too, and nothing has asked for that.
///
/// Returns what the item was finished without. See [`Notice`].
///
/// # Errors
///
/// Returns [`FixError`] when any step fails.
pub fn fix(item: &PlannedItem, video: &VideoOptions) -> Result<Vec<Notice>, FixError> {
    let notices = match item.leg {
        Leg::Image => fix_image(item)?,
        Leg::Video => fix_video(item, video)?,
    };
    set_modified(&item.output, item.capture.instant()).map_err(|source| FixError::Touch { path: item.output.clone(), source })?;
    keep_originals(item)?;
    Ok(notices)
}

/// Copies the export's own files beside the output, decision 44b's `both` overlay mode.
///
/// Verbatim, under the names the export gave them, which is the point of keeping them at all: the
/// output is the repaired file and these are the bytes it was made from, so putting them under the
/// output's `YYYYMMDD_HHMMSS` shape would leave the run with two files claiming to be the same
/// thing. They get the output's modification time so a browser sorts the set together, and nothing
/// else about them is touched — no composite, no stamp.
///
/// A no-op unless the item names a [`PlannedItem::originals`] directory **and** carries an overlay:
/// with nothing composited there is no un-merged version to lose, and decision 46c's "the two
/// originals" is exactly the pair a composite consumed. Decision 47 closed the case that used to sit
/// here as a residual — a lone PNG is no longer re-encoded at all, so its own bytes ARE the output
/// and there is nothing left to keep a copy of.
fn keep_originals(item: &PlannedItem) -> Result<(), FixError> {
    let (Some(dir), Some(overlay)) = (&item.originals, &item.media.overlay) else {
        return Ok(());
    };
    fs::create_dir_all(dir).map_err(|source| FixError::Create { path: dir.clone(), source })?;
    for source in [&item.media.main, overlay] {
        // A discovered file always has a name; a source that somehow does not is skipped rather
        // than joined onto the directory itself, which would overwrite the directory's own path.
        let Some(name) = source.file_name() else { continue };
        let copy = dir.join(name);
        fs::copy(source, &copy).map_err(|error| FixError::Copy { from: source.clone(), to: copy.clone(), source: error })?;
        set_modified(&copy, item.capture.instant()).map_err(|source| FixError::Touch { path: copy, source })?;
    }
    Ok(())
}

/// The pure-Rust leg: composite the overlay in, stamp the EXIF, write a JPEG — or, for a PNG with
/// nothing to composite, copy the bytes through untouched.
///
/// The output directory is made last, right before the write, so a failure earlier leaves no empty
/// year and month behind. The video leg cannot do the same, since ffmpeg needs somewhere to mux
/// into before there is anything to write.
///
/// Returns what the item was finished without. See [`Notice`].
fn fix_image(item: &PlannedItem) -> Result<Vec<Notice>, FixError> {
    // Decision 47. Nothing is composited, so nothing is re-encoded and the alpha survives: `image`'s
    // flatten DROPS the alpha channel rather than compositing it, and with no main behind the layer
    // there is nothing for a transparent region to show through to, so it would land as whatever RGB
    // happened to sit under `alpha = 0`.
    //
    // **No metadata call on this path, deliberately and by construction.** `Jpeg` never sees these
    // bytes, so no `little_exif` entry point is reachable from here under any spelling — which is
    // what keeps RUSTSEC-2026-0194 unreachable rather than merely unobserved. See `exif.rs`'s
    // `library` module doc before adding anything to this branch.
    if passes_through(item.leg, &item.media) {
        let bytes = fs::read(&item.media.main).map_err(|source| FixError::Read { path: item.media.main.clone(), source })?;
        make_parent(&item.output)?;
        fs::write(&item.output, &bytes).map_err(|source| FixError::Create { path: item.output.clone(), source })?;
        return Ok(vec![Notice::NotStamped]);
    }

    // Matched on the overlay itself rather than on a predicate, so the composite arm can only be
    // reached with an overlay in hand — which is what let `overlay::compose` drop its `Option`.
    let bytes = match item.media.overlay.as_deref() {
        Some(overlay) => overlay::compose(&item.media.main, overlay)?,
        // No overlay, so nothing is composited and re-encoding would spend a generation of lossy
        // compression for nothing; the copy is also what keeps whatever EXIF the source carried
        // around for `stamp` to read and preserve.
        //
        // Everything reaching this arm is already a JPEG, which is why there is no extension check:
        // `Leg::of` admits `jpg`, `jpeg` and `png` alone, and `passes_through` answered `png`
        // above. A fourth image format added to that list would arrive here and be refused by
        // `Jpeg::new` below, naming the file — a loud per-item failure rather than a silent
        // re-encode of a format nobody validated.
        None => fs::read(&item.media.main).map_err(|source| FixError::Read { path: item.media.main.clone(), source })?,
    };

    // The gate, and it runs before anything else looks at the bytes. Ordering matters: reading the
    // dimensions first means a corrupt file is reported by the image decoder, whose message is
    // about a byte count rather than about the file being unusable, and the guard never gets to say
    // what it refused. A `.jpg` that is really something else fails here, not further in.
    let mut jpeg = Jpeg::new(bytes).map_err(|source| ExifError::NotJpeg { path: item.media.main.clone(), source })?;
    let (width, height) = overlay::dimensions(jpeg.as_bytes())?;
    jpeg.stamp(&Stamp {
        local: item.capture.local(),
        offset: item.capture.offset(),
        location: item.location,
        width,
        height,
        attribution: item.attribution.as_ref(),
    })?;
    make_parent(&item.output)?;
    jpeg.write(&item.output)?;
    Ok(Vec::new())
}

/// The video leg: ffmpeg for pixels when the run is transcoding, pure Rust for metadata always.
///
/// **The metadata half never varies.** Whether the frames were re-encoded or copied, the times and
/// the coordinate go in through [`crate::export::video`], so there is one code path to reason about
/// and one to test. ffmpeg's job here is pixels and nothing else.
fn fix_video(item: &PlannedItem, options: &VideoOptions) -> Result<Vec<Notice>, FixError> {
    let overlay = item.media.overlay.as_deref();
    let mut notices = Vec::new();

    let mut video = match options.transcoder() {
        Ok(ffmpeg) => {
            make_parent(&item.output)?;
            let scratch = Scratch::beside(&item.output)?;
            ffmpeg::transcode(ffmpeg, &item.media.main, overlay, scratch.path())?;
            // Reading the scratch file back is load-bearing, not plumbing: **ffmpeg can exit 0
            // having written nothing at all** (measured — an argument list it parses but that names
            // no output file exits 0 with an empty directory behind it). This read is what turns
            // that into a per-item failure instead of a zero-byte video the manifest records as
            // done. Do not "optimise" it into a rename.
            let bytes = fs::read(scratch.path()).map_err(|source| FixError::Read { path: scratch.path().to_path_buf(), source })?;
            // Blamed on the re-encode, not on the memory: these bytes are ffmpeg's output, and
            // reporting them as `NotMp4` against the SOURCE path would tell a user their perfectly
            // good video "needs converting first" when converting it is exactly what broke it.
            Mp4::new(bytes).map_err(|source| FixError::Transcoded { main: item.media.main.clone(), source })?
        }
        Err(skip) => {
            notices.push(Notice::NotTranscoded(skip));
            // Burning a caption in is a re-encode, so a run that is not transcoding cannot draw
            // one. Said out loud rather than left for the user to notice a missing caption.
            if overlay.is_some() {
                notices.push(Notice::OverlayNotBurned);
            }
            Mp4::read(&item.media.main)?
        }
    };

    // Asked before the stamp so the answer can be reported, rather than inferred from the stamp
    // having quietly skipped it.
    if let (Some(atom), Some(_)) = (video.location_atom(), item.location) {
        notices.push(Notice::LocationShadowed(atom));
    }
    video.stamp(&VideoStamp {
        local: item.capture.local(),
        offset: item.capture.offset(),
        location: item.location,
        attribution: item.attribution.as_ref(),
    })?;
    make_parent(&item.output)?;
    video.write(&item.output)?;
    Ok(notices)
}

/// Makes the year and month directories an output lands in. Idempotent, so the video leg calling it
/// twice — once for the scratch file, once for the real write — costs a stat.
fn make_parent(output: &Path) -> Result<(), FixError> {
    match output.parent() {
        Some(parent) => fs::create_dir_all(parent).map_err(|source| FixError::Create { path: parent.to_path_buf(), source }),
        None => Ok(()),
    }
}

/// The file a transcode writes into, removed whatever happens next.
///
/// ffmpeg has to mux into something seekable, so this is the pass's only output that is not one
/// `fs::write` of a finished buffer. Keeping it under a reserved name beside the real output and
/// deleting it on drop is what keeps a failed item from leaving a half-made video where a later
/// run's resume sweep would hash one.
struct Scratch(PathBuf);

impl Scratch {
    /// A scratch path beside `output`, derived from its name so two items in one directory cannot
    /// pick the same one.
    fn beside(output: &Path) -> Result<Self, FixError> {
        let (Some(parent), Some(name)) = (output.parent(), output.file_name()) else {
            return Err(FixError::Create { path: output.to_path_buf(), source: io::Error::from(io::ErrorKind::InvalidInput) });
        };
        let mut scratch = std::ffi::OsString::from(SCRATCH_PREFIX);
        scratch.push(name);
        Ok(Self(parent.join(scratch)))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best-effort: the run has already produced its real answer by here, and a scratch file
        // that outlives a crash is a hidden file rather than a corrupted memory.
        let _ = fs::remove_file(&self.0);
    }
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
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Compose {
        source: OverlayError,
    },
    Metadata {
        source: ExifError,
    },
    /// The video's container metadata could not be read or written.
    Container {
        source: VideoError,
    },
    /// ffmpeg could not re-encode this item. Per-item like the rest: a run keeps going and the
    /// videos ffmpeg is happy with still get transcoded.
    Transcode {
        source: FfmpegError,
    },
    /// ffmpeg exited cleanly and produced something this build cannot walk.
    ///
    /// Split from [`Self::Container`] because the two need opposite advice. A source this build
    /// cannot read is a memory in the wrong container and the fix is to convert it; **this** is a
    /// memory that was fine until the re-encode touched it, and the fix is to stop re-encoding.
    /// Telling the second user the first user's answer sends them to convert a working file.
    Transcoded {
        /// The memory, which is what the user recognises — not the scratch file it came out of.
        main: PathBuf,
        source: NotMp4,
    },
    Create {
        path: PathBuf,
        source: io::Error,
    },
    /// One of the export's own files could not be copied beside the output.
    ///
    /// Split from [`Self::Create`] and [`Self::Read`] because it is the only step naming two paths,
    /// and a message that picked one of them would send a user to check the wrong end.
    Copy {
        from: PathBuf,
        to: PathBuf,
        source: io::Error,
    },
    Touch {
        path: PathBuf,
        source: io::Error,
    },
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

impl From<VideoError> for FixError {
    fn from(source: VideoError) -> Self {
        Self::Container { source }
    }
}

impl From<FfmpegError> for FixError {
    fn from(source: FfmpegError) -> Self {
        Self::Transcode { source }
    }
}

impl fmt::Display for FixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "could not read {}: {source}", path.display()),
            Self::Compose { source } => write!(f, "{source}"),
            Self::Metadata { source } => write!(f, "{source}"),
            Self::Container { source } => write!(f, "{source}"),
            Self::Transcode { source } => write!(f, "{source}"),
            // `what()` rather than `{source}`: the full `NotMp4` message ends in the advice that
            // fits a SOURCE file ("needs converting first"), which is the exact wrong answer here
            // and would sit inside this sentence contradicting it.
            Self::Transcoded { main, source } => write!(
                f,
                "re-encoding {} produced a video this build cannot read ({}); the memory itself is fine, so this is \
                 the transcode going wrong rather than the export — re-run with transcoding off to copy it instead",
                main.display(),
                source.what()
            ),
            Self::Create { path, source } => {
                write!(f, "could not create {}: {source}; check the output directory is writable", path.display())
            }
            Self::Copy { from, to, source } => write!(
                f,
                "could not keep a copy of {} at {}: {source}; check the source is readable and the output directory is writable",
                from.display(),
                to.display()
            ),
            Self::Touch { path, source } => write!(f, "wrote {} but could not set its date: {source}", path.display()),
        }
    }
}

impl Error for FixError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Create { source, .. } | Self::Copy { source, .. } | Self::Touch { source, .. } => {
                Some(source)
            }
            Self::Compose { source } => Some(source),
            Self::Metadata { source } => Some(source),
            Self::Container { source } => Some(source),
            Self::Transcode { source } => Some(source),
            Self::Transcoded { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::NaiveDate;

    use super::{Capture, Leg, TimeSource, VideoOptions, output_path};

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day).unwrap().and_hms_opt(hour, minute, second).unwrap()
    }

    #[test]
    fn an_output_path_is_year_month_and_the_local_wall_time() {
        let local = at(2021, 1, 15, 14, 30, 5);
        assert_eq!(
            output_path(Path::new("/out"), local, "20210115_143005", Leg::Image.extension(), 0),
            Path::new("/out/2021/01/20210115_143005.jpg")
        );
        // Same tree, same name, and the extension is the only thing the leg moves.
        assert_eq!(
            output_path(Path::new("/out"), local, "20210115_143005", Leg::Video.extension(), 0),
            Path::new("/out/2021/01/20210115_143005.mp4")
        );
    }

    #[test]
    fn a_name_a_previous_item_already_claimed_gets_a_counted_suffix() {
        let local = at(2021, 1, 15, 0, 0, 0);
        // The suffix counts from two, so the first file of a colliding set keeps the plain name and
        // nobody has to work out that `_1` means "the second one".
        let named = |ordinal| output_path(Path::new("/out"), local, "20210115_000000", Leg::Image.extension(), ordinal);
        assert_eq!(named(1), Path::new("/out/2021/01/20210115_000000_2.jpg"));
        assert_eq!(named(2), Path::new("/out/2021/01/20210115_000000_3.jpg"));
        assert_eq!(
            output_path(Path::new("/out"), local, "20210115_000000", Leg::Video.extension(), 1),
            Path::new("/out/2021/01/20210115_000000_2.mp4")
        );
    }

    #[test]
    fn a_run_with_no_ffmpeg_reads_as_degraded_rather_than_as_transcoding() {
        // The trap a `Default` impl would set: transcoding "on" with nothing to do it. Both halves
        // have to be true before ffmpeg is invoked, and the run says which one was missing.
        assert_eq!(VideoOptions { transcode: true, ffmpeg: None }.transcoder(), Err(super::TranscodeSkip::NoFfmpeg));
        assert_eq!(
            VideoOptions { transcode: false, ffmpeg: Some("/usr/bin/ffmpeg".into()) }.transcoder(),
            Err(super::TranscodeSkip::OptedOut)
        );
        // Opting out outranks having the tool, so a user's "do not re-encode" is never overridden
        // by ffmpeg happening to be installed.
        assert_eq!(VideoOptions { transcode: false, ffmpeg: None }.transcoder(), Err(super::TranscodeSkip::OptedOut));
        assert_eq!(VideoOptions { transcode: true, ffmpeg: Some("/usr/bin/ffmpeg".into()) }.transcoder(), Ok(Path::new("/usr/bin/ffmpeg")));
    }

    #[test]
    fn a_transcode_that_produced_rubbish_blames_the_re_encode_rather_than_the_memory() {
        // These two failures carry opposite advice and the wrong one sends a user to convert a file
        // that was already fine. Unreachable today — nothing found an ffmpeg invocation that exits
        // zero and muxes something unwalkable — so the wording is what a test can hold, and the
        // wording is the whole point of the variant existing.
        let source = super::NotMp4::Signature { found: b"\x89PNG".to_vec() };
        let transcoded = super::FixError::Transcoded { main: "/x/2021-01-15_a-main.mp4".into(), source: source.clone() }.to_string();
        assert!(transcoded.contains("re-encoding /x/2021-01-15_a-main.mp4 produced"), "{transcoded}");
        assert!(transcoded.contains("the memory itself is fine"), "{transcoded}");
        assert!(transcoded.contains("transcoding off"), "{transcoded}");

        // The source-side failure keeps the advice this one must not borrow.
        let unreadable =
            super::FixError::Container { source: super::VideoError::NotMp4 { path: "/x/2021-01-15_a-main.mp4".into(), source } }
                .to_string();
        assert!(unreadable.contains("needs converting first"), "{unreadable}");
        assert!(!transcoded.contains("needs converting first"), "the two must not give the same advice: {transcoded}");
    }

    #[test]
    fn every_extension_the_export_carries_lands_on_a_leg() {
        // Ascii-case-insensitive, matching how the rest of the export layer reads an extension.
        for (extension, leg) in [
            ("jpg", Some(Leg::Image)),
            ("JPEG", Some(Leg::Image)),
            ("png", Some(Leg::Image)),
            ("mp4", Some(Leg::Video)),
            ("MP4", Some(Leg::Video)),
        ] {
            assert_eq!(Leg::of(extension), leg, "{extension}");
        }
        assert_eq!(Leg::of("heic"), None, "an unknown format is deferred rather than guessed at");
        assert_eq!(Leg::of("mov"), None);
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
