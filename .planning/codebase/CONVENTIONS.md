# Coding Conventions

**Analysis Date:** 2026-03-07

## Naming Patterns

**Files:**
- Use `main.rs` for binary entry points in each app crate
- Use `lib.rs` for library crate roots
- snake_case for all Rust source files (standard Rust convention)

**Functions:**
- snake_case for all functions: `add_repo`, `run_git_status`, `move_up`, `move_down`
- Short, verb-first names describing the action
- Private helpers use plain snake_case without prefixes: `generate_id()`, `state_dir()`, `state_file()`

**Variables:**
- snake_case for all variables: `canonical_str`, `repo_path`, `output_text`
- Short, descriptive names preferred over abbreviations
- Single-letter variables only in small closures: `|r|`, `|n|`, `|e|`

**Types:**
- PascalCase for structs and enums: `AppState`, `RepoEntry`, `Status`, `RepoAction`
- Enum variants are PascalCase: `Status::Idle`, `Status::Done`, `Status::Error(String)`
- Derive macros listed on the line above the struct/enum definition

**Constants:**
- Associated constants use SCREAMING_SNAKE_CASE: `const STATE_DIR: &str`, `const STATE_FILE: &str`
- Defined as associated constants on the relevant struct (not module-level) when they belong to a type

## Code Style

**Formatting:**
- No `.rustfmt.toml` or `rustfmt.toml` detected -- use default `rustfmt` settings
- Run `cargo fmt` before committing
- Rust 2024 edition (set in `Cargo.toml` workspace)

**Linting:**
- No `.clippy.toml` detected -- use default `clippy` settings
- Run `cargo clippy` for lint checks
- No CI pipeline detected; linting is manual

## Import Organization

**Order:**
1. `std` library imports (grouped with nested braces): `use std::fs;`, `use std::path::PathBuf;`
2. External crate imports: `use anyhow::{Context, bail};`, `use serde::{Deserialize, Serialize};`
3. Internal workspace crate imports: `use kommand0_core::AppState;`

**Style:**
- Group related imports from the same crate using nested braces: `use crossterm::{execute, terminal::{...}};`
- One `use` statement per crate (with nested paths), not one per item
- No blank lines between import groups (current style); consider adding them for clarity in larger files

**Path Aliases:**
- Workspace dependencies referenced via `kommand0_core` (the crate name)
- No path aliases or custom module re-exports

## Error Handling

**Strategy: `anyhow` for application errors, `thiserror` available for typed errors**

**Patterns:**
- Use `anyhow::Result<T>` as the return type for all fallible functions
- Use `anyhow::bail!()` for early returns with error messages: `bail!("path does not exist or is not a directory: {}", path)` in `crates/core/src/lib.rs`
- Use `.with_context(|| format!(...))` to add context to `std::io::Error` and other errors: see `AppState::load()` and `AppState::save()` in `crates/core/src/lib.rs`
- Use `?` operator for propagation -- never `.unwrap()` on Results in production paths
- `.expect()` is acceptable only for truly impossible conditions: `generate_id()` in `crates/core/src/lib.rs` uses `.expect("time went backwards")`
- In the TUI, errors are captured into an enum variant (`Status::Error(String)`) and displayed in the UI rather than propagated -- see `App::run_status()` in `apps/tui/src/main.rs`

**Rules for new code:**
- Use `anyhow::Result` for functions that can fail
- Add `.with_context()` to any I/O or parsing operation with a descriptive message
- Use `bail!()` for validation failures
- Never use `.unwrap()` on `Result` or `Option` in production code paths
- Use `thiserror` when defining domain-specific error enums (declared as workspace dep but not yet used)

## Logging

**Framework:** `tracing` (declared as workspace dependency in `crates/core/Cargo.toml`)

**Current usage:** Not yet used in any source files. `println!()` is used for CLI output in `apps/cli/src/main.rs`.

**Rules for new code:**
- Use `tracing` macros (`tracing::info!`, `tracing::error!`, etc.) for structured logging
- Use `println!()` only for direct user-facing CLI output, not for debugging or logging
- Do not use `eprintln!()` for error reporting; propagate errors via `anyhow::Result` instead

## Comments

**When to Comment:**
- Use `///` doc comments on public items (CLI subcommands use `///` for clap help text): see `apps/cli/src/main.rs`
- Use `//` inline comments sparingly to explain non-obvious UI layout decisions: see `apps/tui/src/main.rs` lines 101, 119
- No comments on self-explanatory code

**JSDoc/TSDoc:** Not applicable (Rust project)

## Function Design

**Size:** Functions are small -- longest is `App::run_status()` at ~12 lines. Keep functions under 30 lines.

**Parameters:**
- Use `&str` for string inputs (not `String`): `add_repo(&mut self, path: &str)`, `run_git_status(repo_path: &str)`
- Use `&self` / `&mut self` for methods
- Avoid more than 3 parameters; use a struct if needed

**Return Values:**
- Return `anyhow::Result<T>` for fallible operations
- Return `Option<T>` for nullable values: `selected_index(&self) -> Option<usize>`
- Avoid returning raw tuples; use named structs

## Module Design

**Exports:**
- All public types and functions are exported from `crates/core/src/lib.rs` directly (flat module structure)
- Public fields on structs use `pub` directly: `pub id: String`, `pub repos: Vec<RepoEntry>`
- Private helpers are module-private (no `pub`): `generate_id()`, `state_dir()`, `state_file()`

**Barrel Files:**
- `lib.rs` serves as the single public API surface for `kommand0-core`
- No sub-modules exist yet; when adding them, re-export key types from `lib.rs`

## Derive Macro Conventions

**Pattern:** List derives on the line above the type definition:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEntry { ... }

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppState { ... }
```

**Standard derives to include:**
- `Debug` on all public types
- `Clone` on types that need to be copied (data transfer structs)
- `Serialize, Deserialize` on all persisted types
- `Default` when a sensible zero-value exists
- `Parser`, `Subcommand` for clap CLI types (in `apps/cli`)

## Workspace Dependency Management

**Pattern:** Declare all shared dependencies in the workspace `Cargo.toml` under `[workspace.dependencies]`, then reference them with `.workspace = true` in member crates.

**Example from `apps/cli/Cargo.toml`:**
```toml
[dependencies]
anyhow.workspace = true
clap.workspace = true
kommand0-core.workspace = true
```

**Rule:** Always add new dependencies to `[workspace.dependencies]` in the root `Cargo.toml` first, then reference with `.workspace = true` in the member crate.

---

*Convention analysis: 2026-03-07*
