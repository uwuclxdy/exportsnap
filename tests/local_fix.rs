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
//! dependency, so a test needing one asks `tests/common`'s shared gate up front and prints a skip
//! notice when it is not usable — the box this repo is gated on has them, and the phase-5 CI leg
//! has to install them or the coverage silently disappears. Everything a byte-level assertion can
//! cover is asserted unconditionally instead.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use chrono::NaiveDate;
use common::Tool;
use exportsnap::export::exif::{Jpeg, Stamp};
use exportsnap::export::local_fix::{self, DeferralReason, Leg, Notice, Plan, RecordedOutputs, TimeSource, TranscodeSkip, VideoOptions};
use exportsnap::export::manifest::{Checksum, DemotionReason, ExportId, ItemKind, ItemStatus, Manifest};
use exportsnap::export::memories::{Discovery, MemoryFile, Reconciliation, reconcile};
use exportsnap::export::model::{Field, LocationPoint, Memories};
use exportsnap::export::schema;
use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use tempfile::TempDir;

mod common;

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

/// The colour [`write_main`] paints at `(x, y)`, which is what a transparent overlay region has to
/// leave showing.
fn main_colour(x: u32, y: u32) -> [u8; 3] {
    [(x % 7) as u8 * 5, ((x * 13 + y * 7) % 251) as u8, ((x * 29 + y * 17) % 253) as u8]
}

/// Asserts the overlay's transparent region left the MAIN showing through, on all three channels.
///
/// **What used to stand here was a brightness threshold on subpixel 0, and it could not discriminate
/// at all.** The main's red in the asserted region is 20-30 against black's 0, so `< 60` passed
/// whether the transparent half showed the main through or the alpha had been dropped to black —
/// the fixture held the asserted channel near-constant across the two outcomes, which is this repo's
/// own recorded trap. Green and blue separate them, and matching all three also reds on a composite
/// onto any invented background rather than only on black.
///
/// **Asserted as a block MEAN rather than as one pixel, and that is the load-bearing half.** The
/// main fixture is high-frequency by design — `write_main`'s doc says so, because the byte-for-byte
/// copy test needs it — and JPEG's DCT smears neighbours: measured on these fixtures, a lone pixel
/// drifts up to 21 levels across the two generations they carry, which is close enough to the gap
/// being detected that a per-pixel tolerance would be guessing. A block mean is essentially the DC
/// coefficient, which JPEG preserves closely, so the comparison is tight and the margin to the
/// failure it must catch is about 125 on the chroma channels instead of single digits.
fn assert_shows_main_through(composite: &RgbImage, label: &str) {
    /// Comfortably above the drift a preserved block mean shows and far below the ~125 that
    /// separates the main from black on green and blue.
    const TOLERANCE: f64 = 8.0;

    let [left, top, right, bottom] = TRANSPARENT_BLOCK;
    let count = f64::from((right - left) * (bottom - top));
    let mut actual = [0.0; 3];
    let mut expected = [0.0; 3];
    for y in top..bottom {
        for x in left..right {
            let painted = main_colour(x, y);
            for channel in 0..3 {
                actual[channel] += f64::from(composite.get_pixel(x, y).0[channel]) / count;
                expected[channel] += f64::from(painted[channel]) / count;
            }
        }
    }
    for channel in 0..3 {
        let drift = actual[channel] - expected[channel];
        assert!(
            drift.abs() <= TOLERANCE,
            "{label}: channel {channel} averaged {actual:?} over {TRANSPARENT_BLOCK:?}, expected about {expected:?}"
        );
    }
}

/// The eight bytes every PNG opens with, so an output asserted to be one is checked against the
/// container rather than against the name it was given.
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A block wholly inside the overlay's TRANSPARENT half and away from its edge. Shared with
/// [`assert_shows_main_through`]'s own block for the same reason it picked those coordinates.
const TRANSPARENT_BLOCK: [u32; 4] = [48, 8, 56, 16];

/// A block wholly inside the overlay's OPAQUE half and away from its edge, where a composite that
/// ran paints the caption's red.
const OPAQUE_BLOCK: [u32; 4] = [8, 8, 16, 16];

/// The mean of one channel of an RGBA image over a block.
///
/// **A block mean rather than a lone subpixel, which is this repo's own recorded fix.** A single
/// pixel threshold passed for both the correct result and the failure it existed to catch, because
/// the fixture held the asserted channel near-constant across the two. Nothing about that trap is
/// codec-specific, so the shape is kept even where the codec is lossless: on the PNG path the mean
/// is exact, and the tolerance the callers pass exists to red a PARTIAL regression — a half-blended
/// composite, or an encoder that dropped alpha over part of the block — rather than to absorb drift
/// that cannot happen here.
fn block_mean(image: &RgbaImage, channel: usize, block: [u32; 4]) -> f64 {
    let [left, top, right, bottom] = block;
    let count = f64::from((right - left) * (bottom - top));
    let mut total = 0.0;
    for y in top..bottom {
        for x in left..right {
            total += f64::from(image.get_pixel(x, y).0[channel]) / count;
        }
    }
    total
}

/// Writes a JPEG main file into `dir/memories` and returns its parsed name.
///
/// The pattern is high-frequency on purpose: a solid colour survives a JPEG re-encode with every
/// pixel bit-identical, so a fixture painted flat holds constant the exact dimension
/// `a_main_with_no_overlay_is_copied_byte_for_byte_rather_than_re_encoded` asserts on, and that
/// test passes whether the copy happens or not. The red channel is kept under 40 so the overlay
/// assertions elsewhere still have a clean "is this red" question to ask.
fn write_main(dir: &Path, day: &str, seed: u32) -> MemoryFile {
    write_main_sized(dir, day, seed, WIDTH, HEIGHT)
}

/// [`write_main`] painted a distinct colour, so two items that land on one path produce two
/// different files.
///
/// **The constraint is the one [`write_overlay`]'s neighbours record, and here it decides whether an
/// overwrite is observable at all.** [`write_main`] paints one deterministic pattern and the fix pass
/// is deterministic too, so two items built from it come out byte-identical: a checksum then cannot
/// separate "this file survived" from "it was overwritten by its neighbour", which is the exact
/// question `an_item_leaving_the_export_does_not_shift_a_survivor_onto_its_finished_file` asks.
/// `shade` is that dimension and the test guards it rather than assuming it.
fn write_main_shaded(dir: &Path, day: &str, seed: u32, shade: u8) -> MemoryFile {
    let mut pixels = RgbImage::new(WIDTH, HEIGHT);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = Rgb([shade, ((x * 13 + y * 7) % 251) as u8, ((x * 29 + y * 17) % 253) as u8]);
    }
    let path = memories_dir(dir).join(format!("{day}_{}-main.jpg", uuid(seed)));
    pixels.save_with_format(&path, ImageFormat::Jpeg).unwrap();
    MemoryFile::parse(path).unwrap()
}

/// [`write_main`] at any size, for a fixture whose dimensions have to disagree with the overlay's.
fn write_main_sized(dir: &Path, day: &str, seed: u32, width: u32, height: u32) -> MemoryFile {
    let mut pixels = RgbImage::new(width, height);
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
    write_overlay_sized(dir, day, seed, WIDTH, HEIGHT)
}

/// [`write_overlay`] at any size, same red/transparent split.
fn write_overlay_sized(dir: &Path, day: &str, seed: u32, width: u32, height: u32) -> MemoryFile {
    let mut pixels = RgbaImage::new(width, height);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < width / 2 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 0, 0]) };
    }
    let path = memories_dir(dir).join(format!("{day}_{}-overlay.png", uuid(seed)));
    pixels.save_with_format(&path, ImageFormat::Png).unwrap();
    MemoryFile::parse(path).unwrap()
}

/// A 64x48 WebP whose left half is opaque red and whose right half is fully transparent, encoded
/// once by this crate's own `webp` encoder and embedded so the fixture itself needs no encoder.
///
/// 9 of the 162 overlays in the observed export are WebP payloads in `.png`-named files (measured
/// 2026-08-04, header bytes only), and this is written under a `.png` name exactly like them.
/// Pre-encoded bytes are the point rather than a convenience: with the `webp` Cargo feature
/// dropped, image's encoder arm is gated away too, so a fixture that encoded at build time would
/// panic in the fixture instead of failing at `overlay::decode` with `Unsupported(Exact(WebP))`
/// — the exact failure the test exists to pin, and the one the feature-off mutation must produce.
const OVERLAY_WEBP_BYTES: &[u8] = &[
    0x52, 0x49, 0x46, 0x46, 0x9a, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x4c, 0x8d, 0x00, 0x00, 0x00, 0x2f, 0x3f,
    0xc0, 0x0b, 0x10, 0xcd, 0x55, 0x20, 0x22, 0x02, 0x1e, 0x88, 0x04, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x80, 0x79, 0x20, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe7, 0xdf, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xe0, 0xf0, 0x40, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xce, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x20, 0x0d, 0x55, 0x71, 0xf7, 0x00,
];

/// [`write_overlay_webp`]'s fixture under the `.png` name a real WebP-carrying overlay has.
fn write_overlay_webp(dir: &Path, day: &str, seed: u32) -> MemoryFile {
    let path = memories_dir(dir).join(format!("{day}_{}-overlay.png", uuid(seed)));
    fs::write(&path, OVERLAY_WEBP_BYTES).unwrap();
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
///
/// Every caller runs past a `common::usable` gate that claimed [`Tool::FfmpegFixtures`], so a
/// failure here is a genuine red rather than an absence, and this reports none.
fn write_video(dir: &Path, day: &str, seed: u32) -> MemoryFile {
    let path = memories_dir(dir).join(format!("{day}_{}-main.mp4", uuid(seed)));
    let built = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", &format!("color=c=blue:s={WIDTH}x{HEIGHT}:r=15:d=0.5")])
        .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono", "-shortest"])
        .args(["-c:v", "libx265", "-tag:v", "hvc1", "-pix_fmt", "yuv420p", "-c:a", "aac", "-t", "0.5"])
        .arg(&path)
        .output()
        .expect("the gate at the top of this test proved ffmpeg runs here");
    assert!(built.status.success(), "ffmpeg could not build the fixture: {}", String::from_utf8_lossy(&built.stderr));
    MemoryFile::parse(path).unwrap()
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

/// The plan a FIRST run builds: no manifest has recorded an output path yet, so every name this
/// hands out is a position in the plan. The same shape `tests/chat_fix.rs` uses for its own leg.
fn first_run(memories: &Memories, reconciliation: &Reconciliation, out: impl AsRef<Path>) -> Plan {
    Plan::build(memories, reconciliation, out, &RecordedOutputs::default())
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
/// distinguishable in the raw output.
fn exiftool(path: &Path) -> BTreeMap<String, String> {
    let output = Command::new("exiftool")
        .args(["-s", "-a", "-G0:1", "-validate", "-All"])
        .arg(path)
        .output()
        .expect("the gate at the top of this test proved exiftool runs here");
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        // `": "` rather than `':'`: the group prefix and every date value hold a colon, and only
        // the separator is followed by a space.
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| {
            // `[EXIF:ExifIFD]  DateTimeOriginal` -> `DateTimeOriginal`.
            let name = key.rsplit(']').next().unwrap_or(key).trim().to_owned();
            (name, value.trim().to_owned())
        })
        .collect()
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
///
/// A stream `ffprobe` cannot describe is a failure of the thing under test, not an absent tool, so
/// this asserts its way through the parse rather than reporting a `None` a caller could read as
/// "not installed".
fn probe_video(path: &Path) -> (String, u32, u32) {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height", "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .expect("the gate at the top of this test proved ffprobe runs here");
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines().map(str::trim);
    let described = format!("ffprobe did not describe the first video stream of {}: {text:?}", path.display());
    let codec = lines.next().expect(&described).to_owned();
    let width = lines.next().and_then(|line| line.parse().ok()).expect(&described);
    let height = lines.next().and_then(|line| line.parse().ok()).expect(&described);
    (codec, width, height)
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
        .expect("the gate at the top of this test proved ffmpeg runs here");
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    let bytes = fs::read(&raw).unwrap();
    let at = ((y * width + x) * 3) as usize;
    [bytes[at], bytes[at + 1], bytes[at + 2]]
}

// ---- the plan ----

/// The refactor that gave the memories and chat-media legs one `PlannedItem` must not have handed
/// this leg either of the two fields the other one brought. A memory has no sender and no thread, so
/// nothing may reach the metadata fields decision 44c defines; and this leg has only ever written
/// its composite, so none of decision 44b's three overlay modes may follow the shared type across.
///
/// This is the memories half of the overlay-mode seam, and it is the half with no control: the mode
/// is a chat-leg argument, `Plan::build` takes none, and the whole of what that has to mean here is
/// that a memory with an overlay is composited and keeps nothing.
#[test]
fn a_memory_carries_no_attribution_and_keeps_no_originals() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    // With an overlay, which is the case the other leg would keep originals for.
    let files = vec![write_main(dir.path(), "2021-01-15", 1), write_overlay(dir.path(), "2021-01-15", 1)];
    let reconciliation = reconciled(&memories, files);
    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);

    assert_eq!(plan.kind, ItemKind::Memory);
    assert!(plan.excluded.is_empty(), "the memories leg excludes nothing: {:?}", plan.excluded);
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].attribution, None);
    assert_eq!(plan.items[0].originals, None, "even with an overlay composited in");

    // And the run agrees with the plan: one file out, no `originals/` anywhere under the out root.
    let mut manifest = manifest(&dir, &reconciliation);
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1);
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_143005.jpg"]);
    assert!(!out.join("2021/01/originals").exists());
}

#[test]
fn an_exact_bucket_takes_its_time_and_place_from_the_entry() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let files = vec![write_main(dir.path(), "2021-01-15", 1)];
    let reconciliation = reconciled(&memories, files);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);

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
    let plan = first_run(&memories, &reconciliation, dir.path().join("out"));

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
    let plan = first_run(&memories, &reconciliation, dir.path().join("out"));

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
    let plan = first_run(&memories, &reconciliation, &out);

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
    let plan = first_run(&memories, &reconciliation, &out);

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
    let plan = first_run(&memories, &reconciliation, dir.path().join("out"));

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
    let plan = first_run(&memories, &reconciliation, &out);
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_000000.jpg", "2021/01/20210115_000000_2.jpg", "2021/01/20210115_000000_3.jpg"]);

    // Re-planning the same input hands out the same names, which is what a FIRST run's answer has
    // to be: the suffix is a position in the plan, never the next free slot on disk. What makes a
    // resume safe is a different mechanism and it is not this one — decision 52's adopt-plus-reserve
    // off the manifest, which `first_run` deliberately withholds by seeding an empty
    // `RecordedOutputs`. See `Plan`'s own doc for the split.
    let again = first_run(&memories, &reconciliation, &out);
    assert_eq!(outputs(&again, &out), outputs(&plan, &out));
}

/// Decision 52b's half: the memories leg carries the same defect and takes the same fix.
///
/// Two items in one directory on one second take `20210115_000000.jpg` and `_2.jpg`. The second is
/// driven back to work, which per decision 50 CLEARS its output record; the first then leaves the
/// export, taking its position in the plan with it. Re-deriving the ordinal plans the survivor onto
/// the departed row's path and the run writes over a repaired file that nothing will produce again,
/// since its source is gone.
///
/// **Only the reservation can save it, because the survivor has no record left to adopt**, which is
/// why the mutation that drops the reservation reds here and the one that drops adoption does not.
/// The chat leg's twin is `tests/chat_fix.rs`'s test of the same name, and the two are pinned apart
/// because 52b is a ruling about both legs: the memories leg's reachability is unmeasured, and its
/// date chain falls through to the filename day at midnight exactly as the chat one does.
///
/// The first digest assertion is the fixture's own self-guard: with both mains painted alike the
/// two outputs are byte-identical and the comparison below cannot fail whatever the code does.
#[test]
fn an_item_leaving_the_export_does_not_shift_a_survivor_onto_its_finished_file() {
    let dir = TempDir::new().unwrap();
    // Two ambiguous entries on one day, so neither may take its time from its entry and both fall
    // all the way through to the filename's midnight.
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Image", PARIS), (&at("2021-01-15", "23:00:00"), "Image", PARIS)]);
    let files = vec![write_main_shaded(dir.path(), "2021-01-15", 1, 40), write_main_shaded(dir.path(), "2021-01-15", 2, 220)];
    let reconciliation = reconciled(&memories, files);
    let mut first = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut first, 3, &copying()).unwrap().fixed, 2);
    assert_eq!(
        outputs(&plan, &out),
        ["2021/01/20210115_000000.jpg", "2021/01/20210115_000000_2.jpg"],
        "the fixture is not two items on one second in one directory"
    );

    let row = |manifest: &Manifest, source_id: &str| manifest.item(ItemKind::Memory, source_id).unwrap().expect("the row is enrolled");
    let (departing, survivor) = (plan.items[0].source_id.clone(), plan.items[1].source_id.clone());
    assert_ne!(
        row(&first, &departing).checksum,
        row(&first, &survivor).checksum,
        "the two outputs are byte-identical, so no digest below could see an overwrite"
    );

    // Driven back to work: `Pending`, output record dropped, file left alone — the state a resume
    // writes when the user deletes an output, reached here without a run.
    first.reset(ItemKind::Memory, &survivor).unwrap();
    fs::remove_file(&plan.items[0].media.main).unwrap();

    let after = reconciled(&memories, vec![MemoryFile::parse(plan.items[1].media.main.clone()).unwrap()]);
    let mut second = manifest(&dir, &after);
    let recorded = RecordedOutputs::read(&second, ItemKind::Memory).unwrap();
    let replan = Plan::build(&memories, &after, &out, &recorded);
    assert_eq!(local_fix::run(&replan, &mut second, 3, &copying()).unwrap().fixed, 1);

    // The assertion this test is NAMED for goes first, deliberately: a sibling assertion above it
    // aborts the body, and a red from there banks as a kill while this line never executes.
    let kept = row(&second, &departing);
    let output = kept.output_path.expect("the departed row still records the file it finished");
    let digest = Checksum::of_file(&output).expect("and that file is still there").0;
    assert_eq!(Some(digest), kept.checksum, "the departed item's repaired file was written over: {}", output.display());

    assert_eq!(outputs(&replan, &out), ["2021/01/20210115_000000_2.jpg"], "the survivor moved off its own name");
}

/// Decision 52's ADOPTION half at run level: an item whose finished output the user deleted is
/// rewritten at the path it recorded, rather than at wherever a fresh walk would put it. The chat
/// leg's twin carries the same name in `tests/chat_fix.rs`, and decision 52b is a ruling about both.
///
/// **It does NOT pin the read-before-resume ordering, and an earlier draft of this doc said it
/// did.** Measured: with the item set unmoved, a derived name walks to the first unclaimed path and
/// an adopted path normally IS that path, so the two mechanisms coincide. `Plan::build`'s own doc
/// carries the residual and the one case that separates them.
///
/// **No departure and no `reset`, and both omissions are the point.** The item set does not move, so
/// a red here can only be about adoption; and the record has to still be on the row when the plan is
/// built, which means the demotion must come from `resume` INSIDE `local_fix::run` rather than from
/// a `reset` beforehand. Reach for `reset` out of symmetry with the test above and this silently
/// becomes a second copy of the reservation pin.
///
/// Without adoption both records are still RESERVED, so the two items derive `_3` and `_4`: the
/// survivor's file moves for no reason and the numbering is scrambled from then on.
///
/// **Dropping BOTH halves leaves this green, which is not a weakness.** With nothing reserved the
/// positional walk hands out the same two names it always did, so the two errors cancel on this
/// fixture. The discriminating mutation for this test is the adoption half alone; the test above is
/// the one that reds when the whole fix goes.
///
/// The demotion arm is named rather than assumed: a deleted output is [`DemotionReason::Vanished`],
/// a different arm of `demotion_reason` from the one
/// `an_output_that_changed_since_it_was_recorded_is_redone_rather_than_trusted` exercises, and
/// asserting it is what proves the sweep ran rather than the row being skipped.
#[test]
fn an_item_whose_output_was_deleted_is_rewritten_at_the_path_it_recorded() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Image", PARIS), (&at("2021-01-15", "23:00:00"), "Image", PARIS)]);
    let files = vec![write_main_shaded(dir.path(), "2021-01-15", 1, 40), write_main_shaded(dir.path(), "2021-01-15", 2, 220)];
    let reconciliation = reconciled(&memories, files);
    let mut first = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut first, 3, &copying()).unwrap().fixed, 2);
    assert_eq!(
        outputs(&plan, &out),
        ["2021/01/20210115_000000.jpg", "2021/01/20210115_000000_2.jpg"],
        "the fixture is not two items on one second in one directory"
    );

    // The one on the SUFFIXED name is the one with something to lose: its path is the one a
    // re-derivation would move. Only its OUTPUT goes — both sources stay, both rows keep their
    // records, and nothing is reset.
    let suffixed = plan.items[1].source_id.clone();
    let suffixed_output = out.join("2021/01/20210115_000000_2.jpg");
    fs::remove_file(&suffixed_output).unwrap();

    let again = reconciled(
        &memories,
        vec![MemoryFile::parse(plan.items[0].media.main.clone()).unwrap(), MemoryFile::parse(plan.items[1].media.main.clone()).unwrap()],
    );
    let mut second = manifest(&dir, &again);
    let recorded = RecordedOutputs::read(&second, ItemKind::Memory).unwrap();
    let replan = Plan::build(&memories, &again, &out, &recorded);
    let report = local_fix::run(&replan, &mut second, 3, &copying()).unwrap();

    assert_eq!(
        report.resumed.demoted.iter().map(|one| (one.source_id.as_str(), one.reason)).collect::<Vec<_>>(),
        [(suffixed.as_str(), DemotionReason::Vanished)],
        "the resume sweep did not demote the deleted output, so nothing below is about a rewrite"
    );
    assert_eq!(report.fixed, 1, "{:?}", report.failed);

    let rewritten = second.item(ItemKind::Memory, &suffixed).unwrap().expect("the row is enrolled");
    assert_eq!(
        rewritten.output_path.as_deref(),
        Some(suffixed_output.as_path()),
        "the rewrite landed somewhere other than the file this item had already finished at"
    );
    assert!(suffixed_output.is_file(), "the recorded path was not written back");
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
            attribution: None,
        })
        .unwrap();
    stamped.write(&files[0].path).unwrap();

    let reconciliation = reconciled(&memories, files);
    let plan = first_run(&memories, &reconciliation, dir.path().join("out"));

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
    let plan = first_run(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 1);
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    let written = out.join("2021/01/20210115_143005.jpg");
    assert!(written.is_file(), "the year and month directories are the rename");

    // The overlay was actually drawn: its opaque half is red and its transparent half is not.
    let composite = image::open(&written).unwrap().to_rgb8();
    assert_eq!(composite.dimensions(), (WIDTH, HEIGHT));
    assert!(composite.get_pixel(2, 2).0[0] > 200, "the overlay's opaque half reached the composite");
    assert_shows_main_through(&composite, "the overlay's transparent half left the main showing");

    // The file's own date is the derived instant, not today.
    let modified = fs::metadata(&written).unwrap().modified().unwrap();
    let expected = UNIX_EPOCH + Duration::from_secs(u64::try_from(plan.items[0].capture.instant().timestamp()).unwrap());
    assert_eq!(modified, expected);
}

#[test]
fn an_overlay_smaller_than_its_main_is_scaled_up_to_cover_the_whole_frame() {
    // The dimensions the 2026-08-04 census found on real data: a 1440x2560 main with a
    // 1080x1920 overlay (38 of 161 real pairs, the modal image shape; see the local-fix
    // section of docs/design.md). `composite` scales the overlay to fit WITHIN the main, so an unscaled
    // composite would leave the main's bottom 640 rows and right 360 columns unpainted —
    // which is what the low-row asserts below catch, since the fixture main's red channel
    // never rises past 40 while the overlay's opaque half is 255.
    const MAIN_W: u32 = 1440;
    const MAIN_H: u32 = 2560;
    const OVERLAY_W: u32 = 1080;
    const OVERLAY_H: u32 = 1920;

    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let files = vec![
        write_main_sized(dir.path(), "2021-01-15", 1, MAIN_W, MAIN_H),
        write_overlay_sized(dir.path(), "2021-01-15", 1, OVERLAY_W, OVERLAY_H),
    ];
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    assert!(plan.items[0].media.overlay.is_some(), "the fixture must actually pair an overlay");
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1, "{:?}", plan.items[0].media);

    let composite = image::open(&plan.items[0].output).unwrap().to_rgb8();
    assert_eq!(composite.dimensions(), (MAIN_W, MAIN_H), "the composite keeps the MAIN's size, not the overlay's");
    // After scaling, the overlay's opaque red half is 720 wide and covers the full 2560
    // rows. The pixel at (700, MAIN_H - 60) sits inside that half only after scaling: an
    // unscaled 1080x1920 overlay never paints below row 1920, so this assert reds when the
    // resize is dropped even though the composite's dimensions do not move.
    assert!(composite.get_pixel(700, MAIN_H - 60).0[0] > 200, "the scaled overlay's opaque half reached the main's bottom rows");
    assert!(composite.get_pixel(1400, MAIN_H - 60).0[0] < 60, "the scaled transparent half still leaves the main showing, bottom rows");
    assert!(composite.get_pixel(1400, 100).0[0] < 60, "the scaled transparent half still leaves the main showing, a row both sizes reach");
}

#[test]
fn an_overlay_whose_aspect_mismatches_the_main_is_scaled_to_fit_centred_rather_than_stretched() {
    // The shape of the observed export's ninth WebP pair — a portrait 827x1548 overlay over a
    // video whose DECODED frame is 656x1232 portrait (tkhd says 1232x656, a 90° display-rotation
    // matrix turns it; see docs/domain-knowledge.md). There is no fill answer for that pair:
    // scaling the overlay TO the frame would distort the caption. Contain scales it within the
    // frame instead, preserving its aspect and centring it — the user's pick on 2026-08-04
    // (agent call contain-vs-skip, recorded in docs/design.md) — and the caption is never
    // dropped. No observed pair triggers this: all 161 real pairs are same-aspect, and on their
    // shapes contain and fill agree to within one unpainted row or column (13 pairs leave a
    // single line; the modal shapes round exactly — see docs/domain-knowledge.md).
    const MAIN_W: u32 = 1232;
    const MAIN_H: u32 = 656;
    const OVERLAY_W: u32 = 827;
    const OVERLAY_H: u32 = 1548;

    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let files = vec![
        write_main_sized(dir.path(), "2021-01-15", 1, MAIN_W, MAIN_H),
        write_overlay_sized(dir.path(), "2021-01-15", 1, OVERLAY_W, OVERLAY_H),
    ];
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    assert!(plan.items[0].media.overlay.is_some(), "the fixture must actually pair an overlay");
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1, "{:?}", plan.items[0].media);

    let composite = image::open(&plan.items[0].output).unwrap().to_rgb8();
    assert_eq!(composite.dimensions(), (MAIN_W, MAIN_H), "the composite keeps the MAIN's size, not the overlay's");

    // Contain scales by min(MAIN_W/OVERLAY_W, MAIN_H/OVERLAY_H) = 656/1548, so the drawn overlay
    // is height-constrained: it spans the full frame height, and the middle row crosses its
    // opaque left half (the fixture's red/transparent split). The main fixture's own red channel
    // never passes 30, so a >200 red channel is unambiguously overlay.
    let row = MAIN_H / 2;
    let red: Vec<u32> = (0..MAIN_W).filter(|x| composite.get_pixel(*x, row).0[0] > 200).collect();
    let left = *red.first().unwrap();
    let right = *red.last().unwrap();
    assert!(
        left > 100,
        "the opaque half must sit clear of the left edge (it starts at {left}): an unscaled or stretched overlay would reach it"
    );
    assert!(
        composite.get_pixel(MAIN_W - 1, row).0[0] < 60,
        "the frame's right edge must stay main: the drawn overlay ends well short of it"
    );
    // The opaque half is about half the drawn overlay, so its full width is recoverable and the
    // drawn aspect is preserved: ~827/1548, nowhere near the 1232/656 a stretch would produce.
    let drawn_w = 2 * (right - left + 1);
    let drawn_aspect = f64::from(drawn_w) / f64::from(MAIN_H);
    let expected = f64::from(OVERLAY_W) / f64::from(OVERLAY_H);
    assert!(
        (drawn_aspect - expected).abs() < 0.02,
        "drawn aspect {drawn_aspect:.3} vs the overlay's {expected:.3} — a stretch would give {:.3}",
        1232.0 / 656.0
    );
    // And it is centred: the transparent half has the same width on both sides of the frame.
    let centred = i64::from(MAIN_W - drawn_w) / 2;
    assert!((i64::from(left) - centred).abs() <= 3, "the opaque half starts at {left}, centred placement would start it at {centred}");
    // Height-constrained, so the drawn overlay reaches the main's bottom rows as well.
    assert!(
        composite.get_pixel(left + (right - left) / 2, MAIN_H - 60).0[0] > 200,
        "the drawn overlay's opaque half reached the main's bottom rows"
    );
}

#[test]
fn an_overlay_that_is_webp_bytes_under_a_png_name_composites_through_the_real_pairing() {
    // 9 of the 162 overlays in the observed export carry WebP payloads in `.png`-named files
    // (measured 2026-08-04, header bytes only). The decoder guesses from magic bytes rather than
    // the extension, so these must pair and composite exactly like a real png — and with the
    // `webp` feature dropped from Cargo.toml this test reds at `overlay::decode` with
    // `Unsupported(Exact(WebP))` rather than passing by accident (mutation-verified).
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let files = vec![write_main(dir.path(), "2021-01-15", 1), write_overlay_webp(dir.path(), "2021-01-15", 1)];
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    assert!(plan.items[0].media.overlay.is_some(), "the fixture must actually pair an overlay");
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1, "{:?}", plan.items[0].media);

    let composite = image::open(&plan.items[0].output).unwrap().to_rgb8();
    assert_eq!(composite.dimensions(), (WIDTH, HEIGHT));
    assert!(composite.get_pixel(2, 2).0[0] > 200, "the webp overlay's opaque half reached the composite");
    assert!(composite.get_pixel(WIDTH - 2, 2).0[0] < 60, "the webp overlay's transparent half left the main showing");
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
    let plan = first_run(&memories, &reconciliation, &out);
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
    let plan = first_run(&memories, &reconciliation, &out);
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
    let plan = first_run(&memories, &reconciliation, &out);
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
    let plan = first_run(&memories, &reconciliation, &out);
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
    let plan = first_run(&memories, &reconciliation, &out);
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

/// Task 45. The rule reads the EXTENSION and never the pixels, so a PNG main carrying no
/// transparency at all still keeps its own format under an overlay rather than being flattened into
/// a stamped JPEG.
///
/// **This test used to assert the opposite** — `a_png_main_with_an_overlay_is_composited_so_the_
/// metadata_writer_still_only_ever_sees_jpeg`. The old assertion was right for the old rule and the
/// property it named was never the one holding the advisory shut: `little_exif` dispatches on the
/// file type its caller passes, and nothing on this path passes one. What is asserted here instead
/// is the fact that decides an output path, which is the thing a plan and a fix step can disagree
/// about.
#[test]
fn an_opaque_png_main_under_an_overlay_keeps_its_own_format_because_the_rule_reads_the_extension() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);

    // Fully opaque, every pixel. Its red channel is 10, so the overlay's opaque red half is what
    // tells a composite that ran from a byte-for-byte copy.
    let mut pixels = RgbaImage::new(WIDTH, HEIGHT);
    for pixel in pixels.pixels_mut() {
        *pixel = Rgba([10, 200, 30, 255]);
    }
    let path = memories_dir(dir.path()).join(format!("2021-01-15_{}-main.png", uuid(1)));
    pixels.save_with_format(&path, ImageFormat::Png).unwrap();
    let source = fs::read(&path).unwrap();

    let files = vec![MemoryFile::parse(path).unwrap(), write_overlay(dir.path(), "2021-01-15", 1)];
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 1, "{:?}", report.failed);
    let written = &plan.items[0].output;
    assert_eq!(written.extension().unwrap(), "png");
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_143005.png"]);
    assert_eq!(&fs::read(written).unwrap()[..8], PNG_SIGNATURE, "the output is really a png, not a jpeg under a png name");

    // Composited rather than copied: the caption reached the frame, and the bytes are not the
    // source's own. Both are needed — the extension assertion above holds for the copy arm too.
    let composite = image::open(written).unwrap().to_rgba8();
    assert!((block_mean(&composite, 0, OPAQUE_BLOCK) - 255.0).abs() <= 1.0, "the overlay's opaque red half reached the frame");
    assert_ne!(fs::read(written).unwrap(), source, "a composite is not the export's own bytes");

    // Not stamped, because the output is not a JPEG — the cost task 45 accepted for this shape.
    assert_eq!(report.notices.len(), 1, "{:?}", report.notices);
    assert_eq!(report.notices[0].notice, Notice::NotStamped);
}

/// Task 45's own case: transparency the MAIN carries, under an overlay.
///
/// The old composite path ended in `to_rgb8`, which DISCARDS the alpha channel rather than
/// compositing it — correct for what the overlay left transparent, since the main is underneath it,
/// and wrong for what the main leaves transparent, where nothing is underneath and the stored RGB is
/// what lands. Measured unreachable on the observed export (all 77 overlay-paired image mains are
/// JPEG/RGB, and JPEG cannot carry alpha at all) and reachable in code, which is why it is pinned
/// here rather than left to the census.
#[test]
fn a_png_main_whose_own_transparency_sits_under_an_overlay_keeps_it() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);

    // Left half opaque `main_colour`, right half fully transparent with BLACK stored under
    // `alpha = 0` — the exact colour a flatten leaves behind, so nothing asserted below can be
    // satisfied by the defect it exists to catch. The overlay's own transparent half is the right
    // half too, so `TRANSPARENT_BLOCK` is a region BOTH layers left transparent and the composite
    // has to as well.
    let mut pixels = RgbaImage::new(WIDTH, HEIGHT);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        let [red, green, blue] = main_colour(x, y);
        *pixel = if x < WIDTH / 2 { Rgba([red, green, blue, 255]) } else { Rgba([0, 0, 0, 0]) };
    }
    let path = memories_dir(dir.path()).join(format!("2021-01-15_{}-main.png", uuid(1)));
    pixels.save_with_format(&path, ImageFormat::Png).unwrap();

    let files = vec![MemoryFile::parse(path).unwrap(), write_overlay(dir.path(), "2021-01-15", 1)];
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 1, "{:?}", report.failed);
    let written = &plan.items[0].output;
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_143005.png"]);
    assert_eq!(&fs::read(written).unwrap()[..8], PNG_SIGNATURE);

    let composite = image::open(written).unwrap().to_rgba8();
    // The assertion the whole task is about. Alpha, not brightness: a flatten produces `alpha = 255`
    // over black, and `255` versus `0` is the widest gap this image has. A JPEG output would answer
    // 255 here whatever its RGB was, so the failure cannot dress itself up as the fix.
    assert!(block_mean(&composite, 3, TRANSPARENT_BLOCK) <= 1.0, "the main's own transparent region was flattened away");
    // …and the composite really ran, so the assertion above is not passing on a copied-through file.
    assert!((block_mean(&composite, 0, OPAQUE_BLOCK) - 255.0).abs() <= 1.0, "the overlay's opaque red half reached the frame");
    assert!((block_mean(&composite, 3, OPAQUE_BLOCK) - 255.0).abs() <= 1.0, "the caption itself is opaque");

    assert_eq!(report.notices.len(), 1, "{:?}", report.notices);
    assert_eq!(report.notices[0].notice, Notice::NotStamped);
    let modified = fs::metadata(written).unwrap().modified().unwrap();
    let expected = UNIX_EPOCH + Duration::from_secs(u64::try_from(plan.items[0].capture.instant().timestamp()).unwrap());
    assert_eq!(modified, expected, "the capture date still reached the file's own timestamp");
}

/// Decision 47. A PNG with nothing to composite is copied through byte for byte, so its alpha
/// survives — `image`'s flatten DROPS the alpha channel rather than compositing it, and with no main
/// behind the layer a transparent region would land as whatever RGB sat under `alpha = 0`.
///
/// On the memories leg this shape is unreachable in the observed export (`memories.rs` records that
/// every `.png` in a memories dir is an overlay), but it is representable in the code — `png` is in
/// `MemoryKind::IMAGE` — so the shared rule is pinned here rather than assumed never to fire.
#[test]
fn a_lone_png_main_is_copied_through_with_its_transparency_intact() {
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);

    // Opaque red left half, fully transparent right half — and the transparent half's stored RGB is
    // BLACK, which is the colour a flatten would leave behind. So an assertion that the right half
    // is non-black cannot be satisfied by the defect.
    let mut pixels = RgbaImage::new(WIDTH, HEIGHT);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < WIDTH / 2 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 0, 0]) };
    }
    let path = memories_dir(dir.path()).join(format!("2021-01-15_{}-main.png", uuid(1)));
    pixels.save_with_format(&path, ImageFormat::Png).unwrap();
    let source = fs::read(&path).unwrap();

    let reconciliation = reconciled(&memories, vec![MemoryFile::parse(path).unwrap()]);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();
    assert_eq!(report.fixed, 1, "{:?}", report.failed);

    // The plan decided the extension, not the fix step: the collision key and the emitted name have
    // to agree or a `_2` suffix moves between runs.
    let written = &plan.items[0].output;
    assert_eq!(written.extension().unwrap(), "png");
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_143005.png"]);

    // Byte-for-byte, which is what proves a copy rather than a re-encode that happened to survive.
    assert_eq!(fs::read(written).unwrap(), source, "the bytes are the export's own");

    // And the alpha really is still there, read back independently of the byte comparison.
    let kept = image::open(written).unwrap().to_rgba8();
    assert_eq!(kept.get_pixel(WIDTH - 4, 4).0[3], 0, "the transparent half stayed transparent");
    assert_eq!(kept.get_pixel(4, 4).0, [255, 0, 0, 255], "and the opaque half is untouched");

    // Constraint 3: the date still reaches the file even though no metadata was written.
    let modified = fs::metadata(written).unwrap().modified().unwrap();
    let expected = UNIX_EPOCH + Duration::from_secs(u64::try_from(plan.items[0].capture.instant().timestamp()).unwrap());
    assert_eq!(modified, expected, "the capture date still reached the file's own timestamp");

    // Constraint 5: the run says what it did not do, rather than leaving a user to open the file.
    assert_eq!(report.notices.len(), 1, "{:?}", report.notices);
    assert_eq!(report.notices[0].notice, Notice::NotStamped);
    assert!(report.notices[0].notice.to_string().contains("transparency"), "{}", report.notices[0].notice);
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
    let plan = first_run(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 0);
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].reason.contains("cannot read"), "{}", report.failed[0].reason);
    assert!(!plan.items[0].output.exists(), "a failure before the write must leave no output behind");
    assert_eq!(fs::read(&plan.items[0].media.main).unwrap(), damaged, "the source is read-only to this pass, damaged or not");
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
    let plan = first_run(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].reason.contains("truncated or corrupt"), "{}", report.failed[0].reason);
    assert!(!plan.items[0].output.exists());
}

// ---- read back through exiftool ----

#[test]
fn the_stamped_output_reads_back_correctly_through_an_independent_reader() {
    if !common::usable("the_stamped_output_reads_back_correctly_through_an_independent_reader", &[Tool::Exiftool]) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let files = vec![write_main(dir.path(), "2021-01-15", 1), write_overlay(dir.path(), "2021-01-15", 1)];
    let reconciliation = reconciled(&memories, files);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1);

    let tags = exiftool(&plan.items[0].output);

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
    if !common::usable("a_southern_and_western_coordinate_reads_back_with_the_right_hemispheres", &[Tool::Exiftool]) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", RIO)]);
    let reconciliation = reconciled(&memories, vec![write_main(dir.path(), "2021-01-15", 1)]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1);

    let tags = exiftool(&plan.items[0].output);

    assert_eq!(tags.get("Validate").map(String::as_str), Some("OK"), "{tags:#?}");
    assert_eq!(tags.get("GPSLatitudeRef").map(String::as_str), Some("South"));
    assert_eq!(tags.get("GPSLongitudeRef").map(String::as_str), Some("West"));
    // Rio is UTC-3 all year, so 13:30:05 UTC is 10:30:05 on the wall.
    assert_eq!(tags.get("DateTimeOriginal").map(String::as_str), Some("2021:01:15 10:30:05"));
    assert_eq!(tags.get("OffsetTimeOriginal").map(String::as_str), Some("-03:00"));
}

#[test]
fn an_ambiguous_buckets_gps_verdict_survives_all_the_way_into_the_written_file() {
    if !common::usable("an_ambiguous_buckets_gps_verdict_survives_all_the_way_into_the_written_file", &[Tool::Exiftool]) {
        return;
    }
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
    let plan = first_run(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 4);

    let agreeing = exiftool(&plan.items[0].output);
    let disagreeing = exiftool(&plan.items[2].output);

    assert_eq!(agreeing.get("Validate").map(String::as_str), Some("OK"), "{agreeing:#?}");
    assert!(agreeing.contains_key("GPSLatitude"), "the agreeing bucket's file carries a coordinate: {agreeing:#?}");
    assert_eq!(disagreeing.get("Validate").map(String::as_str), Some("OK"), "{disagreeing:#?}");
    assert!(!disagreeing.contains_key("GPSLatitude"), "the disagreeing bucket's file must carry none: {disagreeing:#?}");
    assert!(!disagreeing.contains_key("GPSPosition"), "{disagreeing:#?}");
}

#[test]
fn metadata_the_source_already_carried_survives_the_stamp() {
    if !common::usable("metadata_the_source_already_carried_survives_the_stamp", &[Tool::Exiftool]) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Image", PARIS)]);
    let main = write_main(dir.path(), "2021-01-15", 1);

    // A foreign tag this build never writes, put there by the independent tool rather than by the
    // crate under test, so its survival is not the crate agreeing with itself.
    let status = Command::new("exiftool")
        .args(["-overwrite_original", "-Artist=A Foreign Writer", "-Make=SomeCamera"])
        .arg(&main.path)
        .status()
        .expect("the gate at the top of this test proved exiftool runs here");
    assert!(status.success());

    let reconciliation = reconciled(&memories, vec![main]);
    let mut manifest = manifest(&dir, &reconciliation);
    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    assert_eq!(local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap().fixed, 1);

    let tags = exiftool(&plan.items[0].output);
    assert_eq!(tags.get("Artist").map(String::as_str), Some("A Foreign Writer"), "{tags:#?}");
    assert_eq!(tags.get("Make").map(String::as_str), Some("SomeCamera"), "{tags:#?}");
    assert_eq!(tags.get("DateTimeOriginal").map(String::as_str), Some("2021:01:15 14:30:05"));
}

// ---- the video leg ----

#[test]
fn a_transcoding_run_re_encodes_a_memory_video_to_h264_and_dates_it() {
    if !common::usable(
        "a_transcoding_run_re_encodes_a_memory_video_to_h264_and_dates_it",
        &[Tool::FfmpegFixtures, Tool::FfmpegTranscode, Tool::Ffprobe],
    ) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    let video = write_video(dir.path(), "2021-01-15", 1);
    let source = fs::read(&video.path).unwrap();
    let reconciliation = reconciled(&memories, vec![video]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &transcoding()).unwrap();

    assert_eq!(report.fixed, 1, "{:?}", report.failed);
    assert!(report.notices.is_empty(), "a full transcode has nothing to report: {:?}", report.notices);

    let written = out.join("2021/01/20210115_143005.mp4");
    assert!(written.is_file(), "the year and month directories are the rename, and video keeps its own extension");
    // The whole reason the transcode is on by default: `hvc1` in, something Windows plays out.
    assert_eq!(probe_video(&written), ("h264".to_owned(), WIDTH, HEIGHT));
    assert_ne!(fs::read(&written).unwrap(), source, "the pixels were re-encoded, so the bytes cannot match");
    // The source is read-only to this pass, transcode or not.
    assert_eq!(fs::read(&plan.items[0].media.main).unwrap(), source);

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
    // No transcode encoder claimed: this run copies pixels, and a gate asking for one it never
    // reaches would skip on a box that can run every assertion here.
    if !common::usable(
        "a_run_that_is_not_transcoding_copies_the_pixels_and_still_writes_the_metadata",
        &[Tool::FfmpegFixtures, Tool::Ffprobe],
    ) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    let video = write_video(dir.path(), "2021-01-15", 1);
    let reconciliation = reconciled(&memories, vec![video]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
    let report = local_fix::run(&plan, &mut manifest, 3, &copying()).unwrap();

    assert_eq!(report.fixed, 1, "{:?}", report.failed);
    let written = &plan.items[0].output;
    // Still HEVC: nothing re-encoded a frame. That is the whole contract of the opt-out.
    assert_eq!(probe_video(written), ("hevc".to_owned(), WIDTH, HEIGHT));
    // And the run says so rather than leaving the user to notice.
    assert_eq!(
        report.notices.iter().map(|one| one.notice).collect::<Vec<_>>(),
        [Notice::NotTranscoded(TranscodeSkip::OptedOut)],
        "{:?}",
        report.notices
    );
    assert_eq!(report.notices[0].source_id, plan.items[0].source_id);

    // The metadata still landed, because metadata never goes through ffmpeg on either route.
    let probed = ffprobe_format(written);
    assert_eq!(probed.get("creation_time").map(String::as_str), Some("2021-01-15T13:30:05.000000Z"));
    assert_eq!(probed.get("date").map(String::as_str), Some("2021-01-15T14:30:05+01:00"), "Paris is UTC+1 in January");
    assert_eq!(probed.get("location").map(String::as_str), Some("+48.858844+002.294351/"));
}

#[test]
fn an_overlay_is_burned_in_only_by_a_transcode_and_the_run_says_when_it_was_not() {
    if !common::usable(
        "an_overlay_is_burned_in_only_by_a_transcode_and_the_run_says_when_it_was_not",
        &[Tool::FfmpegFixtures, Tool::FfmpegTranscode],
    ) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    let video = write_video(dir.path(), "2021-01-15", 1);
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
    let plan = first_run(&memories, &reconciliation, &out);
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
    let plan = first_run(&memories, &reconciliation, &out);
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
    let plan = first_run(&memories, &reconciliation, &out);
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
    // The transcode encoder is claimed even though this item ends refused: `fix_video` re-encodes
    // BEFORE it stamps, so the run reaches libx264 on its way to the refusal.
    if !common::usable("a_video_whose_date_cannot_be_stored_leaves_the_output_path_alone", &[Tool::FfmpegFixtures, Tool::FfmpegTranscode]) {
        return;
    }
    let dir = TempDir::new().unwrap();
    // 1965: a legal raw value against MP4's 1904 epoch, and one both readers show as a date in the
    // 2030s, so it is refused rather than written. Reachable from an entry date alone.
    let memories = entries(&[(&at("1965-06-01", "12:00:00"), "Video", PARIS)]);
    let video = write_video(dir.path(), "1965-06-01", 1);
    let reconciliation = reconciled(&memories, vec![video]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
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
    if !common::usable("a_video_that_is_not_one_is_recorded_against_its_own_row_and_the_run_carries_on", &[Tool::FfmpegFixtures]) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Video", PARIS), (&at("2021-01-15", "23:00:00"), "Video", PARIS)]);
    let healthy = write_video(dir.path(), "2021-01-15", 2);
    // A `.mp4` that is a JPEG underneath. Nothing re-encodes it, so the guard is what stops it.
    let liar = write_raw(dir.path(), &format!("2021-01-15_{}-main.mp4", uuid(1)), &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]);
    let reconciliation = reconciled(&memories, vec![liar, healthy]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
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
    if !common::usable(
        "a_video_ffmpeg_cannot_decode_fails_that_item_alone_and_keeps_its_message",
        &[Tool::FfmpegFixtures, Tool::FfmpegTranscode],
    ) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Video", PARIS), (&at("2021-01-15", "23:00:00"), "Video", PARIS)]);
    let healthy = write_video(dir.path(), "2021-01-15", 2);
    let liar = write_raw(dir.path(), &format!("2021-01-15_{}-main.mp4", uuid(1)), b"not really an mp4 at all");
    let reconciliation = reconciled(&memories, vec![liar, healthy]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
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
    if !common::usable("a_video_whose_time_falls_back_reads_its_own_movie_header_before_its_filename", &[Tool::FfmpegFixtures]) {
        return;
    }
    let dir = TempDir::new().unwrap();
    // Two ambiguous entries on one day: neither may take its time from an entry, so the only thing
    // that can move one off midnight is the file's own header.
    let memories = entries(&[(&at("2021-01-15", "01:00:00"), "Video", PARIS), (&at("2021-01-15", "23:00:00"), "Video", PARIS)]);
    let dated = write_video(dir.path(), "2021-01-15", 1);
    let undated = write_video(dir.path(), "2021-01-15", 2);

    // Give the first one a header time of its own, through this crate's own writer, since ffmpeg
    // leaves those fields zeroed unless told otherwise.
    let mut stamped = exportsnap::export::video::Mp4::read(&dated.path).unwrap();
    stamped
        .stamp(&exportsnap::export::video::VideoStamp {
            local: NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(9, 17, 42).unwrap(),
            offset: None,
            location: None,
            attribution: None,
        })
        .unwrap();
    stamped.write(&dated.path).unwrap();

    let reconciliation = reconciled(&memories, vec![dated, undated]);
    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);

    assert_eq!(plan.items.iter().map(|item| item.capture.source()).collect::<Vec<_>>(), [TimeSource::Embedded, TimeSource::Filename]);
    // The header holds a UTC instant with no offset field, so Paris moves it an hour forward on the
    // wall — the same conversion an entry's own `Date` goes through.
    assert_eq!(plan.items[0].capture.local(), NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(10, 17, 42).unwrap());
    assert_eq!(plan.items[1].capture.local(), NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(0, 0, 0).unwrap());
    assert_eq!(outputs(&plan, &out), ["2021/01/20210115_101742.mp4", "2021/01/20210115_000000.mp4"]);
}

#[test]
fn a_finished_video_is_not_transcoded_again_on_a_resume() {
    if !common::usable("a_finished_video_is_not_transcoded_again_on_a_resume", &[Tool::FfmpegFixtures, Tool::FfmpegTranscode]) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let memories = entries(&[(&at("2021-01-15", "13:30:05"), "Video", PARIS)]);
    let video = write_video(dir.path(), "2021-01-15", 1);
    let reconciliation = reconciled(&memories, vec![video]);
    let mut manifest = manifest(&dir, &reconciliation);

    let out = dir.path().join("out");
    let plan = first_run(&memories, &reconciliation, &out);
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
fn ffprobe_format(path: &Path) -> BTreeMap<String, String> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format_tags", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .expect("the gate at the top of this test proved ffprobe runs here");
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| line.strip_prefix("TAG:"))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}
