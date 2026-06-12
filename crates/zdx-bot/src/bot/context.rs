use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use zdx_engine::config::{Config, TelegramProfileConfig};
use zdx_engine::core::agent::ToolConfig;

use crate::ask_user::PendingQuestionMap;
use crate::followups::FollowupMap;
use crate::telegram::TelegramClient;

/// Key for the per-turn cancellation map: (`chat_id`, `user_message_id`).
/// User message IDs are per-chat unique, so stale buttons from previous turns
/// cannot cancel a new turn.
pub(crate) type CancelKey = (i64, i64);

/// Key for queued-item cancellation: (`chat_id`, `user_message_id`).
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
    config: RwLock<Config>,
    allowlist_user_ids: HashSet<i64>,
    allowlist_chat_ids: HashSet<i64>,
    root: PathBuf,
    bot_instruction_layer: Option<String>,
    tool_config: ToolConfig,
    exit_signal: Notify,
    cancel_map: CancelMap,
    queue_cancel_map: QueueCancelMap,
    ask_user_map: PendingQuestionMap,
    followup_map: FollowupMap,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedProfileRoot {
    pub(crate) profile_name: Option<String>,
    pub(crate) root: PathBuf,
}

pub(crate) struct BotContextDeps {
    pub allowlist_user_ids: HashSet<i64>,
    pub allowlist_chat_ids: HashSet<i64>,
    pub root: PathBuf,
    pub bot_instruction_layer: Option<String>,
    pub tool_config: ToolConfig,
    pub cancel_map: CancelMap,
    pub queue_cancel_map: QueueCancelMap,
    pub ask_user_map: PendingQuestionMap,
    pub followup_map: FollowupMap,
}

impl BotContext {
    pub(crate) fn new(client: TelegramClient, config: Config, deps: BotContextDeps) -> Self {
        let BotContextDeps {
            allowlist_user_ids,
            allowlist_chat_ids,
            root,
            bot_instruction_layer,
            tool_config,
            cancel_map,
            queue_cancel_map,
            ask_user_map,
            followup_map,
        } = deps;
        let root = root.canonicalize().unwrap_or(root);
        Self {
            client,
            config: RwLock::new(config),
            allowlist_user_ids,
            allowlist_chat_ids,
            root,
            bot_instruction_layer,
            tool_config,
            exit_signal: Notify::new(),
            cancel_map,
            queue_cancel_map,
            ask_user_map,
            followup_map,
        }
    }

    pub(crate) fn client(&self) -> &TelegramClient {
        &self.client
    }

    pub(crate) fn config(&self) -> Config {
        self.config
            .read()
            .expect("bot config lock poisoned")
            .clone()
    }

    pub(crate) fn update_config(&self, f: impl FnOnce(&mut Config)) {
        let mut config = self.config.write().expect("bot config lock poisoned");
        f(&mut config);
    }

    pub(crate) fn allowlist_user_ids(&self) -> &HashSet<i64> {
        &self.allowlist_user_ids
    }

    pub(crate) fn allowlist_chat_ids(&self) -> &HashSet<i64> {
        &self.allowlist_chat_ids
    }

    pub(crate) fn root_for_chat(&self, chat_id: i64) -> ResolvedProfileRoot {
        let config = self.config.read().expect("bot config lock poisoned");
        if let Some((name, profile)) = config.telegram_profile_for_chat(chat_id) {
            return ResolvedProfileRoot {
                profile_name: Some(name.to_string()),
                root: profile_root_path(profile),
            };
        }

        ResolvedProfileRoot {
            profile_name: None,
            root: self.root.clone(),
        }
    }

    pub(crate) fn bot_instruction_layer(&self) -> Option<&str> {
        self.bot_instruction_layer.as_deref()
    }

    pub(crate) fn tool_config(&self) -> &ToolConfig {
        &self.tool_config
    }

    /// Signal the bot to exit (with code 42) so a supervisor can restart it.
    pub(crate) fn request_exit(&self) {
        self.exit_signal.notify_one();
    }

    /// Wait for an exit signal.
    pub(crate) async fn exit_notified(&self) {
        self.exit_signal.notified().await;
    }

    pub(crate) fn cancel_map(&self) -> &CancelMap {
        &self.cancel_map
    }

    pub(crate) fn queue_cancel_map(&self) -> &QueueCancelMap {
        &self.queue_cancel_map
    }

    pub(crate) fn ask_user_map(&self) -> &PendingQuestionMap {
        &self.ask_user_map
    }

    pub(crate) fn followup_map(&self) -> &FollowupMap {
        &self.followup_map
    }
}

fn profile_root_path(profile: &TelegramProfileConfig) -> PathBuf {
    let root = profile.cwd_path();
    root.canonicalize().unwrap_or(root)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use zdx_engine::config::TelegramConfig;

    use super::*;

    #[test]
    fn test_root_for_chat_uses_matching_profile_cwd() {
        let temp_root = unique_temp_dir("fallback");
        let profile_root = unique_temp_dir("profile");
        fs::create_dir_all(&temp_root).unwrap();
        fs::create_dir_all(&profile_root).unwrap();

        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([(
                    "zdx".to_string(),
                    TelegramProfileConfig {
                        chat_id: -100_123,
                        cwd: profile_root.display().to_string(),
                    },
                )]),
                ..Default::default()
            },
            ..Default::default()
        };
        let context = test_context(config, temp_root.clone());

        let resolved = context.root_for_chat(-100_123);
        assert_eq!(resolved.profile_name.as_deref(), Some("zdx"));
        assert_eq!(resolved.root, profile_root.canonicalize().unwrap());

        let fallback = context.root_for_chat(-100_999);
        assert_eq!(fallback.profile_name, None);
        assert_eq!(fallback.root, temp_root.canonicalize().unwrap());
    }

    fn test_context(config: Config, root: PathBuf) -> BotContext {
        BotContext::new(
            TelegramClient::new("token".to_string()),
            config,
            BotContextDeps {
                allowlist_user_ids: HashSet::new(),
                allowlist_chat_ids: HashSet::new(),
                root,
                bot_instruction_layer: None,
                tool_config: ToolConfig::default(),
                cancel_map: new_cancel_map(),
                queue_cancel_map: new_queue_cancel_map(),
                ask_user_map: crate::ask_user::new_pending_map(),
                followup_map: crate::followups::new_followup_map(),
            },
        )
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("zdx-bot-profile-{label}-{nanos}"))
    }
}
