//! The merged per-conversation timeline (decision 61): [`model::ChatHistory`] and
//! [`model::SnapHistory`] unioned into one time-ordered [`Thread`] per conversation.
//!
//! Snapchat keys `snap_history.json` by conversation exactly as it keys `chat_history.json`, so a
//! snap is a row in the same thread as the chat messages beside it: it carries its kind, its
//! direction and its timestamp, and no body, because a snap holds no text. That absence is
//! STRUCTURAL rather than a `None` spelling — [`Record`] wraps the two model types whole rather
//! than flattening them into one struct, so a [`Record::Snap`] cannot even be asked for a body,
//! and [`Record::content`]/[`Record::media_ids`] return `None` on the snap arm as a consequence
//! of what the type holds, not as a convention every call site has to remember.
//!
//! Nothing here writes a file, renders a row, or knows a screen exists. This is the document model the four phase-4 writers render over (decision 58), and the four writers (json, text, csv, html) live here as pure functions over that document; the `fs::write` that lands one on disk is [`super::history_run`]'s.
//!
//! # Ordering
//!
//! Records within a thread sort by [`Record::resolved_created`] ascending — the SAME resolution
//! [`super::chat_media::ChatMediaItem::date`] applies,
//! `created.or_else(|| created_epoch_ms.and_then(Timestamp::from_epoch_ms))` — so the render
//! stream and the media legs agree about which instant a record happened at (decision 61's "same
//! fixed timestamps the media legs stamp with"). The `Created(microseconds)` absence collapse is
//! NOT re-implemented here: [`model::ChatMessage::created_epoch_ms`] and
//! [`model::Snap::created_epoch_ms`] already read the absent key and the literal `0` both as
//! `None` (`model::optional_epoch_ms`), so this module sorts on the already-collapsed value.
//!
//! The order is TOTAL, and pinned by tests rather than left to a sort's stability:
//! 1. [`Record::resolved_created`], `Some` ascending and every `None` after every `Some`;
//! 2. raw `created_epoch_ms`, `None` before `Some`, ascending;
//! 3. kind, [`RecordKind::Chat`] before [`RecordKind::Snap`];
//! 4. the record's position in the thread's concatenated chat-then-snap list, before sorting.
//!
//! A full total-order key means the result never depends on sort stability or a hash seed.
//!
//! # Privacy
//!
//! [`Record`], [`Thread`], [`MergedHistory`] and [`Document`] derive `Debug` and `Clone` and nothing else. The bodies they wrap keep [`model::MessageText`]'s redacting `Debug`, so a `{:?}` cannot leak a message body, and nothing here derives `Serialize` — the json writer builds a separate serializable mirror inside the call and drops it before returning (decisions 3, 58). That mirror is `Serialize`-only, with no `Debug`: it holds the plain-text bodies, so a `{:?}` on it would leak exactly what [`model::MessageText`]'s redacting `Debug` exists to keep off a terminal. The html writer's [`Html`] holds the whole transcript, so its `Debug` is hand-written to redact the `html` field the way [`model::MessageText`] redacts its body — escaping makes markup inert, not private, and an escaped body is still readable text.
//!
//! # The document model and writers
//!
//! [`Document`] is one conversation's transcript: the merged records of one [`Thread`], kept in the order [`merge`] produced them. The json, text and csv writers are pure functions over it, each byte-stable — the same [`Document`] always renders the same bytes — and free of any `HashMap` iteration, timestamp, or randomness, which is what makes the byte-equality tests meaningful. The html writer is a pure function of the document and an already-opened [`Manifest`] (decision 62): deterministic for a given pair, and the only writer whose output the manifest's state can change.
//!
//! ## json (decision 58's re-import path)
//!
//! A transient [`Serialize`]-only mirror is built inside [`write_json`] and dropped before returning. It wraps the document as `{ "conversation": <key>, "records": [...] }`, one record per merged row, with `None` fields omitted rather than written as `null` (`skip_serializing_if`) — a snap row therefore carries no `content` key at all. `created` is the record's resolved instant ([`Record::resolved_created`]) rendered as a string: the same fixed timestamp the media legs stamp with (decision 61). [`write_json`] returns a `Result` rather than swallowing the `Result` the serializer returns: the mirror holds only `String`s and `bool`s, so the error arm is unreachable in practice, but propagating keeps a future mirror field that CAN fail (a float, a hand-written `Serialize`) loud instead of `""`.
//!
//! ## csv (RFC 4180, decision 58's spreadsheet path)
//!
//! A header row, then one row per record, `\n`-terminated, with absent fields as the empty string. A field containing a comma, a double quote, a newline or a carriage return is wrapped in double quotes and every embedded quote is doubled (`"` → `""`); every other field is written verbatim.
//!
//! ## text (the plain-transcript path)
//!
//! One header line per record — `[<created>] <from> (<kind> <media_type>, <sent|received>)`, with `no date` and `unknown` standing in for an absent instant and sender — then the body with every line prefixed `> `, and a blank line between records. The prefix is what keeps the format unambiguous: a reader treats any line starting with `> ` as the current record's body and every other non-blank line as a new record's header, so a multi-line body (an embedded blank line included) can never be read as two records, and a header always starts with `[`, so it can never be mistaken for a continuation. The body renders through `str::lines()`, which normalizes `\r\n` to `\n` and drops a trailing line break — a display choice, since text is for a person to read rather than a byte-exact round trip. An absent body renders as no body lines at all.
//!
//! ## html (decision 58's "the format a user reads", decision 62's links)
//!
//! A complete, self-contained document a browser opens: a `<!doctype html>` document whose `<title>`
//! is the conversation title where any record carries one and the conversation key otherwise, and one
//! `<article>` per record in the merged order, each carrying the same header line the text writer
//! renders plus the body and the media links. No external assets and no scripts, so the file needs
//! nothing but itself.
//!
//! Every piece of untrusted text is escaped before it is interpolated — the message body, the sender,
//! the conversation title, and the media type, which [`MediaKind::Other`] can spell with arbitrary
//! text — and the controlled-vocabulary pieces (the rendered timestamp, the kind word, the direction)
//! are escaped too, for a uniform rule with no arm to reason about. A body containing markup renders
//! as text, never as markup.
//!
//! A chat record's `Media IDs` splits into tokens with [`crate::export::chat_media::media_tokens`],
//! the same rule `reconcile` joins with, and each token is read against its manifest row
//! (decision 62): a `done` row renders a relative link whose `href` is the bare output filename —
//! the conversation directory is flat (decision 60) and this document sits beside the media — while
//! every other status, and a missing row, renders an inert placeholder naming the message's own
//! `Media Type` and nothing else: no token, no id, no path. The manifest lookup is the authority, and
//! the lookup KEY is the join's own canonical spelling — `chat_media`'s one
//! `parse_history_token`, now shared — so a shouted prefix reaches the row it names; a token outside
//! that grammar keeps its own spelling, has no row, and falls to the placeholder naturally. Nothing
//! is rejected here, only normalized.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;

use serde::Serialize;

use crate::export::chat_media::{media_tokens, parse_history_token};
use crate::export::manifest::{ItemKind, ItemStatus, Manifest, ManifestError};
use crate::export::model;
use crate::export::model::{ConversationId, MediaKind, MessageText, Timestamp, Username};

/// Which of the two sources a merged record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Chat,
    Snap,
}

impl RecordKind {
    /// The lowercase word the four writers use for this kind.
    ///
    /// [`RecordKind`] is this crate's own invention — decision 61 merges two files that never name a kind — so the vocabulary is ours and has no wire form: `chat` for a chat message, `snap` for a snap. Named like [`MediaKind::as_wire`] for consistency with that established idiom.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Snap => "snap",
        }
    }
}

/// One merged record: a chat message or a snap, carrying a uniform view for the render stream.
///
/// Wraps the model types whole rather than flattening them, so a snap's body-absence is
/// structural (see the module docs).
#[derive(Debug, Clone)]
pub enum Record {
    Chat(model::ChatMessage),
    Snap(model::Snap),
}

impl Record {
    /// Which source this record came from.
    #[must_use]
    pub const fn kind(&self) -> RecordKind {
        match self {
            Self::Chat(_) => RecordKind::Chat,
            Self::Snap(_) => RecordKind::Snap,
        }
    }

    /// `From`, as the record spells it, and `None` for the export's empty string.
    ///
    /// Carried beside [`Self::is_sender`] rather than resolved with it into one "who sent this"
    /// answer: nothing observed establishes what `From` holds on a row the account owner sent,
    /// the same reason `chat_media::Message` keeps the two apart.
    #[must_use]
    pub fn from(&self) -> Option<&Username> {
        match self {
            Self::Chat(record) => record.from.as_ref(),
            Self::Snap(record) => record.from.as_ref(),
        }
    }

    /// Whether the account owner sent it.
    #[must_use]
    pub const fn is_sender(&self) -> bool {
        match self {
            Self::Chat(record) => record.is_sender,
            Self::Snap(record) => record.is_sender,
        }
    }

    #[must_use]
    pub fn media_type(&self) -> &MediaKind {
        match self {
            Self::Chat(record) => &record.media_type,
            Self::Snap(record) => &record.media_type,
        }
    }

    /// The record's own `Created` string, already a [`Timestamp`]; `None` where the export wrote
    /// `""`.
    #[must_use]
    pub const fn created(&self) -> Option<Timestamp> {
        match self {
            Self::Chat(record) => record.created,
            Self::Snap(record) => record.created,
        }
    }

    /// `Created(microseconds)`, already collapsed by the model layer: `None` for the absent key
    /// and the literal `0` alike.
    #[must_use]
    pub const fn created_epoch_ms(&self) -> Option<i64> {
        match self {
            Self::Chat(record) => record.created_epoch_ms,
            Self::Snap(record) => record.created_epoch_ms,
        }
    }

    #[must_use]
    pub fn conversation_title(&self) -> Option<&str> {
        match self {
            Self::Chat(record) => record.conversation_title.as_deref(),
            Self::Snap(record) => record.conversation_title.as_deref(),
        }
    }

    /// The resolved sort/display instant: `created`, else `created_epoch_ms` resolved through
    /// [`Timestamp::from_epoch_ms`]. The same resolution the media legs date with.
    #[must_use]
    pub fn resolved_created(&self) -> Option<Timestamp> {
        self.created().or_else(|| self.created_epoch_ms().and_then(Timestamp::from_epoch_ms))
    }

    /// The body of a chat message. Always `None` for a snap, which holds no text at all.
    #[must_use]
    pub fn content(&self) -> Option<&MessageText> {
        match self {
            Self::Chat(record) => record.content.as_ref(),
            Self::Snap(_) => None,
        }
    }

    /// The raw `Media IDs` string. Always `None` for a snap.
    #[must_use]
    pub fn media_ids(&self) -> Option<&str> {
        match self {
            Self::Chat(record) => record.media_ids.as_deref(),
            Self::Snap(_) => None,
        }
    }
}

/// One thread's records, sorted by the ordering rule in the module docs.
#[derive(Debug, Clone)]
pub struct Thread {
    pub id: ConversationId,
    pub records: Vec<Record>,
}

/// All threads, sorted by [`ConversationId`].
#[derive(Debug, Clone)]
pub struct MergedHistory {
    pub threads: Vec<Thread>,
}

/// Unions the two histories into one per-conversation timeline (decision 61).
///
/// A conversation key present in only one source keeps that source's records, sorted by the
/// ordering rule like every other thread — a conversation's export order is not guaranteed
/// chronological, so single-source threads are sorted too. Threads come out sorted by
/// [`ConversationId`]; the two inputs already sort their conversations that way, but the result
/// is produced here rather than trusted to them.
#[must_use]
pub fn merge(chat: &model::ChatHistory, snap: &model::SnapHistory) -> MergedHistory {
    // BTreeMap iteration is sorted by key, so the thread list needs no second sort.
    let mut by_key: BTreeMap<ConversationId, Vec<Record>> = BTreeMap::new();
    for conversation in &chat.conversations {
        by_key.entry(conversation.id.clone()).or_default().extend(conversation.records.iter().cloned().map(Record::Chat));
    }
    for conversation in &snap.conversations {
        by_key.entry(conversation.id.clone()).or_default().extend(conversation.records.iter().cloned().map(Record::Snap));
    }

    let threads = by_key
        .into_iter()
        .map(|(id, records)| {
            // Position is captured BEFORE the sort, so it is the record's place in the
            // concatenated chat-then-snap list and the key is total — no two records share one.
            let mut keyed: Vec<(SortKey, Record)> =
                records.into_iter().enumerate().map(|(position, record)| (SortKey::of(&record, position), record)).collect();
            keyed.sort_by_key(|(key, _)| *key);
            Thread { id, records: keyed.into_iter().map(|(_, record)| record).collect() }
        })
        .collect();
    MergedHistory { threads }
}

/// The total-order key a thread's records sort by; see the module docs for the four steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SortKey {
    /// Whether the record resolves to an instant at all, `Some` first.
    resolved_present: Reverse<bool>,
    /// The resolved instant, ascending; `None` only beside `resolved_present == Reverse(false)`.
    resolved: Option<Timestamp>,
    /// The raw epoch value, ascending with `None` (absent or zero) before `Some`.
    epoch_ms: Option<i64>,
    /// 0 for chat, 1 for snap.
    kind: u8,
    /// Position in the concatenated chat-then-snap list, before sorting.
    position: usize,
}

impl SortKey {
    fn of(record: &Record, position: usize) -> Self {
        let resolved = record.resolved_created();
        Self {
            resolved_present: Reverse(resolved.is_some()),
            resolved,
            epoch_ms: record.created_epoch_ms(),
            kind: match record.kind() {
                RecordKind::Chat => 0,
                RecordKind::Snap => 1,
            },
            position,
        }
    }
}

// ---- the per-conversation document model and its writers (decision 58) ----

/// One conversation's transcript: the conversation's identity and its merged, ordered records.
///
/// Built from a [`Thread`] with [`Self::from_thread`], which moves the thread's records rather than copying them — [`merge`] already ordered them, so the document stores the render stream verbatim and every writer walks the same `Vec<Record>` (decision 58's "one render stream"). The records are reused whole, never flattened or re-wrapped, so the bodies stay [`model::MessageText`] values and the redacting `Debug` survives into the document.
///
/// The transcript omits the `is_saved` bookmark flag: it is a client-side toggle about the account,
/// not part of what was said, and no format here has a consumer for it.
#[derive(Debug, Clone)]
pub struct Document {
    /// The conversation's identity; decision 60 names the output file by it.
    pub key: ConversationId,
    /// The merged records, already sorted by the ordering rule in the module docs.
    pub records: Vec<Record>,
}

impl Document {
    /// The document for one already-merged thread.
    #[must_use]
    pub fn from_thread(thread: Thread) -> Self {
        Self { key: thread.id, records: thread.records }
    }
}

/// The json writer's transient per-record projection.
///
/// [`Serialize`]-only, deliberately: this struct holds the plain-text bodies ([`model::MessageText::expose`]'d) that the redacting `Debug` exists to keep off a terminal, so it must never gain a `Debug` — a `{:?}` here would leak a body the way [`model::MessageText`]'s own `Debug` prevents. The `None` fields are omitted rather than written as `null` (`skip_serializing_if`), so a snap row carries no `content` key at all; see [`write_json`].
#[derive(Serialize)]
struct RecordJson<'a> {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<&'a str>,
    is_sender: bool,
    media_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_ids: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_title: Option<&'a str>,
}

/// The json writer's whole-document projection; see [`RecordJson`] for why it derives `Serialize` and nothing else.
#[derive(Serialize)]
struct DocumentJson<'a> {
    conversation: &'a str,
    records: Vec<RecordJson<'a>>,
}

/// The json form of a [`Document`] (decision 58's re-import path).
///
/// A top-level object `{ "conversation": <key>, "records": [...] }` — the key rides in the file as well as in the path decision 60 names, so a transcript is self-identifying if it is moved or renamed. Rows are the merged records in order; each row's `created` is [`Record::resolved_created`] rendered as a string, the same fixed instant the media legs stamp with (decision 61). `None` fields are omitted (see [`RecordJson`]).
///
/// # Errors
///
/// Returns `serde_json::Error` when serialization fails. The mirror holds only `String`s and `bool`s, so this is unreachable in practice — the `Result` is returned rather than swallowed so a future mirror field that CAN fail (a float, a hand-written `Serialize`) surfaces loudly instead of rendering as `""`.
pub fn write_json(document: &Document) -> Result<String, serde_json::Error> {
    let mirror = DocumentJson { conversation: document.key.as_str(), records: document.records.iter().map(record_json).collect() };
    serde_json::to_string(&mirror)
}

fn record_json(record: &Record) -> RecordJson<'_> {
    RecordJson {
        kind: record.kind().as_wire(),
        from: record.from().map(Username::as_str),
        is_sender: record.is_sender(),
        media_type: record.media_type().as_wire(),
        created: record.resolved_created().map(|timestamp| timestamp.to_string()),
        content: record.content().map(model::MessageText::expose),
        media_ids: record.media_ids(),
        conversation_title: record.conversation_title(),
    }
}

/// The csv form of a [`Document`] (decision 58's spreadsheet path), RFC 4180.
///
/// A header row, then one row per record in the merged order, `\n`-terminated, with an absent field as the empty string. The columns are `kind,from,is_sender,media_type,created,content,media_ids,conversation_title`; `created` is the resolved instant rendered as a string, like the json writer. A field containing a comma, a double quote, a newline or a carriage return is wrapped in double quotes and any embedded quote is doubled (`"` → `""`); every other field is written verbatim. `\n` line endings rather than RFC 4180's `\r\n`, stated so the choice is a decision rather than a surprise: the crate is Unix-flavored, the files land beside the media on the same tree, and the parsers this targets accept both.
pub fn write_csv(document: &Document) -> String {
    let mut out = String::new();
    out.push_str("kind,from,is_sender,media_type,created,content,media_ids,conversation_title\n");
    for record in &document.records {
        let from = record.from().map(Username::as_str).unwrap_or("");
        let created = record.resolved_created().map(|timestamp| timestamp.to_string()).unwrap_or_default();
        let content = record.content().map(model::MessageText::expose).unwrap_or("");
        let media_ids = record.media_ids().unwrap_or("");
        let conversation_title = record.conversation_title().unwrap_or("");
        out.push_str(&csv_field(record.kind().as_wire()));
        out.push(',');
        out.push_str(&csv_field(from));
        out.push(',');
        out.push_str(if record.is_sender() { "true" } else { "false" });
        out.push(',');
        out.push_str(&csv_field(record.media_type().as_wire()));
        out.push(',');
        out.push_str(&csv_field(&created));
        out.push(',');
        out.push_str(&csv_field(content));
        out.push(',');
        out.push_str(&csv_field(media_ids));
        out.push(',');
        out.push_str(&csv_field(conversation_title));
        out.push('\n');
    }
    out
}

/// The RFC 4180 quoting for one field, applied only when the field needs it.
///
/// A value whose first character is a spreadsheet formula trigger (`=`, `+`, `-`, `@`, a tab, or a
/// carriage return) is prefixed with a single quote first, so Excel and Sheets render it as text
/// instead of evaluating it (CWE-1236). That quote is the cost of csv being the display path rather
/// than a lossless one: a body that really begins with `=` comes back out of a spreadsheet as `'=`,
/// and [`write_json`] is the re-import path that keeps the bytes verbatim. The prefix lands before
/// quoting, so a value needing both a guard and quoting carries the guard inside its quotes.
fn csv_field(value: &str) -> String {
    let neutralized;
    let value = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        neutralized = format!("'{value}");
        neutralized.as_str()
    } else {
        value
    };
    if value.contains([',', '"', '\n', '\r']) {
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for character in value.chars() {
            if character == '"' {
                out.push('"');
            }
            out.push(character);
        }
        out.push('"');
        out
    } else {
        value.to_owned()
    }
}

/// The plain-text form of a [`Document`] (the transcript path).
///
/// One header line per record — `[<created>] <from> (<kind> <media_type>, <sent|received>)`, with the literals `no date` and `unknown` standing in for an absent instant and sender — then the body with every line prefixed `> `. A blank line separates records. See the module docs for why the prefix and the `str::lines()` rendering keep the format unambiguous.
///
/// The `no date` and `unknown` literals are display placeholders, not a lossless encoding: a handle
/// that literally spells `unknown` renders here exactly as an absent sender does, and [`write_json`]
/// (key omitted) and [`write_csv`] (empty field) keep the two apart.
pub fn write_text(document: &Document) -> String {
    let blocks: Vec<String> = document.records.iter().map(text_block).collect();
    blocks.join("\n")
}

fn text_block(record: &Record) -> String {
    let created = record.resolved_created().map(|timestamp| timestamp.to_string()).unwrap_or_else(|| "no date".to_owned());
    let from = record.from().map(ToString::to_string).unwrap_or_else(|| "unknown".to_owned());
    let direction = if record.is_sender() { "sent" } else { "received" };
    let mut out = format!(
        "[{created}] {from} ({kind} {media_type}, {direction})",
        kind = record.kind().as_wire(),
        media_type = record.media_type().as_wire(),
    );
    out.push('\n');
    if let Some(content) = record.content() {
        for line in content.expose().lines() {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

// ---- the html writer (decision 58's "the format a user reads", decision 62's links) ----

/// What a rendered document's media links did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlLinks {
    /// A manifest was read; links resolve where a row is `done`.
    Manifest,
    /// No manifest exists (no export id), so every media reference renders as a placeholder.
    NoManifest,
}

/// A rendered html document and the state of its media links.
#[derive(Clone)]
pub struct Html {
    pub html: String,
    pub links: HtmlLinks,
}

impl fmt::Debug for Html {
    /// Redacted for the reason [`model::MessageText`]'s own `Debug` is: `html` holds every body's
    /// text, so a `{:?}` would print a whole transcript. Escaping makes markup inert, not private.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Html").field("links", &self.links).field("html", &"<redacted>").finish()
    }
}

/// The html form of a [`Document`] (decision 58's "the format a user reads", decision 62's links).
///
/// `manifest` is `None` when the source names no `mydata~*` part group, so there is no `ExportId`
/// and no manifest to read from; every media reference then renders as a placeholder and
/// [`Html::links`] is [`HtmlLinks::NoManifest`], so the run that lands the file
/// ([`super::history_run`]) can state the reason once rather than per message. `Some` is the
/// already-opened manifest that run holds.
///
/// # Errors
///
/// Returns [`ManifestError`] when the manifest read for a media token fails.
pub fn write_html(document: &Document, manifest: Option<&Manifest>) -> Result<Html, ManifestError> {
    let title = html_escape(&document_title(document));
    let body: String = document.records.iter().map(|record| html_block(record, manifest)).collect::<Result<_, _>>()?;
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n</head>\n<body>\n{body}\n</body>\n</html>\n"
    );
    Ok(Html { html, links: if manifest.is_some() { HtmlLinks::Manifest } else { HtmlLinks::NoManifest } })
}

/// The document's `<title>`: the conversation title where any record carries one, else the key.
///
/// A title is written per message, so a renamed group carries two under one key; the first in the
/// merged order wins, which makes the choice a fact about the render stream rather than a lookup.
fn document_title(document: &Document) -> String {
    document.records.iter().find_map(Record::conversation_title).map(ToOwned::to_owned).unwrap_or_else(|| document.key.as_str().to_owned())
}

/// One record as an `<article>`, the html analogue of [`text_block`]: the header line both render,
/// then the body, then the media links.
fn html_block(record: &Record, manifest: Option<&Manifest>) -> Result<String, ManifestError> {
    let created = html_escape(&record.resolved_created().map(|timestamp| timestamp.to_string()).unwrap_or_default());
    let from = html_escape(record.from().map(Username::as_str).unwrap_or(""));
    let direction = html_escape(if record.is_sender() { "sent" } else { "received" });
    let kind = html_escape(record.kind().as_wire());
    let media_type = html_escape(record.media_type().as_wire());
    let mut out = format!(
        "<article><p><span class=\"time\">{created}</span> {from} (<span class=\"kind\">{kind}</span> <span class=\"media-type\">{media_type}</span>, {direction})</p>"
    );
    if let Some(content) = record.content() {
        // `white-space: pre-wrap` keeps the body's own line breaks, which a browser would otherwise
        // collapse to spaces — the html is the format a user reads (decision 58), so a multi-line
        // message reads as the lines it was sent in.
        out.push_str("<p class=\"body\" style=\"white-space: pre-wrap\">");
        out.push_str(&html_escape(content.expose()));
        out.push_str("</p>");
    }
    if let Some(media_ids) = record.media_ids() {
        let media: String = media_tokens(media_ids).map(|token| html_media_token(record, token, manifest)).collect::<Result<_, _>>()?;
        if !media.is_empty() {
            out.push_str("<p class=\"media\">");
            out.push_str(&media);
            out.push_str("</p>");
        }
    }
    out.push_str("</article>");
    Ok(out)
}

/// One `Media IDs` token as html: a link where the manifest row is `done`, else a placeholder.
///
/// The module docs hold the reading of decision 62 that drives this; the manifest lookup is the
/// authority. The lookup KEY is the join's own canonical spelling — [`parse_history_token`]'s
/// normalization, not a second one — so a token shouting its prefix (`B~x`) reaches the row the
/// join mints under `b~x`. A token outside the grammar keeps its own spelling and
/// falls to the placeholder unless that spelling names a row: nothing is REJECTED here, the
/// canonicalization only ever widens the lookup to the spelling the rows actually carry.
fn html_media_token(record: &Record, token: &str, manifest: Option<&Manifest>) -> Result<String, ManifestError> {
    let Some(manifest) = manifest else {
        return Ok(media_placeholder(record));
    };
    let canonical = parse_history_token(token);
    let token = canonical.as_deref().unwrap_or(token);
    let Some(item) = manifest.item(ItemKind::ChatMedia, token)? else {
        return Ok(media_placeholder(record));
    };
    if item.status == ItemStatus::Done
        && let Some(file_name) = item.output_path.as_deref().and_then(|path| path.file_name()).and_then(OsStr::to_str)
    {
        let href = html_escape(file_name);
        Ok(format!("<a href=\"{href}\">{href}</a>"))
    } else {
        Ok(media_placeholder(record))
    }
}

/// The inert html for a media reference the manifest does not resolve: the record's own `Media Type`
/// and nothing else (decision 62) — no token, no id, no path.
fn media_placeholder(record: &Record) -> String {
    format!("<span class=\"media-placeholder\">{}</span>", html_escape(record.media_type().as_wire()))
}

/// Escapes the five characters that carry markup meaning, for text and double-quoted attribute
/// contexts alike. The set is what the html-escape crates escape; hand-rolled so no dependency is
/// bought for five characters.
fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::model::{ChatMessage, Field};

    /// The UTC instant `text` spells, through the same parser the loader uses.
    fn at(text: &str) -> Timestamp {
        Timestamp::parse(Field::Created, text).expect("the synthesized timestamp parses")
    }

    fn chat_msg(created: Option<Timestamp>, created_epoch_ms: Option<i64>) -> ChatMessage {
        ChatMessage {
            from: None,
            media_type: MediaKind::Text,
            created,
            created_epoch_ms,
            content: None,
            conversation_title: None,
            is_sender: false,
            is_saved: false,
            media_ids: None,
        }
    }

    fn snap_msg(created: Option<Timestamp>, created_epoch_ms: Option<i64>) -> model::Snap {
        model::Snap { from: None, media_type: MediaKind::Media, created, created_epoch_ms, conversation_title: None, is_sender: false }
    }

    fn merge_one_thread(chat_records: Vec<ChatMessage>, snap_records: Vec<model::Snap>) -> Vec<Record> {
        let chat = model::ChatHistory { conversations: vec![model::Conversation { id: ConversationId::new("k"), records: chat_records }] };
        let snap = model::SnapHistory { conversations: vec![model::Conversation { id: ConversationId::new("k"), records: snap_records }] };
        let merged = merge(&chat, &snap);
        assert_eq!(merged.threads.len(), 1, "the single conversation becomes one thread");
        merged.threads.into_iter().next().expect("one thread").records
    }

    fn kinds(records: &[Record]) -> Vec<RecordKind> {
        records.iter().map(Record::kind).collect()
    }

    #[test]
    fn records_interleave_by_resolved_created_across_both_sources() {
        let records = merge_one_thread(
            vec![chat_msg(Some(at("2021-03-04 09:00:00 UTC")), None), chat_msg(Some(at("2021-03-04 11:00:00 UTC")), None)],
            vec![snap_msg(Some(at("2021-03-04 08:00:00 UTC")), None), snap_msg(Some(at("2021-03-04 10:00:00 UTC")), None)],
        );
        assert_eq!(kinds(&records), vec![RecordKind::Snap, RecordKind::Chat, RecordKind::Snap, RecordKind::Chat]);
    }

    /// Two records resolving to the same instant, one stated in full and one only by epoch. The
    /// epoch tiebreak (`None` before `Some`) is what pins the order, since both records are chat
    /// and the source order is the REVERSE of the expected one — position alone would put the
    /// epoch-only record first.
    #[test]
    fn an_epoch_only_record_resolving_to_the_same_second_sorts_after_the_created_only_one() {
        let records = merge_one_thread(
            vec![chat_msg(None, Some(1_614_848_400_000)), chat_msg(Some(at("2021-03-04 09:00:00 UTC")), None)],
            Vec::new(),
        );
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].resolved_created(), records[1].resolved_created(), "both resolve to the same instant");
        assert!(records[0].created().is_some(), "the created-stated record sorts first");
        assert_eq!(records[1].created_epoch_ms(), Some(1_614_848_400_000));
        assert!(records[1].created().is_none());
    }

    #[test]
    fn records_with_no_timestamp_at_all_sort_after_every_timestamped_one() {
        let records =
            merge_one_thread(vec![chat_msg(None, None), chat_msg(Some(at("2021-03-04 09:00:00 UTC")), None)], vec![snap_msg(None, None)]);
        assert_eq!(kinds(&records), vec![RecordKind::Chat, RecordKind::Chat, RecordKind::Snap]);
        assert!(records[0].resolved_created().is_some(), "the timestamped record sorts first");
        assert!(records[1].resolved_created().is_none(), "then the no-timestamp chat, before the no-timestamp snap (kind tiebreak)");
        assert!(records[2].resolved_created().is_none());
    }
}
