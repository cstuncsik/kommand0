# Requirements: Kommand0

**Defined:** 2026-03-07
**Core Value:** Reliable process lifecycle management for parallel coding sessions from a fast keyboard-driven TUI

## v1 Requirements

Requirements for Milestone 1 (current). Each maps to roadmap phases.

### Stabilization

- [x] **STAB-01**: Codebase naming is consistent across core, cli, and tui (no misleading names)
- [x] **STAB-02**: Package boundaries match architecture direction (domain logic in core, thin apps)
- [x] **STAB-03**: Unit tests exist for core logic (state load/save, add_repo validation)
- [x] **STAB-04**: README has accurate build/run/test instructions
- [x] **STAB-05**: Git status execution handles edge cases (missing repo, not a git dir)
- [x] **STAB-06**: Panic hook restores terminal state on crash
- [x] **STAB-07**: TUI event loop migrated to async (tokio + crossterm event-stream)

### Workspace

- [x] **WORK-01**: User can create a logical workspace from a repo via CLI
- [x] **WORK-02**: User can list workspaces via CLI
- [ ] **WORK-03**: User can list and select workspaces in TUI
- [x] **WORK-04**: Workspaces are persisted in state.json
- [ ] **WORK-05**: TUI shows repo -> workspace relationships

### Session

- [ ] **SESS-01**: User can run a command in a workspace and see streaming output
- [ ] **SESS-02**: User can stop a running session (SIGTERM with SIGKILL fallback)
- [ ] **SESS-03**: User can restart a stopped session
- [ ] **SESS-04**: App cleans up all child processes on quit (process group management)
- [ ] **SESS-05**: Process status indicators visible in TUI (running/stopped/failed/exited)
- [ ] **SESS-06**: Output scrollback buffer with configurable capacity

### UX

- [ ] **UX-01**: Keyboard-first navigation with consistent bindings (j/k, arrows, Enter, Tab)
- [ ] **UX-02**: Help overlay showing available keys for current context
- [ ] **UX-03**: Pane navigation between repo list, workspace list, and output
- [ ] **UX-04**: Focused/zoomed output view (full-screen single session)

## v2 Requirements

Deferred to future milestones. Tracked but not in current roadmap.

### Worktree

- **TREE-01**: User can create workspace backed by git worktree for isolated branch work
- **TREE-02**: User can list and manage git worktrees through TUI

### Advanced Session

- **ASESS-01**: Session templates / presets (reusable command sets per workspace)
- **ASESS-02**: Auto-restart policies (never, on-failure, always with backoff)
- **ASESS-03**: Workspace-scoped environment variables
- **ASESS-04**: Session command history (last N commands per workspace)
- **ASESS-05**: Copy mode for selecting and copying text from output buffer
- **ASESS-06**: Multi-session per workspace (run multiple commands, switch between outputs)

## Out of Scope

| Feature | Reason |
|---------|--------|
| Full terminal emulator / PTY | Massive complexity; stdout/stderr capture sufficient |
| Plugin / extension system | Premature abstraction before core is stable |
| Remote execution | Breaks local-only, fast, simple philosophy |
| Process dependency graphs | Enterprise complexity not needed for dev tool |
| Health checks / readiness probes | PID alive + exit code is sufficient |
| REST API / remote control | Adds attack surface for a local tool |
| Config file format (YAML/TOML) | JSON state file is single source of truth |
| Windows support | macOS-first per project constraints |
| AI/LLM integration | Keep agent-agnostic; processes can be AI agents |
| Multiplayer / collaboration | Single-user local tool |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| STAB-01 | Phase 1 | Complete |
| STAB-02 | Phase 1 | Complete |
| STAB-03 | Phase 1 | Complete |
| STAB-04 | Phase 1 | Complete |
| STAB-05 | Phase 1 | Complete |
| STAB-06 | Phase 1 | Complete |
| STAB-07 | Phase 1 | Complete |
| WORK-01 | Phase 2 | Complete |
| WORK-02 | Phase 2 | Complete |
| WORK-03 | Phase 2 | Pending |
| WORK-04 | Phase 2 | Complete |
| WORK-05 | Phase 2 | Pending |
| SESS-01 | Phase 3 | Pending |
| SESS-02 | Phase 3 | Pending |
| SESS-03 | Phase 3 | Pending |
| SESS-04 | Phase 3 | Pending |
| SESS-05 | Phase 3 | Pending |
| SESS-06 | Phase 3 | Pending |
| UX-01 | Phase 4 | Pending |
| UX-02 | Phase 4 | Pending |
| UX-03 | Phase 4 | Pending |
| UX-04 | Phase 4 | Pending |

**Coverage:**
- v1 requirements: 22 total
- Mapped to phases: 22
- Unmapped: 0 ✓

---
*Requirements defined: 2026-03-07*
*Last updated: 2026-03-07 after initial definition*
