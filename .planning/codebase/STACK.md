# Technology Stack

**Analysis Date:** 2026-03-11

## Languages

**Primary:**
- Rust (Edition 2024) - All application and library code

**Secondary:**
- None

## Runtime

**Environment:**
- Rust native binary (no VM/interpreter)
- Async runtime: Tokio (full features) for TUI and session management
- Minimum Rust edition: 2024 (set in `Cargo.toml` workspace)

**Package Manager:**
- Cargo (workspace mode)
- Lockfile: `Cargo.lock` present and committed (version 4)

## Frameworks

**Core:**
- `ratatui` 0.29 - Terminal UI framework (`apps/tui/`)
- `crossterm` 0.28 (with `event-stream` feature) - Terminal backend and event handling (`apps/tui/`)
- `clap` 4 (with `derive` feature) - CLI argument parsing (`apps/cli/`)
- `tokio` 1 (with `full` features) - Async runtime (`apps/tui/`, `crates/core/`)

**Testing:**
- Built-in `#[test]` with `cargo test` - No external test framework
- `tempfile` 3 - Temporary directories for test isolation (dev-dependency of `crates/core`)

**Build/Dev:**
- Cargo workspace - Monorepo build orchestration
- Cargo resolver version 3

## Key Dependencies

**Critical:**
- `ratatui` 0.29 - Entire TUI rendering layer depends on this
- `crossterm` 0.28 - Terminal I/O, event streams, mouse support
- `tokio` 1 - Async process spawning, channels, timers for session management
- `clap` 4 - CLI argument parsing with derive macros

**Infrastructure:**
- `serde` 1 (with `derive`) - Serialization for all domain types and state persistence
- `serde_json` 1 - JSON state file format, Claude CLI stream-json protocol parsing
- `anyhow` 1 - Application-level error handling in CLI and TUI
- `thiserror` 2 - Typed errors in core library
- `nix` 0.30 (with `process`, `signal` features) - Unix process group management (SIGTERM/SIGKILL)
- `uuid` 1 (with `v4` feature) - Session ID generation
- `chrono` 0.4 - Timestamp formatting for display
- `futures` 0.3 - Stream combinators for TUI event loop
- `tui-textarea` 0.7 - Multi-line text input widget for composer
- `strip-ansi-escapes` 0.2 - Clean ANSI codes from Claude CLI output
- `tracing` 0.1 - Structured logging (declared but lightly used)

## Workspace Structure

**Workspace members (defined in root `Cargo.toml`):**
- `apps/cli` - Binary crate `kmd` (`kommand0-cli`)
- `apps/tui` - Binary crate (`kommand0-tui`)
- `crates/core` - Library crate (`kommand0-core`)

**Dependency graph:**
- `apps/cli` depends on: `kommand0-core`, `clap`, `anyhow`, `tokio`, `serde_json`, `nix`
- `apps/tui` depends on: `kommand0-core`, `ratatui`, `crossterm`, `tokio`, `futures`, `nix`, `tui-textarea`, `strip-ansi-escapes`, `serde_json`, `serde`, `uuid`
- `crates/core` depends on: `tokio`, `serde`, `serde_json`, `thiserror`, `anyhow`, `tracing`, `chrono`, `uuid`

## Configuration

**Environment:**
- No `.env` files used
- No environment variables required for the application itself
- `CLAUDECODE` env var is explicitly removed when spawning Claude CLI processes (see `apps/cli/src/main.rs` line 299, `apps/tui/src/session_manager.rs` line 104)

**Build:**
- `Cargo.toml` (root) - Workspace configuration with shared dependency versions
- `apps/cli/Cargo.toml` - CLI binary crate config
- `apps/tui/Cargo.toml` - TUI binary crate config
- `crates/core/Cargo.toml` - Core library crate config
- No `.rustfmt.toml` or `rust-toolchain.toml` detected

**State:**
- Application state persisted to `.kommand0-dev/state.json` (JSON via serde)
- Session logs stored in `.kommand0-dev/sessions/<id>.log`
- `.kommand0-dev/` is gitignored

## Platform Requirements

**Development:**
- Rust toolchain (edition 2024, tested with rustc 1.88.0)
- Git on PATH (used for worktree operations and repo status)
- Claude CLI installed and authenticated (spawned as child process)
- macOS (stated in README; `nix` crate for Unix signals limits to Unix-like OSes)

**Production:**
- Native binary, no runtime dependencies beyond system libraries
- Runs locally only (no server/deployment target)
- Target platform: macOS (per README)

---

*Stack analysis: 2026-03-11*
