//! Lifetime supervision for invocation-owned Unix process groups.
//!
//! The caller retains one lease endpoint. A native supervisor in a separate
//! session owns and reaps the target process. If the caller drops the lease or
//! dies, the supervisor terminates the target group before it exits.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const LEASE_FD: RawFd = 3;
const COMPLETION_FD: RawFd = 4;
const STDOUT_FD: RawFd = 5;
const STDERR_FD: RawFd = 6;
const EXEC_ERROR_FD: RawFd = 7;
const GATE_READ_FD: RawFd = 8;
const GATE_WRITE_FD: RawFd = 9;
const TARGET_STDIN_FD: RawFd = 10;
const FIRST_UNUSED_FD: RawFd = 11;
const POLL_INTERVAL_MS: i32 = 25;

/// Startup frame tags written on `EXEC_ERROR_FD`.
///
/// The supervisor reports the target's identity once it is forked and grouped;
/// either side reports a startup failure as an errno. Frames are tagged because
/// both processes hold the descriptor until the target execs.
const STARTUP_ERROR: u8 = 0;
const STARTUP_READY: u8 = 1;
const STARTUP_ERROR_LEN: usize = 5;
const STARTUP_READY_LEN: usize = 9;

/// Why a supervised wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    Exited,
    Cancelled,
    TimedOut,
}

/// OS identity of a supervised target, reported by the supervisor at startup.
///
/// The target is its own process-group leader, so `pgid == pid`. Callers need
/// both to register the process outside this handle (liveness, identity guards,
/// group signalling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetIdentity {
    pub pid: u32,
    pub pgid: i32,
}

/// Result of a wait that carries a non-lethal foreground bound.
#[derive(Debug)]
pub enum BoundedWait {
    /// The target reached a terminal state.
    Finished(WaitOutcome),
    /// The bound elapsed. The target is untouched and still supervised: this
    /// handle keeps its lease and completion, so it can be handed off.
    Bounded,
}

/// Result of waiting for a supervised target and its final group sweep.
#[derive(Debug)]
pub struct WaitOutcome {
    pub status: ExitStatus,
    pub reason: WaitReason,
    pub cleaned_leftovers: bool,
}

/// Narrow command builder for an invocation-owned child process.
#[derive(Debug)]
pub struct SupervisedCommand {
    program: OsString,
    args: Vec<OsString>,
    cwd: PathBuf,
    env_overrides: Vec<(OsString, OsString)>,
    stdin_null: bool,
}

impl SupervisedCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            cwd: PathBuf::from("."),
            env_overrides: Vec::new(),
            stdin_null: false,
        }
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    pub fn current_dir(&mut self, cwd: impl AsRef<Path>) -> &mut Self {
        self.cwd = cwd.as_ref().to_path_buf();
        self
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.env_overrides
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    pub fn stdin_null(&mut self) -> &mut Self {
        self.stdin_null = true;
        self
    }

    /// Spawns the target behind a lease-backed external supervisor.
    ///
    /// `shutdown_grace` is used only after cancellation or after the direct
    /// target exits with same-group descendants still alive. It is not an
    /// execution deadline.
    ///
    /// # Errors
    /// Returns an error when command preparation, process creation, or target
    /// startup fails.
    pub async fn spawn(self, shutdown_grace: Duration) -> io::Result<SupervisedChild> {
        spawn(self, shutdown_grace).await
    }
}

/// Invocation-owned target plus the lease held only by its owner.
pub struct SupervisedChild {
    pub stdout: Option<tokio::process::ChildStdout>,
    pub stderr: Option<tokio::process::ChildStderr>,
    /// OS identity of the target, for registering it outside this handle.
    pub identity: TargetIdentity,
    lease: Option<StdUnixStream>,
    completion: Option<JoinHandle<io::Result<CompletionRecord>>>,
}

impl std::fmt::Debug for SupervisedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupervisedChild")
            .field("identity", &self.identity)
            .field("lease_held", &self.lease.is_some())
            .field("completion_pending", &self.completion.is_some())
            .finish_non_exhaustive()
    }
}

impl SupervisedChild {
    /// Closes the owner lease, asking the supervisor to terminate the group.
    pub fn cancel(&mut self) {
        self.lease.take();
    }

    /// Waits without imposing any deadline while the owner remains alive.
    ///
    /// # Errors
    /// Returns an error if the supervisor fails or cannot be reaped.
    pub async fn wait(&mut self) -> io::Result<WaitOutcome> {
        self.wait_with(None, None).await
    }

    /// Waits for normal completion, cancellation, or an explicit execution
    /// timeout. Cancellation/timeout closes the lease and then awaits teardown.
    ///
    /// # Errors
    /// Returns an error if the supervisor fails or cannot be reaped.
    pub async fn wait_with(
        &mut self,
        cancel: Option<&CancellationToken>,
        timeout: Option<Duration>,
    ) -> io::Result<WaitOutcome> {
        match self.wait_bounded(cancel, timeout, None).await? {
            BoundedWait::Finished(outcome) => Ok(outcome),
            // `bound: None` never elapses.
            BoundedWait::Bounded => Err(io::Error::other("unbounded wait reported a bound")),
        }
    }

    /// Waits like [`Self::wait_with`], plus a non-lethal foreground `bound`.
    ///
    /// The bound is not a deadline: when it elapses the target keeps running
    /// untouched and this handle keeps its lease, completion, and pipes, so the
    /// caller can hand the job off to the background. Completion is polled
    /// first, so a target finishing right at the bound is reported as finished.
    ///
    /// # Errors
    /// Returns an error if the supervisor fails or cannot be reaped.
    pub async fn wait_bounded(
        &mut self,
        cancel: Option<&CancellationToken>,
        timeout: Option<Duration>,
        bound: Option<Duration>,
    ) -> io::Result<BoundedWait> {
        let mut completion = self
            .completion
            .take()
            .ok_or_else(|| io::Error::other("supervised child was already waited"))?;

        let step = tokio::select! {
            biased;
            result = &mut completion => WaitStep::Finished(join_completion(result)?),
            () = cancelled_or_pending(cancel) => WaitStep::Interrupted(WaitReason::Cancelled),
            () = elapsed_or_pending(timeout) => WaitStep::Interrupted(WaitReason::TimedOut),
            () = elapsed_or_pending(bound) => WaitStep::Bounded,
        };

        match step {
            WaitStep::Finished(record) => {
                self.lease.take();
                Ok(BoundedWait::Finished(
                    record.into_outcome(WaitReason::Exited),
                ))
            }
            WaitStep::Bounded => {
                // The target is untouched, so hand the handle back intact: the
                // caller can keep waiting on it or adopt it as it stands.
                self.completion = Some(completion);
                Ok(BoundedWait::Bounded)
            }
            WaitStep::Interrupted(reason) => {
                self.cancel();
                let record = join_completion(completion.await)?;
                Ok(BoundedWait::Finished(record.into_outcome(reason)))
            }
        }
    }
}

/// How one pass of [`SupervisedChild::wait_bounded`]'s select ended.
enum WaitStep {
    Finished(CompletionRecord),
    Interrupted(WaitReason),
    Bounded,
}

/// Resolves when `cancel` is cancelled, or never when there is no token.
async fn cancelled_or_pending(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

/// Resolves after `duration`, or never when there is none.
async fn elapsed_or_pending(duration: Option<Duration>) {
    match duration {
        Some(duration) => tokio::time::sleep(duration).await,
        None => std::future::pending().await,
    }
}

fn join_completion(
    result: Result<io::Result<CompletionRecord>, tokio::task::JoinError>,
) -> io::Result<CompletionRecord> {
    result.map_err(|err| io::Error::other(format!("process supervisor task failed: {err}")))?
}

#[derive(Debug, Clone, Copy)]
struct CompletionRecord {
    raw_status: i32,
    cleaned_leftovers: bool,
}

impl CompletionRecord {
    fn into_outcome(self, reason: WaitReason) -> WaitOutcome {
        use std::os::unix::process::ExitStatusExt as _;

        WaitOutcome {
            status: ExitStatus::from_raw(self.raw_status),
            reason,
            cleaned_leftovers: self.cleaned_leftovers,
        }
    }
}

struct PreparedCommand {
    program: CString,
    argv: Vec<CString>,
    argv_ptrs: Vec<usize>,
    env: Vec<CString>,
    env_ptrs: Vec<usize>,
    cwd: CString,
}

impl PreparedCommand {
    fn new(command: &SupervisedCommand) -> io::Result<Self> {
        let program = cstring(command.program.as_os_str(), "program")?;
        let mut argv = Vec::with_capacity(command.args.len() + 1);
        argv.push(program.clone());
        for arg in &command.args {
            argv.push(cstring(arg, "argument")?);
        }
        let mut argv_ptrs: Vec<usize> = argv.iter().map(|value| value.as_ptr() as usize).collect();
        argv_ptrs.push(0);

        let env = build_environment(&command.env_overrides)?;
        let mut env_ptrs: Vec<usize> = env.iter().map(|value| value.as_ptr() as usize).collect();
        env_ptrs.push(0);

        Ok(Self {
            program,
            argv,
            argv_ptrs,
            env,
            env_ptrs,
            cwd: cstring(command.cwd.as_os_str(), "working directory")?,
        })
    }

    fn keep_allocations_alive(&self) {
        let _ = (&self.argv, &self.env);
    }
}

fn cstring(value: &OsStr, field: &str) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_err| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{field} contains a NUL byte"),
        )
    })
}

fn build_environment(overrides: &[(OsString, OsString)]) -> io::Result<Vec<CString>> {
    let mut values: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (key, value) in overrides {
        if key.as_bytes().contains(&b'=') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "environment key contains '='",
            ));
        }
        if let Some((_, existing)) = values.iter_mut().find(|(name, _)| name == key) {
            existing.clone_from(value);
        } else {
            values.push((key.clone(), value.clone()));
        }
    }

    values
        .into_iter()
        .map(|(key, value)| {
            let mut entry = key.into_vec();
            entry.push(b'=');
            entry.extend(value.into_vec());
            CString::new(entry).map_err(|_err| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "environment contains a NUL byte",
                )
            })
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
async fn spawn(
    command: SupervisedCommand,
    shutdown_grace: Duration,
) -> io::Result<SupervisedChild> {
    let prepared = PreparedCommand::new(&command)?;

    let (lease_owner, lease_supervisor) = StdUnixStream::pair()?;
    let (completion_owner, completion_supervisor) = StdUnixStream::pair()?;
    let (stdout_owner, stdout_target) = std::io::pipe()?;
    let (stderr_owner, stderr_target) = std::io::pipe()?;
    let (exec_error_owner, exec_error_target) = StdUnixStream::pair()?;
    let (gate_supervisor, gate_target) = StdUnixStream::pair()?;

    let stdin_file = if command.stdin_null {
        File::open("/dev/null")?
    } else {
        duplicate_stdin_or_null()?
    };

    let source_fds = [
        lease_supervisor.as_raw_fd(),
        completion_supervisor.as_raw_fd(),
        stdout_target.as_raw_fd(),
        stderr_target.as_raw_fd(),
        exec_error_target.as_raw_fd(),
        gate_target.as_raw_fd(),
        gate_supervisor.as_raw_fd(),
        stdin_file.as_raw_fd(),
    ];
    let high_fds: Vec<OwnedFd> = source_fds
        .into_iter()
        .map(duplicate_high)
        .collect::<io::Result<_>>()?;
    let high_raw: Vec<RawFd> = high_fds.iter().map(AsRawFd::as_raw_fd).collect();
    let max_fd = max_open_fd();
    let grace_ms = shutdown_grace.as_millis().min(i32::MAX as u128) as i32;

    let old_signal_mask = block_signals()?;
    // SAFETY: both child branches call only libc/syscall-style operations and
    // `_exit`; all strings, pointer arrays, descriptors, and limits were
    // prepared before forking the multi-threaded owner. Signals stay blocked
    // until the target has removed inherited handlers.
    let supervisor_pid = unsafe { libc::fork() };
    let fork_errno = (supervisor_pid == -1).then(last_errno);
    if supervisor_pid != 0 {
        restore_signal_mask(&old_signal_mask)?;
    }
    if let Some(errno) = fork_errno {
        return Err(io::Error::from_raw_os_error(errno));
    }
    if supervisor_pid == 0 {
        // SAFETY: this function never returns and follows the post-fork
        // restrictions documented above.
        unsafe {
            run_supervisor(&prepared, &high_raw, max_fd, grace_ms, old_signal_mask);
        }
    }

    drop(high_fds);
    drop(lease_supervisor);
    drop(completion_supervisor);
    drop(stdout_target);
    drop(stderr_target);
    drop(exec_error_target);
    drop(gate_supervisor);
    drop(gate_target);
    drop(stdin_file);
    drop(prepared);

    let completion = tokio::task::spawn_blocking(move || {
        read_completion_and_reap(completion_owner, supervisor_pid)
    });

    let startup = tokio::task::spawn_blocking(move || read_startup(exec_error_owner))
        .await
        .map_err(|err| io::Error::other(format!("process startup task failed: {err}")))?;
    let identity = match startup {
        Ok(identity) => identity,
        Err(err) => {
            drop(lease_owner);
            let _ = completion.await;
            return Err(err);
        }
    };

    let stdout = tokio::process::ChildStdout::from_std(std::process::ChildStdout::from(
        OwnedFd::from(stdout_owner),
    ))?;
    let stderr = tokio::process::ChildStderr::from_std(std::process::ChildStderr::from(
        OwnedFd::from(stderr_owner),
    ))?;

    Ok(SupervisedChild {
        stdout: Some(stdout),
        stderr: Some(stderr),
        identity,
        lease: Some(lease_owner),
        completion: Some(completion),
    })
}

fn duplicate_stdin_or_null() -> io::Result<File> {
    let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
    if fd >= 0 {
        // SAFETY: fcntl returned a new owned descriptor.
        return Ok(unsafe { File::from_raw_fd(fd) });
    }
    File::open("/dev/null")
}

fn duplicate_high(fd: RawFd) -> io::Result<OwnedFd> {
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 64) };
    if duplicated == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

fn max_open_fd() -> RawFd {
    let value = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    if value <= 0 {
        65_536
    } else {
        value.min(i64::from(i32::MAX)) as RawFd
    }
}

/// Parses the startup frames written on `EXEC_ERROR_FD`.
///
/// Both the supervisor and the target hold the write end until the target
/// execs, so the stream can carry a ready frame followed by the target's own
/// failure. Any error frame wins; otherwise the reported identity is returned.
#[allow(clippy::similar_names)] // pid / pgid are the real domain names here
fn read_startup(mut stream: StdUnixStream) -> io::Result<TargetIdentity> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;

    let mut identity = None;
    let mut cursor = 0;
    while cursor < bytes.len() {
        let rest = &bytes[cursor..];
        match rest[0] {
            STARTUP_ERROR if rest.len() >= STARTUP_ERROR_LEN => {
                let errno = i32::from_ne_bytes(rest[1..5].try_into().expect("length checked"));
                return Err(io::Error::from_raw_os_error(errno));
            }
            STARTUP_READY if rest.len() >= STARTUP_READY_LEN => {
                let pid = i32::from_ne_bytes(rest[1..5].try_into().expect("length checked"));
                let pgid = i32::from_ne_bytes(rest[5..9].try_into().expect("length checked"));
                identity = Some(TargetIdentity {
                    pid: pid.unsigned_abs(),
                    pgid,
                });
                cursor += STARTUP_READY_LEN;
            }
            _ => return Err(io::Error::other("invalid process startup response")),
        }
    }

    identity.ok_or_else(|| io::Error::other("process supervisor did not report target identity"))
}

fn read_completion_and_reap(
    mut stream: StdUnixStream,
    supervisor_pid: libc::pid_t,
) -> io::Result<CompletionRecord> {
    let mut bytes = [0_u8; 8];
    let read_result = stream.read_exact(&mut bytes);
    let supervisor_status = waitpid_blocking(supervisor_pid)?;
    read_result?;

    if !libc::WIFEXITED(supervisor_status) || libc::WEXITSTATUS(supervisor_status) != 0 {
        return Err(io::Error::other("process supervisor exited unexpectedly"));
    }

    Ok(CompletionRecord {
        raw_status: i32::from_ne_bytes(bytes[..4].try_into().expect("fixed slice")),
        cleaned_leftovers: i32::from_ne_bytes(bytes[4..].try_into().expect("fixed slice")) != 0,
    })
}

fn waitpid_blocking(pid: libc::pid_t) -> io::Result<i32> {
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &raw mut status, 0) };
        if result == pid {
            return Ok(status);
        }
        if result == -1 && last_errno() == libc::EINTR {
            continue;
        }
        return Err(io::Error::last_os_error());
    }
}

/// Runs in the first post-fork child and never returns.
unsafe fn run_supervisor(
    command: &PreparedCommand,
    sources: &[RawFd],
    max_fd: RawFd,
    grace_ms: i32,
    old_signal_mask: libc::sigset_t,
) -> ! {
    command.keep_allocations_alive();
    let fixed = [
        LEASE_FD,
        COMPLETION_FD,
        STDOUT_FD,
        STDERR_FD,
        EXEC_ERROR_FD,
        GATE_READ_FD,
        GATE_WRITE_FD,
        TARGET_STDIN_FD,
    ];
    for (&source, &target) in sources.iter().zip(&fixed) {
        if unsafe { libc::dup2(source, target) } == -1 {
            unsafe { report_errno_and_exit(sources[4], last_errno()) };
        }
    }
    unsafe { close_unrelated_fds(max_fd) };
    unsafe {
        libc::close(libc::STDIN_FILENO);
        libc::close(libc::STDOUT_FILENO);
        libc::close(libc::STDERR_FILENO);
    }

    if unsafe { libc::setsid() } == -1 {
        unsafe { report_errno_and_exit(EXEC_ERROR_FD, last_errno()) };
    }
    unsafe { reset_supervisor_signal_state() };

    let target_pid = unsafe { libc::fork() };
    if target_pid == -1 {
        unsafe { report_errno_and_exit(EXEC_ERROR_FD, last_errno()) };
    }
    if target_pid == 0 {
        unsafe { run_target(command, old_signal_mask) };
    }

    unsafe {
        libc::close(STDOUT_FD);
        libc::close(STDERR_FD);
        libc::close(GATE_READ_FD);
        libc::close(TARGET_STDIN_FD);
    }

    if unsafe { libc::setpgid(target_pid, target_pid) } == -1 {
        let errno = last_errno();
        unsafe {
            libc::kill(target_pid, libc::SIGKILL);
            reap_target(target_pid);
            report_errno_and_exit(EXEC_ERROR_FD, errno);
        }
    }
    if unsafe { write_all_fd(GATE_WRITE_FD, &[1]) } == -1 {
        let errno = last_errno();
        unsafe {
            libc::kill(-target_pid, libc::SIGKILL);
            reap_target(target_pid);
            report_errno_and_exit(EXEC_ERROR_FD, errno);
        }
    }
    // The target is forked and is its own group leader, so `pgid == pid`.
    // Report it before the gated target can exec away from this descriptor.
    let mut ready = [0_u8; STARTUP_READY_LEN];
    ready[0] = STARTUP_READY;
    ready[1..5].copy_from_slice(&target_pid.to_ne_bytes());
    ready[5..].copy_from_slice(&target_pid.to_ne_bytes());
    if unsafe { write_all_fd(EXEC_ERROR_FD, &ready) } == -1 {
        let errno = last_errno();
        unsafe {
            libc::kill(-target_pid, libc::SIGKILL);
            reap_target(target_pid);
            report_errno_and_exit(EXEC_ERROR_FD, errno);
        }
    }
    unsafe {
        libc::close(GATE_WRITE_FD);
        libc::close(EXEC_ERROR_FD);
    }

    let (raw_status, cleaned_leftovers) = unsafe { supervise_target(target_pid, grace_ms) };
    let mut record = [0_u8; 8];
    record[..4].copy_from_slice(&raw_status.to_ne_bytes());
    record[4..].copy_from_slice(&i32::from(cleaned_leftovers).to_ne_bytes());
    unsafe {
        let _ = write_all_fd(COMPLETION_FD, &record);
        libc::close(COMPLETION_FD);
        libc::close(LEASE_FD);
        libc::_exit(0);
    }
}

unsafe fn run_target(command: &PreparedCommand, old_signal_mask: libc::sigset_t) -> ! {
    unsafe {
        libc::close(LEASE_FD);
        libc::close(COMPLETION_FD);
        libc::close(GATE_WRITE_FD);
    }
    if unsafe { libc::setpgid(0, 0) } == -1 {
        unsafe { report_errno_and_exit(EXEC_ERROR_FD, last_errno()) };
    }

    let mut gate = [0_u8; 1];
    loop {
        let result = unsafe { libc::read(GATE_READ_FD, gate.as_mut_ptr().cast(), 1) };
        if result == 1 {
            break;
        }
        if result == -1 && last_errno() == libc::EINTR {
            continue;
        }
        unsafe { report_errno_and_exit(EXEC_ERROR_FD, libc::ECANCELED) };
    }
    unsafe { libc::close(GATE_READ_FD) };

    for (source, target) in [
        (TARGET_STDIN_FD, libc::STDIN_FILENO),
        (STDOUT_FD, libc::STDOUT_FILENO),
        (STDERR_FD, libc::STDERR_FILENO),
    ] {
        if unsafe { libc::dup2(source, target) } == -1 {
            unsafe { report_errno_and_exit(EXEC_ERROR_FD, last_errno()) };
        }
    }
    unsafe {
        libc::close(STDOUT_FD);
        libc::close(STDERR_FD);
        libc::close(TARGET_STDIN_FD);
    }
    if unsafe { libc::fcntl(EXEC_ERROR_FD, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        unsafe { report_errno_and_exit(EXEC_ERROR_FD, last_errno()) };
    }
    if unsafe { libc::chdir(command.cwd.as_ptr()) } == -1 {
        unsafe { report_errno_and_exit(EXEC_ERROR_FD, last_errno()) };
    }
    unsafe { reset_target_signal_state(old_signal_mask) };
    unsafe {
        libc::execve(
            command.program.as_ptr(),
            command.argv_ptrs.as_ptr().cast(),
            command.env_ptrs.as_ptr().cast(),
        );
        report_errno_and_exit(EXEC_ERROR_FD, last_errno());
    }
}

unsafe fn supervise_target(target_pid: libc::pid_t, grace_ms: i32) -> (i32, bool) {
    loop {
        let mut status = 0;
        let waited = unsafe { libc::waitpid(target_pid, &raw mut status, libc::WNOHANG) };
        if waited == target_pid {
            let leftovers = unsafe { group_exists(target_pid) };
            if leftovers {
                unsafe { terminate_group(target_pid, grace_ms) };
            }
            return (status, leftovers);
        }
        if waited == -1 && last_errno() != libc::EINTR {
            unsafe { libc::_exit(126) };
        }

        let mut lease_poll = libc::pollfd {
            fd: LEASE_FD,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&raw mut lease_poll, 1, POLL_INTERVAL_MS) };
        if polled == -1 && last_errno() == libc::EINTR {
            continue;
        }
        if polled == -1 {
            unsafe { libc::_exit(126) };
        }
        if polled > 0 {
            let mut byte = [0_u8; 1];
            let read = unsafe { libc::read(LEASE_FD, byte.as_mut_ptr().cast(), 1) };
            if read == 0 {
                let status = unsafe { terminate_target_group(target_pid, grace_ms) };
                return (status, false);
            }
            if read == -1 && last_errno() != libc::EINTR {
                let status = unsafe { terminate_target_group(target_pid, grace_ms) };
                return (status, false);
            }
        }
    }
}

unsafe fn terminate_target_group(target_pid: libc::pid_t, grace_ms: i32) -> i32 {
    unsafe { libc::kill(-target_pid, libc::SIGTERM) };
    let deadline = monotonic_millis().saturating_add(i64::from(grace_ms.max(0)));
    let mut raw_status = None;

    loop {
        if raw_status.is_none() {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(target_pid, &raw mut status, libc::WNOHANG) };
            if waited == target_pid {
                raw_status = Some(status);
            } else if waited == -1 && last_errno() != libc::EINTR {
                unsafe { libc::_exit(126) };
            }
        }

        if let Some(status) = raw_status
            && !unsafe { group_exists(target_pid) }
        {
            return status;
        }
        if monotonic_millis() >= deadline {
            break;
        }
        let remaining = deadline.saturating_sub(monotonic_millis());
        let delay = remaining.min(i64::from(POLL_INTERVAL_MS)).max(1) as i32;
        unsafe {
            libc::poll(std::ptr::null_mut(), 0, delay);
        }
    }

    unsafe { libc::kill(-target_pid, libc::SIGKILL) };
    let status = match raw_status {
        Some(status) => status,
        None => unsafe { reap_target(target_pid) },
    };
    unsafe { libc::kill(-target_pid, libc::SIGKILL) };
    status
}

unsafe fn terminate_group(target_pid: libc::pid_t, grace_ms: i32) {
    unsafe { libc::kill(-target_pid, libc::SIGTERM) };
    let deadline = monotonic_millis().saturating_add(i64::from(grace_ms.max(0)));
    while unsafe { group_exists(target_pid) } && monotonic_millis() < deadline {
        let remaining = deadline.saturating_sub(monotonic_millis());
        let delay = remaining.min(i64::from(POLL_INTERVAL_MS)).max(1) as i32;
        unsafe {
            libc::poll(std::ptr::null_mut(), 0, delay);
        }
    }
    if unsafe { group_exists(target_pid) } {
        unsafe { libc::kill(-target_pid, libc::SIGKILL) };
    }
}

unsafe fn reap_target(target_pid: libc::pid_t) -> i32 {
    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(target_pid, &raw mut status, 0) };
        if waited == target_pid {
            unsafe { libc::kill(-target_pid, libc::SIGKILL) };
            return status;
        }
        if waited == -1 && last_errno() == libc::EINTR {
            continue;
        }
        unsafe { libc::_exit(126) };
    }
}

unsafe fn group_exists(pgid: libc::pid_t) -> bool {
    if unsafe { libc::kill(-pgid, 0) } == 0 {
        return true;
    }
    last_errno() == libc::EPERM
}

fn monotonic_millis() -> i64 {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, time.as_mut_ptr()) } == -1 {
        return 0;
    }
    // SAFETY: clock_gettime initialized the value on success.
    let time = unsafe { time.assume_init() };
    time.tv_sec
        .saturating_mul(1_000)
        .saturating_add(time.tv_nsec / 1_000_000)
}

unsafe fn close_unrelated_fds(max_fd: RawFd) {
    #[cfg(target_os = "linux")]
    {
        let result = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                FIRST_UNUSED_FD as libc::c_uint,
                libc::c_uint::MAX,
                0,
            )
        };
        if result == 0 {
            return;
        }
    }

    for fd in FIRST_UNUSED_FD..max_fd {
        unsafe { libc::close(fd) };
    }
}

fn block_signals() -> io::Result<libc::sigset_t> {
    let mut all = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    let mut old = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    let result = unsafe {
        libc::sigfillset(all.as_mut_ptr());
        libc::pthread_sigmask(libc::SIG_SETMASK, all.as_ptr(), old.as_mut_ptr())
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    // SAFETY: pthread_sigmask initialized `old` on success.
    Ok(unsafe { old.assume_init() })
}

fn restore_signal_mask(mask: &libc::sigset_t) -> io::Result<()> {
    let result = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, mask, std::ptr::null_mut()) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result))
    }
}

unsafe fn reset_supervisor_signal_state() {
    let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
    action.sa_sigaction = libc::SIG_DFL;
    unsafe {
        libc::sigemptyset(&raw mut action.sa_mask);
        libc::sigaction(libc::SIGCHLD, &raw const action, std::ptr::null_mut());
    }
}

unsafe fn reset_target_signal_state(old_signal_mask: libc::sigset_t) {
    for signal in 1..128 {
        if signal == libc::SIGKILL || signal == libc::SIGSTOP {
            continue;
        }
        let mut inherited = unsafe { std::mem::zeroed::<libc::sigaction>() };
        if unsafe { libc::sigaction(signal, std::ptr::null(), &raw mut inherited) } == -1 {
            continue;
        }
        if signal == libc::SIGPIPE || inherited.sa_sigaction != libc::SIG_IGN {
            let mut default = unsafe { std::mem::zeroed::<libc::sigaction>() };
            default.sa_sigaction = libc::SIG_DFL;
            unsafe {
                libc::sigemptyset(&raw mut default.sa_mask);
                libc::sigaction(signal, &raw const default, std::ptr::null_mut());
            }
        }
    }
    unsafe {
        libc::pthread_sigmask(
            libc::SIG_SETMASK,
            &raw const old_signal_mask,
            std::ptr::null_mut(),
        );
    }
}

unsafe fn report_errno_and_exit(fd: RawFd, errno: i32) -> ! {
    let mut frame = [0_u8; STARTUP_ERROR_LEN];
    frame[0] = STARTUP_ERROR;
    frame[1..].copy_from_slice(&errno.to_ne_bytes());
    unsafe {
        let _ = write_all_fd(fd, &frame);
        libc::_exit(127);
    }
}

unsafe fn write_all_fd(fd: RawFd, mut bytes: &[u8]) -> isize {
    while !bytes.is_empty() {
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written > 0 {
            bytes = &bytes[written as usize..];
            continue;
        }
        if written == -1 && last_errno() == libc::EINTR {
            continue;
        }
        return -1;
    }
    0
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn last_errno() -> i32 {
    // SAFETY: libc exposes thread-local errno through __error on Apple targets.
    unsafe { *libc::__error() }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn last_errno() -> i32 {
    // SAFETY: libc exposes thread-local errno through __errno_location.
    unsafe { *libc::__errno_location() }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
)))]
fn last_errno() -> i32 {
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::Stdio;
    use std::time::Instant;

    use tempfile::TempDir;
    use tokio::io::AsyncReadExt as _;

    use super::*;

    async fn wait_for_pid(path: &Path) -> i32 {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(raw) = std::fs::read_to_string(path)
                    && let Ok(pid) = raw.trim().parse()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("target pid was not published")
    }

    async fn wait_until_gone(pid: i32) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while process_exists(pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("target process survived cleanup");
    }

    fn process_exists(pid: i32) -> bool {
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn shell_command(script: &str, pidfile: &Path) -> SupervisedCommand {
        let mut command = SupervisedCommand::new("/bin/sh");
        command.args([
            OsString::from("-c"),
            OsString::from(script),
            OsString::from("supervised-fixture"),
            pidfile.as_os_str().to_os_string(),
        ]);
        command
    }

    #[tokio::test]
    async fn preserves_output_and_exit_status() {
        let mut command = SupervisedCommand::new("/bin/sh");
        command.args(["-c", "printf stdout; printf stderr >&2; exit 7"]);
        let mut child = command.spawn(Duration::from_millis(100)).await.unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let stdout_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        let stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).await.unwrap();
            bytes
        });

        let outcome = child.wait().await.unwrap();
        assert_eq!(outcome.status.code(), Some(7));
        assert_eq!(stdout_task.await.unwrap(), b"stdout");
        assert_eq!(stderr_task.await.unwrap(), b"stderr");
    }

    #[tokio::test]
    async fn stdout_and_stderr_are_reopenable_pipes() {
        let mut command = SupervisedCommand::new("/bin/sh");
        command.args([
            "-c",
            "printf reopened-out > /dev/stdout; printf reopened-err > /dev/stderr",
        ]);
        let mut child = command.spawn(Duration::from_millis(100)).await.unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let stdout_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        let stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).await.unwrap();
            bytes
        });

        assert!(child.wait().await.unwrap().status.success());
        assert_eq!(stdout_task.await.unwrap(), b"reopened-out");
        assert_eq!(stderr_task.await.unwrap(), b"reopened-err");
    }

    #[tokio::test]
    async fn preserves_signal_status() {
        let mut command = SupervisedCommand::new("/bin/sh");
        command.args(["-c", "kill -KILL $$"]);
        let mut child = command.spawn(Duration::from_millis(100)).await.unwrap();

        let outcome = child.wait().await.unwrap();
        assert_eq!(outcome.status.signal(), Some(libc::SIGKILL));
        assert_eq!(outcome.status.code(), None);
    }

    #[tokio::test]
    async fn restores_default_sigpipe_disposition() {
        let mut command = SupervisedCommand::new("/bin/sh");
        command.args(["-c", "kill -PIPE $$; exit 23"]);
        let mut child = command.spawn(Duration::from_millis(100)).await.unwrap();

        let outcome = child.wait().await.unwrap();
        assert_eq!(outcome.status.signal(), Some(libc::SIGPIPE));
        assert_eq!(outcome.status.code(), None);
    }

    #[tokio::test]
    async fn reports_the_target_identity() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("target.pid");
        let mut command = shell_command("echo $$ > \"$1\"; sleep 0.2", &pidfile);
        command.stdin_null();
        let mut child = command.spawn(Duration::from_millis(100)).await.unwrap();
        let pid = wait_for_pid(&pidfile).await;

        assert_eq!(child.identity.pid, pid as u32);
        // The target leads its own process group.
        assert_eq!(child.identity.pgid, pid);
        assert!(child.wait().await.unwrap().status.success());
    }

    #[tokio::test]
    async fn bound_leaves_the_target_running_and_waitable() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("target.pid");
        let mut command = shell_command("echo $$ > \"$1\"; sleep 1; exit 9", &pidfile);
        command.stdin_null();
        let mut child = command.spawn(Duration::from_millis(100)).await.unwrap();
        let pid = wait_for_pid(&pidfile).await;

        let bounded = child
            .wait_bounded(None, None, Some(Duration::from_millis(100)))
            .await
            .unwrap();
        assert!(matches!(bounded, BoundedWait::Bounded));
        assert!(process_exists(pid), "a bound must not touch the target");

        // The handle is intact, so the same child can still be waited on.
        let outcome = child.wait().await.unwrap();
        assert_eq!(outcome.reason, WaitReason::Exited);
        assert_eq!(outcome.status.code(), Some(9));
    }

    #[tokio::test]
    async fn completion_wins_a_bound_that_expires_at_the_same_time() {
        let mut command = SupervisedCommand::new("/bin/sh");
        command.args(["-c", "exit 3"]);
        let mut child = command.spawn(Duration::from_millis(100)).await.unwrap();

        // Zero bound: the biased select must still report the finished target
        // rather than relocating a command that already exited.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let bounded = child
            .wait_bounded(None, None, Some(Duration::ZERO))
            .await
            .unwrap();
        match bounded {
            BoundedWait::Finished(outcome) => assert_eq!(outcome.status.code(), Some(3)),
            BoundedWait::Bounded => panic!("a finished target must not be reported as bounded"),
        }
    }

    #[tokio::test]
    async fn startup_failure_returns_without_leaking() {
        let command = SupervisedCommand::new("/zdx-fixture-does-not-exist");
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            command.spawn(Duration::from_millis(100)),
        )
        .await
        .expect("startup failure hung");
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ENOENT));
    }

    #[tokio::test]
    async fn cancellation_escalates_and_reaps_target() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("target.pid");
        let mut command = shell_command(
            "trap '' TERM; echo $$ > \"$1\"; while :; do sleep 1; done",
            &pidfile,
        );
        command.stdin_null();
        let mut child = command.spawn(Duration::from_millis(150)).await.unwrap();
        let pid = wait_for_pid(&pidfile).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let started = Instant::now();

        let outcome = child.wait_with(Some(&cancel), None).await.unwrap();
        assert_eq!(outcome.reason, WaitReason::Cancelled);
        assert_eq!(outcome.status.signal(), Some(libc::SIGKILL));
        assert!(started.elapsed() >= Duration::from_millis(100));
        wait_until_gone(pid).await;
    }

    #[tokio::test]
    async fn dropping_owner_handle_closes_lease() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("target.pid");
        let command = shell_command("echo $$ > \"$1\"; while :; do sleep 1; done", &pidfile);
        let child = command.spawn(Duration::from_millis(150)).await.unwrap();
        let pid = wait_for_pid(&pidfile).await;

        drop(child);
        wait_until_gone(pid).await;
    }

    #[tokio::test]
    async fn healthy_target_has_no_supervision_deadline() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("target.pid");
        let command = shell_command("echo $$ > \"$1\"; sleep 1", &pidfile);
        let mut child = command.spawn(Duration::from_millis(100)).await.unwrap();
        let pid = wait_for_pid(&pidfile).await;

        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(process_exists(pid));
        assert!(child.wait().await.unwrap().status.success());
    }

    #[tokio::test]
    async fn independent_leases_do_not_retain_each_other() {
        let temp = TempDir::new().unwrap();
        let first_pidfile = temp.path().join("first.pid");
        let second_pidfile = temp.path().join("second.pid");
        let first = shell_command(
            "echo $$ > \"$1\"; while :; do sleep 1; done",
            &first_pidfile,
        );
        let second = shell_command("echo $$ > \"$1\"; sleep 1", &second_pidfile);
        let first = first.spawn(Duration::from_millis(100)).await.unwrap();
        let mut second = second.spawn(Duration::from_millis(100)).await.unwrap();
        let first_pid = wait_for_pid(&first_pidfile).await;
        let second_pid = wait_for_pid(&second_pidfile).await;

        drop(first);
        wait_until_gone(first_pid).await;
        assert!(process_exists(second_pid));
        assert!(second.wait().await.unwrap().status.success());
    }

    #[tokio::test]
    async fn aborted_reader_releases_pipe_held_by_escaped_descendant() {
        let temp = TempDir::new().unwrap();
        let ready = temp.path().join("escaped.pid");
        let mut command = SupervisedCommand::new("/bin/sh");
        command
            .args([
                OsString::from("-c"),
                OsString::from(
                    "\"$1\" process_supervisor::tests::escaped_pipe_holder_fixture --exact --ignored --nocapture & while [ ! -s \"$2\" ]; do sleep 0.02; done",
                ),
                OsString::from("supervised-fixture"),
                std::env::current_exe().unwrap().into_os_string(),
                ready.as_os_str().to_os_string(),
            ])
            .env("ZDX_ESCAPED_PIPE_READY", &ready);
        let mut child = command.spawn(Duration::from_millis(100)).await.unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut reader = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.unwrap();
        });

        assert!(child.wait().await.unwrap().status.success());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut reader)
                .await
                .is_err()
        );
        reader.abort();
        tokio::time::timeout(Duration::from_secs(1), reader)
            .await
            .expect("aborted pipe reader stayed blocked")
            .expect_err("reader should be cancelled");

        let escaped_pid = wait_for_pid(&ready).await;
        unsafe { libc::kill(escaped_pid, libc::SIGKILL) };
        wait_until_gone(escaped_pid).await;
    }

    #[tokio::test]
    async fn owner_sigkill_still_cleans_target() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("target.pid");
        let mut owner = tokio::process::Command::new(std::env::current_exe().unwrap());
        owner
            .args([
                "process_supervisor::tests::owner_fixture",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("ZDX_SUPERVISOR_FIXTURE_PIDFILE", &pidfile)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let mut owner = owner.spawn().unwrap();
        let owner_pid = owner.id().unwrap() as i32;
        let target_pid = wait_for_pid(&pidfile).await;

        unsafe { libc::kill(owner_pid, libc::SIGKILL) };
        owner.wait().await.unwrap();
        wait_until_gone(target_pid).await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "process fixture invoked by owner_sigkill_still_cleans_target"]
    async fn owner_fixture() {
        let mut command = SupervisedCommand::new(std::env::current_exe().unwrap());
        command.args([
            "process_supervisor::tests::nested_target_fixture",
            "--exact",
            "--ignored",
            "--nocapture",
        ]);
        let _child = command.spawn(Duration::from_millis(150)).await.unwrap();
        std::future::pending::<()>().await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "nested process fixture"]
    async fn nested_target_fixture() {
        let Some(pidfile) = std::env::var_os("ZDX_SUPERVISOR_FIXTURE_PIDFILE") else {
            return;
        };
        let mut command = shell_command(
            "trap '' TERM; echo $$ > \"$1\"; while :; do sleep 1; done",
            Path::new(&pidfile),
        );
        command.stdin_null();
        let _child = command.spawn(Duration::from_millis(150)).await.unwrap();
        std::future::pending::<()>().await;
    }

    #[test]
    #[ignore = "escaped pipe-holder fixture"]
    fn escaped_pipe_holder_fixture() {
        let Some(ready) = std::env::var_os("ZDX_ESCAPED_PIPE_READY") else {
            return;
        };
        assert_ne!(unsafe { libc::setsid() }, -1);
        std::fs::write(ready, std::process::id().to_string()).unwrap();
        std::thread::sleep(Duration::from_secs(5));
    }
}
