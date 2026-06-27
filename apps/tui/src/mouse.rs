use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::{App, TreeNode};

/// Stored pane areas from the last render frame, used for mouse hit-testing.
#[derive(Default, Clone, Copy)]
pub(crate) struct PaneAreas {
    pub tree: Rect,
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
        MouseEventKind::Moved => {
            app.mouse_pos = Some((mouse.column, mouse.row));
        }
        _ => {}
    }
}

fn contains(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
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
        app.focus = super::Focus::Tree;
        // Leaving the embedded pane by click must not carry a half-typed Ctrl+A
        // prefix into the next session (it would mis-fire the next keystroke).
        app.embedded_prefix = false;

        // Map the clicked viewport row to a tree_items index. The click row is
        // relative to the visible top, but the list scrolls, so add the offset
        // recorded at render time (the icon hit regions use the same offset).
        let viewport_row = row.saturating_sub(areas.tree.y + 1) as usize; // +1 for border
        let item_row = viewport_row + app.tree_scroll_offset;
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
    }
}

fn handle_scroll(app: &mut App, col: u16, row: u16, up: bool) {
    let areas = app.pane_areas;

    if contains(areas.tree, col, row) {
        // Navigate tree
        if up {
            app.move_up();
        } else {
            app.move_down();
        }
    }
}
