//! Public-API tests for `exportsnap::export::manifest`: what a second run skips, what the resume
//! sweep takes back off the finished pile, and what never reaches the manifest's text columns.
//!
//! Every database and every "media" file here is built inside the test. Nothing reads a real
//! export, and the per-user data dir is never touched: every manifest is opened with `open_in`
//! against a tempdir.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use exportsnap::export::manifest::{
    Checksum, Demotion, DemotionReason, ExportId, Item, ItemKind, ItemStatus, Manifest, ManifestError, NewItem, PathProblem, manifest_dir,
};
use exportsnap::export::memories::UnreadableDir;
use exportsnap::export::model::DownloadUrl;
use exportsnap::export::zip::discover_parts;
use rusqlite::OptionalExtension;
use tempfile::TempDir;

/// The 13-digit id shape the one observed export used.
const ID: &str = "1784667002819";

/// A signed url of the shape the export's `Media Download Url` carries. Synthetic: the signature
/// is a word this test greps for, not a real one.
const SIGNED_URL: &str = "https://cf-st.sc-cdn.net/d/abc123?uc=12&sig=SECRETSIGNATURE";

/// The `sig` value out of [`SIGNED_URL`] on its own, as a caller that split its own url apart
/// would hand it over.
const SIGNATURE: &str = "SECRETSIGNATURE";

/// A tempdir plus the two dirs the tests use: manifest state, and where "media" lands.
struct Workspace {
    _temp: TempDir,
    state: PathBuf,
    outputs: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let state = temp.path().join("state");
        let outputs = temp.path().join("out");
        fs::create_dir_all(&outputs).unwrap();
        Self { _temp: temp, state, outputs }
    }

    fn open(&self) -> Manifest {
        self.open_as(ID)
    }

    fn open_as(&self, id: &str) -> Manifest {
        Manifest::open_in(&self.state, &ExportId::new(id).unwrap()).unwrap()
    }

    /// A source dir holding one unpacked export part named exactly as the delivery names it,
    /// `json/` included, so `discover_parts` sees the dir the brief calls the export's identity.
    fn source_with_export_dir(&self, dir_name: &str) -> PathBuf {
        let source = self._temp.path().join("source");
        fs::create_dir_all(source.join(dir_name).join("json")).unwrap();
        source
    }

    /// A finished output file whose bytes are unique to `source_id`, so a manifest that mixed two
    /// items' checksums up would not verify.
    fn write_output(&self, source_id: &str, body: &str) -> PathBuf {
        let path = self.outputs.join(format!("{source_id}.jpg"));
        fs::write(&path, body).unwrap();
        path
    }

    /// An enrolled row driven all the way to `Done` through the real check-in, returning what the
    /// three output columns now hold.
    ///
    /// The assertion inside is a fixture guard, NOT the vacuity control for the tests below. Each of
    /// those owns its own: the `keeps the record` ones assert `Some(..)` in every output column, and
    /// the `drops the record` ones — whose every assertion is otherwise an enrollment default —
    /// `assert_output_kept` on the row before the transition and watch a field the transition itself
    /// writes. Scored with the mutant that deletes the `mark_done` below AND this assertion together;
    /// deleting only `mark_done` measures this assertion rather than the tests.
    fn finish(&self, manifest: &Manifest, source_id: &str, body: &'static str) -> Finished {
        let output = self.write_output(source_id, body);
        manifest.mark_done(ItemKind::Memory, source_id, &output).unwrap();
        let (checksum, bytes) = Checksum::of_file(&output).unwrap();
        let item = manifest.item(ItemKind::Memory, source_id).unwrap().unwrap();
        assert_eq!(
            (item.status, item.output_path.as_deref(), item.checksum, item.bytes),
            (ItemStatus::Done, Some(output.as_path()), Some(checksum), Some(bytes)),
            "the fixture has to start from a row that really carries an output record"
        );
        Finished { output, body, checksum, bytes }
    }
}

/// What [`Workspace::finish`] left on disk and in the row, so a later assertion compares against
/// recomputed values rather than against whatever the row happens to say now.
struct Finished {
    output: PathBuf,
    body: &'static str,
    checksum: Checksum,
    bytes: u64,
}

fn enrollment<'a>(source_ids: &'a [&'a str]) -> Vec<NewItem<'a>> {
    source_ids.iter().map(|source_id| NewItem { kind: ItemKind::Memory, source_id, url: None }).collect()
}

fn owed(manifest: &Manifest, max_attempts: u32) -> Vec<String> {
    manifest.pending(ItemKind::Memory, max_attempts).unwrap().into_iter().map(|item| item.source_id).collect()
}

fn status_of(manifest: &Manifest, source_id: &str) -> ItemStatus {
    manifest.item(ItemKind::Memory, source_id).unwrap().expect("the row is enrolled").status
}

// ---- the resume contract ----

#[test]
fn a_second_run_skips_exactly_the_completed_items() {
    let work = Workspace::new();
    let sources = ["m-01", "m-02", "m-03", "m-04", "m-05"];

    // First run: enrol five, finish two of them, then die without finishing the rest. The two are
    // interleaved rather than a prefix so "skipped the first n" cannot pass as "skipped the done
    // ones".
    {
        let mut manifest = work.open();
        manifest.enroll(&enrollment(&sources)).unwrap();
        for source_id in ["m-02", "m-04"] {
            let output = work.write_output(source_id, &format!("bytes belonging to {source_id}"));
            manifest.mark_done(ItemKind::Memory, source_id, &output).unwrap();
        }
    }

    // Second run: same enumeration, same order, from a manifest reopened off disk.
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&sources)).unwrap();
    let report = manifest.resume(ItemKind::Memory).unwrap();

    assert_eq!(report.demoted, Vec::new(), "nothing on disk changed between the runs");
    assert_eq!(report.verified, 2);
    assert_eq!(report.pending, 3);
    assert_eq!(owed(&manifest, 3), ["m-01", "m-03", "m-05"]);
}

#[test]
fn resume_demotes_a_finished_item_whose_bytes_changed_at_the_same_length() {
    let work = Workspace::new();
    let before = "the bytes checked in";
    let after = "the bytes swapped in";
    // The point of the fixture: a length comparison cannot tell these apart, so only the checksum
    // can. If this ever fails, the two strings drifted and the test stopped proving anything.
    assert_eq!(before.len(), after.len(), "the replacement has to be the same length to test the checksum");

    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
    let output = work.write_output("m-01", before);
    manifest.mark_done(ItemKind::Memory, "m-01", &output).unwrap();
    fs::write(&output, after).unwrap();

    let report = manifest.resume(ItemKind::Memory).unwrap();

    assert_eq!(report.demoted, vec![Demotion { kind: ItemKind::Memory, source_id: "m-01".to_owned(), reason: DemotionReason::Changed }]);
    assert_eq!(report.verified, 0);
    assert_eq!(owed(&manifest, 3), ["m-01", "m-02"]);

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(item.status, ItemStatus::Pending);
    assert_eq!(item.checksum, None, "a demoted item keeps no checksum");
    assert_eq!(item.output_path, None);
    assert_eq!(item.bytes, None);
}

#[test]
fn resume_demotes_a_finished_item_whose_file_vanished() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
    let output = work.write_output("m-01", "bytes that will be deleted");
    manifest.mark_done(ItemKind::Memory, "m-01", &output).unwrap();
    fs::remove_file(&output).unwrap();

    let report = manifest.resume(ItemKind::Memory).unwrap();

    assert_eq!(report.demoted, vec![Demotion { kind: ItemKind::Memory, source_id: "m-01".to_owned(), reason: DemotionReason::Vanished }]);
    assert_eq!(report.verified, 0);
    assert_eq!(owed(&manifest, 3), ["m-01", "m-02"]);
}

#[test]
fn resume_leaves_a_finished_item_whose_bytes_are_untouched() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
    let output = work.write_output("m-01", "bytes nobody touches");
    manifest.mark_done(ItemKind::Memory, "m-01", &output).unwrap();

    let report = manifest.resume(ItemKind::Memory).unwrap();

    assert_eq!(report.demoted, Vec::new());
    assert_eq!(report.verified, 1);
    assert_eq!(owed(&manifest, 3), ["m-02"]);

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(item.status, ItemStatus::Done);
    assert_eq!(item.output_path.as_deref(), Some(output.as_path()));
    assert_eq!(item.bytes, Some("bytes nobody touches".len() as u64));
    let (expected, _) = Checksum::of_file(&output).unwrap();
    assert_eq!(item.checksum, Some(expected));
}

#[test]
fn a_finished_row_with_no_checksum_is_demoted_rather_than_trusted() {
    let work = Workspace::new();
    let output = {
        let mut manifest = work.open();
        manifest.enroll(&enrollment(&["m-01"])).unwrap();
        let output = work.write_output("m-01", "bytes with a checksum, for now");
        manifest.mark_done(ItemKind::Memory, "m-01", &output).unwrap();
        output
    };

    // Only reachable by editing the database outside exportsnap, which is exactly the case this
    // arm exists for: a row claiming to be finished with nothing to check it against.
    let db = work.state.join(format!("{ID}.sqlite"));
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE items SET checksum = NULL", []).unwrap();
    drop(conn);
    assert!(output.exists(), "the file is still there, so only the missing checksum can demote it");

    let mut manifest = work.open();
    let report = manifest.resume(ItemKind::Memory).unwrap();

    assert_eq!(report.demoted, vec![Demotion { kind: ItemKind::Memory, source_id: "m-01".to_owned(), reason: DemotionReason::Incomplete }]);
    assert_eq!(owed(&manifest, 3), ["m-01"]);
}

// ---- the states a run moves through ----

#[test]
fn a_source_missing_item_is_reported_but_never_handed_back_as_work() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
    manifest.mark_source_missing(ItemKind::Memory, "m-01", "no -main file on disk and no download link").unwrap();

    let report = manifest.resume(ItemKind::Memory).unwrap();

    assert_eq!(report.source_missing, 1, "the gap has to be countable, not silent");
    assert_eq!(report.pending, 1);
    assert_eq!(owed(&manifest, 3), ["m-02"], "a missing source is not work a run can retry");

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(item.status, ItemStatus::SourceMissing);
    assert_eq!(item.retry_count, 0, "finding no source is not a failed attempt");
    assert_eq!(item.last_error.as_deref(), Some("no -main file on disk and no download link"));
}

#[test]
fn reset_puts_a_source_missing_item_back_on_the_work_list() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
    manifest.mark_source_missing(ItemKind::Memory, "m-01", "no media for it in the parts extracted so far").unwrap();
    assert_eq!(owed(&manifest, 3), ["m-02"]);

    manifest.reset(ItemKind::Memory, "m-01").unwrap();

    assert_eq!(owed(&manifest, 3), ["m-01", "m-02"]);
    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(item.status, ItemStatus::Pending);
    assert_eq!(item.last_error, None);
}

/// Both media legs re-derive their gaps from scratch on every run and re-state every one of them —
/// 90 rows on the observed export — so a `mark_source_missing` that wrote unconditionally would turn
/// `updated_at` into the last RUN for exactly the rows that answer "when did this vanish".
///
/// The sentinel is load-bearing, not decoration: `unixepoch()` has one-second resolution, so a second
/// call landing in the same second as the first rewrites the row with the value it already had and no
/// assertion could tell a rewrite from a skip.
///
/// **The still-`Pending` row is the positive control.** Without it a `mark_source_missing` that had
/// stopped writing anything at all would pass, because a dead call leaves a backdated row alone just
/// as well as a correct one does.
#[test]
fn re_marking_a_gap_row_with_the_same_reason_leaves_it_untouched() {
    /// A timestamp far enough in the past that `unixepoch()` cannot produce it during this test.
    const SENTINEL: i64 = 1_000_000_000;
    const REASON: &str = "the export holds no memory media for this entry's day and kind";

    let work = Workspace::new();
    {
        let mut manifest = work.open();
        manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
        manifest.mark_source_missing(ItemKind::Memory, "m-01", REASON).unwrap();
        assert_eq!(status_of(&manifest, "m-01"), ItemStatus::SourceMissing, "the row the next run must leave alone");
        assert_eq!(status_of(&manifest, "m-02"), ItemStatus::Pending, "and the one it must still be able to park");
    }

    // Only reachable by editing the database, which is the point: it backdates both rows so a
    // rewrite is distinguishable from a skip.
    let conn = rusqlite::Connection::open(work.state.join(format!("{ID}.sqlite"))).unwrap();
    conn.execute("UPDATE items SET updated_at = ?1", [SENTINEL]).unwrap();
    drop(conn);

    // The second run, re-deriving the same gap and re-stating it verbatim.
    let manifest = work.open();
    manifest.mark_source_missing(ItemKind::Memory, "m-01", REASON).unwrap();
    manifest.mark_source_missing(ItemKind::Memory, "m-02", REASON).unwrap();

    let untouched = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(untouched.updated_at, SENTINEL, "a second run rewrote a row that says exactly what it said before");
    assert_eq!(untouched.status, ItemStatus::SourceMissing, "and it is still a gap, so the timestamp did not survive by the row moving");
    assert_eq!(untouched.last_error.as_deref(), Some(REASON), "and it still says why");
    let control = manifest.item(ItemKind::Memory, "m-02").unwrap().unwrap();
    assert_eq!(control.status, ItemStatus::SourceMissing, "positive control: the same run did run, and did park a new row");
    assert_ne!(control.updated_at, SENTINEL, "and it stamped that transition");
}

/// The write direction, one leg per half of the guard's condition: a guard is only as good as the
/// narrowest thing it still lets through, and each half is what lets one of these two rows through.
///
/// The REASON leg is why this is not `exclude`'s bare `status <> ?1`. That note is a constant the
/// module owns; this one is CALLER TEXT, and `MissingReason::Unscanned` is scan-wide, so one
/// unlistable directory makes a run write it for every unpaired entry and the next clean run has to
/// replace it on all of them. A status-only guard would freeze the stale reason with the status
/// column reading correct.
///
/// The STATUS leg is the mirror, and it is the half a note-only guard would drop: a failed attempt
/// whose note already reads like a gap's is still not a gap until this call says so.
#[test]
fn a_gap_row_is_written_whenever_its_status_or_its_reason_differs() {
    const SENTINEL: i64 = 1_000_000_000;
    const UNSCANNED: &str = "part of the source could not be listed, so media for this entry may exist but was never seen";
    const NO_MEDIA: &str = "the export holds no memory media for this entry's day and kind";

    let work = Workspace::new();
    {
        let mut manifest = work.open();
        manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
        manifest.mark_source_missing(ItemKind::Memory, "m-01", UNSCANNED).unwrap();
        manifest.mark_failed(ItemKind::Memory, "m-02", NO_MEDIA).unwrap();
        let failed = manifest.item(ItemKind::Memory, "m-02").unwrap().unwrap();
        // Both notes have to be the SAME stored bytes or the status leg is testing a note that
        // differs too, and the redactor is between the caller and the column on both paths.
        assert_eq!(
            failed.last_error.as_deref(),
            Some(NO_MEDIA),
            "the note reached the column verbatim, which is what the leg below rests on"
        );
    }
    let conn = rusqlite::Connection::open(work.state.join(format!("{ID}.sqlite"))).unwrap();
    conn.execute("UPDATE items SET updated_at = ?1", [SENTINEL]).unwrap();
    drop(conn);

    // The run with the directory readable again: a verdict the last run could not make.
    let manifest = work.open();
    manifest.mark_source_missing(ItemKind::Memory, "m-01", NO_MEDIA).unwrap();
    manifest.mark_source_missing(ItemKind::Memory, "m-02", NO_MEDIA).unwrap();

    let reason_changed = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(reason_changed.last_error.as_deref(), Some(NO_MEDIA), "a run that learned why has to be able to say so");
    assert_ne!(reason_changed.updated_at, SENTINEL, "and that is a change to what the row records, so it is stamped");
    assert_eq!(reason_changed.status, ItemStatus::SourceMissing);

    let status_changed = manifest.item(ItemKind::Memory, "m-02").unwrap().unwrap();
    assert_eq!(status_changed.status, ItemStatus::SourceMissing, "a matching note does not make a failed attempt a gap already");
    assert_ne!(status_changed.updated_at, SENTINEL, "and parking it is a transition, so it is stamped");
}

/// The guard's `IS NOT` rather than the `<>` a reader reaches for first. `last_error` is nullable and
/// SQL defines `<>` against `NULL` as `NULL` — which is not true — so the plain operator would read a
/// note-less gap row as "already says this" and never give it one.
///
/// No call in this crate produces that row: `mark_source_missing` always writes a note. It is reached
/// by editing the database, the same way the `Incomplete` demotion above reaches a `Done` row with no
/// checksum, so the operator choice is pinned rather than resting on the comment next to it.
#[test]
fn a_gap_row_carrying_no_note_is_given_one_rather_than_read_as_already_saying_it() {
    const SENTINEL: i64 = 1_000_000_000;
    const REASON: &str = "the export holds no memory media for this entry's day and kind";

    let work = Workspace::new();
    {
        let mut manifest = work.open();
        manifest.enroll(&enrollment(&["m-01"])).unwrap();
        manifest.mark_source_missing(ItemKind::Memory, "m-01", REASON).unwrap();
    }
    let conn = rusqlite::Connection::open(work.state.join(format!("{ID}.sqlite"))).unwrap();
    conn.execute("UPDATE items SET last_error = NULL, updated_at = ?1", [SENTINEL]).unwrap();
    drop(conn);

    let manifest = work.open();
    manifest.mark_source_missing(ItemKind::Memory, "m-01", REASON).unwrap();

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(item.last_error.as_deref(), Some(REASON), "a null note is not the note the caller passed");
    assert_ne!(item.updated_at, SENTINEL, "and writing it is a change to what the row records, so it is stamped");
}

#[test]
fn marking_a_gap_on_a_row_no_run_enrolled_is_refused_rather_than_silently_doing_nothing() {
    // The guard above means a zero row count no longer implies the row is absent, so this is the case
    // that would go quiet if the read that discriminates them were dropped — and the case that reds
    // every already-parked row as unknown if the read were left out entirely.
    let work = Workspace::new();
    let manifest = work.open();
    match manifest.mark_source_missing(ItemKind::Memory, "never-enrolled", "no media for it") {
        Err(ManifestError::UnknownItem { kind, source_id }) => {
            assert_eq!((kind, source_id.as_str()), (ItemKind::Memory, "never-enrolled"));
        }
        other => panic!("expected UnknownItem, got {other:?}"),
    }
}

/// The sweep's whole rule in one fixture, one row per verdict: every status an unnamed row can be
/// at is retired, and `Done` is the exemption — its bytes are on disk and checksum-verified, so the
/// source leaving the export does not un-do the work.
#[test]
fn retiring_takes_every_unnamed_row_except_the_finished_ones() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02", "m-03", "m-04", "m-05"])).unwrap();

    let output = work.write_output("m-02", "bytes a run finished");
    manifest.mark_done(ItemKind::Memory, "m-02", &output).unwrap();
    manifest.mark_failed(ItemKind::Memory, "m-03", "connection reset").unwrap();
    manifest.mark_source_missing(ItemKind::Memory, "m-04", "no media for it in the parts extracted so far").unwrap();

    // The next run's enumeration names one row of the five; the export no longer names the rest
    // under any identity.
    manifest.retire_absent(ItemKind::Memory, &BTreeSet::from(["m-05"]), &[]).unwrap();

    assert_eq!(status_of(&manifest, "m-01"), ItemStatus::Retired, "a pending row nothing names");
    assert_eq!(status_of(&manifest, "m-02"), ItemStatus::Done, "finished bytes are not swept");
    assert_eq!(status_of(&manifest, "m-03"), ItemStatus::Retired, "a failed row nothing names");
    assert_eq!(status_of(&manifest, "m-04"), ItemStatus::Retired, "and a gap row nothing names either");
    assert_eq!(status_of(&manifest, "m-05"), ItemStatus::Pending, "the one the enumeration still names");

    let retired = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert!(retired.last_error.unwrap().contains("no longer holds a source"), "a retired row says what happened to it");
    assert_eq!(retired.retry_count, 0, "retiring a row is not a failed attempt");
    let finished = manifest.item(ItemKind::Memory, "m-02").unwrap().unwrap();
    assert_eq!(finished.output_path.as_deref(), Some(output.as_path()), "and the finished row keeps what a resume re-checks");
    assert!(finished.checksum.is_some());
}

/// The guard, which is the part of the sweep that can lose data. A directory the walk could not list
/// is not evidence a row's source is gone, so one of them stops the sweep for EVERY row: nothing can
/// say whether this row's file was in the dir that could not be read without reading it.
#[test]
fn retiring_sweeps_nothing_while_a_directory_could_not_be_listed() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
    let locked = [UnreadableDir { dir: PathBuf::from("/export/lost+found"), kind: io::ErrorKind::PermissionDenied }];

    manifest.retire_absent(ItemKind::Memory, &BTreeSet::new(), &locked).unwrap();

    assert_eq!(status_of(&manifest, "m-01"), ItemStatus::Pending, "nothing named it, and nothing established it is gone either");
    assert_eq!(status_of(&manifest, "m-02"), ItemStatus::Pending);
    assert_eq!(owed(&manifest, 3), ["m-01", "m-02"], "so both are still work");

    // The same call with the scan complete retires them, which is what proves the guard is what left
    // the rows standing rather than the fixture never reaching the sweep at all.
    manifest.retire_absent(ItemKind::Memory, &BTreeSet::new(), &[]).unwrap();
    assert_eq!(status_of(&manifest, "m-01"), ItemStatus::Retired);
    assert_eq!(status_of(&manifest, "m-02"), ItemStatus::Retired);
}

/// The sweep has to be idempotent, and `updated_at` is the field that proves it. A retired row is
/// unnamed by definition — being unnamed is why it was retired — so it re-enters the sweep on every
/// later run unless it is exempt, and the `UPDATE` would reset a timestamp `Item` documents as the
/// last time the row's own state moved. That destroys the "when did this vanish" half of what a
/// retired row is kept for.
///
/// The sentinel is load-bearing, not decoration: `unixepoch()` has one-second resolution, so a second
/// sweep landing in the same second as the first rewrites the row with the value it already had and
/// no assertion could tell a rewrite from a skip.
///
/// **The still-`Pending` row is the positive control, and it shares the one call.** Without it a
/// `retire_absent` that did nothing whatever would pass, because a dead sweep leaves a sentinel alone
/// just as well as a correct one does.
#[test]
fn retiring_leaves_an_already_retired_row_untouched() {
    /// A timestamp far enough in the past that `unixepoch()` cannot produce it during this test.
    const SENTINEL: i64 = 1_000_000_000;

    let work = Workspace::new();
    {
        let mut manifest = work.open();
        manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
        manifest.retire_absent(ItemKind::Memory, &BTreeSet::from(["m-02"]), &[]).unwrap();
        assert_eq!(status_of(&manifest, "m-01"), ItemStatus::Retired, "the row the next sweep must leave alone");
        assert_eq!(status_of(&manifest, "m-02"), ItemStatus::Pending, "and the one it must still be able to retire");
    }

    // Only reachable by editing the database, which is the point: it backdates the row so a rewrite
    // is distinguishable from a skip.
    let db = work.state.join(format!("{ID}.sqlite"));
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE items SET updated_at = ?1 WHERE source_id = 'm-01'", [SENTINEL]).unwrap();
    drop(conn);

    let mut manifest = work.open();
    // Neither row is named now: `m-01` is already retired, `m-02` has just left the export.
    manifest.retire_absent(ItemKind::Memory, &BTreeSet::new(), &[]).unwrap();

    let untouched = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(untouched.updated_at, SENTINEL, "a second sweep rewrote a row whose status never transitioned");
    assert_eq!(untouched.status, ItemStatus::Retired, "and it is still retired, so the timestamp did not survive by the row going away");
    assert_eq!(status_of(&manifest, "m-02"), ItemStatus::Retired, "positive control: that same call did run, and did retire a new row");
}

#[test]
fn a_retired_item_is_reported_apart_from_the_gap_and_never_handed_back_as_work() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02", "m-03"])).unwrap();
    manifest.mark_source_missing(ItemKind::Memory, "m-02", "no media for it in the parts extracted so far").unwrap();

    manifest.retire_absent(ItemKind::Memory, &BTreeSet::from(["m-02", "m-03"]), &[]).unwrap();

    let report = manifest.resume(ItemKind::Memory).unwrap();
    assert_eq!(report.retired, 1, "the row whose whole record left the export");
    assert_eq!(report.source_missing, 1, "counted apart from the gap the export still names");
    assert_eq!(report.pending, 1);
    assert_eq!(owed(&manifest, 3), ["m-03"], "and neither of them is work");

    // A source that comes back takes the same way out of a retired row as out of a gap.
    manifest.reset(ItemKind::Memory, "m-01").unwrap();
    assert_eq!(status_of(&manifest, "m-01"), ItemStatus::Pending);
    assert_eq!(owed(&manifest, 3), ["m-01", "m-03"]);
}

#[test]
fn a_failed_item_comes_back_until_it_hits_the_attempt_cap() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();

    manifest.mark_failed(ItemKind::Memory, "m-01", "connection reset").unwrap();
    assert_eq!(owed(&manifest, 2), ["m-01", "m-02"], "one recorded failure is under a cap of two");

    manifest.mark_failed(ItemKind::Memory, "m-01", "connection reset again").unwrap();
    assert_eq!(owed(&manifest, 2), ["m-02"], "two recorded failures is not under a cap of two");
    assert_eq!(owed(&manifest, 3), ["m-01", "m-02"], "and a caller willing to try three times still gets it");

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(item.retry_count, 2);
    assert_eq!(item.status, ItemStatus::Failed);
    assert_eq!(item.last_error.as_deref(), Some("connection reset again"));
}

#[test]
fn re_enrolling_refreshes_the_url_without_touching_progress() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();
    let output = work.write_output("m-01", "already finished bytes");
    manifest.mark_done(ItemKind::Memory, "m-01", &output).unwrap();

    let url = DownloadUrl::new(SIGNED_URL);
    manifest.enroll(&[NewItem { kind: ItemKind::Memory, source_id: "m-01", url: Some(&url) }]).unwrap();

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(item.status, ItemStatus::Done, "re-enumerating an export must not cost finished work");
    assert_eq!(item.output_path.as_deref(), Some(output.as_path()));
    assert_eq!(item.url.map(|url| url.expose().to_owned()).as_deref(), Some(SIGNED_URL));
}

// ---- secrets ----

#[test]
fn a_stored_download_url_never_reaches_a_debug_render() {
    let work = Workspace::new();
    let mut manifest = work.open();
    let url = DownloadUrl::new(SIGNED_URL);
    manifest.enroll(&[NewItem { kind: ItemKind::Memory, source_id: "m-01", url: Some(&url) }]).unwrap();

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    // The url really is stored, so the assertions below are not passing on an empty column.
    assert_eq!(item.url.as_ref().map(DownloadUrl::expose), Some(SIGNED_URL));

    let rendered = format!("{item:?}");
    assert!(!rendered.contains("SECRETSIGNATURE"), "{rendered}");
    assert!(!rendered.contains("cf-st.sc-cdn.net"), "{rendered}");
    assert!(rendered.contains("DownloadUrl(<redacted>)"), "the url field is rendered, just not its value: {rendered}");
    assert!(rendered.contains("m-01"), "the rest of the row still has to be debuggable: {rendered}");
}

#[test]
fn a_failure_note_carrying_a_signed_url_is_stored_without_it() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();

    // Shaped like `reqwest`'s own `Display`, which puts the url it was fetching in the message.
    manifest.mark_failed(ItemKind::Memory, "m-01", &format!("error sending request for url ({SIGNED_URL}): timed out")).unwrap();

    // The `(` and `):` go with the url because redaction is per whitespace-separated token, and
    // splitting punctuation off the token would mean deciding which punctuation belongs to a url —
    // the detector this deliberately is not. The prose either side survives, which is the readable
    // part.
    let note = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap().last_error.unwrap();
    assert_eq!(note, "error sending request for url <redacted> timed out");
}

/// The stripper is an allowlist, so a spelling nobody anticipated is dropped rather than passed.
/// Every case here walks straight through a `://` detector, which is what this replaced.
#[test]
fn a_failure_note_is_stripped_of_url_spellings_no_scheme_check_would_catch() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();

    let cases = [
        // No scheme at all.
        ("GET cf-st.sc-cdn.net/d/abc123?uc=12&sig=SECRETSIGNATURE failed", "GET <redacted> failed"),
        // Percent-encoded, the way a url nested in another url's query arrives.
        ("target https%3A%2F%2Fcf-st.sc-cdn.net%2Fd%3Fsig%3DSECRETSIGNATURE gone", "target <redacted> gone"),
        // Upper-cased scheme.
        ("HTTPS://CF-ST.SC-CDN.NET/D?SIG=SECRETSIGNATURE refused", "<redacted> refused"),
        // Quoted, and angle-bracketed.
        ("fetching \"https://cf-st.sc-cdn.net/d?sig=SECRETSIGNATURE\" died", "fetching <redacted> died"),
        ("fetching <https://cf-st.sc-cdn.net/d?sig=SECRETSIGNATURE> died", "fetching <redacted> died"),
        // A bare host carrying only a query.
        ("cf-st.sc-cdn.net?sig=SECRETSIGNATURE unreachable", "<redacted> unreachable"),
        // An opaque run long enough to be payload on its own, holding no url punctuation at all.
        ("signature SECRETSIGNATUREaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bad", "signature <redacted> bad"),
        // Ordinary prose survives whole. A note nobody can read is its own kind of failure.
        (
            "connection reset by peer (os error 104), HTTP 403 after 3 tries",
            "connection reset by peer (os error 104), HTTP 403 after 3 tries",
        ),
    ];

    for (raw, expected) in cases {
        manifest.mark_failed(ItemKind::Memory, "m-01", raw).unwrap();
        let stored = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap().last_error.unwrap();
        assert_eq!(stored, expected, "stripping {raw:?}");
        assert!(!stored.contains("SECRETSIGNATURE"), "{stored}");
    }
}

/// The hole the shape pass alone leaves: `sig` is base62, so a signature lifted out of its url
/// carries none of `/ = % & @` and sits under the length cap. Only an exact match against the url
/// this row actually holds catches it.
#[test]
fn a_bare_signature_lifted_out_of_the_items_own_url_is_still_stripped() {
    let work = Workspace::new();
    let mut manifest = work.open();
    let url = DownloadUrl::new(SIGNED_URL);
    manifest.enroll(&[NewItem { kind: ItemKind::Memory, source_id: "m-01", url: Some(&url) }]).unwrap();

    // Exactly the token a shape rule has to let through: alphanumeric, no url punctuation at all,
    // well under the 64-character cap.
    assert!(!SIGNATURE.contains(['/', '=', '%', '&', '@']), "the fixture has to defeat the shape pass to test the other one");
    assert!(SIGNATURE.len() < 64, "and it has to be under the length cap too");

    manifest.mark_failed(ItemKind::Memory, "m-01", &format!("rejected signature {SIGNATURE} at the edge")).unwrap();

    let stored = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap().last_error.unwrap();
    assert_eq!(stored, "rejected signature <redacted> at the edge");
}

/// One punctuation character next to the secret is all it takes if the match runs the wrong way
/// round. Asking "does the url contain this token" fails the moment the token carries a comma, a
/// quote, a paren or a full stop, because the url contains none of those — and none of them is url
/// punctuation, so the shape pass waves them through too. Every leg here holds the signature's own
/// bytes intact.
#[test]
fn a_signature_wearing_ordinary_punctuation_is_still_stripped() {
    let work = Workspace::new();
    let mut manifest = work.open();
    let url = DownloadUrl::new(SIGNED_URL);
    manifest.enroll(&[NewItem { kind: ItemKind::Memory, source_id: "m-01", url: Some(&url) }]).unwrap();

    let spellings = [
        ("bare", format!("upload rejected {SIGNATURE} try again")),
        ("trailing comma", format!("upload rejected {SIGNATURE}, try again")),
        ("sentence end", format!("the signature was {SIGNATURE}.")),
        ("parenthesised", format!("failed ({SIGNATURE}) after 3 tries")),
        ("json quoted", format!("cdn said {{\"sig\": \"{SIGNATURE}\"}} expired")),
        ("interior of a longer token", format!("token=[{SIGNATURE}];rejected")),
    ];

    for (spelling, note) in spellings {
        manifest.mark_failed(ItemKind::Memory, "m-01", &note).unwrap();
        let stored = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap().last_error.unwrap();
        assert!(!stored.contains(SIGNATURE), "{spelling}: {stored}");
    }
}

/// The other direction, pinned one character either side of the floor rather than nowhere near it.
///
/// The identity pass matches on runs the url holds, so without a floor it would eat any word the
/// url happens to contain. The two segments below sit at 11 and 12 characters, so moving the floor
/// down reds the survivor and moving it up reds the redaction — a fixture six characters clear of
/// the constant would pass at every value in between and pin nothing.
#[test]
fn the_identity_pass_floor_sits_between_an_eleven_and_a_twelve_character_run() {
    /// `elevenchars` is 11 characters, `twelvechars0` is 12: one either side of the floor.
    const BOUNDARY_URL: &str = "https://cdn.example.net/elevenchars/twelvechars0?sig=SECRETSIGNATURE";
    const SHORT_RUN: &str = "elevenchars";
    const LONG_RUN: &str = "twelvechars0";

    assert_eq!(SHORT_RUN.len(), 11, "the fixture stops pinning the floor if this drifts");
    assert_eq!(LONG_RUN.len(), 12, "and this one is the first length the floor should catch");

    let work = Workspace::new();
    let mut manifest = work.open();
    let url = DownloadUrl::new(BOUNDARY_URL);
    manifest.enroll(&[NewItem { kind: ItemKind::Memory, source_id: "m-01", url: Some(&url) }]).unwrap();

    manifest.mark_failed(ItemKind::Memory, "m-01", &format!("saw {SHORT_RUN} and {LONG_RUN} here")).unwrap();

    let stored = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap().last_error.unwrap();
    assert_eq!(stored, format!("saw {SHORT_RUN} and <redacted> here"));
}

#[test]
fn a_missing_source_reason_is_stripped_the_same_way_as_a_failure_note() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();

    manifest.mark_source_missing(ItemKind::Memory, "m-01", &format!("nothing on disk and the link was {SIGNED_URL} only")).unwrap();

    let stored = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap().last_error.unwrap();
    assert_eq!(stored, "nothing on disk and the link was <redacted> only");
}

#[cfg(unix)]
#[test]
fn the_manifest_and_its_sidecars_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();

    let db = manifest.path().to_path_buf();
    let sidecar = |suffix: &str| PathBuf::from(format!("{}{suffix}", db.display()));
    for path in [db.clone(), sidecar("-wal"), sidecar("-shm")] {
        assert!(path.exists(), "{} is missing, so its mode was never checked", path.display());
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{} is mode {mode:o}", path.display());
    }

    // The directory is half of the same control and was previously the untested half.
    let dir_mode = fs::metadata(&work.state).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "{} is mode {dir_mode:o}", work.state.display());
}

/// The file half of this control tightens an existing loose file, so the directory half has to as
/// well or the same control disagrees with itself depending on which half you look at.
#[cfg(unix)]
#[test]
fn an_existing_loose_manifest_dir_is_tightened_on_open() {
    use std::os::unix::fs::PermissionsExt;

    let work = Workspace::new();
    fs::create_dir_all(&work.state).unwrap();
    fs::set_permissions(&work.state, fs::Permissions::from_mode(0o755)).unwrap();

    let _manifest = work.open();

    let mode = fs::metadata(&work.state).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "{} was left at {mode:o}", work.state.display());
}

// ---- opening the wrong thing ----

#[test]
fn a_manifest_from_a_newer_schema_is_refused_with_the_fix_in_the_message() {
    let work = Workspace::new();
    let db = {
        let manifest = work.open();
        manifest.path().to_path_buf()
    };

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);

    let error = Manifest::open_in(&work.state, &ExportId::new(ID).unwrap()).unwrap_err();

    assert!(matches!(error, ManifestError::FutureSchema { found: 99, .. }), "{error:?}");
    let message = error.to_string();
    assert!(message.contains("schema version 99"), "{message}");
    assert!(message.contains("upgrade exportsnap"), "{message}");
    assert!(message.contains(&db.display().to_string()), "{message}");
}

#[test]
fn a_manifest_belonging_to_another_export_is_refused() {
    let work = Workspace::new();
    let other = "1799123456780";
    {
        let mut manifest = work.open_as(other);
        manifest.enroll(&enrollment(&["m-01"])).unwrap();
    }
    fs::rename(work.state.join(format!("{other}.sqlite")), work.state.join(format!("{ID}.sqlite"))).unwrap();

    let error = Manifest::open_in(&work.state, &ExportId::new(ID).unwrap()).unwrap_err();

    assert!(matches!(&error, ManifestError::WrongExport { found, wanted, .. } if found == other && wanted == ID), "{error:?}");
}

/// The only export on this box is a directory called `mydata~1784667002819`, and that name carries
/// a `~` that [`ExportId::new`] refuses. So the id has to reach the manifest already stripped, and
/// this drives that from a directory on disk through the real discovery path rather than from a
/// hand-written id — a synthetic id cannot show which component does the stripping.
#[test]
fn the_id_a_real_export_directory_yields_opens_a_manifest() {
    let work = Workspace::new();
    let source = work.source_with_export_dir(&format!("mydata~{ID}"));

    let groups = discover_parts(&source).unwrap();
    let [group] = groups.as_slice() else { panic!("expected one export group, got {}", groups.len()) };

    // The brief's identity is the `mydata~<epoch>` dir that holds `json/`; this is that dir.
    assert_eq!(group.extracted.len(), 1);
    assert!(group.extracted[0].json_dir.is_some(), "the discovered part is the one holding json/");

    // `PartName::parse` strips the `mydata~` prefix, so the group id is already `~`-free. The raw
    // directory name is not an export id and must not be mistaken for one.
    assert_eq!(group.id, ID);
    assert!(ExportId::new(&format!("mydata~{ID}")).is_none(), "the whole directory name is not an id");

    let export = ExportId::new(&group.id).expect("a discovered group id has to open a manifest");
    let mut manifest = Manifest::open_in(&work.state, &export).unwrap();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();
    assert_eq!(owed(&manifest, 3), ["m-01"]);
    assert_eq!(manifest.path().file_name().unwrap(), format!("{ID}.sqlite").as_str());
}

/// A database with a schema and no export pin cannot come out of `install`, which writes both in
/// one transaction. A hand-edited one can, and it must not be reported as a rename: that names a
/// cause that never happened, and the message reads as a broken sentence when the id is empty.
#[test]
fn a_manifest_carrying_no_export_pin_says_so_rather_than_blaming_a_rename() {
    let work = Workspace::new();
    let db = {
        let manifest = work.open();
        manifest.path().to_path_buf()
    };

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("DELETE FROM meta WHERE key = 'export_id'", []).unwrap();
    drop(conn);

    let error = Manifest::open_in(&work.state, &ExportId::new(ID).unwrap()).unwrap_err();

    assert!(matches!(error, ManifestError::MissingExportPin { .. }), "{error:?}");
    let message = error.to_string();
    assert!(message.contains("carries no export id"), "{message}");
    assert!(message.contains("edited outside exportsnap"), "{message}");
    assert!(!message.contains("renamed or copied"), "nothing was renamed: {message}");
}

/// A status word this build does not know has two causes and the message must not pick one. A NEWER
/// exportsnap writing a status this one has not learned is exactly how `retired` looked to the build
/// before it, and that case is repaired by upgrading — while deleting, the remedy for a hand-edit,
/// throws away resumable state the newer build would have read fine.
#[test]
fn an_unreadable_status_word_offers_the_upgrade_before_the_delete() {
    let work = Workspace::new();
    {
        let mut manifest = work.open();
        manifest.enroll(&enrollment(&["m-01"])).unwrap();
    }

    // Only reachable by another build or a hand-edit, which is the whole point: this build cannot
    // write a word it does not know.
    let db = work.state.join(format!("{ID}.sqlite"));
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE items SET status = 'a_status_from_a_later_build'", []).unwrap();
    drop(conn);

    let manifest = work.open();
    let error = manifest.item(ItemKind::Memory, "m-01").unwrap_err();

    assert!(matches!(&error, ManifestError::CorruptRow { column, .. } if column.to_string() == "status"), "{error:?}");
    let message = error.to_string();
    assert!(message.contains("a_status_from_a_later_build"), "the message names the value it could not read: {message}");
    assert!(message.contains("newer exportsnap may have written it"), "{message}");
    assert!(message.contains("upgrade first"), "the remedy that costs nothing comes first: {message}");
    assert!(
        !message.contains("the file was edited outside exportsnap, so delete"),
        "the old message asserted a cause it cannot know and prescribed the destructive fix for it: {message}"
    );
}

/// The pin lands in the same transaction as the schema, so a fresh manifest can never present a
/// stamped `user_version` with no pin behind it.
#[test]
fn installing_a_manifest_leaves_the_schema_and_the_export_pin_together() {
    let work = Workspace::new();
    let db = {
        let manifest = work.open();
        manifest.path().to_path_buf()
    };

    let conn = rusqlite::Connection::open(&db).unwrap();
    let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    let pin: Option<String> = conn.query_row("SELECT value FROM meta WHERE key = 'export_id'", [], |row| row.get(0)).optional().unwrap();

    assert_eq!(version, 1, "a stamped version means the whole install committed");
    assert_eq!(pin.as_deref(), Some(ID), "and the pin is part of what committed");
}

#[test]
fn an_export_id_that_could_escape_the_manifest_dir_is_refused() {
    for raw in ["", "..", "../elsewhere", "a/b", "a\\b", "with space", "mydata~1784667002819", "."] {
        assert!(ExportId::new(raw).is_none(), "{raw:?} was accepted as an export id");
    }
    assert_eq!(ExportId::new(ID).unwrap().as_str(), ID);
    assert_eq!(ExportId::new("a-b_C9").unwrap().as_str(), "a-b_C9");
}

/// Reads the real environment and writes nothing to it. Redirecting `HOME` would need
/// `std::env::set_var`, which edition 2024 made `unsafe` and the crate forbids — but the parts
/// worth pinning do not need a redirect, because they hold whatever `HOME` is: the application
/// name (which decides where every user's state lives, so renaming it orphans their manifests),
/// the subdirectory, and the design's invariant that a file holding signed urls never lands in the
/// repo or the output tree.
#[test]
fn the_manifest_dir_is_a_per_user_data_dir_outside_the_repo() {
    let dir = manifest_dir().expect("a box running these tests has a home directory");

    assert!(dir.ends_with("manifests"), "{}", dir.display());
    let app = dir.parent().unwrap().file_name().unwrap().to_string_lossy().to_ascii_lowercase();
    assert!(app.contains("exportsnap"), "the data dir is named for the app, got {app:?}");
    assert!(dir.is_absolute(), "{}", dir.display());
    assert!(!dir.starts_with(env!("CARGO_MANIFEST_DIR")), "the manifest holds signed urls: {}", dir.display());
}

// ---- excluding a row this build writes nothing for ----

/// Writing `Excluded` over an already-`Excluded` row must touch nothing, or `updated_at` degrades
/// from "the last status transition" to "the last run" — and an excluded row is re-derived by every
/// run from the same rule, so every run would rewrite every one of them.
///
/// The sentinel is load-bearing, not decoration: `unixepoch()` has one-second resolution, so a second
/// call landing in the same second as the first rewrites the row with the value it already had and no
/// assertion could tell a rewrite from a skip.
///
/// **The still-`Pending` row is the positive control, and it shares the pair of calls.** Without it a
/// `exclude` that had stopped writing anything at all would pass, because a dead call leaves a
/// backdated row alone just as well as a correct one does.
#[test]
fn excluding_an_already_excluded_row_leaves_it_untouched() {
    /// A timestamp far enough in the past that `unixepoch()` cannot produce it during this test.
    const SENTINEL: i64 = 1_000_000_000;

    let work = Workspace::new();
    {
        let mut manifest = work.open();
        manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
        manifest.exclude(ItemKind::Memory, &["m-01".to_owned()]).unwrap();
        assert_eq!(status_of(&manifest, "m-01"), ItemStatus::Excluded, "the row the next call must leave alone");
        assert_eq!(status_of(&manifest, "m-02"), ItemStatus::Pending, "and the one it must still be able to exclude");
    }

    // Only reachable by editing the database, which is the point: it backdates both rows so a
    // rewrite is distinguishable from a skip.
    let conn = rusqlite::Connection::open(work.state.join(format!("{ID}.sqlite"))).unwrap();
    conn.execute("UPDATE items SET updated_at = ?1", [SENTINEL]).unwrap();
    drop(conn);

    let mut manifest = work.open();
    // One call carrying both, which is also the shape `local_fix::run` uses: the already-excluded
    // row and the newly-excluded one go through a single transaction.
    manifest.exclude(ItemKind::Memory, &["m-01".to_owned(), "m-02".to_owned()]).unwrap();

    let untouched = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(untouched.updated_at, SENTINEL, "a second call rewrote a row whose status never transitioned");
    assert_eq!(untouched.status, ItemStatus::Excluded, "and it is still excluded, so the timestamp did not survive by the row moving");
    let control = manifest.item(ItemKind::Memory, "m-02").unwrap().unwrap();
    assert_eq!(control.status, ItemStatus::Excluded, "positive control: the same call did run, and did exclude a new row");
    assert_ne!(control.updated_at, SENTINEL, "and it stamped that transition");
}

#[test]
fn an_excluded_row_is_never_offered_as_work_and_comes_back_through_reset() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
    manifest.exclude(ItemKind::Memory, &["m-01".to_owned()]).unwrap();

    assert_eq!(owed(&manifest, 3), ["m-02"], "an excluded row is not work, whatever the retry cap");
    // Counted apart from every other status, which is the whole reason the status exists.
    let report = manifest.resume(ItemKind::Memory).unwrap();
    assert_eq!(report.excluded, 1);
    assert_eq!((report.pending, report.failed, report.source_missing, report.retired, report.verified), (1, 0, 0, 0, 0));
    assert!(report.demoted.is_empty(), "resume never demotes an excluded row: {:?}", report.demoted);

    // The way back, for the build whose rule about what to write has changed.
    manifest.reset(ItemKind::Memory, "m-01").unwrap();
    assert_eq!(owed(&manifest, 3), ["m-01", "m-02"]);
}

#[test]
fn excluding_a_row_no_run_enrolled_is_refused_rather_than_silently_doing_nothing() {
    // The `status <> excluded` clause means a zero row count no longer implies the row is absent, so
    // this is the case that would go quiet if the read that discriminates them were dropped.
    let work = Workspace::new();
    let mut manifest = work.open();
    match manifest.exclude(ItemKind::Memory, &["never-enrolled".to_owned()]) {
        Err(ManifestError::UnknownItem { kind, source_id }) => {
            assert_eq!((kind, source_id.as_str()), (ItemKind::Memory, "never-enrolled"));
        }
        other => panic!("expected UnknownItem, got {other:?}"),
    }
}

/// The batch is one transaction, and an unknown id in it rolls the whole thing back rather than
/// leaving half the set excluded. That is the observable the transaction buys — the fsync it saves
/// is not something a test can see from outside — and it is also the behaviour a caller has to be
/// able to reason about: a partially-applied exclusion would leave the plan and the manifest
/// disagreeing about which rows this build writes nothing for.
#[test]
fn one_unknown_id_rolls_the_whole_exclusion_back() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();

    match manifest.exclude(ItemKind::Memory, &["m-01".to_owned(), "never-enrolled".to_owned(), "m-02".to_owned()]) {
        Err(ManifestError::UnknownItem { source_id, .. }) => assert_eq!(source_id, "never-enrolled"),
        other => panic!("expected UnknownItem, got {other:?}"),
    }
    // `m-01` sorted before the bad id and was written inside the transaction, so it is the one that
    // proves the rollback rather than the one that was never reached.
    assert_eq!(status_of(&manifest, "m-01"), ItemStatus::Pending, "the write before the failure was rolled back");
    assert_eq!(status_of(&manifest, "m-02"), ItemStatus::Pending);
}

/// Excluding is a decision about OUTPUT; the retirement sweep is a fact about the SOURCE. So an
/// excluded row the enumeration can no longer name is retired like any other, because "gone from the
/// export" is a thing only `Retired` records and leaving it excluded would have the row claiming an
/// enrolled source that is not there.
///
/// It costs no churn either, which is what separates this from the `Retired` exemption: the row this
/// writes is `Retired`, and that one IS exempt, so the rewrite happens once and never again.
#[test]
fn a_vanished_excluded_row_is_retired() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();
    manifest.exclude(ItemKind::Memory, &["m-01".to_owned()]).unwrap();

    manifest.retire_absent(ItemKind::Memory, &BTreeSet::new(), &[]).unwrap();
    assert_eq!(status_of(&manifest, "m-01"), ItemStatus::Retired, "the source left the export, so the row says so");

    // And the second sweep is a no-op, because the status it wrote is the exempt one.
    let conn = rusqlite::Connection::open(work.state.join(format!("{ID}.sqlite"))).unwrap();
    conn.execute("UPDATE items SET updated_at = ?1", [1_000_000_000_i64]).unwrap();
    drop(conn);
    let mut manifest = work.open();
    manifest.retire_absent(ItemKind::Memory, &BTreeSet::new(), &[]).unwrap();
    assert_eq!(manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap().updated_at, 1_000_000_000);
}

// ---- what a transition out of `done` does to the output and its record ----
//
// One test per status a finished row can be driven to, each starting from a row that genuinely
// finished: real bytes on disk, checked in through `mark_done`, all three output columns populated.
// The decision (user's call, 2026-08-08) splits the statuses two ways — the parked ones keep the
// record, the work ones drop it — and every test here asserts the file itself survived either way,
// because nothing in this crate deletes an output and a future change that did would be invisible
// to a row-only assertion.

/// The row still names the output an earlier run checked in, and that file is still the bytes it was
/// checked in as. Both halves: "the record survived" and "the file survived" are different claims
/// and a transition could lose either one.
///
/// `status` is the positive control every caller passes. Without it a transition that had stopped
/// writing anything at all would pass this, because a row left at `Done` keeps its record too.
fn assert_output_kept(item: &Item, finished: &Finished, status: ItemStatus) {
    assert_eq!(item.status, status, "the transition under test has to have happened at all");
    assert_eq!(item.output_path.as_deref(), Some(finished.output.as_path()), "a parked row keeps the path it wrote");
    assert_eq!(item.checksum, Some(finished.checksum), "and the digest describing it");
    assert_eq!(item.bytes, Some(finished.bytes), "and the length");
    assert_eq!(fs::read_to_string(&finished.output).unwrap(), finished.body, "and nothing deleted or rewrote the file");
}

/// The row names no output any more, and the file an earlier run wrote is still on disk untouched:
/// what a work status drops is the only pointer to it, never the data.
fn assert_output_unrecorded(item: &Item, finished: &Finished, status: ItemStatus) {
    assert_eq!(item.status, status, "the transition under test has to have happened at all");
    assert_eq!(item.output_path, None, "a work status carries no path");
    assert_eq!(item.checksum, None, "and no digest describing bytes the next attempt is about to overwrite");
    assert_eq!(item.bytes, None);
    assert_eq!(fs::read_to_string(&finished.output).unwrap(), finished.body, "and dropping the record did not delete the file");
}

/// The chat-media case, measured on a real manifest before this was decided: the file a message
/// names disappears between two runs, so the row run 1 finished is driven straight to
/// `SourceMissing` under the same id. Run 1's output is still on disk, so the row goes on naming it.
#[test]
fn a_finished_row_parked_as_a_gap_keeps_its_output_and_the_record_of_it() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();
    let finished = work.finish(&manifest, "m-01", "bytes a first run wrote and hashed");

    manifest.mark_source_missing(ItemKind::Memory, "m-01", "the file the message names is gone").unwrap();

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_output_kept(&item, &finished, ItemStatus::SourceMissing);
    assert_eq!(item.last_error.as_deref(), Some("the file the message names is gone"), "and it still says why it parked");
    assert!(owed(&manifest, 3).is_empty(), "a parked row carrying a record is still not work");
}

/// Decision 44d's case turned around: a build whose rule changed excludes a row an EARLIER build
/// finished. Excluding is a decision about what THIS build writes and never about the file the last
/// one already wrote, so the record survives and nothing is orphaned on disk.
#[test]
fn a_finished_row_this_build_stops_writing_keeps_its_output_and_the_record_of_it() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();
    let finished = work.finish(&manifest, "m-01", "bytes an earlier build wrote and hashed");

    manifest.exclude(ItemKind::Memory, &["m-01".to_owned()]).unwrap();

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_output_kept(&item, &finished, ItemStatus::Excluded);
    let report = manifest.resume(ItemKind::Memory).unwrap();
    assert_eq!((report.excluded, report.verified), (1, 0), "and it counts as excluded rather than as finished work");
}

/// `Retired` is only reachable from `Done` through a park, because `retire_absent` exempts finished
/// rows. So this drives the whole route and pins the record at BOTH hops: pinning only the end would
/// pass a `mark_source_missing` that cleared the columns, since the sweep would then have nothing
/// left to lose.
#[test]
fn a_finished_row_parked_then_retired_keeps_its_output_and_the_record_of_it() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();
    let finished = work.finish(&manifest, "m-01", "bytes finished before the export forgot the row");

    manifest.mark_source_missing(ItemKind::Memory, "m-01", "no media for it in the parts extracted so far").unwrap();
    let parked = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_output_kept(&parked, &finished, ItemStatus::SourceMissing);

    // The next run's enumeration cannot name the row at all, and a gap row is not exempt from the
    // sweep the way a finished one is.
    manifest.retire_absent(ItemKind::Memory, &BTreeSet::new(), &[]).unwrap();

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_output_kept(&item, &finished, ItemStatus::Retired);
    assert!(
        item.last_error.unwrap().contains("no longer holds a source"),
        "the sweep wrote this row rather than being the thing that skipped it"
    );
}

/// The other half of the rule, and the reason it is a split rather than one answer. `Failed` is a
/// WORK status — `pending` hands the row straight back — so the next attempt overwrites whatever is
/// at that path and a kept digest would be describing bytes on their way out. The file is untouched:
/// what is dropped is the pointer, not the data.
///
/// **Pinned at both hops, and it has to be**: every state a `drops the record` assertion names is
/// also the enrollment default, so a fixture that never really finished would satisfy the whole
/// second half. The `assert_output_kept` before the call is what makes the `None`s below mean
/// something, and it shares the fixture rather than living in a helper. `retry_count` and
/// `last_error` are the second control: the transition writes them, so they cannot read as a default.
#[test]
fn a_finished_row_driven_back_to_work_by_a_failure_drops_the_record_and_keeps_the_file() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();
    let finished = work.finish(&manifest, "m-01", "bytes a first run wrote before a later one broke");

    let before = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_output_kept(&before, &finished, ItemStatus::Done);

    manifest.mark_failed(ItemKind::Memory, "m-01", "connection reset").unwrap();

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_output_unrecorded(&item, &finished, ItemStatus::Failed);
    assert_eq!(
        (item.retry_count, item.last_error.as_deref()),
        (1, Some("connection reset")),
        "and the transition wrote its own fields, so the row is not merely sitting where it started"
    );
    assert_eq!(owed(&manifest, 3), ["m-01"], "it is work again, which is what makes keeping the digest unsafe");
}

/// `reset` is the only place a parked row's record ends, and it ends there for the same reason:
/// `Pending` is a work status, so the item is about to be re-fixed and its output overwritten.
///
/// **The fixture fails once before it finishes**, which is not decoration. `Pending` IS the
/// enrollment default and so is a null output record, so without a non-default `retry_count` to
/// watch go to zero — and without the `assert_output_kept` before the call — every assertion here
/// would hold on a row nothing had ever touched. `mark_done` leaves `retry_count` alone, so the row
/// really is finished while still carrying the earlier failure.
#[test]
fn a_finished_row_reset_to_pending_drops_the_record_and_keeps_the_file() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();
    manifest.mark_failed(ItemKind::Memory, "m-01", "connection reset").unwrap();
    let finished = work.finish(&manifest, "m-01", "bytes a build that changed its mind wrote");

    let before = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_output_kept(&before, &finished, ItemStatus::Done);
    assert_eq!(before.retry_count, 1, "finishing an item does not forget that it failed on the way");

    manifest.reset(ItemKind::Memory, "m-01").unwrap();

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_output_unrecorded(&item, &finished, ItemStatus::Pending);
    assert_eq!(item.retry_count, 0, "reset is 'as if no run had ever touched it', retry count included");
    assert_eq!(item.last_error, None);
    assert_eq!(owed(&manifest, 3), ["m-01"]);
}

/// The widened rule's one real hazard, and the assumption the split rests on: a parked row now
/// carries a checksum, so anything reading "has a checksum" as "is finished work" picks it up.
/// `resume` scopes its re-verify on the STATUS instead, so the parked row is neither re-hashed nor
/// demoted — pinned with its output DELETED, which is exactly the state that would demote it if the
/// scoping were on the columns.
///
/// **The control shares the call and is a demotion, not a survival**: `m-02` is `Done` with its bytes
/// swapped, so the sweep demonstrably opened and hashed a file in this very call. A sweep that had
/// stopped working altogether would leave `m-01` alone just as well as a correctly-scoped one does.
#[test]
fn a_parked_row_carrying_a_checksum_is_never_re_verified_as_finished_work() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01", "m-02"])).unwrap();
    let parked = work.finish(&manifest, "m-01", "bytes m-01 finished with");
    let control = work.finish(&manifest, "m-02", "bytes m-02 finished with");

    manifest.mark_source_missing(ItemKind::Memory, "m-01", "no media for it in the parts extracted so far").unwrap();
    fs::remove_file(&parked.output).unwrap();
    fs::write(&control.output, "bytes m-02 was rewritten with").unwrap();

    let report = manifest.resume(ItemKind::Memory).unwrap();

    assert_eq!(
        report.demoted,
        vec![Demotion { kind: ItemKind::Memory, source_id: "m-02".to_owned(), reason: DemotionReason::Changed }],
        "the parked row is absent because it is out of scope, not because the sweep did nothing"
    );
    assert_eq!((report.source_missing, report.verified, report.pending), (1, 0, 1));
    assert_eq!(owed(&manifest, 3), ["m-02"], "a vanished output does not put a parked row back on the work list");

    let item = manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap();
    assert_eq!(item.status, ItemStatus::SourceMissing);
    assert_eq!(item.output_path.as_deref(), Some(parked.output.as_path()), "nor take its record away");
    assert_eq!(item.checksum, Some(parked.checksum));
}

// ---- the on-disk vocabulary ----

#[test]
fn every_kind_and_status_keeps_the_word_it_is_stored_as() {
    // These words are in every user's database. Renaming one silently orphans their rows, so the
    // list is a contract rather than an implementation detail.
    assert_eq!(ItemKind::ALL.map(ItemKind::as_stored), ["memory", "chat_media", "history_export"]);
    assert_eq!(ItemStatus::ALL.map(ItemStatus::as_stored), ["pending", "done", "failed", "source_missing", "retired", "excluded"]);

    // Second witness for each; `ItemKind::as_stored`/`ItemStatus::as_stored` above are the first,
    // and `ItemStatus` also has `resume`'s per-status match (src/export/manifest.rs) as a second.
    // Survives any of those being weakened to a wildcard. Residual and rationale:
    // `MissingReason::ALL`, src/export/memories.rs. `ItemStatus` is where an omission bites
    // hardest: `from_stored` parses through `ALL`, so a missing variant fails on READ as
    // `CorruptRow`, blaming the user for a gap in this crate. Never collapse either match to
    // `_ => {}`.
    for kind in ItemKind::ALL {
        match kind {
            ItemKind::Memory | ItemKind::ChatMedia | ItemKind::HistoryExport => {}
        }
    }
    for status in ItemStatus::ALL {
        match status {
            ItemStatus::Pending
            | ItemStatus::Done
            | ItemStatus::Failed
            | ItemStatus::SourceMissing
            | ItemStatus::Retired
            | ItemStatus::Excluded => {}
        }
    }
}

#[test]
fn one_kinds_work_list_is_not_another_kinds() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest
        .enroll(&[
            NewItem { kind: ItemKind::Memory, source_id: "shared-id", url: None },
            NewItem { kind: ItemKind::ChatMedia, source_id: "shared-id", url: None },
        ])
        .unwrap();
    let output = work.write_output("shared-id", "chat media bytes");
    manifest.mark_done(ItemKind::ChatMedia, "shared-id", &output).unwrap();

    assert_eq!(owed(&manifest, 3), ["shared-id"], "the memory is untouched by the chat media finishing");
    assert_eq!(manifest.pending(ItemKind::ChatMedia, 3).unwrap().len(), 0);
    assert_eq!(manifest.resume(ItemKind::Memory).unwrap().verified, 0, "resume counts one kind at a time");
    assert_eq!(manifest.resume(ItemKind::ChatMedia).unwrap().verified, 1);
}

// ---- calls that are wrong ----

#[test]
fn recording_work_against_an_unenrolled_item_names_it_instead_of_passing() {
    let work = Workspace::new();
    let manifest = work.open();
    let output = work.write_output("never-enrolled", "bytes with no row");

    let error = manifest.mark_done(ItemKind::Memory, "never-enrolled", &output).unwrap_err();

    assert!(matches!(&error, ManifestError::UnknownItem { kind: ItemKind::Memory, source_id } if source_id == "never-enrolled"));
    assert!(error.to_string().contains("never-enrolled"), "{error}");
}

#[test]
fn a_relative_output_path_is_refused_rather_than_stored() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();

    let error = manifest.mark_done(ItemKind::Memory, "m-01", Path::new("out/m-01.jpg")).unwrap_err();

    assert!(matches!(error, ManifestError::OutputPath { problem: PathProblem::Relative, .. }), "{error:?}");
    assert_eq!(manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap().status, ItemStatus::Pending);
}

#[test]
fn an_unreadable_output_is_a_different_failure_from_a_broken_database() {
    let work = Workspace::new();
    let mut manifest = work.open();
    manifest.enroll(&enrollment(&["m-01"])).unwrap();

    let error = manifest.mark_done(ItemKind::Memory, "m-01", work.outputs.join("was-never-written.jpg")).unwrap_err();

    // A caller retries this one; it does not mean the manifest itself is broken.
    assert!(matches!(error, ManifestError::Output { .. }), "{error:?}");
    assert_eq!(
        manifest.item(ItemKind::Memory, "m-01").unwrap().unwrap().status,
        ItemStatus::Pending,
        "a failed check-in leaves the row where it was"
    );
}
