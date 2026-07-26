//! Entry point: theme argument, tier detection, terminal bootstrap and teardown.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use exportsnap::app::App;
use exportsnap::tui::screens::overview::Overview;
use exportsnap::tui::theme::{self, Tier};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let cli_tier = parse_theme_arg(args.iter().cloned())?;
    // The config precedence level has no loader yet; `detect_from_env` still orders it.
    let tier = theme::detect_from_env(cli_tier, None);

    // The dir the user points at, or the one they ran from. Read before the terminal is taken
    // over, so a bad argument is a plain message on a plain terminal rather than a flash of
    // alternate screen. The read itself never fails: an absent or unreadable export is a state the
    // overview has words for.
    //
    // Deliberate ceiling: this is blocking, so a large `json/` delays the first frame with nothing
    // on screen to say why. The upgrade path is the phase-2 tokio runtime plus the overview's own
    // loading state, which needs the tick timer no screen has earned yet.
    let source = match parse_source_arg(args)? {
        Some(dir) => dir,
        None => std::env::current_dir().context("could not read the working dir; pass --source=<dir> instead")?,
    };
    let overview = Overview::load(&source);

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
    let mut app = App::new(tier).with_overview(overview);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Tier>> {
        parse_theme_arg(args.iter().map(|arg| (*arg).to_string()))
    }

    fn parse_source(args: &[&str]) -> Result<Option<PathBuf>> {
        parse_source_arg(args.iter().map(|arg| (*arg).to_string()))
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
}
