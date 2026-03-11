# Codebase Structure

**Analysis Date:** 2026-03-11

## Directory Layout

```
san-jose/
├── apps/
│   ├── cli/                # CLI binary (kmd)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs     # CLI entry point, clap parser, command handlers
│   └── tui/                # TUI binary
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs           # App struct, event loop, keybindings
│           ├── render.rs         # All rendering (tree, output, scrollbar, markdown)
│           ├── session_manager.rs # Claude process spawning and streaming
│           ├── scrollback.rs     # Ring buffer for output lines
│           ├── composer.rs       # Multi-line text input widget
│           ├── modal.rs          # Modal dialogs (add repo, add workspace, confirm delete)
│           ├── buttons.rs        # Clickable button hit regions
│           ├── help.rs           # Help overlay keybinding reference
│           └── mouse.rs          # Mouse event handling and pane hit-testing
├── crates/
│   └── core/               # Shared core library
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs       # AppState struct, CRUD methods, re-exports
│           ├── repo.rs      # RepoEntry struct, git status helper
│           ├── session.rs   # Session struct, SessionStatus enum
│           ├── workspace.rs # Workspace struct, format_timestamp helper
│           ├── worktree.rs  # Git worktree create/remove operations
│           └── id.rs        # Timestamp-based ID generation
├── .context/               # Context files (not code)
├── .planning/              # Planning documents
│   └── codebase/           # Codebase analysis documents
├── Cargo.toml              # Workspace manifest
├── Cargo.lock              # Dependency lockfile
├── CLAUDE.md               # AI assistant instructions
├── README.md               # Project documentation
├── LICENSE                  # License file
└── .gitignore              # Git ignore rules
```

## Directory Purposes

**`apps/cli/`:**
- Purpose: Command-line interface binary
- Contains: Single `main.rs` with clap-derived parser and all command implementations
- Key files: `apps/cli/src/main.rs` (413 lines, all CLI logic)
- Binary name: `kmd`

**`apps/tui/`:**
- Purpose: Interactive terminal UI application
- Contains: Modular TUI with separate files per concern
- Key files:
  - `apps/tui/src/main.rs` - App state struct, async event loop, keyboard handling (~800 lines)
  - `apps/tui/src/render.rs` - All rendering logic including markdown (~900 lines)
  - `apps/tui/src/session_manager.rs` - Process management and stream-json parsing (~480 lines)

**`crates/core/`:**
- Purpose: Shared domain logic consumed by both apps
- Contains: Domain models, state persistence, git operations
- Key files:
  - `crates/core/src/lib.rs` - `AppState` with all CRUD methods (~585 lines including tests)
  - `crates/core/src/worktree.rs` - Git worktree operations (~290 lines including tests)

**`.kommand0-dev/` (runtime, not in repo):**
- Purpose: Runtime state directory created at CWD
- Contains: `state.json`, `sessions/*.log`, `worktrees/*/`
- Generated: Yes
- Committed: No (in `.gitignore`)

## Key File Locations

**Entry Points:**
- `apps/cli/src/main.rs`: CLI entry point, binary `kmd`
- `apps/tui/src/main.rs`: TUI entry point

**Configuration:**
- `Cargo.toml`: Workspace-level dependency versions and member list
- `apps/cli/Cargo.toml`: CLI binary configuration
- `apps/tui/Cargo.toml`: TUI binary configuration
- `crates/core/Cargo.toml`: Core library configuration

**Core Logic:**
- `crates/core/src/lib.rs`: `AppState` (central state + all business logic)
- `crates/core/src/repo.rs`: `RepoEntry` model + `run_git_status()`
- `crates/core/src/session.rs`: `Session` model + `SessionStatus` enum
- `crates/core/src/workspace.rs`: `Workspace` model + `format_timestamp()`
- `crates/core/src/worktree.rs`: `create_worktree()`, `remove_worktree()`
- `crates/core/src/id.rs`: `generate_id()` (hex-encoded millisecond timestamp)

**TUI Components:**
- `apps/tui/src/render.rs`: All widget rendering, markdown parsing
- `apps/tui/src/session_manager.rs`: `SessionManager`, `SessionEvent`, stream-json classification
- `apps/tui/src/scrollback.rs`: `ScrollbackBuffer` (VecDeque ring buffer)
- `apps/tui/src/composer.rs`: `Composer` (tui-textarea wrapper)
- `apps/tui/src/modal.rs`: `ModalState`, `ModalResult`, `handle_modal_key()`, path completion
- `apps/tui/src/buttons.rs`: `HitRegion`, `HitAction`, `button_span()`, `is_hovered()`
- `apps/tui/src/help.rs`: `render_help_overlay()`, keybinding constants
- `apps/tui/src/mouse.rs`: `PaneAreas`, `handle_mouse()`, click/scroll dispatch

**Testing:**
- `crates/core/src/lib.rs`: Unit tests for AppState CRUD
- `crates/core/src/session.rs`: Unit tests for session lifecycle
- `crates/core/src/workspace.rs`: Unit tests for workspace operations
- `crates/core/src/worktree.rs`: Integration tests for git worktree operations
- `apps/tui/src/scrollback.rs`: Unit tests for scrollback buffer

## Naming Conventions

**Files:**
- snake_case for all Rust source files: `session_manager.rs`, `scrollback.rs`
- Single `main.rs` per binary crate
- `lib.rs` for library crate root

**Directories:**
- `apps/` for binary crates (CLI, TUI)
- `crates/` for library crates
- Plural nouns for top-level groupings

**Crate Names:**
- `kommand0-cli` -> binary `kmd`
- `kommand0-tui` -> unnamed binary
- `kommand0-core` -> library

**Modules:**
- One module per file in the TUI (no nested module directories)
- Flat module structure in core: `lib.rs` declares `pub mod` for each file

## Where to Add New Code

**New Domain Model:**
- Create: `crates/core/src/{model_name}.rs`
- Register: Add `pub mod {model_name};` in `crates/core/src/lib.rs`
- Add to `AppState` if it needs persistence (add field with `#[serde(default)]` for backward compat)
- Tests: Add `#[cfg(test)] mod tests` in the same file, use `tempfile::TempDir` for state dir

**New CLI Command:**
- Add variant to `Commands` enum in `apps/cli/src/main.rs`
- Add corresponding action enum if subcommands needed
- Implement handler in the `match cli.command` block
- All CLI code lives in the single `main.rs` file

**New TUI Feature/Component:**
- Create: `apps/tui/src/{component}.rs`
- Register: Add `mod {component};` at top of `apps/tui/src/main.rs`
- Use `pub(crate)` visibility for types shared across TUI modules
- If it needs rendering, add render function and call from `apps/tui/src/render.rs`
- If it needs key handling, integrate into the event loop in `apps/tui/src/main.rs`

**New TUI Modal:**
- Add variant to `ModalState` enum in `apps/tui/src/modal.rs`
- Add `ModalResult` variant for the submit action
- Handle in `handle_modal_key()` function
- Add rendering in `render_modal()` function
- Integrate submit result in the modal handler section of `apps/tui/src/main.rs`

**New Core Utility:**
- Shared helpers: `crates/core/src/` as a new module
- TUI-only helpers: In the relevant TUI module file
- CLI-only helpers: In `apps/cli/src/main.rs` (or split into modules if it grows)

**New Session Event Type:**
- Add variant to `SessionEvent` enum in `apps/tui/src/session_manager.rs`
- Handle in the stdout reader task where events are classified
- Process in the event loop's `SessionEvent` match block in `apps/tui/src/main.rs`

## Special Directories

**`.kommand0-dev/` (runtime):**
- Purpose: All runtime state and artifacts
- Generated: Yes, at current working directory
- Committed: No
- Contents: `state.json`, `sessions/*.log`, `worktrees/*/`

**`.context/`:**
- Purpose: Project context files
- Generated: No
- Committed: Yes

**`.planning/`:**
- Purpose: Planning and analysis documents
- Generated: Partially (by analysis tools)
- Committed: No (in `.gitignore`)

---

*Structure analysis: 2026-03-11*
