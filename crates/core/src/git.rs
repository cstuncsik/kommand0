//! Git plumbing for workspaces.
//!
//! [`branch_status`] reports a worktree's current branch, how far it is
//! ahead/behind its upstream, and whether it has uncommitted changes — the
//! data the TUI surfaces per workspace. It is deliberately panic-free and
//! returns `None` (rather than erroring) when the directory isn't a git repo,
//! so the caller can run it across every workspace without special-casing.
//!
//! [`cleanup_merged_workspace`] removes a merged workspace's worktree and branch
//! via the `gh` CLI. It runs synchronously and is meant to be called off the UI
//! thread (it makes a network call).

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

/// Resolve the ref to diff a branch against: the repository's default branch.
/// Prefer the remote's advertised default (`origin/HEAD`), then the common
/// remote/local names, returning the first ref that actually exists. `None`
/// when none resolve (e.g. not a git repo).
fn default_branch_ref(working_dir: &str) -> Option<String> {
    // Fully-qualified refs (`refs/remotes/…`, `refs/heads/…`) so a tag or local
    // branch named e.g. `origin/main` can't shadow the intended ref — gitrevisions
    // ranks `refs/tags/*` above `refs/remotes/*` for a bare name. `^{commit}`
    // dereferences the symbolic `origin/HEAD` and rejects non-commit refs.
    for cand in [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
        "refs/heads/main",
        "refs/heads/master",
    ] {
        let exists = Command::new("git")
            .args([
                "-C",
                working_dir,
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{cand}^{{commit}}"),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if exists {
            return Some(cand.to_string());
        }
    }
    None
}

/// One file's section of a PR-style diff (see [`diff_files_vs_default_branch`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// The new path (the `b/` side; a delete uses its `a/` side — see
    /// [`file_diff_path`]).
    pub path: String,
    /// That file's diff section, verbatim (from its `diff --git` line to the next).
    pub text: String,
}

/// The `git diff <default>...HEAD` for a worktree, split per file — the "PR-style"
/// diff of every change the current branch has committed since it diverged from
/// the default branch (the `A...B` form excludes the working tree, matching what a
/// PR shows).
///
/// `Some(vec![])` means no difference (HEAD is the default branch, or nothing is
/// committed ahead of it). `None` means the directory isn't a git repo or the
/// default branch couldn't be resolved. Panic-free, meant to run off the UI
/// thread like [`branch_status`].
pub fn diff_files_vs_default_branch(working_dir: &str) -> Option<Vec<FileDiff>> {
    let base = default_branch_ref(working_dir)?;
    // `--no-ext-diff` avoids slow user-configured external diff drivers on the UI
    // thread; `--no-color` keeps ANSI codes out of the captured text regardless of
    // the user's `color.diff` config (the overlay does its own colouring).
    let out = Command::new("git")
        .args([
            "-C",
            working_dir,
            "diff",
            "--no-ext-diff",
            "--no-color",
            &format!("{base}...HEAD"),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut files = split_file_diffs(&text);
    // Bound the retained/rendered diff so a pathological worktree can't pin an
    // unbounded string in memory (the overlay separately caps rendered lines).
    // Cap on whole sections — never a truncated `diff --git` fragment — by
    // dropping the sections that overflow the budget, keeping the total ~1 MiB.
    const MAX_BYTES: usize = 1 << 20; // 1 MiB
    let mut used = 0usize;
    // Always keep the first section, even if it alone exceeds the budget — a
    // single huge file should still render (line-capped) rather than vanish.
    let keep = files
        .iter()
        .take_while(|f| {
            let first = used == 0;
            used += f.text.len();
            first || used <= MAX_BYTES
        })
        .count();
    if keep < files.len() {
        files.truncate(keep);
        // Note the drop on the last kept section (not a phantom file row) so the
        // overlay surfaces it without inventing an empty-path entry.
        if let Some(last) = files.last_mut() {
            last.text
                .push_str("\n… diff truncated (over 1 MiB) — use a shell tab for the full diff\n");
        }
    }
    Some(files)
}

/// Split a raw multi-file diff into per-file [`FileDiff`] sections on
/// `"diff --git "` boundaries, deriving each section's path with
/// [`file_diff_path`]. Anything before the first header (usually nothing) is
/// dropped.
fn split_file_diffs(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for raw in text.split_inclusive('\n') {
        if raw.starts_with("diff --git ") {
            // Path is filled in once the whole section is collected (from its
            // `+++ b/` line — see file_diff_path); the header alone can't be
            // split reliably when the path contains a space.
            files.push(FileDiff { path: String::new(), text: raw.to_string() });
        } else if let Some(cur) = files.last_mut() {
            cur.text.push_str(raw);
        }
    }
    for f in &mut files {
        f.path = file_diff_path(&f.text);
    }
    files
}

/// The new-side path of one `diff --git` section. Prefers the unambiguous
/// `+++ b/<path>` line (a single token from `+++ ` to end-of-line, so a path
/// with spaces is fine); for a delete (`+++ /dev/null`) uses the `--- a/<path>`
/// line instead. Falls back to the `diff --git … b/…` split (then the whole
/// header line) only when neither `+++`/`---` line is present — a git-quoted or
/// special-char path stays on that fallback, which is acceptable. Never panics.
fn file_diff_path(section: &str) -> String {
    let mut plus: Option<&str> = None;
    let mut minus: Option<&str> = None;
    for line in section.lines() {
        // Git appends a literal tab to a `+++`/`---` path that contains spaces
        // (its own ambiguity guard); trim it off the token.
        if let Some(rest) = line.strip_prefix("+++ ") {
            plus = Some(rest.trim_end());
        } else if let Some(rest) = line.strip_prefix("--- ") {
            minus = Some(rest.trim_end());
        }
        if line.starts_with("@@") {
            break; // headers are done once the first hunk starts
        }
    }
    // A present `+++` that isn't /dev/null gives the new path; a delete's
    // /dev/null `+++` defers to the `---` (old) path.
    if let Some(p) = plus
        && p != "/dev/null"
    {
        return p.strip_prefix("b/").unwrap_or(p).to_string();
    }
    if let Some(m) = minus
        && m != "/dev/null"
    {
        return m.strip_prefix("a/").unwrap_or(m).to_string();
    }
    // No usable +++/--- (mode-only change, binary with none): fall back to the
    // header's " b/" split, then the whole header line. Never panics.
    let header = section.lines().next().unwrap_or("");
    header
        .split_once(" b/")
        .map(|(_, b)| b.trim_end().to_string())
        .unwrap_or_else(|| header.trim_end().to_string())
}

/// The `gh` binary to invoke (overridable via `KOMMAND0_GH_BIN`, mirroring the
/// `KOMMAND0_CLAUDE_BIN` override used for the embedded pane).
fn gh_bin() -> String {
    std::env::var("KOMMAND0_GH_BIN").unwrap_or_else(|_| "gh".to_string())
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
/// pager, no update notifier). Bounded by a wall-clock timeout because `gh` is a
/// network call: a caller off the UI thread guards a latch on this returning, and
/// gh wedged on the network (proxy black-hole, hung TLS) must not pin it forever.
fn run_gh(gh_bin: &str, cwd: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    // Generous — a slow GraphQL query is fine; this only trips on a true hang.
    const GH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    // Retry on ETXTBSY ("text file busy"): exec'ing a binary that was just
    // written can transiently fail when another thread's concurrent fork+exec
    // still holds a write fd to it. This is a real race under parallel tests
    // (which exec freshly-written `gh` stubs) on Linux, and possible in the wild
    // right after a `gh` upgrade. It's transient — back off briefly and retry,
    // rather than surfacing it as a bogus "gh not found".
    let mut attempt = 0u32;
    loop {
        let spawned = Command::new(gh_bin)
            .args(args)
            .current_dir(cwd)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_PAGER", "cat")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let child = match spawned {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 8 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(10 * attempt as u64));
                continue;
            }
            Err(e) => return Err(e),
            Ok(child) => child,
        };
        // Collect output on a helper thread so the pipe can't deadlock; if it
        // outruns the deadline we give up (gh has its own HTTP timeouts, and the
        // OS reaps the abandoned child on exit) rather than block indefinitely.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        return match rx.recv_timeout(GH_TIMEOUT) {
            Ok(out) => out,
            Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "gh timed out")),
        };
    }
}

/// A pull request's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// Aggregate CI outcome across a PR's `statusCheckRollup` (see [`pr_statuses`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrChecks {
    Passing,
    Failing,
    Pending,
    None,
}

/// A PR's review decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrReview {
    Approved,
    ChangesRequested,
    ReviewRequired,
    None,
}

/// One pull request's status, as surfaced in the tree/detail panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatus {
    pub number: u64,
    pub state: PrState,
    pub checks: PrChecks,
    pub review: PrReview,
    pub url: String,
}

/// One `gh pr list` per repo → a map of `headRefName` → [`PrStatus`]. Panic-free:
/// returns an empty map on any gh failure (not installed, not authenticated, not
/// a gh-recognised repo) or parse error. Shells out to `gh` (a network call), so
/// callers should run it off the UI thread — like [`branch_status`].
pub fn pr_statuses(repo_dir: &str) -> std::collections::HashMap<String, PrStatus> {
    pr_statuses_with(repo_dir, &gh_bin())
}

/// Classify a single `statusCheckRollup` item as (is_failure, is_pending). A
/// CheckRun carries `conclusion` (null until it completes) + `status`; a
/// StatusContext carries `state`.
fn classify_check(item: &serde_json::Value) -> (bool, bool) {
    let conclusion = item.get("conclusion").and_then(|v| v.as_str());
    let status = item.get("status").and_then(|v| v.as_str());
    let state = item.get("state").and_then(|v| v.as_str());

    let is_failure = matches!(
        conclusion,
        Some("FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE")
    ) || matches!(state, Some("FAILURE" | "ERROR"));
    if is_failure {
        return (true, false);
    }

    // Still running: a CheckRun with no conclusion and no terminal state, one not
    // COMPLETED, or a StatusContext explicitly pending/expected.
    let is_pending = (conclusion.is_none() && state.is_none())
        || matches!(status, Some(s) if s != "COMPLETED")
        || matches!(state, Some("PENDING" | "EXPECTED"));
    (false, is_pending)
}

/// Aggregate a PR's `statusCheckRollup` array into a single [`PrChecks`].
/// Precedence: Failing > Pending > Passing > None (empty array).
fn aggregate_checks(rollup: &serde_json::Value) -> PrChecks {
    let Some(items) = rollup.as_array() else {
        return PrChecks::None;
    };
    if items.is_empty() {
        return PrChecks::None;
    }
    let mut any_pending = false;
    for item in items {
        let (is_failure, is_pending) = classify_check(item);
        if is_failure {
            return PrChecks::Failing;
        }
        any_pending |= is_pending;
    }
    if any_pending {
        PrChecks::Pending
    } else {
        PrChecks::Passing
    }
}

/// [`pr_statuses`] with the `gh` binary injected, so tests can pass a stub path
/// instead of mutating the process environment.
fn pr_statuses_with(repo_dir: &str, gh_bin: &str) -> std::collections::HashMap<String, PrStatus> {
    let mut map = std::collections::HashMap::new();
    let out = match run_gh(
        gh_bin,
        repo_dir,
        &[
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            "100",
            "--json",
            "number,headRefName,state,url,reviewDecision,statusCheckRollup",
        ],
    ) {
        Ok(o) if o.status.success() => o,
        // Any failure (gh missing, not authed, not a gh repo) → empty map.
        _ => return map,
    };
    let Ok(prs) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) else {
        return map;
    };
    for pr in prs {
        let Some(branch) = pr.get("headRefName").and_then(|v| v.as_str()) else {
            continue;
        };
        let state = match pr.get("state").and_then(|v| v.as_str()) {
            Some("OPEN") => PrState::Open,
            Some("MERGED") => PrState::Merged,
            _ => PrState::Closed,
        };
        let review = match pr.get("reviewDecision").and_then(|v| v.as_str()) {
            Some("APPROVED") => PrReview::Approved,
            Some("CHANGES_REQUESTED") => PrReview::ChangesRequested,
            Some("REVIEW_REQUIRED") => PrReview::ReviewRequired,
            _ => PrReview::None,
        };
        let checks = pr
            .get("statusCheckRollup")
            .map(aggregate_checks)
            .unwrap_or(PrChecks::None);
        let status = PrStatus {
            number: pr.get("number").and_then(|v| v.as_u64()).unwrap_or(0),
            state,
            checks,
            review,
            url: pr.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        };
        // A branch name can carry several PRs over time (a reused `kommand0/<name>`
        // branch may have an old merged/closed PR and a newer open one, and
        // `gh pr list` order isn't guaranteed). Keep the most relevant: an OPEN PR
        // wins over non-open, then the higher (newer) number wins.
        map.entry(branch.to_string())
            .and_modify(|existing| {
                if pr_supersedes(&status, existing) {
                    *existing = status.clone();
                }
            })
            .or_insert(status);
    }
    map
}

/// Whether `a` should replace `b` as the PR shown for a shared branch name.
fn pr_supersedes(a: &PrStatus, b: &PrStatus) -> bool {
    let open_rank = |s: PrState| u8::from(s == PrState::Open);
    (open_rank(a.state), a.number) > (open_rank(b.state), b.number)
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

    /// Write an executable shell stub at `path` with the given body.
    fn write_stub(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    // --- diff_files_vs_default_branch ---

    #[test]
    fn diff_splits_committed_changes_into_per_file_sections() {
        // Two files in different dirs, changed on a branch → two FileDiffs with
        // the right (b/-side) paths and each file's own +/- content.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path()); // main + a.txt "hello"
        git(tmp.path(), &["switch", "-c", "feature"]);
        std::fs::write(tmp.path().join("a.txt"), "hello world").unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "fn main() {}\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "edit"]);

        let files = diff_files_vs_default_branch(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(files.len(), 2, "one section per changed file: {files:?}");
        let by_path: std::collections::HashMap<&str, &str> =
            files.iter().map(|f| (f.path.as_str(), f.text.as_str())).collect();
        assert!(by_path.contains_key("a.txt"), "parsed the top-level path: {files:?}");
        assert!(by_path.contains_key("src/lib.rs"), "parsed the nested path: {files:?}");
        assert!(by_path["a.txt"].contains("+hello world"), "a.txt shows its added line");
        assert!(by_path["a.txt"].starts_with("diff --git "), "section starts at its header");
        assert!(by_path["src/lib.rs"].contains("+fn main() {}"), "src/lib.rs shows its content");
        // Each section is self-contained (a file's text doesn't leak the other's).
        assert!(!by_path["a.txt"].contains("fn main"), "sections don't bleed into each other");
    }

    #[test]
    fn diff_is_empty_on_the_default_branch() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path()); // HEAD == main
        let files = diff_files_vs_default_branch(tmp.path().to_str().unwrap()).unwrap();
        assert!(files.is_empty(), "HEAD is the default branch → no diff: {files:?}");
    }

    #[test]
    fn diff_excludes_uncommitted_changes() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        git(tmp.path(), &["switch", "-c", "feature"]);
        // Working-tree change only — never committed.
        std::fs::write(tmp.path().join("a.txt"), "dirty").unwrap();
        let files = diff_files_vs_default_branch(tmp.path().to_str().unwrap()).unwrap();
        assert!(files.is_empty(), "committed-only diff excludes the working tree: {files:?}");
    }

    #[test]
    fn diff_is_none_outside_a_repo() {
        let tmp = TempDir::new().unwrap();
        assert!(diff_files_vs_default_branch(tmp.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn diff_is_pr_style_when_default_branch_advances() {
        // The load-bearing property of the three-dot `A...B` form: after the
        // branch diverges, later commits on the default branch must NOT show as
        // removals (two-dot `A..B` would show them). Pins the merge-base semantics.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path()); // main + a.txt
        git(tmp.path(), &["switch", "-c", "feature"]);
        std::fs::write(tmp.path().join("feat.txt"), "feature").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "add feat"]);
        // main advances independently after the branch diverged.
        git(tmp.path(), &["switch", "main"]);
        std::fs::write(tmp.path().join("main.txt"), "main").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "add main"]);
        git(tmp.path(), &["switch", "feature"]);

        let files = diff_files_vs_default_branch(tmp.path().to_str().unwrap()).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"feat.txt"), "shows the branch's own change: {paths:?}");
        assert!(
            !paths.contains(&"main.txt"),
            "three-dot must not show the default branch's later commits: {paths:?}"
        );
    }

    #[test]
    fn diff_resolves_master_when_main_is_absent() {
        // Exercises the fallback walk past refs/heads/main to refs/heads/master
        // (the base-resolution chain, not just the last-resort local `main`).
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-b", "master"]);
        git(tmp.path(), &["config", "user.email", "t@t"]);
        git(tmp.path(), &["config", "user.name", "t"]);
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "init"]);
        git(tmp.path(), &["switch", "-c", "feature"]);
        std::fs::write(tmp.path().join("a.txt"), "hello world").unwrap();
        git(tmp.path(), &["commit", "-am", "edit"]);

        let files = diff_files_vs_default_branch(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.txt");
        assert!(files[0].text.contains("+hello world"), "based against master: {files:?}");
    }

    #[test]
    fn diff_path_of_a_rename_into_a_subdir_is_the_new_side() {
        // A committed rename into a subdir: the path must be the new (`b/`) side,
        // taken from `+++ b/...`, not the old location.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path()); // main + a.txt
        git(tmp.path(), &["switch", "-c", "feature"]);
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::rename(tmp.path().join("a.txt"), tmp.path().join("sub/moved.txt")).unwrap();
        git(tmp.path(), &["add", "-A"]);
        git(tmp.path(), &["commit", "-m", "rename into sub"]);

        let files = diff_files_vs_default_branch(tmp.path().to_str().unwrap()).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"sub/moved.txt"), "path is the new b/ side: {paths:?}");
    }

    #[test]
    fn diff_path_of_a_delete_is_the_deleted_file_not_dev_null() {
        // A committed delete: `+++ /dev/null` must fall through to the `--- a/...`
        // (old) path — never "/dev/null".
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path()); // main + a.txt
        git(tmp.path(), &["switch", "-c", "feature"]);
        std::fs::remove_file(tmp.path().join("a.txt")).unwrap();
        git(tmp.path(), &["commit", "-am", "delete a.txt"]);

        let files = diff_files_vs_default_branch(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.txt", "delete keeps the deleted path: {files:?}");
    }

    #[test]
    fn diff_path_with_a_space_in_a_directory_name() {
        // A dir name with a space makes the `diff --git a/… b/…` header ambiguous
        // (`a b/` matches the wrong `" b/"`); deriving from `+++ b/` gets it right.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        git(tmp.path(), &["switch", "-c", "feature"]);
        std::fs::create_dir(tmp.path().join("a b")).unwrap();
        std::fs::write(tmp.path().join("a b/c.txt"), "x\n").unwrap();
        git(tmp.path(), &["add", "-A"]);
        git(tmp.path(), &["commit", "-m", "add spaced dir"]);

        let files = diff_files_vs_default_branch(tmp.path().to_str().unwrap()).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"a b/c.txt"), "spaced path parsed from +++ b/: {paths:?}");
    }

    #[test]
    fn file_diff_path_derivation_rules() {
        // +++ b/ wins (add/modify).
        assert_eq!(
            file_diff_path("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@\n"),
            "x"
        );
        // Delete: +++ /dev/null falls back to --- a/ (the old path).
        assert_eq!(
            file_diff_path("diff --git a/gone b/gone\n--- a/gone\n+++ /dev/null\n@@\n"),
            "gone"
        );
        // Neither +++/--- present (mode-only): fall back to the header " b/" split.
        assert_eq!(
            file_diff_path("diff --git a/m b/m\nold mode 100644\nnew mode 100755\n"),
            "m"
        );
        // A path with a space: the header split is wrong, but +++ b/ is exact.
        assert_eq!(
            file_diff_path("diff --git a/a b/c b/a b/c\n--- a/a b/c\n+++ b/a b/c\n@@\n"),
            "a b/c"
        );
    }

    #[test]
    fn diff_over_the_byte_cap_drops_whole_sections_with_a_note() {
        // Two big files, together over 1 MiB: the cap must drop a whole section
        // (never a truncated `diff --git` fragment) and note the drop.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        git(tmp.path(), &["switch", "-c", "feature"]);
        let big = "x\n".repeat(600 * 1024); // ~1.2 MiB each once diffed
        std::fs::write(tmp.path().join("big1.txt"), &big).unwrap();
        std::fs::write(tmp.path().join("big2.txt"), &big).unwrap();
        git(tmp.path(), &["add", "-A"]);
        git(tmp.path(), &["commit", "-m", "two big files"]);

        let files = diff_files_vs_default_branch(tmp.path().to_str().unwrap()).unwrap();
        // Every retained section is a whole diff (starts at its own header).
        for f in &files {
            assert!(
                f.text.starts_with("diff --git "),
                "no section is a truncated fragment: {:?}",
                &f.text[..f.text.len().min(40)]
            );
        }
        let joined: String = files.iter().map(|f| f.text.as_str()).collect();
        assert!(joined.contains("diff truncated"), "the drop is noted: {joined:.80}");
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

    // --- pr_statuses ---

    /// A `gh` stub whose `pr list` prints the given JSON array verbatim (and
    /// fails any other subcommand, so a stray call is caught).
    fn gh_list_stub(path: &Path, json: &str) {
        write_stub(
            path,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = pr ] && [ \"$2\" = list ]; then\n  cat <<'JSON'\n{json}\nJSON\n  exit 0\nfi\nexit 1\n"
            ),
        );
    }

    #[test]
    fn pr_statuses_parses_states_checks_and_reviews() {
        // One repo, many branches: a single `gh pr list` returns them all, keyed
        // by headRefName. Each branch exercises a distinct combination.
        let json = r#"[
          {"number":1,"headRefName":"pass","state":"OPEN","url":"https://x/1","reviewDecision":"APPROVED",
           "statusCheckRollup":[{"conclusion":"SUCCESS","status":"COMPLETED"},{"state":"SUCCESS"}]},
          {"number":2,"headRefName":"fail","state":"OPEN","url":"https://x/2","reviewDecision":"CHANGES_REQUESTED",
           "statusCheckRollup":[{"conclusion":"SUCCESS","status":"COMPLETED"},{"conclusion":"FAILURE","status":"COMPLETED"}]},
          {"number":3,"headRefName":"pending","state":"OPEN","url":"https://x/3","reviewDecision":"REVIEW_REQUIRED",
           "statusCheckRollup":[{"conclusion":null,"status":"IN_PROGRESS"}]},
          {"number":4,"headRefName":"merged","state":"MERGED","url":"https://x/4","reviewDecision":"APPROVED",
           "statusCheckRollup":[{"conclusion":"SUCCESS","status":"COMPLETED"}]},
          {"number":5,"headRefName":"empty","state":"OPEN","url":"https://x/5","reviewDecision":"",
           "statusCheckRollup":[]},
          {"number":6,"headRefName":"closed","state":"CLOSED","url":"https://x/6","reviewDecision":null,
           "statusCheckRollup":[{"state":"ERROR"}]},
          {"number":7,"headRefName":"statusctx-pending","state":"OPEN","url":"https://x/7","reviewDecision":"APPROVED",
           "statusCheckRollup":[{"state":"PENDING"}]},
          {"number":8,"headRefName":"neutral","state":"OPEN","url":"https://x/8","reviewDecision":"APPROVED",
           "statusCheckRollup":[{"conclusion":"NEUTRAL","status":"COMPLETED"},{"conclusion":"SKIPPED","status":"COMPLETED"}]},
          {"number":9,"headRefName":"statusctx-fail","state":"OPEN","url":"https://x/9","reviewDecision":"APPROVED",
           "statusCheckRollup":[{"state":"FAILURE"}]}
        ]"#;
        let tmp = TempDir::new().unwrap();
        let gh = tmp.path().join("gh");
        gh_list_stub(&gh, json);
        let m = pr_statuses_with(tmp.path().to_str().unwrap(), gh.to_str().unwrap());

        assert_eq!(m.len(), 9);

        let pass = &m["pass"];
        assert_eq!(pass.number, 1);
        assert_eq!(pass.state, PrState::Open);
        assert_eq!(pass.checks, PrChecks::Passing);
        assert_eq!(pass.review, PrReview::Approved);
        assert_eq!(pass.url, "https://x/1");

        // A failure anywhere in the rollup wins over passing checks.
        assert_eq!(m["fail"].checks, PrChecks::Failing);
        assert_eq!(m["fail"].review, PrReview::ChangesRequested);

        // A null conclusion (not-yet-complete CheckRun) reads as pending.
        assert_eq!(m["pending"].checks, PrChecks::Pending);
        assert_eq!(m["pending"].review, PrReview::ReviewRequired);

        assert_eq!(m["merged"].state, PrState::Merged);

        // An empty rollup → no checks; an empty reviewDecision → no review.
        assert_eq!(m["empty"].checks, PrChecks::None);
        assert_eq!(m["empty"].review, PrReview::None);

        // A non-open/merged state is Closed; a StatusContext ERROR is a failure;
        // a null reviewDecision → no review.
        assert_eq!(m["closed"].state, PrState::Closed);
        assert_eq!(m["closed"].checks, PrChecks::Failing);
        assert_eq!(m["closed"].review, PrReview::None);

        // A StatusContext PENDING (no conclusion field at all) reads as pending.
        assert_eq!(m["statusctx-pending"].checks, PrChecks::Pending);

        // NEUTRAL/SKIPPED are benign completed conclusions — not failing, not
        // pending, so a rollup of only those is Passing.
        assert_eq!(m["neutral"].checks, PrChecks::Passing);

        // A StatusContext state FAILURE (distinct literal from ERROR) is a failure.
        assert_eq!(m["statusctx-fail"].checks, PrChecks::Failing);
    }

    #[test]
    fn pr_statuses_dedupes_reused_branch_to_the_open_pr() {
        // Same headRefName across two PRs (an old merged one and a new open one):
        // the OPEN PR must win regardless of gh's list order.
        let json = r#"[
          {"number":20,"headRefName":"kommand0/feat","state":"OPEN","url":"https://x/20","reviewDecision":"APPROVED",
           "statusCheckRollup":[{"conclusion":"SUCCESS","status":"COMPLETED"}]},
          {"number":9,"headRefName":"kommand0/feat","state":"MERGED","url":"https://x/9","reviewDecision":"APPROVED",
           "statusCheckRollup":[{"conclusion":"SUCCESS","status":"COMPLETED"}]}
        ]"#;
        let tmp = TempDir::new().unwrap();
        let gh = tmp.path().join("gh");
        gh_list_stub(&gh, json);
        let m = pr_statuses_with(tmp.path().to_str().unwrap(), gh.to_str().unwrap());
        assert_eq!(m["kommand0/feat"].number, 20, "the open PR wins over the merged one");
        assert_eq!(m["kommand0/feat"].state, PrState::Open);
    }

    #[test]
    fn pr_statuses_failing_beats_pending() {
        // Precedence guard: a failure and a still-running check together → Failing.
        let json = r#"[
          {"number":9,"headRefName":"br","state":"OPEN","url":"https://x/9","reviewDecision":"APPROVED",
           "statusCheckRollup":[{"conclusion":null,"status":"QUEUED"},{"conclusion":"TIMED_OUT","status":"COMPLETED"}]}
        ]"#;
        let tmp = TempDir::new().unwrap();
        let gh = tmp.path().join("gh");
        gh_list_stub(&gh, json);
        let m = pr_statuses_with(tmp.path().to_str().unwrap(), gh.to_str().unwrap());
        assert_eq!(m["br"].checks, PrChecks::Failing);
    }

    #[test]
    fn pr_statuses_is_empty_when_gh_missing() {
        let tmp = TempDir::new().unwrap();
        let m = pr_statuses_with(tmp.path().to_str().unwrap(), "/nonexistent/definitely/not/gh");
        assert!(m.is_empty(), "a missing gh yields an empty map, never a panic");
    }

    #[test]
    fn pr_statuses_is_empty_on_gh_failure() {
        // gh present but exits non-zero (e.g. not a gh repo / not authed).
        let tmp = TempDir::new().unwrap();
        let gh = tmp.path().join("gh");
        write_stub(&gh, "#!/bin/sh\nexit 1\n");
        let m = pr_statuses_with(tmp.path().to_str().unwrap(), gh.to_str().unwrap());
        assert!(m.is_empty());
    }
}
