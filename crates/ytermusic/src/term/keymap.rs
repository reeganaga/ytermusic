use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{action::Action, Screens};

/// Counts are capped so `99999999j` can't overflow or spin forever
const MAX_COUNT: usize = 9999;

/// A key press normalized so it can be compared with the keymap
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPress {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl KeyPress {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        // Shift is already part of the character (`C` vs `c`, `<` vs `,`)
        let mask = match code {
            KeyCode::Char(_) => KeyModifiers::CONTROL | KeyModifiers::ALT,
            _ => KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
        };
        Self {
            code,
            modifiers: modifiers & mask,
        }
    }
}

impl From<KeyEvent> for KeyPress {
    fn from(key: KeyEvent) -> Self {
        Self::new(key.code, key.modifiers)
    }
}

/// Parses vim-style key notation such as `gg`, `<Esc>`, `<C-Left>` or `<lt>`
pub fn parse_keys(notation: &str) -> Result<Vec<KeyPress>, String> {
    let mut keys = Vec::new();
    let mut chars = notation.chars();
    while let Some(c) = chars.next() {
        if c != '<' {
            keys.push(KeyPress::new(KeyCode::Char(c), KeyModifiers::NONE));
            continue;
        }
        let mut inner = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '>' {
                closed = true;
                break;
            }
            inner.push(c);
        }
        if !closed {
            return Err(format!("Missing `>` in `{notation}`"));
        }
        keys.push(parse_special(&inner)?);
    }
    if keys.is_empty() {
        return Err("Empty key sequence".to_owned());
    }
    Ok(keys)
}

/// Parses the inside of a `<...>` key, like `C-Left`
fn parse_special(inner: &str) -> Result<KeyPress, String> {
    let mut modifiers = KeyModifiers::NONE;
    let mut name = inner;
    while let Some((prefix, rest)) = name.split_once('-') {
        // `<C-->` is Ctrl and the `-` key
        if rest.is_empty() {
            break;
        }
        modifiers |= match prefix {
            "C" => KeyModifiers::CONTROL,
            "A" | "M" => KeyModifiers::ALT,
            "S" => KeyModifiers::SHIFT,
            _ => return Err(format!("Unknown modifier `{prefix}` in `<{inner}>`")),
        };
        name = rest;
    }
    let code = match name.to_ascii_lowercase().as_str() {
        "esc" => KeyCode::Esc,
        "cr" | "enter" | "return" => KeyCode::Enter,
        "space" => KeyCode::Char(' '),
        "bs" | "backspace" => KeyCode::Backspace,
        "del" | "delete" => KeyCode::Delete,
        "tab" => KeyCode::Tab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "lt" => KeyCode::Char('<'),
        "gt" => KeyCode::Char('>'),
        lower => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => match lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    Some(n) => KeyCode::F(n),
                    None => return Err(format!("Unknown key `<{inner}>`")),
                },
            }
        }
    };
    Ok(KeyPress::new(code, modifiers))
}

struct Binding {
    /// `None` binds the keys on every screen
    screen: Option<Screens>,
    keys: Vec<KeyPress>,
    action: Action,
}

pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Keymap {
    pub fn empty() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    pub fn bind(
        &mut self,
        screen: Option<Screens>,
        notation: &str,
        action: Action,
    ) -> Result<(), String> {
        self.bindings.push(Binding {
            screen,
            keys: parse_keys(notation)?,
            action,
        });
        Ok(())
    }

    /// Screen specific bindings come first so they win over global ones
    fn bindings_for(&self, screen: Screens) -> impl Iterator<Item = &Binding> {
        let specific = self
            .bindings
            .iter()
            .filter(move |b| b.screen == Some(screen));
        let global = self.bindings.iter().filter(|b| b.screen.is_none());
        specific.chain(global)
    }
}

const LIST_SCREENS: [Screens; 2] = [Screens::Playlist, Screens::PlaylistViewer];

const GLOBAL_BINDINGS: &[(&[&str], Action)] = &[(&["<C-c>", "<C-d>"], Action::Quit)];

const PLAYER_BINDINGS: &[(&[&str], Action)] = &[
    (&["<Esc>"], Action::Back),
    (&["<F5>"], Action::RetryDownloads),
    (&["f"], Action::OpenSearch),
    (&["s"], Action::Shuffle),
    (&["C"], Action::Cleanup),
    (&["<Space>"], Action::PlayPause),
    (&["<Up>", "k"], Action::Up),
    (&["<Down>", "j"], Action::Down),
    (&["<CR>"], Action::Select),
    (&["+", "="], Action::VolumeUp),
    (&["-"], Action::VolumeDown),
    (&["<lt>", "<Left>", "h"], Action::SeekBackward),
    (&["<C-lt>", "<C-Left>", "<C-h>"], Action::PreviousSong),
    (&[">", "<Right>", "l"], Action::SeekForward),
    (&["<C-gt>", "<C-Right>", "<C-l>"], Action::NextSong),
    (&["r"], Action::RemoveSong),
];

const LIST_BINDINGS: &[(&[&str], Action)] = &[
    (&["<Esc>"], Action::Back),
    (&["f"], Action::OpenSearch),
    (&["<CR>"], Action::Select),
    (&["+", "<Up>", "k"], Action::Up),
    (&["-", "<Down>", "j"], Action::Down),
];

/// Every other key typed in search goes to the text input
const SEARCH_BINDINGS: &[(&[&str], Action)] = &[
    (&["<Esc>"], Action::Back),
    (&["<CR>"], Action::Select),
    (&["<C-CR>"], Action::SelectInBackground),
    (&["<Up>"], Action::Up),
    (&["<Down>"], Action::Down),
];

const DEVICE_LOST_BINDINGS: &[(&[&str], Action)] = &[
    (&["<CR>", "<Space>"], Action::Select),
    (&["<Esc>"], Action::Quit),
];

impl Default for Keymap {
    fn default() -> Self {
        let mut keymap = Self::empty();
        let mut bind_all = |screen: Option<Screens>, bindings: &[(&[&str], Action)]| {
            for (notations, action) in bindings {
                for notation in *notations {
                    keymap
                        .bind(screen, notation, *action)
                        .expect("Invalid default key binding");
                }
            }
        };
        bind_all(None, GLOBAL_BINDINGS);
        bind_all(Some(Screens::MusicPlayer), PLAYER_BINDINGS);
        for screen in LIST_SCREENS {
            bind_all(Some(screen), LIST_BINDINGS);
        }
        bind_all(Some(Screens::Search), SEARCH_BINDINGS);
        bind_all(Some(Screens::DeviceLost), DEVICE_LOST_BINDINGS);
        keymap
    }
}

#[derive(Debug, PartialEq)]
pub enum KeyResult {
    /// The keys typed so far form a binding, repeated `count` times
    Action(Action, usize),
    /// The key was used as part of a count or a key sequence
    Consumed,
    /// A single key with no binding, used for text input
    Unbound(KeyEvent),
}

/// Turns key presses into actions, keeping track of counts (`5j`) and
/// unfinished key sequences (`g` waiting for a second `g`)
#[derive(Default)]
pub struct KeyResolver {
    pending: Vec<KeyPress>,
    count: Option<usize>,
}

impl KeyResolver {
    pub fn feed(&mut self, keymap: &Keymap, screen: Screens, key: KeyEvent) -> KeyResult {
        let press = KeyPress::from(key);
        let waiting = !self.pending.is_empty() || self.count.is_some();
        if waiting && press == KeyPress::new(KeyCode::Esc, KeyModifiers::NONE) {
            self.reset();
            return KeyResult::Consumed;
        }

        if self.pending.is_empty() && !is_text_input(screen) {
            if let Some(digit) = self.count_digit(press) {
                let count = self.count.unwrap_or(0).saturating_mul(10) + digit;
                self.count = Some(count.min(MAX_COUNT));
                return KeyResult::Consumed;
            }
        }

        self.pending.push(press);
        let mut exact = None;
        let mut longer = false;
        for binding in keymap.bindings_for(screen) {
            if binding.keys == self.pending {
                exact = exact.or(Some(binding.action));
            } else if binding.keys.starts_with(&self.pending) {
                longer = true;
            }
        }
        // Like vim with `notimeout`, wait as long as a longer binding can still match
        if longer {
            return KeyResult::Consumed;
        }

        let count = self.count.unwrap_or(1);
        self.reset();
        match exact {
            Some(action) => KeyResult::Action(action, count),
            None if !waiting => KeyResult::Unbound(key),
            None => KeyResult::Consumed,
        }
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.count = None;
    }

    /// `0` only continues a count, like in vim
    fn count_digit(&self, press: KeyPress) -> Option<usize> {
        if press.modifiers != KeyModifiers::NONE {
            return None;
        }
        match press.code {
            KeyCode::Char(c @ '1'..='9') => c.to_digit(10).map(|d| d as usize),
            KeyCode::Char('0') if self.count.is_some() => Some(0),
            _ => None,
        }
    }
}

/// Screens where typed characters are text, so digits are not counts
fn is_text_input(screen: Screens) -> bool {
    screen == Screens::Search
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn char_key(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn feed_str(
        resolver: &mut KeyResolver,
        keymap: &Keymap,
        screen: Screens,
        keys: &str,
    ) -> Vec<KeyResult> {
        keys.chars()
            .map(|c| resolver.feed(keymap, screen, char_key(c)))
            .collect()
    }

    #[test]
    fn parses_plain_and_special_keys() {
        assert_eq!(
            parse_keys("gg").unwrap(),
            vec![
                KeyPress::new(KeyCode::Char('g'), KeyModifiers::NONE),
                KeyPress::new(KeyCode::Char('g'), KeyModifiers::NONE),
            ]
        );
        assert_eq!(
            parse_keys("<C-Left>").unwrap(),
            vec![KeyPress::new(KeyCode::Left, KeyModifiers::CONTROL)]
        );
        assert_eq!(
            parse_keys("<lt><Space><F5><C-->").unwrap(),
            vec![
                KeyPress::new(KeyCode::Char('<'), KeyModifiers::NONE),
                KeyPress::new(KeyCode::Char(' '), KeyModifiers::NONE),
                KeyPress::new(KeyCode::F(5), KeyModifiers::NONE),
                KeyPress::new(KeyCode::Char('-'), KeyModifiers::CONTROL),
            ]
        );
        assert_eq!(
            parse_keys("<C-F>").unwrap(),
            vec![KeyPress::new(KeyCode::Char('F'), KeyModifiers::CONTROL)]
        );
    }

    #[test]
    fn rejects_invalid_notation() {
        assert!(parse_keys("").is_err());
        assert!(parse_keys("<Esc").is_err());
        assert!(parse_keys("<Nope>").is_err());
        assert!(parse_keys("<X-a>").is_err());
    }

    #[test]
    fn shift_is_ignored_for_characters() {
        let shifted = KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT);
        assert_eq!(KeyPress::from(shifted), parse_keys("C").unwrap()[0]);
    }

    #[test]
    fn default_keymap_has_no_ambiguous_bindings() {
        let keymap = Keymap::default();
        for screen in [
            Screens::MusicPlayer,
            Screens::Playlist,
            Screens::Search,
            Screens::DeviceLost,
            Screens::PlaylistViewer,
        ] {
            let bindings = keymap.bindings_for(screen).collect::<Vec<_>>();
            for a in &bindings {
                for b in &bindings {
                    assert!(
                        a.keys.len() >= b.keys.len() || !b.keys.starts_with(&a.keys),
                        "{:?} shadows a longer binding on {screen:?}",
                        a.action
                    );
                }
            }
        }
    }

    #[test]
    fn resolves_single_keys_per_screen() {
        let keymap = Keymap::default();
        let mut resolver = KeyResolver::default();
        assert_eq!(
            resolver.feed(&keymap, Screens::MusicPlayer, char_key('+')),
            KeyResult::Action(Action::VolumeUp, 1)
        );
        assert_eq!(
            resolver.feed(&keymap, Screens::Playlist, char_key('+')),
            KeyResult::Action(Action::Up, 1)
        );
        assert_eq!(
            resolver.feed(&keymap, Screens::DeviceLost, key(KeyCode::Esc)),
            KeyResult::Action(Action::Quit, 1)
        );
    }

    #[test]
    fn global_bindings_work_everywhere() {
        let keymap = Keymap::default();
        let mut resolver = KeyResolver::default();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            resolver.feed(&keymap, Screens::Search, ctrl_c),
            KeyResult::Action(Action::Quit, 1)
        );
    }

    #[test]
    fn counts_repeat_actions() {
        let keymap = Keymap::default();
        let mut resolver = KeyResolver::default();
        let results = feed_str(&mut resolver, &keymap, Screens::MusicPlayer, "10j");
        assert_eq!(
            results,
            vec![
                KeyResult::Consumed,
                KeyResult::Consumed,
                KeyResult::Action(Action::Down, 10)
            ]
        );
    }

    #[test]
    fn zero_alone_is_not_a_count() {
        let keymap = Keymap::default();
        let mut resolver = KeyResolver::default();
        assert_eq!(
            resolver.feed(&keymap, Screens::MusicPlayer, char_key('0')),
            KeyResult::Unbound(char_key('0'))
        );
    }

    #[test]
    fn counts_are_capped() {
        let keymap = Keymap::default();
        let mut resolver = KeyResolver::default();
        let results = feed_str(&mut resolver, &keymap, Screens::MusicPlayer, "99999999j");
        assert_eq!(
            results.last(),
            Some(&KeyResult::Action(Action::Down, MAX_COUNT))
        );
    }

    #[test]
    fn escape_cancels_a_count() {
        let keymap = Keymap::default();
        let mut resolver = KeyResolver::default();
        resolver.feed(&keymap, Screens::MusicPlayer, char_key('5'));
        assert_eq!(
            resolver.feed(&keymap, Screens::MusicPlayer, key(KeyCode::Esc)),
            KeyResult::Consumed
        );
        assert_eq!(
            resolver.feed(&keymap, Screens::MusicPlayer, char_key('j')),
            KeyResult::Action(Action::Down, 1)
        );
    }

    #[test]
    fn digits_are_text_in_search() {
        let keymap = Keymap::default();
        let mut resolver = KeyResolver::default();
        assert_eq!(
            resolver.feed(&keymap, Screens::Search, char_key('5')),
            KeyResult::Unbound(char_key('5'))
        );
        assert_eq!(
            resolver.feed(&keymap, Screens::Search, char_key('j')),
            KeyResult::Unbound(char_key('j'))
        );
    }

    #[test]
    fn resolves_key_sequences() {
        let mut keymap = Keymap::empty();
        keymap
            .bind(Some(Screens::MusicPlayer), "gg", Action::Up)
            .unwrap();
        let mut resolver = KeyResolver::default();
        let results = feed_str(&mut resolver, &keymap, Screens::MusicPlayer, "3gg");
        assert_eq!(
            results,
            vec![
                KeyResult::Consumed,
                KeyResult::Consumed,
                KeyResult::Action(Action::Up, 3)
            ]
        );
    }

    #[test]
    fn unknown_key_drops_the_sequence() {
        let mut keymap = Keymap::empty();
        keymap
            .bind(Some(Screens::MusicPlayer), "gg", Action::Up)
            .unwrap();
        keymap
            .bind(Some(Screens::MusicPlayer), "j", Action::Down)
            .unwrap();
        let mut resolver = KeyResolver::default();
        let results = feed_str(&mut resolver, &keymap, Screens::MusicPlayer, "gxj");
        assert_eq!(
            results,
            vec![
                KeyResult::Consumed,
                KeyResult::Consumed,
                KeyResult::Action(Action::Down, 1)
            ]
        );
    }
}
