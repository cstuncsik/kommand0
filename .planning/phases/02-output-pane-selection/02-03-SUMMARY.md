---
phase: 02-output-pane-selection
plan: 03
subsystem: ui
tags: [ratatui, mouse, selection, click-to-place, drag-to-select, tui, crossterm]

# Dependency graph
requires:
  - phase: 02-output-pane-selection
    plan: 01
    provides: App selection/cursor state fields, overlay_style_on_line, apply_selection_highlight, compute_scroll_from_top
  - phase: 01-coordinate-translation-infrastructure
    provides: WrapMap screen_to_logical, SelectionState, ScrollbackBuffer
provides:
  - Mouse click-to-place cursor in output pane
  - Mouse drag-to-select with real-time cyan highlight
  - MouseUp selection finalization
  - Streaming text inclusion in coordinate translation during active streaming
affects: [03-clipboard-integration]

# Tech tracking
tech-stack:
  added: []
  patterns: [collect-output-lines helper for borrow-safe WrapMap building in mouse handler]

key-files:
  created: []
  modified:
    - apps/tui/src/mouse.rs
    - apps/tui/src/main.rs

key-decisions:
  - "streaming_text made pub(crate) so mouse handler can include partial lines in coordinate translation"
  - "Click always clears existing selection and places cursor -- standard editor behavior"

patterns-established:
  - "Mouse coordinate translation: subtract pane origin + border to get inner coords, then screen_to_logical"
  - "Drag selection: first drag from Cursor creates Range with anchor at cursor; subsequent drags update cursor end"

requirements-completed: [OSEL-03]

# Metrics
duration: 7min
completed: 2026-03-28
---

# Phase 2 Plan 03: Mouse Interaction Summary

**Mouse click-to-place cursor and drag-to-select with real-time cyan highlight in the output pane, verified across Ghostty and iTerm2**

## Performance

- **Duration:** 7 min (execution) + user verification checkpoint
- **Started:** 2026-03-24T11:21:00Z
- **Completed:** 2026-03-28T07:36:15Z (including checkpoint approval delay)
- **Tasks:** 2 (1 auto + 1 human-verify checkpoint)
- **Files modified:** 2

## Accomplishments
- Click in output pane places blinking cursor at the clicked character position via WrapMap::screen_to_logical
- Click clears any existing selection; click on empty space past line end snaps to end of line
- Mouse drag extends selection from click anchor position to current drag position in real-time with cyan bg/black fg highlight
- Streaming text included in coordinate translation so click positions are accurate during active streaming
- All 16 verification steps passed in manual checkpoint (Ghostty, iTerm2)

## Task Commits

Each task was committed atomically:

1. **Task 1: Mouse click-to-place + drag-to-select + MouseUp** - `df1d5e6` (feat)
2. **Task 2: Visual verification checkpoint** - approved by user (no code changes)

## Files Created/Modified
- `apps/tui/src/mouse.rs` - Added click-to-place cursor in output pane (screen_to_logical translation), drag-to-select with Range state management, MouseUp no-op finalization, collect_output_lines helper for borrow-safe line collection
- `apps/tui/src/main.rs` - Made streaming_text field pub(crate) for cross-module access in mouse handler

## Decisions Made
- streaming_text made pub(crate) so mouse.rs can append partial streaming lines to the WrapMap input, ensuring accurate click coordinates during active streaming
- Click always clears existing selection and places cursor at clicked position -- standard editor behavior matching CONTEXT.md decisions

## Deviations from Plan

None - plan executed exactly as written.

## Known Limitations

**Shift+Up/Down in Zed terminal:** Shift+arrow key events for Up/Down are not forwarded by Zed's built-in terminal emulator. This is a Zed terminal limitation, not a bug in our code. Shift+Left/Right selection works correctly in Zed. All Shift+arrow combinations work correctly in Ghostty, iTerm2, and other standard terminal emulators.

## Issues Encountered
None.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Phase 2 is now complete: cursor navigation, keyboard selection, mouse selection, and highlight rendering are all functional
- SelectionState is populated and ready for Phase 3 clipboard copy (extract_text from ordered_range)
- ClipboardBridge from Phase 1 is ready to wire into Ctrl+C handler

---
*Phase: 02-output-pane-selection*
*Completed: 2026-03-28*
