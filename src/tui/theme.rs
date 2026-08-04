//! Every cloudy-tui color and glyph the crate renders, plus capability-tier detection and
//! the toast glass-blend helper (cloudy-tui skill, DNA rule 10: raw hex / `Color::Rgb` /
//! one-off glyph literals outside this module are a bug).

use ratatui::style::{Color, Style};

/// Which color depth + glyph set the terminal gets. Auto-detected at startup; an explicit
/// override (CLI flag or config file, resolved by the caller) always wins over env sniffing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    Full,
    Compatible,
}

impl Tier {
    /// Maps a `--theme=` argument or a config `[theme] name` value to a tier. `None` for
    /// anything else, so a caller reports an unrecognized value instead of silently falling
    /// back to a tier the user didn't ask for.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "full" => Some(Self::Full),
            "compatible" => Some(Self::Compatible),
            _ => None,
        }
    }
}

/// The three tier sources, in the contract's precedence order (cloudy-tui skill: Capability
/// tiers → Theme selection precedence). Named fields rather than positional arguments so the
/// two adjacent `Option<Tier>` levels can't be swapped at a call site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TierSources<'a> {
    /// `--theme=full | compatible`. Highest precedence.
    pub cli: Option<Tier>,
    /// `[theme] name = "..."`. No config loader exists yet, so callers pass `None`.
    pub config: Option<Tier>,
    /// `$COLORTERM`. Lowest precedence.
    pub colorterm: Option<&'a str>,
}

/// Pure tier decision (cloudy-tui skill: Capability tiers → Theme selection precedence):
/// CLI flag beats config file beats env detection, where `$COLORTERM=truecolor` selects
/// `Full` and everything else (unset, empty, any other value) falls back to `Compatible`.
/// Kept free of `std::env` so it's directly testable without process-global state; see
/// [`detect_from_env`] for the real startup path.
///
/// SKILL.md's own precedence text names only the literal `truecolor`; the `examples-
/// ratatui.md` reference snippet in the same skill additionally accepts `24bit`. SKILL.md
/// itself resolves that: "This file is the contract. Component code lives in the
/// `examples-ratatui.md` / ... files next to it" — so this follows SKILL.md's literal
/// wording rather than the demo file (which independently drifts from SKILL.md elsewhere too,
/// e.g. its `toast_bg`/`blend` snippet ignores the "unknown/reset under-bg counts as BG"
/// rule) — flagging the inconsistency for skill maintenance instead of resolving it
/// unilaterally in either direction.
pub fn detect(sources: TierSources<'_>) -> Tier {
    if let Some(tier) = sources.cli.or(sources.config) {
        return tier;
    }
    match sources.colorterm {
        Some(value) if value.eq_ignore_ascii_case("truecolor") => Tier::Full,
        _ => Tier::Compatible,
    }
}

/// Reads `$COLORTERM` and applies [`detect`] to it under the two explicit overrides. Call
/// once at startup.
pub fn detect_from_env(cli: Option<Tier>, config: Option<Tier>) -> Tier {
    detect(TierSources { cli, config, colorterm: std::env::var("COLORTERM").ok().as_deref() })
}

/// Every palette role resolved for one tier (cloudy-tui skill: Palette table), so widget code
/// never branches on [`Tier`] to pick a color. The [`full`] and [`compatible`] modules stay
/// the single source of truth; this only selects between them.
///
/// `bg` / `bg_raised` / `bg_sunken` are the surface roles DNA rule 3 leaves unpainted on the
/// `compatible` tier. Paint the base one through [`Palette::surface`], which already resolves
/// that; the bare fields are the color values, not a licence to fill with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bg: Color,
    pub bg_raised: Color,
    pub bg_sunken: Color,
    pub bg_hover: Color,
    pub line: Color,
    pub line_strong: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub accent_2: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    /// Private so widget code can't reach past [`Palette::surface`] and branch on the tier
    /// itself — resolving tier-dependent choices here is the whole point of this type.
    tier: Tier,
}

impl Palette {
    /// The base surface fill, or `None` on a tier that paints no surface fills (DNA rule 3:
    /// on `compatible`, `BG` / `BG_RAISED` / `BG_SUNKEN` inherit the terminal's own
    /// background and elevation falls to borders + color).
    ///
    /// Distinct from the [`Palette::bg`] field, which is the color value itself and stays
    /// usable on both tiers — the compact banner paints it as a *foreground* over a semantic
    /// wash, which is a legible-text choice, not a surface fill.
    #[must_use]
    pub const fn surface(&self) -> Option<Color> {
        match self.tier {
            Tier::Full => Some(self.bg),
            Tier::Compatible => None,
        }
    }

    /// The usage-role color for a percentage (cloudy-tui skill: Progress bar — usage/quota role,
    /// higher = worse). `<60` reads as nothing notable, `60..=80` needs attention, `>80` is
    /// danger — so the boundaries themselves are `WARNING`, pinned by
    /// `the_usage_threshold_is_warning_at_both_boundaries` in `tests/theme.rs`.
    #[must_use]
    pub const fn usage_color(self, percent: u8) -> Color {
        if percent < 60 {
            self.text_dim
        } else if percent <= 80 {
            self.warning
        } else {
            self.danger
        }
    }

    /// The label color of a status pill for a manifest item (cloudy-tui skill: Status pill —
    /// semantic when the state carries a charge, neutral `TEXT_DIM` otherwise). `pending` is the
    /// default state and nothing has gone wrong yet; `source_missing` is a real gap to report.
    #[must_use]
    pub const fn status_pill(self, status: crate::export::manifest::ItemStatus) -> Color {
        use crate::export::manifest::ItemStatus;
        match status {
            ItemStatus::Pending => self.text_dim,
            ItemStatus::Done => self.success,
            ItemStatus::Failed => self.danger,
            ItemStatus::SourceMissing => self.warning,
        }
    }

    /// The fill of a determinate progress bar (cloudy-tui skill: Progress bar — progress role,
    /// higher = good): solid `ACCENT`, no threshold coloring.
    #[must_use]
    pub const fn progress_fill(self) -> Style {
        Style::new().fg(self.accent)
    }

    /// The bare track a determinate bar's `░` run sits on.
    #[must_use]
    pub const fn bar_track(self) -> Style {
        Style::new().fg(self.line)
    }

    /// The toggle control's rendered form (cloudy-tui skill: Toggle row; Capability tiers table):
    /// a two-tone slide switch on `full`, a bracketed `[on]`/`[off]` word on `compatible`. The
    /// tier difference is resolved here so widget code never branches on `Tier`.
    #[must_use]
    pub fn toggle(self, on: bool) -> Vec<ratatui::text::Span<'static>> {
        let track = Style::new().fg(self.line);
        let bracket = Style::new().fg(self.text_dim);
        match (self.tier, on) {
            (Tier::Full, true) => {
                let knob = Style::new().fg(self.accent);
                vec![
                    ratatui::text::Span::styled(full::TOGGLE_TRACK.to_string(), track),
                    ratatui::text::Span::styled(full::TOGGLE_KNOB_ON.to_string(), knob),
                ]
            }
            (Tier::Full, false) => {
                let knob = Style::new().fg(self.text_dim);
                vec![
                    ratatui::text::Span::styled(full::TOGGLE_KNOB_OFF.to_string(), knob),
                    ratatui::text::Span::styled(full::TOGGLE_TRACK.to_string(), track),
                ]
            }
            (Tier::Compatible, true) => vec![
                ratatui::text::Span::styled(compatible::TOGGLE_BRACKET_OPEN.to_string(), bracket),
                ratatui::text::Span::styled(compatible::TOGGLE_WORD_ON, Style::new().fg(self.accent)),
                ratatui::text::Span::styled(compatible::TOGGLE_BRACKET_CLOSE.to_string(), bracket),
            ],
            (Tier::Compatible, false) => vec![
                ratatui::text::Span::styled(compatible::TOGGLE_BRACKET_OPEN.to_string(), bracket),
                ratatui::text::Span::styled(compatible::TOGGLE_WORD_OFF, Style::new().fg(self.text_dim)),
                ratatui::text::Span::styled(compatible::TOGGLE_BRACKET_CLOSE.to_string(), bracket),
            ],
        }
    }

    #[must_use]
    pub const fn new(tier: Tier) -> Self {
        match tier {
            Tier::Full => Self {
                bg: full::BG,
                bg_raised: full::BG_RAISED,
                bg_sunken: full::BG_SUNKEN,
                bg_hover: full::BG_HOVER,
                line: full::LINE,
                line_strong: full::LINE_STRONG,
                text: full::TEXT,
                text_dim: full::TEXT_DIM,
                text_faint: full::TEXT_FAINT,
                accent: full::ACCENT,
                accent_2: full::ACCENT_2,
                success: full::SUCCESS,
                warning: full::WARNING,
                danger: full::DANGER,
                info: full::INFO,
                tier,
            },
            Tier::Compatible => Self {
                bg: compatible::BG,
                bg_raised: compatible::BG_RAISED,
                bg_sunken: compatible::BG_SUNKEN,
                bg_hover: compatible::BG_HOVER,
                line: compatible::LINE,
                line_strong: compatible::LINE_STRONG,
                text: compatible::TEXT,
                text_dim: compatible::TEXT_DIM,
                text_faint: compatible::TEXT_FAINT,
                accent: compatible::ACCENT,
                accent_2: compatible::ACCENT_2,
                success: compatible::SUCCESS,
                warning: compatible::WARNING,
                danger: compatible::DANGER,
                info: compatible::INFO,
                tier,
            },
        }
    }
}

/// 24-bit palette, plus the glyphs that only render on the `full` tier (cloudy-tui skill:
/// Palette table, Capability tiers table).
pub mod full {
    use ratatui::style::Color;

    /// Backs `BG` / `BG_SUNKEN` below and the toast blend's base values (see `toast_bg` in
    /// the parent module) — the single source for both, so the blend can't silently decay
    /// into a plausible-but-wrong color the way extracting components back out of a
    /// `Color::Rgb` via a fallible pattern match could.
    pub(super) const BG_RGB: (u8, u8, u8) = (30, 30, 46);
    pub(super) const BG_SUNKEN_RGB: (u8, u8, u8) = (17, 17, 27);

    const fn rgb((r, g, b): (u8, u8, u8)) -> Color {
        Color::Rgb(r, g, b)
    }

    pub const BG: Color = rgb(BG_RGB);
    pub const BG_RAISED: Color = Color::Rgb(24, 24, 37);
    pub const BG_SUNKEN: Color = rgb(BG_SUNKEN_RGB);
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

    /// Toggle-switch glyphs (Capability tiers table: "Toggle switch"; Toggle row component).
    /// Full tier is a two-tone slide switch — track always `LINE`, "on" knob `ACCENT`, "off"
    /// knob `TEXT_DIM` — so the parts are separate constants for per-span styling: one
    /// `Span` can only carry one color, and the reference (`examples-ratatui.md` §Toggle row)
    /// builds exactly these three pieces rather than one pre-joined string.
    pub const TOGGLE_TRACK: char = '─';
    pub const TOGGLE_KNOB_ON: char = '●';
    pub const TOGGLE_KNOB_OFF: char = '○';
}

/// xterm-256 palette, plus the glyphs that only render on the `compatible` tier. Colors are
/// the skill's own nearest-256 picks (Palette table), not derived at runtime. `nearest_xterm256`
/// below is a separate, general-purpose Euclidean quantizer used only for the toast blend's
/// arbitrary output — it is NOT guaranteed to reproduce this table (verified: disagrees with
/// the table on 5 of 15 roles, e.g. `ACCENT` snaps to 74 here vs. the table's 75) because the
/// table's xterm-256 column isn't itself distance-metric-derived. Don't use one to justify the
/// other.
///
/// `BG` / `BG_RAISED` / `BG_SUNKEN` exist here for completeness and construction, but DNA
/// rule 3 says the `compatible` tier does NOT paint them as ordinary surface fills — only
/// `BG_HOVER` (selected-row tint) and `BG_SUNKEN` (toast glass-blend, via `toast_bg`) are
/// painted. Widget code choosing a background must gate on that; these constants alone don't
/// encode it.
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

    /// Toggle-switch glyphs (Capability tiers table: "Toggle switch"; Toggle row component).
    /// Compatible tier is `[on]`/`[off]` — brackets always `TEXT_DIM`, the word `ACCENT` for
    /// "on" / `TEXT_DIM` for "off" — so brackets and word are separate constants, matching
    /// the full-tier decomposition above and letting a caller color each piece correctly
    /// without inlining a bracket literal (DNA rule 10).
    pub const TOGGLE_BRACKET_OPEN: char = '[';
    pub const TOGGLE_BRACKET_CLOSE: char = ']';
    pub const TOGGLE_WORD_ON: &str = "on";
    pub const TOGGLE_WORD_OFF: &str = "off";
}

/// Glyphs that render identically on both tiers (Capability tiers table marks these "same"),
/// plus the always-lowercase key glyphs from Keyboard grammar. Two cloudy-tui glyphs are
/// deliberately absent because ratatui already provides the exact composite (verified against
/// `ratatui-core` 0.1.2 source, not assumed): panel/border corners (`BorderType::Rounded` /
/// `symbols::border::ROUNDED`, DNA rule 2) and sub-cell block glyphs (`symbols::block`/
/// `bar::NINE_LEVELS`, modernization checklist). The tooltip leader and stacked-hints rail
/// (`└ ├ │`) are likewise absent — `symbols::line::{BOTTOM_LEFT, VERTICAL_RIGHT, VERTICAL}`
/// already give those exact glyphs.
///
/// `TOAST_BAR` and `SCROLLBAR_TRACK`/`SCROLLBAR_THUMB` below DO get their own constants even
/// though the bare glyphs also live in `symbols::line` (`THICK_VERTICAL` = `┃`,
/// `LIGHT_QUADRUPLE_DASH_VERTICAL` = `┊`): ratatui's own `symbols::scrollbar::VERTICAL` preset
/// pairs the wrong glyphs for this contract (`line::VERTICAL` = `│` track / `block::FULL` = `█`
/// thumb, plus arrow caps the contract doesn't have), so the `┊`/`┃` pairing cloudy-tui
/// actually wants has no matching built-in `Set` to defer to and is curated here instead. The
/// toast bar has no ratatui `Set` at all to begin with (toast isn't a ratatui widget).
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

    /// Toast left-bar (DNA rule 2, Toast component). Curated separately from
    /// `SCROLLBAR_THUMB` below even though both are `┃` — see the module doc comment.
    pub const TOAST_BAR: char = '┃';
    /// Scrollbar (component: Scrollbar) — deliberately NOT `ratatui::symbols::scrollbar::
    /// VERTICAL`, whose track/thumb pair (`│`/`█`) doesn't match this contract.
    pub const SCROLLBAR_TRACK: char = '┊';
    pub const SCROLLBAR_THUMB: char = '┃';

    /// Tab bar Overflow form (component: Tab bar → Overflow): only the active tab label
    /// renders, with these markers indicating a prev/next tab exists (`TEXT_FAINT`).
    pub const TAB_OVERFLOW_PREV: char = '‹';
    pub const TAB_OVERFLOW_NEXT: char = '›';

    /// Banner / Footer alert prefix glyph: `!` for a WARNING/DANGER alert, `i` for an INFO
    /// alert (both components).
    pub const ALERT_MARKER: char = '!';
    pub const ALERT_MARKER_INFO: char = 'i';

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
    let (under_r, under_g, under_b) = match under {
        Some(Color::Rgb(r, g, b)) => (r, g, b),
        _ => full::BG_RGB,
    };
    let (base_r, base_g, base_b) = full::BG_SUNKEN_RGB;

    let r = blend_channel(base_r, under_r);
    let g = blend_channel(base_g, under_g);
    let b = blend_channel(base_b, under_b);

    match tier {
        Tier::Full => Color::Rgb(r, g, b),
        Tier::Compatible => nearest_xterm256(r, g, b),
    }
}

fn blend_channel(base: u8, under: u8) -> u8 {
    (0.75 * f32::from(base) + 0.25 * f32::from(under)).round() as u8
}

/// Nearest xterm-256 index for an RGB triple: the 6×6×6 color cube (levels `0, 95, 135, 175,
/// 215, 255`) versus the 24-step grayscale ramp (`8 + 10*n`), picking whichever is closer in
/// squared Euclidean distance.
///
/// This is a general approximation, not a lookup into the palette table above: verified
/// against all 15 named roles, it disagrees with the table's own picks for `LINE`,
/// `LINE_STRONG`, `TEXT_DIM`, `TEXT_FAINT`, and `ACCENT` (the table's xterm-256 column isn't
/// itself Euclidean-nearest, so no distance metric fully reproduces it — don't "correct" this
/// function toward the table's values). It exists solely for the toast blend's arbitrary
/// output, where no lookup table exists to consult instead.
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
        // Pure red sits exactly on a cube vertex; the grayscale ramp can't get close. This
        // input is outside `toast_bg`'s reachable output range (r,g in [13,77], b in [20,84])
        // — it exercises the cube branch directly, not caller behavior.
        assert_eq!(nearest_xterm256(255, 0, 0), Color::Indexed(196));
    }
}
