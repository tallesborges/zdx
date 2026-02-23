use std::path::{Path, PathBuf};

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::events::{SkillListing, SkillUiEvent, UiEvent};

#[derive(Debug, Clone)]
struct RepoSpec {
    owner: String,
    repo: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct GitHubContentItem {
    name: String,
    #[serde(rename = "type")]
    item_type: String,
    download_url: Option<String>,
}

pub async fn fetch_skills_list(repo: String, cancel: Option<CancellationToken>) -> UiEvent {
    let cancel = cancel.unwrap_or_default();
    match fetch_skills_list_inner(&repo, &cancel).await {
        Ok(skills) => UiEvent::Skill(SkillUiEvent::ListLoaded { repo, skills }),
        Err(error) => UiEvent::Skill(SkillUiEvent::ListFailed { repo, error }),
    }
}

pub async fn install_skill(
    repo: String,
    skill_path: String,
    cancel: Option<CancellationToken>,
) -> UiEvent {
    let cancel = cancel.unwrap_or_default();
    let skill_name = skill_name_from_path(&skill_path);
    match install_skill_inner(&repo, &skill_path, &cancel).await {
        Ok(()) => UiEvent::Skill(SkillUiEvent::Installed {
            repo,
            skill: skill_name,
        }),
        Err(error) => UiEvent::Skill(SkillUiEvent::InstallFailed {
            repo,
            skill: skill_name,
            error,
        }),
    }
}

pub async fn fetch_skill_instructions(
    repo: String,
    skill_path: String,
    cancel: Option<CancellationToken>,
) -> UiEvent {
    let cancel = cancel.unwrap_or_default();
    match fetch_skill_instructions_inner(&repo, &skill_path, &cancel).await {
        Ok(content) => UiEvent::Skill(SkillUiEvent::InstructionsLoaded {
            repo,
            skill_path,
            content,
        }),
        Err(error) => UiEvent::Skill(SkillUiEvent::InstructionsFailed {
            repo,
            skill_path,
            error,
        }),
    }
}

async fn fetch_skills_list_inner(
    repo: &str,
    cancel: &CancellationToken,
) -> Result<Vec<SkillListing>, String> {
    let spec = parse_repo_spec(repo)?;
    let client = github_client()?;
    let base_path = spec.path.clone();
    let url = contents_url(&spec, &base_path)?;
    let entries = list_directory(&client, url, cancel).await?;

    // Assume all directories are skills (avoids N+1 API requests)
    let mut skills: Vec<SkillListing> = entries
        .into_iter()
        .filter(|entry| entry.item_type == "dir")
        .map(|entry| SkillListing {
            name: entry.name.clone(),
            path: entry.name,
            description: None,
        })
        .collect();

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

async fn install_skill_inner(
    repo: &str,
    skill_path: &str,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let spec = parse_repo_spec(repo)?;
    let skill_dir_name = skill_name_from_path(skill_path);
    let install_root = skill_install_root();
    let dest_root = install_root.join(&skill_dir_name);

    if dest_root.exists() {
        return Err("Skill already exists.".to_string());
    }

    let skill_repo_path = join_repo_path(&spec.path, skill_path);
    let result = sparse_clone_skill(
        &spec.owner,
        &spec.repo,
        &skill_repo_path,
        &dest_root,
        cancel,
    )
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&dest_root).await;
    }

    result
}

async fn sparse_clone_skill(
    owner: &str,
    repo: &str,
    skill_repo_path: &str,
    dest_root: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let repo_url = format!("https://github.com/{owner}/{repo}.git");

    // Use a sibling temp dir so cleanup is easy
    let tmp_dir = dest_root.with_extension("__tmp");
    if tmp_dir.exists() {
        tokio::fs::remove_dir_all(&tmp_dir)
            .await
            .map_err(|e| format!("Failed to clean temp dir: {e}"))?;
    }

    // 1. Init repo with no checkout and blobless filter
    run_git(
        &[
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            "--depth=1",
            "--sparse",
            &repo_url,
            tmp_dir.to_str().unwrap_or("."),
        ],
        None,
        cancel,
    )
    .await?;

    // 2. Sparse-checkout the specific skill folder
    run_git(
        &["sparse-checkout", "set", "--no-cone", skill_repo_path],
        Some(&tmp_dir),
        cancel,
    )
    .await?;

    // 3. Checkout
    run_git(&["checkout"], Some(&tmp_dir), cancel).await?;

    // 4. Move the skill subfolder to the final destination
    let skill_src = tmp_dir.join(skill_repo_path);
    if !skill_src.exists() {
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        return Err(format!("Skill path '{skill_repo_path}' not found in repo."));
    }

    tokio::fs::rename(&skill_src, dest_root)
        .await
        .map_err(|e| format!("Failed to move skill to destination: {e}"))?;

    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    Ok(())
}

async fn run_git(
    args: &[&str],
    cwd: Option<&std::path::Path>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    use tokio::process::Command;

    if cancel.is_cancelled() {
        return Err("Installation cancelled.".to_string());
    }

    let mut git_cmd = Command::new("git");
    git_cmd
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    if let Some(dir) = cwd {
        git_cmd.current_dir(dir);
    }

    let output = git_cmd
        .output()
        .await
        .map_err(|e| format!("Failed to run git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git error: {stderr}"));
    }

    Ok(())
}

async fn fetch_skill_instructions_inner(
    repo: &str,
    skill_path: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    if cancel.is_cancelled() {
        return Err("Operation cancelled.".to_string());
    }

    let spec = parse_repo_spec(repo)?;
    let client = github_client()?;
    let skill_repo_path = join_repo_path(&spec.path, skill_path);
    let file_path = format!("{skill_repo_path}/SKILL.md");
    let url = contents_url(&spec, &file_path)?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| format!("Request failed: {err}"))?;

    if !response.status().is_success() {
        return Err("No SKILL.md found.".to_string());
    }

    let item: GitHubContentItem = response
        .json()
        .await
        .map_err(|err| format!("Failed to parse response: {err}"))?;

    let download_url = item
        .download_url
        .ok_or_else(|| "Missing download URL.".to_string())?;

    let content_response = client
        .get(&download_url)
        .send()
        .await
        .map_err(|err| format!("Failed to download: {err}"))?;

    content_response
        .text()
        .await
        .map_err(|err| format!("Failed to read content: {err}"))
}

async fn list_directory(
    client: &reqwest::Client,
    url: Url,
    cancel: &CancellationToken,
) -> Result<Vec<GitHubContentItem>, String> {
    if cancel.is_cancelled() {
        return Err("Operation cancelled.".to_string());
    }

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| format!("Request failed: {err}"))?;

    let status = response.status();
    if !status.is_success() {
        let is_rate_limited = status == StatusCode::FORBIDDEN
            && response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|remaining| remaining == "0");
        if is_rate_limited {
            return Err(
                "GitHub API rate limit exceeded. Set GITHUB_TOKEN to increase limits.".to_string(),
            );
        }

        let body = response.text().await.unwrap_or_else(|_| String::new());
        return Err(format!("GitHub API error ({status}): {body}"));
    }

    response
        .json::<Vec<GitHubContentItem>>()
        .await
        .map_err(|err| format!("Failed to parse GitHub response: {err}"))
}

fn parse_repo_spec(spec: &str) -> Result<RepoSpec, String> {
    let mut parts = spec.split('/');
    let owner = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Missing repository owner.".to_string())?;
    let repo = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Missing repository name.".to_string())?;
    let path = parts.collect::<Vec<_>>().join("/");

    Ok(RepoSpec {
        owner: owner.to_string(),
        repo: repo.to_string(),
        path,
    })
}

fn github_client() -> Result<reqwest::Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static("zdx"));

    if let Some(token) = github_token() {
        let value = format!("Bearer {token}");
        let header =
            HeaderValue::from_str(&value).map_err(|_err| "Invalid GitHub token.".to_string())?;
        headers.insert(AUTHORIZATION, header);
    }

    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .map_err(|err| format!("Failed to build HTTP client: {err}"))
}

fn github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty())
        .or_else(|| {
            std::env::var("GH_TOKEN")
                .ok()
                .filter(|token| !token.trim().is_empty())
        })
}

fn contents_url(spec: &RepoSpec, path: &str) -> Result<Url, String> {
    let mut url = Url::parse("https://api.github.com")
        .map_err(|err| format!("Invalid GitHub API URL: {err}"))?;
    let path = if path.is_empty() {
        format!("/repos/{}/{}/contents", spec.owner, spec.repo)
    } else {
        format!("/repos/{}/{}/contents/{}", spec.owner, spec.repo, path)
    };
    url.set_path(&path);
    Ok(url)
}

fn join_repo_path(base: &str, child: &str) -> String {
    if base.is_empty() {
        child.trim_start_matches('/').to_string()
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            child.trim_start_matches('/')
        )
    }
}

fn skill_install_root() -> PathBuf {
    // Install to user's ZDX home skills directory (~/.zdx/skills/)
    zdx_core::config::paths::zdx_home().join("skills")
}

fn skill_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}
