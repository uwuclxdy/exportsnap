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
//! The image leg carries the behavioural coverage here on purpose. Both legs go through one
//! `local_fix::fix`, whose video half is pinned by `tests/local_fix.rs` and `tests/video.rs` against
//! real ffmpeg output; what is chat-specific is the plan, and the plan asks nothing about a leg
//! beyond the extension it writes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use exportsnap::export::chat_fix::{self, OverlayMode, RecordedDirs, dir_name};
use exportsnap::export::chat_media::{ChatMediaFile, Discovery, Reconciliation, Token, discover, reconcile};
use exportsnap::export::exif::{Jpeg, Stamp};
use exportsnap::export::local_fix::{self, DeferralReason, FixReport, Notice, Plan, TimeSource, VideoOptions};
use exportsnap::export::manifest::{ExportId, Item, ItemKind, ItemStatus, Manifest};
use exportsnap::export::model::ChatHistory;
use exportsnap::export::schema;
use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use tempfile::TempDir;

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

/// Set this and a missing exiftool fails the run instead of quietly covering nothing.
const REQUIRE_EXIFTOOL: &str = "EXPORTSNAP_REQUIRE_EXIFTOOL";

// ---- fixtures ----

/// A distinct alphanumeric id per `seed`, in the shape a plain filename carries.
fn id(seed: u32) -> String {
    format!("aB3xY9{seed:04}")
}

/// The colour [`paint_jpeg`] paints at `(x, y)`, which is what a transparent overlay region has to
/// leave showing.
fn main_colour(x: u32, y: u32) -> [u8; 3] {
    [(x % 7) as u8 * 5, ((x * 13 + y * 7) % 251) as u8, ((x * 29 + y * 17) % 253) as u8]
}

/// Asserts the overlay's transparent region left the MAIN showing through, on all three channels.
///
/// **What used to stand here was a brightness threshold on subpixel 0, and it could not discriminate
/// at all.** The main's red in the asserted region is 20 against black's 0, so `< 60` passed whether
/// the transparent half showed the main through or the alpha had been dropped to black — the fixture
/// held the asserted channel near-constant across the two outcomes, which is this repo's own recorded
/// trap. Green and blue separate them, and matching all three also reds on a composite onto any
/// invented background rather than only on black.
///
/// **Asserted as a block MEAN rather than as one pixel, and that is the load-bearing half.** The main
/// fixture is high-frequency by design and JPEG's DCT smears neighbours: measured on these fixtures a
/// lone pixel drifts up to 21 levels, close enough to the gap being detected that a per-pixel
/// tolerance would be guessing. A block mean is essentially the DC coefficient, which JPEG preserves
/// closely, so the margin to the failure it must catch is about 125 on the chroma channels.
fn assert_shows_main_through(composite: &RgbImage, label: &str) {
    /// A block wholly inside the overlay's transparent half and away from its edge.
    const BLOCK: [u32; 4] = [48, 8, 56, 16];
    /// Comfortably above the drift a preserved block mean shows and far below the ~125 that
    /// separates the main from black on green and blue.
    const TOLERANCE: f64 = 8.0;

    let [left, top, right, bottom] = BLOCK;
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
        assert!(drift.abs() <= TOLERANCE, "{label}: channel {channel} averaged {actual:?} over {BLOCK:?}, expected about {expected:?}");
    }
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
    let mut pixels = RgbaImage::new(WIDTH, HEIGHT);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < WIDTH / 2 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 0, 0]) };
    }
    pixels.save_with_format(path, ImageFormat::Png).unwrap();
}

/// A plain-family file on disk. Returns the `<token>~<id>` stem, which is also its manifest id.
fn plain(root: &Path, token: Token, seed: u32) -> String {
    let stem = format!("{}~{}", token.as_word(), id(seed));
    paint_jpeg(&chat_media_dir(root).join(format!("{DAY}_{stem}.jpg")));
    stem
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
/// a filename operation that never opens either file, so the extension is free, and keeping both
/// halves on the image leg is what lets this crate assert on composited PIXELS with no ffmpeg on the
/// box.
fn zip_pair(root: &Path, seed: u32) -> String {
    let mid = format!("{ZIP_WORD}-{seed:07}");
    let dir = chat_media_dir(root);
    paint_jpeg(&dir.join(format!("{DAY}_media~{mid}.zip.a1b2c3d.jpg")));
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
        let reconciliation = reconcile(history, discover(self.source()).unwrap());
        let mut manifest = self.manifest();
        reconciliation.enroll(&mut manifest).unwrap();
        let recorded = RecordedDirs::read(&reconciliation, &manifest).unwrap();
        let plan = chat_fix::plan(&reconciliation, self.out(), mode, &recorded);
        let report = local_fix::run(&plan, &mut manifest, 3, &copying()).unwrap();
        Run { plan, manifest, report }
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

/// The run this crate drives: no re-encode and no ffmpeg, so nothing here depends on a tool being
/// installed. The chat leg's plan is what is under test, and it is the same plan either way.
fn copying() -> VideoOptions {
    VideoOptions { transcode: false, ffmpeg: None }
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

/// exiftool's view of a file, keyed by tag name, or `None` when it is not installed.
///
/// The same shape `tests/local_fix.rs` reads its outputs with, down to the `": "` split: the group
/// prefix and every date value hold a colon, and only the separator is followed by a space.
fn exiftool(path: &Path) -> Option<BTreeMap<String, String>> {
    let output = Command::new("exiftool").args(["-s", "-a", "-G0:1", "-All"]).arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Some(
        text.lines()
            .filter_map(|line| line.split_once(": "))
            .map(|(key, value)| {
                // `[EXIF:IFD0]  Artist` -> `Artist`.
                (key.rsplit(']').next().unwrap_or(key).trim().to_owned(), value.trim().to_owned())
            })
            .collect(),
    )
}

fn skipped(test: &str) {
    assert!(
        std::env::var_os(REQUIRE_EXIFTOOL).is_none(),
        "{test}: {REQUIRE_EXIFTOOL} is set and exiftool is not on PATH, so the assertions that need it would have \
         been skipped; install exiftool on this runner or unset the variable"
    );
    println!("SKIPPED {test}: exiftool is not on PATH, so its assertions did not run");
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
    assert_shows_main_through(&merged, "the transparent half is the main showing through");
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
            assert_shows_main_through(&written, &format!("{mode}: the transparent half is the main"));
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

/// Under `originals` the caption never reaches the item-level pass, so decision 47's copy-through
/// rule applies to a PNG main that pairs — the same rule one mode over, and the one place a mode
/// moves an output PATH.
///
/// Unreachable from the observed export (only the zip family pairs and every zip main is a video),
/// which is why it is asserted here rather than left to a reader to infer from `passes_through`.
#[test]
fn a_png_main_that_pairs_is_copied_through_under_originals_and_re_encoded_otherwise() {
    let names = ["2021-03-04_media~vantsnap-0000009.zip.a1b2c3d.png", "2021-03-04_overlay~vantsnap-0000009.zip.a1b2c3d.png"];
    let merged = first_run(&from_names(&no_history(), &names), "/out", OverlayMode::Both);
    let kept = first_run(&from_names(&no_history(), &names), "/out", OverlayMode::Originals);

    assert_eq!(
        merged.items[0].output,
        Path::new("/out/chat/_no-conversation/2021/03/20210304_000000.jpg"),
        "compositing a PNG ends in a JPEG encode"
    );
    assert_eq!(
        kept.items[0].output,
        Path::new("/out/chat/_no-conversation/2021/03/20210304_000000.png"),
        "with nothing to composite the bytes are copied through under their own extension"
    );
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

/// The pass-through membership test is ascii-case-insensitive, so a `.PNG` source is admitted — and
/// then its extension has to be normalized before it reaches an output path, or the same file
/// spelled two ways lands at two names. Both planners key their collision map on that same string,
/// so a divergence moves output paths rather than staying cosmetic, and a case-folding filesystem
/// would have the two spellings fighting over one directory entry.
///
/// This pins the normalization and **not** the length-independence of `PASS_THROUGH_EXTENSIONS`,
/// which no shipped test can pin: while that list holds one member, indexing it and reading the
/// item's own extension answer identically on every input that exists. The evidence for that is a
/// mutation, recorded in the round's table, not an assertion here.
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
fn a_png_that_does_have_an_overlay_still_composites_to_a_stamped_jpeg() {
    // Constraint 6: decision 47 is about the LONE case. A zip pair whose media half is a png still
    // goes through the compositor and still lands as a stamped JPEG, which is also what keeps
    // `little_exif` on its JPEG path.
    let work = Workspace::new();
    let mid = format!("{ZIP_WORD}-0000006");
    let dir = chat_media_dir(&work.source());
    paint_overlay(&dir.join(format!("{DAY}_media~{mid}.zip.a1b2c3d.png")));
    paint_overlay(&dir.join(format!("{DAY}_overlay~{mid}.zip.a1b2c3d.png")));

    let run = work.run(&no_history());
    assert_eq!(run.report.fixed, 1);
    let written = work.out().join("chat/_no-conversation/2021/03/20210304_000000.jpg");
    assert!(written.is_file(), "{:?}", tree(&work.out()));
    assert_eq!(&fs::read(&written).unwrap()[..3], &[0xff, 0xd8, 0xff], "a composited png still lands as a jpeg");
    assert!(run.report.notices.is_empty(), "a composited item is fully repaired: {:?}", run.report.notices);
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
    let work = Workspace::new();
    let named = plain(&work.source(), Token::B, 1);
    let run = work.run(&history(vec![(GROUP_KEY, vec![message("sender-handle", "2021-03-04 14:30:05 UTC", &named)])]));

    let attribution = run.plan.items[0].attribution.as_ref().expect("a named file carries one");
    assert_eq!(attribution.sender.as_ref().map(|from| from.as_str()), Some("sender-handle"));
    // The KEY, never the per-message title: a group renamed mid-thread carries two titles.
    assert_eq!(attribution.conversation.as_ref().map(|key| key.as_str()), Some(GROUP_KEY));

    let output = work.out().join(format!("chat/{GROUP_KEY}/20210304_143005.jpg"));
    let Some(tags) = exiftool(&output) else {
        skipped("the_sender_and_the_conversation_reach_the_outputs_metadata");
        return;
    };
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
    let plan = Plan::build(&memories, &reconciliation, "/out");
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
