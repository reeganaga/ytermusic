use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    widgets::{Paragraph, Widget, Wrap},
};

use super::{
    widgets::{error, hint, pad, panel, Hint},
    Ctx, EventResponse, ManagerMessage, Screen,
};

/// Error screen (audio device lost, invalid cookies, ...).
pub struct DeviceLost(pub Vec<String>, pub Option<ManagerMessage>);

impl Screen for DeviceLost {
    fn on_key(&mut self, key: KeyEvent, _: &Ctx) -> Option<EventResponse> {
        match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => Some(if let Some(m) = self.1.take() {
                EventResponse::Message(vec![m])
            } else {
                ManagerMessage::RestartPlayer.event()
            }),
            KeyCode::Esc | KeyCode::Char('q') => Some(ManagerMessage::Quit.event()),
            _ => None,
        }
    }

    fn on_mouse(&mut self, _: MouseEvent, _: Rect, _: &Ctx) -> EventResponse {
        EventResponse::None
    }

    fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &Ctx) {
        let inner = pad(panel(buf, area, "Error", ctx.focused), 2, 1);
        Paragraph::new(format!(
            "{}\n\nPress [Enter] or [Space] to retry.\nOr [Esc] to exit",
            self.0.join("\n")
        ))
        .style(error())
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
    }

    fn handle_global_message(&mut self, m: ManagerMessage) -> EventResponse {
        match m {
            ManagerMessage::Error(a, m) => {
                self.1 = *m;
                self.0.push(a);
                ManagerMessage::ChangeState(super::Screens::DeviceLost).event()
            }
            _ => EventResponse::None,
        }
    }

    fn hints(&self, _: &Ctx) -> Vec<Hint> {
        vec![hint("↵", "retry", "retry"), hint("esc", "quit", "quit")]
    }
}
