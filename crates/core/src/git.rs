//! Git plumbing for workspaces.
//!
//! [`branch_status`] reports a worktree's current branch, how far it is
//! ahead/behind its upstream, and whether it has uncommitted changes — the
//! data the TUI surfaces per workspace. It is deliberately panic-free and
//! returns `None` (rather than erroring) when the directory isn't a git repo,
//! so the caller can run it across every workspace without special-casing.
//!
//! [`open_pull_request`] pushes a workspace's branch and opens a GitHub PR via
//! the `gh` CLI. Both run synchronously and are meant to be called off the UI
//! thread (PR creation is a network call).

use std::process::{Command, Stdio};

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

/// The `gh` binary to invoke (overridable via `KOMMAND0_GH_BIN`, mirroring the
/// `KOMMAND0_CLAUDE_BIN` override used for the embedded pane).
fn gh_bin() -> String {
    std::env::var("KOMMAND0_GH_BIN").unwrap_or_else(|_| "gh".to_string())
}

/// Push a workspace's branch and open a GitHub PR for it, returning the PR URL.
///
/// On error returns a user-facing message. This shells out to `git push` and
/// the `gh` CLI (a network call), so callers should run it off the UI thread.
pub fn open_pull_request(worktree_path: &str, branch: &str) -> Result<String, String> {
    open_pull_request_with(worktree_path, branch, &gh_bin())
}

/// Last non-empty, trimmed line of some command output (gh prints the PR URL as
/// the last stdout line; this also yields a one-line error from stderr).
fn last_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Run `gh <args>` in `cwd`, non-interactively (no prompts, no tty read, no
/// pager, no update notifier) so it can never hang a background thread.
fn run_gh(gh_bin: &str, cwd: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new(gh_bin)
        .args(args)
        .current_dir(cwd)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_PAGER", "cat")
        .stdin(Stdio::null())
        .output()
}

/// [`open_pull_request`] with the `gh` binary injected, so tests can pass a stub
/// path instead of mutating the process environment.
fn open_pull_request_with(worktree_path: &str, branch: &str, gh_bin: &str) -> Result<String, String> {
    // Friendly pre-check: a missing `origin` otherwise yields a confusing
    // "src refspec ... does not match any" from push.
    let has_origin = Command::new("git")
        .args(["-C", worktree_path, "remote", "get-url", "origin"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_origin {
        return Err("no 'origin' remote configured for this repository".to_string());
    }

    // Push the branch with upstream tracking (never force — a diverged branch
    // should error, not clobber the remote).
    match Command::new("git")
        .args(["-C", worktree_path, "push", "-u", "origin", branch])
        .output()
    {
        Ok(o) if o.status.success() => {}
        Ok(o) => return Err(format!("git push failed: {}", last_line(&o.stderr))),
        Err(e) => return Err(format!("git push failed: {e}")),
    }

    // Create the PR: --head pins the branch (no HEAD-inference prompt), --fill
    // takes title/body from the commits, and the base is left to gh's
    // default-branch detection.
    match run_gh(gh_bin, worktree_path, &["pr", "create", "--fill", "--head", branch]) {
        Ok(out) if out.status.success() => Ok(last_line(&out.stdout)),
        Ok(out) => {
            // A PR may already exist — recover its URL so the action is idempotent.
            if let Ok(view) = run_gh(
                gh_bin,
                worktree_path,
                &["pr", "view", "--head", branch, "--json", "url", "-q", ".url"],
            ) && view.status.success()
            {
                let url = last_line(&view.stdout);
                if !url.is_empty() {
                    return Ok(url);
                }
            }
            Err(format!("gh: {}", last_line(&out.stderr)))
        }
        Err(_) => Err("gh CLI not found — install GitHub CLI to open PRs".to_string()),
    }
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

    /// A work repo on `feature` with a real bare `origin`, so `git push -u` in
    /// `open_pull_request_with` actually succeeds. Returns the work-repo path.
    fn repo_with_remote(root: &Path) -> std::path::PathBuf {
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare", "-b", "main"]);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "t@t"]);
        git(&work, &["config", "user.name", "t"]);
        std::fs::write(work.join("a.txt"), "1").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "c1"]);
        git(&work, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&work, &["checkout", "-b", "feature"]);
        std::fs::write(work.join("b.txt"), "2").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "c2"]);
        work
    }

    /// Write an executable shell stub at `path` with the given body.
    fn write_stub(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn open_pull_request_returns_the_url_on_success() {
        let tmp = TempDir::new().unwrap();
        let work = repo_with_remote(tmp.path());
        let stub = tmp.path().join("gh");
        write_stub(
            &stub,
            "#!/bin/sh\nif [ \"$1\" = pr ] && [ \"$2\" = create ]; then\n  echo https://github.com/x/y/pull/1\n  exit 0\nfi\nexit 1\n",
        );
        let url = open_pull_request_with(work.to_str().unwrap(), "feature", stub.to_str().unwrap());
        assert_eq!(url, Ok("https://github.com/x/y/pull/1".to_string()));
    }

    #[test]
    fn open_pull_request_recovers_existing_pr_url() {
        let tmp = TempDir::new().unwrap();
        let work = repo_with_remote(tmp.path());
        let stub = tmp.path().join("gh");
        // `create` fails ("already exists"); `view` returns the existing URL.
        write_stub(
            &stub,
            "#!/bin/sh\nif [ \"$1\" = pr ] && [ \"$2\" = create ]; then\n  echo 'a pull request already exists' >&2\n  exit 1\nfi\nif [ \"$1\" = pr ] && [ \"$2\" = view ]; then\n  echo https://github.com/x/y/pull/2\n  exit 0\nfi\nexit 1\n",
        );
        let url = open_pull_request_with(work.to_str().unwrap(), "feature", stub.to_str().unwrap());
        assert_eq!(url, Ok("https://github.com/x/y/pull/2".to_string()));
    }

    #[test]
    fn open_pull_request_errors_without_origin() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path()); // a repo, but no `origin` remote
        let err = open_pull_request_with(tmp.path().to_str().unwrap(), "feature", "gh-not-used")
            .unwrap_err();
        assert!(err.contains("origin"), "friendly no-remote error: {err}");
    }

    #[test]
    fn open_pull_request_reports_missing_gh() {
        let tmp = TempDir::new().unwrap();
        let work = repo_with_remote(tmp.path());
        let err = open_pull_request_with(
            work.to_str().unwrap(),
            "feature",
            "/nonexistent/definitely/not/gh",
        )
        .unwrap_err();
        assert!(err.contains("gh CLI not found"), "friendly gh-missing error: {err}");
    }
}
