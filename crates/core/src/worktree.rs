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

/// Whether a fully-qualified ref (e.g. `refs/heads/foo`, `refs/remotes/origin/foo`)
/// exists in the repo. Uses `show-ref --verify` (exact ref lookup), not
/// `rev-parse` (which applies revision syntax, so e.g. `main^{commit}` would
/// false-positively resolve).
fn verify_ref(repo_path: &str, full_ref: &str) -> bool {
    Command::new("git")
        .args(["-C", repo_path, "show-ref", "--verify", "--quiet", full_ref])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True if a branch named exactly `name` exists locally (`refs/heads/<name>`) or
/// on origin (`refs/remotes/origin/<name>`). Exact-name lookup — a legacy
/// `kommand0/<name>` branch does NOT count — via `git show-ref --verify`, so
/// revision syntax like `main^{commit}` never matches. Both the "branch exists →
/// checkout or fork?" offer and [`unique_branch_name`] use THIS check, so
/// detection and minting agree on what "exists" means (an origin-only branch
/// must suffix the fork, or we'd mint a bare local branch shadowing a divergent
/// `origin/<name>`).
pub fn branch_exists_bare(repo_path: &str, name: &str) -> bool {
    // "HEAD" is never a branch: `refs/remotes/origin/HEAD` is a symbolic pointer
    // to the default branch, so treat it as no match rather than a false positive.
    if name == "HEAD" {
        return false;
    }
    verify_ref(repo_path, &format!("refs/heads/{name}"))
        || verify_ref(repo_path, &format!("refs/remotes/origin/{name}"))
}

/// Resolve `<base_dir>/worktrees/<repo_id>/<workspace_name>` to an absolute
/// path string, clearing a stale worktree already there. `Err(reason)` if the
/// repo id is unsafe as a path segment or a dir still blocks the path.
///
/// Coexistence invariant: `worktrees/` may contain legacy flat `<name>` dirs
/// (pre-nesting layout) alongside `<repo-id>/` dirs. Paths are read from
/// state, never re-derived from names; never sweep `worktrees/` by pattern.
fn prepare_worktree_dir(
    repo_path: &str,
    repo_id: &str,
    workspace_name: &str,
    base_dir: &Path,
) -> std::result::Result<String, String> {
    // state.json is hand-editable: the id must be exactly one normal path
    // segment (rejects `.`/`..`, separators, and a Windows drive prefix like
    // `C:x`, any of which would make `join` escape `worktrees/`). A leading
    // dash could trip git arg parsing, and a backslash is refused even where
    // it is a plain segment char (mirrors `validate_new_workspace_name`).
    let mut comps = Path::new(repo_id).components();
    let one_normal = matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none();
    if repo_id.trim().is_empty()
        || repo_id.starts_with('-')
        || repo_id.contains('\\')
        || !one_normal
    {
        return Err(format!("invalid repo id for worktree dir: {repo_id:?}"));
    }
    let parent = base_dir.join("worktrees").join(repo_id);
    // A legacy flat WORKTREE that happens to be named like this repo's id:
    // nesting inside a checked-out tree would let that legacy workspace's
    // later deletion recursively remove the nested worktrees. Refuse instead.
    if parent.join(".git").exists() {
        return Err(format!("parent dir is a checked-out worktree: {}", parent.display()));
    }
    let worktree_dir = parent.join(workspace_name);
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

/// Find a unique branch name by appending -2, -3, etc. Uniqueness is checked
/// against local AND origin refs (`branch_exists_bare`), so a fork after the
/// checkout offer always suffixes rather than shadowing the existing branch.
fn unique_branch_name(repo_path: &str, base: &str) -> String {
    if !branch_exists_bare(repo_path, base) {
        return base.to_string();
    }
    for i in 2..100 {
        let name = format!("{base}-{i}");
        if !branch_exists_bare(repo_path, &name) {
            return name;
        }
    }
    // Fallback with timestamp
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{base}-{ts}")
}

/// Create a git worktree for a workspace.
///
/// The worktree is placed at `<base_dir>/worktrees/<repo_id>/<workspace_name>`.
/// A new branch named after the workspace is created (suffixed `-2`, `-3`, …
/// when a branch of that name already exists locally or on origin).
///
/// Returns `WorktreeResult::Fallback` if the repo is not a git repo or
/// if worktree creation fails for any reason.
pub fn create_worktree(
    repo_path: &str,
    repo_id: &str,
    workspace_name: &str,
    base_dir: &Path,
) -> WorktreeResult {
    // Validate git repo
    if !is_git_repo(repo_path) {
        return WorktreeResult::Fallback {
            reason: format!("{repo_path} is not a git repository"),
        };
    }

    let worktree_path = match prepare_worktree_dir(repo_path, repo_id, workspace_name, base_dir) {
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
/// manifest is **absent** the patterns fall back to `[".env*"]` (the common case
/// of carrying local env files across worktrees); a **present-but-empty**
/// manifest (all blank/comment lines) is an explicit "copy nothing" and does NOT
/// fall back, and a present-but-unreadable one copies nothing (no surprise env
/// copy on a permission/UTF-8 error).
///
/// Best-effort throughout: every failure is `tracing::warn!`-logged (never
/// stdout — that would corrupt the TUI's alt-screen) and skipped. The worktree
/// already exists, so a copy error must not be fatal.
fn copy_worktree_files(repo_path: &str, worktree_path: &str) {
    let root = Path::new(repo_path);
    let dest_root = Path::new(worktree_path);

    // Fallback fires only when the manifest is ABSENT. A present-but-empty file
    // means "copy nothing"; a present-but-unreadable one (permissions, non-UTF-8)
    // also copies nothing rather than silently falling back to `.env*`.
    let patterns: Vec<String> = match std::fs::read_to_string(root.join(".worktree-copy")) {
        Ok(contents) => contents
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(String::from)
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![".env*".to_string()],
        Err(e) => {
            tracing::warn!("worktree-copy: cannot read .worktree-copy ({e}); copying nothing");
            return;
        }
    };

    for pattern in &patterns {
        copy_pattern(root, dest_root, pattern);
    }
}

/// Expand one glob `pattern` (relative to `root`) and copy each match into
/// `dest_root`, preserving the match's path relative to `root`.
///
/// Anchoring: the pattern is glued to `root` by string concatenation (not
/// `Path::join`, which would discard an absolute pattern), with `root` escaped so
/// glob metacharacters in the repo path itself (`[`, `*`) are treated literally;
/// `glob` would otherwise walk from the process cwd.
///
/// Matching mirrors zsh: **case-sensitive** (unlike `MatchOptions::default()`),
/// and `require_literal_leading_dot` is gated per pattern off its FINAL component
/// so `.env*` matches dotfiles while a bare `*` (or `src/**/*.rs`) does not. The
/// gate keys on the last segment only; exotic patterns mixing dot and non-dot
/// directory segments (e.g. `*/.env*`) may differ slightly from zsh.
///
/// Escapes are blocked on both sides: a match whose relative path has a `..`/root
/// component, or whose real (canonicalized) path leaves `root`, is skipped —
/// `glob` follows symlinked directories, so `link/secret` can resolve outside the
/// repo with a clean-looking `rel`. A destination that would be written through a
/// pre-existing symlink in the worktree is also skipped. All failures are warned
/// and skipped.
fn copy_pattern(root: &Path, dest_root: &Path, pattern: &str) {
    let pat = format!("{}/{}", glob::Pattern::escape(&root.to_string_lossy()), pattern);
    // zsh matches dotfiles only when the final component leads with a literal `.`.
    let final_dot = pattern.rsplit('/').next().is_some_and(|c| c.starts_with('.'));
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: !final_dot,
    };
    let entries = match glob::glob_with(&pat, options) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(pattern, "bad worktree-copy glob pattern: {e}");
            return;
        }
    };
    // Real repo root, to confirm each match stays inside it once symlinks resolve.
    let canon_root = match root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("worktree-copy: cannot canonicalize repo root: {e}");
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!("worktree-copy glob walk error: {e}");
                continue;
            }
        };
        let Ok(rel) = entry.strip_prefix(root) else {
            tracing::warn!(entry = %entry.display(), "worktree-copy match outside repo root; skipping");
            continue;
        };
        // Source guard: cheap `..`/root reject (strip_prefix doesn't normalize),
        // then a canonical-containment check, since `glob` follows symlinked dirs.
        if rel.components().any(|c| matches!(c, Component::ParentDir | Component::RootDir)) {
            tracing::warn!(rel = %rel.display(), "worktree-copy path escapes worktree; skipping");
            continue;
        }
        match entry.canonicalize() {
            Ok(real) if real.starts_with(&canon_root) => {}
            _ => {
                tracing::warn!(entry = %entry.display(), "worktree-copy match resolves outside repo (symlink?); skipping");
                continue;
            }
        }

        let dest = dest_root.join(rel);
        // Dest guard: don't write THROUGH a pre-existing symlink in the worktree
        // (a committed symlink could redirect the write outside it).
        if !dest_path_is_safe(dest_root, rel) {
            tracing::warn!(dest = %dest.display(), "worktree-copy dest crosses a symlink; skipping");
            continue;
        }
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

/// True if no already-existing ancestor of `rel` under `dest_root` is a symlink —
/// i.e. writing `dest_root/rel` won't follow a link out of the worktree. Missing
/// components are fine (created later as real dirs); only an existing symlink
/// component is unsafe.
fn dest_path_is_safe(dest_root: &Path, rel: &Path) -> bool {
    let mut p = dest_root.to_path_buf();
    for comp in rel.components() {
        p.push(comp);
        if let Ok(meta) = std::fs::symlink_metadata(&p)
            && meta.file_type().is_symlink()
        {
            return false;
        }
    }
    true
}

/// Recursively copy `src` to `dest`, mirroring `cp -r`.
///
/// A directory is created and its entries copied recursively; a regular file is
/// copied with [`std::fs::copy`]. A symlink encountered during the walk is
/// skipped (logged) — unlike `cp -r` we don't follow it, avoiding cycles and
/// stray link targets. A small, deliberate divergence from `cp -r`. (Matches that
/// escape the repo via a symlinked component are already rejected in
/// `copy_pattern`; this skip covers symlinks found while recursing a copied dir.)
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

/// Create a worktree (at `<base_dir>/worktrees/<repo_id>/<workspace_name>`)
/// that checks out an EXISTING branch instead of forking a
/// new one. `branch_ref` may be a local branch (`feat/x`), a
/// remote-tracking ref (`origin/feat/x`), or a bare name that exists under
/// `origin/`. For a remote-only ref a local tracking branch is created; for a
/// local branch it's checked out directly (git refuses if it's already checked
/// out in another worktree — surfaced as a `Fallback`).
pub fn create_worktree_from_branch(
    repo_path: &str,
    repo_id: &str,
    workspace_name: &str,
    base_dir: &Path,
    branch_ref: &str,
) -> WorktreeResult {
    if !is_git_repo(repo_path) {
        return WorktreeResult::Fallback {
            reason: format!("{repo_path} is not a git repository"),
        };
    }
    let worktree_path = match prepare_worktree_dir(repo_path, repo_id, workspace_name, base_dir) {
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

/// Best-effort rmdir of the worktree's parent (`worktrees/<repo-id>/`) once
/// its last worktree is gone. `remove_dir` (never `_all`) only deletes an
/// EMPTY dir, so a sibling worktree keeps it alive; for a legacy flat
/// worktree the parent is `worktrees/` itself, removed only when empty and
/// recreated on demand, a harmless no-op.
fn remove_empty_parent(worktree_path: &str) {
    if let Some(parent) = Path::new(worktree_path).parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Remove a git worktree. Idempotent — returns Ok if path doesn't exist.
///
/// Uses `--force` to handle dirty worktrees (since the workspace is being deleted).
pub fn remove_worktree(repo_path: &str, worktree_path: &str) -> Result<()> {
    if !Path::new(worktree_path).exists() {
        // Common flow: the merged-PR cleanup already removed the worktree and
        // the follow-up workspace delete lands here; still clear a now-empty
        // worktrees/<repo-id>/ parent. NOT done on the failure arms below
        // (git refusing can mean the state path never was a worktree).
        remove_empty_parent(worktree_path);
        return Ok(());
    }

    // `--` so a state-authored path can never parse as an option.
    let output = Command::new("git")
        .args([
            "-C",
            repo_path,
            "worktree",
            "remove",
            "--force",
            "--",
            worktree_path,
        ])
        .output();

    match output {
        Ok(result) if result.status.success() => {
            remove_empty_parent(worktree_path);
            Ok(())
        }
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
            "r1",
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
            "r1",
            "my-feature",
            base.path(),
        );
        match result {
            WorktreeResult::Created {
                worktree_path,
                branch_name,
            } => {
                assert!(Path::new(&worktree_path).exists());
                assert_eq!(branch_name, "my-feature", "branch is named after the workspace");

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
    fn create_worktree_rejects_unsafe_repo_ids() {
        // state.json is hand-editable: a repo id with a path separator or
        // `..` must not let the worktree dir escape `worktrees/` (analogue of
        // create_workspace_rejects_unsafe_names for the sibling segment).
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        for bad in ["", "  ", ".", "..", "a/b", "a\\b", "-rf"] {
            match create_worktree(repo.path().to_str().unwrap(), bad, "ws", base.path()) {
                WorktreeResult::Fallback { reason } => {
                    assert!(reason.contains("invalid repo id"), "{bad:?}: {reason}");
                }
                WorktreeResult::Created { .. } => panic!("{bad:?} must be rejected"),
            }
        }
        // Nothing escaped `worktrees/` (a `..` id would land at base/ws) and
        // the rejection fired before any dir was created at all.
        assert!(!base.path().join("ws").exists(), "`..` id must not escape worktrees/");
        assert!(!base.path().join("worktrees").exists(), "no dir created for a rejected id");
    }

    #[test]
    fn create_worktree_refuses_a_flat_worktree_parent() {
        // A legacy flat worktree named exactly like this repo's id: nesting
        // inside a checked-out tree would let its later deletion recursively
        // remove the nested worktrees, so it must fall back instead.
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        let flat = base.path().join("worktrees").join("r1");
        let out = Command::new("git")
            .args(["-C", rp, "worktree", "add", flat.to_str().unwrap(), "-b", "legacy"])
            .output()
            .unwrap();
        assert!(out.status.success(), "flat add: {}", String::from_utf8_lossy(&out.stderr));

        match create_worktree(rp, "r1", "ws", base.path()) {
            WorktreeResult::Fallback { reason } => {
                assert!(reason.contains("checked-out worktree"), "{reason}");
            }
            WorktreeResult::Created { .. } => panic!("must not nest inside a checked-out tree"),
        }
        assert!(flat.join(".git").exists(), "flat worktree untouched");
        assert!(!flat.join("ws").exists(), "nothing created inside it");
    }

    #[test]
    fn remove_worktree_clears_the_empty_repo_id_parent() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        let create = |name: &str| match create_worktree(rp, "r1", name, base.path()) {
            WorktreeResult::Created { worktree_path, .. } => worktree_path,
            WorktreeResult::Fallback { reason } => panic!("expected Created, got: {reason}"),
        };
        let a = create("a");
        let b = create("b");
        let parent = base.path().join("worktrees").join("r1");

        remove_worktree(rp, &a).unwrap();
        assert!(Path::new(&b).exists(), "sibling worktree survives (rmdir, not remove_dir_all)");
        assert!(parent.exists(), "parent stays while a sibling remains");

        remove_worktree(rp, &b).unwrap();
        assert!(!parent.exists(), "empty worktrees/<repo-id>/ removed with the last worktree");
    }

    #[test]
    fn remove_worktree_already_gone_still_clears_the_empty_parent() {
        // The merged-PR cleanup removes the worktree first; the follow-up
        // workspace delete hits remove_worktree's early return, which must
        // still clear the now-empty worktrees/<repo-id>/ dir.
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        let path = match create_worktree(rp, "r1", "ws", base.path()) {
            WorktreeResult::Created { worktree_path, .. } => worktree_path,
            WorktreeResult::Fallback { reason } => panic!("expected Created, got: {reason}"),
        };
        // Remove the worktree out-of-band (as cleanup_merged_workspace does).
        Command::new("git")
            .args(["-C", rp, "worktree", "remove", &path, "--force"])
            .output()
            .unwrap();
        assert!(!Path::new(&path).exists(), "sanity: worktree gone");

        remove_worktree(rp, &path).unwrap();
        assert!(
            !base.path().join("worktrees").join("r1").exists(),
            "empty parent cleared on the early return"
        );
    }

    #[test]
    fn unique_branch_handles_collision() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());

        // Create first worktree
        let result1 = create_worktree(
            repo.path().to_str().unwrap(),
            "r1",
            "feature",
            base.path(),
        );
        assert!(matches!(result1, WorktreeResult::Created { .. }));

        // Create second worktree with same name but different base
        let base2 = TempDir::new().unwrap();
        let result2 = create_worktree(
            repo.path().to_str().unwrap(),
            "r1",
            "feature",
            base2.path(),
        );
        match result2 {
            WorktreeResult::Created { branch_name, .. } => {
                assert_eq!(branch_name, "feature-2", "second workspace gets a suffixed branch");
            }
            WorktreeResult::Fallback { reason } => {
                panic!("expected Created, got Fallback: {reason}");
            }
        }
    }

    #[test]
    fn unique_branch_name_suffixes_on_origin_only_branch() {
        // Pins the detection/minting agreement: the checkout offer fires on an
        // origin-only branch (branch_exists_bare checks origin refs), so the
        // fork must suffix too — a bare local `feat` would silently shadow a
        // divergent `origin/feat`.
        let origin = TempDir::new().unwrap();
        init_git_repo(origin.path());
        let op = origin.path().to_str().unwrap();
        Command::new("git").args(["-C", op, "branch", "feat"]).output().unwrap();
        let work = TempDir::new().unwrap();
        let clone = work.path().join("repo");
        Command::new("git").args(["clone", op, clone.to_str().unwrap()]).output().unwrap();
        let cp = clone.to_str().unwrap();
        // The clone has origin/feat but no local feat.
        assert!(!verify_ref(cp, "refs/heads/feat"));

        assert_eq!(unique_branch_name(cp, "feat"), "feat-2");
        assert_eq!(unique_branch_name(cp, "other"), "other", "non-colliding name stays bare");
    }

    #[test]
    fn from_existing_local_branch_checks_it_out() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        Command::new("git").args(["-C", rp, "branch", "feat"]).output().unwrap();

        match create_worktree_from_branch(rp, "r1", "ws", base.path(), "feat") {
            WorktreeResult::Created { worktree_path, branch_name } => {
                assert!(Path::new(&worktree_path).exists());
                assert_eq!(branch_name, "feat", "existing branch checked out as-is (no fork)");
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
    fn branch_exists_bare_true_for_local_branch() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        Command::new("git").args(["-C", rp, "branch", "feat"]).output().unwrap();
        assert!(branch_exists_bare(rp, "feat"), "local branch is detected");
    }

    #[test]
    fn branch_exists_bare_true_for_origin_only_branch() {
        // An "origin" repo with a branch only present there, cloned so the clone
        // has `origin/feat` but no local `feat` (same harness as the tracking test).
        let origin = TempDir::new().unwrap();
        init_git_repo(origin.path());
        let op = origin.path().to_str().unwrap();
        Command::new("git").args(["-C", op, "branch", "feat"]).output().unwrap();
        let work = TempDir::new().unwrap();
        let clone = work.path().join("repo");
        Command::new("git").args(["clone", op, clone.to_str().unwrap()]).output().unwrap();

        assert!(
            branch_exists_bare(clone.to_str().unwrap(), "feat"),
            "an origin-only branch is detected via refs/remotes/origin/"
        );
    }

    #[test]
    fn branch_exists_bare_false_when_no_branch() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        assert!(!branch_exists_bare(repo.path().to_str().unwrap(), "nope"));
    }

    #[test]
    fn branch_exists_bare_ignores_kommand0_prefixed_branch() {
        // Legacy-coexistence guard: a pre-existing `kommand0/<name>` branch (from
        // versions that prefixed forks) must NOT make the BARE name match — the
        // two names are distinct branches and must not collide in detection.
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        Command::new("git").args(["-C", rp, "branch", "kommand0/feat"]).output().unwrap();
        assert!(!branch_exists_bare(rp, "feat"), "kommand0/<name> is not a bare-name match");
    }

    #[test]
    fn branch_exists_bare_false_for_head() {
        // A clone has `refs/remotes/origin/HEAD` (a symbolic default-branch
        // pointer), but "HEAD" is not a branch name — must not false-match.
        let origin = TempDir::new().unwrap();
        init_git_repo(origin.path());
        let work = TempDir::new().unwrap();
        let clone = work.path().join("repo");
        Command::new("git").args(["clone", origin.path().to_str().unwrap(), clone.to_str().unwrap()]).output().unwrap();
        assert!(!branch_exists_bare(clone.to_str().unwrap(), "HEAD"), "HEAD is not a branch");
    }

    #[test]
    fn branch_exists_bare_ignores_a_tag() {
        // A tag named `<name>` is not a branch; show-ref --verify refs/heads/<name>
        // and refs/remotes/origin/<name> both miss it.
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        Command::new("git").args(["-C", rp, "tag", "rel"]).output().unwrap();
        assert!(!branch_exists_bare(rp, "rel"), "a tag is not a branch");
    }

    #[test]
    fn branch_exists_bare_rejects_revision_syntax() {
        // Pins the show-ref (exact) vs rev-parse (revision-syntax) fix: `main^{commit}`
        // resolves under rev-parse but is not a ref name, so it must be false even
        // though `main` exists.
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        assert!(!branch_exists_bare(rp, "main^{commit}"), "revision syntax must not match");
    }

    #[test]
    fn branch_exists_bare_discriminates_among_branches() {
        // `false` in a repo that HAS other branches proves it checks the exact
        // name, not just "any branch exists".
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let rp = repo.path().to_str().unwrap();
        Command::new("git").args(["-C", rp, "branch", "other"]).output().unwrap();
        assert!(!branch_exists_bare(rp, "feat"), "a different existing branch must not match");
    }

    #[test]
    fn from_missing_branch_is_a_fallback() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        match create_worktree_from_branch(repo.path().to_str().unwrap(), "r1", "ws", base.path(), "nope") {
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

        match create_worktree_from_branch(clone.to_str().unwrap(), "r1", "ws", base.path(), "feat") {
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
    fn copy_worktree_files_swallows_errors_and_keeps_going() {
        // Locks the invariant `finish_worktree_add` relies on: the copy entrypoint
        // it calls returns `()` and never propagates — so a copy failure can't flip
        // a `Created` worktree into a `Fallback`. (A real fault can't be injected
        // through `create_worktree` itself: the worktree is created and copied into
        // atomically, and the shared work-tree makes a dest-type collision
        // impossible, so we lock the contract at the function it invokes.)
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        // Manifest: a doomed pattern (its dest parent is a blocking file) followed
        // by a good one — the good copy must still happen after the failure.
        write_file(root.path(), ".worktree-copy", "sub/x.txt\nok.txt\n");
        write_file(root.path(), "sub/x.txt", "x");
        write_file(root.path(), "ok.txt", "good");
        std::fs::write(dest.path().join("sub"), "blocker").unwrap();

        // Returns `()`, must not panic.
        copy_worktree_files(root.path().to_str().unwrap(), dest.path().to_str().unwrap());

        assert_eq!(std::fs::read_to_string(dest.path().join("sub")).unwrap(), "blocker", "failure skipped");
        assert_eq!(std::fs::read_to_string(dest.path().join("ok.txt")).unwrap(), "good", "later copy still ran");
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
    fn manifest_copies_a_whole_directory_subtree() {
        // A pattern matching a DIRECTORY exercises copy_recursive's dir arm
        // (read_dir recursion); a symlink found *inside* it is skipped.
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        write_file(root.path(), "assets/a.txt", "a");
        write_file(root.path(), "assets/nested/b.txt", "b");
        write_file(root.path(), ".worktree-copy", "assets\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path().join("assets/a.txt"), root.path().join("assets/link.txt")).unwrap();

        copy_worktree_files(root.path().to_str().unwrap(), dest.path().to_str().unwrap());

        assert_eq!(std::fs::read_to_string(dest.path().join("assets/a.txt")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(dest.path().join("assets/nested/b.txt")).unwrap(), "b", "subtree copied recursively");
        #[cfg(unix)]
        assert!(!dest.path().join("assets/link.txt").exists(), "symlink inside a copied dir is skipped");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_source_dir_does_not_escape() {
        // `glob` follows symlinked dirs, so a match can resolve outside the repo
        // with a clean `rel`. The canonical-containment guard must reject it.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "LEAKED").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let dest = TempDir::new().unwrap();

        // A direct cross and a recursive sweep must both copy nothing from outside.
        copy_pattern(&root, dest.path(), "link/*");
        copy_pattern(&root, dest.path(), "**/*");

        assert!(!dest.path().join("link/secret.txt").exists(), "no copy through a symlinked dir");
        assert!(std::fs::read_dir(dest.path()).unwrap().next().is_none(), "nothing escaped into the worktree");
    }

    #[cfg(unix)]
    #[test]
    fn dest_symlink_is_not_followed() {
        // A pre-existing symlinked dir at the dest path must not redirect the
        // write outside the worktree.
        let root = TempDir::new().unwrap();
        write_file(root.path(), "cfg/app.conf", "x");
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("wt");
        std::fs::create_dir(&dest).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, dest.join("cfg")).unwrap();

        copy_pattern(root.path(), &dest, "cfg/app.conf");

        assert!(!outside.join("app.conf").exists(), "did not write through the dest symlink");
    }

    #[test]
    fn matching_is_case_sensitive() {
        // Unlike glob's case-insensitive Default, wildcard matching is
        // case-sensitive (zsh-like). A LITERAL pattern can't test this — a
        // case-insensitive filesystem resolves it regardless — so use a wildcard.
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        write_file(root.path(), "config.txt", "c");

        // Wrong-case extension must not match (would copy under glob's Default).
        copy_pattern(root.path(), dest.path(), "*.TXT");
        assert!(!dest.path().join("config.txt").exists(), "wrong-case wildcard must not match");

        // Sanity: the correct case still matches, so we didn't just break globbing.
        copy_pattern(root.path(), dest.path(), "*.txt");
        assert_eq!(std::fs::read_to_string(dest.path().join("config.txt")).unwrap(), "c");
    }

    #[test]
    fn bad_glob_pattern_is_skipped() {
        // An invalid glob (unclosed `[`) must not panic and must copy nothing.
        let root = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        write_file(root.path(), "a.txt", "a");

        copy_pattern(root.path(), dest.path(), "[");

        assert!(std::fs::read_dir(dest.path()).unwrap().next().is_none());
    }

    #[test]
    fn create_worktree_copies_env_into_worktree() {
        let repo = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        init_git_repo(repo.path());
        // No manifest -> fallback copies `.env*` into the new worktree.
        write_file(repo.path(), ".env", "API=1");

        match create_worktree(repo.path().to_str().unwrap(), "r1", "ws", base.path()) {
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

        let worktree_dir = base.path().join("worktrees").join("r1").join("ws");
        match create_worktree(repo.path().to_str().unwrap(), "r1", "ws", base.path()) {
            WorktreeResult::Fallback { .. } => {
                assert!(!worktree_dir.exists(), "no worktree created, nothing copied");
            }
            WorktreeResult::Created { .. } => panic!("expected Fallback for a non-git dir"),
        }
    }
}
