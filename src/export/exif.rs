//! Capture metadata written into a JPEG, and the guard type that keeps `little_exif` on the one
//! path it is safe on.
//!
//! Two measured facts about `little_exif 0.6.23` decide this module's whole shape. Both are
//! settled in `docs/design.md`'s **Metadata write notes** with crate internals in
//! `docs/domain-knowledge.md`; neither is re-derived here.
//!
//! 1. **`write_to_file` never truncates.** It opens without `.truncate(true)`, seeks to 0 and
//!    writes with no `set_len`, so a write producing a smaller buffer leaves the old tail on disk
//!    — measured at 4004 stale bytes with the previous payload still greppable, and both
//!    `exiftool -validate` and ffmpeg calling the file clean. For a tool whose job is stripping
//!    and replacing metadata, that is metadata you believe you replaced staying readable.
//! 2. **The XML parser carrying RUSTSEC-2026-0194 is reachable only from the PNG write path.**
//!    The advisory is live the moment anything hands a PNG to this crate, and inert while nothing
//!    does. The export's 162 `.png` files are all overlays, so what gets stamped is the JPEG
//!    composite, never the overlay.
//!
//! Both are made structural instead of remembered, because nothing checks a comment — but by two
//! different mechanisms, and conflating them is how one of them quietly stops holding:
//!
//! - **Constraint 1 is closed by [`Jpeg`] owning the bytes.** No path is ever handed to
//!   `little_exif`, so `write_to_file` — which takes a path and nothing else — has no reachable
//!   call site. [`Jpeg::write`] is the only way out and it is `fs::write`, which truncates.
//! - **Constraint 2 is closed by the private `library` module, not by the signature check.** Every
//!   call into `little_exif` goes through two functions that take **no file type**, and the type
//!   naming one is not in scope anywhere else. So there is no call site that chooses a file type
//!   and therefore nothing to smuggle a PNG variant through. The compiler carries it.
//!
//! [`Jpeg::new`]'s marker walk is a third, weaker thing and is worth being honest about: it stops
//! corrupt input reaching the JPEG parser and gives a better message than the library's, and it is
//! **not** what keeps the advisory unreachable. Reading it as the security boundary would make a
//! PNG file type look safe to pass as long as the bytes were checked. It is not.
//!
//! The residual after all three is caller-side: a caller can still write [`Jpeg::as_bytes`] itself.
//! What the type removes is the ability to do it wrong through this module.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{FixedOffset, NaiveDateTime};
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
use little_exif::rational::uR64;

use crate::export::model::{Attribution, LocationPoint};

mod library {
    //! The entire surface this crate has on `little_exif`, and the boundary that keeps
    //! RUSTSEC-2026-0194 unreachable.
    //!
    //! **`little_exif` has eleven public entry points and they split in two. This module makes the
    //! compiler carry six of them; the other five are held by convention.** Say it that way and not
    //! more, because `deny.toml` points here for a posture the user approved.
    //!
    //! - **Six take a `FileExtension`** (`new_from_vec`, `write_to_vec`, `clear_metadata`,
    //!   `clear_app12_segment`, `clear_app13_segment`, `as_u8_vec`). Neither function below takes
    //!   one and `FileExtension` is not nameable outside this module, so no call site in the crate
    //!   can choose a variant — not conditionally, not behind a type alias, not under any spelling.
    //!   **That half is a compiler guarantee**: the counterexample is `error[E0425]`/`[E0433]`.
    //! - **Five infer it from a PATH and need no `FileExtension` at all** (`new_from_path`,
    //!   `write_to_file`, `file_clear_metadata`, `file_clear_app12_segment`,
    //!   `file_clear_app13_segment`). Nothing stops a future caller reaching one from any module,
    //!   with no import. **That half is a convention, held only by there being no caller** —
    //!   verified, all five have none. Adding one is new code visible in a diff, which is the
    //!   protection; the compiler is not.
    //!
    //! The distinction is not academic: `Metadata::file_clear_metadata(path)` compiles from a module
    //! with no `little_exif` import and reaches `png::file_clear_metadata` ->
    //! `png::clear_metadata` -> `remove_exif_from_xmp` -> `quick_xml`, which is the 0194 call.
    //! Measured, not reasoned. An "X cannot happen" is a guarantee only when the compiler rejects
    //! the counterexample, and this one builds.
    //!
    //! This replaced a `const FILE_TYPE` visible to the whole module plus a test that scanned the
    //! source for other spellings. Review beat that scan twice — an aliased type at a call site
    //! dispatching on a fixture dimension that was held constant, and a `//` inside a string
    //! literal hiding code from the stripper — and the second break was the argument for deleting
    //! the instrument rather than sharpening it.
    //!
    //! What is left, stated so nobody reads more into this than it holds. A future edit can name
    //! the type by **any path** and call `Metadata::write_to_vec` directly, bypassing this module.
    //! Not only through a `use`: a fully-qualified `little_exif::filetype::FileExtension::PNG`
    //! needs no import at all, and it compiles — measured, not assumed. So the guarantee is not
    //! "the type is unreachable"; it is that **no existing call site can be made to pass a
    //! different file type**, because none of them takes one. Subverting it means adding a new
    //! direct call to the library, which is new code in the diff rather than a one-variant edit.
    //!
    //! An earlier wording of this said "import", which described a narrower hole than exists. It is
    //! the second time a conceded residual here was too small; if the next reader finds a third,
    //! widen it rather than defending the sentence.
    //!
    //! **All five path-inferred entry points lose the property, and here they are by name**, since
    //! a correct claim propped up by a short list is how the next hole gets conceded too small:
    //!
    //! | entry point | what decides the dispatch |
    //! |---|---|
    //! | `new_from_path` | the file's CONTENT, which beats the extension |
    //! | `write_to_file` | the path's extension |
    //! | `file_clear_metadata` | the path's extension — **the most direct reach to 0194** |
    //! | `file_clear_app12_segment` | the path's extension |
    //! | `file_clear_app13_segment` | the path's extension |
    //!
    //! Passing a constant from here is the only form fixed at compile time. `file_clear_metadata` is
    //! called out because it is the shortest route: one public call, no import, straight into
    //! `png::clear_metadata` and the XML parser. It was the unnamed one when this list said "the two
    //! alternatives", which is the enumeration trap in a smaller costume.

    use little_exif::filetype::FileExtension;
    use little_exif::metadata::Metadata;

    /// The only file type this crate ever names, at the only two places that can name it.
    const FILE_TYPE: FileExtension = FileExtension::JPEG;

    /// Decodes the APP1 the bytes carry.
    ///
    /// `&Vec<u8>` rather than `&[u8]` because `new_from_vec` demands it and a slice does not
    /// coerce. Clippy's `ptr_arg` does not fire here — it recognises the argument being forwarded
    /// to something needing the concrete type — so this carries no suppression.
    pub(super) fn read(bytes: &Vec<u8>) -> Result<Metadata, std::io::Error> {
        Metadata::new_from_vec(bytes, FILE_TYPE)
    }

    /// Writes `metadata` into `bytes`, replacing whatever APP1 was there.
    pub(super) fn write(metadata: &Metadata, bytes: &mut Vec<u8>) -> Result<(), std::io::Error> {
        metadata.write_to_vec(bytes, FILE_TYPE)
    }
}

/// How EXIF spells a date: `YYYY:MM:DD HH:MM:SS`, colons in the date and all.
const EXIF_DATETIME: &str = "%Y:%m:%d %H:%M:%S";

/// The EXIF version this build writes, as the four ASCII bytes the tag holds (no terminator).
const EXIF_VERSION: [u8; 4] = *b"0232";

/// `ComponentsConfiguration` for a YCbCr image: Y, Cb, Cr, then unused.
const YCBCR_COMPONENTS: [u8; 4] = [1, 2, 3, 0];

/// `ColorSpace` 1 is sRGB, which is what the JPEG encoder writes and what an untagged JPEG is
/// read as anyway.
const COLOR_SPACE_SRGB: u16 = 1;

/// `YCbCrPositioning` 1 is "centered", the value baseline JPEG chroma subsampling implies.
const YCBCR_POSITIONING_CENTERED: u16 = 1;
/// The display default: a repaired memory has no physical size, so the resolution pair carries the
/// value every screen assumes rather than a measured one.
const RESOLUTION_DPI: u32 = 72;
const RESOLUTION_UNIT_INCHES: u16 = 2;
const FLASHPIX_VERSION: [u8; 4] = *b"0100";

/// GPS tag version 2.3.0.0, the version the coordinate tags below are defined by.
const GPS_VERSION: [u8; 4] = [2, 3, 0, 0];

/// The longest a caller-supplied string may be when it reaches a tag, in bytes.
///
/// **This is a JPEG ceiling and it belongs to this module**, which is why it is not on
/// [`Attribution`]: the APP1 segment carries a 16-bit length, and `little_exif` does not enforce it.
/// `little_exif-0.6.23`'s `src/jpg.rs:37` builds that field as
/// `2u16 + (EXIF_HEADER.len() as u16) + (exif_vec.len() as u16)` — the cast lands **before** the
/// add, so an oversized payload wraps the declared length with no arithmetic an overflow check could
/// catch, silently, in release *and* in debug. Measured on that version: at a 70,000-byte
/// description `write_to_vec` returns `Ok` and declares 4,504 bytes for a ~70,044-byte segment;
/// exiftool then reports `Bad ExifOffset SubDirectory start` and emits no row at all for
/// `DateTimeOriginal`, the offset tags, `Artist` or the description. **The whole APP1 this program
/// exists to write is lost, and `Jpeg::stamp` returns `Ok`.** So the failure is not a mangled field,
/// it is every field.
///
/// Truncating rather than refusing is decision 2's degrade posture: a capability is lost, never the
/// run. The number is far above both observed shapes — a Snapchat handle and a 36-character uuid —
/// so it costs no real value, and far below the point where the sum of two could matter.
///
/// **What it does not close, stated rather than implied.** [`Jpeg::stamp`] is a read-modify-write:
/// `docs/domain-knowledge.md` records (measured 2026-07-26) that the decode-and-reencode preserves
/// foreign tags and an embedded IFD1 thumbnail byte-identically, so the payload is `preserved
/// foreign tags + preserved thumbnail + this build's tags` and only the last term is bounded here. A
/// source carrying a large enough thumbnail can still overflow the segment. How large a real
/// Snapchat thumbnail is has not been measured, so that is an unquantified residual and not a
/// claim either way.
const MAX_TAG_TEXT: usize = 256;

/// Denominator for the seconds component of a coordinate, giving ~3 mm of resolution — far past
/// what a phone's GPS or the export's six-decimal-place coordinates carry.
const ARCSECOND_SCALE: f64 = 10_000.0;

// ---- what gets written ----

/// Everything a run knows about one image, ready to go into its EXIF.
///
/// Split from the pipeline that derives it so this module needs to know nothing about buckets,
/// pairings or output paths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stamp<'a> {
    /// Local wall-clock time where the memory was taken, which is what `DateTimeOriginal` means.
    /// Never a UTC instant unless [`Self::offset`] says so.
    pub local: NaiveDateTime,
    /// The offset [`Self::local`] is at.
    ///
    /// `None` means the run could not work it out, and then **no offset tag is written at all**
    /// rather than a `+00:00` that would claim the wall time is UTC. A reader then sees a local
    /// time in an unstated zone, which is what is actually known.
    pub offset: Option<FixedOffset>,
    /// `None` when the run has no coordinate, or when the pairing that would supply one is too
    /// arbitrary to stamp from. Deciding that is the caller's job, not this module's.
    pub location: Option<LocationPoint>,
    pub width: u32,
    pub height: u32,
    /// Where the file came from, or `None` when the run knows nothing about it.
    ///
    /// The memories leg passes `None` — a memory has no sender and no thread — so the two tags
    /// below are written on the chat-media leg alone, and a memory's own `Artist` or
    /// `ImageDescription` survives [`Jpeg::stamp`]'s read-modify-write byte for byte, which is what
    /// that method's preservation promise means here.
    ///
    /// Both halves are bounded at [`MAX_TAG_TEXT`] on the way into the tag — see that constant for
    /// what an unbounded one costs, which is the whole APP1 rather than the field. The bound is here
    /// and not on [`Attribution`] because the ceiling is JPEG's: the video leg's `ilst` atoms carry a
    /// 32-bit size and shortening them would be this format's constraint crossing into another.
    ///
    /// **`Artist` carries the sender and `ImageDescription` carries the conversation**, and both
    /// picks are against a named neighbour rather than for a vibe:
    ///
    /// - `Artist` is IFD0's only string field whose defined meaning is a PERSON, so every reader
    ///   that shows an author shows the sender. `Copyright` is the near neighbour and asserts a
    ///   legal claim this build cannot know; `UserComment` sits in the Exif IFD behind an 8-byte
    ///   character-code prefix and is rendered as a free-form note, not as a name.
    /// - `ImageDescription` is IFD0's free-text caption, which is where a line of per-file context
    ///   belongs, and it is the only IFD0 string tag not already spoken for by a camera fact.
    ///   **EXIF has no album or grouping tag at all**, which is why the video leg's answer for the
    ///   conversation (`©alb`) has no counterpart here and the two legs' tag names differ.
    pub attribution: Option<&'a Attribution>,
}

// ---- the guard type ----

/// A JPEG held in memory: the only form `little_exif` is ever handed anywhere in this crate.
///
/// See the module docs for what the type is guarding and why a comment could not.
#[derive(Clone, PartialEq, Eq)]
pub struct Jpeg(Vec<u8>);

/// Why bytes were refused before they could reach `little_exif`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotJpeg {
    /// The bytes do not open with the start-of-image marker.
    ///
    /// Carries the leading bytes it saw, which are format magic rather than image content, so the
    /// message can name what the file actually is without printing any of it.
    Signature {
        /// Up to the two bytes a start-of-image marker occupies. Shorter when the buffer was.
        found: Vec<u8>,
    },
    /// It opens like a JPEG and its marker chain does not hold together: a segment declares a
    /// length running past the end of the buffer, or the chain stops before the scan begins.
    ///
    /// Refused rather than passed on, because `little_exif`'s own walk fails on exactly these and
    /// the message it gives back names none of them. Nothing that would have worked is lost.
    Structure,
}

impl fmt::Display for NotJpeg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signature { found } => {
                let found: Vec<String> = found.iter().map(|byte| format!("{byte:02x}")).collect();
                write!(
                    f,
                    "not a jpeg: starts with {} rather than the ff d8 start-of-image marker; only jpeg output is stamped, \
                     so composite or transcode it first",
                    if found.is_empty() { "nothing".to_owned() } else { found.join(" ") }
                )
            }
            Self::Structure => f.write_str(
                "not a usable jpeg: it opens with the start-of-image marker and its segment chain breaks before the scan \
                 does; the file is truncated or corrupt, so re-extract the export part holding it",
            ),
        }
    }
}

impl Error for NotJpeg {}

impl fmt::Debug for Jpeg {
    /// Hand-written because the derived form would print every byte of an image into whatever a
    /// `{:?}` lands in. The length is the only thing about the buffer worth reading.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Jpeg").field("bytes", &self.0.len()).finish()
    }
}

impl Jpeg {
    /// Takes ownership of `bytes` if they are a JPEG.
    ///
    /// Not a signature prefix test: the whole marker chain is walked from the start-of-image marker
    /// to the start of scan, which is the exact region `little_exif` parses. A prefix test admits a
    /// truncated or corrupt file and defers the failure into the library, where the message names
    /// nothing useful.
    ///
    /// # Errors
    ///
    /// Returns [`NotJpeg`] for anything else. See the module docs for what this closes and what
    /// closes RUSTSEC-2026-0194 (that one is the private `library` module, not this check).
    ///
    /// # Examples
    ///
    /// ```
    /// use exportsnap::export::exif::Jpeg;
    ///
    /// // Start-of-image, an eight-byte APP0, then the scan.
    /// let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x08, 0, 0, 0, 0, 0, 0];
    /// jpeg.extend([0xff, 0xda, 0x00, 0x02]);
    /// assert!(Jpeg::new(jpeg).is_ok());
    ///
    /// assert!(Jpeg::new(b"\x89PNG\r\n\x1a\n".to_vec()).is_err());
    /// // Opens right, then claims a segment far longer than the buffer.
    /// assert!(Jpeg::new(vec![0xff, 0xd8, 0xff, 0xe0, 0xff, 0xff]).is_err());
    /// ```
    pub fn new(bytes: Vec<u8>) -> Result<Self, NotJpeg> {
        match walk(&bytes) {
            Structure::Walkable { .. } => Ok(Self(bytes)),
            Structure::NoSignature => Err(NotJpeg::Signature { found: bytes.iter().take(2).copied().collect() }),
            Structure::Broken => Err(NotJpeg::Structure),
        }
    }

    /// Reads `path` into memory and checks it is a JPEG.
    ///
    /// # Errors
    ///
    /// Returns [`ExifError::Read`] when the file cannot be read and [`ExifError::NotJpeg`] when it
    /// is not one.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, ExifError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| ExifError::Read { path: path.to_path_buf(), source })?;
        Self::new(bytes).map_err(|source| ExifError::NotJpeg { path: path.to_path_buf(), source })
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Writes the bytes to `path`, replacing whatever was there.
    ///
    /// `fs::write` truncates, which is the whole reason this method exists rather than
    /// `Metadata::write_to_file`. A write producing fewer bytes than the file already held leaves
    /// nothing of the old one behind.
    ///
    /// # Errors
    ///
    /// Returns [`ExifError::Write`] when the file cannot be created or written.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), ExifError> {
        let path = path.as_ref();
        fs::write(path, &self.0).map_err(|source| ExifError::Write { path: path.to_path_buf(), source })
    }

    /// The capture time the file already carries, if it carries one.
    ///
    /// `DateTimeOriginal` first, then `CreateDate`, then IFD0's `ModifyDate`: earliest-meaning
    /// first, since a rewritten file gets a fresh `ModifyDate` and keeps the other
    /// two. Best-effort by design — an unreadable or absent APP1 is `None`, not an error, because
    /// every caller of this has a fallback and none of them wants a missing tag to fail a run.
    #[must_use]
    pub fn embedded_time(&self) -> Option<NaiveDateTime> {
        let metadata = self.metadata().ok()?;
        [ExifTag::DateTimeOriginal(String::new()), ExifTag::CreateDate(String::new()), ExifTag::ModifyDate(String::new())]
            .iter()
            .find_map(|tag| text_of(&metadata, tag))
            .and_then(|text| NaiveDateTime::parse_from_str(text.trim_end_matches('\0'), EXIF_DATETIME).ok())
    }

    /// The offset the file states for its own `DateTimeOriginal`, if it states one.
    #[must_use]
    pub fn embedded_offset(&self) -> Option<FixedOffset> {
        let metadata = self.metadata().ok()?;
        [ExifTag::OffsetTimeOriginal(String::new()), ExifTag::OffsetTime(String::new())]
            .iter()
            .find_map(|tag| text_of(&metadata, tag))
            .and_then(|text| parse_offset(text.trim_end_matches('\0')))
    }

    /// Writes `stamp` into the buffer's EXIF, keeping every tag already there.
    ///
    /// A read-modify-write: whatever APP1 the bytes carry is decoded first and the stamp is set
    /// over it, so a foreign `Make`, `Artist` or embedded thumbnail survives byte-identically.
    /// `Metadata::new()` is used only when the bytes carry no APP1 at all, which is decided from
    /// the bytes rather than inferred from a decode failure — the two are different situations and
    /// only one of them may throw metadata away.
    ///
    /// # Errors
    ///
    /// Returns [`ExifError::Decode`] when the bytes carry an APP1 this build cannot read, and
    /// [`ExifError::Encode`] when the new metadata cannot be written back into them.
    pub fn stamp(&mut self, stamp: &Stamp<'_>) -> Result<(), ExifError> {
        let mut metadata = if matches!(walk(&self.0), Structure::Walkable { exif: true }) {
            library::read(&self.0).map_err(|source| ExifError::Decode { source })?
        } else {
            Metadata::new()
        };

        // The first six EXIF-mandatory tags plus `GPSVersionID` below. They are facts about the
        // image rather than guesses, so they are written on the read-modify-write path too.
        // Together with the four below the whole set takes `exiftool -validate` to `Validate: OK`
        // on an APP1 built from nothing.
        metadata.set_tag(ExifTag::ExifVersion(EXIF_VERSION.to_vec()));
        metadata.set_tag(ExifTag::ComponentsConfiguration(YCBCR_COMPONENTS.to_vec()));
        metadata.set_tag(ExifTag::ColorSpace(vec![COLOR_SPACE_SRGB]));
        metadata.set_tag(ExifTag::ExifImageWidth(vec![stamp.width]));
        metadata.set_tag(ExifTag::ExifImageHeight(vec![stamp.height]));
        metadata.set_tag(ExifTag::YCbCrPositioning(vec![YCBCR_POSITIONING_CENTERED]));
        // The rest of the Exif 2.32 mandatory set. exiftool 12.76's `-validate` names these four
        // when they are absent and 13.x stopped naming them, so they are written rather than
        // version-pinned: `FlashpixVersion` is the one value the spec defines, and the resolution
        // pair is the display default with the unit the spec assumes.
        metadata.set_tag(ExifTag::XResolution(vec![uR64 { nominator: RESOLUTION_DPI, denominator: 1 }]));
        metadata.set_tag(ExifTag::YResolution(vec![uR64 { nominator: RESOLUTION_DPI, denominator: 1 }]));
        metadata.set_tag(ExifTag::ResolutionUnit(vec![RESOLUTION_UNIT_INCHES]));
        metadata.set_tag(ExifTag::FlashpixVersion(FLASHPIX_VERSION.to_vec()));
        metadata.set_tag(ExifTag::GPSVersionID(GPS_VERSION.to_vec()));

        let local = stamp.local.format(EXIF_DATETIME).to_string();
        metadata.set_tag(ExifTag::DateTimeOriginal(local.clone()));
        metadata.set_tag(ExifTag::CreateDate(local.clone()));
        metadata.set_tag(ExifTag::ModifyDate(local));

        if let Some(offset) = stamp.offset {
            let offset = format_offset(offset);
            metadata.set_tag(ExifTag::OffsetTime(offset.clone()));
            metadata.set_tag(ExifTag::OffsetTimeOriginal(offset.clone()));
            metadata.set_tag(ExifTag::OffsetTimeDigitized(offset));
        }

        if let Some(location) = stamp.location {
            let (latitude, longitude) = (location.latitude(), location.longitude());
            metadata.set_tag(ExifTag::GPSLatitudeRef(hemisphere(latitude, 'N', 'S')));
            metadata.set_tag(ExifTag::GPSLatitude(sexagesimal(latitude)));
            metadata.set_tag(ExifTag::GPSLongitudeRef(hemisphere(longitude, 'E', 'W')));
            metadata.set_tag(ExifTag::GPSLongitude(sexagesimal(longitude)));
        }

        // Set only when the caller supplies one, so the memories leg — which never does — keeps
        // whatever these two tags already held. See [`Stamp::attribution`] for why these two tags.
        if let Some(attribution) = stamp.attribution {
            if let Some(sender) = &attribution.sender {
                metadata.set_tag(ExifTag::Artist(bounded(sender.as_str())));
            }
            if let Some(conversation) = &attribution.conversation {
                metadata.set_tag(ExifTag::ImageDescription(bounded(conversation.as_str())));
            }
        }

        library::write(&metadata, &mut self.0).map_err(|source| ExifError::Encode { source })
    }

    /// The decoded APP1, or the error the crate gave for it.
    fn metadata(&self) -> Result<Metadata, io::Error> {
        library::read(&self.0)
    }
}

/// `text` cut to [`MAX_TAG_TEXT`] bytes on a character boundary.
///
/// Built up character by character rather than sliced at a byte index, so the boundary is right by
/// construction: `&text[..MAX_TAG_TEXT]` **panics** when that index lands inside a multi-byte
/// codepoint, and a conversation key is arbitrary UTF-8 off `chat_history.json`. Neither
/// `clippy::unwrap_used` nor `expect_used` sees a slice panic, so nothing in the gate would have
/// caught it.
///
/// **The crate's other cap is safe for a reason this one does not inherit, which is why it is not
/// the model to copy.** `chat_fix::dir_name`'s `.take(MAX_DIR_NAME)` runs *after* its `portable()`
/// pass has already mapped every non-ASCII character to `_`, so every element it counts is one byte
/// and the count is a byte count for free. This cap sits on the raw key, before anything has
/// narrowed it. Refactoring `portable()` would break that coupling over there and not here.
fn bounded(text: &str) -> String {
    let mut kept = String::new();
    for character in text.chars() {
        if kept.len() + character.len_utf8() > MAX_TAG_TEXT {
            break;
        }
        kept.push(character);
    }
    kept
}

/// Something went wrong reading, decoding or writing an image's metadata.
///
/// [`Self::NotJpeg`] is the one that says a caller handed over the wrong thing; the rest are the
/// filesystem or the crate.
#[derive(Debug)]
pub enum ExifError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    NotJpeg {
        path: PathBuf,
        source: NotJpeg,
    },
    /// The bytes carry an APP1 segment this build cannot read back.
    Decode {
        source: io::Error,
    },
    /// The metadata could not be encoded into the bytes.
    Encode {
        source: io::Error,
    },
    Write {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for ExifError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "could not read {} to stamp it: {source}", path.display()),
            Self::NotJpeg { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Decode { source } => write!(
                f,
                "the image carries metadata this build cannot read ({source}); it is left alone rather than replaced, \
                 since overwriting it would throw away what is there"
            ),
            Self::Encode { source } => write!(f, "could not write the capture metadata into the image: {source}"),
            Self::Write { path, source } => {
                write!(f, "could not write {}: {source}; check the output directory is writable and has room", path.display())
            }
        }
    }
}

impl Error for ExifError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Decode { source } | Self::Encode { source } | Self::Write { source, .. } => Some(source),
            Self::NotJpeg { source, .. } => Some(source),
        }
    }
}

// ---- helpers ----

/// A tag's text, when it is present and is a string tag.
fn text_of<'a>(metadata: &'a Metadata, wanted: &ExifTag) -> Option<&'a String> {
    metadata.get_tag(wanted).find_map(|tag| match tag {
        ExifTag::DateTimeOriginal(text)
        | ExifTag::CreateDate(text)
        | ExifTag::ModifyDate(text)
        | ExifTag::OffsetTime(text)
        | ExifTag::OffsetTimeOriginal(text) => Some(text),
        _ => None,
    })
}

/// Writes EXIF 2.31's `+HH:MM` offset form.
///
/// Hand-rolled rather than taken from `FixedOffset`'s `Display`, which appends a seconds field for
/// any offset that has one. EXIF's tag is exactly six characters and a reader that takes the field
/// width literally would misread a seven-part one; sub-minute offsets are historical and none
/// survives in the tz database's modern rules, so truncating to whole minutes loses nothing a
/// memory could carry.
fn format_offset(offset: FixedOffset) -> String {
    let total = offset.local_minus_utc();
    let sign = if total < 0 { '-' } else { '+' };
    let minutes = total.unsigned_abs() / 60;
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Parses EXIF 2.31's `+HH:MM` offset form.
fn parse_offset(text: &str) -> Option<FixedOffset> {
    let (sign, rest) = text.split_at_checked(1)?;
    let (hours, minutes) = rest.split_once(':')?;
    let seconds = hours.parse::<i32>().ok()?.checked_mul(3600)?.checked_add(minutes.parse::<i32>().ok()?.checked_mul(60)?)?;
    match sign {
        "+" => FixedOffset::east_opt(seconds),
        "-" => FixedOffset::west_opt(seconds),
        _ => None,
    }
}

/// What a walk of the JPEG marker chain found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Structure {
    /// The chain held together from the start-of-image marker to the scan. `exif` says whether an
    /// APP1 carrying `Exif\0\0` stood in it.
    Walkable { exif: bool },
    /// It does not open with the start-of-image marker.
    NoSignature,
    /// It opens right, and a segment declares a length running past the end of the buffer, or the
    /// chain ends before the scan does.
    Broken,
}

/// Walks the segment chain from the start-of-image marker to the start of scan.
///
/// Two callers with one walk, because they are two questions about the same structure and a second
/// walk would be a second place to get the marker grammar wrong.
///
/// The EXIF answer is read off the markers rather than inferred from a `Metadata::new_from_vec`
/// failure, because that call answers "no EXIF here" and "this EXIF is broken" with the same
/// `io::ErrorKind`. Those two need opposite handling: the first may start from an empty
/// `Metadata`, the second must not, since starting from empty is how a file's existing metadata
/// gets silently thrown away.
///
/// Nothing past the scan is looked at. That is where entropy-coded pixel data begins, `0xff` bytes
/// inside it are not markers, and EXIF is always ahead of it — so walking on would be reading
/// pixels as structure.
fn walk(bytes: &[u8]) -> Structure {
    /// `Exif\0\0`, the header an APP1 segment carrying EXIF opens with. An APP1 can also hold XMP,
    /// which this build neither reads nor writes.
    const EXIF_HEADER: [u8; 6] = *b"Exif\0\0";
    /// Markers carrying no length field after them: TEM and the eight restart markers.
    const STANDALONE: [u8; 9] = [0x01, 0xd0, 0xd1, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7];

    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Structure::NoSignature;
    }

    let mut exif = false;
    let mut at = 2;
    loop {
        // Any number of `0xff` bytes may pad the gap before a marker, and a decoder that reads the
        // padding as the marker gets a different segment than the one that is there.
        while bytes.get(at) == Some(&0xff) && bytes.get(at + 1) == Some(&0xff) {
            at += 1;
        }
        let (Some(0xff), Some(&marker)) = (bytes.get(at).copied(), bytes.get(at + 1)) else {
            return Structure::Broken;
        };
        // Start of scan, or an image that ends without one. Either way there is nothing further
        // ahead that this build reads.
        if marker == 0xda || marker == 0xd9 {
            return Structure::Walkable { exif };
        }
        if STANDALONE.contains(&marker) {
            at += 2;
            continue;
        }
        let Some(raw) = bytes.get(at + 2..at + 4) else {
            return Structure::Broken;
        };
        // The length field counts its own two bytes, so anything under two is impossible, and a
        // segment reaching past the buffer means the file was cut short.
        let length = usize::from(u16::from_be_bytes([raw[0], raw[1]]));
        if length < 2 || bytes.len() < at + 2 + length {
            return Structure::Broken;
        }
        if marker == 0xe1 && bytes.get(at + 4..at + 4 + EXIF_HEADER.len()) == Some(&EXIF_HEADER[..]) {
            exif = true;
        }
        at += 2 + length;
    }
}

/// The single-letter hemisphere reference for a signed coordinate.
fn hemisphere(degrees: f64, positive: char, negative: char) -> String {
    if degrees < 0.0 { negative.to_string() } else { positive.to_string() }
}

/// A coordinate as EXIF stores it: unsigned degrees, minutes and seconds, the sign living in the
/// separate reference tag.
fn sexagesimal(degrees: f64) -> Vec<uR64> {
    let total = degrees.abs();
    // `total` is at most 180 and `whole_*` are floors of it, so neither cast can overflow a u32.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "bounded by the ±180 range LocationPoint validates")]
    let whole_degrees = total.trunc() as u32;
    let minutes = (total - total.trunc()) * 60.0;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a fraction of a degree is under 60 minutes")]
    let whole_minutes = minutes.trunc() as u32;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a fraction of a minute is under 60 seconds")]
    let scaled_seconds = ((minutes - minutes.trunc()) * 60.0 * ARCSECOND_SCALE).round() as u32;
    vec![
        uR64 { nominator: whole_degrees, denominator: 1 },
        uR64 { nominator: whole_minutes, denominator: 1 },
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "ARCSECOND_SCALE is a small positive constant")]
        uR64 { nominator: scaled_seconds, denominator: ARCSECOND_SCALE as u32 },
    ]
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::{Structure, format_offset, parse_offset, sexagesimal, walk};

    /// Whether the marker chain holds together AND carries an EXIF APP1, which is the pair of
    /// answers `stamp` branches on.
    fn exif(bytes: &[u8]) -> bool {
        matches!(walk(bytes), Structure::Walkable { exif: true })
    }

    /// A minimal JPEG: SOI, an APP0 JFIF segment, SOS, EOI. No APP1 anywhere.
    fn without_exif() -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        bytes.extend(b"JFIF\0");
        bytes.extend([0x01, 0x02, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
        bytes.extend([0xff, 0xda, 0x00, 0x02, 0xff, 0xd9]);
        bytes
    }

    fn with_exif() -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1, 0x00, 0x0a];
        bytes.extend(b"Exif\0\0");
        bytes.extend([0x49, 0x49]);
        bytes.extend([0xff, 0xda, 0x00, 0x02, 0xff, 0xd9]);
        bytes
    }

    #[test]
    fn an_app1_holding_exif_is_told_apart_from_one_that_is_absent() {
        assert!(exif(&with_exif()));
        assert!(!exif(&without_exif()));
        assert_eq!(walk(&without_exif()), Structure::Walkable { exif: false });
    }

    #[test]
    fn an_app1_that_is_not_exif_does_not_count_as_one() {
        // XMP travels in an APP1 too, and it is not what the decoder reads. It must not be counted
        // as EXIF, because `stamp` would then hand it to a decoder that answers "this is XMP" and
        // fail an item that had no EXIF to lose.
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1, 0x00, 0x1f];
        bytes.extend(b"http://ns.adobe.com/xap/1.0/\0");
        bytes.extend([0xff, 0xda, 0x00, 0x02, 0xff, 0xd9]);
        assert_eq!(walk(&bytes), Structure::Walkable { exif: false });
    }

    #[test]
    fn an_exif_app1_standing_behind_another_segment_is_still_found() {
        // The library's own reader returns the FIRST app1 whatever it holds, so a walk that stopped
        // at the first app1 would answer "no exif" here and start from an empty `Metadata` — which
        // is how the exif below gets thrown away.
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1, 0x00, 0x1f];
        bytes.extend(b"http://ns.adobe.com/xap/1.0/\0");
        bytes.extend([0xff, 0xe1, 0x00, 0x0a]);
        bytes.extend(b"Exif\0\0");
        bytes.extend([0x49, 0x49]);
        bytes.extend([0xff, 0xda, 0x00, 0x02, 0xff, 0xd9]);
        assert_eq!(walk(&bytes), Structure::Walkable { exif: true });
    }

    #[test]
    fn the_scan_stops_at_the_start_of_scan_rather_than_walking_pixel_data() {
        // `ff e1` inside entropy-coded data must not be read as a segment. Without the SOS stop
        // this finds the `Exif\0\0` sitting in the pixel bytes.
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xda, 0x00, 0x02];
        bytes.extend([0xff, 0xe1, 0x00, 0x0a]);
        bytes.extend(b"Exif\0\0");
        assert_eq!(walk(&bytes), Structure::Walkable { exif: false });
    }

    #[test]
    fn fill_bytes_before_a_marker_do_not_shift_which_segment_is_read() {
        // Any run of `0xff` may pad the gap before a marker. Reading the padding as the marker
        // finds a segment that is not there.
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xff, 0xff, 0xe1, 0x00, 0x0a];
        bytes.extend(b"Exif\0\0");
        bytes.extend([0x49, 0x49]);
        bytes.extend([0xff, 0xda, 0x00, 0x02]);
        assert_eq!(walk(&bytes), Structure::Walkable { exif: true });
    }

    #[test]
    fn a_chain_that_does_not_hold_together_is_broken_rather_than_walkable() {
        // Opens right, then claims a segment far longer than the buffer: a truncated download.
        assert_eq!(walk(&[0xff, 0xd8, 0xff, 0xe0, 0xff, 0xff]), Structure::Broken);
        // Truncated mid-marker, with no length to read at all.
        assert_eq!(walk(&[0xff, 0xd8, 0xff, 0xe1]), Structure::Broken);
        // A length under two is impossible: the field counts its own bytes.
        assert_eq!(walk(&[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x01]), Structure::Broken);
        // Ends without ever reaching the scan.
        assert_eq!(walk(&[0xff, 0xd8]), Structure::Broken);

        assert_eq!(walk(&[]), Structure::NoSignature);
        assert_eq!(walk(b"\x89PNG\r\n\x1a\n"), Structure::NoSignature);
    }

    #[test]
    fn an_offset_is_written_as_six_characters_whatever_it_holds() {
        assert_eq!(format_offset(FixedOffset::east_opt(2 * 3600).unwrap()), "+02:00");
        assert_eq!(format_offset(FixedOffset::west_opt(5 * 3600 + 1800).unwrap()), "-05:30");
        assert_eq!(format_offset(FixedOffset::east_opt(0).unwrap()), "+00:00");
        // A sub-minute offset is where `FixedOffset`'s own `Display` grows a seconds field.
        assert_eq!(format_offset(FixedOffset::east_opt(3600 + 30).unwrap()), "+01:00");
    }

    #[test]
    fn an_offset_round_trips_through_the_form_exif_spells_it_in() {
        assert_eq!(parse_offset("+02:00").map(|offset| offset.local_minus_utc()), Some(7200));
        assert_eq!(parse_offset("-05:30").map(|offset| offset.local_minus_utc()), Some(-19800));
        assert_eq!(parse_offset("+00:00").map(|offset| offset.local_minus_utc()), Some(0));
        assert_eq!(parse_offset("02:00"), None, "the sign is not optional");
        assert_eq!(parse_offset(""), None);
    }

    #[test]
    fn a_coordinate_becomes_unsigned_degrees_minutes_and_seconds() {
        // 48.858844 -> 48° 51' 31.8384"
        let parts = sexagesimal(48.858_844);
        assert_eq!((parts[0].nominator, parts[0].denominator), (48, 1));
        assert_eq!((parts[1].nominator, parts[1].denominator), (51, 1));
        assert_eq!((parts[2].nominator, parts[2].denominator), (318_384, 10_000));

        // The sign lives in the reference tag, so the magnitude is identical either way.
        assert_eq!(sexagesimal(-48.858_844), parts);
    }
}
