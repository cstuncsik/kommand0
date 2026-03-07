# Phase 2: Workspace Model - Research

**Researched:** 2026-03-07
**Domain:** Rust domain modeling, CLI subcommands, TUI tree views, JSON state persistence
**Confidence:** HIGH

## Summary

Phase 2 adds a `Workspace` domain model to the core crate, CLI subcommands for workspace CRUD, and a tree view in the TUI showing repo-to-workspace relationships. The technical surface is well-understood: the existing `AppState`/`RepoEntry`/clap/ratatui stack provides clear extension points. The main complexity is the TUI tree view rendering (replacing a flat list with expandable repo nodes containing workspace children) and the smart repo resolver for CLI commands.

The `tui-tree-widget` crate (v0.24) is compatible with ratatui 0.29 and provides a ready-made tree widget with expand/collapse. However, given the shallow tree depth (only 2 levels: repo -> workspace) and the specific visual requirements (triangle indicators, status dots, connector lines, dimmed archived items), a custom implementation using ratatui's built-in rendering primitives is the recommended approach. This avoids an external dependency for a simple structure and gives full control over the visual design.

**Primary recommendation:** Extend `AppState` with `Workspace` struct and `#[serde(default)]` vec, mirror the `Repo` CLI subcommand pattern for `Workspace`, and implement the tree view as a custom stateful widget using ratatui spans/lines with manual indentation and expand/collapse state tracking.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Many-to-one: a repo can have multiple workspaces
- Globally unique workspace names (no two workspaces share a name across all repos)
- ID uses same hex-timestamp pattern as RepoEntry (`generate_id()`)
- Fields: `id`, `name`, `repo_id` (foreign key to RepoEntry), `working_dir` (absolute path), `active` (bool), `created_at` (unix timestamp, seconds)
- `working_dir` defaults to repo root path in Phase 2 (override comes with worktrees in v2)
- `repo_id` is a reference only -- look up repo details when needed (normalized, single source of truth)
- Workspace creation requires the referenced repo to exist in the registry
- Auto-generate name from repo name; error if name collision (require `--name` override, no auto-suffix)
- Flat sibling in state.json: `{ "repos": [...], "workspaces": [...] }` with `repo_id` foreign key
- `#[serde(default)]` on workspaces vec so old state files load cleanly (missing field = empty vec)
- Insertion order preserved (newest at end, simple append)
- Full file rewrite on every save (consistent with existing `AppState::save()` pattern)
- Keep `serde_json::to_string_pretty` for inspectability
- No state.json versioning -- serde defaults handle migration
- `created_at` stored as unix seconds (not milliseconds)
- Subcommand pattern mirrors repo: `kmd workspace create/list/show/delete/archive/activate`
- Full command surface: create, list, show, delete, archive, activate
- `create`: `kmd workspace create <name> --repo <ref>` -- name is positional, repo is required flag
- Auto-generate name from repo name if name omitted, error on collision
- `list`: table format (ID, Name, Repo, Status columns), active-only by default, `--all` for archived, `--repo` filter
- `show`: `kmd workspace show <name>` -- all fields displayed
- `delete`: `kmd workspace delete <name>` -- hard delete, confirmation prompt, `--force` to skip
- Error if not a TTY and `--force` not provided
- `archive`: `kmd workspace archive <name>` -- sets active=false
- `activate`: `kmd workspace activate <name>` -- sets active=true
- Smart repo resolver: accepts name, path, or ID; resolution order: path (if `/`), name, ID
- TUI tree view with repos as parent nodes, workspaces as indented children
- Collapse/expand with Enter, triangle indicators
- Flat navigation: j/k moves through repos and expanded workspaces sequentially
- Status dots: green filled circle = active, gray open circle = archived
- Archived workspaces always visible but dimmed
- Context-sensitive right pane based on selection type
- CLI errors: terse unix-like format to stderr, exit code 1
- Hard delete removes from state.json; block repo removal if workspaces exist (note: `kmd repo remove` not in Phase 2 scope)

### Claude's Discretion
- Exact tree view rendering implementation (ratatui widget choice)
- Table formatting library/approach for CLI list output
- Whether to split core/lib.rs into modules as workspace model is added
- Test organization for workspace model
- Path truncation algorithm details
- Confirmation prompt implementation (stdin read approach)

### Deferred Ideas (OUT OF SCOPE)
- Shell tab completion for workspace names -- UX polish item, not Phase 2
- `kmd repo remove` command -- needed eventually but not in scope for Phase 2
- Workspace rename capability -- future enhancement
- Git worktree-backed workspaces with custom working_dir -- v2 (TREE-01, TREE-02)
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| WORK-01 | User can create a logical workspace from a repo via CLI | Workspace struct, `create_workspace()` method, clap `WorkspaceAction::Create` variant, smart repo resolver |
| WORK-02 | User can list workspaces via CLI | `list_workspaces()` with filtering, clap `WorkspaceAction::List` with `--all` and `--repo` flags, table output formatting |
| WORK-03 | User can list and select workspaces in TUI | Tree view rendering with `TreeItem` enum (Repo/Workspace), expand/collapse state, flat sequential navigation |
| WORK-04 | Workspaces are persisted in state.json | `#[serde(default)]` on `workspaces: Vec<Workspace>` in `AppState`, backward-compatible deserialization |
| WORK-05 | TUI shows repo -> workspace relationships | Tree view with repos as parent nodes, workspace children indented with connector lines, status indicators |
</phase_requirements>

## Standard Stack

### Core (already in workspace)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| ratatui | 0.29 | TUI framework | Already used; tree view built with its primitives |
| crossterm | 0.28 | Terminal backend | Already used with event-stream feature |
| clap | 4 | CLI argument parsing | Already used; extend with Workspace subcommand |
| serde + serde_json | 1 | JSON serialization | Already used for state persistence |
| tokio | 1 | Async runtime | Already used in TUI event loop |
| anyhow | 1 | Error handling | Already used throughout |

### Supporting (no new dependencies needed)
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| `std::io::IsTerminal` | std | TTY detection for delete confirmation | Check if stdin is interactive before prompting |
| `std::io::stdin().read_line()` | std | Confirmation prompt input | Simple y/n prompt for delete |
| `chrono` or manual formatting | -- | Timestamp display | See recommendation below |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Custom tree rendering | `tui-tree-widget` 0.24 | Compatible with ratatui 0.29, but adds dependency for a 2-level tree; custom gives full control over triangle/dot/connector styling |
| Manual table formatting | `comfy-table` or `tabled` | Adds dependency; simple `format!` with padding is sufficient for 4-column output |
| `dialoguer` for confirmation | `std::io::stdin` | Overkill for a single y/n prompt |
| `chrono` for timestamps | Manual `time` formatting | chrono is heavy; `created_at` display can use a small helper or the lightweight `time` crate |

**Recommendation on timestamp display:** Use manual formatting with `libc::localtime_r` or add the `chrono` crate. Given the requirement to display "2026-03-07 10:30" in local timezone, `chrono` is the pragmatic choice -- it handles timezone correctly. Add `chrono = "0.4"` to workspace deps. Alternatively, use `time = "0.3"` with the `local-offset` feature (lighter weight). This is Claude's discretion per CONTEXT.md.

**Installation (if chrono chosen):**
```bash
# Add to [workspace.dependencies] in root Cargo.toml:
chrono = "0.4"
# Add to crates/core/Cargo.toml [dependencies]:
chrono.workspace = true
```

## Architecture Patterns

### Recommended Core Module Split

As workspace model adds significant code to `lib.rs`, split into modules (Claude's discretion per CONTEXT.md):

```
crates/core/src/
├── lib.rs           # Re-exports, AppState (persistence root)
├── repo.rs          # RepoEntry struct, repo-specific methods
├── workspace.rs     # Workspace struct, workspace CRUD methods
└── id.rs            # generate_id() helper (shared)
```

Alternative: keep everything in `lib.rs` if the file stays under ~300 lines. The split becomes necessary if workspace methods + tests push it beyond that.

### Pattern 1: Workspace Struct

**What:** Domain model for workspace, mirroring RepoEntry pattern
**When to use:** Core data model for all workspace operations

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub repo_id: String,
    pub working_dir: String,
    pub active: bool,
    pub created_at: u64,
}
```

### Pattern 2: AppState Extension with Serde Default

**What:** Add workspaces vec to AppState with backward-compatible deserialization
**When to use:** State persistence

```rust
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppState {
    pub repos: Vec<RepoEntry>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
}
```

This ensures old `state.json` files without a `"workspaces"` key deserialize cleanly -- the field defaults to an empty vec. Verified pattern from serde docs (https://serde.rs/attr-default.html).

### Pattern 3: Smart Repo Resolver

**What:** Resolve a user-provided string to a RepoEntry by trying path, name, then ID
**When to use:** All workspace CLI commands that accept `--repo <ref>`

```rust
impl AppState {
    pub fn resolve_repo(&self, reference: &str) -> anyhow::Result<&RepoEntry> {
        // 1. If contains '/', try as path (canonicalize and match)
        if reference.contains('/') {
            if let Some(repo) = self.repos.iter().find(|r| r.path == canonical) {
                return Ok(repo);
            }
        }
        // 2. Try as name
        if let Some(repo) = self.repos.iter().find(|r| r.name == reference) {
            return Ok(repo);
        }
        // 3. Try as ID
        if let Some(repo) = self.repos.iter().find(|r| r.id == reference) {
            return Ok(repo);
        }
        bail!("No repo found matching '{}'. Checked: name, path, id. Use `kmd repo list` to see tracked repos.", reference)
    }
}
```

### Pattern 4: Testable Methods with Injectable Base Path

**What:** Follow the existing `add_repo_with_base()` pattern for all workspace mutation methods
**When to use:** All state-mutating workspace methods

```rust
impl AppState {
    pub fn create_workspace_with_base(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
    ) -> anyhow::Result<Workspace> {
        let repo = self.resolve_repo(repo_ref)?;
        let ws_name = match name {
            Some(n) => n.to_string(),
            None => repo.name.clone(),
        };
        if self.workspaces.iter().any(|w| w.name == ws_name) {
            bail!("workspace already exists: {}", ws_name);
        }
        let ws = Workspace {
            id: generate_id(),
            name: ws_name,
            repo_id: repo.id.clone(),
            working_dir: repo.path.clone(),
            active: true,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_secs(),
        };
        self.workspaces.push(ws.clone());
        self.save_to(base)?;
        Ok(ws)
    }
}
```

### Pattern 5: CLI Subcommand Structure

**What:** Clap nested subcommand mirroring existing Repo pattern
**When to use:** CLI command definitions

```rust
#[derive(Subcommand)]
enum Commands {
    /// Manage tracked repos
    Repo {
        #[command(subcommand)]
        action: RepoAction,
    },
    /// Manage workspaces
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
}

#[derive(Subcommand)]
enum WorkspaceAction {
    /// Create a new workspace
    Create {
        /// Workspace name (auto-generated from repo if omitted)
        name: Option<String>,
        /// Repo reference (name, path, or ID)
        #[arg(long)]
        repo: String,
    },
    /// List workspaces
    List {
        /// Show all including archived
        #[arg(long)]
        all: bool,
        /// Filter by repo
        #[arg(long)]
        repo: Option<String>,
    },
    /// Show workspace details
    Show {
        /// Workspace name
        name: String,
    },
    /// Delete a workspace
    Delete {
        /// Workspace name
        name: String,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
    /// Archive a workspace (set inactive)
    Archive {
        /// Workspace name
        name: String,
    },
    /// Activate an archived workspace
    Activate {
        /// Workspace name
        name: String,
    },
}
```

### Pattern 6: TUI Tree View (Custom Implementation)

**What:** Custom tree rendering using ratatui's `Line`, `Span`, and manual state tracking
**When to use:** Left pane of TUI

The tree view needs these state additions to the `App` struct:

```rust
enum TreeSelection {
    Repo(usize),        // index into repos vec
    Workspace(usize),   // index into flattened visible items
}

struct App {
    repos: Vec<RepoEntry>,
    workspaces: Vec<Workspace>,
    expanded: HashSet<String>,  // repo IDs that are expanded
    tree_items: Vec<TreeNode>,  // flattened visible tree for rendering
    selected_index: usize,      // index into tree_items
    // ... existing fields
}

enum TreeNode {
    Repo { repo_id: String, name: String, workspace_count: usize, has_active: bool },
    Workspace { ws: Workspace, repo_name: String },
    Hint { text: String },  // "(no workspaces)" etc.
}
```

Rendering approach:
- Build a flat `Vec<TreeNode>` from repos + workspaces based on expand state
- Each `TreeNode` renders as a `Line` with appropriate `Span`s for indent, icons, text, styling
- Use `Paragraph` widget with the lines vec (not `List` widget) to have full control, OR use `List` with styled `ListItem`s
- Track `selected_index` into the flat vec for navigation

### Anti-Patterns to Avoid
- **Storing repo data in Workspace struct:** The workspace holds `repo_id` only. Look up repo details when needed. Do not denormalize.
- **Using `List` widget as-is for tree:** The built-in `List` widget doesn't support mixed indentation well. Build custom `ListItem`s with styled `Span`s, or use `Paragraph` with styled lines.
- **Saving on every navigation event in TUI:** Only save state when workspace CRUD operations occur through CLI. TUI is read-only in Phase 2 (no TUI-initiated workspace creation).
- **Blocking on git operations during tree render:** Tree view only shows workspace/repo metadata. Git status display remains as-is (right pane on Enter).

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| CLI argument parsing | Custom arg parser | clap 4 derive macros | Handles help text, validation, error messages automatically |
| JSON serialization | Manual JSON string building | serde_json | Handles escaping, nested types, backward compat via `#[serde(default)]` |
| Local timezone display | Manual UTC offset calculation | chrono 0.4 or time 0.3 | Timezone rules are complex (DST transitions, system tz database) |
| Terminal state management | Manual raw mode/alternate screen | ratatui::init/restore | Panic hook integration already done in Phase 1 |

**Key insight:** The workspace model is pure domain logic -- no need for new infrastructure. The existing stack (serde, clap, ratatui, anyhow) handles everything. The only new dependency consideration is timestamp formatting.

## Common Pitfalls

### Pitfall 1: Name Collision Race Condition
**What goes wrong:** Two rapid CLI calls could both pass the uniqueness check before either saves
**Why it happens:** File-based state with no locking
**How to avoid:** Acceptable risk for a single-user local tool. Document but don't over-engineer. Full file rewrite is atomic enough via `fs::write()`.
**Warning signs:** Tests that run workspace creation in parallel

### Pitfall 2: Stale Repo References
**What goes wrong:** User deletes a repo from state.json manually, workspace now references nonexistent repo_id
**Why it happens:** No referential integrity enforcement at storage layer
**How to avoid:** When loading/displaying workspaces, gracefully handle missing repo references. Show "(unknown repo)" instead of panicking. The `resolve_repo` is for creation; display code should be tolerant.
**Warning signs:** `unwrap()` on repo lookups by ID

### Pitfall 3: Serde Default Not Applied to Existing Fields
**What goes wrong:** Developer puts `#[serde(default)]` on the struct instead of the field, or forgets it entirely
**Why it happens:** Confusion between struct-level and field-level `#[serde(default)]`
**How to avoid:** Use field-level `#[serde(default)]` on the `workspaces` vec specifically. Test by deserializing a JSON object with only `"repos"` key.
**Warning signs:** Deserialization errors when loading old state files

### Pitfall 4: Tree Selection State Drift
**What goes wrong:** After expand/collapse, the selected index points to the wrong item because the flat list changed size
**Why it happens:** Flat index not recalculated after tree structure change
**How to avoid:** After any expand/collapse, rebuild the flat tree items list and clamp/adjust the selected index. If the previously selected item is still visible, keep it selected. If collapsed away, select the parent repo.
**Warning signs:** Selection jumping to wrong item after toggling expand

### Pitfall 5: TTY Detection for Delete Confirmation
**What goes wrong:** Script pipes input to `kmd workspace delete name` and hangs waiting for confirmation
**Why it happens:** stdin.read_line() blocks even when stdin is not a terminal
**How to avoid:** Check `std::io::stdin().is_terminal()` before prompting. If not a TTY and `--force` not provided, error out immediately.
**Warning signs:** CI/script hangs on workspace delete

### Pitfall 6: Path Canonicalization Mismatch
**What goes wrong:** Smart repo resolver fails because user-provided path is relative but stored paths are canonical absolute
**Why it happens:** Comparing raw user input against canonicalized stored paths
**How to avoid:** In the resolver, canonicalize the input path before comparison (when input contains `/`). Handle the case where canonicalization fails (path doesn't exist) gracefully.
**Warning signs:** `kmd workspace create --repo ./my-repo` fails even though repo is tracked

## Code Examples

### Confirmation Prompt (Delete)
```rust
use std::io::{self, IsTerminal, Write};

fn confirm_delete(name: &str, force: bool) -> anyhow::Result<bool> {
    if force {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        bail!("error: refusing to delete without --force in non-interactive mode");
    }
    print!("Delete workspace '{}'? [y/N] ", name);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}
```

### CLI Table Output (List)
```rust
fn print_workspace_table(workspaces: &[Workspace], repos: &[RepoEntry]) {
    println!("{:<12} {:<20} {:<20} {:<10}", "ID", "NAME", "REPO", "STATUS");
    for ws in workspaces {
        let repo_name = repos.iter()
            .find(|r| r.id == ws.repo_id)
            .map(|r| r.name.as_str())
            .unwrap_or("(unknown)");
        let status = if ws.active { "active" } else { "archived" };
        println!("{:<12} {:<20} {:<20} {:<10}", ws.id, ws.name, repo_name, status);
    }
}
```

### Tree View Line Rendering
```rust
use ratatui::text::{Line, Span};
use ratatui::style::{Color, Modifier, Style};

fn render_repo_line(name: &str, expanded: bool, is_selected: bool) -> Line<'static> {
    let arrow = if expanded { "\u{25BC} " } else { "\u{25B6} " }; // down/right triangles
    let style = if is_selected {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(arrow.to_string(), style),
        Span::styled(name.to_string(), style),
    ])
}

fn render_workspace_line(name: &str, active: bool, is_selected: bool) -> Line<'static> {
    let connector = "  \u{251C}\u{2500} "; // "  ├─ "
    let dot = if active { "\u{25CF}" } else { "\u{25CB}" }; // filled/open circle
    let dot_color = if active { Color::Green } else { Color::DarkGray };
    let text_style = if is_selected {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else if !active {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(connector.to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{} ", dot), Style::default().fg(dot_color)),
        Span::styled(name.to_string(), text_style),
    ])
}
```

### Timestamp Display Helper
```rust
// If using chrono:
use chrono::{Local, TimeZone};

fn format_timestamp(unix_secs: u64) -> String {
    Local.timestamp_opt(unix_secs as i64, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| format!("{}", unix_secs))
}
```

### Path Truncation
```rust
fn truncate_path(path: &str, max_width: usize) -> String {
    if path.len() <= max_width {
        return path.to_string();
    }
    if max_width < 4 {
        return "...".to_string();
    }
    let keep = max_width - 3; // room for "..."
    format!("...{}", &path[path.len() - keep..])
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `atty` crate for TTY detection | `std::io::IsTerminal` trait | Rust 1.70 (June 2023) | No external dependency needed |
| `tui-rs` tree widgets | `tui-tree-widget` 0.24 for ratatui 0.29 | Jan 2026 | Available if custom is too complex |
| `ratatui::Terminal::new()` + manual init | `ratatui::init()` / `ratatui::restore()` | ratatui 0.28+ | Already used in Phase 1; includes panic hook |

## Open Questions

1. **Chrono vs time crate for timestamp formatting**
   - What we know: Both work. chrono 0.4 is more popular and API is simpler. `time` 0.3 is lighter but `local-offset` feature has soundness concerns on some platforms.
   - What's unclear: Project preference for dependency weight vs API simplicity
   - Recommendation: Use `chrono = "0.4"` -- it is the standard Rust choice for human-readable datetime formatting. The weight difference is negligible for a local CLI/TUI tool.

2. **Whether to split core/lib.rs into modules now**
   - What we know: Current lib.rs is 227 lines. Adding Workspace struct + 6 CRUD methods + resolver + tests could push it to 500+ lines.
   - What's unclear: Exact final size depends on test density
   - Recommendation: Split into modules (`repo.rs`, `workspace.rs`) during this phase. Re-export from `lib.rs` so consumers don't change their imports.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test framework (`#[cfg(test)]`) |
| Config file | None (cargo test uses Cargo.toml) |
| Quick run command | `cargo test -p kommand0-core` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements to Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| WORK-01 | Create workspace from repo via CLI | unit | `cargo test -p kommand0-core -- workspace::tests::create` | No -- Wave 0 |
| WORK-01 | Reject creation for nonexistent repo | unit | `cargo test -p kommand0-core -- workspace::tests::create_missing_repo` | No -- Wave 0 |
| WORK-01 | Reject duplicate workspace name | unit | `cargo test -p kommand0-core -- workspace::tests::create_duplicate_name` | No -- Wave 0 |
| WORK-01 | Auto-generate name from repo name | unit | `cargo test -p kommand0-core -- workspace::tests::create_auto_name` | No -- Wave 0 |
| WORK-01 | Smart repo resolver (name, path, ID) | unit | `cargo test -p kommand0-core -- workspace::tests::resolve_repo` | No -- Wave 0 |
| WORK-02 | List active workspaces | unit | `cargo test -p kommand0-core -- workspace::tests::list_active` | No -- Wave 0 |
| WORK-02 | List all including archived | unit | `cargo test -p kommand0-core -- workspace::tests::list_all` | No -- Wave 0 |
| WORK-02 | List filtered by repo | unit | `cargo test -p kommand0-core -- workspace::tests::list_by_repo` | No -- Wave 0 |
| WORK-03 | TUI tree view renders repos and workspaces | manual-only | Visual inspection in TUI | N/A |
| WORK-04 | Workspaces persist across save/load | unit | `cargo test -p kommand0-core -- workspace::tests::roundtrip` | No -- Wave 0 |
| WORK-04 | Old state.json without workspaces loads cleanly | unit | `cargo test -p kommand0-core -- workspace::tests::backward_compat` | No -- Wave 0 |
| WORK-05 | TUI shows repo-workspace tree hierarchy | manual-only | Visual inspection in TUI | N/A |

### Sampling Rate
- **Per task commit:** `cargo test -p kommand0-core`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] Workspace CRUD unit tests in `crates/core/src/` (workspace module tests block)
- [ ] Smart repo resolver tests
- [ ] Backward compatibility test (deserialize JSON without `workspaces` key)
- [ ] Archive/activate/delete tests
- [ ] `chrono` (or `time`) added to workspace dependencies for timestamp formatting

## Sources

### Primary (HIGH confidence)
- Existing codebase: `crates/core/src/lib.rs`, `apps/cli/src/main.rs`, `apps/tui/src/main.rs` -- current patterns verified by direct reading
- [serde field attributes](https://serde.rs/field-attrs.html) -- `#[serde(default)]` behavior for backward-compatible deserialization
- [Rust std::io::IsTerminal](https://doc.rust-lang.org/std/io/trait.IsTerminal.html) -- TTY detection without external dependency

### Secondary (MEDIUM confidence)
- [tui-tree-widget v0.24](https://crates.io/crates/tui-tree-widget) -- confirmed ratatui 0.29 compatibility, available as fallback
- [ratatui widgets docs](https://docs.rs/ratatui/latest/ratatui/widgets/index.html) -- List, Paragraph, Span primitives for custom tree
- [clap derive tutorial](https://docs.rs/clap/latest/clap/_derive/_tutorial/index.html) -- nested subcommand pattern

### Tertiary (LOW confidence)
- None -- all findings verified against primary or secondary sources

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- all libraries already in workspace, versions verified from Cargo.toml
- Architecture: HIGH -- patterns directly extend existing codebase patterns visible in source
- Pitfalls: HIGH -- derived from domain analysis and existing code patterns
- Tree view rendering: MEDIUM -- custom implementation recommended over tui-tree-widget; approach is sound but specific rendering details need iteration

**Research date:** 2026-03-07
**Valid until:** 2026-04-07 (stable domain, no fast-moving dependencies)
