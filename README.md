# kommand0

Keyboard-first local orchestrator for parallel Claude Code sessions.

Run several Claude Code sessions at once, each in its own isolated git worktree,
and drive them all from one fast TUI: a tree of repos and workspaces on the left,
the live embedded `claude` (or a workspace's git status) on the right. Open PRs,
clean up merged work, and get a nudge when a backgrounded session needs you —
without leaving the keyboard. A `kmd` CLI mirrors the core actions for scripting.

![kommand0 demo](demo/demo.gif)

> The demo is generated with [vhs](https://github.com/charmbracelet/vhs) —
> see [`demo/`](demo/) to regenerate it.

## Prerequisites

- Rust toolchain (edition 2024)
- [Claude CLI](https://docs.anthropic.com/en/docs/claude-cli) installed and authenticated
- Git on PATH
- macOS or Linux

## Build

```sh
cargo build --workspace
```

## Install

Two binaries: `kommand0` (the TUI) and `kmd` (the CLI). macOS (Apple Silicon +
Intel) and Linux x86_64 have prebuilt binaries.

**Install script** (easiest — downloads the right binaries, verifies the
checksum, installs both):

```sh
curl -fsSL https://github.com/cstuncsik/kommand0/releases/latest/download/install.sh | sh
```

Installs to `/usr/local/bin` (uses `sudo` if needed). Override with
`KOMMAND0_INSTALL_DIR=~/.local/bin` or pin a version with `KOMMAND0_VERSION=v0.1.5`.

**Homebrew**:

```sh
brew install cstuncsik/tap/kommand0
```

**cargo-binstall** (prebuilt binary, no compile):

```sh
cargo binstall --git https://github.com/cstuncsik/kommand0 kommand0-tui kommand0-cli
```

(The bare `cargo binstall kommand0` form needs a crates.io publish, which the
project doesn't do yet — hence `--git`.)

**From a release, by hand**: download the archive for your platform from the
[Releases](https://github.com/cstuncsik/kommand0/releases) page —
`kommand0-<version>-macos-universal.tar.gz` or
`kommand0-<version>-linux-x86_64.tar.gz` — then:

```sh
tar -xzf kommand0-*.tar.gz              # the archive you downloaded
sudo mv kommand0 kmd /usr/local/bin/    # or anywhere on your PATH
```

**From source** (installs into `~/.cargo/bin`):

```sh
cargo install --path apps/tui   # kommand0 (the TUI)
cargo install --path apps/cli   # kmd (the CLI)
```

## CLI Usage

```sh
# Repos
kmd repo add /path/to/your/repo
kmd repo list
kmd repo delete <name-or-path> [--force]

# Workspaces
kmd workspace create [<name>] --repo <name-or-path> [--branch <existing>]
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
kommand0              # if installed
cargo run -p kommand0-tui   # from a checkout
```

### Features

- **Two-pane layout**: Tree (repos/workspaces) on the left; the embedded Claude pane or workspace details on the right
- **Embedded Claude**: opening a workspace launches a real interactive `claude` in a pseudo-terminal, composited into the right pane — full fidelity (its own input box, slash commands, `/model`, colours), not a reimplemented chat UI
- **Session tabs**: a workspace can run several sessions, shown as tabs across the top of the right pane (`1 2 3 … +`); switch with `Ctrl+A [`/`]` or a click (up to 9). Open a new **Claude** tab with `Ctrl+A c` (or the `[+]` tab), or a **shell** tab with `Ctrl+A s` — a `$SHELL` session in the worktree, for running anything (codex, lazygit, or `tmux`/`zellij` for splits inside the pane). Shell tabs are marked `$` and are ephemeral; Claude tabs persist their session and resume on reopen
- **Session persistence**: each workspace gets a stable Claude session id, so reopening it (even after quitting kommand0) resumes the conversation via `claude --resume`; if that session was cleared from `~/.claude`, reopening starts a fresh one
- **Mouse support**: click tree items and scroll the tree; inside the embedded pane, clicks and scroll are forwarded to Claude when it requests mouse input, so its own UI is fully interactive
- **Modals**: add repos (`a`) and workspaces (`w`) directly from the TUI with path tab-completion. The add-workspace modal has an optional **Branch** field (`Tab` to switch fields) — leave it blank to fork a new branch, or enter an existing branch (local, or a remote `origin/…` ref) to check it out instead. With the Branch field blank, if the workspace **name** matches an existing branch (local or `origin`), a prompt offers to check it out instead of forking
- **Filter & archive**: press `/` to live-filter the workspace tree by name or branch (matched repos auto-expand, `Esc` clears); press `A` to archive/activate a workspace — so the tree stays navigable as you accumulate repos and workspaces
- **Git worktrees**: each workspace gets an isolated git worktree branch
- **Branch/diff status**: each workspace shows its git branch and how far it is ahead/behind its upstream plus whether it has uncommitted changes — a compact `↑2↓1*` segment in the tree row and full detail (`Branch:` / `Changes:`) in the detail pane. Computed off the render loop (never blocks keystrokes), refreshed every couple of seconds and on workspace create/close
- **Open a PR**: press `p` (or click `[Open PR]` in the detail pane) to push a workspace's branch and open a GitHub PR via the `gh` CLI. Runs off the render loop with progress (`Opening PR…`) and shows the resulting URL (or a readable error); idempotent — re-running on a branch that already has a PR returns its URL. Requires `gh` installed and authenticated
- **Clean up merged workspaces**: press `c` (or click `[Clean up]`) to remove a workspace's worktree and delete its branch once its PR is merged. Behind a confirmation, and it only proceeds when it's provably safe — the PR is `MERGED` (per `gh`), the worktree is clean, and the branch has no commits beyond what the PR merged (squash-safe); otherwise it refuses and tells you why. On success the workspace is dropped from the tree
- **Status bar**: bottom row shows the current mode (TREE / CLAUDE), the selected repo/workspace, the live-session count (and how many are active / waiting), and context key hints
- **Activity indicator**: a workspace's tree row animates its prompt into a spinner while its embedded Claude is actively producing output (debounced, so a stray keystroke doesn't flicker it)
- **Attention indicator**: when a session produces output you haven't viewed and then goes quiet, its workspace gets a magenta dot (and a "N waiting" count in the status bar) so you can tell at a glance which of your parallel sessions has come back to you. The flag is per-session and latches until you actually open that session (a mid-turn pause won't flicker it) — so a workspace stays flagged while any of its tabs has unseen output, and selecting a workspace in the tree doesn't count as viewing it (you have to open the session to clear it). Optionally ring a terminal bell or raise a desktop notification on that edge — see the `notify` config option below (off by default).

### Keybindings

| Key | Context | Action |
|-----|---------|--------|
| `j` / `k` / `Up` / `Down` | Tree | Navigate workspaces |
| `h` / `l` / `Left` / `Right` | Tree | Collapse repo (or jump to parent) / expand repo |
| `gg` / `G` | Tree | Jump to first / last item |
| `<` / `>` | Tree | Shrink / widen the tree pane (5% steps, 15–60%; this session only — set `tree_width_pct` for a persistent default) |
| `/` | Tree | Filter workspaces by name/branch (`Esc` clears) |
| `:` | Tree | Command palette: fuzzy-find a workspace (across collapsed repos) and either jump to it or run an action on it — open PR, clean up, archive/activate, new session, or jump to a session tab |
| `n` / `N` | Tree | Jump to + open the next / previous workspace that needs you (cycles the "N waiting") |
| `A` | Tree | Archive / activate the selected workspace |
| `Enter` / `e` / `r` / `R` | Tree | Open the embedded Claude pane for the workspace |
| `x` | Tree | Close the embedded Claude pane |
| `p` | Tree | Open a GitHub PR for the selected workspace (`gh`) |
| `c` | Tree | Clean up the selected merged workspace (worktree + branch) |
| `a` | Tree | Add repository (modal) |
| `w` | Tree | Add workspace to selected repo (modal) |
| `d` / `D` | Tree | Delete / force-delete selected |
| _typing_ | Embedded | Goes straight to the embedded Claude |
| `Ctrl+A` then `c` | Embedded | New Claude session tab |
| `Ctrl+A` then `s` | Embedded | New shell tab (`$SHELL` / `shell` config; ephemeral) |
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
- In-process TUI tests (`key_tests` in `apps/tui/src/main.rs`): drive `handle_key` directly and assert rendering via ratatui's `TestBackend`, including full-screen [`insta`](https://insta.rs) golden snapshots of key layouts (stored in `apps/tui/src/snapshots/`). After an intentional UI change, refresh them with `INSTA_UPDATE=always cargo test` (or `cargo insta review`) and commit the updated `.snap` files.
- PTY end-to-end tests (`apps/tui/tests/e2e.rs`): spawn the real binary in a pseudo-terminal, send keystrokes, and assert the vt100-parsed screen. The embedded Claude pane is driven by the `embed-stub` fixture in `apps/tui/tests/fixtures/` (selected via `KOMMAND0_CLAUDE_BIN`), and each test gets an isolated state dir via `KOMMAND0_STATE_DIR`

## Project Structure

- `apps/cli` - Command-line interface (`kommand0-cli`)
- `apps/tui` - Terminal UI (ratatui + crossterm + tokio)
- `crates/core` - Shared core library (models, state, git helpers, session management)

## State

The state directory is resolved in this order:

1. `KOMMAND0_STATE_DIR` environment variable, if set
2. Debug builds: `.kommand0-dev/` relative to the current directory
3. Release builds: the platform data directory (`~/Library/Application Support/kommand0` on macOS, `~/.local/share/kommand0` on Linux)

`state.json` (repos, workspaces, sessions) lives at the root of that directory; session logs are written as JSON lines files in its `sessions/` subdirectory. The app's own diagnostics (warnings/errors that can't go to the terminal while the TUI is running) are appended to `kommand0.log` there. It's append-only and not rotated — safe to delete anytime.

## Copying files into a worktree (`.worktree-copy`)

Each workspace gets a fresh git worktree, which doesn't carry over git-ignored files (local `.env`, editor configs, etc.). Drop a `.worktree-copy` file in the repo root to copy selected files into every new worktree: one glob pattern per line (`*`, `?`, `[...]`, `**`), paths relative to the repo root, with blank lines and `#` comments ignored. Each match is copied into the worktree preserving its relative path; a matched directory is copied with its whole subtree. Matching is case-sensitive (like zsh): a bare `*` skips dotfiles, while a pattern whose last segment leads with a dot (e.g. `.env*`) matches them. `**` matches any number of directories — `config/**/*` copies everything under `config/`. Symlinks are skipped, and matches that resolve outside the repo are ignored.

```
.env*
config/local.json
.vscode/**/*
```

When there's no `.worktree-copy`, kommand0 falls back to copying the repo root's `.env*` files. Copying is best-effort — any failure is logged to `kommand0.log` and never blocks worktree creation.

## Config

Optional, hand-edited `config.json` (in the state directory, or at the path in `KOMMAND0_CONFIG`). Every field is optional and a missing/invalid file falls back to defaults:

```json
{
  "claude_args": ["--model", "sonnet"],
  "claude_bin": "/usr/local/bin/claude",
  "status_refresh_secs": 2,
  "tree_width_pct": 30,
  "keybindings": { "quit": ["ctrl+q"], "open": ["o"] },
  "theme": "high-contrast",
  "theme_colors": { "accent": "blue", "attention": "#ff8800" },
  "notify": "bell",
  "shell": "zsh"
}
```

- `claude_args` — extra args appended to every embedded `claude` spawn (e.g. `--model`, `--permission-mode`). Don't put `--session-id`/`--resume` here — kommand0 manages those.
- `claude_bin` — override the `claude` binary (the `KOMMAND0_CLAUDE_BIN` env var still takes precedence).
- `status_refresh_secs` — how often the background git-status refresh runs (default 2; floored at 1).
- `tree_width_pct` — the tree (left) pane width as a percent of the terminal (default 30; clamped to 15–60). This is the persistent baseline; the live `<`/`>` keys adjust a per-session value seeded from it (and reset to it next launch). You can also drag the border between the tree and content panes with the mouse to resize it live.
- `keybindings` — rebind tree-pane actions: `"<action>": ["<key>", …]`. The listed keys **replace** that action's defaults. Key specs: a single char (`q`, `/`, case-sensitive), a named key (`Up`/`Down`/`Left`/`Right`/`Enter`/`Esc`/`Tab`/`Space`/`Delete`/`Backspace`/`Home`/`End`), with optional `ctrl+`/`alt+`/`shift+`. Actions: `move-up`, `move-down`, `collapse`, `expand`, `last`, `widen-tree`, `shrink-tree`, `activate`, `open`, `close`, `open-pr`, `cleanup`, `filter`, `palette`, `next-waiting`, `prev-waiting`, `archive`, `add-repo`, `add-workspace`, `delete`, `force-delete`, `help`, `quit`. The `gg` motion, `Esc` (clears the filter), and the embedded `Ctrl+A` prefix are fixed (not rebindable). Unknown actions, bad specs, or reusing a reserved key are warned (tree border + log), not fatal. If a rebind leaves an action with no valid keys it shows as `(unbound)` in the help overlay (`?`).
- `theme` — a built-in palette for the app chrome: `"default"` or `"high-contrast"` (the embedded `claude` pane keeps its own colours either way). Unknown names warn and fall back to default.
- `theme_colors` — per-role overrides applied on top of `theme`: `"<role>": "<color>"`. Roles: `accent`, `selected`, `active`, `attention`, `dirty`, `error`, `muted`, `text`, `inverse`. Colors: a named color (`cyan`, `light-red`, `darkgray`), an `#rrggbb` hex, a 0–255 palette index, or `reset`/`default` (the terminal's own default color — not the role's built-in). Unknown roles / unparseable colors are warned (tree border + log), not fatal.
- `notify` — alert when a backgrounded session goes quiet with unseen output (the same "needs you" edge as the magenta dot): `"off"` (default), `"bell"` (terminal bell), `"desktop"` (an OS notification — `osascript` on macOS, `notify-send` on Linux; silently skipped if unavailable), or `"both"`. Fires once per rising edge (the latch means it won't repeat until you view the session and it comes back). Unknown values warn and fall back to `off`.
- `shell` — command for a shell session tab (`Ctrl+A s`); defaults to `$SHELL`, then `/bin/sh`. Can be any command — e.g. `"tmux"` to open a tmux session (with its own splits) directly. The `KOMMAND0_SHELL` env var takes precedence.

The config is read once at startup, so edits take effect on the next launch. Any JSON error discards the whole file and is flagged in the tree border.

## Releasing

One button, from the **Actions → Release → Run workflow** menu: pick a bump
(`patch`/`minor`/`major`) or type an explicit version. The workflow runs the
test gate, bumps the workspace version, rolls `[Unreleased]` into a dated
section in `CHANGELOG.md` (via `scripts/cut-release.sh`), commits + tags on
`main`, then builds and publishes the universal macOS binary. Pushing a `v*`
tag by hand also builds + publishes (skipping the bump).
