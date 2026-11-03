//! Client for the Better Lyrics API (<https://better-lyrics.boidu.dev>):
//! word-synced lyrics from Apple Music style TTML or QQ Music.
//!
//! Without an API key only songs already in its cache are served (people
//! using the Better Lyrics browser extension fill it); other songs answer
//! 401 and are treated as not found.

use log::{debug, info};
use reqwest::StatusCode;
use serde::Deserialize;

use crate::{qrc, title, ttml, Fetched, Source, Track};

const API: &str = "https://api.betterlyrics.org";

#[derive(Debug, Deserialize)]
struct Response {
    /// TTML document (default provider)
    #[serde(default)]
    ttml: Option<String>,
    /// Provider specific lyrics (QQ: QRC document)
    #[serde(default)]
    lyrics: Option<String>,
}

/// Looks the song up on `source` ([`Source::BetterLyrics`] or
/// [`Source::BetterLyricsQq`]). `api_key` is sent when not empty.
pub async fn fetch(
    client: &reqwest::Client,
    source: Source,
    track: &Track<'_>,
    api_key: Option<&str>,
) -> Fetched {
    let path = match source {
        Source::BetterLyrics => "getLyrics",
        Source::BetterLyricsQq => "qq/getLyrics",
        _ => return Fetched::Missing,
    };
    let artist = title::clean_artist(track.artist);
    let name = title::clean_title(track.title, &artist);
    info!("{} lookup: {name} / {artist}", source.label());

    let mut query = vec![("s", name), ("a", artist), ("al", track.album.to_owned())];
    if let Some(duration) = track.duration_secs {
        query.push(("d", duration.to_string()));
    }
    let mut request = client.get(format!("{API}/{path}")).query(&query);
    if let Some(key) = api_key.filter(|x| !x.trim().is_empty()) {
        request = request.header("X-API-Key", key.trim());
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(e) => return Fetched::Failed(e.to_string()),
    };
    let status = response.status();
    if is_missing(status) {
        debug!("{}: {status}", source.label());
        return Fetched::Missing;
    }
    if !status.is_success() {
        return Fetched::Failed(format!("HTTP {status}"));
    }
    let body: Response = match response.json().await {
        Ok(body) => body,
        Err(e) => return Fetched::Failed(e.to_string()),
    };
    let parsed = match source {
        Source::BetterLyrics => body.ttml.map(|x| ttml::parse(&x, source)),
        _ => body.lyrics.map(|x| qrc::parse(&x, source)),
    };
    match parsed {
        Some(Ok(lyrics)) => Fetched::Found(lyrics),
        Some(Err(e)) => Fetched::Failed(e),
        None => Fetched::Missing,
    }
}

/// 401: not in the cache and no API key; 404: no match; 422: bad query.
fn is_missing(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_statuses() {
        assert!(is_missing(StatusCode::UNAUTHORIZED));
        assert!(is_missing(StatusCode::NOT_FOUND));
        assert!(!is_missing(StatusCode::TOO_MANY_REQUESTS));
        assert!(!is_missing(StatusCode::BAD_GATEWAY));
    }

    #[test]
    fn parses_response_body() {
        let body: Response = serde_json::from_str(r#"{"ttml":"<tt/>","score":0.9}"#).unwrap();
        assert_eq!(body.ttml.as_deref(), Some("<tt/>"));
        let body: Response =
            serde_json::from_str(r#"{"lyrics":"<QrcInfos/>","provider":"qq"}"#).unwrap();
        assert!(body.ttml.is_none() && body.lyrics.is_some());
    }
}
