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
//! Nothing here is a `serde` type. The schema layer owns the wire; this layer owns meaning.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: expected {}, got {:?}", self.field, self.kind.expected(), self.value)
    }
}

impl Error for ParseError {}

// ---- validated primitives ----

/// A UTC instant as the export spells it, with every component inside its calendar range.
///
/// Field order makes the derived `Ord` chronological.
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

        // Day is range-checked, not calendar-checked: 2021-02-30 parses. A real calendar lands
        // with the date crate the phase-2 tz work brings in; nothing downstream does arithmetic
        // on these yet.
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
        let head = text.get(..Self::LABEL.len())?;
        if !head.eq_ignore_ascii_case(Self::LABEL) {
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

fn optional_location(field: Field, raw: &str) -> Result<Option<LocationPoint>, ParseError> {
    if raw.is_empty() {
        return Ok(None);
    }
    LocationPoint::parse(field, raw).map(Some)
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
                    Ok(Memory {
                        date: optional_timestamp(Field::Date, &entry.date)?,
                        media_type: MediaKind::from_wire(&entry.media_type),
                        location: optional_location(Field::Location, &entry.location)?,
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
    /// The observed values are 13 digits and agree with `Created` to the second.
    pub created_epoch_ms: i64,
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
    /// Milliseconds since the unix epoch; see [`ChatMessage::created_epoch_ms`].
    pub created_epoch_ms: i64,
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
            created_epoch_ms: raw.created_epoch,
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
            created_epoch_ms: raw.created_epoch,
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
