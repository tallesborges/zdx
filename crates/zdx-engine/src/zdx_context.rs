//! Shared `{{ZDX_CONTEXT}}` block for helper subagents (handoff, tldr,
//! prompt-builder, `Read_Thread`) which run with `no_system_prompt: true`
//! and would otherwise have no awareness of installed subagents/skills,
//! the user's memory index, or project conventions.
//!
//! Custom commands are intentionally excluded: they expand client-side
//! into a user message, so the assistant cannot invoke them.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::core::context::{
    MAX_AGENTS_FILE_SIZE, ScopedContextFile, discover_scoped_context, load_all_agents_files,
};
use crate::skills::{LoadSkillsOptions, load_skills};
use crate::subagents;

/// Returns the shared `{{ZDX_CONTEXT}}` block, or an empty string when no
/// section could be assembled.
#[must_use]
pub fn build_zdx_context(root: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(section) = build_manifest_section(root) {
        parts.push(section);
    }
    if let Some(section) = build_memory_section() {
        parts.push(section);
    }
    if let Some(section) = build_instructions_section(root) {
        parts.push(section);
    }

    parts.join("\n\n")
}

fn build_manifest_section(root: &Path) -> Option<String> {
    let subagents = subagents::discover(root).unwrap_or_default();
    let skills = load_skills(&LoadSkillsOptions::new(root)).skills;

    if subagents.is_empty() && skills.is_empty() {
        return None;
    }

    let mut out = String::from("## Project context\n");

    if !subagents.is_empty() {
        out.push_str("\nSubagents:\n");
        for sub in &subagents {
            let _ = writeln!(out, "- {} — {}", sub.name, first_line(&sub.description));
        }
    }
    if !skills.is_empty() {
        out.push_str("\nSkills:\n");
        for skill in &skills {
            let _ = writeln!(out, "- {} — {}", skill.name, first_line(&skill.description));
        }
    }

    Some(out.trim_end().to_string())
}

fn build_memory_section() -> Option<String> {
    let memory_root = std::env::var("ZDX_MEMORY_ROOT").ok()?;
    let path = PathBuf::from(memory_root).join("Notes").join("MEMORY.md");
    let body = std::fs::read_to_string(&path).ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!("## Memory index\n\n{trimmed}"))
}

fn build_instructions_section(root: &Path) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();

    if let Some(loaded) = load_all_agents_files(root) {
        let body = loaded.content.trim();
        if !body.is_empty() {
            sections.push(body.to_string());
        }
    }

    for scoped in discover_scoped_context(root) {
        if let Some(section) = render_scoped_section(&scoped) {
            sections.push(section);
        }
    }

    if sections.is_empty() {
        return None;
    }

    Some(format!(
        "## Project instructions\n\n{}",
        sections.join("\n\n")
    ))
}

fn render_scoped_section(scoped: &ScopedContextFile) -> Option<String> {
    let bytes = std::fs::read(&scoped.path).ok()?;
    let cap = MAX_AGENTS_FILE_SIZE.min(bytes.len());
    let body = String::from_utf8_lossy(&bytes[..cap]);
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!(
        "### {} ({})\n\n{trimmed}",
        scoped.path.display(),
        scoped.scope
    ))
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_handles_multiline() {
        assert_eq!(first_line("hello\nworld"), "hello");
        assert_eq!(first_line("  spaced  \nrest"), "spaced");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn returns_empty_when_no_context_available() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_memory = std::env::var("ZDX_MEMORY_ROOT").ok();
        // SAFETY: tests in this module do not run concurrently with code
        // reading ZDX_MEMORY_ROOT; the previous value is restored below.
        unsafe {
            std::env::set_var("ZDX_MEMORY_ROOT", tmp.path());
        }
        let result = build_zdx_context(tmp.path());
        unsafe {
            match prev_memory {
                Some(v) => std::env::set_var("ZDX_MEMORY_ROOT", v),
                None => std::env::remove_var("ZDX_MEMORY_ROOT"),
            }
        }
        // Built-in subagents always exist, so the manifest section is
        // present even when memory + AGENTS.md sources are empty.
        assert!(result.contains("## Project context"));
        assert!(result.contains("Subagents:"));
    }
}
