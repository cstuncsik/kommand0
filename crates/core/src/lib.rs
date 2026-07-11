pub mod config;
pub mod git;
pub mod id;
pub mod repo;
pub mod session;
pub mod workspace;
pub mod worktree;

pub use config::Config;
pub use git::{
    BranchStatus, FileDiff, PrChecks, PrReview, PrState, PrStatus, branch_status,
    cleanup_merged_workspace, diff_files_vs_default_branch, pr_statuses,
};
pub use id::generate_id;
pub use repo::{RepoEntry, run_git_status};
pub use session::{Session, SessionStatus};
pub use workspace::Workspace;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    /// On-disk schema version, stamped on every save. A file without it predates
    /// versioning and loads as v1 (see [`legacy_state_version`]); [`AppState::migrate`]
    /// brings any older file up to `STATE_VERSION` on load, so an upgrade can evolve
    /// the shape without bricking an existing `state.json`.
    #[serde(default = "legacy_state_version")]
    pub version: u32,
    pub repos: Vec<RepoEntry>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// Per-workspace Claude session ids (caller-assigned UUIDs), one per session
    /// tab, in tab order — so reopening a workspace resumes each of its sessions
    /// (`claude --resume <id>`) across app restarts.
    #[serde(default, deserialize_with = "de_embedded_sessions")]
    pub embedded_sessions: HashMap<String, Vec<String>>,
    /// Optional per-session display titles, keyed workspace-id → session-id →
    /// title. Additive: absent for un-renamed sessions, so old state files (and
    /// the common case) carry nothing. Kept in lockstep with `embedded_sessions`
    /// — pruned whenever a session id or workspace is removed — so a title never
    /// outlives its session.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub embedded_titles: HashMap<String, HashMap<String, String>>,
}

/// A `state.json` with no `version` field predates schema versioning; that shape
/// is schema v1, so a missing value deserializes as 1 — deliberately NOT the
/// current version, or a genuinely-old file would skip the migrations between
/// v1 and today.
fn legacy_state_version() -> u32 {
    1
}

impl Default for AppState {
    // A freshly-created state is at the current schema (unlike a derived Default,
    // which would leave `version` at 0 and mis-stamp a brand-new file).
    fn default() -> Self {
        Self {
            version: Self::STATE_VERSION,
            repos: Vec::new(),
            workspaces: Vec::new(),
            sessions: Vec::new(),
            embedded_sessions: HashMap::new(),
            embedded_titles: HashMap::new(),
        }
    }
}

/// Tolerates the legacy single-string form (`{"w1":"uuid"}`) and an explicit
/// `null`, mapping both into the current `Vec<String>` shape. Serialization
/// always emits the array form.
fn de_embedded_sessions<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    let opt: Option<HashMap<String, Option<OneOrMany>>> = Option::deserialize(deserializer)?;
    Ok(opt
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(ws, v)| {
            let ids = match v {
                Some(OneOrMany::One(id)) => vec![id],
                Some(OneOrMany::Many(ids)) => ids,
                None => return None, // tolerate a per-entry null
            };
            if ids.is_empty() {
                None
            } else {
                Some((ws, ids))
            }
        })
        .collect())
}

/// The profile used when `--profile` is omitted (also the migration target
/// for a pre-profiles layout, and what the TUI hides in its tree title).
pub const DEFAULT_PROFILE: &str = "default";

/// The `--profile` value for this process, recorded once at startup via
/// [`AppState::set_profile`] (absent = [`DEFAULT_PROFILE`]). Process-global on
/// purpose: [`AppState::state_dir`] has no-arg call sites throughout the
/// crates, and "which dir" is already ambient state (the env override is read
/// the same way).
static PROFILE: OnceLock<String> = OnceLock::new();

impl AppState {
    const STATE_DIR: &str = ".kommand0-dev";
    const STATE_FILE: &str = "state.json";
    /// Current on-disk schema version. Bump this when the `state.json` shape
    /// changes in a way that needs a migration, and add the step in [`Self::migrate`].
    const STATE_VERSION: u32 = 1;

    /// Bring a just-loaded state up to `STATE_VERSION`. A file with no `version`
    /// loaded as v1 ([`legacy_state_version`]). No migrations exist yet (v1 is
    /// current), so this only stamps the version; a future schema bump adds
    /// `while state.version < Self::STATE_VERSION { …transform…; state.version += 1 }`.
    fn migrate(state: &mut Self) {
        state.version = Self::STATE_VERSION;
    }

    /// `KOMMAND0_STATE_DIR` when set and non-empty — the exact-dir escape
    /// hatch (no `profiles/` suffix). A set-but-empty value counts as unset;
    /// every consumer ([`Self::state_dir`], [`Self::set_profile`],
    /// [`Self::migrate_legacy_profiles`]) shares this predicate so they can
    /// never disagree on that edge.
    fn state_dir_override() -> Option<PathBuf> {
        std::env::var_os("KOMMAND0_STATE_DIR")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
    }

    /// The profile-independent data root: `.kommand0-dev/` relative to the
    /// current directory in debug builds, else the platform data dir
    /// (`~/Library/Application Support/kommand0` on macOS,
    /// `~/.local/share/kommand0` on Linux).
    fn base_dir() -> PathBuf {
        if cfg!(debug_assertions) {
            PathBuf::from(Self::STATE_DIR)
        } else {
            dirs::data_dir()
                .map(|d| d.join("kommand0"))
                .unwrap_or_else(|| PathBuf::from(Self::STATE_DIR))
        }
    }

    /// Resolve the state directory.
    ///
    /// Priority: `KOMMAND0_STATE_DIR` env var (an exact directory — no
    /// profile suffix), then `<base>/profiles/<profile>`, where `<base>` is
    /// [`Self::base_dir`] and `<profile>` is the `--profile` value recorded
    /// via [`Self::set_profile`] (`default` when the flag was omitted).
    pub fn state_dir() -> PathBuf {
        if let Some(dir) = Self::state_dir_override() {
            return dir; // exact dir, unchanged — the env escape hatch
        }
        let profile = PROFILE.get().map(String::as_str).unwrap_or(DEFAULT_PROFILE);
        Self::base_dir().join("profiles").join(profile)
    }

    /// Validate a would-be profile name: it becomes a path segment under
    /// `profiles/`, so allow only `[A-Za-z0-9._-]+` and at most 64 bytes;
    /// reject `""`, `"."` (would alias the profiles root itself), and `".."`.
    pub fn validate_profile_name(name: &str) -> Result<(), String> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.len() > 64
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
        {
            // The name failed validation, so it may carry control/escape
            // bytes — never echo it raw into a terminal.
            return Err(format!(
                "invalid profile name '{}': allowed characters are letters, digits, '.', '_', '-'",
                name.escape_debug()
            ));
        }
        Ok(())
    }

    /// Validate and record the `--profile` value for this process. Errors on
    /// an invalid name, or when `KOMMAND0_STATE_DIR` is also set (the env var
    /// targets one exact directory — combining it with a profile is
    /// ambiguous). Call once at startup, before any state/config/log access
    /// and before spawning threads.
    pub fn set_profile(name: &str) -> Result<(), String> {
        if Self::state_dir_override().is_some() {
            return Err(
                "--profile cannot be combined with KOMMAND0_STATE_DIR; unset the variable or drop the flag"
                    .to_string(),
            );
        }
        Self::validate_profile_name(name)?;
        // A second call is unreachable by construction (both binaries set the
        // profile exactly once, pre-thread-spawn) — assert that in debug;
        // in release the first value simply stays.
        let set = PROFILE.set(name.to_string());
        debug_assert!(set.is_ok(), "set_profile called more than once");
        Ok(())
    }

    /// One-time migration of the pre-profiles layout: when `<base>/profiles/`
    /// doesn't exist yet and a legacy `state.json` / `config.json` sits at the
    /// `<base>` root, move them into `<base>/profiles/default/`. Legacy
    /// `worktrees/`, `sessions/`, and `kommand0.log` stay at the old root —
    /// state stores worktree and session-log paths absolute, so they keep
    /// resolving. A no-op when `KOMMAND0_STATE_DIR` is set (non-empty): the
    /// env var targets an exact directory outside the profiles layout. When
    /// `KOMMAND0_CONFIG` is set (non-empty), config.json also stays at the
    /// root (the override may point at that very file). On `Err` the caller
    /// must abort startup — proceeding with fresh state would mask the legacy
    /// file (and, after the first save, permanently orphan it).
    pub fn migrate_legacy_profiles() -> anyhow::Result<()> {
        if Self::state_dir_override().is_some() {
            return Ok(());
        }
        // An active KOMMAND0_CONFIG (same non-empty semantics as the state-dir
        // override) freezes config.json: the override may point AT the legacy
        // root file — moving it would silently break the user's settings —
        // and when it points elsewhere the root config.json is unused anyway.
        let config_overridden =
            std::env::var_os("KOMMAND0_CONFIG").is_some_and(|v| !v.is_empty());
        Self::migrate_legacy_profiles_at(&Self::base_dir(), config_overridden)
    }

    /// [`Self::migrate_legacy_profiles`] against an explicit base dir (the
    /// test seam; `config_overridden` threads the KOMMAND0_CONFIG decision so
    /// tests never touch process env).
    fn migrate_legacy_profiles_at(base: &Path, config_overridden: bool) -> anyhow::Result<()> {
        let profiles = base.join("profiles");
        // Idempotence guard: an existing profiles/ dir — even an empty one —
        // means the layout is already current. `exists()` stats through
        // symlinks deliberately: a dangling link counts as absent, so a failed
        // migration below stays retriable instead of being masked.
        if profiles.exists() {
            return Ok(());
        }
        // The legacy filenames are frozen — this migrates the pre-profiles
        // layout only. state.json moves first: if the move fails partway, the
        // orphan is at worst config.json (defaults; recoverable). With
        // KOMMAND0_CONFIG active, config.json stays at the root (see the
        // wrapper above).
        let candidates: &[&str] = if config_overridden {
            &[Self::STATE_FILE]
        } else {
            &[Self::STATE_FILE, "config.json"]
        };
        let legacy: Vec<&str> = candidates
            .iter()
            .copied()
            .filter(|f| base.join(f).exists())
            .collect();
        if legacy.is_empty() {
            // Fresh install: create nothing (the first save / log open creates
            // the profile dir lazily).
            return Ok(());
        }
        let default_dir = profiles.join(DEFAULT_PROFILE);
        // On failure, best-effort remove the dirs we may have just created.
        // `remove_dir` is rmdir(2) — empty-only, never recursive — so it's a
        // no-op once any file has landed; it exists to prevent an empty
        // profiles/ husk that would trip the guard above on every later run
        // and silently mask the legacy files.
        let rollback = |err: anyhow::Error| -> anyhow::Result<()> {
            let _ = fs::remove_dir(&default_dir);
            let _ = fs::remove_dir(&profiles);
            Err(err)
        };
        if let Err(e) = fs::create_dir_all(&default_dir) {
            return rollback(anyhow::anyhow!(
                "failed to migrate {} into {} ({e}); move it there by hand and retry",
                base.join(legacy[0]).display(),
                default_dir.display()
            ));
        }
        for file in legacy {
            let from = base.join(file);
            let to = default_dir.join(file);
            if let Err(e) = fs::rename(&from, &to) {
                // Lost race: a concurrent process won this file's move between
                // our guard and the rename — keep going so any remaining
                // legacy file (e.g. config.json after state.json's race loss)
                // still migrates.
                if e.kind() == std::io::ErrorKind::NotFound && !from.exists() && to.exists() {
                    continue;
                }
                return rollback(anyhow::anyhow!(
                    "failed to migrate {} into {} ({e}); move it there by hand and retry",
                    from.display(),
                    default_dir.display()
                ));
            }
        }
        Ok(())
    }

    /// Load state from the given base directory. Returns default if no state file
    /// exists, and ERRORS on a corrupt file — so a CLI command aborts (leaving the
    /// bad file untouched and recoverable) rather than silently resetting. The TUI,
    /// which can't usefully abort to a corrupted alt-screen, uses
    /// [`Self::load_checked_from`] to degrade gracefully instead.
    pub fn load_from(base: &Path) -> anyhow::Result<Self> {
        let path = base.join(Self::STATE_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut state: Self = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Self::migrate(&mut state);
        Ok(state)
    }

    /// Like [`Self::load_from`] but never aborts on a corrupt file: it backs the
    /// bad file up and resets to default, returning a warning to surface (mirroring
    /// how a bad `config.json` degrades). For the TUI's startup load.
    pub fn load_checked_from(base: &Path) -> anyhow::Result<(Self, Option<String>)> {
        let path = base.join(Self::STATE_FILE);
        if !path.exists() {
            return Ok((Self::default(), None));
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        match serde_json::from_str::<Self>(&data) {
            Ok(mut state) => {
                Self::migrate(&mut state);
                Ok((state, None))
            }
            Err(e) => {
                // Back up the bad file without clobbering an earlier backup — the
                // first/original is the most valuable one to keep.
                let mut backup = path.with_file_name(format!("{}.corrupt", Self::STATE_FILE));
                if backup.exists() {
                    backup = path
                        .with_file_name(format!("{}.corrupt.{}", Self::STATE_FILE, generate_id()));
                }
                let _ = fs::rename(&path, &backup);
                Ok((
                    Self::default(),
                    Some(format!(
                        "ignoring corrupt state ({e}); backed up to {} and reset",
                        backup.display()
                    )),
                ))
            }
        }
    }

    /// Save state to the given base directory, creating it if needed. The write
    /// is atomic — a uniquely-named temp file in the same dir is renamed over
    /// `state.json` — so a process crash mid-write can't leave a partially-written
    /// state file. (Power-loss corruption is caught by the corrupt-load backstop.)
    pub fn save_to(&self, base: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(base)
            .with_context(|| format!("failed to create {}", base.display()))?;
        let path = base.join(Self::STATE_FILE);
        let data = serde_json::to_string_pretty(self)?;
        // Unique temp name so concurrent saves (or parallel tests sharing a dir)
        // never collide on the same temp file mid-rename.
        let tmp = path.with_file_name(format!("{}.tmp.{}", Self::STATE_FILE, generate_id()));
        fs::write(&tmp, &data)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
    }

    /// Load state from the default state directory.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(Self::state_dir().as_path())
    }

    /// Like [`Self::load`] but also returns a warning for a corrupt state file.
    pub fn load_checked() -> anyhow::Result<(Self, Option<String>)> {
        Self::load_checked_from(Self::state_dir().as_path())
    }

    /// Save state to the default state directory.
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(Self::state_dir().as_path())
    }

    /// Reload the on-disk state, merge ours over it relative to `baseline`, and
    /// write the result atomically — so a `kmd` command that changed `state.json`
    /// while the TUI was open isn't clobbered on the next save. Best-effort: if
    /// the on-disk file can't be reloaded (corrupt), write ours rather than fail.
    pub fn merge_save_to(&self, baseline: &AppState, base: &Path) -> anyhow::Result<()> {
        let merged = match Self::load_from(base) {
            Ok(disk) => self.merged_over(baseline, disk),
            Err(_) => self.clone(),
        };
        merged.save_to(base)
    }

    /// [`merge_save_to`](Self::merge_save_to) against the default state dir.
    pub fn merge_save(&self, baseline: &AppState) -> anyhow::Result<()> {
        self.merge_save_to(baseline, Self::state_dir().as_path())
    }

    /// 3-way merge: this in-memory state ("mine") over the current `disk` state,
    /// relative to the `baseline` we loaded. Start from `disk` (so entries a
    /// concurrent CLI added survive), drop entries we deleted since `baseline`,
    /// and let our copy win for entries we still hold ("mine wins on conflict").
    /// We do NOT adopt the result in memory — it only protects the on-disk file
    /// from being overwritten; CLI-added rows appear in the TUI on its next load.
    ///
    /// Scope: this protects concurrent CLI *adds* and *deletes*. A CLI *in-place
    /// edit* to a row the TUI also holds is still lost on the TUI's next save —
    /// there's no per-field/version comparison, and "mine wins" overwrites it.
    pub fn merged_over(&self, baseline: &AppState, disk: AppState) -> AppState {
        use std::collections::HashSet;

        // Keep disk entries we neither hold nor deleted-since-baseline, then
        // append ours (so ours wins for ids present in both).
        fn merge_vec<T: Clone>(
            mine: &[T],
            baseline: &[T],
            disk: Vec<T>,
            id: impl Fn(&T) -> &str,
        ) -> Vec<T> {
            let mine_ids: HashSet<&str> = mine.iter().map(&id).collect();
            let deleted: HashSet<&str> =
                baseline.iter().map(&id).filter(|i| !mine_ids.contains(i)).collect();
            let mut out: Vec<T> = disk
                .into_iter()
                .filter(|d| {
                    let i = id(d);
                    !mine_ids.contains(i) && !deleted.contains(i)
                })
                .collect();
            out.extend(mine.iter().cloned());
            out
        }

        fn merge_map<V: Clone>(
            mine: &HashMap<String, V>,
            baseline: &HashMap<String, V>,
            disk: HashMap<String, V>,
        ) -> HashMap<String, V> {
            let deleted: HashSet<&str> =
                baseline.keys().map(|k| k.as_str()).filter(|k| !mine.contains_key(*k)).collect();
            let mut out: HashMap<String, V> = disk
                .into_iter()
                .filter(|(k, _)| !mine.contains_key(k) && !deleted.contains(k.as_str()))
                .collect();
            for (k, v) in mine {
                out.insert(k.clone(), v.clone());
            }
            out
        }

        AppState {
            version: Self::STATE_VERSION,
            repos: merge_vec(&self.repos, &baseline.repos, disk.repos, |r| r.id.as_str()),
            workspaces: merge_vec(&self.workspaces, &baseline.workspaces, disk.workspaces, |w| {
                w.id.as_str()
            }),
            sessions: merge_vec(&self.sessions, &baseline.sessions, disk.sessions, |s| s.id.as_str()),
            embedded_sessions: merge_map(
                &self.embedded_sessions,
                &baseline.embedded_sessions,
                disk.embedded_sessions,
            ),
            embedded_titles: merge_map(
                &self.embedded_titles,
                &baseline.embedded_titles,
                disk.embedded_titles,
            ),
        }
    }

    /// A fresh Claude session id (UUID v4) to assign to a new embedded session.
    pub fn new_claude_session_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// The stored Claude session ids for a workspace's session tabs, in tab
    /// order (empty slice when none).
    pub fn embedded_session_ids(&self, workspace_id: &str) -> &[String] {
        self.embedded_sessions
            .get(workspace_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Append a Claude session id for a workspace (idempotent — no duplicates).
    pub fn add_embedded_session(&mut self, workspace_id: &str, session_id: &str) {
        let ids = self.embedded_sessions.entry(workspace_id.to_string()).or_default();
        if !ids.iter().any(|id| id == session_id) {
            ids.push(session_id.to_string());
        }
    }

    /// Forget a single session id for a workspace (its tab was closed, or its
    /// resume failed because the Claude session was purged). Removes the
    /// workspace entry when its last id is gone. Preserves the order of the rest.
    pub fn remove_embedded_session(&mut self, workspace_id: &str, session_id: &str) {
        if let Some(ids) = self.embedded_sessions.get_mut(workspace_id) {
            ids.retain(|id| id != session_id);
            if ids.is_empty() {
                self.embedded_sessions.remove(workspace_id);
            }
        }
        // Keep titles in lockstep so a renamed-then-closed session leaves nothing
        // behind (a title must never outlive its session id).
        self.set_embedded_session_title(workspace_id, session_id, "");
    }

    /// Forget all of a workspace's session ids (on workspace/repo delete).
    pub fn clear_all_embedded_sessions(&mut self, workspace_id: &str) {
        self.embedded_sessions.remove(workspace_id);
        self.embedded_titles.remove(workspace_id);
    }

    /// Drop embedded-session entries (ids and titles) for workspaces that no
    /// longer exist.
    pub fn prune_embedded_sessions(&mut self) {
        self.embedded_sessions
            .retain(|ws_id, _| self.workspaces.iter().any(|w| &w.id == ws_id));
        self.embedded_titles
            .retain(|ws_id, _| self.workspaces.iter().any(|w| &w.id == ws_id));
    }

    /// The display title for a session tab, if the user has renamed it.
    pub fn embedded_session_title(&self, workspace_id: &str, session_id: &str) -> Option<&str> {
        self.embedded_titles
            .get(workspace_id)
            .and_then(|m| m.get(session_id))
            .map(String::as_str)
    }

    /// Set (or, with an empty `title`, clear) a session tab's display title.
    /// Drops the workspace's title map once its last title is cleared so the
    /// field stays absent from serialized state in the common case.
    pub fn set_embedded_session_title(&mut self, workspace_id: &str, session_id: &str, title: &str) {
        if title.is_empty() {
            if let Some(m) = self.embedded_titles.get_mut(workspace_id) {
                m.remove(session_id);
                if m.is_empty() {
                    self.embedded_titles.remove(workspace_id);
                }
            }
        } else {
            self.embedded_titles
                .entry(workspace_id.to_string())
                .or_default()
                .insert(session_id.to_string(), title.to_string());
        }
    }

    /// Add a repo, saving state to a custom base directory.
    pub fn add_repo_with_base(&mut self, path: &str, base: &Path) -> anyhow::Result<RepoEntry> {
        let dir = Path::new(path);
        if !dir.is_dir() {
            bail!("path does not exist or is not a directory: {path}");
        }

        let canonical = fs::canonicalize(dir)
            .with_context(|| format!("failed to canonicalize {path}"))?;
        let canonical_str = canonical.to_string_lossy().to_string();

        if self.repos.iter().any(|r| r.path == canonical_str) {
            bail!("repo already tracked: {canonical_str}");
        }

        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| canonical_str.clone());

        let id = generate_id();

        let entry = RepoEntry {
            id,
            name,
            path: canonical_str,
        };

        self.repos.push(entry.clone());
        self.save_to(base)?;
        Ok(entry)
    }

    /// Add a repo, saving state to the default state directory.
    pub fn add_repo(&mut self, path: &str) -> anyhow::Result<RepoEntry> {
        self.add_repo_with_base(path, Self::state_dir().as_path())
    }

    /// Delete a repo by reference (name, path, or ID), cascading to workspaces and sessions.
    ///
    /// Removes all workspaces (and their worktrees) and sessions belonging to the repo.
    pub fn delete_repo_with_base(
        &mut self,
        repo_ref: &str,
        base: &Path,
    ) -> anyhow::Result<RepoEntry> {
        let repo = self.resolve_repo(repo_ref)?.clone();

        // Collect workspace IDs for this repo
        let ws_ids: Vec<String> = self
            .workspaces
            .iter()
            .filter(|w| w.repo_id == repo.id)
            .map(|w| w.id.clone())
            .collect();

        // Remove sessions for these workspaces and clean up log files
        self.sessions.retain(|s| {
            if ws_ids.contains(&s.workspace_id) {
                let log_path = Path::new(&s.log_file);
                if log_path.exists() {
                    let _ = fs::remove_file(log_path);
                }
                false
            } else {
                true
            }
        });

        // Remove workspaces and their worktrees
        self.workspaces.retain(|w| {
            if w.repo_id == repo.id {
                if let Some(wt_path) = &w.worktree_path {
                    let _ = worktree::remove_worktree(&repo.path, wt_path);
                }
                false
            } else {
                true
            }
        });

        // Forget any embedded Claude session ids (and titles) for the removed
        // workspaces.
        for id in &ws_ids {
            self.embedded_sessions.remove(id);
            self.embedded_titles.remove(id);
        }

        // Remove the repo itself
        self.repos.retain(|r| r.id != repo.id);

        self.save_to(base)?;
        Ok(repo)
    }

    /// Delete a repo, saving state to the default state directory.
    pub fn delete_repo(&mut self, repo_ref: &str) -> anyhow::Result<RepoEntry> {
        self.delete_repo_with_base(repo_ref, Self::state_dir().as_path())
    }

    // --- Workspace methods ---

    /// Resolve a repo reference by name, path, or ID.
    pub fn resolve_repo(&self, reference: &str) -> anyhow::Result<&RepoEntry> {
        // Path-first if input contains '/'
        if reference.contains('/') {
            // Try path match (canonicalize if possible)
            let canonical = fs::canonicalize(reference)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| reference.to_string());
            if let Some(repo) = self.repos.iter().find(|r| r.path == canonical || r.path == reference) {
                return Ok(repo);
            }
            // Fall through to name/id
        }

        // Name match
        if let Some(repo) = self.repos.iter().find(|r| r.name == reference) {
            return Ok(repo);
        }

        // Path match (for non-slash inputs that happen to match)
        if let Some(repo) = self.repos.iter().find(|r| r.path == reference) {
            return Ok(repo);
        }

        // ID match
        if let Some(repo) = self.repos.iter().find(|r| r.id == reference) {
            return Ok(repo);
        }

        bail!(
            "No repo found matching '{reference}'. Checked: name, path, id. Use `kmd repo list` to see tracked repos."
        )
    }

    /// Create a workspace with a custom base directory for state persistence.
    ///
    /// By default, creates a git worktree for the workspace. Set `use_worktree` to
    /// `false` to skip worktree creation and use the repo root as working directory.
    pub fn create_workspace_with_base(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_impl(name, repo_ref, base, true, None)
    }

    /// Create a workspace, optionally skipping worktree creation.
    pub fn create_workspace_with_options(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
        use_worktree: bool,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_impl(name, repo_ref, base, use_worktree, None)
    }

    /// Create a workspace whose worktree checks out an EXISTING branch (local or
    /// a remote `origin/…` ref) instead of forking a new one. The name defaults
    /// to a path-safe form of the branch when omitted.
    pub fn create_workspace_from_branch(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        branch: &str,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_from_branch_with_base(name, repo_ref, Self::state_dir().as_path(), branch)
    }

    /// [`create_workspace_from_branch`](Self::create_workspace_from_branch) with an
    /// explicit base dir (for tests).
    pub fn create_workspace_from_branch_with_base(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
        branch: &str,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_impl(name, repo_ref, base, true, Some(branch))
    }

    /// Validate a would-be workspace name: it becomes a path segment under
    /// `worktrees/` and the branch name itself, so reject anything that could
    /// escape the dir or trip git's arg parsing (empty/whitespace, `.`/`..`, a
    /// path separator, a leading dash), the two names git refuses as bare
    /// branches (`HEAD`, `@` — `git worktree add -b` would fail and silently
    /// fall back to the repo root), and a name already in use.
    pub fn validate_new_workspace_name(&self, name: &str) -> anyhow::Result<()> {
        if name.trim().is_empty()
            || name == "."
            || name == ".."
            || name == "HEAD"
            || name == "@"
            || name.contains('/')
            || name.contains('\\')
            || name.starts_with('-')
        {
            bail!(
                "invalid workspace name {name:?}: must not be empty, '.'/'..', \
                 'HEAD', '@', contain a path separator, or start with '-'"
            );
        }
        if self.workspaces.iter().any(|w| w.name == name) {
            bail!("workspace already exists: {name}");
        }
        Ok(())
    }

    fn create_workspace_impl(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
        use_worktree: bool,
        from_branch: Option<&str>,
    ) -> anyhow::Result<Workspace> {
        let repo = self.resolve_repo(repo_ref)?.clone();

        let ws_name = match name {
            Some(n) => n.to_string(),
            // Default the name from the branch when checking one out (path-safe:
            // drop a leading `origin/`, then `/` -> `-`), else from the repo.
            None => match from_branch {
                Some(b) => b.strip_prefix("origin/").unwrap_or(b).replace('/', "-"),
                None => repo.name.clone(),
            },
        };

        self.validate_new_workspace_name(&ws_name)?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();

        // Create a git worktree: on an existing branch if one was requested, else
        // on a fresh branch named after the workspace.
        let (working_dir, worktree_path, branch_name) = if !use_worktree {
            (repo.path.clone(), None, None)
        } else if let Some(branch_ref) = from_branch {
            match worktree::create_worktree_from_branch(&repo.path, &ws_name, base, branch_ref) {
                worktree::WorktreeResult::Created { worktree_path, branch_name } => {
                    (worktree_path.clone(), Some(worktree_path), Some(branch_name))
                }
                // An explicit branch request that can't be satisfied is an error,
                // not a silent fall back to the repo root.
                worktree::WorktreeResult::Fallback { reason } => {
                    bail!("couldn't check out branch {branch_ref:?}: {reason}");
                }
            }
        } else {
            match worktree::create_worktree(&repo.path, &ws_name, base) {
                worktree::WorktreeResult::Created { worktree_path, branch_name } => {
                    (worktree_path.clone(), Some(worktree_path), Some(branch_name))
                }
                worktree::WorktreeResult::Fallback { reason: _ } => (repo.path.clone(), None, None),
            }
        };

        let ws = Workspace {
            id: generate_id(),
            name: ws_name,
            repo_id: repo.id.clone(),
            working_dir,
            active: true,
            created_at: now,
            worktree_path,
            branch_name,
        };

        self.workspaces.push(ws.clone());
        self.save_to(base)?;
        Ok(ws)
    }

    /// Create a workspace, saving state to the default state directory.
    pub fn create_workspace(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_with_base(name, repo_ref, Self::state_dir().as_path())
    }

    /// List workspaces, optionally showing all (including archived) and filtering by repo.
    pub fn list_workspaces(
        &self,
        all: bool,
        repo_ref: Option<&str>,
    ) -> anyhow::Result<Vec<&Workspace>> {
        let repo_id = match repo_ref {
            Some(r) => Some(self.resolve_repo(r)?.id.clone()),
            None => None,
        };

        let result: Vec<&Workspace> = self
            .workspaces
            .iter()
            .filter(|w| all || w.active)
            .filter(|w| match &repo_id {
                Some(rid) => w.repo_id == *rid,
                None => true,
            })
            .collect();

        Ok(result)
    }

    /// Show a workspace by name.
    pub fn show_workspace(&self, name: &str) -> anyhow::Result<&Workspace> {
        self.workspaces
            .iter()
            .find(|w| w.name == name)
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {name}"))
    }

    /// Delete a workspace by name, saving to custom base directory.
    /// Also removes the git worktree if one was created.
    pub fn delete_workspace_with_base(
        &mut self,
        name: &str,
        base: &Path,
    ) -> anyhow::Result<Workspace> {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.name == name)
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {name}"))?;
        let removed = self.workspaces.remove(idx);
        self.embedded_sessions.remove(&removed.id);
        self.embedded_titles.remove(&removed.id);

        // Clean up worktree if present
        if let Some(wt_path) = &removed.worktree_path
            && let Some(repo) = self.repos.iter().find(|r| r.id == removed.repo_id)
        {
            let _ = worktree::remove_worktree(&repo.path, wt_path);
        }

        self.save_to(base)?;
        Ok(removed)
    }

    /// Delete a workspace by name, saving to the default state directory.
    pub fn delete_workspace(&mut self, name: &str) -> anyhow::Result<Workspace> {
        self.delete_workspace_with_base(name, Self::state_dir().as_path())
    }

    /// Archive a workspace (set active=false) with custom base directory.
    pub fn archive_workspace_with_base(&mut self, name: &str, base: &Path) -> anyhow::Result<()> {
        let ws = self
            .workspaces
            .iter_mut()
            .find(|w| w.name == name)
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {name}"))?;
        ws.active = false;
        self.save_to(base)?;
        Ok(())
    }

    /// Archive a workspace (set active=false).
    pub fn archive_workspace(&mut self, name: &str) -> anyhow::Result<()> {
        self.archive_workspace_with_base(name, Self::state_dir().as_path())
    }

    /// Activate a workspace (set active=true) with custom base directory.
    pub fn activate_workspace_with_base(&mut self, name: &str, base: &Path) -> anyhow::Result<()> {
        let ws = self
            .workspaces
            .iter_mut()
            .find(|w| w.name == name)
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {name}"))?;
        ws.active = true;
        self.save_to(base)?;
        Ok(())
    }

    /// Activate a workspace (set active=true).
    pub fn activate_workspace(&mut self, name: &str) -> anyhow::Result<()> {
        self.activate_workspace_with_base(name, Self::state_dir().as_path())
    }

    // --- Session methods ---

    /// Create a session for a workspace, saving state to a custom base directory.
    pub fn create_session_with_base(
        &mut self,
        workspace_id: &str,
        base: &Path,
    ) -> anyhow::Result<Session> {
        // Validate workspace exists
        if !self.workspaces.iter().any(|w| w.id == workspace_id) {
            bail!("workspace not found: {workspace_id}");
        }

        // Check no running session for this workspace
        if self
            .sessions
            .iter()
            .any(|s| s.workspace_id == workspace_id && s.status == SessionStatus::Running)
        {
            bail!(
                "workspace {workspace_id} already has a running session"
            );
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();

        let session = Session {
            log_file: base
                .join("sessions")
                .join(format!("{id}.log"))
                .to_string_lossy()
                .into_owned(),
            id,
            workspace_id: workspace_id.to_string(),
            claude_session_id: None,
            pid: None,
            status: SessionStatus::Running,
            created_at: now,
            ended_at: None,
        };

        self.sessions.push(session.clone());
        self.save_to(base)?;
        Ok(session)
    }

    /// Create a session, saving state to the default state directory.
    pub fn create_session(&mut self, workspace_id: &str) -> anyhow::Result<Session> {
        self.create_session_with_base(workspace_id, Self::state_dir().as_path())
    }

    /// Find the most recent session for a workspace.
    pub fn find_session_by_workspace(&self, workspace_id: &str) -> Option<&Session> {
        self.sessions
            .iter()
            .rev()
            .find(|s| s.workspace_id == workspace_id)
    }

    /// Find a session by ID (mutable).
    pub fn find_session_mut(&mut self, session_id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == session_id)
    }

    /// Update session status, saving state to a custom base directory.
    /// Sets ended_at for terminal states (Stopped, Failed, Exited).
    pub fn update_session_status_with_base(
        &mut self,
        session_id: &str,
        status: SessionStatus,
        base: &Path,
    ) -> anyhow::Result<()> {
        let session = self
            .sessions
            .iter_mut()
            .find(|s| s.id == session_id)
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;

        session.status = status.clone();

        match status {
            SessionStatus::Stopped | SessionStatus::Failed | SessionStatus::Exited => {
                session.ended_at = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("time went backwards")
                        .as_secs(),
                );
            }
            SessionStatus::Running => {}
        }

        self.save_to(base)?;
        Ok(())
    }

    /// Update session status, saving state to the default state directory.
    pub fn update_session_status(
        &mut self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        self.update_session_status_with_base(session_id, status, Self::state_dir().as_path())
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> &[Session] {
        &self.sessions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws(id: &str, name: &str) -> Workspace {
        Workspace {
            id: id.into(),
            name: name.into(),
            repo_id: "r".into(),
            working_dir: "/tmp".into(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: None,
        }
    }

    #[test]
    fn merge_preserves_cli_adds_and_respects_tui_deletes() {
        // baseline = what the TUI loaded; mine = TUI's in-memory state (deleted B,
        // added C); disk = the file now (a CLI added D; B not yet deleted on disk).
        let baseline = AppState { workspaces: vec![ws("A", "a"), ws("B", "b")], ..Default::default() };
        let mine = AppState { workspaces: vec![ws("A", "a"), ws("C", "c")], ..Default::default() };
        let disk =
            AppState { workspaces: vec![ws("A", "a"), ws("B", "b"), ws("D", "d")], ..Default::default() };

        let merged = mine.merged_over(&baseline, disk);
        let ids: std::collections::HashSet<&str> =
            merged.workspaces.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains("A"), "untouched entry kept");
        assert!(!ids.contains("B"), "a TUI delete wins over the disk copy");
        assert!(ids.contains("C"), "a TUI add is present");
        assert!(ids.contains("D"), "a concurrent CLI add survives");
        assert_eq!(merged.workspaces.len(), 3);
    }

    #[test]
    fn merge_lets_mine_win_on_a_conflicting_edit() {
        let baseline = AppState { workspaces: vec![ws("A", "v1")], ..Default::default() };
        let mine = AppState { workspaces: vec![ws("A", "mine")], ..Default::default() };
        let disk = AppState { workspaces: vec![ws("A", "disk")], ..Default::default() };
        let merged = mine.merged_over(&baseline, disk);
        assert_eq!(merged.workspaces.len(), 1);
        assert_eq!(merged.workspaces[0].name, "mine", "in-memory copy wins for a shared id");
    }

    #[test]
    fn merge_applies_the_same_rules_to_maps() {
        let mk = |pairs: &[(&str, &str)]| -> HashMap<String, Vec<String>> {
            pairs.iter().map(|(k, v)| (k.to_string(), vec![v.to_string()])).collect()
        };
        let baseline = AppState { embedded_sessions: mk(&[("A", "a"), ("B", "b")]), ..Default::default() };
        // TUI deleted B and re-tabbed A; a CLI added C.
        let mine = AppState { embedded_sessions: mk(&[("A", "a2")]), ..Default::default() };
        let disk =
            AppState { embedded_sessions: mk(&[("A", "a"), ("B", "b"), ("C", "c")]), ..Default::default() };
        let merged = mine.merged_over(&baseline, disk);
        assert_eq!(merged.embedded_sessions.get("A").unwrap(), &vec!["a2".to_string()], "mine wins");
        assert!(!merged.embedded_sessions.contains_key("B"), "TUI delete respected");
        assert!(merged.embedded_sessions.contains_key("C"), "CLI add preserved");
    }

    #[test]
    fn merge_save_reloads_and_preserves_a_concurrent_add() {
        let tmp = TempDir::new().unwrap();
        // Disk already has a workspace the TUI never saw (a CLI added it).
        let disk = AppState { workspaces: vec![ws("cli", "from-cli")], ..Default::default() };
        disk.save_to(tmp.path()).unwrap();
        // The TUI loaded an empty baseline, created its own workspace, and saves.
        let baseline = AppState::default();
        let mine = AppState { workspaces: vec![ws("tui", "from-tui")], ..Default::default() };
        mine.merge_save_to(&baseline, tmp.path()).unwrap();
        let on_disk = AppState::load_from(tmp.path()).unwrap();
        let ids: std::collections::HashSet<&str> =
            on_disk.workspaces.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains("cli") && ids.contains("tui"), "both survive the save: {ids:?}");
    }

    #[test]
    fn create_workspace_from_branch_defaults_name_and_adopts_the_branch() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&repo_dir).output().unwrap()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "--allow-empty", "-m", "init"]);
        git(&["branch", "feat/login"]);

        let base = tmp.path().join("base");
        let mut state = AppState {
            repos: vec![RepoEntry {
                id: "r".into(),
                name: "repo".into(),
                path: repo_dir.to_string_lossy().into_owned(),
            }],
            ..Default::default()
        };

        let ws = state.create_workspace_from_branch_with_base(None, "r", &base, "feat/login").unwrap();
        assert_eq!(ws.name, "feat-login", "name defaults to a path-safe form of the branch");
        assert_eq!(
            ws.branch_name.as_deref(),
            Some("feat/login"),
            "adopts the existing branch as-is (no fork)"
        );
        assert!(ws.worktree_path.is_some());

        // A branch that doesn't exist is an error, not a silent repo-root fallback.
        let err = state.create_workspace_from_branch_with_base(None, "r", &base, "ghost").unwrap_err();
        assert!(err.to_string().contains("couldn't check out branch"), "got: {err}");
    }

    #[test]
    fn load_from_returns_default_when_no_file() {
        let tmp = TempDir::new().unwrap();
        let state = AppState::load_from(tmp.path()).unwrap();
        assert!(state.repos.is_empty());
    }

    #[test]
    fn save_to_load_from_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.repos.push(RepoEntry {
            id: "abc123".to_string(),
            name: "my-repo".to_string(),
            path: "/tmp/my-repo".to_string(),
        });
        state.save_to(tmp.path()).unwrap();

        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.repos.len(), 1);
        assert_eq!(loaded.repos[0].id, "abc123");
        assert_eq!(loaded.repos[0].name, "my-repo");
        assert_eq!(loaded.repos[0].path, "/tmp/my-repo");
    }

    #[test]
    fn embedded_sessions_add_get_clear_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        assert!(state.embedded_session_ids("w1").is_empty());

        let id = AppState::new_claude_session_id();
        state.add_embedded_session("w1", &id);
        assert_eq!(state.embedded_session_ids("w1"), std::slice::from_ref(&id));
        state.save_to(tmp.path()).unwrap();

        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.embedded_session_ids("w1"), std::slice::from_ref(&id));

        state.clear_all_embedded_sessions("w1");
        assert!(state.embedded_session_ids("w1").is_empty());
    }

    #[test]
    fn new_claude_session_id_is_a_unique_uuid() {
        let a = AppState::new_claude_session_id();
        let b = AppState::new_claude_session_id();
        assert_ne!(a, b);
        assert!(uuid::Uuid::parse_str(&a).is_ok(), "should be a valid UUID: {a}");
    }

    #[test]
    fn embedded_sessions_multiple_per_workspace_in_order() {
        let mut state = AppState::default();
        state.add_embedded_session("w1", "a");
        state.add_embedded_session("w1", "b");
        state.add_embedded_session("w1", "a"); // idempotent
        assert_eq!(state.embedded_session_ids("w1"), &["a".to_string(), "b".to_string()]);
        assert_eq!(state.embedded_session_ids("missing"), &[] as &[String]);

        // Removing a middle id preserves the order of the rest.
        state.add_embedded_session("w1", "c");
        state.remove_embedded_session("w1", "b");
        assert_eq!(state.embedded_session_ids("w1"), &["a".to_string(), "c".to_string()]);

        // Removing the last id drops the workspace entry entirely.
        state.remove_embedded_session("w1", "a");
        state.remove_embedded_session("w1", "c");
        assert!(!state.embedded_sessions.contains_key("w1"));
    }

    #[test]
    fn embedded_sessions_serialize_as_array_and_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.add_embedded_session("w1", "a");
        state.add_embedded_session("w1", "b");
        state.save_to(tmp.path()).unwrap();

        // On disk the field is the new array shape.
        let raw = std::fs::read_to_string(tmp.path().join("state.json")).unwrap();
        assert!(raw.contains("\"w1\""));
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v["embedded_sessions"]["w1"].is_array());

        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.embedded_session_ids("w1"), &["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn embedded_sessions_reads_legacy_single_string_form() {
        // A state.json written before tabs stored one id per workspace as a
        // string; it must load as a one-element Vec.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[],"embedded_sessions":{"w1":"legacy-id"}}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.embedded_session_ids("w1"), &["legacy-id".to_string()]);
    }

    #[test]
    fn state_without_version_loads_as_current_and_restamps() {
        // A state.json written before schema versioning has no `version` field —
        // it must still load (treated as v1) and re-save stamped with the current
        // schema version, so an upgrade never bricks an existing file.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[]}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(
            loaded.version,
            AppState::STATE_VERSION,
            "a versionless file loads at the current schema"
        );

        loaded.save_to(tmp.path()).unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("state.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["version"], AppState::STATE_VERSION, "save stamps the schema version");
    }

    #[test]
    fn fresh_state_carries_the_current_version() {
        // A brand-new state is at the current schema (not 0 from a derived Default).
        assert_eq!(AppState::default().version, AppState::STATE_VERSION);
    }

    #[test]
    fn embedded_sessions_tolerates_explicit_null() {
        // The field has been observed written as `null`; it must load as empty.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[],"embedded_sessions":null}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert!(loaded.embedded_sessions.is_empty());
    }

    #[test]
    fn embedded_sessions_tolerates_per_entry_null_and_empty() {
        // A null or empty value for a single workspace must not abort the whole
        // load (it just contributes no sessions).
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[],"embedded_sessions":{"w1":null,"w2":[],"w3":["keep"]}}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.embedded_session_ids("w1"), &[] as &[String]);
        assert_eq!(loaded.embedded_session_ids("w2"), &[] as &[String]);
        assert_eq!(loaded.embedded_session_ids("w3"), &["keep".to_string()]);
    }

    #[test]
    fn prune_embedded_sessions_drops_orphans() {
        let mut state = AppState::default();
        state.workspaces.push(Workspace {
            id: "w1".to_string(),
            name: "ws".to_string(),
            repo_id: "r1".to_string(),
            working_dir: "/tmp/ws".to_string(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: None,
        });
        state.add_embedded_session("w1", "keep");
        state.add_embedded_session("w-gone", "drop");
        state.prune_embedded_sessions();
        assert_eq!(state.embedded_session_ids("w1"), &["keep".to_string()]);
        assert!(state.embedded_session_ids("w-gone").is_empty());
    }

    #[test]
    fn load_from_tolerates_state_without_embedded_sessions() {
        // Backward compat: a state.json written before this field must still load.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[]}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert!(loaded.embedded_sessions.is_empty());
    }

    #[test]
    fn embedded_session_title_set_get_clear_and_persist() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.add_embedded_session("w1", "a");
        assert_eq!(state.embedded_session_title("w1", "a"), None);

        state.set_embedded_session_title("w1", "a", "auth refactor");
        assert_eq!(state.embedded_session_title("w1", "a"), Some("auth refactor"));
        state.save_to(tmp.path()).unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.embedded_session_title("w1", "a"), Some("auth refactor"));

        // Clearing drops the entry and the (now-empty) field disappears on disk.
        state.set_embedded_session_title("w1", "a", "");
        assert_eq!(state.embedded_session_title("w1", "a"), None);
        assert!(state.embedded_titles.is_empty());
        state.save_to(tmp.path()).unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("state.json")).unwrap();
        assert!(!raw.contains("embedded_titles"), "empty title map is omitted: {raw}");
    }

    #[test]
    fn embedded_titles_never_outlive_their_session() {
        // The titles map must stay a subset of the session ids through every
        // removal path (close, workspace delete, prune).
        let mut state = AppState::default();
        state.workspaces.push(Workspace {
            id: "w1".to_string(),
            name: "ws".to_string(),
            repo_id: "r1".to_string(),
            working_dir: "/tmp/ws".to_string(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: None,
        });
        state.add_embedded_session("w1", "a");
        state.add_embedded_session("w1", "b");
        state.set_embedded_session_title("w1", "a", "alpha");
        state.set_embedded_session_title("w1", "b", "beta");

        // Closing one tab forgets only its title.
        state.remove_embedded_session("w1", "a");
        assert_eq!(state.embedded_session_title("w1", "a"), None);
        assert_eq!(state.embedded_session_title("w1", "b"), Some("beta"));

        // Pruning an orphan workspace drops its titles too.
        state.add_embedded_session("w-gone", "z");
        state.set_embedded_session_title("w-gone", "z", "zed");
        state.prune_embedded_sessions();
        assert!(!state.embedded_titles.contains_key("w-gone"));

        // Clearing a whole workspace drops its titles.
        state.clear_all_embedded_sessions("w1");
        assert!(!state.embedded_titles.contains_key("w1"));
        assert!(state.embedded_titles.is_empty());
    }

    #[test]
    fn load_tolerates_state_without_embedded_titles() {
        // Back-compat: a state.json written before titles existed must load.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[],"embedded_sessions":{"w1":["a"]}}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert!(loaded.embedded_titles.is_empty());
        assert_eq!(loaded.embedded_session_title("w1", "a"), None);
    }

    #[test]
    fn corrupt_state_degrades_to_default_and_backs_up() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        std::fs::write(&path, "{ not valid json").unwrap();

        let (state, warn) = AppState::load_checked_from(tmp.path()).unwrap();
        // load_checked_from degrades to default instead of aborting startup.
        assert!(state.repos.is_empty() && state.workspaces.is_empty());
        // Warns, and the bad file is preserved (not silently lost).
        assert!(warn.is_some_and(|w| w.contains("corrupt state")), "warns on a corrupt file");
        assert!(tmp.path().join("state.json.corrupt").exists(), "bad file backed up");
        assert!(!path.exists(), "corrupt state.json moved aside");

        // A SECOND corruption must not clobber the first/original backup.
        std::fs::write(&path, "garbage again").unwrap();
        let (_, warn3) = AppState::load_checked_from(tmp.path()).unwrap();
        assert!(warn3.is_some());
        assert!(tmp.path().join("state.json.corrupt").exists(), "original backup kept");
        let extra_backups = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with("state.json.corrupt") && n != "state.json.corrupt"
            })
            .count();
        assert_eq!(extra_backups, 1, "second corruption backed up under a distinct name");

        // A missing file is a silent default (no warning).
        let tmp2 = TempDir::new().unwrap();
        let (_, warn2) = AppState::load_checked_from(tmp2.path()).unwrap();
        assert!(warn2.is_none());
    }

    #[test]
    fn load_from_errors_on_corrupt_rather_than_resetting() {
        // The CLI path must abort (leaving the file recoverable), not silently
        // reset — only the TUI's load_checked_from degrades.
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("state.json"), "{ not valid json").unwrap();
        assert!(AppState::load_from(tmp.path()).is_err());
        assert!(tmp.path().join("state.json").exists(), "corrupt file left untouched");
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.repos.push(RepoEntry {
            id: "r1".into(),
            name: "x".into(),
            path: "/tmp/x".into(),
        });
        state.save_to(tmp.path()).unwrap();
        state.save_to(tmp.path()).unwrap(); // a second save must not leave temp debris
        assert!(tmp.path().join("state.json").exists());
        let temp_leftovers = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("state.json.tmp"))
            .count();
        assert_eq!(temp_leftovers, 0, "no temp file left after rename");
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.repos.len(), 1);
    }

    #[test]
    fn add_repo_rejects_nonexistent_path() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        let result = state.add_repo_with_base("/nonexistent/path/xyz", tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist") || err.contains("not a directory"));
    }

    #[test]
    fn add_repo_rejects_duplicate_path() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = TempDir::new().unwrap();
        let mut state = AppState::default();

        state.add_repo_with_base(repo_dir.path().to_str().unwrap(), tmp.path()).unwrap();

        let result = state.add_repo_with_base(repo_dir.path().to_str().unwrap(), tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already tracked"));
    }

    #[test]
    fn add_repo_canonicalizes_path_and_derives_name() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = TempDir::new().unwrap();
        let mut state = AppState::default();
        let entry = state.add_repo_with_base(repo_dir.path().to_str().unwrap(), tmp.path()).unwrap();

        let expected_name = repo_dir.path().file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(entry.name, expected_name);
        assert!(entry.path.starts_with('/'));
    }

    #[test]
    fn run_git_status_errors_on_nonexistent_path() {
        let result = run_git_status("/nonexistent/path/xyz");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn run_git_status_errors_on_file_not_directory() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("not-a-dir.txt");
        fs::write(&file_path, "hello").unwrap();
        let result = run_git_status(file_path.to_str().unwrap());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not a directory"));
    }

    #[test]
    fn run_git_status_errors_on_non_git_directory() {
        let tmp = TempDir::new().unwrap();
        let result = run_git_status(tmp.path().to_str().unwrap());
        assert!(result.is_err());
    }

    // NOTE: no test in this module ever calls `set_profile` — the TUI test
    // harness sets KOMMAND0_STATE_DIR process-wide, and PROFILE is a
    // process-global OnceLock; the set_profile/state_dir glue is covered by
    // the process-level CLI and e2e tests instead.

    #[test]
    fn validate_profile_name_accepts_and_rejects() {
        let max_len = "a".repeat(64);
        for ok in ["work", "a.B-2_x", max_len.as_str()] {
            assert!(AppState::validate_profile_name(ok).is_ok(), "{ok:?} should be valid");
        }
        let too_long = "a".repeat(65);
        for bad in ["", ".", "..", "a/b", "a b", "café", too_long.as_str()] {
            let err = AppState::validate_profile_name(bad).unwrap_err();
            assert!(err.contains("invalid profile name"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn migrate_legacy_moves_state_and_config_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        let dir = tmp.path().join("profiles").join("default");
        assert!(dir.join("state.json").exists(), "state.json migrated");
        assert!(dir.join("config.json").exists(), "config.json migrated");
        assert!(!tmp.path().join("state.json").exists(), "root left clean");
        assert!(!tmp.path().join("config.json").exists(), "root left clean");
        // Second call: profiles/ exists, so the guard makes it a no-op.
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(dir.join("state.json").exists());
    }

    #[test]
    fn migrate_legacy_state_only() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(tmp.path().join("profiles").join("default").join("state.json").exists());
        assert!(!tmp.path().join("state.json").exists());
    }

    #[test]
    fn migrate_legacy_config_only_still_migrates() {
        // A user who only ever changed settings has config.json but no
        // state.json — those settings must not be orphaned at the old root.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(tmp.path().join("profiles").join("default").join("config.json").exists());
        assert!(!tmp.path().join("config.json").exists());
    }

    #[test]
    fn migrate_legacy_leaves_config_alone_when_config_env_overrides_it() {
        // KOMMAND0_CONFIG active (threaded as the flag — tests never touch
        // process env): the override may point AT the root config.json, so it
        // stays put; state.json still migrates. The `false` flavor of this
        // seed is `migrate_legacy_moves_state_and_config_and_is_idempotent`.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), true).unwrap();
        let dir = tmp.path().join("profiles").join("default");
        assert!(dir.join("state.json").exists(), "state.json still migrates");
        assert!(!dir.join("config.json").exists(), "config.json not moved");
        assert!(tmp.path().join("config.json").exists(), "config.json stays at the root");
    }

    #[test]
    fn migrate_legacy_noop_when_profiles_dir_exists() {
        // An existing profiles/ dir — even empty — means the migration already
        // ran (or a save beat us to it): never move anything then.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(tmp.path().join("state.json").exists(), "legacy file untouched");
        assert!(!tmp.path().join("profiles").join("default").exists(), "nothing moved");
    }

    #[test]
    fn migrate_legacy_fresh_dir_creates_nothing() {
        let tmp = TempDir::new().unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(
            !tmp.path().join("profiles").exists(),
            "a fresh install stays side-effect-free (first save creates the dir)"
        );
    }

    #[test]
    fn migrate_legacy_preserves_content_and_leaves_worktrees_in_place() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.repos.push(RepoEntry {
            id: "r1".to_string(),
            name: "my-repo".to_string(),
            path: "/tmp/my-repo".to_string(),
        });
        state.save_to(tmp.path()).unwrap();
        fs::create_dir_all(tmp.path().join("worktrees").join("ws")).unwrap();

        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();

        let migrated =
            AppState::load_from(&tmp.path().join("profiles").join("default")).unwrap();
        assert_eq!(migrated.repos.len(), 1);
        assert_eq!(migrated.repos[0].name, "my-repo", "content survives the move");
        assert!(
            tmp.path().join("worktrees").join("ws").is_dir(),
            "worktrees stay at the old root (state stores absolute paths)"
        );
        assert!(!tmp.path().join("profiles").join("default").join("worktrees").exists());
    }

    #[cfg(unix)]
    #[test]
    fn migrate_legacy_failure_leaves_no_masking_husk_and_stays_retriable() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        // A dangling symlink at base/profiles: `exists()` follows the link
        // (stat), so the idempotence guard passes, and create_dir_all then
        // fails deterministically.
        std::os::unix::fs::symlink("missing", tmp.path().join("profiles")).unwrap();

        let err = AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap_err();
        assert!(err.to_string().contains("failed to migrate"), "got: {err}");
        assert!(tmp.path().join("state.json").exists(), "legacy file untouched");
        assert!(
            !tmp.path().join("profiles").exists(),
            "no masking husk left behind (a dangling link still counts as absent)"
        );
        // Retriable: a second run errors again rather than silently masking
        // the legacy file behind the guard.
        assert!(AppState::migrate_legacy_profiles_at(tmp.path(), false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn migrate_legacy_failure_message_names_the_failing_file() {
        // Config-only flavor of the husk test: the error must be keyed to the
        // file actually being migrated, not hardcoded to state.json.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        std::os::unix::fs::symlink("missing", tmp.path().join("profiles")).unwrap();
        let err = AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap_err();
        assert!(err.to_string().contains("config.json"), "file-keyed message: {err}");
    }
}
