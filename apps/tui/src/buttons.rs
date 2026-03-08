use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// An action triggered by clicking a button.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub(crate) enum HitAction {
    StartSession,
    StopSession,
    ResumeSession,
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
    let text = format!("[{}]", label);
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
