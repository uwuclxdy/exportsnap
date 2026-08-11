//! Entry point: theme argument, tier detection, terminal bootstrap and teardown.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use exportsnap::app::App;
use exportsnap::tui::theme::{self, Tier};

/// The `--version` text, four lines: the binary name and version, the
/// OpenStreetMap/ODbL credit with the data's vehicle named, the ODbL URL, and
/// a pointer at the generated third-party notices. A constant so the inline
/// tests can pin its whole shape instead of re-implementing what they assert.
pub const VERSION_TEXT: &str = concat!(
    "exportsnap ",
    env!("CARGO_PKG_VERSION"),
    "\n",
    "Contains timezone boundary polygons © OpenStreetMap contributors, licensed under the Open Database License (ODbL-1.0)\n",
    "https://opendatacommons.org/licenses/odbl/1-0/\n",
    "Timezone data via the tzf-dist crate; full third-party notices: THIRD-PARTY-LICENSES\n",
);

/// The `--help` text: what the binary is, how it is invoked, and every flag it answers to. A
/// constant for [`VERSION_TEXT`]'s reason — the inline tests pin its shape against the flag set
/// rather than re-implementing it — and the flags are spelled with their `=` so the one form the
/// parsers accept is the one the help shows.
///
/// **No project or bug-report URL**, which GNU would end a help text with: `Cargo.toml` carries no
/// `repository` field (the publish metadata is deliberately deleted until phase 5), so there is
/// nothing honest to print yet. Phase 5 owns the URL, along with the README and the wiki this text
/// stands in for until they exist.
pub const HELP_TEXT: &str = concat!(
    "exportsnap — give a Snapchat data export back its metadata\n",
    "\n",
    "Usage: exportsnap [OPTIONS]\n",
    "\n",
    "Options:\n",
    "  --source=<dir>   the dir holding the export's zips and unpacked parts; defaults to the working dir\n",
    "  --out=<dir>      where a run writes the fixed files; defaults to <source>/exportsnap-out\n",
    "  --theme=<tier>   full or compatible; detected from the environment when absent\n",
    "  --print-source   print what exportsnap was launched against, then exit; takes no value\n",
    "  --version        print the version and the third-party attribution, then exit; takes no value\n",
    "  -h, --help       print this text, then exit\n",
    "\n",
    "With no options exportsnap opens its terminal ui against the dir you ran it from.\n",
);

/// Every `--` spelling the parsers below answer to, and the set [`reject_unknown_args`] measures an
/// argument against. An entry matches whole or followed by `=`, so both a flag's bare form and its
/// valued form reach the parser that owns its own error message.
const KNOWN_FLAGS: [&str; 6] = ["--theme", "--source", "--out", "--version", "--print-source", "--help"];

fn main() -> Result<()> {
    let args = utf8_args(std::env::args_os().skip(1))?;

    // `--version` wins over every other flag and prints before the terminal is
    // taken over, so it works headless, piped, and in scripts. A reader leaving
    // early (`exportsnap --version | head -1`) is a finished run, not a
    // failure: exit 0, per the EPIPE convention in the rust learnings.
    if wants_version_arg(args.iter().cloned())? {
        let mut out = std::io::stdout().lock();
        if let Err(e) = out.write_all(VERSION_TEXT.as_bytes())
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(e).context("failed to print the version text");
        }
        return Ok(());
    }

    // `--help` is checked immediately after `--version` and ahead of every parse, and prints before
    // the terminal is taken over for `--version`'s reasons. **The order between the two is fixed
    // rather than first-in-argv**: `--version` keeps winning, so the comment above stays literally
    // true and no shipped precedence moves, and the ODbL attribution it carries (decision 38) stays
    // unconditional. Both print to stdout and exit 0, so a user passing both loses nothing either
    // way. GNU is first-in-argv (`ls --version --help` prints the version, `ls --help --version`
    // prints help) and ripgrep is version-always; a fixed order sits inside that spread and is what
    // this crate's per-flag scans can express.
    if wants_help_arg(args.iter().cloned())? {
        let mut out = std::io::stdout().lock();
        if let Err(e) = out.write_all(HELP_TEXT.as_bytes())
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(e).context("failed to print the help text");
        }
        return Ok(());
    }

    // Runs after the two flags that ignore the rest of the command line and before every parse, so a
    // typo is reported as a typo rather than as a downstream parse failure or a silent TUI launch.
    reject_unknown_args(args.iter().cloned())?;

    // Unlike `--version`, this one does not short-circuit here: its whole report is read off the
    // composed app below, so everything that feeds the composition still has to parse first.
    let print_source = wants_print_source_arg(args.iter().cloned())?;

    let cli_tier = parse_theme_arg(args.iter().cloned())?;
    // The config precedence level has no loader yet; `detect_from_env` still orders it.
    let tier = theme::detect_from_env(cli_tier, None);

    // The dir the user points at, or the one they ran from. Parsed before the terminal is taken
    // over, so a bad argument is a plain message on a plain terminal rather than a flash of
    // alternate screen.
    let source = match parse_source_arg(args.iter().cloned())? {
        Some(dir) => dir,
        None => std::env::current_dir().context("could not read the working dir; pass --source=<dir> instead")?,
    };
    // `--out=<dir>` names where a memories run writes (decision 33); absent, the run uses
    // `default_out_root(source)`. Parsed before the terminal is taken over, like the source.
    let out = parse_out_arg(args)?;

    // Every screen is built here, before the terminal is taken over, and the reads never fail: an
    // absent or unreadable export is a state the overview has words for.
    //
    // Deliberate ceiling: this is blocking, so a large `json/` delays the first frame with nothing
    // on screen to say why. The upgrade path is the phase-2 tokio runtime plus the overview's own
    // loading state, which needs the tick timer no screen has earned yet.
    let mut app = App::start(tier, source, out);

    // Read off the COMPOSED app rather than off the `source` local above. `main` could print the
    // path byte-identically — its own M2 mutation proved that — so what makes this a pin is the rest
    // of the report: the part counts, the space figures and each media screen's copy of the argument
    // are all things only a built app holds. `tests/print_source.rs` is what reds when a delivery
    // between here and the screens is dropped. Prints and exits before the terminal is taken over,
    // so it works headless and in scripts; the EPIPE arm is `--version`'s, for the same reason.
    if print_source {
        let mut stdout = std::io::stdout().lock();
        if let Err(e) = stdout.write_all(app.source_report().as_bytes())
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(e).context("failed to print the source report");
        }
        return Ok(());
    }

    // `try_init` installs ratatui's own restore-then-chain panic hook; unlike `init` it hands
    // back the failure instead of panicking on it. It enables raw mode on `/dev/tty` before
    // writing to stdout and sizing the terminal, so a failure in either later step returns an
    // `Err` with raw mode already on — hence the restore on that arm. `disable_raw_mode`
    // no-ops when no prior mode was saved, so it is safe even when init failed on its first
    // step. `DefaultTerminal::drop` restores nothing but the cursor, so the `restore` after
    // the loop is likewise what covers the error path out of `App::run`.
    let mut terminal = ratatui::try_init()
        .inspect_err(|_| ratatui::restore())
        .context("failed to take over the terminal; run exportsnap in an interactive terminal")?;
    let result = app.run(&mut terminal);
    ratatui::restore();

    result.context("the terminal ui stopped on an error")
}

/// Reads argv as text, naming the argument the crate cannot represent instead of aborting on it
/// (decision 56a). `std::env::args` panics on the first argument that is not valid UTF-8, which is
/// exit 101 and a bug's failure shape for what is plain bad input — and a caller building
/// `--source=<dir>` out of a filesystem walk can hand it bytes no unix filesystem forbids.
///
/// **The argument is REFUSED rather than honoured.** Carrying the bytes down to `--source` would
/// mean `OsStr::as_encoded_bytes` plus the reconstruction back to an `OsStr`, which is `unsafe` and
/// this crate forbids `unsafe`. Collecting here keeps the five parsers on `String` and moves one
/// call site.
///
/// The lossy spelling is printed through `Debug` for the reason
/// [`exportsnap::app::App::source_report`] quotes its paths: an argument is unvalidated bytes
/// reaching a stream, and the escapes keep a control character in one from acting on the terminal
/// it lands in. Lossless it is not — the invalid bytes are already `U+FFFD` by then, which is why
/// the message says what it says rather than offering to echo the original.
fn utf8_args(args: impl IntoIterator<Item = OsString>) -> Result<Vec<String>> {
    args.into_iter()
        .map(|arg| {
            arg.into_string().map_err(|arg| {
                let lossy = arg.to_string_lossy();
                anyhow!("{lossy:?}: not valid utf-8; exportsnap reads every argument as text, so pass a utf-8 spelling of it")
            })
        })
        .collect()
}

/// Hand-parses `--theme=full` / `--theme=compatible`, last one wins. A real CLI with
/// subcommands is phase 5 and brings its own argument parser then; a bare argument is left alone
/// until it exists, while an unrecognized `--` one is refused by [`reject_unknown_args`]
/// (decision 56c).
fn parse_theme_arg(args: impl IntoIterator<Item = String>) -> Result<Option<Tier>> {
    let mut tier = None;
    for arg in args {
        if arg == "--theme" {
            bail!("--theme needs a value; pass --theme=full or --theme=compatible");
        }
        let Some(value) = arg.strip_prefix("--theme=") else {
            continue;
        };
        tier = Some(
            Tier::from_name(value).with_context(|| format!("--theme={value}: unknown theme; pass --theme=full or --theme=compatible"))?,
        );
    }
    Ok(tier)
}

/// Hand-parses `--source=<dir>`, last one wins. Same shape as [`parse_theme_arg`] and for the same
/// reason: a real CLI with subcommands is phase 5 and brings its own argument parser then.
///
/// `None` means no `--source` was passed, which the caller resolves to the working dir. A dir that
/// does not exist is NOT rejected here — that is the overview's `source dir unreadable` state, and
/// refusing to start over it would mean the user cannot open the app to see what it thinks.
fn parse_source_arg(args: impl IntoIterator<Item = String>) -> Result<Option<PathBuf>> {
    let mut source = None;
    for arg in args {
        if arg == "--source" {
            bail!("--source needs a value; pass --source=<dir> naming the dir that holds the export's zips");
        }
        let Some(value) = arg.strip_prefix("--source=") else {
            continue;
        };
        if value.is_empty() {
            bail!("--source= names no dir; pass --source=<dir> naming the dir that holds the export's zips");
        }
        source = Some(PathBuf::from(value));
    }
    Ok(source)
}

/// Hand-parses `--out=<dir>`, last one wins: where a memories run writes (decision 33). Same
/// shape as [`parse_source_arg`] and for the same reason: a real CLI with subcommands is phase 5
/// and brings its own argument parser then.
///
/// `None` means no `--out` was passed, which each media screen resolves to
/// `default_out_root(source)`.
fn parse_out_arg(args: impl IntoIterator<Item = String>) -> Result<Option<PathBuf>> {
    let mut out = None;
    for arg in args {
        if arg == "--out" {
            bail!("--out needs a value; pass --out=<dir> naming where the fixed memories land");
        }
        let Some(value) = arg.strip_prefix("--out=") else {
            continue;
        };
        if value.is_empty() {
            bail!("--out= names no dir; pass --out=<dir> naming where the fixed memories land");
        }
        out = Some(PathBuf::from(value));
    }
    Ok(out)
}

/// Hand-parses `--version`: any occurrence wins — the flag takes no state, so
/// there is no last one to speak of. A `--version=` value is the one early
/// fire. The caller checks it before anything touches the terminal. Same shape
/// as [`parse_theme_arg`] and for the same reason: a real CLI with subcommands
/// is phase 5 and brings its own argument parser then.
fn wants_version_arg(args: impl IntoIterator<Item = String>) -> Result<bool> {
    let mut version = false;
    for arg in args {
        if arg == "--version" {
            version = true;
        } else if arg.starts_with("--version=") {
            bail!("--version takes no value; pass --version alone");
        }
    }
    Ok(version)
}

/// Hand-parses `-h` / `--help`: any occurrence wins and a `--help=` value is a hard error, both for
/// [`wants_version_arg`]'s reasons — the flag carries no state, and a value is a user believing it
/// takes one. `-h` is the only single-dash spelling this crate reads; every other one is still left
/// alone, so `-h=1` reaches no parser and opens the ui, the way `-x` always has.
///
/// Landing help before phase 5's argument parser is decision 56b: phase 5 is gated behind phases
/// 2-4, while `-h`/`--help` is reserved by convention and reachable today — without this arm it
/// fell through to the terminal takeover and exited 1 with a message about the terminal.
fn wants_help_arg(args: impl IntoIterator<Item = String>) -> Result<bool> {
    let mut help = false;
    for arg in args {
        if arg == "--help" || arg == "-h" {
            help = true;
        } else if arg.starts_with("--help=") {
            bail!("--help takes no value; pass --help alone");
        }
    }
    Ok(help)
}

/// Refuses an argument that starts with `--` and is not one of [`KNOWN_FLAGS`] (decision 56c), which
/// supersedes the "anything else on the command line is left alone" convention for that shape only:
/// `--print-sourc` used to launch the ui, which is hostile for a flag whose whole point is
/// scripting. A BARE argument keeps the old convention, so nothing that works today breaks. `--` on
/// its own is refused with the rest — this crate reads no positional arguments, so there is nothing
/// for it to separate.
///
/// A known flag's own error still comes from the parser that owns it: every spelling in the set is
/// matched whole or followed by `=`, so `--theme` alone and `--source=` reach their own messages
/// rather than being reported as unknown.
///
/// The refused argument is quoted through `Debug` for [`utf8_args`]'s reason: it is unvalidated
/// bytes on their way to a stream.
fn reject_unknown_args(args: impl IntoIterator<Item = String>) -> Result<()> {
    for arg in args {
        if !arg.starts_with("--") {
            continue;
        }
        if KNOWN_FLAGS.iter().any(|flag| arg == *flag || arg.strip_prefix(flag).is_some_and(|rest| rest.starts_with('='))) {
            continue;
        }
        bail!("{arg:?}: unknown flag; run exportsnap --help for the flags it takes");
    }
    Ok(())
}

/// Hand-parses `--print-source`: what the app was launched against and what it found there, on
/// stdout, with no terminal taken over. Any occurrence wins and a `--print-source=` value is a hard
/// error, both for [`wants_version_arg`]'s reasons — the flag carries no state, so there is no last
/// one to speak of, and a value is a user believing it takes one. `--version` and `--help` are both
/// checked first and return before this is read. Same shape as [`parse_theme_arg`] and for the same
/// CLI with subcommands is phase 5 and brings its own argument parser then.
fn wants_print_source_arg(args: impl IntoIterator<Item = String>) -> Result<bool> {
    let mut print_source = false;
    for arg in args {
        if arg == "--print-source" {
            print_source = true;
        } else if arg.starts_with("--print-source=") {
            bail!("--print-source takes no value; pass --print-source alone, with --source=<dir> naming the dir");
        }
    }
    Ok(print_source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Tier>> {
        parse_theme_arg(args.iter().map(|arg| (*arg).to_string()))
    }

    fn parse_source(args: &[&str]) -> Result<Option<PathBuf>> {
        parse_source_arg(args.iter().map(|arg| (*arg).to_string()))
    }

    fn parse_version(args: &[&str]) -> Result<bool> {
        wants_version_arg(args.iter().map(|arg| (*arg).to_string()))
    }

    fn parse_out(args: &[&str]) -> Result<Option<PathBuf>> {
        parse_out_arg(args.iter().map(|arg| (*arg).to_string()))
    }

    fn parse_print_source(args: &[&str]) -> Result<bool> {
        wants_print_source_arg(args.iter().map(|arg| (*arg).to_string()))
    }

    fn parse_help(args: &[&str]) -> Result<bool> {
        wants_help_arg(args.iter().map(|arg| (*arg).to_string()))
    }

    fn reject_unknown(args: &[&str]) -> Result<()> {
        reject_unknown_args(args.iter().map(|arg| (*arg).to_string()))
    }

    #[test]
    fn argv_reaches_the_parsers_as_text() {
        let args = utf8_args(["--theme=full", "--source=/tmp/export"].map(OsString::from)).unwrap();
        assert_eq!(args, ["--theme=full", "--source=/tmp/export"]);
        assert!(utf8_args([]).unwrap().is_empty());
    }

    /// Unix only, and the gate is the FIXTURE rather than the assertion, the same way
    /// `tests/print_source.rs`'s hostile path is: `OsString::from_vec` is `std::os::unix` and a
    /// lone `0xff` is a legal byte in a unix filename. The Windows analogue is an unpaired
    /// surrogate through `OsStringExt::from_wide`, which is a different fixture, not a different
    /// assertion.
    #[cfg(unix)]
    #[test]
    fn an_argument_that_is_not_utf8_is_a_hard_error_naming_it() {
        use std::os::unix::ffi::OsStringExt;

        let bad = OsString::from_vec(b"--source=/tmp/\xff".to_vec());
        let error = utf8_args([bad]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "\"--source=/tmp/\u{fffd}\": not valid utf-8; exportsnap reads every argument as text, so pass a utf-8 spelling of it"
        );
    }

    #[test]
    fn absent_out_arg_leaves_the_dir_to_the_memories_screen() {
        assert_eq!(parse_out(&[]).unwrap(), None);
        assert_eq!(parse_out(&["--theme=full", "somewhere"]).unwrap(), None);
    }

    #[test]
    fn out_arg_takes_the_dir_after_the_equals() {
        assert_eq!(parse_out(&["--out=/tmp/fixed"]).unwrap(), Some(PathBuf::from("/tmp/fixed")));
    }

    #[test]
    fn last_out_arg_wins() {
        assert_eq!(parse_out(&["--out=/tmp/one", "--out=/tmp/two"]).unwrap(), Some(PathBuf::from("/tmp/two")));
    }

    #[test]
    fn out_flag_without_a_value_is_a_hard_error() {
        let error = parse_out(&["--out", "/tmp/fixed"]).unwrap_err();
        assert_eq!(error.to_string(), "--out needs a value; pass --out=<dir> naming where the fixed memories land");
    }

    #[test]
    fn an_empty_out_value_is_a_hard_error() {
        let error = parse_out(&["--out="]).unwrap_err();
        assert_eq!(error.to_string(), "--out= names no dir; pass --out=<dir> naming where the fixed memories land");
    }

    #[test]
    fn absent_theme_arg_leaves_the_cli_level_unset() {
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(parse(&["--verbose", "some/path"]).unwrap(), None);
    }

    #[test]
    fn theme_arg_parses_both_tier_names() {
        assert_eq!(parse(&["--theme=full"]).unwrap(), Some(Tier::Full));
        assert_eq!(parse(&["--theme=compatible"]).unwrap(), Some(Tier::Compatible));
    }

    #[test]
    fn last_theme_arg_wins() {
        assert_eq!(parse(&["--theme=full", "--theme=compatible"]).unwrap(), Some(Tier::Compatible));
    }

    #[test]
    fn unknown_theme_value_is_a_hard_error_naming_input_and_fix() {
        let error = parse(&["--theme=24bit"]).unwrap_err();
        assert_eq!(error.to_string(), "--theme=24bit: unknown theme; pass --theme=full or --theme=compatible");
    }

    #[test]
    fn theme_flag_without_a_value_is_a_hard_error() {
        let error = parse(&["--theme", "full"]).unwrap_err();
        assert_eq!(error.to_string(), "--theme needs a value; pass --theme=full or --theme=compatible");
    }

    #[test]
    fn absent_source_arg_leaves_the_dir_to_the_caller() {
        assert_eq!(parse_source(&[]).unwrap(), None);
        assert_eq!(parse_source(&["--theme=full", "somewhere"]).unwrap(), None);
    }

    #[test]
    fn source_arg_takes_the_dir_after_the_equals() {
        assert_eq!(parse_source(&["--source=/tmp/export"]).unwrap(), Some(PathBuf::from("/tmp/export")));
        // A dir that does not exist still parses: whether it is there is the overview's answer.
        assert_eq!(parse_source(&["--source=./nope"]).unwrap(), Some(PathBuf::from("./nope")));
    }

    #[test]
    fn last_source_arg_wins() {
        assert_eq!(parse_source(&["--source=/tmp/one", "--source=/tmp/two"]).unwrap(), Some(PathBuf::from("/tmp/two")));
    }

    #[test]
    fn source_flag_without_a_value_is_a_hard_error() {
        let error = parse_source(&["--source", "/tmp/export"]).unwrap_err();
        assert_eq!(error.to_string(), "--source needs a value; pass --source=<dir> naming the dir that holds the export's zips");
    }

    #[test]
    fn an_empty_source_value_is_a_hard_error() {
        let error = parse_source(&["--source="]).unwrap_err();
        assert_eq!(error.to_string(), "--source= names no dir; pass --source=<dir> naming the dir that holds the export's zips");
    }

    #[test]
    fn version_flag_wins_among_other_args() {
        assert!(parse_version(&["--version"]).unwrap());
        assert!(parse_version(&["--theme=full", "--source=/tmp/export", "--version"]).unwrap());
        assert!(!parse_version(&[]).unwrap());
        assert!(!parse_version(&["--verbose", "some/path"]).unwrap());
    }

    #[test]
    fn version_flag_with_a_value_is_a_hard_error() {
        let error = parse_version(&["--version=full"]).unwrap_err();
        assert_eq!(error.to_string(), "--version takes no value; pass --version alone");
    }

    #[test]
    fn print_source_flag_is_read_alongside_the_dir_it_reports() {
        assert!(parse_print_source(&["--print-source"]).unwrap());
        assert!(parse_print_source(&["--source=/tmp/export", "--print-source"]).unwrap());
        assert!(!parse_print_source(&[]).unwrap());
        // `--source` on its own is the launch path, not the print one.
        assert!(!parse_print_source(&["--source=/tmp/export"]).unwrap());
    }

    #[test]
    fn print_source_flag_with_a_value_is_a_hard_error() {
        let error = parse_print_source(&["--print-source=/tmp/export"]).unwrap_err();
        assert_eq!(error.to_string(), "--print-source takes no value; pass --print-source alone, with --source=<dir> naming the dir");
    }

    #[test]
    fn help_flag_is_read_in_both_of_its_spellings() {
        assert!(parse_help(&["--help"]).unwrap());
        assert!(parse_help(&["-h"]).unwrap());
        assert!(parse_help(&["--source=/tmp/export", "-h"]).unwrap());
        assert!(!parse_help(&[]).unwrap());
        // Every other single-dash argument is still left alone, `-h=1` included.
        assert!(!parse_help(&["-x", "some/path"]).unwrap());
        assert!(!parse_help(&["-h=1"]).unwrap());
    }

    #[test]
    fn help_flag_with_a_value_is_a_hard_error() {
        let error = parse_help(&["--help=flags"]).unwrap_err();
        assert_eq!(error.to_string(), "--help takes no value; pass --help alone");
    }

    /// The coupling that makes the help text answerable for the parsers: a flag added to
    /// [`KNOWN_FLAGS`] and left out of [`HELP_TEXT`] reds here rather than shipping undocumented.
    #[test]
    fn help_text_names_every_flag_the_binary_parses() {
        for flag in KNOWN_FLAGS {
            assert!(HELP_TEXT.contains(flag), "the help text lacks '{flag}'");
        }
        assert!(HELP_TEXT.contains("-h, --help"), "the help text must name the short spelling too");
        assert!(HELP_TEXT.ends_with('\n'));
        // Decision: no bug or project URL until phase 5 has one to print.
        assert!(!HELP_TEXT.contains("http"), "the help text must not carry an invented url");
    }

    #[test]
    fn an_unknown_double_dash_argument_is_refused_by_name() {
        let error = reject_unknown(&["--print-sourc"]).unwrap_err();
        assert_eq!(error.to_string(), "\"--print-sourc\": unknown flag; run exportsnap --help for the flags it takes");
        // A prefix of a known flag is not a known flag, and neither is a bare `--`.
        assert!(reject_unknown(&["--print"]).is_err());
        assert!(reject_unknown(&["--"]).is_err());
    }

    /// The other half of the refusal: every known spelling has to survive the scan, or a flag's own
    /// error message is replaced by "unknown flag" and the fix it names is lost.
    #[test]
    fn every_known_flag_survives_the_unknown_scan() {
        for flag in KNOWN_FLAGS {
            reject_unknown(&[flag]).unwrap_or_else(|error| panic!("{flag} must reach its own parser: {error}"));
            let valued = format!("{flag}=value");
            reject_unknown(&[&valued]).unwrap_or_else(|error| panic!("{valued} must reach its own parser: {error}"));
        }
    }

    #[test]
    fn a_bare_or_single_dash_argument_is_still_left_alone() {
        // Decision 56c supersedes the leave-it-alone convention for the `--` shape only.
        reject_unknown(&["some/path", "-x", "-", "--theme=full"]).unwrap();
    }

    #[test]
    fn version_text_is_four_lines_headed_by_name_and_version() {
        let lines: Vec<&str> = VERSION_TEXT.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], format!("exportsnap {}", env!("CARGO_PKG_VERSION")));
        assert!(VERSION_TEXT.ends_with('\n'));
    }

    #[test]
    fn version_text_carries_the_osm_credit_and_notices_pointer() {
        for needle in ["OpenStreetMap", "ODbL-1.0", "https://opendatacommons.org/licenses/odbl/1-0/", "tzf-dist", "THIRD-PARTY-LICENSES"] {
            assert!(VERSION_TEXT.contains(needle), "version text lacks '{needle}'");
        }
    }
}
