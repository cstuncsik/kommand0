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

// Embedded-pane rows that aren't keymap actions. The post-prefix commands come
// from the keymap (`pane_help_rows`), so rebinds show correctly; these are the
// typing passthrough, the mouse hint and the fixed (non-rebindable) prefix keys.
// Each row is kept to one unwrapped line (see the scroll-clamp note below).
const EMBEDDED_FIXED_HEAD: &[KeyBinding] = &[
    KeyBinding {
        keys: "(typing)",
        description: "Goes to the embedded claude",
    },
    KeyBinding {
        keys: "Alt+Enter",
        description: "Newline (when Shift+Enter submits)",
    },
    KeyBinding {
        keys: "Ctrl+]",
        description: "Back to tree, no prefix needed",
    },
];

const EMBEDDED_FIXED_TAIL: &[KeyBinding] = &[
    KeyBinding {
        keys: "(wheel tilt)",
        description: "Prev / next tab (also Shift+wheel)",
    },
    KeyBinding {
        keys: "Ctrl+A then 1-9",
        description: "Jump to tab N",
    },
    KeyBinding {
        keys: "Ctrl+A then Ctrl+A",
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

/// Render the help overlay. `tree_rows` / `pane_rows` are the live tree-pane and
/// post-prefix bindings (`(keys, description)`) from the keymap, so the overlay
/// reflects any rebinds;
/// `icon_sections` is the glyph legend, built from the same helpers that draw
/// the glyphs.
pub fn render_help_overlay(
    frame: &mut ratatui::Frame,
    focus: Focus,
    scroll: &mut u16,
    tree_rows: &[(String, &'static str)],
    pane_rows: &[(String, &'static str)],
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

    // Sections: both panes' bindings come from the keymap (dynamic); the
    // embedded section wraps its rows in the fixed ones.
    let tree: Vec<(&str, &str)> = tree_rows.iter().map(|(k, d)| (k.as_str(), *d)).collect();
    let embedded: Vec<(&str, &str)> = EMBEDDED_FIXED_HEAD
        .iter()
        .map(|b| (b.keys, b.description))
        .chain(pane_rows.iter().map(|(k, d)| (k.as_str(), *d)))
        .chain(EMBEDDED_FIXED_TAIL.iter().map(|b| (b.keys, b.description)))
        .collect();

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
