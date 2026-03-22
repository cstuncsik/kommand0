# External Integrations

**Analysis Date:** 2026-03-22

## APIs & External Services

**Claude Integration (Planned):**
- Integration point: `Session.claude_session_id` field in `crates/core/src/session.rs`
- Status: Not yet implemented (field reserved for future use)
- Purpose: Link sessions to Claude conversation contexts

**No other external APIs currently integrated**

## Data Storage

**Databases:**
- Not used - No relational or NoSQL database

**File Storage:**
- Local filesystem only
- State file: `.kommand0-dev/state.json` (JSON format)
- Session logs: `.kommand0-dev/sessions/{session_id}.log` (plain text log files)
- Git repositories: Managed by Git (no custom storage)

**Caching:**
- In-memory application state only
- Scrollback buffers for TUI output: `apps/tui/src/scrollback.rs`
- No persistent cache layer

## Authentication & Identity

**Auth Provider:**
- None - No authentication system
- Access is implicit to any user with file system access to `.kommand0-dev/`

**User Identification:**
- No user model - State is per-machine in a single `.kommand0-dev` directory
- Sessions tracked by UUID (v4) generated via `uuid` crate in `crates/core/src/id.rs`

## System Integration

**Git Operations:**
- Uses native `git` command-line tool (spawned via `std::process::Command`)
- Operations:
  - `git status --short --branch` - Repository status (`crates/core/src/repo.rs`)
  - `git worktree add/remove` - Workspace isolation (`crates/core/src/worktree.rs`)
  - `git rev-parse --verify` - Branch validation
  - Fallback behavior if Git not available (uses repo root as working directory)

**Process Management:**
- Direct process spawning via Rust standard library
- No job control or daemon framework
- Session PID tracking in `Session.pid` field (optional, not yet populated)

**Terminal/Shell Integration:**
- Crossterm for terminal control (colors, cursor, events)
- Parses ANSI escape codes from shell output via `strip-ansi-escapes` crate
- No shell wrapper or session management

## Monitoring & Observability

**Error Tracking:**
- None - No error reporting service

**Logs:**
- Approach: File-based session logs
  - Location: `~/.kommand0-dev/sessions/{session_id}.log`
  - Format: Plain text accumulation of command output
  - Written during: Session execution (output streamed to log)
  - Cleanup: Automatic when session/workspace/repo deleted
- Internal logging: None (no tracing/logging framework initialized)

## CI/CD & Deployment

**Hosting:**
- Local machine only - No remote deployment
- Single executable: `kmd` (from `apps/cli`)
- TUI binary: `kommand0-tui` (from `apps/tui`)

**CI Pipeline:**
- Not detected - No GitHub Actions, GitLab CI, or build workflow files
- Build via standard `cargo build` / `cargo test`

## Environment Configuration

**Required env vars:**
- None - All configuration is via CLI arguments or interactive TUI

**Optional env vars:**
- Not used in codebase

**Secrets location:**
- Not applicable - No external credentials needed
- Claude session ID would be in-memory only (once implemented)

## Webhooks & Callbacks

**Incoming:**
- None - No HTTP server

**Outgoing:**
- None - No external service calls

## External Command Dependencies

**Git:**
- Required for repository operations and worktree creation
- Graceful fallback if unavailable (uses repo root directory instead)
- No version requirements specified

**Standard Unix Tools:**
- Not required - No explicit dependencies on common Unix utilities

---

*Integration audit: 2026-03-22*
