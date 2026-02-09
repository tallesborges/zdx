//! Skills discovery and parsing.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{fmt, fs};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::config::{SkillSourceToggles, Toggle, paths};

/// Skill source location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    ZdxUser,
    ZdxProject,
    CodexUser,
    ClaudeUser,
    ClaudeProject,
    AgentsUser,
    AgentsProject,
}

impl fmt::Display for SkillSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl SkillSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillSource::ZdxUser => "zdx-user",
            SkillSource::ZdxProject => "zdx-project",
            SkillSource::CodexUser => "codex-user",
            SkillSource::ClaudeUser => "claude-user",
            SkillSource::ClaudeProject => "claude-project",
            SkillSource::AgentsUser => "agents-user",
            SkillSource::AgentsProject => "agents-project",
        }
    }
}

/// Discovered skill metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
    pub base_dir: PathBuf,
    pub source: SkillSource,
}

/// Warning generated during skill loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillWarning {
    pub skill_path: PathBuf,
    pub message: String,
}

impl SkillWarning {
    fn new(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            skill_path: path.into(),
            message: message.into(),
        }
    }
}

/// Result of loading skills.
#[derive(Debug, Clone, Default)]
pub struct LoadSkillsResult {
    pub skills: Vec<Skill>,
    pub warnings: Vec<SkillWarning>,
}

/// Options for skill discovery.
#[derive(Debug, Clone)]
pub struct LoadSkillsOptions {
    pub cwd: PathBuf,
    pub sources: SkillSourceToggles,
    pub ignored_skills: Vec<String>,
    pub include_skills: Vec<String>,
}

impl LoadSkillsOptions {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            sources: SkillSourceToggles {
                zdx_user: Toggle::On,
                zdx_project: Toggle::On,
                codex_user: Toggle::On,
                claude_user: Toggle::On,
                claude_project: Toggle::On,
                agents_user: Toggle::On,
                agents_project: Toggle::On,
            },
            ignored_skills: Vec::new(),
            include_skills: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SkillDirFormat {
    Recursive,
    ClaudeOneLevel,
}

#[derive(Debug, Clone)]
struct SkillSourceSpec {
    source: SkillSource,
    base_dir: PathBuf,
    format: SkillDirFormat,
}

impl SkillSourceSpec {
    fn new(source: SkillSource, base_dir: PathBuf, format: SkillDirFormat) -> Self {
        Self {
            source,
            base_dir,
            format,
        }
    }

    fn recursive(source: SkillSource, base_dir: PathBuf) -> Self {
        Self::new(source, base_dir, SkillDirFormat::Recursive)
    }

    fn claude(source: SkillSource, base_dir: PathBuf) -> Self {
        Self::new(source, base_dir, SkillDirFormat::ClaudeOneLevel)
    }
}

#[derive(Debug, Default)]
struct LoadState {
    skills: Vec<Skill>,
    warnings: Vec<SkillWarning>,
    seen_names: HashSet<String>,
    seen_paths: HashSet<PathBuf>,
    seen_dirs: HashSet<PathBuf>,
    filters: SkillFilters,
}

impl LoadState {
    fn new(filters: SkillFilters, warnings: Vec<SkillWarning>) -> Self {
        Self {
            skills: Vec::new(),
            warnings,
            seen_names: HashSet::new(),
            seen_paths: HashSet::new(),
            seen_dirs: HashSet::new(),
            filters,
        }
    }

    fn warn(&mut self, path: &Path, message: impl Into<String>) {
        self.warnings.push(SkillWarning::new(path, message));
    }
}

#[derive(Debug, Clone, Default)]
struct SkillFilters {
    include: Option<GlobSet>,
    ignore: Option<GlobSet>,
}

impl SkillFilters {
    fn should_include(&self, name: &str) -> bool {
        if let Some(ignore) = &self.ignore
            && ignore.is_match(name)
        {
            return false;
        }

        if let Some(include) = &self.include {
            return include.is_match(name);
        }

        true
    }
}

/// Loads skills from all enabled sources.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn load_skills(options: &LoadSkillsOptions) -> LoadSkillsResult {
    let zdx_home = paths::zdx_home();
    let home_dir = dirs::home_dir();
    let sources = build_skill_sources(options, &zdx_home, home_dir.as_deref());
    let mut warnings = Vec::new();
    let filters = build_skill_filters(
        &options.include_skills,
        &options.ignored_skills,
        &options.cwd,
        &mut warnings,
    );
    load_skills_from_sources_with_filters(sources, filters, warnings)
}

/// Loads skills from a single directory using recursive discovery.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn load_skills_from_dir(dir: &Path, source: SkillSource) -> LoadSkillsResult {
    let mut state = LoadState::new(SkillFilters::default(), Vec::new());
    load_skills_from_dir_with_format(dir, source, SkillDirFormat::Recursive, &mut state);

    LoadSkillsResult {
        skills: state.skills,
        warnings: state.warnings,
    }
}

fn build_skill_sources(
    options: &LoadSkillsOptions,
    zdx_home: &Path,
    home_dir: Option<&Path>,
) -> Vec<SkillSourceSpec> {
    let mut sources = Vec::new();

    if options.sources.zdx_user.is_on() {
        if let Some(home) = home_dir {
            sources.push(SkillSourceSpec::recursive(
                SkillSource::ZdxUser,
                home.join(".zdx").join("skills"),
            ));
        }
        sources.push(SkillSourceSpec::recursive(
            SkillSource::ZdxUser,
            zdx_home.join("skills"),
        ));
    }

    if options.sources.zdx_project.is_on() {
        sources.push(SkillSourceSpec::recursive(
            SkillSource::ZdxProject,
            options.cwd.join(".zdx").join("skills"),
        ));
    }

    if options.sources.codex_user.is_on()
        && let Some(home) = home_dir
    {
        sources.push(SkillSourceSpec::recursive(
            SkillSource::CodexUser,
            home.join(".codex").join("skills"),
        ));
    }

    if options.sources.claude_user.is_on()
        && let Some(home) = home_dir
    {
        sources.push(SkillSourceSpec::claude(
            SkillSource::ClaudeUser,
            home.join(".claude").join("skills"),
        ));
    }

    if options.sources.claude_project.is_on() {
        sources.push(SkillSourceSpec::claude(
            SkillSource::ClaudeProject,
            options.cwd.join(".claude").join("skills"),
        ));
    }

    if options.sources.agents_user.is_on()
        && let Some(home) = home_dir
    {
        sources.push(SkillSourceSpec::recursive(
            SkillSource::AgentsUser,
            home.join(".agents").join("skills"),
        ));
    }

    if options.sources.agents_project.is_on() {
        sources.push(SkillSourceSpec::recursive(
            SkillSource::AgentsProject,
            options.cwd.join(".agents").join("skills"),
        ));
    }

    sources
}

fn load_skills_from_sources_with_filters(
    sources: Vec<SkillSourceSpec>,
    filters: SkillFilters,
    warnings: Vec<SkillWarning>,
) -> LoadSkillsResult {
    let mut state = LoadState::new(filters, warnings);
    for source in sources {
        load_skills_from_dir_with_format(
            &source.base_dir,
            source.source,
            source.format,
            &mut state,
        );
    }

    LoadSkillsResult {
        skills: state.skills,
        warnings: state.warnings,
    }
}

#[cfg(test)]
fn load_skills_from_sources(sources: Vec<SkillSourceSpec>) -> LoadSkillsResult {
    load_skills_from_sources_with_filters(sources, SkillFilters::default(), Vec::new())
}

fn load_skills_from_dir_with_format(
    dir: &Path,
    source: SkillSource,
    format: SkillDirFormat,
    state: &mut LoadState,
) {
    if !dir.exists() {
        return;
    }

    match format {
        SkillDirFormat::Recursive => scan_recursive(dir, source, state),
        SkillDirFormat::ClaudeOneLevel => scan_claude_one_level(dir, source, state),
    }
}

fn scan_recursive(dir: &Path, source: SkillSource, state: &mut LoadState) {
    let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !state.seen_dirs.insert(canonical_dir) {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            state.warn(dir, format!("Failed to read skills directory: {e}"));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                state.warn(dir, format!("Failed to read skills directory entry: {e}"));
                continue;
            }
        };

        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) => {
                state.warn(&path, format!("Failed to read entry type: {e}"));
                continue;
            }
        };

        if file_type.is_dir() {
            scan_recursive(&path, source, state);
            continue;
        }

        if file_type.is_symlink() {
            if let Ok(metadata) = fs::metadata(&path)
                && metadata.is_dir()
            {
                scan_recursive(&path, source, state);
                continue;
            }

            if is_skill_file(&path) {
                load_skill_file(&path, source, state);
            }
            continue;
        }

        if file_type.is_file() && is_skill_file(&path) {
            load_skill_file(&path, source, state);
        }
    }
}

fn scan_claude_one_level(dir: &Path, source: SkillSource, state: &mut LoadState) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            state.warn(dir, format!("Failed to read skills directory: {e}"));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                state.warn(dir, format!("Failed to read skills directory entry: {e}"));
                continue;
            }
        };

        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) => {
                state.warn(&path, format!("Failed to read entry type: {e}"));
                continue;
            }
        };

        let is_dir = if file_type.is_dir() {
            true
        } else if file_type.is_symlink() {
            fs::metadata(&path)
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false)
        } else {
            false
        };

        if !is_dir {
            continue;
        }

        let skill_path = path.join("SKILL.md");
        if skill_path.exists() {
            load_skill_file(&skill_path, source, state);
        }
    }
}

fn is_skill_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "SKILL.md")
}

fn load_skill_file(path: &Path, source: SkillSource, state: &mut LoadState) {
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !state.seen_paths.insert(canonical_path.clone()) {
        state.warn(path, "Duplicate skill path detected; skipping");
        return;
    }

    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            state.warn(path, format!("Failed to read skill file: {e}"));
            return;
        }
    };

    let frontmatter = match parse_frontmatter(&content) {
        Ok(frontmatter) => frontmatter,
        Err(message) => {
            state.warn(path, message);
            return;
        }
    };

    let name = frontmatter.name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        state.warn(path, "Missing name in skill frontmatter");
        return;
    }

    if let Err(message) = validate_name(&name) {
        state.warn(path, message);
        return;
    }

    if !state.filters.should_include(&name) {
        return;
    }

    let description = frontmatter
        .description
        .unwrap_or_default()
        .trim()
        .to_string();
    if description.is_empty() {
        state.warn(path, "Missing description in skill frontmatter");
        return;
    }

    if let Err(message) = validate_description(&description) {
        state.warn(path, message);
        return;
    }

    let base_dir = canonical_path
        .parent()
        .map(Path::to_path_buf)
        .or_else(|| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf());

    if let Some(dir_name) = base_dir.file_name().and_then(|name| name.to_str())
        && dir_name != name
    {
        state.warn(
            path,
            format!("Skill name '{name}' does not match directory '{dir_name}'"),
        );
    }

    if !state.seen_names.insert(name.clone()) {
        state.warn(
            path,
            format!("Duplicate skill name '{name}' detected; skipping"),
        );
        return;
    }

    state.skills.push(Skill {
        name,
        description,
        file_path: canonical_path,
        base_dir,
        source,
    });
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

fn parse_frontmatter(content: &str) -> Result<SkillFrontmatter, String> {
    let content = strip_utf8_bom(content);
    let mut lines = content.lines();
    let Some(first) = lines.next() else {
        return Err("Missing YAML frontmatter".to_string());
    };

    if first.trim() != "---" {
        return Err("Missing YAML frontmatter".to_string());
    }

    let mut yaml_lines = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            let yaml = yaml_lines.join("\n");
            return serde_yaml::from_str(&yaml)
                .map_err(|e| format!("Failed to parse YAML frontmatter: {e}"));
        }
        yaml_lines.push(line);
    }

    Err("Unterminated YAML frontmatter".to_string())
}

fn strip_utf8_bom(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.len() > 64 {
        return Err(format!("Skill name '{name}' exceeds 64 characters"));
    }

    if name.starts_with('-') || name.ends_with('-') {
        return Err(format!(
            "Skill name '{name}' must not start or end with '-'"
        ));
    }

    if name.contains("--") {
        return Err(format!(
            "Skill name '{name}' must not contain consecutive '-'"
        ));
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!(
            "Skill name '{name}' must be lowercase alphanumeric or '-'"
        ));
    }

    Ok(())
}

fn validate_description(description: &str) -> Result<(), String> {
    let len = description.chars().count();
    if len > 1024 {
        return Err("Skill description exceeds 1024 characters".to_string());
    }

    Ok(())
}

fn build_skill_filters(
    include_skills: &[String],
    ignored_skills: &[String],
    warning_path: &Path,
    warnings: &mut Vec<SkillWarning>,
) -> SkillFilters {
    let include = compile_globset(include_skills, warning_path, warnings);
    let ignore = compile_globset(ignored_skills, warning_path, warnings);

    SkillFilters { include, ignore }
}

fn compile_globset(
    patterns: &[String],
    warning_path: &Path,
    warnings: &mut Vec<SkillWarning>,
) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }

    let mut builder = GlobSetBuilder::new();
    let mut added = 0usize;
    for pattern in patterns {
        match Glob::new(pattern) {
            Ok(glob) => {
                builder.add(glob);
                added += 1;
            }
            Err(err) => warnings.push(SkillWarning::new(
                warning_path,
                format!("Invalid skill glob pattern '{pattern}': {err}"),
            )),
        }
    }

    if added == 0 {
        return None;
    }

    match builder.build() {
        Ok(set) => Some(set),
        Err(err) => {
            warnings.push(SkillWarning::new(
                warning_path,
                format!("Failed to build skill glob matcher: {err}"),
            ));
            None
        }
    }
}

/// Formats skills metadata for the system prompt.
pub fn format_skills_for_prompt(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut output = String::new();
    output.push_str("The following skills provide specialized instructions for specific tasks.\n");
    output.push_str(
        "When a task matches a skill description, you MUST read the skill file from <path> and follow its instructions.\n\n",
    );

    output.push_str("<example>\n");
    output.push_str("User: [task matching a skill description]\n");
    output.push_str("Assistant: [read the skill <path>]\n");
    output.push_str("[reads and follows the skill instructions]\n");
    output.push_str("</example>\n\n");

    output.push_str("<available_skills>\n");

    for skill in skills {
        output.push_str("  <skill>\n");
        writeln!(output, "    <name>{}</name>", escape_xml(&skill.name)).expect("write");
        writeln!(
            output,
            "    <description>{}</description>",
            escape_xml(&skill.description)
        )
        .expect("write");
        writeln!(
            output,
            "    <path>{}</path>",
            escape_xml(&skill.file_path.display().to_string())
        )
        .expect("write");
        output.push_str("  </skill>\n");
    }

    output.push_str("</available_skills>");
    Some(output)
}

fn escape_xml(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn write_skill(dir: &Path, name: &str, description: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let content =
            format!("---\nname: {name}\ndescription: {description}\n---\n# Instructions\n");
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        skill_dir
    }

    #[test]
    fn test_valid_skill_loads() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "demo-skill", "A demo skill.");

        let result = load_skills_from_dir(dir.path(), SkillSource::ZdxUser);

        assert_eq!(result.skills.len(), 1);
        let skill = &result.skills[0];
        assert_eq!(skill.name, "demo-skill");
        assert_eq!(skill.description, "A demo skill.");
        assert_eq!(skill.source, SkillSource::ZdxUser);
    }

    #[test]
    fn test_missing_description_skipped() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("demo-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "---\nname: demo-skill\n---\n").unwrap();

        let result = load_skills_from_dir(dir.path(), SkillSource::ZdxUser);

        assert!(result.skills.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.message.contains("Missing description"))
        );
    }

    #[test]
    fn test_invalid_name_warns() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("bad-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: BadSkill\ndescription: Nope\n---\n",
        )
        .unwrap();

        let result = load_skills_from_dir(dir.path(), SkillSource::ZdxUser);

        assert!(result.skills.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.message.contains("must be lowercase"))
        );
    }

    #[test]
    fn test_name_directory_mismatch_warns() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("dir-name");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: other-name\ndescription: Ok\n---\n",
        )
        .unwrap();

        let result = load_skills_from_dir(dir.path(), SkillSource::ZdxUser);

        assert_eq!(result.skills.len(), 1);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.message.contains("does not match"))
        );
    }

    #[test]
    fn test_name_collision_first_wins() {
        let dir = tempdir().unwrap();
        let first_dir = dir.path().join("first");
        let second_dir = dir.path().join("second");
        write_skill(&first_dir, "dup-skill", "First");
        write_skill(&second_dir, "dup-skill", "Second");

        let sources = vec![
            SkillSourceSpec::recursive(SkillSource::ZdxUser, first_dir),
            SkillSourceSpec::recursive(SkillSource::CodexUser, second_dir),
        ];
        let result = load_skills_from_sources(sources);

        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].description, "First");
        assert_eq!(result.skills[0].source, SkillSource::ZdxUser);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.message.contains("Duplicate skill name"))
        );
    }

    #[test]
    fn test_format_skills_xml_escapes() {
        let skill = Skill {
            name: "demo-skill".to_string(),
            description: "Use <tag> & \"quotes\"".to_string(),
            file_path: PathBuf::from("/tmp/demo&skill/SKILL.md"),
            base_dir: PathBuf::from("/tmp/demo&skill"),
            source: SkillSource::ZdxUser,
        };

        let formatted = format_skills_for_prompt(&[skill]).unwrap();
        assert!(formatted.contains("&lt;tag&gt;"));
        assert!(formatted.contains("&amp;"));
        assert!(formatted.contains("&quot;"));
    }

    #[test]
    fn test_claude_format_one_level_only() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("claude-skill");
        fs::create_dir_all(skill_dir.join("nested")).unwrap();

        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: claude-skill\ndescription: Root\n---\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("nested").join("SKILL.md"),
            "---\nname: nested-skill\ndescription: Nested\n---\n",
        )
        .unwrap();

        let sources = vec![SkillSourceSpec::claude(
            SkillSource::ClaudeUser,
            dir.path().to_path_buf(),
        )];
        let result = load_skills_from_sources(sources);

        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "claude-skill");
    }

    #[test]
    fn test_utf8_bom_handled() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("bom-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        let content = "\u{feff}---\nname: bom-skill\ndescription: Ok\n---\n";
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();

        let result = load_skills_from_dir(dir.path(), SkillSource::ZdxUser);

        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "bom-skill");
    }

    #[cfg(unix)]
    #[test]
    fn test_broken_symlink_skipped() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("broken-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        let link_path = skill_dir.join("SKILL.md");
        symlink(skill_dir.join("missing.md"), &link_path).unwrap();

        let result = load_skills_from_dir(dir.path(), SkillSource::ZdxUser);

        assert!(result.skills.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.message.contains("Failed to read skill file"))
        );
    }

    #[test]
    fn test_config_based_source_filtering() {
        let root = tempdir().unwrap();
        let home = tempdir().unwrap();
        let zdx_home = tempdir().unwrap();

        write_skill(&zdx_home.path().join("skills"), "zdx-user", "User");
        write_skill(
            &root.path().join(".zdx").join("skills"),
            "zdx-project",
            "Proj",
        );
        write_skill(&home.path().join(".codex").join("skills"), "codex", "Codex");
        write_skill(
            &home.path().join(".claude").join("skills"),
            "claude-user",
            "Claude",
        );
        write_skill(
            &root.path().join(".claude").join("skills"),
            "claude-project",
            "ClaudeProj",
        );
        write_skill(
            &home.path().join(".agents").join("skills"),
            "agents-user",
            "AgentsUser",
        );
        write_skill(
            &root.path().join(".agents").join("skills"),
            "agents-project",
            "AgentsProj",
        );

        let options = LoadSkillsOptions {
            cwd: root.path().to_path_buf(),
            sources: SkillSourceToggles {
                zdx_user: Toggle::On,
                zdx_project: Toggle::On,
                codex_user: Toggle::Off,
                claude_user: Toggle::Off,
                claude_project: Toggle::On,
                agents_user: Toggle::Off,
                agents_project: Toggle::On,
            },
            ignored_skills: Vec::new(),
            include_skills: Vec::new(),
        };

        let sources = build_skill_sources(&options, zdx_home.path(), Some(home.path()));
        let result = load_skills_from_sources(sources);

        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"zdx-user"));
        assert!(names.contains(&"zdx-project"));
        assert!(names.contains(&"claude-project"));
        assert!(names.contains(&"agents-project"));
        assert!(!names.contains(&"codex"));
        assert!(!names.contains(&"claude-user"));
        assert!(!names.contains(&"agents-user"));
    }

    #[test]
    fn test_ignored_skills_filtering() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "test-skill", "Test");
        write_skill(dir.path(), "prod-skill", "Prod");

        let filters = build_skill_filters(
            &[] as &[String],
            &["test-*".to_string()],
            dir.path(),
            &mut Vec::new(),
        );
        let sources = vec![SkillSourceSpec::recursive(
            SkillSource::ZdxUser,
            dir.path().to_path_buf(),
        )];
        let result = load_skills_from_sources_with_filters(sources, filters, Vec::new());

        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"prod-skill"));
        assert!(!names.contains(&"test-skill"));
    }

    #[test]
    fn test_include_skills_filtering() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha-skill", "Alpha");
        write_skill(dir.path(), "beta-skill", "Beta");

        let filters = build_skill_filters(
            &["alpha-*".to_string()],
            &[] as &[String],
            dir.path(),
            &mut Vec::new(),
        );
        let sources = vec![SkillSourceSpec::recursive(
            SkillSource::ZdxUser,
            dir.path().to_path_buf(),
        )];
        let result = load_skills_from_sources_with_filters(sources, filters, Vec::new());

        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha-skill"));
        assert!(!names.contains(&"beta-skill"));
    }

    #[test]
    fn test_ignore_overrides_include() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha-skill", "Alpha");

        let filters = build_skill_filters(
            &["alpha-*".to_string()],
            &["alpha-*".to_string()],
            dir.path(),
            &mut Vec::new(),
        );
        let sources = vec![SkillSourceSpec::recursive(
            SkillSource::ZdxUser,
            dir.path().to_path_buf(),
        )];
        let result = load_skills_from_sources_with_filters(sources, filters, Vec::new());

        assert!(result.skills.is_empty());
    }
}
