//! Parser for QRC, QQ Music's word-synced lyrics (served by Better Lyrics).
//!
//! The lyrics sit in the `LyricContent` attribute of a `<Lyric_1>` element,
//! one line per text line: `[start,duration]word(start,duration)word(...)`,
//! times in milliseconds. The first lines are usually credits
//! (`Title - Artist`, `Lyrics by：...`) and are dropped.

use crate::{lrc, Line, Lyrics, Source, Word};

/// QQ's text for songs without lyrics.
const INSTRUMENTAL: &str = "纯音乐";
/// Credit line labels.
const CREDITS: &[&str] = &[
    "作词",
    "作曲",
    "编曲",
    "制作",
    "词：",
    "曲：",
    "lyrics by",
    "composed by",
    "produced by",
    "written by",
    "arranged by",
    "lyricist",
    "composer",
    "producer",
];

/// Credit labels of fan covers (`原曲：` original song, `翻唱：` cover
/// singers, `中翻` Chinese translation), which QQ sometimes returns instead of
/// the original song.
const COVER: &[&str] = &["原曲", "翻唱", "中翻", "翻译填词"];

/// Whether the lyrics are a fan cover rather than the song itself: looks for
/// cover credits in the first lines.
pub fn is_cover(document: &str) -> bool {
    let content = lyric_content(document).unwrap_or_else(|| document.to_owned());
    content
        .lines()
        .filter(|x| !x.trim().is_empty())
        .take(15)
        .any(|line| COVER.iter().any(|x| line.contains(x)))
}

/// Parses a QRC document (or plain LRC text, which QQ sometimes returns).
pub fn parse(document: &str, source: Source) -> Result<Lyrics, String> {
    let content = lyric_content(document).unwrap_or_else(|| document.to_owned());
    let mut lines: Vec<Line> = content.lines().filter_map(parse_line).collect();
    if lines.is_empty() {
        // Not QRC: maybe LRC
        lines = lrc::parse(&content)
            .into_iter()
            .map(|(time, text)| Line::new(Some(time), text))
            .collect();
    }
    let start = lines
        .iter()
        .enumerate()
        .position(|(i, line)| !is_credit(i, &line.text))
        .unwrap_or(lines.len());
    lines.drain(..start);
    if lines.len() == 1 && lines[0].text.contains(INSTRUMENTAL) {
        return Ok(Lyrics::instrumental(source));
    }
    if lines.is_empty() {
        return Err("QRC: no lyrics line".to_owned());
    }
    lines.sort_by_key(|x| x.time_ms);
    Ok(Lyrics::new(source, lines))
}

/// The `LyricContent` attribute, `None` when the document is not QRC.
///
/// Read by hand: QQ leaves raw quotes inside the attribute
/// (`Juan "Saucy" Peña`), which XML parsers stop at. The value runs to the
/// last quote closing the element.
fn lyric_content(document: &str) -> Option<String> {
    const OPEN: &str = "LyricContent=\"";
    let start = document.find(OPEN)? + OPEN.len();
    let rest = &document[start..];
    let end = rest.rfind("\"/>").or_else(|| rest.rfind("\">"))?;
    let raw = &rest[..end];
    Some(match quick_xml::escape::unescape(raw) {
        Ok(text) => text.into_owned(),
        Err(_) => raw.to_owned(),
    })
}

/// `[351,1039]I (351,397)love(748,642)` -> a line at 351ms with two words.
fn parse_line(raw: &str) -> Option<Line> {
    let rest = raw.trim().strip_prefix('[')?;
    let (head, body) = rest.split_once(']')?;
    let (start, _) = parse_pair(head)?;
    let mut words = Vec::new();
    let mut text_start = 0;
    let mut search = 0;
    while let Some(open) = body[search..].find('(').map(|i| i + search) {
        let Some(close) = body[open..].find(')').map(|i| i + open) else {
            break;
        };
        match parse_pair(&body[open + 1..close]) {
            Some((word_start, duration)) => {
                let text = &body[text_start..open];
                if !text.is_empty() {
                    words.push(Word {
                        start_ms: word_start,
                        end_ms: word_start + duration,
                        text: text.to_owned(),
                    });
                }
                text_start = close + 1;
                search = close + 1;
            }
            // A parenthesis in the lyrics themselves
            None => search = open + 1,
        }
    }
    if words.is_empty() {
        let text = body.trim();
        return (!text.is_empty()).then(|| Line::new(Some(start), text));
    }
    if let Some(first) = words.first_mut() {
        first.text = first.text.trim_start().to_owned();
    }
    if let Some(last) = words.last_mut() {
        last.text = last.text.trim_end().to_owned();
    }
    words.retain(|x| !x.text.is_empty());
    let text: String = words.iter().map(|x| x.text.as_str()).collect();
    let mut line = Line::new(Some(start), text);
    line.words = words;
    Some(line)
}

/// `351,1039` -> (351, 1039).
fn parse_pair(text: &str) -> Option<(u64, u64)> {
    let (a, b) = text.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// Credit lines at the top: `Title - Artist` first, then `Label：names`.
fn is_credit(index: usize, text: &str) -> bool {
    let lower = text.to_lowercase();
    (index == 0 && text.contains(" - "))
        || text.contains('：')
        || CREDITS.iter().any(|x| lower.starts_with(x))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<QrcInfos>
<QrcHeadInfo SaveTime="153" Version="100"/>
<LyricInfo LyricCount="1">
<Lyric_1 LyricType="1" LyricContent="[ti:E85 (Explicit)]
[ar:Don Toliver]
[offset:0]
[0,51]E(0,10)85 ((10,10)Explicit) - (21,10)Don (31,10)Toliver(41,10)
[52,205]Lyrics (52,10)by：(62,10)Someone(72,10)
[351,1039]I (351,397)love(748,642)
[3323,800]Tryin&apos; (3323,246)(so (3569,300)hard)(3869,254)
[5000,500]Juan "Saucy"(5000,500)
"/>
</LyricInfo>
</QrcInfos>"#;

    #[test]
    fn parses_words_and_drops_credits() {
        let lyrics = parse(DOC, Source::BetterLyricsQq).unwrap();
        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[2].text, "Juan \"Saucy\"");
        let line = &lyrics.lines[0];
        assert_eq!(line.time_ms, Some(351));
        assert_eq!(line.text, "I love");
        assert_eq!(line.words.len(), 2);
        assert_eq!((line.words[1].start_ms, line.words[1].end_ms), (748, 1390));
        let line = &lyrics.lines[1];
        assert_eq!(line.text, "Tryin' (so hard)");
        let joined: String = line.words.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(joined, line.text);
    }

    #[test]
    fn detects_fan_covers() {
        let doc = r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[4871,1000]— 宅舞曲目中翻企划07 —(4871,1000)
[5990,1000]原曲：花になって - 绿黄色社会(5990,1000)
[10748,1000]翻唱：蚂蚁(10748,1000)
[15403,1000]屏默的呼吸(15403,1000)"/></LyricInfo></QrcInfos>"#;
        assert!(is_cover(doc));
        assert!(!is_cover(DOC));
    }

    #[test]
    fn instrumental_and_lrc_fallback() {
        let doc = r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[0,1000]此歌曲为没有填词的纯音乐，请您欣赏(0,1000)"/></LyricInfo></QrcInfos>"#;
        assert!(parse(doc, Source::BetterLyricsQq).unwrap().instrumental);
        let lyrics = parse("[00:01.00]hello\n[00:02.00]world", Source::BetterLyricsQq).unwrap();
        assert_eq!(lyrics.texts(), vec!["hello", "world"]);
        assert!(parse("<QrcInfos/>", Source::BetterLyricsQq).is_err());
    }
}
