//! Worktree command handlers.

use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_core::core::worktree;

pub fn ensure(root: &str, id: &str) -> Result<()> {
    let root_path = PathBuf::from(root);
    let path = worktree::ensure_worktree(&root_path, id)
        .with_context(|| format!("ensure worktree for '{id}'"))?;
    println!("{}", path.display());
    Ok(())
}

pub fn remove(path: Option<&str>) -> Result<()> {
    let worktree_path = path.map_or_else(|| PathBuf::from("."), PathBuf::from);
    let info = worktree::remove_worktree_at(&worktree_path)?;
    println!("Removed worktree: {}", info.worktree_path.display());
    if let Some(branch) = &info.branch {
        println!("Deleted branch: {branch}");
    }
    println!("Project root: {}", info.project_root.display());
    Ok(())
}
