//! The run-completion footer alert, shared by every screen that drives a run (cloudy-tui skill:
//! Footer alert).
//!
//! Lives beside [`crate::tui::footer`] rather than inside one screen because two screens now raise
//! one, and the footer owns a single row: whichever screen the alert came from, it renders through
//! the same three fields. A copy per screen would be two spellings of one contract element, and the
//! footer would have to know both.
//!
//! **What a message may hold.** These strings reach the footer verbatim, so the privacy gate applies
//! here rather than at each call site: counts, verbs and a typed error's own `Display`. No
//! conversation key, no username, no message text, no coordinate. [`completion`] is built from
//! [`FixReport`]'s integers alone for exactly that reason — a per-item detail would have to name an
//! item.

use crate::export::local_fix::FixReport;
use crate::tui::theme::glyph;

/// The run-completion footer alert. Dismissed only by `x`; a new run resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAlert {
    pub kind: AlertKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    /// A run that finished with no failures.
    Info,
    /// A run that failed somewhere, or could not start.
    Warning,
}

impl RunAlert {
    /// The alert a finished run raises: a clean completion is `INFO`, anything with a failure is
    /// `WARNING`.
    ///
    /// Zero counts are hidden (Patterns → Counts and plurals): a clean resume reads "5 skipped",
    /// never "0 fixed". A run that fixed, failed, skipped, deferred and excluded nothing at all had
    /// an empty plan, which the copy says outright rather than leaving a bare "run finished · ".
    #[must_use]
    pub fn completion(report: &FixReport) -> Self {
        let mut clauses = Vec::new();
        for (count, word) in [
            (report.fixed, "fixed"),
            (report.failed.len(), "failed"),
            (report.skipped, "skipped"),
            (report.deferred, "deferred"),
            (report.excluded, "dropped"),
        ] {
            if count > 0 {
                clauses.push(format!("{count} {word}"));
            }
        }
        let message = if clauses.is_empty() {
            format!("run finished {} nothing to fix", glyph::CLAUSE_SEPARATOR)
        } else {
            format!("run finished {} {}", glyph::CLAUSE_SEPARATOR, clauses.join(&format!(" {} ", glyph::CLAUSE_SEPARATOR)))
        };
        Self { kind: if report.failed.is_empty() { AlertKind::Info } else { AlertKind::Warning }, message }
    }

    /// The alert a run that could not start, or whose state store broke, raises. `error` is a typed
    /// [`crate::export::memories_run::RunError`] or
    /// [`crate::export::chat_run::RunError`], whose `Display` is written to be read here.
    #[must_use]
    pub fn failure(error: &impl std::fmt::Display) -> Self {
        Self { kind: AlertKind::Warning, message: error.to_string() }
    }
}
