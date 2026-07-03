use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::theme::Theme;

/// What kind of delete is being confirmed.
#[derive(Clone)]
pub(crate) enum DeleteTarget {
    Workspace { name: String },
    Repo { id: String, name: String, workspace_count: usize },
}

/// The focused field in the Add-Workspace modal.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AddWorkspaceField {
    #[default]
    Name,
    Branch,
}

impl AddWorkspaceField {
    fn toggle(self) -> Self {
        match self {
            Self::Name => Self::Branch,
            Self::Branch => Self::Name,
        }
    }
}

/// Modal dialog state.
#[derive(Default)]
pub(crate) enum ModalState {
    #[default]
    None,
    AddRepo {
        input: String,
        cursor: usize,
        error: Option<String>,
        completions: Vec<String>,
        completion_index: Option<usize>,
    },
    AddWorkspace {
        repo_id: String,
        repo_name: String,
        /// Name field.
        input: String,
        cursor: usize,
        /// Optional existing branch to check out (blank = fork a new branch).
        branch: String,
        branch_cursor: usize,
        /// Which field has focus (Tab toggles).
        field: AddWorkspaceField,
        error: Option<String>,
    },
    ConfirmDelete {
        target: DeleteTarget,
    },
    RenameSession {
        ws_id: String,
        session_id: String,
        input: String,
        cursor: usize,
        error: Option<String>,
    },
    ConfirmCleanup {
        ws_id: String,
        ws_name: String,
        branch: String,
        dirty: bool,
        unpushed: bool,
    },
    /// A branch named `name` already exists (local or origin); offer to check it
    /// out instead of forking a fresh `kommand0/<name>`.
    ConfirmBranchCheckout {
        repo_id: String,
        repo_name: String,
        name: String,
    },
}

impl ModalState {
    pub fn is_active(&self) -> bool {
        !matches!(self, ModalState::None)
    }
}

/// Result of handling a modal key event.
pub(crate) enum ModalResult {
    /// Key was consumed, stay in modal.
    Consumed,
    /// Modal was cancelled (Esc).
    Cancelled,
    /// AddRepo submitted with path.
    SubmitRepo(String),
    /// AddWorkspace submitted with (repo_id, name, branch). An empty branch
    /// means fork a new branch; otherwise check out that existing branch.
    SubmitWorkspace(String, String, String),
    /// Delete confirmed.
    ConfirmDelete(DeleteTarget),
    /// Rename submitted with (ws_id, session_id, title); an empty title clears it.
    SubmitRename(String, String, String),
    /// Cleanup confirmed for a workspace id.
    ConfirmCleanup(String),
    /// Choice from the branch-exists prompt: check out the existing branch when
    /// `checkout`, else fork a fresh `kommand0/<name>`.
    BranchCheckoutChoice { repo_id: String, name: String, checkout: bool },
}

/// Handle a key event when a modal is active.
pub(crate) fn handle_modal_key(modal: &mut ModalState, key: KeyEvent) -> ModalResult {
    match modal {
        ModalState::None => ModalResult::Consumed,
        ModalState::AddRepo {
            input,
            cursor,
            error,
            completions,
            completion_index,
        } => {
            // Clear error on any key
            *error = None;

            match key.code {
                KeyCode::Esc => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                KeyCode::Enter => {
                    let path = input.trim().to_string();
                    if path.is_empty() {
                        *error = Some("Path cannot be empty".to_string());
                        ModalResult::Consumed
                    } else {
                        let result = ModalResult::SubmitRepo(path);
                        *modal = ModalState::None;
                        result
                    }
                }
                KeyCode::Tab => {
                    // Path completion
                    if completions.is_empty() {
                        // Generate completions
                        *completions = complete_path(input);
                        *completion_index = if completions.is_empty() {
                            None
                        } else {
                            Some(0)
                        };
                    } else {
                        // Cycle to next completion
                        if let Some(idx) = completion_index {
                            *idx = (*idx + 1) % completions.len();
                        }
                    }
                    // Apply current completion
                    if let Some(idx) = *completion_index
                        && let Some(path) = completions.get(idx) {
                            *input = path.clone();
                            *cursor = input.len();
                        }
                    ModalResult::Consumed
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                KeyCode::Char(c) => {
                    input.insert(*cursor, c);
                    *cursor += c.len_utf8();
                    // Reset completions when typing
                    completions.clear();
                    *completion_index = None;
                    ModalResult::Consumed
                }
                KeyCode::Backspace => {
                    if *cursor > 0 {
                        let prev = input[..*cursor]
                            .char_indices()
                            .last()
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        input.replace_range(prev..*cursor, "");
                        *cursor = prev;
                    }
                    completions.clear();
                    *completion_index = None;
                    ModalResult::Consumed
                }
                KeyCode::Left => {
                    if *cursor > 0 {
                        *cursor = input[..*cursor]
                            .char_indices()
                            .last()
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                    }
                    ModalResult::Consumed
                }
                KeyCode::Right => {
                    if *cursor < input.len() {
                        *cursor = input[*cursor..]
                            .char_indices()
                            .nth(1)
                            .map(|(i, _)| *cursor + i)
                            .unwrap_or(input.len());
                    }
                    ModalResult::Consumed
                }
                KeyCode::Home => {
                    *cursor = 0;
                    ModalResult::Consumed
                }
                KeyCode::End => {
                    *cursor = input.len();
                    ModalResult::Consumed
                }
                _ => ModalResult::Consumed,
            }
        }
        ModalState::AddWorkspace {
            repo_id,
            repo_name: _,
            input,
            cursor,
            branch,
            branch_cursor,
            field,
            error,
        } => {
            *error = None;

            match key.code {
                KeyCode::Esc => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                // Tab (or up/down) moves focus between the Name and Branch fields.
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                    *field = field.toggle();
                    ModalResult::Consumed
                }
                KeyCode::Enter => {
                    let name = input.trim().to_string();
                    if name.is_empty() {
                        *error = Some("Name cannot be empty".to_string());
                        ModalResult::Consumed
                    } else {
                        let result = ModalResult::SubmitWorkspace(
                            repo_id.clone(),
                            name,
                            branch.trim().to_string(),
                        );
                        *modal = ModalState::None;
                        result
                    }
                }
                _ => {
                    // Text editing acts on the focused field.
                    let (buf, cur): (&mut String, &mut usize) = match field {
                        AddWorkspaceField::Name => (input, cursor),
                        AddWorkspaceField::Branch => (branch, branch_cursor),
                    };
                    match key.code {
                        KeyCode::Char(c) => {
                            buf.insert(*cur, c);
                            *cur += c.len_utf8();
                        }
                        KeyCode::Backspace if *cur > 0 => {
                            let prev =
                                buf[..*cur].char_indices().last().map(|(i, _)| i).unwrap_or(0);
                            buf.replace_range(prev..*cur, "");
                            *cur = prev;
                        }
                        KeyCode::Left if *cur > 0 => {
                            *cur = buf[..*cur].char_indices().last().map(|(i, _)| i).unwrap_or(0);
                        }
                        KeyCode::Right if *cur < buf.len() => {
                            *cur = buf[*cur..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| *cur + i)
                                .unwrap_or(buf.len());
                        }
                        _ => {}
                    }
                    ModalResult::Consumed
                }
            }
        }
        ModalState::ConfirmDelete { target } => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let t = target.clone();
                    *modal = ModalState::None;
                    ModalResult::ConfirmDelete(t)
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                _ => ModalResult::Consumed,
            }
        }
        ModalState::RenameSession {
            ws_id,
            session_id,
            input,
            cursor,
            error,
        } => {
            *error = None;

            match key.code {
                KeyCode::Esc => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                KeyCode::Enter => {
                    // An empty title is allowed — it clears any existing one.
                    let title = input.trim().to_string();
                    let result = ModalResult::SubmitRename(ws_id.clone(), session_id.clone(), title);
                    *modal = ModalState::None;
                    result
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                KeyCode::Char(c) => {
                    input.insert(*cursor, c);
                    *cursor += c.len_utf8();
                    ModalResult::Consumed
                }
                KeyCode::Backspace => {
                    if *cursor > 0 {
                        let prev = input[..*cursor]
                            .char_indices()
                            .last()
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        input.replace_range(prev..*cursor, "");
                        *cursor = prev;
                    }
                    ModalResult::Consumed
                }
                KeyCode::Left => {
                    if *cursor > 0 {
                        *cursor = input[..*cursor]
                            .char_indices()
                            .last()
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                    }
                    ModalResult::Consumed
                }
                KeyCode::Right => {
                    if *cursor < input.len() {
                        *cursor = input[*cursor..]
                            .char_indices()
                            .nth(1)
                            .map(|(i, _)| *cursor + i)
                            .unwrap_or(input.len());
                    }
                    ModalResult::Consumed
                }
                _ => ModalResult::Consumed,
            }
        }
        ModalState::ConfirmCleanup { ws_id, .. } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let id = ws_id.clone();
                *modal = ModalState::None;
                ModalResult::ConfirmCleanup(id)
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                *modal = ModalState::None;
                ModalResult::Cancelled
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                *modal = ModalState::None;
                ModalResult::Cancelled
            }
            _ => ModalResult::Consumed,
        },
        ModalState::ConfirmBranchCheckout { repo_id, name, .. } => {
            match key.code {
                // Ctrl+C cancels (every modal treats it so). Guard arms are tried
                // top-down, so this wins over the bare-`c` confirm below.
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                KeyCode::Enter | KeyCode::Char('c') => {
                    let (repo_id, name) = (repo_id.clone(), name.clone());
                    *modal = ModalState::None;
                    ModalResult::BranchCheckoutChoice { repo_id, name, checkout: true }
                }
                KeyCode::Char('f') => {
                    let (repo_id, name) = (repo_id.clone(), name.clone());
                    *modal = ModalState::None;
                    ModalResult::BranchCheckoutChoice { repo_id, name, checkout: false }
                }
                KeyCode::Esc => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                _ => ModalResult::Consumed,
            }
        }
    }
}

/// Sanitize pasted text for a single-line input. Drops control chars (so a
/// pasted newline can't submit or corrupt the buffer) plus line/paragraph
/// separators, bidi overrides, and zero-width/BOM format chars — which would
/// otherwise allow Trojan-Source-style visual spoofing of a name/path. Shared
/// by every paste sink (modal, palette, filter) so they stay in step.
pub(crate) fn sanitize_paste(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    *c,
                    '\u{2028}' | '\u{2029}'
                        | '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                )
        })
        .collect()
}

/// Insert pasted text into the focused text field of a modal.
///
/// Bracketed paste arrives as one `Event::Paste`, not as `Char` keys, so the
/// key handler above never sees it — without this, paste is dropped in every
/// modal. Confirm-only modals (delete/cleanup) have no field and ignore it.
/// Text is sanitized via [`sanitize_paste`] (these are single-line fields).
pub(crate) fn handle_modal_paste(modal: &mut ModalState, text: &str) {
    let clean = sanitize_paste(text);
    if clean.is_empty() {
        return;
    }
    let (buf, cur): (&mut String, &mut usize) = match modal {
        ModalState::AddRepo {
            input,
            cursor,
            error,
            completions,
            completion_index,
        } => {
            // Paste is like typing: clear the error and stale completions.
            *error = None;
            completions.clear();
            *completion_index = None;
            (input, cursor)
        }
        ModalState::AddWorkspace {
            input,
            cursor,
            branch,
            branch_cursor,
            field,
            error,
            ..
        } => {
            *error = None;
            match field {
                AddWorkspaceField::Name => (input, cursor),
                AddWorkspaceField::Branch => (branch, branch_cursor),
            }
        }
        ModalState::RenameSession { input, cursor, error, .. } => {
            *error = None;
            (input, cursor)
        }
        ModalState::None
        | ModalState::ConfirmDelete { .. }
        | ModalState::ConfirmCleanup { .. }
        | ModalState::ConfirmBranchCheckout { .. } => {
            return;
        }
    };
    buf.insert_str(*cur, &clean);
    *cur += clean.len();
}

/// Generate path completions for the given partial input.
fn complete_path(partial: &str) -> Vec<String> {
    let path = if partial.is_empty() {
        std::path::PathBuf::from(".")
    } else if partial.starts_with('~') {
        // Expand tilde
        if let Some(home) = dirs_home(partial) {
            home
        } else {
            std::path::PathBuf::from(partial)
        }
    } else {
        std::path::PathBuf::from(partial)
    };

    // Split into parent dir and prefix
    let (dir, prefix) = if path.is_dir() && partial.ends_with('/') {
        (path.clone(), String::new())
    } else {
        let parent = path.parent().unwrap_or(std::path::Path::new("."));
        let prefix = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        (parent.to_path_buf(), prefix)
    };

    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type()
                && ft.is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(&prefix) && !name.starts_with('.') {
                        let full = dir.join(&name);
                        let mut display = full.to_string_lossy().to_string();
                        display.push('/');
                        results.push(display);
                    }
                }
            if results.len() >= 20 {
                break;
            }
        }
    }
    results.sort();
    results
}

/// Expand ~ to home directory.
fn dirs_home(partial: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    if partial == "~" {
        Some(std::path::PathBuf::from(&home))
    } else { partial.strip_prefix("~/").map(|rest| std::path::PathBuf::from(home).join(rest)) }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

/// Render the modal dialog overlay.
pub(crate) fn render_modal(frame: &mut ratatui::Frame, modal: &ModalState, theme: Theme) {
    let th = theme;
    match modal {
        ModalState::None => {}
        ModalState::AddRepo {
            input,
            cursor,
            error,
            completions,
            completion_index,
        } => {
            let area = centered_rect(50, 30, frame.area());
            frame.render_widget(Clear, area);

            let inner = Layout::vertical([
                Constraint::Length(2), // padding + label
                Constraint::Length(1), // input field
                Constraint::Length(1), // error line
                Constraint::Min(1),   // completions
                Constraint::Length(1), // footer
            ])
            .split(Rect::new(
                area.x + 2,
                area.y + 1,
                area.width.saturating_sub(4),
                area.height.saturating_sub(2),
            ));

            // Title + border
            let block = Block::default()
                .title(" Add Repository ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(th.accent));
            frame.render_widget(block, area);

            // Label
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "Repository path:",
                    Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                )),
                inner[0],
            );

            // Input field with cursor
            let display_input = render_input_with_cursor(input, *cursor, inner[1].width as usize, th);
            frame.render_widget(
                Paragraph::new(display_input)
                    .style(Style::default().fg(th.text).bg(th.muted)),
                inner[1],
            );

            // Error
            if let Some(err) = error {
                frame.render_widget(
                    Paragraph::new(err.as_str()).style(Style::default().fg(th.error)),
                    inner[2],
                );
            }

            // Completions
            if !completions.is_empty() {
                let comp_lines: Vec<Line> = completions
                    .iter()
                    .enumerate()
                    .take(inner[3].height as usize)
                    .map(|(i, path)| {
                        let selected = *completion_index == Some(i);
                        let style = if selected {
                            Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(th.muted)
                        };
                        Line::styled(format!("  {path}"), style)
                    })
                    .collect();
                frame.render_widget(Paragraph::new(comp_lines), inner[3]);
            }

            // Footer
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter", Style::default().fg(th.accent)),
                    Span::raw(": submit  "),
                    Span::styled("Tab", Style::default().fg(th.accent)),
                    Span::raw(": complete  "),
                    Span::styled("Esc", Style::default().fg(th.accent)),
                    Span::raw(": cancel"),
                ])),
                inner[4],
            );
        }
        ModalState::AddWorkspace {
            repo_id: _,
            repo_name,
            input,
            cursor,
            branch,
            branch_cursor,
            field,
            error,
        } => {
            let area = centered_rect(55, 40, frame.area());
            frame.render_widget(Clear, area);

            let inner = Layout::vertical([
                Constraint::Length(1), // name label
                Constraint::Length(1), // name input
                Constraint::Length(1), // branch label
                Constraint::Length(1), // branch input
                Constraint::Length(1), // error
                Constraint::Min(0),    // spacer
                Constraint::Length(1), // footer
            ])
            .split(Rect::new(
                area.x + 2,
                area.y + 1,
                area.width.saturating_sub(4),
                area.height.saturating_sub(2),
            ));

            frame.render_widget(
                Block::default()
                    .title(format!(" Add Workspace to {repo_name} "))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(th.accent)),
                area,
            );

            let name_focused = *field == AddWorkspaceField::Name;
            let lbl = |focused: bool| {
                if focused {
                    Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(th.muted)
                }
            };

            // Name field — the cursor is drawn only on the focused field.
            frame.render_widget(
                Paragraph::new(Line::styled("Workspace name:", lbl(name_focused))),
                inner[0],
            );
            if name_focused {
                frame.render_widget(
                    Paragraph::new(render_input_with_cursor(input, *cursor, inner[1].width as usize, th))
                        .style(Style::default().fg(th.text).bg(th.muted)),
                    inner[1],
                );
            } else {
                frame.render_widget(
                    Paragraph::new(input.as_str()).style(Style::default().fg(th.text)),
                    inner[1],
                );
            }

            // Branch field.
            frame.render_widget(
                Paragraph::new(Line::styled("Branch (blank = new):", lbl(!name_focused))),
                inner[2],
            );
            if name_focused {
                frame.render_widget(
                    Paragraph::new(branch.as_str()).style(Style::default().fg(th.text)),
                    inner[3],
                );
            } else {
                frame.render_widget(
                    Paragraph::new(render_input_with_cursor(branch, *branch_cursor, inner[3].width as usize, th))
                        .style(Style::default().fg(th.text).bg(th.muted)),
                    inner[3],
                );
            }

            if let Some(err) = error {
                frame.render_widget(
                    Paragraph::new(err.as_str()).style(Style::default().fg(th.error)),
                    inner[4],
                );
            }

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter", Style::default().fg(th.accent)),
                    Span::raw(": submit  "),
                    Span::styled("Tab", Style::default().fg(th.accent)),
                    Span::raw(": field  "),
                    Span::styled("Esc", Style::default().fg(th.accent)),
                    Span::raw(": cancel"),
                ])),
                inner[6],
            );
        }
        ModalState::ConfirmDelete { target } => {
            let area = centered_rect(50, 20, frame.area());
            frame.render_widget(Clear, area);

            let (title, message) = match target {
                DeleteTarget::Workspace { name } => (
                    " Delete Workspace ".to_string(),
                    format!("Delete workspace '{name}'?"),
                ),
                DeleteTarget::Repo { name, workspace_count, .. } => {
                    if *workspace_count > 0 {
                        (
                            " Delete Repository ".to_string(),
                            format!(
                                "Delete repo '{name}' and its {workspace_count} workspace(s)?"
                            ),
                        )
                    } else {
                        (
                            " Delete Repository ".to_string(),
                            format!("Delete repo '{name}'?"),
                        )
                    }
                }
            };

            let inner = Layout::vertical([
                Constraint::Length(2), // message
                Constraint::Min(0),   // spacer
                Constraint::Length(1), // footer
            ])
            .split(Rect::new(
                area.x + 2,
                area.y + 1,
                area.width.saturating_sub(4),
                area.height.saturating_sub(2),
            ));

            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(th.error));
            frame.render_widget(block, area);

            frame.render_widget(
                Paragraph::new(Line::styled(
                    message,
                    Style::default().fg(th.text),
                )),
                inner[0],
            );

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("y", Style::default().fg(th.error).add_modifier(Modifier::BOLD)),
                    Span::raw(": delete  "),
                    Span::styled("n/Esc", Style::default().fg(th.accent)),
                    Span::raw(": cancel"),
                ])),
                inner[2],
            );
        }
        ModalState::RenameSession {
            input, cursor, error, ..
        } => {
            let area = centered_rect(50, 25, frame.area());
            frame.render_widget(Clear, area);

            let inner = Layout::vertical([
                Constraint::Length(2), // label
                Constraint::Length(1), // input
                Constraint::Length(1), // error
                Constraint::Min(0),   // spacer
                Constraint::Length(1), // footer
            ])
            .split(Rect::new(
                area.x + 2,
                area.y + 1,
                area.width.saturating_sub(4),
                area.height.saturating_sub(2),
            ));

            let block = Block::default()
                .title(" Rename Session ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(th.accent));
            frame.render_widget(block, area);

            frame.render_widget(
                Paragraph::new(Line::styled(
                    "Session name (empty clears):",
                    Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                )),
                inner[0],
            );

            let display_input = render_input_with_cursor(input, *cursor, inner[1].width as usize, th);
            frame.render_widget(
                Paragraph::new(display_input)
                    .style(Style::default().fg(th.text).bg(th.muted)),
                inner[1],
            );

            if let Some(err) = error {
                frame.render_widget(
                    Paragraph::new(err.as_str()).style(Style::default().fg(th.error)),
                    inner[2],
                );
            }

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter", Style::default().fg(th.accent)),
                    Span::raw(": submit  "),
                    Span::styled("Esc", Style::default().fg(th.accent)),
                    Span::raw(": cancel"),
                ])),
                inner[4],
            );
        }
        ModalState::ConfirmCleanup {
            ws_name,
            branch,
            dirty,
            unpushed,
            ..
        } => {
            let area = centered_rect(60, 35, frame.area());
            frame.render_widget(Clear, area);

            let inner = Layout::vertical([
                Constraint::Length(1), // message
                Constraint::Length(1), // branch
                Constraint::Length(1), // requirement
                Constraint::Length(2), // warnings
                Constraint::Min(0),
                Constraint::Length(1), // footer
            ])
            .split(Rect::new(
                area.x + 2,
                area.y + 1,
                area.width.saturating_sub(4),
                area.height.saturating_sub(2),
            ));

            let block = Block::default()
                .title(" Clean Up Workspace ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(th.dirty));
            frame.render_widget(block, area);

            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!("Remove worktree and delete branch for '{ws_name}'?"),
                    Style::default().fg(th.text),
                )),
                inner[0],
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Branch: ", Style::default().fg(th.accent)),
                    Span::raw(branch.as_str()),
                ])),
                inner[1],
            );
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "Only proceeds if the branch's PR has been merged.",
                    Style::default().fg(th.muted),
                )),
                inner[2],
            );
            let mut warn = Vec::new();
            if *dirty {
                warn.push(Line::styled(
                    "⚠ Uncommitted changes will block cleanup.",
                    Style::default().fg(th.error),
                ));
            }
            if *unpushed {
                warn.push(Line::styled(
                    "⚠ Unpushed commits will block cleanup.",
                    Style::default().fg(th.error),
                ));
            }
            frame.render_widget(Paragraph::new(warn), inner[3]);

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("y", Style::default().fg(th.dirty).add_modifier(Modifier::BOLD)),
                    Span::raw(": clean up  "),
                    Span::styled("n/Esc", Style::default().fg(th.accent)),
                    Span::raw(": cancel"),
                ])),
                inner[5],
            );
        }
        ModalState::ConfirmBranchCheckout { repo_name, name, .. } => {
            let area = centered_rect(55, 20, frame.area());
            frame.render_widget(Clear, area);

            let inner = Layout::vertical([
                Constraint::Length(2), // message
                Constraint::Min(0),   // spacer
                Constraint::Length(1), // footer
            ])
            .split(Rect::new(
                area.x + 2,
                area.y + 1,
                area.width.saturating_sub(4),
                area.height.saturating_sub(2),
            ));

            let block = Block::default()
                .title(format!(" Add Workspace to {repo_name} "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(th.accent));
            frame.render_widget(block, area);

            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!("Branch '{name}' already exists."),
                    Style::default().fg(th.text),
                )),
                inner[0],
            );

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
                    Span::raw(" check it out   "),
                    Span::styled("f", Style::default().fg(th.accent)),
                    Span::raw(format!(" fork kommand0/{name}   ")),
                    Span::styled("Esc", Style::default().fg(th.accent)),
                    Span::raw(" cancel"),
                ])),
                inner[2],
            );
        }
    }
}

/// Render an input string with a visible cursor.
pub(crate) fn render_input_with_cursor(input: &str, cursor: usize, width: usize, th: Theme) -> Line<'static> {
    if input.is_empty() {
        return Line::from(vec![
            Span::styled("█", Style::default().fg(th.accent)),
            Span::raw(" ".repeat(width.saturating_sub(1))),
        ]);
    }

    let before = &input[..cursor];
    let cursor_char = input[cursor..].chars().next();
    let after_start = cursor + cursor_char.map_or(0, |c| c.len_utf8());
    let after = &input[after_start..];

    let mut spans = vec![Span::raw(before.to_string())];
    if let Some(c) = cursor_char {
        spans.push(Span::styled(
            c.to_string(),
            Style::default().fg(th.inverse).bg(th.accent),
        ));
    } else {
        spans.push(Span::styled("█", Style::default().fg(th.accent)));
    }
    spans.push(Span::raw(after.to_string()));

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn add_workspace_modal() -> ModalState {
        ModalState::AddWorkspace {
            repo_id: "r1".into(),
            repo_name: "demo".into(),
            input: String::new(),
            cursor: 0,
            branch: String::new(),
            branch_cursor: 0,
            field: AddWorkspaceField::Name,
            error: None,
        }
    }

    #[test]
    fn add_workspace_edits_both_fields_and_submits_the_branch() {
        let mut modal = add_workspace_modal();
        for c in "ws".chars() {
            handle_modal_key(&mut modal, key(KeyCode::Char(c)));
        }
        // Tab moves focus to the branch field; typing now edits the branch.
        handle_modal_key(&mut modal, key(KeyCode::Tab));
        for c in "feat/x".chars() {
            handle_modal_key(&mut modal, key(KeyCode::Char(c)));
        }
        match handle_modal_key(&mut modal, key(KeyCode::Enter)) {
            ModalResult::SubmitWorkspace(repo_id, name, branch) => {
                assert_eq!(repo_id, "r1");
                assert_eq!(name, "ws", "name field unaffected by editing the branch");
                assert_eq!(branch, "feat/x", "branch field is carried in the submit");
            }
            _ => panic!("expected SubmitWorkspace"),
        }
        assert!(matches!(modal, ModalState::None), "submit closes the modal");
    }

    #[test]
    fn add_workspace_blank_branch_submits_empty() {
        let mut modal = add_workspace_modal();
        for c in "ws".chars() {
            handle_modal_key(&mut modal, key(KeyCode::Char(c)));
        }
        match handle_modal_key(&mut modal, key(KeyCode::Enter)) {
            ModalResult::SubmitWorkspace(_, name, branch) => {
                assert_eq!(name, "ws");
                assert!(branch.is_empty(), "a blank branch means fork a new one");
            }
            _ => panic!("expected SubmitWorkspace"),
        }
    }

    #[test]
    fn paste_inserts_at_cursor_and_strips_newlines() {
        let mut modal = ModalState::AddRepo {
            input: "ab".into(),
            cursor: 1, // between 'a' and 'b'
            error: Some("stale".into()),
            completions: vec!["x".into()],
            completion_index: Some(0),
        };
        handle_modal_paste(&mut modal, "/tmp\n");
        match modal {
            ModalState::AddRepo { input, cursor, error, completions, .. } => {
                assert_eq!(input, "a/tmpb", "text lands at the cursor, newline stripped");
                assert_eq!(cursor, 5, "cursor advances past the pasted bytes");
                assert!(error.is_none() && completions.is_empty(), "paste clears error+completions");
            }
            _ => panic!("modal changed variant"),
        }
    }

    #[test]
    fn paste_targets_the_focused_workspace_field() {
        let mut modal = add_workspace_modal();
        handle_modal_paste(&mut modal, "my-ws");
        handle_modal_key(&mut modal, key(KeyCode::Tab)); // focus Branch
        handle_modal_paste(&mut modal, "feat/paste");
        match handle_modal_key(&mut modal, key(KeyCode::Enter)) {
            ModalResult::SubmitWorkspace(_, name, branch) => {
                assert_eq!(name, "my-ws");
                assert_eq!(branch, "feat/paste");
            }
            _ => panic!("expected SubmitWorkspace"),
        }
    }

    #[test]
    fn paste_is_a_noop_on_confirm_modals() {
        // Confirm-only modals have no field — must not panic or change variant.
        let mut modal = ModalState::ConfirmDelete {
            target: DeleteTarget::Workspace { name: "w".into() },
        };
        handle_modal_paste(&mut modal, "ignored");
        assert!(matches!(modal, ModalState::ConfirmDelete { .. }));
    }

    #[test]
    fn paste_multibyte_keeps_cursor_on_a_char_boundary() {
        // "áb": á is 2 bytes (0..2), b at byte 2 — cursor 2 is a valid boundary.
        // Guards the byte-index invariant: a refactor to chars().count() would
        // land the cursor mid-codepoint here and panic in render's slice.
        let mut modal = ModalState::AddRepo {
            input: "áb".into(),
            cursor: 2,
            error: None,
            completions: vec![],
            completion_index: None,
        };
        handle_modal_paste(&mut modal, "é"); // 2 bytes
        match modal {
            ModalState::AddRepo { input, cursor, .. } => {
                assert_eq!(input, "áéb");
                assert_eq!(cursor, 4, "cursor advances by byte length, not char count");
                assert!(input.is_char_boundary(cursor), "cursor must stay slice-safe");
            }
            _ => panic!("modal changed variant"),
        }
    }

    #[test]
    fn paste_into_rename_session_inserts_and_clears_error() {
        let mut modal = ModalState::RenameSession {
            ws_id: "w".into(),
            session_id: "s".into(),
            input: "ab".into(),
            cursor: 1,
            error: Some("stale".into()),
        };
        handle_modal_paste(&mut modal, "/x\n");
        match modal {
            ModalState::RenameSession { input, cursor, error, .. } => {
                assert_eq!(input, "a/xb", "text lands at the cursor, newline stripped");
                assert_eq!(cursor, 3);
                assert!(error.is_none(), "paste clears the error like typing does");
            }
            _ => panic!("modal changed variant"),
        }
    }

    #[test]
    fn paste_of_only_control_chars_is_a_noop_in_modals() {
        // Sanitizes to empty -> early return, before touching error/completions.
        let mut modal = ModalState::AddRepo {
            input: "ab".into(),
            cursor: 1,
            error: Some("stale".into()),
            completions: vec!["x".into()],
            completion_index: Some(0),
        };
        handle_modal_paste(&mut modal, "\n\t");
        match modal {
            ModalState::AddRepo { input, cursor, error, completions, .. } => {
                assert_eq!(input, "ab", "nothing inserted");
                assert_eq!(cursor, 1, "cursor unchanged");
                assert!(error.is_some() && completions.len() == 1, "no-op leaves state untouched");
            }
            _ => panic!("modal changed variant"),
        }
    }

    #[test]
    fn add_workspace_empty_name_is_rejected() {
        let mut modal = add_workspace_modal();
        // Type only into the branch field, leaving the name blank.
        handle_modal_key(&mut modal, key(KeyCode::Tab));
        handle_modal_key(&mut modal, key(KeyCode::Char('x')));
        assert!(matches!(handle_modal_key(&mut modal, key(KeyCode::Enter)), ModalResult::Consumed));
        assert!(matches!(modal, ModalState::AddWorkspace { error: Some(_), .. }), "stays open with an error");
    }

    fn confirm_branch_checkout_modal() -> ModalState {
        ModalState::ConfirmBranchCheckout {
            repo_id: "r1".into(),
            repo_name: "demo".into(),
            name: "feat".into(),
        }
    }

    #[test]
    fn confirm_branch_checkout_enter_checks_out() {
        let mut modal = confirm_branch_checkout_modal();
        match handle_modal_key(&mut modal, key(KeyCode::Enter)) {
            ModalResult::BranchCheckoutChoice { repo_id, name, checkout } => {
                assert_eq!(repo_id, "r1");
                assert_eq!(name, "feat");
                assert!(checkout, "Enter checks out the existing branch");
            }
            _ => panic!("expected BranchCheckoutChoice"),
        }
        assert!(matches!(modal, ModalState::None), "choice closes the modal");
    }

    #[test]
    fn confirm_branch_checkout_c_checks_out() {
        let mut modal = confirm_branch_checkout_modal();
        match handle_modal_key(&mut modal, key(KeyCode::Char('c'))) {
            ModalResult::BranchCheckoutChoice { checkout, .. } => assert!(checkout, "bare c checks out"),
            _ => panic!("expected BranchCheckoutChoice"),
        }
    }

    #[test]
    fn confirm_branch_checkout_f_forks() {
        let mut modal = confirm_branch_checkout_modal();
        match handle_modal_key(&mut modal, key(KeyCode::Char('f'))) {
            ModalResult::BranchCheckoutChoice { repo_id, name, checkout } => {
                assert_eq!(repo_id, "r1");
                assert_eq!(name, "feat");
                assert!(!checkout, "f forks a new branch");
            }
            _ => panic!("expected BranchCheckoutChoice"),
        }
    }

    #[test]
    fn confirm_branch_checkout_esc_cancels_and_n_is_a_noop() {
        let mut modal = confirm_branch_checkout_modal();
        assert!(matches!(handle_modal_key(&mut modal, key(KeyCode::Esc)), ModalResult::Cancelled), "Esc cancels");
        assert!(matches!(modal, ModalState::None), "Esc closes the modal with no result");

        // In a 3-way choice `n` is ambiguous (the footer offers only Esc), so it's
        // a no-op that leaves the prompt open rather than cancelling.
        let mut modal = confirm_branch_checkout_modal();
        assert!(matches!(handle_modal_key(&mut modal, key(KeyCode::Char('n'))), ModalResult::Consumed), "n is a no-op");
        assert!(matches!(modal, ModalState::ConfirmBranchCheckout { .. }), "n leaves the prompt open");
    }

    #[test]
    fn confirm_branch_checkout_ctrl_c_cancels_not_checks_out() {
        // Ctrl+C must cancel (like every other modal), not fire the bare-c confirm.
        let mut modal = confirm_branch_checkout_modal();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(handle_modal_key(&mut modal, ev), ModalResult::Cancelled));
        assert!(matches!(modal, ModalState::None), "Ctrl+C closes the modal with no result");
    }
}
