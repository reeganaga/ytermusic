//! Song lyrics: time-synced lyrics from LRCLIB, LRC parsing, translation and
//! romanization through Google Translate, and an on-disk cache.
//!
//! The YouTube Music fallback lives in `ytpapi2`; its plain text goes through
//! [`Lyrics::from_plain`].

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use log::warn;
use serde::{Deserialize, Serialize};

pub mod lrc;
pub mod lrclib;
pub mod title;
pub mod translate;

pub use lrclib::Track;
pub use reqwest::Client as HttpClient;
pub use translate::Translation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Lrclib,
    YoutubeMusic,
}

impl Source {
    /// Name used in cache file names.
    pub fn key(self) -> &'static str {
        match self {
            Source::Lrclib => "lrclib",
            Source::YoutubeMusic => "youtube",
        }
    }

    /// Name shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Source::Lrclib => "LRCLIB",
            Source::YoutubeMusic => "YouTube Music",
        }
    }
}

/// Answer of one lyrics source.
#[derive(Debug)]
pub enum Fetched {
    Found(Lyrics),
    /// The source has no lyrics for this song
    Missing,
    /// Temporary failure (network, rate limit...): worth asking again later
    Failed(String),
}

impl From<Result<Option<Lyrics>, String>> for Fetched {
    fn from(result: Result<Option<Lyrics>, String>) -> Self {
        match result {
            Ok(Some(lyrics)) => Fetched::Found(lyrics),
            Ok(None) => Fetched::Missing,
            Err(e) => Fetched::Failed(e),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// Start time; `None` for unsynced lyrics
    pub time_ms: Option<u64>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lyrics {
    pub source: Source,
    #[serde(default)]
    pub instrumental: bool,
    pub lines: Vec<Line>,
    /// Translations already fetched, by target language
    #[serde(default)]
    pub translations: HashMap<String, Translation>,
}

impl Lyrics {
    pub fn new(source: Source, lines: Vec<Line>) -> Self {
        Self {
            source,
            instrumental: false,
            lines,
            translations: HashMap::new(),
        }
    }

    pub fn instrumental(source: Source) -> Self {
        Self {
            instrumental: true,
            ..Self::new(source, Vec::new())
        }
    }

    /// Unsynced lyrics from plain text, one line per text line.
    pub fn from_plain(source: Source, text: &str) -> Self {
        let lines = text
            .trim()
            .lines()
            .map(|line| Line {
                time_ms: None,
                text: line.trim().to_owned(),
            })
            .collect();
        Self::new(source, lines)
    }

    /// Whether the lines have timestamps (auto-scroll possible).
    pub fn synced(&self) -> bool {
        self.lines.first().is_some_and(|x| x.time_ms.is_some())
    }

    /// Start times of the lines (synced lyrics only).
    pub fn times(&self) -> Vec<u64> {
        self.lines.iter().filter_map(|x| x.time_ms).collect()
    }

    pub fn texts(&self) -> Vec<String> {
        self.lines.iter().map(|x| x.text.clone()).collect()
    }
}

/// HTTP client for LRCLIB and Google Translate.
pub fn http_client() -> HttpClient {
    reqwest::Client::builder()
        .user_agent(concat!(
            "ytermusic/",
            env!("CARGO_PKG_VERSION"),
            " (https://github.com/ccgauche/ytermusic)"
        ))
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default()
}

/// A "not found" answer is trusted this long, then the source is asked again
/// (its database grows over time).
pub const MISS_TTL: Duration = Duration::from_secs(3 * 24 * 3600);

/// Lyrics files, one JSON per video id and source (`{id}.{source}.json`),
/// plus `{id}.{source}.miss` markers for sources that had nothing.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, video_id: &str, source: Source, extension: &str) -> PathBuf {
        // Video ids are [A-Za-z0-9_-], keep the file name safe anyway
        let name: String = video_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        self.dir
            .join(format!("{name}.{}.{extension}", source.key()))
    }

    pub fn load(&self, video_id: &str, source: Source) -> Option<Lyrics> {
        let text = std::fs::read_to_string(self.path(video_id, source, "json")).ok()?;
        serde_json::from_str(&text)
            .inspect_err(|e| warn!("Corrupted lyrics cache for {video_id}: {e}"))
            .ok()
    }

    /// Saves under the lyrics' own source and clears its miss marker.
    pub fn save(&self, video_id: &str, lyrics: &Lyrics) {
        if let Err(e) = write_json(&self.path(video_id, lyrics.source, "json"), lyrics) {
            warn!("Can't write lyrics cache for {video_id}: {e}");
        }
        let _ = std::fs::remove_file(self.path(video_id, lyrics.source, "miss"));
    }

    /// Remembers that `source` has no lyrics for the video, for [`MISS_TTL`].
    pub fn mark_missing(&self, video_id: &str, source: Source) {
        let path = self.path(video_id, source, "miss");
        let result = std::fs::create_dir_all(&self.dir).and_then(|_| std::fs::write(&path, ""));
        if let Err(e) = result {
            warn!("Can't write lyrics miss marker for {video_id}: {e}");
        }
    }

    /// Whether `source` had no lyrics for the video less than [`MISS_TTL`] ago.
    pub fn is_missing(&self, video_id: &str, source: Source) -> bool {
        std::fs::metadata(self.path(video_id, source, "miss"))
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age < MISS_TTL)
    }
}

fn write_json(path: &Path, lyrics: &Lyrics) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(lyrics).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_lyrics() {
        let lyrics = Lyrics::from_plain(Source::YoutubeMusic, "\n a \n\nb\n");
        assert!(!lyrics.synced());
        assert_eq!(lyrics.texts(), vec!["a", "", "b"]);
    }

    #[test]
    fn cache_roundtrip() {
        let dir = std::env::temp_dir().join(format!("lyrics-test-{}", std::process::id()));
        let cache = Cache::new(&dir);
        let mut lyrics = Lyrics::new(
            Source::Lrclib,
            vec![Line {
                time_ms: Some(1000),
                text: "x".into(),
            }],
        );
        lyrics
            .translations
            .insert("en".into(), Translation::default());
        cache.save("abc_-1/..", &lyrics);
        assert_eq!(
            cache.load("abc_-1/..", Source::Lrclib),
            Some(lyrics.clone())
        );
        assert!(dir.join("abc_-1.lrclib.json").exists());
        assert_eq!(cache.load("abc_-1/..", Source::YoutubeMusic), None);
        assert_eq!(cache.load("missing", Source::Lrclib), None);

        assert!(!cache.is_missing("x", Source::YoutubeMusic));
        cache.mark_missing("x", Source::YoutubeMusic);
        assert!(cache.is_missing("x", Source::YoutubeMusic));
        assert!(!cache.is_missing("x", Source::Lrclib));
        // Finding lyrics later clears the marker
        let found = Lyrics::new(Source::YoutubeMusic, Vec::new());
        cache.save("x", &found);
        assert!(!cache.is_missing("x", Source::YoutubeMusic));
        let _ = std::fs::remove_dir_all(dir);
    }
}
