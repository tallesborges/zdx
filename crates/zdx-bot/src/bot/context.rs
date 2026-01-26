use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zdx_core::config::Config;
use zdx_core::core::agent::ToolConfig;

use crate::telegram::TelegramClient;

#[derive(Clone)]
pub(crate) struct BotContext {
    client: TelegramClient,
    config: Arc<Config>,
    allowlist: Arc<HashSet<i64>>,
    root: Arc<PathBuf>,
    system_prompt: Option<Arc<str>>,
    tool_config: Arc<ToolConfig>,
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
            config: Arc::new(config),
            allowlist: Arc::new(allowlist),
            root: Arc::new(root),
            system_prompt: system_prompt.map(Arc::<str>::from),
            tool_config: Arc::new(tool_config),
        }
    }

    pub(crate) fn client(&self) -> &TelegramClient {
        &self.client
    }

    pub(crate) fn config(&self) -> &Config {
        self.config.as_ref()
    }

    pub(crate) fn allowlist(&self) -> &HashSet<i64> {
        self.allowlist.as_ref()
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.as_ref().as_path()
    }

    pub(crate) fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub(crate) fn tool_config(&self) -> &ToolConfig {
        self.tool_config.as_ref()
    }
}
