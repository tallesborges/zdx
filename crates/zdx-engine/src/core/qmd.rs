//! qmd binary discovery and setup.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::{self, QmdConfig};

/// qmd package installed by the supported Node/Bun installers.
pub const QMD_PACKAGE: &str = "@tobilu/qmd";

/// qmd collection used for exported ZDX thread transcripts.
pub const THREAD_COLLECTION_NAME: &str = "zdx-threads";

/// File pattern qmd should index inside the thread transcript export directory.
pub const THREAD_COLLECTION_PATTERN: &str = "**/*.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QmdBinary {
    /// Resolved executable path.
    pub path: PathBuf,
    /// Whether this call installed qmd before resolving it.
    pub installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QmdThreadIndexSummary {
    /// Resolved executable path.
    pub binary_path: PathBuf,
    /// Whether this call installed qmd before resolving it.
    pub installed: bool,
    /// Thread export directory registered with qmd.
    pub export_dir: PathBuf,
    /// Whether the qmd collection was created during this call.
    pub collection_added: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QmdCollectionInfo {
    path: PathBuf,
    pattern: String,
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

/// Registers/updates qmd's `zdx-threads` collection over exported thread transcripts.
///
/// # Errors
/// Returns an error when qmd is unavailable, the collection points at a different path
/// or pattern, or qmd indexing/embedding fails.
pub fn index_thread_exports(config: &QmdConfig) -> Result<QmdThreadIndexSummary> {
    let binary = ensure_qmd_binary(config)?;
    index_thread_exports_with_binary(binary)
}

fn index_thread_exports_with_binary(binary: QmdBinary) -> Result<QmdThreadIndexSummary> {
    let export_dir = config::paths::thread_exports_dir();
    fs::create_dir_all(&export_dir).context("create thread exports directory")?;
    let export_dir = fs::canonicalize(&export_dir).context("resolve thread exports directory")?;

    let collection = qmd_collection_info(&binary, THREAD_COLLECTION_NAME)?;
    let collection_added = match collection {
        Some(info) => {
            if info.path != export_dir || info.pattern != THREAD_COLLECTION_PATTERN {
                bail!(
                    "qmd collection '{name}' already exists for path '{}' with pattern '{}'; expected path '{}' with pattern '{}'. Remove or rename the existing collection with `qmd collection remove {name}` and try again.",
                    info.path.display(),
                    info.pattern,
                    export_dir.display(),
                    THREAD_COLLECTION_PATTERN,
                    name = THREAD_COLLECTION_NAME
                );
            }
            false
        }
        None => {
            run_qmd_command(
                &binary,
                [
                    OsString::from("collection"),
                    OsString::from("add"),
                    export_dir.as_os_str().to_os_string(),
                    OsString::from("--name"),
                    OsString::from(THREAD_COLLECTION_NAME),
                    OsString::from("--mask"),
                    OsString::from(THREAD_COLLECTION_PATTERN),
                ],
            )
            .context("create qmd thread collection")?;
            true
        }
    };

    run_qmd_command(&binary, [OsString::from("update")]).context("update qmd index")?;
    run_qmd_command(
        &binary,
        [
            OsString::from("embed"),
            OsString::from("-c"),
            OsString::from(THREAD_COLLECTION_NAME),
        ],
    )
    .context("embed qmd thread collection")?;

    Ok(QmdThreadIndexSummary {
        binary_path: binary.path,
        installed: binary.installed,
        export_dir,
        collection_added,
    })
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
    let detail = if !stderr.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
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

    fn write_executable(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}
