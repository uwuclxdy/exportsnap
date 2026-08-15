//! Formatting helpers shared by the screens: cell-aware padding, ellipsis truncation, and the
//! numeric forms the contract's Patterns → Numeric formatting table names.

use ratatui::text::Span;

use super::theme::glyph;

/// Cell width, through the same unicode-aware measure the rendered spans carry.
#[must_use]
pub fn cells(text: &str) -> usize {
    Span::raw(text).width()
}

/// Left-pads to `width` cells so a column of values lines up on its right edge. `format!`'s own
/// `{:>width$}` counts chars, which is the wrong unit the moment a value carries a wide character.
#[must_use]
pub fn left_pad(text: &str, width: usize) -> String {
    format!("{}{text}", " ".repeat(width.saturating_sub(cells(text))))
}

/// Right-pads to `width` cells. Only the trailing filler is invisible, so this is for holding a
/// row's width steady, never for aligning what the reader sees.
#[must_use]
pub fn right_pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(cells(text))))
}

/// The singular or plural form, per the count (Patterns → Counts and plurals).
#[must_use]
pub const fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

/// Head-ellipsis truncation for a path (Patterns → Truncation, path-from-right): the leaf is what
/// identifies a source dir, so the cut takes from the front.
///
/// Measured in cells, not chars: a path is user data and can carry a wide character anywhere in it.
#[must_use]
pub fn head_ellipsis(text: &str, budget: usize) -> String {
    if cells(text) <= budget {
        return text.to_owned();
    }
    if budget == 0 {
        return String::new();
    }

    let mut kept = "";
    for (index, _) in text.char_indices().rev() {
        let tail = &text[index..];
        if cells(tail) + 1 > budget {
            break;
        }
        kept = tail;
    }
    format!("{}{kept}", glyph::ELLIPSIS)
}

/// Middle-ellipsis an id to `budget` cells (Patterns → Truncation): both ends of an id carry
/// meaning, so the cut takes the middle, and the result is right-padded so the column holds its
/// width whatever the id's length.
///
/// The head/tail split is on cells, not chars: a wide character costs its full two cells, so the
/// result can neither overrun the budget nor let the head and tail overlap on a shared character
/// (both failures of a char-count split below `keep` chars). On ascii input the two measures agree,
/// so ascii output is byte-identical to the char split. The cut walks `chars()` and indexes no
/// bytes, so a wide character can never panic the split.
///
/// Shared by both media screens' identity column — a memory's uuid and a chat-media file's
/// `b~<id>` — which is why it sits here beside [`head_ellipsis`] rather than inside one screen.
/// Those ids are ascii by their filename grammars (`every_id_the_filename_grammar_mints_is_ascii`,
/// `every_uuid_the_filename_grammar_mints_is_ascii`), but the free-form call sites — the account
/// username, the history picker's conversation labels, and the place-name column (decision 76) —
/// admit wide characters, and the cell-count cut is what keeps them inside the column.
#[must_use]
pub fn middle_ellipsis(text: &str, budget: usize) -> String {
    if cells(text) <= budget {
        return right_pad(text, budget);
    }
    let keep = budget.saturating_sub(1);
    if keep == 0 {
        return right_pad(&glyph::ELLIPSIS.to_string(), budget);
    }
    let head_budget = keep / 2;
    let tail_budget = keep - keep / 2;

    let mut head = String::new();
    let mut head_cells = 0;
    for character in text.chars() {
        let width = cells(&character.to_string());
        if head_cells + width > head_budget {
            break;
        }
        head_cells += width;
        head.push(character);
    }

    let mut tail_rev = String::new();
    let mut tail_cells = 0;
    for character in text.chars().rev() {
        let width = cells(&character.to_string());
        if tail_cells + width > tail_budget {
            break;
        }
        tail_cells += width;
        tail_rev.push(character);
    }
    let tail: String = tail_rev.chars().rev().collect();
    right_pad(&format!("{head}{}{tail}", glyph::ELLIPSIS), budget)
}

/// Trailing-ellipsis truncation for prose (Patterns → Truncation): the row keeps the text's
/// beginning and a `…` names what was cut. This is the right cut for something READ (a banner,
/// a conversation title), where the identity cuts above serve something IDENTIFIED.
///
/// Measured in cells like the other truncators here — prose is user data and can carry a wide
/// character anywhere — and the char-boundary walk keeps a whole character rather than a clipped
/// half, at the cost of the last cell when the boundary lands there.
#[must_use]
pub fn truncate_prose(text: &str, width: usize) -> String {
    if cells(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut kept = String::new();
    for character in text.chars() {
        if cells(&format!("{kept}{character}")) >= width {
            break;
        }
        kept.push(character);
    }
    kept.push(glyph::ELLIPSIS);
    kept
}

/// Thousands-separated, the form a detail panel uses (Patterns → Numeric formatting).
#[must_use]
pub fn grouped(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.char_indices() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// IEC binary storage with one decimal above a kibibyte, single space before the unit (Patterns →
/// Numeric formatting: storage is `B KiB MiB GiB TiB`).
///
/// Abbreviated rather than the detail panel's full `N bytes`: a byte-exact free-space figure is
/// noise, and the contract's own storage examples are all abbreviated. Counts still take the full
/// comma-separated form, where the exact number is the information.
#[must_use]
pub fn binary_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];

    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64 / 1024.0;
    let mut unit = UNITS[0];
    for next in &UNITS[1..] {
        // 1023.95, not 1024.0: the comparison has to be made against what ONE DECIMAL will print,
        // not against the exact value. Testing `< 1024.0` lets anything in `[1023.95, 1024)` stop
        // here and then render as `1024.0 KiB` — a figure of the smaller unit that reads as the
        // larger one, reachable for a real free-space reading just under any 1 GiB boundary.
        if value < 1023.95 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_prose_cuts_by_cells_so_a_wide_character_stays_whole() {
        // `世` is two cells wide: a 5-cell budget holds the ellipsis plus two of them — the
        // char-count cut would keep four and overrun. The kept text never exceeds the budget.
        assert_eq!(truncate_prose("世世世", 5), "世世…");
        assert_eq!(cells(truncate_prose("世世世", 5).as_str()), 5);
        assert_eq!(truncate_prose("世世世", 3), "世…");
        assert_eq!(truncate_prose("世世世", 2), "…");
        assert_eq!(truncate_prose("世世世", 0), "");
    }

    #[test]
    fn grouped_separates_every_third_digit_from_the_right() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(1), "1");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(12_847), "12,847");
        assert_eq!(grouped(2_576_980_377), "2,576,980,377");
    }

    #[test]
    fn binary_bytes_stays_exact_below_a_kibibyte() {
        assert_eq!(binary_bytes(0), "0 B");
        assert_eq!(binary_bytes(512), "512 B");
        assert_eq!(binary_bytes(1023), "1023 B");
    }

    #[test]
    fn binary_bytes_climbs_one_unit_per_1024() {
        assert_eq!(binary_bytes(1024), "1.0 KiB");
        assert_eq!(binary_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(binary_bytes(2 * 1024 * 1024 * 1024), "2.0 GiB");
        assert_eq!(binary_bytes(1024_u64.pow(4)), "1.0 TiB");
        assert_eq!(binary_bytes(1024_u64.pow(5)), "1.0 PiB");
    }

    #[test]
    fn binary_bytes_never_renders_1024_of_the_smaller_unit() {
        // A value that rounds UP to the next unit's boundary has to carry the next unit's name.
        // `1_048_525 / 1024` is 1023.95 KiB, which one decimal prints as `1024.0` — a figure of the
        // smaller unit reading as the larger one. Reachable for a real free-space figure just under
        // any boundary, which is why the loop compares against what will be printed.
        assert_eq!(binary_bytes(1_048_525), "1.0 MiB");
        assert_eq!(binary_bytes(1_073_689_400), "1.0 GiB");
        assert_eq!(binary_bytes(1_099_511_000_000), "1.0 TiB");
        // One byte below the rounding threshold still belongs to the smaller unit.
        assert_eq!(binary_bytes(1_048_524), "1023.9 KiB");
    }

    #[test]
    fn binary_bytes_keeps_one_decimal_and_stops_at_pebibytes() {
        assert_eq!(binary_bytes(2_684_354_560), "2.5 GiB");
        // u64 runs out inside the exbibytes, and adding a unit for a disk nobody has would be
        // dead code — the figure simply keeps growing in PiB.
        assert_eq!(binary_bytes(u64::MAX), "16384.0 PiB");
    }

    #[test]
    fn a_middle_ellipsis_keeps_both_ends_of_the_id() {
        let id = "2ca92da1-3ff7-45f1-95f9-a2fda6ba0f8e";
        assert_eq!(middle_ellipsis(id, 18), "2ca92da1…da6ba0f8e");
        assert_eq!(middle_ellipsis(id, 36), id);
        assert_eq!(middle_ellipsis(id, 2), "…e");
        // A chat-media id is the other shape this column carries, and its tail is the discriminating
        // half: `b~` heads every one of the 8005 plain files.
        assert_eq!(middle_ellipsis("b~aB3xY9aB3xY9aB3xY9", 12), "b~aB3…aB3xY9");
        assert_eq!(middle_ellipsis("b~aB3xY9aB3xY9aB3xY9", 12).chars().count(), 12);
    }

    /// No id reaches this — see [`middle_ellipsis`]'s own docs for the two grammars that keep the
    /// identity column ascii — but the cut has to survive one anyway, because `&text[..keep / 2]`
    /// is the obvious collapse of the two `chars()` walks and it PANICS here: `keep / 2` is 2 and
    /// `世` is three bytes, so byte 2 is not a char boundary.
    #[test]
    fn a_middle_ellipsis_counts_cells_so_a_wide_character_neither_overruns_nor_overlaps() {
        // `世` is two cells, so a 5-cell budget holds the ellipsis plus one head and one tail wide
        // character (2 + 1 + 2) — never the two per side a char-count split would take.
        assert_eq!(middle_ellipsis("世界世界", 5), "世…界");
        assert_eq!(cells(middle_ellipsis("世界世界", 5).as_str()), 5);
        // Fewer chars than `keep` no longer overlap the head and tail on `界`: the cell-count cut
        // takes one wide character per side and the middle is what the ellipsis replaces.
        assert_eq!(middle_ellipsis("世界語", 5), "世…語");
        // A budget that fits takes the early return, where the split never comes into it.
        assert_eq!(middle_ellipsis("世界世界", 8), "世界世界");
    }

    #[test]
    fn a_path_that_fits_its_budget_is_left_alone() {
        assert_eq!(head_ellipsis("/tmp/export", 18), "/tmp/export");
        assert_eq!(head_ellipsis("/tmp/export", 11), "/tmp/export");
    }

    #[test]
    fn a_longer_path_loses_its_head_and_keeps_its_leaf() {
        // One cell goes to the `…`, so an 18-cell budget keeps the last 17 characters of an ascii
        // path and a 10-cell budget keeps the last 9.
        assert_eq!(head_ellipsis("/tmp/export", 10), "…mp/export");
        assert_eq!(head_ellipsis("/home/someone/snap/export", 18), "…meone/snap/export");
    }

    #[test]
    fn the_cut_counts_cells_so_a_wide_character_costs_two() {
        // `世` is two cells wide, so a 5-cell budget holds the ellipsis plus two of them, not four.
        assert_eq!(head_ellipsis("世界世界", 5), "…世界");
        assert_eq!(head_ellipsis("世界世界", 8), "世界世界");
    }

    #[test]
    fn a_budget_with_no_room_for_content_degrades_to_the_ellipsis_alone() {
        assert_eq!(head_ellipsis("/tmp/export", 1), "…");
        assert_eq!(head_ellipsis("/tmp/export", 0), "");
    }

    #[test]
    fn left_pad_counts_cells_so_a_column_of_counts_lines_up_on_the_right() {
        assert_eq!(left_pad("123", 3), "123");
        assert_eq!(left_pad("45", 3), " 45");
        assert_eq!(left_pad("6", 3), "  6");
        // Padding to zero, or to less than the text needs, never truncates.
        assert_eq!(left_pad("123", 0), "123");
        // A wide character costs two cells, so `format!`'s char-counting `{:>3}` would under-pad.
        assert_eq!(left_pad("世", 3), " 世");
    }

    #[test]
    fn right_pad_holds_a_row_width_steady_without_moving_what_is_read() {
        assert_eq!(right_pad("/tmp", 8), "/tmp    ");
        assert_eq!(right_pad("/tmp", 4), "/tmp");
        assert_eq!(right_pad("/tmp/deeper", 4), "/tmp/deeper");
        assert_eq!(right_pad("世", 3), "世 ");
    }
}
