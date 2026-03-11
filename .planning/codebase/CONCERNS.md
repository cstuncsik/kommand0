# Codebase Concerns

**Analysis Date:** 2026-03-11

## Tech Debt

**TUI main.rs is a monolithic 1282-line file with massive code duplication:**
- Issue: `apps/tui/src/main.rs` contains all event handling, session lifecycle, and state management in a single `run()` function. Session start/restart/resume logic is duplicated across Enter key, `r` key, `R` key, button click handler, and auto-resume on startup -- at least 5 nearly identical blocks.
- Files: `apps/tui/src/main.rs` (lines 819-971 for key handlers, lines 1096-1151 for button handlers, lines 386-467 for auto-resume)
- Impact: Adding a new session lifecycle action requires changes in multiple places. Easy to forget one, causing inconsistent behavior.
- Fix approach: Extract a `start_session_for_workspace()` method on `App` and a `resume_session_for_workspace()` method. Each handler calls the shared method. This consolidates the pattern of: create session record, call session_manager.start_session, update PID, save state, update scrollback, set focus.

**ID generation uses millisecond timestamps -- not unique under concurrent use:**
- Issue: `generate_id()` in `crates/core/src/id.rs` returns `format!("{:x}", millis)` where `millis` is the current time in milliseconds. Two calls within the same millisecond produce identical IDs. Session IDs already use `uuid::Uuid::new_v4()` but repo and workspace IDs use this weaker scheme.
- Files: `crates/core/src/id.rs`
- Impact: If two repos or workspaces are added in the same millisecond (unlikely from CLI, possible from TUI or automated scripts), IDs collide silently. State corruption possible since lookup by ID would be ambiguous.
- Fix approach: Replace `generate_id()` body with `uuid::Uuid::new_v4().to_string()` (uuid is already a workspace dependency). Alternatively, use a shorter format like `nanoid`.

**Duplicated state between App fields and AppState:**
- Issue: `App` struct stores `repos` and `workspaces` as separate `Vec` fields cloned from `AppState`. After any mutation, code must manually sync: `app.repos = app.state.repos.clone()` and `app.workspaces = app.state.workspaces.clone()`. Several mutation sites in `main.rs` update one but not the other, or forget entirely.
- Files: `apps/tui/src/main.rs` (App struct lines 58-88, sync points scattered throughout)
- Impact: Stale data displayed in TUI tree view after mutations. Tree rebuilds from the stale `app.repos`/`app.workspaces` rather than `app.state.repos`/`app.state.workspaces`.
- Fix approach: Remove the duplicated fields. Have `rebuild_tree()` and other methods read directly from `self.state.repos` and `self.state.workspaces`. Or add a single `sync_from_state()` method called after every mutation.

**Render.rs is 897 lines with duplicated layout logic:**
- Issue: `render_right_pane()` and `render_zoomed()` share substantial duplicated output rendering, scrollbar, and composer rendering logic.
- Files: `apps/tui/src/render.rs` (lines 149-434 for right pane, lines 738-871 for zoomed)
- Impact: Bug fixes to output rendering must be applied in two places.
- Fix approach: Extract shared rendering into helper functions (partially done with `render_output_content` and `render_scrollbar`, but composer rendering and streaming text assembly are still duplicated).

**`#[allow(dead_code)]` used liberally to suppress warnings:**
- Issue: Multiple structs and impl blocks are annotated with `#[allow(dead_code)]`: `App` struct, `ScrollbackBuffer` impl, `SessionManager` impl, `Status` enum, `HitAction` variants. This masks genuinely unused code.
- Files: `apps/tui/src/main.rs` (lines 26, 57), `apps/tui/src/scrollback.rs` (lines 4, 11), `apps/tui/src/session_manager.rs` (line 64), `apps/tui/src/buttons.rs` (lines 9, 25, 61)
- Impact: Dead code accumulates without compiler warnings. Difficult to know which fields/methods are actually used.
- Fix approach: Remove blanket `#[allow(dead_code)]` from impl blocks. Add targeted allows only on specific items that are intentionally reserved for future use, with a comment explaining why.

## Known Bugs

**CLI session start spawns fire-and-forget process without monitoring:**
- Symptoms: `kmd session start` spawns a `claude` subprocess and records the PID, but never monitors whether it exits. The session status stays "Running" in state.json even if the process crashed immediately.
- Files: `apps/cli/src/main.rs` (lines 287-318)
- Trigger: Run `kmd session start <workspace>` when `claude` is not installed or when it fails early.
- Workaround: Manually run `kmd session stop <workspace>`.

**Process group signal sends negative PID without verifying process group:**
- Symptoms: In `apps/cli/src/main.rs` (lines 335-344), `kill(Pid::from_raw(-pgid), Signal::SIGTERM)` assumes the child is a process group leader. If `process_group(0)` was not set (CLI path uses `std::process::Command` which does not call `.process_group(0)`), this sends SIGTERM to an unrelated process group.
- Files: `apps/cli/src/main.rs` (lines 335-344)
- Trigger: CLI `kmd session stop` on any session started via CLI.
- Workaround: The TUI path correctly sets `.process_group(0)` in `session_manager.rs` (line 105). CLI users should use TUI for session management.

**`is_running()` in SessionManager checks map presence, not actual process state:**
- Symptoms: `session_manager.is_running(session_id)` returns `true` as long as the session entry exists in the HashMap, even after the child process has exited (since the Exited event only removes on explicit `stop_session` call).
- Files: `apps/tui/src/session_manager.rs` (lines 366-369)
- Trigger: Process exits naturally; `is_running()` still returns true until session is removed from map.
- Workaround: Check `app.state` session status instead, which is updated on `SessionEvent::Exited`.

## Security Considerations

**State file stored in predictable location without access control:**
- Risk: `.kommand0-dev/state.json` is written with default umask permissions. It contains repo paths, workspace paths, session PIDs, and Claude session IDs. The Claude session ID could potentially be used for session hijacking.
- Files: `crates/core/src/lib.rs` (lines 50-57 -- `save_to` method)
- Current mitigation: None. File is created with whatever the user's default umask is.
- Recommendations: Set file permissions to 0600 on state.json. Consider storing Claude session IDs separately or not at all.

**Log files contain full conversation history in plaintext:**
- Risk: `.kommand0-dev/sessions/*.log` files contain complete user prompts and Claude responses as JSON lines. These could contain secrets, code, or sensitive instructions.
- Files: `apps/tui/src/main.rs` (lines 329-353 -- `write_log` method)
- Current mitigation: None.
- Recommendations: Set log file permissions to 0600 on creation. Consider adding a `--no-log` flag. Document that log files may contain sensitive content.

**`env_remove("CLAUDECODE")` is the only environment sanitization:**
- Risk: Both CLI and TUI pass the full parent environment to child `claude` processes, minus `CLAUDECODE`. Any sensitive environment variables (database URLs, API keys) are inherited.
- Files: `apps/tui/src/session_manager.rs` (line 104), `apps/cli/src/main.rs` (line 299)
- Current mitigation: Only `CLAUDECODE` is removed.
- Recommendations: Consider using an allowlist for environment variables passed to child processes, or at minimum document the inherited-environment behavior.

## Performance Bottlenecks

**`all_lines()` copies entire scrollback as Vec on every render frame:**
- Problem: `ScrollbackBuffer::all_lines()` creates a new `Vec<&str>` from all lines (up to 50,000) every frame. This is called in both `render_right_pane()` and `render_zoomed()`.
- Files: `apps/tui/src/scrollback.rs` (lines 101-103), `apps/tui/src/render.rs` (lines 198-200, 773-775)
- Cause: The renderer needs all lines to compute visual (wrapped) line counts for proper scroll positioning.
- Improvement path: Pass an iterator or slice reference instead of copying to a Vec. Or cache the visual line count and only recompute when lines are added.

**`styled_total_visual()` recomputes on every frame:**
- Problem: Visual line count (accounting for wrapping) is recalculated by iterating every styled line, summing span lengths, and computing wrapped heights. This runs twice per frame (once for clamping, once for rendering).
- Files: `apps/tui/src/render.rs` (lines 695-702, called at lines 214+222 and 789+809)
- Cause: No caching of visual dimensions between frames.
- Improvement path: Cache the total visual line count in `ScrollbackBuffer` and invalidate when lines are added or terminal is resized.

**Tick interval at 50ms (20 FPS) with forced redraws:**
- Problem: The TUI redraws every iteration of the event loop (every tick or event), even when nothing changed. At 50ms tick interval, this is 20 redraws/second minimum.
- Files: `apps/tui/src/main.rs` (line 470 -- `interval(Duration::from_millis(50))`, line 473 -- unconditional `terminal.draw()`)
- Cause: No dirty-tracking mechanism.
- Improvement path: Add a `needs_redraw` flag. Only call `terminal.draw()` when state has actually changed (event received, tick with active spinner, etc.).

## Fragile Areas

**Session lifecycle state machine (start/stop/restart/resume) across TUI main.rs:**
- Files: `apps/tui/src/main.rs` (entire file), `apps/tui/src/session_manager.rs`
- Why fragile: Session lifecycle is spread across ~5 code paths that must all maintain consistent invariants: create state record, start process, update PID, save state, update scrollback, update focus, set active_session_id. Missing any step in any path causes subtle bugs (orphaned processes, stale UI, lost session IDs).
- Safe modification: When changing session lifecycle, search for ALL occurrences of `create_session`, `start_session`, `update_session_status`, `active_session_id` to ensure all paths are updated.
- Test coverage: Zero tests for TUI session lifecycle. Only the core state machine has tests.

**Streaming text accumulation and flushing:**
- Files: `apps/tui/src/main.rs` (lines 1164-1206), `apps/tui/src/render.rs` (lines 198-207, 773-782)
- Why fragile: Streaming deltas accumulate in `app.streaming_text` HashMap, flush completed lines to scrollback on `\n`, and the renderer appends the partial line separately. The `StreamEnd` event flushes remaining text. If events arrive out of order or are dropped, text can be lost or duplicated.
- Safe modification: The streaming pipeline has three stages (accumulate -> flush lines -> render partial). Changes to any stage must consider the other two. Add integration tests for streaming scenarios.
- Test coverage: No tests for streaming accumulation or flushing.

**`render.rs` markdown parser:**
- Files: `apps/tui/src/render.rs` (lines 446-654 -- `parse_inline_markdown`, `style_markdown_line`)
- Why fragile: Hand-rolled character-by-character markdown parser handles bold, italic, inline code, code blocks, headers, bullets, numbered lists, and user chat bubbles. Edge cases (nested formatting, unclosed markers, multi-byte characters) may cause panics or garbled output.
- Safe modification: Test with diverse markdown inputs before changing. The `truncate_path` function (lines 13-22) slices by byte offset which can panic on multi-byte characters.
- Test coverage: No tests for the markdown parser.

## Scaling Limits

**Scrollback buffer capped at 50,000 lines per workspace:**
- Current capacity: 50,000 lines per workspace session, stored in memory as `VecDeque<String>`.
- Limit: With many concurrent workspaces, memory usage grows proportionally. 10 workspaces with full buffers at ~200 bytes/line averages ~100MB.
- Scaling path: Consider writing scrollback to disk (memory-mapped file or append-only log) with only a viewport window in memory.

**State file is a single JSON file with no locking:**
- Current capacity: Works for single-user, single-process use.
- Limit: CLI and TUI can both read/write `.kommand0-dev/state.json` simultaneously, causing data loss via last-writer-wins. No file locking mechanism.
- Scaling path: Add file locking (flock) or use a lightweight database (SQLite).

## Dependencies at Risk

**Tight coupling to `claude` CLI binary interface:**
- Risk: The application depends on `claude` CLI accepting specific flags (`-p`, `--verbose`, `--input-format stream-json`, `--output-format stream-json`, `--resume`) and producing specific JSON event formats (`content_block_delta`, `content_block_stop`, `message_stop`, `session_id`). Any breaking change to the Claude CLI protocol breaks session management.
- Impact: All session functionality in both CLI and TUI.
- Migration plan: Abstract the Claude CLI interaction behind a trait/interface so alternative backends or protocol versions can be swapped in.

## Missing Critical Features

**No error feedback in TUI for failed operations:**
- Problem: When `add_repo`, `create_workspace`, `start_session`, etc. fail, errors are silently swallowed with `let _ = ...` in many TUI code paths.
- Blocks: Users cannot diagnose why a session fails to start or why a workspace cannot be created.

**No tracing/logging for the TUI application itself:**
- Problem: `tracing` is a workspace dependency but not initialized anywhere in the TUI. Debugging production issues requires adding print statements.
- Blocks: Diagnosing intermittent issues, understanding session lifecycle in production.

## Test Coverage Gaps

**TUI has zero tests:**
- What's not tested: All of `apps/tui/src/` -- event handling, rendering, session management integration, mouse interaction, modal dialogs, scrollback rendering, markdown parsing, streaming accumulation.
- Files: `apps/tui/src/main.rs`, `apps/tui/src/render.rs`, `apps/tui/src/session_manager.rs`, `apps/tui/src/scrollback.rs` (has unit tests), `apps/tui/src/composer.rs`, `apps/tui/src/modal.rs`, `apps/tui/src/mouse.rs`
- Risk: Any refactoring of the TUI (especially the monolithic main.rs) has no safety net. Session lifecycle bugs are likely.
- Priority: High -- `scrollback.rs` has tests but is the only TUI module with coverage. The markdown parser in `render.rs` and session lifecycle in `main.rs` are the highest-risk untested areas.

**CLI has zero tests:**
- What's not tested: All CLI command handling in `apps/cli/src/main.rs`.
- Files: `apps/cli/src/main.rs`
- Risk: Regressions in CLI output formatting, flag handling, or confirmation prompts go unnoticed.
- Priority: Medium -- the CLI is thin and delegates to core, which has good test coverage.

**Core worktree module tests depend on real git operations:**
- What's not tested: Worktree tests in `crates/core/src/worktree.rs` run actual `git init`, `git commit`, `git worktree add` commands. They pass in CI but are slow and may fail in unusual git configurations (no global user.name/user.email set).
- Files: `crates/core/src/worktree.rs` (lines 174-289)
- Risk: Flaky CI if git configuration differs between environments.
- Priority: Low -- these tests are valuable as integration tests, but consider setting local git config in test setup.

---

*Concerns audit: 2026-03-11*
