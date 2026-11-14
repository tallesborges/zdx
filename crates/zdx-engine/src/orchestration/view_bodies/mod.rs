//! Bodies of the read-only views ([`super::views`]): the exact JSON the Mini
//! App API answers, built from saved state by plain engine functions. The
//! service's view provider (`zdx orchestrator serve`, `zdx views`) and the
//! bot's HTTP routes both call [`EngineViews`]; neither re-derives a body.
//!
//! Errors are [`ViewError`]s: the [`ErrorCode`] a surface maps to its HTTP
//! status, and the exact text the route answers with.

mod artifacts;
mod git;
mod monitor;
mod threads;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};

pub use self::artifacts::{
    ArtifactFile, ArtifactVersionResponse, ArtifactsResponse, check_artifact_path,
};
pub use self::git::{GitDiffResponse, GitResponse, GitScopeResponse};
pub use self::monitor::{
    MAX_USAGE_DAYS, MonitorAgent, MonitorResponse, MonitorSubscription, UsageResponse,
    WorkspacesResponse,
};
pub use self::threads::{
    EventsLoad, ThreadActivity, ThreadListItem, ThreadListKind, ThreadListResponse, ThreadResponse,
    WorkerItem, WorkerRollup, WorkersResponse,
};
use super::OrchestratorClient;
use super::protocol::ErrorCode;
use super::views::ViewError;
use crate::core::workers::{WorkerManager, WorkerSnapshot, WorkerStatus};
use crate::core::{thread_index, thread_persistence};

/// Builds view bodies for one root: the fallback root of Git views and the
/// root whose config the dashboard and usage read. The root is the caller's,
/// passed explicitly; nothing here reads the current directory.
///
/// Holds the caches the bodies share (dashboard snapshot, usage windows,
/// quotas, artifact hashes) and the image-preview permits, so every surface
/// built on one instance (or a clone of it) shares them.
#[derive(Clone)]
pub struct EngineViews {
    root: PathBuf,
    /// Live worker state for orchestrator threads. In-memory in the service
    /// and process-scoped: it is empty for workers that ran before the
    /// service restarted (or while it is down), which is why worker listings
    /// merge it with persisted lineage.
    workers: Arc<LiveWorkers>,
    monitor_cache: Arc<RwLock<Option<monitor::CachedMonitor>>>,
    /// Single-flight guard for building `monitor_cache`; see `monitor`.
    monitor_build: Arc<Mutex<()>>,
    /// Usage reports per window, held for `MONITOR_CACHE_TTL`. The lock is
    /// held across a build, so concurrent requests share one usage-cache sync.
    usage_cache: Arc<Mutex<HashMap<u32, monitor::CachedUsage>>>,
    subscription_cache: Arc<Mutex<Option<monitor::CachedSubscriptions>>>,
    /// Single-flight guard for the background quota refresh, held for the
    /// whole fetch. Every open Mini App polls `/api/monitor` every 30s, so
    /// without it a slow provider would accumulate overlapping refreshes.
    subscription_refresh: Arc<Mutex<()>>,
    /// Image previews served at once: each reads and hashes an original of
    /// up to 25 MiB (`canonical::MAX_SOURCE_BYTES`, checked from the file
    /// length first). Making the copy is bounded separately, process-wide, by
    /// `canonical::MAX_NORMALIZATIONS` (shared with feedback saves). Requests
    /// wait for a permit instead of failing: a preview is a read.
    image_permits: Arc<tokio::sync::Semaphore>,
    artifact_versions: Arc<artifacts::VersionCache>,
}

impl EngineViews {
    /// Views inside the orchestrator service: live workers come from its own
    /// manager.
    #[must_use]
    pub fn in_service(root: PathBuf, manager: Arc<WorkerManager>) -> Self {
        Self::new(root, LiveWorkers::Manager(manager))
    }

    /// Views outside the service (`zdx views`): live workers come over its
    /// socket, or none when it is not running.
    #[must_use]
    pub fn client(root: PathBuf) -> Self {
        Self::with_client(root, OrchestratorClient::new())
    }

    /// Views that read live workers through `client`.
    #[must_use]
    pub fn with_client(root: PathBuf, client: OrchestratorClient) -> Self {
        Self::new(root, LiveWorkers::Client(client))
    }

    fn new(root: PathBuf, workers: LiveWorkers) -> Self {
        Self {
            root,
            workers: Arc::new(workers),
            monitor_cache: Arc::new(RwLock::new(None)),
            monitor_build: Arc::new(Mutex::new(())),
            usage_cache: Arc::new(Mutex::new(HashMap::new())),
            subscription_cache: Arc::new(Mutex::new(None)),
            subscription_refresh: Arc::new(Mutex::new(())),
            image_permits: Arc::new(tokio::sync::Semaphore::new(artifacts::IMAGE_READS)),
            artifact_versions: Arc::new(artifacts::VersionCache::default()),
        }
    }

    /// The root these views were built for.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Blocking reads of live worker state, for bodies that already run on the
/// blocking pool: from the orchestrator service over its socket (an
/// unreachable service reads as no live workers), or, inside the service,
/// from its own worker manager.
pub enum LiveWorkers {
    Client(OrchestratorClient),
    Manager(Arc<WorkerManager>),
}

impl LiveWorkers {
    fn list(&self, owner_thread_id: Option<&str>) -> Vec<WorkerSnapshot> {
        match self {
            Self::Client(client) => client.workers_blocking(owner_thread_id),
            Self::Manager(manager) => match owner_thread_id {
                Some(owner) => manager.list_for_owner(owner),
                None => manager.list_all(),
            },
        }
    }

    fn list_for_owner(&self, owner_thread_id: &str) -> Vec<WorkerSnapshot> {
        self.list(Some(owner_thread_id))
    }

    fn statuses_by_owner(&self) -> HashMap<String, Vec<WorkerStatus>> {
        let mut grouped: HashMap<String, Vec<WorkerStatus>> = HashMap::new();
        for worker in self.list(None) {
            grouped
                .entry(worker.owner_thread_id)
                .or_default()
                .push(worker.status);
        }
        grouped
    }
}

fn refused(code: ErrorCode, message: impl Into<String>) -> ViewError {
    ViewError {
        code,
        message: message.into(),
    }
}

fn invalid(message: &str) -> ViewError {
    refused(ErrorCode::Invalid, message)
}

fn internal(message: &str) -> ViewError {
    refused(ErrorCode::Internal, message)
}

/// Whether `id` names a thread file without escaping the threads directory.
#[must_use]
pub fn valid_thread_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\', '\0'])
}

/// Resolves a thread id the way the thread view does: `active` means the
/// most recent Telegram thread. Callers still apply [`resolve_alias_id`] when
/// they need the effective thread behind a mirror alias.
#[must_use]
pub fn resolve_thread_param(id: String) -> String {
    if id == "active" {
        thread_index::latest_thread_id_with_prefix("telegram-")
            .ok()
            .flatten()
            .unwrap_or(id)
    } else {
        id
    }
}

/// Follows a single `alias_to` hop, so an id taken from a Telegram worker
/// mirror topic resolves to the worker thread it mirrors.
///
/// Mini App deep links are built from the *topic session* id
/// (`?startapp={thread_id}` in the pinned header), and a worker mirror topic is
/// an alias whose own thread file carries neither lineage nor workers. Without
/// this hop, opening a worker from its mirror looks like a thread with no
/// orchestrator and no workers. The Git view resolves the same way.
#[must_use]
pub fn resolve_alias_id(id: &str) -> String {
    thread_persistence::read_thread_alias(id)
        .ok()
        .flatten()
        .filter(|alias| valid_thread_id(alias))
        .unwrap_or_else(|| id.to_string())
}

/// Builds a `t.me/c/<internal_id>/<topic_id>` link for a Telegram topic thread.
///
/// Thread ids are minted as `telegram-{chat_id}-topic-{topic_id}`; the link
/// form itself is [`crate::telegram::topic_link`]. Threads with no topic (plain
/// DMs) and non-Telegram threads have no direct linkable target.
fn telegram_topic_link(thread_id: &str) -> Option<String> {
    let (chat_id, topic_id) = crate::telegram::parse_topic_conversation_id(thread_id)?;
    crate::telegram::topic_link(chat_id, topic_id)
}

/// Resolves a Telegram topic link for either a direct Telegram thread or a
/// worker thread linked to a mirror topic.
///
/// The worker→mirror lookup is answered by the thread index from an indexed
/// column, so a thread that is not a Telegram topic costs one SQL row read
/// instead of opening every saved thread file.
fn resolve_telegram_link(thread_id: &str) -> Option<String> {
    if let Some(link) = telegram_topic_link(thread_id) {
        return Some(link);
    }
    let mirror_id = thread_index::mirror_thread_id_for_worker(thread_id)
        .inspect_err(|error| {
            tracing::warn!(thread_id, error = %format!("{error:#}"), "Failed to resolve worker mirror topic");
        })
        .ok()
        .flatten()?;
    telegram_topic_link(&mirror_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_topic_links_only_for_supergroup_topic_threads() {
        assert_eq!(
            telegram_topic_link("telegram--1001234567890-topic-17771").as_deref(),
            Some("https://t.me/c/1234567890/17771")
        );

        // Plain DM threads carry no topic, and DM chat ids have no `-100`
        // prefix, so neither form yields a linkable topic.
        assert_eq!(telegram_topic_link("telegram--1001234567890"), None);
        assert_eq!(telegram_topic_link("telegram-12345-topic-7"), None);
        // Threads started outside Telegram (TUI/CLI/zbar).
        assert_eq!(
            telegram_topic_link("3d6f20e3-78d4-4d1f-9f8e-4d442f3bba31"),
            None
        );
        // Non-numeric segments must not be pasted into a URL.
        assert_eq!(telegram_topic_link("telegram--100abc-topic-7"), None);
        assert_eq!(telegram_topic_link("telegram--1001-topic-"), None);
    }

    #[test]
    fn resolves_telegram_link_for_direct_topic_threads() {
        // Direct topic threads resolve from the ID alone and never consult the
        // thread index. The worker→mirror branch is covered by
        // `thread_index::mirror_thread_id_for_worker`, which owns the indexed
        // lookup.
        assert_eq!(
            resolve_telegram_link("telegram--1001234567890-topic-17771").as_deref(),
            Some("https://t.me/c/1234567890/17771")
        );
    }

    #[test]
    fn rejects_thread_ids_that_can_escape_the_threads_directory() {
        assert!(valid_thread_id("telegram--100-topic-42"));
        assert!(valid_thread_id("3d6f20e3-78d4-4d1f-9f8e-4d442f3bba31"));
        assert!(!valid_thread_id(""));
        assert!(!valid_thread_id(".."));
        assert!(!valid_thread_id("../config"));
        assert!(!valid_thread_id("folder/thread"));
        assert!(!valid_thread_id("folder\\thread"));
    }
}
