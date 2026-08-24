//! Public-API tests for `exportsnap::config`: the toml loader, the precedence wiring, and the
//! settings screen's write-back. Every test runs against scratch dirs only — the platform
//! config dir is never resolved here: on windows `directories` answers via
//! `SHGetKnownFolderPath`, so an in-process call would read the operator's real tree.
//!
//! The wiring below reaches as far as `detect_from_env` itself; `main`'s call site — the half
//! that feeds the loaded config into the resolver — is pinned by the source witness at the end
//! of this file, the only observation a test crate has of the binary target.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use exportsnap::app::RunDefaults;
use exportsnap::config::{self, Config, ConfigError};
use exportsnap::export::chat_fix::OverlayMode;
use exportsnap::tui::theme::{Tier, detect_from_env};
use tempfile::tempdir;

/// The crate-level allow is scoped here rather than inside `common`: this crate reads the source
/// witness and no other half. See `tests/common/mod.rs` for what that placement keeps measuring.
#[allow(dead_code, reason = "this crate reads the source witness and no other half of the module")]
mod common;

/// Writes `text` as the config file in `dir`, then loads it — the path every happy-path
/// test goes through.
fn load_text(dir: &Path, text: &str) -> Result<Config, ConfigError> {
    fs::write(dir.join("config.toml"), text).unwrap();
    config::load(dir)
}

/// One config with every key set, for the round-trip tests.
fn full_config() -> Config {
    Config {
        theme: Some(Tier::Compatible),
        out_dir: Some("/tmp/fixed".into()),
        ffmpeg_path: Some("/usr/bin/ffmpeg".into()),
        transcode: Some(false),
        overlay_mode: Some(OverlayMode::Originals),
    }
}

#[test]
fn a_missing_file_is_defaults_not_an_error() {
    let dir = tempdir().unwrap();
    assert_eq!(config::load(dir.path()).unwrap(), Config::default());
    // A missing config DIR is the same answer.
    assert_eq!(config::load(&dir.path().join("no-such-subdir")).unwrap(), Config::default());
}

#[test]
fn a_config_naming_a_theme_wins_inside_detect_from_env() {
    let dir = tempdir().unwrap();
    let config = load_text(dir.path(), "[theme]\nname = \"full\"\n").unwrap();
    assert_eq!(config.theme, Some(Tier::Full));
    // Through `detect_from_env` itself, with no flag set: the config's tier must win over
    // whatever `$COLORTERM` says, so the assertion holds under any environment.
    assert_eq!(detect_from_env(None, config.theme), Tier::Full);
}

/// The startup wiring the review filed as unpinned: `main` passes the loaded config's `theme`
/// into `detect_from_env`'s config slot (decision 66: flag > config > detection), which is the
/// call the `--theme` help line's "defaults to the config's theme" claim rests on. A test
/// crate can observe that wiring only as text: `main` is the binary target, so no test can
/// call it, and its call is the one delivery of the resolved tier the app ever reads —
/// `App::new` stages it into the settings layers too, but `start_with` overwrites those before
/// anything reads them. Any tier a test reads elsewhere (a palette of an `App` the test
/// built, the settings row's answer) is one the test composed itself, so re-deriving it pins
/// nothing about `main`. The behavioral
/// channels are closed the same way: no in-repo harness renders the TUI (the tests/flows
/// replay driver is external) and `--print-source` deliberately carries no tier key. The
/// witness reads the call site instead.
///
/// Ceiling, stated rather than assumed: this pins the call's SPELLING, so a semantically
/// equivalent rewrite of the line reds here and wants this witness updated with it, and a doc
/// comment in `src/main.rs` quoting the call verbatim would keep this green while the wiring
/// dropped — none does. What it kills that no behavioral test can is the drop the review named
/// — `detect_from_env(cli_tier, None)` leaves every other test green while the help text's
/// claim lies.
#[test]
fn the_startup_wiring_passes_the_loaded_configs_theme_into_tier_detection() {
    let source = common::main_source::read();

    // The named assertion comes first: it is the one the drop-plant must red.
    let wiring = "theme::detect_from_env(cli_tier, config.theme)";
    assert!(source.contains(wiring), "main must resolve the tier from the loaded config's theme, but {wiring:?} is gone from src/main.rs");

    // The claim that call backs: the help line and the call site can drift apart only if this
    // reds while the wiring assert above stays green.
    assert!(source.contains("defaults to the config's theme"), "the --theme help line must keep claiming the config layer");
}

#[test]
fn a_cli_flag_beats_the_same_key_in_the_config() {
    let dir = tempdir().unwrap();
    let config = load_text(dir.path(), "[theme]\nname = \"compatible\"\n").unwrap();
    assert_eq!(config.theme, Some(Tier::Compatible));
    assert_eq!(detect_from_env(Some(Tier::Full), config.theme), Tier::Full);
}

#[test]
fn a_config_out_dir_is_honoured_by_resolve() {
    let dir = tempdir().unwrap();
    let config = load_text(dir.path(), "out_dir = \"/tmp/fixed\"\n").unwrap();
    assert_eq!(config.out_dir.as_deref(), Some(Path::new("/tmp/fixed")));
    // Through `RunDefaults::resolve` itself, with no flag set: the file's dir must beat the
    // source-derived default.
    let defaults = RunDefaults::resolve(None, &config, Path::new("/export"));
    assert_eq!(defaults.out_root, PathBuf::from("/tmp/fixed"));
}

#[test]
fn a_cli_out_flag_beats_the_same_key_in_the_config() {
    let dir = tempdir().unwrap();
    let config = load_text(dir.path(), "out_dir = \"/tmp/fixed\"\n").unwrap();
    let defaults = RunDefaults::resolve(Some(Path::new("/tmp/flag")), &config, Path::new("/export"));
    assert_eq!(defaults.out_root, PathBuf::from("/tmp/flag"));
}

#[test]
fn the_out_root_defaults_to_the_source_dir_under_exportsnap_out() {
    // The last layer of the precedence: no flag, no file — the source-derived default stands
    // (decision 33).
    let defaults = RunDefaults::resolve(None, &Config::default(), Path::new("/export"));
    assert_eq!(defaults.out_root, PathBuf::from("/export/exportsnap-out"));
}

#[test]
fn a_config_transcode_is_the_memories_default_else_on() {
    let dir = tempdir().unwrap();
    let config = load_text(dir.path(), "transcode = false\n").unwrap();
    assert_eq!(config.transcode, Some(false));
    assert!(!RunDefaults::resolve(None, &config, Path::new("/export")).transcode);
    // Absent, the run starts on — the default the decisions fixed.
    assert!(RunDefaults::resolve(None, &Config::default(), Path::new("/export")).transcode);
}

#[test]
fn a_config_overlay_mode_is_the_chat_default_else_both() {
    let dir = tempdir().unwrap();
    let config = load_text(dir.path(), "overlay_mode = \"merged\"\n").unwrap();
    assert_eq!(config.overlay_mode, Some(OverlayMode::Merged));
    assert_eq!(RunDefaults::resolve(None, &config, Path::new("/export")).overlay_mode, OverlayMode::Merged);
    // Absent, the run starts at `both` — [`OverlayMode::default`].
    assert_eq!(RunDefaults::resolve(None, &Config::default(), Path::new("/export")).overlay_mode, OverlayMode::Both);
}

#[test]
fn a_malformed_theme_value_names_itself_in_the_error() {
    let dir = tempdir().unwrap();
    let error = load_text(dir.path(), "[theme]\nname = \"24bit\"\n").unwrap_err();
    let ConfigError::UnknownTheme { value, .. } = &error else {
        panic!("expected UnknownTheme, got {error}");
    };
    assert_eq!(value, "24bit");
    // The message names the key, the bad value, and the fix.
    let message = error.to_string();
    assert!(message.contains("[theme] name"), "{message}");
    assert!(message.contains("\"24bit\""), "{message}");
    assert!(message.contains("full"), "{message}");
}

#[test]
fn a_wrong_typed_value_names_the_key_and_what_was_expected() {
    let dir = tempdir().unwrap();
    let error = load_text(dir.path(), "transcode = \"yes\"\n").unwrap_err();
    let ConfigError::WrongType { key, expected, got, .. } = &error else {
        panic!("expected WrongType, got {error}");
    };
    assert_eq!(key, "transcode");
    assert_eq!(expected, &"true or false");
    assert_eq!(got, &"a string");
}

#[test]
fn an_unknown_key_is_refused_by_name_rather_than_ignored() {
    let dir = tempdir().unwrap();
    let error = load_text(dir.path(), "them = \"full\"\n").unwrap_err();
    let ConfigError::UnknownKey { key, .. } = &error else {
        panic!("expected UnknownKey, got {error}");
    };
    assert_eq!(key, "them");
    // The same refusal inside [theme].
    let error = load_text(dir.path(), "[theme]\nname = \"full\"\nnmae = \"compatible\"\n").unwrap_err();
    let ConfigError::UnknownThemeKey { key, .. } = &error else {
        panic!("expected UnknownThemeKey, got {error}");
    };
    assert_eq!(key, "nmae");
}

#[test]
fn text_that_is_not_toml_is_a_named_parse_error() {
    let dir = tempdir().unwrap();
    let error = load_text(dir.path(), "= not toml").unwrap_err();
    assert!(matches!(error, ConfigError::Toml { .. }), "{error}");
}

#[test]
fn a_write_back_reloads_to_the_same_config_with_every_key_set() {
    let dir = tempdir().unwrap();
    let config = full_config();
    config::write(dir.path(), &config).unwrap();
    assert_eq!(config::load(dir.path()).unwrap(), config);
    // `write` stages through the temp name beside the file and renames it away.
    assert!(!dir.path().join("config.toml.tmp").exists());
}

#[test]
fn a_defaulted_config_round_trips_too() {
    let dir = tempdir().unwrap();
    config::write(dir.path(), &Config::default()).unwrap();
    assert_eq!(config::load(dir.path()).unwrap(), Config::default());
}

#[test]
fn writing_a_config_with_an_empty_out_dir_is_refused_with_empty_path() {
    let dir = tempdir().unwrap();
    let config = Config { out_dir: Some(PathBuf::new()), ..Config::default() };
    let error = config::write(dir.path(), &config).unwrap_err();
    let ConfigError::EmptyPath { key, .. } = &error else {
        panic!("expected EmptyPath, got {error}");
    };
    assert_eq!(key, &"out_dir");
    assert!(error.to_string().contains("names no path"), "{error}");
    // Refused before anything landed on disk.
    assert!(!dir.path().join("config.toml").exists());
}

#[test]
fn an_empty_path_key_is_refused_by_name_on_load() {
    let dir = tempdir().unwrap();
    let error = load_text(dir.path(), "out_dir = \"\"\n").unwrap_err();
    let ConfigError::EmptyPath { key, .. } = &error else {
        panic!("expected EmptyPath, got {error}");
    };
    assert_eq!(key, &"out_dir");
    assert!(error.to_string().contains("names no path"), "{error}");
}

#[test]
fn a_document_whose_root_is_not_a_table_is_a_toml_parse_error() {
    // `toml::from_str` parses a document, and a toml document's root is a table or a parse
    // error. `[1, 2]` dies at the parse step, so `Toml` is the variant that names it; `load`
    // deserializes the root straight into a `toml::Table` and holds no runtime root check.
    let dir = tempdir().unwrap();
    let error = load_text(dir.path(), "[1, 2]\n").unwrap_err();
    let ConfigError::Toml { .. } = &error else {
        panic!("expected Toml, got {error}");
    };
    assert!(error.to_string().contains("not valid toml"), "{error}");
}

#[test]
fn a_theme_table_without_a_name_is_a_named_missing_key() {
    let dir = tempdir().unwrap();
    let error = load_text(dir.path(), "[theme]\n").unwrap_err();
    let ConfigError::MissingKey { key, expected, .. } = &error else {
        panic!("expected MissingKey, got {error}");
    };
    assert_eq!(key, &"theme.name");
    assert_eq!(expected, &"\"full\" or \"compatible\"");
    assert!(error.to_string().contains("missing"), "{error}");
}

#[test]
fn a_config_file_that_is_not_utf8_is_a_named_read_error() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("config.toml"), [0xff, 0xfe, 0x00]).unwrap();
    let error = config::load(dir.path()).unwrap_err();
    let ConfigError::Read { .. } = &error else {
        panic!("expected Read, got {error}");
    };
    // The fix names utf-8 rather than the raw bytes.
    assert!(error.to_string().contains("valid utf-8"), "{error}");
}

/// Unix-only, and the gate is the ASSERTION rather than the fixture: mode bits are a unix
/// concept. On windows this build sets no ACL and the file inherits the per-user config dir's
/// ACL — the same posture `manifest::reserve_private` documents — so there are no mode bits
/// to assert and the test would red for a posture that does not exist there.
#[cfg(unix)]
#[test]
fn the_written_config_is_owner_only_0600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    config::write(dir.path(), &full_config()).unwrap();
    let mode = fs::metadata(dir.path().join("config.toml")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
