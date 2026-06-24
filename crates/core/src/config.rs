//! User configuration (separate from `state.json`, which is app-managed).
//!
//! Loaded once at startup from `config.json` in the state dir (or the path in
//! `KOMMAND0_CONFIG`). Every field is optional with a sensible default, and a
//! missing or malformed file degrades to defaults rather than failing — config
//! should never prevent the app from starting.

use std::path::Path;

use serde::Deserialize;

use crate::AppState;

/// Hand-editable settings: `claude` passthrough and a few tunables.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Extra args appended to every embedded `claude` spawn, e.g.
    /// `["--model", "sonnet"]` or `["--permission-mode", "plan"]`.
    pub claude_args: Vec<String>,
    /// Override the `claude` binary. Lower precedence than the
    /// `KOMMAND0_CLAUDE_BIN` env var; falls back to `claude`.
    pub claude_bin: Option<String>,
    /// Seconds between background git-status refreshes (default 2).
    pub status_refresh_secs: Option<u64>,
    /// Tree-pane key rebindings: action name (e.g. `"quit"`) → key specs (e.g.
    /// `["ctrl+q"]`). Parsed/validated by the TUI; an action's configured keys
    /// replace its defaults. Unknown actions / bad specs are warned, not fatal.
    #[serde(default)]
    pub keybindings: std::collections::HashMap<String, Vec<String>>,
    /// Built-in color theme name (e.g. `"high-contrast"`); `None` = default.
    pub theme: Option<String>,
    /// Per-role color overrides: role (e.g. `"accent"`) → color spec (e.g.
    /// `"blue"`, `"#ff8800"`). Parsed by the TUI; bad roles/colors are warned.
    #[serde(default)]
    pub theme_colors: std::collections::HashMap<String, String>,
    /// Notify when a backgrounded session goes quiet with unseen output:
    /// `"off"` (default), `"bell"` (terminal bell), `"desktop"` (OS
    /// notification), or `"both"`. Parsed by the TUI; an unknown value warns.
    pub notify: Option<String>,
}

impl Config {
    const FILE: &str = "config.json";

    /// Read a config file, returning a warning if a present file fails to parse
    /// (a missing/unreadable file is a silent default). Any parse error discards
    /// the WHOLE file — config must never block startup.
    fn read(path: &Path) -> (Self, Option<String>) {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return (Self::default(), None); // missing/unreadable -> silent default
        };
        match serde_json::from_str(&contents) {
            Ok(cfg) => (cfg, None),
            Err(e) => (
                Self::default(),
                Some(format!("ignoring invalid config {}: {e}", path.display())),
            ),
        }
    }

    /// Load `config.json` from a base directory (defaults on any problem).
    pub fn load_from(base: &Path) -> Self {
        Self::read(&base.join(Self::FILE)).0
    }

    /// Load config: `KOMMAND0_CONFIG` (a file path) if set, else the state dir.
    pub fn load() -> Self {
        Self::load_checked().0
    }

    /// Like [`Self::load`] but also returns a human-readable warning when a
    /// present config file couldn't be parsed (so the caller can surface it).
    pub fn load_checked() -> (Self, Option<String>) {
        if let Some(path) = std::env::var_os("KOMMAND0_CONFIG").filter(|p| !p.is_empty()) {
            return Self::read(Path::new(&path));
        }
        Self::read(&AppState::state_dir().join(Self::FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_config_is_default() {
        let tmp = TempDir::new().unwrap();
        let cfg = Config::load_from(tmp.path());
        assert!(cfg.claude_args.is_empty());
        assert_eq!(cfg.claude_bin, None);
        assert_eq!(cfg.status_refresh_secs, None);
    }

    #[test]
    fn parses_fields_and_defaults_the_rest() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("config.json"),
            r#"{ "claude_args": ["--model", "sonnet"] }"#,
        )
        .unwrap();
        let cfg = Config::load_from(tmp.path());
        assert_eq!(cfg.claude_args, vec!["--model", "sonnet"]);
        assert_eq!(cfg.claude_bin, None); // absent field defaults
    }

    #[test]
    fn malformed_config_degrades_to_default_with_a_warning() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        let cfg = Config::load_from(tmp.path());
        assert!(cfg.claude_args.is_empty(), "invalid config falls back to default");
        // The warning path (so a typo isn't invisible).
        let (cfg, warn) = Config::read(&path);
        assert!(cfg.claude_args.is_empty());
        assert!(warn.is_some_and(|w| w.contains("invalid config")), "warns on a present-but-bad file");
        // A missing file is a silent default (no warning).
        let (_, warn) = Config::read(&tmp.path().join("absent.json"));
        assert!(warn.is_none());
    }
}
