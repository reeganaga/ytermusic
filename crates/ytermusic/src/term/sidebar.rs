//! Left navigation panel: Home / Search / Liked, the library playlists and the
//! downloaded songs. Collapses to an icon rail on narrow terminals.

use ratatui::{buffer::Buffer, layout::Rect};

use super::{
    library::Library,
    widgets::{accent, border, dim, ellipsize, panel, scroll_offset, text, theme},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavItem {
    Home,
    Search,
    Liked,
    /// Index into `Library::entries`
    Playlist(usize),
    Downloaded,
}

enum Row {
    Item(NavItem),
    Separator,
    Header,
}

#[derive(Default)]
pub struct Sidebar {
    /// Index of the cursor among the selectable items
    cursor: usize,
    offset: usize,
}

fn rows(library: &Library, narrow: bool) -> Vec<Row> {
    let mut rows = vec![
        Row::Item(NavItem::Home),
        Row::Item(NavItem::Search),
        Row::Item(NavItem::Liked),
        Row::Separator,
    ];
    if !narrow {
        rows.push(Row::Header);
    }
    rows.extend(
        library
            .library_indexes()
            .map(|i| Row::Item(NavItem::Playlist(i))),
    );
    rows.push(Row::Separator);
    rows.push(Row::Item(NavItem::Downloaded));
    rows
}

fn items(library: &Library) -> Vec<NavItem> {
    rows(library, false)
        .into_iter()
        .filter_map(|r| match r {
            Row::Item(item) => Some(item),
            _ => None,
        })
        .collect()
}

impl Sidebar {
    pub fn move_cursor(&mut self, library: &Library, delta: isize) {
        let count = items(library).len();
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
    }

    pub fn cursor_item(&self, library: &Library) -> Option<NavItem> {
        items(library).get(self.cursor).copied()
    }

    /// Item under the mouse (row `y` of the screen), also moving the cursor there.
    pub fn click(&mut self, library: &Library, y: u16, area: Rect) -> Option<NavItem> {
        let narrow = area.width < 10;
        let row = (y.checked_sub(area.y + 1)?) as usize + self.offset;
        let item = match rows(library, narrow).into_iter().nth(row)? {
            Row::Item(item) => item,
            _ => return None,
        };
        self.cursor = items(library).iter().position(|x| *x == item)?;
        Some(item)
    }

    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        library: &Library,
        active: Option<NavItem>,
        focused: bool,
        narrow: bool,
    ) {
        let inner = panel(buf, area, if narrow { "" } else { "Library" }, focused);
        if inner.height == 0 {
            return;
        }
        let rows = rows(library, narrow);
        let item_list = items(library);
        self.cursor = self.cursor.min(item_list.len().saturating_sub(1));
        let cursor_item = item_list.get(self.cursor).copied();
        let cursor_row = rows
            .iter()
            .position(|r| matches!(r, Row::Item(i) if Some(*i) == cursor_item))
            .unwrap_or(0);
        let follow = if focused {
            cursor_row
        } else {
            rows.iter()
                .position(|r| matches!(r, Row::Item(i) if Some(*i) == active))
                .unwrap_or(0)
        };
        self.offset = scroll_offset(follow, self.offset, inner.height as usize);

        for (i, row) in rows
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(inner.height as usize)
        {
            let y = inner.y + (i - self.offset) as u16;
            let line = Rect {
                y,
                height: 1,
                ..inner
            };
            match row {
                Row::Separator => {
                    let width = inner.width.saturating_sub(2) as usize;
                    buf.set_string(inner.x + 1, y, "─".repeat(width), border());
                }
                Row::Header => {
                    buf.set_string(inner.x + 2, y, "PLAYLISTS", dim());
                }
                Row::Item(item) => {
                    let is_active = Some(*item) == active;
                    let is_cursor = focused && Some(*item) == cursor_item;
                    let (icon, label) = match item {
                        NavItem::Home => ("▸", "Home"),
                        NavItem::Search => ("⌕", "Search"),
                        NavItem::Liked => ("♥", "Liked Songs"),
                        NavItem::Playlist(index) => (
                            "▤",
                            library
                                .entries
                                .get(*index)
                                .map(|x| x.name.as_str())
                                .unwrap_or(""),
                        ),
                        NavItem::Downloaded => ("⇩", "Downloaded"),
                    };
                    let style = if is_active || is_cursor {
                        text().bg(theme().highlight)
                    } else {
                        dim()
                    };
                    if is_active || is_cursor {
                        buf.set_style(line, style);
                    }
                    if is_active {
                        buf.set_string(inner.x, y, "▌", accent().bg(theme().highlight));
                    }
                    if narrow {
                        buf.set_string(inner.x + 2, y, icon, style);
                    } else {
                        let width = inner.width.saturating_sub(5) as usize;
                        buf.set_string(
                            inner.x + 2,
                            y,
                            format!("{icon} {}", ellipsize(label, width)),
                            style,
                        );
                    }
                }
            }
        }
    }
}
