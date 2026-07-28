pub mod config;
pub mod git;
pub mod id;
pub mod repo;
pub mod session;
pub mod workspace;
pub mod worktree;

pub use config::Config;
pub use git::{
    BranchStatus, FileDiff, PrChecks, PrReview, PrState, PrStatus, branch_status,
    cleanup_merged_workspace, diff_files_vs_default_branch, pr_statuses,
};
pub use id::generate_id;
pub use repo::{RepoEntry, run_git_status};
pub use session::{Session, SessionStatus};
pub use workspace::Workspace;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    /// On-disk schema version, stamped on every save. A file without it predates
    /// versioning and loads as v1 (see [`legacy_state_version`]); [`AppState::migrate`]
    /// brings any older file up to `STATE_VERSION` on load, so an upgrade can evolve
    /// the shape without bricking an existing `state.json`.
    #[serde(default = "legacy_state_version")]
    pub version: u32,
    pub repos: Vec<RepoEntry>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// Per-workspace session entries, one per session tab, in tab order: a
    /// Claude session id (caller-assigned UUID, resumed on reopen via
    /// `claude --resume <id>`), or a `shell:<uuid>` sentinel for a shell tab
    /// (reopens as a fresh shell). So reopening a workspace restores its whole
    /// tab row across app restarts.
    #[serde(default, deserialize_with = "de_embedded_sessions")]
    pub embedded_sessions: HashMap<String, Vec<String>>,
    /// Optional per-session display titles, keyed workspace-id → session-id →
    /// title. Additive: absent for un-renamed sessions, so old state files (and
    /// the common case) carry nothing. Kept in lockstep with `embedded_sessions`
    /// — pruned whenever a session id or workspace is removed — so a title never
    /// outlives its session.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub embedded_titles: HashMap<String, HashMap<String, String>>,
}

/// A `state.json` with no `version` field predates schema versioning; that shape
/// is schema v1, so a missing value deserializes as 1 — deliberately NOT the
/// current version, or a genuinely-old file would skip the migrations between
/// v1 and today.
fn legacy_state_version() -> u32 {
    1
}

impl Default for AppState {
    // A freshly-created state is at the current schema (unlike a derived Default,
    // which would leave `version` at 0 and mis-stamp a brand-new file).
    fn default() -> Self {
        Self {
            version: Self::STATE_VERSION,
            repos: Vec::new(),
            workspaces: Vec::new(),
            sessions: Vec::new(),
            embedded_sessions: HashMap::new(),
            embedded_titles: HashMap::new(),
        }
    }
}

/// Tolerates the legacy single-string form (`{"w1":"uuid"}`) and an explicit
/// `null`, mapping both into the current `Vec<String>` shape. Serialization
/// always emits the array form.
fn de_embedded_sessions<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    let opt: Option<HashMap<String, Option<OneOrMany>>> = Option::deserialize(deserializer)?;
    Ok(opt
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(ws, v)| {
            let ids = match v {
                Some(OneOrMany::One(id)) => vec![id],
                Some(OneOrMany::Many(ids)) => ids,
                None => return None, // tolerate a per-entry null
            };
            if ids.is_empty() {
                None
            } else {
                Some((ws, ids))
            }
        })
        .collect())
}

/// The profile used when `--profile` is omitted (also the migration target
/// for a pre-profiles layout, and what the TUI hides in its tree title).
pub const DEFAULT_PROFILE: &str = "default";

/// The env var a profiled TUI exports to its embedded sessions so a nested
/// `kmd`/`kommand0` targets the same profile. Read by
/// [`AppState::init_profile`] (an explicit `--profile` beats it;
/// `KOMMAND0_STATE_DIR` silently wins). One const for the read and write
/// sites so they can never drift.
pub const PROFILE_ENV: &str = "KOMMAND0_PROFILE";

/// The profile for this process, recorded once at startup via
/// [`AppState::init_profile`] (absent = [`DEFAULT_PROFILE`]). Process-global
/// on purpose: [`AppState::state_dir`] has no-arg call sites throughout the
/// crates, and "which dir" is already ambient state (the env override is read
/// the same way).
static PROFILE: OnceLock<String> = OnceLock::new();

/// Prefix marking a persisted embedded-session entry as a shell tab. The one
/// place the sentinel shape is built/inspected.
const SHELL_SESSION_PREFIX: &str = "shell:";

impl AppState {
    /// The profile-independent dev-build data root (also the release fallback
    /// when the platform data dir can't be resolved).
    const DEV_BASE_DIR: &str = ".kommand0-dev";
    const STATE_FILE: &str = "state.json";
    /// Current on-disk schema version. Bump this when the `state.json` shape
    /// changes in a way that needs a migration, and add the step in [`Self::migrate`].
    const STATE_VERSION: u32 = 1;

    /// Bring a just-loaded state up to `STATE_VERSION`. A file with no `version`
    /// loaded as v1 ([`legacy_state_version`]). No migrations exist yet (v1 is
    /// current), so this only stamps the version; a future schema bump adds
    /// `while state.version < Self::STATE_VERSION { …transform…; state.version += 1 }`.
    fn migrate(state: &mut Self) {
        state.version = Self::STATE_VERSION;
    }

    /// `KOMMAND0_STATE_DIR` when set and non-empty — the exact-dir escape
    /// hatch (no `profiles/` suffix). A set-but-empty value counts as unset;
    /// every consumer ([`Self::state_dir`], [`Self::init_profile`],
    /// [`Self::migrate_legacy_profiles`]) shares this predicate so they can
    /// never disagree on that edge.
    fn state_dir_override() -> Option<PathBuf> {
        std::env::var_os("KOMMAND0_STATE_DIR")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
    }

    /// The profile-independent data root: `.kommand0-dev/` relative to the
    /// current directory in debug builds, else the platform data dir
    /// (`~/Library/Application Support/kommand0` on macOS,
    /// `~/.local/share/kommand0` on Linux).
    fn base_dir() -> PathBuf {
        if cfg!(debug_assertions) {
            PathBuf::from(Self::DEV_BASE_DIR)
        } else {
            dirs::data_dir()
                .map(|d| d.join("kommand0"))
                .unwrap_or_else(|| PathBuf::from(Self::DEV_BASE_DIR))
        }
    }

    /// Resolve the state directory.
    ///
    /// Priority: `KOMMAND0_STATE_DIR` env var (an exact directory — no
    /// profile suffix), then `<base>/profiles/<profile>`, where `<base>` is
    /// [`Self::base_dir`] and `<profile>` is the profile recorded via
    /// [`Self::init_profile`] (`default` when nothing selected one).
    pub fn state_dir() -> PathBuf {
        if let Some(dir) = Self::state_dir_override() {
            return dir; // exact dir, unchanged — the env escape hatch
        }
        let profile = PROFILE.get().map(String::as_str).unwrap_or(DEFAULT_PROFILE);
        Self::base_dir().join("profiles").join(profile)
    }

    /// Validate a would-be profile name: it becomes a path segment under
    /// `profiles/`, so allow only `[A-Za-z0-9._-]+` and at most 64 bytes;
    /// reject `""`, `"."` (would alias the profiles root itself), `".."`, and
    /// a leading `-` (an arg-parsing foot-gun — `--profile --resume` must not
    /// mint a profile; mirrors `validate_new_workspace_name`).
    fn validate_profile_name(name: &str) -> Result<(), String> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.len() > 64
            || name.starts_with('-')
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
        {
            // The name failed validation, so it may carry control/escape
            // bytes — never echo it raw into a terminal.
            return Err(format!(
                "invalid profile name '{}': use 1-64 characters from letters, digits, '.', '_', '-' (not '.', '..', or leading '-')",
                name.escape_debug()
            ));
        }
        Ok(())
    }

    /// Decide the profile from its three inputs — pure, so the precedence
    /// table is unit-testable without touching process env or the OnceLock.
    ///
    /// - an explicit `--profile` flag + `KOMMAND0_STATE_DIR` is an error (the
    ///   env var targets one exact directory — combining them is ambiguous);
    /// - `KOMMAND0_STATE_DIR` alone wins SILENTLY over `KOMMAND0_PROFILE`
    ///   (the exact-dir contract: children spawned by an env-mode parent
    ///   stay hermetic even with a stale profile var in their env);
    /// - else the flag beats `KOMMAND0_PROFILE`; whichever is used must be a
    ///   valid name.
    fn resolve_profile(
        flag: Option<&str>,
        env_profile: Option<&str>,
        state_dir_overridden: bool,
    ) -> Result<Option<String>, String> {
        if state_dir_overridden {
            if flag.is_some() {
                return Err(
                    "--profile cannot be combined with KOMMAND0_STATE_DIR; unset the variable or drop the flag"
                        .to_string(),
                );
            }
            return Ok(None);
        }
        match flag.or(env_profile) {
            Some(name) => {
                Self::validate_profile_name(name)?;
                Ok(Some(name.to_string()))
            }
            None => Ok(None),
        }
    }

    /// Resolve and record this process's profile: the `--profile` flag, else
    /// the `KOMMAND0_PROFILE` env var (set by a profiled TUI for its embedded
    /// sessions, so a nested `kmd`/`kommand0` targets the same profile —
    /// same non-empty semantics as the other overrides; an invalid value is
    /// a loud startup error, not a silent default). Returns the EFFECTIVE
    /// name ([`DEFAULT_PROFILE`] when nothing selected one) for the caller's
    /// label logic. Call once at startup, before any state/config/log access
    /// and before spawning threads.
    pub fn init_profile(flag: Option<&str>) -> Result<String, String> {
        // var_os + lossy (not var().ok()): a non-UTF8 value must reach
        // validation and fail loudly, not silently count as unset.
        let env_profile = std::env::var_os(PROFILE_ENV)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string_lossy().into_owned());
        let resolved =
            Self::resolve_profile(flag, env_profile.as_deref(), Self::state_dir_override().is_some())?;
        match resolved {
            Some(name) => {
                // A second call is unreachable by construction (both binaries
                // init the profile exactly once, pre-thread-spawn) — assert
                // that in debug; in release the first value simply stays.
                let set = PROFILE.set(name.clone());
                debug_assert!(set.is_ok(), "init_profile called more than once");
                Ok(name)
            }
            None => Ok(DEFAULT_PROFILE.to_string()),
        }
    }

    /// One-time migration of the pre-profiles layout: when `<base>/profiles/`
    /// doesn't exist yet and a legacy `state.json` / `config.json` sits at the
    /// `<base>` root, move them into `<base>/profiles/default/`. Legacy
    /// `worktrees/`, `sessions/`, and `kommand0.log` stay at the old root —
    /// state stores worktree and session-log paths that are absolute in
    /// release builds (and unchanged either way), so they keep resolving. A
    /// no-op when `KOMMAND0_STATE_DIR` is set (non-empty): the env var
    /// targets an exact directory outside the profiles layout. When
    /// `KOMMAND0_CONFIG` is set (non-empty), config.json also stays at the
    /// root (the override may point at that very file). On `Err` the caller
    /// must abort startup — proceeding with fresh state would mask the legacy
    /// file (and, after the first save, permanently orphan it).
    pub fn migrate_legacy_profiles() -> anyhow::Result<()> {
        if Self::state_dir_override().is_some() {
            return Ok(());
        }
        // An active KOMMAND0_CONFIG freezes config.json: the override may
        // point AT the legacy root file — moving it would silently break the
        // user's settings — and when it points elsewhere the root config.json
        // is unused anyway.
        let config_overridden = Config::path_override().is_some();
        Self::migrate_legacy_profiles_at(&Self::base_dir(), config_overridden)
    }

    /// [`Self::migrate_legacy_profiles`] against an explicit base dir (the
    /// test seam; `config_overridden` threads the KOMMAND0_CONFIG decision so
    /// tests never touch process env).
    fn migrate_legacy_profiles_at(base: &Path, config_overridden: bool) -> anyhow::Result<()> {
        let profiles = base.join("profiles");
        // Idempotence guard: an existing profiles/ dir — even an empty one —
        // means the layout is already current. `exists()` stats through
        // symlinks deliberately: a dangling link counts as absent, so a failed
        // migration below stays retriable instead of being masked.
        if profiles.exists() {
            return Ok(());
        }
        // state.json moves first: if the move fails partway, the orphan is at
        // worst config.json (defaults; recoverable). With KOMMAND0_CONFIG
        // active, config.json stays at the root (see the wrapper above).
        let candidates: &[&str] = if config_overridden {
            &[Self::STATE_FILE]
        } else {
            &[Self::STATE_FILE, Config::FILE]
        };
        let legacy: Vec<&str> = candidates
            .iter()
            .copied()
            .filter(|f| base.join(f).exists())
            .collect();
        if legacy.is_empty() {
            // Fresh install: create nothing (the first save / log open creates
            // the profile dir lazily).
            return Ok(());
        }
        let default_dir = profiles.join(DEFAULT_PROFILE);
        // On failure, best-effort remove the dirs we may have just created.
        // `remove_dir` is rmdir(2) — empty-only, never recursive — so it's a
        // no-op once any file has landed; it exists to prevent an empty
        // profiles/ husk that would trip the guard above on every later run
        // and silently mask the legacy files.
        let rollback = |err: anyhow::Error| -> anyhow::Result<()> {
            let _ = fs::remove_dir(&default_dir);
            let _ = fs::remove_dir(&profiles);
            Err(err)
        };
        if let Err(e) = fs::create_dir_all(&default_dir) {
            return rollback(anyhow::anyhow!(
                "failed to migrate {} into {} ({e}); move it there by hand and retry",
                base.join(legacy[0]).display(),
                default_dir.display()
            ));
        }
        for file in legacy {
            let from = base.join(file);
            let to = default_dir.join(file);
            // A symlinked legacy file is materialized — copy through the link
            // (follows it), then drop the link — because a relative link would
            // dangle when moved a level deeper. Regular files keep the atomic
            // same-filesystem rename.
            let is_link = from
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            let moved = if is_link {
                fs::copy(&from, &to).and_then(|_| fs::remove_file(&from))
            } else {
                fs::rename(&from, &to)
            };
            if let Err(e) = moved {
                // Lost race: a concurrent process won this file's move between
                // our guard and the rename — keep going so any remaining
                // legacy file (e.g. config.json after state.json's race loss)
                // still migrates.
                if e.kind() == std::io::ErrorKind::NotFound && !from.exists() && to.exists() {
                    continue;
                }
                // A failed copy (the symlink path) can leave a PARTIAL
                // destination the empty-only rollback can't remove — drop it,
                // or the guard would mask the intact legacy link on every
                // later run. Deliberately after the race check: a concurrent
                // winner's file must never be deleted. (Untested: a
                // deterministic mid-copy failure can't be staged without
                // device tricks — same accepted class as the rollback lines.)
                if is_link {
                    let _ = fs::remove_file(&to);
                }
                return rollback(anyhow::anyhow!(
                    "failed to migrate {} into {} ({e}); move it there by hand and retry",
                    from.display(),
                    default_dir.display()
                ));
            }
        }
        Ok(())
    }

    /// Rename profile `old` to `new`: move `<base>/profiles/<old>` to
    /// `<base>/profiles/<new>`, rewrite the profile's own stored paths
    /// (workspace `worktree_path`/`working_dir` and session `log_file`
    /// prefixes under the old dir) in its state.json, and best-effort follow
    /// up with `git worktree repair` for each moved worktree (the repos'
    /// gitdir links) and a move of each moved dir's Claude Code project store
    /// (`~/.claude/projects/<slug>` — claude keys sessions by cwd, so
    /// `--resume` would otherwise miss). Returns `(rewritten, migrated,
    /// warnings)`: how many worktree/session paths were rewritten, how many
    /// Claude project dirs moved, and any best-effort steps that failed
    /// (rerunnable by hand — the rename and state rewrite are already
    /// complete and correct). A failure after the dir move (corrupt
    /// state.json, save error) moves the dir back, so an `Err` leaves the
    /// original profile intact. Legacy-root worktrees/sessions (outside the
    /// profile dir) are left alone. Errors when `KOMMAND0_STATE_DIR` is set
    /// (non-empty): that override targets one exact directory — there is no
    /// profiles tree to rename in. Renaming `default` is allowed — the next
    /// plain run just starts a fresh default. Renaming a profile while any
    /// kommand0/kmd instance is running on it is unsupported (nothing locks
    /// the directory).
    pub fn rename_profile(old: &str, new: &str) -> anyhow::Result<(usize, usize, Vec<String>)> {
        if Self::state_dir_override().is_some() {
            bail!(
                "profile rename is unavailable when KOMMAND0_STATE_DIR is set; \
                 it targets one exact directory, not a profiles tree"
            );
        }
        // Claude Code's per-directory session store lives under
        // `$CLAUDE_CONFIG_DIR` (set and non-empty) else `~/.claude`, in
        // `projects/` — threaded into the seam so tests never touch env/home.
        let claude_projects = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
            .map(|d| d.join("projects"))
            .unwrap_or_default();
        Self::rename_profile_at(&Self::base_dir(), old, new, &claude_projects)
    }

    /// Claude Code's project-store dir name for a working directory: the
    /// path string with every non-ASCII-alphanumeric char replaced by `-`
    /// (the rule extracted from the claude binary:
    /// `path.replace(/[^a-zA-Z0-9]/g, "-")`). JS regexes operate per UTF-16
    /// code unit, so a BMP char becomes one dash and an astral-plane char
    /// (e.g. an emoji in a workspace name) becomes two.
    fn claude_project_slug(path: &Path) -> String {
        path.to_string_lossy()
            .chars()
            .flat_map(|c| {
                if c.is_ascii_alphanumeric() {
                    std::iter::repeat_n(c, 1)
                } else {
                    std::iter::repeat_n('-', c.len_utf16())
                }
            })
            .collect()
    }

    /// [`Self::rename_profile`] against an explicit base dir and Claude
    /// projects root (the test seam).
    fn rename_profile_at(
        base: &Path,
        old: &str,
        new: &str,
        claude_projects: &Path,
    ) -> anyhow::Result<(usize, usize, Vec<String>)> {
        Self::validate_profile_name(old).map_err(|e| anyhow::anyhow!(e))?;
        Self::validate_profile_name(new).map_err(|e| anyhow::anyhow!(e))?;
        if old == new {
            bail!("old and new profile names are the same: '{old}'");
        }
        let src = base.join("profiles").join(old);
        let dst = base.join("profiles").join(new);
        if !src.is_dir() {
            bail!("profile '{old}' not found at {}", src.display());
        }
        if dst.exists() {
            bail!("profile '{new}' already exists at {}", dst.display());
        }
        fs::rename(&src, &dst).with_context(|| {
            format!("failed to rename {} to {}", src.display(), dst.display())
        })?;

        // Fresh/empty profile: the rename alone is complete.
        if !dst.join(Self::STATE_FILE).exists() {
            return Ok((0, 0, Vec::new()));
        }

        // Rewrite stored paths under the old profile dir. Two prefix forms
        // cover how paths were recorded at creation time: the base verbatim
        // (session logs — relative in debug builds), and, for a relative
        // base, its cwd-absolutized form (worktree paths:
        // `prepare_worktree_dir` absolutizes the same way).
        let mut prefixes: Vec<(String, String)> = vec![(
            src.to_string_lossy().into_owned(),
            dst.to_string_lossy().into_owned(),
        )];
        if src.is_relative() {
            let cwd = std::env::current_dir().unwrap_or_default();
            prefixes.push((
                cwd.join(&src).to_string_lossy().into_owned(),
                cwd.join(&dst).to_string_lossy().into_owned(),
            ));
        }
        // Path-boundary match only, so profile `work` never rewrites `work2`.
        let rewrite = |path: &str| -> Option<String> {
            for (from, to) in &prefixes {
                if let Some(rest) = path.strip_prefix(from.as_str())
                    && (rest.is_empty() || rest.starts_with(std::path::MAIN_SEPARATOR))
                {
                    return Some(format!("{to}{rest}"));
                }
            }
            None
        };

        // The load→rewrite→save phase can still fail (corrupt state.json,
        // save error). Nothing besides the dir move has happened yet, so on
        // failure move the dir back — an `Err` then leaves the exact original.
        type Rewritten = (usize, Vec<String>, Vec<(String, String)>, Vec<(String, String)>);
        let result = (|| -> anyhow::Result<Rewritten> {
            let mut state = Self::load_from(&dst)?;
            let mut rewritten = 0usize;
            let mut warnings: Vec<String> = Vec::new();
            let mut repairs: Vec<(String, String)> = Vec::new(); // (repo path, new worktree path)
            let mut dir_moves: Vec<(String, String)> = Vec::new(); // workspace dirs (old, new)
            for ws in &mut state.workspaces {
                if let Some(wt) = &ws.worktree_path
                    && let Some(new_wt) = rewrite(wt)
                {
                    match state.repos.iter().find(|r| r.id == ws.repo_id) {
                        Some(repo) => repairs.push((repo.path.clone(), new_wt.clone())),
                        None => warnings.push(format!(
                            "no repo found for workspace '{}'; run `git worktree repair {new_wt}` in it by hand",
                            ws.name
                        )),
                    }
                    dir_moves.push((wt.clone(), new_wt.clone()));
                    ws.worktree_path = Some(new_wt);
                    rewritten += 1;
                }
                // A worktree-backed workspace's working_dir IS its worktree
                // path — it must follow, or sessions would spawn in the dead
                // directory.
                if let Some(new_dir) = rewrite(&ws.working_dir) {
                    dir_moves.push((ws.working_dir.clone(), new_dir.clone()));
                    ws.working_dir = new_dir;
                }
            }
            for session in &mut state.sessions {
                if let Some(new_log) = rewrite(&session.log_file) {
                    session.log_file = new_log;
                    rewritten += 1;
                }
            }
            state.save_to(&dst)?;
            Ok((rewritten, warnings, repairs, dir_moves))
        })();
        let (rewritten, mut warnings, repairs, mut dir_moves) = match result {
            Ok(v) => v,
            Err(e) => {
                return match fs::rename(&dst, &src) {
                    Ok(()) => Err(e),
                    Err(undo) => Err(e.context(format!(
                        "undoing the rename also failed ({undo}); move {} back to {} by hand",
                        dst.display(),
                        src.display()
                    ))),
                };
            }
        };

        // Best-effort git-side fix-up: repair each moved worktree from its
        // repo's root so `.git/worktrees/<n>/gitdir` (and the worktree's
        // `.git` file) point at the new location. A failure is a warning, not
        // an error — the rename and state rewrite above are already correct.
        for (repo_path, worktree) in repairs {
            let out = std::process::Command::new("git")
                .args(["-C", &repo_path, "worktree", "repair", &worktree])
                .output();
            match out {
                Ok(out) if out.status.success() => {}
                Ok(out) => warnings.push(format!(
                    "couldn't repair worktree {worktree}: {} (rerun `git worktree repair` in {repo_path})",
                    String::from_utf8_lossy(&out.stderr).trim()
                )),
                Err(e) => warnings.push(format!(
                    "couldn't repair worktree {worktree}: {e} (rerun `git worktree repair` in {repo_path})"
                )),
            }
        }

        // Claude Code keys its session store by a slug of each session's cwd
        // (`<projects>/<slug>/<uuid>.jsonl`) — move the store dirs of moved
        // workspace dirs along, or every `--resume` in the renamed profile
        // misses and starts fresh. Same best-effort tier as the repair above;
        // silently a no-op on machines without a claude store.
        let mut migrated = 0usize;
        if claude_projects.exists() {
            dir_moves.sort();
            dir_moves.dedup(); // worktree_path and working_dir are usually the same dir
            for (old_dir, new_dir) in dir_moves {
                let old_slug = Self::claude_project_slug(Path::new(&old_dir));
                let new_slug = Self::claude_project_slug(Path::new(&new_dir));
                if old_slug.len() > 200 || new_slug.len() > 200 {
                    // claude truncates + hashes long slugs — we can't
                    // replicate the hash, so leave the store and name the fix.
                    warnings.push(format!(
                        "couldn't migrate Claude sessions for {new_dir}: the store dir name \
                         exceeds 200 chars (claude hashes those); move it by hand under {}",
                        claude_projects.display()
                    ));
                    continue;
                }
                let old_store = claude_projects.join(&old_slug);
                if !old_store.exists() {
                    continue; // no claude sessions for that dir
                }
                let new_store = claude_projects.join(&new_slug);
                if new_store.exists() {
                    warnings.push(format!(
                        "couldn't migrate Claude sessions: {} already exists (left {} in place)",
                        new_store.display(),
                        old_store.display()
                    ));
                    continue;
                }
                match fs::rename(&old_store, &new_store) {
                    Ok(()) => migrated += 1,
                    Err(e) => warnings.push(format!(
                        "couldn't migrate Claude sessions {} to {} ({e}); move it by hand",
                        old_store.display(),
                        new_store.display()
                    )),
                }
            }
        }
        Ok((rewritten, migrated, warnings))
    }

    /// Load state from the given base directory. Returns default if no state file
    /// exists, and ERRORS on a corrupt file — so a CLI command aborts (leaving the
    /// bad file untouched and recoverable) rather than silently resetting. The TUI,
    /// which can't usefully abort to a corrupted alt-screen, uses
    /// [`Self::load_checked_from`] to degrade gracefully instead.
    pub fn load_from(base: &Path) -> anyhow::Result<Self> {
        let path = base.join(Self::STATE_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut state: Self = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Self::migrate(&mut state);
        Ok(state)
    }

    /// Like [`Self::load_from`] but never aborts on a corrupt file: it backs the
    /// bad file up and resets to default, returning a warning to surface (mirroring
    /// how a bad `config.json` degrades). For the TUI's startup load.
    pub fn load_checked_from(base: &Path) -> anyhow::Result<(Self, Option<String>)> {
        let path = base.join(Self::STATE_FILE);
        if !path.exists() {
            return Ok((Self::default(), None));
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        match serde_json::from_str::<Self>(&data) {
            Ok(mut state) => {
                Self::migrate(&mut state);
                Ok((state, None))
            }
            Err(e) => {
                // Back up the bad file without clobbering an earlier backup — the
                // first/original is the most valuable one to keep.
                let mut backup = path.with_file_name(format!("{}.corrupt", Self::STATE_FILE));
                if backup.exists() {
                    backup = path
                        .with_file_name(format!("{}.corrupt.{}", Self::STATE_FILE, generate_id()));
                }
                let _ = fs::rename(&path, &backup);
                Ok((
                    Self::default(),
                    Some(format!(
                        "ignoring corrupt state ({e}); backed up to {} and reset",
                        backup.display()
                    )),
                ))
            }
        }
    }

    /// Save state to the given base directory, creating it if needed. The write
    /// is atomic — a uniquely-named temp file in the same dir is renamed over
    /// `state.json` — so a process crash mid-write can't leave a partially-written
    /// state file. (Power-loss corruption is caught by the corrupt-load backstop.)
    pub fn save_to(&self, base: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(base)
            .with_context(|| format!("failed to create {}", base.display()))?;
        let path = base.join(Self::STATE_FILE);
        let data = serde_json::to_string_pretty(self)?;
        // Unique temp name so concurrent saves (or parallel tests sharing a dir)
        // never collide on the same temp file mid-rename.
        let tmp = path.with_file_name(format!("{}.tmp.{}", Self::STATE_FILE, generate_id()));
        fs::write(&tmp, &data)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
    }

    /// Load state from the default state directory.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(Self::state_dir().as_path())
    }

    /// Like [`Self::load`] but also returns a warning for a corrupt state file.
    pub fn load_checked() -> anyhow::Result<(Self, Option<String>)> {
        Self::load_checked_from(Self::state_dir().as_path())
    }

    /// Save state to the default state directory.
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(Self::state_dir().as_path())
    }

    /// Reload the on-disk state, merge ours over it relative to `baseline`, and
    /// write the result atomically — so a `kmd` command that changed `state.json`
    /// while the TUI was open isn't clobbered on the next save. Best-effort: if
    /// the on-disk file can't be reloaded (corrupt), write ours rather than fail.
    pub fn merge_save_to(&self, baseline: &AppState, base: &Path) -> anyhow::Result<()> {
        let merged = match Self::load_from(base) {
            Ok(disk) => self.merged_over(baseline, disk),
            Err(_) => self.clone(),
        };
        merged.save_to(base)
    }

    /// [`merge_save_to`](Self::merge_save_to) against the default state dir.
    pub fn merge_save(&self, baseline: &AppState) -> anyhow::Result<()> {
        self.merge_save_to(baseline, Self::state_dir().as_path())
    }

    /// 3-way merge: this in-memory state ("mine") over the current `disk` state,
    /// relative to the `baseline` we loaded. Start from `disk` (so entries a
    /// concurrent CLI added survive), drop entries we deleted since `baseline`,
    /// and let our copy win for entries we still hold ("mine wins on conflict").
    /// We do NOT adopt the result in memory — it only protects the on-disk file
    /// from being overwritten; CLI-added rows appear in the TUI on its next load.
    ///
    /// Scope: this protects concurrent CLI *adds* and *deletes*. A CLI *in-place
    /// edit* to a row the TUI also holds is still lost on the TUI's next save —
    /// there's no per-field/version comparison, and "mine wins" overwrites it.
    pub fn merged_over(&self, baseline: &AppState, disk: AppState) -> AppState {
        use std::collections::HashSet;

        // Keep disk entries we neither hold nor deleted-since-baseline, then
        // append ours (so ours wins for ids present in both).
        fn merge_vec<T: Clone>(
            mine: &[T],
            baseline: &[T],
            disk: Vec<T>,
            id: impl Fn(&T) -> &str,
        ) -> Vec<T> {
            let mine_ids: HashSet<&str> = mine.iter().map(&id).collect();
            let deleted: HashSet<&str> =
                baseline.iter().map(&id).filter(|i| !mine_ids.contains(i)).collect();
            let mut out: Vec<T> = disk
                .into_iter()
                .filter(|d| {
                    let i = id(d);
                    !mine_ids.contains(i) && !deleted.contains(i)
                })
                .collect();
            out.extend(mine.iter().cloned());
            out
        }

        fn merge_map<V: Clone>(
            mine: &HashMap<String, V>,
            baseline: &HashMap<String, V>,
            disk: HashMap<String, V>,
        ) -> HashMap<String, V> {
            let deleted: HashSet<&str> =
                baseline.keys().map(|k| k.as_str()).filter(|k| !mine.contains_key(*k)).collect();
            let mut out: HashMap<String, V> = disk
                .into_iter()
                .filter(|(k, _)| !mine.contains_key(k) && !deleted.contains(k.as_str()))
                .collect();
            for (k, v) in mine {
                out.insert(k.clone(), v.clone());
            }
            out
        }

        AppState {
            version: Self::STATE_VERSION,
            repos: merge_vec(&self.repos, &baseline.repos, disk.repos, |r| r.id.as_str()),
            workspaces: merge_vec(&self.workspaces, &baseline.workspaces, disk.workspaces, |w| {
                w.id.as_str()
            }),
            sessions: merge_vec(&self.sessions, &baseline.sessions, disk.sessions, |s| s.id.as_str()),
            embedded_sessions: merge_map(
                &self.embedded_sessions,
                &baseline.embedded_sessions,
                disk.embedded_sessions,
            ),
            embedded_titles: merge_map(
                &self.embedded_titles,
                &baseline.embedded_titles,
                disk.embedded_titles,
            ),
        }
    }

    /// A fresh Claude session id (UUID v4) to assign to a new embedded session.
    pub fn new_claude_session_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Mint a persisted embedded-session entry for a shell tab. The entry is also
    /// the tab's runtime id, so close/reap removal needs no translation. UUIDs
    /// contain no ':', so a sentinel can never collide with a Claude session id.
    pub fn new_shell_session_id() -> String {
        format!("{SHELL_SESSION_PREFIX}{}", uuid::Uuid::new_v4())
    }

    /// Whether a persisted embedded-session entry denotes a shell tab.
    pub fn is_shell_session_id(id: &str) -> bool {
        id.starts_with(SHELL_SESSION_PREFIX)
    }

    /// Mint a persisted embedded-session entry: `prefix` + a fresh UUID v4.
    /// `""` yields a bare claude id; kind prefixes (`shell:` etc.) live in the TUI.
    pub fn new_prefixed_session_id(prefix: &str) -> String {
        format!("{prefix}{}", uuid::Uuid::new_v4())
    }

    /// Whether `bare` (a persisted entry with its kind prefix stripped) is a
    /// kommand0-minted session uuid. Only these may reach a `--resume` argv:
    /// a hand-edited entry must not smuggle flags into the spawn.
    pub fn is_valid_session_uuid(bare: &str) -> bool {
        uuid::Uuid::parse_str(bare).is_ok()
    }

    /// The stored session entries (Claude session ids, or `shell:<uuid>`
    /// sentinels for shell tabs) for a workspace's session tabs, in tab
    /// order (empty slice when none).
    pub fn embedded_session_ids(&self, workspace_id: &str) -> &[String] {
        self.embedded_sessions
            .get(workspace_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Append a session entry for a workspace (idempotent, no duplicates).
    pub fn add_embedded_session(&mut self, workspace_id: &str, session_id: &str) {
        let ids = self.embedded_sessions.entry(workspace_id.to_string()).or_default();
        if !ids.iter().any(|id| id == session_id) {
            ids.push(session_id.to_string());
        }
    }

    /// Forget a single session entry for a workspace (its tab was closed, its
    /// shell exited, or its resume failed because the Claude session was
    /// purged). Removes the workspace entry when its last id is gone. Preserves
    /// the order of the rest.
    pub fn remove_embedded_session(&mut self, workspace_id: &str, session_id: &str) {
        if let Some(ids) = self.embedded_sessions.get_mut(workspace_id) {
            ids.retain(|id| id != session_id);
            if ids.is_empty() {
                self.embedded_sessions.remove(workspace_id);
            }
        }
        // Keep titles in lockstep so a renamed-then-closed session leaves nothing
        // behind (a title must never outlive its session id).
        self.set_embedded_session_title(workspace_id, session_id, "");
    }

    /// Replace `old_id` with `new_id` in place (same index) so tab order is
    /// preserved (used by resume auto-heal); appends when `old_id` is absent
    /// (including a missing workspace key), so the persisted Vec stays aligned
    /// with the runtime tabs either way. Moves `old_id`'s title to `new_id`
    /// (a title never outlives its id, and the tab's purpose survives the swap).
    pub fn replace_embedded_session(&mut self, workspace_id: &str, old_id: &str, new_id: &str) {
        if let Some(ids) = self.embedded_sessions.get_mut(workspace_id)
            && let Some(pos) = ids.iter().position(|id| id == old_id)
        {
            // `new_id` is a fresh UUID v4, so no duplicate check is needed.
            debug_assert!(
                !ids.iter().any(|id| id == new_id),
                "replace_embedded_session: new_id {new_id} already present"
            );
            ids[pos] = new_id.to_string();
        } else {
            self.add_embedded_session(workspace_id, new_id);
        }
        let title = self.embedded_session_title(workspace_id, old_id).map(str::to_string);
        self.set_embedded_session_title(workspace_id, old_id, "");
        if let Some(title) = &title {
            self.set_embedded_session_title(workspace_id, new_id, title);
        }
    }

    /// Forget all of a workspace's session ids (on workspace/repo delete).
    pub fn clear_all_embedded_sessions(&mut self, workspace_id: &str) {
        self.embedded_sessions.remove(workspace_id);
        self.embedded_titles.remove(workspace_id);
    }

    /// Drop embedded-session entries (ids and titles) for workspaces that no
    /// longer exist.
    pub fn prune_embedded_sessions(&mut self) {
        self.embedded_sessions
            .retain(|ws_id, _| self.workspaces.iter().any(|w| &w.id == ws_id));
        self.embedded_titles
            .retain(|ws_id, _| self.workspaces.iter().any(|w| &w.id == ws_id));
    }

    /// The display title for a session tab, if the user has renamed it.
    pub fn embedded_session_title(&self, workspace_id: &str, session_id: &str) -> Option<&str> {
        self.embedded_titles
            .get(workspace_id)
            .and_then(|m| m.get(session_id))
            .map(String::as_str)
    }

    /// Set (or, with an empty `title`, clear) a session tab's display title.
    /// Drops the workspace's title map once its last title is cleared so the
    /// field stays absent from serialized state in the common case.
    pub fn set_embedded_session_title(&mut self, workspace_id: &str, session_id: &str, title: &str) {
        if title.is_empty() {
            if let Some(m) = self.embedded_titles.get_mut(workspace_id) {
                m.remove(session_id);
                if m.is_empty() {
                    self.embedded_titles.remove(workspace_id);
                }
            }
        } else {
            self.embedded_titles
                .entry(workspace_id.to_string())
                .or_default()
                .insert(session_id.to_string(), title.to_string());
        }
    }

    /// Add a repo, saving state to a custom base directory.
    pub fn add_repo_with_base(&mut self, path: &str, base: &Path) -> anyhow::Result<RepoEntry> {
        let dir = Path::new(path);
        if !dir.is_dir() {
            bail!("path does not exist or is not a directory: {path}");
        }

        let canonical = fs::canonicalize(dir)
            .with_context(|| format!("failed to canonicalize {path}"))?;
        let canonical_str = canonical.to_string_lossy().to_string();

        if self.repos.iter().any(|r| r.path == canonical_str) {
            bail!("repo already tracked: {canonical_str}");
        }

        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| canonical_str.clone());

        let id = generate_id();

        let entry = RepoEntry {
            id,
            name,
            path: canonical_str,
        };

        self.repos.push(entry.clone());
        self.save_to(base)?;
        Ok(entry)
    }

    /// Add a repo, saving state to the default state directory.
    pub fn add_repo(&mut self, path: &str) -> anyhow::Result<RepoEntry> {
        self.add_repo_with_base(path, Self::state_dir().as_path())
    }

    /// Delete a repo by reference (name, path, or ID), cascading to workspaces and sessions.
    ///
    /// Removes all workspaces (and their worktrees) and sessions belonging to the repo.
    pub fn delete_repo_with_base(
        &mut self,
        repo_ref: &str,
        base: &Path,
    ) -> anyhow::Result<RepoEntry> {
        let repo = self.resolve_repo(repo_ref)?.clone();

        // Collect workspace IDs for this repo
        let ws_ids: Vec<String> = self
            .workspaces
            .iter()
            .filter(|w| w.repo_id == repo.id)
            .map(|w| w.id.clone())
            .collect();

        // Remove sessions for these workspaces and clean up log files
        self.sessions.retain(|s| {
            if ws_ids.contains(&s.workspace_id) {
                let log_path = Path::new(&s.log_file);
                if log_path.exists() {
                    let _ = fs::remove_file(log_path);
                }
                false
            } else {
                true
            }
        });

        // Remove workspaces and their worktrees
        self.workspaces.retain(|w| {
            if w.repo_id == repo.id {
                if let Some(wt_path) = &w.worktree_path {
                    let _ = worktree::remove_worktree(&repo.path, wt_path);
                }
                false
            } else {
                true
            }
        });

        // Forget any embedded Claude session ids (and titles) for the removed
        // workspaces.
        for id in &ws_ids {
            self.embedded_sessions.remove(id);
            self.embedded_titles.remove(id);
        }

        // Remove the repo itself
        self.repos.retain(|r| r.id != repo.id);

        self.save_to(base)?;
        Ok(repo)
    }

    /// Delete a repo, saving state to the default state directory.
    pub fn delete_repo(&mut self, repo_ref: &str) -> anyhow::Result<RepoEntry> {
        self.delete_repo_with_base(repo_ref, Self::state_dir().as_path())
    }

    // --- Workspace methods ---

    /// Resolve a repo reference by name, path, or ID.
    pub fn resolve_repo(&self, reference: &str) -> anyhow::Result<&RepoEntry> {
        // Path-first if input contains '/'
        if reference.contains('/') {
            // Try path match (canonicalize if possible)
            let canonical = fs::canonicalize(reference)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| reference.to_string());
            if let Some(repo) = self.repos.iter().find(|r| r.path == canonical || r.path == reference) {
                return Ok(repo);
            }
            // Fall through to name/id
        }

        // Name match
        if let Some(repo) = self.repos.iter().find(|r| r.name == reference) {
            return Ok(repo);
        }

        // Path match (for non-slash inputs that happen to match)
        if let Some(repo) = self.repos.iter().find(|r| r.path == reference) {
            return Ok(repo);
        }

        // ID match
        if let Some(repo) = self.repos.iter().find(|r| r.id == reference) {
            return Ok(repo);
        }

        bail!(
            "No repo found matching '{reference}'. Checked: name, path, id. Use `kmd repo list` to see tracked repos."
        )
    }

    /// Create a workspace with a custom base directory for state persistence.
    ///
    /// By default, creates a git worktree for the workspace. Set `use_worktree` to
    /// `false` to skip worktree creation and use the repo root as working directory.
    pub fn create_workspace_with_base(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_impl(name, repo_ref, base, true, None)
    }

    /// Create a workspace, optionally skipping worktree creation.
    pub fn create_workspace_with_options(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
        use_worktree: bool,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_impl(name, repo_ref, base, use_worktree, None)
    }

    /// Create a workspace whose worktree checks out an EXISTING branch (local or
    /// a remote `origin/…` ref) instead of forking a new one. The name defaults
    /// to a path-safe form of the branch when omitted.
    pub fn create_workspace_from_branch(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        branch: &str,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_from_branch_with_base(name, repo_ref, Self::state_dir().as_path(), branch)
    }

    /// [`create_workspace_from_branch`](Self::create_workspace_from_branch) with an
    /// explicit base dir (for tests).
    pub fn create_workspace_from_branch_with_base(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
        branch: &str,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_impl(name, repo_ref, base, true, Some(branch))
    }

    /// Validate a would-be workspace name: it becomes a path segment under
    /// `worktrees/<repo-id>/` and the branch name itself, so reject anything
    /// that could escape the dir or trip git's arg parsing (empty/whitespace,
    /// `.`/`..`, a path separator, a leading dash), the two names git refuses
    /// as bare branches (`HEAD`, `@`: `git worktree add -b` would fail and
    /// silently fall back to the repo root), and a name already in use within
    /// that repo (names are per-repo; other repos may reuse them).
    pub fn validate_new_workspace_name(&self, name: &str, repo_id: &str) -> anyhow::Result<()> {
        if name.trim().is_empty()
            || name == "."
            || name == ".."
            || name == "HEAD"
            || name == "@"
            || name.contains('/')
            || name.contains('\\')
            || name.starts_with('-')
        {
            bail!(
                "invalid workspace name {name:?}: must not be empty, '.'/'..', \
                 'HEAD', '@', contain a path separator, or start with '-'"
            );
        }
        if self.workspaces.iter().any(|w| w.repo_id == repo_id && w.name == name) {
            bail!("workspace already exists: {name}");
        }
        Ok(())
    }

    fn create_workspace_impl(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
        base: &Path,
        use_worktree: bool,
        from_branch: Option<&str>,
    ) -> anyhow::Result<Workspace> {
        let repo = self.resolve_repo(repo_ref)?.clone();

        let ws_name = match name {
            Some(n) => n.to_string(),
            // Default the name from the branch when checking one out (path-safe:
            // drop a leading `origin/`, then `/` -> `-`), else from the repo.
            None => match from_branch {
                Some(b) => b.strip_prefix("origin/").unwrap_or(b).replace('/', "-"),
                None => repo.name.clone(),
            },
        };

        self.validate_new_workspace_name(&ws_name, &repo.id)?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();

        // Create a git worktree: on an existing branch if one was requested, else
        // on a fresh branch named after the workspace.
        let (working_dir, worktree_path, branch_name) = if !use_worktree {
            (repo.path.clone(), None, None)
        } else if let Some(branch_ref) = from_branch {
            match worktree::create_worktree_from_branch(&repo.path, &repo.id, &ws_name, base, branch_ref) {
                worktree::WorktreeResult::Created { worktree_path, branch_name } => {
                    (worktree_path.clone(), Some(worktree_path), Some(branch_name))
                }
                // An explicit branch request that can't be satisfied is an error,
                // not a silent fall back to the repo root.
                worktree::WorktreeResult::Fallback { reason } => {
                    bail!("couldn't check out branch {branch_ref:?}: {reason}");
                }
            }
        } else {
            match worktree::create_worktree(&repo.path, &repo.id, &ws_name, base) {
                worktree::WorktreeResult::Created { worktree_path, branch_name } => {
                    (worktree_path.clone(), Some(worktree_path), Some(branch_name))
                }
                worktree::WorktreeResult::Fallback { reason: _ } => (repo.path.clone(), None, None),
            }
        };

        let ws = Workspace {
            id: generate_id(),
            name: ws_name,
            repo_id: repo.id.clone(),
            working_dir,
            active: true,
            created_at: now,
            worktree_path,
            branch_name,
        };

        self.workspaces.push(ws.clone());
        self.save_to(base)?;
        Ok(ws)
    }

    /// Create a workspace, saving state to the default state directory.
    pub fn create_workspace(
        &mut self,
        name: Option<&str>,
        repo_ref: &str,
    ) -> anyhow::Result<Workspace> {
        self.create_workspace_with_base(name, repo_ref, Self::state_dir().as_path())
    }

    /// List workspaces, optionally showing all (including archived) and filtering by repo.
    pub fn list_workspaces(
        &self,
        all: bool,
        repo_ref: Option<&str>,
    ) -> anyhow::Result<Vec<&Workspace>> {
        let repo_id = match repo_ref {
            Some(r) => Some(self.resolve_repo(r)?.id.clone()),
            None => None,
        };

        let result: Vec<&Workspace> = self
            .workspaces
            .iter()
            .filter(|w| all || w.active)
            .filter(|w| match &repo_id {
                Some(rid) => w.repo_id == *rid,
                None => true,
            })
            .collect();

        Ok(result)
    }

    /// Resolve a workspace reference to its index: one pass collecting every
    /// workspace whose ID or name equals the ref; exactly one distinct match
    /// wins, several are an ambiguity error.
    ///
    /// Letting IDs match here deliberately diverges from
    /// [`Self::resolve_repo`] (name-first, where a name can shadow an id):
    /// the ambiguity error's remedy is "use the ID", so an ID must be
    /// un-shadowable. A name carried by workspaces in several repos is
    /// ambiguous, as is a ref matching one workspace's ID and a DIFFERENT
    /// workspace's name (fail-safe over a silent wrong-target delete when a
    /// workspace is named with another workspace's id-shaped string).
    /// Trade-off: in that shadow state the TUI's by-id archive/activate
    /// calls hit the error and silently no-op until the shadowing row is
    /// deleted by its own id; destructive deletes are immune, they use the
    /// exact-id [`Self::delete_workspace_by_id`] path instead.
    fn workspace_index(&self, ws_ref: &str) -> anyhow::Result<usize> {
        let matches: Vec<usize> = self
            .workspaces
            .iter()
            .enumerate()
            .filter_map(|(i, w)| (w.id == ws_ref || w.name == ws_ref).then_some(i))
            .collect();
        match matches.as_slice() {
            [] => bail!("workspace not found: {ws_ref}"),
            [i] => Ok(*i),
            _ => {
                // Refuse rather than act on the wrong repo's workspace; list
                // every match (in state order) so the error self-documents.
                let list = matches
                    .iter()
                    .map(|&i| {
                        let w = &self.workspaces[i];
                        let repo = self
                            .repos
                            .iter()
                            .find(|r| r.id == w.repo_id)
                            .map(|r| r.name.as_str())
                            .unwrap_or("(unknown)");
                        format!("{} (repo {repo})", w.id)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "ambiguous workspace reference '{ws_ref}': matches {list}; \
                     use the ID (shown by `kmd workspace list`)"
                )
            }
        }
    }

    /// Show a workspace by name or ID (an ambiguous name is an error).
    pub fn show_workspace(&self, ws_ref: &str) -> anyhow::Result<&Workspace> {
        Ok(&self.workspaces[self.workspace_index(ws_ref)?])
    }

    /// Shared removal body once a delete entry point has resolved the row
    /// index: drop the row, its embedded-session bookkeeping, and its
    /// worktree, then save.
    fn remove_workspace_at(&mut self, idx: usize, base: &Path) -> anyhow::Result<Workspace> {
        let removed = self.workspaces.remove(idx);
        self.embedded_sessions.remove(&removed.id);
        self.embedded_titles.remove(&removed.id);

        // Clean up worktree if present
        if let Some(wt_path) = &removed.worktree_path
            && let Some(repo) = self.repos.iter().find(|r| r.id == removed.repo_id)
        {
            let _ = worktree::remove_worktree(&repo.path, wt_path);
        }

        self.save_to(base)?;
        Ok(removed)
    }

    /// Delete a workspace by name or ID, saving to custom base directory.
    /// Also removes the git worktree if one was created.
    pub fn delete_workspace_with_base(
        &mut self,
        ws_ref: &str,
        base: &Path,
    ) -> anyhow::Result<Workspace> {
        let idx = self.workspace_index(ws_ref)?;
        self.remove_workspace_at(idx, base)
    }

    /// Delete a workspace by name or ID, saving to the default state directory.
    pub fn delete_workspace(&mut self, ws_ref: &str) -> anyhow::Result<Workspace> {
        self.delete_workspace_with_base(ws_ref, Self::state_dir().as_path())
    }

    /// Delete a workspace by its EXACT id, never falling back to a name
    /// match: "workspace not found" when the id is absent. For callers
    /// holding an authentic state-held id (the TUI's tree actions, the CLI
    /// cleanup's second step): immune to name shadowing AND to the race
    /// where the row is deleted meanwhile and a new workspace is created
    /// NAMED that id string, which a ws_ref lookup would mis-target.
    /// User-facing refs stay on [`Self::delete_workspace`].
    pub fn delete_workspace_by_id_with_base(
        &mut self,
        id: &str,
        base: &Path,
    ) -> anyhow::Result<Workspace> {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {id}"))?;
        self.remove_workspace_at(idx, base)
    }

    /// [`Self::delete_workspace_by_id_with_base`] against the default state
    /// directory.
    pub fn delete_workspace_by_id(&mut self, id: &str) -> anyhow::Result<Workspace> {
        self.delete_workspace_by_id_with_base(id, Self::state_dir().as_path())
    }

    /// Archive a workspace (set active=false) with custom base directory.
    pub fn archive_workspace_with_base(&mut self, ws_ref: &str, base: &Path) -> anyhow::Result<()> {
        let idx = self.workspace_index(ws_ref)?;
        self.workspaces[idx].active = false;
        self.save_to(base)?;
        Ok(())
    }

    /// Archive a workspace by name or ID (set active=false).
    pub fn archive_workspace(&mut self, ws_ref: &str) -> anyhow::Result<()> {
        self.archive_workspace_with_base(ws_ref, Self::state_dir().as_path())
    }

    /// Activate a workspace (set active=true) with custom base directory.
    pub fn activate_workspace_with_base(&mut self, ws_ref: &str, base: &Path) -> anyhow::Result<()> {
        let idx = self.workspace_index(ws_ref)?;
        self.workspaces[idx].active = true;
        self.save_to(base)?;
        Ok(())
    }

    /// Activate a workspace by name or ID (set active=true).
    pub fn activate_workspace(&mut self, ws_ref: &str) -> anyhow::Result<()> {
        self.activate_workspace_with_base(ws_ref, Self::state_dir().as_path())
    }

    // --- Session methods ---

    /// Create a session for a workspace, saving state to a custom base directory.
    pub fn create_session_with_base(
        &mut self,
        workspace_id: &str,
        base: &Path,
    ) -> anyhow::Result<Session> {
        // Validate workspace exists
        if !self.workspaces.iter().any(|w| w.id == workspace_id) {
            bail!("workspace not found: {workspace_id}");
        }

        // Check no running session for this workspace
        if self
            .sessions
            .iter()
            .any(|s| s.workspace_id == workspace_id && s.status == SessionStatus::Running)
        {
            bail!(
                "workspace {workspace_id} already has a running session"
            );
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();

        let session = Session {
            log_file: base
                .join("sessions")
                .join(format!("{id}.log"))
                .to_string_lossy()
                .into_owned(),
            id,
            workspace_id: workspace_id.to_string(),
            claude_session_id: None,
            pid: None,
            status: SessionStatus::Running,
            created_at: now,
            ended_at: None,
        };

        self.sessions.push(session.clone());
        self.save_to(base)?;
        Ok(session)
    }

    /// Create a session, saving state to the default state directory.
    pub fn create_session(&mut self, workspace_id: &str) -> anyhow::Result<Session> {
        self.create_session_with_base(workspace_id, Self::state_dir().as_path())
    }

    /// Find the most recent session for a workspace.
    pub fn find_session_by_workspace(&self, workspace_id: &str) -> Option<&Session> {
        self.sessions
            .iter()
            .rev()
            .find(|s| s.workspace_id == workspace_id)
    }

    /// Find a session by ID (mutable).
    pub fn find_session_mut(&mut self, session_id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == session_id)
    }

    /// Update session status, saving state to a custom base directory.
    /// Sets ended_at for terminal states (Stopped, Failed, Exited).
    pub fn update_session_status_with_base(
        &mut self,
        session_id: &str,
        status: SessionStatus,
        base: &Path,
    ) -> anyhow::Result<()> {
        let session = self
            .sessions
            .iter_mut()
            .find(|s| s.id == session_id)
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;

        session.status = status.clone();

        match status {
            SessionStatus::Stopped | SessionStatus::Failed | SessionStatus::Exited => {
                session.ended_at = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("time went backwards")
                        .as_secs(),
                );
            }
            SessionStatus::Running => {}
        }

        self.save_to(base)?;
        Ok(())
    }

    /// Update session status, saving state to the default state directory.
    pub fn update_session_status(
        &mut self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        self.update_session_status_with_base(session_id, status, Self::state_dir().as_path())
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> &[Session] {
        &self.sessions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws(id: &str, name: &str) -> Workspace {
        Workspace {
            id: id.into(),
            name: name.into(),
            repo_id: "r".into(),
            working_dir: "/tmp".into(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: None,
        }
    }

    /// Init a real git repo at `tmp/<name>` (identity + initial commit, branch
    /// main) and return its path.
    fn init_git_repo(tmp: &TempDir, name: &str) -> std::path::PathBuf {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&dir).output().unwrap()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "--allow-empty", "-m", "init"]);
        dir
    }

    #[test]
    fn merge_preserves_cli_adds_and_respects_tui_deletes() {
        // baseline = what the TUI loaded; mine = TUI's in-memory state (deleted B,
        // added C); disk = the file now (a CLI added D; B not yet deleted on disk).
        let baseline = AppState { workspaces: vec![ws("A", "a"), ws("B", "b")], ..Default::default() };
        let mine = AppState { workspaces: vec![ws("A", "a"), ws("C", "c")], ..Default::default() };
        let disk =
            AppState { workspaces: vec![ws("A", "a"), ws("B", "b"), ws("D", "d")], ..Default::default() };

        let merged = mine.merged_over(&baseline, disk);
        let ids: std::collections::HashSet<&str> =
            merged.workspaces.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains("A"), "untouched entry kept");
        assert!(!ids.contains("B"), "a TUI delete wins over the disk copy");
        assert!(ids.contains("C"), "a TUI add is present");
        assert!(ids.contains("D"), "a concurrent CLI add survives");
        assert_eq!(merged.workspaces.len(), 3);
    }

    #[test]
    fn merge_lets_mine_win_on_a_conflicting_edit() {
        let baseline = AppState { workspaces: vec![ws("A", "v1")], ..Default::default() };
        let mine = AppState { workspaces: vec![ws("A", "mine")], ..Default::default() };
        let disk = AppState { workspaces: vec![ws("A", "disk")], ..Default::default() };
        let merged = mine.merged_over(&baseline, disk);
        assert_eq!(merged.workspaces.len(), 1);
        assert_eq!(merged.workspaces[0].name, "mine", "in-memory copy wins for a shared id");
    }

    #[test]
    fn merge_applies_the_same_rules_to_maps() {
        let mk = |pairs: &[(&str, &str)]| -> HashMap<String, Vec<String>> {
            pairs.iter().map(|(k, v)| (k.to_string(), vec![v.to_string()])).collect()
        };
        let baseline = AppState { embedded_sessions: mk(&[("A", "a"), ("B", "b")]), ..Default::default() };
        // TUI deleted B and re-tabbed A with a shell tab (the merge treats the
        // Vec opaquely, so a sentinel entry rides through untouched); a CLI added C.
        let mine = AppState { embedded_sessions: mk(&[("A", "shell:a2")]), ..Default::default() };
        let disk =
            AppState { embedded_sessions: mk(&[("A", "a"), ("B", "b"), ("C", "c")]), ..Default::default() };
        let merged = mine.merged_over(&baseline, disk);
        assert_eq!(merged.embedded_sessions.get("A").unwrap(), &vec!["shell:a2".to_string()], "mine wins");
        assert!(!merged.embedded_sessions.contains_key("B"), "TUI delete respected");
        assert!(merged.embedded_sessions.contains_key("C"), "CLI add preserved");
    }

    #[test]
    fn merge_save_reloads_and_preserves_a_concurrent_add() {
        let tmp = TempDir::new().unwrap();
        // Disk already has a workspace the TUI never saw (a CLI added it).
        let disk = AppState { workspaces: vec![ws("cli", "from-cli")], ..Default::default() };
        disk.save_to(tmp.path()).unwrap();
        // The TUI loaded an empty baseline, created its own workspace, and saves.
        let baseline = AppState::default();
        let mine = AppState { workspaces: vec![ws("tui", "from-tui")], ..Default::default() };
        mine.merge_save_to(&baseline, tmp.path()).unwrap();
        let on_disk = AppState::load_from(tmp.path()).unwrap();
        let ids: std::collections::HashSet<&str> =
            on_disk.workspaces.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains("cli") && ids.contains("tui"), "both survive the save: {ids:?}");
    }

    #[test]
    fn create_workspace_from_branch_defaults_name_and_adopts_the_branch() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&repo_dir).output().unwrap()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "--allow-empty", "-m", "init"]);
        git(&["branch", "feat/login"]);

        let base = tmp.path().join("base");
        let mut state = AppState {
            repos: vec![RepoEntry {
                id: "r".into(),
                name: "repo".into(),
                path: repo_dir.to_string_lossy().into_owned(),
            }],
            ..Default::default()
        };

        let ws = state.create_workspace_from_branch_with_base(None, "r", &base, "feat/login").unwrap();
        assert_eq!(ws.name, "feat-login", "name defaults to a path-safe form of the branch");
        assert_eq!(
            ws.branch_name.as_deref(),
            Some("feat/login"),
            "adopts the existing branch as-is (no fork)"
        );
        assert!(ws.worktree_path.is_some());

        // A branch that doesn't exist is an error, not a silent repo-root fallback.
        let err = state.create_workspace_from_branch_with_base(None, "r", &base, "ghost").unwrap_err();
        assert!(err.to_string().contains("couldn't check out branch"), "got: {err}");
    }

    #[test]
    fn same_workspace_name_in_two_repos_nests_worktrees_per_repo() {
        // The motivating bug: `development` in one repo used to block creating
        // `development` in another. Both must succeed, with worktrees at
        // distinct `worktrees/<repo-id>/development` paths.
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("state");
        let repo_a = init_git_repo(&tmp, "alpha");
        let repo_b = init_git_repo(&tmp, "beta");
        let mut state = AppState::default();
        let a = state.add_repo_with_base(repo_a.to_str().unwrap(), &base).unwrap();
        let b = state.add_repo_with_base(repo_b.to_str().unwrap(), &base).unwrap();

        let wa = state.create_workspace_with_base(Some("development"), &a.id, &base).unwrap();
        let wb = state.create_workspace_with_base(Some("development"), &b.id, &base).unwrap();

        let pa = wa.worktree_path.as_deref().unwrap();
        let pb = wb.worktree_path.as_deref().unwrap();
        assert_ne!(pa, pb, "each repo gets its own worktree dir");
        assert!(
            pa.ends_with(&format!("worktrees/{}/development", a.id)),
            "nested under the repo id: {pa}"
        );
        assert!(
            pb.ends_with(&format!("worktrees/{}/development", b.id)),
            "nested under the repo id: {pb}"
        );
        assert!(Path::new(pa).exists() && Path::new(pb).exists(), "both worktrees on disk");
    }

    #[test]
    fn legacy_flat_worktree_coexists_with_a_nested_same_name() {
        // A pre-nesting workspace with a real FLAT worktree (worktrees/<name>)
        // must keep working next to a new nested one of the same name: the new
        // one nests, the flat dir is untouched, and deleting the legacy row
        // removes the flat dir while the nested sibling survives.
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("state");
        let repo_a = init_git_repo(&tmp, "alpha");
        let repo_b = init_git_repo(&tmp, "beta");
        let mut state = AppState::default();
        let a = state.add_repo_with_base(repo_a.to_str().unwrap(), &base).unwrap();
        let b = state.add_repo_with_base(repo_b.to_str().unwrap(), &base).unwrap();

        // Hand-craft the legacy flat worktree, as pre-nesting versions laid it out.
        let flat = base.join("worktrees").join("development");
        let out = std::process::Command::new("git")
            .args(["-C", &a.path, "worktree", "add", flat.to_str().unwrap(), "-b", "development"])
            .output()
            .unwrap();
        assert!(out.status.success(), "flat worktree add: {}", String::from_utf8_lossy(&out.stderr));
        state.workspaces.push(Workspace {
            id: "legacy".into(),
            name: "development".into(),
            repo_id: a.id.clone(),
            working_dir: flat.to_string_lossy().into_owned(),
            active: true,
            created_at: 0,
            worktree_path: Some(flat.to_string_lossy().into_owned()),
            branch_name: Some("development".into()),
        });

        let wb = state.create_workspace_with_base(Some("development"), &b.id, &base).unwrap();
        let nested = wb.worktree_path.as_deref().unwrap();
        assert!(
            nested.ends_with(&format!("worktrees/{}/development", b.id)),
            "new workspace nests: {nested}"
        );
        assert!(flat.exists(), "legacy flat dir untouched by the nested create");

        // Deleting the legacy workspace (by id, the name is now ambiguous)
        // removes the flat dir; the other repo's nested worktree survives.
        state.delete_workspace_with_base("legacy", &base).unwrap();
        assert!(!flat.exists(), "legacy flat worktree removed");
        assert!(Path::new(nested).exists(), "nested sibling survives the flat delete");
    }

    #[test]
    fn ambiguous_workspace_name_errors_and_id_hits_the_right_row() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("state");
        let repo_a = init_git_repo(&tmp, "alpha");
        let repo_b = init_git_repo(&tmp, "beta");
        let mut state = AppState::default();
        let a = state.add_repo_with_base(repo_a.to_str().unwrap(), &base).unwrap();
        let b = state.add_repo_with_base(repo_b.to_str().unwrap(), &base).unwrap();
        let wa = state.create_workspace_with_base(Some("dev"), &a.id, &base).unwrap();
        let wb = state.create_workspace_with_base(Some("dev"), &b.id, &base).unwrap();
        let pa = wa.worktree_path.clone().unwrap();
        let pb = wb.worktree_path.clone().unwrap();

        // The bare name is ambiguous; the error names each match and the remedy.
        let err = state.show_workspace("dev").unwrap_err().to_string();
        assert!(err.contains("use the ID"), "remedy named: {err}");
        assert!(err.contains(&wa.id) && err.contains(&wb.id), "lists each match: {err}");
        assert!(err.contains("alpha") && err.contains("beta"), "names the repos: {err}");

        // Delete by ambiguous name refuses AND leaves both rows + dirs intact
        // (pins the destructive surface directly, not just via show).
        let err = state.delete_workspace_with_base("dev", &base).unwrap_err().to_string();
        assert!(err.contains("use the ID"), "{err}");
        assert_eq!(state.workspaces.len(), 2, "no row deleted on the ambiguity error");
        assert!(Path::new(&pa).exists() && Path::new(&pb).exists(), "both worktrees intact");
        assert!(state.archive_workspace_with_base("dev", &base).is_err(), "archive refuses too");
        assert!(
            state.workspaces.iter().all(|w| w.active),
            "the ambiguous archive mutated nothing (all rows still active)"
        );
        assert!(state.activate_workspace_with_base("dev", &base).is_err(), "activate refuses too");

        // By ID everything targets the right row.
        assert_eq!(state.show_workspace(&wa.id).unwrap().repo_id, a.id);
        let removed = state.delete_workspace_with_base(&wa.id, &base).unwrap();
        assert_eq!(removed.id, wa.id);
        assert!(!Path::new(&pa).exists(), "the addressed worktree is removed");
        assert!(Path::new(&pb).exists(), "the other repo's worktree survives");
    }

    #[test]
    fn delete_workspace_by_id_ignores_name_shadowing() {
        // Shadow state: workspace A is NAMED exactly workspace B's id. The
        // exact-id delete must hit the id-matched row, never the name.
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.workspaces.push(ws("b-id", "b-name"));
        state.workspaces.push(ws("a-id", "b-id")); // A named exactly B's id
        let removed = state.delete_workspace_by_id_with_base("b-id", tmp.path()).unwrap();
        assert_eq!(removed.id, "b-id", "the id match wins over the shadowing name");
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].id, "a-id", "the shadowing row is untouched");

        // The id row is gone and a workspace NAMED that id string remains
        // (the deleted-row-plus-name-reuse race): the by-id delete must NOT
        // fall back to the name match. Error, nothing mutated.
        let err = state.delete_workspace_by_id_with_base("b-id", tmp.path()).unwrap_err();
        assert!(err.to_string().contains("workspace not found"), "{err}");
        assert_eq!(state.workspaces.len(), 1, "no name-match fallback delete");
    }

    #[test]
    fn workspace_named_as_another_workspaces_id_is_ambiguous() {
        // Workspace A's NAME equals workspace B's ID: resolving that string
        // matches B by id and A by name: refuse rather than silently act on
        // either (fail-safe over fail-wrong).
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.workspaces.push(ws("b-id", "b-name"));
        state.workspaces.push(ws("a-id", "b-id")); // A named exactly B's id
        let err = state.show_workspace("b-id").unwrap_err().to_string();
        assert!(err.contains("use the ID"), "{err}");
        let err = state.delete_workspace_with_base("b-id", tmp.path()).unwrap_err().to_string();
        assert!(err.contains("use the ID"), "{err}");
        assert_eq!(state.workspaces.len(), 2, "nothing mutated on the double match");
    }

    #[test]
    fn load_from_returns_default_when_no_file() {
        let tmp = TempDir::new().unwrap();
        let state = AppState::load_from(tmp.path()).unwrap();
        assert!(state.repos.is_empty());
    }

    #[test]
    fn save_to_load_from_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.repos.push(RepoEntry {
            id: "abc123".to_string(),
            name: "my-repo".to_string(),
            path: "/tmp/my-repo".to_string(),
        });
        state.save_to(tmp.path()).unwrap();

        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.repos.len(), 1);
        assert_eq!(loaded.repos[0].id, "abc123");
        assert_eq!(loaded.repos[0].name, "my-repo");
        assert_eq!(loaded.repos[0].path, "/tmp/my-repo");
    }

    #[test]
    fn embedded_sessions_add_get_clear_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        assert!(state.embedded_session_ids("w1").is_empty());

        let id = AppState::new_claude_session_id();
        state.add_embedded_session("w1", &id);
        assert_eq!(state.embedded_session_ids("w1"), std::slice::from_ref(&id));
        state.save_to(tmp.path()).unwrap();

        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.embedded_session_ids("w1"), std::slice::from_ref(&id));

        state.clear_all_embedded_sessions("w1");
        assert!(state.embedded_session_ids("w1").is_empty());
    }

    #[test]
    fn new_claude_session_id_is_a_unique_uuid() {
        let a = AppState::new_claude_session_id();
        let b = AppState::new_claude_session_id();
        assert_ne!(a, b);
        assert!(uuid::Uuid::parse_str(&a).is_ok(), "should be a valid UUID: {a}");
    }

    #[test]
    fn shell_session_id_mint_and_classify() {
        let a = AppState::new_shell_session_id();
        let b = AppState::new_shell_session_id();
        assert!(a.starts_with("shell:"), "sentinel carries the prefix: {a}");
        assert_ne!(a, b, "each mint is unique");
        assert!(AppState::is_shell_session_id(&a));
        assert!(
            !AppState::is_shell_session_id(&AppState::new_claude_session_id()),
            "a Claude session id is never a shell sentinel"
        );
        assert!(!AppState::is_shell_session_id("plain-id"));
    }

    #[test]
    fn new_prefixed_session_id_mints_prefix_plus_uuid() {
        let a = AppState::new_prefixed_session_id("shell:");
        let b = AppState::new_prefixed_session_id("shell:");
        assert!(a.starts_with("shell:"), "sentinel carries the prefix: {a}");
        assert_ne!(a, b, "each mint is unique");
        assert!(
            uuid::Uuid::parse_str(a.strip_prefix("shell:").unwrap()).is_ok(),
            "the tail is a valid UUID: {a}"
        );
        // An empty prefix mints a bare (claude) id.
        let bare = AppState::new_prefixed_session_id("");
        assert!(uuid::Uuid::parse_str(&bare).is_ok(), "should be a valid UUID: {bare}");
    }

    #[test]
    fn is_valid_session_uuid_accepts_only_uuids() {
        assert!(AppState::is_valid_session_uuid(&AppState::new_prefixed_session_id("")));
        assert!(!AppState::is_valid_session_uuid("--yolo"));
        assert!(!AppState::is_valid_session_uuid("plain-id"));
        assert!(!AppState::is_valid_session_uuid(""));
    }

    #[test]
    fn embedded_sessions_multiple_per_workspace_in_order() {
        let mut state = AppState::default();
        state.add_embedded_session("w1", "a");
        state.add_embedded_session("w1", "b");
        state.add_embedded_session("w1", "a"); // idempotent
        assert_eq!(state.embedded_session_ids("w1"), &["a".to_string(), "b".to_string()]);
        assert_eq!(state.embedded_session_ids("missing"), &[] as &[String]);

        // Removing a middle id preserves the order of the rest.
        state.add_embedded_session("w1", "c");
        state.remove_embedded_session("w1", "b");
        assert_eq!(state.embedded_session_ids("w1"), &["a".to_string(), "c".to_string()]);

        // Removing the last id drops the workspace entry entirely.
        state.remove_embedded_session("w1", "a");
        state.remove_embedded_session("w1", "c");
        assert!(!state.embedded_sessions.contains_key("w1"));
    }

    #[test]
    fn replace_embedded_session_keeps_position_and_moves_title() {
        let mut state = AppState::default();
        for id in ["a", "b", "c"] {
            state.add_embedded_session("w1", id);
        }
        state.set_embedded_session_title("w1", "b", "build");

        state.replace_embedded_session("w1", "b", "x");
        assert_eq!(
            state.embedded_session_ids("w1"),
            &["a".to_string(), "x".to_string(), "c".to_string()],
            "the replacement takes b's slot, not the tail"
        );
        assert_eq!(state.embedded_session_title("w1", "b"), None, "the old id's title is gone");
        assert_eq!(state.embedded_session_title("w1", "x"), Some("build"), "the title moved");

        // A missing old id appends, keeping the Vec aligned with runtime tabs.
        state.replace_embedded_session("w1", "ghost", "y");
        assert_eq!(
            state.embedded_session_ids("w1"),
            &["a".to_string(), "x".to_string(), "c".to_string(), "y".to_string()]
        );

        // A wholly absent workspace key is created via the append path.
        state.replace_embedded_session("w2", "ghost", "z");
        assert_eq!(state.embedded_session_ids("w2"), &["z".to_string()]);
    }

    #[test]
    fn embedded_sessions_serialize_as_array_and_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.add_embedded_session("w1", "a");
        // A shell sentinel is a plain string on disk: it must round-trip
        // verbatim, in order, with no special serialization.
        state.add_embedded_session("w1", "shell:5e0f");
        state.add_embedded_session("w1", "b");
        state.save_to(tmp.path()).unwrap();

        // On disk the field is the new array shape.
        let raw = std::fs::read_to_string(tmp.path().join("state.json")).unwrap();
        assert!(raw.contains("\"w1\""));
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v["embedded_sessions"]["w1"].is_array());

        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(
            loaded.embedded_session_ids("w1"),
            &["a".to_string(), "shell:5e0f".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn embedded_sessions_reads_legacy_single_string_form() {
        // A state.json written before tabs stored one id per workspace as a
        // string; it must load as a one-element Vec.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[],"embedded_sessions":{"w1":"legacy-id"}}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.embedded_session_ids("w1"), &["legacy-id".to_string()]);
    }

    #[test]
    fn state_without_version_loads_as_current_and_restamps() {
        // A state.json written before schema versioning has no `version` field —
        // it must still load (treated as v1) and re-save stamped with the current
        // schema version, so an upgrade never bricks an existing file.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[]}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(
            loaded.version,
            AppState::STATE_VERSION,
            "a versionless file loads at the current schema"
        );

        loaded.save_to(tmp.path()).unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("state.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["version"], AppState::STATE_VERSION, "save stamps the schema version");
    }

    #[test]
    fn fresh_state_carries_the_current_version() {
        // A brand-new state is at the current schema (not 0 from a derived Default).
        assert_eq!(AppState::default().version, AppState::STATE_VERSION);
    }

    #[test]
    fn embedded_sessions_tolerates_explicit_null() {
        // The field has been observed written as `null`; it must load as empty.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[],"embedded_sessions":null}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert!(loaded.embedded_sessions.is_empty());
    }

    #[test]
    fn embedded_sessions_tolerates_per_entry_null_and_empty() {
        // A null or empty value for a single workspace must not abort the whole
        // load (it just contributes no sessions).
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[],"embedded_sessions":{"w1":null,"w2":[],"w3":["keep"]}}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.embedded_session_ids("w1"), &[] as &[String]);
        assert_eq!(loaded.embedded_session_ids("w2"), &[] as &[String]);
        assert_eq!(loaded.embedded_session_ids("w3"), &["keep".to_string()]);
    }

    #[test]
    fn prune_embedded_sessions_drops_orphans() {
        let mut state = AppState::default();
        state.workspaces.push(Workspace {
            id: "w1".to_string(),
            name: "ws".to_string(),
            repo_id: "r1".to_string(),
            working_dir: "/tmp/ws".to_string(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: None,
        });
        state.add_embedded_session("w1", "keep");
        state.add_embedded_session("w-gone", "drop");
        // A shell sentinel (with a user title) on the gone workspace is pruned
        // like any other entry, titles in lockstep.
        state.add_embedded_session("w-gone", "shell:drop2");
        state.set_embedded_session_title("w-gone", "shell:drop2", "build");
        state.prune_embedded_sessions();
        assert_eq!(state.embedded_session_ids("w1"), &["keep".to_string()]);
        assert!(state.embedded_session_ids("w-gone").is_empty());
        assert!(state.embedded_titles.is_empty(), "titles pruned with the workspace");
    }

    #[test]
    fn load_from_tolerates_state_without_embedded_sessions() {
        // Backward compat: a state.json written before this field must still load.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[]}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert!(loaded.embedded_sessions.is_empty());
    }

    #[test]
    fn embedded_session_title_set_get_clear_and_persist() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.add_embedded_session("w1", "a");
        assert_eq!(state.embedded_session_title("w1", "a"), None);

        state.set_embedded_session_title("w1", "a", "auth refactor");
        assert_eq!(state.embedded_session_title("w1", "a"), Some("auth refactor"));
        state.save_to(tmp.path()).unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.embedded_session_title("w1", "a"), Some("auth refactor"));

        // Clearing drops the entry and the (now-empty) field disappears on disk.
        state.set_embedded_session_title("w1", "a", "");
        assert_eq!(state.embedded_session_title("w1", "a"), None);
        assert!(state.embedded_titles.is_empty());
        state.save_to(tmp.path()).unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("state.json")).unwrap();
        assert!(!raw.contains("embedded_titles"), "empty title map is omitted: {raw}");
    }

    #[test]
    fn embedded_titles_never_outlive_their_session() {
        // The titles map must stay a subset of the session ids through every
        // removal path (close, workspace delete, prune).
        let mut state = AppState::default();
        state.workspaces.push(Workspace {
            id: "w1".to_string(),
            name: "ws".to_string(),
            repo_id: "r1".to_string(),
            working_dir: "/tmp/ws".to_string(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: None,
        });
        state.add_embedded_session("w1", "a");
        state.add_embedded_session("w1", "b");
        state.set_embedded_session_title("w1", "a", "alpha");
        state.set_embedded_session_title("w1", "b", "beta");

        // Closing one tab forgets only its title.
        state.remove_embedded_session("w1", "a");
        assert_eq!(state.embedded_session_title("w1", "a"), None);
        assert_eq!(state.embedded_session_title("w1", "b"), Some("beta"));

        // Pruning an orphan workspace drops its titles too.
        state.add_embedded_session("w-gone", "z");
        state.set_embedded_session_title("w-gone", "z", "zed");
        state.prune_embedded_sessions();
        assert!(!state.embedded_titles.contains_key("w-gone"));

        // Clearing a whole workspace drops its titles.
        state.clear_all_embedded_sessions("w1");
        assert!(!state.embedded_titles.contains_key("w1"));
        assert!(state.embedded_titles.is_empty());
    }

    #[test]
    fn load_tolerates_state_without_embedded_titles() {
        // Back-compat: a state.json written before titles existed must load.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("state.json"),
            r#"{"repos":[],"workspaces":[],"sessions":[],"embedded_sessions":{"w1":["a"]}}"#,
        )
        .unwrap();
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert!(loaded.embedded_titles.is_empty());
        assert_eq!(loaded.embedded_session_title("w1", "a"), None);
    }

    #[test]
    fn corrupt_state_degrades_to_default_and_backs_up() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        std::fs::write(&path, "{ not valid json").unwrap();

        let (state, warn) = AppState::load_checked_from(tmp.path()).unwrap();
        // load_checked_from degrades to default instead of aborting startup.
        assert!(state.repos.is_empty() && state.workspaces.is_empty());
        // Warns, and the bad file is preserved (not silently lost).
        assert!(warn.is_some_and(|w| w.contains("corrupt state")), "warns on a corrupt file");
        assert!(tmp.path().join("state.json.corrupt").exists(), "bad file backed up");
        assert!(!path.exists(), "corrupt state.json moved aside");

        // A SECOND corruption must not clobber the first/original backup.
        std::fs::write(&path, "garbage again").unwrap();
        let (_, warn3) = AppState::load_checked_from(tmp.path()).unwrap();
        assert!(warn3.is_some());
        assert!(tmp.path().join("state.json.corrupt").exists(), "original backup kept");
        let extra_backups = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with("state.json.corrupt") && n != "state.json.corrupt"
            })
            .count();
        assert_eq!(extra_backups, 1, "second corruption backed up under a distinct name");

        // A missing file is a silent default (no warning).
        let tmp2 = TempDir::new().unwrap();
        let (_, warn2) = AppState::load_checked_from(tmp2.path()).unwrap();
        assert!(warn2.is_none());
    }

    #[test]
    fn load_from_errors_on_corrupt_rather_than_resetting() {
        // The CLI path must abort (leaving the file recoverable), not silently
        // reset — only the TUI's load_checked_from degrades.
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("state.json"), "{ not valid json").unwrap();
        assert!(AppState::load_from(tmp.path()).is_err());
        assert!(tmp.path().join("state.json").exists(), "corrupt file left untouched");
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.repos.push(RepoEntry {
            id: "r1".into(),
            name: "x".into(),
            path: "/tmp/x".into(),
        });
        state.save_to(tmp.path()).unwrap();
        state.save_to(tmp.path()).unwrap(); // a second save must not leave temp debris
        assert!(tmp.path().join("state.json").exists());
        let temp_leftovers = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("state.json.tmp"))
            .count();
        assert_eq!(temp_leftovers, 0, "no temp file left after rename");
        let loaded = AppState::load_from(tmp.path()).unwrap();
        assert_eq!(loaded.repos.len(), 1);
    }

    #[test]
    fn add_repo_rejects_nonexistent_path() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        let result = state.add_repo_with_base("/nonexistent/path/xyz", tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist") || err.contains("not a directory"));
    }

    #[test]
    fn add_repo_rejects_duplicate_path() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = TempDir::new().unwrap();
        let mut state = AppState::default();

        state.add_repo_with_base(repo_dir.path().to_str().unwrap(), tmp.path()).unwrap();

        let result = state.add_repo_with_base(repo_dir.path().to_str().unwrap(), tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already tracked"));
    }

    #[test]
    fn add_repo_canonicalizes_path_and_derives_name() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = TempDir::new().unwrap();
        let mut state = AppState::default();
        let entry = state.add_repo_with_base(repo_dir.path().to_str().unwrap(), tmp.path()).unwrap();

        let expected_name = repo_dir.path().file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(entry.name, expected_name);
        assert!(entry.path.starts_with('/'));
    }

    #[test]
    fn run_git_status_errors_on_nonexistent_path() {
        let result = run_git_status("/nonexistent/path/xyz");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn run_git_status_errors_on_file_not_directory() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("not-a-dir.txt");
        fs::write(&file_path, "hello").unwrap();
        let result = run_git_status(file_path.to_str().unwrap());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not a directory"));
    }

    #[test]
    fn run_git_status_errors_on_non_git_directory() {
        let tmp = TempDir::new().unwrap();
        let result = run_git_status(tmp.path().to_str().unwrap());
        assert!(result.is_err());
    }

    // NOTE: no test in this module ever calls `init_profile` — the TUI test
    // harness sets KOMMAND0_STATE_DIR process-wide, and PROFILE is a
    // process-global OnceLock; the init_profile/state_dir glue is covered by
    // the process-level CLI and e2e tests instead. `resolve_profile` is pure,
    // so its precedence table lives here.

    #[test]
    fn resolve_profile_precedence_table() {
        let ok = |s: &str| Ok(Some(s.to_string()));
        // Flag wins over env; env used when no flag; nothing → None.
        assert_eq!(AppState::resolve_profile(Some("flag"), Some("env"), false), ok("flag"));
        assert_eq!(AppState::resolve_profile(None, Some("env"), false), ok("env"));
        assert_eq!(AppState::resolve_profile(None, None, false), Ok(None));
        // The exact-dir override wins SILENTLY over the env profile (children
        // of an env-mode parent stay hermetic even with a stale profile var)…
        assert_eq!(AppState::resolve_profile(None, Some("env"), true), Ok(None));
        // …but an explicit flag against it is a loud conflict.
        let err = AppState::resolve_profile(Some("flag"), None, true).unwrap_err();
        assert!(err.contains("cannot be combined"), "{err}");
        // Whichever source is used must be a valid name.
        assert!(AppState::resolve_profile(Some("../evil"), None, false).is_err());
        assert!(AppState::resolve_profile(None, Some("../evil"), false).is_err());
    }

    #[test]
    fn validate_profile_name_accepts_and_rejects() {
        let max_len = "a".repeat(64);
        for ok in ["work", "a.B-2_x", max_len.as_str()] {
            assert!(AppState::validate_profile_name(ok).is_ok(), "{ok:?} should be valid");
        }
        let too_long = "a".repeat(65);
        for bad in ["", ".", "..", "a/b", "a b", "-x", "café", too_long.as_str()] {
            let err = AppState::validate_profile_name(bad).unwrap_err();
            assert!(err.contains("invalid profile name"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn migrate_legacy_moves_state_and_config_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        let dir = tmp.path().join("profiles").join("default");
        assert!(dir.join("state.json").exists(), "state.json migrated");
        assert!(dir.join("config.json").exists(), "config.json migrated");
        assert!(!tmp.path().join("state.json").exists(), "root left clean");
        assert!(!tmp.path().join("config.json").exists(), "root left clean");
        // Second call: profiles/ exists, so the guard makes it a no-op.
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(dir.join("state.json").exists());
    }

    #[test]
    fn migrate_legacy_state_only() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(tmp.path().join("profiles").join("default").join("state.json").exists());
        assert!(!tmp.path().join("state.json").exists());
    }

    #[test]
    fn migrate_legacy_config_only_still_migrates() {
        // A user who only ever changed settings has config.json but no
        // state.json — those settings must not be orphaned at the old root.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(tmp.path().join("profiles").join("default").join("config.json").exists());
        assert!(!tmp.path().join("config.json").exists());
    }

    #[test]
    fn migrate_legacy_leaves_config_alone_when_config_env_overrides_it() {
        // KOMMAND0_CONFIG active (threaded as the flag — tests never touch
        // process env): the override may point AT the root config.json, so it
        // stays put; state.json still migrates. The `false` flavor of this
        // seed is `migrate_legacy_moves_state_and_config_and_is_idempotent`.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), true).unwrap();
        let dir = tmp.path().join("profiles").join("default");
        assert!(dir.join("state.json").exists(), "state.json still migrates");
        assert!(!dir.join("config.json").exists(), "config.json not moved");
        assert!(tmp.path().join("config.json").exists(), "config.json stays at the root");
    }

    #[test]
    fn migrate_legacy_noop_when_profiles_dir_exists() {
        // An existing profiles/ dir — even empty — means the migration already
        // ran (or a save beat us to it): never move anything then.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(tmp.path().join("state.json").exists(), "legacy file untouched");
        assert!(!tmp.path().join("profiles").join("default").exists(), "nothing moved");
    }

    #[test]
    fn migrate_legacy_fresh_dir_creates_nothing() {
        let tmp = TempDir::new().unwrap();
        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();
        assert!(
            !tmp.path().join("profiles").exists(),
            "a fresh install stays side-effect-free (first save creates the dir)"
        );
    }

    #[test]
    fn migrate_legacy_preserves_content_and_leaves_worktrees_in_place() {
        let tmp = TempDir::new().unwrap();
        let mut state = AppState::default();
        state.repos.push(RepoEntry {
            id: "r1".to_string(),
            name: "my-repo".to_string(),
            path: "/tmp/my-repo".to_string(),
        });
        state.save_to(tmp.path()).unwrap();
        fs::create_dir_all(tmp.path().join("worktrees").join("ws")).unwrap();

        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();

        let migrated =
            AppState::load_from(&tmp.path().join("profiles").join("default")).unwrap();
        assert_eq!(migrated.repos.len(), 1);
        assert_eq!(migrated.repos[0].name, "my-repo", "content survives the move");
        assert!(
            tmp.path().join("worktrees").join("ws").is_dir(),
            "worktrees stay at the old root (state stores absolute paths)"
        );
        assert!(!tmp.path().join("profiles").join("default").join("worktrees").exists());
    }

    #[cfg(unix)]
    #[test]
    fn migrate_legacy_failure_leaves_no_masking_husk_and_stays_retriable() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("state.json"), "{}").unwrap();
        // A dangling symlink at base/profiles: `exists()` follows the link
        // (stat), so the idempotence guard passes, and create_dir_all then
        // fails deterministically.
        std::os::unix::fs::symlink("missing", tmp.path().join("profiles")).unwrap();

        let err = AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap_err();
        assert!(err.to_string().contains("failed to migrate"), "got: {err}");
        assert!(tmp.path().join("state.json").exists(), "legacy file untouched");
        assert!(
            !tmp.path().join("profiles").exists(),
            "no masking husk left behind (a dangling link still counts as absent)"
        );
        // Retriable: a second run errors again rather than silently masking
        // the legacy file behind the guard.
        assert!(AppState::migrate_legacy_profiles_at(tmp.path(), false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn migrate_legacy_failure_message_names_the_failing_file() {
        // Config-only flavor of the husk test: the error must be keyed to the
        // file actually being migrated, not hardcoded to state.json.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        std::os::unix::fs::symlink("missing", tmp.path().join("profiles")).unwrap();
        let err = AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap_err();
        assert!(err.to_string().contains("config.json"), "file-keyed message: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn migrate_legacy_materializes_a_symlinked_config() {
        // A RELATIVE symlink would dangle when moved a level deeper into
        // profiles/default/ — the migration copies through the link instead
        // and drops the link.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("dotfiles-config.json"), r#"{"claude_bin":"x"}"#).unwrap();
        std::os::unix::fs::symlink("dotfiles-config.json", tmp.path().join("config.json"))
            .unwrap();

        AppState::migrate_legacy_profiles_at(tmp.path(), false).unwrap();

        let migrated = tmp.path().join("profiles").join("default").join("config.json");
        assert!(
            !fs::symlink_metadata(&migrated).unwrap().file_type().is_symlink(),
            "materialized as a regular file"
        );
        assert_eq!(
            fs::read_to_string(&migrated).unwrap(),
            r#"{"claude_bin":"x"}"#,
            "contents came through the link"
        );
        assert!(
            fs::symlink_metadata(tmp.path().join("config.json")).is_err(),
            "the link is gone from the root"
        );
        assert!(tmp.path().join("dotfiles-config.json").exists(), "link target untouched");
    }

    #[test]
    fn rename_profile_moves_dir_rewrites_paths_and_repairs_worktrees() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = tmp.path().join("repo");
        fs::create_dir_all(&repo_dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&repo_dir).output().unwrap()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "--allow-empty", "-m", "init"]);

        let profile = tmp.path().join("profiles").join("work");
        let mut state = AppState::default();
        let repo = state.add_repo_with_base(repo_dir.to_str().unwrap(), &profile).unwrap();
        let ws = state.create_workspace_with_base(Some("feat"), &repo.id, &profile).unwrap();
        assert!(ws.worktree_path.as_deref().unwrap().contains("/profiles/work/"), "sanity");
        let session = state.create_session_with_base(&ws.id, &profile).unwrap();
        assert!(session.log_file.contains("/profiles/work/"), "sanity");
        // A legacy-root worktree (pre-profiles layout, OUTSIDE the profile
        // dir) must come through untouched.
        let legacy_wt = tmp.path().join("worktrees").join("legacy-ws").to_string_lossy().into_owned();
        state.workspaces.push(Workspace {
            id: "legacy".into(),
            name: "legacy-ws".into(),
            repo_id: repo.id.clone(),
            working_dir: legacy_wt.clone(),
            active: true,
            created_at: 0,
            worktree_path: Some(legacy_wt.clone()),
            branch_name: None,
        });
        // A sibling profile that shares the name as a PREFIX (`work2`) must
        // never be rewritten — pins the path-boundary guard in the matcher.
        let boundary_wt =
            tmp.path().join("profiles").join("work2").join("worktrees").join("other").to_string_lossy().into_owned();
        state.workspaces.push(Workspace {
            id: "boundary".into(),
            name: "boundary-ws".into(),
            repo_id: repo.id.clone(),
            working_dir: boundary_wt.clone(),
            active: true,
            created_at: 0,
            worktree_path: Some(boundary_wt.clone()),
            branch_name: None,
        });
        // A fallback workspace (no worktree; working_dir = the repo root)
        // passes through untouched and uncounted.
        state.workspaces.push(Workspace {
            id: "fallback".into(),
            name: "fallback-ws".into(),
            repo_id: repo.id.clone(),
            working_dir: repo.path.clone(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: None,
        });
        state.save_to(&profile).unwrap();

        // An absent Claude projects root is a silent no-op (machine without
        // claude) — nothing created, no warnings.
        let no_claude = tmp.path().join("no-claude-store");
        let (rewritten, migrated, warnings) =
            AppState::rename_profile_at(tmp.path(), "work", "personal", &no_claude).unwrap();
        assert_eq!(rewritten, 2, "one worktree path + one session log");
        assert_eq!(migrated, 0, "no claude store to migrate");
        assert!(warnings.is_empty(), "repair should succeed: {warnings:?}");
        assert!(!no_claude.exists(), "absent projects root stays absent");
        assert!(!tmp.path().join("profiles").join("work").exists(), "old dir gone");
        let personal = tmp.path().join("profiles").join("personal");
        assert!(personal.is_dir(), "new dir present");

        let renamed = AppState::load_from(&personal).unwrap();
        let feat = renamed.workspaces.iter().find(|w| w.name == "feat").unwrap();
        let wt = feat.worktree_path.as_deref().unwrap();
        assert!(
            wt.contains(&format!("/profiles/personal/worktrees/{}/feat", repo.id)),
            "worktree rewritten: {wt}"
        );
        assert_eq!(feat.working_dir, wt, "working_dir follows its worktree");
        let legacy = renamed.workspaces.iter().find(|w| w.name == "legacy-ws").unwrap();
        assert_eq!(
            legacy.worktree_path.as_deref(),
            Some(legacy_wt.as_str()),
            "legacy-root path untouched"
        );
        let boundary = renamed.workspaces.iter().find(|w| w.name == "boundary-ws").unwrap();
        assert_eq!(
            boundary.worktree_path.as_deref(),
            Some(boundary_wt.as_str()),
            "prefix-sharing profile work2 untouched (path-boundary guard)"
        );
        assert_eq!(boundary.working_dir, boundary_wt, "work2 working_dir untouched");
        let fallback = renamed.workspaces.iter().find(|w| w.name == "fallback-ws").unwrap();
        assert_eq!(fallback.worktree_path, None, "fallback stays worktree-less");
        assert_eq!(fallback.working_dir, repo.path, "fallback working_dir (repo root) untouched");
        let log = &renamed.sessions[0].log_file;
        assert!(log.contains("/profiles/personal/sessions/"), "session log rewritten: {log}");

        // Only `git worktree repair` updates the repo's gitdir link after a
        // move — `worktree list` showing the NEW path proves it ran and stuck.
        let list = git(&["worktree", "list"]);
        let list = String::from_utf8_lossy(&list.stdout).to_string();
        assert!(
            list.contains(&format!("profiles/personal/worktrees/{}/feat", repo.id)),
            "gitdir repaired: {list}"
        );
        assert!(!list.contains("profiles/work/"), "no stale old worktree path: {list}");
    }

    #[test]
    fn rename_profile_fresh_profile_moves_clean() {
        // No state.json in the profile: the dir rename alone is complete.
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("profiles").join("scratch")).unwrap();
        let (rewritten, migrated, warnings) =
            AppState::rename_profile_at(tmp.path(), "scratch", "kept", &tmp.path().join("nc"))
                .unwrap();
        assert_eq!((rewritten, migrated), (0, 0));
        assert!(warnings.is_empty());
        assert!(tmp.path().join("profiles").join("kept").is_dir());
        assert!(!tmp.path().join("profiles").join("scratch").exists());
    }

    #[test]
    fn rename_profile_rejects_bad_input() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("profiles").join("a")).unwrap();
        fs::create_dir_all(tmp.path().join("profiles").join("b")).unwrap();
        let err = |old: &str, new: &str| {
            AppState::rename_profile_at(tmp.path(), old, new, &tmp.path().join("nc"))
                .unwrap_err()
                .to_string()
        };
        assert!(err("missing", "x").contains("not found"), "missing src");
        assert!(err("a", "b").contains("already exists"), "occupied dst");
        assert!(err("../evil", "x").contains("invalid profile name"), "bad old");
        assert!(err("a", "../evil").contains("invalid profile name"), "bad new");
        assert!(err("a", "a").contains("same"), "old == new");
        // None of the failed attempts moved anything.
        assert!(tmp.path().join("profiles").join("a").is_dir());
        assert!(tmp.path().join("profiles").join("b").is_dir());
    }

    #[test]
    fn rename_profile_corrupt_state_rolls_back() {
        // A failure after the dir move (here: unparseable state.json) must
        // move the dir back — an Err leaves the original profile intact.
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().join("profiles").join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("state.json"), "{ not json").unwrap();

        let err =
            AppState::rename_profile_at(tmp.path(), "work", "personal", &tmp.path().join("nc"))
                .unwrap_err();
        assert!(err.to_string().contains("failed to parse"), "load error surfaces: {err}");
        assert!(work.is_dir(), "dir moved back to profiles/work");
        assert!(!tmp.path().join("profiles").join("personal").exists(), "dst gone");
        assert!(work.join("state.json").exists(), "garbage file back where it started");
    }

    #[test]
    fn rename_profile_reports_repair_warnings_but_completes() {
        let tmp = TempDir::new().unwrap();
        let profile = tmp.path().join("profiles").join("work");
        let wt = |name: &str| {
            profile.join("worktrees").join(name).to_string_lossy().into_owned()
        };
        let mut state = AppState::default();
        // A repo whose path doesn't exist: `git -C` fails → repair warning.
        state.repos.push(RepoEntry {
            id: "gone".into(),
            name: "gone".into(),
            path: tmp.path().join("no-such-repo").to_string_lossy().into_owned(),
        });
        state.workspaces.push(Workspace {
            id: "w1".into(),
            name: "broken-repo-ws".into(),
            repo_id: "gone".into(),
            working_dir: wt("a"),
            active: true,
            created_at: 0,
            worktree_path: Some(wt("a")),
            branch_name: None,
        });
        // A workspace with an orphan repo_id: the no-repo warning.
        state.workspaces.push(Workspace {
            id: "w2".into(),
            name: "orphan-ws".into(),
            repo_id: "orphan".into(),
            working_dir: wt("b"),
            active: true,
            created_at: 0,
            worktree_path: Some(wt("b")),
            branch_name: None,
        });
        state.save_to(&profile).unwrap();

        let (rewritten, _migrated, warnings) =
            AppState::rename_profile_at(tmp.path(), "work", "personal", &tmp.path().join("nc"))
                .unwrap();
        assert_eq!(rewritten, 2, "both worktree paths rewritten despite warnings");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("no repo found")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("couldn't repair")), "{warnings:?}");
        let renamed =
            AppState::load_from(&tmp.path().join("profiles").join("personal")).unwrap();
        assert!(
            renamed.workspaces.iter().all(|w| {
                w.worktree_path.as_deref().unwrap().contains("/profiles/personal/")
            }),
            "rename + rewrite completed: {:?}",
            renamed.workspaces
        );
    }

    /// The claude store slug rule (`path.replace(/[^a-zA-Z0-9]/g, "-")` — a
    /// JS regex, so one dash per UTF-16 code unit). The rule itself is
    /// pinned independently by the literal rows in
    /// `claude_project_slug_matches_claude_utf16_rule`.
    fn expected_claude_slug(path: &str) -> String {
        path.chars()
            .flat_map(|c| {
                if c.is_ascii_alphanumeric() {
                    std::iter::repeat_n(c, 1)
                } else {
                    std::iter::repeat_n('-', c.len_utf16())
                }
            })
            .collect()
    }

    #[test]
    fn claude_project_slug_matches_claude_utf16_rule() {
        let slug = |s: &str| AppState::claude_project_slug(Path::new(s));
        assert_eq!(slug("/tmp/my-repo"), "-tmp-my-repo");
        // Per UTF-16 code unit, like claude's JS regex: a BMP char is one
        // dash, an astral-plane char (two units — legal in workspace names)
        // is two. One-dash emoji slugs would silently miss claude's dir.
        assert_eq!(slug("aéb"), "a-b");
        assert_eq!(slug("a🚀b"), "a--b");
    }

    /// One profile with a worktree-backed workspace (orphan repo, so the only
    /// expected warning is the no-repo one) — for the claude-store tests.
    fn profile_with_worktree_ws(tmp: &TempDir) -> String {
        let profile = tmp.path().join("profiles").join("work");
        let old_wt = profile.join("worktrees").join("feat").to_string_lossy().into_owned();
        let mut state = AppState::default();
        state.workspaces.push(Workspace {
            id: "w1".into(),
            name: "feat".into(),
            repo_id: "orphan".into(),
            working_dir: old_wt.clone(),
            active: true,
            created_at: 0,
            worktree_path: Some(old_wt.clone()),
            branch_name: None,
        });
        state.save_to(&profile).unwrap();
        old_wt
    }

    #[test]
    fn rename_profile_migrates_the_claude_project_store() {
        let tmp = TempDir::new().unwrap();
        let old_wt = profile_with_worktree_ws(&tmp);
        let projects = tmp.path().join("claude").join("projects");
        let old_slug = expected_claude_slug(&old_wt);
        fs::create_dir_all(projects.join(&old_slug)).unwrap();
        fs::write(projects.join(&old_slug).join("abc.jsonl"), "{}").unwrap();

        let (rewritten, migrated, warnings) =
            AppState::rename_profile_at(tmp.path(), "work", "personal", &projects).unwrap();
        assert_eq!((rewritten, migrated), (1, 1));
        let new_slug = expected_claude_slug(&old_wt.replace("/work/", "/personal/"));
        assert!(
            projects.join(&new_slug).join("abc.jsonl").exists(),
            "the store dir followed the worktree (transcript intact)"
        );
        assert!(!projects.join(&old_slug).exists(), "old store dir gone");
        // worktree_path and working_dir were the same dir — deduped, so no
        // second (colliding) move was attempted; only the orphan-repo warning.
        assert!(
            warnings.iter().all(|w| w.contains("no repo found")),
            "no claude warnings: {warnings:?}"
        );
    }

    #[test]
    fn rename_profile_claude_store_collision_warns_and_touches_nothing() {
        let tmp = TempDir::new().unwrap();
        let old_wt = profile_with_worktree_ws(&tmp);
        let projects = tmp.path().join("claude").join("projects");
        let old_slug = expected_claude_slug(&old_wt);
        let new_slug = expected_claude_slug(&old_wt.replace("/work/", "/personal/"));
        fs::create_dir_all(projects.join(&old_slug)).unwrap();
        fs::write(projects.join(&old_slug).join("old.jsonl"), "{}").unwrap();
        fs::create_dir_all(projects.join(&new_slug)).unwrap();
        fs::write(projects.join(&new_slug).join("taken.jsonl"), "{}").unwrap();

        let (_, migrated, warnings) =
            AppState::rename_profile_at(tmp.path(), "work", "personal", &projects).unwrap();
        assert_eq!(migrated, 0);
        assert!(
            warnings.iter().any(|w| w.contains("already exists")),
            "collision warned: {warnings:?}"
        );
        assert!(projects.join(&old_slug).join("old.jsonl").exists(), "source untouched");
        assert!(projects.join(&new_slug).join("taken.jsonl").exists(), "target untouched");
    }

    #[test]
    fn rename_profile_skips_overlong_claude_slugs_with_a_warning() {
        // claude truncates + hashes store names past ~200 chars — we can't
        // replicate the hash, so the migration must skip and say so.
        let tmp = TempDir::new().unwrap();
        let profile = tmp.path().join("profiles").join("work");
        let long_name = "a".repeat(200);
        let long_wt =
            profile.join("worktrees").join(&long_name).to_string_lossy().into_owned();
        let mut state = AppState::default();
        state.workspaces.push(Workspace {
            id: "w1".into(),
            name: "long".into(),
            repo_id: "orphan".into(),
            working_dir: long_wt.clone(),
            active: true,
            created_at: 0,
            worktree_path: Some(long_wt),
            branch_name: None,
        });
        state.save_to(&profile).unwrap();
        let projects = tmp.path().join("claude").join("projects");
        fs::create_dir_all(&projects).unwrap();

        let (_, migrated, warnings) =
            AppState::rename_profile_at(tmp.path(), "work", "personal", &projects).unwrap();
        assert_eq!(migrated, 0);
        assert!(
            warnings.iter().any(|w| w.contains("exceeds 200 chars")),
            "overlong slug warned: {warnings:?}"
        );
    }
}
