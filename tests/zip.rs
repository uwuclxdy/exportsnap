//! Public-API tests for `exportsnap::export::zip`: which `mydata~*` parts a source dir holds, and
//! turning one of them into files on disk.
//!
//! Every archive here is built inside the test with the `zip` crate. Nothing reads a real export.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use exportsnap::export::zip::{DiscoverError, EntryAction, EntryOutcome, ExtractError, PartName, discover_parts, extract_part};
use tempfile::TempDir;
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

/// The 13-digit id shape the one observed export used.
const ID: &str = "1784667002819";
const OTHER_ID: &str = "1799123456780";

/// A directory entry when the body is `None`, a file entry otherwise.
type Entry<'a> = (&'a str, Option<&'a [u8]>);

fn write_zip(path: &Path, entries: &[Entry<'_>]) {
    let mut writer = ZipWriter::new(fs::File::create(path).unwrap());
    let options = SimpleFileOptions::default();
    for (name, body) in entries {
        match body {
            Some(bytes) => {
                writer.start_file(*name, options).unwrap();
                writer.write_all(bytes).unwrap();
            }
            None => writer.add_directory(*name, options).unwrap(),
        }
    }
    writer.finish().unwrap();
}

/// The name Snapchat gives part `number`: part 1 carries no suffix at all.
fn part_path(dir: &Path, id: &str, number: u32) -> PathBuf {
    if number == 1 { dir.join(format!("mydata~{id}.zip")) } else { dir.join(format!("mydata~{id}-{number}.zip")) }
}

/// A source dir holding `numbers` as zips of one trivial entry each.
fn source_with_parts(numbers: &[u32]) -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    for number in numbers {
        write_zip(&part_path(&source, ID, *number), &[("chat_media/photo.jpg", Some(b"jpegbytes"))]);
    }
    (temp, source)
}

fn one_group(source: &Path) -> exportsnap::export::zip::PartGroup {
    let mut groups = discover_parts(source).unwrap();
    assert_eq!(groups.len(), 1, "expected exactly one stem group in {}", source.display());
    groups.remove(0)
}

fn zip_numbers(group: &exportsnap::export::zip::PartGroup) -> Vec<u32> {
    group.zips.iter().map(|part| part.number).collect()
}

// ---- PartName ----------------------------------------------------------------------------------

#[test]
fn part_one_is_the_suffixless_name_and_the_rest_carry_a_stem_suffix() {
    let first = PartName::parse("mydata~1784667002819").unwrap();
    assert_eq!(first.id, "1784667002819");
    assert_eq!(first.number, 1);

    let fifth = PartName::parse("mydata~1784667002819-5").unwrap();
    assert_eq!(fifth.id, "1784667002819");
    assert_eq!(fifth.number, 5);
}

#[test]
fn a_suffix_that_is_not_a_part_number_stays_part_of_the_id() {
    let dashed = PartName::parse("mydata~1784667002819-copy").unwrap();
    assert_eq!(dashed.id, "1784667002819-copy");
    assert_eq!(dashed.number, 1);

    // `-1` is not a spelling of part 1: part 1 has no suffix.
    let one = PartName::parse("mydata~1784667002819-1").unwrap();
    assert_eq!(one.id, "1784667002819-1");
    assert_eq!(one.number, 1);
}

/// `u32::from_str` accepts `+2` and `02`, so without a digit check those would land in the same
/// group as a real `-2` and give one delivery two part 2s with nothing flagging it.
#[test]
fn a_number_spelled_oddly_is_not_a_part_number() {
    let signed = PartName::parse("mydata~1784667002819-+2").unwrap();
    assert_eq!(signed.id, "1784667002819-+2");
    assert_eq!(signed.number, 1);

    let padded = PartName::parse("mydata~1784667002819-02").unwrap();
    assert_eq!(padded.id, "1784667002819-02");
    assert_eq!(padded.number, 1);
}

#[test]
fn a_name_without_the_prefix_or_without_an_id_is_not_a_part() {
    assert_eq!(PartName::parse("holiday-photos"), None);
    assert_eq!(PartName::parse("mydata"), None);
    assert_eq!(PartName::parse("mydata~"), None);
    assert_eq!(PartName::parse("mydata~-2"), None);
}

// ---- discovery ---------------------------------------------------------------------------------

#[test]
fn parts_come_back_in_numeric_order_however_they_sit_on_disk() {
    // 10 sorts before 2 lexicographically and the creation order is scrambled, so neither a
    // name-ordered nor a creation-ordered answer matches. Eight parts leave a hash-ordering
    // filesystem 1 chance in 40320 of landing on the sorted permutation by luck.
    let (_temp, source) = source_with_parts(&[10, 2, 1, 3, 7, 5, 9, 4]);

    // The fixture's whole point is that disk order is not numeric order, so assert that rather than
    // assume it. On a filesystem that happened to hand these back sorted, the run cannot tell a
    // real sort from a passthrough, and a removed `sort_by_key` would read as a pass.
    let on_disk: Vec<u32> = fs::read_dir(&source)
        .unwrap()
        .map(|entry| PartName::parse(entry.unwrap().path().file_stem().unwrap().to_str().unwrap()).unwrap().number)
        .collect();
    assert_ne!(on_disk, vec![1, 2, 3, 4, 5, 7, 9, 10], "inconclusive: the filesystem returned the parts already sorted");

    let group = one_group(&source);

    assert_eq!(group.id, ID);
    assert_eq!(zip_numbers(&group), vec![1, 2, 3, 4, 5, 7, 9, 10]);
    assert_eq!(group.zips[0].path, part_path(&source, ID, 1));
    assert_eq!(group.zips[7].path, part_path(&source, ID, 10));
}

#[test]
fn two_deliveries_in_one_dir_stay_two_groups() {
    let (_temp, source) = source_with_parts(&[1, 2]);
    write_zip(&part_path(&source, OTHER_ID, 1), &[("chat_media/other.jpg", Some(b"other"))]);
    write_zip(&part_path(&source, OTHER_ID, 3), &[("chat_media/other.jpg", Some(b"other"))]);

    let groups = discover_parts(&source).unwrap();

    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].id, ID);
    assert_eq!(zip_numbers(&groups[0]), vec![1, 2]);
    assert_eq!(groups[1].id, OTHER_ID);
    assert_eq!(zip_numbers(&groups[1]), vec![1, 3]);
}

#[test]
fn a_gap_in_the_numbering_is_reported() {
    let (_temp, source) = source_with_parts(&[1, 2, 4]);

    assert_eq!(one_group(&source).missing_parts(), vec![3]);
}

#[test]
fn a_complete_delivery_reports_no_gap() {
    let (_temp, source) = source_with_parts(&[1, 2, 3]);

    assert_eq!(one_group(&source).missing_parts(), Vec::<u32>::new());
}

#[test]
fn an_already_extracted_part_fills_the_gap_its_deleted_zip_left() {
    let (_temp, source) = source_with_parts(&[1, 3]);
    fs::create_dir(source.join(format!("mydata~{ID}-2"))).unwrap();

    assert_eq!(one_group(&source).missing_parts(), Vec::<u32>::new());
}

#[test]
fn already_extracted_parts_are_reported_with_the_json_dir_they_hold() {
    let (_temp, source) = source_with_parts(&[3]);
    let first = source.join(format!("mydata~{ID}"));
    let second = source.join(format!("mydata~{ID}-2"));
    fs::create_dir_all(first.join("json")).unwrap();
    fs::create_dir_all(second.join("chat_media")).unwrap();

    let group = one_group(&source);

    assert_eq!(group.extracted.iter().map(|part| part.number).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(group.extracted[0].path, first);
    assert_eq!(group.extracted[0].json_dir, Some(first.join("json")));
    assert_eq!(group.extracted[1].path, second);
    assert_eq!(group.extracted[1].json_dir, None);
    assert_eq!(zip_numbers(&group), vec![3]);
}

#[test]
fn anything_not_shaped_like_a_part_is_ignored() {
    let (_temp, source) = source_with_parts(&[1]);
    fs::create_dir(source.join("memories (1)")).unwrap();
    fs::write(source.join("notes.txt"), b"mine").unwrap();
    write_zip(&source.join("holiday.zip"), &[("photo.jpg", Some(b"jpegbytes"))]);

    let group = one_group(&source);

    assert_eq!(zip_numbers(&group), vec![1]);
    assert_eq!(group.extracted, Vec::new());
}

#[test]
fn an_uppercase_extension_is_still_a_part() {
    let (_temp, source) = source_with_parts(&[1]);
    write_zip(&source.join(format!("mydata~{ID}-2.ZIP")), &[("chat_media/photo.jpg", Some(b"jpegbytes"))]);

    assert_eq!(zip_numbers(&one_group(&source)), vec![1, 2]);
}

#[test]
fn a_source_dir_that_is_not_there_names_itself() {
    let temp = TempDir::new().unwrap();
    let missing = temp.path().join("nowhere");

    let error: DiscoverError = discover_parts(&missing).unwrap_err();

    assert_eq!(error.dir, missing);
    assert!(error.to_string().contains(&missing.display().to_string()), "{error}");
}

// ---- extraction --------------------------------------------------------------------------------

/// A part holding one dir entry and two files, plus the dest to unpack it into.
fn extractable_part() -> (TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new().unwrap();
    let zip = temp.path().join("part.zip");
    write_zip(&zip, &[("json/", None), ("json/account.json", Some(b"{\"a\":1}")), ("chat_media/photo.jpg", Some(b"jpegbytes"))]);
    let dest = temp.path().join("out");
    (temp, zip, dest)
}

fn expect_full_extraction() -> Vec<EntryOutcome> {
    vec![
        EntryOutcome { path: PathBuf::from("json"), bytes: 0, action: EntryAction::Directory },
        EntryOutcome { path: PathBuf::from("json/account.json"), bytes: 7, action: EntryAction::Extracted },
        EntryOutcome { path: PathBuf::from("chat_media/photo.jpg"), bytes: 9, action: EntryAction::Extracted },
    ]
}

#[test]
fn extraction_writes_every_entry_into_a_destination_it_creates() {
    let (_temp, zip, dest) = extractable_part();

    let outcomes = extract_part(&zip, &dest).unwrap();

    assert_eq!(outcomes, expect_full_extraction());
    assert!(dest.join("json").is_dir());
    assert_eq!(fs::read(dest.join("json/account.json")).unwrap(), b"{\"a\":1}");
    assert_eq!(fs::read(dest.join("chat_media/photo.jpg")).unwrap(), b"jpegbytes");
}

#[test]
fn a_second_run_leaves_every_entry_that_is_already_the_right_size() {
    let (_temp, zip, dest) = extractable_part();
    extract_part(&zip, &dest).unwrap();
    // Same length, different bytes: only a genuine skip leaves this standing.
    fs::write(dest.join("json/account.json"), b"XXXXXXX").unwrap();

    let outcomes = extract_part(&zip, &dest).unwrap();

    assert_eq!(
        outcomes,
        vec![
            EntryOutcome { path: PathBuf::from("json"), bytes: 0, action: EntryAction::Directory },
            EntryOutcome { path: PathBuf::from("json/account.json"), bytes: 7, action: EntryAction::AlreadyPresent },
            EntryOutcome { path: PathBuf::from("chat_media/photo.jpg"), bytes: 9, action: EntryAction::AlreadyPresent },
        ]
    );
    assert_eq!(fs::read(dest.join("json/account.json")).unwrap(), b"XXXXXXX");
}

#[test]
fn a_second_run_rewrites_an_entry_left_short_by_an_interrupted_one() {
    let (_temp, zip, dest) = extractable_part();
    extract_part(&zip, &dest).unwrap();
    fs::write(dest.join("json/account.json"), b"{\"a").unwrap();

    let outcomes = extract_part(&zip, &dest).unwrap();

    assert_eq!(outcomes[1], EntryOutcome { path: PathBuf::from("json/account.json"), bytes: 7, action: EntryAction::Extracted });
    assert_eq!(outcomes[2].action, EntryAction::AlreadyPresent);
    assert_eq!(fs::read(dest.join("json/account.json")).unwrap(), b"{\"a\":1}");
}

#[test]
fn a_second_run_rewrites_an_entry_that_grew_past_its_size() {
    let (_temp, zip, dest) = extractable_part();
    extract_part(&zip, &dest).unwrap();
    fs::write(dest.join("json/account.json"), b"{\"a\":1} trailing junk").unwrap();

    let outcomes = extract_part(&zip, &dest).unwrap();

    assert_eq!(outcomes[1], EntryOutcome { path: PathBuf::from("json/account.json"), bytes: 7, action: EntryAction::Extracted });
    assert_eq!(fs::read(dest.join("json/account.json")).unwrap(), b"{\"a\":1}");
}

#[test]
fn a_truncated_part_names_itself_and_writes_nothing() {
    let (_temp, zip, dest) = extractable_part();
    let whole = fs::read(&zip).unwrap();
    fs::write(&zip, &whole[..whole.len() / 2]).unwrap();

    let error = extract_part(&zip, &dest).unwrap_err();

    let ExtractError::Archive { zip: named, .. } = &error else { panic!("expected Archive, got {error:?}") };
    assert_eq!(named, &zip);
    assert!(error.to_string().contains("re-download"), "{error}");
    assert!(!dest.exists(), "a part that cannot be read must not create the destination");
}

#[test]
fn a_missing_part_names_itself() {
    let temp = TempDir::new().unwrap();
    let zip = temp.path().join("gone.zip");

    let error = extract_part(&zip, temp.path().join("out")).unwrap_err();

    let ExtractError::Open { zip: named, .. } = &error else { panic!("expected Open, got {error:?}") };
    assert_eq!(named, &zip);
}

// ---- zip slip ----------------------------------------------------------------------------------

/// Builds a zip whose second entry is `hostile`, extracts it, and returns the rejected name plus
/// the tempdir the destination sits inside.
fn reject_hostile_entry(hostile: &str) -> (TempDir, PathBuf, String) {
    let temp = TempDir::new().unwrap();
    let zip = temp.path().join("part.zip");
    write_zip(&zip, &[("json/account.json", Some(b"{\"a\":1}")), (hostile, Some(b"pwned"))]);
    let dest = temp.path().join("out");

    let error = extract_part(&zip, &dest).unwrap_err();
    let ExtractError::Escape { zip: named, entry } = error else { panic!("expected Escape for {hostile:?}") };
    assert_eq!(named, zip);
    assert!(!dest.exists(), "a rejected archive must not write its harmless entries either");
    (temp, dest, entry)
}

#[test]
fn a_parent_dir_entry_is_rejected_and_nothing_lands_outside_the_destination() {
    let (temp, _dest, entry) = reject_hostile_entry("../escaped.txt");

    assert_eq!(entry, "../escaped.txt");
    assert!(!temp.path().join("escaped.txt").exists());
}

#[test]
fn a_deep_traversal_entry_is_rejected() {
    let (temp, _dest, entry) = reject_hostile_entry("json/../../escaped.txt");

    assert_eq!(entry, "json/../../escaped.txt");
    assert!(!temp.path().join("escaped.txt").exists());
}

#[test]
fn an_absolute_entry_is_rejected_rather_than_quietly_rewritten() {
    let (_temp, dest, entry) = reject_hostile_entry("/etc/hosts");

    assert_eq!(entry, "/etc/hosts");
    assert!(!dest.join("etc/hosts").exists(), "the root must not be stripped into a relative write");
}

#[test]
fn a_drive_relative_entry_is_rejected() {
    let (_temp, dest, entry) = reject_hostile_entry("C:windows/win.ini");

    assert_eq!(entry, "C:windows/win.ini");
    assert!(!dest.join("windows/win.ini").exists());
}

/// A drive segment PAST the first one still escapes on Windows: `enclosed_name` keeps `C:` as a
/// normal component, and `PathBuf::push` discards everything left of a drive prefix, so
/// `dest.join("C:pwned.txt")` is drive C:'s working dir rather than a path under `dest`.
#[test]
fn a_drive_segment_in_the_middle_of_an_entry_name_is_rejected() {
    let (_temp, dest, entry) = reject_hostile_entry("json/C:/pwned.txt");

    assert_eq!(entry, "json/C:/pwned.txt");
    assert!(!dest.join("json/C:/pwned.txt").exists());
}

#[test]
fn a_drive_segment_behind_a_current_dir_entry_is_rejected() {
    let (_temp, dest, entry) = reject_hostile_entry("./C:/y.txt");

    assert_eq!(entry, "./C:/y.txt");
    assert!(!dest.join("y.txt").exists());
}

// ---- compression methods -----------------------------------------------------------------------

fn find_nth(haystack: &[u8], needle: &[u8], nth: usize) -> usize {
    let mut hits = haystack.windows(needle.len()).enumerate().filter(|(_, window)| *window == needle).map(|(at, _)| at);
    hits.nth(nth).expect("header signature")
}

/// A part whose SECOND entry is the one to doctor, with a harmless first entry ahead of it.
///
/// The harmless entry is what makes a rejection provable: it is the file that must NOT be on disk
/// when the second entry is refused. Both are stored uncompressed with bodies carrying no `PK`
/// signature, which is what keeps the header searches unambiguous.
fn part_with_a_doctorable_second_entry() -> (TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new().unwrap();
    let zip = temp.path().join("part.zip");
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut writer = ZipWriter::new(fs::File::create(&zip).unwrap());
    writer.start_file("json/account.json", stored).unwrap();
    writer.write_all(b"{\"a\":1}").unwrap();
    writer.start_file("chat_media/photo.jpg", stored).unwrap();
    writer.write_all(b"hello").unwrap();
    writer.finish().unwrap();
    let dest = temp.path().join("out");
    (temp, zip, dest)
}

/// Rewrites the second entry's compression method in both headers, since this build's `zip` cannot
/// WRITE a method it refuses to read.
fn stamp_compression_method(path: &Path, method: u16) {
    let mut bytes = fs::read(path).unwrap();
    let local = find_nth(&bytes, b"PK\x03\x04", 1);
    let central = find_nth(&bytes, b"PK\x01\x02", 1);
    bytes[local + 8..local + 10].copy_from_slice(&method.to_le_bytes());
    bytes[central + 10..central + 12].copy_from_slice(&method.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

/// Rewrites the `nth` entry's declared uncompressed size in the CENTRAL header only, leaving the
/// real bytes and the crc valid so the archive still reads. This is how an archive lies about a
/// size, which is the whole reason `EntryOutcome::bytes` must not come from the header.
fn stamp_declared_size(path: &Path, nth: usize, size: u32) {
    let mut bytes = fs::read(path).unwrap();
    let central = find_nth(&bytes, b"PK\x01\x02", nth);
    bytes[central + 24..central + 28].copy_from_slice(&size.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

/// Sets general-purpose flag bit 0 on the second entry in both headers. Same reason as the method
/// stamp: this build's `zip` has no encryption writer.
fn stamp_encrypted_flag(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    let local = find_nth(&bytes, b"PK\x03\x04", 1);
    let central = find_nth(&bytes, b"PK\x01\x02", 1);
    bytes[local + 6] |= 1;
    bytes[central + 8] |= 1;
    fs::write(path, bytes).unwrap();
}

#[test]
fn an_entry_this_build_cannot_decode_names_the_method_and_the_entry() {
    let (_temp, zip, dest) = part_with_a_doctorable_second_entry();
    stamp_compression_method(&zip, 12);

    let error = extract_part(&zip, &dest).unwrap_err();

    let ExtractError::Unsupported { entry, method, .. } = &error else { panic!("expected Unsupported, got {error:?}") };
    assert_eq!(entry, "chat_media/photo.jpg");
    assert_eq!(*method, CompressionMethod::BZIP2);
    assert!(error.to_string().contains("Bzip2"), "{error}");
    assert!(!dest.exists(), "the entry ahead of the undecodable one must not reach disk");
}

/// A method number `zip` has no name for still has to leave the reader something to report.
#[test]
fn a_method_with_no_name_still_carries_its_number() {
    let (_temp, zip, dest) = part_with_a_doctorable_second_entry();
    stamp_compression_method(&zip, 42);

    let error = extract_part(&zip, &dest).unwrap_err();

    assert!(error.to_string().contains("Unsupported(42)"), "{error}");
}

#[test]
fn an_encrypted_entry_is_named_as_encrypted_and_stops_the_extraction() {
    let (_temp, zip, dest) = part_with_a_doctorable_second_entry();
    stamp_encrypted_flag(&zip);

    let error = extract_part(&zip, &dest).unwrap_err();

    let ExtractError::Encrypted { zip: named, entry } = &error else { panic!("expected Encrypted, got {error:?}") };
    assert_eq!(named, &zip);
    assert_eq!(entry, "chat_media/photo.jpg");
    assert!(!error.to_string().contains("re-download"), "an encrypted part is not a damaged download: {error}");
    assert!(!dest.exists(), "the entry ahead of the encrypted one must not reach disk");
}

// ---- reported bytes describe the destination, not the header ------------------------------------

/// A stored `hello` whose central header claims 5000 bytes, so the declared size and the real one
/// disagree while the archive still reads.
fn part_with_a_lying_size() -> (TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new().unwrap();
    let zip = temp.path().join("part.zip");
    let mut writer = ZipWriter::new(fs::File::create(&zip).unwrap());
    writer.start_file("chat_media/photo.jpg", SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
    writer.write_all(b"hello").unwrap();
    writer.finish().unwrap();
    stamp_declared_size(&zip, 0, 5000);
    let dest = temp.path().join("out");
    (temp, zip, dest)
}

#[test]
fn reported_bytes_are_what_was_written_not_what_the_header_claimed() {
    let (_temp, zip, dest) = part_with_a_lying_size();

    let outcomes = extract_part(&zip, &dest).unwrap();

    assert_eq!(outcomes, vec![EntryOutcome { path: PathBuf::from("chat_media/photo.jpg"), bytes: 5, action: EntryAction::Extracted }]);
    assert_eq!(fs::metadata(dest.join("chat_media/photo.jpg")).unwrap().len(), 5);
}

/// The documented ceiling: the skip predicate is the DECLARED size, so an entry whose header lies
/// is re-extracted on every run rather than ever converging on `AlreadyPresent`.
#[test]
fn an_entry_whose_header_lies_about_its_size_never_gets_skipped() {
    let (_temp, zip, dest) = part_with_a_lying_size();
    extract_part(&zip, &dest).unwrap();

    let second = extract_part(&zip, &dest).unwrap();

    assert_eq!(second[0].action, EntryAction::Extracted);
}

#[test]
fn a_directory_entry_reports_no_bytes_whatever_its_header_declares() {
    let temp = TempDir::new().unwrap();
    let zip = temp.path().join("part.zip");
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut writer = ZipWriter::new(fs::File::create(&zip).unwrap());
    writer.add_directory("json", stored).unwrap();
    writer.start_file("json/account.json", stored).unwrap();
    writer.write_all(b"{\"a\":1}").unwrap();
    writer.finish().unwrap();
    stamp_declared_size(&zip, 0, 777);
    let dest = temp.path().join("out");

    let outcomes = extract_part(&zip, &dest).unwrap();

    assert_eq!(outcomes[0], EntryOutcome { path: PathBuf::from("json"), bytes: 0, action: EntryAction::Directory });
}

#[test]
fn a_part_with_no_entries_creates_nothing() {
    let temp = TempDir::new().unwrap();
    let zip = temp.path().join("part.zip");
    write_zip(&zip, &[]);
    let dest = temp.path().join("out");

    let outcomes = extract_part(&zip, &dest).unwrap();

    assert_eq!(outcomes, Vec::new());
    assert!(!dest.exists(), "an empty part must not leave a bare dir behind");
}

#[test]
fn a_stored_entry_still_extracts() {
    let temp = TempDir::new().unwrap();
    let zip = temp.path().join("part.zip");
    let mut writer = ZipWriter::new(fs::File::create(&zip).unwrap());
    writer.start_file("chat_media/photo.jpg", SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
    writer.write_all(b"hello").unwrap();
    writer.finish().unwrap();
    let dest = temp.path().join("out");

    let outcomes = extract_part(&zip, &dest).unwrap();

    assert_eq!(outcomes, vec![EntryOutcome { path: PathBuf::from("chat_media/photo.jpg"), bytes: 5, action: EntryAction::Extracted }]);
    assert_eq!(fs::read(dest.join("chat_media/photo.jpg")).unwrap(), b"hello");
}
