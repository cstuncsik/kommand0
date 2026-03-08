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
            // Shift+Enter = newline (use insert_newline directly for cross-terminal reliability)
            KeyEvent {
                code: KeyCode::Enter,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::SHIFT) => {
                self.textarea.insert_newline();
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

    /// Dynamic height hint for layout: content lines (capped at 6) + 2 border lines.
    pub fn height_hint(&self) -> u16 {
        let content_lines = self.textarea.lines().len().max(1);
        let capped = content_lines.min(6); // max 6 lines of content
        (capped as u16) + 2 // +2 for top/bottom borders
    }

    /// Return a status string showing line:char count.
    pub fn status_text(&self) -> String {
        let lines = self.textarea.lines();
        let line_count = lines.len();
        let char_count: usize = lines.iter().map(|l| l.len()).sum::<usize>() + line_count.saturating_sub(1); // +newlines
        format!("{}:{}", line_count, char_count)
    }

    fn make_block(active: bool) -> Block<'static> {
        let border_style = if active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        Block::default()
            .title(" Composer ")
            .borders(Borders::ALL)
            .border_style(border_style)
    }

    fn make_textarea(active: bool) -> TextArea<'static> {
        let mut textarea = TextArea::default();
        textarea.set_block(Self::make_block(active));
        textarea.set_placeholder_text("Type a message...");
        textarea.set_cursor_line_style(Style::default());
        textarea
    }
}
