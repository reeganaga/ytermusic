use common_structs::MusicDownloadStatus;
use download_manager::{DownloadManagerMessage, MessageHandler};
use flume::Sender;
use log::{error, trace};
use std::{fs, sync::Arc, time::Duration};
use ytpapi2::YoutubeMusicVideoRef;

use crate::{
    consts::CACHE_DIR,
    errors::handle_error_option,
    systems::{player::PlayerState, DOWNLOAD_MANAGER},
    ShutdownSignal, DATABASE,
};

/// Actions that can be sent to the player from other services
#[derive(Debug, Clone)]
pub enum SoundAction {
    /// Set the volume of the player to the given value
    SetVolume(f32),
    Cleanup,
    PlayPause,
    RestartPlayer,
    Plus,
    Minus,
    /// Seek to a specific time in the current song in seconds
    SeekTo(Duration),
    Previous(usize),
    Forward,
    Backward,
    Next(usize),
    AddVideosToQueue(Vec<YoutubeMusicVideoRef>),
    AddVideoUnary(YoutubeMusicVideoRef),
    /// Remove the song at this queue index (and its downloaded file)
    RemoveIndex(usize),
    /// Replace the whole queue and start playing at the given index
    PlayList(Vec<YoutubeMusicVideoRef>, usize),
    /// Jump to the song at this queue index (toggles pause if already current)
    PlayIndex(usize),
    ToggleShuffle,
    CycleRepeat,
    /// Download songs without adding them to the queue
    Download(Vec<YoutubeMusicVideoRef>),
    VideoStatusUpdate(String, MusicDownloadStatus),
}

fn downloaded_status(video_id: &str) -> MusicDownloadStatus {
    if DATABASE.contains(video_id) {
        MusicDownloadStatus::Downloaded
    } else {
        MusicDownloadStatus::NotDownloaded
    }
}

impl SoundAction {
    fn insert(player: &mut PlayerState, video: String, status: MusicDownloadStatus) {
        if matches!(
            player.music_status.get(&video),
            Some(&MusicDownloadStatus::DownloadFailed)
        ) {
            DOWNLOAD_MANAGER.remove_from_in_downloads(&video);
        }
        if matches!(
            player.music_status.get(&video),
            Some(&MusicDownloadStatus::Downloading(_) | &MusicDownloadStatus::Downloaded)
        ) && status == MusicDownloadStatus::NotDownloaded
        {
            return;
        }
        player.music_status.insert(video, status);
    }

    pub fn apply_sound_action(self, player: &mut PlayerState) {
        match self {
            Self::SetVolume(volume) => player.sink.set_volume((volume * 100.) as i32),
            Self::SeekTo(time) => player.sink.seek_to(time),
            Self::Backward => player.sink.seek_bw(),
            Self::Forward => player.sink.seek_fw(),
            Self::PlayPause => player.sink.toggle_playback(),
            Self::Cleanup => {
                player.list.clear();
                player.original_order = None;
                player.current = 0;
                player.keep_only_explicit_statuses();
                player.sink.stop();
            }
            Self::Plus => player.sink.volume_up(),
            Self::Minus => player.sink.volume_down(),
            Self::Next(a) => {
                player.sink.stop();

                player.set_relative_current(a as _);
            }
            Self::VideoStatusUpdate(video, status) => {
                player.music_status.insert(video, status);
            }
            Self::AddVideosToQueue(video) => {
                for v in video {
                    Self::insert(player, v.video_id.clone(), downloaded_status(&v.video_id));
                    if let Some(original) = player.original_order.as_mut() {
                        original.push(v.clone());
                    }
                    player.list.push(v)
                }
            }
            Self::PlayList(videos, start) => {
                Self::Cleanup.apply_sound_action(player);
                DOWNLOAD_MANAGER.clean(
                    ShutdownSignal,
                    download_manager_handler(player.soundaction_sender.clone()),
                );
                player.reset_download_list();
                Self::AddVideosToQueue(videos).apply_sound_action(player);
                player.current = start.min(player.list.len().saturating_sub(1));
                if player.shuffle {
                    player.original_order = Some(player.list.clone());
                    player.shuffle_upcoming();
                }
            }
            Self::PlayIndex(index) => match index.cmp(&player.current) {
                std::cmp::Ordering::Less => {
                    Self::Previous(player.current - index).apply_sound_action(player)
                }
                std::cmp::Ordering::Equal => Self::PlayPause.apply_sound_action(player),
                std::cmp::Ordering::Greater => {
                    Self::Next(index - player.current).apply_sound_action(player)
                }
            },
            Self::ToggleShuffle => player.toggle_shuffle(),
            Self::CycleRepeat => player.repeat = player.repeat.next(),
            Self::Download(videos) => {
                for video in videos {
                    let status = player
                        .music_status
                        .get(&video.video_id)
                        .copied()
                        .unwrap_or_else(|| downloaded_status(&video.video_id));
                    if matches!(
                        status,
                        MusicDownloadStatus::Downloaded | MusicDownloadStatus::Downloading(_)
                    ) || player
                        .explicit_downloads
                        .iter()
                        .any(|x| x.video_id == video.video_id)
                    {
                        continue;
                    }
                    if status == MusicDownloadStatus::DownloadFailed {
                        DOWNLOAD_MANAGER.remove_from_in_downloads(&video.video_id);
                    }
                    player
                        .music_status
                        .insert(video.video_id.clone(), MusicDownloadStatus::Queued);
                    player.explicit_downloads.push(video);
                }
            }
            Self::Previous(a) => {
                player.set_relative_current(-(a as isize));
                player.sink.stop();
            }
            Self::RestartPlayer => {
                player.sink =
                    handle_error_option(&player.updater, "update player", player.sink.update())
                        .unwrap();
                if let Some(e) = player.current().cloned() {
                    Self::AddVideoUnary(e).apply_sound_action(player);
                }
            }
            Self::AddVideoUnary(video) => {
                Self::insert(
                    player,
                    video.video_id.clone(),
                    downloaded_status(&video.video_id),
                );
                if let Some(original) = player.original_order.as_mut() {
                    original.push(video.clone());
                }
                if player.list.is_empty() {
                    player.list.push(video);
                } else {
                    player.list.insert(player.current + 1, video);
                }
            }
            Self::RemoveIndex(index) => {
                let Some(video) = player.list.get(index).cloned() else {
                    return;
                };
                if matches!(
                    player.music_status.get(&video.video_id),
                    Some(
                        &MusicDownloadStatus::DownloadFailed
                            | &MusicDownloadStatus::Downloading(_)
                            | &MusicDownloadStatus::NotDownloaded
                    )
                ) {
                    DOWNLOAD_MANAGER.remove_from_in_downloads(&video.video_id);
                }
                player.music_status.remove(&video.video_id);

                player.list.remove(index);
                if let Some(original) = player.original_order.as_mut() {
                    original.retain(|vid| vid.video_id != video.video_id);
                }
                match index.cmp(&player.current) {
                    std::cmp::Ordering::Less => player.set_relative_current(-1),
                    std::cmp::Ordering::Equal => Self::Next(0).apply_sound_action(player),
                    std::cmp::Ordering::Greater => {}
                }

                // manage deleting physically
                DATABASE.remove_video(&video);

                let cache_folder = CACHE_DIR.join("downloads");
                let json_path = cache_folder.join(format!("{}.json", video.video_id));
                match fs::remove_file(json_path) {
                    Ok(_) => trace!("Deleted JSON file"),
                    Err(e) => error!("Error deleting JSON video file: {}", e),
                }

                let mp4_path = cache_folder.join(format!("{}.mp4", video.video_id));
                match fs::remove_file(mp4_path) {
                    Ok(_) => trace!("Deleted MP4 file"),
                    Err(e) => error!("Error deleting MP4 video file: {}", e),
                }
            }
        }
    }
}

pub fn download_manager_handler(sender: Sender<SoundAction>) -> MessageHandler {
    Arc::new(move |message| match message {
        DownloadManagerMessage::VideoStatusUpdate(video, status) => {
            sender
                .send(SoundAction::VideoStatusUpdate(video, status))
                .unwrap();
        }
    })
}
