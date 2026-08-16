//! Top-level app state and the blocking event loop. Keys follow the cloudy-tui Keyboard
//! grammar; the frame itself is composed by [`crate::tui::shell`].

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, ModifierKeyCode, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;

use crate::config::Config;
use crate::export::chat_fix::OverlayMode;
use crate::export::env::{Environment, Tool, locate, probe_target};
use crate::export::local_fix::default_out_root;
use crate::tui::alert::{RunAlert, TabActivity};
use crate::tui::screens::account::Account;
use crate::tui::screens::chat_media::ChatMedia;
use crate::tui::screens::history::History;
use crate::tui::screens::memories::Memories;
use crate::tui::screens::overview::{Overview, OverviewKey};
use crate::tui::screens::settings::{Settings, SettingsLayers};
use crate::tui::shell;
use crate::tui::theme::{Palette, Tier};
use crate::tui::widgets::{self, HelpSection};

/// How long the event loop waits for input before ticking. One spinner frame per tick (80 ms),
/// which is also the rate the run's manifest statuses are polled at. `pub(crate)` so the
/// settings screen's toast-lifetime coupling test can hold the 80 ms side of its ratio.
pub(crate) const TICK: Duration = Duration::from_millis(80);

/// The kitty keyboard protocol flags the jump-key overlay probe pushes before the event loop.
/// `DISAMBIGUATE_ESCAPE_CODES` is what makes a bare `⌥` arrive as a `KeyCode::Modifier` event at
/// all, and `REPORT_EVENT_TYPES` is what reports its release — without the release the overlay
/// would latch on and never clear (cloudy-tui: Tab bar → Jump-key overlay, "overlay support is
/// best-effort"). Deliberately NOT `REPORT_ALTERNATE_KEYS`, which would retarget the `ctrl+shift+c`
/// copy-chord guard in [`App::handle_key`].
const KITTY_KEYBOARD_FLAGS: KeyboardEnhancementFlags =
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES.union(KeyboardEnhancementFlags::REPORT_EVENT_TYPES);

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

    /// The `⌥<digit>` that jumps here, for the overlay index (cloudy-tui: Tab bar → Jump-key
    /// overlay). The first eight tabs are positional `1`–`8`; past nine tabs the last carries
    /// `9` and the ones between render bare. With six tabs every tab carries its positional
    /// digit, `settings` included, so none returns `None`.
    #[must_use]
    pub const fn jump_index(self) -> Option<u8> {
        let index = self.index();
        if index < 8 {
            Some(index as u8 + 1)
        } else if index + 1 == Self::ALL.len() {
            Some(9)
        } else {
            None
        }
    }
}

/// The run inputs the startup composition settled, once, in decision 66's order (flag > config >
/// detection > default). The config file is the raw layer, kept reachable in `main` so the
/// settings screen can state provenance; this is the effective answer a run reads, the way the
/// resolved tier is the only layer the screens see.
///
/// Deliberately no `Default` impl, for the reason [`crate::export::local_fix::VideoOptions`]
/// documents: one would have to answer an out root without resolving it, which reads as the
/// flag's answer while behaving as a guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDefaults {
    /// Where every leg writes: `--out` when it was passed, else the file's `out_dir`, else
    /// [`default_out_root`].
    pub out_root: PathBuf,
    /// The file's `ffmpeg_path`, or `None` to let detection decide. Not yet the final answer —
    /// [`App::start_with`] replaces the probe's finding with this when it is set, which is where
    /// the merge lives because the locate seam does.
    pub ffmpeg: Option<PathBuf>,
    /// The memories leg's transcode default — the file's `transcode`, else on.
    pub transcode: bool,
    /// The chat leg's overlay mode — the file's `overlay_mode`, else [`OverlayMode::Both`].
    pub overlay_mode: OverlayMode,
}

impl RunDefaults {
    /// Decision 66's order for every key, each resolved exactly once at startup.
    #[must_use]
    pub fn resolve(cli_out: Option<&Path>, config: &Config, source: &Path) -> Self {
        Self {
            out_root: cli_out.map(Path::to_path_buf).or_else(|| config.out_dir.clone()).unwrap_or_else(|| default_out_root(source)),
            ffmpeg: config.ffmpeg_path.clone(),
            transcode: config.transcode.unwrap_or(true),
            overlay_mode: config.overlay_mode.unwrap_or_default(),
        }
    }
}

/// A modal that owns input while open (cloudy-tui: Modals). Exactly one at a time: `a` opens the
/// action menu, `?` the help modal, and `q`/`esc`/`?` close whichever is open. While one is up,
/// every key but `ctrl+c` is the modal's — no tab switch, no quit arming, no `⌥<digit>` jump.
#[derive(Debug)]
pub enum Modal {
    ActionMenu(ActionMenuState),
    /// The help modal's vertical scroll offset: content taller than the 80%-of-terminal-height cap
    /// scrolls instead of clipping, and this is the row the viewport starts at.
    Help {
        scroll: u16,
    },
}

/// The action menu's captured state: the actions available when `a` was pressed, their
/// algorithm-assigned hotkeys, and the caret. Captured rather than re-derived each frame so a live
/// state change cannot move a row under the caret while the menu is open.
#[derive(Debug)]
pub struct ActionMenuState {
    pub labels: Vec<&'static str>,
    pub hotkeys: Vec<Option<char>>,
    pub selected: usize,
}

/// Top-level app state: which screen is active, whether the 2-step quit is armed, and whether
/// the event loop should keep running.
///
/// # Three screens now drive runs, and every branch that used to name the memories one was decided
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
/// - **The tick drives all three runs and the settings toast** ([`Self::tick`]), because a run
///   keeps running when its tab is not in view, its poll has to keep up with it, and the toast's
///   DANGER lifetime has to elapse with no input in the meantime.
#[derive(Debug)]
pub struct App {
    palette: Palette,
    active: Tab,
    /// Whether a bare `⌥` is held right now, for the jump-key overlay. Driven by the
    /// `KeyCode::Modifier` press/release events the kitty keyboard protocol reports; absent
    /// those (an unsupported terminal) it never leaves `false` and the overlay never renders.
    alt_held: bool,
    quit_armed: bool,
    running: bool,
    /// The open modal, if any. `None` while input goes to the active screen.
    modal: Option<Modal>,
    /// Per-tab activity: a run finishing on a background tab colors that tab's label until it is
    /// visited (cloudy-tui: Tab bar → Tab activity). Indexed by [`Tab::index`], which is why the
    /// array is sized by [`Tab::ALL`].
    activity: [Option<TabActivity>; Tab::ALL.len()],
    overview: Overview,
    memories: Memories,
    chat_media: ChatMedia,
    history: History,
    account: Account,
    settings: Settings,
    /// The terminal height the last frame drew at — the help modal's scroll clamp needs it between
    /// draws, since the key handler has no frame to read a viewport off.
    terminal_height: u16,
}

impl App {
    /// An app with no export behind it: the overview tab active and every screen in its own
    /// pre-read state.
    ///
    /// Every screen starts on [`Environment::default`] rather than on a probe of its own. Nothing
    /// here knows which dir the run is about yet, so a probe would measure a path the user never
    /// named and be thrown away by [`Self::start`] a moment later — which is what it used to do,
    /// twice. The run defaults resolve the same way: nothing given — no flag, no config, no
    /// source — which is [`RunDefaults::resolve`] with an empty source, and `start_with` replaces
    /// every one of those fields before the first frame.
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        let defaults = RunDefaults::resolve(None, &Config::default(), Path::new(""));
        Self {
            palette: Palette::new(tier),
            active: Tab::Overview,
            alt_held: false,
            quit_armed: false,
            running: true,
            modal: None,
            activity: [None; Tab::ALL.len()],
            overview: Overview::unloaded(),
            memories: Memories::with_environment(PathBuf::new(), defaults.out_root.clone(), Environment::default(), defaults.transcode),
            chat_media: ChatMedia::with_environment(
                PathBuf::new(),
                defaults.out_root.clone(),
                Environment::default(),
                defaults.transcode,
                defaults.overlay_mode,
            ),
            history: History::with_environment(PathBuf::new(), defaults.out_root),
            account: Account::with_environment(PathBuf::new()),
            settings: Settings::with_layers(SettingsLayers::defaults_for(tier)),
            terminal_height: 0,
        }
    }

    /// The whole startup composition, and the only thing `main` builds a running app with: read the
    /// source dir once, probe the machine once, settle the run defaults once (decision 66, in
    /// [`RunDefaults::resolve`]), hand every screen the result.
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
    pub fn start(tier: Tier, source: PathBuf, defaults: RunDefaults, layers: SettingsLayers) -> Self {
        Self::start_with(tier, source, defaults, layers, locate)
    }

    /// [`Self::start`] against an explicit locator — the seam that makes the walk count above
    /// observable, since the real [`locate`] cannot be made to report its calls.
    fn start_with(
        tier: Tier, source: PathBuf, defaults: RunDefaults, layers: SettingsLayers, locate: impl Fn(Tool) -> Option<PathBuf>,
    ) -> Self {
        let mut app = Self::new(tier);
        app.settings = Settings::with_layers(layers);
        app.reprobe_with(source, defaults, locate);
        app
    }

    /// The startup composition, run once at launch and again for each path the overview's input
    /// commits: probe the machine once, merge the config's ffmpeg over detection, re-measure the
    /// space at the output root, re-read the overview, and hand every screen the result.
    fn reprobe_with(&mut self, source: PathBuf, defaults: RunDefaults, locate: impl Fn(Tool) -> Option<PathBuf>) {
        let mut environment = Environment::probe_with(locate, &source);
        // The settings ffmpeg row reads the PROBE's own answer as its detection layer, so the
        // answer is captured before the file's path replaces it below: a derivation after the
        // merge would read the merged value as the detection, and a commit would then move the
        // file layer under it while the row kept mis-stating itself.
        let detected_ffmpeg = environment.ffmpeg.clone();
        // The file's `ffmpeg_path` beats detection (decision 66: config > detection). The probe
        // still ran first, so the once-per-tool walk stays counted and `vlc` keeps its own answer;
        // the overview and the media screens then see the same composed machine.
        if let Some(ffmpeg) = &defaults.ffmpeg {
            environment.ffmpeg = Some(ffmpeg.clone());
        }
        let media = environment.measured_at(probe_target(&defaults.out_root));

        self.settings.set_detected_ffmpeg(detected_ffmpeg);
        self.overview = Overview::load_with(&source, environment);
        self.set_source_environment(source, defaults, media);
    }

    /// Re-probes the source dir from the TUI's path input: re-resolves the run defaults against the
    /// new source (its out root is source-derived until `--out` or the file names one), then re-runs
    /// the startup composition. The overview's `enter` on a committed path calls this.
    pub fn reprobe_source(&mut self, source: PathBuf) {
        let defaults = RunDefaults::resolve(self.settings.cli_out(), self.settings.config(), &source);
        self.reprobe_with(source, defaults, locate);
    }

    /// Re-resolves the run screens' values after the settings screen commits a config change.
    ///
    /// [`Self::reprobe_source`] re-resolves the same four keys but re-runs the whole startup
    /// composition — a fresh machine probe and an overview re-read — and rebuilds every screen,
    /// which drops a run in flight. A settings commit changes only the config, so the resolved
    /// defaults (decision 66) are re-derived from the settings screen's own live layers and applied
    /// to the run screens in place: a live run keeps the inputs it captured at start, and the values
    /// the NEXT run reads move to what the settings screen now shows.
    ///
    /// `out_root` and `ffmpeg` always move — the run screens have no control over either. `transcode`
    /// and `overlay` are also live form controls, so each screen's `apply_run_defaults` moves them
    /// only while the user has not overridden them: a commit that changed only `out_dir` or the
    /// theme must not revert a per-run override the user set on a run form.
    fn refresh_run_defaults(&mut self) {
        let out_root = self.settings.effective_out_root();
        let ffmpeg = self.settings.effective_ffmpeg().map(Path::to_path_buf);
        let transcode = self.settings.effective_transcode();
        let overlay = self.settings.effective_overlay();
        self.memories.apply_run_defaults(out_root.clone(), ffmpeg.clone(), transcode);
        self.chat_media.apply_run_defaults(out_root.clone(), ffmpeg, transcode, overlay);
        self.history.apply_out_root(out_root);
    }

    /// Hands the overview screen a real read of the source dir. [`Self::start`] calls this before
    /// the first frame; [`Self::new`] on its own draws the unloaded state.
    #[must_use]
    pub fn with_overview(mut self, overview: Overview) -> Self {
        self.overview = overview;
        self
    }

    /// Hands the run screens their run context: the source dir, the resolved run defaults
    /// ([`RunDefaults`], decision 66) and the machine probe [`Self::start`] already made. The
    /// account screen takes the source alone: it is read-only and writes nothing.
    ///
    /// One call rather than one per screen: the legs read one export and write under one resolved
    /// output root, so a caller handing them different sources or different defaults would be
    /// describing a state that cannot arise from the command line. It is also the seam a render
    /// test uses to pin the disk-free rows without reaching for the real filesystem.
    #[must_use]
    pub fn with_source_environment(mut self, source: PathBuf, defaults: RunDefaults, environment: Environment) -> Self {
        self.set_source_environment(source, defaults, environment);
        self
    }

    /// The body of [`Self::with_source_environment`], shared with the re-probe so the two hand-offs
    /// cannot drift apart.
    fn set_source_environment(&mut self, source: PathBuf, defaults: RunDefaults, environment: Environment) {
        // The settings form's out-dir default derives from the same source the run screens
        // read — one delivery, like every other source consumer.
        self.settings.set_source(source.clone());
        self.memories = Memories::with_environment(source.clone(), defaults.out_root.clone(), environment.clone(), defaults.transcode);
        self.chat_media =
            ChatMedia::with_environment(source.clone(), defaults.out_root.clone(), environment, defaults.transcode, defaults.overlay_mode);
        self.history = History::with_environment(source.clone(), defaults.out_root);
        self.account = Account::with_environment(source);
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

    /// [`Self::with_memories_channel`] for the history screen.
    pub fn with_history_channel(&mut self, receiver: std::sync::mpsc::Receiver<crate::export::history_run::RunEvent>) {
        self.history.with_channel(receiver);
    }

    /// What `--print-source` prints: every screen's own view of the dir this app was launched
    /// against, as `key=value` lines, `\n`-terminated. Machine-first — the flag exists to be read by
    /// something other than a human, so no alignment, no glyphs, no color.
    ///
    /// **Assembled from all five screens, not from one.** [`Self::start`] hands the source to the
    /// overview's read, to the space probe, to the three run screens, and to the account screen,
    /// and those are six separate deliveries of one argument. A report observing only the overview
    /// left the other four able to take `PathBuf::new()` with the whole suite green — measured
    /// 2026-08-11, and it is why the keys below are per-screen rather than one `source=`.
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
        let (history_source, history_out) = self.history.run_paths();
        let account_source = self.account.source();
        format!(
            "{}memories-source={memories_source:?}\nmemories-out={memories_out:?}\nchat-source={chat_source:?}\nchat-out={chat_out:?}\nhistory-source={history_source:?}\nhistory-out={history_out:?}\naccount-source={account_source:?}\n",
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

    #[must_use]
    pub const fn history(&self) -> &History {
        &self.history
    }

    /// The history screen's mutable half, for the same two reasons as [`Self::memories_mut`].
    pub fn history_mut(&mut self) -> &mut History {
        &mut self.history
    }

    #[must_use]
    pub const fn account(&self) -> &Account {
        &self.account
    }

    /// The account screen's mutable half — the shell borrows it to render the stateful list.
    pub fn account_mut(&mut self) -> &mut Account {
        &mut self.account
    }

    /// The settings screen. The shell renders it and reads its toast; `handle_key` routes its
    /// keys from the tab match like every other screen's.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The alert the footer row is showing this frame: the ACTIVE screen's, or none.
    ///
    /// The single source for both the footer and the `x` key, so the row and the dismissal can never
    /// disagree about which of two live alerts is the one on screen. See the type's own docs for why
    /// the active screen wins rather than a priority order across screens.
    #[must_use]
    pub const fn alert(&self) -> Option<&RunAlert> {
        self.screen_alert(self.active)
    }

    /// The run-completion alert a given tab holds, `None` on the screens that never drive one.
    /// [`Self::alert`] is this for the active screen; the tab-activity propagation reads it for
    /// whichever screen a background run belongs to.
    #[must_use]
    pub const fn screen_alert(&self, tab: Tab) -> Option<&RunAlert> {
        match tab {
            Tab::Memories => self.memories.alert(),
            Tab::ChatMedia => self.chat_media.alert(),
            Tab::History => self.history.alert(),
            Tab::Overview | Tab::Account | Tab::Settings => None,
        }
    }

    /// Whether the active screen's read-only pane owns the caret. `false` on every screen that has
    /// no pane to descend into, which is what makes `q` the quit key there.
    #[must_use]
    pub const fn descended(&self) -> bool {
        match self.active {
            Tab::Memories => self.memories.descended(),
            Tab::ChatMedia => self.chat_media.descended(),
            Tab::History => self.history.descended(),
            Tab::Account => self.account.descended(),
            Tab::Overview | Tab::Settings => false,
        }
    }

    /// Whether the ACTIVE screen has a text input mid-edit — the `q`/`x`/`?`/`a` suspension, which
    /// lets those letters type into the field rather than fire their shell meanings. The settings
    /// form and the overview's source-path input are the two text inputs.
    fn editing_text(&self) -> bool {
        match self.active {
            Tab::Settings => self.settings.is_editing(),
            Tab::Overview => self.overview.is_editing(),
            _ => false,
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
            Tab::History => self.history.ascend(),
            Tab::Account => self.account.ascend(),
            Tab::Overview | Tab::Settings => {}
        }
    }

    /// Dismisses the alert the footer is showing, answering whether there was one. The `x` key's
    /// whole job; `x` with nothing showing is inert.
    fn dismiss_alert(&mut self) -> bool {
        match self.active {
            Tab::Memories => self.memories.dismiss_alert(),
            Tab::ChatMedia => self.chat_media.dismiss_alert(),
            Tab::History => self.history.dismiss_alert(),
            Tab::Overview | Tab::Account | Tab::Settings => false,
        }
    }

    /// Makes `tab` the active screen and clears its activity — visiting a tab is what resolves its
    /// tab-activity color (cloudy-tui: Tab bar → Tab activity). The only way tabs change, so the
    /// two cannot drift apart.
    fn switch_to(&mut self, tab: Tab) {
        self.active = tab;
        self.activity[tab.index()] = None;
    }

    #[must_use]
    pub const fn active(&self) -> Tab {
        self.active
    }

    /// The per-tab activity the header colors each inactive label with, indexed by tab-bar
    /// position. `None` for a tab with nothing outstanding; a run finishing on a background tab
    /// fills its slot and visiting the tab clears it.
    #[must_use]
    pub fn activity(&self) -> &[Option<TabActivity>] {
        self.activity.as_slice()
    }

    #[must_use]
    pub const fn is_quit_armed(&self) -> bool {
        self.quit_armed
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// Whether the jump-key overlay renders this frame: a bare `⌥` is held and the stack reported
    /// the hold. Pure state — the overlay is the shell's to draw, this is the one answer both the
    /// header and its tests read.
    #[must_use]
    pub const fn alt_held(&self) -> bool {
        self.alt_held
    }

    /// The modal open this frame, if any — the shell renders it over the finished screen and the
    /// key router gives it every key but `ctrl+c`.
    #[must_use]
    pub const fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    /// Whether the ACTIVE screen's action menu would list anything (cloudy-tui: Action menu). The
    /// hint bar's `a actions` group derives from this single answer, so it never advertises a key
    /// that opens nothing; the help modal's `GLOBAL` section lists `a actions` unconditionally.
    #[must_use]
    pub fn has_actions(&self) -> bool {
        match self.active {
            Tab::Memories => !self.memories.actions().is_empty(),
            Tab::ChatMedia => !self.chat_media.actions().is_empty(),
            Tab::History => !self.history.actions().is_empty(),
            Tab::Overview | Tab::Account | Tab::Settings => false,
        }
    }

    /// The help modal's sections for this frame (cloudy-tui: Help modal): the `GLOBAL` universal
    /// keys (`q`/`?`/`a` unconditionally, per the spec), then the active screen's own bound keys
    /// named after that screen. Rebuilt each frame, so it tracks pane focus exactly like the hint bar.
    #[must_use]
    pub fn help_sections(&self) -> Vec<HelpSection<'static>> {
        let mut global: Vec<(&'static str, &'static str)> = vec![("q", "back / quit"), ("?", "help"), ("a", "actions")];
        global.extend([("← →", "switch tab"), ("⌃c", "quit")]);

        let mut sections = vec![HelpSection { title: "global", rows: global }];
        let screen = match self.active {
            Tab::Memories => self.memories.help_keys(),
            Tab::ChatMedia => self.chat_media.help_keys(),
            Tab::History => self.history.help_keys(),
            Tab::Account => self.account.help_keys(),
            Tab::Settings => self.settings.help_keys(),
            Tab::Overview => Vec::new(),
        };
        if !screen.is_empty() {
            sections.push(HelpSection { title: self.active.label(), rows: screen });
        }
        sections
    }

    /// The help modal's maximum scroll offset this frame, in rows: its content lines minus the
    /// viewport the modal shell gives at the last-drawn terminal height. [`crate::tui::widgets::help_scroll_max`]
    /// holds the arithmetic, shared with the render so the two cannot drift.
    fn help_max_scroll(&self) -> u16 {
        let lines = widgets::help_line_count(&self.help_sections());
        u16::try_from(widgets::help_scroll_max(lines, self.terminal_height)).unwrap_or(u16::MAX)
    }

    /// Draws, waits for an event, applies it, ticks, repeats.
    ///
    /// While ANY screen's run is live — or while the settings toast is — the wait is capped at
    /// one tick (80 ms), so the spinner animates, the per-item poll refreshes without input, and
    /// the toast's DANGER lifetime elapses; with nothing live there is nothing to animate, and
    /// the loop blocks on input instead of redrawing an identical frame every 80 ms forever. The
    /// gate is a disjunction rather than "the active screen's run" because a run keeps running
    /// when its tab is not in view, and a poll that stopped there would leave its table frozen at
    /// whatever the user last saw.
    ///
    /// # Errors
    ///
    /// Returns the backend's error if drawing to the terminal or reading an event fails.
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        // Probe the kitty keyboard protocol before the loop (best-effort): a bare `⌥` hold only
        // reaches us when the terminal reports modifier-key events AND this driver asks for
        // release events, so push both flags and pop them after. A terminal that ignores the push
        // never reports the hold, `alt_held` stays `false`, and the overlay simply never renders;
        // `⌥<digit>` still jumps either way, since that chord also arrives via the legacy escape
        // prefix. The push and pop are ignored-on-error: an unsupported terminal (Windows, a
        // legacy emulator) is not a reason to refuse to run.
        let _ = execute!(std::io::stdout(), PushKeyboardEnhancementFlags(KITTY_KEYBOARD_FLAGS));
        let outcome = self.run_loop(terminal);
        // Popped on every exit path, so the flags never outlive the terminal takeover.
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        outcome
    }

    fn run_loop(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while self.running {
            // The help modal's scroll clamp reads the terminal height between draws; `size` is the
            // same figure the next draw will lay out against.
            self.terminal_height = terminal.size()?.height;
            {
                let app = &mut *self;
                terminal.draw(|frame| shell::render(frame, app))?;
            }
            if self.memories.run_in_flight()
                || self.chat_media.run_in_flight()
                || self.history.run_in_flight()
                || self.settings.toast_live()
            {
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

    /// One timer tick: pump each run's channel, poll its statuses, advance its spinner, age the
    /// settings toast. A screen with no run in flight returns immediately, so ticking all three
    /// costs nothing when one is idle — and the toast ages only while the gate above says it is
    /// live.
    pub fn tick(&mut self) {
        let before = self.alert_presence();
        self.memories.tick();
        self.chat_media.tick();
        self.history.tick();
        self.settings.tick();
        self.record_activity(before);
    }

    /// Which run screens held a completion alert before a tick, in [`Tab`] order. The propagation
    /// below compares this to the after state, so it records a run's finish exactly once — the
    /// alert persists until dismissed or the next run, so presence alone would re-color a tab the
    /// user already visited and left again.
    fn alert_presence(&self) -> [bool; 3] {
        [self.memories.alert().is_some(), self.chat_media.alert().is_some(), self.history.alert().is_some()]
    }

    /// Colors a background tab's label when its run finishes there (cloudy-tui: Tab bar → Tab
    /// activity). A screen whose alert appeared between `before` and now, on a tab the user is not
    /// looking at, sets that tab's activity from the alert's kind. A run that finishes on the
    /// active tab needs no cue — its footer alert is already on the row — and a run that finished
    /// on an earlier tick has no new edge, so it is not re-recorded after a visit.
    fn record_activity(&mut self, before: [bool; 3]) {
        for (index, tab) in [Tab::Memories, Tab::ChatMedia, Tab::History].into_iter().enumerate() {
            let activity = self.screen_alert(tab).map(RunAlert::activity);
            if !before[index]
                && tab != self.active
                && let Some(activity) = activity
            {
                self.activity[tab.index()] = Some(activity);
            }
        }
    }

    /// Applies one terminal event. Resizes need no state change — the next draw reads the new
    /// size straight off the frame.
    pub fn handle_event(&mut self, event: &Event) {
        if let Event::Key(key) = event {
            // A bare `⌥` hold/release is modifier state, not an app key: it feeds the
            // jump-key overlay. Handled here and only here, so it neither disarms the quit nor
            // reaches a screen's key handler. Repeat counts as held. A release missed by a
            // focus steal leaves the overlay up until the next press/release pair self-heals
            // it; there is deliberately no `FocusLost` clearing, because crossterm emits that
            // event only when focus reporting (CSI ? 1004 h) is enabled and this loop arms no
            // such capability, so a `FocusLost` arm would be dead code reading as a working
            // safety net.
            if matches!(key.code, KeyCode::Modifier(ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt)) {
                self.alt_held = key.kind != KeyEventKind::Release;
                return;
            }
            // crossterm reports Press *and* Release on Windows, so the Release half must be
            // ignored or every binding fires twice there. Repeat is forwarded: with the kitty
            // protocol's REPORT_EVENT_TYPES pushed, an auto-repeated key arrives as Repeat
            // rather than a fresh Press, so dropping it would leave holding ←/→/↑/↓ advancing
            // a single step instead of repeating.
            if key.kind != KeyEventKind::Release {
                self.handle_key(*key);
            }
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

        // `x` dismisses whatever the frame is showing: the settings toast floats over every tab,
        // so it goes first, then the run-completion footer alert the row is actually showing.
        // With nothing live it is a key like any other, and either way it disarms an armed quit —
        // a dismissal is a key press like any other, so the dismissing branch disarms before its
        // early return rather than falling through to the shared disarm below. While a text input
        // is being edited it is a letter the field types — the dismissal keys are suspended exactly
        // like `q`. The dismissal stays live while a modal owns input: a toast or footer alert can
        // be live beneath an open action menu or help modal, and `x` must still reach them
        // (cloudy-tui: Dismissal precedence) without disturbing the modal's own keys below.
        if matches!(key.code, KeyCode::Char('x' | 'X')) && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            let editing_input = self.editing_text();
            if (!editing_input && self.settings.dismiss_toast()) || self.dismiss_alert() {
                self.quit_armed = false;
                return;
            }
        }

        // A modal owns the rest of the input: while one is open, every other key is the modal's —
        // no `q` arming, no screen routing, no tab switch, no `⌥` jump (cloudy-tui: Modals →
        // Focus). `x` is the one exception, handled above so it stays live.
        if self.modal.is_some() {
            self.handle_modal_key(key);
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
            // While a text input is being edited (settings or the overview's path input), `q` is a
            // letter the field types — the same suspension a descended pane gets, without the
            // ascend step because neither screen has a pane. The screen must receive the key, so
            // this falls through.
            let editing_input = self.editing_text();
            if !editing_input {
                // `q` at the top level arms a 2-step quit and never quits in one press. Hotkeys are
                // case-insensitive, so caps lock can't strand a user with no way out.
                if self.quit_armed {
                    self.running = false;
                } else {
                    self.quit_armed = true;
                }
                return;
            }
        }

        self.quit_armed = false;

        // `?` opens the help modal and `a` the action menu (cloudy-tui: Action menu; Help modal).
        // Both are suspended while a text input is being edited, where they are letters the field
        // types — the same suspension `q` and `x` get. `a` with no actions on the active screen is
        // inert, so it opens nothing and the hint bar derives its hint from that.
        let editing_input = self.editing_text();
        if !editing_input {
            if matches!(key.code, KeyCode::Char('?')) && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                self.modal = Some(Modal::Help { scroll: 0 });
                return;
            }
            if matches!(key.code, KeyCode::Char('a' | 'A')) && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                if let Some(menu) = self.open_action_menu() {
                    self.modal = Some(Modal::ActionMenu(menu));
                }
                return;
            }
        }

        // Screen-owned keys on whichever screen is active (form rows, table scroll,
        // descend/ascend). A screen answers `false` for keys that belong to the shell, so the tab
        // switching below still works when a form owns the caret.
        let consumed = match self.active {
            Tab::Memories => self.memories.handle_key(key),
            Tab::ChatMedia => self.chat_media.handle_key(key),
            Tab::History => self.history.handle_key(key),
            Tab::Account => self.account.handle_key(key),
            Tab::Settings => {
                let consumed = self.settings.handle_key(key);
                if self.settings.take_config_commit() {
                    self.refresh_run_defaults();
                }
                consumed
            }
            Tab::Overview => match self.overview.handle_key(key) {
                OverviewKey::Reprobbed(path) => {
                    self.reprobe_source(path);
                    true
                }
                OverviewKey::Handled => true,
                OverviewKey::Unhandled => false,
            },
        };
        if consumed {
            return;
        }

        match key.code {
            KeyCode::Left if key.modifiers == KeyModifiers::NONE => {
                self.switch_to(self.active.previous());
            }
            KeyCode::Right if key.modifiers == KeyModifiers::NONE => {
                self.switch_to(self.active.next());
            }
            KeyCode::Char(c) if key.modifiers == KeyModifiers::ALT => {
                if let Some(tab) = c.to_digit(10).and_then(Tab::from_jump_digit) {
                    // A jump ascends the pane it is LEAVING (cloudy-tui: the jump ascends
                    // implicitly), and moving focus away disarms the quit like any other key.
                    // Ascending every screen instead would reset a pane the user is not on.
                    self.ascend_active();
                    self.switch_to(tab);
                }
            }
            _ => {}
        }
    }

    /// Captures the ACTIVE screen's actions into a menu, or `None` when it has none (`a` is inert
    /// there). The hotkeys are assigned once at open so they cannot shift while the menu is up.
    fn open_action_menu(&self) -> Option<ActionMenuState> {
        let labels: Vec<&'static str> = match self.active {
            Tab::Memories => self.memories.actions(),
            Tab::ChatMedia => self.chat_media.actions(),
            Tab::History => self.history.actions(),
            Tab::Overview | Tab::Account | Tab::Settings => return None,
        };
        if labels.is_empty() {
            return None;
        }
        let hotkeys = widgets::assign_hotkeys(&labels);
        Some(ActionMenuState { labels, hotkeys, selected: 0 })
    }

    /// Runs a picked action on the ACTIVE screen. The label is one the menu's own [`Self::open_action_menu`]
    /// captured, so it matches a screen's `actions()` entry by construction.
    fn dispatch_action(&mut self, label: &'static str) {
        match self.active {
            Tab::Memories => self.memories.run_action(label),
            Tab::ChatMedia => self.chat_media.run_action(label),
            Tab::History => self.history.run_action(label),
            Tab::Overview | Tab::Account | Tab::Settings => {}
        }
    }

    /// One key while a modal is open. The action menu wraps its caret and picks on `↵` or an
    /// assigned hotkey; both modals close on `esc`/`q`, and help also closes on `?`. Everything
    /// else is inert.
    fn handle_modal_key(&mut self, key: KeyEvent) {
        enum Outcome {
            Keep,
            Close,
            Pick(&'static str),
        }

        // The help modal's scroll clamp is read before the modal is borrowed mutably, so the arm
        // can adjust the offset without a second borrow of `self`.
        let help_max = self.help_max_scroll();
        let outcome = match &mut self.modal {
            Some(Modal::ActionMenu(menu)) => match key.code {
                KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                    let len = menu.labels.len();
                    if len > 0 {
                        let delta: isize = if key.code == KeyCode::Up { -1 } else { 1 };
                        let current = menu.selected as isize;
                        menu.selected = (current + delta).rem_euclid(len as isize) as usize;
                    }
                    Outcome::Keep
                }
                KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                    menu.labels.get(menu.selected).copied().map_or(Outcome::Keep, Outcome::Pick)
                }
                KeyCode::Esc => Outcome::Close,
                KeyCode::Char('q' | 'Q') if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => Outcome::Close,
                KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                    let lower = c.to_ascii_lowercase();
                    menu.hotkeys
                        .iter()
                        .position(|hotkey| *hotkey == Some(lower))
                        .and_then(|index| menu.labels.get(index).copied())
                        .map_or(Outcome::Keep, Outcome::Pick)
                }
                _ => Outcome::Keep,
            },
            Some(Modal::Help { scroll }) => match key.code {
                KeyCode::Up | KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                    // Clamp against the viewport the next draw will use, so scrolling past the
                    // bottom cannot leave the offset stuck beyond the real maximum — the render
                    // re-clamps for display, but the state must not grow past it or `↑` would need
                    // a press per stale cell before the view moved again.
                    *scroll = if key.code == KeyCode::Up { (*scroll).saturating_sub(1) } else { (*scroll).saturating_add(1).min(help_max) };
                    Outcome::Keep
                }
                KeyCode::Esc | KeyCode::Char('q' | 'Q') | KeyCode::Char('?')
                    if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
                {
                    Outcome::Close
                }
                _ => Outcome::Keep,
            },
            None => Outcome::Keep,
        };

        match outcome {
            Outcome::Close => self.modal = None,
            Outcome::Pick(label) => {
                self.modal = None;
                self.dispatch_action(label);
            }
            Outcome::Keep => {}
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
        // which leaves the probes as the only thing this drives. An empty config leaves every key
        // to its default, so `resolve` cannot shadow the locator's answers.
        let app = App::start_with(
            Tier::Full,
            PathBuf::from("/nope"),
            RunDefaults::resolve(None, &Config::default(), Path::new("/nope")),
            SettingsLayers::defaults_for(Tier::Full),
            |tool| {
                walks.borrow_mut().push(tool);
                Some(PathBuf::from(format!("/located/{}", tool.command())))
            },
        );

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
        // measured at all rather than by a byte count no machine agrees on. "Measured at all"
        // answers differently per OS: on unix the probe of an absent dir fails, and on windows the
        // probe measures the volume the path names (`GetVolumePathNameW` climbs to the drive root
        // even when the dir itself is not there), so the figure is the drive's rather than `None`.
        let overview_space = app.overview().environment().available_space;
        if cfg!(windows) {
            assert!(overview_space.is_some(), "the overview measures the volume the source dir names, which exists");
        } else {
            assert_eq!(overview_space, None, "the overview measures the source dir, which is absent");
        }
        assert!(app.memories().environment().available_space.is_some(), "the media screens measure the output root");
        assert!(app.chat_media().environment().available_space.is_some(), "the media screens measure the output root");
    }

    #[test]
    fn a_config_ffmpeg_path_beats_the_probe() {
        // The config's answer must win over where the locator found the tool, and every screen
        // must see the same winner — the overview's machine panel included. The probe still runs,
        // so the walk count stays once per tool (the test above pins it).
        let walks = RefCell::new(Vec::new());
        let defaults = RunDefaults {
            out_root: PathBuf::from("/nope/exportsnap-out"),
            ffmpeg: Some(PathBuf::from("/usr/bin/ffmpeg")),
            transcode: true,
            overlay_mode: OverlayMode::Both,
        };
        let app = App::start_with(Tier::Full, PathBuf::from("/nope"), defaults, SettingsLayers::defaults_for(Tier::Full), |tool| {
            walks.borrow_mut().push(tool);
            Some(PathBuf::from(format!("/probed/{}", tool.command())))
        });

        for environment in [app.overview().environment(), app.memories().environment(), app.chat_media().environment()] {
            assert_eq!(
                environment.ffmpeg.as_deref(),
                Some(Path::new("/usr/bin/ffmpeg")),
                "the config's ffmpeg_path must beat the probe on every screen"
            );
        }
        assert_eq!(walks.into_inner().len(), Tool::ALL.len(), "the probe still runs once per tool");
    }

    #[test]
    fn a_reprobe_reads_the_settings_screens_live_config() {
        // A config change committed on the settings screen must reach the run screens on the next
        // re-probe, not be dropped in favour of a launch snapshot. `App` keeps no duplicate of the
        // config, so the re-probe resolves defaults from the screen's own live layers.
        let config_dir = tempfile::TempDir::new().unwrap();
        let layers = SettingsLayers { config_dir: Some(config_dir.path().to_path_buf()), ..SettingsLayers::defaults_for(Tier::Full) };
        let mut app = App::start_with(
            Tier::Full,
            PathBuf::from("/nope"),
            RunDefaults::resolve(None, &Config::default(), Path::new("/nope")),
            layers,
            |_| None,
        );

        // Commit an out_dir through the settings form's own write path, exactly as the user
        // would: the output-dir row is the form's first, so `enter` opens it, the letters fill
        // the draft, and `enter` commits through `config::write`.
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.settings.handle_key(key(KeyCode::Enter));
        for ch in "/committed/out".chars() {
            app.settings.handle_key(key(KeyCode::Char(ch)));
        }
        app.settings.handle_key(key(KeyCode::Enter));

        app.reprobe_source(PathBuf::from("/nope"));

        let (_, out) = app.memories().run_paths();
        assert_eq!(out, Path::new("/committed/out"), "the committed out_dir must reach the run screens");
    }

    #[test]
    fn a_committed_setting_reaches_the_run_screens_without_a_reprobe() {
        // The bug: a setting committed on the settings tab never reached the already-built run
        // screens, so the run kept the value it resolved at startup. Driving the app's own key
        // routing must re-resolve the run screens from the settings screen's live config, with no
        // manual `reprobe_source` call and no run-state rebuild in between.
        let config_dir = tempfile::TempDir::new().unwrap();
        let layers = SettingsLayers { config_dir: Some(config_dir.path().to_path_buf()), ..SettingsLayers::defaults_for(Tier::Full) };
        let mut app = App::start_with(
            Tier::Full,
            PathBuf::from("/nope"),
            RunDefaults::resolve(None, &Config::default(), Path::new("/nope")),
            layers,
            |_| None,
        );
        app.switch_to(Tab::Settings);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // out_dir: the form opens focused on the output-dir row.
        app.handle_key(key(KeyCode::Enter));
        for ch in "/committed/out".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Enter));

        // transcode: three Downs reach the transcode row (past theme and ffmpeg); enter flips it.
        for _ in 0..3 {
            app.handle_key(key(KeyCode::Down));
        }
        app.handle_key(key(KeyCode::Enter));

        // overlay mode: one Down more, enter cycles both -> originals.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));

        // ffmpeg path: two Ups back to the ffmpeg row, then edit and commit.
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Enter));
        for ch in "/committed/ffmpeg".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.memories().run_paths().1, Path::new("/committed/out"), "out_dir reaches the memories root");
        assert_eq!(app.chat_media().run_paths().1, Path::new("/committed/out"), "out_dir reaches the chat root");
        assert_eq!(app.history().run_paths().1, Path::new("/committed/out"), "out_dir reaches the history root");
        assert!(!app.memories().is_transcode_on(), "transcode reaches the memories screen");
        assert!(!app.chat_media().is_transcode_on(), "transcode reaches the chat screen");
        assert_eq!(app.chat_media().overlay_mode(), OverlayMode::Originals, "overlay mode reaches the chat screen");
        assert_eq!(
            app.memories().environment().ffmpeg.as_deref(),
            Some(Path::new("/committed/ffmpeg")),
            "ffmpeg path reaches the memories screen"
        );
        assert_eq!(
            app.chat_media().environment().ffmpeg.as_deref(),
            Some(Path::new("/committed/ffmpeg")),
            "ffmpeg path reaches the chat screen"
        );
    }

    #[test]
    fn a_settings_commit_preserves_a_per_run_form_override() {
        // The bug: `refresh_run_defaults` treated a resolved default and a live form value as the
        // same thing, so a settings commit that changed only out_dir reverted a per-run override the
        // user had set on a run form. `out_root` and `ffmpeg` have no per-run control and must move;
        // `transcode` and `overlay` are live form controls, so a commit must not move one the user
        // already flipped. Drive the app's own key routing end to end, with no manual re-probe.
        let config_dir = tempfile::TempDir::new().unwrap();
        let layers = SettingsLayers { config_dir: Some(config_dir.path().to_path_buf()), ..SettingsLayers::defaults_for(Tier::Full) };
        let mut app = App::start_with(
            Tier::Full,
            PathBuf::from("/nope"),
            RunDefaults::resolve(None, &Config::default(), Path::new("/nope")),
            layers,
            |_| None,
        );
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // The memories form opens focused on the transcode row, so a bare space flips it off and
        // marks it as the user's override.
        app.switch_to(Tab::Memories);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(!app.memories().is_transcode_on(), "space flips the memories toggle off");

        // The chat form opens focused on the overlay row, so space cycles both -> originals and
        // marks that as the user's override.
        app.switch_to(Tab::ChatMedia);
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.chat_media().overlay_mode(), OverlayMode::Originals, "space cycles the chat overlay to originals");

        // Commit only out_dir on the settings tab, exactly the case that used to clobber the two
        // overrides above.
        app.switch_to(Tab::Settings);
        app.handle_key(key(KeyCode::Enter));
        for ch in "/committed/out".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Enter));

        // The out root moves, and the two per-run overrides survive.
        assert_eq!(app.memories().run_paths().1, Path::new("/committed/out"), "out_dir reaches the memories root");
        assert!(!app.memories().is_transcode_on(), "the per-run transcode override survives an out_dir commit");
        assert_eq!(app.chat_media().overlay_mode(), OverlayMode::Originals, "the per-run overlay override survives an out_dir commit");
    }
}
