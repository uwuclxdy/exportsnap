//! exportsnap library surface. `src/main.rs` wires this into the running app; tests under
//! `tests/` exercise it as an external crate.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod tui;
