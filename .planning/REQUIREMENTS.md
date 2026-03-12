# Requirements: Inline Tree Icon-Buttons

**Defined:** 2026-03-12
**Core Value:** Every workspace action is one click away, visible at a glance in the tree

## v1 Requirements

### Icon Rendering

- [ ] **ICON-01**: Each workspace row displays right-aligned icons based on session state (none→▶, idle→▶■, thinking→⠸■, stopped→▶, failed→↺)
- [ ] **ICON-02**: Workspace name truncates with ellipsis when icons need space
- [ ] **ICON-03**: Icon positions calculated relative to tree pane width (layout-safe)
- [ ] **ICON-04**: Icons hidden gracefully when pane is too narrow to fit them

### Click Interaction

- [ ] **CLICK-01**: User can click an icon to execute its action (start, stop, focus-composer, retry)
- [ ] **CLICK-02**: Each icon hit region carries workspace ID to prevent action-target mismatch
- [ ] **CLICK-03**: Hovering an icon shows tooltip with action name

### Visual Polish

- [ ] **VIS-01**: Icons highlight cyan on hover (matching existing button style)
- [ ] **VIS-02**: Thinking state shows animated braille spinner (not clickable)
- [ ] **VIS-03**: Running idle session shows ▶ icon that focuses composer on click

### Bug Fix

- [ ] **FIX-01**: Fix `truncate_path` to use char-safe slicing instead of byte offsets

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
| FIX-01 | — | Pending |
| ICON-01 | — | Pending |
| ICON-02 | — | Pending |
| ICON-03 | — | Pending |
| ICON-04 | — | Pending |
| CLICK-01 | — | Pending |
| CLICK-02 | — | Pending |
| CLICK-03 | — | Pending |
| VIS-01 | — | Pending |
| VIS-02 | — | Pending |
| VIS-03 | — | Pending |

**Coverage:**
- v1 requirements: 11 total
- Mapped to phases: 0
- Unmapped: 11 ⚠️

---
*Requirements defined: 2026-03-12*
*Last updated: 2026-03-12 after initial definition*
