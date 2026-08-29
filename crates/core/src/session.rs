use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionStatus {
    Running,
    Stopped,
    Failed,
    Exited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub workspace_id: String,
    pub claude_session_id: Option<String>,
    pub pid: Option<u32>,
    pub status: SessionStatus,
    pub created_at: u64,
    pub ended_at: Option<u64>,
    pub log_file: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use crate::repo::RepoEntry;
    use tempfile::TempDir;

    fn make_state_with_workspace(tmp: &TempDir) -> (AppState, String) {
        let mut state = AppState::default();
        state.repos.push(RepoEntry {
            id: "repo1".to_string(),
            name: "myapp".to_string(),
            path: "/tmp/myapp".to_string(),
            added_at: None,
        });
        state.workspaces.push(crate::Workspace {
            id: "ws1".to_string(),
            name: "myapp".to_string(),
            repo_id: "repo1".to_string(),
            working_dir: "/tmp/myapp".to_string(),
            active: true,
            created_at: 1000,
            worktree_path: None,
            branch_name: None,
        });
        state.save_to(tmp.path()).unwrap();
        (state, "ws1".to_string())
    }

    #[test]
    fn session_serializes_and_deserializes() {
        let session = Session {
            id: "abc-123".to_string(),
            workspace_id: "ws1".to_string(),
            claude_session_id: Some("claude-xyz".to_string()),
            pid: Some(1234),
            status: SessionStatus::Running,
            created_at: 1000,
            ended_at: None,
            log_file: ".kommand0-dev/sessions/abc-123.log".to_string(),
        };
        let json = serde_json::to_string(&session).unwrap();
        let deserialized: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "abc-123");
        assert_eq!(deserialized.status, SessionStatus::Running);
        assert_eq!(deserialized.claude_session_id, Some("claude-xyz".to_string()));
        assert_eq!(deserialized.pid, Some(1234));
        assert_eq!(deserialized.log_file, ".kommand0-dev/sessions/abc-123.log");
    }

    #[test]
    fn session_status_variants_serialize_deserialize() {
        for status in [
            SessionStatus::Running,
            SessionStatus::Stopped,
            SessionStatus::Failed,
            SessionStatus::Exited,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: SessionStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn appstate_backward_compat_no_sessions_key() {
        let tmp = TempDir::new().unwrap();
        let json = r#"{"repos": [{"id": "r1", "name": "foo", "path": "/tmp/foo"}]}"#;
        std::fs::write(tmp.path().join("state.json"), json).unwrap();

        let state = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(state.repos.len(), 1);
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn appstate_sessions_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.sessions.push(Session {
            id: "s1".to_string(),
            workspace_id: "ws1".to_string(),
            claude_session_id: None,
            pid: None,
            status: SessionStatus::Running,
            created_at: 1000,
            ended_at: None,
            log_file: ".kommand0-dev/sessions/s1.log".to_string(),
        });
        state.save_to(tmp.path()).unwrap();

        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.sessions[0].id, "s1");
    }

    #[test]
    fn create_session_returns_session_with_correct_fields() {
        let tmp = TempDir::new().unwrap();
        let (mut state, ws_id) = make_state_with_workspace(&tmp);
        let session = state.create_session_with_base(&ws_id, tmp.path()).unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.workspace_id, ws_id);
        assert_eq!(session.status, SessionStatus::Running);
        assert!(session.log_file.contains(&session.id));
        assert!(session.log_file.contains("sessions"));
        assert!(session.created_at > 0);
        assert!(session.ended_at.is_none());
    }

    #[test]
    fn create_session_errors_if_running_session_exists() {
        let tmp = TempDir::new().unwrap();
        let (mut state, ws_id) = make_state_with_workspace(&tmp);
        state.create_session_with_base(&ws_id, tmp.path()).unwrap();
        let err = state.create_session_with_base(&ws_id, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("already has a running session"));
    }

    #[test]
    fn find_session_by_workspace_returns_most_recent() {
        let tmp = TempDir::new().unwrap();
        let (mut state, ws_id) = make_state_with_workspace(&tmp);
        // Add two sessions for same workspace (first one stopped)
        state.sessions.push(Session {
            id: "old".to_string(),
            workspace_id: ws_id.clone(),
            claude_session_id: None,
            pid: None,
            status: SessionStatus::Stopped,
            created_at: 100,
            ended_at: Some(200),
            log_file: ".kommand0-dev/sessions/old.log".to_string(),
        });
        state.sessions.push(Session {
            id: "new".to_string(),
            workspace_id: ws_id.clone(),
            claude_session_id: None,
            pid: None,
            status: SessionStatus::Running,
            created_at: 300,
            ended_at: None,
            log_file: ".kommand0-dev/sessions/new.log".to_string(),
        });

        let found = state.find_session_by_workspace(&ws_id).unwrap();
        assert_eq!(found.id, "new");
    }

    #[test]
    fn update_session_status_changes_status_and_sets_ended_at() {
        let tmp = TempDir::new().unwrap();
        let (mut state, ws_id) = make_state_with_workspace(&tmp);
        let session = state.create_session_with_base(&ws_id, tmp.path()).unwrap();
        let session_id = session.id.clone();

        state
            .update_session_status_with_base(&session_id, SessionStatus::Stopped, tmp.path())
            .unwrap();

        let updated = state.find_session_mut(&session_id).unwrap();
        assert_eq!(updated.status, SessionStatus::Stopped);
        assert!(updated.ended_at.is_some());
    }
}
