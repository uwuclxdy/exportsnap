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
//! An image is composited and stamped entirely in pure Rust and lands as a JPEG, unless its own
//! format CAN carry an alpha channel JPEG would drop — read off the extension, never off the pixels
//! — in which case it keeps that format and is not stamped at all ([`needs_its_own_format`],
//! [`Notice::NotStamped`]). A video's
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
//! which enumeration produced them. So is [`Outputs`], which decides that output path: the two legs
//! disagree about which directory an item lands in and about how its stem is worked out, and not at
//! all about what a collision is or what a run already wrote. [`super::chat_fix`] is the second planner, and it fills the
//! same [`Plan`] rather than growing a second copy of the composite-stamp-write-date sequence —
//! two copies of that sequence would be two places a metadata rule has to be kept true.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};

use crate::export::env::{self, Tool};
use crate::export::exif::{ExifError, Jpeg, Stamp};
use crate::export::ffmpeg::{self, FfmpegError};
use crate::export::manifest::{Item, ItemKind, Manifest, ManifestError, ResumeReport};
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

/// How many recorded failures an item may carry before a run stops offering it.
///
/// [`run`] takes the cap as an argument because it is a caller's policy and a test drives it with
/// its own; this is the one both run compositions pass. One constant rather than one per leg: two
/// legs agreeing on a number by coincidence is a number that drifts.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Extensions the image leg reads. A main outside this set is deferred rather than attempted.
///
/// **The ceiling, and its upgrade path.** The image leg admits exactly these three today, and adding
/// a fourth needs one answer: **does JPEG lose something this format carries?** That is
/// [`ALPHA_CAPABLE_EXTENSIONS`], and it decides whether the output keeps the source's own format or
/// becomes a JPEG.
///
/// The second question this paragraph used to ask — **can this build stamp it?** — is no longer
/// independent of the first. [`crate::export::exif`] writes metadata into a JPEG and nothing
/// else, so a format that keeps its own format is unstampable BY THAT FACT, and the run says so per
/// item ([`Notice::NotStamped`]). The implication runs one way only: a format that answers "no"
/// above becomes a JPEG and is stamped like the rest, and it stops holding at all the day
/// `crate::export::exif` learns a second container.
///
/// **What used to be here read the two as independent, and it was right when it was written**:
/// `jpg`/`jpeg` were stamped and re-encoded while `png` was copied through and unstampable, a
/// matched pair no rule produced. Task 45 supplied the rule, so what was a coincidence is now a
/// consequence.
///
/// A format added here but NOT to [`ALPHA_CAPABLE_EXTENSIONS`] reaches [`crate::export::exif::Jpeg`]
/// and is refused by name, one item at a time. That failure is chosen: the alternative is silently
/// re-encoding a format nobody validated into a JPEG and keeping no original, which is the defect
/// class decision 47 exists to close, one format over.
const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

/// Extensions the video leg reads.
const VIDEO_EXTENSIONS: [&str; 1] = ["mp4"];

/// The image formats whose output keeps the SOURCE's own format instead of becoming a JPEG, because
/// JPEG cannot represent the alpha channel they can.
///
/// **Read off the EXTENSION and never off the pixels, deliberately.** Every output path is decided
/// before a byte is read (see [`output_extension`]), so a decode is not available at plan time; a
/// format that CAN carry alpha therefore keeps its format whether or not this file uses the channel.
/// A fully opaque PNG comes out a PNG, **and that is not free**: it costs the whole stamp — capture
/// date, GPS, sender, conversation — because [`crate::export::exif`] writes into a JPEG and nothing
/// else, so the date reaches the file's own mtime and nothing else reaches it at all. Taken anyway,
/// because the alternative is a decode at plan time, and the error it trades against is worse and
/// silent: a transparent main flattened onto black with the manifest recording the run finished.
/// [`Notice::NotStamped`] reports the trade per item rather than leaving it to be found.
///
/// The question is about the OUTPUT'S FORMAT, not about whether the bytes are re-encoded — a `.jpg`
/// main with no overlay is copied byte for byte too, and is still stamped, and it is not in this
/// list because its format is already the leg's default.
///
/// **Read as a membership set and nowhere as a position.** It used to be indexed for the output
/// extension, which made its LENGTH load-bearing at a call site that never mentioned the length: the
/// two readings agreed only while it held exactly one member, and a second one added at the front
/// would have had [`output_extension`] answer with the wrong format's name while the predicate
/// admitted the right one. [`output_extension`] now takes the item's own extension instead, so IT no
/// longer moves when this does.
///
/// **The length is load-bearing again one layer up, so this list may not grow until that is closed.**
/// [`fix_image`]'s arm picks its encoder off this predicate's BOOLEAN and calls
/// [`crate::export::overlay::compose_png`], which hardcodes PNG. A second member would therefore have
/// [`output_extension`] name that member's format while PNG bytes were written under it and
/// `mark_done` recorded the item finished — the same defect the paragraph above records, resurrected
/// at a different call site rather than fixed everywhere. The upgrade path is one edit and it is not
/// in this constant: `fix_image` has to choose its encoder off the RESOLVED extension, and refuse by
/// name a format it has no encoder for, the way [`crate::export::exif::Jpeg`] refuses one. Until
/// then `png` alone is what makes that arm's claims true, and it is a cap rather than a coincidence.
///
/// **The cap is compiler-held, and the assertion below is what holds it.** The array's own length
/// type does not do it alone: bumping the length alongside the new member builds clean (measured
/// 2026-08-09), which by this repo's own test leaves a prose cap a convention rather than a
/// guarantee.
///
/// Growing it is the one question [`IMAGE_EXTENSIONS`] sets out; a format has to be in that list to
/// reach this one at all.
const ALPHA_CAPABLE_EXTENSIONS: [&str; 1] = ["png"];

/// The cap above, held by the compiler rather than by whoever reads the paragraph stating it.
///
/// Same instrument as the TUI's row-fit invariants (`overview.rs`, `memories.rs`, `chat_media.rs`),
/// for the reason `overview.rs` gives at its own: the constraint belongs in a compile-time assertion
/// "rather than left as arithmetic in a comment". The message is the entire explanation anyone gets
/// at the moment they hit it, so it names the two things that have to change together rather than
/// pointing at a doc.
const _: () = assert!(
    ALPHA_CAPABLE_EXTENSIONS.len() == 1,
    "fix_image's arm hardcodes compose_png; a second member needs the encoder picked off the resolved extension first"
);

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
    /// An instant the chat message that named the file stated — its `Created`, or its
    /// `Created(microseconds)` where `Created` is empty: the chat-media leg's first step, and the
    /// one thing in either chain with no twin on the other side. Which of the two it came from is
    /// not carried, because both are the same record speaking and
    /// [`super::chat_media::ChatMediaItem::date`] has already chosen between them. Kept apart from
    /// [`Self::Entry`] rather than reworded to cover both, because the two are different records in
    /// different files and a user reading "the memory's own entry" against a chat photo learns
    /// something false.
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

    /// An instant the message that named a chat-media file stated: its `Created`, or the
    /// `Created(microseconds)` its empty `Created` fell through to.
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
    /// what every output path and every collision key goes through. An image whose format JPEG can
    /// hold comes out as a JPEG whatever it went in as; one whose format CAN carry alpha keeps that
    /// format, composited or copied alike, per [`needs_its_own_format`]. Videos come out as MP4
    /// whether they were re-encoded or copied, since both routes end in an MP4 container.
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
    /// The export's own files kept verbatim beside the output, or `None` when nothing is kept.
    ///
    /// Decision 44b's overlay mode, which only the chat-media leg runs. `None` on the memories leg,
    /// whose composite is the only artifact it has ever written.
    pub originals: Option<Originals>,
}

/// The export's own two files, copied verbatim beside the output.
///
/// **The overlay is here rather than read back off [`SourceMedia::overlay`], and that split is the
/// whole overlay-mode seam.** [`SourceMedia`] says what the item-level pass CONSUMES; this says what
/// the run refuses to lose. Under decision 44b's `originals` mode the caption is never burned in, so
/// the pass is handed a main with no overlay — and this is then the only thing that still knows one
/// existed. Under `both` the two carry the same path; under `merged` this is `None` and only
/// [`SourceMedia`] carries it.
///
/// A pair, not a list: what decision 46c keeps is exactly the two files a composite would have
/// consumed, and the main is already [`SourceMedia::main`]. An item with nothing to keep is `None`
/// on [`PlannedItem::originals`] rather than an empty collection here, so "keep originals" and
/// "there is an overlay to keep" cannot be recorded out of step — which is the agreement two
/// independent `Option` fields used to rest on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Originals {
    /// The directory the copies land in.
    pub dir: PathBuf,
    /// The caption layer, under the name the export gave it.
    pub overlay: PathBuf,
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
/// answer than the first one, depending on which of the two had finished.
///
/// A name a run already placed comes back off the manifest and every name any row records is
/// reserved before one is derived — [`RecordedOutputs`] and [`Outputs`] are that, and the constant
/// they preserve is that no item is ever planned onto a path another row's record still claims. A
/// name nothing has recorded is a position in this list, which is what makes a first run's answer
/// and a re-plan of it the same answer.
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
    ///
    /// `recorded` is where this export's memories have been landing so far, and a caller with no
    /// manifest to read one out of passes [`RecordedOutputs::default`] to get a first run's answer.
    /// The seed is read in a window with a constraint on each side. AFTER the enrollment, because
    /// enrollment `reset`s a row whose source came back (`memories.rs`'s
    /// `SourceMissing | Retired` arm, `chat_media.rs`'s parked set) and a reset clears the output
    /// record, so a read ahead of it adopts a path the run is about to stop believing. BEFORE
    /// [`run`]'s resume sweep, because that sweep clears the record of an output the user deleted,
    /// and a cleared record seeds nothing: read afterwards, such an item is adopted by nobody and
    /// derives instead.
    ///
    /// **What the ordering is worth is smaller than it reads, and the reason is the reservation.**
    /// A derived name walks to the first path nothing has claimed, and an adopted path is normally
    /// exactly that — it was assigned by this same walk on an earlier run — so the two mechanisms
    /// usually answer identically and the ordering is unobservable. They separate only where the
    /// plan ORDER moved between runs: a new item sorting ahead of one that already holds a record
    /// derives that record's path first, and the recorded item is pushed off its own file.
    ///
    /// **What is pinned, each row measured by planting that exact mutation rather than argued:**
    ///
    /// | the mutation | what reds |
    /// |---|---|
    /// | default the seed read in `memories_run::prepare` | `a_departed_items_recorded_output_is_reserved_through_the_run_composition`, driven through `memories_run::run` |
    /// | default the seed read in `chat_run::prepare` | `a_conversation_that_outlives_its_neighbour_keeps_its_own_directory` (the directory half) AND `a_newcomer_sorting_ahead_of_a_recorded_item_does_not_take_its_output_name` (the item half), both through `chat_run::run` |
    /// | move the resume sweep AHEAD of the chat seed read | `a_newcomer_sorting_ahead_of_a_recorded_item_does_not_take_its_output_name`, and nothing else — which is what separates "the seed is read" from "read in the right place" |
    /// | move the resume sweep ahead of THIS leg's seed read | `a_newcomer_sorting_ahead_of_a_recorded_item_does_not_take_its_output_name` in `tests/memories_screen.rs`, the twin of the chat one |
    /// | move either seed read ahead of the enrollment | **nothing.** That half of the window is unpinned on both legs |
    ///
    /// So both reads are pinned at the composition and so is each leg's position against its own
    /// sweep. **One gap is left, and this is its own reason rather than a shared one**: nothing
    /// pins either read against the ENROLLMENT. Separating that needs a row that PARKED and came
    /// back — `SourceMissing` or `Retired`, since both legs reset on the pair
    /// (`memories.rs`'s `matches!` on the item's status, `chat_media.rs`'s `parked` set) — because
    /// enrollment's `reset` is what clears a record and a row that never parked has none to clear. That is unbuilt rather than unbuildable — three
    /// `collect()` calls with a source removed and then restored, plus a newcomer to make the
    /// adopted and derived answers differ, all of it with helpers both screen suites already have.
    /// A cost, not a barrier.
    ///
    /// Three earlier drafts of this paragraph were wrong and all three are retracted here rather
    /// than quietly dropped: the first claimed the ordering was what saved a rewrite; the second
    /// claimed no test reached either run composition; the third gave the parked-row reason above
    /// for TWO gaps when it fits only the enrollment one, and named `SourceMissing` alone where both
    /// legs reset on `SourceMissing` or `Retired` — the position gap was open because
    /// no fixture had a newcomer sorting ahead of a recorded item, which is a different fact, and
    /// it is now closed on both legs.
    #[must_use]
    pub fn build(memories: &Memories, reconciliation: &Reconciliation, out_root: impl AsRef<Path>, recorded: &RecordedOutputs) -> Self {
        let out_root = out_root.as_ref();
        let agreements = agreements(memories);
        let mut items = Vec::new();
        let mut deferred = Vec::new();
        let mut outputs = Outputs::new(out_root.to_path_buf(), recorded);

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

            // Keyed by the whole PATH rather than the stem: two memories collide only when they
            // would land on one file, and a video and an image on the same second do not. The
            // extension is the RESOLVED one, so a PNG kept as a PNG and a stamped JPEG on the same
            // second do not claim each other's suffix.
            let stem = capture.local.format("%Y%m%d_%H%M%S").to_string();
            let extension = output_extension(leg, &source);
            let output = outputs.path(&item.source_id, &output_dir(out_root, capture.local), &stem, &extension);

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
                // overlay modes belong to the chat leg, and widening one here would change what a
                // memories run produces.
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

/// Whether this item's output has to be written in the SOURCE's own format rather than as this
/// leg's default, because the default would drop something the source carries.
///
/// **Decision 47 and task 45.** JPEG has nowhere to put an alpha channel, and `image`'s flatten
/// DISCARDS that channel rather than compositing it, so a main carrying transparency comes out as
/// whatever RGB sat under `alpha = 0` — black, for everything the export ships. That is true of a
/// main with no layer over it and of one with a layer over it alike: what an OVERLAY leaves
/// transparent shows the main through, and what the MAIN leaves transparent has nothing behind it
/// either way.
///
/// **This answers ONE of the two questions one predicate used to answer for both call sites**, and
/// the other is deliberately not a predicate at all. This one is about the output's FORMAT: not
/// whether the bytes are re-encoded, and not whether the item is stamped. A `.jpg` main answers
/// `false` and still comes out `.jpg`, because the leg's default already IS its format and nothing
/// has to be kept.
///
/// The other question — are the output's bytes the source's own, verbatim — belongs to [`fix_image`]
/// and is answered there by matching [`SourceMedia::overlay`] with the layer in hand, which is a
/// thing that cannot drift out of step with this. It used to be folded in here as
/// `media.overlay.is_none()`, and the two answers agreed only while every alpha-capable main that
/// paired was re-encoded to JPEG; a composited PNG keeps its format and is NOT a verbatim copy, so
/// one predicate read two ways would now be wrong at one of its call sites.
///
/// **Both the plan and [`fix_image`] ask this one function**, which is what keeps the extension a
/// name claims and the bytes that land under it from disagreeing. The plan needs it because every
/// output path AND every collision key is decided before a byte is written; `fix_image` needs it
/// because it decides which encoder runs. Pinned at both call sites rather than only here.
#[must_use]
pub(crate) fn needs_its_own_format(leg: Leg, media: &SourceMedia) -> bool {
    leg == Leg::Image && matches(&media.extension, &ALPHA_CAPABLE_EXTENSIONS)
}

/// The extension an item's output carries.
///
/// Worked out at PLAN time and never at fix time, because the collision key and the output name both
/// depend on it: an extension decided later would let two items that collide stop colliding, which
/// moves a `_2` suffix between runs.
///
/// **It reads the extension and never the overlay, which is a change rather than a fact that was
/// always true.** While the predicate below folded in `overlay.is_none()`, withholding an overlay
/// moved an output path — so [`super::chat_fix::OverlayMode::Originals`] landed a paired PNG at
/// `.png` and the other two modes at `.jpg`. All three now agree on the NAME. What a mode still
/// decides is the FILE at that name — `originals` withholds the layer, so the pass copies the main
/// byte for byte instead of burning the caption in — and what is kept beside it.
///
/// The format-keeping arm answers with the item's OWN extension rather than with a member of
/// [`ALPHA_CAPABLE_EXTENSIONS`], so that list's length is not load-bearing here — see the constant
/// for what indexing it cost. **Normalized to lower case**, which is the load-bearing half of that: the
/// membership test is ascii-case-insensitive, so a `.PNG` source is admitted, and answering with its
/// own spelling would put `.PNG` in the output path while the same file spelled `.png` produced
/// `.png`. Both planners build the path [`Outputs`] claims out of this string, so a divergence there
/// moves output paths rather than staying cosmetic. It would no longer let two spellings claim one
/// file — that set folds ascii case since decision 52 — but it would still write `.PNG` into a tree
/// whose every other name is lower case. Pinned by
/// `a_shouted_extension_is_normalized_rather_than_carried_into_the_output_path`.
#[must_use]
pub(crate) fn output_extension(leg: Leg, media: &SourceMedia) -> Cow<'_, str> {
    if !needs_its_own_format(leg, media) {
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

/// `<stem>.<ext>`, with `_2`, `_3` and so on for a name something already claimed — an earlier
/// position in this plan, or, since decision 52, a path a MANIFEST ROW records. [`Outputs`] is what
/// decides which, and after that change the second is as ordinary a reason as the first.
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

/// The memories tree's directory for one capture: `<root>/YYYY/MM`.
fn output_dir(root: &Path, local: NaiveDateTime) -> PathBuf {
    root.join(local.format("%Y").to_string()).join(local.format("%m").to_string())
}

/// Ascii-case-insensitive membership, matching how the rest of the export layer reads an
/// extension.
fn matches(extension: &str, known: &[&str]) -> bool {
    known.iter().any(|candidate| extension.eq_ignore_ascii_case(candidate))
}

// ---- where the last run's outputs actually landed ----

/// Where each item's output has actually been landing, read back out of the manifest.
///
/// [`Outputs`] mints one path per item and breaks a collision between two of them with an ordinal
/// that is a position in this run's plan. That is stable against nothing: an item leaving the export
/// takes its ordinal with it, so every later item on that name slides down one and a survivor is
/// planned onto a path a FINISHED row still claims — measured run by run, the run then writes over a
/// repaired file, the departed row is demoted and retired, and its output is gone. Decision 52
/// closes it the way queue task 40 closed the same shape one layer up at the directory: the run
/// keeps the path each row's own record already names rather than re-deriving one from this run's
/// item list every time.
///
/// **One map, where the directory layer needs two, and the difference is the join.**
/// [`super::chat_fix::RecordedDirs`] carries an attributed half and an unattributed one because a
/// CONVERSATION is a join over rows: a row's directory says nothing about which conversation owns
/// it, so the two questions ("what is this conversation's directory" and "what names does the tree
/// already contain") need different sets. An item IS a row. The identity a plan looks a path up
/// under is the identity the manifest stores it against, so adoption and reservation are two
/// readings of one map: a record whose item this run plans is handed back, and a record whose item
/// it does not plan is never looked up while its path stays claimed, which IS the reservation.
///
/// **Every row carrying an output record seeds, whatever status it carries**, for the reason
/// [`super::chat_fix::RecordedDirs`] states at its own read: the manifest's output-record rule is
/// that the three output columns survive a transition into a parked status and are cleared by the
/// work ones, so `output_path.is_some()` already asks "a run finished this row and nothing has
/// driven it back to work". Naming a status list here would be a second spelling of that rule, free
/// to drift from it the next time a status is added.
#[derive(Debug, Default)]
pub struct RecordedOutputs {
    /// The output path each row of one kind records, keyed by that row's own source id.
    recorded: BTreeMap<String, PathBuf>,
}

impl RecordedOutputs {
    /// Reads what `manifest` records for every row of `kind`.
    ///
    /// One whole-kind [`Manifest::items`] read rather than a point query per item, for the reason
    /// [`super::chat_media::Reconciliation::enroll`] gives about its own: a real export would make
    /// that 9001 point queries.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when the manifest read fails.
    pub fn read(manifest: &Manifest, kind: ItemKind) -> Result<Self, ManifestError> {
        Ok(Self::of(&manifest.items(kind)?))
    }

    /// [`Self::read`] over rows a caller has already read.
    ///
    /// The chat leg reads its whole kind once for [`super::chat_fix::RecordedDirs`] and builds this
    /// from the same rows, so the two layers cost one query between them rather than one each.
    pub(crate) fn of(rows: &[Item]) -> Self {
        Self { recorded: rows.iter().filter_map(|row| Some((row.source_id.clone(), row.output_path.clone()?))).collect() }
    }
}

mod claimed {
    //! The append-only set the ordinal walk's soundness rests on, held by the compiler.
    //!
    //! Same property, same instrument and the same concession as `chat_fix`'s `issued` module one
    //! layer up, whose doc states the argument in full and is the place to read it from:
    //! [`super::Outputs::path`] starts its walk from a hint and climbs, which skips every candidate
    //! below the hint, and that is sound only because a claimed path can never be released. What is
    //! here is that shape over a whole path instead of over one directory name.

    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::path::Path;

    /// Paths already handed out, compared ascii-case-insensitively.
    ///
    /// **Folded, for the reason the directory layer folds a name**: `20210304_143005.JPG` and
    /// `20210304_143005.jpg` are one file on APFS and NTFS, and decision 11 is cross-platform. What
    /// this build DERIVES cannot produce that pair on its own — a stem is digits and an underscore,
    /// and [`super::output_extension`] lower-cases what it answers with — but half of what is
    /// claimed here comes off the MANIFEST, which outlives the build that filled it: a row written
    /// before task 45 carries the source file's own `.PNG` spelling, and an adopted conversation
    /// directory keeps whatever case its key had. So the pair is reachable from the store, and a
    /// case-sensitive set would hand a derived `.png` the path a recorded `.PNG` already names.
    /// Folding costs a suffix where two spellings really are two files and prevents an overwrite
    /// where they are one, which is the direction this whole type exists to fail in.
    ///
    /// The WHOLE path folds rather than its last component, because a filesystem that folds case
    /// folds every component of a path and not only the leaf.
    #[derive(Debug, Default)]
    pub(super) struct ClaimedPaths(BTreeSet<OsString>);

    impl ClaimedPaths {
        /// Records `path` as taken, answering whether it was still free.
        ///
        /// The only method, deliberately: every operation this type does not expose is one the walk
        /// cannot be broken by.
        pub(super) fn claim(&mut self, path: &Path) -> bool {
            self.0.insert(path.as_os_str().to_ascii_lowercase())
        }
    }
}

/// One output path per item: the one the manifest already records for it, or the first name in this
/// plan nothing has claimed.
///
/// Both planners drive this. They disagree about which directory an item lands in and about how the
/// stem is worked out, and not at all about what a collision is or how a broken one is spelled — the
/// same split that already puts [`output_name`] here rather than in either of them.
///
/// **[`Self::adopt`] runs before a single path is derived**, which is the whole of what the ordering
/// has to get right: a record is only worth keeping if nothing can be derived on top of it first.
pub(crate) struct Outputs {
    /// The root this run writes under. The whole of what makes a path off the manifest safe to hand
    /// back — see [`under`].
    root: PathBuf,
    /// The next ordinal to try for a folded base path, so a long collision run costs one lookup
    /// rather than a scan. Not decoration: the `_no-conversation` bucket puts 6413 items in one
    /// directory and every one of them falling through to the filename day wants one name, so a
    /// per-item scan of the claimed set is quadratic in exactly the case decision 52 is about.
    next: BTreeMap<OsString, u32>,
    /// Every path handed out. **The authority**, and not the same question as [`Self::next`]: a
    /// recorded path can spell `<stem>_2.<ext>` verbatim, and only a set of the paths actually
    /// issued can see that a derived `_2` would land on it. [`Self::next`] is a starting hint.
    used: claimed::ClaimedPaths,
    /// The path each item is handed back, keyed by source id.
    assigned: BTreeMap<String, PathBuf>,
}

impl Outputs {
    pub(crate) fn new(root: PathBuf, recorded: &RecordedOutputs) -> Self {
        let mut outputs = Self { root, next: BTreeMap::new(), used: claimed::ClaimedPaths::default(), assigned: BTreeMap::new() };
        outputs.adopt(recorded);
        outputs
    }

    /// Takes back the path each row's own record names, and claims it either way.
    ///
    /// **Adoption and reservation are one pass here**, and that is a consequence of a row being an
    /// item rather than a shortcut — see [`RecordedOutputs`] for why the directory layer needs two.
    /// A record this run plans an item for is handed back; a record for an item this run does not
    /// plan is claimed and never looked up, which is what keeps a departed item's finished file from
    /// being planned over. Both come out of the one claim below.
    ///
    /// **A record that LOSES the claim is not adopted and needs no reservation of its own**: some
    /// other row already holds that path, so it is reserved by whoever won it. Two rows recorded on
    /// one path is not hypothetical — it is the state the defect this closes actually produces — and
    /// the loser deriving a fresh name is the outcome that stops one of the two files being written
    /// over on every run from then on. The winner is the lowest source id, which is a function of
    /// the SET of rows rather than of which item the plan reaches first.
    ///
    /// **A record outside this run's root neither adopts nor reserves.** See [`under`].
    fn adopt(&mut self, recorded: &RecordedOutputs) {
        for (source_id, output) in &recorded.recorded {
            if under(&self.root, output) && self.used.claim(output) {
                self.assigned.insert(source_id.clone(), output.clone());
            }
        }
    }

    /// Where `source_id`'s output lands, given the directory and name its planner worked out.
    ///
    /// **An adopted path is only handed back inside the directory this run planned for the item**,
    /// and that subordination is the whole of what keeps this layer from overruling the one above
    /// it. A record whose parent has moved — a conversation that adopted a different directory, a
    /// memory this run dates into another month — is left claimed and not returned, so the item
    /// derives a fresh name where it now belongs. Two things would break without it: decision 44a's
    /// grouping, since one conversation's items would sit in two folders; and decision 46c, since
    /// [`Originals::dir`] is built from `dir` while the merged file would have come back from
    /// somewhere else, splitting a pair the mode exists to keep together.
    ///
    /// The parent equality is also what makes the returned path containment-safe on its own: `dir`
    /// is the planner's own join onto the output root, so a record equal to `dir` plus one component
    /// cannot leave the tree. `child_name` states the same property one layer up.
    pub(crate) fn path(&mut self, source_id: &str, dir: &Path, stem: &str, extension: &str) -> PathBuf {
        if let Some(output) = self.assigned.get(source_id).filter(|output| output.parent() == Some(dir)) {
            return output.clone();
        }
        let folded = dir.join(output_name(stem, extension, 0)).as_os_str().to_ascii_lowercase();
        let mut ordinal = self.next.get(&folded).copied().unwrap_or_default();
        // Terminates: every iteration raises the ordinal, the names it spells are all distinct, and
        // `used` is finite, so a free one is reached in at most `used.len() + 1` steps.
        let output = loop {
            let candidate = dir.join(output_name(stem, extension, ordinal));
            ordinal += 1;
            if self.used.claim(&candidate) {
                break candidate;
            }
        };
        self.next.insert(folded, ordinal);
        output
    }
}

/// Whether `output` is a path under `root` that this run could have written itself.
///
/// What this gates is the RESERVATION: the claim set is meant to hold the paths this run could
/// otherwise derive, and a record naming a file somewhere else is not one of them. Its ordinal is a
/// suffix for a collision this root does not have, so claiming it would cost a real item a suffix
/// for a name nothing here holds. `a_path_recorded_under_another_out_root_is_neither_adopted_nor_reserved`
/// pins that, on the arm where refusing is right.
///
/// **It compares SPELLINGS, not directories, and that is a known hole rather than a property.** Any
/// respelling of one directory between two runs makes the whole of decision 52 inert for the second
/// one: every recorded path is refused here, so `adopt` assigns nothing and claims nothing, and an
/// item leaving the export shifts a survivor onto a finished row's file exactly as it did before the
/// decision existed. The failure is silent — nothing reports a seed that matched zero rows. Both
/// spellings are user-typed, through `--out=<dir>` or through the source path
/// [`default_out_root`] inherits, so this needs no hand-edited store to reach.
///
/// Measured, `rustc -O`, edition 2024:
///
/// | root | recorded output | `under` |
/// |---|---|---|
/// | `/out` | `/out/2021/01/a.jpg` | true |
/// | `/out` | `/out2/2021/01/a.jpg` | false (component-wise, so this one is right) |
/// | `/out/` | `/out/2021/01/a.jpg` | true (a trailing slash is the one thing it does normalize) |
/// | `out` | `/home/u/a/out/2021/01/a.jpg` | **false** — relative against absolute, every filesystem |
/// | `./out` | `out/2021/01/a.jpg` | **false** — every filesystem |
/// | `/Out` | `/out/2021/01/a.jpg` | **false** — right on ext4, wrong on APFS and NTFS |
///
/// **Case is one instance of that class and not the class**, which is worth keeping straight because
/// the fixture below builds the case pair. It builds that one because the claim set's own ascii fold
/// is what makes the item layer's answer observable there — [`claimed::ClaimedPaths`] folds one
/// function down, so on a folding filesystem these two decide the same question opposite ways. The
/// relative-vs-absolute rows need no folding filesystem at all and are the wider half of the
/// residual. `a_path_recorded_under_another_out_root_is_neither_adopted_nor_reserved` pins the arm
/// where refusing is RIGHT and bounds none of this.
///
/// **Left standing deliberately, and it is not this function's call to make.**
/// [`super::chat_fix`]'s `child_name` compares spellings the same way for the directory layer, so
/// this matches task 40's behaviour rather than diverging from it, and closing it is one ruling for
/// both layers. Recorded here so the next reader inherits the hole rather than the impression of a
/// guarantee.
///
/// An empty root makes this true for every path on earth, and no shipped surface can produce one:
/// `main.rs` rejects both a valueless `--out` and a bare `--out=` with a hard error
/// (`an_empty_out_value_is_a_hard_error`), and every other producer is [`default_out_root`], which
/// joins [`OUT_DIR`] onto the source and so is non-empty even for an empty source. A library caller
/// is the only route, and it would be inert anyway: a derived path under an empty root is relative
/// and can never fold onto an absolute record.
///
/// **This gates the claim set and nothing else.** Adoption is gated separately and more tightly, on
/// the record's parent being the directory the planner worked out for that item
/// ([`Outputs::path`]), and that is what makes a returned path containment-safe. So nothing here has
/// to out-parse a hostile record: `Path::starts_with` is component-wise and admits `..`, and a
/// `<root>/../elsewhere/x.jpg` a hand-edited store could hold names no file this run competes for
/// and is never handed to anybody. Refusing it as well would be a check with no reachable effect,
/// which this repo's own rules would rather not have than have.
fn under(root: &Path, output: &Path) -> bool {
    output.starts_with(root)
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
    /// The raw [`FixError`] message, verbatim.
    ///
    /// **The redaction is the MANIFEST's, and it applies to the manifest's copy alone.** The same
    /// message also goes to `last_error`, where `mark_failed` reduces it to prose tokens on the way
    /// in; this field is not that copy and is not filtered. Saying only "where it is redacted on the
    /// way in" read as covering the field, which is the shape where conceding a limitation launders
    /// the wider case — the concession has to state which side it covers.
    ///
    /// It matters on the chat-media leg specifically: [`FixError::Create`], [`FixError::Touch`] and
    /// [`FixError::Copy`] all name a path under `<out_root>/chat/<cleaned conversation key>/`, and a
    /// conversation key is a friend's username. So this string can hold one, and any consumer that
    /// renders it is a disclosure surface. The screens are safe because
    /// [`crate::tui::alert::RunAlert::completion`] reads `failed.len()` and never a reason — a
    /// deliberate boundary, pinned by `a_failing_chat_item_keeps_its_conversation_out_of_the_alert`
    /// in `tests/chat_media_screen.rs` rather than left to whoever edits the copy next.
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
    /// It came out in its own format instead of as a JPEG, so it carries no capture metadata at all
    /// — decision 47 for a main with nothing to composite, task 45 for one composited under an
    /// overlay. The date still reaches the file's own modification time; the sender and the
    /// conversation reach nothing.
    ///
    /// **One variant for both shapes, and that was a choice.** The two differ in how the bytes were
    /// produced and in nothing a user can act on: same missing metadata, same date on the file
    /// itself, same reason. A notice exists to say what someone got rather than which branch made
    /// it, so a second variant would put an extra arm in every exhaustive match to carry a
    /// distinction that never reaches a screen.
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
                "kept in its own format, which is what preserves any transparency it carries, so the capture date reached \
                 only the file's own date and the sender and conversation reached nothing: this build writes that metadata \
                 into JPEG alone",
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
    // `exclude` leaves an already-excluded row's timestamp alone — and a no-op loop on the
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

/// Copies the export's own files beside the output, decision 44b's `both` and `originals` overlay
/// modes.
///
/// Verbatim, under the names the export gave them, which is the point of keeping them at all: the
/// output is the repaired file and these are the bytes it was made from, so putting them under the
/// output's `YYYYMMDD_HHMMSS` shape would leave the run with two files claiming to be the same
/// thing. They get the output's modification time so a browser sorts the set together, and nothing
/// else about them is touched — no composite, no stamp.
///
/// A no-op unless the item carries an [`Originals`], which a planner mints only where a composite
/// had two files to consume: with no overlay there is no un-merged version to lose. Decision 47
/// closed the case that used to sit here as a residual — a lone PNG is no longer re-encoded at all,
/// so its own bytes ARE the output and there is nothing left to keep a copy of.
///
/// **This reads [`Originals::overlay`] and never [`SourceMedia::overlay`]**, which is what lets the
/// `originals` mode hand the fix pass a main alone while still keeping the pair.
fn keep_originals(item: &PlannedItem) -> Result<(), FixError> {
    let Some(Originals { dir, overlay }) = &item.originals else {
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

/// The pure-Rust leg: composite the overlay in, stamp the EXIF, write a JPEG — or, for a main whose
/// own format CAN carry an alpha channel, write that format and stamp nothing.
///
/// The output directory is made last, right before the write, so a failure earlier leaves no empty
/// year and month behind. The video leg cannot do the same, since ffmpeg needs somewhere to mux
/// into before there is anything to write.
///
/// Returns what the item was finished without. See [`Notice`].
fn fix_image(item: &PlannedItem) -> Result<Vec<Notice>, FixError> {
    // Decision 47 and task 45: the main's own transparency survives, whether or not a caption is
    // drawn over it. `image`'s flatten DISCARDS the alpha channel rather than compositing it, so
    // either shape would otherwise land as whatever RGB sat under `alpha = 0`.
    //
    // **The verbatim question is answered HERE, by the `Option` and not by a predicate.** A lone
    // main is copied byte for byte, so it keeps its own format whatever that format is; a paired one
    // is composited and re-encoded losslessly, and `compose_png` hardcodes PNG. So the bytes agree
    // with the extension the plan committed to only while `ALPHA_CAPABLE_EXTENSIONS` holds `png`
    // alone — that constant carries the cap and the one edit that lifts it. What the `Option` buys
    // unconditionally is different and smaller: the verbatim answer cannot DRIFT from the
    // format answer, because only one of the two is a predicate.
    //
    // **No metadata call on either shape, deliberately.** No `Jpeg` value is constructed in this
    // block, so nothing here reaches `little_exif` at all. That is the whole of what this branch
    // contributes, and it is a property of one function body that a reader can check. What makes
    // RUSTSEC-2026-0194 unreachable is a separate and stronger argument with a compiler half and a
    // convention half that must not be run together — `exif.rs`'s `library` module doc states both
    // and is the only place either should be read from. Read it before adding anything here.
    if needs_its_own_format(item.leg, &item.media) {
        let bytes = match item.media.overlay.as_deref() {
            Some(overlay) => overlay::compose_png(&item.media.main, overlay)?,
            None => fs::read(&item.media.main).map_err(|source| FixError::Read { path: item.media.main.clone(), source })?,
        };
        make_parent(&item.output)?;
        fs::write(&item.output, &bytes).map_err(|source| FixError::Create { path: item.output.clone(), source })?;
        return Ok(vec![Notice::NotStamped]);
    }

    // Matched on the overlay itself rather than on a predicate, so the composite arm can only be
    // reached with an overlay in hand — which is what let `overlay::composite` drop its `Option`.
    let bytes = match item.media.overlay.as_deref() {
        Some(overlay) => overlay::compose_jpeg(&item.media.main, overlay)?,
        // No overlay, so nothing is composited and re-encoding would spend a generation of lossy
        // compression for nothing; the copy is also what keeps whatever EXIF the source carried
        // around for `stamp` to read and preserve.
        //
        // Everything reaching this arm is already a JPEG, which is why there is no extension check:
        // `Leg::of` admits `jpg`, `jpeg` and `png` alone, and `needs_its_own_format` took `png`
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

    use super::{Capture, Leg, Outputs, RecordedOutputs, TimeSource, VideoOptions, output_dir, output_name};

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day).unwrap().and_hms_opt(hour, minute, second).unwrap()
    }

    /// The path the memories planner lands on, through the two functions it now builds one from.
    fn output_path(root: &str, local: chrono::NaiveDateTime, stem: &str, extension: &str, ordinal: u32) -> std::path::PathBuf {
        output_dir(Path::new(root), local).join(output_name(stem, extension, ordinal))
    }

    #[test]
    fn an_output_path_is_year_month_and_the_local_wall_time() {
        let local = at(2021, 1, 15, 14, 30, 5);
        assert_eq!(output_path("/out", local, "20210115_143005", Leg::Image.extension(), 0), Path::new("/out/2021/01/20210115_143005.jpg"));
        // Same tree, same name, and the extension is the only thing the leg moves.
        assert_eq!(output_path("/out", local, "20210115_143005", Leg::Video.extension(), 0), Path::new("/out/2021/01/20210115_143005.mp4"));
    }

    #[test]
    fn a_name_a_previous_item_already_claimed_gets_a_counted_suffix() {
        let local = at(2021, 1, 15, 0, 0, 0);
        // The suffix counts from two, so the first file of a colliding set keeps the plain name and
        // nobody has to work out that `_1` means "the second one".
        let named = |ordinal| output_path("/out", local, "20210115_000000", Leg::Image.extension(), ordinal);
        assert_eq!(named(1), Path::new("/out/2021/01/20210115_000000_2.jpg"));
        assert_eq!(named(2), Path::new("/out/2021/01/20210115_000000_3.jpg"));
        assert_eq!(
            output_path("/out", local, "20210115_000000", Leg::Video.extension(), 1),
            Path::new("/out/2021/01/20210115_000000_2.mp4")
        );
    }

    // ---- the claim set decision 52 put under both planners ----

    /// What [`RecordedOutputs::read`] builds, without a manifest to build it out of.
    fn recorded(rows: &[(&str, &str)]) -> RecordedOutputs {
        RecordedOutputs { recorded: rows.iter().map(|(source_id, output)| ((*source_id).to_owned(), (*output).into())).collect() }
    }

    /// Where each of `source_ids` lands, planned into one directory on one second under whatever
    /// `recorded` already holds — the collision the whole type is about.
    fn planned(recorded: &RecordedOutputs, source_ids: &[&str]) -> Vec<std::path::PathBuf> {
        let mut outputs = Outputs::new("/out".into(), recorded);
        source_ids.iter().map(|source_id| outputs.path(source_id, Path::new("/out/2021/01"), "20210115_000000", "jpg")).collect()
    }

    #[test]
    fn a_first_run_hands_out_positions_in_the_plan() {
        assert_eq!(
            planned(&RecordedOutputs::default(), &["a", "b", "c"]),
            [
                Path::new("/out/2021/01/20210115_000000.jpg"),
                Path::new("/out/2021/01/20210115_000000_2.jpg"),
                Path::new("/out/2021/01/20210115_000000_3.jpg"),
            ]
        );
    }

    /// The adoption half: a row that still records a path gets that exact path back, whatever
    /// position it now holds in the plan.
    ///
    /// The fixture puts the second item's record on the suffixed name and drops the first item from
    /// the plan entirely, which is the departure that used to shift it. All three rules answer
    /// differently on this one input — re-deriving gives the plain name, reserving without adopting
    /// gives `_3` because the row's own record is claimed too, and adopting gives `_2`.
    #[test]
    fn a_recorded_path_is_handed_back_to_the_item_that_recorded_it() {
        let kept = recorded(&[("a", "/out/2021/01/20210115_000000.jpg"), ("b", "/out/2021/01/20210115_000000_2.jpg")]);
        assert_eq!(planned(&kept, &["b"]), [Path::new("/out/2021/01/20210115_000000_2.jpg")]);
    }

    /// The reservation half, and the one the measured defect actually turns on: the survivor was
    /// driven back to work, which per decision 50 CLEARED its record, so adoption has nothing to
    /// give it and only the departed row's reservation keeps it off that row's file.
    #[test]
    fn a_departed_items_recorded_path_is_not_handed_to_another_item() {
        let left_behind = recorded(&[("a", "/out/2021/01/20210115_000000.jpg")]);
        assert_eq!(planned(&left_behind, &["b"]), [Path::new("/out/2021/01/20210115_000000_2.jpg")]);
    }

    /// Two rows recorded on one path is the state the defect this closes actually produces, so it is
    /// not a forged fixture: the second of them has to derive a fresh name rather than be handed a
    /// file the first one already claims.
    #[test]
    fn two_rows_recorded_on_one_path_do_not_both_adopt_it() {
        let doubled = recorded(&[("a", "/out/2021/01/20210115_000000.jpg"), ("b", "/out/2021/01/20210115_000000.jpg")]);
        assert_eq!(
            planned(&doubled, &["a", "b"]),
            [Path::new("/out/2021/01/20210115_000000.jpg"), Path::new("/out/2021/01/20210115_000000_2.jpg")]
        );
    }

    /// A record under another out root names a file this run is not writing, so its ordinal is a
    /// suffix for a collision this root does not have.
    ///
    /// **The fixture spells the other root as a case variant of this one on purpose.** The claim set
    /// folds ascii case, so an unfiltered reservation would treat `/OUT/...` as the path `/out/...`
    /// this run is about to derive and cost the item a suffix for a file that, on a case-sensitive
    /// filesystem, is a different tree.
    ///
    /// **That is the arm where refusing is RIGHT, and it is the only arm this pins.** Swap which
    /// side is shouted and the same shape is a hole on a filesystem that folds; respell the root
    /// relative-against-absolute and it is a hole on every filesystem. [`under`]'s own doc carries
    /// the measured table. Nothing here bounds any of that, and reading this test as though it did
    /// is the mistake its previous wording invited.
    ///
    /// The second half is the control — without it, a seed that reserved nothing at all reads green.
    #[test]
    fn a_path_recorded_under_another_out_root_is_neither_adopted_nor_reserved() {
        let elsewhere = recorded(&[("a", "/OUT/2021/01/20210115_000000.jpg")]);
        assert_eq!(planned(&elsewhere, &["b"]), [Path::new("/out/2021/01/20210115_000000.jpg")]);
        let here = recorded(&[("a", "/out/2021/01/20210115_000000.jpg")]);
        assert_eq!(planned(&here, &["b"]), [Path::new("/out/2021/01/20210115_000000_2.jpg")]);
    }

    /// A record whose parent is not the directory this run planned for the item is left claimed and
    /// not handed back, so the item derives where it now belongs.
    ///
    /// This is what keeps the item layer under the one above it: `chat_fix` decides a conversation's
    /// directory off the manifest too, and an item returning a path in the directory that run
    /// DIDN'T pick would split one conversation across two folders and put decision 46c's
    /// `originals/` beside a file that is not there. The old path stays claimed, because the file it
    /// names is still on disk.
    #[test]
    fn a_record_in_a_directory_this_run_did_not_plan_is_not_handed_back() {
        let moved = recorded(&[("a", "/out/2020/12/20210115_000000.jpg")]);
        assert_eq!(planned(&moved, &["a"]), [Path::new("/out/2021/01/20210115_000000.jpg")]);
    }

    /// Case-sensitivity is a property of the filesystem, not of the string: a recorded `.JPG` and a
    /// derived `.jpg` are one file on APFS and NTFS, and decision 11 is cross-platform. The recorded
    /// spelling is reachable from the STORE rather than from this build, which is why the fixture is
    /// a record and not a second derived name.
    #[test]
    fn a_recorded_path_differing_only_in_case_still_reserves_its_file() {
        let shouted = recorded(&[("a", "/out/2021/01/20210115_000000.JPG")]);
        assert_eq!(planned(&shouted, &["b"]), [Path::new("/out/2021/01/20210115_000000_2.jpg")]);
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
