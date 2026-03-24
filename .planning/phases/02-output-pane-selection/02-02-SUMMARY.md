---
phase: 02-output-pane-selection
plan: 02
subsystem: ui
tags: [ratatui, cursor, selection, keyboard-navigation, word-boundary, auto-scroll, tui]

# Dependency graph
requires:
  - phase: 02-output-pane-selection
    plan: 01
    provides: App selection/cursor state fields, overlay_style_on_line, apply_selection_highlight, apply_cursor_highlight, compute_scroll_from_top, set_scroll_offset
  - phase: 01-coordinate-translation-infrastructure
    provides: WrapMap screen_to_logical/logical_to_screen, SelectionState, ScrollbackBuffer
provides:
  - Full keyboard-driven cursor navigation in output pane (arrow, word jump, page, document)
  - Shift+key selection extension (character, word, document)
  - Ctrl+A select-all
  - Auto-scroll suppression when cursor placed mid-document
  - Selection-clear-on-scroll for mouse wheel and j/k scroll
  - clear_selection_for_workspace helper for cross-module use
  - next_word_boundary / prev_word_boundary free functions
affects: [02-output-pane-selection, 03-clipboard-integration]

# Tech tracking
tech-stack:
  added: []
  patterns: [cursor-init-on-first-keypress, desired-column-memory for vertical movement, output_context helper for WrapMap building]

key-files:
  created: []
  modified:
    - apps/tui/src/main.rs
    - apps/tui/src/mouse.rs

key-decisions:
  - "Cursor initializes to bottom-left of output on first arrow key press (lazy init)"
  - "Sending a user message always re-enables auto-scroll and clears selection"
  - "j/k remain as scroll-only shortcuts that clear selection (not cursor movement)"

patterns-established:
  - "Cursor helper methods on App struct: init_cursor_if_needed, move_cursor_*, extend_selection_*, ensure_cursor_visible"
  - "output_context(ws_id) returns owned lines + inner_width for WrapMap building in key handlers"
  - "Word boundary detection via grapheme iteration: skip-word then skip-whitespace pattern"

requirements-completed: [CURS-01, CURS-02, OSEL-01, OSEL-02, OSEL-05, OSEL-06]

# Metrics
duration: 5min
completed: 2026-03-24
---

# Phase 2 Plan 02: Key Dispatch Summary

**Full cursor navigation with arrow/word/page/document movement, Shift+key selection extension, Ctrl+A select-all, and auto-scroll suppression in output pane**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-24T11:19:55Z
- **Completed:** 2026-03-24T11:25:52Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments
- Rewrote Focus::Output key dispatch from scroll-only to full cursor-based navigation with modifier key detection
- Arrow keys move cursor by visual rows (Up/Down) and characters (Left/Right) with line wrapping
- Ctrl+Left/Right jumps by word boundaries, Home/End navigate to document start/end
- Shift+any of the above extends selection from anchor; Ctrl+A selects all output text
- PageUp/PageDown move cursor and scroll viewport together (VS Code style)
- Auto-scroll suppressed when cursor placed mid-document; re-enabled on user message send or End key
- Mouse wheel scroll and j/k shortcuts clear selection

## Task Commits

Each task was committed atomically:

1. **Task 1: Cursor movement + auto-scroll + initial position** - `968179b` (feat)
2. **Task 2: Selection-clear-on-scroll wiring + auto-scroll suppression** - `3cf68b3` (feat)

## Files Created/Modified
- `apps/tui/src/main.rs` - Added ~700 lines of cursor movement/selection helper methods on App, word boundary free functions, rewrote Focus::Output key dispatch with full modifier key matching
- `apps/tui/src/mouse.rs` - Added clear_selection_for_workspace call in handle_scroll for mouse wheel events

## Decisions Made
- Cursor lazily initializes to bottom-left of visible content on first arrow key press (not on focus change)
- Sending a user message always re-enables auto-scroll and resets scroll to bottom, clearing selection -- explicit user intent to see response
- j/k keys remain as scroll-only shortcuts (not cursor movement) per user decision for editor-style navigation over vim motions
- Desired column memory maintained on vertical movement, updated on horizontal movement -- standard editor behavior

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All keyboard navigation is complete, ready for Plan 03 (mouse click-to-place cursor and drag-to-select)
- clear_selection_for_workspace is pub(crate) and ready for use by mouse handlers
- output_context helper pattern established for building WrapMap in input handlers

---
*Phase: 02-output-pane-selection*
*Completed: 2026-03-24*
