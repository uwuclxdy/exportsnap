//! One module per tab (`docs/design.md`, TUI screen map). A screen owns its own panels; the
//! [`crate::tui::shell`] owns the frame around them.

pub mod chat_media;
pub mod history;
pub mod memories;
pub mod overview;
