//! Worktree command handlers.

use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_core::core::worktree;

pub fn ensure(root: &str, id: &str) -> Result<()> {
    let root_path = PathBuf::from(root);
    let path = worktree::ensure_worktree(&root_path, id)
        .with_context(|| format!("ensure worktree for '{}'", id))?;
    println!("{}", path.display());
    Ok(())
}
