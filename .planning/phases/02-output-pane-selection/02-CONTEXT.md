# Phase 2: Output Pane Selection - Context

**Gathered:** 2026-03-24
**Status:** Ready for planning

<domain>
## Phase Boundary

Users can see a cursor, navigate it, and select text in the output pane with visual feedback. Covers: cursor rendering, arrow/Home/End/PageUp/PageDown navigation, mouse click-to-place, mouse drag selection, Shift+key selection, Ctrl+A select-all, and highlight rendering. Clipboard copy and keybinding changes are Phase 3.

</domain>

<decisions>
## Implementation Decisions

### Cursor Appearance
- Block cursor (full character highlight, like vim normal mode)
- White background / black text (distinct from cyan selection highlight)
- Standard blink (~500ms toggle)
- Block on empty space (past end of line or empty lines — full-width block on space char)
- Dim/hollow block when output pane is unfocused (show position but indicate unfocused)
- Always visible when output pane is focused

### Cursor Initial Position
- Bottom-left of visible content (most recent output) when output pane first gets focus
- Remember last cursor position when switching back to output pane

### Cursor Movement
- Up/Down moves by visual rows (respects wrapping — one press = one screen row)
- Left/Right wraps across line boundaries (Left at start → end of previous line)
- Ctrl+Left/Right jumps by word boundaries
- Home = first character of entire output (document top), scrolls if needed
- End = last character of entire output (document bottom), scrolls if needed
- Home then Shift+End = select all (equivalent to Ctrl+A)
- Page Up/Down moves cursor + scrolls viewport (cursor stays relative to viewport, VS Code style)
- Maintain desired column across Up/Down movements (remember horizontal position across short lines)
- Auto-scroll to reveal content when cursor hits visible edge, stop at document boundary

### Selection via Keyboard
- Shift+Arrow extends selection from cursor position (character-level)
- Shift+Ctrl+Left/Right extends selection by word
- Shift+Home/Shift+End extends selection to document start/end
- Ctrl+A selects all text in output pane when focused
- Selection clears on manual scroll (already decided in PROJECT.md)

### Mouse Behavior
- Click focuses output pane AND places cursor at clicked character position
- Click clears any existing selection (standard editor behavior)
- Click on empty space (past line end) snaps cursor to end of the line
- Mouse drag: anchor set immediately on MouseDown, drag extends selection in real time
- Standard editor behavior for anything not specified

### Selection Highlight
- Cyan background / black text (already decided in PROJECT.md)
- Visually distinct from cursor (white bg vs cyan bg)
- Applied via span re-styling (already decided in PROJECT.md)

### Streaming Output Interaction
- Cursor stays at its logical position when new output arrives — independent of content changes
- Placing cursor mid-document stops auto-scroll (implies user is reading)
- Cursor does NOT track bottom during auto-scroll — it's independent once placed
- Selection persists through new output arriving (as long as user doesn't scroll manually)
- Selection clears on manual scroll only

### Claude's Discretion
- Blink implementation details (timer mechanism)
- Exact word-boundary detection algorithm for Ctrl+Left/Right
- Mouse drag edge-scrolling behavior (if user drags past viewport edge)
- Any standard editor behavior not explicitly specified above

</decisions>

<specifics>
## Specific Ideas

- Cursor should feel like vim normal mode (block) but with editor-style navigation (not vim motions)
- Home/End are document-level, not line-level — this is a deliberate choice for output pane (unlike typical text editors)
- "Standard behavior applies" for anything not covered — follow common terminal/editor conventions

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `SelectionState` (selection.rs): None/Cursor/Range enum with `ordered_range()` — ready to use for cursor and selection state
- `WrapMap` (wrap_map.rs): `screen_to_logical()` and `logical_to_screen()` for coordinate translation — handles wrapping, CJK, emoji
- `WrapMap::extract_text()` — extracts text from logical range (needed for Phase 3 copy, but selection range feeds into it)
- `ScrollbackBuffer` (scrollback.rs): `visible_lines()`, `all_lines()`, `scroll_offset` — viewport management
- `build_output_lines()` (render.rs): Line building pipeline where selection highlight spans can be injected
- `PaneAreas` (mouse.rs): Screen rectangles for hit-testing mouse events against panes

### Established Patterns
- Per-workspace state via `HashMap<String, T>` (scrollbacks already use this — selection state should follow same pattern)
- Focus-based key dispatch in main.rs (Output arm at lines 797-861 — extend with Shift+arrow, Ctrl+A, etc.)
- Mouse dispatch in mouse.rs `handle_mouse()` — extend with MouseUp handler and click-to-place

### Integration Points
- `App` struct needs: per-workspace `SelectionState`, desired-column memory, auto-scroll suppression flag
- `render_output_content()` in render.rs: inject highlight spans before Paragraph rendering
- `handle_mouse()`: add MouseUp handler, click-to-place cursor, drag-to-select
- Key dispatch Focus::Output: add Shift+modifiers, Ctrl+arrows, Home/End redefinition, PageUp/PageDown cursor movement
- ScrollbackBuffer: may need method to suppress auto-scroll when cursor is placed mid-document

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 02-output-pane-selection*
*Context gathered: 2026-03-24*
