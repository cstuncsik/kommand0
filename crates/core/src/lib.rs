pub mod config;
pub mod git;
pub mod id;
pub mod repo;
pub mod session;
pub mod workspace;
pub mod worktree;

pub use config::Config;
pub use git::{BranchStatus, branch_status, cleanup_merged_workspace, open_pull_request};
pub use id::generate_id;
pub use repo::{RepoEntry, run_git_status};
pub use session::{Session, SessionStatus};
pub use workspace::Workspace;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppState {
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

impl AppState {
    const STATE_DIR: &str = ".kommand0-dev";
    const STATE_FILE: &str = "state.json";

    /// Resolve the state directory.
    ///
    /// Priority: `KOMMAND0_STATE_DIR` env var, then `.kommand0-dev/` relative
    /// to the current directory in debug builds, then the platform data dir
    /// (`~/Library/Application Support/kommand0` on macOS) in release builds.
    pub fn state_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("KOMMAND0_STATE_DIR")
            && !dir.is_empty()
        {
            return PathBuf::from(dir);
        }
        if cfg!(debug_assertions) {
            PathBuf::from(Self::STATE_DIR)
        } else {
            dirs::data_dir()
                .map(|d| d.join("kommand0"))
                .unwrap_or_else(|| PathBuf::from(Self::STATE_DIR))
        }
    }

    /// Load state from the given base directory. Returns default if no state file exists.
    pub fn load_from(base: &Path) -> anyhow::Result<Self> {
        let path = base.join(Self::STATE_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let state: Self = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(state)
    }

    /// Save state to the given base directory, creating it if needed.
    pub fn save_to(&self, base: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(base)
            .with_context(|| format!("failed to create {}", base.display()))?;
        let path = base.join(Self::STATE_FILE);
        let data = serde_json::to_string_pretty(self)?;
        fs::write(&path, data)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    /// Load state from the default state directory.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(Self::state_dir().as_path())
    }

    /// Save state to the default state directory.
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(Self::state_dir().as_path())
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
        self.create_workspace_impl(name, repo_ref, base, true)
    }

    /// Create a workspace, optionally skipping worktree creation.
    pub fn create_workspace_with_options(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
        use_worktree: bool,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_impl(name, repo_ref, base, use_worktree)
    }

    fn create_workspace_impl(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
        use_worktree: bool,
    ) -> anyhow::Result<Workspace> {
        let repo = self.resolve_repo(repo_ref)?.clone();

        let ws_name = match name {
            Some(n) => n.to_string(),
            None => repo.name.clone(),
        };

        if self.workspaces.iter().any(|w| w.name == ws_name) {
            bail!("workspace already exists: {ws_name}");
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();

        // Try to create a git worktree
        let (working_dir, worktree_path, branch_name) = if use_worktree {
            match worktree::create_worktree(&repo.path, &ws_name, base) {
                worktree::WorktreeResult::Created {
                    worktree_path,
                    branch_name,
                } => (worktree_path.clone(), Some(worktree_path), Some(branch_name)),
                worktree::WorktreeResult::Fallback { reason: _ } => (repo.path.clone(), None, None),
            }
        } else {
            (repo.path.clone(), None, None)
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
}
