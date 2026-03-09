use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What kind of delete is being confirmed.
#[derive(Clone)]
pub(crate) enum DeleteTarget {
    Workspace { name: String },
    Repo { id: String, name: String, workspace_count: usize },
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
        input: String,
        cursor: usize,
        error: Option<String>,
    },
    ConfirmDelete {
        target: DeleteTarget,
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
    /// AddWorkspace submitted with (repo_id, name).
    SubmitWorkspace(String, String),
    /// Delete confirmed.
    ConfirmDelete(DeleteTarget),
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
                    if let Some(idx) = *completion_index {
                        if let Some(path) = completions.get(idx) {
                            *input = path.clone();
                            *cursor = input.len();
                        }
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
            error,
        } => {
            *error = None;

            match key.code {
                KeyCode::Esc => {
                    *modal = ModalState::None;
                    ModalResult::Cancelled
                }
                KeyCode::Enter => {
                    let name = input.trim().to_string();
                    if name.is_empty() {
                        *error = Some("Name cannot be empty".to_string());
                        ModalResult::Consumed
                    } else {
                        let rid = repo_id.clone();
                        let result = ModalResult::SubmitWorkspace(rid, name);
                        *modal = ModalState::None;
                        result
                    }
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
    }
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
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(&prefix) && !name.starts_with('.') {
                        let full = dir.join(&name);
                        let mut display = full.to_string_lossy().to_string();
                        display.push('/');
                        results.push(display);
                    }
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
    } else if let Some(rest) = partial.strip_prefix("~/") {
        Some(std::path::PathBuf::from(home).join(rest))
    } else {
        None
    }
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
pub(crate) fn render_modal(frame: &mut ratatui::Frame, modal: &ModalState) {
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
                .border_style(Style::default().fg(Color::Cyan));
            frame.render_widget(block, area);

            // Label
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "Repository path:",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                inner[0],
            );

            // Input field with cursor
            let display_input = render_input_with_cursor(input, *cursor, inner[1].width as usize);
            frame.render_widget(
                Paragraph::new(display_input)
                    .style(Style::default().fg(Color::White).bg(Color::DarkGray)),
                inner[1],
            );

            // Error
            if let Some(err) = error {
                frame.render_widget(
                    Paragraph::new(err.as_str()).style(Style::default().fg(Color::Red)),
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
                        let selected = completion_index.map_or(false, |idx| idx == i);
                        let style = if selected {
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        };
                        Line::styled(format!("  {}", path), style)
                    })
                    .collect();
                frame.render_widget(Paragraph::new(comp_lines), inner[3]);
            }

            // Footer
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter", Style::default().fg(Color::Cyan)),
                    Span::raw(": submit  "),
                    Span::styled("Tab", Style::default().fg(Color::Cyan)),
                    Span::raw(": complete  "),
                    Span::styled("Esc", Style::default().fg(Color::Cyan)),
                    Span::raw(": cancel"),
                ])),
                inner[4],
            );
        }
        ModalState::AddWorkspace {
            repo_name,
            input,
            cursor,
            error,
            ..
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

            let title = format!(" Add Workspace to {} ", repo_name);
            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan));
            frame.render_widget(block, area);

            // Label
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "Workspace name:",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                inner[0],
            );

            // Input
            let display_input = render_input_with_cursor(input, *cursor, inner[1].width as usize);
            frame.render_widget(
                Paragraph::new(display_input)
                    .style(Style::default().fg(Color::White).bg(Color::DarkGray)),
                inner[1],
            );

            // Error
            if let Some(err) = error {
                frame.render_widget(
                    Paragraph::new(err.as_str()).style(Style::default().fg(Color::Red)),
                    inner[2],
                );
            }

            // Footer
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter", Style::default().fg(Color::Cyan)),
                    Span::raw(": submit  "),
                    Span::styled("Esc", Style::default().fg(Color::Cyan)),
                    Span::raw(": cancel"),
                ])),
                inner[4],
            );
        }
        ModalState::ConfirmDelete { target } => {
            let area = centered_rect(50, 20, frame.area());
            frame.render_widget(Clear, area);

            let (title, message) = match target {
                DeleteTarget::Workspace { name } => (
                    " Delete Workspace ".to_string(),
                    format!("Delete workspace '{}'?", name),
                ),
                DeleteTarget::Repo { name, workspace_count, .. } => {
                    if *workspace_count > 0 {
                        (
                            " Delete Repository ".to_string(),
                            format!(
                                "Delete repo '{}' and its {} workspace(s)?",
                                name, workspace_count
                            ),
                        )
                    } else {
                        (
                            " Delete Repository ".to_string(),
                            format!("Delete repo '{}'?", name),
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
                .border_style(Style::default().fg(Color::Red));
            frame.render_widget(block, area);

            frame.render_widget(
                Paragraph::new(Line::styled(
                    message,
                    Style::default().fg(Color::White),
                )),
                inner[0],
            );

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("y", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                    Span::raw(": delete  "),
                    Span::styled("n/Esc", Style::default().fg(Color::Cyan)),
                    Span::raw(": cancel"),
                ])),
                inner[2],
            );
        }
    }
}

/// Render an input string with a visible cursor.
fn render_input_with_cursor(input: &str, cursor: usize, width: usize) -> Line<'static> {
    if input.is_empty() {
        return Line::from(vec![
            Span::styled("█", Style::default().fg(Color::Cyan)),
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
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ));
    } else {
        spans.push(Span::styled("█", Style::default().fg(Color::Cyan)));
    }
    spans.push(Span::raw(after.to_string()));

    Line::from(spans)
}
