//! Capture metadata written into an MP4, and the guard type that keeps `mp4ameta` on the one path
//! it is safe on.
//!
//! Video metadata is split across two mechanisms in one container, and both live here. `mp4ameta`
//! cannot reach the header times **by design** (its `Mvhd` reads `creation_time` into a discarded
//! buffer), and nothing but `mp4ameta` should be hand-rolling the `ilst` write, whose growth
//! cascades through `stco`/`co64`. So:
//!
//! 1. **The `mvhd`/`tkhd`/`mdhd` times are a hand-rolled fixed-size patch.** Those fields never
//!    change an atom's length, so `mdat` never moves and no chunk-offset table is touched. Measured
//!    byte-identical to ffmpeg's own write of the same instant.
//! 2. **The coordinate and the local date go through `mp4ameta`'s `ilst`**, which splices and
//!    fixes up every offset behind it.
//!
//! Everything ffmpeg does to a video is pixels. **No metadata this crate writes goes through
//! ffmpeg**, so there is one metadata code path whatever the run did to the frames.
//!
//! # The two things this module makes structural
//!
//! Both are settled in `docs/design.md`'s **Metadata write notes**, with crate internals in
//! `docs/domain-knowledge.md`. Neither is re-derived here, and neither is left to a comment:
//!
//! - **Never a bare `write_to_path`.** Its `WriteConfig::DEFAULT` turns on `write_chapter_list` and
//!   `write_chapter_track` alongside the meta items, and on input carrying a chapter track those
//!   legs rewrite `mdat` and the sample tables on a metadata-only write. The safe config is
//!   `WriteConfig { write_meta_items: true, ..WriteConfig::NONE }`.
//! - **Never the chapter API.** Same reason, reached from the other side.
//!
//! The private [`library`] module is what carries both: it owns the only two calls into the crate,
//! bakes the safe configs into consts, and returns bytes rather than a `Tag`, so no `Tag` or
//! `Userdata` value exists anywhere else in this crate for a caller to reach `chapter_list_mut` or
//! `write_to_path` on. Stated the way [`crate::export::exif`]'s equivalent is stated, because the
//! honest form of the guarantee is narrower than "unreachable": a future edit can `use
//! mp4ameta::Tag` in a new module and do as it likes. What is closed is that **no existing call
//! site can be made to turn on a chapter leg**, since none of them takes a config.
//!
//! # Writing the whole file at once
//!
//! [`Mp4`] owns the bytes and [`Mp4::write`] is one `fs::write` of a finished buffer. A failed
//! patch therefore leaves no half-written file, and that property is **structural rather than
//! guarded**: it holds only because the patch mutates an in-memory `Vec` and the write runs on
//! `Ok`. Nothing observes the absence of a file that was never opened, so a refactor that streams
//! patches at a file would lose it in silence. Four tests stand between that and a corrupted
//! archive, and they are worth naming because none of them reads as being about this on its own:
//!
//! - `a_capture_before_1970_is_refused_and_changes_nothing` (`tests/video.rs`) — the buffer and the
//!   source file both come through a refused stamp untouched.
//! - `a_chunk_table_the_tagging_crate_refuses_errors_cleanly_and_changes_nothing` (`tests/video.rs`)
//!   — the tag write itself failing, not a refused date, leaves the buffer untouched; the first
//!   test to pin the all-or-nothing property at the `mp4ameta` step.
//! - `a_write_that_shrinks_leaves_no_tail_of_whatever_was_at_the_output_path` (`tests/video.rs`) —
//!   the write truncates, so a smaller output cannot leave an older, larger one's tail behind.
//! - `a_video_whose_date_cannot_be_stored_leaves_the_output_path_alone` (`tests/local_fix.rs`) —
//!   the same failure driven through the whole pass, with something already sitting where the
//!   output would go.

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset, NaiveDateTime, SecondsFormat, TimeZone, Utc};

use crate::export::model::LocationPoint;

mod library {
    //! The entire surface this crate has on `mp4ameta`, and the boundary that keeps the chapter
    //! legs out of reach.
    //!
    //! Two functions, neither taking a config, and no `Tag` or `Userdata` crossing the boundary in
    //! either direction. The configs are consts here, so there is no call site anywhere in the
    //! crate that could pass a different one — not conditionally, not behind an alias.
    //!
    //! The residual, said plainly rather than talked around: a new module can `use mp4ameta::Tag`
    //! and call `write_to_path` with no import from here at all, and it compiles. That is new code
    //! in a diff, which is the protection; the compiler is not.

    use std::io::Cursor;

    use mp4ameta::{Data, Fourcc, ReadConfig, Tag, WriteConfig};

    /// Read the metadata item list and nothing else. Chapter reads are off because the write below
    /// is not allowed to touch chapters, and reading what cannot be written is wasted I/O on a file
    /// that may carry a chapter track.
    const READ: ReadConfig = ReadConfig { read_meta_items: true, ..ReadConfig::NONE };

    /// The one safe write configuration. `WriteConfig::DEFAULT` would additionally set
    /// `write_chapter_list` and `write_chapter_track`, which rewrite `mdat` and the sample tables
    /// on chapter-track input.
    const WRITE: WriteConfig = WriteConfig { write_meta_items: true, ..WriteConfig::NONE };

    /// Sets every `(fourcc, text)` pair on `bytes`' metadata item list, keeping whatever else the
    /// list already held, and hands back the spliced file.
    ///
    /// Read-modify-write: the existing list is decoded first, so a tag this build never writes
    /// survives. Everything happens in one owned `Vec`, which is what keeps the caller's
    /// all-or-nothing write property intact.
    pub(super) fn set_text(bytes: Vec<u8>, tags: &[([u8; 4], String)]) -> Result<Vec<u8>, mp4ameta::Error> {
        let mut tag = Tag::read_with(&mut Cursor::new(&bytes[..]), &READ)?;
        for (fourcc, text) in tags {
            tag.userdata.set_data(Fourcc(*fourcc), Data::Utf8(text.clone()));
        }
        let mut file = Cursor::new(bytes);
        tag.userdata.write_with(&mut file, &WRITE)?;
        Ok(file.into_inner())
    }
}

/// Seconds between the MP4 epoch (1904-01-01 UTC) and the unix epoch.
///
/// It doubles as the boundary both readers this project checks against use as a heuristic, which is
/// why it is also the floor on what gets written and the floor on what gets read back. See
/// [`Mp4::embedded_time`] and [`TimeRange`].
const MP4_EPOCH_OFFSET: u64 = 2_082_844_800;

/// The `ilst` tag holding an ISO-6709 coordinate. exiftool renders it `GPSCoordinates` and
/// resolves `Composite:GPSPosition` from it; ffprobe calls it `location`.
const COORDINATE: [u8; 4] = *b"\xa9xyz";

/// The `ilst` tag holding the creation date as a string. Unlike the header fields it keeps a UTC
/// offset, so it is where the local wall clock survives.
const CONTENT_DATE: [u8; 4] = *b"\xa9day";

/// Atoms this build descends into looking for header times.
const CONTAINERS: [[u8; 4]; 4] = [*b"moov", *b"trak", *b"mdia", *b"udta"];

/// Atoms carrying a version byte followed by creation and modification times.
///
/// All three spell that prefix identically — a four-byte version-and-flags word, then the two times
/// — so one patch shape reaches every one of them.
const TIMED: [[u8; 4]; 3] = [*b"mvhd", *b"tkhd", *b"mdhd"];

// ---- what gets written ----

/// Everything a run knows about one video, ready to go into its container metadata.
///
/// Shaped like [`crate::export::exif::Stamp`] on purpose: the two legs derive their values the same
/// way and a reader moving between them should not have to re-learn the field meanings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoStamp {
    /// Local wall-clock time where the memory was taken.
    pub local: NaiveDateTime,
    /// The offset [`Self::local`] is at.
    ///
    /// `None` means the run could not work it out, and then [`CONTENT_DATE`] is written **with no
    /// zone designator** rather than a `+00:00` that would claim the wall time is UTC. The header
    /// fields have no offset to omit — MP4 defines them as UTC — so they get the wall time read as
    /// UTC, which is the same fallback the file's own modification time takes.
    pub offset: Option<FixedOffset>,
    /// `None` when the run has no coordinate, or when the pairing that would supply one is too
    /// arbitrary to stamp from. Deciding that is the caller's job.
    pub location: Option<LocationPoint>,
}

impl VideoStamp {
    /// The instant the header fields carry.
    fn instant(&self) -> DateTime<Utc> {
        match self.offset {
            Some(offset) => offset.from_local_datetime(&self.local).earliest().map_or_else(|| self.local.and_utc(), |at| at.to_utc()),
            None => self.local.and_utc(),
        }
    }

    /// The `©day` string: ISO-8601, with the offset only when one is known.
    fn content_date(&self) -> String {
        match self.offset {
            Some(offset) => offset
                .from_local_datetime(&self.local)
                .earliest()
                .map_or_else(|| self.local.format("%Y-%m-%dT%H:%M:%S").to_string(), |at| at.to_rfc3339_opts(SecondsFormat::Secs, false)),
            None => self.local.format("%Y-%m-%dT%H:%M:%S").to_string(),
        }
    }
}

/// The `moov/udta` child that decides a video's location for every reader that has one.
///
/// Both of these beat anything `mp4ameta` can write: with either present, exiftool's
/// `Composite:GPSPosition` and ffprobe both resolve to the `udta` value, and no Rust crate can
/// write a `udta` child. So a run that found one **skips the coordinate and says so** rather than
/// adding a shadowed duplicate. [`Self::Xyz`] is the worse of the two to write over: it does not
/// merely shadow, it makes the composite vanish and leaves a broken latitude row behind.
///
/// An arbitrary other `udta` child does not shadow. The `©eng` sentinel real memory videos carry is
/// categorically not a GPS source, which is what makes the pure-Rust write reachable on real data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocationAtom {
    /// ffmpeg's spelling, a structured record with latitude, longitude and altitude.
    Loci,
    /// The same fourcc `mp4ameta` writes into `ilst`, but sitting directly under `udta`, where it
    /// takes precedence.
    Xyz,
}

impl fmt::Display for LocationAtom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Loci => "loci",
            Self::Xyz => "\u{a9}xyz",
        })
    }
}

// ---- the guard type ----

/// An MP4 held in memory: the only form `mp4ameta` is ever handed anywhere in this crate.
///
/// See the module docs for what the type is guarding and why a comment could not.
#[derive(Clone, PartialEq, Eq)]
pub struct Mp4(Vec<u8>);

impl fmt::Debug for Mp4 {
    /// Hand-written because the derived form would print every byte of a video into whatever a
    /// `{:?}` lands in.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mp4").field("bytes", &self.0.len()).finish()
    }
}

impl Mp4 {
    /// Takes ownership of `bytes` if they are an MP4 this build can walk.
    ///
    /// Not a signature prefix test: the whole box chain is walked and the header times are located,
    /// which is the exact structure the patch below writes into. A prefix test admits a truncated
    /// file and defers the failure into a crate whose message names nothing useful.
    ///
    /// # Errors
    ///
    /// Returns [`NotMp4`] for anything else.
    ///
    /// # Examples
    ///
    /// ```
    /// use exportsnap::export::video::Mp4;
    ///
    /// assert!(Mp4::new(b"\x89PNG\r\n\x1a\n".to_vec()).is_err());
    /// // Opens with a file-type box, then claims one far longer than the buffer.
    /// assert!(Mp4::new(b"\xff\xff\xff\xffftypisom".to_vec()).is_err());
    /// ```
    pub fn new(bytes: Vec<u8>) -> Result<Self, NotMp4> {
        let layout = layout(&bytes)?;
        if layout.movie.is_none() {
            return Err(NotMp4::NoHeader);
        }
        // Refused here rather than left to the crate: `mp4ameta 0.13.0` divides by this on every
        // read, with no zero check, so a file carrying a zero timescale PANICS the process instead
        // of failing one item. The divisor is `atom/util.rs:157`, reached from `atom/mod.rs:352`,
        // because it happens before any of the read flags are consulted.
        if layout.movie_timescale == Some(0) {
            return Err(NotMp4::ZeroTimescale);
        }
        Ok(Self(bytes))
    }

    /// Reads `path` into memory and checks it is an MP4.
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::Read`] when the file cannot be read and [`VideoError::NotMp4`] when it
    /// is not one.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, VideoError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| VideoError::Read { path: path.to_path_buf(), source })?;
        Self::new(bytes).map_err(|source| VideoError::NotMp4 { path: path.to_path_buf(), source })
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Writes the bytes to `path`, replacing whatever was there.
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::Write`] when the file cannot be created or written.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), VideoError> {
        let path = path.as_ref();
        fs::write(path, &self.0).map_err(|source| VideoError::Write { path: path.to_path_buf(), source })
    }

    /// Which `udta` LOCATION atom the file carries, if it carries one.
    ///
    /// `None` is the answer that makes a coordinate write meaningful. See [`LocationAtom`].
    #[must_use]
    pub fn location_atom(&self) -> Option<LocationAtom> {
        layout(&self.0).ok().and_then(|layout| layout.location)
    }

    /// The creation time the file's `mvhd` already carries, when it carries an unambiguous one.
    ///
    /// **The whole band below [`MP4_EPOCH_OFFSET`] reads as `None`, deliberately.** MP4 stores
    /// seconds since 1904-01-01 UTC, but ffmpeg always — and exiftool by default — reads any raw
    /// value under that boundary as a unix timestamp instead. Nothing in the file says which
    /// convention its writer used, and the two readings are different instants, decades apart. So
    /// this reader refuses the whole ambiguous band rather than picking a side: a caller that gets
    /// `None` falls back to the day in the filename, which is a stated unknown, where a guess would
    /// be a coin flip wearing a timestamp's clothes.
    ///
    /// The boundary is exactly the floor the write path enforces, so anything this build wrote
    /// round-trips through here, and the never-written zero and exiftool's `0000:00:00` sentinel
    /// both fall on the refusing side.
    #[must_use]
    pub fn embedded_time(&self) -> Option<DateTime<Utc>> {
        let movie = layout(&self.0).ok()?.movie?;
        instant_of(read_time(&self.0, movie)?)
    }

    /// Writes `stamp` into the container, keeping every tag already there.
    ///
    /// Order matters and is not arbitrary: the fixed-size header patch runs **first**, while its
    /// byte offsets still describe the buffer, and the `ilst` splice — the one write that moves
    /// bytes — runs second.
    ///
    /// Both run against a copy, and `self` is replaced only once both have succeeded, so a stamp
    /// that fails at either step leaves the buffer exactly as it was. That is what lets a caller
    /// retry, and it is the buffer-level half of the whole-file property in the module docs.
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::Time`] when the instant does not fit the header fields, and
    /// [`VideoError::Tag`] when the metadata item list cannot be spliced.
    pub fn stamp(&mut self, stamp: &VideoStamp) -> Result<(), VideoError> {
        let layout = layout(&self.0).map_err(|source| VideoError::Structure { source })?;
        let instant = stamp.instant();
        let raw = raw_time(instant).map_err(|source| VideoError::Time { source })?;

        // Every field is checked before the first one is written, so a file mixing header versions
        // cannot end up with the ones that fit patched and the rest not.
        if let Some(field) = layout.times.iter().find(|field| !field.holds(raw)) {
            return Err(VideoError::Time { source: TimeRange::PastHeader { instant, version: field.version } });
        }
        let mut patched = self.0.clone();
        for field in &layout.times {
            field.write(&mut patched, raw);
        }

        let mut tags = vec![(CONTENT_DATE, stamp.content_date())];
        // A `udta` LOCATION atom outranks anything writable here, so on one of those the coordinate
        // is skipped entirely rather than written where it would be read past. The caller asks
        // `location_atom` first and reports it; this is the second belt on the same trousers.
        if let (Some(location), None) = (stamp.location, layout.location) {
            tags.push((COORDINATE, iso6709(location)));
        }
        self.0 = library::set_text(patched, &tags).map_err(|source| VideoError::Tag { source: Box::new(source) })?;
        Ok(())
    }
}

/// The creation time in `path`'s `mvhd`, without pulling the whole file into memory.
///
/// Seeks over the top-level boxes and reads only `moov`, which on a real memory video is tens of
/// kilobytes against a file of tens of megabytes. That matters because the plan reads this for
/// every video whose time falls back to the file, and the same files get read in full again when
/// the run reaches them.
///
/// Best-effort by design: an unreadable file, a broken box chain and an absent or ambiguous time
/// are all `None`, because every caller has a fallback and none wants a missing header to fail a
/// run. See [`Mp4::embedded_time`] for what "ambiguous" means here.
#[must_use]
pub fn header_time(path: impl AsRef<Path>) -> Option<DateTime<Utc>> {
    let mut file = File::open(path).ok()?;
    let end = file.seek(SeekFrom::End(0)).ok()?;
    let mut at = 0;
    while at < end {
        let head = read_head(&mut file, at, end)?;
        if head.fourcc == *b"moov" {
            let mut moov = vec![0; usize::try_from(head.size).ok()?.checked_sub(head.header)?];
            file.read_exact(&mut moov).ok()?;
            let mut layout = Layout::default();
            collect(&moov, 0, moov.len(), &mut layout).ok()?;
            return instant_of(read_time(&moov, layout.movie?)?);
        }
        at = at.checked_add(head.size)?;
        file.seek(SeekFrom::Start(at)).ok()?;
    }
    None
}

/// Why bytes were refused before they could reach `mp4ameta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotMp4 {
    /// The first box is not a file-type box, which every MP4 opens with and which `mp4ameta`'s own
    /// writer parses before anything else.
    ///
    /// Carries the fourcc it saw instead, which is container magic rather than media content, so
    /// the message can name what the file actually is without printing any of it.
    Signature {
        /// Up to the four bytes a fourcc occupies. Shorter when the buffer was.
        found: Vec<u8>,
    },
    /// The box chain does not hold together: a box declares a size running past its parent, or one
    /// too small to hold its own header.
    Structure,
    /// It walks, and carries no `moov/mvhd`, so there is no header time to correct and nothing
    /// downstream would have a movie to splice into.
    NoHeader,
    /// Its movie header declares a timescale of zero, which is not a duration this build can leave
    /// to the tagging crate: that crate divides by it unconditionally and takes the whole process
    /// down rather than failing the file.
    ZeroTimescale,
}

impl NotMp4 {
    /// What is wrong with the bytes, carrying **no advice about what to do**.
    ///
    /// Split from [`Display`](fmt::Display) because the same broken bytes want opposite advice
    /// depending on where they came from. A source file that will not walk is a memory in the wrong
    /// container and the answer is to convert it; the identical failure on a file ffmpeg *just
    /// produced* means the memory was fine and the re-encode broke it, where "convert it first" is
    /// precisely backwards. A caller in the second position quotes this and supplies its own
    /// ending — see [`crate::export::local_fix::FixError::Transcoded`].
    #[must_use]
    pub fn what(&self) -> String {
        match self {
            Self::Signature { found } => {
                let found: Vec<String> = found.iter().map(|byte| format!("{byte:02x}")).collect();
                format!(
                    "its first box is {} rather than the ftyp every mp4 opens with",
                    if found.is_empty() { "nothing".to_owned() } else { found.join(" ") }
                )
            }
            Self::Structure => "a box inside it declares a size that does not fit its parent".to_owned(),
            Self::NoHeader => "it holds no moov/mvhd, so it carries no movie header to date".to_owned(),
            Self::ZeroTimescale => {
                "its movie header gives the file a timescale of zero, which no player can turn into a duration".to_owned()
            }
        }
    }
}

impl fmt::Display for NotMp4 {
    /// The description plus the advice that fits a **source** file. See [`NotMp4::what`] for why
    /// the two are separable at all.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let advice = match self {
            Self::Signature { .. } => "only mp4 video is stamped, so a memory in another container needs converting first",
            Self::Structure | Self::NoHeader => "the file is truncated or corrupt, so re-extract the export part holding it",
            Self::ZeroTimescale => "the file is corrupt, so re-extract the export part holding it",
        };
        let opening = if matches!(self, Self::Signature { .. }) { "not an mp4" } else { "not a usable mp4" };
        write!(f, "{opening}: {}; {advice}", self.what())
    }
}

impl Error for NotMp4 {}

/// The instant cannot be stored in an MP4 header the way both readers will read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    /// Before 1970, which is spec-representable and gets misread.
    ///
    /// MP4's epoch is 1904, so a 1950s capture is a perfectly legal raw value — and it lands under
    /// the boundary where ffmpeg and exiftool both switch to reading the field as a unix timestamp,
    /// which puts it in the 2030s instead. Two distinct raw values also collide on one displayed
    /// instant down there. Refusing is the only answer that never writes a date a reader will
    /// disagree with.
    BeforeEpoch { instant: DateTime<Utc> },
    /// Past what the 32-bit field of a version 0 header holds. Never wrapped, because a wrap is a
    /// wrong date that looks like a right one.
    PastHeader { instant: DateTime<Utc>, version: u8 },
}

impl fmt::Display for TimeRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeEpoch { instant } => write!(
                f,
                "{} is before 1970 and an mp4 header holding it is read back as a date in the 2030s by both ffmpeg \
                 and exiftool, so it is refused rather than written",
                instant.to_rfc3339_opts(SecondsFormat::Secs, true)
            ),
            Self::PastHeader { instant, version } => write!(
                f,
                "{} does not fit the 32-bit time field of this file's version {version} movie header, and wrapping it \
                 would write a wrong date that reads as a right one",
                instant.to_rfc3339_opts(SecondsFormat::Secs, true)
            ),
        }
    }
}

impl Error for TimeRange {}

/// Something went wrong reading or writing a video's container metadata.
#[derive(Debug)]
pub enum VideoError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    NotMp4 {
        path: PathBuf,
        source: NotMp4,
    },
    /// The buffer walked when it was taken in and does not now. Unreachable while [`Mp4`] owns its
    /// bytes, and kept rather than unwrapped because that ownership is the only thing making it so.
    Structure {
        source: NotMp4,
    },
    Time {
        source: TimeRange,
    },
    /// The metadata item list could not be read back or spliced.
    ///
    /// Boxed because `mp4ameta::Error` carries a `String` description and an `io::Error`, which
    /// makes it the largest variant here by a wide margin.
    Tag {
        source: Box<mp4ameta::Error>,
    },
    Write {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for VideoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "could not read {} to stamp it: {source}", path.display()),
            Self::NotMp4 { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Structure { source } => write!(f, "the video stopped parsing part-way through being stamped: {source}"),
            Self::Time { source } => write!(f, "{source}"),
            Self::Tag { source } => write!(
                f,
                "could not write the capture metadata into the video ({source}); it is left alone rather than \
                 half-written, since a partly-spliced movie box is worse than an undated file"
            ),
            Self::Write { path, source } => {
                write!(f, "could not write {}: {source}; check the output directory is writable and has room", path.display())
            }
        }
    }
}

impl Error for VideoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::NotMp4 { source, .. } | Self::Structure { source } => Some(source),
            Self::Time { source } => Some(source),
            Self::Tag { source } => Some(source),
        }
    }
}

// ---- the box walk ----

/// One `creation_time` field, and the width its version gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeaderTime {
    /// Where `creation_time` starts. `modification_time` is the same width immediately after it.
    at: usize,
    /// 0 for the 32-bit form, 1 for the 64-bit one. Every other value is rejected at parse time,
    /// because a version this build does not know has an unknown field layout at this offset.
    version: u8,
}

impl HeaderTime {
    /// Whether `raw` fits this field. Only version 0 can fail, and it must fail rather than wrap.
    const fn holds(self, raw: u64) -> bool {
        self.version == 1 || raw <= u32::MAX as u64
    }

    /// Overwrites both times, which is fixed-size and so moves nothing.
    ///
    /// The 32-bit form is the low half of the same big-endian encoding rather than a cast, so
    /// there is no truncation to get wrong; [`Self::holds`] has already established the high half
    /// is zero.
    fn write(self, bytes: &mut [u8], raw: u64) {
        let encoded = raw.to_be_bytes();
        let field = if self.version == 1 { &encoded[..] } else { &encoded[4..] };
        bytes[self.at..self.at + field.len()].copy_from_slice(field);
        bytes[self.at + field.len()..self.at + 2 * field.len()].copy_from_slice(field);
    }
}

/// What one walk of the box chain found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Layout {
    /// Every `mvhd`/`tkhd`/`mdhd` time field, in file order.
    times: Vec<HeaderTime>,
    /// The `moov/mvhd` one, which is the file's own creation time rather than a track's.
    movie: Option<HeaderTime>,
    /// The `moov/mvhd` timescale. Read for one reason only: zero is a division by zero inside the
    /// tagging crate. See [`NotMp4::ZeroTimescale`].
    movie_timescale: Option<u32>,
    /// The `moov/udta` LOCATION child, if one stands there.
    location: Option<LocationAtom>,
}

/// A parsed box header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Head {
    /// The whole box, header included.
    size: u64,
    /// 8 for the ordinary form, 16 when a 64-bit size follows the fourcc.
    header: usize,
    fourcc: [u8; 4],
}

/// Walks a whole file, checking it opens with `ftyp`.
///
/// One walk answering three questions — where the header times are, which one is the movie's, and
/// whether a location atom shadows — because they are three questions about the same structure and
/// a second walk would be a second place to get the box grammar wrong.
fn layout(bytes: &[u8]) -> Result<Layout, NotMp4> {
    // The fourcc alone, before any size is trusted: a file whose first box is not `ftyp` has no
    // path through this module at all, and saying so beats reporting the size field as broken.
    let found: Vec<u8> = bytes.iter().skip(4).take(4).copied().collect();
    if found != b"ftyp" {
        return Err(NotMp4::Signature { found });
    }
    let mut layout = Layout::default();
    collect(bytes, 0, bytes.len(), &mut layout)?;
    Ok(layout)
}

/// Walks the boxes in `bytes[at..end]`, descending into the containers and recording the fields.
fn collect(bytes: &[u8], mut at: usize, end: usize, layout: &mut Layout) -> Result<(), NotMp4> {
    while at < end {
        let Some(head) = head(bytes, at, end) else {
            // A tail too short to be a box is the truncation case, not a clean end: a well-formed
            // chain lands exactly on `end`.
            return if at == end { Ok(()) } else { Err(NotMp4::Structure) };
        };
        let size = usize::try_from(head.size).map_err(|_| NotMp4::Structure)?;
        // Neither sum can overflow: `head` returned `Some`, which means `at + size` was checked and
        // fits `end`, and `header <= size`. Both facts come from there, so do not weaken that
        // check without revisiting this line. Asserted rather than only described, because a
        // contract that spans two functions is not something prose can hold — and because this is
        // the exact arithmetic that shipped an infinite loop once already.
        //
        // **Deliberate ceiling: this is the weaker of the two forms that are allowed here.** A guard
        // type is compiler-checked in every profile; an assert is only as good as whatever runs it,
        // and what runs this one is the debug test leg — which this repo arms fragilely (see the
        // Gate section of `CLAUDE.md`). So the tripwire's coverage rests on that arrangement
        // holding. What does NOT rest on it is the fix itself: `head`'s checked sum and the two
        // wrapping regression tests are release-side and unconditional, and they are what actually
        // stop the bug. Upgrade path, which removes the dependency entirely: have `head` hand back
        // the validated `body` and `next` offsets it already computed, so this function does no
        // arithmetic at all and there is no contract left to assert.
        debug_assert!(
            at.checked_add(size).is_some_and(|next| next <= end) && head.header <= size,
            "head returned a box that does not fit: at={at} size={size} header={} end={end}",
            head.header
        );
        let (body, next) = (at + head.header, at + size);
        if CONTAINERS.contains(&head.fourcc) {
            collect(bytes, body, next, layout)?;
        } else if TIMED.contains(&head.fourcc) {
            let field = timed(bytes, body, next).ok_or(NotMp4::Structure)?;
            if head.fourcc == *b"mvhd" && layout.movie.is_none() {
                layout.movie = Some(field);
                // Immediately after `modification_time`, at whichever width the version gives the
                // two times.
                let width = if field.version == 1 { 8 } else { 4 };
                let at = field.at + 2 * width;
                layout.movie_timescale = bytes.get(at..at + 4).and_then(|raw| raw.try_into().ok()).map(u32::from_be_bytes);
            }
            layout.times.push(field);
        } else if head.fourcc == *b"loci" {
            layout.location = Some(LocationAtom::Loci);
        } else if head.fourcc == COORDINATE {
            // Only a `udta` child shadows. `©xyz` also lives inside `ilst`, which is where this
            // build's own coordinate goes, and finding one of those would make the run refuse to
            // correct a file it wrote itself.
            layout.location.get_or_insert(LocationAtom::Xyz);
        }
        at = next;
    }
    Ok(())
}

/// The header of the box at `at`, or `None` when there is not a well-formed one that fits `end`.
///
/// **`at + size` is checked, and that is a correctness requirement rather than defensive habit.**
/// The 64-bit extended-size form puts a full `u64` from the file straight into `size`, so a corrupt
/// or hostile box can pick a value that wraps the sum. Wrapping makes an absurd size look like it
/// fits, and [`collect`] then sets its cursor to the wrapped `next` — which can be *behind* where it
/// started, so the walk re-reads the same boxes forever. Release builds turn overflow checks off,
/// so the symptom there is an infinite loop rather than a panic: measured, a 28-byte file with a
/// `moov` claiming `2^64 - 12` hung until it was killed, where debug panicked at this line.
fn head(bytes: &[u8], at: usize, end: usize) -> Option<Head> {
    let raw = u64::from(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let fourcc: [u8; 4] = bytes.get(at + 4..at + 8)?.try_into().ok()?;
    // Size 1 means the real one is a 64-bit field after the fourcc; size 0 means the box runs to
    // the end of its parent, which is legal for the last one.
    let (size, header) = match raw {
        1 => (u64::from_be_bytes(bytes.get(at + 8..at + 16)?.try_into().ok()?), 16),
        0 => (u64::try_from(end - at).ok()?, 8),
        _ => (raw, 8),
    };
    let fits = usize::try_from(size).is_ok_and(|size| size >= header && at.checked_add(size).is_some_and(|next| next <= end));
    fits.then_some(Head { size, header, fourcc })
}

/// The same header parse against a reader, for the seek-over-the-file probe.
///
/// Carries the same checked sum as [`head`] and for the same reason. Its caller happens to check
/// the cursor advance too, which saves release builds here — but a bounds test that overflows is
/// wrong wherever it sits, and in debug this line panicked on the same fixture.
fn read_head(file: &mut File, at: u64, end: u64) -> Option<Head> {
    let mut prefix = [0; 8];
    file.seek(SeekFrom::Start(at)).ok()?;
    file.read_exact(&mut prefix).ok()?;
    let raw = u64::from(u32::from_be_bytes(prefix[..4].try_into().ok()?));
    let fourcc: [u8; 4] = prefix[4..].try_into().ok()?;
    let (size, header) = match raw {
        1 => {
            let mut large = [0; 8];
            file.read_exact(&mut large).ok()?;
            (u64::from_be_bytes(large), 16)
        }
        0 => (end - at, 8),
        _ => (raw, 8),
    };
    let fits = size >= header as u64 && at.checked_add(size).is_some_and(|next| next <= end);
    fits.then_some(Head { size, header, fourcc })
}

/// The time field of an `mvhd`/`tkhd`/`mdhd` body running `[body, end)`.
fn timed(bytes: &[u8], body: usize, end: usize) -> Option<HeaderTime> {
    let version = *bytes.get(body)?;
    // A version this build does not know puts unknown fields where the times should be, and
    // writing there would corrupt whatever they are.
    let width = match version {
        0 => 4,
        1 => 8,
        _ => return None,
    };
    // The version-and-flags word, then both times.
    let at = body + 4;
    (at + 2 * width <= end).then_some(HeaderTime { at, version })
}

/// The raw value in a header time field.
fn read_time(bytes: &[u8], field: HeaderTime) -> Option<u64> {
    Some(match field.version {
        1 => u64::from_be_bytes(bytes.get(field.at..field.at + 8)?.try_into().ok()?),
        _ => u64::from(u32::from_be_bytes(bytes.get(field.at..field.at + 4)?.try_into().ok()?)),
    })
}

/// A raw header value as an instant, or `None` for the ambiguous band. See [`Mp4::embedded_time`].
fn instant_of(raw: u64) -> Option<DateTime<Utc>> {
    let unix = raw.checked_sub(MP4_EPOCH_OFFSET).filter(|unix| *unix >= 1)?;
    DateTime::from_timestamp(i64::try_from(unix).ok()?, 0)
}

/// An instant as an MP4 header stores one, refusing everything a reader would misread.
fn raw_time(instant: DateTime<Utc>) -> Result<u64, TimeRange> {
    let unix = instant.timestamp();
    // The floor is 1 rather than 0: raw `MP4_EPOCH_OFFSET` exactly renders as exiftool's
    // `0000:00:00 00:00:00`, which is the same string it prints for a field that was never written.
    u64::try_from(unix)
        .ok()
        .filter(|unix| *unix >= 1)
        .and_then(|unix| unix.checked_add(MP4_EPOCH_OFFSET))
        .ok_or(TimeRange::BeforeEpoch { instant })
}

/// A coordinate as ISO 6709 Annex H spells one, which is what both readers parse out of `©xyz`.
///
/// Signs are mandatory and the degree fields are fixed-width, so `+05.100000` and `+002.294351` are
/// the shapes even where the value is small. Six decimal places is what the export's own
/// coordinates carry.
fn iso6709(location: LocationPoint) -> String {
    format!("{:+010.6}{:+011.6}/", location.latitude(), location.longitude())
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset, NaiveDate};

    use std::fs::File;

    use super::{
        HeaderTime, Layout, LocationAtom, MP4_EPOCH_OFFSET, Mp4, NotMp4, TimeRange, VideoStamp, instant_of, iso6709, layout, raw_time,
        read_head, read_time,
    };
    use crate::export::model::{Field, LocationPoint};

    /// A box: size, fourcc, body.
    fn atom(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(8 + body.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend(fourcc);
        bytes.extend(body);
        bytes
    }

    /// An `mvhd`/`tkhd`/`mdhd` body: version, flags, creation, modification, then zero filler out
    /// to `content`, the size the spec gives that box at that version.
    ///
    /// The sizes are spelled out rather than left short because `mp4ameta` checks them against its
    /// own constants and refuses a stub, so a fixture that only satisfied this crate's walk would
    /// only ever exercise half the stamp.
    fn header(version: u8, raw: u64, content: usize) -> Vec<u8> {
        header_scaled(version, raw, content, TIMESCALE)
    }

    /// A plausible movie timescale. Non-zero because zero is a division by zero inside the tagging
    /// crate, which is the thing [`NotMp4::ZeroTimescale`] exists to keep away from it.
    const TIMESCALE: u32 = 1000;

    fn header_scaled(version: u8, raw: u64, content: usize, timescale: u32) -> Vec<u8> {
        let encoded = raw.to_be_bytes();
        let times = if version == 1 { &encoded[..] } else { &encoded[4..] };
        let mut body = vec![version, 0, 0, 0];
        body.extend(times);
        body.extend(times);
        body.extend(timescale.to_be_bytes());
        body.resize(content, 0);
        body
    }

    /// The smallest file both this crate's walk and `mp4ameta` accept: `ftyp`, a `moov` holding a
    /// spec-sized `mvhd` and one track, and an `mdat`.
    fn minimal(version: u8, raw: u64) -> Vec<u8> {
        let (movie, track, media) = if version == 1 { (112, 96, 36) } else { (100, 84, 24) };
        let mvhd = atom(b"mvhd", &header(version, raw, movie));
        let mdia = atom(b"mdia", &atom(b"mdhd", &header(version, raw, media)));
        let trak = atom(b"trak", &[atom(b"tkhd", &header(version, raw, track)), mdia].concat());
        let mut bytes = atom(b"ftyp", b"isom\0\0\x02\0isommp42");
        bytes.extend(atom(b"moov", &[mvhd, trak].concat()));
        bytes.extend(atom(b"mdat", &[0; 16]));
        bytes
    }

    fn stamp(local: chrono::NaiveDateTime, offset: Option<FixedOffset>) -> VideoStamp {
        VideoStamp { local, offset, location: None }
    }

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day).unwrap().and_hms_opt(hour, minute, second).unwrap()
    }

    #[test]
    fn every_header_time_in_the_file_is_found_and_the_movies_is_told_from_a_tracks() {
        let found = layout(&minimal(0, 0)).unwrap();
        assert_eq!(found.times.len(), 3, "mvhd, tkhd and mdhd");
        assert_eq!(found.movie, Some(found.times[0]), "the movie header is the first, not a track's");
        assert_eq!(found.location, None);
    }

    #[test]
    fn a_file_that_does_not_open_with_ftyp_is_refused_before_anything_reads_it() {
        // `mp4ameta`'s own writer parses `ftyp` first, so a file without one has no path through
        // this module at all and the message should say what it is instead.
        assert_eq!(layout(b"\x89PNG\r\n\x1a\n").unwrap_err(), NotMp4::Signature { found: b"\r\n\x1a\n".to_vec() });
        assert_eq!(layout(&atom(b"moov", &[])).unwrap_err(), NotMp4::Signature { found: b"moov".to_vec() });
        assert!(matches!(layout(&[]).unwrap_err(), NotMp4::Signature { .. }));
    }

    #[test]
    fn a_box_chain_that_does_not_hold_together_is_refused_rather_than_walked() {
        // Opens right, then claims a box longer than the file: a truncated download.
        let mut truncated = atom(b"ftyp", b"isom");
        truncated.extend(b"\xff\xff\xff\xffmoov");
        assert_eq!(layout(&truncated).unwrap_err(), NotMp4::Structure);

        // A size under its own header is impossible.
        let mut impossible = atom(b"ftyp", b"isom");
        impossible.extend(b"\x00\x00\x00\x04moov");
        assert_eq!(layout(&impossible).unwrap_err(), NotMp4::Structure);

        // A tail too short to hold a box header at all.
        let mut stub = atom(b"ftyp", b"isom");
        stub.extend(b"\x00\x00\x00");
        assert_eq!(layout(&stub).unwrap_err(), NotMp4::Structure);

        // Walks, and carries no movie header to date.
        let mut headless = atom(b"ftyp", b"isom");
        headless.extend(atom(b"mdat", &[0; 8]));
        assert_eq!(Mp4::new(headless).unwrap_err(), NotMp4::NoHeader);
    }

    /// A box in the 64-bit extended-size form: the 32-bit field holds 1 and the real size follows
    /// the fourcc. `size` is written verbatim so a test can put a value in it that no honest file
    /// would carry.
    fn extended(fourcc: &[u8; 4], size: u64, body: &[u8]) -> Vec<u8> {
        let mut bytes = 1_u32.to_be_bytes().to_vec();
        bytes.extend(fourcc);
        bytes.extend(size.to_be_bytes());
        bytes.extend(body);
        bytes
    }

    /// Walks `bytes` on a worker thread and gives up after five seconds.
    ///
    /// The failure this guards is an infinite loop, and the gate runs `--release`, where overflow
    /// checks are off so there is no panic to red on. Without a time bound a regression would
    /// **hang** the suite instead of failing it — measured, killed at 90 seconds. The leaked worker
    /// spins until the process exits, which is fine: it only ever happens on an already-broken
    /// tree, and nextest gives each test its own process.
    fn walk_within_five_seconds(bytes: Vec<u8>) -> Result<Layout, NotMp4> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(layout(&bytes)));
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the box walk did not finish in five seconds, which is the infinite-loop regression this test exists for")
    }

    #[test]
    fn a_64_bit_box_size_that_wraps_the_cursor_is_refused_rather_than_walked_forever() {
        // The 32-bit size field cannot express this, but the extended form puts a whole `u64` from
        // the file into the walk's arithmetic. `2^64 - 12` is picked so `at + size` at `at == 12`
        // wraps to exactly 0: unchecked, the box "fits", the cursor moves BACKWARDS to 0, and the
        // walk re-reads the same two boxes for ever. Release builds have overflow checks off, so
        // the symptom is a hang rather than a panic — measured at 10s and killed, against a debug
        // panic on the identical bytes. A hang is worse than a panic: a panic at least stops.
        let mut bytes = atom(b"ftyp", b"isom");
        bytes.extend(extended(b"moov", u64::MAX - 11, &[]));
        assert_eq!(bytes.len(), 28);
        assert_eq!(walk_within_five_seconds(bytes.clone()).unwrap_err(), NotMp4::Structure);
        assert_eq!(Mp4::new(bytes).unwrap_err(), NotMp4::Structure);

        // The other wrapping shape: a size so large the sum overflows without landing on zero.
        let mut past = atom(b"ftyp", b"isom");
        past.extend(extended(b"moov", u64::MAX, &[]));
        assert_eq!(walk_within_five_seconds(past).unwrap_err(), NotMp4::Structure);
    }

    #[test]
    fn the_seeking_header_parse_refuses_a_wrapping_size_in_every_profile() {
        // `read_head` is pinned HERE rather than only through `header_time`, and the reason is
        // worth keeping: through the public probe the two behaviours are indistinguishable in
        // release. An unchecked `at + size` wraps to something small that passes the bounds test,
        // but any size that wraps also makes the caller's own `checked_add` fail, so both the
        // broken and the fixed build answer `None`. Measured: reverting the fix leaves the
        // `header_time` test green in release and red only in debug. Calling the parse directly is
        // what gives the fix teeth in the profile the gate actually runs.
        //
        // The debug profile matters here more than anywhere else in this crate, because release
        // turns overflow checks off: an arithmetic bug that panics loudly in debug becomes a silent
        // wrong answer, or an infinite loop, in the build the gate tests. `cargo.sh` runs its debug
        // leg only when the string below appears somewhere in the tree, and it greps comments as
        // well as code — so this sentence naming `debug_assertions` is, unusually, load-bearing:
        // delete it and the debug leg silently stops running. Said out loud because a build gate
        // armed by a comment is not something anyone should have to rediscover.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("wrapping.mp4");
        let mut bytes = atom(b"ftyp", b"isom");
        bytes.extend(extended(b"free", u64::MAX, &[]));
        std::fs::write(&path, &bytes).unwrap();
        let end = u64::try_from(bytes.len()).unwrap();

        let mut file = File::open(&path).unwrap();
        assert_eq!(read_head(&mut file, 0, end).map(|head| head.fourcc), Some(*b"ftyp"), "the first box is well formed");
        assert_eq!(read_head(&mut file, 12, end), None, "a size whose sum with the cursor wraps is not a box that fits");

        // The positive control, so a parse that refused every extended-size box would not pass.
        let honest = dir.path().join("honest.mp4");
        let mut good = atom(b"ftyp", b"isom");
        good.extend(extended(b"free", 32, &[0; 16]));
        std::fs::write(&honest, &good).unwrap();
        let mut file = File::open(&honest).unwrap();
        let head = read_head(&mut file, 12, u64::try_from(good.len()).unwrap()).expect("a legal 64-bit size must still parse");
        assert_eq!((head.fourcc, head.size, head.header), (*b"free", 32, 16));
    }

    #[test]
    fn a_well_formed_64_bit_box_size_still_walks() {
        // The positive control for the test above, and it is load-bearing: a "fix" that refused
        // every extended-size box would satisfy every negative case and silently stop reading a
        // legal file. Real memory videos are small enough never to need this form, but a `mdat`
        // over 4 GiB requires it and nothing stops one appearing.
        let mvhd = atom(b"mvhd", &header(0, 0, 100));
        let mut bytes = atom(b"ftyp", b"isom\0\0\x02\0isommp42");
        bytes.extend(atom(b"moov", &mvhd));
        // 16 bytes of header plus a 16-byte body, spelled the long way round.
        bytes.extend(extended(b"mdat", 32, &[0; 16]));

        let found = layout(&bytes).unwrap();
        assert_eq!(found.times.len(), 1, "the walk reached the movie header past the extended-size box");
        assert!(Mp4::new(bytes).is_ok());
    }

    #[test]
    fn a_zero_movie_timescale_is_refused_because_the_tagging_crate_divides_by_it() {
        // Not a fussy validation: `mp4ameta 0.13.0` runs `duration / timescale` on every read
        // before it consults a single config flag, so one corrupt video in an export would take
        // the whole run down with a panic instead of failing its own row. The guard type is where
        // that gets stopped, since the guard type is what the crate is only ever reached through.
        let zeroed = |timescale| {
            let mvhd = atom(b"mvhd", &header_scaled(0, 0, 100, timescale));
            let mut bytes = atom(b"ftyp", b"isom\0\0\x02\0isommp42");
            bytes.extend(atom(b"moov", &mvhd));
            bytes.extend(atom(b"mdat", &[0; 16]));
            bytes
        };
        assert_eq!(Mp4::new(zeroed(0)).unwrap_err(), NotMp4::ZeroTimescale);
        // The positive control: the same fixture with a real timescale is accepted, so the
        // rejection is about the value and not about the shape.
        assert!(Mp4::new(zeroed(1)).is_ok());
    }

    #[test]
    fn a_header_version_this_build_does_not_know_is_refused_rather_than_written_over() {
        // Version 2 puts unknown fields where the times are, and this build must not guess at their
        // width: writing four or eight bytes there corrupts whatever they actually are.
        let mut bytes = atom(b"ftyp", b"isom");
        bytes.extend(atom(b"moov", &atom(b"mvhd", &header(2, 0, 100))));
        assert_eq!(layout(&bytes).unwrap_err(), NotMp4::Structure);
    }

    #[test]
    fn both_udta_location_atoms_are_seen_and_an_ilst_coordinate_is_not_mistaken_for_one() {
        let with = |child: Vec<u8>| {
            let mut bytes = atom(b"ftyp", b"isom");
            bytes.extend(atom(b"moov", &[atom(b"mvhd", &header(0, 0, 100)), atom(b"udta", &child)].concat()));
            bytes
        };
        assert_eq!(layout(&with(atom(b"loci", &[0; 12]))).unwrap().location, Some(LocationAtom::Loci));
        assert_eq!(layout(&with(atom(b"\xa9xyz", b"+00.0+000.0/"))).unwrap().location, Some(LocationAtom::Xyz));
        // The sentinel real memory videos carry is not a GPS source, so it must not read as one:
        // treating it as a shadow would refuse the coordinate on every real video.
        assert_eq!(layout(&with(atom(b"\xa9eng", b"-180.00-180.000/"))).unwrap().location, None);
        // The same fourcc inside `ilst` is where this build writes its OWN coordinate. Reading it
        // as a shadow would make a run refuse to correct a file it wrote itself.
        let mut mine = atom(b"ftyp", b"isom");
        let ilst = atom(b"ilst", &atom(b"\xa9xyz", b"+00.0+000.0/"));
        let meta = atom(b"meta", &[vec![0; 4], atom(b"hdlr", &[0; 24]), ilst].concat());
        mine.extend(atom(b"moov", &[atom(b"mvhd", &header(0, 0, 100)), atom(b"udta", &meta)].concat()));
        assert_eq!(layout(&mine).unwrap().location, None);
    }

    #[test]
    fn a_pre_1970_instant_is_refused_rather_than_written_where_a_reader_would_misread_it() {
        // 1965 is a legal raw value against the 1904 epoch and it lands in the band both readers
        // switch to reading as unix seconds, which shows it as 2036.
        let instant = at(1965, 6, 1, 12, 0, 0).and_utc();
        assert_eq!(raw_time(instant), Err(TimeRange::BeforeEpoch { instant }));
        // Unix zero exactly, which exiftool renders with the same string it uses for a field that
        // was never written, so it is indistinguishable from absent.
        let epoch = at(1970, 1, 1, 0, 0, 0).and_utc();
        assert_eq!(raw_time(epoch), Err(TimeRange::BeforeEpoch { instant: epoch }));
        // One second later is the floor, and it is the first value that survives the round trip.
        assert_eq!(raw_time(epoch + chrono::Duration::seconds(1)), Ok(MP4_EPOCH_OFFSET + 1));
    }

    #[test]
    fn the_reader_refuses_the_whole_band_the_two_conventions_disagree_over() {
        // Raw 1610717405 is a real 2021 date under the unix reading and a 1955 one under the spec
        // reading, and nothing in the file says which its writer meant. Answering `None` sends the
        // caller to the filename day instead of to one of two instants 66 years apart.
        assert_eq!(instant_of(1_610_717_405), None);
        assert_eq!(instant_of(0), None, "a header that was never written");
        assert_eq!(instant_of(MP4_EPOCH_OFFSET), None, "exiftool's never-written sentinel");
        // Immediately above the boundary the two conventions no longer both apply, and this is the
        // floor the write path enforces, so everything this build writes reads back.
        assert_eq!(instant_of(MP4_EPOCH_OFFSET + 1).map(|at| at.timestamp()), Some(1));
        assert_eq!(instant_of(3_693_562_205).map(|at| at.to_rfc3339()), Some("2021-01-15T13:30:05+00:00".to_owned()));
    }

    #[test]
    fn an_instant_past_a_version_0_field_errors_rather_than_wrapping() {
        let field = HeaderTime { at: 0, version: 0 };
        assert!(field.holds(u64::from(u32::MAX)));
        assert!(!field.holds(u64::from(u32::MAX) + 1));
        // Version 1's field is 64 bits, so nothing a `DateTime<Utc>` holds overflows it.
        assert!(HeaderTime { at: 0, version: 1 }.holds(u64::MAX));

        // The exact boundary, taken from the field width rather than from a hand-picked year that
        // would drift the moment either constant moved. One second past it a wrap would land the
        // capture in 1904, which reads as a plausible date rather than as a failure.
        let ceiling = i64::from(u32::MAX) - i64::try_from(MP4_EPOCH_OFFSET).unwrap();
        let mut video = Mp4::new(minimal(0, 0)).unwrap();
        let refused = video.stamp(&stamp(DateTime::from_timestamp(ceiling + 1, 0).unwrap().naive_utc(), None)).unwrap_err().to_string();
        assert!(refused.contains("does not fit"), "{refused}");
        assert_eq!(video.as_bytes(), &minimal(0, 0), "a refused stamp leaves the buffer exactly as it was");
        // The last instant that does fit is written rather than refused, so the boundary is pinned
        // from both sides and neither assertion can pass on its own.
        assert!(video.stamp(&stamp(DateTime::from_timestamp(ceiling, 0).unwrap().naive_utc(), None)).is_ok());
    }

    #[test]
    fn the_patch_writes_both_times_of_every_header_in_the_file() {
        let mut video = Mp4::new(minimal(0, 0)).unwrap();
        // Read off the unpatched buffer, so the offsets are the ones the patch was aimed at rather
        // than ones re-derived from its own output.
        let found = layout(video.as_bytes()).unwrap();
        video.stamp(&stamp(at(2021, 1, 15, 13, 30, 5), Some(FixedOffset::east_opt(0).unwrap()))).unwrap();

        let raw = 1_610_717_405 + MP4_EPOCH_OFFSET;
        for field in &found.times {
            assert_eq!(read_time(video.as_bytes(), *field), Some(raw), "{field:?}");
            // Modification time sits immediately after creation time and gets the same value.
            let modification = HeaderTime { at: field.at + 4, version: field.version };
            assert_eq!(read_time(video.as_bytes(), modification), Some(raw));
        }
        assert_eq!(video.embedded_time().map(|at| at.to_rfc3339()), Some("2021-01-15T13:30:05+00:00".to_owned()));
    }

    #[test]
    fn a_version_1_header_takes_the_64_bit_form() {
        let mut video = Mp4::new(minimal(1, 0)).unwrap();
        video.stamp(&stamp(at(2021, 1, 15, 13, 30, 5), None)).unwrap();
        assert_eq!(video.embedded_time().map(|at| at.to_rfc3339()), Some("2021-01-15T13:30:05+00:00".to_owned()));
        // The far future that overflows a version 0 field is representable here.
        let mut video = Mp4::new(minimal(1, 0)).unwrap();
        assert!(video.stamp(&stamp(at(2400, 1, 1, 0, 0, 0), None)).is_ok());
    }

    #[test]
    fn version_1_track_and_media_headers_keep_both_times_through_the_stamp() {
        let mut video = Mp4::new(minimal(1, 0)).unwrap();
        // Read off the unpatched buffer, like the v0 read-back test does, so the offsets are the
        // ones the patch was aimed at rather than ones re-derived from its own output.
        let found = layout(video.as_bytes()).unwrap();
        // The count is asserted rather than relied on, because the walker's own record is what a
        // test that only iterated it would be proving: drop a fourcc and "every field" shrinks
        // with the walker, which is exactly the silent failure this test exists to catch.
        assert_eq!(found.times.len(), 3, "mvhd, tkhd and mdhd, in file order");
        let (track, media) = (found.times[1], found.times[2]);

        video.stamp(&stamp(at(2021, 1, 15, 13, 30, 5), None)).unwrap();
        let raw = 1_610_717_405 + MP4_EPOCH_OFFSET;
        for field in [track, media] {
            assert_eq!(read_time(video.as_bytes(), field), Some(raw), "{field:?}");
            // Modification time sits immediately after creation and carries the same value, at
            // the 64-bit width this version gives both fields.
            let modification = HeaderTime { at: field.at + 8, version: field.version };
            assert_eq!(read_time(video.as_bytes(), modification), Some(raw));
        }

        // A value past 2^32 cannot round-trip through a version 0 field, so this leg pins the
        // width handling where a date that fits both versions cannot tell the two apart.
        let future = at(2400, 1, 1, 0, 0, 0).and_utc();
        video.stamp(&stamp(future.naive_utc(), None)).unwrap();
        let future_raw = u64::try_from(future.timestamp() + i64::try_from(MP4_EPOCH_OFFSET).unwrap()).unwrap();
        for field in [track, media] {
            assert_eq!(read_time(video.as_bytes(), field), Some(future_raw), "{field:?}");
        }
    }

    #[test]
    fn the_content_date_states_an_offset_only_when_the_run_knows_one() {
        let local = at(2021, 1, 15, 14, 30, 5);
        let paris = FixedOffset::east_opt(3600).unwrap();
        assert_eq!(stamp(local, Some(paris)).content_date(), "2021-01-15T14:30:05+01:00");
        // No offset means the wall time is in no stated zone, and writing `+00:00` would upgrade
        // "unknown" to "UTC" for free. The header fields still read it as UTC, because they have no
        // way to say anything else.
        assert_eq!(stamp(local, None).content_date(), "2021-01-15T14:30:05");
        assert_eq!(stamp(local, None).instant().to_rfc3339(), "2021-01-15T14:30:05+00:00");
        assert_eq!(stamp(local, Some(paris)).instant().to_rfc3339(), "2021-01-15T13:30:05+00:00");
    }

    #[test]
    fn a_coordinate_is_written_in_the_fixed_width_form_both_readers_parse() {
        let point = |text: &str| LocationPoint::parse(Field::Location, text).unwrap();
        assert_eq!(iso6709(point("Latitude, Longitude: 48.858844, 2.294351")), "+48.858844+002.294351/");
        // Southern and western, where a dropped sign is the whole error.
        assert_eq!(iso6709(point("Latitude, Longitude: -22.951916, -43.210487")), "-22.951916-043.210487/");
        // Small magnitudes still get the full field width, signs included.
        assert_eq!(iso6709(point("Latitude, Longitude: 5.1, 2.0")), "+05.100000+002.000000/");
        assert_eq!(iso6709(point("Latitude, Longitude: 0.0, 0.0")), "+00.000000+000.000000/");
        // The extremes the model validates to.
        assert_eq!(iso6709(point("Latitude, Longitude: -90.0, -180.0")), "-90.000000-180.000000/");
    }
}
