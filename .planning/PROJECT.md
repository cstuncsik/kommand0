# Inline Tree Icon-Buttons

## What This Is

Add inline action icons (start, stop, resume, retry) right-aligned on each workspace row in the kommand0 TUI tree pane. Replaces scattered button placements with a consistent, discoverable mouse interaction pattern directly in the tree. The TUI is a Rust terminal app built on ratatui/crossterm that manages Claude Code sessions across git worktrees.

## Core Value

Every workspace action is one click away, visible at a glance in the tree — no navigation to a detail pane required.

## Requirements

### Validated

- ✓ Tree pane displays repos and workspaces — existing
- ✓ Mouse click selection on tree rows — existing
- ✓ Session lifecycle (start/stop/resume) via keyboard — existing
- ✓ Hit region system for clickable UI elements — existing
- ✓ Animated braille spinner for thinking state — existing (tick mechanism)
- ✓ Workspace detail pane shows session info — existing

### Active

- [ ] Right-aligned inline icons on each workspace row based on session state
- [ ] Clickable icons with hit regions for start, stop, focus-composer, retry
- [ ] Animated spinner icon (braille frames) for thinking state (display-only, not clickable)
- [ ] Workspace name truncation when icons need space
- [ ] Icon highlight on hover (cyan, matching existing button style)
- [ ] Layout-safe icon positioning relative to tree pane width
- [ ] Detail pane retains workspace info but action buttons become redundant

### Out of Scope

- Right-click context menu with extended actions (rename, delete worktree, copy branch, open terminal) — v2, after inline icons ship
- Drag-to-reorder workspaces — not planned
- Icon customization or theming — unnecessary complexity

## Context

- The TUI already has a `HitAction` enum and `hit_regions` vec for mouse interaction
- `render_tree` in `render.rs` builds tree rows; this is the main file to modify
- Session state is accessible via `app.state.find_session_by_workspace`
- The existing `[Start Session]` button in the detail pane and `[Resume]` button in the composer area become redundant but detail pane info stays
- Tree pane width is `app.pane_areas.tree.width`; icons go at `tree.x + tree.width - icon_cluster_width - 1`

## Constraints

- **Tech stack**: Rust, ratatui 0.29, crossterm 0.28 — no new dependencies
- **Architecture**: Keep TUI thin, domain logic in core crate — icon rendering is purely TUI concern
- **Layout**: Icons must be relative to tree pane width, not hardcoded positions
- **Compatibility**: Keyboard shortcuts unchanged — this is additive mouse UX only

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Icons in tree rows, not separate column | Keeps tree compact, familiar pattern from file managers | — Pending |
| Animated braille spinner for thinking | Already have tick mechanism, consistent with existing UX | — Pending |
| Detail pane keeps info, loses action buttons | Icons make buttons redundant but info (session ID, status) still useful | — Pending |
| Right-click context menu deferred to v2 | Ship inline icons first, validate the pattern, then extend | — Pending |

---
*Last updated: 2026-03-12 after initialization*
