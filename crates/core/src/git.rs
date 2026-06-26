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
    // Retry on ETXTBSY ("text file busy"): exec'ing a binary that was just
    // written can transiently fail when another thread's concurrent fork+exec
    // still holds a write fd to it. This is a real race under parallel tests
    // (which exec freshly-written `gh` stubs) on Linux, and possible in the wild
    // right after a `gh` upgrade. It's transient — back off briefly and retry,
    // rather than surfacing it as a bogus "gh not found".
    let mut attempt = 0u32;
    loop {
        let result = Command::new(gh_bin)
            .args(args)
            .current_dir(cwd)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_PAGER", "cat")
            .stdin(Stdio::null())
            .output();
        match result {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 8 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(10 * attempt as u64));
            }
            other => return other,
        }
    }
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
        Ok(out) if out.status.success() && !last_line(&out.stdout).is_empty() => {
            Ok(last_line(&out.stdout))
        }
        Ok(out) => {
            // Either create failed (a PR may already exist) or it exited 0 without
            // a URL on stdout — recover the existing PR's URL so the action is
            // idempotent. `gh pr view` takes the branch as a positional arg.
            if let Ok(view) = run_gh(
                gh_bin,
                worktree_path,
                &["pr", "view", branch, "--json", "url", "-q", ".url"],
            ) && view.status.success()
            {
                let url = last_line(&view.stdout);
                if !url.is_empty() {
                    return Ok(url);
                }
            }
            let msg = last_line(&out.stderr);
            Err(format!(
                "gh: {}",
                if msg.is_empty() {
                    "PR created but no URL was returned".to_string()
                } else {
                    msg
                }
            ))
        }
        Err(_) => Err("gh CLI not found — install GitHub CLI to open PRs".to_string()),
    }
}

/// Remove a merged workspace's worktree and delete its branch — but only when it
/// is provably safe. Returns a message (deleting nothing) unless ALL hold:
/// - the branch is workspace-owned (`kommand0/…`) — never the default branch;
/// - its PR is `MERGED` (per `gh`);
/// - the worktree is clean (no uncommitted/untracked changes; an unreadable
///   status aborts rather than assuming clean); and
/// - the branch tip equals the last commit the PR merged (so there are no
///   commits beyond the PR — squash-safe, and catches pushed or unpushed extras).
///
/// The worktree is removed WITHOUT `--force` (a last-moment dirty state still
/// fails safe), and only then is the branch deleted.
pub fn cleanup_merged_workspace(
    repo_path: &str,
    worktree_path: &str,
    branch: &str,
) -> Result<(), String> {
    cleanup_merged_workspace_with(repo_path, worktree_path, branch, &gh_bin())
}

fn cleanup_merged_workspace_with(
    repo_path: &str,
    worktree_path: &str,
    branch: &str,
    gh_bin: &str,
) -> Result<(), String> {
    // Never delete a branch kommand0 didn't create — the `kommand0/` prefix
    // blocks `main`/`master`/any default branch; the `..` reject is explicit
    // defense against a traversal-y ref name (git would reject it anyway).
    if branch.is_empty() || !branch.starts_with("kommand0/") || branch.contains("..") {
        return Err("refusing to delete a branch kommand0 didn't create".to_string());
    }

    // The PR must be merged; capture the oid of the last commit it merged. Run gh
    // from the repo (not the worktree) so a partial-cleanup retry — worktree
    // already gone — still works instead of failing with a bogus "gh not found".
    let out = match run_gh(
        gh_bin,
        repo_path,
        &[
            "pr",
            "view",
            branch,
            "--json",
            "state,commits",
            "-q",
            ".state, (.commits[-1].oid // \"\")",
        ],
    ) {
        Ok(o) if o.status.success() => o,
        Ok(o) => return Err(format!("no merged PR found for this branch ({})", last_line(&o.stderr))),
        Err(_) => return Err("gh CLI not found — install GitHub CLI to clean up".to_string()),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let state = lines.next().unwrap_or("").trim();
    let pr_tip = lines.next().unwrap_or("").trim().to_string();
    if state != "MERGED" {
        return Err(format!(
            "the PR for this branch isn't merged (state: {state}) — not cleaning up"
        ));
    }

    // The worktree must be clean — but only check if it still exists (a retry
    // after a partial cleanup has no worktree left to be dirty). An unreadable
    // status on an existing worktree aborts (never assume safe).
    let worktree_exists = std::path::Path::new(worktree_path).exists();
    if worktree_exists {
        let st = branch_status(worktree_path)
            .ok_or_else(|| "couldn't read the worktree's git status — not cleaning up".to_string())?;
        if st.dirty {
            return Err(
                "the worktree has uncommitted changes — commit or discard them first".to_string(),
            );
        }
    }

    // The branch tip must be exactly what the PR merged: no commits beyond it
    // (pushed OR unpushed). This is the guard that makes the force-delete safe.
    let local_tip = match Command::new("git")
        .args(["-C", repo_path, "rev-parse", &format!("refs/heads/{branch}")])
        .output()
    {
        Ok(o) if o.status.success() => last_line(&o.stdout),
        _ => return Err("couldn't resolve the branch tip — not cleaning up".to_string()),
    };
    if pr_tip.is_empty() || local_tip != pr_tip {
        return Err("the branch has commits beyond its merged PR — not cleaning up".to_string());
    }

    // Remove the worktree (no --force, so a last-moment dirty state still fails
    // safe); if it's still there after the attempt, the remove failed — abort.
    if worktree_exists {
        let _ = Command::new("git")
            .args(["-C", repo_path, "worktree", "remove", worktree_path])
            .output();
        if std::path::Path::new(worktree_path).exists() {
            return Err(
                "couldn't remove the worktree (it may have changes) — not cleaning up".to_string(),
            );
        }
    }
    // Clean up any stale worktree admin entry (whether we removed it or it was
    // already gone) so the branch is deletable.
    let _ = Command::new("git")
        .args(["-C", repo_path, "worktree", "prune"])
        .output();

    // Delete the local branch (force: a squash-merge leaves it "unmerged" locally,
    // but the PR-tip check above proved there's nothing beyond the merge).
    match Command::new("git")
        .args(["-C", repo_path, "branch", "-D", branch])
        .output()
    {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "worktree removed, but couldn't delete branch {branch}: {}",
            last_line(&o.stderr)
        )),
        Err(e) => Err(format!("worktree removed, but couldn't delete branch {branch}: {e}")),
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
        // `gh pr view` takes the branch as a positional arg — reject the bogus
        // `--head` flag so a regression to it would fail this test.
        write_stub(
            &stub,
            "#!/bin/sh\nif [ \"$1\" = pr ] && [ \"$2\" = create ]; then\n  echo 'a pull request already exists' >&2\n  exit 1\nfi\nif [ \"$1\" = pr ] && [ \"$2\" = view ]; then\n  if [ \"$3\" = --head ]; then echo 'unknown flag: --head' >&2; exit 1; fi\n  echo https://github.com/x/y/pull/2\n  exit 0\nfi\nexit 1\n",
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

    // --- cleanup_merged_workspace ---

    /// A repo with a linked worktree on `kommand0/feat`. Returns
    /// `(repo_path, worktree_path, branch, branch_tip_sha)`.
    fn repo_with_worktree(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, String, String) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "1").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);
        let wt = root.join("wt");
        git(
            &repo,
            &["worktree", "add", wt.to_str().unwrap(), "-b", "kommand0/feat"],
        );
        let tip = Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "rev-parse", "refs/heads/kommand0/feat"])
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&tip.stdout).trim().to_string();
        (repo, wt, "kommand0/feat".to_string(), sha)
    }

    /// A `gh` stub whose `pr view` prints `<state>\n<oid>`.
    fn gh_view_stub(path: &Path, state: &str, oid: &str) {
        write_stub(
            path,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = pr ] && [ \"$2\" = view ]; then\n  printf '{state}\\n{oid}\\n'\n  exit 0\nfi\nexit 1\n"
            ),
        );
    }

    fn cleanup(repo: &Path, wt: &Path, branch: &str, gh: &Path) -> Result<(), String> {
        cleanup_merged_workspace_with(
            repo.to_str().unwrap(),
            wt.to_str().unwrap(),
            branch,
            gh.to_str().unwrap(),
        )
    }

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "rev-parse", "--verify", branch])
            .output()
            .unwrap()
            .status
            .success()
    }

    #[test]
    fn cleanup_merged_clean_removes_worktree_and_branch() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        let gh = tmp.path().join("gh");
        gh_view_stub(&gh, "MERGED", &sha);
        assert_eq!(cleanup(&repo, &wt, &branch, &gh), Ok(()));
        assert!(!wt.exists(), "worktree dir removed");
        assert!(!branch_exists(&repo, &branch), "branch deleted");
    }

    #[test]
    fn cleanup_refuses_when_pr_open_and_destroys_nothing() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        let gh = tmp.path().join("gh");
        gh_view_stub(&gh, "OPEN", &sha);
        let err = cleanup(&repo, &wt, &branch, &gh).unwrap_err();
        assert!(err.contains("isn't merged"), "expected 'isn't merged', got: {err}");
        assert!(wt.exists(), "worktree untouched");
        assert!(branch_exists(&repo, &branch), "branch untouched");
    }

    #[test]
    fn cleanup_refuses_when_no_pr() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, _) = repo_with_worktree(tmp.path());
        let gh = tmp.path().join("gh");
        write_stub(&gh, "#!/bin/sh\nexit 1\n"); // gh finds no PR
        assert!(cleanup(&repo, &wt, &branch, &gh).is_err());
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_refuses_dirty_worktree() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        std::fs::write(wt.join("scratch.txt"), "wip").unwrap(); // untracked => dirty
        let gh = tmp.path().join("gh");
        gh_view_stub(&gh, "MERGED", &sha);
        let err = cleanup(&repo, &wt, &branch, &gh).unwrap_err();
        assert!(err.contains("uncommitted"), "expected 'uncommitted', got: {err}");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_refuses_commits_beyond_the_merged_pr() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        // The PR's last merged commit is the original tip; the local branch has
        // since advanced (a commit beyond the PR).
        std::fs::write(wt.join("more.txt"), "x").unwrap();
        git(&wt, &["config", "user.email", "t@t"]);
        git(&wt, &["config", "user.name", "t"]);
        git(&wt, &["add", "."]);
        git(&wt, &["commit", "-m", "beyond"]);
        let gh = tmp.path().join("gh");
        gh_view_stub(&gh, "MERGED", &sha); // stale (pre-extra-commit) oid
        let err = cleanup(&repo, &wt, &branch, &gh).unwrap_err();
        assert!(err.contains("beyond its merged PR"), "expected 'beyond its merged PR', got: {err}");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_refuses_a_non_workspace_branch() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, _, _) = repo_with_worktree(tmp.path());
        // Even with a "MERGED" gh, a non-kommand0 branch (e.g. main) is refused
        // before any gh/git deletion.
        let err = cleanup(&repo, &wt, "main", &tmp.path().join("gh")).unwrap_err();
        assert!(err.contains("kommand0 didn't create"));
        assert!(branch_exists(&repo, "main"), "main untouched");
    }

    #[test]
    fn cleanup_refuses_when_pr_has_no_commits() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, _) = repo_with_worktree(tmp.path());
        let gh = tmp.path().join("gh");
        gh_view_stub(&gh, "MERGED", ""); // empty oid (no commits)
        assert!(cleanup(&repo, &wt, &branch, &gh).unwrap_err().contains("beyond its merged PR"));
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_completes_when_worktree_dir_already_gone() {
        // A retry after a partial cleanup (worktree removed, branch left) must
        // still delete the orphaned branch — gh runs from the repo, the missing
        // worktree skips the dirty check, and `worktree prune` clears the entry.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        std::fs::remove_dir_all(&wt).unwrap(); // worktree dir vanished
        let gh = tmp.path().join("gh");
        gh_view_stub(&gh, "MERGED", &sha);
        assert_eq!(cleanup(&repo, &wt, &branch, &gh), Ok(()));
        assert!(!branch_exists(&repo, &branch), "orphaned branch deleted");
    }
}
