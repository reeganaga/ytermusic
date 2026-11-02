//! Cleans YouTube titles and channel names into what lyrics databases use.

/// Tokens that mark a bracketed part of a title as noise, e.g. `(Official Video)`.
const NOISE_EXACT: &[&str] = &["mv", "m/v", "pv", "hd", "4k", "ft", "ft."];
/// Prefixes with the same role: `lyric` covers `lyrics`, `remaster` covers `remastered`.
const NOISE_PREFIX: &[&str] = &[
    "official", "video", "lyric", "audio", "visuali", "feat", "remaster", "explicit",
];

fn is_noise(group: &str) -> bool {
    group
        .to_lowercase()
        .split(|c: char| c.is_whitespace() || "-:,|".contains(c))
        .any(|token| {
            NOISE_EXACT.contains(&token) || NOISE_PREFIX.iter().any(|p| token.starts_with(p))
        })
}

/// `夜に駆ける (Official Music Video) [HD]` -> `夜に駆ける`.
/// `YOASOBI「群青」Official Music Video` -> `群青`.
/// `Artist - Song` -> `Song` when `Artist` is the given artist.
pub fn clean_title(title: &str, artist: &str) -> String {
    // Japanese style quotes hold the actual song name
    if let Some(inner) = between(title, '「', '」').or_else(|| between(title, '『', '』')) {
        if !inner.trim().is_empty() {
            return inner.trim().to_owned();
        }
    }
    let mut out = strip_noise_groups(title);
    if let Some((left, right)) = out.split_once(" - ") {
        if !artist.is_empty() && left.trim().eq_ignore_ascii_case(artist.trim()) {
            out = right.to_owned();
        }
    }
    // Trailing "feat. X" outside brackets
    let lower = out.to_lowercase();
    for marker in [" feat. ", " ft. ", " feat "] {
        if let Some(index) = lower.find(marker) {
            out.truncate(index);
            break;
        }
    }
    let cleaned = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        title.trim().to_owned()
    } else {
        cleaned
    }
}

/// `YOASOBI - Topic` -> `YOASOBI`, `A, B & C` -> `A` (main artist).
pub fn clean_artist(artist: &str) -> String {
    let artist = artist.split(" • ").next().unwrap_or(artist);
    let artist = artist.strip_suffix(" - Topic").unwrap_or(artist);
    let main = artist
        .split([',', '&', '×'])
        .next()
        .unwrap_or(artist)
        .split(" x ")
        .next()
        .unwrap_or(artist);
    main.trim().to_owned()
}

fn between(text: &str, open: char, close: char) -> Option<&str> {
    let start = text.find(open)? + open.len_utf8();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

/// Removes `(...)`, `[...]` and `【...】` groups containing a noise word.
fn strip_noise_groups(title: &str) -> String {
    let mut out = String::new();
    let mut rest = title;
    loop {
        let open = rest.find(['(', '[', '【']);
        let Some(open) = open else {
            out.push_str(rest);
            break;
        };
        let open_char = rest[open..].chars().next().unwrap_or('(');
        let close_char = match open_char {
            '(' => ')',
            '[' => ']',
            _ => '】',
        };
        let after = open + open_char.len_utf8();
        let Some(close) = rest[after..].find(close_char).map(|i| i + after) else {
            out.push_str(rest);
            break;
        };
        let noisy = is_noise(&rest[after..close]);
        out.push_str(&rest[..open]);
        if !noisy {
            out.push_str(&rest[open..close + close_char.len_utf8()]);
        }
        rest = &rest[close + close_char.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_noise() {
        assert_eq!(
            clean_title("Blinding Lights (Official Video)", "The Weeknd"),
            "Blinding Lights"
        );
        assert_eq!(clean_title("Song [MV] (feat. Someone)", "X"), "Song");
        assert_eq!(clean_title("Song 【Official Lyric Video】", "X"), "Song");
        assert_eq!(clean_title("Song (Remastered 2011)", "X"), "Song");
        assert_eq!(clean_title("Song ft. Other", "X"), "Song");
    }

    #[test]
    fn keeps_meaningful_brackets() {
        assert_eq!(clean_title("Song (Acoustic)", "X"), "Song (Acoustic)");
        assert_eq!(clean_title("Song (Live)", "X"), "Song (Live)");
    }

    #[test]
    fn japanese_quotes_and_artist_prefix() {
        assert_eq!(
            clean_title("YOASOBI「群青」Official Music Video", "YOASOBI"),
            "群青"
        );
        assert_eq!(clean_title("Adele - Hello", "Adele"), "Hello");
        assert_eq!(clean_title("Hello - Live", "Adele"), "Hello - Live");
    }

    #[test]
    fn artists() {
        assert_eq!(clean_artist("YOASOBI - Topic"), "YOASOBI");
        assert_eq!(clean_artist("A, B & C"), "A");
        assert_eq!(clean_artist("A x B"), "A");
        assert_eq!(clean_artist("Artist • Album"), "Artist");
    }
}
