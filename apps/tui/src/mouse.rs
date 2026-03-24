use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::{App, Focus, TreeNode};
use crate::render::compute_scroll_from_top;
use crate::selection::SelectionState;
use crate::wrap_map::WrapMap;

/// Stored pane areas from the last render frame, used for mouse hit-testing.
#[derive(Default, Clone, Copy)]
pub(crate) struct PaneAreas {
    pub tree: Rect,
    pub output: Rect,
    pub composer: Rect,
}

/// Handle a mouse event, updating app state accordingly.
pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            handle_click(app, mouse.column, mouse.row);
        }
        MouseEventKind::ScrollUp => {
            handle_scroll(app, mouse.column, mouse.row, true);
        }
        MouseEventKind::ScrollDown => {
            handle_scroll(app, mouse.column, mouse.row, false);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.mouse_pos = Some((mouse.column, mouse.row));
            // Extend selection if dragging in output pane
            let areas = app.pane_areas;
            if contains(areas.output, mouse.column, mouse.row) {
                handle_output_drag(app, &areas, mouse.column, mouse.row);
            }
        }
        MouseEventKind::Moved => {
            app.mouse_pos = Some((mouse.column, mouse.row));
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // Selection finalized (already in Range state from drag)
        }
        _ => {}
    }
}

fn contains(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x
        && col < area.x + area.width
        && row >= area.y
        && row < area.y + area.height
}

fn handle_click(app: &mut App, col: u16, row: u16) {
    // Check button hit regions first
    for region in &app.hit_regions {
        if contains(region.area, col, row) {
            app.pending_button_action = Some(region.action.clone());
            return;
        }
    }

    let areas = app.pane_areas;

    if contains(areas.tree, col, row) {
        // Click in tree pane
        if app.focus == Focus::Composer {
            app.composer.set_active(false);
        }
        app.focus = Focus::Tree;

        // Calculate which tree item was clicked
        let item_row = (row.saturating_sub(areas.tree.y + 1)) as usize; // +1 for border
        if item_row < app.tree_items.len() {
            // Don't select hint nodes
            if !app.is_hint(item_row) {
                let old = app.selected_index;
                app.swap_composer_draft(old, item_row);
                app.selected_index = item_row;
                app.update_active_session();

                // Toggle expand on repo nodes
                if let Some(TreeNode::Repo { .. }) = app.tree_items.get(item_row) {
                    app.toggle_expand();
                }
            }
        }
    } else if contains(areas.output, col, row) {
        if app.focus == Focus::Composer {
            app.composer.set_active(false);
        }
        app.focus = Focus::Output;

        // Place cursor at clicked position
        if let Some(ws_id) = app.selected_workspace().map(|ws| ws.id.clone()) {
            let inner_width = (areas.output.width.saturating_sub(2)) as usize;
            let inner_height = (areas.output.height.saturating_sub(2)) as usize;
            let owned_lines = collect_output_lines(app, &ws_id);
            let all_lines: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
            let wrap_map = WrapMap::build(&all_lines, inner_width);
            let scroll_offset = app.scrollbacks.get(&ws_id)
                .map(|buf| buf.scroll_offset()).unwrap_or(0);
            let total_visual = wrap_map.total_visual_rows();
            let scroll_from_top = compute_scroll_from_top(scroll_offset, total_visual, inner_height);

            // Translate screen coords (relative to output pane inner area) to logical
            let inner_x = col.saturating_sub(areas.output.x + 1); // +1 for left border
            let inner_y = row.saturating_sub(areas.output.y + 1); // +1 for top border

            if let Some((log_line, log_char)) = wrap_map.screen_to_logical(inner_x, inner_y, scroll_from_top, &all_lines) {
                // Clear any existing selection, set cursor
                let sel = app.selections.entry(ws_id.clone()).or_insert(SelectionState::None);
                *sel = SelectionState::Cursor { line: log_line, char_offset: log_char };
                // Update desired column
                app.cursor_desired_col.insert(ws_id.clone(), inner_x as usize);
                // Suppress auto-scroll
                app.auto_scroll_suppressed.insert(ws_id);
            }
        }
    } else if contains(areas.composer, col, row) {
        // Only focus composer if there's a running session
        let has_running = app
            .selected_workspace()
            .and_then(|ws| app.state.find_session_by_workspace(&ws.id))
            .map(|s| s.status == kommand0_core::SessionStatus::Running)
            .unwrap_or(false);
        if has_running {
            app.focus = Focus::Composer;
            app.composer.set_active(true);
        }
    }
}

/// Collect output lines for a workspace, including any in-progress streaming text.
fn collect_output_lines(app: &App, ws_id: &str) -> Vec<String> {
    let mut lines: Vec<String> = app.scrollbacks.get(ws_id)
        .map(|buf| buf.all_lines().into_iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    if let Some(partial) = app.streaming_text.get(ws_id) {
        if !partial.is_empty() {
            lines.push(partial.clone());
        }
    }
    lines
}

/// Handle mouse drag in the output pane to extend selection.
fn handle_output_drag(app: &mut App, areas: &PaneAreas, col: u16, row: u16) {
    if let Some(ws_id) = app.selected_workspace().map(|ws| ws.id.clone()) {
        let inner_width = (areas.output.width.saturating_sub(2)) as usize;
        let inner_height = (areas.output.height.saturating_sub(2)) as usize;
        let owned_lines = collect_output_lines(app, &ws_id);
        let all_lines: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let wrap_map = WrapMap::build(&all_lines, inner_width);
        let scroll_offset = app.scrollbacks.get(&ws_id)
            .map(|buf| buf.scroll_offset()).unwrap_or(0);
        let total_visual = wrap_map.total_visual_rows();
        let scroll_from_top = compute_scroll_from_top(scroll_offset, total_visual, inner_height);

        let inner_x = col.saturating_sub(areas.output.x + 1);
        let inner_y = row.saturating_sub(areas.output.y + 1);

        if let Some((log_line, log_char)) = wrap_map.screen_to_logical(inner_x, inner_y, scroll_from_top, &all_lines) {
            let sel = app.selections.entry(ws_id.clone()).or_insert(SelectionState::None);
            match sel {
                SelectionState::Cursor { line, char_offset } => {
                    // First drag from cursor: set anchor at cursor, cursor at drag pos
                    *sel = SelectionState::Range {
                        anchor_line: *line,
                        anchor_char: *char_offset,
                        cursor_line: log_line,
                        cursor_char: log_char,
                    };
                }
                SelectionState::Range { cursor_line, cursor_char, .. } => {
                    // Continuing drag: update cursor end
                    *cursor_line = log_line;
                    *cursor_char = log_char;
                }
                SelectionState::None => {
                    // Drag without prior click (shouldn't happen, but handle gracefully)
                    *sel = SelectionState::Cursor { line: log_line, char_offset: log_char };
                }
            }
        }
    }
}

fn handle_scroll(app: &mut App, col: u16, row: u16, up: bool) {
    let areas = app.pane_areas;
    let scroll_lines = 3;

    if contains(areas.output, col, row) {
        // Scroll output and clear selection
        if let Some(ws_id) = app.selected_workspace().map(|ws| ws.id.clone()) {
            if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                if up {
                    buf.scroll_up(scroll_lines);
                } else {
                    buf.scroll_down(scroll_lines);
                }
            }
            app.clear_selection_for_workspace(&ws_id);
        }
    } else if contains(areas.tree, col, row) {
        // Navigate tree
        if up {
            app.move_up();
        } else {
            app.move_down();
        }
    }
}
