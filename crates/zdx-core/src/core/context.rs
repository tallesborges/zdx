//! Context module for loading project-specific guidelines.
//!
//! AGENTS.md files are loaded hierarchically:
//! 1. `ZDX_HOME/AGENTS.md` (global user guidelines)
//! 2. ~/AGENTS.md (user home)
//! 3. Ancestor directories from home to project root
//! 4. Project root (--root or cwd)
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
use crate::skills::{LoadSkillsOptions, LoadSkillsResult, Skill, load_skills};
use crate::{prompts, subagents};

/// Sets `ZDX_ARTIFACT_DIR` and `ZDX_THREAD_ID` as process environment variables.
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

/// Maximum size for a single AGENTS.md file (64KB).
/// Files larger than this are truncated with a warning.
pub const MAX_AGENTS_FILE_SIZE: usize = 64 * 1024;

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

/// Result of loading AGENTS.md files.
#[derive(Debug, Clone)]
pub struct LoadedContext {
    /// Combined content from all AGENTS.md files.
    pub content: String,
    /// Paths of files that were loaded (in order).
    pub loaded_paths: Vec<PathBuf>,
    /// Warnings generated during loading (e.g., unreadable files, truncation).
    pub warnings: Vec<ContextWarning>,
}

/// A scoped AGENTS.md file discovered in a project subdirectory.
/// These are listed by path in the prompt (not inlined) — the agent reads on demand.
#[derive(Debug, Clone)]
pub struct ScopedContextFile {
    /// Absolute path to the AGENTS.md file.
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
struct PromptTemplateSubagents {
    available_models: Vec<String>,
    available_subagents: Vec<PromptTemplateNamedSubagent>,
}

#[derive(Debug, Clone, Serialize)]
struct PromptTemplateNamedSubagent {
    name: String,
    description: String,
}

#[derive(Debug, Clone, Serialize)]
struct PromptTemplateScopedContext {
    scope: String,
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
    memory_suggestions: bool,
    surface_rules: String,
    skills_list: Vec<PromptTemplateSkill>,
    scoped_context: Vec<PromptTemplateScopedContext>,
    subagents_config: Option<PromptTemplateSubagents>,
    cwd: String,
    date: String,
}

#[derive(Debug, Clone, Copy)]
struct PromptTemplateSections<'a> {
    base_prompt: Option<&'a str>,
    project_context: Option<&'a str>,
    memory_index: Option<&'a str>,
    memory_suggestions: bool,
    surface_rules: Option<&'a str>,
    skills_list: &'a [Skill],
    scoped_context: &'a [ScopedContextFile],
    subagents_enabled: bool,
    subagent_models: &'a [String],
}

fn combine_prompt_sections(
    base_prompt: Option<&str>,
    inline_project_context: Option<&str>,
    memory_index_block: Option<&str>,
    surface_rules_block: Option<&str>,
) -> Option<String> {
    let mut sections: Vec<&str> = Vec::new();
    for value in [
        base_prompt,
        inline_project_context,
        memory_index_block,
        surface_rules_block,
    ]
    .into_iter()
    .flatten()
    {
        let trimmed = value.trim();
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
    let base_prompt = sections.base_prompt.unwrap_or_default().trim().to_string();
    let project_context = sections
        .project_context
        .unwrap_or_default()
        .trim()
        .to_string();
    let memory_index = sections.memory_index.unwrap_or_default().trim().to_string();
    let surface_rules = sections
        .surface_rules
        .unwrap_or_default()
        .trim()
        .to_string();
    let skills_list = sections
        .skills_list
        .iter()
        .map(|skill| PromptTemplateSkill {
            name: skill.name.clone(),
            description: skill.description.clone(),
            path: skill.file_path.display().to_string(),
        })
        .collect();
    let available_subagents = if sections.subagents_enabled {
        subagents::list_summaries(root)
            .unwrap_or_default()
            .into_iter()
            .map(|subagent| PromptTemplateNamedSubagent {
                name: subagent.name,
                description: subagent.description,
            })
            .collect()
    } else {
        Vec::new()
    };
    let subagents_config = sections.subagents_enabled.then(|| PromptTemplateSubagents {
        available_models: sections.subagent_models.to_vec(),
        available_subagents,
    });
    let scoped_context = sections
        .scoped_context
        .iter()
        .map(|sa| PromptTemplateScopedContext {
            scope: sa.scope.clone(),
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
        memory_suggestions: sections.memory_suggestions,
        surface_rules,
        skills_list,
        scoped_context,
        subagents_config,
        cwd: root.display().to_string(),
        date: Utc::now().format("%Y-%m-%d").to_string(),
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

/// Maximum depth to walk when discovering scoped AGENTS.md files.
/// Keeps traversal fast in large repos.
const SCOPED_CONTEXT_MAX_DEPTH: usize = 4;

/// Maximum number of scoped AGENTS.md files to discover.
const SCOPED_CONTEXT_LIMIT: usize = 200;

/// Discovers AGENTS.md files in subdirectories of the project root.
///
/// Uses gitignore-aware walking to skip ignored directories (target/, .git/, etc.).
/// Limited to [`SCOPED_CONTEXT_MAX_DEPTH`] levels deep and [`SCOPED_CONTEXT_LIMIT`] files.
/// Returns files sorted by path for deterministic ordering.
/// The root AGENTS.md itself is excluded (it's handled as inline context).
pub fn discover_scoped_context(root: &Path) -> Vec<ScopedContextFile> {
    use ignore::WalkBuilder;

    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root_agents = canonical_root.join("AGENTS.md");

    let mut scoped: Vec<ScopedContextFile> = Vec::new();

    let walker = WalkBuilder::new(&canonical_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .max_depth(Some(SCOPED_CONTEXT_MAX_DEPTH))
        .build();

    for entry in walker.flatten() {
        if scoped.len() >= SCOPED_CONTEXT_LIMIT {
            break;
        }
        let path = entry.path();
        if path.file_name().is_none_or(|f| f != "AGENTS.md") || !path.is_file() {
            continue;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if canonical == root_agents {
            continue;
        }
        if let Ok(relative) = canonical.strip_prefix(&canonical_root)
            && let Some(scope_dir) = relative.parent()
        {
            let scope = scope_dir.display().to_string();
            scoped.push(ScopedContextFile {
                path: canonical,
                scope,
            });
        }
    }

    scoped.sort_by(|a, b| a.path.cmp(&b.path));
    scoped
}

/// Loads all AGENTS.md files from the collected paths.
///
/// Returns None if no files were found or all were empty.
/// Empty files are skipped silently.
/// Unreadable files generate a warning but don't fail.
/// Large files are truncated with a warning.
pub fn load_all_agents_files(root: &Path) -> Option<LoadedContext> {
    let paths = collect_agents_paths(root);
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
    /// The combined system prompt (config + AGENTS.md + optional memory index + template sections).
    pub prompt: Option<String>,
    /// Paths of AGENTS.md files that were inlined (in order).
    pub loaded_agents_paths: Vec<PathBuf>,
    /// Scoped AGENTS.md files discovered in subdirectories (listed as paths, not inlined).
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

/// Renders an arbitrary `MiniJinja` prompt template with the same variables and
/// context pipeline used by the built-in system prompt.
///
/// # Errors
/// Returns an error if the operation fails or the template cannot be rendered.
pub fn render_prompt_template_with_context(
    config: &Config,
    root: &Path,
    template: &str,
    model: &str,
    surface_rules: Option<&str>,
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

    let subagent_models = if config.subagents.enabled {
        config.subagent_available_models()
    } else {
        Vec::new()
    };

    let vars = build_prompt_template_vars(
        root,
        model,
        PromptTemplateSections {
            base_prompt: base_prompt.as_deref(),
            project_context: inline_project_context.as_deref(),
            memory_index: memory_index.as_deref(),
            memory_suggestions,
            surface_rules,
            skills_list: &skills,
            scoped_context: &scoped_context,
            subagents_enabled: config.subagents.enabled,
            subagent_models: &subagent_models,
        },
    );

    let prompt = render_prompt_template(template, &vars)
        .map_err(|error| anyhow::anyhow!("Failed to render prompt template: {error}"))?;

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

fn render_system_prompt_with_fallback(
    config: &Config,
    vars: &PromptTemplateVars,
    warnings: &mut Vec<ContextWarning>,
    base_prompt: Option<&str>,
    inline_project_context: Option<&str>,
    memory_index: Option<&str>,
    surface_rules: Option<&str>,
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
            warnings.push(ContextWarning {
                path: template_source.path.clone(),
                message: format!(
                    "Failed to render system prompt template: {error}; falling back to default template"
                ),
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
                        surface_rules,
                    )
                }
            }
        }
    }
}

/// Builds the effective system prompt by combining config, AGENTS.md files,
/// an optional memory index, and template-driven sections.
///
/// AGENTS.md files are loaded hierarchically from:
/// 1. `ZDX_HOME/AGENTS.md`
/// 2. ~/AGENTS.md  
/// 3. Ancestor directories from home to project root
/// 4. Project root
///
/// Returns the combined prompt, the list of loaded AGENTS.md paths, and any warnings.
/// This function is UI-agnostic; callers should surface warnings via the renderer.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn build_effective_system_prompt_with_paths(
    config: &Config,
    root: &Path,
    memory_suggestions: bool,
) -> Result<EffectivePrompt> {
    build_effective_system_prompt_with_paths_and_surface_rules(
        config,
        root,
        None,
        memory_suggestions,
    )
}

/// Builds the effective system prompt by combining config, AGENTS.md files,
/// an optional memory index, template-driven sections, and optional
/// surface-specific output rules (e.g., Telegram formatting constraints).
///
/// # Errors
/// Returns an error if the operation fails.
pub fn build_effective_system_prompt_with_paths_and_surface_rules(
    config: &Config,
    root: &Path,
    surface_rules: Option<&str>,
    memory_suggestions: bool,
) -> Result<EffectivePrompt> {
    let base_prompt = config.effective_system_prompt()?;
    let PromptContextSectionsResult {
        loaded_agents_paths,
        scoped_context,
        mut warnings,
        inline_project_context,
        memory_index,
    } = load_prompt_context_sections(root, config);

    let skills_result = load_skills_with_config(config, root);
    let LoadSkillsResult {
        skills,
        warnings: skill_warnings,
    } = skills_result;

    let subagent_models = if config.subagents.enabled {
        config.subagent_available_models()
    } else {
        Vec::new()
    };

    let vars = build_prompt_template_vars(
        root,
        &config.model,
        PromptTemplateSections {
            base_prompt: base_prompt.as_deref(),
            project_context: inline_project_context.as_deref(),
            memory_index: memory_index.as_deref(),
            memory_suggestions,
            surface_rules,
            skills_list: &skills,
            scoped_context: &scoped_context,
            subagents_enabled: config.subagents.enabled,
            subagent_models: &subagent_models,
        },
    );

    let system_prompt = render_system_prompt_with_fallback(
        config,
        &vars,
        &mut warnings,
        base_prompt.as_deref(),
        inline_project_context.as_deref(),
        memory_index.as_deref(),
        surface_rules,
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

    let scoped_context_paths = scoped_context.iter().map(|sa| sa.path.clone()).collect();
    let loaded_skills = skills;

    Ok(EffectivePrompt {
        prompt: system_prompt,
        loaded_agents_paths,
        scoped_context_paths,
        warnings,
        loaded_skills,
    })
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
                surface_rules: None,
                skills_list: &[],
                scoped_context: &[],
                subagents_enabled: false,
                subagent_models: &[],
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
                surface_rules: None,
                skills_list: &[],
                scoped_context: &[],
                subagents_enabled: false,
                subagent_models: &[],
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
                surface_rules: None,
                skills_list: &[],
                scoped_context: &[],
                subagents_enabled: false,
                subagent_models: &[],
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
    fn test_render_prompt_template_supports_structured_skills_and_subagents() {
        let skills = vec![Skill {
            name: "demo-skill".to_string(),
            description: "Use <special> syntax".to_string(),
            file_path: PathBuf::from("/tmp/demo&skill/SKILL.md"),
            base_dir: PathBuf::from("/tmp/demo&skill"),
            source: SkillSource::ZdxUser,
        }];
        let models = vec!["codex:gpt-5.3-codex".to_string()];

        let vars = build_prompt_template_vars(
            Path::new("/tmp"),
            "codex:gpt-5.3-codex",
            PromptTemplateSections {
                base_prompt: Some("hello"),
                project_context: None,
                memory_index: None,
                memory_suggestions: false,
                surface_rules: None,
                skills_list: &skills,
                scoped_context: &[],
                subagents_enabled: true,
                subagent_models: &models,
            },
        );

        let rendered = render_prompt_template(
            "{% for skill in skills_list %}<name>{{ skill.name }}</name><description>{{ skill.description }}</description><path>{{ skill.path }}</path>{% endfor %}\n{% if subagents_config %}Available named subagents: {% for subagent in subagents_config.available_subagents %}{{ subagent.name }}{% endfor %}\nAvailable model overrides: {% for model in subagents_config.available_models %}{{ model }}{% endfor %}{% endif %}",
            &vars,
        )
        .unwrap()
        .unwrap();

        assert!(rendered.contains("<name>demo-skill</name>"));
        assert!(rendered.contains("Use <special> syntax"));
        assert!(rendered.contains("demo&skill"));
        assert!(rendered.contains("general_assistant"));
        assert!(rendered.contains("codex:gpt-5.3-codex"));
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
                surface_rules: None,
                skills_list: &[],
                scoped_context: &[],
                subagents_enabled: false,
                subagent_models: &[],
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
                surface_rules: None,
                skills_list: &[],
                scoped_context: &[],
                subagents_enabled: false,
                subagent_models: &[],
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
        assert!(prompt.contains("Current directory:"));
        assert!(prompt.contains("Current date:"));
        assert!(prompt.contains("`ZDX_HOME`: ZDX runtime home/config directory."));
        assert!(prompt.contains(
            "`ZDX_ARTIFACT_DIR`: Directory for artifacts generated for the current run/thread."
        ));
        assert!(prompt.contains("`ZDX_THREAD_ID`: Identifier for the current thread/session."));
        assert!(prompt.contains("Base prompt"));
        assert!(prompt.contains("<project-context>"));
        assert!(prompt.contains("Agent note"));
        assert!(prompt.contains("Available model overrides"));
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
                surface_rules: None,
                skills_list: &[],
                scoped_context: &[],
                subagents_enabled: false,
                subagent_models: &[],
            },
        );

        let rendered = render_prompt_template(
            "{% if memory_index %}## Memory\n<memory>{{ memory_index }}</memory>{% endif %}",
            &vars,
        )
        .unwrap()
        .unwrap_or_default();

        assert!(!rendered.contains("<memory>"));
        assert!(!rendered.contains("## Memory"));
    }

    #[test]
    fn test_template_mode_includes_surface_rules_when_provided() {
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

        let effective = build_effective_system_prompt_with_paths_and_surface_rules(
            &config,
            dir.path(),
            Some("Telegram output rules"),
            false,
        )
        .unwrap();
        let prompt = effective.prompt.unwrap_or_default();

        assert!(prompt.contains("<surface_rules>"));
        assert!(prompt.contains("Telegram output rules"));
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
    fn test_subagents_block_not_appended_when_disabled() {
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
        assert!(!prompt.contains("Available model overrides"));
    }
}
