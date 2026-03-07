# Kommand0

## What This Is

Kommand0 is a keyboard-first local orchestrator for parallel coding sessions. It is a lightweight, fast, terminal-first tool for developers who want to manage multiple repos, workspaces, and sessions with live output and strong keyboard UX. macOS-first, local-only.

## Core Value

Reliable process lifecycle management for parallel coding sessions — start, stop, stream output, and clean up child processes — all from a fast keyboard-driven TUI.

## Requirements

### Validated

- ✓ REPO-01: User can add a repo via CLI — existing
- ✓ REPO-02: User can list tracked repos via CLI — existing
- ✓ REPO-03: User can select a repo in TUI and see git status — existing

### Active

- [ ] Stabilize and clean up the current implementation (naming, boundaries, tests, README)
- [ ] User can create a logical workspace from a repo
- [ ] User can list and select workspaces in TUI
- [ ] Workspaces are persisted in state
- [ ] User can run a command in a workspace and see streaming output
- [ ] User can stop a running session
- [ ] User can restart a stopped session
- [ ] App cleans up all child processes on quit
- [ ] Keyboard-first navigation with consistent bindings
- [ ] Help overlay showing available keys
- [ ] Pane navigation between repo list, workspace list, and output
- [ ] Optional git worktree integration for workspaces

### Out of Scope

- Full terminal emulator — too complex, not needed for MVP
- Plugin system — premature abstraction
- Remote execution — local-only to start
- Windows support — macOS-first initially
- Rich graphical diff UI — out of scope for terminal tool
- AI-provider-specific abstractions — not needed yet
- Enterprise workflow features — not the target user

## Context

- Brownfield project with existing Rust workspace: apps/cli, apps/tui, crates/core
- CLI binary name: `kmd`, TUI binary: `kommand0-tui`
- State persisted as JSON in `.kommand0-dev/state.json`
- Current vertical slice works: repo registry, TUI selection, git status, split-pane output
- tokio declared but not yet used (needed for async session management)
- tracing and thiserror declared but not yet used
- No tests exist yet
- Target user: experienced macOS developer comfortable with terminal workflows

## Constraints

- **Tech stack**: Rust 2024 edition, ratatui, crossterm, clap, tokio, serde, serde_json, thiserror, anyhow, tracing
- **Architecture**: Shared logic in crates/core, thin apps/cli and apps/tui
- **Persistence**: Simple JSON file-backed state, easy to inspect
- **Platform**: macOS-first, terminal with raw mode support, git on PATH
- **Philosophy**: Simple code over abstractions, avoid broad refactors, small testable vertical slices

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Rust workspace with core/cli/tui split | Shared domain logic, thin frontends | — Pending |
| JSON file persistence | Simple, inspectable, sufficient for local tool | — Pending |
| Logical workspaces before git worktrees | Reduce complexity, ship workspace UX first | — Pending |
| tokio for async session execution | Needed for streaming output and process management | — Pending |
| Coarse milestone sequence (stabilize → workspaces → sessions → UX → worktrees) | Small testable slices, protect existing baseline | — Pending |

---
*Last updated: 2026-03-07 after initialization*
