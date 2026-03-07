# Technology Stack

**Analysis Date:** 2026-03-07

## Languages

**Primary:**
- Rust (2024 edition) - All application and library code

**Secondary:**
- None

## Runtime

**Environment:**
- Native binary compilation (no runtime required)
- macOS-first target platform

**Package Manager:**
- Cargo (Rust standard)
- Lockfile: `Cargo.lock` present (version 4)

## Workspace Structure

**Cargo Workspace** with resolver version 3.

| Crate | Type | Binary Name | Purpose |
|-------|------|-------------|---------|
| `kommand0-cli` | Binary | `kmd` | CLI entry point |
| `kommand0-tui` | Binary | (default) | Terminal UI application |
| `kommand0-core` | Library | N/A | Shared domain logic |

- Workspace root: `Cargo.toml`
- CLI manifest: `apps/cli/Cargo.toml`
- TUI manifest: `apps/tui/Cargo.toml`
- Core manifest: `crates/core/Cargo.toml`

## Frameworks

**Core:**
- ratatui `0.29` - Terminal UI framework (`apps/tui/Cargo.toml`)
- crossterm `0.28` - Terminal backend for ratatui (`apps/tui/Cargo.toml`)
- clap `4` (with `derive` feature) - CLI argument parsing (`apps/cli/Cargo.toml`)
- tokio `1` (with `full` features) - Async runtime (`crates/core/Cargo.toml`), declared but not yet used in current code

**Testing:**
- No test framework configured beyond Rust's built-in `#[test]` infrastructure
- No test files exist yet

**Build/Dev:**
- Cargo (standard Rust build system)
- No custom build scripts detected
- No CI configuration files present

## Key Dependencies

**Critical (workspace-level):**
- `clap` 4 (derive) - CLI argument parsing and help generation (`apps/cli/src/main.rs`)
- `ratatui` 0.29 - TUI rendering, widgets, layout (`apps/tui/src/main.rs`)
- `crossterm` 0.28 - Terminal raw mode, alternate screen, key events (`apps/tui/src/main.rs`)
- `serde` 1 (derive) - State serialization/deserialization (`crates/core/src/lib.rs`)
- `serde_json` 1 - JSON state file read/write (`crates/core/src/lib.rs`)
- `tokio` 1 (full) - Async runtime, declared for future session/process management (`crates/core/Cargo.toml`)

**Error Handling:**
- `anyhow` 1 - Application-level error handling, used across all crates
- `thiserror` 2 - Typed error definitions (`crates/core/Cargo.toml`), declared but not yet used in code

**Observability:**
- `tracing` 0.1 - Structured logging (`crates/core/Cargo.toml`), declared but not yet used in code

**Internal:**
- `kommand0-core` (path dependency) - Shared domain logic, used by both `apps/cli` and `apps/tui`

## Configuration

**Environment:**
- No environment variables required
- No `.env` files present
- Local-only application with no external service dependencies

**State/Data:**
- Application state stored in `.kommand0-dev/state.json` (relative to working directory)
- `.kommand0-dev/` is gitignored
- State format: JSON via serde_json, human-readable (`serde_json::to_string_pretty`)

**Build:**
- `Cargo.toml` (workspace root) - workspace members and shared dependency versions
- No `rust-toolchain.toml` detected - relies on system Rust installation
- Requires Rust edition 2024 support (nightly or recent stable)

## Platform Requirements

**Development:**
- Rust toolchain with edition 2024 support
- macOS (primary target)
- Git (required at runtime for `git status` execution)
- Terminal emulator with raw mode support

**Production:**
- Self-contained native binary
- No external services or databases required
- Git must be available on PATH

## External Tools (Runtime)

- `git` - Invoked via `std::process::Command` in `crates/core/src/lib.rs` for `git -C <path> status --short --branch`

---

*Stack analysis: 2026-03-07*
