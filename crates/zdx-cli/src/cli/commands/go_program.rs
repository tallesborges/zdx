//! `zdx-go`, the one Go binary `zdx` runs in its own place (exec), with the
//! command first: the Telegram gateway (`zdx bot`), the monitor
//! (`zdx monitor`), the chat TUI (bare `zdx`), the quota reader
//! (`zdx quota`), the jobs program (`zdx jobs`), and the Git views
//! (`zdx git`). All get the same contract: `$ZDX_GO` (an absolute path) or
//! the installed `~/.local/bin/zdx-go`, never a PATH lookup, never this
//! binary; this `ZDX_HOME`; this binary as `ZDX_BIN` unless already set.
//! Being the same process, the program receives this process's signals and
//! its exit status is the command's.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use zdx_engine::config::paths;
use zdx_engine::service;

/// The binary name.
pub const NAME: &str = "zdx-go";
/// Environment variable naming an absolute override path.
pub const ENV: &str = "ZDX_GO";
/// The `just` recipe that installs it.
pub const INSTALL: &str = "go-install";

/// One `zdx-go` command.
pub struct GoProgram {
    /// The `zdx-go` command, e.g. `gateway`.
    pub command: &'static str,
    /// Why it is needed, for the not-found error.
    pub why: &'static str,
}

impl GoProgram {
    /// `zdx-go <command> --root ROOT` with `ZDX_HOME` and `ZDX_BIN` set.
    ///
    /// # Errors
    /// When the program is not an absolute path, does not exist, or is this
    /// binary, or a path cannot be resolved.
    pub fn command(&self, root: &Path) -> Result<(Command, PathBuf)> {
        let program = match std::env::var_os(ENV) {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => service::default_go_program(),
        };
        if !program.is_absolute() {
            bail!(
                "{ENV} must be an absolute path to a {NAME} binary (got {})",
                program.display()
            );
        }
        if !program.is_file() {
            bail!(
                "{NAME} not found at {}: {}. Build and install it with `just {INSTALL}`, or set {ENV} to a {NAME} binary",
                program.display(),
                self.why,
            );
        }
        let exe = std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .context("resolve the zdx binary")?;
        let resolved = program
            .canonicalize()
            .with_context(|| format!("resolve {}", program.display()))?;
        if resolved == exe {
            bail!(
                "{} is this zdx binary, not {NAME}; set {ENV} to a {NAME} binary",
                program.display(),
            );
        }
        let root = root
            .canonicalize()
            .with_context(|| format!("resolve root {}", root.display()))?;

        let mut command = Command::new(&program);
        command
            .arg(self.command)
            .arg("--root")
            .arg(&root)
            .env("ZDX_HOME", paths::zdx_home());
        if std::env::var_os("ZDX_BIN").is_none_or(|bin| bin.is_empty()) {
            command.env("ZDX_BIN", &exe);
        }
        Ok((command, program))
    }
}

/// Replaces this process with `command`.
///
/// # Errors
/// When the exec fails, or on a system without exec.
#[cfg(unix)]
pub fn exec(mut command: Command, program: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    let error = command.exec();
    Err(error).with_context(|| format!("start {}", program.display()))
}

#[cfg(not(unix))]
pub fn exec(_command: Command, program: &Path) -> Result<()> {
    bail!(
        "zdx starts {} in its own place, which needs a Unix system",
        program.display()
    )
}
