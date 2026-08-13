//! `--print-source` is the pin on the startup composition: that the dir the binary was launched
//! against is the dir every screen is built from.
//!
//! Every other test in this crate hand-composes an `App` out of `App::new` plus the screen seams, so
//! `App::start(tier, source, out)` — the whole production composition — had no test that could tell
//! it apart from `App::start(tier, PathBuf::new(), out)`. Reaching it needs a launched binary, and a
//! launched binary takes over the terminal, so decision 55 added a flag that resolves the source,
//! builds the app, prints what the app holds, and exits first. That makes the pin a plain
//! `Command::output()`, the same shape `tests/attribution.rs` uses for `--version`: no pty, no new
//! dependency.
//!
//! **One argument, six deliveries, and the report observes all of them.** `App::start` hands the
//! source to the overview's read of the dir, to the `statvfs` probe, to each of the three run
//! screens, and to the account screen. A first cut of this file watched only the overview, and the
//! other four then survived `PathBuf::new()` with all 694 tests green. The keys are per-screen for
//! that reason.
//!
//! **What the flag prints comes off the composed `App`, never off `main`'s locals.** `main` can
//! print the path byte-identically, so the path alone would pin nothing; the part counts, the space
//! figures and each screen's own copy are what a build with a dropped argument cannot produce.
//! Since task 85 (2026-08-13) the composition also includes the config file, so the flag reads it
//! like the TUI does — the report and the run describe the same root or neither can start
//! (`a_config_out_dir_reaches_every_out_key` pins it).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use exportsnap::export::env;
use tempfile::TempDir;

/// The leaf `default_out_root` appends when no `--out` is passed, spelled here rather than imported
/// so the expectation is not taken from the code under test.
const OUT_DIR: &str = "exportsnap-out";

/// Launches the built binary with `--print-source` and hands back its whole run.
///
/// Every spawn is sandboxed against the operator's real config: the flag reads the config file now
/// (the report is the composed app), so a real `out_dir` on the box would red every default-root
/// assertion below. The scratch home is empty, so the sandbox itself adds no key. The config dir
/// is spelled per platform — linux reads `XDG_CONFIG_HOME`, mac reads `HOME` — and both point at
/// the scratch dir; on windows neither env var redirects `directories`, which reads the shell
/// folders, so that leg stays unsandboxed (the CI box has no config).
fn print_source(args: &[String]) -> Output {
    let home = tempfile::tempdir().unwrap();
    print_source_at(home.path(), args)
}

/// The spawn itself, with the sandbox home handed in: the caller writes the config file into that
/// dir before the child reads it, so the child and the writer must share one home.
fn print_source_at(home: &Path, args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_exportsnap"))
        .args(args)
        .arg("--print-source")
        .env("XDG_CONFIG_HOME", home)
        .env("HOME", home)
        .output()
        .unwrap()
}

/// The report for a source dir, with the run required to have succeeded.
fn report(source: &Path) -> String {
    let output = print_source(&[format!("--source={}", source.display())]);
    assert!(output.status.success(), "--print-source must exit 0, got {:?}: {}", output.status, String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

/// The report split into `(key, value)` pairs, in the order printed.
///
/// A line that does not hold a `=` is a hard failure rather than a skip: the whole point of the
/// quoting is that every line of this stream is exactly one field, so an unsplittable line means the
/// framing broke.
fn fields(report: &str) -> Vec<(&str, &str)> {
    report
        .lines()
        .map(|line| line.split_once('=').unwrap_or_else(|| panic!("every report line must be one key=value, got {line:?} in {report:?}")))
        .collect()
}

fn keys<'a>(fields: &[(&'a str, &str)]) -> Vec<&'a str> {
    fields.iter().map(|(key, _)| *key).collect()
}

/// The one value stored under `key`, with the key required to appear exactly once — a duplicate is
/// how a forged key would show up.
fn value<'a>(fields: &[(&str, &'a str)], key: &str) -> &'a str {
    let mut found = fields.iter().filter(|(name, _)| *name == key).map(|(_, value)| *value);
    let first = found.next().unwrap_or_else(|| panic!("the report has no {key:?} key: {fields:?}"));
    assert!(found.next().is_none(), "{key:?} appears more than once: {fields:?}");
    first
}

/// How an ascii path must come back: wrapped in quotes, with backslashes and quotes doubled.
/// Worked out the long way so the expectation is not `Debug`'s own output.
///
/// The two replacements are what makes this runnable on Windows, where every `TempDir` path is full
/// of separators that need escaping — an earlier version asserted a path held no `\` and would have
/// aborted the whole file there. Control characters are still refused, because a caller passing one
/// wants [`a_hostile_source_path_cannot_forge_a_key`]'s spelled-out expectation instead.
fn quoted(path: &Path) -> String {
    let text = path.display().to_string();
    assert!(text.is_ascii() && !text.contains(|c: char| c.is_control()), "this spelling only holds for a printable ascii path: {text:?}");
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One delivery in an otherwise empty dir: part 1 unpacked, parts 4-6 still zipped, so parts 2 and 3
/// are missing. The three figures differ, so none of them can be a hardcoded constant that happens
/// to match. `discover_parts` reads names only, so the zips can be empty files.
fn export_tree() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("mydata~t1")).unwrap();
    for part in 4..=6 {
        fs::write(dir.path().join(format!("mydata~t1-{part}.zip")), b"").unwrap();
    }
    dir
}

#[test]
fn the_launched_binary_reports_the_export_in_the_source_dir_it_was_given() {
    let dir = export_tree();
    let source = dir.path();
    let report = report(source);
    let fields = fields(&report);

    // The exact key set, so a key silently dropped is a failure rather than an assertion nobody
    // makes any more.
    assert_eq!(
        keys(&fields),
        [
            "source",
            "parts",
            "zips",
            "unpacked",
            "missing",
            "free",
            "total",
            "memories-source",
            "memories-out",
            "chat-source",
            "chat-out",
            "history-source",
            "history-out",
            "account-source"
        ]
    );
    assert_eq!(value(&fields, "source"), quoted(source));
    assert_eq!(value(&fields, "parts"), "one");
    assert_eq!(value(&fields, "zips"), "3");
    assert_eq!(value(&fields, "unpacked"), "1");
    assert_eq!(value(&fields, "missing"), "2");

    // The three run screens read the export and write files, so each carries its own copy of the
    // argument and each is observed. One can be blanked without the other.
    assert_eq!(value(&fields, "memories-source"), quoted(source));
    assert_eq!(value(&fields, "chat-source"), quoted(source));
    assert_eq!(value(&fields, "history-source"), quoted(source));
    assert_eq!(value(&fields, "memories-out"), quoted(&source.join(OUT_DIR)));
    assert_eq!(value(&fields, "chat-out"), quoted(&source.join(OUT_DIR)));
    assert_eq!(value(&fields, "history-out"), quoted(&source.join(OUT_DIR)));
    // The account screen is read-only, so it carries the source alone.
    assert_eq!(value(&fields, "account-source"), quoted(source));

    // The probe is measured on the source's own filesystem, so its figures are the fourth delivery
    // of the argument. A blanked probe path measures nothing and drops both keys, which the key-set
    // assertion above catches; these two pin that a present key holds a real measurement.
    //
    // `total` is checked against a second, independent measurement of the same filesystem rather
    // than against `free`, because every ordering between the two is satisfied by printing
    // `available_space` under both names. The filesystem's size does not move between the two calls;
    // its free bytes do, which is why only this one is exact. Bound worth stating: the mutation is
    // separated by `free != total`, so a runner whose source filesystem is exactly 100% free would
    // not kill it.
    let free: u64 = value(&fields, "free").parse().expect("free must be a byte count");
    let total: u64 = value(&fields, "total").parse().expect("total must be a byte count");
    assert_eq!(total, env::total_space(source).unwrap(), "total must be the filesystem's size, not its free bytes");
    assert!(free > 0 && free <= total, "the source filesystem must measure as non-empty, got free={free} total={total}");

    // `App::source_report` documents the text as `\n`-terminated, and `lines()` cannot see that.
    assert!(report.ends_with('\n'), "the report must end with a newline: {report:?}");
    assert!(!report.contains("\n\n"), "a dropped value would show up as a blank line: {report:?}");
}

#[test]
fn the_out_root_the_binary_was_given_reaches_both_media_screens() {
    let dir = export_tree();
    let out = dir.path().join("elsewhere");
    let output = print_source(&[format!("--source={}", dir.path().display()), format!("--out={}", out.display())]);
    assert!(output.status.success(), "--print-source must exit 0, got {:?}", output.status);
    let report = String::from_utf8(output.stdout).unwrap();
    let fields = fields(&report);

    assert_eq!(value(&fields, "memories-out"), quoted(&out));
    assert_eq!(value(&fields, "chat-out"), quoted(&out));
    assert_eq!(value(&fields, "history-out"), quoted(&out));
    // `--out` moves where the run writes and nothing else; the source keys must not follow it.
    assert_eq!(value(&fields, "memories-source"), quoted(dir.path()));
}

/// Unix only, because the sandbox's config dir is spelled per platform and the platform answer
/// decides which one the binary reads. Linux reads `$XDG_CONFIG_HOME/exportsnap` and mac reads
/// `$HOME/Library/Application Support/exportsnap`; writing both spellings keeps one test body on
/// both legs, with the unread one inert. Windows reads the shell folders that no env var can
/// redirect, so it has no sandbox to write into — the same split `tests/config.rs` documents.
#[cfg(unix)]
#[test]
fn a_config_out_dir_reaches_every_out_key() {
    let dir = export_tree();
    let home = tempfile::tempdir().unwrap();
    let out = dir.path().join("elsewhere");
    for config_dir in [home.path().join("exportsnap"), home.path().join("Library/Application Support/exportsnap")] {
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("config.toml"), format!("out_dir = {}\n", quoted(&out))).unwrap();
    }

    // The spawn must read THIS home — the one the config was written into — or it sees an empty
    // scratch dir and prints the default root with the file unread, which is the failure this test
    // exists to catch, not a test-harness artifact.
    let output = print_source_at(home.path(), &[format!("--source={}", dir.path().display())]);
    assert!(output.status.success(), "--print-source must exit 0, got {:?}", output.status);
    let report = String::from_utf8(output.stdout).unwrap();
    let fields = fields(&report);

    assert_eq!(value(&fields, "memories-out"), quoted(&out));
    assert_eq!(value(&fields, "chat-out"), quoted(&out));
    assert_eq!(value(&fields, "history-out"), quoted(&out));
    // The file moves the out root and nothing else; the source keys must not follow it.
    assert_eq!(value(&fields, "memories-source"), quoted(dir.path()));
}

/// Unix only, and the gate is the FIXTURE rather than the assertion: a newline, a tab and a quote
/// are all legal in a unix filename and none of them is a legal Win32 one, so this path cannot be
/// built to attack a Windows binary. The composition pins in this file carry no such restriction and
/// run on all three phase-5 CI legs.
#[cfg(unix)]
#[test]
fn a_hostile_source_path_cannot_forge_a_key() {
    let dir = tempfile::tempdir().unwrap();
    // Every character `Debug` has to do something with, plus an `=` it must leave alone: a newline
    // that would open a second line, a tab, a quote that would close the value early, a backslash
    // that would escape whatever follows it.
    let hostile = dir.path().join("x\nparts=one\tq\"w\\e=r");
    let report = report(&hostile);
    let fields = fields(&report);

    // `value` already refuses a duplicate key, so this is the forgery caught from the other side:
    // the injected `parts=one` must not exist at all, and the real verdict must be the only one.
    assert_eq!(value(&fields, "parts"), "missing");
    assert_eq!(
        keys(&fields),
        [
            "source",
            "parts",
            "memories-source",
            "memories-out",
            "chat-source",
            "chat-out",
            "history-source",
            "history-out",
            "account-source"
        ]
    );

    // Spelled out rather than derived, so a toolchain that moves any of these four escapes reds here
    // instead of shipping a changed output format. Each pair below is two characters in the value.
    let escaped = format!("\"{}/x\\nparts=one\\tq\\\"w\\\\e=r\"", dir.path().display());
    assert_eq!(value(&fields, "source"), escaped);
    assert_eq!(value(&fields, "memories-source"), escaped);
    assert_eq!(value(&fields, "account-source"), escaped);
    // The `=` is data inside the value, not a separator: the split takes the first one only.
    assert!(escaped.contains("e=r"), "an `=` inside a path must survive unescaped: {escaped:?}");
}

#[test]
fn the_hand_spelled_escaping_agrees_with_debug_on_a_windows_shaped_path() {
    // Every assertion in this file compares against `quoted`, so it has to be right on a path shape
    // this runner never produces. A Windows `TempDir` path is all separators; the point is both that
    // the helper survives one and that the long-hand spelling is what `Debug` actually emits.
    let windows_shaped = Path::new(r"C:\Users\a\AppData\Local\Temp\.tmpAbC123");
    assert_eq!(quoted(windows_shaped), format!("{windows_shaped:?}"));
    assert_eq!(quoted(windows_shaped), r#""C:\\Users\\a\\AppData\\Local\\Temp\\.tmpAbC123""#);
}

#[test]
fn a_source_that_is_not_a_dir_reports_unreadable_rather_than_missing() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a-file");
    fs::write(&source, b"not a dir").unwrap();
    let report = report(&source);
    let fields = fields(&report);

    // The overview keeps these two apart on purpose — a typo in `--source` is the likeliest failure
    // of the lot and answering it with "unreadable" misdiagnoses it as a permissions fault. This
    // report must keep them apart too: collapsing the pair is a live defect class on this branch.
    assert_eq!(value(&fields, "parts"), "unreadable");
    assert_eq!(value(&fields, "source"), quoted(&source));
}

#[test]
fn several_deliveries_in_one_dir_report_how_many_rather_than_picking_one() {
    let dir = tempfile::tempdir().unwrap();
    for id in ["t1", "t2", "t3"] {
        fs::write(dir.path().join(format!("mydata~{id}.zip")), b"").unwrap();
    }
    let report = report(dir.path());
    let fields = fields(&report);

    assert_eq!(value(&fields, "parts"), "several");
    // Three, not zero and not one: which delivery the dir is about would be a guess, so the count is
    // the whole answer and it has to be the real one.
    assert_eq!(value(&fields, "exports"), "3");
    assert_eq!(
        keys(&fields),
        [
            "source",
            "parts",
            "exports",
            "free",
            "total",
            "memories-source",
            "memories-out",
            "chat-source",
            "chat-out",
            "history-source",
            "history-out",
            "account-source"
        ]
    );
}

#[test]
fn a_source_dir_that_is_not_there_is_reported_rather_than_refused() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("nope");
    let report = report(&source);
    let fields = fields(&report);

    // `parse_source_arg` deliberately accepts a dir that is not there — the overview has words for
    // it, and refusing to start would mean the user cannot open the app to see what it thinks. The
    // flag inherits that: a typo is a report, exit 0, not an error.
    assert_eq!(value(&fields, "parts"), "missing");
    assert_eq!(value(&fields, "source"), quoted(&source));
    // Nothing was counted and nothing was measured, so neither the part numbers nor the space
    // figures appear. A `zips=0` here would be a confident wrong answer.
    assert_eq!(
        keys(&fields),
        [
            "source",
            "parts",
            "memories-source",
            "memories-out",
            "chat-source",
            "chat-out",
            "history-source",
            "history-out",
            "account-source"
        ]
    );
}

#[test]
fn an_empty_source_dir_reports_no_export_rather_than_a_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    let report = report(dir.path());
    let fields = fields(&report);

    assert_eq!(value(&fields, "parts"), "none");
    // The dir is there, so unlike the case above the filesystem does measure.
    assert_eq!(
        keys(&fields),
        [
            "source",
            "parts",
            "free",
            "total",
            "memories-source",
            "memories-out",
            "chat-source",
            "chat-out",
            "history-source",
            "history-out",
            "account-source"
        ]
    );
}

#[test]
fn version_wins_over_print_source() {
    // `--version` returns before anything else is parsed or composed, per the GNU convention that it
    // ignores the rest of the command line. Passing both prints the version text alone.
    let output = Command::new(env!("CARGO_BIN_EXE_exportsnap")).arg("--print-source").arg("--version").output().unwrap();
    assert!(output.status.success(), "--version must exit 0, got {:?}", output.status);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("exportsnap "), "--version stdout must lead with the binary name, got {stdout:?}");
    assert!(!stdout.contains("source="), "--version must not also print the source report, got {stdout:?}");
}

#[test]
fn print_source_with_a_value_fails_on_stderr_without_printing_a_report() {
    let output = Command::new(env!("CARGO_BIN_EXE_exportsnap")).arg("--print-source=/tmp/export").output().unwrap();
    assert!(!output.status.success(), "--print-source=<value> must fail, got {:?}", output.status);
    assert!(output.stdout.is_empty(), "a rejected flag must leave stdout clean, got {:?}", String::from_utf8_lossy(&output.stdout));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--print-source takes no value"), "the error must name the flag and the fix, got {stderr:?}");
}
