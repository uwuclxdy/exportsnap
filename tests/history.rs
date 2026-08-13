//! Public-API tests for `exportsnap::export::history`: the merged per-conversation timeline
//! (decision 61).
//!
//! Everything here is synthetic and filesystem-free. Inputs are built through the real
//! schema-to-model path, the same one the loader uses, so the merge never sees a state the loader
//! could not produce — including the `Created(microseconds)` absence collapse, which turns an
//! absent key or a literal `0` into `None` before `merge` runs. The full ordering rule is pinned
//! at the module's own inline tests; these three cases pin what `merge` does with the two
//! histories as a whole.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use exportsnap::export::ExportJson;
use exportsnap::export::history::{Record, RecordKind, merge};
use exportsnap::export::model::{ChatHistory, ConversationId, MessageText, SnapHistory};
use exportsnap::export::schema;

/// The crate-level allow is scoped here rather than inside `common`: this crate reads the fixture
/// half and gates on no tool. See `tests/common/mod.rs` for what that placement keeps measuring.
#[allow(dead_code, reason = "this crate reads the fixture tree and gates on no external tool")]
mod common;

/// `chat_history.json` conversations, built through the real schema-to-model path.
fn chat_from(rows: Vec<(&str, Vec<schema::ChatEntry>)>) -> ChatHistory {
    let conversations: BTreeMap<String, Vec<schema::ChatEntry>> =
        rows.into_iter().map(|(key, entries)| (key.to_owned(), entries)).collect();
    ChatHistory::try_from(schema::ChatHistory { conversations }).expect("the synthesized chat entries parse")
}

/// `snap_history.json` conversations, built through the real schema-to-model path.
fn snap_from(rows: Vec<(&str, Vec<schema::SnapEntry>)>) -> SnapHistory {
    let conversations: BTreeMap<String, Vec<schema::SnapEntry>> =
        rows.into_iter().map(|(key, entries)| (key.to_owned(), entries)).collect();
    SnapHistory::try_from(schema::SnapHistory { conversations }).expect("the synthesized snap entries parse")
}

/// One chat message, every field other than its dates left at the loader's own default.
fn chat_entry(created: &str, created_epoch: Option<i64>) -> schema::ChatEntry {
    schema::ChatEntry { created: created.to_owned(), created_epoch, ..schema::ChatEntry::default() }
}

/// One chat message with a body, so two records with no timestamps are still distinguishable in
/// the pinned order.
fn chat_entry_with_content(created: &str, created_epoch: Option<i64>, content: &str) -> schema::ChatEntry {
    schema::ChatEntry { created: created.to_owned(), created_epoch, content: Some(content.to_owned()), ..schema::ChatEntry::default() }
}

/// One snap, every field other than its dates left at the loader's own default.
fn snap_entry(created: &str, created_epoch: Option<i64>) -> schema::SnapEntry {
    schema::SnapEntry { created: created.to_owned(), created_epoch, ..schema::SnapEntry::default() }
}

fn kinds(records: &[Record]) -> Vec<RecordKind> {
    records.iter().map(Record::kind).collect()
}

#[test]
fn a_conversation_present_in_only_one_source_keeps_that_sources_records() {
    let chat = chat_from(vec![("solo-chat", vec![chat_entry("2021-03-04 09:00:00 UTC", None)])]);
    let snap = snap_from(vec![("solo-snap", vec![snap_entry("2021-03-04 08:00:00 UTC", None)])]);
    let merged = merge(&chat, &snap);

    assert_eq!(merged.threads.len(), 2);
    assert_eq!(merged.threads[0].id, ConversationId::new("solo-chat"));
    assert_eq!(merged.threads[0].records.len(), 1);
    assert_eq!(merged.threads[0].records[0].kind(), RecordKind::Chat);
    assert_eq!(merged.threads[1].id, ConversationId::new("solo-snap"));
    assert_eq!(merged.threads[1].records.len(), 1);
    assert_eq!(merged.threads[1].records[0].kind(), RecordKind::Snap);
}

/// A conversation whose records carry no timestamp at all still gets one deterministic order —
/// the rule has only kind and source position to go on, and must not panic or depend on a hash
/// seed. `Some(0)` exercises the loader's other spelling of absence for `Created(microseconds)`.
#[test]
fn a_conversation_with_no_timestamp_at_all_sorts_deterministically() {
    let chat = chat_from(vec![("k", vec![chat_entry_with_content("", None, "first"), chat_entry("", Some(0))])]);
    let snap = snap_from(vec![("k", vec![snap_entry("", None)])]);
    let merged = merge(&chat, &snap);

    let thread = &merged.threads[0];
    assert_eq!(thread.id, ConversationId::new("k"));
    assert_eq!(thread.records.len(), 3);
    assert_eq!(kinds(&thread.records), vec![RecordKind::Chat, RecordKind::Chat, RecordKind::Snap]);
    // The position tiebreak is pinned too: the first chat record keeps its pre-sort place.
    assert_eq!(thread.records[0].content().map(MessageText::expose), Some("first"));
}

#[test]
fn a_chat_records_timestamp_between_two_snaps_lands_between_them() {
    let chat = chat_from(vec![("k", vec![chat_entry("2021-03-04 09:00:00 UTC", None)])]);
    let snap = snap_from(vec![("k", vec![snap_entry("2021-03-04 08:00:00 UTC", None), snap_entry("2021-03-04 10:00:00 UTC", None)])]);
    let merged = merge(&chat, &snap);

    let thread = &merged.threads[0];
    assert_eq!(thread.id, ConversationId::new("k"));
    assert_eq!(kinds(&thread.records), vec![RecordKind::Snap, RecordKind::Chat, RecordKind::Snap]);
    // The timestamps too, not just the kinds: `[Snap, Chat, Snap]` is a palindrome of kinds, so
    // a whole reversal would survive a kinds-only assertion.
    let times: Vec<String> =
        thread.records.iter().map(|record| record.resolved_created().expect("every record here is timestamped").to_string()).collect();
    assert_eq!(times, ["2021-03-04 08:00:00 UTC", "2021-03-04 09:00:00 UTC", "2021-03-04 10:00:00 UTC"]);
}

/// The task's "fixture-backed" verify, over the real redacted export rather than a synthetic tree.
///
/// The synthetic cases above join keys by construction — both sides get the same literal — so they
/// cannot disprove decision 61's load-bearing premise that `snap_history.json` keys by conversation
/// exactly as `chat_history.json` does. This pins it: the redactor's value-keyed HMAC masking keeps
/// the two files' keys aligned, so a key shared by both files is the same real conversation, and it
/// must merge into one thread carrying both kinds rather than two.
#[test]
fn the_real_export_merges_chat_and_snap_into_aligned_threads() {
    // `fixtures/` is gitignored, so CI never has it; asked through the shared gate rather than by
    // rebuilding the path here.
    let Some(root) = common::fixtures::root("the_real_export_merges_chat_and_snap_into_aligned_threads") else {
        return;
    };
    let json = root.join("mydata~xxxxxxxxxxxx/json");
    let export = ExportJson::load_dir(&json).expect("the fixture export loads");
    let chat = export.chat_history.expect("the fixture carries chat_history.json");
    let snap = export.snap_history.expect("the fixture carries snap_history.json");
    let merged = merge(&chat, &snap);

    let chat_keys: BTreeSet<ConversationId> = chat.conversations.iter().map(|c| c.id.clone()).collect();
    let snap_keys: BTreeSet<ConversationId> = snap.conversations.iter().map(|c| c.id.clone()).collect();
    let shared: BTreeSet<ConversationId> = chat_keys.intersection(&snap_keys).cloned().collect();
    let union: BTreeSet<ConversationId> = chat_keys.union(&snap_keys).cloned().collect();

    // One thread per distinct key, and the thread list is exactly the union of the two files' keys.
    assert_eq!(merged.threads.len(), union.len(), "a key in both files is one thread, not two");
    let merged_keys: BTreeSet<ConversationId> = merged.threads.iter().map(|t| t.id.clone()).collect();
    assert_eq!(merged_keys, union);

    // No record dropped or duplicated: the merged count is the two files' counts summed.
    let chat_total: usize = chat.conversations.iter().map(|c| c.records.len()).sum();
    let snap_total: usize = snap.conversations.iter().map(|c| c.records.len()).sum();
    assert_eq!(merged.threads.iter().map(|t| t.records.len()).sum::<usize>(), chat_total + snap_total);

    // The alignment claim, stated as a fact about the fixture rather than assumed.
    assert!(!shared.is_empty(), "the fixture exercises the alignment claim; empty means the redactor or export changed");
    for thread in &merged.threads {
        let has_chat = thread.records.iter().any(|record| record.kind() == RecordKind::Chat);
        let has_snap = thread.records.iter().any(|record| record.kind() == RecordKind::Snap);
        if shared.contains(&thread.id) {
            assert!(has_chat && has_snap, "a key in both files merges both kinds into one thread");
        } else if chat_keys.contains(&thread.id) {
            assert!(has_chat && !has_snap, "a chat-only key carries no snap record");
        } else {
            assert!(has_snap && !has_chat, "a snap-only key carries no chat record");
        }
    }
}
