# Codebase Structure

**Analysis Date:** 2026-03-22

## Directory Layout

```
kommand0/
├── apps/                          # Frontend applications
│   ├── cli/                       # CLI application (kmd binary)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   └── tui/                       # Terminal UI application
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs            # Event loop, app state
│           ├── render.rs          # Frame rendering (layout, tree, panes)
│           ├── session_manager.rs # Claude process spawning & streaming
│           ├── scrollback.rs      # Output buffer with scroll tracking
│           ├── composer.rs        # Message input widget
│           ├── modal.rs           # Add repo/workspace modals
│           ├── buttons.rs         # Clickable button regions & hit detection
│           ├── mouse.rs           # Mouse event handling
│           ├── help.rs            # Help overlay rendering
│           └── buttons.rs         # Button rendering and interaction
├── crates/                        # Shared libraries
│   └── core/                      # Core domain logic
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs             # AppState (repo/workspace/session mgmt)
│           ├── repo.rs            # RepoEntry struct, git status
│           ├── workspace.rs       # Workspace struct, timestamp formatting
│           ├── session.rs         # Session struct, SessionStatus enum
│           ├── worktree.rs        # Git worktree creation/removal
│           └── id.rs              # ID generation helper
├── Cargo.toml                     # Workspace root (members, dependencies)
├── Cargo.lock
├── README.md
└── CLAUDE.md                      # Project instructions

# Data directories (created at runtime)
.kommand0-dev/
├── state.json                     # Persistent app state (repos, workspaces, sessions)
├── worktrees/                     # Git worktree directories (per workspace)
│   └── <workspace-name>/          # Checked out git worktree
├── sessions/                      # Session logs
│   └── <session-id>.log           # JSON lines log file
└── logs/                          # (Legacy, may be used in some contexts)
```

## Directory Purposes

**`apps/`:**
- Purpose: Frontend binaries that consume `kommand0-core`
- Contains: CLI and TUI application logic
- Key files: `apps/cli/src/main.rs`, `apps/tui/src/main.rs`

**`apps/cli/`:**
- Purpose: Command-line interface for managing repos, workspaces, and sessions
- Contains: Clap argument parsing, CLI command handlers
- Key files: `apps/cli/src/main.rs` (single ~413-line file with all commands)

**`apps/tui/`:**
- Purpose: Interactive terminal UI with real-time session streaming
- Contains: Event loop, rendering, session management, input handling
- Key files:
  - `src/main.rs` (~500 lines initial, grows with event loop)
  - `src/render.rs` (~600 lines, frame rendering and tree display)
  - `src/session_manager.rs` (~500 lines, tokio process management)

**`crates/`:**
- Purpose: Shared libraries (currently only core)
- Contains: Workspace that isolates shared logic
- Key files: `crates/core/src/*`

**`crates/core/`:**
- Purpose: Domain logic library for state, persistence, git operations
- Contains: All data models, state management, validation
- Key files:
  - `src/lib.rs` (~500 lines, AppState with load/save/crud methods)
  - `src/repo.rs` (RepoEntry, git status helper)
  - `src/workspace.rs` (Workspace struct, timestamp formatting)
  - `src/session.rs` (Session struct, SessionStatus enum)
  - `src/worktree.rs` (~290 lines, git worktree management)
  - `src/id.rs` (simple ID generation)

## Key File Locations

**Entry Points:**
- `apps/cli/src/main.rs`: CLI binary entry, Clap command parsing
- `apps/tui/src/main.rs`: TUI binary entry, tokio::main event loop

**Configuration:**
- `Cargo.toml`: Workspace root, defines all members and shared dependencies
- `apps/cli/Cargo.toml`: CLI app config
- `apps/tui/Cargo.toml`: TUI app config
- `crates/core/Cargo.toml`: Core library config

**Core Logic:**
- `crates/core/src/lib.rs`: AppState root with persistence methods
- `crates/core/src/worktree.rs`: Git worktree lifecycle
- `apps/tui/src/session_manager.rs`: Session spawning and async streaming

**Testing:**
- Test code lives in module at bottom of source files (` #[cfg(test)]` sections)
- No separate test files; use `cargo test --workspace`

## Naming Conventions

**Files:**
- Crate names: `kommand0-cli`, `kommand0-tui`, `kommand0-core` (kebab-case)
- Module files: lowercase with underscores (`session_manager.rs`, `scrollback.rs`)
- Binary: `kmd` (short CLI alias)

**Directories:**
- Workspace members: plural nouns (`apps/`, `crates/`)
- Logical groups: feature-based (`cli`, `tui`, `core`)

**Identifiers:**
- Types: PascalCase (`AppState`, `Workspace`, `Session`, `TreeNode`, `SessionStatus`)
- Functions: snake_case (`create_workspace`, `load_from`, `find_session_by_workspace`)
- Constants: UPPER_CASE (`STATE_DIR`, `SPINNER_FRAMES`)
- Enum variants: PascalCase (`Running`, `Stopped`, `Created`, `Fallback`)
- Private fields: lowercase with underscore prefix when needed

## Where to Add New Code

**New CLI Command:**
- File: `apps/cli/src/main.rs`
- Steps:
  1. Add variant to enum `Commands`, `RepoAction`, `WorkspaceAction`, or `SessionAction`
  2. Add Clap struct for the subcommand
  3. Add match arm in the main match statement with implementation
  4. Call appropriate `AppState` method and print result

**New Core Feature (repo/workspace/session logic):**
- File: `crates/core/src/lib.rs` (for AppState methods) or new module
- Steps:
  1. Add fields to `AppState` or create new domain struct
  2. Implement new methods on AppState following pattern: `method()` and `method_with_base(&mut self, base: &Path)`
  3. Call `self.save_to(base)` at end of mutations
  4. Add tests in `#[cfg(test)]` section
  5. Export from `lib.rs` if public

**New TUI Component:**
- File: `apps/tui/src/` (new module or existing)
- Steps:
  1. For visual components: add to `render.rs` or create new `<component>.rs` module
  2. For input handling: add to appropriate handler (mouse.rs, composer, etc.)
  3. For state: add fields to `App` struct in `main.rs`
  4. Register in module declarations at top of `main.rs`
  5. Integrate into event loop and render function

**New Session Event Type:**
- File: `apps/tui/src/session_manager.rs`
- Steps:
  1. Add variant to `SessionEvent` enum
  2. Emit from appropriate reader task
  3. Handle in `main.rs` event loop's session event match
  4. Update rendering or state as needed

**Git Integration (worktree operations):**
- File: `crates/core/src/worktree.rs`
- Pattern: Wrap git commands in `Command::new("git")`, return `WorktreeResult` or `anyhow::Result`
- Do not spawn child processes in core; cli/tui handle process lifecycle

**Utilities (helpers, formatters):**
- Shared utilities: `crates/core/src/` (e.g., `id.rs`, timestamp formatting in `workspace.rs`)
- TUI-specific utilities: `apps/tui/src/` (e.g., path truncation in `render.rs`)

## Special Directories

**`.kommand0-dev/`:**
- Purpose: Runtime state and session artifacts
- Generated: Yes (created on first run)
- Committed: No (in `.gitignore`)
- Contents:
  - `state.json`: JSON serialization of full AppState (repos, workspaces, sessions)
  - `worktrees/<name>/`: Git worktree checkouts (isolated working directories)
  - `sessions/<id>.log`: JSON lines event log for each session

**`.planning/`:**
- Purpose: GSD codebase documentation (created by orchestrator)
- Generated: Yes (created by gsd:map-codebase)
- Committed: No (in `.gitignore`)
- Contents: Codebase analysis docs (ARCHITECTURE.md, STRUCTURE.md, etc.)

**`target/`:**
- Purpose: Cargo build artifacts
- Generated: Yes
- Committed: No (in `.gitignore`)

## Import/Dependency Patterns

**Workspace Dependencies:**
All shared dependencies defined in `Cargo.toml` `[workspace.dependencies]` section:
- Core: serde, tokio, anyhow, chrono, uuid, nix, thiserror
- CLI: clap (on top of core deps)
- TUI: ratatui, crossterm, tui-textarea, unicode-width, futures (on top of core deps)

**Internal Dependencies:**
- `apps/cli` → `kommand0-core`
- `apps/tui` → `kommand0-core`
- Never: `apps/cli` ↔ `apps/tui` (share only via core)

**Import Organization in modules:**
1. Standard library imports (`std::*`)
2. External crate imports (alphabetical: `anyhow`, `serde`, `tokio`, etc.)
3. Internal crate imports (`kommand0_core::*`, `super::*`)
4. Specific use statements for clarity

---

*Structure analysis: 2026-03-22*
