//! Public-API tests for `exportsnap::export::manifest`: what a second run skips, what the resume
//! sweep takes back off the finished pile, and what never reaches the manifest's text columns.
//!
//! Every database and every "media" file here is built inside the test. Nothing reads a real
//! export, and the per-user data dir is never touched: every manifest is opened with `open_in`
//! against a tempdir.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use exportsnap::export::manifest::{
    Checksum, Demotion, DemotionReason, ExportId, ItemKind, ItemStatus, Manifest, ManifestError, NewItem, PathProblem, manifest_dir,
};
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
}

fn enrollment<'a>(source_ids: &'a [&'a str]) -> Vec<NewItem<'a>> {
    source_ids.iter().map(|source_id| NewItem { kind: ItemKind::Memory, source_id, url: None }).collect()
}

fn owed(manifest: &Manifest, max_attempts: u32) -> Vec<String> {
    manifest.pending(ItemKind::Memory, max_attempts).unwrap().into_iter().map(|item| item.source_id).collect()
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

// ---- the on-disk vocabulary ----

#[test]
fn every_kind_and_status_keeps_the_word_it_is_stored_as() {
    // These words are in every user's database. Renaming one silently orphans their rows, so the
    // list is a contract rather than an implementation detail.
    assert_eq!(ItemKind::ALL.map(ItemKind::as_stored), ["memory", "chat_media", "history_export"]);
    assert_eq!(ItemStatus::ALL.map(ItemStatus::as_stored), ["pending", "done", "failed", "source_missing"]);
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
