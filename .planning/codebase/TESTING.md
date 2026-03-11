# Testing Patterns

**Analysis Date:** 2026-03-11

## Test Framework

**Runner:**
- Rust built-in test framework (`#[test]`, `#[cfg(test)]`)
- No external test runner (no `nextest`, no custom harness)

**Assertion Library:**
- Standard `assert!`, `assert_eq!`, `assert!(result.is_err())`
- No `assert_matches!` macro used; pattern matching done manually

**Run Commands:**
```bash
cargo test                    # Run all tests (57 total: 41 core + 16 TUI)
cargo test -p kommand0-core   # Run core crate tests only
cargo test -p kommand0-tui    # Run TUI tests only (scrollback only)
cargo test -- --nocapture     # Show println output during tests
```

## Test File Organization

**Location:**
- Co-located: tests live in `#[cfg(test)] mod tests { ... }` blocks at the bottom of each source file
- No separate `tests/` directory for integration tests
- No test utilities crate

**Which files have tests:**
- `crates/core/src/lib.rs` - 10 tests (AppState: load, save, add_repo, delete_repo, git_status)
- `crates/core/src/session.rs` - 8 tests (session CRUD, serialization, backward compat)
- `crates/core/src/workspace.rs` - 14 tests (workspace CRUD, resolve_repo, list, archive, format_timestamp)
- `crates/core/src/worktree.rs` - 4 tests (create, remove, fallback, branch collision)
- `apps/tui/src/scrollback.rs` - 16 tests (buffer operations, scroll, capacity)

**Files WITHOUT tests:**
- `crates/core/src/id.rs` - no tests (trivial ID generation)
- `crates/core/src/repo.rs` - no tests (tested via lib.rs tests)
- `apps/tui/src/main.rs` - no tests (TUI event loop, hard to unit test)
- `apps/tui/src/render.rs` - no tests (rendering logic)
- `apps/tui/src/session_manager.rs` - no tests (async process management)
- `apps/tui/src/composer.rs` - no tests
- `apps/tui/src/modal.rs` - no tests
- `apps/tui/src/mouse.rs` - no tests
- `apps/tui/src/buttons.rs` - no tests
- `apps/tui/src/help.rs` - no tests
- `apps/cli/src/main.rs` - no tests

## Test Structure

**Suite Organization:**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Optional: shared setup helper
    fn make_state_with_repo(tmp: &TempDir) -> (AppState, String) {
        let mut state = AppState::default();
        let repo = RepoEntry {
            id: "repo1".to_string(),
            name: "myapp".to_string(),
            path: "/tmp/myapp".to_string(),
        };
        state.repos.push(repo);
        state.save_to(tmp.path()).unwrap();
        (state, "repo1".to_string())
    }

    #[test]
    fn descriptive_test_name_with_underscores() {
        let tmp = TempDir::new().unwrap();
        // arrange
        let (mut state, _) = make_state_with_repo(&tmp);
        // act
        let result = state.create_workspace_with_base(None, "myapp", tmp.path());
        // assert
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name, "myapp");
    }
}
```

**Naming convention for tests:**
- `fn descriptive_name_describing_behavior()` - e.g., `create_session_errors_if_running_session_exists`
- Name describes the scenario AND expected outcome
- Grouped by section comments: `// --- create_workspace tests ---`, `// --- persistence tests ---`

**Setup patterns:**
- `TempDir::new().unwrap()` for filesystem isolation in every test
- Helper functions `make_state_with_repo()` and `make_state_with_workspace()` for common setups
- For git tests: `init_git_repo()` helper that runs `git init` + `git commit --allow-empty`

**Teardown:**
- Automatic via `TempDir` drop (RAII cleanup)
- No explicit teardown needed

## Mocking

**Framework:** None. No mocking library is used.

**Approach:**
- Dependency injection via `_with_base()` method pattern instead of mocks
- All state-mutating `AppState` methods accept a `base: &Path` parameter for testability
- Tests use `TempDir` to provide isolated filesystem state
- External process calls (git) tested against real git repos created in temp directories

**What to mock (if adding mocking):**
- Process spawning for `SessionManager` tests
- Git operations for worktree tests that currently require real git repos

**What NOT to mock:**
- Filesystem operations -- use `TempDir` instead
- `AppState` serialization -- test the real JSON roundtrip
- Core business logic -- test through public API

## Fixtures and Factories

**Test Data:**
```rust
// Factory-style helper for creating test state
fn make_state_with_repo(tmp: &TempDir) -> (AppState, String) {
    let mut state = AppState::default();
    state.repos.push(RepoEntry {
        id: "repo1".to_string(),
        name: "myapp".to_string(),
        path: "/tmp/myapp".to_string(),
    });
    state.save_to(tmp.path()).unwrap();
    (state, "repo1".to_string())
}

// Git repo fixture
fn init_git_repo(dir: &Path) {
    Command::new("git").args(["init"]).current_dir(dir).output().unwrap();
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();
}
```

**Fixture locations:**
- Inline in test modules (no separate fixtures directory)
- Helper functions defined at the top of each `mod tests` block

**Inline JSON for backward-compat tests:**
```rust
let json = r#"{"repos": [{"id": "r1", "name": "foo", "path": "/tmp/foo"}]}"#;
std::fs::write(tmp.path().join("state.json"), json).unwrap();
```

## Coverage

**Requirements:** None enforced. No coverage thresholds configured.

**Current coverage areas:**
- Core domain logic: well tested (AppState CRUD, serialization, backward compat)
- Scrollback buffer: thoroughly tested (16 tests covering all operations)
- TUI rendering/interaction: not tested
- CLI command dispatch: not tested
- Session manager (process spawning): not tested

**View Coverage:**
```bash
# No coverage tool configured. To add:
cargo install cargo-tarpaulin
cargo tarpaulin -p kommand0-core --out html
```

## Test Types

**Unit Tests:**
- All 57 tests are unit tests
- Co-located in source files with `#[cfg(test)]`
- Test individual functions and methods in isolation
- Use `TempDir` for filesystem isolation

**Integration Tests:**
- Not present. No `tests/` directory exists.
- CLI integration tests (running `kmd` as subprocess) would be valuable but don't exist

**E2E Tests:**
- Not present.
- Would require mocking the `claude` CLI process

## Common Patterns

**Testing error cases:**
```rust
#[test]
fn operation_rejects_invalid_input() {
    let tmp = TempDir::new().unwrap();
    let mut state = AppState::default();
    let result = state.add_repo_with_base("/nonexistent/path/xyz", tmp.path());
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("does not exist") || err.contains("not a directory"));
}
```

**Testing serialization roundtrips:**
```rust
#[test]
fn save_to_load_from_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let mut state = AppState::default();
    state.repos.push(RepoEntry { /* ... */ });
    state.save_to(tmp.path()).unwrap();

    let loaded = AppState::load_from(tmp.path()).unwrap();
    assert_eq!(loaded.repos.len(), 1);
    assert_eq!(loaded.repos[0].id, "abc123");
}
```

**Testing backward compatibility:**
```rust
#[test]
fn backward_compat_no_sessions_key() {
    let tmp = TempDir::new().unwrap();
    let json = r#"{"repos": [{"id": "r1", "name": "foo", "path": "/tmp/foo"}]}"#;
    std::fs::write(tmp.path().join("state.json"), json).unwrap();

    let state = AppState::load_from(tmp.path()).unwrap();
    assert_eq!(state.repos.len(), 1);
    assert!(state.sessions.is_empty());
}
```

**Testing with real git repos:**
```rust
#[test]
fn create_and_remove_worktree() {
    let repo = TempDir::new().unwrap();
    let base = TempDir::new().unwrap();
    init_git_repo(repo.path());

    let result = create_worktree(repo.path().to_str().unwrap(), "my-feature", base.path());
    match result {
        WorktreeResult::Created { worktree_path, branch_name } => {
            assert!(Path::new(&worktree_path).exists());
            assert!(branch_name.starts_with("kommand0/"));
        }
        WorktreeResult::Fallback { reason } => {
            panic!("expected Created, got Fallback: {}", reason);
        }
    }
}
```

**Pattern for enum variant matching in tests:**
```rust
// Use match with panic for unexpected variants
match result {
    WorktreeResult::Fallback { reason } => {
        assert!(reason.contains("not a git repository"));
    }
    WorktreeResult::Created { .. } => panic!("expected fallback"),
}

// Or use assert!(matches!(...)) for simpler checks
assert!(matches!(result1, WorktreeResult::Created { .. }));
```

## Adding New Tests

**For a new core domain feature:**
1. Add tests to the bottom of the relevant source file in `crates/core/src/`
2. Use `TempDir` for any filesystem state
3. Create a `_with_base()` variant of state-mutating methods for testability
4. Test both success and error paths
5. Add backward-compat test if modifying serialized structs

**For a new TUI component:**
1. Add `#[cfg(test)] mod tests` in the component file
2. Test pure logic functions (no rendering)
3. Extract testable logic from render functions into separate helpers

**Dev dependency for tests:**
- `tempfile` (workspace dependency, already in `[dev-dependencies]` for core crate)

---

*Testing analysis: 2026-03-11*
