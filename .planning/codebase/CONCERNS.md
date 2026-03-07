# Codebase Concerns

**Analysis Date:** 2026-03-07

## Tech Debt

**No tests exist anywhere in the codebase:**
- Issue: Zero unit tests, integration tests, or any `#[test]` or `#[cfg(test)]` blocks across all crates
- Files: `crates/core/src/lib.rs`, `apps/cli/src/main.rs`, `apps/tui/src/main.rs`
- Impact: Core logic (state load/save, `add_repo` validation, `generate_id`) has no automated verification. Regressions can ship silently.
- Fix approach: Add `#[cfg(test)] mod tests` in `crates/core/src/lib.rs` covering `AppState::load`, `AppState::save`, `AppState::add_repo` (duplicate detection, non-directory rejection, canonicalization), and `run_git_status` (success and failure paths). Use `tempdir` for filesystem tests.

**Timestamp-based ID generation is collision-prone:**
- Issue: `generate_id()` in `crates/core/src/lib.rs` (line 89-95) uses millisecond-precision `SystemTime` cast to hex. Two repos added within the same millisecond get the same ID.
- Files: `crates/core/src/lib.rs`
- Impact: Duplicate IDs could cause repo confusion in state. Currently unlikely with manual CLI usage but becomes a real risk with programmatic or scripted use.
- Fix approach: Use a proper unique ID generator (e.g., `uuid` crate or combine timestamp with a random component). Alternatively, use a monotonic counter persisted in state.

**Tokio dependency declared but never used:**
- Issue: `crates/core/Cargo.toml` declares `tokio` with `features = ["full"]` but no async code exists anywhere in the codebase. No `async fn`, no `.await`, no tokio runtime.
- Files: `crates/core/Cargo.toml`
- Impact: Unnecessary compile-time cost and binary bloat. `tokio` with `features = ["full"]` pulls in a significant dependency tree.
- Fix approach: Remove tokio from `crates/core/Cargo.toml` and `Cargo.toml` workspace dependencies until async execution is actually needed (likely Milestone 3 for session streaming).

**TUI runs git commands synchronously on the main thread:**
- Issue: `App::run_status()` in `apps/tui/src/main.rs` (line 67) calls `run_git_status()` which uses `std::process::Command` synchronously. This blocks the entire TUI event loop.
- Files: `apps/tui/src/main.rs` (line 67-81), `crates/core/src/lib.rs` (line 97-109)
- Impact: The UI freezes while git status runs. On large repos or slow filesystems, this creates a noticeable hang. This directly contradicts the brief's priority of "keep the TUI responsive."
- Fix approach: When async execution is introduced (Milestone 3), move command execution to a background task. For now, document the limitation.

**Hardcoded relative state directory:**
- Issue: `AppState` uses a hardcoded relative path `.kommand0-dev/state.json` (line 22-23 in `crates/core/src/lib.rs`). State location depends entirely on the current working directory.
- Files: `crates/core/src/lib.rs`
- Impact: Running `kmd` from different directories creates separate, disconnected state files. Users lose track of their repos if they run the tool from a different location. The TUI binary has the same problem.
- Fix approach: Use a fixed location like `$HOME/.kommand0/state.json` or `$XDG_DATA_HOME/kommand0/state.json`. Make configurable via environment variable as an escape hatch.

**Dead code in TUI error title handling:**
- Issue: The `Status::Error` match arm in the TUI (lines 123-129 of `apps/tui/src/main.rs`) has an `if e.len() > 40` branch that returns the same string `" Output (error) "` in both arms.
- Files: `apps/tui/src/main.rs`
- Impact: Minor - dead/useless conditional. Suggests incomplete implementation (likely intended to show truncated error in the title bar).
- Fix approach: Either implement the truncated error display or remove the conditional.

## Security Considerations

**No git directory validation before running commands:**
- Risk: `add_repo` validates that the path is a directory but does not check if it is actually a git repository. `run_git_status` will run `git -C <path> status` on any directory.
- Files: `crates/core/src/lib.rs` (lines 56-86, 97-109)
- Current mitigation: Git itself returns an error for non-git directories, which is caught and displayed.
- Recommendations: Add a check for `.git` directory or run `git rev-parse --is-inside-work-tree` during `add_repo` to fail early with a clear message.

**State file has no integrity or access controls:**
- Risk: `state.json` is plain JSON written with default permissions. Any local process can read or tamper with it, potentially injecting malicious repo paths that get passed to `git -C`.
- Files: `crates/core/src/lib.rs` (lines 33-54)
- Current mitigation: None. The tool is local-only and single-user, so risk is low.
- Recommendations: For now, acceptable. If the tool ever accepts untrusted state, validate repo paths before passing to shell commands.

## Performance Bottlenecks

**Blocking event loop in TUI:**
- Problem: The entire TUI event loop in `apps/tui/src/main.rs` is synchronous. `event::read()` (line 144) blocks until input arrives, and `run_status()` blocks during git execution.
- Files: `apps/tui/src/main.rs` (lines 94-153)
- Cause: No async runtime, no background threads, no channels. Everything runs on one thread.
- Improvement path: Introduce a tick-based event loop with `event::poll()` and a timeout, or move to an async event loop with tokio. This is a prerequisite for Milestone 3 (streaming session output).

**Full state rewrite on every repo add:**
- Problem: `add_repo` calls `self.save()` which serializes and rewrites the entire `state.json` file every time.
- Files: `crates/core/src/lib.rs` (line 85)
- Cause: Simple file-based persistence with no incremental updates.
- Improvement path: Acceptable for current scale. If state grows (workspaces, sessions), consider a more structured approach. Not urgent.

## Fragile Areas

**State file path coupling:**
- Files: `crates/core/src/lib.rs` (lines 22-31)
- Why fragile: The state directory path is hardcoded as a constant with no injection point. Both CLI and TUI silently depend on being run from the same working directory.
- Safe modification: Extract state path resolution into a function that checks environment variables or uses a home-directory-based default. All callers already go through `AppState::load()` and `AppState::save()`, so the change is localized.
- Test coverage: None. No tests verify load/save behavior.

**TUI terminal cleanup on panic:**
- Files: `apps/tui/src/main.rs` (lines 88-89, 155-156)
- Why fragile: `enable_raw_mode` and `EnterAlternateScreen` are called at startup. If any code between startup and the cleanup at lines 155-156 panics, the terminal is left in raw mode, corrupting the user's shell session.
- Safe modification: Use a panic hook or `scopeguard`/`Drop` guard to ensure `disable_raw_mode()` and `LeaveAlternateScreen` always run.
- Test coverage: None.

## Dependencies at Risk

**No pinned dependency versions beyond major:**
- Risk: Workspace `Cargo.toml` uses broad version specs (e.g., `clap = "4"`, `ratatui = "0.29"`, `tokio = "1"`). While `Cargo.lock` pins exact versions, removing or regenerating the lockfile could pull breaking changes.
- Impact: Ratatui at `0.29` is pre-1.0 and breaking changes between minor versions are common.
- Migration plan: `Cargo.lock` is committed, so this is low risk in practice. Consider pinning ratatui and crossterm more tightly if stability is critical.

**Unused workspace dependencies:**
- Risk: `tracing = "0.1"` is declared in both workspace deps and `crates/core/Cargo.toml` but never used (`use tracing` does not appear anywhere). `thiserror = "2"` is similarly declared but no `#[derive(Error)]` exists.
- Impact: Unnecessary compile-time overhead.
- Migration plan: Remove `tracing` and `thiserror` from `crates/core/Cargo.toml` until actually needed. Keep them in workspace deps if they are planned for near-term use.

## Missing Critical Features

**No repo removal or editing:**
- Problem: Once a repo is added via `kmd repo add`, there is no way to remove or update it short of manually editing `state.json`.
- Blocks: Users who add a wrong path or want to clean up tracked repos.

**No validation that tracked repos still exist:**
- Problem: If a tracked repo's directory is deleted or moved, both CLI list and TUI will show stale entries. The TUI will show an error only when Enter is pressed.
- Blocks: Clean UX when filesystem changes occur outside the tool.

## Test Coverage Gaps

**Entire codebase is untested:**
- What's not tested: All functionality -- state persistence, repo addition/validation, git status execution, CLI argument parsing, TUI rendering and input handling
- Files: `crates/core/src/lib.rs`, `apps/cli/src/main.rs`, `apps/tui/src/main.rs`
- Risk: Any refactoring or feature addition could silently break existing functionality. The brief's Milestone 1 explicitly calls for "basic unit tests for core."
- Priority: High -- this is the number one deliverable for Milestone 1.

---

*Concerns audit: 2026-03-07*
