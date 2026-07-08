# kommand0 — agent guide

Keyboard-first TUI that orchestrates parallel Claude Code sessions, each in its own
git worktree. A `kmd` CLI mirrors the core actions.

## Layout (Rust workspace · edition 2024 · MSRV 1.88)
- `crates/core` (`kommand0-core`) — all domain logic: git plumbing (`git.rs`),
  worktrees, workspace/session/repo state, config. No TUI/CLI deps. Functions that
  shell out (git/gh) are **panic-free** (return `Option`/empty on failure) and safe
  to call off the UI thread.
- `apps/tui` (`kommand0` binary) — the ratatui TUI. Keep it **thin**: presentation
  + input, not domain logic. Modules: render, pane (embedded PTY via vt100), diff,
  palette, modal, help, keymap, mouse, buttons, notify, theme.
- `apps/cli` (`kmd` binary) — the clap CLI over the same core.

## Commands
- Build: `cargo build --workspace`
- **CI gates (must pass):** `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace`. A `cargo audit` job also runs. There is **no fmt check**.
- Dev TUI: `cargo run -p kommand0-tui`
- Release: `gh workflow run release.yml -f level=patch|minor|major` — bumps the
  version, rolls the CHANGELOG, tags, builds macOS+Linux, publishes the release,
  updates the Homebrew tap.

## Commits, PRs & versioning
- **Conventional Commits.** `type(scope): summary` — types: `feat`, `fix`, `docs`,
  `refactor`, `test`, `perf`, `chore`, `ci`; scope is the crate/area (`tui`, `core`,
  `cli`, `release`). Flag a breaking change with `!` (`feat(cli)!: …`) or a
  `BREAKING CHANGE:` footer. End commit messages with the
  `Co-Authored-By: Claude …` trailer.
- **PR titles** follow the same convention — a single `type(scope): summary` line
  for the primary change; the body carries the detail.
- **Versioning is SemVer, 0.x-aware.** Derive the recommended release bump from the
  Conventional Commits since the last tag:
  - `fix` / `docs` / `chore` / `refactor` / `perf` / `test` → **patch**
  - `feat` → **minor**
  - a breaking change (`!` / `BREAKING CHANGE:`) → **major** at ≥1.0; while on 0.x,
    **minor** (0.x minors may break).
  Recommend that bump when cutting a release; the maintainer confirms and may
  override.

## Conventions that bite
- **Hand-formatted — never run `cargo fmt`.** Match the surrounding 4-space,
  brace-on-same-line style by hand; `cargo fmt` would reflow the whole tree.
- **Never block the render loop.** Anything slow (git status, `gh`, diffs, cleanup)
  runs on a background thread that sends its result back over an `mpsc` channel to
  the event loop, gated by an `*_inflight` flag + a drop-guard that always clears it
  (see `request_branch_status_refresh` / `request_pr_status_refresh`). Branch status
  refreshes ~2s; PR/CI status ~60s (network).
- **All `gh` calls go through `git::run_gh`** — non-interactive (prompts/pager off,
  stdin null), ETXTBSY-retry, 20s timeout. Never invoke `gh` directly.
- **Overlays** (help, palette, modal, diff) own the screen: they swallow keys AND
  must appear in the mouse/paste guards in `main.rs` so clicks/paste don't leak to
  the tree behind them. Global focus is `Focus { Tree, Embedded }` (Tab-switched).
- **Adding a keybinding = 5 sites in `keymap.rs`**: the `Action` enum, `ALL_ACTIONS`,
  `name()`, `description()`, `DEFAULT_BINDINGS`. All keys are rebindable via config.
- **State** (`AppState`) persists to `state.json` atomically, 3-way-merged against
  concurrent `kmd` writes. Workspaces are git worktrees on per-workspace branches
  named after the workspace (suffixed `-2`… on collision; pre-0.11 workspaces may
  carry a legacy `kommand0/<name>` branch — still fully supported);
  a **fallback workspace has no `worktree_path`** (its `working_dir` is the repo
  root) — per-workspace git/PR features gate on `worktree_path.is_some()`.

## Testing
- Unit tests inline (`#[cfg(test)] mod tests`). **Core** tests build real temp git
  repos (`init_repo`). **TUI** tests use `test_app()` + `render_to_string`
  (`TestBackend`); insta `.snap` files snapshot the main layouts — review diffs
  cell-by-cell, **never bulk-accept**.
- `apps/tui/tests/e2e.rs` spawns the real binary in a PTY and asserts the rendered
  screen (vt100). **Known-flaky under parallel load** — re-run
  `cargo test -p kommand0-tui -- --test-threads=1` to confirm a real failure.
- External tools are stubbed via env vars: `KOMMAND0_CLAUDE_BIN` (the `embed-stub`
  fixture), `KOMMAND0_GH_BIN` (shell-script gh stubs), `KOMMAND0_STATE_DIR`
  (hermetic state), `KOMMAND0_CONFIG`, `KOMMAND0_SHELL`.

## Gotchas
- **PR CI builds `refs/pull/N/merge`** (your branch + latest main). Green on the
  branch but red on the PR is usually a *semantic* merge conflict with an advanced
  main — merge `main` in, don't chase the cache.
- ratatui bundles crossterm; the workspace's direct `crossterm` must be the **same
  version** ratatui pulls, or event types won't unify (`cargo tree -i crossterm`
  should show one node).
- Keep the CHANGELOG `[Unreleased]` section current as you land changes (Keep a
  Changelog format) — the release workflow rolls it into the version automatically.
  0.x semver: minor bumps may carry breaking changes.

## Working style
Default to **Discuss → Research → Plan → Execute → Verify**, proportionate to size
(skip the ceremony for tiny obvious edits). Smallest reasonable change; preserve
structure; ask ≤3 focused questions when ambiguous or risky; get build + clippy +
tests green before calling it done, and say what you did *not* verify.

For non-trivial work prefer the `cst:*` agents / skills: `/cst:dev-flow`
(plan → review → implement → review → PR → watch CI), `/cst:pr-review <n>`,
`cst:planner`, `cst:implementer`, and the `cst:*-reviewer` lenses. Don't spawn
subagents for trivial single-file edits.
