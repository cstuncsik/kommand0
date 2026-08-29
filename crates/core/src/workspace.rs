use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub repo_id: String,
    pub working_dir: String,
    pub active: bool,
    pub created_at: u64,
    #[serde(default)]
    pub worktree_path: Option<String>,
    /// The git branch the worktree was created on (named after the workspace,
    /// suffixed `-2`/`-3`… on collision; adopted branches keep their name; pre-
    /// existing workspaces may carry a legacy `kommand0/<name>`), captured at
    /// creation. `None` for fallback workspaces with no own worktree/branch.
    #[serde(default)]
    pub branch_name: Option<String>,
}

/// Format a unix timestamp (seconds) as "YYYY-MM-DD HH:MM" in local timezone.
pub fn format_timestamp(unix_secs: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(unix_secs as i64, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => format!("{unix_secs}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use crate::repo::RepoEntry;
    use tempfile::TempDir;

    fn make_state_with_repo(tmp: &TempDir) -> (AppState, String) {
        let mut state = AppState::default();
        let repo = RepoEntry {
            id: "repo1".to_string(),
            name: "myapp".to_string(),
            path: "/tmp/myapp".to_string(),
            added_at: None,
        };
        state.repos.push(repo);
        state.save_to(tmp.path()).unwrap();
        (state, "repo1".to_string())
    }

    // --- create_workspace tests ---

    #[test]
    fn create_workspace_auto_name_from_repo() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        let ws = state.create_workspace_with_base(None, "myapp", tmp.path()).unwrap();
        assert_eq!(ws.name, "myapp");
        assert_eq!(ws.repo_id, "repo1");
        assert!(ws.active);
        assert!(!ws.id.is_empty());
        assert_eq!(ws.working_dir, "/tmp/myapp");
    }

    #[test]
    fn create_workspace_explicit_name() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        let ws = state.create_workspace_with_base(Some("my-feature"), "myapp", tmp.path()).unwrap();
        assert_eq!(ws.name, "my-feature");
    }

    #[test]
    fn create_workspace_duplicate_name_error() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        state.create_workspace_with_base(Some("dup"), "myapp", tmp.path()).unwrap();
        let err = state.create_workspace_with_base(Some("dup"), "myapp", tmp.path()).unwrap_err();
        assert!(err.to_string().contains("workspace already exists: dup"));
    }

    #[test]
    fn create_workspace_rejects_unsafe_names() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        // Names that would escape the worktrees dir, trip git arg parsing, or
        // that git refuses as bare branch names (HEAD, @).
        for bad in ["", "   ", ".", "..", "../escape", "a/b", "a\\b", "-rf", "HEAD", "@"] {
            let err = state
                .create_workspace_with_base(Some(bad), "myapp", tmp.path())
                .unwrap_err();
            assert!(
                err.to_string().contains("invalid workspace name"),
                "{bad:?} should be rejected, got: {err}"
            );
        }
        // A dot or internal dash in an otherwise plain name is still fine.
        assert!(state.create_workspace_with_base(Some("feat.v2"), "myapp", tmp.path()).is_ok());
        assert!(state.create_workspace_with_base(Some("my-feature"), "myapp", tmp.path()).is_ok());
    }

    #[test]
    fn create_workspace_unknown_repo_error() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        let err = state.create_workspace_with_base(Some("ws"), "nope", tmp.path()).unwrap_err();
        assert!(err.to_string().contains("No repo found matching"));
        assert!(err.to_string().contains("Checked: name, path, id"));
    }

    #[test]
    fn create_workspace_sets_created_at() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        let ws = state.create_workspace_with_base(None, "myapp", tmp.path()).unwrap();
        assert!(ws.created_at > 0);
    }

    // --- resolve_repo tests ---

    #[test]
    fn resolve_repo_by_name() {
        let tmp = TempDir::new().unwrap();
        let (state, _) = make_state_with_repo(&tmp);
        let repo = state.resolve_repo("myapp").unwrap();
        assert_eq!(repo.id, "repo1");
    }

    #[test]
    fn resolve_repo_by_id() {
        let tmp = TempDir::new().unwrap();
        let (state, _) = make_state_with_repo(&tmp);
        let repo = state.resolve_repo("repo1").unwrap();
        assert_eq!(repo.name, "myapp");
    }

    #[test]
    fn resolve_repo_by_path() {
        let tmp = TempDir::new().unwrap();
        let (state, _) = make_state_with_repo(&tmp);
        let repo = state.resolve_repo("/tmp/myapp").unwrap();
        assert_eq!(repo.id, "repo1");
    }

    #[test]
    fn resolve_repo_path_first_if_contains_slash() {
        let _tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        // Add repo with path that also matches a name-like string
        state.repos.push(RepoEntry {
            id: "r1".to_string(),
            name: "some/name".to_string(),
            path: "/opt/repos/foo".to_string(),
            added_at: None,
        });
        // When input contains '/', path is checked first
        let repo = state.resolve_repo("/opt/repos/foo").unwrap();
        assert_eq!(repo.id, "r1");
    }

    #[test]
    fn resolve_repo_error_includes_checked() {
        let state = AppState::default();
        let err = state.resolve_repo("missing").unwrap_err();
        assert!(err.to_string().contains("Checked: name, path, id"));
    }

    // --- list_workspaces tests ---

    #[test]
    fn list_workspaces_active_only() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        state.create_workspace_with_base(Some("active-ws"), "myapp", tmp.path()).unwrap();
        state.create_workspace_with_base(Some("archived-ws"), "myapp", tmp.path()).unwrap();
        state.archive_workspace_with_base("archived-ws", tmp.path()).unwrap();

        let active = state.list_workspaces(false, None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "active-ws");
    }

    #[test]
    fn list_workspaces_all() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        state.create_workspace_with_base(Some("ws1"), "myapp", tmp.path()).unwrap();
        state.create_workspace_with_base(Some("ws2"), "myapp", tmp.path()).unwrap();
        state.archive_workspace_with_base("ws2", tmp.path()).unwrap();

        let all = state.list_workspaces(true, None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn list_workspaces_by_repo() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.repos.push(RepoEntry { id: "r1".to_string(), name: "app1".to_string(), path: "/tmp/app1".to_string(), added_at: None });
        state.repos.push(RepoEntry { id: "r2".to_string(), name: "app2".to_string(), path: "/tmp/app2".to_string(), added_at: None });
        state.save_to(tmp.path()).unwrap();
        state.create_workspace_with_base(Some("ws-a"), "app1", tmp.path()).unwrap();
        state.create_workspace_with_base(Some("ws-b"), "app2", tmp.path()).unwrap();

        let filtered = state.list_workspaces(false, Some("app1")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "ws-a");
    }

    // --- archive/activate tests ---

    #[test]
    fn archive_workspace_sets_inactive() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        state.create_workspace_with_base(Some("ws"), "myapp", tmp.path()).unwrap();
        state.archive_workspace_with_base("ws", tmp.path()).unwrap();
        let ws = state.show_workspace("ws").unwrap();
        assert!(!ws.active);
    }

    #[test]
    fn activate_workspace_sets_active() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        state.create_workspace_with_base(Some("ws"), "myapp", tmp.path()).unwrap();
        state.archive_workspace_with_base("ws", tmp.path()).unwrap();
        state.activate_workspace_with_base("ws", tmp.path()).unwrap();
        let ws = state.show_workspace("ws").unwrap();
        assert!(ws.active);
    }

    // --- delete tests ---

    #[test]
    fn delete_workspace_removes_from_vec() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        state.create_workspace_with_base(Some("ws"), "myapp", tmp.path()).unwrap();
        let removed = state.delete_workspace_with_base("ws", tmp.path()).unwrap();
        assert_eq!(removed.name, "ws");
        assert!(state.workspaces.is_empty());
    }

    // --- show tests ---

    #[test]
    fn show_workspace_finds_by_name() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        state.create_workspace_with_base(Some("ws"), "myapp", tmp.path()).unwrap();
        let ws = state.show_workspace("ws").unwrap();
        assert_eq!(ws.name, "ws");
    }

    #[test]
    fn show_workspace_error_for_missing() {
        let state = AppState::default();
        let err = state.show_workspace("nope").unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    // --- persistence tests ---

    #[test]
    fn roundtrip_workspaces_persist() {
        let tmp = TempDir::new().unwrap();
        let (mut state, _) = make_state_with_repo(&tmp);
        state.create_workspace_with_base(Some("ws"), "myapp", tmp.path()).unwrap();

        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.workspaces[0].name, "ws");
    }

    #[test]
    fn backward_compat_no_workspaces_key() {
        let tmp = TempDir::new().unwrap();
        let json = r#"{"repos": [{"id": "r1", "name": "foo", "path": "/tmp/foo"}]}"#;
        std::fs::write(tmp.path().join("state.json"), json).unwrap();

        let state = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(state.repos.len(), 1);
        assert!(state.workspaces.is_empty());
    }

    // --- format_timestamp test ---

    #[test]
    fn format_timestamp_produces_readable_output() {
        // 2026-01-01 00:00:00 UTC = 1767225600
        let result = format_timestamp(1767225600);
        assert!(result.contains("2026"));
        assert!(result.contains("01"));
    }
}
