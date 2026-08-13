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

use exportsnap::export::ExportJson;
use exportsnap::export::history::{Document, HtmlLinks, Record, RecordKind, merge, write_csv, write_html, write_json, write_text};
use exportsnap::export::manifest::{ExportId, ItemKind, Manifest, NewItem};
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
    let output = workspace.path().join("we\"ird.jpg");
    fs::write(&output, b"media bytes").unwrap();
    manifest.mark_done(ItemKind::ChatMedia, token, &output).unwrap();

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
