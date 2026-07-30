# kommand0

Keyboard-first local orchestrator for parallel Claude Code sessions.

Run several Claude Code sessions at once, each in its own isolated git worktree,
and drive them all from one fast TUI: a tree of repos and workspaces on the left,
the live embedded `claude` (or a workspace's git status) on the right. See each
branch's PR and CI status at a glance, clean up merged work, and get a nudge when
a backgrounded session needs you —
without leaving the keyboard. A `kmd` CLI mirrors the core actions for scripting.

![kommand0 demo](demo/demo.gif)

> The demo is generated with [vhs](https://github.com/charmbracelet/vhs) —
> see [`demo/`](demo/) to regenerate it.

## Prerequisites

- Rust toolchain (edition 2024)
- [Claude Code CLI](https://code.claude.com/docs) installed and authenticated
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
kmd workspace create [<name>] --repo <name-or-path> [--branch <existing>] [--fork] [--no-worktree]
kmd workspace list [--all] [--repo <name>]
kmd workspace show <name>
kmd workspace status [<name>]          # git branch / ahead-behind / dirty
kmd workspace cleanup <name> [--force] # remove a merged worktree + branch
kmd workspace archive <name>
kmd workspace activate <name>
kmd workspace delete <name> [--force]

# Sessions
kmd session start <workspace>
kmd session stop <workspace>
kmd session list [--workspace <name>]
kmd session clear <workspace>

# Profiles (every kmd command also takes the global --profile <name> flag)
kmd profile rename <old> <new>
```

`workspace create <name>` forks a branch named `<name>` (suffixed `<name>-2`,
`-3`, … when that branch already exists). Without `--branch` it detects an
existing branch first: if a branch matching `<name>` already exists (local or
`origin`), on a terminal it prompts to check it out instead of forking;
non-interactively (piped/CI) it forks the suffixed branch and notes the actual
name on stderr. Pass `--fork` to force a new branch, or `--branch <name>` to
check one out explicitly. `--no-worktree` skips the worktree entirely and uses
the repo root as the working directory (can't be combined with `--branch` or
`--fork`).

> Replace `kmd` with `cargo run -p kommand0-cli --` during development.

## TUI

```sh
kommand0              # if installed
cargo run -p kommand0-tui   # from a checkout
```

### Features

- **Two-pane layout**: Tree (repos/workspaces) on the left; the embedded Claude pane or workspace details on the right
- **Embedded Claude**: opening a workspace launches a real interactive `claude` in a pseudo-terminal, composited into the right pane — full fidelity (its own input box, slash commands, `/model`, colours), not a reimplemented chat UI
- **Session tabs**: a workspace can run several sessions, shown as tabs across the top of the right pane (`1 2 3 … +`); switch with `Ctrl+A [`/`]` or a click (up to 9), and `Ctrl+A l` toggles back to the last-active tab (tmux-style). Open a new **Claude Code** tab with `Ctrl+A c` (or the `[+]` tab), a **codex** tab with `Ctrl+A e`, a **gemini** tab with `Ctrl+A g`, an **opencode** tab with `Ctrl+A o`, or a **shell** tab with `Ctrl+A s`: a `$SHELL` session in the worktree, for running anything (lazygit, or `tmux`/`zellij` for splits inside the pane). Tabs are marked by kind (codex `>`, gemini `✦`, opencode `○`, shell `$`). All four agent tabs resume their conversation on reopen; shell tabs reopen as fresh shells
- **Session persistence**: each workspace gets a stable Claude session id, so reopening it (even after quitting kommand0) resumes the conversation via `claude --resume`; if that session was cleared from `~/.claude`, reopening starts a fresh one
- **Mouse support**: click tree items and scroll the tree; inside the embedded pane, clicks and scroll are forwarded to Claude when it requests mouse input, so its own UI is fully interactive. Horizontal scroll (tilt wheel) or Shift+scroll over the content pane switches session tabs
- **Modals**: add repos (`a`) and workspaces (`w`) directly from the TUI with path tab-completion. The add-workspace modal has an optional **Branch** field (`Tab` to switch fields) — leave it blank to fork a new branch, or enter an existing branch (local, or a remote `origin/…` ref) to check it out instead. With the Branch field blank, if the workspace **name** matches an existing branch (local or `origin`), a prompt offers to check it out instead of forking
- **Filter & archive**: press `/` to live-filter the workspace tree by name or branch (matched repos auto-expand, `Esc` clears); press `A` to archive/activate a workspace — so the tree stays navigable as you accumulate repos and workspaces
- **Git worktrees**: each workspace gets an isolated git worktree branch
- **Branch/diff status**: each workspace shows its git branch and how far it is ahead/behind its upstream plus whether it has uncommitted changes — a compact `↑2↓1*` segment in the tree row and full detail (`Branch:` / `Changes:`) in the detail pane. Computed off the render loop (never blocks keystrokes), refreshed every couple of seconds and on workspace create/close
- **PR/CI status**: each own-branch workspace surfaces its GitHub PR at a glance — a compact `#12 ✓` in the tree row (`✓` checks passing · `✗` failing · `◍` pending · `⬤` merged · `✕` closed) and a full `PR #12 · open · CI passing · approved` line + URL in the detail pane. One read-only `gh pr list` per repo, off the render loop, refreshed periodically. Requires `gh` installed and authenticated; nothing shows without it. Press `p` to open the PR in your browser
- **Review a workspace's diff**: press `v` on a workspace to open a two-pane dialog (GitHub-style) — a file tree with collapsible folders on the left, the selected file's diff on the right (`git diff <default>...HEAD`, the committed changes a PR would show, coloured by add / remove / hunk). `Tab` switches focus between the panes; click or select a file. In the file pane `j`/`k` move, `Enter`/`l`/`h` expand/collapse folders; in the diff pane `j`/`k`, `PgUp`/`PgDn`, `g`/`G` scroll; `Esc`/`v`/`q` close. Rebindable as `review-diff`
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
| `{` / `}` | Tree | Jump to the previous / next repo header (skips workspace rows) |
| `<` / `>` | Tree | Shrink / widen the tree pane (5% steps, 15–60%; this session only — set `tree_width_pct` for a persistent default) |
| `/` | Tree | Filter workspaces by name/branch (`Esc` clears) |
| `:` | Tree | Command palette: fuzzy-find a workspace (across collapsed repos) and either jump to it or run an action on it — clean up, archive/activate, new session, or jump to a session tab |
| `n` / `N` | Tree | Jump to + open the next / previous workspace that needs you (cycles the "N waiting") |
| `A` | Tree | Archive / activate the selected workspace |
| `Enter` | Tree | Activate the selection: open the workspace / expand the repo |
| `e` / `r` / `R` | Tree | Open the embedded Claude pane for the workspace |
| `x` / `Delete` | Tree | Close the embedded Claude pane |
| `v` | Tree | Review the workspace's diff (two-pane: file tree + selected file's diff; `Tab` switches focus) |
| `p` | Tree | Open the workspace's PR in a browser |
| `c` | Tree | Clean up the selected merged workspace (worktree + branch) |
| `a` | Tree | Add repository (modal) |
| `w` | Tree | Add workspace to selected repo (modal) |
| `d` / `D` | Tree | Delete / force-delete selected |
| _typing_ | Embedded | Goes straight to the embedded Claude |
| `Ctrl+A` then `c` | Embedded | New Claude Code session tab |
| `Ctrl+A` then `e` | Embedded | New codex session tab (marked `>`) |
| `Ctrl+A` then `g` | Embedded | New gemini session tab (marked `✦`) |
| `Ctrl+A` then `o` | Embedded | New opencode session tab (marked `○`) |
| `Ctrl+A` then `s` | Embedded | New shell tab (`$SHELL` / `shell` config; reopens fresh) |
| `Ctrl+A` then `[` / `]` | Embedded | Previous / next tab |
| `Ctrl+A` then `1`–`9` | Embedded | Jump to tab N |
| `Ctrl+A` then `l` | Embedded | Jump to the last-active tab |
| `Ctrl+A` then `r` | Embedded | Rename the active tab |
| `Ctrl+A` then `x` | Embedded | Close the active tab |
| `Ctrl+A` then `d` | Embedded | Detach: kill the panes (interrupts a running turn), sessions stay resumable |
| `Ctrl+A` then `t` | Embedded | Back to tree (also `Tab` / `Esc`) |
| `Ctrl+A` then `q` | Embedded | Quit kommand0 |
| `Ctrl+A` then `Ctrl+A` | Embedded | Send a literal `Ctrl+A` to the embedded tool |
| `,` | Tree | Settings page: edit the simple `config.json` fields in-app (`j`/`k` select, `Enter` edit/save, blank = default, `Esc` close) |
| `?` | Tree | Toggle help overlay |
| `q` | Tree | Quit |

## Terminals & tmux

Shift+Enter (a newline in an embedded session, without submitting) needs a terminal that reports modified keys distinctly. Outside tmux, kommand0 uses the kitty keyboard protocol. tmux never passes that protocol through, but it can deliver the same keys via xterm's modifyOtherKeys — kommand0 requests it automatically when tmux is configured for CSI u delivery. Add these two lines to `tmux.conf` and restart tmux:

```
set -s extended-keys on
set -s extended-keys-format csi-u
```

Without them (tmux defaults are `off`/`xterm`), Shift+Enter stays byte-identical to Enter under tmux and submits; kommand0 then shows a one-time startup hint with exactly this config. `Alt+Enter` remains the zero-config newline everywhere (as does Claude's own `\` + Enter).

kommand0 also synthesizes terminal **focus events** for embedded sessions: a child that opts into focus reporting (mode 1004 — Claude does) receives `CSI I`/`CSI O` as it becomes or stops being the active tab of the focused pane, so it always knows whether you're actually looking at it. This per-tab synthesis works even when the outer terminal never reports focus; to also propagate real window focus through tmux, add `set -g focus-events on` to `tmux.conf`.

**Copying text** from an embedded pane: just **drag over the pane** — when the embedded program isn't itself capturing the mouse (a shell tab at rest, build output, logs), kommand0 selects the dragged cells (tmux-style: content-aware, never tree text or borders), highlights them, and copies on release via OSC 52, which Ghostty, kitty, iTerm2, WezTerm et al. put straight on the system clipboard. Under tmux, add `set -s set-clipboard on` to `tmux.conf` — the default (`external`) forwards only tmux's own copies and silently drops OSC 52 coming from applications inside panes. A program that *has* enabled mouse reporting (claude's UI, `lazygit`, …) keeps receiving every mouse event exactly as before — for those, hold **Shift while dragging** to bypass everything and use the terminal's native selection (most terminals also offer `Option`/`Alt`+drag for block selection); that path selects the literal screen, so it can pick up tree text and borders.

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

1. `KOMMAND0_STATE_DIR` environment variable, if set — an exact directory, no profile layer (and it can't be combined with `--profile`)
2. Debug builds: `.kommand0-dev/profiles/<profile>` relative to the current directory
3. Release builds: `profiles/<profile>` under the platform data directory (`~/Library/Application Support/kommand0` on macOS, `~/.local/share/kommand0` on Linux)

`<profile>` is the `--profile <name>` value — both binaries take the flag (`kommand0 --profile work`, `kmd --profile work repo list`) — and defaults to `default`. Each profile is a fully isolated instance with its own `state.json`, `config.json`, `kommand0.log`, `sessions/`, and `worktrees/`; the TUI shows a non-default profile in the tree title. For a non-default profile the TUI also exports `KOMMAND0_PROFILE` into its embedded sessions, so a nested `kmd` (or `kommand0`) targets the same profile; an explicit `--profile` beats the variable, and `KOMMAND0_STATE_DIR` still wins silently over it. On first run, a pre-profiles layout (`state.json`/`config.json` at the data-dir root) migrates into `profiles/default/` automatically; existing worktrees and session logs stay where they are and keep working. Rename a profile with `kmd profile rename <old> <new>` — it rewrites the profile's stored worktree/session paths, repairs the git worktree links, and moves each worktree's Claude Code session store (`~/.claude/projects/<cwd-slug>`, honoring `CLAUDE_CONFIG_DIR`) so embedded sessions keep resuming; don't run it while an instance is using that profile.

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
  "codex_args": ["--model", "o3"],
  "codex_bin": "/usr/local/bin/codex",
  "gemini_args": ["--model", "gemini-2.5-pro"],
  "gemini_bin": "/usr/local/bin/gemini",
  "opencode_args": ["--model", "anthropic/claude-sonnet-4-5"],
  "opencode_bin": "/usr/local/bin/opencode",
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
- `keybindings` — rebind tree-pane actions: `"<action>": ["<key>", …]`. The listed keys **replace** that action's defaults. Key specs: a single char (`q`, `/`, case-sensitive), a named key (`Up`/`Down`/`Left`/`Right`/`Enter`/`Esc`/`Tab`/`Space`/`Delete`/`Backspace`/`Home`/`End`), with optional `ctrl+`/`alt+`/`shift+`. Actions: `move-up`, `move-down`, `collapse`, `expand`, `last`, `widen-tree`, `shrink-tree`, `activate`, `open`, `close`, `review-diff`, `open-pr-web`, `cleanup`, `filter`, `palette`, `next-waiting`, `prev-waiting`, `archive`, `add-repo`, `add-workspace`, `delete`, `force-delete`, `help`, `settings`, `quit`. The `gg` motion, `Esc` (clears the filter), and the embedded `Ctrl+A` prefix are fixed (not rebindable). Unknown actions, bad specs, or reusing a reserved key are warned (tree border + log), not fatal. If a rebind leaves an action with no valid keys it shows as `(unbound)` in the help overlay (`?`).
- `theme` — a built-in palette for the app chrome: `"default"` or `"high-contrast"` (the embedded `claude` pane keeps its own colours either way). Unknown names warn and fall back to default.
- `theme_colors` — per-role overrides applied on top of `theme`: `"<role>": "<color>"`. Roles: `accent`, `selected`, `active`, `attention`, `dirty`, `error`, `muted`, `text`, `inverse`. Colors: a named color (`cyan`, `light-red`, `darkgray`), an `#rrggbb` hex, a 0–255 palette index, or `reset`/`default` (the terminal's own default color — not the role's built-in). Unknown roles / unparseable colors are warned (tree border + log), not fatal.
- `notify` — alert when a backgrounded session goes quiet with unseen output (the same "needs you" edge as the magenta dot): `"off"` (default), `"bell"` (terminal bell), `"desktop"` (an OS notification — `osascript` on macOS, `notify-send` on Linux; silently skipped if unavailable), or `"both"`. Fires once per rising edge (the latch means it won't repeat until you view the session and it comes back). Unknown values warn and fall back to `off`.
- `shell` — command for a shell session tab (`Ctrl+A s`); defaults to `$SHELL`, then `/bin/sh`. Can be any command — e.g. `"tmux"` to open a tmux session (with its own splits) directly. The `KOMMAND0_SHELL` env var takes precedence.
- `codex_bin`/`codex_args`, `gemini_bin`/`gemini_args`, `opencode_bin`/`opencode_args`: the same binary-override + extra-args pair for the codex (`Ctrl+A e`), gemini (`Ctrl+A g`) and opencode (`Ctrl+A o`) session tabs (also in the command palette). The `KOMMAND0_CODEX_BIN`/`KOMMAND0_GEMINI_BIN`/`KOMMAND0_OPENCODE_BIN` env vars take precedence over the config bins. Gemini tabs resume their conversation when a workspace reopens (kommand0 manages `--session-id`/`--resume`, so don't put those in `gemini_args`); codex and opencode tabs capture the session id their CLI prints when a session closes and resume it on reopen. Quitting kommand0 (or detaching a workspace) terminates codex/opencode tabs gracefully and captures the id opencode prints on SIGTERM, and codex session ids are also captured from codex's own session store right after the tab starts, so a codex tab still open at quit (or lost to a crash) resumes too; anything uncaptured reopens fresh.

The config is read once at startup, so hand-edits take effect on the next launch. Any JSON error discards the whole file and is flagged in the tree border. The simple fields above (everything except `keybindings` and `theme_colors`) can also be edited in-app on the settings page (`,`): each save rewrites only that key — preserving any hand-edited or unknown keys — and `theme`, `tree_width_pct`, and `notify` apply immediately, while `shell` and the per-tool `*_bin`/`*_args` fields apply to the next tab you open.

## Troubleshooting

**No PR/CI status in the tree.** PR status needs the [GitHub CLI](https://cli.github.com):
`gh` installed and authenticated (`gh auth status` to check, `gh auth login` to fix).
Without it the tree simply omits the PR segment, nothing else breaks.

**The embedded pane won't open (or exits immediately).** The `claude` binary
isn't on PATH, or the one found isn't the Claude Code CLI. Install it, or point
`claude_bin` in `config.json` (or the `KOMMAND0_CLAUDE_BIN` env var) at the right
binary. `kommand0.log` in the state directory has the spawn error.

**Shift+Enter submits instead of inserting a newline.** You're under tmux without
extended keys, see [Terminals & tmux](#terminals--tmux) for the two-line
`tmux.conf` fix. `Alt+Enter` inserts a newline everywhere, no config needed.

**Drag-copying from an embedded pane never reaches the clipboard under tmux.**
tmux drops OSC 52 from applications by default; add `set -s set-clipboard on` to
`tmux.conf` (details in [Terminals & tmux](#terminals--tmux)).

**The tree is empty but you had repos.** You're looking at a different state
directory: another `--profile`, an inherited `KOMMAND0_PROFILE`/`KOMMAND0_STATE_DIR`,
or a debug build (`cargo run`), which keeps its state in `.kommand0-dev/` relative
to the current directory instead of the platform data dir. See [State](#state).

**Reporting a bug?** Include `kommand0 --version` and the tail of `kommand0.log`
from the state directory (see [State](#state) for where that is).

## Releasing

One button, from the **Actions → Release → Run workflow** menu: pick a bump
(`patch`/`minor`/`major`) or type an explicit version. The workflow runs the
test gate, bumps the workspace version, rolls `[Unreleased]` into a dated
section in `CHANGELOG.md` (via `scripts/cut-release.sh`), commits + tags on
`main`, then builds the universal macOS and Linux x86_64 binaries, publishes
the release, and updates the Homebrew tap. Pushing a `v*` tag by hand also
builds + publishes (skipping the bump).
