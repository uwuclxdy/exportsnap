//! Top-level app state and the blocking event loop. Keys follow the cloudy-tui Keyboard
//! grammar; the frame itself is composed by [`crate::tui::shell`].

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::export::env::{Environment, Tool, locate, probe_target};
use crate::export::local_fix::default_out_root;
use crate::tui::alert::RunAlert;
use crate::tui::screens::chat_media::ChatMedia;
use crate::tui::screens::memories::Memories;
use crate::tui::screens::overview::Overview;
use crate::tui::shell;
use crate::tui::theme::{Palette, Tier};

/// How long the event loop waits for input before ticking. One spinner frame per tick (80 ms),
/// which is also the rate the run's manifest statuses are polled at.
const TICK: Duration = Duration::from_millis(80);

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
///
/// # Two screens now drive runs, and every branch that used to name the memories one was decided
/// per consumer
///
/// - **The footer alert belongs to the ACTIVE screen** ([`Self::alert`]). A footer row is screen
///   chrome, and "run finished · 12 fixed" carries no clue which run it is about, so showing the
///   chat screen's outcome while the user reads the memories tab misattributes it. An alert raised
///   on a background tab is not lost — it waits, and `x` on that tab clears it — which is a better
///   failure than a message the reader cannot place. The cost, stated rather than hidden: a run
///   finishing on a tab the user is not looking at is silent until they come back. Closing that
///   properly is the contract's tab-activity color channel, which this app does not have yet.
/// - **`x` dismisses whatever the footer is SHOWING**, which is the same function, so the key and
///   the row can never disagree about which of two alerts is live.
/// - **`q`, the key routing and the `⌥` jump's implicit ascend all address the active screen**
///   ([`Self::descended`], [`Self::ascend_active`]). Ascending a screen the user is not on would
///   silently reset a pane they left descended.
/// - **The tick drives BOTH screens** ([`Self::tick`]), because a run keeps running when its tab is
///   not in view and its manifest poll has to keep up with it.
#[derive(Debug)]
pub struct App {
    palette: Palette,
    active: Tab,
    quit_armed: bool,
    running: bool,
    overview: Overview,
    memories: Memories,
    chat_media: ChatMedia,
}

impl App {
    /// An app with no export behind it: the overview tab active and every screen in its own
    /// pre-read state.
    ///
    /// Every screen starts on [`Environment::default`] rather than on a probe of its own. Nothing
    /// here knows which dir the run is about yet, so a probe would measure a path the user never
    /// named and be thrown away by [`Self::start`] a moment later — which is what it used to do,
    /// twice.
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        Self {
            palette: Palette::new(tier),
            active: Tab::Overview,
            quit_armed: false,
            running: true,
            overview: Overview::unloaded(),
            memories: Memories::with_environment(PathBuf::new(), None, Environment::default()),
            chat_media: ChatMedia::with_environment(PathBuf::new(), None, Environment::default()),
        }
    }

    /// The whole startup composition, and the only thing `main` builds a running app with: read the
    /// source dir once, probe the machine once, hand every screen the result.
    ///
    /// **`PATH` is walked once per [`Tool`], not once per screen.** Where a tool sits does not
    /// depend on the path being measured, so the media screens take the overview's answers with
    /// only the two space figures re-measured — the output root's filesystem is the source's until
    /// `--out` names another, and even when it does the difference is two `statvfs` calls rather
    /// than a second walk of every `PATH` entry.
    ///
    /// Deliberately not a process-wide cache behind a `OnceLock`. This is a long-lived TUI: a cached
    /// "ffmpeg missing" would outlive the user leaving to install it, and no reload path could ever
    /// clear it. A snapshot taken per startup carries the same staleness the screens already have
    /// and stays a value a test can hand in.
    #[must_use]
    pub fn start(tier: Tier, source: PathBuf, out_root: Option<PathBuf>) -> Self {
        Self::start_with(tier, source, out_root, locate)
    }

    /// [`Self::start`] against an explicit locator — the seam that makes the walk count above
    /// observable, since the real [`locate`] cannot be made to report its calls.
    fn start_with(tier: Tier, source: PathBuf, out_root: Option<PathBuf>, locate: impl Fn(Tool) -> Option<PathBuf>) -> Self {
        // Resolved here rather than left to each screen so the probe below measures the filesystem
        // the run will actually write to.
        let out_root = out_root.unwrap_or_else(|| default_out_root(&source));
        let environment = Environment::probe_with(locate, &source);
        let media = environment.measured_at(probe_target(&out_root));

        Self::new(tier).with_overview(Overview::load_with(&source, environment)).with_source_environment(source, Some(out_root), media)
    }

    /// Hands the overview screen a real read of the source dir. [`Self::start`] calls this before
    /// the first frame; [`Self::new`] on its own draws the unloaded state.
    #[must_use]
    pub fn with_overview(mut self, overview: Overview) -> Self {
        self.overview = overview;
        self
    }

    /// Hands both media screens their run context: the source dir, the output root — `--out`'s
    /// value or the default — and the machine probe [`Self::start`] already made.
    ///
    /// One call rather than one per screen: the two legs read one export and write under one output
    /// root, so a caller handing them different sources would be describing a state that cannot
    /// arise from the command line. It is also the seam a render test uses to pin the disk-free
    /// rows without reaching for the real filesystem.
    #[must_use]
    pub fn with_source_environment(mut self, source: PathBuf, out_root: Option<PathBuf>, environment: Environment) -> Self {
        self.memories = Memories::with_environment(source.clone(), out_root.clone(), environment.clone());
        self.chat_media = ChatMedia::with_environment(source, out_root, environment);
        self
    }

    /// Hands the memories screen a receiver the test feeds — the seam the render and tick tests
    /// drive; the events flow through the real `tick` machinery.
    pub fn with_memories_channel(&mut self, receiver: std::sync::mpsc::Receiver<crate::export::memories_run::RunEvent>) {
        self.memories.with_channel(receiver);
    }

    /// [`Self::with_memories_channel`] for the chat media screen.
    pub fn with_chat_media_channel(&mut self, receiver: std::sync::mpsc::Receiver<crate::export::chat_run::RunEvent>) {
        self.chat_media.with_channel(receiver);
    }

    /// What `--print-source` prints: every screen's own view of the dir this app was launched
    /// against, as `key=value` lines, `\n`-terminated. Machine-first — the flag exists to be read by
    /// something other than a human, so no alignment, no glyphs, no color.
    ///
    /// **Assembled from all three screens, not from one.** [`Self::start`] hands the source to the
    /// overview's read, to the space probe, and to both media screens, and those are four separate
    /// deliveries of one argument. A report observing only the overview left the other three able to
    /// take `PathBuf::new()` with the whole suite green — measured 2026-08-11, and it is why the keys
    /// below are per-screen rather than one `source=`.
    ///
    /// **Every path value is quoted and escaped** ([`std::path::Path`]'s `Debug`), because a path is
    /// user-supplied bytes going into a line-oriented format: a source dir whose name contains a
    /// newline could otherwise emit `parts=one` ahead of the real answer and a first-wins reader
    /// would believe it. Two ceilings, both in `Debug`'s hands rather than this crate's: the exact
    /// escaping is a `Debug` impl, so a toolchain bump can move it — `tests/print_source.rs` spells
    /// the expected bytes out the long way and reds if it does — and a path that is not UTF-8 comes
    /// back with `\x` escapes, which is lossless enough to compare but is not the original bytes.
    /// Values that are not paths are bare: a token from a closed set, or digits.
    ///
    /// Keys are stable and a reader should match on the name, not the position. Adding one is
    /// allowed; the numeric keys are already absent whenever nothing measured them. `memories-out`
    /// and `chat-out` are the roots each screen was handed rather than where either writes — see
    /// [`ChatMedia::run_paths`] for why the chat leg's own output sits one level below its key.
    #[must_use]
    pub fn source_report(&self) -> String {
        let (memories_source, memories_out) = self.memories.run_paths();
        let (chat_source, chat_out) = self.chat_media.run_paths();
        format!(
            "{}memories-source={memories_source:?}\nmemories-out={memories_out:?}\nchat-source={chat_source:?}\nchat-out={chat_out:?}\n",
            self.overview.report()
        )
    }

    #[must_use]
    pub const fn palette(&self) -> &Palette {
        &self.palette
    }

    #[must_use]
    pub const fn overview(&self) -> &Overview {
        &self.overview
    }

    #[must_use]
    pub const fn memories(&self) -> &Memories {
        &self.memories
    }

    /// The memories screen's mutable half — the shell borrows it to render the stateful table,
    /// and tests reach the worker seam through it.
    pub fn memories_mut(&mut self) -> &mut Memories {
        &mut self.memories
    }

    #[must_use]
    pub const fn chat_media(&self) -> &ChatMedia {
        &self.chat_media
    }

    /// The chat media screen's mutable half, for the same two reasons as [`Self::memories_mut`].
    pub fn chat_media_mut(&mut self) -> &mut ChatMedia {
        &mut self.chat_media
    }

    /// The alert the footer row is showing this frame: the ACTIVE screen's, or none.
    ///
    /// The single source for both the footer and the `x` key, so the row and the dismissal can never
    /// disagree about which of two live alerts is the one on screen. See the type's own docs for why
    /// the active screen wins rather than a priority order across screens.
    #[must_use]
    pub const fn alert(&self) -> Option<&RunAlert> {
        match self.active {
            Tab::Memories => self.memories.alert(),
            Tab::ChatMedia => self.chat_media.alert(),
            Tab::Overview | Tab::History | Tab::Account | Tab::Settings => None,
        }
    }

    /// Whether the active screen's read-only pane owns the caret. `false` on every screen that has
    /// no pane to descend into, which is what makes `q` the quit key there.
    #[must_use]
    pub const fn descended(&self) -> bool {
        match self.active {
            Tab::Memories => self.memories.descended(),
            Tab::ChatMedia => self.chat_media.descended(),
            Tab::Overview | Tab::History | Tab::Account | Tab::Settings => false,
        }
    }

    /// Returns the caret to the active screen's form. A no-op on a screen with no descended pane.
    ///
    /// Deliberately NOT "ascend every screen": a jump away from the memories tab must not quietly
    /// reset a chat-media pane the user left descended and will come back to.
    fn ascend_active(&mut self) {
        match self.active {
            Tab::Memories => self.memories.ascend(),
            Tab::ChatMedia => self.chat_media.ascend(),
            Tab::Overview | Tab::History | Tab::Account | Tab::Settings => {}
        }
    }

    /// Dismisses the alert the footer is showing, answering whether there was one. The `x` key's
    /// whole job; `x` with nothing showing is inert.
    fn dismiss_alert(&mut self) -> bool {
        match self.active {
            Tab::Memories => self.memories.dismiss_alert(),
            Tab::ChatMedia => self.chat_media.dismiss_alert(),
            Tab::Overview | Tab::History | Tab::Account | Tab::Settings => false,
        }
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

    /// Draws, waits for an event, applies it, ticks, repeats.
    ///
    /// While EITHER screen's run is live the wait is capped at one tick (80 ms), so the spinner
    /// animates and the per-item poll refreshes without input; with no run live there is nothing
    /// to animate, and the loop blocks on input instead of redrawing an identical frame every
    /// 80 ms forever. The gate is a disjunction rather than "the active screen's run" because a run
    /// keeps running when its tab is not in view, and a poll that stopped there would leave its
    /// table frozen at whatever the user last saw.
    ///
    /// # Errors
    ///
    /// Returns the backend's error if drawing to the terminal or reading an event fails.
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while self.running {
            {
                let app = &mut *self;
                terminal.draw(|frame| shell::render(frame, app))?;
            }
            if self.memories.run_in_flight() || self.chat_media.run_in_flight() {
                if event::poll(TICK)? {
                    self.handle_event(&event::read()?);
                }
                self.tick();
            } else {
                self.handle_event(&event::read()?);
            }
        }
        Ok(())
    }

    /// One timer tick: pump each run's channel, poll its statuses, advance its spinner. A screen
    /// with no run in flight returns immediately, so ticking both costs nothing when one is idle.
    pub fn tick(&mut self) {
        self.memories.tick();
        self.chat_media.tick();
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

        // `q` while the ACTIVE screen's table pane is descended is the back key, not the quit key —
        // it ascends, exactly like esc (cloudy-tui: q back whenever q would ascend a level). The
        // hint bar advertises the same, off the same answer.
        if matches!(key.code, KeyCode::Char('q' | 'Q')) && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            if self.descended() {
                self.ascend_active();
                self.quit_armed = false;
                return;
            }
            // `q` at the top level arms a 2-step quit and never quits in one press. Hotkeys are
            // case-insensitive, so caps lock can't strand a user with no way out.
            if self.quit_armed {
                self.running = false;
            } else {
                self.quit_armed = true;
            }
            return;
        }

        // `x` dismisses the run-completion footer alert the row is actually showing — the only
        // thing it is bound to. With no alert live it is a key like any other (it still disarms an
        // armed quit below).
        if matches!(key.code, KeyCode::Char('x' | 'X')) && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() && self.dismiss_alert()
        {
            return;
        }

        self.quit_armed = false;

        // Screen-owned keys on whichever screen is active (form rows, table scroll,
        // descend/ascend). A screen answers `false` for keys that belong to the shell, so the tab
        // switching below still works when a form owns the caret.
        let consumed = match self.active {
            Tab::Memories => self.memories.handle_key(key),
            Tab::ChatMedia => self.chat_media.handle_key(key),
            Tab::Overview | Tab::History | Tab::Account | Tab::Settings => false,
        };
        if consumed {
            return;
        }

        match key.code {
            KeyCode::Left if key.modifiers == KeyModifiers::NONE => {
                self.active = self.active.previous();
            }
            KeyCode::Right if key.modifiers == KeyModifiers::NONE => {
                self.active = self.active.next();
            }
            KeyCode::Char(c) if key.modifiers == KeyModifiers::ALT => {
                if let Some(tab) = c.to_digit(10).and_then(Tab::from_jump_digit) {
                    // A jump ascends the pane it is LEAVING (cloudy-tui: the jump ascends
                    // implicitly), and moving focus away disarms the quit like any other key.
                    // Ascending every screen instead would reset a pane the user is not on.
                    self.ascend_active();
                    self.active = tab;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;

    use super::*;

    #[test]
    fn startup_walks_path_once_per_tool() {
        // Three screens want a machine probe and the two space figures are the only part of one
        // that depends on the path, so the walk count belongs to the tool roster and not to the
        // screen count. It read five walks per tool before this was the composition.
        let walks = RefCell::new(Vec::new());
        // A source dir that is not there: the export read answers "missing" off one failed listing,
        // which leaves the probes as the only thing this drives.
        let app = App::start_with(Tier::Full, PathBuf::from("/nope"), None, |tool| {
            walks.borrow_mut().push(tool);
            Some(PathBuf::from(format!("/located/{}", tool.command())))
        });

        // Tied to the tool roster but deliberately NOT to its order: `Tool::ALL` is declared as
        // report order, a display concern, and reordering it for a display reason must not red a
        // walk-count pin.
        let walks = walks.into_inner();
        assert_eq!(walks.len(), Tool::ALL.len(), "one walk per tool and no more: {walks:?}");
        for tool in Tool::ALL {
            assert_eq!(walks.iter().filter(|walked| **walked == tool).count(), 1, "{tool:?} is looked up exactly once");
        }

        // The count alone stays green if the composition locates the tools and then hands the
        // screens a default environment, so pin that the one probe reaches all three of them.
        for environment in [app.overview().environment(), app.memories().environment(), app.chat_media().environment()] {
            assert_eq!(environment.ffmpeg.as_deref(), Some(Path::new("/located/ffmpeg")), "that one probe reaches every screen");
        }

        // The tools are shared but the space figures are not: the overview measures the source dir
        // and the media screens measure the output root. Here the source is not there and the
        // default out root climbs to a dir that is, so the two differ by whether they can be
        // measured at all rather than by a byte count no machine agrees on.
        assert_eq!(app.overview().environment().available_space, None, "the overview measures the source dir, which is absent");
        assert!(app.memories().environment().available_space.is_some(), "the media screens measure the output root");
        assert!(app.chat_media().environment().available_space.is_some(), "the media screens measure the output root");
    }
}
