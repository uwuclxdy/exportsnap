//! The one answer to "can this box run the checks that need an external tool", shared by every
//! `tests/*.rs` crate that gates on one.
//!
//! **The question a gate has to ask is whether a tool is USABLE, not whether a spawn succeeded.**
//! Each of the three video-touching crates used to answer it by launching `ffmpeg` and reading a
//! spawn failure as "not installed", so `ffprobe` and the encoders were never asked about at all.
//! Measured at `277feac` on a box carrying `ffmpeg` and `exiftool` and no `ffprobe`: four tests
//! redded naming something other than the missing tool. `video::every_header_date_and_both_tags_…`
//! panicked at its own `let probed = ffprobe(&written).unwrap();` — the reader handed back a `None`
//! and the test body unwrapped it, so the death was in the body and `fn ffprobe`, which ends
//! `.output().ok()?`, never panicked at all. `local_fix::a_transcoding_run_…`,
//! `local_fix::a_run_that_is_not_transcoding_…` and `chat_fix::each_overlay_mode_…_on_the_video_leg`
//! each failed a codec `assert_eq!` against a `None`. A fifth,
//! `video::an_unknown_offset_writes_no_zone_rather_than_claiming_utc`, carried an ffprobe skip of
//! its own and skipped cleanly — it was the one call site already asking the right question, and
//! the second spelling that made the answer inconsistent. [`usable`] is the single place that
//! answers it now.
//!
//! **Every test that needs a tool asks once, up front, for every tool it will reach.** Past that
//! gate the fixture builders and the independent readers assert rather than returning `Option`,
//! because past it a spawn failure is a genuine red and not an absence — an `Option` there would be
//! a second place that can say "not installed", which is the shape this module exists to remove.
//!
//! [`Tool`] names a capability rather than a binary, so a call site claims only what it uses: an
//! `ffmpeg` that runs is not an `ffmpeg` that can encode HEVC. Nothing in these crates decodes
//! without also having built a fixture, so there is no decode-only variant; add one when a call
//! site needs it rather than widening one that already means more.
//!
//! [`fixtures`] answers the same demanded-versus-absent question about the `fixtures/` TREE. Its
//! own module because the two halves are read by disjoint sets of crates and each half is dead code
//! in the other's, which the allow below is scoped against. [`composite`] is a third half on the
//! same terms and for the same reason, holding the one spelling of the overlay-transparency
//! assertion that `local_fix` and `chat_fix` had a copy of each.

use std::process::Command;
use std::sync::OnceLock;

/// The module-level allow is what keeps the three tool-gating crates measuring their own half.
///
/// **Measured under `RUSTFLAGS="--force-warn dead_code"`, which overrides every allow in the tree
/// and so reports what each crate would warn with none of them.** Stated per HALF rather than as a
/// per-crate total, because the total moves whenever a half is added and the split is what decides
/// where an allow goes — re-measured 2026-08-11 when [`composite`] became the third half:
///
/// | crate | this module's top level | [`fixtures`] | [`composite`] |
/// |---|---|---|---|
/// | `local_fix` | 0 | 6 | 0 |
/// | `chat_fix` | 0 | 6 | 3 |
/// | `video` | 1 | 6 | 6 |
/// | `export` | 11 | 0 | 6 |
/// | `overview` | 11 | 0 | 6 |
/// | `history` | 11 | 0 | 6 |
/// | `fixture_gate` | 11 | 2 | 6 |
/// | `tool_gate` | 5 | 6 | 6 |
///
/// `video`, `local_fix` and `chat_fix` reach every function out here, which is the zero in the first
/// column; `video`'s 1 is the [`Tool`] variant its own allow is documented against, not a function.
/// The alternative placement is a crate-level allow on those three `mod common;` declarations, which
/// would have taken that zero with it. `export`, `overview`, `history` and `fixture_gate` are the
/// mirror image at 11 tool-side warnings each and carry that crate-level allow instead. `tool_gate` warns 5 out
/// here, which is the number its own allow is documented against, and 17 with nothing allowed at
/// all.
///
/// **The cost, stated rather than glossed: an uncalled function under [`fixtures`] warns nowhere.**
/// All six of its items are live in `export` and `overview` — those two reach the whole chain from
/// `root` down — but both crates have to allow dead code crate-wide for the tool half anyway, so
/// there is no crate left in which a new dead fixture helper would surface. With two halves and no
/// crate reading both, one of them loses that signal whichever way the allows are placed; this
/// keeps it on the larger half, the one three crates exercise in full.
#[allow(dead_code, reason = "the crates that gate on a tool never read the fixture tree, and the reverse")]
pub mod fixtures;

/// The overlay-transparency assertion and the blocks it is measured at, read by `local_fix` and
/// `chat_fix` and by nothing else.
///
/// Scoped for the same reason [`fixtures`] is, and the table above is why it could not go at this
/// module's top level: the third column is what these six items cost every crate that does not read
/// them, and at the top level that cost would have landed on the first column instead — taking the
/// zero `video`, `local_fix` and `chat_fix` hold there, which is the signal the whole placement
/// exists to keep.
///
/// **The cost is [`fixtures`]'s cost, on a smaller half.** `local_fix` reaches all six items and
/// `chat_fix` three, so a new dead helper here warns in neither, and no other crate reads the module
/// at all. The three it costs are the blocks only `local_fix` passes: they are that crate's fixture
/// sizes, they belong beside the measured table in [`composite::assert_shows_main_through`] that
/// cites them by name, and splitting them out to buy the signal back is the fork this module was
/// added to end.
#[allow(dead_code, reason = "chat_fix passes only the 64x48 block; the other three are local_fix's fixture sizes")]
pub mod composite;

/// A capability a gating call site depends on, named for exactly what its probe verifies.
///
/// The allow is scoped to the VARIANTS and measured: `tests/video.rs` drives no transcode, so it
/// never constructs [`Self::FfmpegTranscode`] and warns once without this, while `tests/local_fix.rs`
/// and `tests/chat_fix.rs` warn zero. Deliberately not widened to the whole module — a function
/// here going uncalled is a real signal and those three crates keep it.
#[allow(dead_code, reason = "a crate that claims only some capabilities is correct, not incomplete")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `exiftool` runs and answers `-ver`: the independent metadata reader.
    Exiftool,
    /// `ffprobe` runs and answers `-version`: the independent container reader.
    ///
    /// Answers to the ffmpeg variable rather than one of its own, and the reason is `ffprobe`
    /// itself rather than any count of variables: it ships inside the ffmpeg distribution, so a
    /// runner that satisfied `EXPORTSNAP_REQUIRE_FFMPEG` already has it, and a second variable would
    /// be one more thing to forget for no coverage gained.
    Ffprobe,
    /// `ffmpeg` runs and carries the encoders every fixture builder in these crates asks for:
    /// `libx265` for the `hvc1` main and `aac` for its audio track.
    ///
    /// Decoding an output back needs nothing beyond `ffmpeg` itself, so this covers the read-back
    /// legs too, and every one of them belongs to a test that built a fixture first.
    FfmpegFixtures,
    /// `ffmpeg` runs and carries the encoder the crate's own transcode names, `libx264`.
    ///
    /// That crate-side name is a private const in `src/export/ffmpeg.rs` which a test crate cannot
    /// reach, so this is a hand-maintained copy of it with nothing linking the two: changing the
    /// codec there and not here leaves this gate claiming an encoder the run no longer uses.
    ///
    /// Separate from [`Self::FfmpegFixtures`] because the runs that copy pixels rather than
    /// re-encode them never reach it, and a gate must not make them claim it.
    FfmpegTranscode,
}

impl Tool {
    /// What a message calls this, spelling the capability rather than only the binary: "ffmpeg is
    /// not on PATH" is the wrong sentence to print at a box whose `ffmpeg` is right there without
    /// `libx265`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Exiftool => "exiftool",
            Self::Ffprobe => "ffprobe",
            Self::FfmpegFixtures => "ffmpeg with the libx265 and aac encoders",
            Self::FfmpegTranscode => "ffmpeg with the libx264 encoder",
        }
    }

    /// Set this on a runner and this tool being unusable fails the run instead of skipping the
    /// checks that needed it.
    ///
    /// **These are not the only two.** [`fixtures::VARIABLE`] is the third and answers for the
    /// `fixtures/` tree, so a CI leg exports every one of the three it can satisfy — a leg that
    /// exports a subset leaves the rest of the gates disarmed and reads green over whatever they
    /// were guarding, which is the defect this whole mechanism exists to remove.
    #[must_use]
    pub fn variable(self) -> &'static str {
        match self {
            Self::Exiftool => "EXPORTSNAP_REQUIRE_EXIFTOOL",
            Self::Ffprobe | Self::FfmpegFixtures | Self::FfmpegTranscode => "EXPORTSNAP_REQUIRE_FFMPEG",
        }
    }
}

/// What a call site does about the tools it asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every tool asked for is usable here.
    Run,
    /// This one is not, and no runner demanded it.
    Skip(Tool),
    /// This one is not, and its [`Tool::variable`] demanded it.
    Fail(Tool),
}

/// `true` when every tool in `needed` is usable here.
///
/// Otherwise records why the caller's checks did not run and returns `false`, so the caller
/// returns — unless the tool's [`Tool::variable`] is set, which is a runner stating it expects the
/// tool, and then this fails the test naming both.
///
/// Call it as the first statement of a test, listing every tool that test will reach, so an absent
/// one is reported as an absent tool rather than surfacing later as a fixture that would not build.
///
/// `#[must_use]` because dropping the answer is what silently disarms the gate: a bare
/// `usable("t", &[Tool::Exiftool]);` reads like a gate, opens nothing, and dies at the reader's
/// `expect` with the wrong message — the exact failure this module removes. The attribute makes
/// that an `unused_must_use`, which the gate's `-D warnings` rejects; no clippy lint in this
/// crate's `Cargo.toml` would have.
#[must_use]
pub fn usable(test: &str, needed: &[Tool]) -> bool {
    match decide(needed, probe, demanded) {
        Verdict::Run => true,
        Verdict::Skip(tool) => {
            println!("SKIPPED {test}: {} is not usable here, so its assertions did not run", tool.label());
            false
        }
        Verdict::Fail(tool) => panic!("{}", failure(test, tool)),
    }
}

/// [`usable`]'s decision, against arbitrary answers rather than against this box.
///
/// The seam exists because neither half can be pinned any other way: a real probe answers whatever
/// the machine running the suite happens to have installed, and the demand half cannot be set at
/// all from inside the process — `std::env::set_var` is `unsafe` from edition 2024 and this crate
/// forbids `unsafe_code`, tests included. `tests/tool_gate.rs` drives this; the two real probes and
/// the variable read are covered only by running a built test binary under a constructed
/// environment.
#[must_use]
pub fn decide(needed: &[Tool], usable: impl Fn(Tool) -> bool, demanded: impl Fn(Tool) -> bool) -> Verdict {
    for &tool in needed {
        if usable(tool) {
            continue;
        }
        return if demanded(tool) { Verdict::Fail(tool) } else { Verdict::Skip(tool) };
    }
    Verdict::Run
}

/// What a run fails with when a demanded tool is not usable: the runner's own variable and the
/// capability it has to install, so the fix travels with the failure.
#[must_use]
pub fn failure(test: &str, tool: Tool) -> String {
    format!(
        "{test}: {} is set and {} is not usable on this runner, so the assertions that need it would have been \
         skipped; install it on this runner or unset the variable",
        tool.variable(),
        tool.label()
    )
}

/// The slot `tool` caches its answer in.
///
/// **One `static` per variant, selected by a wildcard-free match, and deliberately not an array
/// indexed by the variant.** An array has to be sized by something, and the only thing available
/// is a hand-written `const ALL: [Self; N]` that no compiler check ties to the variant count: add
/// a fifth [`Tool`], give it its three match arms — which the wildcard-free matches DO force — and
/// leave `ALL` at four, and the whole thing builds with zero warnings and then panics `index out of
/// bounds: the len is 4 but the index is 4` at the first probe. `docs/handoff-state.md` records
/// that class already ("`const ALL` is not an exhaustiveness witness, and reads exactly like one").
/// Selecting the slot by name has no length to get wrong: a fifth variant cannot compile until it
/// is given a slot here.
///
/// Exposed so a test can assert the four slots are four rather than two spellings of one; the
/// arms are near-identical and a copy-paste between them returns another tool's answer.
pub fn cache(tool: Tool) -> &'static OnceLock<bool> {
    static EXIFTOOL: OnceLock<bool> = OnceLock::new();
    static FFPROBE: OnceLock<bool> = OnceLock::new();
    static FFMPEG_FIXTURES: OnceLock<bool> = OnceLock::new();
    static FFMPEG_TRANSCODE: OnceLock<bool> = OnceLock::new();

    match tool {
        Tool::Exiftool => &EXIFTOOL,
        Tool::Ffprobe => &FFPROBE,
        Tool::FfmpegFixtures => &FFMPEG_FIXTURES,
        Tool::FfmpegTranscode => &FFMPEG_TRANSCODE,
    }
}

/// Whether `tool` is usable here, answered once per process.
///
/// Cached because every answer costs a process spawn and nextest gives each test its own process,
/// so an uncached probe would be paid again by every call site in that test. Lazy per variant: a
/// crate that never asks about `exiftool` never spawns it.
fn probe(tool: Tool) -> bool {
    *cache(tool).get_or_init(|| match tool {
        Tool::Exiftool => answers("exiftool", "-ver"),
        Tool::Ffprobe => answers("ffprobe", "-version"),
        Tool::FfmpegFixtures => answers("ffmpeg", "-version") && reports_encoder("libx265") && reports_encoder("aac"),
        Tool::FfmpegTranscode => answers("ffmpeg", "-version") && reports_encoder("libx264"),
    })
}

/// Whether `command` spawns at all and its version flag exits 0.
///
/// **A failed spawn is not proof the tool is absent, and this whole suite lives with that.** On
/// Windows `CreateProcess` appends only `.exe` and never consults `PATHEXT`, so a `.cmd`/`.ps1`
/// shim the shell runs fine is invisible to any spawn probe — the reason `src/export/env.rs`'s
/// `locate` resolves through `which` instead. Read its doc before assuming that fix ports here.
///
/// It does not, and the two hard reasons come first. The ceiling is the SUITE's rather than this
/// function's: the fixture builders spawn `ffmpeg` by bare name too, so a Windows CI leg has to
/// answer this for every `Command::new` under `tests/` and fixing one probe buys nothing. And
/// `env::Tool` carries only `Ffmpeg` and `Vlc`, where widening it to reach `exiftool` and
/// `ffprobe` is not merely a longer list: `src/tui/screens/overview.rs` sizes `ENVIRONMENT_ROWS`
/// from `Tool::ALL.len()` under a `const _: () = assert!(ENVIRONMENT_ROWS <=
/// GUARANTEED_INTERIOR_ROWS)`, so a fifth tool puts a compile-time assert at risk. (Reaching
/// `which` from here would also want it in `[dev-dependencies]`, which is a cost rather than a
/// blocker and is why it is listed last.)
fn answers(command: &str, version_flag: &str) -> bool {
    Command::new(command).arg(version_flag).output().is_ok_and(|output| output.status.success())
}

/// Whether this `ffmpeg` reports an encoder under exactly this name.
///
/// **The exit code is not a discriminator here.** `ffmpeg -h encoder=<name>` exits 0 for a name it
/// has never heard of, printing `Exiting with exit code 0` (measured at n9.0). Stdout is what
/// separates the two, and the match is on the PRESENT form (`Encoder <name> `) rather than on the
/// absence line, because a check for an absence message fails open the day upstream rewords it.
///
/// The name has to be the ENCODER's rather than the codec's: `-h encoder=h264` answers about
/// `libx264` (measured at n9.0), so a query spelled with a codec name never matches its own string.
fn reports_encoder(name: &str) -> bool {
    let Ok(output) = Command::new("ffmpeg").args(["-hide_banner", "-h"]).arg(format!("encoder={name}")).output() else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).contains(format!("Encoder {name} ").as_str())
}

/// Whether a runner demanded this tool.
///
/// The one half of the gate no test in this process can reach: setting the variable needs
/// `std::env::set_var`, which edition 2024 makes `unsafe` and this crate forbids. Its pin is a run
/// of a built test binary under a constructed environment.
fn demanded(tool: Tool) -> bool {
    std::env::var_os(tool.variable()).is_some()
}
