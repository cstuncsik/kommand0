# Testing Patterns

**Analysis Date:** 2026-03-22

## Test Framework

**Runner:**
- Built-in Rust test framework (no external test runner like Jest or Vitest)
- Run via Cargo: `cargo test`
- Configured in workspace `Cargo.toml` with `edition = "2024"`

**Assertion Library:**
- Standard Rust assertions: `assert!()`, `assert_eq!()`, `assert_ne!()`
- No external assertion library dependency

**Run Commands:**
```bash
cargo test                        # Run all tests
cargo test --lib               # Run library tests only
cargo test --doc               # Run doc tests only
cargo test -- --nocapture      # Show println! output during tests
cargo test -- --test-threads=1 # Run tests sequentially
```

**Available in workspace:**
- No coverage tool configured
- No watch mode configured; use `cargo watch -x test` externally if needed

## Test File Organization

**Location:**
- Tests inline in the same module file, not separate
- All tests are co-located with implementation using `#[cfg(test)]` module blocks
- No separate `tests/` directory structure

**Naming:**
- Test module: `#[cfg(test)] mod tests { ... }`
- Test functions: `#[test] fn test_name() { ... }`
- Descriptive names describing the scenario: `test_name_specific_behavior()`

**Structure - Example from `crates/core/src/lib.rs`:**
```
lib.rs (primary code)
  ↓
  #[cfg(test)]
  mod tests {
      use super::*;
      use tempfile::TempDir;

      #[test]
      fn test_name() { ... }

      #[test]
      fn test_name_error_case() { ... }
  }
```

## Test Structure

**Suite Organization:**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Test data factories
    fn make_state_with_repo(tmp: &TempDir) -> (AppState, String) {
        let mut state = AppState::default();
        state.repos.push(RepoEntry { /* ... */ });
        state.save_to(tmp.path()).unwrap();
        (state, "repo1".to_string())
    }

    // Individual tests organized by feature
    // --- create_workspace tests ---
    #[test]
    fn create_workspace_auto_name_from_repo() { /* ... */ }

    #[test]
    fn create_workspace_explicit_name() { /* ... */ }

    // --- list_workspaces tests ---
    #[test]
    fn list_workspaces_active_only() { /* ... */ }
}
```

**Patterns:**

1. **Setup Pattern:** Use `tempfile::TempDir` for isolated state directories
   ```rust
   let tmp = TempDir::new().unwrap();
   let mut state = AppState::default();
   state.add_repo_with_base("/path", tmp.path())?
   ```

2. **Factories:** Helper functions create consistent test fixtures
   ```rust
   fn make_state_with_repo(tmp: &TempDir) -> (AppState, String) { ... }
   fn make_state_with_workspace(tmp: &TempDir) -> (AppState, String) { ... }
   ```

3. **Teardown Pattern:** Automatic via TempDir drop; no explicit cleanup needed

4. **Assertion Pattern:** Direct assertions on returned values or state
   ```rust
   assert_eq!(ws.name, "my-feature");
   assert!(ws.active);
   assert_eq!(ws.repo_id, "repo1");
   ```

## Mocking

**Framework:**
- No external mocking library (no mockall, mock, etc.)
- Use enum variants to simulate different outcomes: `WorktreeResult::Created` vs `WorktreeResult::Fallback`

**Patterns:**

1. **Enum-based simulation** - Operations that might fail return enum with success/failure variants
   ```rust
   pub enum WorktreeResult {
       Created { worktree_path: String, branch_name: String },
       Fallback { reason: String },
   }
   ```

2. **Test double via optional parameters:**
   - Functions accept `&Path base` parameter for test state directory
   - Tests pass `tmp.path()` instead of `Self::state_dir()`
   - Example: `create_workspace_with_base(..., tmp.path())` vs `create_workspace(...)`

3. **Direct subprocess calls:**
   - No mocking of git commands in unit tests
   - Git operations are tested via actual git commands on test repos
   - Example in `worktree.rs`: Creates temporary git repos in tests

4. **What NOT to Mock:**
   - File system operations (use tempfile instead)
   - Git operations (create actual test repos with `git init`)
   - Core domain logic (test implementation directly)

## Fixtures and Factories

**Test Data:**
```rust
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

fn make_state_with_workspace(tmp: &TempDir) -> (AppState, String) {
    let mut state = AppState::default();
    state.repos.push(RepoEntry { /* ... */ });
    state.workspaces.push(Workspace {
        id: "ws1".to_string(),
        name: "myapp".to_string(),
        repo_id: "repo1".to_string(),
        working_dir: "/tmp/myapp".to_string(),
        active: true,
        created_at: 1000,
        worktree_path: None,
    });
    state.save_to(tmp.path()).unwrap();
    (state, "ws1".to_string())
}
```

**Location:**
- Defined at the top of test module, before test functions
- Reused across multiple tests in the same module
- Each test calls the factory with its own `TempDir` instance

## Coverage

**Requirements:**
- Not enforced; no coverage configuration detected
- No coverage reports generated by default

**View Coverage:**
- Not applicable; requires external tool setup (tarpaulin or llvm-cov)

## Test Types

**Unit Tests:**
- Scope: Individual functions and methods on core domain types
- Approach: Direct function calls with assertions on return values
- Location: `crates/core/src/lib.rs`, `crates/core/src/workspace.rs`, `crates/core/src/session.rs`, `crates/core/src/worktree.rs`
- Example: `AppState::add_repo()`, `AppState::create_workspace()`, git operations

**Integration Tests:**
- Scope: Multi-step workflows combining several operations
- Approach: Create state with repos, add workspaces, create sessions, verify persistence
- Location: Same modules as unit tests (co-located)
- Example: `roundtrip_workspaces_persist()` - create workspace, save, load, verify

**Git Integration Tests:**
- Scope: Git worktree creation and removal
- Approach: Initialize actual git repos, call create/remove worktree functions
- Helper: `init_git_repo()` sets up test git repo with initial commit
- Example in `crates/core/src/worktree.rs`: `create_and_remove_worktree()`, `unique_branch_handles_collision()`

**E2E Tests:**
- Not present; CLI and TUI tested via manual interaction or integration tests

## Common Patterns

**Async Testing:**
- Not applicable; core library is not async
- TUI and CLI may use tokio runtime, but no test patterns detected in current codebase

**Error Testing:**
```rust
#[test]
fn add_repo_rejects_nonexistent_path() {
    let tmp = TempDir::new().unwrap();
    let mut state = AppState::default();
    let result = state.add_repo_with_base("/nonexistent/path/xyz", tmp.path());
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("does not exist") || err.contains("not a directory"));
}

#[test]
fn resolve_repo_error_includes_checked() {
    let state = AppState::default();
    let err = state.resolve_repo("missing").unwrap_err();
    assert!(err.to_string().contains("Checked: name, path, id"));
}
```

**Serialization Testing:**
```rust
#[test]
fn session_serializes_and_deserializes() {
    let session = Session {
        id: "abc-123".to_string(),
        workspace_id: "ws1".to_string(),
        claude_session_id: Some("claude-xyz".to_string()),
        pid: Some(1234),
        status: SessionStatus::Running,
        created_at: 1000,
        ended_at: None,
        log_file: ".kommand0-dev/sessions/abc-123.log".to_string(),
    };
    let json = serde_json::to_string(&session).unwrap();
    let deserialized: Session = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.id, "abc-123");
    assert_eq!(deserialized.status, SessionStatus::Running);
}
```

**Enum Variant Testing:**
```rust
#[test]
fn session_status_variants_serialize_deserialize() {
    for status in [
        SessionStatus::Running,
        SessionStatus::Stopped,
        SessionStatus::Failed,
        SessionStatus::Exited,
    ] {
        let json = serde_json::to_string(&status).unwrap();
        let back: SessionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }
}
```

**Backward Compatibility Testing:**
```rust
#[test]
fn backward_compat_no_workspaces_key() {
    let tmp = TempDir::new().unwrap();
    let json = r#"{"repos": [{"id": "r1", "name": "foo", "path": "/tmp/foo"}]}"#;
    std::fs::write(tmp.path().join("state.json"), json).unwrap();

    let state = AppState::load_from(tmp.path()).unwrap();
    assert_eq!(state.repos.len(), 1);
    assert!(state.workspaces.is_empty()); // Defaults to empty vec
}
```

**Cascading Operation Testing:**
```rust
#[test]
fn delete_workspace_removes_from_vec() {
    let tmp = TempDir::new().unwrap();
    let (mut state, _) = make_state_with_repo(&tmp);
    state.create_workspace_with_base(Some("ws"), "myapp", tmp.path()).unwrap();
    let removed = state.delete_workspace_with_base("ws", tmp.path()).unwrap();
    assert_eq!(removed.name, "ws");
    assert!(state.workspaces.is_empty());
}
```

**Persistence Testing:**
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

## Test Coverage

**Current coverage:**
- `crates/core/src/lib.rs` - 25 tests covering AppState and major workflows
- `crates/core/src/workspace.rs` - 15 tests covering workspace CRUD and listing
- `crates/core/src/session.rs` - 8 tests covering session lifecycle and serialization
- `crates/core/src/worktree.rs` - 5 tests covering git worktree creation/removal
- Total: ~53 tests, all in core library

**Coverage gaps:**
- `apps/cli/src/main.rs` - No automated tests (CLI testing requires manual interaction or end-to-end framework)
- `apps/tui/src/` - No automated tests (UI framework tests would require terminal mocking)
- No integration tests spanning CLI and core library

**Untested areas:**
- CLI command execution and output formatting
- TUI rendering and event handling
- Session manager lifecycle and event dispatch
- Scrollback buffer management
- Modal and help overlay interaction

---

*Testing analysis: 2026-03-22*
