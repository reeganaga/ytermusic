//! Player side of `--lyrics-view`: the socket, the connected viewers and what
//! is sent to them. Polled from the manager loop, nothing runs in between.

use std::{
    io::Write,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    time::{Duration, Instant},
};

use log::{error, info, warn};

use crate::{
    structures::sound_action::SoundAction,
    systems::player::PlayerState,
    term::lyrics::{LyricsView, State},
};

use super::{read_lines, socket_path, Playback, Song, ToHost, ToViewRef};

/// The position is resent this often even when nothing seeked, so a viewer
/// never drifts far.
const RESYNC: Duration = Duration::from_secs(2);
/// Position error that counts as a seek and is sent right away
const SEEK_THRESHOLD_MS: i64 = 400;
/// A viewer that does not read gets dropped past this much unsent data
const MAX_PENDING: usize = 32 * 1024 * 1024;

struct Client {
    stream: UnixStream,
    /// Bytes read, waiting for a newline
    input: Vec<u8>,
    /// Bytes not yet accepted by the socket
    output: Vec<u8>,
}

/// What the viewers were last told, to send only the changes.
#[derive(Default)]
struct Sent {
    song: Option<Song>,
    lyrics_for: Option<(Option<String>, u64)>,
    playback: Option<(Playback, Instant)>,
}

pub struct PipServer {
    listener: UnixListener,
    path: PathBuf,
    clients: Vec<Client>,
    sent: Sent,
}

impl PipServer {
    /// Starts listening. `None` when another player already does, or when the
    /// socket can't be made (logged): the player works the same without it.
    pub fn start() -> Option<Self> {
        let path = socket_path();
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                info!(
                    "--lyrics-view socket {} is used by another player",
                    path.display()
                );
                return None;
            }
            // Left behind by a player that did not exit cleanly
            let _ = std::fs::remove_file(&path);
        }
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(e) => {
                warn!("Can't listen on {}: {e}", path.display());
                return None;
            }
        };
        if let Err(e) = listener.set_nonblocking(true) {
            warn!("Can't make {} non-blocking: {e}", path.display());
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        info!("--lyrics-view socket on {}", path.display());
        Some(Self {
            listener,
            path,
            clients: Vec::new(),
            sent: Sent::default(),
        })
    }

    /// Accepts viewers, applies their buttons and sends what changed.
    /// Returns a notice for the player to show (provider or toggle changed).
    pub fn tick(&mut self, player: &mut PlayerState, lyrics: &mut LyricsView) -> Option<String> {
        let mut accepted = false;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(true).is_ok() {
                        self.clients.push(Client {
                            stream,
                            input: Vec::new(),
                            output: Vec::new(),
                        });
                        accepted = true;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    error!("--lyrics-view accept: {e}");
                    break;
                }
            }
        }
        if self.clients.is_empty() {
            // Nothing to keep up to date: the next viewer gets everything
            self.sent = Sent::default();
            return None;
        }

        let mut actions = Vec::new();
        self.clients.retain_mut(|client| {
            let (lines, alive) = read_lines(&mut client.stream, &mut client.input);
            for line in lines {
                match serde_json::from_slice::<ToHost>(&line) {
                    Ok(action) => actions.push(action),
                    Err(e) => warn!("--lyrics-view sent a bad message: {e}"),
                }
            }
            alive
        });
        let mut notice = None;
        for action in actions {
            match action {
                ToHost::PlayPause => SoundAction::PlayPause.apply_sound_action(player),
                ToHost::Previous => SoundAction::Previous(1).apply_sound_action(player),
                ToHost::Next => SoundAction::Next(1).apply_sound_action(player),
                ToHost::CycleProvider => notice = Some(lyrics.next_provider(player)),
                ToHost::ToggleRomanize => notice = Some(lyrics.toggle_option(true)),
                ToHost::ToggleTranslate => notice = Some(lyrics.toggle_option(false)),
            }
        }

        // Lyrics are only fetched while the panel is open, or here for the viewer
        lyrics.sync(player);
        if accepted {
            self.sent = Sent::default();
        }
        let messages = self.changes(player, lyrics);
        for message in messages {
            for client in &mut self.clients {
                client.output.extend_from_slice(message.as_bytes());
                client.output.push(b'\n');
            }
        }
        self.clients.retain_mut(flush);
        notice
    }

    /// Messages for what changed since the last call.
    fn changes(&mut self, player: &PlayerState, lyrics: &LyricsView) -> Vec<String> {
        let mut messages = Vec::new();
        let song = player.current().map(|video| Song {
            video_id: video.video_id.clone(),
            title: video.title.clone(),
            author: video.author.clone(),
            album: video.album.clone(),
        });
        if song != self.sent.song {
            messages.push(encode(&ToViewRef::Song(song.as_ref())));
        }
        let lyrics_for = (song.as_ref().map(|x| x.video_id.clone()), lyrics.revision());
        if Some(&lyrics_for) != self.sent.lyrics_for.as_ref() {
            // The state of another song is not worth sending: the viewer
            // would show it under the new title
            let state = if lyrics_for.0.is_some() {
                lyrics.state()
            } else {
                &State::Idle
            };
            messages.push(encode(&ToViewRef::Lyrics(state)));
        }
        let (offset_ms, romanize, translate) = lyrics.options();
        let playback = Playback {
            position_ms: player.sink.elapsed().as_millis() as u64,
            paused: player.sink.is_paused() || player.sink.is_finished(),
            offset_ms,
            romanize,
            translate,
            provider: lyrics.provider_status(),
        };
        let send_playback = match &self.sent.playback {
            None => true,
            Some((last, at)) => {
                let expected = last.position_ms as i64
                    + if last.paused {
                        0
                    } else {
                        at.elapsed().as_millis() as i64
                    };
                last.paused != playback.paused
                    || last.offset_ms != playback.offset_ms
                    || last.romanize != playback.romanize
                    || last.translate != playback.translate
                    || last.provider != playback.provider
                    || (playback.position_ms as i64 - expected).abs() > SEEK_THRESHOLD_MS
                    || at.elapsed() > RESYNC
            }
        };
        if send_playback {
            messages.push(encode(&ToViewRef::Playback(playback)));
            self.sent.playback = Some((playback, Instant::now()));
        }
        self.sent.song = song;
        self.sent.lyrics_for = Some(lyrics_for);
        messages
    }
}

impl Drop for PipServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn encode(message: &ToViewRef) -> String {
    serde_json::to_string(message).unwrap_or_default()
}

/// Writes what the socket takes. False when the viewer is gone.
fn flush(client: &mut Client) -> bool {
    while !client.output.is_empty() {
        match client.stream.write(&client.output) {
            Ok(0) => return false,
            Ok(n) => {
                client.output.drain(..n);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    client.output.len() < MAX_PENDING
}
