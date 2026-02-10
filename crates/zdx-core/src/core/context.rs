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

use crate::config::{Config, paths};
use crate::skills::{
    LoadSkillsOptions, LoadSkillsResult, Skill, format_skills_for_prompt, load_skills,
};

/// Maximum size for a single AGENTS.md file (64KB).
/// Files larger than this are truncated with a warning.
pub const MAX_AGENTS_FILE_SIZE: usize = 64 * 1024;

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
        Self {
            path: Some(path.to_path_buf()),
            message: format!(
                "Truncated {} ({} bytes) to {} bytes",
                path.display(),
                original_size,
                MAX_AGENTS_FILE_SIZE
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

/// Result of building the effective system prompt.
#[derive(Debug, Clone, Default)]
pub struct EffectivePrompt {
    /// The combined system prompt (config + AGENTS.md files).
    pub prompt: Option<String>,
    /// Paths of AGENTS.md files that were loaded (in order).
    pub loaded_agents_paths: Vec<PathBuf>,
    /// Warnings generated during context loading.
    pub warnings: Vec<ContextWarning>,
    /// Skills loaded from configured sources.
    pub loaded_skills: Vec<Skill>,
}

/// Builds the effective system prompt by combining config and AGENTS.md files.
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
) -> Result<EffectivePrompt> {
    let mut system_prompt = config.effective_system_prompt()?;
    let mut loaded_agents_paths = Vec::new();
    let mut warnings = Vec::new();

    // Auto-include AGENTS.md files from hierarchy
    if let Some(loaded) = load_all_agents_files(root) {
        loaded_agents_paths = loaded.loaded_paths;
        warnings = loaded.warnings;

        if !loaded.content.is_empty() {
            let combined = match system_prompt {
                Some(sp) => format!("{}\n\n# Project Context\n\n{}", sp, loaded.content),
                None => format!("# Project Context\n\n{}", loaded.content),
            };
            system_prompt = Some(combined);
        }
    }

    let mut skill_options = LoadSkillsOptions::new(root);
    skill_options.sources = config.skills.sources.clone();
    skill_options
        .ignored_skills
        .clone_from(&config.skills.ignored_skills);
    skill_options
        .include_skills
        .clone_from(&config.skills.include_skills);

    let skills_result = load_skills(&skill_options);
    let LoadSkillsResult {
        skills,
        warnings: skill_warnings,
    } = skills_result;

    if let Some(skills_block) = format_skills_for_prompt(&skills) {
        let combined = match system_prompt {
            Some(sp) => format!("{sp}\n\n{skills_block}"),
            None => skills_block,
        };
        system_prompt = Some(combined);
    }

    if config.subagents.enabled {
        let available_models = config.subagent_available_models();
        let subagents_block =
            format_subagents_for_prompt(config.subagents.enabled, &available_models);
        let combined = match system_prompt {
            Some(sp) => format!("{sp}\n\n{subagents_block}"),
            None => subagents_block,
        };
        system_prompt = Some(combined);
    }

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

    let loaded_skills = skills;

    Ok(EffectivePrompt {
        prompt: system_prompt,
        loaded_agents_paths,
        warnings,
        loaded_skills,
    })
}

fn format_subagents_for_prompt(enabled: bool, available_models: &[String]) -> String {
    let available_models = if available_models.is_empty() {
        "(none)".to_string()
    } else {
        available_models
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };

    format!(
        "<subagents>\n  <enabled>{enabled}</enabled>\n  <available_models>{available_models}</available_models>\n</subagents>",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::config::SkillSourceToggles;

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
    fn test_subagents_block_includes_config_values() {
        let block = format_subagents_for_prompt(
            true,
            &[
                "codex:gpt-5.3-codex".to_string(),
                "openai:gpt-5.2".to_string(),
            ],
        );
        assert!(block.contains("<subagents>"));
        assert!(block.contains("<enabled>true</enabled>"));
        assert!(block.contains("<available_models>"));
        assert!(block.contains("codex:gpt-5.3-codex"));
        assert!(block.contains("openai:gpt-5.2"));
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

        let effective = build_effective_system_prompt_with_paths(&config, dir.path()).unwrap();
        let prompt = effective.prompt.unwrap_or_default();
        assert!(!prompt.contains("<subagents>"));
    }
}
