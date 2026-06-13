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
    /// Slash commands available this session (names without a leading '/').
    commands: Vec<String>,
    /// Active slash-command popup, when the first line is a lone `/token`.
    popup: Option<SlashPopup>,
}

/// State of the slash-command completion popup.
struct SlashPopup {
    /// Commands matching the current `/token`, prefix matches ranked first.
    matches: Vec<String>,
    /// Index into `matches` of the highlighted row.
    selected: usize,
}

#[allow(dead_code)]
impl Composer {
    pub fn new() -> Self {
        let textarea = Self::make_textarea(false);
        Self {
            textarea,
            active: false,
            commands: Vec::new(),
            popup: None,
        }
    }

    /// Set the slash commands offered by completion (names without '/').
    ///
    /// Re-evaluates the popup unconditionally so a command list that arrives
    /// *after* the user has already typed `/` still opens the popup.
    pub fn set_slash_commands(&mut self, commands: Vec<String>) {
        self.commands = commands;
        self.refresh_popup();
    }

    /// True while the slash-command popup is open.
    pub fn slash_popup_open(&self) -> bool {
        self.popup.is_some()
    }

    /// Filtered command names shown in the popup (empty when closed).
    pub fn slash_matches(&self) -> &[String] {
        self.popup.as_ref().map(|p| p.matches.as_slice()).unwrap_or(&[])
    }

    /// Index of the highlighted popup row.
    pub fn slash_selected(&self) -> usize {
        self.popup.as_ref().map(|p| p.selected).unwrap_or(0)
    }

    /// Move the popup selection by `delta`, wrapping around.
    fn slash_move(&mut self, delta: i32) {
        if let Some(p) = &mut self.popup {
            let len = p.matches.len() as i32;
            if len > 0 {
                p.selected = (p.selected as i32 + delta).rem_euclid(len) as usize;
            }
        }
    }

    /// Replace the composer text with the highlighted command and close the popup.
    fn accept_slash(&mut self) {
        let chosen = self
            .popup
            .as_ref()
            .and_then(|p| p.matches.get(p.selected).cloned());
        if let Some(name) = chosen {
            self.set_text(&format!("/{name} "));
        }
        self.popup = None;
    }

    /// Recompute the popup from the current text. Opens it when the first (only)
    /// line is a lone `/token` with matches; closes it otherwise.
    fn refresh_popup(&mut self) {
        let lines = self.textarea.lines();
        let single_line = lines.len() == 1;
        let first = lines.first().map(|s| s.as_str()).unwrap_or("");
        if single_line
            && let Some(rest) = first.strip_prefix('/')
            && !rest.contains(char::is_whitespace)
        {
            let filter = rest.to_lowercase();
            let mut prefix: Vec<String> = Vec::new();
            let mut other: Vec<String> = Vec::new();
            for c in &self.commands {
                let cl = c.to_lowercase();
                if cl.starts_with(&filter) {
                    prefix.push(c.clone());
                } else if cl.contains(&filter) {
                    other.push(c.clone());
                }
            }
            prefix.extend(other);
            if prefix.is_empty() {
                self.popup = None;
            } else {
                let prev = self.popup.as_ref().map(|p| p.selected).unwrap_or(0);
                let selected = prev.min(prefix.len() - 1);
                self.popup = Some(SlashPopup { matches: prefix, selected });
            }
        } else {
            self.popup = None;
        }
    }

    /// Handle a key event. Returns `Some(text)` if Enter is pressed to send,
    /// `None` otherwise.
    ///
    /// While the slash-command popup is open, navigation/accept/dismiss keys
    /// drive the popup instead of the text area. Otherwise the key is processed
    /// normally and the popup is recomputed from the new text.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        if self.popup.is_some() {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Up => return self.slash_nav_none(-1),
                KeyCode::Down => return self.slash_nav_none(1),
                KeyCode::Char('p') if ctrl => return self.slash_nav_none(-1),
                KeyCode::Char('n') if ctrl => return self.slash_nav_none(1),
                KeyCode::Tab => {
                    self.accept_slash();
                    return None;
                }
                KeyCode::Enter
                    if !key.modifiers.contains(KeyModifiers::SHIFT)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.accept_slash();
                    return None;
                }
                KeyCode::Esc => {
                    self.popup = None;
                    return None;
                }
                _ => {}
            }
        }
        let result = self.handle_key_inner(key);
        self.refresh_popup();
        result
    }

    /// Move the popup selection and return None (helper to keep match arms tidy).
    fn slash_nav_none(&mut self, delta: i32) -> Option<String> {
        self.slash_move(delta);
        None
    }

    fn handle_key_inner(&mut self, key: KeyEvent) -> Option<String> {
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
            // Ctrl+A / Cmd+A = select all (override tui-textarea's default "move to line start")
            KeyEvent {
                code: KeyCode::Char('a'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL)
                || modifiers.contains(KeyModifiers::SUPER) =>
            {
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
        self.popup = None;
    }

    /// Set active state, updating border and selection styling.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        // Leaving the composer dismisses the popup so it can't render or capture
        // keys once focus has moved elsewhere.
        if !active {
            self.popup = None;
        }
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

    /// Set copy-flash style (white highlight) on the selection.
    pub fn set_copy_flash(&mut self, flash: bool) {
        if flash {
            self.textarea.set_selection_style(Style::default().bg(Color::White).fg(Color::Black));
        } else if self.active {
            self.textarea.set_selection_style(Style::default().bg(Color::Cyan).fg(Color::Black));
        } else {
            self.textarea.set_selection_style(Style::default().bg(Color::DarkGray).fg(Color::White));
        }
    }

    /// Returns true if the composer has no text content.
    pub fn is_empty(&self) -> bool {
        self.textarea.lines().iter().all(|l| l.is_empty())
    }

    /// Get the current draft text.
    pub fn draft_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Insert pasted text at cursor, preserving newlines without triggering send.
    pub fn insert_paste(&mut self, text: &str) {
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.textarea.insert_newline();
            }
            if !line.is_empty() {
                self.textarea.insert_str(line);
            }
        }
    }

    /// Replace the composer content with the given text.
    pub fn set_text(&mut self, text: &str) {
        self.popup = None;
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
        format!("{line_count}:{char_count}")
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

    #[test]
    fn insert_paste_into_empty() {
        let mut c = Composer::new();
        c.insert_paste("hello");
        assert_eq!(c.draft_text(), "hello");
        assert!(!c.is_empty());
    }

    #[test]
    fn insert_paste_multiline_preserves_newlines() {
        let mut c = Composer::new();
        c.insert_paste("a\nb\nc");
        assert_eq!(c.draft_text(), "a\nb\nc");
        assert_eq!(c.widget().lines().len(), 3);
    }

    #[test]
    fn insert_paste_appends_at_cursor() {
        let mut c = Composer::new();
        c.set_text("start");
        c.insert_paste(" more");
        assert_eq!(c.draft_text(), "start more");
    }

    fn ch(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn slash_composer() -> Composer {
        let mut c = Composer::new();
        c.set_slash_commands(vec![
            "compact".into(),
            "context".into(),
            "clear".into(),
            "review".into(),
        ]);
        c
    }

    #[test]
    fn slash_opens_popup_with_all_commands() {
        let mut c = slash_composer();
        assert!(!c.slash_popup_open());
        c.handle_key(ch('/'));
        assert!(c.slash_popup_open());
        assert_eq!(c.slash_matches().len(), 4);
    }

    #[test]
    fn typing_filters_popup_prefix_first() {
        let mut c = slash_composer();
        c.handle_key(ch('/'));
        c.handle_key(ch('c')); // compact, context, clear (review has no 'c')
        assert_eq!(
            c.slash_matches(),
            &["compact".to_string(), "context".to_string(), "clear".to_string()]
        );
        c.handle_key(ch('o')); // "co" -> compact, context
        assert_eq!(c.slash_matches(), &["compact".to_string(), "context".to_string()]);
    }

    #[test]
    fn tab_accepts_highlighted_command() {
        let mut c = slash_composer();
        c.handle_key(ch('/'));
        c.handle_key(ch('c'));
        c.handle_key(ch('o'));
        c.handle_key(ch('n')); // "con" -> context only
        assert_eq!(c.slash_matches(), &["context".to_string()]);
        let sent = c.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(sent, None);
        assert!(!c.slash_popup_open());
        assert_eq!(c.draft_text(), "/context ");
    }

    #[test]
    fn enter_accepts_does_not_send_while_popup_open() {
        let mut c = slash_composer();
        c.handle_key(ch('/'));
        c.handle_key(ch('c'));
        c.handle_key(ch('o'));
        c.handle_key(ch('m')); // compact only
        let sent = c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(sent, None);
        assert_eq!(c.draft_text(), "/compact ");
        assert!(!c.slash_popup_open());
    }

    #[test]
    fn esc_dismisses_popup_but_keeps_text() {
        let mut c = slash_composer();
        c.handle_key(ch('/'));
        assert!(c.slash_popup_open());
        c.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!c.slash_popup_open());
        assert_eq!(c.draft_text(), "/");
    }

    #[test]
    fn popup_closes_when_no_command_matches() {
        let mut c = slash_composer();
        c.handle_key(ch('/'));
        for x in "zzz".chars() {
            c.handle_key(ch(x));
        }
        assert!(!c.slash_popup_open());
    }

    #[test]
    fn popup_closes_after_space() {
        let mut c = slash_composer();
        c.handle_key(ch('/'));
        assert!(c.slash_popup_open());
        c.handle_key(ch(' '));
        assert!(!c.slash_popup_open());
    }

    #[test]
    fn arrow_keys_move_selection_and_wrap() {
        let mut c = Composer::new();
        c.set_slash_commands(vec!["a".into(), "ab".into(), "abc".into()]);
        c.handle_key(ch('/'));
        assert_eq!(c.slash_selected(), 0);
        c.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(c.slash_selected(), 1);
        c.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(c.slash_selected(), 0);
        c.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)); // wrap to last
        assert_eq!(c.slash_selected(), 2);
    }

    #[test]
    fn no_popup_without_commands() {
        let mut c = Composer::new();
        c.handle_key(ch('/'));
        assert!(!c.slash_popup_open());
    }

    #[test]
    fn prefix_matches_rank_before_substring_matches() {
        let mut c = Composer::new();
        c.set_slash_commands(vec!["redeploy".into(), "deploy".into()]);
        for x in "/deploy".chars() {
            c.handle_key(ch(x));
        }
        // "deploy" is a prefix match, "redeploy" only a substring match.
        assert_eq!(c.slash_matches(), &["deploy".to_string(), "redeploy".to_string()]);
    }

    #[test]
    fn filtering_is_case_insensitive() {
        let mut c = Composer::new();
        c.set_slash_commands(vec!["compact".into(), "context".into()]);
        c.handle_key(ch('/'));
        c.handle_key(ch('C'));
        c.handle_key(ch('O'));
        assert!(c.slash_popup_open());
        assert_eq!(c.slash_matches(), &["compact".to_string(), "context".to_string()]);
    }

    #[test]
    fn selection_clamps_when_matches_shrink() {
        let mut c = slash_composer();
        c.handle_key(ch('/'));
        c.handle_key(ch('c')); // compact, context, clear
        c.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        c.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // selected = 2 (clear)
        assert_eq!(c.slash_selected(), 2);
        c.handle_key(ch('o')); // "co" -> compact, context (len 2)
        assert!(c.slash_selected() < c.slash_matches().len());
    }

    #[test]
    fn set_active_false_dismisses_popup() {
        let mut c = slash_composer();
        c.handle_key(ch('/'));
        assert!(c.slash_popup_open());
        c.set_active(false);
        assert!(!c.slash_popup_open());
    }

    #[test]
    fn commands_arriving_after_slash_open_the_popup() {
        let mut c = Composer::new();
        c.handle_key(ch('/')); // no commands yet -> no popup
        assert!(!c.slash_popup_open());
        c.set_slash_commands(vec!["compact".into()]); // arrives late
        assert!(c.slash_popup_open());
    }
}
