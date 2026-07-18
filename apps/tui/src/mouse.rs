use std::io::Write;

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

/// A drag-selection over the visible grid of a mouse-less embedded pane.
/// Cells are pane-local **(row, col)** — reading order, matching blit's
/// range compare and vt100's `contents_between` — NOT the `(col, row)` that
/// `translate_mouse` produces; producers must swap at assignment.
#[derive(Clone, Copy)]
pub(crate) struct PaneSelection {
    /// The Down cell (fixed end).
    pub anchor: (u16, u16),
    /// The latest drag cell, inclusive (moving end).
    pub head: (u16, u16),
    /// Still receiving Drag events (false after Up or a stray event).
    pub dragging: bool,
}

impl PaneSelection {
    /// The normalized inclusive range in reading order — tuple lexicographic
    /// min/max, so a backwards or upward drag just swaps ends.
    pub(crate) fn range(&self) -> ((u16, u16), (u16, u16)) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

/// Clamp an absolute mouse position into the pane's content rect and convert
/// to pane-local **(row, col)** — note the swap from the `(col, row)` the
/// mouse event carries. A drag that leaves the pane keeps selecting to the
/// nearest edge.
fn clamp_to_content(right_pane_area: Rect, col: u16, row: u16) -> (u16, u16) {
    let inner = super::pane_content_rect(right_pane_area);
    let col = col.clamp(inner.x, inner.x + inner.width.saturating_sub(1));
    let row = row.clamp(inner.y, inner.y + inner.height.saturating_sub(1));
    (row - inner.y, col - inner.x)
}

/// tmux-rule drag selection for embedded panes (runs before the focus-based
/// routing split). A Left-Down on the content of a child that has NOT
/// enabled mouse reporting starts a kommand0 selection — highlight while
/// dragging, OSC 52 copy to `out` on release; the child receives nothing for
/// that gesture. A mouse-mode child (claude included) keeps receiving every
/// event exactly as before. Arbitration is per-gesture at Down: a child
/// enabling mouse mid-drag doesn't steal the drag. Returns whether the event
/// was consumed. `out` is the host terminal's stdout in production; tests
/// capture a `Vec<u8>` (the `notify::ring_bell` pattern).
pub(crate) fn handle_selection(app: &mut App, mouse: MouseEvent, out: &mut impl Write) -> bool {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Clear-on-mouse-down, unconditional — a lingering highlight dies
            // on the next press wherever it lands.
            app.pane_selection = None;
            // COORDINATE SWAP: translate_mouse returns (col, row); the
            // selection stores (row, col). Both are (u16, u16) — a
            // transposition compiles silently, so swap exactly here.
            if let Some((col, row)) =
                super::translate_mouse(app.right_pane_area, mouse.column, mouse.row)
                && app.active_pane_mut().is_some_and(|p| !p.wants_mouse())
            {
                app.focus = super::Focus::Embedded;
                app.embedded_prefix = false;
                let cell = (row, col);
                app.pane_selection =
                    Some(PaneSelection { anchor: cell, head: cell, dragging: true });
                return true;
            }
            false
        }
        MouseEventKind::Down(_) => {
            app.pane_selection = None;
            false
        }
        _ => {
            // PaneSelection is Copy: work on a copy and write back, so `app`
            // stays borrowable for the pane/rect lookups below.
            let Some(mut sel) = app.pane_selection.filter(|s| s.dragging) else {
                return false;
            };
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    sel.head = clamp_to_content(app.right_pane_area, mouse.column, mouse.row);
                    app.pane_selection = Some(sel);
                    true
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    sel.dragging = false;
                    let (start, end) = sel.range();
                    if start == end {
                        // Plain click: focus already switched at Down; the
                        // clipboard is untouched and nothing lingers.
                        app.pane_selection = None;
                        return true;
                    }
                    app.pane_selection = Some(sel); // keep the highlight
                    let text = app
                        .active_pane_mut()
                        .map(|p| p.selection_text(start, end))
                        .unwrap_or_default();
                    // Trailing newlines never reach the clipboard: a drag past
                    // the prompt into empty rows would otherwise paste a
                    // line-executing \n into a shell. The extractor stays
                    // faithful — only the payload trims. Trimmed-to-empty (a
                    // drag over nothing but empty rows) behaves exactly like
                    // an empty extraction: no copy, the highlight stays until
                    // the next key/Down.
                    let text = text.trim_end_matches('\n');
                    if !text.is_empty() {
                        let _ = out.write_all(&super::pane::encode_osc52_copy(text));
                        let _ = out.flush();
                    }
                    true
                }
                // A stray event mid-drag (wheel, second button, Moved) ends
                // the grab and routes normally — the divider's convention.
                // The highlight lingers un-copied until the next key/Down;
                // self-healing.
                _ => {
                    sel.dragging = false;
                    app.pane_selection = Some(sel);
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_range_normalizes_reading_order() {
        // Backwards drag up-left: head before the anchor in reading order.
        let sel = PaneSelection { anchor: (5, 10), head: (2, 3), dragging: true };
        assert_eq!(sel.range(), ((2, 3), (5, 10)));
        // Same-row backwards drag.
        let sel = PaneSelection { anchor: (4, 8), head: (4, 2), dragging: true };
        assert_eq!(sel.range(), ((4, 2), (4, 8)));
        // Single cell.
        let sel = PaneSelection { anchor: (3, 3), head: (3, 3), dragging: false };
        assert_eq!(sel.range(), ((3, 3), (3, 3)));
    }

    #[test]
    fn clamp_to_content_clamps_edge_overshoot() {
        // Right pane at (30,0) 70x30: content x 31..=98, y 2..=28 (border +
        // 1-row tab strip). Expectations are (row, col) — the swap.
        let area = Rect::new(30, 0, 70, 30);
        assert_eq!(clamp_to_content(area, 50, 10), (8, 19), "in-range converts");
        assert_eq!(clamp_to_content(area, 200, 200), (26, 67), "overshoot clamps to the far edge");
        assert_eq!(clamp_to_content(area, 0, 0), (0, 0), "undershoot clamps to the origin");
    }

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
