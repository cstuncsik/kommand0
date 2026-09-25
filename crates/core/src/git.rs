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
//!
//! [`pr_statuses`] batches each workspace's PR/CI state out of `gh pr list`, and
//! [`issue_branch`] resolves a GitHub issue reference to the branch GitHub links
//! to it (creating and linking one if there is none yet). Both are network calls
//! with the same off-the-UI-thread contract.
//!
//! Every `gh` invocation goes through [`run_gh`], which pins the environment
//! non-interactive and bounds the call; nothing here shells out to `gh` directly.

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

/// Wall-clock bound for a subprocess that talks to the network (`gh`, `git
/// fetch`). Generous: a slow GraphQL query is fine, this only trips on a true
/// hang.
const NET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Collect a spawned child's output, giving up after [`NET_TIMEOUT`].
///
/// Reading happens on a helper thread so the pipes can't deadlock; if it outruns
/// the deadline we abandon the child (it has its own network timeouts, and the OS
/// reaps it on exit) rather than block indefinitely. A non-zero exit is still
/// `Ok`: that's the caller's to inspect.
fn wait_bounded(child: std::process::Child) -> std::io::Result<std::process::Output> {
    wait_bounded_in(child, NET_TIMEOUT)
}

/// [`wait_bounded`] with an explicit deadline, so the give-up path is testable
/// without a 20-second test.
fn wait_bounded_in(
    child: std::process::Child,
    deadline: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(deadline) {
        Ok(out) => out,
        Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out")),
    }
}

/// The ssh command git would have used for `repo_dir`, with batch mode appended:
/// `GIT_TERMINAL_PROMPT` and a null stdin do NOT stop ssh asking for a key
/// passphrase on /dev/tty, which from the TUI's worker thread would write into
/// the alt-screen and outlive the timeout.
///
/// Appends rather than clobbers, and follows git's OWN precedence:
/// `GIT_SSH_COMMAND`, else `core.sshCommand`, else plain ssh. Setting the
/// environment variable overrides `core.sshCommand` (see git-config(1)), so
/// reading only the environment would silently swap out a repo-scoped identity
/// (`core.sshCommand = ssh -i ~/.ssh/id_work`, the usual multi-account setup)
/// for the default key — and on the issue path that failure lands AFTER
/// `gh issue develop` has already written the branch to origin, with every
/// retry failing the same way.
///
/// ssh takes the FIRST value of a repeated option, so someone who has already
/// put an explicit `-oBatchMode=no` in their own command keeps it, and their
/// choice to be prompted stands.
fn batch_ssh_command(repo_dir: &str) -> String {
    let env = std::env::var("GIT_SSH_COMMAND").ok();
    // Only read the config when the environment doesn't already decide it.
    let cfg = env
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .is_none()
        .then(|| git_config_value(repo_dir, "core.sshCommand"))
        .flatten();
    ssh_command_with_batch_mode(env.as_deref(), cfg.as_deref())
}

/// The precedence rule itself, pure so it can be tabled: a non-blank
/// `GIT_SSH_COMMAND` wins, else `core.sshCommand`, else plain ssh. An empty
/// environment variable is treated as absent — git would die on it
/// (`error: cannot run :`), and repairing it beats propagating it.
fn ssh_command_with_batch_mode(env: Option<&str>, cfg: Option<&str>) -> String {
    let base = env.filter(|v| !v.trim().is_empty()).or(cfg);
    format!("{} -oBatchMode=yes", base.unwrap_or("ssh"))
}

/// A single git config value for `repo_dir`, or `None` when unset (or git
/// couldn't be run). Panic-free: this is called off the UI thread.
fn git_config_value(repo_dir: &str, key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", repo_dir, "config", "--get", key])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Run `gh <args>` in `cwd`, non-interactively (no prompts, no tty read, no
/// pager, no update notifier). Bounded by a wall-clock timeout because `gh` is a
/// network call: a caller off the UI thread guards a latch on this returning, and
/// gh wedged on the network (proxy black-hole, hung TLS) must not pin it forever.
fn run_gh(gh_bin: &str, cwd: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
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
            // Keep the output machine-shaped: GH_FORCE_TTY would add table
            // headers and ANSI colour to the rows callers parse.
            .env("GH_FORCE_TTY", "")
            .env("CLICOLOR_FORCE", "0")
            // GH_REPO retargets gh at another repo entirely, overriding the one
            // it would infer from the remotes, so a stray one in the environment
            // must never decide which repo we read a PR from or create a branch
            // on. (An explicit `--repo` does win over it, but only the issue
            // path passes one.) An empty value reads as unset.
            .env("GH_REPO", "")
            // `gh issue develop` runs its own `git fetch`, which inherits this
            // environment: without it an encrypted ssh key that isn't in the
            // agent blocks on a /dev/tty prompt, pinning a TUI worker thread
            // past the timeout.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", batch_ssh_command(cwd))
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
        return wait_bounded(child);
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
    // workspace name but an adopted ref could carry one. `is_valid_branch_name`
    // rejects the rest of the shapes git can reinterpret too; every one of them
    // is already illegal as a branch, so nothing git would accept is lost here.
    if !is_valid_branch_name(branch) {
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

/// A parsed GitHub issue reference.
struct ParsedRef<'a> {
    /// The issue number, digits only.
    number: &'a str,
    /// `(host, "owner/repo")`, lowercased, when the ref was a URL. `None` for
    /// `123` / `#123`. A URL names a repo that may not be THIS one, so the
    /// resolver must gate it against `origin` before acting on it.
    target: Option<(String, String)>,
}

fn is_all_digits(t: &str) -> bool {
    !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())
}

/// Drop an explicit `:<port>` from an SSH authority, where the port is a
/// transport detail rather than part of the GitHub host (GitHub documents
/// `ssh://git@ssh.github.com:443/o/r` for SSH over 443). An HTTP(S) authority
/// keeps its port: see [`parse_url_head`].
fn strip_ssh_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((h, port)) if is_all_digits(port) => h,
        _ => host,
    }
}

/// `(host, "owner/repo")` from the `[scheme://]host/owner/repo` head of a GitHub
/// web URL, lowercased.
///
/// The port is KEPT. `gh --repo host:port/owner/repo` both parses and dials that
/// port (verified against gh 2.101: it posted to `https://127.0.0.1:18443/api/graphql`),
/// so dropping it would silently point a GHES served off a non-default port at 443.
fn parse_url_head(head: &str) -> Option<(String, String)> {
    let head = head.split_once("://").map(|(_, r)| r).unwrap_or(head);
    let mut parts = head.split('/');
    let (host, owner, repo) = (parts.next()?, parts.next()?, parts.next()?);
    // Exactly host/owner/repo, no empty segment, and no userinfo:
    // `github.com@evil.host/...` reads as github.com but dials evil.host, and a
    // `user:token@` URL must never be echoed into an error or handed to a
    // subprocess. The emptiness checks are load-bearing: without them
    // `https:///o/r/issues/1` and `https://github.com//r/issues/1` are accepted.
    if parts.next().is_some()
        || host.contains('@')
        || host.is_empty()
        || owner.is_empty()
        || repo.is_empty()
    {
        return None;
    }
    Some((
        host.to_ascii_lowercase(),
        format!("{}/{}", owner.to_ascii_lowercase(), repo.to_ascii_lowercase()),
    ))
}

/// `(host, "owner/repo")` of the repo a `gh issue develop` URL points at: the
/// second field of a `--list` row, or the `…/tree/<branch>` line the create call
/// prints. `None` when there is no `/tree/` URL to read.
/// Positional, not `rsplit("/tree/")`: a branch name may itself contain
/// `/tree/`, and rsplitting on the last one then yields an over-long head that
/// parses as nothing, silently skipping the check. Taking the first four
/// segments and requiring the fourth to be `tree` is also correct for a repo
/// literally named `tree`, which is why rsplit was there in the first place.
fn linked_branch_repo(url: &str) -> Option<(String, String)> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let mut parts = rest.split('/');
    let (host, owner, repo, tree) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if tree != "tree" {
        return None;
    }
    parse_url_head(&format!("{host}/{owner}/{repo}"))
}

fn parse_issue_ref(s: &str) -> Option<ParsedRef<'_>> {
    let bare = s.strip_prefix('#').unwrap_or(s);
    if is_all_digits(bare) {
        return Some(ParsedRef { number: bare, target: None });
    }
    // URL shape: [scheme://]<host>/<owner>/<repo>/issues/<number>[/][?..|#..]
    let s = s.split(['?', '#']).next()?;
    let s = s.strip_suffix('/').unwrap_or(s);
    // rsplit: the LAST `/issues/` is the separator, so an owner or repo
    // literally named `issues` doesn't shadow it.
    let (head, number) = s.rsplit_once("/issues/")?;
    if !is_all_digits(number) {
        return None;
    }
    Some(ParsedRef { number, target: Some(parse_url_head(head)?) })
}

/// Whether `s` is a GitHub issue reference rather than a workspace name: a bare
/// number (`123`), `#123`, or an issue URL (any host, scheme optional, trailing
/// slash / `?query` / `#fragment` tolerated). Callers pass already-trimmed
/// input: the TUI modal trims on submit, and a CLI positional with stray
/// whitespace is a name, not a ref.
pub fn is_issue_ref(s: &str) -> bool {
    parse_issue_ref(s).is_some()
}

/// `(host, "owner/repo")` of `origin`'s URL, lowercased. `None` when there's no
/// origin, or its URL isn't `host` + `owner/repo` (a local-path remote, which is
/// what most tests use). Deliberately never returns or logs the raw URL: it can
/// carry a token.
///
/// `git remote get-url` expands `url.<base>.insteadOf`, so a rewritten remote
/// resolves correctly; a bare ~/.ssh/config `Host` alias cannot be expanded by
/// anything and stays unusable here, as it already is for gh.
fn origin_slug(repo_dir: &str) -> Option<(String, String)> {
    let out = Command::new("git")
        .args(["-C", repo_dir, "remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout);
    let url = url.trim();
    let url = url.strip_suffix('/').unwrap_or(url);
    let url = url.strip_suffix(".git").unwrap_or(url);
    // `https://host/o/r`, `ssh://git@host[:port]/o/r`, and the scp shape
    // `git@host:o/r`.
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (Some(s.to_ascii_lowercase()), r),
        None => (None, url),
    };
    let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest); // drop userinfo
    let (host, path) = match &scheme {
        // With a scheme the path separator is always `/`, so a trailing
        // `:<digits>` on the authority is a port. Whether it survives depends on
        // the transport: an HTTP(S) port is the API endpoint gh must dial, while
        // an ssh port is transport-only (GitHub documents
        // `ssh://git@ssh.github.com:443/o/r` for SSH over 443) and would point
        // gh at a port that serves no API.
        Some(s) => {
            let (host, path) = rest.split_once('/')?;
            let host = if s == "http" || s == "https" { host } else { strip_ssh_port(host) };
            (host, path)
        }
        // In the scp shape `host:path` there is no port, so `git@host:2222/o/r`
        // is a PATH and must stay unparseable rather than pin gh to a repo the
        // user didn't name.
        None => rest.split_once([':', '/'])?,
    };
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if parts.next().is_some() || host.is_empty() || owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((
        host.to_ascii_lowercase(),
        format!("{}/{}", owner.to_ascii_lowercase(), repo.to_ascii_lowercase()),
    ))
}

/// The name of the first remote that isn't `origin`, if there is one.
fn other_remote(repo_dir: &str) -> Option<String> {
    let out = Command::new("git").args(["-C", repo_dir, "remote"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|r| !r.is_empty() && *r != "origin")
        .map(str::to_string)
}

/// Whether a branch name is safe to interpolate into a refspec or hand to
/// `git worktree add`. Shared by the issue resolver (names come from the remote)
/// and by [`cleanup_merged_workspace_with`] (names come from state, possibly
/// adopted). Not a full `git check-ref-format`: the point is that everything
/// reaching a refspec, a `-`-leading argv slot or `create_worktree_from_branch`
/// is boring. A `-evil` branch name really does create a junk tracking ref, and
/// `refs/heads/a..b` REINTERPRETS as a range.
fn is_valid_branch_name(branch: &str) -> bool {
    // `HEAD` is the dangerous one: fetching `+refs/heads/HEAD:refs/remotes/origin/HEAD`
    // writes THROUGH the `refs/remotes/origin/HEAD` symref, so local
    // `origin/<default>` silently moves to that branch's commit and every
    // ahead/behind and default-branch diff is skewed until the next full fetch.
    // git itself refuses to create it, so only the API can. `@` git WILL create,
    // but it is rev-parse shorthand for HEAD, so refuse it here too.
    !(branch.is_empty()
        || branch == "HEAD"
        || branch == "@"
        || branch.starts_with('-')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.contains("//")
        || branch.ends_with('.')
        || branch.contains("..")
        || branch.contains("@{")
        || branch.contains(['~', '^', ':', '?', '*', '[', '\\'])
        || branch.chars().any(|c| c.is_whitespace() || c.is_control()))
}

/// `create_worktree_from_branch` tries `refs/heads/<branch>` FIRST, so a
/// leftover local branch beats the tracking ref we just fetched. Deleting a
/// workspace leaves its local branch behind, so the leftover is routine and is
/// usually the user's own unpushed work: adopting it is RIGHT whenever it
/// already contains origin's tip. Refuse only when it is behind or genuinely
/// diverged, where adopting it would silently drop what origin (and the linked
/// branch) has.
fn refuse_diverged_local(repo_dir: &str, branch: &str) -> Result<(), String> {
    // No leftover branch, nothing to shadow the tracking ref we just fetched.
    // Settled here rather than by `merge-base`'s exit code: 128 means only
    // "couldn't answer" (an absent ref, but equally an unreadable object), so
    // reading it as "no local branch" would let a real one through unchecked.
    if !crate::worktree::verify_ref(repo_dir, &format!("refs/heads/{branch}")) {
        return Ok(());
    }
    // The branch is really there, so only a clean exit 0 clears it: origin's tip
    // is an ancestor of (or equal to) the local branch, and adopting it keeps
    // everything origin has. Everything else refuses, whether that is 1 (behind
    // or diverged), 128, or no code at all (spawn failed, killed by a signal).
    let contains_origin = Command::new("git")
        .args([
            "-C",
            repo_dir,
            "merge-base",
            "--is-ancestor",
            &format!("refs/remotes/origin/{branch}"),
            &format!("refs/heads/{branch}"),
        ])
        // Null both: a `fatal:` for a missing ref would otherwise be written
        // straight onto the TUI's alt screen from the worker thread.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.code() == Some(0))
        .unwrap_or(false);
    if !contains_origin {
        return Err(format!(
            "the local branch {branch} is behind or has diverged from origin/{branch}; \
             merge, rebase or rename it, then try again"
        ));
    }
    Ok(())
}

/// Fetch the linked branch, then refuse the two ways adopting it could pick the
/// wrong commit. Runs after the name gate: the branch reaches a refspec here.
fn prepare_linked_branch(repo_dir: &str, branch: &str, gh_bin: &str) -> Result<(), String> {
    fetch_origin_branch(repo_dir, branch, gh_bin)?;
    // `create_worktree_from_branch` resolves `refs/remotes/<ref>` BEFORE
    // `refs/remotes/origin/<ref>`, so a linked branch named `alice/fix` would
    // adopt remote `alice`'s `fix` instead. Only an existing ref shadows, hence
    // a lookup rather than a rule against `/`: the names GitHub generates are
    // full of slashes once a repo uses prefixes.
    if crate::worktree::verify_ref(repo_dir, &format!("refs/remotes/{branch}")) {
        return Err(format!(
            "the linked branch {branch} is shadowed by the remote-tracking ref \
             refs/remotes/{branch}; check out origin/{branch} explicitly instead"
        ));
    }
    refuse_diverged_local(repo_dir, branch)
}

/// Whether a configured `remote.origin.fetch` refspec already maps
/// `refs/heads/<branch>` into the local ref space. False for a `--single-branch`
/// clone asked about any other branch.
fn origin_fetches(repo_dir: &str, branch: &str) -> bool {
    let Ok(out) = Command::new("git")
        .args(["-C", repo_dir, "config", "--get-all", "remote.origin.fetch"])
        .output()
    else {
        return false;
    };
    let want = format!("refs/heads/{branch}");
    String::from_utf8_lossy(&out.stdout).lines().any(|spec| {
        let src = spec.trim().trim_start_matches('+').split(':').next().unwrap_or_default();
        match src.split_once('*') {
            // The length term stops `pre` and `post` overlapping in `want`:
            // without it `+refs/heads/a*a:…` claims to cover `refs/heads/a`,
            // and the widening that the branch actually needs is skipped.
            Some((pre, post)) => {
                want.len() >= pre.len() + post.len()
                    && want.starts_with(pre)
                    && want.ends_with(post)
            }
            None => src == want,
        }
    })
}

/// Single-quote `s` as one shell word. Git runs a `!`-prefixed credential helper
/// through `sh`, and kommand0's own state directory can contain a space.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Refresh `refs/remotes/origin/<branch>` from origin, bounded and
/// non-interactive. Explicit refspec: a bare `git fetch origin <branch>` leaves
/// the tracking ref to the configured refspec, and a STALE tracking ref passes
/// `verify_ref` and silently yields a worktree behind origin.
fn fetch_origin_branch(repo_dir: &str, branch: &str, gh_bin: &str) -> Result<(), String> {
    let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    // A `--single-branch` clone maps only its own branch, and `worktree add
    // --track` then dies with "not a branch" on the tracking ref we just
    // fetched. Widen the remote first, unless a refspec already covers it (a
    // second `set-branches --add` would duplicate the entry). Best-effort: the
    // fetch may still work, and its own error is the one worth showing.
    if !origin_fetches(repo_dir, branch) {
        match Command::new("git")
            .args(["-C", repo_dir, "remote", "set-branches", "--add", "origin", branch])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(st) if st.success() => {}
            Ok(st) => tracing::warn!(branch, "couldn't widen remote.origin.fetch ({st})"),
            Err(e) => tracing::warn!(branch, "couldn't widen remote.origin.fetch ({e})"),
        }
    }
    let helper = format!("credential.helper=!{} auth git-credential", shell_quote(gh_bin));
    let spawned = Command::new("git")
        // gh authenticates HTTPS through the credential helper it injects, so a
        // private origin + a bare GH_TOKEN fetches only with this. `-c` APPENDS
        // to the helper list, so a user's own helper still runs first, and it's
        // never consulted for an ssh or local origin.
        .args(["-C", repo_dir, "-c", &helper, "fetch", "origin", &refspec])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", batch_ssh_command(repo_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    match spawned.and_then(wait_bounded) {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!("couldn't fetch {branch} from origin: {}", last_line(&o.stderr))),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            Err("git fetch timed out: check your network and try again".to_string())
        }
        Err(e) => Err(format!("couldn't fetch {branch} from origin: {e}")),
    }
}

/// Map a `run_gh` spawn/timeout failure onto its user-facing message. A non-zero
/// exit isn't one of these: the caller reads `status` itself.
///
/// Only `NotFound` earns the "install it" hint. A non-executable binary
/// (EACCES) or an exhausted ETXTBSY retry arrive here too, and telling that
/// user to install the gh they already have sends them down the wrong path. (A
/// deleted working directory is NOT one of these: `current_dir` on a missing
/// path also reports `NotFound`, so it still gets the install hint.)
fn gh_unavailable(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::TimedOut => {
            "gh timed out: check your network and try again".to_string()
        }
        std::io::ErrorKind::NotFound => {
            "gh CLI not found: install GitHub CLI to create a branch from an issue".to_string()
        }
        _ => format!("couldn't run gh: {e}"),
    }
}

/// Refuse a linked branch that lives in a repo other than the pinned one.
///
/// A linked branch is NOT necessarily in this repo: `gh issue develop
/// --branch-repo` creates one elsewhere, and GitHub's Development panel offers a
/// repository picker. `--list` reports only the branch NAME plus a URL, and the
/// name alone is indistinguishable from a branch of ours, so without this we
/// would fetch that name from `origin` and — whenever origin happens to have an
/// unrelated branch by the same name — silently check the user out onto the
/// wrong one, where their PR would never close the issue.
///
/// Only enforced when a pin exists: with no `owner/repo` origin there is nothing
/// to compare against (and the caller has already refused that case whenever a
/// second remote could be chosen). A URL we can't parse is left alone rather
/// than treated as foreign, so a change in gh's output shape degrades to today's
/// behaviour instead of blocking every lookup.
fn refuse_a_foreign_linked_branch(
    pin: &Option<String>,
    url: Option<&str>,
    number: &str,
    branch: &str,
) -> Result<(), String> {
    let Some(pin) = pin else { return Ok(()) };
    let Some((host, slug)) = url.and_then(linked_branch_repo) else {
        return Ok(());
    };
    if format!("{host}/{slug}") != *pin {
        return Err(format!(
            "issue {number}'s linked branch {branch} lives in {host}/{slug}, not {pin}; \
             check that branch out from its own repo instead"
        ));
    }
    Ok(())
}

/// The branch GitHub links to an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueBranch {
    pub branch: String,
    /// The branch already existed and was adopted, rather than created now.
    pub reused: bool,
}

/// Resolve a GitHub issue reference to the branch GitHub links to it, via
/// `gh issue develop`: an existing linked branch is reused, otherwise a new one
/// is created on `origin` and linked (a remote write).
///
/// On `Ok` the branch exists on `origin`, `refs/remotes/origin/<branch>` has
/// just been fetched from it, and no local branch of that name is missing
/// origin's tip, so `create_worktree_from_branch` adopts either the linked
/// branch or a local branch that already contains it.
///
/// Makes network calls; call it off the UI thread.
pub fn issue_branch(repo_dir: &str, issue_ref: &str) -> Result<IssueBranch, String> {
    issue_branch_with(repo_dir, issue_ref, &gh_bin())
}

fn issue_branch_with(repo_dir: &str, issue_ref: &str, gh_bin: &str) -> Result<IssueBranch, String> {
    // ONE origin lookup, used for both the URL gate and the --repo pin.
    let origin = origin_slug(repo_dir);
    let parsed = parse_issue_ref(issue_ref).ok_or_else(|| {
        "not a GitHub issue reference (expected 123, #123, or an issue URL)".to_string()
    })?;
    if let Some((host, slug)) = &parsed.target {
        // A pasted URL can name ANY repo on ANY host. Un-gated, an upstream or
        // third-party URL makes `gh issue develop` create the linked branch on
        // THAT repo (silently, not this workspace's), and an attacker-chosen
        // host gets dialled with whatever GH_* credentials run_gh inherits.
        // Compare against origin: local, no network, no hardcoded github.com,
        // so GHES still works.
        let Some((o_host, o_slug)) = &origin else {
            return Err(format!(
                "can't check the issue URL ({host}/{slug}) against origin: origin isn't an \
                 owner/repo URL. Pass the issue number instead"
            ));
        };
        if (o_host, o_slug) != (host, slug) {
            return Err(format!(
                "that issue URL points at {host}/{slug}, but this repo's origin is {o_host}/{o_slug}"
            ));
        }
    }
    // Past the gate the URL and the number denote the same issue by
    // construction, so gh is handed digits only: no URL shapes to probe, no
    // credential echo, no arbitrary host reach.
    let number = parsed.number;

    // Pin every gh call to origin. Unpinned, gh scores ALL remotes and
    // `upstream` wins over `origin`, so in a fork checkout an issue ref would
    // create and link a branch on UPSTREAM, and the worktree add would then die
    // with "branch not found" after an irreversible remote write. Derived from
    // `origin` ONLY, never from the pasted URL; the host prefix keeps GHES
    // working. A local-path origin (or none at all) means no pin, and gh infers
    // as it does today.
    let pin = origin.as_ref().map(|(h, s)| format!("{h}/{s}"));
    // Unpinned, gh only infers safely while `origin` is its sole candidate, so
    // bail before the first call rather than after the remote write. Origin's URL
    // stays out of the message: it can carry a token.
    if pin.is_none()
        && let Some(other) = other_remote(repo_dir)
    {
        return Err(format!(
            "can't tell gh which repo to use: origin isn't an owner/repo URL, and gh may \
             pick the remote {other} instead. Point origin at the repo you mean, or check \
             the linked branch out by name"
        ));
    }

    let mut args: Vec<&str> = vec!["issue", "develop", "--list"];
    if let Some(p) = &pin {
        args.extend(["--repo", p.as_str()]);
    }
    args.extend(["--", number]);
    let out = run_gh(gh_bin, repo_dir, &args).map_err(|e| gh_unavailable(&e))?;
    if !out.status.success() {
        // A PR number, an unauthenticated gh, an issue that isn't on origin:
        // all abort here, so the create call is never reached.
        return Err(format!("couldn't look up issue {number}: {}", last_line(&out.stderr)));
    }
    // Piped `--list` output is bare `branch<TAB>url` rows, no header. Take the
    // first field: refnames can't contain whitespace.
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if rows.len() > 1 {
        let names: Vec<&str> = rows.iter().filter_map(|r| r.split_whitespace().next()).collect();
        return Err(format!(
            "issue {number} has {} linked branches ({}): check one out explicitly",
            rows.len(),
            names.join(", ")
        ));
    }
    if let Some(row) = rows.first() {
        let branch = row.split_whitespace().next().unwrap_or("").to_string();
        // `{:?}` escapes control characters.
        if !is_valid_branch_name(&branch) {
            return Err(format!("gh returned an unusable branch name ({branch:?})"));
        }
        refuse_a_foreign_linked_branch(&pin, row.split_whitespace().nth(1), number, &branch)?;
        prepare_linked_branch(repo_dir, &branch, gh_bin)?;
        return Ok(IssueBranch { branch, reused: true });
    }

    // No linked branch yet: ask GitHub to create one. No `--base`, linked
    // branches start from the repo's default branch.
    let mut args: Vec<&str> = vec!["issue", "develop"];
    if let Some(p) = &pin {
        args.extend(["--repo", p.as_str()]);
    }
    args.extend(["--", number]);
    let out = run_gh(gh_bin, repo_dir, &args).map_err(|e| gh_unavailable(&e))?;
    if !out.status.success() {
        return Err(format!(
            "couldn't create a branch for issue {number}: {}",
            last_line(&out.stderr)
        ));
    }
    // stdout's last non-empty line is `github.com/<owner>/<repo>/tree/<branch>`
    // (no scheme). rsplit: the LAST `/tree/` is the separator, so an owner or
    // repo named `tree` parses correctly. The raw line goes into the failure
    // message: after a successful create, it is the operator's only record of
    // what gh just made on origin.
    let line = last_line(&out.stdout);
    let branch = line.rsplit_once("/tree/").map(|(_, b)| b.trim().to_string()).unwrap_or_default();
    if !is_valid_branch_name(&branch) {
        return Err(format!(
            "gh returned an unusable branch name ({branch:?}); last output line was {line:?}"
        ));
    }
    // We never pass `--branch-repo`, so this should always be the pinned repo.
    // Only WARN if it isn't: gh has already created the branch on the remote by
    // now, and refusing here would dead-end the issue (the retry takes the
    // `--list` path and refuses again). A repo renamed on GitHub since the
    // remote URL was set is enough to trip it, with nothing wrong.
    if let Err(e) = refuse_a_foreign_linked_branch(&pin, Some(line.as_str()), number, &branch) {
        tracing::warn!("{e}");
    }
    prepare_linked_branch(repo_dir, &branch, gh_bin)?;
    Ok(IssueBranch { branch, reused: false })
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
        // A developer with global `commit.gpgsign = true` would otherwise have
        // every commit here block on a signing agent.
        git(dir, &["config", "commit.gpgsign", "false"]);
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
        git(&wp, &["config", "commit.gpgsign", "false"]);
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
        git(tmp.path(), &["config", "commit.gpgsign", "false"]);
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
        git(&repo, &["config", "commit.gpgsign", "false"]);
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
        git(&wt, &["config", "commit.gpgsign", "false"]);
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
        git(&origin, &["config", "commit.gpgsign", "false"]);
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

    // --- issue_branch ---

    /// Every gh stub below starts with this line: ONE line per invocation
    /// appended to `<stub>.args`, holding the whole argv plus the three env vars
    /// `run_gh` must neutralise, so every argv assertion ends in ` [][0][]`.
    /// `${VAR-UNSET}` distinguishes "unset" from "set to empty", so dropping an
    /// `.env()` call fails the assertion. "gh was never called" = the file is
    /// ABSENT.
    const GH_ARGS_LINE: &str = "printf '%s [%s][%s][%s]\\n' \"$*\" \
        \"${GH_FORCE_TTY-UNSET}\" \"${CLICOLOR_FORCE-UNSET}\" \"${GH_REPO-UNSET}\" >> \"$0.args\"\n";

    /// A `gh issue develop` stub: prints `list` for any argv containing
    /// `--list`, `create` otherwise. Both go through `printf '%s'` from a
    /// single-quoted literal, so a real TAB stays a tab and a `%` in a URL can't
    /// be eaten.
    fn gh_issue_stub(path: &Path, list: &str, create: &str) {
        write_stub(
            path,
            &format!(
                "#!/bin/sh\n{GH_ARGS_LINE}case \"$*\" in\n  *--list*) printf '%s' '{list}' ;;\n  *) printf '%s' '{create}' ;;\nesac\nexit 0\n"
            ),
        );
    }

    /// A real `origin` plus a clone of it, following
    /// `cleanup_refuses_default_branch_via_origin_head`. Origin is a local path,
    /// so `origin_slug` is `None` and no `--repo` lands in the argv (pinning has
    /// its own test). Nothing ever pushes into it, so its being non-bare is fine.
    fn issue_fixture(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let origin = tmp.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        init_repo(&origin);
        let clone = tmp.join("clone");
        git(tmp, &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()]);
        // A clone inherits no commit identity, and several rows commit locally.
        git(&clone, &["config", "user.email", "t@t"]);
        git(&clone, &["config", "user.name", "t"]);
        git(&clone, &["config", "commit.gpgsign", "false"]);
        (origin, clone)
    }

    fn rev_parse(dir: &Path, rev: &str) -> String {
        let out = Command::new("git")
            .args(["-C", dir.to_str().unwrap(), "rev-parse", rev])
            .output()
            .unwrap();
        assert!(out.status.success(), "rev-parse {rev} failed in {}", dir.display());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The stub's recorded argv lines, empty when gh was never invoked.
    fn gh_args(gh: &Path) -> Vec<String> {
        std::fs::read_to_string(format!("{}.args", gh.display()))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn issue_ref_detection() {
        let cases: &[(&str, bool)] = &[
            ("123", true),
            ("#123", true),
            ("https://github.com/o/r/issues/123", true),
            ("http://github.com/o/r/issues/123", true),
            ("github.com/o/r/issues/123", true),
            ("https://github.com/o/r/issues/123/", true),
            ("https://github.com/o/r/issues/123?x=1", true),
            ("https://github.com/o/r/issues/123#issuecomment-4", true),
            // Owner and repo both named `issues`: only the LAST `/issues/` may
            // separate, or this reads as a 1-segment head and is rejected.
            ("https://github.com/issues/issues/issues/7", true),
            // Userinfo in the authority: reads as github.com, dials evil.host.
            ("github.com@evil.host/o/r/issues/1", false),
            ("https:///o/r/issues/1", false),
            ("https://github.com//r/issues/1", false),
            ("https://github.com/o//issues/1", false),
            ("https://github.com/o/r/issues/r/issues/123", false),
            ("https://github.com/o/r/issues/abc", false),
            ("feat/123", false),
            ("123-fix", false),
            ("", false),
            ("abc", false),
            ("#", false),
            ("   ", false),
            ("12 3", false),
        ];
        for (input, want) in cases {
            assert_eq!(is_issue_ref(input), *want, "{input:?}");
        }
    }

    #[test]
    fn branch_names_git_could_reinterpret_are_rejected() {
        // Slashes are fine: a linked branch is routinely `feat/123-x`, and the
        // reuse path has to accept whatever name the existing branch carries.
        let cases: &[(&str, bool)] = &[
            ("123-fix-it", true),
            ("kommand0/legacy", true),
            // Accept side: shapes GitHub really generates. An interior dot is
            // one character away from the `a.`/`a..` rejections, and a `HEAD`
            // PREFIX must not be caught by the `HEAD` rule.
            ("42-bump-to-v1.2.3", true),
            ("123-Fix-It", true),
            ("release/1.0", true),
            ("HEADer", true),
            ("", false),
            // Fetching `+refs/heads/HEAD:refs/remotes/origin/HEAD` writes
            // THROUGH the symref and moves local `origin/<default>`.
            ("HEAD", false),
            ("@", false),
            ("-evil", false),
            ("a..b", false),
            ("a/", false),
            ("/a", false),
            ("a//b", false),
            ("a.", false),
            ("a@{0}", false),
            ("a:b", false),
            ("a^", false),
            ("a~1", false),
            ("a?b", false),
            ("a*b", false),
            ("a[b", false),
            ("a\\b", false),
            ("a b", false),
            ("a\u{1}b", false),
        ];
        for (input, want) in cases {
            assert_eq!(is_valid_branch_name(input), *want, "{input:?}");
        }
    }

    #[test]
    fn a_gh_path_survives_the_shell_that_runs_the_credential_helper() {
        // git runs a `!`-prefixed helper through sh, so the path has to come out
        // the other side byte for byte.
        for raw in ["/plain/gh", "/with space/gh", "/quote'and$dollar/gh"] {
            let out = Command::new("sh")
                .args(["-c", &format!("printf '%s' {}", shell_quote(raw))])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), raw);
        }
    }

    #[test]
    fn issue_branch_pins_gh_to_origin_and_gates_urls() {
        const PIN7: &str = "issue develop --list --repo github.com/o/r -- 7 [][0][]";
        let url7 = "https://github.com/o/r/issues/7";
        let mismatch = "points at github.com/other/repo, but this repo's origin is github.com/o/r";
        // (origin url, None means no `remote add` at all; issue ref; expected
        // `.args` lines, empty means gh was never invoked; expected error)
        let rows: &[(Option<&str>, &str, &[&str], &str)] = &[
            // A bare number must be pinned too: every other accept row is a
            // URL, so without this one the pin could live in the URL branch only.
            (
                Some("https://github.com/o/r.git"),
                "123",
                &["issue develop --list --repo github.com/o/r -- 123 [][0][]"],
                "MARKER-NOPE",
            ),
            (Some("https://github.com/o/r.git"), url7, &[PIN7], "MARKER-NOPE"),
            (Some("git@github.com:o/r.git"), url7, &[PIN7], "MARKER-NOPE"),
            (Some("ssh://git@github.com/o/r.git"), url7, &[PIN7], "MARKER-NOPE"),
            (Some("https://github.com/o/r/"), url7, &[PIN7], "MARKER-NOPE"),
            (Some("https://GitHub.com/O/R.git"), url7, &[PIN7], "MARKER-NOPE"),
            (
                Some("https://github.com/Owner/Repo.git"),
                "https://GitHub.com/Owner/Repo/issues/7",
                &["issue develop --list --repo github.com/owner/repo -- 7 [][0][]"],
                "MARKER-NOPE",
            ),
            // GHES: the host prefix is what keeps a non-github.com install
            // working, so the pin must carry it.
            (
                Some("https://ghe.corp/o/r.git"),
                "https://ghe.corp/o/r/issues/7",
                &["issue develop --list --repo ghe.corp/o/r -- 7 [][0][]"],
                "MARKER-NOPE",
            ),
            // `#123` is handed to gh as bare digits: the `#` is a fragment
            // marker in a URL and a comment character in a shell.
            (
                Some("https://github.com/o/r.git"),
                "#123",
                &["issue develop --list --repo github.com/o/r -- 123 [][0][]"],
                "MARKER-NOPE",
            ),
            (
                Some("https://github.com/o/r.git"),
                "https://github.com/other/repo/issues/7",
                &[],
                mismatch,
            ),
            (None, url7, &[], "Pass the issue number instead"),
            // An explicit port belongs to the host (GitHub documents SSH over
            // 443), so the pin survives it.
            (
                Some("ssh://git@github.com:443/o/r.git"),
                url7,
                &[PIN7],
                "MARKER-NOPE",
            ),
            // A GHES on a nonstandard port gates its own issue URL through, and
            // the port SURVIVES into the pin: `gh --repo host:port/o/r` dials
            // that port (verified against gh 2.101), so dropping it would send
            // the lookup to 443 instead.
            (
                Some("https://ghe.corp:8443/o/r.git"),
                "https://ghe.corp:8443/o/r/issues/7",
                &["issue develop --list --repo ghe.corp:8443/o/r -- 7 [][0][]"],
                "MARKER-NOPE",
            ),
            // An http(s) port is part of the API endpoint, an ssh one is
            // transport-only: `ssh://…:2222` must still pin the bare host, or gh
            // would dial an endpoint that serves no API.
            (
                Some("ssh://git@ghe.corp:2222/o/r.git"),
                "https://ghe.corp/o/r/issues/7",
                &["issue develop --list --repo ghe.corp/o/r -- 7 [][0][]"],
                "MARKER-NOPE",
            ),
            // The port is part of the identity, so a portless URL against a
            // ported origin is a different host and must not gate through.
            (
                Some("https://ghe.corp:8443/o/r.git"),
                "https://ghe.corp/o/r/issues/7",
                &[],
                "but this repo's origin is ghe.corp:8443/o/r",
            ),
            // The scp shape has no port, so `:2222/o/r` is a PATH with three
            // segments: not an owner/repo URL, nothing to gate or pin.
            (Some("git@github.com:2222/o/r.git"), url7, &[], "Pass the issue number instead"),
            (
                Some("https://user:token@github.com/o/r.git"),
                "https://github.com/other/repo/issues/7",
                &[],
                mismatch,
            ),
        ];
        for (i, (origin, reference, want_args, want_err)) in rows.iter().enumerate() {
            let tmp = TempDir::new().unwrap();
            let repo = tmp.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]);
            if let Some(o) = origin {
                git(&repo, &["remote", "add", "origin", o]);
            }
            let gh = tmp.path().join("gh");
            // Records the argv, then fails before any network call or fetch.
            write_stub(
                &gh,
                &format!("#!/bin/sh\n{GH_ARGS_LINE}printf 'MARKER-NOPE\\n' >&2\nexit 1\n"),
            );
            let err = issue_branch_with(repo.to_str().unwrap(), reference, gh.to_str().unwrap())
                .unwrap_err();
            assert!(err.contains(want_err), "row {i}: expected {want_err:?}, got: {err}");
            assert!(!err.contains("token"), "row {i}: leaked the origin's credentials: {err}");
            assert_eq!(gh_args(&gh), *want_args, "row {i}: gh argv");
        }

        // The create call carries the pin too: it is the irreversible remote
        // write. `--list` succeeds with no rows, then the create fails before
        // the fetch, so this row needs no real origin either.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["remote", "add", "origin", "https://github.com/o/r.git"]);
        let gh = tmp.path().join("gh");
        write_stub(
            &gh,
            &format!(
                "#!/bin/sh\n{GH_ARGS_LINE}case \"$*\" in\n  *--list*) exit 0 ;;\n  *) printf 'MARKER-NOPE\\n' >&2; exit 1 ;;\nesac\n"
            ),
        );
        let err =
            issue_branch_with(repo.to_str().unwrap(), "123", gh.to_str().unwrap()).unwrap_err();
        assert!(err.contains("MARKER-NOPE"), "the create failure surfaces: {err}");
        assert_eq!(
            gh_args(&gh),
            [
                "issue develop --list --repo github.com/o/r -- 123 [][0][]",
                "issue develop --repo github.com/o/r -- 123 [][0][]",
            ]
        );
    }

    #[test]
    fn issue_branch_reuses_and_always_refetches_the_linked_branch() {
        // The clone carries the tracking ref at A; origin then moves the linked
        // branch on to B. A "fetch only if the ref is missing" implementation
        // leaves the worktree silently behind origin, so assert the OID.
        let tmp = TempDir::new().unwrap();
        let (origin, clone) = issue_fixture(tmp.path());
        git(&origin, &["branch", "123-linked", "HEAD"]);
        git(&clone, &["fetch", "origin"]);
        let a = rev_parse(&clone, "refs/remotes/origin/123-linked");
        std::fs::write(origin.join("b.txt"), "b").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-m", "b"]);
        git(&origin, &["branch", "-f", "123-linked", "HEAD"]);
        let b = rev_parse(&origin, "refs/heads/123-linked");
        assert_ne!(a, b, "origin really moved on");

        let gh = tmp.path().join("gh");
        gh_issue_stub(&gh, "123-linked\thttps://github.com/o/r/tree/123-linked\n", "");
        let got = issue_branch_with(clone.to_str().unwrap(), "123", gh.to_str().unwrap()).unwrap();
        assert_eq!(got, IssueBranch { branch: "123-linked".to_string(), reused: true });
        assert_eq!(
            rev_parse(&clone, "refs/remotes/origin/123-linked"),
            b,
            "the tracking ref was re-fetched, not left stale at A"
        );
        assert_eq!(gh_args(&gh), ["issue develop --list -- 123 [][0][]"]);
    }

    #[test]
    fn issue_branch_parses_the_created_branch_name() {
        // (the create stub's stdout, the branch it should yield or an error
        // fragment).
        let rows: &[(&str, Result<&str, &str>)] = &[
            ("github.com/o/r/tree/123-fix-it\n", Ok("123-fix-it")),
            // A slash is legal in a linked branch name, and common once a repo
            // uses prefixes.
            ("github.com/o/r/tree/feat/123-fix-it\n", Ok("feat/123-fix-it")),
            // The LAST `/tree/` separates, so a repo named `tree` still parses.
            ("github.com/o/tree/tree/123-fix-it\n", Ok("123-fix-it")),
            ("github.com/o/r/tree/-evil\n", Err("unusable branch name")),
            ("github.com/o/r/tree/a..b\n", Err("unusable branch name")),
            ("no tree url here\n", Err("no tree url here")),
        ];
        for (stdout, want) in rows {
            let tmp = TempDir::new().unwrap();
            let (origin, clone) = issue_fixture(tmp.path());
            // gh would have created this on origin; the Err rows never get here.
            if let Ok(branch) = want {
                git(&origin, &["branch", branch, "HEAD"]);
            }
            let gh = tmp.path().join("gh");
            gh_issue_stub(&gh, "", stdout);
            let got = issue_branch_with(clone.to_str().unwrap(), "123", gh.to_str().unwrap());
            match want {
                Ok(branch) => {
                    assert_eq!(
                        got,
                        Ok(IssueBranch { branch: (*branch).to_string(), reused: false }),
                        "{stdout:?}"
                    );
                    assert_eq!(
                        gh_args(&gh),
                        ["issue develop --list -- 123 [][0][]", "issue develop -- 123 [][0][]"],
                        "no --base, and the create call really ran"
                    );
                    assert_eq!(
                        rev_parse(&clone, &format!("refs/remotes/origin/{branch}")),
                        rev_parse(&origin, &format!("refs/heads/{branch}")),
                        "the create path fetches too"
                    );
                }
                Err(fragment) => {
                    let err = got.unwrap_err();
                    assert!(err.contains(fragment), "{stdout:?}: got {err}");
                }
            }
        }

        // The reuse path runs the same gate, so cover its call site too.
        let tmp = TempDir::new().unwrap();
        let (_origin, clone) = issue_fixture(tmp.path());
        let gh = tmp.path().join("gh");
        gh_issue_stub(&gh, "-evil\thttps://github.com/o/r/tree/-evil\n", "");
        let err =
            issue_branch_with(clone.to_str().unwrap(), "123", gh.to_str().unwrap()).unwrap_err();
        assert!(err.contains("unusable branch name"), "the reuse path gates too: {err}");
    }

    #[test]
    fn a_local_branch_blocks_only_when_it_is_behind_or_diverged() {
        // Origin's linked branch is at B (parent A). Only a local branch that
        // already contains B may be adopted.
        let cases =
            [("at origin", true), ("ahead", true), ("behind", false), ("diverged", false)];
        for (case, want_ok) in cases {
            let tmp = TempDir::new().unwrap();
            let (origin, clone) = issue_fixture(tmp.path());
            let a = rev_parse(&origin, "HEAD");
            std::fs::write(origin.join("b.txt"), "b").unwrap();
            git(&origin, &["add", "."]);
            git(&origin, &["commit", "-m", "b"]);
            let b = rev_parse(&origin, "HEAD");
            git(&origin, &["branch", "123-linked", &b]);
            // "behind" deliberately leaves the tracking ref absent: the refusal
            // can then only come from the fetch having run FIRST, which is the
            // whole reason the fetch is unconditional.
            if case != "behind" {
                git(&clone, &["fetch", "origin"]);
            }
            match case {
                "at origin" => git(&clone, &["branch", "123-linked", &b]),
                // The routine case: a deleted workspace left its branch behind
                // with unpushed work on top.
                "ahead" => {
                    git(&clone, &["switch", "-c", "123-linked", &b]);
                    std::fs::write(clone.join("c.txt"), "c").unwrap();
                    git(&clone, &["add", "."]);
                    git(&clone, &["commit", "-m", "unpushed"]);
                    git(&clone, &["switch", "main"]);
                }
                "behind" => git(&clone, &["branch", "123-linked", &a]),
                _ => {
                    std::fs::write(clone.join("d.txt"), "d").unwrap();
                    git(&clone, &["add", "."]);
                    git(&clone, &["commit", "-m", "elsewhere"]);
                    git(&clone, &["branch", "123-linked", "HEAD"]);
                }
            }
            let gh = tmp.path().join("gh");
            gh_issue_stub(&gh, "123-linked\thttps://github.com/o/r/tree/123-linked\n", "");
            let got = issue_branch_with(clone.to_str().unwrap(), "123", gh.to_str().unwrap());
            if want_ok {
                let want = IssueBranch { branch: "123-linked".to_string(), reused: true };
                assert_eq!(got, Ok(want), "{case}");
            } else {
                let err = got.unwrap_err();
                assert!(err.contains("origin/123-linked"), "{case}: {err}");
            }
        }
    }

    #[test]
    fn an_unpinnable_origin_next_to_another_remote_refuses_before_gh_runs() {
        // Unpinned, gh scores every remote and `upstream` beats `origin`, so in a
        // fork checkout the create would land on a repo the user never named.
        // Refuse while nothing has been written yet.
        for with_origin in [true, false] {
            let tmp = TempDir::new().unwrap();
            let repo = tmp.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]);
            if with_origin {
                // A local path: a real remote, but not an owner/repo URL.
                git(&repo, &["remote", "add", "origin", tmp.path().to_str().unwrap()]);
            }
            git(&repo, &["remote", "add", "upstream", "https://github.com/o/r.git"]);
            let gh = tmp.path().join("gh");
            write_stub(&gh, &format!("#!/bin/sh\n{GH_ARGS_LINE}exit 0\n"));
            let err = issue_branch_with(repo.to_str().unwrap(), "123", gh.to_str().unwrap())
                .unwrap_err();
            assert!(err.contains("upstream"), "names the remote gh might pick: {err}");
            assert!(gh_args(&gh).is_empty(), "origin {with_origin}: gh never ran");
        }
    }

    #[test]
    fn a_fork_checkout_with_a_pinnable_origin_is_not_refused() {
        // origin + upstream is exactly what the pin is for: gh is told the repo,
        // so there is nothing left to guess and the lookup must go through.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["remote", "add", "origin", "https://github.com/o/r.git"]);
        git(&repo, &["remote", "add", "upstream", "https://github.com/up/r.git"]);
        let gh = tmp.path().join("gh");
        write_stub(&gh, &format!("#!/bin/sh\n{GH_ARGS_LINE}printf 'MARKER-NOPE\\n' >&2\nexit 1\n"));
        let err =
            issue_branch_with(repo.to_str().unwrap(), "123", gh.to_str().unwrap()).unwrap_err();
        assert!(err.contains("MARKER-NOPE"), "gh ran and its own failure surfaced: {err}");
        assert_eq!(gh_args(&gh), ["issue develop --list --repo github.com/o/r -- 123 [][0][]"]);
    }

    #[test]
    fn a_single_branch_clone_still_adopts_the_linked_branch() {
        // `clone --single-branch` maps one branch, so the tracking ref we fetch
        // is covered by no refspec and `worktree add --track` dies with
        // "not a branch": the linked branch must be added to the remote first.
        let tmp = TempDir::new().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        init_repo(&origin);
        git(&origin, &["branch", "123-linked", "HEAD"]);
        let clone = tmp.path().join("clone");
        git(
            tmp.path(),
            &[
                "clone",
                "--single-branch",
                "--branch",
                "main",
                origin.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        let gh = tmp.path().join("gh");
        gh_issue_stub(&gh, "123-linked\thttps://github.com/o/r/tree/123-linked\n", "");
        let dir = clone.to_str().unwrap();
        let got = issue_branch_with(dir, "123", gh.to_str().unwrap()).unwrap();
        assert_eq!(got.branch, "123-linked");

        // Widening writes to the user's config, so a repeat create must not
        // append the same refspec again.
        issue_branch_with(dir, "123", gh.to_str().unwrap()).unwrap();
        let fetch = Command::new("git")
            .args(["-C", dir, "config", "--get-all", "remote.origin.fetch"])
            .output()
            .unwrap();
        let added = String::from_utf8_lossy(&fetch.stdout)
            .lines()
            .filter(|l| l.contains("123-linked"))
            .count();
        assert_eq!(added, 1, "one refspec for the linked branch, not one per create");

        match crate::worktree::create_worktree_from_branch(
            dir,
            "repo",
            "ws",
            &tmp.path().join("state"),
            &got.branch,
        ) {
            crate::worktree::WorktreeResult::Created { branch_name, .. } => {
                assert_eq!(branch_name, "123-linked", "adopted the linked branch")
            }
            crate::worktree::WorktreeResult::Fallback { reason } => {
                panic!("the adopt must succeed: {reason}")
            }
        }
    }

    #[test]
    fn a_linked_branch_shadowed_by_a_remote_tracking_ref_is_refused() {
        // `alice/fix` is a legal branch name, but `create_worktree_from_branch`
        // resolves `refs/remotes/alice/fix` before `refs/remotes/origin/…`, so
        // adopting it would check out remote `alice`'s `fix` instead.
        let tmp = TempDir::new().unwrap();
        let (origin, clone) = issue_fixture(tmp.path());
        git(&origin, &["branch", "alice/fix", "HEAD"]);
        let head = rev_parse(&clone, "HEAD");
        git(&clone, &["update-ref", "refs/remotes/alice/fix", &head]);
        let gh = tmp.path().join("gh");
        gh_issue_stub(&gh, "alice/fix\thttps://github.com/o/r/tree/alice/fix\n", "");
        let err =
            issue_branch_with(clone.to_str().unwrap(), "123", gh.to_str().unwrap()).unwrap_err();
        assert!(err.contains("refs/remotes/alice/fix"), "names the shadowing ref: {err}");
    }

    #[test]
    fn gh_cannot_prompt_through_its_own_git() {
        // `gh issue develop` runs its own `git fetch`; an encrypted ssh key that
        // isn't in the agent would otherwise block on /dev/tty, pinning a TUI
        // worker thread past the timeout.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let gh = tmp.path().join("gh");
        write_stub(
            &gh,
            "#!/bin/sh\nprintf '%s|%s\\n' \"${GIT_TERMINAL_PROMPT-UNSET}\" \
             \"${GIT_SSH_COMMAND-UNSET}\" > \"$0.env\"\nexit 1\n",
        );
        let _ = issue_branch_with(tmp.path().to_str().unwrap(), "123", gh.to_str().unwrap());
        let env = std::fs::read_to_string(format!("{}.env", gh.display())).unwrap();
        let (prompt, ssh) = env.trim().split_once('|').unwrap();
        assert_eq!(prompt, "0", "git can't fall back to a terminal prompt");
        assert!(ssh.ends_with("-oBatchMode=yes"), "ssh can't ask for a passphrase: {ssh}");
    }

    #[test]
    fn issue_branch_aborts_when_the_lookup_fails() {
        // A PR number (or an unauthenticated gh) must never reach the create
        // call, which would perform a remote write.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let gh = tmp.path().join("gh");
        write_stub(
            &gh,
            &format!(
                "#!/bin/sh\n{GH_ARGS_LINE}printf 'GraphQL: Could not resolve to an Issue with the number of 118.\\n' >&2\nexit 1\n"
            ),
        );
        let dir = tmp.path().to_str().unwrap();
        let err = issue_branch_with(dir, "118", gh.to_str().unwrap()).unwrap_err();
        assert!(err.contains("Could not resolve to an Issue"), "{err}");
        assert_eq!(gh_args(&gh).len(), 1, "the create call is never reached");
    }

    #[test]
    fn issue_branch_surfaces_a_failed_create() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let gh = tmp.path().join("gh");
        write_stub(
            &gh,
            &format!(
                "#!/bin/sh\n{GH_ARGS_LINE}case \"$*\" in\n  *--list*) exit 0 ;;\n  *) printf 'remote: Permission to o/r.git denied\\n' >&2; exit 1 ;;\nesac\n"
            ),
        );
        let dir = tmp.path().to_str().unwrap();
        let err = issue_branch_with(dir, "123", gh.to_str().unwrap()).unwrap_err();
        assert!(err.contains("Permission to o/r.git denied"), "gh's own message surfaces: {err}");
    }

    #[test]
    fn issue_branch_refuses_when_several_branches_are_linked() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let gh = tmp.path().join("gh");
        gh_issue_stub(
            &gh,
            "118-a\thttps://github.com/o/r/tree/118-a\n118-b\thttps://github.com/o/r/tree/118-b\n",
            "",
        );
        let dir = tmp.path().to_str().unwrap();
        let err = issue_branch_with(dir, "118", gh.to_str().unwrap()).unwrap_err();
        assert!(err.contains("2 linked branches"), "{err}");
        assert!(err.contains("118-a") && err.contains("118-b"), "names the candidates: {err}");
        assert_eq!(gh_args(&gh).len(), 1, "no create call, no extra lookup");
    }

    #[test]
    fn issue_branch_reports_a_missing_gh() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let dir = tmp.path().to_str().unwrap();
        let err = issue_branch_with(dir, "123", "/nonexistent/definitely/not/gh").unwrap_err();
        assert!(err.contains("gh CLI not found"), "{err}");
    }

    #[test]
    fn a_linked_branch_is_matched_against_the_pinned_repo() {
        // A linked branch can live somewhere else (`gh issue develop
        // --branch-repo`, and GitHub's Development panel offers a repo
        // picker). `--list` reports only the NAME, so without the URL check we
        // would fetch that name from origin and silently adopt origin's
        // unrelated branch of the same name.
        //
        // `.invalid` is reserved (RFC 2606), so the accept rows fail fast at
        // DNS instead of reaching github.com with the developer's credentials.
        // Getting as far as `couldn't fetch` IS the accept assertion: it proves
        // control flow went past the check.
        let rows: &[(&str, &str)] = &[
            ("fix\thttps://github.invalid/fork/r/tree/fix\n", "lives in github.invalid/fork/r"),
            // Same repo, different case: must pass.
            ("fix\thttps://GitHub.invalid/O/R/tree/fix\n", "couldn't fetch"),
            // No URL to read: gh changing its output shape must degrade to the
            // old behaviour, not refuse every lookup.
            ("fix\n", "couldn't fetch"),
        ];
        for (row, want) in rows {
            let tmp = TempDir::new().unwrap();
            let repo = tmp.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]);
            git(&repo, &["remote", "add", "origin", "https://github.invalid/o/r.git"]);
            let gh = tmp.path().join("gh");
            gh_issue_stub(&gh, row, "");
            let err =
                issue_branch_with(repo.to_str().unwrap(), "7", gh.to_str().unwrap()).unwrap_err();
            assert!(err.contains(want), "{row:?}: expected {want:?}, got: {err}");
            if !want.starts_with("lives in") {
                assert!(!err.contains("lives in"), "{row:?}: must not be refused: {err}");
            } else {
                assert!(err.contains("not github.invalid/o/r"), "names the pin: {err}");
            }
        }
    }

    #[test]
    fn gh_unavailable_distinguishes_a_timeout_from_a_missing_binary() {
        use std::io::{Error, ErrorKind};
        let timed_out = gh_unavailable(&Error::new(ErrorKind::TimedOut, "x"));
        assert!(timed_out.contains("timed out"), "{timed_out}");
        let missing = gh_unavailable(&Error::from(ErrorKind::NotFound));
        assert!(missing.contains("gh CLI not found"), "{missing}");
        // Anything else must NOT tell the user to install the gh they have.
        let denied = gh_unavailable(&Error::from(ErrorKind::PermissionDenied));
        assert!(denied.contains("couldn't run gh"), "{denied}");
        assert!(!denied.contains("not found"), "no bogus install hint: {denied}");
    }

    #[test]
    fn batch_mode_is_appended_to_the_command_git_itself_would_use() {
        // git's own precedence: GIT_SSH_COMMAND beats core.sshCommand (see
        // git-config(1)). Reading only the environment would swap a repo-scoped
        // identity for the default key, and on the issue path that failure
        // lands AFTER the branch has been created on origin. Tabled against the
        // pure rule: reading the real environment here would switch the test
        // off for anyone who exports GIT_SSH_COMMAND, and `set_var` is
        // process-global and unsafe.
        let rows: &[(Option<&str>, Option<&str>, &str)] = &[
            (None, None, "ssh -oBatchMode=yes"),
            (None, Some("ssh -i /k/cfg"), "ssh -i /k/cfg -oBatchMode=yes"),
            (Some("ssh -i /k/env"), None, "ssh -i /k/env -oBatchMode=yes"),
            // The environment wins, exactly as it does for git itself.
            (Some("ssh -i /k/env"), Some("ssh -i /k/cfg"), "ssh -i /k/env -oBatchMode=yes"),
            // Blank reads as absent, so the config still gets its turn.
            (Some("   "), Some("ssh -i /k/cfg"), "ssh -i /k/cfg -oBatchMode=yes"),
            // ssh takes the FIRST value of a repeated option, so someone who
            // asked to be prompted keeps that.
            (Some("ssh -oBatchMode=no"), None, "ssh -oBatchMode=no -oBatchMode=yes"),
        ];
        for (env, cfg, want) in rows {
            assert_eq!(&ssh_command_with_batch_mode(*env, *cfg), want, "{env:?} / {cfg:?}");
        }

        // The repo-backed half, which does not depend on the environment.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let dir = tmp.path().to_str().unwrap();
        assert_eq!(git_config_value(dir, "core.sshCommand"), None, "unset reads as None");
        git(tmp.path(), &["config", "core.sshCommand", "ssh -i /k/id"]);
        assert_eq!(git_config_value(dir, "core.sshCommand"), Some("ssh -i /k/id".to_string()));
    }

    #[test]
    fn wait_bounded_in_gives_up_on_a_child_that_outlives_the_deadline() {
        let child = Command::new("sh")
            // 100x the deadline: long enough to be deterministic, short enough
            // that the orphan is gone soon after the suite.
            .args(["-c", "sleep 5"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let err = wait_bounded_in(child, std::time::Duration::from_millis(50)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
    }

    #[test]
    fn issue_branch_errors_when_the_fetch_fails() {
        // No origin to fetch from: returning Ok here would hand out a branch
        // that isn't in the repo at all.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let gh = tmp.path().join("gh");
        gh_issue_stub(&gh, "123-linked\thttps://github.com/o/r/tree/123-linked\n", "");
        let dir = tmp.path().to_str().unwrap();
        let err = issue_branch_with(dir, "123", gh.to_str().unwrap()).unwrap_err();
        assert!(err.contains("couldn't fetch"), "{err}");
    }
}
