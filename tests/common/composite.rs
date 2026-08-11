//! The one answer to "did the overlay's transparent region leave the main showing through", shared
//! by the two crates that composite a main and a caption layer.
//!
//! **`tests/chat_fix.rs` and `tests/local_fix.rs` both drive one `local_fix::fix`, so they were
//! asserting one property twice.** The copy was never wrong, only un-generalized, and three tasks
//! aimed elsewhere widened the gap between the halves: the drift figures below were re-measured on
//! the `local_fix` side alone, and the `chat_fix` copy kept a two-argument signature and a
//! hard-coded block spelled `[48, 8, 56, 16]` — [`TRANSPARENT_BLOCK`] to the digit. Same class as
//! the three spellings of "is the `fixtures/` tree usable" that [`super::fixtures`] exists to
//! collapse.
//!
//! **The block constants live here with the assertion because its doc's measured table names
//! them.** Split them from it and the table can fork from the constants it cites, which is the
//! defect this module was written to stop. [`super::fixtures`]'s own doc records the general form:
//! two halves nothing links drift apart, and the grep that would have caught it reaches one.
//!
//! `local_fix`'s `OPAQUE_BLOCK` deliberately stayed behind. It is handed to that crate's
//! `block_mean`, never to [`assert_shows_main_through`], and no doc here names it.

use image::RgbImage;

/// The colour `chat_fix`'s `paint_jpeg` and `local_fix`'s `write_main_sized` both paint at
/// `(x, y)`, which is what a transparent overlay region has to leave showing.
///
/// The two painters build different fixture SHAPES — a zip pair's media half against a memory's
/// main file — and are deliberately NOT merged. What they share is this expression, verified
/// byte-identical across all three spellings, and [`assert_shows_main_through`] is only sound while
/// they do: a shared assertion against two painters that had drifted would be worse than the fork
/// it replaced.
///
/// **`local_fix`'s third painter, `write_main_shaded`, deliberately disagrees on red** and is
/// handed to no call site of that assertion. It exists so two items landing on one path produce
/// different bytes; reading this colour off it would be wrong on channel 0 by construction.
pub fn main_colour(x: u32, y: u32) -> [u8; 3] {
    [(x % 7) as u8 * 5, ((x * 13 + y * 7) % 251) as u8, ((x * 29 + y * 17) % 253) as u8]
}

/// Asserts the overlay's transparent region left the MAIN showing through, on all three channels.
///
/// **What used to stand here was a brightness threshold on subpixel 0, and it could not discriminate
/// at all.** [`main_colour`]'s red is `(x % 7) * 5` and every block below is 8 wide, so all four
/// span the full residue cycle: the main's red runs 0-30 per pixel, which is what that threshold
/// sampled, and its block mean lands 13-21, against black's 0. `< 60` therefore passed whether the
/// transparent half showed the main through or the alpha had been dropped to black — the fixture
/// held the asserted channel near-constant across the two outcomes, which is this repo's own
/// recorded trap. Green and blue separate them, and matching all three also reds on a composite onto
/// any invented background rather than only on black.
///
/// **Asserted as a block MEAN rather than as one pixel, and that is the load-bearing half.** Both
/// mains are high-frequency by design and each painter's own doc says why — `write_main`'s because
/// the byte-for-byte copy test needs a pattern a JPEG re-encode cannot reproduce, `paint_jpeg`'s so
/// that a composite which ran and one which did not differ on more than one channel — and JPEG's DCT
/// smears neighbours: measured on these fixtures, a lone pixel drifts up to 21 levels across the two
/// generations they carry, which is close enough to the gap being detected that a per-pixel
/// tolerance would be guessing. A block mean is essentially the DC coefficient, which JPEG preserves
/// closely, so the comparison stays tight.
///
/// **The block is the caller's, because which region is transparent moves with the fixture.**
/// [`TRANSPARENT_BLOCK`] is the 64x48 pairs'; a frame of another size puts its own somewhere else
/// entirely. Whatever a caller passes has to lie wholly inside the transparent half and clear of
/// its edge, where an overlay scaled by Lanczos rings alpha across the boundary.
///
/// **Both halves of the margin are per-block and neither generalizes for free**, so a new block
/// re-measures instead of inheriting these. The drift is a property of the quantiser at that
/// block's frequency content; the gap to black is a property of where that block's pseudo-random
/// green and blue happen to average. Measured 2026-08-11 through this assertion, worst channel and
/// then the green/blue the main shows through at:
///
/// | block | reached from | drift | green/blue |
/// |---|---|---|---|
/// | [`TRANSPARENT_BLOCK`] | both crates | 3.47 | 126/131 |
/// | [`WIDE_TRANSPARENT_BLOCK`] | `local_fix` | 3.16 | 82/129 |
/// | [`TALL_TRANSPARENT_BLOCK_BOTTOM`] | `local_fix` | 0.95 | 100/129 |
/// | [`TALL_TRANSPARENT_BLOCK_EITHER_SIZE`] | `local_fix` | 3.59 | 140/124 |
///
/// Drift spans nearly 4x across the four and the gaps run 82 to 140, which is why both are stated
/// per block rather than as one number.
///
/// **The first row is one figure because it was measured from BOTH crates and they agreed, not
/// because one was carried over.** `chat_fix` composites a zip pair and `local_fix` a memory pair,
/// so the two legs could have quantised differently; measured at all four call sites that pass this
/// block — `local_fix`'s png and webp pairs, `chat_fix`'s zip pair, and its overlay-mode loop, which
/// reaches it once per compositing mode — every one of the five invocations reported 3.47 at
/// 126.27/131.38. One `local_fix::fix` does the compositing under both plans, and the fixtures agree
/// on size and on [`main_colour`], which is what makes the figure shared rather than coincidental.
pub fn assert_shows_main_through(composite: &RgbImage, block: [u32; 4], label: &str) {
    /// Clears the worst measured drift by better than 2x and sits an order of magnitude under the
    /// smallest of the four gaps that separate the main from black on green and blue.
    const TOLERANCE: f64 = 8.0;

    let [left, top, right, bottom] = block;
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
        assert!(drift.abs() <= TOLERANCE, "{label}: channel {channel} averaged {actual:?} over {block:?}, expected about {expected:?}");
    }
}

/// A block wholly inside the overlay's TRANSPARENT half and away from its edge, on the 64x48 pairs.
/// What the 64x48 callers in both crates hand [`assert_shows_main_through`], which takes its block
/// from the caller for the reason its doc gives.
pub const TRANSPARENT_BLOCK: [u32; 4] = [48, 8, 56, 16];

/// [`TRANSPARENT_BLOCK`] for the 4608x64 pair, whose scaled overlay puts its transparent half
/// somewhere else entirely. Named rather than inlined at the call site so a grep for one of these
/// blocks reaches every one of them.
pub const WIDE_TRANSPARENT_BLOCK: [u32; 4] = [4000, 24, 4008, 32];

/// [`TRANSPARENT_BLOCK`] for the 1440x2560 pair's bottom rows. Scaled, the overlay covers the whole
/// frame and its opaque half ends at x = 720, so this sits 680px into the transparent half — clear
/// of the ringing Lanczos leaves at that boundary — and 32 short of the frame's right edge.
/// **Only the scale-up puts the overlay here at all**: unscaled and centred it spans 180..1260 x
/// 320..2240, which this block is outside on both axes, so a dropped resize leaves the main showing
/// here too and this assertion stays green through it (measured 2026-08-11). The sibling `> 200`
/// assert is what observes the resize.
///
/// **What it holds over [`TALL_TRANSPARENT_BLOCK_EITHER_SIZE`] is spatial coverage, not a second
/// discrimination**: it is the only TRANSPARENCY sample in the region the scale-up alone reaches
/// (the sibling `> 200` covers that region on the opaque claim), but no mutation tried on
/// 2026-08-11 — dropped compositing, dropped resize, `Nearest` in place of Lanczos — killed either
/// block without the other. Deleting it costs that coverage and nothing measured.
pub const TALL_TRANSPARENT_BLOCK_BOTTOM: [u32; 4] = [1400, 2496, 1408, 2504];

/// [`TRANSPARENT_BLOCK`] for the 1440x2560 pair, in the region the overlay's transparent half covers
/// at EITHER size: 280px right of the opaque half's edge, which both placements put at x = 720, and
/// inside the 720..1260 x 320..2240 the unscaled centred overlay leaves transparent. So it reads
/// "the alpha composited" whether or not the resize ran, which is the half
/// [`TALL_TRANSPARENT_BLOCK_BOTTOM`] cannot cover.
pub const TALL_TRANSPARENT_BLOCK_EITHER_SIZE: [u32; 4] = [1000, 1000, 1008, 1008];
