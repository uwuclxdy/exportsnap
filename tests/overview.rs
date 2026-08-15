//! Render tests for the overview screen's two read-only panels.
//!
//! Expectations are cross-checked against the cloudy-tui skill's Panel, Status pill, Empty state,
//! Forms → Static key:value rows and Patterns → Numeric formatting / Truncation sections, and
//! against `docs/design.md`'s TUI screen map, not against this crate.
//!
//! The export trees below are written by the tests themselves out of the smallest json each parser
//! accepts. `fixtures/` is gitignored so CI has none; the one test that reads it goes through
//! `common::fixtures`, which skips it on a box without the tree and fails it on a runner that set
//! `EXPORTSNAP_REQUIRE_FIXTURES`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use exportsnap::app::App;
use exportsnap::export::env::Environment;
use exportsnap::tui::screens::overview::Overview;
use exportsnap::tui::shell;
use exportsnap::tui::theme::{Palette, Tier};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

/// The crate-level allow is scoped here rather than inside `common`, and only on the crates that
/// need it: this one reads the fixture half and gates on no tool, so without it every tool-side
/// function warns. See `tests/common/mod.rs` for what that placement keeps measuring.
#[allow(dead_code, reason = "this crate reads the fixture tree and gates on no external tool")]
mod common;

/// A frame wide enough for both panels and tall enough for no compact banner, so the body starts
/// at row 1 and the two panels are 50 cells each.
const WIDE: u16 = 100;
const TALL: u16 = 20;
/// Row the panels' top border sits on: the header owns row 0.
const TOP_BORDER: u16 = 1;
/// First interior row of a panel.
const FIRST_ROW: u16 = TOP_BORDER + 1;
/// Interior columns of the left and right panel: one border plus one padding cell in from each
/// edge of a 50-cell panel.
const LEFT: Range<u16> = 2..48;
const RIGHT: Range<u16> = 52..98;
/// Cells the source path is truncated to.
const SOURCE_CELLS: usize = 18;

// ---- harness ----

fn draw(overview: Overview, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut app = App::new(Tier::Full).with_overview(overview);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| shell::render(frame, &mut app)).unwrap();
    terminal
}

fn row(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect()
}

/// One panel's interior on row `y`, trailing filler dropped.
fn cell_run(buffer: &Buffer, columns: Range<u16>, y: u16) -> String {
    columns.map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned()
}

/// Every interior row of one panel, from the first down to `count` rows.
fn panel_rows(buffer: &Buffer, columns: Range<u16>, count: u16) -> Vec<String> {
    (0..count).map(|offset| cell_run(buffer, columns.clone(), FIRST_ROW + offset)).collect()
}

/// A scratch dir under cargo's own test tmpdir, emptied first so a rerun starts clean.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The head-ellipsis form of an ascii path, worked out the long way so the expectation is not
/// taken from the code under test.
fn shortened(path: &Path) -> String {
    let text = path.display().to_string();
    assert!(text.is_ascii(), "the scratch path has to be ascii for this arithmetic: {text:?}");
    if text.len() <= SOURCE_CELLS {
        return text;
    }
    format!("…{}", &text[text.len() - (SOURCE_CELLS - 1)..])
}

/// The smallest json each parser accepts: three memories spanning 2019-2021 (one undated), three
/// chat records across two conversations, one snap, four friends and one blocked user. Every
/// struct in the schema layer carries `#[serde(default)]`, so an empty record object is valid.
fn write_json(json_dir: &Path) {
    fs::create_dir_all(json_dir).unwrap();
    for (file, body) in [
        ("memories_history.json", r#"{"Saved Media":[{"Date":"2019-05-04 10:00:00 UTC"},{"Date":"2021-07-09 11:00:00 UTC"},{}]}"#),
        ("chat_history.json", r#"{"conv_a":[{},{}],"conv_b":[{}]}"#),
        ("snap_history.json", r#"{"conv_a":[{}]}"#),
        ("friends.json", r#"{"Friends":[{},{},{},{}],"Blocked Users":[{}]}"#),
    ] {
        fs::write(json_dir.join(file), body).unwrap();
    }
}

/// A `json/` holding exactly the record counts asked for, so a test can make the three bare counts
/// differ in digit width — every count in [`write_json`] is a single digit, where the numeric
/// column's padding is a no-op.
fn write_json_counts(json_dir: &Path, chats: usize, snaps: usize, friends: usize) {
    fs::create_dir_all(json_dir).unwrap();
    let records = |count: usize| vec!["{}"; count].join(",");

    fs::write(json_dir.join("chat_history.json"), format!("{{\"conv_a\":[{}]}}", records(chats))).unwrap();
    fs::write(json_dir.join("snap_history.json"), format!("{{\"conv_a\":[{}]}}", records(snaps))).unwrap();
    fs::write(json_dir.join("friends.json"), format!("{{\"Friends\":[{}]}}", records(friends))).unwrap();
    fs::write(json_dir.join("memories_history.json"), br#"{"Saved Media":[{"Date":"2019-05-04 10:00:00 UTC"}]}"#).unwrap();
}

/// One delivery: part 1 unpacked with its `json/`, parts 2 and 3 still zipped. `discover_parts`
/// only reads names, so the zips can be empty files.
fn export_tree(name: &str) -> PathBuf {
    let dir = scratch(name);
    write_json(&dir.join("mydata~t1").join("json"));
    fs::write(dir.join("mydata~t1-2.zip"), b"").unwrap();
    fs::write(dir.join("mydata~t1-3.zip"), b"").unwrap();
    dir
}

/// ffmpeg on `PATH`, vlc not, 2 GiB free — none of it read off the real machine.
fn environment() -> Environment {
    Environment {
        ffmpeg: Some(PathBuf::from("/usr/bin/ffmpeg")),
        vlc: None,
        available_space: Some(2 * 1024 * 1024 * 1024),
        total_space: Some(4 * 1024 * 1024 * 1024),
    }
}

// ---- both panels against a known export ----

#[test]
fn both_panels_report_the_export_they_were_pointed_at() {
    let dir = export_tree("overview-loaded");
    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    assert_eq!(
        row(buffer, TOP_BORDER),
        "╭─ EXPORT SUMMARY ───────────────────────────────╮╭─ ENVIRONMENT ──────────────────────────────────╮"
    );

    // Labels pad to the widest in their own panel plus a 2-cell gap, so the values stack in one
    // column: `memories` (8) then `disk free` (9).
    assert_eq!(
        panel_rows(buffer, LEFT, 5),
        ["parts     2 zips · 1 unpacked", "memories  3 · 2019-2021", "chats     3", "snaps     1", "friends   4",]
    );

    assert_eq!(
        panel_rows(buffer, RIGHT, 4),
        ["ffmpeg     [ present ]", "vlc        [ missing ]", "disk free  2.0 GiB", format!("source     {}", shortened(&dir)).as_str(),]
    );
}

#[test]
fn a_complete_delivery_carries_no_missing_row() {
    // Parts 1, 2 and 3 are all accounted for, so the row that only exists to report a gap is
    // absent — an element with no status shows nothing.
    let dir = export_tree("overview-complete");
    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);

    assert_eq!(cell_run(terminal.backend().buffer(), LEFT, FIRST_ROW + 1), "memories  3 · 2019-2021");
}

#[test]
fn the_parts_row_counts_what_discover_parts_found() {
    // Parts 1 and 4 are there, 2 and 3 are not: `PartGroup::missing_parts` sees the gap below the
    // highest number seen, and the summary reports it on its own row.
    let dir = scratch("overview-gappy");
    write_json(&dir.join("mydata~t9").join("json"));
    fs::write(dir.join("mydata~t9-4.zip"), b"").unwrap();
    // Neither of these is a part of this delivery, and neither may be counted.
    fs::write(dir.join("holiday-photos.zip"), b"").unwrap();
    fs::create_dir_all(dir.join("memories")).unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    assert_eq!(cell_run(buffer, LEFT, FIRST_ROW), "parts     1 zip · 1 unpacked");
    assert_eq!(cell_run(buffer, LEFT, FIRST_ROW + 1), "missing   2 parts");
    assert_eq!(cell_run(buffer, LEFT, FIRST_ROW + 2), "memories  3 · 2019-2021");
}

#[test]
fn a_single_missing_part_reads_as_one_part() {
    // Parts 1 and 3 present, so exactly one is missing. Every other test that shows this row has
    // two, which leaves the singular unrendered.
    let dir = scratch("overview-one-missing");
    write_json(&dir.join("mydata~t14").join("json"));
    fs::write(dir.join("mydata~t14-3.zip"), b"").unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);

    assert_eq!(cell_run(terminal.backend().buffer(), LEFT, FIRST_ROW + 1), "missing   1 part");
}

#[test]
fn the_missing_count_is_danger_and_the_rest_of_the_row_is_not() {
    let dir = scratch("overview-missing-style");
    write_json(&dir.join("mydata~t9").join("json"));
    fs::write(dir.join("mydata~t9-4.zip"), b"").unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // `missing` is at column 2, its value at column 2 + 10.
    let label = buffer[(2, FIRST_ROW + 1)].style();
    assert_eq!(buffer[(2, FIRST_ROW + 1)].symbol(), "m");
    assert_eq!(label.fg, Some(palette.text_dim), "a static key stays dim");
    assert!(label.add_modifier.contains(Modifier::BOLD), "a static key is always bold");

    let value = buffer[(12, FIRST_ROW + 1)].style();
    assert_eq!(buffer[(12, FIRST_ROW + 1)].symbol(), "2");
    assert_eq!(value.fg, Some(palette.danger), "an incomplete delivery is a charged state");
}

// ---- the json half, and its two absences ----

#[test]
fn the_json_rows_are_placeholders_until_a_part_is_unpacked() {
    // Zips only, nothing extracted: there is no `json/` to read, so the four counts have no value
    // rather than a zero that would read as a real answer.
    let dir = scratch("overview-not-unpacked");
    fs::write(dir.join("mydata~t2.zip"), b"").unwrap();
    fs::write(dir.join("mydata~t2-2.zip"), b"").unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);

    assert_eq!(
        panel_rows(terminal.backend().buffer(), LEFT, 5),
        ["parts     2 zips", "memories  —", "chats     —", "snaps     —", "friends   —",]
    );
}

#[test]
fn the_bare_counts_are_left_padded_so_their_right_edges_line_up() {
    // Patterns → Numeric column alignment: pad each value on the left to the column's widest width
    // so the right edges line up, with the column still anchored at its label. Not right-justified
    // against the panel edge, and not decimal-aligned.
    let dir = scratch("overview-ragged");
    write_json_counts(&dir.join("mydata~t10").join("json"), 123, 45, 6);

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let rows = panel_rows(terminal.backend().buffer(), LEFT, 5);

    assert_eq!(rows, ["parts     1 unpacked", "memories  1 · 2019", "chats     123", "snaps      45", "friends     6"]);

    // Spelled out as the property too, so the literals above cannot drift apart from the rule.
    for line in &rows[2..] {
        assert_eq!(line.chars().count(), 13, "{line:?}");
    }
}

#[test]
fn the_memories_row_is_not_part_of_the_numeric_column() {
    // Its value is a count plus a year clause, so it is a composite row: padding it would shove the
    // whole clause rightwards. It stays flush at the label column whatever the counts beside it do.
    let dir = scratch("overview-memories-flush");
    let json = dir.join("mydata~t11").join("json");
    write_json_counts(&json, 1234, 5, 6);
    // Undated, so the row's value is a bare `1` — NARROWER than the 5-cell count column. With a
    // year clause the value is wider than the column and padding it would be a no-op, which is
    // exactly the fixture that cannot see this rule at all.
    fs::write(json.join("memories_history.json"), br#"{"Saved Media":[{}]}"#).unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let rows = panel_rows(terminal.backend().buffer(), LEFT, 5);

    assert_eq!(rows[1], "memories  1", "a composite row stays flush at the label column");
    assert_eq!(rows[2], "chats     1,234");
    assert_eq!(rows[3], "snaps         5");
    assert_eq!(rows[4], "friends       6");
}

// ---- absent section vs present-and-empty (a count of 0 must mean something) ----

#[test]
fn a_section_absent_from_the_json_reads_as_no_value_not_as_zero() {
    // `ExportJson` holds every file it models optionally, so a `json/` that arrived without
    // `chat_history.json` must not claim zero chats — that is a confident wrong answer where the
    // truth is "that section is not here". No extraction driver exists yet, so every `json/` on
    // disk got there by a manual or interrupted unzip and this is reachable today.
    let dir = scratch("overview-partial-json");
    let json = dir.join("mydata~t12").join("json");
    fs::create_dir_all(&json).unwrap();
    fs::write(json.join("memories_history.json"), br#"{"Saved Media":[{"Date":"2019-05-04 10:00:00 UTC"},{}]}"#).unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);

    assert_eq!(
        panel_rows(terminal.backend().buffer(), LEFT, 5),
        ["parts     1 unpacked", "memories  2 · 2019", "chats     —", "snaps     —", "friends   —",]
    );
}

#[test]
fn a_section_that_is_present_and_empty_reads_as_zero() {
    // The other half of the distinction: `0` is reserved for a section that IS there and holds
    // nothing, which is a real answer and not an absence.
    let dir = scratch("overview-empty-sections");
    write_json_counts(&dir.join("mydata~t13").join("json"), 0, 0, 0);

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);

    assert_eq!(
        panel_rows(terminal.backend().buffer(), LEFT, 5),
        ["parts     1 unpacked", "memories  1 · 2019", "chats     0", "snaps     0", "friends   0",]
    );
}

#[test]
fn the_json_search_walks_every_unpacked_part_not_just_the_first() {
    // Part 1 is unpacked without a `json/` and part 2 is unpacked with one. Only the first part
    // carried `json/` in the one export observed (n=1), so which part holds it is a shape hint and
    // not a contract — a search that stopped at the first unpacked part would report no counts here.
    let dir = scratch("overview-json-on-part-two");
    fs::create_dir_all(dir.join("mydata~t8").join("chat_media")).unwrap();
    write_json(&dir.join("mydata~t8-2").join("json"));

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);

    assert_eq!(
        panel_rows(terminal.backend().buffer(), LEFT, 5),
        ["parts     2 unpacked", "memories  3 · 2019-2021", "chats     3", "snaps     1", "friends   4"]
    );
}

#[test]
fn the_json_rows_say_unreadable_when_the_export_json_will_not_load() {
    // An unpacked part whose json is there and broken is a different thing from one that was never
    // unpacked, and the word is the whole report: a `LoadError` can carry the offending value, and
    // `Field::Location` makes that a coordinate pair.
    let dir = scratch("overview-broken-json");
    let json = dir.join("mydata~t3").join("json");
    write_json(&json);
    fs::write(json.join("friends.json"), b"{ not json").unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    assert_eq!(
        panel_rows(buffer, LEFT, 5),
        ["parts     1 unpacked", "memories  unreadable", "chats     unreadable", "snaps     unreadable", "friends   unreadable",]
    );

    let value = buffer[(12, FIRST_ROW + 1)].style();
    assert_eq!(buffer[(12, FIRST_ROW + 1)].symbol(), "u");
    assert_eq!(value.fg, Some(Palette::new(Tier::Full).danger));
}

#[test]
fn a_broken_export_never_spells_out_why_on_screen() {
    // A coordinate that fails validation is the load error the privacy gate is about: it reaches
    // `LoadError::Display` as `Location: expected ... got "Latitude, Longitude: 91.5, 8.2"`.
    // Nothing of it may appear anywhere on the frame.
    let dir = scratch("overview-leaky-json");
    let json = dir.join("mydata~t4").join("json");
    write_json(&json);
    fs::write(json.join("memories_history.json"), br#"{"Saved Media":[{"Location":"Latitude, Longitude: 91.5, 8.25"}]}"#).unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();
    let frame: String = (0..TALL).map(|y| row(buffer, y)).collect();

    assert!(frame.contains("unreadable"), "the screen still says the json did not load");
    for leak in ["91.5", "8.25", "Latitude", "Longitude", "memories_history"] {
        assert!(!frame.contains(leak), "{leak:?} reached the screen: {frame:?}");
    }
}

// ---- the empty states ----

#[test]
fn a_source_dir_with_no_delivery_says_so_in_a_framed_empty_state() {
    let dir = scratch("overview-empty");
    fs::write(dir.join("holiday-photos.zip"), b"").unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    // 16 interior rows, a 4-row frame: 6 above and 6 below.
    assert_eq!(
        (0..4).map(|offset| cell_run(buffer, LEFT, FIRST_ROW + 6 + offset).trim().to_owned()).collect::<Vec<_>>(),
        ["╭─────────────────────────╮", "│   no export found       │", "│   pass --source=<dir>   │", "╰─────────────────────────╯",]
    );
    // The frame carries no action line with a hotkey in it, because no key is bound yet.
    assert_eq!(cell_run(buffer, LEFT, FIRST_ROW), "");
}

#[test]
fn the_empty_state_frame_sits_centred_in_its_panel() {
    // Its 27 cells inside a 46-cell interior leave 19 to split, so the two pads differ by one —
    // which side takes the extra is ratatui's remainder rule, not a choice this screen makes.
    // Vertically it centers in the full-height panel: 16 interior rows, a 4-row frame, 6 above
    // and 6 below.
    let terminal = draw(Overview::unloaded(), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    let framed = cell_run(buffer, LEFT, FIRST_ROW + 6);
    let left_pad = framed.chars().take_while(|c| *c == ' ').count();
    let right_pad = LEFT.len() - framed.chars().count();

    assert_eq!(framed.trim_start().chars().count(), 27, "frame width");
    assert!(left_pad.abs_diff(right_pad) <= 1, "pads {left_pad} and {right_pad} are not a centred split");
}

#[test]
fn a_source_dir_that_is_not_there_says_not_found_rather_than_unreadable() {
    // A typo in `--source` is the likeliest failure of the lot. Diagnosing it as "unreadable" would
    // point at permissions, and an action line reading `pass --source=<dir>` would answer with the
    // step the user just took.
    let dir = scratch("overview-not-found").join("gone").join("deeper");
    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    assert_eq!(
        (0..4).map(|offset| cell_run(buffer, LEFT, FIRST_ROW + 6 + offset).trim().to_owned()).collect::<Vec<_>>(),
        ["╭──────────────────────────╮", "│   source dir not found   │", "│   check --source=<dir>   │", "╰──────────────────────────╯",]
    );
}

/// Unix-only: making a dir genuinely unlistable while it exists needs a mode change, and running as
/// root defeats it. The precondition is checked with `read_dir` directly rather than through the
/// code under test, so a root run skips loudly instead of passing on the wrong branch.
#[cfg(unix)]
#[test]
fn a_source_dir_that_exists_and_cannot_be_listed_still_says_unreadable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("overview-unreadable");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();

    if fs::read_dir(&dir).is_ok() {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        println!("skipping: this user can list a 0o000 dir, so the unreadable branch is unreachable here");
        return;
    }

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let rendered: Vec<String> =
        (0..4).map(|offset| cell_run(terminal.backend().buffer(), LEFT, FIRST_ROW + 6 + offset).trim().to_owned()).collect();

    // Restore before asserting, so a failure does not leave an unlistable dir behind for the rerun.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(
        rendered,
        [
            "╭───────────────────────────╮",
            "│   source dir unreadable   │",
            "│   pass --source=<dir>     │",
            "╰───────────────────────────╯",
        ]
    );
}

#[test]
fn several_deliveries_are_counted_rather_than_guessed_between() {
    let dir = scratch("overview-several");
    write_json(&dir.join("mydata~t5").join("json"));
    write_json(&dir.join("mydata~t6").join("json"));
    fs::write(dir.join("mydata~t7.zip"), b"").unwrap();

    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    assert_eq!(
        (0..4).map(|offset| cell_run(buffer, LEFT, FIRST_ROW + 6 + offset).trim().to_owned()).collect::<Vec<_>>(),
        [
            "╭───────────────────────────╮",
            "│   3 exports found here    │",
            "│   point --source at one   │",
            "╰───────────────────────────╯",
        ]
    );
}

#[test]
fn an_app_that_never_loaded_anything_draws_the_no_export_state() {
    // `App::new` alone, which is what every shell render test draws.
    let terminal = draw(Overview::unloaded(), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    assert_eq!(cell_run(buffer, LEFT, FIRST_ROW + 7).trim(), "│   no export found       │");
    assert_eq!(panel_rows(buffer, RIGHT, 4), ["ffmpeg     [ missing ]", "vlc        [ missing ]", "disk free  unknown", "source     —",]);
}

// ---- the environment half ----

#[test]
fn a_tool_on_path_and_one_off_it_render_different_pills() {
    let dir = export_tree("overview-tools");
    let both = Environment { ffmpeg: Some(PathBuf::from("/usr/bin/ffmpeg")), vlc: Some(PathBuf::from("/usr/bin/vlc")), ..environment() };

    let terminal = draw(Overview::load_with(&dir, both), WIDE, TALL);
    assert_eq!(panel_rows(terminal.backend().buffer(), RIGHT, 2), ["ffmpeg     [ present ]", "vlc        [ present ]"]);

    let neither = Environment { ffmpeg: None, vlc: None, ..environment() };
    let terminal = draw(Overview::load_with(&dir, neither), WIDE, TALL);
    assert_eq!(panel_rows(terminal.backend().buffer(), RIGHT, 2), ["ffmpeg     [ missing ]", "vlc        [ missing ]"]);
}

#[test]
fn a_pill_keeps_its_brackets_dim_and_its_label_semantic() {
    let dir = export_tree("overview-pill-style");
    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    // Row 0 is ffmpeg (present), row 1 is vlc (missing). The environment panel's interior starts
    // at column 52, and its value column 11 cells further in.
    let bracket = buffer[(63, FIRST_ROW)].style();
    assert_eq!(buffer[(63, FIRST_ROW)].symbol(), "[");
    assert_eq!(bracket.fg, Some(palette.text_dim));
    assert!(!bracket.add_modifier.contains(Modifier::BOLD), "brackets are not part of the label");

    let present = buffer[(65, FIRST_ROW)].style();
    assert_eq!(buffer[(65, FIRST_ROW)].symbol(), "p");
    assert_eq!(present.fg, Some(palette.success));
    assert!(present.add_modifier.contains(Modifier::BOLD));

    let missing = buffer[(65, FIRST_ROW + 1)].style();
    assert_eq!(buffer[(65, FIRST_ROW + 1)].symbol(), "m");
    // Every one of these tools is optional and the pipeline degrades without it, so an absent one
    // is `WARNING` and never `DANGER`.
    assert_eq!(missing.fg, Some(palette.warning));
    assert!(missing.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn a_disk_free_probe_that_failed_renders_unknown_and_leaves_the_tools_alone() {
    let dir = export_tree("overview-no-space");
    let unmeasured = Environment { available_space: None, ..environment() };

    let terminal = draw(Overview::load_with(&dir, unmeasured), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    // The tool that WAS found is still found: the two probes fail independently.
    assert_eq!(panel_rows(buffer, RIGHT, 3), ["ffmpeg     [ present ]", "vlc        [ missing ]", "disk free  unknown"]);

    let value = buffer[(63, FIRST_ROW + 2)].style();
    assert_eq!(buffer[(63, FIRST_ROW + 2)].symbol(), "u");
    assert_eq!(value.fg, Some(Palette::new(Tier::Full).warning), "a failed probe is a charged state, not an absent value");
}

#[test]
fn disk_free_takes_the_abbreviated_binary_form() {
    let dir = export_tree("overview-space-units");

    for (bytes, expected) in [(0_u64, "0 B"), (999, "999 B"), (1024, "1.0 KiB"), (5 * 1024 * 1024, "5.0 MiB"), (2_684_354_560, "2.5 GiB")] {
        let environment = Environment { available_space: Some(bytes), ..environment() };
        let terminal = draw(Overview::load_with(&dir, environment), WIDE, TALL);

        assert_eq!(cell_run(terminal.backend().buffer(), RIGHT, FIRST_ROW + 2), format!("disk free  {expected}"));
    }
}

// ---- panel chrome ----

#[test]
fn both_panels_are_blurred_because_focus_never_reaches_them() {
    // Read-only panes focus never descends into: `LINE` borders and italic titles with no bold.
    // The first panel takes the warm anchor, the second `TEXT_DIM`.
    let dir = export_tree("overview-chrome");
    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();
    let palette = Palette::new(Tier::Full);

    for (x, expected, panel) in [(3, palette.accent_2, "first"), (53, palette.text_dim, "second")] {
        let title = buffer[(x, TOP_BORDER)].style();
        assert_eq!(title.fg, Some(expected), "{panel} panel title");
        assert!(title.add_modifier.contains(Modifier::ITALIC), "{panel} panel title is italic");
        assert!(!title.add_modifier.contains(Modifier::BOLD), "{panel} panel title drops the bold when blurred");
    }

    for (x, panel) in [(0, "first"), (50, "second")] {
        let corner = buffer[(x, TOP_BORDER)].style();
        assert_eq!(buffer[(x, TOP_BORDER)].symbol(), "╭", "{panel} panel corner");
        assert_eq!(corner.fg, Some(palette.line), "{panel} panel border is blurred");
    }
}

#[test]
fn the_two_panel_borders_touch() {
    // 0-cell panel gap in every mode: the left panel's right border and the right panel's left
    // border are adjacent cells.
    let dir = export_tree("overview-gap");
    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    assert_eq!(buffer[(49, TOP_BORDER)].symbol(), "╮");
    assert_eq!(buffer[(50, TOP_BORDER)].symbol(), "╭");
}

/// The fill pin: every panel spans its full allotted body height — its bottom border closes on
/// the body's last row, not capped under its content. Walks the frame for panel top-left corners
/// (`╭`), finds each panel's bottom border (`╰` in the same column), and requires it to close at
/// the body's bottom row (`height - 2`, one above the footer).
fn assert_panels_fill(buffer: &Buffer, top: u16) {
    for y in top..buffer.area.height {
        for x in 0..buffer.area.width {
            if buffer[(x, y)].symbol() != "╭" {
                continue;
            }
            let bottom = (y + 1..buffer.area.height)
                .find(|&by| buffer[(x, by)].symbol() == "╰")
                .unwrap_or_else(|| panic!("the panel at ({x}, {y}) has no bottom border"));
            assert_eq!(
                bottom,
                buffer.area.height - 2,
                "the panel at ({x}, {y}) closes at row {bottom}, not on the body's last row {}",
                buffer.area.height - 2
            );
        }
    }
}

#[test]
fn the_panels_fill_the_body_at_the_designed_sizes() {
    // The fill contract: a panel spans its allotted body height rather than sizing to its rows.
    // At both designed sizes the panels sit side by side, so each one's bottom border must close
    // on the body's last row — the density pass used to cap both under their content.
    let dir = export_tree("overview-fill");
    for (width, height) in [(80, 24), (110, 32)] {
        let terminal = draw(Overview::load_with(&dir, environment()), width, height);
        assert_panels_fill(terminal.backend().buffer(), 1);
    }
}

// ---- too small ----

#[test]
fn a_body_too_narrow_for_both_panels_gives_all_of_it_to_the_summary() {
    // Both panels need 33 cells with this data, so 66 is the last width that fits two halves.
    let dir = export_tree("overview-narrow");
    let overview = || Overview::load_with(&dir, environment());

    let two = draw(overview(), 66, TALL);
    assert_eq!(row(two.backend().buffer(), TOP_BORDER).matches('╮').count(), 2);

    let one = draw(overview(), 65, TALL);
    let single = row(one.backend().buffer(), TOP_BORDER);
    assert_eq!(single.matches('╮').count(), 1, "{single:?}");
    assert!(single.starts_with("╭─ EXPORT SUMMARY ─"), "{single:?}");
    assert!(single.ends_with('╮'), "{single:?}");
    // The environment panel is gone, not squeezed: nothing of its title survives anywhere.
    assert!(!single.contains("ENVIRONMENT"), "{single:?}");
}

#[test]
fn a_body_too_narrow_for_two_columns_stacks_them_instead_of_dropping_one() {
    // Columns run out before rows do, so the narrow answer is to stack (cloudy-tui `mobile.md`:
    // "stack, don't truncate"; hiding a pane is a named anti-pattern there). Both panels need 33
    // cells with this data, so 40 is too narrow for two halves but wide enough for one column.
    let dir = export_tree("overview-stacked");
    let terminal = draw(Overview::load_with(&dir, environment()), 40, TALL);
    let buffer = terminal.backend().buffer();

    assert!(row(buffer, TOP_BORDER).starts_with("╭─ EXPORT SUMMARY "), "{:?}", row(buffer, TOP_BORDER));
    // Five summary rows, then its bottom border, then the environment panel opens on the very next
    // row — 0-row panel gaps, borders touching.
    assert_eq!(
        panel_rows(buffer, 2..38, 5),
        ["parts     2 zips · 1 unpacked", "memories  3 · 2019-2021", "chats     3", "snaps     1", "friends   4",]
    );
    assert!(row(buffer, 7).starts_with('╰'), "{:?}", row(buffer, 7));
    assert!(row(buffer, 8).starts_with("╭─ ENVIRONMENT "), "{:?}", row(buffer, 8));
    assert_eq!(cell_run(buffer, 2..38, 9), "ffmpeg     [ present ]");
}

#[test]
fn stacking_gives_way_to_the_summary_alone_when_the_rows_do_not_fit_either() {
    // Stacked needs 7 + 6 = 13 body rows. At h15 the body is 13 and both panels stack; at h14 it is
    // 12 and neither layout fits, so the summary — the screen's primary content — takes the body.
    let dir = export_tree("overview-stack-floor");

    let stacked = draw(Overview::load_with(&dir, environment()), 40, 15);
    assert!(row(stacked.backend().buffer(), 8).starts_with("╭─ ENVIRONMENT "), "{:?}", row(stacked.backend().buffer(), 8));

    let alone = draw(Overview::load_with(&dir, environment()), 40, 14);
    let frame: String = (0..14).map(|y| row(alone.backend().buffer(), y)).collect();
    assert!(frame.contains("EXPORT SUMMARY"), "{frame:?}");
    assert!(!frame.contains("ENVIRONMENT"), "{frame:?}");
}

#[test]
fn the_two_panel_breakpoint_does_not_move_with_the_source_path_length() {
    // The source value is padded to a fixed width, so the environment panel's own minimum — and
    // from there the layout decision — cannot depend on how deep the user's source dir sits. Both
    // dirs below are absent, so the summary panel's content is identical and the source row is the
    // only difference between the two frames.
    let short = PathBuf::from("/x");
    let long = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("overview-a-considerably-deeper-source-path-that-is-not-there");
    assert!(!short.exists(), "this test needs {} to be absent", short.display());
    assert!(!long.exists(), "this test needs {} to be absent", long.display());

    let panels = |source: &Path, width: u16| {
        let terminal = draw(Overview::load_with(source, environment()), width, TALL);
        row(terminal.backend().buffer(), TOP_BORDER).matches('╮').count()
    };

    for width in 0..=100u16 {
        assert_eq!(panels(&short, width), panels(&long, width), "the layout differs at width {width}");
    }
}

#[test]
fn a_short_source_path_renders_whole() {
    // The padding must not be visible as anything but trailing filler.
    let terminal = draw(Overview::load_with("/x", environment()), WIDE, TALL);
    assert_eq!(cell_run(terminal.backend().buffer(), RIGHT, FIRST_ROW + 3), "source     /x");
}

#[test]
fn a_panel_too_narrow_for_its_widest_row_renders_no_rows_at_all() {
    // The widest summary row is 29 cells, so a panel needs 33 to hold it beside its border and
    // padding. One cell under that the box still names itself and the interior stays blank — a row
    // clipped mid-way would hide its value beside a label that is still there.
    let dir = export_tree("overview-too-narrow");
    let overview = || Overview::load_with(&dir, environment());

    let fits = draw(overview(), 33, TALL);
    assert_eq!(cell_run(fits.backend().buffer(), 2..31, FIRST_ROW), "parts     2 zips · 1 unpacked");

    let blank = draw(overview(), 32, TALL);
    let buffer = blank.backend().buffer();
    assert!(row(buffer, TOP_BORDER).starts_with("╭─ EXPORT SUMMARY "), "{:?}", row(buffer, TOP_BORDER));
    assert_eq!(panel_rows(buffer, 2..30, 5), ["", "", "", "", ""]);
}

#[test]
fn a_body_too_short_for_the_empty_state_frame_leaves_the_panel_blank() {
    // The frame is 4 rows and cannot be cut down without lying about what it says, so it is all or
    // nothing the same way a row is.
    let dir = scratch("overview-short");
    let terminal = draw(Overview::load_with(&dir, environment()), WIDE, 8);
    let buffer = terminal.backend().buffer();

    // Height 8 is under the compact floor, so the banner takes body row 1 and the panel starts at
    // row 2 with 3 interior rows — one short of the frame.
    assert!(row(buffer, 1).starts_with(" ! terminal too small"));
    assert!(row(buffer, 2).starts_with("╭─ EXPORT SUMMARY "), "{:?}", row(buffer, 2));
    assert_eq!(panel_rows(buffer, LEFT, 4).into_iter().skip(1).collect::<Vec<_>>(), ["", "", ""]);
}

#[test]
fn every_summary_row_survives_the_compact_floor() {
    // The height invariant behind `GUARANTEED_INTERIOR_ROWS` claims a body panel gets 10 interior
    // rows at or above the compact floor, and that the size banner cannot eat one of them there.
    // Both halves are reasoning about `shell::render`, so both get checked here.
    //
    // Six rows is the panel's maximum, so this is the frame the invariant is actually about: a gappy
    // delivery, which is the only shape that renders the conditional `missing` row.
    let dir = scratch("overview-compact-floor");
    write_json(&dir.join("mydata~t15").join("json"));
    fs::write(dir.join("mydata~t15-4.zip"), b"").unwrap();

    let expected =
        ["parts     1 zip · 1 unpacked", "missing   2 parts", "memories  3 · 2019-2021", "chats     3", "snaps     1", "friends   4"];

    // At exactly the floor no banner shows: `shell::render` gates the body banner on
    // `area.height < COMPACT_HEIGHT`, and 14 < 14 is false. All six rows render.
    let at_floor = draw(Overview::load_with(&dir, environment()), WIDE, 14);
    let buffer = at_floor.backend().buffer();
    assert!(row(buffer, 0).starts_with(" exportsnap"), "the header owns row 0 at the floor: {:?}", row(buffer, 0));
    assert_eq!(panel_rows(buffer, LEFT, 6), expected);

    // One row down the banner IS showing and takes a body row. The invariant claims nothing here —
    // below the floor is deliberately uncovered — so this is a separate, weaker guard: the rows
    // still fit at 13, and pinning that means a future edit costs `friends` loudly rather than
    // silently. Where they stop fitting is its own test,
    // `row_clipping_begins_one_row_below_the_last_height_that_fits`.
    let banner_showing = draw(Overview::load_with(&dir, environment()), WIDE, 13);
    let buffer = banner_showing.backend().buffer();
    assert!(row(buffer, 1).starts_with(" ! terminal too small"), "the banner owns body row 1: {:?}", row(buffer, 1));
    assert_eq!((0..6).map(|offset| cell_run(buffer, LEFT, 3 + offset)).collect::<Vec<_>>(), expected);
}

#[test]
fn row_clipping_begins_one_row_below_the_last_height_that_fits() {
    // The figures the height invariant's doc quotes. Written down as a test because a number in a
    // comment has already produced two wrong findings on this lane, and an exact figure nobody can
    // check is worth less than none.
    //
    // Below the compact floor the banner is up, so the interior is `h - 5`: the header row, the
    // footer row, the banner row and the panel's two borders. Six rows therefore fit iff `h >= 11`.
    let gappy = scratch("overview-clip-onset-six");
    write_json(&gappy.join("mydata~t16").join("json"));
    fs::write(gappy.join("mydata~t16-4.zip"), b"").unwrap();

    // A complete delivery has no `missing` row, so five rows and one row lower a floor.
    let complete = export_tree("overview-clip-onset-five");

    let rendered = |dir: &Path, height: u16| -> Vec<String> {
        let terminal = draw(Overview::load_with(dir, environment()), WIDE, height);
        let buffer = terminal.backend().buffer();
        // Footer at `h-1`, so the body is rows `1..=h-2`; the banner takes row 1, the panel's top
        // border row 2 and its bottom border row `h-2`. That leaves the interior at `3..h-2`.
        (3..height - 2).map(|y| cell_run(buffer, LEFT, y)).filter(|line| !line.is_empty()).collect()
    };

    assert_eq!(rendered(&gappy, 11).len(), 6, "h11 is the last height that shows all six rows");
    assert_eq!(rendered(&gappy, 10).len(), 5, "clipping starts at h10");
    assert_eq!(rendered(&complete, 10).len(), 5, "five rows still fit one row lower");
    assert_eq!(rendered(&complete, 9).len(), 4, "clipping starts at h9 without the missing row");

    // The row that goes is the LAST one, so what a user loses at h10 is `friends` — not a hole
    // punched in the middle of the block.
    assert_eq!(rendered(&gappy, 10).last().unwrap(), "snaps     1");

    // The panel's bottom border holds its own row at every one of those heights — the banner
    // shrinks the panel area to end at `h - 2`, so the border must close there and the footer
    // row below it must survive. This pins the fill contract's lower edge: the panel spans the
    // whole body rather than stopping under its rows, so its border closes at the body's last
    // row and never overdraws the footer.
    // The corner cell at the panel's left column — the summary panel is anchored at the body's
    // left edge in every arm (side-by-side, stacked, and summary-only alike), so column 0 is
    // its border whichever layout the frame takes.
    let bottom_corner = |dir: &Path, height: u16| {
        let terminal = draw(Overview::load_with(dir, environment()), WIDE, height);
        terminal.backend().buffer()[(0, height - 2)].symbol().to_owned()
    };
    for (dir, height) in [(&gappy, 9), (&gappy, 10), (&gappy, 11), (&complete, 10)] {
        assert_eq!(
            bottom_corner(dir, height),
            "╰",
            "h{height}: the panel's bottom border must close at row {}, not overdraw past it",
            height - 2
        );
    }
}

#[test]
fn load_probes_the_environment_rather_than_leaving_it_empty() {
    // Every other test here uses `load_with` to bypass the real probe, so without this, swapping
    // `Environment::probe(dir)` for `Environment::default()` inside `load` leaves the whole suite
    // green while `load` reports no tools and no disk space.
    //
    // Scope, since it moved: this pins `load`'s own composition and no longer covers the binary.
    // `main` builds every screen through `App::start`, which calls `load_with` and probes once for
    // all three — the equivalent production mutation now lives there and is caught by that
    // composition's own walk-count test.
    //
    // The disk figure is what gets asserted: it is the one part of a real probe whose answer is
    // knowable here. Whether ffmpeg is installed on the machine running this is not this test's
    // business.
    let dir = export_tree("overview-load-composition");
    let terminal = draw(Overview::load(&dir), WIDE, TALL);

    let disk = cell_run(terminal.backend().buffer(), RIGHT, FIRST_ROW + 2);
    assert!(disk.starts_with("disk free  "), "{disk:?}");
    assert_ne!(disk, "disk free  unknown", "load has to run the real space probe");
}

#[test]
fn a_control_character_in_the_source_path_cannot_reach_a_cell() {
    // The source path is the only string on this screen that comes from argv, which makes it the
    // only injection surface here. ratatui filters control graphemes on their way into the buffer
    // (`ratatui-core` 0.1.2 `Span::styled_graphemes`, `Buffer::set_stringn`), so this pins the
    // outcome rather than the mechanism — if a future refactor writes cells directly, it reds.
    let terminal = draw(Overview::load_with("/x\u{1b}[31mred", environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();
    let frame: String = (0..TALL).map(|y| row(buffer, y)).collect();

    assert!(!frame.contains('\u{1b}'), "an escape reached the screen");
}

#[test]
fn every_overview_state_survives_degenerate_sizes() {
    // A panic floor, not coverage: this asserts only that `draw` returns `Ok` at each size. The
    // arithmetic it exercises is the three-way layout ladder, the whole-or-nothing gate and the
    // centred empty-state frame, all of which do width and height subtraction on areas that can be
    // zero.
    let loaded = export_tree("overview-degenerate");
    let bare = scratch("overview-degenerate-empty");
    let sizes = [(0, 0), (0, 20), (20, 0), (1, 1), (1, 3), (3, 1), (2, 2), (4, 4), (5, 5), (33, 3), (66, 4), (255, 1), (1, 255)];

    for source in [&loaded, &bare] {
        for (width, height) in sizes {
            let mut app = App::new(Tier::Full).with_overview(Overview::load_with(source, environment()));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| shell::render(frame, &mut app))
                .unwrap_or_else(|error| panic!("{} at {width}x{height}: {error}", source.display()));
        }
    }
}

// ---- the real export ----

#[test]
fn the_fixture_export_populates_every_summary_row() {
    // `fixtures/` is gitignored, so CI never has it. Asked through the shared gate rather than by
    // rebuilding the path here: this crate's copy of the check was invisible to every census taken
    // by grepping `tests/export.rs`'s helper names, which is how it stayed unpinned longest.
    let Some(root) = common::fixtures::root("the_fixture_export_populates_every_summary_row") else {
        return;
    };

    let terminal = draw(Overview::load_with(&root, environment()), WIDE, TALL);
    let buffer = terminal.backend().buffer();

    // The fixture tree holds one delivery with its `json/` unpacked, so every count resolves —
    // neither placeholder may appear, and the parts row must name the unpacked part.
    let summary = panel_rows(buffer, LEFT, 5);
    assert!(summary[0].starts_with("parts     "), "{summary:?}");
    assert!(summary[0].contains("unpacked"), "{summary:?}");
    for (index, label) in [(1, "memories  "), (2, "chats     "), (3, "snaps     "), (4, "friends   ")] {
        let line = &summary[index];
        assert!(line.starts_with(label), "{line:?}");
        // Trimmed because the three bare counts are left-padded into a shared column; the padding
        // itself is asserted below.
        let value = line[label.len()..].trim();
        assert!(value != "—" && value != "unreadable", "{label:?} did not resolve: {line:?}");
        assert!(value.starts_with(|c: char| c.is_ascii_digit()), "{label:?} is not a count: {line:?}");
    }

    // Real counts are 2-6 digits, so this is where the ragged column would actually show. The three
    // bare-count rows must end on the same column.
    let right_edges: Vec<usize> = summary[2..].iter().map(|line| line.chars().count()).collect();
    assert!(right_edges.iter().all(|edge| *edge == right_edges[0]), "bare counts are ragged: {:?}", &summary[2..]);
}
