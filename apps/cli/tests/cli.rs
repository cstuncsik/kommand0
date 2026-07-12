//! Integration tests for the `kmd` CLI: run the real binary against an isolated
//! state dir (and a temp git repo + `gh` stub for the git-lifecycle commands).

use std::path::Path;
use std::process::{Command, Output};

fn run_git(cwd: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} in {cwd:?} failed");
}

/// Run `kmd <args>` with `KOMMAND0_STATE_DIR` (and optional extra env) set.
fn kmd(state_dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kmd"));
    cmd.args(args)
        .env("KOMMAND0_STATE_DIR", state_dir)
        // Belt + braces: the exact-dir override already ignores it, but tests
        // must not depend on the developer's shell exporting a profile.
        .env_remove("KOMMAND0_PROFILE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// Run `kmd <args>` from `cwd` with NO inherited kommand0 env —
/// `KOMMAND0_STATE_DIR`, `KOMMAND0_CONFIG`, and `KOMMAND0_PROFILE` removed
/// (`env` adds back what a test needs). A debug binary then resolves its base
/// dir to `<cwd>/.kommand0-dev`, so a per-test tempdir cwd isolates it
/// (parallel-safe: cwd is per-child). Tests using this are
/// `#[cfg(debug_assertions)]`-gated — under `cargo test --release` the binary
/// would resolve the developer's REAL data dir instead.
#[cfg(debug_assertions)]
fn kmd_at(cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kmd"));
    cmd.args(args)
        .current_dir(cwd)
        .env_remove("KOMMAND0_STATE_DIR")
        .env_remove("KOMMAND0_CONFIG")
        .env_remove("KOMMAND0_PROFILE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// True if `pid` is a live process we can signal (`kill -0`).
fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .unwrap()
        .status
        .success()
}

/// The process group id of `pid` (via `ps`), or None if it's gone.
fn pgid_of(pid: u32) -> Option<u32> {
    let out = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn write_stub(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A tracked repo (with a bare `origin` so pushes succeed) + a workspace on its
/// own branch (`feat`, named after the workspace). Returns the state dir.
fn setup(root: &Path) -> std::path::PathBuf {
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let repo = root.join("repo");
    let remote = root.join("remote.git");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&remote).unwrap();
    run_git(&remote, &["init", "--bare", "-b", "main"]);
    run_git(&repo, &["init", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "1").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "init"]);
    run_git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);

    let add = kmd(&state_dir, &[], &["repo", "add", repo.to_str().unwrap()]);
    assert!(add.status.success(), "repo add: {}", String::from_utf8_lossy(&add.stderr));
    let create = kmd(
        &state_dir,
        &[],
        &["workspace", "create", "feat", "--repo", repo.to_str().unwrap()],
    );
    assert!(create.status.success(), "create: {}", String::from_utf8_lossy(&create.stderr));
    state_dir
}

#[test]
fn workspace_status_shows_the_branch() {
    // Uses a suffixed branch (`feat-2`) so the assertion can only match the
    // BRANCH column — with bare naming, a plain workspace's branch equals its
    // name and `contains` would pass off the NAME column.
    let tmp = tempfile::tempdir().unwrap();
    let (state, repo) = repo_with_branch_feat(tmp.path());
    let create = kmd(&state, &[], &["workspace", "create", "feat", "--repo", &repo]);
    assert!(create.status.success(), "create: {}", String::from_utf8_lossy(&create.stderr));
    let out = kmd(&state, &[], &["workspace", "status"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("BRANCH"), "has a header: {text}");
    assert!(text.contains("feat-2"), "shows the workspace's (suffixed) branch: {text}");
}

#[test]
fn workspace_create_from_an_existing_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "1").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "init"]);
    run_git(&repo, &["branch", "feat/login"]); // an existing branch to adopt

    let add = kmd(&state_dir, &[], &["repo", "add", repo.to_str().unwrap()]);
    assert!(add.status.success(), "repo add: {}", String::from_utf8_lossy(&add.stderr));

    let out = kmd(
        &state_dir,
        &[],
        &["workspace", "create", "--repo", repo.to_str().unwrap(), "--branch", "feat/login"],
    );
    assert!(out.status.success(), "create --branch: {}", String::from_utf8_lossy(&out.stderr));

    // The workspace adopted feat/login (no fresh fork).
    let status = kmd(&state_dir, &[], &["workspace", "status"]);
    assert!(stdout(&status).contains("feat/login"), "shows the adopted branch: {}", stdout(&status));

    // --branch with --no-worktree is a clear error.
    let bad = kmd(
        &state_dir,
        &[],
        &["workspace", "create", "x", "--repo", repo.to_str().unwrap(), "--branch", "feat/login", "--no-worktree"],
    );
    assert!(!bad.status.success(), "conflicting flags should fail");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("can't be combined"),
        "clear error: {}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

/// A tracked repo with a real branch `feat` (no matching workspace yet). Returns
/// `(state_dir, repo_path_string)` for `create feat` detection tests.
fn repo_with_branch_feat(root: &Path) -> (std::path::PathBuf, String) {
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "1").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "init"]);
    run_git(&repo, &["branch", "feat"]); // the existing branch to detect
    let repo_str = repo.to_str().unwrap().to_string();
    let add = kmd(&state_dir, &[], &["repo", "add", &repo_str]);
    assert!(add.status.success(), "repo add: {}", String::from_utf8_lossy(&add.stderr));
    (state_dir, repo_str)
}

#[test]
fn workspace_create_over_existing_branch_forks_and_notes_when_non_interactive() {
    // A spawned `kmd` has no TTY, so `workspace create feat` (no --branch) hits the
    // non-interactive path: fork a suffixed branch (do NOT adopt `feat`) + stderr note.
    let tmp = tempfile::tempdir().unwrap();
    let (state, repo) = repo_with_branch_feat(tmp.path());

    let out = kmd(&state, &[], &["workspace", "create", "feat", "--repo", &repo]);
    assert!(out.status.success(), "create feat: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("branch 'feat' exists"),
        "prints the non-interactive fork note: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Forked, not adopted: bare `feat` exists, so the fork is exactly `feat-2`.
    let status = stdout(&kmd(&state, &[], &["workspace", "status", "feat"]));
    assert!(
        status.contains("feat-2") && !status.contains("feat-2-") && !status.contains("feat-3"),
        "forked exactly feat-2 (not adopted 'feat' or a further-suffixed variant): {status}"
    );
}

#[test]
fn workspace_create_with_fork_notes_the_suffixed_branch() {
    // --fork skips DETECTION (no checkout offer), not uniqueness: with `feat`
    // taken the fork lands on `feat-2`, and the note names it — otherwise the
    // user never learns the actual branch name.
    let tmp = tempfile::tempdir().unwrap();
    let (state, repo) = repo_with_branch_feat(tmp.path());

    let out = kmd(&state, &[], &["workspace", "create", "feat", "--repo", &repo, "--fork"]);
    assert!(out.status.success(), "create --fork: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("forked feat-2"),
        "--fork notes the branch it actually created: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let status = stdout(&kmd(&state, &[], &["workspace", "status", "feat"]));
    assert!(
        status.contains("feat-2") && !status.contains("feat-2-") && !status.contains("feat-3"),
        "forked exactly feat-2 (not adopted 'feat' or a further-suffixed variant): {status}"
    );
}

#[test]
fn non_interactive_note_names_the_actual_forked_branch() {
    // With `feat` AND `feat-2` taken, the fork suffixes to `feat-3`; the note
    // must name the branch actually created (read from `ws.branch_name`), not
    // a hardcoded first-suffix guess.
    let tmp = tempfile::tempdir().unwrap();
    let (state, repo) = repo_with_branch_feat(tmp.path());
    // Occupy the first fork candidate so `unique_branch_name` has to go further.
    run_git(std::path::Path::new(&repo), &["branch", "feat-2"]);

    let out = kmd(&state, &[], &["workspace", "create", "feat", "--repo", &repo]);
    assert!(out.status.success(), "create feat: {}", String::from_utf8_lossy(&out.stderr));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("forked feat-3"),
        "note names the real (suffixed) fork branch, not the first guess: {err}"
    );
}

#[test]
fn workspace_create_over_existing_branch_and_workspace_errors_without_a_note() {
    // Name that's BOTH an existing branch AND an existing workspace: the gate's
    // `validate_new_workspace_name` fails (name in use), so we fall through to
    // create's canonical "already exists" error — and never print a fork note.
    let tmp = tempfile::tempdir().unwrap();
    let (state, repo) = repo_with_branch_feat(tmp.path());

    // First create takes the name `feat` (forks the suffixed feat-2).
    let first = kmd(&state, &[], &["workspace", "create", "feat", "--repo", &repo]);
    assert!(first.status.success(), "first create: {}", String::from_utf8_lossy(&first.stderr));

    // Second create with the same name must fail without a misleading fork note.
    let dup = kmd(&state, &[], &["workspace", "create", "feat", "--repo", &repo]);
    assert!(!dup.status.success(), "duplicate name should fail");
    let err = String::from_utf8_lossy(&dup.stderr);
    assert!(err.contains("already exists"), "canonical error: {err}");
    assert!(!err.contains("forked"), "no misleading fork note: {err}");
}

#[test]
fn cleanup_removes_a_merged_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let state = setup(tmp.path());
    let gh = tmp.path().join("gh");
    // gh runs from the repo dir; answer the `pr list --head <branch>` lookup by
    // reporting the branch's tip as the merged commit.
    write_stub(
        &gh,
        "#!/bin/sh\nif [ \"$1\" = pr ] && [ \"$2\" = list ] && [ \"$3\" = --head ]; then oid=$(git rev-parse \"refs/heads/$4\"); printf 'MERGED\\n%s\\n' \"$oid\"; exit 0; fi\nexit 1\n",
    );
    let out = kmd(
        &state,
        &[("KOMMAND0_GH_BIN", gh.to_str().unwrap())],
        &["workspace", "cleanup", "feat", "--force"],
    );
    assert!(out.status.success(), "cleanup: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout(&out).contains("Cleaned up"));

    // The workspace is gone from `workspace list`.
    let list = kmd(&state, &[], &["workspace", "list", "--all"]);
    assert!(!stdout(&list).contains("feat"), "workspace dropped: {}", stdout(&list));
}

#[test]
fn session_list_filters_by_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    // Seed two workspaces, each with one session.
    let json = serde_json::json!({
        "repos": [{ "id": "r1", "name": "demo", "path": "/tmp" }],
        "workspaces": [
            { "id": "w1", "name": "alpha", "repo_id": "r1", "working_dir": "/tmp", "active": true, "created_at": 0 },
            { "id": "w2", "name": "beta", "repo_id": "r1", "working_dir": "/tmp", "active": true, "created_at": 0 }
        ],
        "sessions": [
            { "id": "s-alpha", "workspace_id": "w1", "status": "Stopped", "pid": null, "log_file": "/tmp/a.log", "created_at": 0 },
            { "id": "s-beta", "workspace_id": "w2", "status": "Stopped", "pid": null, "log_file": "/tmp/b.log", "created_at": 0 }
        ]
    });
    std::fs::write(state_dir.join("state.json"), json.to_string()).unwrap();

    let out = kmd(&state_dir, &[], &["session", "list", "--workspace", "alpha"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("s-alpha"), "shows alpha's session: {text}");
    assert!(!text.contains("s-beta"), "hides beta's session: {text}");

    // No filter shows both.
    let all = stdout(&kmd(&state_dir, &[], &["session", "list"]));
    assert!(all.contains("s-alpha") && all.contains("s-beta"), "all: {all}");
}

#[test]
fn session_start_owns_its_group_and_stop_kills_it() {
    let tmp = tempfile::tempdir().unwrap();
    let state = setup(tmp.path());
    // Stub `claude`: a sleep that outlives the start→stop window (kept short so a
    // mid-test failure leaves at most a ~5s orphan, not 30s).
    let stub = tmp.path().join("claude-stub");
    write_stub(&stub, "#!/bin/sh\nexec sleep 5\n");

    let start = kmd(
        &state,
        &[("KOMMAND0_CLAUDE_BIN", stub.to_str().unwrap())],
        &["session", "start", "feat"],
    );
    assert!(start.status.success(), "start: {}", String::from_utf8_lossy(&start.stderr));
    let pid: u32 = stdout(&start)
        .lines()
        .find_map(|l| l.strip_prefix("PID: "))
        .expect("start prints the PID")
        .trim()
        .parse()
        .expect("a numeric pid");

    // The child leads its OWN process group (process_group(0)) — without the fix
    // it would inherit kmd's group and `kill(-pgid)` would miss it.
    assert!(process_alive(pid), "child is running after start");
    assert_eq!(pgid_of(pid), Some(pid), "child is its own group leader");

    // Stop must actually kill it now that kill(-pgid) reaches the right group.
    let stop = kmd(&state, &[], &["session", "stop", "feat"]);
    assert!(stop.status.success(), "stop: {}", String::from_utf8_lossy(&stop.stderr));

    // Give the kernel a moment to reap the signalled child, then confirm it's gone.
    let mut gone = false;
    for _ in 0..30 {
        if !process_alive(pid) {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(gone, "stop killed the child (pid {pid} still alive)");
}

#[cfg(debug_assertions)]
#[test]
fn profile_flag_isolates_state_and_default_equals_no_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("proj-alpha");
    std::fs::create_dir_all(&repo).unwrap();
    let repo_str = repo.to_str().unwrap();

    let add = kmd_at(tmp.path(), &[], &["--profile", "work", "repo", "add", repo_str]);
    assert!(add.status.success(), "repo add: {}", String::from_utf8_lossy(&add.stderr));

    // Visible under the same profile, absent from the (default) profile.
    let work = stdout(&kmd_at(tmp.path(), &[], &["--profile", "work", "repo", "list"]));
    assert!(work.contains("proj-alpha"), "work profile sees the repo: {work}");
    // The global flag also parses AFTER the subcommand.
    let after = stdout(&kmd_at(tmp.path(), &[], &["repo", "list", "--profile", "work"]));
    assert!(after.contains("proj-alpha"), "flag after the subcommand: {after}");
    let plain_out = kmd_at(tmp.path(), &[], &["repo", "list"]);
    assert!(
        plain_out.status.success(),
        "plain list: {}",
        String::from_utf8_lossy(&plain_out.stderr)
    );
    let plain = stdout(&plain_out);
    assert!(!plain.contains("proj-alpha"), "default profile doesn't: {plain}");
    assert!(
        tmp.path().join(".kommand0-dev").join("profiles").join("work").join("state.json").exists(),
        "state landed under profiles/work/"
    );

    // `--profile default` is exactly the no-flag profile.
    let beta = tmp.path().join("proj-beta");
    std::fs::create_dir_all(&beta).unwrap();
    let add = kmd_at(tmp.path(), &[], &["repo", "add", beta.to_str().unwrap()]);
    assert!(add.status.success(), "repo add: {}", String::from_utf8_lossy(&add.stderr));
    let dflt = stdout(&kmd_at(tmp.path(), &[], &["--profile", "default", "repo", "list"]));
    assert!(dflt.contains("proj-beta"), "--profile default == no flag: {dflt}");
}

#[test]
fn profile_flag_conflicts_with_state_dir_env() {
    let tmp = tempfile::tempdir().unwrap();
    let out = kmd(tmp.path(), &[], &["--profile", "work", "repo", "list"]);
    assert!(!out.status.success(), "env + --profile must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot be combined"),
        "clear conflict error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(debug_assertions)]
#[test]
fn profile_flag_rejects_an_invalid_name() {
    let tmp = tempfile::tempdir().unwrap();
    let out = kmd_at(tmp.path(), &[], &["--profile", "../evil", "repo", "list"]);
    assert!(!out.status.success(), "traversal name must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid profile name"),
        "clear validation error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(debug_assertions)]
#[test]
fn legacy_state_migrates_into_the_default_profile_once() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join(".kommand0-dev");
    std::fs::create_dir_all(&base).unwrap();
    let json = serde_json::json!({
        "repos": [{ "id": "r1", "name": "legacy-repo", "path": "/tmp" }],
        "workspaces": [],
        "sessions": []
    });
    std::fs::write(base.join("state.json"), json.to_string()).unwrap();

    let out = kmd_at(tmp.path(), &[], &["repo", "list"]);
    assert!(out.status.success(), "list: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout(&out).contains("legacy-repo"), "migrated state visible: {}", stdout(&out));
    let migrated = base.join("profiles").join("default").join("state.json");
    assert!(migrated.exists(), "state.json moved under profiles/default/");
    assert!(!base.join("state.json").exists(), "root left clean");

    // Idempotent at the binary level: a second run changes nothing.
    let again = kmd_at(tmp.path(), &[], &["repo", "list"]);
    assert!(again.status.success());
    assert!(stdout(&again).contains("legacy-repo"), "still listed: {}", stdout(&again));
    assert!(migrated.exists());
}

#[cfg(debug_assertions)]
#[test]
fn migration_is_skipped_when_state_dir_env_is_set() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join(".kommand0-dev");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("state.json"), "{}").unwrap();
    let other = tmp.path().join("other-state");
    std::fs::create_dir_all(&other).unwrap();

    let out = kmd_at(
        tmp.path(),
        &[("KOMMAND0_STATE_DIR", other.to_str().unwrap())],
        &["repo", "list"],
    );
    assert!(out.status.success(), "list: {}", String::from_utf8_lossy(&out.stderr));
    assert!(base.join("state.json").exists(), "legacy file untouched");
    assert!(!base.join("profiles").exists(), "no profiles/ dir created under the legacy base");
}

#[cfg(debug_assertions)]
#[test]
fn profile_env_var_selects_the_profile_and_the_flag_beats_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("proj-env");
    std::fs::create_dir_all(&repo).unwrap();
    let dev = tmp.path().join(".kommand0-dev");

    // KOMMAND0_PROFILE alone (as a profiled TUI exports it to its embedded
    // sessions) targets that profile.
    let add = kmd_at(
        tmp.path(),
        &[("KOMMAND0_PROFILE", "work")],
        &["repo", "add", repo.to_str().unwrap()],
    );
    assert!(add.status.success(), "repo add: {}", String::from_utf8_lossy(&add.stderr));
    assert!(
        dev.join("profiles").join("work").join("state.json").exists(),
        "state landed under profiles/work/"
    );
    let plain = kmd_at(tmp.path(), &[], &["repo", "list"]);
    assert!(plain.status.success());
    assert!(
        !stdout(&plain).contains("proj-env"),
        "isolated from the default profile: {}",
        stdout(&plain)
    );

    // An explicit --profile beats the inherited variable.
    let other = kmd_at(
        tmp.path(),
        &[("KOMMAND0_PROFILE", "work")],
        &["--profile", "other", "repo", "add", repo.to_str().unwrap()],
    );
    assert!(other.status.success(), "add w/ flag: {}", String::from_utf8_lossy(&other.stderr));
    assert!(
        dev.join("profiles").join("other").join("state.json").exists(),
        "flag wins: state landed under profiles/other/"
    );
    let work = stdout(&kmd_at(tmp.path(), &[("KOMMAND0_PROFILE", "work")], &["repo", "list"]));
    assert!(work.contains("proj-env"), "work profile keeps its own repo: {work}");
}

#[cfg(debug_assertions)]
#[test]
fn migration_respects_a_config_env_override_end_to_end() {
    // Pins the env wire into migrate_legacy_profiles: with KOMMAND0_CONFIG
    // set, state migrates but config.json stays at the legacy root.
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join(".kommand0-dev");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("state.json"), r#"{"repos":[]}"#).unwrap();
    std::fs::write(base.join("config.json"), "{}").unwrap();
    let other_cfg = tmp.path().join("other-config.json");
    std::fs::write(&other_cfg, "{}").unwrap();

    let out = kmd_at(
        tmp.path(),
        &[("KOMMAND0_CONFIG", other_cfg.to_str().unwrap())],
        &["repo", "list"],
    );
    assert!(out.status.success(), "list: {}", String::from_utf8_lossy(&out.stderr));
    let dflt = base.join("profiles").join("default");
    assert!(dflt.join("state.json").exists(), "state migrated");
    assert!(base.join("config.json").exists(), "config.json stays at the root");
    assert!(!dflt.join("config.json").exists());

    // Without the override (fresh layout): both migrate.
    let tmp2 = tempfile::tempdir().unwrap();
    let base2 = tmp2.path().join(".kommand0-dev");
    std::fs::create_dir_all(&base2).unwrap();
    std::fs::write(base2.join("state.json"), r#"{"repos":[]}"#).unwrap();
    std::fs::write(base2.join("config.json"), "{}").unwrap();
    let out = kmd_at(tmp2.path(), &[], &["repo", "list"]);
    assert!(out.status.success(), "list: {}", String::from_utf8_lossy(&out.stderr));
    let dflt2 = base2.join("profiles").join("default");
    assert!(
        dflt2.join("state.json").exists() && dflt2.join("config.json").exists(),
        "both migrated without the override"
    );
    assert!(!base2.join("config.json").exists());
}

#[cfg(debug_assertions)]
#[test]
fn profile_rename_moves_state_and_rejects_occupied_target() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("proj-ren");
    std::fs::create_dir_all(&repo).unwrap();
    let add =
        kmd_at(tmp.path(), &[], &["--profile", "work", "repo", "add", repo.to_str().unwrap()]);
    assert!(add.status.success(), "repo add: {}", String::from_utf8_lossy(&add.stderr));

    let out = kmd_at(tmp.path(), &[], &["profile", "rename", "work", "personal"]);
    assert!(out.status.success(), "rename: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout(&out).contains("Renamed profile"), "summary printed: {}", stdout(&out));

    let personal = stdout(&kmd_at(tmp.path(), &[], &["--profile", "personal", "repo", "list"]));
    assert!(personal.contains("proj-ren"), "state followed the rename: {personal}");
    assert!(
        !tmp.path().join(".kommand0-dev").join("profiles").join("work").exists(),
        "old dir gone"
    );
    let work = stdout(&kmd_at(tmp.path(), &[], &["--profile", "work", "repo", "list"]));
    assert!(!work.contains("proj-ren"), "old profile name is fresh: {work}");

    // Renaming onto an existing profile is refused.
    let other = tmp.path().join("proj-other");
    std::fs::create_dir_all(&other).unwrap();
    let add =
        kmd_at(tmp.path(), &[], &["--profile", "other", "repo", "add", other.to_str().unwrap()]);
    assert!(add.status.success(), "repo add: {}", String::from_utf8_lossy(&add.stderr));
    let dup = kmd_at(tmp.path(), &[], &["profile", "rename", "personal", "other"]);
    assert!(!dup.status.success(), "occupied target must fail");
    assert!(
        String::from_utf8_lossy(&dup.stderr).contains("already exists"),
        "clear error: {}",
        String::from_utf8_lossy(&dup.stderr)
    );
}

#[cfg(debug_assertions)]
#[test]
fn profile_rename_rewrites_worktree_and_session_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "1").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "init"]);

    let add =
        kmd_at(tmp.path(), &[], &["--profile", "work", "repo", "add", repo.to_str().unwrap()]);
    assert!(add.status.success(), "repo add: {}", String::from_utf8_lossy(&add.stderr));
    let create = kmd_at(
        tmp.path(),
        &[],
        &["--profile", "work", "workspace", "create", "feat", "--repo", repo.to_str().unwrap()],
    );
    assert!(create.status.success(), "create: {}", String::from_utf8_lossy(&create.stderr));

    // Seed a session row whose log path is the relative form
    // create_session_with_base stores when the base (the debug state dir) is
    // relative — pinning the base-verbatim rewrite arm alongside the
    // cwd-absolutized worktree arm.
    let state_path =
        tmp.path().join(".kommand0-dev").join("profiles").join("work").join("state.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    let ws_id = v["workspaces"][0]["id"].as_str().unwrap().to_string();
    v["sessions"] = serde_json::json!([{
        "id": "s1", "workspace_id": ws_id, "claude_session_id": null, "pid": null,
        "status": "Stopped", "created_at": 0, "ended_at": null,
        "log_file": ".kommand0-dev/profiles/work/sessions/s1.log"
    }]);
    std::fs::write(&state_path, v.to_string()).unwrap();

    let out = kmd_at(tmp.path(), &[], &["profile", "rename", "work", "personal"]);
    assert!(out.status.success(), "rename: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        stdout(&out).contains("2 worktree/session path(s) rewritten"),
        "count wired through to the summary: {}",
        stdout(&out)
    );

    // The repo's gitdir link follows (repair ran)…
    let list =
        Command::new("git").args(["worktree", "list"]).current_dir(&repo).output().unwrap();
    let list = String::from_utf8_lossy(&list.stdout).to_string();
    assert!(list.contains("profiles/personal/worktrees/feat"), "gitdir repaired: {list}");

    // …and the profile's state carries both rewritten paths.
    let new_state = std::fs::read_to_string(
        tmp.path().join(".kommand0-dev").join("profiles").join("personal").join("state.json"),
    )
    .unwrap();
    assert!(
        new_state.contains("profiles/personal/worktrees/feat"),
        "worktree path rewritten: {new_state}"
    );
    assert!(
        new_state.contains(".kommand0-dev/profiles/personal/sessions/s1.log"),
        "relative session log rewritten: {new_state}"
    );
    assert!(!new_state.contains("profiles/work/"), "no old-path residue: {new_state}");
}

#[test]
fn profile_rename_unavailable_with_state_dir_env() {
    let tmp = tempfile::tempdir().unwrap();
    let out = kmd(tmp.path(), &[], &["profile", "rename", "a", "b"]);
    assert!(!out.status.success(), "env-mode rename must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unavailable when KOMMAND0_STATE_DIR"),
        "clear error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
