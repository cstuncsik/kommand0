# Codebase Concerns

**Analysis Date:** 2026-03-22

## Error Handling & Panic Risk

**Unwrap calls in critical paths:**
- Issue: 112 `unwrap()` calls across codebase, primarily in session management and state handling
- Files: `apps/tui/src/main.rs`, `apps/tui/src/session_manager.rs`, `crates/core/src/lib.rs`, `crates/core/src/workspace.rs`, `crates/core/src/worktree.rs`
- Impact: Runtime panics possible when:
  - Session not found in HashMap during streaming (line 1404 in `apps/tui/src/main.rs`: `app.streaming_text.get_mut(&ws_id).unwrap()`)
  - State lookup fails unexpectedly
  - System time read fails (handled with `expect("time went backwards")` in `crates/core/src/lib.rs:244`)
- Fix approach: Replace panic-prone unwraps with Result propagation or defensive default values where appropriate

**Test panics:**
- Issue: Tests use `panic!()` for assertions instead of proper assertions
- Files: `crates/core/src/worktree.rs` lines 206, 238, 285
- Example: `panic!("expected fallback")` instead of `assert!`
- Impact: Test failures halt execution rather than reporting clearly
- Fix approach: Replace all test `panic!()` with `assert!()` or `assert_eq!()`

## Session Management Fragility

**Process lifecycle edge cases:**
- Issue: Session restart doesn't block on old process termination
- File: `apps/tui/src/session_manager.rs` line 291-296
- Problem: `restart_session()` removes the old session from HashMap (best-effort cleanup via `kill_on_drop`), but doesn't wait for the process to fully exit before spawning a new one
- Impact: Race condition possible if Claude CLI outputs are still being read by old reader tasks while new session starts
- Fix approach: Add explicit wait-for-exit logic before spawning new session

**Shutdown timeout behavior:**
- Issue: `shutdown_all()` uses fixed 5-second timeout for all sessions
- File: `apps/tui/src/session_manager.rs` lines 313-323
- Problem: Deadline calculation (`deadline.saturating_duration_since(tokio::time::Instant::now())`) is correct but if multiple sessions take >5 seconds total, some may be killed without SIGTERM
- Impact: Ungraceful shutdown of some sessions, possible orphaned processes
- Fix approach: Implement per-session timeout with cooperative shutdown

**Process group handling on non-Unix platforms:**
- Issue: SIGTERM/SIGKILL logic uses Unix-specific process groups (`process_group(0)`, negative PID)
- Files: `apps/tui/src/session_manager.rs`, `apps/cli/src/main.rs`
- Platform limitation: Documented as macOS-only in README, but code would panic on other platforms
- Fix approach: Add platform guards or fail gracefully on non-Unix

## State Management & Concurrency

**HashMap.get().unwrap() in event loop:**
- Issue: Multiple locations assume session/workspace exists when looking up by ID
- File: `apps/tui/src/main.rs` lines 1388-1390, 1416-1418, 1433-1435
- Pattern: `app.state.sessions.iter().find(|s| s.id == session_id).map(|s| s.workspace_id.clone())`
- Risk: If SessionManager and AppState get out of sync (session cleaned up but event still fires), map returns None
- Impact: Events silently dropped, no error logging
- Fix approach: Add defensive checks and error logging, ensure SessionManager state always matches AppState

**Streaming text buffer lifetime:**
- Issue: Partial streaming text accumulated in `app.streaming_text` HashMap with no timeout
- File: `apps/tui/src/main.rs` lines 87-88, 1404-1410
- Problem: If StreamEnd event is lost or session crashes mid-stream, text remains buffered forever
- Impact: Memory leak of partial responses, potential display of stale text on next session
- Fix approach: Add automatic flush on session Exited/Error events, cap buffer size per workspace

**RAYON/TOKIO context mixing:**
- Issue: `spawn()` calls in session_manager assume tokio runtime available
- File: `apps/tui/src/session_manager.rs` lines 138, 200
- Risk: Reader tasks spawned from synchronous methods; if runtime is dropped before tasks complete, undefined behavior
- Fix approach: Ensure TaskJoinHandles are stored and awaited cleanly on shutdown

## Modal Input & Validation

**Path completion edge cases:**
- Issue: Tab-completion in AddRepo modal regenerates completions on each Tab press
- File: `apps/tui/src/modal.rs` lines 91-114
- Problem: `complete_path()` function generates all candidates; no caching; cursor position may not match input after cycling
- Impact: Performance lag with many directories; inconsistent cursor positioning
- Fix approach: Cache completions until text changes; normalize cursor after completion selection

**Empty path submission:**
- Issue: Modal validates empty paths but doesn't clear completions
- File: `apps/tui/src/modal.rs` lines 82-84
- Problem: After error, completions array persists; next Tab press starts from stale list
- Impact: Confusing UX; stale suggestions shown
- Fix approach: Clear completions on error

## Large File Risk

**TUI main.rs at 1523 lines:**
- File: `apps/tui/src/main.rs`
- Issue: Event loop, state mutations, session management, UI updates all in one file
- Impact: Hard to test, debug, or reason about; mutation-heavy event dispatch
- Fix approach: Extract event handling into smaller modules (dispatch.rs, events.rs)

**Render module at 1518 lines:**
- File: `apps/tui/src/render.rs`
- Issue: All rendering logic monolithic; tree rendering, buttons, status bar, zoom mode all interleaved
- Impact: Changes to one pane affect others unexpectedly; hard to unit test
- Fix approach: Split into: render_tree.rs, render_output.rs, render_composer.rs, render_status.rs

## Test Coverage Gaps

**Integration test missing:**
- What's not tested: End-to-end session lifecycle with actual Claude CLI process
- Files: No E2E test files found
- Risk: Session manager changes could break process communication without detection
- Priority: High - this is a critical path

**State persistence not tested:**
- What's not tested: JSON serialization roundtrip with edge cases (special characters in paths, Unicode in workspace names)
- Files: `crates/core/src/lib.rs` has basic tests but no property-based testing
- Risk: State file corruption possible with unusual inputs
- Priority: Medium

**Modal error recovery untested:**
- What's not tested: Multiple consecutive error submissions in AddRepo modal
- Files: `apps/tui/src/modal.rs`
- Risk: Error state accumulation not validated
- Priority: Low

## Memory & Performance

**ScrollbackBuffer unbounded allocation potential:**
- Issue: Capacity capped at parameter but VecDeque allocated with `with_capacity(capacity.min(10_000))`
- File: `apps/tui/src/scrollback.rs` line 15
- Problem: If scrollback is created with capacity > 10,000, allocated capacity is 10,000 but pushed_line() enforces full capacity
- Impact: Mismatch between allocated and managed capacity; memory usage spikes after 10,000 lines
- Fix approach: Use consistent capacity limit throughout

**HashMap clones on every App creation:**
- File: `apps/tui/src/main.rs` lines 94-95
- Issue: `repos` and `workspaces` cloned from `AppState` into separate App fields; duplicated memory
- Impact: With 100+ workspaces, doubling memory footprint
- Fix approach: Use references or Arc<> instead of clones

**String cloning in event handlers:**
- Issue: 142 `clone()` calls in TUI code; many in hot paths (event loop tick)
- File: `apps/tui/src/main.rs` throughout
- Example: Session ID cloned multiple times per event (lines 1388-1390)
- Impact: Unnecessary allocations every 50ms
- Fix approach: Use Cow<str> or borrowing where possible

## Security Considerations

**Environment variable cleanup incomplete:**
- Issue: Only `CLAUDECODE` is removed before spawning Claude session
- File: `apps/tui/src/session_manager.rs` line 112: `.env_remove("CLAUDECODE")`
- Risk: Other sensitive env vars (API keys, tokens) inherited from parent process
- Fix approach: Explicit whitelist of allowed env vars instead of blacklist

**State file permissions:**
- Issue: State JSON written with default file mode; may be readable by other users
- File: `crates/core/src/lib.rs` lines 50-57
- Risk: Workspace paths and session info exposed if home directory has open permissions
- Fix approach: Set mode 0600 on state.json after write

**No input sanitization in workspace/repo names:**
- Issue: Names used in git commands, paths without validation
- File: `crates/core/src/worktree.rs` lines 36-52 (unique_branch_name)
- Risk: Injection if workspace name contains shell metacharacters
- Fix approach: Validate alphanumeric + hyphen, or escape for shell

## Architecture & Design Issues

**Bidirectional dependency between AppState and SessionManager:**
- Issue: SessionManager spawns processes, AppState manages sessions, but no clear ownership
- Files: `apps/tui/src/main.rs` (orchestrates both)
- Problem: State inconsistency possible if one is updated without the other
- Fix approach: Move session lifecycle into AppState or create separate SessionState that both read

**Modal state machine incomplete:**
- Issue: ModalState has three variants but transitions aren't exhaustive
- File: `apps/tui/src/modal.rs` lines 18-39
- Problem: Cancelling AddWorkspace leaves repo_id set; could leak into next modal
- Fix approach: Explicitly clear all fields on cancel

**No logger abstraction:**
- Issue: `write_log()` method writes JSON files but no structured logging/rotation/archival
- File: `apps/tui/src/main.rs` (write_log method not shown in sample)
- Risk: Logs grow unbounded; no visibility into errors
- Fix approach: Implement rotating file handler with tracing crate

## Known Operational Constraints

**macOS-only platform support:**
- Limitation: Code uses Unix-only features without guards
- Files: `crates/core/src/worktree.rs`, `apps/tui/src/session_manager.rs`
- Example: Process groups, SIGTERM/SIGKILL, git worktree creation
- Workaround: Can be added but requires platform-specific tests

**Git worktree not created if repo not git:**
- Issue: Graceful fallback to repo root, but silently; no user feedback
- File: `crates/core/src/lib.rs` lines 248-259
- Impact: User doesn't know workspace is using repo root instead of isolated worktree
- Fix approach: Log or return WorktreeResult reason to caller

**Fixed 50ms event loop tick:**
- Issue: Hard-coded tick_interval.tick() cycle
- File: `apps/tui/src/main.rs` (not shown in sample but used in main event loop)
- Concern: May be too fast (CPU usage) or too slow (responsiveness) depending on system
- Fix approach: Make configurable or adaptive

---

*Concerns audit: 2026-03-22*
