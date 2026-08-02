//! launchd-backed control for the long-lived `bot` and `daemon` services.
//!
//! launchd owns process lifetime (start at login, restart on crash, restart
//! after the bot's `/exit`); this module owns the ergonomics. Status still comes
//! from [`crate::pidfile`], which is the source of truth for "is it running".

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fmt, fs};

use anyhow::{Context, Result, bail};

use crate::config::paths;
use crate::pidfile::{self, ServiceStatus};

/// Environment variable set in the generated plists so a launchd-started
/// service knows it is supervised (see the bot's `/exit` gate).
pub const SUPERVISOR_ENV: &str = "ZDX_SERVICE_SUPERVISOR";

/// A ZDX service that can run under launchd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Bot,
    Daemon,
}

impl Service {
    /// Every managed service, in display order.
    pub const ALL: [Self; 2] = [Self::Bot, Self::Daemon];

    /// PID-file / log-file base name, also the user-facing service name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bot => "bot",
            Self::Daemon => "daemon",
        }
    }

    /// launchd label for this service.
    pub fn label(self) -> String {
        format!("dev.zdx.{}", self.name())
    }

    /// `~/Library/LaunchAgents/<label>.plist`
    pub fn plist_path(self) -> PathBuf {
        let home = paths::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", self.label()))
    }

    /// Captured stdout (`err = false`) or stderr (`err = true`) path.
    pub fn log_path(self, err: bool) -> PathBuf {
        let ext = if err { "err" } else { "out" };
        logs_dir().join(format!("{}.{ext}", self.name()))
    }

    /// CLI arguments appended after the program path.
    fn cli_args(self, root: &Path) -> Vec<String> {
        let root = root.to_string_lossy().to_string();
        match self {
            Self::Bot => vec!["--root".into(), root, "bot".into()],
            Self::Daemon => vec!["--root".into(), root, "automations".into(), "daemon".into()],
        }
    }

    /// Resolves a CLI target into the services it refers to.
    ///
    /// Accepts `bot`, `daemon`, or `all`.
    ///
    /// # Errors
    /// Returns an error for any other value.
    pub fn parse_target(target: &str) -> Result<Vec<Self>> {
        match target {
            "all" => Ok(Self::ALL.to_vec()),
            "bot" => Ok(vec![Self::Bot]),
            "daemon" => Ok(vec![Self::Daemon]),
            other => bail!("unknown service '{other}' (expected: bot, daemon, all)"),
        }
    }
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Observable state of a service: launchd registration plus live process status.
pub struct ServiceState {
    pub service: Service,
    pub installed: bool,
    pub pid: Option<u32>,
    pub uptime: Option<std::time::Duration>,
}

impl ServiceState {
    pub fn running(&self) -> bool {
        self.pid.is_some()
    }
}

/// Reads the current state of `service`.
pub fn state(service: Service) -> ServiceState {
    let (pid, uptime) = match pidfile::status(service.name()) {
        ServiceStatus::Running { pid, started } => {
            (Some(pid), started.and_then(|s| s.elapsed().ok()))
        }
        ServiceStatus::Stopped => (None, None),
    };
    ServiceState {
        service,
        installed: service.plist_path().is_file(),
        pid,
        uptime,
    }
}

/// The stable binary launchd should run: `~/.local/bin/zdx` (the `just install` target).
///
/// Deliberately not `current_exe()` — restart must pick up the freshly
/// installed binary, not whatever binary happened to issue the command.
pub fn default_program() -> PathBuf {
    paths::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("bin")
        .join("zdx")
}

/// Renders the launchd plist for `service`.
///
/// The agent runs through `zsh -c` rather than executing the binary directly:
/// launchd sources no shell startup files, and `~/.zshenv` is where the
/// provider API keys live. `exec` keeps launchd tracking the real process, so
/// `KeepAlive` still works.
pub fn render_plist(service: Service, program: &Path, root: &Path) -> String {
    let mut command = vec![shell_quote(&program.to_string_lossy())];
    command.extend(service.cli_args(root).iter().map(|a| shell_quote(a)));
    let args = [
        "/bin/zsh".to_string(),
        "-c".to_string(),
        format!("exec {}", command.join(" ")),
    ];
    let mut program_args = String::new();
    for arg in &args {
        use std::fmt::Write;
        let _ = writeln!(program_args, "        <string>{}</string>", xml_escape(arg));
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{program_args}    </array>
    <key>WorkingDirectory</key>
    <string>{root}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{out_log}</string>
    <key>StandardErrorPath</key>
    <string>{err_log}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
        <key>ZDX_HOME</key>
        <string>{zdx_home}</string>
        <key>{supervisor_env}</key>
        <string>launchd</string>
    </dict>
</dict>
</plist>
"#,
        label = xml_escape(&service.label()),
        root = xml_escape(&root.to_string_lossy()),
        out_log = xml_escape(&service.log_path(false).to_string_lossy()),
        err_log = xml_escape(&service.log_path(true).to_string_lossy()),
        path = xml_escape(&agent_path_env()),
        zdx_home = xml_escape(&paths::zdx_home().to_string_lossy()),
        supervisor_env = SUPERVISOR_ENV,
    )
}

/// Writes the plist for `service` and bootstraps it into the GUI domain.
///
/// # Errors
/// Returns an error if the program is missing, a manually started instance is
/// already running, the plist cannot be written, or `launchctl` fails.
pub fn install(service: Service, program: &Path, root: &Path) -> Result<String> {
    if !program.is_file() {
        bail!("{} not found — run `just install` first", program.display());
    }
    // launchd has no useful notion of the caller's cwd, so a relative root
    // (the CLI's default `.`) has to be resolved before it reaches the plist.
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve root {}", root.display()))?;
    let root = root.as_path();
    if let ServiceStatus::Running { pid, .. } = pidfile::status(service.name())
        && !service.plist_path().is_file()
    {
        bail!(
            "{service} is already running (PID {pid}) outside launchd — stop it first, then re-run install"
        );
    }

    fs::create_dir_all(logs_dir())
        .with_context(|| format!("create log dir {}", logs_dir().display()))?;
    let plist = service.plist_path();
    if let Some(parent) = plist.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create LaunchAgents dir {}", parent.display()))?;
    }
    // Re-installing over a loaded agent leaves launchd holding the old plist.
    if plist.is_file() {
        let _ = launchctl(&["bootout".into(), service_target(service)]);
        wait_until_unloaded(service);
    }
    fs::write(&plist, render_plist(service, program, root))
        .with_context(|| format!("write {}", plist.display()))?;

    launchctl(&["bootstrap".into(), domain_target(), plist_arg(service)])?;
    Ok(format!(
        "Installed and started {service} ({})",
        plist.display()
    ))
}

/// Boots the agent out and removes its plist.
///
/// # Errors
/// Returns an error if the plist cannot be removed.
pub fn uninstall(service: Service) -> Result<String> {
    let plist = service.plist_path();
    if !plist.is_file() {
        return Ok(format!("{service} is not installed"));
    }
    let _ = launchctl(&["bootout".into(), service_target(service)]);
    fs::remove_file(&plist).with_context(|| format!("remove {}", plist.display()))?;
    Ok(format!("Uninstalled {service}"))
}

/// Bootstraps a previously installed agent (`RunAtLoad` starts it).
///
/// # Errors
/// Returns an error if the agent is not installed or `launchctl` fails.
pub fn start(service: Service) -> Result<String> {
    require_installed(service)?;
    if let ServiceStatus::Running { pid, .. } = pidfile::status(service.name()) {
        return Ok(format!("{service} is already running (PID {pid})"));
    }
    launchctl(&["bootstrap".into(), domain_target(), plist_arg(service)])?;
    Ok(format!("Started {service}"))
}

/// Boots the agent out so it stays stopped until an explicit start.
///
/// # Errors
/// Returns an error if the agent is not installed or `launchctl` fails.
pub fn stop(service: Service) -> Result<String> {
    require_installed(service)?;
    launchctl(&["bootout".into(), service_target(service)])?;
    Ok(format!("Stopped {service}"))
}

/// Restarts the agent, waiting for the old process to exit first.
///
/// # Errors
/// Returns an error if the agent is not installed or `launchctl` fails.
pub fn restart(service: Service) -> Result<String> {
    require_installed(service)?;
    let old_pid = match pidfile::status(service.name()) {
        ServiceStatus::Running { pid, .. } => pid,
        ServiceStatus::Stopped => {
            // `kickstart -k` on a booted-out agent fails; bootstrap it instead.
            launchctl(&["bootstrap".into(), domain_target(), plist_arg(service)])?;
            return Ok(format!("Started {service}"));
        }
    };
    launchctl(&["kickstart".into(), "-k".into(), service_target(service)])?;
    Ok(match wait_for_new_pid(service, old_pid) {
        Some(new_pid) => format!("Restarted {service} (PID {old_pid} → {new_pid})"),
        None => format!("Restarted {service}"),
    })
}

/// Polls the PID file briefly so restart can report the replacement PID.
fn wait_for_new_pid(service: Service, old_pid: u32) -> Option<u32> {
    for _ in 0..40 {
        if let ServiceStatus::Running { pid, .. } = pidfile::status(service.name())
            && pid != old_pid
        {
            return Some(pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    None
}

/// Waits for a booted-out job to leave the domain.
///
/// `bootout` returns before launchd finishes tearing the job down, and
/// bootstrapping into a domain that still holds the old job fails with
/// `Bootstrap failed: 5: Input/output error`.
fn wait_until_unloaded(service: Service) {
    for _ in 0..40 {
        if !is_loaded(service) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn is_loaded(service: Service) -> bool {
    Command::new("launchctl")
        .args(["print", &service_target(service)])
        .output()
        .is_ok_and(|out| out.status.success())
}

fn require_installed(service: Service) -> Result<()> {
    if service.plist_path().is_file() {
        Ok(())
    } else {
        bail!("{service} is not installed — run `zdx service install` first")
    }
}

/// Formats a service uptime for display (shared by the CLI and the monitor).
pub fn format_uptime(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

fn logs_dir() -> PathBuf {
    paths::zdx_home().join("run").join("logs")
}

fn domain_target() -> String {
    format!("gui/{}", uid())
}

fn service_target(service: Service) -> String {
    format!("{}/{}", domain_target(), service.label())
}

fn plist_arg(service: Service) -> String {
    service.plist_path().to_string_lossy().to_string()
}

/// launchd agents inherit a minimal PATH, so spawned helpers (`cargo`, `qmd`,
/// `ffmpeg`, `git`) would not be found without this.
fn agent_path_env() -> String {
    let home = paths::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let home = home.display();
    format!(
        "{home}/.cargo/bin:{home}/.bun/bin:{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    )
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[String]) -> Result<()> {
    let output = Command::new("launchctl")
        .args(args)
        .output()
        .context("run launchctl")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let detail = if stderr.is_empty() {
        format!("exit status {}", output.status)
    } else {
        stderr
    };
    bail!("launchctl {} failed: {detail}", args.join(" "))
}

#[cfg(not(target_os = "macos"))]
fn launchctl(_args: &[String]) -> Result<()> {
    bail!("`zdx service` control is macOS-only (launchd)")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Wraps a value in single quotes for the `zsh -c` command string.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        assert_eq!(Service::parse_target("bot").unwrap(), vec![Service::Bot]);
        assert_eq!(
            Service::parse_target("daemon").unwrap(),
            vec![Service::Daemon]
        );
        assert_eq!(Service::parse_target("all").unwrap(), Service::ALL.to_vec());
        assert!(Service::parse_target("monitor").is_err());
    }

    #[test]
    fn labels_and_names() {
        assert_eq!(Service::Bot.label(), "dev.zdx.bot");
        assert_eq!(Service::Daemon.label(), "dev.zdx.daemon");
    }

    #[test]
    fn bot_plist_runs_installed_binary_with_root() {
        let plist = render_plist(
            Service::Bot,
            Path::new("/Users/x/.local/bin/zdx"),
            Path::new("/Users/x/projects/zdx"),
        );
        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains("<string>dev.zdx.bot</string>"));
        assert!(plist.contains("<string>/bin/zsh</string>"));
        assert!(plist.contains("<string>-c</string>"));
        assert!(plist.contains(
            "<string>exec '/Users/x/.local/bin/zdx' '--root' '/Users/x/projects/zdx' 'bot'</string>"
        ));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>ThrottleInterval</key>"));
        assert!(plist.contains("<string>launchd</string>"));
        assert!(plist.contains("bot.out"));
        assert!(plist.contains("bot.err"));
    }

    #[test]
    fn daemon_plist_runs_automations_daemon() {
        let plist = render_plist(
            Service::Daemon,
            Path::new("/Users/x/.local/bin/zdx"),
            Path::new("/Users/x/projects/zdx"),
        );
        assert!(plist.contains("<string>dev.zdx.daemon</string>"));
        assert!(plist.contains(
            "<string>exec '/Users/x/.local/bin/zdx' '--root' '/Users/x/projects/zdx' 'automations' 'daemon'</string>"
        ));
        assert!(plist.contains("daemon.err"));
    }

    #[test]
    fn shell_quotes_embedded_single_quote() {
        assert_eq!(shell_quote("/tmp/it's"), r"'/tmp/it'\''s'");
    }

    #[test]
    fn plist_path_includes_toolchain_dirs() {
        let plist = render_plist(
            Service::Bot,
            Path::new("/Users/x/.local/bin/zdx"),
            Path::new("/Users/x/projects/zdx"),
        );
        assert!(plist.contains(".cargo/bin"));
        assert!(plist.contains("/opt/homebrew/bin"));
    }

    #[test]
    fn escapes_xml_in_paths() {
        let plist = render_plist(
            Service::Bot,
            Path::new("/tmp/a&b/zdx"),
            Path::new("/tmp/<root>"),
        );
        assert!(plist.contains("/tmp/a&amp;b/zdx"));
        assert!(plist.contains("/tmp/&lt;root&gt;"));
    }
}
