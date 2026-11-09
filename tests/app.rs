//! Event→state transitions for the app shell. Pure: no terminal backend is involved, the
//! same handler the event loop calls is fed synthetic crossterm events.
//!
//! Every expectation is cross-checked against the cloudy-tui skill's Keyboard grammar and
//! Tab bar → Switching tabs sections, not against this crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use exportsnap::app::{App, RunDefaults, Tab};
use exportsnap::config::Config;
use exportsnap::tui::screens::settings::SettingsLayers;
use exportsnap::tui::theme::Tier;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode};

fn app() -> App {
    App::new(Tier::Full)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn press_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_event(&Event::Key(KeyEvent::new(code, modifiers)));
}

fn jump(app: &mut App, digit: char) {
    press_with(app, KeyCode::Char(digit), KeyModifiers::ALT);
}

// ---- initial state ----

#[test]
fn starts_running_on_the_first_tab_with_quit_disarmed() {
    let app = app();
    assert_eq!(app.active(), Tab::Overview);
    assert!(app.is_running());
    assert!(!app.is_quit_armed());
}

#[test]
fn tab_order_matches_the_design_screen_map() {
    assert_eq!(Tab::ALL, [Tab::Overview, Tab::Memories, Tab::ChatMedia, Tab::History, Tab::Account, Tab::Settings,]);
    assert_eq!(Tab::ALL.map(Tab::label), ["overview", "memories", "chat media", "history", "account", "settings",]);

    // Second witness; `Tab::label`/`Tab::index` (src/app.rs) are the first. Survives either being
    // weakened to a wildcard. Residual and rationale: `MissingReason::ALL`, src/export/memories.rs.
    // Never collapse to `_ => {}`.
    for tab in Tab::ALL {
        match tab {
            Tab::Overview | Tab::Memories | Tab::ChatMedia | Tab::History | Tab::Account | Tab::Settings => {}
        }
    }
}

// ---- arrow navigation (skill: Keyboard grammar — `←`/`→` switch screens) ----

#[test]
fn right_arrow_walks_forward_through_every_tab() {
    let mut app = app();
    for expected in [Tab::Memories, Tab::ChatMedia, Tab::History, Tab::Account, Tab::Settings] {
        press(&mut app, KeyCode::Right);
        assert_eq!(app.active(), expected);
    }
}

#[test]
fn right_arrow_wraps_from_the_last_tab_to_the_first() {
    let mut app = app();
    jump(&mut app, '6');
    assert_eq!(app.active(), Tab::Settings);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.active(), Tab::Overview);
}

#[test]
fn left_arrow_wraps_from_the_first_tab_to_the_last() {
    let mut app = app();
    press(&mut app, KeyCode::Left);
    assert_eq!(app.active(), Tab::Settings);
    press(&mut app, KeyCode::Left);
    assert_eq!(app.active(), Tab::Account);
}

// ---- `⌥<digit>` jump (skill: Tab bar → Switching tabs) ----

#[test]
fn alt_digits_one_through_six_jump_positionally() {
    let mut app = app();
    for (digit, expected) in
        [('1', Tab::Overview), ('2', Tab::Memories), ('3', Tab::ChatMedia), ('4', Tab::History), ('5', Tab::Account), ('6', Tab::Settings)]
    {
        // Park somewhere else first so each jump has to move, rather than passing because the
        // tab happened to already be active.
        jump(&mut app, if expected == Tab::History { '1' } else { '4' });
        assert_ne!(app.active(), expected);
        jump(&mut app, digit);
        assert_eq!(app.active(), expected);
    }
}

#[test]
fn alt_nine_lands_on_the_last_tab() {
    let mut app = app();
    jump(&mut app, '9');
    assert_eq!(app.active(), Tab::Settings);
}

#[test]
fn alt_digits_past_the_tab_count_are_inert() {
    // Only six tabs exist, so `⌥7` and `⌥8` address nothing and must leave the active tab
    // alone rather than clamping to the last one (`⌥9` is the binding for that).
    for digit in ['7', '8'] {
        let mut app = app();
        jump(&mut app, '3');
        jump(&mut app, digit);
        assert_eq!(app.active(), Tab::ChatMedia);
    }
}

#[test]
fn alt_zero_is_unbound() {
    let mut app = app();
    jump(&mut app, '3');
    jump(&mut app, '0');
    assert_eq!(app.active(), Tab::ChatMedia);
}

#[test]
fn bare_digits_never_switch_tabs() {
    // Unmodified digits stay free for app use; only the `⌥`-modified form jumps.
    let mut app = app();
    press(&mut app, KeyCode::Char('4'));
    assert_eq!(app.active(), Tab::Overview);
}

#[test]
fn every_tab_carries_its_jump_index() {
    // Six tabs: positional `1`–`6`, none past the eighth, so no tab renders bare and `settings`
    // is indexed by its positional digit even though `⌥9` also lands on it. The overlay reads this
    // same mapping rather than a second spelling of it.
    assert_eq!(Tab::ALL.map(Tab::jump_index), [Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)]);
}

// ---- 2-step quit (skill: Keyboard grammar — `q` arms, never quits in one press) ----

#[test]
fn first_q_arms_the_quit_and_the_second_confirms_it() {
    let mut app = app();
    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed());
    assert!(app.is_running(), "one press must never quit");

    press(&mut app, KeyCode::Char('q'));
    assert!(!app.is_running());
}

#[test]
fn shifted_q_arms_and_confirms_the_quit_too() {
    // Hotkeys are case-insensitive. With caps lock on, crossterm reports `Char('Q')` carrying
    // SHIFT; if that missed the binding the user would have no way out but ctrl+c.
    let mut app = app();
    press_with(&mut app, KeyCode::Char('Q'), KeyModifiers::SHIFT);
    assert!(app.is_quit_armed());
    assert!(app.is_running());

    press_with(&mut app, KeyCode::Char('Q'), KeyModifiers::SHIFT);
    assert!(!app.is_running());
}

#[test]
fn a_shifted_q_confirms_a_quit_armed_by_a_lowercase_q() {
    // The two cases must be the same binding, not two independent arming states.
    let mut app = app();
    press(&mut app, KeyCode::Char('q'));
    press_with(&mut app, KeyCode::Char('Q'), KeyModifiers::SHIFT);
    assert!(!app.is_running());
}

#[test]
fn ctrl_q_is_not_the_quit_binding() {
    // Only SHIFT is masked off; any other modifier falls through and disarms like a stray key.
    let mut app = app();
    press_with(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(!app.is_quit_armed());
    assert!(app.is_running());
}

#[test]
fn any_other_key_disarms_the_quit() {
    for disarming in [KeyCode::Right, KeyCode::Left, KeyCode::Char('z'), KeyCode::Esc] {
        let mut app = app();
        press(&mut app, KeyCode::Char('q'));
        assert!(app.is_quit_armed());

        press(&mut app, disarming);
        assert!(!app.is_quit_armed(), "{disarming:?} should disarm");
        assert!(app.is_running());
    }
}

#[test]
fn a_tab_jump_disarms_the_quit() {
    let mut app = app();
    press(&mut app, KeyCode::Char('q'));
    jump(&mut app, '5');
    assert!(!app.is_quit_armed());
    assert_eq!(app.active(), Tab::Account);
    assert!(app.is_running());
}

#[test]
fn disarming_then_pressing_q_again_only_rearms() {
    let mut app = app();
    press(&mut app, KeyCode::Char('q'));
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed());
    assert!(app.is_running());
}

// ---- ctrl+c (skill: Keyboard grammar — quit from anywhere in two presses) ----

#[test]
fn ctrl_c_quits_in_two_presses() {
    // Also the shape legacy-encoded `ctrl+shift+c` arrives in — the terminal sends the bare byte
    // 0x03 for both — so the legacy shape must still reach the quit binding however the copy
    // chord is guarded below.
    let mut app = app();
    press_with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(app.is_running(), "the first press only arms");
    assert!(app.is_quit_armed());
    press_with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(!app.is_running());
}

#[test]
fn ctrl_c_still_quits_with_extra_modifiers_held() {
    // "quit from anywhere" — the binding tests for CONTROL rather than equality, so a stray alt
    // or super held alongside must not swallow the one guaranteed way out. SHIFT is the one
    // exception, carved out below.
    let mut app = app();
    press_with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL | KeyModifiers::ALT);
    assert!(app.is_running(), "the first press only arms");
    press_with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL | KeyModifiers::ALT);
    assert!(!app.is_running());
}

#[test]
fn bare_c_does_not_quit() {
    let mut app = app();
    press(&mut app, KeyCode::Char('c'));
    assert!(app.is_running());
}

// The terminal's own copy chord reaches crossterm in three shapes, each read off
// `crossterm-0.29.0/src/event/sys/unix/parse.rs` and `.../windows/parse.rs`. One test per shape,
// because the guard rejects each of them on a different clause.

#[test]
fn ctrl_shift_c_leaves_the_app_running() {
    // Kitty protocol, `disambiguate escape codes` only: `CSI 99;6u` is the base codepoint `c` with
    // mask 6, and `parse_modifiers` turns that into CONTROL|SHIFT. Not an uppercased `Char('C')` —
    // the SHIFT rejection is what carries this one.
    let mut app = app();
    press_with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert!(app.is_running(), "the copy chord must never quit");
    assert_eq!(app.active(), Tab::Overview, "it must not move the tab either");
}

#[test]
fn ctrl_shift_c_with_alternate_keys_reported_leaves_the_app_running() {
    // Kitty protocol with `report alternate keys` on: `CSI 99:67;6u` carries the shifted codepoint
    // too, and `parse_csi_u_encoded_key_code` substitutes it and then *clears* SHIFT — so this
    // shape arrives as `Char('C')` + CONTROL alone. Only the case check rejects it; widen the
    // guard to `'c' | 'C'` and the copy chord quits with the SHIFT rejection fully intact.
    //
    // The same `KeyEvent` is what Windows produces for plain `ctrl+c` with caps lock on, so this
    // pins that as not-quitting too. Deliberate, not collateral — the guard's comment carries the
    // reasoning and `q q` is that user's way out.
    let mut app = app();
    press_with(&mut app, KeyCode::Char('C'), KeyModifiers::CONTROL);
    assert!(app.is_running(), "the copy chord must never quit");
    assert_eq!(app.active(), Tab::Overview);
}

#[test]
fn ctrl_shift_c_in_the_windows_shape_leaves_the_app_running() {
    // The Windows console reports the char uppercased *and* keeps SHIFT in the modifier set, so
    // this shape is the one both clauses catch. Pinned so neither can be dropped unnoticed.
    let mut app = app();
    press_with(&mut app, KeyCode::Char('C'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert!(app.is_running(), "the copy chord must never quit, whichever case it arrives in");
    assert_eq!(app.active(), Tab::Overview);
}

#[test]
fn ctrl_shift_c_does_not_confirm_an_armed_quit() {
    // It falls through to the disarm, so an armed `q` must not be confirmed by the copy chord.
    let mut app = app();
    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed());
    press_with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert!(app.is_running());
    assert!(!app.is_quit_armed());
}

// ---- event filtering ----

#[test]
fn key_release_events_are_ignored() {
    // crossterm reports Press and Release on Windows; without the `kind` filter every binding
    // would fire twice there, so a release must move nothing.
    let mut app = app();
    let mut release = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    app.handle_event(&Event::Key(release));
    assert_eq!(app.active(), Tab::Overview);

    let mut release_quit = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    release_quit.kind = KeyEventKind::Release;
    app.handle_event(&Event::Key(release_quit));
    assert!(!app.is_quit_armed());
}

#[test]
fn a_repeat_key_switches_tabs_like_a_press() {
    // With the kitty protocol's REPORT_EVENT_TYPES pushed, an auto-repeated key arrives as
    // `KeyEventKind::Repeat` rather than as a fresh `Press`. Holding ←/→ must still cycle tabs
    // (and ↑/↓ still scroll), so a repeat is forwarded like a press; only Release is dropped.
    let mut app = app();
    let mut repeat = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    repeat.kind = KeyEventKind::Repeat;
    app.handle_event(&Event::Key(repeat));
    assert_eq!(app.active(), Tab::Memories);
}

#[test]
fn a_resize_changes_no_state() {
    let mut app = app();
    press(&mut app, KeyCode::Char('q'));
    app.handle_event(&Event::Resize(20, 5));
    assert!(app.is_quit_armed(), "a resize is not a key press");
    assert_eq!(app.active(), Tab::Overview);
    assert!(app.is_running());
}

// ---- `⌥` hold tracking (skill: Tab bar → Jump-key overlay) ----

fn alt_press(app: &mut App) {
    app.handle_event(&Event::Key(KeyEvent::new(KeyCode::Modifier(ModifierKeyCode::LeftAlt), KeyModifiers::ALT)));
}

fn alt_release(app: &mut App) {
    let mut release = KeyEvent::new(KeyCode::Modifier(ModifierKeyCode::LeftAlt), KeyModifiers::ALT);
    release.kind = KeyEventKind::Release;
    app.handle_event(&Event::Key(release));
}

#[test]
fn alt_hold_is_tracked_until_release() {
    let mut app = app();
    assert!(!app.alt_held());
    alt_press(&mut app);
    assert!(app.alt_held());
    alt_release(&mut app);
    assert!(!app.alt_held());
}

#[test]
fn both_alt_keys_track_the_hold() {
    // Left and right alt are two distinct modifier keycodes; either must hold the overlay and its
    // release must clear it.
    for modifier in [ModifierKeyCode::LeftAlt, ModifierKeyCode::RightAlt] {
        let mut app = app();
        app.handle_event(&Event::Key(KeyEvent::new(KeyCode::Modifier(modifier), KeyModifiers::ALT)));
        assert!(app.alt_held(), "{modifier:?} press holds");

        let mut release = KeyEvent::new(KeyCode::Modifier(modifier), KeyModifiers::ALT);
        release.kind = KeyEventKind::Release;
        app.handle_event(&Event::Key(release));
        assert!(!app.alt_held(), "{modifier:?} release clears");
    }
}

#[test]
fn a_bare_alt_press_is_not_an_app_key() {
    // A modifier hold is state, not a key: it must neither disarm an armed quit nor reach the tab
    // switcher.
    let mut app = app();
    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed());

    alt_press(&mut app);
    assert!(app.is_quit_armed(), "a bare alt press must not disarm the quit");
    assert_eq!(app.active(), Tab::Overview, "a bare alt press must not switch tabs");

    alt_release(&mut app);
    assert!(app.is_quit_armed(), "a bare alt release must not disarm the quit");
}

#[test]
fn a_latched_hold_self_heals_on_the_next_press_release_pair() {
    // A release can be missed if focus is stolen while `⌥` is held, leaving the overlay up. There
    // is deliberately no `FocusLost` clearing (crossterm emits that event only when focus
    // reporting is enabled, which this app never arms), so the latch persists until the next
    // press/release pair — the press re-asserts the hold, the release clears it.
    let mut app = app();
    alt_press(&mut app);
    assert!(app.alt_held());
    // Simulate the missed release: the hold is still latched.
    alt_press(&mut app);
    assert!(app.alt_held());
    alt_release(&mut app);
    assert!(!app.alt_held());
}

// ---- action menu + help modal (skill: Action menu; Help modal; Keyboard grammar `a`/`?`) ----

#[test]
fn question_mark_opens_the_help_modal_and_q_closes_it() {
    let mut app = app();
    press(&mut app, KeyCode::Char('?'));
    assert!(matches!(app.modal(), Some(exportsnap::app::Modal::Help { .. })));
    press(&mut app, KeyCode::Char('q'));
    assert!(app.modal().is_none());
    assert!(!app.is_quit_armed(), "q closes the modal, never arms the quit");
    assert!(app.is_running());
}

#[test]
fn esc_and_question_mark_also_close_the_help_modal() {
    for closer in [KeyCode::Esc, KeyCode::Char('?')] {
        let mut app = app();
        press(&mut app, KeyCode::Char('?'));
        assert!(app.modal().is_some());
        press(&mut app, closer);
        assert!(app.modal().is_none(), "{closer:?} closes help");
    }
}

#[test]
fn a_opens_the_action_menu_on_a_screen_with_actions() {
    let mut app = app();
    jump(&mut app, '2'); // memories
    press(&mut app, KeyCode::Char('a'));
    match app.modal() {
        Some(exportsnap::app::Modal::ActionMenu(menu)) => {
            assert_eq!(menu.labels, ["start run"]);
            assert_eq!(menu.hotkeys, [Some('s')], "the run trigger takes its first free letter");
        }
        other => panic!("expected the action menu, got {other:?}"),
    }
}

#[test]
fn a_is_inert_on_a_screen_with_no_actions() {
    let mut app = app(); // overview: read-only, no actions
    press(&mut app, KeyCode::Char('a'));
    assert!(app.modal().is_none(), "overview has no action to list");
    assert_eq!(app.active(), Tab::Overview);
}

#[test]
fn esc_and_q_close_the_action_menu_without_arming_the_quit() {
    for closer in [KeyCode::Esc, KeyCode::Char('q')] {
        let mut app = app();
        jump(&mut app, '2');
        press(&mut app, KeyCode::Char('a'));
        assert!(app.modal().is_some());
        press(&mut app, closer);
        assert!(app.modal().is_none(), "{closer:?} closes the menu");
        assert!(!app.is_quit_armed(), "closing the menu never arms the quit");
    }
}

#[test]
fn a_modal_owns_input_arrows_and_jumps_do_not_switch_tabs() {
    let mut app = app();
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Right);
    assert_eq!(app.active(), Tab::Overview, "← → must not switch tabs while a modal is open");
    jump(&mut app, '3');
    assert_eq!(app.active(), Tab::Overview, "⌥<digit> must not jump while a modal is open");
}

#[test]
fn the_action_menu_hotkey_runs_its_action() {
    let state = tempfile::TempDir::new().unwrap();
    let mut app = app();
    jump(&mut app, '2'); // memories
    app.memories_mut().set_manifest_dir(state.path().to_path_buf());
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Char('s')); // "start run" → s
    assert!(app.modal().is_none(), "the hotkey closes the menu");
    assert!(app.memories().run_in_flight(), "the picked action ran");
}

#[test]
fn enter_picks_the_selected_action_too() {
    let state = tempfile::TempDir::new().unwrap();
    let mut app = app();
    jump(&mut app, '2'); // memories
    app.memories_mut().set_manifest_dir(state.path().to_path_buf());
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Enter);
    assert!(app.modal().is_none());
    assert!(app.memories().run_in_flight());
}

#[test]
fn the_help_modal_derives_its_sections_from_the_active_screen() {
    let mut app = app();
    // Overview has no screen keys: just GLOBAL, which lists `q`/`?`/`a` unconditionally (the spec's
    // GLOBAL section is not gated on the screen having actions).
    let sections = app.help_sections();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].title, "global");
    assert_eq!(sections[0].rows, [("q", "back / quit"), ("?", "help"), ("a", "actions"), ("← →", "switch tab"), ("⌃c", "quit")]);

    // Memories has its own keys: GLOBAL stays fixed and the screen section follows it.
    jump(&mut app, '2');
    let sections = app.help_sections();
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].rows, [("q", "back / quit"), ("?", "help"), ("a", "actions"), ("← →", "switch tab"), ("⌃c", "quit")]);
    assert_eq!(sections[1].title, "memories");
    assert_eq!(sections[1].rows, [("↑ ↓", "move"), ("↵", "start run"), ("space", "toggle transcode")]);
}

#[test]
fn a_and_question_mark_do_not_open_modals_while_a_settings_field_is_editing() {
    let mut app = app();
    jump(&mut app, '6'); // settings
    press(&mut app, KeyCode::Enter); // begin editing the output-dir row
    assert!(app.settings().is_editing(), "the edit session opened");
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Char('?'));
    assert!(app.modal().is_none(), "a and ? type into the field, never open a modal");
}

// ---- `x` dismissal stays live while a modal owns input (skill: Dismissal precedence) ----

#[test]
fn x_dismisses_a_live_footer_alert_while_a_modal_owns_input() {
    use exportsnap::export::memories_run::{RunError, RunEvent, RunOutcome};
    use std::path::PathBuf;
    use std::sync::mpsc;

    // A finished memories run raises the footer alert; an open action menu owns input, but `x`
    // still reaches the dismissal handler (cloudy-tui: Dismissal precedence) and leaves the menu
    // open — the modal's own keys (`?`, `esc`, `q`, arrows) are untouched.
    let mut app = app();
    jump(&mut app, '2'); // memories
    let (sender, receiver) = mpsc::channel();
    app.with_memories_channel(receiver);
    sender.send(RunEvent::Finished(RunOutcome::Failed(RunError::NoExportId(PathBuf::from("/nope"))))).unwrap();
    app.tick();
    assert!(app.memories().alert().is_some(), "the footer alert must be live before the menu opens");

    press(&mut app, KeyCode::Char('a'));
    assert!(matches!(app.modal(), Some(exportsnap::app::Modal::ActionMenu(_))));
    press(&mut app, KeyCode::Char('x'));
    assert!(app.memories().alert().is_none(), "x dismisses the footer alert while the menu is open");
    assert!(app.modal().is_some(), "x leaves the modal open");
}

#[test]
fn x_dismisses_a_live_toast_while_a_modal_owns_input() {
    // A failed settings write (no config dir) raises the DANGER toast, and it floats over every
    // tab — so an action menu open on another screen must still let `x` dismiss it, not swallow
    // the key the way `?`, `esc`, `q` and the arrows stay swallowed.
    let mut app = app();
    jump(&mut app, '6'); // settings
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // transcode row, past output dir, theme and ffmpeg
    }
    press(&mut app, KeyCode::Enter); // commit the toggle with no config dir → toast
    assert!(app.settings().toast_live(), "the failed write raises the toast");

    jump(&mut app, '2'); // memories, which has actions
    press(&mut app, KeyCode::Char('a'));
    assert!(matches!(app.modal(), Some(exportsnap::app::Modal::ActionMenu(_))));
    press(&mut app, KeyCode::Char('x'));
    assert!(!app.settings().toast_live(), "x dismisses the toast while the menu is open");
    assert!(app.modal().is_some(), "x leaves the modal open");
}

#[test]
fn x_dismissing_a_live_footer_alert_disarms_the_quit() {
    use exportsnap::export::memories_run::{RunError, RunEvent, RunOutcome};
    use std::path::PathBuf;
    use std::sync::mpsc;

    // The completion alert and an armed 2-step quit can coexist (the armed quit wins the footer
    // row, but the alert stays live beneath it). `x` dismisses the alert and must also disarm the
    // quit — it is a key press like any other, so the next `q` re-arms rather than quitting.
    let mut app = app();
    jump(&mut app, '2'); // memories
    let (sender, receiver) = mpsc::channel();
    app.with_memories_channel(receiver);
    sender.send(RunEvent::Finished(RunOutcome::Failed(RunError::NoExportId(PathBuf::from("/nope"))))).unwrap();
    app.tick();
    assert!(app.memories().alert().is_some(), "the footer alert must be live before arming");

    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed());

    press(&mut app, KeyCode::Char('x'));
    assert!(app.memories().alert().is_none(), "x dismisses the footer alert");
    assert!(!app.is_quit_armed(), "x dismissing the alert must also disarm the armed quit");
    assert!(app.is_running());
}

#[test]
fn x_dismissing_a_live_toast_disarms_the_quit() {
    // Same invariant on the toast path: `x` clears the floating toast and must not leave a 2-step
    // quit armed behind it.
    let mut app = app();
    jump(&mut app, '6'); // settings
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // transcode row, past output dir, theme and ffmpeg
    }
    press(&mut app, KeyCode::Enter); // commit the toggle with no config dir → toast
    assert!(app.settings().toast_live(), "the failed write raises the toast");

    press(&mut app, KeyCode::Char('q'));
    assert!(app.is_quit_armed());

    press(&mut app, KeyCode::Char('x'));
    assert!(!app.settings().toast_live(), "x dismisses the toast");
    assert!(!app.is_quit_armed(), "x dismissing the toast must also disarm the armed quit");
    assert!(app.is_running());
}

// ---- the loss warning (cloudy-tui: Loss warning; Keyboard grammar) ----
//
// Changed work is an open text edit whose draft differs from the value it opened from: the
// settings form's two path rows and the overview's source row are the crate's only text inputs.
// While such an edit is open, `q` types the letter into the field and every other key disarms an
// armed quit, so a completing `q` can never see changed work — the reachable triggers are the
// second ctrl+c (which arms inside an open edit) and a screen switch. The q completion carries
// the same check defensively, for a screen whose changed work is not an open edit.
//
// The suspended drafts a switch leaves behind can only ever be UNCHANGED ones, so the ACTIVE
// screen is the only holder of changed work any of these checks inspect.

fn ctrl_c(app: &mut App) {
    press_with(app, KeyCode::Char('c'), KeyModifiers::CONTROL);
}

/// The settings tab, with an out-dir draft that differs from the value it opened from.
fn changed_settings_draft(app: &mut App, typed: &str) {
    jump(app, '6'); // settings
    press(app, KeyCode::Enter); // begin editing the output-dir row, the form's first
    for ch in typed.chars() {
        press(app, KeyCode::Char(ch));
    }
    assert!(app.settings().is_editing(), "the draft must be open before the quit is attempted");
}

/// The overview tab, with a source draft that differs from the value it opened from.
fn changed_overview_draft(app: &mut App, typed: &str) {
    press(app, KeyCode::Char('s')); // focus the source row
    press(app, KeyCode::Enter); // open the edit
    for ch in typed.chars() {
        press(app, KeyCode::Char(ch));
    }
    assert!(app.overview().is_editing(), "the draft must be open before the quit is attempted");
}

/// An app whose settings screen writes a real config file, so a save can succeed and be read
/// back. The temp dir outlives the call, so the tuple must be kept for the assertion to see it.
fn app_with_config_dir() -> (App, tempfile::TempDir) {
    let config_dir = tempfile::TempDir::new().unwrap();
    let layers = SettingsLayers { config_dir: Some(config_dir.path().to_path_buf()), ..SettingsLayers::defaults_for(Tier::Full) };
    let app = App::start(Tier::Full, PathBuf::from("/nope"), RunDefaults::resolve(None, &Config::default(), Path::new("/nope")), layers);
    (app, config_dir)
}

#[test]
fn ctrl_c_past_a_changed_settings_draft_opens_the_loss_warning() {
    let mut app = app();
    changed_settings_draft(&mut app, "x");

    ctrl_c(&mut app);
    ctrl_c(&mut app);

    assert!(app.is_running(), "quitting past changed work must warn first, never drop it");
    match app.modal() {
        Some(exportsnap::app::Modal::LossWarning(warning)) => assert_eq!(warning.pending, exportsnap::app::PendingExit::Quit),
        other => panic!("expected the loss warning over the quit, got {other:?}"),
    }
    assert!(app.settings().is_editing(), "the warning keeps the draft open behind it");
}

#[test]
fn ctrl_c_past_a_changed_overview_draft_opens_the_loss_warning() {
    let mut app = app();
    changed_overview_draft(&mut app, "/warned/src");

    ctrl_c(&mut app);
    ctrl_c(&mut app);

    assert!(app.is_running(), "quitting past changed work must warn first, never drop it");
    match app.modal() {
        Some(exportsnap::app::Modal::LossWarning(warning)) => assert_eq!(warning.pending, exportsnap::app::PendingExit::Quit),
        other => panic!("expected the loss warning over the quit, got {other:?}"),
    }
    assert!(app.overview().is_editing(), "the warning keeps the draft open behind it");
}

#[test]
fn the_loss_warning_save_button_commits_the_draft_then_quits() {
    let (mut app, _config_dir) = app_with_config_dir();
    changed_settings_draft(&mut app, "/warned/out");

    ctrl_c(&mut app);
    ctrl_c(&mut app);
    press(&mut app, KeyCode::Enter); // save is default-focused

    assert!(!app.is_running(), "a valid save commits, then performs the quit");
    assert!(app.modal().is_none());
    assert!(!app.settings().is_editing(), "the commit closes the edit");
    assert_eq!(app.settings().effective_out_root(), Path::new("/warned/out"), "the draft landed in the config");
}

#[test]
fn the_loss_warning_discard_button_drops_the_draft_then_quits() {
    // No config dir, so any commit would fail loudly and raise the toast — discard must write
    // nothing at all.
    let mut app = app();
    changed_settings_draft(&mut app, "/dropped/out");

    ctrl_c(&mut app);
    ctrl_c(&mut app);
    press(&mut app, KeyCode::Up); // save -> discard
    press(&mut app, KeyCode::Enter);

    assert!(!app.is_running(), "discard drops the work, then performs the quit");
    assert!(app.modal().is_none());
    assert!(!app.settings().is_editing(), "discard drops the edit");
    assert!(!app.settings().toast_live(), "discard commits nothing");
}

#[test]
fn the_loss_warning_go_back_button_keeps_the_draft_and_the_app() {
    let (mut app, _config_dir) = app_with_config_dir();
    changed_settings_draft(&mut app, "/kept/out");

    ctrl_c(&mut app);
    ctrl_c(&mut app);
    press(&mut app, KeyCode::Up); // save -> discard
    press(&mut app, KeyCode::Up); // discard -> go back
    press(&mut app, KeyCode::Enter);

    assert!(app.is_running());
    assert!(app.modal().is_none());
    assert!(!app.is_quit_armed(), "opening the warning disarms the quit; go back must not re-arm it");
    assert!(app.settings().is_editing(), "go back keeps the edit open");
    // The draft survived whole: committing it through the normal enter path writes it.
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.settings().effective_out_root(), Path::new("/kept/out"), "the draft carried go back untouched");
}

#[test]
fn esc_and_q_close_the_loss_warning_without_quitting() {
    for closer in [KeyCode::Esc, KeyCode::Char('q')] {
        let mut app = app();
        changed_settings_draft(&mut app, "x");
        ctrl_c(&mut app);
        ctrl_c(&mut app);
        assert!(matches!(app.modal(), Some(exportsnap::app::Modal::LossWarning(_))));

        press(&mut app, closer);
        assert!(app.modal().is_none(), "{closer:?} closes the warning");
        assert!(app.is_running(), "closing the warning never quits");
        assert!(app.settings().is_editing(), "the work stays as it was");
        assert!(!app.is_quit_armed(), "closing the warning never re-arms the quit");
    }
}

#[test]
fn a_failing_save_closes_the_warning_without_quitting_or_dropping() {
    // No config dir: the commit's write fails, and the contract's save rule is all-or-nothing —
    // nothing commits or drops, the modal closes, nothing quits, and the failure surfaces.
    let mut app = app();
    changed_settings_draft(&mut app, "/failing/out");

    ctrl_c(&mut app);
    ctrl_c(&mut app);
    press(&mut app, KeyCode::Enter); // save, which cannot write

    assert!(app.is_running(), "a failed save never quits");
    assert!(app.modal().is_none(), "the modal closes on a failed save");
    assert!(app.settings().is_editing(), "nothing commits or drops: the edit stays open");
    assert!(app.settings().toast_live(), "the failure surfaces as this crate's DANGER toast");
}

// ---- switch away from changed work (cloudy-tui: Tab bar → Switching tabs; todo §21 row 3) ----
//
// The jump and the arrows share one switch path: unchanged work suspends silently as today, a
// changed draft opens the loss warning in its switch form, and an `⌥<digit>` naming the screen
// already shown is no switch at all. A completing ctrl+c over a switch warning turns it to its
// quit form; `q` closes it like any modal.

#[test]
fn a_jump_away_from_an_unchanged_draft_suspends_it_silently() {
    // The guard: the contract's suspension for unchanged work is untouched by the warning.
    let mut app = app();
    changed_overview_draft(&mut app, "");
    assert!(app.overview().is_editing(), "the edit is open with nothing typed");

    jump(&mut app, '3');

    assert_eq!(app.active(), Tab::ChatMedia, "the jump completed");
    assert!(app.overview().is_editing(), "the unchanged draft suspends with the screen it left");
    assert!(app.modal().is_none(), "unchanged work never warns");
}

#[test]
fn a_jump_away_from_a_changed_draft_opens_the_loss_warning_in_its_switch_form() {
    let mut app = app();
    changed_overview_draft(&mut app, "/warned/src");

    jump(&mut app, '3');

    assert_eq!(app.active(), Tab::Overview, "the switch waits behind the warning");
    match app.modal() {
        Some(exportsnap::app::Modal::LossWarning(warning)) => {
            assert_eq!(warning.pending, exportsnap::app::PendingExit::Switch(Tab::ChatMedia))
        }
        other => panic!("expected the loss warning over the jump, got {other:?}"),
    }
    assert!(app.overview().is_editing(), "the draft stays open behind the warning");
}

#[test]
fn the_switch_warning_save_commits_and_completes_the_jump() {
    let mut app = app();
    changed_overview_draft(&mut app, "/committed/src");

    jump(&mut app, '3');
    press(&mut app, KeyCode::Enter); // save is default-focused

    assert_eq!(app.active(), Tab::ChatMedia, "a valid save commits, then performs the jump");
    assert!(app.modal().is_none());
    assert!(!app.overview().is_editing(), "the commit closes the edit");
    assert_eq!(app.account().source(), Path::new("/committed/src"), "the committed source reached the re-probe");
}

#[test]
fn the_switch_warning_discard_drops_the_draft_and_completes_the_jump() {
    let mut app = app();
    changed_overview_draft(&mut app, "/dropped/src");

    jump(&mut app, '3');
    press(&mut app, KeyCode::Up); // save -> discard
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.active(), Tab::ChatMedia, "discard drops the work, then performs the jump");
    assert!(app.modal().is_none());
    assert!(!app.overview().is_editing(), "discard drops the edit");
    assert_eq!(app.account().source(), Path::new(""), "the source was never committed");
}

#[test]
fn the_switch_warning_go_back_cancels_the_jump() {
    let mut app = app();
    changed_overview_draft(&mut app, "/kept/src");

    jump(&mut app, '3');
    press(&mut app, KeyCode::Up); // save -> discard
    press(&mut app, KeyCode::Up); // discard -> go back
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.active(), Tab::Overview, "go back cancels the switch");
    assert!(app.modal().is_none());
    assert!(app.overview().is_editing(), "the draft stays as it was");
    assert!(!app.is_quit_armed());
    // The draft survived whole: committing it through the normal enter path re-probes it.
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.account().source(), Path::new("/kept/src"), "the draft carried go back untouched");
}

#[test]
fn an_arrow_mid_edit_stays_with_the_field_instead_of_switching() {
    // The contract gives the arrows to the field while an input is being edited ("in edit mode
    // ←/→ move the caret"; switching is "suspended while editing a text input"), so an open
    // changed draft must not convert the caret keys into switches — the escape from an edit is
    // the never-trapped ⌥<digit> jump. The switch path's own changed-work check stays live for
    // the jump and defensive for the arrows.
    let mut app = app();
    changed_settings_draft(&mut app, "x");

    press(&mut app, KeyCode::Right);

    assert_eq!(app.active(), Tab::Settings, "the arrow never switched");
    assert!(app.modal().is_none(), "the arrow never opened the loss warning");
    assert!(app.settings().is_editing(), "the field keeps its caret keys");
    assert!(app.is_running());
}

#[test]
fn an_alt_digit_naming_the_active_screen_neither_switches_nor_warns() {
    let mut app = app();
    changed_overview_draft(&mut app, "x");
    ctrl_c(&mut app);
    assert!(app.is_quit_armed(), "the ctrl+c arm is live");

    jump(&mut app, '1'); // overview's own digit

    assert_eq!(app.active(), Tab::Overview, "no switch happened");
    assert!(app.modal().is_none(), "no warning either — an ⌥<digit> naming the shown screen is no switch");
    assert!(!app.is_quit_armed(), "the arm disarms, as on any key");
    assert!(app.overview().is_editing(), "the draft stays open");
}

#[test]
fn a_second_ctrl_c_over_a_switch_warning_turns_it_to_its_quit_form() {
    let mut app = app();
    changed_overview_draft(&mut app, "/warned/src");
    jump(&mut app, '3');
    match app.modal() {
        Some(exportsnap::app::Modal::LossWarning(warning)) => {
            assert_eq!(warning.pending, exportsnap::app::PendingExit::Switch(Tab::ChatMedia))
        }
        other => panic!("expected the switch warning, got {other:?}"),
    }

    ctrl_c(&mut app); // arms; the modal keeps its state
    ctrl_c(&mut app); // completes: the switch warning becomes the quit warning

    assert!(app.is_running(), "the morphed warning still resolves the quit");
    match app.modal() {
        Some(exportsnap::app::Modal::LossWarning(warning)) => assert_eq!(warning.pending, exportsnap::app::PendingExit::Quit),
        other => panic!("expected the quit form after the morph, got {other:?}"),
    }
    assert!(app.overview().is_editing(), "the draft stays open behind the morphed warning");
}

// ---- two-press ctrl+c (cloudy-tui: Keyboard grammar — the audit's quit row) ----

#[test]
fn any_other_key_disarms_a_ctrl_c_arm() {
    for disarming in [KeyCode::Right, KeyCode::Char('z'), KeyCode::Esc] {
        let mut app = app();
        ctrl_c(&mut app);
        assert!(app.is_quit_armed());

        press(&mut app, disarming);
        assert!(!app.is_quit_armed(), "{disarming:?} should disarm");
        assert!(app.is_running());
    }
}

#[test]
fn a_q_arm_and_a_ctrl_c_arm_are_independent() {
    // A completing press must be the SAME key that armed: a q arm completed by ctrl+c, or a
    // ctrl+c arm completed by q, is the contract's named anti-pattern. The other key disarms and
    // does its own job, so three presses total either way. Each direction scopes its own app.
    {
        let mut app = app();
        press(&mut app, KeyCode::Char('q'));
        assert!(app.is_quit_armed());
        ctrl_c(&mut app);
        assert!(app.is_running(), "ctrl+c must not complete a q arm");
        ctrl_c(&mut app);
        assert!(!app.is_running(), "the ctrl+c arm completes on its own second press");
    }

    {
        let mut app = app();
        ctrl_c(&mut app);
        assert!(app.is_quit_armed());
        press(&mut app, KeyCode::Char('q'));
        assert!(app.is_running(), "q must not complete a ctrl+c arm");
        press(&mut app, KeyCode::Char('q'));
        assert!(!app.is_running(), "the q arm completes on its own second press");
    }
}

#[test]
fn the_first_ctrl_c_keeps_a_modal_and_an_open_edit_intact() {
    // The first press only arms — a modal keeps its state and an open edit is not touched, per
    // the contract (Keyboard grammar: ctrl+c quits from anywhere in two presses). Each half
    // scopes its own app.
    {
        let mut app = app();
        press(&mut app, KeyCode::Char('?'));
        assert!(app.modal().is_some());
        ctrl_c(&mut app);
        assert!(app.modal().is_some(), "the modal keeps its state through the arm");
        assert!(app.is_quit_armed());
        assert!(app.is_running());
        ctrl_c(&mut app);
        assert!(!app.is_running());
    }

    {
        let mut app = app();
        jump(&mut app, '6');
        press(&mut app, KeyCode::Enter); // begin editing the output-dir row
        assert!(app.settings().is_editing());
        ctrl_c(&mut app);
        assert!(app.settings().is_editing(), "the open edit keeps its state through the arm");
        assert!(app.is_quit_armed());
        assert!(app.is_running());
        ctrl_c(&mut app);
        assert!(!app.is_running(), "an unchanged edit quits clean");
    }
}

#[test]
fn a_modal_key_disarms_an_armed_quit() {
    // "any other key (q included) disarms and does its own job" holds for the keys a modal
    // consumes too: an arm taken inside the help modal dies with the modal's esc, and the next
    // ctrl+c only arms again. Each half scopes its own app.
    {
        let mut app = app();
        press(&mut app, KeyCode::Char('?'));
        assert!(app.modal().is_some());
        ctrl_c(&mut app);
        assert!(app.is_quit_armed(), "ctrl+c arms inside the modal");
        press(&mut app, KeyCode::Esc);
        assert!(app.modal().is_none());
        assert!(!app.is_quit_armed(), "esc closed the modal and disarmed the arm with it");
        assert!(app.is_running());
        ctrl_c(&mut app);
        assert!(app.is_quit_armed(), "the next ctrl+c only arms again");
        assert!(app.is_running());
    }

    // The loss warning's own keys disarm as well: an arm taken inside the warning dies with go
    // back instead of surviving into the resolved state.
    {
        let mut app = app();
        changed_overview_draft(&mut app, "x");
        jump(&mut app, '3');
        ctrl_c(&mut app);
        assert!(app.is_quit_armed());
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter); // go back
        assert!(!app.is_quit_armed(), "go back did not carry the in-warning arm out");
        assert!(app.overview().is_editing(), "the draft stays as it was");
        assert!(app.is_running());
    }
}

#[test]
fn a_second_ctrl_c_over_the_quit_warning_does_nothing_more() {
    let mut app = app();
    changed_settings_draft(&mut app, "x");
    ctrl_c(&mut app);
    ctrl_c(&mut app);
    match app.modal() {
        Some(exportsnap::app::Modal::LossWarning(warning)) => assert_eq!(warning.pending, exportsnap::app::PendingExit::Quit),
        other => panic!("expected the quit warning, got {other:?}"),
    }

    ctrl_c(&mut app); // arms again
    ctrl_c(&mut app); // over a quit's warning: nothing more

    assert!(app.is_running(), "the completing press does not bypass the open warning");
    match app.modal() {
        Some(exportsnap::app::Modal::LossWarning(warning)) => assert_eq!(warning.pending, exportsnap::app::PendingExit::Quit),
        other => panic!("the warning stays as it was, got {other:?}"),
    }
}
