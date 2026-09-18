//! Registry of foreground commands that were moved to the background.
//!
//! When a foreground Bash command outruns its foreground bound it is *adopted*
//! rather than killed: the still-live [`SupervisedChild`] moves out of the tool
//! call and into this process-global registry. Nothing about the running
//! command changes — same supervisor, same session, same process group, same
//! lease. Only the owner of the handle moves.
//!
//! Because this process keeps holding the lease, adopted jobs are
//! **session-scoped**. A process that outlives its runs (the TUI, the bot
//! daemon) simply keeps them; a one-shot process (`zdx exec`, and therefore
//! every subagent and orchestrator worker) must [`drain`] before it exits, or
//! dropping the lease would kill work that was relocated precisely because it
//! was slow. Draining never kills: it waits.
//!
//! Termination always runs through the lease, which is the authority on target
//! identity, so there is no PID-reuse hazard to guard against here.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::process_supervisor::{BoundedWait, SupervisedChild};

/// Called exactly once with the target's exit code when an adopted job ends.
/// `None` means the job was killed or its status could not be read.
pub type OnExit = Box<dyn FnOnce(Option<i32>) + Send + 'static>;

/// How long [`terminate`] waits for the supervisor's TERM→KILL teardown to be
/// confirmed before reporting the job as stopped anyway. The supervisor always
/// escalates to KILL, so this only bounds the confirmation, not the kill.
const TERMINATE_CONFIRM: Duration = Duration::from_secs(10);

/// How often [`drain`] repeats its "still waiting" notice.
const DRAIN_NOTICE_INTERVAL: Duration = Duration::from_secs(30);

/// How long an adopted job's reader tasks get to finish writing after the
/// target exits, before they are abandoned. Matches the foreground grace.
const READER_DRAIN_GRACE: Duration = Duration::from_millis(500);

/// One adopted job: the token that closes its lease, plus identity for logs.
struct Job {
    cancel: CancellationToken,
    pid: u32,
    command: String,
}

static JOBS: LazyLock<Mutex<HashMap<String, Job>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Signalled whenever a job deregisters, so [`drain`] never polls.
static JOBS_CHANGED: LazyLock<Notify> = LazyLock::new(Notify::new);

fn jobs() -> std::sync::MutexGuard<'static, HashMap<String, Job>> {
    JOBS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Takes ownership of a still-running supervised command under `bg_id`.
///
/// The handle is moved into a detached task that holds the lease for the rest
/// of the job's life and calls `on_exit` once it ends. The command keeps
/// running throughout; adoption never signals it.
///
/// `readers` are the stdout/stderr reader tasks, which keep appending to the
/// job's log files. They are awaited after the target exits and before the job
/// deregisters, so once [`drain`] returns the logs are complete — a process
/// that drains before exiting never truncates a job's output.
pub fn adopt(
    bg_id: String,
    mut child: SupervisedChild,
    command: String,
    readers: [tokio::task::JoinHandle<()>; 2],
    on_exit: OnExit,
) {
    let cancel = CancellationToken::new();
    jobs().insert(
        bg_id.clone(),
        Job {
            cancel: cancel.clone(),
            pid: child.identity.pid,
            command,
        },
    );

    tokio::spawn(async move {
        // `wait_bounded` with no bound waits indefinitely; cancelling the token
        // closes the lease, which is what asks the supervisor to terminate.
        let outcome = child.wait_bounded(Some(&cancel), None, None).await;

        // Let the readers finish writing what the command already produced.
        // Bounded, because a descendant that escaped the process group can hold
        // the pipe open forever and must not pin the process.
        for reader in readers {
            finish_reader(reader).await;
        }

        let code = match outcome {
            Ok(BoundedWait::Finished(outcome)) => outcome.status.code(),
            _ => None,
        };
        // Record the exit before deregistering, so a drained process never
        // exits with the job's bookkeeping half-written.
        on_exit(code);

        jobs().remove(&bg_id);
        JOBS_CHANGED.notify_waiters();
    });
}

/// Awaits a reader task, aborting it if it outlives the grace period.
///
/// Mirrors the foreground path: an orphan descendant holding the pipe open must
/// not keep this task, and therefore the process, alive indefinitely.
async fn finish_reader(mut reader: tokio::task::JoinHandle<()>) {
    tokio::select! {
        _ = &mut reader => {}
        () = tokio::time::sleep(READER_DRAIN_GRACE) => {
            reader.abort();
            let _ = reader.await;
        }
    }
}

/// Whether `bg_id` is an adopted job owned by this process.
#[must_use]
pub fn is_adopted(bg_id: &str) -> bool {
    jobs().contains_key(bg_id)
}

/// `(bg_id, pid, command)` for every job still running here, for logs and tests.
#[must_use]
pub fn live_jobs() -> Vec<(String, u32, String)> {
    let mut live: Vec<(String, u32, String)> = jobs()
        .iter()
        .map(|(bg_id, job)| (bg_id.clone(), job.pid, job.command.clone()))
        .collect();
    live.sort_by(|a, b| a.0.cmp(&b.0));
    live
}

/// Waits for every adopted job to finish on its own.
///
/// This is how a one-shot process keeps its promise not to kill relocated work:
/// the lease is held for the whole wait, the reader tasks keep appending to the
/// job's logs, and only then may the process exit. It never signals a job and
/// has no deadline, so a legitimately long build is waited out in full.
///
/// Returns immediately when nothing was adopted, which is the common case.
/// The caller is responsible for racing this against interrupt handling; see
/// [`terminate_all`].
pub async fn drain() {
    let started = Instant::now();
    let mut next_notice = Instant::now();

    loop {
        // Register for the wakeup *before* reading the map, or a job that
        // finishes in between would be a lost notification and hang the drain.
        let changed = JOBS_CHANGED.notified();

        let live = live_jobs();
        if live.is_empty() {
            if started.elapsed() > Duration::from_millis(1) {
                tracing::info!(
                    waited_secs = started.elapsed().as_secs(),
                    "All background jobs finished"
                );
            }
            return;
        }

        if Instant::now() >= next_notice {
            // warn! so this reaches stderr under the default filter: a human
            // watching a process that has not exited needs to see why.
            tracing::warn!(
                jobs = live.len(),
                pids = %live.iter().map(|(_, pid, _)| pid.to_string()).collect::<Vec<_>>().join(", "),
                waited_secs = started.elapsed().as_secs(),
                "Waiting for background commands to finish before exiting; \
                 they are still running and will not be killed. Interrupt to stop them."
            );
            for (bg_id, pid, command) in &live {
                tracing::info!(bg_id, pid, command, "Still running");
            }
            next_notice = Instant::now() + DRAIN_NOTICE_INTERVAL;
        }

        // Wake on the next deregistration, or to re-log the pending notice.
        let _ = tokio::time::timeout(DRAIN_NOTICE_INTERVAL, changed).await;
    }
}

/// Terminates every adopted job by closing its lease, then waits for the
/// supervisor teardown to be confirmed.
///
/// Used when the process is interrupted while draining: the operator asked to
/// stop, so the jobs stop, and no orphan is left behind.
pub async fn terminate_all() {
    let live = live_jobs();
    if live.is_empty() {
        return;
    }
    tracing::warn!(
        jobs = live.len(),
        pids = %live.iter().map(|(_, pid, _)| pid.to_string()).collect::<Vec<_>>().join(", "),
        "Interrupted; stopping background commands"
    );
    for (bg_id, _, _) in live {
        terminate(&bg_id).await;
    }
}

/// Terminates an adopted job by closing its lease.
///
/// Returns `false` when `bg_id` is not adopted here (it may be a detached
/// `background: true` process, or owned by another zdx process), leaving the
/// caller to fall back to the signalling path. Returns `true` once the job has
/// been asked to stop, waiting for confirmation that it is gone.
pub async fn terminate(bg_id: &str) -> bool {
    let Some(cancel) = jobs().get(bg_id).map(|job| job.cancel.clone()) else {
        return false;
    };
    cancel.cancel();

    let deadline = Instant::now() + TERMINATE_CONFIRM;
    while is_adopted(bg_id) {
        if Instant::now() >= deadline {
            tracing::warn!(bg_id, "Adopted job teardown was not confirmed in time");
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    true
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use tempfile::TempDir;

    use super::*;
    use crate::process_supervisor::SupervisedCommand;

    /// Reader tasks for an adopted fixture: drain the child's pipes the way the
    /// bash tool's readers do, so `adopt` has something real to await.
    fn readers_for(child: &mut SupervisedChild) -> [tokio::task::JoinHandle<()>; 2] {
        use tokio::io::AsyncReadExt as _;

        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        [
            tokio::spawn(async move {
                if let Some(handle) = stdout.as_mut() {
                    let mut sink = Vec::new();
                    let _ = handle.read_to_end(&mut sink).await;
                }
            }),
            tokio::spawn(async move {
                if let Some(handle) = stderr.as_mut() {
                    let mut sink = Vec::new();
                    let _ = handle.read_to_end(&mut sink).await;
                }
            }),
        ]
    }

    fn process_exists(pid: i32) -> bool {
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    async fn wait_for_pid(path: &Path) -> i32 {
        tokio::time::timeout(Duration::from_secs(5), async {
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

    async fn spawn_shell(script: &str, pidfile: &Path) -> SupervisedChild {
        let mut command = SupervisedCommand::new("/bin/sh");
        command
            .args(["-c", script, "adopted-fixture", &pidfile.to_string_lossy()])
            .stdin_null();
        command.spawn(Duration::from_millis(150)).await.unwrap()
    }

    /// An adopted job that finishes on its own reports its real exit code, and
    /// deregisters itself. This is the "long build completes unharmed after
    /// handoff" shape, scaled down.
    #[tokio::test]
    async fn adopted_job_runs_to_completion_and_reports_exit_code() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("target.pid");
        let mut child = spawn_shell("echo $$ > \"$1\"; sleep 1; exit 17", &pidfile).await;
        let pid = wait_for_pid(&pidfile).await;

        let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
        let code = Arc::new(AtomicI64::new(i64::MIN));
        let recorded = Arc::clone(&code);
        let readers = readers_for(&mut child);
        adopt(
            bg_id.clone(),
            child,
            "sleep-fixture".to_string(),
            readers,
            Box::new(move |code: Option<i32>| {
                recorded.store(code.map_or(-1, i64::from), Ordering::SeqCst);
            }),
        );

        assert!(is_adopted(&bg_id));
        assert!(process_exists(pid), "adoption must not disturb the target");

        tokio::time::timeout(Duration::from_secs(10), async {
            while code.load(Ordering::SeqCst) == i64::MIN {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("adopted job never reported completion");

        assert_eq!(code.load(Ordering::SeqCst), 17);
        assert!(!is_adopted(&bg_id), "finished job should deregister");
    }

    /// Killing an adopted job closes the lease, which runs the supervisor's
    /// TERM→KILL sweep — even for a target that ignores TERM.
    #[tokio::test]
    async fn terminate_closes_the_lease_and_reaps_the_group() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("target.pid");
        let mut child = spawn_shell(
            "trap '' TERM; echo $$ > \"$1\"; while :; do sleep 1; done",
            &pidfile,
        )
        .await;
        let pid = wait_for_pid(&pidfile).await;

        let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
        let readers = readers_for(&mut child);
        adopt(
            bg_id.clone(),
            child,
            "sleep-fixture".to_string(),
            readers,
            Box::new(|_| {}),
        );

        assert!(terminate(&bg_id).await);
        assert!(!is_adopted(&bg_id));

        tokio::time::timeout(Duration::from_secs(5), async {
            while process_exists(pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("adopted target survived termination");
    }

    #[tokio::test]
    async fn terminate_reports_unknown_ids_as_not_adopted() {
        assert!(!terminate("bg-not-adopted-here").await);
    }

    /// The ordering guarantee `drain` relies on: a job deregisters only after
    /// its readers have finished writing and its exit has been recorded. That
    /// is what lets a one-shot process exit without truncating a relocated
    /// job's log.
    ///
    /// Scoped to this job's own id: the registry is process-global and other
    /// tests adopt jobs concurrently, so global counts are not assertable here.
    /// End-to-end draining is covered by the `zdx-cli` integration tests, which
    /// get a real process of their own.
    #[tokio::test]
    async fn job_deregisters_only_after_its_log_is_complete() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("target.pid");
        let log = temp.path().join("job.out");

        // Writes across time, with the last line arriving well after adoption.
        let mut child = spawn_shell(
            "echo $$ > \"$1\"; for i in 1 2 3 4 5; do echo line-$i; sleep 0.1; done; echo final",
            &pidfile,
        )
        .await;
        let pid = wait_for_pid(&pidfile).await;

        // Reader that appends to a log file, like the bash tool's post-handoff
        // sink. It writes only at EOF, so a deregistration that did not await
        // the reader would leave the log missing.
        let mut stdout = child.stdout.take();
        let sink_path = log.clone();
        let stdout_reader = tokio::spawn(async move {
            use tokio::io::AsyncReadExt as _;
            let Some(handle) = stdout.as_mut() else {
                return;
            };
            let mut bytes = Vec::new();
            let _ = handle.read_to_end(&mut bytes).await;
            std::fs::write(&sink_path, bytes).unwrap();
        });
        let stderr_reader = tokio::spawn(async {});

        let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
        let exited = Arc::new(AtomicI64::new(i64::MIN));
        let recorded = Arc::clone(&exited);
        adopt(
            bg_id.clone(),
            child,
            "streaming-fixture".to_string(),
            [stdout_reader, stderr_reader],
            Box::new(move |code: Option<i32>| {
                recorded.store(code.map_or(-1, i64::from), Ordering::SeqCst);
            }),
        );

        assert!(live_jobs().iter().any(|(id, _, _)| id == &bg_id));

        tokio::time::timeout(Duration::from_secs(20), async {
            while is_adopted(&bg_id) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("job never deregistered");

        // Everything below must already be true at deregistration, which is the
        // instant `drain` is allowed to return.
        assert!(
            !process_exists(pid),
            "deregistered before the target exited"
        );
        assert_ne!(
            exited.load(Ordering::SeqCst),
            i64::MIN,
            "exit must be recorded before deregistering"
        );

        let logged = std::fs::read_to_string(&log).expect("log should exist");
        for i in 1..=5 {
            assert!(logged.contains(&format!("line-{i}")), "log was: {logged}");
        }
        assert!(
            logged.contains("final"),
            "readers must be awaited before deregistering: {logged}"
        );
    }
}
