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
}

impl Config {
    const FILE: &str = "config.json";

    /// Read a config file directly, defaulting on a missing/unreadable/invalid
    /// file (config must never block startup).
    fn read(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Load `config.json` from a base directory.
    pub fn load_from(base: &Path) -> Self {
        Self::read(&base.join(Self::FILE))
    }

    /// Load config: `KOMMAND0_CONFIG` (a file path) if set, else the state dir.
    pub fn load() -> Self {
        if let Some(path) = std::env::var_os("KOMMAND0_CONFIG").filter(|p| !p.is_empty()) {
            return Self::read(Path::new(&path));
        }
        Self::load_from(AppState::state_dir().as_path())
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
    fn malformed_config_degrades_to_default() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("config.json"), "{ not valid json").unwrap();
        let cfg = Config::load_from(tmp.path());
        assert!(cfg.claude_args.is_empty(), "invalid config falls back to default");
    }
}
