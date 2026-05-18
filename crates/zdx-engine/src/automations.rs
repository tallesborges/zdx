//! Automation discovery and parsing.
//!
//! Automations are markdown files with YAML frontmatter and prompt body.
//!
//! There are two sources:
//! - [`AutomationSource::Bundled`] — embedded via `zdx_assets::bundled_automation_assets()`.
//!   Bundled automations are **manual-only by contract**: their frontmatter MUST NOT include
//!   a `schedule` field. Parsing rejects scheduled bundled assets so the daemon never silently
//!   runs them.
//! - [`AutomationSource::User`] — markdown files under `<ZDX_HOME>/automations/`.
//!
//! When both sources define an automation with the same file stem, the user definition
//! shadows the bundled one. Duplicate names within the same source are an error.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, Local, Timelike};
use serde::Deserialize;
use zdx_assets::{BundledAutomationAsset, bundled_automation_assets};

use crate::config::paths;

/// Source location for an automation definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationSource {
    /// Embedded in the `zdx-assets` crate (manual-only).
    Bundled,
    /// `<ZDX_HOME>/automations/*.md`
    User,
}

impl AutomationSource {
    /// Returns a short display label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
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
    /// Optional named subagent for this automation run.
    pub subagent: Option<String>,
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
    subagent: Option<String>,
    timeout_secs: Option<u32>,
    max_retries: Option<u32>,
}

/// Discovers and parses automations from bundled assets and the user-global directory.
///
/// Scans:
/// - Embedded bundled automations in `zdx-assets` (manual-only).
/// - `<ZDX_HOME>/automations/*.md`
///
/// User definitions shadow bundled ones with the same file stem.
///
/// # Errors
/// Returns an error if parsing fails, duplicate names exist within a single source, or I/O fails.
pub fn discover(root: &Path) -> Result<Vec<AutomationDefinition>> {
    let user_dir = paths::zdx_home().join("automations");
    discover_with_user_dir(root, &user_dir)
}

/// Discovers and parses automations with an explicit user automations directory.
///
/// Intended for testing without env-var mutation.
///
/// # Errors
/// Returns an error if parsing fails, duplicate names exist within a single source, or I/O fails.
pub fn discover_with_user_dir(root: &Path, user_dir: &Path) -> Result<Vec<AutomationDefinition>> {
    discover_with_sources(bundled_automation_assets(), root, user_dir)
}

/// Discovers and parses automations from explicit bundled assets and user directory.
///
/// Intended for tests that need to inject a custom bundled-asset slice.
///
/// # Errors
/// Returns an error if parsing fails, duplicate names exist within a single source, or I/O fails.
pub fn discover_with_sources(
    bundled: &[BundledAutomationAsset],
    root: &Path,
    user_dir: &Path,
) -> Result<Vec<AutomationDefinition>> {
    let _ = root;
    let mut by_name: BTreeMap<String, AutomationDefinition> = BTreeMap::new();

    // Bundled first — same-source collisions still bail.
    for asset in bundled {
        // Defensive: `build.rs` embeds every file under `bundled_automations/`. Skip non-`.md`
        // files so future sidecars (README, references, scripts) cannot accidentally surface
        // as malformed automations.
        if !asset
            .relative_path
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("md"))
        {
            continue;
        }

        let definition = parse_bundled_automation(asset.relative_path, asset.bytes)
            .with_context(|| format!("parse bundled automation {}", asset.relative_path))?;

        if let Some(existing) = by_name.get(&definition.name)
            && existing.source == AutomationSource::Bundled
        {
            bail!(
                "Duplicate bundled automation name '{}': '{}' and '{}'",
                definition.name,
                existing.path.display(),
                definition.path.display()
            );
        }
        by_name.insert(definition.name.clone(), definition);
    }

    // User next — overwrites bundled with same name; duplicate user files bail.
    let mut user_entries: Vec<PathBuf> = Vec::new();
    collect_markdown_files(user_dir, &mut user_entries)?;

    for path in user_entries {
        let definition = parse_automation_file(&path, AutomationSource::User)
            .with_context(|| format!("parse automation {}", path.display()))?;

        if let Some(existing) = by_name.get(&definition.name)
            && existing.source == AutomationSource::User
        {
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
        .ok_or_else(|| anyhow::anyhow!("Automation '{trimmed}' not found"))
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
///
/// # Panics
/// Panics only if chrono returns invalid field values that do not fit in `i32`.
pub fn schedule_matches_local_time(schedule: &str, now: DateTime<Local>) -> Result<bool> {
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        bail!(
            "Invalid schedule '{schedule}': expected 5 cron fields (minute hour day month weekday)"
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

fn collect_markdown_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("read automation dir {}", dir.display()))?
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
        out.push(path);
    }
    Ok(())
}

fn parse_automation_file(path: &Path, source: AutomationSource) -> Result<AutomationDefinition> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read automation file {}", path.display()))?;
    let name = file_stem(path)?;
    parse_automation_content(&name, path.to_path_buf(), &content, source)
}

fn parse_bundled_automation(relative_path: &str, bytes: &[u8]) -> Result<AutomationDefinition> {
    let content = std::str::from_utf8(bytes)
        .with_context(|| format!("bundled automation {relative_path} is not valid UTF-8"))?;

    // Synthesize a display path so error messages and listings have a stable identity.
    let display_path = PathBuf::from(format!("<bundled>/{relative_path}"));
    let name = file_stem(Path::new(relative_path))?;

    let definition =
        parse_automation_content(&name, display_path, content, AutomationSource::Bundled)?;

    if definition.schedule.is_some() {
        bail!(
            "Bundled automation '{}' must not declare a schedule. Bundled automations are \
             manual-only by contract; copy it to $ZDX_HOME/automations/{}.md to add a schedule.",
            definition.name,
            definition.name
        );
    }

    Ok(definition)
}

fn parse_automation_content(
    name: &str,
    path: PathBuf,
    content: &str,
    source: AutomationSource,
) -> Result<AutomationDefinition> {
    let (yaml, body) = split_frontmatter(content)?;

    let frontmatter: AutomationFrontmatter = if yaml.trim().is_empty() {
        AutomationFrontmatter::default()
    } else {
        serde_yaml::from_str(&yaml)
            .with_context(|| format!("parse YAML frontmatter in {}", path.display()))?
    };

    let schedule = normalize_optional_string(frontmatter.schedule, "schedule")?;
    let model = normalize_optional_string(frontmatter.model, "model")?;
    let subagent = normalize_optional_string(frontmatter.subagent, "subagent")?;

    if matches!(frontmatter.timeout_secs, Some(0)) {
        bail!("timeout_secs must be greater than zero");
    }

    let prompt = body.trim().to_string();
    if prompt.is_empty() {
        bail!("Automation prompt body cannot be empty");
    }

    Ok(AutomationDefinition {
        name: name.to_string(),
        path,
        source,
        schedule,
        model,
        subagent,
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
                bail!("Invalid step '{part}': must be greater than zero");
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
                bail!("Invalid range '{part}': start must be <= end");
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
        bail!("Cron value '{trimmed}' out of range {min}..{max}");
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
        assert!(parsed.subagent.is_none());
        assert_eq!(parsed.max_retries, 0);
        assert_eq!(
            parsed.prompt,
            "Generate morning report from recent threads."
        );
    }

    #[test]
    fn parse_automation_with_subagent() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("morning-report.md");
        fs::write(
            &file,
            "---\nsubagent: task\n---\nGenerate morning report from recent threads.",
        )
        .unwrap();

        let parsed = parse_automation_file(&file, AutomationSource::User).unwrap();
        assert_eq!(parsed.subagent.as_deref(), Some("task"));
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

        let all = discover_with_sources(&[], root.path(), &user_dir).unwrap();
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

        let all = discover_with_sources(&[], root.path(), &user_dir).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "user-only");
    }

    #[test]
    fn cron_schedule_matches_expected_local_time() {
        let LocalResult::Single(dt) = Local.with_ymd_and_hms(2026, 2, 11, 8, 0, 0) else {
            panic!("expected unambiguous local datetime");
        };

        assert!(schedule_matches_local_time("0 8 * * *", dt).unwrap());
        assert!(!schedule_matches_local_time("1 8 * * *", dt).unwrap());
    }

    #[test]
    fn cron_schedule_supports_steps_and_ranges() {
        let LocalResult::Single(dt) = Local.with_ymd_and_hms(2026, 2, 11, 8, 30, 0) else {
            panic!("expected unambiguous local datetime");
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

    // ---- Bundled automations ----

    fn bundled_asset(relative_path: &'static str, bytes: &'static [u8]) -> BundledAutomationAsset {
        BundledAutomationAsset {
            relative_path,
            bytes,
        }
    }

    #[test]
    fn bundled_automation_is_discovered_without_user_dir() {
        let root = tempdir().unwrap();
        let user = tempdir().unwrap();
        let user_dir = user.path().join("automations");

        let assets = [bundled_asset(
            "memory-curator.md",
            b"---\n---\nReview recent threads and propose memory items.",
        )];

        let all = discover_with_sources(&assets, root.path(), &user_dir).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "memory-curator");
        assert_eq!(all[0].source, AutomationSource::Bundled);
        assert!(all[0].schedule.is_none());
        assert_eq!(all[0].path, PathBuf::from("<bundled>/memory-curator.md"));
    }

    #[test]
    fn user_definition_shadows_bundled_with_same_stem() {
        let root = tempdir().unwrap();
        let user = tempdir().unwrap();
        let user_dir = user.path().join("automations");
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(
            user_dir.join("memory-curator.md"),
            "---\nschedule: \"0 8 * * *\"\n---\nMy custom curator prompt body.",
        )
        .unwrap();

        let assets = [bundled_asset(
            "memory-curator.md",
            b"---\n---\nBundled curator body.",
        )];

        let all = discover_with_sources(&assets, root.path(), &user_dir).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "memory-curator");
        assert_eq!(all[0].source, AutomationSource::User);
        assert_eq!(all[0].schedule.as_deref(), Some("0 8 * * *"));
        assert!(all[0].prompt.contains("My custom curator prompt"));
    }

    #[test]
    fn bundled_with_schedule_is_rejected() {
        let root = tempdir().unwrap();
        let user = tempdir().unwrap();
        let user_dir = user.path().join("automations");

        let assets = [bundled_asset(
            "scheduled.md",
            b"---\nschedule: \"0 8 * * *\"\n---\nNope.",
        )];

        let err = discover_with_sources(&assets, root.path(), &user_dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("manual-only"),
            "expected manual-only error, got: {msg}"
        );
        assert!(msg.contains("scheduled"), "expected name in error: {msg}");
    }

    #[test]
    fn bundled_invalid_utf8_bails_with_path_context() {
        let root = tempdir().unwrap();
        let user = tempdir().unwrap();
        let user_dir = user.path().join("automations");

        let assets = [bundled_asset("broken.md", b"\xff\xfe not utf8")];

        let err = discover_with_sources(&assets, root.path(), &user_dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("broken.md"),
            "expected relative path in error: {msg}"
        );
    }

    #[test]
    fn duplicate_bundled_names_bail() {
        let root = tempdir().unwrap();
        let user = tempdir().unwrap();
        let user_dir = user.path().join("automations");

        let assets = [
            bundled_asset("memory-curator.md", b"---\n---\nFirst."),
            bundled_asset("memory-curator.md", b"---\n---\nSecond."),
        ];

        let err = discover_with_sources(&assets, root.path(), &user_dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Duplicate bundled automation"),
            "expected duplicate-bundled error: {msg}"
        );
    }

    #[test]
    fn bundled_non_markdown_assets_are_skipped() {
        let root = tempdir().unwrap();
        let user = tempdir().unwrap();
        let user_dir = user.path().join("automations");

        // Simulate a future sidecar file shipped under bundled_automations/ — a README,
        // a script, or anything that is not a `.md` automation. Discovery must skip it
        // rather than fail trying to parse it as automation YAML+body.
        let assets = [
            bundled_asset("README.txt", b"This is not an automation."),
            bundled_asset("helpers/util.py", b"print('helper')"),
            bundled_asset("memory-curator.md", b"---\n---\nReal automation prompt."),
        ];

        let all = discover_with_sources(&assets, root.path(), &user_dir).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "memory-curator");
        assert_eq!(all[0].source, AutomationSource::Bundled);
    }

    #[test]
    fn shipped_memory_curator_bundled_parses() {
        let root = tempdir().unwrap();
        let user = tempdir().unwrap();
        let user_dir = user.path().join("automations");

        let all = discover_with_sources(
            zdx_assets::bundled_automation_assets(),
            root.path(),
            &user_dir,
        )
        .unwrap();

        let curator = all
            .iter()
            .find(|a| a.name == "memory-curator")
            .expect("shipped memory-curator bundled automation should be present");

        assert_eq!(curator.source, AutomationSource::Bundled);
        assert!(curator.schedule.is_none(), "must be manual-only");
        assert!(!curator.prompt.is_empty());
        assert!(curator.prompt.contains("Thread_Search"));
        assert!(curator.prompt.contains("memory_suggestions"));
    }
}
