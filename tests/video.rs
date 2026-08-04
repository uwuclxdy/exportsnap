//! Public-API tests for `exportsnap::export::video`: the header-time patch, the coordinate and
//! local-date tags, the `udta` shadow rule, and what the guard type refuses.
//!
//! Nothing here reads a real export. Every fixture is built by ffmpeg from a synthetic colour
//! source in a tempdir, or assembled byte by byte in the test.
//!
//! **The metadata assertions read the output back through `exiftool` and `ffprobe`, not through
//! `mp4ameta`.** A crate reading its own write can agree with itself about a wrong encoding, which
//! is exactly what an independent reader is for. Neither tool is a build dependency, so those tests
//! print a skip notice and pass when one is absent — see [`REQUIRE_FFMPEG`] for the env var that
//! turns absence into a failure on a runner.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{FixedOffset, NaiveDate, NaiveDateTime};
use exportsnap::export::model::{Field, LocationPoint};
use exportsnap::export::video::{LocationAtom, Mp4, NotMp4, VideoError, VideoStamp, header_time};
use tempfile::TempDir;

/// Paris, with non-zero degrees, minutes and seconds so a dropped component shows in a round trip.
const PARIS: &str = "Latitude, Longitude: 48.858844, 2.294351";
/// Southern and western, and far enough away that no rounding confuses the two.
const RIO: &str = "Latitude, Longitude: -22.951916, -43.210487";

/// Set any of these and a missing tool fails the run instead of quietly covering nothing.
///
/// The skip notices below cannot be relied on: nextest captures a passing test's output, so on a
/// box without the tools the suite prints nothing at all and reads as fully green. That is fine
/// while the box this repo is gated on has ffmpeg n8.1.2, ffprobe and exiftool 13.55 installed, and
/// it is exactly wrong for a CI runner, where these tests are the only independent-reader coverage
/// of the header-time encoding and the coordinate form.
const REQUIRE_FFMPEG: &str = "EXPORTSNAP_REQUIRE_FFMPEG";
const REQUIRE_EXIFTOOL: &str = "EXPORTSNAP_REQUIRE_EXIFTOOL";

/// Records why a check did not run. Loud where the caller asked for loud.
fn skipped(test: &str, tool: &str, variable: &str) {
    assert!(
        std::env::var_os(variable).is_none(),
        "{test}: {variable} is set and {tool} is not on PATH, so the assertions that need it would have been \
         skipped; install {tool} on this runner or unset the variable"
    );
    println!("SKIPPED {test}: {tool} is not on PATH, so its assertions did not run");
}

// ---- fixtures ----

/// A half-second HEVC video with an audio track, which is the shape every memory video has.
///
/// HEVC on purpose rather than for speed: `hvc1` is what the export ships and what the transcode
/// exists to move away from, so a fixture in anything else would let a leg that quietly skipped the
/// re-encode pass.
fn hevc(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    let built = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=blue:s=64x48:r=15:d=0.5"])
        .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono", "-shortest"])
        .args(["-c:v", "libx265", "-tag:v", "hvc1", "-pix_fmt", "yuv420p", "-c:a", "aac", "-t", "0.5"])
        .arg(&path)
        .output()
        .ok()?;
    assert!(built.status.success(), "ffmpeg could not build the fixture: {}", String::from_utf8_lossy(&built.stderr));
    Some(path)
}

/// The same, with the movie box moved in front of the media data.
///
/// Worth having as its own fixture: `header_time` seeks over the top-level boxes, and the two
/// layouts put `moov` on opposite sides of a multi-megabyte `mdat` on a real file.
fn faststart(dir: &Path, name: &str) -> Option<PathBuf> {
    let source = hevc(dir, "source-for-faststart.mp4")?;
    let path = dir.join(name);
    let built = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&source)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&path)
        .output()
        .ok()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    Some(path)
}

/// Replaces the file's `moov/udta` with one holding exactly `child`, which is how a real memory
/// video's shape is reproduced: a `udta` child present and no `meta` at all, so the tag write has
/// to create that structure rather than splice into one.
fn with_udta_child(source: &Path, dest: &Path, fourcc: &[u8; 4], payload: &[u8]) {
    let bytes = fs::read(source).unwrap();
    let mut out = Vec::new();
    for (at, size, kind) in boxes(&bytes, 0, bytes.len()) {
        if kind != *b"moov" {
            out.extend(&bytes[at..at + size]);
            continue;
        }
        let mut body: Vec<u8> = Vec::new();
        for (kid_at, kid_size, kid) in boxes(&bytes, at + 8, at + size) {
            if kid != *b"udta" {
                body.extend(&bytes[kid_at..kid_at + kid_size]);
            }
        }
        body.extend(wrap(b"udta", &wrap(fourcc, payload)));
        out.extend(wrap(b"moov", &body));
    }
    fs::write(dest, out).unwrap();
}

fn wrap(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(8 + body.len()).unwrap().to_be_bytes().to_vec();
    bytes.extend(fourcc);
    bytes.extend(body);
    bytes
}

/// `(offset, size, fourcc)` for every box in `bytes[at..end]`. Only the plain 32-bit size form,
/// which is all ffmpeg writes at these sizes.
fn boxes(bytes: &[u8], mut at: usize, end: usize) -> Vec<(usize, usize, [u8; 4])> {
    let mut found = Vec::new();
    while at + 8 <= end {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let fourcc: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
        assert!(size >= 8 && at + size <= end, "fixture box {fourcc:?} at {at} does not fit");
        found.push((at, size, fourcc));
        at += size;
    }
    found
}

/// The bytes of the first box with this fourcc, header included.
fn box_bytes(bytes: &[u8], fourcc: &[u8; 4]) -> Vec<u8> {
    let (at, size, _) = boxes(bytes, 0, bytes.len()).into_iter().find(|(_, _, kind)| kind == fourcc).expect("no such box");
    bytes[at..at + size].to_vec()
}

fn point(text: &str) -> LocationPoint {
    LocationPoint::parse(Field::Location, text).unwrap()
}

fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(year, month, day).unwrap().and_hms_opt(hour, minute, second).unwrap()
}

// ---- synthetic-fixture builders (no external tools) ----

/// A version-0 `mvhd`/`tkhd`/`mdhd` body: version and flags, creation and modification times, a
/// timescale, then zero filler out to `content`.
///
/// The sizes are the ones `mp4ameta` checks against its own constants (mvhd 100, tkhd 84, mdhd 24
/// at version 0), so a stub would satisfy only this crate's walker and only ever exercise half the
/// stamp. The fixture bodies never carry a real capture time: raw 0 is the "never written" state.
fn header_body(raw: u32, content: usize) -> Vec<u8> {
    let mut body = vec![0, 0, 0, 0];
    body.extend(raw.to_be_bytes());
    body.extend(raw.to_be_bytes());
    body.extend(1000_u32.to_be_bytes());
    body.resize(content, 0);
    body
}

/// A track carrying `stbl` under `mdia/minf`: a spec-sized `tkhd` and `mdhd`, and nothing else.
fn trak_with(stbl: &[u8]) -> Vec<u8> {
    let mdhd = wrap(b"mdhd", &header_body(0, 24));
    let mdia = wrap(b"mdia", &[mdhd, wrap(b"minf", stbl)].concat());
    let tkhd = wrap(b"tkhd", &header_body(0, 84));
    wrap(b"trak", &[tkhd, mdia].concat())
}

/// A `stbl` holding `stsd` and a `co64` chunk table.
///
/// Spec-sized because `mp4ameta` checks both against its own constants on the write path: `stsd`
/// must be version 0 with at least 8 content bytes, and `co64` version 0 with exactly
/// `8 + 8 * entries` content bytes.
fn stbl_with_co64(offsets: &[u64]) -> Vec<u8> {
    let mut body = vec![0, 0, 0, 0];
    body.extend(u32::try_from(offsets.len()).unwrap().to_be_bytes());
    for offset in offsets {
        body.extend(offset.to_be_bytes());
    }
    wrap(b"stbl", &[wrap(b"stsd", &[0; 8]), wrap(b"co64", &body)].concat())
}

/// A small file carrying a real `co64` chunk table: `ftyp`, a `moov` holding one track whose
/// sample table is a `stsd` plus a 64-bit offset table, and an `mdat` the offsets point at.
///
/// The table shape is the point: a real >4 GiB video is this same layout with an `mdat` so large
/// its chunk offsets stop fitting in 32 bits. Each offset names a distinct 8-byte marker inside
/// `mdat` (first byte `0x80 + i`, rest `0x11`), so a wrong shift in a rewrite shows up as the
/// wrong marker.
fn with_co64(entries: usize) -> Vec<u8> {
    // Build once with placeholder offsets to fix `moov`'s size, which fixes where `mdat` starts.
    // The offset values do not change any box size (a co64's size depends only on its entry
    // count), so both passes land on the same layout and the offsets can point at markers inside
    // `mdat` rather than drifting back into `moov`.
    let mvhd = wrap(b"mvhd", &header_body(0, 100));
    let moov = wrap(b"moov", &[mvhd.clone(), trak_with(&stbl_with_co64(&vec![0; entries]))].concat());
    let mut ftyp = wrap(b"ftyp", b"isom\0\0\x02\0isommp42");
    let mdat_start = ftyp.len() + moov.len() + 8;
    // Then rebuild with offsets pointing at `entries` distinct markers inside `mdat`.
    let offsets: Vec<u64> = (0..entries).map(|i| (mdat_start + 8 + 8 * i) as u64).collect();
    let moov = wrap(b"moov", &[mvhd, trak_with(&stbl_with_co64(&offsets))].concat());
    let mut content = vec![0x11; 8 + 8 * entries];
    for i in 0..entries {
        content[8 + 8 * i] = 0x80 + i as u8;
    }
    ftyp.extend(moov);
    ftyp.extend(wrap(b"mdat", &content));
    ftyp
}

/// A fragmented file: `moov` carries `mvex`, and `moof`/`mdat`/`mfra` all sit before it at the top
/// level, as they do on a file whose movie box is written at finalization.
///
/// Every box the metadata write could move precedes `moov`, so a splice inside `moov` must leave
/// each of them byte-identical: the `trun` sample offsets are relative to the `moof`, and the
/// `tfra` entry names the `moof` by an absolute offset (24, its position in this layout) that must
/// stay true. The box bodies are shape-only — spec-plausible, and nothing in this crate or in
/// `mp4ameta` reads them.
fn fragmented() -> Vec<u8> {
    let trex = wrap(b"trex", &[0; 24]);
    let mvex = wrap(b"mvex", &trex);
    let trak = trak_with(&wrap(b"stsd", &[0; 8]));
    let moov = wrap(b"moov", &[wrap(b"mvhd", &header_body(0, 100)), trak, mvex].concat());

    let mfhd = wrap(b"mfhd", &[0; 8]);
    let trun = wrap(b"trun", &[0; 20]);
    let moof = wrap(b"moof", &[mfhd, wrap(b"traf", &trun)].concat());

    // One fragment-random-access entry: 1-byte traf/trun/sample numbers (length word 0x11000000),
    // then time, the absolute moof offset, and the three numbers.
    let mut tfra_body = vec![0, 0, 0, 0];
    tfra_body.extend(1_u32.to_be_bytes());
    tfra_body.extend(0x11000000_u32.to_be_bytes());
    tfra_body.extend(0_u32.to_be_bytes());
    tfra_body.extend(24_u32.to_be_bytes());
    tfra_body.extend([1, 1, 1]);
    let mfra = wrap(b"mfra", &wrap(b"tfra", &tfra_body));

    let mut bytes = wrap(b"ftyp", b"isom\0\0\x02\0isommp42");
    bytes.extend(moof);
    bytes.extend(wrap(b"mdat", &[0xdd; 16]));
    bytes.extend(mfra);
    bytes.extend(moov);
    bytes
}

/// The declared size of the top-level `moov` box.
fn moov_size(bytes: &[u8]) -> usize {
    boxes(bytes, 0, bytes.len()).into_iter().find(|(_, _, kind)| kind == b"moov").map(|(_, size, _)| size).expect("no moov")
}

/// The `(offset, size)` of the first direct child of the box at `bytes[at..at + size]` with this
/// fourcc.
fn child(bytes: &[u8], at: usize, size: usize, fourcc: &[u8; 4]) -> (usize, usize) {
    boxes(bytes, at + 8, at + size)
        .into_iter()
        .find(|(_, _, kind)| kind == fourcc)
        .map(|(offset, size, _)| (offset, size))
        .expect("the fixture box does not hold the expected child")
}

// ---- the independent readers ----

/// Every tag `exiftool` reports for `path`, keyed by its bare name.
///
/// `-a` keeps duplicate rows (the five header date fields print as three without it), `-u` surfaces
/// a `udta` child that is neither table-known nor `©`-prefixed, `-s` gives the short tag id, and
/// `-G1` puts the group in front of it. `None` means `exiftool` is not installed.
fn exiftool(path: &Path) -> Option<BTreeMap<String, String>> {
    let output = Command::new("exiftool").args(["-s", "-a", "-u", "-G1"]).arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Some(
        text.lines()
            // `": "` rather than `':'`: the group prefix and every date value hold a colon, and
            // only the separator is followed by a space.
            .filter_map(|line| line.split_once(": "))
            .map(|(key, value)| (key.rsplit(']').next().unwrap_or(key).trim().to_owned(), value.trim().to_owned()))
            .collect(),
    )
}

/// The container-level tags `ffprobe` reports, keyed by name.
fn ffprobe(path: &Path) -> Option<BTreeMap<String, String>> {
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

// ---- what the guard refuses ----

#[test]
fn nothing_but_a_walkable_mp4_can_be_handed_to_the_metadata_writer() {
    for (name, bytes) in [
        ("png", b"\x89PNG\r\n\x1a\n".to_vec()),
        ("jpeg", vec![0xff, 0xd8, 0xff, 0xe0]),
        ("empty", Vec::new()),
        ("a bare fourcc with no size", b"ftyp".to_vec()),
        // Opens with a real `ftyp` and then claims a box longer than the file: a truncated
        // download, which a signature prefix test admits.
        ("ftyp then a box longer than the file", [wrap(b"ftyp", b"isom"), b"\xff\xff\xff\xffmoov".to_vec()].concat()),
    ] {
        assert!(Mp4::new(bytes).is_err(), "{name} must not become an Mp4");
    }
}

#[test]
fn the_seeking_probe_refuses_a_64_bit_box_size_that_wraps_its_cursor() {
    let dir = TempDir::new().unwrap();
    // No ffmpeg needed: this is about the walk's arithmetic, not about any codec. The extended
    // size form puts a whole `u64` from the file into the bounds test, and an unchecked `at + size`
    // wraps — which reads as "this box fits" and then walks the cursor backwards. Debug panicked
    // here before the checked sum landed; release was saved only by the caller's own `checked_add`,
    // which is not a reason to leave a bounds test that overflows.
    let path = dir.path().join("wrapping.mp4");
    let mut bytes = wrap(b"ftyp", b"isom");
    bytes.extend([1_u32.to_be_bytes().to_vec(), b"free".to_vec(), u64::MAX.to_be_bytes().to_vec()].concat());
    fs::write(&path, &bytes).unwrap();

    assert_eq!(header_time(&path), None, "a file this build cannot walk has no header time, and must not hang finding that out");
    assert!(Mp4::new(bytes).is_err());

    // The positive control, so a probe that refused every extended-size box would not pass: the
    // same form carrying an honest size still reads its movie header.
    let honest = dir.path().join("honest.mp4");
    let mut mvhd = vec![0, 0, 0, 0];
    mvhd.extend(3_693_562_205_u32.to_be_bytes());
    mvhd.extend(3_693_562_205_u32.to_be_bytes());
    mvhd.extend(1000_u32.to_be_bytes());
    mvhd.resize(100, 0);
    let moov = wrap(b"moov", &wrap(b"mvhd", &mvhd));
    let mut good = wrap(b"ftyp", b"isom\0\0\x02\0isommp42");
    good.extend([1_u32.to_be_bytes().to_vec(), b"free".to_vec(), 32_u64.to_be_bytes().to_vec(), vec![0; 16]].concat());
    good.extend(moov);
    fs::write(&honest, &good).unwrap();

    assert_eq!(
        header_time(&honest).map(|at| at.to_rfc3339()),
        Some("2021-01-15T13:30:05+00:00".to_owned()),
        "the probe must seek past a legal 64-bit-sized box, not stop at it"
    );
}

#[test]
fn a_real_encoders_output_is_accepted_and_its_header_time_survives_a_round_trip() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("a_real_encoders_output_is_accepted_and_its_header_time_survives_a_round_trip", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };

    let mut video = Mp4::read(&source).unwrap();
    // ffmpeg writes zeros into the header dates unless told a creation time, which is the same
    // "never written" state a memory video without one would be in.
    assert_eq!(video.embedded_time(), None, "an unwritten header reads as absent, not as 1904");

    let paris = FixedOffset::east_opt(3600).unwrap();
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 14, 30, 5), offset: Some(paris), location: Some(point(PARIS)) }).unwrap();
    let written = dir.path().join("out.mp4");
    video.write(&written).unwrap();

    // Read back through a fresh parse rather than through the value that wrote it.
    assert_eq!(Mp4::read(&written).unwrap().embedded_time().map(|at| at.to_rfc3339()), Some("2021-01-15T13:30:05+00:00".to_owned()));
    // And through the seeking probe, which is a different code path over the same bytes.
    assert_eq!(header_time(&written).map(|at| at.to_rfc3339()), Some("2021-01-15T13:30:05+00:00".to_owned()));
}

#[test]
fn the_seeking_probe_finds_the_movie_box_on_either_side_of_the_media_data() {
    let dir = TempDir::new().unwrap();
    let Some(plain) = hevc(dir.path(), "mdat-first.mp4") else {
        skipped("the_seeking_probe_finds_the_movie_box_on_either_side_of_the_media_data", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let faststarted = faststart(dir.path(), "moov-first.mp4").unwrap();

    // The two layouts differ, or the test proves nothing about seeking past `mdat`.
    let order = |path: &Path| {
        let bytes = fs::read(path).unwrap();
        boxes(&bytes, 0, bytes.len())
            .into_iter()
            .map(|(_, _, kind)| kind)
            .filter(|kind| kind == b"moov" || kind == b"mdat")
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&plain), [*b"mdat", *b"moov"], "the fixture must put the media data first");
    assert_eq!(order(&faststarted), [*b"moov", *b"mdat"]);

    for source in [&plain, &faststarted] {
        let mut video = Mp4::read(source).unwrap();
        video.stamp(&VideoStamp { local: at(2021, 1, 15, 13, 30, 5), offset: None, location: None }).unwrap();
        let written = dir.path().join("probed.mp4");
        video.write(&written).unwrap();
        assert_eq!(header_time(&written).map(|at| at.to_rfc3339()), Some("2021-01-15T13:30:05+00:00".to_owned()), "{}", source.display());
    }
}

#[test]
fn a_write_that_shrinks_leaves_no_tail_of_whatever_was_at_the_output_path() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("a_write_that_shrinks_leaves_no_tail_of_whatever_was_at_the_output_path", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let written = dir.path().join("out.mp4");

    const MARKER: &[u8] = b"PREVIOUS-PAYLOAD-THAT-MUST-NOT-SURVIVE";
    let stale: Vec<u8> = MARKER.iter().copied().cycle().take(512 * 1024).collect();
    fs::write(&written, &stale).unwrap();

    let mut video = Mp4::read(&source).unwrap();
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 13, 30, 5), offset: None, location: None }).unwrap();
    video.write(&written).unwrap();

    let out = fs::read(&written).unwrap();
    assert!(out.len() < stale.len(), "the fixture only tests truncation if the new write is the smaller one");
    assert!(!out.windows(MARKER.len()).any(|window| window == MARKER), "the previous payload is still readable on disk");
}

// ---- the media data is not touched ----

#[test]
fn the_media_data_comes_through_a_stamp_byte_for_byte() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("the_media_data_comes_through_a_stamp_byte_for_byte", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let before = fs::read(&source).unwrap();

    let mut video = Mp4::read(&source).unwrap();
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 13, 30, 5), offset: None, location: Some(point(RIO)) }).unwrap();

    // The whole point of a fixed-size header patch plus a splicing tag write: not one frame moves.
    // A chapter leg turned on by a `WriteConfig::DEFAULT` would rewrite exactly this box.
    assert_eq!(box_bytes(video.as_bytes(), b"mdat"), box_bytes(&before, b"mdat"), "the media data changed");
    // The file-type box sits in front of everything and must be untouched too.
    assert_eq!(box_bytes(video.as_bytes(), b"ftyp"), box_bytes(&before, b"ftyp"));
    // The video still decodes, which is the property a corrupted sample table would break and a
    // byte comparison alone would not catch.
    let written = dir.path().join("out.mp4");
    video.write(&written).unwrap();
    let decoded = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&written)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    assert!(decoded.stderr.is_empty(), "ffmpeg complained about the stamped file: {}", String::from_utf8_lossy(&decoded.stderr));
}

// ---- the udta shadow rule ----

#[test]
fn the_sentinel_real_memory_videos_carry_does_not_block_the_coordinate() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("the_sentinel_real_memory_videos_carry_does_not_block_the_coordinate", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    // The real export's shape: a `udta/©eng` holding an invalid-latitude sentinel, and no `meta`
    // at all, so the tag write has to create that structure rather than splice into one.
    let sentinel = b"-180.00-180.000/";
    let mut payload = u16::try_from(sentinel.len()).unwrap().to_be_bytes().to_vec();
    payload.extend(0x55c4_u16.to_be_bytes());
    payload.extend(sentinel);
    let shaped = dir.path().join("with-eng.mp4");
    with_udta_child(&source, &shaped, b"\xa9eng", &payload);

    let mut video = Mp4::read(&shaped).unwrap();
    assert_eq!(video.location_atom(), None, "an arbitrary udta child is not a location atom");
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 14, 30, 5), offset: None, location: Some(point(PARIS)) }).unwrap();
    let written = dir.path().join("out.mp4");
    video.write(&written).unwrap();

    // The sentinel survives verbatim: the tag write splices rather than re-serialising `udta`.
    let out = fs::read(&written).unwrap();
    let carried = [b"\xa9eng".to_vec(), payload.clone()].concat();
    assert!(out.windows(carried.len()).any(|window| window == carried), "the existing udta child did not survive the write");

    let Some(tags) = exiftool(&written) else {
        skipped("the_sentinel_real_memory_videos_carry_does_not_block_the_coordinate", "exiftool", REQUIRE_EXIFTOOL);
        return;
    };
    // The composite resolves to OUR coordinate, which is the whole verdict: the sentinel loses.
    assert!(tags.get("GPSPosition").is_some_and(|value| value.starts_with("48 deg 51")), "{tags:#?}");
}

#[test]
fn a_video_already_carrying_a_location_atom_gets_no_shadowed_duplicate() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("a_video_already_carrying_a_location_atom_gets_no_shadowed_duplicate", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    // ffmpeg's own spelling: version+flags, language, an empty name, role, then longitude,
    // latitude and altitude as 16.16 fixed point.
    let mut loci = vec![0, 0, 0, 0, 0x15, 0xc7, 0, 0];
    for degrees in [2.294_351_f64, 48.858_844, 0.0] {
        loci.extend((f64::from(1 << 16) * degrees).round().to_string().parse::<i32>().unwrap().to_be_bytes());
    }
    loci.push(0);
    let shaped = dir.path().join("with-loci.mp4");
    with_udta_child(&source, &shaped, b"loci", &loci);

    let mut video = Mp4::read(&shaped).unwrap();
    assert_eq!(video.location_atom(), Some(LocationAtom::Loci));

    // Rio, which is nowhere near the Paris the `loci` names, so a written duplicate is visible in
    // any reader rather than hidden behind rounding.
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 14, 30, 5), offset: None, location: Some(point(RIO)) }).unwrap();
    let written = dir.path().join("out.mp4");
    video.write(&written).unwrap();

    // No coordinate went in at all. Not "it lost the precedence fight": it was never written, so
    // the file carries one location rather than two that disagree.
    let out = fs::read(&written).unwrap();
    assert!(!out.windows(4).any(|window| window == b"\xa9xyz"), "a shadowed duplicate coordinate was written anyway");
    // The date still landed, so the skip is the coordinate alone and not the whole stamp.
    assert_eq!(Mp4::read(&written).unwrap().embedded_time().map(|at| at.to_rfc3339()), Some("2021-01-15T14:30:05+00:00".to_owned()));

    let Some(tags) = exiftool(&written) else {
        skipped("a_video_already_carrying_a_location_atom_gets_no_shadowed_duplicate", "exiftool", REQUIRE_EXIFTOOL);
        return;
    };
    // Still resolves to the atom that was already there, and to nothing of ours.
    assert!(tags.get("GPSPosition").is_some_and(|value| value.starts_with("48 deg 51")), "{tags:#?}");
    assert!(!tags.contains_key("GPSCoordinates"), "{tags:#?}");
}

// ---- read back through independent readers ----

#[test]
fn every_header_date_and_both_tags_read_back_correctly_through_two_independent_readers() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("every_header_date_and_both_tags_read_back_correctly_through_two_independent_readers", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };

    let mut video = Mp4::read(&source).unwrap();
    let paris = FixedOffset::east_opt(3600).unwrap();
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 14, 30, 5), offset: Some(paris), location: Some(point(PARIS)) }).unwrap();
    let written = dir.path().join("out.mp4");
    video.write(&written).unwrap();

    let Some(tags) = exiftool(&written) else {
        skipped("every_header_date_and_both_tags_read_back_correctly_through_two_independent_readers", "exiftool", REQUIRE_EXIFTOOL);
        return;
    };

    // The header fields are UTC by definition and exiftool renders them verbatim, so the wall time
    // it prints is the instant, an hour behind the Paris local time that went in.
    for field in ["CreateDate", "ModifyDate", "TrackCreateDate", "TrackModifyDate", "MediaCreateDate", "MediaModifyDate"] {
        assert_eq!(tags.get(field).map(String::as_str), Some("2021:01:15 13:30:05"), "{field} in {tags:#?}");
    }
    // `©day` is where the local wall clock and its offset survive, which the header cannot hold.
    assert_eq!(tags.get("ContentCreateDate").map(String::as_str), Some("2021:01:15 14:30:05+01:00"));
    assert!(tags.get("GPSCoordinates").is_some_and(|value| value.starts_with("48 deg 51")), "{tags:#?}");
    assert!(tags.get("GPSPosition").is_some_and(|value| value.starts_with("48 deg 51")), "{tags:#?}");

    let probed = ffprobe(&written).unwrap();
    assert_eq!(probed.get("creation_time").map(String::as_str), Some("2021-01-15T13:30:05.000000Z"));
    assert_eq!(probed.get("date").map(String::as_str), Some("2021-01-15T14:30:05+01:00"));
    // ffprobe emits both `location` and `location-eng` on a `loci` file, so the bare key alone
    // would be a sibling match. Here there is no `loci`, and the value is the one that was written.
    assert_eq!(probed.get("location").map(String::as_str), Some("+48.858844+002.294351/"));
}

#[test]
fn a_southern_and_western_coordinate_reads_back_in_the_right_hemispheres() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("a_southern_and_western_coordinate_reads_back_in_the_right_hemispheres", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };

    let mut video = Mp4::read(&source).unwrap();
    let rio = FixedOffset::west_opt(3 * 3600).unwrap();
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 10, 30, 5), offset: Some(rio), location: Some(point(RIO)) }).unwrap();
    let written = dir.path().join("out.mp4");
    video.write(&written).unwrap();

    let Some(tags) = exiftool(&written) else {
        skipped("a_southern_and_western_coordinate_reads_back_in_the_right_hemispheres", "exiftool", REQUIRE_EXIFTOOL);
        return;
    };
    // A dropped sign is the whole error here, and it shows as a northern hemisphere reading.
    assert!(tags.get("GPSPosition").is_some_and(|value| value.contains('S') && value.contains('W')), "{tags:#?}");
    assert_eq!(tags.get("CreateDate").map(String::as_str), Some("2021:01:15 13:30:05"), "the instant, not the Rio wall time");
    assert_eq!(tags.get("ContentCreateDate").map(String::as_str), Some("2021:01:15 10:30:05-03:00"));
}

#[test]
fn an_unknown_offset_writes_no_zone_rather_than_claiming_utc() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("an_unknown_offset_writes_no_zone_rather_than_claiming_utc", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };

    let mut video = Mp4::read(&source).unwrap();
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 0, 0, 0), offset: None, location: None }).unwrap();
    let written = dir.path().join("out.mp4");
    video.write(&written).unwrap();

    let probed = ffprobe(&written).unwrap_or_default();
    let Some(date) = probed.get("date") else {
        skipped("an_unknown_offset_writes_no_zone_rather_than_claiming_utc", "ffprobe", REQUIRE_FFMPEG);
        return;
    };
    // A filename's midnight is in no stated zone at all, and `+00:00` there would upgrade
    // "unknown" to "UTC" for free — the same call the image leg makes about its offset tags.
    assert_eq!(date, "2021-01-15T00:00:00");
    assert!(!probed.contains_key("location"), "no coordinate was asked for, so none is written: {probed:#?}");
}

// ---- what a header cannot hold ----

#[test]
fn a_capture_before_1970_is_refused_and_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("a_capture_before_1970_is_refused_and_changes_nothing", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };
    let before = fs::read(&source).unwrap();

    let mut video = Mp4::read(&source).unwrap();
    // 1965 is a legal raw value against MP4's 1904 epoch, and both readers would show it as a date
    // in the 2030s because they treat anything under the boundary as unix seconds.
    let refused = video.stamp(&VideoStamp { local: at(1965, 6, 1, 12, 0, 0), offset: None, location: None }).unwrap_err();
    assert!(refused.to_string().contains("before 1970"), "{refused}");
    assert_eq!(video.as_bytes(), &before[..], "a refused stamp leaves the buffer exactly as it was");
    assert_eq!(fs::read(&source).unwrap(), before, "and the source is never written to at all");
}

#[test]
fn a_video_whose_movie_header_gives_it_no_timescale_is_refused_rather_than_crashing_the_run() {
    let dir = TempDir::new().unwrap();
    let Some(source) = hevc(dir.path(), "memory.mp4") else {
        skipped("a_video_whose_movie_header_gives_it_no_timescale_is_refused_rather_than_crashing_the_run", "ffmpeg", REQUIRE_FFMPEG);
        return;
    };

    // `mp4ameta 0.13.0` computes `duration / timescale` on every read before it looks at a single
    // config flag, so a zero here PANICS the process rather than failing one row. Reproduced on a
    // real encoder's output, not only on a hand-built stub, because the guard has to hold for the
    // files an export actually contains.
    let mut bytes = fs::read(&source).unwrap();
    let (moov, moov_size, _) = boxes(&bytes, 0, bytes.len()).into_iter().find(|(_, _, kind)| kind == b"moov").unwrap();
    let (mvhd, _, _) = boxes(&bytes, moov + 8, moov + moov_size).into_iter().find(|(_, _, kind)| kind == b"mvhd").unwrap();
    // Version 0: version and flags, both times, then the timescale.
    assert_eq!(bytes[mvhd + 8], 0, "the fixture's movie header is version 0");
    bytes[mvhd + 20..mvhd + 24].copy_from_slice(&0_u32.to_be_bytes());
    let zeroed = dir.path().join("zero-timescale.mp4");
    fs::write(&zeroed, &bytes).unwrap();

    let refused = Mp4::read(&zeroed).unwrap_err();
    assert!(refused.to_string().contains("timescale of zero"), "{refused}");
    // The positive control: the same file with its real timescale is accepted, so the rejection is
    // about the value rather than about the surgery.
    assert!(Mp4::read(&source).is_ok());
    assert_eq!(Mp4::new(fs::read(&zeroed).unwrap()).unwrap_err(), NotMp4::ZeroTimescale);
}

// ---- co64 chunk tables and fragmented input (synthetic bytes, no external tools) ----
//
// The task-13 spike verified the tag splice only against `stco` files built by ffmpeg. A `co64`
// table — 64-bit chunk offsets, which a >4 GiB file is required to carry — fixes up through the
// same mp4ameta leg (`UpdateChunkOffset` with `ChunkOffsets::Co64`), and a fragmented input puts
// `mvex`/`moof`/`mfra` boxes the walker must step over. Neither shape has a fixture until here, so
// neither write has ever been observed. All three tests are byte-built and run without ffmpeg.

#[test]
fn a_co64_chunk_table_is_rewritten_behind_the_tag_splice() {
    let before = with_co64(4);
    let mut video = Mp4::new(before.clone()).unwrap();
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 13, 30, 5), offset: None, location: None }).unwrap();
    let out = video.as_bytes();

    // The fixed-size header patch still landed on a file carrying a 64-bit table.
    assert_eq!(Mp4::new(out.to_vec()).unwrap().embedded_time().map(|at| at.to_rfc3339()), Some("2021-01-15T13:30:05+00:00".to_owned()));
    // The media data never moves: the table is rewritten, not the data.
    assert_eq!(box_bytes(out, b"mdat"), box_bytes(&before, b"mdat"));

    // The tag splice grows moov — the whole reason a chunk table needs rewriting at all. If the
    // splice never ran, this assertion fails first.
    let growth = moov_size(out) - moov_size(&before);
    assert!(growth > 0, "the stamp must grow moov by the spliced tag");

    // Every entry of the 64-bit table shifts by exactly that growth: the entries point past the
    // splice, and the splice is the only size-changing change in the file.
    let co64_entries = |bytes: &[u8]| {
        let (moov_at, moov_size) =
            boxes(bytes, 0, bytes.len()).into_iter().find(|(_, _, kind)| kind == b"moov").map(|(at, size, _)| (at, size)).unwrap();
        let (trak_at, trak_size) = child(bytes, moov_at, moov_size, b"trak");
        let (mdia_at, mdia_size) = child(bytes, trak_at, trak_size, b"mdia");
        let (minf_at, minf_size) = child(bytes, mdia_at, mdia_size, b"minf");
        let (stbl_at, stbl_size) = child(bytes, minf_at, minf_size, b"stbl");
        let (co64_at, co64_size) = child(bytes, stbl_at, stbl_size, b"co64");
        let body = &bytes[co64_at + 8..co64_at + co64_size];
        let count = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
        (0..count).map(|i| u64::from_be_bytes(body[8 + 8 * i..16 + 8 * i].try_into().unwrap())).collect::<Vec<_>>()
    };
    let (in_offsets, out_offsets) = (co64_entries(&before), co64_entries(out));
    assert_eq!(in_offsets.len(), 4, "the fixture's table must carry the entries the test counts");
    let growth = growth as u64;
    for (i, (in_offset, out_offset)) in in_offsets.iter().zip(&out_offsets).enumerate() {
        assert_eq!(*out_offset, *in_offset + growth, "entry {i}: offset {in_offset} was not shifted by the moov growth");
    }

    // The shifted pointers land on the same markers they named before: a wrong shift that still
    // passed the arithmetic above would land on the wrong marker, and this fails.
    for (i, offset) in out_offsets.iter().enumerate() {
        let chunk = [0x80 + i as u8, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11];
        assert_eq!(&out[*offset as usize..*offset as usize + 8], &chunk[..], "chunk {i} is no longer where the table says");
    }
}

#[test]
fn a_fragmented_file_stamps_cleanly_and_everything_before_moov_stays_put() {
    let before = fragmented();
    // The fixture must put every box the splice could move before moov, or the byte-identity
    // assertions below would be about the layout rather than about the splice.
    let order: Vec<[u8; 4]> = boxes(&before, 0, before.len()).into_iter().map(|(_, _, kind)| kind).collect();
    assert_eq!(order, [*b"ftyp", *b"moof", *b"mdat", *b"mfra", *b"moov"], "the fixture's top-level order");

    let mut video = Mp4::new(before.clone()).unwrap();
    video.stamp(&VideoStamp { local: at(2021, 1, 15, 13, 30, 5), offset: None, location: None }).unwrap();
    let out = video.as_bytes();

    // The header patch landed, so the stamp ran at all on a fragmented file.
    assert_eq!(Mp4::new(out.to_vec()).unwrap().embedded_time().map(|at| at.to_rfc3339()), Some("2021-01-15T13:30:05+00:00".to_owned()));

    // The splice grew moov, and nothing that precedes it moved a byte: the trun offsets are
    // relative to the moof, and the tfra's absolute moof pointer stays true.
    let growth = moov_size(out) - moov_size(&before);
    assert!(growth > 0, "the stamp must grow moov by the spliced tag");
    for kind in [b"moof", b"mdat", b"mfra"] {
        assert_eq!(box_bytes(out, kind), box_bytes(&before, kind), "the {:?} box moved", kind);
    }

    // The mvex box — the thing that makes this a fragmented file — survived the splice verbatim.
    let mvex = |bytes: &[u8]| {
        let (moov_at, moov_size, _) = boxes(bytes, 0, bytes.len()).into_iter().find(|(_, _, kind)| kind == b"moov").unwrap();
        let (at, size) = child(bytes, moov_at, moov_size, b"mvex");
        bytes[at..at + size].to_vec()
    };
    assert_eq!(mvex(out), mvex(&before));
}

#[test]
fn a_chunk_table_the_tagging_crate_refuses_errors_cleanly_and_changes_nothing() {
    // A co64 table claiming two entries in a box sized for one: `mp4ameta` checks the table size
    // against its entry count on the write path, and this crate's own walker never descends into
    // the sample table, so the refusal happens exactly at the tag write — the step whose failure
    // the all-or-nothing property cares about, and one none of the existing tests reaches (the
    // pre-1970 refusal fails before the patch mutates anything).
    let mut body = vec![0, 0, 0, 0];
    body.extend(2_u32.to_be_bytes());
    body.extend(0x1a_u64.to_be_bytes());
    let stbl = wrap(b"stbl", &[wrap(b"stsd", &[0; 8]), wrap(b"co64", &body)].concat());
    let mut before = wrap(b"ftyp", b"isom\0\0\x02\0isommp42");
    before.extend(wrap(b"moov", &[wrap(b"mvhd", &header_body(0, 100)), trak_with(&stbl)].concat()));
    before.extend(wrap(b"mdat", &[0; 16]));

    let mut video = Mp4::new(before.clone()).unwrap();
    let refused = video.stamp(&VideoStamp { local: at(2021, 1, 15, 13, 30, 5), offset: None, location: None }).unwrap_err();
    assert!(matches!(refused, VideoError::Tag { .. }), "{refused}");
    assert!(refused.to_string().contains("co64"), "{refused}");
    // The header patch runs first but against a copy, so the refusal leaves the buffer exactly as
    // it was — the tag-step half of the all-or-nothing property, on a file carrying a co64.
    assert_eq!(video.as_bytes(), &before[..]);
}
