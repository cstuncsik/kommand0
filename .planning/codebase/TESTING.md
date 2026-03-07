# Testing Patterns

**Analysis Date:** 2026-03-07

## Test Framework

**Runner:**
- Rust built-in test framework (`cargo test`)
- No additional test runner or harness configured
- Config: No custom test configuration files

**Assertion Library:**
- Standard `assert!`, `assert_eq!`, `assert_ne!` macros (built-in)
- No third-party assertion crate detected

**Run Commands:**
```bash
cargo test                    # Run all tests in workspace
cargo test -p kommand0-core   # Run tests for core crate only
cargo test -p kommand0-cli    # Run tests for CLI crate only
cargo test -p kommand0-tui    # Run tests for TUI crate only
cargo test -- --nocapture     # Run tests with stdout visible
```

## Test File Organization

**Location:**
- No tests exist yet. The project brief (`gds-brief.md`) calls for adding unit tests in Milestone 1.

**Recommended pattern (co-located inline tests):**
- Place unit tests in the same file as the code under test, inside a `#[cfg(test)]` module at the bottom
- Place integration tests in a `tests/` directory at the crate root

**Naming:**
- Test modules: `mod tests` (standard Rust convention)
- Test functions: `test_` prefix with snake_case describing the scenario

**Recommended structure:**
```
crates/core/
  src/
    lib.rs          # Unit tests at bottom in #[cfg(test)] mod tests
  tests/
    integration.rs  # Integration tests (optional)
apps/cli/
  src/
    main.rs         # Minimal tests; most logic lives in core
  tests/
    cli_tests.rs    # CLI integration tests using assert_cmd (optional)
```

## Test Structure

**Recommended suite organization:**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;  // Add tempfile to dev-dependencies

    #[test]
    fn test_add_repo_creates_entry() {
        // Arrange
        let dir = TempDir::new().unwrap();
        let mut state = AppState::default();

        // Act
        let entry = state.add_repo(dir.path().to_str().unwrap()).unwrap();

        // Assert
        assert_eq!(state.repos.len(), 1);
        assert_eq!(entry.name, dir.path().file_name().unwrap().to_str().unwrap());
    }

    #[test]
    fn test_add_repo_rejects_nonexistent_path() {
        let mut state = AppState::default();
        let result = state.add_repo("/nonexistent/path");
        assert!(result.is_err());
    }

    #[test]
    fn test_add_repo_rejects_duplicate() {
        let dir = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.add_repo(dir.path().to_str().unwrap()).unwrap();
        let result = state.add_repo(dir.path().to_str().unwrap());
        assert!(result.is_err());
    }
}
```

**Patterns:**
- Use Arrange/Act/Assert structure
- Each test function tests one behavior
- Use `#[should_panic]` sparingly; prefer checking `Result::is_err()`

## Mocking

**Framework:** No mocking framework in use. For this codebase size, manual test doubles are sufficient.

**Patterns for future use:**
```rust
// For testing functions that call external commands (like run_git_status),
// extract the command execution behind a trait:
pub trait GitRunner {
    fn status(&self, repo_path: &str) -> anyhow::Result<String>;
}

pub struct RealGitRunner;
impl GitRunner for RealGitRunner {
    fn status(&self, repo_path: &str) -> anyhow::Result<String> {
        run_git_status(repo_path)
    }
}

#[cfg(test)]
pub struct MockGitRunner {
    pub result: anyhow::Result<String>,
}

#[cfg(test)]
impl GitRunner for MockGitRunner {
    fn status(&self, _repo_path: &str) -> anyhow::Result<String> {
        // Return predefined result
        Ok("mock output".to_string())
    }
}
```

**What to mock:**
- External process execution (`Command::new("git")`)
- File system operations when testing logic (use `tempfile` crate for real temp dirs)

**What NOT to mock:**
- Simple data structures and their methods
- Serialization/deserialization (test with real JSON)
- `AppState` load/save (test with real temp files)

## Fixtures and Factories

**Test Data:**
```rust
// Helper to create test AppState with repos
#[cfg(test)]
fn test_state_with_repos(paths: &[&str]) -> AppState {
    AppState {
        repos: paths.iter().enumerate().map(|(i, p)| RepoEntry {
            id: format!("test-{}", i),
            name: format!("repo-{}", i),
            path: p.to_string(),
        }).collect(),
    }
}
```

**Location:**
- Small helpers go inside the `#[cfg(test)] mod tests` block
- Shared test utilities (if needed later) go in a `crates/core/src/test_helpers.rs` module gated with `#[cfg(test)]`

## Coverage

**Requirements:** None enforced. No coverage tooling configured.

**Recommended setup:**
```bash
cargo install cargo-tarpaulin           # Install coverage tool
cargo tarpaulin --workspace --out html  # Generate coverage report
```

## Test Types

**Unit Tests:**
- Target: `crates/core/src/lib.rs` -- all domain logic and state management
- Scope: Individual functions like `AppState::add_repo()`, `AppState::load()`, `AppState::save()`, `run_git_status()`, `generate_id()`
- Use `#[cfg(test)] mod tests` inline in the source file

**Integration Tests:**
- Target: CLI binary behavior
- Scope: End-to-end command execution via `assert_cmd` crate (not yet added)
- Place in `apps/cli/tests/` directory
- Example: verify `kmd repo add <path>` creates state file, `kmd repo list` shows output

**E2E Tests:**
- Not applicable for TUI at this stage
- TUI testing is manual (verify rendering and keyboard interaction)

## Common Patterns

**Async Testing:**
```rust
// tokio is a workspace dependency; use #[tokio::test] for async tests
#[tokio::test]
async fn test_async_operation() {
    let result = some_async_fn().await;
    assert!(result.is_ok());
}
```

**Error Testing:**
```rust
#[test]
fn test_operation_returns_error_on_invalid_input() {
    let mut state = AppState::default();
    let result = state.add_repo("/path/that/does/not/exist");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("does not exist"));
}
```

**File System Testing:**
```rust
// Add to [dev-dependencies] in crates/core/Cargo.toml:
// tempfile = "3"

#[test]
fn test_state_save_and_load() {
    let dir = TempDir::new().unwrap();
    // Override state dir or use a helper that accepts a path
    // Test round-trip: save then load, verify equality
}
```

## Priority Test Targets

Based on `gds-brief.md` Milestone 1 deliverables, these are the first tests to write:

1. **`AppState::load()`** in `crates/core/src/lib.rs` -- returns default when no file exists, parses valid JSON, errors on malformed JSON
2. **`AppState::save()`** in `crates/core/src/lib.rs` -- creates directory, writes valid JSON, round-trips with load
3. **`AppState::add_repo()`** in `crates/core/src/lib.rs` -- validates path exists, rejects duplicates, canonicalizes path, generates unique ID
4. **`run_git_status()`** in `crates/core/src/lib.rs` -- succeeds on valid git repo, fails on non-git directory, fails on nonexistent path
5. **`generate_id()`** in `crates/core/src/lib.rs` -- returns non-empty hex string, produces unique values on successive calls

## Dev Dependencies to Add

For testing support, add these to `crates/core/Cargo.toml`:
```toml
[dev-dependencies]
tempfile = "3"
```

For CLI integration tests, add to `apps/cli/Cargo.toml`:
```toml
[dev-dependencies]
assert_cmd = "2"
predicates = "3"
tempfile = "3"
```

---

*Testing analysis: 2026-03-07*
