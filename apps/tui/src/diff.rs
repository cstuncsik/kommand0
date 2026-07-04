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
/// Colour a diff line, tracking whether we're inside a hunk via `in_hunk`. This
/// is stateful on purpose: outside a hunk, `+++`/`---` are file headers; *inside*
/// a hunk the first byte decides, so an added/removed line whose content begins
/// with `++`/`--` is coloured as an add/remove, not mis-read as a header.
fn diff_line_style(raw: &str, in_hunk: &mut bool, th: Theme) -> Style {
    if raw.starts_with("@@") {
        *in_hunk = true; // hunk header; body lines follow
        return Style::default().fg(Color::Cyan);
    }
    if raw.starts_with("diff --git") {
        *in_hunk = false; // next file's header block starts
        return Style::default().fg(th.muted).add_modifier(Modifier::BOLD);
    }
    if !*in_hunk {
        // Header region (index/mode/+++/--- etc.) — all metadata.
        return Style::default().fg(th.muted).add_modifier(Modifier::BOLD);
    }
    match raw.as_bytes().first() {
        Some(b'+') => Style::default().fg(Color::Green),
        Some(b'-') => Style::default().fg(Color::Red),
        _ => Style::default().fg(th.text), // context, "\ No newline", blank
    }
}

/// Replace C0 control bytes (except tab) with the replacement char so a crafted
/// file in the reviewed diff can't emit raw terminal escapes into the overlay.
fn sanitize(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() && c != '\t' { '\u{fffd}' } else { c })
        .collect()
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
        let mut in_hunk = false;
        for raw in text.lines().take(MAX_LINES) {
            let style = diff_line_style(raw, &mut in_hunk, th);
            lines.push(Line::styled(sanitize(raw), style));
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

    #[test]
    fn diff_lines_are_coloured_by_hunk_state() {
        let th = Theme::default();
        let mut in_hunk = false;
        // Header region (before any @@): file metadata is muted + bold.
        let hdr = diff_line_style("diff --git a/x b/x", &mut in_hunk, th);
        assert_eq!(hdr.fg, Some(th.muted));
        assert!(hdr.add_modifier.contains(Modifier::BOLD));
        assert_eq!(diff_line_style("+++ b/x.rs", &mut in_hunk, th).fg, Some(th.muted));
        assert_eq!(diff_line_style("--- a/x.rs", &mut in_hunk, th).fg, Some(th.muted));
        // The hunk header flips us into the hunk body.
        assert_eq!(diff_line_style("@@ -1 +1 @@", &mut in_hunk, th).fg, Some(Color::Cyan));
        assert!(in_hunk);
        // Inside a hunk the first byte wins — INCLUDING content starting with ++/--
        // (the whole point of the stateful classifier).
        assert_eq!(diff_line_style("+added", &mut in_hunk, th).fg, Some(Color::Green));
        assert_eq!(diff_line_style("-removed", &mut in_hunk, th).fg, Some(Color::Red));
        assert_eq!(diff_line_style("++still added", &mut in_hunk, th).fg, Some(Color::Green));
        assert_eq!(diff_line_style("--still removed", &mut in_hunk, th).fg, Some(Color::Red));
        assert_eq!(diff_line_style(" context", &mut in_hunk, th).fg, Some(th.text));
        // The next file's header resets the hunk state.
        assert_eq!(diff_line_style("diff --git a/y b/y", &mut in_hunk, th).fg, Some(th.muted));
        assert!(!in_hunk);
    }

    #[test]
    fn sanitize_neutralises_escapes_but_keeps_tabs() {
        // A crafted diff line with a raw ESC must not reach the terminal.
        assert_eq!(sanitize("+\x1b[31mred\x07"), "+\u{fffd}[31mred\u{fffd}");
        assert_eq!(sanitize("\tindented"), "\tindented", "tabs are preserved");
    }
}
