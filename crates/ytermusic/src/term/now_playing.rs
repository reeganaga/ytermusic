use std::time::Duration;

use common_structs::AppStatus;
use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::{
    consts::CONFIG,
    structures::{app_status::AppStatusExt, sound_action::SoundAction},
    systems::player::PlayerState,
    utils::to_bidi_string,
};

use super::split_y;

/// Height of the panel, borders included
const HEIGHT: u16 = 5;
/// Below this terminal height the panel is hidden so screens keep enough room
const MIN_TERMINAL_HEIGHT: u16 = 12;
const TIME_WIDTH: u16 = 7;
const VOLUME_LABEL: &str = "Vol ";
const VOLUME_CELLS: u16 = 10;
/// Label, cells and ` 100%`
const VOLUME_WIDTH: u16 = VOLUME_LABEL.len() as u16 + VOLUME_CELLS + 5;
const VOLUME_GAP: u16 = 2;

/// Splits the terminal between the current screen and the now playing panel
pub fn split(area: Rect) -> (Rect, Option<Rect>) {
    if area.height < MIN_TERMINAL_HEIGHT {
        return (area, None);
    }
    let [screen, panel] = split_y(area, HEIGHT);
    (screen, Some(panel))
}

/// Where each part of the panel is drawn. Rendering and mouse handling both
/// use it so click targets always match what is on screen.
struct PanelLayout {
    play_icon: Rect,
    title: Rect,
    artist: Rect,
    start_time: Rect,
    progress: Rect,
    end_time: Rect,
    volume: Option<Rect>,
}

impl PanelLayout {
    fn new(panel: Rect) -> Self {
        let inner = Block::default().borders(Borders::ALL).inner(panel);
        let row = |line: u16, x: u16, width: u16| {
            Rect::new(x, inner.y + line, width, 1).intersection(inner)
        };
        let volume_space = if CONFIG.player.volume_slider {
            VOLUME_WIDTH + VOLUME_GAP
        } else {
            0
        };
        let progress_width = inner.width.saturating_sub(2 * TIME_WIDTH + volume_space);
        let progress_x = inner.x + TIME_WIDTH;
        let end_time_x = progress_x + progress_width;
        Self {
            play_icon: row(0, inner.x + 1, 2),
            title: row(0, inner.x + 4, inner.width.saturating_sub(4)),
            artist: row(1, inner.x + 4, inner.width.saturating_sub(4)),
            start_time: row(2, inner.x, TIME_WIDTH),
            progress: row(2, progress_x, progress_width),
            end_time: row(2, end_time_x, TIME_WIDTH),
            volume: CONFIG
                .player
                .volume_slider
                .then(|| row(2, end_time_x + TIME_WIDTH + VOLUME_GAP, VOLUME_WIDTH)),
        }
    }

    /// The clickable cells of the volume meter, without the label around them
    fn volume_cells(&self) -> Option<Rect> {
        self.volume.map(|volume| {
            Rect::new(
                volume.x + VOLUME_LABEL.len() as u16,
                volume.y,
                VOLUME_CELLS,
                1,
            )
            .intersection(volume)
        })
    }
}

pub fn render(player: &PlayerState, frame: &mut Frame, panel: Rect) {
    let status = status(player);
    let accent = status.style();
    let dim = Style::default().add_modifier(Modifier::DIM);
    let layout = PanelLayout::new(panel);

    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded),
        panel,
    );

    let (title, artist) = match player.current() {
        Some(video) => (to_bidi_string(&video.title), to_bidi_string(&video.author)),
        None => ("No music playing".to_owned(), String::new()),
    };
    frame.render_widget(
        Paragraph::new(status_icon(status)).style(accent),
        layout.play_icon,
    );
    frame.render_widget(
        Paragraph::new(title).style(accent.add_modifier(Modifier::BOLD)),
        layout.title,
    );
    frame.render_widget(
        Paragraph::new(artist).style(CONFIG.player.text_next_style),
        layout.artist,
    );

    let total_time = player
        .sink
        .duration()
        .map(|seconds| format_time(seconds as u64))
        // The player only knows the duration once the file is loaded
        .or_else(|| player.current().map(|video| video.duration.clone()))
        .unwrap_or_else(|| format_time(0));
    frame.render_widget(
        Paragraph::new(format!("{} ", format_time(player.sink.elapsed().as_secs())))
            .alignment(Alignment::Right),
        layout.start_time,
    );
    frame.render_widget(Paragraph::new(format!(" {total_time}")), layout.end_time);

    let ratio = if player.sink.is_finished() {
        0.0
    } else {
        player.sink.percentage().clamp(0.0, 1.0)
    };
    frame.render_widget(
        Paragraph::new(progress_line(ratio, layout.progress.width, accent, dim)),
        layout.progress,
    );

    if let Some(volume) = layout.volume {
        frame.render_widget(
            Paragraph::new(volume_line(player.sink.volume_percent(), accent, dim)),
            volume,
        );
    }
}

/// Handles clicks and scrolling on the panel.
/// Returns false when the event happened outside of it.
pub fn on_mouse(player: &mut PlayerState, mouse: MouseEvent, panel: Rect) -> bool {
    let (x, y) = (mouse.column, mouse.row);
    if !contains(panel, x, y) {
        return false;
    }
    let layout = PanelLayout::new(panel);
    let on_volume = layout.volume.is_some_and(|volume| contains(volume, x, y));
    match mouse.kind {
        MouseEventKind::Down(_) => {
            if contains(layout.progress, x, y) {
                let ratio = (x - layout.progress.x) as f64 / layout.progress.width as f64;
                if let Some(duration) = player.sink.duration() {
                    player
                        .sink
                        .seek_to(Duration::from_secs_f64(duration * ratio));
                }
            } else if let Some(cells) = layout.volume_cells().filter(|cells| contains(*cells, x, y))
            {
                let percent = (x - cells.x + 1) * 100 / VOLUME_CELLS;
                player.sink.set_volume(percent as i32);
            } else if contains(layout.play_icon, x, y) {
                SoundAction::PlayPause.apply_sound_action(player);
            }
        }
        MouseEventKind::ScrollUp if on_volume => SoundAction::Plus.apply_sound_action(player),
        MouseEventKind::ScrollDown if on_volume => SoundAction::Minus.apply_sound_action(player),
        MouseEventKind::ScrollUp => SoundAction::Forward.apply_sound_action(player),
        MouseEventKind::ScrollDown => SoundAction::Backward.apply_sound_action(player),
        _ => {}
    }
    true
}

fn status(player: &PlayerState) -> AppStatus {
    if player.sink.is_paused() {
        AppStatus::Paused
    } else if player.sink.is_finished() {
        AppStatus::NoMusic
    } else {
        AppStatus::Playing
    }
}

fn status_icon(status: AppStatus) -> &'static str {
    match status {
        AppStatus::Playing => "▶",
        AppStatus::Paused => "❚❚",
        AppStatus::NoMusic => "■",
    }
}

fn format_time(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn progress_line(ratio: f64, width: u16, accent: Style, dim: Style) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::default();
    }
    let knob = ((ratio * width as f64) as usize).min(width - 1);
    Line::from(vec![
        Span::styled("━".repeat(knob), accent),
        Span::styled("●", accent),
        Span::styled("─".repeat(width - knob - 1), dim),
    ])
}

fn volume_line(volume: u8, accent: Style, dim: Style) -> Line<'static> {
    let cells = VOLUME_CELLS as usize;
    let filled = (volume.min(100) as usize * cells + 50) / 100;
    Line::from(vec![
        Span::raw(VOLUME_LABEL),
        Span::styled("▮".repeat(filled), accent),
        Span::styled("▯".repeat(cells - filled), dim),
        Span::raw(format!(" {volume:>3}%")),
    ])
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn formats_time() {
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(83), "1:23");
        assert_eq!(format_time(3725), "62:05");
    }

    #[test]
    fn progress_fills_the_whole_width() {
        let style = Style::default();
        assert_eq!(text(&progress_line(0.0, 10, style, style)), "●─────────");
        assert_eq!(text(&progress_line(0.5, 10, style, style)), "━━━━━●────");
        assert_eq!(text(&progress_line(1.0, 10, style, style)), "━━━━━━━━━●");
        assert_eq!(text(&progress_line(0.5, 0, style, style)), "");
    }

    #[test]
    fn volume_rounds_to_cells() {
        let style = Style::default();
        assert_eq!(text(&volume_line(0, style, style)), "Vol ▯▯▯▯▯▯▯▯▯▯   0%");
        assert_eq!(text(&volume_line(60, style, style)), "Vol ▮▮▮▮▮▮▯▯▯▯  60%");
        assert_eq!(text(&volume_line(100, style, style)), "Vol ▮▮▮▮▮▮▮▮▮▮ 100%");
    }

    #[test]
    fn hides_panel_on_short_terminals() {
        assert_eq!(
            split(Rect::new(0, 0, 80, 8)),
            (Rect::new(0, 0, 80, 8), None)
        );
        assert_eq!(
            split(Rect::new(0, 0, 80, 24)),
            (Rect::new(0, 0, 80, 19), Some(Rect::new(0, 19, 80, 5)))
        );
    }
}
