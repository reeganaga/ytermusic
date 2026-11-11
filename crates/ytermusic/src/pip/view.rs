//! `ytermusic --lyrics-view`: shows the lyrics, cover and play buttons of the
//! running player in this terminal. Everything comes from the player's socket
//! (see `pip`), nothing is fetched here but the cover.
//!
//! Layout, wide enough and with a terminal graphics protocol:
//!
//! ```text
//!  ┌─────────┐  Title
//!  │  cover  │  artist • album
//!  │         │  ⇄ provider p        ● romaji r  ○ translate t
//!  │         │
//!  └─────────┘  lyrics, the active line highlighted
//!                romanization, translation
//!
//!                    ◀◀   ⏸   ▶▶
//! ```
//!
//! Without a cover the text takes the whole width.

use std::{
    io::{self, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, buffer::Buffer, layout::Rect, Frame, Terminal};
use unicode_width::UnicodeWidthStr;

#[cfg(feature = "thumbnails")]
use crate::term::thumbnail::Thumbnail;
use crate::term::{
    lyrics::{LyricsView, ProviderStatus, State},
    widgets::{accent, base, bold, dim, ellipsize, text},
};

use super::{read_lines, socket_path, Playback, Song, ToHost, ToView};

/// Redraw interval while the lyrics animate (~30 fps)
const ANIMATION_FRAME: Duration = Duration::from_millis(33);
/// Redraw and socket poll interval otherwise
const TICK: Duration = Duration::from_millis(200);
/// Time between attempts to reach the player
const RECONNECT: Duration = Duration::from_secs(1);
/// Narrower than this the cover is dropped and the lyrics take the width
const COVER_MIN_WIDTH: u16 = 60;
/// Share of the width (percent) the cover can take, at most
const COVER_SHARE: u16 = 50;
/// Width of the three buttons and the gaps between them
const BUTTONS_WIDTH: u16 = 18;

/// Where things go in the terminal.
#[derive(Debug, PartialEq)]
struct Areas {
    cover: Option<Rect>,
    /// Title row, then the artist row under it
    title: Rect,
    artist: Rect,
    /// Lyrics provider, clickable to switch it, and the romaji / translate
    /// toggles (two rows: the toggles wrap to the second when narrow)
    provider: Rect,
    lyrics: Rect,
    controls: Rect,
}

/// Splits the terminal. `cover_font`: size in pixels of a terminal cell when
/// the terminal can draw images, to make the cover square.
///
/// With a cover, it is as big as the height and the width allow (the lyrics
/// keep the rest, close beside it) and the buttons sit right under it.
/// Without one, the buttons are centered on the last row.
fn areas(area: Rect, cover_font: Option<(u16, u16)>) -> Areas {
    let body = Rect {
        x: area.x + 2.min(area.width / 4),
        y: area.y + 1.min(area.height / 4),
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    // Cells are taller than wide: a square cover has more columns than rows.
    // Two rows go to the buttons under it.
    let (cell_w, cell_h) = cover_font
        .filter(|(w, h)| *w > 0 && *h > 0)
        .map_or((1, 2), |(w, h)| (u32::from(w), u32::from(h)));
    let by_height = u32::from(body.height.saturating_sub(2)) * cell_h / cell_w;
    let cover_width = (u32::from(body.width * COVER_SHARE / 100).min(by_height)) as u16;
    let cover =
        (cover_font.is_some() && area.width >= COVER_MIN_WIDTH && cover_width >= BUTTONS_WIDTH)
            .then_some(Rect {
                x: body.x,
                y: body.y,
                width: cover_width,
                height: (u32::from(cover_width) * cell_w / cell_h) as u16,
            });
    let controls = match cover {
        Some(cover) => Rect {
            x: cover.x,
            y: cover.y + cover.height + 1,
            width: cover.width,
            height: 1,
        },
        None => Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(1),
            width: area.width,
            height: area.height.min(1),
        },
    };
    let (x, width) = match cover {
        Some(cover) => {
            let taken = cover.width + 2;
            (body.x + taken, body.width.saturating_sub(taken))
        }
        None => (body.x, body.width),
    };
    // The row of buttons at the bottom is the lyrics' no more when there is
    // no cover
    let reserved = if cover.is_some() { 0 } else { 2 };
    let row = |dy: u16| Rect {
        x,
        y: body.y + dy,
        width,
        height: 1.min(body.height.saturating_sub(dy)),
    };
    Areas {
        cover,
        title: row(0),
        artist: row(1),
        // The toggles go on the second row when the first is too narrow
        provider: Rect {
            height: 2.min(body.height.saturating_sub(2)),
            ..row(2)
        },
        lyrics: Rect {
            x,
            y: body.y + 4.min(body.height),
            width,
            height: body.height.saturating_sub(4 + reserved),
        },
        controls,
    }
}

struct Viewer {
    stream: Option<UnixStream>,
    input: Vec<u8>,
    output: Vec<u8>,
    last_try: Option<Instant>,
    song: Option<Song>,
    lyrics: LyricsView,
    /// Last position the player told, and when
    playback: Option<(Playback, Instant)>,
    /// Lyrics provider of the player
    provider: Option<ProviderStatus>,
    /// Hit boxes of the buttons, from the last draw
    buttons: Vec<(Rect, ToHost)>,
    #[cfg(feature = "thumbnails")]
    cover: Thumbnail,
}

impl Viewer {
    fn new() -> Self {
        Self {
            stream: None,
            input: Vec::new(),
            output: Vec::new(),
            last_try: None,
            song: None,
            lyrics: LyricsView::new(None),
            playback: None,
            provider: None,
            buttons: Vec::new(),
            #[cfg(feature = "thumbnails")]
            cover: Thumbnail::large(),
        }
    }

    /// Cell size in pixels when the terminal can draw the cover.
    fn cover_font(&self) -> Option<(u16, u16)> {
        #[cfg(feature = "thumbnails")]
        return self.cover.font_size();
        #[cfg(not(feature = "thumbnails"))]
        None
    }

    fn disconnect(&mut self) {
        self.stream = None;
        self.input.clear();
        self.output.clear();
        self.song = None;
        self.playback = None;
        self.provider = None;
        self.lyrics.set_remote(State::Idle, None);
    }

    /// Reaches the player when not connected, reads what it sent and writes
    /// the buttons pressed.
    fn poll(&mut self) {
        if self.stream.is_none() {
            if self.last_try.is_some_and(|at| at.elapsed() < RECONNECT) {
                return;
            }
            self.last_try = Some(Instant::now());
            match UnixStream::connect(socket_path()) {
                Ok(stream) if stream.set_nonblocking(true).is_ok() => self.stream = Some(stream),
                _ => return,
            }
        }
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        let (lines, mut alive) = read_lines(stream, &mut self.input);
        for line in lines {
            match serde_json::from_slice::<ToView>(&line) {
                Ok(message) => self.apply(message),
                Err(e) => log::warn!("bad message from the player: {e}"),
            }
        }
        if let Some(stream) = self.stream.as_mut() {
            while !self.output.is_empty() {
                match stream.write(&self.output) {
                    Ok(0) => {
                        alive = false;
                        break;
                    }
                    Ok(n) => {
                        self.output.drain(..n);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        alive = false;
                        break;
                    }
                }
            }
        }
        if !alive {
            self.disconnect();
        }
    }

    fn apply(&mut self, message: ToView) {
        match message {
            ToView::Song(song) => self.song = song,
            ToView::Lyrics(state) => {
                let id = self.song.as_ref().map(|x| x.video_id.clone());
                self.lyrics.set_remote(state, id);
            }
            ToView::Playback(playback) => {
                self.lyrics.set_remote_options(
                    playback.offset_ms,
                    playback.romanize,
                    playback.translate,
                );
                self.provider = Some(playback.provider);
                self.playback = Some((playback, Instant::now()));
            }
        }
    }

    fn send(&mut self, action: ToHost) {
        if self.stream.is_none() {
            return;
        }
        if let Ok(json) = serde_json::to_string(&action) {
            self.output.extend_from_slice(json.as_bytes());
            self.output.push(b'\n');
        }
    }

    /// Song position now: the player's anchor plus the time since, unless paused.
    fn position_ms(&self) -> u64 {
        match &self.playback {
            Some((playback, at)) if !playback.paused => {
                playback.position_ms + at.elapsed().as_millis() as u64
            }
            Some((playback, _)) => playback.position_ms,
            None => 0,
        }
    }

    fn paused(&self) -> bool {
        self.playback
            .as_ref()
            .is_none_or(|(playback, _)| playback.paused)
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let areas = areas(area, self.cover_font());
        let (position, paused) = (self.position_ms(), self.paused());
        let connected = self.stream.is_some();
        let buf = frame.buffer_mut();
        buf.set_style(area, base());
        if area.width < 12 || area.height < 4 {
            return;
        }

        if let Some(song) = self.song.as_ref().filter(|_| connected) {
            let width = areas.title.width as usize;
            buf.set_stringn(
                areas.title.x,
                areas.title.y,
                ellipsize(&song.title, width),
                width,
                bold(),
            );
            let artist = match song.album.is_empty() {
                true => song.author.clone(),
                false => format!("{} • {}", song.author, song.album),
            };
            buf.set_stringn(
                areas.artist.x,
                areas.artist.y,
                ellipsize(&artist, width),
                width,
                dim(),
            );
        }
        if connected {
            self.lyrics.draw(buf, areas.lyrics, position, paused);
        } else {
            self.lyrics.draw(buf, Rect::default(), 0, true);
            let message = "Waiting for ytermusic…";
            let width = areas.lyrics.width.max(areas.title.width);
            let x = area.x + area.width.saturating_sub(message.width() as u16) / 2;
            let y = area.y + area.height / 3;
            buf.set_stringn(x, y, message, width as usize, dim());
            let hint = "run `ytermusic` in another terminal";
            let x = area.x + area.width.saturating_sub(hint.width() as u16) / 2;
            buf.set_stringn(x, y + 1, hint, area.width as usize, dim());
        }
        self.buttons.clear();
        if connected {
            self.draw_provider(buf, areas.provider);
        }
        self.draw_controls(buf, areas.controls, paused);

        #[cfg(feature = "thumbnails")]
        if let Some(cover) = areas.cover {
            let id = self
                .song
                .as_ref()
                .filter(|_| connected)
                .map(|x| x.video_id.clone());
            self.cover.draw(frame, cover, id);
        }
    }

    /// `⇄ BetterLyrics · showing LRCLIB  p`: the picked provider, what is
    /// shown when another source had the lyrics, and the switch state. Then
    /// `● romaji r  ○ translate t`, on the right when both fit on one row,
    /// on the row under it otherwise (shortened when even that is too narrow).
    /// Each part is clickable.
    fn draw_provider(&mut self, buf: &mut Buffer, area: Rect) {
        let Some(status) = self.provider else {
            return;
        };
        if area.height == 0 || area.width == 0 {
            return;
        }
        let (_, romanize, translate) = self.lyrics.options();
        let width = area.width as usize;
        let text = provider_text(status, self.lyrics.state());
        let key = "  p";
        let wanted = text.width() + key.width();
        let long = toggle_labels(romanize, translate, false);
        let long_width = labels_width(&long);
        // One row when everything fits, else as much room as the provider can get
        let (provider_width, toggles) = if wanted + TOGGLES_GAP + long_width <= width {
            (
                width - long_width,
                Some((area.x + (width - long_width) as u16, area.y, long)),
            )
        } else if area.height >= 2 {
            let labels = if long_width <= width {
                long
            } else {
                toggle_labels(romanize, translate, true)
            };
            (width, Some((area.x, area.y + 1, labels)))
        } else {
            (width, None)
        };

        let shown = ellipsize(&text, provider_width.saturating_sub(key.width()));
        let (x, _) = buf.set_stringn(area.x, area.y, &shown, provider_width, dim());
        let (end, _) = buf.set_stringn(
            x,
            area.y,
            key,
            provider_width.saturating_sub(shown.width()),
            accent(),
        );
        self.buttons.push((
            Rect {
                x: area.x,
                y: area.y,
                width: end - area.x,
                height: 1,
            },
            ToHost::CycleProvider,
        ));

        let Some((mut x, y, labels)) = toggles else {
            return;
        };
        let right = area.x + area.width;
        for (action, label, on) in labels {
            let room = right.saturating_sub(x) as usize;
            if room == 0 {
                break;
            }
            let (end, _) = buf.set_stringn(x, y, &label, room, if on { accent() } else { dim() });
            self.buttons.push((
                Rect {
                    x,
                    y,
                    width: end - x,
                    height: 1,
                },
                action,
            ));
            x = end + TOGGLES_GAP as u16;
        }
    }

    fn draw_controls(&mut self, buf: &mut Buffer, area: Rect, paused: bool) {
        if area.height == 0 || self.stream.is_none() {
            return;
        }
        let buttons = [
            (ToHost::Previous, " ◀◀ ", text()),
            (
                ToHost::PlayPause,
                if paused { " ▶  " } else { " ⏸  " },
                accent(),
            ),
            (ToHost::Next, " ▶▶ ", text()),
        ];
        const WIDTH: u16 = 4;
        const GAP: u16 = 3;
        let total = WIDTH * buttons.len() as u16 + GAP * (buttons.len() as u16 - 1);
        debug_assert_eq!(total, BUTTONS_WIDTH);
        let mut x = area.x + area.width.saturating_sub(total) / 2;
        for (action, label, style) in buttons {
            buf.set_stringn(x, area.y, label, WIDTH as usize, style);
            self.buttons.push((
                Rect {
                    x,
                    y: area.y,
                    width: WIDTH,
                    height: 1,
                },
                action,
            ));
            x += WIDTH + GAP;
        }
    }
}

/// Gap before and between the romaji / translate toggles
const TOGGLES_GAP: usize = 2;

/// `● romaji r`, `○ translate t`, or `● rom r`, `○ tl t` when `short`.
fn toggle_labels(romanize: bool, translate: bool, short: bool) -> [(ToHost, String, bool); 2] {
    let (romaji, translation) = if short {
        ("rom", "tl")
    } else {
        ("romaji", "translate")
    };
    [
        (ToHost::ToggleRomanize, romaji, 'r', romanize),
        (ToHost::ToggleTranslate, translation, 't', translate),
    ]
    .map(|(action, name, key, on)| {
        let mark = if on { '●' } else { '○' };
        (action, format!("{mark} {name} {key}"), on)
    })
}

/// Columns the toggles take, gaps between them included.
fn labels_width(labels: &[(ToHost, String, bool)]) -> usize {
    labels.iter().map(|(_, x, _)| x.width()).sum::<usize>()
        + TOGGLES_GAP * labels.len().saturating_sub(1)
}

fn provider_text(status: ProviderStatus, state: &State) -> String {
    let mut text = format!("⇄ {}", status.provider.label());
    if let State::Loaded(lyrics) = state {
        if lyrics.source != status.provider {
            text.push_str(&format!(" · showing {}", lyrics.source.label()));
        }
    }
    if let Some(source) = status.switching {
        text.push_str(&format!(" · asking {}…", source.label()));
    } else if let Some(source) = status.missing_on {
        text.push_str(&format!(" · ⚠ not on {}", source.label()));
    }
    text
}

pub fn run() -> io::Result<()> {
    // The cover download needs a runtime
    #[cfg(feature = "thumbnails")]
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    #[cfg(feature = "thumbnails")]
    let _guard = runtime.enter();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut viewer = Viewer::new();
    #[cfg(feature = "thumbnails")]
    viewer.cover.init();

    let result = (|| -> io::Result<()> {
        loop {
            viewer.poll();
            terminal.draw(|frame| viewer.draw(frame))?;
            let timeout = if viewer.lyrics.animating() {
                ANIMATION_FRAME
            } else {
                viewer
                    .lyrics
                    .redraw_in()
                    .map_or(TICK, |wait| wait.min(TICK))
            };
            if !event::poll(timeout)? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    if key.code == KeyCode::Char('q')
                        || (ctrl && matches!(key.code, KeyCode::Char('c' | 'd')))
                    {
                        return Ok(());
                    }
                    match key.code {
                        KeyCode::Char('p' | 'P') => viewer.send(ToHost::CycleProvider),
                        KeyCode::Char('r' | 'R') => viewer.send(ToHost::ToggleRomanize),
                        KeyCode::Char('t' | 'T') => viewer.send(ToHost::ToggleTranslate),
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let hit = viewer
                            .buttons
                            .iter()
                            .find(|(rect, _)| rect.contains((mouse.column, mouse.row).into()))
                            .map(|(_, action)| *action);
                        if let Some(action) = hit {
                            viewer.send(action);
                        }
                    }
                    MouseEventKind::ScrollUp => viewer.lyrics.scroll_by(-3),
                    MouseEventKind::ScrollDown => viewer.lyrics.scroll_by(3),
                    _ => {}
                },
                _ => {}
            }
        }
    })();

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn cover_on_the_left_when_wide() {
        let layout = areas(area(100, 20), Some((10, 20)));
        let cover = layout.cover.expect("cover");
        // Square on screen: twice as many columns as rows
        assert_eq!(cover.width, cover.height * 2);
        // Lyrics right beside it, buttons right under it and as wide as it
        assert_eq!(layout.title.x, cover.x + cover.width + 2);
        assert_eq!(layout.title.x, layout.lyrics.x);
        assert!(layout.lyrics.y > layout.artist.y);
        assert_eq!(layout.controls.y, cover.y + cover.height + 1);
        assert_eq!(layout.controls.x, cover.x);
        assert_eq!(layout.controls.width, cover.width);
        assert!(layout.controls.width >= BUTTONS_WIDTH);
    }

    #[test]
    fn cover_grows_with_the_terminal() {
        let small = areas(area(60, 20), Some((10, 20)))
            .cover
            .expect("cover")
            .width;
        let wide = areas(area(160, 20), Some((10, 20)))
            .cover
            .expect("cover")
            .width;
        let tall = areas(area(160, 40), Some((10, 20)))
            .cover
            .expect("cover")
            .width;
        // Bound by the height first, then by its share of the width
        assert_eq!(wide, 2 * (20 - 2 - 2));
        assert!(small < wide);
        assert!(tall > wide);
        assert!(tall <= 160 * 50 / 100);
    }

    #[test]
    fn cover_is_square_whatever_the_cells() {
        // 9x22 pixel cells: a square cover has 22/9 times more columns than rows
        let cover = areas(area(160, 40), Some((9, 22))).cover.expect("cover");
        assert!((i32::from(cover.width) * 9 - i32::from(cover.height) * 22).abs() <= 22);
        // and never goes under the buttons row
        assert!(cover.y + cover.height + 2 <= 40);
    }

    #[test]
    fn no_cover_when_narrow_or_unsupported() {
        assert!(areas(area(50, 20), Some((10, 20))).cover.is_none());
        let layout = areas(area(100, 20), None);
        assert!(layout.cover.is_none());
        assert_eq!(layout.title.x, 2);
        assert_eq!(layout.lyrics.width, 96);
        // Buttons on the last row, below the lyrics
        assert_eq!(layout.controls.y, 19);
        assert!(layout.lyrics.y + layout.lyrics.height < layout.controls.y);
    }

    #[test]
    fn provider_line_tells_fallback_and_switch() {
        use lyrics::{Lyrics, Source};
        let status = ProviderStatus {
            provider: Source::BetterLyrics,
            switching: None,
            missing_on: None,
        };
        assert_eq!(provider_text(status, &State::Loading), "⇄ BetterLyrics");
        let fallback = State::Loaded(Lyrics::from_plain(Source::Lrclib, "a"));
        assert_eq!(
            provider_text(status, &fallback),
            "⇄ BetterLyrics · showing LRCLIB"
        );
        let asking = ProviderStatus {
            switching: Some(Source::BetterLyricsQq),
            ..status
        };
        assert!(provider_text(asking, &State::Loading).ends_with("asking BetterLyrics (QQ)…"));
        let missing = ProviderStatus {
            missing_on: Some(Source::BetterLyrics),
            ..status
        };
        assert!(provider_text(missing, &fallback).ends_with("⚠ not on BetterLyrics"));
    }

    #[test]
    fn toggles_wrap_under_the_provider_when_narrow() {
        use lyrics::Source;
        let mut viewer = Viewer::new();
        viewer.provider = Some(ProviderStatus {
            provider: Source::BetterLyricsQq,
            switching: None,
            missing_on: None,
        });
        let row = |width| Rect {
            x: 0,
            y: 0,
            width,
            height: 2,
        };
        let toggle_rows = |viewer: &Viewer| {
            viewer
                .buttons
                .iter()
                .filter(|(_, action)| !matches!(action, ToHost::CycleProvider))
                .map(|(rect, _)| rect.y)
                .collect::<Vec<_>>()
        };
        // Wide: provider and toggles share the first row
        let mut buf = Buffer::empty(row(80));
        viewer.draw_provider(&mut buf, row(80));
        assert_eq!(toggle_rows(&viewer), vec![0, 0]);
        // 46 columns wide terminal: the toggles go under the provider
        viewer.buttons.clear();
        let mut buf = Buffer::empty(row(42));
        viewer.draw_provider(&mut buf, row(42));
        assert_eq!(toggle_rows(&viewer), vec![1, 1]);
        // Very narrow: short labels, still both shown and inside the area
        viewer.buttons.clear();
        let mut buf = Buffer::empty(row(16));
        viewer.draw_provider(&mut buf, row(16));
        assert_eq!(toggle_rows(&viewer), vec![1, 1]);
        assert!(viewer.buttons.iter().all(|(rect, _)| rect.right() <= 16));
    }

    #[test]
    fn tiny_terminals_do_not_overflow() {
        for (w, h) in [(0, 0), (1, 1), (5, 2), (12, 4)] {
            let _ = areas(area(w, h), Some((10, 20)));
        }
    }
}
