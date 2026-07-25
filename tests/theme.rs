//! Public-API tests for `exportsnap::tui::theme`: palette values, tier selection, glyph
//! degradation, and the toast blend helper. Cross-check every expected value against the
//! cloudy-tui skill's Palette / Capability tiers / Toast tables, not against this crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use exportsnap::tui::theme::{Palette, Tier, TierSources, compatible, detect, full, glyph, toast_bg};
use ratatui::style::Color;

/// Every source absent — the base every precedence test varies one level away from.
fn no_sources() -> TierSources<'static> {
    TierSources::default()
}

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
fn cli_level_beats_config_level() {
    // Both levels set and disagreeing, so whichever wins is the one the assertion names —
    // neither could pass by falling through to the env level, which disagrees with both.
    assert_eq!(
        detect(TierSources { cli: Some(Tier::Compatible), config: Some(Tier::Full), colorterm: Some("truecolor") }),
        Tier::Compatible
    );
    assert_eq!(detect(TierSources { cli: Some(Tier::Full), config: Some(Tier::Compatible), colorterm: None }), Tier::Full);
}

#[test]
fn config_level_beats_env_detection() {
    assert_eq!(detect(TierSources { config: Some(Tier::Compatible), colorterm: Some("truecolor"), ..no_sources() }), Tier::Compatible);
    assert_eq!(detect(TierSources { config: Some(Tier::Full), colorterm: Some("dumb"), ..no_sources() }), Tier::Full);
}

#[test]
fn cli_level_beats_env_detection_with_no_config() {
    assert_eq!(detect(TierSources { cli: Some(Tier::Compatible), colorterm: Some("truecolor"), ..no_sources() }), Tier::Compatible);
    assert_eq!(detect(TierSources { cli: Some(Tier::Full), ..no_sources() }), Tier::Full);
}

#[test]
fn truecolor_colorterm_selects_full_tier() {
    assert_eq!(detect(TierSources { colorterm: Some("truecolor"), ..no_sources() }), Tier::Full);
}

#[test]
fn truecolor_colorterm_is_case_insensitive() {
    for value in ["TrueColor", "TRUECOLOR"] {
        assert_eq!(detect(TierSources { colorterm: Some(value), ..no_sources() }), Tier::Full);
    }
}

#[test]
fn absent_colorterm_falls_back_to_compatible_tier() {
    assert_eq!(detect(no_sources()), Tier::Compatible);
}

#[test]
fn empty_colorterm_falls_back_to_compatible_tier() {
    assert_eq!(detect(TierSources { colorterm: Some(""), ..no_sources() }), Tier::Compatible);
}

#[test]
fn unrecognized_colorterm_falls_back_to_compatible_tier() {
    // SKILL.md's own precedence text names only the literal `truecolor` (examples-ratatui.md's
    // demo snippet additionally accepts `24bit`, but SKILL.md declares itself "the contract"
    // over the examples files — see the `detect` doc comment in theme.rs). `24bit` is
    // deliberately asserted Compatible here, pinning that reading.
    for value in ["24bit", "256color", "yes"] {
        assert_eq!(detect(TierSources { colorterm: Some(value), ..no_sources() }), Tier::Compatible);
    }
}

#[test]
fn theme_names_map_to_tiers_and_anything_else_is_rejected() {
    assert_eq!(Tier::from_name("full"), Some(Tier::Full));
    assert_eq!(Tier::from_name("compatible"), Some(Tier::Compatible));
    assert_eq!(Tier::from_name("Full"), None);
    assert_eq!(Tier::from_name("truecolor"), None);
    assert_eq!(Tier::from_name(""), None);
}

// ---- palette resolver (skill: Palette table; DNA rule 10) ----

#[test]
fn palette_resolves_every_role_from_the_full_tier_module() {
    let palette = Palette::new(Tier::Full);
    assert_eq!(palette.bg, Color::Rgb(30, 30, 46));
    assert_eq!(palette.bg_raised, Color::Rgb(24, 24, 37));
    assert_eq!(palette.bg_sunken, Color::Rgb(17, 17, 27));
    assert_eq!(palette.bg_hover, Color::Rgb(40, 40, 56));
    assert_eq!(palette.line, Color::Rgb(49, 50, 68));
    assert_eq!(palette.line_strong, Color::Rgb(69, 71, 90));
    assert_eq!(palette.text, Color::Rgb(205, 214, 244));
    assert_eq!(palette.text_dim, Color::Rgb(166, 173, 200));
    assert_eq!(palette.text_faint, Color::Rgb(127, 132, 156));
    assert_eq!(palette.accent, Color::Rgb(67, 171, 229));
    assert_eq!(palette.accent_2, Color::Rgb(217, 119, 87));
    assert_eq!(palette.success, Color::Rgb(166, 227, 161));
    assert_eq!(palette.warning, Color::Rgb(249, 226, 175));
    assert_eq!(palette.danger, Color::Rgb(243, 139, 168));
    assert_eq!(palette.info, Color::Rgb(116, 199, 236));
}

#[test]
fn palette_resolves_every_role_from_the_compatible_tier_module() {
    let palette = Palette::new(Tier::Compatible);
    assert_eq!(palette.bg, Color::Indexed(235));
    assert_eq!(palette.bg_raised, Color::Indexed(234));
    assert_eq!(palette.bg_sunken, Color::Indexed(233));
    assert_eq!(palette.bg_hover, Color::Indexed(236));
    assert_eq!(palette.line, Color::Indexed(238));
    assert_eq!(palette.line_strong, Color::Indexed(240));
    assert_eq!(palette.text, Color::Indexed(189));
    assert_eq!(palette.text_dim, Color::Indexed(145));
    assert_eq!(palette.text_faint, Color::Indexed(102));
    assert_eq!(palette.accent, Color::Indexed(75));
    assert_eq!(palette.accent_2, Color::Indexed(173));
    assert_eq!(palette.success, Color::Indexed(151));
    assert_eq!(palette.warning, Color::Indexed(223));
    assert_eq!(palette.danger, Color::Indexed(211));
    assert_eq!(palette.info, Color::Indexed(117));
}

// ---- glyph degradation per tier (skill: Capability tiers table) ----

#[test]
fn toggle_switch_glyphs_degrade_between_tiers() {
    // One `Span` carries one color, and the skill's toggle is two-tone in both tiers (full:
    // track vs. knob; compatible: brackets vs. word), so the glyphs are parts to be assembled
    // into separately-styled spans, not pre-joined strings — see the `full`/`compatible` doc
    // comments in theme.rs.
    assert_eq!(full::TOGGLE_TRACK, '─');
    assert_eq!(full::TOGGLE_KNOB_ON, '●');
    assert_eq!(full::TOGGLE_KNOB_OFF, '○');
    assert_eq!(compatible::TOGGLE_BRACKET_OPEN, '[');
    assert_eq!(compatible::TOGGLE_BRACKET_CLOSE, ']');
    assert_eq!(compatible::TOGGLE_WORD_ON, "on");
    assert_eq!(compatible::TOGGLE_WORD_OFF, "off");
}

#[test]
fn shared_glyphs_match_skill_literal_values() {
    // These have exactly one definition in `glyph` (no tier split) — Capability tiers table
    // marks them "same" — so this pins the literal values rather than comparing across tiers;
    // there's nothing to compare, since the type itself only offers one value.
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
    assert_eq!(glyph::TAB_OVERFLOW_PREV, '‹');
    assert_eq!(glyph::TAB_OVERFLOW_NEXT, '›');
    assert_eq!(glyph::ALERT_MARKER, '!');
    assert_eq!(glyph::ALERT_MARKER_INFO, 'i');
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
fn toast_bg_rounds_half_away_from_zero_at_exact_boundary() {
    // base=BG_SUNKEN(17,17,27), under=(3,3,1): r/g land exactly on 13.5, b exactly on 20.5 —
    // genuine .5 boundaries, pinning the rounding direction rather than leaving it incidental.
    assert_eq!(toast_bg(Tier::Full, Some(Color::Rgb(3, 3, 1))), Color::Rgb(14, 14, 21));
}

#[test]
fn toast_bg_snaps_to_nearest_xterm256_on_compatible_tier() {
    assert_eq!(toast_bg(Tier::Compatible, None), Color::Indexed(234));
    assert_eq!(toast_bg(Tier::Compatible, Some(Color::Rgb(0, 0, 0))), Color::Indexed(233));
}

#[test]
fn toast_bg_treats_non_rgb_underlay_as_unknown_full_tier() {
    // Indexed/named/Reset colors carry no extractable RGB — the skill's "unknown / reset
    // under-bg counts as BG" fallback applies to all of them, not just literal Reset. Pinned
    // to the literal result (not compared against `toast_bg(.., None)`) so an implementation
    // that ignores `under` entirely couldn't pass vacuously.
    assert_eq!(toast_bg(Tier::Full, Some(Color::Reset)), Color::Rgb(20, 20, 32));
    assert_eq!(toast_bg(Tier::Full, Some(Color::Indexed(42))), Color::Rgb(20, 20, 32));
}

#[test]
fn toast_bg_treats_non_rgb_underlay_as_unknown_compatible_tier() {
    assert_eq!(toast_bg(Tier::Compatible, Some(Color::Reset)), Color::Indexed(234));
    assert_eq!(toast_bg(Tier::Compatible, Some(Color::Indexed(42))), Color::Indexed(234));
}
