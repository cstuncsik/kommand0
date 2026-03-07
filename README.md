# kommand0

Keyboard-first local orchestrator for parallel coding sessions.

## Prerequisites

- Rust toolchain (edition 2024)
- Git on PATH
- macOS

## Build

```sh
cargo build --workspace
```

## CLI Usage

```sh
# Add a repo
cargo run -p kommand0-cli -- repo add /path/to/your/repo

# List tracked repos
cargo run -p kommand0-cli -- repo list
```

## TUI

```sh
cargo run -p kommand0-tui
```

### Keybindings

- `j` / `k` or `Up` / `Down` - navigate repos
- `Enter` - run git status on selected repo
- `q` - quit

## Test

```sh
cargo test --workspace
```

## Workspace Structure

- `apps/cli` - Command-line interface (`kommand0-cli` binary)
- `apps/tui` - Terminal UI (ratatui + crossterm)
- `crates/core` - Shared core library (models, state, git helpers)

## State

State is stored in `.kommand0-dev/state.json` relative to the current directory.
