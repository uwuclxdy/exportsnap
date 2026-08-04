//! Public-API tests for `exportsnap::export::local_fix`: the composite, the EXIF/GPS/datetime
//! stamp, the timezone, the file date, the year/month rename, the video leg's transcode and
//! degrade behavior, and what a resume skips.
//!
//! Nothing here reads a real export. Every image is synthesized in the test, every video is built
//! by ffmpeg from a colour source, every directory is a tempdir, and every manifest is opened with
//! `open_in` so the per-user data dir is never touched.
//!
//! **The metadata assertions read the output back through `exiftool` and `ffprobe`, not through
//! `little_exif` or `mp4ameta`.** A crate reading its own write can agree with itself about a wrong
//! encoding, which is exactly what an independent reader is for. Neither tool is a build
//! dependency, so those tests print a skip notice and pass when one is absent — the box this repo
//! is gated on has them, and the phase-5 CI leg has to install them or the coverage silently
//! disappears. Everything a byte-level assertion can cover is asserted unconditionally instead.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use chrono::NaiveDate;
use exportsnap::export::exif::{Jpeg, Stamp};
use exportsnap::export::local_fix::{self, DeferralReason, Leg, Notice, Plan, TimeSource, TranscodeSkip, VideoOptions};
use exportsnap::export::manifest::{ExportId, ItemKind, ItemStatus, Manifest};
use exportsnap::export::memories::{Discovery, MemoryFile, Reconciliation, reconcile};
use exportsnap::export::model::{Field, LocationPoint, Memories};
use exportsnap::export::schema;
use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use tempfile::TempDir;

/// The 13-digit id shape the one observed export used.
const EXPORT_ID: &str = "1784667002819";

/// Paris, and a coordinate whose degrees, minutes and seconds are all non-zero so a dropped
/// component is visible in the round trip.
const PARIS: &str = "Latitude, Longitude: 48.858844, 2.294351";
/// Far enough from Paris that no rounding could confuse the two, and southern + western so the
/// hemisphere refs are exercised in both directions.
const RIO: &str = "Latitude, Longitude: -22.951916, -43.210487";

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

// ---- fixtures ----

/// A distinct 36-character dashed uuid per `seed`, in the shape a memory filename carries.
fn uuid(seed: u32) -> String {
    format!("{seed:08x}-3ff7-45f1-95f9-a2fda6ba0f8e")
}

/// Writes a JPEG main file into `dir/memories` and returns its parsed name.
///
/// The pattern is high-frequency on purpose: a solid colour survives a JPEG re-encode with every
/// pixel bit-identical, so a fixture painted flat holds constant the exact dimension
/// `a_main_with_no_overlay_is_copied_byte_for_byte_rather_than_re_encoded` asserts on, and that
/// test passes whether the copy happens or not. The red channel is kept under 40 so the overlay
/// assertions elsewhere still have a clean "is this red" question to ask.
fn write_main(dir: &Path, day: &str, seed: u32) -> MemoryFile {
    let mut pixels = RgbImage::new(WIDTH, HEIGHT);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = Rgb([(x % 7) as u8 * 5, ((x * 13 + y * 7) % 251) as u8, ((x * 29 + y * 17) % 253) as u8]);
    }
    let path = memories_dir(dir).join(format!("{day}_{}-main.jpg", uuid(seed)));
    pixels.save_with_format(&path, ImageFormat::Jpeg).unwrap();
    MemoryFile::parse(path).unwrap()
}

/// An overlay whose left half is opaque red and whose right half is fully transparent, so a
/// composite that ran and one that did not are told apart by a single pixel each way.
fn write_overlay(dir: &Path, day: &str, seed: u32) -> MemoryFile {
    let mut pixels = RgbaImage::new(WIDTH, HEIGHT);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < WIDTH / 2 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 0, 0]) };
    }
    let path = memories_dir(dir).join(format!("{day}_{}-overlay.png", uuid(seed)));
    pixels.save_with_format(&path, ImageFormat::Png).unwrap();
    MemoryFile::parse(path).unwrap()
}

/// A main file of an arbitrary format under an arbitrary name, for the paths that have to refuse
/// or transcode one.
fn write_raw(dir: &Path, name: &str, bytes: &[u8]) -> MemoryFile {
    let path = memories_dir(dir).join(name);
    fs::write(&path, bytes).unwrap();
    MemoryFile::parse(path).unwrap()
}

/// A half-second HEVC video with an audio track, named like a memory's main file.
///
/// `hvc1` on purpose: it is what the export ships and what the transcode exists to move away from,
/// so a leg that quietly skipped the re-encode would pass against a fixture in anything else.
/// `None` when ffmpeg is absent, which is the one thing the fixture cannot fake.
fn write_video(dir: &Path, day: &str, seed: u32) -> Option<MemoryFile> {
    let path = memories_dir(dir).join(format!("{day}_{}-main.mp4", uuid(seed)));
    let built = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", &format!("color=c=blue:s={WIDTH}x{HEIGHT}:r=15:d=0.5")])
        .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono", "-shortest"])
        .args(["-c:v", "libx265", "-tag:v", "hvc1", "-pix_fmt", "yuv420p", "-c:a", "aac", "-t", "0.5"])
        .arg(&path)
        .output()
        .ok()?;
    assert!(built.status.success(), "ffmpeg could not build the fixture: {}", String::from_utf8_lossy(&built.stderr));
    Some(MemoryFile::parse(path).unwrap())
}

/// A minimal but structurally real MP4, built in pure Rust, named like a memory's main file.
///
/// Exists so the no-ffmpeg degrade path can be tested **on a box with no ffmpeg** — the one
/// environment whose behaviour that test describes. An ffmpeg-built fixture makes the test skip
/// itself exactly there, which is coverage of the wrong machine. Real pixels are not needed for it:
/// with `ffmpeg: None` nothing re-encodes, so every assertion it makes is about the container.
///
/// The sizes are the ones the spec gives each box, because `mp4ameta` checks them against its own
/// constants and refuses a stub, and the movie timescale is non-zero because zero is a division by
/// zero inside that crate.
fn write_synthetic_video(dir: &Path, day: &str, seed: u32) -> MemoryFile {
    fn atom(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(8 + body.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend(fourcc);
        bytes.extend(body);
        bytes
    }
    fn header(content: usize) -> Vec<u8> {
        let mut body = vec![0; 12];
        body.extend(1000_u32.to_be_bytes());
        body.resize(content, 0);
        body
    }

    let mdia = atom(b"mdia", &atom(b"mdhd", &header(24)));
    let trak = atom(b"trak", &[atom(b"tkhd", &header(84)), mdia].concat());
    let mut bytes = atom(b"ftyp", b"isom\0\0\x02\0isommp42");
    bytes.extend(atom(b"moov", &[atom(b"mvhd", &header(100)), trak].concat()));
    bytes.extend(atom(b"mdat", &[0; 16]));

    let path = memories_dir(dir).join(format!("{day}_{}-main.mp4", uuid(seed)));
    fs::write(&path, bytes).unwrap();
    MemoryFile::parse(path).unwrap()
}

fn memories_dir(dir: &Path) -> PathBuf {
    let memories = dir.join("memories");
    fs::create_dir_all(&memories).unwrap();
    memories
}

/// `memories_history.json` entries, built through the real schema-to-model path so the plan never
/// sees a state the loader could not produce.
fn entries(rows: &[(&str, &str, &str)]) -> Memories {
    let saved_media = rows
        .iter()
        .map(|(date, media_type, location)| schema::SavedMediaEntry {
            date: (*date).to_owned(),
            media_type: (*media_type).to_owned(),
            location: (*location).to_owned(),
            ..schema::SavedMediaEntry::default()
        })
        .collect();
    Memories::try_from(schema::MemoriesHistory { saved_media }).unwrap()
}

/// `YYYY-MM-DD` plus a time of day, as a whole `Date` value.
fn at(day: &str, time: &str) -> String {
    format!("{day} {time} UTC")
}

fn reconciled(memories: &Memories, files: Vec<MemoryFile>) -> Reconciliation {
    reconcile(memories, Discovery::from_files(files, Vec::new()))
}

fn manifest(dir: &TempDir, reconciliation: &Reconciliation) -> Manifest {
    let mut manifest = Manifest::open_in(dir.path().join("state"), &ExportId::new(EXPORT_ID).unwrap()).unwrap();
    reconciliation.enroll(&mut manifest).unwrap();
    manifest
}

/// Relative output paths, so an assertion reads as the tree a user would see.
fn outputs(plan: &Plan, out: &Path) -> Vec<String> {
    plan.items.iter().map(|item| item.output.strip_prefix(out).unwrap().to_string_lossy().replace('\\', "/")).collect()
}

// ---- the independent reader ----

/// Every tag `exiftool` reports for `path`, keyed by its bare name.
///
/// `-All` is needed alongside `-validate`, because naming any tag turns the run into a request for
/// only the tags named. `-a` keeps duplicate rows, `-s` gives the short tag id, and `-G0:1` puts
/// the group in front of it so `[EXIF:GPS] GPSLatitude` and `[Composite] GPSLatitude` are
/// distinguishable in the raw output. `None` means `exiftool` is not installed.
fn exiftool(path: &Path) -> Option<BTreeMap<String, String>> {
    let output = Command::new("exiftool").args(["-s", "-a", "-G0:1", "-validate", "-All"]).arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Some(
        text.lines()
            // `": "` rather than `':'`: the group prefix and every date value hold a colon, and
            // only the separator is followed by a space.
            .filter_map(|line| line.split_once(": "))
            .map(|(key, value)| {
                // `[EXIF:ExifIFD]  DateTimeOriginal` -> `DateTimeOriginal`.
                let name = key.rsplit(']').next().unwrap_or(key).trim().to_owned();
                (name, value.trim().to_owned())
            })
            .collect(),
    )
}

/// Set this and a missing `exiftool` fails the run instead of quietly covering nothing.
///
/// The notice below cannot be relied on: nextest captures a passing test's output, so on a box
/// without `exiftool` the suite prints nothing at all and reads as fully green. That is fine while
/// the only box this repo is gated on has 13.55 installed, and it is exactly wrong for a CI runner,
/// where these four tests are the sole independent-reader coverage of the EXIF, GPS and offset
/// encodings. CI sets this variable, so a runner missing the tool reds rather than skipping.
const REQUIRE_EXIFTOOL: &str = "EXPORTSNAP_REQUIRE_EXIFTOOL";

/// The same for ffmpeg, which every video test needs before it has a fixture at all.
///
/// A separate variable from the one above because the two tools cover different things and a
/// runner that has one and not the other should be told which is missing.
const REQUIRE_FFMPEG: &str = "EXPORTSNAP_REQUIRE_FFMPEG";

/// Records why a check did not run. Loud where the caller asked for loud.
fn skipped(test: &str) {
    skipped_for(test, "exiftool", REQUIRE_EXIFTOOL);
}

fn skipped_for(test: &str, tool: &str, variable: &str) {
    assert!(
        std::env::var_os(variable).is_none(),
        "{test}: {variable} is set and {tool} is not on PATH, so the assertions that need it would have been \
         skipped; install {tool} on this runner or unset the variable"
    );
    println!("SKIPPED {test}: {tool} is not on PATH, so its assertions did not run");
}

/// Transcoding on with a real ffmpeg, which is what [`VideoOptions::probe`] resolves to on the box
/// this repo is gated on. Built explicitly rather than probed so a test says which branch it is in.
fn transcoding() -> VideoOptions {
    VideoOptions { transcode: true, ffmpeg: Some(PathBuf::from("ffmpeg")) }
}

/// Transcoding off, the opt-out.
fn copying() -> VideoOptions {
    VideoOptions { transcode: false, ffmpeg: Some(PathBuf::from("ffmpeg")) }
}

/// The codec and pixel dimensions of a video's first stream, through `ffprobe`.
fn probe_video(path: &Path) -> Option<(String, u32, u32)> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height", "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let codec = lines.next()?.trim().to_owned();
    Some((codec, lines.next()?.trim().parse().ok()?, lines.next()?.trim().parse().ok()?))
}

/// The colour of one pixel of a video's first frame.
fn first_frame_pixel(path: &Path, x: u32, y: u32, width: u32) -> [u8; 3] {
    let dir = path.parent().unwrap();
    let raw = dir.join("frame.raw");
    let decoded = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24"])
        .arg(&raw)
        .output()
        .unwrap();
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    let bytes = fs::read(&raw).unwrap();
    let at = ((y * width + x) * 3) as usize;
    [bytes[at], bytes[at + 1], bytes[at + 2]]
}

// ---- the plan ----

#[test]
fn an_exact_bucket_takes_its_time_and_place_from_the_entry() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let files = vec![write_main(dir.path(), "2021-01-15", 1)];
    let reconciliation = reconciled(&memories, files);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);

    assert_eq!(plan.items.len(), 1);
    let item = &plan.items[0];
    assert_eq!(item.capture.source(), TimeSource::Entry);
    // Paris is UTC+1 in January, so 13:30:05 UTC is 14:30:05 on the wall.
    assert_eq!(item.capture.local(), NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(14, 30, 5).unwrap());
    assert_eq!(item.capture.offset().map(|offset| offset.local_minus_utc()), Some(3600));
    assert_eq!(item.location, Some(LocationPoint::parse(Field::Location, PARIS).unwrap()));
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_143005.jpg"]);
}

#[test]
fn an_agreeing_ambiguous_bucket_keeps_its_gps_and_a_disagreeing_one_loses_it() {
    let dir = TempDir::new().unwrap();
    // Two n:n buckets, both ambiguous by construction (two entries, two files, one day, one kind).
    // The only thing that differs between them is whether their entries name the same place, which
    // is the dimension decision 32 turns on.
    let memories = entries(&[
        (&at("2021-01-15", "01:00:00"), "Image", PARIS),
        (&at("2021-01-15", "23:00:00"), "Image", PARIS),
        (&at("2021-02-20", "01:00:00"), "Image", PARIS),
        (&at("2021-02-20", "23:00:00"), "Image", RIO),
    ]);
    let files = vec![
        write_main(dir.path(), "2021-01-15", 1),
        write_main(dir.path(), "2021-01-15", 2),
        write_main(dir.path(), "2021-02-20", 3),
        write_main(dir.path(), "2021-02-20", 4),
    ];
    let reconciliation = reconciled(&memories, files);
    let plan = Plan::build(&memories, &reconciliation, dir.path().join("out"));

    let paris = LocationPoint::parse(Field::Location, PARIS).unwrap();
    let located: Vec<Option<LocationPoint>> = plan.items.iter().map(|item| item.location).collect();
    assert_eq!(
        located,
        [Some(paris), Some(paris), None, None],
        "the agreeing bucket keeps the one place all its entries name; the disagreeing one stamps nothing"
    );

    // The time rule is the opposite one, and it applies to both buckets alike: nothing ambiguous
    // takes its time from an entry, however well the bucket agrees about where it was.
    for item in &plan.items {
        assert_ne!(item.capture.source(), TimeSource::Entry, "{}", item.source_id);
    }
}

#[test]
fn an_ambiguous_bucket_holding_one_entry_with_no_location_stamps_nothing() {
    let dir = TempDir::new().unwrap();
    // An entry with no location does not abstain, it splits the bucket: the file in hand might be
    // that entry's, and stamping its neighbour's coordinate onto it is the mistake being avoided.
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Image", PARIS), (&at("2021-01-15", "23:00:00"), "Image", "")]);
    let files = vec![write_main(dir.path(), "2021-01-15", 1), write_main(dir.path(), "2021-01-15", 2)];
    let reconciliation = reconciled(&memories, files);
    let plan = Plan::build(&memories, &reconciliation, dir.path().join("out"));

    assert_eq!(plan.items.iter().map(|item| item.location).collect::<Vec<_>>(), [None, None]);
}

#[test]
fn an_unpaired_entry_is_planned_for_nothing_while_a_video_gets_its_own_leg() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[
        (&at("2021-01-15", "01:00:00"), "Image", PARIS),
        (&at("2021-03-01", "01:00:00"), "Video", PARIS),
        (&at("2021-04-01", "01:00:00"), "Image", PARIS),
    ]);
    let files = vec![
        write_main(dir.path(), "2021-01-15", 1),
        // Not a real mp4, and it does not need to be: which leg an item lands on is decided from
        // the extension at plan time, and nothing opens the file until the run reaches it.
        write_raw(dir.path(), &format!("2021-03-01_{}-main.mp4", uuid(2)), b"not really an mp4"),
    ];
    let reconciliation = reconciled(&memories, files);
    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);

    assert_eq!(plan.items.iter().map(|item| item.leg).collect::<Vec<_>>(), [Leg::Image, Leg::Video]);
    assert!(plan.deferred.is_empty(), "a video is fixed by this pass now, not deferred out of it: {:?}", plan.deferred);
    // Both buckets are exact, so both take the entry's instant, moved into Paris local time.
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_020000.jpg", "2021/03/20210301_020000.mp4"]);
    // The unpaired third entry is neither planned nor deferred: it has no media to fix at all, and
    // `memories` already recorded it as source-missing.
    assert_eq!(plan.items.len(), 2);
}

#[test]
fn an_image_and_a_video_landing_on_one_second_both_keep_the_plain_name() {
    let dir = TempDir::new().unwrap();
    // Two exact buckets on one day, one per kind, so both take the same instant from their entry.
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS), (&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    let files = vec![
        write_main(dir.path(), "2021-01-15", 1),
        write_raw(dir.path(), &format!("2021-01-15_{}-main.mp4", uuid(2)), b"not really an mp4"),
    ];
    let reconciliation = reconciled(&memories, files);
    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);

    // Two files, one name, two extensions: nothing collides on disk, so nothing gets a suffix.
    // Counting per stem alone would hand the second one `_2` for a clash that cannot happen.
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_143005.jpg", "2021/01/20210115_143005.mp4"]);
}

#[test]
fn a_main_in_a_format_the_image_leg_does_not_read_is_deferred_rather_than_attempted() {
    let dir = TempDir::new().unwrap();
    // The entry's word and the file's extension both land in the `Unknown` bucket, which is the
    // only way a memory this build cannot decode pairs with an entry at all.
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "SHARE", PARIS)]);
    let files = vec![write_raw(dir.path(), &format!("2021-01-15_{}-main.heic", uuid(1)), b"\x00\x00\x00\x18ftypheic")];
    let reconciliation = reconciled(&memories, files);
    let plan = Plan::build(&memories, &reconciliation, dir.path().join("out"));

    assert!(plan.items.is_empty());
    assert_eq!(plan.deferred.iter().map(|one| one.reason).collect::<Vec<_>>(), [DeferralReason::UnknownFormat]);
}

#[test]
fn memories_landing_on_one_second_get_counted_names_rather_than_overwriting_each_other() {
    let dir = TempDir::new().unwrap();
    // Three ambiguous entries on one day: none may take its time from its entry, so all three fall
    // back to the filename's midnight and want the same output name.
    let memories = entries(&[
        (&at("2021-01-15", "01:00:00"), "Image", PARIS),
        (&at("2021-01-15", "02:00:00"), "Image", PARIS),
        (&at("2021-01-15", "03:00:00"), "Image", PARIS),
    ]);
    let files = (1..=3).map(|seed| write_main(dir.path(), "2021-01-15", seed)).collect();
    let reconciliation = reconciled(&memories, files);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_000000.jpg", "2021/01/20210115_000000_2.jpg", "2021/01/20210115_000000_3.jpg"]);

    // Re-planning the same input hands out the same names. That is what makes a resume safe: the
    // suffix is a position in the plan, never the next free slot on disk.
    let again = Plan::build(&memories, &reconciliation, &out);
    assert_eq!(outputs(&again, &out), outputs(&plan, &out));
}

#[test]
fn a_file_whose_own_metadata_dates_it_uses_that_before_falling_back_to_its_filename() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Image", PARIS), (&at("2021-01-15", "23:00:00"), "Image", PARIS)]);
    let files = vec![write_main(dir.path(), "2021-01-15", 1), write_main(dir.path(), "2021-01-15", 2)];

    // Give the first file a capture time of its own. Ambiguous items may not read the entry's
    // time, so this is the only thing that can move them off midnight.
    let mut stamped = Jpeg::read(&files[0].path).unwrap();
    stamped
        .stamp(&Stamp {
            local: NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(9, 17, 42).unwrap(),
            offset: None,
            location: None,
            width: WIDTH,
            height: HEIGHT,
        })
        .unwrap();
    stamped.write(&files[0].path).unwrap();

    let reconciliation = reconciled(&memories, files);
    let plan = Plan::build(&memories, &reconciliation, dir.path().join("out"));

    let sources: Vec<TimeSource> = plan.items.iter().map(|item| item.capture.source()).collect();
    assert_eq!(sources, [TimeSource::Embedded, TimeSource::Filename]);
    assert_eq!(plan.items[0].capture.local(), NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(9, 17, 42).unwrap());
    assert_eq!(plan.items[1].capture.local(), NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(0, 0, 0).unwrap());
}

// ---- the fix ----

#[test]
fn a_fixed_memory_lands_under_its_year_and_month_carrying_the_overlay_and_the_derived_date() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let files = vec![write_main(dir.path(), "2021-01-15", 1), write_overlay(dir.path(), "2021-01-15", 1)];
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 1);
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    let written = out.join("2021/01/20210115_143005.jpg");
    assert!(written.is_file(), "the year and month directories are the rename");

    // The overlay was actually drawn: its opaque half is red and its transparent half is not.
    let composite = image::open(&written).unwrap().to_rgb8();
    assert_eq!(composite.dimensions(), (WIDTH, HEIGHT));
    assert!(composite.get_pixel(2, 2).0[0] > 200, "the overlay's opaque half reached the composite");
    assert!(composite.get_pixel(WIDTH - 2, 2).0[0] < 60, "the overlay's transparent half left the main showing");

    // The file's own date is the derived instant, not today.
    let modified = fs::metadata(&written).unwrap().modified().unwrap();
    let expected = UNIX_EPOCH + Duration::from_secs(u64::try_from(plan.items[0].capture.instant().timestamp()).unwrap());
    assert_eq!(modified, expected);
}

#[test]
fn a_main_with_no_overlay_is_copied_byte_for_byte_rather_than_re_encoded() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let main = write_main(dir.path(), "2021-01-15", 1);
    let source_pixels = image::open(&main.path).unwrap().to_rgb8();
    let reconciliation = reconciled(&memories, vec![main]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    // Only the metadata segment changed, so decoding gives back the identical pixels. A re-encode
    // at any quality would move at least one of them.
    let written = image::open(&plan.items[0].output).unwrap().to_rgb8();
    assert_eq!(written.as_raw(), source_pixels.as_raw(), "an untouched main must not pay a generation of jpeg loss");
}

#[test]
fn a_write_that_shrinks_leaves_no_tail_of_whatever_was_at_the_output_path() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let reconciliation = reconciled(&memories, vec![write_main(dir.path(), "2021-01-15", 1)]);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let output = plan.items[0].output.clone();

    // Something much larger is already sitting where this item is about to land — the shape a
    // `write_to_file` that never truncates leaves behind, and the shape a re-run over a previous
    // larger output would hit. `little_exif`'s own file writer seeks to zero and writes without
    // `set_len`, so everything past the new length would survive, still greppable.
    const MARKER: &[u8] = b"PREVIOUS-PAYLOAD-THAT-MUST-NOT-SURVIVE";
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    let stale: Vec<u8> = MARKER.iter().copied().cycle().take(512 * 1024).collect();
    fs::write(&output, &stale).unwrap();

    local_fix::fix(&plan.items[0], &transcoding()).unwrap();

    let written = fs::read(&output).unwrap();
    assert!(written.len() < stale.len(), "the fixture only tests truncation if the new write is the smaller one");
    assert!(!written.windows(MARKER.len()).any(|window| window == MARKER), "the previous payload is still readable on disk");
    assert_eq!(fs::metadata(&output).unwrap().len(), written.len() as u64);
}

#[test]
fn a_run_that_already_finished_an_item_does_not_touch_it_again() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let reconciliation = reconciled(&memories, vec![write_main(dir.path(), "2021-01-15", 1)]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let first = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();
    assert_eq!((first.fixed, first.skipped), (1, 0));

    let source_id = plan.items[0].source_id.clone();
    assert_eq!(manifest.item(ItemKind::Memory, &source_id).unwrap().unwrap().status, ItemStatus::Done);

    let second = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();
    assert_eq!((second.fixed, second.skipped), (0, 1), "a resume skips what the manifest verified");
    assert_eq!(second.resumed.verified, 1);
}

#[test]
fn an_output_that_changed_since_it_was_recorded_is_redone_rather_than_trusted() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let reconciliation = reconciled(&memories, vec![write_main(dir.path(), "2021-01-15", 1)]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    fs::remove_file(&plan.items[0].output).unwrap();
    let second = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(second.resumed.demoted.len(), 1);
    assert_eq!(second.fixed, 1, "the demoted item is offered again and finished");
    assert!(plan.items[0].output.is_file());
}

#[test]
fn a_memory_that_cannot_be_fixed_is_recorded_against_its_own_row_and_the_rest_of_the_run_carries_on() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Image", PARIS), (&at("2021-01-15", "23:00:00"), "Image", PARIS)]);
    // A main claiming to be a jpeg that is a png underneath: the verbatim copy path hands its
    // bytes straight on, and the guard is what stops them.
    let mut png = Vec::new();
    RgbaImage::new(WIDTH, HEIGHT).write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png).unwrap();
    let files = vec![write_raw(dir.path(), &format!("2021-01-15_{}-main.jpg", uuid(1)), &png), write_main(dir.path(), "2021-01-15", 2)];
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 1, "the healthy memory is still fixed");
    assert_eq!(report.failed.len(), 1);
    // Named exactly, not by a substring both refusals happen to share: a png is refused on its
    // SIGNATURE, and the truncated-chain case next door is refused on its STRUCTURE. An assertion
    // loose enough to accept either would let the two tests drift into pinning one thing twice.
    assert!(report.failed[0].reason.contains("start-of-image marker"), "{}", report.failed[0].reason);
    // The leading bytes are named, so the message says what the file actually was. `contains`
    // rather than `starts_with` because the path is prefixed in front of it.
    assert!(report.failed[0].reason.contains("not a jpeg: starts with 89 50"), "{}", report.failed[0].reason);

    let failed = manifest.item(ItemKind::Memory, &report.failed[0].source_id).unwrap().unwrap();
    assert_eq!(failed.status, ItemStatus::Failed);
    assert_eq!(failed.retry_count, 1);
}

// ---- the guard on `little_exif` ----

#[test]
fn nothing_but_jpeg_bytes_can_be_handed_to_the_metadata_writer() {
    // The constructor is the only way to build the type the stamping API takes, so every one of
    // these is a shape that cannot reach `little_exif` at all. The png case is the one that
    // matters: RUSTSEC-2026-0194 lives on that crate's png write path.
    for (name, bytes) in [
        ("png", b"\x89PNG\r\n\x1a\n".to_vec()),
        ("gif", b"GIF89a".to_vec()),
        ("tiff little endian", b"II\x2a\x00".to_vec()),
        ("tiff big endian", b"MM\x00\x2a".to_vec()),
        ("webp", b"RIFF\x00\x00\x00\x00WEBP".to_vec()),
        ("mp4", b"\x00\x00\x00\x18ftypisom".to_vec()),
        ("bare soi with no marker after it", vec![0xff, 0xd8]),
        ("empty", Vec::new()),
        // Opens with a real start-of-image marker, so a signature prefix test admits all three.
        ("soi then a segment longer than the file", vec![0xff, 0xd8, 0xff, 0xe0, 0xff, 0xff]),
        ("soi then a marker with no length at all", vec![0xff, 0xd8, 0xff, 0xe1]),
        ("soi then an impossible segment length", vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x01]),
    ] {
        assert!(Jpeg::new(bytes).is_err(), "{name} must not become a Jpeg");
    }

    // A whole minimal JPEG: start-of-image, an eight-byte APP0, the scan. Not a four-byte prefix,
    // because the constructor walks the chain and a prefix is not one.
    let mut minimal = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x08, 0, 0, 0, 0, 0, 0];
    minimal.extend([0xff, 0xda, 0x00, 0x02]);
    assert!(Jpeg::new(minimal).is_ok());

    // A real encoder's output is the case that actually has to keep working.
    let mut encoded = Vec::new();
    RgbImage::new(WIDTH, HEIGHT).write_to(&mut std::io::Cursor::new(&mut encoded), ImageFormat::Jpeg).unwrap();
    assert!(Jpeg::new(encoded).is_ok(), "the guard must not refuse what this build's own encoder writes");
}

#[test]
fn a_png_main_is_transcoded_so_the_metadata_writer_still_only_ever_sees_jpeg() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);

    let mut pixels = RgbaImage::new(WIDTH, HEIGHT);
    for pixel in pixels.pixels_mut() {
        *pixel = Rgba([10, 200, 30, 255]);
    }
    let path = memories_dir(dir.path()).join(format!("2021-01-15_{}-main.png", uuid(1)));
    pixels.save_with_format(&path, ImageFormat::Png).unwrap();

    let reconciliation = reconciled(&memories, vec![MemoryFile::parse(path).unwrap()]);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 1, "{:?}", report.failed);
    let written = &plan.items[0].output;
    assert_eq!(written.extension().unwrap(), "jpg");
    assert_eq!(&fs::read(written).unwrap()[..3], &[0xff, 0xd8, 0xff], "the stamped output is a jpeg, never the png that went in");
}

#[test]
fn a_main_whose_existing_metadata_cannot_be_read_fails_instead_of_being_replaced() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let main = write_main(dir.path(), "2021-01-15", 1);

    // An APP1 that IS EXIF (it carries the `Exif\0\0` header) and whose payload is garbage where
    // the byte-order mark belongs. This is the case the marker walk exists to separate from "no
    // metadata at all": both come back from the library as the same `io::ErrorKind`, and only one
    // of them may be answered by starting from an empty `Metadata` — doing that here would delete
    // whatever the segment held and report success.
    let original = fs::read(&main.path).unwrap();
    let mut damaged = original[..2].to_vec();
    damaged.extend([0xff, 0xe1, 0x00, 0x10]);
    damaged.extend(b"Exif\0\0");
    damaged.extend(b"NOT-TIFF");
    damaged.extend(&original[2..]);
    fs::write(&main.path, &damaged).unwrap();

    let reconciliation = reconciled(&memories, vec![main]);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 0);
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].reason.contains("cannot read"), "{}", report.failed[0].reason);
    assert!(!plan.items[0].output.exists(), "a failure before the write must leave no output behind");
    assert_eq!(fs::read(&plan.items[0].media.main.path).unwrap(), damaged, "the source is read-only to this pass, damaged or not");
}

#[test]
fn a_main_whose_marker_chain_is_truncated_is_refused_with_its_own_message() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let main = write_main(dir.path(), "2021-01-15", 1);

    // Opens with a real start-of-image marker and then claims a segment far longer than the file:
    // a truncated download. A signature prefix test admits this and lets the library fail on it.
    fs::write(&main.path, [0xff, 0xd8, 0xff, 0xe0, 0xff, 0xff, 0x00]).unwrap();

    let reconciliation = reconciled(&memories, vec![main]);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].reason.contains("truncated or corrupt"), "{}", report.failed[0].reason);
    assert!(!plan.items[0].output.exists());
}

// ---- read back through exiftool ----

#[test]
fn the_stamped_output_reads_back_correctly_through_an_independent_reader() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let files = vec![write_main(dir.path(), "2021-01-15", 1), write_overlay(dir.path(), "2021-01-15", 1)];
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1);

    let Some(tags) = exiftool(&plan.items[0].output) else {
        skipped("the_stamped_output_reads_back_correctly_through_an_independent_reader");
        return;
    };

    assert_eq!(tags.get("Validate").map(String::as_str), Some("OK"), "{tags:#?}");
    assert_eq!(tags.get("DateTimeOriginal").map(String::as_str), Some("2021:01:15 14:30:05"));
    assert_eq!(tags.get("CreateDate").map(String::as_str), Some("2021:01:15 14:30:05"));
    assert_eq!(tags.get("OffsetTimeOriginal").map(String::as_str), Some("+01:00"));
    assert_eq!(tags.get("GPSLatitudeRef").map(String::as_str), Some("North"));
    assert_eq!(tags.get("GPSLongitudeRef").map(String::as_str), Some("East"));
    assert_eq!(tags.get("ExifImageWidth").map(String::as_str), Some(&*WIDTH.to_string()));
    assert_eq!(tags.get("ExifImageHeight").map(String::as_str), Some(&*HEIGHT.to_string()));
    // exiftool renders a coordinate as degrees/minutes/seconds; the whole degrees are what a
    // dropped or swapped component would move.
    assert!(tags.get("GPSLatitude").is_some_and(|value| value.starts_with("48 deg 51")), "{tags:#?}");
    assert!(tags.get("GPSLongitude").is_some_and(|value| value.starts_with("2 deg 17")), "{tags:#?}");
}

#[test]
fn a_southern_and_western_coordinate_reads_back_with_the_right_hemispheres() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", RIO)]);
    let reconciliation = reconciled(&memories, vec![write_main(dir.path(), "2021-01-15", 1)]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1);

    let Some(tags) = exiftool(&plan.items[0].output) else {
        skipped("a_southern_and_western_coordinate_reads_back_with_the_right_hemispheres");
        return;
    };

    assert_eq!(tags.get("Validate").map(String::as_str), Some("OK"), "{tags:#?}");
    assert_eq!(tags.get("GPSLatitudeRef").map(String::as_str), Some("South"));
    assert_eq!(tags.get("GPSLongitudeRef").map(String::as_str), Some("West"));
    // Rio is UTC-3 all year, so 13:30:05 UTC is 10:30:05 on the wall.
    assert_eq!(tags.get("DateTimeOriginal").map(String::as_str), Some("2021:01:15 10:30:05"));
    assert_eq!(tags.get("OffsetTimeOriginal").map(String::as_str), Some("-03:00"));
}

#[test]
fn an_ambiguous_buckets_gps_verdict_survives_all_the_way_into_the_written_file() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[
        (&at("2021-01-15", "01:00:00"), "Image", PARIS),
        (&at("2021-01-15", "23:00:00"), "Image", PARIS),
        (&at("2021-02-20", "01:00:00"), "Image", PARIS),
        (&at("2021-02-20", "23:00:00"), "Image", RIO),
    ]);
    let files = (1..=4).map(|seed| write_main(dir.path(), if seed <= 2 { "2021-01-15" } else { "2021-02-20" }, seed)).collect();
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 4);

    let Some(agreeing) = exiftool(&plan.items[0].output) else {
        skipped("an_ambiguous_buckets_gps_verdict_survives_all_the_way_into_the_written_file");
        return;
    };
    let disagreeing = exiftool(&plan.items[2].output).unwrap();

    assert_eq!(agreeing.get("Validate").map(String::as_str), Some("OK"), "{agreeing:#?}");
    assert!(agreeing.contains_key("GPSLatitude"), "the agreeing bucket's file carries a coordinate: {agreeing:#?}");
    assert_eq!(disagreeing.get("Validate").map(String::as_str), Some("OK"), "{disagreeing:#?}");
    assert!(!disagreeing.contains_key("GPSLatitude"), "the disagreeing bucket's file must carry none: {disagreeing:#?}");
    assert!(!disagreeing.contains_key("GPSPosition"), "{disagreeing:#?}");
}

#[test]
fn metadata_the_source_already_carried_survives_the_stamp() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let main = write_main(dir.path(), "2021-01-15", 1);

    let Some(exiftool_path) = which_exiftool() else {
        skipped("metadata_the_source_already_carried_survives_the_stamp");
        return;
    };
    // A foreign tag this build never writes, put there by the independent tool rather than by the
    // crate under test, so its survival is not the crate agreeing with itself.
    let status = Command::new(exiftool_path)
        .args(["-overwrite_original", "-Artist=A Foreign Writer", "-Make=SomeCamera"])
        .arg(&main.path)
        .status()
        .unwrap();
    assert!(status.success());

    let reconciliation = reconciled(&memories, vec![main]);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1);

    let tags = exiftool(&plan.items[0].output).unwrap();
    assert_eq!(tags.get("Artist").map(String::as_str), Some("A Foreign Writer"), "{tags:#?}");
    assert_eq!(tags.get("Make").map(String::as_str), Some("SomeCamera"), "{tags:#?}");
    assert_eq!(tags.get("DateTimeOriginal").map(String::as_str), Some("2021:01:15 14:30:05"));
}

fn which_exiftool() -> Option<&'static str> {
    Command::new("exiftool").arg("-ver").output().ok().filter(|output| output.status.success()).map(|_| "exiftool")
}

// ---- the video leg ----

#[test]
fn a_transcoding_run_re_encodes_a_memory_video_to_h264_and_dates_it() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    let Some(video) = write_video(dir.path(), "2021-01-15", 1) else {
        skipped_for("a_transcoding_run_re_encodes_a_memory_video_to_h264_and_dates_it", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let source = fs::read(&video.path).unwrap();
    let reconciliation = reconciled(&memories, vec![video]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 1, "{:?}", report.failed);
    assert!(report.notices.is_empty(), "a full transcode has nothing to report: {:?}", report.notices);

    let written = out.join("2021/01/20210115_143005.mp4");
    assert!(written.is_file(), "the year and month directories are the rename, and video keeps its own extension");
    // The whole reason the transcode is on by default: `hvc1` in, something Windows plays out.
    assert_eq!(probe_video(&written), Some(("h264".to_owned(), WIDTH, HEIGHT)));
    assert_ne!(fs::read(&written).unwrap(), source, "the pixels were re-encoded, so the bytes cannot match");
    // The source is read-only to this pass, transcode or not.
    assert_eq!(fs::read(&plan.items[0].media.main.path).unwrap(), source);

    // The file's own date is the derived instant, exactly as on the image leg.
    let modified = fs::metadata(&written).unwrap().modified().unwrap();
    let expected = UNIX_EPOCH + Duration::from_secs(u64::try_from(plan.items[0].capture.instant().timestamp()).unwrap());
    assert_eq!(modified, expected);

    // Nothing of the scratch file a transcode writes into is left behind.
    let leftovers: Vec<PathBuf> = fs::read_dir(written.parent().unwrap())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path != &written)
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_run_that_is_not_transcoding_copies_the_pixels_and_still_writes_the_metadata() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    let Some(video) = write_video(dir.path(), "2021-01-15", 1) else {
        skipped_for("a_run_that_is_not_transcoding_copies_the_pixels_and_still_writes_the_metadata", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let reconciliation = reconciled(&memories, vec![video]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &copying()).unwrap();

    assert_eq!(report.fixed, 1, "{:?}", report.failed);
    let written = &plan.items[0].output;
    // Still HEVC: nothing re-encoded a frame. That is the whole contract of the opt-out.
    assert_eq!(probe_video(written), Some(("hevc".to_owned(), WIDTH, HEIGHT)));
    // And the run says so rather than leaving the user to notice.
    assert_eq!(
        report.notices.iter().map(|one| one.notice).collect::<Vec<_>>(),
        [Notice::NotTranscoded(TranscodeSkip::OptedOut)],
        "{:?}",
        report.notices
    );
    assert_eq!(report.notices[0].source_id, plan.items[0].source_id);

    // The metadata still landed, because metadata never goes through ffmpeg on either route.
    let probed = ffprobe_format(written).unwrap();
    assert_eq!(probed.get("creation_time").map(String::as_str), Some("2021-01-15T13:30:05.000000Z"));
    assert_eq!(probed.get("date").map(String::as_str), Some("2021-01-15T14:30:05+01:00"), "Paris is UTC+1 in January");
    assert_eq!(probed.get("location").map(String::as_str), Some("+48.858844+002.294351/"));
}

#[test]
fn an_overlay_is_burned_in_only_by_a_transcode_and_the_run_says_when_it_was_not() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    let Some(video) = write_video(dir.path(), "2021-01-15", 1) else {
        skipped_for("an_overlay_is_burned_in_only_by_a_transcode_and_the_run_says_when_it_was_not", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    // Half the size of the frame, so the scaling leg is exercised rather than skipped: a memory's
    // overlay is normally full-frame and a burn that could not scale would still pass without this.
    let mut pixels = RgbaImage::new(WIDTH / 2, HEIGHT / 2);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < WIDTH / 4 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 0, 0]) };
    }
    let overlay_path = memories_dir(dir.path()).join(format!("2021-01-15_{}-overlay.png", uuid(1)));
    pixels.save_with_format(&overlay_path, ImageFormat::Png).unwrap();
    let overlay = MemoryFile::parse(overlay_path).unwrap();

    let reconciliation = reconciled(&memories, vec![video, overlay]);
    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    assert!(plan.items[0].media.overlay.is_some(), "the fixture must actually pair an overlay");

    // Transcoding: the caption is drawn, scaled up to the frame, and nothing is reported.
    let mut burning = manifest(&dir, &reconciliation);
    assert_eq!(local_fix::run(&plan, &mut burning, 3, &transcoding()).unwrap().fixed, 1);
    let burned = &plan.items[0].output;
    assert!(first_frame_pixel(burned, 2, 2, WIDTH)[0] > 200, "the overlay's opaque half reached the frame");
    assert!(first_frame_pixel(burned, WIDTH - 2, 2, WIDTH)[2] > 150, "the overlay's transparent half left the video showing");

    // Not transcoding: the same input keeps its pixels and the run names both things it skipped.
    let plain = TempDir::new().unwrap();
    let mut second = manifest(&plain, &reconciliation);
    let out = plain.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut second, 3, &copying()).unwrap();

    assert_eq!(report.fixed, 1, "{:?}", report.failed);
    assert_eq!(
        report.notices.iter().map(|one| one.notice).collect::<Vec<_>>(),
        [Notice::NotTranscoded(TranscodeSkip::OptedOut), Notice::OverlayNotBurned],
        "burning a caption in IS a re-encode, so a run that does not transcode cannot draw one"
    );
    let copied = &plan.items[0].output;
    assert!(first_frame_pixel(copied, 2, 2, WIDTH)[0] < 60, "no caption was drawn");
    assert!(first_frame_pixel(copied, 2, 2, WIDTH)[2] > 150, "the original frame is what is there");
}

#[test]
fn a_run_with_no_ffmpeg_finishes_every_video_and_reports_what_it_could_not_do() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    // The one video test built in pure Rust, and deliberately so: this is the ONLY test describing
    // what happens on a machine with no ffmpeg, and an ffmpeg-built fixture would make it skip
    // itself on exactly that machine. Nothing here needs real pixels — with `ffmpeg: None` no frame
    // is re-encoded, so every assertion below is about the container and the manifest.
    let video = write_synthetic_video(dir.path(), "2021-01-15", 1);
    let reconciliation = reconciled(&memories, vec![video]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    // Decision 2's degrade: an optional tool being absent costs a capability, never the run. The
    // reason is distinguishable from a user turning transcoding off, because the fixes differ.
    let report = local_fix::run(&plan, &mut manifest, 3, &VideoOptions { transcode: true, ffmpeg: None }).unwrap();

    assert_eq!(report.fixed, 1, "{:?}", report.failed);
    assert!(report.failed.is_empty());
    assert_eq!(report.notices.iter().map(|one| one.notice).collect::<Vec<_>>(), [Notice::NotTranscoded(TranscodeSkip::NoFfmpeg)]);
    assert!(report.notices[0].notice.to_string().contains("ffmpeg is not installed"), "{}", report.notices[0].notice);
    assert!(plan.items[0].output.is_file(), "the video is still dated and copied, it is just not re-encoded");
    assert_eq!(manifest.item(ItemKind::Memory, &plan.items[0].source_id).unwrap().unwrap().status, ItemStatus::Done);
}

#[test]
fn a_video_whose_date_cannot_be_stored_leaves_the_output_path_alone() {
    let dir = TempDir::new().unwrap();
    // 1965: a legal raw value against MP4's 1904 epoch, and one both readers show as a date in the
    // 2030s, so it is refused rather than written. Reachable from an entry date alone.
    let memories = entries(&[(&at("1965-06-01", "12:00:00"), "Video", PARIS)]);
    let Some(video) = write_video(dir.path(), "1965-06-01", 1) else {
        skipped_for("a_video_whose_date_cannot_be_stored_leaves_the_output_path_alone", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let reconciliation = reconciled(&memories, vec![video]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let output = plan.items[0].output.clone();

    // Something already sitting where this item would land. The all-or-nothing property is that a
    // failure before the write touches nothing at all, and it is structural — the patch is a `Vec`
    // and the write runs on `Ok` — so a refactor that streamed patches at the file would eat this.
    const MARKER: &[u8] = b"NOTHING-MAY-OVERWRITE-THIS";
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, MARKER).unwrap();

    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 0);
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].reason.contains("before 1970"), "{}", report.failed[0].reason);
    assert_eq!(fs::read(&output).unwrap(), MARKER, "a failed item wrote over the path anyway");
    assert_eq!(manifest.item(ItemKind::Memory, &report.failed[0].source_id).unwrap().unwrap().status, ItemStatus::Failed);
}

#[test]
fn a_video_that_is_not_one_is_recorded_against_its_own_row_and_the_run_carries_on() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Video", PARIS), (&at("2021-01-15", "23:00:00"), "Video", PARIS)]);
    let Some(healthy) = write_video(dir.path(), "2021-01-15", 2) else {
        skipped_for("a_video_that_is_not_one_is_recorded_against_its_own_row_and_the_run_carries_on", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    // A `.mp4` that is a JPEG underneath. Nothing re-encodes it, so the guard is what stops it.
    let liar = write_raw(dir.path(), &format!("2021-01-15_{}-main.mp4", uuid(1)), &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]);
    let reconciliation = reconciled(&memories, vec![liar, healthy]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    // Transcoding off, so the refusal is this crate's guard rather than ffmpeg's decoder: those are
    // two different messages and only one of them names what the file actually is.
    let report = local_fix::run(&plan, &mut manifest, 3, &copying()).unwrap();

    assert_eq!(report.fixed, 1, "the healthy memory is still fixed");
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].reason.contains("not an mp4: its first box is"), "{}", report.failed[0].reason);
    let failed = manifest.item(ItemKind::Memory, &report.failed[0].source_id).unwrap().unwrap();
    assert_eq!(failed.status, ItemStatus::Failed);
    assert_eq!(failed.retry_count, 1);
}

#[test]
fn a_video_ffmpeg_cannot_decode_fails_that_item_alone_and_keeps_its_message() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Video", PARIS), (&at("2021-01-15", "23:00:00"), "Video", PARIS)]);
    let Some(healthy) = write_video(dir.path(), "2021-01-15", 2) else {
        skipped_for("a_video_ffmpeg_cannot_decode_fails_that_item_alone_and_keeps_its_message", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let liar = write_raw(dir.path(), &format!("2021-01-15_{}-main.mp4", uuid(1)), b"not really an mp4 at all");
    let reconciliation = reconciled(&memories, vec![liar, healthy]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 1, "one bad file out of two must not cost the other");
    assert_eq!(report.failed.len(), 1);
    // ffmpeg's own words, not a message this crate invented for it: a user who has to fix an
    // encoding problem needs what the encoder said.
    assert!(report.failed[0].reason.contains("ffmpeg could not re-encode"), "{}", report.failed[0].reason);
    assert!(!plan.items[0].output.exists(), "a failed transcode leaves no output");
    // And no scratch file either, however the item ended.
    let leftovers: Vec<PathBuf> =
        fs::read_dir(plan.items[0].output.parent().unwrap()).unwrap().filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    assert_eq!(leftovers, [plan.items[1].output.clone()], "{leftovers:?}");
}

#[test]
fn a_video_whose_time_falls_back_reads_its_own_movie_header_before_its_filename() {
    let dir = TempDir::new().unwrap();
    // Two ambiguous entries on one day: neither may take its time from an entry, so the only thing
    // that can move one off midnight is the file's own header.
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Video", PARIS), (&at("2021-01-15", "23:00:00"), "Video", PARIS)]);
    let Some(dated) = write_video(dir.path(), "2021-01-15", 1) else {
        skipped_for("a_video_whose_time_falls_back_reads_its_own_movie_header_before_its_filename", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let undated = write_video(dir.path(), "2021-01-15", 2).unwrap();

    // Give the first one a header time of its own, through this crate's own writer, since ffmpeg
    // leaves those fields zeroed unless told otherwise.
    let mut stamped = exportsnap::export::video::Mp4::read(&dated.path).unwrap();
    stamped
        .stamp(&exportsnap::export::video::VideoStamp {
            local: NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(9, 17, 42).unwrap(),
            offset: None,
            location: None,
        })
        .unwrap();
    stamped.write(&dated.path).unwrap();

    let reconciliation = reconciled(&memories, vec![dated, undated]);
    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);

    assert_eq!(plan.items.iter().map(|item| item.capture.source()).collect::<Vec<_>>(), [TimeSource::Embedded, TimeSource::Filename]);
    // The header holds a UTC instant with no offset field, so Paris moves it an hour forward on the
    // wall — the same conversion an entry's own `Date` goes through.
    assert_eq!(plan.items[0].capture.local(), NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(10, 17, 42).unwrap());
    assert_eq!(plan.items[1].capture.local(), NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(0, 0, 0).unwrap());
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_101742.mp4", "2021/01/20210115_000000.mp4"]);
}

#[test]
fn a_finished_video_is_not_transcoded_again_on_a_resume() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    let Some(video) = write_video(dir.path(), "2021-01-15", 1) else {
        skipped_for("a_finished_video_is_not_transcoded_again_on_a_resume", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let reconciliation = reconciled(&memories, vec![video]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = Plan::build(&memories, &reconciliation, &out);
    let first = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();
    assert_eq!((first.fixed, first.skipped), (1, 0));
    let finished = fs::read(&plan.items[0].output).unwrap();

    let second = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();
    assert_eq!((second.fixed, second.skipped), (0, 1), "a resume skips what the manifest verified");
    assert_eq!(second.resumed.verified, 1);
    // Re-encoding is not deterministic byte for byte across runs, so an identical file is the
    // strongest evidence that ffmpeg was never invoked a second time.
    assert_eq!(fs::read(&plan.items[0].output).unwrap(), finished);
}

/// The container-level tags `ffprobe` reports, keyed by name.
fn ffprobe_format(path: &Path) -> Option<BTreeMap<String, String>> {
    let output =
        Command::new("ffprobe").args(["-v", "error", "-show_entries", "format_tags", "-of", "default=nw=1"]).arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Some(
        text.lines()
            .filter_map(|line| line.strip_prefix("TAG:"))
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
    )
}
