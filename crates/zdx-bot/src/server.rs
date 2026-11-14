use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use hmac::{Hmac, Mac};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tower_http::compression::CompressionLayer;
use zdx_engine::core::artifact_feedback::{
    self, FeedbackError, FeedbackSubmission, SaveOutcome, SavedFeedback,
};
use zdx_engine::core::thread_trajectory::ThreadTrajectoryReport;
use zdx_engine::orchestration::OrchestratorClient;
use zdx_engine::orchestration::protocol::ErrorCode;
use zdx_engine::orchestration::view_bodies::{
    ArtifactVersionResponse, ArtifactsResponse, EngineViews, GitDiffResponse, GitResponse,
    GitScopeResponse, MonitorResponse, ThreadListKind, ThreadListResponse, ThreadResponse,
    UsageResponse, WorkersResponse, WorkspacesResponse, resolve_alias_id, valid_thread_id,
};
use zdx_engine::orchestration::views::ViewError;

use crate::artifact_feedback::{
    FeedbackDelivery, FeedbackRoute, FeedbackSink, resolve_destination,
};
use crate::delivery_tracker::ActiveDeliveries;
use crate::types::LocalAttachment;

const INIT_DATA_MAX_AGE_SECS: u64 = 60 * 60;
const INIT_DATA_FUTURE_SKEW_SECS: u64 = 30;
/// An API request slower than this is logged at `info` so it reaches the
/// default log file: this is roughly where a request becomes a visible wait.
const SLOW_REQUEST: Duration = Duration::from_millis(750);

type HmacSha256 = Hmac<Sha256>;
type ApiError = (StatusCode, &'static str);
/// A refused read route: the status and the engine's exact text.
type ViewFailure = (StatusCode, String);

/// Query for the artifact download endpoint: the artifact's `path` as listed.
#[derive(Deserialize)]
struct ArtifactFileQuery {
    path: String,
}

#[derive(Deserialize)]
struct ThreadQuery {
    /// Return only activity at or after this cursor. Live polling sends the
    /// `cursor` from its previous response; a first load omits it.
    after: Option<usize>,
}

#[derive(Deserialize, Default)]
struct ThreadListQuery {
    #[serde(default)]
    kind: ThreadListKind,
}

#[derive(Deserialize)]
struct GitQuery {
    thread_id: Option<String>,
}

#[derive(Deserialize)]
struct GitDiffQuery {
    thread_id: Option<String>,
    path: String,
    kind: String,
    /// Commit hash for `kind=commit`. Ignored by the other kinds.
    commit: Option<String>,
}

#[derive(Deserialize)]
struct GitScopeQuery {
    thread_id: Option<String>,
    /// `all` or a commit hash.
    scope: String,
}

#[derive(Deserialize)]
struct UsageQuery {
    days: Option<u32>,
}

pub(crate) struct ServerState {
    bot_token: String,
    allowlist_user_ids: HashSet<i64>,
    /// Every read route's body (`zdx_engine::orchestration::view_bodies`):
    /// the same functions the service's views and `zdx views` call, with
    /// their caches. Live worker state comes from the orchestrator service.
    views: EngineViews,
    /// Reply deliveries running in this process (shared with the reply path).
    deliveries: ActiveDeliveries,
    /// Where artifact feedback goes. `None` refuses feedback submissions.
    feedback: Option<Arc<dyn FeedbackSink>>,
    feedback_in_flight: FeedbackInFlight,
    feedback_permits: Arc<tokio::sync::Semaphore>,
}

impl ServerState {
    fn new(
        bot_token: String,
        allowlist_user_ids: HashSet<i64>,
        root: PathBuf,
        workers: OrchestratorClient,
    ) -> Self {
        Self {
            bot_token,
            allowlist_user_ids,
            views: EngineViews::with_client(root, workers),
            deliveries: ActiveDeliveries::default(),
            feedback: None,
            feedback_in_flight: FeedbackInFlight::default(),
            feedback_permits: Arc::new(tokio::sync::Semaphore::new(FEEDBACK_CONCURRENCY)),
        }
    }

    fn with_deliveries(mut self, deliveries: ActiveDeliveries) -> Self {
        self.deliveries = deliveries;
        self
    }

    fn with_feedback(mut self, feedback: Arc<dyn FeedbackSink>) -> Self {
        self.feedback = Some(feedback);
        self
    }
}

#[derive(Deserialize)]
struct InitDataUser {
    id: i64,
}

/// Creates the router for the embedded web server.
pub(crate) fn create_router(state: Arc<ServerState>) -> Router {
    let api = Router::new()
        .route("/threads", get(get_threads))
        .route("/threads/{id}/trajectory", get(get_thread_trajectory))
        .route("/threads/{id}/artifacts", get(get_thread_artifacts))
        .route(
            "/threads/{id}/artifacts/file",
            get(get_thread_artifact_file),
        )
        .route(
            "/threads/{id}/artifacts/version",
            get(get_thread_artifact_version),
        )
        .route(
            "/threads/{id}/artifacts/image",
            get(get_thread_artifact_image),
        )
        .route(
            "/threads/{id}/artifacts/feedback",
            post(post_artifact_feedback)
                .layer(DefaultBodyLimit::max(
                    artifact_feedback::MAX_SUBMISSION_BYTES,
                ))
                .layer(middleware::from_fn_with_state(
                    Arc::clone(&state),
                    limit_feedback,
                )),
        )
        .route("/threads/{id}", get(get_thread))
        .route("/threads/{id}/workers", get(get_thread_workers))
        .route("/monitor", get(get_monitor))
        .route("/workspaces", get(get_workspaces))
        .route("/usage", get(get_usage))
        .route("/git", get(get_git))
        .route("/git/scope", get(get_git_scope))
        .route("/git/diff", get(get_git_diff))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorize_api,
        ))
        .layer(middleware::from_fn(log_api_timing));

    Router::new()
        .route("/app", get(serve_app))
        .route("/app/{*path}", get(serve_asset))
        .route("/threads", get(serve_app))
        .route("/monitor", get(serve_app))
        .nest("/api", api)
        // Shell HTML and thread transcripts are highly compressible text fetched
        // over a tunnel from a phone, so compress whatever the client accepts.
        // The default predicate already skips tiny bodies and images.
        .layer(CompressionLayer::new())
        .with_state(state)
}

/// The built Svelte Mini App (`apps/web/dist`), embedded so the binary stays
/// self-contained. Built by `just web-build`; a missing folder is a compile
/// error rather than a silently empty UI.
#[derive(RustEmbed)]
#[folder = "../../apps/web/dist"]
struct Assets;

/// Serves the SPA entry point. Never cached, so a new build's content-hashed
/// asset names are picked up on the next open.
async fn serve_app() -> Response {
    match Assets::get("index.html") {
        Some(file) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            file.data.into_owned(),
        )
            .into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Mini App assets are missing; run `just web-build`",
        )
            .into_response(),
    }
}

/// Serves a content-hashed asset. Vite renames these on every build, so they are
/// safe to cache permanently — the only caching layer Telegram gives us, since
/// service workers do not work in the iOS `WebView`.
async fn serve_asset(Path(path): Path<String>) -> Response {
    match Assets::get(&path) {
        Some(file) => {
            let mime = file.metadata.mimetype().to_owned();
            (
                [
                    (header::CONTENT_TYPE, mime.as_str()),
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                ],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

/// The Telegram user `authorize_api` verified, for handlers that act on it.
/// Read from request extensions so a long upload is not re-checked against
/// `initData` that expired while the body was arriving.
#[derive(Clone, Copy)]
struct AuthorizedUser(i64);

async fn authorize_api(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let user_id = authorize(&headers, &state)?;
    request.extensions_mut().insert(AuthorizedUser(user_id));
    Ok(next.run(request).await)
}

/// Times every `/api/*` request, including authorization.
///
/// The Mini App renders its shell from embedded assets, so a visible "loading"
/// is always one of these requests still in flight. Without this line a stall
/// reported from a phone cannot be attributed to a route at all. Slow requests
/// are logged at `info` so they land in the default log file; the rest are
/// `debug` because an open thread view polls every few seconds.
async fn log_api_timing(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    let response = next.run(request).await;
    let elapsed = started.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let status = response.status().as_u16();
    if elapsed >= SLOW_REQUEST {
        tracing::info!(%method, path, status, elapsed_ms, "Slow Mini App API request");
    } else {
        tracing::debug!(%method, path, status, elapsed_ms, "Mini App API request");
    }
    response
}

/// The HTTP status of a refused view body. Each code comes from one status
/// the route answered before the bodies moved to the engine.
fn status_of(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::Invalid => StatusCode::BAD_REQUEST,
        ErrorCode::NotFound | ErrorCode::ArtifactNotFound => StatusCode::NOT_FOUND,
        ErrorCode::UnsupportedArtifact => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorCode::Busy => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn view_failure(error: ViewError) -> ViewFailure {
    (status_of(error.code), error.message)
}

/// Lists the most recently active top-level threads for the Mini App browser,
/// or only orchestrator workers with `?kind=workers`.
async fn get_threads(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<ThreadListQuery>,
) -> Result<Json<ThreadListResponse>, ViewFailure> {
    state
        .views
        .threads(query.kind)
        .await
        .map(Json)
        .map_err(view_failure)
}

/// One thread's transcript. `delivering` is sampled from this process's reply
/// deliveries around the event read.
async fn get_thread(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Query(query): Query<ThreadQuery>,
) -> Result<Json<ThreadResponse>, ViewFailure> {
    let deliveries = state.deliveries.clone();
    state
        .views
        .thread(id, query.after, move |source_id, load| {
            deliveries.observe(source_id, load)
        })
        .await
        .map(|(thread, _source_id)| Json(thread))
        .map_err(view_failure)
}

async fn get_thread_trajectory(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<ThreadTrajectoryReport>, ViewFailure> {
    state
        .views
        .trajectory(id)
        .await
        .map(Json)
        .map_err(view_failure)
}

async fn get_thread_workers(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<WorkersResponse>, ViewFailure> {
    state
        .views
        .thread_workers(id)
        .await
        .map(Json)
        .map_err(view_failure)
}

async fn get_thread_artifacts(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<ArtifactsResponse>, ViewFailure> {
    state
        .views
        .artifacts(id)
        .await
        .map(Json)
        .map_err(view_failure)
}

async fn get_thread_artifact_file(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Query(query): Query<ArtifactFileQuery>,
) -> Result<Response, ViewFailure> {
    let (bytes, file) = state
        .views
        .read_artifact(id, query.path)
        .await
        .map_err(view_failure)?;
    let body = axum::body::Body::from(bytes);
    Response::builder()
        .header(header::CONTENT_TYPE, file.mime)
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", file.filename),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(body)
        .map_err(|_build| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Artifact read failed".to_string(),
            )
        })
}

async fn get_thread_artifact_version(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Query(query): Query<ArtifactFileQuery>,
) -> Result<Json<ArtifactVersionResponse>, ViewFailure> {
    state
        .views
        .artifact_version(id, query.path)
        .await
        .map(Json)
        .map_err(view_failure)
}

/// The canonical copy of an image artifact (`zdx_engine::core::
/// artifact_feedback::canonical`): small, upright, made once and cached. The
/// Mini App previews and annotates exactly these bytes and sends back their
/// hash, so marks refer to the pixels the agent gets. The original is served
/// unchanged by `/artifacts/file`. Identity travels in headers:
/// `X-Zdx-Source-Sha256` (the artifact version), `X-Zdx-Canonical-Sha256`,
/// `X-Zdx-Canonical-Width` and `-Height`, and the original's upright size in
/// `X-Zdx-Source-Width` and `-Height` (so the app can say it shows an
/// optimized copy). `422` with a reason when the image
/// cannot be annotated (format or limits); the app then shows the original.
async fn get_thread_artifact_image(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Query(query): Query<ArtifactFileQuery>,
) -> Response {
    let image = match state.views.canonical_image(id, query.path).await {
        Ok(image) => image,
        Err(error) => return view_failure(error).into_response(),
    };
    let meta = image.meta;
    Response::builder()
        .header(header::CONTENT_TYPE, meta.mime.as_str())
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("x-zdx-source-sha256", meta.source_sha256.as_str())
        .header("x-zdx-canonical-sha256", meta.sha256.as_str())
        .header("x-zdx-canonical-width", meta.width.to_string())
        .header("x-zdx-canonical-height", meta.height.to_string())
        .header("x-zdx-source-width", meta.source_upright().0.to_string())
        .header("x-zdx-source-height", meta.source_upright().1.to_string())
        .body(axum::body::Body::from(image.bytes))
        .unwrap_or_else(|_build| {
            (StatusCode::INTERNAL_SERVER_ERROR, "Image preview failed").into_response()
        })
}

#[derive(Serialize)]
struct FeedbackResponse {
    /// `queued`: on the chat's queue. `duplicate`: this submission was already
    /// queued, so nothing was sent again. The queue is in memory: neither means
    /// a model has read it, and a restart before the turn starts loses it.
    status: &'static str,
    annotations: usize,
    /// Sent although the file changed after annotation (`allow_stale`).
    stale: bool,
}

type FeedbackFailure = (StatusCode, String);

/// Submissions being saved or delivered right now, keyed by effective thread
/// and `submission_id`. One bot process serves the API, so this is the only
/// place two requests for the same submission can meet.
#[derive(Default)]
struct FeedbackInFlight(std::sync::Mutex<HashSet<String>>);

struct InFlightGuard {
    state: Arc<ServerState>,
    key: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Ok(mut keys) = self.state.feedback_in_flight.0.lock() {
            keys.remove(&self.key);
        }
    }
}

/// Feedback submissions decoded at once. A submission can be tens of MiB of
/// base64, so the cap holds before the body is read.
const FEEDBACK_CONCURRENCY: usize = 2;

/// The upload permit, handed from [`limit_feedback`] to the handler. The
/// handler moves it into the blocking save (decode, render, write), so a
/// client that disconnects mid-save does not free the slot while that work
/// still runs. Until then the request owns it, so a body that never finishes
/// arriving frees it when the request is dropped.
#[derive(Clone)]
struct FeedbackPermit(Arc<std::sync::Mutex<Option<tokio::sync::OwnedSemaphorePermit>>>);

impl FeedbackPermit {
    fn take(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.0.lock().ok().and_then(|mut slot| slot.take())
    }
}

async fn limit_feedback(
    State(state): State<Arc<ServerState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Ok(permit) = Arc::clone(&state.feedback_permits).try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Another feedback upload is in progress. Try again in a moment.",
        )
            .into_response();
    };
    request
        .extensions_mut()
        .insert(FeedbackPermit(Arc::new(std::sync::Mutex::new(Some(
            permit,
        )))));
    next.run(request).await
}

/// Accepts annotations on a thread artifact and queues them as a turn in the
/// chat that owns the thread.
///
/// Ordering is what keeps this safe: the route and the sender are checked
/// before anything is saved, the engine validates the payload and the artifact
/// version before writing, and a bundle counts as sent only once the sink
/// marked it queued, so a retry after a lost response is answered `duplicate`
/// while a retry after a failure queues it. Delivery runs on its own task: a
/// client that disconnects mid-request cannot cancel it halfway through.
async fn post_artifact_feedback(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    axum::Extension(AuthorizedUser(user_id)): axum::Extension<AuthorizedUser>,
    axum::Extension(permit): axum::Extension<FeedbackPermit>,
    Json(submission): Json<FeedbackSubmission>,
) -> Response {
    let Some(sink) = state.feedback.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Feedback delivery is not available on this server",
        )
            .into_response();
    };

    let task_state = Arc::clone(&state);
    let permit = permit.take();
    let saved = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        save_feedback_for_thread(&task_state, user_id, &id, &submission)
    })
    .await;
    let (route, saved, guard) = match saved {
        Ok(Ok(Some(saved))) => saved,
        Ok(Ok(None)) => {
            return (
                StatusCode::OK,
                Json(FeedbackResponse {
                    status: "duplicate",
                    annotations: 0,
                    stale: false,
                }),
            )
                .into_response();
        }
        Ok(Err(failure)) => return failure.into_response(),
        Err(error) => {
            tracing::warn!(%error, "Mini App feedback task failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Saving feedback failed").into_response();
        }
    };

    let annotations = saved.annotation_count;
    let sent_stale = saved.stale;
    let delivered = tokio::spawn(async move {
        let _guard = guard;
        let delivery = FeedbackDelivery {
            route,
            user_id,
            attachments: feedback_attachments(&saved),
            prompt: saved.prompt,
            artifact_name: saved.artifact_name,
            annotation_count: saved.annotation_count,
            bundle_dir: saved.dir,
        };
        sink.deliver(delivery).await
    })
    .await;
    match delivered {
        Ok(Ok(())) => (
            StatusCode::ACCEPTED,
            Json(FeedbackResponse {
                status: "queued",
                annotations,
                stale: sent_stale,
            }),
        )
            .into_response(),
        Ok(Err(error)) => {
            tracing::error!(error = %format!("{error:#}"), "Failed to queue artifact feedback");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Feedback could not be queued. Try again.",
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "Artifact feedback delivery task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Feedback could not be queued. Try again.",
            )
                .into_response()
        }
    }
}

type SavedForDelivery = (FeedbackRoute, SavedFeedback, InFlightGuard);

/// Blocking half of [`post_artifact_feedback`]: route, permission, then save.
/// `Ok(None)` is a duplicate submission.
fn save_feedback_for_thread(
    state: &Arc<ServerState>,
    user_id: i64,
    id: &str,
    submission: &FeedbackSubmission,
) -> Result<Option<SavedForDelivery>, FeedbackFailure> {
    // `active` moves to whichever thread was used last, so a write through it
    // could land in a different conversation than the one previewed.
    if id == "active" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Reopen the thread from its chat to send feedback.".to_string(),
        ));
    }
    if !valid_thread_id(id) {
        return Err((StatusCode::BAD_REQUEST, "Invalid thread ID".to_string()));
    }
    let Some(sink) = state.feedback.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Feedback delivery is not available on this server".to_string(),
        ));
    };
    // Routing uses the requested id (the chat topic); the artifact belongs to
    // the effective thread behind a resumed or mirror alias.
    let route =
        resolve_destination(id).map_err(|message| (StatusCode::CONFLICT, message.to_string()))?;
    if !sink.allows(user_id, route) {
        return Err((
            StatusCode::FORBIDDEN,
            "You cannot send feedback into this chat".to_string(),
        ));
    }
    let effective = resolve_alias_id(id);
    let key = format!("{effective}/{}", submission.submission_id);
    let fresh = state
        .feedback_in_flight
        .0
        .lock()
        .is_ok_and(|mut keys| keys.insert(key.clone()));
    if !fresh {
        return Err((
            StatusCode::CONFLICT,
            "This feedback is still being sent. Try again in a moment.".to_string(),
        ));
    }
    let guard = InFlightGuard {
        state: Arc::clone(state),
        key,
    };
    // Part of the retry identity: the same id from another user or for
    // another destination is a conflict, not a duplicate.
    let binding = format!("user={user_id};dest={id}");
    match artifact_feedback::save_feedback(&effective, &binding, submission) {
        Ok(SaveOutcome::Saved(saved)) => Ok(Some((route, saved, guard))),
        Ok(SaveOutcome::Duplicate) => Ok(None),
        Err(error) => Err(feedback_failure(&effective, &error)),
    }
}

fn feedback_failure(thread_id: &str, error: &FeedbackError) -> FeedbackFailure {
    let status = match error {
        FeedbackError::Invalid(_) => StatusCode::BAD_REQUEST,
        FeedbackError::ArtifactNotFound => StatusCode::NOT_FOUND,
        FeedbackError::UnsupportedArtifact(_) => StatusCode::UNPROCESSABLE_ENTITY,
        // Distinct from the 409s so the client can offer "send anyway".
        FeedbackError::VersionMismatch => StatusCode::PRECONDITION_FAILED,
        FeedbackError::IdConflict => StatusCode::CONFLICT,
        FeedbackError::Busy => StatusCode::SERVICE_UNAVAILABLE,
        FeedbackError::Storage(err) => {
            tracing::error!(thread_id, error = %format!("{err:#}"), "Failed to save artifact feedback");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Saving feedback failed".to_string(),
            );
        }
    };
    (status, error.to_string())
}

pub(crate) fn feedback_attachments(saved: &SavedFeedback) -> Vec<LocalAttachment> {
    saved
        .audios
        .iter()
        .map(|audio| LocalAttachment::Audio {
            path: audio.path.clone(),
            mime: audio.mime.clone(),
            label: audio.label.clone(),
        })
        .chain(saved.images.iter().map(|image| LocalAttachment::Image {
            path: image.path.clone(),
            label: image.label.clone(),
        }))
        .collect()
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

async fn get_git(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<GitQuery>,
) -> Result<Json<GitResponse>, ViewFailure> {
    state
        .views
        .git_status(query.thread_id)
        .await
        .map(Json)
        .map_err(view_failure)
}

async fn get_git_scope(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<GitScopeQuery>,
) -> Result<Json<GitScopeResponse>, ViewFailure> {
    state
        .views
        .git_scope(query.thread_id, &query.scope)
        .await
        .map(Json)
        .map_err(view_failure)
}

async fn get_git_diff(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<GitDiffQuery>,
) -> Result<Json<GitDiffResponse>, ViewFailure> {
    state
        .views
        .git_diff(
            query.thread_id,
            query.path,
            &query.kind,
            query.commit.as_deref(),
        )
        .await
        .map(Json)
        .map_err(view_failure)
}

/// Read-only model settings of every workspace, a thin adapter over
/// `zdx_engine::workspace_config`.
async fn get_workspaces(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<WorkspacesResponse>, ViewFailure> {
    state
        .views
        .workspaces()
        .await
        .map(Json)
        .map_err(view_failure)
}

/// Recorded usage for the last `?days=` days, a thin adapter over
/// `zdx_engine::core::usage_report` (cached per window by the engine).
async fn get_usage(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<UsageQuery>,
) -> Result<Json<UsageResponse>, ViewFailure> {
    state
        .views
        .usage(query.days)
        .await
        .map(Json)
        .map_err(view_failure)
}

/// The dashboard: cached snapshot, fresh active agents, last known quotas.
async fn get_monitor(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<MonitorResponse>, ViewFailure> {
    state.views.monitor().await.map(Json).map_err(view_failure)
}

/// Spawns the embedded server on a background tokio task.
pub(crate) fn spawn_server(
    bot_token: String,
    allowlist_user_ids: HashSet<i64>,
    port: u16,
    root: PathBuf,
    workers: OrchestratorClient,
    deliveries: ActiveDeliveries,
    feedback: Arc<dyn FeedbackSink>,
) {
    let state = Arc::new(
        ServerState::new(bot_token, allowlist_user_ids, root, workers)
            .with_deliveries(deliveries)
            .with_feedback(feedback),
    );
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
    use std::path::Path as FilePath;

    use serde_json::Value;
    use zdx_engine::config::paths;
    use zdx_engine::core::thread_persistence::{self, ThreadEvent};

    use super::*;

    /// A client of a service that is not running: no live workers.
    fn test_workers() -> OrchestratorClient {
        OrchestratorClient::at(
            std::env::temp_dir().join(format!("zdx-test-{}.sock", uuid::Uuid::new_v4())),
        )
    }

    const SAMPLE_BOT_TOKEN: &str = "1234567890:TEST-BOT-TOKEN-FOR-UNIT-TESTS-ONLY";
    const SAMPLE_AUTH_DATE: u64 = 1_662_771_648;
    const SAMPLE_INIT_DATA: &str = concat!(
        "query_id=AAHdF6IQAAAAAN0XohDhrOrc&",
        "user=%7B%22id%22%3A279058397%2C%22first_name%22%3A%22Vladislav%22%2C",
        "%22last_name%22%3A%22Kibenko%22%2C%22username%22%3A%22vdkfrost%22%2C",
        "%22language_code%22%3A%22ru%22%2C%22is_premium%22%3Atrue%7D&",
        "auth_date=1662771648&",
        "hash=c4d45b6d00cd75400ade8589bba0ac122375ae8594ecc3495de5fb63c3697c17"
    );

    fn sample_allowlist() -> HashSet<i64> {
        HashSet::from([279_058_397])
    }

    /// Builds `initData` the middleware accepts right now, so a route can be
    /// exercised end to end instead of only its 401 path.
    fn fresh_init_data(user_id: i64) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs()
            .to_string();
        let user = format!("{{\"id\":{user_id}}}");
        let data_check_string = format!("auth_date={now}\nuser={user}");

        let mut secret_mac = HmacSha256::new_from_slice(b"WebAppData").expect("hmac key");
        secret_mac.update(SAMPLE_BOT_TOKEN.as_bytes());
        let secret_key = secret_mac.finalize().into_bytes();
        let mut data_mac = HmacSha256::new_from_slice(&secret_key).expect("hmac key");
        data_mac.update(data_check_string.as_bytes());
        let hash = data_mac
            .finalize()
            .into_bytes()
            .iter()
            .fold(String::new(), |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            });

        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("auth_date", &now)
            .append_pair("user", &user)
            .append_pair("hash", &hash)
            .finish()
    }

    /// Pulls the first content-hashed script URL out of the served shell.
    fn first_script_asset(html: &str) -> String {
        html.split(['"', '\''])
            .find(|s| {
                s.starts_with("/app/assets/")
                    && FilePath::new(s)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("js"))
            })
            .expect("shell references a hashed script")
            .to_string()
    }

    #[tokio::test]
    async fn protects_every_api_route_but_not_the_miniapp_page() {
        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            test_workers(),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();

        for path in [
            "/api/threads",
            "/api/threads/active",
            "/api/threads/active/trajectory",
            "/api/threads/active/artifacts",
            "/api/threads/active/artifacts/file?path=x",
            "/api/monitor",
            "/api/workspaces",
            "/api/usage",
            "/api/git?thread_id=active",
            "/api/git/scope?thread_id=active&scope=all",
            "/api/git/diff?thread_id=active&kind=unstaged&path=README.md",
        ] {
            let response = client
                .get(format!("http://{addr}{path}"))
                .send()
                .await
                .expect("request protected API route");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        for path in ["/app", "/threads", "/monitor"] {
            let response = client
                .get(format!("http://{addr}{path}"))
                .send()
                .await
                .expect("request public Mini App page");
            assert_eq!(response.status(), StatusCode::OK);
        }

        server.abort();
    }

    /// Exercises the artifact routes over HTTP with accepted `initData`: the
    /// list must answer once with the thread's files, a listed file must be
    /// downloadable, and a path the thread never produced must be refused.
    #[tokio::test]
    async fn serves_thread_artifacts_and_refuses_foreign_paths() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let thread_id = format!("telegram-artifacts-{}", uuid::Uuid::new_v4());
        let dir = paths::artifact_dir_for_thread(Some(&thread_id));
        std::fs::create_dir_all(&dir).expect("create artifact dir");
        std::fs::write(dir.join("report.html"), "<h1>report</h1>").expect("write artifact");

        let mut thread = thread_persistence::Thread::with_id(thread_id.clone()).expect("thread");
        thread
            .append(&ThreadEvent::assistant_message(format!(
                "attached <media>{}</media>",
                dir.join("report.html").display()
            )))
            .expect("append media message");

        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            test_workers(),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));

        let list = client
            .get(format!("http://{addr}/api/threads/{thread_id}/artifacts"))
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .expect("request artifact list");
        assert_eq!(list.status(), StatusCode::OK);
        let body = list.text().await.expect("read artifact list");
        assert!(body.contains("report.html"), "{body}");
        assert!(body.contains("\"kind\":\"html\""), "{body}");
        assert!(body.contains("\"source\":\"both\""), "{body}");

        let file = client
            .get(format!(
                "http://{addr}/api/threads/{thread_id}/artifacts/file?path=report.html"
            ))
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .expect("request artifact file");
        assert_eq!(file.status(), StatusCode::OK);
        assert_eq!(
            file.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/html")
        );
        assert_eq!(file.text().await.expect("read artifact"), "<h1>report</h1>");

        let foreign = client
            .get(format!(
                "http://{addr}/api/threads/{thread_id}/artifacts/file?path=/etc/hosts"
            ))
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .expect("request foreign path");
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);

        server.abort();
        drop(home);
    }

    /// The usage route serves the shared report: a subagent run counts toward
    /// the project of the thread that started it, and the window is checked.
    #[tokio::test]
    async fn usage_route_splits_by_project_and_run() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let threads = paths::threads_dir();
        std::fs::create_dir_all(&threads).expect("create threads dir");
        let parent = format!("usage-parent-{}", uuid::Uuid::new_v4());
        let child = format!("usage-child-{}", uuid::Uuid::new_v4());
        let ts = chrono::Utc::now().to_rfc3339();
        let usage = |input: u64| {
            format!(
                r#"{{"type":"usage","input_tokens":{input},"output_tokens":0,"cache_read_tokens":0,"cache_write_tokens":0,"model":"m","provider":"p","ts":"{ts}"}}"#
            )
        };
        std::fs::write(
            threads.join(format!("{parent}.jsonl")),
            format!(
                "{{\"type\":\"meta\",\"schema_version\":1,\"root_path\":\"/p/app\",\"ts\":\"{ts}\"}}\n{}\n",
                usage(100)
            ),
        )
        .expect("write parent");
        std::fs::write(
            threads.join(format!("{child}.jsonl")),
            format!(
                "{{\"type\":\"meta\",\"schema_version\":1,\"root_path\":\"/tmp/artifacts\",\"origin_kind\":\"subagent\",\"parent_thread_id\":\"{parent}\",\"subagent_name\":\"html-artifact-specialist\",\"ts\":\"{ts}\"}}\n{}\n",
                usage(30)
            ),
        )
        .expect("write child");

        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            test_workers(),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));

        let response = client
            .get(format!("http://{addr}/api/usage"))
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .expect("request usage");
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.expect("read usage");
        assert_eq!(body["days"], 7);
        assert_eq!(body["tokens"], 130);
        assert_eq!(body["by_project"][0]["project"], "/p/app");
        assert_eq!(body["by_project"][0]["tokens"], 130);
        assert_eq!(body["by_project"][0]["threads"], 2);
        assert_eq!(body["by_run"][0]["kind"], "agent");
        assert_eq!(body["by_run"][1]["kind"], "subagent");
        assert_eq!(body["by_run"][1]["name"], "html-artifact-specialist");
        assert_eq!(body["by_run"][1]["tokens"], 30);

        let invalid = client
            .get(format!("http://{addr}/api/usage?days=0"))
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .expect("request invalid window");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        server.abort();
        drop(home);
    }

    /// The version route answers the SHA-256 of the file bytes (the value
    /// feedback names), follows a change even when the size stays the same,
    /// and keeps the file route's login and path rules.
    #[tokio::test]
    async fn artifact_version_follows_the_file_bytes() {
        use sha2::Digest as _;

        let home = zdx_engine::test_support::temp_zdx_home();
        let thread_id = format!("telegram-version-{}", uuid::Uuid::new_v4());
        let dir = paths::artifact_dir_for_thread(Some(&thread_id));
        std::fs::create_dir_all(&dir).expect("create artifact dir");
        let file = dir.join("report.html");
        std::fs::write(&file, "<h1>one</h1>").expect("write artifact");

        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            test_workers(),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));
        let version = |path: &str| {
            client
                .get(format!(
                    "http://{addr}/api/threads/{thread_id}/artifacts/version?path={path}"
                ))
                .header(AUTHORIZATION, &auth)
                .send()
        };
        let sha_of = |bytes: &[u8]| {
            sha2::Sha256::digest(bytes)
                .iter()
                .fold(String::new(), |mut out, byte| {
                    use std::fmt::Write as _;
                    let _ = write!(out, "{byte:02x}");
                    out
                })
        };
        let read = |response: reqwest::Response| async move {
            assert_eq!(response.status(), StatusCode::OK);
            response.json::<Value>().await.expect("version json")
        };

        let first = read(version("report.html").await.expect("version")).await;
        assert_eq!(first["sha256"], sha_of(b"<h1>one</h1>"));
        assert_eq!(first["size_bytes"], 12);
        assert!(first["modified_at"].is_string());
        // Cached answer, unchanged file.
        let again = read(version("report.html").await.expect("version")).await;
        assert_eq!(again["sha256"], first["sha256"]);

        // Same length, new bytes, a later modified time.
        std::fs::write(&file, "<h1>two</h1>").expect("rewrite artifact");
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|handle| handle.set_modified(later))
            .expect("set mtime");
        let changed = read(version("report.html").await.expect("version")).await;
        assert_eq!(changed["sha256"], sha_of(b"<h1>two</h1>"));

        let foreign = version("/etc/hosts").await.expect("foreign");
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
        let anonymous = reqwest::Client::new()
            .get(format!(
                "http://{addr}/api/threads/{thread_id}/artifacts/version?path=report.html"
            ))
            .send()
            .await
            .expect("anonymous");
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

        server.abort();
        drop(home);
    }

    /// Records deliveries instead of queueing a turn, so feedback tests never
    /// reach a real chat or worker.
    struct RecordingSink {
        allow: bool,
        fail_next: std::sync::atomic::AtomicBool,
        deliveries: std::sync::Mutex<Vec<FeedbackDelivery>>,
    }

    impl RecordingSink {
        fn new(allow: bool) -> Arc<Self> {
            Arc::new(Self {
                allow,
                fail_next: std::sync::atomic::AtomicBool::new(false),
                deliveries: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn count(&self) -> usize {
            self.deliveries.lock().unwrap().len()
        }
    }

    impl FeedbackSink for RecordingSink {
        fn allows(&self, _user_id: i64, _route: FeedbackRoute) -> bool {
            self.allow
        }

        fn deliver(
            &self,
            delivery: FeedbackDelivery,
        ) -> crate::artifact_feedback::DeliverFuture<'_> {
            if self
                .fail_next
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Box::pin(async { Err(anyhow::anyhow!("queue refused")) });
            }
            if let Err(err) = artifact_feedback::mark_queued(&delivery.bundle_dir) {
                return Box::pin(async move { Err(err) });
            }
            self.deliveries.lock().unwrap().push(delivery);
            Box::pin(async { Ok(()) })
        }
    }

    const FEEDBACK_PAGE: &str = "<html><body><h1>Example</h1></body></html>";
    /// Smallest valid PNG (1×1, transparent).
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn feedback_body(submission_id: &str, sha: &str) -> serde_json::Value {
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD.encode(PNG_1X1);
        serde_json::json!({
            "submission_id": submission_id,
            "artifact_path": "report.html",
            "artifact_sha256": sha,
            "annotations": [
                {
                    "intent": "change",
                    "comment": "Make the heading larger",
                    "target": {
                        "kind": "element",
                        "element": {
                            "selector": "body > h1",
                            "tag": "h1",
                            "text": "Example",
                            "rect": { "x": 8.0, "y": 21.0, "width": 300.0, "height": 37.0 },
                        },
                    },
                },
                {
                    "intent": "keep",
                    "target": {
                        "kind": "drawing",
                        "bounds": { "x": 0.0, "y": 0.0, "width": 40.0, "height": 40.0 },
                        "elements": [],
                        "sketch": { "kind": "image", "mime": "image/png", "data": png },
                    },
                },
            ],
        })
    }

    fn sha_of(text: &str) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(text.as_bytes())
            .iter()
            .fold(String::new(), |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            })
    }

    fn new_id() -> String {
        format!("sub-{}", uuid::Uuid::new_v4())
    }

    fn write_feedback_thread(thread_id: &str) {
        let dir = paths::artifact_dir_for_thread(Some(thread_id));
        std::fs::create_dir_all(&dir).expect("create artifact dir");
        std::fs::write(dir.join("report.html"), FEEDBACK_PAGE).expect("write artifact");
        thread_persistence::Thread::with_id(thread_id.to_string())
            .expect("thread")
            .append(&ThreadEvent::assistant_message("made a report"))
            .expect("append");
    }

    async fn spawn_feedback_app(
        sink: Arc<RecordingSink>,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let app = create_router(Arc::new(
            ServerState::new(
                SAMPLE_BOT_TOKEN.to_string(),
                sample_allowlist(),
                PathBuf::from("."),
                test_workers(),
            )
            .with_feedback(sink),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        (addr, server)
    }

    struct FeedbackClient {
        client: reqwest::Client,
        addr: SocketAddr,
        auth: String,
    }

    impl FeedbackClient {
        async fn post(&self, thread: &str, body: &serde_json::Value) -> (StatusCode, String) {
            let response = self
                .client
                .post(format!(
                    "http://{}/api/threads/{thread}/artifacts/feedback",
                    self.addr
                ))
                .header(AUTHORIZATION, &self.auth)
                .json(body)
                .send()
                .await
                .expect("post feedback");
            let status = response.status();
            (status, response.text().await.unwrap_or_default())
        }
    }

    /// Feedback over HTTP: authenticated, routed to the thread's own topic,
    /// queued once per submission, a failed queue retried, and refused without
    /// side effects for a reused id, a changed file, `active`, a foreign path,
    /// a thread with no chat, or a chat the sender may not use.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scripted request sequence
    async fn queues_artifact_feedback_once_into_the_threads_topic() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-9";
        write_feedback_thread(topic_thread);
        let sha = sha_of(FEEDBACK_PAGE);

        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let api = FeedbackClient {
            client: reqwest::Client::new(),
            addr,
            auth: format!("tma {}", fresh_init_data(279_058_397)),
        };
        let submission_id = new_id();

        let unauthenticated = api
            .client
            .post(format!(
                "http://{addr}/api/threads/{topic_thread}/artifacts/feedback"
            ))
            .json(&feedback_body(&submission_id, &sha))
            .send()
            .await
            .expect("post without auth");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        sink.fail_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (status, _) = api
            .post(topic_thread, &feedback_body(&submission_id, &sha))
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(sink.count(), 0);

        // The retry after a failure queues; it is not a duplicate.
        let (status, body) = api
            .post(topic_thread, &feedback_body(&submission_id, &sha))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.contains("\"queued\""), "{body}");

        let (status, body) = api
            .post(topic_thread, &feedback_body(&submission_id, &sha))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"duplicate\""), "{body}");
        assert_eq!(sink.count(), 1, "a retry must not queue a second turn");

        // The same id with different content is refused, not deduplicated.
        let mut changed = feedback_body(&submission_id, &sha);
        changed["note"] = serde_json::json!("something else");
        let (status, _) = api.post(topic_thread, &changed).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(sink.count(), 1);

        {
            let deliveries = sink.deliveries.lock().unwrap();
            let delivery = &deliveries[0];
            assert_eq!(
                delivery.route,
                FeedbackRoute {
                    chat_id: -100_777,
                    topic_id: Some(9)
                }
            );
            assert_eq!(delivery.user_id, 279_058_397);
            assert!(delivery.prompt.contains("Make the heading larger"));
            assert!(delivery.prompt.contains("note-2-drawing.png"));
            assert!(matches!(
                delivery.attachments.as_slice(),
                [LocalAttachment::Image { path, label }]
                    if path.is_file() && label == "Drawing sketch for note 2"
            ));
        }

        // A changed file needs the user's explicit "send anyway".
        let stale_id = new_id();
        let mut stale = feedback_body(&stale_id, &"0".repeat(64));
        let (status, body) = api.post(topic_thread, &stale).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert!(body.contains("changed"), "{body}");
        stale["allow_stale"] = serde_json::json!(true);
        let (status, body) = api.post(topic_thread, &stale).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.contains("\"stale\":true"), "{body}");
        assert_eq!(sink.count(), 2);

        let mut foreign = feedback_body(&new_id(), &sha);
        foreign["artifact_path"] = serde_json::json!("/etc/hosts");
        let (status, _) = api.post(topic_thread, &foreign).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = api.post("active", &feedback_body(&new_id(), &sha)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = api
            .post("cli-thread-1", &feedback_body(&new_id(), &sha))
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(sink.count(), 2);
        server.abort();

        let refusing = RecordingSink::new(false);
        let (addr, server) = spawn_feedback_app(Arc::clone(&refusing)).await;
        let api = FeedbackClient { addr, ..api };
        let refused_id = new_id();
        let (status, _) = api
            .post(topic_thread, &feedback_body(&refused_id, &sha))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(refusing.count(), 0);
        assert!(
            !artifact_feedback::feedback_dir_for_thread(topic_thread)
                .join(&refused_id)
                .exists(),
            "a refused submission saves nothing"
        );

        let oversized = api
            .client
            .post(format!(
                "http://{addr}/api/threads/{topic_thread}/artifacts/feedback"
            ))
            .header(AUTHORIZATION, &api.auth)
            .header(header::CONTENT_TYPE, "application/json")
            .body(vec![b' '; artifact_feedback::MAX_SUBMISSION_BYTES + 1])
            .send()
            .await
            .expect("post oversized body");
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

        server.abort();
        drop(home);
    }

    /// A text note whose quote and context run to the client's limits with
    /// an emoji at each edge is queued. The same note with half an emoji (a
    /// lone surrogate escape, what a plain UTF-16 cut produced) is refused by
    /// the JSON extractor before anything is saved, which is why the Mini App
    /// cuts on code points and repairs page strings (`lib/annotate.ts`).
    #[tokio::test]
    async fn text_notes_with_emoji_edges_are_queued_and_lone_surrogates_refused() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-11";
        write_feedback_thread(topic_thread);
        let sha = sha_of(FEEDBACK_PAGE);
        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let api = FeedbackClient {
            client: reqwest::Client::new(),
            addr,
            auth: format!("tma {}", fresh_init_data(279_058_397)),
        };
        let rockets = "x🚀".repeat(1000);
        let units = |text: &str| text.encode_utf16().count();
        // What the Mini App sends at its limits: whole emoji only.
        let quote: String = rockets.chars().take(1332).collect();
        let prefix: String = rockets.chars().skip(rockets.chars().count() - 42).collect();
        let suffix: String = rockets.chars().take(42).collect();
        assert!(units(&quote) <= 2000 && units(&prefix) <= 64 && units(&suffix) <= 64);
        assert!(prefix.ends_with('🚀') && suffix.ends_with('🚀'));
        let body = |id: &str, prefix: &str| {
            serde_json::json!({
                "submission_id": id,
                "artifact_path": "report.html",
                "artifact_sha256": sha,
                "annotations": [{
                    "intent": "question",
                    "comment": "Why 🚀?",
                    "target": {
                        "kind": "text",
                        "quote": quote,
                        "prefix": prefix,
                        "suffix": suffix,
                        "element": {
                            "selector": "body > h1",
                            "tag": "h1",
                            "text": rockets.chars().take(600).collect::<String>(),
                            "rect": { "x": 8.0, "y": 21.0, "width": 300.0, "height": 37.0 },
                        },
                        "rect": { "x": 9.0, "y": 22.0, "width": 30.0, "height": 12.0 },
                    },
                }],
            })
        };

        let (status, response) = api.post(topic_thread, &body(&new_id(), &prefix)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{response}");
        assert_eq!(sink.count(), 1);
        assert!(
            sink.deliveries.lock().unwrap()[0]
                .prompt
                .contains("Text just before it")
        );

        let lone_id = new_id();
        let lone = serde_json::to_string(&body(&lone_id, "LONE"))
            .expect("json")
            .replace("\"LONE\"", "\"\\ude80x🚀\"");
        let refused = api
            .client
            .post(format!(
                "http://{addr}/api/threads/{topic_thread}/artifacts/feedback"
            ))
            .header(AUTHORIZATION, &api.auth)
            .header(header::CONTENT_TYPE, "application/json")
            .body(lone)
            .send()
            .await
            .expect("post lone surrogate");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert_eq!(sink.count(), 1);
        assert!(
            !artifact_feedback::feedback_dir_for_thread(topic_thread)
                .join(&lone_id)
                .exists()
        );

        server.abort();
        drop(home);
    }

    /// Holds every save inside `allows` (which runs in the blocking save
    /// task) until the test releases it.
    struct GatedSink {
        entered: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl FeedbackSink for GatedSink {
        fn allows(&self, _user_id: i64, _route: FeedbackRoute) -> bool {
            let _ = self.entered.lock().unwrap().send(());
            let _ = self.release.lock().unwrap().recv();
            true
        }

        fn deliver(
            &self,
            _delivery: FeedbackDelivery,
        ) -> crate::artifact_feedback::DeliverFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }

    /// The upload slot stays taken while the blocking save runs, even after
    /// the client hung up: the decode and render keep running, so freeing the
    /// slot on disconnect would let more of them pile up than the cap allows.
    #[tokio::test]
    async fn feedback_slot_is_held_until_the_save_finishes_after_a_disconnect() {
        use tokio::io::AsyncWriteExt as _;

        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-9";
        write_feedback_thread(topic_thread);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let state = Arc::new(
            ServerState::new(
                SAMPLE_BOT_TOKEN.to_string(),
                sample_allowlist(),
                PathBuf::from("."),
                test_workers(),
            )
            .with_feedback(Arc::new(GatedSink {
                entered: std::sync::Mutex::new(entered_tx),
                release: std::sync::Mutex::new(release_rx),
            })),
        );
        let permits = Arc::clone(&state.feedback_permits);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let app = create_router(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });

        let body = feedback_body(&new_id(), &sha_of(FEEDBACK_PAGE)).to_string();
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let request = format!(
            "POST /api/threads/{topic_thread}/artifacts/feedback HTTP/1.1\r\nHost: test\r\nAuthorization: tma {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            fresh_init_data(279_058_397),
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("send request");
        tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(10)))
            .await
            .expect("join")
            .expect("save started");
        assert_eq!(permits.available_permits(), FEEDBACK_CONCURRENCY - 1);

        // Hang up mid-save and give the server time to drop the request.
        drop(stream);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            permits.available_permits(),
            FEEDBACK_CONCURRENCY - 1,
            "the slot is held while the save still runs"
        );

        release_tx.send(()).expect("release the save");
        let deadline = Instant::now() + Duration::from_secs(10);
        while permits.available_permits() < FEEDBACK_CONCURRENCY {
            assert!(Instant::now() < deadline, "slot freed once the save ends");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        server.abort();
        drop(home);
    }

    /// The image route serves the canonical copy (bounded, upright) with its
    /// identity in headers, only for the thread's artifacts and only with
    /// auth; feedback naming that copy is accepted and a different frame is
    /// refused; formats that cannot be annotated answer `422` with a reason.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scripted request sequence
    async fn image_route_serves_the_canonical_copy_feedback_refers_to() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-9";
        write_feedback_thread(topic_thread);
        let dir = paths::artifact_dir_for_thread(Some(topic_thread));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2000,
            1000,
            image::Rgb([10, 80, 10]),
        ))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        std::fs::write(dir.join("chart.png"), &png).unwrap();
        std::fs::write(dir.join("anim.gif"), b"GIF89a\x01\0\x01\0\0\0\0;").unwrap();
        // One byte over the annotation limit (the PNG is padded at the end).
        let mut over = png.clone();
        over.resize(
            usize::try_from(artifact_feedback::canonical::MAX_SOURCE_BYTES + 1).unwrap(),
            0,
        );
        std::fs::write(dir.join("over.png"), over).unwrap();

        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let api = FeedbackClient {
            client: reqwest::Client::new(),
            addr,
            auth: format!("tma {}", fresh_init_data(279_058_397)),
        };
        let get = |path: &str, auth: bool| {
            let request = api.client.get(format!(
                "http://{addr}/api/threads/{topic_thread}/artifacts/image?path={path}"
            ));
            if auth {
                request.header(AUTHORIZATION, &api.auth)
            } else {
                request
            }
        };

        let unauthenticated = get("chart.png", false).send().await.expect("get");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        let foreign = get("/etc/hosts", true).send().await.expect("get");
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
        let gif = get("anim.gif", true).send().await.expect("get");
        assert_eq!(gif.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(gif.text().await.unwrap().contains("not GIF files"));
        let over = get("over.png", true).send().await.expect("get");
        assert_eq!(over.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(over.text().await.unwrap().contains("larger than 25 MiB"));

        let response = get("chart.png", true).send().await.expect("get");
        assert_eq!(response.status(), StatusCode::OK);
        let header = |name: &str| {
            response.headers()[name]
                .to_str()
                .expect("header")
                .to_string()
        };
        assert_eq!(header("content-type"), "image/jpeg");
        assert_eq!(header("x-zdx-source-sha256"), sha_of_bytes(&png));
        let (canonical, width, height) = (
            header("x-zdx-canonical-sha256"),
            header("x-zdx-canonical-width"),
            header("x-zdx-canonical-height"),
        );
        assert_eq!((width.as_str(), height.as_str()), ("1568", "784"));
        assert_eq!(
            (header("x-zdx-source-width"), header("x-zdx-source-height")),
            ("2000".to_string(), "1000".to_string())
        );
        let bytes = response.bytes().await.expect("body");
        assert_eq!(
            sha_of_bytes(&bytes),
            canonical,
            "the header names these bytes"
        );
        assert_eq!(
            std::fs::read(dir.join("chart.png")).unwrap(),
            png,
            "original kept"
        );

        let body = |frame: serde_json::Value| {
            serde_json::json!({
                "submission_id": new_id(),
                "artifact_path": "chart.png",
                "artifact_sha256": sha_of_bytes(&png),
                "image": frame,
                "annotations": [
                    { "intent": "change", "comment": "Bigger", "target": { "kind": "image_pin", "point": { "x": 0.5, "y": 0.5 } } },
                ],
            })
        };
        let (status, text) = api
            .post(
                topic_thread,
                &body(serde_json::json!({ "canonical_sha256": canonical, "width": 2000, "height": 1000 })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert!(text.contains("different preview"), "{text}");
        let (status, text) = api
            .post(
                topic_thread,
                &body(serde_json::json!({ "canonical_sha256": canonical, "width": 1568, "height": 784 })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{text}");
        assert_eq!(sink.count(), 1);
        server.abort();
        drop(home);
    }

    fn sha_of_bytes(bytes: &[u8]) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            })
    }

    /// Previews and feedback saves share the engine's normalization limit, a
    /// slot outlives a client that hangs up mid-decode, and a waiter proceeds
    /// once a slot frees: no deadlock with the request permits.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scripted sequence
    async fn previews_and_feedback_share_the_normalization_limit() {
        use tokio::io::AsyncWriteExt as _;
        use zdx_engine::core::artifact_feedback::canonical::{
            self, MAX_NORMALIZATIONS, normalizations_running, test_hooks,
        };

        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-9";
        write_feedback_thread(topic_thread);
        let dir = paths::artifact_dir_for_thread(Some(topic_thread));
        let mut shas = Vec::new();
        for (name, shade) in [("a.png", 10u8), ("b.png", 90), ("c.png", 170)] {
            let mut png = Vec::new();
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                300,
                200,
                image::Rgb([shade, 40, 40]),
            ))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
            std::fs::write(dir.join(name), &png).unwrap();
            shas.push(sha_of_bytes(&png));
        }
        let pause_a = test_hooks::pause(&shas[0]);
        let pause_b = test_hooks::pause(&shas[1]);

        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let auth = format!("tma {}", fresh_init_data(279_058_397));
        let wait_until = |what: &'static str, done: Box<dyn Fn() -> bool + Send>| async move {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done() {
                assert!(Instant::now() < deadline, "timed out waiting: {what}");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let cached = |sha: &str| canonical::cached(sha).is_some();

        // A preview of a.png from a client that will hang up mid-decode.
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(
                format!(
                    "GET /api/threads/{topic_thread}/artifacts/image?path=a.png HTTP/1.1\r\nHost: test\r\nAuthorization: {auth}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("send request");
        // A feedback save on b.png: it normalizes the original too.
        let client = reqwest::Client::new();
        let post_body = serde_json::json!({
            "submission_id": new_id(),
            "artifact_path": "b.png",
            "artifact_sha256": shas[1],
            "image": { "canonical_sha256": "a".repeat(64), "width": 1, "height": 1 },
            "annotations": [
                { "intent": "change", "comment": "x", "target": { "kind": "image_pin", "point": { "x": 0.5, "y": 0.5 } } },
            ],
        });
        let post = tokio::spawn({
            let client = client.clone();
            let auth = auth.clone();
            async move {
                client
                    .post(format!(
                        "http://{addr}/api/threads/{topic_thread}/artifacts/feedback"
                    ))
                    .header(AUTHORIZATION, auth)
                    .json(&post_body)
                    .send()
                    .await
                    .expect("post")
                    .status()
            }
        });
        wait_until(
            "both slots taken by the paused preview and save",
            Box::new(|| normalizations_running() == MAX_NORMALIZATIONS),
        )
        .await;

        // A third normalization (an unpaused preview) has to wait.
        let third = tokio::spawn({
            let client = client.clone();
            let auth = auth.clone();
            async move {
                client
                    .get(format!(
                        "http://{addr}/api/threads/{topic_thread}/artifacts/image?path=c.png"
                    ))
                    .header(AUTHORIZATION, auth)
                    .send()
                    .await
                    .expect("get c")
                    .status()
            }
        });
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !third.is_finished(),
            "a third normalization waits for a slot"
        );
        assert!(!cached(&shas[2]));

        // The preview's client hangs up: its decode keeps its slot.
        drop(stream);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(normalizations_running(), MAX_NORMALIZATIONS);
        assert!(!third.is_finished(), "the disconnect freed no slot");

        // a.png finishes for nobody (and is cached); its slot goes to c.png.
        drop(pause_a);
        assert_eq!(third.await.expect("join"), StatusCode::OK);
        assert!(cached(&shas[0]), "the abandoned decode completed");
        assert!(cached(&shas[2]));
        assert!(!post.is_finished(), "the save is still paused");

        drop(pause_b);
        assert_eq!(
            post.await.expect("join"),
            StatusCode::BAD_REQUEST,
            "the frame does not match"
        );
        wait_until(
            "every slot released",
            Box::new(|| normalizations_running() == 0),
        )
        .await;
        server.abort();
        drop(home);
    }

    /// A worker thread opened directly is refused, drafts stay on the client,
    /// nothing is saved or queued, and nothing is rerouted to its orchestrator.
    #[tokio::test]
    async fn worker_feedback_is_refused_not_rerouted() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let worker = format!("worker-{}", uuid::Uuid::new_v4());
        // Lineage is recorded when the thread file is created.
        let mut thread = thread_persistence::Thread::with_id(worker.clone()).expect("thread");
        thread.set_origin(None, Some("telegram--100777-topic-9".to_string()), None);
        thread
            .append(&ThreadEvent::assistant_message("started"))
            .expect("append");
        write_feedback_thread(&worker);

        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let api = FeedbackClient {
            client: reqwest::Client::new(),
            addr,
            auth: format!("tma {}", fresh_init_data(279_058_397)),
        };
        let id = new_id();
        let (status, body) = api
            .post(&worker, &feedback_body(&id, &sha_of(FEEDBACK_PAGE)))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("worker thread"), "{body}");
        assert_eq!(sink.count(), 0);
        assert!(
            !artifact_feedback::feedback_dir_for_thread(&worker)
                .join(&id)
                .exists()
        );
        server.abort();
        drop(home);
    }

    /// Live polling over HTTP while a reply is delivered: the pending record
    /// shows a warning; its outcome (sent or failed) replaces it through a
    /// full, non-partial response, so the client never keeps a stale warning;
    /// other new events still arrive as a partial delta.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scripted pending → outcome → delta sequence
    async fn polling_replaces_a_pending_delivery_warning_with_its_outcome() {
        use zdx_engine::core::delivery::{DeliveryItem, DeliveryRecord};

        let home = zdx_engine::test_support::temp_zdx_home();
        let deliveries = ActiveDeliveries::default();
        let app = create_router(Arc::new(
            ServerState::new(
                SAMPLE_BOT_TOKEN.to_string(),
                sample_allowlist(),
                PathBuf::from("."),
                test_workers(),
            )
            .with_deliveries(deliveries.clone()),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));
        let fetch = |thread_id: String, after: Option<usize>| {
            let client = client.clone();
            let auth = auth.clone();
            async move {
                let query = after.map_or_else(String::new, |after| format!("?after={after}"));
                let url = format!("http://{addr}/api/threads/{thread_id}{query}");
                let response = client
                    .get(url)
                    .header(AUTHORIZATION, &auth)
                    .send()
                    .await
                    .expect("request thread");
                assert_eq!(response.status(), StatusCode::OK);
                response
                    .json::<serde_json::Value>()
                    .await
                    .expect("thread json")
            }
        };
        let delivery_notices = |body: &serde_json::Value| -> Vec<String> {
            body["activity"]
                .as_array()
                .expect("activity")
                .iter()
                .filter(|item| item["type"] == "notice" && item["kind"] == "delivery")
                .map(|item| item["message"].as_str().unwrap_or_default().to_string())
                .collect()
        };

        for (outcome_failed, expected_after) in [(false, 0), (true, 1)] {
            let thread_id = format!("telegram-delivery-{}", uuid::Uuid::new_v4());
            let mut thread =
                thread_persistence::Thread::with_id(thread_id.clone()).expect("thread");
            thread
                .append(&ThreadEvent::user_message("make it"))
                .expect("append");
            thread
                .append(&ThreadEvent::assistant_message("Ready"))
                .expect("append");
            let mut record = DeliveryRecord::new(
                "telegram",
                Some(-100),
                Some(7),
                vec![
                    DeliveryItem::text(),
                    DeliveryItem::file("/tmp/out/report.html"),
                ],
            );
            let running = deliveries.start(&thread_id);
            thread
                .append(&ThreadEvent::delivery(record.clone()))
                .expect("append pending");

            let first = fetch(thread_id.clone(), None).await;
            assert_eq!(first["partial"], false);
            assert_eq!(first["delivering"], true, "clients keep polling");
            assert_eq!(delivery_notices(&first).len(), 1, "pending shows a warning");
            let cursor = first["cursor"].as_u64().expect("cursor") as usize;

            record.items[0].mark_sent(Some(10));
            if outcome_failed {
                record.items[1].mark_failed("Telegram API error for sendDocument (429)");
            } else {
                record.items[1].mark_sent(Some(11));
            }
            thread
                .append(&ThreadEvent::delivery(record.clone()))
                .expect("append outcome");
            drop(running);

            let poll = fetch(thread_id.clone(), Some(cursor)).await;
            assert_eq!(poll["partial"], false, "outcome must replace, not merge");
            assert_eq!(poll["delivering"], false);
            let notices = delivery_notices(&poll);
            assert_eq!(notices.len(), expected_after, "{notices:?}");
            if outcome_failed {
                assert!(notices[0].contains("report.html: not delivered"));
            }

            thread
                .append(&ThreadEvent::user_message("next"))
                .expect("append");
            let cursor = poll["cursor"].as_u64().expect("cursor") as usize;
            let delta = fetch(thread_id.clone(), Some(cursor)).await;
            assert_eq!(delta["partial"], true, "unrelated events stay a delta");
            assert_eq!(delta["activity"].as_array().map(Vec::len), Some(1));
        }

        server.abort();
        drop(home);
    }

    /// A thread browser can open a resumed topic by its alias id. Its events
    /// and the running delivery live under the source thread, so the alias
    /// response must show the source transcript, report `delivering` while
    /// the source delivery runs, and settle when the outcome lands.
    #[tokio::test]
    async fn alias_requests_follow_the_source_thread_and_its_delivery() {
        use zdx_engine::core::delivery::{DeliveryItem, DeliveryRecord};

        let home = zdx_engine::test_support::temp_zdx_home();
        let deliveries = ActiveDeliveries::default();
        let app = create_router(Arc::new(
            ServerState::new(
                SAMPLE_BOT_TOKEN.to_string(),
                sample_allowlist(),
                PathBuf::from("."),
                test_workers(),
            )
            .with_deliveries(deliveries.clone()),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));

        let source_id = format!("telegram-source-{}", uuid::Uuid::new_v4());
        let alias_id = "telegram--1001234567890-topic-4242".to_string();
        let mut alias = thread_persistence::Thread::with_id(alias_id.clone()).expect("alias");
        alias.set_alias(Some(source_id.clone())).expect("set alias");
        let mut source = thread_persistence::Thread::with_id(source_id.clone()).expect("source");
        source
            .append(&ThreadEvent::user_message("make it"))
            .expect("append");
        source
            .append(&ThreadEvent::assistant_message("Ready"))
            .expect("append");
        let running = deliveries.start(&source_id);
        let mut record = DeliveryRecord::new(
            "telegram",
            Some(-1_001_234_567_890),
            Some(4242),
            vec![
                DeliveryItem::text(),
                DeliveryItem::file("/tmp/out/report.html"),
            ],
        );
        source
            .append(&ThreadEvent::delivery(record.clone()))
            .expect("append pending");

        let fetch = |after: Option<usize>| {
            let client = client.clone();
            let auth = auth.clone();
            let alias_id = alias_id.clone();
            async move {
                let query = after.map_or_else(String::new, |after| format!("?after={after}"));
                let response = client
                    .get(format!("http://{addr}/api/threads/{alias_id}{query}"))
                    .header(AUTHORIZATION, &auth)
                    .send()
                    .await
                    .expect("request alias thread");
                assert_eq!(response.status(), StatusCode::OK);
                response
                    .json::<serde_json::Value>()
                    .await
                    .expect("thread json")
            }
        };
        let delivery_notices = |body: &serde_json::Value| {
            body["activity"]
                .as_array()
                .expect("activity")
                .iter()
                .filter(|item| item["type"] == "notice" && item["kind"] == "delivery")
                .count()
        };

        let first = fetch(None).await;
        assert_eq!(first["id"], alias_id.as_str(), "requested id is kept");
        assert_eq!(
            first["telegram_link"], "https://t.me/c/1234567890/4242",
            "the alias topic link is kept"
        );
        assert_eq!(first["total_messages"], 2, "source transcript is shown");
        assert_eq!(first["delivering"], true, "source delivery is running");
        assert_eq!(delivery_notices(&first), 1);
        let cursor = first["cursor"].as_u64().expect("cursor") as usize;

        record.items[0].mark_sent(Some(10));
        record.items[1].mark_sent(Some(11));
        source
            .append(&ThreadEvent::delivery(record))
            .expect("append outcome");
        drop(running);

        let settled = fetch(Some(cursor)).await;
        assert_eq!(settled["delivering"], false);
        assert_eq!(
            settled["partial"], false,
            "outcome replaces the pending warning"
        );
        assert_eq!(delivery_notices(&settled), 0);

        server.abort();
        drop(home);
    }

    /// `/api/threads?kind=workers` lists only workers and flags them; the
    /// default list keeps every top-level thread with the same flag.
    #[tokio::test]
    async fn thread_list_filters_and_flags_workers() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let orchestrator_id = format!("orch-{}", uuid::Uuid::new_v4());
        let mut orchestrator =
            thread_persistence::Thread::with_id(orchestrator_id.clone()).expect("thread");
        orchestrator
            .append(&ThreadEvent::user_message("plan"))
            .expect("append");
        let worker_id = format!("worker-{}", uuid::Uuid::new_v4());
        let mut worker = thread_persistence::Thread::with_id(worker_id.clone()).expect("thread");
        worker.set_origin(None, Some(orchestrator_id.clone()), None);
        worker
            .append(&ThreadEvent::user_message("work"))
            .expect("append");

        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            test_workers(),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));
        let fetch = |query: &'static str| {
            let client = client.clone();
            let auth = auth.clone();
            async move {
                let response = client
                    .get(format!("http://{addr}/api/threads{query}"))
                    .header(AUTHORIZATION, &auth)
                    .send()
                    .await
                    .expect("request thread list");
                let status = response.status();
                let body: serde_json::Value = response.json().await.unwrap_or_default();
                (status, body)
            }
        };
        let flags = |body: &serde_json::Value| -> Vec<(String, bool)> {
            body["threads"]
                .as_array()
                .expect("threads array")
                .iter()
                .map(|t| {
                    (
                        t["id"].as_str().unwrap().to_string(),
                        t["is_worker"].as_bool().unwrap(),
                    )
                })
                .collect()
        };

        let (status, all) = fetch("").await;
        assert_eq!(status, StatusCode::OK);
        let all = flags(&all);
        assert!(all.contains(&(worker_id.clone(), true)), "{all:?}");
        assert!(all.contains(&(orchestrator_id.clone(), false)), "{all:?}");

        let (status, workers) = fetch("?kind=workers").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(flags(&workers), vec![(worker_id, true)]);

        let (status, _) = fetch("?kind=bogus").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        server.abort();
        drop(home);
    }

    #[tokio::test]
    async fn serves_the_built_miniapp_with_cacheable_assets() {
        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            test_workers(),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();

        let index = client
            .get(format!("http://{addr}/app"))
            .send()
            .await
            .expect("request shell");
        assert_eq!(index.status(), StatusCode::OK);
        // The entry point must revalidate so new content hashes are picked up.
        assert_eq!(
            index
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-cache")
        );
        let html = index.text().await.expect("read shell");
        assert!(
            html.contains("<div id=\"app\">"),
            "expected the Svelte shell"
        );

        // Pull a hashed asset straight out of the shell and confirm it is served
        // immutable — the only caching layer Telegram WebViews give us.
        let asset = first_script_asset(&html);

        let response = client
            .get(format!("http://{addr}{asset}"))
            .send()
            .await
            .expect("request hashed asset");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=31536000, immutable")
        );

        let missing = client
            .get(format!("http://{addr}/app/assets/nope.js"))
            .send()
            .await
            .expect("request missing asset");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        server.abort();
    }

    #[tokio::test]
    async fn compresses_miniapp_assets_when_the_client_accepts_gzip() {
        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            test_workers(),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });

        // reqwest transparently decodes gzip, so ask for the raw bytes instead.
        let client = reqwest::Client::builder()
            .no_gzip()
            .build()
            .expect("build client");

        // The SPA entry point is tiny; the bundle it pulls in is what actually
        // benefits, so assert the saving there.
        let html = client
            .get(format!("http://{addr}/app"))
            .send()
            .await
            .expect("request shell")
            .text()
            .await
            .expect("read shell");
        let asset = first_script_asset(&html);

        let compressed = client
            .get(format!("http://{addr}{asset}"))
            .header("accept-encoding", "gzip")
            .send()
            .await
            .expect("request compressed asset");
        assert_eq!(compressed.status(), StatusCode::OK);
        assert_eq!(
            compressed
                .headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let compressed_len = compressed.bytes().await.expect("read body").len();

        let identity = client
            .get(format!("http://{addr}{asset}"))
            .header("accept-encoding", "identity")
            .send()
            .await
            .expect("request uncompressed asset");
        assert!(identity.headers().get("content-encoding").is_none());
        let identity_len = identity.bytes().await.expect("read body").len();

        assert!(
            compressed_len * 2 < identity_len,
            "gzip should at least halve the bundle: {compressed_len} vs {identity_len}"
        );

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
