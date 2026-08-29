use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::Focus;
use super::theme::Theme;

struct KeyBinding {
    keys: &'static str,
    description: &'static str,
}

/// One legend row: the glyph exactly as the UI draws it, in the colour the UI
/// draws it. The colour is half the meaning (the same `\u{25CF}` is "needs you"
/// in one colour and "producing" in another), so the overlay renders it rather
/// than describing it in words.
pub struct IconRow {
    pub glyph: String,
    pub color: Color,
    pub description: &'static str,
}

/// A titled group of [`IconRow`]s. Built by `render.rs`, which owns the glyph
/// and colour choices, so the legend can't drift from what is on screen.
pub struct IconSection {
    pub title: &'static str,
    pub rows: Vec<IconRow>,
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
        description: "New Claude Code session tab",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [s]",
        description: "New shell tab (reopens fresh)",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [e]",
        description: "New codex session tab",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [g]",
        description: "New gemini session tab",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [o]",
        description: "New opencode session tab",
    },
    KeyBinding {
        keys: "[Ctrl+A] then [ / ]",
        description: "Previous / next tab",
    },
    KeyBinding {
        keys: "(wheel tilt)",
        description: "Prev / next tab (also Shift+wheel)",
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
        keys: "[Ctrl+A] then [d]",
        description: "Detach: close panes, keep sessions",
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
/// (`(keys, description)`) from the keymap, so the overlay reflects any rebinds;
/// `icon_sections` is the glyph legend, built from the same helpers that draw
/// the glyphs.
pub fn render_help_overlay(
    frame: &mut ratatui::Frame,
    focus: Focus,
    scroll: &mut u16,
    tree_rows: &[(String, &'static str)],
    icon_sections: &[IconSection],
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

    // A section title is bold, and highlighted when it's the pane you're in.
    // The icon sections are never a focus target, so they never highlight.
    let section_title = |title: &str| {
        let style = if title == current_section {
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        Line::styled(format!("  {title}"), style)
    };

    // Keybindings first, then the glyph legend. The legend is reference
    // material you look up once; the keybindings are what `?` is usually for,
    // so ~26 legend rows must not push the Embedded section out of easy reach.
    for (title, bindings) in [("Tree Pane", &tree), ("Embedded claude", &embedded)] {
        lines.push(section_title(title));
        let style = if title == current_section {
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

    for section in icon_sections {
        lines.push(section_title(section.title));
        for row in &section.rows {
            lines.push(Line::from(vec![
                // The glyph keeps its own colour; the description does not, or
                // a red `✗` would drag its text red too.
                Span::styled(format!("    {:<4}", row.glyph), Style::default().fg(row.color)),
                Span::raw(" "),
                Span::styled(row.description, Style::default().fg(th.text)),
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
