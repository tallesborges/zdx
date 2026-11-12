//! Unix-socket server of the orchestrator service. Only processes of the
//! same user may connect: the socket is created `0600` and every peer's uid
//! is checked.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};

use super::protocol::{Event, Follow, Reply, Request};
use super::runtime::{CancelTarget, Orchestrator};

/// Serves requests until the listener fails. The caller owns process
/// uniqueness (the service PID file); a socket file left by a dead service is
/// replaced.
///
/// # Errors
/// Returns an error if the socket cannot be bound or accepting fails.
pub async fn serve(orchestrator: Arc<Orchestrator>, socket: PathBuf) -> Result<()> {
    let socket = socket.as_path();
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    match std::fs::remove_file(socket) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("remove {}", socket.display())),
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("bind {}", socket.display()))?;
    restrict_to_owner(socket)?;
    tracing::info!(socket = %socket.display(), "Orchestrator service listening");

    loop {
        let (stream, _) = listener.accept().await.context("accept connection")?;
        if !same_user(&stream) {
            tracing::warn!("Refused orchestrator connection from another user");
            continue;
        }
        let orchestrator = Arc::clone(&orchestrator);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(orchestrator, stream).await {
                tracing::debug!(err = %format!("{err:#}"), "Orchestrator connection ended with an error");
            }
        });
    }
}

#[cfg(unix)]
fn restrict_to_owner(socket: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restrict {}", socket.display()))
}

#[cfg(not(unix))]
fn restrict_to_owner(_socket: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn same_user(stream: &UnixStream) -> bool {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    stream.peer_cred().is_ok_and(|cred| cred.uid() == uid)
}

#[cfg(not(unix))]
fn same_user(_stream: &UnixStream) -> bool {
    true
}

async fn write_reply(write: &mut OwnedWriteHalf, reply: &Reply) -> Result<()> {
    let mut line = serde_json::to_vec(reply).context("encode reply")?;
    line.push(b'\n');
    write.write_all(&line).await.context("write reply")?;
    write.flush().await.context("flush reply")
}

#[allow(clippy::too_many_lines)]
async fn handle_connection(orchestrator: Arc<Orchestrator>, stream: UnixStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let Some(line) = lines.next_line().await.context("read request")? else {
        return Ok(());
    };
    let request: Request = match serde_json::from_str(&line) {
        Ok(request) => request,
        Err(err) => {
            let message = format!("invalid request: {err}");
            return write_reply(&mut write, &Reply::Error { message }).await;
        }
    };

    let reply = match request {
        Request::Submit(submit) => {
            let follow = submit.follow;
            if follow == Follow::None {
                submitted(orchestrator.submit(submit))
            } else {
                return follow_submit(orchestrator, submit, follow, lines, write).await;
            }
        }
        Request::Subscribe { renderer } => {
            let mut subscription = orchestrator.subscribe(None, renderer);
            write_reply(&mut write, &Reply::Ok).await?;
            loop {
                tokio::select! {
                    event = subscription.events.recv() => {
                        let Some(event) = event else { return Ok(()) };
                        write_reply(&mut write, &Reply::Event { event }).await?;
                    }
                    line = lines.next_line() => {
                        // The client closed its side (or sent junk): stop.
                        if !matches!(line, Ok(Some(_))) {
                            return Ok(());
                        }
                    }
                }
            }
        }
        Request::Status { thread_id } => Reply::Status {
            report: orchestrator.status(thread_id.as_deref()).await,
        },
        Request::Cancel {
            thread_id,
            input_id,
            turn_id,
            clear_queue,
        } => {
            let target = match (input_id, turn_id) {
                (Some(input), _) => CancelTarget::Input(input),
                (None, Some(turn)) => CancelTarget::Turn(turn),
                (None, None) => CancelTarget::Thread { clear_queue },
            };
            let (cancelled_turn, dropped_inputs) = orchestrator.cancel(&thread_id, target);
            Reply::Cancelled {
                thread_id,
                cancelled_turn,
                dropped_inputs,
            }
        }
        Request::Release { thread_id, turn_id } => {
            orchestrator.release(&thread_id, turn_id);
            Reply::Ok
        }
        Request::Workers { owner_thread_id } => {
            let workers = orchestrator.workers();
            Reply::Workers {
                workers: match owner_thread_id {
                    Some(owner) => workers.list_for_owner(&owner),
                    None => workers.list_all(),
                },
            }
        }
        Request::Worker { thread_id } => Reply::Worker {
            worker: orchestrator.workers().snapshot(&thread_id),
        },
        Request::CancelWorker { thread_id } => match orchestrator.workers().cancel(&thread_id) {
            Ok(snapshot) => Reply::Worker {
                worker: Some(snapshot),
            },
            Err(err) => Reply::Error {
                message: format!("{err:#}"),
            },
        },
        Request::EnqueueFromTopic {
            worker_thread_id,
            prompt,
        } => match orchestrator
            .workers()
            .enqueue_from_topic(&worker_thread_id, &prompt)
        {
            Ok(snapshot) => Reply::Worker {
                worker: Some(snapshot),
            },
            Err(err) => Reply::Error {
                message: format!("{err:#}"),
            },
        },
    };
    write_reply(&mut write, &reply).await
}

fn submitted(result: Result<super::runtime::Submitted>) -> Reply {
    match result {
        Ok(submitted) => Reply::Submitted {
            thread_id: submitted.thread_id,
            input_id: submitted.input_id,
            queued_ahead: submitted.queued_ahead,
            in_turn: submitted.in_turn,
        },
        Err(err) => Reply::Error {
            message: format!("{err:#}"),
        },
    }
}

/// Queues the input, replies, then streams the thread's events from the turn
/// that runs it: that turn only (`Follow::Turn`), or every turn until the
/// thread settles (`Follow::Settled`). Ends early when the input is dropped
/// by a cancel or the client goes away.
async fn follow_submit(
    orchestrator: Arc<Orchestrator>,
    submit: super::protocol::Submit,
    follow: Follow,
    mut lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    mut write: OwnedWriteHalf,
) -> Result<()> {
    // The thread id is known only after submit for a new thread, so the
    // subscription takes every event and filters here. A Telegram-routed
    // follower delivers the reply, so its subscription holds the turn.
    let mut subscription = orchestrator.subscribe(None, false);
    let holder = submit.route.is_some().then(|| subscription.id());
    let reply = submitted(orchestrator.submit_held(submit, holder));
    write_reply(&mut write, &reply).await?;
    let Reply::Submitted {
        thread_id,
        input_id,
        in_turn: false,
        ..
    } = reply
    else {
        // An error, or a steer that joined the running turn: nothing of its
        // own to follow.
        return Ok(());
    };

    let mut my_turn: Option<u64> = None;
    let mut turn_over = false;
    loop {
        let event = tokio::select! {
            event = subscription.events.recv() => event,
            line = lines.next_line() => {
                if matches!(line, Ok(Some(_))) { continue; }
                return Ok(());
            }
        };
        let Some(event) = event else {
            return Ok(());
        };
        if event.thread_id() != Some(thread_id.as_str()) {
            continue;
        }
        let mut done = false;
        match &event {
            Event::TurnStarted {
                turn_id, inputs, ..
            } if my_turn.is_none() => {
                if !inputs.iter().any(|input| input.input_id == input_id) {
                    continue;
                }
                my_turn = Some(*turn_id);
            }
            // Only this input's drop concerns this follower.
            Event::InputsDropped { input_ids, .. } => {
                if !input_ids.contains(&input_id) {
                    continue;
                }
                done = true;
            }
            Event::TurnEnded { turn_id, .. } if Some(*turn_id) == my_turn => {
                turn_over = true;
                done = follow == Follow::Turn;
            }
            Event::Settled { .. } => {
                if !turn_over {
                    continue;
                }
                done = true;
            }
            _ if my_turn.is_none() => continue,
            _ => {}
        }
        let held_turn = match &event {
            Event::TurnEnded {
                turn_id,
                held: true,
                ..
            } if holder.is_some() && Some(*turn_id) == my_turn => Some(*turn_id),
            _ => None,
        };
        write_reply(&mut write, &Reply::Event { event }).await?;
        if let Some(turn_id) = held_turn {
            // The hold is this connection's: keep it open until the client
            // releases the turn (or disconnects, which frees the hold).
            tokio::select! {
                () = orchestrator.wait_released(&thread_id, turn_id) => {}
                () = drain_until_closed(&mut lines) => return Ok(()),
            }
        }
        if done {
            return Ok(());
        }
    }
}

/// Reads and ignores client lines until the client closes its side.
async fn drain_until_closed(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) {
    while let Ok(Some(_)) = lines.next_line().await {}
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::core::events::{AgentEvent, TurnStatus};
    use crate::core::workers::{WorkerManager, WorkerRunRequest};
    use crate::orchestration::client::{OrchestratorClient, ok};
    use crate::orchestration::protocol::{Submit, TurnKind};
    use crate::orchestration::runtime::{TurnRequest, TurnRunner};

    fn echo_turns() -> TurnRunner {
        Arc::new(|request: TurnRequest| {
            Box::pin(async move {
                let _ = request.events.send(Arc::new(AgentEvent::TurnFinished {
                    status: TurnStatus::Completed,
                    final_text: format!("echo: {}", request.text),
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            })
        })
    }

    /// A followed steer that joins the running turn gets its receipt and the
    /// connection closes: there is no turn of its own to follow.
    #[tokio::test]
    async fn a_followed_steer_into_the_running_turn_closes_after_the_receipt() {
        let _home = crate::test_support::temp_zdx_home();
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("o.sock");
        let (workers, events) = WorkerManager::with_runner(Arc::new(|_: WorkerRunRequest| {
            Box::pin(async { Ok(String::new()) })
        }));
        let gate = Arc::new(tokio::sync::Notify::new());
        let turn_gate = Arc::clone(&gate);
        let runner: TurnRunner = Arc::new(move |request: TurnRequest| {
            let gate = Arc::clone(&turn_gate);
            Box::pin(async move {
                gate.notified().await;
                let _ = request.events.send(Arc::new(AgentEvent::TurnFinished {
                    status: TurnStatus::Completed,
                    final_text: String::new(),
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            })
        });
        let orchestrator = Orchestrator::start_with(workers, events, runner);
        let mut started = orchestrator.subscribe(Some("busy".to_string()), false);
        tokio::spawn(serve(Arc::clone(&orchestrator), socket.clone()));
        let client = OrchestratorClient::at(&socket);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(tokio::time::Instant::now() < deadline, "socket never bound");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let submit = |text: &str, delivery| Submit {
            thread_id: Some("busy".to_string()),
            root: dir.path().to_path_buf(),
            profile: None,
            model: None,
            route: None,
            text: text.to_string(),
            images: Vec::new(),
            follow: Follow::Turn,
            delivery,
        };
        let _first = client
            .open(&Request::Submit(submit(
                "work",
                crate::orchestration::protocol::Delivery::Queue,
            )))
            .await
            .unwrap();
        while !matches!(started.events.recv().await, Some(Event::TurnStarted { .. })) {}

        let mut stream = client
            .open(&Request::Submit(submit(
                "detail",
                crate::orchestration::protocol::Delivery::Steer,
            )))
            .await
            .unwrap();
        let receipt = ok(stream.next().await.unwrap().unwrap()).unwrap();
        assert!(matches!(receipt, Reply::Submitted { in_turn: true, .. }));
        assert!(
            stream.next().await.unwrap().is_none(),
            "closed after the receipt"
        );
        gate.notify_one();
    }

    /// End to end over the socket: a followed submit streams exactly its own
    /// turn, and the blocking client reads status from the same service.
    #[tokio::test]
    async fn a_followed_submit_streams_its_turn_over_the_socket() {
        let _home = crate::test_support::temp_zdx_home();
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("o.sock");
        let (workers, events) = WorkerManager::with_runner(Arc::new(|_: WorkerRunRequest| {
            Box::pin(async { Ok(String::new()) })
        }));
        let orchestrator = Orchestrator::start_with(workers, events, echo_turns());
        let server = tokio::spawn(serve(orchestrator, socket.clone()));
        let client = OrchestratorClient::at(&socket);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(tokio::time::Instant::now() < deadline, "socket never bound");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let mut stream = client
            .open(&Request::Submit(Submit {
                thread_id: Some("socket-thread".to_string()),
                root: dir.path().to_path_buf(),
                profile: None,
                model: None,
                route: None,
                text: "hello".to_string(),
                images: Vec::new(),
                follow: Follow::Turn,
                delivery: crate::orchestration::protocol::Delivery::Queue,
            }))
            .await
            .unwrap();
        let first = ok(stream.next().await.unwrap().unwrap()).unwrap();
        assert!(
            matches!(first, Reply::Submitted { ref thread_id, .. } if thread_id == "socket-thread")
        );
        let mut events = Vec::new();
        while let Some(reply) = stream.next().await.unwrap() {
            let Reply::Event { event } = reply else {
                panic!("unexpected reply {reply:?}");
            };
            events.push(event);
        }
        assert!(matches!(
            &events[0],
            Event::TurnStarted { kind: TurnKind::User, inputs, followed: true, .. } if inputs.len() == 1
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Agent { event: AgentEvent::TurnFinished { final_text, .. }, .. } if final_text == "echo: hello"
        )));
        assert!(matches!(
            events.last(),
            Some(Event::TurnEnded { held: false, .. })
        ));

        let status = tokio::task::spawn_blocking(move || {
            client.request(&Request::Status {
                thread_id: Some("socket-thread".to_string()),
            })
        })
        .await
        .unwrap()
        .unwrap();
        let Reply::Status { report } = status else {
            panic!("unexpected reply {status:?}");
        };
        assert_eq!(report.threads[0].thread_id, "socket-thread");
        server.abort();
    }

    /// Without a service the client says how to start one.
    #[tokio::test]
    async fn a_missing_service_names_the_command_to_start_it() {
        let dir = tempfile::tempdir().unwrap();
        let client = OrchestratorClient::at(dir.path().join("none.sock"));
        let err = client
            .call(&Request::Status { thread_id: None })
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("zdx service start orchestrator"),
            "{err}"
        );
        assert!(client.workers(None).await.is_empty());
    }
}
