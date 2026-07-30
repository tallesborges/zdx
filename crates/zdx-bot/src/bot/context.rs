use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use zdx_engine::config::{Config, TelegramProfileConfig};
use zdx_engine::core::agent::ToolConfig;

use crate::command_picker::CommandPickerMap;
use crate::followups::FollowupMap;
use crate::handlers::message::LauncherMap;
use crate::retry::RetryMap;
use crate::staging::StagingMap;
use crate::telegram::TelegramClient;

/// Key for the per-turn cancellation map: (`chat_id`, `user_message_id`).
/// User message IDs are per-chat unique, so stale buttons from previous turns
/// cannot cancel a new turn.
pub(crate) type CancelKey = (i64, i64);

/// Key for queued-item cancellation: (`chat_id`, `user_message_id`).
pub(crate) type QueueCancelKey = (i64, i64);

/// Shared map of active agent turns that can be cancelled via inline button.
pub(crate) type CancelMap = Arc<Mutex<HashMap<CancelKey, CancellationToken>>>;

/// Cancellation handle for a queued (not-yet-processing) item.
#[derive(Clone)]
pub(crate) struct QueuedCancel {
    pub token: CancellationToken,
    /// `message_id` of the "⏳ Queued" status message to update on cancel.
    pub status_message_id: i64,
}

/// Shared map of queued (not-yet-processing) items that can be cancelled.
pub(crate) type QueueCancelMap = Arc<Mutex<HashMap<QueueCancelKey, QueuedCancel>>>;

pub(crate) fn new_cancel_map() -> CancelMap {
    Arc::new(Mutex::new(HashMap::new()))
}

pub(crate) fn new_queue_cancel_map() -> QueueCancelMap {
    Arc::new(Mutex::new(HashMap::new()))
}

pub(crate) struct BotContext {
    client: TelegramClient,
    config: RwLock<Config>,
    /// Per-profile config, keyed by `chat_id`, layered from the profile's cwd.
    /// Built once at startup because profiles are static in `config.toml`.
    profile_configs: RwLock<HashMap<i64, Config>>,
    allowlist_user_ids: HashSet<i64>,
    allowlist_chat_ids: HashSet<i64>,
    root: PathBuf,
    bot_instruction_layer: Option<String>,
    tool_config: ToolConfig,
    exit_signal: Notify,
    cancel_map: CancelMap,
    queue_cancel_map: QueueCancelMap,
    followup_map: FollowupMap,
    retry_map: RetryMap,
    staging_map: StagingMap,
    command_picker_map: CommandPickerMap,
    launcher_map: LauncherMap,
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
    pub followup_map: FollowupMap,
    pub retry_map: RetryMap,
    pub staging_map: StagingMap,
    pub command_picker_map: CommandPickerMap,
    pub launcher_map: LauncherMap,
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
            followup_map,
            retry_map,
            staging_map,
            command_picker_map,
            launcher_map,
        } = deps;
        let root = root.canonicalize().unwrap_or(root);
        let profile_configs = load_profile_configs(&config);
        Self {
            client,
            config: RwLock::new(config),
            profile_configs: RwLock::new(profile_configs),
            allowlist_user_ids,
            allowlist_chat_ids,
            root,
            bot_instruction_layer,
            tool_config,
            exit_signal: Notify::new(),
            cancel_map,
            queue_cancel_map,
            followup_map,
            retry_map,
            staging_map,
            command_picker_map,
            launcher_map,
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

    /// Config for `chat_id`: the profile's layered config when the chat is
    /// bound to a profile, otherwise the bot-level config.
    pub(crate) fn config_for_chat(&self, chat_id: i64) -> Config {
        if let Some(config) = self
            .profile_configs
            .read()
            .expect("bot profile config lock poisoned")
            .get(&chat_id)
        {
            return config.clone();
        }

        self.config()
    }

    /// Applies `f` to the bot-level config and every profile config, so runtime
    /// changes (`/model`, `/thinking`) stay in effect for all chats.
    pub(crate) fn update_config(&self, f: impl Fn(&mut Config)) {
        let mut config = self.config.write().expect("bot config lock poisoned");
        f(&mut config);

        let mut profiles = self
            .profile_configs
            .write()
            .expect("bot profile config lock poisoned");
        for profile_config in profiles.values_mut() {
            f(profile_config);
        }
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

    pub(crate) fn followup_map(&self) -> &FollowupMap {
        &self.followup_map
    }

    pub(crate) fn retry_map(&self) -> &RetryMap {
        &self.retry_map
    }

    pub(crate) fn staging_map(&self) -> &StagingMap {
        &self.staging_map
    }

    pub(crate) fn command_picker_map(&self) -> &CommandPickerMap {
        &self.command_picker_map
    }

    pub(crate) fn launcher_map(&self) -> &LauncherMap {
        &self.launcher_map
    }
}

fn profile_root_path(profile: &TelegramProfileConfig) -> PathBuf {
    let root = profile.cwd_path();
    root.canonicalize().unwrap_or(root)
}

/// Projects `[telegram]` model/thinking onto the top-level fields the agent
/// actually reads. Applied to the bot-level config at startup and to every
/// per-profile config, which must stay in sync.
pub(crate) fn apply_telegram_overrides(config: &mut Config) {
    config.model.clone_from(&config.telegram.model);
    config.thinking_level = config.telegram.thinking_level;
}

/// Loads one layered [`Config`] per Telegram profile, anchored at the profile's
/// cwd so a workspace `.zdx/config.toml` applies to chats bound to it.
///
/// Profiles are static in `config.toml`, so this runs once at startup. A profile
/// whose layers fail to load is skipped and falls back to the bot-level config,
/// so one broken workspace file cannot take the whole bot down.
fn load_profile_configs(base: &Config) -> HashMap<i64, Config> {
    let mut configs = HashMap::new();

    for (name, profile) in &base.telegram.profiles {
        let root = profile_root_path(profile);
        let layers = zdx_engine::config::paths::config_layer_paths_for(&root);

        match Config::load_layered(&layers) {
            Ok(mut config) => {
                apply_telegram_overrides(&mut config);
                tracing::info!(
                    profile = %name,
                    chat_id = profile.chat_id,
                    model = %config.model,
                    "Loaded profile config",
                );
                configs.insert(profile.chat_id, config);
            }
            Err(err) => {
                tracing::warn!(
                    profile = %name,
                    root = %root.display(),
                    %err,
                    "Failed to load profile config layers; falling back to bot config",
                );
            }
        }
    }

    configs
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

    /// A profile's workspace `.zdx/config.toml` overrides the global layer for
    /// chats bound to that profile, without affecting the bot-level config.
    ///
    /// The workspace layer is always the highest-precedence layer, so this
    /// assertion holds regardless of the developer's real global config.
    #[test]
    fn test_profile_config_applies_workspace_layer() {
        let fallback_root = unique_temp_dir("layer-fallback");
        let profile_root = unique_temp_dir("layer-profile");
        fs::create_dir_all(&fallback_root).unwrap();
        fs::create_dir_all(profile_root.join(".zdx")).unwrap();
        fs::write(
            profile_root.join(".zdx").join("config.toml"),
            "[telegram]\nmodel = \"sentinel:workspace-model\"\n",
        )
        .unwrap();

        let config = Config {
            telegram: TelegramConfig {
                model: "sentinel:global-model".to_string(),
                profiles: BTreeMap::from([(
                    "zdx".to_string(),
                    TelegramProfileConfig {
                        chat_id: -100_123,
                        cwd: profile_root.display().to_string(),
                    },
                )]),
                ..Default::default()
            },
            model: "sentinel:global-model".to_string(),
            ..Default::default()
        };
        let context = test_context(config, fallback_root);

        assert_eq!(
            context.config_for_chat(-100_123).model,
            "sentinel:workspace-model"
        );
        assert_eq!(
            context.config_for_chat(-100_999).model,
            "sentinel:global-model"
        );
        assert_eq!(context.config().model, "sentinel:global-model");
    }

    /// Runtime `/model` changes must reach profile configs too, otherwise a
    /// bound chat would keep serving the pre-change model.
    #[test]
    fn test_update_config_reaches_profile_configs() {
        let fallback_root = unique_temp_dir("update-fallback");
        let profile_root = unique_temp_dir("update-profile");
        fs::create_dir_all(&fallback_root).unwrap();
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
        let context = test_context(config, fallback_root);

        context.update_config(|cfg| {
            cfg.model = "sentinel:picked".to_string();
        });

        assert_eq!(context.config().model, "sentinel:picked");
        assert_eq!(context.config_for_chat(-100_123).model, "sentinel:picked");
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
                followup_map: crate::followups::new_followup_map(),
                retry_map: crate::retry::new_retry_map(),
                staging_map: crate::staging::new_staging_map(),
                command_picker_map: crate::command_picker::new_command_picker_map(),
                launcher_map: crate::handlers::message::new_launcher_map(),
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
