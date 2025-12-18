//! Context module for loading project-specific guidelines.
//!
//! AGENTS.md files are loaded hierarchically:
//! 1. ZDX_HOME/AGENTS.md (global user guidelines)
//! 2. ~/AGENTS.md (user home)
//! 3. Ancestor directories from home to project root
//! 4. Project root (--root or cwd)

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::Config;
use crate::paths;

/// Result of loading AGENTS.md files.
#[derive(Debug, Clone)]
pub struct LoadedContext {
    /// Combined content from all AGENTS.md files.
    pub content: String,
    /// Paths of files that were loaded (in order).
    pub loaded_paths: Vec<PathBuf>,
}

/// Collects all AGENTS.md paths to check, in order.
///
/// Order:
/// 1. ZDX_HOME/AGENTS.md (always included - global user config)
/// 2. ~/AGENTS.md (only if root is under home)
/// 3. Ancestors from home to root (only if root is under home)
/// 4. root/AGENTS.md
///
/// Paths are deduplicated (later occurrences removed).
pub fn collect_agents_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    // 1. ZDX_HOME/AGENTS.md (always - this is explicit user config)
    let zdx_home = paths::zdx_home();
    paths.push(zdx_home.join("AGENTS.md"));

    // Canonicalize root for comparison
    let canonical_root = root.canonicalize().ok();

    // 2-3. User home and ancestors (only if root is under home)
    if let Some(home) = dirs::home_dir() {
        if let Some(ref cr) = canonical_root {
            if let Ok(canonical_home) = home.canonicalize() {
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
/// Unreadable files log a warning but don't fail.
pub fn load_all_agents_files(root: &Path) -> Option<LoadedContext> {
    let paths = collect_agents_paths(root);
    let mut loaded_paths: Vec<PathBuf> = Vec::new();
    let mut sections: Vec<String> = Vec::new();

    for path in paths {
        if !path.exists() {
            continue;
        }

        match fs::read_to_string(&path) {
            Ok(content) => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    sections.push(format!("## {}\n\n{}", path.display(), trimmed));
                    loaded_paths.push(path);
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to read {}: {}", path.display(), e);
            }
        }
    }

    if sections.is_empty() {
        return None;
    }

    let content = sections.join("\n\n");
    Some(LoadedContext {
        content,
        loaded_paths,
    })
}

/// Loads project context from AGENTS.md in the given root directory.
///
/// **Deprecated:** Use `load_all_agents_files` for hierarchical loading.
#[allow(dead_code)]
pub fn load_project_context(root: &Path) -> Option<String> {
    let agents_md = root.join("AGENTS.md");
    if !agents_md.exists() {
        return None;
    }

    let content = match fs::read_to_string(&agents_md) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Warning: Failed to read AGENTS.md in {}: {}",
                root.display(),
                e
            );
            return None;
        }
    };

    let trimmed = content.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Result of building the effective system prompt.
#[derive(Debug, Clone)]
pub struct EffectivePrompt {
    /// The combined system prompt (config + AGENTS.md files).
    pub prompt: Option<String>,
    /// Paths of AGENTS.md files that were loaded (in order).
    pub loaded_agents_paths: Vec<PathBuf>,
}

/// Builds the effective system prompt by combining config and AGENTS.md files.
///
/// AGENTS.md files are loaded hierarchically from:
/// 1. ZDX_HOME/AGENTS.md
/// 2. ~/AGENTS.md  
/// 3. Ancestor directories from home to project root
/// 4. Project root
///
/// Returns the combined prompt and the list of loaded AGENTS.md paths.
pub fn build_effective_system_prompt_with_paths(
    config: &Config,
    root: &Path,
) -> Result<EffectivePrompt> {
    let mut system_prompt = config.effective_system_prompt()?;
    let mut loaded_agents_paths = Vec::new();

    // Auto-include AGENTS.md files from hierarchy
    if let Some(loaded) = load_all_agents_files(root) {
        loaded_agents_paths = loaded.loaded_paths;

        let combined = match system_prompt {
            Some(sp) => format!("{}\n\n# Project Context\n\n{}", sp, loaded.content),
            None => format!("# Project Context\n\n{}", loaded.content),
        };
        system_prompt = Some(combined);
    }

    Ok(EffectivePrompt {
        prompt: system_prompt,
        loaded_agents_paths,
    })
}

/// Builds the effective system prompt by combining config and AGENTS.md files.
///
/// This is a convenience wrapper that logs loaded files to stderr.
/// For more control, use `build_effective_system_prompt_with_paths`.
pub fn build_effective_system_prompt(config: &Config, root: &Path) -> Result<Option<String>> {
    let result = build_effective_system_prompt_with_paths(config, root)?;

    // Log loaded files to stderr (per SPEC §10)
    if !result.loaded_agents_paths.is_empty() {
        let paths_str: Vec<String> = result
            .loaded_agents_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        eprintln!("Loaded AGENTS.md from: {}", paths_str.join(", "));
    }

    Ok(result.prompt)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_load_project_context_present() {
        let dir = tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");
        fs::write(&agents_md, "Project guidelines").unwrap();

        let context = load_project_context(dir.path());
        assert_eq!(context, Some("Project guidelines".to_string()));
    }

    #[test]
    fn test_load_project_context_missing() {
        let dir = tempdir().unwrap();
        let context = load_project_context(dir.path());
        assert_eq!(context, None);
    }

    #[test]
    fn test_load_project_context_empty() {
        let dir = tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");
        fs::write(&agents_md, "  ").unwrap();

        let context = load_project_context(dir.path());
        assert_eq!(context, None);
    }

    #[test]
    fn test_collect_agents_paths_includes_zdx_home() {
        let dir = tempdir().unwrap();
        let paths = collect_agents_paths(dir.path());

        // Should include ZDX_HOME/AGENTS.md
        let zdx_home_agents = paths::zdx_home().join("AGENTS.md");
        assert!(
            paths.contains(&zdx_home_agents),
            "Should include ZDX_HOME/AGENTS.md"
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
            "Should include root/AGENTS.md, got: {:?}",
            paths
        );
    }

    #[test]
    fn test_collect_agents_paths_deduplicates() {
        // If root is ZDX_HOME, should not have duplicates
        let zdx_home = paths::zdx_home();
        let paths = collect_agents_paths(&zdx_home);

        // Count occurrences of ZDX_HOME/AGENTS.md
        let zdx_agents = zdx_home.join("AGENTS.md");
        let count = paths
            .iter()
            .filter(|p| {
                p.canonicalize().unwrap_or_else(|_| (*p).clone())
                    == zdx_agents
                        .canonicalize()
                        .unwrap_or_else(|_| zdx_agents.clone())
            })
            .count();
        assert!(count <= 1, "Should deduplicate paths");
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
        if let Some(home) = dirs::home_dir() {
            // Create a path that's conceptually under home
            // (we just verify the function produces ordered paths)
            let paths = collect_agents_paths(&home);

            // Should include ZDX_HOME first
            let zdx_home_agents = paths::zdx_home().join("AGENTS.md");
            assert_eq!(
                paths.first().map(|p| p.as_path()),
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
}
