# Architecture Research

**Project:** Inline Tree Icon-Buttons for kommand0 TUI
**Confidence:** HIGH

## Key Findings

### No New Abstractions Needed

The existing hit-region system (`HitRegion` + `pending_button_action` + main loop dispatch) handles the entire click flow. Icons are a rendering concern only.

### One New Pure Function

`workspace_icon_cluster()` in `render.rs` — maps session state to a list of spans + associated hit regions. Pure function, no side effects, easy to test.

**Signature:**
```rust
fn workspace_icon_cluster(
    session_state: Option<&SessionStatus>,
    workspace_id: &str,
    spinner_frame: usize,
) -> (Vec<Span>, Vec<(HitAction, u16)>) // (spans, (action, relative_x_offset))
```

### Right-Alignment Strategy

Use calculated padding span within each ListItem's `Line`:
1. Calculate icon cluster total width
2. Calculate available space = pane_width - prefix_width - name_width
3. If available_space < icon_width: truncate name
4. Insert fill span between name and icons

### Data Flow: Icon Click → Action

```
Mouse click at (x, y)
  → mouse.rs: iterate hit_regions, find matching Rect
  → main.rs: pending_button_action = Some(HitAction::FocusComposer { workspace_id })
  → main.rs: match on HitAction variant, execute action
```

### Component Boundaries

| Component | Responsibility | Files |
|-----------|---------------|-------|
| Icon cluster function | Map state → spans + hit regions | `render.rs` |
| HitAction variants | Define clickable actions | `buttons.rs` |
| Click dispatch | Execute actions from hits | `main.rs` |
| Hover rendering | Highlight on mouse-over | `render.rs` (existing) |

### Build Order

1. **HitAction variants** — Add `FocusComposer`, `RetrySession` to enum
2. **Icon cluster function** — Pure function, state → spans
3. **Tree row integration** — Modify `render_tree` to include icons
4. **Click dispatch** — Add match arms in `pending_button_action` handler
5. **Spinner animation** — Wire tick to spinner frame counter
6. **Polish** — Hover highlight, detail pane cleanup

---
*Research: 2026-03-12*
