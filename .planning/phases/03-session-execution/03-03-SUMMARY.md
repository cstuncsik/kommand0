---
phase: 03-session-execution
plan: 03
subsystem: tui
tags: [session-lifecycle, tui-integration, cli-session, composer, scrollback, status-indicators, log-file]

# Dependency graph
requires:
  - phase: 03-session-execution/01
    provides: Session struct, SessionStatus enum, ScrollbackBuffer, AppState CRUD
  - phase: 03-session-execution/02
    provides: SessionManager with start/stop/restart/send/poll, Composer widget
provides:
  - Full TUI integration: session lifecycle wired into event loop with live output streaming
  - Right pane split view: scrolling output + pinned composer when session active
  - Session status indicators in tree view (running/stopped/failed/exited icons)
  - Key bindings: r=start, R=restart, Ctrl+C=context-stop, Tab=toggle composer, q=shutdown-all
  - CLI session subcommands: start, stop, list, clear
  - Session log file writing in JSON lines format
affects: [04-ux-polish]

# Tech tracking
tech-stack:
  added: []
  patterns: [session state persistence on lifecycle events, right pane split layout for output+composer, context-dependent Ctrl+C handling]

key-files:
  created: []
  modified:
    - apps/tui/src/main.rs
    - apps/cli/src/main.rs
    - apps/cli/Cargo.toml

key-decisions:
  - "Active session ID tracked per-workspace for instant switching when cursor moves"
  - "Composer focus state separate from session running state for explicit user control"
  - "CLI session start uses std::process::Command (fire-and-forget) not tokio async since CLI is synchronous"
  - "Restart creates new session in state while SessionManager tracks with its own UUID"

patterns-established:
  - "Right pane split: Layout with [Min(1), Length(composer_height)] for output+composer"
  - "Session event polling in tick branch of tokio::select! loop"
  - "Context-dependent key handling: composer_focused flag splits key dispatch"

requirements-completed: [SESS-01, SESS-02, SESS-03, SESS-04, SESS-05]

# Metrics
duration: 4min
completed: 2026-03-07
---

# Phase 3 Plan 03: TUI & CLI Session Integration Summary

**Full TUI session wiring with live output streaming, pinned composer, status indicators in tree view, context-dependent key handling, and CLI session start/stop/list/clear subcommands**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-07T22:11:08Z
- **Completed:** 2026-03-07T22:15:12Z
- **Tasks:** 3 (2 auto + 1 checkpoint auto-approved)
- **Files modified:** 3

## Accomplishments
- TUI main.rs rewritten to integrate SessionManager, ScrollbackBuffer, and Composer into App struct and tokio event loop
- Right pane splits into scrolling output area + pinned composer when workspace has a session, falls back to details view with "Press r to start" hint otherwise
- Session status icons appended to workspace names in tree view: running (green triangle), stopped (yellow square), failed/exited (red/gray X)
- Full key binding suite: r=start session, R=restart with --resume, Ctrl+C=stop session or quit, Tab=toggle composer, PageUp/PageDown=scroll output, q=shutdown all and exit
- Session events polled each 250ms tick: output lines pushed to per-workspace ScrollbackBuffer, exit/error status updates saved to state
- Log files written in JSON lines format with timestamp, source (user/claude), and content
- CLI session subcommands: start spawns claude process with PID tracking, stop sends SIGTERM/SIGKILL to process group, list shows tabular session info, clear removes metadata and log file

## Task Commits

Each task was committed atomically:

1. **Task 1: TUI integration -- session lifecycle, output rendering, composer, status indicators** - `161596f` (feat)
2. **Task 2: CLI session subcommands (start/stop/list/clear)** - `10fc20d` (feat)
3. **Task 3: Checkpoint human-verify** - Auto-approved (auto_advance mode)

## Files Created/Modified
- `apps/tui/src/main.rs` - Complete rewrite: App struct with session fields, event loop with session polling, split right pane rendering, key handlers for session lifecycle
- `apps/cli/src/main.rs` - Added Session subcommand with Start/Stop/List/Clear actions
- `apps/cli/Cargo.toml` - Added nix.workspace dependency for signal handling in stop command

## Decisions Made
- Active session ID tracked per-workspace: when cursor moves between workspaces, active_session_id updates instantly for seamless switching
- Composer focus is a separate boolean from session running state, toggled via Tab or Esc, enabling explicit user control
- CLI session start uses synchronous std::process::Command (fire-and-forget) since CLI doesn't need async event loop -- spawns process and records PID
- Restart flow: old session marked Stopped, new session created in AppState, SessionManager generates its own UUID for internal tracking

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed borrow checker error in scroll key handlers**
- **Found during:** Task 1
- **Issue:** `selected_workspace()` returned `&Workspace` while trying to mutably borrow `scrollbacks` HashMap
- **Fix:** Clone workspace ID before mutable borrow of scrollbacks
- **Files modified:** apps/tui/src/main.rs
- **Verification:** `cargo build -p kommand0-tui` succeeds
- **Committed in:** 161596f (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (1 bug fix)
**Impact on plan:** Standard Rust borrow checker fix, no scope change.

## Issues Encountered
None beyond the borrow checker fix documented above.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Phase 3 complete: all session execution requirements (SESS-01 through SESS-05) implemented
- Ready for Phase 4 (UX Polish): keyboard shortcuts, theming, error messages
- Session infrastructure fully functional for end-to-end workflow testing

## Self-Check: PASSED

All files exist, all commits verified (161596f, 10fc20d).

---
*Phase: 03-session-execution*
*Completed: 2026-03-07*
