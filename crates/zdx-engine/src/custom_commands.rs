//! Custom slash command discovery and parsing.
//!
//! Custom commands are user-defined slash commands that augment the built-in
//! command palette. They live in three kinds of locations:
//!
//! - `<ZDX_HOME>/commands/*.md` (user-global)
//! - each ancestor `.zdx/commands/*.md`, through `<cwd>/.zdx/commands/*.md`
//! - bundled commands embedded in the binary (always available)
//!
//! Each Markdown file becomes a command whose name is the file stem. An
//! optional YAML frontmatter block may set `description` (shown in the
//! palette). The remaining body is the prompt content.
//!
//! Built-in commands always win: any custom command whose name matches a
//! built-in name is skipped with a warning. Commands from nearer directories
//! override broader user/ancestor commands, and user/project commands shadow
//! bundled commands silently so users can override the shipped defaults.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zdx_assets::BundledCommandAsset;

use crate::config::paths;

/// Source location for a custom command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomCommandSource {
    /// Bundled with the binary (`crates/zdx-assets/bundled_commands/*.md`).
    BuiltIn,
    /// `<ZDX_HOME>/commands/*.md`
    User,
    /// `<cwd>/.zdx/commands/*.md`
    Project,
}

impl CustomCommandSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "builtin",
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

/// Loads custom commands from the standard user directory and ancestor project
/// directories, then merges in the bundled commands embedded in the binary.
///
/// `builtin_names` are command names that custom commands may not shadow
/// (built-ins always win). The TUI supplies this from its static `COMMANDS`
/// array.
#[must_use]
pub fn load_custom_commands(cwd: &Path, builtin_names: &[&str]) -> LoadCustomCommandsResult {
    let user_dir = paths::zdx_home().join("commands");

    let mut state = LoadState {
        commands: Vec::new(),
        warnings: Vec::new(),
        command_indices: HashMap::new(),
        builtin_names,
    };

    scan_dir(&user_dir, CustomCommandSource::User, &mut state);
    for project_dir in command_dirs_for_cwd(cwd) {
        scan_dir(&project_dir, CustomCommandSource::Project, &mut state);
    }
    load_bundled_commands(zdx_assets::bundled_command_assets(), &mut state);

    LoadCustomCommandsResult {
        commands: state.commands,
        warnings: state.warnings,
    }
}

/// Loads custom commands from explicit user and project directories.
///
/// Filesystem-only — does not include bundled commands. The production entry
/// point [`load_custom_commands`] composes this with bundled commands.
#[must_use]
pub fn load_custom_commands_from_dirs(
    user_dir: &Path,
    project_dir: &Path,
    builtin_names: &[&str],
) -> LoadCustomCommandsResult {
    let mut state = LoadState {
        commands: Vec::new(),
        warnings: Vec::new(),
        command_indices: HashMap::new(),
        builtin_names,
    };

    // User dir is loaded first so project commands with the same name can
    // override broader global commands.
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
    command_indices: HashMap<String, usize>,
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

fn command_dirs_for_cwd(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = cwd
        .ancestors()
        .map(|ancestor| ancestor.join(".zdx").join("commands"))
        .collect();
    dirs.reverse();
    dirs
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

/// Ingests bundled commands embedded in the binary.
///
/// User and project commands with the same name shadow bundled commands
/// silently (no warning) — bundled is the fallback layer. A bundled command
/// that collides with a built-in name is also skipped silently because that
/// would be a build-time mistake in this crate, not a user-facing condition.
fn load_bundled_commands(assets: &[BundledCommandAsset], state: &mut LoadState) {
    for asset in assets {
        load_bundled_command(asset, state);
    }
}

fn load_bundled_command(asset: &BundledCommandAsset, state: &mut LoadState) {
    let synthetic_path = PathBuf::from(format!("<bundled>/{}", asset.relative_path));
    let Some(name) = command_name_from_path(&synthetic_path) else {
        return;
    };

    if name.starts_with('.') {
        return;
    }

    if state
        .builtin_names
        .iter()
        .any(|builtin| builtin.eq_ignore_ascii_case(&name))
    {
        return;
    }

    let key = name.to_ascii_lowercase();
    if state.command_indices.contains_key(&key) {
        // User or project already provided a command with this name; their
        // version wins silently.
        return;
    }
    state.command_indices.insert(key, state.commands.len());

    let raw = if let Ok(s) = std::str::from_utf8(asset.bytes) {
        s.to_string()
    } else {
        state.warn(
            &synthetic_path,
            "Bundled command file is not valid UTF-8; skipping",
        );
        return;
    };

    let (description, content) = parse_command_content(&raw, &synthetic_path, state);

    state.commands.push(CustomCommand {
        name,
        description,
        source: CustomCommandSource::BuiltIn,
        path: synthetic_path,
        content,
        is_executable: false,
    });
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

    let duplicate_index = state.command_indices.get(&key).copied();

    let raw = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            state.warn(path, format!("Failed to read command file: {e}"));
            return;
        }
    };

    // Reserve the name only after the file is successfully read, so a
    // failed read does not block a same-named command from another dir.
    let (description, content) = parse_command_content(&raw, path, state);

    if duplicate_index.is_some() {
        state.warn(
            path,
            format!("Duplicate custom command name '{name}'; overriding broader command"),
        );
    }

    let command = CustomCommand {
        name,
        description,
        source,
        path: path.to_path_buf(),
        content,
        is_executable: false,
    };

    if let Some(index) = duplicate_index {
        state.commands[index] = command;
    } else {
        state.command_indices.insert(key, state.commands.len());
        state.commands.push(command);
    }
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
    use std::collections::HashSet;
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
    fn test_load_custom_commands_project_wins_on_duplicate() {
        let user = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_md(user.path(), "shared", "user version");
        write_md(project.path(), "shared", "project version");

        let result = load_custom_commands_from_dirs(user.path(), project.path(), BUILTINS);

        assert_eq!(result.commands.len(), 1);
        assert_eq!(result.commands[0].source, CustomCommandSource::Project);
        assert_eq!(result.commands[0].content, "project version");
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
        assert_eq!(result.commands[0].source, CustomCommandSource::Project);
        assert_eq!(result.commands[0].content, "project");
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.message.contains("Duplicate custom command name"))
        );
    }

    #[test]
    fn test_command_dirs_for_cwd_walks_ancestors_from_broad_to_near() {
        let root = tempdir().unwrap();
        let nested = root.path().join("one").join("two");
        fs::create_dir_all(&nested).unwrap();

        let dirs = command_dirs_for_cwd(&nested);

        let root_commands = root.path().join(".zdx").join("commands");
        let one_commands = root.path().join("one").join(".zdx").join("commands");
        let nested_commands = nested.join(".zdx").join("commands");
        let root_index = dirs.iter().position(|dir| dir == &root_commands).unwrap();
        let one_index = dirs.iter().position(|dir| dir == &one_commands).unwrap();
        let nested_index = dirs.iter().position(|dir| dir == &nested_commands).unwrap();

        assert!(root_index < one_index);
        assert!(one_index < nested_index);
    }

    #[test]
    fn test_nearer_project_command_overrides_shared_ancestor() {
        let root = tempdir().unwrap();
        let shared = root.path().join(".zdx").join("commands");
        let nested = root.path().join("child").join(".zdx").join("commands");
        write_md(&shared, "review", "shared version");
        write_md(&nested, "review", "nested version");

        let mut state = LoadState {
            commands: Vec::new(),
            warnings: Vec::new(),
            command_indices: HashMap::new(),
            builtin_names: BUILTINS,
        };
        scan_dir(&shared, CustomCommandSource::Project, &mut state);
        scan_dir(&nested, CustomCommandSource::Project, &mut state);

        assert_eq!(state.commands.len(), 1);
        assert_eq!(state.commands[0].content, "nested version");
        assert_eq!(state.commands[0].path, nested.join("review.md"));
        assert!(
            state
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

    #[test]
    fn test_load_bundled_commands_appends_to_state() {
        let assets: &[BundledCommandAsset] = &[
            BundledCommandAsset {
                relative_path: "ship.md",
                bytes: b"---\ndescription: Ship it\n---\nDo the thing.\n",
            },
            BundledCommandAsset {
                relative_path: "raw.md",
                bytes: b"raw body without frontmatter",
            },
        ];

        let mut state = LoadState {
            commands: Vec::new(),
            warnings: Vec::new(),
            command_indices: HashMap::new(),
            builtin_names: BUILTINS,
        };

        load_bundled_commands(assets, &mut state);

        let by_name: std::collections::HashMap<&str, &CustomCommand> = state
            .commands
            .iter()
            .map(|c| (c.name.as_str(), c))
            .collect();

        let ship = by_name.get("ship").expect("ship loaded");
        assert_eq!(ship.source, CustomCommandSource::BuiltIn);
        assert_eq!(ship.description.as_deref(), Some("Ship it"));
        assert_eq!(ship.content, "Do the thing.");

        let raw = by_name.get("raw").expect("raw loaded");
        assert_eq!(raw.source, CustomCommandSource::BuiltIn);
        assert_eq!(raw.description, None);
        assert_eq!(raw.content, "raw body without frontmatter");

        assert!(state.warnings.is_empty());
    }

    #[test]
    fn test_load_bundled_commands_user_shadows_silently() {
        let assets: &[BundledCommandAsset] = &[BundledCommandAsset {
            relative_path: "plan.md",
            bytes: b"bundled body",
        }];

        let user = tempdir().unwrap();
        write_md(user.path(), "plan", "user body");

        let mut state = LoadState {
            commands: Vec::new(),
            warnings: Vec::new(),
            command_indices: HashMap::new(),
            builtin_names: BUILTINS,
        };
        scan_dir(user.path(), CustomCommandSource::User, &mut state);
        load_bundled_commands(assets, &mut state);

        assert_eq!(state.commands.len(), 1);
        assert_eq!(state.commands[0].source, CustomCommandSource::User);
        assert_eq!(state.commands[0].content, "user body");
        assert!(state.warnings.is_empty(), "shadowing bundled must not warn");
    }

    #[test]
    fn test_load_bundled_commands_skips_builtin_collision_silently() {
        let assets: &[BundledCommandAsset] = &[BundledCommandAsset {
            relative_path: "quit.md",
            bytes: b"bundled body",
        }];

        let mut state = LoadState {
            commands: Vec::new(),
            warnings: Vec::new(),
            command_indices: HashMap::new(),
            builtin_names: BUILTINS,
        };
        load_bundled_commands(assets, &mut state);

        assert!(state.commands.is_empty());
        assert!(state.warnings.is_empty());
    }

    #[test]
    fn test_real_bundled_commands_are_present() {
        // Smoke test: confirm the workspace's bundled markdown files actually
        // round-trip into discoverable commands when no user/project commands
        // exist. We exercise the full `load_bundled_commands` path with the
        // crate's real embedded assets.
        let mut state = LoadState {
            commands: Vec::new(),
            warnings: Vec::new(),
            command_indices: HashMap::new(),
            builtin_names: BUILTINS,
        };
        load_bundled_commands(zdx_assets::bundled_command_assets(), &mut state);

        let names: HashSet<String> = state.commands.iter().map(|c| c.name.clone()).collect();
        for expected in ["plan", "investigate", "execute-plan", "review-loop"] {
            assert!(
                names.contains(expected),
                "expected bundled command '{expected}' to be present, got {names:?}",
            );
        }
        for cmd in &state.commands {
            assert_eq!(cmd.source, CustomCommandSource::BuiltIn);
            assert!(!cmd.content.is_empty());
        }
    }
}
