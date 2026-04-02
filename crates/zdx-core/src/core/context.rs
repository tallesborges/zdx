//! Context module for loading project-specific guidelines.
//!
//! Project context files are loaded hierarchically:
//! 1. `ZDX_HOME/AGENTS.md` (or `CLAUDE.md` if `AGENTS.md` is missing)
//! 2. `~/AGENTS.md` (or `CLAUDE.md` if `AGENTS.md` is missing)
//! 3. Ancestor directories from home to project root
//! 4. Project root (`--root` or cwd)
//!
//! This module is UI-agnostic: it returns structured warnings instead of
//! printing directly. The caller (renderer) decides how to display them.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use minijinja::{Environment, UndefinedBehavior};
use serde::Serialize;

use crate::config::{Config, paths};
use crate::providers::{ProviderKind, resolve_provider};
use crate::skills::{LoadSkillsOptions, LoadSkillsResult, Skill, load_skills, skill_access_path};
use crate::{prompts, subagents};

/// Sets runtime `ZDX_*` environment variables for paths and session context.
///
/// These are visible to all child processes (bash tool, subagents) automatically.
/// Must be called once at startup, before any concurrent work begins.
///
/// # Safety
/// Uses `std::env::set_var` which is `unsafe` in Rust 2024 (process-global mutation).
/// Same pattern as `ZDX_DEBUG_TRACE` in `cli/mod.rs`. Acceptable because it's called
/// once at startup before concurrent work.
pub fn set_runtime_env(config: &Config, thread_id: Option<&str>) {
    let zdx_home = paths::zdx_home();
    let artifact_dir = paths::artifact_dir_for_thread(thread_id);
    let _ = crate::skills::ensure_bundled_skills_materialized();
    let memory_notes = config.memory.effective_notes_path();
    let memory_daily = config.memory.effective_daily_path();
    // SAFETY: Called once at startup before any concurrent work.
    // Same pattern as ZDX_DEBUG_TRACE in cli/mod.rs.
    unsafe {
        std::env::set_var("ZDX_HOME", zdx_home.as_os_str());
        std::env::set_var("ZDX_ARTIFACT_DIR", artifact_dir.as_os_str());
        std::env::set_var("ZDX_THREAD_ID", thread_id.unwrap_or(""));
        std::env::set_var("ZDX_MEMORY_NOTES_DIR", memory_notes.as_os_str());
        std::env::set_var("ZDX_MEMORY_DAILY_DIR", memory_daily.as_os_str());
    }
}

/// Maximum size for a single project context file (`AGENTS.md`/`CLAUDE.md`) (64KB).
/// Files larger than this are truncated with a warning.
pub const MAX_AGENTS_FILE_SIZE: usize = 64 * 1024;

const PRIMARY_CONTEXT_FILE_NAME: &str = "AGENTS.md";
const FALLBACK_CONTEXT_FILE_NAME: &str = "CLAUDE.md";

/// Maximum size for a single MEMORY.md file (16KB).
/// Files larger than this are truncated with a warning.
pub const MAX_MEMORY_FILE_SIZE: usize = 16 * 1024;

/// Preferred memory index filename.
pub const MEMORY_INDEX_FILE_NAME: &str = "MEMORY.md";
/// A warning generated during context loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextWarning {
    /// The path that caused the warning (if applicable).
    pub path: Option<PathBuf>,
    /// Human-readable warning message.
    pub message: String,
}

impl ContextWarning {
    /// Creates a warning for a file that couldn't be read.
    pub fn unreadable(path: &Path, error: &std::io::Error) -> Self {
        Self {
            path: Some(path.to_path_buf()),
            message: format!("Failed to read {}: {}", path.display(), error),
        }
    }

    /// Creates a warning for a truncated file.
    pub fn truncated(path: &Path, original_size: usize) -> Self {
        Self::truncated_with_limit(path, original_size, MAX_AGENTS_FILE_SIZE)
    }

    /// Creates a warning for a truncated file with a custom cap.
    pub fn truncated_with_limit(path: &Path, original_size: usize, truncated_size: usize) -> Self {
        Self {
            path: Some(path.to_path_buf()),
            message: format!(
                "Truncated {} ({} bytes) to {} bytes",
                path.display(),
                original_size,
                truncated_size
            ),
        }
    }
}

/// Result of loading inline project context files.
#[derive(Debug, Clone)]
pub struct LoadedContext {
    /// Combined content from all loaded project context files.
    pub content: String,
    /// Paths of files that were loaded (in order).
    pub loaded_paths: Vec<PathBuf>,
    /// Warnings generated during loading (e.g., unreadable files, truncation).
    pub warnings: Vec<ContextWarning>,
}

/// A scoped project context file discovered in a project subdirectory.
/// These are listed by path in the prompt (not inlined) — the agent reads on demand.
#[derive(Debug, Clone)]
pub struct ScopedContextFile {
    /// Absolute path to the context file.
    pub path: PathBuf,
    /// Relative scope directory from project root (e.g., "crates/zdx-core").
    pub scope: String,
}

/// Result of loading the memory index file.
#[derive(Debug, Clone)]
pub struct LoadedMemoryIndex {
    /// Content from the MEMORY.md file.
    pub content: String,
    /// Paths of files that were loaded (in order).
    pub loaded_paths: Vec<PathBuf>,
    /// Warnings generated during loading (e.g., unreadable files, truncation).
    pub warnings: Vec<ContextWarning>,
}

#[derive(Debug, Clone)]
struct TemplateSource {
    content: String,
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
struct PromptTemplateSkill {
    name: String,
    description: String,
    path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedSkillContent {
    pub name: String,
    pub description: String,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct StandalonePromptSkillContext {
    pub available_skills: Vec<Skill>,
    pub auto_loaded_skill_contents: Vec<LoadedSkillContent>,
}

#[derive(Debug, Clone, Serialize)]
struct PromptTemplateCapability {
    name: String,
    title: String,
    description: String,
    kind_label: String,
    backing: String,
}

#[derive(Debug, Clone, Serialize)]
struct PromptTemplateScopedContext {
    scope: String,
    path: String,
}

#[derive(Debug, Clone, Serialize)]
struct PromptTemplateVars {
    identity_prompt: String,
    provider: String,
    invocation_term: String,
    invocation_term_plural: String,
    is_openai_codex: bool,
    base_prompt: String,
    project_context: String,
    memory_index: String,
    instruction_layers: Vec<String>,
    memory_suggestions: bool,
    skills_list: Vec<PromptTemplateSkill>,
    available_skills: Vec<PromptTemplateSkill>,
    auto_loaded_skill_contents: Vec<LoadedSkillContent>,
    scoped_context: Vec<PromptTemplateScopedContext>,
    specialized_capabilities: Vec<PromptTemplateCapability>,
    cwd: String,
    date: String,
}

#[derive(Debug, Clone, Copy)]
struct PromptTemplateSections<'a> {
    base_prompt: Option<&'a str>,
    project_context: Option<&'a str>,
    memory_index: Option<&'a str>,
    memory_suggestions: bool,
    skills_list: &'a [Skill],
    scoped_context: &'a [ScopedContextFile],
    specialized_capabilities: &'a [PromptTemplateCapability],
}

fn combine_prompt_sections(
    base_prompt: Option<&str>,
    inline_project_context: Option<&str>,
    memory_index_block: Option<&str>,
    instruction_layers: &[String],
) -> Option<String> {
    let mut sections: Vec<&str> = Vec::new();
    for value in [base_prompt, inline_project_context, memory_index_block]
        .into_iter()
        .flatten()
    {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            sections.push(trimmed);
        }
    }

    for layer in instruction_layers {
        let trimmed = layer.trim();
        if !trimmed.is_empty() {
            sections.push(trimmed);
        }
    }

    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

fn load_prompt_template(config: &Config) -> std::result::Result<TemplateSource, ContextWarning> {
    if let Some(path_str) = config.prompt_template.file.as_deref() {
        let requested = Path::new(path_str);
        let path = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            paths::zdx_home().join(requested)
        };

        let content = fs::read_to_string(&path).map_err(|error| ContextWarning {
            path: Some(path.clone()),
            message: format!(
                "Failed to read system prompt template {}: {}; falling back to built-in template",
                path.display(),
                error
            ),
        })?;

        if content.trim().is_empty() {
            return Err(ContextWarning {
                path: Some(path.clone()),
                message: format!(
                    "System prompt template {} is empty; falling back to built-in template",
                    path.display()
                ),
            });
        }

        return Ok(TemplateSource {
            content,
            path: Some(path),
        });
    }

    Ok(TemplateSource {
        content: prompts::default_system_prompt_template().to_string(),
        path: None,
    })
}

fn build_prompt_template_vars(
    root: &Path,
    model: &str,
    sections: PromptTemplateSections<'_>,
) -> PromptTemplateVars {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let base_prompt = sections.base_prompt.unwrap_or_default().trim().to_string();
    let project_context = sections
        .project_context
        .unwrap_or_default()
        .trim()
        .to_string();
    let memory_index = sections.memory_index.unwrap_or_default().trim().to_string();
    let skills_list: Vec<PromptTemplateSkill> = sections
        .skills_list
        .iter()
        .map(|skill| PromptTemplateSkill {
            name: skill.name.clone(),
            description: skill.description.clone(),
            path: skill_access_path(skill),
        })
        .collect();
    let scoped_context = sections
        .scoped_context
        .iter()
        .map(|sa| {
            let relative_path = sa
                .path
                .strip_prefix(&canonical_root)
                .or_else(|_| sa.path.strip_prefix(root))
                .unwrap_or(sa.path.as_path())
                .display()
                .to_string();

            PromptTemplateScopedContext {
                scope: sa.scope.clone(),
                path: relative_path,
            }
        })
        .collect();
    let provider_selection = resolve_provider(model);
    let provider = provider_selection.kind.id().to_string();
    let is_openai_codex = provider_selection.kind == ProviderKind::OpenAICodex;

    PromptTemplateVars {
        identity_prompt: prompts::identity_prompt().to_string(),
        provider,
        invocation_term: if is_openai_codex {
            "function".to_string()
        } else {
            "tool".to_string()
        },
        invocation_term_plural: if is_openai_codex {
            "functions".to_string()
        } else {
            "tools".to_string()
        },
        is_openai_codex,
        base_prompt,
        project_context,
        memory_index,
        instruction_layers: Vec::new(),
        memory_suggestions: sections.memory_suggestions,
        available_skills: skills_list.clone(),
        skills_list,
        auto_loaded_skill_contents: Vec::new(),
        scoped_context,
        specialized_capabilities: sections.specialized_capabilities.to_vec(),
        cwd: root.display().to_string(),
        date: Utc::now().format("%Y-%m-%d").to_string(),
    }
}

fn build_prompt_template_capabilities(
    root: &Path,
    delegation_enabled: bool,
) -> Result<Vec<PromptTemplateCapability>> {
    subagents::capability_catalog(root, delegation_enabled).map(|capabilities| {
        capabilities
            .into_iter()
            .map(prompt_template_capability)
            .collect()
    })
}

fn fallback_prompt_template_capabilities(
    delegation_enabled: bool,
) -> Vec<PromptTemplateCapability> {
    subagents::fallback_capability_catalog(delegation_enabled)
        .into_iter()
        .map(prompt_template_capability)
        .collect()
}

fn prompt_template_capability(
    capability: subagents::CapabilityDescriptor,
) -> PromptTemplateCapability {
    let kind_label = match &capability.kind {
        subagents::CapabilityKind::Subagent { .. } => "standalone subagent".to_string(),
        subagents::CapabilityKind::BuiltinAlias(_) => "builtin alias".to_string(),
    };
    let backing = match capability.kind {
        subagents::CapabilityKind::Subagent { subagent } => {
            format!("`invoke_subagent(subagent: \"{subagent}\", prompt: \"...\")`")
        }
        subagents::CapabilityKind::BuiltinAlias(alias) => format!(
            "`invoke_subagent(subagent: \"{}\", prompt: \"...\")`",
            alias.runtime_name()
        ),
    };

    PromptTemplateCapability {
        name: capability.name,
        title: capability.title,
        description: capability.description,
        kind_label,
        backing,
    }
}

fn render_prompt_template(
    template: &str,
    vars: &PromptTemplateVars,
) -> std::result::Result<Option<String>, String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.add_template("system_prompt", template)
        .map_err(|error| error.to_string())?;

    let output = env
        .get_template("system_prompt")
        .map_err(|error| error.to_string())?
        .render(vars)
        .map_err(|error| error.to_string())?;

    let normalized = output.replace("\r\n", "\n");
    let trimmed = normalized.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
}

/// Renders an arbitrary standalone prompt template using the same template vars
/// available to the main system prompt pipeline, without automatically wrapping
/// it in the default ZDX base prompt.
///
/// # Errors
/// Returns an error if template rendering fails or produces an empty prompt.
pub fn render_standalone_prompt_template(
    config: &Config,
    root: &Path,
    model: &str,
    template: &str,
    memory_suggestions: bool,
    inclusion: PromptContextInclusion,
    skill_context: &StandalonePromptSkillContext,
) -> Result<String> {
    let sections_result = load_prompt_context_sections(root, config);
    let inline_project_context = if inclusion.project_context {
        sections_result.inline_project_context.as_deref()
    } else {
        None
    };
    let memory_index = if inclusion.memory_index {
        sections_result.memory_index.as_deref()
    } else {
        None
    };
    let skills_result = if inclusion.skills {
        load_skills_with_config(config, root)
    } else {
        LoadSkillsResult::default()
    };

    let specialized_capabilities =
        build_prompt_template_capabilities(root, config.subagents.enabled)
            .unwrap_or_else(|_| fallback_prompt_template_capabilities(config.subagents.enabled));

    let vars = build_prompt_template_vars(
        root,
        model,
        PromptTemplateSections {
            base_prompt: None,
            project_context: inline_project_context,
            memory_index,
            memory_suggestions,
            skills_list: &skills_result.skills,
            scoped_context: &sections_result.scoped_context,
            specialized_capabilities: &specialized_capabilities,
        },
    );

    let mut vars = vars;
    vars.available_skills = skill_context
        .available_skills
        .iter()
        .map(|skill| PromptTemplateSkill {
            name: skill.name.clone(),
            description: skill.description.clone(),
            path: skill_access_path(skill),
        })
        .collect();
    vars.auto_loaded_skill_contents
        .clone_from(&skill_context.auto_loaded_skill_contents);

    render_prompt_template(template.trim(), &vars)
        .map_err(|error| anyhow::anyhow!(error))?
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("standalone prompt template rendered an empty prompt"))
}

/// Collects all AGENTS.md paths to check, in order.
///
/// Order:
/// 1. `ZDX_HOME/AGENTS.md` (always included - global user config)
/// 2. ~/AGENTS.md (only if root is under home)
/// 3. Ancestors from home to root (only if root is under home)
/// 4. root/AGENTS.md
///
/// Paths are deduplicated (later occurrences removed).
pub fn collect_agents_paths(root: &Path) -> Vec<PathBuf> {
    collect_agents_paths_with_zdx_home(root, &paths::zdx_home())
}

/// Collects all AGENTS.md paths with an explicit ZDX home directory.
///
/// This is the core implementation that allows dependency injection of the
/// ZDX home path, primarily for testing without environment variable mutation.
///
/// See [`collect_agents_paths`] for the order of paths collected.
pub fn collect_agents_paths_with_zdx_home(root: &Path, zdx_home: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    // 1. ZDX_HOME/AGENTS.md (always - this is explicit user config)
    paths.push(zdx_home.join("AGENTS.md"));

    // Canonicalize root for comparison
    let canonical_root = root.canonicalize().ok();

    // 2-3. User home and ancestors (only if root is under home)
    if let Some(home) = paths::home_dir()
        && let Some(ref cr) = canonical_root
        && let Ok(canonical_home) = home.canonicalize()
    {
        // Check if root is under home
        if let Ok(relative) = cr.strip_prefix(&canonical_home) {
            // Include ~/AGENTS.md
            paths.push(home.join("AGENTS.md"));

            // Add each ancestor directory between home and root
            let mut current = canonical_home.clone();
            for component in relative.components() {
                current = current.join(component);
                // Don't add the root itself yet (added at end)
                if current != *cr {
                    paths.push(current.join("AGENTS.md"));
                }
            }
        }
    }

    // 4. Root/AGENTS.md (project root)
    if let Some(cr) = canonical_root {
        paths.push(cr.join("AGENTS.md"));
    } else {
        // Fallback if canonicalization fails
        paths.push(root.join("AGENTS.md"));
    }

    // Deduplicate while preserving order
    deduplicate_paths(paths)
}

/// Removes duplicate paths while preserving order (keeps first occurrence).
fn deduplicate_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|p| {
            // Try to canonicalize for comparison, fallback to original
            let key = p.canonicalize().unwrap_or_else(|_| p.clone());
            seen.insert(key)
        })
        .collect()
}

fn select_context_file(dir: &Path) -> Option<PathBuf> {
    let primary = dir.join(PRIMARY_CONTEXT_FILE_NAME);
    if primary.is_file() {
        return Some(primary);
    }

    let fallback = dir.join(FALLBACK_CONTEXT_FILE_NAME);
    fallback.is_file().then_some(fallback)
}

fn collect_existing_context_paths(root: &Path) -> Vec<PathBuf> {
    collect_agents_paths(root)
        .into_iter()
        .filter_map(|agents_path| agents_path.parent().and_then(select_context_file))
        .collect()
}

fn context_file_priority(path: &Path) -> Option<u8> {
    match path.file_name()?.to_str()? {
        PRIMARY_CONTEXT_FILE_NAME => Some(0),
        FALLBACK_CONTEXT_FILE_NAME => Some(1),
        _ => None,
    }
}

/// Maximum depth to walk when discovering scoped project context files.
/// Keeps traversal fast in large repos.
const SCOPED_CONTEXT_MAX_DEPTH: usize = 4;

/// Maximum number of scoped project context files to discover.
const SCOPED_CONTEXT_LIMIT: usize = 200;

/// Discovers scoped `AGENTS.md`/`CLAUDE.md` files in subdirectories of the project root.
///
/// Uses gitignore-aware walking to skip ignored directories (target/, .git/, etc.).
/// Limited to [`SCOPED_CONTEXT_MAX_DEPTH`] levels deep and [`SCOPED_CONTEXT_LIMIT`] files.
/// Returns files sorted by path for deterministic ordering.
/// The root context file itself is excluded (it's handled as inline context).
pub fn discover_scoped_context(root: &Path) -> Vec<ScopedContextFile> {
    use ignore::WalkBuilder;

    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root_primary = canonical_root.join(PRIMARY_CONTEXT_FILE_NAME);
    let root_fallback = canonical_root.join(FALLBACK_CONTEXT_FILE_NAME);
    let mut candidates: Vec<(String, PathBuf, u8)> = Vec::new();

    let walker = WalkBuilder::new(&canonical_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .max_depth(Some(SCOPED_CONTEXT_MAX_DEPTH))
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        let Some(priority) = context_file_priority(path) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if canonical == root_primary || canonical == root_fallback {
            continue;
        }
        if let Ok(relative) = canonical.strip_prefix(&canonical_root)
            && let Some(scope_dir) = relative.parent()
            && !scope_dir.as_os_str().is_empty()
        {
            let scope = scope_dir.display().to_string();
            candidates.push((scope, canonical, priority));
        }
    }

    candidates.sort_by(
        |(scope_a, path_a, priority_a), (scope_b, path_b, priority_b)| {
            scope_a
                .cmp(scope_b)
                .then(priority_a.cmp(priority_b))
                .then(path_a.cmp(path_b))
        },
    );

    let mut seen_scopes = std::collections::HashSet::new();
    let mut scoped: Vec<ScopedContextFile> = Vec::new();
    for (scope, path, _) in candidates {
        if scoped.len() >= SCOPED_CONTEXT_LIMIT {
            break;
        }
        if seen_scopes.insert(scope.clone()) {
            scoped.push(ScopedContextFile { path, scope });
        }
    }

    scoped
}

/// Loads inline project context files from the collected hierarchy.
///
/// For each directory in the hierarchy, `AGENTS.md` is preferred; `CLAUDE.md`
/// is used only when `AGENTS.md` is absent.
/// Returns None if no files were found or all were empty.
/// Empty files are skipped silently.
/// Unreadable files generate a warning but don't fail.
/// Large files are truncated with a warning.
pub fn load_all_agents_files(root: &Path) -> Option<LoadedContext> {
    let paths = collect_existing_context_paths(root);
    let mut loaded_paths: Vec<PathBuf> = Vec::new();
    let mut sections: Vec<String> = Vec::new();
    let mut warnings: Vec<ContextWarning> = Vec::new();

    for path in paths {
        if !path.exists() {
            continue;
        }

        match fs::read(&path) {
            Ok(bytes) => {
                // Check for truncation
                let (content_bytes, was_truncated) = if bytes.len() > MAX_AGENTS_FILE_SIZE {
                    warnings.push(ContextWarning::truncated(&path, bytes.len()));
                    (&bytes[..MAX_AGENTS_FILE_SIZE], true)
                } else {
                    (bytes.as_slice(), false)
                };

                // Convert to string (lossy for non-UTF8)
                let content = String::from_utf8_lossy(content_bytes);
                let trimmed = content.trim();

                if !trimmed.is_empty() {
                    let suffix = if was_truncated { " [truncated]" } else { "" };
                    sections.push(format!("## {}{}\n\n{}", path.display(), suffix, trimmed));
                    loaded_paths.push(path);
                }
            }
            Err(e) => {
                warnings.push(ContextWarning::unreadable(&path, &e));
            }
        }
    }

    if sections.is_empty() && warnings.is_empty() {
        return None;
    }

    let content = sections.join("\n\n");
    Some(LoadedContext {
        content,
        loaded_paths,
        warnings,
    })
}

/// Loads the memory index file from the configured location.
///
/// Returns None if no file was found or it was empty.
/// Empty files are skipped silently.
/// Unreadable files generate a warning but don't fail.
/// Large files are truncated with a warning.
fn load_memory_index(config: &Config) -> Option<LoadedMemoryIndex> {
    let path = config.memory.effective_index_file();
    load_memory_index_from_path(&path)
}

fn load_memory_index_from_path(path: &Path) -> Option<LoadedMemoryIndex> {
    let mut warnings: Vec<ContextWarning> = Vec::new();

    if !path.exists() {
        return None;
    }

    match fs::read(path) {
        Ok(bytes) => {
            let content_bytes = if bytes.len() > MAX_MEMORY_FILE_SIZE {
                warnings.push(ContextWarning::truncated_with_limit(
                    path,
                    bytes.len(),
                    MAX_MEMORY_FILE_SIZE,
                ));
                &bytes[..MAX_MEMORY_FILE_SIZE]
            } else {
                bytes.as_slice()
            };

            let content = String::from_utf8_lossy(content_bytes);
            let trimmed = content.trim();

            if trimmed.is_empty() && warnings.is_empty() {
                None
            } else {
                Some(LoadedMemoryIndex {
                    content: trimmed.to_string(),
                    loaded_paths: vec![path.to_path_buf()],
                    warnings,
                })
            }
        }
        Err(error) => Some(LoadedMemoryIndex {
            content: String::new(),
            loaded_paths: Vec::new(),
            warnings: vec![ContextWarning::unreadable(path, &error)],
        }),
    }
}

/// Result of building the effective system prompt.
#[derive(Debug, Clone, Default)]
pub struct EffectivePrompt {
    /// The combined system prompt (config + inline project context + optional memory index + template sections).
    pub prompt: Option<String>,
    /// Paths of inline project context files that were inlined (in order).
    pub loaded_agents_paths: Vec<PathBuf>,
    /// Scoped project context files discovered in subdirectories (listed as paths, not inlined).
    pub scoped_context_paths: Vec<PathBuf>,
    /// Warnings generated during context loading.
    pub warnings: Vec<ContextWarning>,
    /// Skills loaded from configured sources.
    pub loaded_skills: Vec<Skill>,
}

/// Selects which ambient context blocks are exposed to a rendered prompt template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptContextInclusion {
    pub project_context: bool,
    pub memory_index: bool,
    pub skills: bool,
}

impl Default for PromptContextInclusion {
    fn default() -> Self {
        Self {
            project_context: true,
            memory_index: true,
            skills: true,
        }
    }
}

#[derive(Debug, Default)]
struct PromptContextSectionsResult {
    loaded_agents_paths: Vec<PathBuf>,
    scoped_context: Vec<ScopedContextFile>,
    warnings: Vec<ContextWarning>,
    inline_project_context: Option<String>,
    memory_index: Option<String>,
}

fn load_prompt_context_sections(root: &Path, config: &Config) -> PromptContextSectionsResult {
    let mut result = PromptContextSectionsResult::default();

    if let Some(loaded) = load_all_agents_files(root) {
        result.loaded_agents_paths = loaded.loaded_paths;
        result.warnings = loaded.warnings;

        let trimmed = loaded.content.trim();
        if !trimmed.is_empty() {
            result.inline_project_context = Some(loaded.content);
        }
    }

    result.scoped_context = discover_scoped_context(root);

    if let Some(loaded_memory_index) = load_memory_index(config) {
        result.warnings.extend(loaded_memory_index.warnings);

        if !loaded_memory_index.content.trim().is_empty() {
            result.memory_index = Some(loaded_memory_index.content);
        }
    }

    result
}

fn load_skills_with_config(config: &Config, root: &Path) -> LoadSkillsResult {
    let mut skill_options = LoadSkillsOptions::new(root);
    skill_options.sources = config.skills.sources.clone();
    skill_options
        .ignored_skills
        .clone_from(&config.skills.ignored_skills);
    skill_options
        .include_skills
        .clone_from(&config.skills.include_skills);
    load_skills(&skill_options)
}

/// Builds an effective prompt from the default system prompt template plus
/// additive instruction layers rendered with the same context/template pipeline.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn build_prompt_with_context_and_layers(
    config: &Config,
    root: &Path,
    model: &str,
    instruction_layers: &[&str],
    memory_suggestions: bool,
    inclusion: PromptContextInclusion,
) -> Result<EffectivePrompt> {
    let base_prompt = config.effective_system_prompt()?;

    let sections_result = load_prompt_context_sections(root, config);
    let loaded_agents_paths = if inclusion.project_context {
        sections_result.loaded_agents_paths.clone()
    } else {
        Vec::new()
    };
    let scoped_context = if inclusion.project_context {
        sections_result.scoped_context.clone()
    } else {
        Vec::new()
    };
    let mut warnings = sections_result.warnings.clone();
    let inline_project_context = if inclusion.project_context {
        sections_result.inline_project_context.clone()
    } else {
        None
    };
    let memory_index = if inclusion.memory_index {
        sections_result.memory_index.clone()
    } else {
        None
    };

    let skills_result = if inclusion.skills {
        load_skills_with_config(config, root)
    } else {
        LoadSkillsResult::default()
    };
    let LoadSkillsResult {
        skills,
        warnings: skill_warnings,
    } = skills_result;

    let specialized_capabilities = match build_prompt_template_capabilities(
        root,
        config.subagents.enabled,
    ) {
        Ok(capabilities) => capabilities,
        Err(error) => {
            warnings.push(ContextWarning {
                path: None,
                message: format!(
                    "Failed to build specialized capability catalog for prompt context: {error}; falling back to built-in capability metadata"
                ),
            });
            fallback_prompt_template_capabilities(config.subagents.enabled)
        }
    };

    let mut vars = build_prompt_template_vars(
        root,
        model,
        PromptTemplateSections {
            base_prompt: base_prompt.as_deref(),
            project_context: inline_project_context.as_deref(),
            memory_index: memory_index.as_deref(),
            memory_suggestions,
            skills_list: &skills,
            scoped_context: &scoped_context,
            specialized_capabilities: &specialized_capabilities,
        },
    );

    vars.instruction_layers = render_instruction_layers(instruction_layers, &vars, &mut warnings);

    let prompt = render_system_prompt_with_fallback(
        config,
        &vars,
        &mut warnings,
        base_prompt.as_deref(),
        inline_project_context.as_deref(),
        memory_index.as_deref(),
    );

    if skills.len() > 20 {
        warnings.push(ContextWarning {
            path: None,
            message: format!("Loaded {} skills; prompt may be large", skills.len()),
        });
    }

    warnings.extend(skill_warnings.into_iter().map(|warning| ContextWarning {
        path: Some(warning.skill_path),
        message: warning.message,
    }));

    Ok(EffectivePrompt {
        prompt,
        loaded_agents_paths,
        scoped_context_paths: scoped_context.iter().map(|sa| sa.path.clone()).collect(),
        warnings,
        loaded_skills: skills,
    })
}

fn render_instruction_layers(
    templates: &[&str],
    vars: &PromptTemplateVars,
    warnings: &mut Vec<ContextWarning>,
) -> Vec<String> {
    let mut rendered = Vec::new();

    for (idx, template) in templates.iter().enumerate() {
        let trimmed = template.trim();
        if trimmed.is_empty() {
            continue;
        }

        match render_prompt_template(trimmed, vars) {
            Ok(Some(layer)) => rendered.push(layer),
            Ok(None) => {}
            Err(error) => warnings.push(ContextWarning {
                path: None,
                message: format!(
                    "Failed to render instruction layer {}: {error}; skipping that layer",
                    idx + 1
                ),
            }),
        }
    }

    rendered
}

fn render_system_prompt_with_fallback(
    config: &Config,
    vars: &PromptTemplateVars,
    warnings: &mut Vec<ContextWarning>,
    base_prompt: Option<&str>,
    inline_project_context: Option<&str>,
    memory_index: Option<&str>,
) -> Option<String> {
    let template_source = match load_prompt_template(config) {
        Ok(source) => source,
        Err(warning) => {
            warnings.push(warning);
            TemplateSource {
                content: prompts::default_system_prompt_template().to_string(),
                path: None,
            }
        }
    };

    match render_prompt_template(&template_source.content, vars) {
        Ok(rendered) => rendered,
        Err(error) => {
            let legacy_surface_rules_hint = error.contains("surface_rules")
                || template_source.content.contains("surface_rules");
            warnings.push(ContextWarning {
                path: template_source.path.clone(),
                message: if legacy_surface_rules_hint {
                    format!(
                        "Failed to render system prompt template: {error}; `surface_rules` is no longer available in template vars, use `instruction_layers` instead; falling back to default template"
                    )
                } else {
                    format!(
                        "Failed to render system prompt template: {error}; falling back to default template"
                    )
                },
            });

            match render_prompt_template(prompts::default_system_prompt_template(), vars) {
                Ok(rendered) => rendered,
                Err(default_error) => {
                    warnings.push(ContextWarning {
                        path: None,
                        message: format!(
                            "Failed to render default system prompt template: {default_error}; falling back to base prompt assembly"
                        ),
                    });
                    combine_prompt_sections(
                        base_prompt,
                        inline_project_context,
                        memory_index,
                        &vars.instruction_layers,
                    )
                }
            }
        }
    }
}

/// Builds the effective system prompt by combining config, inline project context,
/// an optional memory index, and template-driven sections.
///
/// Project context files are loaded hierarchically from:
/// 1. `ZDX_HOME/AGENTS.md` (or `CLAUDE.md` if absent)
/// 2. `~/AGENTS.md` (or `CLAUDE.md` if absent)
/// 3. Ancestor directories from home to project root
/// 4. Project root
///
/// Returns the combined prompt, the list of loaded project context file paths, and any warnings.
/// This function is UI-agnostic; callers should surface warnings via the renderer.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn build_effective_system_prompt_with_paths(
    config: &Config,
    root: &Path,
    memory_suggestions: bool,
) -> Result<EffectivePrompt> {
    build_effective_system_prompt_with_paths_and_instruction_layers(
        config,
        root,
        &[],
        memory_suggestions,
    )
}

/// Builds the effective system prompt by combining config, inline project context,
/// an optional memory index, template-driven sections, and additive
/// instruction layers (for example surfaces or automation harnesses).
///
/// # Errors
/// Returns an error if the operation fails.
pub fn build_effective_system_prompt_with_paths_and_instruction_layers(
    config: &Config,
    root: &Path,
    instruction_layers: &[&str],
    memory_suggestions: bool,
) -> Result<EffectivePrompt> {
    build_prompt_with_context_and_layers(
        config,
        root,
        &config.model,
        instruction_layers,
        memory_suggestions,
        PromptContextInclusion::default(),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::config::SkillSourceToggles;
    use crate::skills::SkillSource;

    #[test]
    fn test_collect_agents_paths_includes_zdx_home() {
        let zdx_home = tempdir().unwrap();
        let root = tempdir().unwrap();
        let paths = collect_agents_paths_with_zdx_home(root.path(), zdx_home.path());

        // Should include ZDX_HOME/AGENTS.md
        let zdx_home_agents = zdx_home.path().join("AGENTS.md");
        assert!(
            paths.contains(&zdx_home_agents),
            "Should include ZDX_HOME/AGENTS.md, got: {paths:?}"
        );
    }

    #[test]
    fn test_collect_agents_paths_includes_root() {
        let dir = tempdir().unwrap();
        let paths = collect_agents_paths(dir.path());

        // Should include root/AGENTS.md (canonicalized)
        let root_agents = dir.path().canonicalize().unwrap().join("AGENTS.md");
        assert!(
            paths.contains(&root_agents),
            "Should include root/AGENTS.md, got: {paths:?}"
        );
    }

    #[test]
    fn test_collect_agents_paths_deduplicates() {
        // Use a temp directory as ZDX_HOME
        let zdx_home = tempdir().unwrap();

        // If root is ZDX_HOME, should not have duplicates
        let paths = collect_agents_paths_with_zdx_home(zdx_home.path(), zdx_home.path());

        // Count occurrences of ZDX_HOME/AGENTS.md
        let zdx_agents = zdx_home.path().join("AGENTS.md");
        let count = paths
            .iter()
            .filter(|p| {
                p.canonicalize().unwrap_or_else(|_| (*p).clone())
                    == zdx_agents
                        .canonicalize()
                        .unwrap_or_else(|_| zdx_agents.clone())
            })
            .count();
        assert!(count <= 1, "Should deduplicate paths, got count: {count}");
    }

    #[test]
    fn test_load_all_agents_files_single() {
        let dir = tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");
        fs::write(&agents_md, "Single file content").unwrap();

        let result = load_all_agents_files(dir.path());
        assert!(result.is_some());

        let loaded = result.unwrap();
        assert!(loaded.content.contains("Single file content"));
        assert!(!loaded.loaded_paths.is_empty());
    }

    #[test]
    fn test_load_all_agents_files_falls_back_to_claude() {
        let dir = tempdir().unwrap();
        let claude_md = dir.path().join("CLAUDE.md");
        fs::write(&claude_md, "Claude fallback content").unwrap();
        let canonical_claude_md = claude_md.canonicalize().unwrap();

        let result = load_all_agents_files(dir.path());
        assert!(result.is_some());

        let loaded = result.unwrap();
        assert!(loaded.content.contains("Claude fallback content"));
        assert!(
            loaded
                .loaded_paths
                .iter()
                .any(|path| path == &canonical_claude_md)
        );
    }

    #[test]
    fn test_load_all_agents_files_prefers_agents_over_claude() {
        let dir = tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");
        let claude_md = dir.path().join("CLAUDE.md");
        fs::write(&agents_md, "Agents content").unwrap();
        fs::write(&claude_md, "Claude should be ignored").unwrap();
        let canonical_agents_md = agents_md.canonicalize().unwrap();
        let canonical_claude_md = claude_md.canonicalize().unwrap();

        let result = load_all_agents_files(dir.path());
        assert!(result.is_some());

        let loaded = result.unwrap();
        assert!(loaded.content.contains("Agents content"));
        assert!(!loaded.content.contains("Claude should be ignored"));
        assert!(
            loaded
                .loaded_paths
                .iter()
                .any(|path| path == &canonical_agents_md)
        );
        assert!(
            !loaded
                .loaded_paths
                .iter()
                .any(|path| path == &canonical_claude_md)
        );
    }

    #[test]
    fn test_load_all_agents_files_none() {
        let dir = tempdir().unwrap();
        // Create a subdirectory with no AGENTS.md anywhere in hierarchy
        let subdir = dir.path().join("deep").join("nested").join("project");
        fs::create_dir_all(&subdir).unwrap();

        // Note: This might still find ~/AGENTS.md or ZDX_HOME/AGENTS.md if they exist
        // The test verifies the function doesn't crash with no files in the temp dir
        let _result = load_all_agents_files(&subdir);
        // Just verify it doesn't panic
    }

    #[test]
    fn test_load_all_agents_files_skips_empty() {
        let dir = tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");
        fs::write(&agents_md, "   ").unwrap(); // Empty/whitespace only

        let result = load_all_agents_files(dir.path());
        // Should not include the empty file in loaded_paths
        if let Some(loaded) = result {
            let root_agents = dir.path().canonicalize().unwrap().join("AGENTS.md");
            assert!(
                !loaded.loaded_paths.contains(&root_agents),
                "Should skip empty files"
            );
        }
    }

    #[test]
    fn test_load_all_agents_files_multiple_in_hierarchy() {
        // Create a nested directory structure
        // Note: tempdir is typically not under home, so we test the root loading
        // and verify ancestor loading works when paths ARE under home
        let base = tempdir().unwrap();
        let child = base.path().join("child");
        fs::create_dir_all(&child).unwrap();

        // Create AGENTS.md in base and child
        fs::write(base.path().join("AGENTS.md"), "Base guidelines").unwrap();
        fs::write(child.join("AGENTS.md"), "Child guidelines").unwrap();

        // When root is child, it should at least find child's AGENTS.md
        let result = load_all_agents_files(&child);
        assert!(result.is_some());

        let loaded = result.unwrap();
        // Should contain child (root) AGENTS.md
        assert!(
            loaded.content.contains("Child guidelines"),
            "Should include child/root"
        );
    }

    #[test]
    fn test_discover_scoped_context_prefers_agents_and_falls_back_to_claude() {
        let dir = tempdir().unwrap();
        let claude_scope = dir.path().join("claude-scope");
        let agents_scope = dir.path().join("agents-scope");
        let mixed_scope = dir.path().join("mixed-scope");
        fs::create_dir_all(&claude_scope).unwrap();
        fs::create_dir_all(&agents_scope).unwrap();
        fs::create_dir_all(&mixed_scope).unwrap();

        fs::write(claude_scope.join("CLAUDE.md"), "Claude scoped rule").unwrap();
        fs::write(agents_scope.join("AGENTS.md"), "Agents scoped rule").unwrap();
        fs::write(
            mixed_scope.join("AGENTS.md"),
            "Preferred agents scoped rule",
        )
        .unwrap();
        fs::write(mixed_scope.join("CLAUDE.md"), "Ignored claude scoped rule").unwrap();

        let scoped = discover_scoped_context(dir.path());
        let paths: Vec<PathBuf> = scoped.iter().map(|entry| entry.path.clone()).collect();

        assert!(
            paths
                .iter()
                .any(|path| path.ends_with("claude-scope/CLAUDE.md"))
        );
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with("agents-scope/AGENTS.md"))
        );
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with("mixed-scope/AGENTS.md"))
        );
        assert!(
            !paths
                .iter()
                .any(|path| path.ends_with("mixed-scope/CLAUDE.md"))
        );
    }

    #[test]
    fn test_collect_agents_paths_order_under_home() {
        // Test that paths are collected in correct order when under home
        let zdx_home = tempdir().unwrap();

        if let Some(home) = paths::home_dir() {
            // Create a path that's conceptually under home
            // (we just verify the function produces ordered paths)
            let paths = collect_agents_paths_with_zdx_home(&home, zdx_home.path());

            // Should include ZDX_HOME first
            let zdx_home_agents = zdx_home.path().join("AGENTS.md");
            assert_eq!(
                paths.first().map(std::path::PathBuf::as_path),
                Some(zdx_home_agents.as_path()),
                "ZDX_HOME/AGENTS.md should be first"
            );

            // Should include home/AGENTS.md
            let home_agents = home.join("AGENTS.md");
            assert!(
                paths.iter().any(|p| {
                    p.canonicalize().unwrap_or_else(|_| p.clone())
                        == home_agents.canonicalize().unwrap_or(home_agents.clone())
                }),
                "Should include ~/AGENTS.md"
            );
        }
    }

    #[test]
    fn test_deduplicate_paths() {
        let paths = vec![
            PathBuf::from("/a/b/c"),
            PathBuf::from("/a/b/c"),
            PathBuf::from("/x/y/z"),
            PathBuf::from("/a/b/c"),
        ];
        let deduped = deduplicate_paths(paths);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0], PathBuf::from("/a/b/c"));
        assert_eq!(deduped[1], PathBuf::from("/x/y/z"));
    }

    #[test]
    fn test_load_memory_index_reads_global_only() {
        let zdx_home = tempdir().unwrap();

        fs::write(zdx_home.path().join(MEMORY_INDEX_FILE_NAME), "Global facts").unwrap();

        let loaded =
            load_memory_index_from_path(&zdx_home.path().join(MEMORY_INDEX_FILE_NAME)).unwrap();

        assert!(loaded.content.contains("Global facts"));
        assert_eq!(loaded.loaded_paths.len(), 1);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn test_large_memory_file_truncated_with_warning() {
        let zdx_home = tempdir().unwrap();
        let memory_md = zdx_home.path().join(MEMORY_INDEX_FILE_NAME);

        let large_content = "x".repeat(MAX_MEMORY_FILE_SIZE + 1000);
        fs::write(&memory_md, &large_content).unwrap();

        let loaded =
            load_memory_index_from_path(&zdx_home.path().join(MEMORY_INDEX_FILE_NAME)).unwrap();

        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.message.contains("Truncated"))
        );
        assert!(loaded.content.len() <= MAX_MEMORY_FILE_SIZE);
    }

    #[test]
    fn test_unreadable_agents_triggers_warning() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");

        // Create file with no read permissions
        fs::write(&agents_md, "Secret content").unwrap();
        let mut perms = fs::metadata(&agents_md).unwrap().permissions();
        perms.set_mode(0o000); // No permissions
        fs::set_permissions(&agents_md, perms).unwrap();

        // If the environment still allows reading, skip because the scenario can't be simulated.
        if fs::read_to_string(&agents_md).is_ok() {
            return;
        }

        let result = load_all_agents_files(dir.path());

        // Restore permissions for cleanup
        let mut perms = fs::metadata(&agents_md).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&agents_md, perms).unwrap();

        // Should return Some because we have a warning to report
        assert!(result.is_some(), "Should return Some with warning");

        let loaded = result.unwrap();
        // Content should not include the unreadable file
        assert!(
            !loaded.content.contains("Secret content"),
            "Should not include unreadable content"
        );
        // Should have a warning
        assert!(!loaded.warnings.is_empty(), "Should have a warning");
        assert!(
            loaded.warnings[0].message.contains("Failed to read"),
            "Warning should mention read failure"
        );
    }

    #[test]
    fn test_large_agents_file_truncated_with_warning() {
        let dir = tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");

        // Create a file larger than MAX_AGENTS_FILE_SIZE
        let large_content = "x".repeat(MAX_AGENTS_FILE_SIZE + 1000);
        fs::write(&agents_md, &large_content).unwrap();

        let result = load_all_agents_files(dir.path());
        assert!(result.is_some());

        let loaded = result.unwrap();
        // Should have a warning about truncation
        assert!(
            !loaded.warnings.is_empty(),
            "Should have a truncation warning"
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.message.contains("Truncated")),
            "Warning should mention truncation"
        );
        // Content should be marked as truncated
        assert!(
            loaded.content.contains("[truncated]"),
            "Content should show truncation marker"
        );
        // Content should be capped at MAX_AGENTS_FILE_SIZE
        // (actual content is trimmed, so just verify it's smaller than original)
        assert!(
            loaded.content.len() < large_content.len(),
            "Content should be truncated"
        );
    }

    #[test]
    fn test_context_warning_constructors() {
        // Test unreadable warning
        let path = PathBuf::from("/test/path/AGENTS.md");
        let io_error =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let warning = ContextWarning::unreadable(&path, &io_error);
        assert!(warning.path.is_some());
        assert!(warning.message.contains("Failed to read"));
        assert!(warning.message.contains("permission denied"));

        // Test truncated warning
        let truncated = ContextWarning::truncated(&path, 100_000);
        assert!(truncated.path.is_some());
        assert!(truncated.message.contains("Truncated"));
        assert!(truncated.message.contains("100000"));
    }

    #[test]
    fn test_render_prompt_template_unknown_variable_fails() {
        let vars = build_prompt_template_vars(
            Path::new("/tmp"),
            "anthropic:claude-opus-4-6",
            PromptTemplateSections {
                base_prompt: Some("hello"),
                project_context: None,
                memory_index: None,
                memory_suggestions: false,
                skills_list: &[],
                scoped_context: &[],
                specialized_capabilities: &[],
            },
        );

        let err = render_prompt_template("{{unknown}}", &vars).unwrap_err();
        assert!(err.contains("undefined") || err.contains("unknown"));
    }

    #[test]
    fn test_render_prompt_template_supports_if_and_for() {
        let vars = build_prompt_template_vars(
            Path::new("/tmp"),
            "anthropic:claude-opus-4-6",
            PromptTemplateSections {
                base_prompt: Some("hello"),
                project_context: None,
                memory_index: None,
                memory_suggestions: false,
                skills_list: &[],
                scoped_context: &[],
                specialized_capabilities: &[],
            },
        );

        let rendered = render_prompt_template(
            "{% if base_prompt %}{{ base_prompt }}\n{% endif %}{% for line in [\"alpha\", \"beta\"] %}- {{ line }}\n{% endfor %}",
            &vars,
        )
        .unwrap()
        .unwrap();

        assert!(rendered.contains("hello"));
        assert!(rendered.contains("- alpha"));
        assert!(rendered.contains("- beta"));
    }

    #[test]
    fn test_render_prompt_template_can_branch_on_memory_suggestions_flag() {
        let vars = build_prompt_template_vars(
            Path::new("/tmp"),
            "anthropic:claude-opus-4-6",
            PromptTemplateSections {
                base_prompt: Some("hello"),
                project_context: None,
                memory_index: None,
                memory_suggestions: true,
                skills_list: &[],
                scoped_context: &[],
                specialized_capabilities: &[],
            },
        );

        let rendered = render_prompt_template(
            "{% if memory_suggestions %}MEMORY_SUGGESTIONS_ON{% endif %}",
            &vars,
        )
        .unwrap()
        .unwrap_or_default();

        assert!(rendered.contains("MEMORY_SUGGESTIONS_ON"));
    }

    #[test]
    fn test_render_prompt_template_supports_structured_skills_and_capabilities() {
        let skills = vec![Skill {
            name: "demo-skill".to_string(),
            description: "Use <special> syntax".to_string(),
            file_path: PathBuf::from("/tmp/demo&skill/SKILL.md"),
            base_dir: PathBuf::from("/tmp/demo&skill"),
            source: SkillSource::ZdxUser,
        }];
        let capabilities = build_prompt_template_capabilities(Path::new("/tmp"), true).unwrap();

        let vars = build_prompt_template_vars(
            Path::new("/tmp"),
            "codex:gpt-5.3-codex",
            PromptTemplateSections {
                base_prompt: Some("hello"),
                project_context: None,
                memory_index: None,
                memory_suggestions: false,
                skills_list: &skills,
                scoped_context: &[],
                specialized_capabilities: &capabilities,
            },
        );

        let rendered = render_prompt_template(
            "{% for skill in skills_list %}<name>{{ skill.name }}</name><description>{{ skill.description }}</description><path>{{ skill.path }}</path>{% endfor %}\n{% for capability in specialized_capabilities %}<title>{{ capability.title }}</title><name>{{ capability.name }}</name><kind>{{ capability.kind_label }}</kind><backing>{{ capability.backing }}</backing>{% endfor %}",
            &vars,
        )
        .unwrap()
        .unwrap();

        assert!(rendered.contains("<name>demo-skill</name>"));
        assert!(rendered.contains("Use <special> syntax"));
        assert!(rendered.contains("demo&skill"));
        assert!(rendered.contains("<title>Task</title>"));
        assert!(rendered.contains("<title>Oracle</title>"));
        assert!(rendered.contains("invoke_subagent"));
    }

    #[test]
    fn test_default_template_disambiguates_skills_memory_and_subagents() {
        let skills = vec![Skill {
            name: "memory".to_string(),
            description: "Memory workflow".to_string(),
            file_path: PathBuf::from("/tmp/memory/SKILL.md"),
            base_dir: PathBuf::from("/tmp/memory"),
            source: SkillSource::ZdxUser,
        }];
        let capabilities = build_prompt_template_capabilities(Path::new("/tmp"), true).unwrap();

        let vars = build_prompt_template_vars(
            Path::new("/tmp"),
            "codex:gpt-5.3-codex",
            PromptTemplateSections {
                base_prompt: None,
                project_context: None,
                memory_index: Some("# Memory Index\nUse the `memory` skill for detailed memory."),
                memory_suggestions: false,
                skills_list: &skills,
                scoped_context: &[],
                specialized_capabilities: &capabilities,
            },
        );

        let rendered =
            render_prompt_template(crate::prompts::default_system_prompt_template(), &vars)
                .unwrap()
                .unwrap();

        assert!(!rendered.contains("### How to use memory"));
        assert!(rendered.contains("### When to consult memory"));
        assert!(rendered.contains(
            "For any memory-related task, the first step is to read the `memory` skill `SKILL.md`."
        ));
        assert!(rendered.contains(
            "For factual questions about the user or something they own or manage — such as belongings, relationships, documents, preferences, work, trips, history, or already-documented projects — MUST consult the embedded memory index and relevant memory notes before answering from general knowledge or asking for more context."
        ));
        assert!(rendered.contains("### Saving memory"));
        assert!(
            rendered
                .contains("If the user explicitly says \"remember X\", MUST save it immediately.")
        );
        assert!(!rendered.contains("### Memory index rules"));
        assert!(!rendered.contains(
            "Use the normal file tools (for example `read`, `grep`, and `glob`) to inspect memory files."
        ));
        assert!(
            !rendered
                .contains("Keep full detail in notes and the memory index as a concise index.")
        );
        assert!(rendered.contains("<memory_index>"));
        assert!(rendered.contains(
            "Skills are instruction files: read the `SKILL.md`, then follow it with normal"
        ));

        let skills_pos = rendered.find("<skills_registry>").unwrap();
        let memory_pos = rendered.find("<memory_contract>").unwrap();
        assert!(skills_pos < memory_pos);

        let memory_skill_pos = rendered
            .find("For any memory-related task, the first step is to read the `memory` skill `SKILL.md`.")
            .unwrap();
        let when_to_consult_pos = rendered.find("### When to consult memory").unwrap();
        let saving_memory_pos = rendered.find("### Saving memory").unwrap();
        let memory_index_pos = rendered.find("<memory_index>").unwrap();
        assert!(memory_skill_pos < when_to_consult_pos);
        assert!(when_to_consult_pos < saving_memory_pos);
        assert!(saving_memory_pos < memory_index_pos);
        assert!(memory_skill_pos < memory_index_pos);
    }

    #[test]
    fn test_prompt_template_vars_provider_terms() {
        let anthropic = build_prompt_template_vars(
            Path::new("/tmp"),
            "anthropic:claude-opus-4-6",
            PromptTemplateSections {
                base_prompt: Some("hello"),
                project_context: None,
                memory_index: None,
                memory_suggestions: false,
                skills_list: &[],
                scoped_context: &[],
                specialized_capabilities: &[],
            },
        );

        assert_eq!(anthropic.provider, "anthropic");
        assert_eq!(anthropic.invocation_term, "tool");
        assert!(!anthropic.is_openai_codex);

        let codex = build_prompt_template_vars(
            Path::new("/tmp"),
            "codex:gpt-5.3-codex",
            PromptTemplateSections {
                base_prompt: Some("hello"),
                project_context: None,
                memory_index: None,
                memory_suggestions: false,
                skills_list: &[],
                scoped_context: &[],
                specialized_capabilities: &[],
            },
        );

        assert_eq!(codex.provider, "openai-codex");
        assert_eq!(codex.invocation_term, "function");
        assert!(codex.is_openai_codex);
    }

    #[test]
    fn test_template_mode_omits_z_identity_for_claude_cli_provider() {
        let dir = tempdir().unwrap();

        let mut config = crate::config::Config {
            model: "claude-cli:claude-sonnet-4-5".to_string(),
            ..Default::default()
        };
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let effective =
            build_effective_system_prompt_with_paths(&config, dir.path(), false).unwrap();
        let prompt = effective.prompt.unwrap_or_default();

        assert!(!prompt.contains(crate::prompts::identity_prompt()));
    }

    #[test]
    fn test_template_mode_includes_linked_identity_for_non_claude() {
        let dir = tempdir().unwrap();

        let mut config = crate::config::Config {
            model: "openai-codex:gpt-5.4".to_string(),
            ..Default::default()
        };
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let effective =
            build_effective_system_prompt_with_paths(&config, dir.path(), false).unwrap();
        let prompt = effective.prompt.unwrap_or_default();

        assert!(prompt.contains(crate::prompts::identity_prompt()));
    }

    #[test]
    fn test_template_mode_default_template_renders_runtime_and_context_sections() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "Agent note").unwrap();

        let mut config = crate::config::Config {
            system_prompt: Some("Base prompt".to_string()),
            ..Default::default()
        };
        config.subagents.enabled = true;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };
        config.prompt_template.file = None;

        let effective =
            build_effective_system_prompt_with_paths(&config, dir.path(), false).unwrap();
        let prompt = effective.prompt.unwrap_or_default();

        assert!(prompt.contains("<environment>"));
        assert!(prompt.contains("The current working directory is '"));
        assert!(prompt.contains("Current date:"));
        assert!(prompt.contains("`ZDX_HOME`: ZDX runtime home/config directory."));
        assert!(prompt.contains(
            "`ZDX_ARTIFACT_DIR`: Directory for artifacts generated for the current run/thread."
        ));
        assert!(prompt.contains("`ZDX_THREAD_ID`: Identifier for the current thread/session."));
        assert!(prompt.contains("Base prompt"));
        assert!(prompt.contains("<project-context>"));
        assert!(prompt.contains(
            "If an `AGENTS.md` or `CLAUDE.md` block mentions a relative file path, resolve it from the directory containing that context file"
        ));
        assert!(prompt.contains("Agent note"));
        assert!(prompt.contains("Available specialized capabilities"));
        assert!(prompt.contains("Task (`task`)"));
        assert!(prompt.contains("Oracle (`oracle`)"));
    }

    #[test]
    fn test_template_mode_lists_scoped_claude_context_path() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("CLAUDE.md"), "Nested Claude note").unwrap();

        let mut config = crate::config::Config {
            system_prompt: Some("Base prompt".to_string()),
            ..Default::default()
        };
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let effective =
            build_effective_system_prompt_with_paths(&config, dir.path(), false).unwrap();
        let prompt = effective.prompt.unwrap_or_default();

        assert!(prompt.contains("nested/CLAUDE.md"));
        assert!(!prompt.contains("nested/AGENTS.md"));
    }

    #[test]
    fn test_render_prompt_template_omits_memory_block_when_memory_index_empty() {
        let vars = build_prompt_template_vars(
            Path::new("/tmp"),
            "anthropic:claude-opus-4-6",
            PromptTemplateSections {
                base_prompt: None,
                project_context: None,
                memory_index: None,
                memory_suggestions: false,
                skills_list: &[],
                scoped_context: &[],
                specialized_capabilities: &[],
            },
        );

        let rendered = render_prompt_template(
            "{% if memory_index %}## Memory\n<memory_index>{{ memory_index }}</memory_index>{% endif %}",
            &vars,
        )
        .unwrap()
        .unwrap_or_default();

        assert!(!rendered.contains("<memory_index>"));
        assert!(!rendered.contains("## Memory"));
    }

    #[test]
    fn test_template_mode_includes_instruction_layers_when_provided() {
        let dir = tempdir().unwrap();

        let mut config = crate::config::Config {
            system_prompt: Some("Base prompt".to_string()),
            ..Default::default()
        };
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let instruction_layers = vec!["Telegram output rules"];
        let effective = build_effective_system_prompt_with_paths_and_instruction_layers(
            &config,
            dir.path(),
            &instruction_layers,
            false,
        )
        .unwrap();
        let prompt = effective.prompt.unwrap_or_default();

        assert!(prompt.contains("Telegram output rules"));
        assert!(prompt.contains("<instruction_layers>"));
    }

    #[test]
    fn test_template_mode_renders_custom_template_file() {
        let dir = tempdir().unwrap();
        let template_file = dir.path().join("template.md");
        fs::write(
            &template_file,
            "Prompt={{base_prompt}}\nRoot={{cwd}}\nDate={{date}}\nContext={{project_context}}",
        )
        .unwrap();
        fs::write(dir.path().join("AGENTS.md"), "Agent note").unwrap();

        let mut config = crate::config::Config {
            system_prompt: Some("Base prompt".to_string()),
            ..Default::default()
        };
        config.prompt_template.file = Some(template_file.display().to_string());
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let effective =
            build_effective_system_prompt_with_paths(&config, dir.path(), false).unwrap();
        let prompt = effective.prompt.unwrap_or_default();
        assert!(prompt.contains("Prompt=Base prompt"));
        assert!(prompt.contains(&format!("Root={}", dir.path().display())));
        assert!(prompt.contains("Date="));
        assert!(prompt.contains("Context=## "));
    }

    #[test]
    fn test_template_mode_falls_back_on_render_error() {
        let dir = tempdir().unwrap();
        let template_file = dir.path().join("template.md");
        fs::write(&template_file, "{{unknown_var}}").unwrap();

        let mut config = crate::config::Config {
            system_prompt: Some("Base prompt".to_string()),
            ..Default::default()
        };
        config.prompt_template.file = Some(template_file.display().to_string());
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let effective =
            build_effective_system_prompt_with_paths(&config, dir.path(), false).unwrap();
        let prompt = effective.prompt.unwrap_or_default();
        assert!(prompt.contains("<environment>"));
        assert!(prompt.contains("Base prompt"));
        assert!(effective.warnings.iter().any(|w| {
            w.message
                .contains("Failed to render system prompt template")
        }));
    }

    #[test]
    fn test_template_mode_falls_back_when_template_file_missing() {
        let dir = tempdir().unwrap();

        let mut config = crate::config::Config {
            system_prompt: Some("Base prompt".to_string()),
            ..Default::default()
        };
        config.prompt_template.file =
            Some(dir.path().join("missing-template.md").display().to_string());
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let effective =
            build_effective_system_prompt_with_paths(&config, dir.path(), false).unwrap();
        let prompt = effective.prompt.unwrap_or_default();
        assert!(prompt.contains("<environment>"));
        assert!(prompt.contains("Base prompt"));
        assert!(
            effective
                .warnings
                .iter()
                .any(|w| w.message.contains("Failed to read system prompt template"))
        );
    }

    #[test]
    fn test_delegation_capabilities_omit_task_and_oracle_when_subagents_disabled() {
        let dir = tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let effective =
            build_effective_system_prompt_with_paths(&config, dir.path(), false).unwrap();
        let prompt = effective.prompt.unwrap_or_default();
        assert!(!prompt.contains("Task (`task`)"));
        assert!(!prompt.contains("Oracle (`oracle`)"));
    }
}
