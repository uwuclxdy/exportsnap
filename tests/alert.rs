//! The run-alert public surface: the footer alert's kind and the tab-activity channel's mapping
//! (cloudy-tui skill: Footer alert, Tab bar → Tab activity).

use exportsnap::export::local_fix::{Failure, FixReport};
use exportsnap::export::manifest::ResumeReport;
use exportsnap::tui::alert::{AlertKind, RunAlert, TabActivity};

/// The tab-activity map distinguishes a clean run, a run that finished but left something to
/// notice, and a genuine failure. The `Warning` arm is the tier that was unreachable while
/// `TabActivity` had only `Success` and `Danger`, and it must not fold into `Danger` — a
/// skipped-elsewhere resume is worth a look, not a failure.
#[test]
fn the_tab_activity_map_distinguishes_attention_from_failure() {
    let clean = RunAlert { kind: AlertKind::Info, message: String::new() };
    let noticed = RunAlert { kind: AlertKind::Warning, message: String::new() };
    let failed = RunAlert { kind: AlertKind::Danger, message: String::new() };

    assert_eq!(clean.activity(), TabActivity::Success);
    assert_eq!(noticed.activity(), TabActivity::Warning);
    assert_eq!(failed.activity(), TabActivity::Danger);
}

/// `completion` reads `failed.len()` and assigns the kind from it alone: a clean report is
/// `Info`, any failure is `Danger`. `failure` — a run that could not start — is `Danger` too.
#[test]
fn completion_and_failure_assign_the_kind() {
    let clean = FixReport {
        resumed: ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0, retired: 0, excluded: 0 },
        fixed: 1,
        failed: vec![],
        skipped: 0,
        deferred: 0,
        excluded: 0,
        notices: vec![],
    };
    assert_eq!(RunAlert::completion(&clean).kind, AlertKind::Info);

    let failed = FixReport {
        resumed: ResumeReport { demoted: vec![], verified: 0, pending: 0, failed: 0, source_missing: 0, retired: 0, excluded: 0 },
        fixed: 0,
        failed: vec![Failure { source_id: "x".into(), reason: "boom".into() }],
        skipped: 0,
        deferred: 0,
        excluded: 0,
        notices: vec![],
    };
    assert_eq!(RunAlert::completion(&failed).kind, AlertKind::Danger);

    assert_eq!(RunAlert::failure(&"boom".to_string()).kind, AlertKind::Danger);
}

/// `note_attention` promotes a clean run to `Warning` but never demotes a genuine failure: a run
/// that both failed items and skipped finished items to a different out dir stays `Danger`
/// (severity precedence, Danger over Warning).
#[test]
fn note_attention_promotes_a_clean_run_but_never_demotes_a_failure() {
    let mut clean = RunAlert { kind: AlertKind::Info, message: String::new() };
    clean.note_attention();
    assert_eq!(clean.kind, AlertKind::Warning);

    let mut failed = RunAlert { kind: AlertKind::Danger, message: String::new() };
    failed.note_attention();
    assert_eq!(failed.kind, AlertKind::Danger);
}
