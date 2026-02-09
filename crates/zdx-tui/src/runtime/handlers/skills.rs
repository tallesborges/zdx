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
    path: String,
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
    let client = github_client()?;
    let skill_dir_name = skill_name_from_path(skill_path);
    let install_root = skill_install_root();
    let dest_root = install_root.join(&skill_dir_name);

    if dest_root.exists() {
        return Err("Skill already exists.".to_string());
    }

    tokio::fs::create_dir_all(&dest_root)
        .await
        .map_err(|err| format!("Failed to create skill directory: {err}"))?;

    let skill_repo_path = join_repo_path(&spec.path, skill_path);
    let result = download_repo_dir(&client, &spec, &skill_repo_path, &dest_root, cancel).await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&dest_root).await;
    }

    result
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

async fn download_repo_dir(
    client: &reqwest::Client,
    spec: &RepoSpec,
    repo_path: &str,
    dest_root: &Path,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let mut stack = vec![(repo_path.to_string(), dest_root.to_path_buf())];

    while let Some((repo_path, dest_root)) = stack.pop() {
        if cancel.is_cancelled() {
            return Err("Installation cancelled.".to_string());
        }

        let url = contents_url(spec, &repo_path)?;
        let entries = list_directory(client, url, cancel).await?;

        for entry in entries {
            if cancel.is_cancelled() {
                return Err("Installation cancelled.".to_string());
            }

            match entry.item_type.as_str() {
                "file" => {
                    let download_url = entry
                        .download_url
                        .ok_or_else(|| "Missing download URL.".to_string())?;
                    download_file(client, &download_url, &dest_root, &repo_path, &entry.path)
                        .await?;
                }
                "dir" => {
                    let sub_dest = dest_root.join(&entry.name);
                    tokio::fs::create_dir_all(&sub_dest)
                        .await
                        .map_err(|err| format!("Failed to create directory: {err}"))?;
                    stack.push((entry.path, sub_dest));
                }
                _ => {}
            }
        }
    }

    Ok(())
}

async fn download_file(
    client: &reqwest::Client,
    download_url: &str,
    dest_root: &Path,
    repo_root: &str,
    repo_path: &str,
) -> Result<(), String> {
    let response = client
        .get(download_url)
        .send()
        .await
        .map_err(|err| format!("Failed to download file: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("Failed to download file ({}).", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("Failed to read file bytes: {err}"))?;

    let relative = repo_path
        .strip_prefix(repo_root)
        .unwrap_or(repo_path)
        .trim_start_matches('/');
    let dest_path = dest_root.join(relative);

    if let Some(parent) = dest_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("Failed to create directory: {err}"))?;
    }

    tokio::fs::write(&dest_path, bytes)
        .await
        .map_err(|err| format!("Failed to write file: {err}"))?;

    Ok(())
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
            HeaderValue::from_str(&value).map_err(|_| "Invalid GitHub token.".to_string())?;
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
