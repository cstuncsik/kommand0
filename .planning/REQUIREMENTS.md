# Requirements: Inline Tree Icon-Buttons

**Defined:** 2026-03-12
**Core Value:** Every workspace action is one click away, visible at a glance in the tree

## v1 Requirements

### Icon Rendering

- [x] **ICON-01**: Each workspace row displays right-aligned icons based on session state (none→▶, idle→▶■, thinking→⠸■, stopped→▶, failed→↺)
- [x] **ICON-02**: Workspace name truncates with ellipsis when icons need space
- [x] **ICON-03**: Icon positions calculated relative to tree pane width (layout-safe)
- [x] **ICON-04**: Icons hidden gracefully when pane is too narrow to fit them

### Click Interaction

- [x] **CLICK-01**: User can click an icon to execute its action (start, stop, focus-composer, retry)
- [x] **CLICK-02**: Each icon hit region carries workspace ID to prevent action-target mismatch
- [x] **CLICK-03**: Hovering an icon shows tooltip with action name

### Visual Polish

- [x] **VIS-01**: Icons highlight cyan on hover (matching existing button style)
- [x] **VIS-02**: Thinking state shows animated braille spinner (not clickable)
- [x] **VIS-03**: Running idle session shows ▶ icon that focuses composer on click

### Bug Fix

- [x] **FIX-01**: Fix `truncate_path` to use char-safe slicing instead of byte offsets

## v2 Requirements

### Context Menu

- **CTX-01**: Right-click on workspace row opens context menu
- **CTX-02**: Context menu includes: start, stop, resume, retry, rename, delete worktree, copy branch name, open in terminal

## Out of Scope

| Feature | Reason |
|---------|--------|
| Drag-to-reorder workspaces | High complexity, not requested |
| Icon customization/theming | Unnecessary complexity for TUI |
| Icons on repo rows | Repos don't have session actions |
| Inline text editing on rows | Confuses click semantics |
| Per-icon keyboard shortcuts | Already have global shortcuts |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| FIX-01 | Phase 1 | Complete |
| ICON-01 | Phase 1 | Complete |
| ICON-02 | Phase 1 | Complete |
| ICON-03 | Phase 1 | Complete |
| ICON-04 | Phase 2 | Complete |
| CLICK-01 | Phase 1 | Complete |
| CLICK-02 | Phase 1 | Complete |
| CLICK-03 | Phase 2 | Complete |
| VIS-01 | Phase 2 | Complete |
| VIS-02 | Phase 2 | Complete |
| VIS-03 | Phase 2 | Complete |

**Coverage:**
- v1 requirements: 11 total
- Mapped to phases: 11
- Unmapped: 0

---
*Requirements defined: 2026-03-12*
*Last updated: 2026-03-12 after roadmap creation*
