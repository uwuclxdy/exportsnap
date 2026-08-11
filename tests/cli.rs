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

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

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
    assert!(
        !stderr.contains("failed to take over the terminal"),
        "the argument must be refused before the terminal is touched, got {stderr:?}"
    );
}
