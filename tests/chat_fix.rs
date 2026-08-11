//! Public-API tests for `exportsnap::export::chat_fix`: the per-conversation output tree, the
//! reserved `_no-conversation` bucket, the `originals/` copies, the filesystem cleaning of an
//! untrusted conversation key, and what a chat-media run writes into the manifest.
//!
//! **Every fixture here is synthetic and none of them is the `fixtures/` tree.** Filenames are
//! synthesized in the test, images are painted by the `image` crate, directories are tempdirs, and
//! every manifest is opened with `open_in` so the per-user data dir is never touched. No real export
//! is read: this crate is about a run's own output tree, and the shapes below mirror the observed
//! export's SHAPE — a named `b` file, an unnamed one, a thumbnail, a zip pair — rather than its
//! counts, which n=1 makes a hint and not a contract.
//!
//! The image leg carries most of the behavioural coverage here, because both legs go through one
//! `local_fix::fix` and what is chat-specific is the plan. **Decision 44b's three overlay modes are
//! the exception and are asserted twice**, once on a JPEG zip pair and once on a zip pair whose media
//! half is the MP4 every real one ships — `each_overlay_mode_writes_exactly_what_decision_44b_says`
//! and `..._on_the_video_leg`. Under `originals` the two legs are handed different things (a main
//! alone, on a leg where the caption burn rides on the transcode), and reading one leg's answer off
//! the other's is what that second test exists to stop.
//!
//! **That one test is this crate's only external-tool dependency besides the `exiftool` read-back.**
//! It builds its fixture with ffmpeg, drives all three modes transcoding, and reads the output's
//! frames back through ffmpeg's own decoder. Both it and the `exiftool` read-back ask
//! `tests/common`'s shared gate up front for everything they will reach, print a skip notice when
//! one is not usable, and red naming the runner when it demanded the tool; everything else here
//! runs on a bare box.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::Tool;
use common::composite::{TRANSPARENT_BLOCK, assert_shows_main_through, main_colour};
use exportsnap::export::chat_fix::{self, OverlayMode, RecordedDirs, dir_name};
use exportsnap::export::chat_media::{ChatMediaFile, Discovery, Family, Reconciliation, Token, discover, reconcile};
use exportsnap::export::exif::{Jpeg, Stamp};
use exportsnap::export::local_fix::{self, DeferralReason, FixReport, Notice, Plan, RecordedOutputs, TimeSource, VideoOptions};
use exportsnap::export::manifest::{Checksum, DemotionReason, ExportId, Item, ItemKind, ItemStatus, Manifest};
use exportsnap::export::memories::Day;
use exportsnap::export::model::ChatHistory;
use exportsnap::export::schema;
use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use tempfile::TempDir;

mod common;

/// The 13-digit id shape the one observed export used.
const EXPORT_ID: &str = "1784667002819";

/// The 8-character word every one of the observed export's 928 zip filenames shares.
const ZIP_WORD: &str = "vantsnap";

/// A one-to-one thread's key is the friend's own handle; a group's is a bare uuid.
const SOLO_KEY: &str = "friend-handle";
const GROUP_KEY: &str = "3f2e1d0c-b9a8-4756-8433-2211aabbccdd";

const DAY: &str = "2021-03-04";
const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

// ---- fixtures ----

/// A distinct alphanumeric id per `seed`, in the shape a plain filename carries.
fn id(seed: u32) -> String {
    format!("aB3xY9{seed:04}")
}

fn chat_media_dir(root: &Path) -> PathBuf {
    let dir = root.join("chat_media");
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A JPEG whose pixels vary in every direction, so a composite that ran and one that did not differ
/// in more than one channel.
fn paint_jpeg(path: &Path) {
    let mut pixels = RgbImage::new(WIDTH, HEIGHT);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = Rgb([(x % 7) as u8 * 5, ((x * 13 + y * 7) % 251) as u8, ((x * 29 + y * 17) % 253) as u8]);
    }
    pixels.save_with_format(path, ImageFormat::Jpeg).unwrap();
}

/// An overlay whose left half is opaque red and whose right half is fully transparent, so a
/// composite that ran and one that did not are told apart by a single pixel each way.
fn paint_overlay(path: &Path) {
    paint_split_png(path, Rgba([255, 0, 0, 255]));
}

/// [`paint_overlay`]'s opaque/transparent split in any colour.
///
/// **A main and the layer drawn over it must not be painted the same colour**, and that is a
/// measured constraint rather than a stylistic one. `image`'s PNG encoder is deterministic, so
/// compositing a buffer over an identical buffer re-encodes to bytes identical to the input's:
/// neither a pixel comparison nor a byte comparison can then tell a composite from a copy. A
/// mutation deleting the compositor from the image leg's alpha arm left exactly that test green
/// until this existed.
fn paint_split_png(path: &Path, opaque: Rgba<u8>) {
    let mut pixels = RgbaImage::new(WIDTH, HEIGHT);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < WIDTH / 2 { opaque } else { Rgba([0, 0, 0, 0]) };
    }
    pixels.save_with_format(path, ImageFormat::Png).unwrap();
}

/// A plain-family file on disk. Returns the `<token>~<id>` stem, which is also its manifest id.
fn plain(root: &Path, token: Token, seed: u32) -> String {
    let stem = format!("{}~{}", token.as_word(), id(seed));
    paint_jpeg(&chat_media_dir(root).join(format!("{DAY}_{stem}.jpg")));
    stem
}

/// [`plain`] painted a distinct colour, so two items that land on one path produce two different
/// files.
///
/// **The constraint is the one [`paint_split_png`] records one layer over, and here it decides
/// whether an overwrite is observable at all.** [`plain`] paints one deterministic pattern and the
/// fix pass is deterministic too, so two items built from it come out byte-identical: a checksum
/// then cannot separate "this file survived" from "it was overwritten by its neighbour", which is
/// the exact question `an_item_leaving_the_export_does_not_shift_a_survivor_onto_its_finished_file`
/// asks. `shade` is that dimension and the test guards it rather than assuming it.
fn plain_shaded(root: &Path, token: Token, seed: u32, shade: u8) -> String {
    let stem = format!("{}~{}", token.as_word(), id(seed));
    paint_shaded_jpeg(&chat_media_dir(root).join(format!("{DAY}_{stem}.jpg")), shade);
    stem
}

/// [`paint_jpeg`] with subpixel 0 held at `shade` instead of varying, which is the dimension
/// [`plain_shaded`] documents: two files painted at different shades differ in their bytes, so a copy
/// of one can be told from a copy of the other.
fn paint_shaded_jpeg(path: &Path, shade: u8) {
    let mut pixels = RgbImage::new(WIDTH, HEIGHT);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = Rgb([shade, ((x * 13 + y * 7) % 251) as u8, ((x * 29 + y * 17) % 253) as u8]);
    }
    pixels.save_with_format(path, ImageFormat::Jpeg).unwrap();
}

/// A lone plain-family `overlay~*.png`, which is its own item with no overlay of its own.
///
/// The observed export holds 117 of these: decision 43a leaves the role-worded family unmatched
/// because its id sets are pairwise disjoint, so each one reaches the image leg alone. That is the
/// shape decision 47 is about, and it is measured reachable rather than hypothetical.
fn lone_overlay_png(root: &Path, seed: u32) -> String {
    let stem = format!("{}~{}", Token::Overlay.as_word(), id(seed));
    paint_overlay(&chat_media_dir(root).join(format!("{DAY}_{stem}.png")));
    stem
}

/// A zip pair on disk: a `media` half and the `overlay` half that pairs to it. Returns the shared
/// `<day>_<mid>.zip.<hash>` id.
///
/// The media half is a JPEG rather than the MP4 the real export ships, deliberately: the pairing is
/// a filename operation that never opens either file, so the extension is free, and keeping this
/// pair on the image leg is what lets every test built on it assert on composited PIXELS with no
/// external tool. [`zip_video_pair`] is the same shape on the leg the export actually uses.
fn zip_pair(root: &Path, seed: u32) -> String {
    let mid = format!("{ZIP_WORD}-{seed:07}");
    let dir = chat_media_dir(root);
    paint_jpeg(&dir.join(format!("{DAY}_media~{mid}.zip.a1b2c3d.jpg")));
    paint_overlay(&dir.join(format!("{DAY}_overlay~{mid}.zip.a1b2c3d.png")));
    format!("{DAY}_{mid}.zip.a1b2c3d")
}

/// [`zip_pair`] with the media half the observed export actually ships: a half-second HEVC video
/// with an audio track. `None` when ffmpeg is absent, which is the one thing the fixture cannot fake.
///
/// `hvc1` on purpose, the same reason `tests/local_fix.rs`'s `write_video` picks it: it is what the
/// export ships and what the transcode exists to move away from, so a leg that quietly skipped the
/// re-encode would pass against a fixture in anything else.
///
/// The frame is solid blue and the overlay's opaque half is red, so "the caption was burned in" and
/// "the frame came through untouched" are two different channels of one pixel rather than a
/// threshold on one.
///
/// **The overlay is exactly frame-sized, which the asserted pixel coordinates silently depend on.**
/// `ffmpeg::transcode` scales the layer to fit and centres it, so at equal dimensions that chain is
/// an identity and the opaque half really is the left half of the output. Size the two apart and
/// every coordinate below moves.
///
/// The one caller runs past a `common::usable` gate that claimed [`Tool::FfmpegFixtures`], so a
/// failure here is a genuine red rather than an absence, and this reports none.
fn zip_video_pair(root: &Path, seed: u32) -> String {
    let mid = format!("{ZIP_WORD}-{seed:07}");
    let dir = chat_media_dir(root);
    let built = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", &format!("color=c=blue:s={WIDTH}x{HEIGHT}:r=15:d=0.5")])
        .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono", "-shortest"])
        .args(["-c:v", "libx265", "-tag:v", "hvc1", "-pix_fmt", "yuv420p", "-c:a", "aac", "-t", "0.5"])
        .arg(dir.join(format!("{DAY}_media~{mid}.zip.a1b2c3d.mp4")))
        .output()
        .expect("the gate at the top of this test proved ffmpeg runs here");
    assert!(built.status.success(), "ffmpeg could not build the fixture: {}", String::from_utf8_lossy(&built.stderr));
    paint_overlay(&dir.join(format!("{DAY}_overlay~{mid}.zip.a1b2c3d.png")));
    format!("{DAY}_{mid}.zip.a1b2c3d")
}

/// One message naming whatever `media_ids` spells, sent by `from` at `created`.
fn message(from: &str, created: &str, media_ids: &str) -> schema::ChatEntry {
    schema::ChatEntry {
        from: from.to_owned(),
        media_type: "MEDIA".to_owned(),
        created: created.to_owned(),
        media_ids: media_ids.to_owned(),
        ..schema::ChatEntry::default()
    }
}

/// `chat_history.json` conversations, built through the real schema-to-model path so the
/// reconciliation never sees a state the loader could not produce.
fn history(rows: Vec<(&str, Vec<schema::ChatEntry>)>) -> ChatHistory {
    let conversations: BTreeMap<String, Vec<schema::ChatEntry>> =
        rows.into_iter().map(|(key, entries)| (key.to_owned(), entries)).collect();
    ChatHistory::try_from(schema::ChatHistory { conversations }).expect("the synthesized entries parse")
}

fn no_history() -> ChatHistory {
    history(vec![])
}

/// A reconciliation over names alone, for the tests about where an item lands rather than about
/// bytes. Nothing here touches a filesystem.
fn from_names(history: &ChatHistory, files: &[&str]) -> Reconciliation {
    let files = files
        .iter()
        .map(|name| ChatMediaFile::parse(Path::new("/export/chat_media").join(name)).expect("the synthesized name parses"))
        .collect();
    reconcile(history, Discovery::from_files(files, Vec::new()))
}

/// The plan a FIRST run builds: no manifest has recorded a directory yet, so every one of them is
/// derived from the conversation-key set.
fn first_run(reconciliation: &Reconciliation, out_root: impl AsRef<Path>, mode: OverlayMode) -> Plan {
    chat_fix::plan(reconciliation, out_root, mode, &RecordedDirs::default())
}

struct Workspace {
    temp: TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self { temp: TempDir::new().unwrap() }
    }

    /// The export tree the run walks. Its own directory, so the out root can be its sibling and a
    /// write into the source is visible as a change to this tree.
    fn source(&self) -> PathBuf {
        let source = self.temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        source
    }

    fn out(&self) -> PathBuf {
        self.temp.path().join("out")
    }

    fn state(&self) -> PathBuf {
        self.temp.path().join("state")
    }

    fn manifest(&self) -> Manifest {
        Manifest::open_in(self.state(), &ExportId::new(EXPORT_ID).unwrap()).unwrap()
    }

    /// Discover, join, enroll, plan and run — the whole chat-media pass, in the order a caller has
    /// to drive it, under decision 44b's default overlay mode.
    fn run(&self, history: &ChatHistory) -> Run {
        self.run_in(history, OverlayMode::Both)
    }

    /// [`Self::run`] under a named overlay mode.
    ///
    /// Enroll, read back where this export's conversations have been landing, plan, run — the order
    /// `chat_run::prepare` drives, and the order matters at both ends: the enrollment is what a
    /// returning file's row is reset by, and the resume sweep that drops a deleted output's record
    /// runs inside `local_fix::run`, after the plan is fixed.
    fn run_in(&self, history: &ChatHistory, mode: OverlayMode) -> Run {
        self.run_with(history, mode, &copying())
    }

    /// [`Self::run_in`] under a named set of video options, for the one test whose subject is what
    /// the video leg does with a mode. Every other caller takes [`copying`], which needs no tool.
    fn run_with(&self, history: &ChatHistory, mode: OverlayMode, video: &VideoOptions) -> Run {
        self.run_over(&reconcile(history, discover(self.source()).unwrap()), mode, video)
    }

    /// [`Self::run_with`] over a reconciliation the caller built rather than one walked out of the
    /// source, for the decision 53 fixtures whose file list no walk can produce. The four steps and
    /// their order are [`Self::run_in`]'s, stated there.
    fn run_over(&self, reconciliation: &Reconciliation, mode: OverlayMode, video: &VideoOptions) -> Run {
        let mut manifest = self.manifest();
        reconciliation.enroll(&mut manifest).unwrap();
        let recorded = RecordedDirs::read(reconciliation, &manifest).unwrap();
        let plan = chat_fix::plan(reconciliation, self.out(), mode, &recorded);
        let report = local_fix::run(&plan, &mut manifest, 3, video).unwrap();
        Run { plan, manifest, report }
    }

    /// A path under this workspace's tempdir that is NOT under the out root, for a decoder that has
    /// to write somewhere: dropping a scratch frame beside an output would put it in the tree
    /// [`tree`] walks.
    fn scratch(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    /// The plan alone, for the tests that assert on what a run WOULD write.
    fn plan(&self, history: &ChatHistory) -> Plan {
        self.plan_in(history, OverlayMode::Both)
    }

    /// [`Self::plan`] under a named overlay mode. A FIRST run's plan: no manifest is consulted.
    fn plan_in(&self, history: &ChatHistory, mode: OverlayMode) -> Plan {
        first_run(&reconcile(history, discover(self.source()).unwrap()), self.out(), mode)
    }

    /// What a resumed run would plan: the same read of this workspace's manifest [`Self::run_in`]
    /// makes, without the run behind it.
    fn replan(&self, history: &ChatHistory) -> Plan {
        let reconciliation = reconcile(history, discover(self.source()).unwrap());
        let recorded = RecordedDirs::read(&reconciliation, &self.manifest()).unwrap();
        chat_fix::plan(&reconciliation, self.out(), OverlayMode::Both, &recorded)
    }
}

struct Run {
    plan: Plan,
    manifest: Manifest,
    report: FixReport,
}

impl Run {
    fn row(&self, source_id: &str) -> Item {
        self.manifest.item(ItemKind::ChatMedia, source_id).unwrap().expect("the row is enrolled")
    }
}

/// The run all but one test here drives: no re-encode and no ffmpeg, so none of them depends on a
/// tool being installed. The chat leg's plan is what is under test, and it is the same plan either
/// way.
///
/// `each_overlay_mode_writes_exactly_what_decision_44b_says_on_the_video_leg` is the exception and
/// takes [`transcoding`], because the one thing it asks about is not the same either way.
fn copying() -> VideoOptions {
    VideoOptions { transcode: false, ffmpeg: None }
}

/// Transcoding on with a real ffmpeg, which is what `VideoOptions::probe` resolves to on the box
/// this repo is gated on. Built explicitly rather than probed so a test says which branch it is in.
///
/// The only options a caption burn is reachable under: `fix_video` draws the overlay inside
/// `ffmpeg::transcode` and nowhere else, so a run holding [`copying`] draws none whatever the mode
/// said.
///
/// **It does not always SAY so, and the exception is this test's own mode.** `OverlayNotBurned` is
/// pushed only where the pass was handed a layer, and `originals` is the mode that withholds one, so
/// a copying run there reports `NotTranscoded` alone. See
/// `each_overlay_mode_writes_exactly_what_decision_44b_says_on_the_video_leg`.
fn transcoding() -> VideoOptions {
    VideoOptions { transcode: true, ffmpeg: Some(PathBuf::from("ffmpeg")) }
}

/// Every file under `root`, as paths relative to it, sorted.
fn tree(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, found);
        } else {
            found.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
        }
    }
}

/// Every planned output, as a path, in plan order.
fn outputs(plan: &Plan) -> Vec<&Path> {
    plan.items.iter().map(|item| item.output.as_path()).collect()
}

/// The directory one planned item's output lands in, or `None` when the plan does not carry it.
fn dir_of(plan: &Plan, source_id: &str) -> Option<PathBuf> {
    plan.items.iter().find(|item| item.source_id == source_id).and_then(|item| item.output.parent()).map(Path::to_path_buf)
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

/// exiftool's view of a file, keyed by tag name.
///
/// The same shape `tests/local_fix.rs` reads its outputs with, down to the `": "` split: the group
/// prefix and every date value hold a colon, and only the separator is followed by a space.
fn exiftool(path: &Path) -> BTreeMap<String, String> {
    let output = Command::new("exiftool")
        .args(["-s", "-a", "-G0:1", "-All"])
        .arg(path)
        .output()
        .expect("the gate at the top of this test proved exiftool runs here");
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| {
            // `[EXIF:IFD0]  Artist` -> `Artist`.
            (key.rsplit(']').next().unwrap_or(key).trim().to_owned(), value.trim().to_owned())
        })
        .collect()
}

/// The codec and pixel dimensions of a video's first stream, through `ffprobe`.
///
/// Read back with a tool that is not this crate, the same reason `tests/local_fix.rs` does: a writer
/// agreeing with its own reader about a wrong encoding is what an independent one is for. A stream
/// `ffprobe` cannot describe is a failure of the thing under test rather than an absent tool, so
/// this asserts its way through the parse.
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

/// The colour of one pixel of a video's first frame, decoded into `raw`.
///
/// The decode target is the CALLER's path rather than one derived from `path`'s directory, so
/// reading an output back never drops a file into the output tree the assertions above walk.
fn first_frame_pixel(path: &Path, raw: &Path, x: u32, y: u32) -> [u8; 3] {
    let decoded = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24"])
        .arg(raw)
        .output()
        .expect("the gate at the top of this test proved ffmpeg runs here");
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    let bytes = fs::read(raw).unwrap();
    let at = ((y * WIDTH + x) * 3) as usize;
    [bytes[at], bytes[at + 1], bytes[at + 2]]
}

// ---- decision 46a and 46b: the tree ----

#[test]
fn a_file_a_message_named_lands_in_that_conversations_own_folder() {
    let work = Workspace::new();
    let named = plain(&work.source(), Token::B, 1);
    let run = work.run(&history(vec![(SOLO_KEY, vec![message("sender-handle", "2021-03-04 14:30:05 UTC", &named)])]));

    assert_eq!(run.report.fixed, 1);
    // Flat inside the conversation folder, and under `chat/` so a key spelling a year can never land
    // in the memories leg's own tree.
    assert_eq!(tree(&work.out()), ["chat/friend-handle/20210304_143005.jpg"]);
}

#[test]
fn a_file_no_message_names_lands_in_the_reserved_bucket_under_a_year_and_month() {
    let work = Workspace::new();
    plain(&work.source(), Token::B, 1);
    let run = work.run(&no_history());

    assert_eq!(run.report.fixed, 1);
    // Midnight, because nothing dated it: no message, and a painted JPEG carries no EXIF date.
    assert_eq!(tree(&work.out()), ["chat/_no-conversation/2021/03/20210304_000000.jpg"]);
}

#[test]
fn a_joined_and_an_unjoined_file_carry_one_filename_shape() {
    let work = Workspace::new();
    let named = plain(&work.source(), Token::B, 1);
    plain(&work.source(), Token::B, 2);
    let run = work.run(&history(vec![(SOLO_KEY, vec![message("sender-handle", "2021-03-04 14:30:05 UTC", &named)])]));
    assert_eq!(run.report.fixed, 2);

    // Decision 44c: the sender is in the metadata and never in the name, so the two names differ
    // only by the time each file is dated to — no prefix on one and not the other.
    let written = tree(&work.out());
    let names: Vec<&str> = written.iter().map(|path| path.rsplit_once('/').map_or(path.as_str(), |(_, name)| name)).collect();
    assert_eq!(names, ["20210304_000000.jpg", "20210304_143005.jpg"]);
}

// ---- decision 46c: the originals ----

#[test]
fn a_composited_pair_keeps_both_originals_beside_the_merged_file() {
    let work = Workspace::new();
    zip_pair(&work.source(), 4);
    let run = work.run(&no_history());
    assert_eq!(run.report.fixed, 1);

    // The merged file in the item's own directory, and `originals/` a subfolder of THAT directory —
    // which for a zip pair can only ever be under `_no-conversation`, since `chat_history.json`
    // names nothing but the plain `b` family, so no zip file is ever joined to a message.
    assert_eq!(
        tree(&work.out()),
        [
            "chat/_no-conversation/2021/03/20210304_000000.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_media~vantsnap-0000004.zip.a1b2c3d.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png",
        ]
    );

    // A kept original is dated like the file it came with, so a browser sorts the set together
    // rather than filing the merged file under the snap's day and its two sources under today's.
    let merged_at = modified(&work.out().join("chat/_no-conversation/2021/03/20210304_000000.jpg"));
    for name in ["2021-03-04_media~vantsnap-0000004.zip.a1b2c3d.jpg", "2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png"] {
        let copy = work.out().join("chat/_no-conversation/2021/03/originals").join(name);
        assert_eq!(modified(&copy), merged_at, "{name}");
        assert_ne!(modified(&copy), modified(&work.source().join("chat_media").join(name)), "{name} kept the download's date");
    }

    // The originals are the export's own bytes, not a second render of them.
    let kept = work.out().join("chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png");
    let original = work.source().join("chat_media/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png");
    assert_eq!(fs::read(&kept).unwrap(), fs::read(&original).unwrap());

    // And the merged file really is merged: the overlay's opaque left half wins and its transparent
    // right half does not, so a run that skipped the composite could not pass this.
    let merged = image::open(work.out().join("chat/_no-conversation/2021/03/20210304_000000.jpg")).unwrap().to_rgb8();
    assert!(merged.get_pixel(4, 4).0[0] > 200, "the left half is the overlay's red: {:?}", merged.get_pixel(4, 4));
    assert_shows_main_through(&merged, TRANSPARENT_BLOCK, "the transparent half is the main showing through");
}

// ---- decision 53: the kept copies go through the claim set too ----

/// A zip pair on disk in a `chat_media` dir of its own, under filenames and an id the caller picks
/// apart from each other.
///
/// **Synthetic by construction, and it does not reproduce an export shape.**
/// [`ChatMediaFile::parse`] derives an item's id FROM its filename, so two files sharing a basename
/// parse to one id and one role and `Discovery::from_walk` keeps the first while reporting the rest
/// as duplicates: no walk of any export can hand a plan two items whose kept filenames collide. The
/// collision is reachable through the struct literal below — public fields, which [`Discovery`]
/// documents as the way in for a caller that already knows the answers — and there only. On the
/// observed export the question cannot arise at all, its 9465 basenames being distinct across all
/// three `chat_media` dirs.
///
/// The id is the ONE thing here a parse would have answered differently. The day, the family, the
/// roles and the extensions are all read off the names the way a parsed file's would be, so nothing
/// downstream of the plan is looking at a shape it could not otherwise meet.
fn colliding_pair(chat_media: &Path, id: &str, main: &str, overlay: &str, shade: u8) -> Vec<ChatMediaFile> {
    fs::create_dir_all(chat_media).unwrap();
    paint_shaded_jpeg(&chat_media.join(main), shade);
    paint_split_png(&chat_media.join(overlay), Rgba([shade, 0, 255, 255]));

    let (mid, hash) = id.strip_prefix(&format!("{DAY}_")).unwrap().split_once(".zip.").unwrap();
    let file = |name: &str, token: Token, extension: &str| ChatMediaFile {
        path: chat_media.join(name),
        day: Day::parse(DAY).unwrap(),
        token,
        family: Family::Zip { mid: mid.to_owned(), hash: hash.to_owned() },
        id: id.to_owned(),
        extension: extension.to_owned(),
    };
    vec![file(main, Token::Media, "jpg"), file(overlay, Token::Overlay, "png")]
}

/// The `<day>_<mid>.zip.<hash>` id a zip pair shares, and the two filenames a parse would spell it
/// out of.
fn zip_id(seed: u32) -> String {
    format!("{DAY}_{ZIP_WORD}-{seed:07}.zip.a1b2c3d")
}

fn zip_main_name(seed: u32) -> String {
    format!("{DAY}_media~{ZIP_WORD}-{seed:07}.zip.a1b2c3d.jpg")
}

fn zip_overlay_name(seed: u32) -> String {
    format!("{DAY}_overlay~{ZIP_WORD}-{seed:07}.zip.a1b2c3d.png")
}

/// [`zip_pair`] under a mid the caller spells and with bytes that tell it from its neighbour, for the
/// one fixture below whose two pairs differ in the CASE of that mid alone. Returns the shared id.
fn zip_pair_shaded(root: &Path, mid: &str, shade: u8) -> String {
    let dir = chat_media_dir(root);
    paint_shaded_jpeg(&dir.join(format!("{DAY}_media~{mid}.zip.a1b2c3d.jpg")), shade);
    paint_split_png(&dir.join(format!("{DAY}_overlay~{mid}.zip.a1b2c3d.png")), Rgba([shade, 0, 255, 255]));
    format!("{DAY}_{mid}.zip.a1b2c3d")
}

/// Every kept copy holds the bytes of the file it was made from.
///
/// **The half that makes the file COUNT above worth anything.** Two items landing on one copy path
/// leaves one file holding one item's bytes, and an assertion that only counted names would read the
/// same on a directory holding two copies of one source. Read off each item's own plan entry rather
/// than off a path spelled here, so a mutation moving where a copy lands cannot move the assertion
/// with it.
fn assert_kept_bytes(plan: &Plan) {
    for item in &plan.items {
        let kept = item.originals.as_ref().expect("`both` keeps every pair the export shipped");
        assert_eq!(fs::read(&kept.main_copy).unwrap(), fs::read(&item.media.main).unwrap(), "{:?}", kept.main_copy);
        assert_eq!(fs::read(&kept.overlay_copy).unwrap(), fs::read(&kept.overlay).unwrap(), "{:?}", kept.overlay_copy);
    }
}

/// Two items whose MAIN filenames collide. See [`colliding_pair`] for what the fixture is and is not.
#[test]
fn two_items_whose_main_filenames_collide_keep_both_of_them() {
    let work = Workspace::new();
    // The same main name in two `chat_media` dirs, which is the only place one export could hold it
    // twice, and an overlay each that does not collide with anything.
    let mut files = colliding_pair(&work.source().join("first/chat_media"), &zip_id(4), &zip_main_name(4), &zip_overlay_name(4), 10);
    files.extend(colliding_pair(&work.source().join("second/chat_media"), &zip_id(5), &zip_main_name(4), &zip_overlay_name(5), 200));
    let reconciliation = reconcile(&no_history(), Discovery::from_files(files, Vec::new()));

    let run = work.run_over(&reconciliation, OverlayMode::Both, &copying());
    assert_eq!(run.plan.items.len(), 2, "the two pairs are two items — the pre-condition everything below rests on");
    assert_eq!(run.report.fixed, 2, "both were written: {:?}", run.report.failed);

    // Both mains want `originals/2021-03-04_media~vantsnap-0000004.zip.a1b2c3d.jpg`. The claim set
    // hands the second a `_2` at PLAN time, so the subfolder holds four files rather than three.
    let kept: Vec<String> = tree(&work.out()).into_iter().filter(|path| path.contains("/originals/")).collect();
    assert_eq!(
        kept,
        [
            "chat/_no-conversation/2021/03/originals/2021-03-04_media~vantsnap-0000004.zip.a1b2c3d.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_media~vantsnap-0000004.zip.a1b2c3d_2.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png",
            "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000005.zip.a1b2c3d.png",
        ]
    );

    // Guarded rather than assumed: painted at one shade the two mains would be byte-identical, and
    // then one file overwriting the other would satisfy every comparison in `assert_kept_bytes`.
    let mains: BTreeSet<Vec<u8>> = run.plan.items.iter().map(|item| fs::read(&item.media.main).unwrap()).collect();
    assert_eq!(mains.len(), 2, "the fixture's two mains have to differ for the byte check below to discriminate");
    assert_kept_bytes(&run.plan);
}

/// The overlay half of the pair above, and it is a separate test rather than a second assertion:
/// [`local_fix::Outputs::kept`] claims two paths, and one test can only red for one of them.
#[test]
fn two_items_whose_overlay_filenames_collide_keep_both_of_them() {
    let work = Workspace::new();
    let mut files = colliding_pair(&work.source().join("first/chat_media"), &zip_id(4), &zip_main_name(4), &zip_overlay_name(4), 10);
    files.extend(colliding_pair(&work.source().join("second/chat_media"), &zip_id(5), &zip_main_name(5), &zip_overlay_name(4), 200));
    let reconciliation = reconcile(&no_history(), Discovery::from_files(files, Vec::new()));

    let run = work.run_over(&reconciliation, OverlayMode::Both, &copying());
    assert_eq!(run.plan.items.len(), 2, "the two pairs are two items — the pre-condition everything below rests on");
    assert_eq!(run.report.fixed, 2, "both were written: {:?}", run.report.failed);

    let kept: Vec<String> = tree(&work.out()).into_iter().filter(|path| path.contains("/originals/")).collect();
    assert_eq!(
        kept,
        [
            "chat/_no-conversation/2021/03/originals/2021-03-04_media~vantsnap-0000004.zip.a1b2c3d.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_media~vantsnap-0000005.zip.a1b2c3d.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png",
            "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d_2.png",
        ]
    );

    let overlays: BTreeSet<Vec<u8>> =
        run.plan.items.iter().map(|item| fs::read(&item.originals.as_ref().unwrap().overlay).unwrap()).collect();
    assert_eq!(overlays.len(), 2, "the fixture's two overlays have to differ for the byte check below to discriminate");
    assert_kept_bytes(&run.plan);
}

/// The same collision reached through the WALK rather than through a struct literal, which is the
/// whole reason it is here beside the two above.
///
/// **They are pinned on a fixture the production path cannot produce and this one is not**, which is
/// a listed way for a test to read as discriminating while every reachable input leaves it
/// equivalent. `ChatMediaFile::parse` makes a file's id a function of its name, so the one collision
/// it admits is a pair of names that fold onto one string without being equal: two ids, two items,
/// and two files or one depending on the DESTINATION filesystem — an export holding both spellings
/// proves its own mount does not fold, so what this stands in for is that export read onto a folding
/// `--out`. The claim set folds ascii case for exactly that reason, so the second copy is suffixed
/// here too, and decision 11 is why it has to be.
///
/// Unobserved rather than impossible: the observed export spells one 8-character word across all 928
/// zip filenames. This does not claim to reproduce an export shape either — it claims the walk can
/// build it.
///
/// One test for both copies, deliberately: the mid is in both filenames, so both claims move
/// together here and neither can be pinned apart from the other. The two above are what separate
/// them.
///
/// **The case answer is doubly held and no ONE-line mutation can red this, so nobody re-runs either
/// as a coverage gap.** `Outputs` folds twice: `next`'s hint key and `ClaimedPaths`' own set.
/// Dropping the SET's fold is a real gap wherever nothing else covers it, and here it is already
/// killed by `local_fix::tests::a_recorded_path_differing_only_in_case_still_reserves_its_file`.
/// Dropping the HINT's fold is **an equivalent mutant on every input, killable by no test that could
/// ever be written**, and that is a property of `free` rather than of this fixture: `next` is read
/// only to choose where the loop STARTS, the loop exits on `used.claim` alone, and the value stored
/// back is the winner's ordinal plus one — every ordinal below it having failed `claim` at that
/// moment, and a claim never being released. So the hint is always a valid lower bound on the first
/// free ordinal; splitting its key space can only lower one, and a lower valid bound reaches the same
/// name after more probes. It changes the probe count and never a returned path. Only dropping both
/// reds this test, and then nothing but this and the unit test named above.
#[test]
fn two_items_whose_filenames_differ_only_in_case_keep_both_of_them() {
    let work = Workspace::new();
    zip_pair_shaded(&work.source(), &format!("{ZIP_WORD}-0000004"), 10);
    zip_pair_shaded(&work.source(), &format!("{}-0000004", ZIP_WORD.to_uppercase()), 200);

    let run = work.run(&no_history());
    assert_eq!(run.plan.items.len(), 2, "two spellings of one mid are two ids and two items");
    assert_eq!(run.report.fixed, 2, "both were written: {:?}", run.report.failed);

    // The uppercase mid sorts first, so it takes the plain names and the lowercase pair is what the
    // fold pushes onto a suffix.
    let kept: Vec<String> = tree(&work.out()).into_iter().filter(|path| path.contains("/originals/")).collect();
    assert_eq!(
        kept,
        [
            "chat/_no-conversation/2021/03/originals/2021-03-04_media~VANTSNAP-0000004.zip.a1b2c3d.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_media~vantsnap-0000004.zip.a1b2c3d_2.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~VANTSNAP-0000004.zip.a1b2c3d.png",
            "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d_2.png",
        ]
    );

    // The same guard the two tests above carry, for the same reason: the shades are the only thing
    // making the two mains different files, and equalized they would leave the byte half of
    // `assert_kept_bytes` unable to tell "each copy holds its own source" from "one source at two
    // names" while every name above still checked out.
    let mains: BTreeSet<Vec<u8>> = run.plan.items.iter().map(|item| fs::read(&item.media.main).unwrap()).collect();
    assert_eq!(mains.len(), 2, "the fixture's two mains have to differ for the byte check below to discriminate");
    assert_kept_bytes(&run.plan);
}

// ---- decision 44b: the three overlay modes ----

/// The three modes differ in exactly two answers, and this asserts both from the OUTPUT rather than
/// from the plan, because the plan is where the seam lives and a test reading it back would be
/// reading the implementation to itself.
///
/// `merged` composites and keeps nothing; `both` composites and keeps the pair; `originals` keeps
/// the pair and does not composite. The composite check is a pixel: the overlay's opaque left half
/// is red and the main under it is not, so a run that skipped the burn cannot pass the merged arm
/// and one that did it cannot pass the originals arm.
#[test]
fn each_overlay_mode_writes_exactly_what_decision_44b_says() {
    for (mode, keeps_originals, composites) in
        [(OverlayMode::Merged, false, true), (OverlayMode::Both, true, true), (OverlayMode::Originals, true, false)]
    {
        let work = Workspace::new();
        let id = zip_pair(&work.source(), 4);
        let run = work.run_in(&no_history(), mode);
        assert_eq!(run.report.fixed, 1, "{mode}");

        // The merged file is the output under every mode: `mark_done` checksums it and a
        // checksum-less Done row demotes on the next resume, so "copy the pair and write nothing"
        // was never available (decision 46d).
        let output = work.out().join("chat/_no-conversation/2021/03/20210304_000000.jpg");
        assert!(output.is_file(), "{mode}: the main is the output whatever happens to the caption");
        assert_eq!(run.row(&id).status, ItemStatus::Done, "{mode}");

        let kept: Vec<String> = tree(&work.out()).into_iter().filter(|path| path.contains("/originals/")).collect();
        if keeps_originals {
            assert_eq!(
                kept,
                [
                    "chat/_no-conversation/2021/03/originals/2021-03-04_media~vantsnap-0000004.zip.a1b2c3d.jpg",
                    "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png",
                ],
                "{mode}"
            );
        } else {
            assert!(kept.is_empty(), "{mode}: nothing is kept, so no originals/ folder exists at all — {kept:?}");
        }

        let written = image::open(&output).unwrap().to_rgb8();
        if composites {
            assert!(written.get_pixel(4, 4).0[0] > 200, "{mode}: the overlay's red left half won — {:?}", written.get_pixel(4, 4));
            assert_shows_main_through(&written, TRANSPARENT_BLOCK, &format!("{mode}: the transparent half is the main"));
        } else {
            let painted = main_colour(4, 4);
            let pixel = written.get_pixel(4, 4).0;
            assert!(
                pixel.iter().zip(painted).all(|(actual, expected)| i16::from(*actual) - i16::from(expected) < 24),
                "{mode}: the caption was NOT burned in, so the main's own pixel survives — got {pixel:?}, painted {painted:?}"
            );
        }
    }
}

/// The same three answers as its image twin, on the leg the observed export actually ships.
///
/// Every one of the 464 zip pairs has an MP4 media half, and until this test no chat-media VIDEO
/// item had been through the fix pass under any mode. `originals` is the arm that needed measuring
/// rather than reading: it is the one that hands `fix_video` a main alone — `chat_fix::plan` clears
/// `SourceMedia::overlay` and keeps the layer in `Originals` instead — and that combination had only
/// ever been reached through the early return `keep_originals` takes on the image arms too.
///
/// **All three arms run TRANSCODING, and that is forced by the leg rather than chosen.** The caption
/// is drawn inside `ffmpeg::transcode` and nowhere else (decision 36), so a run holding [`copying`]
/// cannot burn one under any mode and the pixel check below would answer "not burned" three times —
/// a fixture holding constant the exact dimension its assertion names.
///
/// **The `originals` arm's clean frame would otherwise be free, and ONE assertion carries that.** A
/// run that re-encoded nothing at all reports "not burned" too, so the codec assertion — the output
/// is `h264` where the source was `hvc1` — is the evidence that ffmpeg ran and still drew nothing.
/// The notice assertion beside it is **not** a second guard and must not be read as one: the only
/// notice this fixture can reach comes from the degrade path, and a degrade by construction leaves
/// the codec alone, so it cannot fire anywhere the codec assertion would not. It is kept because it
/// fires first and names the mechanism in the failure message, not because it adds coverage.
///
/// And what that degrade reports under THIS mode is `NotTranscoded` alone. `OverlayNotBurned` needs
/// a layer the pass was handed, and `originals` is the mode that withholds one — so "the notice list
/// is empty" is a weaker statement here than it is one mode over, which is the second reason it is
/// not the guard.
///
/// The composite check is a pixel here exactly as on the image leg, read back through ffmpeg's own
/// decoder rather than through anything in this crate. Two channels of two pixels: the overlay's
/// opaque half is red where the frame is blue, so a drawn caption and an untouched frame differ in
/// both directions and neither answer can be satisfied by the other's failure.
///
/// **A lone pixel is enough here and a block mean was needed on the image leg, and the difference is
/// the fixture rather than a looser standard.** That fixture is high-frequency by design, so JPEG's
/// DCT smears neighbours and a single pixel drifts up to 21 levels; this one is a flat colour field
/// with nothing to smear. Measured on this exact `argv` at n9.0: over the whole opaque region
/// min == max at `[253, 0, 0]` burned and `[0, 0, 254]` not, i.e. zero spatial variance, leaving the
/// three thresholds 53, 60 and 104 clear of the values they must reject.
#[test]
fn each_overlay_mode_writes_exactly_what_decision_44b_says_on_the_video_leg() {
    // Asked once for the whole loop, and for all three of what it reaches: the fixture's encoders,
    // the transcode's own encoder, and the reader that reads the codec back.
    if !common::usable(
        "each_overlay_mode_writes_exactly_what_decision_44b_says_on_the_video_leg",
        &[Tool::FfmpegFixtures, Tool::FfmpegTranscode, Tool::Ffprobe],
    ) {
        return;
    }
    for (mode, keeps_originals, composites) in
        [(OverlayMode::Merged, false, true), (OverlayMode::Both, true, true), (OverlayMode::Originals, true, false)]
    {
        let work = Workspace::new();
        let id = zip_video_pair(&work.source(), 4);
        let run = work.run_with(&no_history(), mode, &transcoding());
        assert_eq!(run.report.fixed, 1, "{mode}: {:?}", run.report.failed);

        // The repaired main is the output under every mode, and it keeps the video leg's own
        // extension: `mark_done` checksums it, so "copy the pair and write nothing" was never
        // available (decision 46d).
        let output = work.out().join("chat/_no-conversation/2021/03/20210304_000000.mp4");
        assert!(output.is_file(), "{mode}: the main is the output whatever happens to the caption — {:?}", tree(&work.out()));
        assert_eq!(run.row(&id).status, ItemStatus::Done, "{mode}");

        // Kept for the failure message rather than for coverage — the assertion BELOW is what makes
        // the missing caption attributable to the mode. An empty notice list cannot carry that on
        // its own: the shipped degrade path does report itself (`NotTranscoded`), so what an empty
        // list fails to exclude is a regression that copies the bytes THROUGH the transcode chain
        // and reports nothing, and the codec is the only thing that separates those two.
        assert!(run.report.notices.is_empty(), "{mode}: {:?}", run.report.notices);
        assert_eq!(probe_video(&output), ("h264".to_owned(), WIDTH, HEIGHT), "{mode}: the re-encode ran in this arm");

        let kept: Vec<String> = tree(&work.out()).into_iter().filter(|path| path.contains("/originals/")).collect();
        if keeps_originals {
            assert_eq!(
                kept,
                [
                    "chat/_no-conversation/2021/03/originals/2021-03-04_media~vantsnap-0000004.zip.a1b2c3d.mp4",
                    "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png",
                ],
                "{mode}"
            );
        } else {
            assert!(kept.is_empty(), "{mode}: nothing is kept, so no originals/ folder exists at all — {kept:?}");
        }

        let raw = work.scratch("frame.raw");
        let opaque = first_frame_pixel(&output, &raw, 2, 2);
        if composites {
            assert!(opaque[0] > 200, "{mode}: the overlay's red opaque half reached the frame — {opaque:?}");
            let transparent = first_frame_pixel(&output, &raw, WIDTH - 2, 2);
            assert!(transparent[2] > 150, "{mode}: the transparent half left the video showing — {transparent:?}");
        } else {
            assert!(opaque[0] < 60, "{mode}: the caption was NOT burned in — {opaque:?}");
            assert!(opaque[2] > 150, "{mode}: the source's own blue frame is what is there — {opaque:?}");
        }
    }
}

/// A PNG main that pairs keeps its own format under EVERY overlay mode, so no mode moves an output
/// path.
///
/// **This test used to assert the opposite half of a divergence that no longer exists.** Under the
/// old rule the extension predicate folded in `SourceMedia::overlay`, which `originals` withholds,
/// so the same file landed `.png` under one mode and `.jpg` under the other two — a mode silently
/// deciding an output name. Task 45 sends a composited alpha-capable main to PNG too, so the
/// extension is read alone and the three modes agree. Kept as a test rather than deleted with the
/// divergence, because "a mode does not move a path" is the property worth holding.
///
/// Unreachable from the observed export (only the zip family pairs and every zip main is a video),
/// which is why it is asserted here rather than left to a reader to infer from the predicate.
#[test]
fn a_png_main_that_pairs_keeps_its_own_format_under_every_overlay_mode() {
    let names = ["2021-03-04_media~vantsnap-0000009.zip.a1b2c3d.png", "2021-03-04_overlay~vantsnap-0000009.zip.a1b2c3d.png"];
    let expected = Path::new("/out/chat/_no-conversation/2021/03/20210304_000000.png");

    for mode in OverlayMode::ALL {
        let plan = first_run(&from_names(&no_history(), &names), "/out", mode);
        assert_eq!(plan.items[0].output, expected, "{mode}: an overlay mode must not move an output path");
    }
}

/// The mode reaches the pass through the plan alone, which is the property that keeps `local_fix`
/// leg-agnostic AND mode-agnostic. Asserted on the two fields that carry it, because a mode flag
/// leaking into `fix` would show up here first as a third field nobody set.
#[test]
fn the_overlay_mode_moves_the_plan_and_nothing_else() {
    let work = Workspace::new();
    zip_pair(&work.source(), 4);

    let merged = work.plan_in(&no_history(), OverlayMode::Merged);
    let both = work.plan_in(&no_history(), OverlayMode::Both);
    let originals = work.plan_in(&no_history(), OverlayMode::Originals);

    // `merged` and `both` hand the pass the same media and differ only in what is kept.
    assert_eq!(merged.items[0].media, both.items[0].media);
    assert_eq!(merged.items[0].originals, None);
    assert!(both.items[0].originals.is_some());

    // `originals` withholds the caption from the pass and is the ONLY mode that does.
    assert!(merged.items[0].media.overlay.is_some(), "merged composites, so the pass gets the layer");
    assert_eq!(originals.items[0].media.overlay, None, "originals never composites, so the pass gets the main alone");

    // …and it is still not lost: the copy knows about it.
    let kept = originals.items[0].originals.as_ref().expect("originals keeps the pair");
    assert_eq!(kept.overlay, work.source().join(format!("chat_media/{DAY}_overlay~{ZIP_WORD}-0000004.zip.a1b2c3d.png")));
    assert_eq!(both.items[0].originals.as_ref().unwrap().overlay, kept.overlay, "both modes keep the same file");
}

/// A file with no overlay has nothing to keep under any mode, so `originals` does not mint an empty
/// folder for it. The 6877 files no message names are all this shape.
#[test]
fn a_lone_file_keeps_nothing_under_every_overlay_mode() {
    for mode in OverlayMode::ALL {
        let work = Workspace::new();
        plain(&work.source(), Token::B, 1);
        let run = work.run_in(&no_history(), mode);
        assert_eq!(run.plan.items[0].originals, None, "{mode}");
        assert!(tree(&work.out()).iter().all(|path| !path.contains("originals")), "{mode}: {:?}", tree(&work.out()));
    }
}

#[test]
fn a_file_with_no_overlay_keeps_no_originals_folder() {
    let work = Workspace::new();
    plain(&work.source(), Token::B, 1);
    let run = work.run(&no_history());

    assert_eq!(run.plan.items.len(), 1);
    assert_eq!(run.plan.items[0].originals, None, "nothing was composited, so there is no un-merged version to lose");
    assert!(tree(&work.out()).iter().all(|path| !path.contains("originals")), "{:?}", tree(&work.out()));
}

// ---- decision 47: a lone PNG passes through ----

#[test]
fn a_lone_overlay_png_keeps_its_alpha_its_extension_and_its_own_bytes() {
    let work = Workspace::new();
    let stem = lone_overlay_png(&work.source(), 5);
    let source = fs::read(work.source().join(format!("chat_media/{DAY}_{stem}.png"))).unwrap();
    let run = work.run(&no_history());
    assert_eq!(run.report.fixed, 1);

    // `.png`, decided at plan time so the collision key and the emitted name cannot disagree.
    assert_eq!(tree(&work.out()), ["chat/_no-conversation/2021/03/20210304_000000.png"]);
    let written = work.out().join("chat/_no-conversation/2021/03/20210304_000000.png");
    assert_eq!(fs::read(&written).unwrap(), source, "the bytes are the export's own, not a re-encode");

    // The fixture's transparent half stores BLACK under `alpha = 0`, which is exactly what a flatten
    // would leave behind — so this assertion cannot be satisfied by the defect it exists to catch.
    let kept = image::open(&written).unwrap().to_rgba8();
    assert_eq!(kept.get_pixel(WIDTH - 4, 4).0[3], 0, "the transparent half stayed transparent");
    assert_eq!(kept.get_pixel(4, 4).0, [255, 0, 0, 255]);

    // Decision 47's costs, both reported rather than discovered: no metadata, and the date on the
    // file itself.
    assert_eq!(run.report.notices.len(), 1, "{:?}", run.report.notices);
    assert_eq!(run.report.notices[0].notice, Notice::NotStamped);
    assert_eq!(run.report.notices[0].source_id, stem);
    let expected = UNIX_EPOCH + Duration::from_secs(u64::try_from(run.plan.items[0].capture.instant().timestamp()).unwrap());
    assert_eq!(modified(&written), expected, "the capture date still reached the file's own timestamp");

    // Nothing was consumed, so nothing is kept beside it.
    assert_eq!(run.plan.items[0].originals, None);
}

/// The format-keeping membership test is ascii-case-insensitive, so a `.PNG` source is admitted — and
/// then its extension has to be normalized before it reaches an output path, or the same file
/// spelled two ways lands at two names. Both planners key their collision map on that same string,
/// so a divergence moves output paths rather than staying cosmetic, and a case-folding filesystem
/// would have the two spellings fighting over one directory entry.
///
/// This pins the normalization and **not** the length-independence of `ALPHA_CAPABLE_EXTENSIONS`,
/// which no shipped test can pin: while that list holds one member, indexing it and reading the
/// item's own extension answer identically on every input that exists. The evidence for that is a
/// mutation, recorded in the round's table, not an assertion here — and the list is capped at one
/// member anyway until `fix_image` picks its encoder off the resolved extension, which the constant's
/// own doc records.
#[test]
fn a_shouted_extension_is_normalized_rather_than_carried_into_the_output_path() {
    let work = Workspace::new();
    let stem = format!("{}~{}", Token::Overlay.as_word(), id(7));
    paint_overlay(&chat_media_dir(&work.source()).join(format!("{DAY}_{stem}.PNG")));
    let run = work.run(&no_history());

    assert_eq!(run.report.fixed, 1, "{:?}", run.report.failed);
    assert_eq!(tree(&work.out()), ["chat/_no-conversation/2021/03/20210304_000000.png"]);
}

#[test]
fn a_copied_png_and_a_stamped_jpeg_on_one_second_do_not_take_each_others_suffix() {
    // The collision key uses the RESOLVED extension. Keyed on the leg's default instead, these two
    // would share one entry and the second would be handed a `_2` it does not need.
    let work = Workspace::new();
    lone_overlay_png(&work.source(), 5);
    plain(&work.source(), Token::B, 1);
    let run = work.run(&no_history());
    assert_eq!(run.report.fixed, 2);
    assert_eq!(
        tree(&work.out()),
        ["chat/_no-conversation/2021/03/20210304_000000.jpg", "chat/_no-conversation/2021/03/20210304_000000.png",]
    );
}

#[test]
fn a_png_that_does_have_an_overlay_composites_to_an_unstamped_png() {
    // Task 45. This asserted a stamped JPEG until the ruling moved it: a zip pair whose media half
    // is a png still goes through the compositor, and now the composite is encoded as a PNG so the
    // main's own transparency is not flattened onto whatever sat under `alpha = 0`. The cost is the
    // metadata, which the run reports rather than leaving to be discovered.
    let work = Workspace::new();
    let mid = format!("{ZIP_WORD}-0000006");
    let dir = chat_media_dir(&work.source());
    // Blue under a red caption, deliberately: see `paint_split_png` for why the two halves of a pair
    // must not share a colour. With both painted red a mutation deleting the compositor left this
    // test green, byte comparison included.
    paint_split_png(&dir.join(format!("{DAY}_media~{mid}.zip.a1b2c3d.png")), Rgba([0, 90, 200, 255]));
    paint_overlay(&dir.join(format!("{DAY}_overlay~{mid}.zip.a1b2c3d.png")));

    let run = work.run(&no_history());
    assert_eq!(run.report.fixed, 1);
    let written = work.out().join("chat/_no-conversation/2021/03/20210304_000000.png");
    assert!(written.is_file(), "{:?}", tree(&work.out()));
    assert_eq!(&fs::read(&written).unwrap()[..8], b"\x89PNG\r\n\x1a\n", "a composited png stays a png");

    let composite = image::open(&written).unwrap().to_rgba8();
    assert_eq!(composite.get_pixel(4, 4).0, [255, 0, 0, 255], "the caption's red beat the main's blue, so the composite ran");
    assert_eq!(composite.get_pixel(WIDTH - 4, 4).0[3], 0, "and the region both layers left transparent still is");

    assert_eq!(run.report.notices.len(), 1, "{:?}", run.report.notices);
    assert_eq!(run.report.notices[0].notice, Notice::NotStamped);
}

// ---- decision 44d: thumbnails ----

#[test]
fn a_thumbnail_enrolls_produces_no_output_and_is_counted_apart() {
    let work = Workspace::new();
    let thumbnail = plain(&work.source(), Token::Thumbnail, 3);
    plain(&work.source(), Token::B, 1);
    let run = work.run(&no_history());

    assert_eq!(run.plan.excluded, std::slice::from_ref(&thumbnail));
    assert!(run.plan.items.iter().all(|item| item.source_id != thumbnail), "a thumbnail is never planned as work");
    assert!(run.plan.deferred.is_empty(), "excluded rather than deferred, so nothing re-offers it every run");
    assert_eq!(run.report.excluded, 1);
    assert_eq!(run.report.fixed, 1, "the file beside it is still fixed");
    // The FIRST run's own resume already sees it excluded, which is what pins the ordering inside
    // `local_fix::run`: exclude, then sweep, then read the work list. Written after the sweep, the
    // row would be counted as owed work on the run that excluded it.
    assert_eq!(run.report.resumed.excluded, 1, "the exclusion lands before the resume sweep counts the statuses");
    assert_eq!(run.report.resumed.pending, 1, "and the file beside it is the only thing the sweep calls owed");
    // Enrolled, and its row says so rather than the row being absent.
    assert_eq!(run.row(&thumbnail).status, ItemStatus::Excluded);
    // One output file, and it is not the thumbnail's.
    assert_eq!(tree(&work.out()), ["chat/_no-conversation/2021/03/20210304_000000.jpg"]);
}

#[test]
fn an_excluded_row_is_never_handed_back_as_work_and_never_demoted() {
    let work = Workspace::new();
    let thumbnail = plain(&work.source(), Token::Thumbnail, 3);
    let mut run = work.run(&no_history());

    assert!(
        run.manifest.pending(ItemKind::ChatMedia, 3).unwrap().iter().all(|item| item.source_id != thumbnail),
        "an excluded row is not work"
    );
    // A second run's resume counts it apart from every other status and leaves it where it is.
    let second = local_fix::run(&run.plan, &mut run.manifest, 3, &copying()).unwrap();
    assert_eq!(second.resumed.excluded, 1);
    assert_eq!((second.resumed.pending, second.resumed.source_missing, second.resumed.retired, second.resumed.verified), (0, 0, 0, 0));
    assert!(second.resumed.demoted.is_empty(), "{:?}", second.resumed.demoted);
    assert_eq!(run.row(&thumbnail).status, ItemStatus::Excluded);
}

/// Writing `Excluded` over an already-`Excluded` row must touch nothing, or `updated_at` degrades
/// from "the last status transition" to "the last run" and every run rewrites every excluded row for
/// nothing. Same class as the `Retired` exemption in the retirement sweep, and the status column
/// cannot see it: only the timestamp can.
///
/// **The still-pending file is the positive control and it shares the one call.** Without it a
/// `run` that had stopped excluding anything at all would pass, because a dead call leaves a
/// backdated row alone just as well as a correct one does.
#[test]
fn a_second_run_leaves_an_already_excluded_rows_timestamp_alone() {
    /// Far enough in the past that `unixepoch()` cannot produce it during this test. The sentinel is
    /// load-bearing: `unixepoch()` has one-second resolution, so two runs inside one second would
    /// rewrite the row with the value it already had and no assertion could tell that from a skip.
    const SENTINEL: i64 = 1_000_000_000;

    let work = Workspace::new();
    let thumbnail = plain(&work.source(), Token::Thumbnail, 3);
    let fresh = plain(&work.source(), Token::B, 1);
    let mut run = work.run(&no_history());
    assert_eq!(run.row(&thumbnail).status, ItemStatus::Excluded);

    // Only reachable by editing the database, which is the point: it backdates the row so a rewrite
    // is distinguishable from a skip. The finished file is backdated too, so the control proves the
    // second run really did reach the manifest.
    let db = work.state().join(format!("{EXPORT_ID}.sqlite"));
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE items SET updated_at = ?1", [SENTINEL]).unwrap();
    drop(conn);
    // The output file goes with it, so the resume sweep demotes the control and writes its row.
    fs::remove_file(work.out().join("chat/_no-conversation/2021/03/20210304_000000.jpg")).unwrap();

    let mut manifest = work.manifest();
    local_fix::run(&run.plan, &mut manifest, 3, &copying()).unwrap();
    run.manifest = manifest;

    let untouched = run.row(&thumbnail);
    assert_eq!(untouched.updated_at, SENTINEL, "the second run rewrote a row whose status never transitioned");
    assert_eq!(untouched.status, ItemStatus::Excluded);
    assert_ne!(run.row(&fresh).updated_at, SENTINEL, "positive control: that same run did reach the manifest and did write a row");
}

// ---- the conversation-key cleaner ----

#[test]
fn a_key_cannot_escape_the_output_root() {
    // Every separator, both traversal spellings, an absolute path and a home shortcut. None of them
    // survives as anything but one harmless component.
    for key in ["..", ".", "../..", "/etc/passwd", "..\\..\\windows", "C:\\x", "~/.ssh", "a/../b"] {
        let cleaned = dir_name(key);
        assert!(!cleaned.is_empty(), "{key:?}");
        assert_eq!(Path::new(&cleaned).components().count(), 1, "{key:?} cleaned to {cleaned:?}");
        assert!(!cleaned.contains('/') && !cleaned.contains('\\'), "{key:?} cleaned to {cleaned:?}");
        // A joined name that resolves back out of the root is the failure this is really about.
        assert_eq!(Path::new("/out/chat").join(&cleaned).parent(), Some(Path::new("/out/chat")), "{key:?} cleaned to {cleaned:?}");
    }
}

#[test]
fn a_key_that_cleans_away_to_nothing_gets_a_name_rather_than_an_empty_one() {
    for key in ["", ".", "..", "...", "\u{0}\u{1}\u{2}", "///"] {
        let cleaned = dir_name(key);
        assert!(!cleaned.is_empty(), "{key:?}");
        assert!(!cleaned.trim_matches(['.', ' ']).is_empty(), "{key:?} cleaned to {cleaned:?}");
    }
    assert_eq!(dir_name(""), "_unnamed");
    assert_eq!(dir_name(".."), "_unnamed");
    // A key made only of refused characters is not empty after the mapping, so it keeps a name of
    // its own rather than joining the empty key's bucket.
    assert_eq!(dir_name("///"), "___");
}

#[test]
fn a_key_windows_could_not_open_is_moved_out_of_the_way() {
    // Device names, with and without an extension, in either case. The reservation is on the STEM,
    // so `CON.txt` is as unopenable as `CON`.
    for key in ["CON", "con", "Con.txt", "nul", "PRN", "aux", "COM1", "com9", "LPT1", "lpt9"] {
        let cleaned = dir_name(key);
        let stem = cleaned.split_once('.').map_or(cleaned.as_str(), |(head, _)| head);
        assert!(stem.starts_with('_'), "{key:?} cleaned to {cleaned:?}, whose stem is still a device name");
    }
    // Not device names, and they must not be mangled: `COM` with no digit, `COM10`, and a handle
    // that merely starts with one are all ordinary directory names.
    for key in ["com", "com0", "com10", "lpt0", "console", "auxiliary"] {
        assert_eq!(dir_name(key), key, "{key:?} is not reserved");
    }

    // The reserved characters, plus the trailing dot and space Windows silently strips and then
    // cannot open.
    assert_eq!(dir_name("a<b>c:d\"e|f?g*h "), "a_b_c_d_e_f_g_h_");
    assert!(!dir_name("trailing.").ends_with('.'));
    assert!(!dir_name("trailing ").ends_with(' '));
    // A control byte and a newline are characters a filesystem accepts and nothing else survives.
    assert_eq!(dir_name("a\nb\tc\u{7f}d"), "a_b_c_d");
}

#[test]
fn a_key_keeps_the_two_shapes_the_export_actually_writes() {
    // The point of an allowlist is that it must not cost a real name. A handle and a dashed uuid are
    // the two shapes `chat_history.json` keys by, and both come through whole.
    assert_eq!(dir_name(SOLO_KEY), SOLO_KEY);
    assert_eq!(dir_name(GROUP_KEY), GROUP_KEY);
    assert_eq!(dir_name("user.name_01"), "user.name_01");
}

#[test]
fn a_key_longer_than_the_cap_is_shortened_and_two_of_them_still_get_two_folders() {
    let long = "z".repeat(200);
    assert_eq!(dir_name(&long).len(), 64);

    // Truncation collides by construction, and the collision breaker is what keeps two keys apart.
    let history = history(vec![
        (&format!("{long}a"), vec![message("a", "2021-03-04 14:30:05 UTC", "b~aB3xY90001")]),
        (&format!("{long}b"), vec![message("b", "2021-03-04 14:30:05 UTC", "b~aB3xY90002")]),
    ]);
    let plan = first_run(&from_names(&history, &["2021-03-04_b~aB3xY90001.jpg", "2021-03-04_b~aB3xY90002.jpg"]), "/out", OverlayMode::Both);
    let dirs: BTreeSet<&Path> = plan.items.iter().filter_map(|item| item.output.parent()).collect();
    assert_eq!(dirs.len(), 2, "two keys, two directories: {dirs:?}");
    assert!(dirs.iter().all(|dir| dir.file_name().is_some_and(|name| name.len() <= 66)), "{dirs:?}");
}

#[test]
fn a_key_that_cleans_to_the_no_conversation_bucket_is_suffixed_away_from_it() {
    let history = history(vec![
        ("_no-conversation", vec![message("a", "2021-03-04 14:30:05 UTC", "b~aB3xY90001")]),
        // A conversation naming nothing, so the second file stays unnamed and really does belong in
        // the bucket the first key tried to take.
        ("other", vec![]),
    ]);
    let plan = first_run(&from_names(&history, &["2021-03-04_b~aB3xY90001.jpg", "2021-03-04_b~aB3xY90002.jpg"]), "/out", OverlayMode::Both);
    assert_eq!(
        outputs(&plan),
        [
            Path::new("/out/chat/_no-conversation_2/20210304_143005.jpg"),
            Path::new("/out/chat/_no-conversation/2021/03/20210304_000000.jpg"),
        ]
    );
}

#[test]
fn the_collision_suffix_is_the_same_answer_on_a_second_run() {
    // Two distinct keys cleaning to one name. The suffix has to be a position in the plan rather
    // than the next free name on disk, or a resume would file the same thread somewhere else once
    // the first run had already created a directory.
    let history = history(vec![
        ("a/b", vec![message("a", "2021-03-04 14:30:05 UTC", "b~aB3xY90001")]),
        ("a?b", vec![message("b", "2021-03-04 14:30:05 UTC", "b~aB3xY90002")]),
    ]);
    let reconciliation = from_names(&history, &["2021-03-04_b~aB3xY90001.jpg", "2021-03-04_b~aB3xY90002.jpg"]);

    let first = first_run(&reconciliation, "/out", OverlayMode::Both);
    let second = first_run(&reconciliation, "/out", OverlayMode::Both);
    assert_eq!(outputs(&first), [Path::new("/out/chat/a_b/20210304_143005.jpg"), Path::new("/out/chat/a_b_2/20210304_143005.jpg"),]);
    assert_eq!(outputs(&first), outputs(&second));
}

/// Directory names are a function of the conversation-key SET, not of which item is reached first.
///
/// Under arrival order, one item leaving the export could change which of two colliding
/// conversations was seen first and swap their two directories — and a resumed run would then file a
/// conversation's remaining media into another conversation's tree, with 44a's grouping quietly no
/// longer holding and nothing reporting it. Renaming a tree is not the class renaming a file is.
///
/// The fixture is built so arrival order and key order genuinely disagree: `a?b` sorts AFTER `a/b`
/// while owning neither the first nor the last item, so dropping the first item flips which
/// conversation arrives first without changing the key set.
#[test]
fn a_conversation_keeps_its_directory_when_a_neighbours_item_leaves_the_export() {
    let files = ["2021-03-04_b~aB3xY90001.jpg", "2021-03-04_b~aB3xY90002.jpg", "2021-03-04_b~aB3xY90003.jpg"];
    let rows = |media: &[&str]| {
        history(vec![
            ("a/b", media.iter().filter(|id| **id != "b~aB3xY90002").map(|id| message("a", "2021-03-04 14:30:05 UTC", id)).collect()),
            ("a?b", media.iter().filter(|id| **id == "b~aB3xY90002").map(|id| message("b", "2021-03-04 14:30:05 UTC", id)).collect()),
        ])
    };
    let all = ["b~aB3xY90001", "b~aB3xY90002", "b~aB3xY90003"];

    let full = first_run(&from_names(&rows(&all), &files), "/out", OverlayMode::Both);
    // The first item, which belongs to `a/b`, leaves the export. `a?b` now arrives first.
    let after = first_run(&from_names(&rows(&all[1..]), &files[1..]), "/out", OverlayMode::Both);

    assert_eq!(dir_of(&full, "b~aB3xY90002"), Some(PathBuf::from("/out/chat/a_b_2")), "sorted key order puts `a?b` second");
    assert_eq!(dir_of(&after, "b~aB3xY90002"), dir_of(&full, "b~aB3xY90002"), "an item leaving moved another conversation's directory");
}

/// The case sorted key order cannot reach: a whole CONVERSATION leaving moves the key set itself,
/// which is that rule's only input, so every survivor below the departure slides down one suffix and
/// lands in the tree the run before it filled for its neighbour.
///
/// What holds it still is the manifest — the directory a conversation's own rows already name — and
/// the fixture is built so the two answers are provably different: with three keys cleaning to
/// `a_b` and the FIRST one's media gone, re-deriving from the key set alone puts `a?b` in `a_b` and
/// `a|b` in `a_b_2`, the directory `a?b`'s own finished output is sitting in.
#[test]
fn a_conversation_keeps_the_directory_the_manifest_recorded_when_a_whole_neighbour_leaves() {
    let work = Workspace::new();
    let leaving = plain(&work.source(), Token::B, 1);
    let middle = plain(&work.source(), Token::B, 2);
    let last = plain(&work.source(), Token::B, 3);
    let sent = |id: &str| vec![message("a", "2021-03-04 14:30:05 UTC", id)];
    let run = work.run(&history(vec![("a/b", sent(&leaving)), ("a?b", sent(&middle)), ("a|b", sent(&last))]));
    assert_eq!(run.report.fixed, 3);

    let recorded_dir = |source_id: &str| -> PathBuf {
        let output = run.row(source_id).output_path.expect("a finished row records where it landed");
        output.parent().expect("an output is inside a directory").to_path_buf()
    };
    let chat = work.out().join("chat");
    assert_eq!(recorded_dir(&middle), chat.join("a_b_2"), "sorted key order put `a?b` second");
    assert_eq!(recorded_dir(&last), chat.join("a_b_3"), "and `a|b` third");

    // Every item of the FIRST key leaves the export: its file off disk AND its thread out of the
    // history, so the key set itself is one shorter rather than one item lighter.
    fs::remove_file(chat_media_dir(&work.source()).join(format!("{DAY}_{leaving}.jpg"))).unwrap();
    let after = work.replan(&history(vec![("a?b", sent(&middle)), ("a|b", sent(&last))]));

    assert_eq!(dir_of(&after, &middle), Some(recorded_dir(&middle)), "a conversation leaving moved a survivor's directory");
    assert_eq!(dir_of(&after, &last), Some(recorded_dir(&last)), "a conversation leaving moved a survivor's directory");
}

/// The mirror of the case above, and the half adoption alone cannot reach: a departed conversation is
/// in NOBODY's key set, so nothing adopts its directory, and a new key cleaning onto that name is
/// derived straight into the departed thread's tree — on top of files its finished rows still name.
///
/// Driven through the real [`RecordedDirs::read`] rather than a hand-built one, because the
/// reservation is the half of that read the join filter does not cover: `a/b`'s row is attributed to
/// no conversation this run names, so only a pass that ignores the join can see its directory at all.
#[test]
fn a_departed_conversations_directory_is_not_handed_to_a_new_key() {
    let work = Workspace::new();
    let leaving = plain(&work.source(), Token::B, 1);
    let staying = plain(&work.source(), Token::B, 2);
    let sent = |id: &str| vec![message("a", "2021-03-04 14:30:05 UTC", id)];
    let run = work.run(&history(vec![("a/b", sent(&leaving)), ("a?b", sent(&staying))]));
    assert_eq!(run.report.fixed, 2);
    let departed = run.row(&leaving).output_path.expect("a finished row records where it landed");
    assert_eq!(departed.parent(), Some(work.out().join("chat").join("a_b").as_path()), "sorted key order puts `a/b` first");

    // `a/b` leaves outright and a NEW key arrives that cleans onto its name. `a:b` sorts before
    // `a?b` (0x3A against 0x3F), so key order alone offers it `a_b` — the departed thread's tree.
    fs::remove_file(chat_media_dir(&work.source()).join(format!("{DAY}_{leaving}.jpg"))).unwrap();
    let arriving = plain(&work.source(), Token::B, 3);
    let after = work.replan(&history(vec![("a:b", sent(&arriving)), ("a?b", sent(&staying))]));

    assert_ne!(dir_of(&after, &arriving), departed.parent().map(Path::to_path_buf), "a new key was planned into a departed thread's tree");
    assert_eq!(dir_of(&after, &arriving), Some(work.out().join("chat").join("a_b_3")), "and it takes the first name nothing recorded");
    assert_eq!(dir_of(&after, &staying), Some(work.out().join("chat").join("a_b_2")), "the survivor still keeps its own");
}

/// A conversation whose only finished file vanished still names its own directory, through the
/// parked row rather than through an item.
///
/// The reservation pass cannot tell a departed conversation's directory from a live one's — it just
/// claims the name — so the only thing keeping a live conversation out of its own tree is that the
/// adopt pass got there first. A row that carries an output record and is attributable to nobody
/// breaks exactly that, and `SourceMissing` is the reachable one: `chat_media`'s own docs call a
/// file vanishing between two runs the ordinary case, and since queue task 39 that transition KEEPS
/// the output record while moving the row out of `Reconciliation::items`.
///
/// Without the key on the gap token the survivor lands in `friend-handle_2` beside the finished
/// output in `friend-handle`, and it does not self-correct: the next run records the split and
/// adopts it.
#[test]
fn a_conversation_whose_only_finished_file_vanished_keeps_its_own_directory() {
    let work = Workspace::new();
    let present = plain(&work.source(), Token::B, 1);
    let absent = format!("{}~{}", Token::B.as_word(), id(2));
    let rows = history(vec![(
        SOLO_KEY,
        vec![message("a", "2021-03-04 14:30:05 UTC", &present), message("a", "2021-03-04 14:30:05 UTC", &absent)],
    )]);

    // One file on disk and one token with no file, which is the gap row this turns on.
    let run = work.run(&rows);
    assert_eq!(run.report.fixed, 1, "{:?}", run.report.failed);
    let finished = run.row(&present).output_path.clone().expect("run 1 finished the present file");
    assert_eq!(run.row(&absent).status, ItemStatus::SourceMissing, "the token with no file parks");

    // The finished file leaves and the parked token's file arrives. The thread is still in the
    // history, so its own row is now the parked one — and it is the only row naming a directory.
    fs::remove_file(chat_media_dir(&work.source()).join(format!("{DAY}_{present}.jpg"))).unwrap();
    let arrived = plain(&work.source(), Token::B, 2);
    assert_eq!(arrived, absent, "the arriving file carries the token run 1 could not place");

    let two = work.run(&rows);
    assert_eq!(two.report.fixed, 1, "{:?}", two.report.failed);
    assert_eq!(two.row(&present).status, ItemStatus::SourceMissing, "the vanished file parks");
    assert!(two.row(&present).output_path.is_some(), "and keeps the record naming the conversation's directory");

    let arrived_dir = two.row(&arrived).output_path.clone().and_then(|path| path.parent().map(Path::to_path_buf));
    assert_eq!(arrived_dir, finished.parent().map(Path::to_path_buf), "the conversation was reserved out of its own directory");
}

/// A new key's minted name must not move on the run AFTER it is minted.
///
/// The reservation half of this only ever runs on the mint; from the next run the newcomer has a
/// record of its own and adoption carries it. Pinned because those are two different code paths
/// reaching one answer, and a fixture that stops at the mint cannot tell them apart.
#[test]
fn a_new_keys_minted_name_survives_the_run_after_it() {
    let work = Workspace::new();
    let leaving = plain(&work.source(), Token::B, 1);
    let staying = plain(&work.source(), Token::B, 2);
    let sent = |id: &str| vec![message("a", "2021-03-04 14:30:05 UTC", id)];
    let run = work.run(&history(vec![("a/b", sent(&leaving)), ("a?b", sent(&staying))]));
    assert_eq!(run.report.fixed, 2);

    fs::remove_file(chat_media_dir(&work.source()).join(format!("{DAY}_{leaving}.jpg"))).unwrap();
    let arriving = plain(&work.source(), Token::B, 3);
    let rows = history(vec![("a:b", sent(&arriving)), ("a?b", sent(&staying))]);
    let second = work.run(&rows);
    assert_eq!(second.report.fixed, 1, "{:?}", second.report.failed);
    let minted = second.row(&arriving).output_path.clone().expect("the newcomer finished").parent().expect("a dir").to_path_buf();
    assert_eq!(minted, work.out().join("chat").join("a_b_3"), "the newcomer takes the first name nothing recorded");

    assert_eq!(dir_of(&work.replan(&rows), &arriving), Some(minted), "the newcomer's name moved on the run after it was minted");
}

/// [`chat_fix::RecordedDirs::read`]'s own end of the fall-through rule: the unit test pins which
/// candidate wins, this pins that the read hands over more than one to choose from.
#[test]
fn an_unadoptable_record_off_the_manifest_falls_through_too() {
    let work = Workspace::new();
    let lower = plain(&work.source(), Token::B, 1);
    let higher = plain(&work.source(), Token::B, 2);
    let rows =
        history(vec![(SOLO_KEY, vec![message("a", "2021-03-04 14:30:05 UTC", &lower), message("a", "2021-03-04 14:30:05 UTC", &higher)])]);
    let run = work.run(&rows);
    assert_eq!(run.report.fixed, 2);

    // The lower row is recorded inside the bucket's month tree, which sorts below the conversation
    // directory and can never be adopted.
    let split = |source_id: &str, dir: &str| {
        let output = work.out().join("chat").join(dir).join("20210304_143005.jpg");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        paint_jpeg(&output);
        run.manifest.mark_done(ItemKind::ChatMedia, source_id, &output).unwrap();
    };
    split(&lower, "_no-conversation/2021/03");
    split(&higher, &format!("{SOLO_KEY}_7"));

    let after = work.replan(&rows);
    let kept = work.out().join("chat").join(format!("{SOLO_KEY}_7"));
    assert_eq!(dir_of(&after, &higher), Some(kept.clone()), "the unadoptable candidate stood in for the conversation");
    assert_eq!(dir_of(&after, &lower), Some(kept), "and one conversation still gets exactly one directory");
}

/// Two rows of one conversation can disagree about its directory, if a run older than this rule
/// split them, and the lowest recorded directory wins.
///
/// The alternative — the lowest ROW's — is the defect this whole rule exists to close, one layer
/// down: the lowest source id leaving would move the answer while every other row still sat where it
/// was. So the fixture puts the LOWER source id in the HIGHER directory, and puts neither of them in
/// the name the key set would derive. `manifest.items` is ordered by source id, so all three rules
/// answer differently here — `_9` for the first row's, `_5` for the lowest directory's, and the bare
/// key for a run that reads the manifest not at all.
///
/// Both directories are forged, through the same `mark_done` a run checks an item in with. What
/// produces a real split is a build older than this rule, which by definition cannot be driven from
/// this suite.
#[test]
fn two_rows_of_one_conversation_that_disagree_settle_on_the_lowest_directory() {
    let work = Workspace::new();
    let lower = plain(&work.source(), Token::B, 1);
    let higher = plain(&work.source(), Token::B, 2);
    let rows =
        history(vec![(SOLO_KEY, vec![message("a", "2021-03-04 14:30:05 UTC", &lower), message("a", "2021-03-04 14:30:05 UTC", &higher)])]);
    let run = work.run(&rows);
    assert_eq!(run.report.fixed, 2);

    let split = |source_id: &str, dir: &str| {
        let output = work.out().join("chat").join(dir).join("20210304_143005.jpg");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        paint_jpeg(&output);
        run.manifest.mark_done(ItemKind::ChatMedia, source_id, &output).unwrap();
    };
    split(&lower, &format!("{SOLO_KEY}_9"));
    split(&higher, &format!("{SOLO_KEY}_5"));

    let after = work.replan(&rows);
    let lowest = work.out().join("chat").join(format!("{SOLO_KEY}_5"));
    assert_eq!(dir_of(&after, &lower), Some(lowest.clone()), "the lowest recorded directory wins, not the lowest row's");
    assert_eq!(dir_of(&after, &higher), Some(lowest), "and one conversation still gets exactly one directory");
}

/// Decision 52's own verify line, on the leg the defect was measured on.
///
/// Two items in one directory on one second take `<stem>.jpg` and `<stem>_2.jpg`. The second is
/// driven back to work, which per decision 50 CLEARS its output record; the first then leaves the
/// export, taking its position in the plan with it. Re-deriving the ordinal plans the survivor onto
/// the departed row's path and the run writes over a repaired file — after which the departed row
/// demotes, is never planned again because its source is gone, and retires, so nothing ever puts
/// that file back.
///
/// **Adoption cannot be what saves it here, and that is why this is the shape the queue named.** The
/// survivor's record is gone by the time the plan runs, so the only thing standing between it and
/// the departed row's file is the reservation of every path the manifest records.
///
/// The first assertion is the fixture's own self-guard: with both files painted alike the two
/// outputs are byte-identical and the digest comparison below cannot fail whatever the code does.
#[test]
fn an_item_leaving_the_export_does_not_shift_a_survivor_onto_its_finished_file() {
    let work = Workspace::new();
    let one = plain_shaded(&work.source(), Token::B, 1, 40);
    let two = plain_shaded(&work.source(), Token::B, 2, 220);
    let run = work.run(&no_history());
    assert_eq!(run.report.fixed, 2, "{:?}", run.report.failed);

    let bucket = work.out().join("chat/_no-conversation/2021/03");
    let recorded = |source_id: &str| {
        let row = run.row(source_id);
        (row.output_path.expect("a finished row records where it landed"), row.checksum.expect("and what it wrote there"))
    };
    let (one_output, one_digest) = recorded(&one);
    let (two_output, two_digest) = recorded(&two);
    assert_ne!(one_digest, two_digest, "the two outputs are byte-identical, so no digest below could see an overwrite");
    assert_eq!(
        [one_output.clone(), two_output.clone()].into_iter().collect::<BTreeSet<_>>(),
        [bucket.join("20210304_000000.jpg"), bucket.join("20210304_000000_2.jpg")].into_iter().collect::<BTreeSet<_>>(),
        "the fixture is not two items on one second in one directory"
    );
    // Which of the two took the plain name is the plan's answer, not this fixture's.
    let (departing, survivor) = if one_output == bucket.join("20210304_000000.jpg") { (one, two) } else { (two, one) };

    // Driven back to work: `Pending`, retry count zeroed, output record dropped, file left alone —
    // the state a resume writes when the user deletes an output, reached here without a run.
    run.manifest.reset(ItemKind::ChatMedia, &survivor).unwrap();
    fs::remove_file(chat_media_dir(&work.source()).join(format!("{DAY}_{departing}.jpg"))).unwrap();

    let after = work.run(&no_history());
    assert_eq!(after.report.fixed, 1, "{:?}", after.report.failed);

    // The assertion this test is NAMED for goes first, deliberately: a sibling assertion above it
    // aborts the body, and a red from there banks as a kill while this line never executes.
    let row = after.row(&departing);
    let output = row.output_path.expect("the departed row still records the file it finished");
    let digest = Checksum::of_file(&output).expect("and that file is still there").0;
    assert_eq!(Some(digest), row.checksum, "the departed item's repaired file was written over: {}", output.display());

    assert_eq!(
        after.row(&survivor).output_path.as_deref(),
        Some(bucket.join("20210304_000000_2.jpg").as_path()),
        "the survivor moved off its own name"
    );
}

/// Decision 52's ADOPTION half at run level: an item whose finished output the user deleted is
/// rewritten at the path it recorded, rather than at wherever a fresh walk would put it.
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
/// The live path, which is why this is not unit-testable: `chat_fix::plan` plans every reconciliation
/// item whatever its status, `local_fix::run` only then filters on what the manifest owes, and the
/// resume sweep sits between the two. So the survivor is adopted off a record that is about to be
/// cleared, demoted, and rewritten at the adopted path.
///
/// Without adoption both records are still RESERVED, so the two items derive `_3` and `_4`: the
/// survivor's file moves for no reason and the numbering is scrambled from then on. That is the
/// outcome this asserts against, and it is why dropping adoption alone stays green on the test above
/// and reds here — the two tests are the 2x2 decision 52a asks for.
///
/// **Dropping BOTH halves leaves this green, which is not a weakness.** With nothing reserved the
/// positional walk hands out the same two names it always did, so the two errors cancel on this
/// fixture. The discriminating mutation for this test is the adoption half alone; the test above is
/// the one that reds when the whole fix goes.
///
/// The demotion arm is named rather than assumed: a deleted output is
/// [`exportsnap::export::manifest::DemotionReason::Vanished`], which is a different arm of
/// `demotion_reason` from the one `an_output_that_changed_since_it_was_recorded_is_redone_rather_than_trusted`
/// exercises, and asserting it is what proves the sweep ran at all rather than the row being skipped.
#[test]
fn an_item_whose_output_was_deleted_is_rewritten_at_the_path_it_recorded() {
    let work = Workspace::new();
    let one = plain_shaded(&work.source(), Token::B, 1, 40);
    let two = plain_shaded(&work.source(), Token::B, 2, 220);
    let run = work.run(&no_history());
    assert_eq!(run.report.fixed, 2, "{:?}", run.report.failed);

    let bucket = work.out().join("chat/_no-conversation/2021/03");
    let recorded = |source_id: &str| run.row(source_id).output_path.expect("a finished row records where it landed");
    let (first, second) = (recorded(&one), recorded(&two));
    assert_eq!(
        [first.clone(), second.clone()].into_iter().collect::<BTreeSet<_>>(),
        [bucket.join("20210304_000000.jpg"), bucket.join("20210304_000000_2.jpg")].into_iter().collect::<BTreeSet<_>>(),
        "the fixture is not two items on one second in one directory"
    );
    // The one on the SUFFIXED name is the one with something to lose: its path is the one a
    // re-derivation would move. Which of the two that is, is the plan's answer, not this fixture's.
    let suffixed_output = bucket.join("20210304_000000_2.jpg");
    let suffixed = if first == suffixed_output { one } else { two };

    // Only the OUTPUT goes. Both sources stay, both rows keep their records, and nothing is reset.
    fs::remove_file(&suffixed_output).unwrap();

    let after = work.run(&no_history());
    assert_eq!(
        after.report.resumed.demoted.iter().map(|one| (one.source_id.as_str(), one.reason)).collect::<Vec<_>>(),
        [(suffixed.as_str(), DemotionReason::Vanished)],
        "the resume sweep did not demote the deleted output, so nothing below is about a rewrite"
    );
    assert_eq!(after.report.fixed, 1, "{:?}", after.report.failed);

    assert_eq!(
        after.row(&suffixed).output_path.as_deref(),
        Some(suffixed_output.as_path()),
        "the rewrite landed somewhere other than the file this item had already finished at"
    );
    assert!(suffixed_output.is_file(), "the recorded path was not written back");
}

#[test]
fn two_files_in_one_conversation_on_one_second_get_a_counted_suffix() {
    let history = history(vec![(
        SOLO_KEY,
        vec![message("a", "2021-03-04 14:30:05 UTC", "b~aB3xY90001"), message("a", "2021-03-04 14:30:05 UTC", "b~aB3xY90002")],
    )]);
    let plan = first_run(&from_names(&history, &["2021-03-04_b~aB3xY90001.jpg", "2021-03-04_b~aB3xY90002.jpg"]), "/out", OverlayMode::Both);
    assert_eq!(
        outputs(&plan),
        [Path::new("/out/chat/friend-handle/20210304_143005.jpg"), Path::new("/out/chat/friend-handle/20210304_143005_2.jpg"),]
    );
}

#[test]
fn two_conversations_with_a_file_on_one_second_do_not_collide_with_each_other() {
    // The collision key is the whole path and not the name: two directories, so no suffix at all.
    let history = history(vec![
        (SOLO_KEY, vec![message("a", "2021-03-04 14:30:05 UTC", "b~aB3xY90001")]),
        (GROUP_KEY, vec![message("b", "2021-03-04 14:30:05 UTC", "b~aB3xY90002")]),
    ]);
    let plan = first_run(&from_names(&history, &["2021-03-04_b~aB3xY90001.jpg", "2021-03-04_b~aB3xY90002.jpg"]), "/out", OverlayMode::Both);
    assert_eq!(
        outputs(&plan),
        [
            PathBuf::from(format!("/out/chat/{SOLO_KEY}/20210304_143005.jpg")).as_path(),
            PathBuf::from(format!("/out/chat/{GROUP_KEY}/20210304_143005.jpg")).as_path(),
        ]
    );
}

// ---- the date chain ----

#[test]
fn the_messages_created_outranks_everything_else() {
    let plan = first_run(
        &from_names(
            &history(vec![(SOLO_KEY, vec![message("a", "2021-03-04 14:30:05 UTC", "b~aB3xY90001")])]),
            &["2021-03-04_b~aB3xY90001.jpg"],
        ),
        "/out",
        OverlayMode::Both,
    );
    assert_eq!(plan.items[0].capture.source(), TimeSource::Message);
    assert_eq!(plan.items[0].capture.local().to_string(), "2021-03-04 14:30:05");
}

#[test]
fn a_file_the_message_did_not_date_falls_to_its_own_embedded_timestamp() {
    let work = Workspace::new();
    let named = plain(&work.source(), Token::B, 1);

    // Give the source a capture time of its own, through this crate's own writer.
    let path = work.source().join(format!("chat_media/{DAY}_{named}.jpg"));
    let mut jpeg = Jpeg::read(&path).unwrap();
    jpeg.stamp(&Stamp {
        local: chrono::NaiveDate::from_ymd_opt(2021, 3, 4).unwrap().and_hms_opt(9, 17, 42).unwrap(),
        offset: None,
        location: None,
        width: WIDTH,
        height: HEIGHT,
        attribution: None,
    })
    .unwrap();
    jpeg.write(&path).unwrap();

    // A message that names the file and states no `Created` at all: step 1 abstains, step 2 answers.
    let plan = work.plan(&history(vec![(SOLO_KEY, vec![message("a", "", &named)])]));
    assert_eq!(plan.items[0].capture.source(), TimeSource::Embedded);
    assert_eq!(plan.items[0].capture.local().to_string(), "2021-03-04 09:17:42");
    // Still filed under the conversation the message names, since only the DATE fell through.
    assert_eq!(plan.items[0].output, work.out().join("chat/friend-handle/20210304_091742.jpg"));
}

/// The epoch route reaching the writer, and the thing about it that is easy to get wrong.
///
/// `chat_media` chooses between `Created` and `Created(microseconds)`; this is the layer that
/// SPENDS the answer, and it spends it on two things — the instant in the filename and the
/// directory the item lands in. Only the first may move. A named item is filed flat under its
/// conversation key (the `YYYY/MM` bucket is the unnamed leg's, which no epoch-dated item can
/// reach), so the epoch has to restamp the name and leave the folder exactly where a `Created`
/// would have put it.
///
/// One assertion carries both halves, and the fixture separates them: the filename day is
/// `2021-03-04` and the epoch is 2020-07-26, so a build that ignored the epoch reds on the stamp
/// while a build that filed by date reds on the path.
///
/// [`TimeSource::Message`] is asserted alongside because it is design call 2 in observable form —
/// an epoch-stated instant is the message speaking rather than a fourth kind of source, and it
/// reaches a user as "the message that sent it".
#[test]
fn an_epoch_dated_message_stamps_its_instant_without_moving_the_item() {
    let named = format!("b~{}", id(1));
    // `Created` empty, so step 1 falls through to the epoch: 2020-07-26 15:48:05.675 UTC.
    let entry = schema::ChatEntry { created_epoch: Some(1_595_778_485_675), ..message("sender-handle", "", &named) };
    let history = history(vec![(SOLO_KEY, vec![entry])]);
    let plan = first_run(&from_names(&history, &[&format!("{DAY}_{named}.jpg")]), "/out", OverlayMode::Both);

    // The both-halves assertion goes FIRST, deliberately: it is the one this test is named for, and
    // a field-level assert ahead of it would abort the body before the path was ever read.
    assert_eq!(plan.items[0].output, Path::new("/out/chat/friend-handle/20200726_154805.jpg"));
    assert_eq!(plan.items[0].capture.local().to_string(), "2020-07-26 15:48:05");
    assert_eq!(plan.items[0].capture.source(), TimeSource::Message, "an epoch is the message speaking, not a fourth source");
}

#[test]
fn a_file_nothing_dates_falls_to_the_day_in_its_own_filename_at_midnight() {
    let work = Workspace::new();
    plain(&work.source(), Token::B, 1);
    let plan = work.plan(&no_history());
    assert_eq!(plan.items[0].capture.source(), TimeSource::Filename);
    assert_eq!(plan.items[0].capture.local().to_string(), "2021-03-04 00:00:00");
}

#[test]
fn no_chat_media_item_ever_carries_a_coordinate() {
    // `chat_history.json` states no location anywhere, so there is nothing to stamp and no timezone
    // lookup to run. Asserted rather than left to an absent call, which reads as an oversight.
    let history = history(vec![(SOLO_KEY, vec![message("a", "2021-03-04 14:30:05 UTC", "b~aB3xY90001")])]);
    let plan = first_run(
        &from_names(&history, &["2021-03-04_b~aB3xY90001.jpg", "2021-03-04_b~aB3xY90002.jpg", "2021-03-04_overlay~aB3xY90003.png"]),
        "/out",
        OverlayMode::Both,
    );
    assert_eq!(plan.items.len(), 3);
    assert!(plan.items.iter().all(|item| item.location.is_none()), "{:?}", plan.items);
    // And with no coordinate the wall time stays UTC and says so, so the instant survives.
    let named = plan.items.iter().find(|item| item.source_id == "b~aB3xY90001").unwrap();
    assert_eq!(named.capture.offset().map(|offset| offset.local_minus_utc()), Some(0));
}

// ---- decision 44c: what reaches the metadata ----

#[test]
fn the_sender_and_the_conversation_reach_the_outputs_metadata() {
    if !common::usable("the_sender_and_the_conversation_reach_the_outputs_metadata", &[Tool::Exiftool]) {
        return;
    }
    let work = Workspace::new();
    let named = plain(&work.source(), Token::B, 1);
    let run = work.run(&history(vec![(GROUP_KEY, vec![message("sender-handle", "2021-03-04 14:30:05 UTC", &named)])]));

    let attribution = run.plan.items[0].attribution.as_ref().expect("a named file carries one");
    assert_eq!(attribution.sender.as_ref().map(|from| from.as_str()), Some("sender-handle"));
    // The KEY, never the per-message title: a group renamed mid-thread carries two titles.
    assert_eq!(attribution.conversation.as_ref().map(|key| key.as_str()), Some(GROUP_KEY));

    let output = work.out().join(format!("chat/{GROUP_KEY}/20210304_143005.jpg"));
    let tags = exiftool(&output);
    // Read back through an independent reader, not through `little_exif`: a crate reading its own
    // write can agree with itself about a wrong encoding.
    assert_eq!(tags.get("Artist").map(String::as_str), Some("sender-handle"), "{tags:?}");
    assert_eq!(tags.get("ImageDescription").map(String::as_str), Some(GROUP_KEY), "{tags:?}");
    assert_eq!(tags.get("DateTimeOriginal").map(String::as_str), Some("2021:03:04 14:30:05"), "{tags:?}");
    // No coordinate, on either spelling exiftool would resolve one under.
    assert_eq!(tags.get("GPSPosition"), None, "{tags:?}");
    assert_eq!(tags.get("GPSLatitude"), None, "{tags:?}");
}

#[test]
fn a_file_no_message_names_carries_no_sender_and_no_conversation() {
    let plan = first_run(&from_names(&no_history(), &["2021-03-04_b~aB3xY90001.jpg"]), "/out", OverlayMode::Both);
    assert_eq!(plan.items[0].attribution, None);
}

#[test]
fn an_empty_conversation_key_is_absence_rather_than_an_empty_metadata_field() {
    // `ConversationId::new` accepts `""` on purpose — the thread behind an empty key still holds its
    // records — so this is reachable, and both ends have to answer for it: an empty string written
    // into `ImageDescription` is noise, and an empty directory name is not a name at all.
    let history = history(vec![("", vec![message("sender-handle", "2021-03-04 14:30:05 UTC", "b~aB3xY90001")])]);
    let plan = first_run(&from_names(&history, &["2021-03-04_b~aB3xY90001.jpg"]), "/out", OverlayMode::Both);

    let attribution = plan.items[0].attribution.as_ref().expect("the message still names a sender");
    assert_eq!(attribution.sender.as_ref().map(|from| from.as_str()), Some("sender-handle"));
    assert_eq!(attribution.conversation, None, "an empty key is absence, not an empty value");
    assert_eq!(plan.items[0].output, Path::new("/out/chat/_unnamed/20210304_143005.jpg"));
}

#[test]
fn a_memories_plan_still_carries_no_attribution_and_no_originals() {
    // The refactor that gave both legs one `PlannedItem` must not have handed the memories leg
    // either field. Pinned here rather than only in `tests/local_fix.rs`, because this is the change
    // that introduced them.
    let memories = exportsnap::export::model::Memories { saved_media: vec![] };
    let reconciliation = exportsnap::export::memories::reconcile(&memories, exportsnap::export::memories::Discovery::default());
    let plan = Plan::build(&memories, &reconciliation, "/out", &RecordedOutputs::default());
    assert_eq!(plan.kind, ItemKind::Memory);
    assert!(plan.excluded.is_empty());
    assert!(plan.items.iter().all(|item| item.attribution.is_none() && item.originals.is_none()));
}

// ---- the run's boundaries ----

#[test]
fn a_run_writes_nothing_outside_the_out_root() {
    let work = Workspace::new();
    let named = plain(&work.source(), Token::B, 1);
    plain(&work.source(), Token::Thumbnail, 3);
    zip_pair(&work.source(), 4);
    let before = tree(&work.source());

    // The out root is the source's sibling: the run may write under it and nowhere else.
    let run = work.run(&history(vec![(SOLO_KEY, vec![message("a", "2021-03-04 14:30:05 UTC", &named)])]));
    assert_eq!(run.report.fixed, 2);

    assert_eq!(tree(&work.source()), before, "the source must stay read-only, originals kept or not");
    assert_eq!(
        tree(&work.out()),
        [
            "chat/_no-conversation/2021/03/20210304_000000.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_media~vantsnap-0000004.zip.a1b2c3d.jpg",
            "chat/_no-conversation/2021/03/originals/2021-03-04_overlay~vantsnap-0000004.zip.a1b2c3d.png",
            "chat/friend-handle/20210304_143005.jpg",
        ]
    );
    // And nothing anywhere else under the workspace but the manifest.
    assert!(tree(&work.state()).iter().all(|path| path.starts_with(EXPORT_ID)), "{:?}", tree(&work.state()));
}

#[test]
fn a_format_this_build_does_not_decode_is_deferred_rather_than_excluded() {
    // `.gif` and `.webp` are 20 of the observed export's plain `b` files. Deferring leaves the row
    // `Pending` so a later build picks it up, which is the side of the line decision 44d's excluded
    // thumbnails are not on.
    let plan =
        first_run(&from_names(&no_history(), &["2021-03-04_b~aB3xY90001.gif", "2021-03-04_b~aB3xY90002.webp"]), "/out", OverlayMode::Both);
    assert!(plan.items.is_empty());
    assert!(plan.excluded.is_empty());
    assert_eq!(
        plan.deferred.iter().map(|deferred| deferred.reason).collect::<Vec<_>>(),
        [DeferralReason::UnknownFormat, DeferralReason::UnknownFormat]
    );
}

#[test]
fn a_second_run_rewrites_nothing_it_already_finished() {
    let work = Workspace::new();
    plain(&work.source(), Token::B, 1);
    let mut run = work.run(&no_history());
    assert_eq!(run.report.fixed, 1);

    let second = local_fix::run(&run.plan, &mut run.manifest, 3, &copying()).unwrap();
    assert_eq!((second.fixed, second.skipped), (0, 1));
    assert_eq!(second.resumed.verified, 1, "the finished output re-hashed to exactly what was recorded");
    assert!(second.resumed.demoted.is_empty(), "{:?}", second.resumed.demoted);
}

/// Plan, run, then plan twice more over an unchanged export: one answer every time.
///
/// The adoption is a new input to the plan, so "the same answer on a second run" is now a different
/// question from the derive-only one `the_collision_suffix_is_the_same_answer_on_a_second_run` asks.
#[test]
fn a_replan_over_an_unchanged_export_is_the_same_answer_every_time() {
    let work = Workspace::new();
    let one = plain(&work.source(), Token::B, 1);
    let two = plain(&work.source(), Token::B, 2);
    let rows = history(vec![
        ("a/b", vec![message("a", "2021-03-04 14:30:05 UTC", &one)]),
        ("a?b", vec![message("b", "2021-03-04 14:30:05 UTC", &two)]),
    ]);
    let run = work.run(&rows);
    assert_eq!(run.report.fixed, 2);

    let recorded_dir = |source_id: &str| -> PathBuf {
        let output = run.row(source_id).output_path.expect("a finished row records where it landed");
        output.parent().expect("an output is inside a directory").to_path_buf()
    };
    let again = work.replan(&rows);
    assert_eq!(dir_of(&again, &one), Some(recorded_dir(&one)));
    assert_eq!(dir_of(&again, &two), Some(recorded_dir(&two)));
    assert_eq!(outputs(&again), outputs(&work.replan(&rows)), "a third pass disagreed with the second");
}

/// The reservation asks whether a row NAMES a directory, never what status it carries, and a retired
/// row is what separates those two questions.
///
/// Every step here is a production writer and the chain is ordinary: the file goes while the thread
/// still names its token, so the row parks and keeps its record; then the thread leaves the history
/// outright, so nothing names the row and it retires — still keeping the record. A reservation that
/// asked for `done` would drop that row and hand a departed thread's directory to the newcomer,
/// which is F2's hole arriving by a different status.
#[test]
fn a_retired_rows_record_still_reserves_the_directory_it_names() {
    let work = Workspace::new();
    let leaving = plain(&work.source(), Token::B, 1);
    let staying = plain(&work.source(), Token::B, 2);
    let sent = |id: &str| vec![message("a", "2021-03-04 14:30:05 UTC", id)];
    let both = || history(vec![("a/b", sent(&leaving)), ("a?b", sent(&staying))]);
    let run = work.run(&both());
    assert_eq!(run.report.fixed, 2);
    let departed = run.row(&leaving).output_path.clone().expect("run 1 finished it");
    assert_eq!(departed.parent(), Some(work.out().join("chat").join("a_b").as_path()));

    // The file goes while the thread still names its token: the row parks and keeps its record.
    fs::remove_file(chat_media_dir(&work.source()).join(format!("{DAY}_{leaving}.jpg"))).unwrap();
    let second = work.run(&both());
    assert_eq!(second.row(&leaving).status, ItemStatus::SourceMissing, "a named token with no file parks");

    // Now the thread leaves the history too, so nothing names the row at all and it retires.
    let arriving = plain(&work.source(), Token::B, 3);
    let rows = history(vec![("a:b", sent(&arriving)), ("a?b", sent(&staying))]);
    let third = work.run(&rows);
    assert_eq!(third.report.fixed, 1, "{:?}", third.report.failed);
    assert_eq!(third.row(&leaving).status, ItemStatus::Retired, "a row nothing names retires");
    assert_eq!(third.row(&leaving).output_path, Some(departed.clone()), "and keeps the record naming its directory");

    let arrived_dir = third.row(&arriving).output_path.clone().and_then(|path| path.parent().map(Path::to_path_buf));
    assert_ne!(arrived_dir, departed.parent().map(Path::to_path_buf), "a new key took a retired thread's directory");
    assert_eq!(arrived_dir, Some(work.out().join("chat").join("a_b_3")), "and it takes the first name nothing records");
}
