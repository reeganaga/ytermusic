//! Small drawing helpers shared by every screen: theme styles, titled panels,
//! column tables, and duration formatting.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Widget},
};
use unicode_width::UnicodeWidthStr;
use ytpapi2::YoutubeMusicVideoRef;

use crate::{config::ThemeConfig, consts::CONFIG};

pub fn theme() -> &'static ThemeConfig {
    &CONFIG.theme
}

/// Normal text on the app background.
pub fn base() -> Style {
    Style::new().fg(theme().text).bg(theme().bg)
}
pub fn text() -> Style {
    Style::new().fg(theme().text)
}
pub fn dim() -> Style {
    Style::new().fg(theme().text_dim)
}
pub fn accent() -> Style {
    Style::new().fg(theme().accent)
}
pub fn border() -> Style {
    Style::new().fg(theme().border)
}
pub fn error() -> Style {
    Style::new().fg(theme().error)
}
pub fn bold() -> Style {
    text().add_modifier(Modifier::BOLD)
}
pub fn highlight_bg() -> Style {
    Style::new().bg(theme().highlight)
}

/// Draws a bordered box with an uppercase title inset in the top border and
/// returns the inner area. Active boxes get the accent color.
pub fn panel(buf: &mut Buffer, area: Rect, title: &str, active: bool) -> Rect {
    let border_style = if active { accent() } else { border() };
    let title_style = if active { accent() } else { dim() };
    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style)
        .style(base());
    if !title.is_empty() {
        block = block.title(Line::from(vec![
            Span::styled("─", border_style),
            Span::styled(format!(" {} ", title.to_uppercase()), title_style),
        ]));
    }
    let inner = block.inner(area);
    block.render(area, buf);
    inner
}

/// A horizontal rule across the area's first row.
pub fn rule(buf: &mut Buffer, area: Rect) {
    if area.height == 0 {
        return;
    }
    buf.set_string(
        area.x,
        area.y,
        "─".repeat(area.width as usize),
        border().bg(theme().bg),
    );
}

/// Shrinks a rect by `x` columns on each side and `y` rows on each side.
pub fn pad(area: Rect, x: u16, y: u16) -> Rect {
    Rect {
        x: area.x + x.min(area.width / 2),
        y: area.y + y.min(area.height / 2),
        width: area.width.saturating_sub(x * 2),
        height: area.height.saturating_sub(y * 2),
    }
}

/// Cuts the text to `width` columns, ending with `…` when it was too long.
pub fn ellipsize(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// Parses `3:41` or `1:02:03` into seconds.
pub fn parse_duration(text: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut parts = 0;
    for part in text.trim().split(':') {
        total = total * 60 + part.parse::<u64>().ok()?;
        parts += 1;
    }
    (parts >= 2).then_some(total)
}

/// `m:ss`, or `h:mm:ss` past an hour.
pub fn fmt_clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// `2h 18m` or `48m`.
pub fn fmt_total(secs: u64) -> String {
    let minutes = secs.div_ceil(60);
    if minutes >= 60 {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

/// Sum of the known durations and whether every song had one.
/// `None` if no song has a duration.
pub fn total_duration(videos: &[YoutubeMusicVideoRef]) -> Option<(u64, bool)> {
    let mut known = 0;
    let total = videos
        .iter()
        .filter_map(|x| parse_duration(&x.duration))
        .inspect(|_| known += 1)
        .sum();
    (known > 0).then_some((total, known == videos.len()))
}

/// `2h 18m`, with a `+` when some durations are unknown.
pub fn fmt_total_partial(total: u64, complete: bool) -> String {
    if complete {
        fmt_total(total)
    } else {
        format!("{}+", fmt_total(total))
    }
}

/// `42 songs · 2h 18m`
pub fn songs_summary(videos: &[YoutubeMusicVideoRef]) -> String {
    let count = videos.len();
    let songs = if count == 1 { "song" } else { "songs" };
    match total_duration(videos) {
        Some((total, complete)) => {
            format!("{count} {songs} · {}", fmt_total_partial(total, complete))
        }
        None => format!("{count} {songs}"),
    }
}

/// Keeps `selected` visible in a window of `height` rows starting at `offset`.
pub fn scroll_offset(selected: usize, offset: usize, height: usize) -> usize {
    if height == 0 {
        return selected;
    }
    if selected < offset {
        selected
    } else if selected >= offset + height {
        selected + 1 - height
    } else {
        offset
    }
}

pub struct TableRow<'a> {
    pub cells: Vec<Line<'a>>,
    pub style: Style,
}

/// Column table: uppercase dim header, a rule under it, then the rows.
/// Returns the area used by the rows (for mouse hit testing).
pub fn table(
    buf: &mut Buffer,
    area: Rect,
    widths: &[Constraint],
    header: &[&str],
    rows: Vec<TableRow>,
) -> Rect {
    if area.height < 3 {
        return Rect::default();
    }
    let layout = Layout::horizontal(widths.to_vec()).spacing(2);
    let header_area = Rect { height: 1, ..area };
    for (cell, title) in layout.split(header_area).iter().zip(header) {
        buf.set_stringn(
            cell.x,
            cell.y,
            title.to_uppercase(),
            cell.width as usize,
            dim(),
        );
    }
    rule(
        buf,
        Rect {
            y: area.y + 1,
            height: 1,
            ..area
        },
    );
    let rows_area = Rect {
        y: area.y + 2,
        height: area.height - 2,
        ..area
    };
    for (i, row) in rows.into_iter().take(rows_area.height as usize).enumerate() {
        let line_area = Rect {
            y: rows_area.y + i as u16,
            height: 1,
            ..rows_area
        };
        buf.set_style(line_area, row.style);
        for (cell, content) in layout.split(line_area).iter().zip(row.cells) {
            buf.set_line(cell.x, cell.y, &content, cell.width);
        }
    }
    rows_area
}

/// Style of a table row given whether it is selected / playing.
pub fn row_style(selected: bool, playing: bool, focused: bool) -> Style {
    let mut style = if playing { accent() } else { text() };
    if selected && focused {
        style = style.bg(theme().highlight).fg(theme().accent);
    }
    style
}

/// Key hint shown in the status line: key, label and a short label for narrow terminals.
#[derive(Clone, Copy)]
pub struct Hint {
    pub key: &'static str,
    pub label: &'static str,
    pub short: &'static str,
}

pub const fn hint(key: &'static str, label: &'static str, short: &'static str) -> Hint {
    Hint { key, label, short }
}

#[test]
fn durations() {
    assert_eq!(parse_duration("3:41"), Some(221));
    assert_eq!(parse_duration("1:02:03"), Some(3723));
    assert_eq!(parse_duration(""), None);
    assert_eq!(fmt_total(8280), "2h 18m");
    assert_eq!(fmt_total(2880), "48m");
    assert_eq!(fmt_clock(102), "1:42");
    assert_eq!(ellipsize("Midnight City Drive", 10), "Midnight …");
}
