---
phase: 01-coordinate-translation-infrastructure
plan: 01
subsystem: ui
tags: [unicode, word-wrap, coordinate-translation, tdd, ratatui, selection]

# Dependency graph
requires: []
provides:
  - WrapMap struct with build(), screen_to_logical(), logical_to_screen(), total_visual_rows(), extract_text()
  - SelectionState enum with None/Cursor/Range and ordered_range()
  - unicode-segmentation dependency in workspace
affects: [01-02, 02-mouse-selection-rendering, 03-clipboard-integration]

# Tech tracking
tech-stack:
  added: [unicode-segmentation 1.x, arboard 3.6]
  patterns: [grapheme-cluster-based coordinate translation, TDD RED-GREEN for domain logic]

key-files:
  created:
    - apps/tui/src/wrap_map.rs
    - apps/tui/src/selection.rs
  modified:
    - apps/tui/src/main.rs
    - apps/tui/Cargo.toml
    - Cargo.toml

key-decisions:
  - "screen_to_logical and logical_to_screen accept lines parameter rather than storing references, avoiding lifetime complexity"
  - "Character-level word breaks handled at flush time, not during accumulation, matching ratatui WordWrapper behavior"
  - "extract_text uses grapheme indices (not byte offsets) for the public API, converting internally"

patterns-established:
  - "Grapheme-cluster iteration via UnicodeSegmentation::graphemes(true) for all text measurement"
  - "Display width via UnicodeWidthStr::width() for all column calculations"
  - "TDD with RED commit (failing tests) followed by GREEN commit (implementation)"

requirements-completed: [CORD-01, CORD-02, CORD-03]

# Metrics
duration: 7min
completed: 2026-03-23
---

# Phase 1 Plan 01: WrapMap & SelectionState Summary

**WrapMap coordinate translation with ratatui-compatible word wrapping (ASCII/CJK/emoji) plus SelectionState data model, both built with TDD**

## Performance

- **Duration:** 7 min
- **Started:** 2026-03-23T19:49:14Z
- **Completed:** 2026-03-23T19:56:19Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- WrapMap replicates ratatui WordWrapper algorithm with trim=false, supporting ASCII word wrapping, CJK double-width characters, emoji grapheme clusters, and character-level breaks for overlong words
- Bidirectional screen_to_logical / logical_to_screen coordinate translation with scroll offset support
- SelectionState models None/Cursor/Range with ordered_range normalization for reversed selections
- 28 total tests passing (17 WrapMap + 11 SelectionState), zero workspace regressions

## Task Commits

Each task was committed atomically:

1. **Task 1: WrapMap (RED)** - `4f8ecfb` (test) - failing tests for coordinate translation
2. **Task 1: WrapMap (GREEN)** - `aa60bbe` (feat) - full implementation passing all 17 tests
3. **Task 2: SelectionState (RED)** - `669c1b2` (test) - failing tests for selection data model
4. **Task 2: SelectionState (GREEN)** - `16ff81b` (feat) - implementation passing all 11 tests

_TDD tasks have separate RED/GREEN commits_

## Files Created/Modified
- `apps/tui/src/wrap_map.rs` - WrapMap struct with word wrapping and coordinate translation (created)
- `apps/tui/src/selection.rs` - SelectionState enum with ordered range normalization (created)
- `apps/tui/src/main.rs` - Added mod declarations for wrap_map and selection (modified)
- `apps/tui/Cargo.toml` - Added unicode-segmentation dependency (modified)
- `Cargo.toml` - Added unicode-segmentation to workspace deps (modified)

## Decisions Made
- screen_to_logical and logical_to_screen accept a `lines: &[&str]` parameter rather than storing references, avoiding lifetime complexity in the struct
- Character-level word breaks are handled at flush time (not during accumulation) to match ratatui WordWrapper behavior correctly
- extract_text uses grapheme indices in its public API, converting to byte offsets internally

## Deviations from Plan

None - plan executed exactly as written.

## User Setup Required

None - no external service configuration required.

## Issues Encountered
- Long word character-level break initially produced extra rows due to early overlong detection during accumulation. Fixed by deferring character-level breaks to flush_word, where the full word is known.

## Next Phase Readiness
- WrapMap and SelectionState are ready for integration in Plan 01-02 (ClipboardBridge) and Phase 2 (mouse selection rendering)
- No blockers identified

---
*Phase: 01-coordinate-translation-infrastructure*
*Completed: 2026-03-23*
