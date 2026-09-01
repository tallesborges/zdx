use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use zdx_engine::config::{Config, TelegramProfileConfig, ThinkingLevel};
use zdx_engine::core::agent::ToolConfig;
use zdx_engine::core::workers::WorkerManager;

use crate::command_picker::CommandPickerMap;
use crate::followups::FollowupMap;
use crate::goal::GoalMap;
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

/// Where an orchestrator thread last ran, so worker completion callbacks can be
/// dispatched into the same chat/topic. Process-lifetime only by design.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OrchestratorRoute {
    pub chat: i64,
    pub topic: Option<i64>,
    pub user: i64,
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
    goal_map: GoalMap,
    command_picker_map: CommandPickerMap,
    launcher_map: LauncherMap,
    /// Shared worker manager (also bound into the tool registry); used to
    /// route mirror-topic messages into worker FIFOs.
    worker_manager: Arc<WorkerManager>,
    /// Owner-thread → Telegram route for orchestrator completion callbacks.
    /// Populated on every orchestrator turn; lost on restart by design.
    orchestrator_routes: RwLock<HashMap<String, OrchestratorRoute>>,
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
    pub worker_manager: Arc<WorkerManager>,
    pub cancel_map: CancelMap,
    pub queue_cancel_map: QueueCancelMap,
    pub followup_map: FollowupMap,
    pub retry_map: RetryMap,
    pub staging_map: StagingMap,
    pub goal_map: GoalMap,
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
            worker_manager,
            cancel_map,
            queue_cancel_map,
            followup_map,
            retry_map,
            staging_map,
            goal_map,
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
            worker_manager,
            exit_signal: Notify::new(),
            cancel_map,
            queue_cancel_map,
            followup_map,
            retry_map,
            staging_map,
            goal_map,
            command_picker_map,
            launcher_map,
            orchestrator_routes: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) fn worker_manager(&self) -> &Arc<WorkerManager> {
        &self.worker_manager
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

    /// Persists a runtime model change for `chat_id`.
    ///
    /// The selection is written to the chat root's workspace overlay (the
    /// profile cwd, else the bot's own cwd) and applied to that chat's config
    /// only, so bound projects keep independent defaults.
    pub(crate) fn set_chat_model(&self, chat_id: i64, model_id: &str) -> anyhow::Result<()> {
        Config::save_model_for_cwd(&self.root_for_chat(chat_id).root, model_id)?;
        self.update_config_for_chat(chat_id, |cfg| cfg.model = model_id.to_string());
        Ok(())
    }

    /// Persists a runtime thinking-level change for `chat_id`.
    /// See [`BotContext::set_chat_model`].
    pub(crate) fn set_chat_thinking_level(
        &self,
        chat_id: i64,
        level: ThinkingLevel,
    ) -> anyhow::Result<()> {
        Config::save_thinking_level_for_cwd(&self.root_for_chat(chat_id).root, level)?;
        self.update_config_for_chat(chat_id, |cfg| cfg.thinking_level = level);
        Ok(())
    }

    /// Applies `f` to the config that `chat_id` resolves to: its profile config
    /// when bound to one, otherwise the bot-level config.
    fn update_config_for_chat(&self, chat_id: i64, f: impl Fn(&mut Config)) {
        let mut profiles = self
            .profile_configs
            .write()
            .expect("bot profile config lock poisoned");
        if let Some(profile_config) = profiles.get_mut(&chat_id) {
            f(profile_config);
            return;
        }
        drop(profiles);

        f(&mut self.config.write().expect("bot config lock poisoned"));
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

    pub(crate) fn goal_map(&self) -> &GoalMap {
        &self.goal_map
    }

    pub(crate) fn command_picker_map(&self) -> &CommandPickerMap {
        &self.command_picker_map
    }

    pub(crate) fn launcher_map(&self) -> &LauncherMap {
        &self.launcher_map
    }

    /// Whether General-created topics in this chat become orchestrator home
    /// bases (opt-in via `telegram.profiles.<name>.orchestrator`).
    pub(crate) fn orchestrator_enabled_for_chat(&self, chat_id: i64) -> bool {
        self.config
            .read()
            .expect("bot config lock poisoned")
            .telegram_profile_for_chat(chat_id)
            .is_some_and(|(_, profile)| profile.orchestrator)
    }

    /// Picks the group chat whose profile `cwd` contains `root` (deepest match)
    /// for hosting a worker's mirror topic, so project workers surface in
    /// their project's group even when orchestrated from elsewhere (e.g. a DM
    /// home). Returns `None` when no group profile covers the root.
    pub(crate) fn mirror_chat_for_root(&self, root: &std::path::Path) -> Option<i64> {
        let config = self.config.read().expect("bot config lock poisoned");
        config
            .telegram
            .profiles
            .values()
            .filter(|profile| profile.chat_id < 0)
            .filter_map(|profile| {
                let cwd = profile.cwd_path();
                let cwd = cwd.canonicalize().unwrap_or(cwd);
                root.starts_with(&cwd)
                    .then(|| (cwd.components().count(), profile.chat_id))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, chat_id)| chat_id)
    }

    /// Records where an orchestrator thread last ran.
    pub(crate) fn record_orchestrator_route(&self, thread_id: &str, route: OrchestratorRoute) {
        self.orchestrator_routes
            .write()
            .expect("orchestrator route lock poisoned")
            .insert(thread_id.to_string(), route);
    }

    /// Latest known Telegram route for an orchestrator thread, if any.
    pub(crate) fn orchestrator_route(&self, thread_id: &str) -> Option<OrchestratorRoute> {
        self.orchestrator_routes
            .read()
            .expect("orchestrator route lock poisoned")
            .get(thread_id)
            .copied()
    }
}

fn profile_root_path(profile: &TelegramProfileConfig) -> PathBuf {
    let root = profile.cwd_path();
    root.canonicalize().unwrap_or(root)
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
            Ok(config) => {
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
                        orchestrator: false,
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

    #[test]
    fn mirror_chat_prefers_deepest_matching_group_profile() {
        let umbrella = unique_temp_dir("mirror-umbrella");
        let project = umbrella.join("dub");
        fs::create_dir_all(&project).unwrap();

        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([
                    (
                        "umbrella".to_string(),
                        TelegramProfileConfig {
                            chat_id: -100_500,
                            cwd: umbrella.display().to_string(),
                            orchestrator: false,
                        },
                    ),
                    (
                        "dub".to_string(),
                        TelegramProfileConfig {
                            chat_id: -100_600,
                            cwd: project.display().to_string(),
                            orchestrator: false,
                        },
                    ),
                    (
                        "dm-like".to_string(),
                        TelegramProfileConfig {
                            chat_id: 777,
                            cwd: project.display().to_string(),
                            orchestrator: false,
                        },
                    ),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let root = unique_temp_dir("mirror-fallback");
        fs::create_dir_all(&root).unwrap();
        let context = test_context(config, root);

        let project = project.canonicalize().unwrap();
        // Deepest matching group wins; positive (DM-like) chat ids are ignored.
        assert_eq!(context.mirror_chat_for_root(&project), Some(-100_600));
        // Parent-only match falls back to the umbrella group.
        assert_eq!(
            context.mirror_chat_for_root(&umbrella.canonicalize().unwrap()),
            Some(-100_500)
        );
        // Roots outside every profile match nothing.
        assert_eq!(
            context.mirror_chat_for_root(std::path::Path::new("/nonexistent/elsewhere")),
            None
        );
    }

    #[test]
    fn orchestrator_flag_is_per_chat_opt_in() {
        let root = unique_temp_dir("orch-flag");
        fs::create_dir_all(&root).unwrap();

        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([
                    (
                        "manager".to_string(),
                        TelegramProfileConfig {
                            chat_id: -100_111,
                            cwd: root.display().to_string(),
                            orchestrator: true,
                        },
                    ),
                    (
                        "project".to_string(),
                        TelegramProfileConfig {
                            chat_id: -100_222,
                            cwd: root.display().to_string(),
                            orchestrator: false,
                        },
                    ),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let context = test_context(config, root);

        assert!(context.orchestrator_enabled_for_chat(-100_111));
        assert!(!context.orchestrator_enabled_for_chat(-100_222));
        // Unprofiled chats never opt in.
        assert!(!context.orchestrator_enabled_for_chat(-100_999));
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
            "model = \"sentinel:workspace-model\"\n",
        )
        .unwrap();

        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([(
                    "zdx".to_string(),
                    TelegramProfileConfig {
                        chat_id: -100_123,
                        cwd: profile_root.display().to_string(),
                        orchestrator: false,
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

    /// `/model` in a profiled chat writes that profile's workspace overlay and
    /// leaves every other chat (and the bot-level config) alone.
    #[test]
    fn test_set_chat_model_is_scoped_to_the_profile() {
        let fallback_root = unique_temp_dir("set-model-fallback");
        let profile_root = unique_temp_dir("set-model-profile");
        fs::create_dir_all(&fallback_root).unwrap();
        fs::create_dir_all(profile_root.join(".zdx")).unwrap();

        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([(
                    "zdx".to_string(),
                    TelegramProfileConfig {
                        chat_id: -100_123,
                        cwd: profile_root.display().to_string(),
                        orchestrator: false,
                    },
                )]),
                ..Default::default()
            },
            model: "sentinel:global-model".to_string(),
            ..Default::default()
        };
        let context = test_context(config, fallback_root);

        context.set_chat_model(-100_123, "sentinel:picked").unwrap();

        assert_eq!(context.config_for_chat(-100_123).model, "sentinel:picked");
        assert_eq!(
            context.config_for_chat(-100_999).model,
            "sentinel:global-model"
        );
        assert_eq!(context.config().model, "sentinel:global-model");

        let overlay = fs::read_to_string(profile_root.join(".zdx").join("config.toml")).unwrap();
        assert_eq!(overlay.trim(), "model = \"sentinel:picked\"");
    }

    fn test_context(config: Config, root: PathBuf) -> BotContext {
        BotContext::new(
            TelegramClient::new("token".to_string()),
            config,
            BotContextDeps {
                goal_map: crate::goal::new_goal_map(),
                allowlist_user_ids: HashSet::new(),
                allowlist_chat_ids: HashSet::new(),
                root,
                bot_instruction_layer: None,
                tool_config: ToolConfig::default(),
                worker_manager: WorkerManager::new().0,
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
