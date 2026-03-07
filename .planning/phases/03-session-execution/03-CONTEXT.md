# Phase 3: Session Execution - Context

**Gathered:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Users can run commands in workspaces, see streaming output, and manage process lifecycle (start/stop/restart/cleanup). Kommand0 is specifically a Claude Code session manager — it spawns `claude` in interactive mode, forwards user input, and renders session output. This phase delivers SESS-01 through SESS-06. UX polish (keyboard navigation consistency, help overlay, pane focus) is Phase 4.

</domain>

<decisions>
## Implementation Decisions

### Session launch model
- Kommand0 is a **Claude Code session manager** — spawns `claude` in interactive mode, not arbitrary shell commands
- Launch config and interactive session I/O are separate concerns
- Use PTY if Claude Code expects terminal semantics; fall back to stdin/stdout if reliable
- Both CLI (`kmd session start <workspace>`) and TUI ('r' key) can start sessions
- 'r' on a workspace immediately launches claude + shows composer
- One active session per workspace; error if session already running
- Launch with `--cwd <workspace_dir>` only — minimal flags, let claude defaults handle rest
- CLI prints confirmation: "Started session for workspace: my-feature"

### Chat composer
- Multi-line text area at the bottom of the right pane, pinned
- Shift+Enter for newline, Enter to send
- Composer always visible when session is active
- Composer input starts empty (not pre-filled with previous input)
- Horizontal border line between output area and composer
- When no session: workspace details (Phase 2) + hint "Press 'r' to start a session"

### Output display
- Full right pane = scrolling output above + composer pinned at bottom
- stdout/stderr interleaved as single stream — no visual distinction
- ANSI escape codes stripped — plain text only
- Markdown shown raw (source text, not rendered)
- User messages shown inline with "> " prefix
- Horizontal rule / blank line separators between exchanges
- Auto-scroll unless user scrolled up; "↓ N new lines" count indicator when paused
- Instant workspace switching — show selected workspace's session output (or details if no session)
- "Session started. Waiting for output..." status line when no output yet
- Soft-wrap long lines within pane width
- Full scrollback history shown when switching to workspace, scrolled to bottom
- 50,000+ line scrollback buffer capacity

### Session status indicators
- Tree view: emoji/icon suffix on workspace name — ▶ running, ■ stopped, ✗ failed
- Workspace active/archived dots kept separate from session status icons
- Right pane title adds session indicator: " Workspace: my-feature ▶ " when running

### Session persistence
- Metadata persisted in state.json `sessions` array (accumulate multiple per workspace)
- Store Claude Code's session ID for `--resume` support
- Scrollback in separate files: `.kommand0-dev/sessions/<session-id>.log`
- Structured JSON lines format: `{ timestamp, source: "user"|"claude", content: string }`
- Auto-relaunch on app restart: lazy — show restored scrollback from disk, relaunch claude only when user selects workspace or types a message
- Use Claude Code's `--resume <session-id>` for session continuity on relaunch
- Restored scrollback shows divider: "─── restored from previous session ───" before new output
- Exited sessions stay visible (not auto-cleaned) until user explicitly clears
- Unbounded log retention — keep all session logs until manual cleanup

### CLI session commands
- `kmd session start <workspace>` — spawn claude in workspace dir (background, prints confirmation)
- `kmd session stop <workspace>` — stop running session
- `kmd session list` — show sessions with workspace, PID, status
- `kmd session clear <workspace>` — remove session metadata and log file

### Stop behavior
- SIGTERM first, SIGKILL after 5 second timeout
- Process groups via setsid on spawn — kill entire process group on stop
- Ctrl+C is context-dependent: in composer → clears input; outside composer → stops session; no session running → quit app
- No confirmation before stopping
- Stopped session: frozen output visible, status changes to "stopped", composer disabled

### Restart behavior
- 'R' (Shift+R) restarts: kill current + relaunch with `--resume` to continue Claude Code session
- New session ID on restart — clean log file, old log preserved separately
- Output pane cleared on restart (fresh start)
- 'r' starts new session, 'R' restarts stopped session

### App quit
- 'q' triggers shutdown: SIGTERM all sessions, show "Stopping sessions..." with progress
- 5 second timeout, then SIGKILL remaining
- Zombie warning on stderr after exit: "Warning: process <PID> may still be running"

### Claude's Discretion
- PTY vs stdin/stdout transport decision (based on what claude CLI requires)
- Exact structured log serialization details
- Session ID format in state.json
- Composer widget implementation (ratatui textarea approach)
- Scrollback buffer data structure (VecDeque, ring buffer, etc.)
- How to parse/capture Claude Code's session ID from process output
- Exact process group setup (setsid vs nix crate approach)
- Shutdown progress UI implementation
- Error state handling for claude process crashes

</decisions>

<specifics>
## Specific Ideas

- Kommand0 should act as a bridge to real Claude Code sessions — manage and render them, not reinterpret their input language
- Session restore should work as in Claude Code itself — use `--resume` for continuity, not re-inventing session management
- Prefer the transport that most faithfully supports real interactive Claude Code sessions (PTY if needed)
- The right pane is already a "placeholder for Phase 3 session output" (Phase 2 context) — easy to extend

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `AppState` (crates/core/src/lib.rs): Add `sessions: Vec<Session>` with `#[serde(default)]` — same pattern as workspaces
- `Workspace` (crates/core/src/workspace.rs): Session references via `workspace_id` foreign key
- `generate_id()` (crates/core/src/id.rs): Reuse for session IDs
- `format_timestamp()` (crates/core/src/workspace.rs): Reuse for session display
- `resolve_repo()` (crates/core/src/lib.rs:106): Pattern for workspace resolver
- Async event loop with `tokio::select!` (apps/tui/src/main.rs:221): Ready for async process I/O
- 250ms tick interval (apps/tui/src/main.rs:216): Use for polling session output
- `TreeNode` enum (apps/tui/src/main.rs:24): Extend with session status display
- `truncate_path()` (apps/tui/src/main.rs:247): Reuse in session output

### Established Patterns
- `anyhow::Result` + `bail!()` for validation — continue for session operations
- `_with_base()` pattern for testable methods — follow for session methods
- `#[derive(Debug, Clone, Serialize, Deserialize)]` on all persisted types
- `#[serde(default)]` for backward-compatible state.json evolution
- `Status` enum for TUI state — extend for session states

### Integration Points
- `AppState` in crates/core/src/lib.rs — add Session model + CRUD methods
- CLI main.rs match block — add Session arm with start/stop/list/clear subcommands
- TUI `App` struct (apps/tui/src/main.rs:40) — add session state, composer state, scrollback buffers
- TUI `ui()` function (apps/tui/src/main.rs:258) — right pane becomes output+composer when session active
- TUI key handling in `run()` (apps/tui/src/main.rs:224) — add 'r', 'R', Ctrl+C context handling
- `tokio::select!` loop (apps/tui/src/main.rs:221) — add session output polling branch

</code_context>

<deferred>
## Deferred Ideas

- CLI-starts-TUI-focused-on-session — future phase
- Text selection/copy mode — v2 (ASESS-05)
- Multiple sessions per workspace — v2 (ASESS-06)
- Session templates / presets — v2 (ASESS-01)
- Auto-restart policies — v2 (ASESS-02)

</deferred>

---

*Phase: 03-session-execution*
*Context gathered: 2026-03-07*
