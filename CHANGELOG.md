# Changelog

All notable changes to kommand0 are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed

- **Flashing green dot while producing**: a workspace row's status dot now
  pulses green in step with the activity spinner while any of its session
  tabs is producing output.
- **Smaller merged-PR glyph**: the tree row's merged indicator is now the
  1-column `●` instead of the oversized `⬤`.

## [0.17.2] - 2026-07-19

## [0.17.1] - 2026-07-19

### Fixed

- **Add Workspace now handles branches with `/` in the name** — typing an
  existing branch like `user/feature` into the Name field checks it out
  directly (the workspace gets the path-safe `user-feature` name) instead of
  failing validation with "must not contain a path separator". The Name field
  may also be left blank when the Branch field is filled; the workspace name
  is then derived from the branch the same way.

- **Docs: drag-to-select under tmux needs `set -s set-clipboard on`** — the
  0.17.0 README claimed tmux's default `set-clipboard` passes the OSC 52 copy
  through; the default (`external`) actually drops clipboard requests from
  applications inside panes, so the copy silently never landed.

## [0.17.0] - 2026-07-18

### Added

- **Drag-to-select in embedded panes** — dragging over a pane whose program
  hasn't enabled mouse reporting (a shell tab at rest, logs, build output)
  now selects the dragged cells tmux-style: content-aware reading-order
  selection over the live grid (never tree text or borders), a reversed
  highlight while dragging, and an OSC 52 clipboard copy on release (native
  in Ghostty/kitty/iTerm2/WezTerm; tmux's default `set-clipboard` passes it
  through). A program that has enabled mouse reporting (claude's UI,
  `lazygit`, …) keeps receiving every mouse event exactly as before —
  Shift+drag remains the raw-screen bypass for those.

## [0.16.0] - 2026-07-15

### Added

- **Focus events for embedded sessions** — a child that opts into terminal
  focus reporting (mode 1004, as Claude does) now receives synthesized
  `CSI I`/`CSI O` on real focus edges: the composite of the terminal
  window's own focus (kommand0 enables mode 1004 upstream), whether the
  embedded pane owns input, and whether the tab is the active one of the
  selected workspace. Edge-triggered per tab, so switching tabs or bouncing
  to the tree and back informs each session exactly once. The per-tab
  synthesis works under tmux even without `focus-events`; add
  `set -g focus-events on` to also propagate the window dimension.

## [0.15.0] - 2026-07-14

### Added

- **`{` / `}` repo jumps in the tree** — move straight to the previous / next
  repo header, skipping workspace rows (vim-style, clamped at the outermost
  repo; works on the filtered view too). Rebindable as `prev-repo` /
  `next-repo`.
- **`Ctrl+A l` — last-active tab** — the tmux-style prefix chord toggles back
  to the previously active session tab of the workspace (a closed tab simply
  no-ops). The resting embedded hint now also names `Ctrl+]` as the fast way
  back to the tree.
- **Armed `Ctrl+A` indicator** — while the tmux-style prefix is waiting for
  its second key, the status line shows `Ctrl+A …` plus the accepted keys
  instead of the resting hint; it disappears on the next keypress.

## [0.14.1] - 2026-07-13

### Fixed

- **Docs drift (v0.12–v0.14)** — README documents the profiles CLI
  (`kmd profile rename`, the global `--profile` flag), `--no-worktree`,
  the tab-rename and `Delete` keybindings, the actual `Enter` vs `e`/`r`/`R`
  split, and the full release output (macOS universal + Linux x86_64 +
  Homebrew tap); the help overlay gains the shell-tab row; `kmd profile
  rename --help` mentions the Claude session-store migration; the agent
  guide catches up on profiles, migration ordering, and test hermeticity.

## [0.14.0] - 2026-07-13

### Added

- **Shift+Enter under tmux** — kommand0 now requests xterm's modifyOtherKeys
  (mode 1) when running inside a tmux configured with `set -s extended-keys on`
  and `set -s extended-keys-format csi-u`, so Shift+Enter inserts a newline in
  embedded sessions instead of submitting (tmux then delivers it as
  `CSI 13;2u`; ordinary keys keep their classic encodings, and the request is
  reset on every exit path). With tmux's default options (`off`/`xterm`) the
  startup hint now names that exact two-line config; `Alt+Enter` remains the
  zero-config fallback.

## [0.13.1] - 2026-07-12

### Fixed

- **`kmd profile rename` now migrates Claude Code's session stores** — Claude
  keys its per-directory transcripts by a slug of the working directory
  (`~/.claude/projects/<slug>/<uuid>.jsonl`, honoring `CLAUDE_CONFIG_DIR`), so
  renaming a profile moved every worktree out from under its store and every
  embedded session resumed as a fresh conversation. The rename now moves each
  affected store dir along (best-effort, like the git worktree repair:
  collisions, overlong hashed store names, and failed moves become warnings
  naming the manual fix).
- **A missed `--resume` is no longer silent** — when Claude's store has no
  session for the stored id (e.g. a moved directory), the auto-heal still
  starts a fresh tab but now names the lost session id in the banner and the
  log, and notes that the old transcript keeps its uuid filename under
  `~/.claude/projects/`.
- **tmux newline hint** — under tmux the kitty keyboard protocol never passes
  through, making Shift+Enter indistinguishable from Enter in embedded
  sessions; kommand0 now shows a one-time startup hint pointing at
  `Alt+Enter` (which works everywhere), and the help overlay + README
  document it.

## [0.13.0] - 2026-07-12

### Added

- **`kmd profile rename <old> <new>`** — renames a profile directory and keeps
  it working: worktree and session-log paths stored in the profile's
  `state.json` are rewritten to the new location, and each moved worktree's
  git link is repaired (`git worktree repair`; a failed repair is reported as
  a warning to rerun by hand). Worktrees still at the pre-profiles data-dir
  root are left alone. Renaming `default` is allowed — the next plain run
  starts a fresh default profile. Caveat: don't rename a profile while a
  kommand0/kmd instance is running on it (nothing locks the directory).

## [0.12.0] - 2026-07-11

### Added

- **Profiles** — `kommand0 --profile <name>` / `kmd --profile <name> …` run fully
  isolated instances: each profile keeps its own `state.json`, `config.json`,
  `kommand0.log`, `sessions/`, and `worktrees/` under `<data dir>/profiles/<name>`.
  Omitting the flag uses the `default` profile. On first run a legacy `state.json`/
  `config.json` at the data-dir root moves into `profiles/default/` automatically
  (existing worktrees and session logs stay in place and keep working).
  Embedded sessions inherit the profile via `KOMMAND0_PROFILE`, so a `kmd` run
  inside a profiled session targets the same profile (an explicit `--profile`
  still wins). `KOMMAND0_STATE_DIR` still targets an exact directory and cannot
  be combined with `--profile`. Caveat: don't run a pre-profiles binary and
  this version concurrently across the migration — the old binary writes the
  old root location again (split-brain). Two new binaries racing the very
  first migration can transiently see fresh state until the files land; the
  worst case is the documented abort-and-retry, not data loss.

## [0.11.0] - 2026-07-08

### Changed

- **Workspace branches drop the `kommand0/` prefix** — a new workspace forks a
  branch named exactly after the workspace (`demo`, suffixed `demo-2`, `-3`, …
  on collision with any local or origin branch), so remote refs read
  `origin/demo` instead of the remote-lookalike `origin/kommand0/demo`.
  Existing prefixed branches keep working unchanged (status, PRs, cleanup).
  Cleanup's ownership gate is relaxed accordingly: it now refuses only the
  repo's default branch and malformed names — its merged-PR, clean-worktree,
  and tip-equality guards are unchanged — which deliberately means an adopted
  (non-kommand0) branch **can** now be cleaned up once its PR merges. Deletion
  remains local-only; the remote branch is never touched.

## [0.10.0] - 2026-07-05

## [0.9.0] - 2026-07-05

### Added

- **Settings page** — press `,` in the tree to open a full-screen editor for the
  simple `config.json` fields (`claude_args`, `claude_bin`, `shell`, `notify`,
  `theme`, `status_refresh_secs`, `tree_width_pct`). `j`/`k` select, `Enter`
  edits (blank = back to default), `Enter` saves the field, `Esc` cancels/closes.
  Saves rewrite only the edited key — unknown/hand-edited keys in the file are
  preserved — and theme / tree width / notify apply live; `claude_args`,
  `claude_bin`, and `shell` apply to the next spawned tab. `keybindings` and
  `theme_colors` remain file-only. Rebindable as `settings`.
- **Versioned `state.json`** — the state file now carries a `version` field, stamped
  on every save, with a migration seam on load. Files written by older versions (no
  `version` field) still load unchanged, so a future schema change can migrate an
  existing state instead of bricking it. Groundwork for a stable 1.0.

## [0.8.0] - 2026-07-05

### Added

- **Two-pane diff dialog** — the review-diff view (`v`) is now GitHub-style: a
  collapsible file tree on the left, the selected file's diff on the right
  (`git diff <default>...HEAD`, coloured by add / remove / hunk). `Tab` switches
  focus; in the file pane `j`/`k` move and `Enter`/`l`/`h` expand/collapse
  folders; in the diff pane `j`/`k`, `PgUp`/`PgDn`, `g`/`G` scroll; click or
  select a file. `Esc`/`v`/`q` close.
- **Open PR in a browser** — press `p` on a workspace to open its GitHub PR
  (from the cached PR/CI status) in your default browser. Rebindable as
  `open-pr-web`.

## [0.7.1] - 2026-07-04

### Added

- **PR/CI status column** — each own-branch workspace surfaces its GitHub PR at a
  glance: a compact `#12 ✓` in the tree row (`✓` checks passing · `✗` failing ·
  `◍` pending · `⬤` merged · `✕` closed) and a full `PR #12 · open · CI passing ·
  approved` line + URL in the detail pane. One read-only `gh pr list` per repo,
  off the render loop, refreshed periodically. Requires `gh` installed and authed.

### Removed

- **Open PR** — the `p` action, palette entry, `[Open PR]` detail button, and the
  `kmd workspace open-pr` CLI subcommand are gone. The diff review (`v`) and the
  new PR/CI status column replace it; create PRs from the embedded Claude or a
  shell tab (which write a real description rather than `--fill` from commits).

## [0.7.0] - 2026-07-04

### Added

- **Review a workspace's diff in-pane** — press `v` on a workspace to open a
  scrollable overlay of its PR-style diff (`git diff <default>...HEAD` — the
  committed changes a PR would show), coloured by add / remove / hunk. `j`/`k`,
  `PgUp`/`PgDn`, and `g`/`G` scroll; `Esc`/`v`/`q` close. Rebindable as
  `review-diff`.

## [0.6.1] - 2026-07-04

### Added

- **Click to focus a pane** — clicking the tree or the content pane now focuses
  it, mirroring the keyboard. A click in the content pane focuses the embedded
  session and passes through to Claude; a click anywhere in the tree pane —
  including the empty space below the rows — focuses the tree, so a
  one-repo/one-workspace layout is no longer keyboard-only for getting back out
  of Claude.

## [0.6.0] - 2026-07-03

### Added

- **Dimmed text in the embedded pane** — faint output (SGR 2), such as Claude
  Code's ghosted input suggestions, now renders dimmed instead of solid white,
  so a suggestion no longer reads like text you already typed.

### Changed

- **Terminal stack upgraded** — ratatui 0.29 → 0.30, vt100 0.15 → 0.16 (which is
  what unlocks the dim attribute above), and crossterm 0.28 → 0.29 to match
  ratatui's default backend. The minimum supported Rust version is now **1.88**,
  declared via `rust-version`.
- **Supply-chain guard** — CI now runs `cargo audit` against the RustSec
  advisory database on every push and PR, and `anyhow` was bumped to 1.0.103 to
  clear RUSTSEC-2026-0190.

## [0.5.1] - 2026-07-03

### Changed

- **Calmer activity indicators** — the "working" spinner now rides through a
  Claude session's bursty output (a ~2s window) instead of flickering on every
  sub-second pause, and a workspace is only flagged idle / "needs you" after it
  has been genuinely quiet for ~3s. Feel over accuracy: fewer rapid state flips,
  a clearer "is this actually idle" read.

## [0.5.0] - 2026-07-03

### Added

- **Add-workspace branch detection** — when adding a workspace by name with the
  Branch field blank, if a git branch with that exact name already exists (local
  or `origin`), a prompt offers to check it out instead of silently forking a new
  `kommand0/<name>` branch. An explicitly-filled Branch field is unchanged. The
  `kmd workspace create <name>` CLI has the same detection: on a terminal it
  prompts to check the branch out, non-interactively it forks as before and notes
  it on stderr; a new `--fork` flag forces a new branch (`--branch` still checks
  one out explicitly).

### Fixed

- **Paste into TUI text inputs** — bracketed paste now works in the add-repo and
  add-workspace modals, the command palette, and the workspace filter. Previously
  a terminal paste was only forwarded to the embedded Claude pane and silently
  dropped in every other input.
- **`?` opens help on Kitty-protocol terminals** — on Ghostty, Kitty, WezTerm,
  foot, and recent iTerm2, pressing `?` now opens the help overlay instead of
  typing `/` into the tree filter.
- **No false activity spinner for an idle foreground shell** — an interactive
  foreground program in a shell tab (nvim, `less`, a pager, `htop`) no longer
  holds the activity indicator busy indefinitely.

## [0.4.0] - 2026-07-03

### Added

- **Resizable tree pane** — adjust the left tree pane's width with `<` (shrink)
  and `>` (widen) in 5% steps, clamped to 15–60%, or drag the border between the
  tree and content panes with the mouse. The live value is per-session
  (resets next launch); set a persistent default with the new `tree_width_pct`
  config knob (default 30, clamped to the same range). Both keys are rebindable
  (`shrink-tree` / `widen-tree`).

## [0.3.0] - 2026-06-29

### Added

- **`.worktree-copy` manifest** — copy files from the repo root into each new
  workspace worktree (ports the personal `wt` shell helper). Put one glob per
  line in `<repo>/.worktree-copy` (`#` comments and blank lines ignored, paths
  relative to the repo root); every match is copied into the worktree preserving
  its relative path. With no manifest it falls back to copying `.env*` — the
  usual case of carrying local env files across worktrees. Best-effort: copy
  failures are logged, never block worktree creation.

## [0.2.3] - 2026-06-28

## [0.2.2] - 2026-06-28

### Added

- **Shell session tabs** — open a `$SHELL` tab in a workspace's worktree with
  `Ctrl+A s` (alongside Claude tabs via `Ctrl+A c`). Run anything in it — codex,
  lazygit, a REPL, or `tmux`/`zellij` for splits inside the pane. Shell tabs are
  marked `$` in the tab strip and are ephemeral (a fresh process, not resumed);
  Claude tabs still persist + resume. Set the command with the `shell` config
  (default `$SHELL`, then `/bin/sh`); `KOMMAND0_SHELL` overrides.

## [0.2.1] - 2026-06-27

### Added

- **Jump to the next waiting workspace** — press `n` (or `N` for the previous) in
  the tree to jump to and open the next workspace flagged "needs you" (the
  magenta dot / "N waiting"), wrapping. Pairs with attention notifications: hear
  the bell, press `n`, land on the session that came back. Rebindable as
  `next-waiting` / `prev-waiting`.

## [0.2.0] - 2026-06-27

First minor release — a maturity marker that promotes the stabilized 0.1.x line.
No functional changes since 0.1.12; the dated sections below have the detail.
Highlights of the 0.1.x arc:

- **Parallel Claude sessions** in embedded PTYs, each a git-worktree workspace
  with persistent, resumable session tabs.
- **Cross-platform** — macOS (universal) and Linux x86_64 binaries, built and
  published by a one-button release workflow.
- **Three ways to install** — `curl | sh`, a Homebrew tap
  (`brew install cstuncsik/tap/kommand0`), and `cargo-binstall`.
- **Command palette** (`:`) — fuzzy-jump to any workspace, or run an action on
  it (open PR, clean up, archive, new session, jump to a session tab).
- **Attention notifications** — terminal bell or desktop notification when a
  backgrounded session goes quiet with unseen output.
- **Workspace from a branch** — adopt an existing local or remote branch (CLI
  `--branch` and the add-workspace modal) to review a teammate's PR with Claude.
- **Theming and rebindable keys**, **PR open** / **merged-workspace cleanup**
  via `gh`, and live **branch/diff status** computed off the render loop.
- **Hardened** — panic-safe terminal restore, atomic crash-proof state with a
  3-way merge against concurrent `kmd` writes, resize-proof input, and a full
  audit pass.

## [0.1.12] - 2026-06-27

### Added

- **Workspace from an existing branch** — check out an existing branch into the
  workspace's worktree instead of forking a fresh `kommand0/<name>` one. In the
  CLI: `kmd workspace create --repo <r> --branch <ref>`; in the TUI: the
  add-workspace modal (`w`) has an optional **Branch** field (`Tab` to switch
  fields). `<ref>` may be a local branch or a remote `origin/…` ref (a local
  tracking branch is created). Handy for reviewing a teammate's PR branch with
  Claude. The workspace name defaults to a path-safe form of the branch, and the
  adopted branch isn't `kommand0/`-prefixed, so `cleanup` won't delete it.

## [0.1.11] - 2026-06-27

### Fixed

- **Concurrent `kmd` changes aren't clobbered by the TUI** — before each save the
  TUI re-reads `state.json` and 3-way merges its in-memory state over it
  (relative to what it loaded), so a repo/workspace/session a `kmd` command added
  while the TUI was open survives the TUI's next save (and a TUI delete still
  sticks). CLI-added rows show up in the TUI on its next launch. (A CLI *in-place
  edit* to a row the TUI also holds is still overwritten — only adds/deletes are
  protected.)
- **Quitting with several sessions no longer freezes the UI** — embedded panes
  are torn down in one shared grace period (broadcast SIGHUP → wait once →
  SIGKILL stragglers) instead of a full ~250ms wait per pane.
- **Detail-pane buttons click reliably on narrow panes** — the detail text no
  longer wraps, so a clickable `[Open PR]` / `[Clean up]` button can't drift
  below its hit region.
- **Ids are unique across processes** — `generate_id` mixes in the process id, so
  the CLI and a running TUI can't mint the same id in the same millisecond.

## [0.1.10] - 2026-06-27

### Fixed

- **Mouse clicks select the right workspace once the tree scrolls** — a tree
  click ignored the list scroll offset, so on a long tree it selected a
  workspace several rows off (and disagreed with the icon click targets, which
  already accounted for the offset).
- **Selection no longer strands on a hint row after cleanup/delete** — those
  paths now re-seat the selection and re-sync the active session like the
  keyboard delete paths, instead of a raw clamp that could land on a
  non-selectable hint row.
- **Unsafe workspace names are rejected** — a name with a path separator,
  `.`/`..`, a leading dash, or that's empty is refused at creation instead of
  escaping the worktrees directory.
- **The `Ctrl+A` prefix no longer leaks across a mouse-leave** — clicking out of
  the embedded pane clears a half-typed prefix so the next keystroke isn't
  misread as a tab command.

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

[Unreleased]: https://github.com/cstuncsik/kommand0/compare/v0.17.2...HEAD
[0.17.2]: https://github.com/cstuncsik/kommand0/compare/v0.17.1...v0.17.2
[0.17.1]: https://github.com/cstuncsik/kommand0/compare/v0.17.0...v0.17.1
[0.17.0]: https://github.com/cstuncsik/kommand0/compare/v0.16.0...v0.17.0
[0.16.0]: https://github.com/cstuncsik/kommand0/compare/v0.15.0...v0.16.0
[0.15.0]: https://github.com/cstuncsik/kommand0/compare/v0.14.1...v0.15.0
[0.14.1]: https://github.com/cstuncsik/kommand0/compare/v0.14.0...v0.14.1
[0.14.0]: https://github.com/cstuncsik/kommand0/compare/v0.13.1...v0.14.0
[0.13.1]: https://github.com/cstuncsik/kommand0/compare/v0.13.0...v0.13.1
[0.13.0]: https://github.com/cstuncsik/kommand0/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/cstuncsik/kommand0/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/cstuncsik/kommand0/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/cstuncsik/kommand0/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/cstuncsik/kommand0/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/cstuncsik/kommand0/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/cstuncsik/kommand0/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/cstuncsik/kommand0/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/cstuncsik/kommand0/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/cstuncsik/kommand0/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/cstuncsik/kommand0/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/cstuncsik/kommand0/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/cstuncsik/kommand0/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/cstuncsik/kommand0/compare/v0.2.3...v0.3.0
[0.2.3]: https://github.com/cstuncsik/kommand0/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/cstuncsik/kommand0/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/cstuncsik/kommand0/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/cstuncsik/kommand0/compare/v0.1.12...v0.2.0
[0.1.12]: https://github.com/cstuncsik/kommand0/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/cstuncsik/kommand0/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/cstuncsik/kommand0/compare/v0.1.9...v0.1.10
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
