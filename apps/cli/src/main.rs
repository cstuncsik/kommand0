use std::io::{IsTerminal, Write};

use clap::{Parser, Subcommand};
use kommand0_core::AppState;
use kommand0_core::workspace::format_timestamp;

#[derive(Parser)]
#[command(name = "kmd", version, about = "Keyboard-first local orchestrator for parallel coding sessions")]
struct Cli {
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
}

#[derive(Subcommand)]
enum WorkspaceAction {
    /// Create a new workspace
    Create {
        /// Workspace name (auto-generated from repo name if omitted)
        name: Option<String>,
        /// Repo reference (name, path, or ID)
        #[arg(long)]
        repo: String,
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

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
        },
        Commands::Workspace { action } => match action {
            WorkspaceAction::Create { name, repo } => {
                let mut state = AppState::load()?;
                let ws = state.create_workspace(name.as_deref(), &repo)?;
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
                println!("Repo:    {} ({})", repo_name, repo_path);
                println!("Dir:     {}", ws.working_dir);
                println!("Status:  {}", status);
                println!("Created: {}", format_timestamp(ws.created_at));
            }
            WorkspaceAction::Delete { name, force } => {
                if !force {
                    let stdin = std::io::stdin();
                    if !stdin.is_terminal() {
                        eprintln!("error: refusing to delete without --force in non-interactive mode");
                        std::process::exit(1);
                    }
                    print!("Delete workspace '{}'? [y/N] ", name);
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
                println!("Archived workspace: {}", name);
            }
            WorkspaceAction::Activate { name } => {
                let mut state = AppState::load()?;
                state.activate_workspace(&name)?;
                println!("Activated workspace: {}", name);
            }
        },
    }

    Ok(())
}
