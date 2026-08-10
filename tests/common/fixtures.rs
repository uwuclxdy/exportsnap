//! The one answer to "is the `fixtures/` tree here", shared by every `tests/*.rs` crate that reads
//! it.
//!
//! **The same demanded-versus-absent decision [`super`] makes about an external tool, made about a
//! data tree.** `fixtures/` is gitignored, so a CI runner never has one, and a call site that
//! early-`return`s on the absence is reported by nextest as PASSED: the run summary is identical
//! whether the fixture assertions executed or did nothing. Printing a notice does not fix that on
//! its own: libtest and nextest both capture a PASSING test's stdout, so the notice in [`root`] is
//! visible only under `--nocapture` or nextest's `--success-output immediate`, and no default run
//! passes either. It is there for whoever already suspects and goes looking; [`VARIABLE`] is what
//! turns the absence into a red for everyone else.
//!
//! **Shared rather than per-crate because the absence had three spellings.** At `e08189e`
//! `tests/export.rs` carried a `json_dir_or_skip!` macro over `fixtures_root`, one test in that same
//! crate hand-rolled the `is_dir` check instead of reaching the helper beside it, and
//! `tests/overview.rs` built the path from `CARGO_MANIFEST_DIR` a third time. A grep for the two
//! helper names reaches the first two and structurally cannot reach the third, which is how the
//! overview crate's one real-export render test stayed out of every census of this gap.
//!
//! This is deliberately NOT a [`super::Tool`] variant. A tool gate answers `bool` and its call site
//! needs nothing back; a fixture gate has to hand back the PATH it verified, or the call site
//! re-derives it and there are two places again saying whether the tree is there — the exact shape
//! `super`'s module doc exists to remove. `super::probe` would also have to answer a filesystem
//! question inside a function whose whole contract is a cached process spawn.

use std::path::{Path, PathBuf};

/// Set this on a runner and an absent `fixtures/` tree fails the run instead of skipping the checks
/// that needed it.
///
/// Named to the grammar of the two that already exist (`EXPORTSNAP_REQUIRE_EXIFTOOL`,
/// `EXPORTSNAP_REQUIRE_FFMPEG`), and a third variable rather than a widening of either: those two
/// mean "this runner installed a program", and no `apt install` produces a redacted export.
pub const VARIABLE: &str = "EXPORTSNAP_REQUIRE_FIXTURES";

/// What a call site does about the fixture tree it asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The tree is here and the call site reads it.
    Read,
    /// It is not, and no runner demanded it.
    Skip,
    /// It is not, and [`VARIABLE`] demanded it.
    Fail,
}

/// The `fixtures/` tree, or `None` after recording why the caller's checks did not run — unless
/// [`VARIABLE`] is set, which is a runner stating it expects the tree, and then this fails the test
/// naming it.
///
/// Call it as the first statement of a test that reads the tree, passing that test's own name so a
/// failure says which assertions were about to be skipped.
///
/// `#[must_use]` because dropping the answer is what silently disarms the gate: a bare
/// `fixtures::root("t");` reads like a gate, opens nothing, and dies later at whatever `unwrap` was
/// about to walk the absent tree. The attribute makes that an `unused_must_use`, which the gate's
/// `-D warnings` rejects.
#[must_use]
pub fn root(test: &str) -> Option<PathBuf> {
    let tree = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    match decide(&tree, demanded()) {
        Verdict::Read => Some(tree),
        Verdict::Skip => {
            println!("SKIPPED {test}: fixtures/ is absent (gitignored, so CI never has it), so its assertions did not run");
            None
        }
        Verdict::Fail => panic!("{}", failure(test, &tree)),
    }
}

/// [`root`]'s decision, against an arbitrary tree rather than against this checkout.
///
/// The seam exists because the demand half cannot be set from inside the process at all —
/// `std::env::set_var` is `unsafe` from edition 2024 and this crate forbids `unsafe_code`, tests
/// included — so a test can only reach it by being handed the answer. `tests/fixture_gate.rs`
/// drives this; the [`VARIABLE`] read itself is covered only by running a built test binary under a
/// constructed environment.
///
/// The presence half takes a `&Path` rather than the `bool` it reduces to, so the two halves cannot
/// be passed in the wrong order and so the pin exercises the real `is_dir` against a real
/// filesystem instead of asserting a literal back to itself.
#[must_use]
pub fn decide(tree: &Path, demanded: bool) -> Verdict {
    if tree.is_dir() {
        return Verdict::Read;
    }
    if demanded { Verdict::Fail } else { Verdict::Skip }
}

/// What a run fails with when a demanded tree is absent: the runner's own variable, the path that
/// was looked at, and what produces one, so the fix travels with the failure.
#[must_use]
pub fn failure(test: &str, tree: &Path) -> String {
    format!(
        "{test}: {VARIABLE} is set and the fixture tree at {} is absent on this runner, so the assertions that need it would have \
         been skipped; generate it with tools/redact_export.py or unset the variable",
        tree.display()
    )
}

/// Whether a runner demanded the tree.
///
/// The one half of the gate no test in this process can reach; see [`decide`].
///
/// **Its wiring into [`root`] is unpinned, and measurably so**: hard-coding the call above to
/// `false` passes all 673 tests on a box with the tree and on one without it, and is caught only by
/// a run with the tree moved aside AND [`VARIABLE`] set, where the 11 fixture tests stay green
/// instead of redding. Hard-coding it to `true` is the mirror image, caught only by a run with the
/// tree moved aside and the variable UNSET. So the check is two runs, not one, and neither of them
/// is something the suite performs on its own.
fn demanded() -> bool {
    std::env::var_os(VARIABLE).is_some()
}
