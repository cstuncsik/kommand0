# Architecture

**Analysis Date:** 2026-03-22

## Pattern Overview

**Overall:** Layered monorepo with shared domain core + two presentation tiers

**Key Characteristics:**
- **Shared Domain Core**: `crates/core` contains all state management, data models, and git operations
- **Two Independent Frontends**: CLI (`apps/cli`) and TUI (`apps/tui`) both depend on core, never directly on each other
- **Event-Driven UI**: TUI uses async/await with tokio + mpsc channels for managing background Claude sessions
- **Stateful Persistence**: All application state (`AppState`) serializes to JSON at ``.kommand0-dev/state.json`

## Layers

**Core Domain (`crates/core/src`):**
- Purpose: Manages repos, workspaces, sessions, git operations, and persistence
- Location: `crates/core/src/`
- Contains: Data models (`RepoEntry`, `Workspace`, `Session`), state management (`AppState`), git integration
- Depends on: Standard library, serde, tokio, chrono, uuid, anyhow
- Used by: `apps/cli` and `apps/tui` exclusively

**CLI Presentation (`apps/cli/src`):**
- Purpose: Command-line interface for repo/workspace/session operations
- Location: `apps/cli/src/main.rs`
- Contains: Clap argument parsing, direct AppState operations, output formatting
- Depends on: `kommand0-core`, clap, anyhow
- Used by: Direct CLI invocation via `kmd` binary

**TUI Presentation (`apps/tui/src`):**
- Purpose: Interactive terminal UI with streaming session management and real-time rendering
- Location: `apps/tui/src/`
- Contains: Ratatui rendering, async session spawning, event loop, keyboard/mouse input handling
- Depends on: `kommand0-core`, ratatui, crossterm, tokio, tui-textarea
- Used by: Direct TUI invocation via `cargo run -p kommand0-tui`

## Data Flow

**Repo/Workspace/Session Lifecycle:**

1. **User Action** (CLI command or TUI keybind)
   - CLI: Clap parses arguments → calls AppState methods
   - TUI: Keyboard/mouse event → handled in main event loop

2. **State Mutation** (always via `crates/core`)
   - AppState methods validate and modify state (add_repo, create_workspace, create_session, etc.)
   - Each mutation calls `save_to()` or `save()` to persist to disk

3. **Session Spawning** (TUI-specific)
   - SessionManager spawns claude process with `--input-format stream-json --output-format stream-json`
   - Output streamed line-by-line via background tokio tasks
   - Session events (Output, StreamDelta, StreamEnd, Error) sent via mpsc channel

4. **Rendering/Output** (presentation-dependent)
   - CLI: Formatted output to stdout
   - TUI: Event loop polls session_manager.poll_events(), updates ScrollbackBuffer, re-renders on each frame

**Git Worktree Flow:**

1. Create workspace → calls `worktree::create_worktree(repo_path, workspace_name, base_dir)`
2. Git command: `git -C <repo> worktree add <worktree_path> -b kommand0/<workspace_name>`
3. Branch names auto-disambiguated with suffix if collision detected (kommand0/name-2, etc.)
4. Workspace stores `worktree_path` for later deletion
5. Delete workspace → calls `worktree::remove_worktree(repo_path, worktree_path)` with --force flag

## Key Abstractions

**AppState:**
- Purpose: Root state container for entire application
- Examples: `crates/core/src/lib.rs`
- Pattern: Serialize/deserialize to ``.kommand0-dev/state.json``, provides methods for all domain operations, validates state before mutations

**SessionManager:**
- Purpose: Spawns and manages background Claude CLI processes, streams output via channels
- Examples: `apps/tui/src/session_manager.rs`
- Pattern: Tokio tasks read claude stdout/stderr, parse JSON stream events, emit SessionEvent via mpsc, TUI main loop polls events

**TreeNode/App:**
- Purpose: Represents in-memory TUI state (repos expanded, selected workspace, focus, modal state)
- Examples: `apps/tui/src/main.rs`
- Pattern: Enum for tree structure (Repo | Workspace | Hint), mutable App struct holds all UX state, event loop mutates and re-renders

**ScrollbackBuffer:**
- Purpose: Circular buffer for session output with scroll tracking
- Examples: `apps/tui/src/scrollback.rs`
- Pattern: VecDeque with capacity limit, tracks scroll offset and new lines since last scroll for auto-follow behavior

**Workspace Worktree:**
- Purpose: Encapsulates git worktree lifecycle and branch naming
- Examples: `crates/core/src/worktree.rs`
- Pattern: `WorktreeResult` enum (Created | Fallback), graceful degradation if worktree creation fails (uses repo root instead)

## Entry Points

**CLI Entry:**
- Location: `apps/cli/src/main.rs`
- Triggers: `kmd <command> <subcommand> [args]`
- Responsibilities: Parse CLI args via Clap, load AppState, dispatch to appropriate action, save state, print output

**TUI Entry:**
- Location: `apps/tui/src/main.rs`
- Triggers: `cargo run -p kommand0-tui` or binary invocation
- Responsibilities: Initialize terminal, load AppState, restore scrollback from logs, enter async event loop, handle keyboard/mouse/session events, render frame

**Core State Load:**
- Location: `crates/core/src/lib.rs::AppState::load()`
- Triggers: On CLI startup or TUI initialization
- Responsibilities: Read ``.kommand0-dev/state.json``, deserialize, return default if file missing

## Error Handling

**Strategy:** Result-based error propagation with anyhow

**Patterns:**
- Core functions return `anyhow::Result<T>` with context messages (`with_context()`)
- CLI: Errors bubble to main, printed to stderr, process exits with code 1
- TUI: Session errors caught and stored as SessionEvent::Error, rendered in output pane
- Git operations gracefully degrade (worktree creation failure falls back to repo root, git status errors logged)

## Cross-Cutting Concerns

**Logging:** Tracing framework integrated at workspace level, sparse use in current codebase (mostly debug output)

**Validation:**
- Repo add: validates path exists and is directory, rejects duplicates
- Workspace create: validates repo reference resolves, rejects duplicate names, validates workspace_id exists for sessions
- Session create: checks no running session for workspace already exists

**Authentication:**
- Repo operations assume local git repos (no remote auth needed)
- Claude session inherits user's CLAUDECODE env var (removed from subprocess env to prevent recursion)

---

*Architecture analysis: 2026-03-22*
