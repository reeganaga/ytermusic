//! Translation and romanization through Google Translate's public `gtx`
//! endpoint (the one used by browser extensions). Unofficial: it needs no key
//! but may be rate limited or change without notice.

use log::{debug, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const URL: &str = "https://translate.googleapis.com/translate_a/single";
/// Characters per request; lines are grouped up to this size.
const CHUNK_CHARS: usize = 1500;

/// Translation and romanization of every lyric line, aligned with the lines.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Translation {
    /// Language detected by Google (`ja`, `ko`, `zh-CN`...)
    pub language: Option<String>,
    /// `None` for a line that needs no translation (empty, or same text)
    pub translated: Vec<Option<String>>,
    /// `None` for a line written in Latin script
    pub romanized: Vec<Option<String>>,
}

/// One request's result: the translated or romanized text and the detected
/// language.
#[derive(Debug, PartialEq)]
struct Response {
    text: String,
    language: Option<String>,
}

/// Translates `lines` to `target` (a language code like `en`) and romanizes
/// them. Per chunk of lines: one translation request (lines joined by
/// newlines, which Google keeps) and, for non-Latin lyrics, one romanization
/// request (joined by [`ROMAN_SEPARATOR`], since Google drops newlines there).
/// When Google merges or splits lines, that chunk is retried line by line.
pub async fn translate(
    client: &reqwest::Client,
    lines: &[String],
    target: &str,
) -> Result<Translation, String> {
    let mut translated = vec![None; lines.len()];
    let mut romanized = vec![None; lines.len()];
    let mut language = None;

    for chunk in chunks(lines) {
        let texts: Vec<&str> = chunk.iter().map(|&i| lines[i].trim()).collect();

        let response = request(client, &texts.join("\n"), target, Mode::Translate).await?;
        language = language.or(response.language);
        match split_exact(&response.text, '\n', texts.len()) {
            Some(parts) => assign(&mut translated, &chunk, parts),
            None => {
                debug!("Translation merged lines, retrying line by line");
                for &i in &chunk {
                    let response =
                        request(client, lines[i].trim(), target, Mode::Translate).await?;
                    translated[i] = Some(response.text.trim().to_owned());
                }
            }
        }

        if !texts.iter().any(|x| needs_romanization(x)) {
            continue;
        }
        let joined = texts.join(&format!(" {ROMAN_SEPARATOR} "));
        let response = request(client, &joined, target, Mode::Romanize).await?;
        match split_exact(&response.text, ROMAN_SEPARATOR, texts.len()) {
            Some(parts) => assign(&mut romanized, &chunk, parts),
            None => {
                debug!("Romanization merged lines, retrying line by line");
                for &i in chunk.iter().filter(|&&i| needs_romanization(&lines[i])) {
                    let response = request(client, lines[i].trim(), target, Mode::Romanize).await?;
                    romanized[i] = Some(response.text.trim().to_owned());
                }
            }
        }
    }

    Ok(finish(lines, translated, romanized, language, target))
}

/// Line separator kept as is in Google's romanization output.
const ROMAN_SEPARATOR: char = '⏎';

fn split_exact(text: &str, separator: char, count: usize) -> Option<Vec<String>> {
    let parts: Vec<String> = text
        .trim()
        .split(separator)
        .map(|x| x.trim().trim_start_matches('-').trim_start().to_owned())
        .collect();
    (parts.len() == count).then_some(parts)
}

fn assign(slots: &mut [Option<String>], chunk: &[usize], parts: Vec<String>) {
    for (&i, part) in chunk.iter().zip(parts) {
        slots[i] = Some(part);
    }
}

/// Drops what should not be shown: translations into the song's own language,
/// translations equal to the original, romanization of Latin-script lines.
fn finish(
    lines: &[String],
    translated: Vec<Option<String>>,
    romanized: Vec<Option<String>>,
    language: Option<String>,
    target: &str,
) -> Translation {
    let same_language = language
        .as_deref()
        .is_some_and(|lang| base_language(lang) == base_language(target));
    let translated = translated
        .into_iter()
        .zip(lines)
        .map(|(t, original)| {
            t.filter(|t| !same_language && !t.is_empty() && !same_text(t, original))
        })
        .collect();
    let romanized = romanized
        .into_iter()
        .zip(lines)
        .map(|(r, original)| {
            r.filter(|r| needs_romanization(original) && !r.is_empty() && !same_text(r, original))
        })
        .collect();
    Translation {
        language,
        translated,
        romanized,
    }
}

/// `zh-CN` -> `zh`, `en_US` -> `en`.
pub fn base_language(code: &str) -> String {
    code.split(['-', '_'])
        .next()
        .unwrap_or(code)
        .to_ascii_lowercase()
}

fn same_text(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Whether the text has letters outside the Latin script (kana, kanji,
/// hangul, cyrillic, arabic...).
pub fn needs_romanization(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_alphabetic() && c as u32 > 0x024F && !(0x1E00..=0x1EFF).contains(&(c as u32)))
}

/// Groups of indices of the non-empty lines, each under `CHUNK_CHARS`.
fn chunks(lines: &[String]) -> Vec<Vec<usize>> {
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let mut size = 0;
    for (i, line) in lines.iter().enumerate() {
        let len = line.trim().chars().count();
        if len == 0 {
            continue;
        }
        match chunks.last_mut() {
            Some(chunk) if size + len < CHUNK_CHARS => chunk.push(i),
            _ => {
                chunks.push(vec![i]);
                size = 0;
            }
        }
        size += len + 1;
    }
    chunks
}

#[derive(Clone, Copy)]
enum Mode {
    Translate,
    Romanize,
}

async fn request(
    client: &reqwest::Client,
    text: &str,
    target: &str,
    mode: Mode,
) -> Result<Response, String> {
    let dt = match mode {
        Mode::Translate => "t",
        Mode::Romanize => "rm",
    };
    let response = client
        .post(URL)
        .query(&[
            ("client", "gtx"),
            ("sl", "auto"),
            ("tl", target),
            ("dt", dt),
        ])
        .form(&[("q", text)])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        warn!("Translate request failed: {}", response.status());
        return Err(format!("translate: HTTP {}", response.status()));
    }
    let json: Value = response.json().await.map_err(|e| e.to_string())?;
    parse_response(&json, mode).ok_or_else(|| "translate: unexpected response".to_owned())
}

/// Response shape: `[[["translated","original",..], ..], null, "ja", ..]` for
/// translations, `[[[null,null,null,"source in latin letters"]], null, "ja"]`
/// for romanizations.
fn parse_response(json: &Value, mode: Mode) -> Option<Response> {
    let segments = json.get(0)?.as_array()?;
    let mut text = String::new();
    for segment in segments {
        let part = match mode {
            Mode::Translate => segment.get(0),
            Mode::Romanize if segment.get(0).is_some_and(Value::is_null) => segment.get(3),
            Mode::Romanize => None,
        };
        if let Some(part) = part.and_then(Value::as_str) {
            text.push_str(part);
        }
    }
    Some(Response {
        text,
        language: json.get(2).and_then(Value::as_str).map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_responses() {
        let json = json!([
            [
                ["As if sinking\n", "沈むように\n", null, null, 3],
                ["On a night", "夜に", null, null, 3]
            ],
            null,
            "ja"
        ]);
        let response = parse_response(&json, Mode::Translate).unwrap();
        assert_eq!(response.text, "As if sinking\nOn a night");
        assert_eq!(response.language.as_deref(), Some("ja"));
        assert_eq!(
            split_exact(&response.text, '\n', 2),
            Some(vec!["As if sinking".to_owned(), "On a night".to_owned()])
        );

        let json = json!([[[null, null, null, "Shizumu yō ni ⏎ yoru ni"]], null, "ja"]);
        let response = parse_response(&json, Mode::Romanize).unwrap();
        assert_eq!(
            split_exact(&response.text, ROMAN_SEPARATOR, 2),
            Some(vec!["Shizumu yō ni".to_owned(), "yoru ni".to_owned()])
        );
        assert_eq!(split_exact(&response.text, ROMAN_SEPARATOR, 3), None);
        assert!(parse_response(&json!({}), Mode::Translate).is_none());
    }

    #[test]
    fn chunks_skip_empty_lines() {
        let lines: Vec<String> = ["a", "", "b", "  "].iter().map(|x| x.to_string()).collect();
        assert_eq!(chunks(&lines), vec![vec![0, 2]]);
        let long = vec!["x".repeat(1000), "y".repeat(1000)];
        assert_eq!(chunks(&long), vec![vec![0], vec![1]]);
    }

    #[test]
    fn romanization_detection() {
        assert!(needs_romanization("夜に駆ける"));
        assert!(needs_romanization("사랑해"));
        assert!(needs_romanization("Привет"));
        assert!(!needs_romanization("Hello, café déjà vu"));
        assert!(!needs_romanization("Tiếng Việt"));
        assert!(!needs_romanization("123 !?"));
    }

    #[test]
    fn hides_useless_lines() {
        let lines: Vec<String> = ["夜に駆ける", "OK", ""]
            .iter()
            .map(|x| x.to_string())
            .collect();
        let t = finish(
            &lines,
            vec![Some("Run into the night".into()), Some("OK".into()), None],
            vec![Some("yoru ni kakeru".into()), Some("OK".into()), None],
            Some("ja".into()),
            "en",
        );
        assert_eq!(
            t.translated,
            vec![Some("Run into the night".into()), None, None]
        );
        assert_eq!(t.romanized, vec![Some("yoru ni kakeru".into()), None, None]);

        let english: Vec<String> = vec!["Hello".into()];
        let t = finish(
            &english,
            vec![Some("Halo".into())],
            vec![None],
            Some("en-US".into()),
            "en",
        );
        assert_eq!(t.translated, vec![None]);
    }
}
