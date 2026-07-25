//! Event→state transitions for the app shell. Pure: no terminal backend is involved, the
//! same handler the event loop calls is fed synthetic crossterm events.
//!
//! Every expectation is cross-checked against the cloudy-tui skill's Keyboard grammar and
//! Tab bar → Switching tabs sections, not against this crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use exportsnap::app::{App, Tab};
use exportsnap::tui::theme::Tier;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

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

// ---- ctrl+c (skill: Keyboard grammar — quit immediately, from anywhere) ----

#[test]
fn ctrl_c_quits_immediately() {
    let mut app = app();
    press_with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(!app.is_running());
}

#[test]
fn ctrl_c_still_quits_with_extra_modifiers_held() {
    // "quit immediately, from anywhere" — the binding tests for CONTROL rather than equality,
    // so a stray alt or super held alongside must not swallow the one guaranteed way out.
    let mut app = app();
    press_with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL | KeyModifiers::ALT);
    assert!(!app.is_running());
}

#[test]
fn bare_c_does_not_quit() {
    let mut app = app();
    press(&mut app, KeyCode::Char('c'));
    assert!(app.is_running());
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
fn a_resize_changes_no_state() {
    let mut app = app();
    press(&mut app, KeyCode::Char('q'));
    app.handle_event(&Event::Resize(20, 5));
    assert!(app.is_quit_armed(), "a resize is not a key press");
    assert_eq!(app.active(), Tab::Overview);
    assert!(app.is_running());
}
