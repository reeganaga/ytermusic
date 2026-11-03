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

/// Lyrics panel (`y` on the player screen).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct LyricsConfig {
    /// Show the romanization (romaji, pinyin...) under non-latin lyrics.
    /// Toggled with `R`, which saves the new value here.
    #[serde(default = "default_true")]
    pub romanize: bool,
    /// Show the translation under each line. Toggled with `T`, which saves
    /// the new value here.
    #[serde(default = "default_true")]
    pub translate: bool,
    /// Language code of the translation: "en", "id", "fr", "ja"...
    #[serde(default = "default_translate_to")]
    pub translate_to: String,
    /// Slide to the next line and fade the highlight instead of jumping.
    /// Redraws at ~30 fps while animating; turn off on slow terminals / SSH.
    #[serde(default = "default_true")]
    pub animate: bool,
    /// Fill the active line word by word when the lyrics have word timings
    /// (Better Lyrics). Redraws when each word starts.
    #[serde(default = "default_true")]
    pub word_sync: bool,
    /// Where lyrics come from first: "betterlyrics" (word-synced, default),
    /// "betterlyrics-qq" (QQ Music, good for Asian songs) or "lrclib". LRCLIB
    /// then YouTube Music are tried when it has nothing. Cycled with `P`,
    /// which saves the new value here.
    #[serde(default)]
    pub provider: lyrics::Provider,
    /// Better Lyrics API key, sent as `X-API-Key`. Without one only songs
    /// already in Better Lyrics' cache are found. Keys are not handed out to
    /// new users at the moment.
    #[serde(default)]
    pub betterlyrics_api_key: String,
}

impl Default for LyricsConfig {
    fn default() -> Self {
        Self {
            romanize: true,
            translate: true,
            translate_to: default_translate_to(),
            animate: true,
            word_sync: true,
            provider: lyrics::Provider::default(),
            betterlyrics_api_key: String::new(),
        }
    }
}

fn default_translate_to() -> String {
    "en".to_owned()
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
    #[serde(default)]
    pub lyrics: LyricsConfig,
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

/// Saves the lyrics toggles in `config.toml`, keeping the rest of the file
/// (comments, formatting) untouched.
pub fn save_lyrics_toggles(romanize: bool, translate: bool) -> Result<(), String> {
    save_lyrics_values(&[
        ("romanize", toml_edit::value(romanize)),
        ("translate", toml_edit::value(translate)),
    ])
}

/// Saves the lyrics provider in `config.toml` (see [`save_lyrics_toggles`]).
pub fn save_lyrics_provider(provider: lyrics::Provider) -> Result<(), String> {
    save_lyrics_values(&[("provider", toml_edit::value(provider_name(provider)))])
}

fn provider_name(provider: lyrics::Provider) -> String {
    toml::Value::try_from(provider)
        .ok()
        .and_then(|x| x.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn save_lyrics_values(values: &[(&str, toml_edit::Item)]) -> Result<(), String> {
    let dirs = get_project_dirs().ok_or("no config directory")?;
    let path = dirs.config_dir().join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let text = with_lyrics_values(&text, values)?;
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

fn with_lyrics_values(config: &str, values: &[(&str, toml_edit::Item)]) -> Result<String, String> {
    let mut doc = config
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| e.to_string())?;
    let lyrics = doc
        .entry("lyrics")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .ok_or("[lyrics] is not a table")?;
    for (key, value) in values {
        lyrics.insert(key, value.clone());
    }
    Ok(doc.to_string())
}

#[cfg(test)]
fn with_lyrics_toggles(config: &str, romanize: bool, translate: bool) -> Result<String, String> {
    with_lyrics_values(
        config,
        &[
            ("romanize", toml_edit::value(romanize)),
            ("translate", toml_edit::value(translate)),
        ],
    )
}

#[test]
fn lyrics_provider_is_saved() {
    let value = provider_name(lyrics::Provider::BetterLyricsQq);
    let saved = with_lyrics_values("", &[("provider", toml_edit::value(value))]).unwrap();
    let parsed: Config = toml::from_str(&saved).unwrap();
    assert_eq!(parsed.lyrics.provider, lyrics::Provider::BetterLyricsQq);
    let parsed: Config = toml::from_str("").unwrap();
    assert_eq!(parsed.lyrics.provider, lyrics::Provider::BetterLyrics);
}

#[test]
fn lyrics_toggles_keep_the_rest_of_the_config() {
    let config = "# my config\n[player]\ninitial_volume = 30 # quiet\n";
    let saved = with_lyrics_toggles(config, false, true).unwrap();
    assert!(saved.starts_with(config));
    let parsed: Config = toml::from_str(&saved).unwrap();
    assert!(!parsed.lyrics.romanize);
    assert!(parsed.lyrics.translate);
    assert_eq!(parsed.lyrics.translate_to, "en");
    assert_eq!(parsed.player.initial_volume, 30);

    let config = "[lyrics]\ntranslate_to = \"id\" # indonesian\nromanize = true\n";
    let saved = with_lyrics_toggles(config, false, false).unwrap();
    assert!(saved.contains("translate_to = \"id\" # indonesian"));
    let parsed: Config = toml::from_str(&saved).unwrap();
    assert!(!parsed.lyrics.romanize && !parsed.lyrics.translate);
}
