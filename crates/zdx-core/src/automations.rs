//! Automation discovery and parsing.
//!
//! Automations are markdown files with YAML frontmatter and prompt body.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, Local, Timelike};
use serde::Deserialize;

use crate::config::paths;

/// Source location for an automation definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationSource {
    /// `<ZDX_HOME>/automations/*.md`
    User,
}

impl AutomationSource {
    /// Returns a short display label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
        }
    }
}

/// Parsed automation definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationDefinition {
    /// Stable automation name (file stem).
    pub name: String,
    /// Absolute path to the source markdown file.
    pub path: PathBuf,
    /// Which directory this automation came from.
    pub source: AutomationSource,
    /// Optional schedule expression.
    pub schedule: Option<String>,
    /// Optional model override for this automation run.
    pub model: Option<String>,
    /// Optional per-run timeout in seconds.
    pub timeout_secs: Option<u32>,
    /// Number of retries after the initial attempt.
    pub max_retries: u32,
    /// Prompt body (markdown content after frontmatter).
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct AutomationFrontmatter {
    schedule: Option<String>,
    model: Option<String>,
    timeout_secs: Option<u32>,
    max_retries: Option<u32>,
}

/// Discovers and parses automations from user-global directory.
///
/// Scans:
/// - `<ZDX_HOME>/automations/*.md`
///
/// # Errors
/// Returns an error if parsing fails, duplicate names are found, or I/O fails.
pub fn discover(root: &Path) -> Result<Vec<AutomationDefinition>> {
    let user_dir = paths::zdx_home().join("automations");
    discover_with_user_dir(root, &user_dir)
}

/// Discovers and parses automations with an explicit user automations directory.
///
/// Intended for testing without env-var mutation.
///
/// # Errors
/// Returns an error if parsing fails, duplicate names are found, or I/O fails.
pub fn discover_with_user_dir(root: &Path, user_dir: &Path) -> Result<Vec<AutomationDefinition>> {
    let _ = root;
    let mut entries: Vec<(PathBuf, AutomationSource)> = Vec::new();
    collect_markdown_files(user_dir, AutomationSource::User, &mut entries)?;

    let mut by_name: BTreeMap<String, AutomationDefinition> = BTreeMap::new();
    for (path, source) in entries {
        let definition = parse_automation_file(&path, source)
            .with_context(|| format!("parse automation {}", path.display()))?;

        if let Some(existing) = by_name.get(&definition.name) {
            bail!(
                "Duplicate automation name '{}': '{}' and '{}'",
                definition.name,
                existing.path.display(),
                definition.path.display()
            );
        }

        by_name.insert(definition.name.clone(), definition);
    }

    Ok(by_name.into_values().collect())
}

/// Loads one automation by file-stem name.
///
/// # Errors
/// Returns an error if discovery/parsing fails or the name is not found.
pub fn load_by_name(root: &Path, name: &str) -> Result<AutomationDefinition> {
    let user_dir = paths::zdx_home().join("automations");
    load_by_name_with_user_dir(root, &user_dir, name)
}

/// Loads one automation by name with explicit user automations directory.
///
/// # Errors
/// Returns an error if discovery/parsing fails or the name is not found.
pub fn load_by_name_with_user_dir(
    root: &Path,
    user_dir: &Path,
    name: &str,
) -> Result<AutomationDefinition> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("Automation name cannot be empty");
    }

    let all = discover_with_user_dir(root, user_dir)?;
    all.into_iter()
        .find(|a| a.name == trimmed)
        .ok_or_else(|| anyhow::anyhow!("Automation '{}' not found", trimmed))
}

/// Returns whether a 5-field cron schedule matches the provided local time.
///
/// Supported field forms per position:
/// - `*`
/// - `N`
/// - `*/N`
/// - `A-B`
/// - comma-separated combinations of the above
///
/// Cron fields order: minute hour day-of-month month day-of-week.
/// Day-of-week accepts 0-6 (Sun-Sat), and 7 as Sunday.
///
/// # Errors
/// Returns an error for invalid cron syntax/ranges.
pub fn schedule_matches_local_time(schedule: &str, now: DateTime<Local>) -> Result<bool> {
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        bail!(
            "Invalid schedule '{}': expected 5 cron fields (minute hour day month weekday)",
            schedule
        );
    }

    let minute = i32::try_from(now.minute()).expect("minute in i32 range");
    let hour = i32::try_from(now.hour()).expect("hour in i32 range");
    let day = i32::try_from(now.day()).expect("day in i32 range");
    let month = i32::try_from(now.month()).expect("month in i32 range");
    let weekday = i32::try_from(now.weekday().num_days_from_sunday()).expect("weekday in range");

    Ok(field_matches(fields[0], minute, 0, 59, false)?
        && field_matches(fields[1], hour, 0, 23, false)?
        && field_matches(fields[2], day, 1, 31, false)?
        && field_matches(fields[3], month, 1, 12, false)?
        && field_matches(fields[4], weekday, 0, 6, true)?)
}

fn collect_markdown_files(
    dir: &Path,
    source: AutomationSource,
    out: &mut Vec<(PathBuf, AutomationSource)>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("read automation dir {}", dir.display()))?
        .filter_map(|entry| entry.ok())
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

fn parse_automation_file(path: &Path, source: AutomationSource) -> Result<AutomationDefinition> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read automation file {}", path.display()))?;
    let (yaml, body) = split_frontmatter(&content)?;

    let frontmatter: AutomationFrontmatter = if yaml.trim().is_empty() {
        AutomationFrontmatter::default()
    } else {
        serde_yaml::from_str(&yaml)
            .with_context(|| format!("parse YAML frontmatter in {}", path.display()))?
    };

    let name = file_stem(path)?;
    let schedule = normalize_optional_string(frontmatter.schedule, "schedule")?;
    let model = normalize_optional_string(frontmatter.model, "model")?;

    if matches!(frontmatter.timeout_secs, Some(0)) {
        bail!("timeout_secs must be greater than zero");
    }

    let prompt = body.trim().to_string();
    if prompt.is_empty() {
        bail!("Automation prompt body cannot be empty");
    }

    Ok(AutomationDefinition {
        name,
        path: path.to_path_buf(),
        source,
        schedule,
        model,
        timeout_secs: frontmatter.timeout_secs,
        max_retries: frontmatter.max_retries.unwrap_or(0),
        prompt,
    })
}

fn file_stem(path: &Path) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Invalid automation file name: {}", path.display()))?;
    Ok(stem.to_string())
}

fn normalize_optional_string(value: Option<String>, field: &str) -> Result<Option<String>> {
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

fn field_matches(expr: &str, value: i32, min: i32, max: i32, is_dow: bool) -> Result<bool> {
    for part in expr.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if part == "*" {
            return Ok(true);
        }

        if let Some(step_str) = part.strip_prefix("*/") {
            let step = parse_value(step_str, min, max, is_dow)?;
            if step <= 0 {
                bail!("Invalid step '{}': must be greater than zero", part);
            }
            if (value - min) % step == 0 {
                return Ok(true);
            }
            continue;
        }

        if let Some((start_str, end_str)) = part.split_once('-') {
            let start = parse_value(start_str, min, max, is_dow)?;
            let end = parse_value(end_str, min, max, is_dow)?;
            if start > end {
                bail!("Invalid range '{}': start must be <= end", part);
            }
            if (start..=end).contains(&value) {
                return Ok(true);
            }
            continue;
        }

        let n = parse_value(part, min, max, is_dow)?;
        if n == value {
            return Ok(true);
        }
    }

    Ok(false)
}

fn parse_value(raw: &str, min: i32, max: i32, is_dow: bool) -> Result<i32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("Invalid empty cron value");
    }

    let mut n = trimmed
        .parse::<i32>()
        .with_context(|| format!("Invalid cron value '{trimmed}'"))?;

    // Allow 7 as Sunday for day-of-week.
    if is_dow && n == 7 {
        n = 0;
    }

    if !(min..=max).contains(&n) {
        bail!("Cron value '{}' out of range {}..{}", trimmed, min, max);
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use chrono::{LocalResult, TimeZone};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parse_minimal_automation_defaults() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("morning-report.md");
        fs::write(
            &file,
            "---\n---\nGenerate morning report from recent threads.",
        )
        .unwrap();

        let parsed = parse_automation_file(&file, AutomationSource::User).unwrap();
        assert_eq!(parsed.name, "morning-report");
        assert!(parsed.schedule.is_none());
        assert!(parsed.model.is_none());
        assert_eq!(parsed.max_retries, 0);
        assert_eq!(
            parsed.prompt,
            "Generate morning report from recent threads."
        );
    }

    #[test]
    fn parse_requires_frontmatter() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("invalid.md");
        fs::write(&file, "no frontmatter").unwrap();

        let err = parse_automation_file(&file, AutomationSource::User).unwrap_err();
        assert!(err.to_string().contains("Missing YAML frontmatter"));
    }

    #[test]
    fn discover_reads_user_automations() {
        let root = tempdir().unwrap();
        let user = tempdir().unwrap();

        let user_dir = user.path().join("automations");
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(user_dir.join("user-report.md"), "---\n---\nuser prompt").unwrap();

        let all = discover_with_user_dir(root.path(), &user_dir).unwrap();
        assert_eq!(all.len(), 1);
        assert!(all.iter().any(|a| a.name == "user-report"));
    }

    #[test]
    fn discover_ignores_project_automations_directory() {
        let root = tempdir().unwrap();
        let user = tempdir().unwrap();

        let project_dir = root.path().join(".zdx").join("automations");
        let user_dir = user.path().join("automations");
        fs::create_dir_all(&project_dir).unwrap();
        fs::create_dir_all(&user_dir).unwrap();

        fs::write(
            project_dir.join("project-only.md"),
            "---\n---\nproject prompt",
        )
        .unwrap();
        fs::write(user_dir.join("user-only.md"), "---\n---\nuser prompt").unwrap();

        let all = discover_with_user_dir(root.path(), &user_dir).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "user-only");
    }

    #[test]
    fn cron_schedule_matches_expected_local_time() {
        let dt = match Local.with_ymd_and_hms(2026, 2, 11, 8, 0, 0) {
            LocalResult::Single(v) => v,
            _ => panic!("expected unambiguous local datetime"),
        };

        assert!(schedule_matches_local_time("0 8 * * *", dt).unwrap());
        assert!(!schedule_matches_local_time("1 8 * * *", dt).unwrap());
    }

    #[test]
    fn cron_schedule_supports_steps_and_ranges() {
        let dt = match Local.with_ymd_and_hms(2026, 2, 11, 8, 30, 0) {
            LocalResult::Single(v) => v,
            _ => panic!("expected unambiguous local datetime"),
        };

        assert!(schedule_matches_local_time("*/15 8-10 * * *", dt).unwrap());
        assert!(!schedule_matches_local_time("*/20 8-10 * * *", dt).unwrap());
    }

    #[test]
    fn cron_invalid_field_count_is_rejected() {
        let dt = Local::now();
        let err = schedule_matches_local_time("0 8 * *", dt).unwrap_err();
        assert!(err.to_string().contains("expected 5 cron fields"));
    }
}
