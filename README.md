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
kmd workspace list [--all] [--repo <name>]
kmd workspace show <name>
kmd workspace status [<name>]          # git branch / ahead-behind / dirty
kmd workspace open-pr <name>           # push branch + open a GitHub PR (gh)
kmd workspace cleanup <name> [--force] # remove a merged worktree + branch
kmd workspace archive <name>
kmd workspace activate <name>
kmd workspace delete <name> [--force]

# Sessions
kmd session start <workspace>
kmd session stop <workspace>
kmd session list [--workspace <name>]
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
- **Filter & archive**: press `/` to live-filter the workspace tree by name or branch (matched repos auto-expand, `Esc` clears); press `A` to archive/activate a workspace — so the tree stays navigable as you accumulate repos and workspaces
- **Git worktrees**: each workspace gets an isolated git worktree branch
- **Branch/diff status**: each workspace shows its git branch and how far it is ahead/behind its upstream plus whether it has uncommitted changes — a compact `↑2↓1*` segment in the tree row and full detail (`Branch:` / `Changes:`) in the detail pane. Computed off the render loop (never blocks keystrokes), refreshed every couple of seconds and on workspace create/close
- **Open a PR**: press `p` (or click `[Open PR]` in the detail pane) to push a workspace's branch and open a GitHub PR via the `gh` CLI. Runs off the render loop with progress (`Opening PR…`) and shows the resulting URL (or a readable error); idempotent — re-running on a branch that already has a PR returns its URL. Requires `gh` installed and authenticated
- **Clean up merged workspaces**: press `c` (or click `[Clean up]`) to remove a workspace's worktree and delete its branch once its PR is merged. Behind a confirmation, and it only proceeds when it's provably safe — the PR is `MERGED` (per `gh`), the worktree is clean, and the branch has no commits beyond what the PR merged (squash-safe); otherwise it refuses and tells you why. On success the workspace is dropped from the tree
- **Status bar**: bottom row shows the current mode (TREE / CLAUDE), the selected repo/workspace, the live-session count (and how many are active / waiting), and context key hints
- **Activity indicator**: a workspace's tree row animates its prompt into a spinner while its embedded Claude is actively producing output (debounced, so a stray keystroke doesn't flicker it)
- **Attention indicator**: when a session produces output you haven't viewed and then goes quiet, its workspace gets a magenta dot (and a "N waiting" count in the status bar) so you can tell at a glance which of your parallel sessions has come back to you. The flag is per-session and latches until you actually open that session (a mid-turn pause won't flicker it) — so a workspace stays flagged while any of its tabs has unseen output, and selecting a workspace in the tree doesn't count as viewing it (you have to open the session to clear it).

### Keybindings

| Key | Context | Action |
|-----|---------|--------|
| `j` / `k` / `Up` / `Down` | Tree | Navigate workspaces |
| `h` / `l` / `Left` / `Right` | Tree | Collapse repo (or jump to parent) / expand repo |
| `gg` / `G` | Tree | Jump to first / last item |
| `/` | Tree | Filter workspaces by name/branch (`Esc` clears) |
| `A` | Tree | Archive / activate the selected workspace |
| `Enter` / `e` / `r` / `R` | Tree | Open the embedded Claude pane for the workspace |
| `x` | Tree | Close the embedded Claude pane |
| `p` | Tree | Open a GitHub PR for the selected workspace (`gh`) |
| `c` | Tree | Clean up the selected merged workspace (worktree + branch) |
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

`state.json` (repos, workspaces, sessions) lives at the root of that directory; session logs are written as JSON lines files in its `sessions/` subdirectory. The app's own diagnostics (warnings/errors that can't go to the terminal while the TUI is running) are appended to `kommand0.log` there.

## Config

Optional, hand-edited `config.json` (in the state directory, or at the path in `KOMMAND0_CONFIG`). Every field is optional and a missing/invalid file falls back to defaults:

```json
{
  "claude_args": ["--model", "sonnet"],
  "claude_bin": "/usr/local/bin/claude",
  "status_refresh_secs": 2
}
```

- `claude_args` — extra args appended to every embedded `claude` spawn (e.g. `--model`, `--permission-mode`). Don't put `--session-id`/`--resume` here — kommand0 manages those.
- `claude_bin` — override the `claude` binary (the `KOMMAND0_CLAUDE_BIN` env var still takes precedence).
- `status_refresh_secs` — how often the background git-status refresh runs (default 2; floored at 1).

The config is read once at startup, so edits take effect on the next launch. Any JSON error discards the whole file and is flagged in the tree border.
