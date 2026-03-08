---
phase: 03-session-execution
plan: 02
subsystem: tui
tags: [session-manager, process-lifecycle, stream-json, tui-textarea, composer, mpsc, sigterm, sigkill]

# Dependency graph
requires:
  - phase: 03-session-execution/01
    provides: Session struct, SessionStatus enum, ScrollbackBuffer, workspace deps (nix, tui-textarea, strip-ansi-escapes, uuid)
provides:
  - SessionManager with start/stop/restart/send/shutdown/poll methods
  - SessionEvent enum for background-to-TUI communication via mpsc
  - Composer widget wrapping tui-textarea with Enter-to-send, Shift+Enter-for-newline
affects: [03-03-tui-cli-integration]

# Tech tracking
tech-stack:
  added: []
  patterns: [mpsc unbounded channel for session events, process_group(0) for clean kill, stream-json transport for Claude CLI, ANSI stripping + JSON content extraction pipeline]

key-files:
  created:
    - apps/tui/src/session_manager.rs
    - apps/tui/src/composer.rs
  modified:
    - apps/tui/src/main.rs

key-decisions:
  - "Exit detection via stdout pipe closure rather than separate waitpid watcher -- simpler, no polling overhead"
  - "JSON content extraction tries result.content, content string, content array of blocks, message.content in order with raw text fallback"
  - "Composer uses static helper make_textarea to DRY textarea configuration across new/clear/set_active"

patterns-established:
  - "SessionEvent mpsc pattern: background tokio tasks send events, TUI polls via try_recv each tick"
  - "Process group kill pattern: SIGTERM to -pgid, 5s timeout, SIGKILL fallback"
  - "Composer widget pattern: wraps TextArea, intercepts Enter key, returns Option<String>"

requirements-completed: [SESS-01, SESS-02, SESS-03, SESS-04, SESS-06]

# Metrics
duration: 3min
completed: 2026-03-07
---

# Phase 3 Plan 02: Session Manager & Composer Summary

**SessionManager with Claude CLI stream-json spawning, process group lifecycle (SIGTERM/SIGKILL), mpsc event streaming, and Composer widget with Enter-to-send/Shift+Enter-for-newline**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-07T22:05:53Z
- **Completed:** 2026-03-07T22:09:03Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments
- SessionManager spawns `claude -p --input-format stream-json --output-format stream-json` with process_group(0), env_remove(CLAUDECODE), kill_on_drop(true)
- Stdout/stderr streaming via mpsc UnboundedChannel with ANSI stripping and multi-format JSON content extraction
- Full lifecycle: start, stop (SIGTERM with 5s SIGKILL fallback), restart with --resume, shutdown-all, event polling
- Composer widget wrapping tui-textarea with Enter-to-send, Shift+Enter-for-newline, active/inactive border styling

## Task Commits

Each task was committed atomically:

1. **Task 1: SessionManager -- process spawning, streaming, lifecycle** - `614e1e9` (feat)
2. **Task 2: Composer widget wrapping tui-textarea** - `edcb5ea` (feat)

## Files Created/Modified
- `apps/tui/src/session_manager.rs` - SessionManager struct with 9 public methods, SessionEvent enum, JSON parsing helpers
- `apps/tui/src/composer.rs` - Composer struct wrapping tui-textarea with key handling and active/inactive styling
- `apps/tui/src/main.rs` - Added mod composer and mod session_manager declarations

## Decisions Made
- Exit detection via stdout pipe closure (when stdout reader loop ends, process has exited) instead of a separate PID-polling watcher -- simpler, no polling overhead, no nix kill(pid, 0) loop
- JSON content extraction cascade: result.content > content string > content array of blocks > message.content > raw text fallback -- covers all stream-json event formats
- Composer uses static helper `make_textarea()` to DRY configuration across new/clear/set_active operations

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- SessionManager and Composer ready for Plan 03 (TUI + CLI integration wiring)
- SessionManager.poll_events() designed for the existing 250ms tick_interval in main.rs tokio::select! loop
- Composer.handle_key() returns Option<String> ready to feed into SessionManager.send_message()

---
*Phase: 03-session-execution*
*Completed: 2026-03-07*
