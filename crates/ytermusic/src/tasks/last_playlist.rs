use flume::Sender;
use log::info;
use ytpapi2::YoutubeMusicVideoRef;

use crate::{
    consts::CACHE_DIR,
    run_service,
    structures::performance,
    term::{
        library::{PlaylistEntry, Source},
        ManagerMessage,
    },
};

pub fn spawn_last_playlist_task(updater_s: Sender<ManagerMessage>) {
    run_service(async move {
        let guard = performance::guard("Last playlist");
        info!("Last playlist task on");
        let playlist = std::fs::read_to_string(CACHE_DIR.join("last-playlist.json")).ok()?;
        let (name, videos): (String, Vec<YoutubeMusicVideoRef>) =
            serde_json::from_str(&playlist).ok()?;
        let name = name
            .strip_prefix("Last playlist: ")
            .unwrap_or(&name)
            .to_owned();
        updater_s
            .send(ManagerMessage::AddPlaylist(PlaylistEntry::new(
                format!("Last played: {name}"),
                Source::Last,
                videos,
            )))
            .unwrap();
        drop(guard);
        Some(())
    });
}
