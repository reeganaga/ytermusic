//! Album art of the playing song, drawn with a terminal graphics protocol
//! (kitty, sixel or iTerm2). Without such a protocol the placeholder stays.

use flume::{Receiver, Sender};
use image::DynamicImage;
use log::{error, info};
use ratatui::{layout::Rect, widgets::Clear, Frame};
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
    Resize, StatefulImage,
};

use crate::{config::AlbumArt, consts::CONFIG, run_service, systems::player::PlayerState};

use super::chrome::FrameLayout;

/// Side of the square image kept in memory, in pixels.
const IMAGE_SIZE: u32 = 120;
/// Same for `--lyrics-view`, where the cover is much bigger (YouTube's
/// thumbnail is 180 pixels square at most, so it only grows up to that).
const LARGE_IMAGE_SIZE: u32 = 256;

pub struct Thumbnail {
    /// Side of the image kept, and whether it is scaled up to the area
    image_size: u32,
    scale_up: bool,
    picker: Option<Picker>,
    video_id: Option<String>,
    protocol: Option<StatefulProtocol>,
    sender: Sender<(String, DynamicImage)>,
    receiver: Receiver<(String, DynamicImage)>,
}

impl Thumbnail {
    pub fn new() -> Self {
        Self::sized(IMAGE_SIZE, false)
    }

    /// A big cover filling its whole area (`--lyrics-view`).
    #[cfg(unix)]
    pub fn large() -> Self {
        Self::sized(LARGE_IMAGE_SIZE, true)
    }

    fn sized(image_size: u32, scale_up: bool) -> Self {
        let (sender, receiver) = flume::unbounded();
        Self {
            image_size,
            scale_up,
            picker: None,
            video_id: None,
            protocol: None,
            sender,
            receiver,
        }
    }

    /// Detects the graphics protocol. Must run after entering the alternate
    /// screen and before reading terminal events.
    pub fn init(&mut self) {
        if CONFIG.player.album_art != AlbumArt::Thumbnail {
            return;
        }
        match Picker::from_query_stdio() {
            Ok(picker) if picker.protocol_type() != ProtocolType::Halfblocks => {
                info!("Thumbnails using {:?}", picker.protocol_type());
                self.picker = Some(picker);
            }
            Ok(_) => info!("No terminal graphics protocol, thumbnails disabled"),
            Err(e) => error!("Terminal graphics query failed: {e:?}"),
        }
    }

    /// Whether album art images are shown (wider art box).
    pub fn active(&self) -> bool {
        self.picker.is_some()
    }

    /// Size of a terminal cell in pixels, to draw a square cover.
    #[cfg(unix)]
    pub fn font_size(&self) -> Option<(u16, u16)> {
        let size = self.picker.as_ref()?.font_size();
        Some((size.width, size.height))
    }

    fn fetch(&self, video_id: String) {
        let sender = self.sender.clone();
        let image_size = self.image_size;
        run_service(async move {
            let url = format!("https://i.ytimg.com/vi/{video_id}/mqdefault.jpg");
            let bytes = match reqwest::get(&url).await {
                Ok(response) => response.bytes().await,
                Err(e) => Err(e),
            };
            let bytes = match bytes {
                Ok(bytes) => bytes,
                Err(e) => {
                    error!("thumbnail download {e:?}");
                    return;
                }
            };
            let image = match image::load_from_memory(&bytes) {
                Ok(image) => image,
                Err(e) => {
                    error!("thumbnail decode {e:?}");
                    return;
                }
            };
            // YouTube thumbnails are 16:9 with the square cover in the middle
            let side = image.width().min(image.height());
            let x = (image.width() - side) / 2;
            let y = (image.height() - side) / 2;
            let square = image
                .crop_imm(x, y, side, side)
                .thumbnail(image_size, image_size);
            let _ = sender.send((video_id, square));
        });
    }

    pub fn render(&mut self, frame: &mut Frame, layout: &FrameLayout, player: &PlayerState) {
        let current = player.current().map(|x| x.video_id.clone());
        self.draw(frame, layout.art, current);
    }

    /// Draws the cover of `video_id` in `area`, fetching it when the song changed.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, current: Option<String>) {
        let Some(picker) = &self.picker else {
            return;
        };
        if current != self.video_id {
            self.video_id = current.clone();
            self.protocol = None;
            if let Some(id) = current {
                self.fetch(id);
            }
        }
        while let Ok((id, image)) = self.receiver.try_recv() {
            if Some(&id) == self.video_id.as_ref() {
                self.protocol = Some(picker.new_resize_protocol(image));
            }
        }
        if let Some(protocol) = self.protocol.as_mut() {
            if area.width == 0 || area.height == 0 {
                return;
            }
            frame.render_widget(Clear, area);
            let resize = if self.scale_up {
                Resize::Scale(None)
            } else {
                Resize::Fit(None)
            };
            frame.render_stateful_widget(StatefulImage::default().resize(resize), area, protocol);
        }
    }
}

impl Default for Thumbnail {
    fn default() -> Self {
        Self::new()
    }
}
