//! Top-level app state and the blocking event loop. Keys follow the cloudy-tui Keyboard
//! grammar; the frame itself is composed by [`crate::tui::shell`].

use std::io;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::tui::shell;
use crate::tui::theme::{Palette, Tier};

/// The six top-level screens (design.md: TUI screen map).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Overview,
    Memories,
    ChatMedia,
    History,
    Account,
    Settings,
}

impl Tab {
    /// Tab-bar order, left to right.
    pub const ALL: [Self; 6] = [Self::Overview, Self::Memories, Self::ChatMedia, Self::History, Self::Account, Self::Settings];

    /// Tab-bar label. Lowercase per the contract; the panel title uppercases it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Memories => "memories",
            Self::ChatMedia => "chat media",
            Self::History => "history",
            Self::Account => "account",
            Self::Settings => "settings",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Memories => 1,
            Self::ChatMedia => 2,
            Self::History => 3,
            Self::Account => 4,
            Self::Settings => 5,
        }
    }

    /// Next tab, wrapping past the last one back to the first.
    #[must_use]
    pub const fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// Previous tab, wrapping past the first one back to the last.
    #[must_use]
    pub const fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    /// `⌥<digit>` target (cloudy-tui skill: Tab bar → Switching tabs). `1`–`8` are positional
    /// and `9` always lands on the last tab whatever the count, so the tail stays one press
    /// away past nine tabs. `0` is unbound and a digit with no tab behind it is inert; both
    /// return `None`.
    #[must_use]
    pub fn from_jump_digit(digit: u32) -> Option<Self> {
        match digit {
            9 => Self::ALL.last().copied(),
            1..=8 => Self::ALL.get(digit as usize - 1).copied(),
            _ => None,
        }
    }
}

/// Top-level app state: which screen is active, whether the 2-step quit is armed, and whether
/// the event loop should keep running.
#[derive(Debug)]
pub struct App {
    palette: Palette,
    active: Tab,
    quit_armed: bool,
    running: bool,
}

impl App {
    #[must_use]
    pub const fn new(tier: Tier) -> Self {
        Self { palette: Palette::new(tier), active: Tab::Overview, quit_armed: false, running: true }
    }

    #[must_use]
    pub const fn palette(&self) -> &Palette {
        &self.palette
    }

    #[must_use]
    pub const fn active(&self) -> Tab {
        self.active
    }

    #[must_use]
    pub const fn is_quit_armed(&self) -> bool {
        self.quit_armed
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// Draws, blocks for one event, applies it, repeats. There is no tick timer because
    /// nothing animates yet — a timer would only wake the process to redraw an identical
    /// frame. Add one alongside the first spinner or progress bar.
    ///
    /// # Errors
    ///
    /// Returns the backend's error if drawing to the terminal or reading an event fails.
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while self.running {
            terminal.draw(|frame| shell::render(frame, self))?;
            self.handle_event(&event::read()?);
        }
        Ok(())
    }

    /// Applies one terminal event. Resizes need no state change — the next draw reads the new
    /// size straight off the frame.
    pub fn handle_event(&mut self, event: &Event) {
        // crossterm reports Press *and* Release on Windows, so a handler that ignores `kind`
        // fires every binding twice there.
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            self.handle_key(*key);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // `ctrl+shift+c` is the terminal's own copy binding, not ours to claim, and it reaches us
        // in more than one shape. Under the kitty keyboard protocol it is the CSI-u base codepoint
        // `Char('c')` carrying SHIFT alongside CONTROL; add the `report alternate keys` flag and
        // crossterm's `parse_csi_u_encoded_key_code` swaps in the shifted codepoint and clears
        // SHIFT, leaving `Char('C')` + CONTROL. Windows sends `Char('C')` + both while shift is
        // held. The case check and the SHIFT rejection each carry a shape the other misses; every
        // other modifier still falls through, since "quit immediately, from anywhere" has to
        // survive a stray alt or super.
        //
        // Two deliberate limits. The legacy encoding offers nothing to reject — the chord arrives
        // as the bare byte 0x03, indistinguishable from plain `ctrl+c`. And `Char('C')` + CONTROL
        // has a second producer: Windows uppercases the char under caps lock and never reports
        // caps as a modifier, so caps-lock `ctrl+c` is the same `KeyEvent` as the alternate-keys
        // copy chord and gets dropped with it. Resolved toward not quitting, because stealing the
        // copy chord is the worse failure — that user still leaves via `q q`, which the Windows
        // parser routes around the case fixup. Doing better needs a signal crossterm does not
        // carry.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::SHIFT) {
            self.running = false;
            return;
        }

        // `q` at the top level arms a 2-step quit and never quits in one press. Hotkeys are
        // case-insensitive, so caps lock can't strand a user with no way out.
        if matches!(key.code, KeyCode::Char('q' | 'Q')) && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            if self.quit_armed {
                self.running = false;
            } else {
                self.quit_armed = true;
            }
            return;
        }

        self.quit_armed = false;

        match key.code {
            KeyCode::Left if key.modifiers == KeyModifiers::NONE => {
                self.active = self.active.previous();
            }
            KeyCode::Right if key.modifiers == KeyModifiers::NONE => {
                self.active = self.active.next();
            }
            KeyCode::Char(c) if key.modifiers == KeyModifiers::ALT => {
                if let Some(tab) = c.to_digit(10).and_then(Tab::from_jump_digit) {
                    self.active = tab;
                }
            }
            _ => {}
        }
    }
}
