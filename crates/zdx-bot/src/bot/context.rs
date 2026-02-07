use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use zdx_core::config::Config;
use zdx_core::core::agent::ToolConfig;

use crate::telegram::TelegramClient;

/// Key for the per-turn cancellation map: (chat_id, status_message_id).
/// Using the status message ID (instead of topic_id) ensures stale cancel
/// buttons from previous turns cannot cancel a new turn in the same topic.
pub(crate) type CancelKey = (i64, i64);

/// Key for queued-item cancellation: (chat_id, user_message_id).
pub(crate) type QueueCancelKey = (i64, i64);

/// Shared map of active agent turns that can be cancelled via inline button.
pub(crate) type CancelMap = Arc<Mutex<HashMap<CancelKey, CancellationToken>>>;

/// Shared map of queued (not-yet-processing) items that can be cancelled.
pub(crate) type QueueCancelMap = Arc<Mutex<HashMap<QueueCancelKey, CancellationToken>>>;

pub(crate) fn new_cancel_map() -> CancelMap {
    Arc::new(Mutex::new(HashMap::new()))
}

pub(crate) fn new_queue_cancel_map() -> QueueCancelMap {
    Arc::new(Mutex::new(HashMap::new()))
}

pub(crate) struct BotContext {
    client: TelegramClient,
    config: Config,
    allowlist_user_ids: HashSet<i64>,
    allowlist_chat_ids: HashSet<i64>,
    root: PathBuf,
    bot_system_prompt: Option<String>,
    tool_config: ToolConfig,
    rebuild_signal: Notify,
    cancel_map: CancelMap,
    queue_cancel_map: QueueCancelMap,
}

impl BotContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        client: TelegramClient,
        config: Config,
        allowlist_user_ids: HashSet<i64>,
        allowlist_chat_ids: HashSet<i64>,
        root: PathBuf,
        bot_system_prompt: Option<String>,
        tool_config: ToolConfig,
        cancel_map: CancelMap,
        queue_cancel_map: QueueCancelMap,
    ) -> Self {
        Self {
            client,
            config,
            allowlist_user_ids,
            allowlist_chat_ids,
            root,
            bot_system_prompt,
            tool_config,
            rebuild_signal: Notify::new(),
            cancel_map,
            queue_cancel_map,
        }
    }

    pub(crate) fn client(&self) -> &TelegramClient {
        &self.client
    }

    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn allowlist_user_ids(&self) -> &HashSet<i64> {
        &self.allowlist_user_ids
    }

    pub(crate) fn allowlist_chat_ids(&self) -> &HashSet<i64> {
        &self.allowlist_chat_ids
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub(crate) fn bot_system_prompt(&self) -> Option<&str> {
        self.bot_system_prompt.as_deref()
    }

    pub(crate) fn tool_config(&self) -> &ToolConfig {
        &self.tool_config
    }

    /// Signal the bot to rebuild (exit with code 42).
    pub(crate) fn request_rebuild(&self) {
        self.rebuild_signal.notify_one();
    }

    /// Wait for a rebuild signal.
    pub(crate) async fn rebuild_notified(&self) {
        self.rebuild_signal.notified().await;
    }

    pub(crate) fn cancel_map(&self) -> &CancelMap {
        &self.cancel_map
    }

    pub(crate) fn queue_cancel_map(&self) -> &QueueCancelMap {
        &self.queue_cancel_map
    }
}
