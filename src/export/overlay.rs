//! Drawing a memory's overlay layer back over the media it belongs to.
//!
//! Snapchat ships captions and stickers as a separate transparent image beside the media rather
//! than burned into it, so a downloaded memory is missing everything that was drawn on it. The
//! two files share a uuid and [`crate::export::memories`] has already paired them; this module is
//! the pixels.
//!
//! **Two encoders, one hardcoded format each, and neither takes a format argument.**
//! [`compose_jpeg`] flattens the composite; [`compose_png`] keeps its alpha channel, for a main
//! whose own format can carry one that JPEG would drop. **Which of the two runs is the CALLER's
//! choice, and since task 70 it is taken from the RESOLVED extension** — the same string the plan
//! built the output name out of — so encoder and extension cannot disagree however many formats keep
//! their own. A format this module has no encoder for is refused by name at that call site rather
//! than handed one of these two; `local_fix`'s `compose_own_format` is where both halves live.
//!
//! **This module used to produce JPEG bytes whatever went in, and used to claim that was what kept
//! a PNG out of `little_exif`. The claim was never what held the property, and it is false now as
//! well.** Nothing here calls into `little_exif` under either encoder, so PNG bytes existing in this
//! process cost that property nothing. What DOES hold it is stated in one place and deliberately not
//! restated here — [`crate::export::exif`]'s `library` module doc, which separates a compiler half
//! from a convention half that a shorter retelling fuses back together.
//!
//! The 162 overlay files in the observed export all carry `.png` names, though 9 of them hold WebP
//! payloads under that name (measured 2026-08-04); an overlay is a layer in a composite here, never
//! a file that gets stamped.

use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::{self, FilterType};
use image::{ImageError, ImageFormat, ImageReader, RgbaImage};

/// How much the re-encode is allowed to cost.
///
/// Only [`compose_jpeg`] pays it — [`compose_png`] is lossless and a main with no overlay is copied
/// byte for byte without reaching this module at all. 95 is high enough that the re-encode is not
/// the lossy step next to whatever compression Snapchat already applied; the cost is roughly a third
/// more bytes than the crate's default 75, which for an archive of one's own photos is the right
/// side of that trade.
const JPEG_QUALITY: u8 = 95;

/// `main` with `overlay` drawn over it, flattened and encoded as JPEG bytes.
///
/// **The alpha channel is dropped here and that is only safe for a main JPEG could hold anyway.**
/// `to_rgb8` does not composite the channel onto anything, it discards it, so whatever RGB sat under
/// `alpha = 0` is what lands. For what the OVERLAY left transparent that is exactly right — the main
/// is underneath it and shows through. For transparency the MAIN itself carries there is nothing
/// underneath at all, and the caller routes those to [`compose_png`] instead.
///
/// **Stated ceiling: the caller routes on the main's NAME while [`decode`] reads its format from the
/// CONTENT.** So a payload that carries alpha under a `.jpg` name still arrives here and is still
/// flattened onto whatever sat under `alpha = 0`. Not hypothetical framing — this export is known to
/// mislabel image payloads by extension, 9 of its 162 overlays holding WebP under `.png` names
/// (measured 2026-08-04). Unchanged by task 45 and left open rather than closed: closing it needs a
/// decode at plan time, which is exactly what deciding every output path up front rules out.
///
/// # Errors
///
/// Returns [`OverlayError`] when either file cannot be read or decoded, or when the composite
/// cannot be encoded.
pub fn compose_jpeg(main: &Path, overlay: &Path) -> Result<Vec<u8>, OverlayError> {
    let flattened = image::DynamicImage::ImageRgba8(composite(main, overlay)?).to_rgb8();
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY).encode_image(&flattened).map_err(|source| OverlayError::Encode { source })?;
    Ok(bytes)
}

/// `main` with `overlay` drawn over it, encoded as PNG bytes with the alpha channel intact.
///
/// For a main whose own format can carry transparency: the composite keeps four channels all the way
/// to the encoder, so a region both layers left transparent comes out transparent rather than as the
/// black that sat under it. Lossless, so unlike [`compose_jpeg`] this spends no generation of
/// compression — what it costs instead is the capture metadata, since this build writes EXIF into a
/// JPEG and nothing else, and the run reports that per item.
///
/// # Errors
///
/// Returns [`OverlayError`] when either file cannot be read or decoded, or when the composite
/// cannot be encoded.
pub fn compose_png(main: &Path, overlay: &Path) -> Result<Vec<u8>, OverlayError> {
    let composited = composite(main, overlay)?;
    let mut bytes = Vec::new();
    composited.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).map_err(|source| OverlayError::Encode { source })?;
    Ok(bytes)
}

/// The pixels both encoders share: `main` with `overlay` drawn over it, still RGBA.
///
/// **There is always an overlay.** The parameter used to be optional, for a main that needed
/// re-encoding with nothing to composite; decision 47 emptied that set — a lone `png` is copied
/// through untouched and a lone `jpg` was already — and the signature now says so, rather than a
/// comment claiming an arm was unreached. An "X cannot happen" is a guarantee only when the compiler
/// rejects the counterexample.
///
/// An overlay whose dimensions differ from the main's is scaled to fit **within** the frame,
/// preserving its own aspect ratio, and centred — contain rather than fill. All 161 observed
/// pairs are same-aspect, and on a pair whose aspect matches exactly — every modal shape observed
/// — the scale rounds to the frame's own dimensions, so the composite is the fill composite; the
/// 10 at 1556x3264/1080x2265 differ in aspect by 0.022%, where contain leaves one unpainted row
/// rather than a sub-pixel stretch. Only a genuinely mismatched pair shows a real difference, and
/// there the caption is kept, never stretched or dropped (user pick 2026-08-04, agent call
/// contain-vs-skip). Alpha is composited, so the transparent parts of the overlay leave the main
/// showing through.
fn composite(main: &Path, overlay: &Path) -> Result<RgbaImage, OverlayError> {
    let mut base = decode(main)?;

    let drawn = decode(overlay)?;
    let drawn = if drawn.dimensions() == base.dimensions() {
        drawn
    } else {
        // Contain: scale to fit within the frame, preserving the overlay's aspect, then centre
        // it. On a same-aspect pair the scale caps both dimensions exactly on the main's (the
        // rounding error of one f64 divide-and-multiply is far under half a pixel), so those
        // pairs composite identically to the fill resize this replaced — the same-aspect
        // fixture must stay green. The `max(1.0)` keeps an extreme overlay aspect from
        // rounding a scaled dimension to zero.
        let scale = (base.width() as f64 / drawn.width() as f64).min(base.height() as f64 / drawn.height() as f64);
        let scaled_w = (drawn.width() as f64 * scale).round().max(1.0) as u32;
        let scaled_h = (drawn.height() as f64 * scale).round().max(1.0) as u32;
        imageops::resize(&drawn, scaled_w, scaled_h, FilterType::Lanczos3)
    };
    // The scaled layer fits within the base by construction, so these cannot underflow.
    let x = i64::from(base.width() - drawn.width()) / 2;
    let y = i64::from(base.height() - drawn.height()) / 2;
    imageops::overlay(&mut base, &drawn, x, y);
    Ok(base)
}

/// The pixel dimensions of an encoded image, read from its header rather than by decoding it.
///
/// Here rather than in [`crate::export::exif`] because this is the module that owns `image`; what
/// needs the answer is `ExifImageWidth`/`ExifImageHeight`, which have to describe the bytes that
/// actually get written and not the source they came from.
///
/// # Errors
///
/// Returns [`OverlayError::Decode`] when the bytes are not an image this build reads.
pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32), OverlayError> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|source| OverlayError::Decode { path: PathBuf::new(), source: ImageError::IoError(source) })?
        .into_dimensions()
        .map_err(|source| OverlayError::Decode { path: PathBuf::new(), source })
}

/// Reads and decodes one layer, with the format taken from the bytes rather than the extension.
///
/// RGBA throughout so an overlay's transparency survives to the composite; a main with no alpha
/// channel is widened to one, which costs memory and keeps both layers in one pixel type.
///
/// `with_guessed_format` reads the format from the BYTES rather than the extension, which is the
/// whole point here. The extension axis it does NOT close is Unicode normalization: two overlays
/// whose extensions differ only by NFC/NFD are held apart by the claim set's ascii fold and merged
/// by a folding filesystem. That is a separate question about the overlay's stored extension, not
/// about the out root, and [`crate::export::local_fix`] states it as a ceiling on `Originals`.
fn decode(path: &Path) -> Result<RgbaImage, OverlayError> {
    let reader = ImageReader::open(path)
        .map_err(|source| OverlayError::Decode { path: path.to_path_buf(), source: ImageError::IoError(source) })?
        .with_guessed_format()
        .map_err(|source| OverlayError::Decode { path: path.to_path_buf(), source: ImageError::IoError(source) })?;
    let decoded = reader.decode().map_err(|source| OverlayError::Decode { path: path.to_path_buf(), source })?;
    Ok(decoded.to_rgba8())
}

/// Something went wrong turning one memory's layers into an image.
#[derive(Debug)]
pub enum OverlayError {
    /// A layer could not be read or is not an image this build decodes.
    Decode { path: PathBuf, source: ImageError },
    /// The composite could not be encoded, in whichever format the plan chose for it.
    Encode { source: ImageError },
}

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode { path, source } if path.as_os_str().is_empty() => {
                write!(f, "could not read the composite's own dimensions back: {source}")
            }
            Self::Decode { path, source } => write!(
                f,
                "could not decode {}: {source}; this build reads jpeg, png and webp, so a memory in another format \
                 needs ffmpeg or another tool first",
                path.display()
            ),
            Self::Encode { source } => write!(f, "could not encode the composite: {source}"),
        }
    }
}

impl Error for OverlayError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Decode { source, .. } | Self::Encode { source } => Some(source),
        }
    }
}
