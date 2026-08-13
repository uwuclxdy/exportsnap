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
//! Nothing here writes a file, renders a row, or knows a screen exists. This is the document
//! model the four phase-4 writers render over (decision 58); the writers themselves, and the
//! serializable mirror one of them needs, are later tasks.
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
//! [`Record`], [`Thread`] and [`MergedHistory`] derive `Debug` and `Clone` and nothing else. The
//! bodies they wrap keep [`model::MessageText`]'s redacting `Debug`, so a `{:?}` cannot leak a
//! message body, and nothing here derives `Serialize` — task 77's json writer builds a separate
//! serializable mirror rather than deriving one on these (decisions 3, 58).

use std::cmp::Reverse;
use std::collections::BTreeMap;

use crate::export::model;
use crate::export::model::{ConversationId, MediaKind, MessageText, Timestamp, Username};

/// Which of the two sources a merged record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Chat,
    Snap,
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
