//! Artifact bodies: a thread's artifact list, one file (bytes or resolved
//! path), its current version, and the canonical image copy.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::{
    EngineViews, internal, invalid, refused, resolve_alias_id, resolve_thread_param,
    valid_thread_id,
};
use crate::core::artifact_feedback::FeedbackError;
use crate::core::artifact_feedback::canonical::{self, CanonicalImage};
use crate::core::artifacts::{self, Artifact};
use crate::orchestration::protocol::ErrorCode;
use crate::orchestration::views::ViewError;

/// Image previews read at once (see `EngineViews::image_permits`).
pub(super) const IMAGE_READS: usize = 2;

/// File hashes by path, valid while the file keeps the same length and
/// modified time, so a viewer checking an open artifact every 30 s does not
/// re-read it each time. Bounded: cleared when it grows past
/// `MAX_VERSION_CACHE` paths.
pub(super) type VersionCache = std::sync::Mutex<HashMap<PathBuf, (u64, SystemTime, String)>>;
const MAX_VERSION_CACHE: usize = 256;

/// Thread artifact tab payload. Rows come from the shared engine artifact
/// model: files in the thread artifact dir plus files the thread sent, merged
/// on the absolute path. `Artifact` is defined there so other surfaces reuse
/// the same shape instead of re-deriving it.
#[derive(Serialize)]
pub struct ArtifactsResponse {
    pub artifacts: Vec<Artifact>,
}

/// The version of one artifact file as it is now. `sha256` is the hash of the
/// file bytes, the same value the Mini App names when it sends feedback, so a
/// viewer can tell the file changed under it without downloading it.
#[derive(Serialize)]
pub struct ArtifactVersionResponse {
    sha256: String,
    size_bytes: u64,
    modified_at: Option<String>,
}

/// One artifact file, resolved for download.
pub struct ArtifactFile {
    pub path: PathBuf,
    pub mime: String,
    /// The file name, with `"` replaced so it fits a quoted header value.
    pub filename: String,
}

fn artifact_not_found() -> ViewError {
    refused(ErrorCode::ArtifactNotFound, "Artifact not found")
}

/// Refuses an artifact path no listing can contain.
///
/// # Errors
/// `Invalid` with the route's text.
pub fn check_artifact_path(path: &str) -> Result<(), ViewError> {
    if path.trim().is_empty() || path.contains('\0') {
        return Err(invalid("Invalid artifact path"));
    }
    Ok(())
}

/// The effective thread (`active` resolved, one alias hop) of an artifact
/// request, after checking the id and then the path.
fn artifact_target(id: String, path: &str) -> Result<String, ViewError> {
    let target_id = resolve_thread_param(id);
    if !valid_thread_id(&target_id) {
        return Err(invalid("Invalid thread ID"));
    }
    check_artifact_path(path)?;
    Ok(resolve_alias_id(&target_id))
}

fn resolve_file(effective: &str, path: &str) -> Result<ArtifactFile, ViewError> {
    let (path, mime) = artifacts::resolve_artifact_for_download(effective, path).map_err(|error| {
        tracing::warn!(thread_id = effective, error = %format!("{error:#}"), "Failed to resolve Mini App artifact");
        artifact_not_found()
    })?;
    let filename = path.file_name().map_or_else(
        || "artifact".to_string(),
        |name| name.to_string_lossy().replace('"', "_"),
    );
    Ok(ArtifactFile {
        path,
        mime,
        filename,
    })
}

impl EngineViews {
    /// The shared artifact projection of a thread (one alias hop resolved).
    ///
    /// # Errors
    /// `Invalid` for a bad id, `Internal` when the list cannot be built.
    pub async fn artifacts(&self, id: String) -> Result<ArtifactsResponse, ViewError> {
        tokio::task::spawn_blocking(move || {
            let target_id = resolve_thread_param(id);
            if !valid_thread_id(&target_id) {
                return Err(invalid("Invalid thread ID"));
            }
            let effective = resolve_alias_id(&target_id);
            let artifacts = artifacts::list_thread_artifacts(&effective).map_err(|error| {
                tracing::warn!(thread_id = effective, error = %format!("{error:#}"), "Failed to list Mini App artifacts");
                internal("Artifact list failed")
            })?;
            Ok(ArtifactsResponse { artifacts })
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App artifact list task failed");
            internal("Artifact list failed")
        })?
    }

    /// Resolves one listed artifact to its file, without reading it.
    ///
    /// # Errors
    /// `Invalid` for a bad id or path, `ArtifactNotFound` when the path is
    /// not an artifact of the thread.
    pub async fn resolve_artifact(
        &self,
        id: String,
        path: String,
    ) -> Result<ArtifactFile, ViewError> {
        tokio::task::spawn_blocking(move || {
            let effective = artifact_target(id, &path)?;
            resolve_file(&effective, &path)
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App artifact read task failed");
            internal("Artifact read failed")
        })?
    }

    /// One listed artifact's bytes, with its file.
    ///
    /// # Errors
    /// As [`Self::resolve_artifact`]; `ArtifactNotFound` also when the file
    /// cannot be read.
    pub async fn read_artifact(
        &self,
        id: String,
        path: String,
    ) -> Result<(Vec<u8>, ArtifactFile), ViewError> {
        let effective = artifact_target(id, &path)?;
        tokio::task::spawn_blocking(move || {
            let file = resolve_file(&effective, &path)?;
            let bytes = std::fs::read(&file.path).map_err(|error| {
                tracing::warn!(path = %file.path.display(), error = %format!("{error:#}"), "Failed to read Mini App artifact");
                artifact_not_found()
            })?;
            Ok((bytes, file))
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App artifact read task failed");
            internal("Artifact read failed")
        })?
    }

    /// The SHA-256, size, and modified time of one listed artifact. Hashes
    /// are cached per path while length and modified time stay the same.
    ///
    /// # Errors
    /// `Invalid` for a bad id or path, `ArtifactNotFound` when the path is
    /// not an artifact of the thread or cannot be read.
    ///
    /// # Panics
    /// When the version cache lock is poisoned.
    pub async fn artifact_version(
        &self,
        id: String,
        path: String,
    ) -> Result<ArtifactVersionResponse, ViewError> {
        let effective = artifact_target(id, &path)?;
        let cache = Arc::clone(&self.artifact_versions);
        tokio::task::spawn_blocking(move || {
            let (path, _mime) = artifacts::resolve_artifact_for_download(&effective, &path)
                .map_err(|error| {
                    tracing::warn!(thread_id = effective, error = %format!("{error:#}"), "Failed to resolve Mini App artifact version");
                    artifact_not_found()
                })?;
            let not_found = |error: std::io::Error| {
                tracing::warn!(path = %path.display(), %error, "Failed to read Mini App artifact version");
                artifact_not_found()
            };
            let stamp = |meta: &std::fs::Metadata| (meta.len(), meta.modified().unwrap_or(UNIX_EPOCH));
            let before = stamp(&std::fs::metadata(&path).map_err(not_found)?);
            let cached = cache
                .lock()
                .expect("version cache lock poisoned")
                .get(&path)
                .filter(|(len, modified, _)| (*len, *modified) == before)
                .map(|(_, _, sha)| sha.clone());
            let sha256 = if let Some(sha) = cached {
                sha
            } else {
                let sha = artifacts::file_sha256(&path).map_err(not_found)?;
                // Cached only when the file did not change while it was read;
                // a mixed read can only cause one extra "new version" notice,
                // because the viewer hashes the bytes it loads on Update.
                if std::fs::metadata(&path).is_ok_and(|meta| stamp(&meta) == before) {
                    let mut cache = cache.lock().expect("version cache lock poisoned");
                    if cache.len() >= MAX_VERSION_CACHE {
                        cache.clear();
                    }
                    cache.insert(path.clone(), (before.0, before.1, sha.clone()));
                }
                sha
            };
            let modified_at = (before.1 != UNIX_EPOCH)
                .then(|| chrono::DateTime::<chrono::Utc>::from(before.1).to_rfc3339());
            Ok(ArtifactVersionResponse {
                sha256,
                size_bytes: before.0,
                modified_at,
            })
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App artifact version task failed");
            internal("Artifact version failed")
        })?
    }

    /// The canonical copy of an image artifact (`artifact_feedback::
    /// canonical`): small, upright, made once and cached. The Mini App
    /// previews and annotates exactly these bytes and sends back their hash,
    /// so marks refer to the pixels the agent gets. At most `IMAGE_READS`
    /// previews read originals at once; requests wait for a permit.
    ///
    /// # Errors
    /// `Invalid` for a bad id or path, `ArtifactNotFound`,
    /// `UnsupportedArtifact` (with the reason) when the image cannot be
    /// annotated, `Busy` when no copy could be made in time.
    pub async fn canonical_image(
        &self,
        id: String,
        path: String,
    ) -> Result<CanonicalImage, ViewError> {
        let effective = artifact_target(id, &path)?;
        let Ok(permit) = Arc::clone(&self.image_permits).acquire_owned().await else {
            return Err(refused(ErrorCode::Busy, "Image previews are unavailable"));
        };
        // The permit lives in the blocking task: a client that leaves early does
        // not free it while the read runs. The engine's normalization slot is
        // taken inside, after the cache check, and held until the decode and
        // shrink finish.
        let made = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            canonical::canonical_for_artifact(&effective, &path)
        })
        .await;
        match made {
            Ok(Ok(image)) => Ok(image),
            Ok(Err(FeedbackError::ArtifactNotFound)) => Err(artifact_not_found()),
            Ok(Err(FeedbackError::UnsupportedArtifact(kind))) => Err(refused(
                ErrorCode::UnsupportedArtifact,
                format!("Annotation works on PNG, JPEG, and WebP images, not {kind} files."),
            )),
            Ok(Err(FeedbackError::Invalid(message))) => {
                Err(refused(ErrorCode::UnsupportedArtifact, message))
            }
            Ok(Err(error @ FeedbackError::Busy)) => {
                Err(refused(ErrorCode::Busy, error.to_string()))
            }
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "Failed to make an image copy");
                Err(internal("Image preview failed"))
            }
            Err(error) => {
                tracing::warn!(%error, "Image copy task failed");
                Err(internal("Image preview failed"))
            }
        }
    }
}
