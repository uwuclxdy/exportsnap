//! Validated domain types built from [`crate::export::schema`].
//!
//! The boundary rule: a value is parsed into a type that carries its invariant once, here, and
//! every later caller reads the type instead of re-checking a string. A [`Timestamp`] is a real
//! calendar-shaped instant, a [`LocationPoint`] is inside the coordinate ranges, a [`Username`]
//! cannot be passed where a [`ConversationId`] belongs.
//!
//! Empty strings are absence. Snapchat writes `""` for a field it has no value for, so `""`
//! becomes `None` and only a non-empty value that fails to parse is a [`ParseError`]. That keeps
//! one missing timestamp in a 3000-row history from failing the whole load while a corrupt one
//! still surfaces loudly.
//!
//! **A numeric field spells the same absence as `0`**, and `Created(microseconds)` is the one that
//! does: see [`ChatMessage::created_epoch_ms`]. The schema layer distinguishes an absent key from a
//! present zero; this layer reads both as absence, so no consumer has to reinvent the rule and none
//! of them can disagree about it.
//!
//! Nothing here is a `serde` type. The schema layer owns the wire; this layer owns meaning.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use chrono::{DateTime, Datelike, Timelike};

use crate::export::schema;

// ---- errors ----

/// A well-formed json value this parser cannot turn into a domain type: bad input, not a bug.
/// Genuine wiring failures (a missing or unreadable file, malformed json) are
/// [`crate::export::LoadError`] instead, so a caller can tell "your export has a broken row"
/// apart from "your export did not arrive".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    field: Field,
    kind: ParseErrorKind,
    value: String,
}

/// The schema keys this module parses out of a string, and the only keys a [`ParseError`] can
/// name.
///
/// A closed set rather than a `&str` because [`ParseError`] carries the offending value into
/// [`crate::export::LoadError`]'s `Display`, and from there into anything that logs it. Every
/// variant below is metadata — a timestamp or a coordinate pair — so a caller cannot point this
/// machinery at `Content` or a download url without first adding a variant for it, which is the
/// review point where that stops being an accident. It constrains the KEY, not the text handed
/// alongside it; parsing message content as a timestamp is still expressible, just no longer
/// nameable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    ApproximateLastSeen,
    Created,
    CreationDate,
    CreationTime,
    CreationTimestamp,
    Date,
    LastModifiedTimestamp,
    Location,
    RequestTime,
    StartTime,
}

impl Field {
    /// The key as Snapchat spells it on the wire.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::ApproximateLastSeen => "Approximate Last Seen",
            Self::Created => "Created",
            Self::CreationDate => "Creation Date",
            Self::CreationTime => "Creation Time",
            Self::CreationTimestamp => "Creation Timestamp",
            Self::Date => "Date",
            Self::LastModifiedTimestamp => "Last Modified Timestamp",
            Self::Location => "Location",
            Self::RequestTime => "Request Time",
            Self::StartTime => "Start Time",
        }
    }
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// What the value should have been.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ParseErrorKind {
    Timestamp,
    Coordinates,
}

impl ParseErrorKind {
    const fn expected(self) -> &'static str {
        match self {
            Self::Timestamp => "a \"YYYY-MM-DD HH:MM:SS UTC\" timestamp",
            Self::Coordinates => "\"Latitude, Longitude: <lat>, <lon>\" with lat in -90..=90 and lon in -180..=180",
        }
    }
}

impl ParseError {
    fn new(field: Field, kind: ParseErrorKind, value: &str) -> Self {
        Self { field, kind, value: value.to_owned() }
    }

    /// The schema key the bad value came from.
    #[must_use]
    pub const fn field(&self) -> Field {
        self.field
    }

    #[must_use]
    pub const fn kind(&self) -> ParseErrorKind {
        self.kind
    }

    /// The value as it appeared on disk. [`Field`] is what keeps this to metadata.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for ParseError {
    /// Names the field and what was expected, and **never the value that was not it**.
    ///
    /// This string reaches a footer alert through [`crate::export::LoadError::Invalid`], and
    /// [`Field`] admits `Location` — so the value it used to render with `{:?}` could be a
    /// coordinate. The restriction on [`Field`] is deliberate and keeps a message body out; it was
    /// never a decision that a lat/long belongs on a terminal.
    ///
    /// **Not done with `crate::export::strip_delimited`, and the difference is not stylistic.** That
    /// function scans a rendered message for delimited runs because serde's message is not ours to
    /// change. This one IS ours, so the value simply never goes in — nothing to scan, nothing to
    /// keep true across a dependency bump. Running the scan here would also destroy the diagnostic:
    /// [`ParseErrorKind::expected`] returns quoted text in both variants, so a delimiter pass turns
    /// `expected a "YYYY-MM-DD HH:MM:SS UTC" timestamp` into `expected a timestamp` and strips the
    /// coordinate form out of the other one entirely. The redaction would eat exactly the half a
    /// user needs.
    ///
    /// **The length stays, and it is not decoration.** Unlike the serde arm — which keeps
    /// `at line N column M`, so a redacted message still says where to look — this one carries no
    /// offset of any kind: [`crate::export::LoadError::Invalid`] renders `{file}: {source}` and
    /// `ParseError` holds no record index. Drop the value outright and an empty string, a
    /// valid-looking ISO-8601 date and 400 KB of garbage all produce the identical sentence, which
    /// is the diagnosis gone rather than trimmed for a tool whose job is reporting schema drift.
    /// A character count separates all three and cannot carry a coordinate: `-33.8688, 151.2093`
    /// and `0.0, 0.0` are both just a number of characters. Shape-over-value is this project's own
    /// idiom, written into `docs/handoff-state.md` after a real privacy breach.
    ///
    /// **Length alone, deliberately — and it diagnoses unevenly across the two kinds, which is worth
    /// naming rather than claiming it is enough everywhere.**
    ///
    /// [`ParseErrorKind::Timestamp`] degrades well: the expected form is 23 characters and the
    /// likeliest drift, ISO-8601 `2021-03-04T14:30:05Z`, is 20, so the length separates the common
    /// case on its own. A zone-label change (`… GMT`) is also 23, but "right length, wrong content"
    /// is itself a signal.
    ///
    /// [`ParseErrorKind::Coordinates`] does not. `51,5074, -0,1278` (locale decimal comma) and
    /// `51.5074; -0.1278` (separator change) are both 16 characters — and so is the well-formed
    /// `51.5074, -0.1278`. Those are the two likeliest coordinate drifts, they need opposite fixes,
    /// and the length tells them apart from each other and from success not at all.
    ///
    /// Left as is on purpose. A charclass ("digits and punctuation") would separate them and still
    /// leak nothing, being identical for every coordinate on earth — but it is a wider claim than
    /// the one that was decided, and widening a redaction's output by an implementer's judgement is
    /// how it grows back toward the value it removed. Recorded so whoever hits a coordinate drift
    /// knows why their message is thin, and what the fix would be.
    ///
    /// [`ParseError::value`] still carries the value for a caller with somewhere safe to put it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let chars = self.value.chars().count();
        let unit = if chars == 1 { "char" } else { "chars" };
        write!(f, "{}: expected {}, got {chars} {unit}", self.field, self.kind.expected())
    }
}

impl Error for ParseError {}

// ---- validated primitives ----

/// A UTC instant as the export spells it, with every component inside its own range.
///
/// Field order makes the derived `Ord` chronological.
///
/// **The two constructors do not carry the same guarantee.** [`Self::parse`] is range-checked only,
/// so `2021-02-30` parses; [`Self::from_epoch_ms`] builds through a date crate and cannot produce a
/// day that does not exist. Nothing depends on the asymmetry today, which is exactly why it is
/// written here rather than left to be discovered: treat the weaker of the two as the type's, and a
/// caller handing one to a date crate still converts fallibly whichever built it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl Timestamp {
    /// Parses `"YYYY-MM-DD HH:MM:SS UTC"`, the only form the observed export uses. A trailing
    /// zone other than `UTC` is rejected rather than assumed: silently reading an offset
    /// timestamp as UTC would misfile every memory by hours.
    ///
    /// `field` names the schema key for the error message.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] when the text is not that exact shape or a component is out of
    /// range.
    pub fn parse(field: Field, text: &str) -> Result<Self, ParseError> {
        Self::try_parse(text).ok_or_else(|| ParseError::new(field, ParseErrorKind::Timestamp, text))
    }

    fn try_parse(text: &str) -> Option<Self> {
        let (date, rest) = text.split_once(' ')?;
        let (time, zone) = rest.split_once(' ')?;
        if zone != "UTC" {
            return None;
        }
        let (year, month, day) = split_three(date, '-')?;
        let (hour, minute, second) = split_three(time, ':')?;

        // Day is range-checked, not calendar-checked: 2021-02-30 parses. The date crate arrived and
        // downstream does do arithmetic on these — `local_fix::calendar` is where one meets a real
        // calendar, and it converts fallibly for exactly this reason, with `output_path` and
        // `system_time` spending the result. So the ceiling is live rather than deferred.
        let parsed = Self {
            year: fixed_width(year, 4)?,
            month: in_range(fixed_width(month, 2)?, 1, 12)?,
            day: in_range(fixed_width(day, 2)?, 1, 31)?,
            hour: in_range(fixed_width(hour, 2)?, 0, 23)?,
            minute: in_range(fixed_width(minute, 2)?, 0, 59)?,
            second: in_range(fixed_width(second, 2)?, 0, 59)?,
        };
        Some(parsed)
    }

    /// The UTC instant `ms` milliseconds after the unix epoch, truncated to the second, or `None`
    /// when it names no instant this type can hold.
    ///
    /// The one caller is the chat-media join reading `Created(microseconds)`, whose values are
    /// milliseconds despite the key (`docs/design.md`). Fallible for two reasons, both reachable
    /// from untrusted json: chrono itself refuses a value outside its representable range, and
    /// [`Self::year`] is a `u16`, so a negative or five-digit year has no [`Timestamp`] even where
    /// chrono has a date. Sub-second precision is DROPPED rather than rounded — every consumer of
    /// this type reduces to whole seconds anyway ([`crate::export::local_fix`]'s writer), and
    /// rounding would put a file one second after the instant its own record states.
    ///
    /// The result is calendar-checked, unlike [`Self::parse`]'s — a stronger guarantee than the type
    /// carries, so nothing may rely on it. See [`Timestamp`] itself for why that asymmetry is
    /// written down.
    ///
    /// # Examples
    ///
    /// ```
    /// use exportsnap::export::model::Timestamp;
    ///
    /// let stamp = Timestamp::from_epoch_ms(1_595_778_485_675).unwrap();
    /// assert_eq!(stamp.to_string(), "2020-07-26 15:48:05 UTC");
    /// assert_eq!(Timestamp::from_epoch_ms(i64::MIN), None);
    /// ```
    #[must_use]
    pub fn from_epoch_ms(ms: i64) -> Option<Self> {
        let utc = DateTime::from_timestamp_millis(ms)?;
        let parsed = Self {
            year: u16::try_from(utc.year()).ok()?,
            month: u8::try_from(utc.month()).ok()?,
            day: u8::try_from(utc.day()).ok()?,
            hour: u8::try_from(utc.hour()).ok()?,
            minute: u8::try_from(utc.minute()).ok()?,
            second: u8::try_from(utc.second()).ok()?,
        };
        Some(parsed)
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

    #[must_use]
    pub const fn hour(self) -> u8 {
        self.hour
    }

    #[must_use]
    pub const fn minute(self) -> u8 {
        self.minute
    }

    #[must_use]
    pub const fn second(self) -> u8 {
        self.second
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { year, month, day, hour, minute, second } = self;
        write!(f, "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
    }
}

/// Splits on the first two `sep` occurrences. A leftover separator in the third part needs no
/// check here, and width is not what catches it — `"1-"` is two characters wide. What rejects it
/// is [`fixed_width`]'s ascii-digit check, since neither `-` nor `:` is a digit. Relaxing that
/// check reopens this hole.
pub(crate) fn split_three(text: &str, sep: char) -> Option<(&str, &str, &str)> {
    let (first, rest) = text.split_once(sep)?;
    let (second, third) = rest.split_once(sep)?;
    Some((first, second, third))
}

/// Parses exactly `width` ascii digits. The digit check is what rejects `"+1"`, `"-1"` and
/// leading whitespace, all of which `str::parse` would otherwise accept.
pub(crate) fn fixed_width<T: std::str::FromStr>(text: &str, width: usize) -> Option<T> {
    if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

pub(crate) fn in_range<T: PartialOrd>(value: T, low: T, high: T) -> Option<T> {
    (value >= low && value <= high).then_some(value)
}

/// A WGS84 coordinate pair inside the valid latitude and longitude ranges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocationPoint {
    latitude: f64,
    longitude: f64,
}

impl LocationPoint {
    const LABEL: &'static str = "latitude, longitude:";

    /// Whether `text` is shaped like a coordinate: the `latitude, longitude:` label, matched
    /// case-insensitively. The one spelling of the shape check, shared by [`Self::try_parse`] and
    /// the memories model's `Location` split (decision 76), so a string can never be a place name
    /// under one and a coordinate under the other.
    pub(crate) fn shaped(text: &str) -> bool {
        text.get(..Self::LABEL.len()).is_some_and(|head| head.eq_ignore_ascii_case(Self::LABEL))
    }

    /// Parses the export's `"Latitude, Longitude: <lat>, <lon>"` form. The label is matched
    /// case-insensitively; both numbers must be finite and in range.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] when the text is not that shape or a coordinate is out of range.
    pub fn parse(field: Field, text: &str) -> Result<Self, ParseError> {
        Self::try_parse(text).ok_or_else(|| ParseError::new(field, ParseErrorKind::Coordinates, text))
    }

    fn try_parse(text: &str) -> Option<Self> {
        if !Self::shaped(text) {
            return None;
        }
        let (latitude, longitude) = text[Self::LABEL.len()..].split_once(',')?;
        // A NaN fails both comparisons, so the range checks reject it without a separate guard.
        Some(Self {
            latitude: in_range(latitude.trim().parse().ok()?, -90.0, 90.0)?,
            longitude: in_range(longitude.trim().parse().ok()?, -180.0, 180.0)?,
        })
    }

    #[must_use]
    pub const fn latitude(self) -> f64 {
        self.latitude
    }

    #[must_use]
    pub const fn longitude(self) -> f64 {
        self.longitude
    }
}

/// A Snapchat account handle, never empty. Distinct from [`ConversationId`] so the two cannot be
/// swapped at a call site: phase 3 matches chat media to history by joining on exactly these.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Username(String);

impl Username {
    /// `None` for `""`. An empty handle is a join key that matches nothing useful yet is
    /// indistinguishable from a real one, so it never becomes a `Username` at all — the module's
    /// empty-is-absence rule, enforced by the constructor rather than by every call site
    /// remembering it.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        (!raw.is_empty()).then_some(Self(raw))
    }
}

/// A conversation key: a friend's username for a one-to-one thread, a uuid for a group.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConversationId(String);

impl ConversationId {
    /// Empty is accepted here, unlike [`Username::new`], because the two failures are opposite.
    /// An empty username is a join key: it matches nothing useful and cannot be told apart from a
    /// real handle. An empty conversation id is an opaque map key, and the thread behind it still
    /// holds its records — refusing it would discard them, not clean anything up.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
}

macro_rules! string_newtype {
    ($name:ident) => {
        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_newtype!(Username);
string_newtype!(ConversationId);

/// Where a file came from: who sent it, and which thread it arrived in.
///
/// Decision 44c puts both of these into the output file's own metadata and into **nothing else** —
/// no filename prefix — so a file a message named and a file none did keep one filename shape. The
/// two stamping legs read it: [`crate::export::exif::Stamp`] and
/// [`crate::export::video::VideoStamp`] each carry one, and each says which tag takes which half.
///
/// Held as the two model types rather than as strings, so neither can be passed where the other
/// belongs — the same reason [`Username`] and [`ConversationId`] are separate types at all. Both
/// halves are absent on the memories leg, which has no sender and no thread.
///
/// **Deliberately unbounded here, and the bound lives at each sink instead.** The length that
/// matters is JPEG's: the APP1 segment carries a 16-bit length and `little_exif` does not enforce it
/// (see [`crate::export::exif::Jpeg::stamp`]). MP4 has no equivalent — an `ilst` atom's size is
/// 32-bit with a 64-bit extension — so capping on this type would apply a JPEG ceiling to `©ART` and
/// `©alb`, shortening a tag the video leg has no reason to shorten, before either stamper could
/// report it. It would also make this type's own documentation stop being true: a truncated
/// [`Self::conversation`] is no longer the export's key, and nothing downstream could tell the key
/// from a prefix of it. [`Username::new`] and [`crate::export::manifest::ExportId::new`] are
/// validating constructors that **reject**; a truncating one is a different thing wearing the same
/// shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    /// Who sent it, as the message spells `From`, and `None` where the export wrote none.
    ///
    /// Carried without [`crate::export::chat_media::Message::is_sender`] beside it, because nothing
    /// observed establishes what `From` holds on a row the account owner sent; a build that filled
    /// this in from the direction flag would be attributing off an inference it cannot check.
    pub sender: Option<Username>,
    /// The export's own conversation key — a friend's handle for a one-to-one thread, a uuid for a
    /// group — and `None` for the empty key the export writes for a thread it names no key for.
    ///
    /// The key rather than the thread's human `Conversation Title`, for the same reason decision 44a
    /// names a directory after the key: a title is written per message, so a group renamed mid-thread
    /// carries two of them under one key and neither of the two is the thread's identity.
    pub conversation: Option<ConversationId>,
}

/// The `Media Type` word, kept open because the observed export is n=1.
///
/// **Matching is ascii-case-insensitive, and the three files carrying this key are why.**
/// `chat_history.json` and `snap_history.json` shout their words (`TEXT`, `MEDIA`, `IMAGE`,
/// `VIDEO`), while `memories_history.json` writes `Image` and `Video` in title case. A
/// case-sensitive match sent every memory entry to [`Self::Other`], which is precisely the field
/// [`crate::export::memories`] keys its day-and-kind buckets off.
///
/// `Other` is not a fallback for a word this parser should have known: it is the honest carrier for
/// one no observed export enumerates. `chat_history.json` alone carries `SHARE` and a whole
/// `STATUS…` family (`STATUSSAVETOCAMERAROLL`, `STATUSPARTICIPANTADDED`, `STATUSERASEDSNAPMESSAGE`,
/// `STATUSNAMECHANGED`). Those are distinct words rather than spellings of [`Self::Status`] — the
/// comparison is against the whole word, never a prefix — so each lands in `Other` carrying its own
/// text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MediaKind {
    Text,
    Media,
    Status,
    Note,
    Sticker,
    Image,
    Video,
    Other(String),
}

impl MediaKind {
    /// Every word this parser places, each spelled as [`Self::as_wire`] gives it back.
    ///
    /// A short `KNOWN` fails SILENTLY, not loudly. `as_wire` below is an exhaustive match, so
    /// adding a variant without an arm there is a compile error (`E0004`) the author must answer —
    /// but answering it does not extend this array, and nothing else forces that second edit.
    /// `from_wire` falls back to `Self::Other(raw)` on no match, so a variant added and left out of
    /// `KNOWN` never surfaces as a parse failure: the word it should have matched just lands in
    /// `Other`, which is the wrong bucket key for `memories`' day-and-kind join. This is the same
    /// residual `ItemKind::ALL`/`ItemStatus::ALL` (`manifest.rs`) and `SummaryRow::ALL`
    /// (`tui/screens/overview.rs`) carry — no exhaustive match anywhere can catch an array staying
    /// short, only catch a variant being used without one — and it is worse here than at
    /// `ItemStatus`, which at least fails loudly as `CorruptRow` on the identical omission.
    const KNOWN: [Self; 7] = [Self::Text, Self::Media, Self::Status, Self::Note, Self::Sticker, Self::Image, Self::Video];

    /// The variant for `raw`, matched without regard to ascii case; see the type's docs for why.
    #[must_use]
    pub fn from_wire(raw: &str) -> Self {
        Self::KNOWN.into_iter().find(|known| raw.eq_ignore_ascii_case(known.as_wire())).unwrap_or_else(|| Self::Other(raw.to_owned()))
    }

    /// The word in the canonical shouted spelling.
    ///
    /// Not a round trip of [`Self::from_wire`]'s input for a word this parser places:
    /// `from_wire("Image").as_wire()` is `"IMAGE"`. Only [`Self::Other`] hands back the original
    /// text, because there the spelling is the whole of what is known.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Text => "TEXT",
            Self::Media => "MEDIA",
            Self::Status => "STATUS",
            Self::Note => "NOTE",
            Self::Sticker => "STICKER",
            Self::Image => "IMAGE",
            Self::Video => "VIDEO",
            Self::Other(raw) => raw,
        }
    }
}

/// A signed memory download url.
///
/// These are secrets with a ~7-day life (`docs/design.md`, privacy invariants): never logged,
/// never printed. `Debug` is hand-written so a `{:?}` on any struct holding one — a panic
/// message, a trace line — cannot leak it, and reading the url takes the deliberately awkward
/// [`Self::expose`]. There is no `Display` on purpose.
#[derive(Clone, PartialEq, Eq)]
pub struct DownloadUrl(String);

impl DownloadUrl {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DownloadUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DownloadUrl(<redacted>)")
    }
}

/// The body of a chat message.
///
/// Message text is never rendered in the TUI and reaches disk only through an explicit
/// user-triggered export (`docs/design.md`, decision 3). Same treatment as [`DownloadUrl`]: a
/// redacting `Debug`, no `Display`, and [`Self::expose`] to read it.
#[derive(Clone, PartialEq, Eq)]
pub struct MessageText(String);

impl MessageText {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MessageText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MessageText(<redacted>)")
    }
}

// ---- shared conversion helpers ----

/// `""` is how the export spells "no value here"; see the module docs.
fn optional_text(raw: String) -> Option<String> {
    (!raw.is_empty()).then_some(raw)
}

fn optional_timestamp(field: Field, raw: &str) -> Result<Option<Timestamp>, ParseError> {
    if raw.is_empty() {
        return Ok(None);
    }
    Timestamp::parse(field, raw).map(Some)
}

/// `0` is how an integer field spells "no value here", the way `""` does for a string.
///
/// **A claim about the ENCODING, not about the calendar**, and the distinction is what keeps the
/// rule from growing. [`optional_text`] and [`optional_timestamp`] above are the siblings: each
/// reads its own type's empty spelling as absence, and none of them judges whether the value that
/// survives is a plausible date. Nothing in this crate applies a plausibility floor to any date
/// source — [`Timestamp::parse`] honours `"1900-01-01 00:00:00 UTC"`, and
/// [`crate::export::local_fix`]'s `system_time` exists to write mtimes on both sides of the epoch —
/// so the epoch field does not get one either. The harm being prevented is narrow and structural:
/// reading `0` as an instant promotes a MISSING field into a stated date that then outranks every
/// weaker source below it. The `None` arm folds the absent key into the same answer; only the
/// schema layer keeps the two apart.
///
/// A negative value therefore passes, and needs no argument from what the export has been observed
/// to hold: it is not an empty spelling, so the rule does not reach it. Widening to `<= 0` would
/// make this integer a plausibility filter while the `Created` string beside it stays none — that
/// is the inconsistency, not the fix.
///
/// **The sentinel catches the spelling, not the instant, and those are different sets.** `1..=999`
/// are all sub-second, so [`Timestamp::from_epoch_ms`] truncates every one of them to the same
/// `1970-01-01 00:00:00` this arm refuses, and every one of them passes. Correct under the frame
/// above, and worth knowing before someone reads the two as one rule and "fixes" the gap.
const fn optional_epoch_ms(raw: Option<i64>) -> Option<i64> {
    match raw {
        Some(0) | None => None,
        Some(ms) => Some(ms),
    }
}

/// Splits the `Location` field under decision 76: a coordinate-shaped string keeps the strict
/// coordinate parse — an invalid coordinate still fails the load — any other non-empty string is
/// a place name held verbatim, and the empty string is absent in both. The halves are mutually
/// exclusive because [`LocationPoint::shaped`] is the one spelling of the shape check, shared
/// with the parse itself.
fn split_location(field: Field, raw: &str) -> Result<(Option<LocationPoint>, Option<String>), ParseError> {
    if raw.is_empty() {
        return Ok((None, None));
    }
    if LocationPoint::shaped(raw) {
        return Ok((Some(LocationPoint::parse(field, raw)?), None));
    }
    Ok((None, Some(raw.to_owned())))
}

// ---- account.json ----

#[derive(Debug, Clone)]
pub struct Account {
    pub basics: AccountBasics,
    pub device: DeviceInfo,
    pub device_history: Vec<DeviceUse>,
    pub logins: Vec<Login>,
    pub associated_accounts: Vec<AssociatedAccount>,
}

#[derive(Debug, Clone)]
pub struct AccountBasics {
    pub username: Option<Username>,
    pub name: Option<String>,
    pub created: Option<Timestamp>,
    pub registration_ip: Option<String>,
    pub country: Option<String>,
    /// Free-form in the observed export, not a timestamp.
    pub last_active: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub make: Option<String>,
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub language: Option<String>,
    pub os_type: Option<String>,
    pub os_version: Option<String>,
    pub connection_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceUse {
    pub make: Option<String>,
    pub model: Option<String>,
    pub started: Option<Timestamp>,
    pub device_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Login {
    pub ip: Option<String>,
    pub country: Option<String>,
    pub created: Option<Timestamp>,
    pub status: Option<String>,
    pub device: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AssociatedAccount {
    pub device_id: Option<String>,
    pub user_id: Option<String>,
    pub requested: Option<Timestamp>,
    pub last_seen: Option<Timestamp>,
}

impl TryFrom<schema::Account> for Account {
    type Error = ParseError;

    fn try_from(raw: schema::Account) -> Result<Self, Self::Error> {
        let basics = AccountBasics {
            username: Username::new(raw.basic_information.username),
            name: optional_text(raw.basic_information.name),
            created: optional_timestamp(Field::CreationDate, &raw.basic_information.creation_date)?,
            registration_ip: optional_text(raw.basic_information.registration_ip),
            country: optional_text(raw.basic_information.country),
            last_active: optional_text(raw.basic_information.last_active),
        };
        let device = DeviceInfo {
            make: optional_text(raw.device_information.make),
            model_id: optional_text(raw.device_information.model_id),
            model_name: optional_text(raw.device_information.model_name),
            language: optional_text(raw.device_information.language),
            os_type: optional_text(raw.device_information.os_type),
            os_version: optional_text(raw.device_information.os_version),
            connection_type: optional_text(raw.device_information.connection_type),
        };
        let device_history = raw
            .device_history
            .into_iter()
            .map(|entry| {
                Ok(DeviceUse {
                    make: optional_text(entry.make),
                    model: optional_text(entry.model),
                    started: optional_timestamp(Field::StartTime, &entry.start_time)?,
                    device_type: optional_text(entry.device_type),
                })
            })
            .collect::<Result<Vec<_>, ParseError>>()?;
        let logins = raw
            .login_history
            .into_iter()
            .map(|entry| {
                Ok(Login {
                    ip: optional_text(entry.ip),
                    country: optional_text(entry.country),
                    created: optional_timestamp(Field::Created, &entry.created)?,
                    status: optional_text(entry.status),
                    device: optional_text(entry.device),
                })
            })
            .collect::<Result<Vec<_>, ParseError>>()?;
        let associated_accounts = raw
            .associated_accounts
            .into_iter()
            .map(|entry| {
                Ok(AssociatedAccount {
                    device_id: optional_text(entry.device_id),
                    user_id: optional_text(entry.user_id),
                    requested: optional_timestamp(Field::RequestTime, &entry.request_time)?,
                    last_seen: optional_timestamp(Field::ApproximateLastSeen, &entry.approximate_last_seen)?,
                })
            })
            .collect::<Result<Vec<_>, ParseError>>()?;

        Ok(Self { basics, device, device_history, logins, associated_accounts })
    }
}

// ---- user_profile.json ----

#[derive(Debug, Clone)]
pub struct UserProfile {
    pub country: Option<String>,
    pub created: Option<Timestamp>,
    pub account_creation_country: Option<String>,
    pub platform_version: Option<String>,
    pub in_app_language: Option<String>,
    pub cohort_age: Option<String>,
    pub derived_ad_demographic: Option<String>,
    pub engagement: Vec<EngagementEvent>,
    pub time_spent_breakdown: Vec<String>,
    pub web_interactions: Vec<String>,
    pub mobile_ad_id: Option<String>,
    /// The count of `Subscriptions` entries (`docs/design.md`: that section earns a typed model
    /// when the account screen lands). The one observed export holds an empty list, so the
    /// element shape is unobserved, and the empty-section rule forbids guessing one — a
    /// validated count is the whole typed model.
    pub subscriptions: usize,
}

#[derive(Debug, Clone)]
pub struct EngagementEvent {
    pub event: String,
    pub occurrences: u64,
}

impl TryFrom<schema::UserProfile> for UserProfile {
    type Error = ParseError;

    fn try_from(raw: schema::UserProfile) -> Result<Self, Self::Error> {
        Ok(Self {
            country: optional_text(raw.app_profile.country),
            created: optional_timestamp(Field::CreationTime, &raw.app_profile.creation_time)?,
            account_creation_country: optional_text(raw.app_profile.account_creation_country),
            platform_version: optional_text(raw.app_profile.platform_version),
            in_app_language: optional_text(raw.app_profile.in_app_language),
            cohort_age: optional_text(raw.demographics.cohort_age),
            derived_ad_demographic: optional_text(raw.demographics.derived_ad_demographic),
            engagement: raw
                .engagement
                .into_iter()
                .map(|entry| EngagementEvent { event: entry.event, occurrences: entry.occurrences })
                .collect(),
            time_spent_breakdown: raw.time_spent_breakdown,
            web_interactions: raw.interactions.web,
            mobile_ad_id: optional_text(raw.mobile_ad_id),
            subscriptions: raw.subscriptions.len(),
        })
    }
}

// ---- friends.json ----

#[derive(Debug, Clone)]
pub struct Friends {
    pub friends: Vec<Friend>,
    pub requests_sent: Vec<Friend>,
    pub blocked: Vec<Friend>,
    pub deleted: Vec<Friend>,
    pub hidden_suggestions: Vec<Friend>,
    pub ignored: Vec<Friend>,
    pub pending_requests: Vec<Friend>,
    pub shortcuts: Vec<Shortcut>,
}

#[derive(Debug, Clone)]
pub struct Friend {
    pub username: Option<Username>,
    pub display_name: Option<String>,
    pub created: Option<Timestamp>,
    pub last_modified: Option<Timestamp>,
    pub source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Shortcut {
    pub name: Option<String>,
    pub created: Option<Timestamp>,
}

impl TryFrom<schema::FriendEntry> for Friend {
    type Error = ParseError;

    fn try_from(raw: schema::FriendEntry) -> Result<Self, Self::Error> {
        Ok(Self {
            username: Username::new(raw.username),
            display_name: optional_text(raw.display_name),
            created: optional_timestamp(Field::CreationTimestamp, &raw.creation_timestamp)?,
            last_modified: optional_timestamp(Field::LastModifiedTimestamp, &raw.last_modified_timestamp)?,
            source: optional_text(raw.source),
        })
    }
}

impl TryFrom<schema::Friends> for Friends {
    type Error = ParseError;

    fn try_from(raw: schema::Friends) -> Result<Self, Self::Error> {
        Ok(Self {
            friends: friend_list(raw.friends)?,
            requests_sent: friend_list(raw.friend_requests_sent)?,
            blocked: friend_list(raw.blocked_users)?,
            deleted: friend_list(raw.deleted_friends)?,
            hidden_suggestions: friend_list(raw.hidden_friend_suggestions)?,
            ignored: friend_list(raw.ignored_snapchatters)?,
            pending_requests: friend_list(raw.pending_requests)?,
            shortcuts: raw
                .shortcuts
                .into_iter()
                .map(|entry| {
                    Ok(Shortcut { name: optional_text(entry.shortcut_name), created: optional_timestamp(Field::Created, &entry.created)? })
                })
                .collect::<Result<Vec<_>, ParseError>>()?,
        })
    }
}

fn friend_list(raw: Vec<schema::FriendEntry>) -> Result<Vec<Friend>, ParseError> {
    raw.into_iter().map(Friend::try_from).collect()
}

// ---- memories_history.json ----

#[derive(Debug, Clone)]
pub struct Memories {
    pub saved_media: Vec<Memory>,
}

#[derive(Debug, Clone)]
pub struct Memory {
    pub date: Option<Timestamp>,
    pub media_type: MediaKind,
    pub location: Option<LocationPoint>,
    /// The entry's `Location` string when it is not a coordinate: a place name the table shows
    /// verbatim (decision 76). Mutually exclusive with [`Self::location`] — a coordinate-shaped
    /// string is never a place name and a place name is never a coordinate.
    pub place_name: Option<String>,
    /// Both urls expire roughly 7 days after the export is cut, which is the race phase 2 runs.
    pub download_link: Option<DownloadUrl>,
    pub media_download_url: Option<DownloadUrl>,
}

impl TryFrom<schema::MemoriesHistory> for Memories {
    type Error = ParseError;

    fn try_from(raw: schema::MemoriesHistory) -> Result<Self, Self::Error> {
        Ok(Self {
            saved_media: raw
                .saved_media
                .into_iter()
                .map(|entry| {
                    let (location, place_name) = split_location(Field::Location, &entry.location)?;
                    Ok(Memory {
                        date: optional_timestamp(Field::Date, &entry.date)?,
                        media_type: MediaKind::from_wire(&entry.media_type),
                        location,
                        place_name,
                        download_link: optional_text(entry.download_link).map(DownloadUrl::new),
                        media_download_url: optional_text(entry.media_download_url).map(DownloadUrl::new),
                    })
                })
                .collect::<Result<Vec<_>, ParseError>>()?,
        })
    }
}

// ---- chat_history.json / snap_history.json ----

/// One thread's worth of records, in the order the export listed them.
#[derive(Debug, Clone)]
pub struct Conversation<T> {
    pub id: ConversationId,
    pub records: Vec<T>,
}

#[derive(Debug, Clone)]
pub struct ChatHistory {
    /// Sorted by [`ConversationId`], because the wire form is a json object and object key order
    /// is not a thing a parser can rely on.
    pub conversations: Vec<Conversation<ChatMessage>>,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub from: Option<Username>,
    pub media_type: MediaKind,
    pub created: Option<Timestamp>,
    /// Milliseconds since the unix epoch, despite the `Created(microseconds)` key it comes from.
    /// The observed values are 13 digits and agree with [`Self::created`] to the second.
    ///
    /// `None` covers both spellings of absence — the key missing and the key holding `0` — per
    /// [`optional_epoch_ms`], so a consumer reading `Some` has a stated instant and not a default.
    /// The raw `i64` is kept rather than a [`Timestamp`] because this is the wire fact; turning it
    /// into an instant is [`Timestamp::from_epoch_ms`]'s job and is fallible.
    pub created_epoch_ms: Option<i64>,
    pub content: Option<MessageText>,
    pub conversation_title: Option<String>,
    pub is_sender: bool,
    pub is_saved: bool,
    /// Held verbatim. Some rows carry more than one id and the delimiter Snapchat uses has not
    /// been observed, so splitting here would be a guess phase 3 then has to unpick.
    pub media_ids: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SnapHistory {
    /// Sorted by [`ConversationId`], and the ids join with [`ChatHistory::conversations`].
    pub conversations: Vec<Conversation<Snap>>,
}

#[derive(Debug, Clone)]
pub struct Snap {
    pub from: Option<Username>,
    pub media_type: MediaKind,
    pub created: Option<Timestamp>,
    /// Milliseconds since the unix epoch, `None` for absent or `0`; see
    /// [`ChatMessage::created_epoch_ms`].
    pub created_epoch_ms: Option<i64>,
    pub conversation_title: Option<String>,
    pub is_sender: bool,
}

impl TryFrom<schema::ChatEntry> for ChatMessage {
    type Error = ParseError;

    fn try_from(raw: schema::ChatEntry) -> Result<Self, Self::Error> {
        Ok(Self {
            from: Username::new(raw.from),
            media_type: MediaKind::from_wire(&raw.media_type),
            created: optional_timestamp(Field::Created, &raw.created)?,
            created_epoch_ms: optional_epoch_ms(raw.created_epoch),
            content: raw.content.and_then(optional_text).map(MessageText::new),
            conversation_title: raw.conversation_title.and_then(optional_text),
            is_sender: raw.is_sender,
            is_saved: raw.is_saved,
            media_ids: optional_text(raw.media_ids),
        })
    }
}

impl TryFrom<schema::SnapEntry> for Snap {
    type Error = ParseError;

    fn try_from(raw: schema::SnapEntry) -> Result<Self, Self::Error> {
        Ok(Self {
            from: Username::new(raw.from),
            media_type: MediaKind::from_wire(&raw.media_type),
            created: optional_timestamp(Field::Created, &raw.created)?,
            created_epoch_ms: optional_epoch_ms(raw.created_epoch),
            conversation_title: raw.conversation_title.and_then(optional_text),
            is_sender: raw.is_sender,
        })
    }
}

impl TryFrom<schema::ChatHistory> for ChatHistory {
    type Error = ParseError;

    fn try_from(raw: schema::ChatHistory) -> Result<Self, Self::Error> {
        Ok(Self { conversations: conversations(raw.conversations)? })
    }
}

impl TryFrom<schema::SnapHistory> for SnapHistory {
    type Error = ParseError;

    fn try_from(raw: schema::SnapHistory) -> Result<Self, Self::Error> {
        Ok(Self { conversations: conversations(raw.conversations)? })
    }
}

/// `BTreeMap` iteration is already sorted by key, so the resulting order is deterministic.
fn conversations<S, T>(raw: BTreeMap<String, Vec<S>>) -> Result<Vec<Conversation<T>>, ParseError>
where
    T: TryFrom<S, Error = ParseError>,
{
    raw.into_iter()
        .map(|(id, records)| {
            Ok(Conversation {
                id: ConversationId::new(id),
                records: records.into_iter().map(T::try_from).collect::<Result<Vec<_>, ParseError>>()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_media_kind_survives_its_own_round_trip() {
        // Driven off `KNOWN` itself rather than a second hand-written list. `from_wire` looks a word
        // up BY calling `as_wire` on each `KNOWN` member (see above), so this loop's real failure
        // mode is two `KNOWN` members sharing a wire spelling: `find` returns the first match, so
        // the later member's own value never round-trips back to itself. That also catches a future
        // variant added to `KNOWN` that collides with an existing spelling. It CANNOT catch `KNOWN`
        // going short when a variant is added — the residual documented at `KNOWN` itself, identical
        // to `ItemKind::ALL`/`ItemStatus::ALL` (`manifest.rs`) and `SummaryRow::ALL`
        // (`tui/screens/overview.rs`). `tests/export.rs`'s
        // `media_kind_keeps_the_words_it_knows_and_carries_the_ones_it_does_not` stays alongside
        // this: its hand-written literals are what catches a member being DELETED from `KNOWN`,
        // which this loop cannot (deleting a member just shrinks what this loop iterates over).
        for kind in MediaKind::KNOWN {
            assert_eq!(MediaKind::from_wire(kind.as_wire()), kind, "{kind:?} did not round-trip through as_wire/from_wire");
        }
    }
}
