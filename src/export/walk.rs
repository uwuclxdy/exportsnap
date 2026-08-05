//! The directory walk both media pipelines run: every file in every dir of one NAME under a root,
//! at any depth.
//!
//! Extracted from [`crate::export::memories::discover`] when [`crate::export::chat_media`] needed
//! the same walk over `chat_media` dirs. The two callers differ in the dir name they look for and
//! the grammar they parse a filename with, and in nothing else — including the symlink rule below,
//! which is the reason this is one function rather than two copies of it.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A directory the walk skipped, and the class of reason.
///
/// [`io::ErrorKind`] rather than the whole [`io::Error`] so a discovery holding these can stay
/// `Clone` and `PartialEq`, which is what lets a test assert two walks agree. The kind is the half
/// that decides what a reader does next: `PermissionDenied` is a dir to leave alone, anything else
/// is worth looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableDir {
    pub dir: PathBuf,
    pub kind: io::ErrorKind,
}

impl fmt::Display for UnreadableDir {
    /// Renders the kind through [`io::Error`], because [`io::ErrorKind`] has no `Display` of its own
    /// and its `Debug` spelling (`PermissionDenied`) is a type name rather than something to put in
    /// front of a reader.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.dir.display(), io::Error::from(self.kind))
    }
}

/// What one walk found: the files a grammar read, the names it did not, and the dirs nobody could
/// list.
#[derive(Debug)]
pub(crate) struct Walk<T> {
    pub files: Vec<T>,
    pub unparsed: Vec<PathBuf>,
    pub unreadable: Vec<UnreadableDir>,
}

/// Every file in every dir named `dir_name` under `root`, at any depth, run through `parse`.
///
/// A directory the walk cannot list is recorded in [`Walk::unreadable`] and the walk carries on.
/// Aborting instead would report zero media for a source root that merely happens to sit on a
/// filesystem with a `lost+found` on it, and reporting the skip answers the same question that an
/// abort was there to answer — which dir, and why — without throwing away everything that was
/// found. `root` keeps the hard error: that one is the caller's own argument, and a run that cannot
/// read it has nothing to report at all.
///
/// # Errors
///
/// Returns the io error from listing `root` itself.
pub(crate) fn walk<T>(root: &Path, dir_name: &str, parse: impl Fn(&Path) -> Option<T>) -> Result<Walk<T>, io::Error> {
    let mut queue = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut unparsed = Vec::new();
    let mut unreadable = Vec::new();

    while let Some(dir) = queue.pop() {
        let inside = dir.file_name().and_then(OsStr::to_str) == Some(dir_name);
        let listing = match fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(source) if dir == root => return Err(source),
            Err(source) => {
                unreadable.push(UnreadableDir { dir, kind: source.kind() });
                continue;
            }
        };
        for entry in listing {
            // An entry that cannot be read mid-listing retires the rest of this dir the same way,
            // rather than the rest of the walk.
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    unreadable.push(UnreadableDir { dir: dir.clone(), kind: source.kind() });
                    break;
                }
            };
            // `DirEntry::file_type` answers about the link rather than its target, so no symlink is
            // ever descended. `zip::discover_parts` can afford `Path::is_dir` because it never
            // recurses; this cannot. With `is_dir` here, a link pointing at its own ancestor is
            // re-entered until the kernel refuses to resolve any more of them, so the walk
            // rediscovers every file below it around forty times over and reports them as
            // duplicates of each other. The bound is `MAXSYMLINKS` (ELOOP at 41 components,
            // measured), not path length — 603 characters against a 4096 `PATH_MAX` — so it
            // terminates in under a millisecond and no timeout can see it. That is exactly why it
            // needs a test rather than a comment. Pinned by
            // `a_symlink_loop_does_not_make_the_walk_re_enter_itself` in both `tests/memories.rs`
            // and `tests/chat_media.rs`.
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(source) => {
                    unreadable.push(UnreadableDir { dir: dir.clone(), kind: source.kind() });
                    break;
                }
            };
            let path = entry.path();
            if kind.is_dir() {
                queue.push(path);
            } else if inside {
                match parse(&path) {
                    Some(file) => files.push(file),
                    None => unparsed.push(path),
                }
            }
        }
    }

    Ok(Walk { files, unparsed, unreadable })
}
