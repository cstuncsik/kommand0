# Coding Conventions

**Analysis Date:** 2026-03-22

## Naming Patterns

**Files:**
- Module files use `snake_case` (e.g., `session_manager.rs`, `worktree.rs`)
- Main entry files: `main.rs`, `lib.rs`
- Tests inline in the same file (no separate `_test.rs` or `tests/` directory)

**Functions:**
- Public functions use `snake_case` (e.g., `create_workspace`, `run_git_status`, `load_from`)
- Private helper functions use `snake_case` with leading underscore pattern not used
- Function names are action-oriented: `create_`, `delete_`, `load_`, `save_`, `resolve_`, `update_`

**Variables:**
- Local variables use `snake_case`
- Struct fields use `snake_case` (e.g., `workspace_id`, `worktree_path`, `created_at`, `working_dir`)
- Temporary/iterator variables: `tmp`, `i`, `j` for loops

**Types:**
- Public structs use `PascalCase` (e.g., `AppState`, `Workspace`, `Session`, `RepoEntry`, `TreeNode`, `Composer`)
- Enums use `PascalCase` variants (e.g., `SessionStatus::Running`, `Focus::Tree`, `WorktreeResult::Created`)
- Trait names use `PascalCase` (e.g., conventions not yet required, none defined yet)

**Constants:**
- Module constants use `SCREAMING_SNAKE_CASE` (e.g., `STATE_DIR`, `STATE_FILE`, `SPINNER_FRAMES`)

## Code Style

**Formatting:**
- Edition: Rust 2024 (defined in `Cargo.toml` workspace)
- Line width: No explicit rustfmt.toml found; default 100 columns assumed
- Uses standard Rust formatting conventions (no custom config detected)

**Imports Organization:**
- Standard library imports first: `use std::...;`
- External crate imports next: `use anyhow::{...};`, `use serde::{...};`, `use ratatui::{...};`
- Internal crate imports last: `use super::...;`, `use crate::...;`
- Grouped logically by functionality
- Example from `crates/core/src/lib.rs`:
  ```rust
  use std::fs;
  use std::path::{Path, PathBuf};
  use std::time::{SystemTime, UNIX_EPOCH};

  use anyhow::{Context, bail};
  use serde::{Deserialize, Serialize};
  ```

**Path Aliases:**
- Crate imported as `kommand0_core` (workspace member)
- Internal modules accessed via `super::` or direct module path

## Derive Attributes

**Structs and Enums:**
- Common derives: `#[derive(Debug, Clone, Serialize, Deserialize)]` for persistent data
- Status enum also derives `PartialEq`: `#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]`
- Default trait used for state structs: `#[derive(Debug, Default, Serialize, Deserialize)]` for `AppState`
- Example from `crates/core/src/workspace.rs`:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct Workspace {
      pub id: String,
      pub name: String,
      pub repo_id: String,
      pub working_dir: String,
      pub active: bool,
      pub created_at: u64,
      #[serde(default)]
      pub worktree_path: Option<String>,
  }
  ```

## Error Handling

**Pattern:**
- Primary error type: `anyhow::Result<T>` (aliased as `Result<T>` in many functions)
- Error propagation: `?` operator with context
- `bail!` macro for explicit error returns with formatted messages
- `with_context()` for wrapping errors with contextual information

**Examples:**
- Validation errors: `bail!("path does not exist or is not a directory: {}", path)`
- IO operation errors: `.with_context(|| format!("failed to read {}", path.display()))?`
- Lookup failures: `bail!("No repo found matching '{}'. Checked: name, path, id. Use `kmd repo list` to see tracked repos.", reference)`

**Error scenarios with wrapped context:**
```rust
let data = fs::read_to_string(&path)
    .with_context(|| format!("failed to read {}", path.display()))?;
let state: Self = serde_json::from_str(&data)
    .with_context(|| format!("failed to parse {}", path.display()))?;
```

**Unwrap usage:**
- Used in tests and setup code only
- Used when result is guaranteed to succeed: `.expect("time went backwards")` for `SystemTime::now().duration_since(UNIX_EPOCH)`
- Acceptable in tests: `TempDir::new().unwrap()`, `.unwrap()` on test assertions

**Graceful degradation:**
- Some operations return `WorktreeResult` enum with `Fallback` variant instead of error
- Example: If git worktree creation fails, falls back to repo root directory
- Pattern in `crates/core/src/worktree.rs`: Returns `Fallback { reason: String }` for non-fatal failures

**Process errors:**
- Graceful handling of subprocess failures (git commands)
- Logs warnings to stderr instead of panicking: `eprintln!("warning: ...")`
- Returns `Ok(())` even after logged warnings for some operations (non-blocking cleanup)

## Documentation

**Doc comments:**
- Use `///` for public API documentation
- Typically one-liner summaries for simple functions
- Multi-line with extra context for complex functions
- Example from `crates/core/src/lib.rs`:
  ```rust
  /// Load state from the given base directory. Returns default if no state file exists.
  pub fn load_from(base: &Path) -> anyhow::Result<Self> {
  ```

**Inline comments:**
- Sparse; code is generally self-documenting via naming
- Used to explain non-obvious logic or git worktree handling
- Example: "Path-first if input contains '/'" to explain resolution order

## Function Design

**Size:**
- Core logic functions: 50-150 lines (e.g., `create_workspace_impl`, `delete_repo_with_base`)
- Test functions: 5-20 lines
- Rendering functions: 20-100 lines
- Helper functions: 5-30 lines

**Parameters:**
- Prefer immutable references over owned values where possible
- `&Path` for paths instead of `&str`
- `&str` for string literals and simple references
- Optional parameters use `Option<&str>` (e.g., workspace name)
- Base path operations take `&Path` parameter for testing flexibility

**Return Values:**
- Functions return `Option<T>` when lookup may fail but error context not needed
- Functions return `Result<T>` when IO, parsing, or validation is involved
- Tuple returns for multiple values: `(WorktreeResult, String)` patterns avoided
- Enum variants encapsulate multiple return values (e.g., `WorktreeResult::Created { worktree_path, branch_name }`)

**Implementation Patterns:**
- Two-function pattern for state-changing operations:
  - `pub fn operation_with_base(..., base: &Path)` for testable version with custom state dir
  - `pub fn operation(...)` as convenience wrapper calling `_with_base(Self::state_dir())`
  - Example: `add_repo_with_base()` and `add_repo()`
- Private `_impl` methods for shared logic: `create_workspace_impl()` used by multiple public methods
- Resolver pattern: `resolve_repo()` method to handle multiple identification methods (name, path, id)

## Module Design

**Module structure:**
- Core domain logic in `crates/core/src/` (reusable across CLI and TUI)
- App-specific logic in separate app crates (`apps/cli/`, `apps/tui/`)
- Workspace management, session handling, git operations in core

**Exports:**
- `lib.rs` re-exports public types: `pub use id::generate_id;`, `pub use session::Session;`
- Main entry points import from workspace members: `use kommand0_core::{AppState, SessionStatus};`

**Visibility:**
- Core structs fully public with public fields
- Serializable structs use `#[serde(default)]` for backwards compatibility
- TUI internal structs use `pub(crate)` visibility: `pub(crate) enum Status`, `pub(crate) struct App`

## Special Patterns

**Serde compatibility:**
- Structs use `#[serde(default)]` on optional fields for backwards compatibility
- Example in `workspace.rs`: Optional `worktree_path` with `#[serde(default)]`
- Enables loading old state files missing new fields

**Testing setup helpers:**
- Factories for test data: `make_state_with_repo()` creates repo within state
- TempDir cleanup is automatic (no manual cleanup needed)

**Cascading operations:**
- Deletions cascade with validation: delete repo → find related workspaces → delete sessions → delete workspaces
- Pattern: Collect IDs first, then filter/retain in multiple passes

**Status patterns:**
- `SessionStatus` enum variants: `Running`, `Stopped`, `Failed`, `Exited`
- `Focus` enum variants: `Tree`, `Output`, `Composer`
- `Status` enum variants: `Idle`, `Done`, `Error(String)`

---

*Convention analysis: 2026-03-22*
