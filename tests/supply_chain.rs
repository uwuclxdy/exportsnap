//! Tests for the advisory posture around `little_exif`, kept in a file named after it so whoever
//! revisits `deny.toml`'s RUSTSEC-2026-0194 ignore finds the code side of it by name.
//!
//! That ignore rests on a reachability argument: 0194 lives in `quick_xml`, `quick_xml` is imported
//! only by `little_exif`'s `xmp.rs`, and the single call reaching it sits on the PNG **write** path
//! (`png::write_metadata` -> `clear_metadata` -> `clear_exif_from_xmp_metadata` ->
//! `remove_exif_from_xmp`). This crate only ever asks for JPEG, so that path has no call site.
//!
//! # The property is structural, so almost nothing here has to carry it
//!
//! `src/export/exif.rs`'s private `library` module is the boundary: two functions that take **no
//! file type**, and `FileExtension` is not in scope anywhere else in the crate. There is no call
//! site that chooses a file type, so there is nothing to scan for and no conditional to hide a
//! variant behind. The compiler holds it — an aliased type at a conditional call site outside that
//! module is now `error[E0425]: cannot find type FileExtension in this scope`, not a test failure.
//!
//! The precise claim, because a looser one has been wrong twice: **no existing call site can be
//! made to pass a different file type.** Not "the type is unreachable" — a fully-qualified
//! `little_exif::filetype::FileExtension::PNG` needs no import and compiles fine. Bypassing the
//! boundary means adding a new direct call to the library, which is new code in a diff.
//!
//! That shape replaced a module-wide `const` plus a source scan, and the history is worth keeping
//! because it is the reason not to reach for a scan again: review beat the scan twice — an aliased
//! type at a call site dispatching on a fixture dimension held constant, then a `//` inside a
//! string literal hiding code from the comment stripper. The second break is what showed the
//! instrument was wrong rather than blunt. Enumerating inputs to prove a property about call sites
//! is the same trap as a fixture that holds constant the dimension its own assertion names.
//!
//! What remains here is a smoke test that the boundary works at all, plus the one thing the
//! compiler genuinely cannot see: that two separate config files agree.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use chrono::{FixedOffset, NaiveDate};
use exportsnap::export::exif::{Jpeg, Stamp};
use exportsnap::export::model::{Attribution, ConversationId, Field, LocationPoint, Username};
use image::{ImageFormat, RgbImage};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A small JPEG with enough detail that the encoder cannot collapse it.
fn encoded_jpeg() -> Vec<u8> {
    let mut pixels = RgbImage::new(8, 8);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = image::Rgb([(x * 31) as u8, (y * 29) as u8, 40]);
    }
    let mut encoded = Vec::new();
    pixels.write_to(&mut Cursor::new(&mut encoded), ImageFormat::Jpeg).unwrap();
    encoded
}

#[test]
fn a_jpeg_round_trips_through_the_only_route_this_crate_has_into_little_exif() {
    // **A round-trip REGRESSION test, not a security pin.** Do not cite it as one; `deny.toml` used
    // to and that was wrong. The aliasing class is dead by construction now — `FileExtension` is out
    // of scope outside `exif.rs`'s `library` module, so an aliased or conditional call site is a
    // compile error rather than something a fixture has to catch.
    //
    // What is left for this to catch is the narrow set of edits the compiler still permits: the
    // constant inside `library` switched, or `library::write` growing an unconditional PNG branch.
    // Both are one-line edits inside a twenty-line module, and both red here.
    //
    // **Its bound, stated so nobody grows it instead of reading this**: a CONDITIONAL inside
    // `library::write` — say keyed on `stamp.width` — beats a two-row fixture exactly the way
    // review's attack 1 beat the four-row one, and a five-row fixture would lose to a condition on
    // the sixth dimension. Enumerating inputs cannot prove a property about branches. If that
    // becomes a real worry the answer is fewer branches in `library`, not more rows here.
    //
    // The 8x8 is a real ceiling, not a detail: no test in this repo varies image dimension, so read
    // nothing here as covering large images. Carried in `docs/todo.md`, and inert only while
    // nothing in the pipeline branches on size.
    let paris = LocationPoint::parse(Field::Location, "Latitude, Longitude: 48.858844, 2.294351").unwrap();
    let local = NaiveDate::from_ymd_opt(2021, 1, 15).unwrap().and_hms_opt(14, 30, 5).unwrap();

    // The third and fourth rows execute the attribution branch, which the first two do not reach at
    // all. `embedded_time` is the discriminator and it needs no external tool: it reads back through
    // `little_exif`, so a write that burst the APP1 segment cannot answer it. That makes the last row
    // a real pin on the tag-length cap rather than a length assertion, which would pass just as well
    // with the Exif SubIFD destroyed.
    let attributed = Attribution { sender: Username::new("sender-handle"), conversation: Some(ConversationId::new("friend-handle")) };
    let oversized = Attribution { sender: Username::new("sender-handle"), conversation: Some(ConversationId::new("z".repeat(70_000))) };
    for (label, stamp) in [
        ("bare", Stamp { local, offset: None, location: None, width: 8, height: 8, attribution: None }),
        (
            "located and offset",
            Stamp { local, offset: FixedOffset::east_opt(3600), location: Some(paris), width: 8, height: 8, attribution: None },
        ),
        ("attributed", Stamp { local, offset: None, location: None, width: 8, height: 8, attribution: Some(&attributed) }),
        (
            "attribution past the segment ceiling",
            Stamp { local, offset: None, location: None, width: 8, height: 8, attribution: Some(&oversized) },
        ),
    ] {
        let mut jpeg = Jpeg::new(encoded_jpeg()).expect("the encoder's own output is a jpeg");
        jpeg.stamp(&stamp).unwrap_or_else(|error| panic!("{label}: stamping a jpeg failed ({error}); check `exif.rs`'s library module"));
        assert_eq!(jpeg.embedded_time(), Some(local), "{label}: the stamp did not survive a read back through the library");
        assert_eq!(&jpeg.as_bytes()[..2], &[0xff, 0xd8], "{label}: the container came back as something other than a jpeg");
    }
}

// ---- the two config files that have to agree ----

/// Every `RUSTSEC-…` id in a file's `[advisories]` table, read the way `~/repos/rs/cargo.sh` reads
/// `audit.toml`: section-scoped, `#` stripped per line, every id on a line taken.
///
/// **Stated ceiling: this REIMPLEMENTS the gate's awk, it does not share it.** `cargo.sh` lives
/// outside this repo, so it cannot be imported, depended on, or diffed against from here — and it
/// can therefore change without this test noticing. "Tracks the gate's awk" is a statement about
/// today, not a guarantee; if the scrape ever moves, this has to be moved by hand to match. The
/// upgrade path is the gate growing a `--print-ignores` mode this could shell out to, which is a
/// change to `cargo.sh` rather than to this repo.
///
/// Section scoping is the half that matters and was got wrong once: an earlier version took the
/// first `ignore` substring anywhere in the file, so ids under a table sorting before
/// `[advisories]` gave a green test while the gate scraped zero `--ignore` flags and reported the
/// advisory against the wrong file.
///
/// **Inherited imprecision, shared with the gate rather than introduced here**: neither this nor
/// the awk keys on the `ignore` KEY, only on the `[advisories]` table. So a RUSTSEC id sitting
/// under any other key in that table — a `#`-free note, a future `deny` list — counts as ignored by
/// both. The two agree, which is what this test is for, and they agree on something slightly wider
/// than "the ignore list". Fixing it means keying on the array, in `cargo.sh` first so the two do
/// not drift apart in the name of precision.
fn ignored_advisories(file: &str) -> BTreeSet<String> {
    let body = fs::read_to_string(repo().join(file)).unwrap();
    let mut inside = false;
    let mut ids = BTreeSet::new();
    for line in body.lines() {
        if line.trim_start().starts_with('[') {
            inside = line.contains("[advisories]");
            continue;
        }
        if !inside {
            continue;
        }
        ids.extend(rustsec_ids(line.split_once('#').map_or(line, |(before, _)| before)));
    }
    ids
}

/// Every `RUSTSEC-<digits>-<digits>` in `text`, which is the awk pattern the gate matches on.
fn rustsec_ids(text: &str) -> Vec<String> {
    text.match_indices("RUSTSEC-")
        .filter_map(|(index, _)| {
            let rest = &text[index + "RUSTSEC-".len()..];
            let year: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let sequence: String = rest[year.len()..].strip_prefix('-')?.chars().take_while(char::is_ascii_digit).collect();
            (!year.is_empty() && !sequence.is_empty()).then(|| format!("RUSTSEC-{year}-{sequence}"))
        })
        .collect()
}

#[test]
fn the_two_supply_chain_configs_ignore_exactly_the_same_advisories() {
    // `cargo deny` reads `deny.toml`; `cargo audit` reads only the GLOBAL `~/.cargo/audit.toml` and
    // gets this repo's ids because `cargo.sh` scrapes them out of `audit.toml` into `--ignore`
    // flags. Two files, one posture, and nothing but this test makes them agree: an id added to one
    // alone leaves the other tool red, and an id REMOVED from one alone silently narrows the
    // posture on that tool only, which is the direction nobody notices.
    let deny = ignored_advisories("deny.toml");
    let audit = ignored_advisories("audit.toml");

    assert!(!deny.is_empty(), "no ids parsed out of deny.toml's [advisories] table, so this test is comparing nothing");
    assert!(!audit.is_empty(), "no ids parsed out of audit.toml's [advisories] table, so the gate would scrape no --ignore flags");
    assert_eq!(deny, audit, "deny.toml and audit.toml disagree about which advisories are ignored");
    // Named explicitly, so removing the pair is a visible diff rather than a quiet narrowing.
    assert_eq!(deny, BTreeSet::from(["RUSTSEC-2026-0194".to_owned(), "RUSTSEC-2026-0195".to_owned()]));
}
