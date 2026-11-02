//! Client for lrclib.net: free lyrics database with time-synced (LRC) lyrics.

use log::{debug, info};
use serde::Deserialize;

use crate::{lrc, title, Line, Lyrics, Source};

const API: &str = "https://lrclib.net/api";

/// Song to look up. Title and artist are cleaned before searching.
pub struct Track<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub duration_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibTrack {
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    synced_lyrics: Option<String>,
}

impl LrclibTrack {
    fn has_synced(&self) -> bool {
        self.synced_lyrics
            .as_deref()
            .is_some_and(|x| !x.trim().is_empty())
    }

    fn into_lyrics(self) -> Option<Lyrics> {
        if self.instrumental {
            return Some(Lyrics::instrumental(Source::Lrclib));
        }
        if let Some(synced) = self.synced_lyrics.filter(|x| !x.trim().is_empty()) {
            let lines: Vec<Line> = lrc::parse(&synced)
                .into_iter()
                .map(|(time, text)| Line {
                    time_ms: Some(time),
                    text,
                })
                .collect();
            if !lines.is_empty() {
                return Some(Lyrics::new(Source::Lrclib, lines));
            }
        }
        let plain = self.plain_lyrics.filter(|x| !x.trim().is_empty())?;
        Some(Lyrics::from_plain(Source::Lrclib, &plain))
    }
}

/// Finds lyrics on LRCLIB: exact match on title/artist/duration first, then a
/// search preferring synced lyrics with a close duration. `Ok(None)` when
/// nothing matches.
pub async fn fetch(client: &reqwest::Client, track: &Track<'_>) -> Result<Option<Lyrics>, String> {
    let artist = title::clean_artist(track.artist);
    let name = title::clean_title(track.title, &artist);
    info!("LRCLIB lookup: {name} / {artist}");

    let mut query = vec![
        ("track_name", name.clone()),
        ("artist_name", artist.clone()),
    ];
    if !track.album.is_empty() {
        query.push(("album_name", track.album.to_owned()));
    }
    if let Some(duration) = track.duration_secs {
        query.push(("duration", duration.to_string()));
    }
    let response = client
        .get(format!("{API}/get"))
        .query(&query)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // An exact match without synced lyrics is kept as a fallback: another
    // entry of the same song may have them.
    let mut exact = None;
    if response.status().is_success() {
        let found: LrclibTrack = response.json().await.map_err(|e| e.to_string())?;
        if found.instrumental || found.has_synced() {
            return Ok(found.into_lyrics());
        }
        exact = Some(found);
    } else {
        debug!("LRCLIB get: {}", response.status());
    }

    let results: Vec<LrclibTrack> = match search(client, &format!("{name} {artist}")).await {
        Ok(results) => results,
        Err(e) if exact.is_some() => {
            debug!("LRCLIB search failed: {e}");
            Vec::new()
        }
        Err(e) => return Err(e),
    };
    let found = match (pick_search_result(results, track.duration_secs), exact) {
        (Some(result), _) if result.has_synced() => Some(result),
        (result, exact) => exact.or(result),
    };
    Ok(found.and_then(LrclibTrack::into_lyrics))
}

async fn search(client: &reqwest::Client, query: &str) -> Result<Vec<LrclibTrack>, String> {
    client
        .get(format!("{API}/search"))
        .query(&[("q", query)])
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}

/// Best search result: duration within 3 seconds (when known), synced lyrics
/// preferred over plain ones, then the API's order.
fn pick_search_result(results: Vec<LrclibTrack>, duration: Option<u64>) -> Option<LrclibTrack> {
    let close = |x: &LrclibTrack| match (duration, x.duration) {
        (Some(expected), Some(actual)) => (actual - expected as f64).abs() <= 3.0,
        _ => true,
    };
    let mut candidates: Vec<LrclibTrack> = results.into_iter().filter(close).collect();
    let index = candidates
        .iter()
        .position(LrclibTrack::has_synced)
        .unwrap_or(0);
    (index < candidates.len()).then(|| candidates.swap_remove(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(duration: f64, synced: bool) -> LrclibTrack {
        LrclibTrack {
            duration: Some(duration),
            instrumental: false,
            plain_lyrics: Some(format!("plain {duration}")),
            synced_lyrics: synced.then(|| format!("[00:01.00]synced {duration}")),
        }
    }

    #[test]
    fn picks_close_synced_result() {
        let picked = pick_search_result(
            vec![track(100.0, true), track(200.0, false), track(201.0, true)],
            Some(200),
        )
        .unwrap();
        assert_eq!(picked.duration, Some(201.0));
        let picked = pick_search_result(vec![track(200.0, false)], Some(200)).unwrap();
        assert!(!picked.has_synced());
        assert!(pick_search_result(vec![track(100.0, true)], Some(200)).is_none());
        assert!(pick_search_result(vec![track(100.0, true)], None).is_some());
    }

    #[test]
    fn converts_response() {
        let json = r#"{"id":1,"trackName":"a","duration":200.0,"instrumental":false,
            "plainLyrics":"x\ny","syncedLyrics":"[00:01.00]x\n[00:02.00]y"}"#;
        let found: LrclibTrack = serde_json::from_str(json).unwrap();
        let lyrics = found.into_lyrics().unwrap();
        assert!(lyrics.synced());
        assert_eq!(lyrics.lines.len(), 2);

        let json = r#"{"instrumental":true,"plainLyrics":null,"syncedLyrics":null}"#;
        let found: LrclibTrack = serde_json::from_str(json).unwrap();
        assert!(found.into_lyrics().unwrap().instrumental);
    }
}
