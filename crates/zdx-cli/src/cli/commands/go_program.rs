//! Go programs `zdx` runs in its own place (exec): the Telegram gateway
//! (`zdx bot`), the monitor (`zdx monitor`), and the quota reader
//! (`zdx quota`). All get the same contract:
//! an absolute override path or the installed copy in `~/.local/bin`, never
//! a PATH lookup, never this binary; this `ZDX_HOME`; this binary as
//! `ZDX_BIN` unless already set. Being the same process, the program
//! receives this process's signals and its exit status is the command's.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use zdx_engine::config::paths;
use zdx_engine::service;

pub struct GoProgram {
    /// Binary name, e.g. `zdx-gateway`.
    pub name: &'static str,
    /// Environment variable naming an absolute override path.
    pub env: &'static str,
    /// What the program is, for "… is this zdx binary, not the {role}".
    pub role: &'static str,
    /// Why it is needed, for the not-found error.
    pub why: &'static str,
    /// The `just` recipe that installs it.
    pub install: &'static str,
}

impl GoProgram {
    /// The installed copy: `~/.local/bin/<name>`.
    pub fn installed(&self) -> PathBuf {
        service::default_program().with_file_name(self.name)
    }

    /// `<program> --root ROOT` with `ZDX_HOME` and `ZDX_BIN` set.
    ///
    /// # Errors
    /// When the program is not an absolute path, does not exist, or is this
    /// binary, or a path cannot be resolved.
    pub fn command(&self, root: &Path) -> Result<(Command, PathBuf)> {
        let program = match std::env::var_os(self.env) {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => self.installed(),
        };
        let (name, env) = (self.name, self.env);
        if !program.is_absolute() {
            bail!(
                "{env} must be an absolute path to a {name} binary (got {})",
                program.display()
            );
        }
        if !program.is_file() {
            bail!(
                "{name} not found at {}: {}. Build and install it with `just {}`, or set {env} to a {name} binary",
                program.display(),
                self.why,
                self.install
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
                "{} is this zdx binary, not the {}; set {env} to a {name} binary",
                program.display(),
                self.role
            );
        }
        let root = root
            .canonicalize()
            .with_context(|| format!("resolve root {}", root.display()))?;

        let mut command = Command::new(&program);
        command
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
