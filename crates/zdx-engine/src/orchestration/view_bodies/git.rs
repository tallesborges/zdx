//! Git bodies: the repository of a thread (or the views root), its
//! uncommitted state, the files a history scope touches, and one file's diff.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use serde::Serialize;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

use super::thread_root::select_thread_root;
use super::{EngineViews, internal, invalid, refused};
use crate::orchestration::protocol::ErrorCode;
use crate::orchestration::views::ViewError;

const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const GIT_DIFF_LIMIT_BYTES: usize = 256 * 1024;
const GIT_COMMIT_LIMIT: usize = 24;

impl EngineViews {
    /// Uncommitted state, worktrees, and recent commits of the repository of
    /// `thread_id` (`active` when absent), else of the views root.
    ///
    /// # Errors
    /// `Invalid` for a bad id, `NotFound` without a repository, `Internal`
    /// when Git fails.
    pub async fn git_status(&self, thread_id: Option<String>) -> Result<GitResponse, ViewError> {
        git_status(&self.root, thread_id).await
    }

    /// The files a history scope touches: `all` (merge base with the main
    /// branch against the working tree, plus untracked files) or a commit.
    ///
    /// # Errors
    /// As [`Self::git_status`]; `Invalid` for a scope that is neither.
    pub async fn git_scope(
        &self,
        thread_id: Option<String>,
        scope: &str,
    ) -> Result<GitScopeResponse, ViewError> {
        git_scope(&self.root, thread_id, scope).await
    }

    /// One file's diff in a category (`staged`, `unstaged`, `untracked`,
    /// `all`, or `commit` with `commit`), bounded to `GIT_DIFF_LIMIT_BYTES`.
    ///
    /// # Errors
    /// As [`Self::git_status`]; `Invalid` for a bad path, category, or hash,
    /// `NotFound` when the file is not in that category or scope.
    pub async fn git_diff(
        &self,
        thread_id: Option<String>,
        path: String,
        kind: &str,
        commit: Option<&str>,
    ) -> Result<GitDiffResponse, ViewError> {
        git_diff(&self.root, thread_id, path, kind, commit).await
    }
}

#[derive(Serialize)]
pub struct GitScopeResponse {
    /// `all` or `commit`.
    scope: &'static str,
    /// Resolved base revision (`all`) or the commit itself.
    base: Option<String>,
    /// For `all`: the main branch the base was taken from (`master`,
    /// `origin/main`). `None` for commits, or when no main branch was found.
    base_ref: Option<String>,
    /// For `all`: commits on this branch since the base.
    ahead: usize,
    files: Vec<GitFile>,
    /// Subset of `files` that are untracked, so the client knows to request
    /// those diffs with `kind=untracked`.
    untracked: Vec<String>,
}

#[derive(Serialize)]
pub struct GitResponse {
    repository: GitRepository,
    worktrees: Vec<GitWorktree>,
    files: GitFiles,
    commits: Vec<GitCommit>,
}

#[derive(Serialize)]
struct GitRepository {
    name: String,
    root: String,
    source: &'static str,
    thread_id: Option<String>,
    branch: String,
    head: Option<String>,
    upstream: Option<String>,
    ahead: u64,
    behind: u64,
    detached: bool,
    clean: bool,
}

#[derive(Default, Serialize)]
struct GitFiles {
    staged: Vec<GitFile>,
    unstaged: Vec<GitFile>,
    untracked: Vec<GitFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct GitFile {
    path: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct GitWorktree {
    path: String,
    head: Option<String>,
    branch: Option<String>,
    current: bool,
    flags: Vec<&'static str>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct GitCommit {
    hash: String,
    short_hash: String,
    author: String,
    authored_at: String,
    relative_time: String,
    refs: String,
    subject: String,
}

#[derive(Serialize)]
pub struct GitDiffResponse {
    path: String,
    kind: &'static str,
    content: String,
    bytes: usize,
    lines: usize,
    truncated: bool,
    limit_bytes: usize,
}

struct ResolvedGitRepository {
    root: PathBuf,
    source: &'static str,
    thread_id: Option<String>,
}

#[derive(Default)]
struct ParsedGitStatus {
    oid: Option<String>,
    branch: Option<String>,
    upstream: Option<String>,
    ahead: u64,
    behind: u64,
    files: GitFiles,
}

#[derive(Default)]
struct GitWorktreeBuilder {
    path: Option<String>,
    head: Option<String>,
    branch: Option<String>,
    flags: Vec<&'static str>,
}

#[derive(Clone)]
enum GitDiffKind {
    Staged,
    Unstaged,
    Untracked,
    /// One commit, addressed by a validated hash (`git show <hash>`).
    Commit(String),
    /// Everything since a base revision *including* the working tree, so a
    /// single view covers committed-but-unpushed work plus local edits.
    Range(String),
}

impl GitDiffKind {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Untracked => "untracked",
            Self::Commit(_) => "commit",
            Self::Range(_) => "all",
        }
    }
}

/// Accepts only an abbreviated-or-full hex object id.
///
/// Everything here reaches a `git` argument list, so a revision that could be
/// read as an option or a range never gets built in the first place.
fn valid_commit_hash(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The revision "all changes" diffs against, plus what it was derived from.
struct AllChangesBase {
    /// Object id (or `HEAD`) handed to `git diff`.
    rev: String,
    /// Branch the base was taken from (`master`, `origin/main`, ...). `None`
    /// when no main branch could be found and the scope fell back to `HEAD`.
    reference: Option<String>,
    /// Commits on this branch since the base, for the selector subtitle.
    ahead: usize,
}

/// Resolves the base for the "all changes" scope: the merge base with the
/// repository's main branch, so the view shows what *this branch* adds, the way
/// a pull request would. On the main branch itself the base is `HEAD` and the
/// scope equals "uncommitted", which is the correct answer there.
///
/// The main branch is read from `origin/HEAD` and falls back to `main` then
/// `master`. A local branch is preferred over its remote so that unpushed
/// commits on main do not appear as "changes"; the remote is used only when no
/// local branch of that name exists.
async fn resolve_all_base(root: &Path) -> AllChangesBase {
    let fallback = AllChangesBase {
        rev: "HEAD".to_string(),
        reference: None,
        ahead: 0,
    };

    let Some(reference) = main_branch_ref(root).await else {
        return fallback;
    };
    let merge_base = git_output(root, &["merge-base", &reference, "HEAD"])
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|base| base.trim().to_string())
        .filter(|base| valid_commit_hash(base));
    let Some(rev) = merge_base else {
        return fallback;
    };

    let ahead = git_output(root, &["rev-list", "--count", &format!("{rev}..HEAD")])
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0);

    AllChangesBase {
        rev,
        reference: Some(reference),
        ahead,
    }
}

/// Finds the repository's main branch as a revision `git merge-base` accepts.
async fn main_branch_ref(root: &Path) -> Option<String> {
    // `origin/HEAD` names the default branch on a normal clone.
    let from_origin = git_output(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await
    .ok()
    .and_then(|bytes| String::from_utf8(bytes).ok())
    .map(|name| name.trim().to_string())
    .and_then(|name| name.strip_prefix("origin/").map(str::to_string));

    let mut candidates: Vec<String> = Vec::new();
    if let Some(name) = from_origin {
        candidates.push(name);
    }
    candidates.extend(["main".to_string(), "master".to_string()]);

    for name in &candidates {
        if git_ref_exists(root, &format!("refs/heads/{name}")).await {
            return Some(name.clone());
        }
    }
    for name in &candidates {
        if git_ref_exists(root, &format!("refs/remotes/origin/{name}")).await {
            return Some(format!("origin/{name}"));
        }
    }
    None
}

async fn git_ref_exists(root: &Path, full_ref: &str) -> bool {
    git_output(root, &["rev-parse", "--verify", "--quiet", full_ref])
        .await
        .is_ok()
}

async fn git_status(
    views_root: &Path,
    thread_id: Option<String>,
) -> Result<GitResponse, ViewError> {
    let repository = resolve_git_repository(views_root, thread_id).await?;
    let status = load_git_status(&repository.root).await.map_err(|error| {
        tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to read Mini App Git state");
        internal("Git state unavailable")
    })?;
    let commits = async {
        if status.oid.is_some() {
            load_git_commits(&repository.root).await
        } else {
            Ok(Vec::new())
        }
    };
    let (mut worktrees, commits) =
        tokio::try_join!(load_git_worktrees(&repository.root), commits).map_err(|error| {
            tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to read Mini App Git state");
            internal("Git state unavailable")
        })?;

    for worktree in &mut worktrees {
        worktree.current = Path::new(&worktree.path) == repository.root;
    }

    let detached = status.branch.as_deref() == Some("(detached)");
    let branch = if detached {
        "detached".to_string()
    } else {
        status.branch.clone().unwrap_or_else(|| "HEAD".to_string())
    };
    let clean = status.files.staged.is_empty()
        && status.files.unstaged.is_empty()
        && status.files.untracked.is_empty();
    let name = repository
        .root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repository")
        .to_string();
    let head = status.oid.as_deref().map(short_hash);

    Ok(GitResponse {
        repository: GitRepository {
            name,
            root: repository.root.to_string_lossy().into_owned(),
            source: repository.source,
            thread_id: repository.thread_id,
            branch,
            head,
            upstream: status.upstream,
            ahead: status.ahead,
            behind: status.behind,
            detached,
            clean,
        },
        worktrees,
        files: status.files,
        commits,
    })
}

/// Lists the files a non-uncommitted scope touches.
///
/// `scope=all` diffs the base against the working tree, so it already covers
/// committed, staged and unstaged changes in one pass; untracked files are not
/// part of a diff and are appended from `status`. A commit scope lists exactly
/// what that commit changed.
async fn load_scope_files(root: &Path, kind: &GitDiffKind) -> anyhow::Result<Vec<GitFile>> {
    let output = match kind {
        GitDiffKind::Range(base) => {
            git_output(root, &["diff", "--name-status", "--no-renames", base]).await?
        }
        GitDiffKind::Commit(hash) => {
            git_output(
                root,
                &["show", "--name-status", "--no-renames", "--format=", hash],
            )
            .await?
        }
        _ => return Ok(Vec::new()),
    };
    let output = String::from_utf8(output).context("Git file list contains non-UTF-8 text")?;

    Ok(output
        .lines()
        .filter_map(|line| {
            let (status, path) = line.split_once('\t')?;
            let path = path.trim();
            (!path.is_empty() && valid_git_path(path)).then(|| GitFile {
                path: path.to_string(),
                status: status.trim().to_string(),
                original_path: None,
            })
        })
        .collect())
}

async fn git_scope(
    views_root: &Path,
    thread_id: Option<String>,
    scope: &str,
) -> Result<GitScopeResponse, ViewError> {
    let repository = resolve_git_repository(views_root, thread_id).await?;
    let scope = scope.trim();

    let (kind, base_ref, ahead) = if scope == "all" {
        let base = resolve_all_base(&repository.root).await;
        (GitDiffKind::Range(base.rev), base.reference, base.ahead)
    } else if valid_commit_hash(scope) {
        (GitDiffKind::Commit(scope.to_string()), None, 0)
    } else {
        return Err(invalid("Invalid Git scope"));
    };

    let mut files = load_scope_files(&repository.root, &kind).await.map_err(|error| {
        tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to list Mini App Git scope");
        internal("Git scope unavailable")
    })?;

    // Untracked files never appear in a diff, so "all changes" would silently
    // omit brand-new files without this.
    let mut untracked = Vec::new();
    if matches!(kind, GitDiffKind::Range(_)) {
        let status = load_git_status(&repository.root).await.map_err(|error| {
            tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to read Mini App Git state");
            internal("Git state unavailable")
        })?;
        untracked = status.files.untracked;
    }
    files.append(&mut untracked.clone());

    Ok(GitScopeResponse {
        scope: kind.as_str(),
        base: match &kind {
            GitDiffKind::Range(base) => Some(base.clone()),
            GitDiffKind::Commit(hash) => Some(hash.clone()),
            _ => None,
        },
        base_ref,
        ahead,
        untracked: untracked.into_iter().map(|file| file.path).collect(),
        files,
    })
}

async fn git_diff(
    views_root: &Path,
    thread_id: Option<String>,
    path: String,
    kind: &str,
    commit: Option<&str>,
) -> Result<GitDiffResponse, ViewError> {
    if !valid_git_path(&path) {
        return Err(invalid("Invalid Git file path"));
    }

    let repository = resolve_git_repository(views_root, thread_id).await?;

    // `all` and `commit` address history, so their base has to be resolved
    // against the repository before the file can be validated.
    let kind = match kind {
        "staged" => GitDiffKind::Staged,
        "unstaged" => GitDiffKind::Unstaged,
        "untracked" => GitDiffKind::Untracked,
        "all" => GitDiffKind::Range(resolve_all_base(&repository.root).await.rev),
        "commit" => {
            let hash = commit
                .map(str::trim)
                .filter(|hash| valid_commit_hash(hash))
                .ok_or_else(|| invalid("Invalid commit hash"))?;
            GitDiffKind::Commit(hash.to_string())
        }
        _ => return Err(invalid("Invalid Git file category")),
    };

    match &kind {
        GitDiffKind::Staged | GitDiffKind::Unstaged | GitDiffKind::Untracked => {
            let status = load_git_status(&repository.root).await.map_err(|error| {
                tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to validate Mini App Git diff");
                internal("Git state unavailable")
            })?;
            let files = match kind {
                GitDiffKind::Staged => &status.files.staged,
                GitDiffKind::Unstaged => &status.files.unstaged,
                _ => &status.files.untracked,
            };
            if !files.iter().any(|file| file.path == path) {
                return Err(refused(
                    ErrorCode::NotFound,
                    "Git file is no longer in that category",
                ));
            }
        }
        GitDiffKind::Commit(_) | GitDiffKind::Range(_) => {
            let files = load_scope_files(&repository.root, &kind).await.map_err(|error| {
                tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to validate Mini App Git diff");
                internal("Git scope unavailable")
            })?;
            if !files.iter().any(|file| file.path == path) {
                return Err(refused(
                    ErrorCode::NotFound,
                    "Git file is not in that scope",
                ));
            }
        }
    }

    let (bytes, truncated) = load_git_diff(&repository.root, &path, &kind)
        .await
        .map_err(|error| {
            tracing::warn!(root = %repository.root.display(), path, error = %format!("{error:#}"), "Failed to read Mini App Git diff");
            internal("Git diff unavailable")
        })?;
    let byte_count = bytes.len().min(GIT_DIFF_LIMIT_BYTES);
    let content = String::from_utf8_lossy(&bytes[..byte_count]).into_owned();
    let lines = content.lines().count();

    Ok(GitDiffResponse {
        path,
        kind: kind.as_str(),
        content,
        bytes: byte_count,
        lines,
        truncated,
        limit_bytes: GIT_DIFF_LIMIT_BYTES,
    })
}

async fn resolve_git_repository(
    views_root: &Path,
    thread_id: Option<String>,
) -> Result<ResolvedGitRepository, ViewError> {
    let bot_root = views_root.to_path_buf();
    let selection =
        select_thread_root(views_root, thread_id, "Git repository resolution failed").await?;

    let mut candidates = Vec::with_capacity(2);
    if let Some(root) = selection.root_path {
        candidates.push(("thread", PathBuf::from(root)));
    }
    if candidates
        .first()
        .is_none_or(|(_, candidate)| candidate != &bot_root)
    {
        candidates.push(("bot_root", bot_root));
    }

    for (source, candidate) in candidates {
        if let Ok(output) = git_output(&candidate, &["rev-parse", "--show-toplevel"]).await {
            let Ok(output) = String::from_utf8(output) else {
                continue;
            };
            let root = output.trim_end_matches(['\n', '\r']).to_string();
            if !root.is_empty() {
                return Ok(ResolvedGitRepository {
                    root: PathBuf::from(root),
                    source,
                    thread_id: selection.thread_id,
                });
            }
        }
    }

    Err(refused(ErrorCode::NotFound, "Git repository not found"))
}

async fn git_output(root: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .kill_on_drop(true);
    let output = tokio::time::timeout(GIT_COMMAND_TIMEOUT, command.output())
        .await
        .context("Git command timed out")?
        .context("Failed to run Git")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git exited with {}: {}", output.status, stderr.trim());
    }
    Ok(output.stdout)
}

async fn load_git_status(root: &Path) -> anyhow::Result<ParsedGitStatus> {
    let output = git_output(
        root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
    )
    .await?;
    let output = String::from_utf8(output).context("Git status contains non-UTF-8 paths")?;
    Ok(parse_git_status(&output))
}

fn parse_git_status(output: &str) -> ParsedGitStatus {
    let mut records = output.split('\0');
    let mut status = ParsedGitStatus::default();

    while let Some(record) = records.next() {
        if let Some(oid) = record.strip_prefix("# branch.oid ") {
            if oid != "(initial)" {
                status.oid = Some(oid.to_string());
            }
        } else if let Some(branch) = record.strip_prefix("# branch.head ") {
            status.branch = Some(branch.to_string());
        } else if let Some(upstream) = record.strip_prefix("# branch.upstream ") {
            status.upstream = Some(upstream.to_string());
        } else if let Some(counts) = record.strip_prefix("# branch.ab ") {
            for count in counts.split_whitespace() {
                if let Some(ahead) = count.strip_prefix('+') {
                    status.ahead = ahead.parse().unwrap_or_default();
                } else if let Some(behind) = count.strip_prefix('-') {
                    status.behind = behind.parse().unwrap_or_default();
                }
            }
        } else if let Some(path) = record.strip_prefix("? ") {
            status.files.untracked.push(GitFile {
                path: path.to_string(),
                status: "?".to_string(),
                original_path: None,
            });
        } else if record.starts_with("1 ") {
            if let Some((xy, path)) = parse_status_path(record, 9) {
                push_changed_file(&mut status.files, xy, path, None);
            }
        } else if record.starts_with("2 ") {
            if let Some((xy, path)) = parse_status_path(record, 10) {
                let original_path = records.next().map(str::to_string);
                push_changed_file(&mut status.files, xy, path, original_path);
            }
        } else if record.starts_with("u ")
            && let Some((xy, path)) = parse_status_path(record, 11)
        {
            push_changed_file(&mut status.files, xy, path, None);
        }
    }

    status
}

fn parse_status_path(record: &str, field_count: usize) -> Option<(&str, &str)> {
    let fields: Vec<&str> = record.splitn(field_count, ' ').collect();
    (fields.len() == field_count).then(|| (fields[1], fields[field_count - 1]))
}

fn push_changed_file(files: &mut GitFiles, xy: &str, path: &str, original_path: Option<String>) {
    let mut status = xy.chars();
    if let Some(index) = status.next()
        && index != '.'
    {
        files.staged.push(GitFile {
            path: path.to_string(),
            status: index.to_string(),
            original_path: original_path.clone(),
        });
    }
    if let Some(worktree) = status.next()
        && worktree != '.'
    {
        files.unstaged.push(GitFile {
            path: path.to_string(),
            status: worktree.to_string(),
            original_path,
        });
    }
}

async fn load_git_worktrees(root: &Path) -> anyhow::Result<Vec<GitWorktree>> {
    let output = git_output(root, &["worktree", "list", "--porcelain", "-z"]).await?;
    let output = String::from_utf8(output).context("Git worktrees contain non-UTF-8 paths")?;
    Ok(parse_git_worktrees(&output))
}

fn parse_git_worktrees(output: &str) -> Vec<GitWorktree> {
    let mut worktrees = Vec::new();
    let mut current = GitWorktreeBuilder::default();

    for field in output.split('\0') {
        if field.is_empty() {
            finish_worktree(&mut worktrees, &mut current);
        } else if let Some(path) = field.strip_prefix("worktree ") {
            finish_worktree(&mut worktrees, &mut current);
            current.path = Some(path.to_string());
        } else if let Some(head) = field.strip_prefix("HEAD ") {
            current.head = Some(short_hash(head));
        } else if let Some(branch) = field.strip_prefix("branch ") {
            current.branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
            );
        } else if field == "detached" {
            current.flags.push("detached");
        } else if field == "bare" {
            current.flags.push("bare");
        } else if field == "locked" || field.starts_with("locked ") {
            current.flags.push("locked");
        } else if field == "prunable" || field.starts_with("prunable ") {
            current.flags.push("prunable");
        }
    }
    finish_worktree(&mut worktrees, &mut current);
    worktrees
}

fn finish_worktree(worktrees: &mut Vec<GitWorktree>, builder: &mut GitWorktreeBuilder) {
    if let Some(path) = builder.path.take() {
        worktrees.push(GitWorktree {
            path,
            head: builder.head.take(),
            branch: builder.branch.take(),
            current: false,
            flags: std::mem::take(&mut builder.flags),
        });
    }
}

async fn load_git_commits(root: &Path) -> anyhow::Result<Vec<GitCommit>> {
    let limit = format!("-n{GIT_COMMIT_LIMIT}");
    let output = git_output(
        root,
        &[
            "log",
            &limit,
            "--date=iso-strict",
            "--format=%H%x1f%h%x1f%an%x1f%aI%x1f%ar%x1f%D%x1f%s%x1e",
        ],
    )
    .await?;
    let output = String::from_utf8(output).context("Git log contains non-UTF-8 text")?;
    Ok(parse_git_commits(&output))
}

fn parse_git_commits(output: &str) -> Vec<GitCommit> {
    output
        .split('\x1e')
        .filter_map(|record| {
            let record = record.trim_matches(['\n', '\r']);
            if record.is_empty() {
                return None;
            }
            let fields: Vec<&str> = record.splitn(7, '\x1f').collect();
            (fields.len() == 7).then(|| GitCommit {
                hash: fields[0].to_string(),
                short_hash: fields[1].to_string(),
                author: fields[2].to_string(),
                authored_at: fields[3].to_string(),
                relative_time: fields[4].to_string(),
                refs: fields[5].to_string(),
                subject: fields[6].to_string(),
            })
        })
        .collect()
}

/// Builds the `git` argument list that renders one file's diff for a scope.
fn git_diff_args<'a>(kind: &'a GitDiffKind, path: &'a str) -> Vec<&'a str> {
    const FLAGS: [&str; 3] = ["--no-color", "--no-ext-diff", "--no-textconv"];
    let mut args = Vec::with_capacity(9);
    match kind {
        GitDiffKind::Staged => {
            args.extend(["diff", "--cached"]);
            args.extend(FLAGS);
            args.extend(["--", path]);
        }
        GitDiffKind::Unstaged => {
            args.push("diff");
            args.extend(FLAGS);
            args.extend(["--", path]);
        }
        GitDiffKind::Untracked => {
            args.extend(["diff", "--no-index"]);
            args.extend(FLAGS);
            args.extend(["--", "/dev/null", path]);
        }
        GitDiffKind::Commit(hash) => {
            args.push("show");
            args.extend(FLAGS);
            args.extend(["--format=", hash.as_str(), "--", path]);
        }
        GitDiffKind::Range(base) => {
            // Base against the working tree, so one diff covers the committed,
            // staged and unstaged state of this file.
            args.push("diff");
            args.extend(FLAGS);
            args.extend([base.as_str(), "--", path]);
        }
    }
    args
}

async fn load_git_diff(
    root: &Path,
    path: &str,
    kind: &GitDiffKind,
) -> anyhow::Result<(Vec<u8>, bool)> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .args(git_diff_args(kind, path));

    let mut child = command.spawn().context("Failed to start Git diff")?;
    let mut stdout = child.stdout.take().context("Git diff stdout unavailable")?;
    let mut bytes = Vec::with_capacity(GIT_DIFF_LIMIT_BYTES + 1);
    let result = tokio::time::timeout(GIT_COMMAND_TIMEOUT, async {
        (&mut stdout)
            .take((GIT_DIFF_LIMIT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .context("Failed to read Git diff")?;
        if bytes.len() > GIT_DIFF_LIMIT_BYTES {
            bytes.truncate(GIT_DIFF_LIMIT_BYTES);
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Ok::<_, anyhow::Error>((true, None));
        }
        let status = child.wait().await.context("Failed to wait for Git diff")?;
        Ok((false, Some(status)))
    })
    .await;

    let (truncated, exit_status) = match result {
        Ok(result) => result?,
        Err(_elapsed) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            anyhow::bail!("Git diff timed out");
        }
    };
    if let Some(exit_status) = exit_status
        && !exit_status.success()
        && !(matches!(kind, GitDiffKind::Untracked) && exit_status.code() == Some(1))
    {
        anyhow::bail!("Git diff exited with {exit_status}");
    }
    Ok((bytes, truncated))
}

fn valid_git_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn short_hash(hash: &str) -> String {
    hash.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_hex_object_ids_as_revisions() {
        assert!(valid_commit_hash("723a11e"));
        assert!(valid_commit_hash(
            "8e66e93a1b2c3d4e5f60718293a4b5c6d7e8f900"
        ));

        // Too short to be an abbreviated hash, or too long to be a hash at all.
        assert!(!valid_commit_hash("723a11"));
        assert!(!valid_commit_hash(&"a".repeat(41)));
        assert!(!valid_commit_hash(""));
        // Anything that git could read as an option, a range or a second
        // argument must never reach the command line.
        assert!(!valid_commit_hash("--output=/tmp/x"));
        assert!(!valid_commit_hash("HEAD~1"));
        assert!(!valid_commit_hash("main..HEAD"));
        assert!(!valid_commit_hash("723a11e; rm -rf /"));
    }

    #[test]
    fn parses_branch_state_and_changed_file_categories() {
        let output = concat!(
            "# branch.oid 0123456789abcdef\0",
            "# branch.head main\0",
            "# branch.upstream origin/main\0",
            "# branch.ab +2 -1\0",
            "1 .M N... 100644 100644 100644 aaaaaaa bbbbbbb src/lib.rs\0",
            "1 M. N... 100644 100644 100644 aaaaaaa bbbbbbb Cargo.toml\0",
            "2 R. N... 100644 100644 100644 aaaaaaa bbbbbbb R100 src/new name.rs\0",
            "src/old name.rs\0",
            "? notes/todo.txt\0",
        );

        let status = parse_git_status(output);

        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert_eq!(status.files.unstaged[0].path, "src/lib.rs");
        assert_eq!(status.files.staged[0].path, "Cargo.toml");
        assert_eq!(status.files.staged[1].status, "R");
        assert_eq!(
            status.files.staged[1].original_path.as_deref(),
            Some("src/old name.rs")
        );
        assert_eq!(status.files.untracked[0].path, "notes/todo.txt");
    }

    #[test]
    fn parses_worktree_flags_and_commit_rows() {
        let worktrees = parse_git_worktrees(concat!(
            "worktree /repo\0",
            "HEAD 0123456789abcdef\0",
            "branch refs/heads/main\0\0",
            "worktree /repo-review\0",
            "HEAD fedcba9876543210\0",
            "detached\0",
            "locked review\0\0",
        ));
        let commits = parse_git_commits(
            "0123456789abcdef\x1f01234567\x1fAlice\x1f2026-08-24T10:00:00Z\x1f2 hours ago\x1fHEAD -> main\x1fBuild Git view\x1e",
        );

        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(worktrees[0].head.as_deref(), Some("01234567"));
        assert!(worktrees[1].flags.contains(&"detached"));
        assert!(worktrees[1].flags.contains(&"locked"));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].short_hash, "01234567");
        assert_eq!(commits[0].subject, "Build Git view");
    }

    #[test]
    fn accepts_only_repository_relative_git_paths() {
        assert!(valid_git_path("src/main.rs"));
        assert!(valid_git_path("notes/file with spaces.md"));
        assert!(!valid_git_path(""));
        assert!(!valid_git_path("../outside"));
        assert!(!valid_git_path("/absolute/path"));
    }

    #[tokio::test]
    async fn caps_lazy_untracked_diffs() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "zdx-engine-git-diff-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create temporary Git repository");
        let init = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("initialize temporary Git repository");
        assert!(init.success());
        std::fs::write(root.join("large.txt"), vec![b'x'; GIT_DIFF_LIMIT_BYTES * 2])
            .expect("write oversized untracked file");

        let (diff, truncated) = load_git_diff(&root, "large.txt", &GitDiffKind::Untracked)
            .await
            .expect("read bounded Git diff");

        assert!(truncated);
        assert_eq!(diff.len(), GIT_DIFF_LIMIT_BYTES);
        std::fs::remove_dir_all(root).expect("remove temporary Git repository");
    }

    #[tokio::test]
    async fn treats_diff_file_names_as_literal_paths() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "zdx-engine-git-literal-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create temporary Git repository");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("run Git fixture command")
        };
        assert!(git(&["init", "--quiet"]).success());
        std::fs::write(root.join("literal*.txt"), "initial\n").expect("write literal path");
        std::fs::write(root.join("literal-match.txt"), "initial\n").expect("write matching path");
        assert!(git(&["add", "."]).success());
        assert!(
            git(&[
                "-c",
                "user.name=ZDX Test",
                "-c",
                "user.email=zdx@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ])
            .success()
        );
        std::fs::write(root.join("literal*.txt"), "selected\n").expect("modify literal path");
        std::fs::write(root.join("literal-match.txt"), "other\n").expect("modify matching path");

        let (diff, truncated) = load_git_diff(&root, "literal*.txt", &GitDiffKind::Unstaged)
            .await
            .expect("read literal Git diff");
        let diff = String::from_utf8(diff).expect("UTF-8 fixture diff");

        assert!(!truncated);
        assert!(diff.contains("literal*.txt"));
        assert!(!diff.contains("literal-match.txt"));
        std::fs::remove_dir_all(root).expect("remove temporary Git repository");
    }
}
