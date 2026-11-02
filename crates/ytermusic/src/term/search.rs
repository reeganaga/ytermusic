//! Search screen: query input, category chips and a results table.
//!
//! Two modes: while the input is focused every printable key edits the query;
//! in the results, letters are commands (`m` mix, `q` quit, ...) and any other
//! character goes back to typing.

use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use flume::Sender;
use log::error;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::Modifier,
    text::{Line, Span},
};
use tokio::task::JoinHandle;
use ytpapi2::{
    BrowseKind, HeaderMap, HeaderValue, SearchFilter, SearchResults, YoutubeMusicInstance,
    YoutubeMusicPlaylistRef, YoutubeMusicVideoRef,
};

use crate::{
    get_header_file, run_service,
    structures::sound_action::{download_manager_handler, SoundAction},
    systems::DOWNLOAD_MANAGER,
    try_get_cookies,
    utils::to_bidi_string,
    ShutdownSignal, DATABASE,
};

use super::{
    library::{PlaylistEntry, Source},
    widgets::{
        accent, border, dim, hint, pad, panel, row_style, scroll_offset, table, text, theme, Hint,
        TableRow,
    },
    Ctx, EventResponse, ManagerMessage, Screen, Screens,
};

#[derive(Clone, Debug)]
pub enum SearchRow {
    Song(YoutubeMusicVideoRef),
    Browse(YoutubeMusicPlaylistRef),
}

#[derive(Default)]
struct Results {
    rows: Vec<SearchRow>,
    loading: bool,
    /// Incremented on each new query so late responses are dropped
    generation: u64,
}

/// Keys that act as commands while the results have the focus.
const RESULT_COMMANDS: &[char] = &[
    'm', 'j', 'k', 'q', 's', 'x', 'r', 'y', ' ', '+', '-', '=', '<', '>', '1', '2', '3', '/', 'g',
    'G',
];

pub struct Search {
    text: String,
    input_focus: bool,
    filter: SearchFilter,
    results: Arc<RwLock<Results>>,
    selected: usize,
    offset: usize,
    rows_area: Rect,
    input_area: Rect,
    chips: Vec<(Rect, SearchFilter)>,
    search_handle: Option<JoinHandle<()>>,
    api: Option<Arc<YoutubeMusicInstance>>,
    action_sender: Sender<SoundAction>,
    updater: Sender<ManagerMessage>,
    added: HashSet<String>,
}

fn browse_parts(playlist: &YoutubeMusicPlaylistRef) -> (String, String) {
    let mut parts = playlist.subtitle.split(" • ");
    let first = parts.next().unwrap_or_default();
    let known = ["Artist", "Album", "Single", "EP", "Playlist", "Profile"];
    if known.contains(&first) {
        (first.to_owned(), parts.collect::<Vec<_>>().join(" • "))
    } else {
        let kind = match BrowseKind::of(&playlist.browse_id) {
            BrowseKind::Artist => "Artist",
            BrowseKind::Album => "Album",
            BrowseKind::Playlist => "Playlist",
        };
        (kind.to_owned(), playlist.subtitle.clone())
    }
}

impl Search {
    pub async fn new(action_sender: Sender<SoundAction>, updater: Sender<ManagerMessage>) -> Self {
        Self {
            text: String::new(),
            input_focus: true,
            filter: SearchFilter::All,
            results: Arc::new(RwLock::new(Results::default())),
            selected: 0,
            offset: 0,
            rows_area: Rect::default(),
            input_area: Rect::default(),
            chips: Vec::new(),
            search_handle: None,
            api: if let Some(cookies) = try_get_cookies() {
                let mut headermap = HeaderMap::new();
                headermap.insert(
                    "cookie",
                    HeaderValue::from_str(&cookies).unwrap(),
                );
                headermap.insert(
                    "user-agent",
                    HeaderValue::from_static("Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0"),
                );
                YoutubeMusicInstance::new(headermap, None).await    //don't think we need a brand account for search
            } else {
                YoutubeMusicInstance::from_header_file(get_header_file().unwrap().1.as_path()).await
            }
                .ok()
                .map(Arc::new),
            action_sender,
            updater,
            added: HashSet::new(),
        }
    }

    /// YouTube Music client, if the cookies were valid.
    pub fn api(&self) -> Option<Arc<YoutubeMusicInstance>> {
        self.api.clone()
    }

    pub fn focus_input(&mut self) {
        self.input_focus = true;
    }

    fn row_count(&self) -> usize {
        self.results.read().unwrap().rows.len()
    }

    fn set_filter(&mut self, delta: isize) {
        let all = SearchFilter::ALL;
        let index = all.iter().position(|x| *x == self.filter).unwrap_or(0);
        let next = (index as isize + delta).rem_euclid(all.len() as isize) as usize;
        self.filter = all[next];
        self.run_search();
    }

    fn run_search(&mut self) {
        if let Some(handle) = self.search_handle.take() {
            handle.abort();
        }
        self.selected = 0;
        self.offset = 0;
        let query = self.text.trim().to_owned();
        let generation = {
            let mut results = self.results.write().unwrap();
            results.generation += 1;
            results.rows.clear();
            results.loading = !query.is_empty();
            results.generation
        };
        if query.is_empty() {
            return;
        }

        // Local songs first, they are playable right away
        let lower = query.to_lowercase();
        let local: Vec<SearchRow> =
            if matches!(self.filter, SearchFilter::All | SearchFilter::Songs) {
                DATABASE
                    .all()
                    .into_iter()
                    .filter(|x| {
                        x.title.to_lowercase().contains(&lower)
                            || x.author.to_lowercase().contains(&lower)
                    })
                    .take(10)
                    .map(SearchRow::Song)
                    .collect()
            } else {
                Vec::new()
            };
        self.results.write().unwrap().rows = local.clone();

        let Some(api) = self.api.clone() else {
            self.results.write().unwrap().loading = false;
            return;
        };
        let results = self.results.clone();
        let filter = self.filter;
        self.search_handle = Some(run_service(async move {
            // Wait a bit so typing does not spam the api
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let mut rows = local;
            match api.search_filtered(&query, filter, 0).await {
                Ok(SearchResults { videos, playlists }) => {
                    for video in videos {
                        if !rows.iter().any(
                            |r| matches!(r, SearchRow::Song(v) if v.video_id == video.video_id),
                        ) {
                            rows.push(SearchRow::Song(video));
                        }
                    }
                    rows.extend(playlists.into_iter().map(SearchRow::Browse));
                }
                Err(e) => error!("search {e:?}"),
            }
            let mut results = results.write().unwrap();
            if results.generation == generation {
                results.rows = rows;
                results.loading = false;
            }
        }));
    }

    fn selected_row(&self) -> Option<SearchRow> {
        self.results
            .read()
            .unwrap()
            .rows
            .get(self.selected)
            .cloned()
    }

    fn notify(&self, text: String) {
        let _ = self.updater.send(ManagerMessage::Notify(text));
    }

    fn execute(&mut self, row: SearchRow) -> EventResponse {
        match row {
            SearchRow::Song(video) => {
                let _ = self
                    .action_sender
                    .send(SoundAction::AddVideoUnary(video.clone()));
                DOWNLOAD_MANAGER.start_task_unary(
                    download_manager_handler(self.action_sender.clone()),
                    video.clone(),
                    ShutdownSignal,
                );
                self.added.insert(video.video_id.clone());
                ManagerMessage::Notify(format!("Added to queue: {}", video.title)).event()
            }
            SearchRow::Browse(playlist) => {
                let Some(api) = self.api.clone() else {
                    return EventResponse::None;
                };
                let updater = self.updater.clone();
                self.notify(format!("Opening {}…", playlist.name));
                run_service(async move {
                    match api.get_browse_songs(&playlist.browse_id, 2).await {
                        Ok(videos) if !videos.is_empty() => {
                            let _ = updater.send(ManagerMessage::Inspect(PlaylistEntry::new(
                                playlist.name.clone(),
                                Source::Search,
                                videos,
                            )));
                        }
                        Ok(_) => {
                            let _ = updater.send(ManagerMessage::Notify(format!(
                                "No songs found in {}",
                                playlist.name
                            )));
                        }
                        Err(e) => {
                            error!("browse {e:?}");
                            let _ = updater.send(ManagerMessage::Notify(format!(
                                "Could not open {}",
                                playlist.name
                            )));
                        }
                    }
                });
                EventResponse::None
            }
        }
    }

    /// Replaces the queue with a radio seeded from the selected song.
    fn start_mix(&self) -> EventResponse {
        let Some(SearchRow::Song(seed)) = self.selected_row() else {
            return ManagerMessage::Notify("Mixes start from a song".to_owned()).event();
        };
        let Some(api) = self.api.clone() else {
            return EventResponse::None;
        };
        let updater = self.updater.clone();
        let sender = self.action_sender.clone();
        self.notify(format!("Starting mix from {}…", seed.title));
        run_service(async move {
            match api.get_radio(&seed.video_id).await {
                Ok(mut videos) => {
                    videos.retain(|x| x.video_id != seed.video_id);
                    videos.insert(0, seed.clone());
                    let _ = sender.send(SoundAction::PlayList(videos, 0));
                    let _ = updater.send(ManagerMessage::ChangeState(Screens::Player));
                    let _ = updater.send(ManagerMessage::Notify(format!("Mix: {}", seed.title)));
                }
                Err(e) => {
                    error!("radio {e:?}");
                    let _ =
                        updater.send(ManagerMessage::Notify("Could not start the mix".to_owned()));
                }
            }
        });
        EventResponse::None
    }

    fn type_char(&mut self, c: char) {
        self.input_focus = true;
        self.text.push(c);
        self.run_search();
    }

    fn render_input(&mut self, buf: &mut Buffer, area: Rect) {
        self.input_area = area;
        let border_style = if self.input_focus { accent() } else { border() };
        let inner_width = area.width.saturating_sub(2) as usize;
        buf.set_string(
            area.x,
            area.y,
            format!("┌{}┐", "─".repeat(inner_width)),
            border_style,
        );
        buf.set_string(area.x, area.y + 1, "│", border_style);
        buf.set_string(area.right() - 1, area.y + 1, "│", border_style);
        buf.set_string(
            area.x,
            area.y + 2,
            format!("└{}┘", "─".repeat(inner_width)),
            border_style,
        );
        let mut spans = vec![Span::styled("/ ", accent())];
        if self.text.is_empty() && !self.input_focus {
            spans.push(Span::styled("search songs, artists, albums…", dim()));
        } else {
            spans.push(Span::styled(self.text.clone(), text()));
            if self.input_focus {
                spans.push(Span::styled("_", text()));
            }
        }
        // Keep the end of long queries visible
        let line = Line::from(spans);
        let max = inner_width.saturating_sub(3);
        let line = if line.width() > max {
            let tail: String = self
                .text
                .chars()
                .rev()
                .take(max.saturating_sub(4))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            Line::from(vec![
                Span::styled("/ …", accent()),
                Span::styled(tail, text()),
                Span::styled(if self.input_focus { "_" } else { "" }, text()),
            ])
        } else {
            line
        };
        buf.set_line(area.x + 2, area.y + 1, &line, inner_width as u16 - 1);
    }

    fn render_chips(&mut self, buf: &mut Buffer, area: Rect) {
        self.chips.clear();
        let mut x = area.x;
        for filter in SearchFilter::ALL {
            let label = filter.label();
            let width = label.chars().count() as u16 + 2;
            if x + width > area.right() {
                break;
            }
            let active = filter == self.filter;
            let (bracket, label_style) = if active {
                (
                    accent().bg(theme().highlight),
                    accent().bg(theme().highlight).add_modifier(Modifier::BOLD),
                )
            } else {
                (border(), dim())
            };
            buf.set_string(x, area.y, "[", bracket);
            buf.set_string(x + 1, area.y, label, label_style);
            buf.set_string(x + width - 1, area.y, "]", bracket);
            self.chips.push((Rect::new(x, area.y, width, 1), filter));
            x += width + 1;
        }
    }
}

impl Screen for Search {
    fn on_key(&mut self, key: KeyEvent, _: &Ctx) -> Option<EventResponse> {
        let count = self.row_count();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.input_focus {
            match key.code {
                KeyCode::Esc if !self.text.is_empty() => {
                    self.text.clear();
                    self.run_search();
                }
                KeyCode::Enter | KeyCode::Down if count > 0 => {
                    self.input_focus = false;
                    self.selected = 0;
                }
                KeyCode::Left => self.set_filter(-1),
                KeyCode::Right => self.set_filter(1),
                KeyCode::Backspace if !self.text.is_empty() => {
                    self.text.pop();
                    self.run_search();
                }
                KeyCode::Char(c) if !ctrl => self.type_char(c),
                _ => return None,
            }
            return Some(EventResponse::None);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected == 0 {
                    self.input_focus = true;
                } else {
                    self.selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Char('g') => self.selected = 0,
            KeyCode::Char('G') => self.selected = count.saturating_sub(1),
            KeyCode::Left => self.set_filter(-1),
            KeyCode::Right => self.set_filter(1),
            KeyCode::Esc => self.input_focus = true,
            KeyCode::Enter => {
                let row = self.selected_row()?;
                return Some(self.execute(row));
            }
            KeyCode::Char('m') => return Some(self.start_mix()),
            KeyCode::Char('/') => self.input_focus = true,
            KeyCode::Char(c) if !ctrl && !RESULT_COMMANDS.contains(&c) => self.type_char(c),
            _ => return None,
        }
        Some(EventResponse::None)
    }

    fn on_mouse(&mut self, mouse: MouseEvent, _: Rect, _: &Ctx) -> EventResponse {
        let position = (mouse.column, mouse.row).into();
        let count = self.row_count();
        match mouse.kind {
            MouseEventKind::Down(_) => {
                if let Some((_, filter)) = self.chips.iter().find(|(r, _)| r.contains(position)) {
                    self.filter = *filter;
                    self.run_search();
                } else if self.input_area.contains(position) {
                    self.input_focus = true;
                } else if self.rows_area.contains(position) {
                    let row = (mouse.row - self.rows_area.y) as usize + self.offset;
                    if row < count {
                        self.input_focus = false;
                        if row == self.selected {
                            if let Some(row) = self.selected_row() {
                                return self.execute(row);
                            }
                        }
                        self.selected = row;
                    }
                }
            }
            MouseEventKind::ScrollUp => self.selected = self.selected.saturating_sub(1),
            MouseEventKind::ScrollDown => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            _ => {}
        }
        EventResponse::None
    }

    fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &Ctx) {
        let inner = panel(buf, area, "Search", ctx.focused);
        let content = pad(inner, if ctx.narrow { 1 } else { 2 }, 0);
        if content.height < 7 {
            return;
        }
        self.render_input(
            buf,
            Rect {
                y: content.y + 1,
                height: 3,
                ..content
            },
        );
        self.render_chips(
            buf,
            Rect {
                y: content.y + 5,
                height: 1,
                ..content
            },
        );
        let list_area = Rect {
            y: content.y + 7,
            height: content.height.saturating_sub(7),
            ..content
        };

        let results = self.results.read().unwrap();
        if results.rows.is_empty() {
            let message = if self.text.trim().is_empty() {
                "Type to search YouTube Music"
            } else if results.loading {
                "Searching…"
            } else {
                "No results"
            };
            buf.set_stringn(
                list_area.x,
                list_area.y,
                message,
                list_area.width as usize,
                dim(),
            );
            self.rows_area = Rect::default();
            return;
        }
        self.selected = self.selected.min(results.rows.len() - 1);
        let visible = list_area.height.saturating_sub(2) as usize;
        self.offset = scroll_offset(self.selected, self.offset, visible);
        let playing_id = ctx.player.current().map(|x| x.video_id.clone());
        let focused = ctx.focused && !self.input_focus;
        let rows = results
            .rows
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(visible)
            .map(|(i, row)| {
                let (title, artist, album, time, action, playing) = match row {
                    SearchRow::Song(video) => {
                        let action = if self.added.contains(&video.video_id) {
                            Span::styled("✓ added", dim())
                        } else {
                            Span::styled("▸ mix", accent())
                        };
                        (
                            to_bidi_string(&video.title),
                            to_bidi_string(&video.author),
                            to_bidi_string(&video.album),
                            video.duration.clone(),
                            action,
                            playing_id.as_deref() == Some(video.video_id.as_str()),
                        )
                    }
                    SearchRow::Browse(playlist) => {
                        let (kind, subtitle) = browse_parts(playlist);
                        (
                            to_bidi_string(&playlist.name),
                            to_bidi_string(&subtitle),
                            kind,
                            String::new(),
                            Span::styled("▸ open", accent()),
                            false,
                        )
                    }
                };
                let cells = if ctx.narrow {
                    vec![
                        Line::from((i + 1).to_string()),
                        Line::from(title),
                        Line::from(time),
                        Line::from(action),
                    ]
                } else {
                    vec![
                        Line::from((i + 1).to_string()),
                        Line::from(title),
                        Line::from(artist),
                        Line::from(album),
                        Line::from(time),
                        Line::from(action),
                    ]
                };
                TableRow {
                    cells,
                    style: row_style(i == self.selected, playing, focused),
                }
            })
            .collect();
        let (widths, header): (Vec<Constraint>, Vec<&str>) = if ctx.narrow {
            (
                vec![
                    Constraint::Length(3),
                    Constraint::Fill(1),
                    Constraint::Length(7),
                    Constraint::Length(8),
                ],
                vec!["#", "Title", "Time", "Action"],
            )
        } else {
            (
                vec![
                    Constraint::Length(3),
                    Constraint::Fill(3),
                    Constraint::Fill(2),
                    Constraint::Fill(2),
                    Constraint::Length(7),
                    Constraint::Length(8),
                ],
                vec!["#", "Title", "Artist", "Album", "Time", "Action"],
            )
        };
        drop(results);
        self.rows_area = table(buf, list_area, &widths, &header, rows);
    }

    fn hints(&self, _: &Ctx) -> Vec<Hint> {
        vec![
            hint("type", "to filter", "filter"),
            hint("←→", "category", "cat"),
            hint("↑↓", "navigate", "nav"),
            hint("↵", "add to queue", "add"),
            hint("m", "start mix", "mix"),
            hint("esc", "clear", "clr"),
            hint("s", "hide sidebar", "side"),
            hint("q", "quit", "quit"),
        ]
    }
}
