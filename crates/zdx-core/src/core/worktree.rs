//! Git worktree management helpers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

/// Ensures a git worktree exists for the given ID.
///
/// Returns the worktree path (existing or newly created).
///
/// # Errors
/// Returns an error if the operation fails.
pub fn ensure_worktree(root: &Path, id: &str) -> Result<PathBuf> {
    let repo_root = git_root(root)?;
    let worktree_path = worktree_path_for_id(&repo_root, id);

    if is_worktree_registered(&repo_root, &worktree_path)? {
        return Ok(worktree_path);
    }

    if worktree_path.exists() {
        bail!(
            "Worktree path exists but is not registered: {}",
            worktree_path.display()
        );
    }

    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create worktree directory {}", parent.display()))?;
    }

    let branch_name = format!("zdx/{}", sanitize_branch_name(id));
    let branch_exists = git_branch_exists(&repo_root, &branch_name)?;

    let add_result = if branch_exists {
        git_worktree_add_existing(&repo_root, &worktree_path, &branch_name)
    } else {
        git_worktree_add_new(&repo_root, &worktree_path, &branch_name)
    };

    if let Err(err) = add_result {
        if is_worktree_registered(&repo_root, &worktree_path)? {
            return Ok(worktree_path);
        }
        return Err(err);
    }

    if is_worktree_registered(&repo_root, &worktree_path)? {
        return Ok(worktree_path);
    }

    Err(anyhow!(
        "Worktree creation did not register: {}",
        worktree_path.display()
    ))
}

fn git_root(root: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("git rev-parse --show-toplevel")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git rev-parse failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        bail!("git rev-parse returned empty repo root");
    }

    Ok(PathBuf::from(trimmed))
}

fn worktree_path_for_id(repo_root: &Path, id: &str) -> PathBuf {
    let base_dir = worktree_base_dir(repo_root);
    let segment = sanitize_segment(id);
    base_dir.join(segment)
}

fn worktree_base_dir(repo_root: &Path) -> PathBuf {
    let parent = repo_root.parent().unwrap_or(repo_root);
    let repo_name = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let hash = stable_hash(&repo_root.display().to_string());
    parent
        .join(".zdx")
        .join("worktrees")
        .join(format!("{repo_name}-{hash}"))
}

fn is_worktree_registered(repo_root: &Path, worktree_path: &Path) -> Result<bool> {
    let list = git_worktree_list(repo_root)?;
    let target = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    Ok(list.into_iter().any(|path| {
        let candidate = path.canonicalize().unwrap_or(path);
        candidate == target
    }))
}

fn git_worktree_list(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .context("git worktree list --porcelain")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree list failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut paths = Vec::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("worktree ")
            && !rest.trim().is_empty()
        {
            paths.push(PathBuf::from(rest.trim()));
        }
    }
    Ok(paths)
}

fn git_branch_exists(repo_root: &Path, branch_name: &str) -> Result<bool> {
    let ref_name = format!("refs/heads/{branch_name}");
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show-ref", "--verify", "--quiet", &ref_name])
        .status()
        .context("git show-ref --verify")?;
    Ok(status.success())
}

fn git_worktree_add_new(repo_root: &Path, path: &Path, branch_name: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(branch_name)
        .arg(path)
        .arg("HEAD")
        .output()
        .context("git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {}", stderr.trim());
    }

    Ok(())
}

fn git_worktree_add_existing(repo_root: &Path, path: &Path, branch_name: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("worktree")
        .arg("add")
        .arg(path)
        .arg(branch_name)
        .output()
        .context("git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {}", stderr.trim());
    }

    Ok(())
}

fn sanitize_segment(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    let trimmed = if trimmed.is_empty() {
        "session"
    } else {
        trimmed
    };
    trimmed.chars().take(64).collect()
}

fn sanitize_branch_name(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    let trimmed = if trimmed.is_empty() {
        "session"
    } else {
        trimmed
    };
    trimmed.chars().take(64).collect()
}

fn stable_hash(input: &str) -> String {
    // FNV-1a 64-bit
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}
