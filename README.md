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

- **Two-pane layout**: Tree (repos/workspaces) on the left; the embedded Claude pane or workspace details on the right
- **Embedded Claude**: opening a workspace launches a real interactive `claude` in a pseudo-terminal, composited into the right pane — full fidelity (its own input box, slash commands, `/model`, colours), not a reimplemented chat UI
- **Session tabs**: a workspace can run several Claude sessions, shown as tabs across the top of the right pane (`1 2 3 … +`); switch with `Ctrl+A [`/`]` or a click, open a new one with `Ctrl+A c` or the `[+]` tab (up to 9). Each tab persists its own session and resumes on reopen
- **Session persistence**: each workspace gets a stable Claude session id, so reopening it (even after quitting kommand0) resumes the conversation via `claude --resume`; if that session was cleared from `~/.claude`, reopening starts a fresh one
- **Mouse support**: click tree items and scroll the tree; inside the embedded pane, clicks and scroll are forwarded to Claude when it requests mouse input, so its own UI is fully interactive
- **Modals**: add repos (`a`) and workspaces (`w`) directly from the TUI with path tab-completion
- **Git worktrees**: each workspace gets an isolated git worktree branch
- **Status bar**: bottom row shows the current mode (TREE / CLAUDE), the selected repo/workspace, the live-session count (and how many are active / waiting), and context key hints
- **Activity indicator**: a workspace's tree row animates its prompt into a spinner while its embedded Claude is actively producing output (debounced, so a stray keystroke doesn't flicker it)
- **Attention indicator**: when a session produces output you haven't viewed and then goes quiet, its workspace gets a magenta dot (and a "N waiting" count in the status bar) so you can tell at a glance which of your parallel sessions has come back to you. It clears as soon as you view that session, and a mid-turn pause won't flicker it — the flag latches until you look.

### Keybindings

| Key | Context | Action |
|-----|---------|--------|
| `j` / `k` / `Up` / `Down` | Tree | Navigate workspaces |
| `h` / `l` / `Left` / `Right` | Tree | Collapse repo (or jump to parent) / expand repo |
| `gg` / `G` | Tree | Jump to first / last item |
| `Enter` / `e` / `r` / `R` | Tree | Open the embedded Claude pane for the workspace |
| `x` | Tree | Close the embedded Claude pane |
| `a` | Tree | Add repository (modal) |
| `w` | Tree | Add workspace to selected repo (modal) |
| `d` / `D` | Tree | Delete / force-delete selected |
| _typing_ | Embedded | Goes straight to the embedded Claude |
| `Ctrl+A` then `c` | Embedded | New session tab |
| `Ctrl+A` then `[` / `]` | Embedded | Previous / next tab |
| `Ctrl+A` then `1`–`9` | Embedded | Jump to tab N |
| `Ctrl+A` then `x` | Embedded | Close the active tab |
| `Ctrl+A` then `t` | Embedded | Back to tree (also `Tab` / `Esc`) |
| `Ctrl+A` then `q` | Embedded | Quit kommand0 |
| `Ctrl+A` then `Ctrl+A` | Embedded | Send a literal `Ctrl+A` to Claude |
| `?` | Tree | Toggle help overlay |
| `q` | Tree | Quit |

## Test

```sh
cargo test --workspace
```

This runs three layers:

- Unit tests in `crates/core` and the TUI modules
- In-process TUI tests (`key_tests` in `apps/tui/src/main.rs`): drive `handle_key` directly and assert rendering via ratatui's `TestBackend`
- PTY end-to-end tests (`apps/tui/tests/e2e.rs`): spawn the real binary in a pseudo-terminal, send keystrokes, and assert the vt100-parsed screen. The embedded Claude pane is driven by the `embed-stub` fixture in `apps/tui/tests/fixtures/` (selected via `KOMMAND0_CLAUDE_BIN`), and each test gets an isolated state dir via `KOMMAND0_STATE_DIR`

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
