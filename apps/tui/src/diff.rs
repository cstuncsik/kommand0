//! The review-diff dialog: a GitHub-style two-pane view of a workspace's
//! PR-style diff (`git diff <default>...HEAD`, committed changes only — see
//! [`kommand0_core::diff_files_vs_default_branch`]). Left: a collapsible file
//! tree; right: the selected file's diff.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::theme::Theme;
use super::{App, DiffFocus, DiffRow};

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

/// Build the coloured lines of one file's diff `text`, capped at `MAX_LINES`.
fn diff_body_lines(text: &str, th: Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
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
    lines
}

/// Render the two-pane review-diff dialog. Reads/writes `app` so it can render
/// both panes and stash the pane rects (`diff_list_area`/`diff_body_area`) for
/// mouse hit-testing. Lines aren't wrapped, so one diff line is one screen row
/// and the scroll clamps stay exact.
pub fn render_diff_overlay(frame: &mut ratatui::Frame, app: &mut App) {
    let th = app.theme;
    let area = centered_rect(85, 85, frame.area());
    frame.render_widget(Clear, area);

    let outer = Block::default()
        .title(format!(" Review — {} ", app.diff_title))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Reserve the footer hint row, then split the rest into left list / right diff.
    let vparts = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let cols = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(vparts[0]);
    let list_col = cols[0];
    let body_col = cols[1];

    let files_focused = app.diff_focus == DiffFocus::Files;
    let border = |focused: bool| {
        if focused {
            Style::default().fg(th.accent)
        } else {
            Style::default().fg(th.muted)
        }
    };

    // --- Left pane: the collapsible file tree ---
    let list_block = Block::default()
        .title(" Files ")
        .borders(Borders::ALL)
        .border_style(border(files_focused));
    let list_inner = list_block.inner(list_col);
    frame.render_widget(list_block, list_col);
    app.diff_list_area = list_inner;

    if app.diff_rows.is_empty() {
        let note = Paragraph::new(Line::styled(
            "  No changes",
            Style::default().fg(th.muted),
        ));
        frame.render_widget(note, list_inner);
    } else {
        // Keep the selection in view: scroll so it sits within the visible window.
        let view_h = list_inner.height as usize;
        let sel = app.diff_selected;
        let mut scroll = app.diff_list_scroll as usize;
        if sel < scroll {
            scroll = sel;
        } else if view_h > 0 && sel >= scroll + view_h {
            scroll = sel + 1 - view_h;
        }
        app.diff_list_scroll = scroll as u16;

        let rows: Vec<Line> = app
            .diff_rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let (indent, label, base_style) = match row {
                    DiffRow::Folder { path, name, depth } => {
                        let caret = if app.diff_expanded.contains(path) {
                            "\u{25BE}" // ▾
                        } else {
                            "\u{25B8}" // ▸
                        };
                        (
                            *depth,
                            format!("{caret} {name}"),
                            Style::default().fg(th.text).add_modifier(Modifier::BOLD),
                        )
                    }
                    DiffRow::File { name, depth, .. } => {
                        (*depth, name.clone(), Style::default().fg(th.text))
                    }
                };
                let text = format!("{}{}", "  ".repeat(indent as usize), label);
                let style = if i == sel {
                    base_style.add_modifier(Modifier::REVERSED)
                } else {
                    base_style
                };
                Line::styled(sanitize(&text), style)
            })
            .collect();
        frame.render_widget(
            Paragraph::new(rows).scroll((app.diff_list_scroll, 0)),
            list_inner,
        );
    }

    // --- Right pane: the selected file's diff ---
    let body_block = Block::default()
        .title(" Diff ")
        .borders(Borders::ALL)
        .border_style(border(!files_focused));
    let body_inner = body_block.inner(body_col);
    frame.render_widget(body_block, body_col);
    app.diff_body_area = body_inner;

    let body_lines: Vec<Line> = match app.diff_rows.get(app.diff_selected) {
        Some(DiffRow::File { file_idx, .. }) => {
            let text = &app.diff_files[*file_idx].text;
            let lines = diff_body_lines(text, th);
            if lines.is_empty() {
                vec![Line::styled(
                    "  (empty diff)",
                    Style::default().fg(th.muted),
                )]
            } else {
                lines
            }
        }
        Some(DiffRow::Folder { .. }) => vec![Line::styled(
            "  Select a file to see its diff.",
            Style::default().fg(th.muted),
        )],
        None => vec![Line::styled(
            "  No committed changes on this branch vs the default branch.",
            Style::default().fg(th.muted),
        )],
    };

    // Clamp the diff scroll so the last line stays at the bottom edge (exact: no
    // wrap). Only meaningful for a File selection; harmless otherwise.
    let body_h = body_inner.height as usize;
    let max_scroll = body_lines.len().saturating_sub(body_h) as u16;
    app.diff_scroll = app.diff_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(body_lines).scroll((app.diff_scroll, 0)),
        body_inner,
    );

    // --- Footer hint ---
    let hint = Paragraph::new(Line::styled(
        "  [Tab] switch · j/k · Enter expand · Esc close",
        Style::default().fg(th.muted),
    ));
    frame.render_widget(hint, vparts[1]);
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
