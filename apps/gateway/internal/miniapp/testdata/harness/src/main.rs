//! Golden harness: the extractors and layers of crates/zdx-bot/src/server.rs
//! (same axum, tower-http, serde versions as the workspace lock), with
//! handlers that echo what they parsed. Used to record transport-level
//! answers the Go Mini App server must reproduce.
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, Request};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::compression::CompressionLayer;

// Copied from server.rs.
#[derive(Deserialize)]
struct ThreadQuery {
    after: Option<usize>,
}
#[derive(Deserialize, Default, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
enum ThreadListKind {
    #[default]
    All,
    Workers,
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
    commit: Option<String>,
}
#[derive(Deserialize)]
struct GitScopeQuery {
    thread_id: Option<String>,
    scope: String,
}
#[derive(Deserialize)]
struct ArtifactFileQuery {
    path: String,
}
#[derive(Deserialize)]
struct UsageQuery {
    days: Option<u32>,
}

/// Top level of zdx_engine `FeedbackSubmission` (nested types as Value).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct TypedSubmission {
    submission_id: String,
    artifact_path: String,
    artifact_sha256: String,
    #[serde(default)]
    allow_stale: bool,
    #[serde(default)]
    note: String,
    #[serde(default)]
    viewport: Option<Value>,
    #[serde(default)]
    image: Option<Value>,
    annotations: Vec<Value>,
}

const MAX_SUBMISSION_BYTES: usize = 48 * 1024 * 1024;

async fn auth(request: Request, next: Next) -> Response {
    if request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("tma ok")
    {
        return (StatusCode::UNAUTHORIZED, "Telegram authorization required").into_response();
    }
    next.run(request).await
}

fn echo(value: Value) -> Json<Value> {
    Json(value)
}

#[tokio::main]
async fn main() {
    let api = Router::new()
        .route("/threads", get(|Query(q): Query<ThreadListQuery>| async move { echo(json!({"kind": format!("{:?}", q.kind).to_lowercase()})) }))
        .route("/threads/{id}/trajectory", get(|Path(id): Path<String>| async move { echo(json!({"id": id})) }))
        .route("/threads/{id}/artifacts", get(|Path(id): Path<String>| async move { echo(json!({"id": id})) }))
        .route("/threads/{id}/artifacts/file", get(|Path(id): Path<String>, Query(q): Query<ArtifactFileQuery>| async move { echo(json!({"id": id, "path": q.path})) }))
        .route("/threads/{id}/artifacts/version", get(|Path(id): Path<String>, Query(q): Query<ArtifactFileQuery>| async move { echo(json!({"id": id, "path": q.path})) }))
        .route("/threads/{id}/artifacts/image", get(|Path(id): Path<String>, Query(q): Query<ArtifactFileQuery>| async move { echo(json!({"id": id, "path": q.path})) }))
        .route(
            "/threads/{id}/artifacts/feedback",
            post(|Path(id): Path<String>, Json(body): Json<Value>| async move { echo(json!({"id": id, "submission_id": body.get("submission_id")})) })
                .layer(DefaultBodyLimit::max(MAX_SUBMISSION_BYTES)),
        )
        .route("/typed", post(|Json(_): Json<TypedSubmission>| async { "ok" }))
        .route("/threads/{id}", get(|Path(id): Path<String>, Query(q): Query<ThreadQuery>| async move { echo(json!({"id": id, "after": q.after})) }))
        .route("/threads/{id}/workers", get(|Path(id): Path<String>| async move { echo(json!({"id": id})) }))
        .route("/usage", get(|Query(q): Query<UsageQuery>| async move { echo(json!({"days": q.days})) }))
        .route("/git", get(|Query(q): Query<GitQuery>| async move { echo(json!({"thread_id": q.thread_id})) }))
        .route("/git/scope", get(|Query(q): Query<GitScopeQuery>| async move { echo(json!({"thread_id": q.thread_id, "scope": q.scope})) }))
        .route("/git/diff", get(|Query(q): Query<GitDiffQuery>| async move { echo(json!({"thread_id": q.thread_id, "path": q.path, "kind": q.kind, "commit": q.commit})) }))
        .route_layer(middleware::from_fn(auth));

    let app = Router::new()
        .route(
            "/app/{*path}",
            get(|Path(path): Path<String>| async move { echo(json!({"path": path})) }),
        )
        .route("/app", get(|| async { "shell" }))
        .route("/threads", get(|| async { "shell" }))
        .route("/monitor", get(|| async { "shell" }))
        .route(
            "/z/{kind}",
            get(|Path(kind): Path<String>| async move {
                let (ctype, body): (&str, String) = match kind.as_str() {
                    "small" => ("text/plain", "x".repeat(31)),
                    "edge" => ("text/plain", "x".repeat(32)),
                    "png" => ("image/png", "x".repeat(400)),
                    "svg" => ("image/svg+xml", "x".repeat(400)),
                    "upper" => ("Image/PNG", "x".repeat(400)),
                    "sse" => ("text/event-stream", "x".repeat(400)),
                    "grpc" => ("application/grpc", "x".repeat(400)),
                    _ => ("application/json", "x".repeat(400)),
                };
                ([(header::CONTENT_TYPE, ctype)], body).into_response()
            }),
        )
        .route(
            "/z/error",
            get(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    "Invalid or expired Telegram authorization",
                )
                    .into_response()
            }),
        )
        .nest("/api", api)
        .layer(CompressionLayer::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    println!("{}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}
