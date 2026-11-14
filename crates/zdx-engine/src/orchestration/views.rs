//! Read-only views of saved state for chat surfaces (the Mini App): the
//! exact JSON bodies of its API routes, built in Rust so no surface
//! re-derives them. The bodies are plain engine functions
//! ([`super::view_bodies::EngineViews`]). The service serves them through a
//! [`ViewProvider`] that the `zdx` binary installs (`zdx orchestrator
//! serve`); the gateway's Mini App routes read them over the socket, and
//! `zdx views` is their CLI twin.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::protocol::ErrorCode;
pub use super::view_bodies::EngineViews;
use super::view_bodies::{ThreadListKind, check_artifact_path};

/// One view request. The JSON carries the op name in `op`, like every other
/// request of the socket protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ViewRequest {
    /// `GET /api/threads`; `kind` is `all` (default) or `workers`.
    ThreadsList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    /// `GET /api/threads/{id}`. `source_id` in the reply is the thread the
    /// events come from (`active` resolved, one alias hop). The body's
    /// `delivering` is always false: the surface that delivers sets it.
    ThreadView {
        thread_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<usize>,
    },
    /// `GET /api/threads/{id}/trajectory`.
    ThreadTrajectory { thread_id: String },
    /// `GET /api/threads/{id}/workers`.
    ThreadWorkers { thread_id: String },
    /// `GET /api/threads/{id}/artifacts`.
    ArtifactsList { thread_id: String },
    /// The file `GET /api/threads/{id}/artifacts/file` serves:
    /// `{path, mime, filename}`.
    ArtifactResolve { thread_id: String, path: String },
    /// `GET /api/threads/{id}/artifacts/version`.
    ArtifactVersion { thread_id: String, path: String },
    /// The image `GET /api/threads/{id}/artifacts/image` serves, with its
    /// identity: `{mime, data (base64), source_sha256, sha256, width, height,
    /// source_width, source_height}`.
    ArtifactCanonical { thread_id: String, path: String },
    /// `GET /api/monitor` without `active_agents` (its own view) and quotas
    /// (the gateway reads them with the shared Go quota package);
    /// `subscriptions` is empty and `subscriptions_status` is `pending`.
    MonitorSnapshot,
    /// The `active_agents` of `GET /api/monitor`, read fresh.
    ActiveAgents,
    /// `GET /api/workspaces`.
    Workspaces,
    /// `GET /api/usage?days=`.
    UsageReport {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        days: Option<u32>,
    },
    /// The thread a Git route means and the roots its repository is looked
    /// for in: `{thread_id, effective_id, root_path, fallback_root}`
    /// ([`super::view_bodies::ThreadRootResponse`]). `thread_id` absent or
    /// blank means `active`. The Go gateway reads it to serve `/api/git*`
    /// with `apps/shared/gitview`.
    ThreadRoot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
    },
    /// `GET /api/git`.
    GitStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
    },
    /// `GET /api/git/scope`.
    GitScope {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        scope: String,
    },
    /// `GET /api/git/diff`.
    GitDiff {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        path: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
    },
}

/// A view's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewOutput {
    pub body: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

/// A refused view: the code a client maps to its HTTP status, and the exact
/// text the route answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewError {
    pub code: ErrorCode,
    pub message: String,
}

pub type ViewFuture = Pin<Box<dyn Future<Output = Result<ViewOutput, ViewError>> + Send>>;

/// Builds views. [`EngineViews`] is the implementation; the trait keeps the
/// service independent of how bodies are built.
pub trait ViewProvider: Send + Sync {
    fn view(&self, request: ViewRequest) -> ViewFuture;
    /// The root the views read config, usage, and the Git fallback from.
    fn root(&self) -> &std::path::Path;
}

/// The capability each view needs (`StatusReport::capabilities`).
#[must_use]
pub fn capability(request: &ViewRequest) -> &'static str {
    match request {
        ViewRequest::ThreadsList { .. }
        | ViewRequest::ThreadView { .. }
        | ViewRequest::ThreadTrajectory { .. }
        | ViewRequest::ThreadWorkers { .. } => super::protocol::CAPABILITY_THREAD_VIEWS,
        ViewRequest::ArtifactsList { .. }
        | ViewRequest::ArtifactResolve { .. }
        | ViewRequest::ArtifactVersion { .. }
        | ViewRequest::ArtifactCanonical { .. } => super::protocol::CAPABILITY_ARTIFACT_VIEWS,
        ViewRequest::MonitorSnapshot
        | ViewRequest::ActiveAgents
        | ViewRequest::Workspaces
        | ViewRequest::UsageReport { .. } => super::protocol::CAPABILITY_MONITOR_VIEWS,
        ViewRequest::ThreadRoot { .. } => super::protocol::CAPABILITY_THREAD_ROOT,
        ViewRequest::GitStatus { .. }
        | ViewRequest::GitScope { .. }
        | ViewRequest::GitDiff { .. } => super::protocol::CAPABILITY_GIT_VIEWS,
    }
}

impl ViewProvider for EngineViews {
    fn root(&self) -> &std::path::Path {
        EngineViews::root(self)
    }

    fn view(&self, request: ViewRequest) -> ViewFuture {
        let views = self.clone();
        Box::pin(async move { build(&views, request).await })
    }
}

fn body<T: Serialize>(value: &T, source_id: Option<String>) -> Result<ViewOutput, ViewError> {
    serde_json::to_value(value)
        .map(|body| ViewOutput { body, source_id })
        .map_err(|error| ViewError {
            code: ErrorCode::Internal,
            message: format!("encode view: {error}"),
        })
}

/// Answers one request with the same body the matching HTTP route returns.
/// Artifact requests check the path before the thread, as the socket always
/// has; the routes check the thread first.
async fn build(views: &EngineViews, request: ViewRequest) -> Result<ViewOutput, ViewError> {
    use base64::Engine as _;

    match request {
        ViewRequest::ThreadsList { kind } => {
            let kind = match kind.as_deref() {
                None | Some("all") => ThreadListKind::All,
                Some("workers") => ThreadListKind::Workers,
                Some(_) => {
                    return Err(ViewError {
                        code: ErrorCode::Invalid,
                        message: "Invalid thread list kind".to_string(),
                    });
                }
            };
            body(&views.threads(kind).await?, None)
        }
        ViewRequest::ThreadView { thread_id, after } => {
            let (thread, source_id) = views.thread(thread_id, after).await?;
            body(&thread, Some(source_id))
        }
        ViewRequest::ThreadTrajectory { thread_id } => {
            body(&views.trajectory(thread_id).await?, None)
        }
        ViewRequest::ThreadWorkers { thread_id } => {
            body(&views.thread_workers(thread_id).await?, None)
        }
        ViewRequest::ArtifactsList { thread_id } => body(&views.artifacts(thread_id).await?, None),
        ViewRequest::ArtifactResolve { thread_id, path } => {
            check_artifact_path(&path)?;
            let file = views.resolve_artifact(thread_id, path).await?;
            Ok(ViewOutput {
                body: serde_json::json!({
                    "path": file.path,
                    "mime": file.mime,
                    "filename": file.filename,
                }),
                source_id: None,
            })
        }
        ViewRequest::ArtifactVersion { thread_id, path } => {
            check_artifact_path(&path)?;
            body(&views.artifact_version(thread_id, path).await?, None)
        }
        ViewRequest::ArtifactCanonical { thread_id, path } => {
            check_artifact_path(&path)?;
            let image = views.canonical_image(thread_id, path).await?;
            let meta = image.meta;
            let (source_width, source_height) = meta.source_upright();
            Ok(ViewOutput {
                body: serde_json::json!({
                    "mime": meta.mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                    "source_sha256": meta.source_sha256,
                    "sha256": meta.sha256,
                    "width": meta.width,
                    "height": meta.height,
                    "source_width": source_width,
                    "source_height": source_height,
                }),
                source_id: None,
            })
        }
        ViewRequest::MonitorSnapshot => body(&views.monitor_snapshot().await?, None),
        ViewRequest::ActiveAgents => body(&views.active_agents().await?, None),
        ViewRequest::Workspaces => body(&views.workspaces().await?, None),
        ViewRequest::UsageReport { days } => body(&views.usage(days).await?, None),
        ViewRequest::ThreadRoot { thread_id } => body(&views.thread_root(thread_id).await?, None),
        ViewRequest::GitStatus { thread_id } => body(&views.git_status(thread_id).await?, None),
        ViewRequest::GitScope { thread_id, scope } => {
            body(&views.git_scope(thread_id, &scope).await?, None)
        }
        ViewRequest::GitDiff {
            thread_id,
            path,
            kind,
            commit,
        } => body(
            &views
                .git_diff(thread_id, path, &kind, commit.as_deref())
                .await?,
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::ViewRequest;
    use crate::orchestration::protocol::Request;

    /// `subscription_quotas` is retired (quotas are read with the shared Go
    /// quota package): neither a view nor any other request accepts it.
    #[test]
    fn retired_subscription_quotas_view_is_refused() {
        let line = r#"{"op":"subscription_quotas"}"#;
        assert!(serde_json::from_str::<ViewRequest>(line).is_err());
        assert!(serde_json::from_str::<Request>(line).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"op":"monitor_snapshot"}"#).is_ok());
    }
}
