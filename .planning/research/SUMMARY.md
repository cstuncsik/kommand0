# Project Research Summary

**Project:** Inline Tree Icon-Buttons for kommand0 TUI
**Domain:** TUI rendering / mouse interaction enhancement (Rust, ratatui)
**Researched:** 2026-03-12
**Confidence:** HIGH

## Executive Summary

This project adds right-aligned action icons (start, stop, focus-composer, retry, spinner) to workspace rows in the kommand0 TUI tree pane. Research confirms that the existing stack — ratatui 0.29 and crossterm 0.28 — provides everything needed with zero new dependencies. The core technique is calculated fill-span positioning within `Line` items, combined with the existing `HitRegion`/`HitAction` system for click handling. This is a rendering-only change that touches primarily `render.rs` and `buttons.rs`.

The recommended approach centers on a single pure function (`workspace_icon_cluster`) that maps session state to spans and hit regions. The existing mouse infrastructure (hover tracking, hit region iteration, `pending_button_action` dispatch) handles the rest. The build order is clear: define HitAction variants first, implement the icon cluster function, integrate into tree rendering, wire up click dispatch, then polish with spinner animation and hover highlights.

The highest-risk pitfall is action-target mismatch: clicking an icon on an unselected row could act on the wrong workspace if `HitAction` variants do not carry the workspace ID. This must be designed correctly from the start. Secondary risks include unicode width miscalculation (the existing `truncate_path` function panics on non-ASCII) and the inherent limitation that `Line` only supports single-direction alignment. Both have clear, well-understood solutions.

## Key Findings

### Recommended Stack

No new dependencies required. The existing ratatui 0.29 + crossterm 0.28 stack provides all necessary APIs.

**Core technologies:**
- **ratatui `Buffer::set_span()`**: Direct buffer writes for precise icon positioning — standard approach for mixed-alignment content
- **`ListState::offset()`**: Scroll offset tracking — ensures icon overlay positions match visible rows
- **Existing `HitRegion`/`HitAction` system**: Click infrastructure — extend with new variants, no new patterns needed
- **`unicode_width::UnicodeWidthStr`**: Display width calculation — prevents coordinate drift between rendering and hit regions

### Expected Features

**Must have (table stakes):**
- State-dependent icon visibility (different icons per session state)
- Click-to-act (click icon triggers action)
- Hover highlight (cyan, matching existing button style)
- Workspace name truncation when icons need space
- Layout-safe positioning relative to tree pane width

**Should have (differentiators):**
- Animated braille spinner for thinking state
- Focus-composer icon on running sessions
- Graceful narrow-width degradation (hide icons if pane too narrow)

**Defer (v2+):**
- Right-click context menu
- Tooltip on hover (action name display)
- Drag-to-reorder workspaces
- Icon customization/theming

### Architecture Approach

No new abstractions needed. One pure function (`workspace_icon_cluster`) maps session state to spans and hit regions. It integrates into the existing `render_tree` function via calculated fill spans. Click dispatch reuses the existing `pending_button_action` handler in `main.rs` with new `HitAction` match arms. The key architectural decision is that `HitAction` variants must carry `workspace_id` to prevent action-target mismatch.

**Major components:**
1. **Icon cluster function** (`render.rs`) — Pure function: session state -> spans + hit regions
2. **HitAction variants** (`buttons.rs`) — `FocusComposer`, `RetrySession`, `StopSession`, `StartSession` with workspace IDs
3. **Click dispatch** (`main.rs`) — New match arms in the existing `pending_button_action` handler
4. **Tree row integration** (`render.rs`) — Fill-span positioning within `render_tree`

### Critical Pitfalls

1. **Action-target mismatch** — Include `workspace_id` in every icon `HitAction` variant. Never rely on selected index for icon clicks.
2. **Hit region coordinate drift** — Use `unicode_width` for all width calculations. Compute hit region positions from the same values used for rendering.
3. **`truncate_path` panics on non-ASCII** — Replace byte slicing with `char_indices()` before adding icon-driven truncation.
4. **`Line` single-alignment limitation** — Use calculated fill spans (padding between name and icons), not alignment properties.
5. **Stale hit regions after scroll** — Always rebuild icon hit regions during render. Never cache across frames.

## Implications for Roadmap

Based on research, suggested phase structure:

### Phase 1: Core Icon Rendering and Click Handling

**Rationale:** All other phases depend on icons being rendered correctly and clickable. The action-target mismatch pitfall and unicode truncation bug must be fixed here — they cannot be deferred.
**Delivers:** Clickable, state-dependent icons on workspace rows with correct action dispatch.
**Addresses:** State-dependent icon visibility, click-to-act, layout-safe positioning, workspace name truncation.
**Avoids:** Action-target mismatch (workspace ID in HitAction), hit region coordinate drift (unicode width), truncate_path panic (char_indices fix).

Steps within this phase (ordered by dependency):
1. Fix `truncate_path` to use `char_indices()` instead of byte slicing
2. Add `HitAction` variants with `workspace_id` field
3. Implement `workspace_icon_cluster()` pure function
4. Integrate into `render_tree` with fill-span positioning
5. Add match arms in `pending_button_action` dispatch

### Phase 2: Interaction Polish

**Rationale:** Once icons render and click correctly, add hover feedback and animation. These are low-risk, low-complexity additions that use existing infrastructure.
**Delivers:** Hover highlighting, animated spinner, narrow-width graceful degradation.
**Addresses:** Hover highlight, animated braille spinner, focus-composer icon, narrow-width degradation.
**Avoids:** Terminal font rendering issues (test unicode glyphs, prepare ASCII fallback).

### Phase 3: Detail Pane Cleanup

**Rationale:** With inline icons working, the detail pane action buttons become redundant. Remove or simplify them. This is a cleanup phase that should only happen after inline icons are validated.
**Delivers:** Streamlined detail pane — retains session info, removes redundant action buttons.
**Addresses:** Detail pane button redundancy noted in PROJECT.md.
**Avoids:** Premature removal of buttons before inline icons are proven.

### Phase Ordering Rationale

- Phase 1 must come first because every subsequent phase depends on correct icon rendering and click dispatch. The critical pitfalls (action-target mismatch, unicode bugs) are all Phase 1 concerns.
- Phase 2 is low-risk polish that builds on Phase 1 infrastructure. Hover uses existing `mouse_pos` tracking. Spinner uses existing tick mechanism.
- Phase 3 is deliberately last because removing existing buttons should only happen after inline icons are working and tested.

### Research Flags

Phases likely needing deeper research during planning:
- **Phase 1:** Needs careful implementation research for the fill-span width calculation. The interaction between tree indent depth, icon cluster width, and available name width requires precise arithmetic.

Phases with standard patterns (skip research-phase):
- **Phase 2:** Hover highlighting and spinner animation use well-documented existing patterns in the codebase.
- **Phase 3:** Straightforward button removal, no research needed.

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | No new dependencies; all APIs verified in ratatui 0.29 |
| Features | HIGH | Clear table-stakes vs differentiators; dependency chain well-mapped |
| Architecture | HIGH | Additive change, existing patterns, one new pure function |
| Pitfalls | HIGH | Critical pitfalls identified with concrete prevention strategies |

**Overall confidence:** HIGH

### Gaps to Address

- **Unicode glyph rendering across terminals**: Icons (triangle, square, braille) may render differently in iTerm2 vs Terminal.app vs other emulators. Needs manual testing in Phase 2. Have ASCII fallback ready.
- **Performance with large workspace counts**: Two-pass rendering overhead is theoretically a concern with 50+ workspaces. Likely fine in practice but should be monitored. Only optimize if measured.
- **Fill-span width arithmetic**: The exact calculation (pane width - tree indent - prefix icons - name width - icon cluster width) involves multiple variables. Getting this right requires careful attention to off-by-one errors during Phase 1 implementation.

## Sources

### Primary (HIGH confidence)
- Existing codebase: `render.rs`, `buttons.rs`, `main.rs`, `mouse.rs` — direct inspection of current patterns
- ratatui 0.29 API: `Buffer::set_span()`, `ListState::offset()`, `Line` alignment behavior

### Secondary (MEDIUM confidence)
- ratatui community patterns for mixed-alignment content (buffer overlay, fill spans)
- `unicode_width` crate for display width calculation

---
*Research completed: 2026-03-12*
*Ready for roadmap: yes*
