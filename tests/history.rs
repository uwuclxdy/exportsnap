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
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use exportsnap::export::ExportJson;
use exportsnap::export::chat_fix::{self, OverlayMode, RecordedDirs};
use exportsnap::export::chat_media::{self, ChatMedia, ChatMediaFile, ChatMediaItem, Join, Message, MessageRef, Reconciliation};
use exportsnap::export::history::{Document, HtmlLinks, Record, RecordKind, merge, write_csv, write_html, write_json, write_text};
use exportsnap::export::history_run::{self, HistoryFormat, HistoryReport, RunEvent, RunInputs, RunOutcome};
use exportsnap::export::manifest::{DirectoryClaim, ExportId, ItemKind, Manifest, NewItem, ResumeReport};
use exportsnap::export::model::{ChatHistory, ConversationId, MessageText, SnapHistory};
use exportsnap::export::schema;
use tempfile::TempDir;

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
/// the pinned order. `Media Type` is set to `TEXT` so the writers' rows carry a real word rather
/// than the schema default's empty string (which `MediaKind::from_wire` maps to `Other("")`).
fn chat_entry_with_content(created: &str, created_epoch: Option<i64>, content: &str) -> schema::ChatEntry {
    schema::ChatEntry {
        created: created.to_owned(),
        created_epoch,
        content: Some(content.to_owned()),
        media_type: "TEXT".to_owned(),
        ..schema::ChatEntry::default()
    }
}

/// One chat message naming media. `Media Type` is `MEDIA`, the word the observed export puts on a
/// row that names a file, and the `Media IDs` value is handed to the writer exactly as written.
fn chat_entry_with_media(created: &str, created_epoch: Option<i64>, media_ids: &str) -> schema::ChatEntry {
    schema::ChatEntry {
        created: created.to_owned(),
        created_epoch,
        media_type: "MEDIA".to_owned(),
        media_ids: media_ids.to_owned(),
        ..schema::ChatEntry::default()
    }
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

// ---- the document model and the three writers (decision 58) ----

/// One conversation holding `chat_entries` and no snaps, as a [`Document`] through the real
/// schema-to-merge path.
fn document_with(chat_entries: Vec<schema::ChatEntry>) -> Document {
    document_from(chat_entries, Vec::new())
}

/// One conversation holding both sources' entries, as a [`Document`] through the real
/// schema-to-merge path.
fn document_from(chat_entries: Vec<schema::ChatEntry>, snap_entries: Vec<schema::SnapEntry>) -> Document {
    let chat = chat_from(vec![("k", chat_entries)]);
    let snap = snap_from(vec![("k", snap_entries)]);
    let merged = merge(&chat, &snap);
    let thread = merged.threads.into_iter().next().expect("the single conversation becomes one thread");
    Document::from_thread(thread)
}

/// One snap with its sender and kind named, so a snap row in the writers is distinguishable.
fn snap_entry_named(from: &str, created: &str) -> schema::SnapEntry {
    schema::SnapEntry { from: from.to_owned(), media_type: "MEDIA".to_owned(), created: created.to_owned(), ..schema::SnapEntry::default() }
}

/// A small RFC 4180 reader, in the test so the write path is not judged by the code that wrote
/// it. Fields are unwrapped on `\n`; a quoted field is unwrapped on `"` with `""` read back as a
/// single quote; `\r` outside a quoted field is skipped rather than misread as a line ending.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    while let Some(character) = chars.next() {
        if in_quotes {
            if character == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(character);
            }
        } else {
            match character {
                '"' => in_quotes = true,
                ',' => row.push(std::mem::take(&mut field)),
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                // The writer never emits a bare `\r` outside a quoted field, so skipping it here
                // cannot lose data this format actually produces.
                '\r' => {}
                _ => field.push(character),
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// Reads [`write_text`] output back into `(header, body_lines)` records. A blank line separates
/// records; a line starting with `> ` continues the current record's body; any other non-blank
/// line starts a new record.
fn parse_text(text: &str) -> Vec<(String, Vec<String>)> {
    let mut records: Vec<(String, Vec<String>)> = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("> ") {
            let (_, body) = records.last_mut().expect("a continuation line always follows a header");
            body.push(rest.to_owned());
        } else {
            records.push((line.to_owned(), Vec::new()));
        }
    }
    records
}

/// The whole risk of csv: a body holding the delimiter, a quote, and a newline has to survive
/// write-then-parse byte-for-byte, and so does a body holding a bare carriage return and nothing
/// else. The `\r` case is separate rather than folded into the first body, because a strict csv
/// consumer treats a bare `\r` as a terminator — a mutation deleting `\r` from the quoting trigger
/// would leave the first body still quoted by its comma, so only the `\r`-only body catches it.
#[test]
fn csv_round_trips_a_body_with_every_quoting_trigger() {
    for body in ["hello, \"world\"\nsecond line", "bare\rreturn"] {
        let document = document_with(vec![chat_entry_with_content("2021-03-04 09:00:00 UTC", None, body)]);
        let rendered = write_csv(&document);

        let rows = parse_csv(&rendered);
        assert_eq!(rows.len(), 2, "the header row plus one data row");
        assert_eq!(rows[0], ["kind", "from", "is_sender", "media_type", "created", "content", "media_ids", "conversation_title"]);
        assert_eq!(rows[1].len(), 8, "an empty absent field still occupies its column");
        assert_eq!(rows[1][5], body, "the content column survives write-then-parse byte-for-byte");
    }
}

/// Each writer is byte-stable: two INDEPENDENTLY built documents — same entries, separate
/// schema-to-merge passes, not a cloned buffer — render byte-identical output. That is the
/// determinism the writers promise and the test the task's "one render stream" (decision 58)
/// exists to guarantee.
#[test]
fn each_writer_is_byte_stable_across_two_independent_builds() {
    let entries = || vec![chat_entry_with_content("2021-03-04 09:00:00 UTC", None, "stable\nbody, \"quoted\"")];
    let snaps = || vec![snap_entry_named("alice", "2021-03-04 09:30:00 UTC")];
    let first = document_from(entries(), snaps());
    let second = document_from(entries(), snaps());

    assert_eq!(write_text(&first), write_text(&second));
    assert_eq!(write_csv(&first), write_csv(&second));
    assert_eq!(write_json(&first).unwrap(), write_json(&second).unwrap());
}

/// The text format's unambiguity: a body with a newline must stay one record, and a record with
/// an empty body must not be swallowed into the one before it. [`parse_text`] is the reader a
/// consumer would write, so "cannot be read as two records" is asserted through it rather than
/// by eyeballing the bytes.
#[test]
fn text_renders_a_multiline_body_and_an_empty_body_as_distinct_records() {
    let document = document_with(vec![
        chat_entry_with_content("2021-03-04 09:00:00 UTC", None, "first line\nsecond line"),
        // `content: Some("")` through the schema path is the loader's empty-spelling of absence,
        // so this record reaches the writers with no body at all.
        chat_entry_with_content("2021-03-04 09:30:00 UTC", None, ""),
        chat_entry_with_content("2021-03-04 10:00:00 UTC", None, "third"),
    ]);
    let rendered = write_text(&document);

    let records = parse_text(&rendered);
    assert_eq!(records.len(), 3, "the empty-body record must not be read as two records or none");
    assert_eq!(records[0].1, ["first line", "second line"], "a newline in the body is one record, not two");
    assert!(records[1].1.is_empty(), "the empty body renders as no body lines");
    assert_eq!(records[2].1, ["third"]);
}

/// The json output has to be the REAL body, not a `Debug` string: it parses back with the body,
/// kind, and timestamp intact, and carries no `MessageText(<redacted>)` marker. A snap row is
/// pinned too — `kind: "snap"` and no `content` key, since `None` fields are omitted.
#[test]
fn json_round_trips_body_kind_and_timestamp_and_never_carries_a_redaction_marker() {
    let body = "hello \"world\"";
    let document = document_from(
        vec![schema::ChatEntry {
            from: "alice".to_owned(),
            media_type: "TEXT".to_owned(),
            created: "2021-03-04 09:00:00 UTC".to_owned(),
            created_epoch: None,
            content: Some(body.to_owned()),
            conversation_title: Some("The Gang".to_owned()),
            is_sender: false,
            ..schema::ChatEntry::default()
        }],
        vec![snap_entry_named("bob", "2021-03-04 09:30:00 UTC")],
    );
    let rendered = write_json(&document).unwrap();

    // The body's embedded quote is JSON-escaped (`"` → `\"`) on the wire, so the byte-level check
    // matches the escaped spelling; the round-trip below proves the value itself survives.
    assert!(rendered.contains(r#"hello \"world\""#), "the real body is in the output, not a placeholder");
    assert!(!rendered.contains("MessageText(<redacted>)"), "the json must never carry the Debug marker");

    let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(value["conversation"], "k", "the conversation key rides in the document");
    let records = value["records"].as_array().expect("records is an array");
    assert_eq!(records.len(), 2, "the chat and the snap both render");

    let chat = &records[0];
    assert_eq!(chat["kind"], "chat");
    assert_eq!(chat["content"], body);
    assert_eq!(chat["created"], "2021-03-04 09:00:00 UTC");
    assert_eq!(chat["media_type"], "TEXT");
    assert_eq!(chat["from"], "alice");
    assert_eq!(chat["conversation_title"], "The Gang");
    assert_eq!(chat["is_sender"], false);

    let snap = &records[1];
    assert_eq!(snap["kind"], "snap");
    assert_eq!(snap["media_type"], "MEDIA");
    assert!(snap.get("content").is_none(), "a snap row carries no content key at all");
}

/// The redacting `Debug` holds all the way up the document: a `{:?}` on the [`Document`] must
/// not contain a body string. This is the leak the json mirror's missing `Debug` exists to stop.
#[test]
fn document_debug_never_prints_a_message_body() {
    let body = "TOP-SECRET-BODY-NEVER-DEBUG";
    let document = document_with(vec![chat_entry_with_content("2021-03-04 09:00:00 UTC", None, body)]);
    let debugged = format!("{document:?}");
    assert!(!debugged.contains(body), "a Debug render of the document must not leak a message body");
    assert!(debugged.contains("MessageText(<redacted>)"), "the body's Debug spelling is the redacted one");
}

/// A body whose first character is a spreadsheet formula trigger is guarded with a leading `'`, so
/// Excel and Sheets render it as text instead of evaluating it (CWE-1236). The guard rides inside
/// the quotes when the value needs quoting too, and it is the whole reason the csv is the display
/// path while [`exportsnap::export::history::write_json`] is the lossless re-import path.
#[test]
fn csv_neutralizes_every_formula_trigger() {
    for trigger in ['=', '+', '-', '@', '\t', '\r'] {
        let body = format!("{trigger}1+1");
        let document = document_with(vec![chat_entry_with_content("2021-03-04 09:00:00 UTC", None, &body)]);
        let rows = parse_csv(&write_csv(&document));
        assert_eq!(rows[1][5], format!("'{body}"), "a leading {trigger:?} is guarded so a spreadsheet reads text");
    }
}

/// The writers stamp the RESOLVED instant (decision 61's fixed timestamps), not the `Created` string
/// alone: a record carrying only `Created(microseconds)` must still render a date, and that is the
/// wiring a `record_json` switch back to `created()` would break.
#[test]
fn the_writers_stamp_the_resolved_instant_not_the_created_string_alone() {
    // `created: ""` is the loader's empty spelling, so only the epoch names the instant.
    let document = document_with(vec![schema::ChatEntry {
        created: "".to_owned(),
        created_epoch: Some(1_614_848_400_000), // 2021-03-04 09:00:00 UTC
        content: Some("only an epoch".to_owned()),
        ..schema::ChatEntry::default()
    }]);
    let value: serde_json::Value = serde_json::from_str(&write_json(&document).unwrap()).unwrap();
    assert_eq!(value["records"][0]["created"], "2021-03-04 09:00:00 UTC");
}

// ---- the html writer (decision 58's "the format a user reads", decision 62's links) ----

/// Escaping is the writer's first concern: a body holding markup, quotes and an ampersand renders
/// as escaped text, and the raw markup never reaches the document. The assertion is on the literal
/// entity spellings, so a mutation that skips one character's escape cannot pass by coincidence.
#[test]
fn html_escapes_a_body_containing_markup() {
    let body = "<script>alert('&\"x');</script>";
    let document = document_with(vec![chat_entry_with_content("2021-03-04 09:00:00 UTC", None, body)]);
    let rendered = write_html(&document, None).unwrap().html;

    assert!(
        rendered.contains("&lt;script&gt;alert(&#39;&amp;&quot;x&#39;);&lt;/script&gt;"),
        "the body renders as entities, not markup: {rendered}"
    );
    assert!(!rendered.contains("<script>"), "the raw markup never reaches the document: {rendered}");
}

/// A `done` manifest row renders a link to the bare output filename — the conversation directory is
/// flat and the document sits beside the media (decision 60) — and that filename names a real file
/// on disk (the path `mark_done` recorded).
#[test]
fn html_links_a_done_media_token_to_the_bare_filename_on_disk() {
    let workspace = TempDir::new().unwrap();
    let mut manifest = Manifest::open_in(workspace.path(), &ExportId::new("1784667002819").unwrap()).unwrap();
    let token = "b~aB3xY9";
    manifest.enroll(&[NewItem { kind: ItemKind::ChatMedia, source_id: token, url: None }]).unwrap();
    let output = workspace.path().join("2021-03-04_b~aB3xY9.jpg");
    fs::write(&output, b"media bytes").unwrap();
    manifest.mark_done(ItemKind::ChatMedia, token, &output).unwrap();

    let document = document_with(vec![chat_entry_with_media("2021-03-04 09:00:00 UTC", None, token)]);
    let rendered = write_html(&document, Some(&manifest)).unwrap().html;

    let href = "2021-03-04_b~aB3xY9.jpg";
    assert!(rendered.contains(&format!("<a href=\"{href}\">{href}</a>")), "{rendered}");
    let recorded = manifest.item(ItemKind::ChatMedia, token).unwrap().unwrap();
    assert_eq!(recorded.output_path.unwrap().file_name().unwrap().to_str().unwrap(), href, "the href is the bare output filename");
    assert!(output.exists(), "the file the link names is on disk beside the document");
}

/// A message spelling the prefix loudly (`B~x`) must reach the row the join mints under the
/// canonical `b~x`: the lookup key is the join's own normalization, so a `done` row still renders
/// its link rather than a placeholder (decision 62).
#[test]
fn html_links_a_done_media_token_spelled_with_a_shouted_prefix() {
    let workspace = TempDir::new().unwrap();
    let mut manifest = Manifest::open_in(workspace.path(), &ExportId::new("1784667002819").unwrap()).unwrap();
    let token = "b~aB3xY9";
    manifest.enroll(&[NewItem { kind: ItemKind::ChatMedia, source_id: token, url: None }]).unwrap();
    let output = workspace.path().join("2021-03-04_b~aB3xY9.jpg");
    fs::write(&output, b"media bytes").unwrap();
    manifest.mark_done(ItemKind::ChatMedia, token, &output).unwrap();

    let document = document_with(vec![chat_entry_with_media("2021-03-04 09:00:00 UTC", None, "B~aB3xY9")]);
    let rendered = write_html(&document, Some(&manifest)).unwrap().html;

    let href = "2021-03-04_b~aB3xY9.jpg";
    assert!(rendered.contains(&format!("<a href=\"{href}\">{href}</a>")), "{rendered}");
    assert!(!rendered.contains("media-placeholder"), "the shouted prefix is a spelling of the row, not an absence: {rendered}");
}

/// A row that is not `done` — failed, source-missing, pending, retired, excluded, or absent —
/// renders an inert placeholder naming the record's own `Media Type` and nothing else (decision 62):
/// no token, no id, no path. Every non-`done` status is pinned in one message, because a mutation
/// weakening the `Done` predicate to admit any one of them (`Done` → `Done | Pending` is the
/// realistic one, the mid-run state) would render a link where the test asserts a placeholder.
#[test]
fn html_renders_a_placeholder_for_every_status_that_is_not_done() {
    let workspace = TempDir::new().unwrap();
    let mut manifest = Manifest::open_in(workspace.path(), &ExportId::new("1784667002819").unwrap()).unwrap();
    let failed = "b~tokA";
    let source_missing = "b~tokB";
    let pending = "b~tokC";
    let retired = "b~tokD";
    let excluded = "b~tokE";
    let missing_row = "b~tokF";
    manifest
        .enroll(&[
            NewItem { kind: ItemKind::ChatMedia, source_id: failed, url: None },
            NewItem { kind: ItemKind::ChatMedia, source_id: source_missing, url: None },
            NewItem { kind: ItemKind::ChatMedia, source_id: pending, url: None },
            NewItem { kind: ItemKind::ChatMedia, source_id: retired, url: None },
            NewItem { kind: ItemKind::ChatMedia, source_id: excluded, url: None },
        ])
        .unwrap();
    manifest.mark_failed(ItemKind::ChatMedia, failed, "it broke").unwrap();
    manifest.mark_source_missing(ItemKind::ChatMedia, source_missing, "no media in the export").unwrap();
    // `pending` stays Pending: enrolled, never marked.
    manifest.exclude(ItemKind::ChatMedia, &[excluded.to_owned()]).unwrap();
    // `retired` left the export: every OTHER token is still named, so the sweep retires only it.
    let named: BTreeSet<&str> = [failed, source_missing, pending, excluded].into_iter().collect();
    manifest.retire_absent(ItemKind::ChatMedia, &named, &[]).unwrap();

    let media_ids = format!("{failed} | {source_missing} | {pending} | {retired} | {excluded} | {missing_row}");
    let document = document_with(vec![chat_entry_with_media("2021-03-04 09:00:00 UTC", None, &media_ids)]);
    let rendered = write_html(&document, Some(&manifest)).unwrap().html;

    assert_eq!(rendered.matches("<span class=\"media-placeholder\">MEDIA</span>").count(), 6, "{rendered}");
    assert!(!rendered.contains("<a href"), "no row is done, so no link is rendered: {rendered}");
    for token in [failed, source_missing, pending, retired, excluded, missing_row] {
        assert!(!rendered.contains(token), "a placeholder carries no token: {rendered}");
    }
}

/// `manifest: None` is the no-`mydata~*`-group run: every media reference renders as a placeholder
/// and the writer states the reason once in [`HtmlLinks::NoManifest`], so a screen does not have to
/// guess per message why every link is missing.
#[test]
fn html_without_a_manifest_renders_placeholders_and_says_no_manifest() {
    let document = document_with(vec![chat_entry_with_media("2021-03-04 09:00:00 UTC", None, "b~aB3xY9")]);
    let rendered = write_html(&document, None).unwrap();

    assert_eq!(rendered.links, HtmlLinks::NoManifest);
    assert!(rendered.html.contains("<span class=\"media-placeholder\">MEDIA</span>"), "{}", rendered.html);
    assert!(!rendered.html.contains("b~aB3xY9"), "no token reaches a placeholder");
    assert!(!rendered.html.contains("<a href"), "no manifest, no link");
}

/// The mirror of the above: a manifest present — even one with no `done` rows — is reported as
/// [`HtmlLinks::Manifest`], because a later run could make a row `done` and the links live.
#[test]
fn html_with_a_manifest_reports_links_manifest() {
    let workspace = TempDir::new().unwrap();
    let manifest = Manifest::open_in(workspace.path(), &ExportId::new("1784667002819").unwrap()).unwrap();
    let document = document_with(vec![chat_entry_with_media("2021-03-04 09:00:00 UTC", None, "b~aB3xY9")]);
    let rendered = write_html(&document, Some(&manifest)).unwrap();

    assert_eq!(rendered.links, HtmlLinks::Manifest);
}

/// The `<title>` is the conversation title where any record carries one, else the conversation key,
/// and both arms are escaped — a title holding markup must render as text, not structure.
#[test]
fn html_title_prefers_the_conversation_title_and_escapes_it() {
    let titled = document_with(vec![schema::ChatEntry {
        from: "alice".to_owned(),
        media_type: "TEXT".to_owned(),
        created: "2021-03-04 09:00:00 UTC".to_owned(),
        content: Some("hello".to_owned()),
        conversation_title: Some("<b>The Gang</b>".to_owned()),
        ..schema::ChatEntry::default()
    }]);
    let rendered = write_html(&titled, None).unwrap().html;
    assert!(rendered.contains("<title>&lt;b&gt;The Gang&lt;/b&gt;</title>"), "{rendered}");
    assert!(!rendered.contains("<b>The Gang</b>"), "the title's markup is escaped, not structural");

    let keyed = document_with(vec![chat_entry_with_content("2021-03-04 09:00:00 UTC", None, "hello")]);
    assert!(
        write_html(&keyed, None).unwrap().html.contains("<title>k</title>"),
        "no record carries a title, so the key names the document"
    );
}

/// A snap record renders its kind, timestamp and direction like the other formats, and carries no
/// media paragraph at all — a snap names no `Media IDs`.
#[test]
fn html_renders_a_snap_record_with_no_media() {
    let document = document_from(Vec::new(), vec![snap_entry_named("bob", "2021-03-04 09:30:00 UTC")]);
    let rendered = write_html(&document, None).unwrap().html;

    assert!(rendered.contains("<span class=\"kind\">snap</span>"), "{rendered}");
    assert!(rendered.contains("<span class=\"time\">2021-03-04 09:30:00 UTC</span>"), "{rendered}");
    assert!(!rendered.contains("media-placeholder"), "a snap names no media, so none is rendered");
}

/// `MediaKind::Other` carries arbitrary text, so a media type that is itself markup must render
/// escaped in the header and the placeholder alike — the call site the body-escape test does not
/// reach, since it drives `TEXT` bodies.
#[test]
fn html_escapes_an_unknown_media_type() {
    let document = document_with(vec![schema::ChatEntry {
        media_type: "<IMG>".to_owned(),
        media_ids: "b~aB3xY9".to_owned(),
        created: "2021-03-04 09:00:00 UTC".to_owned(),
        ..schema::ChatEntry::default()
    }]);
    let rendered = write_html(&document, None).unwrap().html;

    assert!(rendered.contains("<span class=\"media-type\">&lt;IMG&gt;</span>"), "the header escapes the unknown type: {rendered}");
    assert!(rendered.contains("<span class=\"media-placeholder\">&lt;IMG&gt;</span>"), "the placeholder escapes it too: {rendered}");
    assert!(!rendered.contains("<IMG>"), "the raw unknown type never reaches the document: {rendered}");
}

/// The `href` is the bare filename escaped for its attribute context: a recorded output filename
/// carrying a double quote must not break out of the attribute. The crate never writes such a name,
/// but the manifest is `0600` and hand-editable, and the escape is what contains it.
#[test]
fn html_escapes_a_double_quote_in_the_recorded_filename() {
    let workspace = TempDir::new().unwrap();
    let mut manifest = Manifest::open_in(workspace.path(), &ExportId::new("1784667002819").unwrap()).unwrap();
    let token = "b~aB3xY9";
    manifest.enroll(&[NewItem { kind: ItemKind::ChatMedia, source_id: token, url: None }]).unwrap();
    let output = workspace.path().join("weird.jpg");
    fs::write(&output, b"media bytes").unwrap();
    manifest.mark_done(ItemKind::ChatMedia, token, &output).unwrap();
    let db = manifest.path().to_path_buf();
    drop(manifest);

    // The quoted spelling is planted by editing the row, the way a user editing the 0600 manifest
    // could: a file whose name holds `"` cannot exist on windows, where the character is not a
    // legal filename. Nothing about the href reads the file — it is built from the recorded NAME
    // alone — which is the same reason the escape is what contains the hand edit.
    let quoted = workspace.path().join("we\"ird.jpg");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE items SET output_path = ?1 WHERE kind = 'chat_media' AND source_id = ?2", [quoted.to_str().unwrap(), token])
        .unwrap();
    drop(conn);

    let manifest = Manifest::open_in(workspace.path(), &ExportId::new("1784667002819").unwrap()).unwrap();
    let document = document_with(vec![chat_entry_with_media("2021-03-04 09:00:00 UTC", None, token)]);
    let rendered = write_html(&document, Some(&manifest)).unwrap().html;

    assert!(rendered.contains("href=\"we&quot;ird.jpg\""), "the filename's quote is escaped: {rendered}");
    assert!(!rendered.contains("href=\"we\"ird.jpg\""), "an unescaped quote would break the attribute: {rendered}");
}

/// The hand-written `Debug` on [`Html`] redacts the transcript, the way the model's [`MessageText`]
/// redacts a body: escaping is markup safety, not privacy, so a `{:?}` on an `Html` must not print
/// the message text either.
#[test]
fn html_debug_never_prints_a_message_body() {
    let body = "TOP-SECRET-BODY-NEVER-DEBUG";
    let document = document_with(vec![chat_entry_with_content("2021-03-04 09:00:00 UTC", None, body)]);
    let rendered = write_html(&document, None).unwrap();
    let debugged = format!("{rendered:?}");
    assert!(!debugged.contains(body), "a Debug render of an Html must not print a message body");
    assert!(debugged.contains("<redacted>"), "the html field's Debug spelling is the redacted one");
}

// ---- the run entry (decisions 60, 62, 63, 63a) ----

/// The export id every synthetic delivery below carries; it names the manifest file.
const EXPORT_ID: &str = "1784667002819";

/// A `chat_history.json` in the wire shape the parser expects: an object keyed by conversation,
/// each value an array of message records. Built by hand — the schema types are deserialize-only —
/// in the exact spelling the observed export uses.
fn write_chat_history(json_dir: &Path, conversations: &[(&str, &[(&str, &str)])]) {
    fs::create_dir_all(json_dir).unwrap();
    let threads: Vec<String> = conversations
        .iter()
        .map(|(key, rows)| {
            let entries: Vec<String> = rows
                .iter()
                .map(|(created, media_ids)| {
                    format!(
                        r#"{{"From":"sender-handle","Media Type":"MEDIA","Created":"{created}","IsSender":false,"IsSaved":false,"Created(microseconds)":0,"Media IDs":"{media_ids}"}}"#
                    )
                })
                .collect();
            format!(r#""{key}":[{}]"#, entries.join(","))
        })
        .collect();
    fs::write(json_dir.join("chat_history.json"), format!("{{{}}}", threads.join(","))).unwrap();
}

/// A `snap_history.json` in the same wire shape, snap rows carrying no body and no `Media IDs`.
fn write_snap_history(json_dir: &Path, conversations: &[(&str, &[&str])]) {
    fs::create_dir_all(json_dir).unwrap();
    let threads: Vec<String> = conversations
        .iter()
        .map(|(key, rows)| {
            let entries: Vec<String> = rows
                .iter()
                .map(|created| {
                    format!(r#"{{"From":"sender-handle","Media Type":"MEDIA","Created":"{created}","IsSender":false,"Created(microseconds)":0}}"#)
                })
                .collect();
            format!(r#""{key}":[{}]"#, entries.join(","))
        })
        .collect();
    fs::write(json_dir.join("snap_history.json"), format!("{{{}}}", threads.join(","))).unwrap();
}

/// One delivery: part 1 unpacked with its `json/`, no media dirs.
fn export_tree(conversations: &[(&str, &[(&str, &str)])]) -> TempDir {
    let dir = TempDir::new().unwrap();
    write_chat_history(&dir.path().join(format!("mydata~{EXPORT_ID}/json")), conversations);
    dir
}

/// A source naming no `mydata~*` part group: the export extracted flat, `json/` directly under
/// the source. The decision-62 no-manifest arm.
fn flat_tree(conversations: &[(&str, &[(&str, &str)])]) -> TempDir {
    let dir = TempDir::new().unwrap();
    write_chat_history(&dir.path().join("json"), conversations);
    dir
}

/// A run input selecting every conversation and every format — what the screen ships when the
/// user changes nothing. The conversation set comes from the run's own load, so a fixture needs
/// no second spelling of its key list.
fn full_inputs(source: &Path, out_root: &Path, state: &Path) -> RunInputs {
    let loaded = history_run::load_threads(source).expect("the fixture export loads");
    let conversations: BTreeSet<ConversationId> = loaded.merged.threads.iter().map(|thread| thread.id.clone()).collect();
    RunInputs {
        source: source.to_path_buf(),
        out_root: out_root.to_path_buf(),
        manifest_dir: Some(state.to_path_buf()),
        conversations,
        formats: BTreeSet::from(HistoryFormat::ALL),
    }
}

fn inputs(dir: &TempDir) -> (RunInputs, TempDir) {
    let state = TempDir::new().unwrap();
    let inputs = full_inputs(dir.path(), &dir.path().join("out"), state.path());
    (inputs, state)
}

fn collect(inputs: &RunInputs) -> Vec<RunEvent> {
    let (sender, receiver) = mpsc::channel();
    history_run::run(inputs, &sender);
    drop(sender);
    receiver.try_iter().collect()
}

fn finished(events: &[RunEvent]) -> &RunOutcome {
    match events.last().unwrap() {
        RunEvent::Finished(outcome) => outcome,
        RunEvent::Planned(_) => panic!("no Finished event"),
        RunEvent::Written => panic!("no Finished event"),
    }
}

fn report(outcome: &RunOutcome) -> HistoryReport {
    match outcome {
        RunOutcome::Completed(report) => *report,
        RunOutcome::Failed(error) => panic!("run failed: {error}"),
    }
}

/// A reconciliation over one file a message of `key` names, built by hand so the chat-media
/// planner can be driven without a media walk. The file itself is never read past its name.
fn hand_reconciliation(key: &str, token: &str) -> Reconciliation {
    let file = ChatMediaFile::parse(PathBuf::from(format!("/x/chat_media/2021-03-04_{token}.jpg"))).expect("the name parses");
    Reconciliation {
        items: vec![ChatMediaItem {
            media: ChatMedia { file, overlay: None },
            join: Join::Named(Message {
                at: MessageRef { conversation: 0, message: 0 },
                conversation: ConversationId::new(key),
                conversation_title: None,
                from: None,
                is_sender: false,
                created: None,
                created_epoch_ms: None,
            }),
        }],
        missing: Vec::new(),
        unparsed_tokens: Vec::new(),
        unparsed: Vec::new(),
        duplicates: Vec::new(),
        unreadable: Vec::new(),
    }
}

/// The canonical spelling the run's own canonicalization gives the out root, so a relative
/// `--out` respelling cannot split the comparisons.
fn chat_root(dir: &TempDir) -> PathBuf {
    fs::canonicalize(dir.path()).unwrap().join("out").join("chat")
}

/// A chat-media run under an earlier key set put `a?b`'s media in the SUFFIXED directory — with
/// its neighbour gone, `a?b` alone would now derive the plain `a_b`, so only the manifest record
/// can hold it in `a_b_2`. The history run for the same key must adopt that directory, write its
/// four documents into it, and claim it under the adopted name (decisions 60, 63a).
#[test]
fn a_history_run_writes_into_the_directory_a_chat_media_run_already_adopted() {
    let dir = export_tree(&[("a?b", &[("2021-03-04 14:30:05 UTC", "b~tokA")])]);
    let (inputs, state) = inputs(&dir);
    let mut manifest = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    manifest.enroll(&[NewItem { kind: ItemKind::ChatMedia, source_id: "b~tokA", url: None }]).unwrap();
    let media = fs::canonicalize(dir.path()).unwrap().join("out/chat/a_b_2/20210304_143005.jpg");
    fs::create_dir_all(media.parent().unwrap()).unwrap();
    fs::write(&media, b"media bytes").unwrap();
    manifest.mark_done(ItemKind::ChatMedia, "b~tokA", &media).unwrap();

    let events = collect(&inputs);
    let outcome = report(finished(&events));
    assert_eq!(outcome.conversations, 1);
    assert_eq!(outcome.documents, 4);
    assert_eq!(outcome.links, HtmlLinks::Manifest);

    for extension in ["json", "txt", "csv", "html"] {
        assert!(dir.path().join(format!("out/chat/a_b_2/history.{extension}")).is_file(), "the document lands in the adopted directory");
    }
    assert!(
        !dir.path().join("out/chat/a_b/history.json").exists(),
        "a fresh derivation would be the defect: the record names the suffixed directory"
    );

    let manifest = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    let claims = manifest.claims().unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].source_id, "a?b");
    assert_eq!(claims[0].directory, chat_root(&dir).join("a_b_2"), "the claim names the adopted directory, not a re-derivation");
}

/// The 63a back door, closed from both sides. First the history run: a conversation with messages
/// and no media claims the plain name, and a later chat-media plan for a DIFFERENT key cleaning
/// onto it takes the suffix — with a control over a claimless manifest, which hands out the plain
/// name, proving the claim is what moved the assignment. Then the mirror: the claiming
/// conversation's own later media adopts the claimed directory, so one thread never splits.
#[test]
fn a_history_only_conversations_directory_is_reserved_from_a_later_chat_media_plan() {
    let dir = export_tree(&[("a?b", &[("2021-03-04 14:30:05 UTC", "b~tokA")])]);
    let (inputs, state) = inputs(&dir);
    let events = collect(&inputs);
    assert_eq!(report(finished(&events)).conversations, 1);
    assert!(dir.path().join("out/chat/a_b/history.json").is_file(), "the history-only conversation derives the plain name");

    let manifest = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    let newcomer = hand_reconciliation("a:b", "b~tokB");
    let recorded = RecordedDirs::read(&newcomer, &manifest).unwrap();
    let plan = chat_fix::plan(&newcomer, dir.path().join("out"), OverlayMode::Both, &recorded).unwrap();
    assert_eq!(plan.items[0].output.parent().unwrap(), chat_root(&dir).join("a_b_2"), "the claimed name is not handed to another key");

    // The control: without the claim the same planner hands out the plain name, so the suffix
    // above is the claim's work and not this fixture's shape.
    let fresh = TempDir::new().unwrap();
    let fresh_manifest = Manifest::open_in(fresh.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    let recorded = RecordedDirs::read(&newcomer, &fresh_manifest).unwrap();
    let plan = chat_fix::plan(&newcomer, dir.path().join("out"), OverlayMode::Both, &recorded).unwrap();
    assert_eq!(plan.items[0].output.parent().unwrap(), chat_root(&dir).join("a_b"));

    // The claiming conversation's own later media keeps its directory: the claim adopts for the
    // key it names, exactly as a media record would.
    let owner = hand_reconciliation("a?b", "b~tokC");
    let recorded = RecordedDirs::read(&owner, &manifest).unwrap();
    let plan = chat_fix::plan(&owner, dir.path().join("out"), OverlayMode::Both, &recorded).unwrap();
    assert_eq!(plan.items[0].output.parent().unwrap(), chat_root(&dir).join("a_b"));
}

/// A claim is not an item: out of `items`, `pending`, `counts` and the resume sweep, read only
/// through `claims` (decision 63a). The idempotence half rides a backdated sentinel: `unixepoch()`
/// is integer seconds, so two runs landing in one second would read "untouched" even with no skip
/// at all — a sentinel the environment cannot reproduce is what makes the skip observable, and the
/// positive control proves the writer still fires when the claim genuinely moves.
#[test]
fn a_directory_claim_is_out_of_every_item_enumeration_and_never_restamped() {
    let dir = export_tree(&[("a?b", &[("2021-03-04 14:30:05 UTC", "b~tokA")])]);
    let (inputs, state) = inputs(&dir);
    let events = collect(&inputs);
    assert_eq!(report(finished(&events)).conversations, 1);

    let mut manifest = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    assert!(manifest.items(ItemKind::HistoryExport).unwrap().is_empty(), "a claim is not an item");
    assert!(manifest.pending(ItemKind::HistoryExport, 0).unwrap().is_empty(), "a claim is never work");
    assert!(manifest.pending(ItemKind::HistoryExport, 3).unwrap().is_empty());
    let resumed = manifest.resume(ItemKind::HistoryExport).unwrap();
    assert_eq!(
        resumed,
        ResumeReport { demoted: Vec::new(), verified: 0, pending: 0, failed: 0, source_missing: 0, retired: 0, excluded: 0 },
        "the resume sweep surfaces no claim"
    );
    let claims = manifest.claims().unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].kind, ItemKind::HistoryExport);
    assert_eq!(claims[0].source_id, "a?b");
    assert_eq!(claims[0].directory, chat_root(&dir).join("a_b"));

    // Backdate the row, then re-run: the same claim with the same directory touches nothing.
    let db = state.path().join(format!("{EXPORT_ID}.sqlite"));
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE items SET updated_at = 1 WHERE kind = 'history_export' AND source_id = 'a?b'", []).unwrap();
    drop(conn);

    let events = collect(&inputs);
    assert_eq!(report(finished(&events)).conversations, 1, "the one-shot run is idempotent end to end");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let updated: i64 =
        conn.query_row("SELECT updated_at FROM items WHERE kind = 'history_export' AND source_id = 'a?b'", [], |row| row.get(0)).unwrap();
    assert_eq!(updated, 1, "re-claiming the same directory leaves the sentinel alone");

    // The positive control: a claim whose directory moved is re-recorded, so the untouched
    // sentinel is the skip and not a writer that never fires.
    let mut manifest = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    let moved = state.path().join("moved");
    manifest.claim_directories(&[DirectoryClaim { source_id: "a?b", directory: &moved }]).unwrap();
    drop(manifest);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (updated, recorded): (i64, String) = conn
        .query_row("SELECT updated_at, output_path FROM items WHERE kind = 'history_export' AND source_id = 'a?b'", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_ne!(updated, 1, "a changed claim is re-recorded");
    assert_eq!(recorded, moved.to_str().unwrap());
}

/// The run-level half of decision 62: the no-manifest run still lands documents — every media
/// reference a placeholder — and states the reason ONCE, in the report, rather than per message.
#[test]
fn a_run_without_an_export_id_states_the_placeholder_links_once_and_lands_the_documents() {
    let dir = flat_tree(&[("k", &[("2021-03-04 14:30:05 UTC", "b~tokA")])]);
    let (inputs, state) = inputs(&dir);
    let events = collect(&inputs);
    let outcome = report(finished(&events));
    assert_eq!(outcome.conversations, 1);
    assert_eq!(outcome.documents, 4);
    assert_eq!(outcome.links, HtmlLinks::NoManifest, "the run states the no-manifest arm once, in the report");

    let html = fs::read_to_string(dir.path().join("out/chat/k/history.html")).unwrap();
    assert!(html.contains("<span class=\"media-placeholder\">MEDIA</span>"), "{html}");
    assert!(!html.contains("<a href"), "no manifest, no link: {html}");
    assert!(!html.contains("b~tokA"), "the token never reaches the document: {html}");
    assert!(dir.path().join("out/chat/k/history.json").is_file());
    assert!(!state_touched(&state), "nothing to claim into, nothing enrolled");
}

/// The mirror arm: with a manifest present the report says so, and a `done` media row renders a
/// live link in the document the RUN wrote — the writer arms are 78's pins; this pins the run
/// feeding them.
#[test]
fn a_run_with_a_manifest_reports_the_manifest_arm_and_links_done_media() {
    let dir = export_tree(&[("k", &[("2021-03-04 14:30:05 UTC", "b~tokA")])]);
    let (inputs, state) = inputs(&dir);
    let mut manifest = Manifest::open_in(state.path(), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    manifest.enroll(&[NewItem { kind: ItemKind::ChatMedia, source_id: "b~tokA", url: None }]).unwrap();
    let media = dir.path().join("out/chat/k/20210304_143005.jpg");
    fs::create_dir_all(media.parent().unwrap()).unwrap();
    fs::write(&media, b"media bytes").unwrap();
    manifest.mark_done(ItemKind::ChatMedia, "b~tokA", &media).unwrap();
    drop(manifest);

    let events = collect(&inputs);
    let outcome = report(finished(&events));
    assert_eq!(outcome.links, HtmlLinks::Manifest);

    let html = fs::read_to_string(dir.path().join("out/chat/k/history.html")).unwrap();
    assert!(html.contains("<a href=\"20210304_143005.jpg\">20210304_143005.jpg</a>"), "{html}");
}

/// A snap-only conversation merges into one thread of its own and lands its four documents too
/// (decision 61, at the run).
#[test]
fn a_snap_only_conversation_lands_its_documents_too() {
    let dir = TempDir::new().unwrap();
    write_snap_history(&dir.path().join(format!("mydata~{EXPORT_ID}/json")), &[("snapper", &["2021-03-04 09:00:00 UTC"])]);
    let (inputs, _state) = inputs(&dir);
    let events = collect(&inputs);
    assert_eq!(report(finished(&events)).conversations, 1);
    assert!(dir.path().join("out/chat/snapper/history.json").is_file());
    assert!(dir.path().join("out/chat/snapper/history.html").is_file());
}

/// An export holding neither history file has nothing to write, and fails by name rather than
/// writing an empty tree.
#[test]
fn a_run_over_an_export_holding_neither_history_file_fails_by_name() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(format!("mydata~{EXPORT_ID}/json"))).unwrap();
    // This fixture deliberately does not load, so the selection cannot be derived from it — the
    // run's own load fails before the empty-selection guard, which is the error this test names.
    let state = TempDir::new().unwrap();
    let inputs = RunInputs {
        source: dir.path().to_path_buf(),
        out_root: dir.path().join("out"),
        manifest_dir: Some(state.path().to_path_buf()),
        conversations: BTreeSet::new(),
        formats: BTreeSet::from(HistoryFormat::ALL),
    };
    let events = collect(&inputs);
    match finished(&events) {
        RunOutcome::Failed(history_run::RunError::NoHistory(_)) => {}
        other => panic!("expected the no-history failure, got {other:?}"),
    }
}

/// A part whose id cannot name a manifest is refused, the same call `chat_run` makes — the
/// decision-62 no-manifest arm is for a source naming no part group at all, and an unusable id
/// must not read as a clean run with placeholder links and no claim rows.
#[test]
fn a_part_whose_id_cannot_name_a_manifest_is_refused_not_silently_degraded() {
    let dir = TempDir::new().unwrap();
    write_chat_history(&dir.path().join("mydata~bad..id/json"), &[("k", &[("2021-03-04 14:30:05 UTC", "b~tokA")])]);
    // This fixture deliberately does not load, so the selection cannot be derived from it — the
    // run's own load fails before the empty-selection guard, which is the error this test names.
    let state = TempDir::new().unwrap();
    let inputs = RunInputs {
        source: dir.path().to_path_buf(),
        out_root: dir.path().join("out"),
        manifest_dir: Some(state.path().to_path_buf()),
        conversations: BTreeSet::new(),
        formats: BTreeSet::from(HistoryFormat::ALL),
    };
    let events = collect(&inputs);
    match finished(&events) {
        RunOutcome::Failed(history_run::RunError::InvalidExportId(_)) => {}
        other => panic!("expected the invalid-id refusal, got {other:?}"),
    }
    assert!(!state_touched(&state), "nothing to claim into, nothing enrolled");
}

// ---- the run's own selection guards (decision 59, held against a caller bypassing the screen) ----

/// The screen's empty-selection refusal is held again here, run-side: an empty conversation set
/// over a LOADABLE export must refuse with the named error rather than run as "everything" — the
/// screen's load defaults the selection to every conversation, so an empty set is a deliberate
/// deselect the run cannot mistake for "nothing chosen yet". This is the test the [`RunInputs`]
/// `conversations` doc's "held again here so a caller cannot bypass the screen" points at.
#[test]
fn a_run_over_a_loadable_export_refuses_an_empty_conversation_selection() {
    let dir = export_tree(&[("alice", &[("2021-03-04 09:00:00 UTC", "b~tokA")]), ("bob", &[("2021-03-04 10:00:00 UTC", "b~tokB")])]);
    let state = TempDir::new().unwrap();
    let inputs = RunInputs {
        source: dir.path().to_path_buf(),
        out_root: dir.path().join("out"),
        manifest_dir: Some(state.path().to_path_buf()),
        conversations: BTreeSet::new(),
        formats: BTreeSet::from(HistoryFormat::ALL),
    };
    let events = collect(&inputs);
    match finished(&events) {
        RunOutcome::Failed(history_run::RunError::NoSelection) => {}
        other => panic!("expected the empty-selection refusal, got {other:?}"),
    }
    assert!(!state_touched(&state), "nothing to claim into, nothing enrolled");
}

/// An empty format set refuses BEFORE anything is read — the guard's position is its contract: it
/// is independent of the export, so even a source that cannot load refuses with the format reason
/// rather than the load's. Removing the guard's position (moving it after the load) reds this: a
/// source with no history files would then answer `NoHistory` first.
#[test]
fn a_run_refuses_an_empty_format_selection_before_reading_the_export() {
    let dir = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let inputs = RunInputs {
        source: dir.path().to_path_buf(),
        out_root: dir.path().join("out"),
        manifest_dir: Some(state.path().to_path_buf()),
        conversations: BTreeSet::new(),
        formats: BTreeSet::new(),
    };
    let events = collect(&inputs);
    match finished(&events) {
        RunOutcome::Failed(history_run::RunError::NoFormats) => {}
        other => panic!("expected the empty-format refusal, got {other:?}"),
    }
    assert!(!state_touched(&state), "nothing to claim into, nothing enrolled");
}

/// The conversation filter's slice of the run: a partial selection writes exactly the selected
/// conversations in exactly the selected formats, and the unselected conversation's directory is
/// never created. Removing the filter in `history_run`'s `prepare` reds this — the run then
/// writes everything, which is the refusal's own bug.
#[test]
fn a_run_writes_only_the_selected_conversations_and_formats() {
    let dir = export_tree(&[("alice", &[("2021-03-04 09:00:00 UTC", "b~tokA")]), ("bob", &[("2021-03-04 10:00:00 UTC", "b~tokB")])]);
    let state = TempDir::new().unwrap();
    let loaded = history_run::load_threads(dir.path()).expect("the fixture export loads");
    let alice: ConversationId = loaded.merged.threads.iter().find(|thread| thread.id.as_str() == "alice").expect("alice loads").id.clone();
    let inputs = RunInputs {
        source: dir.path().to_path_buf(),
        out_root: dir.path().join("out"),
        manifest_dir: Some(state.path().to_path_buf()),
        conversations: BTreeSet::from([alice]),
        formats: BTreeSet::from([HistoryFormat::Json, HistoryFormat::Csv]),
    };
    let events = collect(&inputs);
    let outcome = report(finished(&events));
    assert_eq!(outcome.conversations, 1);
    assert_eq!(outcome.documents, 2, "one conversation times the two selected formats");

    assert!(dir.path().join("out/chat/alice/history.json").is_file());
    assert!(dir.path().join("out/chat/alice/history.csv").is_file());
    assert!(!dir.path().join("out/chat/alice/history.html").exists(), "an unselected format is not written");
    assert!(!dir.path().join("out/chat/alice/history.txt").exists());
    assert!(!dir.path().join("out/chat/bob").exists(), "the unselected conversation's directory is never created");
}

/// The report names whether html was actually written: a run over a no-manifest source with html
/// NOT selected has no links at all — [`HistoryReport::html_written`] is the run half of the
/// alert's "media links are placeholders" clause, which must not arm over a silence.
#[test]
fn a_run_without_html_reports_that_no_html_was_written() {
    let dir = flat_tree(&[("k", &[("2021-03-04 14:30:05 UTC", "b~tokA")])]);
    let state = TempDir::new().unwrap();
    let loaded = history_run::load_threads(dir.path()).expect("the fixture export loads");
    let conversations: BTreeSet<ConversationId> = loaded.merged.threads.iter().map(|thread| thread.id.clone()).collect();
    let inputs = RunInputs {
        source: dir.path().to_path_buf(),
        out_root: dir.path().join("out"),
        manifest_dir: Some(state.path().to_path_buf()),
        conversations,
        formats: BTreeSet::from([HistoryFormat::Json]),
    };
    let events = collect(&inputs);
    let outcome = report(finished(&events));
    assert_eq!(outcome.links, HtmlLinks::NoManifest);
    assert!(!outcome.html_written, "html was never written");
    assert_eq!(outcome.documents, 1);
    assert!(dir.path().join("out/chat/k/history.json").is_file());
    assert!(!dir.path().join("out/chat/k/history.html").exists());
}

/// The attribution map the run derives from `chat_history.json` alone holds exactly the tokens the
/// join's own grammar could name — a token the join refuses is absent from the map exactly as it is
/// absent from a reconciliation, so the two attributions cannot drift (decision 60).
#[test]
fn the_attribution_map_holds_only_the_tokens_the_join_could_name() {
    let chat = chat_from(vec![("k", vec![chat_entry_with_media("2021-03-04 09:00:00 UTC", None, "b~tokA | media~m1 | not a token")])]);
    let map = chat_media::history_attribution(&chat);
    assert_eq!(map.len(), 1, "one joinable token: {map:?}");
    assert_eq!(map.get("b~tokA"), Some(&&ConversationId::new("k")));
    assert!(!map.contains_key("media~m1"), "a token the join grammar refuses is absent from the attribution");
    assert!(!map.contains_key("not a token"));
}

/// A token spelling its prefix loudly must be keyed by the CANONICAL spelling the join mints rows
/// under, not by the raw `Media IDs` text: the manifest's `source_id` is `b~<id>`, so a map keyed
/// on the raw spelling misses the row the join itself would have attributed and the history
/// planner then derives a second directory beside the reserved one (decision 60).
#[test]
fn the_attribution_map_keys_a_shouted_prefix_by_the_canonical_spelling() {
    let chat = chat_from(vec![("k", vec![chat_entry_with_media("2021-03-04 09:00:00 UTC", None, "B~tokB")])]);
    let map = chat_media::history_attribution(&chat);
    assert_eq!(map.len(), 1);
    assert_eq!(map.get("b~tokB"), Some(&&ConversationId::new("k")), "the canonical spelling is the key");
    assert!(!map.contains_key("B~tokB"), "the raw spelling is not a key: {map:?}");
}

/// The whole composition over the real redacted export: one directory per merged conversation,
/// four documents in each, nothing else, and a claim per conversation. Counts only — the
/// directory names are key-derived, so no name reaches a failure message.
#[test]
fn a_run_over_the_real_export_lands_four_documents_per_conversation() {
    let Some(root) = common::fixtures::root("a_run_over_the_real_export_lands_four_documents_per_conversation") else {
        return;
    };
    let state = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let inputs = full_inputs(&root, out.path(), state.path());
    let events = collect(&inputs);
    let outcome = report(finished(&events));
    assert_eq!(outcome.links, HtmlLinks::Manifest, "the fixture's part id names a manifest");

    let export = ExportJson::load_dir(root.join("mydata~xxxxxxxxxxxx/json")).expect("the fixture export loads");
    let merged = merge(
        export.chat_history.as_ref().expect("the fixture carries chat_history.json"),
        export.snap_history.as_ref().expect("the fixture carries snap_history.json"),
    );
    assert_eq!(outcome.conversations, merged.threads.len(), "one directory per merged conversation");
    assert_eq!(outcome.documents, merged.threads.len() * 4);

    let mut directories = 0;
    for entry in fs::read_dir(out.path().join("chat")).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.path().is_dir(), "nothing but conversation directories under chat/");
        directories += 1;
        let mut names: Vec<String> =
            fs::read_dir(entry.path()).unwrap().map(|file| file.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["history.csv", "history.html", "history.json", "history.txt"], "four documents per conversation");
    }
    assert_eq!(directories, merged.threads.len());

    let manifest = Manifest::open_in(state.path(), &ExportId::new("xxxxxxxxxxxx").unwrap()).unwrap();
    assert_eq!(manifest.claims().unwrap().len(), merged.threads.len(), "one claim row per conversation");
}

fn state_touched(state: &TempDir) -> bool {
    fs::read_dir(state.path()).unwrap().next().is_some()
}
