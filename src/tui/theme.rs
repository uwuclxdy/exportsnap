//! Every cloudy-tui color and glyph the crate renders, plus capability-tier detection and
//! the toast glass-blend helper (cloudy-tui skill, DNA rule 10: raw hex / `Color::Rgb` /
//! one-off glyph literals outside this module are a bug).

use ratatui::style::Color;

/// Which color depth + glyph set the terminal gets. Auto-detected at startup; an explicit
/// override (CLI flag or config file, resolved by the caller) always wins over env sniffing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    Full,
    Compatible,
}

/// Pure tier decision (cloudy-tui skill: Capability tiers → Theme selection precedence).
/// `override_tier` wins outright; otherwise `$COLORTERM=truecolor` selects `Full` and
/// everything else (unset, empty, any other value) falls back to `Compatible`. Kept free of
/// `std::env` so it's directly testable without process-global state; see `detect_from_env`
/// for the real startup path.
pub fn detect(colorterm: Option<&str>, override_tier: Option<Tier>) -> Tier {
    if let Some(tier) = override_tier {
        return tier;
    }
    match colorterm {
        Some(value) if value.eq_ignore_ascii_case("truecolor") => Tier::Full,
        _ => Tier::Compatible,
    }
}

/// Reads `$COLORTERM` and applies [`detect`]. Call once at startup.
pub fn detect_from_env(override_tier: Option<Tier>) -> Tier {
    detect(std::env::var("COLORTERM").ok().as_deref(), override_tier)
}

/// 24-bit palette, plus the glyphs that only render on the `full` tier (cloudy-tui skill:
/// Palette table, Capability tiers table).
pub mod full {
    use ratatui::style::Color;

    pub const BG: Color = Color::Rgb(30, 30, 46);
    pub const BG_RAISED: Color = Color::Rgb(24, 24, 37);
    pub const BG_SUNKEN: Color = Color::Rgb(17, 17, 27);
    pub const BG_HOVER: Color = Color::Rgb(40, 40, 56);
    pub const LINE: Color = Color::Rgb(49, 50, 68);
    pub const LINE_STRONG: Color = Color::Rgb(69, 71, 90);
    pub const TEXT: Color = Color::Rgb(205, 214, 244);
    pub const TEXT_DIM: Color = Color::Rgb(166, 173, 200);
    pub const TEXT_FAINT: Color = Color::Rgb(127, 132, 156);
    pub const ACCENT: Color = Color::Rgb(67, 171, 229);
    pub const ACCENT_2: Color = Color::Rgb(217, 119, 87);
    pub const SUCCESS: Color = Color::Rgb(166, 227, 161);
    pub const WARNING: Color = Color::Rgb(249, 226, 175);
    pub const DANGER: Color = Color::Rgb(243, 139, 168);
    pub const INFO: Color = Color::Rgb(116, 199, 236);

    /// Toggle-switch value glyph (Capability tiers table: "Toggle switch").
    pub const TOGGLE_ON: &str = "─●";
    pub const TOGGLE_OFF: &str = "○─";
}

/// xterm-256 palette, plus the glyphs that only render on the `compatible` tier. Colors are
/// the skill's own nearest-256 picks (Palette table), not derived at runtime — see
/// `nearest_xterm256` below for the general case used by the toast blend.
pub mod compatible {
    use ratatui::style::Color;

    pub const BG: Color = Color::Indexed(235);
    pub const BG_RAISED: Color = Color::Indexed(234);
    pub const BG_SUNKEN: Color = Color::Indexed(233);
    pub const BG_HOVER: Color = Color::Indexed(236);
    pub const LINE: Color = Color::Indexed(238);
    pub const LINE_STRONG: Color = Color::Indexed(240);
    pub const TEXT: Color = Color::Indexed(189);
    pub const TEXT_DIM: Color = Color::Indexed(145);
    pub const TEXT_FAINT: Color = Color::Indexed(102);
    pub const ACCENT: Color = Color::Indexed(75);
    pub const ACCENT_2: Color = Color::Indexed(173);
    pub const SUCCESS: Color = Color::Indexed(151);
    pub const WARNING: Color = Color::Indexed(223);
    pub const DANGER: Color = Color::Indexed(211);
    pub const INFO: Color = Color::Indexed(117);

    /// Toggle-switch value glyph (Capability tiers table: "Toggle switch").
    pub const TOGGLE_ON: &str = "[on]";
    pub const TOGGLE_OFF: &str = "[off]";
}

/// Glyphs that render identically on both tiers (Capability tiers table marks these "same"),
/// plus the always-lowercase key glyphs from Keyboard grammar. Panel/border corners are
/// deliberately absent: `ratatui::widgets::BorderType::Rounded` /
/// `ratatui::symbols::border::ROUNDED` already give the exact `╭─╮ ╰─╯ │` set (DNA rule 2),
/// and hand-written sub-cell block glyphs are `ratatui::symbols::block`/`bar::NINE_LEVELS`
/// per the modernization checklist — redefining either here would just be a stale duplicate.
pub mod glyph {
    // Status dot (component: Status dot).
    pub const STATUS_DOT_ACTIVE: char = '●';
    pub const STATUS_DOT_INACTIVE: char = '○';
    /// Shared by `queued` (ACCENT) and `idle` (TEXT_DIM) — color carries the distinction, a
    /// bare dot never renders without its status word.
    pub const STATUS_DOT_HOLLOW: char = '◌';
    /// `awaiting input` (ACCENT). Same codepoint as `ELLIPSIS`, distinct role.
    pub const STATUS_AWAITING_INPUT: char = '…';

    pub const SELECTION_CARET: char = '❯';
    pub const EDIT_GLYPH: char = '✎';
    pub const DISCLOSURE_COLLAPSED: char = '▶';
    pub const DISCLOSURE_EXPANDED: char = '▼';
    pub const DONE_MARK: char = '✓';
    pub const ELLIPSIS: char = '…';

    pub const TOAST_BAR: char = '┃';
    pub const SCROLLBAR_TRACK: char = '┊';
    pub const SCROLLBAR_THUMB: char = '┃';

    /// Progress bar (component: Progress bar). `█`/`░` are `ratatui::symbols::block::FULL` /
    /// `ratatui::symbols::shade::LIGHT`; the flow variant has no ratatui equivalent.
    pub const PROGRESS_FILL_FLOW: char = '▰';
    pub const PROGRESS_TRACK_FLOW: char = '▱';

    /// 10-frame braille spinner, advance every 80ms (component: Spinner).
    pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

    /// No-logo header separator (App shell: brand • first tab) and clause separator
    /// (Banner/Toast copy: ` · `). Distinct codepoints — bullet vs. mid-dot.
    pub const HEADER_SEPARATOR: char = '•';
    pub const CLAUSE_SEPARATOR: char = '·';

    // Keyboard grammar: key names render as bare glyphs, never bracketed or spelled out.
    pub const KEY_ENTER: char = '↵';
    pub const KEY_UP: char = '↑';
    pub const KEY_DOWN: char = '↓';
    pub const KEY_LEFT: char = '←';
    pub const KEY_RIGHT: char = '→';
    pub const KEY_SHIFT: char = '⇧';
    pub const KEY_CTRL: char = '⌃';
    pub const KEY_ALT: char = '⌥';
}

/// Blends the toast "glass" background over whatever sits beneath it: `0.75 · BG_SUNKEN +
/// 0.25 · under`, snapped to the nearest xterm-256 color on the `compatible` tier (Toast
/// component: Background). `under: None` — an unknown or reset cell — counts as `BG`.
///
/// Both the alpha blend and the 256-color snap are hand-rolled: ratatui has no translucency
/// primitive and no color-distance API (ratatui-patterns limitations.md: "alpha-blend /
/// translucency" and "xterm-256 nearest-color quantization" are both listed gaps).
pub fn toast_bg(tier: Tier, under: Option<Color>) -> Color {
    let (base_r, base_g, base_b) = rgb_tuple(full::BG_SUNKEN);
    let (under_r, under_g, under_b) = match under {
        Some(Color::Rgb(r, g, b)) => (r, g, b),
        _ => rgb_tuple(full::BG),
    };

    let r = blend_channel(base_r, under_r);
    let g = blend_channel(base_g, under_g);
    let b = blend_channel(base_b, under_b);

    match tier {
        Tier::Full => Color::Rgb(r, g, b),
        Tier::Compatible => nearest_xterm256(r, g, b),
    }
}

/// Extracts `(r, g, b)` from one of this module's own `full::*` constants, which are always
/// the `Rgb` variant — the fallback keeps the function total without a panic.
const fn rgb_tuple(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

fn blend_channel(base: u8, under: u8) -> u8 {
    (0.75 * f32::from(base) + 0.25 * f32::from(under)).round() as u8
}

/// Nearest xterm-256 index for an RGB triple: the 6×6×6 color cube (levels `0, 95, 135, 175,
/// 215, 255`) versus the 24-step grayscale ramp (`8 + 10*n`), picking whichever is closer in
/// squared Euclidean distance.
fn nearest_xterm256(r: u8, g: u8, b: u8) -> Color {
    const CUBE_LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];

    let nearest_cube_index = |v: u8| -> usize {
        let mut best_index = 0;
        let mut best_diff = i32::MAX;
        for (i, &level) in CUBE_LEVELS.iter().enumerate() {
            let diff = (i32::from(v) - level).abs();
            if diff < best_diff {
                best_diff = diff;
                best_index = i;
            }
        }
        best_index
    };

    let sq_dist = |cr: i32, cg: i32, cb: i32| -> i64 {
        let dr = i64::from(i32::from(r) - cr);
        let dg = i64::from(i32::from(g) - cg);
        let db = i64::from(i32::from(b) - cb);
        dr * dr + dg * dg + db * db
    };

    let (ri, gi, bi) = (nearest_cube_index(r), nearest_cube_index(g), nearest_cube_index(b));
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    let cube_dist = sq_dist(CUBE_LEVELS[ri], CUBE_LEVELS[gi], CUBE_LEVELS[bi]);

    let avg = (f32::from(r) + f32::from(g) + f32::from(b)) / 3.0;
    let gray_step = ((avg - 8.0) / 10.0).round().clamp(0.0, 23.0) as i32;
    let gray_val = 8 + 10 * gray_step;
    let gray_index = 232 + gray_step;
    let gray_dist = sq_dist(gray_val, gray_val, gray_val);

    let index = if cube_dist <= gray_dist { cube_index } else { gray_index as usize };
    Color::Indexed(index as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    // nearest_xterm256 is private; only its two branches (cube winner, grayscale winner) are
    // worth pinning directly — the full blend pipeline is covered as public API in
    // tests/theme.rs.
    #[test]
    fn nearest_xterm256_picks_grayscale_ramp_for_near_neutral_input() {
        // (17, 17, 27) is near-black-and-slightly-blue: grayscale ramp wins over the cube.
        assert_eq!(nearest_xterm256(17, 17, 27), Color::Indexed(233));
    }

    #[test]
    fn nearest_xterm256_picks_cube_for_saturated_input() {
        // Pure red sits exactly on a cube vertex; the grayscale ramp can't get close.
        assert_eq!(nearest_xterm256(255, 0, 0), Color::Indexed(196));
    }
}
