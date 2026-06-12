use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEntry {
    pub id: String,
    pub name: String,
    pub path: String,
}

pub fn run_git_status(repo_path: &str) -> anyhow::Result<String> {
    let p = std::path::Path::new(repo_path);
    if !p.exists() {
        bail!("path does not exist: {repo_path}");
    }
    if !p.is_dir() {
        bail!("path is not a directory: {repo_path}");
    }

    let output = std::process::Command::new("git")
        .args(["-C", repo_path, "status", "--short", "--branch"])
        .output()
        .with_context(|| format!("failed to run git in {repo_path}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        bail!("git status failed: {}", stderr.trim())
    }
}
