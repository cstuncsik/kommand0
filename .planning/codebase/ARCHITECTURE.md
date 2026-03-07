# Architecture

**Analysis Date:** 2026-03-07

## Pattern Overview

**Overall:** Rust Workspace with shared core library and thin application frontends (CLI + TUI)

**Key Characteristics:**
- Cargo workspace with three members: `apps/cli`, `apps/tui`, `crates/core`
- Domain logic and persistence centralized in `crates/core` (`kommand0-core`)
- Application binaries are thin wrappers: CLI parses commands, TUI renders and handles input
- State persisted as JSON on the local filesystem (`.kommand0-dev/state.json`)
- Synchronous execution model (tokio declared as dependency but not yet used)
- No network layer; all operations are local filesystem and git subprocess calls

## Layers

**Core Domain (`crates/core`):**
- Purpose: Models, persistence, and shared domain logic
- Location: `crates/core/src/lib.rs`
- Contains: `RepoEntry` model, `AppState` (load/save/add_repo), `run_git_status()` helper, ID generation
- Depends on: `serde`, `serde_json`, `anyhow`, `thiserror`, `tracing`, `tokio` (declared but unused)
- Used by: `apps/cli`, `apps/tui`

**CLI Application (`apps/cli`):**
- Purpose: Command-line interface for repo management
- Location: `apps/cli/src/main.rs`
- Contains: `clap`-based command parsing, thin dispatch to `AppState` methods
- Depends on: `kommand0-core`, `clap`, `anyhow`
- Used by: End user via `kmd` binary
- Binary name: `kmd`

**TUI Application (`apps/tui`):**
- Purpose: Terminal UI for interactive repo browsing and git status viewing
- Location: `apps/tui/src/main.rs`
- Contains: `App` struct (view state), `ratatui` rendering, keyboard event loop
- Depends on: `kommand0-core`, `ratatui`, `crossterm`, `anyhow`
- Used by: End user via `kommand0-tui` binary

## Data Flow

**CLI Add Repo:**

1. User runs `kmd repo add <path>`
2. `clap` parses args into `Commands::Repo { action: RepoAction::Add { path } }`
3. `AppState::load()` reads `.kommand0-dev/state.json` (or returns default if missing)
4. `AppState::add_repo()` validates path, canonicalizes, checks duplicates, generates ID, appends to `repos` vec
5. `AppState::save()` writes updated JSON back to `.kommand0-dev/state.json`

**TUI Git Status:**

1. TUI starts, calls `AppState::load()` to get repo list
2. User navigates with j/k/arrows, selects repo with Enter
3. `App::run_status()` calls `run_git_status(repo.path)` from core
4. `run_git_status()` spawns `git -C <path> status --short --branch` as subprocess
5. Output captured and displayed in right pane; errors shown in status title

**State Management:**
- `AppState` is the single persistence model, serialized as JSON
- CLI loads, mutates, and saves state per command invocation
- TUI loads state once at startup (no live reload)
- State file location: `.kommand0-dev/state.json` (relative to working directory)

## Key Abstractions

**RepoEntry:**
- Purpose: Represents a tracked git repository
- Defined in: `crates/core/src/lib.rs`
- Fields: `id` (hex timestamp), `name` (directory name), `path` (canonical absolute path)
- Pattern: Simple data struct with Serialize/Deserialize

**AppState:**
- Purpose: Root persistence object holding all application state
- Defined in: `crates/core/src/lib.rs`
- Fields: `repos: Vec<RepoEntry>`
- Pattern: Self-loading/saving with `load()` and `save()` methods, JSON file-backed
- Constants: `STATE_DIR = ".kommand0-dev"`, `STATE_FILE = "state.json"`

**App (TUI-local):**
- Purpose: TUI view state (not persisted, not in core)
- Defined in: `apps/tui/src/main.rs`
- Fields: `repos`, `selected` (ListState), `output`, `status` (Idle/Done/Error)
- Pattern: Immediate-mode UI state driving ratatui rendering

**Planned Abstractions (from `gds-brief.md`):**
- `Workspace` - logical workspace linked to a repo (Milestone 2)
- `Session` - command execution context within a workspace (Milestone 3)
- `SessionStatus` - running/stopped/failed state for sessions (Milestone 3)

## Entry Points

**CLI Binary (`kmd`):**
- Location: `apps/cli/src/main.rs`
- Binary config: `apps/cli/Cargo.toml` defines `[[bin]] name = "kmd"`
- Triggers: User runs `kmd <subcommand>`
- Responsibilities: Parse CLI args, dispatch to core, print results

**TUI Binary (`kommand0-tui`):**
- Location: `apps/tui/src/main.rs`
- Triggers: User runs the TUI binary directly
- Responsibilities: Initialize terminal, render loop, handle keyboard input, display git status

## Error Handling

**Strategy:** `anyhow::Result` for application-level errors throughout all three crates

**Patterns:**
- `anyhow::bail!()` for validation failures (e.g., path not a directory, repo already tracked, git command failure)
- `.with_context()` for wrapping I/O and serialization errors with human-readable messages
- TUI captures errors into `Status::Error(String)` for display rather than crashing
- CLI lets errors propagate to `main()` which prints them via anyhow's default Display

## Cross-Cutting Concerns

**Logging:** `tracing` crate declared as workspace dependency but not yet initialized or used in any application code
**Validation:** Inline validation in `AppState::add_repo()` -- checks path exists, is a directory, and is not already tracked
**Authentication:** Not applicable (local-only tool)
**Process Management:** Direct `std::process::Command` usage for git subprocess calls; no async or background process handling yet

---

*Architecture analysis: 2026-03-07*
