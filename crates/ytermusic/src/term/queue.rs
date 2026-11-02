//! Player screen: the queue ("Up Next") is the main panel, the current song
//! details live in the bottom player bar.

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use flume::Sender;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    text::Line,
};

use crate::{structures::sound_action::SoundAction, utils::to_bidi_string};

use super::{
    widgets::{
        bold, border, dim, ellipsize, fmt_total, fmt_total_partial, hint, pad, panel,
        parse_duration, row_style, scroll_offset, table, theme, total_duration, Hint, TableRow,
    },
    Ctx, EventResponse, Screen,
};

#[derive(Clone, Copy)]
enum HeaderAction {
    Shuffle,
    Clear,
}

pub struct QueueView {
    /// Selected row: 0 is the current song, n the n-th upcoming song
    selected: usize,
    offset: usize,
    rows_area: Rect,
    /// Lines per row in the last frame (2-line rows on narrow terminals)
    row_height: u16,
    header_actions: Vec<(Rect, HeaderAction)>,
    sender: Sender<SoundAction>,
}

impl QueueView {
    pub fn new(sender: Sender<SoundAction>) -> Self {
        Self {
            selected: 0,
            offset: 0,
            rows_area: Rect::default(),
            row_height: 1,
            header_actions: Vec::new(),
            sender,
        }
    }

    fn send(&self, action: SoundAction) {
        let _ = self.sender.send(action);
    }

    fn row_count(ctx: &Ctx) -> usize {
        let player = ctx.player;
        if player.current().is_some() {
            player.upcoming().len() + 1
        } else {
            0
        }
    }

    fn header_action(&self, action: HeaderAction) {
        match action {
            HeaderAction::Shuffle => self.send(SoundAction::ToggleShuffle),
            HeaderAction::Clear => self.send(SoundAction::Cleanup),
        }
    }
}

impl Screen for QueueView {
    fn on_key(&mut self, key: KeyEvent, ctx: &Ctx) -> Option<EventResponse> {
        let count = Self::row_count(ctx);
        let current = ctx.player.current;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = count.saturating_sub(1),
            KeyCode::Enter if count > 0 => {
                self.send(SoundAction::PlayIndex(current + self.selected))
            }
            KeyCode::Delete if count > 0 => {
                self.send(SoundAction::RemoveIndex(current + self.selected))
            }
            KeyCode::Char('C') => self.send(SoundAction::Cleanup),
            _ => return None,
        }
        Some(EventResponse::None)
    }

    fn on_mouse(&mut self, mouse: MouseEvent, _: Rect, ctx: &Ctx) -> EventResponse {
        let count = Self::row_count(ctx);
        let position = (mouse.column, mouse.row).into();
        match mouse.kind {
            MouseEventKind::Down(_) => {
                if let Some((_, action)) = self
                    .header_actions
                    .iter()
                    .find(|(r, _)| r.contains(position))
                {
                    self.header_action(*action);
                } else if self.rows_area.contains(position) {
                    let row = ((mouse.row - self.rows_area.y) / self.row_height.max(1)) as usize
                        + self.offset;
                    if row < count {
                        if row == self.selected {
                            self.send(SoundAction::PlayIndex(ctx.player.current + row));
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
        let player = ctx.player;
        let inner = panel(buf, area, "Queue", ctx.focused);
        let content = pad(inner, if ctx.narrow { 1 } else { 2 }, 0);
        self.header_actions.clear();
        if content.height < 4 {
            return;
        }
        let count = Self::row_count(ctx);
        self.selected = self.selected.min(count.saturating_sub(1));

        // Header: "Up Next", summary, actions
        let upcoming = player.upcoming();
        let remaining_current = player
            .sink
            .duration()
            .map(|d| (d as u64).saturating_sub(player.sink.elapsed().as_secs()))
            .or_else(|| player.current().and_then(|x| parse_duration(&x.duration)))
            .unwrap_or(0);
        let songs = if upcoming.len() == 1 { "song" } else { "songs" };
        let mut summary = format!("{} {songs} queued", upcoming.len());
        match total_duration(upcoming) {
            Some((total, complete)) => summary.push_str(&format!(
                " · {} remaining",
                fmt_total_partial(remaining_current + total, complete)
            )),
            None if upcoming.is_empty() && remaining_current > 0 => {
                summary.push_str(&format!(" · {} remaining", fmt_total(remaining_current)))
            }
            None => {}
        }
        let header_y = content.y + 1;
        buf.set_string(content.x, header_y, "Up Next", bold());
        if ctx.narrow {
            let text = format!("{} {songs}", upcoming.len());
            let x = content.right().saturating_sub(text.chars().count() as u16);
            buf.set_string(x, header_y, text, dim());
        } else {
            buf.set_stringn(
                content.x,
                header_y + 1,
                &summary,
                content.width as usize,
                dim(),
            );
            let actions = [
                ("⇄ shuffle queue", HeaderAction::Shuffle),
                ("⌫ clear", HeaderAction::Clear),
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
            y: content.y + if ctx.narrow { 3 } else { 4 },
            height: content
                .height
                .saturating_sub(if ctx.narrow { 3 } else { 4 }),
            ..content
        };
        if count == 0 {
            buf.set_stringn(
                list_area.x,
                list_area.y,
                "The queue is empty. Open a playlist in the Library [2] or search with /.",
                list_area.width as usize,
                dim(),
            );
            self.rows_area = Rect::default();
            return;
        }

        let current = player.current;
        if ctx.narrow {
            // Two-line rows (title / artist) separated by rules
            self.row_height = 3;
            let visible = (list_area.height / 3).max(1) as usize;
            self.offset = scroll_offset(self.selected, self.offset, visible);
            for (slot, row) in (self.offset..count).take(visible).enumerate() {
                let Some(video) = player.list.get(current + row) else {
                    break;
                };
                let y = list_area.y + slot as u16 * 3;
                let style = row_style(row == self.selected, row == 0, ctx.focused);
                let line = Rect {
                    y,
                    height: 2,
                    ..list_area
                };
                buf.set_style(line, style);
                let time = &video.duration;
                let title_width = list_area.width.saturating_sub(time.len() as u16 + 3) as usize;
                let marker = if row == 0 { "▸ " } else { "" };
                buf.set_string(
                    list_area.x,
                    y,
                    ellipsize(
                        &format!("{marker}{}", to_bidi_string(&video.title)),
                        title_width,
                    ),
                    style,
                );
                buf.set_string(
                    list_area.right().saturating_sub(time.len() as u16),
                    y,
                    time,
                    style,
                );
                let indent = if row == 0 { 2 } else { 0 };
                buf.set_string(
                    list_area.x + indent,
                    y + 1,
                    ellipsize(
                        &to_bidi_string(&video.author),
                        list_area.width.saturating_sub(indent) as usize,
                    ),
                    if row == 0 {
                        style
                    } else {
                        style.fg(theme().text_dim)
                    },
                );
                if y + 2 < list_area.bottom() {
                    buf.set_string(
                        list_area.x,
                        y + 2,
                        "─".repeat(list_area.width as usize),
                        border(),
                    );
                }
            }
            self.rows_area = list_area;
            return;
        }

        self.row_height = 1;
        let visible = list_area.height.saturating_sub(2) as usize;
        self.offset = scroll_offset(self.selected, self.offset, visible);
        let rows = (self.offset..count)
            .take(visible)
            .filter_map(|row| {
                let video = player.list.get(current + row)?;
                Some(TableRow {
                    cells: vec![
                        Line::from(if row == 0 {
                            "▸".to_owned()
                        } else {
                            row.to_string()
                        }),
                        Line::from(to_bidi_string(&video.title)),
                        Line::from(to_bidi_string(&video.author)),
                        Line::from(video.duration.clone()),
                    ],
                    style: row_style(row == self.selected, row == 0, ctx.focused),
                })
            })
            .collect();
        self.rows_area = table(
            buf,
            list_area,
            &[
                Constraint::Length(4),
                Constraint::Fill(1),
                Constraint::Fill(1),
                Constraint::Length(7),
            ],
            &["#", "Title", "Artist", "Time"],
            rows,
        );
    }

    fn hints(&self, _: &Ctx) -> Vec<Hint> {
        vec![
            hint("space", "play/pause", "play"),
            hint("↑↓", "navigate", "nav"),
            hint("tab", "switch pane", "pane"),
            hint("/", "search", "find"),
            hint("s", "hide sidebar", "side"),
            hint("q", "quit", "quit"),
            hint("↵", "play", "play"),
            hint("del", "remove", "del"),
            hint("x", "shuffle", "shuf"),
            hint("r", "repeat", "rep"),
        ]
    }
}
