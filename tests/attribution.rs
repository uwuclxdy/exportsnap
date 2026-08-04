//! `THIRD-PARTY-LICENSES` is the shipped side of the ODbL attribution decision:
//! the binary embeds OpenStreetMap-derived timezone polygons (via `tzf-dist`),
//! and this file is the red flag if the generated notice is ever dropped from
//! the repo. Phase 5's release workflow must package `THIRD-PARTY-LICENSES`
//! alongside the binary; this test is the durable half of that obligation — if
//! the file is deleted, it reds here, on every gate run, before a release is
//! even staged.
//!
//! The file is generated, never hand-edited: from the repo root, run
//!
//!     cargo about generate about.hbs -o THIRD-PARTY-LICENSES
//!
//! with the committed `about.toml` + `about.hbs` as inputs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

/// The repo root; `THIRD-PARTY-LICENSES` sits there, beside `about.toml` and `about.hbs`.
fn notice_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("THIRD-PARTY-LICENSES")
}

#[test]
fn third_party_licenses_exists_at_the_repo_root() {
    let path = notice_path();
    assert!(path.is_file(), "{} is missing; regenerate it with `cargo about generate about.hbs -o THIRD-PARTY-LICENSES`", path.display());
}

#[test]
fn third_party_licenses_names_osm_odbl_and_tzf_dist() {
    let text = fs::read_to_string(notice_path()).unwrap();
    for needle in ["OpenStreetMap", "ODbL-1.0", "tzf-dist", "https://opendatacommons.org/licenses/odbl/1-0/"] {
        assert!(text.contains(needle), "THIRD-PARTY-LICENSES lacks '{needle}'");
    }
}

#[test]
fn third_party_licenses_links_the_odbl_text_instead_of_shipping_it() {
    // Decision 38: the full ODbL text does not ship; the file links it. If the
    // template conditional is ever removed, cargo-about pastes the canonical
    // text and the "not reproduced here" marker disappears with it.
    let text = fs::read_to_string(notice_path()).unwrap();
    assert!(text.contains("The full ODbL-1.0 text is not reproduced here; it is at"), "the ODbL section must link the text, not paste it");
}
