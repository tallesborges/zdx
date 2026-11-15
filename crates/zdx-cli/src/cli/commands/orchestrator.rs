//! `zdx orchestrator`: the orchestrator service (`serve`) and its command-line
//! client (`send`, `status`, `cancel`). Thin wrappers over
//! `zdx_engine::orchestration`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use zdx_engine::core::interrupt;
use zdx_engine::orchestration::client::{OrchestratorClient, ok};
use zdx_engine::orchestration::protocol::{
    Event, Follow, Reply, Request, Route, StatusReport, Submit, socket_path,
};
use zdx_engine::orchestration::updates::InputImage;
use zdx_engine::orchestration::{Orchestrator, server};

const SERVICE_NAME: &str = "orchestrator";

/// Runs the service until interrupted. Workers it started stop with it.
///
/// # Errors
/// Returns an error when another instance runs or the socket cannot be bound.
pub async fn serve(root: &Path) -> Result<()> {
    zdx_engine::pidfile::ensure_unique(SERVICE_NAME)?;
    let _pid_guard =
        zdx_engine::pidfile::write(SERVICE_NAME).context("write orchestrator PID file")?;
    if std::env::var(zdx_engine::service::SUPERVISOR_ENV).as_deref() == Ok("launchd")
        && let Err(err) = zdx_engine::pidfile::mark_supervised(SERVICE_NAME)
    {
        tracing::warn!(err = %format!("{err:#}"), "Failed to mark orchestrator as launchd-supervised");
    }
    let socket = socket_path();
    let config = zdx_engine::config::Config::load_layered(
        &zdx_engine::config::paths::config_layer_paths_for(root),
    )
    .context("load config")?;
    let orchestrator = Orchestrator::start(&config.workers).context("start worker runner")?;
    orchestrator.install_views(Arc::new(
        zdx_engine::orchestration::views::EngineViews::in_service(
            root.to_path_buf(),
            Arc::clone(orchestrator.workers()),
        ),
    ));
    tracing::info!(
        root = %root.display(),
        runner = ?config.workers.runner,
        "Orchestrator service started"
    );
    let result = tokio::select! {
        result = server::serve(orchestrator, socket.clone()) => result,
        () = interrupt::wait_for_interrupt() => {
            tracing::info!("Orchestrator service stopping");
            Ok(())
        }
    };
    let _ = std::fs::remove_file(&socket);
    result
}

pub struct SendOptions<'a> {
    pub root: PathBuf,
    pub thread_id: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub ordinary: bool,
    pub rerun: bool,
    pub route: Option<Route>,
    pub images: &'a [PathBuf],
    pub model: Option<&'a str>,
    pub prompt: String,
    pub wait: bool,
    pub delivery: zdx_engine::orchestration::protocol::Delivery,
}

/// Queues a message. Without `wait`, prints the receipt as one JSON object.
/// With `wait`, prints the receipt and then every event of the thread as
/// JSON lines until the thread settles; exits non-zero when the message was
/// dropped, its turn did not complete, or the service went away first. A
/// routed turn held for delivery is released at once: nothing here delivers.
///
/// # Errors
/// Returns an error when the service is unreachable or refuses the message.
pub async fn send(options: SendOptions<'_>) -> Result<()> {
    let root = options
        .root
        .canonicalize()
        .with_context(|| format!("resolve root {}", options.root.display()))?;
    let images = options
        .images
        .iter()
        .map(|path| InputImage::from_file(path))
        .collect::<Result<Vec<_>>>()?;
    let submit = Submit {
        thread_id: options.thread_id.map(str::to_string),
        root,
        profile: options.profile.map(str::to_string),
        ordinary: options.ordinary,
        rerun: options.rerun,
        chat: false,
        model: options.model.map(str::to_string),
        route: options.route,
        text: options.prompt,
        images,
        follow: if options.wait {
            Follow::Settled
        } else {
            Follow::None
        },
        delivery: options.delivery,
        replay: false,
    };
    let client = OrchestratorClient::new();
    let mut stream = client.open(&Request::Submit(submit)).await?;
    let receipt = ok(stream
        .next()
        .await?
        .context("the orchestrator service closed the connection")?)?;
    print_json(&receipt)?;
    if !options.wait {
        return Ok(());
    }
    // This connection owns the holds of its routed turns; it releases them.
    let holder = match &receipt {
        Reply::Submitted { holder, .. } => holder.clone(),
        _ => None,
    };

    let mut last_turn_ok = None;
    loop {
        let reply = tokio::select! {
            reply = stream.next() => reply?,
            () = interrupt::wait_for_interrupt() => {
                bail!("interrupted; the thread keeps running in the service");
            }
        };
        let Some(reply) = reply else {
            bail!("the orchestrator service closed the connection before the thread settled");
        };
        print_json(&reply)?;
        let Reply::Event { event } = reply else {
            continue;
        };
        match event {
            Event::TurnEnded {
                thread_id,
                turn_id,
                held: true,
                ..
            } => {
                ok(client
                    .call(&Request::Release {
                        thread_id,
                        turn_id,
                        holder: holder.clone(),
                        force: holder.is_none(),
                    })
                    .await?)?;
            }
            Event::Agent {
                event: zdx_engine::core::events::AgentEvent::TurnFinished { status, .. },
                ..
            } => {
                last_turn_ok = Some(matches!(
                    status,
                    zdx_engine::core::events::TurnStatus::Completed
                ));
            }
            Event::InputsDropped { .. } => bail!("the message was dropped before it ran"),
            Event::Settled { .. } => break,
            _ => {}
        }
    }
    if last_turn_ok == Some(false) {
        bail!("the last turn did not complete");
    }
    Ok(())
}

/// Prints the service status: one thread or all of them.
///
/// # Errors
/// Returns an error when the service is unreachable.
pub async fn status(thread_id: Option<&str>, json: bool) -> Result<()> {
    let reply = ok(OrchestratorClient::new()
        .call(&Request::Status {
            thread_id: thread_id.map(str::to_string),
        })
        .await?)?;
    let Reply::Status { report } = reply else {
        bail!("unexpected reply from the orchestrator service");
    };
    if json {
        return print_json(&report);
    }
    print!("{}", format_status(&report));
    Ok(())
}

fn format_status(report: &StatusReport) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "Settled: {}",
        if report.settled { "yes" } else { "no" }
    );
    if report.threads.is_empty() {
        out.push_str("No threads in this service run.\n");
    }
    for thread in &report.threads {
        let running = thread
            .workers
            .iter()
            .filter(|worker| !worker.is_idle())
            .count();
        let _ = writeln!(
            out,
            "{}  {}  profile={}  queued={}  workers={} ({} busy)  settled={}",
            thread.thread_id,
            serde_json::to_value(thread.state)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default(),
            thread.profile.as_deref().unwrap_or("-"),
            thread.queued_inputs,
            thread.workers.len(),
            running,
            if thread.settled { "yes" } else { "no" },
        );
        for worker in &thread.workers {
            let _ = writeln!(
                out,
                "  worker {}  {}  queued={}{}",
                worker.thread_id,
                worker.status.as_str(),
                worker.queue_depth,
                worker
                    .current_tool
                    .as_deref()
                    .map(|tool| format!("  tool={tool}"))
                    .unwrap_or_default(),
            );
        }
    }
    out
}

pub struct CancelOptions<'a> {
    pub thread_id: Option<&'a str>,
    pub worker: Option<&'a str>,
    pub keep_queue: bool,
    pub input: Option<u64>,
    pub turn: Option<u64>,
}

/// Cancels a thread's running turn (and, unless `keep_queue`, its waiting
/// messages), only one input or turn of it, or one worker's turn and queue.
///
/// # Errors
/// Returns an error when the service is unreachable or refuses.
pub async fn cancel(options: CancelOptions<'_>) -> Result<()> {
    let request = match (options.thread_id, options.worker) {
        (Some(thread_id), None) => Request::Cancel {
            thread_id: thread_id.to_string(),
            input_id: options.input,
            turn_id: options.turn,
            clear_queue: !options.keep_queue && options.input.is_none() && options.turn.is_none(),
        },
        (None, Some(worker)) => Request::CancelWorker {
            thread_id: worker.to_string(),
        },
        _ => bail!("pass exactly one of --thread or --worker"),
    };
    let reply = ok(OrchestratorClient::new().call(&request).await?)?;
    print_json(&reply)
}

/// `zdx orchestrator feedback`: the CLI twin of `feedback_submit`. The body
/// goes to the service unchanged.
///
/// # Errors
/// Returns an error when the body cannot be read, the service is
/// unreachable, or it refuses (nothing is queued then).
pub async fn feedback(
    thread_id: &str,
    user_id: i64,
    root: &std::path::Path,
    file: &std::path::Path,
) -> Result<()> {
    use std::io::Read as _;
    let mut submission = String::new();
    if file == std::path::Path::new("-") {
        std::io::stdin()
            .read_to_string(&mut submission)
            .context("read stdin")?;
    } else {
        submission =
            std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    }
    let request = Request::FeedbackSubmit(zdx_engine::orchestration::feedback::FeedbackSubmit {
        thread_id: thread_id.to_string(),
        user_id,
        root: root.to_path_buf(),
        submission,
    });
    let reply = ok(OrchestratorClient::new().call(&request).await?)?;
    print_json(&reply)
}

/// Releases a turn held for delivery: as its holder (`holder`, the token the
/// service gave the holding connection), or by `force`.
///
/// # Errors
/// Returns an error when the service is unreachable or refuses.
pub async fn release(
    thread_id: &str,
    turn_id: u64,
    holder: Option<String>,
    force: bool,
) -> Result<()> {
    let reply = ok(OrchestratorClient::new()
        .call(&Request::Release {
            thread_id: thread_id.to_string(),
            turn_id,
            holder,
            force,
        })
        .await?)?;
    print_json(&reply)
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, value).context("write output")?;
    writeln!(out).context("write output")?;
    out.flush().context("write output")
}

/// `zdx views`: one view request (JSON, as on the socket), answered by the
/// running service when it runs, else here; prints the reply JSON.
///
/// # Errors
/// Returns an error when the request is not a view or is refused.
pub async fn view(root: &Path, file: &Path) -> Result<()> {
    use zdx_engine::orchestration::views::ViewProvider as _;

    let text = if file.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin()).context("read stdin")?
    } else {
        std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?
    };
    let request: zdx_engine::orchestration::views::ViewRequest =
        serde_json::from_str(&text).context("parse view request")?;
    let reply = match zdx_engine::pidfile::status(SERVICE_NAME) {
        zdx_engine::pidfile::ServiceStatus::Running { .. } => ok(OrchestratorClient::new()
            .call(&Request::View(request))
            .await?)?,
        zdx_engine::pidfile::ServiceStatus::Stopped => {
            match zdx_engine::orchestration::views::EngineViews::client(root.to_path_buf())
                .view(request)
                .await
            {
                Ok(output) => Reply::View(output),
                Err(err) => bail!("{}", err.message),
            }
        }
    };
    print_json(&reply)
}
