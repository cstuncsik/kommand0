# Changelog

All notable changes to kommand0 are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.1.9] - 2026-06-26

### Added

- **Command palette runs actions** — the `:` palette (previously jump-only) now
  also runs an action on the matched workspace: open a PR, clean up, archive /
  activate, start a new session, or jump straight to a specific session tab.
  Type a verb to narrow (`pr`, `clean`, `archive`, `tab 2`); Enter runs it.

## [0.1.8] - 2026-06-26

## [0.1.7] - 2026-06-26

## [0.1.6] - 2026-06-25

### Added

- **Easier install** — three new ways to get the prebuilt binaries without a
  manual tarball dance: an install script
  (`curl -fsSL https://github.com/cstuncsik/kommand0/releases/latest/download/install.sh | sh`)
  that downloads the right archive, verifies its checksum, and installs
  `kommand0` + `kmd`; a Homebrew tap (`brew install cstuncsik/tap/kommand0`),
  auto-updated on each release; and `cargo-binstall` metadata
  (`cargo binstall --git https://github.com/cstuncsik/kommand0 kommand0-tui kommand0-cli`).

## [0.1.5] - 2026-06-24

### Added

- **Attention notifications** — optionally ring the terminal bell or raise a
  desktop notification when a backgrounded session goes quiet with unseen output
  (the same "needs you" edge as the magenta tree dot). Off by default; opt in
  with `"notify": "bell" | "desktop" | "both"` in `config.json`. Fires once per
  edge (the attention latch means no repeats until you view the session and it
  comes back). Desktop uses `osascript` on macOS / `notify-send` on Linux.

## [0.1.4] - 2026-06-24

### Robustness

- **Resize-proof input** — terminal input is now read on a dedicated blocking
  thread feeding the event loop, instead of crossterm's async `EventStream`.
  This removes a class of input-wedge where a rapid burst of terminal resizes
  (SIGWINCH) could leave the async stream no longer delivering keystrokes. A new
  end-to-end test drives a realistic resize "drag" and asserts the app keeps
  rendering and still responds to input afterwards.

## [0.1.3] - 2026-06-23

### Robustness

- **Panic safety net** — a panic in the render/key/tick path now restores the
  terminal (raw mode + alt-screen) before printing, instead of stranding you on
  a scrambled screen.
- **Crash-proof state** — `state.json` is written atomically (a unique temp file
  is renamed over it), so a process crash mid-write can't leave it partially
  written; and a corrupt `state.json` no longer bricks the TUI — it's backed up
  (without clobbering an earlier backup) and reset to default with a warning,
  while the `kmd` CLI still aborts so the bad file stays recoverable.
- Workspace/session id generation no longer panics on a backwards system clock.
- **`kmd session stop` now actually stops the session** — `kmd session start`
  puts the spawned `claude` in its own process group, so `stop`'s group-kill
  reaches it (and its children) instead of missing a group that never existed.
  `session start` also honors `KOMMAND0_CLAUDE_BIN` now, matching the TUI.

## [0.1.2] - 2026-06-22

### Platforms

- **Linux support** — CI builds, lints, and runs the full test suite (including
  the PTY end-to-end tests) on Linux alongside macOS, and releases now publish a
  native `linux-x86_64` binary in addition to the macOS universal one. No
  functional source changes were needed; the code was already cross-platform.

## [0.1.1] - 2026-06-21

## [0.1.0] - 2026-06-21

First tagged release. kommand0 is a keyboard-first local orchestrator for
parallel Claude Code sessions: each workspace is a git worktree, and opening one
launches a real interactive `claude` in an embedded PTY pane. Ships two binaries
— `kommand0` (the TUI) and `kmd` (the CLI).

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
- **Go-to-workspace palette** — `:` opens a fuzzy jump list over every workspace
  (across collapsed repos); Enter jumps to the match and opens it (clearing any
  active `/` filter so the target is reachable). Rebindable (`palette`).
- **Archive/activate** — `A` toggles a workspace's active state.
- Vim-style navigation (`j`/`k`, `h`/`l`, `gg`/`G`), help overlay (`?`).
- **First-run onboarding** — an empty tree shows a centered welcome pointing at
  the in-TUI `a` (add repo) key instead of a CLI command; a freshly added repo
  auto-expands and a workspace-less repo reads `(no workspaces — press w to add)`.

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

[Unreleased]: https://github.com/cstuncsik/kommand0/compare/v0.1.9...HEAD
[0.1.9]: https://github.com/cstuncsik/kommand0/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/cstuncsik/kommand0/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/cstuncsik/kommand0/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/cstuncsik/kommand0/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/cstuncsik/kommand0/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/cstuncsik/kommand0/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/cstuncsik/kommand0/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/cstuncsik/kommand0/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/cstuncsik/kommand0/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cstuncsik/kommand0/releases/tag/v0.1.0
