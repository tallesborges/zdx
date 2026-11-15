//! Unix-socket server of the orchestrator service. Only processes of the
//! same user may connect: the socket is created `0600` and every peer's uid
//! is checked.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};

use super::protocol::{Event, Follow, Reply, Request};
use super::runtime::{CancelTarget, Orchestrator};
use super::thread_ops;

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

/// Largest request line: room for a 48 MiB feedback body sent as a JSON
/// string, or an album of inline images.
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
/// A client must send its whole request line within this time.
const REQUEST_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);

#[allow(clippy::too_many_lines)]
async fn handle_connection(orchestrator: Arc<Orchestrator>, stream: UnixStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    let read = tokio::time::timeout(
        REQUEST_READ_TIMEOUT,
        (&mut reader)
            .take(MAX_REQUEST_BYTES as u64 + 1)
            .read_until(b'\n', &mut line),
    )
    .await;
    match read {
        Err(_) => return Ok(()),
        Ok(read) => {
            if read.context("read request")? == 0 {
                return Ok(());
            }
        }
    }
    if line.len() > MAX_REQUEST_BYTES {
        return write_reply(
            &mut write,
            &Reply::Error {
                message: format!("request is larger than {MAX_REQUEST_BYTES} bytes"),
                code: Some(super::protocol::ErrorCode::Invalid),
            },
        )
        .await;
    }
    let mut lines = reader.lines();
    let request: Request = match serde_json::from_slice(&line) {
        Ok(request) => request,
        Err(err) => {
            let message = format!("invalid request: {err}");
            return write_reply(
                &mut write,
                &Reply::Error {
                    message,
                    code: Some(super::protocol::ErrorCode::Invalid),
                },
            )
            .await;
        }
    };

    let reply = match request {
        Request::Submit(submit) => {
            let follow = submit.follow;
            if follow == Follow::None {
                submitted(orchestrator.submit(submit), None)
            } else {
                return follow_submit(orchestrator, submit, follow, lines, write).await;
            }
        }
        Request::Subscribe {
            renderer,
            replay,
            thread_id,
        } => {
            if let Some(id) = thread_id.as_deref()
                && let Err(err) = thread_ops::validate_thread_id(id)
            {
                return write_reply(&mut write, &error_reply(&err)).await;
            }
            let mut subscription = match orchestrator.subscribe(thread_id, renderer, replay) {
                Ok(subscription) => subscription,
                Err(err) => return write_reply(&mut write, &error_reply(&err)).await,
            };
            let first = if renderer {
                Reply::Subscribed {
                    holder: subscription.token().to_string(),
                }
            } else {
                Reply::Ok
            };
            write_reply(&mut write, &first).await?;
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
        Request::Release {
            thread_id,
            turn_id,
            holder,
            force,
        } => match orchestrator.release(&thread_id, turn_id, holder.as_deref(), force) {
            Ok(()) => Reply::Ok,
            Err(err) => error_reply(&err),
        },
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
            Err(err) => error_reply(&err),
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
            Err(err) => error_reply(&err),
        },
        Request::UpdateThread(update) => {
            let service = Arc::clone(&orchestrator);
            match tokio::task::spawn_blocking(move || service.update_thread(&update)).await {
                Ok(Ok(updated)) => Reply::ThreadUpdated(updated),
                Ok(Err(err)) => error_reply(&err),
                Err(err) => error_reply(&anyhow::anyhow!("update thread: {err}")),
            }
        }
        Request::RecordDelivery {
            thread_id,
            record,
            turn_id,
            holder,
        } => match orchestrator
            .record_delivery(thread_id, record, turn_id, holder)
            .await
        {
            Ok(user_summary) => Reply::DeliveryRecorded { user_summary },
            Err(err) => error_reply(&err),
        },
        Request::Generate(request) => match super::helpers::generate(request).await {
            Ok(text) => Reply::Generated { text },
            Err(err) => error_reply(&err),
        },
        Request::Catalog(query) => {
            let workers = Arc::clone(orchestrator.workers());
            match tokio::task::spawn_blocking(move || {
                super::catalog::catalog(query, Some(&workers))
            })
            .await
            {
                Ok(Ok(body)) => Reply::Catalog { body },
                Ok(Err(err)) => error_reply(&err),
                Err(err) => error_reply(&anyhow::anyhow!("catalog: {err}")),
            }
        }
        Request::SetChatModel { root, model } => {
            match tokio::task::spawn_blocking(move || super::catalog::set_chat_model(&root, &model))
                .await
            {
                Ok(Ok(selection)) => Reply::ChatModel { selection },
                Ok(Err(err)) => error_reply(&err),
                Err(err) => error_reply(&anyhow::anyhow!("set chat model: {err}")),
            }
        }
        Request::BtwSeed {
            thread_id,
            question,
        } => match super::helpers::btw_seed(&thread_id, &question) {
            Ok(text) => Reply::Text { text },
            Err(err) => error_reply(&err),
        },
        Request::TopicStart(request) => {
            match tokio::task::spawn_blocking(move || super::helpers::topic_start(&request)).await {
                Ok(Ok(reply)) => Reply::TopicStart(reply),
                Ok(Err(err)) => error_reply(&err),
                Err(err) => error_reply(&anyhow::anyhow!("topic start: {err}")),
            }
        }
        Request::FeedbackSubmit(request) => {
            // Its own task: a client that goes away cannot stop it between
            // marking the bundle queued and queueing it.
            let orchestrator = Arc::clone(&orchestrator);
            match tokio::spawn(async move { orchestrator.feedback_submit(request).await }).await {
                Ok(Ok(receipt)) => Reply::Feedback(receipt),
                Ok(Err(err)) => error_reply(&err),
                Err(err) => error_reply(&anyhow::anyhow!("feedback task: {err}")),
            }
        }
        Request::ResetThread { thread_id } => match orchestrator.reset_thread(&thread_id) {
            Ok(()) => Reply::Ok,
            Err(err) => error_reply(&err),
        },
        Request::ResolveThread { chat_id, topic_id } => {
            match tokio::task::spawn_blocking(move || thread_ops::resolve_thread(chat_id, topic_id))
                .await
            {
                Ok(Ok(route)) => Reply::ThreadRoute(route),
                Ok(Err(err)) => error_reply(&err),
                Err(err) => error_reply(&anyhow::anyhow!("resolve thread: {err}")),
            }
        }
        Request::ComposeUserText(input) => Reply::Text {
            text: crate::core::chat_turn::compose_user_text(&input),
        },
        Request::PrepareImage { path } => {
            if let Err(err) = check_input_size(&path) {
                return write_reply(&mut write, &error_reply(&err)).await;
            }
            match tokio::task::spawn_blocking(move || {
                std::fs::read(&path)
                    .map(|bytes| crate::core::chat_turn::prepare_inline_image(&path, &bytes))
            })
            .await
            {
                Ok(Ok(prepared)) => Reply::PreparedImage(prepared),
                Ok(Err(err)) => Reply::Error {
                    message: format!("read image: {err}"),
                    code: Some(super::protocol::ErrorCode::NotFound),
                },
                Err(err) => error_reply(&anyhow::anyhow!("prepare image: {err}")),
            }
        }
        Request::PreparePhoto { path } => {
            if !path.is_file() {
                return write_reply(
                    &mut write,
                    &Reply::Error {
                        message: format!("photo not found: {}", path.display()),
                        code: Some(super::protocol::ErrorCode::NotFound),
                    },
                )
                .await;
            }
            match tokio::task::spawn_blocking(move || prepare_photo(&path)).await {
                Ok(Ok(reply)) => reply,
                Ok(Err(err)) => error_reply(&err),
                Err(err) => error_reply(&anyhow::anyhow!("prepare photo: {err}")),
            }
        }
        Request::TranscribeAudio {
            path,
            file_name,
            mime_type,
            root,
        } => match transcribe_audio(&path, &file_name, mime_type.as_deref(), &root).await {
            Ok(transcript) => Reply::Transcript { transcript },
            Err(err) => error_reply(&err),
        },
        Request::View(request) => match orchestrator.views() {
            Some(views) => match views.view(request).await {
                Ok(output) => Reply::View(output),
                Err(err) => Reply::Error {
                    message: err.message,
                    code: Some(err.code),
                },
            },
            None => Reply::Error {
                message: "this service has no views".to_string(),
                code: Some(super::protocol::ErrorCode::Invalid),
            },
        },
        Request::ReplyParts { final_text } => {
            match tokio::task::spawn_blocking(move || {
                crate::core::chat_turn::reply_parts(&final_text)
            })
            .await
            {
                Ok(parts) => Reply::ReplyParts(parts),
                Err(err) => error_reply(&anyhow::anyhow!("reply parts: {err}")),
            }
        }
    };
    write_reply(&mut write, &reply).await
}

fn submitted(result: Result<super::runtime::Submitted>, holder: Option<String>) -> Reply {
    match result {
        Ok(submitted) => Reply::Submitted {
            thread_id: submitted.thread_id,
            input_id: submitted.input_id,
            queued_ahead: submitted.queued_ahead,
            in_turn: submitted.in_turn,
            holder: holder.filter(|_| !submitted.in_turn),
        },
        Err(err) => error_reply(&err),
    }
}

/// `prepare_photo`: the file made ready for `sendPhoto`, inline only when it
/// changed. Blocking.
///
/// # Errors
/// Returns an error when the file cannot be read or prepared.
pub fn prepare_photo(path: &Path) -> Result<Reply> {
    use base64::Engine as _;
    let photo = crate::telegram::photo::prepare_photo_file(path)?;
    Ok(Reply::PreparedPhoto {
        changed: photo.changed,
        data: photo
            .changed
            .then(|| base64::engine::general_purpose::STANDARD.encode(&photo.bytes)),
        file_name: photo.file_name,
        mime_type: photo.mime_type,
    })
}

/// Largest incoming media file `prepare_image` and `transcribe_audio` read:
/// above every input surfaces save (Telegram downloads stop at 20 MiB, voice
/// notes at 25 MiB).
pub const MAX_INPUT_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// `not_found` for a missing file, `invalid` for one over
/// [`MAX_INPUT_FILE_BYTES`]; checked before anything is read.
fn check_input_size(path: &Path) -> Result<()> {
    let len = std::fs::metadata(path)
        .map_err(|err| {
            thread_ops::coded(
                super::protocol::ErrorCode::NotFound,
                format!("read {}: {err}", path.display()),
            )
        })?
        .len();
    if len > MAX_INPUT_FILE_BYTES {
        return Err(thread_ops::coded(
            super::protocol::ErrorCode::Invalid,
            format!(
                "{} is {len} bytes, over the {MAX_INPUT_FILE_BYTES}-byte input limit",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Transcribes an incoming audio file with the transcription config of
/// `root`, as the bot does for a voice note.
///
/// # Errors
/// `not_found` when the file cannot be read; otherwise the provider error.
pub async fn transcribe_audio(
    path: &Path,
    file_name: &str,
    mime_type: Option<&str>,
    root: &Path,
) -> Result<Option<String>> {
    check_input_size(path)?;
    let bytes = tokio::fs::read(path).await.map_err(|err| {
        thread_ops::coded(
            super::protocol::ErrorCode::NotFound,
            format!("read {}: {err}", path.display()),
        )
    })?;
    let config =
        crate::config::Config::load_layered(&crate::config::paths::config_layer_paths_for(root))
            .context("load config")?;
    crate::audio::transcribe::transcribe_audio_if_configured(
        &config,
        &config.transcription,
        bytes,
        file_name,
        mime_type,
        None,
    )
    .await
}

/// The error reply for `err`, with its code when it has one.
fn error_reply(err: &anyhow::Error) -> Reply {
    Reply::Error {
        message: format!("{err:#}"),
        code: thread_ops::error_code(err),
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
    let mut subscription = match orchestrator.subscribe(None, false, false) {
        Ok(subscription) => subscription,
        Err(err) => return write_reply(&mut write, &error_reply(&err)).await,
    };
    let holder = submit
        .route
        .is_some()
        .then(|| subscription.holder(submit.replay));
    let token = holder.as_ref().map(|holder| holder.token.clone());
    let reply = submitted(orchestrator.submit_held(submit, holder), token.clone());
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
            } if token.is_some() && Some(*turn_id) == my_turn => Some(*turn_id),
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
    #[test]
    fn input_files_over_the_limit_are_refused_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.ogg");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_INPUT_FILE_BYTES + 1)
            .unwrap();
        let code = |path: &Path| thread_ops::error_code(&check_input_size(path).unwrap_err());
        assert_eq!(code(&big), Some(super::super::protocol::ErrorCode::Invalid));
        assert_eq!(
            code(&dir.path().join("missing")),
            Some(super::super::protocol::ErrorCode::NotFound)
        );
    }

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
        let mut started = orchestrator
            .subscribe(Some("busy".to_string()), false, false)
            .unwrap();
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
            ordinary: false,
            rerun: false,
            chat: false,
            model: None,
            route: None,
            text: text.to_string(),
            images: Vec::new(),
            follow: Follow::Turn,
            delivery,
            replay: false,
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
                ordinary: false,
                rerun: false,
                chat: false,
                model: None,
                route: None,
                text: "hello".to_string(),
                images: Vec::new(),
                follow: Follow::Turn,
                delivery: crate::orchestration::protocol::Delivery::Queue,
                replay: false,
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

    /// Sends one raw request line, as a non-Rust client does, and returns the
    /// reply line.
    async fn raw(socket: &Path, request: &serde_json::Value) -> serde_json::Value {
        let stream = UnixStream::connect(socket).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut line = serde_json::to_vec(request).unwrap();
        line.push(b'\n');
        write.write_all(&line).await.unwrap();
        let reply = BufReader::new(read)
            .lines()
            .next_line()
            .await
            .unwrap()
            .expect("reply line");
        serde_json::from_str(&reply).unwrap()
    }

    /// The chat-surface thread operations over the wire, in the JSON a Go
    /// client sends: route lookup, metadata, delivery records, user text,
    /// reply parts, and reset with its refusals.
    #[tokio::test]
    async fn thread_operations_answer_raw_json_requests() {
        let _home = crate::test_support::temp_zdx_home();
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("o.sock");
        let (workers, events) = WorkerManager::with_runner(Arc::new(|_: WorkerRunRequest| {
            Box::pin(async { Ok(String::new()) })
        }));
        let orchestrator = Orchestrator::start_with(workers, events, echo_turns());
        let server = tokio::spawn(serve(orchestrator, socket.clone()));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(tokio::time::Instant::now() < deadline, "socket never bound");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let root = dir.path().canonicalize().unwrap();
        let thread = "telegram--100-topic-7";

        let route = raw(
            &socket,
            &serde_json::json!({"op":"resolve_thread","chat_id":-100,"topic_id":7}),
        )
        .await;
        assert_eq!(route["reply"], "thread_route", "{route}");
        assert_eq!(route["thread_id"], thread);
        assert_eq!(route["effective_thread_id"], thread);
        assert_eq!(route["kind"], "ordinary");
        assert_eq!(route["exists"], false);

        let updated = raw(
            &socket,
            &serde_json::json!({"op":"update_thread","thread_id":thread,"root":root,"pending_topic_title":true,"title":"Topic"}),
        )
        .await;
        assert_eq!(updated["reply"], "thread_updated", "{updated}");
        assert_eq!(updated["title"], "Topic");
        let route = raw(
            &socket,
            &serde_json::json!({"op":"resolve_thread","chat_id":-100,"topic_id":7}),
        )
        .await;
        assert_eq!(route["exists"], true);
        assert_eq!(route["pending_topic_title"], true);
        assert_eq!(route["root"], root.display().to_string());

        let record = serde_json::json!({"id":"r1","surface":"telegram","chat_id":-100,"topic_id":7,"items":[{"kind":"text","status":"sent","message_id":5}]});
        let recorded = raw(
            &socket,
            &serde_json::json!({"op":"record_delivery","thread_id":thread,"record":record}),
        )
        .await;
        assert_eq!(recorded["reply"], "delivery_recorded", "{recorded}");
        assert!(recorded.get("user_summary").is_none(), "{recorded}");
        let missing = raw(
            &socket,
            &serde_json::json!({"op":"record_delivery","thread_id":"missing","record":record}),
        )
        .await;
        assert_eq!(missing["reply"], "error");
        assert_eq!(missing["code"], "not_found");

        let text = raw(
            &socket,
            &serde_json::json!({"op":"compose_user_text","text":"hi","documents":[{"file_name":"a.pdf","path":"/tmp/a.pdf"}]}),
        )
        .await;
        assert_eq!(text["reply"], "text", "{text}");
        assert!(
            text["text"]
                .as_str()
                .unwrap()
                .starts_with("hi\n\nDocument attachment 'a.pdf'")
        );

        let parts = raw(
            &socket,
            &serde_json::json!({"op":"reply_parts","final_text":"Done.\n<media>/nope/x.html</media>"}),
        )
        .await;
        assert_eq!(parts["reply"], "reply_parts", "{parts}");
        assert_eq!(parts["media"][0]["kind"], "html");
        assert_eq!(parts["media"][0]["send_as"], "document");
        assert_eq!(parts["media"][0]["sendable"], false);

        let bad = raw(
            &socket,
            &serde_json::json!({"op":"update_thread","thread_id":"../escape"}),
        )
        .await;
        assert_eq!(bad["code"], "invalid", "{bad}");
        let unknown = raw(&socket, &serde_json::json!({"op":"no_such_op"})).await;
        assert_eq!(unknown["code"], "invalid", "{unknown}");

        let reset = raw(
            &socket,
            &serde_json::json!({"op":"reset_thread","thread_id":thread}),
        )
        .await;
        assert_eq!(reset["reply"], "ok", "{reset}");
        assert!(!crate::core::thread_persistence::thread_exists(thread));
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
