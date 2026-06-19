use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::Focus;

pub struct KeyBinding {
    pub keys: &'static str,
    pub description: &'static str,
}

pub struct KeySection {
    pub title: &'static str,
    pub bindings: &'static [KeyBinding],
}

const GLOBAL_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        keys: "[q]",
        description: "Quit",
    },
    KeyBinding {
        keys: "[?]",
        description: "Help",
    },
];

const TREE_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        keys: "[j/k or Up/Down]",
        description: "Navigate",
    },
    KeyBinding {
        keys: "[h/l or Left/Right]",
        description: "Collapse / expand",
    },
    KeyBinding {
        keys: "[gg/G]",
        description: "First / last item",
    },
    KeyBinding {
        keys: "[Enter / e / r / R]",
        description: "Open embedded claude",
    },
    KeyBinding {
        keys: "[x]",
        description: "Close embedded claude",
    },
    KeyBinding {
        keys: "[a]",
        description: "Add repository",
    },
    KeyBinding {
        keys: "[w]",
        description: "Add workspace",
    },
    KeyBinding {
        keys: "[d]",
        description: "Delete selected",
    },
    KeyBinding {
        keys: "[D]",
        description: "Force delete",
    },
];

const EMBEDDED_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        keys: "(typing)",
        description: "Goes to the embedded claude",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [c]",
        description: "New session tab",
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

const SECTIONS: &[KeySection] = &[
    KeySection {
        title: "Global",
        bindings: GLOBAL_BINDINGS,
    },
    KeySection {
        title: "Tree Pane",
        bindings: TREE_BINDINGS,
    },
    KeySection {
        title: "Embedded claude",
        bindings: EMBEDDED_BINDINGS,
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

pub fn render_help_overlay(frame: &mut ratatui::Frame, focus: Focus, scroll: &mut u16) {
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
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::raw(""));

    for section in SECTIONS {
        let is_active = section.title == current_section;
        let title_style = if is_active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };

        lines.push(Line::styled(format!("  {}", section.title), title_style));

        for binding in section.bindings {
            let key_style = if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            let desc_style = if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(binding.keys, key_style),
                Span::raw(" "),
                Span::styled(binding.description, desc_style),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // Dismiss hint at bottom
    lines.push(Line::styled(
        "  Press ? or Esc to close, j/k to scroll",
        Style::default().fg(Color::DarkGray),
    ));

    // Clamp scroll so the last line stays at the bottom edge
    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height) as u16;
    *scroll = (*scroll).min(max_scroll);

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((*scroll, 0));

    frame.render_widget(paragraph, area);
}
