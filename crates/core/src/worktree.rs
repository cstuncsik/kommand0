use std::path::{Component, Path};
use std::process::Command;

use anyhow::Result;

/// Result of attempting to create a git worktree.
pub enum WorktreeResult {
    /// Worktree was created successfully.
    Created {
        worktree_path: String,
        branch_name: String,
    },
    /// Worktree creation failed; caller should fall back to repo root.
    Fallback {
        reason: String,
    },
}

/// Check if a path is a git repository.
fn is_git_repo(repo_path: &str) -> bool {
    Path::new(repo_path).join(".git").exists()
}

/// Check if a git branch exists.
fn branch_exists(repo_path: &str, branch: &str) -> bool {
    Command::new("git")
        .args(["-C", repo_path, "rev-parse", "--verify", branch])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether a fully-qualified ref (e.g. `refs/heads/foo`, `refs/remotes/origin/foo`)
/// resolves in the repo.
fn verify_ref(repo_path: &str, full_ref: &str) -> bool {
    Command::new("git")
        .args(["-C", repo_path, "rev-parse", "--verify", "--quiet", full_ref])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve `<base_dir>/worktrees/<name>` to an absolute path string, clearing a
/// stale worktree already there. `Err(reason)` if a dir still blocks the path.
fn prepare_worktree_dir(
    repo_path: &str,
    workspace_name: &str,
    base_dir: &Path,
) -> std::result::Result<String, String> {
    let worktree_dir = base_dir.join("worktrees").join(workspace_name);
    // The worktree dir doesn't exist yet; make the path absolute so git is happy.
    let worktree_dir = if worktree_dir.is_relative() {
        std::env::current_dir().unwrap_or_default().join(&worktree_dir)
    } else {
        worktree_dir
    };
    let worktree_path = worktree_dir.to_string_lossy().to_string();

    if worktree_dir.exists() {
        let _ = Command::new("git")
            .args(["-C", repo_path, "worktree", "remove", &worktree_path, "--force"])
            .output();
        if worktree_dir.exists() {
            return Err(format!("worktree path already exists: {worktree_path}"));
        }
    }
    Ok(worktree_path)
}

/// Find a unique branch name by appending -2, -3, etc.
fn unique_branch_name(repo_path: &str, base: &str) -> String {
    let candidate = format!("kommand0/{base}");
    if !branch_exists(repo_path, &candidate) {
        return candidate;
    }
    for i in 2..100 {
        let name = format!("kommand0/{base}-{i}");
        if !branch_exists(repo_path, &name) {
            return name;
        }
    }
    // Fallback with timestamp
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("kommand0/{base}-{ts}")
}

/// Create a git worktree for a workspace.
///
/// The worktree is placed at `<base_dir>/worktrees/<workspace_name>`.
/// A new branch `kommand0/<workspace_name>` is created.
///
/// Returns `WorktreeResult::Fallback` if the repo is not a git repo or
/// if worktree creation fails for any reason.
pub fn create_worktree(
    repo_path: &str,
    workspace_name: &str,
    base_dir: &Path,
) -> WorktreeResult {
    // Validate git repo
    if !is_git_repo(repo_path) {
        return WorktreeResult::Fallback {
            reason: format!("{repo_path} is not a git repository"),
        };
    }

    let worktree_path = match prepare_worktree_dir(repo_path, workspace_name, base_dir) {
        Ok(p) => p,
        Err(reason) => return WorktreeResult::Fallback { reason },
    };

    // Find a unique branch name
    let branch = unique_branch_name(repo_path, workspace_name);

    // Create the worktree on a fresh branch.
    let output = Command::new("git")
        .args(["-C", repo_path, "worktree", "add", &worktree_path, "-b", &branch])
        .output();
    finish_worktree_add(repo_path, output, worktree_path, branch)
}

/// Map the `git worktree add` result to a [`WorktreeResult`], copying the repo's
/// `.worktree-copy` files into a freshly-created worktree.
///
/// `repo_path` is the source repo root (where `.worktree-copy` lives). The copy
/// runs only on the success arm — a `Fallback` worktree never exists to copy
/// into — and is best-effort: a copy failure is logged, not propagated, so it
/// can't turn a `Created` worktree into a `Fallback`.
fn finish_worktree_add(
    repo_path: &str,
    output: std::io::Result<std::process::Output>,
    worktree_path: String,
    branch_name: String,
) -> WorktreeResult {
    match output {
        Ok(result) if result.status.success() => {
            copy_worktree_files(repo_path, &worktree_path);
            WorktreeResult::Created { worktree_path, branch_name }
        }
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            WorktreeResult::Fallback {
                reason: format!("git worktree add failed: {}", stderr.trim()),
            }
        }
        Err(e) => WorktreeResult::Fallback { reason: format!("failed to run git: {e}") },
    }
}

/// Copy the repo's configured files into a freshly-created worktree, mirroring
/// the user's `wt` shell helper.
///
/// The manifest is `<repo>/.worktree-copy`: one glob pattern per line (relative
/// to the repo root), with blank lines and `#` comments ignored. Every match is
/// copied into the worktree preserving its path relative to the root. When the
/// manifest is **absent or unreadable** the patterns fall back to `[".env*"]`
/// (the common case of carrying local env files across worktrees); a
/// **present-but-empty** manifest (all blank/comment lines) is an explicit
/// "copy nothing" and does NOT fall back.
///
/// Best-effort throughout: every failure is `tracing::warn!`-logged (never
/// stdout — that would corrupt the TUI's alt-screen) and skipped. The worktree
/// already exists, so a copy error must not be fatal.
fn copy_worktree_files(repo_path: &str, worktree_path: &str) {
    let root = Path::new(repo_path);
    let dest_root = Path::new(worktree_path);

    // Fallback is keyed on file PRESENCE (read error), not on an empty pattern
    // list: an empty-but-present manifest means "copy nothing".
    let patterns: Vec<String> = match std::fs::read_to_string(root.join(".worktree-copy")) {
        Ok(contents) => contents
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(String::from)
            .collect(),
        Err(_) => vec![".env*".to_string()],
    };

    for pattern in &patterns {
        copy_pattern(root, dest_root, pattern);
    }
}

/// Expand one glob `pattern` (relative to `root`) and copy each match into
/// `dest_root`, preserving the match's path relative to `root`.
///
/// The pattern is anchored to `root` by **string** concatenation
/// (`format!("{}/{}", root.display(), pattern)`), not `Path::join` — joining an
/// absolute pattern would discard `root`, and the `glob` crate otherwise walks
/// from the process cwd.
///
/// `require_literal_leading_dot` is gated per-pattern to be zsh-faithful: zsh
/// matches dot-leading names only when the pattern's filename component itself
/// begins with a literal `.`. So a bare `*` (or `*.rs`, `src/**/*.rs`) keeps the
/// guard ON and won't sweep `.git`/`.env`, while `.env*` (final component leads
/// with `.`) turns it OFF and matches the dotfiles. (The glob crate's directory
/// iterator drops *all* dot-leading children when the option is on, regardless
/// of the pattern — unlike its `Pattern::matches_with` — so a single fixed value
/// can't satisfy both cases; the gate replicates zsh.)
///
/// Each match's relative path is rejected if it contains a `..` or root
/// component, so a copy can never land outside the worktree. All failures are
/// warned and skipped.
fn copy_pattern(root: &Path, dest_root: &Path, pattern: &str) {
    let pat = format!("{}/{}", root.display(), pattern);
    // zsh matches dotfiles only when the filename component leads with a literal
    // `.`; mirror that by turning the guard off exactly for such patterns.
    let final_dot = pattern.rsplit('/').next().is_some_and(|c| c.starts_with('.'));
    let options = glob::MatchOptions {
        require_literal_leading_dot: !final_dot,
        ..Default::default()
    };
    let entries = match glob::glob_with(&pat, options) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(pattern, "bad worktree-copy glob pattern: {e}");
            return;
        }
    };

    for entry in entries.flatten() {
        let Ok(rel) = entry.strip_prefix(root) else {
            tracing::warn!(entry = %entry.display(), "worktree-copy match outside repo root; skipping");
            continue;
        };
        // Traversal guard: `strip_prefix` does not normalize, so a glob match
        // could still resolve with `..`/root components and escape the worktree.
        // Reject those rather than write outside it.
        if rel.components().any(|c| matches!(c, Component::ParentDir | Component::RootDir)) {
            tracing::warn!(rel = %rel.display(), "worktree-copy path escapes worktree; skipping");
            continue;
        }

        let dest = dest_root.join(rel);
        if let Some(parent) = dest.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(dest = %dest.display(), "worktree-copy mkdir failed: {e}");
            continue;
        }
        if let Err(e) = copy_recursive(&entry, &dest) {
            tracing::warn!(src = %entry.display(), dest = %dest.display(), "worktree-copy failed: {e}");
        }
    }
}

/// Recursively copy `src` to `dest`, mirroring `cp -r`.
///
/// A directory is created and its entries copied recursively; a regular file is
/// copied with [`std::fs::copy`]. A symlink is skipped (logged) — unlike `cp -r`
/// we don't follow it, which avoids symlink cycles and links that escape the
/// worktree. A small, deliberate divergence from `cp -r`.
fn copy_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        tracing::warn!(src = %src.display(), "worktree-copy skipping symlink");
        return Ok(());
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dest).map(|_| ())
    }
}

/// Create a worktree that checks out an EXISTING branch instead of forking a new
/// `kommand0/<name>` one. `branch_ref` may be a local branch (`feat/x`), a
/// remote-tracking ref (`origin/feat/x`), or a bare name that exists under
/// `origin/`. For a remote-only ref a local tracking branch is created; for a
/// local branch it's checked out directly (git refuses if it's already checked
/// out in another worktree — surfaced as a `Fallback`).
pub fn create_worktree_from_branch(
    repo_path: &str,
    workspace_name: &str,
    base_dir: &Path,
    branch_ref: &str,
) -> WorktreeResult {
    if !is_git_repo(repo_path) {
        return WorktreeResult::Fallback {
            reason: format!("{repo_path} is not a git repository"),
        };
    }
    let worktree_path = match prepare_worktree_dir(repo_path, workspace_name, base_dir) {
        Ok(p) => p,
        Err(reason) => return WorktreeResult::Fallback { reason },
    };

    // Resolve the ref: an existing local branch is checked out directly; a remote
    // ref gets a new local tracking branch (named after the remote's short name).
    let (args, branch_name): (Vec<String>, String) =
        if verify_ref(repo_path, &format!("refs/heads/{branch_ref}")) {
            (vec!["worktree".into(), "add".into(), worktree_path.clone(), branch_ref.into()],
             branch_ref.into())
        } else if verify_ref(repo_path, &format!("refs/remotes/{branch_ref}")) {
            // e.g. "origin/feat/x" -> local "feat/x"
            let local = branch_ref.split_once('/').map(|(_, r)| r).unwrap_or(branch_ref).to_string();
            (vec!["worktree".into(), "add".into(), "--track".into(), "-b".into(),
                  local.clone(), worktree_path.clone(), branch_ref.into()],
             local)
        } else if verify_ref(repo_path, &format!("refs/remotes/origin/{branch_ref}")) {
            (vec!["worktree".into(), "add".into(), "--track".into(), "-b".into(),
                  branch_ref.into(), worktree_path.clone(), format!("origin/{branch_ref}")],
             branch_ref.into())
        } else {
            return WorktreeResult::Fallback { reason: format!("branch not found: {branch_ref}") };
        };

    let output = Command::new("git").args(["-C", repo_path]).args(&args).output();
    finish_worktree_add(repo_path, output, worktree_path, branch_name)
}

/// Remove a git worktree. Idempotent — returns Ok if path doesn't exist.
///
/// Uses `--force` to handle dirty worktrees (since the workspace is being deleted).
pub fn remove_worktree(repo_path: &str, worktree_path: &str) -> Result<()> {
    if !Path::new(worktree_path).exists() {
        return Ok(());
    }

    let output = Command::new("git")
        .args([
            "-C",
            repo_path,
            "worktree",
            "remove",
            worktree_path,
            "--force",
        ])
        .output();

    match output {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            // Log but don't fail — worktree removal shouldn't block workspace
            // deletion. `tracing` (not stderr, which would corrupt the TUI's
            // alt-screen) so it reaches the app's log file.
            tracing::warn!(
                worktree = worktree_path,
                "git worktree remove failed: {}",
                stderr.trim()
            );
            Ok(())
        }
        Err(e) => {
            tracing::warn!(worktree = worktree_path, "failed to run git worktree remove: {e}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_git_repo(dir: &Path) {
        // Set a local identity so the commit succeeds on a runner with no global
        // git config (e.g. Linux CI); `-b main` pins the initial branch. Without
        // the commit, HEAD is unborn and branch-collision detection misbehaves.
        let git = |args: &[&str]| Command::new("git").args(args).current_dir(dir).output().unwrap();
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "--allow-empty", "-m", "init"]);
    }

    /// Write `contents` to `dir/rel`, creating parent dirs as needed.
    fn write_file(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn create_worktree_not_git_repo() {
        let tmp = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        let result = create_worktree(
            tmp.path().to_str().unwrap(),
            "test-ws",
            base.path(),
        );
        match result {
            WorktreeResult::Fallback { reason } => {
                assert!(reason.contains("not a git repository"));
            }
            WorktreeResult::Created { .. } => panic!("expected fallback"),
        }
    }

    #[test]
    fn create_and_remove_worktree() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());

        let result = create_worktree(
            repo.path().to_str().unwrap(),
            "my-feature",
            base.path(),
        );
        match result {
            WorktreeResult::Created {
                worktree_path,
                branch_name,
            } => {
                assert!(Path::new(&worktree_path).exists());
                assert!(branch_name.starts_with("kommand0/"));

                // Remove it
                remove_worktree(
                    repo.path().to_str().unwrap(),
                    &worktree_path,
                )
                .unwrap();
                assert!(!Path::new(&worktree_path).exists());
            }
            WorktreeResult::Fallback { reason } => {
                panic!("expected Created, got Fallback: {reason}");
            }
        }
    }

    #[test]
    fn remove_nonexistent_worktree_ok() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let result = remove_worktree(
            repo.path().to_str().unwrap(),
            "/nonexistent/path",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn unique_branch_handles_collision() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());

        // Create first worktree
        let result1 = create_worktree(
            repo.path().to_str().unwrap(),
            "feature",
            base.path(),
        );
        assert!(matches!(result1, WorktreeResult::Created { .. }));

        // Create second worktree with same name but different base
        let base2 = TempDir::new().unwrap();
        let result2 = create_worktree(
            repo.path().to_str().unwrap(),
            "feature",
            base2.path(),
        );
        match result2 {
            WorktreeResult::Created { branch_name, .. } => {
                // Should get a suffixed branch name
                assert!(
                    branch_name == "kommand0/feature-2" || branch_name.starts_with("kommand0/feature-"),
                    "expected suffixed branch, got: {branch_name}"
                );
            }
            WorktreeResult::Fallback { reason } => {
                panic!("expected Created, got Fallback: {reason}");
            }
        }
    }

    #[test]
    fn from_existing_local_branch_checks_it_out() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        Command::new("git").args(["-C", rp, "branch", "feat"]).output().unwrap();

        match create_worktree_from_branch(rp, "ws", base.path(), "feat") {
            WorktreeResult::Created { worktree_path, branch_name } => {
                assert!(Path::new(&worktree_path).exists());
                assert_eq!(branch_name, "feat", "existing branch checked out as-is (no kommand0/ prefix)");
                let head = Command::new("git")
                    .args(["-C", &worktree_path, "rev-parse", "--abbrev-ref", "HEAD"])
                    .output()
                    .unwrap();
                assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "feat");
            }
            WorktreeResult::Fallback { reason } => panic!("expected Created, got: {reason}"),
        }
    }

    #[test]
    fn from_missing_branch_is_a_fallback() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        match create_worktree_from_branch(repo.path().to_str().unwrap(), "ws", base.path(), "nope") {
            WorktreeResult::Fallback { reason } => assert!(reason.contains("branch not found"), "got: {reason}"),
            WorktreeResult::Created { .. } => panic!("expected Fallback for a missing branch"),
        }
    }

    #[test]
    fn from_remote_branch_creates_a_tracking_branch() {
        // An "origin" repo with a branch only present there...
        let origin = TempDir::new().unwrap();
        init_git_repo(origin.path());
        let op = origin.path().to_str().unwrap();
        Command::new("git").args(["-C", op, "branch", "feat"]).output().unwrap();
        // ...cloned, so the clone has `origin/feat` but no local `feat`.
        let work = TempDir::new().unwrap();
        let clone = work.path().join("repo");
        Command::new("git").args(["clone", op, clone.to_str().unwrap()]).output().unwrap();
        let base = TempDir::new().unwrap();

        match create_worktree_from_branch(clone.to_str().unwrap(), "ws", base.path(), "feat") {
            WorktreeResult::Created { worktree_path, branch_name } => {
                assert_eq!(branch_name, "feat", "a local branch is created for the remote ref");
                let up = Command::new("git")
                    .args(["-C", &worktree_path, "rev-parse", "--abbrev-ref", "feat@{upstream}"])
                    .output()
                    .unwrap();
                assert_eq!(String::from_utf8_lossy(&up.stdout).trim(), "origin/feat", "tracks the remote");
            }
            WorktreeResult::Fallback { reason } => panic!("expected Created (tracking), got: {reason}"),
        }
    }

    // --- worktree file copy --------------------------------------------------

    #[test]
    fn manifest_copies_root_file_and_nested_dir() {
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        write_file(root.path(), ".worktree-copy", "root.txt\nconfig/app/x.json\n");
        write_file(root.path(), "root.txt", "r");
        write_file(root.path(), "config/app/x.json", "j");
        // A sibling not listed in the manifest must NOT be copied.
        write_file(root.path(), "config/app/other.json", "o");

        copy_worktree_files(root.path().to_str().unwrap(), dest.path().to_str().unwrap());

        assert_eq!(std::fs::read_to_string(dest.path().join("root.txt")).unwrap(), "r");
        // The multi-level nested path is preserved.
        assert_eq!(std::fs::read_to_string(dest.path().join("config/app/x.json")).unwrap(), "j");
        assert!(!dest.path().join("config/app/other.json").exists(), "unlisted sibling not copied");
    }

    #[test]
    fn manifest_ignores_blank_lines_and_comments() {
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        let manifest = "# a comment\n\n   \nkeep.txt\n  # indented comment\n";
        write_file(root.path(), ".worktree-copy", manifest);
        write_file(root.path(), "keep.txt", "k");

        copy_worktree_files(root.path().to_str().unwrap(), dest.path().to_str().unwrap());

        assert_eq!(std::fs::read_to_string(dest.path().join("keep.txt")).unwrap(), "k");
    }

    #[test]
    fn recursive_glob_preserves_depth() {
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        write_file(root.path(), ".worktree-copy", "src/**/*.rs\n");
        write_file(root.path(), "src/a.rs", "a");
        write_file(root.path(), "src/deep/b.rs", "b");
        write_file(root.path(), "src/deep/deeper/c.rs", "c");
        write_file(root.path(), "src/notrust.txt", "x");

        copy_worktree_files(root.path().to_str().unwrap(), dest.path().to_str().unwrap());

        assert_eq!(std::fs::read_to_string(dest.path().join("src/a.rs")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(dest.path().join("src/deep/b.rs")).unwrap(), "b");
        let c = std::fs::read_to_string(dest.path().join("src/deep/deeper/c.rs")).unwrap();
        assert_eq!(c, "c");
        assert!(!dest.path().join("src/notrust.txt").exists(), "non-.rs not matched");
    }

    #[test]
    fn absent_manifest_falls_back_to_env_glob() {
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        // No .worktree-copy -> fallback to `.env*`.
        write_file(root.path(), ".env", "1");
        write_file(root.path(), ".env.local", "2");
        write_file(root.path(), ".env.x", "3");
        write_file(root.path(), "notenv", "no");

        copy_worktree_files(root.path().to_str().unwrap(), dest.path().to_str().unwrap());

        assert_eq!(std::fs::read_to_string(dest.path().join(".env")).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(dest.path().join(".env.local")).unwrap(), "2");
        assert_eq!(std::fs::read_to_string(dest.path().join(".env.x")).unwrap(), "3");
        assert!(!dest.path().join("notenv").exists(), "`.env*` must not match `notenv`");
    }

    #[test]
    fn empty_present_manifest_copies_nothing() {
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        // Present but all-blank/comment -> explicit "copy nothing", NO fallback.
        write_file(root.path(), ".worktree-copy", "# nothing here\n\n");
        write_file(root.path(), ".env", "secret");

        copy_worktree_files(root.path().to_str().unwrap(), dest.path().to_str().unwrap());

        assert!(!dest.path().join(".env").exists(), "empty manifest must not fall back");
        assert!(std::fs::read_dir(dest.path()).unwrap().next().is_none(), "dest left empty");
    }

    #[test]
    fn bare_star_does_not_match_dotfiles() {
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        write_file(root.path(), ".worktree-copy", "*\n");
        write_file(root.path(), "visible.txt", "v");
        write_file(root.path(), ".env", "secret");
        write_file(root.path(), ".gitignore", "ignored");

        copy_worktree_files(root.path().to_str().unwrap(), dest.path().to_str().unwrap());

        assert_eq!(std::fs::read_to_string(dest.path().join("visible.txt")).unwrap(), "v");
        // require_literal_leading_dot: a bare `*` must not sweep dotfiles.
        assert!(!dest.path().join(".env").exists(), "bare * must not match .env");
        assert!(!dest.path().join(".gitignore").exists(), "bare * must not match .gitignore");
    }

    #[test]
    fn copy_pattern_is_best_effort_on_fs_error() {
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        write_file(root.path(), "sub/x.txt", "x");
        // Pre-create `sub` in the destination as a *file*, so create_dir_all for
        // the match's parent fails -> the copy is skipped, no panic.
        std::fs::write(dest.path().join("sub"), "blocker").unwrap();

        // Must not panic.
        copy_pattern(root.path(), dest.path(), "sub/x.txt");

        // The blocker file is untouched (the copy was skipped, not forced).
        assert_eq!(std::fs::read_to_string(dest.path().join("sub")).unwrap(), "blocker");
    }

    #[test]
    fn traversal_guard_skips_parent_dir_match() {
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        // `secret.txt` sits OUTSIDE root, alongside it.
        let outside = root.path().parent().unwrap().join("secret.txt");
        std::fs::write(&outside, "leak").unwrap();
        // A real file under root so the glob's directory walk has somewhere to start.
        write_file(root.path(), "inside/here.txt", "ok");

        // Pattern resolves to `<root>/inside/../../secret.txt`; strip_prefix(root)
        // succeeds (leaves `inside/../../secret.txt`) but the `..` components trip
        // the guard, so nothing escaping is written.
        copy_pattern(root.path(), dest.path(), "inside/../../secret.txt");

        assert!(std::fs::read_dir(dest.path()).unwrap().next().is_none(), "no escaping copy");
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn copy_recursive_skips_symlinks() {
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        write_file(root.path(), "real.txt", "r");
        #[cfg(unix)]
        {
            let link = root.path().join("link.txt");
            std::os::unix::fs::symlink(root.path().join("real.txt"), &link).unwrap();
            copy_recursive(&link, &dest.path().join("link.txt")).unwrap();
            assert!(!dest.path().join("link.txt").exists(), "symlink skipped, not followed");
        }
        // A regular file still copies (sanity that we only special-case symlinks).
        copy_recursive(&root.path().join("real.txt"), &dest.path().join("real.txt")).unwrap();
        assert_eq!(std::fs::read_to_string(dest.path().join("real.txt")).unwrap(), "r");
    }

    #[test]
    fn create_worktree_copies_env_into_worktree() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        // No manifest -> fallback copies `.env*` into the new worktree.
        write_file(repo.path(), ".env", "API=1");

        match create_worktree(repo.path().to_str().unwrap(), "ws", base.path()) {
            WorktreeResult::Created { worktree_path, .. } => {
                let env = std::fs::read_to_string(Path::new(&worktree_path).join(".env")).unwrap();
                assert_eq!(env, "API=1", ".env copied into the worktree on the Created arm");
            }
            WorktreeResult::Fallback { reason } => panic!("expected Created, got: {reason}"),
        }
    }

    #[test]
    fn fallback_worktree_copies_nothing() {
        // A non-git dir yields WorktreeResult::Fallback; the copy must not run
        // (no worktree exists). `.env` present proves we don't copy on Fallback.
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        write_file(repo.path(), ".env", "API=1");

        let worktree_dir = base.path().join("worktrees").join("ws");
        match create_worktree(repo.path().to_str().unwrap(), "ws", base.path()) {
            WorktreeResult::Fallback { .. } => {
                assert!(!worktree_dir.exists(), "no worktree created, nothing copied");
            }
            WorktreeResult::Created { .. } => panic!("expected Fallback for a non-git dir"),
        }
    }
}
