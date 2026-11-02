use std::fmt::Display;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::YoutubeMusicPlaylistRef;

/// Applies recursively the `transformer` function to the given json value
/// and returns the transformed values.
pub(crate) fn from_json<T: PartialEq>(
    json: &Value,
    transformer: impl Fn(&Value) -> Option<T>,
) -> crate::Result<Vec<T>> {
    /// Execute a function on each element of a json value recursively.
    /// When the function returns something, the value is added to the result.
    pub(crate) fn inner_crawl<T: PartialEq>(
        value: &Value,
        playlists: &mut Vec<T>,
        transformer: &impl Fn(&Value) -> Option<T>,
    ) {
        if let Some(e) = transformer(value) {
            // Maybe an hashset would be better
            if !playlists.contains(&e) {
                playlists.push(e);
            }
            return;
        }
        match value {
            Value::Array(a) => a
                .iter()
                .for_each(|x| inner_crawl(x, playlists, transformer)),
            Value::Object(a) => a
                .values()
                .for_each(|x| inner_crawl(x, playlists, transformer)),
            _ => (),
        }
    }
    let mut playlists = Vec::new();
    inner_crawl(json, &mut playlists, &transformer);
    Ok(playlists)
}

#[derive(Debug, Clone, PartialOrd, Eq, Ord, PartialEq, Hash, Serialize, Deserialize)]
pub struct YoutubeMusicVideoRef {
    pub title: String,
    pub author: String,
    pub album: String,
    pub video_id: String,
    pub duration: String,
}

impl Display for YoutubeMusicVideoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} | {}", self.author, self.title)
    }
}

/// Tries to extract a playlist from a json value.
/// Quite flexible to reduce odds of API change breaking this.
pub(crate) fn get_playlist(value: &Value) -> Option<YoutubeMusicPlaylistRef> {
    let object = value.as_object()?;
    let title_text = get_text(object.get("title")?, true, false)?;
    let subtitle = object
        .get("subtitle")
        .and_then(|x| get_text(x, false, false));
    let browse_id = &object
        .get("navigationEndpoint")
        .and_then(|x| x.get("browseEndpoint"))
        .and_then(|x| x.get("browseId"))
        .and_then(Value::as_str)?;
    Some(YoutubeMusicPlaylistRef {
        name: title_text,
        subtitle: subtitle.unwrap_or_default(),
        browse_id: browse_id.to_string(),
    })
}

#[derive(Debug, Clone, PartialOrd, Eq, Ord, PartialEq, Hash, Serialize, Deserialize)]
pub struct Continuation {
    pub(crate) continuation: String,
    pub(crate) click_tracking_params: String,
}

pub fn get_continuation(value: &Value) -> Option<Continuation> {
    let continuation = value
        .get("nextContinuationData")
        .and_then(|x| x.get("continuation"))
        .and_then(Value::as_str)?;
    let click_tracking_params = value
        .get("nextContinuationData")
        .and_then(|x| x.get("clickTrackingParams"))
        .and_then(Value::as_str)?;
    Some(Continuation {
        continuation: continuation.to_string(),
        click_tracking_params: click_tracking_params.to_string(),
    })
}

pub fn get_playlist_search(value: &Value) -> Option<YoutubeMusicPlaylistRef> {
    if value.get("playlistItemData").is_some() {
        return None;
    }
    let browse_id = &value
        .get("navigationEndpoint")
        .and_then(|x| x.get("browseEndpoint"))
        .and_then(|x| x.get("browseId"))
        .and_then(Value::as_str)?;
    let titles: Vec<String> = value
        .get("flexColumns")?
        .as_array()?
        .iter()
        .flat_map(|x| {
            x.get("musicResponsiveListItemFlexColumnRenderer")
                .and_then(|x| x.get("text"))
                .and_then(|x| get_text(x, false, false))
        })
        .collect();
    Some(YoutubeMusicPlaylistRef {
        name: titles.first()?.clone(),
        subtitle: titles.get(1)?.clone(),
        browse_id: browse_id.to_string(),
    })
}

pub fn extract_playlist_info(value: &Value) -> Option<(String, String)> {
    if let Some(header) = value
        .get("header")
        .and_then(|x| x.get("musicDetailHeaderRenderer"))
    {
        let title = get_text(header.get("title")?, false, false)?;
        let subtitles = header
            .get("subtitle")?
            .get("runs")?
            .as_array()?
            .iter()
            .flat_map(|x| get_text(x, false, false))
            .filter(|x| x != " • ")
            .collect::<Vec<String>>();
        return Some((title, subtitles.get(1)?.clone()));
    }
    // Newer album pages use a responsive header nested in the tabs.
    from_json(value, |x| {
        let header = x.get("musicResponsiveHeaderRenderer")?;
        let title = get_text(header.get("title")?, false, false)?;
        let artist = header
            .get("straplineTextOne")
            .and_then(|x| get_text(x, false, false))
            .unwrap_or_default();
        Some((title, artist))
    })
    .ok()?
    .into_iter()
    .next()
}

pub fn get_video_from_album(value: &Value) -> Option<YoutubeMusicVideoRef> {
    let video_id = value
        .get("playlistItemData")
        .and_then(|x| x.get("videoId"))
        .and_then(Value::as_str)?;
    let title: Vec<String> = value
        .get("flexColumns")?
        .as_array()?
        .iter()
        .flat_map(|x| {
            x.get("musicResponsiveListItemFlexColumnRenderer")
                .and_then(|x| x.get("text"))
                .and_then(|x| get_text(x, false, false))
        })
        .collect();
    Some(YoutubeMusicVideoRef {
        title: title.first()?.clone(),
        author: String::new(),
        album: String::new(),
        video_id: video_id.to_string(),
        duration: get_fixed_duration(value).unwrap_or_default(),
    })
}

/// Duration stored in the fixed column of playlist / album rows ("3:41").
fn get_fixed_duration(value: &Value) -> Option<String> {
    value
        .get("fixedColumns")?
        .as_array()?
        .iter()
        .filter_map(|x| {
            x.get("musicResponsiveListItemFixedColumnRenderer")
                .and_then(|x| x.get("text"))
                .and_then(|x| get_text(x, false, false))
        })
        .find(|x| is_duration(x))
}

/// Whether the text looks like a track length: `3:41` or `1:02:03`.
pub(crate) fn is_duration(text: &str) -> bool {
    let parts: Vec<&str> = text.split(':').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
        && parts[1..].iter().all(|x| x.len() == 2)
}

/// Text runs of a flex column grouped by their " • " separators.
fn flex_column_groups(column: &Value) -> Vec<String> {
    let Some(runs) = column
        .as_object()
        .and_then(|x| x.values().next())
        .and_then(|x| x.get("text"))
        .and_then(|x| x.get("runs"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut groups = vec![String::new()];
    for run in runs {
        let Some(text) = run.get("text").and_then(Value::as_str) else {
            continue;
        };
        if text.trim() == "•" {
            groups.push(String::new());
        } else if let Some(last) = groups.last_mut() {
            last.push_str(text);
        }
    }
    groups
        .into_iter()
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// Item types prefixed to the subtitle in unfiltered search results.
const ITEM_TYPES_SONG: [&str; 3] = ["Song", "Video", "Episode"];
const ITEM_TYPES_OTHER: [&str; 6] = ["Artist", "Album", "Single", "EP", "Playlist", "Profile"];

fn is_count_text(text: &str) -> bool {
    ["plays", "views", "play", "view"]
        .iter()
        .any(|x| text.ends_with(x))
}

/// Tries to extract a song from a radio queue (`playlistPanelVideoRenderer`).
pub(crate) fn get_video_from_panel(value: &Value) -> Option<YoutubeMusicVideoRef> {
    let panel = value.get("playlistPanelVideoRenderer")?;
    let video_id = panel.get("videoId")?.as_str()?;
    let title = get_text(panel.get("title")?, false, false)?;
    let byline: Vec<String> = panel
        .get("longBylineText")
        .and_then(|x| x.get("runs"))
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|x| x.get("text").and_then(Value::as_str))
                .collect::<String>()
                .split(" • ")
                .map(|x| x.trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    Some(YoutubeMusicVideoRef {
        title,
        author: byline.first().cloned().unwrap_or_default(),
        album: byline
            .get(1)
            .filter(|x| !is_count_text(x))
            .cloned()
            .unwrap_or_default(),
        video_id: video_id.to_string(),
        duration: panel
            .get("lengthText")
            .and_then(|x| get_text(x, false, false))
            .unwrap_or_default(),
    })
}

/// Finds the playlist linked from a shelf's "show all" button (artist "Top songs").
pub(crate) fn get_shelf_playlist_id(value: &Value) -> Option<String> {
    let id = value
        .get("musicShelfRenderer")?
        .get("bottomEndpoint")?
        .get("browseEndpoint")?
        .get("browseId")?
        .as_str()?;
    id.starts_with("VL").then(|| id.to_string())
}

/// Tries to extract the text from a json value.
/// text_clean: Weather to include singleton text.
/// dot: Weather to use the dotted text instead of the space
fn get_text(value: &Value, text_clean: bool, dot: bool) -> Option<String> {
    if let Some(e) = value.as_str() {
        Some(e.to_string())
    } else {
        let obj = value.as_object()?;
        if let Some(e) = obj.get("text") {
            if text_clean && obj.values().count() == 1 {
                return None;
            }
            get_text(e, text_clean, dot)
        } else if let Some(e) = obj.get("runs") {
            let k = e
                .as_array()?
                .iter()
                .flat_map(|x| get_text(x, text_clean, dot))
                .collect::<Vec<_>>();
            if k.is_empty() {
                None
            } else {
                Some(join_clean(&k, dot))
            }
        } else {
            None
        }
    }
}

fn join_clean(strings: &[String], dot: bool) -> String {
    strings
        .iter()
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(if dot { " • " } else { " " })
}

/// Tries to find a video id in the json
pub fn get_videoid(value: &Value) -> Option<String> {
    match value {
        Value::Array(e) => e.iter().find_map(get_videoid),
        Value::Object(e) => e
            .get("videoId")
            .and_then(Value::as_str)
            .map(|x| x.to_string())
            .or_else(|| e.values().find_map(get_videoid)),
        _ => None,
    }
}

/// Tries to extract a video from a json value.
/// Quite flexible to reduce odds of API change breaking this.
pub(crate) fn get_video(value: &Value) -> Option<YoutubeMusicVideoRef> {
    let columns = value.as_object()?.get("flexColumns")?.as_array()?;
    let title = columns
        .first()
        .and_then(|x| x.as_object())
        .and_then(|x| x.values().next())
        .and_then(|x| get_text(x, true, true))?;
    let mut groups = flex_column_groups(columns.get(1)?);
    // Unfiltered search results start the subtitle with the item type.
    if let Some(first) = groups.first() {
        if ITEM_TYPES_OTHER.contains(&first.as_str()) {
            return None;
        }
        if ITEM_TYPES_SONG.contains(&first.as_str()) && groups.len() > 1 {
            groups.remove(0);
        }
    }
    let mut duration = String::new();
    if groups.last().is_some_and(|x| is_duration(x)) {
        duration = groups.pop().unwrap_or_default();
    }
    if duration.is_empty() {
        duration = get_fixed_duration(value).unwrap_or_default();
    }
    groups.retain(|x| !is_count_text(x));
    let author = if groups.is_empty() {
        String::new()
    } else {
        groups.remove(0)
    };
    let album = groups.into_iter().next().or_else(|| {
        columns
            .get(2)
            .map(flex_column_groups)
            .and_then(|x| x.into_iter().next())
            .filter(|x| !is_count_text(x) && !is_duration(x))
    });
    if author.is_empty() {
        return None;
    }
    Some(YoutubeMusicVideoRef {
        video_id: get_videoid(value)?,
        title,
        author,
        album: album.unwrap_or_default(),
        duration,
    })
}

#[test]
fn duration_detection() {
    assert!(is_duration("3:41"));
    assert!(is_duration("1:02:03"));
    assert!(!is_duration("174M plays"));
    assert!(!is_duration("3:4"));
}
