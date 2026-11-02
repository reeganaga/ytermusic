use log::info;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

use crate::utils::get_project_dirs;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloaderConfig {
    #[default]
    Ytdlp,
    RustyYtdl,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct GlobalConfig {
    /// Maximum number of parallel downloads.
    /// If your downloads are failing, try lowering
    /// this.
    /// Default value is 4.
    #[serde(default = "parallel_downloads")]
    pub parallel_downloads: u16,
    #[serde(default)]
    pub downloader: DownloaderConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[non_exhaustive]
pub struct MusicPlayerConfig {
    /// Initial volume of the player, in percent.
    /// Default value is 50, clamped at 100.
    #[serde(default = "default_volume")]
    pub initial_volume: u8,
    #[serde(default = "default_true")]
    pub dbus: bool,
    #[serde(default = "default_true")]
    pub hide_channels_on_homepage: bool,
    #[serde(default = "default_false")]
    pub hide_albums_on_homepage: bool,
    #[serde(default = "enable_volume_slider")]
    pub volume_slider: bool,
    /// Whether shuffle mode is on at startup
    #[serde(default)]
    pub shuffle: bool,
    /// Repeat mode at startup: "off", "all" or "one"
    #[serde(default)]
    pub repeat: RepeatMode,
    /// What the player bar shows as album art: "placeholder" or "thumbnail".
    /// "thumbnail" needs a build with the `thumbnails` feature and a terminal
    /// supporting kitty, sixel or iTerm2 images; otherwise the placeholder is shown.
    #[serde(default)]
    pub album_art: AlbumArt,
    #[serde(default = "default_paused_style", with = "StyleDef")]
    pub gauge_paused_style: Style,
    #[serde(default = "default_playing_style", with = "StyleDef")]
    pub gauge_playing_style: Style,
    #[serde(default = "default_nomusic_style", with = "StyleDef")]
    pub gauge_nomusic_style: Style,
    #[serde(default = "default_paused_style", with = "StyleDef")]
    pub text_paused_style: Style,
    #[serde(default = "default_playing_style", with = "StyleDef")]
    pub text_playing_style: Style,
    #[serde(default = "default_nomusic_style", with = "StyleDef")]
    pub text_next_style: Style,
    #[serde(default = "default_nomusic_style", with = "StyleDef")]
    pub text_waiting_style: Style,
    #[serde(default = "default_downloading_style", with = "StyleDef")]
    pub text_downloading_style: Style,
    #[serde(default = "default_error_style", with = "StyleDef")]
    pub text_error_style: Style,
    #[serde(default = "default_searching_style", with = "StyleDef")]
    pub text_searching_style: Style,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(remote = "Style")]
struct StyleDef {
    #[serde(default)]
    fg: Option<Color>,
    #[serde(default)]
    bg: Option<Color>,
    #[serde(default = "Modifier::empty")]
    add_modifier: Modifier,
    #[serde(default = "Modifier::empty")]
    sub_modifier: Modifier,
    #[serde(default)]
    underline_color: Option<Color>,
}

impl Default for MusicPlayerConfig {
    fn default() -> Self {
        Self {
            hide_albums_on_homepage: default_false(),
            hide_channels_on_homepage: default_true(),
            dbus: default_true(),
            initial_volume: default_volume(),
            shuffle: Default::default(),
            repeat: Default::default(),
            album_art: Default::default(),
            gauge_paused_style: default_paused_style(),
            gauge_playing_style: default_playing_style(),
            gauge_nomusic_style: default_nomusic_style(),
            text_paused_style: default_paused_style(),
            text_playing_style: default_playing_style(),
            text_next_style: default_nomusic_style(),
            text_waiting_style: default_nomusic_style(),
            text_error_style: default_error_style(),
            text_searching_style: default_searching_style(),
            text_downloading_style: default_downloading_style(),
            volume_slider: enable_volume_slider(),
        }
    }
}

fn default_searching_style() -> Style {
    Style::default().fg(Color::LightCyan)
}

fn default_error_style() -> Style {
    Style::default().fg(Color::Red)
}

fn parallel_downloads() -> u16 {
    4
}

fn default_false() -> bool {
    false
}

fn default_true() -> bool {
    true
}

fn enable_volume_slider() -> bool {
    true
}

fn default_paused_style() -> Style {
    Style::default().fg(Color::Yellow)
}

fn default_playing_style() -> Style {
    Style::default().fg(Color::Green)
}

fn default_nomusic_style() -> Style {
    Style::default().fg(Color::White)
}

fn default_downloading_style() -> Style {
    Style::default().fg(Color::Blue)
}

fn default_volume() -> u8 {
    50
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlbumArt {
    #[default]
    Placeholder,
    Thumbnail,
}

/// Colors of the interface. Accepts `#rrggbb`, color names or `reset`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ThemeConfig {
    #[serde(default = "theme_bg")]
    pub bg: Color,
    #[serde(default = "theme_border")]
    pub border: Color,
    #[serde(default = "theme_accent")]
    pub accent: Color,
    /// Text drawn on top of the accent color (active tab)
    #[serde(default = "theme_on_accent")]
    pub on_accent: Color,
    #[serde(default = "theme_text")]
    pub text: Color,
    #[serde(default = "theme_text_dim")]
    pub text_dim: Color,
    /// Background of selected rows, active nav items and cards
    #[serde(default = "theme_highlight")]
    pub highlight: Color,
    #[serde(default = "theme_error")]
    pub error: Color,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            bg: theme_bg(),
            border: theme_border(),
            accent: theme_accent(),
            on_accent: theme_on_accent(),
            text: theme_text(),
            text_dim: theme_text_dim(),
            highlight: theme_highlight(),
            error: theme_error(),
        }
    }
}

fn theme_bg() -> Color {
    Color::Rgb(0x0b, 0x0d, 0x0f)
}
fn theme_border() -> Color {
    Color::Rgb(0x30, 0x36, 0x3d)
}
fn theme_accent() -> Color {
    Color::Rgb(0x1e, 0xd7, 0x60)
}
fn theme_on_accent() -> Color {
    Color::Rgb(0x06, 0x26, 0x0f)
}
fn theme_text() -> Color {
    Color::Rgb(0xe6, 0xed, 0xf3)
}
fn theme_text_dim() -> Color {
    Color::Rgb(0x7d, 0x85, 0x90)
}
fn theme_highlight() -> Color {
    Color::Rgb(0x16, 0x1b, 0x1f)
}
fn theme_error() -> Color {
    Color::Rgb(0xf8, 0x51, 0x49)
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PlaylistConfig {}

#[derive(Debug, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SearchConfig {}

#[allow(unused)]
#[derive(Debug, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct Config {
    #[serde(default)]
    pub global: GlobalConfig,
    #[serde(default)]
    pub player: MusicPlayerConfig,
    #[serde(default)]
    pub playlist: PlaylistConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub theme: ThemeConfig,
}

impl Config {
    pub fn new() -> Self {
        // TODO handle errors
        let opt = || {
            let project_dirs = get_project_dirs()?;
            let config_path = project_dirs.config_dir().join("config.toml");
            config_path
                .parent()
                .map(|p| std::fs::create_dir_all(p).ok());
            info!("Loading config from {:?}", config_path);
            if !config_path.exists() {
                let default_config = Self::default();
                std::fs::write(
                    project_dirs.config_dir().join("config.toml"),
                    toml::to_string_pretty(&default_config).ok()?,
                )
                .ok()?;
                return Some(default_config);
            }
            let config_string = std::fs::read_to_string(config_path).ok()?;
            let config = toml::from_str::<Self>(&config_string).ok()?;
            std::fs::write(
                project_dirs.config_dir().join("config.applied.toml"),
                toml::to_string_pretty(&config).ok()?,
            )
            .ok()?;
            Some(config)
        };
        opt().unwrap_or_default()
    }
}
