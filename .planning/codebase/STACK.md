# Technology Stack

**Analysis Date:** 2026-03-22

## Languages

**Primary:**
- Rust 2024 edition - Core library, TUI application, CLI tool

**Secondary:**
- None

## Runtime

**Environment:**
- Native compilation to system binary (x86_64, ARM64)
- Runtime: Rust runtime (no external VM required)

**Package Manager:**
- Cargo - Rust package manager and build system
- Lockfile: `Cargo.lock` present (committed)

## Frameworks

**Core:**
- None (pure Rust with focused dependencies)

**UI/Terminal:**
- `ratatui` 0.29 - Terminal UI framework for rendering and layout
- `crossterm` 0.28 - Terminal control (events, colors, cursor)
- `tui-textarea` 0.7 - Text input widget for TUI

**CLI:**
- `clap` 4.5 - Command-line argument parsing with derive macros

**Async/Concurrency:**
- `tokio` 1.50 - Async runtime with full features enabled
- `futures` 0.3 - Async utilities and combinators

## Key Dependencies

**Critical:**
- `serde` 1.0 - Serialization framework (used for JSON persistence)
- `serde_json` 1.0 - JSON serialization/deserialization for state management
- `uuid` 1.22 - UUID generation for IDs (v4 feature)
- `chrono` 0.4 - Timestamp formatting for display

**Error Handling:**
- `anyhow` 1.0 - Flexible error handling (primary for core library)
- `thiserror` 2.0 - Derive macros for error types (available but not actively used in core)

**System Integration:**
- `nix` 0.30 - Unix system calls (process, signal handling)

**Utilities:**
- `strip-ansi-escapes` 0.2 - Parse ANSI escape sequences from shell output
- `unicode-width` 0.2 - Character width calculation for text rendering

**Testing:**
- `tempfile` 3.0 - Temporary file/directory creation (dev dependency for core)

## Configuration

**Environment:**
- No `.env` file infrastructure
- Configuration via CLI arguments (for `kmd` CLI) or TUI interaction
- State persisted to `.kommand0-dev/state.json` and session logs to `.kommand0-dev/sessions/`

**Build:**
- Workspace manifest: `Cargo.toml`
- Package manifests:
  - `crates/core/Cargo.toml` - Core library (kommand0-core)
  - `apps/cli/Cargo.toml` - CLI application (kommand0-cli / kmd)
  - `apps/tui/Cargo.toml` - TUI application (kommand0-tui)
- Shared dependencies defined in workspace `[workspace.dependencies]` section

## Platform Requirements

**Development:**
- Rust 1.88.0 or compatible
- Cargo (comes with Rust)
- Unix-like system (uses `nix` crate for signal/process handling)
- Git (required for worktree operations)

**Production:**
- Unix-like system (macOS, Linux)
- Git installation (for `git worktree` and `git status` commands)
- Terminal supporting ANSI escape codes

## Architecture Overview

**Workspace Model:**
- Monorepo structure with three crates:
  - `kommand0-core`: Shared domain logic (repos, workspaces, sessions, state management)
  - `kommand0-cli`: CLI binary (`kmd` command)
  - `kommand0-tui`: Interactive terminal UI

**State Management:**
- JSON-based persistence in `~/.kommand0-dev/state.json`
- Session logs stored as text files in `~/.kommand0-dev/sessions/`
- In-memory state loaded on startup, persisted on changes

**Process Model:**
- Single-threaded event loop in TUI (using `tokio` for async operations)
- Command spawning via `std::process::Command`
- Session lifecycle tracked in state (Running, Stopped, Failed, Exited)

---

*Stack analysis: 2026-03-22*
