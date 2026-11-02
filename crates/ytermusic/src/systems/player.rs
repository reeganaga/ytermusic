use std::collections::{HashMap, VecDeque};

use common_structs::MusicDownloadStatus;
use flume::{unbounded, Receiver, Sender};
use log::error;
use player::{PlayError, Player, PlayerOptions};
use rand::seq::SliceRandom;

use ytpapi2::YoutubeMusicVideoRef;

use crate::{
    config::RepeatMode,
    consts::{CACHE_DIR, CONFIG},
    errors::{handle_error, handle_error_option},
    structures::{media::Media, sound_action::SoundAction},
    systems::DOWNLOAD_MANAGER,
    term::{ManagerMessage, Screens},
    DATABASE,
};

pub struct PlayerState {
    pub list: Vec<YoutubeMusicVideoRef>,
    /// Queue order before shuffle mode was enabled, restored when it is disabled
    pub original_order: Option<Vec<YoutubeMusicVideoRef>>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Songs requested for download without being queued
    pub explicit_downloads: Vec<YoutubeMusicVideoRef>,
    pub current: usize,
    pub rtcurrent: Option<YoutubeMusicVideoRef>,
    pub music_status: HashMap<String, MusicDownloadStatus>,
    pub controls: Media,
    pub sink: Player,
    pub updater: Sender<ManagerMessage>,
    pub soundaction_sender: Sender<SoundAction>,
    pub soundaction_receiver: Receiver<SoundAction>,
    pub stream_error_receiver: Receiver<PlayError>,
    last_download_list: Vec<String>,
}

impl PlayerState {
    fn new(
        soundaction_sender: Sender<SoundAction>,
        soundaction_receiver: Receiver<SoundAction>,
        updater: Sender<ManagerMessage>,
    ) -> Self {
        let (stream_error_sender, stream_error_receiver) = unbounded::<PlayError>();
        let sink = handle_error_option(
            &updater,
            "player creation error",
            Player::new(
                stream_error_sender,
                PlayerOptions::new(CONFIG.player.initial_volume),
            ),
        )
        .unwrap();
        Self {
            controls: Media::new(updater.clone(), soundaction_sender.clone()),
            soundaction_receiver,
            music_status: HashMap::new(),
            updater,
            stream_error_receiver,
            soundaction_sender,
            sink,
            list: Vec::new(),
            original_order: None,
            shuffle: CONFIG.player.shuffle,
            repeat: CONFIG.player.repeat,
            explicit_downloads: Vec::new(),
            current: 0,
            rtcurrent: None,
            last_download_list: Vec::new(),
        }
    }

    pub fn current(&self) -> Option<&YoutubeMusicVideoRef> {
        self.relative_current(0)
    }

    pub fn relative_current(&self, n: isize) -> Option<&YoutubeMusicVideoRef> {
        self.list.get(self.current.saturating_add_signed(n))
    }

    pub fn set_relative_current(&mut self, n: isize) {
        self.current = self.current.saturating_add_signed(n);
    }

    /// Download status of any song, falling back to the local database.
    pub fn status_of(&self, video_id: &str) -> MusicDownloadStatus {
        self.music_status.get(video_id).copied().unwrap_or_else(|| {
            if DATABASE.contains(video_id) {
                MusicDownloadStatus::Downloaded
            } else {
                MusicDownloadStatus::NotDownloaded
            }
        })
    }

    /// Songs after the current one.
    pub fn upcoming(&self) -> &[YoutubeMusicVideoRef] {
        self.list.get(self.current + 1..).unwrap_or(&[])
    }

    pub fn shuffle_upcoming(&mut self) {
        if let Some(rest) = self.list.get_mut(self.current + 1..) {
            rest.shuffle(&mut rand::thread_rng());
        }
    }

    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        if self.shuffle {
            self.original_order = Some(self.list.clone());
            self.shuffle_upcoming();
        } else if let Some(original) = self.original_order.take() {
            let current_id = self.current().map(|x| x.video_id.clone());
            self.list = original;
            if let Some(id) = current_id {
                if let Some(index) = self.list.iter().position(|x| x.video_id == id) {
                    self.current = index;
                    self.rtcurrent = self.current().cloned();
                }
            }
        }
    }

    /// Forget download statuses except for explicitly requested downloads.
    pub fn keep_only_explicit_statuses(&mut self) {
        let explicit = &self.explicit_downloads;
        self.music_status
            .retain(|id, _| explicit.iter().any(|x| &x.video_id == id));
    }

    /// Forces the download list to be sent again (after the manager was cleaned).
    pub fn reset_download_list(&mut self) {
        self.last_download_list.clear();
        for video in &self.explicit_downloads {
            if let Some(status) = self.music_status.get_mut(&video.video_id) {
                if matches!(status, MusicDownloadStatus::Downloading(_)) {
                    *status = MusicDownloadStatus::Queued;
                }
            }
        }
    }

    pub fn is_current_download_failed(&self) -> bool {
        self.current()
            .as_ref()
            .map(|x| {
                self.music_status.get(&x.video_id) == Some(&MusicDownloadStatus::DownloadFailed)
            })
            .unwrap_or(false)
    }

    pub fn is_current_downloaded(&self) -> bool {
        self.current()
            .as_ref()
            .map(|x| self.music_status.get(&x.video_id) == Some(&MusicDownloadStatus::Downloaded))
            .unwrap_or(false)
    }

    pub fn update(&mut self) {
        self.update_controls();
        self.handle_stream_errors();
        if self.current > self.list.len() {
            self.current = self.list.len();
        }
        while let Ok(e) = self.soundaction_receiver.try_recv() {
            e.apply_sound_action(self);
        }
        if self.is_current_download_failed() {
            SoundAction::Next(1).apply_sound_action(self);
        }
        if self.sink.is_finished() {
            if self.is_current_downloaded() && self.rtcurrent.as_ref() == self.current() {
                match self.repeat {
                    // Replay the same song
                    RepeatMode::One => {}
                    RepeatMode::All if self.current + 1 >= self.list.len() => self.current = 0,
                    _ => self.set_relative_current(1),
                }
            }
            self.handle_stream_errors();
            self.update_controls();
            // If the current song is finished, we play the next one but if the next one has failed to download, we skip it
            // TODO(optimize this)
            while self
                .current()
                .map(|x| {
                    self.music_status.get(&x.video_id) == Some(&MusicDownloadStatus::DownloadFailed)
                })
                .unwrap_or(false)
            {
                self.set_relative_current(1);
            }

            if self.is_current_downloaded() {
                if let Some(video) = self.current().cloned() {
                    let k = CACHE_DIR.join(format!("downloads/{}.mp4", video.video_id));
                    if let Err(e) = self.sink.play(k.as_path()) {
                        if matches!(e, PlayError::DecoderError(_)) {
                            // Cleaning the file

                            DATABASE.remove_video(&video);
                            handle_error(
                                &self.updater,
                                "invalid cleaning MP4",
                                std::fs::remove_file(k),
                            );
                            handle_error(
                                &self.updater,
                                "invalid cleaning JSON",
                                std::fs::remove_file(
                                    CACHE_DIR.join(format!("downloads/{}.json", video.video_id)),
                                ),
                            );
                            self.current = 0;
                            DATABASE.write();
                        } else {
                            self.updater
                                .send(ManagerMessage::PassTo(
                                    Screens::DeviceLost,
                                    Box::new(ManagerMessage::Error(
                                        format!("{e:?}"),
                                        Box::new(None),
                                    )),
                                ))
                                .unwrap();
                        }
                    }
                }
            }
        } else {
            self.rtcurrent = self.current().cloned();
        }
        let music_status = &self.music_status;
        self.explicit_downloads.retain(|x| {
            !matches!(
                music_status.get(&x.video_id),
                Some(MusicDownloadStatus::Downloaded | MusicDownloadStatus::DownloadFailed)
            )
        });
        let to_download = self
            .list
            .iter()
            .skip(self.current)
            .chain(self.list.iter().take(self.current).rev())
            .filter(|x| {
                self.music_status.get(&x.video_id) == Some(&MusicDownloadStatus::NotDownloaded)
            })
            .take(12)
            .chain(self.explicit_downloads.iter().filter(|x| {
                matches!(
                    self.music_status.get(&x.video_id),
                    Some(MusicDownloadStatus::Queued | MusicDownloadStatus::NotDownloaded)
                )
            }))
            .cloned()
            .collect::<VecDeque<_>>();
        let new_ids: Vec<String> = to_download.iter().map(|v| v.video_id.clone()).collect();
        if new_ids != self.last_download_list {
            self.last_download_list = new_ids;
            DOWNLOAD_MANAGER.set_download_list(to_download);
        }
    }

    fn handle_stream_errors(&self) {
        while let Ok(e) = self.stream_error_receiver.try_recv() {
            error!("Stream error: {:?}", e);
            handle_error(&self.updater, "audio device stream error", Err(e));
        }
    }
    fn update_controls(&mut self) {
        let current = self.current().cloned();
        let result = self
            .controls
            .update(current, &self.sink)
            .map_err(|x| format!("{x:?}"));
        handle_error::<String>(&self.updater, "Can't update finished media control", result);
    }
}

pub fn player_system(updater: Sender<ManagerMessage>) -> (Sender<SoundAction>, PlayerState) {
    let (tx, rx) = flume::unbounded::<SoundAction>();
    (tx.clone(), PlayerState::new(tx, rx, updater))
}
