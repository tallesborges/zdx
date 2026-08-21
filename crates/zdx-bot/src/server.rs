use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zdx_engine::config::paths::threads_dir;
use zdx_engine::core::thread_persistence::{ThreadEvent, load_thread_events};

const MINIAPP_HTML: &str = include_str!("miniapp.html");
const INIT_DATA_MAX_AGE_SECS: u64 = 60 * 60;
const INIT_DATA_FUTURE_SKEW_SECS: u64 = 30;

type HmacSha256 = Hmac<Sha256>;
type ApiError = (StatusCode, &'static str);

#[derive(Serialize)]
pub struct DialogueLine {
    pub id: usize,
    pub time: String,
    pub speaker: String,
    pub speaker_key: String,
    pub text: String,
}

#[derive(Serialize)]
pub struct ThreadResponse {
    pub id: String,
    pub title: String,
    pub total_lines: usize,
    pub dialogue: Vec<DialogueLine>,
}

#[derive(Serialize)]
pub struct ThreadListItem {
    pub id: String,
    pub raw_id: String,
    pub title: String,
}

#[derive(Serialize)]
pub struct ThreadListResponse {
    pub count: usize,
    pub threads: Vec<ThreadListItem>,
}

pub(crate) struct ServerState {
    bot_token: String,
    allowlist_user_ids: HashSet<i64>,
}

#[derive(Deserialize)]
struct InitDataUser {
    id: i64,
}

/// Creates the router for the embedded web server.
pub(crate) fn create_router(state: Arc<ServerState>) -> Router {
    let api = Router::new()
        .route("/threads", get(list_threads))
        .route("/threads/{id}", get(get_thread))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorize_api,
        ));

    Router::new()
        .route("/threads", get(serve_miniapp))
        .nest("/api", api)
        .with_state(state)
}

async fn serve_miniapp() -> impl IntoResponse {
    Html(MINIAPP_HTML)
}

async fn authorize_api(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    authorize(&headers, &state)?;
    Ok(next.run(request).await)
}

async fn list_threads() -> Result<Json<ThreadListResponse>, ApiError> {
    let threads_dir = threads_dir();
    let mut items = Vec::new();

    if let Ok(entries) = std::fs::read_dir(threads_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                let title = if let Some(topic) = stem.strip_prefix("telegram-") {
                    if let Some((_, topic_id)) = topic.split_once("-topic-") {
                        format!("Telegram Topic #{topic_id}")
                    } else {
                        stem.to_string()
                    }
                } else if stem.len() > 18 {
                    stem[..18].to_string()
                } else {
                    stem.to_string()
                };

                items.push(ThreadListItem {
                    id: urlencoding_encode(stem),
                    raw_id: stem.to_string(),
                    title,
                });
            }
        }
    }

    let count = items.len();
    Ok(Json(ThreadListResponse {
        count,
        threads: items,
    }))
}

async fn get_thread(Path(id): Path<String>) -> Result<Json<ThreadResponse>, ApiError> {
    let target_id = if id == "active" {
        find_latest_telegram_thread().unwrap_or(id)
    } else {
        id
    };

    let events = load_thread_events(&target_id).map_err(|error| {
        tracing::warn!(thread_id = target_id, %error, "Failed to load Mini App thread");
        (StatusCode::NOT_FOUND, "Thread not found")
    })?;

    let mut title = "Thread Transcript".to_string();
    let mut dialogue = Vec::new();
    let mut line_id = 1;

    for event in events {
        match event {
            ThreadEvent::Meta { title: Some(t), .. } => {
                title = t;
            }
            ThreadEvent::Message { role, text, ts, .. } => {
                let speaker = if role == "user" { "You" } else { "Z" };
                let speaker_key = if role == "user" {
                    "speaker-user"
                } else {
                    "speaker-assistant"
                };

                let time = if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&ts) {
                    dt.format("%I:%M %p").to_string()
                } else {
                    "--:--".to_string()
                };

                let clean_text = clean_message_text(&text);
                if !clean_text.is_empty() {
                    dialogue.push(DialogueLine {
                        id: line_id,
                        time,
                        speaker: speaker.to_string(),
                        speaker_key: speaker_key.to_string(),
                        text: clean_text,
                    });
                    line_id += 1;
                }
            }
            _ => {}
        }
    }

    Ok(Json(ThreadResponse {
        id: target_id,
        title,
        total_lines: dialogue.len(),
        dialogue,
    }))
}

#[derive(Debug, PartialEq, Eq)]
enum InitDataError {
    Missing,
    Malformed,
    InvalidSignature,
    Expired,
    Forbidden,
}

fn authorize(headers: &HeaderMap, state: &ServerState) -> Result<i64, ApiError> {
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "Telegram authorization required"))?;
    let (scheme, init_data) = authorization
        .split_once(' ')
        .ok_or((StatusCode::UNAUTHORIZED, "Invalid Telegram authorization"))?;
    if !scheme.eq_ignore_ascii_case("tma") || init_data.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, "Invalid Telegram authorization"));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| (StatusCode::INTERNAL_SERVER_ERROR, "System clock error"))?
        .as_secs();
    validate_init_data(init_data, &state.bot_token, &state.allowlist_user_ids, now).map_err(
        |error| match error {
            InitDataError::Forbidden => (StatusCode::FORBIDDEN, "Telegram user is not allowed"),
            InitDataError::Missing => (StatusCode::UNAUTHORIZED, "Telegram authorization required"),
            InitDataError::Malformed | InitDataError::InvalidSignature | InitDataError::Expired => {
                (
                    StatusCode::UNAUTHORIZED,
                    "Invalid or expired Telegram authorization",
                )
            }
        },
    )
}

fn validate_init_data(
    init_data: &str,
    bot_token: &str,
    allowlist_user_ids: &HashSet<i64>,
    now: u64,
) -> Result<i64, InitDataError> {
    if init_data.is_empty() {
        return Err(InitDataError::Missing);
    }

    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(init_data.as_bytes())
        .into_owned()
        .collect();
    let mut hash = None;
    let mut auth_date = None;
    let mut user = None;
    for (key, value) in &pairs {
        match key.as_str() {
            "hash" if hash.replace(value.as_str()).is_some() => {
                return Err(InitDataError::Malformed);
            }
            "auth_date" if auth_date.replace(value.as_str()).is_some() => {
                return Err(InitDataError::Malformed);
            }
            "user" if user.replace(value.as_str()).is_some() => {
                return Err(InitDataError::Malformed);
            }
            _ => {}
        }
    }

    let received_hash = decode_hash(hash.ok_or(InitDataError::Malformed)?)?;
    let auth_date = auth_date
        .ok_or(InitDataError::Malformed)?
        .parse::<u64>()
        .map_err(|_error| InitDataError::Malformed)?;
    if auth_date > now.saturating_add(INIT_DATA_FUTURE_SKEW_SECS)
        || now.saturating_sub(auth_date) > INIT_DATA_MAX_AGE_SECS
    {
        return Err(InitDataError::Expired);
    }

    let mut checked_pairs: Vec<&(String, String)> =
        pairs.iter().filter(|(key, _)| key != "hash").collect();
    checked_pairs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let data_check_string = checked_pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut secret_mac =
        HmacSha256::new_from_slice(b"WebAppData").expect("HMAC accepts keys of any length");
    secret_mac.update(bot_token.as_bytes());
    let secret_key = secret_mac.finalize().into_bytes();

    let mut data_mac =
        HmacSha256::new_from_slice(&secret_key).expect("HMAC accepts keys of any length");
    data_mac.update(data_check_string.as_bytes());
    data_mac
        .verify_slice(&received_hash)
        .map_err(|_error| InitDataError::InvalidSignature)?;

    let user: InitDataUser = serde_json::from_str(user.ok_or(InitDataError::Malformed)?)
        .map_err(|_error| InitDataError::Malformed)?;
    if !allowlist_user_ids.contains(&user.id) {
        return Err(InitDataError::Forbidden);
    }

    Ok(user.id)
}

fn decode_hash(hash: &str) -> Result<[u8; 32], InitDataError> {
    if hash.len() != 64 {
        return Err(InitDataError::Malformed);
    }

    let mut bytes = [0_u8; 32];
    for (index, pair) in hash.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_digit(pair[0]).ok_or(InitDataError::Malformed)?;
        let low = decode_hex_digit(pair[1]).ok_or(InitDataError::Malformed)?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

fn decode_hex_digit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

fn clean_message_text(text: &str) -> String {
    let mut clean = text.to_string();
    if let Some(start) = clean.find("<followups>")
        && let Some(end) = clean.find("</followups>")
    {
        clean.replace_range(start..end + "</followups>".len(), "");
    }
    if let Some(start) = clean.find("<medias>")
        && let Some(end) = clean.find("</medias>")
    {
        clean.replace_range(start..end + "</medias>".len(), "");
    }
    if let Some(start) = clean.find("<media>")
        && let Some(end) = clean.find("</media>")
    {
        clean.replace_range(start..end + "</media>".len(), "");
    }
    clean.trim().to_string()
}

fn find_latest_telegram_thread() -> Option<String> {
    let threads_dir = threads_dir();
    let mut latest_file: Option<(String, std::time::SystemTime)> = None;

    if let Ok(entries) = std::fs::read_dir(threads_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.starts_with("telegram-")
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
                && let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
            {
                let stem = path.file_stem()?.to_str()?.to_string();
                if latest_file.as_ref().is_none_or(|(_, m)| modified > *m) {
                    latest_file = Some((stem, modified));
                }
            }
        }
    }

    latest_file.map(|(name, _)| name)
}

fn urlencoding_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Spawns the embedded server on a background tokio task.
pub(crate) fn spawn_server(bot_token: String, allowlist_user_ids: HashSet<i64>, port: u16) {
    let state = Arc::new(ServerState {
        bot_token,
        allowlist_user_ids,
    });
    let app = create_router(state);

    tokio::spawn(async move {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        tracing::info!("ZDX Mini App server listening on http://{}", addr);

        if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
            let _ = axum::serve(listener, app).await;
        } else {
            tracing::error!("Failed to bind ZDX Mini App server to port {}", port);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_BOT_TOKEN: &str = "5768337691:AAH5YkoiEuPk8-FZa32hStHTqXiLPtAEhx8";
    const SAMPLE_AUTH_DATE: u64 = 1_662_771_648;
    const SAMPLE_INIT_DATA: &str = concat!(
        "query_id=AAHdF6IQAAAAAN0XohDhrOrc&",
        "user=%7B%22id%22%3A279058397%2C%22first_name%22%3A%22Vladislav%22%2C",
        "%22last_name%22%3A%22Kibenko%22%2C%22username%22%3A%22vdkfrost%22%2C",
        "%22language_code%22%3A%22ru%22%2C%22is_premium%22%3Atrue%7D&",
        "auth_date=1662771648&",
        "hash=c501b71e775f74ce10e377dea85a7ea24ecd640b223ea86dfe453e0eaed2e2b2"
    );

    fn sample_allowlist() -> HashSet<i64> {
        HashSet::from([279_058_397])
    }

    #[tokio::test]
    async fn protects_every_api_route_but_not_the_miniapp_page() {
        let app = create_router(Arc::new(ServerState {
            bot_token: SAMPLE_BOT_TOKEN.to_string(),
            allowlist_user_ids: sample_allowlist(),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();

        for path in ["/api/threads", "/api/threads/active"] {
            let response = client
                .get(format!("http://{addr}{path}"))
                .send()
                .await
                .expect("request protected API route");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let response = client
            .get(format!("http://{addr}/threads"))
            .send()
            .await
            .expect("request public Mini App page");
        assert_eq!(response.status(), StatusCode::OK);

        server.abort();
    }

    #[test]
    fn validates_official_telegram_sample() {
        assert_eq!(
            validate_init_data(
                SAMPLE_INIT_DATA,
                SAMPLE_BOT_TOKEN,
                &sample_allowlist(),
                SAMPLE_AUTH_DATE + 60,
            ),
            Ok(279_058_397)
        );
    }

    #[test]
    fn rejects_tampered_init_data() {
        let tampered = SAMPLE_INIT_DATA.replace("Vladislav", "Mallory");
        assert_eq!(
            validate_init_data(
                &tampered,
                SAMPLE_BOT_TOKEN,
                &sample_allowlist(),
                SAMPLE_AUTH_DATE + 60,
            ),
            Err(InitDataError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_expired_init_data() {
        assert_eq!(
            validate_init_data(
                SAMPLE_INIT_DATA,
                SAMPLE_BOT_TOKEN,
                &sample_allowlist(),
                SAMPLE_AUTH_DATE + INIT_DATA_MAX_AGE_SECS + 1,
            ),
            Err(InitDataError::Expired)
        );
    }

    #[test]
    fn rejects_users_outside_allowlist() {
        assert_eq!(
            validate_init_data(
                SAMPLE_INIT_DATA,
                SAMPLE_BOT_TOKEN,
                &HashSet::new(),
                SAMPLE_AUTH_DATE + 60,
            ),
            Err(InitDataError::Forbidden)
        );
    }
}
