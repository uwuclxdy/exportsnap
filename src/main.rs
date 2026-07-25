//! Entry point: theme argument, tier detection, terminal bootstrap and teardown.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{Context, Result, bail};
use exportsnap::app::App;
use exportsnap::tui::theme::{self, Tier};

fn main() -> Result<()> {
    let cli_tier = parse_theme_arg(std::env::args().skip(1))?;
    // The config precedence level has no loader yet; `detect_from_env` still orders it.
    let tier = theme::detect_from_env(cli_tier, None);

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
    let mut app = App::new(tier);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Tier>> {
        parse_theme_arg(args.iter().map(|arg| (*arg).to_string()))
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
}
