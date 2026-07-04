//! The review-diff overlay: a scrollable view of a workspace's PR-style diff
//! (`git diff <default>...HEAD`, committed changes only — see
//! [`kommand0_core::diff_vs_default_branch`]).

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::theme::Theme;

/// Cap on rendered diff lines — a pathological diff shouldn't build a giant
/// paragraph. The tail is rarely what you review first.
const MAX_LINES: usize = 5000;

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

/// Colour a single diff line by its leading marker. `+++`/`---` file headers are
/// matched before the `+`/`-` add/remove lines so they aren't mis-coloured.
fn styled_diff_line(raw: &str, th: Theme) -> Line<'static> {
    let style = if raw.starts_with("diff --git")
        || raw.starts_with("index ")
        || raw.starts_with("+++")
        || raw.starts_with("---")
        || raw.starts_with("new file")
        || raw.starts_with("deleted file")
        || raw.starts_with("rename ")
        || raw.starts_with("similarity ")
    {
        Style::default().fg(th.muted).add_modifier(Modifier::BOLD)
    } else if raw.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if raw.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if raw.starts_with('-') {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(th.text)
    };
    Line::styled(raw.to_string(), style)
}

/// Render the review-diff overlay. `title` is the workspace label; `text` is the
/// raw diff (empty → a "no changes" note). Lines aren't wrapped, so one diff
/// line is one screen row and the `scroll` clamp stays exact.
pub fn render_diff_overlay(
    frame: &mut ratatui::Frame,
    title: &str,
    text: &str,
    scroll: &mut u16,
    theme: Theme,
) {
    let th = theme;
    let area = centered_rect(80, 80, frame.area());
    frame.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    if text.is_empty() {
        lines.push(Line::styled(
            "  No committed changes on this branch vs the default branch.",
            Style::default().fg(th.muted),
        ));
    } else {
        for raw in text.lines().take(MAX_LINES) {
            lines.push(styled_diff_line(raw, th));
        }
        if text.lines().nth(MAX_LINES).is_some() {
            lines.push(Line::styled(
                "  … diff truncated — open a shell tab for the full diff",
                Style::default().fg(th.muted),
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Esc/v close · j/k scroll · PgUp/PgDn page",
        Style::default().fg(th.muted),
    ));

    // Clamp scroll so the last line stays at the bottom edge (exact: no wrap).
    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height) as u16;
    *scroll = (*scroll).min(max_scroll);

    let block = Block::default()
        .title(format!(" Review — {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent));

    let paragraph = Paragraph::new(lines).block(block).scroll((*scroll, 0));
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg(line: Line) -> Option<Color> {
        line.style.fg
    }

    #[test]
    fn diff_lines_are_coloured_by_marker() {
        let th = Theme::default();
        // `+++`/`---`/`diff --git` are file headers — matched BEFORE the `+`/`-`
        // add/remove branches, or a `+++` line would render green.
        assert_eq!(fg(styled_diff_line("+++ b/x.rs", th)), Some(th.muted));
        assert_eq!(fg(styled_diff_line("--- a/x.rs", th)), Some(th.muted));
        assert_eq!(fg(styled_diff_line("diff --git a/x b/x", th)), Some(th.muted));
        // Real add / remove / hunk / context.
        assert_eq!(fg(styled_diff_line("+added", th)), Some(Color::Green));
        assert_eq!(fg(styled_diff_line("-removed", th)), Some(Color::Red));
        assert_eq!(fg(styled_diff_line("@@ -1 +1 @@", th)), Some(Color::Cyan));
        assert_eq!(fg(styled_diff_line(" context", th)), Some(th.text));
    }
}
