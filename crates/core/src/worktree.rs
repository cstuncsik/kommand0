use std::path::Path;
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
    finish_worktree_add(output, worktree_path, branch)
}

/// Map the `git worktree add` result to a [`WorktreeResult`].
fn finish_worktree_add(
    output: std::io::Result<std::process::Output>,
    worktree_path: String,
    branch_name: String,
) -> WorktreeResult {
    match output {
        Ok(result) if result.status.success() => {
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
    finish_worktree_add(output, worktree_path, branch_name)
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
}
