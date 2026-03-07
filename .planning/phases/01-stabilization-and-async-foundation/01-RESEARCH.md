# Phase 1: Stabilization and Async Foundation - Research

**Researched:** 2026-03-07
**Domain:** Rust TUI stabilization (testing, async migration, panic safety)
**Confidence:** HIGH

## Summary

Phase 1 hardens the existing kommand0 vertical slice. The codebase is small (~270 lines across 3 crates) with no tests, a synchronous TUI event loop, and no panic safety. The work is well-scoped: add unit tests for core logic, migrate the TUI event loop from blocking `crossterm::event::read()` to async `tokio::select!` with `EventStream`, install a panic hook for terminal restoration, and update the README.

A key discovery: ratatui 0.29 (already in use) provides built-in `ratatui::init()` and `ratatui::restore()` functions that handle raw mode, alternate screen, AND panic hook installation automatically. This eliminates the need to hand-roll panic hooks entirely. The async migration requires adding the `event-stream` feature to crossterm 0.28 (already the version in the dependency tree) and adding `tokio` + `futures` to the TUI crate.

**Primary recommendation:** Use `ratatui::init()` for terminal setup + panic hook (STAB-06 for free), then build the async event loop with `crossterm::event::EventStream` + `tokio::select!` in a straightforward main loop -- no channel/mpsc abstraction needed at this stage.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Naming is already consistent; no reorganization needed -- just review for stale comments
- Convert TUI `main()` to `#[tokio::main]` with `tokio::select!` event loop
- Add `event-stream` feature to existing crossterm 0.28 dependency
- Replace blocking `event::read()` with `EventStream` + `.next().fuse()`
- Add a tick timer (~250ms) for future UI refresh needs
- Keep CLI synchronous -- only TUI needs async
- Do NOT change `run_git_status()` to async yet (Phase 3)
- Install panic hook that disables raw mode and exits alternate screen
- Use `std::panic::set_hook()` in TUI `main()` before entering raw mode
- Unit tests in `crates/core/src/lib.rs`: AppState load/save roundtrip, add_repo validation, run_git_status edge cases
- Use `tempfile` crate for test isolation
- Do NOT add integration tests for TUI event loop
- Do NOT add tests for CLI binary
- README: build, run CLI, run TUI, test instructions, prerequisites

### Claude's Discretion
- Exact tick timer interval (200-500ms range is fine)
- Whether to use `better-panic` crate or hand-roll the panic hook
- Test helper organization (inline `#[cfg(test)]` module vs separate test files)
- README formatting and section ordering
- Whether to add `tracing-subscriber` initialization in this phase or defer to Phase 3

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| STAB-01 | Codebase naming is consistent across core, cli, and tui | Current naming already follows Rust conventions; review for stale comments only |
| STAB-02 | Package boundaries match architecture direction | Structure is correct (core/cli/tui); no changes needed |
| STAB-03 | Unit tests exist for core logic (state load/save, add_repo validation) | Use `tempfile` crate for isolation; inline `#[cfg(test)]` module pattern; test patterns documented below |
| STAB-04 | README has accurate build/run/test instructions | Current README exists but incomplete; update with workspace commands and prerequisites |
| STAB-05 | Git status execution handles edge cases | Add pre-validation in `run_git_status()` before spawning git; test with tempfile |
| STAB-06 | Panic hook restores terminal state on crash | Use `ratatui::init()` which installs panic hook automatically; OR manual `set_hook` pattern |
| STAB-07 | TUI event loop migrated to async | crossterm `event-stream` feature + `tokio::select!` + `futures::StreamExt` |
</phase_requirements>

## Standard Stack

### Core (already in workspace)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| ratatui | 0.29.0 | TUI framework | Standard Rust TUI library; has built-in init/restore/panic-hook |
| crossterm | 0.28.1 | Terminal backend | Already pulled by ratatui 0.29; add `event-stream` feature |
| tokio | 1.x (full) | Async runtime | Standard async runtime; already in workspace deps |
| anyhow | 1.x | Error handling | Already used throughout; continue pattern |
| serde + serde_json | 1.x | Serialization | Already used for AppState persistence |

### New Dependencies Needed
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| tempfile | 3.x | Test isolation | `[dev-dependencies]` in core crate for filesystem test isolation |
| futures | 0.3 | Stream combinators | TUI crate needs `StreamExt` for `EventStream::next().fuse()` |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Hand-rolled panic hook | `ratatui::init()` built-in | `ratatui::init()` handles panic hook, raw mode, alt screen in one call -- simpler |
| mpsc channel event handler | Direct `tokio::select!` in main loop | Channel pattern adds abstraction; direct select! is simpler for current needs |
| `better-panic` crate | Manual `set_hook` or `ratatui::init()` | Not needed -- `ratatui::init()` handles the panic hook requirement |

**Recommendation on `ratatui::init()` vs manual panic hook:** The CONTEXT.md says "use `std::panic::set_hook()` in TUI `main()` before entering raw mode." However, `ratatui::init()` does exactly this internally (verified by reading source). Using `ratatui::init()` is the idiomatic ratatui 0.29 approach and satisfies the requirement with less code. The planner should use `ratatui::init()` unless there's a specific reason not to.

**Installation (workspace Cargo.toml changes):**
```toml
# Add to [workspace.dependencies]
crossterm = { version = "0.28", features = ["event-stream"] }
futures = "0.3"
tempfile = "3"
```

```toml
# apps/tui/Cargo.toml - add:
tokio.workspace = true
futures.workspace = true

# crates/core/Cargo.toml - add:
[dev-dependencies]
tempfile.workspace = true
```

## Architecture Patterns

### Current Project Structure (no changes needed)
```
apps/
  cli/src/main.rs      # CLI binary (kmd) -- stays synchronous
  tui/src/main.rs      # TUI binary -- migrate to async
crates/
  core/src/lib.rs      # Domain logic -- add tests here
```

### Pattern 1: Async TUI Event Loop with `tokio::select!`
**What:** Replace blocking `event::read()` with async event stream polling
**When to use:** When TUI needs to handle events AND timers/background tasks concurrently

```rust
// Source: https://ratatui.rs/tutorials/counter-async-app/async-event-stream/
// Adapted for this project's simpler needs

use futures::StreamExt;
use crossterm::event::EventStream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal).await;
    ratatui::restore();
    result
}

async fn run(terminal: &mut ratatui::DefaultTerminal) -> anyhow::Result<()> {
    let mut reader = EventStream::new();
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(250));

    let mut app = App::new(/* ... */);

    loop {
        terminal.draw(|frame| { /* render */ })?;

        let crossterm_event = reader.next().fuse();
        let tick = tick_interval.tick();

        tokio::select! {
            maybe_event = crossterm_event => {
                if let Some(Ok(event)) = maybe_event {
                    if let crossterm::event::Event::Key(key) = event {
                        match key.code {
                            KeyCode::Char('q') => break,
                            // ... handle keys
                            _ => {}
                        }
                    }
                }
            }
            _ = tick => {
                // Future: refresh UI, poll background tasks
            }
        }
    }
    Ok(())
}
```

### Pattern 2: Terminal Init with Built-in Panic Hook
**What:** Use `ratatui::init()` which handles raw mode, alt screen, AND panic hook
**When to use:** Always for ratatui 0.29 crossterm-backed apps

```rust
// Source: ratatui 0.29 source code (terminal/init.rs)
// ratatui::init() internally calls:
//   1. set_panic_hook() -- captures original hook, wraps with restore()
//   2. enable_raw_mode()
//   3. execute!(stdout(), EnterAlternateScreen)
//   4. Terminal::new(CrosstermBackend::new(stdout()))

// Usage:
let mut terminal = ratatui::init();  // panic-safe from this point
// ... run app ...
ratatui::restore();  // clean shutdown
```

### Pattern 3: Unit Tests with tempfile Isolation
**What:** Use `tempfile::TempDir` to isolate filesystem tests
**When to use:** Testing AppState load/save/add_repo

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Key insight: AppState currently hardcodes ".kommand0-dev/state.json"
    // Tests need to either:
    // (a) Make state_dir configurable (preferred), OR
    // (b) Use std::env::set_current_dir (fragile, not parallel-safe)
    //
    // Recommendation: Add a constructor that accepts a base path
    // e.g., AppState::with_dir(path: PathBuf) so tests can point at a TempDir

    #[test]
    fn load_returns_default_when_no_file() {
        let tmp = TempDir::new().unwrap();
        let state = AppState::load_from(tmp.path()).unwrap();
        assert!(state.repos.is_empty());
    }

    #[test]
    fn save_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        // add a repo entry manually
        state.repos.push(RepoEntry { id: "abc".into(), name: "test".into(), path: "/tmp/test".into() });
        state.save_to(tmp.path()).unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.repos.len(), 1);
        assert_eq!(loaded.repos[0].name, "test");
    }
}
```

### Anti-Patterns to Avoid
- **Don't use `std::env::set_current_dir` in tests:** Not thread-safe; breaks parallel test execution. Make state path configurable instead.
- **Don't add `#[tokio::test]` for core logic tests:** Core tests are synchronous; only TUI tests (if any) need tokio.
- **Don't wrap EventStream in channels yet:** The ratatui async template uses mpsc channels between event handler and main loop. This is over-engineering for our current single-stream needs. Direct `tokio::select!` is simpler.
- **Don't import `crossterm` event types directly in TUI:** Use `ratatui::crossterm::event::*` re-exports to avoid version conflicts.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Terminal panic safety | Custom panic hook with raw mode/alt screen cleanup | `ratatui::init()` + `ratatui::restore()` | Built-in, tested, handles edge cases (double-panic safety) |
| Async event stream | Custom polling loop with `poll_timeout` | `crossterm::event::EventStream` + `futures::StreamExt` | Proper async integration, works with `tokio::select!` |
| Test filesystem isolation | Manual temp dir creation/cleanup | `tempfile::TempDir` | RAII cleanup, OS-specific handling, parallel-safe |

**Key insight:** ratatui 0.29 has matured to the point where `init()`/`restore()` handle the entire terminal lifecycle. The days of manually managing raw mode + alt screen + panic hooks are over for standard crossterm apps.

## Common Pitfalls

### Pitfall 1: Hardcoded State Path Blocks Testing
**What goes wrong:** `AppState` uses hardcoded `".kommand0-dev/state.json"` relative path. Tests either pollute the working directory or conflict with each other.
**Why it happens:** The initial implementation didn't need tests.
**How to avoid:** Add `load_from(base: &Path)` and `save_to(base: &Path)` methods. Keep the original `load()`/`save()` as convenience wrappers that call the new methods with the default path.
**Warning signs:** Tests that require `cd` to a temp directory, tests that fail when run in parallel.

### Pitfall 2: crossterm EventStream Requires `fuse()`
**What goes wrong:** `reader.next()` inside `tokio::select!` without `.fuse()` causes a compiler error or UB because `Stream::next()` is not `FusedFuture`.
**Why it happens:** `tokio::select!` requires all branches to be cancel-safe / fused.
**How to avoid:** Always use `reader.next().fuse()` (requires `use futures::FutureExt`).
**Warning signs:** Compile error about `Unpin` or `FusedFuture` bounds.

### Pitfall 3: Duplicate crossterm Versions
**What goes wrong:** Adding `crossterm` with `event-stream` feature as a direct dependency while ratatui also pulls crossterm can cause two versions.
**Why it happens:** Version mismatch between workspace dep and ratatui's internal dep.
**How to avoid:** The workspace already pins `crossterm = "0.28"` and ratatui 0.29 uses crossterm 0.28.1. Just add the `event-stream` feature to the workspace dep. Use `ratatui::crossterm` re-exports for event types.
**Warning signs:** `cargo tree -d` shows duplicate crossterm entries.

### Pitfall 4: Missing `use futures::StreamExt` for `.next()`
**What goes wrong:** `EventStream` implements `Stream` but `.next()` method comes from `StreamExt` trait extension.
**Why it happens:** Unlike iterators, streams need explicit trait import.
**How to avoid:** Add `use futures::StreamExt;` (or `use futures::{StreamExt, FutureExt};`).
**Warning signs:** Compiler error "no method named `next` found for struct `EventStream`".

### Pitfall 5: `ratatui::init()` Must Be Called After Other Panic Hooks
**What goes wrong:** If another panic hook is installed after `ratatui::init()`, it replaces the terminal-restoring hook.
**Why it happens:** `set_hook` replaces the previous hook; `ratatui::init()` chains hooks but only if called last.
**How to avoid:** Call `ratatui::init()` as the last hook setup. In our case, no other hooks exist, so this is not a concern.
**Warning signs:** Terminal left in raw mode after panic.

## Code Examples

### Complete Async TUI Migration Pattern
```rust
// Source: Synthesized from ratatui docs + crossterm event-stream-tokio example
// apps/tui/src/main.rs

use std::time::Duration;

use anyhow::Result;
use futures::{FutureExt, StreamExt};
use crossterm::event::{EventStream, Event, KeyCode, KeyEventKind};
use kommand0_core::{AppState, run_git_status};
use ratatui::DefaultTerminal;

// ... App struct stays the same ...

#[tokio::main]
async fn main() -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal).await;
    ratatui::restore();
    result
}

async fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let state = AppState::load()?;
    let mut app = App::new(state.repos);
    let mut reader = EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(250));

    loop {
        terminal.draw(|frame| ui(frame, &mut app))?;

        let event = reader.next().fuse();
        let tick = tick_interval.tick();

        tokio::select! {
            maybe_event = event => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    // crossterm 0.28 sends both Press and Release on some platforms
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                            KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                            KeyCode::Enter => app.run_status(),
                            _ => {}
                        }
                    }
                }
            }
            _ = tick => {
                // Tick: future UI refresh, polling, etc.
            }
        }
    }

    Ok(())
}
```

### Making AppState Testable
```rust
// Source: Project-specific pattern for testability
// crates/core/src/lib.rs -- additions

impl AppState {
    /// Load state from a specific base directory
    pub fn load_from(base: &std::path::Path) -> anyhow::Result<Self> {
        let path = base.join(Self::STATE_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let state: Self = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(state)
    }

    /// Save state to a specific base directory
    pub fn save_to(&self, base: &std::path::Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(base)
            .with_context(|| format!("failed to create {}", base.display()))?;
        let path = base.join(Self::STATE_FILE);
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, data)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    // Original load()/save() delegate to these with default path
}
```

### Edge Case Testing for run_git_status
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn git_status_on_nonexistent_path() {
        let result = run_git_status("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
    }

    #[test]
    fn git_status_on_non_git_directory() {
        let tmp = TempDir::new().unwrap();
        let result = run_git_status(tmp.path().to_str().unwrap());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("git status failed") || err.contains("not a git repository"));
    }

    #[test]
    fn git_status_on_file_not_directory() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("afile.txt");
        std::fs::write(&file_path, "hello").unwrap();
        let result = run_git_status(file_path.to_str().unwrap());
        assert!(result.is_err());
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Manual raw mode + alt screen + panic hook | `ratatui::init()` / `ratatui::restore()` | ratatui 0.28.1 (late 2024) | Eliminates 15+ lines of boilerplate, handles edge cases |
| Blocking `event::read()` | `EventStream` + `tokio::select!` | crossterm 0.27+ | Enables concurrent event handling without threads |
| `crossterm::event` direct import | `ratatui::crossterm::event` re-export | ratatui 0.28+ | Avoids version conflicts between direct and transitive deps |

**Deprecated/outdated:**
- Manual `enable_raw_mode()` / `disable_raw_mode()` / `EnterAlternateScreen` / `LeaveAlternateScreen` management: still works but `ratatui::init()`/`restore()` is the recommended approach for ratatui 0.29
- `event::poll()` + `event::read()` pattern: replaced by `EventStream` for async apps

## Open Questions

1. **Should we use `ratatui::init()` or manual setup?**
   - What we know: `ratatui::init()` does exactly what CONTEXT.md describes (panic hook + raw mode + alt screen), verified in ratatui 0.29 source
   - What's unclear: CONTEXT.md explicitly says "use `std::panic::set_hook()` in TUI `main()`" which suggests manual approach was intended
   - Recommendation: Use `ratatui::init()` -- it satisfies the requirement with less code and is the idiomatic pattern. The CONTEXT.md decision was likely made before discovering this built-in capability.

2. **Should `tracing-subscriber` be initialized in this phase?**
   - What we know: `tracing` is already a workspace dependency but not used yet
   - What's unclear: Whether logging helps debugging during this phase
   - Recommendation: Defer to Phase 3. No log output is needed for stabilization work.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[test]` + cargo test |
| Config file | None needed (Cargo.toml handles it) |
| Quick run command | `cargo test -p kommand0-core` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements to Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| STAB-01 | Naming consistency | manual review | N/A (code review) | N/A |
| STAB-02 | Package boundaries | manual review | N/A (structural) | N/A |
| STAB-03 | Core unit tests | unit | `cargo test -p kommand0-core -x` | No -- Wave 0 |
| STAB-04 | README accuracy | manual | N/A (doc review) | N/A |
| STAB-05 | Git status edge cases | unit | `cargo test -p kommand0-core --test '*' -- git_status` | No -- Wave 0 |
| STAB-06 | Panic hook restores terminal | manual | N/A (trigger panic, verify terminal restored) | N/A |
| STAB-07 | Async event loop | manual | N/A (run TUI, verify keyboard input works) | N/A |

### Sampling Rate
- **Per task commit:** `cargo test -p kommand0-core`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** `cargo test --workspace` all green + manual TUI verification

### Wave 0 Gaps
- [ ] `crates/core/src/lib.rs` -- add `#[cfg(test)] mod tests { ... }` with tests for STAB-03 and STAB-05
- [ ] Add `tempfile = "3"` to `[dev-dependencies]` in `crates/core/Cargo.toml`
- [ ] Add `load_from()` / `save_to()` methods to `AppState` for test isolation
- [ ] Add `futures = "0.3"` to workspace deps and TUI Cargo.toml

## Sources

### Primary (HIGH confidence)
- ratatui 0.29.0 source code (`~/.cargo/registry/src/.../ratatui-0.29.0/src/terminal/init.rs`) -- verified init/restore/panic-hook behavior
- crossterm 0.28.1 Cargo.toml -- verified `event-stream` feature availability
- [Ratatui Panic Hooks Recipe](https://ratatui.rs/recipes/apps/panic-hooks/) -- panic hook patterns
- [Ratatui Async Event Stream Tutorial](https://ratatui.rs/tutorials/counter-async-app/async-event-stream/) -- tokio::select! pattern
- [crossterm event-stream-tokio example](https://github.com/crossterm-rs/crossterm/blob/master/examples/event-stream-tokio.rs) -- EventStream usage

### Secondary (MEDIUM confidence)
- [Ratatui FAQ](https://ratatui.rs/faq/) -- general best practices
- [EventStream docs.rs](https://docs.rs/crossterm/latest/crossterm/event/struct.EventStream.html) -- API reference

### Tertiary (LOW confidence)
- None -- all findings verified against primary sources

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- all versions verified in existing Cargo.lock and crate sources
- Architecture: HIGH -- patterns verified against official ratatui tutorials and crossterm examples
- Pitfalls: HIGH -- pitfalls derived from actual API constraints (verified fuse() requirement, version conflicts)
- Testing: HIGH -- tempfile is the standard approach, AppState testability gap is clear

**Research date:** 2026-03-07
**Valid until:** 2026-06-07 (stable ecosystem, ratatui/crossterm on established versions)
