//! What the machine running exportsnap can do for the pipeline: which optional external tools are
//! installed, and how much room is left where the export lands.
//!
//! Framework-free like the rest of `export/`: nothing here knows a screen exists. The two probes
//! also fail independently — a filesystem that cannot be measured says nothing about whether
//! ffmpeg is installed — so neither can take the other down.

use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// An external tool the pipeline uses when it is there and degrades without (`docs/design.md`,
/// decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    /// Video overlay burn-in, HEVC transcode, video metadata.
    Ffmpeg,
    /// Playback of a repaired video.
    Vlc,
}

impl Tool {
    /// Report order.
    pub const ALL: [Self; 2] = [Self::Ffmpeg, Self::Vlc];

    /// The executable name, which doubles as the label a screen shows for it.
    #[must_use]
    pub const fn command(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Vlc => "vlc",
        }
    }
}

/// Where `tool` sits on `PATH`, or `None` when it is not there.
///
/// Resolved through `which` rather than by spawning the tool: on Windows `CreateProcess` appends
/// only `.exe` and never consults `PATHEXT`, so a `.cmd` or `.ps1` shim the shell runs fine is
/// invisible to a spawn probe. `which` 8.0.5 does read `PATHEXT` (`sys.rs:157-180` feeding
/// `helper::has_executable_extension`), which is the whole reason to depend on it.
///
/// `which_global` rather than `which` states the intent that the working dir is never searched,
/// but for these names it changes nothing and is not load-bearing: `which` consults a cwd only
/// when the queried name `has_separator()` (`finder.rs:83` — a bare name falls to the
/// PATH-only arm), and every [`Tool::command`] is a bare name. Do not write a comment claiming
/// this guards against an `ffmpeg` sitting in the export dir; it never could, because that lookup
/// never touches the working dir in the first place.
#[must_use]
pub fn locate(tool: Tool) -> Option<PathBuf> {
    which::which_global(tool.command()).ok()
}

/// [`locate`] against an explicit `PATH`-style list instead of the process environment.
///
/// The seam exists because `locate` cannot be pinned otherwise: pointing it at a known dir needs
/// `PATH` changed, and `std::env::set_var` is process-global and `unsafe`, which this crate
/// forbids. A test drives this to pin that each [`Tool`] looks up its own [`Tool::command`] and
/// that the `which` integration still resolves at all across a version bump.
#[must_use]
pub fn locate_in(tool: Tool, path_list: impl AsRef<OsStr>) -> Option<PathBuf> {
    which::WhichConfig::new()
        .binary_name(tool.command().into())
        .custom_path_list(path_list.as_ref().to_os_string())
        // Mirrors `which_global`'s posture. Inert for a bare name, per the note on `locate`.
        .system_cwd(false)
        .first_result()
        .ok()
}

/// The filesystem holding a path could not be measured.
#[derive(Debug)]
pub struct SpaceError {
    /// The path that was being measured.
    pub path: PathBuf,
    /// What the filesystem said.
    pub source: io::Error,
}

impl fmt::Display for SpaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not measure free space on the filesystem holding {}: {}; check the path exists and is readable",
            self.path.display(),
            self.source
        )
    }
}

impl Error for SpaceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Bytes a non-privileged user can still write to the filesystem holding `path`.
///
/// `available_space`, not `free_space`: the two differ by the root-reserved blocks, which the user
/// running this cannot spend. Counting those on the one screen that exists to say whether an
/// export will fit would overpromise by exactly the amount that matters on a nearly full disk.
///
/// # Errors
///
/// Returns [`SpaceError`] when the path cannot be stat'd.
pub fn available_space(path: impl AsRef<Path>) -> Result<u64, SpaceError> {
    let path = path.as_ref();
    fs4::available_space(path).map_err(|source| SpaceError { path: path.to_path_buf(), source })
}

/// The whole size of the filesystem holding `path`, so a caller can turn [`available_space`] into
/// the used share the usage-role bar shows. Same failure shape, same reason.
///
/// # Errors
///
/// Returns [`SpaceError`] when the path cannot be stat'd.
pub fn total_space(path: impl AsRef<Path>) -> Result<u64, SpaceError> {
    let path = path.as_ref();
    fs4::total_space(path).map_err(|source| SpaceError { path: path.to_path_buf(), source })
}

/// One probe of everything the overview's environment panel reports.
///
/// A plain snapshot with public fields, so a caller that already knows the answers — a render test
/// pinning ffmpeg-present against ffmpeg-absent — can build one without reaching for the real
/// `PATH`. [`Self::default`] is "nothing found, nothing measured".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// Where `ffmpeg` was found, or `None` when it is not on `PATH`.
    pub ffmpeg: Option<PathBuf>,
    /// Where `vlc` was found, or `None` when it is not on `PATH`.
    pub vlc: Option<PathBuf>,
    /// Bytes available where the export lands, or `None` when the filesystem could not be
    /// measured. The reason is dropped here on purpose: a panel row has one value slot to say it
    /// in, and [`available_space`] still carries the whole [`SpaceError`] for a caller that can act
    /// on one.
    pub available_space: Option<u64>,
    /// The filesystem's total size, so a usage bar can show the free half as a share of the whole.
    /// `None` when the filesystem could not be measured, exactly like `available_space`.
    pub total_space: Option<u64>,
}

/// The nearest existing ancestor of `path` — the filesystem a `statvfs` probe can actually measure.
///
/// An output root is created only at the first write, so a run pointed at a brand-new dir must not
/// report "unknown" disk free. Lives here rather than beside a screen because both media screens ask
/// it and the question is about the filesystem, not about a widget.
#[must_use]
pub fn probe_target(path: impl AsRef<Path>) -> PathBuf {
    let mut candidate = path.as_ref().to_path_buf();
    while !candidate.exists() {
        match candidate.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => candidate = parent.to_path_buf(),
            _ => break,
        }
    }
    candidate
}

impl Environment {
    /// Probes `PATH` for every [`Tool::ALL`] and measures the filesystem holding `path`.
    #[must_use]
    pub fn probe(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        Self::from_probes(locate, available_space(path).ok(), total_space(path).ok())
    }

    /// The field wiring, split from the probes so a unit test can hand in a locator that answers
    /// differently per [`Tool`]. Without that seam a swap here (`ffmpeg: locate(Tool::Vlc)`) is
    /// invisible on any machine where the two tools agree, which includes every machine with
    /// neither installed.
    fn from_probes(locate: impl Fn(Tool) -> Option<PathBuf>, available_space: Option<u64>, total_space: Option<u64>) -> Self {
        Self { ffmpeg: locate(Tool::Ffmpeg), vlc: locate(Tool::Vlc), available_space, total_space }
    }

    /// Where `tool` was found.
    #[must_use]
    pub fn tool(&self, tool: Tool) -> Option<&Path> {
        match tool {
            Tool::Ffmpeg => self.ffmpeg.as_deref(),
            Tool::Vlc => self.vlc.as_deref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_field_holds_the_tool_it_is_named_after() {
        // A locator that answers differently per tool, which the real `PATH` cannot be made to do
        // from a test. This is what makes a swapped field in `from_probes` observable.
        let environment = Environment::from_probes(|tool| Some(PathBuf::from(tool.command())), Some(7), Some(14));

        assert_eq!(environment.ffmpeg, Some(PathBuf::from("ffmpeg")));
        assert_eq!(environment.vlc, Some(PathBuf::from("vlc")));
        assert_eq!(environment.tool(Tool::Ffmpeg), Some(Path::new("ffmpeg")));
        assert_eq!(environment.tool(Tool::Vlc), Some(Path::new("vlc")));
        assert_eq!(environment.available_space, Some(7));
    }

    #[test]
    fn a_locator_that_finds_nothing_leaves_the_space_figure_alone() {
        let environment = Environment::from_probes(|_| None, Some(42), None);

        assert_eq!(environment.ffmpeg, None);
        assert_eq!(environment.vlc, None);
        assert_eq!(environment.available_space, Some(42));
    }
}
