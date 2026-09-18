//! Registry of foreground commands that were moved to the background.
//!
//! When a foreground Bash command outruns its foreground bound it is *adopted*
//! rather than killed: the still-live [`SupervisedChild`] moves out of the tool
//! call and into this process-global registry. Nothing about the running
//! command changes — same supervisor, same session, same process group, same
//! lease. Only the owner of the handle moves.
//!
//! Because this process keeps holding the lease, adopted jobs are
//! **session-scoped**: they are terminated by the supervisor when zdx exits, in
//! contrast to `background: true`, which detaches at spawn and outlives zdx.
//!
//! Termination always runs through the lease, which is the authority on target
//! identity, so there is no PID-reuse hazard to guard against here.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::process_supervisor::{BoundedWait, SupervisedChild};

/// Called exactly once with the target's exit code when an adopted job ends.
/// `None` means the job was killed or its status could not be read.
pub type OnExit = Box<dyn FnOnce(Option<i32>) + Send + 'static>;

/// How long [`terminate`] waits for the supervisor's TERM→KILL teardown to be
/// confirmed before reporting the job as stopped anyway. The supervisor always
/// escalates to KILL, so this only bounds the confirmation, not the kill.
const TERMINATE_CONFIRM: Duration = Duration::from_secs(10);

static JOBS: LazyLock<Mutex<HashMap<String, CancellationToken>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn jobs() -> std::sync::MutexGuard<'static, HashMap<String, CancellationToken>> {
    JOBS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Takes ownership of a still-running supervised command under `bg_id`.
///
/// The handle is moved into a detached task that holds the lease for the rest
/// of the job's life and calls `on_exit` once it ends. The command keeps
/// running throughout; adoption never signals it.
pub fn adopt(bg_id: String, mut child: SupervisedChild, on_exit: OnExit) {
    let cancel = CancellationToken::new();
    jobs().insert(bg_id.clone(), cancel.clone());

    tokio::spawn(async move {
        // `wait_bounded` with no bound waits indefinitely; cancelling the token
        // closes the lease, which is what asks the supervisor to terminate.
        let outcome = child.wait_bounded(Some(&cancel), None, None).await;
        jobs().remove(&bg_id);

        let code = match outcome {
            Ok(BoundedWait::Finished(outcome)) => outcome.status.code(),
            _ => None,
        };
        on_exit(code);
    });
}

/// Whether `bg_id` is an adopted job owned by this process.
#[must_use]
pub fn is_adopted(bg_id: &str) -> bool {
    jobs().contains_key(bg_id)
}

/// Terminates an adopted job by closing its lease.
///
/// Returns `false` when `bg_id` is not adopted here (it may be a detached
/// `background: true` process, or owned by another zdx process), leaving the
/// caller to fall back to the signalling path. Returns `true` once the job has
/// been asked to stop, waiting for confirmation that it is gone.
pub async fn terminate(bg_id: &str) -> bool {
    let Some(cancel) = jobs().get(bg_id).cloned() else {
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
        let child = spawn_shell("echo $$ > \"$1\"; sleep 1; exit 17", &pidfile).await;
        let pid = wait_for_pid(&pidfile).await;

        let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
        let code = Arc::new(AtomicI64::new(i64::MIN));
        let recorded = Arc::clone(&code);
        adopt(
            bg_id.clone(),
            child,
            Box::new(move |code| {
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
        let child = spawn_shell(
            "trap '' TERM; echo $$ > \"$1\"; while :; do sleep 1; done",
            &pidfile,
        )
        .await;
        let pid = wait_for_pid(&pidfile).await;

        let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
        adopt(bg_id.clone(), child, Box::new(|_| {}));

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
}
