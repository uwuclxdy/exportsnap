//! The one thing this crate asks ffmpeg for: video pixels.
//!
//! Detection lives in [`crate::export::env`], not here. This module is the invocation and nothing
//! else, and it is deliberately the only place in the crate that spawns an external process.
//!
//! # Why the two jobs are one call
//!
//! Every memory video in the observed export is HEVC (`hvc1`), which Windows and older players
//! routinely will not decode, so a run that produces playable files has to transcode. 84 of the 545
//! also carry an overlay, and burning a caption into a video means re-encoding it. Since a
//! transcode is already re-encoding every frame, **the burn rides along at no extra cost** — which
//! is why there is no burn-only entry point here. A run that is not transcoding does not re-encode
//! video pixels at all: it copies the bytes and patches metadata, and reports the overlay it could
//! not draw.
//!
//! So: **ffmpeg is invoked if and only if the run is transcoding.** [`crate::export::local_fix`]
//! owns that decision; this module owns what happens after it.
//!
//! # Version floor
//!
//! The overlay path scales the caption layer to the frame with `scale2ref`, which is deprecated
//! from ffmpeg 7.1 in favour of `scale=rw:rh` but still present in 8.1.2, the build this project
//! verifies against. Deliberate: `scale=rw:rh` does not exist before 7.1, and ffmpeg 6.x is still
//! what several current distributions ship, so the deprecated spelling is the one that works on
//! the wider install base. Upgrade path is a straight swap once the floor moves past 7.1. Nothing
//! silently degrades in the meantime — a filter ffmpeg cannot build is a per-item failure carrying
//! ffmpeg's own message.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The encoder every output uses.
const VIDEO_CODEC: &str = "libx264";

/// H.264 in `yuv420p` is the combination with the fewest players that refuse it, which is the
/// entire point of transcoding away from HEVC. Stated rather than left to the encoder's own choice,
/// which follows the input and would keep a 10-bit or 4:2:2 source in a format half the players
/// that cannot decode HEVC also cannot decode.
const PIXEL_FORMAT: &str = "yuv420p";

/// Constant-rate-factor quality. 20 is visually lossless at the 720x1280 memory videos carry, and
/// the archive of one's own videos is the wrong place to save bytes.
const CRF: &str = "20";

/// How much of ffmpeg's stderr a failure quotes.
///
/// The message lands in the manifest's `last_error` beside a signed-url column, so it is capped
/// rather than pasted whole. `-loglevel error` already keeps it to a line or two.
const REPORTED_STDERR: usize = 400;

/// Re-encodes `main` to H.264 at `output`, drawing `overlay` over every frame when there is one.
///
/// Audio is copied rather than re-encoded: the memory videos carry AAC already and a second lossy
/// generation on the audio buys nothing. Video-only input is fine; the audio mapping is optional.
///
/// `output` is written by ffmpeg itself, so this is the one step of the pass that is not a single
/// `fs::write` of a finished buffer. The caller keeps the all-or-nothing property by pointing it at
/// a scratch path and writing the real output itself.
///
/// # Errors
///
/// Returns [`FfmpegError::Spawn`] when the binary cannot be started and [`FfmpegError::Failed`]
/// when it runs and exits non-zero, carrying what it said.
pub fn transcode(ffmpeg: &Path, main: &Path, overlay: Option<&Path>, output: &Path) -> Result<(), FfmpegError> {
    let result = Command::new(ffmpeg)
        .args(argv(main, overlay, output))
        .output()
        .map_err(|source| FfmpegError::Spawn { ffmpeg: ffmpeg.to_path_buf(), source })?;
    if result.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&result.stderr);
    let said = said.trim();
    Err(FfmpegError::Failed {
        main: main.to_path_buf(),
        said: said.char_indices().nth(REPORTED_STDERR).map_or_else(|| said.to_owned(), |(end, _)| said[..end].to_owned()),
    })
}

/// Everything after the binary name, split from the spawn so it can be asserted without running
/// anything.
///
/// **The `--` before `output` is defence in depth, and worth stating at its real strength rather
/// than as a fixed vulnerability.** The output is the one argument with no flag in front of it, so
/// a path spelled with a leading `-` is read as an option: measured on n8.1.2, a bare `-weird.mp4`
/// output exits **8** with `Unrecognized option 'weird.mp4'` and writes nothing, while the same run
/// behind `--` exits 0 and writes the file. What keeps that off a real run is
/// [`crate::export::manifest::Manifest::mark_done`], which stores absolute paths only — and an
/// absolute path cannot begin with `-` on either platform.
///
/// It is still worth the two bytes, because that guard sits **after** this call rather than before
/// it: `local_fix::fix` is public, transcodes first and records second, so a relative root gets all
/// the way here and the user would read `ffmpeg could not re-encode <file>` where the truth is
/// "the output root is relative". Cheaper to make the path a path than to explain the wrong error.
///
/// The two `-i` paths need no such guard: ffmpeg takes the argument after `-i` literally however it
/// is spelled — also measured, rather than assumed in either direction.
fn argv(main: &Path, overlay: Option<&Path>, output: &Path) -> Vec<OsString> {
    // `-nostdin` so a run inside a TUI cannot have its terminal read out from under it, and
    // `-loglevel error` so the captured stderr is the failure rather than a progress table.
    let mut argv: Vec<OsString> = flags(&["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"]);
    argv.push(main.into());

    match overlay {
        Some(overlay) => {
            // The caption layer is scaled to the frame rather than refused when the two disagree,
            // matching what the image leg does with the same pair of files. `scale2ref` reads the
            // size off the decoded frame, so a rotated video stays right where a dimension copied
            // out of the container's own header would not.
            argv.push("-i".into());
            argv.push(overlay.into());
            argv.extend(flags(&[
                "-filter_complex",
                "[1:v][0:v]scale2ref=w=iw:h=ih[caption][frame];[frame][caption]overlay=0:0:format=auto[burned]",
                "-map",
                "[burned]",
            ]));
        }
        None => argv.extend(flags(&["-map", "0:v:0"])),
    }

    // `?` on the audio: a memory video always has a track, a fixture need not.
    argv.extend(flags(&["-map", "0:a:0?"]));
    argv.extend(flags(&["-c:v", VIDEO_CODEC, "-preset", "medium", "-crf", CRF, "-pix_fmt", PIXEL_FORMAT]));
    argv.extend(flags(&["-c:a", "copy"]));
    // Puts the movie box in front of the media data, so a player can start without reading to the
    // end. Free here, since the file is being written from scratch anyway.
    argv.extend(flags(&["-movflags", "+faststart"]));
    // Everything past here is a path, never an option. See the note above.
    argv.push("--".into());
    argv.push(output.into());
    argv
}

/// Literal arguments, which are always valid UTF-8 because they are spelled in this file. Paths go
/// in with `OsString::from` directly instead, so a name this crate did not choose is never forced
/// through a lossy conversion.
fn flags(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// ffmpeg could not be run, or ran and refused the file.
#[derive(Debug)]
pub enum FfmpegError {
    /// The binary [`crate::export::env::locate`] found could not be started. Distinct from a
    /// missing ffmpeg, which is not an error at all: a run without one degrades to copying bytes.
    Spawn { ffmpeg: PathBuf, source: io::Error },
    /// It ran and exited non-zero.
    Failed {
        main: PathBuf,
        /// What it wrote to stderr, capped. Empty when it said nothing at all.
        said: String,
    },
}

impl fmt::Display for FfmpegError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { ffmpeg, source } => write!(
                f,
                "could not run {}: {source}; it was found on PATH, so check it is executable or turn transcoding off",
                ffmpeg.display()
            ),
            Self::Failed { main, said } if said.is_empty() => {
                write!(f, "ffmpeg refused {} and said nothing about why", main.display())
            }
            Self::Failed { main, said } => write!(f, "ffmpeg could not re-encode {}: {said}", main.display()),
        }
    }
}

impl Error for FfmpegError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            Self::Failed { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::argv;

    /// The argument vector as plain strings, for readable assertions. Every argument this crate
    /// builds is valid UTF-8; a path that is not would fail here rather than silently compare equal.
    fn spelled(main: &str, overlay: Option<&str>, output: &str) -> Vec<String> {
        argv(Path::new(main), overlay.map(Path::new), Path::new(output))
            .iter()
            .map(|arg| arg.to_str().map(str::to_owned).unwrap_or_else(|| panic!("non-utf8 argument {arg:?}")))
            .collect()
    }

    #[test]
    fn the_output_path_is_always_behind_an_end_of_options_terminator() {
        // The output is the one argument with no flag in front of it, so an output root spelled
        // with a leading `-` is otherwise parsed as an option: measured on ffmpeg n8.1.2 as exit 8,
        // `Unrecognized option`, and no file written. The manifest's absolute-path rule keeps that
        // off a real run, but it runs AFTER the transcode, so without this the user reads an ffmpeg
        // decode error where the truth is a relative output root. Pinned here rather than end to
        // end on purpose: reaching it through the pipeline needs a relative path, and a test that
        // changed the process working directory would plant a race across the whole binary.
        let args = spelled("/in/a.mp4", None, "-weird/out/x.mp4");
        let terminator = args.iter().position(|arg| arg == "--").expect("no -- in the argument vector");
        assert_eq!(args[terminator + 1], "-weird/out/x.mp4", "the output must be the argument straight after the terminator");
        assert_eq!(terminator + 2, args.len(), "nothing may follow the output, or it would be read as one more option");
    }

    #[test]
    fn both_inputs_are_named_by_their_own_flag_rather_than_by_position() {
        let args = spelled("/in/main.mp4", Some("/in/over.png"), "/out/x.mp4");
        let inputs: Vec<&String> = args.iter().enumerate().filter(|(at, _)| *at > 0 && args[at - 1] == "-i").map(|(_, arg)| arg).collect();
        assert_eq!(inputs, ["/in/main.mp4", "/in/over.png"], "{args:?}");
        // Order is load-bearing: the filter graph names the main `[0:v]` and the overlay `[1:v]`,
        // so swapping the two inputs would composite the video onto the caption.
        let graph = args.iter().find(|arg| arg.contains("scale2ref")).expect("no filter graph");
        assert!(graph.starts_with("[1:v][0:v]scale2ref"), "{graph}");
    }

    /// Whether `flag` is followed immediately by `value`.
    fn carries(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|window| window[0] == flag && window[1] == value)
    }

    #[test]
    fn a_run_with_no_overlay_builds_no_filter_graph_at_all() {
        let args = spelled("/in/main.mp4", None, "/out/x.mp4");
        assert!(!args.iter().any(|arg| arg == "-filter_complex"), "{args:?}");
        // The video stream is mapped straight from the input instead, and the audio stays optional
        // so a video-only source is transcoded rather than refused.
        assert!(carries(&args, "-map", "0:v:0"), "{args:?}");
        assert!(carries(&args, "-map", "0:a:0?"), "{args:?}");
    }

    #[test]
    fn the_encode_is_pinned_to_what_the_transcode_exists_to_produce() {
        // The whole point is moving off HEVC onto something Windows and older players decode, so
        // these four are the contract rather than tuning knobs a future edit may drift.
        let args = spelled("/in/main.mp4", None, "/out/x.mp4");
        for (flag, value) in [("-c:v", "libx264"), ("-pix_fmt", "yuv420p"), ("-c:a", "copy"), ("-movflags", "+faststart")] {
            assert!(carries(&args, flag, value), "{flag} {value} missing from {args:?}");
        }
    }
}
