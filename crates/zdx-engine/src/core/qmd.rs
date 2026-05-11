//! qmd binary discovery and setup.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{self, MemoryConfig, QmdConfig};

/// qmd package installed by the supported Node/Bun installers.
pub const QMD_PACKAGE: &str = "@tobilu/qmd";

/// qmd collection used for exported ZDX thread transcripts.
pub const THREAD_COLLECTION_NAME: &str = "zdx-threads";

/// File pattern qmd should index inside the thread transcript export directory.
pub const THREAD_COLLECTION_PATTERN: &str = "**/*.md";

/// qmd collection used for canonical Second Brain notes.
pub const NOTES_COLLECTION_NAME: &str = "zdx-notes";

/// qmd collection used for canonical calendar notes.
pub const CALENDAR_COLLECTION_NAME: &str = "zdx-calendar";

/// File pattern qmd should index inside memory Markdown directories.
pub const MEMORY_MARKDOWN_COLLECTION_PATTERN: &str = "**/*.md";

const MEMORY_COLLECTION_IGNORE_PATTERNS: &[&str] =
    &["@Archive/**", "**/@Archive/**", "@Trash/**", "**/@Trash/**"];

const THREAD_COLLECTION_CONTEXT: &str = "ZDX saved conversation thread transcripts exported from canonical JSONL. Each Markdown file is one thread. Search hits should be deep-read by qmd docid before answering; use Read_Thread only when a thread ID is already known.";
const NOTES_COLLECTION_CONTEXT: &str = "Canonical ZDX personal notes from the user's NotePlan Notes directory. Search hits should be deep-read by qmd docid before answering.";
const CALENDAR_COLLECTION_CONTEXT: &str = "Canonical ZDX calendar and daily notes from the user's NotePlan Calendar directory. Search hits should be deep-read by qmd docid before answering.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QmdBinary {
    /// Resolved executable path.
    pub path: PathBuf,
    /// Whether this call installed qmd before resolving it.
    pub installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QmdMemoryIndexSummary {
    /// Resolved executable path.
    pub binary_path: PathBuf,
    /// Whether this call installed qmd before resolving it.
    pub installed: bool,
    /// Thread export directory registered with qmd.
    pub export_dir: PathBuf,
    /// Whether the qmd collection was created during this call.
    pub collection_added: bool,
    /// All ZDX memory collections registered/updated during this call.
    pub collections: Vec<QmdMemoryCollectionIndexSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QmdMemoryCollectionIndexSummary {
    pub name: String,
    pub source: String,
    pub root_dir: PathBuf,
    pub collection_added: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QmdMemorySearchOptions {
    pub query: String,
    pub limit: usize,
    pub exclude_thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QmdMemorySearchOutput {
    pub results: Vec<QmdMemorySearchResult>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QmdMemorySearchResult {
    pub docid: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct QmdSearchJsonResult {
    docid: String,
    file: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    snippet: Option<String>,
    #[serde(default)]
    score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QmdMemoryGetOutput {
    pub docid: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QmdCollectionInfo {
    path: PathBuf,
    pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QmdMemoryCollectionDef {
    name: &'static str,
    source: &'static str,
    root_dir: PathBuf,
    pattern: &'static str,
    context: &'static str,
    ignore_patterns: &'static [&'static str],
}

/// Finds qmd using the configured command path/name.
#[must_use]
pub fn find_qmd_binary(config: &QmdConfig) -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    find_command(&config.command, path_env.as_deref())
}

/// Ensures qmd is available, installing `@tobilu/qmd` with npm or bun when needed.
///
/// # Errors
/// Returns an error when a configured explicit path is missing, no supported installer exists,
/// the installer fails, or qmd is still unavailable after installation.
pub fn ensure_qmd_binary(config: &QmdConfig) -> Result<QmdBinary> {
    let path_env = std::env::var_os("PATH");
    ensure_qmd_binary_with_path(config, path_env.as_deref())
}

/// Registers/updates qmd collections for ZDX memory.
///
/// # Errors
/// Returns an error when qmd is unavailable, the collection points at a different path
/// or pattern, or qmd indexing/embedding fails.
pub fn index_memory_collections(
    qmd_config: &QmdConfig,
    memory_config: &MemoryConfig,
) -> Result<QmdMemoryIndexSummary> {
    let binary = ensure_qmd_binary(qmd_config)?;
    index_memory_collections_with_binary(binary, memory_config)
}

/// Searches the qmd index for ZDX memory and maps hits to stable memory refs.
///
/// # Errors
/// Returns an error when qmd is unavailable, qmd search fails, or JSON output cannot be parsed.
pub fn search_memory_collections(
    qmd_config: &QmdConfig,
    memory_config: &MemoryConfig,
    options: &QmdMemorySearchOptions,
) -> Result<QmdMemorySearchOutput> {
    let binary = require_qmd_binary(qmd_config)?;
    search_memory_collections_with_binary(&binary, memory_config, options)
}

/// Reads an indexed qmd memory document by docid.
///
/// # Errors
/// Returns an error when qmd is unavailable or qmd cannot retrieve the docid.
pub fn get_memory_doc(qmd_config: &QmdConfig, docid: &str) -> Result<QmdMemoryGetOutput> {
    let docid = docid.trim();
    if docid.is_empty() {
        bail!("qmd memory docid cannot be empty");
    }

    let binary = require_qmd_binary(qmd_config)?;
    get_memory_doc_with_binary(&binary, docid)
}

fn get_memory_doc_with_binary(binary: &QmdBinary, docid: &str) -> Result<QmdMemoryGetOutput> {
    let output =
        run_qmd_command_allow_failure(binary, [OsString::from("get"), OsString::from(docid)])?;

    if !output.status.success() {
        return bail_qmd_failure(binary, ["get", docid], &output);
    }

    Ok(QmdMemoryGetOutput {
        docid: docid.to_string(),
        content: String::from_utf8_lossy(&output.stdout).to_string(),
    })
}

fn search_memory_collections_with_binary(
    binary: &QmdBinary,
    memory_config: &MemoryConfig,
    options: &QmdMemorySearchOptions,
) -> Result<QmdMemorySearchOutput> {
    let query = options.query.trim();
    if query.is_empty() {
        bail!("qmd memory search query cannot be empty");
    }

    let collections = memory_collection_defs(memory_config);
    let mut active_collections = Vec::new();
    let mut warnings = Vec::new();
    for collection in &collections {
        match qmd_collection_info(binary, collection.name)? {
            Some(_) => active_collections.push(collection.name),
            None if collection.name == THREAD_COLLECTION_NAME => {
                bail!(
                    "qmd collection '{THREAD_COLLECTION_NAME}' was not found; run `zdx memory index` to set up qmd first"
                );
            }
            None => warnings.push(format!(
                "qmd collection '{}' is not indexed yet; run `zdx memory index` to include {} memory",
                collection.name, collection.source
            )),
        }
    }

    let limit = options.limit.max(1).to_string();
    let mut args = vec![
        OsString::from("search"),
        OsString::from(query),
        OsString::from("--json"),
        OsString::from("-n"),
        OsString::from(limit),
    ];
    for collection_name in active_collections {
        args.push(OsString::from("-c"));
        args.push(OsString::from(collection_name));
    }

    let output = run_qmd_command_allow_failure(binary, args)?;

    if !output.status.success() {
        return bail_qmd_failure(binary, ["search", query, "--json"], &output);
    }

    let mut parsed = parse_qmd_memory_search_output(
        &String::from_utf8_lossy(&output.stdout),
        &collections,
        options.exclude_thread_id.as_deref(),
    )?;
    warnings.append(&mut parsed.warnings);
    parsed.warnings = warnings;
    Ok(parsed)
}

fn require_qmd_binary(config: &QmdConfig) -> Result<QmdBinary> {
    let path_env = std::env::var_os("PATH");
    if let Some(path) = find_command(&config.command, path_env.as_deref()) {
        return Ok(QmdBinary {
            path,
            installed: false,
        });
    }

    bail!(
        "qmd command '{}' was not found or is not executable; run `zdx memory index` to set up qmd first",
        config.command
    )
}

fn parse_qmd_memory_search_output(
    output: &str,
    collections: &[QmdMemoryCollectionDef],
    exclude_thread_id: Option<&str>,
) -> Result<QmdMemorySearchOutput> {
    let raw_results: Vec<QmdSearchJsonResult> = serde_json::from_str(output)
        .with_context(|| "parse qmd search JSON output; expected an array of results")?;
    let mut results = Vec::with_capacity(raw_results.len());
    let mut warnings = Vec::new();

    for raw in raw_results {
        let Some(mapped) = memory_result_from_qmd_file(&raw.file, collections) else {
            warnings.push(format!(
                "ignored qmd result outside ZDX memory collections: {}",
                raw.file
            ));
            continue;
        };
        if mapped.is_excluded_path {
            warnings.push(format!(
                "ignored qmd result under excluded memory path: {}",
                raw.file
            ));
            continue;
        }
        if exclude_thread_id.is_some_and(|excluded| mapped.thread_id.as_deref() == Some(excluded)) {
            continue;
        }

        results.push(QmdMemorySearchResult {
            docid: raw.docid,
            file: raw.file,
            title: raw.title,
            snippet: raw.snippet.unwrap_or_default(),
            score: raw.score,
        });
    }

    Ok(QmdMemorySearchOutput { results, warnings })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MappedMemoryResult {
    thread_id: Option<String>,
    is_excluded_path: bool,
}

fn memory_result_from_qmd_file(
    file: &str,
    collections: &[QmdMemoryCollectionDef],
) -> Option<MappedMemoryResult> {
    for collection in collections {
        let Some(relative_path) = file
            .strip_prefix(&format!("qmd://{}/", collection.name))
            .and_then(valid_qmd_uri_path)
        else {
            continue;
        };

        let is_excluded_path =
            collection.source != "thread" && is_archive_or_trash_path(relative_path);
        if collection.source == "thread" {
            let thread_id = relative_path
                .rsplit('/')
                .next()
                .map(|filename| filename.strip_suffix(".md").unwrap_or(filename))
                .filter(|stem| !stem.is_empty())
                .map(str::to_string)?;
            return Some(MappedMemoryResult {
                thread_id: Some(thread_id),
                is_excluded_path,
            });
        }

        return Some(MappedMemoryResult {
            thread_id: None,
            is_excluded_path,
        });
    }

    None
}

fn valid_qmd_uri_path(path: &str) -> Option<&str> {
    let path = path.trim_start_matches('/');
    if path.is_empty()
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return None;
    }
    Some(path)
}

fn is_archive_or_trash_path(path: &str) -> bool {
    path.split('/')
        .any(|component| component == "@Archive" || component == "@Trash")
}

fn index_memory_collections_with_binary(
    binary: QmdBinary,
    memory_config: &MemoryConfig,
) -> Result<QmdMemoryIndexSummary> {
    let mut indexed_collections = Vec::new();
    for collection in memory_collection_defs(memory_config) {
        fs::create_dir_all(&collection.root_dir)
            .with_context(|| format!("create {} memory directory", collection.source))?;
        let root_dir = fs::canonicalize(&collection.root_dir)
            .with_context(|| format!("resolve {} memory directory", collection.source))?;

        let existing = qmd_collection_info(&binary, collection.name)?;
        let collection_added = if let Some(info) = existing {
            if info.path != root_dir || info.pattern != collection.pattern {
                bail!(
                    "qmd collection '{name}' already exists for path '{}' with pattern '{}'; expected path '{}' with pattern '{}'. Remove or rename the existing collection with `qmd collection remove {name}` and try again.",
                    info.path.display(),
                    info.pattern,
                    root_dir.display(),
                    collection.pattern,
                    name = collection.name
                );
            }
            false
        } else {
            run_qmd_command(
                &binary,
                [
                    OsString::from("collection"),
                    OsString::from("add"),
                    root_dir.as_os_str().to_os_string(),
                    OsString::from("--name"),
                    OsString::from(collection.name),
                    OsString::from("--mask"),
                    OsString::from(collection.pattern),
                ],
            )
            .with_context(|| format!("create qmd {} collection", collection.source))?;
            true
        };

        ensure_qmd_collection_ignores(collection.name, collection.ignore_patterns).with_context(
            || format!("configure qmd ignores for collection '{}'", collection.name),
        )?;
        ensure_qmd_collection_context(&binary, collection.name, collection.context).with_context(
            || format!("configure qmd context for collection '{}'", collection.name),
        )?;

        indexed_collections.push(QmdMemoryCollectionIndexSummary {
            name: collection.name.to_string(),
            source: collection.source.to_string(),
            root_dir,
            collection_added,
        });
    }

    run_qmd_command(&binary, [OsString::from("update")]).context("update qmd index")?;
    for collection in &indexed_collections {
        run_qmd_command(
            &binary,
            [
                OsString::from("embed"),
                OsString::from("-c"),
                OsString::from(&collection.name),
            ],
        )
        .with_context(|| format!("embed qmd {} collection", collection.name))?;
    }

    let thread_collection = indexed_collections
        .iter()
        .find(|collection| collection.name == THREAD_COLLECTION_NAME)
        .context("missing thread collection summary")?;

    Ok(QmdMemoryIndexSummary {
        binary_path: binary.path,
        installed: binary.installed,
        export_dir: thread_collection.root_dir.clone(),
        collection_added: thread_collection.collection_added,
        collections: indexed_collections,
    })
}

fn memory_collection_defs(memory_config: &MemoryConfig) -> Vec<QmdMemoryCollectionDef> {
    vec![
        QmdMemoryCollectionDef {
            name: THREAD_COLLECTION_NAME,
            source: "thread",
            root_dir: config::paths::thread_exports_dir(),
            pattern: THREAD_COLLECTION_PATTERN,
            context: THREAD_COLLECTION_CONTEXT,
            ignore_patterns: &[],
        },
        QmdMemoryCollectionDef {
            name: NOTES_COLLECTION_NAME,
            source: "note",
            root_dir: memory_config.effective_notes_path(),
            pattern: MEMORY_MARKDOWN_COLLECTION_PATTERN,
            context: NOTES_COLLECTION_CONTEXT,
            ignore_patterns: MEMORY_COLLECTION_IGNORE_PATTERNS,
        },
        QmdMemoryCollectionDef {
            name: CALENDAR_COLLECTION_NAME,
            source: "calendar",
            root_dir: memory_config.effective_daily_path(),
            pattern: MEMORY_MARKDOWN_COLLECTION_PATTERN,
            context: CALENDAR_COLLECTION_CONTEXT,
            ignore_patterns: MEMORY_COLLECTION_IGNORE_PATTERNS,
        },
    ]
}

fn ensure_qmd_collection_context(binary: &QmdBinary, name: &str, context: &str) -> Result<()> {
    run_qmd_command(
        binary,
        [
            OsString::from("context"),
            OsString::from("add"),
            OsString::from(format!("qmd://{name}/")),
            OsString::from(context),
        ],
    )
}

fn ensure_qmd_collection_ignores(name: &str, ignore_patterns: &[&str]) -> Result<()> {
    if ignore_patterns.is_empty() {
        return Ok(());
    }

    let config_path = qmd_config_file_path()?;
    let content = fs::read_to_string(&config_path).unwrap_or_default();
    let mut yaml: serde_yaml::Value = if content.trim().is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        serde_yaml::from_str(&content)
            .with_context(|| format!("parse qmd config {}", config_path.display()))?
    };

    let root = yaml
        .as_mapping_mut()
        .context("qmd config root is not a mapping")?;
    let collections_field = serde_yaml::Value::String("collections".to_string());
    if !root.contains_key(&collections_field) {
        root.insert(
            collections_field.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    let collections = root
        .get_mut(&collections_field)
        .and_then(serde_yaml::Value::as_mapping_mut)
        .context("qmd config collections is not a mapping")?;
    let collection_name_field = serde_yaml::Value::String(name.to_string());
    if !collections.contains_key(&collection_name_field) {
        collections.insert(
            collection_name_field.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    let collection = collections
        .get_mut(&collection_name_field)
        .and_then(serde_yaml::Value::as_mapping_mut)
        .context("qmd collection config is not a mapping")?;
    collection.insert(
        serde_yaml::Value::String("ignore".to_string()),
        serde_yaml::Value::Sequence(
            ignore_patterns
                .iter()
                .map(|pattern| serde_yaml::Value::String((*pattern).to_string()))
                .collect(),
        ),
    );

    fs::write(
        &config_path,
        serde_yaml::to_string(&yaml).context("serialize qmd config")?,
    )
    .with_context(|| format!("write qmd config {}", config_path.display()))
}

fn qmd_config_file_path() -> Result<PathBuf> {
    let config_dir = if let Some(dir) = std::env::var_os("QMD_CONFIG_DIR") {
        PathBuf::from(dir)
    } else if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(dir).join("qmd")
    } else {
        config::paths::home_dir()
            .context("determine home directory for qmd config")?
            .join(".config")
            .join("qmd")
    };
    fs::create_dir_all(&config_dir)
        .with_context(|| format!("create qmd config directory {}", config_dir.display()))?;
    Ok(config_dir.join("index.yml"))
}

fn ensure_qmd_binary_with_path(config: &QmdConfig, path_env: Option<&OsStr>) -> Result<QmdBinary> {
    if let Some(path) = find_command(&config.command, path_env) {
        return Ok(QmdBinary {
            path,
            installed: false,
        });
    }

    if is_path_like(&config.command) {
        bail!(
            "configured qmd command was not found or is not executable: {}",
            config.command
        );
    }

    install_qmd(path_env)?;

    let Some(path) = find_command(&config.command, path_env) else {
        bail!(
            "qmd installed via {QMD_PACKAGE}, but '{}' is still not available on PATH",
            config.command
        );
    };

    Ok(QmdBinary {
        path,
        installed: true,
    })
}

fn install_qmd(path_env: Option<&OsStr>) -> Result<()> {
    let Some(installer) = select_installer(path_env) else {
        bail!(
            "qmd is not installed and neither npm nor bun was found on PATH; install with `npm install -g {QMD_PACKAGE}` or set [qmd].command"
        );
    };

    let (program, args): (PathBuf, &[&str]) = match installer {
        Installer::Npm(path) | Installer::Bun(path) => (path, &["install", "-g", QMD_PACKAGE]),
    };

    let mut command = Command::new(&program);
    command.args(args);
    if let Some(path_env) = path_env {
        command.env("PATH", path_env);
    }
    let output = command
        .output()
        .with_context(|| format!("run {} {}", program.display(), args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "qmd installer failed: {} {}: {}",
            program.display(),
            args.join(" "),
            stderr.trim()
        );
    }

    Ok(())
}

fn qmd_collection_info(binary: &QmdBinary, name: &str) -> Result<Option<QmdCollectionInfo>> {
    let output = run_qmd_command_allow_failure(
        binary,
        [
            OsString::from("collection"),
            OsString::from("show"),
            OsString::from(name),
        ],
    )?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        return parse_collection_show(&stdout)
            .with_context(|| format!("parse qmd collection '{name}' details"))
            .map(Some);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stderr.contains("Collection not found") || stdout.contains("Collection not found") {
        return Ok(None);
    }

    bail_qmd_failure(binary, ["collection", "show", name], &output)
}

fn parse_collection_show(output: &str) -> Result<QmdCollectionInfo> {
    let mut path = None;
    let mut pattern = None;
    for line in output.lines() {
        let line = line.trim_start();
        if let Some(value) = line.strip_prefix("Path:") {
            path = Some(PathBuf::from(value.trim()));
        } else if let Some(value) = line.strip_prefix("Pattern:") {
            pattern = Some(value.trim().to_string());
        }
    }

    let path = path.context("missing Path")?;
    let path = fs::canonicalize(&path).unwrap_or(path);
    let pattern = pattern.context("missing Pattern")?;
    Ok(QmdCollectionInfo { path, pattern })
}

fn run_qmd_command<I, S>(binary: &QmdBinary, args: I) -> Result<()>
where
    I: IntoIterator<Item = S> + Clone,
    S: AsRef<OsStr>,
{
    let output = run_qmd_command_allow_failure(binary, args.clone())?;
    if output.status.success() {
        return Ok(());
    }
    bail_qmd_failure(binary, args, &output)
}

fn run_qmd_command_allow_failure<I, S>(binary: &QmdBinary, args: I) -> Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(&binary.path);
    command.args(args);
    command.env("NO_COLOR", "1");
    command
        .output()
        .with_context(|| format!("run qmd command at {}", binary.path.display()))
}

fn bail_qmd_failure<T, I, S>(
    binary: &QmdBinary,
    args: I,
    output: &std::process::Output,
) -> Result<T>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    bail!(
        "qmd command failed: {} {}: {}",
        binary.path.display(),
        args,
        detail
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Installer {
    Npm(PathBuf),
    Bun(PathBuf),
}

fn select_installer(path_env: Option<&OsStr>) -> Option<Installer> {
    find_command("npm", path_env)
        .map(Installer::Npm)
        .or_else(|| find_command("bun", path_env).map(Installer::Bun))
}

fn find_command(command: &str, path_env: Option<&OsStr>) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }

    if is_path_like(command) {
        let path = PathBuf::from(command);
        return is_executable_file(&path).then_some(path);
    }

    let path_env = path_env?;
    std::env::split_paths(path_env).find_map(|dir| {
        candidate_filenames(command)
            .into_iter()
            .map(|filename| dir.join(filename))
            .find(|path| is_executable_file(path))
    })
}

fn is_path_like(command: &str) -> bool {
    Path::new(command).components().count() > 1
}

fn candidate_filenames(command: &str) -> Vec<OsString> {
    #[cfg(windows)]
    {
        if Path::new(command).extension().is_some() {
            return vec![OsString::from(command)];
        }
        let pathext = std::env::var_os("PATHEXT").unwrap_or_else(|| ".EXE;.CMD;.BAT;.COM".into());
        let mut candidates = vec![OsString::from(command)];
        for ext in pathext.to_string_lossy().split(';') {
            if ext.is_empty() {
                continue;
            }
            candidates.push(OsString::from(format!("{command}{ext}")));
            candidates.push(OsString::from(format!(
                "{command}{}",
                ext.to_ascii_lowercase()
            )));
        }
        candidates
    }

    #[cfg(not(windows))]
    {
        vec![OsString::from(command)]
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn finds_configured_command_on_path() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let qmd_path = bin_dir.join("qmd");
        write_executable(&qmd_path, "#!/bin/sh\necho qmd\n");

        let config = QmdConfig::default();
        let found = find_command(&config.command, Some(bin_dir.as_os_str()));

        assert_eq!(found, Some(qmd_path));
    }

    #[test]
    fn finds_configured_explicit_path() {
        let dir = tempdir().unwrap();
        let qmd_path = dir.path().join("qmd-custom");
        write_executable(&qmd_path, "#!/bin/sh\necho qmd\n");
        let config = QmdConfig {
            command: qmd_path.display().to_string(),
        };

        let binary = ensure_qmd_binary_with_path(&config, None).unwrap();

        assert_eq!(binary.path, qmd_path);
        assert!(!binary.installed);
    }

    #[test]
    fn installs_with_npm_when_qmd_is_missing() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let npm_path = bin_dir.join("npm");
        let qmd_path = bin_dir.join("qmd");
        write_executable(
            &npm_path,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = install ] && [ \"$2\" = -g ] && [ \"$3\" = {QMD_PACKAGE:?} ]; then\n  printf '#!/bin/sh\\necho qmd\\n' > {qmd:?}\n  /bin/chmod +x {qmd:?}\n  exit 0\nfi\nexit 1\n",
                qmd = qmd_path.display().to_string()
            ),
        );

        let binary =
            ensure_qmd_binary_with_path(&QmdConfig::default(), Some(bin_dir.as_os_str())).unwrap();

        assert_eq!(binary.path, qmd_path);
        assert!(binary.installed);
    }

    #[test]
    fn errors_when_no_installer_exists() {
        let dir = tempdir().unwrap();
        let error =
            ensure_qmd_binary_with_path(&QmdConfig::default(), Some(dir.path().as_os_str()))
                .unwrap_err()
                .to_string();

        assert!(error.contains("neither npm nor bun"));
    }

    #[test]
    fn parses_qmd_uri_thread_results() {
        let dir = tempdir().unwrap();
        let output = r##"[
            {
                "docid": "#thread123",
                "file": "qmd://zdx-threads/thread-123.md",
                "title": "Thread thread-123",
                "snippet": "User: hello",
                "score": 0.93
            }
        ]"##;

        let parsed = parse_qmd_memory_search_output(
            output,
            &test_collections(
                dir.path(),
                &dir.path().join("Notes"),
                &dir.path().join("Calendar"),
            ),
            None,
        )
        .unwrap();

        assert!(parsed.warnings.is_empty());
        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].docid, "#thread123");
        assert_eq!(parsed.results[0].file, "qmd://zdx-threads/thread-123.md");
        assert_eq!(parsed.results[0].score, Some(0.93));
    }

    #[test]
    fn parses_qmd_uri_note_and_calendar_results() {
        let dir = tempdir().unwrap();
        let output = r##"[
            {"docid": "#note123", "file": "qmd://zdx-notes/Projects/ZDX.md", "snippet": "note match"},
            {"docid": "#calendar123", "file": "qmd://zdx-calendar/2026-05-11.md", "snippet": "calendar match"}
        ]"##;

        let parsed = parse_qmd_memory_search_output(
            output,
            &test_collections(
                dir.path(),
                &dir.path().join("Notes"),
                &dir.path().join("Calendar"),
            ),
            None,
        )
        .unwrap();

        assert!(parsed.warnings.is_empty());
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].docid, "#note123");
        assert_eq!(parsed.results[0].file, "qmd://zdx-notes/Projects/ZDX.md");
        assert_eq!(parsed.results[1].docid, "#calendar123");
        assert_eq!(parsed.results[1].file, "qmd://zdx-calendar/2026-05-11.md");
    }

    #[test]
    fn gets_indexed_qmd_doc_by_docid() {
        let dir = tempdir().unwrap();
        let qmd_path = dir.path().join("qmd");
        write_executable(
            &qmd_path,
            "#!/bin/sh\nif [ \"$1\" = get ] && [ \"$2\" = '#doc123' ]; then\n  printf '# Indexed Doc\\n\\nContent from qmd\\n'\n  exit 0\nfi\necho unexpected qmd args >&2\nexit 1\n",
        );

        let output = get_memory_doc_with_binary(
            &QmdBinary {
                path: qmd_path,
                installed: false,
            },
            "#doc123",
        )
        .unwrap();

        assert_eq!(output.docid, "#doc123");
        assert!(output.content.contains("Content from qmd"));
    }

    #[test]
    fn skips_archive_and_trash_qmd_uri_results() {
        let dir = tempdir().unwrap();
        let output = r##"[
            {"docid": "#archived", "file": "qmd://zdx-notes/@Archive/Old.md", "snippet": "archived"},
            {"docid": "#trash", "file": "qmd://zdx-calendar/2026/@Trash/Deleted.md", "snippet": "trash"},
            {"docid": "#active", "file": "qmd://zdx-notes/Active.md", "snippet": "active"}
        ]"##;

        let parsed = parse_qmd_memory_search_output(
            output,
            &test_collections(
                dir.path(),
                &dir.path().join("Notes"),
                &dir.path().join("Calendar"),
            ),
            None,
        )
        .unwrap();

        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].docid, "#active");
        assert_eq!(parsed.warnings.len(), 2);
        assert!(parsed.warnings[0].contains("excluded memory path"));
    }

    #[test]
    fn skips_results_outside_configured_qmd_collections() {
        let dir = tempdir().unwrap();
        let output = r##"[
            {"docid":"#inside","file":"qmd://zdx-threads/thread-abs.md","snippet":"match"},
            {"docid":"#outside","file":"qmd://other-collection/outside.md","snippet":"outside"}
        ]"##;

        let parsed = parse_qmd_memory_search_output(
            output,
            &test_collections(
                &dir.path().join("exports").join("threads"),
                &dir.path().join("Notes"),
                &dir.path().join("Calendar"),
            ),
            None,
        )
        .unwrap();

        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].docid, "#inside");
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.warnings[0].contains("outside ZDX memory collections"));
    }

    #[test]
    fn excludes_current_thread_from_qmd_results() {
        let dir = tempdir().unwrap();
        let output = r##"[
            {"docid": "#current", "file": "qmd://zdx-threads/current.md", "snippet": "current"},
            {"docid": "#other", "file": "qmd://zdx-threads/other.md", "snippet": "other"}
        ]"##;

        let parsed = parse_qmd_memory_search_output(
            output,
            &test_collections(
                dir.path(),
                &dir.path().join("Notes"),
                &dir.path().join("Calendar"),
            ),
            Some("current"),
        )
        .unwrap();

        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].docid, "#other");
    }

    fn test_collections(
        export_dir: &Path,
        notes_dir: &Path,
        calendar_dir: &Path,
    ) -> Vec<QmdMemoryCollectionDef> {
        vec![
            QmdMemoryCollectionDef {
                name: THREAD_COLLECTION_NAME,
                source: "thread",
                root_dir: export_dir.to_path_buf(),
                pattern: THREAD_COLLECTION_PATTERN,
                context: THREAD_COLLECTION_CONTEXT,
                ignore_patterns: &[],
            },
            QmdMemoryCollectionDef {
                name: NOTES_COLLECTION_NAME,
                source: "note",
                root_dir: notes_dir.to_path_buf(),
                pattern: MEMORY_MARKDOWN_COLLECTION_PATTERN,
                context: NOTES_COLLECTION_CONTEXT,
                ignore_patterns: MEMORY_COLLECTION_IGNORE_PATTERNS,
            },
            QmdMemoryCollectionDef {
                name: CALENDAR_COLLECTION_NAME,
                source: "calendar",
                root_dir: calendar_dir.to_path_buf(),
                pattern: MEMORY_MARKDOWN_COLLECTION_PATTERN,
                context: CALENDAR_COLLECTION_CONTEXT,
                ignore_patterns: MEMORY_COLLECTION_IGNORE_PATTERNS,
            },
        ]
    }

    fn write_executable(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}
