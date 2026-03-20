//! Named subagent discovery and parsing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::{Config, ThinkingLevel, paths};
use crate::core::context::{self, PromptContextInclusion};

/// Source location for a subagent definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentSource {
    /// Built into the binary.
    BuiltIn,
    /// `<ZDX_HOME>/subagents/*.md`
    User,
    /// `<root>/.zdx/subagents/*.md`
    Project,
}

impl SubagentSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "builtin",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// Parsed subagent definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentDefinition {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub source: SubagentSource,
    pub model: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub tools: Option<Vec<String>>,
    pub prompt_template: String,
}

/// Lightweight summary for listings and tool descriptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentSummary {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct SubagentFrontmatter {
    name: Option<String>,
    description: Option<String>,
    model: Option<String>,
    thinking_level: Option<ThinkingLevel>,
    tools: Option<Vec<String>>,
}

/// Discovers built-in, global, and project subagents.
///
/// Precedence is: project > user > built-in.
///
/// # Errors
/// Returns an error if parsing fails or files cannot be read.
pub fn discover(root: &Path) -> Result<Vec<SubagentDefinition>> {
    let mut by_name: BTreeMap<String, SubagentDefinition> = BTreeMap::new();

    for definition in built_in_definitions()? {
        by_name.insert(definition.name.clone(), definition);
    }

    let mut entries: Vec<(PathBuf, SubagentSource)> = Vec::new();
    collect_markdown_files(
        &paths::zdx_home().join("subagents"),
        SubagentSource::User,
        &mut entries,
    )?;
    collect_markdown_files(
        &root.join(".zdx").join("subagents"),
        SubagentSource::Project,
        &mut entries,
    )?;

    for (path, source) in entries {
        let definition = parse_subagent_file(&path, source)
            .with_context(|| format!("parse subagent {}", path.display()))?;
        by_name.insert(definition.name.clone(), definition);
    }

    Ok(by_name.into_values().collect())
}

/// Lists discovered subagent summaries.
///
/// # Errors
/// Returns an error if discovery fails.
pub fn list_summaries(root: &Path) -> Result<Vec<SubagentSummary>> {
    discover(root).map(|defs| {
        defs.into_iter()
            .map(|definition| SubagentSummary {
                name: definition.name,
                description: definition.description,
            })
            .collect()
    })
}

/// Loads a single subagent by name.
///
/// # Errors
/// Returns an error if the named subagent is missing or invalid.
pub fn load_by_name(root: &Path, name: &str) -> Result<SubagentDefinition> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("Subagent name cannot be empty");
    }

    discover(root)?
        .into_iter()
        .find(|definition| definition.name == trimmed)
        .ok_or_else(|| anyhow::anyhow!("Subagent '{trimmed}' not found"))
}

/// Renders a subagent prompt template with the same context/template pipeline used
/// for the main system prompt.
///
/// # Errors
/// Returns an error if rendering fails or produces an empty prompt.
pub fn render_prompt(
    config: &Config,
    root: &Path,
    definition: &SubagentDefinition,
    model: &str,
    surface_rules: Option<&str>,
    memory_suggestions: bool,
    inclusion: PromptContextInclusion,
) -> Result<String> {
    let effective = context::render_prompt_template_with_context(
        config,
        root,
        &definition.prompt_template,
        model,
        surface_rules,
        memory_suggestions,
        inclusion,
    )?;

    let prompt = effective.prompt.unwrap_or_default().trim().to_string();
    if prompt.is_empty() {
        bail!(
            "Subagent '{}' rendered an empty system prompt",
            definition.name
        );
    }

    Ok(prompt)
}

fn collect_markdown_files(
    dir: &Path,
    source: SubagentSource,
    out: &mut Vec<(PathBuf, SubagentSource)>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("read subagent dir {}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        })
        .collect();

    files.sort();
    for path in files {
        out.push((path, source));
    }
    Ok(())
}

fn built_in_definitions() -> Result<Vec<SubagentDefinition>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    [
        (
            manifest_dir.join("subagents").join("general_assistant.md"),
            include_str!("../subagents/general_assistant.md"),
        ),
        (
            manifest_dir
                .join("subagents")
                .join("automation_assistant.md"),
            include_str!("../subagents/automation_assistant.md"),
        ),
    ]
    .into_iter()
    .map(|(path, content)| parse_subagent_content(&path, SubagentSource::BuiltIn, content))
    .collect()
}

fn parse_subagent_file(path: &Path, source: SubagentSource) -> Result<SubagentDefinition> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read subagent file {}", path.display()))?;
    parse_subagent_content(path, source, &content)
}

fn parse_subagent_content(
    path: &Path,
    source: SubagentSource,
    content: &str,
) -> Result<SubagentDefinition> {
    let (yaml, body) = split_frontmatter(content)?;
    let frontmatter: SubagentFrontmatter = if yaml.trim().is_empty() {
        SubagentFrontmatter::default()
    } else {
        serde_yaml::from_str(&yaml)
            .with_context(|| format!("parse YAML frontmatter in {}", path.display()))?
    };

    let fallback_name = file_stem(path)?;
    let name = normalize_required_string(frontmatter.name, "name")?.unwrap_or(fallback_name);
    let description = normalize_required_string(frontmatter.description, "description")?
        .ok_or_else(|| anyhow::anyhow!("description is required"))?;
    let model = normalize_optional_string(frontmatter.model, "model")?;
    let tools = normalize_tools(frontmatter.tools)?;

    let prompt_template = body.trim().to_string();
    if prompt_template.is_empty() {
        bail!("Subagent prompt body cannot be empty");
    }

    Ok(SubagentDefinition {
        name,
        description,
        path: path.to_path_buf(),
        source,
        model,
        thinking_level: frontmatter.thinking_level,
        tools,
        prompt_template,
    })
}

fn file_stem(path: &Path) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Invalid subagent file name: {}", path.display()))?;
    Ok(stem.to_string())
}

fn normalize_required_string(value: Option<String>, field: &str) -> Result<Option<String>> {
    match value {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                bail!("{field} cannot be empty");
            }
            Ok(Some(trimmed.to_string()))
        }
        None => Ok(None),
    }
}

fn normalize_optional_string(value: Option<String>, field: &str) -> Result<Option<String>> {
    normalize_required_string(value, field)
}

fn normalize_tools(value: Option<Vec<String>>) -> Result<Option<Vec<String>>> {
    match value {
        Some(raw) => {
            let mut tools = Vec::with_capacity(raw.len());
            for tool in raw {
                let trimmed = tool.trim();
                if trimmed.is_empty() {
                    bail!("tools cannot contain empty names");
                }
                tools.push(trimmed.to_string());
            }
            if tools.is_empty() {
                bail!("tools cannot be empty when provided");
            }
            Ok(Some(tools))
        }
        None => Ok(None),
    }
}

fn split_frontmatter(content: &str) -> Result<(String, String)> {
    let content = content.trim_start_matches('\u{feff}');
    let lines: Vec<&str> = content.lines().collect();

    let Some(first) = lines.first() else {
        bail!("Missing YAML frontmatter");
    };

    if first.trim() != "---" {
        bail!("Missing YAML frontmatter");
    }

    for idx in 1..lines.len() {
        let trimmed = lines[idx].trim();
        if trimmed == "---" || trimmed == "..." {
            let yaml = lines[1..idx].join("\n");
            let body = lines[idx + 1..].join("\n");
            return Ok((yaml, body));
        }
    }

    bail!("Unterminated YAML frontmatter")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn discover_includes_built_ins() {
        let root = tempdir().unwrap();
        let all = discover(root.path()).unwrap();
        assert!(all.iter().any(|s| s.name == "general_assistant"));
        assert!(all.iter().any(|s| s.name == "automation_assistant"));
    }

    #[test]
    fn project_subagent_overrides_builtin() {
        let root = tempdir().unwrap();
        let project_dir = root.path().join(".zdx").join("subagents");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("general_assistant.md"),
            "---\ndescription: Project override\n---\nProject prompt",
        )
        .unwrap();

        let definition = load_by_name(root.path(), "general_assistant").unwrap();
        assert_eq!(definition.source, SubagentSource::Project);
        assert_eq!(definition.description, "Project override");
    }

    #[test]
    fn parse_subagent_requires_description() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("broken.md");
        fs::write(&file, "---\n---\nBody").unwrap();

        let err = parse_subagent_file(&file, SubagentSource::User).unwrap_err();
        assert!(err.to_string().contains("description is required"));
    }

    #[test]
    fn parse_subagent_accepts_tools_and_model() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("search.md");
        fs::write(
            &file,
            "---\ndescription: Search helper\nmodel: gemini:gemini-2.5-flash\nthinking_level: low\ntools:\n  - read\n  - grep\n---\nSearch prompt",
        )
        .unwrap();

        let definition = parse_subagent_file(&file, SubagentSource::User).unwrap();
        assert_eq!(definition.name, "search");
        assert_eq!(definition.model.as_deref(), Some("gemini:gemini-2.5-flash"));
        assert_eq!(definition.thinking_level, Some(ThinkingLevel::Low));
        assert_eq!(
            definition.tools,
            Some(vec!["read".to_string(), "grep".to_string()])
        );
    }
}
