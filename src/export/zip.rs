//! The zips a Snapchat "My Data" export arrives as: which parts a source dir holds, which of them
//! are already unpacked, and getting one onto disk without redoing an interrupted run's work.
//!
//! Nothing here picks a path. The caller names the source dir and the extraction destination, so
//! this module never reads config and never invents an output location.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ::zip::CompressionMethod;

/// The prefix every delivered part carries, zip and extracted dir alike.
const PART_PREFIX: &str = "mydata~";

/// Snapchat's per-part naming: `mydata~<id>` is part 1, `mydata~<id>-<n>` is part n.
///
/// The suffix sits on the stem rather than after `.zip`, and the delivered zips and the dirs they
/// unpack into share the shape, so both go through here instead of each caller re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PartName {
    /// The export id every part of one delivery shares.
    pub id: String,
    /// 1-based part number; the suffix-less name is part 1.
    pub number: u32,
}

impl PartName {
    /// Parses a zip's file stem or an extracted dir's name.
    ///
    /// `None` for anything not shaped like a part, which is what keeps a source dir holding
    /// unrelated files usable.
    ///
    /// # Examples
    ///
    /// ```
    /// use exportsnap::export::zip::PartName;
    ///
    /// let first = PartName::parse("mydata~1784667002819").unwrap();
    /// assert_eq!((first.id.as_str(), first.number), ("1784667002819", 1));
    ///
    /// let third = PartName::parse("mydata~1784667002819-3").unwrap();
    /// assert_eq!((third.id.as_str(), third.number), ("1784667002819", 3));
    ///
    /// assert!(PartName::parse("holiday-photos").is_none());
    /// ```
    #[must_use]
    pub fn parse(stem: &str) -> Option<Self> {
        let rest = stem.strip_prefix(PART_PREFIX)?;
        // The id is opaque, so a trailing dash group is only a part number when it reads as one.
        // Part 1 has no suffix, which is why a `-1` tail stays part of the id rather than becoming
        // a second spelling of the first part. `parse` alone accepts `+2` and `02`, which would let
        // two files claim the same part number without anything saying so, hence the digit check.
        if let Some((id, suffix)) = rest.rsplit_once('-')
            && suffix.bytes().all(|byte| byte.is_ascii_digit())
            && !suffix.starts_with('0')
            && let Ok(number) = suffix.parse::<u32>()
            && number >= 2
        {
            // A part suffix with nothing in front of it names no delivery, so it is not a part.
            return (!id.is_empty()).then(|| Self { id: id.to_owned(), number });
        }
        if rest.is_empty() { None } else { Some(Self { id: rest.to_owned(), number: 1 }) }
    }
}

/// A delivered zip part sitting in the source dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipPart {
    /// 1-based part number.
    pub number: u32,
    /// The zip's path.
    pub path: PathBuf,
}

/// A part already unpacked in the source dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPart {
    /// 1-based part number.
    pub number: u32,
    /// The extracted dir.
    pub path: PathBuf,
    /// The `json/` dir this part contributes, ready for `ExportJson::load_dir`. Only the first
    /// part carried one in the one export observed, so `None` is the normal answer for the rest.
    pub json_dir: Option<PathBuf>,
}

/// Every part sharing one export id: what is still zipped, and what is already unpacked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartGroup {
    /// The export id shared by every part below.
    pub id: String,
    /// Delivered zips, ordered by part number.
    pub zips: Vec<ZipPart>,
    /// Unpacked dirs, ordered by part number.
    pub extracted: Vec<ExtractedPart>,
}

impl PartGroup {
    /// Part numbers below the highest one seen that the source dir holds neither zipped nor
    /// unpacked.
    ///
    /// A non-empty answer means the delivery is incomplete: a part was moved, deleted, or never
    /// downloaded. It cannot see a part missing off the END of the delivery, because nothing in a
    /// part's name says how many parts there are.
    #[must_use]
    pub fn missing_parts(&self) -> Vec<u32> {
        let numbers: Vec<u32> = self.zips.iter().map(|p| p.number).chain(self.extracted.iter().map(|p| p.number)).collect();
        let highest = numbers.iter().copied().max().unwrap_or(0);
        (1..=highest).filter(|n| !numbers.contains(n)).collect()
    }
}

/// The source dir could not be listed.
#[derive(Debug)]
pub struct DiscoverError {
    /// The dir that was being listed.
    pub dir: PathBuf,
    /// What the filesystem said.
    pub source: io::Error,
}

impl fmt::Display for DiscoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not list {} looking for mydata~ export parts: {}; point the source at the dir holding the export's zips",
            self.dir.display(),
            self.source
        )
    }
}

impl Error for DiscoverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Everything in `source_dir` shaped like a part of a Snapchat export, grouped by export id.
///
/// Groups come back ordered by id and their parts by number. A dir holding two deliveries reports
/// two groups rather than one merged one, and anything unrecognised is ignored: an export dir
/// picks up loose media dirs and OS-deduplicated copies, and none of that makes it unusable.
///
/// # Errors
///
/// Returns [`DiscoverError`] when the dir cannot be listed.
pub fn discover_parts(source_dir: impl AsRef<Path>) -> Result<Vec<PartGroup>, DiscoverError> {
    #[derive(Default)]
    struct GroupParts {
        zips: Vec<ZipPart>,
        extracted: Vec<ExtractedPart>,
    }

    let dir = source_dir.as_ref();
    let listing = fs::read_dir(dir).map_err(|source| DiscoverError { dir: dir.to_path_buf(), source })?;
    let mut groups: BTreeMap<String, GroupParts> = BTreeMap::new();

    for entry in listing {
        let path = entry.map_err(|source| DiscoverError { dir: dir.to_path_buf(), source })?.path();
        if path.is_dir() {
            let Some(part) = path.file_name().and_then(OsStr::to_str).and_then(PartName::parse) else { continue };
            let json_dir = path.join("json");
            let json_dir = json_dir.is_dir().then_some(json_dir);
            groups.entry(part.id).or_default().extracted.push(ExtractedPart { number: part.number, path, json_dir });
        } else if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("zip")) {
            let Some(part) = path.file_stem().and_then(OsStr::to_str).and_then(PartName::parse) else { continue };
            groups.entry(part.id).or_default().zips.push(ZipPart { number: part.number, path });
        }
    }

    Ok(groups
        .into_iter()
        .map(|(id, mut parts)| {
            parts.zips.sort_by_key(|p| p.number);
            parts.extracted.sort_by_key(|p| p.number);
            PartGroup { id, zips: parts.zips, extracted: parts.extracted }
        })
        .collect())
}

/// What extraction did with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAction {
    /// Written out: the destination held no file of the entry's size.
    Extracted,
    /// Left untouched: a file of exactly the entry's size was already there.
    AlreadyPresent,
    /// A directory entry, created if it was missing.
    Directory,
}

/// One entry's result, reported in the archive's own order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryOutcome {
    /// Path under the destination, already checked to stay inside it.
    pub path: PathBuf,
    /// How many bytes this entry accounts for in the destination: what was written, what the
    /// already-present file holds, or 0 for a directory. Never the archive's declared size, which
    /// an archive controls and can misstate.
    pub bytes: u64,
    /// What happened to it.
    pub action: EntryAction,
}

/// Something went wrong turning one zip part into files on disk.
#[derive(Debug)]
pub enum ExtractError {
    /// The zip itself could not be opened.
    Open { zip: PathBuf, source: io::Error },
    /// The zip's structure could not be read.
    Archive { zip: PathBuf, source: ::zip::result::ZipError },
    /// An entry named a path outside the destination.
    Escape { zip: PathBuf, entry: String },
    /// An entry is encrypted, and this build has no decryption.
    Encrypted { zip: PathBuf, entry: String },
    /// An entry used a compression method this build cannot decode.
    Unsupported { zip: PathBuf, entry: String, method: CompressionMethod },
    /// A file or dir could not be created under the destination.
    Create { zip: PathBuf, entry: PathBuf, source: io::Error },
    /// An entry's bytes could not be moved out of the zip and onto disk.
    Entry { zip: PathBuf, entry: PathBuf, source: io::Error },
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open { zip, source } => {
                write!(f, "could not open export part {}: {source}; check it is still there and readable", zip.display())
            }
            Self::Archive { zip, source } => write!(
                f,
                "could not read the zip structure of export part {}: {source}; a part that stopped mid-download reads like this, \
                 so re-download it",
                zip.display()
            ),
            Self::Escape { zip, entry } => write!(
                f,
                "{} holds an entry named {entry:?}, which does not name a file inside the extraction dir; \
                 nothing was extracted, and a real export part never names one",
                zip.display()
            ),
            Self::Encrypted { zip, entry } => write!(
                f,
                "{} holds {entry:?} encrypted; nothing was extracted, and this build has no decryption, \
                 so a password-protected archive is not a Snapchat export part",
                zip.display()
            ),
            // The `Display` name is "Unknown" for any method the crate has no variant for, which
            // nobody can file a bug from; `Debug` carries the number behind it.
            Self::Unsupported { zip, entry, method } => write!(
                f,
                "{} holds {entry:?} compressed with {method} ({method:?}); nothing was extracted, \
                 and this build decodes stored and deflate only",
                zip.display()
            ),
            Self::Create { zip, entry, source } => write!(
                f,
                "could not create {} while extracting {}: {source}; check the destination is writable and has free space",
                entry.display(),
                zip.display()
            ),
            Self::Entry { zip, entry, source } => write!(f, "could not extract {} from {}: {source}", entry.display(), zip.display()),
        }
    }
}

impl Error for ExtractError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Archive { source, .. } => Some(source),
            Self::Open { source, .. } | Self::Create { source, .. } | Self::Entry { source, .. } => Some(source),
            Self::Escape { .. } | Self::Encrypted { .. } | Self::Unsupported { .. } => None,
        }
    }
}

/// Extracts one zip part's entries, creating the dirs each entry needs under `dest`. A part with no
/// entries therefore creates nothing, `dest` included.
///
/// Every entry's path, compression method and encryption flag is checked before the first byte is
/// written, so an archive this build refuses leaves the destination exactly as it found it.
///
/// Resumable: an entry is left alone when the destination already holds a file of the size the
/// archive declares for it, so a re-run after an interrupted extraction only redoes what is missing
/// or half-written. Deliberate ceiling: the skip predicate is size alone, so a same-length file
/// with different content is skipped, and an archive that misdeclares a size gets no skip at all.
/// Checking content means reading every already-present file in full, which is the work resuming
/// exists to avoid; the upgrade path is a caller-opt-in verify pass over `entry.crc32()`.
///
/// A group is extracted by walking [`PartGroup::zips`] in order into one `dest`; each part carries
/// its own top-level dirs and they merge. Deliberate ceiling: extracting one part at a time is what
/// lets a caller report which part it is on, and the upgrade path when that caller exists is a
/// callback here, not a loop hidden inside this function.
///
/// Symlink entries are written as ordinary files holding their target text. Nothing here creates a
/// link, so an archive cannot plant one and then write through it. Deliberate ceiling: a symlink
/// that ALREADY exists inside `dest` is still followed, and the upgrade path is opening entries
/// with `O_NOFOLLOW` once extraction targets a dir the user did not create.
///
/// # Errors
///
/// Returns [`ExtractError`] when the zip cannot be opened or read, when an entry does not name a
/// file inside `dest`, when an entry is encrypted or uses a compression method this build cannot
/// decode, or when writing fails.
pub fn extract_part(zip_path: impl AsRef<Path>, dest: impl AsRef<Path>) -> Result<Vec<EntryOutcome>, ExtractError> {
    let zip_path = zip_path.as_ref();
    let dest = dest.as_ref();

    let file = fs::File::open(zip_path).map_err(|source| ExtractError::Open { zip: zip_path.to_path_buf(), source })?;
    let mut archive =
        ::zip::ZipArchive::new(io::BufReader::new(file)).map_err(|source| ExtractError::Archive { zip: zip_path.to_path_buf(), source })?;

    let plan = plan_entries(&mut archive, zip_path)?;
    let mut outcomes = Vec::with_capacity(plan.len());

    for planned in plan {
        let out_path = dest.join(&planned.path);
        if planned.is_dir {
            fs::create_dir_all(&out_path).map_err(|source| ExtractError::Create {
                zip: zip_path.to_path_buf(),
                entry: planned.path.clone(),
                source,
            })?;
            outcomes.push(EntryOutcome { path: planned.path, bytes: 0, action: EntryAction::Directory });
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|source| ExtractError::Create {
                zip: zip_path.to_path_buf(),
                entry: planned.path.clone(),
                source,
            })?;
        }
        let on_disk = fs::metadata(&out_path).ok().filter(fs::Metadata::is_file).map(|found| found.len());
        if let Some(len) = on_disk
            && len == planned.bytes
        {
            outcomes.push(EntryOutcome { path: planned.path, bytes: len, action: EntryAction::AlreadyPresent });
            continue;
        }

        let mut entry = archive.by_index(planned.index).map_err(|source| ExtractError::Archive { zip: zip_path.to_path_buf(), source })?;
        let mut out = fs::File::create(&out_path).map_err(|source| ExtractError::Create {
            zip: zip_path.to_path_buf(),
            entry: planned.path.clone(),
            source,
        })?;
        let written = io::copy(&mut entry, &mut out).map_err(|source| ExtractError::Entry {
            zip: zip_path.to_path_buf(),
            entry: planned.path.clone(),
            source,
        })?;
        outcomes.push(EntryOutcome { path: planned.path, bytes: written, action: EntryAction::Extracted });
    }

    Ok(outcomes)
}

/// One entry, checked and ready to write.
struct PlannedEntry {
    index: usize,
    path: PathBuf,
    bytes: u64,
    is_dir: bool,
}

/// Validates every entry before the first byte is written, so a rejected archive leaves the
/// destination as it found it.
fn plan_entries<R: io::Read + io::Seek>(archive: &mut ::zip::ZipArchive<R>, zip_path: &Path) -> Result<Vec<PlannedEntry>, ExtractError> {
    let mut plan = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        // Metadata only: `by_index` builds a decompressor and fails on an unsupported method
        // before the method can be named in the error.
        let entry = archive.by_index_raw(index).map_err(|source| ExtractError::Archive { zip: zip_path.to_path_buf(), source })?;
        let name = entry.name().to_owned();
        let Some(path) = entry.enclosed_name().filter(|path| !path.as_os_str().is_empty() && !is_rooted(&name)) else {
            return Err(ExtractError::Escape { zip: zip_path.to_path_buf(), entry: name });
        };
        // `by_index_raw` never consults the encrypted flag, so without this the write loop would
        // discover it at the first encrypted entry, with everything ahead of it already on disk.
        if entry.encrypted() {
            return Err(ExtractError::Encrypted { zip: zip_path.to_path_buf(), entry: name });
        }
        let is_dir = entry.is_dir();
        let method = entry.compression();
        if !is_dir && !::zip::SUPPORTED_COMPRESSION_METHODS.contains(&method) {
            return Err(ExtractError::Unsupported { zip: zip_path.to_path_buf(), entry: name, method });
        }
        plan.push(PlannedEntry { index, path, bytes: entry.size(), is_dir });
    }
    Ok(plan)
}

/// Whether an entry name points at a filesystem root or a drive rather than into the destination.
///
/// `enclosed_name` answers a leading root by stripping it and handing back a relative path, so on
/// its own it would silently write `/etc/hosts` to `<dest>/etc/hosts`. A drive segment ANYWHERE in
/// the name is worse: `typed_path` only recognises a drive prefix at position 0, so `foo/C:/bar`
/// reaches `enclosed_name` as three ordinary components and comes back whole, and on Windows
/// `PathBuf::push` then resolves that `C:` by discarding everything to its left
/// (`library/std/src/path.rs` `_push`: `need_clear = path.is_absolute() || path.prefix().is_some()`).
/// `dest.join(...)` inherits the same rule, so the entry lands on drive C: instead of under `dest`.
/// Every segment is checked here rather than only the head.
fn is_rooted(name: &str) -> bool {
    name.starts_with(['/', '\\'])
        || name.split(['/', '\\']).any(|segment| {
            let mut chars = segment.chars();
            matches!((chars.next(), chars.next()), (Some(drive), Some(':')) if drive.is_ascii_alphabetic())
        })
}

#[cfg(test)]
mod tests {
    use super::is_rooted;

    #[test]
    fn a_plain_relative_entry_name_is_not_rooted() {
        assert!(!is_rooted("json/account.json"));
        assert!(!is_rooted("account.json"));
    }

    #[test]
    fn a_traversal_entry_name_is_not_rooted_because_enclosed_name_owns_that_case() {
        assert!(!is_rooted("../escaped.txt"));
        assert!(!is_rooted("json/../../escaped.txt"));
    }

    #[test]
    fn a_unix_or_windows_root_is_rooted() {
        assert!(is_rooted("/etc/hosts"));
        assert!(is_rooted("\\windows\\system32\\drivers\\etc\\hosts"));
    }

    #[test]
    fn a_drive_segment_is_rooted_wherever_it_sits_in_the_name() {
        assert!(is_rooted("foo/C:/bar"));
        assert!(is_rooted("./C:/y"));
        assert!(is_rooted("json\\C:\\pwned.txt"));
        assert!(is_rooted("a/b/c/D:"));
    }

    #[test]
    fn a_drive_prefix_is_rooted_only_behind_a_letter() {
        assert!(is_rooted("C:/windows/win.ini"));
        assert!(is_rooted("c:windows/win.ini"));
        assert!(!is_rooted("4:00/photo.jpg"));
        assert!(!is_rooted(":/photo.jpg"));
    }

    #[test]
    fn a_one_character_name_does_not_panic_on_the_drive_check() {
        assert!(!is_rooted("a"));
        assert!(!is_rooted(""));
        assert!(is_rooted("/"));
    }
}
