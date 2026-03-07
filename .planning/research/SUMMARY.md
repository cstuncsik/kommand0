# Project Research Summary

**Project:** Kommand0 -- Async Process Management & Streaming Output
**Domain:** Rust terminal process orchestrator / TUI workspace manager
**Researched:** 2026-03-07
**Confidence:** HIGH

## Executive Summary

Kommand0 is a keyboard-first TUI process orchestrator for parallel coding sessions, built in Rust with ratatui/crossterm. The existing codebase has a working vertical slice (repo registry, TUI selection, git status display) but uses a synchronous event loop that fundamentally cannot support the core value proposition: spawning processes, streaming their output, and managing their lifecycle. The central engineering challenge of this milestone is migrating from a blocking `event::read()` loop to an async `tokio::select!` architecture with message-passing channels -- this is a well-documented pattern in the ratatui ecosystem with official tutorials and templates covering exactly this transition.

The recommended approach is to stabilize the existing codebase first (panic hooks, terminal RAII guards, file locking), then build the workspace domain model and persistence, then tackle async process management as a single focused phase. The stack is already largely in place -- tokio, ratatui, crossterm are declared in Cargo.toml. The key additions are crossterm's `event-stream` feature, `tokio-stream` for stream utilities, `nix` for process group management, and `tracing-subscriber`/`tracing-appender` for file-based logging. No exotic dependencies are needed.

The highest-impact risks are zombie processes from improper child cleanup (must use process groups from day one), terminal state corruption on panic (must install panic hook before adding complexity), and the blocking-to-async migration itself (must be done as a clean cut, not incrementally grafted). All three have well-established mitigation patterns. The research sources are predominantly official documentation (ratatui tutorials, tokio docs, nix crate docs) with high confidence.

## Key Findings

### Recommended Stack

The existing Cargo.toml already declares the core dependencies (tokio, ratatui 0.29, crossterm 0.28, serde, clap 4). Stay on ratatui 0.29/crossterm 0.28 -- ratatui 0.30 has breaking changes and upgrading is unnecessary risk during this milestone.

**New dependencies to add:**
- `crossterm` with `event-stream` feature: converts blocking event polling to async `EventStream` compatible with `tokio::select!`
- `tokio-stream` 0.1: provides `StreamExt` for unified stream handling of crossterm events and process output
- `nix` 0.31 with `signal` + `process` features: safe Rust wrappers for `killpg()` and process group management -- essential for killing entire process trees, not just direct children
- `tracing-subscriber` + `tracing-appender`: file-based logging (TUI apps cannot log to stdout)
- `uuid` 1.x with `v4` + `serde`: proper unique IDs for workspaces and sessions
- `chrono` 0.4 with `serde`: human-readable timestamps for session events

**What NOT to add:** portable-pty (no full terminal emulation needed), vt100/vte parsers (line-by-line output is sufficient), async-process (duplicates tokio), duct (synchronous), tui-textarea (no interactive text input in MVP).

### Expected Features

**Must have (table stakes):**
- Start/stop/restart a command in a workspace
- Stream live stdout/stderr output with scrollback buffer
- Process status indicators (running/stopped/failed/exited)
- Clean shutdown -- all child processes die on quit
- Keyboard-first navigation with consistent bindings
- Split-pane layout (list + output area)
- Workspace persistence across app restarts
- Help overlay showing available keys

**Should have (differentiators):**
- Repo-aware workspaces tied to git repos (partially exists)
- Multi-session per workspace (run multiple commands, switch between outputs)
- Focused/zoomed output view (full-screen single session)
- Session command history per workspace
- Copy mode for selecting text from output

**Defer to v2+:**
- Git worktree integration (high complexity, requires solid workspace UX first)
- Session templates/presets
- Auto-restart policies
- Workspace-scoped environment variables
- Config file import (mprocs.yaml compatibility)

### Architecture Approach

The system uses a two-channel message-passing architecture. The TUI event loop runs on the main thread and owns all UI state exclusively (no Arc/Mutex on AppState). Background tokio tasks handle process spawning and output streaming, communicating via mpsc channels. An `action` channel carries commands from the UI to the process manager (StartSession, StopSession, Quit). An `event` channel carries data from background tasks to the UI (Output lines, ProcessExited, Key events, Ticks).

**Major components:**
1. **EventHandler** -- async task polling crossterm events + tick timer, sends `AppEvent` to main loop
2. **TUI Event Loop** -- main thread; receives events, dispatches actions, owns `Terminal` and `AppState`, triggers renders
3. **ProcessManager** -- spawns/kills child processes via tokio tasks, streams output lines back through event channel
4. **SessionRunner** -- wraps a single `tokio::process::Child` with separate stdout/stderr reader tasks
5. **StateStore** -- JSON file persistence for repos and workspaces, loaded on startup, saved on mutations
6. **Workspace Model** -- domain types in `crates/core`: Workspace, Session, SessionStatus, OutputBuffer

### Critical Pitfalls

1. **Terminal state corruption on panic** -- Install a panic hook that restores terminal (disable raw mode, leave alternate screen) BEFORE the default handler. Add RAII guard struct with `Drop` impl. Must be done first, before adding any complexity.
2. **Zombie processes and orphaned process trees** -- Always spawn with `.process_group(0)` and kill with `killpg(-pgid, SIGTERM)` then escalate to SIGKILL. Never use `child.kill()` alone -- it only signals the direct child, leaving grandchildren alive.
3. **Blocking render loop** -- The current `event::read()` loop must become `tokio::select!` with async `EventStream`. This is not incremental -- it requires restructuring `main.rs` as a clean migration.
4. **Stdout/stderr interleaving** -- Separate async readers produce non-deterministic ordering. Simplest fix: merge stderr into stdout with `2>&1` in the shell wrapper. If distinction is needed, timestamp lines and accept approximate ordering.
5. **Unbounded output buffer memory growth** -- Use `VecDeque` ring buffer with configurable capacity (10,000 lines). Design the buffer data structure before implementing streaming.

## Implications for Roadmap

Based on research, suggested phase structure:

### Phase 1: Stabilization and Infrastructure

**Rationale:** The existing codebase lacks safety nets (panic hooks, terminal guards) and the async foundation that everything else depends on. Adding process management on top of the current synchronous architecture would require a rewrite. Do the migration now while the codebase is small.
**Delivers:** Panic-safe terminal handling, async event loop with `tokio::select!`, file-based tracing, state file locking, RAII terminal guard.
**Addresses:** Keyboard-first navigation foundation, help overlay skeleton.
**Avoids:** Pitfall 1 (terminal corruption), Pitfall 3 (blocking render loop), Pitfall 8 (state file corruption), Pitfall 9 (async runtime misconfiguration).

### Phase 2: Workspace Model and CRUD

**Rationale:** Workspace is the gateway entity -- the entire feature dependency graph flows through it. Session execution, output streaming, and process lifecycle all require a workspace to exist. This phase is pure domain modeling with no async complexity.
**Delivers:** Workspace type in core, workspace CRUD operations, workspace list pane in TUI, workspace CLI commands, persistence in state.json.
**Addresses:** Workspace creation, listing, selection, persistence (4 table-stakes features).
**Avoids:** Pitfall 11 (key handling conflicts) by introducing the focus enum when adding the second pane.

### Phase 3: Session Execution and Process Lifecycle

**Rationale:** This is the core value delivery. It depends on Phase 1 (async infrastructure) and Phase 2 (workspace model). This is the most complex phase with the highest pitfall density -- 7 of 13 pitfalls apply here. It should be implemented as one cohesive phase because process spawning, output streaming, stop/restart, and cleanup are tightly coupled.
**Delivers:** Run commands in workspaces, stream live output, stop/restart sessions, process status indicators, clean shutdown on quit.
**Uses:** tokio::process, nix killpg, crossterm event-stream, tokio-stream, bounded output ring buffer.
**Implements:** ProcessManager, SessionRunner, OutputBuffer, AppEvent/AppAction message passing.
**Avoids:** Pitfall 2 (zombies), Pitfall 4 (output ordering), Pitfall 5 (memory growth), Pitfall 6 (graceless shutdown), Pitfall 7 (channel backpressure), Pitfall 10 (task cancellation).

### Phase 4: UX Polish

**Rationale:** Once the core loop works (create workspace, run command, see output, stop command), polish the experience. These features are independent of each other and can be tackled in any order.
**Delivers:** Scrollback with follow/frozen mode, zoomed output view, multi-session per workspace, copy mode, help overlay completion, terminal size handling.
**Addresses:** Remaining table-stakes (scrollback, help) and key differentiators (zoom, multi-session, copy mode).
**Avoids:** Pitfall 12 (scroll management), Pitfall 13 (terminal size assumptions).

### Phase 5: Git Worktree Integration (Future)

**Rationale:** Deferred per PROJECT.md. High complexity, requires workspace UX to be proven first. This is the killer differentiator for AI coding agent workflows but premature before core process management is solid.
**Delivers:** Create git worktrees per workspace, isolated branch work, worktree lifecycle management.

### Phase Ordering Rationale

- Phase 1 before Phase 3 is non-negotiable: the async event loop is prerequisite infrastructure. Attempting process management on the synchronous loop would be thrown away.
- Phase 2 before Phase 3 because workspace is the domain anchor. Sessions belong to workspaces. Building session execution without the workspace model means rework.
- Phase 3 as a single phase (not split) because spawn/stream/stop/cleanup form a tight feedback loop. Shipping "spawn without stop" or "stream without cleanup" creates a tool that leaks processes.
- Phase 4 after Phase 3 because polish on a non-functional core is wasted effort.
- Phase 5 is explicitly deferred and should only start after Phase 4 validates the workspace UX.

### Research Flags

Phases likely needing deeper research during planning:
- **Phase 3 (Session Execution):** Highest complexity and pitfall density. The process group + killpg pattern is well-documented but the integration with tokio's process module and cancellation tokens needs careful design. Recommend `/gsd:research-phase` for this phase.

Phases with standard patterns (skip research-phase):
- **Phase 1 (Stabilization):** Official ratatui async template and tutorials cover this exactly. Copy the pattern.
- **Phase 2 (Workspace Model):** Pure domain CRUD with JSON persistence. Standard Rust struct + serde. No research needed.
- **Phase 4 (UX Polish):** Individual features are well-documented in ratatui examples (scroll, layout, popup).

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | All dependencies are well-known Rust crates. Versions verified. tokio + ratatui + crossterm is the canonical async TUI stack. |
| Features | HIGH | Feature landscape mapped against 7 comparable tools (mprocs, process-compose, procmux, zellij, tmux, just, devbox). Clear consensus on table stakes. |
| Architecture | HIGH | Two-channel message-passing pattern is documented in official ratatui tutorials and async template. Multiple community examples confirm the approach. |
| Pitfalls | HIGH | Pitfalls sourced from official docs, GitHub issues with reproducer code, and well-known Unix process semantics. Each has concrete prevention code. |

**Overall confidence:** HIGH

### Gaps to Address

- **shared_child crate necessity:** Rated MEDIUM confidence in STACK.md. May be possible to use `Arc<Mutex<Child>>` instead. Validate during Phase 3 implementation -- if the race conditions between wait and kill are manageable without it, skip the dependency.
- **Output ordering strategy:** The "merge stderr into stdout" approach is simplest but loses the ability to color stderr differently. Decide during Phase 3 planning whether this tradeoff is acceptable or if timestamp-based approximate ordering is worth the complexity.
- **Bounded channel backpressure:** The "lossy send" pattern (drop old lines when channel is full) needs prototyping. If output-heavy commands drop too many lines, users will notice. May need a batching strategy instead.
- **Integration testing for process cleanup:** No test infrastructure exists. Verifying that zombie processes are properly reaped requires spawning real processes in tests. Plan this during Phase 3.

## Sources

### Primary (HIGH confidence)
- Ratatui official tutorials (async counter, event stream, component architecture)
- Ratatui async template (GitHub repo + docs)
- Tokio official docs (process module, channels, select!)
- nix crate docs (killpg, signal, process)
- crossterm docs (event-stream feature, EventStream)

### Secondary (MEDIUM confidence)
- mprocs, process-compose, procmux GitHub repos (feature comparison)
- Tokio GitHub issues #2504 (process group cleanup), #1386 (stdout/stderr ordering)
- Rust std::process::Child docs and issue #115241 (kill does not kill children)
- Community examples (d-holguin/async-ratatui, ratatui discussions #220)

### Tertiary (LOW confidence)
- shared_child crate (small crate, limited recent activity -- validate before adopting)
- PTY-based output capture blog post (reference for why NOT to use PTY, not directly applicable)

---
*Research completed: 2026-03-07*
*Ready for roadmap: yes*
