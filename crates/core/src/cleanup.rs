//! Cleanup of merged branches and their worktrees.
//!
//! [`cleanup_merged_workspace`] removes a merged workspace's worktree and branch,
//! where merged means its changes are already on the default branch. That is
//! decided from local git, with no GitHub API call (`gh pr list`), after at most
//! one bounded fetch of the default branch. It runs synchronously and is meant
//! to be called off the UI thread. [`scan_merged_branches`] and
//! [`delete_branches`] are its repo-wide counterpart: a verdict per local
//! branch, then a local delete that re-checks every gate.

use crate::git::{
    default_branch_ref, fetch_origin_branch, gh_bin, git_config_value, is_valid_branch_name,
    last_line, shell_quote,
};
use std::process::{Command, Stdio};

/// Whether `branch` is (or plausibly is) the repo's default branch — the
/// cleanup gate's trunk protection. Best-effort: a failed probe just doesn't
/// match, so an offline/remote-less repo never blocks on this. Two checks:
/// - the literals `main`/`master`, unconditionally — even when the actual
///   default is something else, since e.g. in a gitflow repo whose default is
///   `develop`, a `main` back-merged into it sits off its first-parent line
///   and would read as merged. Accepted flip side:
///   a branch literally named `master` in a `main`-default repo is refused
///   (delete it by hand).
/// - whatever `origin/HEAD` points at, when resolvable — covers `trunk`/
///   `develop`-style defaults. Residual: a default named neither main/master
///   in a repo without `origin/HEAD` is unprotected here; the merged check
///   still applies, the delete is local-only, and the branch is restorable
///   from the remote.
///
/// Counterpart of [`default_branch_ref`] (the diff base): that wants precision,
/// this wants recall. Don't unify them.
fn is_default_branch(repo_path: &str, branch: &str) -> bool {
    // ponytail: one `symbolic-ref` spawn per gated branch; resolve origin/HEAD
    // once per scan if a huge repo makes the local gates measurable.
    branch == "main"
        || branch == "master"
        || origin_head_branch(repo_path).as_deref() == Some(branch)
}

/// The branch `origin/HEAD` points at, read from the full symref (`--short`
/// prints `remotes/origin/<b>` once a local branch named `origin/<b>` exists).
/// None when origin/HEAD is missing or isn't a plain origin branch.
fn origin_head_branch(repo_path: &str) -> Option<String> {
    // `-q`: a missing origin/HEAD exits 1 quietly, not 128.
    let target =
        check_git_stdout(repo_path, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"], &[])?;
    let branch = target.strip_prefix("refs/remotes/origin/")?;
    is_valid_branch_name(branch).then(|| branch.to_string())
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

/// Env for every merged-check spawn: replace refs and grafts can't fake a
/// merge, a partial clone never fetches (or prompts) mid-check, no system-wide
/// attributes apply, and the replay's driver overrides read `false`.
const CHECK_ENV: [(&str, &str); 5] = [
    ("GIT_NO_REPLACE_OBJECTS", "1"),
    // Not `/dev/null`, which makes git print its grafts deprecation hint on
    // every spawn; a path under it can't exist, so it is silently ignored.
    ("GIT_GRAFT_FILE", "/dev/null/none"),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_ATTR_NOSYSTEM", "1"),
    // The value of every `--config-env=merge.<driver>.driver=KOMMAND0_MERGE_DRIVER_OFF`.
    ("KOMMAND0_MERGE_DRIVER_OFF", "false"),
];

/// `git -C <repo_path> <args>` under [`CHECK_ENV`] plus `envs`: the trimmed
/// stdout, or None when git can't run or exits non-zero. A failure is logged
/// with the args and git's first stderr line: debug for a plain "no" (exit 1),
/// warn from exit 128 up, where git broke rather than answered.
fn check_git_stdout(repo_path: &str, args: &[&str], envs: &[(&str, &str)]) -> Option<String> {
    let out = match Command::new("git")
        .args(["-C", repo_path])
        .args(args)
        .envs(CHECK_ENV)
        .envs(envs.iter().copied())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("git {}: {e}", args.join(" "));
            return None;
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().next().unwrap_or("").trim();
        if out.status.code().is_none_or(|c| c >= 128) {
            tracing::warn!("git {} failed ({}): {first}", args.join(" "), out.status);
        } else {
            tracing::debug!("git {} failed ({}): {first}", args.join(" "), out.status);
        }
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A default branch pinned at one commit, for a whole scan or cleanup.
/// `Base::default()` pins nothing, so [`delete_branches`] refuses it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Base {
    /// `origin/main`, or the local `main`: what messages call it.
    name: String,
    /// `<ref>^{commit}`, resolved once, after any fetch.
    oid: String,
}

/// The default branch the cleanups compare against, and how.
struct Target {
    base: Base,
    /// `GIT_ATTR_SOURCE` for every check, so no in-tree attributes apply (git
    /// before 2.40 ignores it).
    empty_tree: String,
    /// The config options (`-c`, `--config-env`) every replay runs with. None
    /// when squash merges can't be detected: `merge-tree --write-tree` doesn't
    /// work, this is a partial clone, or the merge drivers couldn't be listed
    /// (so no replay can run without their overrides).
    merge_config: Option<Vec<String>>,
    /// What the answers can't show on their own, the one to act on first: a
    /// failed refresh, then squash detection off.
    notes: Vec<String>,
}

/// Resolve and pin the default branch (see [`default_branch_ref`]), fetching it
/// from origin first when `fetch` is set and it is an origin branch. A failed
/// fetch isn't an error: the checks run against the local copy, and the
/// failure goes into the notes.
fn merge_target(repo_path: &str, fetch: bool) -> Result<Target, String> {
    let git_ref = default_branch_ref(repo_path).ok_or_else(|| {
        "couldn't find the default branch to compare against (no origin/HEAD, origin/main, \
         origin/master, main or master); if origin has one, `git fetch origin` and then \
         `git remote set-head origin -a` set origin/HEAD"
            .to_string()
    })?;
    let origin_branch = match git_ref.strip_prefix("refs/remotes/origin/") {
        Some("HEAD") => origin_head_branch(repo_path),
        b => b.map(str::to_string),
    };
    let name = match &origin_branch {
        Some(b) => format!("origin/{b}"),
        None => git_ref
            .trim_start_matches("refs/heads/")
            .trim_start_matches("refs/remotes/")
            .to_string(),
    };
    let mut refresh_note = None;
    if fetch
        && let Some(b) = &origin_branch
        && let Err(e) = fetch_origin_branch(repo_path, b, &gh_bin())
    {
        // Reported by the caller (TUI log, CLI stderr), not logged here: the
        // CLI's tracing also writes to stderr, so it would print twice.
        refresh_note = Some(format!("{name} not refreshed: {e}"));
    }
    let oid = check_git_stdout(
        repo_path,
        &["rev-parse", "--verify", "--quiet", &format!("{git_ref}^{{commit}}")],
        &[],
    )
    .ok_or_else(|| format!("couldn't resolve {name}"))?;
    let empty_tree = check_git_stdout(repo_path, &["hash-object", "-t", "tree", "/dev/null"], &[])
        .ok_or_else(|| "couldn't compute git's empty tree".to_string())?;
    // Listed so the replay can override them, from every config name filtered
    // here on bytes: in a UTF-8 locale `--get-regexp` silently skips a name
    // that isn't UTF-8. Any failure, or such a name (a lossy decode would
    // override a different key), leaves them unknown, so squash detection goes
    // off rather than replay without the overrides. Without `GIT_CONFIG`, which
    // points `git config` alone at one file: the replay reads the real config.
    let names = Command::new("git")
        .args(["-C", repo_path, "config", "--list", "--name-only"])
        .env_remove("GIT_CONFIG")
        .output();
    let driver_keys = match names {
        Ok(o) if o.status.success() => {
            let keys = o.stdout.split(|b| *b == b'\n').filter(|name| {
                name.strip_prefix(b"merge.").is_some_and(|rest| rest.ends_with(b".driver"))
            });
            let decoded: Option<Vec<String>> =
                keys.map(|k| String::from_utf8(k.to_vec()).ok()).collect();
            if decoded.is_none() {
                tracing::debug!("git config: a merge driver name isn't UTF-8");
            }
            decoded
        }
        Ok(o) => {
            let why = last_line(&o.stderr);
            tracing::debug!("git config --list failed ({}): {why}", o.status);
            None
        }
        Err(e) => {
            tracing::debug!("git config --list: {e}");
            None
        }
    };
    // The version probe logs at debug only: on git before 2.38 it fails every
    // time, and a warning would land on kmd's stderr.
    let merge_tree_works = || match Command::new("git")
        .args(["-C", repo_path, "merge-tree", "--write-tree", &oid, &oid])
        .envs(CHECK_ENV)
        .stdout(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            let why = last_line(&o.stderr);
            tracing::debug!("git merge-tree --write-tree failed ({}): {why}", o.status);
            false
        }
        Err(e) => {
            tracing::debug!("git merge-tree --write-tree: {e}");
            false
        }
    };
    let squash_note = if git_config_value(repo_path, "extensions.partialclone").is_some() {
        Some("squash merges not detected in a partial clone")
    } else if !merge_tree_works() {
        Some("squash merges not detected (needs git 2.38 or newer)")
    } else if driver_keys.is_none() {
        Some("squash merges not detected (couldn't read the merge driver config)")
    } else {
        None
    };
    let merge_config = driver_keys.filter(|_| squash_note.is_none()).map(|keys| {
        let fixed = [
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "merge.renormalize=false",
            "-c",
            "merge.default=text",
        ];
        // Every configured driver becomes `false` (the value in CHECK_ENV),
        // whatever picks it: attribute pins miss `$GIT_DIR/info/attributes`.
        // `--config-env` splits at the last `=`, `-c` at the first, and a driver
        // name may have one.
        let drivers = keys.iter().map(|k| format!("--config-env={k}=KOMMAND0_MERGE_DRIVER_OFF"));
        fixed.map(String::from).into_iter().chain(drivers).collect()
    });
    Ok(Target {
        base: Base { name, oid },
        empty_tree,
        merge_config,
        notes: refresh_note.into_iter().chain(squash_note.map(str::to_string)).collect(),
    })
}

/// A local branch as [`merged_into`] sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Merged {
    /// Its changes are on the default branch. The only answer that deletes.
    Yes,
    /// Nothing of its own: the default branch's own history, changes that
    /// cancel out, or a branch created at a merged tip.
    NoCommits,
    /// Squash-merged (or rebased), but before the branch's newest commit: the
    /// commits since may hold work its net change hides (something added and
    /// removed again).
    CommitsAfter,
    /// Not merged, or git couldn't tell.
    No,
}

/// Whether `branch` (at `tip`) is merged into `target`. Every failure lands on
/// No, CommitsAfter or NoCommits, never Yes. Yes needs one of:
/// - the tip is an ancestor of the base but off its first-parent line (merged
///   by a merge commit; on that line it is the base's own history);
/// - a commit on the base since the fork point carries the branch's net change
///   (same patch-id), replaying the branch onto that commit's parent
///   reproduces its tree exactly, and every such commit was committed no
///   earlier than the branch's newest (a squash, or a one-commit rebase, of the
///   branch as it is now; else CommitsAfter).
///
/// Then [`fresh_by_reflog`] can still veto it.
fn merged_into(repo_path: &str, target: &Target, branch: &str, tip: &str) -> Merged {
    let env = [("GIT_ATTR_SOURCE", target.empty_tree.as_str())];
    let git = |args: &[&str]| check_git_stdout(repo_path, args, &env);
    let base = target.base.oid.as_str();
    let Some(fork) = git(&["merge-base", base, tip]) else { return Merged::No };
    if fork == tip {
        // On the first-parent line, the walk ends on the commit whose first parent is the tip.
        let Some(walk) = git(&["rev-list", "--first-parent", "--parents", base, &format!("^{tip}")])
        else {
            return Merged::No;
        };
        if walk.lines().last().is_none_or(|l| l.split(' ').nth(1) == Some(tip)) {
            return Merged::NoCommits;
        }
    } else {
        let tip_tree = format!("{tip}^{{tree}}");
        let Some(trees) = git(&["rev-parse", &tip_tree, &format!("{fork}^{{tree}}")]) else {
            return Merged::No;
        };
        let Some((a, b)) = trees.split_once('\n') else { return Merged::No };
        if a == b {
            return Merged::NoCommits; // changes that cancel out
        }
        let Some(merge_config) = &target.merge_config else { return Merged::No };
        // The branch's net change as one commit on the fork point. A fixed
        // identity and date: deterministic, and no user config can block it.
        let probe_env = [
            ("GIT_AUTHOR_NAME", "kommand0"),
            ("GIT_AUTHOR_EMAIL", "kommand0@localhost"),
            ("GIT_AUTHOR_DATE", "1700000000 +0000"),
            ("GIT_COMMITTER_NAME", "kommand0"),
            ("GIT_COMMITTER_EMAIL", "kommand0@localhost"),
            ("GIT_COMMITTER_DATE", "1700000000 +0000"),
        ];
        let probe_args = [
            "commit-tree",
            "--no-gpg-sign",
            tip_tree.as_str(),
            "-p",
            fork.as_str(),
            "-m",
            "kommand0 cleanup probe",
        ];
        let Some(probe) = check_git_stdout(repo_path, &probe_args, &probe_env) else {
            return Merged::No;
        };
        let range = format!("{probe}...{base}");
        let Some(marks) = git(&["rev-list", "--cherry-mark", "--right-only", "--no-merges", &range])
        else {
            return Merged::No;
        };
        // Patch-ids ignore whitespace and position, so a candidate counts only
        // if replaying the branch onto its parent reproduces its tree exactly.
        let replays = |c: &str| {
            let parent = format!("{c}^");
            let mut args: Vec<&str> = merge_config.iter().map(String::as_str).collect();
            args.extend(["merge-tree", "--write-tree", parent.as_str(), tip]);
            let tree = git(&["rev-parse", &format!("{c}^{{tree}}")]);
            let (Some(tree), Some(out)) = (tree, git(&args)) else { return false };
            out.lines().next() == Some(tree.as_str())
        };
        let confirmed: Vec<&str> =
            marks.lines().filter_map(|l| l.strip_prefix('=')).filter(|c| replays(c)).collect();
        if confirmed.is_empty() {
            return Merged::No;
        }
        // And only when every such commit is no older than the branch's newest
        // commit: the net change can't show commits made after a merge
        // (something added and removed again), and a later landing of the same
        // change on the base, after a revert, doesn't vouch for them either.
        // Committer dates, so a clock running behind can still hide such a
        // commit, and one running ahead keeps a branch merged within its lead.
        let dates = |revs: &[&str]| -> Option<Vec<i64>> {
            let mut args = vec!["log", "--no-show-signature", "--format=%ct"];
            args.extend_from_slice(revs);
            git(&args)?.lines().map(|t| t.parse().ok()).collect()
        };
        let range = format!("{fork}..{tip}");
        let newest = dates(&[range.as_str()]).and_then(|d| d.into_iter().max());
        let landings = [&["--no-walk"][..], &confirmed].concat();
        let first_landing = dates(&landings).and_then(|d| d.into_iter().min());
        let (Some(newest), Some(first_landing)) = (newest, first_landing) else {
            return Merged::No;
        };
        if first_landing < newest {
            return Merged::CommitsAfter;
        }
    }
    if fresh_by_reflog(repo_path, branch, tip) { Merged::NoCommits } else { Merged::Yes }
}

/// Whether `branch` was created at `tip` and never moved: its only reflog
/// entry is its creation, at the current tip. Its tip can read as merged (a
/// `worktree add -b` from a merged checkout) while it has no commits of its
/// own. An adopted `origin/<same name>` isn't fresh: that is the merged branch
/// itself, fetched. An unreadable reflog counts as fresh; an empty one (expired,
/// or logging off) leaves the graph's answer.
fn fresh_by_reflog(repo_path: &str, branch: &str, tip: &str) -> bool {
    let refname = format!("refs/heads/{branch}");
    let args = ["reflog", "show", "--no-show-signature", "--format=%H%x00%gs", refname.as_str()];
    let Some(log) = check_git_stdout(repo_path, &args, &[]) else { return true };
    let mut entries = log.lines();
    let (Some(only), None) = (entries.next(), entries.next()) else { return false };
    let Some((oid, subject)) = only.split_once('\0') else { return true };
    oid == tip
        && subject.starts_with("branch: Created from ")
        && subject != format!("branch: Created from origin/{branch}")
}

/// `-c`s for a status (or `worktree remove`, whose own check is one) that sees
/// what's on disk however git was told to look: see [`uncommitted_work`].
const LOOK_AT_THE_DISK: [&str; 6] = [
    "-c",
    "status.showUntrackedFiles=normal",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.untrackedCache=false",
];

/// Refuses, with the refusal, when removing `worktree_path` would lose work: a
/// change `git status` lists, even in a file whose index flag hides it from a
/// plain status. Refuses too when git can't tell.
///
/// Its own status, not the tree's `branch_status`, because git can be told not
/// to look: `status.showUntrackedFiles=no` hides untracked files (from
/// `worktree remove`'s own check too, which then deletes them), and a stale
/// fsmonitor or untracked cache answers for the disk. A file flagged
/// assume-unchanged or skip-worktree shows in no status at all, so the flagged
/// files on disk get a second status, on a copy of the index with their flags
/// cleared: git's own verdict (filters and line endings, the executable bit,
/// symlinks). One that isn't on disk (a sparse checkout, or a deletion git
/// doesn't see) loses nothing. Nothing here writes the real index, so a refused
/// worktree keeps its caches.
fn uncommitted_work(worktree_path: &str) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    fn args(a: &[&'static str]) -> Vec<&'static OsStr> {
        a.iter().map(|s| OsStr::new(*s)).collect()
    }
    let unreadable = || "couldn't read the worktree's git status; not cleaning up".to_string();
    let git = |args: &[&OsStr], index: Option<&Path>| {
        let mut cmd = Command::new("git");
        cmd.args(["-C", worktree_path]).args(LOOK_AT_THE_DISK).args(args);
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        if let Some(index) = index {
            cmd.env("GIT_INDEX_FILE", index);
        }
        cmd.output().ok().filter(|o| o.status.success()).ok_or_else(unreadable)
    };
    let status = git(&args(&["status", "--porcelain", "--ignore-submodules=none"]), None)?;
    if let Some(line) = String::from_utf8_lossy(&status.stdout).lines().next() {
        return Err(format!(
            "the worktree has uncommitted changes ({}); commit or discard them first",
            line.trim()
        ));
    }

    // `<tag> <mode> <oid> <stage>\t<path>`: the tag lowercase when
    // assume-unchanged, `S`/`s` when skip-worktree.
    let files = git(&args(&["ls-files", "-s", "-v", "-z"]), None)?;
    let mut flagged: Vec<(&[u8], &[u8], Flags)> = Vec::new();
    for entry in files.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let (&tag, rest) = entry.split_first().ok_or_else(unreadable)?;
        let rest = rest.strip_prefix(b" ").ok_or_else(unreadable)?;
        let tab = rest.iter().position(|b| *b == b'\t').ok_or_else(unreadable)?;
        let (info, path) = (&rest[..tab], &rest[tab + 1..]);
        let flag =
            Flags { assume: tag.is_ascii_lowercase(), skip: tag.eq_ignore_ascii_case(&b's') };
        if !flag.assume && !flag.skip {
            continue;
        }
        // lstat: a dangling symlink is on disk too.
        let on_disk = Path::new(worktree_path).join(OsStr::from_bytes(path));
        match std::fs::symlink_metadata(on_disk) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unreadable()),
            Ok(_) => flagged.push((info, path, flag)),
        }
    }
    if flagged.is_empty() {
        return Ok(());
    }

    // The copy sits beside the index, where a split index's shared file
    // resolves, and is created fresh under a name nothing else can hold.
    let index = git(&args(&["rev-parse", "--git-path", "index"]), None)?;
    let index = PathBuf::from(OsStr::from_bytes(index.stdout.trim_ascii()));
    let index = if index.is_absolute() { index } else { Path::new(worktree_path).join(index) };
    let copy = index.with_file_name(format!("index.kommand0-{}", uuid::Uuid::new_v4()));
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&copy)
        .and_then(|mut to| std::io::copy(&mut std::fs::File::open(&index)?, &mut to));
    let verdict = created.map_err(|_| unreadable()).and_then(|_| {
        // Re-added from their own index info: no flags and no stat data, so
        // status has to read each one's content (through filters), mode and
        // type. Unsplit, unhooked and full, so writing the copy can't expire
        // the real index's shared file or run the repo's hooks.
        let mut info = Vec::new();
        for (entry, path, _) in &flagged {
            info.extend_from_slice(entry);
            info.push(b'\t');
            info.extend_from_slice(path);
            info.push(0);
        }
        let mut child = Command::new("git")
            .args(["-C", worktree_path, "-c", "core.splitIndex=false"])
            .args(["-c", "splitIndex.sharedIndexExpire=never", "-c", "core.hooksPath=/dev/null"])
            .args(["-c", "index.sparse=false", "update-index", "-z", "--index-info"])
            .env("GIT_INDEX_FILE", &copy)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| unreadable())?;
        let written = child.stdin.take().map(|mut stdin| stdin.write_all(&info));
        let done = child.wait().is_ok_and(|s| s.success());
        if !done || !matches!(written, Some(Ok(()))) {
            return Err(unreadable());
        }
        let flags =
            ["status", "--porcelain", "-z", "--untracked-files=no", "--ignore-submodules=none"];
        let status = git(&args(&flags), Some(&copy))?;
        // `XY <path>`, the path raw.
        let Some(entry) = status.stdout.split(|b| *b == 0).find(|e| !e.is_empty()) else {
            return Ok(());
        };
        let path = entry.get(3..).unwrap_or_default();
        let flag = flagged.iter().find(|(_, p, _)| *p == path).map(|(.., f)| *f);
        Err(hidden(path, flag.unwrap_or(Flags { assume: true, skip: true })))
    });
    let _ = std::fs::remove_file(&copy);
    verdict
}

/// The index flags that hide a file's edits from `git status`.
#[derive(Debug, Clone, Copy)]
struct Flags {
    assume: bool,
    skip: bool,
}

/// The refusal for an edit an index flag hides: the command that clears each
/// flag, with the name quoted for a shell. Any other name is shown escaped and
/// left out of the command, so it can't split the refusal into a fake note or
/// be pasted into a shell as is.
fn hidden(path: &[u8], flags: Flags) -> String {
    let name = String::from_utf8_lossy(path);
    // A backslash too (fish reads `\'` inside single quotes as a quote), and a
    // name that isn't UTF-8 (the lossy one would name another file).
    let plain = std::str::from_utf8(path)
        .is_ok_and(|n| !n.chars().any(|c| c.is_control() || c == '\\'));
    let (shown, arg) = if plain {
        (name.to_string(), shell_quote(&name))
    } else {
        (name.escape_debug().to_string(), "<file>".to_string())
    };
    let set: Vec<&str> = [(flags.assume, "assume-unchanged"), (flags.skip, "skip-worktree")]
        .into_iter()
        .filter_map(|(on, flag)| on.then_some(flag))
        .collect();
    let fix: Vec<String> =
        set.iter().map(|flag| format!("git update-index --no-{flag} -- {arg}")).collect();
    format!(
        "{shown} is flagged {}, which hides its edits from git; clear it with `{}`, then \
         commit or discard them",
        set.join(" and "),
        fix.join(" && ")
    )
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

/// `git branch -D -- <branch>`, only while it still points at `tip` (else
/// "moved since it was checked"): force, because a squash-merge leaves the
/// branch "unmerged" to git (the callers proved `tip` merged). `--` makes the
/// gate's leading-dash refusal belt-and-braces, not load-bearing. Otherwise
/// Err is git's last stderr line or the io error.
fn delete_local_branch(repo_path: &str, branch: &str, tip: &str) -> Result<(), String> {
    if branch_tip(repo_path, branch).as_deref() != Some(tip) {
        return Err("moved since it was checked".to_string());
    }
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

/// Remove a merged workspace's worktree and delete its branch, but only when
/// every check holds. Returns a message (deleting nothing) unless ALL hold:
/// - the branch is not the repo's default branch (see [`is_default_branch`]),
///   not a malformed name (empty, `..`, leading `-`), and not one of the
///   `protected` names (the config's `protected_branches`). Any *other* branch,
///   including one kommand0 didn't create, is fair game once the checks below
///   hold: adopting and cleaning up your own branches is deliberate behavior;
/// - it is merged into the default branch (see [`merged_into`]), decided from
///   local git. The default branch is fetched only when that could change the
///   answer, i.e. when the local copy says not merged;
/// - it isn't a branch created at a merged tip without a commit of its own
///   (see [`fresh_by_reflog`]);
/// - the worktree, if it still exists, is live and its HEAD is still on
///   `branch` (a `git switch` inside it made the dir another branch's checkout);
/// - the worktree is clean (see [`uncommitted_work`]; an unreadable status
///   aborts rather than assuming clean); and
/// - the branch is still at the checked tip right before the worktree is
///   removed, and again right before the branch is deleted.
///
/// The fetch runs only on a not-merged answer, so after an upstream
/// force-push a stale default branch can still read merged: the dropped
/// commits stay reachable from `refs/remotes/origin/<b>` until the next fetch
/// (then only from that ref's reflog).
///
/// The worktree is removed WITHOUT `--force` (a last-moment dirty state still
/// fails safe), and only then is the branch deleted, locally only: the remote
/// branch is never touched.
///
/// A removal that *fails* has two shapes, told apart by [`is_live_worktree`]:
/// git refused and touched nothing (its own reason is reported, nothing is
/// deleted), or it died mid-delete after dropping the admin entry, leaving an
/// unusable half-deleted tree that this finishes deleting. Either way the branch
/// survives an `Err`, so a retry re-runs every gate: `Ok`/`Err` is the caller's
/// deregister signal, so a path that deletes the branch must return `Ok`.
///
/// An `Err` is the refusal on its first line, then, for "not merged", any notes
/// (see [`Scan::notes`]) on lines of their own.
pub fn cleanup_merged_workspace(
    repo_path: &str,
    worktree_path: &str,
    branch: &str,
    protected: &[String],
) -> Result<(), String> {
    refuse_branch_delete(repo_path, branch, protected).map_err(|r| r.message(branch))?;

    let Some(tip) = branch_tip(repo_path, branch) else {
        return Err("couldn't resolve the branch tip; not cleaning up".to_string());
    };
    let mut target = merge_target(repo_path, false)?;
    let mut merged = merged_into(repo_path, &target, branch, &tip);
    // Fetch only when it could change the answer: a routed row was scanned
    // after a fetch and read merged, so a batch of them normally fires no fetch
    // (rows that moved since the scan can still race on the ref lock; the loser
    // reads its local copy and says so). The target is re-resolved and
    // re-pinned; a local base just isn't fetched.
    if merged == Merged::No {
        target = merge_target(repo_path, true)?;
        merged = merged_into(repo_path, &target, branch, &tip);
    }
    match merged {
        Merged::Yes => {}
        Merged::NoCommits => {
            return Err("the branch has no commits of its own; not cleaning up".to_string());
        }
        Merged::CommitsAfter => {
            return Err(format!(
                "the branch has commits after its merge into {}; not cleaning up",
                target.base.name
            ));
        }
        Merged::No => {
            let refusal =
                format!("the branch isn't merged into {}; not cleaning up", target.base.name);
            return Err([vec![refusal], target.notes].concat().join("\n"));
        }
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
        uncommitted_work(worktree_path)?;
    }

    // Still the tip that was checked: a commit made meanwhile (an agent in the
    // worktree) isn't merged, and removing the worktree would strand it.
    if branch_tip(repo_path, branch).as_deref() != Some(tip.as_str()) {
        return Err("the branch moved while it was being checked; clean up again".to_string());
    }

    // Remove the worktree (no --force, so a last-moment dirty state still fails
    // safe, untracked files included: the `-c`s reach git's own status check).
    // If the dir survives, which of the two failure shapes it is comes from
    // re-probing, never from matching git's stderr text.
    if worktree_exists {
        let out = Command::new("git")
            .args(["-C", repo_path])
            .args(LOOK_AT_THE_DISK)
            .args(["worktree", "remove", worktree_path])
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
            // and its tracked content was just proven identical to a merged
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

    // The delete re-checks the tip, because kmd doesn't stop a live agent: a
    // commit landed since the first check keeps the branch.
    delete_local_branch(repo_path, branch, &tip).map_err(|e| {
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
    /// Merged into the default branch, checked out nowhere.
    Delete,
    /// Same, but checked out at `worktree` (git refuses `branch -D` there).
    CheckedOut { worktree: String },
    /// Not deletable, and why (short, user-facing).
    Skip(String),
}

/// What [`scan_merged_branches`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    /// One verdict per local branch.
    pub verdicts: Vec<BranchVerdict>,
    /// What the verdicts can't show on their own, the one to act on first: a
    /// failed refresh of the default branch, then squash detection off.
    pub notes: Vec<String>,
    /// The default branch the verdicts were judged against, for
    /// [`delete_branches`].
    pub base: Base,
}

/// One local branch as [`scan_merged_branches`] saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchVerdict {
    /// The short name (`refs/heads/` stripped).
    pub branch: String,
    /// The tip at scan time; [`delete_branches`] refuses a branch that moved.
    pub tip: String,
    /// What the scan decided for it.
    pub verdict: Verdict,
}

/// Classify every local branch of `repo_path` for deletion: the name gates of
/// [`cleanup_merged_workspace`] first, then its merged check against the
/// default branch, fetched first. Deletes and prunes nothing.
///
/// A stale default branch (the fetch failed) only under-reports, except after
/// an upstream force-push: the dropped commits stay reachable from
/// `refs/remotes/origin/<b>` until the next fetch, so a branch can read merged.
pub fn scan_merged_branches(
    repo_path: &str,
    protected: &[String],
) -> Result<Scan, String> {
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
    let target = merge_target(repo_path, true)?;
    // for-each-ref ends each record with '\n', which lands in front of the next
    // refname; the newline-only remainder after the last NUL is dropped.
    let (records, _) = fields.as_chunks::<3>();
    for &[refname, tip, worktree] in records {
        let Some(branch) = refname.trim_start_matches('\n').strip_prefix("refs/heads/") else {
            continue;
        };
        // ponytail: ~5 git spawns per branch plus one cherry-mark walk over every base
        // commit since its fork; fold all probes into one octopus for a single
        // rev-list --cherry-mark pass if hundreds of branches make the scan slow.
        let verdict = match refuse_branch_delete(repo_path, branch, protected) {
            Err(r) => Verdict::Skip(r.short().to_string()),
            Ok(()) => match merged_into(repo_path, &target, branch, tip) {
                Merged::NoCommits => Verdict::Skip("no commits of its own".to_string()),
                Merged::CommitsAfter => Verdict::Skip("commits after its merge".to_string()),
                Merged::No => Verdict::Skip(format!("not merged into {}", target.base.name)),
                Merged::Yes if !worktree.is_empty() => {
                    Verdict::CheckedOut { worktree: worktree.to_string() }
                }
                Merged::Yes => Verdict::Delete,
            },
        };
        verdicts.push(BranchVerdict { branch: branch.to_string(), tip: tip.to_string(), verdict });
    }
    Ok(Scan { verdicts, notes: target.notes, base: target.base })
}

/// Each branch [`delete_branches`] was given, with how its delete went.
pub type BranchResults = Vec<(String, Result<(), String>)>;

/// Delete local branches, each only while it still points at its scan-time
/// `tip` (else "moved since it was checked"), and none once the default branch
/// no longer contains the scan's `base` commit: rewound or rewritten since (an
/// upstream force-push, fetched), the verdicts don't hold anymore. The name
/// gates re-run; nothing here touches remotes, prunes, or calls gh. Results
/// come back in input order.
pub fn delete_branches(
    repo_path: &str,
    branches: &[(String, String)],
    protected: &[String],
    base: &Base,
) -> Result<BranchResults, String> {
    if branches.is_empty() {
        return Ok(Vec::new());
    }
    if base.oid.is_empty() {
        return Err("scan again: no scan pinned the default branch".to_string());
    }
    let contains = default_branch_ref(repo_path).is_some_and(|now| {
        let args = ["merge-base", "--is-ancestor", base.oid.as_str(), now.as_str()];
        check_git_stdout(repo_path, &args, &[]).is_some()
    });
    if !contains {
        // The remedy first: the TUI pane clips the tail.
        return Err(format!("scan again: {} was rewound or rewritten since the scan", base.name));
    }
    let delete = |branch: &str, tip: &str| -> Result<(), String> {
        refuse_branch_delete(repo_path, branch, protected).map_err(|r| r.message(branch))?;
        delete_local_branch(repo_path, branch, tip)
    };
    Ok(branches.iter().map(|(b, t)| (b.clone(), delete(b, t))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, init_repo, issue_fixture, rev_parse};
    use std::path::Path;
    use tempfile::TempDir;

    /// A repo (no remote — pins that an unresolvable `origin/HEAD` never blocks
    /// the gate) with a linked worktree on `branch`. Returns
    /// `(repo_path, worktree_path, branch)`.
    fn repo_with_worktree_on(
        root: &Path,
        branch: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let wt = root.join("wt");
        git(&repo, &["worktree", "add", wt.to_str().unwrap(), "-b", branch]);
        (repo, wt, branch.to_string())
    }

    /// [`repo_with_worktree_on`] with the post-prefix default shape: bare `feat`.
    fn repo_with_worktree(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, String) {
        repo_with_worktree_on(root, "feat")
    }

    /// Write `name` in `dir` and commit it.
    fn commit_file(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", name]);
    }

    /// `git <args>` in `dir`, committing `secs` from now: commit dates decide
    /// "commits after its merge", and a fixture runs within a second.
    fn git_at(dir: &Path, secs: i64, args: &[&str]) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        let out = Command::new("git")
            .args(args)
            .env("GIT_COMMITTER_DATE", format!("{} +0000", now.as_secs() as i64 + secs))
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Commit everything in `dir`, `secs` from now (see [`git_at`]).
    fn commit_at(dir: &Path, secs: i64, msg: &str) {
        git(dir, &["add", "-A"]);
        git_at(dir, secs, &["commit", "-m", msg]);
    }

    /// Squash-merge `branch` into main, which `repo` has checked out.
    fn squash_merge(repo: &Path, branch: &str) {
        git(repo, &["merge", "--squash", "--ff", branch]);
        git(repo, &["commit", "-m", &format!("squash {branch}")]);
    }

    /// [`repo_with_worktree_on`] whose branch has a commit of its own that main
    /// squash-merged.
    fn merged_worktree_on(
        root: &Path,
        branch: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let (repo, wt, branch) = repo_with_worktree_on(root, branch);
        commit_file(&wt, "work.txt", "work");
        squash_merge(&repo, &branch);
        (repo, wt, branch)
    }

    /// [`issue_fixture`] plus a clone worktree on `feat` whose one commit origin
    /// squash-merged after the clone last fetched. Returns
    /// `(origin, clone, worktree)`.
    fn clone_with_upstream_squash(
        root: &Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let (origin, clone) = issue_fixture(root);
        let wt = root.join("wt");
        git(&clone, &["worktree", "add", wt.to_str().unwrap(), "-b", "feat"]);
        commit_file(&wt, "feat.txt", "feat");
        git(&wt, &["push", "origin", "feat"]);
        squash_merge(&origin, "feat");
        (origin, clone, wt)
    }

    /// Point the clone's origin at a path that doesn't exist.
    fn break_origin(clone: &Path, root: &Path) {
        git(clone, &["remote", "set-url", "origin", root.join("gone").to_str().unwrap()]);
    }

    fn cleanup(repo: &Path, wt: &Path, branch: &str) -> Result<(), String> {
        cleanup_merged_workspace(repo.to_str().unwrap(), wt.to_str().unwrap(), branch, &[])
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
    fn cleanup_refuses_an_unmerged_branch_and_destroys_nothing() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = repo_with_worktree(tmp.path());
        commit_file(&wt, "work.txt", "work");
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert_eq!(err, "the branch isn't merged into main; not cleaning up");
        assert!(wt.exists(), "worktree untouched");
        assert!(branch_exists(&repo, &branch), "branch untouched");
    }

    #[test]
    fn cleanup_refuses_a_branch_still_at_main() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = repo_with_worktree(tmp.path());
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert_eq!(err, "the branch has no commits of its own; not cleaning up");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_refuses_a_branch_created_at_a_merged_tip() {
        // Its tip reads as merged, but its only reflog entry is its creation:
        // nothing of its own was ever committed. Off a squash-merged checkout...
        let tmp = TempDir::new().unwrap();
        let (repo, wt, _) = merged_worktree_on(tmp.path(), "feat");
        let wt2 = tmp.path().join("wt2");
        git(&wt, &["worktree", "add", wt2.to_str().unwrap(), "-b", "feat2"]);
        let err = cleanup(&repo, &wt2, "feat2").unwrap_err();
        assert!(err.contains("no commits of its own"), "{err}");

        // ...and off a detached side commit that main merged with --no-ff.
        git(&repo, &["switch", "-c", "side"]);
        commit_file(&repo, "side.txt", "side");
        git(&repo, &["switch", "main"]);
        git(&repo, &["merge", "--no-ff", "-m", "merge side", "side"]);
        git(&repo, &["switch", "--detach", "side"]);
        let wt3 = tmp.path().join("wt3");
        git(&repo, &["worktree", "add", wt3.to_str().unwrap(), "-b", "side2"]);
        let err = cleanup(&repo, &wt3, "side2").unwrap_err();
        assert!(err.contains("no commits of its own"), "{err}");
        assert!(wt2.exists() && wt3.exists(), "worktrees untouched");
        assert!(branch_exists(&repo, "feat2") && branch_exists(&repo, "side2"), "branches untouched");
    }

    #[test]
    fn cleanup_removes_a_merged_adopted_pr_head() {
        // Adopting a remote branch logs `Created from origin/<same name>`: that
        // is the merged branch itself, fetched, not a fresh one.
        let tmp = TempDir::new().unwrap();
        let (origin, clone) = issue_fixture(tmp.path());
        git(&origin, &["switch", "-c", "pr-head"]);
        commit_file(&origin, "pr.txt", "pr");
        git(&origin, &["switch", "main"]);
        squash_merge(&origin, "pr-head");
        git(&clone, &["fetch", "origin"]);
        let wt = tmp.path().join("wt");
        let wt_arg = wt.to_str().unwrap();
        git(&clone, &["worktree", "add", "--track", "-b", "pr-head", wt_arg, "origin/pr-head"]);
        assert_eq!(cleanup(&clone, &wt, "pr-head"), Ok(()));
        assert!(!wt.exists() && !branch_exists(&clone, "pr-head"), "adopted branch cleaned up");
    }

    #[test]
    fn cleanup_refuses_dirty_worktree() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        std::fs::write(wt.join("scratch.txt"), "wip").unwrap(); // untracked => dirty
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.contains("uncommitted"), "expected 'uncommitted', got: {err}");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_sees_untracked_files_a_status_config_hides() {
        // `status.showUntrackedFiles=no` hides them from a plain status AND from
        // `worktree remove`'s own check, which would then delete them.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        git(&repo, &["config", "status.showUntrackedFiles", "no"]);
        std::fs::write(wt.join("notes.txt"), "wip").unwrap();
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.contains("uncommitted changes (?? notes.txt)"), "names it: {err}");
        assert!(wt.join("notes.txt").exists(), "the untracked file survives");
        assert!(branch_exists(&repo, &branch), "the branch survives");
    }

    #[test]
    fn cleanup_sees_edits_an_index_flag_or_a_stale_fsmonitor_hides() {
        // No status lists them, so neither did the gate nor git's own check.
        for flag in ["--assume-unchanged", "--skip-worktree", "both", "fsmonitor"] {
            let tmp = TempDir::new().unwrap();
            let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
            if flag == "fsmonitor" {
                // A hook that always answers "nothing changed" since its token.
                let hook = tmp.path().join("fsmonitor");
                std::fs::write(&hook, "#!/bin/sh\nprintf 'token\\0'\n").unwrap();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
                git(&wt, &["config", "core.fsmonitor", hook.to_str().unwrap()]);
                git(&wt, &["update-index", "--fsmonitor"]);
                git(&wt, &["status"]);
            } else if flag == "both" {
                git(&wt, &["update-index", "--assume-unchanged", "work.txt"]);
                git(&wt, &["update-index", "--skip-worktree", "work.txt"]);
            } else {
                git(&wt, &["update-index", flag, "work.txt"]);
            }
            std::fs::write(wt.join("work.txt"), "edited").unwrap();
            let plain = Command::new("git")
                .args(["-C", wt.to_str().unwrap(), "status", "--porcelain"])
                .output()
                .unwrap();
            assert!(plain.stdout.is_empty(), "{flag}: the fixture hides the edit from a status");
            let index = repo.join(".git/worktrees/wt/index");
            let before = std::fs::read(&index).unwrap();
            let err = cleanup(&repo, &wt, &branch).unwrap_err();
            assert_eq!(std::fs::read(&index).unwrap(), before, "{flag}: the real index is untouched");
            let want = match flag {
                "fsmonitor" => "uncommitted changes (M work.txt)".to_string(),
                "both" => "work.txt is flagged assume-unchanged and skip-worktree, which hides its \
                           edits from git; clear it with `git update-index --no-assume-unchanged \
                           -- 'work.txt' && git update-index --no-skip-worktree -- 'work.txt'`"
                    .to_string(),
                _ => format!("work.txt is flagged {}", flag.trim_start_matches("--")),
            };
            assert!(err.contains(&want), "{flag}: {err}");
            assert_eq!(std::fs::read_to_string(wt.join("work.txt")).unwrap(), "edited", "{flag}");
        }
    }

    #[test]
    fn a_refused_cleanup_leaves_the_worktrees_index_alone() {
        // The gate reads status with the caches off; writing that index back
        // would strip them from a worktree it then refuses to remove.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        git(&wt, &["config", "core.untrackedCache", "true"]);
        std::fs::write(wt.join("notes.txt"), "wip").unwrap();
        git(&wt, &["status"]);
        let out = Command::new("git")
            .args(["-C", wt.to_str().unwrap(), "rev-parse", "--git-path", "index"])
            .output()
            .unwrap();
        let index = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        let index = if index.is_absolute() { index } else { wt.join(index) };
        let cached = || std::fs::read(&index).unwrap().windows(4).any(|w| w == b"UNTR");
        assert!(cached(), "the fixture's index has an untracked cache");
        assert!(cleanup(&repo, &wt, &branch).is_err());
        assert!(cached(), "the refused cleanup kept it");
    }

    #[test]
    fn cleanup_removes_a_worktree_whose_flagged_files_are_unedited() {
        // The flag alone isn't work. `core.ignoreStat` flags every entry a
        // worktree checks out, symlinks included; a blob committed with CRLF
        // under `text=auto`, or a file checked out with `eol=crlf`, differs
        // from its blob on disk yet has no edit, as `git status` itself says.
        for case in ["--assume-unchanged", "--skip-worktree", "core.ignoreStat", "crlf", "eol"] {
            let tmp = TempDir::new().unwrap();
            let repo = tmp.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            init_repo(&repo);
            git(&repo, &["config", "core.autocrlf", "false"]);
            if case == "core.ignoreStat" {
                git(&repo, &["config", "core.ignoreStat", "true"]);
            }
            let wt = tmp.path().join("wt");
            git(&repo, &["worktree", "add", wt.to_str().unwrap(), "-b", "feat"]);
            match case {
                "crlf" => {
                    commit_file(&wt, "work.txt", "one\r\ntwo\r\n");
                    commit_file(&wt, ".gitattributes", "* text=auto\n");
                }
                "eol" => {
                    commit_file(&wt, ".gitattributes", "*.txt text eol=crlf\n");
                    commit_file(&wt, "work.txt", "one\ntwo\n");
                    // Checked out again, now with the CRLF endings eol asks for.
                    std::fs::remove_file(wt.join("work.txt")).unwrap();
                    git(&wt, &["checkout", "--", "work.txt"]);
                }
                _ => {
                    std::os::unix::fs::symlink("a.txt", wt.join("link")).unwrap();
                    commit_file(&wt, "work.txt", "work");
                }
            }
            squash_merge(&repo, "feat");
            if case != "core.ignoreStat" {
                let flag = if case.starts_with("--") { case } else { "--assume-unchanged" };
                git(&wt, &["update-index", flag, "work.txt"]);
            }
            let tags = Command::new("git")
                .args(["-C", wt.to_str().unwrap(), "ls-files", "-v"])
                .output()
                .unwrap();
            let tags = String::from_utf8_lossy(&tags.stdout);
            assert!(tags.contains("h work.txt") || tags.contains("S work.txt"), "{case}: {tags}");
            assert_eq!(cleanup(&repo, &wt, "feat"), Ok(()), "{case}");
            assert!(!wt.exists() && !branch_exists(&repo, "feat"), "{case}");
        }
    }

    #[test]
    fn cleanup_sees_any_edit_among_many_flagged_files() {
        // Every flagged file on disk counts, not just the first; an executable
        // bit is an edit too (git status says so).
        for edit in ["content", "mode"] {
            let tmp = TempDir::new().unwrap();
            let (repo, wt, branch) = repo_with_worktree_on(tmp.path(), "feat");
            for i in 0..300 {
                std::fs::write(wt.join(format!("f{i:03}.txt")), format!("{i}\n")).unwrap();
            }
            commit_file(&wt, "work.txt", "work");
            squash_merge(&repo, "feat");
            let names: Vec<String> = (0..300).map(|i| format!("f{i:03}.txt")).collect();
            let mut flag = vec!["update-index", "--assume-unchanged", "--"];
            flag.extend(names.iter().map(String::as_str));
            git(&wt, &flag);
            let admin = |repo: &Path| -> Vec<String> {
                let dir = std::fs::read_dir(repo.join(".git/worktrees/wt")).unwrap();
                let mut names: Vec<String> =
                    dir.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
                names.sort();
                names
            };
            let admin_before = admin(&repo);
            let last = wt.join("f299.txt");
            if edit == "content" {
                std::fs::write(&last, "edited\n").unwrap();
            } else {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&last, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let err = cleanup(&repo, &wt, &branch).unwrap_err();
            assert!(err.starts_with("f299.txt is flagged assume-unchanged"), "{edit}: {err}");
            assert!(wt.exists() && branch_exists(&repo, &branch), "{edit}: nothing destroyed");
            assert_eq!(admin(&repo), admin_before, "{edit}: nothing left beside the index");
        }
    }

    #[test]
    fn cleanup_keeps_a_flagged_file_whatever_git_would_see_on_disk() {
        // A skip-worktree symlink repointed at nothing is on disk (a dangling
        // link doesn't "exist" to a stat that follows it), and a newline in a
        // flagged name must not split the refusal into a fake note.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = repo_with_worktree_on(tmp.path(), "feat");
        std::os::unix::fs::symlink("work.txt", wt.join("link")).unwrap();
        std::fs::write(wt.join("a\nb.txt"), "odd").unwrap();
        commit_file(&wt, "work.txt", "work");
        squash_merge(&repo, "feat");
        git(&wt, &["update-index", "--skip-worktree", "link"]);
        std::fs::remove_file(wt.join("link")).unwrap();
        std::os::unix::fs::symlink("missing", wt.join("link")).unwrap();
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.starts_with("link is flagged skip-worktree"), "{err}");
        git(&wt, &["update-index", "--no-skip-worktree", "link"]);
        std::fs::remove_file(wt.join("link")).unwrap();
        std::os::unix::fs::symlink("work.txt", wt.join("link")).unwrap();

        git(&wt, &["update-index", "--assume-unchanged", "a\nb.txt"]);
        std::fs::write(wt.join("a\nb.txt"), "edited").unwrap();
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.starts_with("a\\nb.txt is flagged assume-unchanged"), "{err}");
        assert!(!err.contains('\n') && err.contains("-- <file>`"), "one line, no name to paste: {err}");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn the_suggested_fix_quotes_a_hostile_file_name() {
        // A name that's shell syntax is quoted in the command the refusal
        // suggests; one with a backslash (fish reads `\'` in quotes) isn't
        // offered as a command at all.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = repo_with_worktree_on(tmp.path(), "feat");
        let (hostile, slashed) = ("$(touch pwned) it's.txt", "a\\b.txt");
        std::fs::write(wt.join(hostile), "x").unwrap();
        std::fs::write(wt.join(slashed), "x").unwrap();
        commit_file(&wt, "work.txt", "work");
        squash_merge(&repo, "feat");
        git(&wt, &["update-index", "--assume-unchanged", hostile]);
        std::fs::write(wt.join(hostile), "edited").unwrap();
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.contains(r#"-- '$(touch pwned) it'\''s.txt'`"#), "quoted: {err}");
        std::fs::write(wt.join(hostile), "x").unwrap();
        git(&wt, &["update-index", "--assume-unchanged", slashed]);
        std::fs::write(wt.join(slashed), "edited").unwrap();
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.starts_with("a\\\\b.txt is flagged") && err.contains("-- <file>`"), "{err}");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_reads_a_flagged_files_content_not_its_cached_stat() {
        // Same length, mtime put back, and git told to compare little else:
        // the stat git cached for the flagged file still matches (clearing
        // the flag keeps it), and only its content tells.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        git(&repo, &["config", "core.trustctime", "false"]);
        git(&repo, &["config", "core.checkStat", "minimal"]);
        git(&wt, &["update-index", "--assume-unchanged", "work.txt"]);
        let file = wt.join("work.txt");
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::fs::write(&file, "WORK").unwrap();
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(mtime).unwrap();
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.starts_with("work.txt is flagged assume-unchanged"), "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "WORK");
    }

    #[test]
    fn cleanup_removes_a_sparse_worktree() {
        // A skip-worktree file that isn't on disk is a sparse checkout, not work.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        git(&wt, &["update-index", "--skip-worktree", "work.txt"]);
        std::fs::remove_file(wt.join("work.txt")).unwrap();
        assert_eq!(cleanup(&repo, &wt, &branch), Ok(()));
        assert!(!wt.exists() && !branch_exists(&repo, &branch));
    }

    #[test]
    fn cleanup_refuses_default_branch() {
        // Literal main/master arm, unconditional. The fixture repo has no
        // remote → origin/HEAD is unresolvable → only the literal arm can refuse.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, _) = repo_with_worktree(tmp.path());
        for default in ["main", "master"] {
            let err = cleanup(&repo, &wt, default).unwrap_err();
            assert!(err.contains("default branch"), "expected default-branch refusal, got: {err}");
        }
        assert!(branch_exists(&repo, "main"), "main untouched");
    }

    #[test]
    fn cleanup_refuses_default_branch_via_origin_head() {
        // origin/HEAD arm: the origin's default is `trunk` (NOT main/master, or
        // the literal arm would make this vacuous), and a local branch named
        // `origin/trunk` must not hide it. A sibling branch must get PAST the
        // gate (failing later, with nothing of its own): the refusal is "is
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
        git(&clone, &["branch", "origin/trunk"]);

        let wt = tmp.path().join("no-wt"); // gate fires before any worktree use
        let err = cleanup(&clone, &wt, "trunk").unwrap_err();
        assert!(err.contains("default branch"), "trunk refused via origin/HEAD: {err}");
        let err = cleanup(&clone, &wt, "sibling").unwrap_err();
        assert!(err.contains("no commits of its own"), "sibling passes the gate: {err}");
    }

    #[test]
    fn cleanup_refuses_empty_dotdot_and_dash_names() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, _) = repo_with_worktree(tmp.path());
        for bad in ["", "a..b", "-feat"] {
            let err = cleanup(&repo, &wt, bad).unwrap_err();
            assert!(err.contains("malformed"), "{bad:?} refused as malformed, got: {err}");
        }
    }

    #[test]
    fn cleanup_removes_a_merged_worktree_and_its_branch() {
        // A workspace's own branch, one kommand0 didn't name (adopted via
        // --branch / the checkout offer), and a pre-0.11 `kommand0/` one.
        for name in ["feat", "feat/login", "kommand0/legacy"] {
            let tmp = TempDir::new().unwrap();
            let (repo, wt, branch) = merged_worktree_on(tmp.path(), name);
            assert_eq!(cleanup(&repo, &wt, &branch), Ok(()), "{name}");
            assert!(!wt.exists() && !branch_exists(&repo, &branch), "{name} cleaned up");
        }
    }

    #[test]
    fn cleanup_refuses_a_worktree_switched_to_another_branch() {
        // The workspace's dir is live but a `git switch` inside it moved HEAD off
        // the workspace's branch: removing the dir would destroy the OTHER
        // branch's checkout (and its ignored files) on a merged verdict that
        // was never about it.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        git(&wt, &["switch", "-c", "elsewhere"]);
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.contains("is on elsewhere, not"), "names both branches: {err}");
        assert!(wt.exists(), "worktree untouched");
        assert!(branch_exists(&repo, &branch) && branch_exists(&repo, "elsewhere"), "both branches intact");
        // Detached HEAD is the same refusal.
        git(&wt, &["switch", "--detach"]);
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.contains("detached HEAD"), "detached is refused too: {err}");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn cleanup_completes_when_worktree_dir_already_gone() {
        // A retry after a partial cleanup (worktree removed, branch left) must
        // still delete the orphaned branch: the merged check runs in the repo,
        // the missing worktree skips the dirty check, and `worktree prune`
        // clears the entry.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        std::fs::remove_dir_all(&wt).unwrap(); // worktree dir vanished
        assert_eq!(cleanup(&repo, &wt, &branch), Ok(()));
        assert!(!branch_exists(&repo, &branch), "orphaned branch deleted");
    }

    #[test]
    fn cleanup_does_not_claim_a_removal_when_the_worktree_was_already_gone() {
        // Nothing was removed on this path, so a failed `branch -D` (the branch
        // is checked out in the main repo by now) must not say "worktree removed".
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        std::fs::remove_dir_all(&wt).unwrap();
        git(&repo, &["worktree", "prune"]);
        git(&repo, &["switch", &branch]);
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.starts_with(&format!("couldn't delete branch {branch}:")), "{err}");
        assert!(branch_exists(&repo, &branch), "the branch survives the refusal");
    }

    #[test]
    fn is_live_worktree_tells_wreckage_from_everything_else() {
        // The guard the fs fallback rides on: only a dir git has ALREADY stopped
        // recognizing may be deleted outright, and every adjacent shape has to
        // land on the protected side.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, _branch) = repo_with_worktree(tmp.path());
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
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        std::fs::remove_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::remove_file(wt.join("a.txt")).unwrap(); // git got this far
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert!(err.contains(wt.to_str().unwrap()), "names the dir to delete: {err}");
        assert!(wt.exists(), "the leftovers are NOT deleted for us");
        assert!(branch_exists(&repo, &branch), "branch survives, so a retry can run");
        // And once the user deletes it, the retry finishes the job.
        std::fs::remove_dir_all(&wt).unwrap();
        assert_eq!(cleanup(&repo, &wt, &branch), Ok(()));
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
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        // `info/exclude` lives in the common dir, so it covers the worktree too
        // (no commit, so the branch stays merged).
        std::fs::write(repo.join(".git/info/exclude"), ".certs/\n").unwrap();
        std::fs::create_dir_all(wt.join(".certs")).unwrap();
        std::fs::write(wt.join(".certs/k.pem"), "key").unwrap();
        std::fs::set_permissions(wt.join(".certs"), std::fs::Permissions::from_mode(0o555)).unwrap();
        let res = cleanup(&repo, &wt, &branch);
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
    fn cleanup_refuses_a_protected_branch_even_when_merged() {
        // A listed name is refused whatever its merge state (the gate fires
        // before the worktree is looked at); with an empty list the same merged
        // branch goes through, so the list is honored, not hardcoded.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "development");
        git(&repo, &["switch", "-c", "staging"]);
        commit_file(&repo, "staging.txt", "staging");
        git(&repo, &["switch", "main"]);
        let protected = ["development".to_string(), "staging".to_string()];
        for name in ["development", "staging"] {
            let err = cleanup_merged_workspace(
                repo.to_str().unwrap(),
                wt.to_str().unwrap(),
                name,
                &protected,
            )
            .unwrap_err();
            assert!(err.contains("protected branch"), "{name}: expected protected refusal, got: {err}");
        }
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
        assert_eq!(cleanup(&repo, &wt, &branch), Ok(()), "an empty list lets it through");
    }

    #[test]
    fn cleanup_fetches_when_the_squash_landed_after_the_last_fetch() {
        let tmp = TempDir::new().unwrap();
        let (_origin, clone, wt) = clone_with_upstream_squash(tmp.path());
        assert_eq!(cleanup(&clone, &wt, "feat"), Ok(()));
        assert!(!wt.exists() && !branch_exists(&clone, "feat"), "cleaned up after the fetch");
    }

    #[test]
    fn cleanup_skips_the_fetch_when_already_merged() {
        let tmp = TempDir::new().unwrap();
        let (origin, clone, wt) = clone_with_upstream_squash(tmp.path());
        git(&clone, &["fetch", "origin"]);
        commit_file(&origin, "later.txt", "later");
        let fetched = rev_parse(&clone, "refs/remotes/origin/main");
        assert_eq!(cleanup(&clone, &wt, "feat"), Ok(()));
        assert_eq!(rev_parse(&clone, "refs/remotes/origin/main"), fetched, "origin/main never refetched");
    }

    #[test]
    fn cleanup_refusal_names_a_failed_fetch() {
        let tmp = TempDir::new().unwrap();
        let (_origin, clone, wt) = clone_with_upstream_squash(tmp.path());
        break_origin(&clone, tmp.path());
        let err = cleanup(&clone, &wt, "feat").unwrap_err();
        assert!(
            err.contains("isn't merged into origin/main; not cleaning up\norigin/main not refreshed:"),
            "{err}"
        );
        assert!(err.contains("does not appear to be a git repository"), "{err}");
        assert!(wt.exists() && branch_exists(&clone, "feat"), "nothing destroyed");
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

    /// Whether patch-ids alone would call `branch` merged: its net change as
    /// one commit on `fork`, then any `=` mark among main's commits since.
    fn lookalike(repo: &Path, branch: &str, fork: &str) -> bool {
        let dir = repo.to_str().unwrap();
        let tree = rev_parse(repo, &format!("{branch}^{{tree}}"));
        let out = Command::new("git")
            .args(["-C", dir, "commit-tree", &tree, "-p", fork, "-m", "probe"])
            .output()
            .unwrap();
        let probe = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let range = format!("{probe}...main");
        let out = Command::new("git")
            .args(["-C", dir, "rev-list", "--cherry-mark", "--right-only", "--no-merges", &range])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).lines().any(|l| l.starts_with('='))
    }

    #[test]
    fn scan_classifies_branches_from_local_git() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let r = repo.as_path();
        git(r, &["branch", "stale"]);
        let lines: String = (1..=8).map(|n| format!("{n}\n")).collect();
        // Twenty identical lines, so a hunk looks the same wherever it lands.
        let rep = |at: usize| -> String {
            (1..=20).map(|n| if n == at { "b\n" } else { "a\n" }).collect()
        };
        std::fs::write(repo.join("rep.txt"), rep(0)).unwrap();
        commit_file(r, "lines.txt", &lines);
        let fork = rev_parse(r, "main");
        // A branch off `fork` with one commit writing `file`.
        let branch = |name: &str, file: &str, content: &str| {
            git(r, &["switch", "-c", name, &fork]);
            commit_file(r, file, content);
            git(r, &["switch", "main"]);
        };
        // One more commit on an existing branch.
        let extend = |name: &str, file: &str, content: &str| {
            git(r, &["switch", name]);
            commit_file(r, file, content);
            git(r, &["switch", "main"]);
        };

        // Merged: a squash that main then edits next to, a two-commit squash
        // whose replay merges with an edit main made to the same file first, a
        // slashed name, a --no-ff merge, a cherry-pick, and a squash whose
        // reflog expired.
        branch("squashed", "lines.txt", &lines.replace("2\n", "two\n"));
        squash_merge(r, "squashed");
        let main_lines = lines.replace("2\n", "two\n").replace("3\n", "three\n");
        commit_file(r, "lines.txt", &main_lines);
        branch("content-merge", "lines.txt", &lines.replace("7\n", "seven\n"));
        extend("content-merge", "lines.txt", &lines.replace("7\n", "seven\n").replace("8\n", "eight\n"));
        // Outside the diff context of the branch's hunk: patch-ids see context.
        commit_file(r, "lines.txt", &main_lines.replace("1\n", "one\n"));
        squash_merge(r, "content-merge");
        branch("feat/x", "x.txt", "x");
        squash_merge(r, "feat/x");
        branch("merge-commit", "m.txt", "m");
        git(r, &["merge", "--no-ff", "-m", "merge", "merge-commit"]);
        branch("rebased", "r.txt", "r");
        git(r, &["cherry-pick", "rebased"]);
        branch("expired", "e.txt", "e");
        squash_merge(r, "expired");
        git(r, &["reflog", "expire", "--expire=now", "refs/heads/expired"]);

        // Not merged: more work after a squash or a merge, never merged, no
        // shared history, and two branches patch-ids alone would call merged
        // (whitespace inside the squashed lines; the same hunk elsewhere).
        branch("beyond", "b.txt", "b");
        squash_merge(r, "beyond");
        extend("beyond", "b2.txt", "b2");
        branch("merged-then-beyond", "mb.txt", "mb");
        git(r, &["merge", "--no-ff", "-m", "merge", "merged-then-beyond"]);
        extend("merged-then-beyond", "mb2.txt", "mb2");
        branch("unmerged", "u.txt", "u");
        git(r, &["switch", "--orphan", "unrelated"]);
        commit_file(r, "o.txt", "o");
        git(r, &["switch", "main"]);
        branch("ws-only", "w.txt", "alpha beta\n");
        squash_merge(r, "ws-only");
        extend("ws-only", "w.txt", "alpha  beta\n");
        branch("same-hunk-elsewhere", "rep.txt", &rep(15));
        commit_file(r, "rep.txt", &rep(5));
        assert!(lookalike(r, "ws-only", &fork), "ws-only fools patch-ids");
        assert!(lookalike(r, "same-hunk-elsewhere", &fork), "same-hunk-elsewhere fools patch-ids");

        // Nothing of its own: changes that cancel out (with an empty commit on
        // main that an empty probe would match), a fast-forward that main
        // moved past, a branch just created at a merged tip, and main's first
        // commit.
        branch("net-zero", "z.txt", "z");
        git(r, &["switch", "net-zero"]);
        git(r, &["rm", "z.txt"]);
        git(r, &["commit", "-m", "undo z"]);
        git(r, &["switch", "main"]);
        git(r, &["commit", "--allow-empty", "-m", "empty"]);
        git(r, &["switch", "-c", "ff-merged"]);
        commit_file(r, "ff.txt", "ff");
        git(r, &["switch", "main"]);
        git(r, &["merge", "--ff-only", "ff-merged"]);
        commit_file(r, "after-ff.txt", "after");
        git(r, &["branch", "fresh", "feat/x"]);

        // Gated, and merged but checked out: in a linked worktree, and in the
        // repo itself.
        branch("development", "d.txt", "d");
        squash_merge(r, "development");
        let wt = tmp.path().join("wt");
        git(r, &["worktree", "add", wt.to_str().unwrap(), "-b", "wt-branch", &fork]);
        commit_file(&wt, "wt.txt", "wt");
        squash_merge(r, "wt-branch");
        branch("other", "other.txt", "other");
        squash_merge(r, "other");
        // Last: a replace graft that makes main look like it merged `grafted`.
        branch("grafted", "g.txt", "g");
        git(r, &["switch", "other"]);
        git(r, &["replace", "--graft", "main", "main^", "grafted"]);
        // The probe commit must bring its own identity.
        git(r, &["config", "user.name", ""]);

        let Scan { verdicts, notes, .. } =
            scan_merged_branches(repo.to_str().unwrap(), &["development".to_string()]).unwrap();
        let of = |name: &str| {
            verdicts.iter().find(|v| v.branch == name).unwrap_or_else(|| panic!("{name} scanned"))
        };
        assert_eq!(verdicts.len(), 21);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(of("feat/x").tip, rev_parse(r, "refs/heads/feat/x"), "the scan-time tip");
        for name in ["squashed", "content-merge", "feat/x", "merge-commit", "rebased", "expired"] {
            assert_eq!(of(name).verdict, Verdict::Delete, "{name}");
        }
        for name in [
            "beyond",
            "merged-then-beyond",
            "unmerged",
            "unrelated",
            "ws-only",
            "same-hunk-elsewhere",
            "grafted",
        ] {
            assert_eq!(of(name).verdict, Verdict::Skip("not merged into main".into()), "{name}");
        }
        for name in ["net-zero", "ff-merged", "fresh", "stale"] {
            assert_eq!(of(name).verdict, Verdict::Skip("no commits of its own".into()), "{name}");
        }
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
    }

    #[test]
    fn a_branch_with_commits_after_its_merge_is_kept() {
        // Something added and removed again after the squash landed: the net
        // change still replays to the squash, but those commits are on no
        // other ref, and deleting the branch would strand them.
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        std::fs::write(wt.join("spike.txt"), "spike").unwrap();
        commit_at(&wt, 60, "spike");
        std::fs::remove_file(wt.join("spike.txt")).unwrap();
        commit_at(&wt, 61, "drop the spike");
        let verdicts = scan_merged_branches(repo.to_str().unwrap(), &[]).unwrap().verdicts;
        let feat = verdicts.iter().find(|v| v.branch == "feat").unwrap();
        assert_eq!(feat.verdict, Verdict::Skip("commits after its merge".into()));
        let err = cleanup(&repo, &wt, &branch).unwrap_err();
        assert_eq!(err, "the branch has commits after its merge into main; not cleaning up");
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn commits_after_a_merge_count_by_the_newest_and_against_every_landing() {
        // The spike again, but (a) its undo comes from a machine whose clock
        // runs behind, dated before the squash, or (b) main reverts the squash
        // and lands the same change again after the spike. Neither the tip's
        // date nor the newer landing may vouch for the spike.
        for relanded in [false, true] {
            let tmp = TempDir::new().unwrap();
            let (repo, wt, _) = merged_worktree_on(tmp.path(), "feat");
            std::fs::write(wt.join("spike.txt"), "spike").unwrap();
            commit_at(&wt, 60, "spike");
            std::fs::remove_file(wt.join("spike.txt")).unwrap();
            commit_at(&wt, if relanded { 61 } else { -60 }, "drop the spike");
            if relanded {
                git_at(&repo, 120, &["revert", "--no-edit", "HEAD"]);
                git_at(&repo, 121, &["revert", "--no-edit", "HEAD"]);
            }
            let verdicts = scan_merged_branches(repo.to_str().unwrap(), &[]).unwrap().verdicts;
            let feat = verdicts.iter().find(|v| v.branch == "feat").unwrap();
            let want = Verdict::Skip("commits after its merge".into());
            assert_eq!(feat.verdict, want, "relanded: {relanded}");
        }
    }

    #[test]
    fn scan_ignores_a_grafts_file() {
        // A grafts entry giving main's tip `g` as a second parent would read
        // as a merge commit.
        let tmp = TempDir::new().unwrap();
        let r = tmp.path();
        init_repo(r);
        git(r, &["switch", "-c", "g"]);
        commit_file(r, "g.txt", "g");
        git(r, &["switch", "main"]);
        commit_file(r, "m.txt", "m");
        let graft = [rev_parse(r, "main"), rev_parse(r, "main^"), rev_parse(r, "g")].join(" ");
        std::fs::write(r.join(".git/info/grafts"), format!("{graft}\n")).unwrap();
        let verdicts = scan_merged_branches(r.to_str().unwrap(), &[]).unwrap().verdicts;
        let g = verdicts.iter().find(|v| v.branch == "g").unwrap();
        assert_eq!(g.verdict, Verdict::Skip("not merged into main".into()));
    }

    #[test]
    fn scan_fetches_the_default_branch_first() {
        // Found through a clone's origin/HEAD, and by name once that is gone.
        for symbolic in [true, false] {
            let tmp = TempDir::new().unwrap();
            let (origin, clone, _wt) = clone_with_upstream_squash(tmp.path());
            if !symbolic {
                git(&clone, &["remote", "set-head", "origin", "-d"]);
            }
            // A tag on the commit the fetch brings in. update-ref, not `git
            // tag`: a global tag.gpgSign would sign it and open an editor.
            git(&origin, &["update-ref", "refs/tags/v1", &rev_parse(&origin, "main")]);
            let Scan { verdicts, notes, .. } =
                scan_merged_branches(clone.to_str().unwrap(), &[]).unwrap();
            let feat = verdicts.iter().find(|v| v.branch == "feat").unwrap();
            assert!(
                matches!(feat.verdict, Verdict::CheckedOut { .. }),
                "origin/HEAD {symbolic}: {:?}",
                feat.verdict
            );
            assert!(notes.is_empty(), "origin/HEAD {symbolic}: {notes:?}");
            assert_eq!(
                rev_parse(&clone, "refs/remotes/origin/main"),
                rev_parse(&origin, "main"),
                "origin/HEAD {symbolic}: origin/main refreshed"
            );
            let tags = Command::new("git")
                .args(["-C", clone.to_str().unwrap(), "tag", "--list"])
                .output()
                .unwrap();
            assert!(tags.stdout.is_empty(), "origin/HEAD {symbolic}: the fetch brought tags");
        }
    }

    #[test]
    fn scan_falls_back_to_the_last_fetch_when_the_refresh_fails() {
        let tmp = TempDir::new().unwrap();
        let (_origin, clone, _wt) = clone_with_upstream_squash(tmp.path());
        git(&clone, &["fetch", "origin"]); // the last fetch saw the squash
        break_origin(&clone, tmp.path());
        let Scan { verdicts, notes, .. } = scan_merged_branches(clone.to_str().unwrap(), &[]).unwrap();
        let feat = verdicts.iter().find(|v| v.branch == "feat").unwrap();
        assert!(matches!(feat.verdict, Verdict::CheckedOut { .. }), "merged: {:?}", feat.verdict);
        let [refresh] = &notes[..] else { panic!("one note: {notes:?}") };
        assert!(refresh.starts_with("origin/main not refreshed: couldn't fetch"), "{refresh}");
    }

    #[test]
    fn scan_reports_a_failed_fetch_once() {
        let tmp = TempDir::new().unwrap();
        let (_origin, clone, _wt) = clone_with_upstream_squash(tmp.path());
        git(&clone, &["switch", "-c", "unmerged"]);
        commit_file(&clone, "u.txt", "u");
        git(&clone, &["switch", "main"]);
        break_origin(&clone, tmp.path());
        // A second note, to pin the order: the one to act on comes first.
        git(&clone, &["config", "extensions.partialclone", "origin"]);
        let Scan { verdicts, notes, .. } = scan_merged_branches(clone.to_str().unwrap(), &[]).unwrap();
        for name in ["feat", "unmerged"] {
            let v = verdicts.iter().find(|v| v.branch == name).unwrap();
            assert_eq!(v.verdict, Verdict::Skip("not merged into origin/main".into()), "{name}");
        }
        let [refresh, squash] = &notes[..] else { panic!("two notes: {notes:?}") };
        assert!(
            refresh.starts_with("origin/main not refreshed: couldn't fetch main from origin: fatal:"),
            "{refresh}"
        );
        assert!(refresh.contains("does not appear to be a git repository"), "{refresh}");
        assert_eq!(squash, "squash merges not detected in a partial clone");
    }

    #[test]
    fn scan_and_cleanup_read_the_base_history_after_the_fetch() {
        // A branch at origin's new tip: unmerged work against the stale
        // origin/main, main's own history against the fetched one.
        let tmp = TempDir::new().unwrap();
        let (origin, clone) = issue_fixture(tmp.path());
        commit_file(&origin, "new.txt", "new");
        git(&origin, &["push", clone.to_str().unwrap(), "main:refs/heads/upstream-tip"]);
        let stale = rev_parse(&clone, "refs/remotes/origin/main");
        let wt = tmp.path().join("wt");
        git(&clone, &["worktree", "add", wt.to_str().unwrap(), "upstream-tip"]);
        let err = cleanup(&clone, &wt, "upstream-tip").unwrap_err();
        assert!(err.contains("no commits of its own"), "{err}");

        git(&clone, &["update-ref", "refs/remotes/origin/main", &stale]);
        let verdicts = scan_merged_branches(clone.to_str().unwrap(), &[]).unwrap().verdicts;
        let v = verdicts.iter().find(|v| v.branch == "upstream-tip").unwrap();
        assert_eq!(v.verdict, Verdict::Skip("no commits of its own".into()));
    }

    #[test]
    fn scan_skips_squash_detection_in_a_partial_clone() {
        let tmp = TempDir::new().unwrap();
        let r = tmp.path();
        init_repo(r);
        for name in ["squashed", "merged"] {
            git(r, &["switch", "-c", name]);
            commit_file(r, &format!("{name}.txt"), name);
            git(r, &["switch", "main"]);
        }
        squash_merge(r, "squashed");
        git(r, &["merge", "--no-ff", "-m", "merge", "merged"]);
        git(r, &["config", "extensions.partialclone", "origin"]);
        let Scan { verdicts, notes, .. } = scan_merged_branches(r.to_str().unwrap(), &[]).unwrap();
        let of = |name: &str| &verdicts.iter().find(|v| v.branch == name).unwrap().verdict;
        assert_eq!(of("squashed"), &Verdict::Skip("not merged into main".into()));
        assert_eq!(of("merged"), &Verdict::Delete, "merge commits still count");
        assert_eq!(notes, ["squash merges not detected in a partial clone"]);
    }

    #[test]
    fn scan_never_runs_a_custom_merge_driver() {
        // A driver picked by `$GIT_DIR/info/attributes`, which no attribute pin
        // reaches. The replay needs a content merge (main edited the file
        // before the squash), so it would run the driver; the scan runs `false`
        // instead and keeps the branch (a known false negative). `a=b` is a
        // legal driver name that a `-c` override splits in the wrong place.
        // A squash that needs no driver must still count: the overrides may
        // not break the replay itself.
        for driver in ["spy", "a=b"] {
            let tmp = TempDir::new().unwrap();
            let repo = tmp.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            init_repo(&repo);
            let r = repo.as_path();
            let lines: String = (1..=8).map(|n| format!("{n}\n")).collect();
            commit_file(r, "f.txt", &lines);
            git(r, &["switch", "-c", "added"]);
            commit_file(r, "new.txt", "new");
            git(r, &["switch", "main"]);
            squash_merge(r, "added");
            git(r, &["switch", "-c", "spied"]);
            commit_file(r, "f.txt", &lines.replace("2\n", "two\n"));
            git(r, &["switch", "main"]);
            commit_file(r, "f.txt", &lines.replace("7\n", "seven\n"));
            squash_merge(r, "spied");
            let marker = tmp.path().join("driver-ran");
            let key = format!("merge.{driver}.driver");
            git(r, &["config", &key, &format!("touch '{}'", marker.display())]);
            std::fs::write(repo.join(".git/info/attributes"), format!("* merge={driver}\n")).unwrap();
            let _ = Command::new("git")
                .args(["-C", r.to_str().unwrap(), "merge-tree", "--write-tree", "main^", "spied"])
                .output()
                .unwrap();
            assert!(marker.exists(), "{driver}: the fixture really reaches the driver");
            std::fs::remove_file(&marker).unwrap();

            let Scan { verdicts, notes, .. } = scan_merged_branches(r.to_str().unwrap(), &[]).unwrap();
            assert!(!marker.exists(), "{driver}: the scan ran the custom driver");
            let of = |name: &str| &verdicts.iter().find(|v| v.branch == name).unwrap().verdict;
            assert_eq!(of("spied"), &Verdict::Skip("not merged into main".into()), "{driver}");
            assert_eq!(of("added"), &Verdict::Delete, "{driver}: a squash that needs no driver");
            assert!(notes.is_empty(), "{driver}: {notes:?}");
        }
    }

    #[test]
    fn scan_skips_squash_detection_for_a_driver_name_that_is_not_utf8() {
        // A lossy decode would override a different key and leave this driver
        // free to run, so a name that won't decode turns squash detection off.
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let r = tmp.path();
        init_repo(r);
        git(r, &["switch", "-c", "added"]);
        commit_file(r, "new.txt", "new");
        git(r, &["switch", "main"]);
        squash_merge(r, "added");
        // Raw bytes, not git argv: a git wrapper may reject non-UTF-8 arguments.
        let mut config =
            std::fs::OpenOptions::new().append(true).open(r.join(".git/config")).unwrap();
        config.write_all(b"[merge \"\xff\"]\n\tdriver = false\n").unwrap();
        let Scan { verdicts, notes, .. } = scan_merged_branches(r.to_str().unwrap(), &[]).unwrap();
        let added = verdicts.iter().find(|v| v.branch == "added").unwrap();
        assert_eq!(added.verdict, Verdict::Skip("not merged into main".into()));
        assert_eq!(notes, ["squash merges not detected (couldn't read the merge driver config)"]);
    }

    #[test]
    fn cleanups_error_without_a_default_branch() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        git(&repo, &["branch", "-m", "main", "trunk"]);
        let want = "couldn't find the default branch to compare against (no origin/HEAD, \
                    origin/main, origin/master, main or master); if origin has one, `git fetch \
                    origin` and then `git remote set-head origin -a` set origin/HEAD";
        assert_eq!(cleanup(&repo, &wt, &branch), Err(want.to_string()));
        assert_eq!(scan_merged_branches(repo.to_str().unwrap(), &[]), Err(want.to_string()));
        assert!(wt.exists() && branch_exists(&repo, &branch), "nothing destroyed");
    }

    #[test]
    fn delete_branches_refuses_every_gate_and_deletes_the_rest() {
        let tmp = TempDir::new().unwrap();
        let repo = repo_with_branches(tmp.path(), &["a", "b", "development"]);
        let wt = tmp.path().join("wt");
        git(&repo, &["worktree", "add", wt.to_str().unwrap(), "-b", "wt-branch"]);
        let sha = rev_parse(&repo, "refs/heads/main");
        let input = [
            ("a".to_string(), sha.clone()),
            ("b".to_string(), "0".repeat(40)), // stale tip
            ("main".to_string(), sha.clone()),
            ("x..y".to_string(), sha.clone()),
            ("development".to_string(), sha.clone()),
            ("wt-branch".to_string(), sha.clone()),
        ];
        let dir = repo.to_str().unwrap();
        let base = scan_merged_branches(dir, &[]).unwrap().base;
        let results = delete_branches(dir, &input, &["development".to_string()], &base).unwrap();
        let names: Vec<&str> = results.iter().map(|(b, _)| b.as_str()).collect();
        assert_eq!(names, ["a", "b", "main", "x..y", "development", "wt-branch"], "input order");
        assert_eq!(results[0].1, Ok(()));
        assert_eq!(results[1].1, Err("moved since it was checked".to_string()));
        assert!(results[2].1.as_ref().unwrap_err().contains("default branch"));
        assert!(results[3].1.as_ref().unwrap_err().contains("malformed"));
        assert!(results[4].1.as_ref().unwrap_err().contains("protected branch"));
        assert!(results[5].1.is_err(), "git refuses a checked-out branch");
        assert!(!branch_exists(&repo, "a"), "a deleted");
        for survivor in ["b", "main", "development", "wt-branch"] {
            assert!(branch_exists(&repo, survivor), "{survivor} survives");
        }
    }

    #[test]
    fn delete_branches_refuses_once_the_default_branch_was_rewound() {
        // The squash that made `feat` read merged is gone from main (a reset,
        // or an upstream force-push fetched while the preview was open).
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        git(&repo, &["worktree", "remove", wt.to_str().unwrap()]);
        let dir = repo.to_str().unwrap();
        let scan = scan_merged_branches(dir, &[]).unwrap();
        let feat = scan.verdicts.iter().find(|v| v.branch == branch).unwrap();
        assert_eq!(feat.verdict, Verdict::Delete);
        let input = [(branch.clone(), feat.tip.clone())];
        git(&repo, &["reset", "--hard", "HEAD~1"]);
        let err = delete_branches(dir, &input, &[], &scan.base).unwrap_err();
        assert_eq!(err, "scan again: main was rewound or rewritten since the scan");
        assert!(branch_exists(&repo, &branch), "nothing deleted");
        assert_eq!(delete_branches(dir, &[], &[], &Base::default()), Ok(vec![]), "nothing to do");
        assert!(delete_branches(dir, &input, &[], &Base::default()).is_err(), "no base, no deletes");
    }

    #[test]
    fn delete_branches_still_deletes_after_the_default_branch_moved_on() {
        let tmp = TempDir::new().unwrap();
        let (repo, wt, branch) = merged_worktree_on(tmp.path(), "feat");
        git(&repo, &["worktree", "remove", wt.to_str().unwrap()]);
        let dir = repo.to_str().unwrap();
        let scan = scan_merged_branches(dir, &[]).unwrap();
        let tip = rev_parse(&repo, &format!("refs/heads/{branch}"));
        commit_file(&repo, "later.txt", "later");
        let results = delete_branches(dir, &[(branch.clone(), tip)], &[], &scan.base).unwrap();
        assert_eq!(results, [(branch.clone(), Ok(()))]);
        assert!(!branch_exists(&repo, &branch));
    }
}
