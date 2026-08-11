//! The argv boundary of the LAUNCHED binary: what `exportsnap` does with an argument before it
//! takes the terminal over, observed the only way it can be — by spawning the built binary, the
//! same `Command::output()` shape `tests/print_source.rs` and `tests/attribution.rs` use. No pty,
//! no new dependency.
//!
//! The inline tests in `src/main.rs` cover the parsers as functions. What only a spawned run can
//! answer is the pair a script actually reads: the exit code and which stream carried the words.
//! A parser that returns the right `Err` still ships the wrong failure when the process aborts
//! before `main` runs, which is precisely the defect this file was written for.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// Launches the built binary with `args` and hands back its whole run. `output()` gives the child a
/// captured stdout and no tty, which is the point: every flag pinned here has to work with no
/// terminal to take over.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_exportsnap")).args(args).output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// The message the binary exits 1 with when it reaches `ratatui::try_init` without a terminal. Every
/// short-circuiting flag below asserts its ABSENCE: that is what "before anything touches the
/// terminal" means from the outside, and it is the exact failure `--help` used to produce.
const TOOK_OVER: &str = "failed to take over the terminal";

/// Unix only, and the gate is the FIXTURE rather than the assertion, the same way
/// `tests/print_source.rs`'s hostile path is: `OsStr::from_bytes` is `std::os::unix::ffi` and a
/// lone `0xff` is a byte no unix filesystem forbids in a name, so a caller building `--source`
/// out of a directory walk can produce this argument without trying to. The Windows analogue is
/// an unpaired surrogate through `OsStringExt::from_wide` — a different fixture, not a different
/// assertion, and nothing here is skipped out of caution.
#[cfg(unix)]
#[test]
fn an_argument_that_is_not_utf8_is_named_on_stderr_rather_than_aborting() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let bad = OsStr::from_bytes(b"--source=/tmp/\xff");
    let output = Command::new(env!("CARGO_BIN_EXE_exportsnap")).arg(bad).output().unwrap();

    // 101 is the panic exit, which is what `std::env::args` produces here and what decision 56a
    // rejected: bad input must not be spelled the way a bug is. The run still fails, on stderr.
    assert!(!output.status.success(), "a non-utf-8 argument must fail the run, got {:?}", output.status);
    assert_ne!(output.status.code(), Some(101), "a non-utf-8 argument must not abort the process: {}", stderr(&output));

    let stderr = stderr(&output);
    assert!(stderr.contains("not valid utf-8"), "the error must name what is wrong with the argument, got {stderr:?}");
    assert!(stderr.contains("--source=/tmp/"), "the error must name the argument it refused, got {stderr:?}");
    assert!(stderr.contains("argument 1"), "the error must place the argument, since its spelling cannot, got {stderr:?}");
    assert!(stderr.contains("shown lossily"), "the error must say the spelling is not the bytes passed, got {stderr:?}");
    // The refusal happens at the boundary, so nothing downstream of it ran: no terminal takeover,
    // no report. `--source` is refused rather than honoured (decision 56a).
    assert!(!stderr.contains(TOOK_OVER), "the argument must be refused before the terminal is touched, got {stderr:?}");
    // Last, for `an_unknown_dash_led_argument_fails_on_stderr_naming_it`'s reason: a run that
    // reaches the ui writes escape sequences to stdout on its way out, so this assertion fires
    // under any mutation that lets the argument through and would abort the body before the
    // assertions above ran.
    assert!(output.stdout.is_empty(), "a refused argument must leave stdout clean, got {:?}", String::from_utf8_lossy(&output.stdout));
}

/// Every flag `--help` claims the binary takes, spelled out here rather than imported, so the
/// expectation is not taken from the code under test. `-h` is checked apart from `--help` because
/// the text names the pair on one line.
const DOCUMENTED_FLAGS: [&str; 5] = ["--source=", "--out=", "--theme=", "--print-source", "--version"];

#[test]
fn help_prints_usage_to_stdout_at_exit_zero_in_both_spellings() {
    for flag in ["--help", "-h"] {
        let output = run(&[flag]);
        assert!(output.status.success(), "{flag} must exit 0, got {:?}: {}", output.status, stderr(&output));

        let stdout = stdout(&output);
        for documented in DOCUMENTED_FLAGS {
            assert!(stdout.contains(documented), "{flag} must name '{documented}', got {stdout:?}");
        }
        assert!(stdout.contains("-h, --help"), "{flag} must name its own pair of spellings, got {stdout:?}");
        assert!(stdout.contains("Usage: exportsnap"), "{flag} must show how the binary is invoked, got {stdout:?}");

        // Help is the program's output here, not a message about the run, so it goes to stdout and
        // stderr stays empty — and it prints with no terminal taken over, which is what it did
        // wrong before this arm existed.
        assert!(output.stderr.is_empty(), "{flag} must leave stderr clean, got {:?}", stderr(&output));
    }
}

#[test]
fn help_with_a_value_fails_on_stderr_without_printing_usage() {
    let output = run(&["--help=flags"]);
    assert!(!output.status.success(), "--help=<value> must fail, got {:?}", output.status);
    assert!(stderr(&output).contains("--help takes no value"), "the error must name the flag and the fix, got {:?}", stderr(&output));
    // Last, for the reason spelled out at `an_unknown_dash_led_argument_fails_on_stderr_naming_it`.
    assert!(output.stdout.is_empty(), "a rejected flag must leave stdout clean, got {:?}", stdout(&output));
}

/// The one arm all three payload flags share, from the outside: a reader that walked away mid-pipe
/// (`exportsnap --help | head -1`) is a finished run, so the exit code stays 0 instead of becoming
/// the 101 the print macros give an `EPIPE`. Nothing pinned this while the block was being copied
/// from one flag to the next, which is why the copy was the risk rather than the length.
///
/// **The read end is closed before the child is spawned**, so the first write it makes has no reader
/// at all and fails whatever a buffer would have absorbed. A real `head -1` on the other end races
/// these payloads — all three fit in one pipe buffer, so the write would succeed and the run would
/// pass over the arm this exists for.
#[test]
fn a_payload_flag_exits_zero_when_its_reader_has_left() {
    let dir = tempfile::tempdir().unwrap();
    let source = format!("--source={}", dir.path().display());
    for args in [["--version"].as_slice(), ["--help"].as_slice(), ["--print-source", &source].as_slice()] {
        let (reader, writer) = std::io::pipe().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_exportsnap"));
        command.args(args).stdout(writer).stderr(Stdio::piped());
        drop(reader);
        let output = command.output().unwrap();

        assert!(output.status.success(), "{args:?} must exit 0 when its reader left, got {:?}: {}", output.status, stderr(&output));
        // The exit code a `println!` would produce here, so a rewrite away from `write_all` reds too.
        assert_ne!(output.status.code(), Some(101), "{args:?} must not panic on a closed pipe: {}", stderr(&output));
        assert!(output.stderr.is_empty(), "{args:?} must say nothing about a reader that chose to leave, got {:?}", stderr(&output));
    }
}

#[test]
fn version_wins_over_help_and_help_wins_over_everything_after_it() {
    // The precedence is fixed rather than first-in-argv, so both orderings print the version.
    for args in [["--version", "--help"], ["--help", "--version"]] {
        let output = run(&args);
        assert!(output.status.success(), "{args:?} must exit 0, got {:?}", output.status);
        let stdout = stdout(&output);
        assert!(stdout.starts_with("exportsnap "), "{args:?} must print the version text, got {stdout:?}");
        assert!(!stdout.contains("Usage: exportsnap"), "{args:?} must not also print the help text, got {stdout:?}");
    }

    // GNU: once help is seen the rest of the command line is ignored, so an argument that would
    // otherwise be refused does not turn help into a failure.
    let output = run(&["--help", "--bogus"]);
    assert!(output.status.success(), "--help must ignore what follows it, got {:?}: {}", output.status, stderr(&output));
    assert!(stdout(&output).contains("Usage: exportsnap"), "--help must still print usage, got {:?}", stdout(&output));
}

/// The honest edge of a fixed precedence, pinned so the reasoning at the call site cannot drift back
/// into "a user passing both loses nothing either way" — which is false, and was measured false.
/// `--version` is scanned first and BAILS during that pass, so a malformed value on it is reported
/// even when `--help` is also present. terminal-ux §6's "ignore the other arguments once either is
/// seen" is about well-formed arguments; a value on a flag that takes none is the user believing it
/// takes one, and reporting that beats swallowing it.
#[test]
fn a_malformed_version_value_is_reported_even_when_help_is_asked_for() {
    let output = run(&["--help", "--version=1"]);
    assert!(stderr(&output).contains("--version takes no value"), "the version error must survive --help, got {:?}", stderr(&output));
    assert!(!output.status.success(), "--version=<value> must fail even beside --help, got {:?}", output.status);
    assert!(!stdout(&output).contains("Usage: exportsnap"), "help must not print over a rejected flag, got {:?}", stdout(&output));
}

#[test]
fn an_unknown_dash_led_argument_fails_on_stderr_naming_it() {
    // A typo of a scripting flag: this used to match no `strip_prefix` and open the ui instead.
    let output = run(&["--print-sourc"]);
    assert!(!output.status.success(), "an unknown flag must fail the run, got {:?}", output.status);

    // Named before anything else is asserted: a scan that swallowed this argument leaves the run
    // failing anyway, on the terminal takeover, so an earlier assertion on the exit code or on
    // stdout would abort the body and bank the kill against a line that never ran.
    let stderr = stderr(&output);
    assert!(stderr.contains("--print-sourc"), "the error must name the argument it refused, got {stderr:?}");
    assert!(stderr.contains("--help"), "the error must name where the flags are listed, got {stderr:?}");
    assert!(!stderr.contains(TOOK_OVER), "an unknown flag must be refused before the terminal is touched, got {stderr:?}");
    // The ui writes escape sequences to stdout on its way to failing, so this is the same pin from
    // the other side: a refused argument leaves a piped stdout untouched.
    assert!(output.stdout.is_empty(), "a refused argument must leave stdout clean, got {:?}", stdout(&output));
}

#[test]
fn a_known_flag_still_fails_with_its_own_message() {
    // The scan runs ahead of the parsers, so a known spelling it swallowed would replace each of
    // these messages — and the fix they name — with "unknown flag".
    for (args, expected) in [
        (["--theme"].as_slice(), "--theme needs a value"),
        (["--source"].as_slice(), "--source needs a value"),
        (["--out"].as_slice(), "--out needs a value"),
        (["--source="].as_slice(), "--source= names no dir"),
        (["--theme=24bit"].as_slice(), "unknown theme"),
        (["--print-source=/tmp/export"].as_slice(), "--print-source takes no value"),
        (["--version=1"].as_slice(), "--version takes no value"),
    ] {
        let output = run(args);
        assert!(!output.status.success(), "{args:?} must fail, got {:?}", output.status);
        assert!(stderr(&output).contains(expected), "{args:?} must fail with its own message '{expected}', got {:?}", stderr(&output));
    }
}

#[test]
fn a_bare_argument_is_still_left_alone() {
    // Decision 57 supersedes the leave-it-alone convention for dash-led arguments only, so a run
    // that works today has to keep working: these reach the parsers and the report prints.
    let dir = tempfile::tempdir().unwrap();
    let output = run(&["--print-source", &format!("--source={}", dir.path().display()), "some/path", "elsewhere"]);
    assert!(output.status.success(), "a bare argument must still be left alone, got {:?}: {}", output.status, stderr(&output));
    assert!(stdout(&output).starts_with("source="), "the report must still print, got {:?}", stdout(&output));
}

/// Decision 57's own regression pin, and the fixture is what makes it one: `--print-source` is
/// passed WITH the bad argument, so a scan that lets it through does not fail on the terminal — it
/// answers, at exit 0, about the working dir the caller never named. That is the measured defect
/// (`-source=/mnt/hdd-1` reported `source="/tmp"` on the shipped binary), and it is why the stderr
/// assertion comes before the exit-code one: under the old scan the run SUCCEEDS, so an exit-code
/// assertion would fire on a case where the message is what went missing.
#[test]
fn a_single_dash_spelling_of_a_flag_is_refused_rather_than_silently_ignored() {
    for arg in ["-source=/mnt/hdd-1", "-out=/tmp/x", "-theme=nonsense", "-print-source", "-version", "-q", "-h=1", "-"] {
        let output = run(&["--print-source", arg]);

        let stderr = stderr(&output);
        assert!(stderr.contains(arg), "the error must name {arg}, got {stderr:?}");
        assert!(!stderr.contains(TOOK_OVER), "{arg} must be refused before the terminal is touched, got {stderr:?}");
        assert!(!output.status.success(), "{arg} must fail the run, got {:?} with stdout {:?}", output.status, stdout(&output));
        assert!(
            !stdout(&output).contains("source="),
            "{arg} must not produce a report about a dir nobody named, got {:?}",
            stdout(&output)
        );
    }
}

/// `src/main.rs` down to its inline test module: the parsers and the flag set, without the test
/// literals below them. `src/` is tracked, so this reaches the file in CI too — the same reason
/// `tests/export.rs` reads the redactor's tuple through `CARGO_MANIFEST_DIR`.
fn parser_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("main.rs");
    let source = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
    let (parsers, _tests) = source.split_once("#[cfg(test)]").expect("src/main.rs must still carry its inline test module");
    parsers.to_string()
}

/// Every `--` spelling the parsers actually read, recovered from the source rather than promised by
/// a comment.
///
/// Three ceilings, named rather than assumed away:
/// - it reads the LITERAL shapes every parser here uses today (`arg == "--x"`, `strip_prefix("--x=")`,
///   `starts_with("--x=")`). A parser matching against a const or a built string is invisible to it
///   and wants this scan extended rather than trusted.
/// - a doc comment holding one of those three needles verbatim would be read as code. None does.
/// - `-h` is out of scope by construction: only `--` literals are kept, because the single-dash
///   spelling is `reject_unknown_args`' hard-coded exception rather than a member of the set.
///
/// A bare `--` is dropped rather than read as a flag whose name is empty: `reject_unknown_args`
/// classifies a dash-led argument and is not a parser, so a `starts_with("--")` written there is not
/// a flag this set should hold. Measured while planting decision 57's own mutation, which restored
/// exactly that spelling and made this witness fire for a reason that was not the drift it guards.
fn flags_the_parsers_read() -> BTreeSet<String> {
    let source = parser_source();
    let mut flags = BTreeSet::new();
    for needle in ["arg == \"", "strip_prefix(\"", "starts_with(\""] {
        for (offset, _) in source.match_indices(needle) {
            let literal = source[offset + needle.len()..].split('"').next().expect("a literal opened here must close");
            let Some(name) = literal.strip_prefix("--").map(|name| name.trim_end_matches('=')) else {
                continue;
            };
            if !name.is_empty() {
                flags.insert(format!("--{name}"));
            }
        }
    }
    flags
}

/// The `KNOWN_FLAGS` array's own members, read the same way. The const is private to the binary
/// crate, so a test crate cannot import it; recovering both sides from one file is what makes the
/// comparison below a coupling rather than two hand-copied lists.
fn the_known_set() -> BTreeSet<String> {
    let source = parser_source();
    let (_, declaration) = source.split_once("const KNOWN_FLAGS").expect("src/main.rs must still declare KNOWN_FLAGS");
    let (body, _) = declaration.split_once("];").expect("the KNOWN_FLAGS declaration must end on one line of its own");
    body.split('"').filter(|piece| piece.starts_with("--")).map(str::to_string).collect()
}

/// The witness the prose used to stand in for, in the direction the set-iterating tests cannot see:
/// they all start FROM `KNOWN_FLAGS`, so a parser added with a spelling missing from it stays green
/// everywhere while `exportsnap --verbose=1` exits 1 "unknown flag" before that parser ever runs, and
/// the help text never mentions it. `~/repos/CLAUDE.md` forbids carrying that as a comment.
#[test]
fn every_flag_the_parsers_read_is_in_the_known_set() {
    let read = flags_the_parsers_read();

    // The harness works before the comparison means anything: a scan reading nothing, or reading a
    // shape that stopped being a flag, would otherwise agree with an equally empty other side.
    for documented in DOCUMENTED_FLAGS {
        let flag = documented.trim_end_matches('=');
        assert!(read.contains(flag), "the scan missed {flag}, which the help text documents: {read:?}");
    }
    assert!(read.contains("--help"), "the scan missed --help: {read:?}");

    assert_eq!(read, the_known_set(), "every flag a parser reads must be in KNOWN_FLAGS and the reverse");
}

/// `-h` is the one single-dash spelling the refusal above exempts, so the exemption gets its own
/// pin: a scan that stopped exempting it would turn the documented short help into an error, and
/// every assertion in `help_prints_usage_to_stdout_at_exit_zero_in_both_spellings` would still hold
/// for `--help`.
#[test]
fn the_short_help_survives_the_refusal_of_every_other_single_dash_argument() {
    let output = run(&["-h"]);
    assert!(output.status.success(), "-h must exit 0, got {:?}: {}", output.status, stderr(&output));
    assert!(stdout(&output).contains("Usage: exportsnap"), "-h must print usage, got {:?}", stdout(&output));
}
