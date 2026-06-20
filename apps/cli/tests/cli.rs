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
    cmd.args(args).env("KOMMAND0_STATE_DIR", state_dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
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
/// own `kommand0/<name>` branch. Returns the state dir.
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
    let tmp = tempfile::tempdir().unwrap();
    let state = setup(tmp.path());
    let out = kmd(&state, &[], &["workspace", "status"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("BRANCH"), "has a header: {text}");
    assert!(text.contains("kommand0/feat"), "shows the workspace branch: {text}");
}

#[test]
fn open_pr_prints_the_url() {
    let tmp = tempfile::tempdir().unwrap();
    let state = setup(tmp.path());
    let gh = tmp.path().join("gh");
    write_stub(
        &gh,
        "#!/bin/sh\nif [ \"$1\" = pr ] && [ \"$2\" = create ]; then echo https://github.com/x/y/pull/7; exit 0; fi\nexit 1\n",
    );
    let out = kmd(
        &state,
        &[("KOMMAND0_GH_BIN", gh.to_str().unwrap())],
        &["workspace", "open-pr", "feat"],
    );
    assert!(out.status.success(), "open-pr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&out).trim(), "https://github.com/x/y/pull/7");
}

#[test]
fn cleanup_removes_a_merged_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let state = setup(tmp.path());
    let gh = tmp.path().join("gh");
    // gh runs from the repo dir; report the branch's tip as the merged commit.
    write_stub(
        &gh,
        "#!/bin/sh\nif [ \"$1\" = pr ] && [ \"$2\" = view ]; then oid=$(git rev-parse \"refs/heads/$3\"); printf 'MERGED\\n%s\\n' \"$oid\"; exit 0; fi\nexit 1\n",
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
fn open_pr_without_a_branch_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    // Seed a fallback workspace (no worktree/branch) directly.
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let json = serde_json::json!({
        "repos": [{ "id": "r1", "name": "demo", "path": repo.to_str().unwrap() }],
        "workspaces": [{
            "id": "w1", "name": "fallback", "repo_id": "r1",
            "working_dir": repo.to_str().unwrap(), "active": true, "created_at": 0
        }],
        "sessions": []
    });
    std::fs::write(state_dir.join("state.json"), json.to_string()).unwrap();
    let out = kmd(&state_dir, &[], &["workspace", "open-pr", "fallback"]);
    assert!(!out.status.success(), "should fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no branch"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
