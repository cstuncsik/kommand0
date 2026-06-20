use kommand0_core::{SessionStatus, workspace::format_timestamp};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::buttons::HitAction;
use super::theme::Theme;
use super::{App, Focus, TreeNode, buttons, help, modal};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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

pub fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    app.hit_regions.clear();

    // Reserve a one-row status bar at the bottom.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());
    let body = rows[0];

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(body);

    app.pane_areas.tree = chunks[0];

    // Left pane: tree view
    render_tree(frame, app, chunks[0]);

    // Right pane: workspace details or the embedded claude pane
    render_right_pane(frame, app, chunks[1]);

    render_status_line(frame, app, rows[1]);

    // Help overlay on top of any layout. Tree bindings come from the (rebindable)
    // keymap so the overlay reflects the user's config.
    if app.show_help {
        let tree_rows = app.keymap.help_rows();
        help::render_help_overlay(frame, app.focus, &mut app.help_scroll, &tree_rows, app.theme);
    }

    // Modal overlay on top of everything
    if app.modal.is_active() {
        modal::render_modal(frame, &app.modal, app.theme);
    }
}

/// Bottom status bar: mode, current selection, live-pane count, and key hints.
fn render_status_line(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    let th = app.theme;
    let (mode, mode_color) = match app.focus {
        Focus::Tree => (" TREE ", th.accent),
        Focus::Embedded => (" CLAUDE ", th.active),
    };
    let context = match app.tree_items.get(app.selected_index) {
        Some(TreeNode::Workspace { ws, .. }) => ws.name.clone(),
        Some(TreeNode::Repo { name, .. }) => name.clone(),
        _ => "—".to_string(),
    };
    // Count session tabs across all workspaces (not workspaces).
    let live: usize = app.embedded.values().map(|s| s.tabs.len()).sum();
    // Active = sessions producing output; filter to live tab ids (waiting_response
    // is rebuilt each tick, but a removal earlier this frame can briefly leave a
    // stale id).
    let live_ids: std::collections::HashSet<&str> = app
        .embedded
        .values()
        .flat_map(|s| s.tabs.iter().map(|t| t.id.as_str()))
        .collect();
    let active = app
        .waiting_response
        .iter()
        .filter(|id| live_ids.contains(id.as_str()))
        .count();
    // Sessions that produced unseen output and went quiet — same session unit as
    // `live`/`active` (the stale-id guard applies here too).
    let waiting = app
        .attention
        .iter()
        .filter(|id| live_ids.contains(id.as_str()))
        .count();
    let live_label = if live == 0 {
        "no live sessions".to_string()
    } else if active > 0 {
        format!("{live} live · {active} active")
    } else {
        format!("{live} live")
    };

    let mut left_spans = vec![
        Span::styled(
            mode,
            Style::default().fg(th.inverse).bg(mode_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(context, Style::default().fg(th.text)),
        Span::raw("  "),
        Span::styled(live_label, Style::default().fg(th.muted)),
    ];
    if waiting > 0 {
        left_spans.push(Span::styled(
            format!("  {waiting} waiting"),
            Style::default().fg(th.attention).add_modifier(Modifier::BOLD),
        ));
    }
    let left = Line::from(left_spans);

    let hints = match app.focus {
        Focus::Tree => "Enter open · a repo · w ws · ? help · q quit",
        Focus::Embedded => "Ctrl+A t tree · Ctrl+A q quit",
    };

    // Size the right (hints) half by display width, not byte length — the "·"
    // separators are multi-byte — and use Max so the mode badge keeps priority on
    // a narrow terminal.
    let hint_cols = UnicodeWidthStr::width(hints) as u16;
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Max(hint_cols)])
        .split(area);
    frame.render_widget(Paragraph::new(left), halves[0]);
    frame.render_widget(
        Paragraph::new(Line::styled(hints, Style::default().fg(th.muted)))
            .alignment(ratatui::layout::Alignment::Right),
        halves[1],
    );
}

fn render_tree(frame: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let th = app.theme;
    // Tree title doubles as the `/` filter box when filtering.
    let title = if app.filter_input || !app.filter_query.is_empty() {
        let cursor = if app.filter_input { "\u{2588}" } else { "" };
        format!(" Repos · /{}{} ", app.filter_query, cursor)
    } else {
        " Repos ".to_string()
    };
    // A present-but-invalid config surfaces in the bottom border.
    let warn = app.config_warning.clone();
    let tree_block = |title: String, border_style: Style| {
        let mut b = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);
        if let Some(w) = &warn {
            b = b.title_bottom(Line::styled(
                format!(" ⚠ {w} "),
                Style::default().fg(th.error).add_modifier(Modifier::BOLD),
            ));
        }
        b
    };

    if app.tree_items.is_empty() {
        let border_style = if app.focus == Focus::Tree {
            Style::default().fg(th.accent)
        } else {
            Style::default().fg(th.muted)
        };
        let block = tree_block(title.clone(), border_style);
        if !app.filter_query.is_empty() {
            // Filter matched nothing — a single muted line.
            let hint = Paragraph::new(format!("No workspaces match /{}", app.filter_query))
                .style(Style::default().fg(th.muted))
                .block(block);
            frame.render_widget(hint, area);
        } else {
            // First-run welcome: lead with the in-TUI action (not the CLI), and
            // vertically center it within the bordered pane.
            let mut lines = vec![
                Line::styled(
                    "Welcome to kommand0",
                    Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                Line::from(vec![
                    Span::raw("Press "),
                    Span::styled("a", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
                    Span::raw(" to add a repo"),
                ]),
                Line::styled("? help · q quit", Style::default().fg(th.muted)),
            ];
            let inner_h = area.height.saturating_sub(2) as usize;
            for _ in 0..inner_h.saturating_sub(lines.len()) / 2 {
                lines.insert(0, Line::raw(""));
            }
            let hint = Paragraph::new(lines)
                .alignment(ratatui::layout::Alignment::Center)
                .style(Style::default().fg(th.text))
                .block(block);
            frame.render_widget(hint, area);
        }
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
                                .fg(th.selected)
                                .add_modifier(Modifier::BOLD)
                        } else if is_selected {
                            Style::default()
                                .fg(th.muted)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };

                        // Build repo line icons: ✕ (delete) + (add workspace)
                        let icons = repo_line_icons(th, id, name, pane_inner_width);

                        let prefix = format!("{indicator}{name}");
                        let prefix_width = UnicodeWidthStr::width(prefix.as_str());
                        let fill_width = pane_inner_width
                            .saturating_sub(prefix_width + icons.total_width as usize);

                        let mut spans = vec![
                            Span::styled(prefix, style),
                            Span::raw(" ".repeat(fill_width)),
                        ];
                        spans.extend(icons.spans.clone());

                        repo_icons.push((i, icons));

                        ListItem::new(Line::from(spans))
                    }
                    TreeNode::Workspace { ws, .. } => {
                        // A magenta dot flags a workspace whose session produced
                        // output you haven't viewed and went quiet ("needs you").
                        // It can coexist with the right-side activity spinner: the
                        // dot answers "unseen since it last went quiet", the
                        // spinner "producing right now".
                        let (dot, dot_color) = if app.ws_needs_attention(&ws.id) {
                            ("\u{25CF}", th.attention)
                        } else if ws.active {
                            ("\u{25CF}", th.active)
                        } else {
                            ("\u{25CB}", th.muted)
                        };
                        let prefix = "  \u{251C}\u{2500} ";
                        let style = if is_selected && app.focus == Focus::Tree {
                            Style::default()
                                .fg(th.selected)
                                .add_modifier(Modifier::BOLD)
                        } else if is_selected {
                            Style::default()
                                .fg(th.muted)
                                .add_modifier(Modifier::BOLD)
                        } else if !ws.active {
                            Style::default().fg(th.muted)
                        } else {
                            Style::default()
                        };

                        // Build icon cluster from session state
                        let session = app.state.find_session_by_workspace(&ws.id);
                        let is_thinking = app.ws_has_active_session(&ws.id);
                        let is_expanded_narrow = app.expanded_icon_rows.contains(&ws.id);
                        let icons = workspace_icon_cluster(
                            th,
                            session,
                            &ws.id,
                            is_thinking,
                            app.spinner_tick,
                            pane_inner_width,
                            is_expanded_narrow,
                            app.embedded.contains_key(&ws.id),
                        );

                        // Fill-span layout: prefix + dot + space + name + fill + git + icons
                        let prefix_width = UnicodeWidthStr::width(prefix);
                        let dot_width: usize = 1;
                        let space_after_dot: usize = 1;
                        let fixed_width = prefix_width + dot_width + space_after_dot;

                        // Compact git-status segment for own-branch workspaces,
                        // e.g. " ↑2↓1*". Right-anchored before the icons; its width
                        // is reserved from the name/fill so the icon hit-regions
                        // (measured from the right edge) stay aligned. Suppressed
                        // when the row is too narrow to also fit a name + icons.
                        let git_seg = if ws.worktree_path.is_some() {
                            app.branch_status
                                .get(&ws.id)
                                .map(|s| {
                                    let mut seg = String::new();
                                    if s.ahead > 0 {
                                        seg.push_str(&format!("↑{}", s.ahead));
                                    }
                                    if s.behind > 0 {
                                        seg.push_str(&format!("↓{}", s.behind));
                                    }
                                    if s.dirty {
                                        seg.push('*');
                                    }
                                    seg
                                })
                                .filter(|s| !s.is_empty())
                                .map(|s| format!(" {s}"))
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        let git_w_raw = UnicodeWidthStr::width(git_seg.as_str());
                        let git_w = if fixed_width + icons.total_width as usize + git_w_raw
                            < pane_inner_width
                        {
                            git_w_raw
                        } else {
                            0 // too narrow — drop the segment, keep name + icons
                        };
                        let git_seg = if git_w == 0 { String::new() } else { git_seg };

                        let name_max_width = pane_inner_width
                            .saturating_sub(fixed_width + git_w + icons.total_width as usize);
                        let display_name = truncate_to_width(&ws.name, name_max_width);
                        let name_display_width = UnicodeWidthStr::width(display_name.as_str());
                        let fill_width = pane_inner_width.saturating_sub(
                            fixed_width + name_display_width + git_w + icons.total_width as usize,
                        );

                        let mut spans = vec![
                            Span::styled(prefix, style),
                            Span::styled(dot, Style::default().fg(dot_color)),
                            Span::styled(format!(" {display_name}"), style),
                            Span::raw(" ".repeat(fill_width)),
                        ];
                        if git_w > 0 {
                            spans.push(Span::styled(git_seg, Style::default().fg(th.dirty)));
                        }
                        spans.extend(icons.spans.clone());

                        // Collect icon data for hit region registration after render
                        workspace_icons.push((i, icons));

                        ListItem::new(Line::from(spans))
                    }
                    TreeNode::Hint { text } => {
                        let display = format!("     {text}");
                        let style = Style::default()
                            .fg(th.muted)
                            .add_modifier(Modifier::ITALIC);
                        ListItem::new(Line::styled(display, style))
                    }
                }
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(app.selected_index));

        let tree_border_style = if app.focus == Focus::Tree {
            Style::default().fg(th.accent)
        } else {
            Style::default().fg(th.muted)
        };

        let list = List::new(items)
            .block(tree_block(title, tree_border_style))
            .highlight_style(Style::default());

        frame.render_stateful_widget(list, area, &mut list_state);

        // Register hit regions using scroll offset from rendered list state
        let scroll_offset = list_state.offset();
        let all_icons: Vec<&(usize, IconCluster)> =
            workspace_icons.iter().chain(repo_icons.iter()).collect();
        for (item_idx, icons) in all_icons {
            if let Some(row_in_viewport) = item_idx.checked_sub(scroll_offset) {
                let y = area.y + 1 + row_in_viewport as u16; // +1 for top border
                if y < area.y + area.height - 1 {
                    // within viewport (before bottom border)
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
                        let text = icons
                            .hover_texts
                            .get(idx)
                            .or_else(|| icons.texts.get(idx))
                            .unwrap_or(&empty);
                        let overlay =
                            Paragraph::new(text.clone()).style(Style::default().fg(th.text));
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
                        let overlay = Paragraph::new(text).style(Style::default().fg(th.text));
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
            Style::default().fg(th.accent)
        } else {
            Style::default().fg(th.muted)
        };
        let plus_x = area.x + area.width.saturating_sub(4);
        let plus_rect = Rect::new(plus_x, area.y, 3, 1);
        let plus_hovered = buttons::is_hovered(app.mouse_pos, plus_rect);
        let plus_style = if plus_hovered {
            Style::default().fg(th.text)
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

/// Render the session tab strip ("1 2 3 … +") for a workspace into `strip`,
/// highlighting the active tab and animating a tab into a spinner while its
/// session produces output. Registers click hit regions for each tab and `[+]`.
fn render_session_tabs(frame: &mut ratatui::Frame, app: &mut App, ws_id: &str, strip: Rect) {
    let th = app.theme;
    if strip.height == 0 {
        return;
    }
    // Snapshot what we need (incl. the session id, to look up its title) so we
    // can mutate app.hit_regions afterward.
    let snapshot: Vec<(usize, bool, bool, String)> = match app.embedded.get(ws_id) {
        Some(s) => s
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (i, i == s.active, app.waiting_response.contains(&t.id), t.id.clone()))
            .collect(),
        None => return,
    };
    let tab_count = snapshot.len();
    let spinner = SPINNER_FRAMES[app.spinner_tick as usize % SPINNER_FRAMES.len()];
    // Cap a title so one long name can't push later tabs (and the [+]) off-screen.
    const MAX_TITLE_COLS: usize = 16;

    let mut spans: Vec<Span> = Vec::new();
    let mut regions: Vec<(Rect, HitAction)> = Vec::new();
    let mut x = strip.x;
    let right = strip.x + strip.width;
    for (i, is_active, producing, id) in &snapshot {
        // Producing replaces the number with the spinner (unchanged); a user
        // title, when set, is appended so the tab is identifiable by name too.
        let glyph = if *producing {
            spinner.to_string()
        } else {
            (i + 1).to_string()
        };
        let label = match app.state.embedded_session_title(ws_id, id) {
            Some(title) if !title.is_empty() => {
                if UnicodeWidthStr::width(title) > MAX_TITLE_COLS {
                    // Reserve one column for the ellipsis so the title block stays
                    // within MAX_TITLE_COLS.
                    let shown = truncate_to_width(title, MAX_TITLE_COLS - 1);
                    format!(" {glyph} {shown}… ")
                } else {
                    format!(" {glyph} {title} ")
                }
            }
            _ => format!(" {glyph} "),
        };
        let w = UnicodeWidthStr::width(label.as_str()) as u16;
        let style = if *is_active {
            Style::default()
                .fg(th.inverse)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.muted)
        };
        if x + w <= right {
            regions.push((
                Rect::new(x, strip.y, w, 1),
                HitAction::SelectSessionTab {
                    workspace_id: ws_id.to_string(),
                    index: *i,
                },
            ));
        }
        spans.push(Span::styled(label, style));
        x += w;
    }
    // The [+] new-tab affordance, while under the cap and it fits.
    if tab_count < super::MAX_SESSION_TABS && x + 3 <= right {
        spans.push(Span::styled(
            " + ".to_string(),
            Style::default().fg(th.active).add_modifier(Modifier::BOLD),
        ));
        regions.push((
            Rect::new(x, strip.y, 3, 1),
            HitAction::NewSessionTab {
                workspace_id: ws_id.to_string(),
            },
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), strip);
    for (area, action) in regions {
        app.hit_regions.push(buttons::HitRegion { area, action });
    }
}

fn render_right_pane(frame: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let th = app.theme;
    // Remember the right-pane geometry so a newly-toggled embedded pane spawns at
    // its final size (avoids a resize-after-spawn that loses claude's first screen).
    app.right_pane_area = area;

    // Keep every live pane (all tabs, all workspaces — not just the visible one)
    // sized to the content area, so a terminal resize reaches background panes
    // and switching to one is instant. No-op per pane when the size is unchanged.
    app.resize_embedded_panes(super::pane_content_rect(area));

    // Embedded interactive claude (Phase 2): if the selected workspace has a live
    // pane, it owns the whole right area (claude renders its own input box).
    let sel_ws = match app.tree_items.get(app.selected_index) {
        Some(TreeNode::Workspace { ws, .. }) => Some((ws.id.clone(), ws.name.clone())),
        _ => None,
    };
    if let Some((ws_id, ws_name)) = &sel_ws
        && app.embedded.contains_key(ws_id)
    {
        let border = if app.focus == Focus::Embedded {
            th.accent
        } else {
            th.muted
        };
        let mut block = Block::default()
            .title(format!(
                " {ws_name} — claude · Ctrl+A: c new · [ ] switch · r rename · x close · t tree · q quit "
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border));
        // Surface a spawn/resume/cap error for this workspace in the bottom
        // border (the detail-pane error surface is unreachable while embedded).
        if let Some((err_ws, msg)) = &app.embed_error
            && err_ws == ws_id
        {
            block = block.title_bottom(Line::styled(
                format!(" ⚠ {msg} "),
                Style::default().fg(th.error).add_modifier(Modifier::BOLD),
            ));
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);
        // Session tab strip across the top row of the inner area.
        let strip = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: super::TAB_BAR_HEIGHT.min(inner.height),
        };
        render_session_tabs(frame, app, ws_id, strip);
        // The active session's pane fills the area below the tab strip. It was
        // already sized to `content` by resize_embedded_panes above.
        let content = super::pane_content_rect(area);
        if let Some(p) = app
            .embedded
            .get_mut(ws_id)
            .and_then(|s| s.active_pane_mut())
        {
            p.blit(frame.buffer_mut(), content);
        }
        return;
    }

    let right_width = area.width.saturating_sub(4) as usize;

    // The embedded claude pane is the only session view (handled above); here we
    // show workspace/repo details for the current selection.
    let (right_title, mut right_content) = match app.tree_items.get(app.selected_index) {
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
                            .fg(th.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(name.as_str()),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Path: ",
                        Style::default()
                            .fg(th.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(truncate_path(repo_path, right_width)),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Workspaces: ",
                        Style::default()
                            .fg(th.accent)
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
                Span::styled("active", Style::default().fg(th.active))
            } else {
                Span::styled("archived", Style::default().fg(th.muted))
            };
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(
                        "Name: ",
                        Style::default()
                            .fg(th.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(ws.name.as_str()),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Repo: ",
                        Style::default()
                            .fg(th.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(repo_name.as_str()),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Dir: ",
                        Style::default()
                            .fg(th.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(truncate_path(&ws.working_dir, right_width)),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Status: ",
                        Style::default()
                            .fg(th.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    status_span,
                ]),
                Line::from(vec![
                    Span::styled(
                        "Created: ",
                        Style::default()
                            .fg(th.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format_timestamp(ws.created_at)),
                ]),
            ];

            // Git branch / changes, from the off-loop status cache.
            let label_style = Style::default().fg(th.accent).add_modifier(Modifier::BOLD);
            if ws.worktree_path.is_none() {
                lines.push(Line::from(vec![
                    Span::styled("Branch: ", label_style),
                    Span::styled(
                        "(shared checkout — no workspace branch)",
                        Style::default().fg(th.muted),
                    ),
                ]));
            } else {
                let st = app.branch_status.get(&ws.id);
                let branch = st
                    .and_then(|s| s.branch.clone())
                    .or_else(|| ws.branch_name.clone())
                    .unwrap_or_else(|| "(detached)".to_string());
                let mut branch_spans =
                    vec![Span::styled("Branch: ", label_style), Span::raw(branch)];
                if let Some(s) = st {
                    if !s.has_upstream {
                        branch_spans.push(Span::styled(
                            " (no upstream)",
                            Style::default().fg(th.muted),
                        ));
                    } else if s.ahead > 0 || s.behind > 0 {
                        branch_spans.push(Span::styled(
                            format!(" ↑{} ↓{}", s.ahead, s.behind),
                            Style::default().fg(th.dirty),
                        ));
                    } else {
                        branch_spans.push(Span::styled(
                            " (up to date)",
                            Style::default().fg(th.active),
                        ));
                    }
                }
                lines.push(Line::from(branch_spans));

                let changes = match st {
                    Some(s) if s.dirty => {
                        Span::styled("uncommitted changes", Style::default().fg(th.dirty))
                    }
                    Some(_) => Span::styled("clean", Style::default().fg(th.active)),
                    None => Span::styled("…", Style::default().fg(th.muted)),
                };
                lines.push(Line::from(vec![Span::styled("Changes: ", label_style), changes]));
            }

            // Hint + button to open the embedded interactive claude.
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Press Enter to open Claude here",
                Style::default().fg(th.muted),
            ));
            {
                // Derive the button row from the line count so the hit
                // region can never drift from the rendered text: +1 for the
                // top border, lines.len() lines precede the button text.
                let btn_y = area.y + 1 + lines.len() as u16;
                let btn_x = area.x + 2; // inside border + 1 padding
                let btn_label = "Open Claude";
                let btn_rect = Rect::new(btn_x, btn_y, (btn_label.len() + 2) as u16, 1);
                let hovered = buttons::is_hovered(app.mouse_pos, btn_rect);
                let style = if hovered {
                    Style::default()
                        .fg(th.inverse)
                        .bg(th.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(th.accent)
                        .add_modifier(Modifier::BOLD)
                };
                lines.push(Line::styled(format!("[{btn_label}]"), style));
                app.hit_regions.push(buttons::HitRegion {
                    area: btn_rect,
                    action: buttons::HitAction::StartSession,
                });
            }

            // Open-PR affordance + last outcome (own-branch workspaces).
            let has_branch = ws.worktree_path.is_some() && ws.branch_name.is_some();
            let pr_inflight = app.pr_inflight.contains(&ws.id);
            if has_branch || pr_inflight || app.pr_result.contains_key(&ws.id) {
                lines.push(Line::raw(""));
                if pr_inflight {
                    let spin = SPINNER_FRAMES[app.spinner_tick as usize % SPINNER_FRAMES.len()];
                    lines.push(Line::styled(
                        format!("{spin} Opening PR…"),
                        Style::default().fg(th.accent),
                    ));
                } else if has_branch {
                    let btn_y = area.y + 1 + lines.len() as u16;
                    let btn_x = area.x + 2;
                    let btn_label = "Open PR";
                    let btn_rect = Rect::new(btn_x, btn_y, (btn_label.len() + 2) as u16, 1);
                    let hovered = buttons::is_hovered(app.mouse_pos, btn_rect);
                    let style = if hovered {
                        Style::default()
                            .fg(th.inverse)
                            .bg(th.active)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(th.active).add_modifier(Modifier::BOLD)
                    };
                    lines.push(Line::styled(format!("[{btn_label}]"), style));
                    app.hit_regions.push(buttons::HitRegion {
                        area: btn_rect,
                        action: buttons::HitAction::OpenPrFor {
                            workspace_id: ws.id.clone(),
                        },
                    });
                }
                match app.pr_result.get(&ws.id) {
                    Some(Ok(url)) => lines.push(Line::from(vec![
                        Span::styled("PR: ", label_style),
                        Span::styled(url.clone(), Style::default().fg(th.active)),
                    ])),
                    Some(Err(msg)) => lines.push(Line::from(vec![
                        Span::styled(
                            "PR failed: ",
                            Style::default().fg(th.error).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(msg.clone(), Style::default().fg(th.error)),
                    ])),
                    None => {}
                }
            }

            // Clean-up affordance + last failure (own-branch workspaces).
            let cleanup_inflight = app.cleanup_inflight.contains(&ws.id);
            if has_branch || cleanup_inflight || app.cleanup_result.contains_key(&ws.id) {
                lines.push(Line::raw(""));
                if cleanup_inflight {
                    let spin = SPINNER_FRAMES[app.spinner_tick as usize % SPINNER_FRAMES.len()];
                    lines.push(Line::styled(
                        format!("{spin} Cleaning up…"),
                        Style::default().fg(th.dirty),
                    ));
                } else if has_branch {
                    let btn_y = area.y + 1 + lines.len() as u16;
                    let btn_x = area.x + 2;
                    let btn_label = "Clean up";
                    let btn_rect = Rect::new(btn_x, btn_y, (btn_label.len() + 2) as u16, 1);
                    let hovered = buttons::is_hovered(app.mouse_pos, btn_rect);
                    let style = if hovered {
                        Style::default()
                            .fg(th.inverse)
                            .bg(th.dirty)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(th.dirty).add_modifier(Modifier::BOLD)
                    };
                    lines.push(Line::styled(format!("[{btn_label}]"), style));
                    app.hit_regions.push(buttons::HitRegion {
                        area: btn_rect,
                        action: buttons::HitAction::CleanupWorkspaceFor {
                            workspace_id: ws.id.clone(),
                        },
                    });
                }
                if let Some(msg) = app.cleanup_result.get(&ws.id) {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "Cleanup blocked: ",
                            Style::default().fg(th.error).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(msg.clone(), Style::default().fg(th.error)),
                    ]));
                }
            }

            (title, lines)
        }
        Some(TreeNode::Hint { .. }) | None => {
            let title = " Details ".to_string();
            let lines = vec![Line::styled(
                "Select a workspace to see details",
                Style::default().fg(th.muted),
            )];
            (title, lines)
        }
    };

    // Surface a spawn failure only in the detail pane of the workspace it
    // happened in (keyed by id, so navigating away doesn't show it elsewhere).
    let selected_ws_id = match app.tree_items.get(app.selected_index) {
        Some(TreeNode::Workspace { ws, .. }) => Some(ws.id.as_str()),
        _ => None,
    };
    if let (Some(sel), Some((err_ws, msg))) = (selected_ws_id, &app.embed_error)
        && sel == err_ws.as_str()
    {
        right_content.push(Line::raw(""));
        right_content.push(Line::styled(
            msg.clone(),
            Style::default().fg(th.error).add_modifier(Modifier::BOLD),
        ));
    }

    let paragraph = Paragraph::new(right_content)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .title(right_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(th.muted)),
        );
    frame.render_widget(paragraph, area);
}
/// Icon cluster for a workspace or repo line in the tree view.
/// Contains the spans to render and hit regions for click handling.
pub(crate) struct IconCluster {
    pub spans: Vec<Span<'static>>,
    pub hit_regions: Vec<(HitAction, u16)>, // (action, icon_display_width)
    pub total_width: u16,
    pub texts: Vec<String>, // icon text for each hit region (normal state)
    pub hover_texts: Vec<String>, // alternative text for hover overlay (e.g., spinner -> stop)
}

/// Build an icon cluster for a workspace based on its session state.
/// Each icon is " X" (space + glyph) = 2 display columns.
///
/// Icons: ❯ (write prompt), ■ (stop), ▶ (start/resume), ↺ (retry), ✕ (delete)
/// Spinner (braille) shown when thinking, morphs to ■ on hover.
#[allow(clippy::too_many_arguments)] // positional render inputs; a struct adds more noise than it removes
pub(crate) fn workspace_icon_cluster(
    th: Theme,
    session: Option<&kommand0_core::Session>,
    workspace_id: &str,
    is_thinking: bool,
    spinner_tick: u8,
    pane_inner_width: usize,
    is_expanded_narrow: bool,
    embedded: bool,
) -> IconCluster {
    let ws_id = workspace_id.to_string();
    let icon_style = Style::default().fg(th.accent);
    let delete_text = " \u{2715}".to_string(); // " ✕"

    // Narrow-width degradation: below 12 cols, show ellipsis unless force-expanded
    if pane_inner_width < 12 && !is_expanded_narrow {
        let text = " \u{2026}".to_string(); // " …"
        return IconCluster {
            spans: vec![Span::styled(
                text.clone(),
                Style::default().fg(th.muted),
            )],
            hit_regions: vec![(
                HitAction::ToggleIconsFor {
                    workspace_id: ws_id,
                },
                2,
            )],
            total_width: 2,
            texts: vec![text.clone()],
            hover_texts: vec![text],
        };
    }

    // A live embedded claude pane takes priority over any persisted stream
    // session status: opening a workspace creates a pane but no stream session,
    // so without this the row would advertise "start" while claude is running.
    if embedded {
        let prompt_text = " \u{276F}".to_string(); // " ❯"
        let stop_text = " \u{25A0}".to_string(); // " ■"
        // While the pane is producing output, animate the prompt glyph into a
        // spinner (same width, so the hit regions below are unaffected).
        let prompt_glyph = if is_thinking {
            format!(" {}", SPINNER_FRAMES[spinner_tick as usize % SPINNER_FRAMES.len()])
        } else {
            prompt_text.clone()
        };
        let mut spans = vec![
            Span::styled(prompt_glyph, icon_style),
            Span::styled(stop_text.clone(), icon_style),
            Span::styled(delete_text.clone(), icon_style),
        ];
        let mut regions = vec![
            (
                HitAction::FocusComposerFor {
                    workspace_id: ws_id.clone(),
                },
                2,
            ),
            (
                HitAction::StopSessionFor {
                    workspace_id: ws_id.clone(),
                },
                2,
            ),
            (
                HitAction::DeleteWorkspaceFor {
                    workspace_id: ws_id,
                },
                2,
            ),
        ];
        let mut texts_v = vec![prompt_text.clone(), stop_text.clone(), delete_text.clone()];
        let mut hover_v = vec![prompt_text, stop_text, delete_text];
        let total = if pane_inner_width < 20 {
            // Narrow: keep stop only
            spans = vec![spans.remove(1)];
            regions = vec![regions.remove(1)];
            texts_v = vec![texts_v.remove(1)];
            hover_v = vec![hover_v.remove(1)];
            2
        } else {
            6
        };
        return IconCluster {
            spans,
            hit_regions: regions,
            total_width: total,
            texts: texts_v,
            hover_texts: hover_v,
        };
    }

    match session.map(|s| &s.status) {
        None => {
            // No session: start + delete
            let start_text = " \u{25B6}".to_string(); // " ▶"
            let mut spans = vec![
                Span::styled(start_text.clone(), Style::default().fg(th.active)),
                Span::styled(delete_text.clone(), icon_style),
            ];
            let mut regions = vec![
                (
                    HitAction::StartSessionFor {
                        workspace_id: ws_id.clone(),
                    },
                    2,
                ),
                (
                    HitAction::DeleteWorkspaceFor {
                        workspace_id: ws_id,
                    },
                    2,
                ),
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
            IconCluster {
                spans,
                hit_regions: regions,
                total_width: total,
                texts,
                hover_texts,
            }
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
                    (
                        HitAction::StopSessionFor {
                            workspace_id: ws_id.clone(),
                        },
                        2,
                    ),
                    (
                        HitAction::DeleteWorkspaceFor {
                            workspace_id: ws_id,
                        },
                        2,
                    ),
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
                IconCluster {
                    spans,
                    hit_regions: regions,
                    total_width: total,
                    texts: texts_v,
                    hover_texts: hover_v,
                }
            } else {
                // Idle running: prompt + stop + delete
                let prompt_text = " \u{276F}".to_string(); // " ❯"
                let stop_text = " \u{25A0}".to_string(); // " ■"
                let mut spans = vec![
                    Span::styled(prompt_text.clone(), icon_style),
                    Span::styled(stop_text.clone(), icon_style),
                    Span::styled(delete_text.clone(), icon_style),
                ];
                let mut regions = vec![
                    (
                        HitAction::FocusComposerFor {
                            workspace_id: ws_id.clone(),
                        },
                        2,
                    ),
                    (
                        HitAction::StopSessionFor {
                            workspace_id: ws_id.clone(),
                        },
                        2,
                    ),
                    (
                        HitAction::DeleteWorkspaceFor {
                            workspace_id: ws_id,
                        },
                        2,
                    ),
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
                IconCluster {
                    spans,
                    hit_regions: regions,
                    total_width: total,
                    texts: texts_v,
                    hover_texts: hover_v,
                }
            }
        }
        Some(SessionStatus::Stopped) | Some(SessionStatus::Exited) => {
            let resume_text = " \u{25B6}".to_string(); // " ▶"
            let mut spans = vec![
                Span::styled(resume_text.clone(), Style::default().fg(th.active)),
                Span::styled(delete_text.clone(), icon_style),
            ];
            let mut regions = vec![
                (
                    HitAction::ResumeSessionFor {
                        workspace_id: ws_id.clone(),
                    },
                    2,
                ),
                (
                    HitAction::DeleteWorkspaceFor {
                        workspace_id: ws_id,
                    },
                    2,
                ),
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
            IconCluster {
                spans,
                hit_regions: regions,
                total_width: total,
                texts,
                hover_texts,
            }
        }
        Some(SessionStatus::Failed) => {
            let retry_text = " \u{21BA}".to_string(); // " ↺"
            let mut spans = vec![
                Span::styled(retry_text.clone(), Style::default().fg(th.error)),
                Span::styled(delete_text.clone(), icon_style),
            ];
            let mut regions = vec![
                (
                    HitAction::RetrySessionFor {
                        workspace_id: ws_id.clone(),
                    },
                    2,
                ),
                (
                    HitAction::DeleteWorkspaceFor {
                        workspace_id: ws_id,
                    },
                    2,
                ),
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
            IconCluster {
                spans,
                hit_regions: regions,
                total_width: total,
                texts,
                hover_texts,
            }
        }
    }
}

/// Build icons for a repo line: ✕ (delete repo) + (add workspace)
fn repo_line_icons(th: Theme, repo_id: &str, repo_name: &str, pane_inner_width: usize) -> IconCluster {
    let icon_style = Style::default().fg(th.accent);

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
            (
                HitAction::DeleteRepoFor {
                    repo_name: repo_name.to_string(),
                },
                2,
            ),
            (
                HitAction::AddWorkspaceFor {
                    repo_id: repo_id.to_string(),
                },
                2,
            ),
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
        let cluster = workspace_icon_cluster(Theme::default(), None, "ws-1", false, 0, 40, false, false);
        assert_eq!(cluster.total_width, 4); // start + delete
        assert_eq!(cluster.hit_regions.len(), 2);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::StartSessionFor {
                workspace_id: "ws-1".to_string()
            }
        );
        assert_eq!(
            cluster.hit_regions[1].0,
            HitAction::DeleteWorkspaceFor {
                workspace_id: "ws-1".to_string()
            }
        );
    }

    #[test]
    fn icon_cluster_embedded_overrides_no_session() {
        // A live embedded pane (no stream session) must show the running
        // prompt/stop/delete cluster, never the green "start" affordance.
        let cluster = workspace_icon_cluster(Theme::default(), None, "ws-1", false, 0, 40, false, true);
        assert_eq!(cluster.total_width, 6); // prompt + stop + delete
        assert_eq!(cluster.hit_regions.len(), 3);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::FocusComposerFor {
                workspace_id: "ws-1".to_string()
            }
        );
        assert_eq!(
            cluster.hit_regions[1].0,
            HitAction::StopSessionFor {
                workspace_id: "ws-1".to_string()
            }
        );
        assert!(!cluster.hit_regions.iter().any(|(a, _)| matches!(
            a,
            HitAction::StartSessionFor { .. } | HitAction::ResumeSessionFor { .. }
        )));
    }

    #[test]
    fn icon_cluster_embedded_active_animates_prompt_into_spinner() {
        // The live shipping path: an embedded pane that is producing output must
        // animate its prompt glyph into a spinner (the no-session/Running branches
        // below are unreachable now that the TUI creates no stream sessions).
        let cluster = workspace_icon_cluster(Theme::default(), None, "ws-1", /*is_thinking*/ true, 3, 40, false, true);
        let prompt = &cluster.spans[0].content;
        assert!(
            SPINNER_FRAMES.iter().any(|f| prompt.contains(f)),
            "active embedded pane should show a spinner, got: {prompt:?}"
        );
        // Idle embedded pane keeps the static prompt glyph.
        let idle = workspace_icon_cluster(Theme::default(), None, "ws-1", false, 3, 40, false, true);
        assert!(
            idle.spans[0].content.contains('\u{276F}'),
            "idle embedded pane should show the ❯ prompt, got: {:?}",
            idle.spans[0].content
        );
    }

    #[test]
    fn icon_cluster_running_thinking_returns_spinner() {
        let session = make_session(SessionStatus::Running);
        let cluster = workspace_icon_cluster(Theme::default(), Some(&session), "ws-1", true, 0, 40, false, false);
        assert_eq!(cluster.total_width, 4); // spinner + delete
        // Should contain a braille spinner character
        let text = &cluster.spans[0].content;
        assert!(
            SPINNER_FRAMES.iter().any(|f| text.contains(f)),
            "Spinner span should contain a braille frame, got: {text:?}"
        );
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::StopSessionFor {
                workspace_id: "ws-1".to_string()
            }
        );
    }

    #[test]
    fn icon_cluster_running_idle_returns_prompt_stop_delete() {
        let session = make_session(SessionStatus::Running);
        let cluster = workspace_icon_cluster(Theme::default(), Some(&session), "ws-1", false, 0, 40, false, false);
        assert_eq!(cluster.total_width, 6); // prompt + stop + delete
        assert_eq!(cluster.spans.len(), 3);
        assert_eq!(cluster.hit_regions.len(), 3);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::FocusComposerFor {
                workspace_id: "ws-1".to_string()
            }
        );
        assert_eq!(
            cluster.hit_regions[1].0,
            HitAction::StopSessionFor {
                workspace_id: "ws-1".to_string()
            }
        );
        assert_eq!(
            cluster.hit_regions[2].0,
            HitAction::DeleteWorkspaceFor {
                workspace_id: "ws-1".to_string()
            }
        );
        // Prompt icon should be ❯
        assert!(cluster.spans[0].content.contains('\u{276F}'));
    }

    #[test]
    fn icon_cluster_running_narrow_drops_to_stop_only() {
        let session = make_session(SessionStatus::Running);
        // pane_inner_width < 20 but >= 12: keep stop only
        let cluster = workspace_icon_cluster(Theme::default(), Some(&session), "ws-1", false, 0, 15, false, false);
        assert_eq!(cluster.total_width, 2);
        assert_eq!(cluster.hit_regions.len(), 1);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::StopSessionFor {
                workspace_id: "ws-1".to_string()
            }
        );
    }

    #[test]
    fn icon_cluster_very_narrow_shows_ellipsis() {
        let session = make_session(SessionStatus::Running);
        // pane_inner_width < 12, not expanded: ellipsis
        let cluster = workspace_icon_cluster(Theme::default(), Some(&session), "ws-1", false, 0, 10, false, false);
        assert_eq!(cluster.total_width, 2);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::ToggleIconsFor {
                workspace_id: "ws-1".to_string()
            }
        );
        assert!(cluster.spans[0].content.contains('\u{2026}')); // ellipsis
    }

    #[test]
    fn icon_cluster_very_narrow_expanded_shows_normal() {
        let session = make_session(SessionStatus::Running);
        // pane_inner_width < 12, but is_expanded_narrow=true: normal icons
        let cluster = workspace_icon_cluster(Theme::default(), Some(&session), "ws-1", false, 0, 10, true, false);
        // Should NOT be ellipsis -- should be stop icon (narrow < 20 drops others)
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::StopSessionFor {
                workspace_id: "ws-1".to_string()
            }
        );
    }

    #[test]
    fn icon_cluster_stopped() {
        let session = make_session(SessionStatus::Stopped);
        let cluster = workspace_icon_cluster(Theme::default(), Some(&session), "ws-1", false, 0, 40, false, false);
        assert_eq!(cluster.total_width, 4); // resume + delete
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::ResumeSessionFor {
                workspace_id: "ws-1".to_string()
            }
        );
        assert_eq!(
            cluster.hit_regions[1].0,
            HitAction::DeleteWorkspaceFor {
                workspace_id: "ws-1".to_string()
            }
        );
    }

    #[test]
    fn icon_cluster_exited() {
        let session = make_session(SessionStatus::Exited);
        let cluster = workspace_icon_cluster(Theme::default(), Some(&session), "ws-1", false, 0, 40, false, false);
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::ResumeSessionFor {
                workspace_id: "ws-1".to_string()
            }
        );
    }

    #[test]
    fn icon_cluster_failed() {
        let session = make_session(SessionStatus::Failed);
        let cluster = workspace_icon_cluster(Theme::default(), Some(&session), "ws-1", false, 0, 40, false, false);
        assert_eq!(cluster.total_width, 4); // retry + delete
        assert_eq!(
            cluster.hit_regions[0].0,
            HitAction::RetrySessionFor {
                workspace_id: "ws-1".to_string()
            }
        );
        // Retry icon should be red "↺"
        assert!(cluster.spans[0].content.contains('\u{21BA}'));
    }

    #[test]
    fn icon_cluster_no_session_narrow_drops_delete() {
        let cluster = workspace_icon_cluster(Theme::default(), None, "ws-1", false, 0, 15, false, false);
        assert_eq!(cluster.total_width, 2); // start only
        assert_eq!(cluster.hit_regions.len(), 1);
    }

    #[test]
    fn repo_line_icons_normal_width() {
        let icons = repo_line_icons(Theme::default(), "r-1", "myrepo", 40);
        assert_eq!(icons.total_width, 4); // delete + add
        assert_eq!(icons.hit_regions.len(), 2);
        assert_eq!(
            icons.hit_regions[0].0,
            HitAction::DeleteRepoFor {
                repo_name: "myrepo".to_string()
            }
        );
        assert_eq!(
            icons.hit_regions[1].0,
            HitAction::AddWorkspaceFor {
                repo_id: "r-1".to_string()
            }
        );
    }

    #[test]
    fn repo_line_icons_narrow_hidden() {
        let icons = repo_line_icons(Theme::default(), "r-1", "myrepo", 15);
        assert_eq!(icons.total_width, 0);
        assert!(icons.hit_regions.is_empty());
    }

    #[test]
    fn icons_are_colored_by_theme_accent() {
        // A non-default accent must thread through to the rendered spans, proving
        // chrome color comes from the theme rather than a hardcoded `Color`.
        let custom = ratatui::style::Color::Rgb(1, 2, 3);
        let theme = Theme { accent: custom, ..Theme::default() };
        let icons = repo_line_icons(theme, "r-1", "myrepo", 40);
        assert!(
            icons.spans.iter().all(|s| s.style.fg == Some(custom)),
            "repo icon spans should use the theme accent color"
        );
    }

    #[test]
    fn truncate_to_width_cjk_no_partial() {
        let result = truncate_to_width("\u{4F60}\u{597D}x", 3);
        assert_eq!(result, "\u{4F60}");
        assert!(UnicodeWidthStr::width(result.as_str()) <= 3);
    }
}
