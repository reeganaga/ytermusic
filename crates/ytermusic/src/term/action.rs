/// Something the user asked for, independent of the key that triggered it.
///
/// Keys are turned into actions by the [`Keymap`](super::keymap::Keymap), so screens
/// never have to look at raw key events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Leave the current screen
    Back,
    OpenSearch,
    Up,
    Down,
    /// Activate the selected item
    Select,
    /// Activate the selected item but stay on the current screen
    SelectInBackground,
    PlayPause,
    VolumeUp,
    VolumeDown,
    SeekForward,
    SeekBackward,
    NextSong,
    PreviousSong,
    Shuffle,
    RemoveSong,
    Cleanup,
    /// Download again every music that failed to download
    RetryDownloads,
}
