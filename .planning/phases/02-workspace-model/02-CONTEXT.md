# Phase 2: Workspace Model - Context

**Gathered:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Users can create logical workspaces from tracked repos and navigate them in both CLI and TUI. Workspaces are persisted in state.json. The TUI shows repo-to-workspace relationships in a tree view. This phase does NOT add session execution, process management, or git worktree backing -- those are Phases 3 and v2 respectively.

</domain>

<decisions>
## Implementation Decisions

### Workspace identity
- Many-to-one: a repo can have multiple workspaces
- Globally unique workspace names (no two workspaces share a name across all repos)
- ID uses same hex-timestamp pattern as RepoEntry (`generate_id()`)
- Fields: `id`, `name`, `repo_id` (foreign key to RepoEntry), `working_dir` (absolute path), `active` (bool), `created_at` (unix timestamp, seconds)
- `working_dir` defaults to repo root path in Phase 2 (override comes with worktrees in v2)
- `repo_id` is a reference only -- look up repo details when needed (normalized, single source of truth)
- Workspace creation requires the referenced repo to exist in the registry
- Auto-generate name from repo name; error if name collision (require `--name` override, no auto-suffix)

### State persistence
- Flat sibling in state.json: `{ "repos": [...], "workspaces": [...] }` with `repo_id` foreign key
- `#[serde(default)]` on workspaces vec so old state files load cleanly (missing field = empty vec)
- Insertion order preserved (newest at end, simple append)
- Full file rewrite on every save (consistent with existing `AppState::save()` pattern)
- Keep `serde_json::to_string_pretty` for inspectability
- No state.json versioning -- serde defaults handle migration
- `created_at` stored as unix seconds (not milliseconds)

### CLI commands
- Subcommand pattern mirrors repo: `kmd workspace create/list/show/delete/archive/activate`
- Full command surface: create, list, show, delete, archive, activate
- `create`: `kmd workspace create <name> --repo <ref>` -- name is positional, repo is required flag
  - Auto-generate name from repo name if name omitted, error on collision
  - Prints confirmation line: "Created workspace: my-feature (repo: myapp)"
- `list`: table format (ID, Name, Repo, Status columns), active-only by default, `--all` for archived, `--repo` filter
- `show`: `kmd workspace show <name>` -- all fields displayed
- `delete`: `kmd workspace delete <name>` -- hard delete from state.json, confirmation prompt, `--force` to skip
  - Error if not a TTY and `--force` not provided (safe for scripts)
  - Prints: "Deleted workspace: my-feature (repo: myapp)"
  - Archived workspaces can be deleted directly (no activate-first requirement)
- `archive`: `kmd workspace archive <name>` -- sets active=false
- `activate`: `kmd workspace activate <name>` -- sets active=true
- All names/refs are positional arguments (not `--name` flags)

### Smart repo resolver
- Accepts repo name, path, or ID -- resolves automatically
- Resolution order: path first (if input contains `/`), then name, then ID
- On failure: specific error -- "No repo found matching 'foo'. Checked: name, path, id. Use `kmd repo list` to see tracked repos."

### TUI tree view
- Left pane: tree view with repos as parent nodes, workspaces as indented children
- Collapse/expand: repos start collapsed, Enter toggles expand/collapse
- Indicators: triangle before repo name (collapsed), triangle-down (expanded)
- Workspace indent: 2-space indent with connector line characters for workspace items under repo
- Flat navigation: j/k moves through repos and their expanded workspaces sequentially
- Workspace items show: name + status dot (green filled circle = active, gray open circle = archived)
- Archived workspaces always visible but dimmed/grayed out
- Repos with no workspaces: show "(no workspaces)" hint in dimmed text when expanded
- All-archived repos: show expand arrow, expanding reveals "(all archived)" hint
- Auto-select first repo on launch, but don't auto-expand

### TUI right pane
- Context-sensitive content based on selection:
  - Nothing selected: welcome text "Select a workspace to see details"
  - Repo selected: key-value summary -- Name, Path, Workspaces count
  - Workspace selected: key-value details -- Name, Repo, Dir, Status, Created
- Dynamic title: " Repo: myapp " or " Workspace: my-feature " based on selection
- Key-value layout: bold/colored labels, normal values (easy to scan)
- Long paths truncated with ellipsis if wider than pane
- created_at displayed as human-readable local timezone: "2026-03-07 10:30"
- No action hints in pane -- Phase 4 (UX Polish) handles that

### Empty states
- No repos: hint text in left pane -- "No repos tracked. Run: kmd repo add <path>"
- No workspaces under expanded repo: dimmed "(no workspaces)" hint
- No selection: right pane shows welcome text

### Error handling
- CLI errors: terse unix-like format -- "error: workspace already exists: my-feature"
- Exception: repo resolver error is specific with guidance (see smart repo resolver above)
- Errors to stderr, normal output to stdout (unix standard)
- Exit code 1 for all errors, 0 for success
- TUI errors: inline in right pane with red text + "Error:" prefix (consistent with existing Status::Error pattern)

### Workspace lifecycle
- Hard delete removes from state.json entirely (separate from archive)
- Block repo removal if workspaces exist (force user to delete workspaces first) -- note: `kmd repo remove` doesn't exist yet, not in Phase 2 scope
- No limit on workspaces per repo
- No rename capability in Phase 2

### Claude's Discretion
- Exact tree view rendering implementation (ratatui widget choice)
- Table formatting library/approach for CLI list output
- Whether to split core/lib.rs into modules as workspace model is added
- Test organization for workspace model
- Path truncation algorithm details
- Confirmation prompt implementation (stdin read approach)

</decisions>

<code_context>
## Existing Code Insights

### Reusable Assets
- `AppState` (crates/core/src/lib.rs): Add `workspaces: Vec<Workspace>` with `#[serde(default)]` -- load/save already handles JSON roundtrip
- `generate_id()` (crates/core/src/lib.rs:102): Reuse for workspace IDs
- `RepoEntry` (crates/core/src/lib.rs:9): Workspace references via `repo_id` foreign key to `RepoEntry.id`
- CLI `Commands` enum (apps/cli/src/main.rs:12): Add `Workspace { action: WorkspaceAction }` variant following `Repo` pattern
- `App` struct (apps/tui/src/main.rs:21): Needs new fields for tree state (expanded repos, selected item type)
- `ui()` function (apps/tui/src/main.rs:125): Refactor left pane from flat list to tree view

### Established Patterns
- `anyhow::Result` + `bail!()` for validation -- continue for all workspace operations
- `add_repo_with_base()` pattern for testable methods with injectable base path -- follow for workspace methods
- `#[derive(Debug, Clone, Serialize, Deserialize)]` on all persisted types
- Workspace deps via `.workspace = true` in member Cargo.toml files
- `Status` enum for TUI state -- extend for workspace/repo selection context

### Integration Points
- `AppState` in crates/core/src/lib.rs -- add workspace CRUD methods alongside existing repo methods
- CLI main.rs match block -- add Workspace arm
- TUI `App::new()` -- load workspaces alongside repos
- TUI `ui()` left pane rendering -- replace flat List widget with tree rendering
- TUI key handling in `run()` -- add Enter for expand/collapse, context-sensitive right pane

</code_context>

<specifics>
## Specific Ideas

- Tree view should feel like a file explorer -- expand/collapse with triangle indicators, indented children with connector lines
- Status dots (green filled / gray open) give quick visual scan of workspace state
- CLI follows unix conventions: terse errors to stderr, table output to stdout, exit codes, TTY-aware confirmation
- Details pane is a placeholder for Phase 3 session output -- keep it simple (key-value) so it's easy to replace

</specifics>

<deferred>
## Deferred Ideas

- Shell tab completion for workspace names -- UX polish item, not Phase 2
- `kmd repo remove` command -- needed eventually but not in scope for Phase 2
- Workspace rename capability -- future enhancement
- Git worktree-backed workspaces with custom working_dir -- v2 (TREE-01, TREE-02)

</deferred>

---

*Phase: 02-workspace-model*
*Context gathered: 2026-03-07*
