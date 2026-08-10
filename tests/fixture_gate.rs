//! Pins the shared fixture-tree gate in `tests/common`, which every test crate reading `fixtures/`
//! routes its "is the tree here" question through.
//!
//! Its own crate for the reason `tests/tool_gate.rs` is: these run once rather than once per crate
//! declaring `mod common;`. What they can cover is the DECISION. The requirement variable cannot be
//! set at all from inside a process that forbids `unsafe`, so running a built test binary under a
//! constructed environment is what covers that half — with the tree moved aside, once with the
//! variable and once without.

#![allow(clippy::unwrap_used, clippy::expect_used)]

/// The crate-level allow lives here rather than inside `common`, and only on the crates that need
/// it: this one reads the fixture half and never the tool half, so without it this crate warns on
/// every tool-side function. `video`, `local_fix` and `chat_fix` carry no such allow and so keep
/// warning on an uncalled tool-side function, which is the coverage a blanket allow inside the
/// module would have cost them.
#[allow(dead_code, reason = "this crate pins the fixture half and never gates on a tool")]
mod common;

use std::fs;

use common::fixtures::{self, Verdict};
use tempfile::TempDir;

#[test]
fn a_tree_that_is_here_is_read_whether_or_not_a_runner_demanded_it() {
    let present = TempDir::new().unwrap();

    assert_eq!(fixtures::decide(present.path(), false), Verdict::Read);
    // Demanding a tree that IS here changes nothing: the variable removes the skip, it does not add
    // a check. Same contract as `tool_gate`'s `decide(&[Ffprobe], |_| true, |_| true) == Run`.
    assert_eq!(fixtures::decide(present.path(), true), Verdict::Read);
}

#[test]
fn an_absent_tree_skips_a_dev_box_and_fails_the_runner_that_demanded_it() {
    let parent = TempDir::new().unwrap();
    let absent = parent.path().join("fixtures");

    // The whole point of the task: these two inputs differ only in the variable, and before it
    // existed both of them were the same silent PASS.
    assert_eq!(fixtures::decide(&absent, false), Verdict::Skip);
    assert_eq!(fixtures::decide(&absent, true), Verdict::Fail);
}

#[test]
fn a_path_that_exists_and_is_not_a_directory_counts_as_absent() {
    // `exists()` instead of `is_dir()` reads identically on every box that has either a real tree or
    // nothing at all, which is every box the suite has ever run on. It diverges only here, and it
    // diverges towards the dangerous side: a `fixtures` FILE would be handed to the callers as a
    // tree and die inside `read_dir` instead of at the gate.
    let parent = TempDir::new().unwrap();
    let not_a_tree = parent.path().join("fixtures");
    fs::write(&not_a_tree, b"not a tree").unwrap();

    assert_eq!(fixtures::decide(&not_a_tree, false), Verdict::Skip);
    assert_eq!(fixtures::decide(&not_a_tree, true), Verdict::Fail);
}

#[test]
fn a_failure_names_the_runner_variable_the_test_and_the_tree_that_was_looked_for() {
    let tree = std::path::Path::new("/nowhere/exportsnap/fixtures");
    let message = fixtures::failure("some_test", tree);

    assert!(message.contains("some_test"), "{message}");
    assert!(message.contains("EXPORTSNAP_REQUIRE_FIXTURES"), "{message}");
    // The path, not only the word "fixtures". A runner that set the variable in the wrong working
    // directory gets told which directory was actually looked at.
    assert!(message.contains("/nowhere/exportsnap/fixtures"), "{message}");
    // The tree is generated, not installed, so the fix is a script rather than a package name —
    // which is the one way this message cannot be a copy of the tool gate's.
    assert!(message.contains("tools/redact_export.py"), "{message}");
}

#[test]
fn the_variable_is_a_third_one_rather_than_a_widening_of_either_tool_variable() {
    // The literal below is the guard, because the literal is what a CI author copies. The two
    // `assert_ne!` state the intent it enforces and cannot red on their own: `tests/tool_gate.rs`
    // pins `Tool::Exiftool.variable()` and `Tool::Ffprobe.variable()` to their own literals, and
    // three literals pinned separately cannot collide without redding one of those pins first.
    // The intent: a runner exporting the tool variables must not silently acquire a fixture demand
    // it has no way to satisfy, since no package manager produces a redacted export.
    assert_eq!(fixtures::VARIABLE, "EXPORTSNAP_REQUIRE_FIXTURES");
    assert_ne!(fixtures::VARIABLE, common::Tool::Exiftool.variable());
    assert_ne!(fixtures::VARIABLE, common::Tool::Ffprobe.variable());
}
