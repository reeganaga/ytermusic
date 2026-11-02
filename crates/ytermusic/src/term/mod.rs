pub mod chrome;
pub mod device_lost;
pub mod library;
pub mod lyrics;
pub mod playlist_view;
pub mod queue;
pub mod search;
pub mod sidebar;
#[cfg(feature = "thumbnails")]
pub mod thumbnail;
pub mod widgets;

use std::{
    io::{self},
    time::{Duration, Instant},
};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use flume::{Receiver, Sender};
use ratatui::{backend::CrosstermBackend, buffer::Buffer, layout::Rect, Frame, Terminal};

use common_structs::MusicDownloadStatus;

use crate::{
    is_shutdown_sent, shutdown, structures::sound_action::SoundAction,
    systems::player::PlayerState, systems::DOWNLOAD_MANAGER,
};

use self::{
    chrome::{Button, FrameLayout},
    device_lost::DeviceLost,
    library::{Library, PlaylistEntry},
    lyrics::LyricsView,
    playlist_view::PlaylistView,
    queue::QueueView,
    search::Search,
    sidebar::{NavItem, Sidebar},
    widgets::{hint, Hint},
};

/// What a screen can read while handling events or rendering.
pub struct Ctx<'a> {
    pub player: &'a PlayerState,
    /// Whether the main pane has the keyboard focus (not the sidebar)
    pub focused: bool,
    /// Narrow terminal layout (< 80 columns)
    pub narrow: bool,
}

// A trait to handle the different screens
pub trait Screen {
    /// Returns `None` when the key is not used by the screen, so global keys apply.
    fn on_key(&mut self, key: KeyEvent, ctx: &Ctx) -> Option<EventResponse>;
    fn on_mouse(&mut self, mouse_event: MouseEvent, area: Rect, ctx: &Ctx) -> EventResponse;
    /// Draws the screen in the main area (right of the sidebar).
    fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &Ctx);
    fn handle_global_message(&mut self, _message: ManagerMessage) -> EventResponse {
        EventResponse::None
    }
    fn hints(&self, ctx: &Ctx) -> Vec<Hint>;
    fn open(&mut self) {}
    /// Whether an animation is running: the screen is then redrawn at
    /// `ANIMATION_FRAME` instead of the normal tick.
    fn animating(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
pub enum EventResponse {
    Message(Vec<ManagerMessage>),
    None,
}

// A message that can be sent to the manager
#[derive(Debug, Clone)]
pub enum ManagerMessage {
    Error(String, Box<Option<ManagerMessage>>),
    PassTo(Screens, Box<ManagerMessage>),
    /// Open the playlist viewer on this playlist
    Inspect(PlaylistEntry),
    ChangeState(Screens),
    RestartPlayer,
    Quit,
    AddPlaylist(PlaylistEntry),
    /// Short message shown in the status line for a few seconds
    Notify(String),
}

impl ManagerMessage {
    pub fn pass_to(self, screen: Screens) -> Self {
        Self::PassTo(screen, Box::new(self))
    }
    pub fn event(self) -> EventResponse {
        EventResponse::Message(vec![self])
    }
}

// The different screens
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Screens {
    Player = 0x0,
    Library = 0x1,
    Search = 0x2,
    DeviceLost = 0x3,
    PlaylistViewer = 0x4,
}

const NOTICE_DURATION: Duration = Duration::from_secs(4);
/// Redraw interval while a screen animates (~30 fps)
const ANIMATION_FRAME: Duration = Duration::from_millis(33);

// The screen manager that handles the different screens
pub struct Manager {
    player: PlayerState,
    queue: QueueView,
    lyrics: LyricsView,
    /// The player screen shows the lyrics instead of the queue
    show_lyrics: bool,
    library: Library,
    search: Search,
    viewer: PlaylistView,
    device_lost: DeviceLost,
    current: Screens,
    history: Vec<Screens>,
    sidebar: Sidebar,
    sidebar_visible: bool,
    focus_sidebar: bool,
    layout: FrameLayout,
    notice: Option<(String, Instant)>,
    #[cfg(feature = "thumbnails")]
    thumbnail: thumbnail::Thumbnail,
}

impl Manager {
    pub async fn new(action_sender: Sender<SoundAction>, player: PlayerState) -> Self {
        let updater = player.updater.clone();
        let search = Search::new(action_sender.clone(), updater).await;
        Self {
            queue: QueueView::new(action_sender.clone()),
            lyrics: LyricsView::new(search.api()),
            show_lyrics: false,
            library: Library::new(),
            viewer: PlaylistView::new(action_sender),
            search,
            device_lost: DeviceLost(Vec::new(), None),
            current: Screens::Player,
            history: Vec::new(),
            sidebar: Sidebar::default(),
            sidebar_visible: true,
            focus_sidebar: false,
            layout: FrameLayout::default(),
            notice: None,
            #[cfg(feature = "thumbnails")]
            thumbnail: thumbnail::Thumbnail::new(),
            player,
        }
    }

    fn screen_mut(&mut self, screen: Screens) -> &mut dyn Screen {
        match screen {
            Screens::Player if self.show_lyrics => &mut self.lyrics,
            Screens::Player => &mut self.queue,
            Screens::Library => &mut self.library,
            Screens::Search => &mut self.search,
            Screens::DeviceLost => &mut self.device_lost,
            Screens::PlaylistViewer => &mut self.viewer,
        }
    }

    /// Runs `f` on the current screen with a context borrowing the player.
    fn with_screen<R>(&mut self, f: impl FnOnce(&mut dyn Screen, &Ctx) -> R) -> R {
        let ctx = Ctx {
            player: &self.player,
            focused: !self.focus_sidebar,
            narrow: self.layout.narrow,
        };
        let screen: &mut dyn Screen = match self.current {
            Screens::Player if self.show_lyrics => &mut self.lyrics,
            Screens::Player => &mut self.queue,
            Screens::Library => &mut self.library,
            Screens::Search => &mut self.search,
            Screens::DeviceLost => &mut self.device_lost,
            Screens::PlaylistViewer => &mut self.viewer,
        };
        f(screen, &ctx)
    }

    fn set_current_screen(&mut self, screen: Screens, remember: bool) {
        if screen == self.current {
            return;
        }
        if remember && self.current != Screens::DeviceLost {
            self.history.push(self.current);
            if self.history.len() > 32 {
                self.history.remove(0);
            }
        }
        if self.current == Screens::DeviceLost {
            self.device_lost.0.clear();
        }
        self.current = screen;
        self.focus_sidebar = false;
        self.screen_mut(screen).open();
    }

    fn go_back(&mut self) {
        let previous = self.history.pop().unwrap_or(Screens::Player);
        self.set_current_screen(previous, false);
    }

    pub fn handle_event(&mut self, event: EventResponse) -> bool {
        match event {
            EventResponse::Message(messages) => {
                for message in messages {
                    if self.handle_manager_message(message) {
                        return true;
                    }
                }
            }
            EventResponse::None => {}
        }
        false
    }

    pub fn handle_manager_message(&mut self, e: ManagerMessage) -> bool {
        match e {
            ManagerMessage::PassTo(e, a) => {
                let rs = self.screen_mut(e).handle_global_message(*a);
                return self.handle_event(rs);
            }
            ManagerMessage::Quit => return true,
            ManagerMessage::ChangeState(e) => self.set_current_screen(e, true),
            ManagerMessage::Inspect(entry) => {
                self.viewer.show(entry);
                self.set_current_screen(Screens::PlaylistViewer, true);
            }
            ManagerMessage::AddPlaylist(entry) => self.library.add(entry),
            ManagerMessage::RestartPlayer => {
                SoundAction::RestartPlayer.apply_sound_action(&mut self.player);
                self.set_current_screen(Screens::Player, false);
            }
            ManagerMessage::Notify(text) => self.notice = Some((text, Instant::now())),
            e @ ManagerMessage::Error(..) => {
                return self.handle_manager_message(e.pass_to(Screens::DeviceLost));
            }
        }
        false
    }

    fn apply(&mut self, action: SoundAction) {
        action.apply_sound_action(&mut self.player);
    }

    /// Keys available everywhere (unless the current screen used the key).
    fn on_global_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('1') => self.set_current_screen(Screens::Player, true),
            KeyCode::Char('2') => self.set_current_screen(Screens::Library, true),
            KeyCode::Char('3') | KeyCode::Char('/') | KeyCode::Char('f') => {
                self.set_current_screen(Screens::Search, true);
                self.search.focus_input();
            }
            KeyCode::Char('y') => {
                if self.current == Screens::Player {
                    self.show_lyrics = !self.show_lyrics;
                } else {
                    self.show_lyrics = true;
                    self.set_current_screen(Screens::Player, true);
                }
            }
            KeyCode::Char('s') => {
                self.sidebar_visible = !self.sidebar_visible;
                self.focus_sidebar &= self.sidebar_visible;
            }
            KeyCode::Tab | KeyCode::BackTab if self.sidebar_visible => {
                self.focus_sidebar = !self.focus_sidebar;
            }
            KeyCode::Backspace | KeyCode::Esc => self.go_back(),
            KeyCode::Char(' ') => self.apply(SoundAction::PlayPause),
            KeyCode::Char('x') => self.apply(SoundAction::ToggleShuffle),
            KeyCode::Char('r') => self.apply(SoundAction::CycleRepeat),
            KeyCode::Char('+') | KeyCode::Char('=') => self.apply(SoundAction::Plus),
            KeyCode::Char('-') => self.apply(SoundAction::Minus),
            KeyCode::Left | KeyCode::Char('<') | KeyCode::Char('h') => self.apply(if ctrl {
                SoundAction::Previous(1)
            } else {
                SoundAction::Backward
            }),
            KeyCode::Right | KeyCode::Char('>') | KeyCode::Char('l') => self.apply(if ctrl {
                SoundAction::Next(1)
            } else {
                SoundAction::Forward
            }),
            KeyCode::F(5) => self.retry_failed_downloads(),
            _ => {}
        }
        false
    }

    fn retry_failed_downloads(&mut self) {
        let mut musics = Vec::new();
        for (key, status) in self.player.music_status.iter_mut() {
            if *status != MusicDownloadStatus::DownloadFailed {
                continue;
            }
            if let Some(e) = self.player.list.iter().find(|x| &x.video_id == key) {
                musics.push(e.clone());
                *status = MusicDownloadStatus::NotDownloaded;
            }
        }
        DOWNLOAD_MANAGER.add_to_download_list(musics);
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        if (key.code == KeyCode::Char('c') || key.code == KeyCode::Char('d'))
            && key.modifiers == KeyModifiers::CONTROL
        {
            return true;
        }
        if self.current == Screens::DeviceLost {
            let response = self.with_screen(|s, ctx| s.on_key(key, ctx));
            return response.is_some_and(|r| self.handle_event(r));
        }
        if self.focus_sidebar && self.sidebar_visible {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.sidebar.move_cursor(&self.library, -1);
                    return false;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.sidebar.move_cursor(&self.library, 1);
                    return false;
                }
                KeyCode::Enter => {
                    let item = self.sidebar.cursor_item(&self.library);
                    return item.is_some_and(|item| self.activate_nav(item));
                }
                _ => {}
            }
        } else if let Some(response) = self.with_screen(|s, ctx| s.on_key(key, ctx)) {
            return self.handle_event(response);
        }
        self.on_global_key(key)
    }

    fn activate_nav(&mut self, item: NavItem) -> bool {
        let keep_focus = self.focus_sidebar;
        let message = match item {
            NavItem::Home => ManagerMessage::ChangeState(Screens::Player),
            NavItem::Search => {
                self.search.focus_input();
                ManagerMessage::ChangeState(Screens::Search)
            }
            NavItem::Liked => match self.library.liked() {
                Some(entry) => ManagerMessage::Inspect(entry.clone()),
                None => ManagerMessage::Notify("Liked songs are still loading…".to_owned()),
            },
            NavItem::Playlist(index) => match self.library.entries.get(index) {
                Some(entry) => ManagerMessage::Inspect(entry.clone()),
                None => return false,
            },
            NavItem::Downloaded => ManagerMessage::Inspect(PlaylistEntry::downloaded()),
        };
        let quit = self.handle_manager_message(message);
        self.focus_sidebar = keep_focus && item != NavItem::Search;
        quit
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> bool {
        let (x, y) = (mouse.column, mouse.row);
        let inside = |r: Rect| r.contains((x, y).into());
        let layout = self.layout.clone();
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            if let Some(index) = layout.tabs.iter().position(|r| inside(*r)) {
                let screen = [Screens::Player, Screens::Library, Screens::Search][index];
                self.set_current_screen(screen, true);
                return false;
            }
            if let Some(sidebar) = layout.sidebar.filter(|r| inside(*r)) {
                if let Some(item) = self.sidebar.click(&self.library, y, sidebar) {
                    return self.activate_nav(item);
                }
                return false;
            }
            if let Some((_, button)) = layout.buttons.iter().find(|(r, _)| inside(*r)) {
                self.apply(match button {
                    Button::Shuffle => SoundAction::ToggleShuffle,
                    Button::Previous => SoundAction::Previous(1),
                    Button::PlayPause => SoundAction::PlayPause,
                    Button::Next => SoundAction::Next(1),
                    Button::Repeat => SoundAction::CycleRepeat,
                });
                return false;
            }
            if inside(layout.progress) && layout.progress.width > 0 {
                let ratio = (x - layout.progress.x) as f64 / layout.progress.width as f64;
                if let Some(duration) = self.player.sink.duration() {
                    self.apply(SoundAction::SeekTo(Duration::from_secs_f64(
                        duration * ratio,
                    )));
                }
                return false;
            }
            if inside(layout.volume) && layout.volume.width > 0 {
                let cells = layout.volume.width as f64;
                let ratio = ((x - layout.volume.x) as f64 + 1.0) / cells;
                self.player.sink.set_volume((ratio * 100.0).round() as i32);
                return false;
            }
        }
        if layout.sidebar.is_some_and(inside) {
            match mouse.kind {
                MouseEventKind::ScrollUp => self.sidebar.move_cursor(&self.library, -1),
                MouseEventKind::ScrollDown => self.sidebar.move_cursor(&self.library, 1),
                _ => {}
            }
            return false;
        }
        if inside(layout.main) {
            if let MouseEventKind::Down(_) = mouse.kind {
                self.focus_sidebar = false;
            }
            let response = self.with_screen(|s, ctx| s.on_mouse(mouse, layout.main, ctx));
            return self.handle_event(response);
        }
        false
    }

    /// Item of the sidebar matching what is on screen.
    fn active_nav(&self) -> Option<NavItem> {
        match self.current {
            Screens::Player => Some(NavItem::Home),
            Screens::Search => Some(NavItem::Search),
            Screens::Library => self.library.selected_nav(),
            Screens::PlaylistViewer => self.library.nav_for(self.viewer.entry()),
            Screens::DeviceLost => None,
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let mut layout = FrameLayout::compute(area, self.sidebar_visible);
        let active_nav = self.active_nav();
        let notice = self
            .notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTICE_DURATION)
            .map(|(text, _)| text.clone());
        let tab = match self.current {
            Screens::Player => Some(0),
            Screens::Library | Screens::PlaylistViewer => Some(1),
            Screens::Search => Some(2),
            Screens::DeviceLost => None,
        };
        let buf = frame.buffer_mut();
        buf.set_style(area, widgets::base());
        chrome::tab_bar(buf, &mut layout, tab, &self.player);
        if let Some(sidebar_area) = layout.sidebar {
            self.sidebar.render(
                buf,
                sidebar_area,
                &self.library,
                active_nav,
                self.focus_sidebar,
                layout.narrow,
            );
        }
        self.layout.narrow = layout.narrow;
        let main = layout.main;
        let mut hints = self.with_screen(|s, ctx| {
            s.render(buf, main, ctx);
            s.hints(ctx)
        });
        // The lyrics panel has its own "y queue" hint
        let lyrics_shown = self.current == Screens::Player && self.show_lyrics;
        if self.current != Screens::DeviceLost && !lyrics_shown {
            hints.insert(hints.len().min(1), hint("y", "lyrics", "lyr"));
        }
        #[cfg(feature = "thumbnails")]
        let art_width = if self.thumbnail.active() { 6 } else { 5 };
        #[cfg(not(feature = "thumbnails"))]
        let art_width = 5;
        chrome::player_bar(buf, &mut layout, &self.player, art_width);
        #[cfg(feature = "thumbnails")]
        self.thumbnail.render(frame, &layout, &self.player);
        chrome::status_line(frame.buffer_mut(), &layout, &hints, notice.as_deref());
        self.layout = layout;
    }

    /// The main loop of the manager
    pub fn run(&mut self, updater: &Receiver<ManagerMessage>) -> Result<(), io::Error> {
        // setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        #[cfg(feature = "thumbnails")]
        self.thumbnail.init();

        // create app and run it
        let tick_rate = Duration::from_millis(250);

        let mut last_tick = Instant::now();
        'a: loop {
            if is_shutdown_sent() {
                break;
            }
            while let Ok(e) = updater.try_recv() {
                if self.handle_manager_message(e) {
                    break 'a;
                }
            }
            terminal.draw(|f| {
                self.player.update();
                self.render(f);
            })?;

            let frame = if self.with_screen(|s, _| s.animating()) {
                ANIMATION_FRAME
            } else {
                tick_rate
            };
            let timeout = frame
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));
            if crossterm::event::poll(timeout)? {
                let quit = match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
                    Event::Mouse(mouse) => self.on_mouse(mouse),
                    _ => false,
                };
                if quit {
                    break;
                }
            }
            if last_tick.elapsed() >= frame {
                last_tick = Instant::now();
            }
        }

        // restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        shutdown();

        Ok(())
    }
}
