# Codebase Structure

**Analysis Date:** 2026-03-07

## Directory Layout

```
minnetonka-v1/
├── apps/
│   ├── cli/                # CLI binary (kmd)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   └── tui/                # TUI binary (kommand0-tui)
│       ├── Cargo.toml
│       └── src/
│           └── main.rs
├── crates/
│   └── core/               # Shared domain logic (kommand0-core)
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs
├── .context/               # Project context notes
│   ├── notes.md
│   └── todos.md
├── .planning/              # GSD planning documents
│   └── codebase/
├── Cargo.toml              # Workspace root manifest
├── Cargo.lock              # Dependency lockfile
├── gds-brief.md            # Project brief and roadmap
├── .gitignore
├── LICENSE
└── README.md
```

## Directory Purposes

**`apps/`:**
- Purpose: Application binaries (thin frontends)
- Contains: CLI and TUI entry points
- Key files: `apps/cli/src/main.rs`, `apps/tui/src/main.rs`

**`apps/cli/`:**
- Purpose: Command-line interface binary
- Binary name: `kmd` (configured in `apps/cli/Cargo.toml`)
- Contains: clap command definitions and dispatch logic
- Key files: `apps/cli/src/main.rs`

**`apps/tui/`:**
- Purpose: Terminal UI application
- Contains: ratatui rendering, keyboard event loop, TUI-local view state
- Key files: `apps/tui/src/main.rs`

**`crates/`:**
- Purpose: Shared libraries
- Contains: Core domain logic shared between CLI and TUI

**`crates/core/`:**
- Purpose: Domain models, persistence, and shared helpers
- Package name: `kommand0-core`
- Contains: `RepoEntry`, `AppState`, `run_git_status()`
- Key files: `crates/core/src/lib.rs`

**`.context/`:**
- Purpose: Project notes and todos (currently empty)
- Contains: `notes.md`, `todos.md`

**`.planning/`:**
- Purpose: GSD planning and codebase analysis documents
- Contains: `codebase/` subdirectory for mapping docs
- Generated: Yes (by GSD tooling)
- Committed: Yes

## Key File Locations

**Entry Points:**
- `apps/cli/src/main.rs`: CLI binary entry point (`kmd`)
- `apps/tui/src/main.rs`: TUI binary entry point

**Configuration:**
- `Cargo.toml`: Workspace root -- defines members, shared dependencies, edition
- `apps/cli/Cargo.toml`: CLI crate config, defines `kmd` binary name
- `apps/tui/Cargo.toml`: TUI crate config
- `crates/core/Cargo.toml`: Core library crate config

**Core Logic:**
- `crates/core/src/lib.rs`: All domain logic -- models, persistence, git helpers

**Runtime State:**
- `.kommand0-dev/state.json`: Persisted app state (created at runtime, gitignored)

**Project Brief:**
- `gds-brief.md`: Product vision, requirements, milestone roadmap, architecture direction

## Naming Conventions

**Files:**
- Rust standard: `main.rs` for binaries, `lib.rs` for libraries
- Snake_case for any additional Rust source files (none yet beyond entry points)

**Crate Names:**
- Prefix: `kommand0-` (e.g., `kommand0-core`, `kommand0-cli`, `kommand0-tui`)
- Import alias: underscores replace hyphens (`kommand0_core`)

**Structs/Enums:**
- PascalCase: `RepoEntry`, `AppState`, `Commands`, `RepoAction`, `Status`

**Functions:**
- snake_case: `run_git_status`, `add_repo`, `move_up`, `move_down`

**Constants:**
- SCREAMING_SNAKE_CASE: `STATE_DIR`, `STATE_FILE`

**Directories:**
- Lowercase, short names: `apps/`, `crates/`, `core/`, `cli/`, `tui/`

## Where to Add New Code

**New Domain Model (e.g., Workspace, Session):**
- Add struct and impl to `crates/core/src/lib.rs`
- When `lib.rs` grows large, split into modules: `crates/core/src/models.rs`, `crates/core/src/persistence.rs`, etc.
- Re-export from `crates/core/src/lib.rs`

**New CLI Subcommand:**
- Add variant to `Commands` enum in `apps/cli/src/main.rs`
- Add corresponding action enum if needed (like `RepoAction`)
- Dispatch to core logic in the `match` block

**New TUI View/Pane:**
- Add rendering logic in `apps/tui/src/main.rs`
- When the file grows, consider splitting into modules: `apps/tui/src/views/`, `apps/tui/src/input.rs`

**New Shared Utility:**
- Add to `crates/core/src/lib.rs` (or a new module under `crates/core/src/`)

**New Crate:**
- Create directory under `crates/` (e.g., `crates/git/`)
- Add to workspace members in root `Cargo.toml`
- Follow naming pattern: `kommand0-<name>`

**Tests:**
- Unit tests: Add `#[cfg(test)] mod tests` block in the same file as the code being tested
- Integration tests: Create `crates/core/tests/` directory
- No test files exist yet

## Special Directories

**`.kommand0-dev/`:**
- Purpose: Runtime application state storage
- Generated: Yes (created by `AppState::save()` at runtime)
- Committed: No (gitignored via `/target` pattern or explicit ignore)
- Contains: `state.json` with serialized `AppState`

**`target/`:**
- Purpose: Cargo build artifacts
- Generated: Yes
- Committed: No (gitignored)

---

*Structure analysis: 2026-03-07*
