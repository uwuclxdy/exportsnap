//! The config file: per-user settings the settings screen (phase 5, task §6) writes and every
//! run reads at startup.
//!
//! # Shape
//!
//! `config.toml` in the platform config dir ([`config_dir`]; `~/.config/exportsnap` on linux),
//! every key optional:
//!
//! ```toml
//! [theme]
//! name = "full"              # "full" | "compatible"
//! out_dir = "/path"          # where a run writes; beats the --source-derived default
//! ffmpeg_path = "/usr/bin/ffmpeg"
//! transcode = true
//! overlay_mode = "both"      # "merged" | "both" | "originals"
//! ```
//!
//! The key set is exactly what task §6's settings rows consume — output dir, theme, ffmpeg
//! path, and the two run defaults (transcode, overlay mode) — and nothing more. `[theme] name`
//! is the only key a consumer reads today (decision 66 wires it into the tier resolver); the
//! rest is the shape the settings screen's write-back rounds through.
//!
//! # Contract
//!
//! - A **missing file is not an error**: no config means defaults, and [`load`] returns
//!   [`Config::default()`] with every key `None`.
//! - A **malformed file is a [`ConfigError`] naming the key and what was expected** — never a
//!   panic, never a silent fallback to a value the user didn't ask for.
//! - **Unknown keys are refused, not ignored** — deliberately. A misspelled key
//!   (`them = "full"`) silently dropping the user's setting is a wrong answer at exit 0, the
//!   exact failure decision 57 refused dash-led arguments for, and a refused key is one
//!   keystroke from fixed. The settings screen only ever writes keys this module defines, so
//!   a self-written file never trips it.
//! - [`write`] never truncates the file in place: it stages a temp beside it — at `0600`
//!   before the temp exists, the crate's token posture, since the file names personal paths —
//!   then renames over. Mode bits are unix-only; windows sets no ACL here and the file
//!   inherits the per-user config dir's ACL (the split `manifest::reserve_private` documents).
//!
//! # Precedence
//!
//! flag > config > env is implemented by `tui::theme::detect`, which the file's theme value
//! feeds (decision 66). [`load`] and [`write`] take the config dir as a **parameter** and
//! never resolve it themselves: on windows `directories` answers via `SHGetKnownFolderPath`,
//! so a test calling the resolver in-process would read the operator's real tree. The binary
//! resolves it once via [`config_dir`]; tests pass scratch dirs.

use std::error::Error as StdError;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::export::chat_fix::OverlayMode;
use crate::export::manifest::{APPLICATION, ORGANIZATION, QUALIFIER, create_private_dir};
use crate::tui::theme::Tier;

/// The file name in the config dir.
const FILE_NAME: &str = "config.toml";

/// The temp [`write`] stages through beside the final file, so a crash mid-write leaves either
/// the old file or this — never a truncated config.
const TEMP_NAME: &str = "config.toml.tmp";

/// What the config file says. `None` means the key is absent from the file and the consumer's
/// default rules; the defaults are the consumers' to own, not this module's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// `[theme] name` — the tier override feeding the `flag > config > env` resolver.
    pub theme: Option<Tier>,
    /// `out_dir` — where a run writes, beating the `--source`-derived default.
    pub out_dir: Option<PathBuf>,
    /// `ffmpeg_path` — the ffmpeg binary, beating location detection.
    pub ffmpeg_path: Option<PathBuf>,
    /// `transcode` — the memories run's transcode default (on when absent).
    pub transcode: Option<bool>,
    /// `overlay_mode` — the chat leg's overlay mode ([`OverlayMode::Both`] when absent).
    pub overlay_mode: Option<OverlayMode>,
}

/// A config file that exists but cannot be read as one. Each variant names the key, the bad
/// value, and the fix; a missing file is no error at all (see the module doc).
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read as text (permissions, or bytes that are not valid utf-8 — a
    /// toml document has to be text).
    Read { path: PathBuf, source: io::Error },

    /// The file is not valid toml at all (syntax, duplicate keys, ...).
    Toml { path: PathBuf, source: toml::de::Error },

    /// A key this module does not define — refused rather than ignored, see the module doc.
    UnknownKey { path: PathBuf, key: String },

    /// A key inside `[theme]` that is not `name`.
    UnknownThemeKey { path: PathBuf, key: String },

    /// A key the shape requires that the file left out.
    MissingKey { path: PathBuf, key: &'static str, expected: &'static str },

    /// A key of the right name holding the wrong toml type.
    WrongType { path: PathBuf, key: String, expected: &'static str, got: &'static str },

    /// `[theme] name` spelled a word that maps to no tier.
    UnknownTheme { path: PathBuf, value: String },

    /// `overlay_mode` spelled a word that maps to no mode.
    UnknownOverlayMode { path: PathBuf, value: String },

    /// A path key set to the empty string.
    EmptyPath { path: PathBuf, key: &'static str },

    /// A path this build cannot spell in toml (write side).
    PathNotUtf8 { path: PathBuf, key: &'static str, value: PathBuf },

    /// Serializing the settings failed.
    Serialize { path: PathBuf, source: toml::ser::Error },

    /// Any io step of the write-back: dir creation, temp write, rename.
    Write { path: PathBuf, source: io::Error },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(f, "could not read config {}: {source}; the file must be valid utf-8 toml, or absent", path.display())
            }
            Self::Toml { path, source } => {
                write!(f, "config {} is not valid toml: {source}", path.display())
            }
            Self::UnknownKey { path, key } => write!(
                f,
                "config {}: unknown key {key:?}; expected one of [theme], out_dir, ffmpeg_path, transcode, overlay_mode",
                path.display()
            ),
            Self::UnknownThemeKey { path, key } => {
                write!(f, "config {}: unknown key {key:?} in [theme]; expected `name`", path.display())
            }
            Self::MissingKey { path, key, expected } => {
                write!(f, "config {}: {key} is missing; expected {expected}", path.display())
            }
            Self::WrongType { path, key, expected, got } => {
                write!(f, "config {}: {key} must be {expected}; got {got}", path.display())
            }
            Self::UnknownTheme { path, value } => {
                write!(f, "config {}: [theme] name = {value:?} is not a theme; pass \"full\" or \"compatible\"", path.display())
            }
            Self::UnknownOverlayMode { path, value } => write!(
                f,
                "config {}: overlay_mode = {value:?} is not an overlay mode; pass one of \"merged\", \"both\", \"originals\"",
                path.display()
            ),
            Self::EmptyPath { path, key } => {
                write!(f, "config {}: {key} names no path; set it to a path or drop the key", path.display())
            }
            Self::PathNotUtf8 { path, key, value } => write!(
                f,
                "could not write config {}: {key} names a path that is not valid utf-8 ({value:?}), which no toml string can hold; rename it",
                path.display()
            ),
            Self::Serialize { path, source } => {
                write!(f, "could not serialize config {}: {source}", path.display())
            }
            Self::Write { path, source } => {
                write!(f, "could not write config {}: {source}", path.display())
            }
        }
    }
}

impl StdError for ConfigError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Toml { source, .. } => Some(source),
            Self::Serialize { source, .. } => Some(source),
            Self::Write { source, .. } => Some(source),
            Self::UnknownKey { .. }
            | Self::UnknownThemeKey { .. }
            | Self::MissingKey { .. }
            | Self::WrongType { .. }
            | Self::UnknownTheme { .. }
            | Self::UnknownOverlayMode { .. }
            | Self::EmptyPath { .. }
            | Self::PathNotUtf8 { .. } => None,
        }
    }
}

/// The platform config dir (`directories` 6: `~/.config/exportsnap` on linux, roaming AppData
/// on windows, `~/Library/Application Support` on macos), or `None` when no home dir can be
/// resolved — in which case there is no config file and defaults rule.
///
/// Kept separate from [`load`] and [`write`], which take the dir as a parameter: the binary
/// calls this once at startup, and tests pass scratch dirs so the windows leg never resolves
/// the operator's real tree in-process.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION).map(|dirs| dirs.config_dir().to_path_buf())
}

/// Loads `config.toml` from `config_dir`. A missing file or dir returns
/// [`Config::default()`]; anything else unreadable or malformed is a [`ConfigError`].
pub fn load(config_dir: &Path) -> Result<Config, ConfigError> {
    let path = config_dir.join(FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(source) => return Err(ConfigError::Read { path, source }),
    };
    let root = toml::from_str::<toml::Table>(&text).map_err(|source| ConfigError::Toml { path: path.clone(), source })?;
    parse_root(&path, root)
}

/// Writes `config` as `config.toml` in `config_dir`, creating the dir owner-only where the
/// platform has modes. Never truncates in place: the file is staged at [`TEMP_NAME`] beside it
/// with the final mode set before the rename, so a crash leaves the old file, not a half one.
pub fn write(config_dir: &Path, config: &Config) -> Result<(), ConfigError> {
    let path = config_dir.join(FILE_NAME);
    create_private_dir(config_dir).map_err(|source| ConfigError::Write { path: path.clone(), source })?;

    let text = serialize(&path, config)?;

    let tmp = config_dir.join(TEMP_NAME);
    // A stale temp from a crashed write is never wanted: it holds an unfinished file, and
    // leaving it would fail the `create_new` below on every later write.
    //
    // This removal is the one step that assumes a SINGLE writer — the settings screen of one
    // running instance. Two concurrent instances racing a write lose one write with a named
    // error: A's remove deletes B's live temp and B's rename fails NotFound. Nothing is ever
    // corrupted, because `create_new` + rename make every interleaving loud. If a second
    // writer ever exists, this needs a lock before the remove.
    if let Err(source) = fs::remove_file(&tmp)
        && source.kind() != io::ErrorKind::NotFound
    {
        return Err(ConfigError::Write { path: path.clone(), source });
    }
    write_private(&tmp, &text).map_err(|source| ConfigError::Write { path: path.clone(), source })?;
    fs::rename(&tmp, &path).map_err(|source| ConfigError::Write { path, source })
}

/// The serialized form of `config`, every `Some` key written and every `None` omitted — the
/// round trip `load(write(x)) == x` holds for every `x` `write` accepts, because an omitted
/// key reads back as its field's `None` and an empty path is refused here exactly as `load`
/// refuses one.
fn serialize(path: &Path, config: &Config) -> Result<String, ConfigError> {
    let mut root = toml::Table::new();
    if let Some(tier) = config.theme {
        let mut theme = toml::Table::new();
        theme.insert("name".into(), toml::Value::String(tier.as_name().into()));
        root.insert("theme".into(), toml::Value::Table(theme));
    }
    if let Some(out) = &config.out_dir {
        root.insert("out_dir".into(), toml::Value::String(path_text(path, "out_dir", out)?));
    }
    if let Some(ffmpeg) = &config.ffmpeg_path {
        root.insert("ffmpeg_path".into(), toml::Value::String(path_text(path, "ffmpeg_path", ffmpeg)?));
    }
    if let Some(transcode) = config.transcode {
        root.insert("transcode".into(), toml::Value::Boolean(transcode));
    }
    if let Some(mode) = config.overlay_mode {
        root.insert("overlay_mode".into(), toml::Value::String(mode.as_word().into()));
    }
    toml::to_string(&root).map_err(|source| ConfigError::Serialize { path: path.to_path_buf(), source })
}

/// A path spelled as a toml string. An empty path is refused with the same
/// [`ConfigError::EmptyPath`] `load` uses, or `write` would emit a file `load` then rejects
/// and the round trip would not hold for what `write` accepted. Non-utf-8 paths are refused
/// rather than lossily spelled, since the lossy form would round-trip to a different path
/// than the user set.
fn path_text(path: &Path, key: &'static str, value: &Path) -> Result<String, ConfigError> {
    if value.as_os_str().is_empty() {
        return Err(ConfigError::EmptyPath { path: path.to_path_buf(), key });
    }
    value.to_str().map(str::to_owned).ok_or_else(|| ConfigError::PathNotUtf8 { path: path.to_path_buf(), key, value: value.to_path_buf() })
}

/// Puts the temp on disk at `0600` before it exists, the crate's token posture (mirroring
/// `manifest::reserve_private`): a temp created under the default umask carries `0644` through
/// the rename and leaves the config world-readable — the file names personal paths. Mode bits
/// are unix-only; on windows this sets no ACL and the file inherits the per-user config dir's.
fn write_private(tmp: &Path, text: &str) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// The root is a [`toml::Table`] by construction: `toml::from_str` parses a document, and a
/// toml document's root is a table or a parse error. There is no runtime root check because
/// there is no non-table value for one to refuse.
fn parse_root(path: &Path, root: toml::Table) -> Result<Config, ConfigError> {
    let mut config = Config::default();
    for (key, value) in root {
        match key.as_str() {
            "theme" => config.theme = Some(parse_theme(path, value)?),
            "out_dir" => config.out_dir = Some(parse_path(path, "out_dir", value)?),
            "ffmpeg_path" => config.ffmpeg_path = Some(parse_path(path, "ffmpeg_path", value)?),
            "transcode" => config.transcode = Some(parse_bool(path, "transcode", value)?),
            "overlay_mode" => config.overlay_mode = Some(parse_overlay_mode(path, value)?),
            other => {
                return Err(ConfigError::UnknownKey { path: path.to_path_buf(), key: other.to_owned() });
            }
        }
    }
    Ok(config)
}

fn parse_theme(path: &Path, value: toml::Value) -> Result<Tier, ConfigError> {
    let toml::Value::Table(table) = value else {
        return Err(ConfigError::WrongType { path: path.to_path_buf(), key: "theme".into(), expected: "a table", got: type_name(&value) });
    };
    let mut name = None;
    for (key, value) in table {
        match key.as_str() {
            "name" => {
                let toml::Value::String(spelling) = value else {
                    return Err(ConfigError::WrongType {
                        path: path.to_path_buf(),
                        key: "theme.name".into(),
                        expected: "a string",
                        got: type_name(&value),
                    });
                };
                name = Some(
                    Tier::from_name(&spelling).ok_or_else(|| ConfigError::UnknownTheme { path: path.to_path_buf(), value: spelling })?,
                );
            }
            other => {
                return Err(ConfigError::UnknownThemeKey { path: path.to_path_buf(), key: other.to_owned() });
            }
        }
    }
    name.ok_or_else(|| ConfigError::MissingKey { path: path.to_path_buf(), key: "theme.name", expected: "\"full\" or \"compatible\"" })
}

fn parse_path(path: &Path, key: &'static str, value: toml::Value) -> Result<PathBuf, ConfigError> {
    let toml::Value::String(text) = value else {
        return Err(ConfigError::WrongType {
            path: path.to_path_buf(),
            key: key.into(),
            expected: "a string path",
            got: type_name(&value),
        });
    };
    if text.is_empty() {
        return Err(ConfigError::EmptyPath { path: path.to_path_buf(), key });
    }
    Ok(PathBuf::from(text))
}

fn parse_bool(path: &Path, key: &'static str, value: toml::Value) -> Result<bool, ConfigError> {
    let toml::Value::Boolean(flag) = value else {
        return Err(ConfigError::WrongType {
            path: path.to_path_buf(),
            key: key.into(),
            expected: "true or false",
            got: type_name(&value),
        });
    };
    Ok(flag)
}

fn parse_overlay_mode(path: &Path, value: toml::Value) -> Result<OverlayMode, ConfigError> {
    let toml::Value::String(word) = value else {
        return Err(ConfigError::WrongType {
            path: path.to_path_buf(),
            key: "overlay_mode".into(),
            expected: "a string",
            got: type_name(&value),
        });
    };
    OverlayMode::ALL
        .iter()
        .copied()
        .find(|mode| mode.as_word() == word)
        .ok_or_else(|| ConfigError::UnknownOverlayMode { path: path.to_path_buf(), value: word })
}

/// The toml type a value is, for a [`ConfigError::WrongType`] message.
fn type_name(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "an integer",
        toml::Value::Float(_) => "a float",
        toml::Value::Boolean(_) => "a boolean",
        toml::Value::Datetime(_) => "a datetime",
        toml::Value::Array(_) => "an array",
        toml::Value::Table(_) => "a table",
    }
}
