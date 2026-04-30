//! Custom slash command discovery and parsing.
//!
//! Custom commands are user-defined slash commands that augment the built-in
//! command palette. They live in two locations:
//!
//! - `<ZDX_HOME>/commands/*.md` (user-global)
//! - `<cwd>/.zdx/commands/*.md` (per-project)
//!
//! Each Markdown file becomes a command whose name is the file stem. An
//! optional YAML frontmatter block may set `description` (shown in the
//! palette). The remaining body is the prompt content.
//!
//! Built-in commands always win: any custom command whose name matches a
//! built-in name is skipped with a warning.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::paths;

/// Source location for a custom command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomCommandSource {
    /// `<ZDX_HOME>/commands/*.md`
    User,
    /// `<cwd>/.zdx/commands/*.md`
    Project,
}

impl CustomCommandSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// Parsed custom command definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCommand {
    /// Command name (file stem, without extension).
    pub name: String,
    /// Optional short description (from YAML frontmatter `description`).
    pub description: Option<String>,
    /// Where the command was discovered.
    pub source: CustomCommandSource,
    /// Path to the source file.
    pub path: PathBuf,
    /// Prompt content (file body with frontmatter stripped).
    pub content: String,
    /// Reserved for a future executable-command variant; always `false` today
    /// (only Markdown commands are loaded).
    pub is_executable: bool,
}

/// Warning produced during custom command discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCommandWarning {
    pub path: PathBuf,
    pub message: String,
}

/// Result of loading custom commands.
#[derive(Debug, Clone, Default)]
pub struct LoadCustomCommandsResult {
    pub commands: Vec<CustomCommand>,
    pub warnings: Vec<CustomCommandWarning>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CommandFrontmatter {
    description: Option<String>,
}

/// Loads custom commands from the standard user and project directories.
///
/// `builtin_names` are command names that custom commands may not shadow
/// (built-ins always win). The TUI supplies this from its static `COMMANDS`
/// array.
#[must_use]
pub fn load_custom_commands(cwd: &Path, builtin_names: &[&str]) -> LoadCustomCommandsResult {
    let user_dir = paths::zdx_home().join("commands");
    let project_dir = cwd.join(".zdx").join("commands");
    load_custom_commands_from_dirs(&user_dir, &project_dir, builtin_names)
}

/// Loads custom commands from explicit user and project directories.
///
/// Useful for tests that want to avoid env-var mutation.
#[must_use]
pub fn load_custom_commands_from_dirs(
    user_dir: &Path,
    project_dir: &Path,
    builtin_names: &[&str],
) -> LoadCustomCommandsResult {
    let mut state = LoadState {
        commands: Vec::new(),
        warnings: Vec::new(),
        seen_names: HashSet::new(),
        builtin_names,
    };

    // User dir is loaded first so project commands with the same name are
    // skipped as duplicates (with a warning) rather than overriding.
    scan_dir(user_dir, CustomCommandSource::User, &mut state);
    scan_dir(project_dir, CustomCommandSource::Project, &mut state);

    LoadCustomCommandsResult {
        commands: state.commands,
        warnings: state.warnings,
    }
}

struct LoadState<'a> {
    commands: Vec<CustomCommand>,
    warnings: Vec<CustomCommandWarning>,
    seen_names: HashSet<String>,
    builtin_names: &'a [&'a str],
}

impl LoadState<'_> {
    fn warn(&mut self, path: &Path, message: impl Into<String>) {
        self.warnings.push(CustomCommandWarning {
            path: path.to_path_buf(),
            message: message.into(),
        });
    }
}

fn scan_dir(dir: &Path, source: CustomCommandSource, state: &mut LoadState) {
    if !dir.exists() {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            state.warn(dir, format!("Failed to read commands directory: {e}"));
            return;
        }
    };

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                state.warn(dir, format!("Failed to read commands directory entry: {e}"));
                continue;
            }
        };

        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        paths.push(path);
    }

    paths.sort();
    for path in paths {
        load_command_file(&path, source, state);
    }
}

fn load_command_file(path: &Path, source: CustomCommandSource, state: &mut LoadState) {
    let Some(name) = command_name_from_path(path) else {
        state.warn(path, "Invalid command file name; skipping");
        return;
    };

    // Hidden files (e.g. `.draft.md`) are intentionally skipped; their stem
    // would produce a confusing slash command name like `/.draft`.
    if name.starts_with('.') {
        return;
    }

    let key = name.to_ascii_lowercase();

    if state
        .builtin_names
        .iter()
        .any(|builtin| builtin.eq_ignore_ascii_case(&name))
    {
        state.warn(
            path,
            format!("Custom command '{name}' shadows a built-in command; skipping"),
        );
        return;
    }

    if state.seen_names.contains(&key) {
        state.warn(
            path,
            format!("Duplicate custom command name '{name}'; skipping"),
        );
        return;
    }

    let raw = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            state.warn(path, format!("Failed to read command file: {e}"));
            return;
        }
    };

    // Reserve the name only after the file is successfully read, so a
    // failed read does not block a same-named command from another dir.
    state.seen_names.insert(key);

    let (description, content) = parse_command_content(&raw, path, state);

    state.commands.push(CustomCommand {
        name,
        description,
        source,
        path: path.to_path_buf(),
        content,
        is_executable: false,
    });
}

fn command_name_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem().and_then(|s| s.to_str()).map(str::trim)?;
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_string())
}

/// Splits frontmatter from body.
///
/// Behavior:
/// - No opening `---` fence → the whole file is the body, no description.
/// - Opening fence with no closing fence → the whole file is treated as body
///   and a warning is recorded (we cannot tell where metadata ends).
/// - Opening + closing fences with malformed YAML → body after the closing
///   fence is used as content, no description, warning recorded.
fn parse_command_content(
    raw: &str,
    path: &Path,
    state: &mut LoadState,
) -> (Option<String>, String) {
    let stripped = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let lines: Vec<&str> = stripped.lines().collect();

    let starts_with_fence = lines.first().is_some_and(|line| line.trim() == "---");
    if !starts_with_fence {
        return (None, trim_blank_envelope(stripped));
    }

    // Find the closing `---` / `...` fence.
    let mut close_idx: Option<usize> = None;
    for (idx, line) in lines.iter().enumerate().skip(1) {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            close_idx = Some(idx);
            break;
        }
    }

    let Some(idx) = close_idx else {
        state.warn(
            path,
            "Unterminated YAML frontmatter; treating full file as content",
        );
        return (None, trim_blank_envelope(stripped));
    };

    let yaml = lines[1..idx].join("\n");
    let body = lines[idx + 1..].join("\n");

    let description = if yaml.trim().is_empty() {
        None
    } else {
        match serde_yaml::from_str::<CommandFrontmatter>(&yaml) {
            Ok(fm) => normalize_description(fm.description),
            Err(e) => {
                state.warn(
                    path,
                    format!("Failed to parse YAML frontmatter: {e}; using body as content"),
                );
                None
            }
        }
    };

    (description, trim_blank_envelope(&body))
}

/// Trims leading and trailing blank lines while preserving any indentation on
/// the first and last non-blank lines. This keeps user-authored prompts
/// (including indented code blocks) intact when inserted into the input.
fn trim_blank_envelope(s: &str) -> String {
    let lines: Vec<&str> = s.split('\n').collect();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    match (first, last) {
        (Some(start), Some(end)) => lines[start..=end].join("\n"),
        _ => String::new(),
    }
}

fn normalize_description(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    const BUILTINS: &[&str] = &["quit", "new", "model"];

    fn write_md(dir: &Path, name: &str, body: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}.md"));
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn test_load_custom_commands_empty_dir() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();

        let result = load_custom_commands_from_dirs(
            &user.path().join("commands"),
            &project.path().join(".zdx").join("commands"),
            BUILTINS,
        );

        assert!(result.commands.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_load_custom_commands_skips_builtin_names() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(user.path(), "quit", "should be skipped");
        write_md(user.path(), "review", "should be kept");

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        let names: Vec<&str> = result.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["review"]);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.message.contains("shadows a built-in"))
        );
    }

    #[test]
    fn test_load_custom_commands_parses_frontmatter() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(
            user.path(),
            "review",
            "---\ndescription: Review code for bugs\n---\nReview this code carefully.\n",
        );

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        assert_eq!(result.commands.len(), 1);
        let cmd = &result.commands[0];
        assert_eq!(cmd.name, "review");
        assert_eq!(cmd.description.as_deref(), Some("Review code for bugs"));
        assert_eq!(cmd.content, "Review this code carefully.");
        assert_eq!(cmd.source, CustomCommandSource::User);
        assert!(!cmd.is_executable);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_load_custom_commands_no_frontmatter() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(user.path(), "test", "Hello from custom command");

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        assert_eq!(result.commands.len(), 1);
        let cmd = &result.commands[0];
        assert_eq!(cmd.name, "test");
        assert_eq!(cmd.description, None);
        assert_eq!(cmd.content, "Hello from custom command");
    }

    #[test]
    fn test_load_custom_commands_user_and_project_merged() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(user.path(), "user-only", "user");
        write_md(project.path(), "project-only", "project");

        let result = load_custom_commands_from_dirs(user.path(), project.path(), BUILTINS);

        let mut names: Vec<&str> = result.commands.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["project-only", "user-only"]);

        let by_name: std::collections::HashMap<&str, CustomCommandSource> = result
            .commands
            .iter()
            .map(|c| (c.name.as_str(), c.source))
            .collect();
        assert_eq!(by_name["user-only"], CustomCommandSource::User);
        assert_eq!(by_name["project-only"], CustomCommandSource::Project);
    }

    #[test]
    fn test_load_custom_commands_user_wins_on_duplicate() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(user.path(), "shared", "user version");
        write_md(project.path(), "shared", "project version");

        let result = load_custom_commands_from_dirs(user.path(), project.path(), BUILTINS);

        assert_eq!(result.commands.len(), 1);
        assert_eq!(result.commands[0].source, CustomCommandSource::User);
        assert_eq!(result.commands[0].content, "user version");
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.message.contains("Duplicate custom command name"))
        );
    }

    #[test]
    fn test_load_custom_commands_malformed_frontmatter_falls_back_to_body() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(
            user.path(),
            "weird",
            "---\nthis is: : not yaml\n  bad indent\n---\nactual prompt body\n",
        );

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        assert_eq!(result.commands.len(), 1);
        let cmd = &result.commands[0];
        assert_eq!(cmd.name, "weird");
        assert_eq!(cmd.description, None);
        assert_eq!(cmd.content, "actual prompt body");
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.message.contains("Failed to parse YAML frontmatter"))
        );
    }

    #[test]
    fn test_load_custom_commands_unterminated_frontmatter_uses_full_body() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(
            user.path(),
            "open",
            "---\ndescription: never closed\nbody here",
        );

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        assert_eq!(result.commands.len(), 1);
        let cmd = &result.commands[0];
        assert_eq!(cmd.description, None);
        assert!(cmd.content.contains("description: never closed"));
        assert!(cmd.content.contains("body here"));
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.message.contains("Unterminated YAML frontmatter"))
        );
    }

    #[test]
    fn test_load_custom_commands_ignores_non_md_files() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        fs::create_dir_all(user.path()).unwrap();
        fs::write(user.path().join("notes.txt"), "ignored").unwrap();
        fs::write(user.path().join("README"), "ignored").unwrap();
        write_md(user.path(), "kept", "yes");

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        let names: Vec<&str> = result.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["kept"]);
    }

    #[test]
    fn test_load_custom_commands_handles_utf8_bom() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(
            user.path(),
            "bom",
            "\u{feff}---\ndescription: With BOM\n---\nbody\n",
        );

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        assert_eq!(result.commands.len(), 1);
        assert_eq!(result.commands[0].description.as_deref(), Some("With BOM"));
        assert_eq!(result.commands[0].content, "body");
    }

    #[test]
    fn test_load_custom_commands_builtin_shadow_is_case_insensitive() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(user.path(), "Quit", "should be skipped");

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        assert!(result.commands.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.message.contains("shadows a built-in"))
        );
    }

    #[test]
    fn test_load_custom_commands_duplicate_detection_is_case_insensitive() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(user.path(), "Review", "user");
        write_md(project.path(), "review", "project");

        let result = load_custom_commands_from_dirs(user.path(), project.path(), BUILTINS);

        assert_eq!(result.commands.len(), 1);
        assert_eq!(result.commands[0].source, CustomCommandSource::User);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.message.contains("Duplicate custom command name"))
        );
    }

    #[test]
    fn test_load_custom_commands_skips_hidden_files() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        fs::create_dir_all(user.path()).unwrap();
        fs::write(user.path().join(".draft.md"), "hidden").unwrap();
        write_md(user.path(), "kept", "yes");

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        let names: Vec<&str> = result.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["kept"]);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_load_custom_commands_preserves_first_line_indentation() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(
            user.path(),
            "indented",
            "    Indented first line\n    More indented\n",
        );

        let result =
            load_custom_commands_from_dirs(user.path(), &project.path().join("missing"), BUILTINS);

        assert_eq!(result.commands.len(), 1);
        assert_eq!(
            result.commands[0].content,
            "    Indented first line\n    More indented"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_load_custom_commands_failed_read_does_not_block_project_file() {
        use std::os::unix::fs::PermissionsExt;

        // Skip when running as root: chmod 000 is bypassed.
        // SAFETY: `geteuid` is a thread-safe libc call with no side effects.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        let user_file = write_md(user.path(), "blocked", "should not load");
        fs::set_permissions(&user_file, fs::Permissions::from_mode(0o000)).unwrap();
        write_md(project.path(), "blocked", "real content");

        let result = load_custom_commands_from_dirs(user.path(), project.path(), BUILTINS);

        // Restore permissions so the tempdir cleanup can run.
        fs::set_permissions(&user_file, fs::Permissions::from_mode(0o644)).unwrap();

        let blocked: Vec<&CustomCommand> = result
            .commands
            .iter()
            .filter(|c| c.name == "blocked")
            .collect();
        assert_eq!(blocked.len(), 1, "project copy must still load");
        assert_eq!(blocked[0].source, CustomCommandSource::Project);
        assert_eq!(blocked[0].content, "real content");
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.message.contains("Failed to read command file"))
        );
    }
}
