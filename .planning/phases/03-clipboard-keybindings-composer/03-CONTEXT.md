# Phase 3: Clipboard, Keybindings & Composer - Context

**Gathered:** 2026-03-28
**Status:** Ready for planning

<domain>
## Phase Boundary

Wire copy to system clipboard, rewire Ctrl+C/Q keybinding semantics, and enable composer text selection. Covers: Ctrl+C copies selection, Ctrl+Q stops session/quits, composer selection (Shift+arrows, Ctrl+A), and copy feedback. Tree pane selection is out of scope.

</domain>

<decisions>
## Implementation Decisions

### Ctrl+C / Ctrl+Q Semantics
- Ctrl+C is ALWAYS copy — clean separation, no dual behavior
- Ctrl+C with no selection = pure no-op (no side effects, no error, no clear)
- Ctrl+C never clears composer anymore — user does Ctrl+A then Delete to clear
- No double-tap Ctrl+C escape hatch — Ctrl+C is always copy, period
- No dedicated clear-composer shortcut — select-all + delete replaces it
- Ctrl+Q stops session from ANY pane (including Composer) — universally available
- Ctrl+Q with no running session quits app
- Ctrl+Q with running session: stop session first, then quit on next press

### Composer Selection
- Full selection like output pane — SelectionState + highlight rendering + extract_text
- Consistent UX across panes (same interaction model)
- Cyan background / black text highlight (same as output pane)
- Ctrl+A selects all composer text when Composer is focused
- Shift+arrow extends selection character-by-character
- Typing replaces selection (standard editor behavior)

### Copy Feedback
- Brief flash — selection highlight flashes/pulses once on copy
- Selection persists after copy (editor convention, allows re-copy)
- Selection clears on click (like iTerm2), not automatically after copy
- Escape clears selection in Output pane
- Clipboard unavailable (headless/SSH) = silent no-op, no error shown

### Selection Persistence Across Focus
- Selection persists (dimmed) when pane loses focus
- Can still copy from unfocused pane with Ctrl+C (selection is global)

### Claude's Discretion
- Flash animation timing/implementation for copy feedback
- Composer WrapMap implementation details (simpler than output pane — no scroll offset)
- How to intercept tui-textarea key events for Shift+arrow selection
- Exact integration of SelectionState with tui-textarea cursor position

</decisions>

<specifics>
## Specific Ideas

- Copy feedback flash should be subtle — brief pulse, not distracting
- "Selection clears on click" like iTerm2 — click anywhere clears selection
- Ctrl+Q behavior is two-stage: first press stops session, second press quits app (when session was running)

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `ClipboardBridge` (clipboard.rs): `set_text(&mut self, text: &str)` — implemented in Phase 1, not yet in App struct
- `SelectionState` (selection.rs): None/Cursor/Range enum with `ordered_range()` — reuse for composer
- `WrapMap::extract_text()` (wrap_map.rs:356): extracts text from logical range — reuse for composer copy
- `build_output_lines()` (render.rs): highlight injection pipeline — pattern to replicate for composer

### Established Patterns
- Per-workspace state via `HashMap<String, T>` — composer selection should follow same pattern
- Focus-based key dispatch in main.rs — extend Ctrl+C handler to check selection before acting
- Mouse dispatch in mouse.rs — click-clears-selection logic goes here

### Integration Points
- `App` struct: add `ClipboardBridge` field, composer `SelectionState` per workspace
- Ctrl+C handler (main.rs:1278): replace clear/stop with copy-if-selection, else no-op
- Ctrl+Q handler (main.rs:1267): extend to work in Composer focus, add two-stage stop→quit
- Composer key handling: intercept Shift+arrows before tui-textarea gets them
- Composer render: inject selection highlight spans like output pane does

</code_context>

<deferred>
## Deferred Ideas

- Tree pane text selection — user wants to copy repo/workspace/branch names. Needs its own phase with selection model for tree items.

</deferred>

---

*Phase: 03-clipboard-keybindings-composer*
*Context gathered: 2026-03-28*
