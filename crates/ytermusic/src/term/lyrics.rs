//! Lyrics panel: takes the place of the queue on the player screen (`y`).
//! Synced lyrics follow the song with the active line kept near the top third;
//! the romanization and translation of each line are shown under it.
//!
//! Lyrics are only fetched while this panel is visible: `render` notices the
//! playing song changed and starts a fetch.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use flume::{Receiver, Sender};
use log::{error, info};
use lyrics::{lrc, Cache, HttpClient, Lyrics, Source, Track, Translation};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use ytpapi2::{YoutubeMusicInstance, YoutubeMusicVideoRef};

use crate::{
    config,
    consts::{CACHE_DIR, CONFIG},
    run_service,
    systems::player::PlayerState,
    utils::to_bidi_string,
};

use super::{
    widgets::{dim, hint, pad, panel, parse_duration, theme, Hint},
    Ctx, EventResponse, ManagerMessage, Screen,
};

/// Auto-scroll resumes after this long without scrolling by hand.
const MANUAL_SCROLL_TIMEOUT: Duration = Duration::from_secs(5);
const OFFSET_STEP_MS: i64 = 500;
/// Time to slide to the next line (`animate = true`)
const SCROLL_DURATION: Duration = Duration::from_millis(220);
/// Time for a line to fade in / out of the highlight
const FADE_DURATION: Duration = Duration::from_millis(300);

enum State {
    /// Nothing playing
    Idle,
    Loading,
    Loaded(Lyrics),
    NotFound,
}

enum Update {
    /// Lyrics of a video, `None` when no source has them
    Lyrics(String, Option<Lyrics>),
    /// Translation of a video's lyrics into a language
    Translation(String, String, Result<Translation, String>),
}

pub struct LyricsView {
    /// Video whose lyrics are shown (or loading)
    video_id: Option<String>,
    state: State,
    translating: bool,
    translate_failed: bool,
    romanize: bool,
    translate: bool,
    /// Shifts the lyrics: positive shows them earlier. Reset on song change.
    offset_ms: i64,
    /// First visible row while scrolling by hand, and when it last moved
    manual: Option<(usize, Instant)>,
    top: usize,
    max_top: usize,
    /// Wrapped rows of the lyrics, rebuilt when the lyrics, toggles or width change
    layout: Option<Layout>,
    animate: bool,
    /// Displayed first row; fractional while sliding
    scroll: f32,
    /// Slide in progress: from, to, start
    scroll_anim: Option<(f32, usize, Instant)>,
    last_active: Option<usize>,
    /// Highlight fade in progress: line losing the highlight, start
    fade: Option<(Option<usize>, Instant)>,
    sender: Sender<Update>,
    receiver: Receiver<Update>,
    client: HttpClient,
    cache: Cache,
    api: Option<Arc<YoutubeMusicInstance>>,
}

impl LyricsView {
    pub fn new(api: Option<Arc<YoutubeMusicInstance>>) -> Self {
        let (sender, receiver) = flume::unbounded();
        Self {
            video_id: None,
            state: State::Idle,
            translating: false,
            translate_failed: false,
            romanize: CONFIG.lyrics.romanize,
            translate: CONFIG.lyrics.translate,
            offset_ms: 0,
            manual: None,
            top: 0,
            max_top: 0,
            layout: None,
            animate: CONFIG.lyrics.animate,
            scroll: 0.0,
            scroll_anim: None,
            last_active: None,
            fade: None,
            sender,
            receiver,
            client: lyrics::http_client(),
            cache: Cache::new(CACHE_DIR.join("lyrics")),
            api,
        }
    }

    fn target() -> &'static str {
        &CONFIG.lyrics.translate_to
    }

    /// Applies finished fetches and starts the ones the current song needs.
    fn sync(&mut self, player: &PlayerState) {
        while let Ok(update) = self.receiver.try_recv() {
            self.apply(update);
        }
        let current = player.current();
        if current.map(|x| &x.video_id) != self.video_id.as_ref() {
            self.video_id = current.map(|x| x.video_id.clone());
            self.offset_ms = 0;
            self.manual = None;
            self.layout = None;
            self.scroll = 0.0;
            self.scroll_anim = None;
            self.last_active = None;
            self.fade = None;
            self.translating = false;
            self.translate_failed = false;
            self.state = match current {
                Some(video) => {
                    self.fetch(video);
                    State::Loading
                }
                None => State::Idle,
            };
        }
        if let (State::Loaded(lyrics), Some(id)) = (&self.state, &self.video_id) {
            let wanted = self.romanize || self.translate;
            if wanted
                && !self.translating
                && !self.translate_failed
                && !lyrics.lines.is_empty()
                && !lyrics.translations.contains_key(Self::target())
            {
                self.translating = true;
                self.fetch_translation(id.clone(), lyrics.texts());
            }
        }
    }

    fn apply(&mut self, update: Update) {
        self.layout = None;
        match update {
            Update::Lyrics(id, lyrics) if Some(&id) == self.video_id.as_ref() => {
                self.state = match lyrics {
                    Some(lyrics) => State::Loaded(lyrics),
                    None => State::NotFound,
                };
            }
            Update::Translation(id, target, result) if Some(&id) == self.video_id.as_ref() => {
                self.translating = false;
                match (result, &mut self.state) {
                    (Ok(translation), State::Loaded(lyrics)) => {
                        lyrics.translations.insert(target, translation);
                        self.cache.save(&id, lyrics);
                    }
                    (Err(e), _) => {
                        error!("Lyrics translation failed: {e}");
                        self.translate_failed = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn fetch(&self, video: &YoutubeMusicVideoRef) {
        let sender = self.sender.clone();
        let client = self.client.clone();
        let cache = self.cache.clone();
        let api = self.api.clone();
        let video = video.clone();
        run_service(async move {
            let id = video.video_id.clone();
            if let Some(lyrics) = cache.load(&id) {
                let _ = sender.send(Update::Lyrics(id, Some(lyrics)));
                return;
            }
            let track = Track {
                title: &video.title,
                artist: &video.author,
                album: &video.album,
                duration_secs: parse_duration(&video.duration),
            };
            let mut found = lyrics::lrclib::fetch(&client, &track)
                .await
                .inspect_err(|e| error!("LRCLIB: {e}"))
                .ok()
                .flatten();
            if found.is_none() {
                if let Some(api) = api {
                    info!("No LRCLIB lyrics for {id}, trying YouTube Music");
                    found = api
                        .get_lyrics(&id)
                        .await
                        .inspect_err(|e| error!("YouTube Music lyrics: {e:?}"))
                        .ok()
                        .flatten()
                        .map(|text| Lyrics::from_plain(Source::YoutubeMusic, &text));
                }
            }
            if let Some(lyrics) = &found {
                cache.save(&id, lyrics);
            }
            let _ = sender.send(Update::Lyrics(id, found));
        });
    }

    fn fetch_translation(&self, id: String, lines: Vec<String>) {
        let sender = self.sender.clone();
        let client = self.client.clone();
        let target = Self::target().to_owned();
        run_service(async move {
            let result = lyrics::translate::translate(&client, &lines, &target).await;
            let _ = sender.send(Update::Translation(id, target, result));
        });
    }

    fn synced(&self) -> bool {
        matches!(&self.state, State::Loaded(lyrics) if lyrics.synced())
    }

    fn scroll(&mut self, delta: isize) {
        let top = self.manual.map(|(top, _)| top).unwrap_or(self.top);
        let top = top.saturating_add_signed(delta).min(self.max_top);
        self.manual = Some((top, Instant::now()));
    }

    fn toggle(&mut self, romanize: bool) -> EventResponse {
        let (name, on) = if romanize {
            self.romanize = !self.romanize;
            ("Romanization", self.romanize)
        } else {
            self.translate = !self.translate;
            ("Translation", self.translate)
        };
        self.layout = None;
        let mut notice = format!("{name} {}", if on { "on" } else { "off" });
        if let Err(e) = config::save_lyrics_toggles(self.romanize, self.translate) {
            error!("Can't save lyrics toggles: {e}");
            notice.push_str(" (not saved to config)");
        }
        ManagerMessage::Notify(notice).event()
    }

    fn title(&self) -> String {
        let mut title = "Lyrics".to_owned();
        if matches!(&self.state, State::Loaded(l) if !l.synced() && !l.instrumental) {
            title.push_str(" · unsynced");
        }
        if self.offset_ms != 0 {
            title.push_str(&format!(
                " · offset {:+.1}s",
                self.offset_ms as f64 / 1000.0
            ));
        }
        if self.translating {
            title.push_str(" · translating…");
        } else if self.translate_failed && (self.romanize || self.translate) {
            title.push_str(" · ⚠ translate failed");
        }
        title
    }

    /// Wrapped rows of every lyric line with its romanization and translation.
    fn build_layout(&self, lyrics: &Lyrics, width: usize) -> Layout {
        let translation = lyrics.translations.get(Self::target());
        let romanized = translation.map(|t| &t.romanized).filter(|_| self.romanize);
        let translated = translation
            .map(|t| &t.translated)
            .filter(|_| self.translate);
        let spaced = (0..lyrics.lines.len())
            .any(|i| extra(romanized, i).is_some() || extra(translated, i).is_some());

        let mut layout = Layout {
            width,
            ..Default::default()
        };
        for (i, line) in lyrics.lines.iter().enumerate() {
            if spaced && i > 0 {
                layout.rows.push((String::new(), RowKind::Blank, i));
            }
            layout.starts.push(layout.rows.len());
            let original = if line.text.is_empty() {
                "♪".to_owned()
            } else {
                to_bidi_string(&line.text)
            };
            layout.push_wrapped(&original, RowKind::Main, i);
            if let Some(romanized) = extra(romanized, i) {
                layout.push_wrapped(romanized, RowKind::Romanized, i);
            }
            if let Some(translated) = extra(translated, i) {
                layout.push_wrapped(translated, RowKind::Translated, i);
            }
        }
        layout
    }

    /// Style of a row given the active line and how far its fade is (0 to 1).
    fn row_style(&self, kind: RowKind, line: usize, active: Option<usize>, synced: bool) -> Style {
        let t = theme();
        let progress = self.fade_progress();
        let leaving = self.fade.is_some_and(|(prev, _)| prev == Some(line));
        let fg = match kind {
            RowKind::Blank => t.text,
            RowKind::Main if !synced => t.text,
            RowKind::Main if active == Some(line) => lerp(t.text, t.accent, progress),
            RowKind::Main if leaving => lerp(t.accent, t.text_dim, progress),
            RowKind::Main if active.is_some_and(|a| line < a) => t.text_dim,
            RowKind::Main => t.text,
            _ if active == Some(line) => lerp(t.text_dim, t.text, progress),
            _ if leaving => lerp(t.text, t.text_dim, progress),
            _ => t.text_dim,
        };
        let mut style = Style::new().fg(fg);
        if kind == RowKind::Main && active == Some(line) {
            style = style.add_modifier(Modifier::BOLD);
        }
        if kind == RowKind::Translated {
            style = style.add_modifier(Modifier::ITALIC);
        }
        style
    }

    fn fade_progress(&self) -> f32 {
        match self.fade {
            Some((_, start)) => {
                (start.elapsed().as_secs_f32() / FADE_DURATION.as_secs_f32()).min(1.0)
            }
            None => 1.0,
        }
    }

    /// Starts a fade when the active line changes and ends finished ones.
    fn update_fade(&mut self, active: Option<usize>) {
        if active != self.last_active {
            self.fade = self.animate.then(|| (self.last_active, Instant::now()));
            self.last_active = active;
        }
        if self.fade_progress() >= 1.0 {
            self.fade = None;
        }
    }

    /// Moves the displayed first row toward `target`, sliding when animated.
    fn update_scroll(&mut self, target: usize, height: usize) {
        let jump = self.manual.is_some()
            || !self.animate
            || (self.scroll - target as f32).abs() > height as f32;
        if jump {
            self.scroll = target as f32;
            self.scroll_anim = None;
            return;
        }
        if self.scroll_anim.is_none_or(|(_, to, _)| to != target)
            && (self.scroll - target as f32).abs() > 0.01
        {
            self.scroll_anim = Some((self.scroll, target, Instant::now()));
        }
        if let Some((from, to, start)) = self.scroll_anim {
            let t = start.elapsed().as_secs_f32() / SCROLL_DURATION.as_secs_f32();
            if t >= 1.0 {
                self.scroll = to as f32;
                self.scroll_anim = None;
            } else {
                self.scroll = from + (to as f32 - from) * ease_out_cubic(t);
            }
        }
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Color between `from` and `to`; non-RGB colors switch halfway.
fn lerp(from: Color, to: Color, t: f32) -> Color {
    match (from, to) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
            Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        _ if t < 0.5 => from,
        _ => to,
    }
}

/// Line `i` of an optional romanization / translation list.
fn extra(list: Option<&Vec<Option<String>>>, i: usize) -> Option<&str> {
    list.and_then(|x| x.get(i)).and_then(|x| x.as_deref())
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum RowKind {
    Blank,
    Main,
    Romanized,
    Translated,
}

#[derive(Default)]
struct Layout {
    width: usize,
    /// Text, kind and lyric line index of every row
    rows: Vec<(String, RowKind, usize)>,
    /// First row of each lyric line
    starts: Vec<usize>,
}

impl Layout {
    fn push_wrapped(&mut self, text: &str, kind: RowKind, line: usize) {
        for part in wrap(text, self.width) {
            self.rows.push((part, kind, line));
        }
    }
}

/// Splits the text into rows of at most `width` columns, breaking at spaces
/// when possible and anywhere for scripts without spaces (CJK).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_width = 0;
    for word in text.split_inclusive(' ') {
        let word_width = word.trim_end().width();
        if row_width + word_width > width && !row.is_empty() {
            rows.push(row.trim_end().to_owned());
            row.clear();
            row_width = 0;
        }
        if word_width > width {
            // Word longer than the row: break it by characters
            for c in word.chars() {
                let w = c.width().unwrap_or(0);
                if row_width + w > width && !row.is_empty() {
                    rows.push(row.trim_end().to_owned());
                    row.clear();
                    row_width = 0;
                }
                if row.is_empty() && c == ' ' {
                    continue;
                }
                row.push(c);
                row_width += w;
            }
        } else {
            row.push_str(word);
            row_width += word.width();
        }
    }
    if !row.trim().is_empty() || rows.is_empty() {
        rows.push(row.trim_end().to_owned());
    }
    rows
}

impl Screen for LyricsView {
    fn on_key(&mut self, key: KeyEvent, _: &Ctx) -> Option<EventResponse> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll(1),
            KeyCode::PageUp => self.scroll(-10),
            KeyCode::PageDown => self.scroll(10),
            KeyCode::Home | KeyCode::Char('g') => self.manual = Some((0, Instant::now())),
            KeyCode::End | KeyCode::Char('G') => self.manual = Some((self.max_top, Instant::now())),
            KeyCode::Char('c') => self.manual = None,
            KeyCode::Char('R') => return Some(self.toggle(true)),
            KeyCode::Char('T') => return Some(self.toggle(false)),
            KeyCode::Char('[') => self.offset_ms -= OFFSET_STEP_MS,
            KeyCode::Char(']') => self.offset_ms += OFFSET_STEP_MS,
            _ => return None,
        }
        Some(EventResponse::None)
    }

    fn on_mouse(&mut self, mouse: MouseEvent, _: Rect, _: &Ctx) -> EventResponse {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll(-3),
            MouseEventKind::ScrollDown => self.scroll(3),
            _ => {}
        }
        EventResponse::None
    }

    fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &Ctx) {
        self.sync(ctx.player);
        let inner = panel(buf, area, &self.title(), ctx.focused);
        let content = pad(inner, if ctx.narrow { 1 } else { 2 }, 1);
        if content.height == 0 || content.width == 0 {
            return;
        }
        let message = match &self.state {
            State::Idle => Some("Nothing playing"),
            State::Loading => Some("Loading lyrics…"),
            State::NotFound => Some("No lyrics found"),
            State::Loaded(lyrics) if lyrics.instrumental => Some("♪ Instrumental"),
            State::Loaded(lyrics) if lyrics.lines.is_empty() => Some("No lyrics found"),
            State::Loaded(_) => None,
        };
        if let Some(message) = message {
            let x = content.x + content.width.saturating_sub(message.width() as u16) / 2;
            let y = content.y + content.height / 3;
            buf.set_stringn(x, y, message, content.width as usize, dim());
            return;
        }
        let State::Loaded(lyrics) = &self.state else {
            return;
        };

        let synced = lyrics.synced();
        let active = synced
            .then(|| {
                let position = ctx.player.sink.elapsed().as_millis() as i64 + self.offset_ms;
                lrc::active_line(&lyrics.times(), position.max(0) as u64)
            })
            .flatten();
        let width = content.width as usize;
        if self.layout.as_ref().is_none_or(|l| l.width != width) {
            self.layout = Some(self.build_layout(lyrics, width));
        }
        let Some(layout) = self.layout.take() else {
            return;
        };
        let height = content.height as usize;
        self.max_top = layout.rows.len().saturating_sub(height);

        // Hand scrolling wins for a few seconds (for good on unsynced lyrics)
        if let Some((_, at)) = self.manual {
            if synced && at.elapsed() > MANUAL_SCROLL_TIMEOUT {
                self.manual = None;
            }
        }
        let target = match self.manual {
            Some((top, _)) => top,
            None => active
                .and_then(|a| layout.starts.get(a))
                .map(|start| start.saturating_sub(height / 3))
                .unwrap_or(0),
        }
        .min(self.max_top);
        self.update_scroll(target, height);
        self.update_fade(active);
        self.top = (self.scroll.round() as usize).min(self.max_top);

        for (i, (line, kind, index)) in layout.rows.iter().skip(self.top).take(height).enumerate() {
            buf.set_stringn(
                content.x,
                content.y + i as u16,
                line,
                width,
                self.row_style(*kind, *index, active, synced),
            );
        }
        self.layout = Some(layout);
    }

    fn animating(&self) -> bool {
        self.scroll_anim.is_some() || self.fade.is_some()
    }

    fn hints(&self, _: &Ctx) -> Vec<Hint> {
        let mut hints = vec![hint("y", "queue", "queue"), hint("j/k", "scroll", "scrl")];
        if self.synced() {
            hints.push(hint("c", "center", "ctr"));
        }
        hints.extend([hint("R", "romaji", "rom"), hint("T", "translate", "tr")]);
        if self.synced() {
            hints.push(hint("[/]", "offset ±0.5s", "ofs"));
        }
        hints.extend([
            hint("space", "play/pause", "play"),
            hint("q", "quit", "quit"),
        ]);
        hints
    }
}

#[cfg(test)]
mod tests {
    use super::{ease_out_cubic, lerp, wrap};
    use ratatui::style::Color;

    #[test]
    fn animation_helpers() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.5);
        let (a, b) = (Color::Rgb(0, 100, 200), Color::Rgb(100, 200, 0));
        assert_eq!(lerp(a, b, 0.0), a);
        assert_eq!(lerp(a, b, 0.5), Color::Rgb(50, 150, 100));
        assert_eq!(lerp(a, b, 1.0), b);
        assert_eq!(lerp(Color::Red, Color::Blue, 0.4), Color::Red);
        assert_eq!(lerp(Color::Red, Color::Blue, 0.6), Color::Blue);
    }

    #[test]
    fn wraps_words_and_cjk() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(
            wrap("沈むように溶けて", 6),
            vec!["沈むよ", "うに溶", "けて"]
        );
        assert_eq!(wrap("abcdefgh ij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), vec![""]);
    }
}
