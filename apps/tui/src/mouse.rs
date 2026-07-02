use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::{App, TreeNode};

/// Stored pane areas from the last render frame, used for mouse hit-testing.
#[derive(Default, Clone, Copy)]
pub(crate) struct PaneAreas {
    pub tree: Rect,
    /// The horizontal split parent (tree + content). Needed to convert a mouse
    /// column into a tree-width percentage — `tree` alone lacks the total width.
    pub body: Rect,
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

/// The tree-width percent for a divider dropped at `col`. Computed in `u32`
/// (`col * 100` overflows `u16` past ~655 cols), with `saturating_sub` on the
/// origin (a `Drag` can report `col < body.x`) and a `+1` bias so the tree's
/// right border (`tree.x + tree.width - 1`) tracks the cursor, not trails it.
fn width_pct_at(body: Rect, col: u16) -> u16 {
    if body.width == 0 {
        return 0;
    }
    let off = col.saturating_sub(body.x) as u32 + 1;
    (off * 100 / body.width as u32) as u16 // ≤ 100, fits u16
}

/// Whether `(col, row)` lands on the tree's right border grab-zone: `col`
/// within ±1 of `tree.x + tree.width - 1` (via `abs_diff`, no underflow) and
/// `row` inside the pane's vertical span (so the status row can't start a drag).
fn on_divider(tree: Rect, col: u16, row: u16) -> bool {
    let divider = tree.x + tree.width - 1;
    col.abs_diff(divider) <= 1 && row >= tree.y && row < tree.y + tree.height
}

/// Handle a border-drag resize of the tree pane, before the focus-based split
/// so it works whether the tree or the embedded pane is focused. Keyed on
/// `app.dragging_divider` (NOT re-hit-testing `on_divider` per `Drag`, so a fast
/// drag that outruns the ±1 zone keeps resizing). Returns whether it consumed
/// the event.
pub(crate) fn handle_divider_drag(app: &mut App, mouse: MouseEvent) -> bool {
    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        && on_divider(app.pane_areas.tree, mouse.column, mouse.row)
    {
        app.dragging_divider = true;
        return true; // grab the divider; width unchanged until the drag
    }
    if app.dragging_divider {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                app.set_tree_width_pct(width_pct_at(app.pane_areas.body, mouse.column));
                true
            }
            MouseEventKind::Up(_) => {
                app.dragging_divider = false;
                true
            }
            _ => {
                app.dragging_divider = false;
                false
            }
        }
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_pct_at_biases_and_subtracts_origin() {
        // +1 bias: col 39 in a width-100 body is the 40th col → 40%.
        assert_eq!(width_pct_at(Rect::new(0, 0, 100, 8), 39), 40);
        // body.x != 0: the origin is subtracted (col 59 - 10 + 1 = 50).
        assert_eq!(width_pct_at(Rect::new(10, 0, 100, 8), 59), 50);
        // Overflow guard: (4000 - 0 + 1) * 100 wraps under u16; u32 keeps it right.
        assert_eq!(width_pct_at(Rect::new(0, 0, 5000, 8), 4000), 80);
        // Zero-width body must not divide by zero.
        assert_eq!(width_pct_at(Rect::new(0, 0, 0, 8), 40), 0);
    }

    #[test]
    fn on_divider_hits_border_zone_and_span() {
        // Divider col for a width-30 tree at x=0 is 29 (last col inside).
        let tree = Rect::new(0, 0, 30, 8);
        assert!(on_divider(tree, 29, 0));
        assert!(on_divider(tree, 28, 0)); // -1 zone
        assert!(on_divider(tree, 30, 0)); // +1 zone (content's left border)
        assert!(!on_divider(tree, 27, 0)); // outside the ±1 zone
        assert!(!on_divider(tree, 5, 0)); // tree middle
        // Vertical span: rows 0..8 count, 8 is past it.
        assert!(on_divider(tree, 29, 7)); // last pane row
        assert!(!on_divider(tree, 29, 8)); // past the span (e.g. status row)
    }
}
