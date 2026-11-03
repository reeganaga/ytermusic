//! Parser for TTML lyrics (Apple Music style, served by Better Lyrics).
//!
//! Shape: `<p begin end itunes:key ttm:agent>` per line holding one
//! `<span begin end>` per word or syllable; the whitespace between spans
//! matters (`Dy-</span><span>na` is one word). Background vocals are a
//! `<span ttm:role="x-bg">` around timed spans. The head may hold singers
//! (`<ttm:agent type="person|group" xml:id>`), translations and
//! transliterations (`<text for="L1">` matching the lines' `itunes:key`).

use std::collections::HashMap;

use quick_xml::{
    events::{BytesStart, Event},
    Reader,
};

use crate::{Align, Extra, Line, Lyrics, Source, Word};

/// Parses a TTML document. Fails on invalid XML or when it has no line.
pub fn parse(ttml: &str, source: Source) -> Result<Lyrics, String> {
    let mut parser = Parser::default();
    let mut reader = Reader::from_str(ttml);
    reader.config_mut().trim_text(false);
    loop {
        match reader.read_event().map_err(|e| format!("TTML: {e}"))? {
            Event::Start(e) => {
                parser.start(&e);
                parser.depth += 1;
            }
            Event::Empty(e) => parser.empty(&e),
            Event::End(e) => {
                parser.depth = parser.depth.saturating_sub(1);
                parser.end(e.local_name().as_ref());
            }
            Event::Text(e) => {
                let text = e.unescape().map_err(|e| format!("TTML: {e}"))?;
                parser.text(&text);
            }
            Event::CData(e) => parser.text(&String::from_utf8_lossy(&e)),
            Event::Eof => break,
            _ => {}
        }
    }
    parser.finish(source)
}

/// Text of a line (or of a translation / transliteration): timed words, or
/// plain text for line-synced documents.
#[derive(Default)]
struct Collector {
    words: Vec<Word>,
    plain: String,
    background: Vec<Word>,
    background_plain: String,
    /// Depth of the `x-bg` span while inside it
    background_depth: Option<usize>,
}

impl Collector {
    fn group(&mut self) -> (&mut Vec<Word>, &mut String) {
        if self.background_depth.is_some() {
            (&mut self.background, &mut self.background_plain)
        } else {
            (&mut self.words, &mut self.plain)
        }
    }

    fn start_word(&mut self, start_ms: u64, end_ms: u64) {
        self.group().0.push(Word {
            start_ms,
            end_ms,
            text: String::new(),
        });
    }

    fn text(&mut self, text: &str) {
        let (words, plain) = self.group();
        let target = match words.last_mut() {
            Some(word) => &mut word.text,
            None => plain,
        };
        // Spacing between spans: a single space, whatever the source has
        if text.trim().is_empty() {
            if !target.is_empty() && !target.ends_with(char::is_whitespace) {
                target.push(' ');
            }
            return;
        }
        target.push_str(text);
    }

    /// Text and words, background vocals appended in parentheses.
    fn finish(self) -> (String, Vec<Word>) {
        let mut words = trim_words(self.words);
        let background = trim_words(self.background);
        if words.is_empty() && background.is_empty() {
            let mut text = self.plain.trim().to_owned();
            push_background(&mut text, self.background_plain.trim());
            return (text, Vec::new());
        }
        if !background.is_empty() {
            if let Some(last) = words.last_mut() {
                last.text.push(' ');
            }
            let count = background.len();
            let wrap = !background[0].text.starts_with('(');
            for (i, mut word) in background.into_iter().enumerate() {
                if wrap && i == 0 {
                    word.text.insert(0, '(');
                }
                if wrap && i + 1 == count {
                    word.text.push(')');
                }
                words.push(word);
            }
        }
        let text = words.iter().map(|x| x.text.as_str()).collect();
        (text, words)
    }
}

fn push_background(text: &mut String, background: &str) {
    if background.is_empty() {
        return;
    }
    if !text.is_empty() {
        text.push(' ');
    }
    if background.starts_with('(') {
        text.push_str(background);
    } else {
        text.push_str(&format!("({background})"));
    }
}

/// Drops the spaces around the words of a group and the empty words.
fn trim_words(words: Vec<Word>) -> Vec<Word> {
    let mut words: Vec<Word> = words
        .into_iter()
        .filter(|x| !x.text.trim().is_empty())
        .collect();
    if let Some(first) = words.first_mut() {
        first.text = first.text.trim_start().to_owned();
    }
    if let Some(last) = words.last_mut() {
        last.text = last.text.trim_end().to_owned();
    }
    words
}

struct PendingLine {
    key: Option<String>,
    begin: Option<u64>,
    agent: Option<String>,
    text: Collector,
}

enum Extras {
    Translation(String),
    Transliteration,
}

#[derive(Default)]
struct Parser {
    depth: usize,
    /// Singer id -> type (`person`, `group`...)
    agents: HashMap<String, String>,
    lines: Vec<(Option<String>, Option<String>, Line)>,
    line: Option<PendingLine>,
    extras: Option<Extras>,
    /// `<text for>` being read: line key and text
    extra_text: Option<(String, Collector)>,
    /// Language -> line key -> text
    translations: HashMap<String, HashMap<String, String>>,
    romanization_language: Option<String>,
    romanizations: HashMap<String, Extra>,
}

impl Parser {
    fn collector(&mut self) -> Option<&mut Collector> {
        match (&mut self.extra_text, &mut self.line) {
            (Some((_, text)), _) => Some(text),
            (None, Some(line)) => Some(&mut line.text),
            _ => None,
        }
    }

    fn start(&mut self, e: &BytesStart) {
        match e.local_name().as_ref() {
            b"p" => {
                self.line = Some(PendingLine {
                    key: attr(e, b"key"),
                    begin: attr(e, b"begin").and_then(|x| parse_time(&x)),
                    agent: attr(e, b"agent"),
                    text: Collector::default(),
                })
            }
            b"span" => {
                let depth = self.depth;
                let background = attr(e, b"role").is_some_and(|x| x == "x-bg");
                let times = attr(e, b"begin")
                    .and_then(|x| parse_time(&x))
                    .zip(attr(e, b"end").and_then(|x| parse_time(&x)));
                if let Some(text) = self.collector() {
                    if background {
                        text.background_depth = Some(depth);
                    } else if let Some((start, end)) = times {
                        text.start_word(start, end);
                    }
                }
            }
            b"translation" => self.extras = attr(e, b"lang").map(Extras::Translation),
            b"transliteration" => {
                let lang = attr(e, b"lang").unwrap_or_default();
                // Only one romanization is shown: keep the first language
                if self
                    .romanization_language
                    .as_ref()
                    .is_none_or(|x| *x == lang)
                {
                    self.romanization_language = Some(lang);
                    self.extras = Some(Extras::Transliteration);
                }
            }
            b"text" if self.extras.is_some() => {
                if let Some(key) = attr(e, b"for") {
                    self.extra_text = Some((key, Collector::default()));
                }
            }
            _ => self.empty(e),
        }
    }

    fn empty(&mut self, e: &BytesStart) {
        if e.local_name().as_ref() == b"agent" {
            if let Some(id) = attr(e, b"id") {
                let kind = attr(e, b"type").unwrap_or_else(|| "person".to_owned());
                self.agents.insert(id, kind);
            }
        }
    }

    fn text(&mut self, text: &str) {
        if let Some(collector) = self.collector() {
            collector.text(text);
        }
    }

    fn end(&mut self, name: &[u8]) {
        let depth = self.depth;
        match name {
            b"span" => {
                if let Some(text) = self.collector() {
                    if text.background_depth == Some(depth) {
                        text.background_depth = None;
                    }
                }
            }
            b"p" => {
                if let Some(pending) = self.line.take() {
                    let (text, words) = pending.text.finish();
                    let time = pending.begin.or(words.first().map(|x| x.start_ms));
                    let mut line = Line::new(time, text);
                    line.words = words;
                    self.lines.push((pending.key, pending.agent, line));
                }
            }
            b"text" => {
                if let Some((key, collector)) = self.extra_text.take() {
                    let (text, words) = collector.finish();
                    if text.is_empty() {
                        return;
                    }
                    match &self.extras {
                        Some(Extras::Translation(lang)) => {
                            self.translations
                                .entry(lang.clone())
                                .or_default()
                                .insert(key, text);
                        }
                        Some(Extras::Transliteration) => {
                            self.romanizations.insert(key, Extra { text, words });
                        }
                        None => {}
                    }
                }
            }
            b"translation" | b"transliteration" => self.extras = None,
            _ => {}
        }
    }

    fn finish(self, source: Source) -> Result<Lyrics, String> {
        if self.lines.is_empty() {
            return Err("TTML: no lyrics line".to_owned());
        }
        let aligns = alignments(&self.agents, &self.lines);
        let mut keyed: Vec<(Option<String>, Line)> = Vec::new();
        for (key, agent, mut line) in self.lines {
            line.align = agent
                .and_then(|x| aligns.get(&x).copied())
                .unwrap_or_default();
            // Latin-script lines come back as their own text: nothing to show
            line.romanized = key
                .as_ref()
                .and_then(|k| self.romanizations.get(k))
                .filter(|x| x.text.trim().to_lowercase() != line.text.trim().to_lowercase())
                .cloned();
            keyed.push((key, line));
        }
        // Lines are in order, but keep the times sorted for the active line search
        if keyed.iter().all(|(_, x)| x.time_ms.is_some()) {
            keyed.sort_by_key(|(_, x)| x.time_ms);
        }
        let builtin_translations = self
            .translations
            .into_iter()
            .map(|(lang, texts)| {
                let lines = keyed
                    .iter()
                    .map(|(key, _)| key.as_ref().and_then(|k| texts.get(k).cloned()))
                    .collect();
                (lang, lines)
            })
            .collect();
        let mut lyrics = Lyrics::new(source, keyed.into_iter().map(|(_, x)| x).collect());
        lyrics.builtin_translations = builtin_translations;
        Ok(lyrics)
    }
}

/// Duet layout: the first singer on the left, the other singers on the
/// right, groups (everyone together) in the middle. Songs with one singer
/// stay on the left.
fn alignments(
    agents: &HashMap<String, String>,
    lines: &[(Option<String>, Option<String>, Line)],
) -> HashMap<String, Align> {
    let mut used: Vec<&String> = Vec::new();
    for agent in lines.iter().filter_map(|(_, agent, _)| agent.as_ref()) {
        if !used.contains(&agent) {
            used.push(agent);
        }
    }
    let mut aligns = HashMap::new();
    if used.len() < 2 {
        return aligns;
    }
    let mut first = true;
    for agent in used {
        let align = if agents.get(agent).is_some_and(|x| x == "group") {
            Align::Center
        } else if first {
            first = false;
            Align::Left
        } else {
            Align::Right
        };
        aligns.insert(agent.clone(), align);
    }
    aligns
}

/// Unescaped value of the attribute with this local name (`xml:lang` ->
/// `lang`, `itunes:key` -> `key`).
fn attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|x| x.key.local_name().as_ref() == name)
        .and_then(|x| x.unescape_value().ok())
        .map(|x| x.into_owned())
}

/// `1:16.656`, `00:01:30.500`, `3.168`, `3.168s` or `250ms` in milliseconds.
pub fn parse_time(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(ms) = text.strip_suffix("ms") {
        return ms.trim().parse::<f64>().ok().map(|x| x.round() as u64);
    }
    let text = text.strip_suffix('s').unwrap_or(text);
    let mut seconds = 0.0;
    for part in text.split(':') {
        let value: f64 = part.trim().parse().ok()?;
        if value < 0.0 {
            return None;
        }
        seconds = seconds * 60.0 + value;
    }
    Some((seconds * 1000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" itunes:timing="Word" xml:lang="ja">"#;

    fn doc(head: &str, body: &str) -> String {
        format!("{HEAD}<head><metadata>{head}</metadata></head><body><div>{body}</div></body></tt>")
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("0.200"), Some(200));
        assert_eq!(parse_time("1:16.656"), Some(76_656));
        assert_eq!(parse_time("00:01:30.500"), Some(90_500));
        assert_eq!(parse_time("3.5s"), Some(3500));
        assert_eq!(parse_time("250ms"), Some(250));
        assert_eq!(parse_time("x"), None);
    }

    #[test]
    fn words_keep_spacing_and_syllables() {
        let ttml = doc(
            "",
            r#"<p begin="0.2" end="3" itunes:key="L1"><span begin="0.2" end="0.5">Dy-</span><span begin="0.5" end="0.8">na</span> <span begin="0.8" end="1.2">mite &amp; more</span></p>"#,
        );
        let lyrics = parse(&ttml, Source::BetterLyrics).unwrap();
        let line = &lyrics.lines[0];
        assert_eq!(line.time_ms, Some(200));
        assert_eq!(line.text, "Dy-na mite & more");
        let words: Vec<&str> = line.words.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(words, ["Dy-", "na ", "mite & more"]);
        assert_eq!((line.words[2].start_ms, line.words[2].end_ms), (800, 1200));
        assert_eq!(line.align, Align::Left);
    }

    #[test]
    fn background_vocals_in_parentheses() {
        let ttml = doc(
            "",
            r#"<p begin="1" end="4"><span begin="1" end="2">I'm</span> <span begin="2" end="3">fine</span> <span ttm:role="x-bg"><span begin="3" end="3.5">oh</span> <span begin="3.5" end="4">yeah</span></span></p>"#,
        );
        let line = &parse(&ttml, Source::BetterLyrics).unwrap().lines[0];
        assert_eq!(line.text, "I'm fine (oh yeah)");
        let joined: String = line.words.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(joined, line.text);
        assert_eq!(line.words.last().unwrap().start_ms, 3500);
    }

    #[test]
    fn line_synced_document() {
        let ttml = doc(
            "",
            r#"<p begin="1.5" end="3">Hello there</p><p begin="3" end="5">General <span ttm:role="x-bg">Kenobi</span></p>"#,
        );
        let lyrics = parse(&ttml, Source::BetterLyrics).unwrap();
        assert!(lyrics.synced());
        assert_eq!(lyrics.lines[0].text, "Hello there");
        assert!(lyrics.lines[0].words.is_empty());
        assert_eq!(lyrics.lines[1].text, "General (Kenobi)");
        assert_eq!(lyrics.times(), vec![1500, 3000]);
    }

    #[test]
    fn duet_alignment() {
        let head = r#"<ttm:agent type="person" xml:id="v1"/><ttm:agent type="person" xml:id="v2"/><ttm:agent type="group" xml:id="v1000"/>"#;
        let ttml = doc(
            head,
            r#"<p begin="1" ttm:agent="v1">a</p><p begin="2" ttm:agent="v2">b</p><p begin="3" ttm:agent="v1000">c</p><p begin="4" ttm:agent="v1">d</p>"#,
        );
        let aligns: Vec<Align> = parse(&ttml, Source::BetterLyrics)
            .unwrap()
            .lines
            .iter()
            .map(|x| x.align)
            .collect();
        assert_eq!(
            aligns,
            [Align::Left, Align::Right, Align::Center, Align::Left]
        );

        let solo = doc(
            r#"<ttm:agent type="person" xml:id="v1"/>"#,
            r#"<p begin="1" ttm:agent="v1">a</p>"#,
        );
        assert_eq!(
            parse(&solo, Source::BetterLyrics).unwrap().lines[0].align,
            Align::Left
        );
    }

    #[test]
    fn builtin_translation_and_romanization() {
        let head = r#"<iTunesMetadata xmlns="http://music.apple.com/lyric-ttml-internal"><translations><translation type="replacement" xml:lang="en"><text for="L1">I love you</text></translation></translations><transliterations><transliteration xml:lang="ko-Latn"><text for="L1"><span xmlns="http://www.w3.org/ns/ttml" begin="1" end="2">saranghae</span> <span xmlns="http://www.w3.org/ns/ttml" begin="2" end="3">yo</span></text></transliteration></transliterations></iTunesMetadata>"#;
        let ttml = doc(
            head,
            r#"<p begin="1" end="3" itunes:key="L1"><span begin="1" end="2">사랑해</span> <span begin="2" end="3">요</span></p><p begin="4" itunes:key="L2"><span begin="4" end="5">음</span></p>"#,
        );
        let lyrics = parse(&ttml, Source::BetterLyrics).unwrap();
        assert_eq!(
            lyrics.builtin_translation("en-US"),
            Some(&vec![Some("I love you".to_owned()), None])
        );
        assert!(lyrics.builtin_translation("fr").is_none());
        let roman = lyrics.lines[0].romanized.as_ref().unwrap();
        assert_eq!(roman.text, "saranghae yo");
        assert_eq!(roman.words.len(), 2);
        assert_eq!(roman.words[1].start_ms, 2000);
        assert!(lyrics.lines[1].romanized.is_none());
        assert!(lyrics.has_builtin_romanization());
    }

    #[test]
    fn rejects_empty_and_invalid() {
        assert!(parse(&doc("", ""), Source::BetterLyrics).is_err());
        assert!(parse("<tt><p>unclosed", Source::BetterLyrics).is_err());
    }
}
