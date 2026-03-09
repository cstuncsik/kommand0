use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::{App, Focus, TreeNode};

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
        MouseEventKind::Moved | MouseEventKind::Drag(MouseButton::Left) => {
            app.mouse_pos = Some((mouse.column, mouse.row));
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
            app.pending_button_action = Some(region.action);
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

fn handle_scroll(app: &mut App, col: u16, row: u16, up: bool) {
    let areas = app.pane_areas;
    let scroll_lines = 3;

    if contains(areas.output, col, row) {
        // Scroll output
        if let Some(ws_id) = app.selected_workspace().map(|ws| ws.id.clone()) {
            if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                if up {
                    buf.scroll_up(scroll_lines);
                } else {
                    buf.scroll_down(scroll_lines);
                }
            }
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
