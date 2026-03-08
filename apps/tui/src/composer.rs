use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;

/// Multi-line text input widget wrapping `tui-textarea`.
///
/// Handles Enter-to-send and Shift+Enter-for-newline semantics.
/// Active/inactive states control border styling.
pub struct Composer {
    textarea: TextArea<'static>,
    active: bool,
}

#[allow(dead_code)]
impl Composer {
    pub fn new() -> Self {
        let textarea = Self::make_textarea(false);
        Self {
            textarea,
            active: false,
        }
    }

    /// Handle a key event. Returns `Some(text)` if Enter is pressed to send,
    /// `None` otherwise.
    ///
    /// - `Shift+Enter` inserts a newline
    /// - `Enter` extracts text, clears the composer, returns the text
    /// - All other keys are forwarded to the underlying TextArea
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        match key {
            // Shift+Enter = newline
            KeyEvent {
                code: KeyCode::Enter,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::SHIFT) => {
                self.textarea.input(key);
                None
            }
            // Enter = send
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                let lines: Vec<String> = self
                    .textarea
                    .lines()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let text = lines.join("\n").trim().to_string();
                if text.is_empty() {
                    return None;
                }
                // Reset textarea
                self.textarea = Self::make_textarea(self.active);
                Some(text)
            }
            // All other keys
            _ => {
                self.textarea.input(key);
                None
            }
        }
    }

    /// Clear the composer, resetting to an empty textarea.
    pub fn clear(&mut self) {
        self.textarea = Self::make_textarea(self.active);
    }

    /// Set active state, updating border styling.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        let block = Self::make_block(active);
        self.textarea.set_block(block);
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Returns true if the composer has no text content.
    pub fn is_empty(&self) -> bool {
        self.textarea.lines().iter().all(|l| l.is_empty())
    }

    /// Return a reference to the inner TextArea for rendering.
    pub fn widget(&self) -> &TextArea<'static> {
        &self.textarea
    }

    /// Minimum height hint for layout: 1 line of text + 2 border lines.
    pub fn height_hint(&self) -> u16 {
        3
    }

    fn make_block(active: bool) -> Block<'static> {
        let border_style = if active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        Block::default()
            .title(" Send message ")
            .borders(Borders::ALL)
            .border_style(border_style)
    }

    fn make_textarea(active: bool) -> TextArea<'static> {
        let mut textarea = TextArea::default();
        textarea.set_block(Self::make_block(active));
        textarea.set_placeholder_text("Type a message... (Enter to send, Shift+Enter for newline)");
        textarea.set_cursor_line_style(Style::default());
        textarea
    }
}
