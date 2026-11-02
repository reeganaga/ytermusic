//! Playlist viewer: header with stats, songs with their download status.

use common_structs::MusicDownloadStatus;
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use flume::Sender;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    text::{Line, Span},
};

use crate::{
    consts::CACHE_DIR, structures::sound_action::SoundAction, systems::player::PlayerState,
    utils::to_bidi_string,
};

use super::{
    library::{PlaylistEntry, Source},
    widgets::{
        bold, dim, error, hint, pad, panel, row_style, scroll_offset, songs_summary, table, text,
        Hint, TableRow,
    },
    Ctx, EventResponse, ManagerMessage, Screen,
};

#[derive(Clone, Copy)]
enum HeaderAction {
    PlayAll,
    DownloadAll,
}

pub struct PlaylistView {
    entry: Option<PlaylistEntry>,
    selected: usize,
    offset: usize,
    rows_area: Rect,
    header_actions: Vec<(Rect, HeaderAction)>,
    sender: Sender<SoundAction>,
}

impl PlaylistView {
    pub fn new(sender: Sender<SoundAction>) -> Self {
        Self {
            entry: None,
            selected: 0,
            offset: 0,
            rows_area: Rect::default(),
            header_actions: Vec::new(),
            sender,
        }
    }

    pub fn show(&mut self, entry: PlaylistEntry) {
        self.entry = Some(entry);
        self.selected = 0;
        self.offset = 0;
    }

    pub fn entry(&self) -> Option<&PlaylistEntry> {
        self.entry.as_ref()
    }

    fn len(&self) -> usize {
        self.entry.as_ref().map_or(0, |x| x.videos.len())
    }

    fn play_from(&self, index: usize) {
        let Some(entry) = &self.entry else {
            return;
        };
        if entry.videos.is_empty() {
            return;
        }
        if entry.source != Source::Downloaded {
            // Remember it to offer it again on next start
            if let Ok(json) = serde_json::to_string(&(&entry.name, &entry.videos)) {
                let _ = std::fs::write(CACHE_DIR.join("last-playlist.json"), json);
            }
        }
        let _ = self
            .sender
            .send(SoundAction::PlayList(entry.videos.clone(), index));
    }

    fn download(&self, all: bool, player: &PlayerState) -> EventResponse {
        let Some(entry) = &self.entry else {
            return EventResponse::None;
        };
        let videos: Vec<_> = if all {
            entry.videos.clone()
        } else {
            entry
                .videos
                .get(self.selected)
                .cloned()
                .into_iter()
                .collect()
        };
        let videos: Vec<_> = videos
            .into_iter()
            .filter(|x| {
                !matches!(
                    player.status_of(&x.video_id),
                    MusicDownloadStatus::Downloaded | MusicDownloadStatus::Downloading(_)
                )
            })
            .collect();
        let message = match videos.as_slice() {
            [] => "Already downloaded".to_owned(),
            [video] => format!("Downloading {}", video.title),
            _ => format!("Downloading {} songs", videos.len()),
        };
        let _ = self.sender.send(SoundAction::Download(videos));
        ManagerMessage::Notify(message).event()
    }

    fn header_action(&self, action: HeaderAction, player: &PlayerState) -> EventResponse {
        match action {
            HeaderAction::PlayAll => {
                self.play_from(0);
                EventResponse::None
            }
            HeaderAction::DownloadAll => self.download(true, player),
        }
    }
}

fn status_cell(status: MusicDownloadStatus) -> Span<'static> {
    match status {
        MusicDownloadStatus::Downloaded => Span::styled("⇩ ready", text()),
        MusicDownloadStatus::Downloading(p) => Span::styled(format!("↓ {p}%"), dim()),
        MusicDownloadStatus::Queued => Span::styled("queued", dim()),
        MusicDownloadStatus::DownloadFailed => Span::styled("⚠ failed", error()),
        MusicDownloadStatus::NotDownloaded => Span::raw(""),
    }
}

impl Screen for PlaylistView {
    fn on_key(&mut self, key: KeyEvent, ctx: &Ctx) -> Option<EventResponse> {
        let count = self.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = count.saturating_sub(1),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(count.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::Enter => self.play_from(self.selected),
            KeyCode::Char('p') => self.play_from(0),
            KeyCode::Char('d') => return Some(self.download(false, ctx.player)),
            KeyCode::Char('D') => return Some(self.download(true, ctx.player)),
            _ => return None,
        }
        Some(EventResponse::None)
    }

    fn on_mouse(&mut self, mouse: MouseEvent, _: Rect, ctx: &Ctx) -> EventResponse {
        let count = self.len();
        let position = (mouse.column, mouse.row).into();
        match mouse.kind {
            MouseEventKind::Down(_) => {
                if let Some((_, action)) = self
                    .header_actions
                    .iter()
                    .find(|(r, _)| r.contains(position))
                {
                    return self.header_action(*action, ctx.player);
                }
                if self.rows_area.contains(position) {
                    let row = (mouse.row - self.rows_area.y) as usize + self.offset;
                    if row < count {
                        if row == self.selected {
                            self.play_from(row);
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
        self.header_actions.clear();
        let Some(entry) = &self.entry else {
            panel(buf, area, "Playlist", ctx.focused);
            return;
        };
        let inner = panel(buf, area, &entry.name, ctx.focused);
        let content = pad(inner, if ctx.narrow { 1 } else { 2 }, 0);
        if content.height < 5 {
            return;
        }
        let player = ctx.player;
        let downloaded = entry
            .videos
            .iter()
            .filter(|x| player.status_of(&x.video_id) == MusicDownloadStatus::Downloaded)
            .count();

        let header_y = content.y + 1;
        buf.set_stringn(
            content.x,
            header_y,
            to_bidi_string(&entry.name),
            content.width as usize,
            bold(),
        );
        buf.set_stringn(
            content.x,
            header_y + 1,
            format!("{} · {downloaded} downloaded", songs_summary(&entry.videos)),
            content.width as usize,
            dim(),
        );
        if !ctx.narrow {
            let actions = [
                ("⏎ play all", HeaderAction::PlayAll),
                ("⇩ download all", HeaderAction::DownloadAll),
            ];
            let mut x = content.right();
            for (label, action) in actions.iter().rev() {
                let width = label.chars().count() as u16;
                x = x.saturating_sub(width);
                buf.set_string(x, header_y, *label, dim());
                self.header_actions
                    .push((Rect::new(x, header_y, width, 1), *action));
                x = x.saturating_sub(2);
            }
        }

        let list_area = Rect {
            y: content.y + 4,
            height: content.height.saturating_sub(4),
            ..content
        };
        let count = entry.videos.len();
        self.selected = self.selected.min(count.saturating_sub(1));
        let visible = list_area.height.saturating_sub(2) as usize;
        self.offset = scroll_offset(self.selected, self.offset, visible);
        let playing_id = player.current().map(|x| x.video_id.as_str());
        let rows = entry
            .videos
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(visible)
            .map(|(i, video)| {
                let playing = Some(video.video_id.as_str()) == playing_id;
                let number = if playing {
                    format!("▸ {}", i + 1)
                } else {
                    (i + 1).to_string()
                };
                let mut cells = vec![
                    Line::from(number),
                    Line::from(to_bidi_string(&video.title)),
                    Line::from(to_bidi_string(&video.author)),
                ];
                if !ctx.narrow {
                    cells.push(Line::from(status_cell(player.status_of(&video.video_id))));
                }
                cells.push(Line::from(video.duration.clone()));
                TableRow {
                    cells,
                    style: row_style(i == self.selected, playing, ctx.focused),
                }
            })
            .collect();
        let (widths, header): (Vec<Constraint>, Vec<&str>) = if ctx.narrow {
            (
                vec![
                    Constraint::Length(5),
                    Constraint::Fill(2),
                    Constraint::Fill(1),
                    Constraint::Length(7),
                ],
                vec!["#", "Title", "Artist", "Time"],
            )
        } else {
            (
                vec![
                    Constraint::Length(5),
                    Constraint::Fill(1),
                    Constraint::Fill(1),
                    Constraint::Length(10),
                    Constraint::Length(7),
                ],
                vec!["#", "Title", "Artist", "Status", "Time"],
            )
        };
        self.rows_area = table(buf, list_area, &widths, &header, rows);
    }

    fn hints(&self, _: &Ctx) -> Vec<Hint> {
        vec![
            hint("↑↓", "navigate", "nav"),
            hint("↵", "play song", "play"),
            hint("d", "download", "dl"),
            hint("s", "hide sidebar", "side"),
            hint("backspace", "back", "back"),
            hint("q", "quit", "quit"),
            hint("p", "play all", "all"),
            hint("D", "download all", "dl all"),
        ]
    }
}
