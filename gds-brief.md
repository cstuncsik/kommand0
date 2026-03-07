# Kommand0 — GSD project brief

## Project type
Brownfield project on an existing Rust workspace.

Current repo shape already exists:
- apps/cli
- apps/tui
- crates/core

Current state:
- project/repo name: kommand0
- CLI binary name: kmd
- a first vertical slice already works:
  - local repo storage in ./.kommand0-dev/state.json
  - CLI supports adding and listing repos
  - TUI loads repos
  - repo selection works
  - pressing Enter runs `git -C <path> status --short --branch`
  - output appears in a right pane
  - q quits cleanly

## Product vision
Kommand0 is a keyboard-first local orchestrator for parallel coding sessions.

It should eventually feel like a lightweight, fast, terminal-first alternative to tools like Conductor for developers who want:
- local execution
- multiple repos
- multiple isolated workspaces
- multiple sessions per workspace
- live output
- strong keyboard UX
- low UI overhead

The design philosophy is:
- terminal-first
- minimal
- fast
- understandable
- local-only to start
- macOS-first initially
- avoid overengineering
- shared logic in crates/core
- keep apps/tui thin

## Target user
Primary user:
- an experienced developer on macOS
- comfortable with terminal workflows
- wants to manage several coding tasks/workspaces in parallel
- values speed, clarity, and keyboard-driven operation

## Non-goals for now
Do NOT build these early unless explicitly required by the roadmap:
- full terminal emulator
- plugin system
- remote execution
- Windows support
- rich graphical diff UI
- AI-provider-specific abstractions beyond what is needed
- enterprise workflow features

## Tech stack
Rust 2024 edition, ratatui, crossterm, clap, tokio, serde, serde_json, thiserror, anyhow, tracing

## Technical constraints
- Rust workspace stays in place
- keep current structure unless a strong reason appears
- simple code is preferred over abstractions
- avoid broad refactors
- state should remain easy to inspect
- persistence can stay simple early on
- keep the TUI responsive under streaming output
- process lifecycle must be reliable: start, stop, cleanup, restart

## Requirements
- REPO-01: User can add a repo via CLI (DONE)
- REPO-02: User can list tracked repos via CLI (DONE)
- REPO-03: User can select a repo in TUI and see git status (DONE)
- WORK-01: User can create a logical workspace from a repo
- WORK-02: User can list and select workspaces in TUI
- WORK-03: Workspaces are persisted in state
- SESS-01: User can run a command in a workspace and see streaming output
- SESS-02: User can stop a running session
- SESS-03: User can restart a stopped session
- SESS-04: App cleans up all child processes on quit
- UX-01: Keyboard-first navigation with consistent bindings
- UX-02: Help overlay showing available keys
- UX-03: Pane navigation between repo list, workspace list, and output
- TREE-01: Optional git worktree integration for workspaces

## Current MVP baseline
Already done:
1. repo registry
2. TUI repo selection
3. git status command execution
4. split-pane output view

This should be treated as the starting point, not rebuilt from scratch.

## Desired architecture direction
Core domain concepts should likely become:
- Repo
- Workspace
- Session
- SessionStatus
- AppState

Likely responsibilities:
- crates/core: models, persistence helpers, execution/domain logic that can be shared
- apps/cli: command entrypoints and thin command handling
- apps/tui: rendering, input handling, view state

Try to preserve this direction unless the codebase strongly suggests a better one.

## Preferred development strategy
Build in small, testable vertical slices.

After each slice:
- keep the code compiling
- keep the UX testable manually
- avoid adding unrelated features
- keep README/testing steps accurate

## Planned milestone sequence

### Milestone 1 — stabilize the current slice
Goal:
- clean up the current implementation
- simplify any unnecessary abstractions
- ensure naming, package boundaries, and README are consistent
- confirm process execution and output handling are reliable

Concrete deliverables:
- review and simplify naming across core, cli, and tui
- ensure package boundaries match the architecture direction
- add basic unit tests for core (state load/save, add_repo validation)
- update README with accurate build/run/test instructions
- confirm git status execution handles edge cases (missing repo, not a git dir)

Definition of done:
- current features still work
- codebase is easier to explain
- unit tests pass for core logic
- no unnecessary complexity remains in the first slice

### Milestone 2 — workspace management
Goal:
- introduce WorkspaceEntry persisted in state
- allow creating a workspace for a selected repo
- allow listing and selecting workspaces in the TUI
- keep the first version simple

Important:
- it is acceptable to start with logical workspaces first
- real git worktree integration can come later
- do not jump to full git worktree support unless clearly justified

Definition of done:
- user can create a workspace from a repo
- workspace appears in persisted state
- TUI can show repo -> workspace relationships
- keyboard interaction remains simple and clean

### Milestone 3 — session execution
Goal:
- run commands in a selected workspace
- show session status
- stream output
- support stop/restart
- keep process cleanup correct

Definition of done:
- user can select a workspace and run a command
- output streams into the UI
- session state is visible
- stopping a session works reliably
- quitting the app does not leave stray processes behind

### Milestone 4 — stronger app state and UX
Goal:
- shape AppState cleanly
- add stable keybindings and help
- improve pane navigation and clarity
- keep background activity from overwhelming the UI

Likely UX ideas:
- j/k or arrows to move
- Enter for primary action
- n to create workspace
- r to run command
- s to stop session
- tab to switch pane
- q to quit
- ? for help

Definition of done:
- interaction model feels coherent
- background state does not create UI confusion
- app remains understandable with multiple items visible

### Milestone 5 — real worktree-aware workflows
Goal:
- evaluate adding real git worktree support
- connect workspaces to branches/tasks where appropriate
- keep behavior predictable and easy to reason about

Definition of done:
- worktree integration, if added, is deliberate and reliable
- UX still stays lightweight
- branch/worktree behavior is documented clearly

## Priorities
Highest priorities:
1. reliability of process lifecycle
2. clean workspace/session domain model
3. keyboard-first TUI clarity
4. responsiveness under multiple sessions
5. simple persistence and debuggability

## Success criteria for the MVP
The MVP is successful if:
- it feels faster and simpler than heavier GUI tools for local orchestration
- it supports a real repo -> workspace -> session flow
- it stays understandable while managing multiple active items
- it gives a strong terminal-native workflow without feeling brittle

## Notes for planning
When creating requirements and roadmap:
- prefer smaller phases over large ambitious ones
- prefer testable vertical slices
- protect the current working baseline
- treat this as an existing codebase, not a blank slate
- avoid speculative architecture
- optimize for shipping a usable local tool on macOS first
