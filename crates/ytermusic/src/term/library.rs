//! Library screen: a grid of playlist cards ("YOUR PLAYLISTS", then "FOR YOU").

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};
use ytpapi2::YoutubeMusicVideoRef;

use crate::DATABASE;

use super::{
    sidebar::NavItem,
    widgets::{
        accent, bold, border, dim, ellipsize, highlight_bg, hint, pad, panel, songs_summary, theme,
        Hint,
    },
    Ctx, EventResponse, ManagerMessage, Screen,
};

/// Where a playlist comes from, which decides where it is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Saved in the user's library (sidebar "Playlists" and the main grid)
    Library,
    /// The "Liked Music" auto playlist
    Liked,
    /// Recommendation from the home feed ("FOR YOU")
    Home,
    /// Songs downloaded on this machine
    Downloaded,
    /// Playlist played during the previous session
    Last,
    /// Opened from search results
    Search,
}

#[derive(Clone, Debug)]
pub struct PlaylistEntry {
    pub name: String,
    pub source: Source,
    pub videos: Vec<YoutubeMusicVideoRef>,
}

impl PlaylistEntry {
    pub fn new(name: impl Into<String>, source: Source, videos: Vec<YoutubeMusicVideoRef>) -> Self {
        Self {
            name: name.into(),
            source,
            videos,
        }
    }

    /// Every song downloaded on this machine.
    pub fn downloaded() -> Self {
        Self::new("Downloaded", Source::Downloaded, DATABASE.all())
    }

    pub fn icon(&self) -> &'static str {
        match self.source {
            Source::Liked => "♥",
            Source::Downloaded => "⇩",
            Source::Last => "↺",
            _ => "▤",
        }
    }
}

const CARD_HEIGHT: u16 = 5;
const CARD_GAP_X: u16 = 2;
const CARD_GAP_Y: u16 = 1;

/// Position of a card in the (unscrolled) grid.
#[derive(Clone, Copy)]
struct Slot {
    entry: usize,
    row: usize,
    col: usize,
    /// First line of the card, in rows from the top of the grid
    y: u16,
}

pub struct Library {
    pub entries: Vec<PlaylistEntry>,
    selected: usize,
    offset: u16,
    columns: usize,
    /// Cards drawn during the last frame with their entry index
    drawn: Vec<(Rect, usize)>,
}

impl Library {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            offset: 0,
            columns: 3,
            drawn: Vec::new(),
        }
    }

    pub fn add(&mut self, entry: PlaylistEntry) {
        if entry.source == Source::Downloaded {
            return;
        }
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|x| x.name == entry.name && x.source == entry.source)
        {
            *existing = entry;
            return;
        }
        self.entries.push(entry);
    }

    pub fn liked(&self) -> Option<&PlaylistEntry> {
        self.entries.iter().find(|x| x.source == Source::Liked)
    }

    /// Indexes of the library playlists, as listed in the sidebar.
    pub fn library_indexes(&self) -> impl Iterator<Item = usize> + '_ {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, x)| x.source == Source::Library)
            .map(|(i, _)| i)
    }

    /// Sidebar item matching an opened playlist.
    pub fn nav_for(&self, entry: Option<&PlaylistEntry>) -> Option<NavItem> {
        let entry = entry?;
        match entry.source {
            Source::Liked => Some(NavItem::Liked),
            Source::Downloaded => Some(NavItem::Downloaded),
            Source::Library => self
                .entries
                .iter()
                .position(|x| x.source == Source::Library && x.name == entry.name)
                .map(NavItem::Playlist),
            _ => None,
        }
    }

    pub fn selected_nav(&self) -> Option<NavItem> {
        let slots = self.slots();
        let entry = slots.get(self.selected).map(|x| &self.entries[x.entry]);
        self.nav_for(entry)
    }

    /// Entries in display order: own playlists then recommendations.
    fn sections(&self) -> [Vec<usize>; 2] {
        let pick = |sources: &[Source]| -> Vec<usize> {
            sources
                .iter()
                .flat_map(|source| {
                    self.entries
                        .iter()
                        .enumerate()
                        .filter(move |(_, x)| x.source == *source && !x.videos.is_empty())
                        .map(|(i, _)| i)
                })
                .collect()
        };
        [
            pick(&[Source::Library, Source::Liked]),
            pick(&[Source::Last, Source::Home]),
        ]
    }

    fn slots(&self) -> Vec<Slot> {
        let columns = self.columns.max(1);
        let mut slots = Vec::new();
        let mut y = 0;
        let mut row = 0;
        for (section, entries) in self.sections().into_iter().enumerate() {
            if entries.is_empty() {
                continue;
            }
            if section == 1 && !slots.is_empty() {
                // Room for the "FOR YOU" heading
                y += 2;
            }
            for (i, entry) in entries.into_iter().enumerate() {
                if i > 0 && i % columns == 0 {
                    y += CARD_HEIGHT + CARD_GAP_Y;
                    row += 1;
                }
                slots.push(Slot {
                    entry,
                    row,
                    col: i % columns,
                    y,
                });
            }
            y += CARD_HEIGHT + CARD_GAP_Y;
            row += 1;
        }
        slots
    }

    fn move_selection(&mut self, dx: isize, dy: isize) {
        let slots = self.slots();
        if slots.is_empty() {
            return;
        }
        let current = slots[self.selected.min(slots.len() - 1)];
        if dx != 0 {
            self.selected = self.selected.saturating_add_signed(dx).min(slots.len() - 1);
            return;
        }
        let target_row = current.row as isize + dy;
        if target_row < 0 {
            return;
        }
        let candidates: Vec<usize> = (0..slots.len())
            .filter(|i| slots[*i].row == target_row as usize)
            .collect();
        if let Some(best) = candidates
            .iter()
            .min_by_key(|i| slots[**i].col.abs_diff(current.col))
        {
            self.selected = *best;
        }
    }

    fn open_selected(&self) -> Option<EventResponse> {
        let slot = *self.slots().get(self.selected)?;
        Some(ManagerMessage::Inspect(self.entries[slot.entry].clone()).event())
    }

    fn render_card(&self, buf: &mut Buffer, area: Rect, entry: &PlaylistEntry, selected: bool) {
        let border_style = if selected { accent() } else { border() };
        let bg = if selected {
            highlight_bg()
        } else {
            dim().bg(theme().bg)
        };
        buf.set_style(area, bg);
        let inner_width = area.width.saturating_sub(2) as usize;
        let top = format!("┌{}┐", "─".repeat(inner_width));
        let bottom = format!("└{}┘", "─".repeat(inner_width));
        buf.set_string(area.x, area.y, top, border_style);
        buf.set_string(area.x, area.bottom() - 1, bottom, border_style);
        for row in 1..area.height - 1 {
            buf.set_string(area.x, area.y + row, "│", border_style);
            buf.set_string(area.right() - 1, area.y + row, "│", border_style);
        }
        let text_x = area.x + 2;
        let width = inner_width.saturating_sub(2);
        buf.set_string(
            text_x,
            area.y + 1,
            entry.icon(),
            dim().add_modifier(Modifier::BOLD),
        );
        buf.set_string(text_x, area.y + 2, ellipsize(&entry.name, width), bold());
        buf.set_string(
            text_x,
            area.y + 3,
            ellipsize(&songs_summary(&entry.videos), width),
            dim(),
        );
    }
}

impl Default for Library {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for Library {
    fn on_key(&mut self, key: KeyEvent, _: &Ctx) -> Option<EventResponse> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(0, -1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(0, 1),
            KeyCode::Left | KeyCode::Char('h') => self.move_selection(-1, 0),
            KeyCode::Right | KeyCode::Char('l') => self.move_selection(1, 0),
            KeyCode::Enter => return self.open_selected(),
            _ => return None,
        }
        Some(EventResponse::None)
    }

    fn on_mouse(&mut self, mouse: MouseEvent, _: Rect, _: &Ctx) -> EventResponse {
        match mouse.kind {
            MouseEventKind::Down(_) => {
                let hit = self
                    .drawn
                    .iter()
                    .find(|(r, _)| r.contains((mouse.column, mouse.row).into()))
                    .map(|(_, entry)| *entry);
                if let Some(entry) = hit {
                    let slots = self.slots();
                    if let Some(index) = slots.iter().position(|s| s.entry == entry) {
                        if index == self.selected {
                            return self.open_selected().unwrap_or(EventResponse::None);
                        }
                        self.selected = index;
                    }
                }
            }
            MouseEventKind::ScrollUp => self.move_selection(0, -1),
            MouseEventKind::ScrollDown => self.move_selection(0, 1),
            _ => {}
        }
        EventResponse::None
    }

    fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &Ctx) {
        let inner = panel(buf, area, "Your Playlists", ctx.focused);
        let grid = pad(inner, 2, 1);
        self.columns = if grid.width >= 60 {
            3
        } else if grid.width >= 34 {
            2
        } else {
            1
        };
        self.drawn.clear();
        let slots = self.slots();
        if slots.is_empty() {
            buf.set_stringn(
                grid.x,
                grid.y,
                "Loading playlists…",
                grid.width as usize,
                dim(),
            );
            return;
        }
        self.selected = self.selected.min(slots.len() - 1);

        // Keep the selected card visible
        let selected = slots[self.selected];
        if selected.y < self.offset {
            self.offset = selected.y;
        } else if selected.y + CARD_HEIGHT > self.offset + grid.height {
            self.offset = (selected.y + CARD_HEIGHT).saturating_sub(grid.height);
        }

        let columns = self.columns as u16;
        let card_width = grid.width.saturating_sub(CARD_GAP_X * (columns - 1)) / columns;
        let [own, _] = self.sections();
        let heading_y = slots
            .iter()
            .find(|s| !own.contains(&s.entry))
            .filter(|_| !own.is_empty())
            .map(|s| s.y - 2);
        if let Some(y) = heading_y {
            if y >= self.offset && y < self.offset + grid.height {
                buf.set_string(grid.x, grid.y + y - self.offset, "FOR YOU", dim());
            }
        }
        for (index, slot) in slots.iter().enumerate() {
            if slot.y < self.offset || slot.y + CARD_HEIGHT > self.offset + grid.height {
                continue;
            }
            let card = Rect {
                x: grid.x + slot.col as u16 * (card_width + CARD_GAP_X),
                y: grid.y + slot.y - self.offset,
                width: card_width,
                height: CARD_HEIGHT,
            };
            self.render_card(
                buf,
                card,
                &self.entries[slot.entry],
                index == self.selected && ctx.focused,
            );
            self.drawn.push((card, slot.entry));
        }
    }

    fn hints(&self, _: &Ctx) -> Vec<Hint> {
        vec![
            hint("↑↓", "navigate", "nav"),
            hint("↵", "open playlist", "open"),
            hint("tab", "switch pane", "pane"),
            hint("/", "search", "find"),
            hint("s", "hide sidebar", "side"),
            hint("q", "quit", "quit"),
        ]
    }
}
