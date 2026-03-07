# kommand0

Keyboard-first local orchestrator for parallel coding sessions.

## Build

```sh
cargo build
```

## CLI Usage (kmd)

```sh
# Add a repo
kmd repo add /path/to/your/repo

# List tracked repos
kmd repo list
```

State is stored in `.kommand0-dev/state.json` relative to the current directory.

## TUI

```sh
cargo run -p kommand0-tui
```

- `j` / `k` or `Up` / `Down` - navigate repos
- `Enter` - run git status on selected repo
- `q` - quit

## Workspace Structure

- `apps/cli` - Command-line interface (`kmd` binary)
- `apps/tui` - Terminal UI (ratatui + crossterm)
- `crates/core` - Shared core library (models, state, git helpers)
