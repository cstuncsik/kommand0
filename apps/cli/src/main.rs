use std::io::{IsTerminal, Write};
use std::os::unix::process::CommandExt; // for Command::process_group

use clap::{Parser, Subcommand};
use kommand0_core::workspace::format_timestamp;
use kommand0_core::{
    AppState, SessionStatus, Workspace, branch_status, cleanup_merged_workspace,
};

#[derive(Parser)]
#[command(name = "kmd", version, about = "Keyboard-first local orchestrator for parallel coding sessions")]
struct Cli {
    /// Run against an isolated profile (own state, config, log, sessions, worktrees)
    #[arg(long, global = true, value_name = "NAME")]
    profile: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage tracked repos
    Repo {
        #[command(subcommand)]
        action: RepoAction,
    },
    /// Manage workspaces
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
    /// Manage sessions
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Manage profiles
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
}

#[derive(Subcommand)]
enum ProfileAction {
    /// Rename a profile directory, rewriting workspace/session paths and
    /// repairing git worktree links
    Rename {
        /// Current profile name
        old: String,
        /// New profile name
        new: String,
    },
}

#[derive(Subcommand)]
enum RepoAction {
    /// Add a repo by path
    Add {
        /// Path to the repository directory
        path: String,
    },
    /// List all tracked repos
    List,
    /// Delete a tracked repo and all its workspaces/sessions
    Delete {
        /// Repo reference (name, path, or ID)
        name: String,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum WorkspaceAction {
    /// Create a new workspace
    Create {
        /// Workspace name (auto-generated from the repo or branch if omitted)
        name: Option<String>,
        /// Repo reference (name, path, or ID)
        #[arg(long)]
        repo: String,
        /// Check out an EXISTING branch (local, or a remote `origin/…` ref)
        /// instead of forking a new one
        #[arg(long)]
        branch: Option<String>,
        /// Skip git worktree creation (use repo root as working directory)
        #[arg(long)]
        no_worktree: bool,
        /// Force forking a new branch even if a branch `<name>` exists (the fork
        /// gets a `-2`/`-3` suffix then; skips the existing-branch checkout prompt)
        #[arg(long, conflicts_with_all = ["branch", "no_worktree"])]
        fork: bool,
    },
    /// List workspaces
    List {
        /// Show archived workspaces too
        #[arg(long)]
        all: bool,
        /// Filter by repo reference
        #[arg(long)]
        repo: Option<String>,
    },
    /// Show workspace details
    Show {
        /// Workspace name
        name: String,
    },
    /// Delete a workspace
    Delete {
        /// Workspace name
        name: String,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
    /// Archive a workspace (set inactive)
    Archive {
        /// Workspace name
        name: String,
    },
    /// Activate a workspace (set active)
    Activate {
        /// Workspace name
        name: String,
    },
    /// Show git branch/diff status (one workspace, or all)
    Status {
        /// Workspace name (omit for all)
        name: Option<String>,
    },
    /// Remove a merged workspace's worktree and delete its branch
    Cleanup {
        /// Workspace name
        name: String,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// Start a Claude session in a workspace
    Start {
        /// Workspace name
        workspace: String,
    },
    /// Stop a running session
    Stop {
        /// Workspace name
        workspace: String,
    },
    /// List sessions (optionally for one workspace)
    List {
        /// Only sessions for this workspace
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Clear session metadata and log file
    Clear {
        /// Workspace name
        workspace: String,
    },
}

/// Print one `workspace status` table row (looks up live git status).
fn print_status_row(ws: &Workspace) {
    let (branch, ahead, behind, dirty) = match &ws.worktree_path {
        Some(_) => match branch_status(&ws.working_dir) {
            Some(s) => (
                s.branch
                    .clone()
                    .or_else(|| ws.branch_name.clone())
                    .unwrap_or_else(|| "(detached)".into()),
                s.ahead.to_string(),
                s.behind.to_string(),
                if s.dirty { "yes" } else { "no" }.to_string(),
            ),
            None => (
                ws.branch_name.clone().unwrap_or_else(|| "?".into()),
                "-".into(),
                "-".into(),
                "-".into(),
            ),
        },
        None => (
            "(shared checkout)".into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ),
    };
    println!("{:<20} {:<26} {ahead:>6} {behind:>6}  {dirty}", ws.name, branch);
}

/// Interpret the reply to the existing-branch checkout prompt. Case-insensitive
/// and whitespace-trimmed; `n`/`no` fork, everything else (empty Enter, `y`/`yes`,
/// or an unrecognised reply) defaults to checkout — it's non-destructive, so the
/// safe default is yes (deliberately unlike cleanup's destructive `[y/N]`).
fn wants_checkout(answer: &str) -> bool {
    !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no")
}

fn main() -> anyhow::Result<()> {
    // Surface core's diagnostics (e.g. a failed worktree removal on delete/
    // cleanup) on stderr — fine for a CLI (no alt-screen to corrupt).
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .try_init();

    let cli = Cli::parse();

    // Resolve + record the profile (--profile, else an inherited
    // KOMMAND0_PROFILE; an invalid name / env conflict aborts) before any
    // state, config, or log access resolves a directory.
    AppState::init_profile(cli.profile.as_deref()).map_err(|e| anyhow::anyhow!(e))?;
    AppState::migrate_legacy_profiles()?;

    match cli.command {
        Commands::Repo { action } => match action {
            RepoAction::Add { path } => {
                let mut state = AppState::load()?;
                let entry = state.add_repo(&path)?;
                println!("Added repo: {} ({})", entry.name, entry.path);
            }
            RepoAction::List => {
                let state = AppState::load()?;
                if state.repos.is_empty() {
                    println!("No repos tracked. Use `kmd repo add <path>` to add one.");
                } else {
                    for repo in &state.repos {
                        println!("{}  {}  {}", repo.id, repo.name, repo.path);
                    }
                }
            }
            RepoAction::Delete { name, force } => {
                let mut state = AppState::load()?;
                let repo = state.resolve_repo(&name)?.clone();
                let ws_count = state.workspaces.iter().filter(|w| w.repo_id == repo.id).count();

                if !force {
                    let stdin = std::io::stdin();
                    if !stdin.is_terminal() {
                        eprintln!("error: refusing to delete without --force in non-interactive mode");
                        std::process::exit(1);
                    }
                    if ws_count > 0 {
                        print!(
                            "Delete repo '{}' and its {} workspace(s)? [y/N] ",
                            repo.name, ws_count
                        );
                    } else {
                        print!("Delete repo '{}'? [y/N] ", repo.name);
                    }
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    stdin.read_line(&mut input)?;
                    if !matches!(input.trim(), "y" | "Y") {
                        println!("Cancelled.");
                        return Ok(());
                    }
                }

                state.delete_repo(&name)?;
                println!("Deleted repo: {} ({} workspace(s) removed)", repo.name, ws_count);
            }
        },
        Commands::Workspace { action } => match action {
            WorkspaceAction::Create { name, repo, branch, no_worktree, fork } => {
                let mut state = AppState::load()?;
                let ws = match (branch, no_worktree) {
                    (Some(_), true) => {
                        anyhow::bail!("--branch and --no-worktree can't be combined")
                    }
                    (Some(b), false) => {
                        state.create_workspace_from_branch(name.as_deref(), &repo, &b)?
                    }
                    (None, true) => state.create_workspace_with_options(
                        name.as_deref(),
                        &repo,
                        AppState::state_dir().as_path(),
                        false,
                    )?,
                    (None, false) => {
                        // Gate exactly like the TUI (tui/main.rs): only a valid,
                        // unused name whose bare branch already exists opens the
                        // offer — otherwise fall through to create, which surfaces
                        // core's canonical error (so the note never lies about a
                        // fork that then fails). `--fork` and the no-name case skip
                        // detection entirely.
                        let offer = match (&name, fork) {
                            (Some(n), false) => state.validate_new_workspace_name(n).is_ok()
                                && kommand0_core::worktree::branch_exists_bare(
                                    &state.resolve_repo(&repo)?.path,
                                    n,
                                ),
                            _ => false,
                        };
                        let ws = if !offer {
                            state.create_workspace(name.as_deref(), &repo)?
                        } else {
                            let n = name.as_deref().expect("offer implies Some(name)");
                            if std::io::stdin().is_terminal() {
                                print!("Branch '{n}' already exists. Check it out? [Y/n] ");
                                std::io::stdout().flush()?;
                                let mut input = String::new();
                                std::io::stdin().read_line(&mut input)?;
                                if wants_checkout(&input) {
                                    state.create_workspace_from_branch(Some(n), &repo, n)?
                                } else {
                                    state.create_workspace(Some(n), &repo)?
                                }
                            } else {
                                state.create_workspace(Some(n), &repo)?
                            }
                        };
                        // Report the branch actually created whenever it isn't the
                        // requested name — `unique_branch_name` suffixes on collision
                        // (`{name}-2`), which `--fork` and the non-interactive
                        // fallthrough would otherwise do silently. Scoped to this
                        // fresh-branch arm: an adopted `--branch` workspace differs
                        // legitimately, and a worktree fallback has no branch.
                        if let Some(b) = &ws.branch_name
                            && *b != ws.name
                        {
                            eprintln!(
                                "note: branch '{}' exists; forked {b} (use --branch {} to check it out)",
                                ws.name, ws.name
                            );
                        }
                        ws
                    }
                };
                let repo_name = state
                    .repos
                    .iter()
                    .find(|r| r.id == ws.repo_id)
                    .map(|r| r.name.as_str())
                    .unwrap_or("(unknown)");
                println!("Created workspace: {} (repo: {})", ws.name, repo_name);
            }
            WorkspaceAction::List { all, repo } => {
                let state = AppState::load()?;
                let workspaces = state.list_workspaces(all, repo.as_deref())?;
                if workspaces.is_empty() {
                    println!("No workspaces found.");
                } else {
                    println!(
                        "{:<12} {:<20} {:<20} {:<10}",
                        "ID", "NAME", "REPO", "STATUS"
                    );
                    for ws in workspaces {
                        let repo_name = state
                            .repos
                            .iter()
                            .find(|r| r.id == ws.repo_id)
                            .map(|r| r.name.as_str())
                            .unwrap_or("(unknown)");
                        let status = if ws.active { "active" } else { "archived" };
                        println!(
                            "{:<12} {:<20} {:<20} {:<10}",
                            ws.id, ws.name, repo_name, status
                        );
                    }
                }
            }
            WorkspaceAction::Show { name } => {
                let state = AppState::load()?;
                let ws = state.show_workspace(&name)?;
                let repo = state.repos.iter().find(|r| r.id == ws.repo_id);
                let (repo_name, repo_path) = match repo {
                    Some(r) => (r.name.as_str(), r.path.as_str()),
                    None => ("(unknown)", "(unknown)"),
                };
                let status = if ws.active { "active" } else { "archived" };
                println!("Name:    {}", ws.name);
                println!("ID:      {}", ws.id);
                println!("Repo:    {repo_name} ({repo_path})");
                println!("Dir:     {}", ws.working_dir);
                println!("Status:  {status}");
                println!("Created: {}", format_timestamp(ws.created_at));
            }
            WorkspaceAction::Delete { name, force } => {
                if !force {
                    let stdin = std::io::stdin();
                    if !stdin.is_terminal() {
                        eprintln!("error: refusing to delete without --force in non-interactive mode");
                        std::process::exit(1);
                    }
                    print!("Delete workspace '{name}'? [y/N] ");
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    stdin.read_line(&mut input)?;
                    if !matches!(input.trim(), "y" | "Y") {
                        println!("Cancelled.");
                        return Ok(());
                    }
                }
                let mut state = AppState::load()?;
                let ws = state.delete_workspace(&name)?;
                let repo_name = state
                    .repos
                    .iter()
                    .find(|r| r.id == ws.repo_id)
                    .map(|r| r.name.as_str())
                    .unwrap_or("(unknown)");
                println!("Deleted workspace: {} (repo: {})", ws.name, repo_name);
            }
            WorkspaceAction::Archive { name } => {
                let mut state = AppState::load()?;
                state.archive_workspace(&name)?;
                println!("Archived workspace: {name}");
            }
            WorkspaceAction::Activate { name } => {
                let mut state = AppState::load()?;
                state.activate_workspace(&name)?;
                println!("Activated workspace: {name}");
            }
            WorkspaceAction::Status { name } => {
                let state = AppState::load()?;
                let targets: Vec<&Workspace> = match &name {
                    Some(n) => vec![state.show_workspace(n)?],
                    None => state.workspaces.iter().collect(),
                };
                if targets.is_empty() {
                    println!("No workspaces found.");
                } else {
                    println!("{:<20} {:<26} {:>6} {:>6}  DIRTY", "NAME", "BRANCH", "AHEAD", "BEHIND");
                    for ws in targets {
                        print_status_row(ws);
                    }
                }
            }
            WorkspaceAction::Cleanup { name, force } => {
                let mut state = AppState::load()?;
                let ws = state.show_workspace(&name)?.clone();
                let (Some(worktree), Some(branch)) =
                    (ws.worktree_path.clone(), ws.branch_name.clone())
                else {
                    anyhow::bail!("workspace '{name}' has no worktree/branch to clean up");
                };
                let repo = state
                    .repos
                    .iter()
                    .find(|r| r.id == ws.repo_id)
                    .map(|r| r.path.clone())
                    .ok_or_else(|| anyhow::anyhow!("repo not found for workspace '{name}'"))?;

                if !force {
                    let stdin = std::io::stdin();
                    if !stdin.is_terminal() {
                        eprintln!("error: refusing to clean up without --force in non-interactive mode");
                        std::process::exit(1);
                    }
                    print!("Clean up '{name}' (remove worktree, delete branch {branch})? [y/N] ");
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    stdin.read_line(&mut input)?;
                    if !matches!(input.trim(), "y" | "Y") {
                        println!("Cancelled.");
                        return Ok(());
                    }
                }

                match cleanup_merged_workspace(&repo, &worktree, &branch) {
                    Ok(()) => {
                        // The worktree + branch are gone — drop the workspace entry.
                        state.delete_workspace(&name)?;
                        println!("Cleaned up workspace: {name}");
                    }
                    Err(e) => anyhow::bail!("{e}"),
                }
            }
        },
        Commands::Session { action } => match action {
            SessionAction::Start { workspace } => {
                let mut state = AppState::load()?;
                let ws = state.show_workspace(&workspace)?.clone();

                // Check no running session
                if let Some(s) = state.find_session_by_workspace(&ws.id)
                    && s.status == SessionStatus::Running {
                        anyhow::bail!("Workspace '{}' already has a running session ({})", workspace, s.id);
                    }

                // Create session record
                let session = state.create_session(&ws.id)?;
                let session_id = session.id.clone();

                // Spawn claude process (non-async, fire-and-forget for CLI).
                // Honor KOMMAND0_CLAUDE_BIN (tests / ad-hoc override), else `claude`.
                let claude_bin = std::env::var("KOMMAND0_CLAUDE_BIN")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "claude".to_string());
                let child = std::process::Command::new(&claude_bin)
                    .args([
                        "-p",
                        "--verbose",
                        "--input-format", "stream-json",
                        "--output-format", "stream-json",
                    ])
                    .current_dir(&ws.working_dir)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .env_remove("CLAUDECODE")
                    // Put the child in its OWN process group so `session stop`'s
                    // `kill(-pgid)` reaches it (and its descendants) instead of
                    // kmd's own group. Without this the child is never reliably
                    // killed — the group `child_pid` simply doesn't exist.
                    .process_group(0)
                    .spawn();

                match child {
                    Ok(child) => {
                        let pid = child.id();
                        // Update session with PID
                        if let Some(s) = state.find_session_mut(&session_id) {
                            s.pid = Some(pid);
                        }
                        state.save()?;
                        println!("Started session for workspace: {workspace}");
                        println!("Session ID: {session_id}");
                        println!("PID: {pid}");
                    }
                    Err(e) => {
                        let _ = state.update_session_status(&session_id, SessionStatus::Failed);
                        anyhow::bail!("Failed to start claude process: {e}");
                    }
                }
            }
            SessionAction::Stop { workspace } => {
                let mut state = AppState::load()?;
                let ws = state.show_workspace(&workspace)?.clone();

                let session = state.find_session_by_workspace(&ws.id)
                    .filter(|s| s.status == SessionStatus::Running)
                    .ok_or_else(|| anyhow::anyhow!("No running session for workspace: {workspace}"))?;

                let session_id = session.id.clone();
                let pid = session.pid;

                if let Some(pid) = pid {
                    use nix::sys::signal::{Signal, kill};
                    use nix::unistd::Pid;

                    // Send SIGTERM to process group
                    let pgid = pid as i32;
                    let _ = kill(Pid::from_raw(-pgid), Signal::SIGTERM);

                    // Wait briefly, then SIGKILL if needed
                    std::thread::sleep(std::time::Duration::from_secs(1));

                    // Check if still running via kill(pid, 0)
                    if kill(Pid::from_raw(pgid), None).is_ok() {
                        let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
                    }
                }

                state.update_session_status(&session_id, SessionStatus::Stopped)?;
                println!("Stopped session for workspace: {workspace}");
            }
            SessionAction::List { workspace } => {
                let state = AppState::load()?;
                let ws_filter = match &workspace {
                    Some(n) => Some(state.show_workspace(n)?.id.clone()),
                    None => None,
                };
                let sessions: Vec<&_> = state
                    .list_sessions()
                    .iter()
                    .filter(|s| ws_filter.as_ref().is_none_or(|id| &s.workspace_id == id))
                    .collect();
                if sessions.is_empty() {
                    println!("No sessions.");
                } else {
                    println!(
                        "{:<38} {:<20} {:<10} {:<8} {:<20}",
                        "SESSION_ID", "WORKSPACE", "STATUS", "PID", "CREATED"
                    );
                    for session in sessions {
                        let ws_name = state.workspaces.iter()
                            .find(|w| w.id == session.workspace_id)
                            .map(|w| w.name.as_str())
                            .unwrap_or("(unknown)");
                        let status = match session.status {
                            SessionStatus::Running => "running",
                            SessionStatus::Stopped => "stopped",
                            SessionStatus::Failed => "failed",
                            SessionStatus::Exited => "exited",
                        };
                        let pid_str = session.pid
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "-".to_string());
                        println!(
                            "{:<38} {:<20} {:<10} {:<8} {:<20}",
                            session.id, ws_name, status, pid_str, format_timestamp(session.created_at)
                        );
                    }
                }
            }
            SessionAction::Clear { workspace } => {
                let mut state = AppState::load()?;
                let ws = state.show_workspace(&workspace)?.clone();

                // Remove ALL sessions for this workspace and delete their log files
                let mut cleared = 0;
                state.sessions.retain(|s| {
                    if s.workspace_id == ws.id {
                        let log_path = std::path::Path::new(&s.log_file);
                        if log_path.exists() {
                            let _ = std::fs::remove_file(log_path);
                        }
                        cleared += 1;
                        false // remove
                    } else {
                        true // keep
                    }
                });

                if cleared > 0 {
                    state.save()?;
                    println!("Cleared {cleared} session(s) for workspace: {workspace}");
                } else {
                    println!("No session found for workspace: {workspace}");
                }
            }
        },
        Commands::Profile { action } => match action {
            ProfileAction::Rename { old, new } => {
                let (rewritten, warnings) = AppState::rename_profile(&old, &new)?;
                for w in &warnings {
                    eprintln!("warning: {w}");
                }
                println!(
                    "Renamed profile: {old} → {new} ({rewritten} worktree/session path(s) rewritten)"
                );
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wants_checkout_defaults_to_yes() {
        // Empty (bare Enter) and any affirmative → check out.
        for yes in ["", "y", "yes", "Y", "maybe"] {
            assert!(wants_checkout(yes), "{yes:?} should check out");
        }
        // Only an explicit no forks (case- and whitespace-insensitive).
        for no in ["n", "no", "N", "No", "n\n", " n "] {
            assert!(!wants_checkout(no), "{no:?} should fork");
        }
    }

    #[test]
    fn fork_conflicts_with_branch_and_no_worktree() {
        // `conflicts_with_all` rejects these declaratively — no hand-rolled bail.
        assert!(
            Cli::try_parse_from([
                "kmd", "workspace", "create", "x", "--repo", "r", "--fork", "--branch", "b",
            ])
            .is_err(),
            "--fork + --branch must be rejected"
        );
        assert!(
            Cli::try_parse_from([
                "kmd", "workspace", "create", "x", "--repo", "r", "--fork", "--no-worktree",
            ])
            .is_err(),
            "--fork + --no-worktree must be rejected"
        );
    }
}
