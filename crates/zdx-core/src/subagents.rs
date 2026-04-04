//! Named subagent discovery and parsing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::{ThinkingLevel, paths};
use crate::core::context::{
    LoadedSkillContent, PromptContextInclusion, StandalonePromptSkillContext,
};
use crate::skills::{LoadSkillsOptions, Skill, load_skills, read_skill_content, skill_access_path};

pub const TASK_BUILTIN_ALIAS_NAME: &str = "task";
pub const FINDER_SUBAGENT_NAME: &str = "finder";
pub const LIBRARIAN_SUBAGENT_NAME: &str = "librarian";
pub const DESIGNER_SUBAGENT_NAME: &str = "designer";
pub const ORACLE_SUBAGENT_NAME: &str = "oracle";

/// Reserved runtime aliases that are not backed by a markdown subagent file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinAlias {
    /// Default delegated ZDX behavior using the base prompt + context pipeline.
    Task,
}

impl BuiltinAlias {
    #[must_use]
    pub const fn runtime_name(self) -> &'static str {
        match self {
            Self::Task => TASK_BUILTIN_ALIAS_NAME,
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Task => "Task",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Task => {
                "Delegate an independent sub-task using the default full ZDX prompt and project context. Use it for complex multi-step work, output-heavy subtasks, or parallelizable implementation slices, and prefer direct execution when the work is small enough to do yourself."
            }
        }
    }
}

#[must_use]
pub fn builtin_alias_from_name(name: &str) -> Option<BuiltinAlias> {
    match name.trim() {
        name if name.eq_ignore_ascii_case(TASK_BUILTIN_ALIAS_NAME) => Some(BuiltinAlias::Task),
        _ => None,
    }
}

#[must_use]
pub fn is_reserved_runtime_alias(name: &str) -> bool {
    builtin_alias_from_name(name).is_some()
}

/// Curated capability metadata surfaced in the main system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    pub name: String,
    pub title: String,
    pub description: String,
    pub kind: CapabilityKind,
}

/// Internal implementation backing a curated capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityKind {
    /// Backed by a named standalone subagent prompt.
    Subagent { subagent: String },
    /// Backed by a reserved runtime alias.
    BuiltinAlias(BuiltinAlias),
}

/// Runtime resolution for `subagent:` inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeSubagentSelection {
    /// Use the default delegated ZDX prompt/context behavior.
    Default,
    /// Use a named standalone subagent prompt.
    Named(SubagentDefinition),
}

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
    pub skills: Option<Vec<String>>,
    pub auto_loaded_skills: Option<Vec<String>>,
    /// Standalone prompt body used as the child subagent system prompt.
    pub prompt_body: String,
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
    skills: Option<Vec<String>>,
    auto_loaded_skills: Option<Vec<String>>,
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
            .filter(|definition| !is_reserved_runtime_alias(&definition.name))
            .map(|definition| SubagentSummary {
                name: definition.name,
                description: definition.description,
            })
            .collect()
    })
}

/// Resolves a runtime `subagent` selection, including reserved aliases.
///
/// # Errors
/// Returns an error if a named subagent is requested but missing or invalid.
pub fn resolve_runtime_selection(
    root: &Path,
    requested: Option<&str>,
) -> Result<RuntimeSubagentSelection> {
    match requested.map(str::trim).filter(|name| !name.is_empty()) {
        None => Ok(RuntimeSubagentSelection::Default),
        Some(name) if builtin_alias_from_name(name) == Some(BuiltinAlias::Task) => {
            Ok(RuntimeSubagentSelection::Default)
        }
        Some(name) => load_by_name(root, name).map(RuntimeSubagentSelection::Named),
    }
}

/// Builds the curated specialized capability catalog for the main system prompt.
///
/// # Errors
/// Returns an error if a required named subagent capability cannot be resolved.
pub fn capability_catalog(
    root: &Path,
    delegation_enabled: bool,
) -> Result<Vec<CapabilityDescriptor>> {
    let mut capabilities = Vec::new();

    if delegation_enabled {
        capabilities.push(task_capability());
        capabilities.push(finder_capability(root)?);
        capabilities.push(librarian_capability(root)?);
        capabilities.push(designer_capability(root)?);
        capabilities.push(oracle_capability(root)?);
    }

    Ok(capabilities)
}

/// Returns a built-in fallback specialized capability catalog.
#[must_use]
pub fn fallback_capability_catalog(delegation_enabled: bool) -> Vec<CapabilityDescriptor> {
    let mut capabilities = Vec::new();

    if delegation_enabled {
        capabilities.push(task_capability());
        capabilities.push(fallback_finder_capability());
        capabilities.push(fallback_librarian_capability());
        capabilities.push(fallback_designer_capability());
        capabilities.push(fallback_oracle_capability());
    }

    capabilities
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

/// Renders a subagent prompt body as the standalone system prompt for the child run.
///
/// # Errors
/// Returns an error if rendering fails or produces an empty prompt.
pub fn render_prompt(
    config: &crate::config::Config,
    root: &Path,
    definition: &SubagentDefinition,
    model: &str,
    inclusion: PromptContextInclusion,
) -> Result<String> {
    let mut inclusion = inclusion;
    inclusion.skills = false;

    let subagent_skills = resolve_subagent_skills(config, root, definition)?;

    crate::core::context::render_standalone_prompt_template(
        config,
        root,
        model,
        &definition.prompt_body,
        false,
        inclusion,
        &StandalonePromptSkillContext {
            available_skills: subagent_skills.allowed,
            auto_loaded_skill_contents: subagent_skills.auto_loaded,
        },
    )
    .with_context(|| format!("render subagent '{}'", definition.name))
}

#[derive(Debug, Default)]
struct ResolvedSubagentSkills {
    allowed: Vec<Skill>,
    auto_loaded: Vec<LoadedSkillContent>,
}

fn resolve_subagent_skills(
    config: &crate::config::Config,
    root: &Path,
    definition: &SubagentDefinition,
) -> Result<ResolvedSubagentSkills> {
    let allowed_names = definition
        .skills
        .clone()
        .or_else(|| definition.auto_loaded_skills.clone())
        .unwrap_or_default();
    let auto_loaded_names = definition.auto_loaded_skills.clone().unwrap_or_default();

    if allowed_names.is_empty() && auto_loaded_names.is_empty() {
        return Ok(ResolvedSubagentSkills::default());
    }

    let mut options = LoadSkillsOptions::new(root);
    options.sources = config.skills.sources.clone();

    let loaded = load_skills(&options);
    let mut by_name = BTreeMap::new();
    for skill in loaded.skills {
        by_name.insert(skill.name.clone(), skill);
    }

    let allowed = resolve_named_skills(&by_name, &allowed_names, definition, "skills")?;
    let auto_loaded_skills = resolve_named_skills(
        &by_name,
        &auto_loaded_names,
        definition,
        "auto_loaded_skills",
    )?;
    let auto_loaded = load_auto_loaded_skill_contents(&auto_loaded_skills)?;

    Ok(ResolvedSubagentSkills {
        allowed,
        auto_loaded,
    })
}

fn resolve_named_skills(
    available: &BTreeMap<String, Skill>,
    names: &[String],
    definition: &SubagentDefinition,
    field_name: &str,
) -> Result<Vec<Skill>> {
    names
        .iter()
        .map(|name| {
            available.get(name).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Subagent '{}' references unknown skill '{}' in {}",
                    definition.name,
                    name,
                    field_name
                )
            })
        })
        .collect()
}

fn load_auto_loaded_skill_contents(skills: &[Skill]) -> Result<Vec<LoadedSkillContent>> {
    skills
        .iter()
        .map(|skill| {
            let content = read_skill_content(skill)
                .with_context(|| format!("read auto-loaded skill {}", skill.file_path.display()))?;
            Ok(LoadedSkillContent {
                name: skill.name.clone(),
                description: skill.description.clone(),
                path: skill_access_path(skill),
                content: content.trim().to_string(),
            })
        })
        .collect()
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
            manifest_dir.join("subagents").join("finder.md"),
            include_str!("../subagents/finder.md"),
        ),
        (
            manifest_dir.join("subagents").join("librarian.md"),
            include_str!("../subagents/librarian.md"),
        ),
        (
            manifest_dir.join("subagents").join("designer.md"),
            include_str!("../subagents/designer.md"),
        ),
        (
            manifest_dir.join("subagents").join("oracle.md"),
            include_str!("../subagents/oracle.md"),
        ),
    ]
    .into_iter()
    .map(|(path, content)| parse_subagent_content(&path, SubagentSource::BuiltIn, content))
    .collect()
}

fn task_capability() -> CapabilityDescriptor {
    CapabilityDescriptor {
        name: BuiltinAlias::Task.runtime_name().to_string(),
        title: BuiltinAlias::Task.display_name().to_string(),
        description: BuiltinAlias::Task.description().to_string(),
        kind: CapabilityKind::BuiltinAlias(BuiltinAlias::Task),
    }
}

fn oracle_capability(root: &Path) -> Result<CapabilityDescriptor> {
    let definition = load_by_name(root, ORACLE_SUBAGENT_NAME)?;
    Ok(CapabilityDescriptor {
        name: definition.name.clone(),
        title: "Oracle".to_string(),
        description: definition.description,
        kind: CapabilityKind::Subagent {
            subagent: definition.name,
        },
    })
}

fn finder_capability(root: &Path) -> Result<CapabilityDescriptor> {
    let definition = load_by_name(root, FINDER_SUBAGENT_NAME)?;
    Ok(CapabilityDescriptor {
        name: definition.name.clone(),
        title: "Finder".to_string(),
        description: definition.description,
        kind: CapabilityKind::Subagent {
            subagent: definition.name,
        },
    })
}

fn librarian_capability(root: &Path) -> Result<CapabilityDescriptor> {
    let definition = load_by_name(root, LIBRARIAN_SUBAGENT_NAME)?;
    Ok(CapabilityDescriptor {
        name: definition.name.clone(),
        title: "Librarian".to_string(),
        description: definition.description,
        kind: CapabilityKind::Subagent {
            subagent: definition.name,
        },
    })
}

fn designer_capability(root: &Path) -> Result<CapabilityDescriptor> {
    let definition = load_by_name(root, DESIGNER_SUBAGENT_NAME)?;
    Ok(CapabilityDescriptor {
        name: definition.name.clone(),
        title: "Designer".to_string(),
        description: definition.description,
        kind: CapabilityKind::Subagent {
            subagent: definition.name,
        },
    })
}

fn fallback_finder_capability() -> CapabilityDescriptor {
    CapabilityDescriptor {
        name: FINDER_SUBAGENT_NAME.to_string(),
        title: "Finder".to_string(),
        description:
            "Use for read-only local code and thread discovery: complex multi-step search across the current workspace, other machine-local paths, and saved thread history."
                .to_string(),
        kind: CapabilityKind::Subagent {
            subagent: FINDER_SUBAGENT_NAME.to_string(),
        },
    }
}

fn fallback_librarian_capability() -> CapabilityDescriptor {
    CapabilityDescriptor {
        name: LIBRARIAN_SUBAGENT_NAME.to_string(),
        title: "Librarian".to_string(),
        description:
            "Use for remote repository and external reference research: GitHub/Bitbucket codebases, cross-repo architecture, commit history, and detailed explanatory answers."
                .to_string(),
        kind: CapabilityKind::Subagent {
            subagent: LIBRARIAN_SUBAGENT_NAME.to_string(),
        },
    }
}

fn fallback_designer_capability() -> CapabilityDescriptor {
    CapabilityDescriptor {
        name: DESIGNER_SUBAGENT_NAME.to_string(),
        title: "Designer".to_string(),
        description:
            "Use for UI/UX implementation, design review, accessibility refinement, and visual polish in existing product surfaces."
                .to_string(),
        kind: CapabilityKind::Subagent {
            subagent: DESIGNER_SUBAGENT_NAME.to_string(),
        },
    }
}

fn fallback_oracle_capability() -> CapabilityDescriptor {
    CapabilityDescriptor {
        name: ORACLE_SUBAGENT_NAME.to_string(),
        title: "Oracle".to_string(),
        description:
            "Read-only deep reasoning advisor for code review, difficult debugging, planning, and architecture decisions."
                .to_string(),
        kind: CapabilityKind::Subagent {
            subagent: ORACLE_SUBAGENT_NAME.to_string(),
        },
    }
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
    if is_reserved_runtime_alias(&name) {
        bail!("Subagent name '{name}' is reserved for runtime aliases and cannot be used");
    }
    let description = normalize_required_string(frontmatter.description, "description")?
        .ok_or_else(|| anyhow::anyhow!("description is required"))?;
    let model = normalize_optional_string(frontmatter.model, "model")?;
    let tools = normalize_tools(frontmatter.tools)?;
    let skills = normalize_named_items(frontmatter.skills, "skills")?;
    let auto_loaded_skills =
        normalize_named_items(frontmatter.auto_loaded_skills, "auto_loaded_skills")?;
    validate_auto_loaded_skills(skills.as_deref(), auto_loaded_skills.as_deref())?;

    let prompt_body = body.trim().to_string();

    Ok(SubagentDefinition {
        name,
        description,
        path: path.to_path_buf(),
        source,
        model,
        thinking_level: frontmatter.thinking_level,
        tools,
        skills,
        auto_loaded_skills,
        prompt_body,
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
            validate_tool_names(&tools)?;
            Ok(Some(tools))
        }
        None => Ok(None),
    }
}

fn normalize_named_items(value: Option<Vec<String>>, field: &str) -> Result<Option<Vec<String>>> {
    match value {
        Some(raw) => {
            let mut values = Vec::with_capacity(raw.len());
            let mut seen = BTreeSet::new();
            for item in raw {
                let trimmed = item.trim();
                if trimmed.is_empty() {
                    bail!("{field} cannot contain empty names");
                }
                if seen.insert(trimmed.to_string()) {
                    values.push(trimmed.to_string());
                }
            }
            if values.is_empty() {
                bail!("{field} cannot be empty when provided");
            }
            Ok(Some(values))
        }
        None => Ok(None),
    }
}

fn validate_auto_loaded_skills(
    skills: Option<&[String]>,
    auto_loaded_skills: Option<&[String]>,
) -> Result<()> {
    let Some(auto_loaded_skills) = auto_loaded_skills else {
        return Ok(());
    };
    let Some(skills) = skills else {
        return Ok(());
    };

    let allowed: BTreeSet<&String> = skills.iter().collect();
    let invalid: Vec<&String> = auto_loaded_skills
        .iter()
        .filter(|name| !allowed.contains(*name))
        .collect();

    if invalid.is_empty() {
        return Ok(());
    }

    bail!(
        "auto_loaded_skills must be a subset of skills; unexpected values: {}",
        invalid
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn validate_tool_names(tools: &[String]) -> Result<()> {
    let available = crate::tools::all_tool_names();
    let available_set: std::collections::BTreeSet<String> = available
        .iter()
        .map(|tool| tool.to_ascii_lowercase())
        .collect();
    let mut unknown: Vec<String> = tools
        .iter()
        .filter(|tool| !available_set.contains(&tool.to_ascii_lowercase()))
        .cloned()
        .collect();

    if unknown.is_empty() {
        return Ok(());
    }

    unknown.sort();
    let mut available = available;
    available.sort();
    bail!(
        "Unknown tool(s): {}. Available tools: {}",
        unknown.join(", "),
        available.join(", ")
    );
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
        assert!(all.iter().any(|s| s.name == "finder"));
        assert!(all.iter().any(|s| s.name == "librarian"));
        assert!(all.iter().any(|s| s.name == "designer"));
        assert!(all.iter().any(|s| s.name == "oracle"));
    }

    #[test]
    fn project_subagent_overrides_builtin() {
        let root = tempdir().unwrap();
        let project_dir = root.path().join(".zdx").join("subagents");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("oracle.md"),
            "---\ndescription: Project override\n---\nProject prompt",
        )
        .unwrap();

        let definition = load_by_name(root.path(), "oracle").unwrap();
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
        assert_eq!(definition.prompt_body, "Search prompt");
    }

    #[test]
    fn parse_subagent_accepts_skills_and_auto_loaded_skills() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("research.md");
        fs::write(
            &file,
            "---\ndescription: Research helper\nskills:\n  - deepwiki-cli\n  - memory\nauto_loaded_skills:\n  - deepwiki-cli\n---\nPrompt",
        )
        .unwrap();

        let definition = parse_subagent_file(&file, SubagentSource::User).unwrap();
        assert_eq!(
            definition.skills,
            Some(vec!["deepwiki-cli".to_string(), "memory".to_string()])
        );
        assert_eq!(
            definition.auto_loaded_skills,
            Some(vec!["deepwiki-cli".to_string()])
        );
    }

    #[test]
    fn parse_subagent_rejects_auto_loaded_skills_outside_allowed_skills() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("research.md");
        fs::write(
            &file,
            "---\ndescription: Research helper\nskills:\n  - memory\nauto_loaded_skills:\n  - deepwiki-cli\n---\nPrompt",
        )
        .unwrap();

        let err = parse_subagent_file(&file, SubagentSource::User).unwrap_err();
        assert!(
            err.to_string()
                .contains("auto_loaded_skills must be a subset of skills")
        );
    }

    #[test]
    fn parse_subagent_allows_empty_prompt_body() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("general.md");
        fs::write(&file, "---\ndescription: General alias\n---\n").unwrap();

        let definition = parse_subagent_file(&file, SubagentSource::User).unwrap();
        assert!(definition.prompt_body.is_empty());
    }

    #[test]
    fn parse_subagent_rejects_reserved_runtime_alias_name() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("task.md");
        fs::write(&file, "---\ndescription: Reserved\n---\nPrompt").unwrap();

        let err = parse_subagent_file(&file, SubagentSource::User).unwrap_err();
        assert!(err.to_string().contains("reserved for runtime aliases"));
    }

    #[test]
    fn parse_subagent_rejects_unknown_tool_names() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("bad-tools.md");
        fs::write(
            &file,
            "---\ndescription: Bad tools\ntools:\n  - reed\n---\nPrompt",
        )
        .unwrap();

        let err = parse_subagent_file(&file, SubagentSource::User).unwrap_err();
        assert!(err.to_string().contains("Unknown tool(s): reed"));
    }

    #[test]
    fn resolve_runtime_selection_treats_task_alias_as_default() {
        let root = tempdir().unwrap();

        let selection = resolve_runtime_selection(root.path(), Some("task")).unwrap();
        assert_eq!(selection, RuntimeSubagentSelection::Default);
    }

    #[test]
    fn discover_rejects_reserved_runtime_alias_files() {
        let root = tempdir().unwrap();
        let project_dir = root.path().join(".zdx").join("subagents");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("task.md"),
            "---\ndescription: User-defined task file\n---\nPrompt",
        )
        .unwrap();

        let err = discover(root.path()).unwrap_err();
        assert!(format!("{err:#}").contains("reserved for runtime aliases"));
    }

    #[test]
    fn capability_catalog_includes_curated_entries() {
        let root = tempdir().unwrap();
        let capabilities = capability_catalog(root.path(), true).unwrap();

        assert_eq!(
            capabilities
                .iter()
                .map(|cap| cap.name.as_str())
                .collect::<Vec<_>>(),
            vec!["task", "finder", "librarian", "designer", "oracle"]
        );
    }

    #[test]
    fn render_prompt_supports_template_vars() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("AGENTS.md"), "Project note").unwrap();

        let definition = SubagentDefinition {
            name: "templated".to_string(),
            description: "Templated subagent".to_string(),
            path: root.path().join("templated.md"),
            source: SubagentSource::User,
            model: None,
            thinking_level: None,
            tools: None,
            skills: None,
            auto_loaded_skills: None,
            prompt_body: "{% if project_context %}{{ project_context }}{% endif %}\n{{ cwd }}"
                .to_string(),
        };

        let mut config = crate::config::Config::default();
        config.skills.sources.zdx_user = false;
        config.skills.sources.codex_user = false;
        config.skills.sources.claude_user = false;
        config.skills.sources.claude_project = false;
        config.skills.sources.agents_user = false;
        config.skills.sources.agents_project = false;

        let rendered = render_prompt(
            &config,
            root.path(),
            &definition,
            "anthropic:claude-opus-4-6",
            PromptContextInclusion::default(),
        )
        .unwrap();

        assert!(rendered.contains("Project note"));
        assert!(rendered.contains(&root.path().display().to_string()));
    }

    #[test]
    fn render_prompt_includes_available_and_auto_loaded_skills() {
        let root = tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.skills.sources.zdx_user = false;
        config.skills.sources.codex_user = false;
        config.skills.sources.claude_user = false;
        config.skills.sources.claude_project = false;
        config.skills.sources.agents_user = false;
        config.skills.sources.agents_project = false;

        let definition = SubagentDefinition {
            name: "librarian".to_string(),
            description: "Librarian".to_string(),
            path: root.path().join("librarian.md"),
            source: SubagentSource::User,
            model: None,
            thinking_level: None,
            tools: Some(vec!["read".to_string()]),
            skills: Some(vec!["deepwiki-cli".to_string(), "memory".to_string()]),
            auto_loaded_skills: Some(vec!["deepwiki-cli".to_string()]),
            prompt_body: "{% if available_skills %}## Available Skills\n{% for skill in available_skills %}- {{ skill.name }} => {{ skill.path }}\n{% endfor %}{% endif %}\n{% if auto_loaded_skill_contents %}## Auto-loaded Skills\n{% for skill in auto_loaded_skill_contents %}Path: {{ skill.path }}\n{{ skill.content }}\n{% endfor %}{% endif %}".to_string(),
        };

        let rendered = render_prompt(
            &config,
            root.path(),
            &definition,
            "anthropic:claude-opus-4-6",
            PromptContextInclusion::default(),
        )
        .unwrap();

        assert!(rendered.contains("## Available Skills"));
        assert!(rendered.contains("deepwiki-cli"));
        assert!(rendered.contains("memory"));
        assert!(rendered.contains("## Auto-loaded Skills"));
        assert!(rendered.contains("${ZDX_HOME}/bundled-skills/deepwiki-cli/SKILL.md"));
        assert!(rendered.contains("# DeepWiki"));
        assert!(!rendered.contains("# Memory\nUse this skill for memory-related tasks."));
    }
}
