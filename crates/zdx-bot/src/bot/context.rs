use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tokio::sync::Notify;
use zdx_core::config::Config;
use zdx_core::core::agent::ToolConfig;

use crate::telegram::TelegramClient;

pub(crate) struct BotContext {
    client: TelegramClient,
    config: Config,
    allowlist_user_ids: HashSet<i64>,
    allowlist_chat_ids: HashSet<i64>,
    root: PathBuf,
    bot_system_prompt: Option<String>,
    tool_config: ToolConfig,
    rebuild_signal: Notify,
}

impl BotContext {
    pub(crate) fn new(
        client: TelegramClient,
        config: Config,
        allowlist_user_ids: HashSet<i64>,
        allowlist_chat_ids: HashSet<i64>,
        root: PathBuf,
        bot_system_prompt: Option<String>,
        tool_config: ToolConfig,
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
}
