# Roadmap: Kommand0

## Overview

Kommand0 is a brownfield Rust TUI project with a working vertical slice (repo registry, git status, split-pane display). This milestone transforms it from a read-only viewer into a functioning process orchestrator. The path is: stabilize the foundation and migrate to async (protecting the baseline), build the workspace domain model, deliver session execution (the core value), then polish keyboard UX. Each phase delivers a verifiable capability that unblocks the next.

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [ ] **Phase 1: Stabilization and Async Foundation** - Harden the codebase and migrate the TUI event loop to async tokio
- [ ] **Phase 2: Workspace Model** - Users can create, list, and navigate workspaces tied to repos
- [ ] **Phase 3: Session Execution** - Users can run commands in workspaces with live output and full process lifecycle
- [ ] **Phase 4: UX Polish** - Keyboard navigation, help overlay, pane focus, and zoomed output view

## Phase Details

### Phase 1: Stabilization and Async Foundation
**Goal**: The existing codebase is safe, tested, and running on an async event loop ready for process management
**Depends on**: Nothing (first phase)
**Requirements**: STAB-01, STAB-02, STAB-03, STAB-04, STAB-05, STAB-06, STAB-07
**Success Criteria** (what must be TRUE):
  1. Running `cargo test` executes unit tests for core logic (state persistence, repo validation, git status edge cases) and they pass
  2. TUI event loop uses `tokio::select!` with crossterm `EventStream` -- the app renders and responds to keyboard input without blocking
  3. If the app panics, the terminal is restored to normal state (raw mode disabled, alternate screen exited)
  4. README contains accurate build, run, and test instructions that a developer can follow from a fresh clone
  5. Naming across core/cli/tui is consistent -- no references to stale or misleading identifiers
**Plans**: TBD

Plans:
- [ ] 01-01: TBD
- [ ] 01-02: TBD
- [ ] 01-03: TBD

### Phase 2: Workspace Model
**Goal**: Users can create logical workspaces from repos and navigate them in both CLI and TUI
**Depends on**: Phase 1
**Requirements**: WORK-01, WORK-02, WORK-03, WORK-04, WORK-05
**Success Criteria** (what must be TRUE):
  1. User can run `kmd workspace create <name> --repo <path>` and see the workspace in `kmd workspace list`
  2. User can see workspaces in the TUI, select one, and see which repo it belongs to
  3. Workspaces survive app restart (persisted in state.json, loadable on next launch)
  4. TUI shows the repo-to-workspace relationship (which workspaces belong to which repo)
**Plans**: TBD

Plans:
- [ ] 02-01: TBD
- [ ] 02-02: TBD

### Phase 3: Session Execution
**Goal**: Users can run commands in workspaces, see streaming output, and manage process lifecycle
**Depends on**: Phase 2
**Requirements**: SESS-01, SESS-02, SESS-03, SESS-04, SESS-05, SESS-06
**Success Criteria** (what must be TRUE):
  1. User can select a workspace in TUI, run a command, and see its stdout/stderr streaming live in the output pane
  2. User can stop a running session and the process (plus all its children) is terminated -- no zombie processes remain
  3. User can restart a previously stopped session and see fresh output streaming
  4. Quitting the app cleans up all child processes -- `ps` shows no orphaned children after exit
  5. Each session shows a visible status indicator (running/stopped/failed/exited) in the TUI
**Plans**: TBD

Plans:
- [ ] 03-01: TBD
- [ ] 03-02: TBD
- [ ] 03-03: TBD

### Phase 4: UX Polish
**Goal**: The TUI has consistent keyboard navigation, contextual help, focused pane management, and a zoomed output view
**Depends on**: Phase 3
**Requirements**: UX-01, UX-02, UX-03, UX-04
**Success Criteria** (what must be TRUE):
  1. User can navigate between repo list, workspace list, and output panes using Tab/Shift-Tab with a visible focus indicator
  2. User can press `?` to see a help overlay showing all available keys for the current context
  3. User can press a key to zoom the output pane to full screen and press again to return to split view
  4. Navigation bindings are consistent: j/k or arrows for list movement, Enter to select, Esc to go back
**Plans**: TBD

Plans:
- [ ] 04-01: TBD
- [ ] 04-02: TBD

## Progress

**Execution Order:**
Phases execute in numeric order: 1 -> 2 -> 3 -> 4

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Stabilization and Async Foundation | 0/3 | Not started | - |
| 2. Workspace Model | 0/2 | Not started | - |
| 3. Session Execution | 0/3 | Not started | - |
| 4. UX Polish | 0/2 | Not started | - |
