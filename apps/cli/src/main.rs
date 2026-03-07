use clap::{Parser, Subcommand};
use kommand0_core::AppState;

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
    }

    Ok(())
}
