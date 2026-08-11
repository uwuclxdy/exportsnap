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

use std::process::{Command, Output};

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
    assert!(output.stdout.is_empty(), "a refused argument must leave stdout clean, got {:?}", String::from_utf8_lossy(&output.stdout));

    let stderr = stderr(&output);
    assert!(stderr.contains("not valid utf-8"), "the error must name what is wrong with the argument, got {stderr:?}");
    assert!(stderr.contains("--source=/tmp/"), "the error must name the argument it refused, got {stderr:?}");
    // The refusal happens at the boundary, so nothing downstream of it ran: no terminal takeover,
    // no report. `--source` is refused rather than honoured (decision 56a).
    assert!(!stderr.contains(TOOK_OVER), "the argument must be refused before the terminal is touched, got {stderr:?}");
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
    assert!(output.stdout.is_empty(), "a rejected flag must leave stdout clean, got {:?}", stdout(&output));
    assert!(stderr(&output).contains("--help takes no value"), "the error must name the flag and the fix, got {:?}", stderr(&output));
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

#[test]
fn an_unknown_double_dash_argument_fails_on_stderr_naming_it() {
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
fn a_bare_or_single_dash_argument_is_still_left_alone() {
    // Decision 56c supersedes the leave-it-alone convention for the `--` shape only, so a run that
    // works today has to keep working: these reach the parsers and the report prints.
    let dir = tempfile::tempdir().unwrap();
    let output = run(&["--print-source", &format!("--source={}", dir.path().display()), "some/path", "-x"]);
    assert!(output.status.success(), "a bare argument must still be left alone, got {:?}: {}", output.status, stderr(&output));
    assert!(stdout(&output).starts_with("source="), "the report must still print, got {:?}", stdout(&output));
}
