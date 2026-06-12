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
kmd repo delete <name-or-path> [--force]

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

### Features

- **3-pane layout**: Tree (repos/workspaces), Output (streaming Claude responses), Composer (message input)
- **Streaming output**: Responses stream in real-time with markdown rendering (headers, bold, italic, code blocks, lists)
- **Mouse support**: Click to focus panes, click tree items, scroll wheel in output and tree
- **Buttons**: Clickable [Start], [Stop], [Resume] with hover highlighting
- **Modals**: Add repos (`a`) and workspaces (`w`) directly from the TUI with path tab-completion
- **Git worktrees**: Each workspace gets an isolated git worktree branch
- **Activity indicator**: Animated spinner on workspace tree item while Claude is thinking
- **Zoom mode**: Full-screen output with `z`, status bar shows workspace/session info
- **Session resume**: Sessions auto-resume on TUI restart with full scrollback history

### Keybindings

| Key | Context | Action |
|-----|---------|--------|
| `j` / `k` / `Up` / `Down` | Tree | Navigate workspaces |
| `h` / `l` / `Left` / `Right` | Tree | Collapse repo (or jump to parent) / expand repo |
| `gg` / `G` | Tree | Jump to first / last item |
| `Enter` | Tree | Select / start / resume workspace session |
| `r` | Tree | Start Claude session in selected workspace |
| `R` | Tree | Restart / resume session |
| `a` | Tree | Add repository (modal) |
| `w` | Tree | Add workspace to selected repo (modal) |
| `Ctrl+Q` | Any | Stop running session (quit if none running) |
| `Tab` | Any | Cycle focus: Tree -> Output -> Composer |
| `Shift+Tab` | Any | Reverse cycle focus |
| `Esc` | Any | Return focus to Tree / exit zoom |
| `z` | Output | Toggle zoom mode |
| `h` / `j` / `k` / `l` / arrows | Output | Move cursor |
| `gg` / `G` | Output | Jump to top / bottom |
| `Ctrl+D` / `Ctrl+U` | Output | Half page down / up |
| `PageUp` / `PageDown` | Output | Scroll output by page |
| `i` | Output | Switch focus to Composer |
| `Enter` | Composer | Send message |
| `Shift+Enter` | Composer | New line (Alt+Enter also works) |
| `Ctrl+A` / `Cmd+A` | Composer | Select all |
| `Ctrl+C` / `Cmd+C` | Composer | Copy selection |
| `Ctrl+V` / `Cmd+V` | Composer | Paste |
| `?` | Tree / Output | Toggle help overlay |
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

The state directory is resolved in this order:

1. `KOMMAND0_STATE_DIR` environment variable, if set
2. Debug builds: `.kommand0-dev/` relative to the current directory
3. Release builds: the platform data directory (`~/Library/Application Support/kommand0` on macOS)

`state.json` (repos, workspaces, sessions) lives at the root of that directory; session logs are written as JSON lines files in its `sessions/` subdirectory.
