//! The run-completion footer alert, shared by every screen that drives a run (cloudy-tui skill:
//! Footer alert).
//!
//! Lives beside [`crate::tui::footer`] rather than inside one screen because three screens now raise
//! one, and the footer owns a single row: whichever screen the alert came from, it renders through
//! the same three fields. A copy per screen would be three spellings of one contract element, and
//! the footer would have to know all of them.
//!
//! **What a message may hold.** These strings reach the footer verbatim, so the privacy gate applies
//! here rather than at each call site: counts, verbs and a typed error's own `Display`. No
//! conversation key, no username, no message text, no coordinate. [`completion`] and
//! [`history_completion`] are built from counts alone for exactly that reason — a per-item detail
//! would have to name an item.

use crate::export::history::HtmlLinks;
use crate::export::history_run::HistoryReport;
use crate::export::local_fix::FixReport;
use crate::tui::format::plural;
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

/// What a finished run does to its tab's label while the user is on another tab (cloudy-tui:
/// Tab bar → Tab activity). The inactive label takes the semantic color until the tab is visited;
/// the footer alert stays behind on the screen it came from, to be read there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabActivity {
    /// A run that finished cleanly: the inactive label takes SUCCESS.
    Success,
    /// A run that failed somewhere, or could not start: the inactive label takes DANGER.
    Danger,
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

    /// The alert the history run raises: "run finished · N conversations · M documents", plus the
    /// placeholder-media note exactly once when the run wrote html over a source with no manifest
    /// to read (decision 62). The note names the html the run actually wrote: a run with no html
    /// selected over a no-manifest source has no links at all, and "placeholders" would misname a
    /// silence — [`HistoryReport::html_written`] keeps the two apart. The run writes only what the
    /// screen selected, so zero counts name the shape of the selection the way the completion copy
    /// names a resume. Always `Info`: the run's failures already have their own alert on the way
    /// here.
    #[must_use]
    pub fn history_completion(report: &HistoryReport) -> Self {
        let mut clauses: Vec<String> = Vec::new();
        for (count, one, many) in [(report.conversations, "conversation", "conversations"), (report.documents, "document", "documents")] {
            if count > 0 {
                clauses.push(format!("{count} {}", plural(count, one, many)));
            }
        }
        if report.html_written && report.links == HtmlLinks::NoManifest {
            clauses.push("media links are placeholders".to_owned());
        }
        let message = if clauses.is_empty() {
            format!("run finished {} nothing written", glyph::CLAUSE_SEPARATOR)
        } else {
            format!("run finished {} {}", glyph::CLAUSE_SEPARATOR, clauses.join(&format!(" {} ", glyph::CLAUSE_SEPARATOR)))
        };
        Self { kind: AlertKind::Info, message }
    }

    /// The alert a run that could not start, or whose state store broke, raises. `error` is a typed
    /// [`crate::export::memories_run::RunError`],
    /// [`crate::export::chat_run::RunError`] or
    /// [`crate::export::history_run::RunError`], whose `Display` is written to be read here.
    #[must_use]
    pub fn failure(error: &impl std::fmt::Display) -> Self {
        Self { kind: AlertKind::Warning, message: error.to_string() }
    }

    /// The tab-activity state this alert raises on a background tab (cloudy-tui: Tab bar → Tab
    /// activity). The two-way map is the two kinds the alert already keeps: a clean run is
    /// [`TabActivity::Success`], anything else — a failure, a run that could not start, a resume
    /// whose outputs land under a different root — is [`TabActivity::Danger`]. The contract's
    /// `WARNING` "needs attention" tier is a possible refinement, not a thing this channel
    /// distinguishes yet.
    #[must_use]
    pub const fn activity(&self) -> TabActivity {
        match self.kind {
            AlertKind::Info => TabActivity::Success,
            AlertKind::Warning => TabActivity::Danger,
        }
    }
}
