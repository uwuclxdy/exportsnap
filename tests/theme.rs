//! Public-API tests for `exportsnap::tui::theme`: palette values, tier selection, glyph
//! degradation, and the toast blend helper. Cross-check every expected value against the
//! cloudy-tui skill's Palette / Capability tiers / Toast tables, not against this crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use exportsnap::tui::theme::{Tier, compatible, detect, full, glyph, toast_bg};
use ratatui::style::Color;

// ---- palette: exact hex + xterm-256 values (skill: Palette table) ----

#[test]
fn full_tier_palette_matches_skill_table() {
    assert_eq!(full::BG, Color::Rgb(30, 30, 46));
    assert_eq!(full::BG_RAISED, Color::Rgb(24, 24, 37));
    assert_eq!(full::BG_SUNKEN, Color::Rgb(17, 17, 27));
    assert_eq!(full::BG_HOVER, Color::Rgb(40, 40, 56));
    assert_eq!(full::LINE, Color::Rgb(49, 50, 68));
    assert_eq!(full::LINE_STRONG, Color::Rgb(69, 71, 90));
    assert_eq!(full::TEXT, Color::Rgb(205, 214, 244));
    assert_eq!(full::TEXT_DIM, Color::Rgb(166, 173, 200));
    assert_eq!(full::TEXT_FAINT, Color::Rgb(127, 132, 156));
    assert_eq!(full::ACCENT, Color::Rgb(67, 171, 229));
    assert_eq!(full::ACCENT_2, Color::Rgb(217, 119, 87));
    assert_eq!(full::SUCCESS, Color::Rgb(166, 227, 161));
    assert_eq!(full::WARNING, Color::Rgb(249, 226, 175));
    assert_eq!(full::DANGER, Color::Rgb(243, 139, 168));
    assert_eq!(full::INFO, Color::Rgb(116, 199, 236));
}

#[test]
fn compatible_tier_palette_matches_skill_table() {
    assert_eq!(compatible::BG, Color::Indexed(235));
    assert_eq!(compatible::BG_RAISED, Color::Indexed(234));
    assert_eq!(compatible::BG_SUNKEN, Color::Indexed(233));
    assert_eq!(compatible::BG_HOVER, Color::Indexed(236));
    assert_eq!(compatible::LINE, Color::Indexed(238));
    assert_eq!(compatible::LINE_STRONG, Color::Indexed(240));
    assert_eq!(compatible::TEXT, Color::Indexed(189));
    assert_eq!(compatible::TEXT_DIM, Color::Indexed(145));
    assert_eq!(compatible::TEXT_FAINT, Color::Indexed(102));
    assert_eq!(compatible::ACCENT, Color::Indexed(75));
    assert_eq!(compatible::ACCENT_2, Color::Indexed(173));
    assert_eq!(compatible::SUCCESS, Color::Indexed(151));
    assert_eq!(compatible::WARNING, Color::Indexed(223));
    assert_eq!(compatible::DANGER, Color::Indexed(211));
    assert_eq!(compatible::INFO, Color::Indexed(117));
}

// ---- tier selection matrix (skill: Capability tiers → Theme selection precedence) ----

#[test]
fn override_wins_over_truecolor_colorterm() {
    assert_eq!(detect(Some("truecolor"), Some(Tier::Compatible)), Tier::Compatible);
}

#[test]
fn override_wins_over_absent_colorterm() {
    assert_eq!(detect(None, Some(Tier::Full)), Tier::Full);
}

#[test]
fn truecolor_colorterm_selects_full_tier() {
    assert_eq!(detect(Some("truecolor"), None), Tier::Full);
}

#[test]
fn truecolor_colorterm_is_case_insensitive() {
    assert_eq!(detect(Some("TrueColor"), None), Tier::Full);
    assert_eq!(detect(Some("TRUECOLOR"), None), Tier::Full);
}

#[test]
fn absent_colorterm_falls_back_to_compatible_tier() {
    assert_eq!(detect(None, None), Tier::Compatible);
}

#[test]
fn empty_colorterm_falls_back_to_compatible_tier() {
    assert_eq!(detect(Some(""), None), Tier::Compatible);
}

#[test]
fn unrecognized_colorterm_falls_back_to_compatible_tier() {
    // Common near-miss values (e.g. `24bit`) are still not `truecolor`; the skill names only
    // that one literal value, so anything else — however truecolor-adjacent — degrades.
    assert_eq!(detect(Some("24bit"), None), Tier::Compatible);
    assert_eq!(detect(Some("256color"), None), Tier::Compatible);
    assert_eq!(detect(Some("yes"), None), Tier::Compatible);
}

// ---- glyph degradation per tier (skill: Capability tiers table) ----

#[test]
fn toggle_switch_glyph_degrades_between_tiers() {
    assert_eq!(full::TOGGLE_ON, "─●");
    assert_eq!(full::TOGGLE_OFF, "○─");
    assert_eq!(compatible::TOGGLE_ON, "[on]");
    assert_eq!(compatible::TOGGLE_OFF, "[off]");
}

#[test]
fn shared_glyphs_are_identical_regardless_of_tier() {
    // Capability tiers table: borders, status dot, caret, edit glyph, disclosure, done mark,
    // spinner, ellipsis, toast bar, scrollbar all render "same" on both tiers — there is
    // exactly one definition of each in `glyph`, so this pins the literal values rather than
    // a tier comparison.
    assert_eq!(glyph::STATUS_DOT_ACTIVE, '●');
    assert_eq!(glyph::STATUS_DOT_INACTIVE, '○');
    assert_eq!(glyph::STATUS_DOT_HOLLOW, '◌');
    assert_eq!(glyph::STATUS_AWAITING_INPUT, '…');
    assert_eq!(glyph::SELECTION_CARET, '❯');
    assert_eq!(glyph::EDIT_GLYPH, '✎');
    assert_eq!(glyph::DISCLOSURE_COLLAPSED, '▶');
    assert_eq!(glyph::DISCLOSURE_EXPANDED, '▼');
    assert_eq!(glyph::DONE_MARK, '✓');
    assert_eq!(glyph::ELLIPSIS, '…');
    assert_eq!(glyph::TOAST_BAR, '┃');
    assert_eq!(glyph::SCROLLBAR_TRACK, '┊');
    assert_eq!(glyph::SCROLLBAR_THUMB, '┃');
    assert_eq!(glyph::SPINNER_FRAMES, ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']);
}

// ---- toast blend helper (skill: Toast → Background) ----

#[test]
fn toast_bg_blends_sunken_over_unknown_background_as_bg_full_tier() {
    // under=None counts as BG: 0.75*BG_SUNKEN(17,17,27) + 0.25*BG(30,30,46), rounded.
    assert_eq!(toast_bg(Tier::Full, None), Color::Rgb(20, 20, 32));
}

#[test]
fn toast_bg_blends_sunken_over_known_background_full_tier() {
    // 0.75*BG_SUNKEN(17,17,27) + 0.25*black(0,0,0), rounded.
    assert_eq!(toast_bg(Tier::Full, Some(Color::Rgb(0, 0, 0))), Color::Rgb(13, 13, 20));
}

#[test]
fn toast_bg_snaps_to_nearest_xterm256_on_compatible_tier() {
    assert_eq!(toast_bg(Tier::Compatible, None), Color::Indexed(234));
    assert_eq!(toast_bg(Tier::Compatible, Some(Color::Rgb(0, 0, 0))), Color::Indexed(233));
}

#[test]
fn toast_bg_treats_non_rgb_underlay_as_unknown() {
    // Indexed/named/Reset colors carry no extractable RGB — the skill's "unknown / reset
    // under-bg counts as BG" fallback applies to all of them, not just literal Reset.
    assert_eq!(toast_bg(Tier::Full, Some(Color::Reset)), toast_bg(Tier::Full, None));
    assert_eq!(toast_bg(Tier::Full, Some(Color::Indexed(42))), toast_bg(Tier::Full, None));
}
