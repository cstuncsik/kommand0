# Coding Conventions

**Analysis Date:** 2026-03-11

## Naming Patterns

**Files:**
- Use `snake_case.rs` for all Rust source files
- Module files named after their domain concept: `session.rs`, `workspace.rs`, `worktree.rs`, `repo.rs`
- TUI feature modules named by UI concern: `composer.rs`, `scrollback.rs`, `modal.rs`, `mouse.rs`, `buttons.rs`, `help.rs`, `render.rs`

**Functions:**
- Use `snake_case` for all functions
- Prefix with verb: `create_workspace`, `delete_repo`, `handle_click`, `render_modal`
- Private helpers use descriptive names without `_impl` suffix except for one case: `create_workspace_impl` in `crates/core/src/lib.rs`
- Boolean-returning functions use `is_` prefix: `is_git_repo`, `is_empty`, `is_active`, `is_hovered`, `is_at_bottom`
- Getter functions omit `get_` prefix: `scroll_offset()`, `page_size()`, `height_hint()`
- Exception: `get_claude_session_id()` in `apps/tui/src/session_manager.rs`

**Variables:**
- Use `snake_case` for all variables
- Abbreviations kept short but readable: `ws` for workspace, `buf` for buffer, `sid` for session_id, `tx`/`rx` for channel ends
- Prefix iterators/closures contextually: `ws_ids`, `repo_workspaces`, `comp_lines`

**Types:**
- Use `PascalCase` for structs, enums, traits
- Enum variants are `PascalCase`: `SessionStatus::Running`, `Focus::Composer`, `ModalState::AddRepo`
- Struct names reflect domain: `AppState`, `RepoEntry`, `Workspace`, `Session`, `ScrollbackBuffer`

**Constants:**
- Use `SCREAMING_SNAKE_CASE`: `STATE_DIR`, `STATE_FILE`, `SPINNER_FRAMES`
- Static arrays use `const` with explicit type: `const GLOBAL_BINDINGS: &[KeyBinding] = &[...]`

## Code Style

**Formatting:**
- Default `rustfmt` settings (no `.rustfmt.toml` present)
- 4-space indentation
- Trailing commas in multi-line structs, enums, and function calls
- Line width appears to be standard 100-character default

**Linting:**
- No `clippy.toml` present; uses default clippy settings
- `#[allow(dead_code)]` used liberally in TUI code for fields/methods not yet wired up
- Derive macros ordered consistently: `Debug, Clone, Serialize, Deserialize` for data types
- `PartialEq` added only when needed for comparisons (e.g., `SessionStatus`)

## Import Organization

**Order:**
1. `std` library imports
2. External crate imports (alphabetical by crate)
3. Internal crate imports (`kommand0_core::...`)
4. Local module imports (`use super::...`, `use crate::...`)

**Style:**
- Group related imports with nested paths: `use ratatui::{layout::{...}, style::{...}, ...}`
- Use explicit item imports, not glob imports (no `use foo::*`)
- `use super::*` allowed only in `#[cfg(test)]` modules

**Path Aliases:**
- No path aliases configured; all imports use full crate paths
- Workspace crate referenced as `kommand0_core`

## Error Handling

**Core crate (`crates/core/`):**
- Use `anyhow::Result<T>` for all fallible public functions
- Use `anyhow::bail!()` for early-return errors with descriptive messages
- Use `.with_context(|| format!(...))` to add context to propagated errors
- Error messages are user-facing and actionable: `"No repo found matching '{}'. Checked: name, path, id. Use 'kmd repo list' to see tracked repos."`
- `thiserror` is a dependency but not yet used (no custom error types defined)

**TUI app (`apps/tui/`):**
- Use `anyhow::Result<T>` at the session manager boundary
- Use `anyhow!()` macro for inline error construction
- Swallow non-critical errors with `let _ = ...` pattern (e.g., worktree cleanup, file deletion)
- Print warnings to stderr for non-fatal failures: `eprintln!("warning: ...")`

**CLI app (`apps/cli/`):**
- `main()` returns `anyhow::Result<()>` for automatic error display
- Uses `anyhow::bail!()` for user-facing errors
- Pattern: check preconditions, bail with message, or proceed

**Pattern to follow for new code:**
```rust
// Public API: return anyhow::Result with context
pub fn do_thing(input: &str) -> anyhow::Result<Output> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if !valid(&data) {
        bail!("invalid data in {}: expected X", path.display());
    }
    Ok(output)
}

// Non-critical side effect: swallow with let _ =
let _ = fs::remove_file(log_path);
```

## Logging

**Framework:** `tracing` is a workspace dependency but not actively used in application code.

**Current practice:**
- `eprintln!()` for warnings in non-critical paths (worktree cleanup)
- `println!()` for CLI user output
- No structured logging in the codebase yet

## Comments

**When to Comment:**
- Doc comments (`///`) on all public functions and types in core crate
- Doc comments on key structs in TUI (e.g., `SessionManager`, `Composer`)
- Inline comments for non-obvious logic: scroll calculations, worktree cleanup reasoning
- Section separators: `// --- Workspace methods ---`, `// --- Session methods ---`

**Doc Comment Style:**
```rust
/// Short description of what the function does.
///
/// Additional context about behavior, edge cases, or fallback logic.
/// Uses `backtick` for parameter names and code references.
pub fn function_name(...) -> ... {
```

**No JSDoc/TSDoc equivalent patterns** -- this is a pure Rust project.

## Function Design

**Size:**
- Core business logic functions are moderate (20-60 lines)
- Render functions in TUI can be large (100+ lines for `render_right_pane`)
- Helper functions extracted when logic is reused: `truncate_path`, `wrapped_line_height`, `parse_inline_markdown`

**Parameters:**
- Use `&str` for string parameters (not `String`)
- Use `&Path` for filesystem paths
- Use `Option<&str>` for optional string params
- Use `&self` / `&mut self` consistently

**Return Values:**
- `anyhow::Result<T>` for fallible operations
- `Option<T>` for lookups that may not find a match
- Return owned values (`String`, `Vec<T>`) from constructors
- Return references (`&T`, `&[T]`) from accessors

**Pattern for dual-base methods:**
```rust
// Public convenience method using default state dir
pub fn do_thing(&mut self, name: &str) -> anyhow::Result<Thing> {
    self.do_thing_with_base(name, Self::state_dir().as_path())
}

// Testable method accepting custom base directory
pub fn do_thing_with_base(&mut self, name: &str, base: &Path) -> anyhow::Result<Thing> {
    // actual implementation
}
```
This pattern exists throughout `crates/core/src/lib.rs` for every state-mutating operation. Use it when adding new operations to `AppState`.

## Module Design

**Exports in core crate (`crates/core/src/lib.rs`):**
- Re-export key types at crate root: `pub use session::{Session, SessionStatus}`
- Re-export key functions: `pub use id::generate_id`
- Submodule types accessed via path for less-common items: `kommand0_core::workspace::format_timestamp`

**Barrel Files:**
- `crates/core/src/lib.rs` serves as the barrel file for the core crate
- No barrel files in TUI app; modules imported directly

**Visibility in TUI:**
- Module-level items use `pub(crate)` for cross-module access within the TUI binary
- Internal helpers are private (no visibility modifier)
- Pattern: `pub(crate) struct`, `pub(crate) enum`, `pub(crate) fn`

## Data Serialization

**Pattern:**
- All persistent data types derive `Serialize, Deserialize`
- Use `#[serde(default)]` for backward-compatible field additions
- State persisted as pretty-printed JSON via `serde_json::to_string_pretty`
- JSON files are the only persistence format (no database)

**Example for adding a new field to an existing struct:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExistingStruct {
    pub existing_field: String,
    #[serde(default)]  // Required for backward compatibility
    pub new_field: Option<String>,
}
```

## Process Management

**Pattern for spawning external processes:**
- Use `std::process::Command` in core/CLI (synchronous)
- Use `tokio::process::Command` in TUI (async)
- Always set `.env_remove("CLAUDECODE")` when spawning claude CLI
- Use `.process_group(0)` and `.kill_on_drop(true)` for TUI child processes
- Signal handling: SIGTERM first, wait, then SIGKILL as fallback
- Send signals to process group (`-pgid`) not individual PID

---

*Convention analysis: 2026-03-11*
