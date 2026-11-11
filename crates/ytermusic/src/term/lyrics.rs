//! Lyrics panel: takes the place of the queue on the player screen (`y`).
//! Synced lyrics follow the song with the active line kept near the top third;
//! the romanization and translation of each line are shown under it. With
//! word timings (Better Lyrics) the active line fills word by word. In duets
//! the second singer's lines are on the right, lines sung together centered.
//!
//! Lyrics are only fetched while this panel is visible: `render` notices the
//! playing song changed and starts a fetch.

use std::{
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use flume::{Receiver, Sender};
use log::{error, info};
use lyrics::{
    lrc, Align, Cache, Fetched, HttpClient, Lyrics, Provider, Source, Track, Translation, Word,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use serde::{Deserialize, Serialize};
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
/// Longest wait for the player to know a song's duration when YouTube gave
/// none: lyrics are looked up without it after that.
const DURATION_WAIT: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
pub enum State {
    /// Nothing playing
    Idle,
    Loading,
    Loaded(Lyrics),
    NotFound,
}

/// Lyrics provider as `--lyrics-view` shows it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub provider: Source,
    /// Being asked after `P`
    pub switching: Option<Source>,
    /// Picked with `P` and had nothing for this song
    pub missing_on: Option<Source>,
}

enum Update {
    /// Lyrics of a video for a fetch (generation), `None` when no source has them
    Lyrics(String, u64, Option<Lyrics>),
    /// The provider picked with `P` has no lyrics for the video
    ProviderMiss(String, u64, Source),
    /// Translation of a video's lyrics (from a source) into a language
    Translation(String, Source, String, Result<Translation, String>),
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
    /// Fill the active line word by word (`word_sync = true`)
    word_sync: bool,
    /// When the next word (or line) starts: the panel is redrawn then
    next_change: Option<Instant>,
    provider: Provider,
    /// Latest fetch: older answers are dropped
    generation: u64,
    /// Provider being asked after `P`, the shown lyrics stay meanwhile
    switching: Option<Source>,
    /// Provider picked with `P` that had nothing for this song
    missing_on: Option<Source>,
    /// `P` had nothing while nothing was shown yet: ask the whole chain
    refetch: bool,
    /// Song length in seconds, from YouTube or else from the player
    duration: Option<u64>,
    /// Waiting since then for the player to know the duration (lyrics
    /// sources match songs on it)
    duration_wait: Option<Instant>,
    /// Bumped whenever `state` may have changed, so the `--lyrics-view`
    /// socket knows when to resend the lyrics
    revision: u64,
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
            word_sync: CONFIG.lyrics.word_sync,
            next_change: None,
            provider: CONFIG.lyrics.provider,
            generation: 0,
            switching: None,
            missing_on: None,
            refetch: false,
            duration: None,
            duration_wait: None,
            revision: 0,
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

    /// Lyrics state, offset and toggles shared with `--lyrics-view`.
    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn options(&self) -> (i64, bool, bool) {
        (self.offset_ms, self.romanize, self.translate)
    }

    /// `--lyrics-view`: shows lyrics received from the player instead of
    /// fetching them.
    pub fn set_remote(&mut self, state: State, video_id: Option<String>) {
        if video_id != self.video_id {
            self.video_id = video_id;
            self.manual = None;
            self.scroll = 0.0;
            self.scroll_anim = None;
            self.last_active = None;
            self.fade = None;
        }
        self.state = state;
        self.layout = None;
    }

    /// `--lyrics-view`: offset and toggles of the player.
    pub fn set_remote_options(&mut self, offset_ms: i64, romanize: bool, translate: bool) {
        if (romanize, translate) != (self.romanize, self.translate) {
            self.layout = None;
        }
        self.offset_ms = offset_ms;
        self.romanize = romanize;
        self.translate = translate;
    }

    /// Applies finished fetches and starts the ones the current song needs.
    pub fn sync(&mut self, player: &PlayerState) {
        while let Ok(update) = self.receiver.try_recv() {
            self.apply(update);
        }
        let current = player.current();
        if std::mem::take(&mut self.refetch) {
            if let Some(video) = current.filter(|x| Some(&x.video_id) == self.video_id.as_ref()) {
                self.fetch(video, None);
            }
        }
        if current.map(|x| &x.video_id) != self.video_id.as_ref() {
            self.revision += 1;
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
            self.switching = None;
            self.missing_on = None;
            self.duration = current.and_then(|x| parse_duration(&x.duration));
            self.duration_wait = None;
            self.state = match current {
                Some(_) if self.duration.is_none() => {
                    self.duration_wait = Some(Instant::now());
                    State::Loading
                }
                Some(video) => {
                    self.fetch(video, None);
                    State::Loading
                }
                None => State::Idle,
            };
        }
        if let (Some(since), Some(video)) = (self.duration_wait, current) {
            // The player's duration is the current song's once it plays it
            let playing = player.rtcurrent.as_ref().map(|x| &x.video_id) == Some(&video.video_id)
                && !player.sink.is_finished();
            let known = playing.then(|| player.sink.duration()).flatten();
            if known.is_some() || since.elapsed() > DURATION_WAIT {
                self.duration = known.map(|x| x.round() as u64);
                self.duration_wait = None;
                self.fetch(video, None);
            }
        }
        if let (State::Loaded(lyrics), Some(id)) = (&self.state, &self.video_id) {
            // Google is only asked for what the source does not give
            let wanted = (self.romanize && !lyrics.has_builtin_romanization())
                || (self.translate && lyrics.builtin_translation(Self::target()).is_none());
            if wanted
                && !self.translating
                && !self.translate_failed
                && !lyrics.lines.is_empty()
                && !lyrics.translations.contains_key(Self::target())
            {
                self.translating = true;
                self.fetch_translation(id.clone(), lyrics.source, lyrics.texts());
            }
        }
    }

    fn apply(&mut self, update: Update) {
        self.layout = None;
        self.revision += 1;
        let current = |id: &String, generation: u64| {
            Some(id) == self.video_id.as_ref() && generation == self.generation
        };
        match update {
            Update::Lyrics(id, generation, lyrics) if current(&id, generation) => {
                self.switching = None;
                self.translating = false;
                self.translate_failed = false;
                self.manual = None;
                self.state = match lyrics {
                    Some(lyrics) => State::Loaded(lyrics),
                    None => State::NotFound,
                };
            }
            Update::ProviderMiss(id, generation, source) if current(&id, generation) => {
                self.switching = None;
                self.missing_on = Some(source);
                // `P` while loading dropped the first fetch: run it again
                self.refetch = matches!(self.state, State::Loading);
            }
            Update::Translation(id, source, target, result)
                if Some(&id) == self.video_id.as_ref() =>
            {
                self.translating = false;
                match (result, &mut self.state) {
                    // Dropped when the lyrics changed source meanwhile
                    (Ok(_), State::Loaded(lyrics)) if lyrics.source != source => {}
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

    /// Fetches the lyrics through the provider's chain, or only from `only`
    /// (after `P`: its miss marker is ignored, a miss keeps the shown lyrics).
    fn fetch(&mut self, video: &YoutubeMusicVideoRef, only: Option<Source>) {
        self.generation += 1;
        let generation = self.generation;
        let chain = match only {
            Some(source) => vec![source],
            None => self.provider.chain(),
        };
        let sender = self.sender.clone();
        let client = self.client.clone();
        let cache = self.cache.clone();
        let api = self.api.clone();
        let video = video.clone();
        let duration = self.duration;
        run_service(async move {
            let id = video.video_id.clone();
            let track = Track {
                title: &video.title,
                artist: &video.author,
                album: &video.album,
                duration_secs: duration,
            };
            let mut found = None;
            for &source in &chain {
                if let Some(lyrics) = cache.load(&id, source) {
                    found = Some(lyrics);
                    break;
                }
                if only.is_none() && source.remembers_misses() && cache.is_missing(&id, source) {
                    continue;
                }
                match fetch_from(source, &client, &track, &id, api.as_deref()).await {
                    Fetched::Found(lyrics) => {
                        cache.save(&id, &lyrics);
                        found = Some(lyrics);
                        break;
                    }
                    Fetched::Missing => {
                        info!("No {} lyrics for {id}", source.label());
                        if source.remembers_misses() {
                            cache.mark_missing(&id, source);
                        }
                    }
                    Fetched::Failed(e) => error!("{} lyrics: {e}", source.label()),
                }
            }
            let update = match (found, only) {
                (None, Some(source)) => Update::ProviderMiss(id, generation, source),
                (found, _) => Update::Lyrics(id, generation, found),
            };
            let _ = sender.send(update);
        });
    }

    /// Picked provider and whether it is being asked or had nothing, shared
    /// with `--lyrics-view`.
    pub fn provider_status(&self) -> ProviderStatus {
        ProviderStatus {
            provider: self.provider.source(),
            switching: self.switching,
            missing_on: self.missing_on,
        }
    }

    /// `P`: next provider, saved to the config, asked for the current song.
    fn cycle_provider(&mut self, ctx: &Ctx) -> EventResponse {
        ManagerMessage::Notify(self.next_provider(ctx.player)).event()
    }

    /// Switches to the next provider and asks it for the playing song.
    /// Returns the notice to show.
    pub fn next_provider(&mut self, player: &PlayerState) -> String {
        self.provider = self.provider.next();
        let source = self.provider.source();
        let mut notice = format!("Lyrics provider: {}", source.label());
        if let Err(e) = config::save_lyrics_provider(self.provider) {
            error!("Can't save lyrics provider: {e}");
            notice.push_str(" (not saved to config)");
        }
        self.missing_on = None;
        if let Some(video) = player.current().cloned() {
            self.switching = Some(source);
            self.fetch(&video, Some(source));
        }
        notice
    }

    fn fetch_translation(&self, id: String, source: Source, lines: Vec<String>) {
        let sender = self.sender.clone();
        let client = self.client.clone();
        let target = Self::target().to_owned();
        run_service(async move {
            let result = lyrics::translate::translate(&client, &lines, &target).await;
            let _ = sender.send(Update::Translation(id, source, target, result));
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
        ManagerMessage::Notify(self.toggle_option(romanize)).event()
    }

    /// Turns the romanization (or the translation) on or off, saved to the
    /// config. Returns the notice to show.
    pub fn toggle_option(&mut self, romanize: bool) -> String {
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
        notice
    }

    fn title(&self) -> String {
        let mut title = "Lyrics".to_owned();
        if let State::Loaded(lyrics) = &self.state {
            title.push_str(" · ");
            title.push_str(lyrics.source.label());
        }
        if matches!(&self.state, State::Loaded(l) if !l.synced() && !l.instrumental) {
            title.push_str(" · unsynced");
        }
        if let Some(source) = self.switching {
            title.push_str(&format!(" · asking {}…", source.label()));
        } else if let Some(source) = self.missing_on {
            title.push_str(&format!(" · ⚠ not on {}", source.label()));
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
        // The source's own romanization / translation first, Google's otherwise
        let google = lyrics.translations.get(Self::target());
        let builtin_romanization = lyrics.has_builtin_romanization();
        let romanized = google
            .map(|t| &t.romanized)
            .filter(|_| self.romanize && !builtin_romanization);
        let translated = lyrics
            .builtin_translation(Self::target())
            .or(google.map(|t| &t.translated))
            .filter(|_| self.translate);
        let romanization = |i: usize| -> Option<(&str, &[Word])> {
            if !self.romanize {
                return None;
            }
            match &lyrics.lines[i].romanized {
                Some(x) => Some((&x.text, &x.words)),
                None => extra(romanized, i).map(|x| (x, &[][..])),
            }
        };
        let spaced = (0..lyrics.lines.len())
            .any(|i| romanization(i).is_some() || extra(translated, i).is_some());

        let mut layout = Layout {
            width,
            ..Default::default()
        };
        for (i, line) in lyrics.lines.iter().enumerate() {
            if spaced && i > 0 {
                layout.rows.push(Row {
                    text: String::new(),
                    kind: RowKind::Blank,
                    line: i,
                    words: Vec::new(),
                    align: Align::Left,
                });
            }
            layout.starts.push(layout.rows.len());
            let original = if line.text.is_empty() {
                "♪".to_owned()
            } else {
                to_bidi_string(&line.text)
            };
            // Reordered right-to-left text no longer matches the word timings
            let words: &[Word] = if self.word_sync && original == line.text {
                &line.words
            } else {
                &[]
            };
            layout.align = line.align;
            layout.push_wrapped(&original, RowKind::Main, i, words);
            if let Some((text, words)) = romanization(i) {
                let words = if self.word_sync { words } else { &[] };
                layout.push_wrapped(text, RowKind::Romanized, i, words);
            }
            if let Some(translated) = extra(translated, i) {
                layout.push_wrapped(translated, RowKind::Translated, i, &[]);
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

    /// When the karaoke fill next changes: the next word of the active line,
    /// or the next line. `None` when paused or without word timings.
    fn next_change(
        &self,
        lyrics: &Lyrics,
        active: Option<usize>,
        position: u64,
        paused: bool,
    ) -> Option<Instant> {
        if !self.word_sync || paused || !synced_words(lyrics) {
            return None;
        }
        let line = active.and_then(|a| lyrics.lines.get(a));
        let romanized = line
            .and_then(|x| x.romanized.as_ref())
            .filter(|_| self.romanize)
            .map(|x| &x.words);
        let next_word = line
            .map(|x| &x.words)
            .into_iter()
            .chain(romanized)
            .flatten()
            .map(|x| x.start_ms)
            .filter(|&start| start > position)
            .min();
        let next_line = lyrics.times().into_iter().find(|&start| start > position);
        let next = next_word.into_iter().chain(next_line).min()?;
        // A few ms late so the word has started when redrawn
        Some(Instant::now() + Duration::from_millis(next - position + 5))
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

async fn fetch_from(
    source: Source,
    client: &HttpClient,
    track: &Track<'_>,
    video_id: &str,
    api: Option<&YoutubeMusicInstance>,
) -> Fetched {
    match source {
        Source::BetterLyrics | Source::BetterLyricsQq => {
            let key = Some(CONFIG.lyrics.betterlyrics_api_key.as_str());
            lyrics::betterlyrics::fetch(client, source, track, key).await
        }
        Source::Lrclib => lyrics::lrclib::fetch(client, track).await.into(),
        Source::YoutubeMusic => match api {
            Some(api) => api
                .get_lyrics(video_id)
                .await
                .map(|text| text.map(|text| Lyrics::from_plain(Source::YoutubeMusic, &text)))
                .map_err(|e| format!("{e:?}"))
                .into(),
            None => Fetched::Missing,
        },
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

/// Whether some line has word timings.
fn synced_words(lyrics: &Lyrics) -> bool {
    lyrics.lines.iter().any(|x| !x.words.is_empty())
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

struct Row {
    text: String,
    kind: RowKind,
    /// Lyric line index
    line: usize,
    /// Timed words of the row (karaoke): byte range in `text`, start time
    words: Vec<(Range<usize>, u64)>,
    align: Align,
}

#[derive(Default)]
struct Layout {
    width: usize,
    rows: Vec<Row>,
    /// First row of each lyric line
    starts: Vec<usize>,
    /// Alignment of the rows pushed next (the line's singer)
    align: Align,
}

impl Layout {
    /// Wraps `text` into rows. `words` (whose texts put together are `text`)
    /// are split along the rows for the karaoke fill.
    fn push_wrapped(&mut self, text: &str, kind: RowKind, line: usize, words: &[Word]) {
        let mut spans = Vec::with_capacity(words.len());
        let mut end = 0;
        for word in words {
            spans.push((end..end + word.text.len(), word.start_ms));
            end += word.text.len();
        }
        if end != text.len() {
            spans.clear();
        }
        for range in wrap_ranges(text, self.width) {
            let words = spans
                .iter()
                .filter_map(|(word, time)| {
                    let start = word.start.max(range.start);
                    let end = word.end.min(range.end);
                    (start < end).then(|| (start - range.start..end - range.start, *time))
                })
                .collect();
            self.rows.push(Row {
                text: text[range].to_owned(),
                kind,
                line,
                words,
                align: self.align,
            });
        }
    }
}

/// Splits the text into rows of at most `width` columns, breaking at spaces
/// when possible and anywhere for scripts without spaces (CJK). Rows are
/// byte ranges of `text` without their leading and trailing spaces.
fn wrap_ranges(text: &str, width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0;
    let mut row_width = 0;
    let mut pos = 0;
    for word in text.split_inclusive(' ') {
        let word_width = word.trim_end().width();
        if row_width + word_width > width {
            if let Some(start) = start.take() {
                rows.push(start..end);
                row_width = 0;
            }
        }
        if word_width > width {
            // Word longer than the row: break it by characters
            for (i, c) in word.char_indices() {
                let w = c.width().unwrap_or(0);
                if row_width + w > width {
                    if let Some(start) = start.take() {
                        rows.push(start..end);
                        row_width = 0;
                    }
                }
                if start.is_none() && c == ' ' {
                    continue;
                }
                start.get_or_insert(pos + i);
                row_width += w;
                if c != ' ' {
                    end = pos + i + c.len_utf8();
                }
            }
        } else if !word.trim().is_empty() || start.is_some() {
            start.get_or_insert(pos);
            row_width += word.width();
            let trimmed = word.trim_end().len();
            if trimmed > 0 {
                end = pos + trimmed;
            }
        }
        pos += word.len();
    }
    match start {
        Some(start) if start < end => rows.push(start..end),
        _ if rows.is_empty() => rows.push(0..0),
        _ => {}
    }
    rows
}

impl Screen for LyricsView {
    fn on_key(&mut self, key: KeyEvent, ctx: &Ctx) -> Option<EventResponse> {
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
            KeyCode::Char('P') => return Some(self.cycle_provider(ctx)),
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
        let elapsed = ctx.player.sink.elapsed().as_millis() as u64;
        self.draw(buf, content, elapsed, ctx.player.sink.is_paused());
    }

    fn animating(&self) -> bool {
        LyricsView::animating(self)
    }

    fn redraw_in(&self) -> Option<Duration> {
        LyricsView::redraw_in(self)
    }

    fn hints(&self, _: &Ctx) -> Vec<Hint> {
        self.hints_list()
    }
}

impl LyricsView {
    /// Draws the lyrics in `content` for a song at `elapsed_ms` (before the
    /// offset), paused or not.
    pub fn draw(&mut self, buf: &mut Buffer, content: Rect, elapsed_ms: u64, paused: bool) {
        self.next_change = None;
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
        let position = (elapsed_ms as i64 + self.offset_ms).max(0) as u64;
        let active = synced
            .then(|| lrc::active_line(&lyrics.times(), position))
            .flatten();
        let next_change = self.next_change(lyrics, active, position, paused);
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

        for (i, row) in layout.rows.iter().skip(self.top).take(height).enumerate() {
            let y = content.y + i as u16;
            let style = self.row_style(row.kind, row.line, active, synced);
            // Duets: second singer on the right, everyone together centered
            let free = content.width.saturating_sub(row.text.width() as u16);
            let left = content.x
                + match row.align {
                    Align::Right => free,
                    Align::Center => free / 2,
                    Align::Left => 0,
                };
            if active != Some(row.line) || row.words.is_empty() {
                buf.set_stringn(left, y, &row.text, width, style);
                continue;
            }
            // Karaoke: sung words in the accent color
            let t = theme();
            let sung = style.fg(t.accent);
            let unsung = style.fg(t.text);
            let mut x = left;
            let mut cursor = 0;
            let right = content.x + content.width;
            let mut draw = |x: &mut u16, text: &str, style: Style| {
                let room = right.saturating_sub(*x) as usize;
                *x = buf.set_stringn(*x, y, text, room, style).0;
            };
            for (range, start) in &row.words {
                if range.start > cursor {
                    draw(&mut x, &row.text[cursor..range.start], unsung);
                }
                let style = if *start <= position { sung } else { unsung };
                draw(&mut x, &row.text[range.clone()], style);
                cursor = range.end;
            }
            if cursor < row.text.len() {
                draw(&mut x, &row.text[cursor..], unsung);
            }
        }
        self.next_change = next_change;
        self.layout = Some(layout);
    }

    pub fn animating(&self) -> bool {
        self.scroll_anim.is_some() || self.fade.is_some()
    }

    pub fn redraw_in(&self) -> Option<Duration> {
        self.next_change
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// Scrolls by hand (mouse wheel).
    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll(delta);
    }

    fn hints_list(&self) -> Vec<Hint> {
        let mut hints = vec![hint("y", "queue", "queue"), hint("j/k", "scroll", "scrl")];
        if self.synced() {
            hints.push(hint("c", "center", "ctr"));
        }
        hints.extend([
            hint("R", "romaji", "rom"),
            hint("T", "translate", "tr"),
            hint("P", "provider", "prov"),
        ]);
        if self.synced() {
            hints.push(hint("[ ]", "offset ±0.5s", "ofs"));
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
    use super::{ease_out_cubic, lerp, wrap_ranges, Layout, RowKind};
    use lyrics::Word;
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

    fn wrap(text: &str, width: usize) -> Vec<&str> {
        wrap_ranges(text, width)
            .into_iter()
            .map(|x| &text[x])
            .collect()
    }

    #[test]
    fn splits_words_along_rows() {
        let words: Vec<Word> = [("one ", 1), ("two ", 2), ("th", 3), ("ree", 4)]
            .iter()
            .map(|(text, start)| Word {
                start_ms: *start,
                end_ms: start + 1,
                text: text.to_string(),
            })
            .collect();
        let mut layout = Layout {
            width: 7,
            ..Default::default()
        };
        layout.push_wrapped("one two three", RowKind::Main, 0, &words);
        assert_eq!(layout.rows.len(), 2);
        assert_eq!(layout.rows[0].text, "one two");
        assert_eq!(layout.rows[0].words, vec![(0..4, 1), (4..7, 2)]);
        assert_eq!(layout.rows[1].text, "three");
        assert_eq!(layout.rows[1].words, vec![(0..2, 3), (2..5, 4)]);
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
        assert_eq!(wrap("  a  b ", 10), vec!["a  b"]);
    }
}
