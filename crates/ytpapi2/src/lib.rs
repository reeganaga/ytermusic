use std::{
    path::Path,
    string::FromUtf8Error,
    time::{SystemTime, UNIX_EPOCH},
};

use json_extractor::{
    extract_playlist_info, from_json, get_continuation, get_playlist, get_playlist_search,
    get_shelf_playlist_id, get_video, get_video_from_album, get_video_from_panel, Continuation,
};
use log::{debug, error, trace};
pub use reqwest::header::HeaderMap;
pub use reqwest::header::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use string_utils::StringUtils;

mod json_extractor;
mod string_utils;

pub use json_extractor::YoutubeMusicVideoRef;

pub type Result<T> = std::result::Result<T, YoutubeMusicError>;

const YTM_DOMAIN: &str = "https://music.youtube.com";

#[cfg(test)]
fn get_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    let file = std::fs::read_to_string("../headers.txt").unwrap();
    for header in file.lines() {
        if header.trim().is_empty() {
            continue;
        }
        let (key, value) = header.split_once(": ").unwrap();
        headers.insert(
            match key {
                "Cookie" => reqwest::header::COOKIE,
                "User-Agent" => reqwest::header::USER_AGENT,
                _ => {
                    println!("Unknown header key: {}", key);
                    continue;
                }
            },
            value.parse().unwrap(),
        );
    }
    headers
}

#[cfg(test)]
fn get_account_id() -> Option<String> {
    let file = std::fs::read_to_string("../account_id.txt").unwrap();
    let account_id = match std::fs::read_to_string(file) {
        Ok(id) => Some(id),
        Err(_) => None,
    };
    return account_id;
}

#[test]
fn advanced_like() {
    use tokio::runtime::Runtime;
    Runtime::new().unwrap().block_on(async {
        let ytm = YoutubeMusicInstance::new(get_headers(), get_account_id())
            .await
            .unwrap();
        println!("{}", ytm.compute_sapi_hash());
        let search = ytm
            .get_library(&Endpoint::MusicLibraryLanding, 0)
            .await
            .unwrap();
        assert_eq!(search.is_empty(), false);
        println!("{:?}", search[1]);
        println!("{:?}", ytm.get_playlist(&search[1], 0).await.unwrap());
    });
}

#[test]
fn advanced_test() {
    use tokio::runtime::Runtime;
    Runtime::new().unwrap().block_on(async {
        let ytm = YoutubeMusicInstance::new(get_headers(), get_account_id())
            .await
            .unwrap();
        let search = ytm.search("j'ai la danse qui va avec", 0).await.unwrap();
        assert_eq!(search.videos.is_empty(), false);
        assert_eq!(search.playlists.is_empty(), false);
        let playlist_contents = ytm.get_playlist(&search.playlists[1], 0).await.unwrap();
        println!("{:?}", playlist_contents);
    });
}

#[test]
fn home_test() {
    use tokio::runtime::Runtime;
    Runtime::new().unwrap().block_on(async {
        let ytm = YoutubeMusicInstance::new(get_headers(), get_account_id())
            .await
            .unwrap();
        let search = ytm.get_home(0).await.unwrap();
        println!("{:?}", search.playlists);
        assert_eq!(search.playlists.is_empty(), false);
        let playlist_contents = ytm.get_playlist(&search.playlists[0], 0).await.unwrap();
        println!("{:?}", playlist_contents);
    });
}

#[derive(Debug, Clone, PartialOrd, Eq, Ord, PartialEq, Hash, Serialize, Deserialize)]
pub struct YoutubeMusicPlaylistRef {
    pub name: String,
    pub subtitle: String,
    pub browse_id: String,
}

pub struct YoutubeMusicInstance {
    sapisid: String,
    innertube_api_key: String,
    client_version: String,
    cookies: String,
    account_id: Option<String>,
    client: reqwest::Client,
}

impl YoutubeMusicInstance {
    pub async fn from_header_file(path: &Path) -> Result<Self> {
        let mut headers = HeaderMap::new();
        for header in tokio::fs::read_to_string(path)
            .await
            .map_err(YoutubeMusicError::IoError)?
            .lines()
        {
            if let Some((key, value)) = header.split_once(": ") {
                headers.insert(
                    match key.to_lowercase().as_str() {
                        "cookie" => reqwest::header::COOKIE,
                        "user-agent" => reqwest::header::USER_AGENT,
                        _ => {
                            #[cfg(test)]
                            println!("Unknown header key: {key}");
                            continue;
                        }
                    },
                    value.parse().unwrap(),
                );
            }
        }
        if !headers.contains_key(reqwest::header::COOKIE) {
            return Err(YoutubeMusicError::InvalidHeaders);
        }
        if !headers.contains_key(reqwest::header::USER_AGENT) {
            headers.insert(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (X11; Linux x86_64; rv:108.0) Gecko/20100101 Firefox/108.0"
                    .parse()
                    .unwrap(),
            );
        }
        let account_path = path
            .parent()
            .unwrap_or(Path::new("../"))
            .join("account_id.txt");
        let account_id = match tokio::fs::read_to_string(account_path).await {
            Ok(mut id) => {
                if id.ends_with("\n") {
                    id.pop();
                    if id.ends_with("\r") {
                        id.pop();
                    }
                }
                Some(id)
            }
            Err(_) => None, //don't care if there is no files or nothing in the file
        };
        Self::new(headers, account_id).await
    }

    pub async fn new(headers: HeaderMap, account_id: Option<String>) -> Result<Self> {
        trace!("Creating new YoutubeMusicInstance");
        let rest_client = reqwest::ClientBuilder::default()
            .default_headers(headers.clone())
            .build()
            .map_err(YoutubeMusicError::RequestError)?;
        trace!("Fetching YoutubeMusic homepage");
        let response: String = rest_client
            .get(YTM_DOMAIN)
            .headers(headers.clone())
            .send()
            .await
            .map_err(YoutubeMusicError::RequestError)?
            .text()
            .await
            .map_err(YoutubeMusicError::RequestError)?;
        trace!("Fetched");

        if response.contains("<base href=\"https://accounts.google.com/v3/signin/\">")
            || response.contains("<base href=\"https://consent.youtube.com/\">")
        {
            error!("Need to login");
            return Err(YoutubeMusicError::NeedToLogin);
        }
        trace!("Parsing cookies");
        let cookies = headers
            .get("Cookie")
            .ok_or(YoutubeMusicError::NoCookieAttribute)?;
        let cookies_bytes = cookies.as_bytes();
        let cookies = String::from_utf8(cookies_bytes.to_vec())
            .map_err(YoutubeMusicError::InvalidCookie)?
            .to_string();
        let sapisid = cookies
            .between("SAPISID=", ";")
            .ok_or_else(|| YoutubeMusicError::NoSapsidInCookie)?;
        trace!("Cookies parsed! SAPISID: {}", sapisid);
        let innertube_api_key = response
            .between("INNERTUBE_API_KEY\":\"", "\"")
            .ok_or_else(|| YoutubeMusicError::CantFindInnerTubeApiKey(response.to_string()))?;
        trace!("Innertube API key: {}", innertube_api_key);
        let client_version = response
            .between("INNERTUBE_CLIENT_VERSION\":\"", "\"")
            .ok_or_else(|| {
                YoutubeMusicError::CantFindInnerTubeClientVersion(response.to_string())
            })?;
        trace!("Innertube client version: {}", client_version);
        // New file for brand accounts, maybe put it in config or headers.txt is better but more complex.
        trace!("account id {:?}", account_id);
        Ok(Self {
            sapisid: sapisid.to_string(),
            innertube_api_key: innertube_api_key.to_string(),
            client_version: client_version.to_string(),
            cookies,
            account_id,
            client: rest_client,
        })
    }
    fn compute_sapi_hash(&self) -> String {
        let start = SystemTime::now();
        let since_the_epoch = start
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let timestamp = since_the_epoch.as_secs();
        let mut hasher = Sha1::new();
        hasher.update(format!("{timestamp} {} {YTM_DOMAIN}", self.sapisid));
        let result = hasher.finalize();
        let mut hex = String::with_capacity(40);
        for byte in result {
            hex.push_str(&format!("{byte:02x}"));
        }
        trace!("Computed SAPI Hash{timestamp}_{hex}");
        format!("{timestamp}_{hex}")
    }
    async fn browse_continuation(
        &self,
        continuation: &Continuation,
        continuations: bool,
    ) -> Result<(Value, Vec<Continuation>)> {
        let playlist_json: Value =
            serde_json::from_str(&self.browse_continuation_raw(continuation).await?)
                .map_err(YoutubeMusicError::SerdeJson)?;
        debug!("Browse continuation response: {playlist_json}");
        if playlist_json.get("error").is_some() {
            error!("Error in browse_continuation");
            error!("{:?}", playlist_json);
            return Err(YoutubeMusicError::YoutubeMusicError(playlist_json));
        }
        let continuation = if continuations {
            from_json(&playlist_json, get_continuation)?
        } else {
            Vec::new()
        };
        Ok((playlist_json, continuation))
    }
    async fn browse_continuation_raw(
        &self,
        Continuation {
            continuation,
            click_tracking_params,
        }: &Continuation,
    ) -> Result<String> {
        trace!("Browse continuation {continuation}");
        let url = format!(
            "https://music.youtube.com/youtubei/v1/browse?ctoken={continuation}&continuation={continuation}&type=next&itct={click_tracking_params}&key={}&prettyPrint=false",
            self.innertube_api_key
        );
        // let body = format!(
        //     r#"{{"context":{{"client":{{"clientName":"WEB_REMIX","clientVersion":"{}"}}}}}}"#,
        //     self.client_version
        // );
        let body = self.context_body().to_string();
        self.client
            .post(&url)
            .header("Content-Type", "application/json")
            .header(
                "Authorization",
                format!("SAPISIDHASH {}", self.compute_sapi_hash()),
            )
            .header("X-Origin", "https://music.youtube.com")
            .header("Cookie", &self.cookies)
            .body(body)
            .send()
            .await
            .map_err(YoutubeMusicError::RequestError)?
            .text()
            .await
            .map_err(YoutubeMusicError::RequestError)
    }
    fn context_body(&self) -> Value {
        let mut context = json!({
            "client": {"clientName": "WEB_REMIX", "clientVersion": self.client_version}
        });
        if let Some(id) = &self.account_id {
            context["user"] = json!({ "onBehalfOfUser": id });
        }
        json!({ "context": context })
    }
    async fn browse_raw(&self, endpoint: &Endpoint) -> Result<String> {
        let endpoint_route = endpoint.get_route();
        trace!("Browse {endpoint_route}");
        let url = format!(
            "https://music.youtube.com/youtubei/v1/{endpoint_route}?key={}&prettyPrint=false",
            self.innertube_api_key
        );
        let mut body = self.context_body();
        for (key, value) in endpoint.get_fields() {
            body[key] = Value::String(value);
        }
        self.client
            .post(&url)
            .header("Content-Type", "application/json")
            .header(
                "Authorization",
                format!("SAPISIDHASH {}", self.compute_sapi_hash()),
            )
            .header("X-Origin", "https://music.youtube.com")
            .header("Cookie", &self.cookies)
            .body(body.to_string())
            .send()
            .await
            .map_err(YoutubeMusicError::RequestError)?
            .text()
            .await
            .map_err(YoutubeMusicError::RequestError)
    }
    async fn browse(
        &self,
        endpoint: &Endpoint,
        continuations: bool,
    ) -> Result<(serde_json::Value, Vec<Continuation>)> {
        let playlist_json: Value = serde_json::from_str(&self.browse_raw(endpoint).await?)
            .map_err(YoutubeMusicError::SerdeJson)?;
        debug!("Browse response: {playlist_json}");
        if playlist_json.get("error").is_some() {
            error!("Error in browse ({endpoint:?})");
            error!("{:?}", playlist_json);
            return Err(YoutubeMusicError::YoutubeMusicError(playlist_json));
        }
        let continuation = if continuations {
            from_json(&playlist_json, get_continuation)?
        } else {
            Vec::new()
        };
        Ok((playlist_json, continuation))
    }
    pub async fn get_library(
        &self,
        endpoint: &Endpoint,
        mut n_continuations: usize,
    ) -> Result<Vec<YoutubeMusicPlaylistRef>> {
        let (library_json, mut continuations) = self.browse(endpoint, n_continuations > 0).await?;
        trace!("Fetched library");
        debug!("Library response: {library_json}");
        debug!("Continuations: {continuations:?}");
        let mut library = from_json(&library_json, get_playlist)?;
        debug!("Library: {library:?}");
        while let Some(continuation) = continuations.pop() {
            n_continuations -= 1;
            trace!("Fetching continuation {continuation:?} ({endpoint:?})");
            let (library_json, new_continuations) = self
                .browse_continuation(&continuation, (n_continuations - 1) > 0)
                .await?;
            debug!("Library response: {library_json}");
            continuations.extend(new_continuations);
            let new_library = from_json(&library_json, get_playlist)?;
            trace!("Fetched {} playlists", new_library.len());
            debug!("Library response: {library_json}");
            library.extend(new_library);
            if n_continuations == 0 {
                break;
            }
        }

        Ok(library)
    }
    pub async fn get_playlist(
        &self,
        playlist: &YoutubeMusicPlaylistRef,
        n_continuations: usize,
    ) -> Result<Vec<YoutubeMusicVideoRef>> {
        self.get_playlist_raw(&playlist.browse_id, n_continuations)
            .await
    }
    pub async fn get_playlist_raw(
        &self,
        playlist_id: &str,
        mut n_continuations: usize,
    ) -> Result<Vec<YoutubeMusicVideoRef>> {
        let (playlist_json, mut continuations) = self
            .browse(
                &Endpoint::Playlist(playlist_id.to_string()),
                n_continuations > 0,
            )
            .await?;
        trace!("Fetched playlist");
        debug!("Playlist response: {playlist_json}");
        debug!("Continuations: {continuations:?}");
        let mut videos = parse_playlist(&playlist_json)?;

        debug!("Videos: {videos:?}");

        while let Some(continuation) = continuations.pop() {
            n_continuations -= 1;
            trace!("Fetching continuation {continuation:?}");
            let (playlist_json, new_continuations) = self
                .browse_continuation(&continuation, (n_continuations - 1) > 0)
                .await?;
            debug!("Playlist response: {playlist_json}");
            continuations.extend(new_continuations);
            let new_videos = parse_playlist(&playlist_json)?;
            trace!("Fetched {} videos", new_videos.len());
            debug!("Playlist response: {playlist_json}");
            videos.extend(new_videos);
            if n_continuations == 0 {
                break;
            }
        }

        Ok(videos)
    }
    pub async fn search(
        &self,
        search_query: &str,
        n_continuations: usize,
    ) -> Result<SearchResults> {
        self.search_filtered(search_query, SearchFilter::All, n_continuations)
            .await
    }
    pub async fn search_filtered(
        &self,
        search_query: &str,
        filter: SearchFilter,
        mut n_continuations: usize,
    ) -> Result<SearchResults> {
        let (search_json, mut continuations) = self
            .browse(&Endpoint::Search(search_query.to_string(), filter), false)
            .await?;
        debug!("Search response: {search_json}");
        let mut videos = from_json(&search_json, get_video)?;
        debug!("Videos: {videos:?}");
        let mut playlists = from_json(&search_json, get_playlist_search)?;
        debug!("Playlists: {playlists:?}");

        while let Some(continuation) = continuations.pop() {
            n_continuations -= 1;
            trace!("Fetching continuation {continuation:?}");
            let (search_json, new_continuations) =
                self.browse_continuation(&continuation, false).await?;
            trace!("Search response: {search_json}");
            continuations.extend(new_continuations);
            let new_videos = from_json(&search_json, get_video)?;
            debug!("Videos: {videos:?}");
            let new_playlists = from_json(&search_json, get_playlist_search)?;
            debug!("Playlists: {playlists:?}");
            videos.extend(new_videos);
            playlists.extend(new_playlists);
            if n_continuations == 0 {
                break;
            }
        }

        Ok(SearchResults { videos, playlists })
    }

    /// Songs of a radio ("mix") seeded from the given video.
    pub async fn get_radio(&self, video_id: &str) -> Result<Vec<YoutubeMusicVideoRef>> {
        let (json, _) = self
            .browse(&Endpoint::Radio(video_id.to_string()), false)
            .await?;
        from_json(&json, get_video_from_panel)
    }

    /// Songs behind any browse id: playlists, albums, or an artist's full song list.
    pub async fn get_browse_songs(
        &self,
        browse_id: &str,
        n_continuations: usize,
    ) -> Result<Vec<YoutubeMusicVideoRef>> {
        if BrowseKind::of(browse_id) != BrowseKind::Artist {
            return self.get_playlist_raw(browse_id, n_continuations).await;
        }
        let (artist_json, _) = self
            .browse(&Endpoint::Playlist(browse_id.to_string()), false)
            .await?;
        // The "Top songs" shelf links to a playlist with every song of the artist.
        if let Some(all_songs) = from_json(&artist_json, get_shelf_playlist_id)?.first() {
            let songs = self.get_playlist_raw(all_songs, n_continuations).await?;
            if !songs.is_empty() {
                return Ok(songs);
            }
        }
        from_json(&artist_json, get_video)
    }

    pub async fn get_home(&self, mut n_continuations: usize) -> Result<SearchResults> {
        let (home_json, mut continuations) = self
            .browse(&Endpoint::MusicHome, n_continuations > 0)
            .await?;
        debug!("Home response: {home_json}");
        let mut videos = from_json(&home_json, get_video)?;
        debug!("Videos: {videos:?}");
        let mut playlists = from_json(&home_json, get_playlist)?;
        debug!("Playlists: {playlists:?}");

        while let Some(continuation) = continuations.pop() {
            n_continuations -= 1;
            trace!("Fetching continuation {continuation:?}");
            let (home_json, new_continuations) = self
                .browse_continuation(&continuation, n_continuations > 0)
                .await?;
            debug!("Home response: {home_json}");
            continuations.extend(new_continuations);
            let new_videos = from_json(&home_json, get_video)?;
            debug!("Videos: {videos:?}");
            let new_playlists = from_json(&home_json, get_playlist)?;
            debug!("Playlists: {playlists:?}");
            videos.extend(new_videos);
            playlists.extend(new_playlists);
            if n_continuations == 0 {
                break;
            }
        }
        Ok(SearchResults { playlists, videos })
    }
}

fn parse_playlist(playlist_json: &Value) -> Result<Vec<YoutubeMusicVideoRef>> {
    let mut videos = from_json(playlist_json, get_video)?;
    let info = extract_playlist_info(playlist_json);
    for mut video in from_json(playlist_json, get_video_from_album)? {
        if videos.iter().any(|x| x.video_id == video.video_id) {
            continue;
        }
        if let Some((title, artist)) = info.as_ref() {
            if video.album.is_empty() {
                video.album = title.to_string();
            }
            if video.author.is_empty() {
                video.author = artist.to_string();
            }
        }
        videos.push(video);
    }
    Ok(videos)
}

#[derive(Debug, Clone, PartialOrd, Eq, Ord, PartialEq, Hash)]
pub struct SearchResults {
    pub videos: Vec<YoutubeMusicVideoRef>,
    pub playlists: Vec<YoutubeMusicPlaylistRef>,
}

/// Search result category, mapped to the YouTube Music search filter chips.
#[derive(Debug, Clone, Copy, PartialOrd, Eq, Ord, PartialEq, Hash, Default)]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Artists,
    Albums,
    FeaturedPlaylists,
    CommunityPlaylists,
}

impl SearchFilter {
    pub const ALL: [SearchFilter; 6] = [
        SearchFilter::All,
        SearchFilter::Songs,
        SearchFilter::Artists,
        SearchFilter::Albums,
        SearchFilter::FeaturedPlaylists,
        SearchFilter::CommunityPlaylists,
    ];
    pub fn label(&self) -> &'static str {
        match self {
            SearchFilter::All => "All",
            SearchFilter::Songs => "Songs",
            SearchFilter::Artists => "Artists",
            SearchFilter::Albums => "Albums",
            SearchFilter::FeaturedPlaylists => "Featured Playlists",
            SearchFilter::CommunityPlaylists => "Community Playlists",
        }
    }
    fn params(&self) -> Option<&'static str> {
        match self {
            SearchFilter::All => None,
            SearchFilter::Songs => Some("EgWKAQIIAWoSEAUQCRADEBAQBBAKEA4QFRAR"),
            SearchFilter::Artists => Some("EgWKAQIgAWoSEAUQCRADEBAQBBAKEA4QFRAR"),
            SearchFilter::Albums => Some("EgWKAQIYAWoSEAUQCRADEBAQBBAKEA4QFRAR"),
            SearchFilter::FeaturedPlaylists => Some("EgeKAQQoADgBahIQBRAJEAMQEBAEEAoQDhAVEBE="),
            SearchFilter::CommunityPlaylists => Some("EgeKAQQoAEABahIQBRAJEAMQEBAEEAoQDhAVEBE="),
        }
    }
}

/// What a browse id points to, derived from its prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseKind {
    Artist,
    Album,
    Playlist,
}

impl BrowseKind {
    pub fn of(browse_id: &str) -> Self {
        if browse_id.starts_with("UC") {
            BrowseKind::Artist
        } else if browse_id.starts_with("MPRE") {
            BrowseKind::Album
        } else {
            BrowseKind::Playlist
        }
    }
}

#[derive(Debug, Clone, PartialOrd, Eq, Ord, PartialEq, Hash)]
pub enum Endpoint {
    MusicLikedPlaylists,
    MusicHome,
    MusicLibraryLanding,
    Playlist(String),
    Search(String, SearchFilter),
    /// Radio ("mix") seeded from a video id.
    Radio(String),
}

impl Endpoint {
    fn get_fields(&self) -> Vec<(&'static str, String)> {
        match self {
            Endpoint::MusicLikedPlaylists => {
                vec![("browseId", "FEmusic_liked_playlists".to_owned())]
            }
            Endpoint::MusicLibraryLanding => {
                vec![("browseId", "FEmusic_library_landing".to_owned())]
            }
            Endpoint::Playlist(id) => vec![("browseId", id.to_owned())],
            Endpoint::MusicHome => vec![("browseId", "FEmusic_home".to_owned())],
            Endpoint::Search(query, filter) => {
                let mut fields = vec![("query", query.to_owned())];
                if let Some(params) = filter.params() {
                    fields.push(("params", params.to_owned()));
                }
                fields
            }
            Endpoint::Radio(video_id) => vec![
                ("videoId", video_id.to_owned()),
                ("playlistId", format!("RDAMVM{video_id}")),
            ],
        }
    }
    fn get_route(&self) -> &'static str {
        match self {
            Endpoint::Search(..) => "search",
            Endpoint::Radio(_) => "next",
            _ => "browse",
        }
    }
}

#[derive(Debug)]
pub enum YoutubeMusicError {
    RequestError(reqwest::Error),
    Other(String),
    NoCookieAttribute,
    NoSapsidInCookie,
    InvalidCookie(FromUtf8Error),
    NeedToLogin,
    CantFindInnerTubeApiKey(String),
    CantFindInnerTubeClientVersion(String),
    CantFindVisitorData(String),
    SerdeJson(serde_json::Error),
    IoError(std::io::Error),
    YoutubeMusicError(Value),
    InvalidHeaders,
}
