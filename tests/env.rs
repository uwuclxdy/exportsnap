//! Public-API tests for `exportsnap::export::env`: tool detection and the free-space probe.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use exportsnap::export::env::{Environment, Tool, available_space, locate_in};

/// A path under cargo's own test tmpdir that is guaranteed not to exist.
fn missing_path() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("env-no-such-dir").join("nor-this-one")
}

#[test]
fn every_tool_names_the_binary_it_looks_for() {
    assert_eq!(Tool::ALL.map(Tool::command), ["ffmpeg", "vlc"]);
}

#[test]
fn available_space_reports_a_real_filesystem() {
    // The crate's own dir is on a mounted filesystem with room on it, or this checkout could not
    // have been built.
    let space = available_space(env!("CARGO_MANIFEST_DIR")).unwrap();
    assert!(space > 0, "the checkout's filesystem reported {space} available bytes");
}

#[test]
fn a_space_probe_that_fails_names_the_path_and_the_fix() {
    let path = missing_path();
    let error = available_space(&path).unwrap_err();

    assert_eq!(error.path, path);
    assert_eq!(
        error.to_string(),
        format!(
            "could not measure free space on the filesystem holding {}: No such file or directory (os error 2); \
             check the path exists and is readable",
            path.display()
        )
    );
}

#[test]
fn a_probe_measures_the_filesystem_the_path_sits_on() {
    let probe = Environment::probe(env!("CARGO_MANIFEST_DIR"));
    assert!(probe.available_space.is_some_and(|space| space > 0));
}

#[test]
fn a_space_probe_that_fails_reports_no_figure_rather_than_a_wrong_one() {
    // The tool fields are deliberately NOT asserted here: any comparison against `locate` is
    // `f(x) == f(x)` in a thin disguise and cannot fail. The field-to-tool wiring is pinned by
    // `each_field_holds_the_tool_it_is_named_after` (inline in `src/export/env.rs`, over an injected
    // locator) and `locate` itself by `locate_in_finds_each_tool_under_the_name_it_declares`.
    assert_eq!(Environment::probe(missing_path()).available_space, None);
}

/// Unix-only because it needs the executable bit, and `which` will not return a file without it.
/// The name wiring it pins is platform-independent, so covering it on one platform is enough.
#[cfg(unix)]
#[test]
fn locate_in_finds_each_tool_under_the_name_it_declares() {
    use std::os::unix::fs::PermissionsExt;

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("env-fake-tools");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    for name in ["ffmpeg", "vlc", "unrelated"] {
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Each tool resolves to the file named after its own `command()`, so swapping the two in
    // `locate` reds here rather than on whichever machine happens to have only one installed.
    assert_eq!(locate_in(Tool::Ffmpeg, &dir), Some(dir.join("ffmpeg")));
    assert_eq!(locate_in(Tool::Vlc, &dir), Some(dir.join("vlc")));

    // A dir holding neither name yields nothing, so the two above are real lookups and not an
    // artefact of `which` returning the first executable it sees.
    let empty = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("env-fake-tools-empty");
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    assert_eq!(locate_in(Tool::Ffmpeg, &empty), None);
    assert_eq!(locate_in(Tool::Vlc, &empty), None);
}

#[test]
fn tool_reads_back_the_field_that_holds_it() {
    let environment = Environment { ffmpeg: Some(PathBuf::from("/usr/bin/ffmpeg")), vlc: None, available_space: Some(1) };

    assert_eq!(environment.tool(Tool::Ffmpeg), Some(PathBuf::from("/usr/bin/ffmpeg").as_path()));
    assert_eq!(environment.tool(Tool::Vlc), None);
}

#[test]
fn a_default_environment_found_nothing_and_measured_nothing() {
    let environment = Environment::default();

    assert_eq!(environment.tool(Tool::Ffmpeg), None);
    assert_eq!(environment.tool(Tool::Vlc), None);
    assert_eq!(environment.available_space, None);
}
