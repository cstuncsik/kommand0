use kommand0_core::{SessionStatus, workspace::format_timestamp};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use std::time::Instant;

use super::{App, Focus, TreeNode, buttons, help, modal};
use super::composer::Composer;
use super::buttons::HitAction;
use super::selection::SelectionState;

const SPINNER_FRAMES: &[&str] = &["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];

fn truncate_path(path: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(path) <= max_width {
        return path.to_string();
    }
    if max_width < 4 {
        return "...".to_string();
    }
    let keep = max_width - 3; // display columns available for the tail
    // Walk from the end, accumulating display width to find the tail substring
    let mut tail_width = 0;
    let mut start_byte = path.len();
    for ch in path.chars().rev() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if tail_width + cw > keep {
            break;
        }
        tail_width += cw;
        start_byte -= ch.len_utf8();
    }
    format!("...{}", &path[start_byte..])
}

/// Render the slash-command completion popup anchored above the composer.
///
/// Shows up to 8 rows, windowed around the selection so longer lists scroll,
/// with a count in the title. Drawn over (Clear) whatever sits above the
/// composer (normally the output pane).
fn render_slash_popup(frame: &mut ratatui::Frame, composer_area: Rect, composer: &Composer) {
    let matches = composer.slash_matches();
    if matches.is_empty() {
        return;
    }
    // Need room above the composer for a bordered list, and a usable width.
    let avail_above = composer_area.y;
    if avail_above < 3 || composer_area.width < 4 {
        return;
    }
    let max_rows = 8usize;
    let visible = matches.len().min(max_rows);
    // Never overlap the composer: cap height to the room above it.
    let height = ((visible as u16) + 2).min(avail_above);
    let visible = (height.saturating_sub(2)) as usize;

    let longest = matches.iter().map(|m| m.len() + 1).max().unwrap_or(0); // +1 for '/'
    // Order-safe: cap to the pane width, raise to a 20-col minimum only when the
    // pane can afford it (clamp() would panic if min > max on a narrow pane).
    let width = ((longest as u16) + 4)
        .max(20)
        .min(composer_area.width);
    let y = composer_area.y.saturating_sub(height);
    let area = Rect::new(composer_area.x, y, width, height);

    // Window the rows so the selected entry stays visible.
    let selected = composer.slash_selected();
    let start = if selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let items: Vec<ListItem> = matches
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, name)| {
            let style = if i == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::styled(format!(" /{name}"), style))
        })
        .collect();

    let title = format!(" /commands ({}) ", matches.len());
    let list = List::new(items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(list, area);
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
        help::render_help_overlay(frame, app.focus, &mut app.help_scroll);
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
    }

    if !app.tree_items.is_empty() {
        let pane_inner_width = area.width.saturating_sub(2) as usize; // subtract left+right borders

        // Reset expanded icon rows when pane width changes
        if pane_inner_width as u16 != app.last_pane_width {
            app.expanded_icon_rows.clear();
            app.last_pane_width = pane_inner_width as u16;
        }

        // Two-phase approach: collect items + icon data, then register hit regions after render
        let mut workspace_icons: Vec<(usize, IconCluster)> = Vec::new();
        let mut repo_icons: Vec<(usize, IconCluster)> = Vec::new();

        let items: Vec<ListItem> = app
            .tree_items
            .iter()
            .enumerate()
            .map(|(i, node)| {
                let is_selected = i == app.selected_index;
                match node {
                    TreeNode::Repo { id, name, .. } => {
                        let expanded = app.expanded.contains(id);
                        let indicator = if expanded { "\u{25BE} " } else { "\u{203A} " }; // ▾ / ›
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

                        // Build repo line icons: ✕ (delete) + (add workspace)
                        let icons = repo_line_icons(id, name, pane_inner_width);

                        let prefix = format!("{indicator}{name}");
                        let prefix_width = UnicodeWidthStr::width(prefix.as_str());
                        let fill_width = pane_inner_width.saturating_sub(prefix_width + icons.total_width as usize);

                        let mut spans = vec![
                            Span::styled(prefix, style),
                            Span::raw(" ".repeat(fill_width)),
                        ];
                        spans.extend(icons.spans.clone());

                        repo_icons.push((i, icons));

                        ListItem::new(Line::from(spans))
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

                        // Build icon cluster from session state
                        let session = app.state.find_session_by_workspace(&ws.id);
                        let is_thinking = app.waiting_response.contains(&ws.id);
                        let is_expanded_narrow = app.expanded_icon_rows.contains(&ws.id);
                        let icons = workspace_icon_cluster(
                            session,
                            &ws.id,
                            is_thinking,
                            app.spinner_tick,
                            pane_inner_width,
                            is_expanded_narrow,
                        );

                        // Fill-span layout: prefix + dot + space + name + fill + icons
                        let prefix_width = UnicodeWidthStr::width(prefix);
                        let dot_width: usize = 1;
                        let space_after_dot: usize = 1;
                        let fixed_width = prefix_width + dot_width + space_after_dot;
                        let name_max_width = pane_inner_width.saturating_sub(fixed_width + icons.total_width as usize);
                        let display_name = truncate_to_width(&ws.name, name_max_width);
                        let name_display_width = UnicodeWidthStr::width(display_name.as_str());
                        let fill_width = pane_inner_width.saturating_sub(fixed_width + name_display_width + icons.total_width as usize);

                        let mut spans = vec![
                            Span::styled(prefix, style),
                            Span::styled(dot, Style::default().fg(dot_color)),
                            Span::styled(format!(" {display_name}"), style),
                            Span::raw(" ".repeat(fill_width)),
                        ];
                        spans.extend(icons.spans.clone());

                        // Collect icon data for hit region registration after render
                        workspace_icons.push((i, icons));

                        ListItem::new(Line::from(spans))
                    }
                    TreeNode::Hint { text } => {
                        let display = format!("     {text}");
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

        // Register hit regions using scroll offset from rendered list state
        let scroll_offset = list_state.offset();
        let all_icons: Vec<&(usize, IconCluster)> = workspace_icons.iter().chain(repo_icons.iter()).collect();
        for (item_idx, icons) in all_icons {
            if let Some(row_in_viewport) = item_idx.checked_sub(scroll_offset) {
                let y = area.y + 1 + row_in_viewport as u16; // +1 for top border
                if y < area.y + area.height - 1 { // within viewport (before bottom border)
                    let mut icon_x = area.x + 1 + pane_inner_width as u16 - icons.total_width;
                    for (action, icon_width) in &icons.hit_regions {
                        app.hit_regions.push(buttons::HitRegion {
                            area: Rect::new(icon_x, y, *icon_width, 1),
                            action: action.clone(),
                        });
                        icon_x += icon_width;
                    }
                }
            }
        }

        // Render hover overlays (white color) for hovered icons
        let mouse_pos = app.mouse_pos;
        for (item_idx, icons) in &workspace_icons {
            if let Some(row_in_viewport) = item_idx.checked_sub(scroll_offset) {
                let y = area.y + 1 + row_in_viewport as u16;
                if y >= area.y + area.height - 1 {
                    continue;
                }
                let mut icon_x = area.x + 1 + pane_inner_width as u16 - icons.total_width;
                for (idx, (_action, icon_width)) in icons.hit_regions.iter().enumerate() {
                    let icon_rect = Rect::new(icon_x, y, *icon_width, 1);
                    if buttons::is_hovered(mouse_pos, icon_rect) {
                        let empty = String::new();
                        let text = icons.hover_texts.get(idx)
                            .or_else(|| icons.texts.get(idx))
                            .unwrap_or(&empty);
                        let overlay = Paragraph::new(text.clone())
                            .style(Style::default().fg(Color::White));
                        frame.render_widget(overlay, icon_rect);
                    }
                    icon_x += icon_width;
                }
            }
        }

        // Phase 4: Render hover overlays for repo line icons
        for (item_idx, icons) in &repo_icons {
            if let Some(row_in_viewport) = item_idx.checked_sub(scroll_offset) {
                let y = area.y + 1 + row_in_viewport as u16;
                if y >= area.y + area.height - 1 {
                    continue;
                }
                let mut icon_x = area.x + 1 + pane_inner_width as u16 - icons.total_width;
                for (idx, (_action, icon_width)) in icons.hit_regions.iter().enumerate() {
                    let icon_rect = Rect::new(icon_x, y, *icon_width, 1);
                    if buttons::is_hovered(mouse_pos, icon_rect) {
                        let text = icons.texts.get(idx).cloned().unwrap_or_default();
                        let overlay = Paragraph::new(text)
                            .style(Style::default().fg(Color::White));
                        frame.render_widget(overlay, icon_rect);
                    }
                    icon_x += icon_width;
                }
            }
        }
    }

    // Render "+" button on title bar (top-right corner) — after all widgets so it's not overwritten
    {
        let border_style = if app.focus == Focus::Tree {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let plus_x = area.x + area.width.saturating_sub(4);
        let plus_rect = Rect::new(plus_x, area.y, 3, 1);
        let plus_hovered = buttons::is_hovered(app.mouse_pos, plus_rect);
        let plus_style = if plus_hovered {
            Style::default().fg(Color::White)
        } else {
            border_style
        };
        frame.render_widget(Paragraph::new(" + ").style(plus_style), plus_rect);
        app.hit_regions.push(buttons::HitRegion {
            area: plus_rect,
            action: HitAction::AddRepo,
        });
    }
}




fn render_right_pane(frame: &mut ratatui::Frame, app: &mut App, area: Rect) {
    // Remember the right-pane geometry so a newly-toggled embedded pane spawns at
    // its final size (avoids a resize-after-spawn that loses claude's first screen).
    app.right_pane_area = area;

    // Embedded interactive claude (Phase 2): if the selected workspace has a live
    // pane, it owns the whole right area (claude renders its own input box).
    let sel_ws = match app.tree_items.get(app.selected_index) {
        Some(TreeNode::Workspace { ws, .. }) => Some((ws.id.clone(), ws.name.clone())),
        _ => None,
    };
    if let Some((ws_id, ws_name)) = &sel_ws
        && app.embedded.contains_key(ws_id)
    {
        let border = if app.focus == Focus::Embedded { Color::Cyan } else { Color::DarkGray };
        let block = Block::default()
            .title(format!(" {ws_name} — claude · Ctrl+A then: q quit · t tree "))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border));
        let inner = block.inner(area);
        app.pane_areas.output = area;
        frame.render_widget(block, area);
        if let Some(p) = app.embedded.get_mut(ws_id) {
            let _ = p.resize(inner.height, inner.width);
            p.blit(frame.buffer_mut(), inner);
        }
        return;
    }

    let right_width = area.width.saturating_sub(4) as usize;

    // The interactive embedded pane is the default session view; only a *running*
    // legacy stream session falls back to the old output+composer layout.
    let session_info = match app.tree_items.get(app.selected_index) {
        Some(TreeNode::Workspace { ws, .. }) => {
            app.state
                .find_session_by_workspace(&ws.id)
                .filter(|s| s.status == SessionStatus::Running)
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
        // Collect lines into owned Vec to avoid borrow conflict with scrollbacks.get_mut
        let owned_lines: Vec<String> = {
            let mut lines: Vec<String> = app.scrollbacks.get(&ws.id)
                .map(|buf| buf.all_lines().into_iter().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            if let Some(partial) = app.streaming_text.get(&ws.id)
                && !partial.is_empty() {
                    lines.push(partial.clone());
                }
            lines
        };
        let all_lines: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let spinner = if app.waiting_response.contains(&ws.id) {
            Some(app.spinner_tick)
        } else {
            None
        };
        let mut styled_lines = build_output_lines(&all_lines, inner_width, &session_status, spinner);
        let total_visual = styled_total_visual(&styled_lines, inner_width);
        let max_offset = total_visual.saturating_sub(inner_height);
        if let Some(buf) = app.scrollbacks.get_mut(&ws.id) {
            buf.clamp_scroll_offset(max_offset);
        }
        let scroll_offset = app.scrollbacks.get(&ws.id)
            .map(|buf| buf.scroll_offset()).unwrap_or(0);

        // Apply cursor and selection highlights
        let selection = app.selections.get(&ws.id).cloned().unwrap_or_default();
        if !selection.is_none() {
            let wrap_map = super::wrap_map::WrapMap::build(&all_lines, inner_width);
            let scroll_from_top = compute_scroll_from_top(scroll_offset, total_visual, inner_height);
            if selection.has_range() {
                apply_selection_highlight(
                    &mut styled_lines,
                    &selection,
                    scroll_from_top,
                    inner_height,
                    &all_lines,
                    &wrap_map,
                    app.copy_flash_until.and_then(|(f, t)| if f == Focus::Output { Some(t) } else { None }),
                    app.focus == Focus::Output,
                );
            }
            let (cursor_line, cursor_char) = match &selection {
                SelectionState::Cursor { line, char_offset } => (*line, *char_offset),
                SelectionState::Range { cursor_line, cursor_char, .. } => (*cursor_line, *cursor_char),
                SelectionState::None => unreachable!(),
            };
            apply_cursor_highlight(
                &mut styled_lines,
                cursor_line,
                cursor_char,
                app.cursor_blink_on,
                app.focus == Focus::Output,
            );
        }

        render_output_content(frame, output_area, styled_lines, scroll_offset, inner_width, inner_height, app.focus, right_title);

        // Scrollbar (use visual line counts)
        if app.scrollbacks.contains_key(&ws.id) {
            render_scrollbar(frame, output_area, total_visual, inner_height, scroll_offset);
        }

        // New lines indicator when scrolled up
        if let Some(buf) = app.scrollbacks.get(&ws.id)
            && !buf.is_at_bottom() {
                let new_count = buf.new_lines_count();
                if new_count > 0 {
                    let indicator = format!(" \u{2193} {new_count} new lines ");
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

        // Composer area
        let composer_area = right_chunks[1];
        app.pane_areas.composer = composer_area;
        if session_status == SessionStatus::Running {
            frame.render_widget(app.composer.widget(), composer_area);
            if app.composer.slash_popup_open() {
                render_slash_popup(frame, composer_area, &app.composer);
            }
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
            let hint = Paragraph::new(Line::styled(format!("[{btn_label}]"), btn_style))
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

                let title = format!(" Repo: {name} ");
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
                        Span::raw(format!("{active} active, {total} total")),
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
                // Hint + button to open the embedded interactive claude.
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "Press Enter to open Claude here",
                    Style::default().fg(Color::DarkGray),
                ));
                {
                    // Button area: 6 detail lines + 1 empty + 1 hint + 1 border = offset 9.
                    let btn_y = area.y + 9;
                    let btn_x = area.x + 2; // inside border + 1 padding
                    let btn_label = "Open Claude";
                    let btn_rect = Rect::new(btn_x, btn_y, (btn_label.len() + 2) as u16, 1);
                    let hovered = buttons::is_hovered(app.mouse_pos, btn_rect);
                    let style = if hovered {
                        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                    };
                    lines.push(Line::styled(format!("[{btn_label}]"), style));
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
fn wrapped_line_height(display_width: usize, width: usize) -> usize {
    if width == 0 || display_width == 0 {
        return 1;
    }
    display_width.div_ceil(width)
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
    if let Some(fence_rest) = line.strip_prefix("```") {
        *in_code_block = !*in_code_block;
        if *in_code_block {
            // Opening fence — show language hint if present
            let lang = fence_rest.trim();
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
        return Line::styled(format!("  {line}"), code_block_style);
    }

    // User message (chat bubble)
    if let Some(content) = line.strip_prefix("> ") {
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
    if let Some(text) = line.strip_prefix("### ") {
        return Line::from(parse_inline_markdown(text, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
    }
    if let Some(text) = line.strip_prefix("## ") {
        return Line::from(parse_inline_markdown(text, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
    }
    if let Some(text) = line.strip_prefix("# ") {
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
    if let Some(rest) = line.strip_prefix(|c: char| c.is_ascii_digit())
        && let Some(rest) = rest.strip_prefix(". ") {
            let prefix_len = line.len() - rest.len();
            let mut spans = vec![Span::styled(
                line[..prefix_len].to_string(),
                Style::default().fg(Color::Cyan),
            )];
            spans.extend(parse_inline_markdown(rest, Style::default()));
            return Line::from(spans);
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
            format!(" {frame_char} Thinking..."),
            Style::default().fg(Color::Cyan),
        ));
    }

    lines
}

/// Calculate total visual (wrapped) line count from styled Lines.
fn styled_total_visual(lines: &[Line], inner_width: usize) -> usize {
    lines.iter()
        .map(|l| {
            let display_width: usize = l.spans.iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
            wrapped_line_height(display_width, inner_width)
        })
        .sum()
}

/// Split a Line's spans to apply a highlight style over a display-column range.
///
/// `start_col` and `end_col` are display-column offsets (half-open: [start_col, end_col)).
/// Spans that partially overlap the range are split at grapheme boundaries.
pub(crate) fn overlay_style_on_line(
    line: &mut Line<'static>,
    start_col: usize,
    end_col: usize,
    style: Style,
) {
    let mut new_spans: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;

    for span in line.spans.drain(..) {
        let span_text: &str = span.content.as_ref();
        let span_width: usize = UnicodeWidthStr::width(span_text);
        let span_end = col + span_width;

        if span_end <= start_col || col >= end_col {
            // Entirely outside selection
            new_spans.push(span);
        } else if col >= start_col && span_end <= end_col {
            // Entirely inside selection
            new_spans.push(Span::styled(span.content.into_owned(), style));
        } else {
            // Partially overlapping -- split by graphemes
            let mut pre = String::new();
            let mut mid = String::new();
            let mut post = String::new();
            let mut c = col;
            for g in span_text.graphemes(true) {
                let gw = UnicodeWidthStr::width(g);
                if c < start_col {
                    pre.push_str(g);
                } else if c < end_col {
                    mid.push_str(g);
                } else {
                    post.push_str(g);
                }
                c += gw;
            }
            if !pre.is_empty() {
                new_spans.push(Span::styled(pre, span.style));
            }
            if !mid.is_empty() {
                new_spans.push(Span::styled(mid, style));
            }
            if !post.is_empty() {
                new_spans.push(Span::styled(post, span.style));
            }
        }
        col = span_end;
    }
    line.spans = new_spans;
}

/// Apply selection highlight (cyan bg / black fg) to lines that overlap the selection range.
///
/// Operates on pre-wrap styled Lines using logical line indices and character offsets.
/// The styled Lines from build_output_lines correspond 1:1 with the logical raw_lines.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_selection_highlight(
    lines: &mut [Line<'static>],
    selection: &SelectionState,
    _scroll_from_top: usize,
    _inner_height: usize,
    _raw_lines: &[&str],
    _wrap_map: &super::wrap_map::WrapMap,
    copy_flash_until: Option<Instant>,
    focused: bool,
) {
    let Some(((start_line, start_char), (end_line, end_char))) = selection.ordered_range() else {
        return;
    };

    let sel_style = if copy_flash_until.is_some_and(|until| Instant::now() < until) {
        Style::default().bg(Color::White).fg(Color::Black)
    } else if focused {
        Style::default().bg(Color::Cyan).fg(Color::Black)
    } else {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    };

    for line_idx in start_line..=end_line.min(lines.len().saturating_sub(1)) {
        if line_idx >= lines.len() {
            break;
        }

        let line_display_width: usize = lines[line_idx]
            .spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();

        let col_start = if line_idx == start_line { start_char } else { 0 };
        let col_end = if line_idx == end_line {
            end_char + 1 // inclusive end -> exclusive for overlay
        } else {
            line_display_width
        };

        if col_start < col_end {
            overlay_style_on_line(&mut lines[line_idx], col_start, col_end, sel_style);
        }
    }
}

/// Apply cursor highlight to a single character position.
///
/// - focused + blink_on: white bg / black fg (visible cursor)
/// - focused + blink_off: no change (cursor invisible during off phase)
/// - unfocused: dim modifier on cursor character
/// - cursor past end of line: append a styled space
pub(crate) fn apply_cursor_highlight(
    lines: &mut [Line<'static>],
    cursor_line: usize,
    cursor_char: usize,
    blink_on: bool,
    focused: bool,
) {
    if cursor_line >= lines.len() {
        return;
    }

    let cursor_style = if !focused {
        Style::default().add_modifier(Modifier::DIM)
    } else if blink_on {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        // blink_off + focused: no visible cursor
        return;
    };

    let line = &lines[cursor_line];
    let line_display_width: usize = line
        .spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();

    if cursor_char >= line_display_width {
        // Cursor past end of line -- append a styled space
        lines[cursor_line]
            .spans
            .push(Span::styled(" ".to_string(), cursor_style));
    } else {
        // Highlight single character at cursor_char
        overlay_style_on_line(
            &mut lines[cursor_line],
            cursor_char,
            cursor_char + 1,
            cursor_style,
        );
    }
}

/// Convert a bottom-based scroll_offset to a top-based scroll_from_top value.
///
/// `scroll_offset` is the number of visual lines from the bottom (0 = at bottom).
/// Returns the number of visual lines from the top for Paragraph::scroll.
pub(crate) fn compute_scroll_from_top(scroll_offset: usize, total_visual: usize, inner_height: usize) -> usize {
    let max_scroll = total_visual.saturating_sub(inner_height);
    let clamped = scroll_offset.min(max_scroll);
    max_scroll.saturating_sub(clamped)
}

#[allow(clippy::too_many_arguments)]
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
    let scroll_from_top = compute_scroll_from_top(scroll_offset, total_visual, inner_height);

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
    let owned_lines: Vec<String> = {
        let mut lines: Vec<String> = app.scrollbacks.get(&ws.id)
            .map(|buf| buf.all_lines().into_iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        if let Some(partial) = app.streaming_text.get(&ws.id)
            && !partial.is_empty() {
                lines.push(partial.clone());
            }
        lines
    };
    let all_lines: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
    let spinner = if app.waiting_response.contains(&ws.id) {
        Some(app.spinner_tick)
    } else {
        None
    };
    let mut styled_lines = build_output_lines(&all_lines, inner_width, &session_status, spinner);
    let total_visual = styled_total_visual(&styled_lines, inner_width);
    let max_offset = total_visual.saturating_sub(inner_height);
    if let Some(buf) = app.scrollbacks.get_mut(&ws.id) {
        buf.clamp_scroll_offset(max_offset);
    }
    let scroll_offset = app.scrollbacks.get(&ws.id)
        .map(|buf| buf.scroll_offset()).unwrap_or(0);

    // Apply cursor and selection highlights
    let selection = app.selections.get(&ws.id).cloned().unwrap_or_default();
    if !selection.is_none() {
        let wrap_map = super::wrap_map::WrapMap::build(&all_lines, inner_width);
        let scroll_from_top = compute_scroll_from_top(scroll_offset, total_visual, inner_height);
        if selection.has_range() {
            apply_selection_highlight(
                &mut styled_lines,
                &selection,
                scroll_from_top,
                inner_height,
                &all_lines,
                &wrap_map,
                app.copy_flash_until.and_then(|(f, t)| if f == Focus::Output { Some(t) } else { None }),
                app.focus == Focus::Output,
            );
        }
        let (cursor_line, cursor_char) = match &selection {
            SelectionState::Cursor { line, char_offset } => (*line, *char_offset),
            SelectionState::Range { cursor_line, cursor_char, .. } => (*cursor_line, *cursor_char),
            SelectionState::None => unreachable!(),
        };
        apply_cursor_highlight(
            &mut styled_lines,
            cursor_line,
            cursor_char,
            app.cursor_blink_on,
            app.focus == Focus::Output,
        );
    }

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
        if app.composer.slash_popup_open() {
            render_slash_popup(frame, composer_area, &app.composer);
        }
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
        format!("line {current_line}/{total_visual}")
    } else {
        String::new()
    };

    let status_line = Line::from(vec![
        Span::styled(format!(" {} ", ws.name), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {bar_icon} "), Style::default().fg(bar_color)),
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

/// Icon cluster for a workspace or repo line in the tree view.
/// Contains the spans to render and hit regions for click handling.
pub(crate) struct IconCluster {
    pub spans: Vec<Span<'static>>,
    pub hit_regions: Vec<(HitAction, u16)>, // (action, icon_display_width)
    pub total_width: u16,
    pub texts: Vec<String>,         // icon text for each hit region (normal state)
    pub hover_texts: Vec<String>,   // alternative text for hover overlay (e.g., spinner -> stop)
}

/// Build an icon cluster for a workspace based on its session state.
/// Each icon is " X" (space + glyph) = 2 display columns.
///
/// Icons: ❯ (write prompt), ■ (stop), ▶ (start/resume), ↺ (retry), ✕ (delete)
/// Spinner (braille) shown when thinking, morphs to ■ on hover.
pub(crate) fn workspace_icon_cluster(
    session: Option<&kommand0_core::Session>,
    workspace_id: &str,
    is_thinking: bool,
    spinner_tick: u8,
    pane_inner_width: usize,
    is_expanded_narrow: bool,
) -> IconCluster {
    let ws_id = workspace_id.to_string();
    let icon_style = Style::default().fg(Color::Cyan);
    let delete_text = " \u{2715}".to_string(); // " ✕"

    // Narrow-width degradation: below 12 cols, show ellipsis unless force-expanded
    if pane_inner_width < 12 && !is_expanded_narrow {
        let text = " \u{2026}".to_string(); // " …"
        return IconCluster {
            spans: vec![Span::styled(text.clone(), Style::default().fg(Color::DarkGray))],
            hit_regions: vec![(HitAction::ToggleIconsFor { workspace_id: ws_id }, 2)],
            total_width: 2,
            texts: vec![text.clone()],
            hover_texts: vec![text],
        };
    }

    match session.map(|s| &s.status) {
        None => {
            // No session: start + delete
            let start_text = " \u{25B6}".to_string(); // " ▶"
            let mut spans = vec![
                Span::styled(start_text.clone(), Style::default().fg(Color::Green)),
                Span::styled(delete_text.clone(), icon_style),
            ];
            let mut regions = vec![
                (HitAction::StartSessionFor { workspace_id: ws_id.clone() }, 2),
                (HitAction::DeleteWorkspaceFor { workspace_id: ws_id }, 2),
            ];
            let mut texts = vec![start_text.clone(), delete_text.clone()];
            let mut hover_texts = vec![start_text, delete_text];
            let total = if pane_inner_width < 20 {
                // Narrow: drop delete
                spans.truncate(1);
                regions.truncate(1);
                texts.truncate(1);
                hover_texts.truncate(1);
                2
            } else {
                4
            };
            IconCluster { spans, hit_regions: regions, total_width: total, texts, hover_texts }
        }
        Some(SessionStatus::Running) => {
            if is_thinking {
                // Spinner for thinking state + delete
                let frame = SPINNER_FRAMES[spinner_tick as usize % SPINNER_FRAMES.len()];
                let text = format!(" {frame}");
                let hover_text = " \u{25A0}".to_string(); // " ■" on hover
                let mut spans = vec![
                    Span::styled(text.clone(), icon_style),
                    Span::styled(delete_text.clone(), icon_style),
                ];
                let mut regions = vec![
                    (HitAction::StopSessionFor { workspace_id: ws_id.clone() }, 2),
                    (HitAction::DeleteWorkspaceFor { workspace_id: ws_id }, 2),
                ];
                let mut texts_v = vec![text, delete_text.clone()];
                let mut hover_v = vec![hover_text, delete_text];
                let total = if pane_inner_width < 20 {
                    spans.truncate(1);
                    regions.truncate(1);
                    texts_v.truncate(1);
                    hover_v.truncate(1);
                    2
                } else {
                    4
                };
                IconCluster { spans, hit_regions: regions, total_width: total, texts: texts_v, hover_texts: hover_v }
            } else {
                // Idle running: prompt + stop + delete
                let prompt_text = " \u{276F}".to_string(); // " ❯"
                let stop_text = " \u{25A0}".to_string();   // " ■"
                let mut spans = vec![
                    Span::styled(prompt_text.clone(), icon_style),
                    Span::styled(stop_text.clone(), icon_style),
                    Span::styled(delete_text.clone(), icon_style),
                ];
                let mut regions = vec![
                    (HitAction::FocusComposerFor { workspace_id: ws_id.clone() }, 2),
                    (HitAction::StopSessionFor { workspace_id: ws_id.clone() }, 2),
                    (HitAction::DeleteWorkspaceFor { workspace_id: ws_id }, 2),
                ];
                let mut texts_v = vec![prompt_text.clone(), stop_text.clone(), delete_text.clone()];
                let mut hover_v = vec![prompt_text, stop_text, delete_text];
                let total = if pane_inner_width < 20 {
                    // Narrow: drop prompt and delete, keep stop only
                    spans = vec![spans.remove(1)];
                    regions = vec![regions.remove(1)];
                    texts_v = vec![texts_v.remove(1)];
                    hover_v = vec![hover_v.remove(1)];
                    2
                } else {
                    6
                };
                IconCluster { spans, hit_regions: regions, total_width: total, texts: texts_v, hover_texts: hover_v }
            }
        }
        Some(SessionStatus::Stopped) | Some(SessionStatus::Exited) => {
            let resume_text = " \u{25B6}".to_string(); // " ▶"
            let mut spans = vec![
                Span::styled(resume_text.clone(), Style::default().fg(Color::Green)),
                Span::styled(delete_text.clone(), icon_style),
            ];
            let mut regions = vec![
                (HitAction::ResumeSessionFor { workspace_id: ws_id.clone() }, 2),
                (HitAction::DeleteWorkspaceFor { workspace_id: ws_id }, 2),
            ];
            let mut texts = vec![resume_text.clone(), delete_text.clone()];
            let mut hover_texts = vec![resume_text, delete_text];
            let total = if pane_inner_width < 20 {
                spans.truncate(1);
                regions.truncate(1);
                texts.truncate(1);
                hover_texts.truncate(1);
                2
            } else {
                4
            };
            IconCluster { spans, hit_regions: regions, total_width: total, texts, hover_texts }
        }
        Some(SessionStatus::Failed) => {
            let retry_text = " \u{21BA}".to_string(); // " ↺"
            let mut spans = vec![
                Span::styled(retry_text.clone(), Style::default().fg(Color::Red)),
                Span::styled(delete_text.clone(), icon_style),
            ];
            let mut regions = vec![
                (HitAction::RetrySessionFor { workspace_id: ws_id.clone() }, 2),
                (HitAction::DeleteWorkspaceFor { workspace_id: ws_id }, 2),
            ];
            let mut texts = vec![retry_text.clone(), delete_text.clone()];
            let mut hover_texts = vec![retry_text, delete_text];
            let total = if pane_inner_width < 20 {
                spans.truncate(1);
                regions.truncate(1);
                texts.truncate(1);
                hover_texts.truncate(1);
                2
            } else {
                4
            };
            IconCluster { spans, hit_regions: regions, total_width: total, texts, hover_texts }
        }
    }
}

/// Build icons for a repo line: ✕ (delete repo) + (add workspace)
fn repo_line_icons(repo_id: &str, repo_name: &str, pane_inner_width: usize) -> IconCluster {
    let icon_style = Style::default().fg(Color::Cyan);

    if pane_inner_width < 20 {
        // Too narrow for repo icons
        return IconCluster {
            spans: vec![],
            hit_regions: vec![],
            total_width: 0,
            texts: vec![],
            hover_texts: vec![],
        };
    }

    let delete_text = " \u{2715}".to_string(); // " ✕"
    let add_text = " +".to_string();

    IconCluster {
        spans: vec![
            Span::styled(delete_text.clone(), icon_style),
            Span::styled(add_text.clone(), icon_style),
        ],
        hit_regions: vec![
            (HitAction::DeleteRepoFor { repo_name: repo_name.to_string() }, 2),
            (HitAction::AddWorkspaceFor { repo_id: repo_id.to_string() }, 2),
        ],
        total_width: 4,
        texts: vec![delete_text.clone(), add_text.clone()],
        hover_texts: vec![delete_text, add_text],
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn open_popup_composer() -> Composer {
        let mut c = Composer::new();
        c.set_slash_commands(vec!["compact".into(), "context".into(), "clear".into()]);
        c.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(c.slash_popup_open());
        c
    }

    #[test]
    fn slash_popup_renders_without_panic_on_narrow_pane() {
        // A pane narrower than the 20-col minimum used to panic in clamp().
        let composer = open_popup_composer();
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 15, 10, 5); // width 10 < 20
                render_slash_popup(frame, area, &composer);
            })
            .unwrap();
    }

    #[test]
    fn slash_popup_skips_when_no_room_above() {
        let composer = open_popup_composer();
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 1, 30, 5); // only 1 row above -> skip, no panic
                render_slash_popup(frame, area, &composer);
            })
            .unwrap();
    }

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

    fn make_session(status: SessionStatus) -> kommand0_core::Session {
        kommand0_core::Session {
            id: "s-1".to_string(),
            workspace_id: "ws-1".to_string(),
            claude_session_id: None,
            pid: None,
            status,
            created_at: 0,
            ended_at: None,
            log_file: String::new(),
        }
    }

    #[test]
    fn icon_cluster_no_session() {
        let cluster = workspace_icon_cluster(None, "ws-1", false, 0, 40, false);
        assert_eq!(cluster.total_width, 4); // start + delete
        assert_eq!(cluster.hit_regions.len(), 2);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::StartSessionFor { workspace_id: "ws-1".to_string() }
        );
        assert_eq!(
            cluster.hit_regions[1].0,
            HitAction::DeleteWorkspaceFor { workspace_id: "ws-1".to_string() }
        );
    }

    #[test]
    fn icon_cluster_running_thinking_returns_spinner() {
        let session = make_session(SessionStatus::Running);
        let cluster = workspace_icon_cluster(Some(&session), "ws-1", true, 0, 40, false);
        assert_eq!(cluster.total_width, 4); // spinner + delete
        // Should contain a braille spinner character
        let text = &cluster.spans[0].content;
        assert!(SPINNER_FRAMES.iter().any(|f| text.contains(f)),
            "Spinner span should contain a braille frame, got: {text:?}");
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::StopSessionFor { workspace_id: "ws-1".to_string() }
        );
    }

    #[test]
    fn icon_cluster_running_idle_returns_prompt_stop_delete() {
        let session = make_session(SessionStatus::Running);
        let cluster = workspace_icon_cluster(Some(&session), "ws-1", false, 0, 40, false);
        assert_eq!(cluster.total_width, 6); // prompt + stop + delete
        assert_eq!(cluster.spans.len(), 3);
        assert_eq!(cluster.hit_regions.len(), 3);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::FocusComposerFor { workspace_id: "ws-1".to_string() }
        );
        assert_eq!(
            cluster.hit_regions[1].0,
            HitAction::StopSessionFor { workspace_id: "ws-1".to_string() }
        );
        assert_eq!(
            cluster.hit_regions[2].0,
            HitAction::DeleteWorkspaceFor { workspace_id: "ws-1".to_string() }
        );
        // Prompt icon should be ❯
        assert!(cluster.spans[0].content.contains('\u{276F}'));
    }

    #[test]
    fn icon_cluster_running_narrow_drops_to_stop_only() {
        let session = make_session(SessionStatus::Running);
        // pane_inner_width < 20 but >= 12: keep stop only
        let cluster = workspace_icon_cluster(Some(&session), "ws-1", false, 0, 15, false);
        assert_eq!(cluster.total_width, 2);
        assert_eq!(cluster.hit_regions.len(), 1);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::StopSessionFor { workspace_id: "ws-1".to_string() }
        );
    }

    #[test]
    fn icon_cluster_very_narrow_shows_ellipsis() {
        let session = make_session(SessionStatus::Running);
        // pane_inner_width < 12, not expanded: ellipsis
        let cluster = workspace_icon_cluster(Some(&session), "ws-1", false, 0, 10, false);
        assert_eq!(cluster.total_width, 2);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::ToggleIconsFor { workspace_id: "ws-1".to_string() }
        );
        assert!(cluster.spans[0].content.contains('\u{2026}')); // ellipsis
    }

    #[test]
    fn icon_cluster_very_narrow_expanded_shows_normal() {
        let session = make_session(SessionStatus::Running);
        // pane_inner_width < 12, but is_expanded_narrow=true: normal icons
        let cluster = workspace_icon_cluster(Some(&session), "ws-1", false, 0, 10, true);
        // Should NOT be ellipsis -- should be stop icon (narrow < 20 drops others)
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::StopSessionFor { workspace_id: "ws-1".to_string() }
        );
    }

    #[test]
    fn icon_cluster_stopped() {
        let session = make_session(SessionStatus::Stopped);
        let cluster = workspace_icon_cluster(Some(&session), "ws-1", false, 0, 40, false);
        assert_eq!(cluster.total_width, 4); // resume + delete
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::ResumeSessionFor { workspace_id: "ws-1".to_string() }
        );
        assert_eq!(
            cluster.hit_regions[1].0,
            HitAction::DeleteWorkspaceFor { workspace_id: "ws-1".to_string() }
        );
    }

    #[test]
    fn icon_cluster_exited() {
        let session = make_session(SessionStatus::Exited);
        let cluster = workspace_icon_cluster(Some(&session), "ws-1", false, 0, 40, false);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::ResumeSessionFor { workspace_id: "ws-1".to_string() }
        );
    }

    #[test]
    fn icon_cluster_failed() {
        let session = make_session(SessionStatus::Failed);
        let cluster = workspace_icon_cluster(Some(&session), "ws-1", false, 0, 40, false);
        assert_eq!(cluster.total_width, 4); // retry + delete
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::RetrySessionFor { workspace_id: "ws-1".to_string() }
        );
        // Retry icon should be red "↺"
        assert!(cluster.spans[0].content.contains('\u{21BA}'));
    }

    #[test]
    fn icon_cluster_no_session_narrow_drops_delete() {
        let cluster = workspace_icon_cluster(None, "ws-1", false, 0, 15, false);
        assert_eq!(cluster.total_width, 2); // start only
        assert_eq!(cluster.hit_regions.len(), 1);
    }

    #[test]
    fn repo_line_icons_normal_width() {
        let icons = repo_line_icons("r-1", "myrepo", 40);
        assert_eq!(icons.total_width, 4); // delete + add
        assert_eq!(icons.hit_regions.len(), 2);
        assert_eq!(
            icons.hit_regions[0].0,
            HitAction::DeleteRepoFor { repo_name: "myrepo".to_string() }
        );
        assert_eq!(
            icons.hit_regions[1].0,
            HitAction::AddWorkspaceFor { repo_id: "r-1".to_string() }
        );
    }

    #[test]
    fn repo_line_icons_narrow_hidden() {
        let icons = repo_line_icons("r-1", "myrepo", 15);
        assert_eq!(icons.total_width, 0);
        assert!(icons.hit_regions.is_empty());
    }

    // === overlay_style_on_line tests ===

    fn make_line(spans: Vec<(&str, Style)>) -> Line<'static> {
        Line::from(
            spans
                .into_iter()
                .map(|(text, style)| Span::styled(text.to_string(), style))
                .collect::<Vec<_>>(),
        )
    }

    fn cyan_sel() -> Style {
        Style::default().bg(Color::Cyan).fg(Color::Black)
    }

    #[test]
    fn overlay_entire_span() {
        let base = Style::default().fg(Color::White);
        let mut line = make_line(vec![("hello", base)]);
        overlay_style_on_line(&mut line, 0, 5, cyan_sel());
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content.as_ref(), "hello");
        assert_eq!(line.spans[0].style, cyan_sel());
    }

    #[test]
    fn overlay_partial_span_splits() {
        let base = Style::default().fg(Color::White);
        let mut line = make_line(vec![("hello world", base)]);
        // Select columns 2..7 ("llo w")
        overlay_style_on_line(&mut line, 2, 7, cyan_sel());
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content.as_ref(), "he");
        assert_eq!(line.spans[0].style, base);
        assert_eq!(line.spans[1].content.as_ref(), "llo w");
        assert_eq!(line.spans[1].style, cyan_sel());
        assert_eq!(line.spans[2].content.as_ref(), "orld");
        assert_eq!(line.spans[2].style, base);
    }

    #[test]
    fn overlay_outside_range_unchanged() {
        let base = Style::default().fg(Color::White);
        let mut line = make_line(vec![("abc", base)]);
        overlay_style_on_line(&mut line, 5, 10, cyan_sel());
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content.as_ref(), "abc");
        assert_eq!(line.spans[0].style, base);
    }

    #[test]
    fn overlay_cjk_at_split_boundary() {
        let base = Style::default().fg(Color::White);
        // "\u{4F60}\u{597D}" = "你好", each 2 display columns wide = total 4 columns
        let mut line = make_line(vec![("\u{4F60}\u{597D}", base)]);
        // Select columns 0..2 (just the first CJK character)
        overlay_style_on_line(&mut line, 0, 2, cyan_sel());
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content.as_ref(), "\u{4F60}");
        assert_eq!(line.spans[0].style, cyan_sel());
        assert_eq!(line.spans[1].content.as_ref(), "\u{597D}");
        assert_eq!(line.spans[1].style, base);
    }

    #[test]
    fn overlay_multiple_spans_splits_correctly() {
        let s1 = Style::default().fg(Color::Red);
        let s2 = Style::default().fg(Color::Green);
        let mut line = make_line(vec![("abc", s1), ("def", s2)]);
        // Select columns 1..5 ("bcde")
        overlay_style_on_line(&mut line, 1, 5, cyan_sel());
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line.spans[0].content.as_ref(), "a");
        assert_eq!(line.spans[0].style, s1);
        assert_eq!(line.spans[1].content.as_ref(), "bc");
        assert_eq!(line.spans[1].style, cyan_sel());
        assert_eq!(line.spans[2].content.as_ref(), "de");
        assert_eq!(line.spans[2].style, cyan_sel());
        assert_eq!(line.spans[3].content.as_ref(), "f");
        assert_eq!(line.spans[3].style, s2);
    }

    // === apply_cursor_highlight tests ===

    #[test]
    fn cursor_highlight_focused_blink_on() {
        let base = Style::default().fg(Color::White);
        let mut lines = vec![make_line(vec![("hello", base)])];
        let cursor_style = Style::default().bg(Color::White).fg(Color::Black);
        apply_cursor_highlight(&mut lines, 0, 2, true, true);
        // Character at col 2 ('l') should be white bg/black fg
        // Line should be split into: "he" (base), "l" (cursor), "lo" (base)
        assert_eq!(lines[0].spans.len(), 3);
        assert_eq!(lines[0].spans[1].content.as_ref(), "l");
        assert_eq!(lines[0].spans[1].style, cursor_style);
    }

    #[test]
    fn cursor_highlight_past_end_appends_space() {
        let base = Style::default().fg(Color::White);
        let mut lines = vec![make_line(vec![("hi", base)])];
        let cursor_style = Style::default().bg(Color::White).fg(Color::Black);
        apply_cursor_highlight(&mut lines, 0, 5, true, true);
        // Should append styled space at the end
        let last = lines[0].spans.last().unwrap();
        assert_eq!(last.content.as_ref(), " ");
        assert_eq!(last.style, cursor_style);
    }

    #[test]
    fn cursor_highlight_unfocused_uses_dim() {
        let base = Style::default().fg(Color::White);
        let mut lines = vec![make_line(vec![("hello", base)])];
        apply_cursor_highlight(&mut lines, 0, 2, true, false);
        // Unfocused: dim style on cursor character
        let cursor_span = &lines[0].spans[1];
        assert_eq!(cursor_span.content.as_ref(), "l");
        assert!(cursor_span.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn cursor_highlight_blink_off_no_change() {
        let base = Style::default().fg(Color::White);
        let mut lines = vec![make_line(vec![("hello", base)])];
        apply_cursor_highlight(&mut lines, 0, 2, false, true);
        // blink_off + focused: no style override, line unchanged
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content.as_ref(), "hello");
    }

    // === compute_scroll_from_top tests ===

    #[test]
    fn compute_scroll_from_top_at_bottom() {
        // scroll_offset=0 means at bottom, scroll_from_top should be max
        assert_eq!(compute_scroll_from_top(0, 100, 20), 80);
    }

    #[test]
    fn compute_scroll_from_top_at_top() {
        // scroll_offset=max means at top, scroll_from_top should be 0
        assert_eq!(compute_scroll_from_top(80, 100, 20), 0);
    }

    #[test]
    fn compute_scroll_from_top_mid() {
        // scroll_offset=30 with total=100, height=20 => max=80, clamped=30, from_top=50
        assert_eq!(compute_scroll_from_top(30, 100, 20), 50);
    }

    #[test]
    fn compute_scroll_from_top_oversized_offset() {
        // scroll_offset exceeds max -- should clamp
        assert_eq!(compute_scroll_from_top(200, 100, 20), 0);
    }

    #[test]
    fn compute_scroll_from_top_content_smaller_than_viewport() {
        // total < inner_height => max_scroll=0, always scroll_from_top=0
        assert_eq!(compute_scroll_from_top(0, 10, 20), 0);
        assert_eq!(compute_scroll_from_top(5, 10, 20), 0);
    }

    #[test]
    fn truncate_to_width_cjk_no_partial() {
        let result = truncate_to_width("\u{4F60}\u{597D}x", 3);
        assert_eq!(result, "\u{4F60}");
        assert!(UnicodeWidthStr::width(result.as_str()) <= 3);
    }
}
