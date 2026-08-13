//! Public-API tests for `exportsnap::config`: the toml loader, the precedence wiring, and the
//! settings screen's write-back. Every test runs against scratch dirs only — the platform
//! config dir is never resolved here: on windows `directories` answers via
//! `SHGetKnownFolderPath`, so an in-process call would read the operator's real tree.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use exportsnap::config::{self, Config, ConfigError};
use exportsnap::export::chat_fix::OverlayMode;
use exportsnap::tui::theme::{Tier, detect_from_env};
use tempfile::tempdir;

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
fn a_config_naming_a_theme_is_honoured_through_the_real_wiring() {
    let dir = tempdir().unwrap();
    let config = load_text(dir.path(), "[theme]\nname = \"full\"\n").unwrap();
    assert_eq!(config.theme, Some(Tier::Full));
    // Through `detect_from_env` itself, with no flag set: the config's tier must win over
    // whatever `$COLORTERM` says, so the assertion holds under any environment.
    assert_eq!(detect_from_env(None, config.theme), Tier::Full);
}

#[test]
fn a_cli_flag_beats_the_same_key_in_the_config() {
    let dir = tempdir().unwrap();
    let config = load_text(dir.path(), "[theme]\nname = \"compatible\"\n").unwrap();
    assert_eq!(config.theme, Some(Tier::Compatible));
    assert_eq!(detect_from_env(Some(Tier::Full), config.theme), Tier::Full);
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
