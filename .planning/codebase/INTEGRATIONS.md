# External Integrations

**Analysis Date:** 2026-03-11

## APIs & External Services

**Claude CLI (Anthropic):**
- Primary external integration. The entire application orchestrates Claude CLI sessions.
- Spawned as child process: `claude -p --verbose --input-format stream-json --output-format stream-json`
- Communication protocol: stream-json over stdin/stdout pipes
- Resume support: `--resume <claude_session_id>` flag for session continuity
- CLI invocation in TUI: `apps/tui/src/session_manager.rs` lines 85-107
- CLI invocation in CLI: `apps/cli/src/main.rs` lines 288-300
- `CLAUDECODE` env var explicitly removed from child process environment
- JSON event types parsed: `system`, `start`, `ping`, `error`, `content_block_delta`, `content_block_stop`, `message_stop`, `assistant`, `message`, `result`
- Session ID extraction: Reads `session_id` field from JSON output for resume capability

**Input format (sent to Claude stdin):**
```json
{
  "type": "user",
  "message": {
    "role": "user",
    "content": "<user text>"
  }
}
```
Reference: `apps/tui/src/session_manager.rs` lines 228-234

## Data Storage

**Databases:**
- None. All state is file-based.

**File Storage:**
- Local filesystem only
- State file: `.kommand0-dev/state.json` (JSON, read/written via `serde_json`)
  - Contains: repos, workspaces, sessions arrays
  - Managed by: `crates/core/src/lib.rs` (`AppState::load()` / `AppState::save()`)
- Session logs: `.kommand0-dev/sessions/<session_id>.log`
  - Created per session, cleaned up on session clear or repo deletion

**Caching:**
- None

## Authentication & Identity

**Auth Provider:**
- None for kommand0 itself
- Claude CLI authentication is handled externally by the Claude CLI tool (user must have it pre-configured)

## Git Integration

**Git CLI (spawned as subprocess):**
- Git status: `git -C <repo_path> status --short --branch`
  - Implementation: `crates/core/src/repo.rs` `run_git_status()`
- Git worktree create: `git -C <repo_path> worktree add <path> -b <branch>`
  - Implementation: `crates/core/src/worktree.rs` `create_worktree()`
  - Branch naming: `kommand0/<workspace_name>` with auto-suffix for collisions
- Git worktree remove: `git -C <repo_path> worktree remove <path> --force`
  - Implementation: `crates/core/src/worktree.rs` `remove_worktree()`
- Git branch check: `git -C <repo_path> rev-parse --verify <branch>`
  - Implementation: `crates/core/src/worktree.rs` `branch_exists()`

**Requirements:**
- Git must be on PATH
- Repos must be valid git repositories for worktree features (graceful fallback if not)

## Monitoring & Observability

**Error Tracking:**
- None (local-only tool)

**Logs:**
- `tracing` 0.1 is declared as dependency in `crates/core/Cargo.toml` but no subscriber is configured
- Claude session output is captured via stdout/stderr pipe readers
- Session log files at `.kommand0-dev/sessions/<id>.log`

## CI/CD & Deployment

**Hosting:**
- Local binary only. No deployment target.

**CI Pipeline:**
- Not detected. No `.github/workflows/`, `.gitlab-ci.yml`, or similar config files.

## Environment Configuration

**Required env vars:**
- None for kommand0 itself

**Removed env vars:**
- `CLAUDECODE` - Explicitly removed from Claude CLI child processes to prevent interference

**Secrets location:**
- No secrets managed by this application
- Claude CLI handles its own authentication externally

## Webhooks & Callbacks

**Incoming:**
- None

**Outgoing:**
- None

## Process Management

**Unix Signals (via `nix` crate):**
- SIGTERM sent to process groups (`-pgid`) for graceful shutdown
- SIGKILL as fallback after timeout (1s in CLI, 5s in TUI)
- Process group isolation: child processes spawned with `.process_group(0)` (TUI) for clean signal delivery
- Implementation: `apps/tui/src/session_manager.rs` `stop_session()`, `shutdown_all()`
- Implementation: `apps/cli/src/main.rs` `SessionAction::Stop`

---

*Integration audit: 2026-03-11*
