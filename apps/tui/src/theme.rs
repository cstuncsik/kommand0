//! Semantic color theme for the app's chrome (not the embedded claude pane,
//! which renders claude's own vt100 colors).
//!
//! Render code uses named roles (`accent`, `selected`, …) instead of raw
//! `Color`s, so a config `theme` (a built-in name) plus `theme_colors`
//! (per-role overrides) can recolor the UI. Modifiers (bold/dim) are not
//! themed — they stay, so even a monochrome palette keeps emphasis.

use std::collections::HashMap;

use ratatui::style::Color;

/// Semantic color roles used across the chrome.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    /// Borders, labels, buttons, prompts, focus.
    pub accent: Color,
    /// The selected tree row.
    pub selected: Color,
    /// Active workspace, clean/up-to-date git state, start affordances.
    pub active: Color,
    /// "Needs you" attention dot + count.
    pub attention: Color,
    /// Diverged/uncommitted git state and caution affordances (cleanup).
    pub dirty: Color,
    /// Errors and destructive affordances.
    pub error: Color,
    /// Inactive/archived, hints, unfocused borders, secondary text.
    pub muted: Color,
    /// Primary body text.
    pub text: Color,
    /// Foreground on a highlighted (bright) background.
    pub inverse: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            accent: Color::Cyan,
            selected: Color::Yellow,
            active: Color::Green,
            attention: Color::Magenta,
            dirty: Color::Yellow,
            error: Color::Red,
            muted: Color::DarkGray,
            text: Color::White,
            inverse: Color::Black,
        }
    }
}

impl Theme {
    fn high_contrast() -> Self {
        Theme {
            accent: Color::LightCyan,
            selected: Color::LightYellow,
            active: Color::LightGreen,
            attention: Color::LightMagenta,
            dirty: Color::LightYellow,
            error: Color::LightRed,
            muted: Color::Gray,
            text: Color::White,
            inverse: Color::Black,
        }
    }

    /// A built-in palette by name (`None` for an unknown name).
    fn builtin(name: &str) -> Option<Self> {
        match name {
            "default" => Some(Self::default()),
            "high-contrast" => Some(Self::high_contrast()),
            _ => None,
        }
    }

    /// Set a role by its config name. Returns `false` for an unknown role.
    fn set(&mut self, role: &str, color: Color) -> bool {
        match role {
            "accent" => self.accent = color,
            "selected" => self.selected = color,
            "active" => self.active = color,
            "attention" => self.attention = color,
            "dirty" => self.dirty = color,
            "error" => self.error = color,
            "muted" => self.muted = color,
            "text" => self.text = color,
            "inverse" => self.inverse = color,
            _ => return false,
        }
        true
    }

    /// Build a theme from a base name + per-role overrides, collecting warnings
    /// for an unknown theme name, unknown roles, and unparseable colors.
    pub(crate) fn build(name: Option<&str>, overrides: &HashMap<String, String>) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut theme = match name {
            None => Self::default(),
            Some(n) => Self::builtin(n).unwrap_or_else(|| {
                warnings.push(format!("unknown theme '{n}', using default"));
                Self::default()
            }),
        };
        // Deterministic order for stable warnings.
        let mut entries: Vec<(&String, &String)> = overrides.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (role, spec) in entries {
            match parse_color(spec) {
                Some(color) => {
                    if !theme.set(role, color) {
                        warnings.push(format!("unknown theme color role '{role}'"));
                    }
                }
                None => warnings.push(format!("invalid color '{spec}' for '{role}'")),
            }
        }
        (theme, warnings)
    }
}

/// Parse a color spec: a named color (`cyan`, `light-red`, `darkgray`), an
/// `#rrggbb` hex, or a 0–255 palette index.
pub(crate) fn parse_color(spec: &str) -> Option<Color> {
    let s = spec.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6
            && let Ok(rgb) = u32::from_str_radix(hex, 16)
        {
            return Some(Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
        }
        return None;
    }
    if let Ok(idx) = s.parse::<u8>() {
        return Some(Color::Indexed(idx));
    }
    match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" | "darkgrey" => Some(Color::DarkGray),
        "white" => Some(Color::White),
        "lightred" => Some(Color::LightRed),
        "lightgreen" => Some(Color::LightGreen),
        "lightyellow" => Some(Color::LightYellow),
        "lightblue" => Some(Color::LightBlue),
        "lightmagenta" => Some(Color::LightMagenta),
        "lightcyan" => Some(Color::LightCyan),
        "reset" | "default" => Some(Color::Reset),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_builtins() {
        assert_eq!(Theme::default().accent, Color::Cyan);
        assert_eq!(Theme::high_contrast().accent, Color::LightCyan);
        assert!(Theme::builtin("default").is_some());
        assert!(Theme::builtin("high-contrast").is_some());
        assert!(Theme::builtin("nope").is_none());
    }

    #[test]
    fn parse_colors() {
        assert_eq!(parse_color("cyan"), Some(Color::Cyan));
        assert_eq!(parse_color("Light-Red"), Some(Color::LightRed));
        assert_eq!(parse_color("darkgray"), Some(Color::DarkGray));
        assert_eq!(parse_color("#ff8800"), Some(Color::Rgb(255, 136, 0)));
        assert_eq!(parse_color("33"), Some(Color::Indexed(33)));
        assert_eq!(parse_color("nope"), None);
        assert_eq!(parse_color("#zzz"), None);
    }

    #[test]
    fn build_applies_base_then_overrides_with_warnings() {
        let mut ov = HashMap::new();
        ov.insert("accent".to_string(), "magenta".to_string());
        ov.insert("nope".to_string(), "red".to_string()); // unknown role
        ov.insert("error".to_string(), "notacolor".to_string()); // bad color
        let (theme, warns) = Theme::build(Some("high-contrast"), &ov);
        // Base applied, then the override wins for accent.
        assert_eq!(theme.accent, Color::Magenta);
        // Untouched role keeps the base (high-contrast).
        assert_eq!(theme.active, Color::LightGreen);
        // error override was invalid -> kept base, warned.
        assert_eq!(theme.error, Color::LightRed);
        assert!(warns.iter().any(|w| w.contains("unknown theme color role 'nope'")));
        assert!(warns.iter().any(|w| w.contains("invalid color 'notacolor'")));
    }

    #[test]
    fn build_warns_on_unknown_theme_name() {
        let (theme, warns) = Theme::build(Some("solarized"), &HashMap::new());
        assert_eq!(theme.accent, Color::Cyan); // fell back to default
        assert!(warns.iter().any(|w| w.contains("unknown theme 'solarized'")));
    }
}
