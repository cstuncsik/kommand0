use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::Focus;
use super::theme::Theme;

struct KeyBinding {
    keys: &'static str,
    description: &'static str,
}

const EMBEDDED_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        keys: "(typing)",
        description: "Goes to the embedded claude",
    },
    // Kept to one unwrapped line (see the scroll-clamp note below); the tmux
    // detail lives in the README and the startup hint.
    KeyBinding {
        keys: "[Alt+Enter]",
        description: "Newline (when Shift+Enter submits)",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [c]",
        description: "New session tab",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [s]",
        description: "New shell tab (reopens fresh)",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [ / ]",
        description: "Previous / next tab",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [1]-[9]",
        description: "Jump to tab N",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [l]",
        description: "Jump to the last-active tab",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [r]",
        description: "Rename the active tab",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [x]",
        description: "Close the active tab",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [t]",
        description: "Back to tree (also Tab/Esc)",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [q]",
        description: "Quit kommand0",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [Ctrl+A]",
        description: "Send a literal Ctrl+A",
    },
];

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

fn focus_to_section(focus: Focus) -> &'static str {
    match focus {
        Focus::Tree => "Tree Pane",
        Focus::Embedded => "Embedded claude",
    }
}

/// Render the help overlay. `tree_rows` are the live tree-pane bindings
/// (`(keys, description)`) from the keymap, so the overlay reflects any rebinds.
pub fn render_help_overlay(
    frame: &mut ratatui::Frame,
    focus: Focus,
    scroll: &mut u16,
    tree_rows: &[(String, &'static str)],
    theme: Theme,
) {
    let th = theme;
    let area = centered_rect(60, 70, frame.area());

    // Clear the area behind the overlay
    frame.render_widget(Clear, area);

    let current_section = focus_to_section(focus);

    // Build content lines
    let mut lines: Vec<Line> = Vec::new();

    // Current pane indicator
    lines.push(Line::from(vec![
        Span::raw("  Current: "),
        Span::styled(
            current_section,
            Style::default()
                .fg(th.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::raw(""));

    // Sections: tree pane bindings come from the keymap (dynamic); the embedded
    // prefix is fixed (static).
    let tree: Vec<(&str, &str)> = tree_rows.iter().map(|(k, d)| (k.as_str(), *d)).collect();
    let embedded: Vec<(&str, &str)> =
        EMBEDDED_BINDINGS.iter().map(|b| (b.keys, b.description)).collect();
    let sections: [(&str, &[(&str, &str)]); 2] =
        [("Tree Pane", &tree), ("Embedded claude", &embedded)];

    for (title, bindings) in sections {
        let is_active = title == current_section;
        let title_style = if is_active {
            Style::default()
                .fg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };

        lines.push(Line::styled(format!("  {title}"), title_style));

        let style = if is_active {
            Style::default().fg(th.accent)
        } else {
            Style::default().fg(th.text)
        };
        for (keys, description) in bindings {
            lines.push(Line::from(vec![
                Span::styled(format!("    {keys:<16}"), style),
                Span::raw(" "),
                Span::styled(*description, style),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // Dismiss hint at bottom
    lines.push(Line::styled(
        "  Press ? or Esc to close, j/k to scroll",
        Style::default().fg(th.muted),
    ));

    // Clamp scroll so the last line stays at the bottom edge. This counts
    // LOGICAL lines while the Paragraph below wraps: a row that wraps makes
    // the tail unreachable — keep rows to one line (or count wrapped lines
    // here).
    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height) as u16;
    *scroll = (*scroll).min(max_scroll);

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((*scroll, 0));

    frame.render_widget(paragraph, area);
}
