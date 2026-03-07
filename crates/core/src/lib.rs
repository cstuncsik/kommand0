pub mod id;
pub mod repo;
pub mod session;
pub mod workspace;

pub use id::generate_id;
pub use repo::{RepoEntry, run_git_status};
pub use session::{Session, SessionStatus};
pub use workspace::Workspace;

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
}

impl AppState {
    const STATE_DIR: &str = ".kommand0-dev";
    const STATE_FILE: &str = "state.json";

    fn state_dir() -> PathBuf {
        PathBuf::from(Self::STATE_DIR)
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

    /// Add a repo, saving state to a custom base directory.
    pub fn add_repo_with_base(&mut self, path: &str, base: &Path) -> anyhow::Result<RepoEntry> {
        let dir = Path::new(path);
        if !dir.is_dir() {
            bail!("path does not exist or is not a directory: {}", path);
        }

        let canonical = fs::canonicalize(dir)
            .with_context(|| format!("failed to canonicalize {}", path))?;
        let canonical_str = canonical.to_string_lossy().to_string();

        if self.repos.iter().any(|r| r.path == canonical_str) {
            bail!("repo already tracked: {}", canonical_str);
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
            "No repo found matching '{}'. Checked: name, path, id. Use `kmd repo list` to see tracked repos.",
            reference
        )
    }

    /// Create a workspace with a custom base directory for state persistence.
    pub fn create_workspace_with_base(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
    ) -> anyhow::Result<Workspace> {
        let repo = self.resolve_repo(repo_ref)?.clone();

        let ws_name = match name {
            Some(n) => n.to_string(),
            None => repo.name.clone(),
        };

        if self.workspaces.iter().any(|w| w.name == ws_name) {
            bail!("workspace already exists: {}", ws_name);
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();

        let ws = Workspace {
            id: generate_id(),
            name: ws_name,
            repo_id: repo.id.clone(),
            working_dir: repo.path.clone(),
            active: true,
            created_at: now,
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
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {}", name))
    }

    /// Delete a workspace by name, saving to custom base directory.
    pub fn delete_workspace_with_base(
        &mut self,
        name: &str,
        base: &Path,
    ) -> anyhow::Result<Workspace> {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.name == name)
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {}", name))?;
        let removed = self.workspaces.remove(idx);
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
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {}", name))?;
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
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {}", name))?;
        ws.active = true;
        self.save_to(base)?;
        Ok(())
    }

    /// Activate a workspace (set active=true).
    pub fn activate_workspace(&mut self, name: &str) -> anyhow::Result<()> {
        self.activate_workspace_with_base(name, Self::state_dir().as_path())
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
