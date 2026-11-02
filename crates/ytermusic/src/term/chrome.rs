//! Frame drawn around every screen: the tab bar on top, the persistent player
//! bar and the key hint line at the bottom.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use download_manager::Downloader;

use crate::{config::RepeatMode, systems::player::PlayerState, systems::DOWNLOAD_MANAGER};

use super::widgets::{accent, border, dim, ellipsize, fmt_clock, pad, rule, text, theme, Hint};

/// Below this width the layout switches to the compact variant.
pub const NARROW_WIDTH: u16 = 80;
const SIDEBAR_WIDTH: u16 = 28;
const RAIL_WIDTH: u16 = 6;
const PLAYER_BAR_HEIGHT: u16 = 3;
const VOLUME_CELLS: u16 = 7;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Button {
    Shuffle,
    Previous,
    PlayPause,
    Next,
    Repeat,
}

/// Where everything was drawn, kept for mouse hit testing.
#[derive(Clone, Default, Debug)]
pub struct FrameLayout {
    pub narrow: bool,
    pub tab_bar: Rect,
    pub tabs: Vec<Rect>,
    pub sidebar: Option<Rect>,
    pub main: Rect,
    pub player_bar: Rect,
    pub art: Rect,
    pub status: Rect,
    pub buttons: Vec<(Rect, Button)>,
    pub progress: Rect,
    pub volume: Rect,
}

impl FrameLayout {
    pub fn compute(area: Rect, sidebar_visible: bool) -> Self {
        let narrow = area.width < NARROW_WIDTH;
        let row = |y: u16, height: u16| Rect {
            x: area.x,
            y: area.y + y.min(area.height),
            width: area.width,
            height: height.min(area.height.saturating_sub(y)),
        };
        // tab bar + rule, body, rule + player bar, rule + status line
        let bottom = PLAYER_BAR_HEIGHT + 3;
        let body_height = area.height.saturating_sub(2 + bottom);
        let tab_bar = row(0, 2);
        let body = pad(row(2, body_height), 1, 0);
        let player_bar = row(2 + body_height, PLAYER_BAR_HEIGHT + 1);
        let status = row(area.height.saturating_sub(2), 2);

        let (sidebar, main) = if sidebar_visible && body.width > 30 {
            let width = if narrow { RAIL_WIDTH } else { SIDEBAR_WIDTH };
            let sidebar = Rect { width, ..body };
            let main = Rect {
                x: body.x + width + 1,
                width: body.width.saturating_sub(width + 1),
                ..body
            };
            (Some(sidebar), main)
        } else {
            (None, body)
        };
        Self {
            narrow,
            tab_bar,
            sidebar,
            main,
            player_bar,
            status,
            ..Default::default()
        }
    }
}

fn downloader_name() -> &'static str {
    match DOWNLOAD_MANAGER.downloader {
        Downloader::YtDlp => "yt-dlp",
        #[cfg(feature = "rusty-ytdl-backend")]
        Downloader::RustyYtdl => "rusty_ytdl",
    }
}

pub fn tab_bar(
    buf: &mut Buffer,
    layout: &mut FrameLayout,
    active: Option<usize>,
    player: &PlayerState,
) {
    let area = layout.tab_bar;
    if area.height == 0 {
        return;
    }
    let y = area.y;
    let narrow = layout.narrow;
    let logo = if narrow { "♪ YTM" } else { "♪ YTerMusic" };
    buf.set_string(area.x + 2, y, logo, accent().add_modifier(Modifier::BOLD));

    // Right side: downloader status and queue size
    let queued = player.upcoming().len();
    let right = if narrow {
        Line::from(Span::styled(format!("q:{queued}"), dim()))
    } else {
        Line::from(vec![
            Span::styled("●", accent()),
            Span::styled(format!(" {}", downloader_name()), dim()),
            Span::styled(format!("   queue: {queued}"), dim()),
        ])
    };
    let right_width = right.width() as u16;
    let right_x = area.right().saturating_sub(right_width + 2);
    buf.set_line(right_x, y, &right, right_width);

    // Centered tabs
    let labels: Vec<String> = ["Player", "Library", "Search"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            if !narrow {
                format!("  [{}] {name}  ", i + 1)
            } else if Some(i) == active {
                format!("[{}]", i + 1)
            } else {
                format!("{}", i + 1)
            }
        })
        .collect();
    let total: u16 = labels.iter().map(|x| x.width() as u16 + 1).sum();
    let mut x = area.x + area.width.saturating_sub(total) / 2;
    layout.tabs.clear();
    for (i, label) in labels.iter().enumerate() {
        let width = label.width() as u16;
        let style = if Some(i) == active {
            accent()
                .bg(theme().accent)
                .fg(theme().on_accent)
                .add_modifier(Modifier::BOLD)
        } else {
            dim()
        };
        buf.set_string(x, y, label, style);
        layout.tabs.push(Rect::new(x, y, width, 1));
        x += width;
        if i + 1 < labels.len() || !narrow {
            buf.set_string(x, y, "│", border());
        }
        x += 1;
    }
    rule(
        buf,
        Rect {
            y: y + 1,
            height: 1,
            ..area
        },
    );
}

/// Placeholder album art: a small box with a note.
pub fn art_placeholder(buf: &mut Buffer, area: Rect) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let inner = area.width - 2;
    buf.set_string(
        area.x,
        area.y,
        format!("┌{}┐", "─".repeat(inner as usize)),
        border(),
    );
    for row in 1..area.height - 1 {
        buf.set_string(area.x, area.y + row, "│", border());
        buf.set_string(area.right() - 1, area.y + row, "│", border());
    }
    buf.set_string(
        area.x,
        area.bottom() - 1,
        format!("└{}┘", "─".repeat(inner as usize)),
        border(),
    );
    buf.set_string(
        area.x + area.width / 2,
        area.y + area.height / 2,
        "♫",
        dim(),
    );
}

pub fn player_bar(buf: &mut Buffer, layout: &mut FrameLayout, player: &PlayerState) {
    let full = layout.player_bar;
    if full.height < 2 {
        return;
    }
    rule(buf, full);
    let area = pad(
        Rect {
            y: full.y + 1,
            height: full.height - 1,
            ..full
        },
        2,
        0,
    );
    let narrow = layout.narrow;
    let middle = area.y + area.height / 2;

    // Art
    let art = Rect { width: 5, ..area };
    layout.art = art;
    art_placeholder(buf, art);

    // Title / artist
    let text_x = art.right() + 2;
    let text_width: u16 = if narrow { 16 } else { 26 };
    let (title, artist) = match player.current() {
        Some(video) => (video.title.as_str(), video.author.as_str()),
        None => ("Nothing playing", ""),
    };
    let title_style = if player.current().is_some() {
        text()
    } else {
        dim()
    };
    buf.set_string(
        text_x,
        middle,
        ellipsize(title, text_width as usize),
        title_style,
    );
    buf.set_string(
        text_x,
        middle + 1,
        ellipsize(artist, text_width as usize),
        dim(),
    );

    // Transport buttons
    let paused = player.sink.is_paused() || player.sink.is_finished();
    let shuffle_style = if player.shuffle { accent() } else { dim() };
    let (repeat_label, repeat_style) = match player.repeat {
        RepeatMode::Off => ("⟳", dim()),
        RepeatMode::All => ("⟳", accent()),
        RepeatMode::One => ("⟳1", accent()),
    };
    let mut buttons = vec![
        (Button::Previous, "◀◀", text()),
        (Button::PlayPause, if paused { "▶" } else { "⏸" }, accent()),
        (Button::Next, "▶▶", text()),
    ];
    if !narrow {
        buttons.insert(0, (Button::Shuffle, "⇄", shuffle_style));
        buttons.push((Button::Repeat, repeat_label, repeat_style));
    }
    let mut x = text_x + text_width + if narrow { 1 } else { 3 };
    layout.buttons.clear();
    for (button, label, style) in buttons {
        let width = label.width() as u16 + 2;
        if x + width > area.right() {
            break;
        }
        buf.set_string(x + 1, middle, label, style);
        layout
            .buttons
            .push((Rect::new(x, middle, width, 1), button));
        x += width;
    }

    // Volume (wide only)
    let volume_width = 6 + VOLUME_CELLS;
    let mut bar_end = area.right();
    if !narrow && area.right() > x + volume_width + 20 {
        let vol_x = area.right() - volume_width;
        let filled = ((player.sink.volume().clamp(0, 100) as f64 / 100.0) * VOLUME_CELLS as f64)
            .round() as u16;
        buf.set_string(vol_x, middle, "♪ vol ", dim());
        for cell in 0..VOLUME_CELLS {
            let (glyph, style) = if cell < filled {
                ("▮", accent())
            } else {
                ("▯", dim())
            };
            buf.set_string(vol_x + 6 + cell, middle, glyph, style);
        }
        layout.volume = Rect::new(vol_x + 6, middle, VOLUME_CELLS, 1);
        bar_end = vol_x.saturating_sub(3);
    } else {
        layout.volume = Rect::default();
    }

    // Progress
    let start = x + if narrow { 1 } else { 3 };
    if bar_end <= start + 4 {
        layout.progress = Rect::default();
        return;
    }
    let elapsed = player.sink.elapsed().as_secs();
    let total = player.sink.duration().map(|x| x as u64).unwrap_or(0);
    let ratio = if player.sink.is_finished() {
        0.0
    } else {
        player.sink.percentage().clamp(0.0, 1.0)
    };
    let (bar_x, bar_width) = if narrow {
        (start, bar_end - start)
    } else {
        let elapsed_text = fmt_clock(elapsed);
        let total_text = fmt_clock(total);
        buf.set_string(start, middle, &elapsed_text, dim());
        let total_x = bar_end - total_text.width() as u16;
        buf.set_string(total_x, middle, &total_text, dim());
        let bar_x = start + elapsed_text.width() as u16 + 1;
        (bar_x, total_x.saturating_sub(bar_x + 1))
    };
    let filled = (bar_width as f64 * ratio).round() as u16;
    buf.set_string(bar_x, middle, "█".repeat(filled as usize), accent());
    buf.set_string(
        bar_x + filled,
        middle,
        "─".repeat(bar_width.saturating_sub(filled) as usize),
        dim(),
    );
    layout.progress = Rect::new(bar_x, middle, bar_width, 1);
}

pub fn status_line(buf: &mut Buffer, layout: &FrameLayout, hints: &[Hint], notice: Option<&str>) {
    let full = layout.status;
    if full.height < 2 {
        return;
    }
    rule(buf, full);
    let area = pad(
        Rect {
            y: full.y + 1,
            height: 1,
            ..full
        },
        2,
        0,
    );
    if let Some(notice) = notice {
        buf.set_stringn(area.x, area.y, notice, area.width as usize, accent());
        return;
    }
    let gap = if layout.narrow { "  " } else { "   " };
    let mut spans = Vec::new();
    for (i, hint) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(gap, dim()));
        }
        let label = if layout.narrow {
            hint.short
        } else {
            hint.label
        };
        spans.push(Span::styled(hint.key, accent()));
        spans.push(Span::styled(format!(" {label}"), dim()));
    }
    buf.set_line(area.x, area.y, &Line::from(spans), area.width);
}
