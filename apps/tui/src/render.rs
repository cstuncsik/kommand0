use kommand0_core::{SessionStatus, workspace::format_timestamp};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
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
        let inner_width = output_area.width.saturating_sub(2) as usize;

        // Build styled lines, compute visual total, clamp scroll, then render
        let mut all_lines: Vec<&str> = app.scrollbacks.get(&ws.id)
            .map(|buf| buf.all_lines())
            .unwrap_or_default();
        // Append in-progress streaming line if present
        let streaming_partial = app.streaming_text.get(&ws.id);
        if let Some(partial) = streaming_partial {
            if !partial.is_empty() {
                all_lines.push(partial.as_str());
            }
        }
        let spinner = if app.waiting_response.contains(&ws.id) {
            Some(app.spinner_tick)
        } else {
            None
        };
        let styled_lines = build_output_lines(&all_lines, inner_width, &session_status, spinner);
        let total_visual = styled_total_visual(&styled_lines, inner_width);
        let max_offset = total_visual.saturating_sub(inner_height);
        if let Some(buf) = app.scrollbacks.get_mut(&ws.id) {
            buf.clamp_scroll_offset(max_offset);
        }
        let scroll_offset = app.scrollbacks.get(&ws.id)
            .map(|buf| buf.scroll_offset()).unwrap_or(0);

        render_output_content(frame, output_area, styled_lines, scroll_offset, inner_width, inner_height, app.focus, right_title);

        // Scrollbar (use visual line counts)
        if app.scrollbacks.contains_key(&ws.id) {
            render_scrollbar(frame, output_area, total_visual, inner_height, scroll_offset);
        }

        // New lines indicator when scrolled up
        if let Some(buf) = app.scrollbacks.get(&ws.id) {
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

/// Calculate visual line count for a single line with wrapping.
fn wrapped_line_height(line_len: usize, width: usize) -> usize {
    if width == 0 || line_len == 0 {
        return 1;
    }
    (line_len + width - 1) / width
}

/// Parse inline markdown into styled spans.
/// Handles: **bold**, *italic*, `inline code`, and plain text.
fn parse_inline_markdown(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut plain_start = 0;

    while let Some(&(i, ch)) = chars.peek() {
        match ch {
            '`' => {
                // Inline code
                if i > plain_start {
                    spans.push(Span::styled(text[plain_start..i].to_string(), base_style));
                }
                chars.next();
                let code_start = i + 1;
                let mut found_end = false;
                while let Some(&(j, c)) = chars.peek() {
                    if c == '`' {
                        spans.push(Span::styled(
                            text[code_start..j].to_string(),
                            Style::default().fg(Color::Yellow),
                        ));
                        chars.next();
                        plain_start = j + 1;
                        found_end = true;
                        break;
                    }
                    chars.next();
                }
                if !found_end {
                    // No closing backtick, treat as plain
                    spans.push(Span::styled(text[i..].to_string(), base_style));
                    return spans;
                }
            }
            '*' => {
                if i > plain_start {
                    spans.push(Span::styled(text[plain_start..i].to_string(), base_style));
                }
                chars.next();
                // Check for ** (bold) vs * (italic)
                if chars.peek().map(|&(_, c)| c) == Some('*') {
                    // Bold **...**
                    chars.next();
                    let bold_start = i + 2;
                    let mut found_end = false;
                    while let Some(&(j, c)) = chars.peek() {
                        if c == '*' {
                            chars.next();
                            if chars.peek().map(|&(_, c2)| c2) == Some('*') {
                                chars.next();
                                spans.push(Span::styled(
                                    text[bold_start..j].to_string(),
                                    base_style.add_modifier(Modifier::BOLD),
                                ));
                                plain_start = j + 2;
                                found_end = true;
                                break;
                            }
                        } else {
                            chars.next();
                        }
                    }
                    if !found_end {
                        spans.push(Span::styled(text[i..].to_string(), base_style));
                        return spans;
                    }
                } else {
                    // Italic *...*
                    let italic_start = i + 1;
                    let mut found_end = false;
                    while let Some(&(j, c)) = chars.peek() {
                        if c == '*' {
                            spans.push(Span::styled(
                                text[italic_start..j].to_string(),
                                base_style.add_modifier(Modifier::ITALIC),
                            ));
                            chars.next();
                            plain_start = j + 1;
                            found_end = true;
                            break;
                        }
                        chars.next();
                    }
                    if !found_end {
                        spans.push(Span::styled(text[i..].to_string(), base_style));
                        return spans;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }

    // Remaining plain text
    if plain_start < text.len() {
        spans.push(Span::styled(text[plain_start..].to_string(), base_style));
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }

    spans
}

/// Style a single line with markdown awareness.
/// `in_code_block` tracks fenced code block state across lines.
fn style_markdown_line(line: &str, inner_width: usize, in_code_block: &mut bool) -> Line<'static> {
    let code_style = Style::default().fg(Color::Green);
    let code_block_style = Style::default().fg(Color::Green);

    // Fenced code block toggle
    if line.starts_with("```") {
        *in_code_block = !*in_code_block;
        if *in_code_block {
            // Opening fence — show language hint if present
            let lang = line[3..].trim();
            if lang.is_empty() {
                return Line::styled("───", Style::default().fg(Color::DarkGray));
            } else {
                return Line::from(vec![
                    Span::styled("─── ", Style::default().fg(Color::DarkGray)),
                    Span::styled(lang.to_string(), Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)),
                    Span::styled(" ───", Style::default().fg(Color::DarkGray)),
                ]);
            }
        } else {
            // Closing fence
            return Line::styled("───", Style::default().fg(Color::DarkGray));
        }
    }

    // Inside code block — render as code
    if *in_code_block {
        return Line::styled(format!("  {}", line), code_block_style);
    }

    // User message (chat bubble)
    if line.starts_with("> ") {
        let content = &line[2..];
        let content_len = content.len();
        let avail = inner_width.saturating_sub(1);
        if content_len <= avail {
            let padding = avail - content_len;
            return Line::from(vec![
                Span::raw(" ".repeat(padding)),
                Span::styled(content.to_string(), Style::default().bg(Color::DarkGray)),
            ]);
        } else {
            return Line::styled(content.to_string(), Style::default().bg(Color::DarkGray));
        }
    }

    // Separator
    if line == "---" {
        return Line::styled("───", Style::default().fg(Color::DarkGray));
    }

    // Headers
    if line.starts_with("### ") {
        let text = &line[4..];
        return Line::from(parse_inline_markdown(text, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
    }
    if line.starts_with("## ") {
        let text = &line[3..];
        return Line::from(parse_inline_markdown(text, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
    }
    if line.starts_with("# ") {
        let text = &line[2..];
        return Line::from(parse_inline_markdown(text, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
    }

    // Bullet lists — keep bullet, parse rest as inline markdown
    if line.starts_with("- ") || line.starts_with("* ") {
        let mut spans = vec![Span::styled(
            line[..2].to_string(),
            Style::default().fg(Color::Cyan),
        )];
        spans.extend(parse_inline_markdown(&line[2..], Style::default()));
        return Line::from(spans);
    }
    // Indented bullets
    if line.starts_with("  - ") || line.starts_with("  * ") {
        let mut spans = vec![Span::styled(
            line[..4].to_string(),
            Style::default().fg(Color::Cyan),
        )];
        spans.extend(parse_inline_markdown(&line[4..], Style::default()));
        return Line::from(spans);
    }

    // Numbered lists
    if let Some(rest) = line.strip_prefix(|c: char| c.is_ascii_digit()) {
        if let Some(rest) = rest.strip_prefix(". ") {
            let prefix_len = line.len() - rest.len();
            let mut spans = vec![Span::styled(
                line[..prefix_len].to_string(),
                Style::default().fg(Color::Cyan),
            )];
            spans.extend(parse_inline_markdown(rest, Style::default()));
            return Line::from(spans);
        }
    }

    // Default: parse inline markdown
    let _ = code_style; // suppress warning
    Line::from(parse_inline_markdown(line, Style::default()))
}

/// Build styled Line items from raw scrollback lines (owned, no lifetime ties).
fn build_output_lines(
    raw_lines: &[&str],
    inner_width: usize,
    session_status: &SessionStatus,
    spinner: Option<u8>,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = if raw_lines.is_empty() {
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
        let mut in_code_block = false;
        raw_lines.iter().map(|l| {
            style_markdown_line(l, inner_width, &mut in_code_block)
        }).collect()
    };

    if let Some(tick) = spinner {
        let frame_char = SPINNER_FRAMES[tick as usize % SPINNER_FRAMES.len()];
        lines.push(Line::styled(
            format!(" {} Thinking...", frame_char),
            Style::default().fg(Color::Cyan),
        ));
    }

    lines
}

/// Calculate total visual (wrapped) line count from styled Lines.
fn styled_total_visual(lines: &[Line], inner_width: usize) -> usize {
    lines.iter()
        .map(|l| {
            let len: usize = l.spans.iter().map(|s| s.content.len()).sum();
            wrapped_line_height(len, inner_width)
        })
        .sum()
}

fn render_output_content(
    frame: &mut ratatui::Frame,
    output_area: Rect,
    lines: Vec<Line<'static>>,
    scroll_offset: usize,
    inner_width: usize,
    inner_height: usize,
    focus: Focus,
    title: Line<'_>,
) {
    let total_visual = styled_total_visual(&lines, inner_width);

    // scroll_offset = visual lines from the bottom (0 = at bottom)
    // Paragraph::scroll takes lines from the top
    let max_scroll = total_visual.saturating_sub(inner_height);
    let clamped_offset = scroll_offset.min(max_scroll);
    let scroll_from_top = max_scroll.saturating_sub(clamped_offset);

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
        .wrap(Wrap { trim: false })
        .scroll((scroll_from_top as u16, 0));
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
    let inner_width = output_area.width.saturating_sub(2) as usize;

    // Build styled lines, compute visual total, clamp scroll, then render
    let mut all_lines: Vec<&str> = app.scrollbacks.get(&ws.id)
        .map(|buf| buf.all_lines())
        .unwrap_or_default();
    // Append in-progress streaming line if present
    let streaming_partial = app.streaming_text.get(&ws.id);
    if let Some(partial) = streaming_partial {
        if !partial.is_empty() {
            all_lines.push(partial.as_str());
        }
    }
    let spinner = if app.waiting_response.contains(&ws.id) {
        Some(app.spinner_tick)
    } else {
        None
    };
    let styled_lines = build_output_lines(&all_lines, inner_width, &session_status, spinner);
    let total_visual = styled_total_visual(&styled_lines, inner_width);
    let max_offset = total_visual.saturating_sub(inner_height);
    if let Some(buf) = app.scrollbacks.get_mut(&ws.id) {
        buf.clamp_scroll_offset(max_offset);
    }
    let scroll_offset = app.scrollbacks.get(&ws.id)
        .map(|buf| buf.scroll_offset()).unwrap_or(0);

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

    render_output_content(frame, output_area, styled_lines, scroll_offset, inner_width, inner_height, app.focus, output_title);

    // Scrollbar (use visual line counts)
    if app.scrollbacks.contains_key(&ws.id) {
        render_scrollbar(frame, output_area, total_visual, inner_height, scroll_offset);
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

    let scroll_info = if app.scrollbacks.contains_key(&ws.id) {
        let current_line = total_visual.saturating_sub(scroll_offset);
        format!("line {}/{}", current_line, total_visual)
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

/// Truncate a string to fit `max_width` display columns, keeping the start.
/// Does not add ellipsis -- the caller handles that.
pub(crate) fn truncate_to_width(s: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    let mut width = 0;
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max_width {
            break;
        }
        width += cw;
        end = i + ch.len_utf8();
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_path_fits() {
        assert_eq!(truncate_path("hello", 10), "hello");
    }

    #[test]
    fn truncate_path_truncated() {
        assert_eq!(truncate_path("hello/world/path", 10), "...ld/path");
    }

    #[test]
    fn truncate_path_exact_fit() {
        assert_eq!(truncate_path("ab", 2), "ab");
    }

    #[test]
    fn truncate_path_max_width_less_than_4() {
        assert_eq!(truncate_path("abcdef", 3), "...");
    }

    #[test]
    fn truncate_path_cjk_no_panic() {
        // \u{4F60} = 你 (width 2), \u{597D} = 好 (width 2), total "你好abc" = 2+2+1+1+1 = 7
        let result = truncate_path("\u{4F60}\u{597D}abc", 5);
        // Should not panic. Result should fit in 5 display columns.
        assert!(UnicodeWidthStr::width(result.as_str()) <= 5);
    }

    #[test]
    fn truncate_path_cjk_only() {
        // "你好世界" = 4 chars, 12 bytes, 8 display width
        // With max_width=6, should truncate by display width, not byte count
        let result = truncate_path("\u{4F60}\u{597D}\u{4E16}\u{754C}", 6);
        // Should show "...界" (3 + 2 = 5 display width) -- fits in 6
        assert!(UnicodeWidthStr::width(result.as_str()) <= 6);
        assert!(result.starts_with("..."));
    }

    #[test]
    fn truncate_path_mixed_cjk_ascii() {
        // "a你b好c" = 5 chars, 1+3+1+3+1=9 bytes, 1+2+1+2+1=7 display width
        // With max_width=5: should use display width (7 > 5), not byte len (9 > 5)
        // keep = 5 - 3 = 2 display cols from tail -> "c" only (width 1) or "好c" won't fit (width 3)
        let result = truncate_path("a\u{4F60}b\u{597D}c", 5);
        assert!(UnicodeWidthStr::width(result.as_str()) <= 5);
        assert!(result.starts_with("..."));
    }

    #[test]
    fn truncate_to_width_fits() {
        assert_eq!(truncate_to_width("hi", 10), "hi");
    }

    #[test]
    fn truncate_to_width_truncates() {
        assert_eq!(truncate_to_width("hello world", 5), "hello");
    }

    #[test]
    fn truncate_to_width_cjk_no_partial() {
        // Each CJK char is width 2. "你好x" = 2+2+1 = 5 width
        // With max 3, should get "你" (width 2), not "你好" (width 4)
        let result = truncate_to_width("\u{4F60}\u{597D}x", 3);
        assert_eq!(result, "\u{4F60}");
        assert!(UnicodeWidthStr::width(result.as_str()) <= 3);
    }
}
