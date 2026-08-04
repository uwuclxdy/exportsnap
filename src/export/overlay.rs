//! Drawing a memory's overlay layer back over the media it belongs to.
//!
//! Snapchat ships captions and stickers as a separate transparent image beside the media rather
//! than burned into it, so a downloaded memory is missing everything that was drawn on it. The
//! two files share a uuid and [`crate::export::memories`] has already paired them; this module is
//! the pixels.
//!
//! Everything here produces **JPEG bytes**, whatever went in. That is not an aesthetic choice: it
//! is what keeps a PNG out of `little_exif`, where RUSTSEC-2026-0194 lives (see
//! [`crate::export::exif`]). The 162 `.png` files in the observed export are all overlays, and an
//! overlay is a layer in a composite here, never a file that gets stamped.

use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::{self, FilterType};
use image::{ImageError, ImageReader, RgbaImage};

/// How much the re-encode is allowed to cost.
///
/// Only the composite path pays it — a main with no overlay is copied byte for byte and never
/// reaches this module. 95 is high enough that the re-encode is not the lossy step next to
/// whatever compression Snapchat already applied; the cost is roughly a third more bytes than the
/// crate's default 75, which for an archive of one's own photos is the right side of that trade.
const JPEG_QUALITY: u8 = 95;

/// `main` with `overlay` drawn over it, encoded as JPEG bytes.
///
/// `overlay` is `None` for a main that needs re-encoding but has nothing to composite — a main
/// this build can decode but that is not already a JPEG.
///
/// An overlay whose dimensions differ from the main's is scaled to fit rather than refused: both
/// are full-frame captures of the same moment, and an overlay one pixel off would otherwise cost
/// the caption entirely. Alpha is composited, so the transparent parts of the overlay leave the
/// main showing through.
///
/// # Errors
///
/// Returns [`OverlayError`] when either file cannot be read or decoded, or when the composite
/// cannot be encoded.
pub fn compose(main: &Path, overlay: Option<&Path>) -> Result<Vec<u8>, OverlayError> {
    let mut base = decode(main)?;

    if let Some(overlay) = overlay {
        let drawn = decode(overlay)?;
        let drawn = if drawn.dimensions() == base.dimensions() {
            drawn
        } else {
            imageops::resize(&drawn, base.width(), base.height(), FilterType::Lanczos3)
        };
        imageops::overlay(&mut base, &drawn, 0, 0);
    }

    // JPEG carries no alpha channel, so the composite is flattened before encoding. Anything the
    // overlay left transparent already shows the main through it.
    let flattened = image::DynamicImage::ImageRgba8(base).to_rgb8();
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY).encode_image(&flattened).map_err(|source| OverlayError::Encode { source })?;
    Ok(bytes)
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
    /// The composite could not be encoded as a JPEG.
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
                "could not decode {}: {source}; this build reads jpeg and png, so a memory in another format needs \
                 ffmpeg or another tool first",
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
