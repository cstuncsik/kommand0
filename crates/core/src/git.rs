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
//! thread (it makes a network call). [`scan_merged_branches`] and
//! [`delete_branches`] are its repo-wide counterpart: a verdict per local branch
//! (one gh call each), then a local delete that re-checks every gate.

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
///
/// Counterpart of [`is_default_branch`], which is the cleanup gate's check:
/// this wants precision (the one true diff base), that wants recall (over-block
/// anything plausibly the default). Don't unify them.
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
        // A branch name can carry several PRs over time (a reused branch may
        // have an old merged/closed PR and a newer open one, and
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
/// - the branch is not the repo's default branch (see [`is_default_branch`]),
///   not a malformed name (empty, `..`, leading `-`), and not one of the
///   `protected` names (the config's `protected_branches`). Any *other* branch,
///   including one kommand0 didn't create, is fair game once the checks below
///   hold: adopting and cleaning up your own branches is deliberate behavior;
/// - its PR is `MERGED` (per `gh`);
/// - the worktree, if it still exists, is live and its HEAD is still on
///   `branch` (a `git switch` inside it made the dir another branch's checkout);
/// - the worktree is clean (no uncommitted/untracked changes; an unreadable
///   status aborts rather than assuming clean); and
/// - the branch tip equals the last commit the PR merged (so there are no
///   commits beyond the PR — squash-safe, and catches pushed or unpushed extras).
///
/// The worktree is removed WITHOUT `--force` (a last-moment dirty state still
/// fails safe), and only then is the branch deleted — locally only; the remote
/// branch is never touched.
///
/// A removal that *fails* has two shapes, told apart by [`is_live_worktree`]:
/// git refused and touched nothing (its own reason is reported, nothing is
/// deleted), or it died mid-delete after dropping the admin entry, leaving an
/// unusable half-deleted tree that this finishes deleting. Either way the branch
/// survives an `Err`, so a retry re-runs every gate: `Ok`/`Err` is the caller's
/// deregister signal, so a path that deletes the branch must return `Ok`.
pub fn cleanup_merged_workspace(
    repo_path: &str,
    worktree_path: &str,
    branch: &str,
    protected: &[String],
) -> Result<(), String> {
    cleanup_merged_workspace_with(repo_path, worktree_path, branch, protected, &gh_bin())
}

/// Whether `branch` is (or plausibly is) the repo's default branch — the
/// cleanup gate's trunk protection. Best-effort: a failed probe just doesn't
/// match, so an offline/remote-less repo never blocks on this. Two checks:
/// - the literals `main`/`master`, unconditionally — even when the actual
///   default is something else, since e.g. a gitflow back-merge PR with
///   `headRefName == main` would pass the merged-PR check. Accepted flip side:
///   a branch literally named `master` in a `main`-default repo is refused
///   (delete it by hand).
/// - whatever `origin/HEAD` points at, when resolvable — covers `trunk`/
///   `develop`-style defaults. Residual: a default named neither main/master
///   in a repo without `origin/HEAD` is unprotected here; the merged-PR +
///   tip-equality checks still apply, the delete is local-only, and the branch
///   is restorable from the remote.
///
/// Counterpart of [`default_branch_ref`] (the diff base): that wants precision,
/// this wants recall. Don't unify them.
fn is_default_branch(repo_path: &str, branch: &str) -> bool {
    if branch == "main" || branch == "master" {
        return true;
    }
    // ponytail: one `symbolic-ref` spawn per gated branch; resolve origin/HEAD
    // once per scan if a huge repo makes the local gates measurable.
    match Command::new("git")
        .args(["-C", repo_path, "symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
    {
        Ok(o) if o.status.success() => {
            let name = last_line(&o.stdout);
            name.strip_prefix("origin/").unwrap_or(&name) == branch
        }
        _ => false,
    }
}

/// Whether `worktree_path` is still a worktree git knows about.
///
/// The discriminator for a failed `git worktree remove`: git deletes the
/// `.git/worktrees/<id>` admin entry BEFORE unlinking the files, so a directory
/// whose `rev-parse` no longer resolves is the wreckage of a half-done removal.
/// Fails safe in every adjacent shape: a nested independent repo and a plain
/// subdirectory of a repo both resolve (so both count as live, never wreckage),
/// and git failing to run at all counts as live too.
fn is_live_worktree(worktree_path: &str) -> bool {
    match Command::new("git")
        .args(["-C", worktree_path, "rev-parse", "--git-dir"])
        .output()
    {
        Ok(o) => o.status.success(),
        Err(_) => true,
    }
}

/// The newest PR whose head is a branch, as `gh` reports it.
struct PrLookup {
    state: String,
    /// The oid of the PR's last commit (empty when it has none).
    tip: String,
    /// Absent when gh printed the older two-line shape.
    number: Option<u64>,
}

/// `Ok(None)` = no PR has that head. `Err` = gh itself failed: not installed or
/// timed out, or a non-zero exit (not authenticated, not a GitHub repo). Looks
/// up by `--head <branch>` (a bare positional would be parsed as a PR NUMBER for
/// an all-digit branch name); several PRs can share a head over time, so the
/// newest by number wins, mirroring the pr-status view.
fn lookup_pr(gh_bin: &str, repo_path: &str, branch: &str) -> Result<Option<PrLookup>, String> {
    let out = match run_gh(
        gh_bin,
        repo_path,
        &[
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "number,state,commits",
            "-q",
            "(sort_by(.number) | last) // {} | (.state // \"NONE\"), (.commits[-1].oid // \"\"), (.number // \"\")",
        ],
    ) {
        Ok(o) if o.status.success() => o,
        Ok(o) => return Err(format!("gh pr list failed ({})", last_line(&o.stderr))),
        Err(_) => return Err("gh CLI not found — install GitHub CLI to clean up".to_string()),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let state = lines.next().unwrap_or("").trim().to_string();
    if state == "NONE" {
        return Ok(None);
    }
    let tip = lines.next().unwrap_or("").trim().to_string();
    let number = lines.next().and_then(|l| l.trim().parse().ok());
    Ok(Some(PrLookup { state, tip, number }))
}

/// Why a local branch must not be deleted, in gate order.
enum BranchRefusal {
    Malformed,
    Default,
    Protected,
}

impl BranchRefusal {
    /// The scan's skip reason.
    fn short(&self) -> &'static str {
        match self {
            Self::Malformed => "malformed branch name",
            Self::Default => "default branch",
            Self::Protected => "protected branch",
        }
    }

    /// The cleanup's refusal message.
    fn message(&self, branch: &str) -> String {
        match self {
            Self::Malformed => "refusing to delete a malformed branch name".to_string(),
            Self::Default => format!("refusing to delete the default branch ({branch})"),
            Self::Protected => format!(
                "refusing to delete a protected branch ({branch}); remove it from protected_branches to clean up"
            ),
        }
    }
}

/// The gate every local branch delete runs first, so protection lives in one
/// place. Order: malformed, default, protected (a protected name that is also
/// the default reports the stronger reason).
fn refuse_branch_delete(
    repo_path: &str,
    branch: &str,
    protected: &[String],
) -> Result<(), BranchRefusal> {
    // Malformed names: `..` is refused because `rev-parse refs/heads/a..b`
    // REINTERPRETS it as a range (the tip check would fail-closed only by output
    // shape, not by rejection); a leading `-` can't come from a validated
    // workspace name but an adopted ref could carry one.
    if branch.is_empty() || branch.contains("..") || branch.starts_with('-') {
        return Err(BranchRefusal::Malformed);
    }
    // Trunk protection: the one branch cleanup must never delete, however the
    // workspace came to sit on it.
    if is_default_branch(repo_path, branch) {
        return Err(BranchRefusal::Default);
    }
    if protected.iter().any(|p| p == branch) {
        return Err(BranchRefusal::Protected);
    }
    Ok(())
}

/// `git branch -D -- <branch>`: force, because a squash-merge leaves the branch
/// "unmerged" locally (the callers' tip checks proved nothing lies beyond the
/// merge). `--` makes the gate's leading-dash refusal belt-and-braces, not
/// load-bearing. Err is git's last stderr line or the io error.
fn delete_local_branch(repo_path: &str, branch: &str) -> Result<(), String> {
    match Command::new("git")
        .args(["-C", repo_path, "branch", "-D", "--", branch])
        .output()
    {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(last_line(&o.stderr)),
        Err(e) => Err(e.to_string()),
    }
}

/// The oid `refs/heads/<branch>` points at, or None when it doesn't resolve.
fn branch_tip(repo_path: &str, branch: &str) -> Option<String> {
    match Command::new("git")
        .args(["-C", repo_path, "rev-parse", &format!("refs/heads/{branch}")])
        .output()
    {
        Ok(o) if o.status.success() => Some(last_line(&o.stdout)),
        _ => None,
    }
}

fn cleanup_merged_workspace_with(
    repo_path: &str,
    worktree_path: &str,
    branch: &str,
    protected: &[String],
    gh_bin: &str,
) -> Result<(), String> {
    refuse_branch_delete(repo_path, branch, protected).map_err(|r| r.message(branch))?;

    // The PR must be merged; capture the oid of the last commit it merged. Run
    // gh from the repo (not the worktree) so a partial-cleanup retry (worktree
    // already gone) still works instead of failing with a bogus "gh not found".
    let Some(PrLookup { state, tip: pr_tip, .. }) = lookup_pr(gh_bin, repo_path, branch)? else {
        return Err("no PR found for this branch — not cleaning up".to_string());
    };
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
        // A dir git no longer recognizes is wreckage from an earlier removal
        // that died mid-delete: there is no status to read, and nothing here can
        // prove what survived, so say so rather than dead-end on an unreadable
        // status (the old message) or delete a path we can't vouch for.
        if !is_live_worktree(worktree_path) {
            return Err(format!(
                "a failed removal left files behind: delete {worktree_path}, then clean up again"
            ));
        }
        // HEAD must still be on `branch`: after a `git switch` inside the
        // worktree the dir is another branch's checkout, and removing it would
        // take that checkout (ignored files included) with it. Full ref, not
        // `--short`: with a same-named tag `--short` prints `heads/<b>`.
        let head = match Command::new("git")
            .args(["-C", worktree_path, "symbolic-ref", "HEAD"])
            .output()
        {
            Ok(o) if o.status.success() => last_line(&o.stdout),
            _ => String::new(), // detached (exit 128), or git itself failed
        };
        if head != format!("refs/heads/{branch}") {
            let on = match head.strip_prefix("refs/heads/") {
                Some(b) => b,
                None if head.is_empty() => "a detached HEAD",
                None => head.as_str(),
            };
            return Err(format!(
                "the worktree is on {on}, not {branch}; switch back or delete it by hand"
            ));
        }
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
    let Some(local_tip) = branch_tip(repo_path, branch) else {
        return Err("couldn't resolve the branch tip — not cleaning up".to_string());
    };
    if pr_tip.is_empty() || local_tip != pr_tip {
        return Err("the branch has commits beyond its merged PR — not cleaning up".to_string());
    }

    // Remove the worktree (no --force, so a last-moment dirty state still fails
    // safe). If the dir survives, which of the two failure shapes it is comes
    // from re-probing, never from matching git's stderr text.
    if worktree_exists {
        let out = Command::new("git")
            .args(["-C", repo_path, "worktree", "remove", worktree_path])
            .output();
        if std::path::Path::new(worktree_path).exists() {
            if is_live_worktree(worktree_path) {
                // Git refused before touching a byte. Report ITS reason: it
                // checks things this gate doesn't (a dirty submodule, since its
                // status runs with --ignore-submodules=none).
                let mut reason = match &out {
                    Ok(o) => last_line(&o.stderr),
                    Err(e) => e.to_string(),
                };
                if reason.is_empty() {
                    reason = "it may have changes".to_string();
                }
                return Err(format!("couldn't remove the worktree ({reason})"));
            }
            // Live at the gate, not live now: git dropped the admin entry and
            // unlinked an arbitrary subset of the files, so the tree is unusable
            // and its tracked content was just proven identical to the merged PR
            // tip. Finish the delete git started; leaving it is what made this
            // state unrecoverable.
            if let Err(e) = std::fs::remove_dir_all(worktree_path) {
                return Err(format!(
                    "a failed removal left files behind: delete {worktree_path}, then clean up again ({e})"
                ));
            }
        }
    }
    // Clean up any stale worktree admin entry (whether we removed it or it was
    // already gone) so the branch is deletable.
    let _ = Command::new("git")
        .args(["-C", repo_path, "worktree", "prune"])
        .output();

    // The PR-tip check above proved there's nothing beyond the merge.
    delete_local_branch(repo_path, branch).map_err(|e| {
        if worktree_exists {
            format!("worktree removed, but couldn't delete branch {branch}: {e}")
        } else {
            format!("couldn't delete branch {branch}: {e}")
        }
    })
}

/// What the repo-wide scan decided for one local branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Merged PR, tip equals the PR's last commit, checked out nowhere.
    Delete,
    /// Same, but checked out at `worktree` (git refuses `branch -D` there).
    CheckedOut { worktree: String },
    /// Not deletable, and why (short, user-facing).
    Skip(String),
}

/// One local branch as [`scan_merged_branches`] saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchVerdict {
    /// The short name (`refs/heads/` stripped).
    pub branch: String,
    /// The tip at scan time; [`delete_branches`] refuses a branch that moved.
    pub tip: String,
    /// The PR number, whenever a PR was found.
    pub pr: Option<u64>,
    /// What the scan decided for it.
    pub verdict: Verdict,
}

/// Classify every local branch of `repo_path` for deletion: the name gates of
/// [`cleanup_merged_workspace`] first (default/malformed/protected names never
/// reach gh), then one PR lookup per remaining branch. Deletes and prunes
/// nothing. The first gh failure aborts the whole scan, so a missing or wedged
/// gh can't read as "no PR" for every branch.
pub fn scan_merged_branches(
    repo_path: &str,
    protected: &[String],
) -> Result<Vec<BranchVerdict>, String> {
    scan_merged_branches_with(repo_path, protected, &gh_bin())
}

fn scan_merged_branches_with(
    repo_path: &str,
    protected: &[String],
    gh_bin: &str,
) -> Result<Vec<BranchVerdict>, String> {
    // NUL-separated, NUL-terminated records: a newline in a foreign worktree
    // path can't truncate one. `%(refname)` + strip, not `refname:short`, so a
    // same-named tag can't shadow the branch.
    let out = Command::new("git")
        .args([
            "-C",
            repo_path,
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(worktreepath)%00",
            "refs/heads",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(last_line(&out.stderr));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let fields: Vec<&str> = text.split('\0').collect();
    let mut verdicts = Vec::new();
    // for-each-ref ends each record with '\n', which lands in front of the next
    // refname; the newline-only remainder after the last NUL is dropped.
    let (records, _) = fields.as_chunks::<3>();
    for &[refname, tip, worktree] in records {
        let Some(branch) = refname.trim_start_matches('\n').strip_prefix("refs/heads/") else {
            continue;
        };
        let tip = tip.to_string();
        let (pr, verdict) = match refuse_branch_delete(repo_path, branch, protected) {
            Err(r) => (None, Verdict::Skip(r.short().to_string())),
            // ponytail: one gh round-trip per branch; batch them through `gh api graphql`
            // if a repo with hundreds of branches makes the scan too slow.
            Ok(()) => match lookup_pr(gh_bin, repo_path, branch).map_err(|e| format!("{branch}: {e}"))? {
                None => (None, Verdict::Skip("no PR".to_string())),
                Some(pr) => {
                    let verdict = if pr.state != "MERGED" {
                        Verdict::Skip(format!("PR not merged ({})", pr.state))
                    } else if pr.tip.is_empty() || pr.tip != tip {
                        Verdict::Skip("commits beyond the merged PR".to_string())
                    } else if !worktree.is_empty() {
                        Verdict::CheckedOut { worktree: worktree.to_string() }
                    } else {
                        Verdict::Delete
                    };
                    (pr.number, verdict)
                }
            },
        };
        verdicts.push(BranchVerdict { branch: branch.to_string(), tip, pr, verdict });
    }
    Ok(verdicts)
}

/// Delete local branches, each only while it still points at its scan-time
/// `tip` (else "moved since scan"). The name gates re-run; nothing here touches
/// remotes, prunes, or calls gh. Results come back in input order.
pub fn delete_branches(
    repo_path: &str,
    branches: &[(String, String)],
    protected: &[String],
) -> Vec<(String, Result<(), String>)> {
    let delete = |branch: &str, tip: &str| -> Result<(), String> {
        refuse_branch_delete(repo_path, branch, protected).map_err(|r| r.message(branch))?;
        if branch_tip(repo_path, branch).as_deref() != Some(tip) {
            return Err("moved since scan".to_string());
        }
        delete_local_branch(repo_path, branch)
    };
    branches.iter().map(|(b, t)| (b.clone(), delete(b, t))).collect()
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

    /// A repo (no remote — pins that an unresolvable `origin/HEAD` never blocks
    /// the gate) with a linked worktree on `branch`. Returns
    /// `(repo_path, worktree_path, branch, branch_tip_sha)`.
    fn repo_with_worktree_on(
        root: &Path,
        branch: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, String, String) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "1").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);
        let wt = root.join("wt");
        git(&repo, &["worktree", "add", wt.to_str().unwrap(), "-b", branch]);
        let sha = tip_of(&repo, branch);
        (repo, wt, branch.to_string(), sha)
    }

    /// [`repo_with_worktree_on`] with the post-prefix default shape: bare `feat`.
    fn repo_with_worktree(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, String, String) {
        repo_with_worktree_on(root, "feat")
    }

    /// A `gh` stub answering the cleanup lookup (`pr list --head <branch> …`)
    /// with `<state>\n<oid>`.
    fn gh_pr_stub(path: &Path, state: &str, oid: &str) {
        write_stub(
            path,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = pr ] && [ \"$2\" = list ] && [ \"$3\" = --head ]; then\n  printf '{state}\\n{oid}\\n'\n  exit 0\nfi\nexit 1\n"
            ),
        );
    }

    fn cleanup(repo: &Path, wt: &Path, branch: &str, gh: &Path) -> Result<(), String> {
        cleanup_merged_workspace_with(
            repo.to_str().unwrap(),
            wt.to_str().unwrap(),
            branch,
            &[],
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
        gh_pr_stub(&gh, "MERGED", &sha);
        assert_eq!(cleanup(&repo, &wt, &branch, &gh), Ok(()));
        assert!(!wt.exists(), "worktree dir removed");
        assert!(!branch_exists(&repo, &branch), "branch deleted");
    }

    #[test]
    fn cleanup_refuses_when_pr_open_and_destroys_nothing() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        let gh = tmp.path().join("gh");
        gh_pr_stub(&gh, "OPEN", &sha);
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
        gh_pr_stub(&gh, "MERGED", &sha);
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
        gh_pr_stub(&gh, "MERGED", &sha); // stale (pre-extra-commit) oid
        let err = cleanup(&repo, &wt, &branch, &gh).unwrap_err();
        assert!(err.contains("beyond its merged PR"), "expected 'beyond its merged PR', got: {err}");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_refuses_default_branch() {
        // Literal main/master arm, unconditional. The gh path doesn't exist, so
        // passing proves the gate fires before any network call. The fixture
        // repo has no remote → origin/HEAD is unresolvable → only the literal
        // arm can refuse.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, _, _) = repo_with_worktree(tmp.path());
        for default in ["main", "master"] {
            let err = cleanup(&repo, &wt, default, &tmp.path().join("gh")).unwrap_err();
            assert!(err.contains("default branch"), "expected default-branch refusal, got: {err}");
        }
        assert!(branch_exists(&repo, "main"), "main untouched");
    }

    #[test]
    fn cleanup_refuses_default_branch_via_origin_head() {
        // origin/HEAD arm: the origin's default is `trunk` (NOT main/master, or
        // the literal arm would make this vacuous). A sibling branch must get
        // PAST the gate (failing later on the absent gh) — the refusal is "is
        // the default", not "refuses everything".
        let tmp = TempDir::new().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-b", "trunk"]);
        git(&origin, &["config", "user.email", "t@t"]);
        git(&origin, &["config", "user.name", "t"]);
        std::fs::write(origin.join("a.txt"), "1").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-m", "init"]);
        let clone = tmp.path().join("clone");
        git(tmp.path(), &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()]);
        git(&clone, &["branch", "sibling"]);

        let wt = tmp.path().join("no-wt"); // gate fires before any worktree use
        let gh = tmp.path().join("gh"); // absent
        let err = cleanup(&clone, &wt, "trunk", &gh).unwrap_err();
        assert!(err.contains("default branch"), "trunk refused via origin/HEAD: {err}");
        let err = cleanup(&clone, &wt, "sibling", &gh).unwrap_err();
        assert!(err.contains("gh CLI not found"), "sibling passes the gate: {err}");
    }

    #[test]
    fn cleanup_refuses_empty_dotdot_and_dash_names() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, _, _) = repo_with_worktree(tmp.path());
        let gh = tmp.path().join("gh"); // absent — gate must fire first
        for bad in ["", "a..b", "-feat"] {
            let err = cleanup(&repo, &wt, bad, &gh).unwrap_err();
            assert!(err.contains("malformed"), "{bad:?} refused as malformed, got: {err}");
        }
    }

    #[test]
    fn cleanup_deletes_an_adopted_branch_when_merged() {
        // The feature's core semantic: a branch kommand0 didn't name (adopted
        // via --branch / the checkout offer) is cleanable once merged — the
        // old `kommand0/` ownership gate is gone by design.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree_on(tmp.path(), "feat/login");
        let gh = tmp.path().join("gh");
        gh_pr_stub(&gh, "MERGED", &sha);
        assert_eq!(cleanup(&repo, &wt, &branch, &gh), Ok(()));
        assert!(!wt.exists() && !branch_exists(&repo, &branch), "adopted branch cleaned up");
    }

    #[test]
    fn cleanup_accepts_a_legacy_prefixed_branch() {
        // Workspaces created before the prefix was dropped keep working.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree_on(tmp.path(), "kommand0/legacy");
        let gh = tmp.path().join("gh");
        gh_pr_stub(&gh, "MERGED", &sha);
        assert_eq!(cleanup(&repo, &wt, &branch, &gh), Ok(()));
        assert!(!wt.exists() && !branch_exists(&repo, &branch), "legacy branch cleaned up");
    }

    #[test]
    fn cleanup_pr_lookup_handles_numeric_branch() {
        // An all-digit branch must be looked up via `--head <branch>` — a bare
        // positional would be parsed by gh as a PR NUMBER. The stub records its
        // argv and only answers the --head form.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree_on(tmp.path(), "123");
        let gh = tmp.path().join("gh");
        write_stub(
            &gh,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\nif [ \"$1\" = pr ] && [ \"$2\" = list ] && [ \"$3\" = --head ] && [ \"$4\" = 123 ]; then\n  printf 'MERGED\\n{sha}\\n'\n  exit 0\nfi\nexit 1\n"
            ),
        );
        assert_eq!(cleanup(&repo, &wt, &branch, &gh), Ok(()));
        let args = std::fs::read_to_string(format!("{}.args", gh.display())).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(&args[..4], &["pr", "list", "--head", "123"], "lookup is --head-based");
    }

    #[test]
    fn cleanup_refuses_a_worktree_switched_to_another_branch() {
        // The workspace's dir is live but a `git switch` inside it moved HEAD off
        // the workspace's branch: removing the dir would destroy the OTHER
        // branch's checkout (and its ignored files) on a merged-PR verdict that
        // was never about it.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        git(&wt, &["switch", "-c", "elsewhere"]);
        let gh = tmp.path().join("gh");
        gh_pr_stub(&gh, "MERGED", &sha);
        let err = cleanup(&repo, &wt, &branch, &gh).unwrap_err();
        assert!(err.contains("is on elsewhere, not"), "names both branches: {err}");
        assert!(wt.exists(), "worktree untouched");
        assert!(branch_exists(&repo, &branch) && branch_exists(&repo, "elsewhere"), "both branches intact");
        // Detached HEAD is the same refusal.
        git(&wt, &["switch", "--detach"]);
        let err = cleanup(&repo, &wt, &branch, &gh).unwrap_err();
        assert!(err.contains("detached HEAD"), "detached is refused too: {err}");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_refuses_when_pr_has_no_commits() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, _) = repo_with_worktree(tmp.path());
        let gh = tmp.path().join("gh");
        gh_pr_stub(&gh, "MERGED", ""); // empty oid (no commits)
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
        gh_pr_stub(&gh, "MERGED", &sha);
        assert_eq!(cleanup(&repo, &wt, &branch, &gh), Ok(()));
        assert!(!branch_exists(&repo, &branch), "orphaned branch deleted");
    }

    #[test]
    fn cleanup_does_not_claim_a_removal_when_the_worktree_was_already_gone() {
        // Nothing was removed on this path, so a failed `branch -D` (the branch
        // is checked out in the main repo by now) must not say "worktree removed".
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        std::fs::remove_dir_all(&wt).unwrap();
        git(&repo, &["worktree", "prune"]);
        git(&repo, &["switch", &branch]);
        let gh = tmp.path().join("gh");
        gh_pr_stub(&gh, "MERGED", &sha);
        let err = cleanup(&repo, &wt, &branch, &gh).unwrap_err();
        assert!(err.starts_with(&format!("couldn't delete branch {branch}:")), "{err}");
        assert!(branch_exists(&repo, &branch), "the branch survives the refusal");
    }

    #[test]
    fn is_live_worktree_tells_wreckage_from_everything_else() {
        // The guard the fs fallback rides on: only a dir git has ALREADY stopped
        // recognizing may be deleted outright, and every adjacent shape has to
        // land on the protected side.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, _branch, _sha) = repo_with_worktree(tmp.path());
        assert!(is_live_worktree(wt.to_str().unwrap()), "a linked worktree is live");
        assert!(is_live_worktree(repo.to_str().unwrap()), "so is the repo itself");
        let sub = wt.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(is_live_worktree(sub.to_str().unwrap()), "so is a subdir of one");
        let nested = tmp.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        git(&nested, &["init", "-b", "main"]);
        assert!(is_live_worktree(nested.to_str().unwrap()), "so is an independent repo");
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!is_live_worktree(plain.to_str().unwrap()), "a non-repo dir is not");
        // The wreckage: admin entry gone, dangling `.git` left behind.
        std::fs::remove_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        assert!(!is_live_worktree(wt.to_str().unwrap()), "half-deleted tree is not live");
    }

    #[test]
    fn cleanup_names_the_leftovers_of_a_half_deleted_worktree() {
        // The shape a failed removal leaves behind: git dropped the admin entry
        // and some files before dying, so the dir has a dangling `.git` and no
        // readable status. The old code dead-ended here ("couldn't read the
        // worktree's git status") with no way to finish from kommand0 at all.
        // It must name what to delete and keep the branch, so the retry works.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        std::fs::remove_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::remove_file(wt.join("a.txt")).unwrap(); // git got this far
        let gh = tmp.path().join("gh");
        gh_pr_stub(&gh, "MERGED", &sha);
        let err = cleanup(&repo, &wt, &branch, &gh).unwrap_err();
        assert!(err.contains(wt.to_str().unwrap()), "names the dir to delete: {err}");
        assert!(wt.exists(), "the leftovers are NOT deleted for us");
        assert!(branch_exists(&repo, &branch), "branch survives, so a retry can run");
        // And once the user deletes it, the retry finishes the job.
        std::fs::remove_dir_all(&wt).unwrap();
        assert_eq!(cleanup(&repo, &wt, &branch, &gh), Ok(()));
        assert!(!branch_exists(&repo, &branch), "retry deletes the orphaned branch");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_never_deletes_the_branch_when_the_worktree_survives() {
        // mkcert-shaped fixture: an IGNORED dir (so kommand0's own dirty gate
        // passes) whose mode blocks unlinking its contents, so git's remove dies
        // mid-delete, after dropping the admin entry. Assert the invariant, not
        // the arm: Ok/Err is the caller's deregister signal, so deleting the
        // branch while the dir survives would leave a workspace row whose next
        // cleanup can never resolve a branch tip. Both shapes are legal here: a
        // root test runner ignores mode 555 and git just succeeds.
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, sha) = repo_with_worktree(tmp.path());
        // `info/exclude` lives in the common dir, so it covers the worktree too
        // (no commit, so the branch tip still matches the stubbed PR).
        std::fs::write(repo.join(".git/info/exclude"), ".certs/\n").unwrap();
        std::fs::create_dir_all(wt.join(".certs")).unwrap();
        std::fs::write(wt.join(".certs/k.pem"), "key").unwrap();
        std::fs::set_permissions(wt.join(".certs"), std::fs::Permissions::from_mode(0o555)).unwrap();
        let gh = tmp.path().join("gh");
        gh_pr_stub(&gh, "MERGED", &sha);
        let res = cleanup(&repo, &wt, &branch, &gh);
        // Restore before the TempDir drop, or the fixture leaks a temp dir.
        let _ =
            std::fs::set_permissions(wt.join(".certs"), std::fs::Permissions::from_mode(0o755));
        match res {
            Ok(()) => {
                assert!(!wt.exists(), "Ok means the dir really is gone");
                assert!(!branch_exists(&repo, &branch), "Ok deletes the branch");
            }
            Err(e) => {
                assert!(wt.exists(), "Err leaves the leftovers in place: {e}");
                assert!(branch_exists(&repo, &branch), "Err keeps the branch: {e}");
                assert!(e.contains("delete") || e.contains("remove"), "actionable: {e}");
            }
        }
    }

    #[test]
    fn cleanup_refuses_protected_branch_before_gh() {
        // gh is absent, so passing proves the gate fires before any network
        // call; with an empty list the same branch reaches gh (the list is
        // honored, not hardcoded).
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch, _) = repo_with_worktree_on(tmp.path(), "development");
        let gh = tmp.path().join("gh");
        let err = cleanup_merged_workspace_with(
            repo.to_str().unwrap(),
            wt.to_str().unwrap(),
            &branch,
            &["development".to_string()],
            gh.to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("protected branch"), "expected protected refusal, got: {err}");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
        let err = cleanup(&repo, &wt, &branch, &gh).unwrap_err();
        assert!(err.contains("gh CLI not found"), "an empty list lets it through to gh: {err}");
    }

    // --- scan_merged_branches / delete_branches ---

    /// [`init_repo`] at `<root>/repo` plus `branches`, all at the initial commit.
    fn repo_with_branches(root: &Path, branches: &[&str]) -> std::path::PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        for b in branches {
            git(&repo, &["branch", b]);
        }
        repo
    }

    fn tip_of(repo: &Path, branch: &str) -> String {
        let out = Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "rev-parse", &format!("refs/heads/{branch}")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn scan_classifies_branches_and_skips_gates_without_gh() {
        let tmp = TempDir::new().unwrap();
        let repo = repo_with_branches(
            tmp.path(),
            &["development", "merged-ok", "feat/x", "open-pr", "no-pr", "other"],
        );
        let initial = tip_of(&repo, "main");
        // One commit beyond what the (stubbed) PR merged.
        git(&repo, &["switch", "-c", "merged-ahead"]);
        std::fs::write(repo.join("more.txt"), "x").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "beyond"]);
        let wt = tmp.path().join("wt");
        git(&repo, &["worktree", "add", wt.to_str().unwrap(), "-b", "wt-branch"]);
        git(&repo, &["switch", "other"]); // checked out in the main repo itself
        let gh = tmp.path().join("gh");
        write_stub(
            &gh,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$4\" >> \"$0.args\"\ncase \"$4\" in\n  merged-ok|feat/x|wt-branch|other) printf 'MERGED\\n%s\\n7\\n' \"$(git rev-parse \"refs/heads/$4\")\"; exit 0 ;;\n  merged-ahead) printf 'MERGED\\n{initial}\\n8\\n'; exit 0 ;;\n  open-pr) printf 'OPEN\\n%s\\n9\\n' \"$(git rev-parse \"refs/heads/$4\")\"; exit 0 ;;\n  no-pr) printf 'NONE\\n\\n\\n'; exit 0 ;;\nesac\nexit 1\n"
            ),
        );

        let verdicts = scan_merged_branches_with(
            repo.to_str().unwrap(),
            &["development".to_string()],
            gh.to_str().unwrap(),
        )
        .unwrap();
        let of = |name: &str| {
            verdicts.iter().find(|v| v.branch == name).unwrap_or_else(|| panic!("{name} scanned"))
        };
        assert_eq!(verdicts.len(), 9);
        assert_eq!(of("merged-ok").verdict, Verdict::Delete);
        assert_eq!(of("merged-ok").pr, Some(7));
        assert_eq!(of("merged-ok").tip, initial);
        assert_eq!(of("feat/x").verdict, Verdict::Delete, "refs/heads/ stripped, slash kept");
        assert_eq!(of("merged-ahead").verdict, Verdict::Skip("commits beyond the merged PR".into()));
        assert_eq!(of("merged-ahead").pr, Some(8), "the PR number rides along with a skip");
        assert_eq!(of("open-pr").verdict, Verdict::Skip("PR not merged (OPEN)".into()));
        assert_eq!(of("no-pr").verdict, Verdict::Skip("no PR".into()));
        assert_eq!(of("no-pr").pr, None);
        assert_eq!(of("development").verdict, Verdict::Skip("protected branch".into()));
        assert_eq!(of("main").verdict, Verdict::Skip("default branch".into()));
        match &of("wt-branch").verdict {
            Verdict::CheckedOut { worktree } => assert!(worktree.ends_with("/wt"), "{worktree}"),
            v => panic!("wt-branch: {v:?}"),
        }
        let repo_real = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(
            of("other").verdict,
            Verdict::CheckedOut { worktree: repo_real.to_str().unwrap().to_string() },
            "the main checkout counts, at the realpath git reports"
        );
        // Gated names never reach gh; everything else is asked exactly once.
        let mut asked: Vec<String> = std::fs::read_to_string(format!("{}.args", gh.display()))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        asked.sort();
        assert_eq!(
            asked,
            ["feat/x", "merged-ahead", "merged-ok", "no-pr", "open-pr", "other", "wt-branch"]
        );
    }

    #[test]
    fn scan_aborts_when_gh_missing() {
        let tmp = TempDir::new().unwrap();
        let repo = repo_with_branches(tmp.path(), &["feat"]);
        let err = scan_merged_branches_with(
            repo.to_str().unwrap(),
            &[],
            "/nonexistent/definitely/not/gh",
        )
        .unwrap_err();
        assert!(err.contains("gh CLI not found"), "the first gh failure aborts: {err}");
    }

    #[test]
    fn delete_branches_refuses_every_gate_and_deletes_the_rest() {
        let tmp = TempDir::new().unwrap();
        let repo = repo_with_branches(tmp.path(), &["a", "b", "development"]);
        let wt = tmp.path().join("wt");
        git(&repo, &["worktree", "add", wt.to_str().unwrap(), "-b", "wt-branch"]);
        let sha = tip_of(&repo, "main");
        let input = [
            ("a".to_string(), sha.clone()),
            ("b".to_string(), "0".repeat(40)), // stale tip
            ("main".to_string(), sha.clone()),
            ("x..y".to_string(), sha.clone()),
            ("development".to_string(), sha.clone()),
            ("wt-branch".to_string(), sha.clone()),
        ];
        let results =
            delete_branches(repo.to_str().unwrap(), &input, &["development".to_string()]);
        let names: Vec<&str> = results.iter().map(|(b, _)| b.as_str()).collect();
        assert_eq!(names, ["a", "b", "main", "x..y", "development", "wt-branch"], "input order");
        assert_eq!(results[0].1, Ok(()));
        assert_eq!(results[1].1, Err("moved since scan".to_string()));
        assert!(results[2].1.as_ref().unwrap_err().contains("default branch"));
        assert!(results[3].1.as_ref().unwrap_err().contains("malformed"));
        assert!(results[4].1.as_ref().unwrap_err().contains("protected branch"));
        assert!(results[5].1.is_err(), "git refuses a checked-out branch");
        assert!(!branch_exists(&repo, "a"), "a deleted");
        for survivor in ["b", "main", "development", "wt-branch"] {
            assert!(branch_exists(&repo, survivor), "{survivor} survives");
        }
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
