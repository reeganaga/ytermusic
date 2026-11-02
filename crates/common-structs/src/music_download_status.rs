#[derive(PartialEq, Debug, Clone, Copy)]
pub enum MusicDownloadStatus {
    NotDownloaded,
    /// Explicitly requested for download, waiting for a free worker.
    Queued,
    Downloaded,
    Downloading(usize),
    DownloadFailed,
}

impl MusicDownloadStatus {
    pub fn character(&self, playing: Option<bool>) -> String {
        match self {
            Self::NotDownloaded => {
                if let Some(e) = playing {
                    if e { '▶' } else { '⏸' }
                } else {
                    ' '
                }
            }
            Self::Downloaded => ' ',
            Self::Queued => return "queued".to_owned(),
            Self::Downloading(progress) => return format!("⭳ [{:02}%]", progress),
            Self::DownloadFailed => '⚠',
        }
        .into()
    }
}
