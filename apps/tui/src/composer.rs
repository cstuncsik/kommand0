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
            // Shift+Enter or Alt+Enter = newline
            // iTerm2 sends Shift+Enter as "\n" (Char('\n')), so catch that too.
            KeyEvent {
                code: KeyCode::Enter,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::SHIFT)
                || modifiers.contains(KeyModifiers::ALT) =>
            {
                self.textarea.insert_newline();
                None
            }
            // iTerm2 key binding: Shift+Return sends "\n" which arrives as Ctrl+J
            KeyEvent {
                code: KeyCode::Char('j'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
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
            // Ctrl+A = select all (override tui-textarea's default "move to line start")
            KeyEvent {
                code: KeyCode::Char('a'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.textarea.select_all();
                None
            }
            // All other keys (Shift+arrows handled natively by tui-textarea for selection)
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

    /// Set active state, updating border and selection styling.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        let block = Self::make_block(active);
        self.textarea.set_block(block);
        if active {
            self.textarea.set_selection_style(Style::default().bg(Color::Cyan).fg(Color::Black));
        } else {
            self.textarea.set_selection_style(Style::default().bg(Color::DarkGray).fg(Color::White));
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Returns true if the composer has no text content.
    pub fn is_empty(&self) -> bool {
        self.textarea.lines().iter().all(|l| l.is_empty())
    }

    /// Get the current draft text.
    pub fn draft_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Replace the composer content with the given text.
    pub fn set_text(&mut self, text: &str) {
        self.textarea = Self::make_textarea(self.active);
        for (i, line) in text.lines().enumerate() {
            if i > 0 {
                self.textarea.insert_newline();
            }
            self.textarea.insert_str(line);
        }
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

    /// Returns true if the composer has an active text selection.
    pub fn has_selection(&self) -> bool {
        self.textarea.is_selecting()
    }

    /// Extract the currently selected text from the composer.
    /// Returns None if no selection is active.
    pub fn selected_text(&self) -> Option<String> {
        let ((r1, c1), (r2, c2)) = self.textarea.selection_range()?;
        let lines = self.textarea.lines();
        if r1 == r2 {
            // Single-line selection
            let line = lines.get(r1)?;
            let chars: Vec<char> = line.chars().collect();
            let start = c1.min(chars.len());
            let end = c2.min(chars.len());
            Some(chars[start..end].iter().collect())
        } else {
            // Multi-line selection
            let mut result = String::new();
            for row in r1..=r2 {
                let line = lines.get(row).map(|s| s.as_str()).unwrap_or("");
                let chars: Vec<char> = line.chars().collect();
                if row == r1 {
                    let start = c1.min(chars.len());
                    result.extend(&chars[start..]);
                } else if row == r2 {
                    let end = c2.min(chars.len());
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.extend(&chars[..end]);
                } else {
                    result.push('\n');
                    result.extend(chars.iter());
                }
            }
            Some(result)
        }
    }

    /// Select all text in the composer.
    pub fn select_all(&mut self) {
        self.textarea.select_all();
    }

    /// Cancel (clear) any active text selection in the composer.
    pub fn cancel_selection(&mut self) {
        self.textarea.cancel_selection();
    }

    fn make_textarea(active: bool) -> TextArea<'static> {
        let mut textarea = TextArea::default();
        textarea.set_block(Self::make_block(active));
        textarea.set_placeholder_text("Type a message...");
        textarea.set_cursor_line_style(Style::default());
        textarea.set_selection_style(Style::default().bg(Color::Cyan).fg(Color::Black));
        textarea
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn has_selection_false_by_default() {
        let c = Composer::new();
        assert!(!c.has_selection());
    }

    #[test]
    fn select_all_then_has_selection() {
        let mut c = Composer::new();
        c.set_text("hello world");
        c.select_all();
        assert!(c.has_selection());
    }

    #[test]
    fn selected_text_after_select_all() {
        let mut c = Composer::new();
        c.set_text("hello world");
        c.select_all();
        let text = c.selected_text();
        assert_eq!(text, Some("hello world".to_string()));
    }

    #[test]
    fn selected_text_multiline() {
        let mut c = Composer::new();
        c.set_text("line one\nline two\nline three");
        c.select_all();
        let text = c.selected_text();
        assert_eq!(text, Some("line one\nline two\nline three".to_string()));
    }

    #[test]
    fn selected_text_none_when_no_selection() {
        let mut c = Composer::new();
        c.set_text("hello");
        assert_eq!(c.selected_text(), None);
    }

    #[test]
    fn selection_range_col_semantics_multibyte() {
        // Verify selection works correctly with multi-byte characters
        let mut c = Composer::new();
        c.set_text("caf\u{00e9}"); // "cafe" with e-acute as single codepoint
        c.select_all();
        assert!(c.has_selection());
        let text = c.selected_text();
        assert!(text.is_some());
        // The text should contain the full multibyte content
        assert!(text.unwrap().contains("caf"));
    }

    #[test]
    fn cancel_selection_clears() {
        let mut c = Composer::new();
        c.set_text("hello");
        c.select_all();
        assert!(c.has_selection());
        c.cancel_selection();
        assert!(!c.has_selection());
    }

    #[test]
    fn ctrl_a_in_handle_key_selects_all() {
        let mut c = Composer::new();
        c.set_text("test text");
        let result = c.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(result, None); // Ctrl+A does not send
        assert!(c.has_selection());
    }
}
