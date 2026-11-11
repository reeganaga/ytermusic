//! `ytermusic --lyrics-view`: a second terminal showing the lyrics, the cover
//! and play controls of the running player.
//!
//! The player listens on a unix socket (`server`) and, while a viewer is
//! connected, sends it JSON lines (`ToView`): the song, its lyrics when they
//! change, and a playback anchor (position, paused) the viewer extrapolates
//! from. The viewer answers with its buttons and keys (`ToHost`).
//! Lyrics are fetched once, by the player, then shared.

pub mod server;
pub mod view;

use std::{io::Read, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    consts::CACHE_DIR,
    term::lyrics::{ProviderStatus, State},
};

/// Socket the player listens on: the runtime dir when there is one (private
/// to the user), the cache dir otherwise.
pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(|| CACHE_DIR.clone())
        .join("ytermusic.sock")
}

/// The playing song.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Song {
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub album: String,
}

/// Player to viewer.
#[derive(Serialize)]
pub enum ToViewRef<'a> {
    Song(Option<&'a Song>),
    Lyrics(&'a State),
    Playback(Playback),
}

/// Player to viewer, as the viewer reads it (same JSON as [`ToViewRef`]).
#[derive(Deserialize)]
pub enum ToView {
    Song(Option<Song>),
    Lyrics(State),
    Playback(Playback),
}

/// Where the song is: the viewer adds the time since it received this.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Playback {
    pub position_ms: u64,
    pub paused: bool,
    pub offset_ms: i64,
    pub romanize: bool,
    pub translate: bool,
    pub provider: ProviderStatus,
}

/// Viewer to player.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ToHost {
    PlayPause,
    Previous,
    Next,
    /// Next lyrics provider, as `P` in the player
    CycleProvider,
    /// Romanization on / off, as `R` in the player
    ToggleRomanize,
    /// Translation on / off, as `T` in the player
    ToggleTranslate,
}

/// Longest line accepted from a peer; a longer one drops the connection.
pub const MAX_LINE: usize = 16 * 1024 * 1024;

/// Reads what a non-blocking stream has and splits it into complete lines.
/// The flag is false when the peer is gone or sends too much.
pub fn read_lines(stream: &mut impl Read, buf: &mut Vec<u8>) -> (Vec<Vec<u8>>, bool) {
    let mut chunk = [0u8; 16 * 1024];
    let mut alive = true;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => {
                alive = false;
                break;
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                alive = false;
                break;
            }
        }
        if buf.len() > MAX_LINE {
            alive = false;
            break;
        }
    }
    let mut lines = Vec::new();
    while let Some(end) = buf.iter().position(|&b| b == b'\n') {
        let mut line: Vec<u8> = buf.drain(..=end).collect();
        line.pop();
        if !line.is_empty() {
            lines.push(line);
        }
    }
    (lines, alive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_across_reads() {
        let mut buf = Vec::new();
        // A slice reports end of file once read: the peer is gone
        let mut data: &[u8] = b"one\ntw";
        let (lines, alive) = read_lines(&mut data, &mut buf);
        assert_eq!(lines, vec![b"one".to_vec()]);
        assert!(!alive);
        assert_eq!(buf, b"tw");
        let mut data: &[u8] = b"o\n\n";
        let (lines, _) = read_lines(&mut data, &mut buf);
        assert_eq!(lines, vec![b"two".to_vec()]);
    }

    #[test]
    fn messages_round_trip() {
        let song = Song {
            video_id: "id".into(),
            title: "t".into(),
            author: "a".into(),
            album: "al".into(),
        };
        let json = serde_json::to_string(&ToViewRef::Song(Some(&song))).unwrap();
        match serde_json::from_str::<ToView>(&json).unwrap() {
            ToView::Song(Some(read)) => assert_eq!(read, song),
            _ => panic!("wrong message"),
        }
        for action in [
            ToHost::CycleProvider,
            ToHost::ToggleRomanize,
            ToHost::ToggleTranslate,
        ] {
            let json = serde_json::to_string(&action).unwrap();
            let read = serde_json::from_str::<ToHost>(&json).unwrap();
            assert_eq!(format!("{read:?}"), format!("{action:?}"));
        }
        let playback = Playback {
            position_ms: 1,
            paused: false,
            offset_ms: 0,
            romanize: true,
            translate: false,
            provider: ProviderStatus {
                provider: lyrics::Source::Lrclib,
                switching: Some(lyrics::Source::BetterLyrics),
                missing_on: None,
            },
        };
        let json = serde_json::to_string(&ToViewRef::Playback(playback)).unwrap();
        match serde_json::from_str::<ToView>(&json).unwrap() {
            ToView::Playback(read) => assert_eq!(read, playback),
            _ => panic!("wrong message"),
        }
        let json = serde_json::to_string(&ToViewRef::Lyrics(&State::NotFound)).unwrap();
        assert!(matches!(
            serde_json::from_str::<ToView>(&json).unwrap(),
            ToView::Lyrics(State::NotFound)
        ));
    }
}
