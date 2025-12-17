//! Context module for loading project-specific guidelines.

use anyhow::Result;

use std::fs;
use std::io::Read;
use std::path::Path;

use crate::config::Config;

const AGENTS_MD_MAX_BYTES: usize = 50 * 1024;

/// Loads project context from AGENTS.md in the given root directory.
pub fn load_project_context(root: &Path) -> Option<String> {
    let agents_md = root.join("AGENTS.md");
    if !agents_md.exists() {
        return None;
    }

    let file = match fs::File::open(&agents_md) {
        Ok(file) => file,
        Err(e) => {
            eprintln!(
                "Warning: Failed to open AGENTS.md in {}: {}",
                root.display(),
                e
            );
            return None;
        }
    };

    let mut bytes = Vec::new();
    let mut limited = file.take((AGENTS_MD_MAX_BYTES + 1) as u64);
    if let Err(e) = limited.read_to_end(&mut bytes) {
        eprintln!(
            "Warning: Failed to read AGENTS.md in {}: {}",
            root.display(),
            e
        );
        return None;
    }

    let was_truncated = bytes.len() > AGENTS_MD_MAX_BYTES;
    if was_truncated {
        bytes.truncate(AGENTS_MD_MAX_BYTES);
        eprintln!(
            "Warning: AGENTS.md in {} is larger than {} bytes; truncating.",
            root.display(),
            AGENTS_MD_MAX_BYTES
        );
    }

    let content = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Warning: AGENTS.md in {} is not valid UTF-8: {}",
                root.display(),
                e
            );
            return None;
        }
    };

    let trimmed = content.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Builds the effective system prompt by combining config and AGENTS.md.
pub fn build_effective_system_prompt(config: &Config, root: &Path) -> Result<Option<String>> {
    let mut system_prompt = config.effective_system_prompt()?;

    // Auto-include AGENTS.md if present
    if let Some(project_context) = load_project_context(root) {
        let combined = match system_prompt {
            Some(sp) => format!("{}\n\n# Project Guidelines\n\n{}", sp, project_context),
            None => format!("# Project Guidelines\n\n{}", project_context),
        };
        system_prompt = Some(combined);
    }
    Ok(system_prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

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
    fn test_load_project_context_truncates_large_files() {
        let dir = tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");
        fs::write(&agents_md, "a".repeat(AGENTS_MD_MAX_BYTES + 10)).unwrap();

        let context = load_project_context(dir.path()).unwrap();
        assert_eq!(context.len(), AGENTS_MD_MAX_BYTES);
    }
}
