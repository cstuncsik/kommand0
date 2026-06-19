//! Read-only git status plumbing for workspaces.
//!
//! [`branch_status`] reports a worktree's current branch, how far it is
//! ahead/behind its upstream, and whether it has uncommitted changes — the
//! data the TUI surfaces per workspace. It is deliberately panic-free and
//! returns `None` (rather than erroring) when the directory isn't a git repo,
//! so the caller can run it across every workspace without special-casing.

use std::process::Command;

/// A worktree's git state at a point in time.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BranchStatus {
    /// Current branch name, or `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Commits ahead of the upstream (0 when no upstream or in sync).
    pub ahead: u32,
    /// Commits behind the upstream (0 when no upstream or in sync).
    pub behind: u32,
    /// Any uncommitted change (staged, unstaged, or untracked).
    pub dirty: bool,
    /// Whether the branch has a configured upstream at all.
    pub has_upstream: bool,
}

/// Read `working_dir`'s git status via `git status --porcelain=v2 --branch`.
///
/// Returns `None` if the directory isn't a git repo or git fails to run. The
/// porcelain v2 format is stable and locale-independent; this parser tolerates
/// missing fields (a degraded field, never a panic).
pub fn branch_status(working_dir: &str) -> Option<BranchStatus> {
    let out = Command::new("git")
        .args(["-C", working_dir, "status", "--porcelain=v2", "--branch"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut s = BranchStatus::default();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            // `(detached)` is the only sentinel on branch.head; an unborn branch
            // still reports its real name here (the `(initial)` marker is on
            // branch.oid, which we ignore).
            s.branch = (head != "(detached)").then(|| head.to_string());
        } else if line.starts_with("# branch.upstream ") {
            // The sole source of truth for "has an upstream" — branch.ab can be
            // absent even with an upstream (e.g. the remote ref was pruned).
            s.has_upstream = true;
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            // Format: "+<ahead> -<behind>".
            for tok in ab.split_whitespace() {
                if let Some(a) = tok.strip_prefix('+') {
                    s.ahead = a.parse().unwrap_or(0);
                } else if let Some(b) = tok.strip_prefix('-') {
                    s.behind = b.parse().unwrap_or(0);
                }
            }
        } else if matches!(line.as_bytes().first(), Some(b'1' | b'2' | b'u' | b'?')) {
            // Content lines: 1=changed, 2=renamed/copied, u=unmerged,
            // ?=untracked. (Ignored `!` lines only appear with --ignored.)
            s.dirty = true;
        }
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    /// Init a git repo with a deterministic default branch + commit identity, so
    /// commits succeed on a machine with no global git config.
    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "init"]);
    }

    #[test]
    fn clean_repo_reports_branch_and_no_changes() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let s = branch_status(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert!(!s.dirty);
        assert!(!s.has_upstream);
        assert_eq!((s.ahead, s.behind), (0, 0));
    }

    #[test]
    fn untracked_and_modified_files_are_dirty() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("new.txt"), "x").unwrap();
        let s = branch_status(tmp.path().to_str().unwrap()).unwrap();
        assert!(s.dirty, "untracked file is dirty");

        std::fs::write(tmp.path().join("a.txt"), "changed").unwrap();
        let s = branch_status(tmp.path().to_str().unwrap()).unwrap();
        assert!(s.dirty, "modified tracked file is dirty");
    }

    #[test]
    fn not_a_git_repo_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(branch_status(tmp.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn detached_head_has_no_branch() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        // Detach onto the current commit.
        git(tmp.path(), &["checkout", "--detach", "HEAD"]);
        let s = branch_status(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(s.branch, None);
    }

    #[test]
    fn ahead_of_upstream_is_reported() {
        // A bare "remote" + a clone that commits one extra commit.
        let remote = TempDir::new().unwrap();
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let work = TempDir::new().unwrap();
        let wp = work.path().join("clone");
        git(
            work.path(),
            &["clone", remote.path().to_str().unwrap(), wp.to_str().unwrap()],
        );
        git(&wp, &["config", "user.email", "t@t"]);
        git(&wp, &["config", "user.name", "t"]);
        std::fs::write(wp.join("a.txt"), "1").unwrap();
        git(&wp, &["add", "."]);
        git(&wp, &["commit", "-m", "c1"]);
        git(&wp, &["push", "-u", "origin", "main"]);
        // One more local commit -> ahead by 1.
        std::fs::write(wp.join("b.txt"), "2").unwrap();
        git(&wp, &["add", "."]);
        git(&wp, &["commit", "-m", "c2"]);

        let s = branch_status(wp.to_str().unwrap()).unwrap();
        assert!(s.has_upstream, "tracking branch has an upstream");
        assert_eq!(s.ahead, 1, "one unpushed commit");
        assert_eq!(s.behind, 0);
    }
}
