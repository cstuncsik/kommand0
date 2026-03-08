# kommand0

Keyboard-first local orchestrator for parallel Claude Code sessions.

## Prerequisites

- Rust toolchain (edition 2024)
- [Claude CLI](https://docs.anthropic.com/en/docs/claude-cli) installed and authenticated
- Git on PATH
- macOS

## Build

```sh
cargo build --workspace
```

## CLI Usage

```sh
# Repos
kmd repo add /path/to/your/repo
kmd repo list

# Workspaces
kmd workspace create --repo <name-or-path>
kmd workspace list
kmd workspace show <name>
kmd workspace archive <name>

# Sessions
kmd session start <workspace>
kmd session stop <workspace>
kmd session list
kmd session clear <workspace>
```

> Replace `kmd` with `cargo run -p kommand0-cli --` during development.

## TUI

```sh
cargo run -p kommand0-tui
```

### Keybindings

| Key | Context | Action |
|-----|---------|--------|
| `j` / `k` / `Up` / `Down` | Tree | Navigate workspaces |
| `Enter` | Tree | Select workspace |
| `r` | Tree | Start Claude session in selected workspace |
| `R` | Tree | Restart / resume session |
| `Ctrl+C` | Tree / Output | Stop running session |
| `Tab` | Any | Cycle focus: Tree -> Output -> Composer |
| `Shift+Tab` | Any | Reverse cycle focus |
| `Esc` | Any | Return focus to Tree |
| `j` / `k` | Output | Scroll output 1 line |
| `PageUp` / `PageDown` | Output | Scroll output 20 lines |
| `G` | Output | Jump to bottom |
| `i` | Output | Switch focus to Composer |
| `Enter` | Composer | Send message |
| `q` | Tree / Output | Quit (stops all sessions) |

## Test

```sh
cargo test --workspace
```

## Project Structure

- `apps/cli` - Command-line interface (`kommand0-cli`)
- `apps/tui` - Terminal UI (ratatui + crossterm + tokio)
- `crates/core` - Shared core library (models, state, git helpers, session management)

## State

State is stored in `.kommand0-dev/state.json` relative to the current directory. Session logs are written as JSON lines files in `.kommand0-dev/logs/`.
