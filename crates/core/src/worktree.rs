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

/// Find a unique branch name by appending -2, -3, etc.
fn unique_branch_name(repo_path: &str, base: &str) -> String {
    let candidate = format!("kommand0/{}", base);
    if !branch_exists(repo_path, &candidate) {
        return candidate;
    }
    for i in 2..100 {
        let name = format!("kommand0/{}-{}", base, i);
        if !branch_exists(repo_path, &name) {
            return name;
        }
    }
    // Fallback with timestamp
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("kommand0/{}-{}", base, ts)
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
            reason: format!("{} is not a git repository", repo_path),
        };
    }

    let worktree_dir = base_dir.join("worktrees").join(workspace_name);
    // Canonicalize base_dir to ensure absolute path (worktree dir doesn't exist yet,
    // so canonicalize the parent and re-append)
    let worktree_dir = if worktree_dir.is_relative() {
        std::env::current_dir()
            .unwrap_or_default()
            .join(&worktree_dir)
    } else {
        worktree_dir
    };
    let worktree_path = worktree_dir.to_string_lossy().to_string();

    // If worktree path already exists, try to clean it up
    if worktree_dir.exists() {
        // Try to remove stale worktree
        let _ = Command::new("git")
            .args(["-C", repo_path, "worktree", "remove", &worktree_path, "--force"])
            .output();
        // If directory still exists after removal attempt, bail
        if worktree_dir.exists() {
            return WorktreeResult::Fallback {
                reason: format!("worktree path already exists: {}", worktree_path),
            };
        }
    }

    // Find a unique branch name
    let branch = unique_branch_name(repo_path, workspace_name);

    // Create the worktree
    let output = Command::new("git")
        .args([
            "-C",
            repo_path,
            "worktree",
            "add",
            &worktree_path,
            "-b",
            &branch,
        ])
        .output();

    match output {
        Ok(result) if result.status.success() => WorktreeResult::Created {
            worktree_path,
            branch_name: branch,
        },
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            WorktreeResult::Fallback {
                reason: format!("git worktree add failed: {}", stderr.trim()),
            }
        }
        Err(e) => WorktreeResult::Fallback {
            reason: format!("failed to run git: {}", e),
        },
    }
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
            // Log but don't fail — worktree removal shouldn't block workspace deletion
            eprintln!(
                "warning: git worktree remove failed for {}: {}",
                worktree_path,
                stderr.trim()
            );
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "warning: failed to run git worktree remove for {}: {}",
                worktree_path, e
            );
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
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
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
                panic!("expected Created, got Fallback: {}", reason);
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
                    "expected suffixed branch, got: {}",
                    branch_name
                );
            }
            WorktreeResult::Fallback { reason } => {
                panic!("expected Created, got Fallback: {}", reason);
            }
        }
    }
}
