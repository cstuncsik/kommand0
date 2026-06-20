# Changelog

All notable changes to kommand0 are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

The 0.1.0 line. kommand0 is a keyboard-first local orchestrator for parallel
Claude Code sessions: each workspace is a git worktree, and opening one launches
a real interactive `claude` in an embedded PTY pane.

### Embedded sessions

- Embedded interactive `claude` per workspace via PTY passthrough (full fidelity:
  its own input box, slash commands, colours), composited into the right pane.
- **Session tabs** — multiple Claude sessions per workspace, shown as `1 2 3 … +`
  across the top of the pane; switch with `Ctrl+A [`/`]` or a click, open with
  `Ctrl+A c` or `[+]` (up to 9). Each tab persists its own session.
- **Tab titles** — name a tab with `Ctrl+A r`; titles persist and survive an
  auto-heal.
- **Session persistence** — each session resumes via `claude --resume` on reopen,
  even after quitting; a gone/cleared session **auto-heals** into a fresh one in
  place.
- **Mouse support** — clicks and scroll are forwarded to Claude when it requests
  mouse input; tree items are clickable.
- **Resize** — every live pane (all tabs, all workspaces) tracks the content
  area, so a terminal resize reaches background panes too.

### Surfacing

- **Activity indicator** — a workspace's tree row animates into a spinner while
  its Claude is producing output (debounced).
- **Attention indicator** — a magenta dot (and a "N waiting" status count) flags
  a workspace whose session produced output you haven't viewed and then went
  quiet; latches until you open that session.
- **Status bar** — mode, selection, live/active/waiting session counts, key hints.

### Git lifecycle

- **Branch/diff status** — per-workspace branch, ahead/behind, and dirty state
  (`↑2↓1*` in the tree, full detail in the detail pane), computed off the render
  loop.
- **Open a PR** — `p` / `[Open PR]` pushes the branch and opens a GitHub PR via
  `gh`; idempotent.
- **Clean up merged workspaces** — `c` / `[Clean up]` removes a merged worktree
  and deletes its branch, only when provably safe (PR merged, worktree clean,
  branch tip == the PR's last merged commit), behind a confirmation.

### Tree & navigation

- **Filter** — `/` live-filters the tree by name/branch (matched repos
  auto-expand; Up/Down walk matches; `Esc` clears).
- **Archive/activate** — `A` toggles a workspace's active state.
- Vim-style navigation (`j`/`k`, `h`/`l`, `gg`/`G`), help overlay (`?`).

### CLI (`kmd`)

- `repo add/list/delete`, `workspace create/list/show/status/open-pr/cleanup/
  archive/activate/delete`, `session start/stop/list/clear`.

### Config

- Optional `config.json` (state dir or `KOMMAND0_CONFIG`): `claude_args`
  passthrough (e.g. `--model`), `claude_bin` override, `status_refresh_secs`. A
  present-but-invalid config degrades to defaults and is flagged in the tree
  border.
- **Keybindings** — `keybindings` rebinds tree-pane actions; the configured keys
  replace an action's defaults. Unknown actions / bad specs / reserved keys warn
  (not fatal); an action left with no keys shows `(unbound)` in `?`.
- **Theming** — `theme` selects a built-in chrome palette (`default` /
  `high-contrast`) and `theme_colors` overrides individual roles (`accent`,
  `attention`, …) with named/`#rrggbb`/indexed colors. The embedded `claude`
  pane keeps its own colours. Bad theme names / roles / colors warn, not fatal.
