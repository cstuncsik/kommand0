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
    KeyBinding { keys: "[q]", description: "Quit" },
    KeyBinding { keys: "[?]", description: "Help" },
    KeyBinding { keys: "[Tab]", description: "Next pane" },
    KeyBinding { keys: "[Shift+Tab]", description: "Previous pane" },
    KeyBinding { keys: "[Esc]", description: "Back to tree" },
];

const TREE_BINDINGS: &[KeyBinding] = &[
    KeyBinding { keys: "[j/k]", description: "Navigate" },
    KeyBinding { keys: "[Up/Down]", description: "Navigate" },
    KeyBinding { keys: "[Enter]", description: "Expand/start session" },
    KeyBinding { keys: "[r]", description: "Start session" },
    KeyBinding { keys: "[R]", description: "Restart session" },
    KeyBinding { keys: "[x]", description: "Stop session" },
    KeyBinding { keys: "[a]", description: "Add repository" },
    KeyBinding { keys: "[w]", description: "Add workspace" },
    KeyBinding { keys: "[d]", description: "Delete selected" },
    KeyBinding { keys: "[D]", description: "Force delete" },
];

const OUTPUT_BINDINGS: &[KeyBinding] = &[
    KeyBinding { keys: "[j/k]", description: "Scroll line" },
    KeyBinding { keys: "[Up/Down]", description: "Scroll line" },
    KeyBinding { keys: "[PgUp/PgDn]", description: "Scroll page" },
    KeyBinding { keys: "[g/Home]", description: "Top" },
    KeyBinding { keys: "[G/End]", description: "Bottom" },
    KeyBinding { keys: "[z]", description: "Zoom toggle" },
    KeyBinding { keys: "[i]", description: "Compose" },
];

const COMPOSER_BINDINGS: &[KeyBinding] = &[
    KeyBinding { keys: "[Enter]", description: "Send message" },
    KeyBinding { keys: "[Shift+Enter / Alt+Enter]", description: "New line" },
    KeyBinding { keys: "[Ctrl+A]", description: "Select all" },
    KeyBinding { keys: "[Ctrl+C]", description: "Copy selection" },
    KeyBinding { keys: "[Ctrl+V]", description: "Paste" },
];

const SECTIONS: &[KeySection] = &[
    KeySection { title: "Global", bindings: GLOBAL_BINDINGS },
    KeySection { title: "Tree Pane", bindings: TREE_BINDINGS },
    KeySection { title: "Output Pane", bindings: OUTPUT_BINDINGS },
    KeySection { title: "Composer", bindings: COMPOSER_BINDINGS },
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
        Focus::Output => "Output Pane",
        Focus::Composer => "Composer",
    }
}

pub fn render_help_overlay(frame: &mut ratatui::Frame, focus: Focus) {
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
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::raw(""));

    for section in SECTIONS {
        let is_active = section.title == current_section;
        let title_style = if is_active {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
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
        "  Press ? or Esc to close",
        Style::default().fg(Color::DarkGray),
    ));

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}
