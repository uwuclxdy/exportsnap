//! Pins the shared external-tool gate in `tests/common`, which every video-touching test crate
//! routes its "is this tool usable" question through.
//!
//! Its own crate so these run once rather than once per crate declaring `mod common;`. What they
//! can cover is the DECISION: the probes answer whatever this box happens to have installed, so a
//! test asserting on them would assert on the machine, and the requirement variables cannot be set
//! at all from inside a process that forbids `unsafe`. Running a built test binary under a
//! constructed `PATH` and environment is what covers those two.

#![allow(clippy::unwrap_used, clippy::expect_used)]

/// The function-level dead-code allow lives on this declaration rather than inside the module, so
/// the crates that reach every tool-side function keep warning when one of them goes uncalled.
/// Measured by removing it: this crate warns 5 times (`usable`, `probe`, `answers`,
/// `reports_encoder`, `demanded` — it drives the decision half and never calls the probing half),
/// while `video`, `local_fix` and `chat_fix` warn zero on functions. Blanket-allowing inside the
/// module would have cost those three that coverage; the separate, narrower allow on `Tool` itself
/// covers the one unused VARIANT `video` legitimately has.
///
/// `export`, `overview` and `fixture_gate` carry the same allow for the mirror-image reason since
/// the fixture gate landed: they read `common::fixtures` and gate on no tool. The 5 above still
/// holds because `common`'s own allow on `pub mod fixtures;` absorbs that half here; with every
/// allow in the tree overridden (`RUSTFLAGS="--force-warn dead_code"`) this crate warns 11.
#[allow(dead_code, reason = "this crate pins the decision half and never calls the probing half")]
mod common;

use common::{Tool, Verdict, cache, decide, failure};

#[test]
fn a_call_site_runs_only_when_every_tool_it_asked_for_is_usable() {
    assert_eq!(decide(&[], |_| false, |_| false), Verdict::Run, "a test that reaches no tool never asks about one");
    assert_eq!(decide(&[Tool::Exiftool, Tool::Ffprobe], |_| true, |_| false), Verdict::Run);

    // The hole this gate exists to close: `ffmpeg` is right there and `ffprobe` is not, so a
    // spawn-only gate on `ffmpeg` answers "installed". What each call site then did at `277feac` is
    // measured and written down once, in `common`'s module doc. Read it there — a second telling
    // here is the copy nobody re-derives, and so the one that drifts.
    assert_eq!(decide(&[Tool::FfmpegFixtures, Tool::Ffprobe], |tool| tool != Tool::Ffprobe, |_| false), Verdict::Skip(Tool::Ffprobe));
    // The other half of it: an `ffmpeg` that spawns without the encoder the fixtures ask for is
    // exactly as unusable, and the gate has to be able to say so.
    assert_eq!(
        decide(&[Tool::Ffprobe, Tool::FfmpegFixtures], |tool| tool != Tool::FfmpegFixtures, |_| false),
        Verdict::Skip(Tool::FfmpegFixtures)
    );
    // The only input here with the unusable tool at index 0, and that is what it kills: a scan
    // starting at index 1 answers `Run`. **Not** a reverse-scan probe — both directions answer
    // `Skip(FfmpegTranscode)`, because `decide` names a tool only after `usable` returned false for
    // it and this closure leaves `Exiftool` usable, so `Skip(Exiftool)` is unreachable either way.
    assert_eq!(
        decide(&[Tool::FfmpegTranscode, Tool::Exiftool], |tool| tool != Tool::FfmpegTranscode, |_| false),
        Verdict::Skip(Tool::FfmpegTranscode)
    );
    // The reverse-scan probe is this one, and it needs BOTH unusable: with nothing to skip past,
    // slice POSITION alone picks which tool the message names — forward `Skip(Ffprobe)`, backward
    // `Skip(Exiftool)`. Neither of these two cases is the other's duplicate.
    assert_eq!(decide(&[Tool::Ffprobe, Tool::Exiftool], |_| false, |_| false), Verdict::Skip(Tool::Ffprobe));
}

#[test]
fn every_tool_caches_its_answer_in_a_slot_of_its_own() {
    // Four near-identical arms select these, and a copy-paste between two of them makes one tool
    // answer with another's cached probe — invisible on any box where the two agree, which is every
    // box with both installed and every box with neither.
    let all = [Tool::Exiftool, Tool::Ffprobe, Tool::FfmpegFixtures, Tool::FfmpegTranscode];
    for (position, one) in all.iter().enumerate() {
        for other in &all[position + 1..] {
            assert!(!std::ptr::eq(cache(*one), cache(*other)), "{one:?} and {other:?} share one cached answer");
        }
    }

    // The witness, and the honest limit of it: a fifth `Tool` makes this match non-exhaustive and
    // reds the build here, which is the prompt to extend `all` above. It cannot force that — a
    // fifth variant left out of `all` costs this test coverage, not correctness. `cache` itself is
    // where correctness is compiler-held: a variant with no slot does not compile at all.
    for tool in all {
        match tool {
            Tool::Exiftool | Tool::Ffprobe | Tool::FfmpegFixtures | Tool::FfmpegTranscode => {}
        }
    }
}

#[test]
fn a_demanded_tool_that_is_not_usable_fails_the_run_rather_than_skipping() {
    assert_eq!(decide(&[Tool::Ffprobe], |_| false, |_| true), Verdict::Fail(Tool::Ffprobe));
    // Demanding a tool that IS usable changes nothing: the variable removes the skip, it does not
    // add a check.
    assert_eq!(decide(&[Tool::Ffprobe], |_| true, |_| true), Verdict::Run);
    // And the demand consulted is the MISSING tool's own. `exiftool` being demanded here is
    // irrelevant because `exiftool` is usable, so this skips rather than failing.
    assert_eq!(
        decide(&[Tool::Exiftool, Tool::Ffprobe], |tool| tool != Tool::Ffprobe, |tool| tool == Tool::Exiftool),
        Verdict::Skip(Tool::Ffprobe)
    );
}

#[test]
fn every_ffmpeg_capability_answers_to_one_variable_and_exiftool_to_its_own() {
    // `ffprobe` ships in the ffmpeg distribution and gets no variable of its own: a runner that
    // satisfied the ffmpeg variable already has it, so a second one would be one more thing to
    // forget for no coverage gained. That is an argument about `ffprobe`, not about how many
    // variables the crate has — `common::fixtures::VARIABLE` is a third, pinned in
    // `tests/fixture_gate.rs`, and a CI leg exports every one of the three it can satisfy.
    assert_eq!(Tool::Ffprobe.variable(), "EXPORTSNAP_REQUIRE_FFMPEG");
    assert_eq!(Tool::FfmpegFixtures.variable(), "EXPORTSNAP_REQUIRE_FFMPEG");
    assert_eq!(Tool::FfmpegTranscode.variable(), "EXPORTSNAP_REQUIRE_FFMPEG");
    assert_eq!(Tool::Exiftool.variable(), "EXPORTSNAP_REQUIRE_EXIFTOOL");
}

#[test]
fn a_failure_names_the_runner_variable_the_test_and_what_has_to_be_installed() {
    let message = failure("some_test", Tool::FfmpegFixtures);

    assert!(message.contains("some_test"), "{message}");
    assert!(message.contains("EXPORTSNAP_REQUIRE_FFMPEG"), "{message}");
    // The capability, not only the binary. An `ffmpeg` built without `libx265` is on `PATH` and
    // still unusable, so "ffmpeg is not on PATH" would send a runner looking for the wrong thing.
    assert!(message.contains("libx265"), "{message}");

    assert!(failure("some_test", Tool::Exiftool).contains("EXPORTSNAP_REQUIRE_EXIFTOOL"));
}
