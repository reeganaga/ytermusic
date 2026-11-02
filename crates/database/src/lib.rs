use std::{fs::OpenOptions, path::PathBuf, sync::RwLock};

mod reader;
mod writer;

pub use writer::write_video;
use ytpapi2::YoutubeMusicVideoRef;

pub struct YTLocalDatabase {
    cache_dir: PathBuf,
    references: RwLock<Vec<YoutubeMusicVideoRef>>,
}

impl YTLocalDatabase {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            references: RwLock::new(Vec::new()),
        }
    }

    pub fn clone_from(&self, videos: &Vec<YoutubeMusicVideoRef>) {
        self.references.write().unwrap().clone_from(videos);
    }

    /// Whether the video is downloaded, using the in-memory index (no disk access).
    pub fn contains(&self, video_id: &str) -> bool {
        self.references
            .read()
            .unwrap()
            .iter()
            .any(|x| x.video_id == video_id)
    }

    /// In-memory copy of every downloaded video.
    pub fn all(&self) -> Vec<YoutubeMusicVideoRef> {
        self.references.read().unwrap().clone()
    }

    pub fn remove_video(&self, video: &YoutubeMusicVideoRef) {
        let mut database = self.references.write().unwrap();
        database.retain(|v| v.video_id != video.video_id);
        drop(database);
        self.write();
    }

    pub fn append(&self, video: YoutubeMusicVideoRef) {
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.cache_dir.join("db.bin"))
            .unwrap();
        write_video(&mut file, &video);
        self.references.write().unwrap().push(video);
    }
}
