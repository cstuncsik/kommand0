use kommand0_core::{SessionStatus, workspace::format_timestamp};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use super::{App, Focus, TreeNode, buttons, help, modal};

const SPINNER_FRAMES: &[&str] = &["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];

fn truncate_path(path: &str, max_width: usize) -> String {
    if path.len() <= max_width {
        return path.to_string();
    }
    if max_width < 4 {
        return "...".to_string();
    }
    let keep = max_width - 3;
    format!("...{}", &path[path.len() - keep..])
}

pub fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    app.hit_regions.clear();

    if app.zoomed {
        render_zoomed(frame, app);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(frame.area());

        app.pane_areas.tree = chunks[0];

        // Left pane: tree view
        render_tree(frame, app, chunks[0]);

        // Right pane: context-sensitive details or session view
        render_right_pane(frame, app, chunks[1]);
    }

    // Help overlay on top of any layout
    if app.show_help {
        help::render_help_overlay(frame, app.focus);
    }

    // Modal overlay on top of everything
    if app.modal.is_active() {
        modal::render_modal(frame, &app.modal);
    }
}

fn render_tree(frame: &mut ratatui::Frame, app: &mut App, area: Rect) {
    if app.tree_items.is_empty() {
        let border_style = if app.focus == Focus::Tree {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let hint = Paragraph::new("No repos tracked. Run: kmd repo add <path>")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title(" Repos ").borders(Borders::ALL).border_style(border_style));
        frame.render_widget(hint, area);
    } else {
        let items: Vec<ListItem> = app
            .tree_items
            .iter()
            .enumerate()
            .map(|(i, node)| {
                let is_selected = i == app.selected_index;
                match node {
                    TreeNode::Repo { id, name, .. } => {
                        let expanded = app.expanded.contains(id);
                        let indicator = if expanded { "\u{25BC} " } else { "\u{25B6} " };
                        let text = format!("{}{}", indicator, name);
                        let style = if is_selected && app.focus == Focus::Tree {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else if is_selected {
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };
                        ListItem::new(Line::styled(text, style))
                    }
                    TreeNode::Workspace { ws, .. } => {
                        let (dot, dot_color) = if ws.active {
                            ("\u{25CF}", Color::Green)
                        } else {
                            ("\u{25CB}", Color::DarkGray)
                        };
                        let prefix = "  \u{251C}\u{2500} ";
                        let style = if is_selected && app.focus == Focus::Tree {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else if is_selected {
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD)
                        } else if !ws.active {
                            Style::default().fg(Color::DarkGray)
                        } else {
                            Style::default()
                        };
                        let mut spans = vec![
                            Span::styled(prefix, style),
                            Span::styled(dot, Style::default().fg(dot_color)),
                            Span::styled(format!(" {}", ws.name), style),
                        ];
                        // Session status icon
                        if let Some((icon, color)) = app.session_status_icon(&ws.id) {
                            spans.push(Span::styled(icon, Style::default().fg(color)));
                        }
                        ListItem::new(Line::from(spans))
                    }
                    TreeNode::Hint { text } => {
                        let display = format!("     {}", text);
                        let style = Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC);
                        ListItem::new(Line::styled(display, style))
                    }
                }
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(app.selected_index));

        let tree_border_style = if app.focus == Focus::Tree {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let list = List::new(items)
            .block(Block::default().title(" Repos ").borders(Borders::ALL).border_style(tree_border_style))
            .highlight_style(Style::default());

        frame.render_stateful_widget(list, area, &mut list_state);
    }
}

fn render_right_pane(frame: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let right_width = area.width.saturating_sub(4) as usize;

    // Check if selected workspace has an active session (running, or stopped/exited with scrollback)
    let session_info = match app.tree_items.get(app.selected_index) {
        Some(TreeNode::Workspace { ws, .. }) => {
            app.state
                .find_session_by_workspace(&ws.id)
                .filter(|s| {
                    s.status == SessionStatus::Running
                        || app.scrollbacks.get(&ws.id).map_or(false, |b| !b.is_empty())
                })
                .map(|s| (ws.clone(), s.id.clone(), s.status.clone()))
        }
        _ => None,
    };

    if let Some((ws, _session_id, session_status)) = session_info {
        // Session view: output + composer
        let (status_str, status_color) = match session_status {
            SessionStatus::Running => ("\u{25B6}", Color::Green),
            SessionStatus::Stopped => ("\u{25A0}", Color::Yellow),
            SessionStatus::Failed => ("\u{2717}", Color::Red),
            SessionStatus::Exited => ("\u{2717}", Color::DarkGray),
        };
        let right_title = Line::from(vec![
            Span::raw(format!(" Workspace: {} ", ws.name)),
            Span::styled(status_str, Style::default().fg(status_color)),
            Span::raw(" "),
        ]);

        let composer_height = app.composer.height_hint();

        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(composer_height),
            ])
            .split(area);

        // Output area
        let output_area = right_chunks[0];
        app.pane_areas.output = output_area;
        app.last_output_height = output_area.height;
        let inner_height = output_area.height.saturating_sub(2) as usize; // account for borders

        let scrollback = app.scrollbacks.get(&ws.id);
        let visible: Vec<&str> = scrollback
            .map(|buf| buf.visible_lines(inner_height))
            .unwrap_or_default();

        let spinner = if app.waiting_response.contains(&ws.id) {
            Some(app.spinner_tick)
        } else {
            None
        };
        render_output_content(frame, output_area, &visible, &session_status, app.focus, right_title, spinner);

        // Scrollbar
        if let Some(buf) = scrollback {
            render_scrollbar(frame, output_area, buf.total_lines(), inner_height, buf.clamped_offset(inner_height));
        }

        // New lines indicator when scrolled up
        if let Some(buf) = scrollback {
            if !buf.is_at_bottom() {
                let new_count = buf.new_lines_count();
                if new_count > 0 {
                    let indicator = format!(" \u{2193} {} new lines ", new_count);
                    let indicator_width = indicator.len() as u16;
                    let indicator_area = Rect::new(
                        output_area.x + output_area.width.saturating_sub(indicator_width + 2),
                        output_area.y + output_area.height.saturating_sub(1),
                        indicator_width,
                        1,
                    );
                    let indicator_widget = Paragraph::new(indicator)
                        .style(Style::default().fg(Color::Cyan));
                    frame.render_widget(indicator_widget, indicator_area);
                }
            }
        }

        // Composer area
        let composer_area = right_chunks[1];
        app.pane_areas.composer = composer_area;
        if session_status == SessionStatus::Running {
            frame.render_widget(app.composer.widget(), composer_area);
            // Char/line count overlay in bottom-right corner
            let status = app.composer.status_text();
            let status_width = status.len() as u16 + 1;
            if composer_area.width > status_width + 2 && composer_area.height > 0 {
                let status_area = Rect::new(
                    composer_area.x + composer_area.width.saturating_sub(status_width + 1),
                    composer_area.y + composer_area.height.saturating_sub(1),
                    status_width,
                    1,
                );
                frame.render_widget(
                    Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
                    status_area,
                );
            }
        } else {
            // Show resume button
            let btn_label = "Resume";
            let btn_x = composer_area.x + 2;
            let btn_y = composer_area.y + 1;
            let btn_rect = Rect::new(btn_x, btn_y, (btn_label.len() + 2) as u16, 1);
            let hovered = buttons::is_hovered(app.mouse_pos, btn_rect);
            let btn_style = if hovered {
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            };
            let hint = Paragraph::new(Line::styled(format!("[{}]", btn_label), btn_style))
                .block(Block::default().title(" Composer ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
            frame.render_widget(hint, composer_area);
            app.hit_regions.push(buttons::HitRegion {
                area: btn_rect,
                action: buttons::HitAction::ResumeSession,
            });
        }
    } else {
        // No session: show workspace/repo details (original behavior)
        let (right_title, right_content) = match app.tree_items.get(app.selected_index) {
            Some(TreeNode::Repo { id, name, .. }) => {
                let repo_path = app
                    .repos
                    .iter()
                    .find(|r| r.id == *id)
                    .map(|r| r.path.as_str())
                    .unwrap_or("unknown");
                let total: usize = app.workspaces.iter().filter(|w| w.repo_id == *id).count();
                let active: usize = app
                    .workspaces
                    .iter()
                    .filter(|w| w.repo_id == *id && w.active)
                    .count();

                let title = format!(" Repo: {} ", name);
                let lines = vec![
                    Line::from(vec![
                        Span::styled(
                            "Name: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(name.as_str()),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Path: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(truncate_path(repo_path, right_width)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Workspaces: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(format!("{} active, {} total", active, total)),
                    ]),
                ];
                (title, lines)
            }
            Some(TreeNode::Workspace { ws, repo_name }) => {
                let title = format!(" Workspace: {} ", ws.name);
                let status_span = if ws.active {
                    Span::styled("active", Style::default().fg(Color::Green))
                } else {
                    Span::styled("archived", Style::default().fg(Color::DarkGray))
                };
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled(
                            "Name: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(ws.name.as_str()),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Repo: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(repo_name.as_str()),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Dir: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(truncate_path(&ws.working_dir, right_width)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Status: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        status_span,
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Created: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(format_timestamp(ws.created_at)),
                    ]),
                ];
                // Button for starting session
                lines.push(Line::raw(""));
                {
                    // Button area: inside the right pane, on the line after the details
                    // 6 detail lines + 1 empty + 1 border = line offset 8 from area.y
                    let btn_y = area.y + 8;
                    let btn_x = area.x + 2; // inside border + 1 padding
                    let btn_label = "Start Session";
                    let btn_rect = Rect::new(btn_x, btn_y, (btn_label.len() + 2) as u16, 1);
                    let hovered = buttons::is_hovered(app.mouse_pos, btn_rect);
                    let style = if hovered {
                        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                    };
                    lines.push(Line::styled(format!("[{}]", btn_label), style));
                    app.hit_regions.push(buttons::HitRegion {
                        area: btn_rect,
                        action: buttons::HitAction::StartSession,
                    });
                }
                (title, lines)
            }
            Some(TreeNode::Hint { .. }) | None => {
                let title = " Details ".to_string();
                let lines = vec![Line::styled(
                    "Select a workspace to see details",
                    Style::default().fg(Color::DarkGray),
                )];
                (title, lines)
            }
        };

        let right_border = if app.focus == Focus::Output {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let paragraph = Paragraph::new(right_content)
            .block(Block::default().title(right_title).borders(Borders::ALL).border_style(right_border));
        frame.render_widget(paragraph, area);
    }
}

fn render_output_content(
    frame: &mut ratatui::Frame,
    output_area: Rect,
    visible: &[&str],
    session_status: &SessionStatus,
    focus: Focus,
    title: Line<'_>,
    spinner: Option<u8>,
) {
    let inner_width = output_area.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = if visible.is_empty() {
        if *session_status == SessionStatus::Running {
            vec![Line::styled(
                "Session started. Waiting for output...",
                Style::default().fg(Color::DarkGray),
            )]
        } else {
            vec![Line::styled(
                "No output.",
                Style::default().fg(Color::DarkGray),
            )]
        }
    } else {
        visible.iter().map(|l| {
            if l.starts_with("> ") {
                let content = &l[2..];
                let content_len = content.len();
                let avail = inner_width.saturating_sub(1);
                if content_len <= avail {
                    let padding = avail - content_len;
                    Line::from(vec![
                        Span::raw(" ".repeat(padding)),
                        Span::styled(content, Style::default().bg(Color::DarkGray)),
                    ])
                } else {
                    Line::styled(content.to_string(), Style::default().bg(Color::DarkGray))
                }
            } else if *l == "---" {
                Line::styled("---", Style::default().fg(Color::DarkGray))
            } else {
                Line::raw(*l)
            }
        }).collect()
    };

    // Append spinner if waiting for response
    if let Some(tick) = spinner {
        let frame_char = SPINNER_FRAMES[tick as usize % SPINNER_FRAMES.len()];
        lines.push(Line::styled(
            format!(" {} Thinking...", frame_char),
            Style::default().fg(Color::Cyan),
        ));
    }

    let output_border_style = if focus == Focus::Output {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let output_block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(output_border_style);
    let paragraph = Paragraph::new(lines)
        .block(output_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, output_area);
}

fn render_zoomed(frame: &mut ratatui::Frame, app: &mut App) {
    let ws_info = app.selected_workspace().cloned().and_then(|ws| {
        app.state
            .find_session_by_workspace(&ws.id)
            .map(|s| (ws, s.id.clone(), s.status.clone()))
    });

    let Some((ws, _session_id, session_status)) = ws_info else {
        // No workspace selected; exit zoom
        app.zoomed = false;
        return;
    };

    let composer_height = if session_status == SessionStatus::Running {
        app.composer.height_hint()
    } else {
        3
    };

    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .split(frame.area());

    // Output area
    let output_area = chunks[0];
    app.pane_areas.output = output_area;
    app.pane_areas.tree = Rect::default(); // No tree in zoom mode
    app.last_output_height = output_area.height;
    let inner_height = output_area.height.saturating_sub(2) as usize;

    let scrollback = app.scrollbacks.get(&ws.id);
    let visible: Vec<&str> = scrollback
        .map(|buf| buf.visible_lines(inner_height))
        .unwrap_or_default();

    let (status_str, status_color) = match session_status {
        SessionStatus::Running => ("\u{25B6}", Color::Green),
        SessionStatus::Stopped => ("\u{25A0}", Color::Yellow),
        SessionStatus::Failed => ("\u{2717}", Color::Red),
        SessionStatus::Exited => ("\u{2717}", Color::DarkGray),
    };
    let output_title = Line::from(vec![
        Span::raw(format!(" {} ", ws.name)),
        Span::styled(status_str, Style::default().fg(status_color)),
        Span::raw(" "),
    ]);

    let spinner = if app.waiting_response.contains(&ws.id) {
        Some(app.spinner_tick)
    } else {
        None
    };
    render_output_content(frame, output_area, &visible, &session_status, app.focus, output_title, spinner);

    // Scrollbar
    if let Some(buf) = scrollback {
        render_scrollbar(frame, output_area, buf.total_lines(), inner_height, buf.clamped_offset(inner_height));
    }

    // Composer area
    let composer_area = chunks[1];
    app.pane_areas.composer = composer_area;
    if session_status == SessionStatus::Running {
        frame.render_widget(app.composer.widget(), composer_area);
        // Char/line count overlay
        let status = app.composer.status_text();
        let status_width = status.len() as u16 + 1;
        if composer_area.width > status_width + 2 && composer_area.height > 0 {
            let status_area = Rect::new(
                composer_area.x + composer_area.width.saturating_sub(status_width + 1),
                composer_area.y + composer_area.height.saturating_sub(1),
                status_width,
                1,
            );
            frame.render_widget(
                Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
                status_area,
            );
        }
    } else {
        let hint = Paragraph::new("Press 'R' to resume session")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title(" Composer ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(hint, composer_area);
    }

    // Status bar (bottom line)
    let status_bar_area = chunks[2];
    let (bar_icon, bar_color) = match session_status {
        SessionStatus::Running => ("\u{25B6}", Color::Green),
        SessionStatus::Stopped => ("\u{25A0}", Color::Yellow),
        SessionStatus::Failed => ("\u{2717}", Color::Red),
        SessionStatus::Exited => ("\u{2717}", Color::DarkGray),
    };

    let scroll_info = if let Some(buf) = app.scrollbacks.get(&ws.id) {
        let total = buf.total_lines();
        let offset = buf.clamped_offset(inner_height);
        let current_line = total.saturating_sub(offset);
        format!("line {}/{}", current_line, total)
    } else {
        String::new()
    };

    let status_line = Line::from(vec![
        Span::styled(format!(" {} ", ws.name), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {} ", bar_icon), Style::default().fg(bar_color)),
        Span::raw(" "),
        Span::styled(scroll_info, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled("[z] exit zoom", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(
        Paragraph::new(status_line).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        status_bar_area,
    );
}

fn render_scrollbar(frame: &mut ratatui::Frame, area: Rect, total_lines: usize, viewport_height: usize, offset: usize) {
    if total_lines <= viewport_height || area.height < 3 {
        return;
    }
    let track_height = area.height.saturating_sub(2) as usize;
    if track_height == 0 {
        return;
    }
    let thumb_size = ((viewport_height as f64 / total_lines as f64) * track_height as f64).max(1.0) as usize;
    let max_offset = total_lines.saturating_sub(viewport_height);
    let thumb_pos = if max_offset == 0 {
        0
    } else {
        ((max_offset.saturating_sub(offset)) as f64 / max_offset as f64 * (track_height.saturating_sub(thumb_size)) as f64) as usize
    };
    for i in 0..track_height {
        let ch = if i >= thumb_pos && i < thumb_pos + thumb_size { "\u{2588}" } else { "\u{2502}" };
        let y = area.y + 1 + i as u16;
        let x = area.x + area.width.saturating_sub(2); // inside right border
        frame.render_widget(
            Paragraph::new(ch).style(Style::default().fg(Color::DarkGray)),
            Rect::new(x, y, 1, 1),
        );
    }
}
