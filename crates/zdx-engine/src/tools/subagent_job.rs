//! Managed subagents: `invoke_subagent` calls that can run in the background.
//!
//! Two kinds of call use this module. `background: true` starts the child and
//! returns a handle at once. A normal call in a run with a handoff bound waits
//! like a blocking call, and hands the same child off to the background if it
//! is still running at the bound, the way `Bash` relocates a slow command. In
//! both, the child is an ordinary `zdx exec` run, started through the same
//! spawn path as a blocking one and held from the start by the shared
//! adopted-jobs registry ([`zdx_tools::adopted`]). Only when it is handed off
//! is it registered in the background-process registry
//! ([`crate::background_activity`]) under its `bg_id`, which makes it a job
//! like any other: `background_output` reads it and `background_kill` stops
//! it, through the same lease that stops a command. A child that finishes
//! before the handoff returns its answer and leaves no job behind.
//!
//! "Background" means the call does not block the turn, not that the child
//! outlives it. Every managed child is registered with the agent turn that
//! started it ([`TurnTasks`]) from the moment it starts, and that turn stops
//! whatever is still running when it ends, however it ends. The caller does
//! not have to collect or kill anything for the child to be cleaned up.
//!
//! The child's stdout carries its JSONL agent events. A reader task condenses
//! them into the job's stdout log as one compact record per line, from the
//! start, so progress made before a handoff is kept; until the handoff it also
//! relays them live to the caller. `background_output` derives progress and
//! the result from that log.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::ToolContext;
use crate::background_activity::{self, BackgroundProcess};
use crate::core::events::ToolOutput;
use crate::core::subagent::{ExecSubagentOptions, SubagentStreamSink};

/// Background work owned by one agent turn.
///
/// Background subagents register here when they start. When the turn ends,
/// [`TurnTasksGuard`] stops each one that is still running and closes the set,
/// so nothing registered later can escape the turn either. Bash jobs are not
/// registered here; they keep their own lifetime rules.
#[derive(Debug, Default)]
pub struct TurnTasks {
    state: Mutex<TurnTasksState>,
}

#[derive(Debug, Default)]
struct TurnTasksState {
    bg_ids: Vec<String>,
    closed: bool,
}

impl TurnTasks {
    /// Registers a started job. Returns `false` when the turn has already
    /// ended; the caller must then stop the job itself.
    pub fn register(&self, bg_id: &str) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return false;
        }
        state.bg_ids.push(bg_id.to_string());
        true
    }

    /// Stops every registered job that is still running, and refuses new ones.
    ///
    /// Closes each job's lease without waiting: the supervisor tears the child
    /// and its process group down (TERM, grace, KILL), and the job leaves the
    /// adopted registry once it is reaped, so a process that drains before
    /// exiting still waits for the teardown to finish.
    pub fn stop_all(&self) {
        let bg_ids = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            std::mem::take(&mut state.bg_ids)
        };
        for bg_id in bg_ids {
            #[cfg(unix)]
            if zdx_tools::adopted::request_terminate(&bg_id) {
                tracing::info!(bg_id, "Stopping background subagent: its turn ended");
            }
            #[cfg(not(unix))]
            let _ = bg_id;
        }
    }
}

/// Stops a turn's background work when dropped, on every way a turn can end:
/// completion, failure, cancellation, or the turn's future being dropped.
pub struct TurnTasksGuard(Arc<TurnTasks>);

impl TurnTasksGuard {
    /// Creates the guard and the shared set that tool calls register into.
    #[must_use]
    pub fn new() -> (Self, Arc<TurnTasks>) {
        let tasks = Arc::new(TurnTasks::default());
        (Self(Arc::clone(&tasks)), tasks)
    }
}

impl Drop for TurnTasksGuard {
    fn drop(&mut self) {
        self.0.stop_all();
    }
}

/// A background subagent ready to start.
pub struct SubagentJob<'a> {
    pub root: &'a Path,
    pub prompt: &'a str,
    /// Child options. `thread_id` must name the child's already-created
    /// thread, so the handle can report it before the child starts.
    pub options: ExecSubagentOptions,
    pub subagent: &'a str,
    pub child_thread_id: String,
    /// The file of a thread created for this call, removed again if the call
    /// ends without the child having written to it. `None` when the call
    /// continues an existing thread, which is never removed.
    pub child_thread_path: Option<std::path::PathBuf>,
    /// Held until the child exits (the exit callback keeps a reference), so
    /// no second run can use its thread meanwhile.
    pub thread_lock: Arc<ChildThreadLock>,
}

/// An exclusive advisory lock on a child thread's file, held for as long as
/// a child runs against that thread.
///
/// `invoke_subagent` takes it for every child, new or continued, so two calls
/// can never run the same thread at once, in this process or another one.
#[derive(Debug)]
pub struct ChildThreadLock {
    _file: std::fs::File,
}

/// Locks the thread `id` for one child run.
///
/// # Errors
/// Returns `thread_busy` when another run holds the thread, or `io_error`
/// when the thread file cannot be opened or locked.
pub(super) fn lock_child_thread(id: &str) -> Result<ChildThreadLock, ToolOutput> {
    let path = crate::config::paths::threads_dir().join(format!("{id}.jsonl"));
    let file = std::fs::File::open(&path).map_err(|err| {
        ToolOutput::failure(
            "io_error",
            format!("Failed to open subagent thread {id}"),
            Some(err.to_string()),
        )
    })?;
    match file.try_lock() {
        Ok(()) => Ok(ChildThreadLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(thread_busy(id)),
        Err(std::fs::TryLockError::Error(err)) => Err(ToolOutput::failure(
            "io_error",
            format!("Failed to lock subagent thread {id}"),
            Some(err.to_string()),
        )),
    }
}

/// The failure for a thread that another run is using right now.
pub(super) fn thread_busy(id: &str) -> ToolOutput {
    ToolOutput::failure(
        "thread_busy",
        format!(
            "Subagent thread {id} is still running in another call. Wait until that run ends \
             (follow it with background_output if it runs in the background), then continue it."
        ),
        None,
    )
}

/// Turns the result of a finished child run into the `invoke_subagent`
/// result.
///
/// An answer becomes `{ "result", "child_thread_id" }`, so the caller can
/// continue the same child later. On failure or cancellation, a thread created
/// for this call (`new_thread`) is removed when the child never wrote to it;
/// a failure names the thread only when the thread still exists.
pub(super) fn child_answer(
    output: ToolOutput,
    child_thread_id: &str,
    new_thread: Option<&Path>,
) -> ToolOutput {
    match output {
        ToolOutput::Success { data, image, .. } => ToolOutput::Success {
            ok: true,
            data: json!({ "result": data, "child_thread_id": child_thread_id }),
            image,
        },
        ToolOutput::Failure { ok, mut error } => {
            if let Some(path) = new_thread {
                remove_unused_child_thread(path);
            }
            if crate::config::paths::threads_dir()
                .join(format!("{child_thread_id}.jsonl"))
                .exists()
            {
                let note = format!(
                    "child_thread_id: {child_thread_id}. The thread keeps what the child did; \
                     pass it as thread_id to continue it."
                );
                error.details = Some(match error.details.take() {
                    Some(details) if !details.trim().is_empty() => format!("{details}\n{note}"),
                    _ => note,
                });
            }
            ToolOutput::Failure { ok, error }
        }
        canceled @ ToolOutput::Canceled { .. } => {
            if let Some(path) = new_thread {
                remove_unused_child_thread(path);
            }
            canceled
        }
    }
}

/// Why a background subagent cannot start in this context, checked before
/// anything is created for it: there is no owning turn, or the platform has
/// no lease supervisor.
pub fn cannot_start(ctx: &ToolContext) -> Option<ToolOutput> {
    if !cfg!(unix) {
        return Some(ToolOutput::failure(
            "unsupported",
            "Background subagents are only supported on Unix.",
            None,
        ));
    }
    ctx.turn_tasks.is_none().then(no_owner)
}

fn no_owner() -> ToolOutput {
    ToolOutput::failure(
        "no_owner",
        "A background subagent can only start inside an agent turn, which owns it and stops it \
         when the turn ends. Call invoke_subagent without background instead.",
        None,
    )
}

/// When a managed subagent call hands its child off to the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// `background: true`: hand off at once and return the handle.
    Immediate,
    /// A normal call: wait like a blocking call, and hand off only if the
    /// child is still running after this bound. The bound never stops the
    /// child.
    After(Duration),
}

/// Exit bookkeeping shared by the child's exit callback, the caller, and the
/// call's [`ForegroundGuard`].
///
/// One lock decides each race: a child that exits at the handoff is either
/// completed in the foreground or handed off, never neither; and a call
/// abandoned before the handoff has its unregistered leftovers removed
/// exactly once, after the child has exited.
#[cfg(unix)]
struct Lifecycle {
    state: Mutex<LifecycleState>,
    exited: tokio::sync::Notify,
    /// What an abandoned, never-registered job leaves behind.
    leftovers: Leftovers,
    /// Stops the deadline timer once the child has exited.
    deadline_stop: tokio_util::sync::CancellationToken,
}

/// A child that has exited: its exit code, `None` when it was killed or its
/// status could not be read.
#[derive(Debug, Clone, Copy)]
struct Exited {
    code: Option<i32>,
}

#[derive(Default)]
struct LifecycleState {
    /// Set once the child exited and its logs are complete.
    exit: Option<Exited>,
    /// The registry marker was written: the job is now a visible background
    /// job and its exit must be recorded there.
    handed_off: bool,
    /// The call was abandoned before it finished or handed off; whoever sees
    /// the child exited removes the leftovers.
    abandoned: bool,
}

/// Files a managed call creates before it knows whether it will hand off.
#[derive(Debug, Clone)]
struct Leftovers {
    bg_id: String,
    /// The thread the child runs against, new or continued.
    child_thread_id: String,
    /// The child thread created for the call; `None` for a continued thread,
    /// which is never removed.
    child_thread: Option<std::path::PathBuf>,
}

impl Leftovers {
    /// Removes the job's logs, and its child thread when the child never
    /// wrote to it. A thread with any history is kept.
    fn remove(&self) {
        remove_logs(&self.bg_id);
        if let Some(path) = &self.child_thread {
            remove_unused_child_thread(path);
        }
    }
}

#[cfg(unix)]
impl Lifecycle {
    fn new(leftovers: Leftovers) -> Self {
        Self {
            state: Mutex::default(),
            exited: tokio::sync::Notify::new(),
            leftovers,
            deadline_stop: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LifecycleState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn wait_exit(&self) -> Exited {
        loop {
            let notified = self.exited.notified();
            if let Some(exited) = self.lock().exit {
                return exited;
            }
            notified.await;
        }
    }

    /// Records the child's exit. Called once, after its logs are complete.
    fn record_exit(&self, code: Option<i32>) {
        let mut state = self.lock();
        state.exit = Some(Exited { code });
        if state.handed_off {
            background_activity::mark_exited(&self.leftovers.bg_id, code);
        } else if state.abandoned {
            self.leftovers.remove();
        }
        drop(state);
        self.deadline_stop.cancel();
        self.exited.notify_one();
    }

    /// The call was dropped before it finished or handed off: stop the child,
    /// and remove the leftovers once it has exited (now, if it already has).
    fn abandon(&self) {
        let mut state = self.lock();
        if state.handed_off || state.abandoned {
            return;
        }
        state.abandoned = true;
        if state.exit.is_some() {
            self.leftovers.remove();
            return;
        }
        drop(state);
        zdx_tools::adopted::request_terminate(&self.leftovers.bg_id);
    }
}

/// The exit callback of a managed child: keeps `keep` (the child's prompt
/// files) alive until the child exits, then records the exit under the
/// event-log lock, so a deadline record written while claiming the stop is
/// always in the log before the exit is.
#[cfg(unix)]
fn exit_callback(
    lifecycle: Arc<Lifecycle>,
    log: EventLog,
    keep: impl Send + 'static,
) -> zdx_tools::adopted::OnExit {
    Box::new(move |code| {
        drop(keep);
        let _log = log.lock();
        lifecycle.record_exit(code);
    })
}

/// Abandons a managed call whose future is dropped, or that fails, before it
/// either finished or was handed off: no child survives it, and nothing it
/// created stays behind except a child transcript with real history.
#[cfg(unix)]
struct ForegroundGuard {
    lifecycle: Option<Arc<Lifecycle>>,
}

#[cfg(unix)]
impl ForegroundGuard {
    fn disarm(&mut self) {
        self.lifecycle = None;
    }
}

#[cfg(unix)]
impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        if let Some(lifecycle) = self.lifecycle.take() {
            lifecycle.abandon();
        }
    }
}

/// Deletes a child thread created for a managed subagent, but only while it
/// still holds nothing but its meta line. A thread the child already wrote to
/// is kept: it is real history.
pub(super) fn remove_unused_child_thread(path: &Path) {
    // The check and the removal under one lock, so no append lands between.
    let Ok(_lock) = crate::core::thread_persistence::ThreadFileLock::acquire(path) else {
        return;
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let mut lines = content.lines().filter(|line| !line.trim().is_empty());
    let only_meta = lines
        .next()
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .is_some_and(|event| event["type"] == "meta")
        && lines.next().is_none();
    if only_meta {
        let _ = std::fs::remove_file(path);
    }
}

/// Runs `invoke_subagent` as a managed child: the same child process for its
/// whole life, owned by the current turn from the moment it starts.
///
/// With [`Launch::Immediate`] it returns the background handle at once. With
/// [`Launch::After`] it behaves like a blocking call: the child's tool
/// activity is relayed live to `live`, and if the child exits within the
/// bound its answer is returned exactly as a blocking call returns it, and
/// nothing stays registered. If the child is still running at the bound it
/// is handed off: it keeps running, the live relay stops, and the call
/// returns the background handle with the progress so far (and the answer, if
/// the child's turn already ended while its process kept running).
///
/// Either way the child is registered with the turn's [`TurnTasks`], so it
/// is stopped when the turn ends, and a run deadline (`options.timeout`)
/// stops it at that deadline. The handoff itself never stops the child.
#[cfg(unix)]
pub async fn run(
    job: SubagentJob<'_>,
    ctx: &ToolContext,
    launch: Launch,
    live: Option<SubagentStreamSink>,
) -> ToolOutput {
    let started = std::time::Instant::now();
    let Some(tasks) = ctx.turn_tasks.clone() else {
        if let Some(path) = &job.child_thread_path {
            remove_unused_child_thread(path);
        }
        return no_owner();
    };
    let mut managed = match spawn_managed(&job, live).await {
        Ok(managed) => managed,
        Err(output) => {
            // Nothing was started, so nothing can still write to the thread.
            if let Some(path) = &job.child_thread_path {
                remove_unused_child_thread(path);
            }
            return output;
        }
    };
    let bg_id = managed.bg_id.clone();

    // From here on, every early return drops `managed`, whose guard stops
    // the child and removes its leftovers once it has exited.
    if !tasks.register(&bg_id) {
        return ToolOutput::failure(
            "turn_ended",
            "The turn ended while the subagent was starting, so it was stopped.",
            None,
        );
    }
    let deadline = job.options.timeout;
    if let Some(after) = deadline {
        spawn_deadline(
            bg_id.clone(),
            after,
            managed.events.clone(),
            managed.lifecycle.deadline_stop.clone(),
        );
    }

    if let Launch::After(bound) = launch {
        let cancelled = async {
            match ctx.cancel_token.as_ref() {
                Some(token) => token.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            exited = managed.lifecycle.wait_exit() => {
                managed.guard.disarm();
                return finish_in_foreground(&managed.lifecycle.leftovers, exited.code);
            }
            () = cancelled => {
                // Returns at once: the guard stops the child, and its
                // leftovers are removed when it has exited, even if this
                // turn's teardown is cut short.
                return ToolOutput::canceled("Interrupted by user");
            }
            () = tokio::time::sleep(bound) => {}
        }
    }

    let rec = BackgroundProcess {
        bg_id: bg_id.clone(),
        pid: managed.identity.pid,
        pgid: managed.identity.pgid,
        birth_id: background_activity::capture_identity(managed.identity.pid).0,
        thread_id: ctx.current_thread_id.clone(),
        command: managed.label.clone(),
        cwd: job.root.to_string_lossy().into_owned(),
        started_at: (chrono::Utc::now()
            - chrono::Duration::from_std(started.elapsed()).unwrap_or_default())
        .to_rfc3339(),
        mode: background_activity::BackgroundMode::Adopted,
        kind: background_activity::BackgroundKind::Subagent,
        child_thread_id: Some(job.child_thread_id.clone()),
        exited_at: None,
        exit_code: None,
    };
    if let Err(output) = hand_off(&mut managed, &rec) {
        return output;
    }

    match launch {
        Launch::Immediate => handle_output(&bg_id, job.subagent, &job.child_thread_id, deadline),
        Launch::After(_) => handoff_output(&rec, job.subagent, started.elapsed(), deadline),
    }
}

/// A spawned managed child, held by the adopted registry.
#[cfg(unix)]
struct ManagedChild {
    bg_id: String,
    identity: zdx_tools::process_supervisor::TargetIdentity,
    label: String,
    events: EventLog,
    /// The live relay to the caller; emptied at the handoff.
    relay: Arc<Mutex<Option<SubagentStreamSink>>>,
    lifecycle: Arc<Lifecycle>,
    /// Abandons the call if it is dropped before it finishes or hands off.
    guard: ForegroundGuard,
}

/// Opens the job's logs, spawns the child, starts its log readers, and hands
/// the child to the adopted registry. Nothing is registered as a visible
/// background job yet.
#[cfg(unix)]
async fn spawn_managed(
    job: &SubagentJob<'_>,
    live: Option<SubagentStreamSink>,
) -> Result<ManagedChild, ToolOutput> {
    let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
    let JobLogs { events, stderr } = JobLogs::open(&bg_id).map_err(|err| {
        remove_logs(&bg_id);
        ToolOutput::failure(
            "io_error",
            format!("failed to open background subagent logs: {err}"),
            None,
        )
    })?;
    let spawned = crate::core::subagent::spawn_exec_subagent(job.root, job.prompt, &job.options)
        .await
        .map_err(|err| {
            remove_logs(&bg_id);
            ToolOutput::failure(
                "spawn_error",
                "Failed to start the subagent",
                Some(format!("{err:#}")),
            )
        })?;
    let (mut child, prompt_files) = spawned.into_parts();
    let identity = child.identity;

    let relay = Arc::new(Mutex::new(live));
    let readers = spawn_log_readers(
        child.stdout.take(),
        child.stderr.take(),
        events.clone(),
        stderr,
        Arc::clone(&relay),
    );

    let label = format!(
        "invoke_subagent {}: {}",
        job.subagent,
        crate::core::subagent::tool_input_preview(job.prompt)
    );
    let lifecycle = Arc::new(Lifecycle::new(Leftovers {
        bg_id: bg_id.clone(),
        child_thread_id: job.child_thread_id.clone(),
        child_thread: job.child_thread_path.clone(),
    }));
    zdx_tools::adopted::adopt(
        bg_id.clone(),
        child,
        label.clone(),
        readers,
        exit_callback(
            Arc::clone(&lifecycle),
            events.clone(),
            (prompt_files, Arc::clone(&job.thread_lock)),
        ),
    );

    Ok(ManagedChild {
        guard: ForegroundGuard {
            lifecycle: Some(Arc::clone(&lifecycle)),
        },
        bg_id,
        identity,
        label,
        events,
        relay,
        lifecycle,
    })
}

/// Makes the child a visible background job and stops the live relay.
///
/// The child's exit callback takes the same lock, so a child exiting at this
/// very moment is either completed in the foreground (returned as `Err` with
/// its blocking-call result) or handed off, never neither.
#[cfg(unix)]
fn hand_off(managed: &mut ManagedChild, rec: &BackgroundProcess) -> Result<(), ToolOutput> {
    {
        let mut state = managed.lifecycle.lock();
        if let Some(exited) = state.exit {
            drop(state);
            managed.guard.disarm();
            return Err(finish_in_foreground(
                &managed.lifecycle.leftovers,
                exited.code,
            ));
        }
        if let Err(err) = background_activity::write_marker(rec) {
            // Dropping `managed` abandons the call: its guard stops the child
            // and removes the leftovers.
            return Err(ToolOutput::failure(
                "io_error",
                format!("failed to register background subagent: {err}"),
                None,
            ));
        }
        state.handed_off = true;
    }
    background_activity::log_spawned(rec);
    managed.guard.disarm();
    // No live event may follow the tool result.
    *managed
        .relay
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    Ok(())
}

/// The blocking-call result of a managed child that exited before handoff.
///
/// Same outcomes and texts as a blocking call: the answer with its
/// `child_thread_id`, or an `execution_failed` error. The job was never
/// registered, so its logs are removed, and so is a new child thread on
/// failure when the child never wrote to it.
#[cfg(unix)]
fn finish_in_foreground(leftovers: &Leftovers, code: Option<i32>) -> ToolOutput {
    let output = foreground_output(&leftovers.bg_id, code);
    remove_logs(&leftovers.bg_id);
    child_answer(
        output,
        &leftovers.child_thread_id,
        leftovers.child_thread.as_deref(),
    )
}

#[cfg(unix)]
fn foreground_output(bg_id: &str, code: Option<i32>) -> ToolOutput {
    let log =
        std::fs::read_to_string(background_activity::stdout_log_path(bg_id)).unwrap_or_default();
    let stderr =
        std::fs::read_to_string(background_activity::stderr_log_path(bg_id)).unwrap_or_default();
    let progress = Progress::parse(&log);
    let failed = |details: String| {
        ToolOutput::failure(
            "execution_failed",
            "Subagent execution failed",
            Some(details),
        )
    };

    if let Some(secs) = progress.deadline_secs {
        return failed(format!("Subagent timed out after {secs} seconds"));
    }
    if let Some(TurnResult::Failed(message)) = &progress.turn {
        return failed(format!("Subagent turn failed: {message}"));
    }
    if code != Some(0) {
        let stderr = stderr.trim();
        if stderr.is_empty() {
            return failed(format!(
                "Subagent failed with exit code {}",
                code.unwrap_or(-1)
            ));
        }
        return failed(format!("Subagent failed: {stderr}"));
    }
    match progress.turn {
        Some(TurnResult::Completed(text) | TurnResult::Interrupted(text))
            if !text.trim().is_empty() =>
        {
            ToolOutput::success(Value::String(text))
        }
        _ => failed("Subagent returned empty output".to_string()),
    }
}

/// Removes the log files of a job that was never registered.
fn remove_logs(bg_id: &str) {
    let _ = std::fs::remove_file(background_activity::stdout_log_path(bg_id));
    let _ = std::fs::remove_file(background_activity::stderr_log_path(bg_id));
}

/// The result of a normal call whose subagent was still running at the
/// handoff bound.
///
/// Its message follows the child's turn: a child still working is followed
/// with `background_output`; a child whose turn already ended (it answered,
/// failed, or was interrupted, but its process had not exited yet) has
/// nothing more to give, so the caller is told not to wait for it.
#[cfg(unix)]
fn handoff_output(
    rec: &BackgroundProcess,
    subagent: &str,
    elapsed: Duration,
    deadline: Option<Duration>,
) -> ToolOutput {
    let mut data = output_view(rec, 0);
    let secs = elapsed.as_secs();
    data["backgrounded"] = json!(true);
    data["subagent"] = json!(subagent);
    data["elapsed_secs"] = json!(secs);
    data["deadline_secs"] = json!(deadline.map(|deadline| deadline.as_secs()));
    if let Some(object) = data.as_object_mut() {
        object.remove("stderr");
    }
    let bg_id = &rec.bg_id;
    let message = if data["turn"] == "running" {
        let deadline_note = deadline.map_or_else(String::new, |deadline| {
            format!(
                " This run's deadline (a job's timeout_secs) still stops it {} seconds after it \
                 started.",
                deadline.as_secs()
            )
        });
        format!(
            "Subagent {subagent} was still working after {secs}s, so it was moved to the \
             background as {bg_id}. It is the same run and keeps working — do NOT invoke it \
             again. Follow it with background_output (wait_secs with wait_for: \"exit\" returns \
             as soon as its result is ready) and stop it with background_kill. It is stopped \
             automatically when this turn ends, so collect its result before you finish.\
             {deadline_note}"
        )
    } else {
        let field = match data["turn"].as_str() {
            Some("completed") => "result",
            Some("interrupted") => "partial_result",
            _ => "error",
        };
        format!(
            "Subagent {subagent} already ended its turn ({}; see {field}), but its process had \
             not exited after {secs}s (it may be waiting on its own background commands), so it \
             was moved to the background as {bg_id}. Nothing more will come from it: do NOT \
             invoke it again or wait for it. background_kill stops it now; otherwise it is \
             stopped when this turn ends.",
            data["turn"].as_str().unwrap_or("ended")
        )
    };
    data["message"] = json!(message);
    ToolOutput::success(data)
}

/// The handle `invoke_subagent` returns for a started background subagent.
fn handle_output(
    bg_id: &str,
    subagent: &str,
    child_thread_id: &str,
    deadline: Option<Duration>,
) -> ToolOutput {
    let deadline_note = deadline.map_or_else(String::new, |deadline| {
        format!(
            " This run's deadline (a job's timeout_secs) also stops it after {} seconds, as it \
             would a blocking call.",
            deadline.as_secs()
        )
    });
    ToolOutput::success(json!({
        "bg_id": bg_id,
        "status": "running",
        "subagent": subagent,
        "child_thread_id": child_thread_id,
        "deadline_secs": deadline.map(|deadline| deadline.as_secs()),
        "message": format!(
            "Started subagent {subagent} in the background as {bg_id}. Follow it with \
             background_output (wait_secs with wait_for: \"exit\" waits for its result) and stop \
             it with background_kill. It is stopped automatically when this turn ends, so collect \
             its result before you finish.{deadline_note}"
        ),
    }))
}

/// Spawns the job's log readers: child events condensed into the stdout log,
/// one compact record per line, and stderr copied.
///
/// The turn's end is written the moment the child reports it, not when its
/// stdout closes: a child can finish its answer and keep running (for
/// example while it waits for its own background commands), and stopping it
/// then must not lose an answer it already gave.
#[cfg(unix)]
fn spawn_log_readers(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    events_log: EventLog,
    stderr_file: std::fs::File,
    relay: Arc<Mutex<Option<SubagentStreamSink>>>,
) -> [tokio::task::JoinHandle<()>; 2] {
    use crate::core::subagent::ChildEvent;

    let stdout_reader = tokio::spawn(async move {
        let Some(stdout) = stdout else { return };
        let outcome = crate::core::subagent::read_stdout_events(stdout, |event| {
            let record = match event {
                ChildEvent::Tool(chunk) => {
                    // Relayed live to the caller until the handoff, which
                    // empties the relay under this same lock.
                    let relay = relay
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(sink) = relay.as_ref() {
                        sink.emit(chunk);
                    }
                    chunk.clone()
                }
                ChildEvent::TurnEnded(outcome) => turn_record(outcome),
            };
            events_log.append(&record);
        })
        .await;
        if let Err(err) = outcome {
            events_log.append(&json!({ "t": "read_error", "message": format!("{err:#}") }));
        }
    });
    let stderr_reader = tokio::spawn(async move {
        let Some(mut stderr) = stderr else { return };
        let mut file = tokio::fs::File::from_std(stderr_file);
        let _ = tokio::io::copy(&mut stderr, &mut file).await;
    });
    [stdout_reader, stderr_reader]
}

/// The log record for the end of the child's turn.
fn turn_record(outcome: &crate::core::subagent::TurnOutcome) -> Value {
    use crate::core::subagent::TurnOutcome;

    match outcome {
        TurnOutcome::Completed(text) => json!({ "t": "turn", "status": "completed", "text": text }),
        TurnOutcome::Interrupted(text) => {
            json!({ "t": "turn", "status": "interrupted", "text": text })
        }
        TurnOutcome::Failed(message) => {
            json!({ "t": "turn", "status": "failed", "message": message })
        }
    }
}

/// Stops the job at the run's tool deadline, as a blocking call would be.
///
/// `stop` is cancelled when the child exits, which ends the timer at once and
/// releases its handle on the event log; a deadline that passes after the
/// exit would claim nothing anyway.
#[cfg(unix)]
fn spawn_deadline(
    bg_id: String,
    deadline: Duration,
    log: EventLog,
    stop: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::select! {
            () = stop.cancelled() => {}
            () = tokio::time::sleep(deadline) => {
                if deadline_fired(&bg_id, deadline, &log) {
                    tracing::info!(
                        bg_id,
                        "Stopping background subagent: the run's deadline passed"
                    );
                }
            }
        }
    })
}

/// Stops the job for its deadline, if nothing else stopped it first.
///
/// The claim and the `deadline` record happen under the event-log lock, and
/// the job's exit is recorded under the same lock, so the record is always in
/// the log before the exit. When an explicit kill or the owner's cleanup has
/// already started the stop, or the job already ended, nothing is written:
/// the view must not blame the deadline for a stop it did not cause.
#[cfg(unix)]
fn deadline_fired(bg_id: &str, deadline: Duration, log: &EventLog) -> bool {
    let mut file = log.lock();
    if !zdx_tools::adopted::claim_terminate(bg_id) {
        return false;
    }
    EventLog::write_record(
        &mut file,
        &json!({ "t": "deadline", "secs": deadline.as_secs() }),
    );
    true
}

/// Managed subagents need the Unix lease supervisor.
#[cfg(not(unix))]
pub async fn run(
    _job: SubagentJob<'_>,
    ctx: &ToolContext,
    _launch: Launch,
    _live: Option<SubagentStreamSink>,
) -> ToolOutput {
    cannot_start(ctx).expect("non-Unix builds never start managed subagents")
}

/// The log files of one background subagent job.
#[cfg(unix)]
struct JobLogs {
    /// The condensed event log (the job's stdout log).
    events: EventLog,
    /// The child's stderr.
    stderr: std::fs::File,
}

#[cfg(unix)]
impl JobLogs {
    fn open(bg_id: &str) -> std::io::Result<Self> {
        background_activity::ensure_dirs()?;
        let events = open_log(&background_activity::stdout_log_path(bg_id))?;
        let stderr = open_log(&background_activity::stderr_log_path(bg_id))?;
        Ok(Self {
            events: EventLog::new(events),
            stderr,
        })
    }
}

/// A job's event log, shared by every task that writes to it.
///
/// Each record is serialized once and written whole while holding the lock,
/// so records from the event reader and the deadline task never interleave,
/// and each line stays one complete JSON record.
#[cfg(unix)]
#[derive(Clone)]
struct EventLog(Arc<Mutex<std::fs::File>>);

#[cfg(unix)]
impl EventLog {
    fn new(file: std::fs::File) -> Self {
        Self(Arc::new(Mutex::new(file)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, std::fs::File> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Appends one record as one line.
    fn append(&self, record: &Value) {
        Self::write_record(&mut self.lock(), record);
    }

    fn write_record(file: &mut std::fs::File, record: &Value) {
        use std::io::Write as _;

        let mut line = record.to_string();
        line.push('\n');
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(unix)]
fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// What the condensed event log says about a background subagent.
#[derive(Debug, Default, PartialEq, Eq)]
struct Progress {
    /// Tool calls started, in order: `(id, name, arg)`.
    tools: Vec<(String, String, Option<String>)>,
    /// Ids of tool calls that finished.
    finished: Vec<String>,
    /// How the child's turn ended, once it reported that.
    turn: Option<TurnResult>,
    /// The run's deadline stopped the child, after this many seconds.
    deadline_secs: Option<u64>,
    /// Reading the child's output failed.
    read_error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum TurnResult {
    Completed(String),
    Interrupted(String),
    Failed(String),
}

impl Progress {
    fn parse(log: &str) -> Self {
        let mut progress = Self::default();
        for line in log.lines() {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let text = |key: &str| record.get(key).and_then(Value::as_str).map(str::to_string);
            match record.get("t").and_then(Value::as_str) {
                Some("start") => {
                    if let (Some(id), Some(name)) = (text("id"), text("name")) {
                        progress.tools.push((id, name, None));
                    }
                }
                Some("input") => {
                    if let (Some(id), Some(arg)) = (text("id"), text("arg"))
                        && let Some(tool) = progress.tools.iter_mut().find(|tool| tool.0 == id)
                    {
                        tool.2 = Some(arg);
                    }
                }
                Some("done" | "error") => {
                    if let Some(id) = text("id") {
                        progress.finished.push(id);
                    }
                }
                Some("turn") => {
                    progress.turn = match text("status").as_deref() {
                        Some("completed") => {
                            Some(TurnResult::Completed(text("text").unwrap_or_default()))
                        }
                        Some("interrupted") => {
                            Some(TurnResult::Interrupted(text("text").unwrap_or_default()))
                        }
                        Some("failed") => {
                            Some(TurnResult::Failed(text("message").unwrap_or_default()))
                        }
                        _ => progress.turn,
                    };
                }
                Some("deadline") => {
                    progress.deadline_secs = record.get("secs").and_then(Value::as_u64);
                }
                Some("read_error") => progress.read_error = text("message"),
                _ => {}
            }
        }
        progress
    }

    /// The most recently started tool call that has not finished.
    fn current_tool(&self) -> Option<&(String, String, Option<String>)> {
        self.tools
            .iter()
            .rev()
            .find(|tool| !self.finished.contains(&tool.0))
    }
}

/// Whether the subagent's log already records the end of its turn, so a
/// wait for its result can return while its process is still running.
pub(super) fn turn_ended(bg_id: &str) -> bool {
    std::fs::read_to_string(background_activity::stdout_log_path(bg_id))
        .is_ok_and(|log| Progress::parse(&log).turn.is_some())
}

/// Seconds since the job's event log last changed, as a progress signal.
fn seconds_since_activity(rec: &BackgroundProcess) -> Option<u64> {
    let modified = std::fs::metadata(background_activity::stdout_log_path(&rec.bg_id))
        .and_then(|meta| meta.modified())
        .ok()?;
    modified.elapsed().ok().map(|age| age.as_secs())
}

/// The `background_output` view of a background subagent.
///
/// `status` follows the child process, exactly as for a command: `"running"`
/// until it exits. The turn's result is reported separately, because a child
/// can finish its turn and still be running (for example while it waits for
/// its own background commands); a result alone does not mean it has exited.
pub(super) fn output_view(rec: &BackgroundProcess, stderr_tail_bytes: usize) -> Value {
    let log = std::fs::read_to_string(background_activity::stdout_log_path(&rec.bg_id))
        .unwrap_or_default();
    let progress = Progress::parse(&log);
    let running = rec.is_running();
    let child_thread = rec.child_thread_id.as_deref().unwrap_or("unknown");

    let mut data = json!({
        "bg_id": rec.bg_id,
        "kind": "subagent",
        "child_thread_id": rec.child_thread_id,
        "status": if running { "running" } else { "exited" },
        "exit_code": rec.exit_code,
        "uptime": rec.uptime(),
        "tool_calls": progress.tools.len(),
        "seconds_since_activity": seconds_since_activity(rec),
    });

    let still_running = if running {
        " Its process has not exited yet (it may be waiting on its own background commands); \
         background_kill stops it."
    } else {
        ""
    };
    let (turn, message) = match &progress.turn {
        Some(TurnResult::Completed(text)) => {
            data["result"] = json!(text);
            (
                "completed",
                format!("The subagent finished its turn; its answer is in result.{still_running}"),
            )
        }
        Some(TurnResult::Interrupted(text)) => {
            data["partial_result"] = json!(text);
            (
                "interrupted",
                format!(
                    "The subagent's turn was interrupted before it finished, so there is no final \
                     answer; partial_result holds what it had written.{still_running}"
                ),
            )
        }
        Some(TurnResult::Failed(error)) => {
            data["error"] = json!(error);
            (
                "failed",
                format!("The subagent's turn failed; see error.{still_running}"),
            )
        }
        None if running => {
            if let Some((_, name, arg)) = progress.current_tool() {
                data["current_tool"] = json!(name);
                if let Some(arg) = arg {
                    data["current_tool_input"] = json!(arg);
                }
            }
            (
                "running",
                "The subagent is still working. Wait with wait_secs (wait_for: \"exit\" waits for \
                 its result), or stop it with background_kill."
                    .to_string(),
            )
        }
        None => {
            data["stderr"] = json!(background_activity::read_log_tail(
                &background_activity::stderr_log_path(&rec.bg_id),
                stderr_tail_bytes,
            ));
            if let Some(error) = &progress.read_error {
                data["error"] = json!(error);
            }
            match progress.deadline_secs {
                Some(secs) => (
                    "timed_out",
                    format!(
                        "This run's deadline (a job's timeout_secs, {secs} seconds) stopped the \
                         subagent before it finished its turn, so there is no result. Its partial \
                         transcript is in thread {child_thread}."
                    ),
                ),
                None => (
                    "stopped",
                    format!(
                        "The subagent stopped before it finished its turn (it was stopped, or it \
                         crashed), so there is no result. Its partial transcript is in thread \
                         {child_thread}."
                    ),
                ),
            }
        }
    };
    data["turn"] = json!(turn);
    data["message"] = json!(message);
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_tracks_tools_and_the_turn() {
        let log = [
            r#"{"t":"start","id":"a","name":"read"}"#,
            r#"{"t":"input","id":"a","arg":"Cargo.toml"}"#,
            r#"{"t":"done","id":"a"}"#,
            r#"{"t":"start","id":"b","name":"bash"}"#,
            r#"{"t":"input","id":"b","arg":"sleep 5"}"#,
            "not json",
        ]
        .join("\n");
        let progress = Progress::parse(&log);
        assert_eq!(progress.tools.len(), 2);
        assert_eq!(
            progress.current_tool(),
            Some(&(
                "b".to_string(),
                "bash".to_string(),
                Some("sleep 5".to_string())
            ))
        );
        assert!(progress.turn.is_none());

        let completed = format!(
            "{log}\n{}\n{}",
            r#"{"t":"error","id":"b"}"#, r#"{"t":"turn","status":"completed","text":"report"}"#
        );
        let progress = Progress::parse(&completed);
        assert!(progress.current_tool().is_none());
        assert_eq!(
            progress.turn,
            Some(TurnResult::Completed("report".to_string()))
        );

        let interrupted = Progress::parse(r#"{"t":"turn","status":"interrupted","text":"half"}"#);
        assert_eq!(
            interrupted.turn,
            Some(TurnResult::Interrupted("half".to_string()))
        );
        let failed = Progress::parse(r#"{"t":"turn","status":"failed","message":"boom"}"#);
        assert_eq!(failed.turn, Some(TurnResult::Failed("boom".to_string())));

        let timed_out = Progress::parse(r#"{"t":"deadline","secs":30}"#);
        assert_eq!(timed_out.deadline_secs, Some(30));
        assert!(timed_out.turn.is_none());
    }

    fn record(bg_id: &str, running: bool) -> BackgroundProcess {
        BackgroundProcess {
            bg_id: bg_id.to_string(),
            pid: 1,
            pgid: 1,
            birth_id: None,
            thread_id: None,
            command: "invoke_subagent helper: task".to_string(),
            cwd: "/tmp".to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            mode: background_activity::BackgroundMode::Adopted,
            kind: background_activity::BackgroundKind::Subagent,
            child_thread_id: Some("child-thread".to_string()),
            exited_at: (!running).then(|| chrono::Utc::now().to_rfc3339()),
            exit_code: None,
        }
    }

    fn view_of(log: &str, running: bool) -> Value {
        let _home = crate::test_support::temp_zdx_home();
        background_activity::ensure_dirs().unwrap();
        let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
        std::fs::write(background_activity::stdout_log_path(&bg_id), log).unwrap();
        output_view(&record(&bg_id, running), 1024)
    }

    /// A finished answer is reported while the process still runs, and is
    /// still reported after the process was stopped. An interrupted turn is
    /// never reported as a completed one.
    #[test]
    fn view_separates_the_turn_from_the_process() {
        let answered = r#"{"t":"turn","status":"completed","text":"ANSWER"}"#;

        let running = view_of(answered, true);
        assert_eq!(running["status"], "running");
        assert_eq!(running["turn"], "completed");
        assert_eq!(running["result"], "ANSWER");
        assert!(
            running["message"]
                .as_str()
                .unwrap()
                .contains("not exited yet")
        );

        let stopped = view_of(&format!("{answered}\n"), false);
        assert_eq!(stopped["status"], "exited");
        assert_eq!(stopped["turn"], "completed");
        assert_eq!(stopped["result"], "ANSWER");

        let interrupted = view_of(
            r#"{"t":"turn","status":"interrupted","text":"HALF"}"#,
            false,
        );
        assert_eq!(interrupted["turn"], "interrupted");
        assert_eq!(interrupted["partial_result"], "HALF");
        assert!(interrupted.get("result").is_none());

        // A turn that ended before the deadline fired keeps its result.
        let answered_then_deadline = view_of(
            &format!("{answered}\n{}", r#"{"t":"deadline","secs":7}"#),
            false,
        );
        assert_eq!(answered_then_deadline["turn"], "completed");
        assert_eq!(answered_then_deadline["result"], "ANSWER");

        let timed_out = view_of(r#"{"t":"deadline","secs":7}"#, false);
        assert_eq!(timed_out["turn"], "timed_out");
        assert!(timed_out["message"].as_str().unwrap().contains("7 seconds"));

        let stopped_early = view_of("", false);
        assert_eq!(stopped_early["turn"], "stopped");
        assert!(stopped_early.get("result").is_none());
    }

    fn event_log_in(dir: &tempfile::TempDir) -> (EventLog, std::path::PathBuf) {
        let path = dir.path().join("events.log");
        (EventLog::new(open_log(&path).unwrap()), path)
    }

    /// Records written concurrently from several writers stay whole: every
    /// line of the log parses as one record, and none is lost.
    #[cfg(unix)]
    #[test]
    fn concurrent_records_never_interleave() {
        const WRITERS: usize = 8;
        const RECORDS: usize = 40;
        let dir = tempfile::TempDir::new().unwrap();
        let (log, path) = event_log_in(&dir);
        // Large records, so a single record needs more than one write call.
        let filler = "x".repeat(64 * 1024);

        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let log = log.clone();
                let filler = &filler;
                scope.spawn(move || {
                    for n in 0..RECORDS {
                        log.append(
                            &json!({ "t": "input", "id": format!("{writer}-{n}"), "arg": filler }),
                        );
                    }
                });
            }
        });

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), WRITERS * RECORDS);
        for line in lines {
            let record: Value = serde_json::from_str(line).expect("a whole record per line");
            assert_eq!(record["arg"].as_str().map(str::len), Some(filler.len()));
        }
    }

    /// A deadline that fires after an explicit or owner stop already started
    /// claims nothing and writes nothing, so the view keeps the real cause.
    /// A deadline that fires first is recorded. No timing is involved: the
    /// order of the calls is the race.
    #[cfg(unix)]
    #[tokio::test]
    async fn deadline_is_recorded_only_when_it_stops_the_job() {
        use zdx_tools::process_supervisor::SupervisedCommand;

        async fn adopt_sleeper(bg_id: &str) {
            let mut command = SupervisedCommand::new("/bin/sh");
            command.args(["-c", "sleep 60"]).stdin_null();
            let child = command
                .spawn(std::time::Duration::from_millis(200))
                .await
                .expect("spawn sleeper");
            zdx_tools::adopted::adopt(
                bg_id.to_string(),
                child,
                "sleep 60".to_string(),
                [tokio::spawn(async {}), tokio::spawn(async {})],
                Box::new(|_| {}),
            );
        }
        let dir = tempfile::TempDir::new().unwrap();
        let deadline = Duration::from_secs(5);

        let killed_first = format!("bg-test-{}", uuid::Uuid::new_v4());
        adopt_sleeper(&killed_first).await;
        let (log, path) = event_log_in(&dir);
        assert!(zdx_tools::adopted::request_terminate(&killed_first));
        assert!(!deadline_fired(&killed_first, deadline, &log));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("deadline"), "{content}");
        assert_eq!(Progress::parse(&content).deadline_secs, None);

        let dir2 = tempfile::TempDir::new().unwrap();
        let deadline_first = format!("bg-test-{}", uuid::Uuid::new_v4());
        adopt_sleeper(&deadline_first).await;
        let (log, path) = event_log_in(&dir2);
        assert!(deadline_fired(&deadline_first, deadline, &log));
        assert!(
            !deadline_fired(&deadline_first, deadline, &log),
            "claimed once"
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(Progress::parse(&content).deadline_secs, Some(5));

        assert!(!deadline_fired("bg-test-never-adopted", deadline, &log));

        zdx_tools::adopted::terminate(&killed_first).await;
        zdx_tools::adopted::terminate(&deadline_first).await;
    }

    /// A managed fixture: a real supervised `sh` adopted with the same exit
    /// callback a managed subagent uses, plus its logs and a child thread
    /// file. `history` adds a line after the meta line.
    #[cfg(unix)]
    struct Fixture {
        bg_id: String,
        pid: u32,
        lifecycle: Arc<Lifecycle>,
        events: EventLog,
        thread: std::path::PathBuf,
    }

    #[cfg(unix)]
    async fn managed_fixture(script: &str, history: bool) -> Fixture {
        use zdx_tools::process_supervisor::SupervisedCommand;

        background_activity::ensure_dirs().unwrap();
        let bg_id = format!("bg-test-{}", uuid::Uuid::new_v4());
        let JobLogs { events, stderr } = JobLogs::open(&bg_id).unwrap();
        drop(stderr);
        events.append(&json!({ "t": "start", "id": "a", "name": "read" }));

        let thread = crate::config::paths::threads_dir().join(format!("{bg_id}.jsonl"));
        std::fs::create_dir_all(thread.parent().unwrap()).unwrap();
        let mut content = String::from("{\"type\":\"meta\",\"schema_version\":1}\n");
        if history {
            content.push_str("{\"type\":\"message\",\"role\":\"user\",\"text\":\"task\"}\n");
        }
        std::fs::write(&thread, content).unwrap();

        let mut command = SupervisedCommand::new("/bin/sh");
        command.args(["-c", script]).stdin_null();
        let child = command
            .spawn(Duration::from_millis(200))
            .await
            .expect("spawn fixture");
        let pid = child.identity.pid;
        let lifecycle = Arc::new(Lifecycle::new(Leftovers {
            bg_id: bg_id.clone(),
            child_thread_id: bg_id.clone(),
            child_thread: Some(thread.clone()),
        }));
        zdx_tools::adopted::adopt(
            bg_id.clone(),
            child,
            script.to_string(),
            [tokio::spawn(async {}), tokio::spawn(async {})],
            exit_callback(Arc::clone(&lifecycle), events.clone(), ()),
        );
        Fixture {
            bg_id,
            pid,
            lifecycle,
            events,
            thread,
        }
    }

    #[cfg(unix)]
    async fn wait_deregistered(bg_id: &str) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while zdx_tools::adopted::is_adopted(bg_id) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the job never ended");
    }

    #[cfg(unix)]
    fn logs_exist(bg_id: &str) -> bool {
        background_activity::stdout_log_path(bg_id).exists()
            || background_activity::stderr_log_path(bg_id).exists()
    }

    /// A call dropped before it finished or handed off stops its child and,
    /// once the child has exited, removes its logs and its never-written
    /// child thread. Nothing was registered, so no marker exists.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_call_stops_the_child_and_removes_its_leftovers() {
        let _home = crate::test_support::temp_zdx_home();
        let fixture = managed_fixture("sleep 60", false).await;
        let guard = ForegroundGuard {
            lifecycle: Some(Arc::clone(&fixture.lifecycle)),
        };
        assert!(logs_exist(&fixture.bg_id));

        drop(guard);
        wait_deregistered(&fixture.bg_id).await;

        assert_eq!(
            unsafe { libc::kill(fixture.pid as i32, 0) },
            -1,
            "child survived"
        );
        assert!(!logs_exist(&fixture.bg_id), "logs left behind");
        assert!(!fixture.thread.exists(), "empty child thread left behind");
        assert!(background_activity::get(&fixture.bg_id).is_none());
    }

    /// A child transcript with real history survives an abandoned call.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_call_keeps_a_child_thread_with_history() {
        let _home = crate::test_support::temp_zdx_home();
        let fixture = managed_fixture("sleep 60", true).await;
        drop(ForegroundGuard {
            lifecycle: Some(Arc::clone(&fixture.lifecycle)),
        });
        wait_deregistered(&fixture.bg_id).await;

        assert!(!logs_exist(&fixture.bg_id));
        assert!(
            fixture.thread.exists(),
            "a thread with history must be kept"
        );
    }

    /// Abandoning a call whose child already exited removes the leftovers at
    /// once: there is no later exit to do it.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_call_after_the_child_exited_cleans_up_at_once() {
        let _home = crate::test_support::temp_zdx_home();
        let fixture = managed_fixture("exit 0", false).await;
        wait_deregistered(&fixture.bg_id).await;
        assert!(
            logs_exist(&fixture.bg_id),
            "nothing removed before abandoning"
        );

        drop(ForegroundGuard {
            lifecycle: Some(Arc::clone(&fixture.lifecycle)),
        });
        assert!(!logs_exist(&fixture.bg_id));
        assert!(!fixture.thread.exists());
    }

    /// A disarmed guard (the call finished or handed off) touches nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn disarmed_guard_leaves_the_child_alone() {
        let _home = crate::test_support::temp_zdx_home();
        let fixture = managed_fixture("sleep 60", false).await;
        let mut guard = ForegroundGuard {
            lifecycle: Some(Arc::clone(&fixture.lifecycle)),
        };
        guard.disarm();
        drop(guard);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(zdx_tools::adopted::is_adopted(&fixture.bg_id));
        assert!(logs_exist(&fixture.bg_id));

        zdx_tools::adopted::terminate(&fixture.bg_id).await;
    }

    /// The deadline timer ends as soon as the child exits, releasing its
    /// handle on the event log, and writes no deadline record.
    #[cfg(unix)]
    #[tokio::test]
    async fn deadline_timer_ends_when_the_child_exits() {
        let _home = crate::test_support::temp_zdx_home();
        let fixture = managed_fixture("sleep 60", false).await;
        let timer = spawn_deadline(
            fixture.bg_id.clone(),
            Duration::from_mins(10),
            fixture.events.clone(),
            fixture.lifecycle.deadline_stop.clone(),
        );

        zdx_tools::adopted::terminate(&fixture.bg_id).await;
        tokio::time::timeout(Duration::from_secs(5), timer)
            .await
            .expect("the deadline timer outlived the child")
            .unwrap();
        assert_eq!(
            Arc::strong_count(&fixture.events.0),
            1,
            "the timer still holds the event log"
        );
        let log =
            std::fs::read_to_string(background_activity::stdout_log_path(&fixture.bg_id)).unwrap();
        assert!(!log.contains("deadline"), "{log}");
    }

    /// A child that exits exactly at the bound is completed in the
    /// foreground, not handed off: the call returns its answer and nothing
    /// is registered.
    #[cfg(unix)]
    #[tokio::test]
    async fn hand_off_after_the_child_exited_returns_its_answer() {
        let _home = crate::test_support::temp_zdx_home();
        let fixture = managed_fixture("exit 0", true).await;
        fixture
            .events
            .append(&json!({ "t": "turn", "status": "completed", "text": "BOUNDARY" }));
        wait_deregistered(&fixture.bg_id).await;

        let mut managed = ManagedChild {
            bg_id: fixture.bg_id.clone(),
            identity: zdx_tools::process_supervisor::TargetIdentity {
                pid: fixture.pid,
                pgid: fixture.pid as i32,
            },
            label: "fixture".to_string(),
            events: fixture.events.clone(),
            relay: Arc::new(Mutex::new(None)),
            lifecycle: Arc::clone(&fixture.lifecycle),
            guard: ForegroundGuard {
                lifecycle: Some(Arc::clone(&fixture.lifecycle)),
            },
        };
        let rec = record(&fixture.bg_id, true);
        let output = hand_off(&mut managed, &rec).expect_err("completed, not handed off");
        assert_eq!(
            output.data(),
            Some(&json!({ "result": "BOUNDARY", "child_thread_id": fixture.bg_id }))
        );
        assert!(background_activity::get(&fixture.bg_id).is_none());
        assert!(!logs_exist(&fixture.bg_id));
        assert!(
            fixture.thread.exists(),
            "the answered child's thread is kept"
        );
    }

    /// The handoff message follows the child's turn, and `elapsed_secs` is
    /// the measured time, not the bound.
    #[cfg(unix)]
    #[test]
    fn handoff_message_follows_the_turn() {
        let _home = crate::test_support::temp_zdx_home();
        background_activity::ensure_dirs().unwrap();
        let handoff = |log: &str| {
            let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
            std::fs::write(background_activity::stdout_log_path(&bg_id), log).unwrap();
            handoff_output(
                &record(&bg_id, true),
                "helper",
                Duration::from_millis(3_400),
                None,
            )
            .data()
            .cloned()
            .unwrap()
        };

        let working = handoff(r#"{"t":"start","id":"a","name":"bash"}"#);
        assert_eq!(working["turn"], "running");
        assert_eq!(working["elapsed_secs"], 3);
        let message = working["message"].as_str().unwrap();
        assert!(message.contains("still working after 3s"), "{message}");
        assert!(message.contains("keeps working"), "{message}");

        let answered = handoff(r#"{"t":"turn","status":"completed","text":"DONE"}"#);
        assert_eq!(answered["turn"], "completed");
        assert_eq!(answered["result"], "DONE");
        let message = answered["message"].as_str().unwrap();
        assert!(message.contains("already ended its turn"), "{message}");
        assert!(message.contains("see result"), "{message}");
        assert!(
            message.contains("do NOT invoke it again or wait for it"),
            "{message}"
        );
        assert!(!message.contains("keeps working"), "{message}");

        let failed = handoff(r#"{"t":"turn","status":"failed","message":"boom"}"#);
        assert!(failed["message"].as_str().unwrap().contains("see error"));
    }

    /// Only jobs registered with the turn are stopped when it ends; another
    /// turn's job and an unrelated adopted command keep running.
    #[cfg(unix)]
    #[tokio::test]
    async fn turn_end_stops_only_its_own_jobs() {
        use zdx_tools::process_supervisor::SupervisedCommand;

        async fn adopt_sleeper(bg_id: &str) -> u32 {
            let mut command = SupervisedCommand::new("/bin/sh");
            command.args(["-c", "sleep 60"]).stdin_null();
            let child = command
                .spawn(std::time::Duration::from_millis(200))
                .await
                .expect("spawn sleeper");
            let pid = child.identity.pid;
            zdx_tools::adopted::adopt(
                bg_id.to_string(),
                child,
                "sleep 60".to_string(),
                [tokio::spawn(async {}), tokio::spawn(async {})],
                Box::new(|_| {}),
            );
            pid
        }
        fn alive(pid: u32) -> bool {
            unsafe { libc::kill(pid as i32, 0) == 0 }
        }

        let ours = format!("bg-test-{}", uuid::Uuid::new_v4());
        let other_turn = format!("bg-test-{}", uuid::Uuid::new_v4());
        let unrelated = format!("bg-test-{}", uuid::Uuid::new_v4());
        let ours_pid = adopt_sleeper(&ours).await;
        let other_pid = adopt_sleeper(&other_turn).await;
        let unrelated_pid = adopt_sleeper(&unrelated).await;

        let (other_guard, other_tasks) = TurnTasksGuard::new();
        assert!(other_tasks.register(&other_turn));
        {
            let (guard, tasks) = TurnTasksGuard::new();
            assert!(tasks.register(&ours));
            drop(guard);
            assert!(!tasks.register("bg-late"), "a closed turn accepts nothing");
        }

        let gone = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while zdx_tools::adopted::is_adopted(&ours) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(gone.is_ok(), "the ended turn's job was not stopped");
        assert!(!alive(ours_pid));
        assert!(alive(other_pid), "another turn's job must keep running");
        assert!(
            alive(unrelated_pid),
            "an unrelated adopted job must keep running"
        );

        drop(other_guard);
        zdx_tools::adopted::terminate(&other_turn).await;
        zdx_tools::adopted::terminate(&unrelated).await;
    }
}
