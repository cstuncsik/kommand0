# Roadmap: Inline Tree Icon-Buttons

## Overview

This project adds right-aligned action icons to workspace rows in the kommand0 TUI tree pane. Phase 1 delivers the core: icons render correctly per session state, clicks trigger actions, and the unicode truncation bug is fixed. Phase 2 adds interaction polish: hover highlights, animated spinner, tooltip, and graceful narrow-width degradation. Two phases, tightly scoped, shipping a complete inline-icon experience.

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [x] **Phase 1: Core Icon Rendering and Click Handling** - State-dependent icons on workspace rows with clickable hit regions and correct action dispatch
- [ ] **Phase 2: Interaction Polish** - Hover feedback, animated spinner, tooltip, and narrow-width graceful degradation

## Phase Details

### Phase 1: Core Icon Rendering and Click Handling
**Goal**: Users can see and click action icons on each workspace row to control sessions directly from the tree
**Depends on**: Nothing (first phase)
**Requirements**: FIX-01, ICON-01, ICON-02, ICON-03, CLICK-01, CLICK-02
**Success Criteria** (what must be TRUE):
  1. Each workspace row shows the correct icons for its session state (e.g. play for stopped, stop+play for idle, retry for failed)
  2. Clicking an icon triggers the correct action on the correct workspace (not the selected one)
  3. Workspace names truncate with ellipsis when icons need space, without panicking on non-ASCII characters
  4. Icon positions adjust correctly when the tree pane is resized
**Plans**: 2 plans

Plans:
- [x] 01-01-PLAN.md — Fix unicode truncation, create icon cluster function, extend HitAction with workspace-ID variants
- [x] 01-02-PLAN.md — Wire icon cluster into tree renderer with fill-span layout, add click dispatch

### Phase 2: Interaction Polish
**Goal**: Icon interactions feel polished with hover feedback, animation, tooltip, and graceful degradation at narrow widths
**Depends on**: Phase 1
**Requirements**: ICON-04, CLICK-03, VIS-01, VIS-02, VIS-03
**Success Criteria** (what must be TRUE):
  1. Hovering an icon highlights it in cyan and shows a tooltip with the action name
  2. Workspaces in thinking state display an animated braille spinner that cycles with the tick mechanism
  3. Running idle sessions show a composer-focus icon that opens the composer on click
  4. Icons hide gracefully when the tree pane is too narrow to fit them
**Plans**: 2 plans

Plans:
- [ ] 02-01-PLAN.md — Extend icon cluster with thinking/idle states, hover highlights, narrow-width degradation
- [ ] 02-02-PLAN.md — Tooltip rendering with 300ms delay, FocusComposer and ToggleIcons click dispatch

## Progress

**Execution Order:**
Phases execute in numeric order: 1 -> 2

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Core Icon Rendering and Click Handling | 2/2 | Complete | 2026-03-20 |
| 2. Interaction Polish | 0/? | Not started | - |
