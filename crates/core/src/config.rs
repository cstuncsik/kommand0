//! User configuration (separate from `state.json`, which is app-managed).
//!
//! Loaded once at startup from `config.json` in the state dir (or the path in
//! `KOMMAND0_CONFIG`). Every field is optional with a sensible default, and a
//! missing or malformed file degrades to defaults rather than failing — config
//! should never prevent the app from starting. The TUI settings page writes
//! single fields back via [`Config::update_file`]; everything else about the
//! file stays hand-edited.

use std::path::{Path, PathBuf};

use anyhow::Context;
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
    /// Command for a shell session tab (`Ctrl+A s`). Defaults to `$SHELL`, then
    /// `/bin/sh`. Can be any command — e.g. `"tmux"` for splits inside the tab.
    pub shell: Option<String>,
    /// Tree (left) pane width as a percent of the terminal. Parsed here; clamped
    /// into a sane range by the TUI (matching the `notify`/`theme` "parsed by the
    /// TUI" convention). Live `<`/`>` adjust from here.
    pub tree_width_pct: Option<u16>,
}

impl Config {
    /// The config filename — also what the legacy-profiles migration moves.
    pub(crate) const FILE: &str = "config.json";

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
        Self::read(&Self::effective_path())
    }

    /// `KOMMAND0_CONFIG` when set and non-empty — the global config-path
    /// override (a set-but-empty value counts as unset, matching the other
    /// `KOMMAND0_*` overrides). Shared by [`Self::effective_path`] and the
    /// legacy-profiles migration so the two can never disagree.
    pub(crate) fn path_override() -> Option<PathBuf> {
        std::env::var_os("KOMMAND0_CONFIG")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
    }

    /// The path [`Self::load_checked`] reads (and the settings page writes):
    /// `KOMMAND0_CONFIG` if set and non-empty, else `config.json` in the state dir.
    pub fn effective_path() -> PathBuf {
        Self::path_override().unwrap_or_else(|| AppState::state_dir().join(Self::FILE))
    }

    /// Set (`Some`) or remove (`None`) one top-level key in the config file at
    /// `path`, preserving every other key — including keys this version doesn't
    /// know about. A missing file starts from `{}`; an unreadable, unparseable,
    /// or non-object file is refused with an error (config.json is user-owned:
    /// never reset, back up, or relocate it). The merged result must still
    /// deserialize as a `Config`, so a wrong-shaped value can't poison the next
    /// startup load. The write is atomic — unique temp + rename next to the
    /// resolved target, so a symlinked config keeps pointing at its source.
    pub fn update_file(path: &Path, key: &str, value: Option<serde_json::Value>) -> anyhow::Result<()> {
        let mut root = match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
                Ok(serde_json::Value::Object(map)) => map,
                Ok(_) => anyhow::bail!("{} is not a JSON object — fix it by hand", path.display()),
                Err(e) => anyhow::bail!("{} is invalid JSON ({e}) — fix it by hand", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        match value {
            Some(v) => {
                root.insert(key.to_string(), v);
            }
            None => {
                root.remove(key);
            }
        }
        let merged = serde_json::Value::Object(root);
        serde_json::from_value::<Self>(merged.clone())
            .with_context(|| format!("refusing to write {}: invalid {key} value", path.display()))?;
        // Resolve symlinks so the rename lands on the real file, not over the
        // link. canonicalize needs the file to exist; fall back to resolving
        // the parent (creating it first — KOMMAND0_CONFIG may point anywhere).
        let target = std::fs::canonicalize(path).unwrap_or_else(|_| {
            match path.parent().filter(|p| !p.as_os_str().is_empty()) {
                Some(dir) => {
                    let _ = std::fs::create_dir_all(dir); // failure surfaces at the write below
                    match (std::fs::canonicalize(dir), path.file_name()) {
                        (Ok(d), Some(f)) => d.join(f),
                        _ => path.to_path_buf(),
                    }
                }
                None => path.to_path_buf(),
            }
        });
        let data = serde_json::to_string_pretty(&merged)?;
        let file_name = target
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| Self::FILE.to_string());
        // Unique temp name in the same dir, like AppState::save_to.
        let tmp = target.with_file_name(format!("{file_name}.tmp.{}", crate::generate_id()));
        std::fs::write(&tmp, &data)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &target)
            .with_context(|| format!("failed to replace {}", target.display()))?;
        Ok(())
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
    fn tree_width_pct_parses_and_defaults_to_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, r#"{ "tree_width_pct": 45 }"#).unwrap();
        assert_eq!(Config::load_from(tmp.path()).tree_width_pct, Some(45));
        // Absent field defaults to None (the TUI then seeds the default).
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(Config::load_from(tmp.path()).tree_width_pct, None);
    }

    #[test]
    fn update_file_preserves_unknown_keys() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, r#"{ "future_knob": true, "claude_bin": "x" }"#).unwrap();
        Config::update_file(&path, "claude_bin", Some("claude-dev".into())).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["future_knob"], true, "unknown key survives a rewrite");
        assert_eq!(Config::load_from(tmp.path()).claude_bin.as_deref(), Some("claude-dev"));
    }

    #[test]
    fn update_file_creates_missing_file_and_removes_on_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("config.json");
        let args = serde_json::json!(["--model", "opus"]);
        Config::update_file(&path, "claude_args", Some(args)).unwrap();
        let cfg = Config::load_from(&tmp.path().join("nested"));
        assert_eq!(cfg.claude_args, vec!["--model", "opus"]);
        // None removes the key entirely (not `null`).
        Config::update_file(&path, "claude_args", None).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw.get("claude_args").is_none());
    }

    #[test]
    fn update_file_refuses_invalid_json_and_non_object_root() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        for bad in ["{ not valid json", "[1, 2]", "\"str\"", "42"] {
            std::fs::write(&path, bad).unwrap();
            let err = Config::update_file(&path, "claude_bin", Some("x".into()));
            assert!(err.is_err(), "must refuse {bad:?}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                bad,
                "user-owned file left untouched"
            );
        }
    }

    #[test]
    fn update_file_refuses_wrong_shape() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        // claude_args must be an array of strings; a bare string would poison
        // the whole file at the next startup load.
        let err = Config::update_file(&path, "claude_args", Some("sonnet".into()));
        assert!(err.is_err());
        assert!(!path.exists(), "nothing written on refusal");
    }

    #[test]
    fn update_file_writes_through_a_symlinked_config() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("dotfiles-config.json");
        std::fs::write(&real, r#"{ "claude_bin": "x" }"#).unwrap();
        let link = tmp.path().join("config.json");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, &link).unwrap();
            Config::update_file(&link, "claude_bin", Some("y".into())).unwrap();
            assert!(
                std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
                "link stays a link"
            );
            let raw: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&real).unwrap()).unwrap();
            assert_eq!(raw["claude_bin"], "y", "write landed in the link target");
        }
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
