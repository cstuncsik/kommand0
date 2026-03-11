# Architecture

**Analysis Date:** 2026-03-11

## Pattern Overview

**Overall:** Rust workspace monorepo with a shared core library and two thin application frontends (CLI and TUI).

**Key Characteristics:**
- Domain logic centralized in `kommand0-core` crate; apps are thin shells
- JSON file-based persistence (no database) via `AppState` struct
- Async runtime (tokio) in TUI for process management and event streaming
- Synchronous CLI that spawns child processes fire-and-forget
- Three-level domain model: Repo -> Workspace -> Session
- Child process management via `claude` CLI with stream-json protocol

## Layers

**Core Domain (`kommand0-core`):**
- Purpose: All shared domain types, state persistence, git worktree management
- Location: `crates/core/src/`
- Contains: Domain models (`RepoEntry`, `Workspace`, `Session`, `SessionStatus`), `AppState` with all CRUD operations, git helpers (`run_git_status`, worktree creation/removal), ID generation
- Depends on: serde, serde_json, anyhow, chrono, uuid
- Used by: `apps/cli`, `apps/tui`

**CLI Application (`kommand0-cli`):**
- Purpose: Headless command-line interface for repo/workspace/session management
- Location: `apps/cli/src/main.rs`
- Contains: clap-derived CLI parser, synchronous command handlers
- Depends on: `kommand0-core`, clap, anyhow, nix
- Used by: End users via `kmd` binary

**TUI Application (`kommand0-tui`):**
- Purpose: Interactive terminal UI with real-time session streaming
- Location: `apps/tui/src/`
- Contains: Async event loop, rendering, session process manager, UI components (composer, modals, buttons, help overlay, mouse handling)
- Depends on: `kommand0-core`, ratatui, crossterm, tokio, tui-textarea, futures, nix, uuid
- Used by: End users via `cargo run -p kommand0-tui`

## Data Flow

**Session Lifecycle (TUI):**

1. User selects a workspace in the tree pane and presses `r` or `Enter`
2. `AppState::create_session()` creates a `Session` record and persists to `state.json`
3. `SessionManager::start_session()` spawns `claude -p --input-format stream-json --output-format stream-json` as a child process in the workspace's working directory
4. Two tokio tasks spawn to read stdout and stderr, sending `SessionEvent` variants through an unbounded mpsc channel
5. The TUI event loop polls `SessionManager::poll_events()` every 50ms tick, routing output to per-workspace `ScrollbackBuffer` instances
6. User messages go through `Composer` -> `SessionManager::send_message()` -> child stdin as JSON
7. On quit, `SessionManager::shutdown_all()` sends SIGTERM to process groups, waits 5s, then SIGKILL

**Session Lifecycle (CLI):**

1. `kmd session start <workspace>` creates session via `AppState::create_session()`
2. Spawns `claude` process synchronously with `std::process::Command`, records PID
3. `kmd session stop <workspace>` sends SIGTERM via nix, waits 1s, then SIGKILL if needed
4. Updates session status in state.json

**State Management:**
- All state lives in a single `AppState` struct serialized to `.kommand0-dev/state.json`
- State is loaded at startup, mutated in memory, and saved after each operation
- `AppState` methods come in pairs: `method()` (uses default dir) and `method_with_base()` (accepts custom dir, used in tests)
- No locking mechanism; concurrent access from CLI and TUI is not protected

**Session Output Streaming (TUI):**
- Claude CLI outputs stream-json events on stdout
- `classify_json_event()` in `session_manager.rs` parses JSON events into `JsonEvent` variants: `Delta` (streaming text), `StreamEnd`, `Complete` (full response), `Empty` (non-content)
- `StreamDelta` events accumulate in `App::streaming_text` HashMap per workspace
- On newlines within deltas, accumulated text flushes to `ScrollbackBuffer`
- `StreamEnd` flushes any remaining partial text
- Log file written as JSON lines to `.kommand0-dev/sessions/{session_id}.log`

**Session Resume:**
- On TUI startup, sessions with status != Running that have non-empty scrollback are auto-resumed
- Claude session ID extracted from stream-json output is stored; used for `--resume` flag on restart
- `SessionManager::restart_session()` removes old process, generates new session ID, starts new process with `--resume`

## Key Abstractions

**AppState:**
- Purpose: Central state container and persistence layer for all domain objects
- Location: `crates/core/src/lib.rs`
- Pattern: God object holding repos, workspaces, sessions with all CRUD methods. Serialized/deserialized as a single JSON file.

**SessionManager:**
- Purpose: Manages lifecycle of Claude CLI child processes in the TUI
- Location: `apps/tui/src/session_manager.rs`
- Pattern: Owns a HashMap of `RunningSession` structs (child process + stdin handle). Uses mpsc channels to bridge async reader tasks with the main event loop.

**ScrollbackBuffer:**
- Purpose: Fixed-capacity ring buffer for terminal output lines per workspace
- Location: `apps/tui/src/scrollback.rs`
- Pattern: VecDeque-backed buffer with scroll offset tracking, new-lines-since-scroll counter, and viewport-aware clamping. Capacity: 50,000 lines.

**App (TUI):**
- Purpose: Root application state for the TUI
- Location: `apps/tui/src/main.rs`
- Pattern: Holds all UI state (focus, selection, expanded nodes, scrollbacks, streaming text, modal state, mouse position, hit regions) plus the `SessionManager` and `AppState`.

**TreeNode:**
- Purpose: Virtual tree items for the left pane (repos, workspaces, hints)
- Location: `apps/tui/src/main.rs`
- Pattern: Enum with three variants. Rebuilt from `AppState` on mutation via `App::rebuild_tree()`.

## Entry Points

**CLI Binary (`kmd`):**
- Location: `apps/cli/src/main.rs`
- Triggers: Direct CLI invocation
- Responsibilities: Parse clap args, load AppState, execute command, save state, print results

**TUI Binary:**
- Location: `apps/tui/src/main.rs`
- Triggers: `cargo run -p kommand0-tui`
- Responsibilities: Initialize terminal, set up mouse capture + keyboard enhancement, run async event loop, restore terminal on exit

**Core Library:**
- Location: `crates/core/src/lib.rs`
- Triggers: Imported by both apps
- Responsibilities: Re-exports all public types and functions

## Error Handling

**Strategy:** `anyhow::Result` for all fallible operations; `thiserror` available but not currently used for custom error types.

**Patterns:**
- Core uses `anyhow::bail!()` for domain validation errors (missing repo, duplicate workspace, etc.)
- Core uses `.with_context()` for IO errors with path information
- CLI propagates errors to `main()` which prints them via anyhow's Display
- TUI catches most errors locally, logging warnings to stderr for non-critical failures (e.g., worktree removal)
- SessionManager treats process spawn failures as `Err`, updates session to `Failed` status
- Worktree removal is intentionally lenient (logs warning, returns Ok) to avoid blocking workspace deletion

## Cross-Cutting Concerns

**Logging:** No structured logging in production. `tracing` is a dependency but not actively used. Session output is logged to JSON lines files in `.kommand0-dev/sessions/`.

**Validation:** Inline in `AppState` methods. Repo paths validated via `is_dir()` and `fs::canonicalize()`. Workspace names checked for uniqueness. Session creation checks for existing running session.

**Authentication:** Not applicable. Relies on `claude` CLI being pre-authenticated.

**Process Signals:** Uses `nix` crate for POSIX signal handling. Sends SIGTERM to process groups (negative PID), escalates to SIGKILL after timeout. TUI uses `process_group(0)` and `kill_on_drop(true)`.

---

*Architecture analysis: 2026-03-11*
