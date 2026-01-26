use std::collections::HashSet;
use std::path::{Path, PathBuf};

use zdx_core::config::Config;
use zdx_core::core::agent::ToolConfig;

use crate::telegram::TelegramClient;

pub(crate) struct BotContext {
    client: TelegramClient,
    config: Config,
    allowlist: HashSet<i64>,
    root: PathBuf,
    system_prompt: Option<String>,
    tool_config: ToolConfig,
}

impl BotContext {
    pub(crate) fn new(
        client: TelegramClient,
        config: Config,
        allowlist: HashSet<i64>,
        root: PathBuf,
        system_prompt: Option<String>,
        tool_config: ToolConfig,
    ) -> Self {
        Self {
            client,
            config,
            allowlist,
            root,
            system_prompt,
            tool_config,
        }
    }

    pub(crate) fn client(&self) -> &TelegramClient {
        &self.client
    }

    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn allowlist(&self) -> &HashSet<i64> {
        &self.allowlist
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub(crate) fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub(crate) fn tool_config(&self) -> &ToolConfig {
        &self.tool_config
    }
}
