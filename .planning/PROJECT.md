# TUI Text Selection & Clipboard

## What This Is

Text selection and clipboard copy for the kommand0 TUI application. Adds mouse drag selection, keyboard selection (Shift+arrows, Ctrl+A), and system clipboard copy (Ctrl+C / Cmd+C) to both the output pane and composer. This is a brownfield addition to an existing Rust TUI built on ratatui + crossterm + tui-textarea.

## Core Value

Users can select any text visible in the TUI and copy it to the system clipboard, matching the selection UX they expect from any terminal or text editor.

## Requirements

### Validated

- ✓ Output pane renders session output with markdown styling — existing
- ✓ Composer accepts text input via tui-textarea — existing
- ✓ Mouse events (click, drag, move) are partially handled — existing
- ✓ Keyboard events dispatched by focus (Tree, Output, Composer) — existing

### Active

- [ ] Mouse drag selection in output pane
- [ ] Mouse drag selection in composer
- [ ] Keyboard cursor navigation in output pane (arrow keys, blinking cursor)
- [ ] Keyboard selection in output pane (Shift+arrows, Shift+Home/End)
- [ ] Ctrl+A selects all in focused pane
- [ ] Selection highlight: cyan background, black text
- [ ] Ctrl+C / Cmd+C copies selection to system clipboard (when selection exists)
- [ ] Ctrl+C with no selection does nothing (ignore)
- [ ] Ctrl+Q stops current session (replaces Ctrl+C's old stop role)
- [ ] Composer selection via tui-textarea built-in support
- [ ] Coordinate translation: screen (x,y) to text position (line, char) accounting for wrapping
- [ ] Cross-platform clipboard via arboard crate

### Out of Scope

- Paste from clipboard (Ctrl+V) — defer to future work
- Selection across panes (output + composer) — too complex, no clear UX
- Right-click context menu — not standard in TUI apps
- Multi-cursor / block selection — overkill for v1
- Selection persistence across scroll — selection clears on scroll for simplicity

## Context

- **Existing TUI**: ratatui 0.29 + crossterm 0.28 + tui-textarea 0.7
- **Output rendering**: `build_output_lines()` in `render.rs` creates `Vec<Line<'static>>` with markdown styling. ratatui Paragraph handles wrapping internally but doesn't expose wrap state.
- **Scrollback**: `VecDeque<String>` in `scrollback.rs`, tracks `scroll_offset` (lines from bottom). No cursor or selection state currently.
- **Mouse handling**: `mouse.rs` handles MouseDown(Left) and Drag(Left) but NOT MouseUp. MouseUp needed for selection end.
- **Key handling**: `main.rs` dispatches by focus. Ctrl+C currently clears composer or stops session. Shift modifiers available but unused.
- **Coordinate translation challenge**: Mapping screen x,y to logical text position requires building a line-wrap map during rendering, accounting for border padding, scroll offset, wrapping, unicode width, and markdown spans.
- **tui-textarea**: v0.7 may have built-in selection support (set_selection_style, copy, Shift+arrow). Needs investigation during implementation.

## Constraints

- **Tech stack**: Must use ratatui + crossterm + tui-textarea (existing stack)
- **Clipboard**: Use `arboard` crate for cross-platform support
- **Performance**: Line-wrap map must only cover visible lines (not entire 50k+ scrollback)
- **Backward compat**: Ctrl+C behavior change requires clear communication. Ctrl+Q replaces stop-session.
- **Rendering approach**: Selection highlight via span re-styling (approach 1 from research) — more correct than overlay for wrapped lines

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Ctrl+C = copy (ignore when no selection) | Standard copy shortcut users expect | — Pending |
| Ctrl+Q = stop session | Clean alternative, not conflicting with common shortcuts | — Pending |
| arboard over pbcopy | Cross-platform from the start | — Pending |
| Span re-styling for selection highlight | Handles wrapped lines correctly, unlike overlay approach | — Pending |
| Selection clears on scroll | Simpler implementation, avoids stale selection coordinates | — Pending |

---
*Last updated: 2026-03-22 after initialization*
