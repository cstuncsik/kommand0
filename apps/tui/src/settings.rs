//! Full-screen settings page: view and edit `config.json` fields in-TUI.
//!
//! Each row is one config field, edited as a single text line (blank = unset,
//! which removes the key from the file). Commits are per-field on Enter via
//! `Config::update_file`, then mirrored into the running `App` so live knobs
//! (theme, tree width, notify) apply immediately. `keybindings`/`theme_colors`
//! stay file-only and are pointed at by a footer hint.

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use kommand0_core::Config;

use super::App;
use super::modal::{LineEdit, render_input_with_cursor};

const NOTIFY_VALUES: &[&str] = &["off", "bell", "desktop", "both"];
const THEME_VALUES: &[&str] = &["default", "high-contrast"];

/// One editable config field. `keybindings`/`theme_colors` are deliberately
/// absent (structured values — hand-edit the file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    ClaudeArgs,
    ClaudeBin,
    Shell,
    CodexArgs,
    CodexBin,
    GeminiArgs,
    GeminiBin,
    OpencodeArgs,
    OpencodeBin,
    Notify,
    Theme,
    StatusRefreshSecs,
    TreeWidthPct,
}

/// Page order.
pub(crate) const FIELDS: &[Field] = &[
    Field::ClaudeArgs,
    Field::ClaudeBin,
    Field::Shell,
    Field::CodexArgs,
    Field::CodexBin,
    Field::GeminiArgs,
    Field::GeminiBin,
    Field::OpencodeArgs,
    Field::OpencodeBin,
    Field::Notify,
    Field::Theme,
    Field::StatusRefreshSecs,
    Field::TreeWidthPct,
];

impl Field {
    /// The `config.json` key — doubles as the row label.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Field::ClaudeArgs => "claude_args",
            Field::ClaudeBin => "claude_bin",
            Field::Shell => "shell",
            Field::CodexArgs => "codex_args",
            Field::CodexBin => "codex_bin",
            Field::GeminiArgs => "gemini_args",
            Field::GeminiBin => "gemini_bin",
            Field::OpencodeArgs => "opencode_args",
            Field::OpencodeBin => "opencode_bin",
            Field::Notify => "notify",
            Field::Theme => "theme",
            Field::StatusRefreshSecs => "status_refresh_secs",
            Field::TreeWidthPct => "tree_width_pct",
        }
    }

    /// The current value as the edit-seed string; `""` = unset. The `*_args`
    /// fields are shell-quoted so an embedded-space arg survives an
    /// open→commit round-trip exactly.
    pub(crate) fn current(self, cfg: &Config) -> String {
        match self {
            Field::ClaudeArgs => shell_words::join(&cfg.claude_args),
            Field::ClaudeBin => cfg.claude_bin.clone().unwrap_or_default(),
            Field::Shell => cfg.shell.clone().unwrap_or_default(),
            Field::CodexArgs => shell_words::join(&cfg.codex_args),
            Field::CodexBin => cfg.codex_bin.clone().unwrap_or_default(),
            Field::GeminiArgs => shell_words::join(&cfg.gemini_args),
            Field::GeminiBin => cfg.gemini_bin.clone().unwrap_or_default(),
            Field::OpencodeArgs => shell_words::join(&cfg.opencode_args),
            Field::OpencodeBin => cfg.opencode_bin.clone().unwrap_or_default(),
            Field::Notify => cfg.notify.clone().unwrap_or_default(),
            Field::Theme => cfg.theme.clone().unwrap_or_default(),
            Field::StatusRefreshSecs => {
                cfg.status_refresh_secs.map(|n| n.to_string()).unwrap_or_default()
            }
            Field::TreeWidthPct => {
                cfg.tree_width_pct.map(|n| n.to_string()).unwrap_or_default()
            }
        }
    }

    /// Parse user input into the JSON value to write; `Ok(None)` (blank input)
    /// removes the key. Errors are user-facing one-liners for the error row.
    pub(crate) fn parse(self, raw: &str) -> Result<Option<serde_json::Value>, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        match self {
            Field::ClaudeArgs | Field::CodexArgs | Field::GeminiArgs | Field::OpencodeArgs => {
                let args =
                    shell_words::split(raw).map_err(|e| format!("{}: {e}", self.key()))?;
                Ok(Some(serde_json::json!(args)))
            }
            Field::ClaudeBin
            | Field::Shell
            | Field::CodexBin
            | Field::GeminiBin
            | Field::OpencodeBin => Ok(Some(serde_json::json!(raw))),
            Field::Notify => {
                if NOTIFY_VALUES.contains(&raw) {
                    Ok(Some(serde_json::json!(raw)))
                } else {
                    Err(format!("notify must be one of: {}", NOTIFY_VALUES.join(", ")))
                }
            }
            Field::Theme => {
                if THEME_VALUES.contains(&raw) {
                    Ok(Some(serde_json::json!(raw)))
                } else {
                    Err(format!("theme must be one of: {}", THEME_VALUES.join(", ")))
                }
            }
            Field::StatusRefreshSecs => raw
                .parse::<u64>()
                .map(|n| Some(serde_json::json!(n)))
                .map_err(|_| "status_refresh_secs must be a whole number of seconds".to_string()),
            Field::TreeWidthPct => {
                let n: u16 = raw
                    .parse()
                    .map_err(|_| "tree_width_pct must be a number (percent)".to_string())?;
                Ok(Some(serde_json::json!(n.clamp(
                    super::TREE_WIDTH_MIN,
                    super::TREE_WIDTH_MAX
                ))))
            }
        }
    }

    /// Mirror a just-written value (or removal) into the in-memory config, so
    /// the running app matches the file without a reload.
    pub(crate) fn store(self, cfg: &mut Config, value: Option<&serde_json::Value>) {
        let as_string = |v: Option<&serde_json::Value>| {
            v.and_then(|v| v.as_str()).map(str::to_string)
        };
        let as_args = |v: Option<&serde_json::Value>| -> Vec<String> {
            v.and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        match self {
            Field::ClaudeArgs => cfg.claude_args = as_args(value),
            Field::ClaudeBin => cfg.claude_bin = as_string(value),
            Field::Shell => cfg.shell = as_string(value),
            Field::CodexArgs => cfg.codex_args = as_args(value),
            Field::CodexBin => cfg.codex_bin = as_string(value),
            Field::GeminiArgs => cfg.gemini_args = as_args(value),
            Field::GeminiBin => cfg.gemini_bin = as_string(value),
            Field::OpencodeArgs => cfg.opencode_args = as_args(value),
            Field::OpencodeBin => cfg.opencode_bin = as_string(value),
            Field::Notify => cfg.notify = as_string(value),
            Field::Theme => cfg.theme = as_string(value),
            Field::StatusRefreshSecs => cfg.status_refresh_secs = value.and_then(|v| v.as_u64()),
            Field::TreeWidthPct => {
                cfg.tree_width_pct = value.and_then(|v| v.as_u64()).map(|n| n as u16);
            }
        }
    }
}

/// State of the open settings page.
#[derive(Debug, Default)]
pub(crate) struct SettingsState {
    /// Index into [`FIELDS`]; always in range.
    pub(crate) selected: usize,
    /// `Some` while a field is being edited.
    pub(crate) edit: Option<LineEdit>,
    /// Last commit/validation error, shown under the rows.
    pub(crate) error: Option<String>,
}

impl SettingsState {
    /// The currently selected field.
    pub(crate) fn field(&self) -> Field {
        FIELDS[self.selected]
    }

    pub(crate) fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(crate) fn move_down(&mut self) {
        self.selected = (self.selected + 1).min(FIELDS.len() - 1);
    }
}

/// Render the settings page (full-screen overlay).
pub(crate) fn render_settings_overlay(frame: &mut ratatui::Frame, app: &App) {
    let Some(state) = app.settings.as_ref() else {
        return;
    };
    let th = app.theme;
    let area = frame.area();
    frame.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    for (i, field) in FIELDS.iter().enumerate() {
        let selected = i == state.selected;
        let marker = if selected { "> " } else { "  " };
        let label = format!("  {marker}{:<22}", field.key());
        let label_style = if selected {
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        if selected && let Some(edit) = state.edit.as_ref() {
            let width = (area.width as usize).saturating_sub(label.len() + 2);
            let input = render_input_with_cursor(&edit.buf, edit.cursor, width, th);
            let mut spans = vec![Span::styled(label, label_style)];
            spans.extend(input.spans);
            lines.push(Line::from(spans));
        } else {
            let value = field.current(&app.config);
            let (value, value_style) = if value.is_empty() {
                ("(default)".to_string(), Style::default().fg(th.muted))
            } else if selected {
                (value, Style::default().fg(th.accent))
            } else {
                (value, Style::default().fg(th.text))
            };
            lines.push(Line::from(vec![
                Span::styled(label, label_style),
                Span::styled(value, value_style),
            ]));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!("    keybindings / theme_colors: edit {}", app.config_path.display()),
        Style::default().fg(th.muted),
    ));
    if let Some(err) = state.error.as_ref() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(format!("    {err}"), Style::default().fg(th.error)));
    }
    lines.push(Line::raw(""));
    let footer = if state.edit.is_some() {
        "    Enter save · Esc cancel"
    } else {
        "    j/k move · Enter edit · Esc close"
    };
    lines.push(Line::styled(footer, Style::default().fg(th.muted)));

    let block = Block::default()
        .title(" Settings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_roundtrip_current_parse_store() {
        // One sample per variant — the length check keeps a future field from
        // dodging coverage; current()→parse() must reproduce the same value.
        let samples: &[(Field, &str)] = &[
            (Field::ClaudeArgs, r#"--model opus --append-system-prompt "be terse""#),
            (Field::ClaudeBin, "claude-dev"),
            (Field::Shell, "tmux"),
            (Field::CodexArgs, "--sandbox workspace-write"),
            (Field::CodexBin, "codex-dev"),
            (Field::GeminiArgs, "--approval-mode auto_edit"),
            (Field::GeminiBin, "gemini-dev"),
            (Field::OpencodeArgs, "--model anthropic/claude"),
            (Field::OpencodeBin, "opencode-dev"),
            (Field::Notify, "bell"),
            (Field::Theme, "high-contrast"),
            (Field::StatusRefreshSecs, "5"),
            (Field::TreeWidthPct, "42"),
        ];
        assert_eq!(samples.len(), FIELDS.len());
        let mut cfg = Config::default();
        for (field, raw) in samples {
            let value = field.parse(raw).unwrap();
            assert!(value.is_some());
            field.store(&mut cfg, value.as_ref());
            let reparsed = field.parse(&field.current(&cfg)).unwrap();
            assert_eq!(value, reparsed, "{} must round-trip", field.key());
        }
        // Blank input = unset: store(None) empties every field again.
        for field in FIELDS {
            assert_eq!(field.parse("  ").unwrap(), None);
            field.store(&mut cfg, None);
            assert_eq!(field.current(&cfg), "", "{} must clear", field.key());
        }
    }

    #[test]
    fn claude_args_quoting_roundtrips_embedded_spaces() {
        let cfg = Config {
            claude_args: vec!["--append-system-prompt".into(), "be terse".into()],
            ..Default::default()
        };
        let shown = Field::ClaudeArgs.current(&cfg);
        let value = Field::ClaudeArgs.parse(&shown).unwrap().unwrap();
        assert_eq!(
            value,
            serde_json::json!(["--append-system-prompt", "be terse"]),
            "open→commit unchanged must not split the quoted arg"
        );
    }

    #[test]
    fn claude_args_unbalanced_quote_errors() {
        assert!(Field::ClaudeArgs.parse(r#"--x "unclosed"#).is_err());
    }

    #[test]
    fn notify_and_theme_errors_list_valid_values() {
        let err = Field::Notify.parse("loud").unwrap_err();
        assert!(err.contains("off") && err.contains("both"), "{err}");
        let err = Field::Theme.parse("neon").unwrap_err();
        assert!(err.contains("high-contrast"), "{err}");
    }

    #[test]
    fn numbers_validate_and_tree_width_clamps() {
        assert!(Field::StatusRefreshSecs.parse("x").is_err());
        assert_eq!(
            Field::TreeWidthPct.parse("999").unwrap(),
            Some(serde_json::json!(super::super::TREE_WIDTH_MAX))
        );
        assert_eq!(
            Field::TreeWidthPct.parse("45").unwrap(),
            Some(serde_json::json!(45))
        );
    }

    #[test]
    fn selection_stays_in_range() {
        let mut s = SettingsState::default();
        s.move_up();
        assert_eq!(s.selected, 0);
        for _ in 0..99 {
            s.move_down();
        }
        assert_eq!(s.selected, FIELDS.len() - 1);
        assert_eq!(s.field(), Field::TreeWidthPct);
    }
}
