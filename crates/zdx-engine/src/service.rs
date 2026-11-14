//! launchd-backed control for the long-lived `bot`, `daemon`, and
//! `orchestrator` services.
//!
//! launchd owns process lifetime (start at login, restart on crash, restart
//! after the bot's `/restart`); this module owns the ergonomics. Status still comes
//! from [`crate::pidfile`], which is the source of truth for "is it running".

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fmt, fs};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent_activity;
use crate::config::paths;
use crate::pidfile::{self, ServiceStatus};

/// Environment variable set in the generated plists so a launchd-started
/// service knows it is supervised (see the bot's `/restart` gate).
pub const SUPERVISOR_ENV: &str = "ZDX_SERVICE_SUPERVISOR";

/// A ZDX service that can run under launchd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    Bot,
    Daemon,
    /// Hosts persistent-profile threads and their workers
    /// (`zdx orchestrator serve`).
    Orchestrator,
}

impl Service {
    /// Every managed service, in display order.
    pub const ALL: [Self; 3] = [Self::Bot, Self::Daemon, Self::Orchestrator];

    /// PID-file / log-file base name, also the user-facing service name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bot => "bot",
            Self::Daemon => "daemon",
            Self::Orchestrator => "orchestrator",
        }
    }

    /// Length of the longest service name, for aligning name columns.
    pub fn name_width() -> usize {
        Self::ALL
            .into_iter()
            .map(|s| s.name().len())
            .max()
            .unwrap_or(0)
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
            Self::Daemon => vec!["--root".into(), root, "jobs".into(), "daemon".into()],
            Self::Orchestrator => {
                vec!["--root".into(), root, "orchestrator".into(), "serve".into()]
            }
        }
    }

    /// Resolves a CLI target into the services it refers to.
    ///
    /// Accepts `bot`, `daemon`, `orchestrator`, or `all`.
    ///
    /// # Errors
    /// Returns an error for any other value.
    pub fn parse_target(target: &str) -> Result<Vec<Self>> {
        match target {
            "all" => Ok(Self::ALL.to_vec()),
            "bot" => Ok(vec![Self::Bot]),
            "daemon" => Ok(vec![Self::Daemon]),
            "orchestrator" => Ok(vec![Self::Orchestrator]),
            other => bail!("unknown service '{other}' (expected: bot, daemon, orchestrator, all)"),
        }
    }
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Restart or stop refused because work would be lost: active agent runs,
/// or (when the orchestrator service is a target) its in-memory workers,
/// queues, and undelivered results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartBlocked {
    action: &'static str,
    active_runs: usize,
    /// What the orchestrator service would lose, when it is a target.
    orchestrator: Option<String>,
}

impl RestartBlocked {
    #[must_use]
    pub fn active_runs(&self) -> usize {
        self.active_runs
    }
}

impl fmt::Display for RestartBlocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} blocked: ", self.action)?;
        if self.active_runs > 0 {
            let (noun, verb) = if self.active_runs == 1 {
                ("run", "is")
            } else {
                ("runs", "are")
            };
            write!(
                f,
                "{} active agent {noun} {verb} still running",
                self.active_runs
            )?;
            if self.orchestrator.is_some() {
                f.write_str("; ")?;
            }
        }
        if let Some(lost) = &self.orchestrator {
            write!(f, "the orchestrator service would lose {lost}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RestartBlocked {}

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

/// The stable gateway binary the bot service runs and `zdx bot` starts:
/// `~/.local/bin/zdx-gateway` (the `just gateway-install` target).
pub fn default_gateway_program() -> PathBuf {
    default_program().with_file_name("zdx-gateway")
}

/// The program `service` runs when none is given: the Go gateway for the
/// bot, the installed `zdx` for the daemon and the orchestrator.
pub fn default_program_for(service: Service) -> PathBuf {
    match service {
        Service::Bot => default_gateway_program(),
        Service::Daemon | Service::Orchestrator => default_program(),
    }
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

    let exit_timeout = if service == Service::Bot {
        format!("\n    <key>ExitTimeOut</key>\n    <integer>{BOT_EXIT_TIMEOUT_SECS}</integer>")
    } else {
        String::new()
    };
    let zdx_bin = xml_escape(&default_program().to_string_lossy());
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
    <integer>10</integer>{exit_timeout}
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
        <key>ZDX_BIN</key>
        <string>{zdx_bin}</string>
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

/// How an installed service is configured to start, read back from its plist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledLaunch {
    /// `--root` passed to the service.
    pub root: PathBuf,
    /// Working directory, which is where the service loads its config layers.
    pub working_directory: PathBuf,
    /// `ZDX_HOME` set for the service, if any.
    pub zdx_home: Option<PathBuf>,
}

/// Reads the installed plist of `service`. `None` when it is not installed.
///
/// This is the configuration launchd applies at the next start, not proof of
/// what a running process was started with.
pub fn installed_launch(service: Service) -> Option<Result<InstalledLaunch>> {
    let path = service.plist_path();
    if !path.is_file() {
        return None;
    }
    Some(
        fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))
            .and_then(|text| {
                parse_launch(&text).with_context(|| format!("parse {}", path.display()))
            }),
    )
}

/// Parses a plist written by [`render_plist`].
///
/// # Errors
/// Returns an error when the plist does not have the rendered shape.
pub fn parse_launch(plist: &str) -> Result<InstalledLaunch> {
    let working_directory =
        plist_string_after_key(plist, "WorkingDirectory").context("missing WorkingDirectory")?;
    let zdx_home = plist_string_after_key(plist, "ZDX_HOME");
    let command = plist_strings(plist)
        .into_iter()
        .find(|value| value.starts_with("exec "))
        .context("missing service command")?;
    let words = shell_unquote_words(command.trim_start_matches("exec "))?;
    let root = words
        .iter()
        .position(|word| word == "--root")
        .and_then(|index| words.get(index + 1))
        .context("service command has no --root")?;
    Ok(InstalledLaunch {
        root: PathBuf::from(root),
        working_directory: PathBuf::from(working_directory),
        zdx_home: zdx_home.map(PathBuf::from),
    })
}

fn plist_strings(plist: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = plist;
    while let Some(start) = rest.find("<string>") {
        let after = &rest[start + "<string>".len()..];
        let Some(end) = after.find("</string>") else {
            break;
        };
        out.push(xml_unescape(&after[..end]));
        rest = &after[end..];
    }
    out
}

fn plist_string_after_key(plist: &str, key: &str) -> Option<String> {
    let marker = format!("<key>{key}</key>");
    let after = &plist[plist.find(&marker)? + marker.len()..];
    let value = after.trim_start().strip_prefix("<string>")?;
    Some(xml_unescape(&value[..value.find("</string>")?]))
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Splits a command made of [`shell_quote`]d words.
fn shell_unquote_words(command: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => bail!("unterminated quote in service command"),
                    }
                }
            }
            '\\' if in_word => match chars.next() {
                Some(c) => current.push(c),
                None => bail!("dangling escape in service command"),
            },
            ' ' => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            other => {
                in_word = true;
                current.push(other);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

/// Writes the plist for `service` and bootstraps it into the GUI domain.
///
/// # Errors
/// Returns an error if the program is missing, a manually started instance is
/// already running, the plist cannot be written, or `launchctl` fails.
pub fn install(service: Service, program: &Path, root: &Path) -> Result<String> {
    check_program(service, program)?;
    // launchd has no useful notion of the caller's cwd, so a relative root
    // (the CLI's default `.`) has to be resolved before it reaches the plist.
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve root {}", root.display()))?;
    let root = root.as_path();
    if service == Service::Bot {
        for warning in preflight(program, root)? {
            tracing::warn!(program = %program.display(), warning, "Bot program preflight warning");
        }
    }
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

/// Checks that `program` can run `service`, before anything is changed: it
/// must exist, and the bot service runs the gateway, never `zdx` (which has
/// no bot of its own).
///
/// # Errors
/// Returns an error naming the build step when the program is missing, or
/// when the bot program is a `zdx` binary.
pub fn check_program(service: Service, program: &Path) -> Result<()> {
    if service == Service::Bot && program.file_name().is_some_and(|name| name == "zdx") {
        bail!(
            "the bot service runs the Go gateway, not zdx ({}); install it with `just gateway-install` or pass --program <zdx-gateway>",
            program.display()
        );
    }
    if !program.is_file() {
        let step = if service == Service::Bot {
            "just gateway-install"
        } else {
            "just install"
        };
        bail!("{} not found — run `{step}` first", program.display());
    }
    Ok(())
}

/// How long a bot program's preflight may take.
const PREFLIGHT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Preflight output kept; the rest is dropped.
const PREFLIGHT_OUTPUT_CAP: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
struct PreflightReport {
    #[serde(default)]
    service: Option<String>,
    ready: bool,
    #[serde(default)]
    problems: Vec<String>,
    #[serde(default)]
    warnings: Vec<String>,
}

/// Runs `<program> preflight --root <root> --json` before the gateway
/// replaces the bot service. Read-only by contract. The
/// install goes on only when it exits 0 and reports `"ready": true`;
/// returns its warnings.
///
/// # Errors
/// The program does not run, times out, fails, prints no valid report, or
/// is not ready (its problems are listed). Nothing was changed then.
fn preflight(program: &Path, root: &Path) -> Result<Vec<String>> {
    use std::io::Read as _;
    use std::process::Stdio;

    let mut child = Command::new(program)
        .args(["preflight", "--root"])
        .arg(root)
        .arg("--json")
        // The same engine path and PATH launchd will give it (`render_plist`).
        .env("ZDX_BIN", default_program())
        .env("PATH", agent_path_env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("run {} preflight", program.display()))?;
    let mut stdout = child.stdout.take().context("preflight stdout")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout)
            .take(PREFLIGHT_OUTPUT_CAP as u64)
            .read_to_end(&mut out);
        // Drain the rest so the child never blocks on a full pipe.
        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        out
    });
    let deadline = std::time::Instant::now() + PREFLIGHT_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().context("wait for preflight")? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{} preflight did not finish in {}s; the bot service was not changed",
                program.display(),
                PREFLIGHT_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        bail!(
            "{} preflight failed ({status}); the bot service was not changed",
            program.display()
        );
    }
    let report: PreflightReport = serde_json::from_slice(&out).with_context(|| {
        format!(
            "{} preflight printed no valid report; the bot service was not changed",
            program.display()
        )
    })?;
    if report.service.as_deref().is_some_and(|name| name != "bot") {
        bail!(
            "{} preflight reports service {:?}, not bot; the bot service was not changed",
            program.display(),
            report.service
        );
    }
    if !report.ready {
        bail!(
            "{} is not ready to run the bot service; nothing was changed:\n- {}",
            program.display(),
            if report.problems.is_empty() {
                "no reason given".to_string()
            } else {
                report.problems.join("\n- ")
            }
        );
    }
    Ok(report.warnings)
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
    if is_loaded(service) {
        launchctl(&["kickstart".into(), service_target(service)])?;
    } else {
        launchctl(&["bootstrap".into(), domain_target(), plist_arg(service)])?;
    }
    Ok(format!("Started {service}"))
}

/// Boots the agent out so it stays stopped until an explicit start.
///
/// # Errors
/// Returns an error if the agent is not installed, work would be lost and
/// `force` is false (the same check as [`restart_many`]), or `launchctl`
/// fails.
pub fn stop(service: Service, force: bool) -> Result<String> {
    require_installed(service)?;
    ensure_restart_allowed("stop", &[service], force)?;
    launchctl(&["bootout".into(), service_target(service)])?;
    Ok(format!("Stopped {service}"))
}

/// Restarts one agent, waiting for the old process to exit first.
///
/// # Errors
/// Returns an error if active agent work exists and `force` is false, the
/// agent is not installed, or `launchctl` fails.
pub fn restart(service: Service, force: bool) -> Result<String> {
    let mut messages = restart_many(&[service], force)?;
    Ok(messages.remove(0))
}

/// Restarts multiple agents after one shared preflight: every service is
/// installed, and (unless `force`) nothing would be lost — no agent run is
/// active and, when the orchestrator service is a target, it has no work in
/// flight.
///
/// # Errors
/// Returns an error before changing any service if one is not installed, or
/// if work would be lost and `force` is false. Also returns an error if
/// `launchctl` fails.
pub fn restart_many(services: &[Service], force: bool) -> Result<Vec<String>> {
    ensure_restart_allowed("restart", services, force)?;
    // Fail before any restart: restarting the bot onto a build that needs a
    // missing service would leave it half deployed.
    for service in services {
        require_installed(*service)?;
    }
    restart_order(services)
        .into_iter()
        .map(restart_unchecked)
        .collect()
}

/// The order services restart in: the orchestrator first, the bot last,
/// because the bot needs the orchestrator socket for every message.
fn restart_order(services: &[Service]) -> Vec<Service> {
    [Service::Orchestrator, Service::Daemon, Service::Bot]
        .into_iter()
        .filter(|service| services.contains(service))
        .collect()
}

/// Seconds launchd waits after SIGTERM before SIGKILL when it stops the bot
/// (`ExitTimeOut`): above the gateway's graceful drain (2 min, plus its
/// update confirm and reply timeout). Other services keep launchd's default.
const BOT_EXIT_TIMEOUT_SECS: u32 = 150;

/// Restarts what the bot service restarts before it exits itself (Telegram
/// `/restart`): the daemon, plus the orchestrator service when it is
/// installed. Gated like [`restart_many`] unless `force`.
///
/// # Errors
/// As [`restart_many`]; a [`RestartBlocked`] error means work would be lost.
pub fn restart_bot_peers(force: bool) -> Result<Vec<String>> {
    let mut services = vec![Service::Daemon];
    if state(Service::Orchestrator).installed {
        services.push(Service::Orchestrator);
    }
    restart_many(&services, force)
}

/// The one guard for restarting or stopping services. Active agent runs
/// count everywhere. The orchestrator service also loses what only lives in
/// its memory: queued worker prompts, worker processes still starting,
/// worker results not yet delivered, and queued or undelivered orchestrator
/// turns. None of those has an agent run yet, so they are asked of the
/// service itself.
fn ensure_restart_allowed(action: &'static str, services: &[Service], force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    let active_runs = agent_activity::list_active().len();
    let orchestrator = services
        .contains(&Service::Orchestrator)
        .then(orchestrator_in_flight)
        .flatten();
    if active_runs > 0 || orchestrator.is_some() {
        return Err(RestartBlocked {
            action,
            active_runs,
            orchestrator,
        }
        .into());
    }
    Ok(())
}

/// What a restart of the running orchestrator service would lose, or `None`
/// when it is not running or reports itself settled. A running service that
/// does not answer counts as busy: its state is unknown.
fn orchestrator_in_flight() -> Option<String> {
    use crate::orchestration::OrchestratorClient;
    use crate::orchestration::protocol::{Reply, Request};

    if !matches!(
        pidfile::status(Service::Orchestrator.name()),
        ServiceStatus::Running { .. }
    ) {
        return None;
    }
    let client = OrchestratorClient::new();
    let report = match client.request(&Request::Status { thread_id: None }) {
        Ok(Reply::Status { report }) => report,
        Ok(_) => return Some("unknown work (unexpected status reply)".to_string()),
        Err(err) => return Some(format!("unknown work (status unavailable: {err:#})")),
    };
    if report.settled {
        return None;
    }
    // Every worker, not only those listed under a thread of this service run.
    let workers = match client.request(&Request::Workers {
        owner_thread_id: None,
    }) {
        Ok(Reply::Workers { workers }) => workers,
        _ => Vec::new(),
    };
    Some(in_flight_summary(&report, &workers))
}

/// Human summary of the work in flight in an unsettled service: busy
/// threads and busy workers.
fn in_flight_summary(
    report: &crate::orchestration::protocol::StatusReport,
    workers: &[crate::core::workers::WorkerSnapshot],
) -> String {
    use crate::orchestration::protocol::ThreadState;

    let mut parts = Vec::new();
    for thread in &report.threads {
        if thread.state != ThreadState::Idle || thread.queued_inputs > 0 {
            let state = serde_json::to_value(thread.state)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default();
            parts.push(format!(
                "thread {} ({state}, {} queued)",
                thread.thread_id, thread.queued_inputs
            ));
        }
    }
    for worker in workers.iter().filter(|worker| !worker.is_idle()) {
        parts.push(format!(
            "worker {} ({}, {} queued)",
            worker.thread_id,
            worker.status.as_str(),
            worker.queue_depth
        ));
    }
    if parts.is_empty() {
        parts.push("a worker result not yet delivered".to_string());
    }
    parts.join(", ")
}

fn restart_unchecked(service: Service) -> Result<String> {
    require_installed(service)?;
    // A stale/missing PID file does not mean launchd dropped the job, and
    // bootstrapping a job the domain still holds fails with `5: Input/output error`.
    if !is_loaded(service) {
        launchctl(&["bootstrap".into(), domain_target(), plist_arg(service)])?;
        return Ok(format!("Started {service}"));
    }
    let old_pid = match pidfile::status(service.name()) {
        ServiceStatus::Running { pid, .. } => Some(pid),
        ServiceStatus::Stopped => None,
    };
    launchctl(&["kickstart".into(), "-k".into(), service_target(service)])?;
    let Some(old_pid) = old_pid else {
        return Ok(format!("Started {service}"));
    };
    Ok(match wait_for_new_pid(service, old_pid) {
        Some(new_pid) => format!("Restarted {service} (PID {old_pid} → {new_pid})"),
        None => format!("Restarted {service}"),
    })
}

/// A restart that waits until no agent run is active.
///
/// Persisted at `$ZDX_HOME/run/restart-queue.json` so any surface (Telegram
/// `/restart q`, `zdx service restart --queue`) can request one. The running
/// bot service performs it, because a queued restart has to outlive the agent
/// run that asked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedRestart {
    pub services: Vec<Service>,
    pub requested_at: DateTime<Utc>,
    /// Thread that asked last; the bot announces the restart there when it
    /// is a Telegram thread.
    pub thread_id: Option<String>,
}

impl QueuedRestart {
    #[must_use]
    pub fn includes(&self, service: Service) -> bool {
        self.services.contains(&service)
    }

    /// True once every requested service runs a process started after the
    /// request — a forced or manual restart already did the work.
    #[must_use]
    pub fn is_fulfilled(&self) -> bool {
        self.services
            .iter()
            .all(|service| match pidfile::status(service.name()) {
                ServiceStatus::Running {
                    started: Some(started),
                    ..
                } => DateTime::<Utc>::from(started) > self.requested_at,
                _ => false,
            })
    }
}

/// Result of [`queue_restart`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueRestartOutcome {
    /// The pending request after this call (merged with any earlier one).
    pub request: QueuedRestart,
    /// An unfulfilled request was already pending and absorbed this one.
    pub already_pending: bool,
}

/// Queues a gated restart of `services` for the bot service to perform once
/// no agent run is active.
///
/// One request is pending at a time: a new call merges its services into the
/// pending one, moves `requested_at` to now so the restart picks up whatever
/// was installed before this call, and takes over the announcement thread
/// when it names one.
///
/// # Errors
/// Returns an error when the bot service is not running under a supervisor —
/// nothing would perform the restart — or when the request cannot be written.
pub fn queue_restart(services: &[Service], thread_id: Option<&str>) -> Result<QueueRestartOutcome> {
    let bot_running = matches!(
        pidfile::status(Service::Bot.name()),
        ServiceStatus::Running { .. }
    );
    if !bot_running || !pidfile::is_supervised(Service::Bot.name()) {
        bail!(
            "the bot service is not running under a supervisor, so nothing would perform a queued restart; run `zdx service restart` once agent runs finish"
        );
    }

    let pending = queued_restart()?.filter(|request| !request.is_fulfilled());
    let already_pending = pending.is_some();
    let mut merged = pending
        .as_ref()
        .map_or_else(Vec::new, |p| p.services.clone());
    merged.extend(services.iter().copied());
    let request = QueuedRestart {
        services: Service::ALL
            .into_iter()
            .filter(|service| merged.contains(service))
            .collect(),
        requested_at: Utc::now(),
        thread_id: thread_id
            .map(str::to_string)
            .or_else(|| pending.and_then(|p| p.thread_id)),
    };
    write_queued_restart(&request)?;
    Ok(QueueRestartOutcome {
        request,
        already_pending,
    })
}

/// The pending queued restart, if any.
///
/// # Errors
/// Returns an error when the request file exists but cannot be read or parsed.
pub fn queued_restart() -> Result<Option<QueuedRestart>> {
    let path = restart_queue_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .with_context(|| format!("parse {}", path.display()))
}

/// What [`run_queued_restart`] did with the pending request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedRestartStatus {
    /// No request is pending (or a newer process already fulfilled it).
    None,
    /// A request is pending, but an agent run is active or the orchestrator
    /// service has work in flight. Nothing changed; check again later.
    Waiting,
    /// The request was dropped without restarting anything.
    Dropped,
    /// A restart failed; the request was dropped and the bot stays up.
    Failed,
    /// The non-bot targets restarted (if any) and the request was cleared.
    Done,
}

/// Result of [`run_queued_restart`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueuedRestartRun {
    pub status: QueuedRestartStatus,
    /// The bot must now exit with code 42 so launchd restarts it.
    pub restart_bot: bool,
    /// Status lines of the services that restarted.
    pub restarted: Vec<String>,
    /// Thread that asked; the bot announces there when it is a Telegram
    /// thread of an allowlisted chat.
    pub thread_id: Option<String>,
    /// Plain-text announcement for that thread; `None` when there is nothing
    /// to announce.
    pub message: Option<String>,
}

impl QueuedRestartRun {
    fn quiet(status: QueuedRestartStatus) -> Self {
        Self {
            status,
            restart_bot: false,
            restarted: Vec::new(),
            thread_id: None,
            message: None,
        }
    }
}

/// Performs the pending queued restart when nothing would be lost, as one
/// step the bot service runs while its own chat queues are empty.
///
/// The gates are the same as for `restart`: no active agent run anywhere and
/// a settled (or stopped) orchestrator service. Daemon and orchestrator
/// targets restart here; the bot target is never restarted here, it is
/// reported as `restart_bot` so the bot exits itself. A request that needs
/// the bot to restart is dropped when the bot is not supervised.
///
/// Blocking: file, process, socket, and `launchctl` checks.
#[must_use]
pub fn run_queued_restart() -> QueuedRestartRun {
    let request = match queued_restart() {
        Ok(Some(request)) => request,
        Ok(None) => return QueuedRestartRun::quiet(QueuedRestartStatus::None),
        Err(err) => {
            tracing::warn!(err = %format!("{err:#}"), "Dropping unreadable queued restart");
            clear_queued_restart();
            return QueuedRestartRun::quiet(QueuedRestartStatus::Dropped);
        }
    };
    if request.is_fulfilled() {
        tracing::info!("Queued restart already fulfilled by a newer process; dropping it");
        clear_queued_restart();
        return QueuedRestartRun::quiet(QueuedRestartStatus::None);
    }
    if !agent_activity::list_active().is_empty() || orchestrator_in_flight().is_some() {
        return QueuedRestartRun::quiet(QueuedRestartStatus::Waiting);
    }
    let finish = |status, restart_bot, restarted, message: String| QueuedRestartRun {
        status,
        restart_bot,
        restarted,
        thread_id: request.thread_id.clone(),
        message: Some(message),
    };

    if request.includes(Service::Bot) && !pidfile::is_supervised(Service::Bot.name()) {
        clear_queued_restart();
        return finish(
            QueuedRestartStatus::Dropped,
            false,
            Vec::new(),
            "⚠️ Queued restart dropped: the bot has no active supervisor, so it cannot exit and come back.".to_string(),
        );
    }

    let services: Vec<Service> = [Service::Daemon, Service::Orchestrator]
        .into_iter()
        .filter(|svc| request.includes(*svc))
        // `all` includes the orchestrator; skip it where it is not installed.
        .filter(|svc| *svc != Service::Orchestrator || state(*svc).installed)
        .collect();
    let mut restarted = Vec::new();
    if !services.is_empty() {
        match restart_many(&services, false) {
            Ok(status) => restarted = status,
            // A run started after the idle check; the gate caught it.
            Err(err) if err.downcast_ref::<RestartBlocked>().is_some() => {
                return QueuedRestartRun::quiet(QueuedRestartStatus::Waiting);
            }
            Err(err) => {
                clear_queued_restart();
                return finish(
                    QueuedRestartStatus::Failed,
                    false,
                    Vec::new(),
                    format!("⚠️ Queued restart failed, so I left the bot running.\n{err:#}"),
                );
            }
        }
    }

    clear_queued_restart();
    let prefix = if restarted.is_empty() {
        String::new()
    } else {
        format!("🔄 {}.\n", restarted.join(", "))
    };
    let restart_bot = request.includes(Service::Bot);
    let message = if restart_bot {
        format!(
            "{prefix}👋 Queued restart: no agent run is active, exiting bot… launchd will restart it shortly."
        )
    } else {
        format!("{prefix}✅ Queued restart done: no agent run was active.")
    };
    finish(QueuedRestartStatus::Done, restart_bot, restarted, message)
}

/// Drops the pending queued restart. Silent no-op when none is pending.
pub fn clear_queued_restart() {
    let _ = fs::remove_file(restart_queue_path());
}

fn write_queued_restart(request: &QueuedRestart) -> Result<()> {
    let path = restart_queue_path();
    let dir = path.parent().context("restart queue path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("create run dir {}", dir.display()))?;
    // Write-then-rename so the bot never reads a half-written request.
    let tmp = dir.join(format!("restart-queue.json.{}.tmp", std::process::id()));
    fs::write(&tmp, serde_json::to_vec_pretty(request)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("write {}", path.display()))
}

fn restart_queue_path() -> PathBuf {
    paths::zdx_home().join("run").join("restart-queue.json")
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
        bail!("{service} is not installed — run `zdx service install {service}` first")
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

/// launchd agents inherit a minimal PATH, so spawned helpers (`cargo`,
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
    fn the_orchestrator_restarts_before_the_bot() {
        assert_eq!(
            restart_order(&Service::ALL),
            [Service::Orchestrator, Service::Daemon, Service::Bot]
        );
        assert_eq!(restart_order(&[Service::Bot]), [Service::Bot]);
    }

    #[test]
    fn parses_targets() {
        assert_eq!(Service::parse_target("bot").unwrap(), vec![Service::Bot]);
        assert_eq!(
            Service::parse_target("daemon").unwrap(),
            vec![Service::Daemon]
        );
        assert_eq!(
            Service::parse_target("orchestrator").unwrap(),
            vec![Service::Orchestrator]
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
        // The plist embeds ZDX_HOME, which other tests change.
        let _home = crate::test_support::temp_zdx_home();
        let plist = render_plist(
            Service::Bot,
            Path::new("/Users/x/.local/bin/zdx-gateway"),
            Path::new("/Users/x/projects/zdx"),
        );
        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains("<string>dev.zdx.bot</string>"));
        assert!(plist.contains("<string>/bin/zsh</string>"));
        assert!(plist.contains("<string>-c</string>"));
        assert!(plist.contains(
            "<string>exec '/Users/x/.local/bin/zdx-gateway' '--root' '/Users/x/projects/zdx' 'bot'</string>"
        ));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>ThrottleInterval</key>"));
        assert!(plist.contains("<string>launchd</string>"));
        assert!(plist.contains("<key>ExitTimeOut</key>\n    <integer>150</integer>"));
        assert!(plist.contains("<key>ZDX_BIN</key>"));
        assert!(
            !render_plist(Service::Daemon, Path::new("/x/zdx"), Path::new("/p"))
                .contains("ExitTimeOut")
        );
        assert!(plist.contains("bot.out"));
        assert!(plist.contains("bot.err"));
    }

    #[test]
    fn the_bot_runs_the_gateway_and_never_zdx() {
        assert_eq!(
            default_program_for(Service::Bot).file_name().unwrap(),
            "zdx-gateway"
        );
        assert_eq!(default_program_for(Service::Daemon), default_program());
        assert_eq!(
            default_program_for(Service::Orchestrator),
            default_program()
        );
        assert_eq!(
            default_gateway_program().parent(),
            default_program().parent()
        );

        let dir = tempfile::tempdir().unwrap();
        let zdx = dir.path().join("zdx");
        std::fs::write(&zdx, "").unwrap();
        let refused = check_program(Service::Bot, &zdx).unwrap_err().to_string();
        assert!(refused.contains("runs the Go gateway"), "{refused}");
        check_program(Service::Daemon, &zdx).unwrap();

        let missing = dir.path().join("zdx-gateway");
        let refused = check_program(Service::Bot, &missing)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("just gateway-install"), "{refused}");
        std::fs::write(&missing, "").unwrap();
        check_program(Service::Bot, &missing).unwrap();
    }

    #[test]
    fn daemon_plist_runs_jobs_daemon() {
        // The plist embeds ZDX_HOME, which other tests change.
        let _home = crate::test_support::temp_zdx_home();
        let plist = render_plist(
            Service::Daemon,
            Path::new("/Users/x/.local/bin/zdx"),
            Path::new("/Users/x/projects/zdx"),
        );
        assert!(plist.contains("<string>dev.zdx.daemon</string>"));
        assert!(plist.contains(
            "<string>exec '/Users/x/.local/bin/zdx' '--root' '/Users/x/projects/zdx' 'jobs' 'daemon'</string>"
        ));
        assert!(plist.contains("daemon.err"));
    }

    #[test]
    fn orchestrator_plist_runs_the_service() {
        // The plist embeds ZDX_HOME, which other tests change.
        let _home = crate::test_support::temp_zdx_home();
        let plist = render_plist(
            Service::Orchestrator,
            Path::new("/Users/x/.local/bin/zdx"),
            Path::new("/Users/x/projects/zdx"),
        );
        assert!(plist.contains("<string>dev.zdx.orchestrator</string>"));
        assert!(plist.contains(
            "<string>exec '/Users/x/.local/bin/zdx' '--root' '/Users/x/projects/zdx' 'orchestrator' 'serve'</string>"
        ));
        assert!(plist.contains("orchestrator.err"));
    }

    #[test]
    fn shell_quotes_embedded_single_quote() {
        assert_eq!(shell_quote("/tmp/it's"), r"'/tmp/it'\''s'");
    }

    #[test]
    fn parse_launch_reads_back_rendered_plist() {
        // The plist embeds ZDX_HOME, which other tests change.
        let _home = crate::test_support::temp_zdx_home();
        let plist = render_plist(
            Service::Daemon,
            Path::new("/Users/x/.local/bin/zdx"),
            Path::new("/Users/x/it's <work> & co"),
        );
        let launch = parse_launch(&plist).unwrap();
        assert_eq!(launch.root, PathBuf::from("/Users/x/it's <work> & co"));
        assert_eq!(launch.working_directory, launch.root);
        assert_eq!(launch.zdx_home, Some(paths::zdx_home()));

        assert!(parse_launch("<plist></plist>").is_err());
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

    #[test]
    fn restart_guard_blocks_active_agent_unless_forced() {
        let _home = crate::test_support::temp_zdx_home();
        let run = crate::agent_activity::start(crate::agent_activity::StartParams {
            kind: Some("test"),
            ..Default::default()
        })
        .expect("active run marker");

        let err = ensure_restart_allowed("restart", &Service::ALL, false)
            .expect_err("restart must be blocked");
        let blocked = err
            .downcast_ref::<RestartBlocked>()
            .expect("typed restart refusal");
        assert_eq!(blocked.active_runs(), 1);
        assert!(ensure_restart_allowed("restart", &Service::ALL, true).is_ok());

        drop(run);
        assert!(ensure_restart_allowed("restart", &Service::ALL, false).is_ok());
    }

    #[test]
    fn queue_restart_requires_supervised_bot() {
        let _home = crate::test_support::temp_zdx_home();
        let err = queue_restart(&Service::ALL, None).expect_err("no bot to perform it");
        assert!(format!("{err:#}").contains("bot service is not running"));
        assert_eq!(queued_restart().unwrap(), None);
    }

    #[test]
    fn queue_restart_merges_into_pending_request() {
        let _home = crate::test_support::temp_zdx_home();
        let _bot = pidfile::write("bot").unwrap();
        pidfile::mark_supervised("bot").unwrap();

        let first = queue_restart(&[Service::Daemon], Some("telegram--100-topic-1")).unwrap();
        assert!(!first.already_pending);
        assert_eq!(first.request.services, vec![Service::Daemon]);

        let second = queue_restart(&[Service::Bot], None).unwrap();
        assert!(second.already_pending);
        assert_eq!(second.request.services, vec![Service::Bot, Service::Daemon]);
        assert_eq!(
            second.request.thread_id.as_deref(),
            Some("telegram--100-topic-1")
        );
        assert!(second.request.requested_at >= first.request.requested_at);
        assert_eq!(queued_restart().unwrap(), Some(second.request.clone()));
        // The daemon is not running, so the request is still owed.
        assert!(!second.request.is_fulfilled());

        clear_queued_restart();
        assert_eq!(queued_restart().unwrap(), None);
    }

    #[test]
    fn queued_restart_is_fulfilled_by_a_newer_process() {
        let _home = crate::test_support::temp_zdx_home();
        let request = QueuedRestart {
            services: vec![Service::Bot],
            requested_at: Utc::now() - chrono::Duration::hours(1),
            thread_id: None,
        };
        assert!(!request.is_fulfilled());
        let _bot = pidfile::write("bot").unwrap();
        assert!(request.is_fulfilled());
    }

    /// The guard asks a running orchestrator service what it would lose: a
    /// worker busy in it blocks a restart or stop of that service (but not of
    /// the others), names the worker, and `force` still overrides it. Once
    /// the work is delivered, the restart is allowed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_guard_counts_work_in_flight_in_the_orchestrator_service() {
        use std::sync::Arc;

        use tokio::sync::Notify;

        use crate::core::events::{AgentEvent, TurnStatus};
        use crate::core::workers::{WorkerManager, WorkerRunRequest};
        use crate::orchestration::protocol::socket_path;
        use crate::orchestration::runtime::{TurnRequest, TurnRunner};
        use crate::orchestration::{Orchestrator, server};

        let home = crate::test_support::temp_zdx_home();
        let gate = Arc::new(Notify::new());
        let held = Arc::clone(&gate);
        let (workers, events) = WorkerManager::with_runner(Arc::new(move |_: WorkerRunRequest| {
            let gate = Arc::clone(&held);
            Box::pin(async move {
                gate.notified().await;
                Ok("done".to_string())
            })
        }));
        let runner: TurnRunner = Arc::new(|request: TurnRequest| {
            Box::pin(async move {
                let _ = request.events.send(Arc::new(AgentEvent::TurnFinished {
                    status: TurnStatus::Completed,
                    final_text: String::new(),
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            })
        });
        let orchestrator = Orchestrator::start_with(Arc::clone(&workers), events, runner);
        let _pid = pidfile::write(Service::Orchestrator.name()).unwrap();
        let socket = socket_path();
        tokio::spawn(server::serve(Arc::clone(&orchestrator), socket.clone()));
        while !socket.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let worker = workers
            .create_worker("owner", home.path(), "go", None, None, None)
            .unwrap();

        let check = |services: Vec<Service>, force: bool| {
            tokio::task::spawn_blocking(move || ensure_restart_allowed("restart", &services, force))
        };
        let err = check(vec![Service::Orchestrator], false)
            .await
            .unwrap()
            .expect_err("a busy worker blocks the restart");
        let blocked = err
            .downcast_ref::<RestartBlocked>()
            .expect("RestartBlocked");
        assert_eq!(blocked.active_runs(), 0);
        let text = blocked.to_string();
        assert!(
            text.starts_with("restart blocked: the orchestrator service would lose"),
            "{text}"
        );
        assert!(text.contains(&format!("worker {worker}")), "{text}");
        assert!(
            tokio::task::spawn_blocking(|| ensure_restart_allowed(
                "stop",
                &[Service::Orchestrator],
                false
            ))
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .starts_with("stop blocked:")
        );
        check(vec![Service::Daemon], false).await.unwrap().unwrap();
        check(vec![Service::Orchestrator], true)
            .await
            .unwrap()
            .unwrap();

        gate.notify_one();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !orchestrator.settled(None).await {
            assert!(tokio::time::Instant::now() < deadline, "never settled");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        check(vec![Service::Orchestrator], false)
            .await
            .unwrap()
            .unwrap();
    }
}
