//! `OpenAI` Responses API WebSocket transport (alpha).
//!
//! One persistent socket to `/v1/responses`, one in-flight response per turn.
//! Server events match the SSE transport, so they reuse `ResponsesEventMapper`.
//! `previous_response_id` chaining is an optimization; any break falls back to a
//! full-input send, which is always correct.

use std::sync::Arc;

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, Stream, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use zdx_types::ToolDefinition;

use super::responses::{ResponsesConfig, build_input, build_request_body_from_input};
use super::responses_sse::ResponsesEventMapper;
use super::responses_types::RequestBody;
use crate::debug_metrics::maybe_wrap_with_metrics;
use crate::{
    ChatMessage, ProviderError, ProviderErrorKind, ProviderResult, ProviderStream, StreamEvent,
};

type WsSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Default)]
struct SessionInner {
    socket: Option<WsSocket>,
    last_response_id: Option<String>,
    /// Serialized full input that produced `last_response_id` (the prefix base).
    last_input: Vec<Value>,
}

/// Produces the auth/identity headers (name/value pairs) attached to the
/// WebSocket handshake. Async so the Codex path can resolve/refresh OAuth
/// credentials at connect time.
pub type WsHeaderFactory =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<(String, String)>>> + Send + Sync>;

/// `OpenAI` Responses API client over a persistent WebSocket connection.
pub struct OpenAIResponsesWsClient {
    header_factory: WsHeaderFactory,
    config: ResponsesConfig,
    /// When true the per-turn system prompt is sent as top-level `instructions`
    /// and omitted from `input` (Codex); otherwise it becomes a `developer`
    /// input item (`OpenAI` API-key path).
    system_as_instructions: bool,
    session: Arc<Mutex<SessionInner>>,
}

impl OpenAIResponsesWsClient {
    pub fn new(
        header_factory: WsHeaderFactory,
        config: ResponsesConfig,
        system_as_instructions: bool,
    ) -> Self {
        Self {
            header_factory,
            config,
            system_as_instructions,
            session: Arc::new(Mutex::new(SessionInner::default())),
        }
    }

    /// Convenience constructor for static `Bearer` auth (API-key path).
    pub fn bearer(api_key: String, config: ResponsesConfig) -> Self {
        let factory: WsHeaderFactory = Arc::new(move || {
            let api_key = api_key.clone();
            Box::pin(async move {
                Ok(vec![(
                    "Authorization".to_string(),
                    format!("Bearer {api_key}"),
                )])
            })
        });
        Self::new(factory, config, false)
    }

    /// Runs one turn over the persistent socket and returns a stream of events.
    ///
    /// # Errors
    /// Returns an error if the request cannot be built, the socket cannot be
    /// opened, or the opening frame cannot be sent.
    ///
    /// # Panics
    /// Panics if the session socket is absent right after being ensured present.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let mut guard = Arc::clone(&self.session).lock_owned().await;

        if guard.socket.is_none() {
            let socket = self.connect().await?;
            guard.socket = Some(socket);
            guard.last_response_id = None;
            guard.last_input.clear();
        }

        let (input_system, instructions) = if self.system_as_instructions {
            let resolved = system
                .map(str::trim)
                .filter(|prompt| !prompt.is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| self.config.instructions.clone());
            (None, resolved)
        } else {
            (system, self.config.instructions.clone())
        };

        let mut full_items = build_input(messages, input_system);
        if full_items.is_empty() {
            bail!("No input messages provided for OpenAI request");
        }
        let snapshot = full_items
            .iter()
            .map(serde_json::to_value)
            .collect::<serde_json::Result<Vec<Value>>>()?;

        let (input, previous_response_id) = match plan_send(
            &snapshot,
            &guard.last_input,
            guard.last_response_id.as_deref(),
        ) {
            SendPlan::Full => (full_items, None),
            SendPlan::Continuation {
                skip,
                previous_response_id,
            } => {
                full_items.drain(..skip);
                (full_items, Some(previous_response_id))
            }
        };

        let mut request =
            build_request_body_from_input(&self.config, input, tools, previous_response_id);
        request.instructions = instructions;
        let frame = response_create_frame(&request)?;

        let send_result = {
            let socket = guard
                .socket
                .as_mut()
                .expect("socket present after ensure-connected");
            socket.send(Message::Text(frame.into())).await
        };
        if let Err(e) = send_result {
            guard.socket = None;
            return Err(classify_websocket_error(e).into());
        }

        let turn = TurnState {
            guard,
            mapper: ResponsesEventMapper::new(self.config.model.clone()),
            pending_snapshot: snapshot,
            completed: false,
        };
        Ok(maybe_wrap_with_metrics(turn_event_stream(turn)))
    }

    async fn connect(&self) -> Result<WsSocket> {
        let ws_url = to_ws_url(&self.config.base_url, &self.config.path);
        let mut request = ws_url
            .into_client_request()
            .map_err(|e| ProviderError::request(format!("Invalid WebSocket URL: {e}")))?;
        for (name, value) in (self.header_factory)().await? {
            let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                ProviderError::new(
                    ProviderErrorKind::Parse,
                    format!("Invalid WebSocket header name {name}: {e}"),
                )
            })?;
            let header_value = HeaderValue::from_str(&value).map_err(|e| {
                ProviderError::new(
                    ProviderErrorKind::Parse,
                    format!("Invalid WebSocket header value for {name}: {e}"),
                )
            })?;
            request.headers_mut().insert(header_name, header_value);
        }

        let (socket, _response) = connect_async(request)
            .await
            .map_err(classify_websocket_error)?;
        Ok(socket)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SendPlan {
    Full,
    Continuation {
        skip: usize,
        previous_response_id: String,
    },
}

/// Continues the active response only when the new input exactly extends the
/// previous snapshot; otherwise resends the full input.
fn plan_send(
    new_input: &[Value],
    last_input: &[Value],
    last_response_id: Option<&str>,
) -> SendPlan {
    if let Some(id) = last_response_id
        && !last_input.is_empty()
        && new_input.len() > last_input.len()
        && new_input[..last_input.len()] == *last_input
    {
        SendPlan::Continuation {
            skip: last_input.len(),
            previous_response_id: id.to_string(),
        }
    } else {
        SendPlan::Full
    }
}

/// Serializes a `RequestBody` as a `response.create` frame; the transport-only
/// `stream` field is dropped.
fn response_create_frame(request: &RequestBody) -> Result<String> {
    let mut value = serde_json::to_value(request)?;
    let object = value
        .as_object_mut()
        .expect("RequestBody serializes to a JSON object");
    object.remove("stream");
    object.insert(
        "type".to_string(),
        Value::String("response.create".to_string()),
    );
    Ok(serde_json::to_string(&value)?)
}

fn to_ws_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let ws_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{ws_base}{path}")
}

fn frame_is_terminal(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(|kind| kind == "response.completed" || kind == "response.done")
        })
        .unwrap_or(false)
}

enum Ingest {
    Continue,
    Completed,
    Failed(ProviderError),
}

fn classify_websocket_error(err: WsError) -> ProviderError {
    match err {
        WsError::ConnectionClosed
        | WsError::AlreadyClosed
        | WsError::Io(_)
        | WsError::Tls(_)
        | WsError::WriteBufferFull(_) => {
            ProviderError::transport(format!("WebSocket transport error: {err}"))
        }
        WsError::Http(response) => {
            let status = response.status().as_u16();
            let body = response
                .body()
                .as_deref()
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            ProviderError::http_status(status, &body)
        }
        WsError::Url(_) | WsError::HttpFormat(_) => {
            ProviderError::request(format!("WebSocket request error: {err}"))
        }
        WsError::Capacity(_) | WsError::Protocol(_) | WsError::Utf8(_) | WsError::AttackAttempt => {
            ProviderError::new(
                ProviderErrorKind::Parse,
                format!("WebSocket protocol error: {err}"),
            )
        }
    }
}

fn ingest_frame(
    mapper: &mut ResponsesEventMapper,
    frame: Option<std::result::Result<Message, WsError>>,
) -> Ingest {
    match frame {
        Some(Ok(Message::Text(text))) => {
            let terminal = frame_is_terminal(text.as_str());
            if let Err(err) = mapper.push_json(text.as_str()) {
                return Ingest::Failed(err);
            }
            if terminal {
                Ingest::Completed
            } else {
                Ingest::Continue
            }
        }
        Some(Ok(Message::Close(_))) | None => Ingest::Failed(ProviderError::transport(
            "WebSocket closed before response.completed",
        )),
        Some(Ok(_)) => Ingest::Continue,
        Some(Err(err)) => Ingest::Failed(classify_websocket_error(err)),
    }
}

/// Holds the session guard for the turn. On clean completion it records the new
/// chain state; on any early end (error, close, dropped stream) its `Drop`
/// poisons the socket so the next turn reconnects with full input.
struct TurnState {
    guard: OwnedMutexGuard<SessionInner>,
    mapper: ResponsesEventMapper,
    pending_snapshot: Vec<Value>,
    completed: bool,
}

impl TurnState {
    fn record_success(&mut self) {
        self.guard.last_response_id = self.mapper.last_response_id().map(str::to_owned);
        self.guard.last_input = std::mem::take(&mut self.pending_snapshot);
    }
}

impl Drop for TurnState {
    fn drop(&mut self) {
        if !self.completed {
            self.guard.socket = None;
            self.guard.last_response_id = None;
            self.guard.last_input.clear();
        }
    }
}

/// Streams one turn's events, holding the session guard until the turn ends.
fn turn_event_stream(turn: TurnState) -> impl Stream<Item = ProviderResult<StreamEvent>> + Send {
    futures_util::stream::unfold(Some(turn), |state| async move {
        let mut st = state?;
        loop {
            if let Some(event) = st.mapper.pop() {
                return Some((Ok(event), Some(st)));
            }
            if st.completed {
                return None;
            }
            let frame = {
                let socket = st
                    .guard
                    .socket
                    .as_mut()
                    .expect("socket present during active turn");
                socket.next().await
            };
            match ingest_frame(&mut st.mapper, frame) {
                Ingest::Continue => {}
                Ingest::Completed => {
                    st.record_success();
                    st.completed = true;
                }
                Ingest::Failed(err) => return Some((Err(err), None)),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ContentBlockType;

    fn text(json: &str) -> Message {
        Message::Text(json.to_string().into())
    }

    #[test]
    fn ingest_frame_maps_full_response_like_sse() {
        let mut mapper = ResponsesEventMapper::new("gpt-test".to_string());
        let mut events = Vec::new();

        let frames: [(std::result::Result<Message, WsError>, bool); 4] = [
            (
                Ok(text(
                    r#"{"type":"response.output_item.added","item":{"type":"message"}}"#,
                )),
                false,
            ),
            (
                Ok(text(
                    r#"{"type":"response.output_text.delta","delta":"hi"}"#,
                )),
                false,
            ),
            (
                Ok(text(
                    r#"{"type":"response.output_item.done","item":{"type":"message"}}"#,
                )),
                false,
            ),
            (
                Ok(text(
                    r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed"}}"#,
                )),
                true,
            ),
        ];

        for (frame, expect_completed) in frames {
            let outcome = ingest_frame(&mut mapper, Some(frame));
            assert_eq!(matches!(outcome, Ingest::Completed), expect_completed);
            while let Some(event) = mapper.pop() {
                events.push(event);
            }
        }

        assert!(matches!(
            events[0],
            StreamEvent::ContentBlockStart {
                block_type: ContentBlockType::Text,
                ..
            }
        ));
        assert!(matches!(events[1], StreamEvent::TextDelta { .. }));
        assert!(matches!(
            events[2],
            StreamEvent::ContentBlockCompleted { .. }
        ));
        assert!(matches!(events[3], StreamEvent::MessageDelta { .. }));
        assert!(matches!(events[4], StreamEvent::MessageCompleted));
        assert!(matches!(events[5], StreamEvent::MessageStart { .. }));
        assert_eq!(events.len(), 6);
        assert_eq!(mapper.last_response_id(), Some("resp_1"));
    }

    #[test]
    fn ingest_frame_fails_on_close_or_eof() {
        let mut mapper = ResponsesEventMapper::new("gpt-test".to_string());
        assert!(matches!(
            ingest_frame(&mut mapper, Some(Ok(Message::Close(None)))),
            Ingest::Failed(_)
        ));
        assert!(matches!(ingest_frame(&mut mapper, None), Ingest::Failed(_)));
    }

    #[test]
    fn websocket_request_errors_are_not_retryable() {
        let err = "not a websocket URL".into_client_request().unwrap_err();
        let mapped = classify_websocket_error(err);
        assert_eq!(mapped.kind, ProviderErrorKind::Request);
        assert!(!mapped.is_retryable());
    }

    #[test]
    fn websocket_handshake_auth_errors_are_not_retryable() {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(401)
            .body(Some(br#"{"error":"unauthorized"}"#.to_vec()))
            .unwrap();
        let mapped = classify_websocket_error(WsError::Http(Box::new(response)));
        assert_eq!(mapped.kind, ProviderErrorKind::HttpStatus);
        assert!(!mapped.is_retryable());
    }

    #[test]
    fn websocket_io_errors_are_retryable_transport_failures() {
        let err = WsError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset by peer",
        ));
        let mapped = classify_websocket_error(err);
        assert_eq!(mapped.kind, ProviderErrorKind::Transport);
        assert!(mapped.is_retryable());
    }

    #[test]
    fn plan_send_is_full_on_fresh_session() {
        let new = vec![json!({"a": 1})];
        assert_eq!(plan_send(&new, &[], None), SendPlan::Full);
    }

    #[test]
    fn plan_send_continues_when_new_input_extends_prefix() {
        let last = vec![json!({"a": 1})];
        let new = vec![json!({"a": 1}), json!({"b": 2})];
        assert_eq!(
            plan_send(&new, &last, Some("resp_1")),
            SendPlan::Continuation {
                skip: 1,
                previous_response_id: "resp_1".to_string(),
            }
        );
    }

    #[test]
    fn plan_send_is_full_when_history_diverges() {
        let last = vec![json!({"a": 1})];
        let new = vec![json!({"x": 9}), json!({"b": 2})];
        assert_eq!(plan_send(&new, &last, Some("resp_1")), SendPlan::Full);
    }

    #[test]
    fn plan_send_is_full_without_response_id() {
        let last = vec![json!({"a": 1})];
        let new = vec![json!({"a": 1}), json!({"b": 2})];
        assert_eq!(plan_send(&new, &last, None), SendPlan::Full);
    }

    #[test]
    fn plan_send_is_full_when_no_new_items() {
        let last = vec![json!({"a": 1})];
        let new = vec![json!({"a": 1})];
        assert_eq!(plan_send(&new, &last, Some("resp_1")), SendPlan::Full);
    }

    #[test]
    fn to_ws_url_converts_scheme() {
        assert_eq!(
            to_ws_url("https://api.openai.com/v1", "/responses"),
            "wss://api.openai.com/v1/responses"
        );
        assert_eq!(
            to_ws_url("http://localhost:1234/v1/", "/responses"),
            "ws://localhost:1234/v1/responses"
        );
    }

    #[test]
    fn frame_is_terminal_detects_completion() {
        assert!(frame_is_terminal(r#"{"type":"response.completed"}"#));
        assert!(frame_is_terminal(r#"{"type":"response.done"}"#));
        assert!(!frame_is_terminal(
            r#"{"type":"response.output_text.delta"}"#
        ));
        assert!(!frame_is_terminal("not json"));
    }

    #[test]
    fn response_create_frame_sets_type_and_strips_stream() {
        let config = ResponsesConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            path: "/responses".to_string(),
            model: "gpt-test".to_string(),
            max_output_tokens: None,
            reasoning_effort: None,
            reasoning_summary: None,
            instructions: None,
            text_verbosity: None,
            store: Some(false),
            include: None,
            stream_options: None,
            prompt_cache_key: None,
            parallel_tool_calls: None,
            tool_choice: None,
            truncation: None,
            service_tier: None,
        };
        let input = build_input(&[ChatMessage::user("hello")], None);
        let request = build_request_body_from_input(&config, input, &[], None);

        let frame = response_create_frame(&request).unwrap();
        let value: Value = serde_json::from_str(&frame).unwrap();

        assert_eq!(
            value.get("type").and_then(Value::as_str),
            Some("response.create")
        );
        assert!(value.get("stream").is_none());
        assert_eq!(value.get("model").and_then(Value::as_str), Some("gpt-test"));
    }
}
