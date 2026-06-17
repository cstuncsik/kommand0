use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// An action triggered by clicking a button.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
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
}

/// A clickable region tracked during rendering.
#[derive(Clone)]
pub(crate) struct HitRegion {
    pub area: Rect,
    pub action: HitAction,
}

/// Render a button label and return its Rect for hit-testing.
/// The button is rendered at a specific position within a line.
#[allow(dead_code)]
pub(crate) fn button_span(
    label: &str,
    area_x: u16,
    area_y: u16,
    hovered: bool,
) -> (Span<'static>, Rect) {
    let text = format!("[{label}]");
    let width = text.len() as u16;
    let style = if hovered {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };
    let rect = Rect::new(area_x, area_y, width, 1);
    (Span::styled(text, style), rect)
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

/// Build a line with a button, returning the hit region.
#[allow(dead_code)]
pub(crate) fn button_line(
    label: &str,
    action: HitAction,
    prefix: &str,
    area_x: u16,
    area_y: u16,
    mouse_pos: Option<(u16, u16)>,
) -> (Line<'static>, HitRegion) {
    let btn_x = area_x + prefix.len() as u16;
    let btn_rect = Rect::new(btn_x, area_y, (label.len() + 2) as u16, 1);
    let hovered = is_hovered(mouse_pos, btn_rect);
    let (span, _) = button_span(label, btn_x, area_y, hovered);

    let line = Line::from(vec![
        Span::raw(prefix.to_string()),
        span,
    ]);
    let region = HitRegion {
        area: btn_rect,
        action,
    };
    (line, region)
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
