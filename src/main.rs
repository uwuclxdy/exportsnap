//! Entry point: theme argument, tier detection, terminal bootstrap and teardown.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
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

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

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

/// Hand-parses `--theme=full` / `--theme=compatible`, last one wins. A real CLI with
/// subcommands is phase 5 and brings its own argument parser then; anything else on the
/// command line is left alone until it exists.
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
