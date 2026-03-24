---
phase: 02-output-pane-selection
plan: 01
subsystem: ui
tags: [ratatui, cursor, selection, unicode, span-splitting, tui]

# Dependency graph
requires:
  - phase: 01-coordinate-translation-infrastructure
    provides: WrapMap, SelectionState, ScrollbackBuffer
provides:
  - App struct selection/cursor state fields (selections, cursor_desired_col, cursor_blink_on, auto_scroll_suppressed)
  - overlay_style_on_line grapheme-aware span splitting for highlight overlay
  - apply_selection_highlight for cyan bg/black fg range rendering
  - apply_cursor_highlight for block cursor with blink and focus states
  - compute_scroll_from_top shared scroll offset conversion helper
  - set_scroll_offset on ScrollbackBuffer for direct scroll positioning
affects: [02-output-pane-selection, 03-clipboard-integration]

# Tech tracking
tech-stack:
  added: []
  patterns: [owned-line-collection for borrow-checker resolution, span-splitting overlay pattern]

key-files:
  created: []
  modified:
    - apps/tui/src/main.rs
    - apps/tui/src/scrollback.rs
    - apps/tui/src/render.rs

key-decisions:
  - "Collect scrollback lines into owned Vec<String> to resolve borrow-checker conflict between all_lines and scrollbacks.get_mut"
  - "Cursor and selection highlights operate on pre-wrap styled Lines using logical line indices"

patterns-established:
  - "Span splitting overlay: use overlay_style_on_line for any style overlay on styled Lines"
  - "Scroll offset conversion: use compute_scroll_from_top for bottom-to-top scroll translation"

requirements-completed: [OSEL-04]

# Metrics
duration: 5min
completed: 2026-03-24
---

# Phase 2 Plan 01: Rendering Foundation Summary

**Grapheme-aware span-splitting highlight overlay engine with cursor blink, selection rendering, and App selection state wired into render pipeline**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-24T11:10:57Z
- **Completed:** 2026-03-24T11:16:00Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments
- App struct now carries per-workspace selection state (selections, cursor_desired_col, cursor_blink_on, auto_scroll_suppressed)
- overlay_style_on_line correctly splits spans at grapheme boundaries for CJK, emoji, and mixed-width text
- Cursor renders as white block (focused+blink_on), invisible (focused+blink_off), or dim (unfocused)
- Selection renders as cyan bg/black fg across partial, full, and multi-span ranges
- Both highlights wired into render_right_pane and render_zoomed for normal and zoomed modes
- Cursor blink toggles every ~500ms via existing tick handler

## Task Commits

Each task was committed atomically:

1. **Task 1: App state fields + ScrollbackBuffer setter + scroll helper** - `357f4da` (feat)
2. **Task 2: Highlight overlay engine + render pipeline wiring** - `6a54fbe` (feat)

_Note: TDD tasks have tests committed alongside implementation (RED+GREEN in single commit)_

## Files Created/Modified
- `apps/tui/src/main.rs` - Added selections, cursor_desired_col, cursor_blink_on, auto_scroll_suppressed fields; cursor blink toggle in tick handler; SelectionState import
- `apps/tui/src/scrollback.rs` - Added set_scroll_offset() method for direct scroll positioning
- `apps/tui/src/render.rs` - Added overlay_style_on_line, apply_selection_highlight, apply_cursor_highlight, compute_scroll_from_top; wired highlights into render_right_pane and render_zoomed; refactored line collection to owned Vec for borrow-checker resolution

## Decisions Made
- Collected scrollback lines into owned Vec<String> before building styled lines to avoid borrow-checker conflict (app.scrollbacks borrowed immutably for all_lines, then mutably for clamp_scroll_offset, then immutably again for highlight WrapMap building)
- Selection highlight operates on logical line indices (pre-wrap), not visual rows -- WrapMap parameters are passed but not yet used for visual-row-level precision (will be refined in subsequent plans if needed)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Resolved borrow checker conflict in render pipeline**
- **Found during:** Task 2 (render pipeline wiring)
- **Issue:** all_lines borrows from app.scrollbacks immutably; highlight code uses all_lines after app.scrollbacks.get_mut() for clamp_scroll_offset
- **Fix:** Collected lines into owned Vec<String> before referencing, breaking the borrow chain
- **Files modified:** apps/tui/src/render.rs (render_right_pane, render_zoomed)
- **Verification:** cargo test passes, cargo build succeeds
- **Committed in:** 6a54fbe (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Necessary for compilation. No scope creep. Minimal performance impact (one extra allocation per render frame).

## Issues Encountered
None beyond the borrow-checker resolution documented above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All rendering infrastructure is in place for Plans 02 (keyboard navigation) and 03 (mouse interaction)
- SelectionState fields on App are ready to be populated by key dispatch and mouse handlers
- overlay functions are pub(crate) and tested, ready for use by any module

---
*Phase: 02-output-pane-selection*
*Completed: 2026-03-24*
