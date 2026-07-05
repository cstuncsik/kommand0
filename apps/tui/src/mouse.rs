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
            // A click the tree/buttons didn't consume, landing in the content
            // pane with a live session, focuses claude and passes through to it.
            if !handle_click(app, mouse.column, mouse.row)
                && contains(app.right_pane_area, mouse.column, mouse.row)
                && app.active_pane_mut().is_some()
            {
                app.focus = super::Focus::Embedded;
                app.embedded_prefix = false;
                app.forward_mouse_to_embedded(mouse);
            }
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

pub(crate) fn contains(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// Route a mouse event while the review-diff overlay owns the screen: a click
/// selects/toggles a file row or focuses the diff pane; the wheel scrolls the
/// pane under the cursor. Any click/scroll is consumed here — it never leaks to
/// the tree/pane behind the overlay.
pub(crate) fn handle_diff_mouse(app: &mut App, mouse: MouseEvent) {
    // An overlay opening mid-drag must not strand the divider grab.
    app.dragging_divider = false;
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.diff_handle_click(mouse.column, mouse.row);
        }
        MouseEventKind::ScrollUp => {
            app.diff_handle_scroll(mouse.column, mouse.row, true);
        }
        MouseEventKind::ScrollDown => {
            app.diff_handle_scroll(mouse.column, mouse.row, false);
        }
        _ => {}
    }
}

/// Returns whether the click was consumed (hit a button region or the tree
/// pane). A `false` return means the click landed elsewhere (e.g. the content
/// pane), so the caller can decide to focus it.
fn handle_click(app: &mut App, col: u16, row: u16) -> bool {
    // Check button hit regions first
    for region in &app.hit_regions {
        if contains(region.area, col, row) {
            app.pending_button_action = Some(region.action.clone());
            return true;
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
        return true;
    }

    false
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
    (off * 100 / body.width as u32) as u16 // fits u16 (bounded by terminal width); caller clamps
}

/// Whether `(col, row)` lands on the tree/content seam: `col` is the tree's
/// right border (`tree.x + tree.width - 1`) or the content's left border just
/// right of it — a 2-col grab that deliberately excludes the tree's last content
/// column, so a row click there still selects. `row` must be inside the pane's
/// vertical span (so the status row can't start a drag).
fn on_divider(tree: Rect, col: u16, row: u16) -> bool {
    let divider = tree.x + tree.width - 1;
    (col == divider || col == divider + 1) && row >= tree.y && row < tree.y + tree.height
}

/// Handle a border-drag resize of the tree pane, before the focus-based split
/// so it works whether the tree or the embedded pane is focused. Keyed on
/// `app.dragging_divider` (not re-hit-testing `on_divider` per `Drag`, so a fast
/// drag that outruns the grab zone keeps resizing). Returns whether it consumed
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
            // A stray event mid-drag (jitter: a button-less Moved, a scroll, a
            // second button) ends the grab and routes normally — never stranded.
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
        // Far-right column → 100% raw (the caller clamps); pins the +1-bias ceiling.
        assert_eq!(width_pct_at(Rect::new(0, 0, 100, 8), 99), 100);
        // Zero-width body must not divide by zero.
        assert_eq!(width_pct_at(Rect::new(0, 0, 0, 8), 40), 0);
    }

    #[test]
    fn on_divider_hits_border_zone_and_span() {
        // Divider col for a width-30 tree at x=0 is 29 (last col inside).
        let tree = Rect::new(0, 0, 30, 8);
        assert!(on_divider(tree, 29, 0)); // tree's right border
        assert!(on_divider(tree, 30, 0)); // content's left border (seam's other side)
        assert!(!on_divider(tree, 28, 0)); // tree's last content col — a row click still selects
        assert!(!on_divider(tree, 31, 0)); // past the seam
        assert!(!on_divider(tree, 5, 0)); // tree middle
        // Vertical span: rows 0..8 count, 8 is past it.
        assert!(on_divider(tree, 29, 7)); // last pane row
        assert!(!on_divider(tree, 29, 8)); // past the span (e.g. status row)
    }
}
