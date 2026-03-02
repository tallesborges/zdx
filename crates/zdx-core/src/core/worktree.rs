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

/// Info about a removed worktree.
pub struct RemovedWorktree {
    pub worktree_path: PathBuf,
    pub branch: Option<String>,
    pub project_root: PathBuf,
}

/// Removes the worktree at `worktree_path` and deletes its associated branch.
///
/// The path must be a git worktree (not the main working tree).
/// Uses `--git-common-dir` to find the main repo, then:
/// 1. `git worktree remove --force <path>`
/// 2. `git branch -D <branch>` (if branch found)
///
/// Returns info about what was removed.
///
/// # Errors
/// Returns an error if the path is not a worktree or removal fails.
pub fn remove_worktree_at(worktree_path: &Path) -> Result<RemovedWorktree> {
    let worktree_path = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());

    // 1. Find the main .git dir via --git-common-dir
    let output = Command::new("git")
        .arg("-C")
        .arg(&worktree_path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .context("git rev-parse --git-common-dir")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git rev-parse failed: {}", stderr.trim());
    }

    let git_common_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());

    // 2. project_root = parent of git-common-dir
    let project_root = git_common_dir
        .parent()
        .ok_or_else(|| {
            anyhow!(
                "cannot derive project root from {}",
                git_common_dir.display()
            )
        })?
        .to_path_buf();

    let project_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.clone());

    // 3. Must not be the main working tree
    if project_root == worktree_path {
        bail!(
            "Not a worktree: {} is the main working tree",
            worktree_path.display()
        );
    }

    // 4. Get branch from porcelain worktree list
    let branch = extract_worktree_branch(&project_root, &worktree_path)?;

    // 5. Remove the worktree
    let output = Command::new("git")
        .arg("-C")
        .arg(&project_root)
        .args(["worktree", "remove", "--force"])
        .arg(&worktree_path)
        .output()
        .context("git worktree remove")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree remove failed: {}", stderr.trim());
    }

    // 6. Delete the branch if found
    if let Some(ref branch) = branch {
        let output = Command::new("git")
            .arg("-C")
            .arg(&project_root)
            .args(["branch", "-D", branch])
            .output()
            .context("git branch -D")?;

        if !output.status.success() {
            // Non-fatal: worktree was removed, branch deletion is best-effort
        }
    }

    Ok(RemovedWorktree {
        worktree_path,
        branch,
        project_root,
    })
}

/// Extracts the branch name for a worktree path from porcelain output.
fn extract_worktree_branch(project_root: &Path, worktree_path: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .context("git worktree list --porcelain")?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let target = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());

    let mut current_path: Option<PathBuf> = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            let p = PathBuf::from(rest.trim());
            current_path = Some(p.canonicalize().unwrap_or(p));
        } else if let Some(rest) = line.strip_prefix("branch refs/heads/") {
            if current_path.as_ref() == Some(&target) {
                return Ok(Some(rest.trim().to_string()));
            }
        } else if line.is_empty() {
            current_path = None;
        }
    }

    Ok(None)
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
