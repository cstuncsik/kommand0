# Phase 1: Stabilization and Async Foundation - Context

**Gathered:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Harden the existing codebase (naming, tests, error handling, README) and migrate the TUI event loop from synchronous `crossterm::event::read()` to async `tokio::select!` with `crossterm::event::EventStream`. This phase does NOT add new features — it makes the existing vertical slice safe and ready for process management in Phase 3.

</domain>

<decisions>
## Implementation Decisions

### Naming cleanup
- Current naming is already consistent: `AppState`, `RepoEntry`, `run_git_status` follow Rust conventions
- Binary names (`kmd`, `kommand0-tui`) are fine as-is
- Module structure is flat (single `lib.rs`) — no reorganization needed at this stage
- Review during implementation: check for any stale comments or misleading doc strings

### Async migration approach
- Convert TUI `main()` to `#[tokio::main]` with `tokio::select!` event loop
- Add `event-stream` feature to existing crossterm 0.28 dependency (no version bump needed)
- Replace blocking `event::read()` with `crossterm::event::EventStream` + `.next().fuse()`
- Add a tick timer (~250ms) for future UI refresh needs
- Keep CLI synchronous — only TUI needs async
- Do NOT change `run_git_status()` to async yet — that happens in Phase 3 when session execution is built

### Panic hook and terminal safety
- Install a panic hook that disables raw mode and exits alternate screen before printing the panic message
- Use `std::panic::set_hook()` in TUI `main()` before entering raw mode
- Pattern: capture the default panic hook, wrap it with terminal cleanup, reinstall
- Also wrap the main event loop in a `catch_unwind` or use RAII guard pattern for terminal restoration on normal error exits

### Test coverage scope
- Add unit tests in `crates/core/src/lib.rs` (or `tests/` module):
  - `AppState::load()` — returns default when no file exists
  - `AppState::save()` + `load()` roundtrip — state survives serialization
  - `AppState::add_repo()` — validates path exists, rejects duplicates, canonicalizes path
  - `run_git_status()` — handles missing repo path, non-git directory gracefully
- Use `tempdir` (or `tempfile` crate) for test isolation — don't rely on real filesystem state
- Do NOT add integration tests for the TUI event loop — that's manual verification
- Do NOT add tests for the CLI binary — integration testing comes later

### README content
- Build instructions: `cargo build --workspace`
- Run CLI: `cargo run -p kommand0-cli -- repo add <path>` and `cargo run -p kommand0-cli -- repo list`
- Run TUI: `cargo run -p kommand0-tui`
- Test: `cargo test --workspace`
- Prerequisites: Rust toolchain (edition 2024), git on PATH, macOS
- Keep it short — this is a dev tool, not a product landing page

### Git status edge cases
- `run_git_status()` should handle: path doesn't exist, path is a file not a directory, path is not a git repo
- Current implementation already handles these via `git` exit code + stderr capture
- Add explicit validation: check directory exists before calling git (bail early with clear message)
- Test these edge cases in unit tests

### Claude's Discretion
- Exact tick timer interval (200-500ms range is fine)
- Whether to use `better-panic` crate or hand-roll the panic hook
- Test helper organization (inline `#[cfg(test)]` module vs separate test files)
- README formatting and section ordering
- Whether to add `tracing-subscriber` initialization in this phase or defer to Phase 3

</decisions>

<specifics>
## Specific Ideas

- The brief says "simple code is preferred over abstractions" — don't over-engineer the async migration
- The brief says "avoid broad refactors" — the async migration should be surgical: change the event loop, keep everything else
- Research recommends staying on ratatui 0.29 / crossterm 0.28 — don't upgrade
- Research recommends adding `nix` crate for process groups — defer to Phase 3, not needed here

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `AppState` (crates/core/src/lib.rs): Well-structured, load/save/add_repo — add tests for these
- `run_git_status()` (crates/core/src/lib.rs:97): Works, just needs edge case tests
- `App` struct (apps/tui/src/main.rs:24): TUI view state — will be refactored for async but core structure is sound

### Established Patterns
- `anyhow::Result` everywhere — continue this pattern
- `bail!()` for validation failures — continue
- `.with_context()` for I/O errors — continue
- Workspace deps via `.workspace = true` — add new deps this way
- `Status` enum for TUI state — extend this pattern as needed

### Integration Points
- TUI `main()` at apps/tui/src/main.rs:84 — this is the function being migrated to async
- crossterm raw mode / alternate screen at lines 88-89 — must be properly guarded
- Event loop at line 94-153 — replace with tokio::select! loop
- `Cargo.toml` workspace deps — add `crossterm` event-stream feature, possibly `tempfile` for tests

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 01-stabilization-and-async-foundation*
*Context gathered: 2026-03-07*
