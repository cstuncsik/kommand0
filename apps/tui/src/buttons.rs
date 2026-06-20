use ratatui::layout::Rect;

/// An action triggered by clicking a button.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HitAction {
    StartSession,
    StartSessionFor { workspace_id: String },
    StopSessionFor { workspace_id: String },
    ResumeSessionFor { workspace_id: String },
    RetrySessionFor { workspace_id: String },
    FocusComposerFor { workspace_id: String },
    ToggleIconsFor { workspace_id: String },
    DeleteWorkspaceFor { workspace_id: String },
    DeleteRepoFor { repo_name: String },
    AddWorkspaceFor { repo_id: String },
    AddRepo,
    /// Select session tab `index` of a workspace (click on the tab strip).
    SelectSessionTab { workspace_id: String, index: usize },
    /// Open a new session tab for a workspace (click on the `[+]` tab).
    NewSessionTab { workspace_id: String },
    /// Open a GitHub PR for a workspace's branch (the detail-pane `[Open PR]`).
    OpenPrFor { workspace_id: String },
    /// Clean up a merged workspace (the detail-pane `[Clean up]`).
    CleanupWorkspaceFor { workspace_id: String },
}

/// A clickable region tracked during rendering.
#[derive(Clone)]
pub(crate) struct HitRegion {
    pub area: Rect,
    pub action: HitAction,
}

/// Check if a position is hovered.
pub(crate) fn is_hovered(mouse_pos: Option<(u16, u16)>, area: Rect) -> bool {
    if let Some((col, row)) = mouse_pos {
        col >= area.x
            && col < area.x + area.width
            && row >= area.y
            && row < area.y + area.height
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_hit_action_variants_clone_and_eq() {
        let start = HitAction::StartSessionFor { workspace_id: "ws-1".to_string() };
        let stop = HitAction::StopSessionFor { workspace_id: "ws-2".to_string() };
        let resume = HitAction::ResumeSessionFor { workspace_id: "ws-3".to_string() };
        let retry = HitAction::RetrySessionFor { workspace_id: "ws-4".to_string() };

        assert_eq!(start.clone(), start);
        assert_eq!(stop.clone(), stop);
        assert_eq!(resume.clone(), resume);
        assert_eq!(retry.clone(), retry);

        assert_ne!(start, HitAction::StartSessionFor { workspace_id: "ws-other".to_string() });
    }

    #[test]
    fn existing_variants_still_work() {
        let a = HitAction::StartSession;
        let b = HitAction::StartSession;
        assert_eq!(a, b);
    }

    #[test]
    fn hit_action_focus_composer_and_toggle_icons_clone_and_eq() {
        let focus = HitAction::FocusComposerFor { workspace_id: "ws-1".to_string() };
        let toggle = HitAction::ToggleIconsFor { workspace_id: "ws-2".to_string() };

        assert_eq!(focus.clone(), focus);
        assert_eq!(toggle.clone(), toggle);

        assert_ne!(focus, HitAction::FocusComposerFor { workspace_id: "ws-other".to_string() });
        assert_ne!(toggle, HitAction::ToggleIconsFor { workspace_id: "ws-other".to_string() });
    }
}
